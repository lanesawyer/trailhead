//! `trailhead app` on a machine with no apps repo of its own (`[shell] from` in the user config, a laptop): the
//! window is built here, from the source of each build the server at that endpoint made, fetched over the same iroh
//! protocol the window dials (`docs/app-platform.md`, "The window on another machine"). The builder is the server's
//! own ([`trailhead_builder::Builder`]) with its git replaced by the server: the head is the server's newest build,
//! and a checkout is that build's source unpacked. Nothing here needs a Trailhead server on this machine, or access
//! to the apps repo.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointId, SecretKey};
use std::ffi::OsString;

use trailhead_builder::{Boxed, Builder, Job, Layout, Origin, ShellConfig, Step, System, Tools};
use trailhead_client::Health;
use trailhead_client::creds::{Cred, CredStore};
use trailhead_core::shell::ShellBuild;
use trailhead_server::Paths;
use trailhead_wire::ui::{UiTunnel, dial_ui, open_ui_tunnel, refused};
use trailhead_wire::{parse_endpoint_id, parse_secret_key};

use super::app::{launch, run, short};
use super::app_lock::shell_dir;
use super::app_notice::Notice;

/// The environment variable a pasted user token is read from, as the window reads it.
const TOKEN_ENV: &str = "TRAILHEAD_SHELL_TOKEN";

/// How long one request to the server may take; a build's source is a few megabytes over a relay at worst.
const REQUEST: Duration = Duration::from_secs(120);

/// How often the server is tried again while it can't be reached or hasn't allowed this machine.
const RETRY: Duration = Duration::from_secs(5);

/// The server, reached: its API on a loopback port, and the user's token for it. Dropping it closes the port.
pub(crate) struct Link {
    /// `None` only for a link a test makes straight to a port.
    _tunnel: Option<UiTunnel>,
    http: reqwest::Client,
    base: String,
    token: String,
}

/// The key in `path`, made and saved when there is none: an endpoint's identity is what a person allows on the
/// server, so it has to outlast a restart.
fn load_or_create_key(path: &Path) -> Result<SecretKey> {
    if let Ok(text) = std::fs::read_to_string(path) {
        return parse_secret_key(text.trim()).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()));
    }
    let key = SecretKey::generate();
    let hex: String = key.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    trailhead_client::creds::write_secret(path, &hex).with_context(|| format!("writing {}", path.display()))?;
    Ok(key)
}

/// What to run on the server's machine so this one may connect, and what each line lets in: two programs here
/// connect at once, each as itself.
fn allow_lines(launcher: &SecretKey, window: &SecretKey) -> String {
    format!(
        "  # this launcher, which fetches and builds each new version of the window\n  trailhead factory allow {}\n\n  # the window itself, which shows your tasks and the factory\n  trailhead factory allow {}",
        launcher.public(),
        window.public()
    )
}

/// Why the server hasn't answered the launcher yet.
#[derive(Debug)]
enum Trouble {
    /// It answered, to say this endpoint isn't one it allows.
    Refused,
    /// It didn't answer, or its API didn't. Allowing this machine again changes nothing.
    Unreachable(String),
}

impl Trouble {
    /// What `trailhead app` prints: `allow` is the lines to run on the server, said only when it refused.
    fn in_a_terminal(&self, from: &str, allow: &str) -> String {
        let retry = RETRY.as_secs();
        match self {
            Trouble::Refused => format!(
                "trailhead app: the server at {from} doesn't allow this machine yet. Run both of these on the server's machine (two, because two programs here connect to it):\n\n{allow}\n\nTrying again every {retry} s, so this carries on by itself once they are run; Ctrl-C to stop."
            ),
            Trouble::Unreachable(why) => format!(
                "trailhead app: can't reach the server at {from} ({why}). Is it running, and is this machine online? Nothing needs allowing for this: a server that refuses this machine says so.\nTrying again every {retry} s, so this carries on by itself once it answers; Ctrl-C to stop."
            ),
        }
    }

    /// The same for Trailhead.app's dialog, which has the log to point at for the lines to run.
    fn in_a_dialog(&self) -> String {
        match self {
            Trouble::Refused => "Your Trailhead server doesn't allow this Mac yet. The two commands to run on the server's machine are in the log.\n\nTrailhead keeps trying, and opens by itself once they are run.".into(),
            Trouble::Unreachable(why) => format!(
                "Trailhead can't reach its server ({why}). Is the server running, and is this Mac online?\n\nTrailhead keeps trying, and opens by itself when the server answers."
            ),
        }
    }
}

