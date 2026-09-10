//! Engineering's minor antagonist: a tech whose "repairs" aren't.
//!
//! One of the five department shenanigan threads. Unlike a main antagonist
//! (`crate::cult`, gated on the save having drawn it), a department minor runs
//! in **every** save, in both modes, always.
//!
//! Built on the [`crate::obsessed`] template, and a sibling of
//! [`crate::smuggler`] — same shape, different hook. Ignore this one and they
//! get bored and start "checking" your glassware: a visit that expires unfilled
//! sends them to an unattended container and splashes a few units of something
//! into it.
//!
//! **The container is not destroyed.** That is the whole difference from the
//! smuggler's theft, and it is the nastier of the two: a stolen beaker is
//! obviously gone, while a contaminated one looks exactly like the batch you
//! made — right up until the delivery grades `Impure` and you cannot work out
//! why. The tell is real and visible for anyone who looks, because
//! `Container::solution` is what drives the liquid's colour.
//!
//! # What this module still owns
//!
//! The thread, and only the thread: the authored visit chain, its cadence, the
//! `SecondInspection` ward, the ignored-shenanigan signal to `arc` and
//! `instability`, and the aftermath chatter. All unchanged.
//!
//! What it no longer owns is the act. An ignored visit used to pick a beaker
//! and dispatch a body at it on a [`crate::crew::Errand`], with the splash
//! landing on arrival — which meant the outcome was decided the moment the
//! visit expired, and the walk was its animation. The only way to intervene was
//! to reach the specific beaker first.
//!
//! Now being ignored arms a [`crate::utility_ai::TamperAuthorization`]: a
//! bounded two-minute licence saying this person *would* take a safe
//! opportunity. `utility_ai::covert` owns everything after that — target
//! choice, line of sight, witness risk, the walk, and the chemistry. The act
//! competes with his ordinary work like any other candidate and can lose.
//!
//! That makes three different endings the old shape could not tell apart:
//! he does it, he is interrupted doing it, or he never finds a safe moment and
//! the window simply closes. Watching the bench is now a real defence, because
//! there is something there to watch for.
//!
//! The old answers all still work, for the same physical reasons: pick the
//! beaker up, slot it, or store it and it stops being a candidate. Sedate him
//! and he stops where he stands.
//!
//! # He lives here now
//!
//! Boyle is a `StationResident` between his beats — see
//! `utility_ai::scripted_residents`. The trigger query below is deliberately
//! *not* `NotResident`; that filter was correct while he only ever existed as a
//! visitor, and would have silently stopped matching him the moment he was
//! embodied.

use bevy::prelude::*;
use chem_sim::Units;
use rand::prelude::*;
use serde::Deserialize;

use crate::chem_data::ChemDb;
use crate::crew::CrewMember;
use crate::net::is_authority;
use crate::orders::{OrderResolved, Shift, StationData};
use crate::player::Chemist;
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::shift::current_rules;
use crate::threat;
use crate::AppState;

pub struct SaboteurPlugin;

impl Plugin for SaboteurPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(threat::ScriptPlugin::<SaboteurScript>::new(
            "data/station.saboteur.ron",
            "saboteur.ron",
        ))
        .init_resource::<SaboteurProgress>()
        .add_systems(OnEnter(AppState::Playing), arm_spawner)
        .add_systems(
            Update,
            (
                generate_saboteur_visit,
                handle_saboteur_resolution,
                air_meddling_aftermath,
            )
                .chain()
                .after(threat::PromoteScripts)
                .run_if(is_authority)
                // No `arc::is_active` gate, unlike a main antagonist —
                // see the module doc.
                .run_if(in_state(AppState::Playing))
                .run_if(crate::session::career_session),
        );
    }
}

/// Which authored visit fires next. Persisted, same as
/// `obsessed::ObsessedProgress`.
#[derive(Resource, Default, Clone, Copy)]
pub struct SaboteurProgress(pub usize);

