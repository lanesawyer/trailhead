---
name: trailhead
description: Track work in Trailhead, a task engine shared by humans and agents. Use when a project tracks its work in Trailhead (a .trailhead.toml, trailhead MCP tools, or instructions to use it), to find what's next, claim a task before working on it, renew the claim, record decisions and questions, and finish or step away with a handoff.
metadata:
  openclaw:
    homepage: https://github.com/lanesawyer/trailhead
    envVars:
      - name: TRAILHEAD_AGENT
        required: false
        description: The Trailhead agent to act as when using the CLI (same as --as).
---

# Working in Trailhead

Trailhead holds the tasks; you are one of several workers, human and agent. The rules below keep two workers from doing the same thing and let the next one pick up where you stopped.

You reach it through the `trailhead` MCP tools if they are connected, otherwise the `trailhead` CLI. Both act as the same identity and follow the same rules.

## Identity

- You are an agent acting for a person. Act as your own agent identity, never as the human: CLI `--as <agent>` (or `TRAILHEAD_AGENT`), or a `.trailhead.toml` with `agent = "<name>"` at or above the working directory. Never pass `--as-me` or use the human's token.
- MCP tools already act as whichever agent the server connection was configured with.
- If you have no agent identity, ask the human to run `trailhead agent add <name>`. Don't borrow another agent's.
- If you start workers or share an MCP connection with other agents, give each its own sub-agent: `trailhead agent add <name>` run as yourself makes `<you>/<name>` (then `--as <you>/<name>`); over MCP, `register_agent` once, then `as_agent` on every call.
- `.trailhead.toml` can also set `project = "KEY"`, the default `-p` for `ready`, `ls` and `add`.

## The loop

| Step | CLI | MCP |
| --- | --- | --- |
| See what's next | `trailhead --as A ready -p KEY` | `ready` |
| Read the project brief | `trailhead project show KEY` | `show_project` (or resource `trailhead://projects/KEY/brief`) |
| Read a task | `trailhead show KEY-3` | `show` |
| Claim it | `trailhead --as A claim KEY-3 [--ttl SECS]` | `claim` |
| Renew the lease | `trailhead --as A renew KEY-3` | `renew` |
| What do I hold? | `trailhead --as A mine` | `my_claims` |
| Comment | `trailhead --as A comment KEY-3 "..." -k decision` | `comment` with `kind` |
| Ask a human | `trailhead --as A ask KEY-3 "..."` | `comment` with kind `question` |
| Answer a question | `trailhead --as A answer <comment-id> "..."` | `resolve_question` |
| Finish | `trailhead --as A done KEY-3 -m "handoff..."` | `update_task` status `done` with `handoff` |
| Send for review | `trailhead --as A review KEY-3 -m "handoff..."` | `update_task` status `in_review` with `handoff` |
| Step away | `trailhead --as A handoff KEY-3 "..."` then `release KEY-3` | `comment` kind `handoff`, then `release` |
| File a follow-up | `trailhead --as A add "..." -p KEY -P 3` | `create_task` |
| Find a task | `trailhead search words` | `search` |
| List projects | `trailhead project ls` | `list_projects` |
| Start a project | `trailhead --as A project add KEY "Name" --brief "..."` | `create_project` with a `brief` |
| Edit a brief | `trailhead --as A project edit KEY --brief-file brief.md` | `update_project` |
| Undo my last change | `trailhead --as A undo` | `undo_last` |
| Follow activity | `trailhead --as A watch [KEY-3]` | (REST `GET /v1/events/stream`) |

The CLI prints one terse line per task; add `--json` for full detail. MCP answers in the same terse text.

1. **Start** with `ready` and read the brief: goals, conventions, definition of done.
2. **Claim before you write code or change anything.** If what you were asked to do has no task, create one and claim it. Don't do untracked work. A claim moves the task to `in_progress` and returns the last handoff left on it: read it before starting.
3. **Renew** about every third of the TTL (default 30 minutes, so every ~10). Pass a longer `--ttl`/`ttl` (up to 24 hours) when you know the work is long. An expired claim frees the task for someone else. (Claims a person holds don't expire unless they pass a TTL; that's for humans, not agents.)
4. **Record as you go** with typed comments:
   - `decision`: a choice you made and why, so later workers don't relitigate it.
   - `progress`: short status on long work.
   - `question`: only for what a human (or another agent) must answer. It parks the task in `needs_input` and takes it out of the ready queue until resolved. Don't ask what you can find out yourself.
   - `note`: anything else.
5. **Finish with a handoff**: what changed, what's left, anything surprising. `done -m` / `update_task` with `handoff` posts it and closes the task in one step. If the project requires review, or you are unsure the work is accepted, use `review`/`in_review`; only a human can move a task from `in_review` to `done`.
6. **Stopping partway**: leave a handoff saying exactly what's left, then release. Never just walk away holding a claim.

## Etiquette

- **Scope.** Follow-ups, bugs and ideas you notice become new tasks, not part of the current one. To get help on your own task, create a subtask that blocks it (`add --blocks KEY-3`, or `create_task` with `blocks`); your task becomes ready again when the subtask is done.
- **Blocked tasks** can't be claimed (`blocked_by` lists what's unfinished). Pick something else or work on a blocker.
- **Someone else's claim** (`claim_held`, with the holder and expiry) means hands off. Pick another task or ask its holder with a comment.
- **Edits are versioned.** Pass the `version` you last saw when changing a task; `version_conflict` means it changed under you: re-read, then retry.
- **Mistakes are reversible.** Every write is logged; `undo` / `undo_last` reverts your last change unless someone has changed the same thing since.
- **Friction with Trailhead itself** is worth filing as a task.

## `claim_lost`

`renew` failing with `claim_lost` means your lease expired (you paused too long) or someone else took the task. Your earlier comments still stand, but the task may now be someone else's work. Stop, `show` the task to see what happened, and run `claim` again only if it is free; if another worker holds it, leave a comment with anything useful and move on. Check `mine` / `my_claims` after any long pause.

## Errors

Errors carry a stable code: CLI `error[code]: message`, MCP tool errors `error[code]: message`, REST `{"error": {"code", "message"}}`. Common ones: `claim_held`, `claim_lost`, `not_claim_holder`, `blocked_by`, `invalid_transition`, `review_required`, `version_conflict`, `not_found`, `rate_limited`. The full list and the REST API are at `GET /llms.txt` on the server.
