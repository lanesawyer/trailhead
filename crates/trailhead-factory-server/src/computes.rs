//! The compute backends a host's targets name, built with their daemons' launch settings.

use std::sync::Arc;

use trailhead_client::paths::Paths;
use trailhead_factory::{Compute, ComputeConfig, Daemon, FactoryConfig, Target};

use crate::{Error, Opts, Result, fnox};

/// A target's `Compute` and launch facts.
pub(crate) type Built = (Arc<dyn Compute>, Target);

/// A target with its `Compute` and the daemon launched in its sandboxes.
pub(crate) type Wired = (Target, Arc<dyn Compute>, Arc<dyn Daemon>);

/// A factory file's targets, each with its `Compute` and launch facts, in placement order. `read_only` is as for
/// [`compute`]. `has_model_credential` is whether the harness secrets the sandboxes get are non-empty.
pub(crate) fn computes(
    config: &FactoryConfig,
    opts: &Opts,
    fnox: &fnox::Fnox,
    paths: &Paths,
    binary: Arc<Vec<u8>>,
    read_only: bool,
    has_model_credential: bool,
) -> Result<Vec<Built>> {
    config
        .target_specs()
        .into_iter()
        .map(|spec| {
            let (compute, target) =
                compute(&spec.compute, opts, fnox, paths, binary.clone(), read_only, has_model_credential)
                    .map_err(|e| Error::context(format!("target {}", spec.name), e))?;
            Ok((compute, target.listed(&spec)))
        })
        .collect()
}

/// The sandboxes' `Compute`, and where the daemon lives in them.
#[cfg_attr(not(feature = "docker"), expect(unused_variables))]
/// `read_only` is for `status` and `logs`, which list sandboxes beside a running factory: a Docker compute made for
/// it never starts, replaces or syncs the gateway.
pub(crate) fn compute(
    config: &ComputeConfig,
    opts: &Opts,
    fnox: &fnox::Fnox,
    paths: &Paths,
    binary: Arc<Vec<u8>>,
    read_only: bool,
    has_model_credential: bool,
) -> Result<(Arc<dyn Compute>, Target)> {
    match config {
        ComputeConfig::Sprites => {
            let token = opts.sprites_token.clone().ok_or(Error::NoSpritesToken)?;
            trailhead_compute_sprites::build(token).map_err(Error::backend)
        }
        #[cfg(feature = "docker")]
        ComputeConfig::Docker { .. } => {
            use trailhead_compute_docker::DockerConfig;
            let mut docker = DockerConfig::new(paths.data_dir.join("factory").join("docker"), binary);
            docker.connections = gateway_connections(crate::login::github_writer_token(fnox, &paths.creds())?);
            if docker.connections.is_empty() {
                tracing::warn!(
                    "no GitHub writer token (GITHUB_WRITER_TOKEN or `trailhead factory login github-writer`): Docker sandboxes can't publish"
                );
            }
            if !read_only && !has_model_credential {
                tracing::warn!(
                    "no harness credential (no Claude subscription token, or the harness cannot use it): Docker sandboxes have no model access until the gateway holds a credential for it"
                );
            }
            trailhead_compute_docker::build(config, docker, read_only).map_err(Error::backend)
        }
        #[cfg(not(feature = "docker"))]
        ComputeConfig::Docker { .. } => Err(Error::DockerNotBuilt),
    }
}

/// What the Docker gateway may forward to: GitHub's REST API for publishing, and `github.com` itself for cloning
/// and fetching private repos, both with the writer token. Subscription mode reaches the model API with the
/// sandbox's own token, through the gateway's proxy (TRAIL-487).
#[cfg(feature = "docker")]
pub(crate) fn gateway_connections(github_writer: Option<String>) -> Vec<trailhead_factory::GatewayConnection> {
    use trailhead_factory::{AuthScheme, GATEWAY_GIT_ID, GATEWAY_GITHUB_ID, GatewayConnection, Secret};
    let Some(token) = github_writer else {
        return Vec::new();
    };
    vec![
        GatewayConnection {
            provider: "github".into(),
            id: GATEWAY_GITHUB_ID.into(),
            upstream: trailhead_codehost_github::API_URL.into(),
            auth: AuthScheme::Bearer,
            credential: Secret::new(token.clone()),
        },
        GatewayConnection {
            provider: "git".into(),
            id: GATEWAY_GIT_ID.into(),
            upstream: "https://github.com".into(),
            auth: AuthScheme::GitBasic,
            credential: Secret::new(token),
        },
    ]
}

pub(crate) fn compute_name(config: &ComputeConfig) -> &'static str {
    match config {
        ComputeConfig::Sprites => "Sprites",
        ComputeConfig::Docker { .. } => "Docker",
    }
}
