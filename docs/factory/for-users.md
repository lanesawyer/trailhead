# The factory for users: install and run without this repo

> **Proposal, for Lane to decide (TRAIL-114).** Nothing here is built. It answers one question: what stands between today's code and a stranger running the factory on their own GitHub repo, and in what order should it be built. Facts are from the code at `88432ed2` (2026-11-13). What was read in code is marked **Verified**; what is reasoned from it but not run is marked **Inferred**. The epic's text (a month old) is the starting point, not the source.

## Where the epic stands

The epic's target flow was `cargo install trailhead`, `trailhead factory init`, `trailhead factory login sprites|claude`, `trailhead factory start|stop|status|logs`, `trailhead factory queue FOO-12`. Much of it exists now in a different shape:

| Epic step | Today |
| --- | --- |
| `cargo install trailhead` | Installs v0.1.0 from crates.io (2026-10-16), the task engine only, from before the factory was folded in. No release contains the factory. |
| `factory init` | Not built (TRAIL-118). Everything it would do is done by hand. |
| `factory login` | Built (TRAIL-117): `sprites`, `claude`, `github`, `github-writer`, saved in the OS keyring or a `0600` file. |
| `factory start\|stop` | Replaced: the server runs the control plane when the user config lists `[factory] repos` (TRAIL-164); `trailhead server stop` drains it. TRAIL-112 was cancelled for this. |
| `factory status\|logs` | Built (TRAIL-109). |
| `factory queue FOO-12` | Replaced by `trailhead edit FOO-12 --parent <queue task>` plus a grade (TRAIL-110). |

So the commands are mostly there. What is missing is a way to get a build that has them, a first-run setup, and a handful of assumptions that only hold on Lane's machine.

## The walk-through today

A stranger on a fresh x86_64 Linux machine, with a GitHub repo `alice/widget`, a Claude subscription and Rust installed, tries to get one task through. Each step says what they must do and where it breaks.

1. **Install.** `cargo install trailhead --locked` gives 0.1.0, which has no `trailhead factory` at all. **Verified** (`git show v0.1.0`: only `trailhead-core`, `trailhead-server` and `trailhead` existed). The README says "The factory isn't packaged yet. It runs from this repo with its dev setup" and points at `mise run factory`. The working route is a source build: `cargo install --git https://github.com/lanesawyer/trailhead trailhead --locked --features docker`. **Inferred**: `cargo install --git` picks a workspace member by name, but nobody has tried it on a clean machine, and I could not check which system packages the build needs.
2. **The daemon.** On first start the control plane needs a `trailhead-daemon` to upload. `daemon_binary::resolve` tries `TRAILHEAD_DAEMON`, then a local `trailhead-daemon` beside `trailhead` or on PATH, then the release asset `trailhead-daemon-v0.1.0-x86_64-unknown-linux-musl`. v0.1.0 has no such asset (the `daemon` job in `release.yml` came later), so without a local daemon the start fails with "no trailhead-daemon next to the control plane or on PATH, and no release asset ... install one with `cargo install --locked --path crates/trailhead-daemon`", a path in this repo. **Verified.** The user has to `cargo install --git ... trailhead-daemon` too. That works only because their host is x86_64 Linux with a glibc no newer than the sandbox's.
3. **A project and a queue task.** `trailhead --as-me project add WID "Widget"`, then a queue task (`trailhead --as-me add "Factory queue" -p WID`). Nothing says to do this except `operating.md`, which is written for this repo (`TRAIL-102`).
4. **The repo file.** `.trailhead.toml` at the repo root with a top-level `project` and a `[factory]` table, written by hand. `[factory.profiles]` has no default: both a `worker` and a `reviewer` profile are required, each with `role`, `adapter`, `provider`, `model` and a `budget` of `usd`, `minutes` and `steps` (`config.rs`). Leaving them out gives `missing field profiles` or `no profile named worker`. The only full example is this repo's own file, which is tuned to a Rust workspace with Bevy (`trim-cargo --cap 12884901888`, `ui-lint`). `owner` and `name` come from `origin`, which must be a `github.com` URL. **Verified.**
5. **Register the repo.** Add `[factory] repos = ["~/code/widget"]` to `~/.config/trailhead/config.toml` by hand; no command writes it (only `factory enable`/`disable` touch that table). **Verified.**
6. **Pick a compute.** `[factory.compute]` defaults to Sprites. **Verified** (`ComputeConfig::default`).
   - **Sprites:** a Fly.io Sprites account, `trailhead factory login sprites`, and a GitHub connector made by hand in the Sprites dashboard, because a worker publishes through whatever `github` connection `api.sprites.dev/v1/gateway/list` returns (`trailhead-daemon/src/connectors.rs`). Nothing in Trailhead creates or checks that connector, and the dashboard's OAuth connector has classic `repo` scope on every repo the user can reach (`sandbox.md`). Private repos can't be cloned (TRAIL-96).
   - **Docker:** a Docker engine, a build with `--features docker` (without it a repo file with `kind = "docker"` is a configuration error and the server refuses to start), `trailhead factory login github-writer`, and the image `trailhead-sandbox:latest`, which exists only as `crates/trailhead-compute-docker/image/Dockerfile` in this repo (`mise run docker-image`). Nothing pulls or builds it. **Verified.** Lane's isolation (rootless Docker in a lima VM with no host mounts) is a `mise run docker-up` recipe; a stranger on plain Docker shares the host's kernel with the sandboxes (TRAIL-177).
