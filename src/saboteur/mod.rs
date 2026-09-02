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
//! # They walk there
//!
//! This module used to contaminate a beaker chosen at random from the whole
//! world, from wherever the saboteur happened to be standing — the module did
//! not contain the word `Transform`. The batch went off while the tech was
//! still at the counter, or halfway out of the door, or on the far side of the
//! station behind two walls. The radio line was the *only* evidence, which made
//! the tell a notification rather than a thing that happened in the room.
//!
//! Now the ignored visit puts them on a [`crate::crew::Errand`] to one specific
//! beaker — the nearest one they can actually *walk* to
//! ([`crate::nav::NavGraph::nearest_reachable`], which measures the route
//! rather than the straight line, so a beaker a metre away through a wall is
//! correctly the far one). They cross the room, and the splash lands when they
//! get there.
//!
//! Everything that makes this worth doing follows from the walk being real:
//! **you can watch them coming, and you can stop them.** Pick the beaker up and
//! their errand has no target and they leave with nothing. Put it in a locker,
//! or in a machine slot — the same three answers this thread always had, except
//! now they are answers you can give *after* the visit expires, while a person
//! walks across the lab, instead of blind precautions taken beforehand. Sedate
//! them and they stop where they stand.
//!
//! Nothing else about the thread changed: the authored chain, its cadence, the
//! `SecondInspection` ward, and the ignored-shenanigan signal to `arc` and
//! `instability` are all as they were.

use bevy::prelude::*;
use chem_sim::Units;
use rand::prelude::*;
use serde::Deserialize;

use crate::chem_data::ChemDb;
use crate::containers::{Container, HeldBy, InSlot, Stored};
use crate::crew::{
    send_on_errand, CrewMember, CrewRoute, ErrandGoal, ErrandOutcome, ErrandResolved, NotResident,
};
use crate::nav::NavGraph;
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
                handle_meddling_arrival,
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
    threat::dispatch_scripted_visit(
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
    );
}

/// Marks the tech while they are on their way to have a fiddle.
///
/// Carries the beaker they set out for so the arrival can be told apart from
/// any other module's errand landing the same frame, and so a target that was
/// picked up on the way is a *miss* rather than a quiet substitution of
/// whatever is nearest on arrival. Deciding once, visibly, is the whole point:
/// the player is being given a specific thing to defend.
#[derive(Component)]
struct Meddling {
    beaker: Entity,
}

/// Glassware they can get at, exactly as `smuggler` defines it: anything held,
/// slotted or shut in a locker is under someone's eye, and "keep hold of it"
/// has to remain the answer.
type LooseGlassware<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static Container, &'static Transform),
    (Without<HeldBy>, Without<InSlot>, Without<Stored>),
>;

/// The same set, writable — for the splash itself. Separate only because
/// arrival needs `&mut Container` and no position, while choosing a target
/// needs the position and no write.
type GettableGlassware<'w, 's> =
    Query<'w, 's, &'static mut Container, (Without<HeldBy>, Without<InSlot>, Without<Stored>)>;

/// Advances the chain — and, on an expired visit, sets them walking.
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
    script: Option<Res<Script>>,
    nav: Option<Res<NavGraph>>,
    arc_script: Option<Res<crate::arc::Script>>,
    campaign: Option<ResMut<crate::arc::Campaign>>,
    instability: Option<ResMut<crate::instability::Instability>>,
    mut resolved: MessageReader<OrderResolved>,
    mut progress: ResMut<SaboteurProgress>,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
    loose: LooseGlassware,
    tech: Query<(Entity, &CrewMember, &Transform), NotResident>,
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

        // The body that just gave up waiting. `expire_orders` has already
        // stripped its `Order` and sent it for the door; this turns it round.
        let Some((entity, _, at)) = tech
            .iter()
            .find(|(_, member, _)| member.name == script.name)
        else {
            continue;
        };
        let Some(nav) = nav.as_deref() else {
            continue;
        };

        // Only a container that already holds something: splashing into an
        // empty beaker is not sabotage, it is a free ingredient, and the
        // player would never even notice.
        let worth_ruining = loose.iter().filter_map(|(beaker, container, transform)| {
            container
                .solution
                .total_volume()
                .is_positive()
                .then_some((beaker, transform.translation))
        });
        // Nearest by the walk, not by the straight line — see the module doc.
        let Some((beaker, _)) = nav.nearest_reachable(at.translation, worth_ruining) else {
            continue;
        };

        commands.entity(entity).insert(Meddling { beaker });
        send_on_errand(&mut commands, entity, ErrandGoal::Target(beaker));
        info!("saboteur: {} is going for the glassware", script.name);
    }
}

