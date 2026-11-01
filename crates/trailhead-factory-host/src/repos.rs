//! Which repos one factory process runs, and the checks that they can share it: `--repo` (repeatable), else the user
//! config's `[factory] repos`, else the repo found upward from the working directory.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use trailhead_factory::{FactoryConfig, HostConfig, Repo};

use crate::find;

/// A repo to run: where it lives, its `.trailhead.toml` and the `[factory]` settings read from it.
pub struct RepoSpec {
    pub dir: PathBuf,
    pub config_path: PathBuf,
    pub remote: Option<Repo>,
    pub config: FactoryConfig,
}

impl RepoSpec {
    /// `owner/name`, which every log line of the repo's loop carries.
    pub fn slug(&self) -> String {
        self.config.repo.repo().slug()
    }
}

/// The directories to run: `flags` if any, else the user config's, else the repo at or above `cwd` (or `config`'s).
pub fn dirs(
    flags: &[PathBuf],
    config: Option<&Path>,
    host: &HostConfig,
    home: Option<&Path>,
    cwd: &Path,
) -> Result<Vec<PathBuf>, String> {
    if !flags.is_empty() {
        return Ok(flags.to_vec());
    }
    if let Some(config) = config {
        return Ok(vec![parent(config)]);
    }
    if !host.repos.is_empty() {
        return Ok(host.repo_dirs(home));
    }
    Ok(vec![parent(&find::config(cwd)?)])
}

fn parent(file: &Path) -> PathBuf {
    file.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf()
}

/// Reads one repo. `dir` is the repo's directory, or (as `--config` gives it) the file itself.
pub fn load(dir: &Path, file: Option<&Path>) -> Result<RepoSpec, String> {
    let config_path = file.map_or_else(|| dir.join(".trailhead.toml"), Path::to_path_buf);
    let src = std::fs::read_to_string(&config_path).map_err(|e| format!("{}: {e}", config_path.display()))?;
    let remote = find::remote(dir);
    let config =
        FactoryConfig::from_repo_file(&src, remote.clone()).map_err(|e| format!("{}: {e}", config_path.display()))?;
    Ok(RepoSpec { dir: dir.to_path_buf(), config_path, remote, config })
}

/// Loads every repo; one that fails fails the lot, so the rest don't run silently without it.
pub fn load_all(dirs: &[PathBuf], file: Option<&Path>) -> Result<Vec<RepoSpec>, String> {
    let specs = dirs.iter().map(|d| load(d, file.filter(|_| dirs.len() == 1))).collect::<Result<Vec<_>, _>>()?;
    validate(&specs)?;
    Ok(specs)
}

