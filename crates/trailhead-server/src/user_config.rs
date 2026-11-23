//! The per-machine user config, `<config dir>/config.toml`, and editing it from the window (`GET` and
//! `PATCH /v1/config`).
//!
//! The file decides whether the server starts at all, so an edit is made safely: it is applied to the file's own
//! document, which keeps its comments, its order and every key the edit doesn't name; the result is checked with the
//! parsers the CLI and the server start with (the factory's whole start-up check included, and the builder's), and
//! refused in their words if any of them would refuse it; and it is written atomically beside a backup of the file it
//! replaces. A change to the file made since the window read it refuses the edit. Nothing is applied to the running
//! server, which reads the file when it starts.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use toml_edit::{Array, DocumentMut, Item, TableLike, Value};
use trailhead_core::config::{
    ConfigChange, FactoryChange, FactorySettings, RunningSettings, ServerConfig, ShellChange, ShellSettings,
};

use crate::Paths;

/// The user config's keys: the CLI's `url`, and the server's `[factory]` and `[shell]`, which the CLI keeps unparsed
/// (it doesn't depend on the factory's crates). Unknown keys are refused, as typos.
#[derive(Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct UserConfig {
    /// The server to use when neither `--url` nor `TRAILHEAD_URL` is set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Host-wide factory settings, which `trailhead_factory::HostConfig` checks when the factory reads them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub factory: Option<toml::Table>,
    /// The shell's builder (`[shell] repo` and its build settings), which the server's builder reads
    /// (`trailhead_builder::ShellConfig`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<toml::Table>,
}

/// Where the user config is.
pub fn path(paths: &Paths) -> PathBuf {
    paths.config_dir.join("config.toml")
}

/// The backup of the file an edit replaced, beside it.
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

/// What the server knows of its user config: where it is, and what it said when the server started.
pub struct ConfigFile {
    path: PathBuf,
    started: Sections,
    /// Held across an edit's read, check and write, so two edits don't interleave.
    writing: tokio::sync::Mutex<()>,
}

/// Why an edit was refused.
#[derive(Debug)]
pub enum Refused {
    /// The file changed on disk since the version the edit names.
    Stale,
    /// The file the edit would leave is one the server or the CLI would refuse, in their words; or the edit itself
    /// is one this server can't make.
    Invalid(String),
    /// The file couldn't be read or written.
    Io(String),
}

/// The two sections, typed, as the parsers read them; `None` for one that doesn't load.
#[derive(Debug, Clone, Default, PartialEq)]
struct Sections {
    factory: Option<FactorySettings>,
    shell: Option<ShellSettings>,
}

