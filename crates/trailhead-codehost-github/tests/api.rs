//! `GitHub` against an in-process stand-in for GitHub's REST API with a small object store, so publishing can be
//! checked by the tree it produces.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use base64::Engine;
use serde_json::{Value, json};
use trailhead_codehost_github::{GitHub, Identity};
use trailhead_factory::{CodeHost, Commit, Error, FileChange, MergeOutcome, NewPr, Repo};

#[derive(Default)]
struct Git {
    next: u64,
    blobs: BTreeMap<String, Vec<u8>>,
    trees: BTreeMap<String, BTreeMap<String, String>>,
    commits: BTreeMap<String, Value>,
    refs: BTreeMap<String, String>,
    prs: Vec<Value>,
    comments: Vec<(u64, String)>,
    unmergeable: bool,
    auth: Vec<Option<String>>,
}

impl Git {
    fn id(&mut self, kind: &str) -> String {
        self.next += 1;
        format!("{kind}{}", self.next)
    }
}

type Shared = Arc<Mutex<Git>>;

fn seed() -> Shared {
    let mut git = Git::default();
    let blob = git.id("blob");
    git.blobs.insert(blob.clone(), b"v1\n".to_vec());
    git.blobs.insert("blob-old".into(), b"bye\n".to_vec());
    let tree = git.id("tree");
    git.trees.insert(tree.clone(), BTreeMap::from([("README.md".into(), blob), ("old.txt".into(), "blob-old".into())]));
    let commit = git.id("commit");
    git.commits
        .insert(commit.clone(), json!({ "sha": commit, "tree": { "sha": tree }, "parents": [], "message": "init" }));
    git.refs.insert("main".into(), commit);
    Arc::new(Mutex::new(git))
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "message": "Not Found" }))).into_response()
}

async fn get_ref(
    State(g): State<Shared>,
    headers: HeaderMap,
    Path((_, _, branch)): Path<(String, String, String)>,
) -> Response {
    let mut g = g.lock().unwrap();
    g.auth.push(headers.get("authorization").map(|h| h.to_str().unwrap().to_string()));
    match g.refs.get(&branch) {
        Some(sha) => Json(json!({ "ref": format!("refs/heads/{branch}"), "object": { "sha": sha } })).into_response(),
        None => not_found(),
    }
}

async fn patch_ref(
    State(g): State<Shared>,
    Path((_, _, branch)): Path<(String, String, String)>,
    Json(b): Json<Value>,
) -> Response {
    let mut g = g.lock().unwrap();
    assert_eq!(b["force"], json!(false), "publishing never forces");
    g.refs.insert(branch, b["sha"].as_str().unwrap().into());
    Json(json!({})).into_response()
}

async fn create_ref(State(g): State<Shared>, Json(b): Json<Value>) -> Response {
    let mut g = g.lock().unwrap();
    let name = b["ref"].as_str().unwrap().strip_prefix("refs/heads/").unwrap().to_string();
    if g.refs.contains_key(&name) {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "message": "Reference already exists" })))
            .into_response();
    }
    g.refs.insert(name, b["sha"].as_str().unwrap().into());
    (StatusCode::CREATED, Json(json!({}))).into_response()
}

async fn get_commit(State(g): State<Shared>, Path((_, _, sha)): Path<(String, String, String)>) -> Response {
    match g.lock().unwrap().commits.get(&sha) {
        Some(c) => Json(c.clone()).into_response(),
        None => not_found(),
    }
}

async fn create_blob(State(g): State<Shared>, Json(b): Json<Value>) -> Response {
    let mut g = g.lock().unwrap();
    assert_eq!(b["encoding"], "base64");
    let bytes = base64::engine::general_purpose::STANDARD.decode(b["content"].as_str().unwrap()).unwrap();
    let sha = g.id("blob");
    g.blobs.insert(sha.clone(), bytes);
    (StatusCode::CREATED, Json(json!({ "sha": sha }))).into_response()
}

async fn create_tree(State(g): State<Shared>, Json(b): Json<Value>) -> Response {
    let mut g = g.lock().unwrap();
    let mut files = g.trees[b["base_tree"].as_str().unwrap()].clone();
    for e in b["tree"].as_array().unwrap() {
        let path = e["path"].as_str().unwrap().to_string();
        match e["sha"].as_str() {
            Some(sha) => files.insert(path, sha.into()),
            None => files.remove(&path),
        };
    }
    let sha = g.id("tree");
    g.trees.insert(sha.clone(), files);
    (StatusCode::CREATED, Json(json!({ "sha": sha }))).into_response()
}