/// Dials the server's endpoint itself, which is what tells a refusal from silence: `None` when it took the dial and
/// kept it, so its API is worth asking.
async fn dial(endpoint: &Endpoint, server: EndpointId) -> Option<Trouble> {
    match dial_ui(endpoint, server).await {
        Err(e) => Some(Trouble::Unreachable(e.to_string())),
        Ok(conn) if refused(&conn).await => Some(Trouble::Refused),
        Ok(conn) => {
            conn.close(0u32.into(), b"only asking");
            None
        }
    }
}

impl Link {
    /// Dials the server at `from`, waits until it answers (saying why it doesn't while it doesn't), and signs in
    /// with the token saved for it, `TRAILHEAD_SHELL_TOKEN`, or one pasted at the prompt.
    pub(crate) async fn open(paths: &Paths, from: &str) -> Result<Link> {
        let server = parse_endpoint_id(from).map_err(|e| anyhow::anyhow!("[shell] from: {e}"))?;
        let dir = shell_dir(paths);
        let launcher = load_or_create_key(&dir.join("launcher.key"))?;
        // The window's key too, so both can be allowed in one go before the window has ever run.
        let window = load_or_create_key(&dir.join("endpoint.key"))?;
        let endpoint = Endpoint::builder(presets::N0)
            .secret_key(launcher.clone())
            .bind()
            .await
            .map_err(|e| anyhow::anyhow!("opening an iroh endpoint: {e}"))?;
        let tunnel = open_ui_tunnel(endpoint.clone(), server).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        let http = reqwest::Client::builder().timeout(REQUEST).no_proxy().build()?;
        let base = format!("http://{}", tunnel.addr());
        let mut notice = Notice::from_env();
        let mut said = None;
        let health: Health = loop {
            let trouble = match dial(&endpoint, server).await {
                Some(trouble) => trouble,
                None => match http.get(format!("{base}/v1/health")).send().await {
                    Ok(r) if r.status().is_success() => break r.json().await.unwrap_or_default(),
                    Ok(r) => Trouble::Unreachable(format!("it answered {}", r.status())),
                    Err(e) => Trouble::Unreachable(e.to_string()),
                },
            };
            // Said once per kind of trouble: a server that comes back and then refuses this machine is news.
            if said != Some(std::mem::discriminant(&trouble)) {
                eprintln!("{}", trouble.in_a_terminal(from, &allow_lines(&launcher, &window)));
                if let Some(notice) = &mut notice {
                    notice.waiting(&trouble.in_a_dialog());
                }
                said = Some(std::mem::discriminant(&trouble));
            }
            if notice.as_mut().is_some_and(Notice::quit) {
                eprintln!("trailhead app: asked to quit while waiting for the server");
                std::process::exit(0);
            }
            tokio::time::sleep(RETRY).await;
        };
        drop(notice);
        let token = sign_in(paths, &http, &base, health.instance.as_deref()).await?;
        Ok(Link { _tunnel: Some(tunnel), http, base, token })
    }

    async fn get(&self, path: &str) -> Result<reqwest::Response, String> {
        let reply = self
            .http
            .get(format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| format!("asking the server: {e}"))?;
        if reply.status().is_success() {
            Ok(reply)
        } else {
            let status = reply.status();
            Err(format!("the server answered {status}: {}", reply.text().await.unwrap_or_default().trim()))
        }
    }
}

/// Where the user token for the server with this instance id is kept, as the window keeps it: at the top of the
/// config directory for this machine's own server, else in a directory named after the instance.
fn creds(paths: &Paths, instance: Option<&str>) -> CredStore {
    let base = CredStore::new(paths.config_dir.clone());
    let ours = std::fs::read_to_string(paths.data_dir.join("instance")).ok();
    match instance {
        Some(id) if ours.as_deref().map(str::trim) != Some(id) => base.for_server(id).unwrap_or(base),
        _ => base,
    }
}

/// Whether `token` is a person's token the server takes.
async fn accepted(http: &reqwest::Client, base: &str, token: &str) -> Result<bool> {
    let me = http.get(format!("{base}/v1/me")).bearer_auth(token).send().await?;
    if me.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(false);
    }
    let me: serde_json::Value = me.error_for_status()?.json().await?;
    if !me["agent_id"].is_null() {
        bail!("that is an agent's token; the window acts only as you, so it needs a user token");
    }
    Ok(true)
}

