//! Security's response to a lab that has been dealing.
//!
//! [`crate::antagonist::SecuritySuspicion`] builds invisibly — a successful
//! illicit delivery raises it, nothing else touches it. Cross the threshold
//! and Security responds in three beats: a warning over the radio, a real
//! window to act on it, then an officer who inspects whatever is physically
//! sitting out right now.
//!
//! The sweep is a global query, not a spatial one, because the decision it
//! is checking — "does the lab hold contraband" — has no location to walk
//! to. `CrewRoute::arrival`/`.leave()` are reused completely unmodified for
//! the officer's entrance and exit; a multi-stop patrol would be animation
//! with no mechanical payoff, since the check runs once regardless of where
//! the officer's model happens to be standing.

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use chem_sim::Category;
use serde::Deserialize;

use crate::antagonist::SecuritySuspicion;
use crate::chem_data::ChemDb;
use crate::containers::Container;
use crate::crew::{spawn_crew_member, CrewDef, CrewPhase, CrewRoute};
use crate::net::is_authority;
use crate::orders::{Department, Shift};
use crate::radio::{RadioEntry, RadioLog};
use crate::AppState;

pub struct SecurityPlugin;

impl Plugin for SecurityPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RonAssetPlugin::<SecurityScript>::new(&["security.ron"]))
            .init_resource::<RaidSchedule>()
            .add_systems(Startup, start_loading)
            .add_systems(
                Update,
                (promote_script, schedule_raid, run_sweep)
                    .chain()
                    .run_if(is_authority)
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// How much Security's standing drops when the sweep finds something.
///
/// One notch past `body::COLLAPSE_PENALTY` (-3): a raid is being caught
/// deliberately holding contraband, where a collapse is an accident.
const RAID_PENALTY: i32 = -4;

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

/// `assets/data/station.security.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct SecurityScript {
    pub threshold: i32,
    pub warning_seconds: f32,
    pub dwell_seconds: f32,
    pub warning_line: String,
    pub confiscation_line: String,
    pub clean_line: String,
}

#[derive(Resource)]
struct PendingSecurityScript(Handle<SecurityScript>);

#[derive(Resource, Deref)]
struct Script(SecurityScript);

fn start_loading(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(PendingSecurityScript(
        assets.load("data/station.security.ron"),
    ));
}

fn promote_script(
    mut commands: Commands,
    pending: Option<Res<PendingSecurityScript>>,
    mut scripts: ResMut<Assets<SecurityScript>>,
) {
    let Some(pending) = pending else {
        return;
    };
    let Some(script) = scripts.remove(&pending.0) else {
        return;
    };
    commands.insert_resource(Script(script));
    commands.remove_resource::<PendingSecurityScript>();
}

// ---------------------------------------------------------------------------
// The warning, and the officer
// ---------------------------------------------------------------------------

/// Marks the crew entity spawned for a sweep. Built inline from a synthetic
/// [`CrewDef`] rather than drawn from `station.crew.ron`, so
/// `generate_orders`'s roster pick can never select "the raid officer" as a
/// legitimate requester. Carries no `Order` and no `Interactable` — there is
/// nothing for the player to do to them directly; the interaction already
/// happened, before they arrived.
#[derive(Component)]
struct RaidOfficer {
    /// Seconds left standing at the counter before the sweep fires, once
    /// they arrive — a beat of "something is happening" before it does.
    dwell: f32,
}

/// When the next warning or sweep is due. `None` means suspicion has not
/// crossed the threshold since the last sweep.
#[derive(Resource, Default)]
struct RaidSchedule {
    warning_in: Option<f32>,
}