async fn create_commit(State(g): State<Shared>, Json(mut b): Json<Value>) -> Response {
    let mut g = g.lock().unwrap();
    let sha = g.id("commit");
    b["sha"] = json!(sha);
    b["tree"] = json!({ "sha": b["tree"] });
    g.commits.insert(sha.clone(), b);
    (StatusCode::CREATED, Json(json!({ "sha": sha }))).into_response()
}

async fn pulls(State(g): State<Shared>, Json(b): Json<Value>) -> Response {
    let mut g = g.lock().unwrap();
    if g.prs.iter().any(|p| p["head"] == b["head"]) {
        let msg = "Validation Failed: A pull request already exists";
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "message": msg }))).into_response();
    }
    let number = g.prs.len() as u64 + 1;
    let pr = json!({ "number": number, "html_url": format!("https://github.test/pull/{number}"), "head": b["head"], "base": b["base"] });
    g.prs.push(pr.clone());
    (StatusCode::CREATED, Json(pr)).into_response()
}

async fn list_pulls(
    State(g): State<Shared>,
    axum::extract::Query(q): axum::extract::Query<BTreeMap<String, String>>,
) -> Response {
    let g = g.lock().unwrap();
    let branch = q["head"].split_once(':').unwrap().1.to_string();
    Json(json!(g.prs.iter().filter(|p| p["head"] == branch).collect::<Vec<_>>())).into_response()
}

async fn pull_files(State(g): State<Shared>, Path((_, _, n)): Path<(String, String, u64)>) -> Response {
    let g = g.lock().unwrap();
    let Some(pr) = g.prs.get(n as usize - 1) else { return not_found() };
    let head = &g.refs[pr["head"].as_str().unwrap()];
    let tree = &g.trees[g.commits[head]["tree"]["sha"].as_str().unwrap()];
    let base = &g.trees[g.commits[&g.refs["main"]]["tree"]["sha"].as_str().unwrap()];
    let mut files = Vec::new();
    for (path, blob) in tree {
        let content = String::from_utf8_lossy(&g.blobs[blob]).into_owned();
        match base.get(path) {
            None => files.push(json!({ "filename": path, "status": "added", "patch": format!("@@ -0,0 +1 @@\n+{}", content.trim_end()) })),
            Some(b) if b != blob => files.push(json!({ "filename": path, "status": "modified", "patch": "@@ -1 +1 @@" })),
            Some(_) => {}
        }
    }
    for path in base.keys().filter(|p| !tree.contains_key(*p)) {
        files.push(json!({ "filename": path, "status": "removed" }));
    }
    Json(json!(files)).into_response()
}

async fn comment(
    State(g): State<Shared>,
    Path((_, _, n)): Path<(String, String, u64)>,
    Json(b): Json<Value>,
) -> Response {
    g.lock().unwrap().comments.push((n, b["body"].as_str().unwrap().into()));
    (StatusCode::CREATED, Json(json!({}))).into_response()
}

async fn merge(State(g): State<Shared>, Json(b): Json<Value>) -> Response {
    assert_eq!(b["merge_method"], "squash");
    if g.lock().unwrap().unmergeable {
        return (StatusCode::METHOD_NOT_ALLOWED, Json(json!({ "message": "Pull Request is not mergeable" })))
            .into_response();
    }
    Json(json!({ "merged": true })).into_response()
}

