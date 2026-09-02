//! Security's response to a lab that has been dealing.
//!
//! [`crate::antagonist::SecuritySuspicion`] builds invisibly — a successful
//! illicit delivery raises it, nothing else touches it. Cross the threshold
//! and Security responds in three beats: a warning over the radio, a real
//! window to act on it, then an officer who inspects whatever is physically
//! sitting out right now.
//!
//! Inspections are physical: officers walk to an accessible Chemistry batch
//! and inspect only nearby, unobstructed loose containers. Held, loaded and
//! stored containers require an explicit search and are outside this sweep.
//! Seized contents are preserved in the Security evidence locker.
//!
//! What the officer *reads*, though, is not simply what is in the bottle.
//! A [`crate::labels::Label`] is a claim about which chemical a container
//! holds, and an officer who is not looking very hard takes it at its word —
//! see [`reads_as`] and [`officer_reads_the_labels`].

use bevy::prelude::*;
use chem_sim::{Category, ReagentId, Solution};
use rand::prelude::*;
use serde::Deserialize;

use crate::antagonist::{clear_suspicion, SecuritySuspicion};
use crate::chem_data::ChemDb;
use crate::containers::Container;
use crate::crew::{spawn_crew_member, CrewDef, CrewPhase, CrewRoute};
use crate::labels::Label;
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

/// What it costs when the officer reads a label, checks it against what is
/// actually in the bottle, and finds they disagree.
///
/// Worse than [`RAID_PENALTY`], because it is a worse fact about you: leaving
/// contraband on the bench is carelessness, and a forged label is a lie told
/// directly to the department that would arrest you for it. One notch past
/// `orders::CAUGHT_LYING_PENALTY` (-5), which is the same offence committed
/// against someone with no power to charge you.
const FORGERY_PENALTY: i32 = -6;

/// What a raid actually firing nudges `instability::Instability` by — see
/// `instability::INCOMPETENCE_PER_IGNORED_SHENANIGAN` for the same scale.
const RAID_INSTABILITY: i32 = 8;

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
    /// Read instead of `confiscation_line` when what the officer found was a
    /// label that disagreed with its own contents.
    pub forged_line: String,
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
    target: Option<Entity>,
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
                target: None,
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
// What the officer sees
// ---------------------------------------------------------------------------

/// Whether Security would seize this chemical on sight.
///
/// The one definition of contraband, so a reagent named on a *label* is
/// judged by exactly the same rule as a reagent actually in the bottle —
/// which is what stops "meth, marked as bath salts" from counting as cover.
fn is_contraband(db: &ChemDb, id: ReagentId) -> bool {
    let reagent = db.reagents.get(id);
    reagent.categories.contains(&Category::Illicit)
        || reagent.controlled
        || reagent.explosive.is_some()
}

fn holds_contraband(db: &ChemDb, solution: &Solution) -> bool {
    solution
        .iter()
        .any(|(id, amount)| amount.is_positive() && is_contraband(db, id))
}

/// What one container looks like to an officer standing over it, before any
/// judgement about how hard they are looking.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Reads {
    /// Nothing in it Security cares about.
    Clean,
    /// Contraband, behind a label claiming to be something legal.
    Covered,
    /// Contraband, with nothing written on it to say otherwise.
    Bare,
}

/// Classifies one container for the sweep.
///
/// Cover requires the label to *name a real, legal chemical* — the same
/// [`crate::orders::claimed_reagent`] rule the counter uses, so a bottle
/// marked "Painkiller" or "do not drink" claims nothing here either. Vague
/// reassurance is not a cover story; a specific false one is.
fn reads_as(db: &ChemDb, solution: &Solution, label: Option<&Label>) -> Reads {
    if !holds_contraband(db, solution) {
        return Reads::Clean;
    }
    match crate::orders::claimed_reagent(db, label) {
        Some(claimed) if !is_contraband(db, claimed) => Reads::Covered,
        _ => Reads::Bare,
    }
}

