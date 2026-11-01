# Plugin seams

> **Design note.** It records decisions and intent; where it says something is built or not, check the code if it matters.

Decided: the factory core is agnostic to compute, agent harness, model provider, task tracker and code host. Each is a trait with one implementation per vendor. Iroh and the standard checks stay fixed; they're the factory's own machinery, not vendor choices.

| Seam | Runs in | Implementation today | Later |
| --- | --- | --- | --- |
| `Compute` | Control plane | Fly.io Sprites (`trailhead-compute-sprites`), Docker (`trailhead-compute-docker`) | Other microVM hosts, local VMs |
| `Harness` | Daemon, in the sandbox | Claude Code (`trailhead-harness-claude-code`) | Codex, an own agent loop |
| `ModelProvider` | Both | Anthropic (`trailhead-provider-anthropic`) | OpenAI, OpenRouter, local models ([local-models.md](local-models.md)); TRAIL-125 |
| `Tracker` | Both | Trailhead: in-process over `trailhead-core` in the control plane (`CoreTracker`), REST from the daemon (`RestTracker`) | Jira and others |
| `CodeHost` | Daemon, and the control plane for merges | GitHub's REST API (`trailhead-codehost-github`) | GitLab, Forgejo |

## The traits

The code in `crates/trailhead-factory` is authoritative; this is what each trait is for and the decisions behind its shape.

- **`Compute`** (`compute.rs`): create, list by label, checkpoint, restore, start a program, wake, set egress, set labels, destroy. `set_labels` is the kill switch: removing the `role` label cuts connector access. There's no "create from a checkpoint", because a Sprite checkpoint can't seed another Sprite, so a pooled sandbox is restored to its own warm checkpoint between tasks ([sandbox.md](sandbox.md)).
- **`Harness`** (`harness.rs`): `run` and `resume` with a `RunContext` (the task, workdir, model, endpoint, step limit and, for reviewers, the diff), streaming `RunEvent`s: steps, tool calls, model usage, questions and subscription usage-window readings (`Quota`, [budgets.md](budgets.md)). It returns changes, a verdict, a question or a failure. The control plane never calls it directly; the daemon hosts it.
- **`ModelProvider`** (`provider.rs`): resolves aliases (`sonnet`, `opus`) to model ids, gives the endpoint, and prices usage. The provider usage API (`spend`) waits for the daily cap, which isn't built.
- **`Tracker`** (`tracker.rs`): handles bound to one identity. `sub_agent(name)` acts as a sandbox's sub-agent, which is how the control plane parks or requeues a task the sandbox holds. Besides the claim loop (`ready`, `claim`, `renew`, `release`, `request_review`, `record_verdict`, `ask`, `note`, `finish`), it has `show` and `in_flight` for reconcile and `create_sub_agent`/`revoke_sub_agent` for sandbox identities.
- **`CodeHost`** (`codehost.rs`): `publish` commits on top of the commit the work started from (never the base branch's current head: TRAIL-161), `open_pr`, `diff` for the reviewer, `comment` and `merge`.
- **`Daemon`** (`daemon.rs`), the control plane's side of the daemon contract: `prepare`, `run` with an event stream, `cancel`, and `shutdown`, which cancels every run in flight. `RemoteDaemon` speaks it over iroh ([agents.md](agents.md)); the testkit's `LocalDaemon` runs the same `Runner` in-process.
- **Run metadata** (`RunUpdate`, stored under `factory` in the task's metadata): `attempt`, `sandbox`, `host`, `profile`, `pr`, `review_rounds`, `conflict_reworks`, `merging` (the head the merger is merging, so a merge whose close failed stays the merger's), `reviewed`, `spend` and `failure` (why the last attempt failed, passed to the next as `TaskSpec.previous_failure`). Reviewer sandboxes and sub-agents are named per round (`trail-42-a1-r1`), so a review interrupted by a crash can be retried under a fresh identity.

## Rules

- **Vendor features map onto neutral concepts.** Sprites connectors become a sandbox's `role` label plus an `Egress` list; another compute host might grant the same role with injected env or a proxy. Checkpoints are required of every `Compute`, since the pool and rollback depend on them.
- **Static plugins.** Implementations are crates compiled into the `trailhead-factory` binary, which wires them up in `trailhead-factory-host`. No dynamic loading or hot swapping. Compute has two implementations (Sprites and Docker), picked by `[factory.compute]`. Harnesses and providers are picked by name: a profile's `adapter` and `provider` look up a `HarnessKind` (`trailhead_daemon::plugins::harnesses`) and a `ProviderKind` (`providers()` in the host) in a `Registry`, and an unknown name fails at config load. Today each registry holds one entry, and a repo's worker and reviewer profiles must agree, as must all repos in one process. A harness takes its credentials from `DaemonSecrets.harness_secrets`, a name-to-value map only that harness reads, and declares the egress it needs for them (Claude Code in subscription mode: `api.anthropic.com`). Cargo features may keep the binary slim.
- **Fakes for testing.** `trailhead-testkit` has an in-memory `Compute`, a scripted `Harness`, and fake `Tracker`, `CodeHost` and `ModelProvider`, so the whole lifecycle runs in `cargo test` with no network or spend. No external emulator.

## Crates

| Crate | Contents |
| --- | --- |
| `trailhead-core` | Task engine: schema, claims, events, undo |
| `trailhead-server` | REST, MCP, SSE, webhooks |
| `trailhead` | CLI; gains `trailhead factory` commands later (TRAIL-116) |
| `trailhead-factory` | Factory domain types, daemon protocol, the five traits, the `[factory]` settings |
| `trailhead-control` | Control plane: run loop, sandbox pool, budget metering, usage-window pauses, reviews, reconcile; `CoreTracker` |
| `trailhead-factory-host` | The `trailhead-factory` binary: runs the control plane beside the server, sharing its database, and forwards the daemons' API tunnel to it. Moving the control plane into the server is TRAIL-164 |
| `trailhead-daemon` | In-sandbox daemon: harness host, checks, publishing; the iroh transport (`RemoteDaemon`, `DaemonProtocol`) and `RestTracker` |
| `trailhead-compute-sprites` | `Compute` for Fly.io Sprites (own thin API wrapper) |
| `trailhead-compute-docker` | `Compute` for Docker or Podman's Docker API, through bollard; behind the host's `docker` feature |
| `trailhead-harness-claude-code` | `Harness` wrapping the Claude Code CLI |
| `trailhead-provider-anthropic` | `ModelProvider` for Anthropic |
| `trailhead-codehost-github` | `CodeHost` for GitHub through the Sprites connector, with the Git Data API |
| `trailhead-testkit` | Fakes for all five seams and an in-process daemon |

The factory crates are `publish = false`. CI and the release workflow package and publish only `trailhead-core`, `trailhead-server` and `trailhead`.
