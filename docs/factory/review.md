# Verification and review

> How the factory decides whether a change may merge: verification layers, risk grades, gate policy and the merger. [Operating](operating.md) has the setup.

Three layers, cheapest first. Each task carries a risk **grade**, and the repo's gate policy says which layers work at that grade must pass before it can merge.

1. **Mechanical checks.** The repo's standard checks ([sandbox.md](sandbox.md)), run in the author's sandbox, and the repo's own CI on the PR. A failure goes back to the author agent, not to review.
2. **Agent review.** A separate agent in a fresh sandbox sees the task, the diff and the check results, never the author's transcript. Different prompt, and where possible a different model.
3. **Human review.** Lane approves the task (`approved`), or merges the PR himself.

## Grades

Named after the climbing classes of the Yosemite Decimal System: the higher the grade, the more exposure. **Treeline** is the line between the grades that may merge without a human and the ones that need one; by default it sits above grade 2.

| Grade | Like | Example changes | Default gates |
| --- | --- | --- | --- |
| 1 | A walk on a trail | Docs, new or changed tests, formatting | Checks + CI; auto-merge |
| 2 | Off-trail | Ordinary code within existing modules | Checks + CI + agent review; auto-merge |
| 3 | Scrambling, above Treeline | New or upgraded dependencies, schema or migrations, public API changes, large diffs | Checks + CI + agent review + human |
| 4 | Exposed | Auth, secrets handling, sandbox egress, CI workflows, the factory's own policy (`.trailhead.toml`) and profiles | Checks + CI + human; agent review advisory |

How grades are set:

- **On the task, by a human, when it's triaged.** The grade is a field on the task ([PLAN.md](../PLAN.md), engine change #6). Grading a task is what makes it the factory's work: the factory only picks up graded tasks under its queue task, and skips an ungraded one, saying so. A task that should never be worked by the factory, like an item on a shopping list, simply has no grade.
- **Only raised after that.** Agents can raise a task's grade but never set the first one, lower it or clear it. The PR is graded the task's grade or higher:
  - **By the diff.** The control plane matches each changed file against the repo's path rules, and a diff over the repo's line limit is at least grade 3. If that comes out higher than the task's grade, the PR and the task are raised, with the file or rule that did it. Deleting a test or a check is never grade 1.
  - **By the reviewer.** Its verdict can name a higher grade with a reason.
- **The grade and anything that raised it** are recorded on the task and stated in the reviewer's PR comment, e.g. "Grade 3 (task was 1; Cargo.lock changed)".

## Gate policy

Decided: gates per grade are configurable per repo, in `.trailhead.toml`. The table above is the default. A repo can be fully autonomous, with no human gate at any grade; there, the top grade requires a quorum of independent agents.

```toml
[factory.grades]
# Path globs that raise a PR to at least this grade; unmatched files don't raise it.
2 = ["crates/*/src/**", "crates/*/build.rs", "crates/*/benches/**", "crates/*/examples/**", "cliff.toml", "rustfmt.toml"]
3 = ["**/Cargo.toml", "Cargo.lock", "**/migrations/**"]
4 = [".github/**", "fnox.toml", "mise.toml", "AGENTS.md", ".claude/**", ".mcp.json", "skills/**", ".trailhead.toml", "LICENSE"]
max_lines = 600          # bigger diffs are at least grade 3

[factory.gates.1]
require = ["checks", "ci"]
auto_merge = true

[factory.gates.2]
require = ["checks", "ci", "agent_review"]
auto_merge = false       # a person merges every code change

[factory.gates.4]
require = ["checks", "quorum"]   # or ["checks", "agent_review", "human"]
quorum = { reviewers = 3, agree = 3, distinct_models = true }

[factory.merge]
mode = "shadow"          # off | shadow | on
```

Gates are keyed by grade (`[gates.1]` to `[gates.4]`); `require` lists any of `checks`, `ci`, `agent_review`, `human` and (once built) `quorum`. Status: the gates, the `approved` move and shadow mode are built (TRAIL-203), and merging with `[merge] mode = "on"` (TRAIL-205).

Decided (2026-10-29): Trailhead's own repo auto-merges grade 1 only to start. Lane grades the queue once grades exist and widens auto-merge from there. With the rules above, code in `crates/*/src` raises a grade-1 task to 2, so docs and test-only PRs merge on their own (deleting or renaming a test is still at least grade 2). Files that steer the agents (`AGENTS.md`, `.claude/`, `.mcp.json`, `skills/`, `.trailhead.toml`) are grade 4 with CI and the factory's policy. Decided (2026-10-29): test-only PRs are grade 1; grade 2 auto-merge waits on TRAIL-260, TRAIL-298 and TRAIL-295 and a run of grade 2 shadow decisions Lane agrees with. Decided (2026-10-29): grade 2 auto-merges too, now that those three have landed and the grade 2 decisions so far were right (TRAIL-314). TRAIL-390, reworking a PR whose CI fails after a branch update, follows rather than blocks it.

Decided: quorum reviewers must be independent. Quorums are built after the merger, once a repo wants a grade with no human gate (TRAIL-204).

- **Different models or providers** (`distinct_models`).
- **Fresh sandboxes and contexts**, no access to the author's transcript.
- **Blind first pass.** Each reviewer records a verdict before seeing the others; disagreement escalates.

## What counts as a review round

`max_review_rounds` (`[repo]`, default 3) caps how many reviews may ask for changes. A reviewer run counts as a round unless it was not the PR's fault:

