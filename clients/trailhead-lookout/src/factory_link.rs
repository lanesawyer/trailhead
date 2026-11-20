//! Lookout's own work on the net thread: the control plane's status stream (`docs/factory/ui.md#factory-status`) and
//! its log (`#settings`), each on its own connection to `trailhead/ui/0`. Both are [`LinkJob`]s, so they run while a
//! control plane is held and go with it. They report over Lookout's own channels, which [`FactoryLinkPlugin`] drains
//! into the [`FactoryItem`]s and [`LogItem`]s the factory mirror and the Logs page read.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Mutex, mpsc};

use bevy::prelude::*;
use iroh::endpoint::ConnectionError;
use iroh::{Endpoint, EndpointId};
use tokio::sync::watch;
use trailhead_shell::connection::SendNet;
use trailhead_shell::net::{Backoff, HeldLink, LinkJob, NetCommand, Reason};
use trailhead_wire::ui::{CLOSE_NOT_ALLOWED, LogFrame, LogStream, StatusFrame, StatusStream, dial_ui};

use crate::factory::{FactoryItem, FactoryUpdate, apply_factory};
use crate::logs::{FollowLogs, LogItem, LogUpdate};

/// What the Logs page says of a factory that doesn't send logs.
pub const NO_LOGS: &str = "this factory is too old to send logs";

/// Lookout's jobs for the net thread, and the plugin that takes what they report. The jobs go to
/// [`trailhead_shell::connection::ConnectionPlugin::with_jobs`].
pub fn factory_link() -> (FactoryLinkPlugin, Vec<Box<dyn LinkJob>>) {
    let (status_tx, status_rx) = mpsc::channel();
    let (logs_tx, logs_rx) = mpsc::channel();
    let (follow_tx, follow_rx) = watch::channel(false);
    let ends = Ends { status: Mutex::new(status_rx), logs: Mutex::new(logs_rx), follow: follow_tx };
    let jobs: Vec<Box<dyn LinkJob>> =
        vec![Box::new(StatusJob { events: status_tx }), Box::new(LogsJob { events: logs_tx, wanted: follow_rx })];
    (FactoryLinkPlugin { ends: Mutex::new(Some(ends)) }, jobs)
}

/// Lookout's end of its jobs' channels.
#[derive(Resource)]
struct Ends {
    status: Mutex<mpsc::Receiver<FactoryUpdate>>,
    logs: Mutex<mpsc::Receiver<LogUpdate>>,
    /// Whether the log is followed, as the Logs page last asked.
    follow: watch::Sender<bool>,
}

/// Takes what [`factory_link`]'s jobs report, and passes the Logs page's asks to them.
pub struct FactoryLinkPlugin {
    ends: Mutex<Option<Ends>>,
}

impl Plugin for FactoryLinkPlugin {
    fn build(&self, app: &mut App) {
        let ends = self.ends.lock().expect("factory link lock").take().expect("the factory link is built once");
        app.insert_resource(ends)
            .add_message::<FactoryItem>()
            .add_message::<LogItem>()
            .add_message::<FollowLogs>()
            .add_message::<SendNet>()
            .add_systems(
                PreUpdate,
                ((unreachable_on_disconnect, drain_status).chain().before(apply_factory), drain_logs, pass_log_asks),
            );
    }
}

/// The status stream goes with the control plane; the mirror keeps its last state, marked unreachable.
fn unreachable_on_disconnect(mut sent: MessageReader<SendNet>, mut factory: MessageWriter<FactoryItem>) {
    for SendNet(command) in sent.read() {
        if matches!(command, NetCommand::Disconnect) {
            factory.write(FactoryItem(FactoryUpdate::Unreachable));
        }
    }
}

fn drain_status(ends: Res<Ends>, mut factory: MessageWriter<FactoryItem>) {
    let rx = ends.status.lock().expect("status lock");
    for update in rx.try_iter() {
        factory.write(FactoryItem(update));
    }
}

fn drain_logs(ends: Res<Ends>, mut logs: MessageWriter<LogItem>) {
    let rx = ends.logs.lock().expect("logs lock");
    for update in rx.try_iter() {
        logs.write(LogItem(update));
    }
}

