//! The sound the bell's ring makes in the window (`docs/app-platform.md#notifications`, "A sound"): the desktop theme's
//! own message sound, which the desktop plays. The shell ships no sound of its own, and the theme's are made by people
//! (on Ubuntu, Yaru's, by Mads Rosendahl under CC-BY-SA-4.0). A desktop notification asks the desktop for the same
//! sound itself ([`crate::desktop`]).
//!
//! It is played through a [`Speaker`]: on Linux, `canberra-gtk-play` started on a thread of its own, so a frame never
//! waits on it, and silent where the program is missing; a fake in tests. A shell with no [`Sound`] plays nothing.

use bevy::prelude::*;

/// A way to play the message sound. It may not block.
pub trait Speaker: Send + Sync + 'static {
    fn play(&self);
}

/// What the shell plays the message sound through.
#[derive(Resource)]
pub struct Sound(pub Box<dyn Speaker>);

impl Sound {
    /// This machine's, if the shell knows how to play the desktop theme's sound on it.
    pub fn of_this_machine() -> Option<Sound> {
        #[cfg(target_os = "linux")]
        {
            Some(Sound(Box::new(canberra::ThemeSound)))
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }
}

/// The desktop theme's sounds, through libcanberra's player.
#[cfg(target_os = "linux")]
mod canberra {
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::Speaker;

    /// The player, from `gnome-session-canberra` or `libcanberra-gtk3-module`, which Ubuntu's desktop has.
    const PLAYER: &str = "canberra-gtk-play";
    /// The freedesktop sound name of a message arriving, which the theme (Yaru on Ubuntu) supplies.
    const SOUND: &str = "message-new-instant";

    /// Said once, so a missing player is in the log without filling it.
    static MISSING: AtomicBool = AtomicBool::new(false);

    pub(super) struct ThemeSound;

    impl Speaker for ThemeSound {
        fn play(&self) {
            let started = std::thread::Builder::new().name("message-sound".into()).spawn(|| {
                let child = Command::new(PLAYER)
                    .args(["-i", SOUND])
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
                match child {
                    // Waited on here, off the frame, so it leaves nothing behind when it ends.
                    Ok(mut child) => {
                        let _ = child.wait();
                    }
                    Err(error) => {
                        if !MISSING.swap(true, Ordering::Relaxed) {
                            tracing::info!(%error, "no {PLAYER}, so the bell rings without a sound");
                        }
                    }
                }
            });
            if let Err(error) = started {
                tracing::warn!(%error, "couldn't start the thread that plays the message sound");
            }
        }
    }
}