| Reviewer outcome | Round |
|---|---|
| Approves, or asks for changes | Counts. At the cap, a review that still asks for changes parks the task. |
| Breaches its budget | Counts, and the task is parked at once with a question naming the breach (TRAIL-268, TRAIL-319). Rerunning a reviewer that already overspent would only overspend again, so there is no retry to cap. Reopening the task starts the count again. |
| Ends with no verdict | Given back and counted like an infrastructure failure: the task waits out the infra backoff, and parks at `max_infra_failures`. |
| Infrastructure failure | Given back; the task waits out the infra backoff, and parks at `max_infra_failures`. |
| Cancellation | Given back, as for an infrastructure failure, but with no failure count, backoff or parking: a deliberate control-plane stop isn't a failure. |
| Conflict rework | Not a round (it has `max_conflict_reworks`). The review after one doesn't use a round either, unless it asks for changes; then it counts as one. |

## The merger

The control plane acts as the merger. It is the factory project's only delegated reviewer ([lifecycle.md](lifecycle.md)), and the only agent that moves a factory task to `approved` or `done`.

1. After each pass that could change the answer (a verdict, a rework, CI finishing, Lane approving, a reconcile), it evaluates the PR's gates against its **current head**: the sandbox checks passed, the repo's CI is green on that commit, the latest verdict on that head approves, and, for grades whose gates require a human, Lane has approved.
2. When they pass, it moves the task to `approved`. A push after that sends it back to `in_review`.
3. It records the decision in the task's `factory` metadata (`merge`: the grade, which gates passed, and what it's waiting on) and says it once on the PR when it changes: "Grade 1: checks, CI and review passed; merging." or "Grade 3 (Cargo.lock changed): waiting for a person."
4. What it does with an `approved` task depends on `[factory.merge] mode` and the grade's `auto_merge`:
   - **`off`:** nothing; Lane merges every PR.
   - **`shadow`:** it posts what it *would* do and doesn't merge, including, when the PR is behind `main`, that it would update the branch first ("Would merge `main` into the branch, wait for CI on the new head, then squash-merge."). Lane merges as today, and compares. The default; Trailhead's own repo ran it until 2026-10-29 and is now `on` (TRAIL-313).
   - **`on`:** for a grade with `auto_merge`, it first brings the PR up to date (below), then squash-merges it, pinned to the head its gates passed on. A merge conflict sends the task back to running, to rework the PR on top of `main`; a moved head or a merge the host refuses for another reason is tried again on the next pass, not reworked. For other grades, Lane's approval is the human gate and the merger merges after it.

   **Updating the branch (TRAIL-212).** `main` requires its checks to pass on a branch that's up to date with it (strict status checks), so a green run against an old base isn't enough: PR #21 merged on one while `main` had since changed a signature, and `main` stopped compiling. Before merging, the merger asks the host whether the PR is behind its base (`CodeHost::behind`). If it is, it merges the base branch into the PR branch (`CodeHost::update_branch`: a merge commit, never a rebase and force-push) and goes back to waiting for CI on the new head. It merges only when CI is green on a head that isn't behind. Another PR landing meanwhile just repeats this.
   - **Conflicts.** If the base branch doesn't merge cleanly, the task goes back to running for a rework on top of `main`, as for any merge conflict ([lifecycle.md](lifecycle.md)). Only a real merge conflict does: the host also refuses an update when there's nothing to merge in (an update it queued earlier has landed, so the merger looks at the head again) or when the head moved since the merger read it (the update names the head it read, so a push that raced it isn't merged over). Those are not conflicts, since a conflict costs a review round (TRAIL-199); the first is `UpToDate` and the second an error the merger retries on its next tick.
   - **The verdict survives a clean update.** Decided: a merge of `main` with no conflicts keeps the reviewer's approval, and CI on the new head is the gate. The merge commit adds no work of the PR's own, and the reviewer saw the PR's diff against `main`, which the merge doesn't change; re-reviewing every PR each time something else lands would never converge. So step 1's "the latest verdict on that head approves" is evaluated against the head before the update, as long as the only commits since are merges of the base branch the host made. The merger checks that with `CodeHost::carries_verdict`: the head the verdict is for must lead to the current head, and every commit since that the PR has and its base lacks must be a two-parent merge commit whose second parent is on the base branch (TRAIL-260). A listing the host cut short can't be vouched for, so it counts as a new head. That is a check of shape, not content: a merge whose second parent is on the base passes even if it changes the tree beyond a clean merge, and a conflict-resolving rework pushes exactly such a merge. So the merger carries only a verdict given on the task's current attempt; a rework takes a new attempt, so its merge commit can't inherit the old approval, and it needs a new verdict like any other push. A person's hand-edited merge of the base (an "evil merge") is a known gap the check can't see. The sandbox checks are not re-run; CI is.
5. However a PR is merged, by the merger or by Lane on GitHub, the task is closed as `done` the same way, with a handoff naming the PR and the merge commit (TRAIL-198). `done` always means merged.

## The ratchet

Decided: agents may tighten the policy under the repo's normal gates, but any change that loosens it always needs a human, even in a fully autonomous repo. Loosening includes dropping a gate, lowering a quorum, raising a budget, moving work to a cheaper profile, moving Treeline up, lowering a path's grade, or lowering a task's grade. Until the policy lives somewhere the control plane can diff, every change to `[factory]` in `.trailhead.toml` is grade 4, so a human sees it. Trailhead already applies the same rule to `require_review` and project reviewers.

## Rules that keep the gate honest

- The grade is set on the task by a human, and only raised after that: by the diff (paths, dependency files, size) or a reviewer. The author never grades its own work.
- A change touching the policy or profile files is top grade.
- Any reviewer can raise a grade through its verdict; nobody can lower one.
- Author and reviewer disagree past the repo's review-round cap → park for a human (TRAIL-180).
- Gates are evaluated on the PR's current head. A push after an approval needs a new approval.
