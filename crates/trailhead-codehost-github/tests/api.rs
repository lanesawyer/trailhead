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
use trailhead_factory::{
    BranchUpdate, ChangedFile, CodeHost, Commit, Error, FileChange, FileStatus, MergeOutcome, NewPr, PrRef, PrState,
    Repo,
};

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
    /// A 422 message `update-branch` answers with, whatever the branches look like.
    update_refusal: Option<String>,
    workflow_scope: bool,
    /// How many more file listings answer 502, and how many were asked for.
    files_failures: usize,
    files_calls: usize,
    auth: Vec<Option<String>>,
}

impl Git {
    fn id(&mut self, kind: &str) -> String {
        self.next += 1;
        format!("{kind}{}", self.next)
    }

    /// 0 when `head`'s history includes `tip`, otherwise 1.
    fn behind_by(&self, tip: &str, head: &str) -> u64 {
        let mut todo = vec![head.to_string()];
        while let Some(sha) = todo.pop() {
            if sha == tip {
                return 0;
            }
            todo.extend(
                self.commits[&sha]["parents"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()),
            );
        }
        1
    }
}

impl Git {
    /// `sha` and everything it descends from.
    fn history(&self, sha: &str) -> std::collections::BTreeSet<String> {
        let mut seen = std::collections::BTreeSet::new();
        let mut todo = vec![sha.to_string()];
        while let Some(sha) = todo.pop() {
            if seen.insert(sha.clone()) {
                todo.extend(
                    self.commits[&sha]["parents"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()),
                );
            }
        }
        seen
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
    git.workflow_scope = true;
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
    // Like GitHub with a token lacking the `workflow` scope: a bare 404 for a commit that touches a workflow.
    let tree = g.commits[b["sha"].as_str().unwrap()]["tree"]["sha"].as_str().unwrap().to_string();
    if !g.workflow_scope && g.trees[&tree].keys().any(|p| p.starts_with(".github/workflows/")) {
        return not_found();
    }
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
    let pr = json!({ "number": number, "html_url": format!("https://github.test/pull/{number}"), "head": b["head"], "base": b["base"], "state": "open", "merged": false });
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
    let mut g = g.lock().unwrap();
    g.files_calls += 1;
    if g.files_failures > 0 {
        g.files_failures -= 1;
        return (StatusCode::BAD_GATEWAY, "Bad Gateway").into_response();
    }
    let Some(pr) = g.prs.get(n as usize - 1) else { return not_found() };
    let head = &g.refs[pr["head"].as_str().unwrap()];
    let tree = &g.trees[g.commits[head]["tree"]["sha"].as_str().unwrap()];
    let base = &g.trees[g.commits[&g.refs["main"]]["tree"]["sha"].as_str().unwrap()];
    let mut files = Vec::new();
    for (path, blob) in tree {
        let content = String::from_utf8_lossy(&g.blobs[blob]).into_owned();
        match base.get(path) {
            None => files.push(json!({ "filename": path, "status": "added", "additions": 1, "deletions": 0, "patch": format!("@@ -0,0 +1 @@\n+{}", content.trim_end()) })),
            Some(b) if b != blob => files.push(json!({ "filename": path, "status": "modified", "patch": "@@ -1 +1 @@" })),
            Some(_) => {}
        }
    }
    for path in base.keys().filter(|p| !tree.contains_key(*p)) {
        files.push(json!({ "filename": path, "status": "removed", "additions": 0, "deletions": 3 }));
    }
    Json(json!(files)).into_response()
}

async fn comment(
    State(g): State<Shared>,
    Path((_, _, n)): Path<(String, String, u64)>,
    Json(b): Json<Value>,
) -> Response {
    let mut g = g.lock().unwrap();
    g.comments.push((n, b["body"].as_str().unwrap().into()));
    (StatusCode::CREATED, Json(json!({ "id": g.comments.len() }))).into_response()
}

async fn edit_comment(
    State(g): State<Shared>,
    Path((_, _, id)): Path<(String, String, u64)>,
    Json(b): Json<Value>,
) -> Response {
    match g.lock().unwrap().comments.get_mut(id as usize - 1) {
        Some(c) => c.1 = b["body"].as_str().unwrap().into(),
        None => return not_found(),
    }
    Json(json!({})).into_response()
}

async fn get_pull(State(g): State<Shared>, Path((_, _, n)): Path<(String, String, u64)>) -> Response {
    let g = g.lock().unwrap();
    match g.prs.get(n as usize - 1) {
        Some(pr) => {
            // GitHub's shape: the refs and the head commit, next to what the test set.
            let mut pr = pr.clone();
            let branch = pr["head"].as_str().unwrap_or_default().to_string();
            pr["head"] = json!({ "ref": branch, "sha": g.refs.get(&branch) });
            pr["base"] = json!({ "ref": "main" });
            Json(pr).into_response()
        }
        None => not_found(),
    }
}

fn parents(commit: &Value) -> Vec<Value> {
    commit["parents"].as_array().unwrap().iter().map(|p| json!({ "sha": p })).collect()
}

/// `behind_by` for `main...<sha>`: 1 unless the head's history includes main's tip.
async fn compare(State(g): State<Shared>, Path((_, _, spec)): Path<(String, String, String)>) -> Response {
    let g = g.lock().unwrap();
    let (base, head) = spec.split_once("...").unwrap();
    let tip = g.refs.get(base).cloned().unwrap_or_else(|| base.to_string());
    if !g.commits.contains_key(&tip) || !g.commits.contains_key(head) {
        return (StatusCode::NOT_FOUND, Json(json!({ "message": "Not Found" }))).into_response();
    }
    let (from, to) = (g.history(&tip), g.history(head));
    let commits: Vec<Value> =
        to.difference(&from).map(|sha| json!({ "sha": sha, "parents": parents(&g.commits[sha]) })).collect();
    let status = match (to.contains(&tip), from.contains(head)) {
        (true, true) => "identical",
        (true, false) => "ahead",
        (false, true) => "behind",
        _ => "diverged",
    };
    Json(json!({
        "behind_by": g.behind_by(&tip, head),
        "status": status,
        "total_commits": commits.len(),
        "commits": commits,
    }))
    .into_response()
}

async fn update_branch(
    State(g): State<Shared>,
    Path((_, _, n)): Path<(String, String, u64)>,
    Json(b): Json<Value>,
) -> Response {
    let mut g = g.lock().unwrap();
    if let Some(message) = &g.update_refusal {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "message": message }))).into_response();
    }
    let branch = g.prs[n as usize - 1]["head"].as_str().unwrap().to_string();
    if b["expected_head_sha"] != json!(g.refs[&branch]) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "message": "expected head sha didn't match current head ref." })),
        )
            .into_response();
    }
    let main = g.refs["main"].clone();
    if g.behind_by(&main, &g.refs[&branch]) == 0 {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "message": "There are no new commits on the base branch." })),
        )
            .into_response();
    }
    if g.unmergeable {
        return (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "message": "merge conflict between base and head" })))
            .into_response();
    }
    let old = g.refs[&branch].clone();
    let tree = g.commits[&old]["tree"].clone();
    let sha = g.id("commit");
    g.commits.insert(sha.clone(), json!({ "sha": sha, "tree": tree, "parents": [old, main] }));
    g.refs.insert(branch, sha);
    (StatusCode::ACCEPTED, Json(json!({ "message": "Updating pull request branch." }))).into_response()
}