/// The user's token for the server: the one saved, else `TRAILHEAD_SHELL_TOKEN`, else one pasted at the prompt,
/// which is saved where the window looks so it is asked for once.
async fn sign_in(paths: &Paths, http: &reqwest::Client, base: &str, instance: Option<&str>) -> Result<String> {
    let store = creds(paths, instance);
    if let Some((saved, _)) = store.read(Cred::User)
        && accepted(http, base, &saved).await?
    {
        return Ok(saved);
    }
    let given = std::env::var(TOKEN_ENV).ok().map(|t| t.trim().to_string()).filter(|t| !t.is_empty());
    let token = match given {
        Some(token) => token,
        None if Notice::from_env().is_some() => bail!(
            "this Mac has no token for the server yet, and the app has nowhere to paste one. Run `trailhead app` once in a terminal, which asks for it and saves it"
        ),
        None => {
            eprintln!(
                "trailhead app: this machine has no token for the server. On the server's machine, run\n\n  trailhead --as-me token add laptop\n\nand paste what it prints here:"
            );
            let mut line = String::new();
            std::io::stdin().read_line(&mut line).context("reading the token")?;
            line.trim().to_string()
        }
    };
    if token.is_empty() || !accepted(http, base, &token).await? {
        bail!("the server didn't take that token");
    }
    if let Err(e) = store.save(Cred::User, &token) {
        eprintln!("trailhead app: couldn't save the token ({e}); it will be asked for again next time");
    }
    Ok(token)
}

/// The builder's tools on a machine that builds another server's builds: that server for what to build and its
/// source, this machine's cargo for the build.
pub(crate) struct FromServer {
    link: Link,
    from: String,
    cargo: System,
}

impl FromServer {
    pub(crate) fn new(link: Link, from: &str) -> FromServer {
        FromServer { link, from: from.to_string(), cargo: System::new() }
    }
}

impl Tools for FromServer {
    fn origin<'a>(&'a self, _repo: &'a Path) -> Boxed<'a, Result<Origin, String>> {
        Box::pin(async move { Ok(Origin { url: format!("trailhead:{}", self.from), branch: String::new() }) })
    }

    /// The server's newest build: what it built is what this machine builds, never a commit it hasn't.
    fn head<'a>(&'a self, _origin: &'a Origin) -> Boxed<'a, Result<String, String>> {
        Box::pin(async move {
            let reply = self.link.get("/v1/shell/build").await?;
            let build: ShellBuild = reply.json().await.map_err(|e| format!("reading the server's builds: {e}"))?;
            if !build.configured {
                return Err("the server builds no shell (its user config has no `[shell] repo`)".into());
            }
            build.current.ok_or_else(|| "the server hasn't built a shell yet".into())
        })
    }

    /// Replaces `src` with the source of `rev` as the server hands it out.
    fn checkout<'a>(&'a self, src: &'a Path, _origin: &'a Origin, rev: &'a str) -> Boxed<'a, Result<(), String>> {
        Box::pin(async move {
            let reply = self.link.get(&format!("/v1/shell/source/{rev}")).await?;
            let tar = reply.bytes().await.map_err(|e| format!("fetching the source of {rev}: {e}"))?;
            unpack(&tar, src).await
        })
    }

    fn build<'a>(&'a self, job: &'a Job) -> Boxed<'a, Result<PathBuf, String>> {
        self.cargo.build(job)
    }

    fn archive<'a>(&'a self, _src: &'a Path, _rev: &'a str) -> Boxed<'a, Result<Vec<u8>, String>> {
        Box::pin(async { Err("a machine that builds another server's builds hands out no source".into()) })
    }

    /// The source arrives without its history; the window asks the server what a newer build brings.
    fn log<'a>(&'a self, _src: &'a Path, _since: &'a str, _until: &'a str) -> Boxed<'a, Result<Vec<String>, String>> {
        Box::pin(async { Ok(Vec::new()) })
    }
}

