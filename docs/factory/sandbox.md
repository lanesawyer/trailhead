# Sandboxes: Sprites, connectors, environment and egress

The first `Compute` implementation is Fly.io Sprites. A sandbox holds no raw credentials, reaches only declared egress, refreshes its own environment on demand, and may install dependencies, with the gate at merge rather than at install.

Checked against the Sprites docs on 2026-10-25: [Connectors](https://docs.sprites.dev/concepts/connectors/), [connector catalog](https://fly.io/sprites/ecosystem/), [networking](https://docs.sprites.dev/concepts/networking/), [policy API](https://sprites.dev/api/sprites/policies).

## Credentials through Sprites Connectors

Decided: model access and GitHub go through connectors. The gateway injects the stored credential and forwards the call; the sandbox never sees the token.

- **Model access.** The first-class Anthropic connector (bring your own key) forwards only the Messages API (including token counting and Message Batches) and `/v1/models`; files, skills and admin paths are rejected. The agent targets the connector's gateway base URL. OpenAI, OpenRouter, Fireworks and others have their own connectors, so the provider stays swappable.
- **GitHub.** The connector proxies GitHub's REST API with the authorizing user's permissions and granted scopes. Each sandbox works on its own branch and opens its own PR; it never pushes to a branch another sandbox is working on.
- **Getting code in and out.** The gateway forwards REST paths only, so there's no `git clone` or `git push`. Agents just commit locally; the daemon publishes through the Git Data API (blobs, tree, commit, ref), then opens the PR. Tested 2026-10-26 (M0): this works end to end at 0.4–1.3 s per call. The commit's author is the OAuth user unless set explicitly, and it's unsigned. Getting the code in: `tarball/{ref}` through the gateway returns a 302 with the Location stripped, and `codeload.github.com` isn't on the allowlist, so tarballs don't work. Decided: the Sprite downloads the repo in one step during bootstrap. For a public repo, bootstrap egress adds `github.com` and `codeload.github.com` and the Sprite runs a plain `git clone`. No credential is needed; Trailhead took 0.4 s for a full clone and 0.3 s for a tarball. The allowlist closes again before agent work. For a private repo, the plan is a `custom_api` connector to `https://github.com` with a fine-grained, single-repo token, so `git clone` (and possibly `git push`) runs through the gateway; untested and deferred (TRAIL-96), since the MVP uses public repos. A last resort that works today is a recursive `git/trees` call plus one `git/blobs` call per file (3.5 s for Trailhead), but that's one API call per file against GitHub's 5,000/hour limit.
- **Access policy.** Default-deny, granted by sandbox label or name prefix. Endpoint allow and block lists exist (API only for now), with exact paths or trailing-`*` prefixes. Policy belongs to the connector, not the sandbox, so author and reviewer roles get separate GitHub connectors with different scopes.
- **Kill switch.** Removing a sandbox's label cuts its model and GitHub access at the gateway. Labels aren't in the OpenAPI spec, but Fly's Go and Python SDKs send them: `labels: [..]` on create, `PUT /v1/sprites/{name}` with `labels` to replace them and `clear_labels: true` to empty them. They're plain strings, so `trailhead-compute-sprites` stores `key=value` pairs and connector policies match on those exact strings (`sprite_labels: ["role=worker"]`). Confirmed against the live API: values such as `repo=lanesawyer/trailhead` are accepted, and `PUT` replaces the set. Still to check: that a label change takes effect at the gateway, which needs a connector restricted by label (TRAIL-95).
- **Managed by API.** Connectors are fully API-managed (`/v1/oauth/connections`: provision, API-key and custom-API connections, access policy, per-connection usage and balance), so the control plane can own their setup. fly-mcp has no connector tools.
- **Media types.** The gateway doesn't pass GitHub's `Accept` media types through: asking for a PR as `application/vnd.github.diff` returns its JSON. So the code host builds diffs from `GET /pulls/{n}/files`.
- **Scopes in practice.** The dashboard's GitHub OAuth connector gets classic `repo,read:org` (every repo the user can reach) and `allow_all` access. Before real use, narrow it with `allowed_endpoints` (`/repos/<owner>/<repo>/*`) and a label or name prefix, or use a fine-grained token or GitHub App through a `custom_api` connector.
- **Discovery.** Inside a sandbox, `GET https://api.sprites.dev/v1/gateway/list` (no auth header; the gateway identifies the Sprite) returns each ready connector's `gateway_base_url` (`https://api.sprites.dev/v1/gateway/<provider>/<connection_id>`). The only egress domain the gateway needs is `api.sprites.dev`. Every Sprite ships a `sprite-api-gateway` skill for Claude Code, Codex, Cursor and Gemini.
- **The one credential a sandbox holds** is its short-lived, project-scoped Trailhead sub-agent token; see [state.md](state.md).

**Subscription mode** (TRAIL-101, opt-in, for Lane's own use): `trailhead-factory run --claude-oauth-token …` (or `CLAUDE_CODE_OAUTH_TOKEN`, from `claude setup-token`) runs Claude Code in sandboxes on a Claude subscription instead of a model connector. The token travels in the daemon's `0600` secrets file, which the daemon reads and deletes, never in run requests or on a command line. Claude Code gets it as `CLAUDE_CODE_OAUTH_TOKEN`, with any API key or base URL stripped, and agent egress adds `api.anthropic.com`. The trade-offs: it breaks "sandboxes hold no raw credentials", since an injected agent could leak a token good for the whole subscription; spend is priced at API rates as a proxy, since a subscription has rate limits rather than per-call prices; parallel sandboxes share the plan's limits; and whether the plan's terms allow unattended use is for Lane to confirm. The connector stays the design beyond Lane's own use.

Assumed to work, not yet tested (TRAIL-97): Claude Code's exact setup (likely `ANTHROPIC_BASE_URL` set to the gateway URL plus a placeholder key), and whether the gateway's 120-second timeout, documented for several connectors, cuts off long streaming calls. The connector takes an API key; a Claude subscription doesn't plug into it.

## Standard checks

Decided: repos describe their stack and a fixed set of named checks. mise is an implementation detail the user never touches.

```toml
[stack]
tools = { rust = "stable", node = "lts" }

[checks]
fmt       = "cargo fmt --check"
fmt_fix   = "cargo fmt"
lint      = "cargo clippy -- -D warnings"
typecheck = "cargo check"
test      = "cargo nextest run"
build     = "cargo build"
```

- **Fixed names, optional entries.** `setup`, `fmt`, `fmt_fix`, `lint`, `typecheck`, `test`, `build`. A repo leaves out what it doesn't have.
- **Contract.** Exit code 0 is a pass. Structured output (JUnit, SARIF) can come later.
- **Generated, not hand-written.** The factory turns `[stack]` and `[checks]` into mise tools and tasks when it bootstraps a sandbox. A repo with its own `mise.toml` keeps it, with the factory's tasks merged in.
- **Policy refers to names.** Gates say which checks must pass (`require = ["fmt", "lint", "test"]`), so policy reads the same across stacks.
- **Onboarding.** An agent can inspect a new repo and propose `[stack]` and `[checks]` for a human to confirm.

This lives in the per-repo factory file, with agent profiles ([agents.md](agents.md)) and gate policy ([review.md](review.md)).

## Bootstrap and checkpoints

Decided 2026-10-26, replacing the per-repo warm checkpoint: each task Sprite bootstraps from the stock image. Revisit if bootstrap proves too slow.

- **Why.** Tested in M0: a checkpoint can't seed a different Sprite. Checkpoint IDs are a per-Sprite sequence (`v1`, `v2`, …); restoring another Sprite's ID fails with "checkpoint not found", and the API (OpenAPI v0.1.12) has no fork, clone or create-from-checkpoint.
- **Bootstrap.** The stock image (Ubuntu 26.04, glibc 2.43) already ships rustup/cargo, node, python, go, `gh`, git and Claude Code, but not mise. The daemon clones the repo and runs the `setup` check under install-phase egress. In the M3 live test, uploading and starting the daemon, connecting over the relay, cloning Trailhead and running setup took about 11 s. mise and dependency installs are still to come (TRAIL-93).
- **Checkpoints stay local.** Per-step rollback restores the Sprite's own checkpoints. A checkpoint captures only the writable overlay, not processes. Checkpointing took about 70 ms and an in-place restore about 1.5 s, which restarts the container.
- **If cold builds are too slow,** a per-repo build cache (`target/`, `~/.cargo`) goes in object storage, keyed by a hash of the lockfiles and `mise.toml`.
- **Rejected:** one long-lived base Sprite per repo, since restoring it serves one task at a time.

## Egress policy

Sprites egress is a DNS-based allowlist set from outside through the API. Changes are enforced about a second after the API call returns: in the M3 live test, a `git fetch` sent right after narrowing still reached GitHub. So each run request lists the hosts the narrowing dropped, and the daemon waits until they stop resolving before the agent starts. It's read-only inside the sandbox, and changes apply live and flush existing connections. An empty rule list means unrestricted, so deny-by-default is an explicit `{"domain": "*", "action": "deny"}` plus allows. Denied names return REFUSED. A raw IP is reachable only after a lookup of an allowed domain resolved to it, and private IPs are always blocked. The `{"address", "protocol", "port"}` rules in fly-mcp's schema are rejected by the Sprite ("invalid policy config") and absent from the OpenAPI spec, so don't rely on them. `{"include": "defaults"}` is a broad development preset (GitHub, npm, PyPI, Docker Hub, AI APIs) and too wide for agent work.

| Phase | Allowed domains |
| --- | --- |
| Install | Iroh relay and discovery, connector gateway, the repo's package registries and mise tool sources; `github.com` and `codeload.github.com` at bootstrap for public repos |
| Agent work | Iroh relay and discovery, connector gateway |
| Install requested mid-task | Install set, opened by the control plane on the daemon's request and closed after; each request logged |

Decided: registry egress only during install phases. The risk isn't secrets, since the sandbox holds none; it's a malicious or typosquatted package whose install script acts with the sandbox's connector identity. Mitigations:

- **New dependency raises the tier.** Adding or upgrading a dependency is at least T2 by default, configurable per repo.
- **Supply-chain checks.** An audit tool per ecosystem (for example `cargo-deny` or `osv-scanner`) runs with the other checks.
- **Narrow connector scopes.** An author sandbox's GitHub connector reaches only its own repo.

## Keeping a sandbox awake

A Sprite pauses about 30 seconds after its last inbound activity (an exec, a request to its URL), and pausing drops every open connection. Services don't count as activity, and neither does the daemon's outbound iroh connection: in M4's live test, a run's connection dropped about a minute in. So while a prepare or run is in flight, the daemon holds the Sprite with the Tasks API on `/.sprite/api.sock`: a 5-minute expiry refreshed every minute, so a crashed daemon frees the Sprite on its own. When a request ends, the hold is cut to 90 seconds rather than deleted. A Sprite that's been idle past its window pauses the moment its last hold goes, which froze one between a worker run and its review. A paused Sprite isn't woken by an iroh dial, so the control plane calls `Compute::wake` before redialing a daemon it isn't connected to. On Sprites that's an exec that takes a 2-minute hold; an exec alone isn't enough, because an idle Sprite pauses again as soon as the command ends. The daemon notices the resume (its wall clock jumps), re-resolves iroh's hosts and reconnects. Verified live: a review ran 1.8 s after waking a Sprite that had been paused for a minute.

## Iroh in a locked-down sandbox

Iroh runs in every phase: Trailhead's `dumbpipe` tunnel is the sandbox's only channel to the control plane and Trailhead.

Tested 2026-10-26 (M0) with dumbpipe 0.40.1. The Sprite was NATed at 10.0.0.1; the peer was Lane's machine behind home NAT, 46 ms ICMP RTT away.

| Policy | Path | Steady RTT on one connection |
| --- | --- | --- |
| None | Direct UDP | 47 ms |
| Deny `*`; allow `iroh.link`, `*.iroh.link`, `*.iroh-svc.com` | n0 relay (`usw1-1.relay.n0.iroh.link`) | 51.6 ms p50, 52.1 ms p90 |
| As above, plus an allowed DNS name for the peer's IP, resolved once in the Sprite | Relay, then upgraded to direct | 47 ms |

Decided: relay-only is fine for the MVP. On n0's public relay it costs about 5 ms, so a self-hosted relay isn't needed yet. Agent-work egress for iroh is `iroh.link`, `*.iroh.link` and `*.iroh-svc.com` (the relay hosts CNAME there). Connecting, including the first round trip, takes about 270 ms.

Leaning: the optional direct path is a DNS name for the control plane's public IP (its own domain, or sslip-style), added to the Sprite's allowlist and resolved by the daemon before it dials. Without the lookup, the IP stays blocked and the connection stays on the relay. Untested: what happens when that record's TTL expires mid-connection.

Narrowing egress resets which raw IPs a Sprite lets through, and iroh redials relays by IPs it resolved earlier. So after a lockdown, every relay reconnect was refused and the run's connection died about 30 seconds later. That happened in about half of M5's live runs. The daemon now resolves iroh's relay and discovery hosts again through the system resolver after each lockdown, and every minute after that, then tells iroh the network changed.

`dumbpipe connect-tcp` opens a new iroh connection for every TCP connection, which costs about 2 RTT per short request (105–112 ms here). The daemon should hold one long-lived iroh connection and multiplex streams on it.
