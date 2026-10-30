# Getting started

Trailhead runs on Linux. These steps use only the task engine; the factory is covered in [Operating the factory](factory/operating.md).

## Install

From a checkout of the repo:

```sh
cargo install --path crates/trailhead --locked
```

The README lists other ways to get the CLI as releases are cut.

## Start the server

You don't have to. The first `trailhead` command starts a local server in the background and restarts it after a reinstall. To manage it by hand:

```sh
trailhead server status    # also: start, stop, restart
```

## Create a project and tasks

```sh
trailhead project add DEMO "Demo"
trailhead add "Write the README" -p DEMO -P 2
trailhead ready -p DEMO          # what can be worked on next
trailhead claim DEMO-1           # take it; prints the last handoff
trailhead done DEMO-1 -m "handoff: what changed, anything surprising"
```

A project's `brief` holds goals, conventions and a definition of done for agents to read before they claim work (`trailhead project show DEMO`). Priority runs from 1 (urgent) to 4.

## Agents and `--as`

Agents act as themselves, never as you. Each belongs to a user, so every change traces back to a person.

```sh
trailhead agent add claude          # creates the agent and saves its token
trailhead --as claude ready -p DEMO
```

Instead of `--as` each time, put `agent = "claude"` (and optionally `project = "DEMO"`) in a `.trailhead.toml` at or above the working directory; `--as-me` acts as you again. When one agent starts others, it makes them sub-agents (`trailhead agent add openapi` as `claude` gives `claude/openapi`).

## MCP for Claude Code

For agents without a shell, or tools like Claude Code, the server speaks MCP at `/mcp`. `trailhead --as claude mcp` is a stdio bridge that needs no token in the config. In `.mcp.json`:

```json
{
  "mcpServers": {
    "trailhead": { "command": "trailhead", "args": ["--as", "claude", "mcp"] }
  }
}
```

Agents can learn the API from `GET /llms.txt` on any server, and the workflow from the portable skill in `skills/trailhead/SKILL.md`.

## Other machines

Reach the server through a tunnel to its loopback port, and give each machine's agent its own token. The README's "Other machines" section has the commands.
