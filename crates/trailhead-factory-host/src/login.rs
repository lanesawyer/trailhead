//! `trailhead factory login sprites|claude|github|github-writer`: saves the factory's secrets in the credential store
//! (the OS keyring, else a `0600` file), so `run` needs no environment variables or fnox. The environment wins when
//! set, then fnox, then these. Secrets are read from stdin and never printed.

use std::io::{BufRead, IsTerminal};

use clap::Subcommand;
use trailhead_codehost_github::GitHub;
use trailhead_compute_sprites::SpritesCompute;
use trailhead_factory::Repo;
use trailhead_server::creds::{Cred, CredStore, Saved};

use crate::fnox::Fnox;

pub const SPRITES: Cred<'static> = Cred::Secret("sprites");
pub const CLAUDE: Cred<'static> = Cred::Secret("claude");
pub const GITHUB: Cred<'static> = Cred::Secret("github");
/// The Docker gateway's GitHub token. Separate from [`GITHUB`] so the control plane's own token stays read-only.
pub const GITHUB_WRITER: Cred<'static> = Cred::Secret("github-writer");

#[derive(Subcommand)]
pub enum Login {
    /// Save a Sprites API token (`SPRITES_TOKEN`) after checking it against the Sprites API.
    Sprites,
    /// Save a Claude subscription token from `claude setup-token` (`CLAUDE_CODE_OAUTH_TOKEN`).
    Claude,
    /// Save a GitHub token (`GITHUB_TOKEN`) after checking it can read the repo's pull requests.
    Github {
        /// The repo file whose `[factory]` names the repo to check. Default: the nearest `.trailhead.toml`.
        #[arg(long)]
        config: Option<std::path::PathBuf>,
    },
    /// Save the Docker gateway's GitHub token (`GITHUB_WRITER_TOKEN`) after checking it can push to the repo. It needs
    /// contents, pull requests and issues write; the read-only `login github` token is kept separate.
    GithubWriter {
        /// The repo file whose `[factory]` names the repo to check. Default: the nearest `.trailhead.toml`.
        #[arg(long)]
        config: Option<std::path::PathBuf>,
    },
}

/// Where a secret came from, for the startup log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Env,
    Fnox,
    Keyring,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Env => "env",
            Source::Fnox => "fnox",
            Source::Keyring => "keyring",
        }
    }
}

/// A secret from the environment if it's set and not blank, else the one fnox defines, else the one saved by `login`.
/// Logs which source it came from, never the value.
pub fn resolve(name: &str, env: Option<String>, fnox: &Fnox, creds: &CredStore, cred: Cred) -> Option<String> {
    let found = match env.map(|v| v.trim().to_string()).filter(|v| !v.is_empty()) {
        Some(v) => Some((v, Source::Env)),
        None => match fnox.get(name) {
            Some(v) => Some((v, Source::Fnox)),
            None => creds.read(cred).map(|(v, _)| v).filter(|v| !v.is_empty()).map(|v| (v, Source::Keyring)),
        },
    };
    match found {
        Some((v, source)) => {
            tracing::info!("{name} from {}", source.label());
            Some(v)
        }
        None => None,
    }
}

/// The first line of `input`, trimmed; prompts on stderr first when stdin is a terminal.
fn read_secret(prompt: &str, input: &mut impl BufRead, interactive: bool) -> Result<String, String> {
    if interactive {
        eprint!("{prompt}: ");
    }
    let mut line = String::new();
    input.read_line(&mut line).map_err(|e| format!("reading stdin: {e}"))?;
    let secret = line.trim().to_string();
    if secret.is_empty() {
        return Err("no token on stdin".into());
    }
    Ok(secret)
}

/// A `claude setup-token` token with the whitespace stripped, since terminals wrap it across lines.
fn clean_claude(input: &str) -> String {
    input.chars().filter(|c| !c.is_whitespace()).collect()
}

fn saved(what: &str, creds: &CredStore, cred: Cred, secret: &str) -> Result<String, String> {
    let at = match creds.save(cred, secret).map_err(|e| format!("saving the {what}: {e}"))? {
        Saved::Keyring => "the OS keyring".to_string(),
        Saved::File(path) => path.display().to_string(),
    };
    Ok(format!("saved the {what} in {at}"))
}