async fn merge(
    State(g): State<Shared>,
    Path((_, _, n)): Path<(String, String, u64)>,
    Json(b): Json<Value>,
) -> Response {
    assert_eq!(b["merge_method"], "squash");
    let mut g = g.lock().unwrap();
    let branch = g.prs[n as usize - 1]["head"].as_str().unwrap().to_string();
    if b["sha"] != json!(g.refs[&branch]) {
        return (StatusCode::CONFLICT, Json(json!({ "message": "Head branch was modified" }))).into_response();
    }
    if g.unmergeable {
        return (StatusCode::METHOD_NOT_ALLOWED, Json(json!({ "message": "Pull Request is not mergeable" })))
            .into_response();
    }
    let pr = &mut g.prs[n as usize - 1];
    pr["state"] = json!("closed");
    pr["merged"] = json!(true);
    pr["merge_commit_sha"] = json!(format!("squash{n}"));
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
        .route("/repos/{o}/{r}/pulls/{n}", get(get_pull))
        .route("/repos/{o}/{r}/pulls/{n}/files", get(pull_files))
        .route("/repos/{o}/{r}/pulls/{n}/merge", put(merge))
        .route("/repos/{o}/{r}/pulls/{n}/update-branch", put(update_branch))
        .route("/repos/{o}/{r}/compare/{spec}", get(compare))
        .route("/repos/{o}/{r}/issues/{n}/comments", post(comment))
        .route("/repos/{o}/{r}/issues/comments/{id}", axum::routing::patch(edit_comment))
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
/// A branch's head commit.
fn head(git: &Shared, branch: &str) -> String {
    git.lock().unwrap().refs[branch].clone()
}

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
        merges: vec![],
    };
    let main = head(&git, "main");
    gh.publish(&repo(), &main, "factory/trail-1", std::slice::from_ref(&first)).await.unwrap();

    let published = files(&git, "factory/trail-1");
    assert_eq!(published.keys().map(String::as_str).collect::<Vec<_>>(), ["README.md", "logo.bin", "src/greet.rs"]);
    assert_eq!(published["README.md"], b"v2\n");
    assert_eq!(published["logo.bin"], [0, 159, 146, 150], "binary files survive");
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
        merges: vec![],
    };
    let branch = head(&git, "factory/trail-1");
    gh.publish(&repo(), &branch, "factory/trail-1", std::slice::from_ref(&second)).await.unwrap();
    let files_now = files(&git, "factory/trail-1");
    assert!(
        files_now.contains_key("tests/greet.rs") && files_now.contains_key("src/greet.rs"),
        "stacked on the branch"
    );

    // Work that started from an older commit than the branch is at would overwrite it, so it's refused.
    let moved = gh.publish(&repo(), &main, "factory/trail-1", &[second]).await.unwrap_err();
    assert!(matches!(&moved, Error::Conflict(m) if m.contains("would overwrite")), "{moved:?}");

    let missing = gh.publish(&repo(), "nope", "factory/trail-2", &[first]).await.unwrap_err();
    assert!(matches!(missing, Error::NotFound(_)), "{missing:?}");
}