7. **Model access.** `claude setup-token`, then `trailhead factory login claude`. This is the only model path that works through the server. **Verified**: the server builds the control plane's options from defaults (`start` parses `["serve"]`), and `--model-endpoint` has no environment variable, so it is always `https://api.anthropic.com`; without the OAuth token Claude Code gets that URL and the placeholder key `sprites-connector` on Sprites (whose target names no model endpoint), and on Docker a gateway URL with no Anthropic connection behind it (TRAIL-487; the API-key login was dropped in TRAIL-420).
8. **GitHub for the control plane.** `trailhead factory login github`, which checks only that the token can read `/user` and the repo's pulls. But in `shadow` and `on` merge modes the control plane comments on PRs, updates branches and merges with that same token (`lib.rs` builds the code host from `github_token`). A token made to the letter of the docs ("read access to the repo's pull requests is enough") fails there. **Verified** in code; Lane never hits it because `fnox.toml` maps `GITHUB_TOKEN` and `GITHUB_WRITER_TOKEN` to the same Bitwarden secret.
9. **Make the factory a reviewer.** After the server has created the `factory` agent, `trailhead --as-me project edit WID --reviewers factory`; otherwise the merger can't approve and falls back to shadow behaviour. Easy to miss, since the agent only appears once the server has started the control plane.
10. **CI.** The default gates require `ci` for every grade, and a commit with no check runs or statuses is `Pending` (`summarize_ci`). A repo without GitHub Actions waits forever with "waiting for CI" unless the user writes `[factory.gates]` without `ci`. **Verified.**
11. **Start.** Any `trailhead` command starts the server in the background. A configuration error stops it and the CLI prints why. An outside failure (no Sprites token, Docker unreachable, a daemon it can't find) leaves the server up and the control plane retrying; the reason shows only in `trailhead factory status` (`control: starting: ...`) and `factory.log`. **Inferred**: the background server inherits the environment of whichever shell first ran `trailhead`, so `DOCKER_HOST` and any secrets set as variables must be in that shell. The keyring avoids this for secrets, not for `DOCKER_HOST`.
12. **Queue and watch.** `trailhead --as-me add "..." -p WID --parent WID-1 --grade 2 -d "spec"`, then `trailhead factory status` and `trailhead factory logs WID-2 --follow`. These work as documented. **Verified** in code, not run.

On a Mac or an arm64 Linux box it breaks earlier. The CLI uses `std::os::unix` without a `cfg` in `creds.rs`, `client.rs` and `cmd/server.rs`, so Windows doesn't build; macOS may build but no CI job or release covers it (TRAIL-131). On any non-x86_64-Linux host, step 2's `cargo install` leaves a daemon for the wrong platform, and `resolve` prefers it to the release asset (the TRAIL-354 note).

## The gaps

| Group | What exists | What is missing | Size | Task |
| --- | --- | --- | --- | --- |
| Install and binaries | `release.yml` builds `trailhead` for x86_64 and aarch64 Linux (gnu), the daemon for x86_64 musl, and `cargo binstall` metadata points at the archives. | A release that contains the factory. The `crates-io` job publishes `trailhead-server` and `trailhead`, which now depend (optionally) on `publish = false` crates; `server-control-plane.md` records that `cargo package` refuses this, and CI's package check leaves both out. `github-release` needs `crates-io`, so the next release run would stop before any GitHub release or daemon asset exists. The release binary is built without `docker`. | Medium, gated on a decision | None; publishing deferred by Lane 2026-11-07 |
| First-run setup | `factory check` reports tasks, compute and endpoint; `login` saves secrets. | `factory init`: a starter `.trailhead.toml` with both profiles and checks for the repo's stack, the `repos` entry in the user config, the project, the queue task, the factory as reviewer, gates that match whether the repo has CI. | Medium | TRAIL-118 |
| Model credentials | Subscription mode: `login claude`, token in the sandbox's `0600` secrets file, egress to `api.anthropic.com`. | An API-key path that works through the server, and a stated position on subscription terms for unattended use (`sandbox.md` leaves it to "the maintainer to confirm"). | Large for API keys; none for subscription only | TRAIL-487 (gateway carries model credentials) |
| Code host credentials | `login github` (read check), `login github-writer` (push check), the Docker gateway's GitHub and git connections. | `login github` doesn't check what the merge mode needs; the docs say read-only is enough. On Sprites, the GitHub connector is manual and undocumented for users. PRs and reviews come from the user's own account. | Small (checks and docs); medium for Sprites connector setup | TRAIL-129 (GitHub App) |
| Compute credentials | `login sprites`, checked against the API. Docker needs none. | Nothing for Docker. | None | |
| Daemon and platforms | Release asset for x86_64 musl, checksum-verified and cached; `--daemon` override. | A check that the daemon matches each target's platform (Sprites: x86_64 Linux; Docker: the engine's architecture), a release asset for aarch64 musl, and no preference for a local daemon that can't run. No version check beyond the ALPN `trailhead/daemon/0`, so a git build with an unbumped version could download an older release daemon (**Inferred**). | Medium | TRAIL-354 (cancelled; the problem stands) |
| Config discovery | The CLI and `check`/`status`/`logs`/`login` find `.trailhead.toml` upwards from the working directory; the server reads `repos` from the user config. | Only the write side: `init` adding the repo to `repos`. Discovery itself is done. | Small | TRAIL-118 |
| Sandbox image and stack tools | mise installs `[stack]` tools in the sandbox (TRAIL-334); the Dockerfile is Ubuntu 26.04 with git, a C toolchain, rustup and Claude Code. | A way to get the image without this repo: built by the control plane from an embedded Dockerfile, or published to a registry. rustup in the base image is a Trailhead-specific choice; mise may make it unneeded (not checked). | Small to medium | None |
| Host platform | Linux, systemd scoping for the background server. | macOS and Windows. | Large | TRAIL-131 |
| Repo assumptions | Path rules, gates, merge mode, all per repo. | Defaults that suit a stranger's repo: `ci` required by default, merge mode `off` by default (fine), grade rules empty by default (fine). | Small | TRAIL-118 |
| Docs | `operating.md` (assumes this repo), `getting-started.md` (engine only, says the factory needs `--path crates/trailhead`). | A user's install page. `operating.md` line on Docker still says "Until TRAIL-174 and TRAIL-175 land ... can't publish", which is out of date. The README's factory section points at `mise run factory`. | Small | None |
| Per-project settings | Everything per repo in `.trailhead.toml`. | Settings in Trailhead itself. Not needed for one repo. | Large | TRAIL-157 |