pub async fn sprites(
    creds: &CredStore,
    input: &mut impl BufRead,
    interactive: bool,
    api: &str,
) -> Result<String, String> {
    let token = read_secret("Sprites token", input, interactive)?;
    SpritesCompute::with_url(api, &token)
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| format!("the Sprites API refused the token: {e}"))?;
    saved("Sprites token", creds, SPRITES, &token)
}

pub fn claude(creds: &CredStore, input: &mut impl BufRead, interactive: bool) -> Result<String, String> {
    // A wrapped token spans lines, so take everything up to the end of input.
    if interactive {
        eprint!("Claude token (from `claude setup-token`): ");
    }
    let mut all = String::new();
    if interactive {
        // A terminal has no end of input until Ctrl-D; one line is the token.
        input.read_line(&mut all).map_err(|e| format!("reading stdin: {e}"))?;
    } else {
        input.read_to_string(&mut all).map_err(|e| format!("reading stdin: {e}"))?;
    }
    let token = clean_claude(&all);
    if token.is_empty() {
        return Err("no token on stdin".into());
    }
    saved("Claude token", creds, CLAUDE, &token)
}

pub async fn github(
    creds: &CredStore,
    input: &mut impl BufRead,
    interactive: bool,
    api: &str,
    repo: &Repo,
) -> Result<String, String> {
    let token = read_secret("GitHub token", input, interactive)?;
    let user = GitHub::new(api, Some(token.clone()))
        .check_token(repo)
        .await
        .map_err(|e| format!("GitHub refused the token: {e}"))?;
    let msg = saved("GitHub token", creds, GITHUB, &token)?;
    Ok(format!("{msg} (user {user}, can read {}'s pull requests)", repo.slug()))
}

pub async fn github_writer(
    creds: &CredStore,
    input: &mut impl BufRead,
    interactive: bool,
    api: &str,
    repo: &Repo,
) -> Result<String, String> {
    let token = read_secret("GitHub token that can write", input, interactive)?;
    let user = GitHub::new(api, Some(token.clone()))
        .check_writer(repo)
        .await
        .map_err(|e| format!("GitHub refused the token: {e}"))?;
    let msg = saved("GitHub writer token", creds, GITHUB_WRITER, &token)?;
    Ok(format!("{msg} (user {user}, can push to {})", repo.slug()))
}

/// The gateway's GitHub token, from `GITHUB_WRITER_TOKEN` or the saved one. Never falls back to the read token.
#[cfg_attr(not(feature = "docker"), allow(dead_code))]
pub fn github_writer_token(fnox: &Fnox, creds: &CredStore) -> Option<String> {
    resolve("GITHUB_WRITER_TOKEN", std::env::var("GITHUB_WRITER_TOKEN").ok(), fnox, creds, GITHUB_WRITER)
}

