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
use chem_sim::Category;
use serde::Deserialize;

use crate::antagonist::{clear_suspicion, SecuritySuspicion};
use crate::chem_data::ChemDb;
use crate::containers::Container;
use crate::crew::{spawn_crew_member, CrewDef, CrewPhase, CrewRoute};
use crate::net::is_authority;
use crate::orders::{Department, Shift};
use crate::radio::{RadioEntry, RadioLog};
use crate::threat;
use crate::AppState;

pub struct SecurityPlugin;

impl Plugin for SecurityPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(threat::ScriptPlugin::<SecurityScript>::new(
            "data/station.security.ron",
            "security.ron",
        ))
            .init_resource::<RaidSchedule>()
            .add_systems(
                Update,
                (schedule_raid, run_sweep)
                    .chain()
                    .after(threat::PromoteScripts)
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

/// What a raid actually firing nudges `instability::Instability` by — see
/// `instability::INCOMPETENCE_PER_IGNORED_SHENANIGAN` for the same scale.
const RAID_INSTABILITY: i32 = 4;

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

/// This thread's authored script, once loaded.
type Script = threat::Authored<SecurityScript>;


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
///
/// `pub(crate)` since the antagonist thread's Spy-flavoured sting
/// (`antagonist::handle_illicit_resolutions`) arms this directly, bypassing
/// the ordinary suspicion accumulation for one specific deal gone wrong —
/// `schedule_raid` still owns everything past that (the officer, the dwell,
/// the sweep) unmodified, since it only ever *reads* `warning_in` being
/// already-`Some` the same way it would after its own threshold check.
#[derive(Resource, Default)]
pub(crate) struct RaidSchedule {
    pub(crate) clock: threat::Countdown,
}

impl RaidSchedule {
    /// Arms the raid directly, bypassing the ordinary suspicion threshold.
    ///
    /// One caller: `antagonist::handle_illicit_resolutions`'s Spy-flavoured
    /// sting. The "only if nothing is already armed" guard lives here rather
    /// than at that call site, because it is this schedule's invariant, not
    /// the stinger's.
    pub(crate) fn arm_sting(&mut self, seconds: f32) {
        if !self.clock.is_armed() {
            self.clock.arm(seconds, 0);
        }
    }
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
    mut instability: Option<ResMut<crate::instability::Instability>>,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
    officers: Query<(), With<RaidOfficer>>,
) {
    let Some(script) = script else {
        return;
    };
    if !shift.accepting_orders || !officers.is_empty() {
        return;
    }

    match schedule.clock.tick(time.delta_secs()) {
        threat::Ticked::Waiting => return,
        threat::Ticked::Fires(_) => {
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
        threat::Ticked::Idle => {}
    }

    if suspicion.level() >= script.threshold {
        // A `LookTheOtherWay` requisition absorbs the raid before it's ever
        // called in: suspicion still clears (matching the ordinary case
        // below), but no warning is armed and no officer ever spawns.
        // Unlike the three department minors, this one also clears the meter
        // and returns outright rather than continuing a loop — the helper
        // spends the ward and speaks, it does not decide what else happens.
        if threat::ward_absorbed(
            &mut shift,
            &mut radio,
            threat::Ward::Raid,
            RadioEntry::new(
                crate::radio::RadioChannel::Security,
                "Security had questions about recent deliveries, then let it drop.",
            )
            .speaker("Warden Bex")
            .positive(),
        ) {
            clear_suspicion(&mut suspicion);
            return;
        }

        schedule.clock.arm(script.warning_seconds, 0);
        radio.push(
            RadioEntry::new(
                crate::radio::RadioChannel::Security,
                script.warning_line.clone(),
            )
            .speaker("Warden Bex")
            .negative()
            .urgent(),
        );
        // The warning is the resolution of "how much suspicion has built" —
        // resetting here rather than after the sweep means a second illicit
        // delivery during the warning window starts building fresh rather
        // than instantly re-triggering the moment this one clears.
        clear_suspicion(&mut suspicion);
        // A raid actually *firing* — not the raw suspicion, which resets
        // whether the raid succeeds or fails either way — is the discrete,
        // "this went unresolved" signal worth sampling.
        if let Some(instability) = instability.as_mut() {
            crate::instability::nudge_instability(instability, RAID_INSTABILITY);
        }
    }
}

// ---------------------------------------------------------------------------
// The sweep
// ---------------------------------------------------------------------------

/// Inspects every container in the world, once, when the officer's dwell
/// runs out.
///
/// No filter on `HeldBy` or `InSlot` — every `Container` is checked, wherever
/// it is: on a bench, in a hand, loaded into a machine.
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
                let reagent = db.reagents.get(id);
                amount.is_positive()
                    && (reagent.categories.contains(&Category::Illicit)
                        || reagent.controlled
                        || reagent.explosive.is_some())
            });
            if contraband {
                found = true;
                container.solution.clear();
            }
        }

        if found {
            shift.adjust(Department::Security, RAID_PENALTY);
            radio.push(
                RadioEntry::new(
                    crate::radio::RadioChannel::Security,
                    script.confiscation_line.clone(),
                )
                .speaker("Officer Reyes")
                .negative(),
            );
            info!("security raid: contraband confiscated, {RAID_PENALTY} standing");
        } else {
            radio.push(
                RadioEntry::new(
                    crate::radio::RadioChannel::Security,
                    script.clean_line.clone(),
                )
                .speaker("Officer Reyes")
                .positive(),
            );
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
            .insert_resource(threat::Authored::<SecurityScript>(
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
        app.world_mut().spawn((RaidOfficer { dwell }, route)).id()
    }

    /// Enough app to run `schedule_raid` directly, headless.
    fn schedule_app() -> App {
        let mut app = App::new();
        app.insert_resource(threat::Authored::<SecurityScript>(
            ron::from_str(include_str!("../../assets/data/station.security.ron")).unwrap(),
        ))
        .init_resource::<RaidSchedule>()
        .init_resource::<SecuritySuspicion>()
        .insert_resource(Shift {
            accepting_orders: true,
            ..Default::default()
        })
        .init_resource::<Time>()
        .init_resource::<RadioLog>()
        .add_systems(Update, schedule_raid);
        app
    }

    #[test]
    fn crossing_the_threshold_without_a_ward_arms_the_warning_as_before() {
        let mut app = schedule_app();
        let threshold = app.world().resource::<Script>().0.threshold;
        app.world_mut().resource_mut::<SecuritySuspicion>().restore(threshold);

        app.update();

        assert_eq!(
            app.world().resource::<SecuritySuspicion>().level(),
            0,
            "suspicion resets the moment a warning is armed"
        );
        assert!(
            app.world().resource::<RaidSchedule>().clock.is_armed(),
            "without a ward, crossing the threshold should still arm a raid"
        );
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 1);
    }

    #[test]
    fn a_raid_ward_absorbs_the_warning_and_resets_suspicion() {
        let mut app = schedule_app();
        let threshold = app.world().resource::<Script>().0.threshold;
        app.world_mut().resource_mut::<SecuritySuspicion>().restore(threshold);
        app.world_mut()
            .resource_mut::<Shift>()
            .requisition
            .raid_wards = 1;

        app.update();

        assert_eq!(
            app.world().resource::<SecuritySuspicion>().level(),
            0,
            "a warded raid still clears suspicion, same as an unwarded one"
        );
        assert!(
            !app.world().resource::<RaidSchedule>().clock.is_armed(),
            "a Look the Other Way requisition should have absorbed this before it armed"
        );
        assert_eq!(
            app.world().resource::<Shift>().requisition.raid_wards,
            0,
            "the ward is spent, not banked indefinitely"
        );
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 1);
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
            app.world()
                .resource::<Shift>()
                .standing(Department::Security),
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
            app.world()
                .resource::<Shift>()
                .standing(Department::Security),
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
            app.world()
                .resource::<Shift>()
                .standing(Department::Security),
            0,
            "the beaker was empty before the dwell ran out; too early to sweep it"
        );

        advance(&mut app, 5.0);
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .standing(Department::Security),
            0,
            "an empty beaker gives the sweep nothing to find"
        );
    }
}