/// `assets/data/station.saboteur.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct SaboteurScript {
    /// Kept off `station.crew.ron` for the same reason every other recurring
    /// identity is: so an ordinary order can never double-book them.
    pub name: String,
    pub role: String,
    pub color: [f32; 3],
    pub gap_multiplier: (f32, f32),
    pub visits: Vec<SaboteurVisitDef>,
    /// What gets splashed into an unattended container when they are ignored.
    /// A real reagent, so the mess is a real mess: it reacts, it colours the
    /// liquid, and it grades.
    pub contaminant: String,
    pub contaminant_units: u32,
    /// Aired when they do it.
    pub meddling_lines: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SaboteurVisitDef {
    pub reagent: String,
    pub amount: u32,
    pub plea: String,
}

/// This thread's authored script, once loaded.
type Script = threat::Authored<SaboteurScript>;

#[derive(Resource)]
struct SaboteurSpawner {
    timer: Timer,
}

/// See `threat::arm_first_visit` for why this has to re-run on
/// `OnEnter(AppState::Playing)` every session rather than only once at
/// process start.
fn arm_spawner(mut commands: Commands) {
    threat::arm_first_visit(&mut commands, threat::MINOR_FIRST_VISIT, |timer| {
        SaboteurSpawner { timer }
    });
}

#[allow(clippy::too_many_arguments)]
fn generate_saboteur_visit(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    script: Option<Res<Script>>,
    mut spawner: Option<ResMut<SaboteurSpawner>>,
    progress: Res<SaboteurProgress>,
    shift: Res<Shift>,
    chemists: Query<(), With<Chemist>>,
    mut intake: crate::order_intake::Intake,
    mut residents: crate::crew::AvailableResidents,
) {
    let (Some(station), Some(script), Some(spawner)) = (station, script, spawner.as_mut()) else {
        return;
    };

    let mut rng = rand::rng();
    let rules = current_rules(&station.config, &shift, chemists.iter().count());
    let Some(visit) = threat::due_visit(
        &time,
        &shift,
        &mut spawner.timer,
        &rules,
        &mut rng,
        script.gap_multiplier,
        threat::ChainProgress(progress.0),
        &script.visits,
    ) else {
        return;
    };
    let Some(reagent) = db.reagents.id_of(&visit.reagent) else {
        warn!("saboteur visit names unknown reagent '{}'", visit.reagent);
        return;
    };

    let Some(context) = intake.admit(
        crate::order_intake::RequestSource::Saboteur,
        &script.name,
        &mut spawner.timer,
        true,
    ) else {
        return;
    };
    if threat::dispatch_scripted_visit(
        &mut commands,
        &db,
        &mut rng,
        &rules,
        &mut residents,
        threat::ScriptedVisit {
            context,
            name: &script.name,
            role: &script.role,
            color: script.color,
            reagent,
            amount_units: visit.amount,
            plea: visit.plea.clone(),
        },
    )
    .is_none()
    {
        intake.cancel_admission(&script.name);
    }
}

