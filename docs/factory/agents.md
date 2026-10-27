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

Decided: strongly typed. Shared Rust types for every message between the control plane and the daemon, serialized over Iroh. Message set and wire format are still open.

- **In:** task spec, repo ref, base checkpoint id, budget, allowed tools, step limit, Trailhead sub-agent token.
- **Out:** a branch and PR opened through the code host, a run report, check results, usage totals.
- **Stream:** structured events (step started, tool call, checkpoint taken, cost tick, needs input).
- **Control:** pause, resume, cancel, checkpoint now.

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
