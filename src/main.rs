//! ChemGame — a focused recreation of the Space Station 13/14 chemist.

mod antagonist;
mod body;
mod chem_data;
mod containers;
mod crew;
mod hazards;
mod interaction;
mod knowledge;
mod lab;
mod machines;
mod menu;
mod net;
mod orders;
mod player;
mod produce;
mod radio;
mod saves;
mod security;
mod shift;
mod ui;

use bevy::prelude::*;

fn main() {
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window {
            title: "ChemGame — Chemistry Lab".into(),
            ..default()
        }),
        ..default()
    }))
    .init_state::<AppState>();

    // Before the plugins, so `NetPlugin` sees a mode already chosen and the
    // loader knows whether there is a menu to show.
    net::apply_command_line(&mut app);

    app.add_plugins((
            net::NetPlugin,
            chem_data::ChemDataPlugin,
            lab::LabPlugin,
            machines::MachinePlugin,
            containers::ContainerPlugin,
            crew::CrewPlugin,
            knowledge::KnowledgePlugin,
            orders::OrderPlugin,
            // Nested so the outer tuple stays inside Bevy's 16-plugin limit.
            // The phase machine, the supply it schedules, and its save file.
            (
                shift::ShiftPlugin,
                shift::RestockPlugin,
                shift::ProgressPlugin,
            ),
            produce::ProducePlugin,
            radio::RadioPlugin,
            // The hidden antagonist thread. After orders/shift/radio, since it
            // leans on `Order`, `current_rules` and `PendingBroadcasts`.
            antagonist::AntagonistPlugin,
            // Security's response to it. After antagonist, since it reads
            // the suspicion antagonist builds.
            security::SecurityPlugin,
            // The chemist themselves: who they are, what they are looking at,
            // and what the chemistry is doing to them.
            (
                player::PlayerPlugin,
                interaction::InteractionPlugin,
                body::BodyPlugin,
                hazards::HazardPlugin,
            ),
            (ui::UiPlugin, menu::MenuPlugin),
        ))
        .run();
}

#[derive(States, Debug, Clone, Copy, Default, Eq, PartialEq, Hash)]
pub enum AppState {
    /// Waiting on the chemistry data files. `ChemDataPlugin` moves us on once
    /// they have parsed.
    #[default]
    Loading,
    /// Picking how to play and which save. Skipped when the command line
    /// already said.
    MainMenu,
    Playing,
}
