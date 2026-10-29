mod cli;
mod client;
mod cmd;
mod config;
mod dates;
mod output;
mod repo;

use anyhow::{Context, Result};
use clap::Parser;

use cli::{Cli, Cmd};
use client::{ApiError, Client, Identity};
use cmd::{Ctx, access, claims, comments, events, projects, server, tasks, webhooks};

/// Rust ignores SIGPIPE, so `println!` panics once a reader like `head` closes
/// the pipe. By then the command has done its work; exit like a Unix tool killed
/// by SIGPIPE instead of printing a panic.
fn exit_quietly_on_broken_pipe() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let msg = info
            .payload()
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| info.payload().downcast_ref::<&str>().copied())
            .unwrap_or_default();
        let broken_pipe = msg.contains("(os error 32)");
        if msg.starts_with("failed printing to std") && broken_pipe {
            std::process::exit(141);
        }
        default(info);
    }));
}

fn main() {
    exit_quietly_on_broken_pipe();
    let cli = Cli::parse();
    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
    if let Err(e) = rt.block_on(run(cli)) {
        match e.downcast_ref::<ApiError>() {
            Some(api) => eprintln!("{api}"),
            None => eprintln!("error: {e:#}"),
        }
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    if let Cmd::Serve { bind } = &cli.cmd {
        return server::serve(bind).await;
    }
    let paths = trailhead_server::Paths::from_env().context("no home directory")?;
    let url = match cli.url.clone() {
        Some(url) => url,
        None => config::load(&paths)?.url.unwrap_or_else(|| config::DEFAULT_URL.into()),
    };
    match &cli.cmd {
        Cmd::Server(cmd) => return server::server(&url, cmd, cli.json).await,
        Cmd::Login => return access::login(&paths, &url, cli.json).await,
        _ => {}
    }
    let repo = repo::find(&std::env::current_dir()?)?.unwrap_or_default();
    let env_agent = std::env::var("TRAILHEAD_AGENT").ok().filter(|a| !a.is_empty());
    let agent = match (cli.as_me, cli.agent.clone(), env_agent, repo.agent.clone()) {
        (true, ..) => None,
        (false, Some(a), ..) | (false, None, Some(a), _) => Some(Identity { name: a, from: None }),
        (false, None, None, Some(a)) => Some(Identity { name: a, from: Some(repo.path.clone()) }),
        (false, None, None, None) => None,
    };
    let c = Client::connect(&url, cli.token.clone(), agent.as_ref()).await?;
    let cx = Ctx { c, json: cli.json, project: repo.project.clone() };

    match cli.cmd {
        Cmd::Serve { .. } | Cmd::Server(_) | Cmd::Login => unreachable!(),
        Cmd::Ready { project, limit } => tasks::ready(&cx, project, limit).await,
        Cmd::Ls { project, status, parent, all, today, limit } => {
            tasks::ls(&cx, project, status, parent, all, today, limit).await
        }
        Cmd::Show { task } => tasks::show(&cx, &task).await,
        Cmd::Add(a) => tasks::add(&cx, a).await,
        Cmd::Edit(a) => tasks::edit(&cx, a).await,
        Cmd::Claim { task, ttl } => claims::claim(&cx, &task, ttl).await,
        Cmd::Mine => claims::mine(&cx).await,
        Cmd::Renew { task, ttl } => claims::renew(&cx, &task, ttl).await,
        Cmd::Release { task } => claims::release(&cx, &task).await,
        Cmd::Done { task, message } => tasks::finish(&cx, &task, "done", message).await,
        Cmd::Review { task, message } => tasks::finish(&cx, &task, "in_review", message).await,
        Cmd::Approve { task, message } => tasks::finish(&cx, &task, "approved", message).await,
        Cmd::Cancel { task } => tasks::finish(&cx, &task, "cancelled", None).await,
        Cmd::Reopen { task } => tasks::finish(&cx, &task, "open", None).await,
        Cmd::Rm { task } => tasks::rm(&cx, &task).await,
        Cmd::Comment { task, body, kind, to } => comments::comment(&cx, &task, &kind, &body, to).await,
        Cmd::Handoff { task, body } => comments::comment(&cx, &task, "handoff", &body, None).await,
        Cmd::Ask { task, body, to } => comments::comment(&cx, &task, "question", &body, to).await,
        Cmd::Verdict { task, verdict, body, model } => comments::verdict(&cx, &task, &verdict, &body, model).await,
        Cmd::Answer { comment_id, body } => comments::answer(&cx, &comment_id, body).await,
        Cmd::Block { blocker, blocked } => tasks::block(&cx, &blocker, &blocked).await,
        Cmd::Unblock { blocker, blocked } => tasks::unblock(&cx, &blocker, &blocked).await,
        Cmd::Search { query, limit } => tasks::search(&cx, &query, limit).await,
        Cmd::Log { task, limit } => events::log(&cx, task, limit).await,
        Cmd::Watch { task, kinds, after } => events::watch(&cx, task, kinds, after).await,
        Cmd::Mcp => cmd::mcp::bridge(&cx.c).await,
        Cmd::Undo { event_id, batch } => events::undo(&cx, event_id, batch).await,
        Cmd::Revert { actor_id, since } => events::revert(&cx, &actor_id, &since).await,
        Cmd::Project(p) => projects::project(&cx, p).await,
        Cmd::Agent(a) => access::agent(&cx, a).await,
        Cmd::Token(t) => access::token(&cx, t).await,
        Cmd::Webhook(w) => webhooks::webhook(&cx, w).await,
        Cmd::Whoami => access::whoami(&cx).await,
    }
}
