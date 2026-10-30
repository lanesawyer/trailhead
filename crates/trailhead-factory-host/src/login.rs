//! `trailhead factory login sprites|claude|github`: saves the factory's secrets in the credential store (the OS
//! keyring, else a `0600` file), so `run` needs no environment variables or fnox. The environment still wins when set.
//! Secrets are read from stdin and never printed.

use std::io::{BufRead, IsTerminal};

use clap::Subcommand;
use trailhead_codehost_github::GitHub;
use trailhead_compute_sprites::SpritesCompute;
use trailhead_factory::Repo;
use trailhead_server::creds::{Cred, CredStore, Saved};

pub const SPRITES: Cred<'static> = Cred::Secret("sprites");
pub const CLAUDE: Cred<'static> = Cred::Secret("claude");
pub const GITHUB: Cred<'static> = Cred::Secret("github");

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
}

/// A secret from the environment if it's set and not blank, else the one saved by `login`.
pub fn resolve(env: Option<String>, creds: &CredStore, cred: Cred) -> Option<String> {
    env.map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .or_else(|| creds.read(cred).map(|(v, _)| v).filter(|v| !v.is_empty()))
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

    #[test]
    fn the_environment_wins_over_the_saved_secret() {
        let dir = tempfile::tempdir().unwrap();
        let creds = store(&dir);
        assert_eq!(resolve(None, &creds, SPRITES), None);
        creds.save(SPRITES, "saved").unwrap();
        assert_eq!(resolve(None, &creds, SPRITES).as_deref(), Some("saved"));
        assert_eq!(resolve(Some("  ".into()), &creds, SPRITES).as_deref(), Some("saved"));
        assert_eq!(resolve(Some("env".into()), &creds, SPRITES).as_deref(), Some("env"));
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
}