/// The splash, once they are standing over it.
///
/// Both outcomes end the same way — they walk out — because from the player's
/// side the difference is already visible: either the beaker changed colour or
/// it did not.
fn handle_meddling_arrival(
    mut commands: Commands,
    db: Res<ChemDb>,
    script: Option<Res<Script>>,
    mut arrivals: MessageReader<ErrandResolved>,
    mut radio: ResMut<RadioLog>,
    meddlers: Query<&Meddling>,
    mut glassware: GettableGlassware,
) {
    let Some(script) = script else {
        arrivals.clear();
        return;
    };

    for arrival in arrivals.read() {
        // Somebody else's errand — this is a shared primitive, and `smuggler`
        // and `security` will be along shortly.
        let Ok(meddling) = meddlers.get(arrival.walker) else {
            continue;
        };
        commands
            .entity(arrival.walker)
            .remove::<Meddling>()
            // Back out of the lab under their own steam. The errand took
            // their `CrewRoute` off them, so they need a fresh one or they
            // stand there forever.
            .insert(CrewRoute::leaving());

        if arrival.outcome != ErrandOutcome::Arrived {
            // Beaten to it: the beaker was picked up, put away, or is
            // somewhere they cannot get to. No line — the player already
            // knows, because they are the one holding it.
            info!("saboteur: {} found nothing to fiddle with", script.name);
            continue;
        }

        let Some(contaminant) = db.reagents.id_of(&script.contaminant) else {
            warn!(
                "saboteur names unknown contaminant '{}'",
                script.contaminant
            );
            continue;
        };
        // The specific beaker they set out for, not whatever is nearest now —
        // and only if it is *still* unattended. The filter has to be applied
        // again here, not just when the target was chosen: a beaker picked up
        // mid-walk keeps its `Transform` (it follows the hand holding it), so
        // an errand that only checked at the start would have the tech trail
        // the chemist around the lab and then meddle with the beaker they are
        // holding. Reaching a target is not the same as being allowed to touch
        // it.
        let Ok(mut container) = glassware.get_mut(meddling.beaker) else {
            info!("saboteur: {} found it already in hand", script.name);
            continue;
        };

        let amount = Units::whole(script.contaminant_units as i32);
        // Through `mutate`, not a raw `solution.add`, so the splash resolves
        // reactions and re-tints the liquid exactly as if the player had
        // poured it in themselves.
        let ph = db.reagents.get(contaminant).ph;
        container.mutate(&db, |solution| {
            let _ = solution.add_profiled(contaminant, amount, 1.0, ph);
        });

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
    use crate::containers::ContainerKind;
    use crate::crew::{CrewDef, CrewPhase, Errand};
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

    /// The whole thread, walking: resolution, errand, arrival.
    ///
    /// A real `NavGraph` and `WalkableAreas` rather than stubs. The old
    /// headless `resolution_app` could not have caught any of what this module
    /// now does, because contaminating by global query needs no floor.
    fn saboteur_app() -> App {
        let mut app = App::new();
        let areas = WalkableAreas::from_floor_plan();
        app.insert_resource(ChemDb(data()))
            .insert_resource(threat::Authored(script()))
            .insert_resource(NavGraph::build(&areas, crate::nav::NAV_RADIUS))
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
            .add_message::<ErrandResolved>()
            .add_systems(
                Update,
                (
                    handle_saboteur_resolution,
                    crate::crew::run_errands,
                    handle_meddling_arrival,
                    crate::crew::walk_route,
                )
                    .chain(),
            );
        app
    }

    fn tech(app: &mut App, at: Vec3) -> Entity {
        let name = app.world().resource::<Script>().0.name.clone();
        let role = app.world().resource::<Script>().0.role.clone();
        app.world_mut()
            .spawn((CrewMember { name, role }, Transform::from_translation(at)))
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

    /// Runs until the meddling is over, or gives up.
    fn let_them_walk(app: &mut App, walker: Entity) {
        for _ in 0..600 {
            tick(app, 0.05);
            if app.world().get::<Meddling>(walker).is_none() {
                return;
            }
        }
    }

    fn volume(app: &App, beaker: Entity) -> Units {
        app.world()
            .get::<Container>(beaker)
            .unwrap()
            .solution
            .total_volume()
    }

    // -----------------------------------------------------------------------
    // The walk
    // -----------------------------------------------------------------------

    #[test]
    fn ignoring_them_sends_them_to_a_specific_beaker_before_anything_is_ruined() {
        // The heart of the rebuild. At the instant the visit expires nothing
        // has happened to the glassware yet — a body is merely on its way, and
        // there is a window in which the player can do something about it.
        let mut app = saboteur_app();
        let walker = tech(&mut app, in_the_bay());
        let beaker = loose_batch(&mut app, in_the_bay() + Vec3::new(4.0, -0.9, 0.0));
        let before = volume(&app, beaker);

        ignored(&mut app);

        assert_eq!(
            app.world().get::<Meddling>(walker).map(|it| it.beaker),
            Some(beaker),
            "they should have set out for the beaker",
        );
        assert!(
            app.world().get::<Errand>(walker).is_some(),
            "and be walking there, not teleporting",
        );
        assert_eq!(
            volume(&app, beaker),
            before,
            "nothing may happen to it until they arrive",
        );
        assert!(
            app.world().resource::<RadioLog>().entries.is_empty(),
            "and nothing is announced before it has actually happened",
        );
    }

    #[test]
    fn once_they_get_there_the_batch_is_ruined() {
        let mut app = saboteur_app();
        let walker = tech(&mut app, in_the_bay());
        let beaker = loose_batch(&mut app, in_the_bay() + Vec3::new(4.0, -0.9, 0.0));
        let before = volume(&app, beaker);

        ignored(&mut app);
        let_them_walk(&mut app, walker);

        assert!(
            volume(&app, beaker) > before,
            "the contaminant should be in it — that is the whole visit",
        );
        assert_eq!(
            app.world().resource::<RadioLog>().entries.len(),
            1,
            "and exactly one line about it",
        );
    }

    #[test]
    fn picking_the_beaker_up_before_they_reach_it_saves_the_batch() {
        // The counterplay the walk exists to create, and the thing the old
        // global-query version could not have: by the time you knew, it had
        // already happened.
        let mut app = saboteur_app();
        let walker = tech(&mut app, in_the_bay());
        let beaker = loose_batch(&mut app, in_the_bay() + Vec3::new(6.0, -0.9, 0.0));
        let before = volume(&app, beaker);

        ignored(&mut app);
        tick(&mut app, 0.05);
        // Into the chemist's hand. Deliberately *keeping* its `Transform`,
        // because a held beaker really does still have one — it follows the
        // hand. So the tech can still walk right up to it, and the thing that
        // saves the batch is that being held puts it out of reach, not that it
        // became hard to find.
        let chemist = app.world_mut().spawn_empty().id();
        app.world_mut().entity_mut(beaker).insert(HeldBy(chemist));

        let_them_walk(&mut app, walker);

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

    #[test]
    fn anyone_who_sets_off_leaves_again_whether_or_not_they_managed_it() {
        // The leak this guards: the errand takes their `CrewRoute` off them,
        // so without a fresh one they stand in the lab forever. `security`'s
        // raid officer already did that once, and it is why `showdown`'s
        // cleanup is unconditional. Both endings of a started errand have to
        // put a route back — the miss just as much as the hit, since the miss
        // is the one a distracted reader forgets.
        for beaten_to_it in [false, true] {
            let mut app = saboteur_app();
            let walker = tech(&mut app, in_the_bay());
            let beaker = loose_batch(&mut app, in_the_bay() + Vec3::new(6.0, -0.9, 0.0));

            ignored(&mut app);
            assert!(
                app.world().get::<Meddling>(walker).is_some(),
                "beaten_to_it={beaten_to_it}: they have to have set off for this to prove anything",
            );
            if beaten_to_it {
                tick(&mut app, 0.05);
                app.world_mut().entity_mut(beaker).despawn();
            }
            let_them_walk(&mut app, walker);

            let leaving = app
                .world()
                .get::<CrewRoute>(walker)
                .map(|route| route.phase);
            assert!(
                leaving == Some(CrewPhase::Leaving) || app.world().get_entity(walker).is_err(),
                "beaten_to_it={beaten_to_it}: left standing there on {leaving:?}",
            );
        }
    }

    #[test]
    fn they_walk_to_the_beaker_they_can_actually_reach() {
        // Straight-line nearest and route-nearest disagree exactly where it
        // matters. The decoy sits a short hop away in a different room; the
        // reachable one is further off in a straight line but is the one a
        // body can walk to without going through a wall.
        let mut app = saboteur_app();
        let from = in_the_bay();
        let walker = tech(&mut app, from);
        let lobby = ROOMS[crate::lab::LOBBY].center();
        let far_but_reachable = loose_batch(&mut app, Vec3::new(lobby.x, from.y - 0.9, lobby.z));

        ignored(&mut app);

        assert_eq!(
            app.world().get::<Meddling>(walker).map(|it| it.beaker),
            Some(far_but_reachable),
            "a reachable beaker must be chosen over none at all",
        );
    }

    // -----------------------------------------------------------------------
    // The rules the thread has always had
    // -----------------------------------------------------------------------

    #[test]
    fn filling_the_order_costs_nothing() {
        let mut app = saboteur_app();
        let walker = tech(&mut app, in_the_bay());
        let beaker = loose_batch(&mut app, in_the_bay() + Vec3::new(4.0, -0.9, 0.0));
        let before = volume(&app, beaker);
        let name = app.world().resource::<Script>().0.name.clone();

        resolve(&mut app, &name, Outcome::Success);
        let_them_walk(&mut app, walker);

        assert_eq!(
            volume(&app, beaker),
            before,
            "a delivered visit is not an invitation",
        );
        assert!(app.world().get::<Meddling>(walker).is_none());
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
        loose_batch(&mut app, in_the_bay() + Vec3::new(4.0, -0.9, 0.0));
        let name = app.world().resource::<Script>().0.name.clone();

        resolve(&mut app, &name, Outcome::Wrong);

        assert!(
            app.world().get::<Meddling>(walker).is_none(),
            "only being ignored outright gives them the idea",
        );
    }

    #[test]
    fn a_banked_ward_absorbs_it_and_they_never_set_off() {
        let mut app = saboteur_app();
        let walker = tech(&mut app, in_the_bay());
        let beaker = loose_batch(&mut app, in_the_bay() + Vec3::new(4.0, -0.9, 0.0));
        let before = volume(&app, beaker);
        app.world_mut()
            .resource_mut::<Shift>()
            .requisition
            .saboteur_wards = 1;

        ignored(&mut app);
        let_them_walk(&mut app, walker);

        assert_eq!(
            volume(&app, beaker),
            before,
            "a Second Inspection requisition should have covered this one",
        );
        assert!(
            app.world().get::<Meddling>(walker).is_none(),
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
                Transform::from_translation(in_the_bay() + Vec3::new(3.0, -0.9, 0.0)),
            ))
            .id();

        ignored(&mut app);
        let_them_walk(&mut app, walker);

        assert!(
            app.world().get::<Meddling>(walker).is_none(),
            "there was nothing worth walking to",
        );
        assert!(volume(&app, empty).is_zero());
    }

    #[test]
    fn glassware_that_is_put_away_is_never_touched() {
        // Held, slotted, stored — the three answers, all still answers.
        for guard in ["held", "slotted", "stored"] {
            let mut app = saboteur_app();
            let walker = tech(&mut app, in_the_bay());
            let beaker = loose_batch(&mut app, in_the_bay() + Vec3::new(4.0, -0.9, 0.0));
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
            let_them_walk(&mut app, walker);

            assert!(
                app.world().get::<Meddling>(walker).is_none(),
                "{guard}: they should not have set out at all",
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
        let beaker = loose_batch(&mut app, in_the_bay() + Vec3::new(4.0, -0.9, 0.0));
        let before = volume(&app, beaker);

        resolve(&mut app, "Dr. Vance", Outcome::Expired);
        let_them_walk(&mut app, walker);

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
