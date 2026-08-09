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
            next.set(AppState::Playing);
        }
        Err(error) => {
            // Bad data is a content bug, not something to limp along with:
            // every recipe downstream would misbehave in confusing ways.
            panic!("chemistry data is invalid: {error}");
        }
    }
}