/// Empties `src` and unpacks the gzipped tar into it, with the system's `tar`.
pub(crate) async fn unpack(tar: &[u8], src: &Path) -> Result<(), String> {
    let at = |what: &str, e: std::io::Error| format!("{what} {}: {e}", src.display());
    match std::fs::remove_dir_all(src) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(at("emptying", e)),
        _ => {}
    }
    std::fs::create_dir_all(src).map_err(|e| at("creating", e))?;
    let file = src.with_extension("tar.gz");
    std::fs::write(&file, tar).map_err(|e| format!("writing {}: {e}", file.display()))?;
    let out = tokio::process::Command::new("tar")
        .arg("-xzf")
        .arg(&file)
        .arg("-C")
        .arg(src)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| format!("tar: {e}"))?;
    let _ = std::fs::remove_file(&file);
    if out.status.success() {
        Ok(())
    } else {
        Err(format!("unpacking the source failed ({}): {}", out.status, String::from_utf8_lossy(&out.stderr).trim()))
    }
}

/// Saves `[shell] from` in the user config, so `trailhead app` builds and runs that server's builds here.
pub(crate) fn build_from(paths: &Paths, endpoint: &str) -> Result<()> {
    parse_endpoint_id(endpoint).map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut cfg = crate::config::load(paths)?;
    let table = cfg.shell.get_or_insert_with(Default::default);
    if table.contains_key("repo") {
        bail!(
            "{} has `[shell] repo`: this machine builds the window from its own apps repo. Remove it to build another server's builds instead",
            crate::config::path(paths).display()
        );
    }
    table.insert("from".into(), toml::Value::String(endpoint.trim().to_string()));
    crate::config::save(paths, &cfg)?;
    println!(
        "saved in {}. `trailhead app` now builds the window from that server's builds and runs it; the first build takes a while",
        crate::config::path(paths).display()
    );
    Ok(())
}

