# Verification and review

Three layers, cheapest first. Each change gets a risk **grade**, and the repo's gate policy says which layers that grade must pass before it can merge.

1. **Mechanical checks.** The repo's standard checks ([sandbox.md](sandbox.md)), run in the author's sandbox, and the repo's own CI on the PR. A failure goes back to the author agent, not to review.
2. **Agent review.** A separate agent in a fresh sandbox sees the task, the diff and the check results, never the author's transcript. Different prompt, and where possible a different model.
3. **Human review.** Lane merges the PR himself.

## Grades

Named after the climbing classes of the Yosemite Decimal System: the higher the grade, the more exposure. **Treeline** is the line between the grades that may merge without a human and the ones that need one; by default it sits above grade 2.

| Grade | Like | Example changes | Default gates |
| --- | --- | --- | --- |
| 1 | A walk on a trail | Docs, new or changed tests, formatting | Checks + CI; auto-merge |
| 2 | Off-trail | Ordinary code within existing modules | Checks + CI + agent review; auto-merge |
| 3 | Scrambling, above Treeline | New or upgraded dependencies, schema or migrations, public API changes, large diffs | Checks + CI + agent review + human |
| 4 | Exposed | Auth, secrets handling, sandbox egress, CI workflows, the factory's own policy (`factory.toml`) and profiles | Human required; agent review advisory |

How a PR gets its grade:

- **The control plane computes it from the diff, never the author.** Each changed file matches the repo's path rules; the PR takes the highest grade of any file. A file no rule matches is grade 2. Deleting a test or a check is never grade 1.
- **Size raises it.** A diff over the repo's line limit is at least grade 3.
- **Reviewers can raise it, nobody can lower it.** The reviewer's verdict can name a higher grade with a reason; the PR keeps the highest grade anyone gave it.
- **The grade is recorded** in the task's `factory` metadata and stated in the reviewer's PR comment.

## Gate policy

Decided: gates per grade are configurable per repo, in `factory.toml`. The table above is the default. A repo can be fully autonomous, with no human gate at any grade; there, the top grade requires a quorum of independent agents.

```toml
[grades]
# Path globs per grade; a PR takes the highest grade of its files, and unmatched files are grade 2.
1 = ["docs/**", "**/*.md", "**/tests/**"]
3 = ["**/Cargo.toml", "Cargo.lock", "**/migrations/**"]
4 = [".github/**", "factory.toml", "fnox.toml", "mise.toml"]
max_lines = 600          # bigger diffs are at least grade 3

[gates.1]
require = ["checks", "ci"]
auto_merge = true

[gates.2]
require = ["checks", "ci", "agent_review"]
auto_merge = false       # Trailhead itself: grade 1 only, to start

[gates.4]
require = ["checks", "quorum"]   # or ["checks", "agent_review", "human"]
quorum = { reviewers = 3, agree = 3, distinct_models = true }

[merge]
mode = "shadow"          # off | shadow | on
```

Decided (2026-10-29): Trailhead's own repo auto-merges grade 1 only to start. Lane triages the queue by grade once grades exist and widens it from there.

Decided: quorum reviewers must be independent. Quorums are built after the merger, once a repo wants a grade with no human gate (TRAIL-204).

- **Different models or providers** (`distinct_models`).
- **Fresh sandboxes and contexts**, no access to the author's transcript.
- **Blind first pass.** Each reviewer records a verdict before seeing the others; disagreement escalates.

## The merger

The control plane acts as the merger. It is the factory project's only delegated reviewer ([lifecycle.md](lifecycle.md)), and the only agent that moves a factory task to `done`.

1. After each pass that could change the answer (a verdict, a rework, CI finishing, a reconcile), it evaluates the PR's gates against its **current head**: the sandbox checks passed, the repo's CI is green on that commit, the latest verdict on that head approves, and the grade allows merging without a human.
2. It records the decision in the task's `factory` metadata (`merge`: the grade, which gates passed, and what it's waiting on) and says it once on the PR: "Grade 1 (docs only); checks, CI and review passed; merging." or "Grade 3 (Cargo.lock changed): waiting for Lane."
3. What it does next depends on `[merge] mode`:
   - **`off`:** nothing; Lane merges every PR.
   - **`shadow`:** it posts what it *would* do and doesn't merge. Lane merges as today, and compares. The default until Lane switches it.
   - **`on`:** it squash-merges the PR and closes the task as `done` with a handoff naming the PR, the merge commit and the gates. A merge conflict sends the task back to running, to rework the PR on top of `main`.
4. However a PR is merged, by the merger or by Lane on GitHub, the task is closed the same way, so `done` always means merged (TRAIL-198).

## The ratchet

Decided: agents may tighten the policy under the repo's normal gates, but any change that loosens it always needs a human, even in a fully autonomous repo. Loosening includes dropping a gate, lowering a quorum, raising a budget, moving work to a cheaper profile, moving Treeline up, or lowering a path's grade. Until the policy lives somewhere the control plane can diff, every change to `factory.toml` is grade 4, so a human sees it. Trailhead already applies the same rule to `require_review` and project reviewers.

## Rules that keep the gate honest

- The grade is computed by the control plane from the diff (paths, dependency files, size), never self-declared by the author.
- A change touching the policy or profile files is top grade.
- Any reviewer can raise a grade through its verdict; nobody can lower one.
- Author and reviewer disagree past the repo's review-round cap → park for a human (TRAIL-180).
- Gates are evaluated on the PR's current head. A push after an approval needs a new approval.
