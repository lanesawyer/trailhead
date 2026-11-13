//! What Lookout's forms share: one in a modal at a time, its save in flight, and what it says about it. The task form
//! ([`crate::author`]) and the project form ([`crate::project_form`]) each add what they hold and how it is drawn.

use bevy::prelude::*;

use crate::commands::{Request, Submit};
use crate::widgets::{ModalClosed, ModalLocked};

/// The form on screen, holding an `O` while it is open. `generation` changes whenever it opens or closes, which is
/// when its modal is redrawn (redrawing for every keystroke would lose the cursor).
#[derive(Resource, Debug)]
pub struct FormState<O> {
    pub open: Option<O>,
    /// The last refusal, or what is in progress.
    pub message: String,
    /// The save awaiting its answer, so it can't be sent twice.
    pub saving: bool,
    pub generation: u64,
}

impl<O> Default for FormState<O> {
    fn default() -> Self {
        FormState { open: None, message: String::new(), saving: false, generation: 0 }
    }
}

impl<O> FormState<O> {
    pub fn open(&mut self, open: O) {
        self.open = Some(open);
        self.reset();
    }

    pub fn close(&mut self) {
        self.open = None;
        self.reset();
    }

    fn reset(&mut self) {
        self.message.clear();
        self.saving = false;
        self.generation += 1;
    }

    /// Sends `request` from the form's modal, `source`, or says why it can't be sent: a save is already out, the
    /// server isn't connected (`doing` is what then waits, "saving" say), or the form doesn't make a request yet.
    pub fn send(
        &mut self,
        request: Result<Request, String>,
        doing: &str,
        live: bool,
        source: Option<Entity>,
        submit: &mut MessageWriter<Submit>,
    ) {
        if self.saving {
            return;
        }
        if !live {
            self.message = format!("Not connected to the server; {doing} waits for Live.");
            return;
        }
        match request {
            Err(why) => self.message = why,
            Ok(request) => {
                let Some(source) = source else { return };
                submit.write(Submit { source, request });
                self.saving = true;
                self.message = "Saving...".into();
            }
        }
    }

    /// The save was refused or failed: the form stays, saying `why`, and can be saved again.
    pub fn refuse(&mut self, why: String) {
        self.saving = false;
        self.message = why;
    }
}

/// A close request (the X, Escape, a click outside) on the modal marked `P` closes the form too; the modal is
/// already going.
pub fn on_modal_closed<O: Send + Sync + 'static, P: Component>(
    closed: On<ModalClosed>,
    panels: Query<(), With<P>>,
    mut form: ResMut<FormState<O>>,
) {
    if panels.contains(closed.modal) {
        form.close();
    }
}

/// A save in flight can't be withdrawn, so the form's modal, marked `P`, stays open until it is answered.
pub fn lock_while_saving<O: Send + Sync + 'static, P: Component>(
    form: Res<FormState<O>>,
    panels: Query<Entity, With<P>>,
    mut commands: Commands,
) {
    if !form.is_changed() {
        return;
    }
    for e in &panels {
        if form.saving {
            commands.entity(e).insert(ModalLocked);
        } else {
            commands.entity(e).remove::<ModalLocked>();
        }
    }
}

/// Keeps the text marked `M` saying the form's message.
pub fn update_message<O: Send + Sync + 'static, M: Component>(
    form: Res<FormState<O>>,
    mut messages: Query<&mut Text, With<M>>,
) {
    if !form.is_changed() {
        return;
    }
    for mut text in &mut messages {
        if text.0 != form.message {
            text.0.clone_from(&form.message);
        }
    }
}