async fn serve() -> (Shared, String) {
    let git = seed();
    let app = Router::new()
        .route("/repos/{o}/{r}/git/ref/heads/{*branch}", get(get_ref))
        .route("/repos/{o}/{r}/git/refs/heads/{*branch}", axum::routing::patch(patch_ref))
        .route("/repos/{o}/{r}/git/refs", post(create_ref))
        .route("/repos/{o}/{r}/git/commits/{sha}", get(get_commit))
        .route("/repos/{o}/{r}/git/commits", post(create_commit))
        .route("/repos/{o}/{r}/git/blobs", post(create_blob))
        .route("/repos/{o}/{r}/git/trees", post(create_tree))
        .route("/repos/{o}/{r}/pulls", post(pulls).get(list_pulls))
        .route("/repos/{o}/{r}/pulls/{n}/files", get(pull_files))
        .route("/repos/{o}/{r}/pulls/{n}/merge", put(merge))
        .route("/repos/{o}/{r}/issues/{n}/comments", post(comment))
        .with_state(git.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (git, url)
}

fn repo() -> Repo {
    Repo { owner: "lanesawyer".into(), name: "trailhead".into() }
}

fn change(path: &str, contents: Option<&[u8]>) -> FileChange {
    FileChange { path: path.into(), contents: contents.map(<[u8]>::to_vec) }
}

/// The files at a branch's head, with their contents.
fn files(git: &Shared, branch: &str) -> BTreeMap<String, Vec<u8>> {
    let g = git.lock().unwrap();
    let tree = g.commits[&g.refs[branch]]["tree"]["sha"].as_str().unwrap().to_string();
    g.trees[&tree].iter().map(|(p, b)| (p.clone(), g.blobs[b].clone())).collect()
}

#[tokio::test]
async fn publish_builds_commits_on_a_new_branch_then_on_top_of_it() {
    let (git, url) = serve().await;
    let bot = Identity { name: "trailhead-factory".into(), email: "factory@trailhead.invalid".into() };
    let gh = GitHub::gateway(&url).with_author(bot);
    let first = Commit {
        message: "TRAIL-1: greet".into(),
        changes: vec![
            change("src/greet.rs", Some(b"pub fn hi() {}\n")),
            change("README.md", Some(b"v2\n")),
            change("old.txt", None),
            change("logo.bin", Some(&[0, 159, 146, 150])),
        ],
    };
    gh.publish(&repo(), "main", "factory/trail-1", std::slice::from_ref(&first)).await.unwrap();

    let head = files(&git, "factory/trail-1");
    assert_eq!(head.keys().map(String::as_str).collect::<Vec<_>>(), ["README.md", "logo.bin", "src/greet.rs"]);
    assert_eq!(head["README.md"], b"v2\n");
    assert_eq!(head["logo.bin"], [0, 159, 146, 150], "binary files survive");
    {
        let g = git.lock().unwrap();
        let commit = &g.commits[&g.refs["factory/trail-1"]];
        assert_eq!(commit["parents"], json!([g.refs["main"]]), "branched from main");
        assert_eq!(commit["author"]["name"], "trailhead-factory");
        assert!(g.auth.iter().all(Option::is_none), "no token through the gateway");
    }

    let second = Commit {
        message: "TRAIL-1: test".into(),
        changes: vec![change("tests/greet.rs", Some(b"#[test] fn t() {}\n"))],
    };
    gh.publish(&repo(), "main", "factory/trail-1", &[second]).await.unwrap();
    let head = files(&git, "factory/trail-1");
    assert!(head.contains_key("tests/greet.rs") && head.contains_key("src/greet.rs"), "stacked on the branch");

    let missing = gh.publish(&repo(), "nope", "factory/trail-2", &[first]).await.unwrap_err();
    assert!(matches!(missing, Error::NotFound(_)), "{missing:?}");
}

#[tokio::test]
async fn prs_open_once_and_take_reviews_and_merges() {
    let (git, url) = serve().await;
    let gh = GitHub::new(&url, Some("ghp_test".into()));
    let commit = Commit { message: "x".into(), changes: vec![change("a.txt", Some(b"a")), change("old.txt", None)] };
    gh.publish(&repo(), "main", "factory/trail-1", &[commit]).await.unwrap();
    let new = NewPr {
        branch: "factory/trail-1".into(),
        base: "main".into(),
        title: "TRAIL-1: greet".into(),
        body: "Adds hi().".into(),
    };

    let pr = gh.open_pr(&repo(), new.clone()).await.unwrap();
    assert_eq!(pr.number, 1);
    let again = gh.open_pr(&repo(), new).await.unwrap();
    assert_eq!(again, pr, "a retry finds the open PR");
    assert!(git.lock().unwrap().auth.iter().all(|a| a.as_deref() == Some("Bearer ghp_test")));

    let diff = gh.diff(&pr).await.unwrap();
    assert!(diff.contains("diff --git a/a.txt b/a.txt\n--- /dev/null\n+++ b/a.txt\n@@ -0,0 +1 @@\n+a\n"), "{diff}");
    assert!(diff.contains("--- a/old.txt\n+++ /dev/null\n(deleted)"), "{diff}");
    gh.comment(&pr, "Looks right.").await.unwrap();
    assert_eq!(git.lock().unwrap().comments, [(1, "Looks right.".to_string())]);

    assert_eq!(gh.merge(&pr).await.unwrap(), MergeOutcome::Merged);
    git.lock().unwrap().unmergeable = true;
    assert_eq!(gh.merge(&pr).await.unwrap(), MergeOutcome::Conflict);
}
