//! `Compute` for Docker, or Podman through its Docker API: sandboxed agents on your own machine.
//!
//! Mapping (`docs/factory/sandbox.md`):
//!
//! - A sandbox's id is its container name, which stays put when a restore recreates the container.
//! - Docker labels can't change after creation, so a sandbox's labels live in a state file per sandbox in the state
//!   directory. The container carries only [`MARKER`], which `list` filters on and `docker ps --filter label=...`
//!   shows.
//! - A checkpoint is the container's filesystem exported and imported as a one-layer image, and restoring recreates
//!   the container from that image. Not a `docker commit`: that adds a layer on top of the image the container came
//!   from, so a pooled sandbox that checkpoints after every task would grow a chain of layers, each keeping what the
//!   last rewrote, until the disk fills or overlay2's ~125-layer limit is hit (TRAIL-176).
//! - Programs are supervised by `supervise.sh`, the container's main process, which restarts them when they exit
//!   and starts them again when the container restarts (it runs with `restart: unless-stopped`).
//! - A sandbox's way out is the gateway (`gateway.rs`): each sandbox has its own `--internal` network shared only
//!   with the gateway's container, and egress is the gateway's allowlist for it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bollard::Docker;
use bollard::errors::Error as DockerError;
use bollard::exec::{StartExecOptions, StartExecResults};
use bollard::models::{
    ContainerConfig, ContainerCreateBody, ExecConfig, HostConfig, RestartPolicy, RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    CreateContainerOptions, CreateImageOptions, InspectContainerOptions, ListContainersOptions, ListImagesOptions,
    RemoveContainerOptions, RemoveImageOptions, UploadToContainerOptions,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use trailhead_factory::{
    CheckpointId, Compute, Egress, Error, GATEWAY_HOST, GatewayConnection, GatewayRequest, Labels, Program, Result,
    Sandbox, SandboxId, SandboxSpec,
};

mod gateway;

pub use gateway::{GATEWAY_BIN, GATEWAY_LABEL, sandbox_network};

/// The label every sandbox container carries, set to its name.
pub const MARKER: &str = "dev.trailhead.sandbox";
/// The label on checkpoint images, set to the sandbox they came from, so `destroy` can remove them.
pub const CHECKPOINT_OF: &str = "dev.trailhead.checkpoint-of";
/// The label on checkpoint images holding the note they were taken with.
pub const CHECKPOINT_NOTE: &str = "dev.trailhead.checkpoint-note";
/// The note of a pool's warm checkpoints, the only ones a later checkpoint may remove: the rest (a parked
/// sandbox's) stay until the sandbox is destroyed.
const WARM_NOTE: &str = "warm";
/// The repository checkpoint images are imported to.
pub const CHECKPOINT_REPO: &str = "trailhead-checkpoint";
/// The sandbox image built by `mise run docker-image`.
pub const DEFAULT_IMAGE: &str = "trailhead-sandbox:latest";
/// The ordinary bridge network the gateway gets out on. Sandboxes are never on it.
pub const DEFAULT_NETWORK: &str = "trailhead-gateway-out";
/// Where the daemon lives in [`DEFAULT_IMAGE`], whose user is `trailhead`.
pub const HOME: &str = "/home/trailhead";

const SUPERVISE: &str = include_str!("supervise.sh");
const SUPERVISE_PATH: &str = "/opt/trailhead/supervise";

/// The least free disk, in bytes, a run may start with (`DockerConfig::min_free_bytes`).
pub const DEFAULT_MIN_FREE_BYTES: u64 = 10 << 30;

/// How long `start` watches a new program for an early exit.
const START_WATCH: Duration = Duration::from_secs(3);

#[derive(Debug, Clone)]
pub struct DockerConfig {
    /// The sandboxes' image, which the gateway's container also runs from.
    pub image: String,
    /// The ordinary bridge network the gateway gets out on.
    pub network: String,
    /// The OCI runtime, such as `runsc` for gVisor. Docker's default (`runc`) when unset.
    pub runtime: Option<String>,
    /// Where sandboxes' labels and egress are kept.
    pub state_dir: PathBuf,
    /// The gateway's container name. Sandboxes reach it as [`GATEWAY_HOST`] whatever this is.
    pub gateway: String,
    /// The `trailhead-daemon` binary, which the gateway runs as.
    pub daemon: Arc<Vec<u8>>,
    /// What the gateway may forward to. Sent to it on stdin, never kept on disk.
    pub connections: Vec<GatewayConnection>,
    /// The least free space the Docker engine's disk may have when a run is about to start.
    pub min_free_bytes: u64,
}

impl DockerConfig {
    pub fn new(state_dir: impl Into<PathBuf>, daemon: Arc<Vec<u8>>) -> DockerConfig {
        DockerConfig {
            image: DEFAULT_IMAGE.into(),
            network: DEFAULT_NETWORK.into(),
            runtime: None,
            state_dir: state_dir.into(),
            gateway: GATEWAY_HOST.into(),
            daemon,
            connections: Vec::new(),
            min_free_bytes: DEFAULT_MIN_FREE_BYTES,
        }
    }
}

/// Held while changing a sandbox's state and pushing it to the gateway.
#[derive(Debug, Default)]
pub(crate) struct Link {
    /// The start time of the gateway run that has the full state.
    synced: Option<String>,
}

#[derive(Clone)]
pub struct DockerCompute {
    docker: Docker,
    config: DockerConfig,
    link: Arc<tokio::sync::Mutex<Link>>,
    watching: Arc<AtomicBool>,
}

/// What Docker can't hold for a sandbox.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct State {
    labels: Labels,
    egress: Egress,
}

