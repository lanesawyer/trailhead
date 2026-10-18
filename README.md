# Trailhead

A fast, headless task engine that humans and agents share. It owns the task data and the rules around it: projects, claims with leases, a ready queue, typed comments for handoffs and questions, and an event log where every write can be undone.

There is no app. Every interface is a client: REST under `/v1` for apps people build, MCP at `/mcp` for agents without a shell, and the `trailhead` CLI.

Agents can learn the API from `GET /llms.txt` on any server, and the workflow from the portable skill in [skills/trailhead/SKILL.md](skills/trailhead/SKILL.md).

**Status:** Phases 1 (core) and 2 (server, CLI, MCP) are done; Phase 3 (clients: webhooks, agent docs, integrations) is under way. See [docs/PLAN.md](docs/PLAN.md) for the architecture and roadmap.

## Install

```sh
cargo install trailhead --locked
```

Or skip the build with [cargo-binstall](https://github.com/cargo-bins/cargo-binstall) (`cargo binstall trailhead`), or download a binary for Linux, macOS or Windows from the [releases page](https://github.com/lanesawyer/trailhead/releases).

## Other machines

Trailhead runs as one server, and agents on other machines reach it through a tunnel to its loopback port (iroh's [dumbpipe](https://www.dumbpipe.dev), Tailscale, `ssh -L`) rather than by exposing it publicly. Give each machine's agent its own token:

```sh
# on the server's machine
trailhead agent add laptop --print-token
# on the other machine, with the tunnel at 127.0.0.1:7242
trailhead --url http://127.0.0.1:7242 login     # paste the token
trailhead --url http://127.0.0.1:7242 --as laptop ready
```

To use the other machine as yourself, mint the token with `trailhead token add laptop` instead and drop `--as`. Set `TRAILHEAD_URL` to skip `--url`. If the tunnel drops, the CLI says so instead of starting an empty server of its own.

## Integrations

- [OpenClaw](docs/integrations/openclaw.md): mount Trailhead's MCP server and agent skill.

## Development

```sh
cargo test
cargo clippy --all-targets
cargo fmt --all
```

CI (`.github/workflows/ci.yml`) runs `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings` and `cargo test --locked` on every pull request and push to `main`, and checks that every crate packages with `cargo package --workspace --locked`.

## Releasing

Releases are cut from GitHub Actions; there's nothing to do locally.

1. Run the **Release** workflow on `main` (Actions → Release → Run workflow). Leave the version empty to take the next version [git-cliff](https://git-cliff.org) calculates from the conventional commits since the last tag (`feat` and breaking changes bump the minor version while we're pre-1.0, everything else the patch), or enter one to override it.
2. It sets every crate to that version, regenerates [CHANGELOG.md](CHANGELOG.md) from `cliff.toml`, commits `chore(release): vX.Y.Z` to `main`, and pushes the tag.
3. It then builds `trailhead` binaries for Linux and macOS (x86_64 and arm64) and Windows (x86_64), publishes `trailhead-core`, `trailhead-server` and `trailhead` to crates.io, and creates a GitHub release with that version's changelog and the binaries.

Publishing needs a crates.io API token in the `CARGO_REGISTRY_TOKEN` repository secret, and the Release workflow needs to be able to push to `main`.

## License

[AGPL-3.0](LICENSE)
