use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde_json::json;

use super::{Ctx, print_json};
use crate::cli::ProjectCmd;
use crate::output::{project_line, s};

pub(crate) async fn project(cx: &Ctx, cmd: ProjectCmd) -> Result<()> {
    let c = &cx.c;
    match cmd {
        ProjectCmd::Ls { all } => {
            let v = c.get("projects", &[("archived", all.to_string())]).await?;
            if cx.json {
                print_json(&v);
            } else {
                v.as_array().into_iter().flatten().for_each(|p| println!("{}", project_line(p)));
            }
        }
        ProjectCmd::Add { key, name, brief, brief_file, review } => {
            let brief = read_brief(brief, brief_file)?.unwrap_or_default();
            let v = c
                .post("projects", json!({ "key": key, "name": name, "brief": brief, "require_review": review }))
                .await?;
            cx.show(&v, project_line);
        }
        ProjectCmd::Show { project } => {
            let v = c.get(&format!("projects/{project}"), &[]).await?;
            if cx.json {
                print_json(&v);
            } else {
                println!("{}", project_line(&v));
                if !s(&v, "brief").is_empty() {
                    println!("\n{}", s(&v, "brief"));
                }
            }
        }
        ProjectCmd::Edit { project, name, brief, brief_file, review, reviewers, archive, unarchive } => {
            let mut body = json!({});
            if let Some(r) = reviewers {
                body["reviewers"] = json!(r.into_iter().filter(|n| !n.trim().is_empty()).collect::<Vec<_>>());
            }
            if let Some(n) = name {
                body["name"] = json!(n);
            }
            if let Some(b) = read_brief(brief, brief_file)? {
                body["brief"] = json!(b);
            }
            if let Some(r) = review {
                body["require_review"] = json!(r);
            }
            match (archive, unarchive) {
                (true, true) => bail!("--archive or --unarchive, not both"),
                (true, false) => body["status"] = json!("archived"),
                (false, true) => body["status"] = json!("active"),
                _ => {}
            }
            cx.show(&c.patch(&format!("projects/{project}"), body).await?, project_line);
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
