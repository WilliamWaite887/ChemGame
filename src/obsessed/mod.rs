//! A stalker fixated specifically on the chemist.
//!
//! Every other visitor in this game is anonymous the moment they leave — a
//! role and a reagent, drawn fresh each time. This is the one exception: the
//! same named individual, every visit, in an authored order rather than a
//! random pool, so an attentive player can watch the pattern escalate across
//! a career. Unlike [`crate::antagonist`] or [`crate::rogue_security`], a
//! visit here costs nothing mechanically — no reagent demand beyond an
//! ordinary order, no standing swing beyond the ordinary reputation curve,
//! no struggle. The entire weight is in the plea and the unsettling line
//! that follows it.
//!
//! Reuses the ordinary [`Order`]/`complete_delivery` pipeline unmodified —
//! this spawns a plain, `specific` order like any honest "asks for it by
//! name" visit. The only bespoke piece is [`handle_obsessed_resolution`],
//! which watches for *this* visitor's name coming back through
//! [`OrderResolved`] to advance the authored sequence.

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use serde::Deserialize;

use crate::chem_data::ChemDb;
use crate::containers::{spawn_container, ContainerKind};
use crate::lab::{DeliveryLane, DeliveryStations};
use crate::net::is_authority;
use crate::orders::{OrderResolved, Shift, StationData};
use crate::player::Chemist;
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::shift::{self, current_rules};
use crate::AppState;

/// Seconds before the very first possible visit. Long — this is meant to be
/// rare and, the first time, easy to write off as an odd customer.
const INITIAL_GAP_SECONDS: (f32, f32) = (600.0, 900.0);

pub struct ObsessedPlugin;

