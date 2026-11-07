//! The control plane's log, kept in memory for Lookout's Logs page (`docs/factory/ui.md#settings`): a [`LogBuffer`] of the
//! last [`CAPACITY`] events fed by a tracing [`LogLayer`], and a broadcast of each new one. The log file is never read
//! back.
//!
//! Secrets are not logged to begin with. This is a second check on what leaves the machine: fields named like a
//! credential are replaced, and so is anything in a value that looks like a token.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::broadcast;
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use trailhead_factory::logs::{LogLevel, LogLine};

/// How many lines the buffer keeps.
pub const CAPACITY: usize = 2_000;

/// What a redacted value is replaced with.
pub const REDACTED: &str = "[redacted]";

struct Inner {
    lines: VecDeque<LogLine>,
    capacity: usize,
}

/// The last lines of the log, and a feed of new ones.
#[derive(Clone)]
pub struct LogBuffer {
    inner: Arc<Mutex<Inner>>,
    live: broadcast::Sender<LogLine>,
}

impl Default for LogBuffer {
    fn default() -> Self {
        Self::with_capacity(CAPACITY)
    }
}

impl LogBuffer {
    pub fn with_capacity(capacity: usize) -> LogBuffer {
        let capacity = capacity.max(1);
        let (live, _) = broadcast::channel(capacity);
        LogBuffer { inner: Arc::new(Mutex::new(Inner { lines: VecDeque::new(), capacity })), live }
    }

    /// Adds a line, dropping the oldest past the capacity, and sends it to whoever follows.
    pub fn push(&self, line: LogLine) {
        // Sent under the lock, so a `subscribe` sees each line either in its backlog or on its receiver, never both
        // or neither.
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.lines.len() >= inner.capacity {
            inner.lines.pop_front();
        }
        inner.lines.push_back(line.clone());
        let _ = self.live.send(line);
    }

    /// The lines kept, oldest first, and a receiver for each one after them.
    pub fn subscribe(&self) -> (Vec<LogLine>, broadcast::Receiver<LogLine>) {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (inner.lines.iter().cloned().collect(), self.live.subscribe())
    }

    pub fn len(&self) -> usize {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The tracing layer that fills this buffer.
    pub fn layer(&self) -> LogLayer {
        LogLayer { buffer: self.clone() }
    }
}

/// Records each event into a [`LogBuffer`]. Add it to the subscriber after the filter, so only what the log shows is
/// kept.
pub struct LogLayer {
    buffer: LogBuffer,
}

/// The `repo` and `task` a span was created with.
#[derive(Default)]
struct SpanScope {
    repo: Option<String>,
    task: Option<String>,
}

impl Visit for SpanScope {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let text = || redact_value(&format!("{value:?}"));
        match field.name() {
            "repo" => self.repo = Some(text()),
            "task" => self.task = Some(text()),
            _ => {}
        }
    }
}

/// An event's message, its other fields and its task.
#[derive(Default)]
struct EventFields {
    message: String,
    rest: String,
    task: Option<String>,
}

impl Visit for EventFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let name = field.name();
        if name == "message" {
            self.message = redact_value(&format!("{value:?}"));
            return;
        }
        let text = if secret_name(name) { REDACTED.to_string() } else { redact_value(&format!("{value:?}")) };
        if name == "task" {
            self.task = Some(text.clone());
        }
        let _ = write!(self.rest, " {name}={text}");
    }

    // `Display` fields (`%x`) reach `record_debug` as their text, quoted or not; plain strings are unquoted here.
    fn record_str(&mut self, field: &Field, value: &str) {
        let name = field.name();
        let text = if secret_name(name) { REDACTED.to_string() } else { redact_value(value) };
        match name {
            "message" => self.message = text,
            "task" => {
                self.task = Some(text.clone());
                let _ = write!(self.rest, " {name}={text}");
            }
            _ => {
                let _ = write!(self.rest, " {name}={text}");
            }
        }
    }
}

/// Whether a field is named like a credential.
fn secret_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    ["token", "secret", "password", "passwd", "authorization", "api_key", "apikey", "credential", "private_key"]
        .iter()
        .any(|s| name.contains(s))
}

/// Prefixes of the tokens this system handles: Trailhead's own, Anthropic's, GitHub's and the like.
const TOKEN_PREFIXES: [&str; 8] = ["th_", "sk-", "ghp_", "gho_", "ghs_", "ghu_", "github_pat_", "xoxb-"];

