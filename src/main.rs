//! ChemGame — a focused recreation of the Space Station 13/14 chemist.

mod chem_data;
mod containers;
mod crew;
mod interaction;
mod knowledge;
mod lab;
mod machines;
mod net;
mod orders;
mod player;
mod radio;
mod ui;

use bevy::prelude::*;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                title: "ChemGame — Chemistry Lab".into(),
                ..default()
            }),
            ..default()
        }))
        .init_state::<AppState>()
        .add_plugins((
            net::NetPlugin,
            chem_data::ChemDataPlugin,
            lab::LabPlugin,
            machines::MachinePlugin,
            containers::ContainerPlugin,
            crew::CrewPlugin,
            knowledge::KnowledgePlugin,
            orders::OrderPlugin,
            radio::RadioPlugin,
            player::PlayerPlugin,
            interaction::InteractionPlugin,
            ui::UiPlugin,
        ))
        .run();
}

#[derive(States, Debug, Clone, Copy, Default, Eq, PartialEq, Hash)]
pub enum AppState {
    /// Waiting on the chemistry data files. `ChemDataPlugin` moves us on once
    /// they have parsed.
    #[default]
    Loading,
    Playing,
}