fn pass_log_asks(ends: Res<Ends>, mut asks: MessageReader<FollowLogs>) {
    for FollowLogs(on) in asks.read() {
        // Sent even when unchanged: each ask starts the log over, with its backlog.
        ends.follow.send_replace(*on);
    }
}

type Job = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Follows the control plane's status stream, redialing until it is back, and says when it can't be reached.
struct StatusJob {
    events: mpsc::Sender<FactoryUpdate>,
}

impl LinkJob for StatusJob {
    fn start(&mut self, link: HeldLink) -> Job {
        Box::pin(follow_status_until_let_go(link, self.events.clone()))
    }

    /// Forget everything heard from the old control plane, so the map doesn't show its state.
    fn released(&mut self) {
        let _ = self.events.send(FactoryUpdate::Reset);
    }
}

/// Follows the log while the Logs page is open.
struct LogsJob {
    events: mpsc::Sender<LogUpdate>,
    wanted: watch::Receiver<bool>,
}

impl LinkJob for LogsJob {
    fn start(&mut self, link: HeldLink) -> Job {
        Box::pin(logs_while_wanted(link, self.wanted.clone(), self.events.clone()))
    }
}

/// Why the factory won't serve the status stream.
enum Refusal {
    /// It doesn't know the request: older than this Lookout.
    TooOld(String),
    /// It knows the request and says no.
    Refused(String),
}

/// What the status task has seen of the current connection.
#[derive(Default)]
struct Progress {
    /// Whether the app was last told the factory is stopped; cleared by anything heard.
    stopped: bool,
    /// Whether the connection has served a frame, which means the factory isn't refusing us.
    heard: bool,
}

/// How the status task treats the end of a connection.
#[derive(Debug, PartialEq, Eq)]
enum Resync {
    /// Not a resync: a drop, reported and retried after a gap.
    No,
    /// The first clean end after serving a snapshot: redial at once, report nothing.
    Now,
    /// A clean end right after the last resync: redial after a gap, still reporting nothing.
    Slowly,
}

/// Whether a connection's end is a resync, given whether it ended cleanly after being heard, whether the one
/// before did, and whether this one was brief (it ended within a retry gap of being dialed).
fn resync_after(clean_and_heard: bool, previous_was_resync: bool, brief: bool) -> Resync {
    match (clean_and_heard, previous_was_resync && brief) {
        (false, _) => Resync::No,
        (true, false) => Resync::Now,
        (true, true) => Resync::Slowly,
    }
}

async fn follow_status_until_let_go(link: HeldLink, events: mpsc::Sender<FactoryUpdate>) {
    let HeldLink { endpoint, id, retry } = link;
    let mut backoff = Backoff::new(retry);
    let mut progress = Progress::default();
    let mut resynced = false;
    loop {
        progress.heard = false;
        let dialed = tokio::time::Instant::now();
        let followed = follow_status(&endpoint, id, &events, &mut progress).await;
        let brief = dialed.elapsed() < backoff.dropped();
        if progress.heard {
            backoff.reset();
        }
        let ended_cleanly = followed.is_ok();
        let refusal = match followed {
            Ok(refusal) => refusal,
            Err(e) => {
                tracing::debug!(error = %e, "the factory status stream ended");
                None
            }
        };
        if let Some(refusal) = refusal {
            let wait = backoff.refused();
            let retry = format!("trying again in {} s", wait.as_secs().max(1));
            progress.stopped = false;
            let update = match refusal {
                Refusal::TooOld(why) => FactoryUpdate::TooOld(format!("{why}; {retry}")),
                Refusal::Refused(why) => FactoryUpdate::Refused(format!("{why}; {retry}")),
            };
            if events.send(update).is_err() {
                return;
            }
            tokio::time::sleep(wait).await;
            continue;
        }
        // A clean end is the server asking for a resync (its feed changed, or we lagged), not a drop:
        // redial at once and don't report Unreachable; the next snapshot says what changed.
        // A clean end right after a resync, on a connection that barely lasted, is a server that ends
        // every stream (one from before TRAIL-761 while stopped), so it waits like a drop, without
        // reporting Unreachable. One that stayed open and then ended is a new resync, however the
        // connection before it ended.
        let resync = resync_after(ended_cleanly && progress.heard, resynced, brief);
        resynced = resync != Resync::No;
        if resync == Resync::Now {
            continue;
        }
        if resync == Resync::No && !progress.stopped {
            progress.stopped = true;
            if events.send(FactoryUpdate::Unreachable).is_err() {
                return;
            }
        }
        tokio::time::sleep(backoff.dropped()).await;
    }
}

