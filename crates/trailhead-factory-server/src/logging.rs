//! The server's log subscriber when it runs the factory: the server's own lines to `server.log`, the control
//! plane's to the rotating `factory.log`, and a buffer of them for the UI's log view.

use trailhead_client::paths::Paths;

use crate::{configured, logfile};

/// The server's logging when it may run the control plane: the factory's events (its crates, iroh and anything in a
/// `repo` span) go to `factory.log` (when `configured`) and the buffer Lookout reads, everything else to stdout,
/// which the CLI points at `server.log`. `factory_on_stdout` sends the factory's events to stdout as well, for
/// `trailhead factory run` at a terminal.
pub fn init_server_logging(paths: &Paths, default: &str, factory_on_stdout: bool) {
    use tracing_subscriber::fmt::writer::BoxMakeWriter;
    use tracing_subscriber::util::SubscriberInitExt;

    let buffer = LOGS.get_or_init(trailhead_control::logs::LogBuffer::default);
    let file = if configured(paths) { open_log_file() } else { None };
    let file = file.map(|w| BoxMakeWriter::new(std::sync::Mutex::new(w)));
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stdout());
    let stdout = BoxMakeWriter::new(std::io::stdout);
    server_subscriber(trailhead_log::env_filter_or(default), buffer, stdout, ansi, file, factory_on_stdout).init();
}

/// [`init_server_logging`]'s subscriber, with its writers given. Spans pass every layer, so each one sees whether an
/// event is inside a `repo` span; only events are routed.
pub(crate) fn server_subscriber(
    filter: tracing_subscriber::EnvFilter,
    buffer: &trailhead_control::logs::LogBuffer,
    stdout: tracing_subscriber::fmt::writer::BoxMakeWriter,
    ansi: bool,
    file: Option<tracing_subscriber::fmt::writer::BoxMakeWriter>,
    factory_on_stdout: bool,
) -> impl tracing::Subscriber + Send + Sync {
    use tracing_subscriber::filter::{FilterExt, dynamic_filter_fn};
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::{Layer, registry::LookupSpan};

    fn is_factory<S: tracing::Subscriber + for<'a> LookupSpan<'a>>(
        meta: &tracing::Metadata<'_>,
        cx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        const TARGETS: [&str; 7] = [
            "trailhead_factory",
            "trailhead_control",
            "trailhead_wire",
            "trailhead_compute_",
            "trailhead_codehost_",
            "trailhead_provider_",
            "iroh",
        ];
        TARGETS.iter().any(|t| meta.target().starts_with(t))
            || cx.lookup_current().is_some_and(|span| span.scope().any(|s| s.name() == "repo"))
    }
    fn factory<S: tracing::Subscriber + for<'a> LookupSpan<'a>>(
        meta: &tracing::Metadata<'_>,
        cx: &tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        !meta.is_event() || is_factory(meta, cx)
    }

    let stdout =
        tracing_subscriber::fmt::layer().with_timer(trailhead_log::local_timer()).with_ansi(ansi).with_writer(stdout);
    let file = file.map(|w| {
        tracing_subscriber::fmt::layer().with_timer(trailhead_log::local_timer()).with_ansi(false).with_writer(w)
    });
    tracing_subscriber::registry()
        .with(buffer.layer().with_filter(filter.clone().and(dynamic_filter_fn(factory))))
        .with(file.with_filter(filter.clone().and(dynamic_filter_fn(factory))))
        .with(
            stdout.with_filter(filter.and(dynamic_filter_fn(move |meta, cx| {
                !meta.is_event() || factory_on_stdout || !is_factory(meta, cx)
            }))),
        )
}

/// The control plane's log file, if it can be opened.
pub(crate) fn open_log_file() -> Option<logfile::Rotating> {
    let paths = Paths::from_env()?;
    let path = logfile::path(&paths.data_dir);
    match logfile::Rotating::open(&path) {
        Ok(file) => Some(file),
        Err(e) => {
            eprintln!(
                "trailhead: can't open the factory's log file {}: {e}; logging to the terminal only",
                path.display()
            );
            None
        }
    }
}

/// The control plane's recent log, for Lookout's Logs page; filled by the layer `init_server_logging` adds.
pub(crate) static LOGS: std::sync::OnceLock<trailhead_control::logs::LogBuffer> = std::sync::OnceLock::new();

/// [`LOGS`] as the feed `trailhead/ui/0` serves.
pub(crate) struct LogsFeed(pub(crate) trailhead_control::logs::LogBuffer);

impl trailhead_wire::ui::LogFeed for LogsFeed {
    fn subscribe(
        &self,
    ) -> (Vec<trailhead_factory::logs::LogLine>, tokio::sync::broadcast::Receiver<trailhead_factory::logs::LogLine>)
    {
        self.0.subscribe()
    }
}
