//! `trailhead factory check`: reads everything a start would and says what is wrong, starting nothing.

use trailhead_client::paths::Paths;

use crate::computes::{compute_name, computes};
use crate::keys::saved_endpoint_id;
use crate::{Error, Opts, Result, RunOpts, fnox, login, ops, plan, read_host_config, resolve_opts};

pub(crate) fn check_result(failed: usize) -> Result<()> {
    match failed {
        0 => Ok(()),
        n => Err(Error::ChecksFailed(n)),
    }
}

/// Where each secret comes from (env, fnox, keyring or file), never its value. `opts` holds what the environment set.
pub(crate) fn print_secret_sources(opts: &Opts, fnox: &fnox::Fnox, paths: &Paths) {
    let creds = paths.creds();
    let writer = std::env::var("GITHUB_WRITER_TOKEN").ok();
    for (name, env, cred) in [
        ("SPRITES_TOKEN", opts.sprites_token.clone(), login::SPRITES),
        ("GITHUB_TOKEN", opts.github_token.clone(), login::GITHUB),
        ("CLAUDE_CODE_OAUTH_TOKEN", opts.claude_oauth_token.clone(), login::CLAUDE),
        ("GITHUB_WRITER_TOKEN", writer, login::GITHUB_WRITER),
    ] {
        println!("secret {}", login::describe(name, env, fnox, &creds, cred));
    }
}

/// Reports what the setup for `run`'s repos can see, without running anything. It asks the server's API for tasks
/// (only the server opens the database) and lists the computes itself; it binds no iroh endpoint, so it runs beside
/// the server.
pub async fn check(run: &RunOpts) -> Result<()> {
    let paths = Paths::from_env().ok_or(Error::NoDirs)?;
    let plan = plan(run, read_host_config(&paths)?)?;
    let (opts, fnox) = resolve_opts(&plan.opts, &paths, &plan.specs[0].dir)?;
    let tracker = ops::api_tracker(&opts, &paths)?;
    let mut failed = 0;
    print_secret_sources(&plan.opts, &fnox, &paths);
    for spec in &plan.specs {
        let slug = spec.slug();
        let listed = match ops::scopes(&tracker, &spec.config).await {
            Ok(scopes) => trailhead_factory::Tracker::ready(&tracker, &scopes, 10).await.map(|ready| (scopes, ready)),
            Err(e) => Err(e),
        };
        match listed {
            Ok((scopes, ready)) => {
                println!("{slug}: tracker: the server's API; {} ready task(s) in {}", ready.len(), describe(&scopes));
                for t in &ready {
                    println!("  {} {}", t.key.0, t.title);
                }
            }
            Err(e) => {
                failed += 1;
                println!("{slug}: tracker: {}", ops::reach(e));
            }
        }
    }
    let first = &plan.specs[0].config;
    let specs = first.target_specs();
    match computes(first, &opts, &fnox, &paths, Default::default(), true, true) {
        Ok(built) => {
            for ((compute, target), spec) in built.iter().zip(specs.iter().map(|s| compute_name(&s.compute))) {
                match compute.list(&Default::default()).await {
                    Ok(live) => println!(
                        "compute: {} ({spec}, priority {}) reachable; {} sandbox(es) visible",
                        target.name,
                        target.priority,
                        live.len()
                    ),
                    Err(e) => {
                        failed += 1;
                        println!("compute: {} ({spec}): {e}", target.name);
                    }
                }
            }
        }
        Err(e) => {
            failed += 1;
            println!("compute: {e}");
        }
    }
    match saved_endpoint_id(&paths.creds()) {
        Some(id) => println!("endpoint: {id}"),
        None => println!("endpoint: none yet (the server's control plane makes it on its first run)"),
    }
    println!("tunnel: the server's control plane forwards to {}", opts.server);
    check_result(failed)
}

/// `TRAIL, under TRAIL-102; TICK (graded, in apps/ticklist)`: the projects a repo works and how.
pub(crate) fn describe(scopes: &[trailhead_factory::Scope]) -> String {
    let each = scopes.iter().map(|s| match (s.pointed, s.queue.as_slice(), &s.path) {
        (false, [], _) => s.project.clone(),
        (false, queue, _) => {
            let keys: Vec<_> = queue.iter().map(|q| q.0.as_str()).collect();
            format!("{}, under {}", s.project, keys.join(", "))
        }
        (true, _, Some(path)) => format!("{} (graded, in {path})", s.project),
        (true, _, None) => format!("{} (graded)", s.project),
    });
    each.collect::<Vec<_>>().join("; ")
}
