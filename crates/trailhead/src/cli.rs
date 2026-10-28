//! The command line: every command, flag and its help text.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

/// Trailhead: a headless task engine for humans and agents.
#[derive(Parser)]
#[command(version, about, after_help = IDENTITY_HELP)]
pub(crate) struct Cli {
    /// Print full JSON instead of terse lines.
    #[arg(long, global = true)]
    pub(crate) json: bool,
    /// The server [default: `url` in <config dir>/config.toml, which `login` sets, else http://127.0.0.1:7241].
    #[arg(long, global = true, env = "TRAILHEAD_URL")]
    pub(crate) url: Option<String>,
    #[arg(long, global = true, env = "TRAILHEAD_TOKEN", hide_env_values = true)]
    pub(crate) token: Option<String>,
    /// Act as a local agent, using its saved token [env: TRAILHEAD_AGENT].
    #[arg(long = "as", global = true, value_name = "AGENT")]
    pub(crate) agent: Option<String>,
    /// Act as yourself, ignoring TRAILHEAD_AGENT and the agent in .trailhead.toml.
    #[arg(long, global = true, conflicts_with = "agent")]
    pub(crate) as_me: bool,
    #[command(subcommand)]
    pub(crate) cmd: Cmd,
}

const IDENTITY_HELP: &str = "\
Identity, first match wins:
  1. --token / TRAILHEAD_TOKEN
  2. --as <agent> / TRAILHEAD_AGENT
  3. agent = \"...\" in the nearest .trailhead.toml at or above the working directory
  4. your own saved credential
--as-me skips 2 and 3. A .trailhead.toml may also set project = \"KEY\" as the default -p.

Saved tokens live in the OS keyring when one is available, else in 0600 files in the
config directory. TRAILHEAD_NO_KEYRING=1 skips the keyring.";

#[derive(Subcommand)]
pub(crate) enum Cmd {
    /// Open, unclaimed, unblocked tasks in priority order.
    Ready {
        #[arg(short, long, env = "TRAILHEAD_PROJECT")]
        project: Option<String>,
        #[arg(short = 'n', long)]
        limit: Option<i64>,
    },
    /// List tasks. Defaults to everything not done or cancelled.
    Ls {
        #[arg(short, long, env = "TRAILHEAD_PROJECT")]
        project: Option<String>,
        /// Comma-separated statuses.
        #[arg(short, long)]
        status: Option<String>,
        #[arg(long)]
        parent: Option<String>,
        /// Include done and cancelled.
        #[arg(short, long)]
        all: bool,
        /// Only tasks scheduled for today or earlier: today's plan.
        #[arg(long)]
        today: bool,
        #[arg(short = 'n', long)]
        limit: Option<i64>,
    },
    /// Show a task with its claim, dependencies and comments.
    Show {
        task: String,
    },
    /// Create a task.
    Add(AddArgs),
    /// Change a task's fields.
    Edit(EditArgs),
    /// Take a lease on a task and start it. Prints the last handoff.
    Claim {
        task: String,
        /// Lease length in seconds.
        #[arg(long)]
        ttl: Option<i64>,
    },
    /// Claims you hold and when they expire.
    Mine,
    /// Extend your lease.
    Renew {
        task: String,
        #[arg(long)]
        ttl: Option<i64>,
    },
    /// Give a task back.
    Release {
        task: String,
    },
    /// Mark done, optionally leaving a handoff first.
    Done {
        task: String,
        #[arg(short, long)]
        message: Option<String>,
    },
    /// Send for human review.
    Review {
        task: String,
        #[arg(short, long)]
        message: Option<String>,
    },
    Cancel {
        task: String,
    },
    Reopen {
        task: String,
    },
    /// Delete a task (undoable).
    Rm {
        task: String,
    },
    /// Comment on a task.
    Comment {
        task: String,
        body: String,
        /// handoff, progress, question, decision or note.
        #[arg(short, long, default_value = "note")]
        kind: String,
        /// User or agent id to address.
        #[arg(long)]
        to: Option<String>,
    },
    /// Record a review verdict on a task: approve or request-changes. Doesn't move the task.
    Verdict {
        task: String,
        #[arg(value_parser = ["approve", "request-changes"])]
        verdict: String,
        body: String,
        /// The model that reviewed, if an agent.
        #[arg(long)]
        model: Option<String>,
    },
    /// Leave a handoff: what's done, what's left, gotchas.
    Handoff {
        task: String,
        body: String,
    },
    /// Ask a question; parks the task until answered.
    Ask {
        task: String,
        body: String,
        #[arg(long)]
        to: Option<String>,
    },
    /// Resolve a question, optionally answering it. Takes the task key (when it
    /// has one open question) or the question's comment id.
    Answer {
        #[arg(value_name = "TASK_OR_COMMENT_ID")]
        comment_id: String,
        body: Option<String>,
    },
    /// BLOCKER must finish before BLOCKED.
    Block {
        blocker: String,
        blocked: String,
    },
    Unblock {
        blocker: String,
        blocked: String,
    },
    /// Full-text search over tasks and comments.
    Search {
        query: Vec<String>,
        #[arg(short = 'n', long)]
        limit: Option<i64>,
    },
    /// The event log, newest first.
    Log {
        task: Option<String>,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: i64,
    },
    /// Follow events live as they happen.
    Watch {
        /// Only this task's events.
        task: Option<String>,
        /// Comma-separated kind prefixes, e.g. `task.,comment.`.
        #[arg(short, long)]
        kinds: Option<String>,
        /// Replay from after this event id instead of starting now.
        #[arg(long)]
        after: Option<i64>,
    },
    /// Undo an event or batch. With no arguments, undoes your last batch.
    Undo {
        event_id: Option<i64>,
        #[arg(long)]
        batch: Option<String>,
    },
    /// Revert everything an agent did since a time.
    Revert {
        actor_id: String,
        /// RFC 3339 timestamp or YYYY-MM-DD.
        #[arg(long)]
        since: String,
    },
    #[command(subcommand)]
    Project(ProjectCmd),
    #[command(subcommand)]
    Agent(AgentCmd),
    #[command(subcommand)]
    Token(TokenCmd),
    /// Outbound webhooks (humans only; in an agent repo, add --as-me).
    #[command(subcommand)]
    Webhook(WebhookCmd),
    Whoami,
    /// Save a token for the server at --url, read from stdin. Get one on the server's
    /// machine with `trailhead agent add <name> --print-token`.
    Login,
    /// Speak MCP over stdin/stdout, forwarding to the server's /mcp endpoint.
    /// For MCP clients that launch a command, e.g. `trailhead --as claude mcp`.
    Mcp,
    /// Manage the local background server.
    #[command(subcommand)]
    Server(ServerCmd),
    /// Run the server in the foreground.
    Serve {
        #[arg(long, env = "TRAILHEAD_BIND", default_value = trailhead_server::DEFAULT_BIND)]
        bind: String,
    },
}

