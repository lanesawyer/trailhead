use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use trailhead_client::http::{NewProject, ProjectUpdate};
use trailhead_core::ProjectStatus;

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
                if !p.brief.is_empty() {
                    println!("\n{}", p.brief);
                }
            }
        }
        ProjectCmd::Edit { project, name, brief, brief_file, review, reviewers, archive, unarchive } => {
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
                ..ProjectUpdate::default()
            };
            let p = api.update_project(&project, &update).await.map_err(api_err)?;
            cx.emit(&p, |p| text().project_line(p));
        }
    }
    Ok(())
}

fn read_brief(brief: Option<String>, file: Option<PathBuf>) -> Result<Option<String>> {
    match (brief, file) {
        (Some(_), Some(_)) => bail!("use --brief or --brief-file, not both"),
        (b, None) => Ok(b),
        (None, Some(f)) => Ok(Some(std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?)),
    }
}
