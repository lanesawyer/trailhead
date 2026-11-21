use std::path::Path;

use anyhow::{Context, Result};
use trailhead_client::http::NewAttachment;
use trailhead_core::{AttachmentRole, TaskKey};

use super::Ctx;
use crate::client::api_err;
use crate::output::text;

/// Attaches a PNG to a task, named after the file unless `name` is given.
pub(crate) async fn attach(
    cx: &Ctx,
    task: &str,
    file: &Path,
    name: Option<String>,
    role: Option<AttachmentRole>,
    set: Option<String>,
) -> Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("reading {}", file.display()))?;
    let name = match name {
        Some(n) => n,
        None => file.file_stem().and_then(|s| s.to_str()).context("give the picture a --name")?.to_string(),
    };
    let new = NewAttachment { name, role, set };
    let a = cx.c.api().add_attachment(&TaskKey(task.into()), &new, bytes).await.map_err(api_err)?;
    cx.emit(&a, |a| text().attachment_line(a));
    Ok(())
}

/// The task's pictures, one line each.
pub(crate) async fn list(cx: &Ctx, task: &str) -> Result<()> {
    let list = cx.c.api().list_attachments(&TaskKey(task.into())).await.map_err(api_err)?;
    cx.emit(&list, |list| match list.as_slice() {
        [] => format!("{task} has no pictures"),
        list => list.iter().map(|a| text().attachment_line(a)).collect::<Vec<_>>().join("\n"),
    });
    Ok(())
}
