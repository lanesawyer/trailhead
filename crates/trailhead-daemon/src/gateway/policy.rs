//! Which requests a sandbox's role may make through a connection: method plus path-prefix allowlists, taken from
//! the sandbox's registered labels.

use trailhead_factory::{Labels, label};

struct Rule {
    methods: &'static [&'static str],
    /// A path prefix, matched by whole segments; a `*` segment matches any one segment.
    prefix: String,
}

fn rule(methods: &'static [&'static str], prefix: impl Into<String>) -> Rule {
    Rule { methods, prefix: prefix.into() }
}

const READ: &[&str] = &["GET", "HEAD"];
const WRITE: &[&str] = &["GET", "HEAD", "POST", "PATCH"];

/// A GitHub repo name as `owner/name`, restricted to what GitHub allows so it can't smuggle path syntax.
fn valid_repo(repo: &str) -> bool {
    let ok = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    };
    matches!(repo.split_once('/'), Some((o, n)) if ok(o) && ok(n))
}

/// The rules `labels` earn for `provider`. A sandbox without a role, or with one we don't know, gets none.
fn rules(provider: &str, labels: &Labels) -> Vec<Rule> {
    let Some(role) = labels.get(label::ROLE).map(String::as_str) else {
        return Vec::new();
    };
    if !matches!(role, "worker" | "reviewer") {
        return Vec::new();
    }
    match provider {
        "github" => {
            let Some(repo) = labels.get(label::REPO).filter(|r| valid_repo(r)) else {
                return Vec::new();
            };
            let r = format!("/repos/{repo}");
            if role == "worker" {
                vec![
                    rule(WRITE, format!("{r}/git")),
                    rule(WRITE, format!("{r}/pulls")),
                    rule(WRITE, format!("{r}/issues")),
                ]
            } else {
                vec![
                    rule(READ, r.clone()),
                    rule(&["POST"], format!("{r}/issues/*/comments")),
                    rule(&["POST"], format!("{r}/pulls/*/comments")),
                    rule(&["POST"], format!("{r}/pulls/*/reviews")),
                ]
            }
        }
        // The Messages API (with token counting and batches) and the model list. Files, skills and admin paths
        // aren't here.
        "anthropic" => vec![rule(&["POST", "GET", "HEAD"], "/v1/messages"), rule(&["GET", "HEAD"], "/v1/models")],
        // A model server on the host speaks the same shapes.
        "model" => vec![rule(&["POST", "GET", "HEAD"], "/v1")],
        _ => Vec::new(),
    }
}

/// Whether any request at all is allowed through `provider` connections.
pub fn grants(provider: &str, labels: &Labels) -> bool {
    !rules(provider, labels).is_empty()
}

fn matches_prefix(prefix: &str, path: &[&str]) -> bool {
    let want: Vec<&str> = prefix.trim_start_matches('/').split('/').collect();
    want.len() <= path.len() && want.iter().zip(path).all(|(w, p)| *w == "*" || w == p)
}

/// Allows or refuses `method` on `path` (no query), with the reason for a refusal.
pub fn check(provider: &str, labels: &Labels, method: &str, path: &str) -> Result<(), String> {
    let rules = rules(provider, labels);
    if rules.is_empty() {
        let why = match labels.get(label::ROLE) {
            None => "this sandbox has no role".to_string(),
            Some(role) => format!("role {role:?} has no access to {provider}"),
        };
        return Err(why);
    }
    let lower = path.to_ascii_lowercase();
    let segments: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    if !path.starts_with('/')
        || path.contains('\\')
        || lower.contains("%2e")
        || lower.contains("%2f")
        || lower.contains("%5c")
        || segments.iter().any(|s| matches!(*s, "." | ".."))
    {
        return Err("the path isn't allowed".to_string());
    }
    let segments: Vec<&str> = segments.into_iter().filter(|s| !s.is_empty()).collect();
    let method = method.to_ascii_uppercase();
    let role = labels.get(label::ROLE).map(String::as_str).unwrap_or_default();
    if rules.iter().any(|r| r.methods.contains(&method.as_str()) && matches_prefix(&r.prefix, &segments)) {
        Ok(())
    } else {
        Err(format!("role {role:?} may not {method} {path} through {provider}"))
    }
}
