//! Lookout, the Trailhead factory's Bevy client (`docs/factory/ui.md`), and the factory's app in the Trailhead shell
//! (`docs/app-platform.md`).

use bevy::prelude::*;
use trailhead_shell::{AppId, Manifest, ShellApp};
use trailhead_ui::{Icon, Theme};

pub mod author;
pub mod board;
pub mod claims;
pub mod commands;
pub mod connect;
pub mod connection;
pub mod detail;
pub mod factory;
pub mod factory_link;
pub mod form;
pub mod icon;
pub mod inspector;
pub mod logs;
pub mod map;
pub mod net;
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
    config: net::NetConfig,
    /// The control plane to dial, ahead of the one dialed last time.
    target: Option<String>,
    /// A user token to sign in with at startup.
    token: Option<String>,
    /// The queue task a task queued for the factory goes under, ahead of the factory's own queues.
    queue: Option<String>,
}

impl Lookout {
    pub fn new(config: net::NetConfig) -> Self {
        Lookout { config, target: None, token: None, queue: None }
    }

    #[must_use]
    pub fn dialing(mut self, target: Option<String>) -> Self {
        self.target = target;
        self
    }

    #[must_use]
    pub fn signing_in_with(mut self, token: Option<String>) -> Self {
        self.token = token;
        self
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

    /// The connection is Lookout's for now. It is the shell's to own once a second app needs the server
    /// (TRAIL-1577), and then this adds only Lookout's own logic.
    fn build(&self, app: &mut App) {
        let (factory_link, jobs) = factory_link::factory_link();
        app.add_plugins(connection::ConnectionPlugin::new(self.config.clone(), self.target.clone()).with_jobs(jobs))
            .add_plugins((
                factory_link,
                factory::FactoryPlugin,
                search::SearchPlugin,
                detail::DetailPlugin,
                logs::LogsPlugin,
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
            connect::ConnectUiPlugin,
            settings::SettingsUiPlugin,
            sidebar::SidebarUiPlugin,
            icon::IconUiPlugin,
        ));
        let (queue, token) = (self.queue.clone(), self.token.clone());
        app.add_systems(Startup, move |mut authoring: ResMut<author::Authoring>| {
            if let Some(queue) = &queue {
                authoring.queue = queue.clone();
                authoring.pinned = true;
            }
        })
        .add_systems(Startup, move |mut send: MessageWriter<connection::SendNet>| {
            if let Some(token) = &token {
                send.write(connection::SendNet(net::NetCommand::Login(token.clone())));
            }
        });
    }

    fn paint(&self, theme: &mut Theme) {
        widgets::paint(theme);
    }
}