impl ConfigFile {
    /// The user config at `path`, read now as what the server starts with.
    pub fn open(path: PathBuf) -> ConfigFile {
        let text = read(&path).unwrap_or_default();
        ConfigFile { started: sections(&text), path, writing: Default::default() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The settings in the file now, beside what the server started with.
    pub fn show(&self, foreground: bool) -> Result<ServerConfig, Refused> {
        let text = read(&self.path).map_err(|e| Refused::Io(format!("{}: {e}", self.path.display())))?;
        Ok(self.reply(&text, foreground))
    }

    /// Applies `change` to the file, checks the result and writes it, answering with the file as written.
    pub async fn change(&self, change: ConfigChange, foreground: bool) -> Result<ServerConfig, Refused> {
        let _writing = self.writing.lock().await;
        let path = self.path.clone();
        let text = tokio::task::spawn_blocking(move || edit(&path, &change))
            .await
            .map_err(|e| Refused::Io(e.to_string()))??;
        Ok(self.reply(&text, foreground))
    }

    fn reply(&self, text: &str, foreground: bool) -> ServerConfig {
        let now = sections(text);
        let waiting = waiting(&self.started, &now);
        let running = RunningSettings {
            factory: self.started.factory.clone().filter(|_| waiting.iter().any(|w| w.starts_with("factory"))),
            shell: self.started.shell.clone().filter(|_| waiting.iter().any(|w| w.starts_with("shell"))),
        };
        ServerConfig {
            path: self.path.display().to_string(),
            version: version(text),
            factory: now.factory,
            factory_built: cfg!(feature = "factory"),
            shell: now.shell,
            problem: check(&self.path, text).err().map(|e| without_excerpts(&e)),
            running,
            restart_needed: !waiting.is_empty(),
            waiting,
            foreground,
        }
    }
}

/// The file's text; an empty one for a file that isn't there.
fn read(path: &Path) -> std::io::Result<String> {
    match std::fs::read_to_string(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        other => other,
    }
}

/// The content's hash, which a change names to say which file it was made against.
pub fn version(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Reads the file, applies `change`, checks it and writes it, all or nothing. Returns the text written.
fn edit(path: &Path, change: &ConfigChange) -> Result<String, Refused> {
    let io = |e: std::io::Error| Refused::Io(format!("{}: {e}", path.display()));
    let text = read(path).map_err(io)?;
    if version(&text) != change.version {
        return Err(Refused::Stale);
    }
    let mut doc: DocumentMut =
        text.parse().map_err(|e| Refused::Invalid(format!("parsing {}: {e}", path.display())))?;
    apply(&mut doc, change)?;
    let edited = doc.to_string();
    check(path, &edited).map_err(|e| Refused::Invalid(without_excerpts(&e)))?;
    if edited != text {
        write_atomic(path, &edited).map_err(io)?;
    }
    Ok(edited)
}

/// Every check the file has to pass before anything is written: the CLI's own parse (every command reads `url`
/// from it, `trailhead serve` and `trailhead server start` included), the factory's start-up check, which refuses
/// the server when it fails, and the builder's, which turns the builder off. Each error is in the words its reader
/// prints.
pub fn check(path: &Path, text: &str) -> Result<(), String> {
    toml::from_str::<UserConfig>(text).map_err(|e| format!("parsing {}: {e}", path.display()))?;
    #[cfg(feature = "factory")]
    trailhead_factory_server::check_user_config(path, text).map_err(|e| e.to_string())?;
    let shell =
        trailhead_builder::ShellConfig::from_user_config(text).map_err(|e| format!("{}: {e}", path.display()))?;
    if let Some(shell) = shell {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        shell.checked_repo_dir(home.as_deref())?;
    }
    Ok(())
}

/// A parser's error without the lines of the file it quotes (`5 | token = "…"`), so no value from the file is sent
/// back that the error doesn't name itself: the line and column, and what is wrong, stay.
fn without_excerpts(error: &str) -> String {
    let quoted = |line: &str| {
        let line = line.trim_start();
        line.trim_start_matches(|c: char| c.is_ascii_digit()).trim_start().starts_with('|')
    };
    error.lines().filter(|l| !quoted(l)).collect::<Vec<_>>().join("\n")
}

/// The file's two sections, typed by the parsers the server starts with, with which keys are defaults.
fn sections(text: &str) -> Sections {
    let doc = toml::from_str::<toml::Table>(text).ok();
    let table = |name: &str| match doc.as_ref().map(|d| d.get(name)) {
        Some(Some(toml::Value::Table(t))) => Some(t.clone()),
        Some(None) => Some(toml::Table::new()),
        _ => None,
    };
    let unset = |table: &toml::Table, keys: &[&str]| -> Vec<String> {
        keys.iter().filter(|k| !table.contains_key(**k)).map(|k| k.to_string()).collect()
    };
    let shell = table("shell").and_then(|t| {
        let parsed = trailhead_builder::ShellConfig::from_table(t.clone()).ok()?;
        Some(ShellSettings {
            repo: parsed.repo,
            package: parsed.package,
            profile: parsed.profile,
            jobs: parsed.jobs,
            poll_secs: parsed.poll_secs,
            defaults: unset(&t, &["repo", "package", "profile", "jobs", "poll_secs"]),
        })
    });
    Sections { factory: factory_settings(table("factory"), unset), shell }
}

#[cfg(feature = "factory")]
fn factory_settings(
    table: Option<toml::Table>,
    unset: impl Fn(&toml::Table, &[&str]) -> Vec<String>,
) -> Option<FactorySettings> {
    let table = table?;
    let parsed = trailhead_factory_server::HostConfig::from_table(table.clone()).ok()?;
    Some(FactorySettings {
        repos: parsed.repos,
        disabled: parsed.disabled,
        pause_at: parsed.pause_at,
        max_sandboxes: parsed.max_sandboxes,
        defaults: unset(&table, &["repos", "disabled", "pause_at", "max_sandboxes"]),
    })
}

#[cfg(not(feature = "factory"))]
fn factory_settings(
    _: Option<toml::Table>,
    _: impl Fn(&toml::Table, &[&str]) -> Vec<String>,
) -> Option<FactorySettings> {
    None
}

/// The settings that differ between what the server started with and the file now, as `section.key`; a section
/// that loads on one side only is named whole.
fn waiting(started: &Sections, now: &Sections) -> Vec<String> {
    let mut names = Vec::new();
    match (&started.factory, &now.factory) {
        (Some(a), Some(b)) => {
            let keys = [
                ("repos", a.repos != b.repos),
                ("disabled", a.disabled != b.disabled),
                ("pause_at", a.pause_at != b.pause_at),
                ("max_sandboxes", a.max_sandboxes != b.max_sandboxes),
            ];
            names.extend(keys.into_iter().filter(|(_, differ)| *differ).map(|(k, _)| format!("factory.{k}")));
        }
        (None, None) => {}
        _ => names.push("factory".into()),
    }
    match (&started.shell, &now.shell) {
        (Some(a), Some(b)) => {
            let keys = [
                ("repo", a.repo != b.repo),
                ("package", a.package != b.package),
                ("profile", a.profile != b.profile),
                ("jobs", a.jobs != b.jobs),
                ("poll_secs", a.poll_secs != b.poll_secs),
            ];
            names.extend(keys.into_iter().filter(|(_, differ)| *differ).map(|(k, _)| format!("shell.{k}")));
        }
        (None, None) => {}
        _ => names.push("shell".into()),
    }
    names
}

/// Makes `change` in the document, touching only the keys it names.
fn apply(doc: &mut DocumentMut, change: &ConfigChange) -> Result<(), Refused> {
    if let Some(factory) = &change.factory {
        if !cfg!(feature = "factory") {
            return Err(Refused::Invalid(
                "this server was built without the factory, so it can't check a change to [factory]".into(),
            ));
        }
        apply_factory(section(doc, "factory")?, factory);
    }
    if let Some(shell) = &change.shell {
        apply_shell(section(doc, "shell")?, shell);
    }
    Ok(())
}

/// The table `name`, made at the file's end when there is none.
fn section<'a>(doc: &'a mut DocumentMut, name: &str) -> Result<&'a mut dyn TableLike, Refused> {
    doc.entry(name)
        .or_insert_with(toml_edit::table)
        .as_table_like_mut()
        .ok_or_else(|| Refused::Invalid(format!("`{name}` in the user config isn't a table; fix it by hand")))
}

fn apply_factory(table: &mut dyn TableLike, change: &FactoryChange) {
    if let Some(repos) = &change.repos {
        set_list(table, "repos", repos);
    }
    match change.disabled {
        Some(true) => set(table, "disabled", Value::from(true)),
        Some(false) => {
            table.remove("disabled");
        }
        None => {}
    }
    if let Some(pause_at) = &change.pause_at {
        set_pause_at(table, pause_at);
    }
}

/// `pause_at` set to `wanted`, edited in place in whichever form it is written: an inline table, laid out afresh when
/// it changes, or a `[factory.pause_at]` table, whose comments stay.
fn set_pause_at(table: &mut dyn TableLike, wanted: &std::collections::BTreeMap<String, u32>) {
    if wanted.is_empty() {
        table.remove("pause_at");
        return;
    }
    let Some(windows) = table.get_mut("pause_at").and_then(Item::as_table_like_mut) else {
        let windows: toml_edit::InlineTable =
            wanted.iter().map(|(k, v)| (k.as_str(), Value::from(i64::from(*v)))).collect();
        set(table, "pause_at", Value::InlineTable(windows));
        return;
    };
    let now: std::collections::BTreeMap<String, Option<i64>> =
        windows.iter().map(|(k, v)| (k.to_string(), v.as_integer())).collect();
    if now.len() == wanted.len() && wanted.iter().all(|(k, v)| now.get(k) == Some(&Some(i64::from(*v)))) {
        return;
    }
    for gone in now.keys().filter(|k| !wanted.contains_key(*k)) {
        windows.remove(gone);
    }
    for (window, percent) in wanted {
        set(windows, window, Value::from(i64::from(*percent)));
    }
    if let Some(inline) = table.get_mut("pause_at").and_then(Item::as_inline_table_mut) {
        inline.fmt();
    }
}

fn apply_shell(table: &mut dyn TableLike, change: &ShellChange) {
    let strings = [("repo", &change.repo), ("package", &change.package), ("profile", &change.profile)];
    for (key, value) in strings {
        set_or_remove(table, key, value.as_ref().map(|v| v.clone().map(Value::from)));
    }
    set_or_remove(table, "jobs", change.jobs.map(|v| v.map(|n| Value::from(i64::from(n)))));
    let poll = change.poll_secs.map(|v| v.map(|n| Value::from(i64::try_from(n).unwrap_or(i64::MAX))));
    set_or_remove(table, "poll_secs", poll);
}

/// Sets `key` (`Some(Some(_))`), takes it out (`Some(None)`) or leaves it (`None`).
fn set_or_remove(table: &mut dyn TableLike, key: &str, value: Option<Option<Value>>) {
    match value {
        Some(Some(value)) => set(table, key, value),
        Some(None) => {
            table.remove(key);
        }
        None => {}
    }
}

/// Sets `key` to `value`, keeping the old value's comments and spacing; an equal value is left as it was written.
fn set(table: &mut dyn TableLike, key: &str, mut value: Value) {
    match table.get_mut(key) {
        Some(item) => {
            if let Some(old) = item.as_value() {
                if same(old, &value) {
                    return;
                }
                *value.decor_mut() = old.decor().clone();
            }
            *item = Item::Value(value);
        }
        None => {
            table.insert(key, Item::Value(value));
        }
    }
}

/// `key` set to the list `values`, keeping what is written around the entries that stay: the comment on an entry's
/// own line goes with it, and a comment after an entry (`"~/a", # work`) with that entry, so removing an entry removes
/// its comment. A new entry is laid out as the last one was, with no comment.
fn set_list(table: &mut dyn TableLike, key: &str, values: &[String]) {
    let Some(old) = table.get(key).and_then(Item::as_array) else {
        set(table, key, Value::Array(values.iter().map(String::as_str).collect()));
        return;
    };
    let raw = |r: Option<&toml_edit::RawString>| r.and_then(|r| r.as_str()).unwrap_or("").to_string();
    let prefixes: Vec<String> = old.iter().map(|v| raw(v.decor().prefix())).collect();
    let suffixes: Vec<String> = old.iter().map(|v| raw(v.decor().suffix())).collect();
    let trailing = raw(Some(old.trailing()));
    // What follows an entry on its line is written as the next entry's prefix (or the list's trailing), or, with no
    // comma after the entry, as its own suffix: split each at its first newline, the part before it being the entry's.
    let ends_line = |i: usize| suffixes[i].contains('\n');
    let opening = split_line(prefixes.first().map_or("", String::as_str)).0;
    let closing = match suffixes.len().checked_sub(1).filter(|last| ends_line(*last)) {
        Some(last) => format!("{}{trailing}", split_line(&suffixes[last]).1),
        None => split_line(&trailing).1.to_string(),
    };
    let lead = |i: usize| split_line(&prefixes[i]).1.to_string();
    let after = |i: usize| match ends_line(i) {
        true => split_line(&suffixes[i]).0.to_string(),
        false => split_line(prefixes.get(i + 1).unwrap_or(&trailing)).0.to_string(),
    };
    let fresh_lead = prefixes.last().map_or(String::new(), |p| {
        let lead = split_line(p).1;
        lead.rfind('\n').map_or(lead, |at| &lead[at..]).to_string()
    });
    let mut list = Array::new();
    let mut tail = opening.to_string();
    for (j, wanted) in values.iter().enumerate() {
        let kept = old.iter().position(|v| v.as_str() == Some(wanted));
        let (mut entry, mut lead, next) = match kept {
            Some(i) => {
                let mut entry = old.get(i).cloned().expect("an entry it found");
                if ends_line(i) {
                    entry.decor_mut().set_suffix("");
                }
                (entry, lead(i), after(i))
            }
            None => (Value::from(wanted.as_str()), fresh_lead.clone(), String::new()),
        };
        if !lead.contains('\n') {
            lead = if j == 0 {
                lead.trim_start().to_string()
            } else if lead.is_empty() {
                " ".into()
            } else {
                lead
            };
        }
        entry.decor_mut().set_prefix(format!("{tail}{lead}"));
        tail = next;
        list.push_formatted(entry);
    }
    list.set_trailing(format!("{tail}{closing}"));
    list.set_trailing_comma(old.trailing_comma() && !values.is_empty());
    set(table, key, Value::Array(list));
}

/// `text` split at its first newline: what is before it, and the rest from the newline on (all of it when there is
/// none).
fn split_line(text: &str) -> (&str, &str) {
    text.find('\n').map_or(("", text), |at| text.split_at(at))
}

/// Whether two values are equal, whatever their formatting.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::String(a), Value::String(b)) => a.value() == b.value(),
        (Value::Integer(a), Value::Integer(b)) => a.value() == b.value(),
        (Value::Boolean(a), Value::Boolean(b)) => a.value() == b.value(),
        (Value::Array(a), Value::Array(b)) => a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| same(a, b)),
        (Value::InlineTable(a), Value::InlineTable(b)) => {
            a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).is_some_and(|w| same(v, w)))
        }
        _ => false,
    }
}

