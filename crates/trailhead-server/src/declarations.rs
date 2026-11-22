//! Apps' declarations from an apps repo's checkout (`docs/app-platform.md`, "The app data store and connectors"):
//! each `apps/<id>/collections.toml`, with a fed collection's fields from the `[feeds.<feed>]` of
//! `connectors/<connector>/connector.toml` in the same checkout. The server registers them from the checkout its
//! builder last built, at start and after each build, so a shape goes live with the window that uses it.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use trailhead_core::app_data::{Declaration, Fields, check_app_id};
use trailhead_core::{Ctx, Error, Store};

/// What registering a checkout's declarations did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Registered {
    /// The apps registered, by id.
    pub registered: Vec<String>,
    /// The apps refused, by id (or by directory, for one that isn't an app's id), with why in plain words.
    pub refused: Vec<(String, String)>,
}

/// Reads every app's declaration in the checkout at `dir`, in id order: an app directory with no `collections.toml`
/// keeps no data and is left out. One broken file is that app's error; the others are read.
pub fn read(dir: &Path) -> Vec<(String, Result<Declaration, String>)> {
    let entries = match std::fs::read_dir(dir.join("apps")) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(e) => return vec![("apps".into(), Err(format!("reading apps/: {e}")))],
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .filter(|e| e.path().join("collections.toml").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    ids.into_iter()
        .map(|id| {
            let read = read_one(dir, &id).map_err(|e| format!("apps/{id}/collections.toml: {e}"));
            (id, read)
        })
        .collect()
}

fn read_one(dir: &Path, id: &str) -> Result<Declaration, String> {
    check_app_id(id).map_err(|e| e.to_string())?;
    let text =
        std::fs::read_to_string(dir.join("apps").join(id).join("collections.toml")).map_err(|e| e.to_string())?;
    let declaration =
        Declaration::parse(&text).map_err(|e| e.0.trim_start_matches("collections.toml: ").to_string())?;
    fill_feeds(declaration, |feed| feed_fields(dir, feed))
}

/// Gives each collection filled from a feed, and sent with no fields, the feed's fields from `shape`.
fn fill_feeds(
    mut declaration: Declaration,
    shape: impl Fn(&str) -> Result<Fields, String>,
) -> Result<Declaration, String> {
    for (name, c) in &mut declaration.collections {
        if let Some(feed) = &c.from
            && c.fields.is_empty()
        {
            c.fields = shape(feed).map_err(|e| format!("collection `{name}`: {e}"))?;
        }
    }
    Ok(declaration)
}

/// What a connector's `connector.toml` says of its feeds. The connector kit reads the whole file when it runs; the
/// server needs only the feeds' fields.
#[derive(Deserialize)]
struct Manifest {
    #[serde(default)]
    feeds: BTreeMap<String, Feed>,
}

#[derive(Deserialize)]
struct Feed {
    fields: Fields,
}

/// The fields of `feed` (`<connector>.<feed>`) as `connectors/<connector>/connector.toml` in `dir` declares them.
fn feed_fields(dir: &Path, feed: &str) -> Result<Fields, String> {
    let Some((connector, name)) = feed.split_once('.') else {
        return Err(format!("`{feed}` names a feed as <connector>.<feed>, like github.pull_requests"));
    };
    let file = format!("connectors/{connector}/connector.toml");
    let text = match std::fs::read_to_string(dir.join(&file)) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(format!("feed `{feed}` is from connector `{connector}`, and there is no {file}"));
        }
        Err(e) => return Err(format!("reading {file}: {e}")),
    };
    let mut manifest: Manifest = toml::from_str(&text).map_err(|e| format!("{file}: {}", e.message()))?;
    manifest.feeds.remove(name).map(|f| f.fields).ok_or_else(|| format!("{file} declares no feed `{name}`"))
}

/// Registers what [`read`] read, each app on its own, as `ctx`: one refused (a broken file, or a change the store
/// refuses to a collection that holds records) stops none of the others.
pub async fn register(store: &Store, ctx: &Ctx, read: Vec<(String, Result<Declaration, String>)>) -> Registered {
    let mut out = Registered::default();
    for (app, declaration) in read {
        let done = match declaration {
            Ok(d) => store.register_app(ctx, &app, d).await.map_err(|e| format!("apps/{app}/collections.toml: {e}")),
            Err(e) => Err(e),
        };
        match done {
            Ok(()) => out.registered.push(app),
            Err(e) => out.refused.push((app, e)),
        }
    }
    out
}

/// [`read`] and [`register`] the checkout at `dir`.
pub async fn register_checkout(store: &Store, ctx: &Ctx, dir: &Path) -> Registered {
    register(store, ctx, read(dir)).await
}

/// Registers the declarations in the checkout the builder last built, and logs what it registered and refused.
/// `None` while it holds no built commit (nothing built yet, a build in progress, or the last one failed): the
/// declarations registered from an earlier build stay.
pub async fn register_built(store: &Store, ctx: &Ctx, builds: &trailhead_builder::Builds) -> Option<Registered> {
    let read = builds.with_built_checkout(read).await?;
    let done = register(store, ctx, read).await;
    for app in &done.registered {
        tracing::info!(target: "shell", app, "registered the app's collections");
    }
    for (app, why) in &done.refused {
        tracing::warn!(target: "shell", app, error = %why, "refused the app's collections");
    }
    Some(done)
}

/// Fills in the fields of each collection in `declaration` filled from a feed that came without them, from the
/// connectors in the builder's last built checkout. Refused, saying so, when there is none to read.
pub(crate) async fn fill_from_build(
    declaration: Declaration,
    builds: Option<&trailhead_builder::Builds>,
) -> Result<Declaration, Error> {
    let unknown = declaration.collections.iter().find(|(_, c)| c.from.is_some() && c.fields.is_empty());
    let Some((name, c)) = unknown else { return Ok(declaration) };
    let none = || {
        let feed = c.from.as_deref().unwrap_or_default();
        Error::Invalid(format!(
            "collection `{name}` is filled from feed `{feed}`, and this server has no built apps repo to read the \
             feed's fields from: send the declaration as JSON with the collection's `fields`"
        ))
    };
    let Some(builds) = builds else { return Err(none()) };
    let filled = builds.with_built_checkout(|dir| fill_feeds(declaration.clone(), |f| feed_fields(dir, f))).await;
    filled.ok_or_else(none)?.map_err(Error::Invalid)
}