/// One connection's worth of the status stream: the snapshot, then events until it ends. Returns why the factory
/// refused, if it did.
async fn follow_status(
    endpoint: &Endpoint,
    id: EndpointId,
    events: &mpsc::Sender<FactoryUpdate>,
    progress: &mut Progress,
) -> Result<Option<Refusal>, trailhead_factory::Error> {
    let conn = dial_ui(endpoint, id).await?;
    match stream_status(conn.clone(), events, progress).await {
        Err(e) => match refusal_of(&conn, &e, progress.heard) {
            Some(refusal) => Ok(Some(refusal)),
            None => Err(e),
        },
        done => done,
    }
}

async fn stream_status(
    conn: iroh::endpoint::Connection,
    events: &mpsc::Sender<FactoryUpdate>,
    progress: &mut Progress,
) -> Result<Option<Refusal>, trailhead_factory::Error> {
    let mut stream = StatusStream::open(conn).await?;
    while let Some(frame) = stream.next().await? {
        let update = match frame {
            StatusFrame::Snapshot { control, repos } => {
                if events.send(FactoryUpdate::Control(control)).is_err() {
                    return Ok(None);
                }
                FactoryUpdate::Snapshot(repos)
            }
            StatusFrame::Event { repo, event } => FactoryUpdate::Event { repo, event },
            StatusFrame::Unsupported(unsupported) => return Ok(Some(Refusal::TooOld(too_old_message(&unsupported)))),
        };
        progress.stopped = false;
        progress.heard = true;
        if events.send(update).is_err() {
            return Ok(None);
        }
    }
    Ok(None)
}

/// Whether a failed stream was the factory saying no, rather than the link dropping: the connection closed because
/// this endpoint isn't allowed, or the stream reset before anything was served.
fn refusal_of(conn: &iroh::endpoint::Connection, error: &trailhead_factory::Error, heard: bool) -> Option<Refusal> {
    match conn.close_reason() {
        Some(ConnectionError::ApplicationClosed(c)) if c.error_code == CLOSE_NOT_ALLOWED.into() => {
            Some(Refusal::Refused(Reason::NotAllowed.to_string()))
        }
        Some(_) => None,
        None if !heard && error.to_string().contains("stream reset by peer") => {
            Some(Refusal::Refused("the factory refused the status request".into()))
        }
        None => None,
    }
}

/// What the map says of a factory that didn't know our request: what it was asked, what it serves, and its build.
fn too_old_message(u: &trailhead_wire::ui::Unsupported) -> String {
    // Only the op: a request that carries parameters would make the label long.
    let asked = u
        .got
        .get("op")
        .and_then(|op| op.as_str())
        .map_or_else(|| "an unreadable request".to_string(), |op| format!("\"{op}\""));
    format!(
        "the factory is older than this Lookout: reinstall trailhead and restart its server (it was asked {}, and \
         serves {}; version {}, build {})",
        asked,
        u.known.join(", "),
        u.version,
        u.build
    )
}

/// Follows the log while it is wanted, starting it over on every ask, for as long as the control plane is held.
async fn logs_while_wanted(link: HeldLink, mut wanted: watch::Receiver<bool>, events: mpsc::Sender<LogUpdate>) {
    enum Then {
        StartOver,
        AwaitAsk,
        Stop,
    }
    loop {
        let on = *wanted.borrow_and_update();
        let then = tokio::select! {
            asked = wanted.changed() => if asked.is_ok() { Then::StartOver } else { Then::Stop },
            open = follow_logs_retrying(&link, &events, on) => if open { Then::AwaitAsk } else { Then::Stop },
        };
        match then {
            Then::StartOver => {}
            Then::AwaitAsk if wanted.changed().await.is_ok() => {}
            Then::AwaitAsk | Then::Stop => return,
        }
    }
}

