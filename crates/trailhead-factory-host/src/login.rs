//! `trailhead factory login sprites|claude|github|github-writer`: saves the factory's secrets in the credential store
//! (the OS keyring, else a `0600` file), so `run` needs no environment variables or fnox. The environment still wins when set.
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
    // A wrapped token spans lines, so keep reading: to the end of input, or on a terminal (which has none until
    // Ctrl-D) until a blank line after the token's first line.
    if interactive {
        eprint!("Claude token (from `claude setup-token`; end with a blank line): ");
    }
    let mut all = String::new();
    if interactive {
        loop {
            let mut line = String::new();
            let n = input.read_line(&mut line).map_err(|e| format!("reading stdin: {e}"))?;
            if n == 0 || (line.trim().is_empty() && !all.is_empty()) {
                break;
            }
            all.push_str(&line);
        }
    } else {
        input.read_to_string(&mut all).map_err(|e| format!("reading stdin: {e}"))?;
    }
    let token = clean_claude(&all);
    if token.is_empty() {
        return Err("no token on stdin".into());
    }
    check_claude_shape(&token)?;
    saved("Claude token", creds, CLAUDE, &token)
}

const CLAUDE_PREFIX: &str = "sk-ant-oat";
/// Real tokens run about 108 characters; a terminal wraps a pasted one at 80 or fewer.
const CLAUDE_MIN_LEN: usize = 90;

/// Rejects what can't be a `claude setup-token` token, such as one cut short by a paste that spanned lines.
fn check_claude_shape(token: &str) -> Result<(), String> {
    if !token.starts_with(CLAUDE_PREFIX) {
        return Err(format!(
            "that doesn't look like a Claude token from `claude setup-token` (it starts `{CLAUDE_PREFIX}`)"
        ));
    }
    if token.len() < CLAUDE_MIN_LEN {
        return Err(format!(
            "the Claude token is only {} characters, so it looks truncated (a pasted token may have wrapped); nothing was saved",
            token.len()
        ));
    }
    Ok(())
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
pub fn github_writer_token(creds: &CredStore) -> Option<String> {
    resolve(std::env::var("GITHUB_WRITER_TOKEN").ok(), creds, GITHUB_WRITER)
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

    const TOKEN: &str =
        "sk-ant-oat01-abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijklmnopqrstuvwxyz";

    #[test]
    fn claude_tokens_lose_their_whitespace() {
        let dir = tempfile::tempdir().unwrap();
        let creds = store(&dir);
        let (a, b) = TOKEN.split_at(40);
        let wrapped = format!("{a}\n  {}\r\n{} \n", &b[..20], &b[20..]);
        let msg = claude(&creds, &mut wrapped.as_bytes(), false).unwrap();
        assert!(!msg.contains("abc"), "{msg}");
        assert_eq!(creds.read(CLAUDE).unwrap().0, TOKEN);
        assert!(claude(&creds, &mut "  \n".as_bytes(), false).is_err());
    }

    #[test]
    fn a_terminal_reads_a_wrapped_token_up_to_a_blank_line() {
        let dir = tempfile::tempdir().unwrap();
        let creds = store(&dir);
        let (a, b) = TOKEN.split_at(40);
        let pasted = format!("{a}\n{b}\n\nignored\n");
        claude(&creds, &mut pasted.as_bytes(), true).unwrap();
        assert_eq!(creds.read(CLAUDE).unwrap().0, TOKEN);
    }

    #[test]
    fn a_misshapen_claude_token_is_not_saved() {
        let dir = tempfile::tempdir().unwrap();
        let creds = store(&dir);
        let cut = format!("{}\n", &TOKEN[..40]);
        let err = claude(&creds, &mut cut.as_bytes(), false).unwrap_err();
        assert!(err.contains("truncated") && !err.contains(&TOKEN[..40]), "{err}");
        let err = claude(&creds, &mut format!("nope{TOKEN}\n").as_bytes(), false).unwrap_err();
        assert!(err.contains("doesn't look like"), "{err}");
        assert!(creds.read(CLAUDE).is_none());
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