pub async fn run(cmd: Login, creds: &CredStore) -> Result<(), String> {
    let interactive = std::io::stdin().is_terminal();
    let mut input = std::io::stdin().lock();
    let msg = match cmd {
        Login::Sprites => sprites(creds, &mut input, interactive, trailhead_compute_sprites::API_URL).await?,
        Login::Claude => claude(creds, &mut input, interactive)?,
        Login::Github { config } => {
            let repo = crate::find::repo(config.as_deref())?;
            github(creds, &mut input, interactive, trailhead_codehost_github::API_URL, &repo).await?
        }
        Login::GithubWriter { config } => {
            let repo = crate::find::repo(config.as_deref())?;
            github_writer(creds, &mut input, interactive, trailhead_codehost_github::API_URL, &repo).await?
        }
    };
    println!("{msg}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::extract::Path;
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::get;

    use super::*;

    fn store(dir: &tempfile::TempDir) -> CredStore {
        CredStore { config_dir: dir.path().into(), keyring: false }
    }

    async fn serve(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        url
    }

    fn authorized(h: &HeaderMap, token: &str) -> bool {
        h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {token}"))
    }

    /// A script standing in for `fnox`: `export --format json` defines SPRITES_TOKEN and a blank CLAUDE_CODE_OAUTH_TOKEN,
    /// and not GITHUB_TOKEN. With `fail` it exits 1 as a refused provider login would.
    fn fake_fnox(dir: &tempfile::TempDir, fail: bool) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.path().join("fnox");
        let body = if fail {
            "echo 'provider auth failed' >&2; exit 1"
        } else {
            "echo '{\"secrets\":{\"SPRITES_TOKEN\":\"from-fnox\",\"OTHER\":\"x\",\"CLAUDE_CODE_OAUTH_TOKEN\":\"\"}}'"
        };
        std::fs::write(&path, format!("#!/bin/sh\n[ \"$1 $2 $3\" = \"export --format json\" ] || exit 2\n{body}\n"))
            .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    /// A working directory holding a `fnox.toml`, which is all the host looks for before running fnox.
    fn repo_with_config(dir: &tempfile::TempDir) -> std::path::PathBuf {
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("fnox.toml"), "").unwrap();
        repo
    }

    fn load(program: &str, repo: &std::path::Path) -> Result<Fnox, String> {
        Fnox::load(program, repo, None)
    }

    #[test]
    fn the_environment_wins_over_fnox_and_fnox_over_the_saved_secret() {
        let _spawning = crate::fnox::SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let creds = store(&dir);
        let fnox = load(&fake_fnox(&dir, false), &repo_with_config(&dir)).unwrap();
        creds.save(SPRITES, "saved").unwrap();
        let got = |env: Option<&str>| resolve("SPRITES_TOKEN", env.map(Into::into), &fnox, &creds, SPRITES);
        assert_eq!(got(Some("env")).as_deref(), Some("env"));
        assert_eq!(got(Some("  ")).as_deref(), Some("from-fnox"));
        assert_eq!(got(None).as_deref(), Some("from-fnox"));
    }

    #[test]
    fn a_name_fnox_doesnt_define_or_leaves_blank_falls_back_to_the_saved_secret() {
        let _spawning = crate::fnox::SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let creds = store(&dir);
        let fnox = load(&fake_fnox(&dir, false), &repo_with_config(&dir)).unwrap();
        assert_eq!(resolve("GITHUB_TOKEN", None, &fnox, &creds, GITHUB), None);
        creds.save(GITHUB, "saved").unwrap();
        creds.save(CLAUDE, "saved-claude").unwrap();
        assert_eq!(resolve("GITHUB_TOKEN", None, &fnox, &creds, GITHUB).as_deref(), Some("saved"));
        assert_eq!(resolve("CLAUDE_CODE_OAUTH_TOKEN", None, &fnox, &creds, CLAUDE).as_deref(), Some("saved-claude"));
    }

    #[test]
    fn fnox_missing_or_without_a_config_defines_nothing() {
        let _spawning = crate::fnox::SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let creds = store(&dir);
        let repo = repo_with_config(&dir);
        let missing = dir.path().join("no-such-fnox").display().to_string();
        let fnox = load(&missing, &repo).unwrap();
        creds.save(GITHUB, "saved").unwrap();
        assert_eq!(resolve("GITHUB_TOKEN", None, &fnox, &creds, GITHUB).as_deref(), Some("saved"));
        assert_eq!(resolve("SPRITES_TOKEN", None, &fnox, &creds, SPRITES), None);
        // Even a fnox that would fail isn't run when there's no config for it to read.
        let bare = dir.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        let fnox = load(&fake_fnox(&dir, true), &bare);
        assert!(fnox.is_ok(), "{fnox:?}");
    }

    #[test]
    fn fnox_failing_against_a_config_is_an_error_naming_fnox() {
        let _spawning = crate::fnox::SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let err = load(&fake_fnox(&dir, true), &repo_with_config(&dir)).unwrap_err();
        assert!(err.contains("fnox") && err.contains("provider auth failed"), "{err}");
    }

    #[test]
    fn a_global_fnox_config_counts_as_a_config() {
        let _spawning = crate::fnox::SPAWNING.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("config.toml");
        std::fs::write(&global, "").unwrap();
        let bare = dir.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        let err = Fnox::load(&fake_fnox(&dir, true), &bare, Some(&global)).unwrap_err();
        assert!(err.contains("fnox"), "{err}");
    }

    #[test]
    fn claude_tokens_lose_their_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let creds = store(&dir);
        let mut input = "sk-ant-oat01-abc\n  def\r\nghi \n".as_bytes();
        let msg = claude(&creds, &mut input, false).unwrap();
        assert!(!msg.contains("abc"), "{msg}");
        assert_eq!(creds.read(CLAUDE).unwrap().0, "sk-ant-oat01-abcdefghi");
        assert!(claude(&creds, &mut "  \n".as_bytes(), false).is_err());
    }

    #[tokio::test]
    async fn a_sprites_token_is_checked_before_it_is_saved() {
        let app = Router::new().route(
            "/v1/sprites",
            get(|h: HeaderMap| async move {
                if authorized(&h, "good") {
                    (StatusCode::OK, r#"{"sprites":[],"has_more":false}"#)
                } else {
                    (StatusCode::UNAUTHORIZED, "no")
                }
            }),
        );
        let api = serve(app).await;
        let dir = tempfile::tempdir().unwrap();
        let creds = store(&dir);

        let err = sprites(&creds, &mut "bad\n".as_bytes(), false, &api).await.unwrap_err();
        assert!(!err.contains("bad\n") && creds.read(SPRITES).is_none(), "{err}");

        let msg = sprites(&creds, &mut "good\n".as_bytes(), false, &api).await.unwrap();
        assert!(!msg.contains("good"), "{msg}");
        assert_eq!(creds.read(SPRITES).unwrap().0, "good");
    }

    #[tokio::test]
    async fn a_github_token_must_read_the_repos_pulls() {
        let app = Router::new()
            .route(
                "/user",
                get(|h: HeaderMap| async move {
                    if h.get("authorization").is_some() {
                        (StatusCode::OK, r#"{"login":"octo"}"#)
                    } else {
                        (StatusCode::UNAUTHORIZED, "{}")
                    }
                }),
            )
            .route(
                "/repos/{o}/{r}/pulls",
                get(|Path((_, r)): Path<(String, String)>, h: HeaderMap| async move {
                    if r == "app" && authorized(&h, "good") {
                        (StatusCode::OK, "[]")
                    } else {
                        (StatusCode::NOT_FOUND, r#"{"message":"Not Found"}"#)
                    }
                }),
            );
        let api = serve(app).await;
        let dir = tempfile::tempdir().unwrap();
        let creds = store(&dir);
        let repo = Repo { owner: "acme".into(), name: "app".into() };

        let err = github(&creds, &mut "other\n".as_bytes(), false, &api, &repo).await.unwrap_err();
        assert!(!err.contains("other") && creds.read(GITHUB).is_none(), "{err}");

        let msg = github(&creds, &mut "good\n".as_bytes(), false, &api, &repo).await.unwrap();
        assert!(msg.contains("octo") && !msg.contains("good"), "{msg}");
        assert_eq!(creds.read(GITHUB).unwrap().0, "good");
    }

    #[tokio::test]
    async fn a_github_writer_token_must_push_and_stays_apart_from_the_read_token() {
        let app = Router::new().route("/user", get(|| async { (StatusCode::OK, r#"{"login":"octo"}"#) })).route(
            "/repos/{o}/{r}",
            get(|h: HeaderMap| async move {
                let push = authorized(&h, "writer");
                (StatusCode::OK, format!(r#"{{"permissions":{{"pull":true,"push":{push}}}}}"#))
            }),
        );
        let api = serve(app).await;
        let dir = tempfile::tempdir().unwrap();
        let creds = store(&dir);
        let repo = Repo { owner: "acme".into(), name: "app".into() };
        creds.save(GITHUB, "reader").unwrap();

        let err = github_writer(&creds, &mut "reader\n".as_bytes(), false, &api, &repo).await.unwrap_err();
        assert!(err.contains("can't push") && creds.read(GITHUB_WRITER).is_none(), "{err}");

        let msg = github_writer(&creds, &mut "writer\n".as_bytes(), false, &api, &repo).await.unwrap();
        assert!(msg.contains("octo") && !msg.contains("writer\n"), "{msg}");
        assert_eq!(creds.read(GITHUB_WRITER).unwrap().0, "writer");
        assert_eq!(creds.read(GITHUB).unwrap().0, "reader");
    }
}