impl Plugin for ObsessedPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RonAssetPlugin::<ObsessedScript>::new(&["obsessed.ron"]))
            .init_resource::<ObsessedProgress>()
            .add_systems(Startup, start_loading)
            .add_systems(OnEnter(AppState::Playing), arm_spawner)
            .add_systems(
                Update,
                (
                    promote_script,
                    generate_obsessed_visit,
                    handle_obsessed_resolution,
                )
                    .chain()
                    .run_if(is_authority)
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Which authored visit fires next. Persisted — this is a career fact, the
/// same as `antagonist::UnderworldStanding`. Clamped at the last entry
/// rather than wrapping or panicking: once the authored content runs out,
/// the same final beat simply repeats.
#[derive(Resource, Default, Clone, Copy)]
pub struct ObsessedProgress(pub usize);

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

/// `assets/data/station.obsessed.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct ObsessedScript {
    /// The one recurring identity. Deliberately not drawn from
    /// `station.crew.ron` — this name belongs to this thread alone, so it
    /// can never double-book with an ordinary order and never be mistaken
    /// for a stranger.
    pub name: String,
    pub role: String,
    pub color: [f32; 3],
    pub gap_multiplier: (f32, f32),
    /// Ordered — escalation is an authored sequence, not a random draw.
    pub visits: Vec<ObsessedVisitDef>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ObsessedVisitDef {
    pub reagent: String,
    pub amount: u32,
    pub plea: String,
    /// What's said or left behind that reads as fixation rather than an
    /// ordinary request — aired right after the plea, not hidden.
    pub unsettling_line: String,
    /// An unasked gift left on the counter — a small sample of the same
    /// reagent being asked for, as though they had already started making
    /// it themselves.
    #[serde(default)]
    pub leaves_token: bool,
}

#[derive(Resource)]
struct PendingObsessedScript(Handle<ObsessedScript>);

#[derive(Resource, Deref)]
struct Script(ObsessedScript);

#[derive(Resource)]
struct ObsessedSpawner {
    timer: Timer,
}

fn start_loading(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(PendingObsessedScript(
        assets.load("data/station.obsessed.ron"),
    ));
}

fn promote_script(
    mut commands: Commands,
    pending: Option<Res<PendingObsessedScript>>,
    mut scripts: ResMut<Assets<ObsessedScript>>,
) {
    let Some(pending) = pending else {
        return;
    };
    let Some(script) = scripts.remove(&pending.0) else {
        return;
    };
    commands.insert_resource(Script(script));
    commands.remove_resource::<PendingObsessedScript>();
}

/// See `shift::arm_first_visit` for why this has to re-run on
/// `OnEnter(AppState::Playing)` every session rather than only once at
/// process start.
fn arm_spawner(mut commands: Commands) {
    shift::arm_first_visit(&mut commands, INITIAL_GAP_SECONDS, |timer| {
        ObsessedSpawner { timer }
    });
}

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn generate_obsessed_visit(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    script: Option<Res<Script>>,
    mut spawner: Option<ResMut<ObsessedSpawner>>,
    progress: Res<ObsessedProgress>,
    shift: Res<Shift>,
    mut radio: ResMut<RadioLog>,
    chemists: Query<(), With<Chemist>>,
    stations: Option<Res<DeliveryStations>>,
) {
    let (Some(station), Some(script), Some(spawner)) = (station, script, spawner.as_mut()) else {
        return;
    };
    if !shift.accepting_orders {
        return;
    }
    if !spawner.timer.tick(time.delta()).just_finished() {
        return;
    }

    let mut rng = rand::rng();
    let rules = current_rules(&station.config, &shift, chemists.iter().count());
    spawner.timer = shift::roll_next_gap(&mut rng, &rules, script.gap_multiplier);

    let Some(visit) = script
        .visits
        .get(progress.0.min(script.visits.len().saturating_sub(1)))
    else {
        return;
    };
    let Some(reagent) = db.reagents.id_of(&visit.reagent) else {
        warn!("obsessed visit names unknown reagent '{}'", visit.reagent);
        return;
    };

    shift::spawn_scripted_visit(
        &mut commands,
        &db,
        &mut rng,
        &rules,
        shift::ScriptedVisit {
            name: &script.name,
            role: &script.role,
            color: script.color,
            reagent,
            amount_units: visit.amount,
            plea: visit.plea.clone(),
        },
    );

    let channel = channel_for(&script.role);
    radio.push(
        RadioEntry::new(channel, visit.plea.clone())
            .speaker(&script.name)
            .negative(),
    );
    radio.push(
        RadioEntry::new(channel, visit.unsettling_line.clone())
            .speaker(&script.name)
            .negative(),
    );

    if visit.leaves_token {
        let (_, height) = ContainerKind::Bottle.dimensions();
        let station = stations
            .as_deref()
            .cloned()
            .unwrap_or_default()
            .station(DeliveryLane::Public);
        let token = spawn_container(
            &mut commands,
            ContainerKind::Bottle,
            station.drop_position(crate::lab::COUNTER_TOP + height * 0.5)
                - (station.transform.rotation * Vec3::X) * 0.6,
        );
        commands.queue(move |world: &mut World| {
            if let Some(mut container) = world.get_mut::<crate::containers::Container>(token) {
                let _ = container.solution.add(reagent, chem_sim::Units::whole(5));
            }
        });
    }

    info!("obsessed: {} visit {}", script.name, progress.0);
}

/// Advances the authored sequence whenever *this* visitor's own name comes
/// back through a resolution — matched by name because, unlike
/// `crate::antagonist`'s illicit thread or `crate::cult`, this identity is
/// unique to this module and never drawn from the ordinary roster, so a
/// name match can never be confused with anyone else's order.
fn handle_obsessed_resolution(
    script: Option<Res<Script>>,
    mut resolved: MessageReader<OrderResolved>,
    mut progress: ResMut<ObsessedProgress>,
) {
    let Some(script) = script else {
        resolved.clear();
        return;
    };
    for report in resolved.read() {
        if report.name != script.name {
            continue;
        }
        progress.0 = (progress.0 + 1).min(script.visits.len().saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orders::{Department, Outcome};

    fn data() -> chem_sim::ChemData {
        chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap()
    }

    fn script() -> ObsessedScript {
        ron::from_str(include_str!("../../assets/data/station.obsessed.ron")).unwrap()
    }

    #[test]
    fn obsessed_ron_parses_and_the_identity_is_off_the_ordinary_roster() {
        let data = data();
        let script = script();
        assert!(!script.name.trim().is_empty());
        assert!(
            Department::from_role(&script.role).is_some(),
            "'{}' names a role no department recognises",
            script.role
        );
        assert!(
            script.visits.len() >= 2,
            "no escalation without at least two beats"
        );
        let roster: Vec<crate::crew::CrewDef> =
            ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();
        assert!(
            roster.iter().all(|member| member.name != script.name),
            "'{}' must stay off the ordinary roster or it could double-book",
            script.name
        );
        for visit in &script.visits {
            assert!(
                data.reagents.id_of(&visit.reagent).is_some(),
                "'{}' names no real reagent",
                visit.reagent
            );
            assert!(!visit.plea.trim().is_empty());
            assert!(!visit.unsettling_line.trim().is_empty());
        }
    }

    fn resolution_app() -> App {
        let mut app = App::new();
        app.insert_resource(Script(script()))
            .init_resource::<ObsessedProgress>()
            .add_message::<OrderResolved>()
            .add_systems(Update, handle_obsessed_resolution);
        app
    }

    fn resolve(app: &mut App, name: &str) {
        app.world_mut().write_message(OrderResolved {
            name: name.to_string(),
            role: "Service".to_string(),
            reagent: None,
            category: None,
            outcome: Outcome::Success,
            kind: crate::orders::OrderKind::Normal,
        });
        app.update();
    }

    #[test]
    fn the_same_name_recurs_and_advances_progress() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();

        resolve(&mut app, &name);

        assert_eq!(app.world().resource::<ObsessedProgress>().0, 1);
    }

    #[test]
    fn an_unrelated_resolution_never_advances_progress() {
        let mut app = resolution_app();

        resolve(&mut app, "Someone Else Entirely");

        assert_eq!(app.world().resource::<ObsessedProgress>().0, 0);
    }

    #[test]
    fn escalation_is_monotonic_and_clamped() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let last = app.world().resource::<Script>().0.visits.len() - 1;

        for _ in 0..(last + 5) {
            resolve(&mut app, &name);
        }

        assert_eq!(
            app.world().resource::<ObsessedProgress>().0,
            last,
            "progress must never run past the authored content"
        );
    }
}
