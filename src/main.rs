//! ChemGame — a focused recreation of the Space Station 13/14 chemist.

mod interaction;
mod lab;
mod machines;
mod player;

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
            lab::LabPlugin,
            machines::MachinePlugin,
            player::PlayerPlugin,
            interaction::InteractionPlugin,
        ))
        .add_systems(Startup, begin)
        .add_systems(
            Update,
            log_interactions.run_if(in_state(AppState::Playing)),
        )
        .run();
}

#[derive(States, Debug, Clone, Copy, Default, Eq, PartialEq, Hash)]
pub enum AppState {
    /// Waiting on chemistry data. M3 gives this something to do; for now it
    /// exists so the transition into `Playing` is already in place.
    #[default]
    Loading,
    Playing,
}

fn begin(mut next: ResMut<NextState<AppState>>) {
    next.set(AppState::Playing);
}

/// Placeholder consumer of interaction requests until M3 opens real panels.
fn log_interactions(
    mut requests: MessageReader<interaction::InteractRequested>,
    interactables: Query<&interaction::Interactable>,
) {
    for request in requests.read() {
        if let Ok(interactable) = interactables.get(request.target) {
            info!("used {}", interactable.label);
        }
    }
}
