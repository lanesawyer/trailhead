//! Runs and drives the sandboxes' gateway (`docs/factory/sandbox.md#the-gateway-credentials-and-egress-for-docker-sandboxes`):
//! one container, `trailhead-daemon gateway`, that is every sandbox's only way out.
//!
//! The gateway keeps everything in memory, and Docker can restart it behind our back. So the state files are the
//! truth: whenever the gateway's start time isn't the one last synced, the full state goes to it again. Every change
//! to a state file and its push to the gateway happen under one lock, so a resync can't put back a wider allowlist
//! that a concurrent `set_egress` just narrowed.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use bollard::exec::{StartExecOptions, StartExecResults};
use bollard::models::{
    ContainerCreateBody, ContainerInspectResponse, EndpointSettings, ExecConfig, HostConfig, NetworkConnectRequest,
    NetworkCreateRequest, NetworkDisconnectRequest, RestartPolicy, RestartPolicyNameEnum,
};
use bollard::query_parameters::{CreateContainerOptions, InspectContainerOptions, RemoveContainerOptions};
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::sync::MutexGuard;
use trailhead_factory::{Error, GATEWAY_HOST, GATEWAY_PORT, GatewayRequest, GatewayResponse, GatewayState, Result};

use crate::{DockerCompute, Link, MARKER, State, docker_err, status, tar_of};

/// The label on the gateway's container. It isn't [`MARKER`], so the gateway is never listed as a sandbox.
pub const GATEWAY_LABEL: &str = "dev.trailhead.gateway";
/// The label holding the SHA-256 of the daemon binary the gateway's container was made with, so a gateway running an
/// older binary is replaced rather than reused.
pub const GATEWAY_BIN_LABEL: &str = "dev.trailhead.gateway.daemon";
/// Where the daemon binary is in the gateway's container.
pub const GATEWAY_BIN: &str = "/opt/trailhead/trailhead-daemon";

/// How often the gateway is checked, so a running sandbox isn't left cut off until the next `Compute` call.
const WATCH_EVERY: Duration = Duration::from_secs(5);
/// How long a started gateway gets to open its control socket.
const READY_WITHIN: Duration = Duration::from_secs(10);

/// The hex SHA-256 of a daemon binary.
pub(crate) fn digest(bin: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bin).iter().map(|b| format!("{b:02x}")).collect()
}

/// The sandbox's own `--internal` network.
pub fn sandbox_network(name: &str) -> String {
    format!("trailhead-sb-{name}")
}

/// What sends a sandbox's traffic through the gateway. curl reads only the lowercase `http_proxy`, so both cases.
pub(crate) fn proxy_env() -> Vec<String> {
    let proxy = format!("http://{GATEWAY_HOST}:{GATEWAY_PORT}");
    let no_proxy = format!("{GATEWAY_HOST},localhost,127.0.0.1,::1");
    ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy"]
        .iter()
        .map(|k| format!("{k}={proxy}"))
        .chain(["NO_PROXY", "no_proxy"].iter().map(|k| format!("{k}={no_proxy}")))
        .collect()
}