/// Watches suspicion, warns, then sends the officer in.
///
/// Gated on `Shift::accepting_orders` exactly like a legitimate visit or an
/// antagonist's — a raid is new traffic in the sense the sign controls, not
/// an order already in progress, so a declared break holds it off too.
#[allow(clippy::too_many_arguments)]
fn schedule_raid(
    mut commands: Commands,
    time: Res<Time>,
    script: Option<Res<Script>>,
    mut schedule: ResMut<RaidSchedule>,
    mut suspicion: ResMut<SecuritySuspicion>,
    shift: Res<Shift>,
    mut radio: ResMut<RadioLog>,
    officers: Query<(), With<RaidOfficer>>,
) {
    let Some(script) = script else {
        return;
    };
    if !shift.accepting_orders || !officers.is_empty() {
        return;
    }

    if let Some(warning) = schedule.warning_in.as_mut() {
        *warning -= time.delta_secs();
        if *warning > 0.0 {
            return;
        }
        schedule.warning_in = None;

        let officer_def = CrewDef {
            name: "Security".to_string(),
            role: "Security".to_string(),
            color: [0.80, 0.18, 0.18],
        };
        let officer = spawn_crew_member(&mut commands, &officer_def, 0.0);
        commands.entity(officer).insert(RaidOfficer {
            dwell: script.dwell_seconds,
        });
        return;
    }

    if suspicion.0 >= script.threshold {
        schedule.warning_in = Some(script.warning_seconds);
        radio.push(RadioEntry {
            channel: "SEC".to_string(),
            text: script.warning_line.clone(),
            good: false,
        });
        // The warning is the resolution of "how much suspicion has built" —
        // resetting here rather than after the sweep means a second illicit
        // delivery during the warning window starts building fresh rather
        // than instantly re-triggering the moment this one clears.
        suspicion.0 = 0;
    }
}

// ---------------------------------------------------------------------------
// The sweep
// ---------------------------------------------------------------------------

