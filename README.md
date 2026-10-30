# Trailhead

A software factory built on a task engine that humans and agents share.

- **The task engine** owns the task data and the rules around it: projects, claims with leases, a ready queue, typed comments for handoffs, questions and review verdicts, and an event log where every write can be undone. It's headless: REST under `/v1` for apps people build, MCP at `/mcp` for agents without a shell, and the `trailhead` CLI.
- **The factory** turns a task with an approved spec into an agent run in a locked-down sandbox, then a pull request, an agent review and a human merge. Claims, questions, handoffs and spend are recorded on the task. Today it runs Claude Code in [Fly Sprites](https://sprites.dev) against GitHub.

Agents can learn the API from `GET /llms.txt` on any server, and the workflow from the portable skill in [skills/trailhead/SKILL.md](skills/trailhead/SKILL.md).

**Status:** the task engine (phases 1 to 3) is in daily use, and Trailhead's own work is tracked in it. The factory MVP was finished on 2026-10-28: real tasks go from a spec to a merged PR. Phase 5, making the factory run unattended, is under way, and a native factory UI comes after. See [docs/PLAN.md](docs/PLAN.md) for the architecture and roadmap, and [docs/factory/](docs/factory/README.md) for the factory's design.

## Install

```sh
cargo install trailhead --locked
```

Or skip the build with [cargo-binstall](https://github.com/cargo-bins/cargo-binstall) (`cargo binstall trailhead`), or download a Linux binary (x86_64 or arm64) from the [releases page](https://github.com/lanesawyer/trailhead/releases). Trailhead runs on Linux.

This installs the task engine. The CLI starts a local server in the background on first use.

```sh
trailhead project add DEMO "Demo"
trailhead add "Write the README" -p DEMO -P 2
trailhead ready -p DEMO
```

Agents act as themselves, never as you: `trailhead agent add claude` creates an agent and saves its token, and `trailhead --as claude ...` (or `agent = "claude"` in a `.trailhead.toml`) acts as it. For agents that speak MCP, `trailhead --as claude mcp` is a stdio bridge to the server.

## The factory

The factory isn't packaged yet. It runs from this repo with its dev setup: a Sprites token and a Claude token through [fnox](https://github.com/jdx/fnox), a `factory` agent, and the repo's settings in `factory.toml`. Then:

```sh
mise run factory                                               # build and run the control plane
trailhead --as-me add "..." -p TRAIL --parent TRAIL-102 -d "spec"   # hand it a task
```

It takes ready subtasks of the queue task, runs each in a pooled Sprite, opens a PR with an agent review, and leaves the merge to you. It pauses new runs when a Claude subscription's usage window is nearly full. [docs/factory/operating.md](docs/factory/operating.md) covers setup, watching runs and what to do with each outcome. Installing and running it without this repo (`trailhead factory init|start|status`) is TRAIL-114.

## Other machines

Trailhead runs as one server, and agents on other machines reach it through a tunnel to its loopback port (iroh's [dumbpipe](https://www.dumbpipe.dev), Tailscale, `ssh -L`) rather than by exposing it publicly. Give each machine's agent its own token:

```sh
# on the server's machine
trailhead agent add laptop --print-token
# on the other machine, with the tunnel at 127.0.0.1:7242
trailhead --url http://127.0.0.1:7242 login     # paste the token
trailhead --as laptop ready
```

To use the other machine as yourself, mint the token with `trailhead token add laptop` instead and drop `--as`. `login` makes that server the machine's default (in `config.toml` in the config directory); `--url` and `TRAILHEAD_URL` still override it. If the tunnel drops, the CLI says so instead of starting an empty server of its own.

## Integrations

- [OpenClaw](docs/integrations/openclaw.md): mount Trailhead's MCP server and agent skill.

## Development

```sh
cargo test
cargo clippy --all-targets
cargo fmt --all
```

`cargo test` covers the engine and the whole factory lifecycle on in-memory fakes. Tests that touch real services are `#[ignore]`d and run as mise tasks (`mise run test-live-sprites`, `mise run test-live-daemon`), with secrets from Bitwarden through fnox. Work on Trailhead is tracked in Trailhead, in the `TRAIL` project; [AGENTS.md](AGENTS.md) has the conventions for agents working here.

CI (`.github/workflows/ci.yml`) runs `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings` and `cargo test --locked` on every pull request and push to `main`, and checks that the published crates package (`cargo package -p trailhead-core -p trailhead-server -p trailhead`). The factory's crates aren't published yet.

The docs in `docs/` are also a book, built with mdBook and published to the [docs site](https://lanesawyer.github.io/trailhead/) by `.github/workflows/docs.yml`, which builds them on pull requests that touch them and deploys from `main`. `mise run docs` builds them locally and `mdbook serve` previews them. A new page has to be listed in `docs/SUMMARY.md`; the build fails on a listed page that doesn't exist.

## Releasing

Releases are cut from GitHub Actions; there's nothing to do locally.

1. Run the **Release** workflow on `main` (Actions → Release → Run workflow). Leave the version empty to take the next version [git-cliff](https://git-cliff.org) calculates from the conventional commits since the last tag (`feat` and breaking changes bump the minor version while we're pre-1.0, everything else the patch), or enter one to override it.
2. It sets every crate to that version, regenerates [CHANGELOG.md](CHANGELOG.md) from `cliff.toml`, commits `chore(release): vX.Y.Z` to `main`, and pushes the tag.
3. It then builds `trailhead` Linux binaries (x86_64 and arm64) and a static `trailhead-daemon` (`trailhead-daemon-vX.Y.Z-x86_64-unknown-linux-musl`, with a `.sha256` beside it, which the factory's control plane downloads and verifies), publishes `trailhead-core`, `trailhead-server` and `trailhead` to crates.io, and creates a GitHub release with that version's changelog and the binaries.

Publishing needs a crates.io API token in the `CARGO_REGISTRY_TOKEN` repository secret, and the Release workflow needs to be able to push to `main`.

## License

[AGPL-3.0](LICENSE)