#[derive(Args)]
pub(crate) struct AddArgs {
    pub(crate) title: String,
    #[arg(short, long, env = "TRAILHEAD_PROJECT")]
    pub(crate) project: Option<String>,
    #[arg(short, long)]
    pub(crate) description: Option<String>,
    /// 1 (urgent) to 4 (low).
    #[arg(short = 'P', long)]
    pub(crate) priority: Option<i64>,
    #[arg(long)]
    pub(crate) parent: Option<String>,
    /// The deadline: a day (YYYY-MM-DD, today, tomorrow, fri), optionally with a time
    /// (fri 5pm), a time alone for today, or an RFC 3339 timestamp.
    #[arg(long, num_args = 1..=3)]
    pub(crate) due: Option<Vec<String>>,
    /// When to work on it: a day (YYYY-MM-DD, today, tomorrow, fri), optionally with a
    /// time (today 1pm, 2026-10-20 13:30), or a time alone for today.
    #[arg(long, num_args = 1..=3)]
    pub(crate) on: Option<Vec<String>>,
    /// Tasks that must finish first.
    #[arg(long, value_delimiter = ',')]
    pub(crate) after: Vec<String>,
    /// Tasks this one blocks.
    #[arg(long, value_delimiter = ',')]
    pub(crate) blocks: Vec<String>,
}

#[derive(Args)]
pub(crate) struct EditArgs {
    pub(crate) task: String,
    #[arg(long)]
    pub(crate) title: Option<String>,
    #[arg(short, long)]
    pub(crate) description: Option<String>,
    #[arg(short = 'P', long)]
    pub(crate) priority: Option<i64>,
    /// The deadline, as for `add --due`, or "none".
    #[arg(long, num_args = 1..=3)]
    pub(crate) due: Option<Vec<String>>,
    /// When to work on it, as for `add --on`, or "none".
    #[arg(long, num_args = 1..=3)]
    pub(crate) on: Option<Vec<String>>,
    #[arg(short, long)]
    pub(crate) status: Option<String>,
    /// Move it under this task (e.g. the factory's queue), or "none" to make it top-level.
    #[arg(long)]
    pub(crate) parent: Option<String>,
    /// Fail if the task changed since this version.
    #[arg(long)]
    pub(crate) version: Option<i64>,
}

#[derive(Subcommand)]
pub(crate) enum ProjectCmd {
    Ls {
        #[arg(short, long)]
        all: bool,
    },
    Add {
        key: String,
        name: String,
        #[arg(long)]
        brief: Option<String>,
        #[arg(long)]
        brief_file: Option<PathBuf>,
        /// Agents must send work for review.
        #[arg(long)]
        review: bool,
    },
    Show {
        project: String,
    },
    Edit {
        project: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        brief: Option<String>,
        #[arg(long)]
        brief_file: Option<PathBuf>,
        #[arg(long)]
        review: Option<bool>,
        /// Comma-separated agents that may approve review work here; "" clears it.
        #[arg(long, value_delimiter = ',')]
        reviewers: Option<Vec<String>>,
        #[arg(long)]
        archive: bool,
        #[arg(long)]
        unarchive: bool,
    },
}

