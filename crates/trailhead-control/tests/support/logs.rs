//! Log capture shared by the test binaries that read their own log lines back.

use std::sync::Arc;

/// Log lines the current thread's test has asked to read back.
///
/// A scoped subscriber (`set_default`) can lose to the callsite interest cache when other tests log at the same
/// callsites in parallel with no subscriber, so a test would miss its lines. One global subscriber keeps every
/// callsite enabled; it writes to the `Logs` the thread installed, which is the test's, since these tests run on a
/// current-thread runtime.
#[derive(Clone, Default)]
pub(crate) struct Logs(Arc<std::sync::Mutex<Vec<u8>>>);

thread_local! {
    static CAPTURING: std::cell::RefCell<Option<Logs>> = const { std::cell::RefCell::new(None) };
}

/// Stops the thread's capture when dropped.
pub(crate) struct Capture;

impl Drop for Capture {
    fn drop(&mut self) {
        CAPTURING.with(|c| *c.borrow_mut() = None);
    }
}

#[derive(Clone, Copy)]
struct ThreadLogs;

impl std::io::Write for ThreadLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        CAPTURING.with(|c| {
            if let Some(logs) = &*c.borrow() {
                logs.0.lock().unwrap().extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ThreadLogs {
    type Writer = ThreadLogs;
    fn make_writer(&'a self) -> ThreadLogs {
        *self
    }
}

impl Logs {
    /// Captures this thread's log lines until the returned guard drops.
    ///
    /// Capture is per thread, so it relies on the current-thread runtime `#[tokio::test]` uses by default: the code
    /// under test runs on the test's own thread. On a multi-thread runtime (`flavor = "multi_thread"`) or in a
    /// spawned OS thread, lines logged off the test's thread are not captured.
    pub(crate) fn capture() -> (Logs, Capture) {
        static INSTALL: std::sync::Once = std::sync::Once::new();
        INSTALL.call_once(|| {
            let sub = tracing_subscriber::fmt().with_writer(ThreadLogs).with_ansi(false).finish();
            tracing::subscriber::set_global_default(sub).expect("no other global subscriber in this test binary");
        });
        let logs = Logs::default();
        CAPTURING.with(|c| *c.borrow_mut() = Some(logs.clone()));
        (logs, Capture)
    }

    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
    }
}
