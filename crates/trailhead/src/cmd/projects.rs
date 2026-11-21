use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::json;
use trailhead_client::http::{NewProject, ProjectUpdate};
use trailhead_core::{PROJECT_REPO_KEY, ProjectRepo, ProjectStatus};

use super::{Ctx, print_json, to_json};
use crate::cli::ProjectCmd;
use crate::client::api_err;
use crate::output::text;

pub(crate) async fn project(cx: &Ctx, cmd: ProjectCmd) -> Result<()> {
    let api = cx.c.api();
    match cmd {
        ProjectCmd::Ls { all } => {
            let projects = api.list_projects(all).await.map_err(api_err)?;
            if cx.json {
                print_json(&to_json(&projects));
            } else {
                projects.iter().for_each(|p| println!("{}", text().project_line(p)));
            }
        }
        ProjectCmd::Add { key, name, brief, brief_file, review } => {
            let brief = read_brief(brief, brief_file)?.unwrap_or_default();
            let new = NewProject { key, name, brief, require_review: review, claim_ttl: None };
            let project = api.create_project(&new).await.map_err(api_err)?;
            cx.emit(&project, |p| text().project_line(p));
        }
        ProjectCmd::Show { project } => {
            let p = api.show_project(&project).await.map_err(api_err)?;
            if cx.json {
                print_json(&to_json(&p));
            } else {
                println!("{}", text().project_line(&p));
                if let Some(to) = p.repo() {
                    println!(
                        "built by the factory of {}{}",
                        to.repo,
                        to.path.map(|p| format!(", in {p}/")).unwrap_or_default()
                    );
                }
                if !p.brief.is_empty() {
                    println!("\n{}", p.brief);
                }
            }
        }
        ProjectCmd::Edit {
            project,
            name,
            brief,
            brief_file,
            review,
            reviewers,
            archive,
            unarchive,
            factory_repo,
            factory_path,
            no_factory,
        } => {
            let brief = read_brief(brief, brief_file)?;
            let status = match (archive, unarchive) {
                (true, true) => bail!("--archive or --unarchive, not both"),
                (true, false) => Some(ProjectStatus::Archived),
                (false, true) => Some(ProjectStatus::Active),
                _ => None,
            };
            let update = ProjectUpdate {
                name,
                brief,
                require_review: review,
                status,
                reviewers: reviewers.map(|r| r.into_iter().filter(|n| !n.trim().is_empty()).collect()),
                metadata: factory_metadata(factory_repo, factory_path, no_factory)?,
                ..ProjectUpdate::default()
            };
            let p = api.update_project(&project, &update).await.map_err(api_err)?;
            cx.emit(&p, |p| text().project_line(p));
        }
    }
    Ok(())
}

/// The metadata patch for `--factory-repo`, `--factory-path` and `--no-factory`: the whole `factory` object, so a path
/// left out clears the one set before.
fn factory_metadata(repo: Option<String>, path: Option<String>, clear: bool) -> Result<Option<serde_json::Value>> {
    if clear {
        return Ok(Some(json!({ PROJECT_REPO_KEY: null })));
    }
    let Some(repo) = repo else { return Ok(None) };
    let to = json!({ PROJECT_REPO_KEY: { "repo": repo, "path": path } });
    let Some(read) = ProjectRepo::of(&to) else {
        bail!(
            "--factory-repo is owner/name and --factory-path a directory inside it (no `..`), got {repo:?} and {path:?}"
        )
    };
    Ok(Some(json!({ PROJECT_REPO_KEY: { "repo": read.repo, "path": read.path } })))
}

fn read_brief(brief: Option<String>, file: Option<PathBuf>) -> Result<Option<String>> {
    match (brief, file) {
        (Some(_), Some(_)) => bail!("use --brief or --brief-file, not both"),
        (b, None) => Ok(b),
        (None, Some(f)) => Ok(Some(std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?)),
    }
}