/// `trailhead app` with `[shell] from`: reaches the server, builds its newest build here when this machine has
/// none, keeps building its newer ones while the window runs, and runs the window as the launcher always does.
pub(crate) async fn launch_remote(
    paths: &Paths,
    from: &str,
    config: ShellConfig,
    layout: &Layout,
    args: &[OsString],
) -> Result<i32> {
    let link = Link::open(paths, from).await?;
    let builder = Builder::new(FromServer::new(link, from), config, std::path::PathBuf::new(), layout.clone());
    if layout.load().current.is_none() {
        eprintln!(
            "trailhead app: building the window for the first time, which takes a while (ten minutes or more); cargo's output is in {}",
            layout.log().display()
        );
        if let Step::Failed(rev) = builder.step().await.map_err(anyhow::Error::msg)? {
            let why = layout.load().error.map(|e| e.message).unwrap_or_default();
            bail!("the build of {} failed ({why}); its output is in {}", short(&rev), layout.log().display());
        }
    }
    let building = builder.spawn();
    // The window dials the server it is built from, unless told another.
    let dial = [OsString::from(from)];
    let args = if args.is_empty() { &dial[..] } else { args };
    let state = layout.state_file();
    let read = || ShellBuild { configured: true, ..layout.read() };
    // The window runs for hours on this thread; the builder's task carries on beside it.
    let code = tokio::task::block_in_place(|| {
        launch(layout, read, |binary, rev| run(binary, rev, args, Some(&state)), &paths.config_dir)
    });
    building.stop().await;
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_build_s_source_replaces_what_was_unpacked_before() {
        let dir = tempfile::tempdir().unwrap();
        let (from, src) = (dir.path().join("from"), dir.path().join("src"));
        std::fs::create_dir_all(from.join("shell")).unwrap();
        std::fs::write(from.join("Cargo.toml"), "[workspace]").unwrap();
        std::fs::write(from.join("shell/main.rs"), "fn main() {}").unwrap();
        let tar =
            std::process::Command::new("tar").arg("-czf").arg("-").arg("-C").arg(&from).arg(".").output().unwrap();
        assert!(tar.status.success());
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("left-over.rs"), "old").unwrap();

        unpack(&tar.stdout, &src).await.unwrap();
        assert_eq!(std::fs::read_to_string(src.join("shell/main.rs")).unwrap(), "fn main() {}");
        assert!(src.join("Cargo.toml").is_file() && !src.join("left-over.rs").exists(), "the last build's files go");
        assert!(!src.with_extension("tar.gz").exists(), "and the download isn't kept");

        let bad = unpack(b"not a tar", &src).await.unwrap_err();
        assert!(bad.starts_with("unpacking the source failed"), "{bad}");
    }

    /// A server that answers each request in turn with the body given for its path, and 404 for any other.
    async fn serve(answers: Vec<(&'static str, Vec<u8>)>) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            loop {
                let (mut tcp, _) = listener.accept().await.unwrap();
                let mut head = vec![0; 4096];
                let n = tcp.read(&mut head).await.unwrap();
                let head = String::from_utf8_lossy(&head[..n]).into_owned();
                let asked = head.split_whitespace().nth(1).unwrap_or_default().to_string();
                assert!(head.to_ascii_lowercase().contains("authorization: bearer tok"), "{head}");
                let (status, body) = match answers.iter().find(|(path, _)| *path == asked) {
                    Some((_, body)) => ("200 OK", body.clone()),
                    None => ("404 Not Found", b"the server keeps no build of that".to_vec()),
                };
                let head = format!("HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len());
                tcp.write_all(head.as_bytes()).await.unwrap();
                tcp.write_all(&body).await.unwrap();
            }
        });
        base
    }

    fn tools(base: String) -> FromServer {
        let link = Link { _tunnel: None, http: reqwest::Client::new(), base, token: "tok".into() };
        FromServer::new(link, "abc")
    }

    #[tokio::test]
    async fn the_head_is_the_servers_newest_build_and_a_checkout_is_its_source() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("from");
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join("Cargo.toml"), "[workspace]").unwrap();
        let tar =
            std::process::Command::new("tar").arg("-czf").arg("-").arg("-C").arg(&from).arg(".").output().unwrap();
        let build = br#"{"configured": true, "current": "c0ffee1", "previous": null, "building": null, "error": null, "bad": []}"#;
        let tools =
            tools(serve(vec![("/v1/shell/build", build.to_vec()), ("/v1/shell/source/c0ffee1", tar.stdout)]).await);
        let origin = tools.origin(Path::new("")).await.unwrap();
        assert_eq!(tools.head(&origin).await.unwrap(), "c0ffee1");
        let src = dir.path().join("src");
        tools.checkout(&src, &origin, "c0ffee1").await.unwrap();
        assert_eq!(std::fs::read_to_string(src.join("Cargo.toml")).unwrap(), "[workspace]");
        let gone = tools.checkout(&src, &origin, "0ddba11").await.unwrap_err();
        assert!(gone.contains("404") && gone.contains("keeps no build"), "{gone}");
    }

    #[tokio::test]
    async fn a_server_with_no_builder_or_no_build_yet_says_so() {
        let off =
            br#"{"configured": false, "current": null, "previous": null, "building": null, "error": null, "bad": []}"#;
        let tools_off = tools(serve(vec![("/v1/shell/build", off.to_vec())]).await);
        let origin = tools_off.origin(Path::new("")).await.unwrap();
        assert!(tools_off.head(&origin).await.unwrap_err().contains("builds no shell"));
        let none = br#"{"configured": true, "current": null, "previous": null, "building": "c0ffee1", "error": null, "bad": []}"#;
        let tools_none = tools(serve(vec![("/v1/shell/build", none.to_vec())]).await);
        assert!(tools_none.head(&origin).await.unwrap_err().contains("hasn't built a shell yet"));
    }

    #[test]
    fn the_allow_lines_are_said_only_when_the_server_refused_this_machine() {
        let refused = Trouble::Refused.in_a_terminal("abc", "  trailhead factory allow 123");
        assert!(refused.contains("doesn't allow this machine") && refused.contains("trailhead factory allow 123"));
        let silent = Trouble::Unreachable("timed out".into()).in_a_terminal("abc", "  trailhead factory allow 123");
        assert!(silent.contains("can't reach the server at abc (timed out)"), "{silent}");
        assert!(!silent.contains("factory allow"), "{silent}");
        assert!(Trouble::Unreachable("timed out".into()).in_a_dialog().contains("timed out"));
        assert!(Trouble::Refused.in_a_dialog().contains("in the log"));
    }

    #[test]
    fn a_key_is_made_once_and_both_endpoints_are_named_to_allow() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shell/launcher.key");
        let made = load_or_create_key(&path).unwrap();
        assert_eq!(load_or_create_key(&path).unwrap().public(), made.public(), "the same identity next time");
        let window = load_or_create_key(&dir.path().join("shell/endpoint.key")).unwrap();
        let lines = allow_lines(&made, &window);
        let launcher_at = lines.find(&format!("allow {}", made.public())).expect("the launcher's line");
        let window_at = lines.find(&format!("allow {}", window.public())).expect("the window's line");
        assert!(lines[..launcher_at].contains("this launcher"), "each line says what it lets in: {lines}");
        assert!(lines[launcher_at..window_at].contains("the window itself"), "{lines}");
    }
}
