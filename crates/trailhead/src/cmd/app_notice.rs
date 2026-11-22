//! What the launcher says when Trailhead.app started it. Its output goes to a log file there and nobody reads it, so
//! why the window isn't up is said in a dialog, drawn by macOS's `osascript`.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

/// Set by the bundle's script to the file the launcher's output goes to. Its being set is how the launcher knows the
/// app started it.
pub(crate) const LOG_ENV: &str = "TRAILHEAD_APP_LOG";

#[cfg(any(feature = "factory", test))]
const QUIT: &str = "Quit";
const SHOW_LOG: &str = "Show Log";

/// The launcher's dialogs, at most one on screen. Dropping it takes down the one that is.
pub(crate) struct Notice {
    log: PathBuf,
    shown: Option<Child>,
}

impl Notice {
    /// `Some` when Trailhead.app started this launcher.
    pub(crate) fn from_env() -> Option<Notice> {
        let log = std::env::var_os(LOG_ENV).filter(|log| !log.is_empty())?;
        Some(Notice { log: log.into(), shown: None })
    }

    /// Says why the launcher is waiting, in place of what it said before. The dialog stays up until it is answered or
    /// the wait is over; the launcher carries on behind it, and [`quit`](Self::quit) reads the answer.
    #[cfg(feature = "factory")]
    pub(crate) fn waiting(&mut self, text: &str) {
        self.close();
        let dialog = dialog(text, &[QUIT, SHOW_LOG, "Keep Waiting"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        match dialog {
            Ok(child) => self.shown = Some(child),
            Err(e) => eprintln!("trailhead app: couldn't show a dialog ({e})"),
        }
    }

    /// Whether the dialog was answered with Quit. Show Log opens the log, and the launcher keeps waiting.
    #[cfg(feature = "factory")]
    pub(crate) fn quit(&mut self) -> bool {
        let answered = self.shown.as_mut().is_some_and(|child| matches!(child.try_wait(), Ok(Some(_))));
        if !answered {
            return false;
        }
        let pressed = self.shown.take().and_then(|child| child.wait_with_output().ok());
        match pressed.map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string()).as_deref() {
            Some(QUIT) => true,
            Some(SHOW_LOG) => {
                self.show_log();
                false
            }
            _ => false,
        }
    }

    /// Says why the launcher gave up, and returns once the dialog is answered.
    pub(crate) fn failed(&mut self, text: &str) {
        self.close();
        match dialog(text, &[SHOW_LOG, "OK"]).stdin(Stdio::null()).stderr(Stdio::null()).output() {
            Ok(out) if String::from_utf8_lossy(&out.stdout).trim() == SHOW_LOG => self.show_log(),
            Ok(_) => {}
            Err(e) => eprintln!("trailhead app: couldn't show a dialog ({e})"),
        }
    }

    fn show_log(&self) {
        if let Err(e) = Command::new("open").arg(&self.log).status() {
            eprintln!("trailhead app: couldn't open {} ({e})", self.log.display());
        }
    }

    fn close(&mut self) {
        if let Some(mut child) = self.shown.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Notice {
    fn drop(&mut self) {
        self.close();
    }
}

/// `osascript` showing `text` over `buttons`, the last the default, and printing the one pressed. The text is an
/// argument to the script, not part of it, so nothing in it is read as AppleScript.
fn dialog(text: &str, buttons: &[&str]) -> Command {
    let list = buttons.iter().map(|b| format!("\"{b}\"")).collect::<Vec<_>>().join(", ");
    let show = format!(
        "display dialog (item 1 of argv) with title \"Trailhead\" buttons {{{list}}} default button {}",
        buttons.len()
    );
    let mut command = Command::new("osascript");
    command.args(["-e", "on run argv", "-e", &show, "-e", "return button returned of result", "-e", "end run"]);
    command.arg(text);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dialog_s_text_is_an_argument_and_never_part_of_the_script() {
        let text = "can't reach \"the server\" (timed out)";
        let command = dialog(text, &[QUIT, "Keep Waiting"]);
        let args: Vec<_> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert_eq!(command.get_program(), "osascript");
        assert_eq!(args.last().map(String::as_str), Some(text));
        let script = args[..args.len() - 1].join("\n");
        assert!(!script.contains("reach"), "{script}");
        assert!(script.contains("buttons {\"Quit\", \"Keep Waiting\"} default button 2"), "{script}");
    }
}
