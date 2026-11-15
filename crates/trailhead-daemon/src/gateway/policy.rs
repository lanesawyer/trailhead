//! Which requests a sandbox's role may make through a connection: method plus path-prefix allowlists, taken from
//! the sandbox's registered labels. A worker's ref writes are held to its own task's branch, by path for an update
//! and by body for a create ([`check_body`]).

use trailhead_factory::{Labels, TaskKey, label, task_branch};

struct Rule {
    methods: &'static [&'static str],
    /// A path prefix, matched by whole segments; a `*` segment matches any one segment.
    prefix: String,
    /// Whether the path must be exactly the prefix, with nothing after it.
    exact: bool,
}

fn rule(methods: &'static [&'static str], prefix: impl Into<String>) -> Rule {
    Rule { methods, prefix: prefix.into(), exact: false }
}

fn exact(methods: &'static [&'static str], path: impl Into<String>) -> Rule {
    Rule { methods, prefix: path.into(), exact: true }
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

/// The ref a worker may write: its task's branch. None without a task label that reads as a task key, so a
/// sandbox between tasks, or with a label that could smuggle path syntax, writes no ref at all.
fn own_ref(labels: &Labels) -> Option<String> {
    let task = labels.get(label::TASK)?;
    let ok = !task.is_empty() && task.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    ok.then(|| format!("refs/heads/{}", task_branch(&TaskKey(task.clone()))))
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
                // Git Data: reads anywhere, new objects anywhere (an object nothing points at changes nothing), but
                // only its own branch moved or created. The writer token could move any ref, the base branch included.
                let mut rules = vec![
                    rule(READ, format!("{r}/git")),
                    exact(&["POST"], format!("{r}/git/blobs")),
                    exact(&["POST"], format!("{r}/git/trees")),
                    exact(&["POST"], format!("{r}/git/commits")),
                    rule(WRITE, format!("{r}/pulls")),
                    rule(WRITE, format!("{r}/issues")),
                ];
                if let Some(own) = own_ref(labels) {
                    rules.push(exact(&["PATCH"], format!("{r}/git/{own}")));
                    rules.push(exact(&["POST"], format!("{r}/git/refs")));
                }
                rules
            } else {
                vec![
                    rule(READ, r.clone()),
                    rule(&["POST"], format!("{r}/issues/*/comments")),
                    rule(&["POST"], format!("{r}/pulls/*/comments")),
                    rule(&["POST"], format!("{r}/pulls/*/reviews")),
                ]
            }
        }
        // Smart-HTTP clone and fetch of the sandbox's own repo, read only: the ref advertisement and upload-pack.
        // The writer token behind it could push, so receive-pack has no rule (and `api` refuses its discovery).
        "git" => {
            let Some(repo) = labels.get(label::REPO).filter(|r| valid_repo(r)) else {
                return Vec::new();
            };
            vec![exact(READ, format!("{repo}.git/info/refs")), exact(&["POST"], format!("{repo}.git/git-upload-pack"))]
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

fn matches_rule(rule: &Rule, path: &[&str]) -> bool {
    let want: Vec<&str> = rule.prefix.trim_start_matches('/').split('/').collect();
    (if rule.exact { want.len() == path.len() } else { want.len() <= path.len() })
        && want.iter().zip(path).all(|(w, p)| *w == "*" || w == p)
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
    // An empty segment (`//`, a trailing `/`) is dropped for prefix rules, but an exact rule wants the path as
    // written, so it refuses one rather than match a spelling the upstream may read differently.
    let has_empty = path[1..].split('/').any(str::is_empty);
    let segments: Vec<&str> = segments.into_iter().filter(|s| !s.is_empty()).collect();
    let method = method.to_ascii_uppercase();
    let role = labels.get(label::ROLE).map(String::as_str).unwrap_or_default();
    if rules
        .iter()
        .any(|r| r.methods.contains(&method.as_str()) && !(r.exact && has_empty) && matches_rule(r, &segments))
    {
        Ok(())
    } else {
        Err(format!("role {role:?} may not {method} {path} through {provider}"))
    }
}

/// Refuses a request whose path [`check`] allows but whose body asks for more: creating a ref names it in the
/// body, so a worker's `POST .../git/refs` must name its own branch.
pub fn check_body(provider: &str, labels: &Labels, method: &str, path: &str, body: &[u8]) -> Result<(), String> {
    // Segments as `check` reads them, so an empty one (`git//refs`, a trailing slash) doesn't slip past.
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let creates_ref = provider == "github"
        && method.eq_ignore_ascii_case("POST")
        && labels.get(label::REPO).is_some_and(|repo| segments.join("/") == format!("repos/{repo}/git/refs"));
    if !creates_ref {
        return Ok(());
    }
    let named = serde_json::from_slice::<serde_json::Value>(body).ok();
    let named = named.as_ref().and_then(|v| v["ref"].as_str());
    match (named, own_ref(labels)) {
        (Some(named), Some(own)) if named == own => Ok(()),
        (_, Some(own)) => Err(format!("this sandbox may only create {own}")),
        (_, None) => Err("this sandbox has no task, so it may not create a ref".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(pairs: &[(&str, &str)]) -> Labels {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn exact_rules_refuse_empty_segments() {
        let l = labels(&[("role", "worker"), ("repo", "lane/trail"), ("task", "TRAIL-7")]);
        assert!(check("github", &l, "POST", "/repos/lane/trail/git/blobs").is_ok());
        for path in [
            "/repos/lane/trail/git/blobs/",
            "/repos/lane/trail//git/blobs",
            "//repos/lane/trail/git/blobs",
            "/repos/lane/trail/git/blobs//",
        ] {
            assert!(check("github", &l, "POST", path).is_err(), "{path}");
        }
    }
}
