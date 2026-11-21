//! Lookout, the Trailhead factory's Bevy client (`docs/factory/ui.md`), and the factory's app in the Trailhead shell
//! (`docs/app-platform.md`).

use std::sync::Mutex;

use bevy::prelude::*;
use trailhead_shell::net::LinkJob;
use trailhead_shell::settings::SettingsPage;
use trailhead_shell::{AppId, Manifest, ShellApp};
use trailhead_ui::{Icon, Theme};

pub mod author;
pub mod board;
pub mod claims;
pub mod detail;
pub mod factory;
pub mod factory_link;
pub mod form;
pub mod inspector;
pub mod logs;
pub mod map;
pub mod peak;
pub mod pictures;
pub mod project;
pub mod project_form;
pub mod projects_page;
pub mod search;
pub mod settings;
pub mod sidebar;
pub mod usage;
pub mod widgets;

/// Lookout's id in the shell.
pub const LOOKOUT: AppId = AppId("lookout");

/// Lookout as the shell holds it.
pub struct Lookout {
    /// The queue task a task queued for the factory goes under, ahead of the factory's own queues.
    queue: Option<String>,
    /// What takes the reports of Lookout's jobs on the net thread, added when Lookout is built.
    factory_link: Mutex<Option<factory_link::FactoryLinkPlugin>>,
    /// Lookout's jobs on the net thread, which the shell takes before it builds Lookout.
    jobs: Mutex<Vec<Box<dyn LinkJob>>>,
}

impl Default for Lookout {
    fn default() -> Self {
        let (factory_link, jobs) = factory_link::factory_link();
        Lookout { queue: None, factory_link: Mutex::new(Some(factory_link)), jobs: Mutex::new(jobs) }
    }
}

impl Lookout {
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn queueing_under(mut self, queue: Option<String>) -> Self {
        self.queue = queue;
        self
    }
}

impl ShellApp for Lookout {
    fn manifest(&self) -> Manifest {
        Manifest { id: LOOKOUT, name: "Lookout", icon: Icon::Board }
    }

    fn link_jobs(&self) -> Vec<Box<dyn LinkJob>> {
        std::mem::take(&mut *self.jobs.lock().expect("jobs lock"))
    }

    fn build(&self, app: &mut App) {
        let factory_link = self.factory_link.lock().expect("factory link lock").take().expect("Lookout is built once");
        app.add_plugins((
            factory_link,
            factory::FactoryPlugin,
            search::SearchPlugin,
            detail::DetailPlugin,
            logs::LogsPlugin,
            pictures::PicturesPlugin,
        ));
    }

    fn build_ui(&self, app: &mut App) {
        app.add_plugins((
            board::BoardUiPlugin,
            map::MapUiPlugin,
            project::ProjectUiPlugin,
            project_form::ProjectFormUiPlugin,
            projects_page::ProjectsPageUiPlugin,
            author::AuthorUiPlugin,
            inspector::InspectorUiPlugin,
            settings::SettingsUiPlugin,
            sidebar::SidebarUiPlugin,
        ));
        let queue = self.queue.clone();
        app.add_systems(Startup, move |mut authoring: ResMut<author::Authoring>| {
            if let Some(queue) = &queue {
                authoring.queue = queue.clone();
                authoring.pinned = true;
            }
        });
    }

    fn paint(&self, theme: &mut Theme) {
        widgets::paint(theme);
    }

    fn settings_pages(&self) -> Vec<SettingsPage> {
        settings::pages()
    }
}