#[derive(Subcommand)]
pub(crate) enum ServerCmd {
    /// Is it running, on which binary, and since when.
    Status,
    /// Start it in the background if it isn't running.
    Start,
    /// Stop it gracefully.
    Stop,
    /// Stop it and start it again, e.g. after a rebuild.
    Restart,
}

#[derive(Subcommand)]
pub(crate) enum AgentCmd {
    Ls,
    /// Create an agent. Run as an agent, this creates a sub-agent named
    /// `<agent>/<name>` that can never have more access than its parent.
    Add {
        name: String,
        /// Limit the token to these projects (comma-separated keys).
        #[arg(long, value_delimiter = ',')]
        projects: Vec<String>,
        /// Requests per minute the token may make.
        #[arg(long)]
        rate_limit: Option<i64>,
        /// Print only the token instead of saving it, to hand to another machine's `trailhead login`.
        #[arg(long)]
        print_token: bool,
    },
    /// Revoke an agent and every sub-agent under it.
    Revoke {
        name: String,
    },
}

#[derive(Subcommand)]
pub(crate) enum WebhookCmd {
    Ls,
    /// Send events to URL, signed with a secret shown once.
    Add {
        /// Where to POST events. (Not `url`: that's the global server flag.)
        #[arg(value_name = "URL")]
        target: String,
        /// Comma-separated event kind prefixes, e.g. `task.,comment.created`. Default: everything.
        #[arg(short, long, value_delimiter = ',')]
        kinds: Vec<String>,
    },
    Rm {
        id: String,
    },
    /// Recent delivery attempts.
    Log {
        id: String,
        #[arg(short = 'n', long, default_value_t = 20)]
        limit: i64,
    },
}

#[derive(Subcommand)]
pub(crate) enum TokenCmd {
    Ls,
    /// Issue a token for yourself (or --agent) and print it once, e.g. for
    /// `trailhead login` on another machine. Humans only.
    Add {
        /// What the token is for, e.g. the machine it goes to.
        name: String,
        /// Issue it for this agent of yours instead.
        #[arg(long)]
        agent: Option<String>,
        /// Limit the token to these projects (comma-separated keys).
        #[arg(long, value_delimiter = ',')]
        projects: Vec<String>,
        /// Requests per minute the token may make.
        #[arg(long)]
        rate_limit: Option<i64>,
        /// How long it works for, e.g. 30m, 12h, 7d. Default: until revoked.
        #[arg(long, value_parser = parse_lifetime)]
        expires: Option<i64>,
    },
    Revoke {
        id: String,
    },
    /// Change a token's limits without reissuing it.
    Edit {
        id: String,
        /// Comma-separated project keys; "all" opens it to every project.
        #[arg(long, value_delimiter = ',')]
        projects: Option<Vec<String>>,
        /// Requests per minute, or "none" to remove the limit.
        #[arg(long)]
        rate_limit: Option<String>,
    },
}

/// A lifetime like `90s`, `30m`, `12h` or `7d`, in seconds.
pub(crate) fn parse_lifetime(s: &str) -> Result<i64, String> {
    let s = s.trim();
    let split = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (n, unit) = s.split_at(split);
    let n: i64 = n.parse().map_err(|_| format!("expected a number then s, m, h or d, like 30m: {s:?}"))?;
    let per = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return Err(format!("expected s, m, h or d after the number, like 30m: {s:?}")),
    };
    match n.checked_mul(per) {
        Some(secs) if secs > 0 => Ok(secs),
        _ => Err(format!("lifetime must be positive and sane: {s:?}")),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, Cmd, WebhookCmd, parse_lifetime};

    #[test]
    fn webhook_url_does_not_clobber_the_server_url() {
        let cli = Cli::try_parse_from(["trailhead", "--url", "http://127.0.0.1:1", "webhook", "add", "http://hook/x"])
            .unwrap();
        assert_eq!(cli.url.as_deref(), Some("http://127.0.0.1:1"));
        assert!(matches!(cli.cmd, Cmd::Webhook(WebhookCmd::Add { ref target, .. }) if target == "http://hook/x"));
    }

    #[test]
    fn lifetimes() {
        assert_eq!(parse_lifetime("30m"), Ok(1800));
        assert_eq!(parse_lifetime("7d"), Ok(604_800));
        assert!(parse_lifetime("0h").is_err());
        assert!(parse_lifetime("5").is_err());
        assert!(parse_lifetime("h").is_err());
    }
}