/// TRAIL-161: the checkout started at an older main, and main moved before publishing. The new branch is built on the
/// commit the work started from, so GitHub sees an ordinary branch and nothing newer on main is overwritten.
#[tokio::test]
async fn a_new_branch_starts_where_the_work_did_not_at_mains_new_head() {
    let (git, url) = serve().await;
    let gh = GitHub::gateway(&url);
    let started = head(&git, "main");
    let landed = Commit {
        message: "meanwhile on main".into(),
        changes: vec![change("README.md", Some(b"newer\n"))],
        merges: vec![],
    };
    gh.publish(&repo(), &started, "main", &[landed]).await.unwrap();
    let main_now = head(&git, "main");
    assert_ne!(main_now, started);

    let work = Commit {
        message: "TRAIL-1: greet".into(),
        changes: vec![change("src/greet.rs", Some(b"pub fn hi() {}\n"))],
        merges: vec![],
    };
    gh.publish(&repo(), &started, "factory/trail-1", &[work]).await.unwrap();
    let g = git.lock().unwrap();
    assert_eq!(g.commits[&g.refs["factory/trail-1"]]["parents"], json!([started]), "parented where the work started");
}

#[tokio::test]
async fn prs_open_once_and_take_reviews_and_merges() {
    let (git, url) = serve().await;
    let gh = GitHub::new(&url, Some("ghp_test".into()));
    let commit = Commit {
        message: "x".into(),
        changes: vec![change("a.txt", Some(b"a")), change("old.txt", None)],
        merges: vec![],
    };
    let main = head(&git, "main");
    gh.publish(&repo(), &main, "factory/trail-1", &[commit]).await.unwrap();
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
    let files = gh.files(&pr).await.unwrap();
    let a = ChangedFile {
        path: "a.txt".into(),
        status: FileStatus::Added,
        additions: 1,
        deletions: 0,
        previous_path: None,
    };
    let old = ChangedFile {
        path: "old.txt".into(),
        status: FileStatus::Removed,
        additions: 0,
        deletions: 3,
        previous_path: None,
    };
    assert_eq!(files, [a, old]);
    let id = gh.comment(&pr, "Looks right.").await.unwrap();
    assert_eq!(git.lock().unwrap().comments, [(1, "Looks right.".to_string())]);
    gh.edit_comment(&pr, id, "Looks right, still.").await.unwrap();
    assert_eq!(git.lock().unwrap().comments, [(1, "Looks right, still.".to_string())]);
    assert!(matches!(gh.edit_comment(&pr, 99, "gone").await, Err(trailhead_factory::Error::NotFound(_))));

    // The merge is pinned to the head the caller saw: an older one is refused as moved, and nothing merges.
    let tip = head(&git, "factory/trail-1");
    assert_eq!(gh.merge(&pr, "stale").await.unwrap(), MergeOutcome::HeadMoved);
    // A 405 is a conflict only when GitHub says the PR doesn't merge cleanly; otherwise its rules refused it.
    git.lock().unwrap().unmergeable = true;
    assert_eq!(gh.merge(&pr, &tip).await.unwrap(), MergeOutcome::Refused("Pull Request is not mergeable".into()));
    git.lock().unwrap().prs[0]["mergeable"] = json!(false);
    assert_eq!(gh.merge(&pr, &tip).await.unwrap(), MergeOutcome::Conflict);
    git.lock().unwrap().prs[0]["mergeable"] = json!(true);
    git.lock().unwrap().unmergeable = false;
    assert_eq!(gh.merge(&pr, &tip).await.unwrap(), MergeOutcome::Merged);
}

