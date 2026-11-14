//! Keeps a typed or pasted secret off the screen while it is read from a terminal.

/// While alive, stdin's terminal doesn't echo what's typed; dropping it restores the old settings.
/// Ctrl-C while it's alive restores them too, before the process dies of SIGINT.
/// Does nothing when stdin isn't a terminal, or off Unix.
pub struct NoEcho {
    #[cfg(unix)]
    saved: Option<libc::termios>,
    #[cfg(unix)]
    old_sigint: Option<libc::sigaction>,
}

/// The settings the SIGINT handler restores; only read once `HANDLER_ARMED` is set.
#[cfg(unix)]
struct SavedTermios(std::cell::UnsafeCell<std::mem::MaybeUninit<libc::termios>>);

// SAFETY: written only before `HANDLER_ARMED` is set (and the handler installed), read only after.
#[cfg(unix)]
unsafe impl Sync for SavedTermios {}

#[cfg(unix)]
static HANDLER_TERMIOS: SavedTermios = SavedTermios(std::cell::UnsafeCell::new(std::mem::MaybeUninit::uninit()));
#[cfg(unix)]
static HANDLER_ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Puts the terminal back, then dies of SIGINT as it would have. Only async-signal-safe calls.
#[cfg(unix)]
extern "C" fn restore_on_sigint(_: libc::c_int) {
    if HANDLER_ARMED.load(std::sync::atomic::Ordering::Acquire) {
        // SAFETY: `HANDLER_ARMED` means the termios was written and is no longer changed.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, (*HANDLER_TERMIOS.0.get()).as_ptr());
        }
    }
    // SAFETY: `signal` and `raise` are async-signal-safe.
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        libc::raise(libc::SIGINT);
    }
}

/// Installs `restore_on_sigint` and returns the disposition it replaced. Returns `None`, leaving
/// SIGINT alone, when it was ignored (`nohup`, a background job): Ctrl-C did nothing before, so it must not kill now.
#[cfg(unix)]
fn install_sigint_handler() -> Option<libc::sigaction> {
    // SAFETY: plain `sigaction` calls on zeroed structs; a null `act` only reads the current disposition.
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGINT, std::ptr::null(), &mut old) != 0 || old.sa_sigaction == libc::SIG_IGN {
            return None;
        }
        let mut act: libc::sigaction = std::mem::zeroed();
        act.sa_sigaction = restore_on_sigint as extern "C" fn(libc::c_int) as usize;
        libc::sigemptyset(&mut act.sa_mask);
        (libc::sigaction(libc::SIGINT, &act, &mut old) == 0).then_some(old)
    }
}

impl NoEcho {
    #[cfg(unix)]
    pub fn new() -> Self {
        // SAFETY: `tcgetattr` fills a zeroed termios for fd 0 and fails on a non-terminal; `tcsetattr` only reads one.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut t) != 0 {
                return Self { saved: None, old_sigint: None };
            }
            let mut quiet = t;
            quiet.c_lflag &= !libc::ECHO;
            // Keep echoing the newline so the shell prompt starts on a fresh line.
            quiet.c_lflag |= libc::ECHONL;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSAFLUSH, &quiet) != 0 {
                return Self { saved: None, old_sigint: None };
            }
            // Ctrl-C would otherwise kill the process before `Drop` runs and leave echo off.
            // Write the termios first: the handler may run as soon as it is installed.
            (*HANDLER_TERMIOS.0.get()).write(t);
            HANDLER_ARMED.store(true, std::sync::atomic::Ordering::Release);
            let old_sigint = install_sigint_handler();
            if old_sigint.is_none() {
                HANDLER_ARMED.store(false, std::sync::atomic::Ordering::Release);
            }
            Self { saved: Some(t), old_sigint }
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
            // SAFETY: restores the settings read in `new` and the SIGINT disposition it replaced.
            unsafe {
                if let Some(old) = &self.old_sigint {
                    libc::sigaction(libc::SIGINT, old, std::ptr::null_mut());
                }
                HANDLER_ARMED.store(false, std::sync::atomic::Ordering::Release);
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
        assert!(guard.saved.is_some() || guard.old_sigint.is_none());
    }

    #[test]
    fn ignored_sigint_stays_ignored() {
        // The only test that touches SIGINT's disposition, so nothing races it.
        // SAFETY: plain `sigaction` calls; the original disposition is put back at the end.
        unsafe {
            let mut ign: libc::sigaction = std::mem::zeroed();
            ign.sa_sigaction = libc::SIG_IGN;
            libc::sigemptyset(&mut ign.sa_mask);
            let mut orig: libc::sigaction = std::mem::zeroed();
            assert_eq!(libc::sigaction(libc::SIGINT, &ign, &mut orig), 0);

            assert!(install_sigint_handler().is_none());
            let mut now: libc::sigaction = std::mem::zeroed();
            libc::sigaction(libc::SIGINT, std::ptr::null(), &mut now);
            assert_eq!(now.sa_sigaction, libc::SIG_IGN);

            libc::sigaction(libc::SIGINT, &orig, std::ptr::null_mut());
        }
    }

    #[test]
    fn handler_without_saved_settings_is_inert() {
        // Disarmed, the handler must not touch the terminal; we only check the flag the handler reads.
        assert!(
            !HANDLER_ARMED.load(std::sync::atomic::Ordering::Acquire)
                || std::io::IsTerminal::is_terminal(&std::io::stdin())
        );
    }
}
