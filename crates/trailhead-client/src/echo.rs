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
            (*HANDLER_TERMIOS.0.get()).write(t);
            HANDLER_ARMED.store(true, std::sync::atomic::Ordering::Release);
            let mut act: libc::sigaction = std::mem::zeroed();
            act.sa_sigaction = restore_on_sigint as extern "C" fn(libc::c_int) as usize;
            libc::sigemptyset(&mut act.sa_mask);
            let mut old: libc::sigaction = std::mem::zeroed();
            let old_sigint = (libc::sigaction(libc::SIGINT, &act, &mut old) == 0).then_some(old);
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

    /// ECHO state of the terminal behind a pty master (Linux reports the slave's settings).
    fn echo_on(master: libc::c_int) -> bool {
        // SAFETY: `tcgetattr` fills a zeroed termios for a valid fd.
        unsafe {
            let mut t: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(master, &mut t), 0);
            t.c_lflag & libc::ECHO != 0
        }
    }

    #[test]
    fn ctrl_c_on_a_terminal_restores_echo_and_dies_of_sigint() {
        // SAFETY: the child only makes plain libc calls before `_exit` or dying of the signal.
        unsafe {
            let (mut master, mut slave) = (0, 0);
            let none = std::ptr::null_mut();
            assert_eq!(libc::openpty(&mut master, &mut slave, none, none.cast(), none.cast()), 0);
            // The child writes one byte once `NoEcho::new` has returned with the handler armed.
            let mut ready = [0; 2];
            assert_eq!(libc::pipe(ready.as_mut_ptr()), 0);
            let pid = libc::fork();
            assert!(pid >= 0);
            if pid == 0 {
                // Child: the pty is its controlling terminal and stdin, then it waits for Ctrl-C.
                // The alarm keeps a failed run from leaking the child.
                libc::alarm(30);
                libc::setsid();
                libc::ioctl(slave, libc::TIOCSCTTY, 0);
                libc::dup2(slave, libc::STDIN_FILENO);
                let guard = NoEcho::new();
                if guard.saved.is_none() {
                    libc::_exit(2);
                }
                libc::write(ready[1], b"r".as_ptr().cast(), 1);
                loop {
                    libc::sleep(1);
                }
            }
            libc::close(slave);
            libc::close(ready[1]);
            let fail = |msg: &str| -> ! {
                libc::kill(pid, libc::SIGKILL);
                libc::waitpid(pid, std::ptr::null_mut(), 0);
                panic!("{msg}");
            };
            // Block until the child is armed; EOF means it exited without arming.
            let mut byte = 0u8;
            if libc::read(ready[0], std::ptr::addr_of_mut!(byte).cast(), 1) != 1 {
                fail("child exited before arming the SIGINT handler");
            }
            libc::close(ready[0]);
            if echo_on(master) {
                fail("child armed but echo is still on");
            }
            assert_eq!(libc::kill(pid, libc::SIGINT), 0);
            let mut status = 0;
            assert_eq!(libc::waitpid(pid, &mut status, 0), pid);
            assert!(libc::WIFSIGNALED(status), "child exited instead of dying of a signal: {status}");
            assert_eq!(libc::WTERMSIG(status), libc::SIGINT);
            assert!(echo_on(master), "echo was not restored");
            libc::close(master);
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