## Decisions for Lane

**1. Which compute is the first supported path.**
- Sprites: no local engine, but a Fly account, a hand-made GitHub connector with broad scope, public repos only, and the open Sprites items (TRAIL-95 label kill switch untested, TRAIL-96 private repos, TRAIL-97 gateway timeout).
- Docker: needs a local engine and the image, but no third-party account, private repos work through the gateway's git connection, credentials stay in the gateway, and it is what this repo runs on (`kind = "docker"`) with a live end-to-end test (`mise run test-live-docker-factory`).
- **Recommendation: Docker first**, with Sprites working but documented as "bring your own connector". The scope decision of 2026-10-29 (Sprites first) predates the gateway; Docker now has fewer outside dependencies. The cost is isolation: say plainly that plain Docker shares the host kernel and recommend gVisor or a VM.

**2. Which platforms for the control plane host.**
- Linux x86_64 only; Linux x86_64 and aarch64; or macOS too.
- **Recommendation: Linux x86_64 and aarch64.** The `trailhead` binaries for both already exist; aarch64 needs only the daemon platform check and an aarch64 musl daemon asset (Docker on an arm64 host runs arm64 containers). macOS waits for TRAIL-131.

**3. GitHub token or GitHub App.**
- Tokens: works today, PRs come from the user's account, the user makes one or two fine-grained tokens.
- App: PRs from a bot, scoped install, no personal token; a new auth flow and a key to store (TRAIL-129).
- **Recommendation: tokens for the first release.** Fix the read/write mismatch: either one token with write for both secrets (simple, honest about what the control plane does), or keep two and have `login github` check write when `[factory.merge] mode` isn't `off`. I lean to keeping two and checking, since `mode = "off"` is the default and a read-only token is then enough.

**4. What `factory init` does.**
- Write `.trailhead.toml` only, or also register the repo, create the project and queue task, create the `factory` agent and add it as reviewer.
- **Recommendation: all of it, each step printed and skipped if already done.** Every step is local and reversible, the person runs it, and steps 3, 5 and 9 of the walk-through are exactly where a stranger gets lost. The server already creates the agent unasked ("listing repos is the person's consent"), so creating it at init is no new power. It should write `[factory.merge] mode = "shadow"` so the first PRs show what the merger would do.

