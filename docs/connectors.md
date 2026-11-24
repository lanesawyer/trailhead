# Writing a connector

A connector is a small Rust program that reads one outside service and hands the Trailhead server **feeds**: each
feed's whole current set of records, keyed. It knows nothing of apps. An app's collection says
`from = "<connector>.<feed>"` in its `collections.toml`, and the server writes every set the feed hands over into
every such collection, adding, changing and removing what differs. The design is in
[Trailhead as a shell of apps](app-platform.md#connectors).

A connector lives in the apps repo as `connectors/<id>/`, a crate of the workspace. The path rules put
`connectors/**` at grade 4, so a person reads every one. It is the only code of an apps repo that opens the network,
and only to the hosts it declares. The kit it is written against is the `trailhead-connector` crate; the whole of a
small one is its example, `crates/trailhead-connector/examples/todo_list/`, which a new connector is copied from.

## The manifest

`connectors/<id>/connector.toml` beside `Cargo.toml`:

```toml
id = "github"                  # the directory's name: lowercase words joined by `-`
name = "GitHub"
every = "5m"                   # how often it runs: `30s`, `5m`, `2h`
timeout = "2m"                 # how long a run may take before it is stopped (default 2m)
hosts = ["api.github.com"]     # the only hosts it may reach, https only
secrets = ["token"]            # given on the server's machine, by name

[settings.repos]
kind = "list"                  # string, list (of strings), boolean or integer
label = "Repositories"
help = "owner/name, one per line"
required = true

[feeds.pull_requests]
fields = { title = "string", url = "url", draft = "boolean", updated = "timestamp", ticket = "string?" }
```

A feed's fields are typed as an app's collection's are (`string`, `integer`, `number`, `boolean`, `timestamp`, `day`,
`url`, `json`; `[]` for a list, `?` for optional), and a collection filled from the feed takes them.

## The program

Its `main` is one call. The kit reads what the server gave the run, calls the sync with a `Cx`, saves the state when
the sync succeeded, and exits: 0, or 1 after saying why on stderr in plain words, which the server keeps as the run's
error.

```rust
use trailhead_connector::{Cx, Error, FeedRecord};

fn main() {
    trailhead_connector::run(include_str!("../connector.toml"), sync)
}

async fn sync(cx: Cx) -> Result<(), Error> {
    let token = cx.secrets().get("token")?;
    let mut records = Vec::new();
    for repo in cx.settings().list("repos")?.unwrap_or_default() {
        let url = format!("https://api.github.com/repos/{repo}/pulls");
        for page in cx.http().pages(cx.http().get(&url).bearer(token), 10).await? {
            // map each pull request to a FeedRecord, keyed `owner/name#number`
        }
    }
    cx.hand_over("pull_requests", records).await?;
    Ok(())
}
```

- `cx.settings()` reads what a person set, typed by the manifest (`string`, `list`, `boolean`, `integer`).
- `cx.secrets().get(name)` is a declared secret's value.
- `cx.http()` is the only way out: declared hosts only (redirects too), https only, retries with backoff on a 429, a
  5xx or a dropped connection, `Request::if_none_match` and `Response::not_modified` for an ETag, `Http::pages` for
  `Link: rel="next"` paging.
- `cx.state()` is a small saved map (a cursor, an ETag), saved only when the run succeeds. Don't hold it across an
  `.await`.
- `cx.hand_over(feed, records)` checks the set against the feed's fields, then sends it to the server. Hand over the
  whole current set each time: what the set lacks is removed from the apps. A run that hands nothing over leaves the
  apps as they are, which is how a `304 Not Modified` is answered.

## Testing it with replay

A connector's tests run its sync against recorded answers, with no server, network or token, and get back what it
handed over:

```rust
use trailhead_connector::testing::Test;

#[tokio::test]
async fn hands_over_the_open_pull_requests() {
    let ran = Test::new(include_str!("../connector.toml"))
        .settings(serde_json::json!({ "repos": ["lanesawyer/trailhead"] }))
        .secret("token", "not-a-real-token")
        .replay_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/replay"))
        .run(sync)
        .await
        .unwrap();
    assert_eq!(ran.feed("pull_requests").len(), 2);
}
```

Each file in the replay directory is one answer, or a JSON array of them: `url`, and optionally `method` (GET),
`status` (200), `headers`, and `body` as text or `json` as a value. A request with no recorded answer fails, naming
what was recorded. To record real answers once, build the client with `Http::record_to(dir)`, and read what it saved
before committing it: it keeps only a few headers, but a body may hold personal data.

## Turning it on

Merging a connector doesn't start it. The server builds each `connectors/<id>/` crate with the window and keeps its
program with that build, so a run always uses the newest finished build. Then, on the server's machine:

```sh
trailhead connector show github                 # what it declares, and what it still needs
trailhead connector set github repos lanesawyer/trailhead
trailhead connector login github                # prompts for each secret, no echo
trailhead connector enable github
trailhead connector run github                  # once now; then every `every`
trailhead connector ls                          # on or off, the last run, what it needs
```

Secrets are kept in the credential store the factory's secrets are in (the OS keyring, else a `0600` file);
`TRAILHEAD_CONNECTOR_<ID>_<SECRET>` in the server's environment overrides one. The API only ever says whether a
secret is set. Whether it is on and its settings are in the server's settings table, so neither needs a restart.

Each run is a process of its own, acting as the agent `connector/<id>` with a token that may hand over that
connector's feeds and do nothing else. It gets `TRAILHEAD_URL`, `TRAILHEAD_TOKEN`, `TRAILHEAD_CONNECTOR_SETTINGS`
(JSON), `TRAILHEAD_CONNECTOR_STATE` (its state file, `connectors/<id>/state.json` in the server's data directory),
one `TRAILHEAD_CONNECTOR_SECRET_<NAME>` per secret and `PATH`, and nothing else of the server's environment. One run
of a connector at a time; it is stopped at its `timeout`, and when the server stops. After three failed runs in a row
the server raises a notification (`connector.failing`), and none again until a run succeeds, which resolves it.
