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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bollard::Docker;
use bollard::errors::Error as DockerError;
use bollard::exec::{StartExecOptions, StartExecResults};
use bollard::models::{
    ContainerConfig, ContainerCreateBody, ExecConfig, HostConfig, ImageSummary, RestartPolicy, RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    CreateContainerOptions, CreateImageOptions, DownloadFromContainerOptions, InspectContainerOptions,
    ListContainersOptions, ListImagesOptions, RemoveContainerOptions, RemoveImageOptions, UploadToContainerOptions,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use trailhead_factory::{
    CheckpointId, Compute, ComputeConfig, Egress, Error, GATEWAY_ANTHROPIC_ID, GATEWAY_HOST, GATEWAY_PORT,
    GatewayConnection, GatewayRequest, Labels, Program, Result, Sandbox, SandboxId, SandboxSpec, Target,
};

mod gateway;

pub use gateway::{GATEWAY_BIN, GATEWAY_LABEL, sandbox_network};

/// The label every sandbox container carries, set to its name.
pub const MARKER: &str = "dev.trailhead.sandbox";
/// The label on checkpoint images, set to the sandbox they came from, so `destroy` can remove them.
pub const CHECKPOINT_OF: &str = "dev.trailhead.checkpoint-of";
/// The label on checkpoint images holding the note they were taken with.
pub const CHECKPOINT_NOTE: &str = "dev.trailhead.checkpoint-note";
/// The label on checkpoint images holding, as a JSON array of `NAME=value`, the environment variables whose values
/// have a line break: a Dockerfile `ENV` can't carry one, so a restore sets them on the container instead.
const CHECKPOINT_ENV: &str = "dev.trailhead.checkpoint-env";
/// The note of a pool's warm checkpoints, the only ones a later checkpoint may remove: the rest (a parked
/// sandbox's) stay until the sandbox is destroyed.
const WARM_NOTE: &str = "warm";
/// The variable capping cargo's parallelism to the memory limit; set on each container, not carried in checkpoints.
const BUILD_JOBS_VAR: &str = "CARGO_BUILD_JOBS";
/// The repository checkpoint images are imported to.
pub const CHECKPOINT_REPO: &str = "trailhead-checkpoint";
/// How long a checkpoint's export and import may take: a whole filesystem, far past the client's usual two minutes.
const CHECKPOINT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// The sandbox image built by `mise run docker-image`.
pub const DEFAULT_IMAGE: &str = "trailhead-sandbox:latest";
/// The ordinary bridge network the gateway gets out on. Sandboxes are never on it.
pub const DEFAULT_NETWORK: &str = "trailhead-gateway-out";
/// Where the daemon lives in [`DEFAULT_IMAGE`], whose user is `trailhead`.
pub const HOME: &str = "/home/trailhead";

/// How many trailing lines of a program's log `logs` returns.
const LOG_LINES: usize = 3000;

/// The tail of `bytes` holding its last `lines` lines (a final line without a newline counts).
fn last_lines(bytes: &[u8], lines: usize) -> &[u8] {
    if lines == 0 {
        return &[];
    }
    let body = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    let mut seen = 0;
    for (i, b) in body.iter().enumerate().rev() {
        if *b == b'\n' {
            seen += 1;
            if seen == lines {
                return &bytes[i + 1..];
            }
        }
    }
    bytes
}

/// The last `lines` lines of the file in a `docker cp` archive of one file.
fn log_from_archive(raw: &[u8], lines: usize) -> Result<String> {
    let bad = |e: std::io::Error| Error::Other(format!("log archive: {e}"));
    let mut archive = tar::Archive::new(raw);
    for entry in archive.entries().map_err(bad)? {
        let mut entry = entry.map_err(bad)?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let mut bytes = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut bytes).map_err(bad)?;
        // Trim on the bytes, then decode lossily as the exec path does, so a log with stray bytes still reads.
        return Ok(String::from_utf8_lossy(last_lines(&bytes, lines)).into_owned());
    }
    Err(Error::NotFound("log archive holds no file".into()))
}

/// A program's log under the target's `dir`, quoted for the shell.
fn program_log(dir: &str, program: &str) -> String {
    shell_quote(&format!("{dir}/programs/{program}/log"))
}

/// A Docker sandbox's launch facts: connectors and model calls both go through the gateway container.
pub fn target() -> Target {
    let gateway = format!("http://{GATEWAY_HOST}:{GATEWAY_PORT}/v1/gateway");
    Target {
        dir: format!("{HOME}/.trailhead"),
        workdir: format!("{HOME}/work"),
        gateway_list: format!("{gateway}/list"),
        model_endpoint: Some(format!("{gateway}/anthropic/{GATEWAY_ANTHROPIC_ID}")),
        hold_socket: None,
        ..Target::default()
    }
}

