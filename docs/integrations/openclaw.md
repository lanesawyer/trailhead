# Trailhead with OpenClaw

OpenClaw agents can work Trailhead tasks with no plugin code: OpenClaw mounts MCP servers, and Trailhead ships an MCP server and an agent skill. Checked against [docs.openclaw.ai](https://docs.openclaw.ai/llms.txt) on 2026-10-15.

## 1. Give OpenClaw its own identity

Agents act for a person but never as them. Create an agent for OpenClaw (as yourself, so outside an agent repo or with `--as-me`):

```sh
trailhead --as-me agent add openclaw            # token saved to the OS keyring
trailhead --as-me agent add openclaw --projects WEB --rate-limit 120   # optionally limit it
```

## 2. Mount the MCP server

Locally, run Trailhead's stdio bridge. It uses the saved token and starts the Trailhead server if it isn't running:

```sh
openclaw mcp set trailhead '{"command":"trailhead","args":["--as","openclaw","mcp"]}'
```

That gives OpenClaw agents `ready`, `claim`, `renew`, `release`, `show`, `create_task`, `update_task` (finish with `status: "done"` and a `handoff`), `comment`, `resolve_question`, `search`, `my_claims` and `undo_last`, plus each project's brief as a `trailhead://projects/<KEY>/brief` resource.

Over HTTP instead (for example when OpenClaw runs elsewhere), point it at `/mcp` with `transport: "streamable-http"` and an `Authorization: Bearer <token>` header. Keep the token in an environment variable (OpenClaw expands `${VAR}` in headers) rather than in committed config.

## 3. Teach the workflow

Add Trailhead's agent skill, `skills/trailhead/SKILL.md` in this repo, to OpenClaw's skills, for example by listing the repo's `skills` directory in `skills.load.extraDirs`. It teaches claiming before working, renewing, decisions and questions as comments, handoffs, and what `claim_lost` means.

## Workboard

OpenClaw's bundled Workboard is a separate task board. Trailhead doesn't sync with other tools by design, so pick one per project. Trailhead is the better fit when humans, other agent platforms or your own apps share the same work. Workboard's own docs show no provider interface for an external backend, so a "Trailhead as Workboard storage" plugin isn't possible today.

## When a plugin would be worth it

An OpenClaw tool plugin (TypeScript, `openclaw plugins init`) would only add value for things MCP can't do, such as pushing Trailhead events into OpenClaw sessions. Trailhead's webhooks (`trailhead webhook add`) already deliver every event to any HTTP endpoint, so start there.