impl DockerCompute {
    /// Takes the gateway lock, with the gateway up and holding the full state.
    pub(crate) async fn gateway(&self) -> Result<MutexGuard<'_, Link>> {
        self.watch();
        self.sync().await
    }

    async fn sync(&self) -> Result<MutexGuard<'_, Link>> {
        let mut link = self.link.lock().await;
        let started = self.gateway_up().await?;
        if link.synced.as_ref() != Some(&started) {
            link.synced = None;
            self.resync().await?;
            link.synced = Some(started);
        }
        Ok(link)
    }

    /// Starts checking the gateway in the background, once. It stops when the last clone of this `DockerCompute` goes.
    pub(crate) fn watch(&self) {
        if self.watching.swap(true, Ordering::SeqCst) {
            return;
        }
        let link = Arc::downgrade(&self.link);
        let (docker, config) = (self.docker.clone(), self.config.clone());
        let daemon_digest = self.daemon_digest.clone();
        tokio::spawn(async move {
            while let Some(link) = link.upgrade() {
                let compute = DockerCompute {
                    docker: docker.clone(),
                    config: config.clone(),
                    link,
                    daemon_digest: daemon_digest.clone(),
                    watching: Default::default(),
                };
                if let Err(e) = compute.sync().await {
                    tracing::warn!(error = %e, "the docker gateway is out of sync");
                }
                drop(compute);
                tokio::time::sleep(WATCH_EVERY).await;
            }
        });
    }

    /// Makes sure the gateway's container exists and is running, and returns when it started.
    async fn gateway_up(&self) -> Result<String> {
        let name = &self.config.gateway;
        let info = match self.docker.inspect_container(name, None::<InspectContainerOptions>).await {
            Ok(info) => Some(info),
            Err(e) if status(&e) == Some(404) => None,
            Err(e) => return Err(docker_err(e)),
        };
        // The container was made with another daemon binary: replace it. Its networks go with it, and the resync that
        // follows the new start puts the sandboxes' back.
        let info = match info {
            Some(i) if !self.has_current_daemon(&i) => {
                tracing::info!(gateway = %name, "the daemon binary changed: replacing the gateway's container");
                let opts = RemoveContainerOptions { force: true, ..Default::default() };
                match self.docker.remove_container(name, Some(opts)).await {
                    Ok(()) => None,
                    Err(e) if status(&e) == Some(404) => None,
                    Err(e) => return Err(docker_err(e)),
                }
            }
            other => other,
        };
        let running = info.as_ref().and_then(|i| i.state.as_ref()).and_then(|s| s.running) == Some(true);
        if running && let Some(started) = info.and_then(|i| i.state).and_then(|s| s.started_at) {
            return Ok(started);
        }
        if !self.exists(name).await? {
            self.create_gateway().await?;
        }
        if let Err(e) = self.docker.start_container(name, None).await {
            // Half made, by a control plane that died before the binary went in: the next check starts over.
            let opts = RemoveContainerOptions { force: true, ..Default::default() };
            let _ = self.docker.remove_container(name, Some(opts)).await;
            return Err(docker_err(e));
        }
        let info = self.docker.inspect_container(name, None::<InspectContainerOptions>).await.map_err(docker_err)?;
        info.state.and_then(|s| s.started_at).ok_or_else(|| Error::Other(format!("gateway {name} has no start time")))
    }

    fn has_current_daemon(&self, info: &ContainerInspectResponse) -> bool {
        let label = info.config.as_ref().and_then(|c| c.labels.as_ref()).and_then(|l| l.get(GATEWAY_BIN_LABEL));
        label.map(String::as_str) == Some(&*self.daemon_digest)
    }

    /// On the bridge network, so it's the one container with a route out. Root in the container, to own its control
    /// socket under `/run` whatever user the image has, but with no capabilities.
    async fn create_gateway(&self) -> Result<()> {
        self.ensure_outbound_network().await?;
        let name = &self.config.gateway;
        let body = ContainerCreateBody {
            image: Some(self.config.image.clone()),
            cmd: Some(vec![GATEWAY_BIN.into(), "gateway".into(), "--listen".into(), format!("0.0.0.0:{GATEWAY_PORT}")]),
            entrypoint: Some(vec![]),
            user: Some("0".into()),
            labels: Some(HashMap::from([
                (GATEWAY_LABEL.to_string(), "1".to_string()),
                (GATEWAY_BIN_LABEL.to_string(), self.daemon_digest.to_string()),
            ])),
            host_config: Some(HostConfig {
                init: Some(true),
                network_mode: Some(self.config.network.clone()),
                restart_policy: Some(RestartPolicy {
                    name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                    maximum_retry_count: None,
                }),
                cap_drop: Some(vec!["ALL".into()]),
                runtime: self.config.runtime.clone(),
                ..Default::default()
            }),
            ..Default::default()
        };
        let opts = CreateContainerOptions { name: Some(name.clone()), ..Default::default() };
        match self.docker.create_container(Some(opts), body).await {
            Ok(_) => {}
            // Another control plane made it first.
            Err(e) if status(&e) == Some(409) => return Ok(()),
            Err(e) => return Err(docker_err(e)),
        }
        let path = GATEWAY_BIN.trim_start_matches('/');
        let tar = tar_of(&[(path, self.config.daemon.as_slice(), 0o755, 0, 0)], &["opt/trailhead"])?;
        self.upload(name, "/", tar).await
    }

    async fn ensure_outbound_network(&self) -> Result<()> {
        let labels = HashMap::from([(MARKER.to_string(), "network".to_string())]);
        self.ensure_network(&self.config.network, false, labels).await
    }

    async fn ensure_network(&self, name: &str, internal: bool, labels: HashMap<String, String>) -> Result<()> {
        match self.docker.inspect_network(name, None).await {
            Ok(_) => return Ok(()),
            Err(e) if status(&e) == Some(404) => {}
            Err(e) => return Err(docker_err(e)),
        }
        let req = NetworkCreateRequest {
            name: name.into(),
            driver: Some("bridge".into()),
            internal: Some(internal),
            labels: Some(labels),
            ..Default::default()
        };
        match self.docker.create_network(req).await {
            Ok(_) => Ok(()),
            // Docker allows two networks of one name, but this one lost a race and is harmless to report as made.
            Err(e) if status(&e) == Some(409) => Ok(()),
            Err(e) => Err(docker_err(e)),
        }
    }

    /// Pushes everything to a gateway that may know nothing, and clears up what it shouldn't know: connections no
    /// longer configured, sandboxes with no state file, and sandboxes whose teardown stopped partway.
    async fn resync(&self) -> Result<()> {
        let known = self.gateway_state().await?;
        for conn in &self.config.connections {
            self.ctl_ok(&GatewayRequest::SetConnection(conn.clone())).await?;
        }
        for held in &known.connections {
            if !self.config.connections.iter().any(|c| c.provider == held.provider && c.id == held.id) {
                let req = GatewayRequest::RemoveConnection { provider: held.provider.clone(), id: held.id.clone() };
                self.ctl_ok(&req).await?;
            }
        }
        let names = self.state_names().await?;
        for sandbox in &known.sandboxes {
            if !names.contains(&sandbox.name) {
                self.ctl_ok(&GatewayRequest::Unregister { name: sandbox.name.clone() }).await?;
            }
        }
        for name in &names {
            if self.exists(name).await? {
                self.attach(name, &self.read_state(name).await?).await?;
            } else {
                self.ctl_ok(&GatewayRequest::Unregister { name: name.clone() }).await?;
                self.detach(name).await?;
                self.remove_state(name).await?;
            }
        }
        Ok(())
    }

    /// The gateway's state, waiting for a gateway that's just started to open its socket.
    async fn gateway_state(&self) -> Result<GatewayState> {
        let deadline = tokio::time::Instant::now() + READY_WITHIN;
        loop {
            match self.ctl(&GatewayRequest::State).await {
                Ok(GatewayResponse::State(state)) => return Ok(state),
                Ok(other) => return Err(Error::Other(format!("gateway: unexpected response {other:?}"))),
                Err(e) if tokio::time::Instant::now() >= deadline => return Err(e),
                Err(_) => tokio::time::sleep(Duration::from_millis(200)).await,
            }
        }
    }

    /// Gives a sandbox its network with the gateway on it, and tells the gateway about it. Safe to repeat.
    pub(crate) async fn attach(&self, name: &str, state: &State) -> Result<()> {
        let network = sandbox_network(name);
        let labels = HashMap::from([(MARKER.to_string(), name.to_string())]);
        self.ensure_network(&network, true, labels).await?;
        let address = self.gateway_address(&network).await?;
        let register = GatewayRequest::Register { name: name.into(), address, labels: state.labels.clone() };
        self.ctl_ok(&register).await?;
        self.ctl_ok(&GatewayRequest::SetEgress { name: name.into(), egress: state.egress.clone() }).await
    }

    /// The gateway's address on `network`, joining it first if need be. Requests that arrive there are that
    /// sandbox's. The alias is the name sandboxes reach it by, whatever its container is called.
    async fn gateway_address(&self, network: &str) -> Result<std::net::IpAddr> {
        let gateway = &self.config.gateway;
        if self.gateway_network(network).await?.is_none() {
            let settings = EndpointSettings { aliases: Some(vec![GATEWAY_HOST.into()]), ..Default::default() };
            let req = NetworkConnectRequest { container: gateway.clone(), endpoint_config: Some(settings) };
            self.docker.connect_network(network, req).await.map_err(docker_err)?;
        }
        let ip = self.gateway_network(network).await?.and_then(|n| n.ip_address).unwrap_or_default();
        ip.parse().map_err(|_| Error::Other(format!("gateway {gateway} has no address on {network}: {ip:?}")))
    }

    async fn gateway_network(&self, network: &str) -> Result<Option<EndpointSettings>> {
        let info = self
            .docker
            .inspect_container(&self.config.gateway, None::<InspectContainerOptions>)
            .await
            .map_err(docker_err)?;
        Ok(info.network_settings.and_then(|n| n.networks).and_then(|mut nets| nets.remove(network)))
    }

    /// Takes the gateway off a sandbox's network and removes the network. Safe to repeat.
    pub(crate) async fn detach(&self, name: &str) -> Result<()> {
        let network = sandbox_network(name);
        if self.gateway_network(&network).await?.is_some() {
            let req = NetworkDisconnectRequest { container: self.config.gateway.clone(), force: Some(true) };
            self.docker.disconnect_network(&network, req).await.map_err(docker_err)?;
        }
        match self.docker.remove_network(&network).await {
            Err(e) if status(&e) != Some(404) => Err(docker_err(e)),
            _ => Ok(()),
        }
    }

    pub(crate) async fn ctl_ok(&self, req: &GatewayRequest) -> Result<()> {
        self.ctl(req).await.map(drop)
    }

    /// Sends one control message through `gateway-ctl`. It goes on stdin, which is what keeps credentials out of the
    /// exec's command line, where `docker inspect` and the engine's logs could see them.
    async fn ctl(&self, req: &GatewayRequest) -> Result<GatewayResponse> {
        let gateway = &self.config.gateway;
        let config = ExecConfig {
            cmd: Some(vec![GATEWAY_BIN.into(), "gateway-ctl".into()]),
            attach_stdin: Some(true),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            ..Default::default()
        };
        let exec = self.docker.create_exec(gateway, config).await.map_err(docker_err)?;
        let started = self.docker.start_exec(&exec.id, None::<StartExecOptions>).await.map_err(docker_err)?;
        let StartExecResults::Attached { mut output, mut input } = started else {
            return Err(Error::Other("gateway-ctl: the exec detached".into()));
        };
        let line = serde_json::to_vec(req).expect("gateway requests serialize");
        let sent = async {
            input.write_all(&line).await?;
            input.shutdown().await
        };
        sent.await.map_err(|e| Error::Other(format!("gateway-ctl: {e}")))?;
        let mut out = String::new();
        while let Some(chunk) = output.next().await {
            out.push_str(&chunk.map_err(docker_err)?.to_string());
        }
        match out.lines().find_map(|l| serde_json::from_str::<GatewayResponse>(l).ok()) {
            Some(GatewayResponse::Error { message }) => Err(Error::Other(format!("gateway: {message}"))),
            Some(resp) => Ok(resp),
            None => Err(Error::Other(format!("gateway-ctl: {}", out.trim()))),
        }
    }

    /// The names of the sandboxes with state files.
    async fn state_names(&self) -> Result<BTreeSet<String>> {
        let dir = &self.config.state_dir;
        let mut entries = match tokio::fs::read_dir(dir).await {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeSet::new()),
            Err(e) => return Err(crate::io_err(dir, e)),
        };
        let mut names = BTreeSet::new();
        while let Some(entry) = entries.next_entry().await.map_err(|e| crate::io_err(dir, e))? {
            let file = entry.file_name();
            if let Some(name) = file.to_string_lossy().strip_suffix(".json") {
                names.insert(name.to_string());
            }
        }
        Ok(names)
    }
}
