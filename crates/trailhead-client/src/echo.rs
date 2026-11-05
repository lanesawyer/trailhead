//! Keeps a typed or pasted secret off the screen while it is read from a terminal.

/// While alive, stdin's terminal doesn't echo what's typed; dropping it restores the old settings.
/// Does nothing when stdin isn't a terminal, or off Unix.
pub struct NoEcho {
    #[cfg(unix)]
    saved: Option<libc::termios>,
}

impl NoEcho {
    #[cfg(unix)]
    pub fn new() -> Self {
        // SAFETY: `tcgetattr` fills a zeroed termios for fd 0 and fails on a non-terminal; `tcsetattr` only reads one.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut t) != 0 {
                return Self { saved: None };
            }
            let mut quiet = t;
            quiet.c_lflag &= !libc::ECHO;
            // Keep echoing the newline so the shell prompt starts on a fresh line.
            quiet.c_lflag |= libc::ECHONL;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &quiet) != 0 {
                return Self { saved: None };
            }
            Self { saved: Some(t) }
        }
    }

    #[cfg(not(unix))]
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for NoEcho {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(unix)]
impl Drop for NoEcho {
    fn drop(&mut self) {
        if let Some(t) = &self.saved {
            // SAFETY: restores the settings read in `new`.
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, t);
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn not_a_terminal_is_a_no_op() {
        // Under `cargo test` stdin is not a terminal, so there is nothing to change or restore.
        let guard = NoEcho::new();
        assert!(guard.saved.is_none() || std::io::IsTerminal::is_terminal(&std::io::stdin()));
    }
}