/// Whether this officer checks the labels against the contents.
///
/// Scaled off Security's own standing by the *same* ladder the counter uses
/// for crew members ([`crate::orders::trust_in_a_label`]), because it is the
/// same judgement being made: a department pleased with the lab does not
/// audit the lab. Which means every raid you fail tightens the next one —
/// the penalty for being caught is also what makes you likelier to be caught
/// again.
///
/// `estranged: false` unconditionally: estrangement is tracked per named
/// crew member, and the raid officer is built from a synthetic [`CrewDef`]
/// that is deliberately not on the roster, so there is no relationship to
/// have broken.
///
/// Pure, and takes its own `roll`, so every band is testable without a world.
fn officer_reads_the_labels(standing: i32, roll: f64) -> bool {
    roll >= crate::orders::trust_in_a_label(standing, false)
}

// ---------------------------------------------------------------------------
// The sweep
// ---------------------------------------------------------------------------

/// After the announced window, walk to one accessible batch and inspect its
/// immediate surroundings. There is no remote disposal or inventory search.
#[allow(clippy::too_many_arguments)]
fn run_sweep(
    mut commands: Commands,
    db: Res<ChemDb>,
    script: Option<Res<Script>>,
    time: Res<Time>,
    mut officers: Query<(Entity, &mut RaidOfficer, &mut CrewRoute, &Transform)>,
    containers: Query<
        (Entity, &Container, Option<&Label>, &Transform),
        (
            Without<crate::containers::HeldBy>,
            Without<crate::containers::InventorySlot>,
            Without<crate::containers::InSlot>,
            Without<crate::containers::InSlotB>,
            Without<crate::containers::InSlotC>,
            Without<crate::containers::Stored>,
            Without<crate::security_case::CaseCustody>,
        ),
    >,
    solids: Query<(&Transform, &crate::lab::Solid)>,
    nav: Option<Res<crate::nav::NavGraph>>,
    areas: Option<Res<crate::lab::WalkableAreas>>,
    lockers: Query<(Entity, &Transform), With<crate::security_case::CaseLocker>>,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
) {
    let Some(script) = script else {
        return;
    };
    for (entity, mut officer, mut route, position) in &mut officers {
        if route.phase != CrewPhase::Waiting {
            continue;
        }
        officer.dwell -= time.delta_secs();
        if officer.dwell > 0.0 {
            continue;
        }

        if officer.target.is_none() {
            let next = containers
                .iter()
                .filter(|(_, container, _, at)| {
                    !container.solution.is_empty()
                        && areas.as_ref().is_none_or(|areas| {
                            areas.room_at(at.translation).is_some_and(|room| {
                                matches!(room, "Chemistry" | "Mixing Hall" | "Reaction Bay")
                            })
                        })
                })
                .filter(|(_, _, _, at)| {
                    nav.as_ref().is_none_or(|nav| {
                        nav.path(position.translation, nav.standable_goal(at.translation))
                            .is_some()
                    })
                })
                .min_by(|a, b| {
                    a.3.translation
                        .distance_squared(position.translation)
                        .total_cmp(&b.3.translation.distance_squared(position.translation))
                });
            if let Some((target, _, _, at)) = next {
                officer.target = Some(target);
                if position.translation.xz().distance(at.translation.xz()) > 1.85 {
                    if let Some(nav) = nav.as_ref() {
                        *route = CrewRoute::to(nav.standable_goal(at.translation));
                        continue;
                    }
                }
            }
        }

        // Two passes, because whether a labelled bottle survives depends on
        // what the *rest* of the bench looks like, which is not known until
        // everything has been classified.
        let suspect: Vec<(Entity, Reads)> = containers
            .iter()
            .filter(|(_, _, _, at)| {
                position.translation.xz().distance(at.translation.xz()) <= 1.85
                    && (position.translation.y - at.translation.y).abs() < 1.1
                    && !solids.iter().any(|(solid_at, solid)| {
                        crate::interaction::authority_segment_blocked(
                            position.translation + Vec3::Y * 0.65,
                            at.translation,
                            solid_at.translation,
                            solid.half_extents,
                        )
                    })
            })
            .map(|(id, container, label, _)| (id, reads_as(&db, &container.solution, label)))
            .filter(|(_, reads)| *reads != Reads::Clean)
            .collect();

        let bare = suspect.iter().any(|(_, reads)| *reads == Reads::Bare);
        // One roll for the whole sweep rather than one per container: this is
        // a single judgement about how thorough this officer is being. Rolling
        // per bottle would mean a lab holding ten labelled beakers is caught
        // near-certainly and one holding a single beaker almost never — which
        // would punish keeping stock rather than telling a lie.
        //
        // And a bare bottle skips the roll entirely. Once the officer has
        // physically turned up contraband nobody even tried to hide, they are
        // going to read everything else properly: one careless beaker blows
        // the cover on every careful one beside it.
        let seized = !lockers.is_empty()
            && (bare
                || (!suspect.is_empty()
                    && officer_reads_the_labels(
                        shift.standing(Department::Security),
                        rand::rng().random::<f64>(),
                    )));

        if seized {
            for (id, _) in &suspect {
                if let Some((locker, at)) = lockers.iter().next() {
                    commands.entity(*id).insert((
                        crate::containers::Stored(locker),
                        crate::security_case::CaseCustody(0),
                        Transform::from_translation(at.translation),
                        Visibility::Hidden,
                    ));
                }
            }
        }

        // Forgery is the more serious finding whenever it is among what was
        // seized, however the officer came to look that closely.
        let forged = seized && suspect.iter().any(|(_, reads)| *reads == Reads::Covered);

        if seized {
            let penalty = if forged {
                FORGERY_PENALTY
            } else {
                RAID_PENALTY
            };
            let line = if forged {
                &script.forged_line
            } else {
                &script.confiscation_line
            };
            shift.adjust(Department::Security, penalty);
            radio.push(
                RadioEntry::new(crate::radio::RadioChannel::Security, line.clone())
                    .speaker("Officer Reyes")
                    .negative(),
            );
            info!("security raid: contraband confiscated, {penalty} standing (forged: {forged})");
        } else {
            // Identical whether the bench was clean or the labels held. The
            // player is not told which, and never should be.
            radio.push(
                RadioEntry::new(
                    crate::radio::RadioChannel::Security,
                    script.clean_line.clone(),
                )
                .speaker("Officer Reyes")
                .positive(),
            );
            info!("security raid: clean ({} covered)", suspect.len());
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
        app.world_mut().spawn((
            crate::security_case::CaseLocker,
            Transform::from_xyz(20.0, 0.5, 0.0),
        ));
        app
    }

    fn officer_waiting(app: &mut App, dwell: f32) -> Entity {
        let mut route = CrewRoute::arrival(0.0);
        route.phase = CrewPhase::Waiting;
        app.world_mut()
            .spawn((
                RaidOfficer {
                    dwell,
                    target: None,
                },
                route,
                Transform::from_xyz(0.0, 0.93, 0.0),
            ))
            .id()
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
        app.world_mut()
            .resource_mut::<SecuritySuspicion>()
            .restore(threshold);

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
        app.world_mut()
            .resource_mut::<SecuritySuspicion>()
            .restore(threshold);
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
        app.world_mut()
            .spawn((container, Transform::from_xyz(1.0, 1.0, 0.0)))
            .id()
    }

    /// What a crew member would expect to read on a bottle of this, taken
    /// from the database rather than hardcoded, so renaming a reagent in
    /// `chem.reagents.ron` cannot quietly turn these labels into gibberish
    /// that claims nothing and passes for the wrong reason.
    fn display_name(app: &App, reagent: &str) -> String {
        let db = app.world().resource::<ChemDb>();
        db.reagents.get(db.reagent(reagent)).name.clone()
    }

    fn beaker_marked(app: &mut App, reagent: &str, amount: i32, claim: &str) -> Entity {
        let text = display_name(app, claim);
        let beaker = beaker_of(app, reagent, amount);
        app.world_mut().entity_mut(beaker).insert(Label(text));
        beaker
    }

    /// Puts Security's standing at exactly `standing`, which is what decides
    /// how hard the officer looks.
    fn security_thinks(app: &mut App, standing: i32) {
        app.world_mut()
            .resource_mut::<Shift>()
            .adjust(Department::Security, standing);
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .standing(Department::Security),
            standing,
            "test setup: Security's standing is the input to the whole sweep"
        );
    }

    fn in_custody(app: &App, beaker: Entity) -> bool {
        // Existing tests ask whether the bottle was removed from the worktop.
        // Custody now preserves the actual solution instead of clearing it.
        app.world()
            .get::<crate::security_case::CaseCustody>(beaker)
            .is_some()
    }

    /// The two ends of the trust ladder where the sweep is deterministic, so
    /// the end-to-end tests below need no seeded RNG. Both are asserted
    /// rather than assumed, because they are properties of
    /// `orders::trust_in_a_label`, which lives in another module.
    const TAKES_YOUR_WORD: i32 = 6;
    const READS_EVERYTHING: i32 = crate::estrangement::RECONCILED_AT;

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
            in_custody(&app, beaker),
            "contraband should be preserved in custody"
        );
        assert_eq!(
            app.world()
                .get::<Container>(beaker)
                .unwrap()
                .solution
                .total_volume(),
            Units::whole(15)
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

    // -----------------------------------------------------------------------
    // Labels
    // -----------------------------------------------------------------------

    #[test]
    fn the_two_ends_of_the_ladder_really_are_certain() {
        // Everything below leans on these, and they are decided in `orders`,
        // so a change there must break this test rather than silently make
        // the end-to-end tests flaky.
        for roll in [0.0, 0.5, 0.999_999] {
            assert!(
                !officer_reads_the_labels(TAKES_YOUR_WORD, roll),
                "an officer who thinks well of the lab takes the label at its word"
            );
            assert!(
                officer_reads_the_labels(READS_EVERYTHING, roll),
                "an officer who has been burned reads every bottle"
            );
        }
    }

    #[test]
    fn between_those_ends_the_label_shifts_the_odds_without_settling_them() {
        // Neutral standing: sometimes read, sometimes believed. The point of
        // the middle band is that labelling contraband is a gamble, not a
        // switch.
        assert!(
            !officer_reads_the_labels(0, 0.1),
            "a distracted glance at neutral standing should believe the label"
        );
        assert!(
            officer_reads_the_labels(0, 0.9),
            "a careful look at neutral standing should catch it"
        );
    }

    #[test]
    fn a_labelled_bottle_survives_a_sweep_that_does_not_look_properly() {
        let mut app = sweep_app();
        security_thinks(&mut app, TAKES_YOUR_WORD);
        officer_waiting(&mut app, 0.5);
        let beaker = beaker_marked(&mut app, "space_drugs", 15, "bicaridine");

        advance(&mut app, 1.0);

        assert!(
            !in_custody(&app, beaker),
            "an officer who takes the label at its word has no reason to seize the bottle"
        );
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .standing(Department::Security),
            TAKES_YOUR_WORD,
            "a sweep that finds nothing costs nothing"
        );
    }

    #[test]
    fn the_report_on_a_forged_bottle_that_held_is_the_same_one_a_clean_bench_gets() {
        // The deception is worthless if the radio announces that it worked.
        let mut fooled = sweep_app();
        security_thinks(&mut fooled, TAKES_YOUR_WORD);
        officer_waiting(&mut fooled, 0.5);
        beaker_marked(&mut fooled, "space_drugs", 15, "bicaridine");
        advance(&mut fooled, 1.0);

        let mut clean = sweep_app();
        security_thinks(&mut clean, TAKES_YOUR_WORD);
        officer_waiting(&mut clean, 0.5);
        beaker_of(&mut clean, "kelotane", 20);
        advance(&mut clean, 1.0);

        let line_of = |app: &App| app.world().resource::<RadioLog>().entries[0].text.clone();
        assert_eq!(
            line_of(&fooled),
            line_of(&clean),
            "the player must not be able to tell a cover story that held from an empty bench"
        );
    }

    #[test]
    fn a_bottle_with_nothing_written_on_it_has_no_cover_story() {
        // Even at the top of the ladder: there is no label to believe, so
        // thoroughness never enters into it.
        let mut app = sweep_app();
        security_thinks(&mut app, TAKES_YOUR_WORD);
        officer_waiting(&mut app, 0.5);
        let beaker = beaker_of(&mut app, "space_drugs", 15);

        advance(&mut app, 1.0);

        assert!(
            in_custody(&app, beaker),
            "unlabelled contraband is always seized"
        );
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .standing(Department::Security),
            TAKES_YOUR_WORD + RAID_PENALTY,
            "carelessness costs the ordinary raid penalty, not the forgery one"
        );
    }

    #[test]
    fn vague_reassurance_on_a_label_is_not_a_cover_story() {
        // "Painkiller" names no chemical, so it claims nothing — the same
        // rule `orders::claimed_reagent` applies at the counter.
        let mut app = sweep_app();
        security_thinks(&mut app, TAKES_YOUR_WORD);
        officer_waiting(&mut app, 0.5);
        let beaker = beaker_of(&mut app, "space_drugs", 15);
        app.world_mut()
            .entity_mut(beaker)
            .insert(Label("Painkiller".to_string()));

        advance(&mut app, 1.0);

        assert!(
            in_custody(&app, beaker),
            "a label has to name something specific to be worth believing"
        );
    }

    #[test]
    fn labelling_one_drug_as_another_drug_is_no_cover_at_all() {
        // Cover is judged by the same contraband rule as contents, so a
        // bottle marked with a chemical Security would seize anyway reads as
        // exactly what it is.
        let mut app = sweep_app();
        security_thinks(&mut app, TAKES_YOUR_WORD);
        officer_waiting(&mut app, 0.5);
        let beaker = beaker_marked(&mut app, "space_drugs", 15, "methamphetamine");

        advance(&mut app, 1.0);

        assert!(
            in_custody(&app, beaker),
            "claiming to be a different controlled substance is still claiming to be contraband"
        );
    }

    #[test]
    fn one_careless_bottle_blows_the_cover_on_every_careful_one_beside_it() {
        let mut app = sweep_app();
        security_thinks(&mut app, TAKES_YOUR_WORD);
        officer_waiting(&mut app, 0.5);
        let careful = beaker_marked(&mut app, "space_drugs", 15, "bicaridine");
        let careless = beaker_of(&mut app, "space_drugs", 15);

        advance(&mut app, 1.0);

        assert!(in_custody(&app, careless));
        assert!(
            in_custody(&app, careful),
            "an officer holding contraband nobody hid will read everything else properly"
        );
    }

    #[test]
    fn security_reads_the_bottle_properly_once_you_have_burned_them() {
        let mut app = sweep_app();
        security_thinks(&mut app, READS_EVERYTHING);
        officer_waiting(&mut app, 0.5);
        let beaker = beaker_marked(&mut app, "space_drugs", 15, "bicaridine");

        advance(&mut app, 1.0);

        assert!(
            in_custody(&app, beaker),
            "the relationship is what lets you lie to them; burn it and the label is just ink"
        );
    }

    #[test]
    fn being_caught_forging_costs_more_than_being_caught_careless() {
        // Same contraband, same officer, same standing — the only difference
        // is whether there was a lie written on the bottle.
        let cost = |labelled: bool| {
            let mut app = sweep_app();
            security_thinks(&mut app, READS_EVERYTHING);
            officer_waiting(&mut app, 0.5);
            if labelled {
                beaker_marked(&mut app, "space_drugs", 15, "bicaridine");
            } else {
                beaker_of(&mut app, "space_drugs", 15);
            }
            advance(&mut app, 1.0);
            app.world()
                .resource::<Shift>()
                .standing(Department::Security)
                - READS_EVERYTHING
        };

        let careless = cost(false);
        let forging = cost(true);
        assert!(
            forging < careless,
            "lying to the department that would arrest you should hurt more than \
             leaving a beaker out: forging cost {forging}, carelessness cost {careless}"
        );
    }

    #[test]
    fn a_forged_label_never_makes_a_legal_bottle_suspicious() {
        // The safety property in the other direction: labels only ever help
        // the sweep decide *what a suspect bottle is*. A container with
        // nothing contraband in it is `Clean` before any label is consulted,
        // so writing "Methamphetamine" on water cannot manufacture a raid.
        let mut app = sweep_app();
        security_thinks(&mut app, READS_EVERYTHING);
        officer_waiting(&mut app, 0.5);
        let beaker = beaker_marked(&mut app, "kelotane", 20, "methamphetamine");

        advance(&mut app, 1.0);

        assert!(
            !in_custody(&app, beaker),
            "there was nothing in it to seize"
        );
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .standing(Department::Security),
            READS_EVERYTHING,
            "a sweep judges contents; a label only ever explains them"
        );
    }
}
