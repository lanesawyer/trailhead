# Task lifecycle

Proposal: the factory's lifecycle is Trailhead's existing one plus conventions, with a few engine changes ([engine-changes.md](engine-changes.md)). One sandbox per task, created when the task starts running and destroyed when it's done.

## Trailhead today

Statuses: `open`, `in_progress`, `needs_input`, `in_review`, `done`, `cancelled`, and `approved` between `in_review` and `done`. Claims are leases (agents 30 minutes by default, renewed by heartbeat; human claims don't expire). Moving a task to `in_review`, `approved`, `done` or `cancelled` releases its claim; `in_progress` and `needs_input` keep it. Delegated reviewers, agents a human lists on a project, can approve `in_review` → `approved` or `done`.

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
| Changes requested | `in_review` → `in_progress` or `open` | Gap: nothing keeps it for the original sandbox |
| Approved | `approved`: the grade's gates passed, waiting to merge | `approved` exists (TRAIL-86) |
| Merging | The merger or Lane merges; `done` means merged | Fits |
| Parked | `ask` → `needs_input` | Fits, but the agent lease expires while parked and an answer sends it back to `open` |
| Done / Cancelled | `done`, `cancelled` | Fits |
| Spend, budget | `metadata` JSON, filterable by key | Fits, by convention |

## Conventions

- **Sandbox per task, capped per repo.** No file locking; overlapping work is resolved through PRs and merge conflicts, which go through the same gates. A merge conflict sends the task back to Running.
- **Suspend during review.** The sandbox hibernates while waiting, so requested changes resume with context. Needs resume affinity; until then (and in the MVP), requested changes start a fresh sandbox from the PR branch.
- **Verdicts live in Trailhead, not GitHub reviews.** Sandboxes act on GitHub as the user who authorized the connector, and GitHub doesn't let a PR's author approve it (inferred, untested). Agent reviews post as PR comments.
- **Merger.** On factory projects the only delegated reviewer is the control plane's own identity, acting as the merger ([review.md](review.md#the-merger)). Reviewer agents record typed `review` verdicts; they don't move the task. The merger moves it to `approved` once the grade's gates pass, which for grades above Treeline includes Lane approving, and to `done` after the PR merges, whether it merged the PR or Lane did. Other agents can't approve or finish factory tasks. Lane adds the factory's identity to the project's reviewers once; only a human can.
- **Who can write task metadata.** Agents can't edit a task another actor holds, so the daemon writes its own run metadata as the claim holder. If the control plane needs to write it, Trailhead must let a parent agent act on tasks its sub-agents hold.
