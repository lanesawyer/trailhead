# Agents: daemon, profiles and context

A small daemon in every sandbox owns a typed contract with the control plane; the coding agent is a `Harness` plugin behind it. Profiles decide which harness, provider and model each role uses. The daemon's name is TBD (`factoryd` was a placeholder; see [naming.md](naming.md)).

## Harness boundary

Proposal: contract + adapters, starting with one CLI-agent adapter.

| Option | What it means | Gains | Costs |
| --- | --- | --- | --- |
| Wrap CLI agents | Run Claude Code, Codex, etc. as subprocesses | Best agents on day one; inherit their improvements | Each has its own config and output format; harder to checkpoint mid-run |
| Own agent loop | A Rust agent loop | Full control of tools, budgets, per-tool-call checkpoints | You maintain a coding agent |
| Contract + adapters | The daemon speaks one protocol; adapters wrap CLI agents or an own loop | Swap agents per task type; one event stream | One more layer to design |
| Claude Managed Agents | Anthropic runs the loop; tool calls execute inside a Sprite ([guide](https://docs.sprites.dev/integrations/claude-managed-agents/)) | Less to build | Less control of the loop |

## The contract

Decided: strongly typed. Shared Rust types for every message between the control plane and the daemon (`trailhead_factory::daemon`), serialized over Iroh. Built in M3 (`trailhead-daemon`).

- **In:** `Prepare` (repo, base branch, `setup` check), then `Run` (task spec, repo and branch, model and endpoint, step limit, required checks with their commands, hosts that must be blocked, Trailhead sub-agent token).
- **Out:** `Prepared`, or a `Report` (result, check results, usage totals), or `Failed` with a typed error.
- **Stream:** `Event`s while a run goes (step started, tool call, usage per model call, needs input).
- **Control:** `Cancel`. Pause and checkpoint-now stay with the control plane, which checkpoints through `Compute`.
- **Wire.** ALPN `trailhead/daemon/0`. One bidirectional stream per request, framed as a 4-byte big-endian length plus JSON.
- **Who dials.** The control plane dials the daemon. Each daemon's iroh key is HMAC-SHA256 of the control plane's master key and the sandbox name, so the control plane can reach any of its daemons, including after a restart, without storing addresses. The daemon gets its key and the control plane's endpoint id in a 0600 secrets file, which stays so a supervised restart can read it again, and refuses every other peer.
- **Getting in.** `Compute::start` uploads the daemon binary (24.5 MB release build) and the secrets file and runs it as a supervised Sprites service.
- **Losing the control plane.** A run outlives its stream. Each run has an id (the run name, such as `trail-103-a5`), and the daemon keeps every event and the final report. If the stream drops, the control plane redials and sends `Attach { run, seen }`; the daemon replays the events after `seen` and carries on. If no control plane reattaches within five minutes, the daemon cancels the run, which also stops the checks before anything is published, and the control plane's reconcile requeues the task. A control plane that restarts doesn't reattach yet; its reconcile requeues the task as before.

The Claude Code harness (`trailhead-harness-claude-code`) runs `claude --print --output-format stream-json` in the checkout, with `ANTHROPIC_BASE_URL` set to the run's endpoint and a placeholder key. It ends with structured output against a per-role schema (`done`, `needs_input` or `failed` for workers; `approve` or `request_changes` for reviewers). The step limit is enforced by killing the CLI, and a worker's changes are everything that differs from where the run started.

## Agent profiles

Decided: every run uses a named profile binding a role to a harness, a provider, a model and default limits. Profiles live in each repo's factory file. Tasks pick a profile; nothing hard-codes a model.

The Rust orchestrator stays deterministic. Model-driven "orchestration" means a planner agent proposing Trailhead tasks; the control plane decides whether to schedule them.

| Role | Default model | Typical work |
| --- | --- | --- |
| Planner | Opus | Break a spec into tasks, pick profiles, write acceptance criteria |
| Worker | Sonnet | Implement a task, fix failing checks, respond to review |
| Reviewer | Sonnet or Opus by tier | Review a diff against the task; Opus for T2 and T3 |
| Utility | Haiku | Summarize logs, draft PR text, classify failures, update status |

```toml
[profiles.worker]
role     = "worker"
adapter  = "claude-code"        # or codex, own-loop, ...
provider = "anthropic"
model    = "sonnet"             # resolved to a model id per provider
budget   = { usd = 5, minutes = 60, steps = 40 }
escalate_to = "worker-opus"     # after repeated failure

[profiles.utility]
role     = "utility"
adapter  = "own-loop"
provider = "anthropic"
model    = "haiku"
budget   = { usd = 0.25, minutes = 5, steps = 5 }
```

- **Model aliases.** `opus`, `sonnet`, `haiku` resolve to concrete ids in one table per provider, so upgrading a model is one line and other providers map the same aliases.
- **Escalation, not guessing.** A task starts on its profile; repeated failure or a stall escalates to `escalate_to`, and the escalation is logged.
- **Different model for review** where possible, to avoid shared blind spots.
- **Utility calls run in sandboxes too**, through the same connectors, so the control plane never holds a model key.
- **Profile changes are policy changes.** Moving work to a cheaper profile or raising a budget is loosening and needs a human; see [review.md](review.md).

## Specs and agent context

Decided: specs come from humans, written together with an AI, and enter through Trailhead, the source of truth.

- **Where specs start.** In Trailhead, a chat, or a doc; later imported one-way from Jira (Trailhead's planned importers). Committing an approved spec to a repo is an ordinary task whose output is a markdown file.
- **Spec stage.** A human starts a spec; a planner agent drafts, asks questions and revises; the human approves. The approved spec is the source of acceptance criteria.
- **Task spec.** Goal, acceptance criteria, link to the parent spec, profile, tier hint, budget.
- **Repo context.** `AGENTS.md` for conventions, plus the factory file for stack, checks, profiles and policy.
- **Per-profile context.** Each profile names its instruction files and skills. Reviewers get the task, diff and check results, never the author's transcript.
- **Typed model decisions.** Where a model makes a decision the factory acts on (tier hint, is it done, review verdict), it returns structured output against a schema. A dedicated decision model such as Jev (TypeSafeAI, which has a Sprites connector) could handle some yes/no and score calls; evaluate once the decision points are known.