/// `value` with anything token-like replaced: a `Bearer` credential, or a word starting with a known token prefix and
/// a run of at least 8 more token characters.
pub fn redact_value(value: &str) -> String {
    let is_token_char = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.';
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while !rest.is_empty() {
        let at_word_start = out.chars().last().is_none_or(|c| !is_token_char(c));
        if at_word_start {
            let lower = rest.to_ascii_lowercase();
            if let Some(after) = lower.strip_prefix("bearer ") {
                let secret =
                    after.chars().take_while(|c| !c.is_whitespace() && *c != '"').map(char::len_utf8).sum::<usize>();
                if secret > 0 {
                    out.push_str("Bearer ");
                    out.push_str(REDACTED);
                    rest = &rest["bearer ".len() + secret..];
                    continue;
                }
            }
            if let Some(prefix) = TOKEN_PREFIXES.iter().find(|p| lower.starts_with(*p)) {
                let body = rest[prefix.len()..].chars().take_while(|c| is_token_char(*c)).count();
                if body >= 8 {
                    out.push_str(REDACTED);
                    rest = &rest[prefix.len() + body..];
                    continue;
                }
            }
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    out
}

impl<S> tracing_subscriber::Layer<S> for LogLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let mut scope = SpanScope::default();
        attrs.record(&mut scope);
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(scope);
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id)
            && let Some(scope) = span.extensions_mut().get_mut::<SpanScope>()
        {
            values.record(scope);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let mut fields = EventFields::default();
        event.record(&mut fields);
        let (mut repo, mut task) = (None, fields.task.clone());
        // Innermost span first, so the nearest repo and task win.
        for span in ctx.event_scope(event).into_iter().flat_map(|s| s.from_root().collect::<Vec<_>>().into_iter().rev())
        {
            if let Some(scope) = span.extensions().get::<SpanScope>() {
                repo = repo.or_else(|| scope.repo.clone());
                task = task.or_else(|| scope.task.clone());
            }
        }
        let meta = event.metadata();
        let ts_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
        self.buffer.push(LogLine {
            ts_ms,
            level: LogLevel::from(meta.level()),
            target: meta.target().to_string(),
            repo,
            task,
            message: format!("{}{}", fields.message, fields.rest),
        });
    }
}

#[cfg(test)]
mod tests {
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    fn line(n: usize) -> LogLine {
        LogLine {
            ts_ms: n as i64,
            level: LogLevel::Info,
            target: "t".into(),
            repo: None,
            task: None,
            message: format!("line {n}"),
        }
    }

    #[test]
    fn the_buffer_keeps_only_the_newest_lines() {
        let buffer = LogBuffer::with_capacity(3);
        for n in 0..5 {
            buffer.push(line(n));
        }
        let (kept, _) = buffer.subscribe();
        assert_eq!(kept.iter().map(|l| l.message.as_str()).collect::<Vec<_>>(), ["line 2", "line 3", "line 4"]);
        assert_eq!(buffer.len(), 3);
    }

    #[tokio::test]
    async fn a_subscriber_gets_the_backlog_then_each_new_line() {
        let buffer = LogBuffer::with_capacity(10);
        buffer.push(line(1));
        let (backlog, mut live) = buffer.subscribe();
        buffer.push(line(2));
        assert_eq!(backlog, vec![line(1)]);
        assert_eq!(live.recv().await.unwrap(), line(2));
    }

    #[test]
    fn events_carry_level_target_repo_task_and_fields() {
        let buffer = LogBuffer::with_capacity(10);
        let subscriber = tracing_subscriber::registry().with(buffer.layer());
        tracing::subscriber::with_default(subscriber, || {
            let _repo = tracing::info_span!("repo", repo = %"lane/trailhead").entered();
            tracing::info!(task = %"TRAIL-7", sandbox = "sb-1", "starting reviewer");
            tracing::warn!("no task here");
        });
        let (lines, _) = buffer.subscribe();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].level, LogLevel::Info);
        assert_eq!(lines[0].repo.as_deref(), Some("lane/trailhead"));
        assert_eq!(lines[0].task.as_deref(), Some("TRAIL-7"));
        assert_eq!(lines[0].message, "starting reviewer task=TRAIL-7 sandbox=sb-1");
        assert!(lines[0].target.contains("logs"), "{}", lines[0].target);
        assert_eq!((lines[1].level, lines[1].task.as_deref()), (LogLevel::Warn, None));
    }

    #[test]
    fn token_like_fields_and_values_never_get_out() {
        let buffer = LogBuffer::with_capacity(10);
        let subscriber = tracing_subscriber::registry().with(buffer.layer());
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(api_token = "abc", error = %"401 with Bearer abc.def-123 sent", "key th_0123456789abcdef used");
        });
        let (lines, _) = buffer.subscribe();
        let text = &lines[0].message;
        assert_eq!(text, "key [redacted] used api_token=[redacted] error=401 with Bearer [redacted] sent");
    }

    #[test]
    fn redaction_survives_non_ascii_after_bearer() {
        assert_eq!(redact_value("Bearer é"), "Bearer [redacted]");
        assert_eq!(redact_value("bearer —x ok"), "Bearer [redacted] ok");
        assert_eq!(redact_value("bearer “x” end"), "Bearer [redacted] end");
    }

    #[test]
    fn redaction_leaves_ordinary_text_alone() {
        for text in ["the th_ prefix alone", "sk-short", "trailhead-factory ready", "bearer"] {
            assert_eq!(redact_value(text), text);
        }
        assert_eq!(redact_value("sk-ant-api03-abcdefghijkl,x"), "[redacted],x");
    }
}