#[tokio::test]
async fn pr_state_tells_open_from_merged_from_closed() {
    let (git, url) = serve().await;
    let gh = GitHub::gateway(&url);
    let main = head(&git, "main");
    let mut prs = Vec::new();
    for n in 1..=3 {
        let branch = format!("factory/trail-{n}");
        let commit = Commit { message: "x".into(), changes: vec![change("a.txt", Some(b"a"))], merges: vec![] };
        gh.publish(&repo(), &main, &branch, &[commit]).await.unwrap();
        let new = NewPr { branch, base: "main".into(), title: "t".into(), body: "b".into() };
        prs.push(gh.open_pr(&repo(), new).await.unwrap());
    }
    assert_eq!(gh.pr_state(&prs[0]).await.unwrap(), PrState::Open { mergeable: None });

    gh.merge(&prs[1], &head(&git, "factory/trail-2")).await.unwrap();
    assert_eq!(gh.pr_state(&prs[1]).await.unwrap(), PrState::Merged { commit: Some("squash2".into()) });

    // Closed without merging: GitHub says `closed` and `merged: false`.
    git.lock().unwrap().prs[2]["state"] = json!("closed");
    assert_eq!(gh.pr_state(&prs[2]).await.unwrap(), PrState::Closed);

    let gone = PrRef { number: 99, ..prs[0].clone() };
    assert!(matches!(gh.pr_state(&gone).await, Err(Error::NotFound(_))));
}

#[tokio::test]
async fn mergeable_reads_githubs_verdict_and_publish_can_make_a_merge_commit() {
    let (git, url) = serve().await;
    let gh = GitHub::gateway(&url);
    let main = head(&git, "main");
    let commit = Commit { message: "x".into(), changes: vec![change("a.txt", Some(b"a"))], merges: vec![] };
    gh.publish(&repo(), &main, "factory/trail-1", &[commit]).await.unwrap();
    let new = NewPr { branch: "factory/trail-1".into(), base: "main".into(), title: "t".into(), body: "b".into() };
    let pr = gh.open_pr(&repo(), new).await.unwrap();

    // GitHub says nothing while it works the answer out.
    git.lock().unwrap().prs[0]["mergeable"] = Value::Null;
    assert_eq!(gh.pr_state(&pr).await.unwrap(), PrState::Open { mergeable: None });
    git.lock().unwrap().prs[0]["mergeable"] = json!(false);
    assert_eq!(gh.pr_state(&pr).await.unwrap(), PrState::Open { mergeable: Some(false) });
    git.lock().unwrap().prs[0]["mergeable"] = json!(true);
    assert_eq!(gh.pr_state(&pr).await.unwrap(), PrState::Open { mergeable: Some(true) });

    // A commit that merges main in has both parents.
    let branch = head(&git, "factory/trail-1");
    let merge = Commit {
        message: "Merge main".into(),
        changes: vec![change("a.txt", Some(b"resolved"))],
        merges: vec!["main-tip".into()],
    };
    git.lock().unwrap().commits.insert("main-tip".into(), json!({ "tree": { "sha": "t0" } }));
    gh.publish(&repo(), &branch, "factory/trail-1", &[merge]).await.unwrap();
    let g = git.lock().unwrap();
    assert_eq!(g.commits[&g.refs["factory/trail-1"]]["parents"], json!([branch, "main-tip"]));
}