/// A Docker compute from its `[factory.compute]` settings (unset ones keep `docker`'s defaults), with its launch facts.
/// `read_only` is as for [`DockerCompute::read_only`]. Settings of another kind are an error.
pub fn build(
    settings: &ComputeConfig,
    mut docker: DockerConfig,
    read_only: bool,
) -> Result<(Arc<dyn Compute>, Target)> {
    let ComputeConfig::Docker { image, network, runtime, memory_mib, min_free_bytes, concurrent_checkpoints } =
        settings
    else {
        return Err(Error::Other("not a Docker compute's settings".into()));
    };
    if let Some(image) = image {
        docker.image = image.clone();
    }
    if let Some(network) = network {
        docker.network = network.clone();
    }
    docker.runtime = runtime.clone();
    docker.memory_bytes = memory_mib.map(|m| m * 1024 * 1024);
    if let Some(min_free) = min_free_bytes {
        docker.min_free_bytes = *min_free;
    }
    if let Some(n) = concurrent_checkpoints {
        docker.concurrent_checkpoints = *n;
    }
    let compute = if read_only { DockerCompute::read_only(docker) } else { DockerCompute::new(docker) }?;
    Ok((Arc::new(compute), target()))
}

const SUPERVISE: &str = include_str!("supervise.sh");
const SUPERVISE_PATH: &str = "/opt/trailhead/supervise";

/// The least free disk, in bytes, a run may start with (`DockerConfig::min_free_bytes`). A floor: a sandbox with a
/// big filesystem needs more ([`disk_needed`]).
pub const DEFAULT_MIN_FREE_BYTES: u64 = 10 << 30;