/// Inspects every container in the world, once, when the officer's dwell
/// runs out.
///
/// No filter on `HeldBy`, `InSlot` or `TestBenchStock` — every `Container`
/// is checked, wherever it is. That is exactly what makes test-bench-origin
/// stock non-exempt with no special-casing: it is still a `Container`
/// holding an `Illicit`-category reagent, indistinguishable from any other.
#[allow(clippy::too_many_arguments)]
fn run_sweep(
    mut commands: Commands,
    db: Res<ChemDb>,
    script: Option<Res<Script>>,
    time: Res<Time>,
    mut officers: Query<(Entity, &mut RaidOfficer, &mut CrewRoute)>,
    mut containers: Query<&mut Container>,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
) {
    let Some(script) = script else {
        return;
    };
    for (entity, mut officer, mut route) in &mut officers {
        if route.phase != CrewPhase::Waiting {
            continue;
        }
        officer.dwell -= time.delta_secs();
        if officer.dwell > 0.0 {
            continue;
        }

        let mut found = false;
        for mut container in &mut containers {
            let contraband = container.solution.iter().any(|(id, amount)| {
                amount.is_positive()
                    && db
                        .reagents
                        .get(id)
                        .categories
                        .contains(&Category::Illicit)
            });
            if contraband {
                found = true;
                container.solution.clear();
            }
        }

        if found {
            shift.adjust(Department::Security, RAID_PENALTY);
            radio.push(RadioEntry {
                channel: "SEC".to_string(),
                text: script.confiscation_line.clone(),
                good: false,
            });
            info!("security raid: contraband confiscated, {RAID_PENALTY} standing");
        } else {
            radio.push(RadioEntry {
                channel: "SEC".to_string(),
                text: script.clean_line.clone(),
                good: true,
            });
            info!("security raid: clean");
        }

        commands.entity(entity).remove::<RaidOfficer>();
        route.leave();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::containers::ContainerKind;
    use crate::machines::TestBenchStock;
    use chem_sim::{ChemData, Units};

    #[test]
    fn the_plugin_initialises_every_resource_its_systems_need() {
        // Regression guard for a real bug: `RaidSchedule` was never
        // `init_resource`d, so the first frame past `Playing` panicked with
        // "Resource does not exist" the moment `schedule_raid` ran — and no
        // test caught it, because every other test here drives `run_sweep`
        // or `schedule_raid` directly rather than the actual `SecurityPlugin`.
        // `init_resource` runs at `build()` time, so this needs no update
        // loop, no asset server and no state machine to check.
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, AssetPlugin::default(), SecurityPlugin));
        assert!(
            app.world().get_resource::<RaidSchedule>().is_some(),
            "SecurityPlugin must initialise every resource its own systems require"
        );
    }

    fn data() -> ChemData {
        ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap()
    }

    fn sweep_app() -> App {
        let mut app = App::new();
        app.insert_resource(ChemDb(data()))
            .insert_resource(Script(
                ron::from_str(include_str!("../../assets/data/station.security.ron")).unwrap(),
            ))
            .init_resource::<Shift>()
            .init_resource::<Time>()
            .init_resource::<RadioLog>()
            .add_systems(Update, run_sweep);
        app
    }

    fn officer_waiting(app: &mut App, dwell: f32) -> Entity {
        let mut route = CrewRoute::arrival(0.0);
        route.phase = CrewPhase::Waiting;
        app.world_mut()
            .spawn((RaidOfficer { dwell }, route))
            .id()
    }

    fn beaker_of(app: &mut App, reagent: &str, amount: i32) -> Entity {
        let id = app.world().resource::<ChemDb>().reagent(reagent);
        let mut container = Container::new(ContainerKind::LargeBeaker);
        let _ = container.solution.add(id, Units::whole(amount));
        app.world_mut().spawn(container).id()
    }

    fn advance(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(seconds));
        app.update();
    }

    #[test]
    fn contraband_is_confiscated_and_costs_security_standing() {
        let mut app = sweep_app();
        officer_waiting(&mut app, 0.5);
        let beaker = beaker_of(&mut app, "space_drugs", 15);

        advance(&mut app, 1.0);

        assert!(
            app.world()
                .get::<Container>(beaker)
                .unwrap()
                .solution
                .is_empty(),
            "contraband should be confiscated"
        );
        assert_eq!(
            app.world().resource::<Shift>().standing(Department::Security),
            RAID_PENALTY
        );
    }

    #[test]
    fn test_bench_stock_is_not_exempt_from_a_sweep() {
        let mut app = sweep_app();
        officer_waiting(&mut app, 0.5);
        let beaker = beaker_of(&mut app, "space_drugs", 15);
        app.world_mut().entity_mut(beaker).insert(TestBenchStock);

        advance(&mut app, 1.0);

        assert!(
            app.world()
                .get::<Container>(beaker)
                .unwrap()
                .solution
                .is_empty(),
            "practice stock is still real contraband sitting in the lab"
        );
        assert_eq!(
            app.world().resource::<Shift>().standing(Department::Security),
            RAID_PENALTY
        );
    }

    #[test]
    fn a_clean_sweep_costs_nothing() {
        let mut app = sweep_app();
        officer_waiting(&mut app, 0.5);
        beaker_of(&mut app, "kelotane", 20);

        advance(&mut app, 1.0);

        assert_eq!(
            app.world().resource::<Shift>().standing(Department::Security),
            0
        );
    }

    #[test]
    fn rinsing_before_the_sweep_defeats_it() {
        // The dwell is the telegraphed window: emptying the container before
        // it elapses is `EmptyRequested`'s existing, already-working effect
        // (`solution.clear()`, unconditional) — nothing here needs to change
        // for that to count as a real escape hatch.
        let mut app = sweep_app();
        officer_waiting(&mut app, 5.0);
        let beaker = beaker_of(&mut app, "space_drugs", 15);

        // Rinsed before the dwell elapses.
        app.world_mut()
            .get_mut::<Container>(beaker)
            .unwrap()
            .solution
            .clear();
        advance(&mut app, 1.0);
        assert_eq!(
            app.world().resource::<Shift>().standing(Department::Security),
            0,
            "the beaker was empty before the dwell ran out; too early to sweep it"
        );

        advance(&mut app, 5.0);
        assert_eq!(
            app.world().resource::<Shift>().standing(Department::Security),
            0,
            "an empty beaker gives the sweep nothing to find"
        );
    }
}
