# Task lifecycle

> How one task moves through the factory, from queued spec to merged PR. Next: [Operating](operating.md) to run it, [Review](review.md) for grades, gates and the merger.

Proposal: the factory's lifecycle is Trailhead's existing one plus conventions, with a few engine changes ([engine-changes.md](engine-changes.md)). One sandbox per task, created when the task starts running and destroyed when it's done.

## Trailhead today

Statuses: `open`, `in_progress`, `needs_input`, `in_review`, `approved`, `done`, `cancelled`, with `approved` between `in_review` and `done`. Claims are leases (agents 30 minutes by default, renewed by heartbeat; human claims don't expire). Moving a task to `in_review`, `approved`, `done` or `cancelled` releases its claim; `in_progress` and `needs_input` keep it. Delegated reviewers, agents a human lists on a project, can approve `in_review` → `approved` or `done`.

## Mapping

| Factory step | Trailhead | Fit |
| --- | --- | --- |
| Spec | Parent task; `description` is the editable spec, comments are history | Fits |
| Approved | Human moves the spec task to `done` (project with `require_review`) | Fits, by convention |
| Planned | Planner creates subtasks with `parent_id`, `blocked_by` the spec task | Fits |
| Queued | `open`, graded, under the queue task; ready queue; repo cap enforced by the control plane | Fits once tasks have grades |
| Running | Claim + `in_progress`; the daemon renews the lease | Fits |
| PR open / Review | `in_review`; PR URL, grade, profile in `metadata` | Fits, but the claim is released |
| Agent review verdict | One delegated reviewer approves straight to `done` | Gap: no verdict record, no quorum |
| Changes requested | `in_review` → `open` (reopened by the control plane, review kept in `metadata`) | Fits; the rework starts a fresh sandbox, not the original one |
| Approved | `approved`: the grade's gates passed, waiting to merge | `approved` exists (TRAIL-86) |
| Merging | The merger or a person merges; `done` means merged | Fits |
| Parked | `ask` → `needs_input` | Fits, but the agent lease expires while parked and an answer sends it back to `open` |
| Done / Cancelled | `done`, `cancelled` | Fits |
| Spend, budget | `metadata` JSON, filterable by key | Fits, by convention |

## Conventions

- **Sandbox per task, capped per repo.** No file locking; overlapping work is resolved through PRs and merge conflicts, which go through the same gates. A merge conflict sends the task back to Running. Reconcile checks the PR of each in-review task, and each approved one, with one `CodeHost::pr_state` call, and also those of open tasks waiting for a rework (a merge finishes them, a closed PR parks them) and parked `needs_input` tasks (only a merge finishes them; an open or closed PR leaves them parked) (TRAIL-323). The call also says whether it merges cleanly (`PrState::Open { mergeable }`); one GitHub reports as conflicting (TRAIL-199) is reopened for a rework like a request for changes (an approved task is unapproved first, so shadow mode and grades that don't auto-merge notice when `main` moves too; TRAIL-299), except the checkout has the base branch merged in, uncommitted, for the worker to resolve. The result is published as a merge commit (`Commit::merges`), checked, and reviewed again. It counts as a conflict rework, not a review round (TRAIL-221): the count is `conflict_reworks` in the `factory` metadata, a PR that keeps conflicting parks for a person at `max_conflict_reworks` (`[repo]`, default 5), and the review after a conflict rework doesn't use a round either, though one that asks for changes does. A review asking for changes doesn't reset the conflict count.
- **Suspend during review.** The sandbox hibernates while waiting, so requested changes resume with context. Needs resume affinity; until then (and in the MVP), requested changes start a fresh sandbox from the PR branch. The control plane records the review (reviewer and body) in the task's `factory` metadata and reopens the task; the next run checks out `origin/<branch>`, is resumed with the review as feedback, publishes on top of the PR head and pushes to the same PR. Review rounds count across reworks; when round `max_review_rounds` (default 3) still asks for changes, the task is parked as `needs_input` with the last review in the question. `max_attempts` counts failed runs since the last review that asked for changes, so reworks get attempts of their own. The review is marked done in the same write that records it, and a reconcile that finds such a task still `in_review` (the control plane stopped before reopening it) finishes the reopen or the park. A review an approval has since superseded is not passed to later runs. A check that passed only on rerun during a rework is posted as a PR comment, since the PR body was written when it opened.
- **Verdicts live in Trailhead, not GitHub reviews.** Sandboxes act on GitHub as the user who authorized the connector, and GitHub doesn't let a PR's author approve it (inferred, untested). Agent reviews post as PR comments.
- **Merger.** On factory projects the only delegated reviewer is the control plane's own identity, acting as the merger ([review.md](review.md#the-merger)). Reviewer agents record typed `review` verdicts; they don't move the task. The merger moves it to `approved` once the grade's gates pass, which for grades above Treeline includes a person approving, and to `done` after the PR merges, whether it merged the PR or a person did. Both go through one function, `Factory::close_merged` (TRAIL-198): reconcile calls it for a merge made outside the factory (`PrState::Merged` from `CodeHost::pr_state`), and the merger calls it right after `CodeHost::merge` (`[merge] mode = "on"`, TRAIL-205), so auto-merge adds the merge call and the gate check, not a second way to finish tasks. It records who merged (`human` or `merger`), the PR and the merge commit in the task's `factory` metadata and in the handoff, which for the merger also names the grade and the gates that passed. A merge conflict at merge time sends the task back to running for a rework, and without delegated-reviewer rights the merger only behaves as in shadow mode. A PR closed without merging parks the task with a question for a person rather than reopening it, since a rework would push to a PR nobody will merge. Other agents can't approve or finish factory tasks. A person adds the factory's identity to the project's reviewers once; only a human can.
- **Who can write task metadata.** Agents can't edit a task another actor holds, so the daemon writes its own run metadata as the claim holder. If the control plane needs to write it, Trailhead must let a parent agent act on tasks its sub-agents hold.