/// How many times its size a checkpoint takes while it is imported: the containerd snapshotter keeps an import as a
/// content blob and again as an unpacked snapshot (`docker system df` showed twice the sizes' sum).
const IMPORT_COPIES: u64 = 2;
/// How many sandboxes may be checkpointing at once by default (`DockerConfig::concurrent_checkpoints`).
pub const DEFAULT_CONCURRENT_CHECKPOINTS: u64 = 2;

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
    /// How many sandboxes may checkpoint at once, which the free-disk check reserves room for: set it to the pool size.
    pub concurrent_checkpoints: u64,
    /// Each sandbox's memory limit, swap included. None leaves sandboxes unlimited, so concurrent builds can take each
    /// other's memory and the engine's OOM killer picks among them. Also sets `CARGO_BUILD_JOBS` ([`build_jobs`]).
    pub memory_bytes: Option<u64>,
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
            concurrent_checkpoints: DEFAULT_CONCURRENT_CHECKPOINTS,
            memory_bytes: None,
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
    /// The SHA-256 of `config.daemon`, hex.
    daemon_digest: Arc<str>,
    watching: Arc<AtomicBool>,
    /// Set once the first `list` has pruned the checkpoints earlier runs left behind.
    swept: Arc<AtomicBool>,
    /// Set by [`DockerCompute::read_only`]: calls that change a sandbox are refused.
    read_only: bool,
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
        let daemon_digest = gateway::digest(&config.daemon).into();
        DockerCompute {
            docker,
            config,
            link: Default::default(),
            daemon_digest,
            watching: Default::default(),
            swept: Default::default(),
            read_only: false,
        }
    }

    /// Connects like [`DockerCompute::new`] for reading only (`list`, `logs`): it never starts, replaces or syncs the
    /// gateway, so it's safe beside a running factory and needs no daemon binary. A sandbox it changed would have no
    /// gateway behind it, so don't use it for that.
    pub fn read_only(config: DockerConfig) -> Result<DockerCompute> {
        let docker = Docker::connect_with_defaults().map_err(docker_err)?;
        Ok(DockerCompute::read_only_with_client(docker, config))
    }

    pub fn read_only_with_client(docker: Docker, config: DockerConfig) -> DockerCompute {
        let mut compute = DockerCompute::with_client(docker, config);
        compute.read_only = true;
        // The watcher is what starts and syncs the gateway; marking it started keeps it from ever running.
        compute.watching.store(true, Ordering::SeqCst);
        compute
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

    /// The environment of a new container: the proxy's, and cargo's parallelism when the memory is limited.
    fn sandbox_env(&self) -> Vec<String> {
        let mut env = gateway::proxy_env();
        if let Some(bytes) = self.config.memory_bytes {
            env.push(format!("{BUILD_JOBS_VAR}={}", build_jobs(bytes)));
        }
        env
    }

    /// Creates and starts the sandbox's container from `image`. A fresh image gets the supervisor first; a checkpoint
    /// already has it.
    async fn run_container(&self, name: &str, image: &str, fresh: bool, extra_env: Vec<String>) -> Result<()> {
        let body = ContainerCreateBody {
            image: Some(image.into()),
            cmd: Some(vec![SUPERVISE_PATH.into()]),
            entrypoint: Some(vec![]),
            env: Some([self.sandbox_env(), extra_env].concat()),
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
                // Memory equal to memory+swap means no swap, so a build over the limit is OOM-killed at once instead
                // of thrashing.
                memory: self.config.memory_bytes.map(|b| b as i64),
                memory_swap: self.config.memory_bytes.map(|b| b as i64),
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

    /// Removes the image tagged `image`, if there is one. Used for what a failed import left behind.
    async fn remove_image_tag(&self, image: &str) {
        match self
            .docker
            .remove_image(image, Some(RemoveImageOptions { force: true, ..Default::default() }), None)
            .await
        {
            Err(e) if status(&e) != Some(404) => {
                tracing::warn!(image, error = %e, "couldn't remove a checkpoint image; reconcile or destroy will");
            }
            _ => {}
        }
    }

    /// Streams the container's filesystem into a new image tagged `image`.
    async fn import_filesystem(&self, name: &str, image: &str, note: &str, changes: Vec<String>) -> Result<()> {
        // The export is read by a task of its own and passed through a channel, so the request body owns what it needs.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<std::io::Result<bytes::Bytes>>(4);
        let docker = self.docker.clone().with_timeout(CHECKPOINT_TIMEOUT);
        let (exporter, owned) = (docker.clone(), name.to_string());
        let export = tokio::spawn(async move {
            let mut tar = exporter.export_container(&owned);
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
        let mut progress = docker.create_image(Some(opts), Some(body), None);
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

    /// Removes the sandbox's warm checkpoints other than `keep` that none of its labels point at, and those from before
    /// checkpoints were noted (no note label). A parked sandbox's checkpoint is never one of them, nor is one a
    /// container still uses (Docker refuses).
    async fn prune_checkpoints(&self, id: &SandboxId, keep: Option<&CheckpointId>) -> Result<()> {
        let labels = self.read_state(&id.0).await?.labels;
        let filter = vec![format!("{CHECKPOINT_OF}={}", id.0)];
        let opts =
            ListImagesOptions { filters: Some(HashMap::from([("label".to_string(), filter)])), ..Default::default() };
        for image in self.docker.list_images(Some(opts)).await.map_err(docker_err)? {
            if image.labels.get(CHECKPOINT_NOTE).is_some_and(|n| n != WARM_NOTE) {
                continue;
            }
            let tags: Vec<&String> = image.repo_tags.iter().filter(|t| t.starts_with(CHECKPOINT_REPO)).collect();
            if tags.iter().any(|t| keep.is_some_and(|k| **t == k.0) || labels.values().any(|v| v == *t)) {
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

    /// The size in bytes and the name of the checkpoint image that sets the sandbox's disk need: the largest of its own
    /// (a stale image of another sandbox must not hold its runs back), or, while it has none, the largest
    /// `trailhead-checkpoint` image on the host. `Ok(None)` if there is none; `Err` if they can't be listed (the check
    /// then falls back to the container's own size).
    async fn largest_checkpoint(&self, id: &SandboxId) -> Result<Option<(u64, String)>, bollard::errors::Error> {
        let filter = vec![CHECKPOINT_REPO.to_string()];
        let opts = ListImagesOptions {
            filters: Some(HashMap::from([("reference".to_string(), filter)])),
            ..Default::default()
        };
        let images = self.docker.list_images(Some(opts)).await?;
        let containers = ListContainersOptions {
            all: true,
            filters: Some(HashMap::from([("label".to_string(), vec![MARKER.to_string()])])),
            ..Default::default()
        };
        let live: HashSet<String> = self
            .docker
            .list_containers(Some(containers))
            .await?
            .into_iter()
            .filter_map(|c| c.labels.and_then(|mut l| l.remove(MARKER)))
            .collect();
        Ok(checkpoint_of(&id.0, &images, &live))
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
        match self.run_container(&spec.name, &self.config.image, true, Vec::new()).await {
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
        // Once per process, for images an earlier run left behind. Not on every list: a checkpoint just taken is
        // unlabelled and unused until its restore, and would be pruned.
        if !self.read_only && !self.swept.swap(true, Ordering::SeqCst) {
            for sandbox in &out {
                if let Err(e) = self.prune_checkpoints(&sandbox.id, None).await {
                    tracing::warn!(sandbox = %sandbox.id.0, error = %e, "couldn't remove old checkpoints at startup");
                }
            }
        }
        Ok(out)
    }

    /// Exports the paused container's filesystem and imports it as a single-layer image, so a checkpoint is about
    /// the size of the live filesystem however many came before it. The container's user, working directory and
    /// environment are carried over. Warm checkpoints of the sandbox that no label points at any more are removed
    /// afterwards.
    async fn checkpoint(&self, id: &SandboxId, note: &str) -> Result<CheckpointId> {
        self.writable()?;
        let millis = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
        let cp = CheckpointId(format!("{CHECKPOINT_REPO}:{}-{millis}", id.0));
        let info = self.docker.inspect_container(&id.0, None::<InspectContainerOptions>).await.map_err(docker_err)?;
        let state = info.state.as_ref();
        let pause = state.and_then(|s| s.running) == Some(true) && state.and_then(|s| s.paused) != Some(true);
        let changes = image_changes(&id.0, note, info.config.as_ref());
        if pause {
            self.docker.pause_container(&id.0).await.map_err(docker_err)?;
        }
        // Before the import, which needs the room: a failed one leaves its image behind, and so did earlier ones.
        if let Err(e) = self.prune_checkpoints(id, None).await {
            tracing::warn!(sandbox = %id.0, error = %e, "couldn't remove old checkpoints before the import");
        }
        let imported = self.import_filesystem(&id.0, &cp.0, note, changes).await;
        if pause && let Err(e) = self.docker.unpause_container(&id.0).await {
            tracing::warn!(sandbox = %id.0, error = %e, "couldn't unpause the sandbox after checkpointing it");
        }
        if let Err(e) = imported {
            // A failed import can leave a partial image of a few GB tagged; it must not stay to fill the disk.
            self.remove_image_tag(&cp.0).await;
            return Err(if full_disk(&e.to_string()) {
                Error::Unavailable(format!(
                    "sandbox {}: the Docker host's disk filled while checkpointing it ({e}); free up space on the host (docker system df, docker image prune)",
                    id.0
                ))
            } else {
                e
            });
        }
        if let Err(e) = self.prune_checkpoints(id, Some(&cp)).await {
            tracing::warn!(sandbox = %id.0, error = %e, "couldn't remove old checkpoints");
        }
        Ok(cp)
    }

    async fn delete_checkpoint(&self, _id: &SandboxId, cp: &CheckpointId) -> Result<()> {
        self.writable()?;
        match self.docker.remove_image(&cp.0, None::<RemoveImageOptions>, None).await {
            Err(e) if status(&e) != Some(404) => Err(docker_err(e)),
            _ => Ok(()),
        }
    }

    /// Recreates the container from the checkpoint's image, keeping its name, labels and egress. Its programs start
    /// again from the image.
    async fn restore(&self, id: &SandboxId, cp: &CheckpointId) -> Result<()> {
        let image = self.docker.inspect_image(&cp.0).await.map_err(docker_err)?;
        let multi_line = multi_line_env(image.config.as_ref().and_then(|c| c.labels.as_ref()));
        let _link = self.gateway().await?;
        let state = self.read_state(&id.0).await?;
        let opts = RemoveContainerOptions { force: true, ..Default::default() };
        match self.docker.remove_container(&id.0, Some(opts)).await {
            Err(e) if status(&e) != Some(404) => return Err(docker_err(e)),
            _ => {}
        }
        self.attach(&id.0, &state).await?;
        self.run_container(&id.0, &cp.0, false, multi_line).await?;
        // The container no longer runs from the previous checkpoint, which checkpoint() couldn't remove while it did.
        if let Err(e) = self.prune_checkpoints(id, Some(cp)).await {
            tracing::warn!(sandbox = %id.0, error = %e, "couldn't remove old checkpoints after the restore");
        }
        Ok(())
    }

    /// Writes the files as the container's user (each beside its target, then renamed over it, so a running binary
    /// can be replaced), writes the program's run script and has the supervisor (re)start it. A program that exits
    /// within the first few seconds is an error carrying its output.
    async fn start(&self, id: &SandboxId, program: &Program) -> Result<()> {
        self.writable()?;
        let name = &id.0;
        let (code, out) = self.exec(name, None, "id -u; id -g; echo \"$HOME\"").await?;
        let [uid, gid, home] = out.lines().collect::<Vec<_>>()[..] else {
            return Err(Error::Other(format!("sandbox {name}: unexpected identity ({code}): {out}")));
        };
        let (uid, gid) = (parse_id(uid)?, parse_id(gid)?);
        let dir = format!("{home}/.trailhead/programs/{}", program.name);
        let run = run_script(&program.cmd, &program.args);
        let mut files: Vec<(&str, &[u8], u32)> =
            program.files.iter().map(|f| (f.path.as_str(), f.contents.as_ref(), f.mode)).collect();
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
        self.writable()?;
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

    /// Reads the log by `tail` in a running container, or by copying it out of a stopped one, which can't exec.
    async fn logs(&self, id: &SandboxId, program: &str) -> Result<String> {
        let info = self.docker.inspect_container(&id.0, None::<InspectContainerOptions>).await.map_err(docker_err)?;
        if info.state.and_then(|s| s.running).unwrap_or(false) {
            let log = program_log(&target().dir, program);
            let (_, out) = self.exec(&id.0, None, &format!("tail -n {LOG_LINES} {log}")).await?;
            return Ok(out);
        }
        let path = format!("{}/programs/{program}/log", target().dir);
        let opts = DownloadFromContainerOptions { path };
        let mut stream = self.docker.download_from_container(&id.0, Some(opts));
        let mut raw = Vec::new();
        while let Some(chunk) = stream.next().await {
            raw.extend_from_slice(&chunk.map_err(docker_err)?);
        }
        log_from_archive(&raw, LOG_LINES)
    }

    /// Fails with [`Error::Unavailable`], for the control plane to wait out, when the sandbox's disk (the Docker
    /// engine's) has less free than [`disk_needed`] for the larger of its filesystem and its own biggest checkpoint image
    /// (the host's biggest while it has none), so a run doesn't start only to fail partway through a build or a checkpoint.
    async fn preflight(&self, id: &SandboxId) -> Result<()> {
        let (code, out) = self.exec(&id.0, None, "df -Pk /").await?;
        let Some(free) = (code == 0).then(|| free_bytes(&out)).flatten() else {
            return Err(Error::Other(format!(
                "sandbox {}: couldn't read its free disk space ({code}): {}",
                id.0,
                out.trim()
            )));
        };
        let opts = InspectContainerOptions { size: true };
        let info = self.docker.inspect_container(&id.0, Some(opts)).await.map_err(docker_err)?;
        let fs = info.size_root_fs.unwrap_or(0).max(0) as u64;
        // A new sandbox's filesystem is small, but its checkpoint after prepare is not: size the need by its own
        // largest checkpoint image, or the host's largest while it has none.
        let listed = self.largest_checkpoint(id).await;
        if let Err(e) = &listed {
            tracing::warn!(error = %e, "couldn't list checkpoint images to size the disk a run needs");
        }
        let checkpoint = listed.as_ref().ok().and_then(Option::as_ref);
        let size = disk_size(fs, checkpoint);
        let need = disk_needed(self.config.min_free_bytes, self.config.concurrent_checkpoints, size);
        if free < need {
            return Err(Error::Unavailable(disk_short(&id.0, free, need, fs, listed.is_ok(), checkpoint)));
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
    // Docker joins the changes with newlines and parses them as a Dockerfile, so a line break would end the
    // instruction early, and no escape keeps one (`\n` stays a backslash and an n); turn each into a space. An
    // environment variable's value keeps its own through `CHECKPOINT_ENV`, whose JSON has none.
    let quote = |s: &str| {
        let s = s.replace(['\r', '\n'], " ");
        format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('$', "\\$"))
    };
    let mut out =
        vec![format!("LABEL {CHECKPOINT_OF}={}", quote(sandbox)), format!("LABEL {CHECKPOINT_NOTE}={}", quote(note))];
    let mut multi_line = Vec::new();
    if let Some(config) = config {
        if let Some(user) = config.user.as_deref().filter(|u| !u.is_empty()) {
            out.push(format!("USER {user}"));
        }
        if let Some(dir) = config.working_dir.as_deref().filter(|d| !d.is_empty()) {
            out.push(format!("WORKDIR {dir}"));
        }
        for var in config.env.iter().flatten() {
            // Left out so a restore sets it afresh from the current `memory_mib`, and drops it once that is unset.
            if let Some((k, v)) = var.split_once('=').filter(|(k, _)| *k != BUILD_JOBS_VAR) {
                if v.contains(['\r', '\n']) {
                    multi_line.push(var.clone());
                } else {
                    out.push(format!("ENV {k}={}", quote(v)));
                }
            }
        }
    }
    if !multi_line.is_empty() {
        out.push(format!("LABEL {CHECKPOINT_ENV}={}", quote(&serde_json::to_string(&multi_line).unwrap_or_default())));
    }
    out
}

/// The variables a checkpoint image's [`CHECKPOINT_ENV`] label carries, for the restored container to set.
fn multi_line_env(labels: Option<&HashMap<String, String>>) -> Vec<String> {
    labels.and_then(|l| l.get(CHECKPOINT_ENV)).and_then(|v| serde_json::from_str(v).ok()).unwrap_or_default()
}

/// `images` as `(of, size, name)` tuples for [`pick_checkpoint`], each image named by its first checkpoint tag, or by
/// its ID when it has none. `live` names the sandboxes whose containers still exist.
fn checkpoint_of(sandbox: &str, images: &[ImageSummary], live: &HashSet<String>) -> Option<(u64, String)> {
    let sized = images
        .iter()
        .map(|i| {
            let name = i.repo_tags.iter().find(|t| t.starts_with(CHECKPOINT_REPO)).unwrap_or(&i.id);
            (i.labels.get(CHECKPOINT_OF).map(String::as_str), i.size.max(0) as u64, name.clone())
        })
        .collect();
    pick_checkpoint(sandbox, sized, live)
}

/// The size the disk need follows: the larger of the container's own `fs` size and the checkpoint image's.
fn disk_size(fs: u64, checkpoint: Option<&(u64, String)>) -> u64 {
    checkpoint.map_or(fs, |(big, _)| fs.max(*big))
}

/// The largest of `images` (the sandbox each is a checkpoint of, its size and its name) made by `sandbox`, or else the
/// largest of those of sandboxes in `live` (an image whose sandbox is gone must not hold a new one back).
fn pick_checkpoint(
    sandbox: &str,
    images: Vec<(Option<&str>, u64, String)>,
    live: &HashSet<String>,
) -> Option<(u64, String)> {
    let largest = |own: bool| {
        images
            .iter()
            .filter(|(of, _, _)| if own { *of == Some(sandbox) } else { of.is_some_and(|o| live.contains(o)) })
            .max_by_key(|(_, size, _)| *size)
            .map(|(_, size, name)| (*size, name.clone()))
    };
    largest(true).or_else(|| largest(false))
}

/// The free disk a sandbox whose filesystem is `size` bytes needs before a run: its checkpoint is imported as
/// [`IMPORT_COPIES`] copies of it, for `concurrent` sandboxes checkpointing together, and never less than `floor`.
fn disk_needed(floor: u64, concurrent: u64, size: u64) -> u64 {
    floor.max(size.saturating_mul(IMPORT_COPIES.saturating_mul(concurrent)))
}

/// How many parallel build jobs fit in `memory_bytes`: one per GiB (a rust-lld link or rustc codegen unit can take that
/// much), at least one.
fn build_jobs(memory_bytes: u64) -> u64 {
    (memory_bytes / (1 << 30)).max(1)
}

/// Whether an error message says the disk is full.
fn full_disk(msg: &str) -> bool {
    let msg = msg.to_ascii_lowercase();
    msg.contains("no space left on device") || msg.contains("disk quota exceeded")
}

/// The message for a sandbox short of disk, naming both sizes the need could rest on so an operator knows what to
/// free: the container's filesystem, or the largest checkpoint image (which `docker rmi` frees if it is stale).
///
/// `checkpoint` is the largest image listed, whether or not it is larger than `fs`; the message says when it is not
/// (then the filesystem sets the need), and `None` means none exists.
///
/// `listed` is whether the checkpoint images could be listed: if not, their size is unknown rather than absent.
fn disk_short(
    sandbox: &str,
    free: u64,
    need: u64,
    fs: u64,
    listed: bool,
    checkpoint: Option<&(u64, String)>,
) -> String {
    let checkpoint = match checkpoint {
        Some((big, name)) if *big > fs => {
            format!("largest checkpoint {} MiB, image {name}; `docker rmi {name}` frees it if it is stale", big >> 20)
        }
        Some((big, name)) => format!("largest checkpoint {} MiB, image {name}, smaller than the filesystem", big >> 20),
        None if listed => "no checkpoint image".to_string(),
        None => "checkpoint size unknown, the images couldn't be listed".to_string(),
    };
    format!(
        "sandbox {sandbox}: only {} MiB of disk free, below the {} MiB a task and its checkpoint need (filesystem {} MiB, {checkpoint}); free up space on the Docker host (docker system df)",
        free >> 20,
        need >> 20,
        fs >> 20
    )
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
    fn build_refuses_another_kinds_settings() {
        let docker = DockerConfig::new("/tmp/unused", Default::default());
        assert!(build(&ComputeConfig::Sprites, docker, true).is_err());
    }

    /// A Docker API on a unix socket that answers every request with an empty list and records the paths asked for.
    async fn recording_docker(dir: &Path) -> (Docker, Arc<std::sync::Mutex<Vec<String>>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let sock = dir.join("docker.sock");
        let listener = tokio::net::UnixListener::bind(&sock).unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut conn, _)) = listener.accept().await else { return };
                let log = log.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    while let Ok(n) = conn.read(&mut buf).await {
                        if n == 0 {
                            return;
                        }
                        let req = String::from_utf8_lossy(&buf[..n]).into_owned();
                        if let Some(path) = req.split_whitespace().nth(1) {
                            log.lock().unwrap().push(path.to_string());
                        }
                        let reply = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n[]";
                        if conn.write_all(reply.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        let docker = Docker::connect_with_unix(sock.to_str().unwrap(), 5, bollard::API_DEFAULT_VERSION).unwrap();
        (docker, seen)
    }

    #[tokio::test]
    async fn listing_read_only_never_touches_the_gateway() {
        let dir = tempfile::tempdir().unwrap();
        let (docker, seen) = recording_docker(dir.path()).await;
        let config = DockerConfig::new(dir.path().join("state"), Default::default());
        let compute = DockerCompute::read_only_with_client(docker, config);
        assert!(compute.list(&Labels::default()).await.unwrap().is_empty());
        tokio::time::sleep(Duration::from_millis(300)).await;
        let paths = seen.lock().unwrap().clone();
        assert!(!paths.is_empty());
        assert!(paths.iter().all(|p| p.contains("/containers/json")), "{paths:?}");
    }

    #[tokio::test]
    async fn changing_read_only_is_refused_before_the_gateway() {
        let dir = tempfile::tempdir().unwrap();
        let (docker, seen) = recording_docker(dir.path()).await;
        let config = DockerConfig::new(dir.path().join("state"), Default::default());
        let compute = DockerCompute::read_only_with_client(docker, config);
        assert!(compute.gateway().await.is_err());
        let err = compute.set_labels(&SandboxId("x".into()), &Labels::default()).await.unwrap_err();
        assert!(err.to_string().contains("read-only"), "{err}");
        let id = SandboxId("x".into());
        let program = Program { name: "p".into(), cmd: "true".into(), args: vec![], files: vec![] };
        for err in [
            compute.checkpoint(&id, "n").await.unwrap_err(),
            compute.start(&id, &program).await.unwrap_err(),
            compute.wake(&id).await.unwrap_err(),
        ] {
            assert!(err.to_string().contains("read-only"), "{err}");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(seen.lock().unwrap().is_empty(), "{:?}", seen.lock().unwrap());
    }

    #[tokio::test]
    async fn listing_normally_watches_the_gateway() {
        // The control for the test above: the same list without `read_only` does go looking for the gateway.
        let dir = tempfile::tempdir().unwrap();
        let (docker, seen) = recording_docker(dir.path()).await;
        let config = DockerConfig::new(dir.path().join("state"), Default::default());
        let compute = DockerCompute::with_client(docker, config);
        compute.list(&Labels::default()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let paths = seen.lock().unwrap().clone();
        assert!(paths.iter().any(|p| p.contains("trailhead-gateway")), "{paths:?}");
    }

    #[test]
    fn sandboxes_go_through_the_gateway_but_not_to_it() {
        let env = gateway::proxy_env();
        for proxy in ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"] {
            assert!(env.contains(&format!("{proxy}=http://trailhead-gateway:8080")), "{env:?}");
        }
        assert!(env.contains(&"NO_PROXY=trailhead-gateway,localhost,127.0.0.1,::1".to_string()), "{env:?}");
    }

    #[test]
    fn the_daemon_digest_follows_the_binary() {
        let (a, b) = (gateway::digest(b"one"), gateway::digest(b"two"));
        assert_eq!(a, gateway::digest(b"one"));
        assert_ne!(a, b);
        assert_eq!(a.len(), 64);
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
            env: Some(vec!["PATH=/usr/bin:/bin".into(), "NOTE=a \"b\" $c".into(), "CARGO_BUILD_JOBS=2".into()]),
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
    fn a_multi_line_env_value_rides_in_a_label_and_comes_back_whole() {
        let value = "-----BEGIN-----\nabc \"q\" \\n $x\r\n-----END-----";
        let config = ContainerConfig { env: Some(vec![format!("KEY={value}"), "A=b".into()]), ..Default::default() };
        let changes = image_changes("t-w1", "warm", Some(&config));
        assert!(changes.iter().all(|c| !c.contains(['\n', '\r'])));
        assert!(!changes.iter().any(|c| c.starts_with("ENV KEY")));
        assert!(changes.contains(&"ENV A=\"b\"".to_string()));
        let label = changes.iter().find_map(|c| c.strip_prefix(&format!("LABEL {CHECKPOINT_ENV}="))).unwrap();
        // Undo the Dockerfile quoting the way its parser does.
        let unquoted = label[1..label.len() - 1].replace("\\\"", "\"").replace("\\$", "$").replace("\\\\", "\\");
        let labels = HashMap::from([(CHECKPOINT_ENV.to_string(), unquoted)]);
        assert_eq!(multi_line_env(Some(&labels)), [format!("KEY={value}")]);
        assert!(multi_line_env(None).is_empty());
    }

    #[test]
    fn a_multi_line_note_stays_on_one_line() {
        let changes = image_changes("t-w1", "parked: first\nsecond\r\nthird", None);
        assert_eq!(changes[1], "LABEL dev.trailhead.checkpoint-note=\"parked: first second  third\"");
        assert!(changes.iter().all(|c| !c.contains(['\n', '\r'])));
    }

    #[test]
    fn the_disk_short_message_names_the_filesystem_and_the_largest_checkpoint() {
        let mib = 1u64 << 20;
        let both = disk_short("sb", 100 * mib, 900 * mib, 300 * mib, true, Some(&(800 * mib, "cp:1".into())));
        assert!(both.contains("filesystem 300 MiB, largest checkpoint 800 MiB, image cp:1"), "{both}");
        assert!(both.contains("`docker rmi cp:1`"), "{both}");
        let none = disk_short("sb", 100 * mib, 900 * mib, 300 * mib, true, None);
        assert!(none.contains("filesystem 300 MiB, no checkpoint image)"), "{none}");
        let small = disk_short("sb", 100 * mib, 900 * mib, 300 * mib, true, Some(&(200 * mib, "cp:2".into())));
        assert!(small.contains("largest checkpoint 200 MiB, image cp:2, smaller than the filesystem"), "{small}");
        assert!(!small.contains("docker rmi"), "{small}");
        let unknown = disk_short("sb", 100 * mib, 900 * mib, 300 * mib, false, None);
        assert!(unknown.contains("filesystem 300 MiB, checkpoint size unknown"), "{unknown}");
    }

    #[test]
    fn the_disk_need_follows_the_sandboxs_own_checkpoint_before_the_hosts_largest() {
        let images = vec![
            (Some("old"), 900, "cp:old".to_string()),
            (Some("sb"), 100, "cp:a".to_string()),
            (Some("sb"), 200, "cp:b".to_string()),
        ];
        let live: HashSet<String> = ["sb", "old", "new"].map(String::from).into();
        assert_eq!(pick_checkpoint("sb", images.clone(), &live), Some((200, "cp:b".into())));
        assert_eq!(pick_checkpoint("new", images.clone(), &live), Some((900, "cp:old".into())), "falls back");
        let gone: HashSet<String> = ["sb", "new"].map(String::from).into();
        assert_eq!(pick_checkpoint("new", images, &gone), Some((200, "cp:b".into())), "skips removed sandboxes");
        assert_eq!(pick_checkpoint("sb", vec![], &live), None);
    }

    fn sb() -> HashSet<String> {
        HashSet::from(["sb".to_string()])
    }

    fn image(id: &str, tags: &[&str], size: i64, of: Option<&str>) -> ImageSummary {
        ImageSummary {
            id: id.into(),
            repo_tags: tags.iter().map(|t| t.to_string()).collect(),
            size,
            labels: of.map(|o| HashMap::from([(CHECKPOINT_OF.to_string(), o.to_string())])).unwrap_or_default(),
            ..Default::default()
        }
    }

    #[test]
    fn a_checkpoint_image_is_named_by_its_first_checkpoint_tag() {
        let tag = format!("{CHECKPOINT_REPO}:a");
        let images = [image("sha:1", &["other:1", &tag, &format!("{CHECKPOINT_REPO}:b")], 10, Some("sb"))];
        assert_eq!(checkpoint_of("sb", &images, &sb()), Some((10, tag)));
    }

    #[test]
    fn a_checkpoint_image_without_a_matching_tag_is_named_by_its_id() {
        let images = [image("sha:1", &["other:1"], 10, Some("sb")), image("sha:2", &[], 5, Some("sb"))];
        assert_eq!(checkpoint_of("sb", &images, &sb()), Some((10, "sha:1".into())));
    }

    #[test]
    fn a_negative_image_size_counts_as_zero() {
        let images = [image("sha:1", &[], -5, Some("sb"))];
        assert_eq!(checkpoint_of("sb", &images, &sb()), Some((0, "sha:1".into())));
    }

    #[test]
    fn no_images_means_no_checkpoint() {
        assert_eq!(checkpoint_of("sb", &[], &sb()), None);
    }

    #[test]
    fn the_disk_need_is_the_larger_of_the_container_and_the_checkpoint() {
        let cp = (800, "cp:1".to_string());
        assert_eq!(disk_size(300, Some(&cp)), 800);
        assert_eq!(disk_size(800, Some(&cp)), 800);
        assert_eq!(disk_size(900, Some(&cp)), 900);
        assert_eq!(disk_size(300, None), 300);
    }

    #[test]
    fn the_disk_a_checkpoint_needs_follows_the_filesystem() {
        let gib = 1u64 << 30;
        assert_eq!(disk_needed(10 * gib, 2, gib), 10 * gib, "the floor holds for a small filesystem");
        assert_eq!(disk_needed(10 * gib, 2, 14 * gib), 56 * gib, "twice over, for two sandboxes");
        assert_eq!(disk_needed(10 * gib, 1, 14 * gib), 28 * gib, "one sandbox needs half that");
        assert_eq!(disk_needed(10 * gib, 4, 14 * gib), 112 * gib, "and four twice that");
        assert_eq!(disk_needed(10 * gib, 2, u64::MAX), u64::MAX);
    }

    #[test]
    fn build_parallelism_follows_the_memory_limit() {
        let gib = 1u64 << 30;
        assert_eq!(build_jobs(2 * gib), 2);
        assert_eq!(build_jobs(3 * gib / 2), 1);
        assert_eq!(build_jobs(gib / 4), 1, "never zero");
    }

    #[test]
    fn a_full_disk_is_recognised() {
        assert!(full_disk("docker: write /var/lib/containerd/x: No space left on device"));
        assert!(!full_disk("docker: connection reset"));
    }

    #[test]
    fn free_space_is_read_from_df() {
        let df = "Filesystem 1024-blocks Used Available Capacity Mounted on\noverlay 60000000 50000000 1000 99% /\n";
        assert_eq!(free_bytes(df), Some(1000 * 1024));
        assert_eq!(free_bytes("garbage"), None);
    }

    #[test]
    fn log_archive_keeps_the_last_lines() {
        let raw = tar_of(&[("log", b"a\nb\nc\n", 0o644, 0, 0)], &[]).unwrap();
        assert_eq!(log_from_archive(&raw, 2).unwrap(), "b\nc\n");
        assert_eq!(log_from_archive(&raw, 10).unwrap(), "a\nb\nc\n");
        assert_eq!(log_from_archive(&raw, 0).unwrap(), "");
        let raw = tar_of(&[("log", b"a\nb\xff\xe2\x82", 0o644, 0, 0)], &[]).unwrap();
        assert_eq!(log_from_archive(&raw, 1).unwrap(), "b\u{fffd}\u{fffd}");
        assert!(matches!(log_from_archive(&tar_of(&[], &["d"]).unwrap(), 2), Err(Error::NotFound(_))));
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

    #[test]
    fn a_program_log_is_under_the_target_dir() {
        assert_eq!(program_log(&target().dir, "it's"), r"'/home/trailhead/.trailhead/programs/it'\''s/log'");
    }

    #[test]
    fn a_docker_target_uses_the_gateway_for_models_and_connectors() {
        let t = target();
        assert_eq!((t.dir.as_str(), t.workdir.as_str()), ("/home/trailhead/.trailhead", "/home/trailhead/work"));
        assert_eq!(t.gateway_list, "http://trailhead-gateway:8080/v1/gateway/list");
        assert_eq!(
            t.model_endpoint("https://m.example"),
            "http://trailhead-gateway:8080/v1/gateway/anthropic/anthropic"
        );
        assert_eq!(t.hold_socket, None);
    }
}
