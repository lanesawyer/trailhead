# Sandboxes: Sprites, connectors, environment and egress

> **Design note.** It records decisions and intent; where it says something is built or not, check the code if it matters.

The first `Compute` implementation is Fly.io Sprites. A sandbox holds no raw credentials, reaches only declared egress, refreshes its own environment on demand, and may install dependencies, with the gate at merge rather than at install.

Checked against the Sprites docs on 2026-10-25: [Connectors](https://docs.sprites.dev/concepts/connectors/), [connector catalog](https://fly.io/sprites/ecosystem/), [networking](https://docs.sprites.dev/concepts/networking/), [policy API](https://sprites.dev/api/sprites/policies).

## Credentials through Sprites Connectors

Decided: model access and GitHub go through connectors. The gateway injects the stored credential and forwards the call; the sandbox never sees the token.

- **Model access.** The first-class Anthropic connector (bring your own key) forwards only the Messages API (including token counting and Message Batches) and `/v1/models`; files, skills and admin paths are rejected. The agent targets the connector's gateway base URL. OpenAI, OpenRouter, Fireworks and others have their own connectors, so the provider stays swappable.
- **GitHub.** The connector proxies GitHub's REST API with the authorizing user's permissions and granted scopes. Each sandbox works on its own branch and opens its own PR; it never pushes to a branch another sandbox is working on.
- **Getting code in and out.** The gateway forwards REST paths only, so there's no `git clone` or `git push`. Agents just commit locally; the daemon publishes through the Git Data API (blobs, tree, commit, ref), then opens the PR. The commit is parented on the commit the checkout was at when the run started, not on the base branch's current head: commits carry whole files, so laying them on a newer head silently undid whatever had landed since (TRAIL-161, PR #7). An existing branch must still be at that commit, or publishing fails and the task parks instead of overwriting it. Tested 2026-10-26 (M0): this works end to end at 0.4–1.3 s per call. The commit's author is the OAuth user unless set explicitly, and it's unsigned. Getting the code in: `tarball/{ref}` through the gateway returns a 302 with the Location stripped, and `codeload.github.com` isn't on the allowlist, so tarballs don't work. Decided: the Sprite downloads the repo in one step during bootstrap. For a public repo, bootstrap egress adds `github.com` and `codeload.github.com` and the Sprite runs a plain `git clone`. No credential is needed; Trailhead took 0.4 s for a full clone and 0.3 s for a tarball. The allowlist closes again before agent work. For a private repo, the plan is a `custom_api` connector to `https://github.com` with a fine-grained, single-repo token, so `git clone` (and possibly `git push`) runs through the gateway; untested and deferred (TRAIL-96), since the MVP uses public repos. A last resort that works today is a recursive `git/trees` call plus one `git/blobs` call per file (3.5 s for Trailhead), but that's one API call per file against GitHub's 5,000/hour limit.
- **Access policy.** Default-deny, granted by sandbox label or name prefix. Endpoint allow and block lists exist (API only for now), with exact paths or trailing-`*` prefixes. Policy belongs to the connector, not the sandbox, so author and reviewer roles get separate GitHub connectors with different scopes.
- **Kill switch.** Removing a sandbox's label cuts its model and GitHub access at the gateway. Labels aren't in the OpenAPI spec, but Fly's Go and Python SDKs send them: `labels: [..]` on create, `PUT /v1/sprites/{name}` with `labels` to replace them and `clear_labels: true` to empty them. They're plain strings, so `trailhead-compute-sprites` stores `key=value` pairs and connector policies match on those exact strings (`sprite_labels: ["role=worker"]`). Confirmed against the live API: values such as `repo=lanesawyer/trailhead` are accepted, and `PUT` replaces the set. Still to check: that a label change takes effect at the gateway, which needs a connector restricted by label (TRAIL-95).
- **Managed by API.** Connectors are fully API-managed (`/v1/oauth/connections`: provision, API-key and custom-API connections, access policy, per-connection usage and balance), so the control plane can own their setup. fly-mcp has no connector tools.
- **Media types.** The gateway doesn't pass GitHub's `Accept` media types through: asking for a PR as `application/vnd.github.diff` returns its JSON. So the code host builds diffs from `GET /pulls/{n}/files`.
- **Scopes in practice.** The dashboard's GitHub OAuth connector gets classic `repo,read:org` (every repo the user can reach) and `allow_all` access. Before real use, narrow it with `allowed_endpoints` (`/repos/<owner>/<repo>/*`) and a label or name prefix, or use a fine-grained token or GitHub App through a `custom_api` connector.
- **Discovery.** Inside a sandbox, `GET https://api.sprites.dev/v1/gateway/list` (no auth header; the gateway identifies the Sprite) returns each ready connector's `gateway_base_url` (`https://api.sprites.dev/v1/gateway/<provider>/<connection_id>`). The only egress domain the gateway needs is `api.sprites.dev`. Every Sprite ships a `sprite-api-gateway` skill for Claude Code, Codex, Cursor and Gemini.
- **The one credential a sandbox holds** is its short-lived, project-scoped Trailhead sub-agent token; see [state.md](state.md).

**Subscription mode** (TRAIL-101, opt-in, for Lane's own use): `trailhead-factory run --claude-oauth-token …` (or `CLAUDE_CODE_OAUTH_TOKEN`, from `claude setup-token`) runs Claude Code in sandboxes on a Claude subscription instead of a model connector. The token travels in the daemon's `0600` secrets file (kept, so a cold wake can restart the daemon), never in run requests or on a command line. Claude Code gets it as `CLAUDE_CODE_OAUTH_TOKEN`, with any API key or base URL stripped, and agent egress adds `api.anthropic.com`. The trade-offs: it breaks "sandboxes hold no raw credentials", since an injected agent could leak a token good for the whole subscription; spend is priced at API rates as a proxy, since a subscription has rate limits rather than per-call prices; parallel sandboxes share the plan's limits; and whether the plan's terms allow unattended use is for Lane to confirm. The connector stays the design beyond Lane's own use.

Assumed to work, not yet tested (TRAIL-97): Claude Code's exact setup (likely `ANTHROPIC_BASE_URL` set to the gateway URL plus a placeholder key), and whether the gateway's 120-second timeout, documented for several connectors, cuts off long streaming calls. The connector takes an API key; a Claude subscription doesn't plug into it.

## Standard checks

Decided; the stack is built (TRAIL-334), the checks still run as written. Repos describe their stack and a fixed set of named checks. mise is an implementation detail the user never touches.

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
- **Generated, not hand-written.** Built (TRAIL-334): the daemon turns `[stack]` tools into a mise config and installs them when it prepares a worker's checkout, before `setup`. The checks stay plain commands from `factory.toml`, not generated mise tasks, since they'd add nothing; they run through `mise exec` so they see the pinned tools.
- **How it installs.** In the prepare step, with install egress open: install mise to `~/.local/bin` if it's missing (the stock image doesn't ship it); write the `[stack]` tools to mise's global `conf.d` (`~/.config/mise/conf.d/trailhead-factory.toml`); `mise trust`; log `mise bootstrap plan`; run `mise bootstrap --only packages,tools,task`. The allowlist keeps out the parts that fight the sandbox (firewall, accounts, user, systemd units, services, dotfiles, remote), since the factory owns egress and the daemon's user, and a part a later mise adds isn't picked up by surprise.
- **A repo with its own `mise.toml`** keeps it, and its `[bootstrap.packages]` and `bootstrap` task run too. The factory's tools sit in the global config, which has the lowest precedence, so the repo's own pins win; the daemon logs which `[stack]` tools the repo overrode. The `[stack]` tools install first, from outside the checkout, and a failure there fails the prepare; the repo's own config is then bootstrapped best effort, so a dev-only tool that can't install in the sandbox (Trailhead's own `lima` or `fnox`) is logged as a warning rather than failing the run. A repo with a `mise.toml` and no `[stack]` is bootstrapped from its own file alone, also best effort. After prepare, mise runs with `MISE_OFFLINE=1`, so checks don't stall resolving versions with egress closed. Reviewers prepare without `setup` but get the same tools.
- **The agent's environment.** The daemon starts the agent with `mise env --json` applied, so its `cargo`, `node` and `python` are the pinned ones, and runs every check through `mise exec`. A checkout with neither a `[stack]` nor a mise config gets the stock environment.
- **Policy refers to names.** Gates say which checks must pass (`require = ["fmt", "lint", "test"]`), so policy reads the same across stacks.
- **Onboarding.** An agent can inspect a new repo and propose `[stack]` and `[checks]` for a human to confirm.

This lives in the per-repo factory file, with agent profiles ([agents.md](agents.md)) and gate policy ([review.md](review.md)).

## Bootstrap and checkpoints

Decided 2026-10-27, replacing a fresh Sprite per task (decided 2026-10-26): workers run in a lazy per-repo pool of warm Sprites, each restored to its own checkpoint before every task. A cold build of Trailhead took about 6½ minutes in the first real run (TRAIL-103), too long to pay on every task.

- **Why not seed from a shared checkpoint.** Tested in M0: a checkpoint can't seed a different Sprite. Checkpoint IDs are a per-Sprite sequence (`v1`, `v2`, …). Restoring a Sprite to its own checkpoint works and takes about 1.5 s.
- **Lazy pool.** Worker Sprites are `<repo>-w<n>`, labelled `pool=worker` and `state=idle|busy`, plus `warm=<checkpoint>`. A task takes an idle one, or creates one when none is idle; the tick's room check keeps the pool within `max_sandboxes`. A new one pays the cold build once.
- **Per task.** Mark busy (with `task` and `role` labels), open install egress, restore the warm checkpoint, prepare (fetch and reset onto the base branch, then the `setup` check, an incremental build), checkpoint the result as the new warm point, narrow egress, run.
- **Isolation.** When the task ends, the Sprite is restored to its warm checkpoint, which wipes everything the task wrote, then marked idle with no `role` label (so no connector access) and no egress. Idle Sprites pause and cost only storage. Parked tasks keep their Sprite busy.
- **Reconcile.** Pooled Sprites are never destroyed at boot; one an interrupted run left busy goes back to idle.
- **Broken Sprites.** A pooled Sprite that keeps failing to release (TRAIL-135: Sprites answered `503 Storage not ready` for 25 minutes) is retried with backoff from 30 s up to 5 min. After five failures or ten minutes, the factory gives up on it. It destroys the Sprite if the API allows, and otherwise labels it `state=broken` with no `role`, then destroys it on a later reconcile once the API recovers. A broken Sprite doesn't count toward `max_sandboxes`, so the next task gets a new one. If even the labels can't be changed, the control plane leaves it out in memory until it restarts. The failure counts are in memory too.
- **Reviewers** get a fresh Sprite each and skip `setup`, since they only read the diff.
- **Tools.** A pooled Sprite installs its `[stack]` once and keeps it in the warm checkpoint; later tasks only re-run the idempotent bootstrap. Sprites carry a `stack` label, a hash of the `[stack]` tools next to `warm`; an idle Sprite whose label differs from the repo's current stack is destroyed rather than reused, so a changed `[stack]` reinstalls on a fresh one.
- **Bootstrap.** The stock image (Ubuntu 26.04, glibc 2.43) ships rustup/cargo, node, python, go, `gh`, git and Claude Code, but not mise. Uploading and starting the daemon and cloning takes about 11 s.
- **Later:** deleting Sprites idle for N days, and a shared build cache in object storage for big pools, behind the same `Compute` seam.

## Egress policy

Sprites egress is a DNS-based allowlist set from outside through the API. Changes are enforced about a second after the API call returns: in the M3 live test, a `git fetch` sent right after narrowing still reached GitHub. So each run request lists the hosts the narrowing dropped, and the daemon waits until they stop resolving before the agent starts. It's read-only inside the sandbox, and changes apply live and flush existing connections. An empty rule list means unrestricted, so deny-by-default is an explicit `{"domain": "*", "action": "deny"}` plus allows. Denied names return REFUSED. A raw IP is reachable only after a lookup of an allowed domain resolved to it, and private IPs are always blocked. The `{"address", "protocol", "port"}` rules in fly-mcp's schema are rejected by the Sprite ("invalid policy config") and absent from the OpenAPI spec, so don't rely on them. `{"include": "defaults"}` is a broad development preset (GitHub, npm, PyPI, Docker Hub, AI APIs) and too wide for agent work.

| Phase | Allowed domains |
| --- | --- |
| Install | Iroh relay and discovery, connector gateway, the repo's package registries (`[egress] install`); `github.com` and `codeload.github.com` at bootstrap for public repos; and exactly the hosts of the tools being installed (below) |
| Agent work | Iroh relay and discovery, connector gateway |
| Install requested mid-task | Hosts the daemon asks for, opened by the control plane on top of the install set and closed after. Built for the tool installs: after the clone the daemon works out the tool list offline and the control plane opens those hosts, logging each with the tool that asked (`opening install egress host=nodejs.org tool=node`) |

**Tool hosts.** The control plane can't see the checkout, so it doesn't guess. Before `prepare`, it asks the daemon which hosts the install needs (`install_hosts`). The daemon refreshes the checkout, collects the tools (the `[stack]` and the `[tools]` of the checkout's own mise config), maps them to hosts with one table (`hosts_for` in `crates/trailhead-daemon/src/mise.rs`, keyed by mise tool or backend) and replies with each host and the tool that needs it. The control plane opens those on top of the install set, logs them, runs `prepare`, and confirms them blocked again before the agent starts. mise's installer (`mise.run` and its GitHub release) is included only when mise is missing; its version lists (`mise.jdx.dev`, `mise-versions.jdx.dev`) whenever there is a tool to install. No tools, no extra hosts.

| Tool or backend | Hosts |
| --- | --- |
| `node` | `nodejs.org` |
| `rust` | `static.rust-lang.org` |
| `go` | `go.dev`, `dl.google.com` |
| `python` | GitHub, PyPI |
| `pnpm`, `github:`, `aqua:`, `ubi:` | GitHub: `github.com`, `api.github.com`, `objects.githubusercontent.com`, `release-assets.githubusercontent.com` |
| `npm:` | `registry.npmjs.org` |
| `pipx:`, `pypi:` | `pypi.org`, `files.pythonhosted.org` |

A tool the table doesn't know takes an entry in factory.toml: `[egress.tools]` maps a tool to its download hosts (`zig = ["ziglang.org"]`). The control plane sends the map in the prepare request, the daemon uses it for `[stack]` and `mise.toml` tools alike (an entry wins over the table), and the hosts open only while that tool installs, logged with the tool that asked. A `[stack]` tool in neither the table nor `[egress.tools]` fails the prepare, naming the tool and pointing at `[egress.tools]`. A tool from the repo's own `mise.toml` that is in neither is skipped with a warning, the same best-effort rule as its install. The hosts of `[bootstrap.packages]` (apt and the like) aren't mapped; they go in `[egress] install`.

Decided: registry egress only during install phases. The risk isn't secrets, since the sandbox holds none; it's a malicious or typosquatted package whose install script acts with the sandbox's connector identity. Mitigations:

- **New dependency raises the grade.** Adding or upgrading a dependency is at least grade 3 by default, configurable per repo ([review.md](review.md#grades)).
- **Supply-chain checks.** An audit tool per ecosystem (for example `cargo-deny` or `osv-scanner`) runs with the other checks.
- **Narrow connector scopes.** An author sandbox's GitHub connector reaches only its own repo.

## Docker

Decided 2026-10-28 (TRAIL-119): `trailhead-compute-docker` runs sandboxes as containers on a Docker engine (or Podman's Docker API), for agents on your own machine. `factory.toml` picks it with `[compute] kind = "docker"`, optionally with `image`, `network` and `runtime` (an OCI runtime such as `runsc`). The `trailhead-factory` binary needs the `docker` feature for it.

- **The engine on a dev machine.** `mise run docker-up` starts rootless Docker in a lima VM with no host mounts, so nothing is installed system-wide and a container escape lands in the VM, not beside the control plane and the Trailhead database. mise installs lima and the Docker CLI; on Linux, qemu and access to `/dev/kvm` are the one prerequisite it can't install (on macOS lima uses the built-in Virtualization framework). `docker_host` in `mise.toml` is the VM's socket, and the Docker tasks set `DOCKER_HOST` to it. colima was tried first and dropped: it forces lima's `user-v2` network on Linux, and qemu 10.2 aborted in that path (`net_fill_rstate: Assertion 'size == 0'`) as soon as an image build pulled packages. lima's own `docker` template uses slirp and doesn't hit it.
- **Identity and labels.** A sandbox's id is its container name, which survives a restore. Docker labels can't change after creation, so labels and egress live in one JSON state file per sandbox under `<data dir>/factory/docker`; the container carries only `dev.trailhead.sandbox=<name>`, which `list` filters on (`docker ps --filter label=dev.trailhead.sandbox`).
- **Checkpoints.** A `docker commit` (paused) to `trailhead-checkpoint:<name>-<millis>`, labelled `dev.trailhead.checkpoint-of=<name>`. Restoring removes the container and recreates it from that image with the same name, labels and egress. Unlike a Sprite checkpoint, an image could seed other containers (TRAIL-93). Images are removed when the sandbox is destroyed; cleaning up earlier and the ~125-layer limit on chained commits are TRAIL-176.
- **Programs.** A small shell supervisor (`supervise.sh`) is uploaded at create and is the container's main process, under Docker's init, with `restart: unless-stopped`. `start` writes the files as the container's user (each beside its target, then renamed over it), writes a run script, and has the supervisor (re)start the program, restarting it whenever it exits. After a restart or restore, the supervisor starts every program again. A program that exits within 3 seconds fails `start` with its output, as on Sprites.
- **Wake** starts a stopped container; containers don't pause.
- **Egress, for now, is all or nothing.** No egress disconnects the container from every network. Any allowlist connects it to the sandbox network (`trailhead-sandboxes`) unfiltered, and logs a warning. The filtering proxy and a direct iroh path to the control plane are TRAIL-174.
- **Credentials.** There are no connectors, so the daemon finds no GitHub connector and can't publish, and Claude Code only works in subscription mode. The credential proxy that keeps "sandboxes hold no keys" is TRAIL-175.
- **The image.** `mise run docker-image` builds `trailhead-sandbox:latest` from `crates/trailhead-compute-docker/image`: Ubuntu 26.04 with git, a C toolchain, rustup and Claude Code, as the user `trailhead`. The daemon lives in `/home/trailhead/.trailhead` and checks out into `/home/trailhead/work`. It's the same Ubuntu as the stock Sprite, so a daemon built on Lane's machine runs in both.
- **Isolation.** Containers share the engine's kernel. On a dev machine that's the lima VM's; on a bare Docker host, prefer gVisor (`runtime = "runsc"`) or rootless Docker (TRAIL-177).
- **Tests.** `mise run test-live-docker` runs the lifecycle against the engine with a small `alpine` container.

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
