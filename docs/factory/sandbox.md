# Sandboxes: Sprites, connectors, environment and egress

The first `Compute` implementation is Fly.io Sprites. A sandbox holds no raw credentials, reaches only declared egress, refreshes its own environment on demand, and may install dependencies, with the gate at merge rather than at install.

Checked against the Sprites docs on 2026-10-25: [Connectors](https://docs.sprites.dev/concepts/connectors/), [connector catalog](https://fly.io/sprites/ecosystem/), [networking](https://docs.sprites.dev/concepts/networking/), [policy API](https://sprites.dev/api/sprites/policies).

## Credentials through Sprites Connectors

Decided: model access and GitHub go through connectors. The gateway injects the stored credential and forwards the call; the sandbox never sees the token.

- **Model access.** The first-class Anthropic connector (bring your own key) forwards only the Messages API (including token counting and Message Batches) and `/v1/models`; files, skills and admin paths are rejected. The agent targets the connector's gateway base URL. OpenAI, OpenRouter, Fireworks and others have their own connectors, so the provider stays swappable.
- **GitHub.** The connector proxies GitHub's REST API with the authorizing user's permissions and granted scopes. Each sandbox works on its own branch and opens its own PR; it never pushes to a branch another sandbox is working on.
- **Getting code in and out.** The docs don't mention `git clone` or `git push` through the gateway, and it forwards REST paths, so plan on REST. The repo arrives via the warm checkpoint (refreshed with a tarball fetch), and the daemon publishes commits through the Git Data API (blobs, tree, commit, ref), then opens the PR. Agents just commit locally.
- **Access policy.** Default-deny, granted by sandbox label or name prefix. Endpoint allow and block lists exist (API only for now), with exact paths or trailing-`*` prefixes. Policy belongs to the connector, not the sandbox, so author and reviewer roles get separate GitHub connectors with different scopes.
- **Kill switch.** Removing a sandbox's label cuts its model and GitHub access at the gateway.
- **Discovery.** Inside a sandbox, `GET /v1/gateway/list` returns each ready connector's `gateway_base_url`. Every Sprite ships a `sprite-api-gateway` skill for Claude Code, Codex, Cursor and Gemini.
- **The one credential a sandbox holds** is its short-lived, project-scoped Trailhead sub-agent token; see [state.md](state.md).

To test hands-on: Claude Code's exact setup (likely `ANTHROPIC_BASE_URL` set to the gateway URL plus a placeholder key), and whether the gateway's 120-second timeout, documented for several connectors, cuts off long streaming calls.

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
- **Generated, not hand-written.** The factory turns `[stack]` and `[checks]` into mise tools and tasks when it builds the warm checkpoint. A repo with its own `mise.toml` keeps it, with the factory's tasks merged in.
- **Policy refers to names.** Gates say which checks must pass (`require = ["fmt", "lint", "test"]`), so policy reads the same across stacks.
- **Onboarding.** An agent can inspect a new repo and propose `[stack]` and `[checks]` for a human to confirm.

This lives in the per-repo factory file, with agent profiles ([agents.md](agents.md)) and gate policy ([review.md](review.md)).

## Warm checkpoint lifecycle

Decided: rebuilt on demand, in the working sandbox.

- A task starts from the newest warm checkpoint for its repo, then runs `mise install` and the dependency install. If nothing changed, these are near no-ops.
- If the lockfiles or `mise.toml` changed, the post-install state becomes the new warm checkpoint, tagged with a hash of those files.
- To verify: whether a checkpoint can seed a different Sprite, or only restore the one it came from. If only the same Sprite, each repo needs a long-lived base Sprite to clone from.

## Egress policy

Sprites egress is a DNS-based allowlist set from outside through the API. It's read-only inside the sandbox, and changes apply live.

| Phase | Allowed domains |
| --- | --- |
| Install | Iroh relay and discovery, connector gateway, the repo's package registries and mise tool sources |
| Agent work | Iroh relay and discovery, connector gateway |
| Install requested mid-task | Install set, opened by the control plane on the daemon's request and closed after; each request logged |

Decided: registry egress only during install phases. The risk isn't secrets, since the sandbox holds none; it's a malicious or typosquatted package whose install script acts with the sandbox's connector identity. Mitigations:

- **New dependency raises the tier.** Adding or upgrading a dependency is at least T2 by default, configurable per repo.
- **Supply-chain checks.** An audit tool per ecosystem (for example `cargo-deny` or `osv-scanner`) runs with the other checks.
- **Narrow connector scopes.** An author sandbox's GitHub connector reaches only its own repo.

## Iroh in a locked-down sandbox

Iroh runs in every phase: Trailhead's `dumbpipe` tunnel is the sandbox's only channel to the control plane and Trailhead. The docs say a Sprite can't bypass the allowlist by dialing an IP directly, so connections will likely go through the relay instead of hole-punching. Direct connections are strongly preferred; relay-only is acceptable. To test hands-on, with a self-hosted relay in the same Fly region as the fallback.