/// Refuses repos that can't share a process: the same slug, the same lowercase name (pool Sprites are `<name>-w<n>`),
/// or the same (project, queue), which would have both claim the same tasks. Names both repos.
pub fn validate(specs: &[RepoSpec]) -> Result<(), String> {
    let mut slugs: HashMap<String, &RepoSpec> = HashMap::new();
    let mut names: HashMap<String, &RepoSpec> = HashMap::new();
    for (i, spec) in specs.iter().enumerate() {
        if let Some(first) =
            specs.first().filter(|f| f.config.compute != spec.config.compute || f.config.daemon != spec.config.daemon)
        {
            return Err(format!(
                "{} ({}) and {} ({}) differ in [factory.compute] or [factory.daemon]; repos in one process share the sandbox provider and daemon launch",
                first.dir.display(),
                first.slug(),
                spec.dir.display(),
                spec.slug(),
            ));
        }
        if let Some(first) =
            specs.first().filter(|f| f.config.plugins().map(|p| p.0) != spec.config.plugins().map(|p| p.0))
        {
            return Err(format!(
                "{} ({}) and {} ({}) use different profile adapters; repos in one process share the daemon launch, which builds one harness",
                first.dir.display(),
                first.slug(),
                spec.dir.display(),
                spec.slug(),
            ));
        }
        let repo = &spec.config.repo;
        let slug = spec.slug();
        let keys = [
            (slugs.insert(slug.clone(), spec), format!("the same repo, {slug}")),
            (
                names.insert(repo.name.to_lowercase(), spec),
                format!("the same repo name, {} (pool sandboxes are named from it)", repo.name.to_lowercase()),
            ),
        ];
        if let Some((other, what)) = keys.into_iter().find_map(|(prev, what)| prev.map(|p| (p, what))) {
            return Err(format!(
                "{} ({}) and {} ({}) have {what}; they would collide, so run only one of them",
                other.dir.display(),
                other.slug(),
                spec.dir.display(),
                spec.slug(),
            ));
        }
        // Lists that share any queue task claim the same tasks (the same list is the common case).
        if let Some((other, shared)) = specs[..i].iter().find_map(|o| {
            let other = &o.config.repo;
            if other.project != repo.project {
                return None;
            }
            let shared = repo.queue.iter().find(|q| other.queue.contains(q))?;
            Some((o, shared))
        }) {
            return Err(format!(
                "{} ({}) and {} ({}) have the same project and queue, {} under {shared}; they would collide, so run only one of them",
                other.dir.display(),
                other.slug(),
                spec.dir.display(),
                spec.slug(),
                repo.project,
            ));
        }
        // A repo with no queue takes every ready task in its project, so it overlaps any other repo there.
        if let Some(other) = specs[..i].iter().find(|o| {
            o.config.repo.project == repo.project && (o.config.repo.queue.is_empty() || repo.queue.is_empty())
        }) {
            return Err(format!(
                "{} ({}) and {} ({}) share project {} and one has no queue, so it would take the other's tasks; give each a queue",
                other.dir.display(),
                other.slug(),
                spec.dir.display(),
                spec.slug(),
                repo.project,
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(dir: &str, owner: &str, name: &str, project: &str, queue: &[&str]) -> RepoSpec {
        let queue = if queue.is_empty() { String::new() } else { format!("queue = {queue:?}\n") };
        let src = format!(
            "project = \"{project}\"\n[factory.repo]\nowner = \"{owner}\"\nname = \"{name}\"\n{queue}\n\
             [factory.checks]\nfmt = \"f\"\nlint = \"l\"\ntest = \"t\"\n\
             [factory.profiles.worker]\nrole = \"worker\"\nadapter = \"claude-code\"\nprovider = \"anthropic\"\nmodel = \"sonnet\"\nbudget = {{ usd = 5, minutes = 60, steps = 40 }}\n\
             [factory.profiles.reviewer]\nrole = \"reviewer\"\nadapter = \"claude-code\"\nprovider = \"anthropic\"\nmodel = \"opus\"\nbudget = {{ usd = 2, minutes = 20, steps = 20 }}\n"
        );
        let config = FactoryConfig::from_repo_file(&src, None).unwrap();
        RepoSpec { dir: dir.into(), config_path: format!("{dir}/.trailhead.toml").into(), remote: None, config }
    }

    #[test]
    fn distinct_repos_are_accepted() {
        let a = spec("/a", "o", "alpha", "ALPHA", &[]);
        let b = spec("/b", "o", "beta", "BETA", &[]);
        assert_eq!(validate(&[a, b]), Ok(()));
    }

    #[test]
    fn the_same_slug_is_refused_naming_both() {
        let a = spec("/a", "o", "app", "ALPHA", &[]);
        let b = spec("/b", "o", "app", "BETA", &[]);
        let err = validate(&[a, b]).unwrap_err();
        assert!(err.contains("/a") && err.contains("/b") && err.contains("the same repo, o/app"), "{err}");
    }

    #[test]
    fn the_same_lowercase_name_is_refused_naming_both() {
        let a = spec("/a", "o", "App", "ALPHA", &[]);
        let b = spec("/b", "p", "app", "BETA", &[]);
        let err = validate(&[a, b]).unwrap_err();
        assert!(err.contains("/a") && err.contains("/b") && err.contains("same repo name"), "{err}");
    }

    #[test]
    fn the_same_project_and_queue_is_refused_naming_both() {
        let a = spec("/a", "o", "alpha", "TRAIL", &["TRAIL-102"]);
        let b = spec("/b", "o", "beta", "TRAIL", &["TRAIL-102"]);
        let err = validate(&[a, b]).unwrap_err();
        assert!(err.contains("/a") && err.contains("/b") && err.contains("project and queue"), "{err}");
        let a = spec("/a", "o", "alpha", "TRAIL", &[]);
        let b = spec("/b", "o", "beta", "TRAIL", &[]);
        assert!(validate(&[a, b]).unwrap_err().contains("no queue"));
    }

    #[test]
    fn repos_with_different_compute_are_refused() {
        let a = spec("/a", "o", "alpha", "ALPHA", &[]);
        let mut b = spec("/b", "o", "beta", "BETA", &[]);
        b.config.compute =
            trailhead_factory::ComputeConfig::Docker { image: None, network: None, runtime: None, memory_mib: None };
        let err = validate(&[a, b]).unwrap_err();
        assert!(err.contains("/a") && err.contains("/b") && err.contains("[factory.compute]"), "{err}");
    }

    #[test]
    fn repos_with_different_adapters_are_refused() {
        let a = spec("/a", "o", "alpha", "ALPHA", &[]);
        let mut b = spec("/b", "o", "beta", "BETA", &[]);
        b.config.profiles.get_mut("worker").unwrap().adapter = "other-harness".into();
        let err = validate(&[a, b]).unwrap_err();
        assert!(err.contains("/a") && err.contains("/b") && err.contains("different profile adapters"), "{err}");
    }

    #[test]
    fn a_queueless_repo_overlaps_another_on_its_project() {
        let a = spec("/a", "o", "alpha", "TRAIL", &[]);
        let b = spec("/b", "o", "beta", "TRAIL", &["TRAIL-2"]);
        assert!(validate(&[a, b]).unwrap_err().contains("no queue"));
        let (a, b) = (spec("/a", "o", "alpha", "TRAIL", &[]), spec("/b", "o", "beta", "TRAIL", &["TRAIL-2"]));
        assert!(validate(&[b, a]).unwrap_err().contains("no queue"));
    }

    #[test]
    fn one_project_with_different_queues_is_fine() {
        let a = spec("/a", "o", "alpha", "TRAIL", &["TRAIL-1"]);
        let b = spec("/b", "o", "beta", "TRAIL", &["TRAIL-2"]);
        assert_eq!(validate(&[a, b]), Ok(()));
    }

    #[test]
    fn overlapping_queue_lists_are_refused_and_disjoint_ones_are_fine() {
        let a = spec("/a", "o", "alpha", "TRAIL", &["TRAIL-1", "TRAIL-2"]);
        let b = spec("/b", "o", "beta", "TRAIL", &["TRAIL-3", "TRAIL-2"]);
        let err = validate(&[a, b]).unwrap_err();
        assert!(err.contains("/a") && err.contains("/b") && err.contains("TRAIL-2"), "{err}");
        let a = spec("/a", "o", "alpha", "TRAIL", &["TRAIL-1", "TRAIL-2"]);
        let b = spec("/b", "o", "beta", "TRAIL", &["TRAIL-3"]);
        assert_eq!(validate(&[a, b]), Ok(()));
        let a = spec("/a", "o", "alpha", "ALPHA", &["TRAIL-1"]);
        let b = spec("/b", "o", "beta", "BETA", &["TRAIL-1"]);
        assert_eq!(validate(&[a, b]), Ok(()));
    }

    #[test]
    fn dirs_prefer_flags_then_the_user_config_then_discovery() {
        let host = HostConfig { repos: vec!["~/Dev/a".into()], ..Default::default() };
        let home = Path::new("/home/x");
        let flags = [PathBuf::from("/f")];
        assert_eq!(dirs(&flags, None, &host, Some(home), Path::new("/")).unwrap(), flags);
        assert_eq!(dirs(&[], None, &host, Some(home), Path::new("/")).unwrap(), [PathBuf::from("/home/x/Dev/a")]);

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".trailhead.toml"), "").unwrap();
        let nested = tmp.path().join("sub");
        std::fs::create_dir_all(&nested).unwrap();
        assert_eq!(dirs(&[], None, &HostConfig::default(), None, &nested).unwrap(), [tmp.path().to_path_buf()]);
    }

    #[test]
    fn a_repo_that_fails_to_parse_fails_the_load() {
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().join("good");
        let bad = tmp.path().join("bad");
        for d in [&good, &bad] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(bad.join(".trailhead.toml"), "project = \"X\"\n").unwrap();
        let err = load_all(&[bad.clone(), good], None).err().unwrap();
        assert!(err.contains(&bad.display().to_string()), "{err}");
    }
}