#[tokio::test]
async fn a_refused_workflow_change_names_the_call_and_the_scope() {
    let (git, url) = serve().await;
    git.lock().unwrap().workflow_scope = false;
    let gh = GitHub::gateway(&url);
    let commit = Commit {
        message: "ci".into(),
        changes: vec![change(".github/workflows/ci.yml", Some(b"on: push\n"))],
        merges: vec![],
    };
    let main = head(&git, "main");
    let err = gh.publish(&repo(), &main, "factory/trail-105", &[commit]).await.unwrap_err().to_string();
    assert!(err.contains("POST /repos/lanesawyer/trailhead/git/refs"), "{err}");
    assert!(err.contains("`workflow` scope"), "{err}");
}

#[tokio::test]
async fn reads_are_retried_through_a_flaky_gateway() {
    let (git, url) = serve().await;
    let gh = GitHub::gateway(&url);
    let commit = Commit { message: "x".into(), changes: vec![change("a.txt", Some(b"a"))], merges: vec![] };
    let main = head(&git, "main");
    gh.publish(&repo(), &main, "factory/trail-1", &[commit]).await.unwrap();
    let new = NewPr { branch: "factory/trail-1".into(), base: "main".into(), title: "t".into(), body: "b".into() };
    let pr = gh.open_pr(&repo(), new).await.unwrap();

    git.lock().unwrap().files_failures = 2;
    let diff = gh.diff(&pr).await.unwrap();
    assert!(diff.contains("+++ b/a.txt"), "{diff}");
    assert_eq!(git.lock().unwrap().files_calls, 3);

    // Past the retries, the last answer comes back.
    git.lock().unwrap().files_failures = 10;
    let err = gh.diff(&pr).await.unwrap_err();
    assert!(err.to_string().contains("502"), "{err}");
}

#[tokio::test]
async fn a_transport_error_says_why() {
    let gh = GitHub::gateway("http://127.0.0.1:1");
    let pr = trailhead_factory::PrRef { repo: repo(), number: 1, url: String::new() };
    let err = gh.diff(&pr).await.unwrap_err().to_string();
    assert!(err.contains("error sending request") && err.matches(": ").count() > 2, "{err}");
}

#[tokio::test]
async fn a_branch_behind_main_is_updated_with_a_merge_commit() {
    let (git, url) = serve().await;
    let gh = GitHub::gateway(&url);
    let started = head(&git, "main");
    let work = Commit { message: "x".into(), changes: vec![change("a.txt", Some(b"a"))], merges: vec![] };
    gh.publish(&repo(), &started, "factory/trail-1", &[work]).await.unwrap();
    let new = NewPr { branch: "factory/trail-1".into(), base: "main".into(), title: "t".into(), body: "b".into() };
    let pr = gh.open_pr(&repo(), new).await.unwrap();
    assert!(!gh.behind(&pr).await.unwrap(), "cut from main's tip");

    let landed = Commit { message: "meanwhile".into(), changes: vec![change("b.txt", Some(b"b"))], merges: vec![] };
    gh.publish(&repo(), &started, "main", &[landed]).await.unwrap();
    assert!(gh.behind(&pr).await.unwrap());

    let before = head(&git, "factory/trail-1");
    assert_eq!(gh.update_branch(&pr).await.unwrap(), BranchUpdate::Updated);
    {
        let g = git.lock().unwrap();
        assert_eq!(g.commits[&g.refs["factory/trail-1"]]["parents"], json!([before, g.refs["main"]]));
    }
    assert!(!gh.behind(&pr).await.unwrap());

    // GitHub applies an update after its 202, so a second ask can find nothing left to merge: that's no conflict.
    git.lock().unwrap().unmergeable = true;
    assert_eq!(gh.update_branch(&pr).await.unwrap(), BranchUpdate::UpToDate);
    git.lock().unwrap().unmergeable = false;

    // Main moves again and the merge conflicts: GitHub answers 422.
    let again = Commit { message: "again".into(), changes: vec![change("a.txt", Some(b"c"))], merges: vec![] };
    let main = head(&git, "main");
    gh.publish(&repo(), &main, "main", &[again]).await.unwrap();
    git.lock().unwrap().unmergeable = true;
    assert_eq!(gh.update_branch(&pr).await.unwrap(), BranchUpdate::Conflict);
}

