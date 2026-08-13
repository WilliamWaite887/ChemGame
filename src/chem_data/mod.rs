//! Loads the chemistry data files into a [`ChemDb`] resource.
//!
//! The files go through bevy's asset pipeline rather than `include_str!` so
//! recipes, hints and overdose thresholds can be tuned without a recompile.
//! Bevy picks loaders by *full* extension — everything after the first dot —
//! which is why the files are named `chem.reagents.ron` and
//! `chem.reactions.ron` rather than plain `reagents.ron`.

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use chem_sim::{ChemData, ReactionDef, ReagentDef};
use serde::Deserialize;

use crate::AppState;

pub struct ChemDataPlugin;

impl Plugin for ChemDataPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            RonAssetPlugin::<ReagentList>::new(&["reagents.ron"]),
            RonAssetPlugin::<ReactionList>::new(&["reactions.ron"]),
        ))
        .add_systems(Startup, start_loading)
        .add_systems(Update, finish_loading.run_if(in_state(AppState::Loading)));
    }
}

#[derive(Asset, TypePath, Deserialize)]
#[serde(transparent)]
pub struct ReagentList(pub Vec<ReagentDef>);

#[derive(Asset, TypePath, Deserialize)]
#[serde(transparent)]
pub struct ReactionList(pub Vec<ReactionDef>);

/// Every reagent and reaction in the game.
#[derive(Resource, Deref)]
pub struct ChemDb(pub ChemData);

#[derive(Resource)]
struct PendingChemData {
    reagents: Handle<ReagentList>,
    reactions: Handle<ReactionList>,
}

fn start_loading(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(PendingChemData {
        reagents: assets.load("data/chem.reagents.ron"),
        reactions: assets.load("data/chem.reactions.ron"),
    });
}

fn finish_loading(
    mut commands: Commands,
    pending: Res<PendingChemData>,
    mut reagent_lists: ResMut<Assets<ReagentList>>,
    mut reaction_lists: ResMut<Assets<ReactionList>>,
    mut next: ResMut<NextState<AppState>>,
    from_args: Option<Res<crate::net::LaunchedFromArgs>>,
    mode: Res<crate::net::LaunchMode>,
) {
    let (Some(reagents), Some(reactions)) = (
        reagent_lists.remove(&pending.reagents),
        reaction_lists.remove(&pending.reactions),
    ) else {
        return;
    };

    match ChemData::from_defs(reagents.0, reactions.0) {
        Ok(data) => {
            info!(
                "chemistry loaded: {} reagents ({} dispensable), {} reactions",
                data.reagents.len(),
                data.reagents.dispensable().count(),
                data.reactions.len()
            );
            commands.insert_resource(ChemDb(data));
            commands.remove_resource::<PendingChemData>();
            // The menu needs the chemistry loaded before it can show a save's
            // recipe count, so it opens here rather than at startup.
            next.set(next_state_after_loading(from_args.is_some(), *mode));
        }
        Err(error) => {
            // Bad data is a content bug, not something to limp along with:
            // every recipe downstream would misbehave in confusing ways.
            panic!("chemistry data is invalid: {error}");
        }
    }
}

/// Where a launch goes once the chemistry has parsed.
///
/// Split out of [`finish_loading`] purely so it can be tested: reaching that
/// system otherwise needs the whole asset pipeline.
///
/// The joining check comes *first*, ahead of the "did the command line say
/// anything" one, because a Steam invite can set the launch mode while this
/// is still loading — `steam::handle_lobby_join_requested` deliberately
/// leaves the state alone in that case, since moving it would strand this
/// very system, and this is where that deferred hand-off lands. `--join` and
/// Steam's `+connect_lobby` reach the same branch by the ordinary route.
fn next_state_after_loading(from_args: bool, mode: crate::net::LaunchMode) -> AppState {
    if matches!(
        mode,
        crate::net::LaunchMode::Join(_) | crate::net::LaunchMode::JoinSteam(_)
    ) {
        // A guest waits here — see `AppState::Connecting`'s doc comment —
        // instead of running the full simulation against a handshake that has
        // not finished, or may never.
        AppState::Connecting
    } else if from_args {
        AppState::Playing
    } else {
        AppState::MainMenu
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::{steam::LobbyId, LaunchMode};

    #[test]
    fn a_guest_waits_in_connecting_however_they_were_launched() {
        // The Steam case is the one with teeth: an invite accepted during
        // `Loading` sets the mode without touching the state, so this branch
        // is the *only* thing that ever moves that launch onwards. Getting
        // the order wrong sends it to `MainMenu` and the invite evaporates.
        for mode in [
            LaunchMode::Join("127.0.0.1:5327".parse().unwrap()),
            LaunchMode::JoinSteam(LobbyId::from_raw(1)),
        ] {
            for from_args in [true, false] {
                assert_eq!(
                    next_state_after_loading(from_args, mode),
                    AppState::Connecting,
                    "{mode:?} launched from_args={from_args} must wait for the handshake"
                );
            }
        }
    }

    #[test]
    fn nothing_on_the_command_line_still_means_the_menu() {
        assert_eq!(
            next_state_after_loading(false, LaunchMode::Singleplayer),
            AppState::MainMenu
        );
        // ...and a flag still skips it, which is what keeps two-terminal
        // co-op testing bearable.
        assert_eq!(
            next_state_after_loading(true, LaunchMode::Singleplayer),
            AppState::Playing
        );
        assert_eq!(
            next_state_after_loading(true, LaunchMode::HostSteam),
            AppState::Playing
        );
    }
}