/// A value the CLI writes to the user config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Setting {
    Text(String),
    Flag(bool),
    /// Takes the key out.
    Unset,
}

/// Sets `key` in `[table]` (the top level when `None`) of the user config, as an edit of the file's own document:
/// its comments, order and every other key stay. The file is written atomically beside a backup, and is made if
/// it isn't there.
pub fn set_setting(paths: &Paths, table: Option<&str>, key: &str, setting: &Setting) -> Result<(), String> {
    let file = path(paths);
    let text = read(&file).map_err(|e| format!("reading {}: {e}", file.display()))?;
    let mut doc: DocumentMut = text.parse().map_err(|e| format!("parsing {}: {e}", file.display()))?;
    let target: &mut dyn TableLike = match table {
        Some(name) if matches!(setting, Setting::Unset) && doc.get(name).is_none() => return Ok(()),
        Some(name) => match section(&mut doc, name) {
            Ok(t) => t,
            Err(Refused::Invalid(e)) => return Err(e),
            Err(_) => return Err(format!("{name} can't be edited")),
        },
        None => doc.as_table_mut(),
    };
    match setting {
        Setting::Text(v) => set(target, key, Value::from(v.as_str())),
        Setting::Flag(v) => set(target, key, Value::from(*v)),
        Setting::Unset => {
            target.remove(key);
        }
    }
    write_atomic(&file, &doc.to_string()).map_err(|e| format!("writing {}: {e}", file.display()))
}

