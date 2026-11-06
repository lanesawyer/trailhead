# Order within a priority

> **Proposal, for Lane to approve (TRAIL-883).** Nothing here is built. Facts are from the code at `bd35c82`; guesses are marked **Guess**. It changes the task data model, so [PLAN.md](../PLAN.md) changes with it once approved ([PLAN.md changes](#plan-md-changes)).

Dragging a card up or down in a Lookout column should set the order the factory picks tasks up in. Today there is no such order within a priority.

## What exists today

- **The store's order.** `ready` sorts by `priority, due_by IS NULL, due_by, created_at` (`tasks.rs`), and `GET /v1/tasks` with the default `sort=priority` by the same keys plus `id`.
- **The factory's order.** `CoreTracker::ready` takes the store's list (filtered to the queue tasks' children), then raises each task's priority to the highest among itself and the open tasks it blocks (`raise_to_blocked_priority`, TRAIL-868). `in_start_order` in the run loop then sorts stably by priority and, within a priority, puts reworks of open PRs first. Everything else keeps the store's order.
- **The board's order.** `board.rs` sorts a column by priority, then key number. It matches neither the store nor the factory.

So a person who wants TRAIL-300 to run before TRAIL-290 can only change its priority, which moves it past every other task of that priority, or edit due dates, which is not what a due date means.

## Decisions to make

Lane's calls are marked **Ask**; the rest are the design's lean.

### The model in one paragraph

Each task has a `rank`, an opaque string. Within a project and a priority, tasks are ordered by `rank`. A move names a neighbour ("put X before Y"), and the server picks a rank between its neighbours, so a move writes one row and two clients moving at once never overwrite each other's numbers. The ready queue, `ls` and the board all sort by `(priority, rank)`. The rank is seeded from today's order and new tasks are placed where today's order would put them, so nothing moves until someone drags.

## Data model

**Lexicographic keys, not integer positions.** Integer positions make every move renumber the rows between the old and new place, which is many writes, many events and a conflict whenever two clients move at once. A fractional key (a string over `0-9A-Za-z`, compared bytewise, so SQLite's default `BINARY` collation sorts it right) needs one row: the key between the neighbours' keys. This is the usual midpoint scheme (Figma, Rocicorp). Floats are out: they run out of precision after about fifty halvings in one spot and hide that as a silent tie.

**Column.** Migration `0019_task_rank.sql`: `tasks.rank TEXT NOT NULL DEFAULT ''` and an index on `(project_id, priority, rank, id)`. `Task` gains `rank: String`, in the API and in event snapshots. Snapshots from before the column load with an empty rank; restoring one (undo) re-places the task by default order (below) rather than putting it first.

**Scope: per project and priority.** One ordered list per `(project_id, priority)`, all statuses together. Not per parent or queue: the factory reads several queue tasks (`queue` in `.trailhead.toml`) as one list, and a person ranking work doesn't think in parents. Compared across priorities a rank means nothing, which is what makes the priority change rule easy:

- **Edit the priority** (`PATCH`, CLI, MCP, the inspector): the task is placed in the new group where default order would put it, the same as a new task. It does not keep its old rank, which would land it at an arbitrary place in a list it was never ranked in.
- **Drag between priorities** (a drop in a different group of the board): one move that sets the priority and the rank together, so it is one event and one undo. See [the API](#the-api).

**Default placement.** The rule used for new tasks, a changed priority and the backfill: put the task where `due_by IS NULL, due_by, created_at, id` puts it among the group's existing tasks, i.e. before the first task that sorts after it. This keeps today's ordering until a person drags, and deadlines keep mattering for tasks nobody has ranked. After a drag, the person's order wins over due dates: **Ask:** is that right? The alternatives are due date as the primary key (a drag then does nothing against an earlier deadline, which looks like a bug) or only ranked tasks beating unranked ones (two orders to explain). Changing a due date later does not move the rank.

**Backfill.** Migration 0019 sets ranks in Rust, as 0014 did for due days (`backfill_due_days`): for each `(project_id, priority)`, in today's order (`due_by IS NULL, due_by, created_at, id`), assign evenly spaced equal-length keys with room to insert between (e.g. three base-62 digits, steps of 8, leaving gaps), including closed and deleted tasks so undoing a delete restores a sane place. Run it in the migration transaction. Because the order is today's, `ready` and `ls` return the same lists before and after.

**Running out of room.** A key is capped at 48 characters. Repeated inserts at one spot lengthen it; when a move would pass the cap, the same transaction re-spaces that group's ranks. That writes many rows with no event of their own, bumps no `version` and leaves `updated_at` alone, then emits one `task.reranked` notification event for the project (like reminders: not a change, skipped by undo, hidden by `changes_only`), so a mirror knows to reload the group. **Guess:** with random last-digit jitter on insert it should be very rare; it is there so the key length never needs an operator.

**Two simultaneous moves** into the same gap compute the same midpoint. The key gets a short random suffix so they differ, and ties still break by `id`, so every reader sees the same order.

## The API and events

**A move is relative.** `POST /v1/tasks/{task}/move`:

```json
{ "before": "TRAIL-290" }
{ "after": "TRAIL-290" }
{ "to": "first" }
{ "to": "last", "priority": 2 }
```

- Exactly one of `before`, `after`, `to` (`first`, `last` within the group). The anchor is a key or id.
- `priority` is optional and names the group the task ends up in (default: the task's own). An anchor must be in that group, in the same project, and not deleted; otherwise `400 invalid`. Lookout always sends it, because a drop at the line between two groups is otherwise ambiguous.
- The new rank is between the anchor and its real neighbour in the group, which includes tasks in other statuses a filtered board does not show, so a hidden task is never jumped by accident.
- Anchoring on the task itself is a no-op that answers `200` and writes nothing.
- The answer is the task, with its new `rank`.

**Event.** One `task.moved` event per move, with full before and after snapshots like every task event. `rank` and, for a cross-group move, `priority` change; `version` increments (it increments on every write). **Guess:** `updated_at` doesn't change, so a `sort=updated_at` list isn't reshuffled by ranking.

**Undo.** Free: undo restores the snapshot from before the move, as for any task edit. The conflict check looks at later events on the same task, so another client moving a different task does not conflict. Undo of a move puts back the old rank string even if the group was re-spaced since; a tie then falls back to `id`, harmless. `trailhead undo` with no arguments undoes the caller's last batch, so "undo my drag" works from the CLI too.

**Versions.** `If-Match`/body `version` is optional on a move and checked against the moved task only (`412 version_conflict` on mismatch), never against the anchor, because the anchor staying where it is is all the move needs. Lookout doesn't send one: a drag should win over a card someone else just edited, and the relative form is what makes that safe.

**Who may reorder.** Anyone who can edit the task: people, and agents within their token's scope. Scoped tokens see the usual `not_found` for an anchor outside the scope. The factory itself never moves tasks. **Ask:** may agents reorder? Leaning yes, because the move is attributed and undoable, and an agent triaging a backlog is useful; the cost is that an agent can reshuffle a person's order, which `revert --actor` undoes in one call. The alternative, humans only, is a one-line check like the grade rules.

**Surfaces.** REST above; MCP `move_task` (`task`, `before`/`after`/`to`, `priority`); CLI `trailhead move TRAIL-5 --before TRAIL-3`, `--after`, `--top`, `--bottom`, with `--priority`. Task JSON, `show` and OpenAPI carry `rank`; the CLI doesn't print it.

## Where the order applies

| Surface | Order after this change |
| --- | --- |
| `ready` (REST, MCP, CLI, the factory's tracker) | `priority, rank, id` |
| `ls` / `GET /v1/tasks`, `sort=priority` (the default) | `priority, rank, id` |
| Other sorts (`updated_at`, `created_at`, `scheduled`) | Unchanged; `scheduled` still ends in priority and now `rank` |
| Board columns | `priority, rank, id`, the same list the factory sees, only filtered to the column's status |

`due_by` drops out of the sort keys. It still drives reminders, `due_before` and the default placement.

**The factory's pick** is a stable chain over the store's order:

1. **Effective priority.** The task's own, raised by the highest priority among the open tasks it blocks (TRAIL-868).
2. **Reworks of open PRs first**, within that priority (TRAIL-868, `in_start_order`).
3. **Rank.** A task that was raised by a blocked task sorts at the place of the task it blocks, i.e. by that task's rank (the best-ranked one if it blocks several), then by its own rank. Without this a blocker's rank, which belongs to a list it no longer sits in, would put it at a random place in the higher group. A blocker therefore runs just ahead of what it unblocks.
4. `id`.

Today `raise_to_blocked_priority` changes the priority but the list is not re-sorted before `in_start_order`'s stable sort, so a raised blocker keeps the store's position. With ranks, `CoreTracker::ready` sorts explicitly after raising, so the tracker's order is the final order and `in_start_order` only has to lift reworks.

The board does not show this inheritance or the rework lift: it shows the human order. A card sitting in the order a person set may run earlier because it blocks something urgent or is a rework; the card already shows the blocker and rework state, and **Guess:** a later "next up" number on cards (from the tracker's order) is a better answer than faking it in the column.

**Which columns take a drag.**

- `open`: yes. This is the factory's queue.
- `in_progress`, `needs_input`: no. Running work is not waiting for a turn.
- `in_review`, `approved`: **Ask / leaning no.** Merge order is the merger's call (queue, CI, conflicts), not a priority, and ranking would suggest a promise the merger doesn't keep. These columns still show `(priority, rank)` order so cards don't jump when a task changes status.
- `done`, `cancelled`: no.

Everything above is about what the factory picks; a drag only has a meaning in `open`, but a task's rank keeps its place through status changes, so a task reopened for rework returns to where it was.

## Lookout

Kit (`clients/trailhead-ui`) gets one reusable widget, **`sortable`**, built on `bevy_picking`'s pointer events (`DragStart`, `Drag`, `DragEnter`/`DragOver`, `DragDrop`, `DragEnd`) because feathers has no drag-to-reorder. It follows the kit's rules (one widget per file, tokens, named sizes) and knows nothing about tasks: a container marks children as `Sortable`, the widget raises a `Reordered { item, anchor, side }` event on a drop, and the app decides what that means.

- **Pickup.** Pressing and moving a few pixels (a named drag threshold, so a click still opens the inspector) lifts the card: the original dims to a placeholder, and a copy follows the pointer on a high `GlobalZIndex` (above the board, below the modal, per `ui.md`).
- **Drop indicator.** A thin line in an accent token between the two cards nearest the pointer, found from each card's layout rect (above or below its midpoint). The line is a single entity moved, not rebuilt. Priority groups in a column have a small labelled divider ("P2"), which makes the boundary between groups two separate drop positions: the line under the last P2 card and the line over the first P3 card. The divider is what tells the move which `priority` to send.
- **Edges.** Dragging near the top or bottom of a column scrolls it. Columns build only the cards on screen (`ui.md`, the board): the anchor is always a built neighbour, and a drop past the built range uses the nearest built card.
- **Commit.** On drop, Lookout posts `move` with `before` or `after` and `priority`. Moves across columns are not accepted: no drop line shows outside the `open` column, and releasing elsewhere cancels, with the card going back. A status change stays an inspector action; dragging to another column to change status is out of scope.
- **Optimistic move.** The drop writes a `PendingMove { task, anchor, side, priority }` to the mirror's overlay and the board shows it at once. It does not compute a rank: the overlay is a local order override, which the mirror drops when the `task.moved` event arrives (the mirror then sorts by the true `(priority, rank, id)`) or when the request fails or doesn't confirm within a few seconds. Rollback is removing the override, the card reappears at its old place, and the refusal text shows beside the column the way the inspector shows one. Another client's move that lands first simply wins the position; ours lands relative to its anchor.
- **In place (TRAIL-853).** The board updates cards in place by task id. A move reorders the column's existing children (`Children` order) and never despawns; the dragged card keeps its entity so its state and picking survive. Priority changes within a drag move the entity between the group dividers in the same column. No redraw, no scroll jump. A test in `board.rs`'s `in_place_tests` style pins that a move event leaves entity ids unchanged.
- **Mirror.** The mirror stores `rank` per task from the snapshot and events (`task.moved`, `task.updated`, `task.reranked` → reload the project's tasks).

## PLAN.md changes

On approval, in the same PR as the first task:

- The **Task** row lists `rank`.
- **Ready queue**: "sorted by priority then due date" becomes priority, then rank (the order a person set; default placement keeps due date and age for tasks nobody has ranked).
- **Listing (Phase 3)**: the default sort is `priority, rank, id`.
- New paragraph **Order (Phase 5)**: model, scope, default placement, event, undo, who may move, the 48-character re-space.
- The REST list gets `POST /v1/tasks/{task}/move`; the MCP list `move_task`; the CLI list `move`.
- [operating.md](operating.md), where it says how the factory orders the queue, says the order is set with `move` or by dragging in Lookout; [ui.md](ui.md) describes dragging. [lifecycle.md](lifecycle.md) is unchanged.

## Implementation tasks

Each ships alone. File them under TRAIL-102 once approved (the first three) and TRAIL-685 or TRAIL-888 (the fourth and fifth).

1. **Core: rank and migration.** `0019_task_rank.sql`, the Rust backfill, `rank` on `Task` and snapshots, the midpoint and re-space code, default placement on create, priority edit and undo of an old snapshot, `Store::move_task` with `task.moved` and `task.reranked`, `priority, rank, id` in `ready` and `sort=priority`. Tests: backfilled order equals the old order for a project with due dates, ties and closed tasks; moves between, first, last; a long run of inserts at one spot re-spaces; two concurrent moves keep both and one order (the `tests/concurrency.rs` suite); undo, revert and scope. PLAN.md updated.
2. **API, CLI and MCP.** `POST /v1/tasks/{task}/move` with the errors above and `If-Match`, OpenAPI (the route-documented test), `rank` in the JSON, `trailhead move`, MCP `move_task`, who-may-move, docs.
3. **The factory's pick.** `CoreTracker::ready` sorts by effective priority, the blocked task's rank, then own rank; `in_start_order` unchanged; the testkit tracker fake orders the same way. Tests in `trailhead-control`: a dragged task runs first among equals; a blocker runs just ahead of what it blocks; reworks still lead their priority. `operating.md` updated.
4. **Lookout: the kit's `sortable` and the board's order.** The widget in `trailhead-ui` with its drop line, placeholder and edge scroll; the board sorts by `(priority, rank, id)` and shows priority dividers; `rank` in the mirror. No server write yet, so it ships and is reviewable alone.
5. **Lookout: drag to move.** The `open` column uses `sortable`, posts `move`, the optimistic overlay with confirm and rollback, the in-place reorder test, `ui.md` updated.

## Open questions for Lane

1. After a drag, may a person's order beat an earlier due date? (Leaning yes, as above.)
2. May agents reorder? (Leaning yes.)
3. Does `in_review`/`approved` take a drag as merge order? (Leaning no.)
4. Is a move allowed to leave `updated_at` alone? (Leaning yes.)
