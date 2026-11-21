//! An app's view, kept in the [`LocalStore`] and restored when the app is shown again.
//!
//! An app's views are a sub-state of [`ActiveApp`], which exists only while the app is shown, so it starts on its
//! default every time. Setting the kept view from `OnEnter(ActiveApp(..))` draws the default for a frame first (the
//! sub-state is created in the same transition, before that schedule runs), and setting it any earlier is dropped
//! (Bevy takes a sub-state's `NextState` and throws it away while the sub-state doesn't exist).
//!
//! [`RememberView::add_remembered_view`] sets the kept view in the `StateTransition` schedule just before the
//! transition that brings the app in is applied ([`StateTransitionSystems::DependentTransitions`]), which Bevy then
//! creates the sub-state with in place of its default. So the kept view is the first whose `OnEnter` runs, on the
//! first start as on every switch back. After the enter schedules, a changed view is written back to the store.

use std::marker::PhantomData;

use bevy::prelude::*;
use bevy::state::state::{StateTransition, StateTransitionSystems, SubStates};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{ActiveApp, AppId, LocalStore};

/// Where an app's remembered view is kept in its part of the [`LocalStore`].
pub const VIEW: &str = "view";

/// Adds an app's views as a sub-state the shell remembers.
pub trait RememberView {
    /// Adds `V`, the views of the app `app`, as a sub-state of [`ActiveApp`] (as `add_sub_state` would) that starts,
    /// each time the app is shown, on the view it was last on, kept in the [`LocalStore`] under [`VIEW`]. Call it in
    /// [`ShellApp::build`](crate::ShellApp::build), so it holds headless too; one sub-state an app.
    fn add_remembered_view<V>(&mut self, app: AppId) -> &mut Self
    where
        V: SubStates<SourceStates = ActiveApp> + Serialize + DeserializeOwned;
}

impl RememberView for App {
    fn add_remembered_view<V>(&mut self, app: AppId) -> &mut Self
    where
        V: SubStates<SourceStates = ActiveApp> + Serialize + DeserializeOwned,
    {
        self.add_sub_state::<V>().insert_resource(ViewsOf::<V>(app, PhantomData)).add_systems(
            StateTransition,
            (
                restore::<V>.before(StateTransitionSystems::DependentTransitions),
                keep::<V>.after(StateTransitionSystems::EnterSchedules),
            ),
        )
    }
}

/// The app whose views `V` are.
#[derive(Resource)]
struct ViewsOf<V>(AppId, PhantomData<fn() -> V>);

/// Sets the kept view as the next one when this transition brings the app in: a switch to it, or its first showing
/// at startup (shown, with no view yet). A view the app has asked for itself is left alone.
fn restore<V>(
    of: Res<ViewsOf<V>>,
    (active, next_app): (Res<State<ActiveApp>>, Res<NextState<ActiveApp>>),
    view: Option<Res<State<V>>>,
    store: Res<LocalStore>,
    mut next: ResMut<NextState<V>>,
) where
    V: SubStates<SourceStates = ActiveApp> + DeserializeOwned,
{
    let (id, shown) = (of.0, active.get().0);
    let target = match &*next_app {
        NextState::Pending(to) | NextState::PendingIfDifferent(to) => to.0,
        NextState::Unchanged => shown,
    };
    let entering = target == id && (shown != id || view.is_none());
    if entering
        && matches!(*next, NextState::Unchanged)
        && let Some(kept) = store.get::<V>(id, VIEW)
    {
        next.set(kept);
    }
}

/// Keeps the view the app is on whenever it changes.
fn keep<V>(of: Res<ViewsOf<V>>, view: Option<Res<State<V>>>, mut store: ResMut<LocalStore>)
where
    V: SubStates + Serialize,
{
    if let Some(view) = view
        && view.is_changed()
    {
        store.set(of.0, VIEW, view.get());
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;
    use crate::testing::{OTHER, Other, headless, show};
    use crate::{Manifest, ShellApp, ShellPlugin};
    use trailhead_ui::Icon;

    const NOTES: AppId = AppId("notes");

    #[derive(SubStates, Serialize, Deserialize, Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
    #[source(ActiveApp = ActiveApp(NOTES))]
    enum Page {
        #[default]
        List,
        Detail,
        Settings,
    }

    /// Every page entered, in order.
    #[derive(Resource, Default)]
    struct Entered(Vec<Page>);

    struct Notes;

    impl ShellApp for Notes {
        fn manifest(&self) -> Manifest {
            Manifest { id: NOTES, name: "Notes", icon: Icon::Board }
        }

        fn build(&self, app: &mut App) {
            app.add_remembered_view::<Page>(NOTES).init_resource::<Entered>();
            for page in [Page::List, Page::Detail, Page::Settings] {
                app.add_systems(OnEnter(page), move |mut entered: ResMut<Entered>| entered.0.push(page));
            }
        }
    }

    fn go(app: &mut App, page: Page) {
        app.world_mut().resource_mut::<NextState<Page>>().set(page);
        app.update();
    }

    fn entered(app: &App) -> &[Page] {
        &app.world().resource::<Entered>().0
    }

    fn kept(app: &App) -> Option<Page> {
        app.world().resource::<LocalStore>().get(NOTES, VIEW)
    }

    #[test]
    fn coming_back_to_the_app_enters_the_kept_view_first() {
        let mut app = headless(ShellPlugin::new().with(Notes).with(Other));
        assert_eq!(entered(&app), [Page::List], "the default with nothing kept");
        go(&mut app, Page::Detail);
        assert_eq!(kept(&app), Some(Page::Detail));
        show(&mut app, OTHER);
        assert_eq!(kept(&app), Some(Page::Detail), "leaving the app is not a change of view");
        show(&mut app, NOTES);
        assert_eq!(entered(&app), [Page::List, Page::Detail, Page::Detail], "never the default on the way");
        assert_eq!(*app.world().resource::<State<Page>>().get(), Page::Detail);
    }

    #[test]
    fn a_new_shell_starts_on_the_kept_view() {
        let dir = tempfile::tempdir().unwrap();
        let shell = || ShellPlugin::new().with(Notes).with(Other).store_in(dir.path().to_path_buf());
        let mut first = headless(shell());
        go(&mut first, Page::Settings);
        first.update();
        drop(first);
        let again = headless(shell());
        assert_eq!(entered(&again), [Page::Settings], "the kept view is the first one entered");
    }

    #[test]
    fn a_kept_view_waits_while_another_app_is_shown_first() {
        let dir = tempfile::tempdir().unwrap();
        let shell = || ShellPlugin::new().with(Notes).with(Other).store_in(dir.path().to_path_buf());
        let mut first = headless(shell());
        go(&mut first, Page::Detail);
        show(&mut first, OTHER);
        drop(first);
        let mut app = headless(shell());
        assert!(entered(&app).is_empty(), "the shell starts on the app shown last");
        show(&mut app, NOTES);
        assert_eq!(entered(&app), [Page::Detail]);
    }
}