/// Replaces the file at `path` with `text` all at once: a temporary file in the same directory, written and synced,
/// renamed over it. The file it replaces is kept as [`backup_path`] first, the same way. Both keep the file's
/// permissions. A file that is a link is written where the link points, so the link stays.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let path = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let permissions = std::fs::metadata(&path).ok().map(|m| m.permissions());
    if permissions.is_some() {
        replace(&backup_path(&path), &std::fs::read(&path)?, permissions.clone())?;
    }
    replace(&path, text.as_bytes(), permissions)?;
    std::fs::File::open(dir)?.sync_all()
}

fn replace(path: &Path, bytes: &[u8], permissions: Option<std::fs::Permissions>) -> std::io::Result<()> {
    let mut name = std::ffi::OsString::from(".");
    name.push(path.file_name().unwrap_or_default());
    name.push(format!(".{}.tmp", std::process::id()));
    let temp = path.with_file_name(name);
    let written = (|| {
        let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&temp)?;
        file.write_all(bytes)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)?;
        }
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if written.is_err() {
        std::fs::remove_file(&temp).ok();
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(text: &str, edit: impl FnOnce(&mut ConfigChange)) -> ConfigChange {
        let mut change = ConfigChange { version: version(text), ..Default::default() };
        edit(&mut change);
        change
    }

    fn applied(text: &str, change: &ConfigChange) -> String {
        let mut doc: DocumentMut = text.parse().unwrap();
        apply(&mut doc, change).unwrap();
        doc.to_string()
    }

    #[test]
    fn an_edit_keeps_comments_order_and_unknown_keys() {
        let before = "\
# Trailhead on this machine.
url = \"http://127.0.0.1:7241\" # the usual port

[extra] # not ours
token = \"th_not_a_real_one\"

[shell]
# where the apps live
repo = \"~/Dev/apps\"   # my checkout
jobs = 2 # leave room for the factory
";
        let edit = change(before, |c| {
            c.shell = Some(ShellChange { jobs: Some(Some(4)), poll_secs: Some(Some(30)), ..Default::default() });
        });
        let after = applied(before, &edit);
        assert_eq!(
            after,
            "\
# Trailhead on this machine.
url = \"http://127.0.0.1:7241\" # the usual port

[extra] # not ours
token = \"th_not_a_real_one\"

[shell]
# where the apps live
repo = \"~/Dev/apps\"   # my checkout
jobs = 4 # leave room for the factory
poll_secs = 30
"
        );
        let same = change(before, |c| {
            c.shell = Some(ShellChange { repo: Some(Some("~/Dev/apps".into())), ..Default::default() });
        });
        assert_eq!(applied(before, &same), before, "setting what is there changes nothing");
        let out = change(before, |c| c.shell = Some(ShellChange { jobs: Some(None), ..Default::default() }));
        assert!(!applied(before, &out).contains("jobs"), "null takes a key out");
    }

    #[cfg(feature = "factory")]
    #[test]
    fn a_list_keeps_its_layout_and_a_table_is_made_when_missing() {
        let before = "[factory]\nrepos = [\n  \"~/a\", # first\n  \"~/b\",\n]\npause_at = { five_hour = 80 }\n";
        let edit = change(before, |c| {
            c.factory = Some(FactoryChange {
                repos: Some(vec!["~/b".into(), "~/c".into()]),
                pause_at: Some([("five_hour".to_string(), 70), ("seven_day".to_string(), 90)].into()),
                ..Default::default()
            });
        });
        assert_eq!(
            applied(before, &edit),
            "[factory]\nrepos = [\n  \"~/b\",\n  \"~/c\",\n]\npause_at = { five_hour = 70, seven_day = 90 }\n"
        );
        let kept = "repos = [\n  # work\n  \"~/a\", # mine\n  \"~/b\" # theirs\n] # done\n";
        let mut doc: DocumentMut = kept.parse().unwrap();
        set_list(doc.as_table_mut(), "repos", &["~/b".into(), "~/a".into(), "~/c".into()]);
        assert_eq!(
            doc.to_string(),
            "repos = [\n  \"~/b\", # theirs\n  # work\n  \"~/a\", # mine\n  \"~/c\"\n] # done\n"
        );
        let inline = "repos = [\"~/a\", \"~/b\"]\n";
        let mut doc: DocumentMut = inline.parse().unwrap();
        set_list(doc.as_table_mut(), "repos", &["~/b".into(), "~/c".into()]);
        assert_eq!(doc.to_string(), "repos = [\"~/b\", \"~/c\"]\n");
        let on = change("url = \"x\"\n", |c| {
            c.factory = Some(FactoryChange { disabled: Some(true), ..Default::default() });
        });
        assert_eq!(applied("url = \"x\"\n", &on), "url = \"x\"\n\n[factory]\ndisabled = true\n");
        let off = change(before, |c| c.factory = Some(FactoryChange { disabled: Some(false), ..Default::default() }));
        assert_eq!(applied(before, &off), before);
    }

    #[test]
    fn an_error_never_quotes_the_file() {
        let path = Path::new("/c/config.toml");
        let error = check(path, "url = \"x\"\ntoken = \"th_not_a_real_one\"\n").unwrap_err();
        assert!(error.contains("th_not_a_real_one"), "the parser quotes the line: {error}");
        let sent = without_excerpts(&error);
        assert!(!sent.contains("th_not_a_real_one") && sent.contains("unknown field `token`"), "{sent}");
        assert!(sent.starts_with("parsing /c/config.toml: TOML parse error at line 2"), "{sent}");
    }

    #[test]
    fn defaults_are_named_and_what_differs_from_the_start_waits() {
        let started = sections("[shell]\nrepo = \"/a\"\njobs = 3\n");
        let shell = started.shell.clone().unwrap();
        assert_eq!((shell.jobs, shell.profile.as_str()), (3, "release"));
        assert_eq!(shell.defaults, ["package", "profile", "poll_secs"]);
        let now = sections("[shell]\nrepo = \"/b\"\n");
        assert_eq!(waiting(&started, &now), ["shell.repo"], "jobs = 3 is the default, so it is the same");
        assert_eq!(waiting(&started, &sections("[shell]\njobs = 0\n")), ["shell"], "a section that doesn't load");
        assert_eq!(sections("").shell.unwrap().defaults.len(), 5);
    }
}
