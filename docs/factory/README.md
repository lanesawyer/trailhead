# Trailhead factory

Trailhead is growing from a task engine into a software factory: it turns tasks into agent runs inside locked-down sandboxes, gates every change through agent or human review, and records the work in Trailhead itself. These docs are the factory's design. `docs/PLAN.md` covers the whole project and the task engine in detail, and points here for the factory.

Read this index first, then open only the file the work in front of you needs.

| File | Read when you're working on |
| --- | --- |
| [overview.md](overview.md) | The big picture: principles, how the factory fits into Trailhead, the decisions table |
| [seams.md](seams.md) | The five plugin traits (`Compute`, `Harness`, `ModelProvider`, `Tracker`, `CodeHost`), crate layout, test fakes |
| [sandbox.md](sandbox.md) | Sprites, connectors and credentials, egress, standard checks, bootstrap and checkpoints |
| [agents.md](agents.md) | The in-sandbox daemon and its contract, agent profiles and models, specs and agent context |
| [lifecycle.md](lifecycle.md) | How a task moves from spec to merged PR, mapped onto Trailhead statuses and claims |
| [engine-changes.md](engine-changes.md) | Changes the task engine needs for the factory, with ready-to-file `TRAIL` tasks |
| [review.md](review.md) | Verification layers, risk tiers, per-repo gate policy, quorums, the policy ratchet |
| [state.md](state.md) | Where run state lives (no control-plane database), reconcile on boot, how sandboxes reach Trailhead |
| [budgets.md](budgets.md) | Cost and time limits, where spend numbers come from |
| [observability.md](observability.md) | Tracing, OpenTelemetry, backend options |
| [mvp.md](mvp.md) | The first slice, its milestones, and what "done" means |
| [operating.md](operating.md) | Running the factory day to day: starting it, handing it work, watching runs, outcomes |
| [open-questions.md](open-questions.md) | Undecided questions and hands-on tests still to run |
| [naming.md](naming.md) | Name ideas for the factory and its parts |

Each file marks its statements as **Decided**, **Leaning** or **Proposal**. Decided items change only with Lane's sign-off; update the file and the decisions table in [overview.md](overview.md) together.

Sandbox and connector facts were checked against the Sprites docs on 2026-10-25; Trailhead facts against commit `bc3fa26`.