/// Follows the log, redialing when it drops, until the factory turns out not to serve one. Never returns while `on`
/// is false. Returns whether Lookout is still listening.
async fn follow_logs_retrying(link: &HeldLink, events: &mpsc::Sender<LogUpdate>, on: bool) -> bool {
    if !on {
        return std::future::pending().await;
    }
    loop {
        match follow_logs(&link.endpoint, link.id, events).await {
            Ok(Some(why)) => return events.send(LogUpdate::TooOld(why)).is_ok(),
            Ok(None) => {}
            Err(e) => tracing::debug!(error = %e, "the factory log stream ended"),
        }
        if events.send(LogUpdate::Dropped).is_err() {
            return false;
        }
        tokio::time::sleep(link.retry).await;
    }
}

/// One connection's worth of the log stream. Returns the message to show when the factory doesn't serve logs.
async fn follow_logs(
    endpoint: &Endpoint,
    id: EndpointId,
    events: &mpsc::Sender<LogUpdate>,
) -> Result<Option<String>, trailhead_factory::Error> {
    let mut stream = LogStream::open(dial_ui(endpoint, id).await?).await?;
    let mut heard = false;
    loop {
        let frame = match stream.next().await {
            Ok(Some(frame)) => frame,
            Ok(None) => return Ok(None),
            // Reset before anything was served: a control plane that has no log feed.
            Err(e) if !heard && e.to_string().contains("stream reset by peer") => return Ok(Some(NO_LOGS.into())),
            Err(e) => return Err(e),
        };
        heard = true;
        let update = match frame {
            LogFrame::Backlog { lines } => LogUpdate::Backlog(lines),
            LogFrame::Line(line) => LogUpdate::Line(line),
            LogFrame::Skipped { count } => LogUpdate::Skipped(count),
            LogFrame::Unsupported(_) => return Ok(Some(NO_LOGS.into())),
        };
        if events.send(update).is_err() {
            return Ok(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn too_old_message_names_only_the_op() {
        let u = trailhead_wire::ui::Unsupported {
            got: serde_json::json!({"op": "logs", "task": "x".repeat(500)}),
            known: vec!["status".into()],
            version: "1".into(),
            build: "b".into(),
        };
        let message = too_old_message(&u);
        assert!(message.contains("asked \"logs\","), "{message}");
        assert!(message.len() < 300, "{message}");
    }

    #[test]
    fn brief_clean_ends_in_a_row_are_slowed_down() {
        assert_eq!(resync_after(true, false, true), Resync::Now);
        assert_eq!(resync_after(true, true, true), Resync::Slowly);
        assert_eq!(resync_after(true, true, false), Resync::Now, "a connection that stayed open resyncs at once");
        assert_eq!(resync_after(false, true, true), Resync::No, "a drop resets the run");
        assert_eq!(resync_after(false, false, false), Resync::No);
    }

    #[test]
    fn letting_go_of_the_control_plane_resets_the_factory_mirror() {
        let (tx, rx) = mpsc::channel();
        StatusJob { events: tx }.released();
        assert_eq!(rx.try_recv(), Ok(FactoryUpdate::Reset));
    }

    #[test]
    fn a_disconnect_marks_the_factory_unreachable_at_once() {
        use crate::factory::{FactoryLink, FactoryPlugin, FactoryStatus};

        let mut app = App::new();
        let (plugin, _jobs) = factory_link();
        app.add_plugins(MinimalPlugins).add_plugins((plugin, FactoryPlugin));
        app.world_mut().resource_mut::<FactoryStatus>().link = FactoryLink::Running;
        app.world_mut().write_message(SendNet(NetCommand::Disconnect));
        app.update();
        assert_eq!(app.world().resource::<FactoryStatus>().link, FactoryLink::Unreachable);
    }
}