/// Advances the chain — and, on an expired visit, arms one attempt.
///
/// Only [`crate::orders::Outcome::Expired`] triggers it, not a wrong delivery:
/// handing them the wrong thing is a mistake, leaving them standing there with
/// nothing to do is what gives them the idea.
///
/// Note what this function no longer does: contaminate anything. It chooses a
/// beaker and points a body at it, and that is all — the splash is
/// [`handle_meddling_arrival`]'s, once they have actually walked there.
#[allow(clippy::too_many_arguments)]
fn handle_saboteur_resolution(
    mut commands: Commands,
    db: Res<ChemDb>,
    script: Option<Res<Script>>,
    arc_script: Option<Res<crate::arc::Script>>,
    campaign: Option<ResMut<crate::arc::Campaign>>,
    instability: Option<ResMut<crate::instability::Instability>>,
    mut resolved: MessageReader<OrderResolved>,
    mut progress: ResMut<SaboteurProgress>,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
    time: Res<Time>,
    // Deliberately *not* `NotResident`.
    //
    // This filter used to be `Without<StationResident>`, which was correct
    // while Boyle only existed as a visitor. He is now an ordinary station
    // resident between his authored beats, and that filter would have stopped
    // matching him the moment he was embodied — no error, no failing test, just
    // an antagonist who silently never does anything again. The identity is the
    // name; residency is orthogonal to it.
    tech: Query<(Entity, &CrewMember, &Transform)>,
) {
    let Some(script) = script else {
        resolved.clear();
        return;
    };
    let mut campaign = campaign;
    let mut instability = instability;

    // Ignored fires it, and a spent visit is spent however it graded — the
    // opposite of `cult`, which fires on a delivery that *landed* and leaves
    // its chain where it was otherwise.
    let mut chain = threat::ChainProgress(progress.0);
    let steps = threat::step_chain(
        &mut resolved,
        &mut chain,
        &script.name,
        script.visits.len(),
        threat::Trigger::Ignored,
        threat::Advance::EveryVisit,
    );
    progress.0 = chain.0;

    for step in steps {
        if !step.fires {
            continue;
        }

        // A `SecondInspection` requisition absorbs one contamination before
        // it happens — before they even set off, so there is nothing to see.
        if threat::ward_absorbed(
            &mut shift,
            &mut radio,
            threat::Ward::Saboteur,
            RadioEntry::new(
                channel_for(&script.role),
                "Engineering signed off without touching the glassware.",
            )
            .positive(),
        ) {
            continue;
        }

        // Being ignored is the signal, whether or not there turns out to be
        // anything worth walking to. Deliberately outside the errand below:
        // an empty counter means they find nothing to meddle with, not that
        // the shift was competently run.
        if let (Some(arc_script), Some(campaign)) = (arc_script.as_deref(), campaign.as_mut()) {
            crate::arc::note_ignored_shenanigan(arc_script, campaign);
        }
        if let Some(instability) = instability.as_mut() {
            crate::instability::nudge_instability(
                instability,
                crate::instability::INCOMPETENCE_PER_IGNORED_SHENANIGAN,
            );
        }

        // The body that just gave up waiting.
        let Some((entity, _, _)) = tech
            .iter()
            .find(|(_, member, _)| member.name == script.name)
        else {
            continue;
        };
        let Some(contaminant) = db.reagents.id_of(&script.contaminant) else {
            warn!(
                "saboteur names unknown contaminant '{}'",
                script.contaminant
            );
            continue;
        };
        // The invariant this route must never break: an NPC's own supply is
        // ordinary station stock. A player-only reagent reaches NPC hands
        // through a physical player delivery or not at all, and a maintenance
        // allotment is emphatically not that.
        debug_assert!(
            !db.reagents.get(contaminant).player_only,
            "the saboteur's contaminant must be ordinary station stock",
        );

        // Arm an intent, not an outcome.
        //
        // This used to pick a beaker here and dispatch a body at it, so the
        // splash was decided the moment the visit expired and the walk was
        // just its animation. Now it grants a bounded licence: for the next two
        // minutes this person would take a safe opportunity if one presents
        // itself. Whether one ever does is the utility scorer's business, and
        // the window runs down while they do ordinary work.
        //
        // Inserting replaces any older unspent authorization rather than
        // stacking, so being ignored twice does not buy two attacks.
        commands
            .entity(entity)
            .insert(crate::utility_ai::TamperAuthorization::new(
                contaminant,
                Units::whole(script.contaminant_units as i32),
                time.elapsed_secs(),
            ));
        info!("saboteur: {} is in the mood to 'check' something", script.name);
    }
}