**5. What "supported" means for model auth.**
- Subscription via `claude setup-token` only; API key only; both.
- **Recommendation: subscription only for the first release**, since it is the only path that works through the server today, and say what it costs: the token sits in each sandbox, and whether the plan allows unattended use is the user's to check. API keys through the gateway come with TRAIL-487.

**6. How a stranger gets the binaries.** This touches publishing, which I make no change proposal for; the options and what each needs:
- crates.io: needs the factory crates published (or folded into published ones), the route `server-control-plane.md` deferred.
- GitHub release binaries plus `cargo binstall`: needs a release that gets past the `crates-io` job, or a GitHub release that doesn't wait on it. That is a change to the release workflow's job graph, so it is Lane's call.
- `cargo install --git ... --tag vX.Y.Z`: needs nothing new except a tag whose daemon asset exists, or a local daemon install, and a Rust toolchain on the user's machine.
- **Question for Lane:** which of these is the first user's route. The first two both need a release run that completes, which today means deciding about crates.io first.

**7. Defaults for a repo with no CI.** Require `ci` only when the repo has workflows (init decides), or make a `ci` gate with nothing reported pass after a timeout. **Recommendation:** init decides; the gate logic stays strict.

## Proposed order of work

Each is one PR or a short stack. "Factory" means it can be queued; "person" means Lane or an interactive session, usually because it touches `.github/**`, release or a decision.

| # | Task | Delivers | Depends on | Who |
| --- | --- | --- | --- | --- |
| 1 | Docs truth pass | Fix the stale Docker paragraph in `operating.md`; state the `github` token's real needs per merge mode; README factory section points at this page. | | Factory, grade 1 |
| 2 | `login github` checks for the merge mode | Push access checked when `mode` isn't `off`; a warning at control plane start when the token can't write in `shadow`/`on`. | Decision 3 | Factory, grade 2 |
| 3 | Daemon platform check | Each target reports its platform (Sprites fixed x86_64 Linux, Docker from the engine's `info`); a local daemon is used only if its ELF header matches, else the release asset for that platform; a clear error otherwise. Reopens TRAIL-354's scope. | | Factory, grade 3 (crosses crates) |
| 4 | aarch64 musl daemon asset | `release.yml` builds `aarch64-unknown-linux-musl`; `daemon_binary` names assets per target. | 3, decision 2 | Person (`.github/**`) |
| 5 | Sandbox image without the repo | The Docker compute builds `trailhead-sandbox:<version>` from an embedded Dockerfile when it's missing, and, if mise covers it, drops rustup from the base. | Decision 1 | Factory, grade 2 |
| 6 | Docker in the default build | `docker` becomes a default feature of `trailhead`, or the release binary builds with it. Revisits TRAIL-541's choice. | Decision 1 | Person (release, `Cargo.toml` features) |
| 7 | `trailhead factory init` | Writes `.trailhead.toml` from a template (profiles, stack and checks guessed from the repo, gates by CI, merge `shadow`), adds the repo to `repos`, creates project, queue task and agent, adds the reviewer. Idempotent. | 2, decision 4 | Factory drafts, grade 3 (CLI surface) |
| 8 | `factory check` as a doctor | Checks every prerequisite the walk-through needs (secrets, image, daemon platform, reviewer, CI, the Sprites GitHub connector) and prints the fix for each. | 3, 5, 7 | Factory, grade 2 |
| 9 | A release with the factory | A tagged release whose assets include the daemon for each supported platform and a `trailhead` with Docker. | 4, 6, decision 6 | Person |
| 10 | User install page | `docs/factory/install.md`: install, init, login, queue, watch, for a repo that isn't Trailhead. | 7, 8, 9 | Factory, grade 1 |
| 11 | Dry run | A fresh Linux VM with no Rust, mise or fnox, a new GitHub account's private repo with and without CI, following only the install page, one task to a merged PR. Every stumble becomes a task. | All above | Person |

Steps 1, 2, 3 and 5 don't depend on any decision except 1 and can start now. Step 11 is the acceptance test for TRAIL-114.

## Out of scope for the first release

- macOS and Windows hosts (TRAIL-131).
- A GitHub App (TRAIL-129) and code hosts other than GitHub.
- Model API keys and other providers; subscription only.
- Sprites as a supported first path, private repos on Sprites (TRAIL-96), and several targets at once ([targets.md](targets.md)).
- Factory settings stored in Trailhead (TRAIL-157).
- A service unit for the server (TRAIL-112 was cancelled; the CLI starts it).
- Lookout packaging and remote access to the factory.
- Changes to what is published on crates.io.