#[tokio::test]
async fn a_422_that_is_not_a_conflict_is_an_error() {
    let (git, url) = serve().await;
    let gh = GitHub::gateway(&url);
    let started = head(&git, "main");
    let work = Commit { message: "x".into(), changes: vec![change("a.txt", Some(b"a"))], merges: vec![] };
    gh.publish(&repo(), &started, "factory/trail-1", &[work]).await.unwrap();
    let new = NewPr { branch: "factory/trail-1".into(), base: "main".into(), title: "t".into(), body: "b".into() };
    let pr = gh.open_pr(&repo(), new).await.unwrap();
    let landed = Commit { message: "meanwhile".into(), changes: vec![change("b.txt", Some(b"b"))], merges: vec![] };
    gh.publish(&repo(), &started, "main", &[landed]).await.unwrap();

    // The head moved under the update: GitHub answers 422 too, and that must not cost a rework.
    git.lock().unwrap().update_refusal = Some("expected head sha didn't match current head ref.".into());
    let err = gh.update_branch(&pr).await.unwrap_err();
    assert!(matches!(&err, Error::Other(m) if m.contains("expected head sha")), "{err:?}");
}

#[tokio::test]
async fn a_verdict_carries_over_merges_of_the_base_but_not_other_commits() {
    let (git, url) = serve().await;
    let gh = GitHub::gateway(&url);
    let started = head(&git, "main");
    let work = Commit { message: "x".into(), changes: vec![change("a.txt", Some(b"a"))], merges: vec![] };
    gh.publish(&repo(), &started, "factory/trail-1", &[work]).await.unwrap();
    let new = NewPr { branch: "factory/trail-1".into(), base: "main".into(), title: "t".into(), body: "b".into() };
    let pr = gh.open_pr(&repo(), new).await.unwrap();
    let reviewed = head(&git, "factory/trail-1");
    assert!(gh.carries_verdict(&pr, &reviewed).await.unwrap(), "the head itself");

    // Two updates in a row, each bringing in a commit that landed on main meanwhile.
    for n in 0..2 {
        let main = head(&git, "main");
        let landed = Commit { message: "meanwhile".into(), changes: vec![change("b.txt", Some(b"b"))], merges: vec![] };
        gh.publish(&repo(), &main, "main", &[landed]).await.unwrap();
        assert_eq!(gh.update_branch(&pr).await.unwrap(), BranchUpdate::Updated, "{n}");
        assert!(gh.carries_verdict(&pr, &reviewed).await.unwrap(), "after {} updates", n + 1);
    }

    // A commit of the PR's own after the updates: unreviewed work.
    let tip = head(&git, "factory/trail-1");
    let more = Commit { message: "more".into(), changes: vec![change("c.txt", Some(b"c"))], merges: vec![] };
    gh.publish(&repo(), &tip, "factory/trail-1", &[more]).await.unwrap();
    assert!(!gh.carries_verdict(&pr, &reviewed).await.unwrap());
}

#[tokio::test]
async fn a_verdict_does_not_carry_when_the_reviewed_sha_is_gone() {
    let (git, url) = serve().await;
    let gh = GitHub::gateway(&url);
    let started = head(&git, "main");
    let work = Commit { message: "x".into(), changes: vec![change("a.txt", Some(b"a"))], merges: vec![] };
    gh.publish(&repo(), &started, "factory/trail-1", &[work]).await.unwrap();
    let new = NewPr { branch: "factory/trail-1".into(), base: "main".into(), title: "t".into(), body: "b".into() };
    let pr = gh.open_pr(&repo(), new).await.unwrap();

    // A force push and GC: the compare 404s, which means stale, not an error.
    let gone = "0123456789abcdef0123456789abcdef01234567";
    assert!(!gh.carries_verdict(&pr, gone).await.unwrap());
}