/// Airs the aftermath line once the utility action has actually landed.
///
/// The splash itself now belongs to `utility_ai::covert`, which owns the
/// target choice, the witness checks and the chemistry. This module keeps only
/// the thing it is the authority on: what the station says about it afterwards.
///
/// Driven by the ambiguous handling stimulus rather than by an arrival, so the
/// line cannot be aired for an attempt that was interrupted, vetoed, or never
/// found a moment — all of which used to be indistinguishable from success once
/// the errand was dispatched.
fn air_meddling_aftermath(
    script: Option<Res<Script>>,
    mut stimuli: MessageReader<crate::utility_ai::Stimulus>,
    mut radio: ResMut<RadioLog>,
    crew: Query<&CrewMember>,
) {
    let Some(script) = script else {
        stimuli.clear();
        return;
    };

    for stimulus in stimuli.read() {
        if stimulus.kind != crate::utility_ai::StimulusKind::SuspiciousHandling {
            continue;
        }
        // Only this thread's actor. The same stimulus is emitted by food
        // tampering and by every ordinary player donation.
        let is_ours = stimulus
            .actor
            .and_then(|actor| crew.get(actor).ok())
            .is_some_and(|member| member.name == script.name);
        if !is_ours {
            continue;
        }

        let line = script
            .meddling_lines
            .choose(&mut rand::rng())
            .cloned()
            .unwrap_or_else(|| "Something in the lab looks different.".to_string());
        radio.push(RadioEntry::new(channel_for(&script.role), line).negative());
        info!("saboteur: {} had a fiddle with the glassware", script.name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::containers::{Container, ContainerKind, HeldBy, InSlot, Stored};
    use crate::crew::CrewDef;
    use crate::lab::{WalkableAreas, REACTION_BAY, ROOMS};
    use crate::orders::Outcome;
    use crate::orders::{Department, OrderKind};

    fn data() -> chem_sim::ChemData {
        chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap()
    }

    fn script() -> SaboteurScript {
        ron::from_str(include_str!("../../assets/data/station.saboteur.ron"))
            .expect("station.saboteur.ron should parse")
    }

    /// Somewhere on the walkable floor, so nav has something to say about it.
    fn in_the_bay() -> Vec3 {
        let at = ROOMS[REACTION_BAY].center();
        Vec3::new(at.x, crate::crew::BODY_OFFSET, at.z)
    }

    /// A spot on the bench a couple of metres from [`in_the_bay`], **in the
    /// same room**.
    ///
    /// The room matters now. `can_see` treats two different rooms as blocked,
    /// and the Reaction Bay is only six metres wide — the old fixtures offset
    /// by four metres, which put the beaker through the west wall into the
    /// Mixing Hall. That was invisible while selection scanned the world by
    /// distance; with a real sight test it means "no candidate", and every
    /// affected test fails for a reason that has nothing to do with what it
    /// is checking.
    fn on_the_bench() -> Vec3 {
        in_the_bay() + Vec3::new(2.0, -0.9, 0.0)
    }

    /// Somewhere a person can stand in that same room, in plain view.
    fn beside_the_bench() -> Vec3 {
        in_the_bay() + Vec3::new(2.0, 0.0, 1.0)
    }

    /// The whole thread through the utility path: resolution arms an
    /// authorization, the covert provider offers a candidate, and the covert
    /// completion handler performs the act.
    ///
    /// A real `NavGraph` and `WalkableAreas` rather than stubs — selection now
    /// requires line of sight and route reachability, so a floorless harness
    /// would refuse every candidate and prove nothing.
    ///
    /// The arming half and the acting half are deliberately both real. The old
    /// harness could assert that a body set off; it could not assert that the
    /// act ever competed with ordinary work, because nothing competed.
    fn saboteur_app() -> App {
        let mut app = App::new();
        let areas = WalkableAreas::from_floor_plan();
        app.insert_resource(ChemDb(data()))
            .insert_resource(threat::Authored(script()))
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
            .insert_resource(areas)
            .init_resource::<SaboteurProgress>()
            .init_resource::<Shift>()
            .init_resource::<RadioLog>()
            .init_resource::<Time>()
            .init_resource::<crate::instability::Instability>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<crate::crew::CrewPosts>()
            .init_resource::<crate::lab::DeliveryStations>()
            .add_message::<OrderResolved>()
            .add_message::<crate::utility_ai::Stimulus>()
            .add_plugins(crate::utility_ai::CovertTestHarness)
            // The aftermath line reads the stimulus the covert act emits, so it
            // has to run after the harness's systems rather than before them —
            // a reader placed earlier in the frame sees the previous frame's
            // messages and reports the act one tick late.
            .add_systems(Update, handle_saboteur_resolution)
            .add_systems(Update, air_meddling_aftermath.after(crate::utility_ai::CovertTestSystems));
        app
    }

    /// Whether this actor currently holds an armed attempt.
    fn armed(app: &App, tech: Entity) -> bool {
        app.world()
            .get::<crate::utility_ai::TamperAuthorization>(tech)
            .is_some()
    }

    /// Boyle, as he now exists: an ordinary utility-controlled resident.
    ///
    /// The `UtilityControlBundle` is not decoration. He is a station resident
    /// between his authored beats, and the covert provider only offers work to
    /// utility agents — a bare body would be skipped, which is exactly the
    /// silent failure the `NotResident` query change exists to prevent.
    fn tech(app: &mut App, at: Vec3) -> Entity {
        let name = app.world().resource::<Script>().0.name.clone();
        let role = app.world().resource::<Script>().0.role.clone();
        app.world_mut()
            .spawn((
                CrewMember { name, role },
                Transform::from_translation(at),
                crate::crew::StationResident,
                crate::utility_ai::UtilityControlBundle::new(
                    crate::utility_ai::UtilityAgent::new(1, 0),
                ),
            ))
            .id()
    }

    /// A beaker sitting out with a real batch in it.
    fn loose_batch(app: &mut App, at: Vec3) -> Entity {
        let kelotane = app.world().resource::<ChemDb>().reagent("kelotane");
        let mut solution = chem_sim::Solution::unbounded();
        let _ = solution.add(kelotane, Units::whole(30));
        app.world_mut()
            .spawn((
                Container {
                    kind: ContainerKind::Beaker,
                    solution,
                },
                Transform::from_translation(at),
            ))
            .id()
    }

    fn tick(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(seconds));
        app.update();
    }

    fn resolve(app: &mut App, name: &str, outcome: Outcome) {
        app.world_mut().write_message(OrderResolved {
            name: name.to_string(),
            role: "Engineering".to_string(),
            reagent: None,
            category: None,
            outcome,
            kind: OrderKind::Normal,
            quality: None,
            development: false,
            campaign: None,
            counter_step: None,
        });
        tick(app, 0.016);
    }

    fn ignored(app: &mut App) {
        let name = app.world().resource::<Script>().0.name.clone();
        resolve(app, &name, Outcome::Expired);
    }

    /// Lets the offered attempt run to completion, as the selector would.
    fn let_them_act(app: &mut App, walker: Entity) -> bool {
        tick(app, 0.05);
        crate::utility_ai::complete_offered_tampering(app, walker)
    }

    fn volume(app: &App, beaker: Entity) -> Units {
        app.world()
            .get::<crate::containers::Container>(beaker)
            .unwrap()
            .solution
            .total_volume()
    }

    // -----------------------------------------------------------------------
    // The attempt
    // -----------------------------------------------------------------------

    #[test]
    fn ignoring_them_arms_an_attempt_before_anything_is_ruined() {
        // The heart of the thread, and the part that survived the migration
        // intact: at the instant the visit expires nothing has happened to the
        // glassware. Someone is merely willing, and there is a window in which
        // the player can do something about it.
        //
        // What changed is that the window is no longer a walk to a beaker
        // chosen in advance. It is two minutes of ordinary life in which a
        // *safe* opportunity may or may not present itself.
        let mut app = saboteur_app();
        let walker = tech(&mut app, in_the_bay());
        let beaker = loose_batch(&mut app, on_the_bench());
        let before = volume(&app, beaker);

        ignored(&mut app);

        assert!(armed(&app, walker), "being ignored is what arms them");
        assert_eq!(
            volume(&app, beaker),
            before,
            "nothing may happen to it merely because they are willing",
        );
        assert!(
            app.world().resource::<RadioLog>().entries.is_empty(),
            "and nothing is announced before it has actually happened",
        );
    }

    #[test]
    fn an_unobserved_opportunity_ruins_the_batch() {
        let mut app = saboteur_app();
        let walker = tech(&mut app, in_the_bay());
        let beaker = loose_batch(&mut app, on_the_bench());
        let before = volume(&app, beaker);

        ignored(&mut app);
        assert!(let_them_act(&mut app, walker), "the act was available");

        assert!(
            volume(&app, beaker) > before,
            "the contaminant should be in it — that is the whole visit",
        );
        assert_eq!(
            app.world().resource::<RadioLog>().entries.len(),
            1,
            "and exactly one line about it",
        );
        assert!(
            !armed(&app, walker),
            "one authorization is one attempt, not a standing licence",
        );
    }

    #[test]
    fn picking_the_beaker_up_saves_the_batch() {
        // The counterplay, preserved exactly. A held beaker keeps its
        // `Transform` — it follows the hand — so what saves the batch is that
        // being held puts it out of reach, not that it became hard to find.
        let mut app = saboteur_app();
        let walker = tech(&mut app, in_the_bay());
        let beaker = loose_batch(&mut app, on_the_bench());
        let before = volume(&app, beaker);

        ignored(&mut app);
        let chemist = app.world_mut().spawn_empty().id();
        app.world_mut()
            .entity_mut(beaker)
            .insert(crate::containers::HeldBy(chemist));

        let_them_act(&mut app, walker);

        assert_eq!(
            volume(&app, beaker),
            before,
            "holding onto it has to actually be the answer",
        );
        assert!(
            app.world().resource::<RadioLog>().entries.is_empty(),
            "nothing happened, so nothing is announced",
        );
    }

    /// New with the migration, and the reason it was worth doing: the act now
    /// competes for a safe moment instead of being decided in advance. Someone
    /// standing at the bench is enough.
    #[test]
    fn they_will_not_do_it_while_someone_is_watching() {
        let mut app = saboteur_app();
        let walker = tech(&mut app, in_the_bay());
        let beaker = loose_batch(&mut app, on_the_bench());
        let before = volume(&app, beaker);

        // A colleague at the bench, in plain view of both.
        app.world_mut().spawn((
            CrewMember {
                name: "Someone Else".into(),
                role: "Engineering".into(),
            },
            Transform::from_translation(beside_the_bench()),
        ));

        ignored(&mut app);
        tick(&mut app, 0.05);

        assert!(
            crate::utility_ai::offered_tampering(&app, walker).is_none(),
            "a watched bench is not an opportunity",
        );
        assert_eq!(volume(&app, beaker), before);
        assert!(
            armed(&app, walker),
            "they stay willing — they just have not had a chance",
        );
    }

    /// The window is what makes surveillance work as a defence. It elapses
    /// while they do ordinary work, so an attempt that never finds its moment
    /// simply ends.
    #[test]
    fn an_attempt_that_never_finds_its_moment_expires() {
        let mut app = saboteur_app();
        let walker = tech(&mut app, in_the_bay());
        let beaker = loose_batch(&mut app, on_the_bench());
        let before = volume(&app, beaker);

        // Watched the entire time.
        app.world_mut().spawn((
            CrewMember {
                name: "Someone Else".into(),
                role: "Engineering".into(),
            },
            Transform::from_translation(beside_the_bench()),
        ));

        ignored(&mut app);
        assert!(armed(&app, walker));
        tick(&mut app, 121.0);

        assert!(!armed(&app, walker), "the window closed on them");
        assert_eq!(
            volume(&app, beaker),
            before,
            "and it closed without anything happening",
        );
    }

    /// The silent failure this whole packet had to land atomically to avoid.
    ///
    /// `handle_saboteur_resolution` used to find its actor with a
    /// `NotResident` filter — `Without<StationResident>`. Boyle is now an
    /// ordinary resident between his beats, so that filter stops matching him
    /// the moment he is embodied: no error, no panic, no failing test, just a
    /// thread that never fires again. Nothing else in the suite would have
    /// caught it, because every other test spawns a bare visitor.
    ///
    /// Asserted with the residency *explicitly* present rather than relying on
    /// `tech` to keep inserting it.
    #[test]
    fn the_thread_still_fires_for_a_tech_who_lives_here() {
        let mut app = saboteur_app();
        let walker = tech(&mut app, in_the_bay());
        assert!(
            app.world().get::<crate::crew::StationResident>(walker).is_some(),
            "this test is only meaningful for an embodied resident",
        );
        loose_batch(&mut app, on_the_bench());

        ignored(&mut app);

        assert!(
            armed(&app, walker),
            "being a resident must not make him invisible to his own thread",
        );
    }

    #[test]
    fn they_choose_the_beaker_they_can_actually_reach() {
        // Straight-line nearest and route-nearest disagree exactly where it
        // matters, and `nearest_reachable` still decides.
        let mut app = saboteur_app();
        let walker = tech(&mut app, in_the_bay());
        // In the room, in view, and walkable to. The decoy is the same beaker
        // one room over: visible on no route the body can take, so
        // `nearest_reachable` never returns it and `can_see` never offers it.
        let reachable = loose_batch(&mut app, on_the_bench());
        let _through_the_wall = loose_batch(&mut app, in_the_bay() + Vec3::new(4.0, -0.9, 0.0));

        ignored(&mut app);
        tick(&mut app, 0.05);

        assert!(
            crate::utility_ai::tampering_targets(&app, walker, reachable),
            "the beaker they can actually get to must be the one chosen",
        );
    }

    // -----------------------------------------------------------------------
    // The rules the thread has always had
    // -----------------------------------------------------------------------

    #[test]
    fn filling_the_order_costs_nothing() {
        let mut app = saboteur_app();
        let walker = tech(&mut app, in_the_bay());
        let beaker = loose_batch(&mut app, on_the_bench());
        let before = volume(&app, beaker);
        let name = app.world().resource::<Script>().0.name.clone();

        resolve(&mut app, &name, Outcome::Success);
        let_them_act(&mut app, walker);

        assert_eq!(
            volume(&app, beaker),
            before,
            "a delivered visit is not an invitation",
        );
        assert!(!armed(&app, walker));
        assert_eq!(
            app.world().resource::<SaboteurProgress>().0,
            1,
            "the chain still advances — they came, they were served",
        );
    }

    #[test]
    fn a_wrong_delivery_is_a_mistake_rather_than_an_invitation() {
        let mut app = saboteur_app();
        let walker = tech(&mut app, in_the_bay());
        loose_batch(&mut app, on_the_bench());
        let name = app.world().resource::<Script>().0.name.clone();

        resolve(&mut app, &name, Outcome::Wrong);

        assert!(
            !armed(&app, walker),
            "only being ignored outright gives them the idea",
        );
    }

    #[test]
    fn a_banked_ward_absorbs_it_and_they_never_set_off() {
        let mut app = saboteur_app();
        let walker = tech(&mut app, in_the_bay());
        let beaker = loose_batch(&mut app, on_the_bench());
        let before = volume(&app, beaker);
        app.world_mut()
            .resource_mut::<Shift>()
            .requisition
            .saboteur_wards = 1;

        ignored(&mut app);
        let_them_act(&mut app, walker);

        assert_eq!(
            volume(&app, beaker),
            before,
            "a Second Inspection requisition should have covered this one",
        );
        assert!(
            !armed(&app, walker),
            "an absorbed contamination is one that never starts",
        );
        assert_eq!(
            app.world().resource::<Shift>().requisition.saboteur_wards,
            0,
            "the ward is spent, not banked indefinitely",
        );
        assert_eq!(
            app.world().resource::<SaboteurProgress>().0,
            1,
            "the chain still advances even though nothing was ruined",
        );
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 1);
    }

    #[test]
    fn an_empty_beaker_is_a_free_ingredient_rather_than_sabotage() {
        let mut app = saboteur_app();
        let walker = tech(&mut app, in_the_bay());
        let empty = app
            .world_mut()
            .spawn((
                Container {
                    kind: ContainerKind::Beaker,
                    solution: chem_sim::Solution::unbounded(),
                },
                Transform::from_translation(on_the_bench()),
            ))
            .id();

        ignored(&mut app);
        let_them_act(&mut app, walker);

        assert!(
            crate::utility_ai::offered_tampering(&app, walker).is_none(),
            "an empty beaker is a free ingredient, not sabotage",
        );
        assert!(volume(&app, empty).is_zero());
        // Still willing — they simply found nothing worth doing. Under the old
        // errand this was indistinguishable from having acted, because being
        // dispatched was the only state there was.
        assert!(armed(&app, walker));
    }

    #[test]
    fn glassware_that_is_put_away_is_never_touched() {
        // Held, slotted, stored — the three answers, all still answers.
        for guard in ["held", "slotted", "stored"] {
            let mut app = saboteur_app();
            let walker = tech(&mut app, in_the_bay());
            let beaker = loose_batch(&mut app, on_the_bench());
            let before = volume(&app, beaker);
            let elsewhere = app.world_mut().spawn_empty().id();
            let mut entity = app.world_mut().entity_mut(beaker);
            match guard {
                "held" => {
                    entity.insert(HeldBy(elsewhere));
                }
                "slotted" => {
                    entity.insert(InSlot(elsewhere));
                }
                _ => {
                    entity.insert(Stored(elsewhere));
                }
            }

            ignored(&mut app);
            let_them_act(&mut app, walker);

            assert!(
                crate::utility_ai::offered_tampering(&app, walker).is_none(),
                "{guard}: it should not even be a candidate",
            );
            assert_eq!(
                volume(&app, beaker),
                before,
                "{guard}: it was got at anyway"
            );
        }
    }

    #[test]
    fn an_unrelated_expiry_never_costs_anything() {
        let mut app = saboteur_app();
        let walker = tech(&mut app, in_the_bay());
        let beaker = loose_batch(&mut app, on_the_bench());
        let before = volume(&app, beaker);

        resolve(&mut app, "Dr. Vance", Outcome::Expired);
        let_them_act(&mut app, walker);

        assert_eq!(volume(&app, beaker), before);
        assert_eq!(app.world().resource::<SaboteurProgress>().0, 0);
    }

    #[test]
    fn being_ignored_is_the_signal_even_when_there_is_nothing_to_ruin() {
        // The nudge to `arc` and `instability` hangs off the *visit* being
        // ignored, not off the meddling landing. A tidy lab is not a
        // competently run one.
        let mut app = saboteur_app();
        tech(&mut app, in_the_bay());

        ignored(&mut app);

        assert_eq!(
            app.world()
                .resource::<crate::instability::Instability>()
                .value,
            crate::instability::STABILITY_MAX
                - crate::instability::INCOMPETENCE_PER_IGNORED_SHENANIGAN as f32,
            "an ignored shenanigan is exactly the signal the instability meter watches for",
        );
    }

    #[test]
    fn saboteur_ron_parses_and_stays_off_the_ordinary_roster() {
        let data = data();
        let script = script();

        assert_eq!(
            Department::from_role(&script.role),
            Some(Department::Engineering),
            "this is Engineering's thread",
        );
        assert!(script.visits.len() >= 2);
        assert!(!script.meddling_lines.is_empty());
        assert!(
            data.reagents.id_of(&script.contaminant).is_some(),
            "'{}' names no real reagent",
            script.contaminant,
        );
        assert!(script.contaminant_units > 0);
        let roster: Vec<CrewDef> =
            ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();
        assert!(
            roster.iter().all(|member| member.name != script.name),
            "'{}' must stay off the ordinary roster or it could double-book",
            script.name,
        );
        for visit in &script.visits {
            assert!(
                data.reagents.id_of(&visit.reagent).is_some(),
                "'{}' names no real reagent",
                visit.reagent,
            );
            assert!(!visit.plea.trim().is_empty());
        }
    }
}