impl DockerCompute {
    /// Connects through `DOCKER_HOST`, or the platform's default socket.
    pub fn new(config: DockerConfig) -> Result<DockerCompute> {
        let docker = Docker::connect_with_defaults().map_err(docker_err)?;
        Ok(DockerCompute::with_client(docker, config))
    }

    pub fn with_client(docker: Docker, config: DockerConfig) -> DockerCompute {
        DockerCompute { docker, config, link: Default::default(), watching: Default::default() }
    }

    /// The recent output of a program started with `start`.
    pub async fn logs(&self, id: &SandboxId, program: &str) -> Result<String> {
        let log = format!("$HOME/.trailhead/programs/{}/log", shell_quote(program));
        let (_, out) = self.exec(&id.0, None, &format!("tail -n 3000 {log}")).await?;
        Ok(out)
    }

    fn state_path(&self, name: &str) -> PathBuf {
        self.config.state_dir.join(format!("{name}.json"))
    }

    async fn read_state(&self, name: &str) -> Result<State> {
        match tokio::fs::read(self.state_path(name)).await {
            Ok(raw) => serde_json::from_slice(&raw).map_err(|e| Error::Other(format!("sandbox {name} state: {e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(io_err(&self.state_path(name), e)),
        }
    }

    /// Written beside the file and renamed over it, so a crash never leaves half a state file.
    async fn write_state(&self, name: &str, state: &State) -> Result<()> {
        let path = self.state_path(name);
        tokio::fs::create_dir_all(&self.config.state_dir).await.map_err(|e| io_err(&self.config.state_dir, e))?;
        let tmp = path.with_extension("json.new");
        let raw = serde_json::to_vec_pretty(state).expect("state serializes");
        tokio::fs::write(&tmp, raw).await.map_err(|e| io_err(&tmp, e))?;
        tokio::fs::rename(&tmp, &path).await.map_err(|e| io_err(&path, e))
    }

    async fn remove_state(&self, name: &str) -> Result<()> {
        match tokio::fs::remove_file(self.state_path(name)).await {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(io_err(&self.state_path(name), e)),
            _ => Ok(()),
        }
    }

    /// Whether a container exists, by name.
    async fn exists(&self, name: &str) -> Result<bool> {
        match self.docker.inspect_container(name, None::<InspectContainerOptions>).await {
            Ok(_) => Ok(true),
            Err(e) if status(&e) == Some(404) => Ok(false),
            Err(e) => Err(docker_err(e)),
        }
    }

    /// Creates and starts the sandbox's container from `image`. A fresh image gets the supervisor first; a checkpoint
    /// already has it.
    async fn run_container(&self, name: &str, image: &str, fresh: bool) -> Result<()> {
        let body = ContainerCreateBody {
            image: Some(image.into()),
            cmd: Some(vec![SUPERVISE_PATH.into()]),
            entrypoint: Some(vec![]),
            env: Some(gateway::proxy_env()),
            labels: Some(HashMap::from([(MARKER.to_string(), name.to_string())])),
            host_config: Some(HostConfig {
                init: Some(true),
                network_mode: Some(sandbox_network(name)),
                cap_drop: Some(vec!["NET_RAW".into(), "NET_ADMIN".into()]),
                restart_policy: Some(RestartPolicy {
                    name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                    maximum_retry_count: None,
                }),
                runtime: self.config.runtime.clone(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let opts = CreateContainerOptions { name: Some(name.into()), ..Default::default() };
        self.docker.create_container(Some(opts), body).await.map_err(docker_err)?;
        if fresh {
            let tar = tar_of(&[("opt/trailhead/supervise", SUPERVISE.as_bytes(), 0o755, 0, 0)], &["opt/trailhead"])?;
            self.upload(name, "/", tar).await?;
        }
        self.docker.start_container(name, None).await.map_err(docker_err)
    }

    async fn upload(&self, name: &str, dir: &str, tar: Vec<u8>) -> Result<()> {
        let opts = UploadToContainerOptions { path: dir.into(), ..Default::default() };
        self.docker.upload_to_container(name, Some(opts), bollard::body_full(tar.into())).await.map_err(docker_err)
    }

    /// Runs a shell command in the container and waits for it, returning its exit code and output.
    async fn exec(&self, name: &str, user: Option<&str>, script: &str) -> Result<(i64, String)> {
        let config = ExecConfig {
            cmd: Some(vec!["/bin/sh".into(), "-c".into(), script.into()]),
            user: user.map(str::to_string),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            ..Default::default()
        };
        let exec = self.docker.create_exec(name, config).await.map_err(docker_err)?;
        let mut out = String::new();
        if let StartExecResults::Attached { mut output, .. } =
            self.docker.start_exec(&exec.id, None::<StartExecOptions>).await.map_err(docker_err)?
        {
            while let Some(chunk) = output.next().await {
                out.push_str(&chunk.map_err(docker_err)?.to_string());
            }
        }
        let code = self.docker.inspect_exec(&exec.id).await.map_err(docker_err)?.exit_code.unwrap_or(-1);
        Ok((code, out))
    }

    /// Runs a command in the background, outliving the exec.
    async fn spawn(&self, name: &str, cmd: Vec<String>) -> Result<()> {
        let config = ExecConfig { cmd: Some(cmd), ..Default::default() };
        let exec = self.docker.create_exec(name, config).await.map_err(docker_err)?;
        let opts = StartExecOptions { detach: true, ..Default::default() };
        self.docker.start_exec(&exec.id, Some(opts)).await.map_err(docker_err).map(drop)
    }

    /// Streams the container's filesystem into a new image tagged `image`.
    async fn import_filesystem(&self, name: &str, image: &str, note: &str, changes: Vec<String>) -> Result<()> {
        // The export is read by a task of its own and passed through a channel, so the request body owns what it needs.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<std::io::Result<bytes::Bytes>>(4);
        let (docker, owned) = (self.docker.clone(), name.to_string());
        let export = tokio::spawn(async move {
            let mut tar = docker.export_container(&owned);
            while let Some(chunk) = tar.next().await {
                let chunk = chunk.map_err(std::io::Error::other);
                let failed = chunk.is_err();
                if tx.send(chunk).await.is_err() || failed {
                    break;
                }
            }
        });
        let body = bollard::body_try_stream(futures_util::stream::poll_fn(move |cx| rx.poll_recv(cx)));
        let opts = CreateImageOptions {
            from_src: Some("-".into()),
            repo: Some(image.into()),
            message: Some(note.into()),
            changes,
            ..Default::default()
        };
        let mut progress = self.docker.create_image(Some(opts), Some(body), None);
        let mut result = Ok(());
        while let Some(step) = progress.next().await {
            if let Err(e) = step {
                result = Err(docker_err(e));
                break;
            }
        }
        drop(progress);
        let _ = export.await;
        result
    }

    /// Removes the sandbox's warm checkpoints other than `keep` that none of its labels point at. A parked
    /// sandbox's checkpoint is never one of them, nor is one a container still uses (Docker refuses).
    async fn prune_checkpoints(&self, id: &SandboxId, keep: &CheckpointId) -> Result<()> {
        let labels = self.read_state(&id.0).await?.labels;
        let filter = vec![format!("{CHECKPOINT_OF}={}", id.0), format!("{CHECKPOINT_NOTE}={WARM_NOTE}")];
        let opts =
            ListImagesOptions { filters: Some(HashMap::from([("label".to_string(), filter)])), ..Default::default() };
        for image in self.docker.list_images(Some(opts)).await.map_err(docker_err)? {
            let tags: Vec<&String> = image.repo_tags.iter().filter(|t| t.starts_with(CHECKPOINT_REPO)).collect();
            if tags.iter().any(|t| **t == keep.0 || labels.values().any(|v| v == *t)) {
                continue;
            }
            for tag in tags {
                match self.docker.remove_image(tag, None::<RemoveImageOptions>, None).await {
                    Err(e) if !matches!(status(&e), Some(404 | 409)) => return Err(docker_err(e)),
                    _ => {}
                }
            }
        }
        Ok(())
    }

    async fn sandbox(&self, name: &str) -> Result<Sandbox> {
        let state = self.read_state(name).await?;
        Ok(Sandbox { id: SandboxId(name.into()), name: name.into(), labels: state.labels })
    }
}

#[async_trait]
impl Compute for DockerCompute {
    async fn create(&self, spec: &SandboxSpec) -> Result<Sandbox> {
        let _link = self.gateway().await?;
        if self.exists(&spec.name).await? {
            return self.sandbox(&spec.name).await;
        }
        let state = State { labels: spec.labels.clone(), egress: spec.egress.clone() };
        self.write_state(&spec.name, &state).await?;
        self.attach(&spec.name, &state).await?;
        match self.run_container(&spec.name, &self.config.image, true).await {
            // Lost a race with another create of the same name.
            Err(Error::Conflict(_)) => {}
            other => other?,
        }
        self.sandbox(&spec.name).await
    }

    async fn list(&self, labels: &Labels) -> Result<Vec<Sandbox>> {
        // The first thing a restarted control plane calls, with sandboxes that may be waiting on the gateway.
        self.watch();
        let opts = ListContainersOptions {
            all: true,
            filters: Some(HashMap::from([("label".to_string(), vec![MARKER.to_string()])])),
            ..Default::default()
        };
        let containers = self.docker.list_containers(Some(opts)).await.map_err(docker_err)?;
        let mut out = Vec::new();
        for c in containers {
            let Some(name) = c.labels.as_ref().and_then(|l| l.get(MARKER)) else { continue };
            let sandbox = self.sandbox(name).await?;
            if labels.iter().all(|(k, v)| sandbox.labels.get(k) == Some(v)) {
                out.push(sandbox);
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Exports the paused container's filesystem and imports it as a single-layer image, so a checkpoint is about
    /// the size of the live filesystem however many came before it. The container's user, working directory and
    /// environment are carried over. Warm checkpoints of the sandbox that no label points at any more are removed
    /// afterwards.
    async fn checkpoint(&self, id: &SandboxId, note: &str) -> Result<CheckpointId> {
        let millis = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
        let cp = CheckpointId(format!("{CHECKPOINT_REPO}:{}-{millis}", id.0));
        let info = self.docker.inspect_container(&id.0, None::<InspectContainerOptions>).await.map_err(docker_err)?;
        let state = info.state.as_ref();
        let pause = state.and_then(|s| s.running) == Some(true) && state.and_then(|s| s.paused) != Some(true);
        let changes = image_changes(&id.0, note, info.config.as_ref());
        if pause {
            self.docker.pause_container(&id.0).await.map_err(docker_err)?;
        }
        let imported = self.import_filesystem(&id.0, &cp.0, note, changes).await;
        if pause && let Err(e) = self.docker.unpause_container(&id.0).await {
            tracing::warn!(sandbox = %id.0, error = %e, "couldn't unpause the sandbox after checkpointing it");
        }
        imported?;
        if let Err(e) = self.prune_checkpoints(id, &cp).await {
            tracing::warn!(sandbox = %id.0, error = %e, "couldn't remove old checkpoints");
        }
        Ok(cp)
    }

    /// Recreates the container from the checkpoint's image, keeping its name, labels and egress. Its programs start
    /// again from the image.
    async fn restore(&self, id: &SandboxId, cp: &CheckpointId) -> Result<()> {
        self.docker.inspect_image(&cp.0).await.map_err(docker_err)?;
        let _link = self.gateway().await?;
        let state = self.read_state(&id.0).await?;
        let opts = RemoveContainerOptions { force: true, ..Default::default() };
        match self.docker.remove_container(&id.0, Some(opts)).await {
            Err(e) if status(&e) != Some(404) => return Err(docker_err(e)),
            _ => {}
        }
        self.attach(&id.0, &state).await?;
        self.run_container(&id.0, &cp.0, false).await
    }

    /// Writes the files as the container's user (each beside its target, then renamed over it, so a running binary
    /// can be replaced), writes the program's run script and has the supervisor (re)start it. A program that exits
    /// within the first few seconds is an error carrying its output.
    async fn start(&self, id: &SandboxId, program: &Program) -> Result<()> {
        let name = &id.0;
        let (code, out) = self.exec(name, None, "id -u; id -g; echo \"$HOME\"").await?;
        let [uid, gid, home] = out.lines().collect::<Vec<_>>()[..] else {
            return Err(Error::Other(format!("sandbox {name}: unexpected identity ({code}): {out}")));
        };
        let (uid, gid) = (parse_id(uid)?, parse_id(gid)?);
        let dir = format!("{home}/.trailhead/programs/{}", program.name);
        let run = run_script(&program.cmd, &program.args);
        let mut files: Vec<(&str, &[u8], u32)> =
            program.files.iter().map(|f| (f.path.as_str(), f.contents.as_slice(), f.mode)).collect();
        let run_path = format!("{dir}/run");
        files.push((&run_path, run.as_bytes(), 0o755));

        let parents: Vec<&str> = files.iter().map(|(p, _, _)| parent(p)).collect();
        let mkdir = format!("mkdir -p {}", parents.iter().map(|p| shell_quote(p)).collect::<Vec<_>>().join(" "));
        let (code, out) = self.exec(name, None, &mkdir).await?;
        if code != 0 {
            return Err(Error::Other(format!("sandbox {name}: {mkdir} ({code}): {}", out.trim())));
        }
        let mut renames = Vec::new();
        for (path, contents, mode) in &files {
            let (dir, file) = (parent(path), path.rsplit('/').next().unwrap_or(path));
            let tmp = format!(".{file}.new");
            self.upload(name, dir, tar_of(&[(&tmp, contents, *mode, uid, gid)], &[])?).await?;
            renames.push(format!("mv -f {} {}", shell_quote(&format!("{dir}/{tmp}")), shell_quote(path)));
        }
        let (code, out) = self.exec(name, None, &renames.join(" && ")).await?;
        if code != 0 {
            return Err(Error::Other(format!("sandbox {name}: moving files into place ({code}): {}", out.trim())));
        }

        self.spawn(name, vec!["setsid".into(), SUPERVISE_PATH.into(), program.name.clone()]).await?;
        tokio::time::sleep(START_WATCH).await;
        let check = format!("cat {0}/exits; echo ---; tail -n 50 {0}/log", shell_quote(&dir));
        let (_, out) = self.exec(name, None, &check).await?;
        let (exits, log) = out.split_once("---\n").unwrap_or((&out, ""));
        match exits.lines().next() {
            Some(code) => Err(Error::Other(format!("program {} exited ({code}): {}", program.name, log.trim()))),
            None => Ok(()),
        }
    }

    /// Containers don't pause, but one can be stopped (by hand, or by the Docker host restarting without it), so
    /// this starts it if it isn't running.
    async fn wake(&self, id: &SandboxId) -> Result<()> {
        let info = self.docker.inspect_container(&id.0, None::<InspectContainerOptions>).await.map_err(docker_err)?;
        if info.state.and_then(|s| s.running) == Some(true) {
            return Ok(());
        }
        self.docker.start_container(&id.0, None).await.map_err(docker_err)
    }

    /// Returns once the gateway has the new list and has closed the tunnels it no longer allows.
    async fn set_egress(&self, id: &SandboxId, egress: &Egress) -> Result<()> {
        let _link = self.gateway().await?;
        if !self.exists(&id.0).await? {
            return Err(Error::NotFound(format!("sandbox {}", id.0)));
        }
        let mut state = self.read_state(&id.0).await?;
        state.egress = egress.clone();
        self.write_state(&id.0, &state).await?;
        self.attach(&id.0, &state).await
    }

    async fn set_labels(&self, id: &SandboxId, labels: &Labels) -> Result<()> {
        let _link = self.gateway().await?;
        if !self.exists(&id.0).await? {
            return Err(Error::NotFound(format!("sandbox {}", id.0)));
        }
        let mut state = self.read_state(&id.0).await?;
        state.labels = labels.clone();
        self.write_state(&id.0, &state).await?;
        self.attach(&id.0, &state).await
    }

    /// Fails when the sandbox's disk, which is the Docker engine's, has less than `min_free_bytes` free, so a run
    /// doesn't start only to fail partway through a build.
    async fn preflight(&self, id: &SandboxId) -> Result<()> {
        let (code, out) = self.exec(&id.0, None, "df -Pk /").await?;
        let Some(free) = (code == 0).then(|| free_bytes(&out)).flatten() else {
            return Err(Error::Other(format!(
                "sandbox {}: couldn't read its free disk space ({code}): {}",
                id.0,
                out.trim()
            )));
        };
        if free < self.config.min_free_bytes {
            return Err(Error::Other(format!(
                "sandbox {}: only {} MiB of disk free, below the {} MiB a task needs; free up space on the Docker host (docker system df)",
                id.0,
                free >> 20,
                self.config.min_free_bytes >> 20
            )));
        }
        Ok(())
    }

    /// Removes the container, its network, its checkpoint images and its state file. The state file goes last, so a
    /// teardown that stops partway is finished by the next resync.
    async fn destroy(&self, id: &SandboxId) -> Result<()> {
        let _link = self.gateway().await?;
        self.ctl_ok(&GatewayRequest::Unregister { name: id.0.clone() }).await?;
        let opts = RemoveContainerOptions { force: true, ..Default::default() };
        match self.docker.remove_container(&id.0, Some(opts)).await {
            Err(e) if status(&e) != Some(404) => return Err(docker_err(e)),
            _ => {}
        }
        self.detach(&id.0).await?;
        let filter = format!("{CHECKPOINT_OF}={}", id.0);
        let opts = ListImagesOptions {
            filters: Some(HashMap::from([("label".to_string(), vec![filter])])),
            ..Default::default()
        };
        for image in self.docker.list_images(Some(opts)).await.map_err(docker_err)? {
            let opts = RemoveImageOptions { force: true, ..Default::default() };
            match self.docker.remove_image(&image.id, Some(opts), None).await {
                Err(e) if status(&e) != Some(404) => return Err(docker_err(e)),
                _ => {}
            }
        }
        self.remove_state(&id.0).await
    }
}

/// A script that runs `cmd` with `args`, quoted for `sh`.
pub fn run_script(cmd: &str, args: &[String]) -> String {
    let words: Vec<String> = std::iter::once(cmd).chain(args.iter().map(String::as_str)).map(shell_quote).collect();
    format!("#!/bin/sh\nexec {}\n", words.join(" "))
}

pub fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The Dockerfile instructions that give an imported filesystem the container's identity: its checkpoint labels,
/// user, working directory and environment. (Its command and entrypoint are set again when it is restored.)
fn image_changes(sandbox: &str, note: &str, config: Option<&ContainerConfig>) -> Vec<String> {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$"));
    let mut out =
        vec![format!("LABEL {CHECKPOINT_OF}={}", quote(sandbox)), format!("LABEL {CHECKPOINT_NOTE}={}", quote(note))];
    if let Some(config) = config {
        if let Some(user) = config.user.as_deref().filter(|u| !u.is_empty()) {
            out.push(format!("USER {user}"));
        }
        if let Some(dir) = config.working_dir.as_deref().filter(|d| !d.is_empty()) {
            out.push(format!("WORKDIR {dir}"));
        }
        for var in config.env.iter().flatten() {
            if let Some((k, v)) = var.split_once('=') {
                out.push(format!("ENV {k}={}", quote(v)));
            }
        }
    }
    out
}

/// The free bytes on the filesystem `df -Pk` reports last.
fn free_bytes(df: &str) -> Option<u64> {
    let line = df.lines().rfind(|l| !l.trim().is_empty())?;
    let kib: u64 = line.split_whitespace().nth(3)?.parse().ok()?;
    kib.checked_mul(1024)
}

fn parent(path: &str) -> &str {
    match path.rsplit_once('/') {
        Some(("", _)) => "/",
        Some((dir, _)) => dir,
        None => ".",
    }
}

fn parse_id(s: &str) -> Result<u64> {
    s.trim().parse().map_err(|_| Error::Other(format!("unexpected id: {s}")))
}

/// A tar of files (path, contents, mode, uid, gid) and directories, owned as given so the sandbox's user can use them.
fn tar_of(files: &[(&str, &[u8], u32, u64, u64)], dirs: &[&str]) -> Result<Vec<u8>> {
    let mut tar = tar::Builder::new(Vec::new());
    for dir in dirs {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Directory);
        header.set_mode(0o755);
        header.set_uid(0);
        header.set_gid(0);
        header.set_size(0);
        tar.append_data(&mut header, dir, std::io::empty()).map_err(|e| Error::Other(e.to_string()))?;
    }
    for (path, contents, mode, uid, gid) in files {
        let mut header = tar::Header::new_gnu();
        header.set_mode(*mode);
        header.set_uid(*uid);
        header.set_gid(*gid);
        header.set_size(contents.len() as u64);
        tar.append_data(&mut header, path, *contents).map_err(|e| Error::Other(e.to_string()))?;
    }
    tar.into_inner().map_err(|e| Error::Other(e.to_string()))
}

fn status(e: &DockerError) -> Option<u16> {
    match e {
        DockerError::DockerResponseServerError { status_code, .. } => Some(*status_code),
        _ => None,
    }
}

fn docker_err(e: DockerError) -> Error {
    let msg = format!("docker: {e}");
    match status(&e) {
        Some(404) => Error::NotFound(msg),
        Some(409) => Error::Conflict(msg),
        Some(401 | 403) => Error::Forbidden(msg),
        Some(s) => Error::from_status(s, msg),
        None => Error::Other(msg),
    }
}

fn io_err(path: &Path, e: std::io::Error) -> Error {
    Error::Other(format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandboxes_go_through_the_gateway_but_not_to_it() {
        let env = gateway::proxy_env();
        for proxy in ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"] {
            assert!(env.contains(&format!("{proxy}=http://trailhead-gateway:8080")), "{env:?}");
        }
        assert!(env.contains(&"NO_PROXY=trailhead-gateway,localhost,127.0.0.1,::1".to_string()), "{env:?}");
    }

    #[test]
    fn run_scripts_quote_every_word() {
        let args = vec!["--secrets".into(), "/home/trailhead/it's.json".into()];
        assert_eq!(
            run_script("/home/trailhead/.trailhead/trailhead-daemon", &args),
            "#!/bin/sh\nexec '/home/trailhead/.trailhead/trailhead-daemon' '--secrets' '/home/trailhead/it'\\''s.json'\n"
        );
    }

    #[test]
    fn docker_client_errors_that_repeat_are_rejected() {
        let err = |status_code| DockerError::DockerResponseServerError { status_code, message: "no".into() };
        assert!(matches!(docker_err(err(400)), Error::Rejected(_)));
        assert!(matches!(docker_err(err(404)), Error::NotFound(_)));
        assert!(matches!(docker_err(err(429)), Error::Other(_)));
        assert!(matches!(docker_err(err(500)), Error::Other(_)));
    }

    #[test]
    fn checkpoint_images_keep_the_containers_identity() {
        let config = ContainerConfig {
            user: Some("trailhead".into()),
            working_dir: Some("/work".into()),
            env: Some(vec!["PATH=/usr/bin:/bin".into(), "NOTE=a \"b\" $c".into()]),
            ..Default::default()
        };
        assert_eq!(
            image_changes("t-w1", "warm", Some(&config)),
            [
                "LABEL dev.trailhead.checkpoint-of=\"t-w1\"",
                "LABEL dev.trailhead.checkpoint-note=\"warm\"",
                "USER trailhead",
                "WORKDIR /work",
                "ENV PATH=\"/usr/bin:/bin\"",
                "ENV NOTE=\"a \\\"b\\\" \\$c\"",
            ]
        );
        assert_eq!(image_changes("t-w1", "warm", None).len(), 2);
    }

    #[test]
    fn free_space_is_read_from_df() {
        let df = "Filesystem 1024-blocks Used Available Capacity Mounted on\noverlay 60000000 50000000 1000 99% /\n";
        assert_eq!(free_bytes(df), Some(1000 * 1024));
        assert_eq!(free_bytes("garbage"), None);
    }

    #[test]
    fn parents() {
        assert_eq!(parent("/home/trailhead/.trailhead/secrets.json"), "/home/trailhead/.trailhead");
        assert_eq!(parent("/x"), "/");
        assert_eq!(parent("x"), ".");
    }

    #[test]
    fn tars_carry_owner_and_mode() {
        let raw = tar_of(&[(".secrets.json.new", b"{}", 0o600, 1001, 1002)], &["opt/trailhead"]).unwrap();
        let mut archive = tar::Archive::new(raw.as_slice());
        let entries: Vec<_> = archive
            .entries()
            .unwrap()
            .map(|e| {
                let h = e.unwrap().header().clone();
                (h.path().unwrap().display().to_string(), h.mode().unwrap(), h.uid().unwrap(), h.gid().unwrap())
            })
            .collect();
        assert_eq!(entries, [("opt/trailhead".into(), 0o755, 0, 0), (".secrets.json.new".into(), 0o600, 1001, 1002)]);
    }

    #[tokio::test]
    async fn state_round_trips_and_defaults_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let docker = Docker::connect_with_http("http://127.0.0.1:1", 1, bollard::API_DEFAULT_VERSION).unwrap();
        let config = DockerConfig::new(dir.path().join("state"), Default::default());
        let compute = DockerCompute::with_client(docker, config);
        assert_eq!(compute.read_state("trail-1-a1").await.unwrap(), State::default());
        let state = State {
            labels: Labels::from([("role".into(), "worker".into())]),
            egress: Egress { domains: vec!["crates.io".into()] },
        };
        compute.write_state("trail-1-a1", &state).await.unwrap();
        assert_eq!(compute.read_state("trail-1-a1").await.unwrap(), state);
    }
}
