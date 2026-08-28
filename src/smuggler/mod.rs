//! Cargo's minor antagonist: a tech with a side business.
//!
//! One of the five department shenanigan threads. Unlike a main antagonist
//! (`crate::cult`, gated on the save having drawn it), a department minor runs
//! in **every** save, in both modes, always — the station is never so busy
//! with an existential threat that Cargo stops being Cargo.
//!
//! Built on the [`crate::obsessed`] template: a recurring named identity kept
//! off `station.crew.ron`, an ordered chain of authored visits, advanced by
//! matching the resolution's name. What is its own is the hook: **ignore them
//! and they help themselves.** A visit that expires unfilled lifts an
//! unattended container off the counter — not one you are holding, and not one
//! sitting in a machine slot. You lose the batch, and you find out by looking
//! for it.
//!
//! That is deliberately the cheapest possible consequence to *avoid*: hold the
//! beaker, or put it in the window. It is a nudge toward tidiness, not a tax.

use bevy::prelude::*;
use rand::prelude::*;
use serde::Deserialize;

use crate::chem_data::ChemDb;
use crate::containers::{Container, HeldBy, InSlot, Stored};
use crate::crew::{spawn_crew_member, CrewDef, CrewMember, CrewPhase, CrewPosts, CrewRoute, NotResident};
use crate::interaction::Interactable;
use crate::net::is_authority;
use crate::orders::{OrderResolved, Shift, StationData};
use crate::player::Chemist;
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::shift::current_rules;
use crate::threat;
use crate::AppState;

/// How often, independent of the scripted-visit cadence, a chance is rolled
/// to send the smuggler's identity visibly loitering between visits — pure
/// atmosphere, no mechanical effect, matching `obsessed`'s own established
/// "costs nothing mechanically" precedent for a minor's ambient beat. See
/// [`loiter_smuggler`].
const LOITER_CHECK_SECONDS: (f32, f32) = (90.0, 180.0);
/// How long they stand there before wandering off.
const LOITER_DWELL_SECONDS: f32 = 20.0;

pub struct SmugglerPlugin;

impl Plugin for SmugglerPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(threat::ScriptPlugin::<SmugglerScript>::new(
            "data/station.smuggler.ron",
            "smuggler.ron",
        ))
            .init_resource::<SmugglerProgress>()
            .add_systems(
                OnEnter(AppState::Playing),
                (arm_spawner, arm_loiter_spawner),
            )
            .add_systems(
                Update,
                (
                    generate_smuggler_visit,
                    handle_smuggler_resolution,
                    loiter_smuggler,
                    expire_smuggler_loitering,
                )
                    .chain()
                    .after(threat::PromoteScripts)
                    .run_if(is_authority)
                    // No `arc::is_active` gate, unlike a main antagonist —
                    // see the module doc.
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// Which authored visit fires next. Persisted, same as
/// `obsessed::ObsessedProgress`.
#[derive(Resource, Default, Clone, Copy)]
pub struct SmugglerProgress(pub usize);

/// `assets/data/station.smuggler.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct SmugglerScript {
    /// Kept off `station.crew.ron` for the same reason every other recurring
    /// identity is: so an ordinary order can never double-book them.
    pub name: String,
    pub role: String,
    pub color: [f32; 3],
    pub gap_multiplier: (f32, f32),
    pub visits: Vec<SmugglerVisitDef>,
    /// Aired when a visit expires and they take something instead.
    pub theft_lines: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SmugglerVisitDef {
    pub reagent: String,
    pub amount: u32,
    pub plea: String,
}

/// This thread's authored script, once loaded.
type Script = threat::Authored<SmugglerScript>;

#[derive(Resource)]
struct SmugglerSpawner {
    timer: Timer,
}

/// See `threat::arm_first_visit` for why this has to re-run on
/// `OnEnter(AppState::Playing)` every session rather than only once at
/// process start.
fn arm_spawner(mut commands: Commands) {
    threat::arm_first_visit(&mut commands, threat::MINOR_FIRST_VISIT, |timer| {
        SmugglerSpawner { timer }
    });
}

/// The clock between loitering appearances — its own, independent of
/// [`SmugglerSpawner`]'s scripted-visit cadence.
#[derive(Resource)]
struct SmugglerLoiterSpawner {
    timer: Timer,
}

fn arm_loiter_spawner(mut commands: Commands) {
    threat::arm_first_visit(&mut commands, LOITER_CHECK_SECONDS, |timer| {
        SmugglerLoiterSpawner { timer }
    });
}

/// Marks the smuggler's identity while it is standing around doing nothing
/// in particular. Ticked down by [`expire_smuggler_loitering`], which sends
/// them on their way once it runs out — the same "walk_route despawns a
/// Leaving crew member once their route finishes" cleanup every other
/// visitor already gets for free, see `crew::walk_route`.
#[derive(Component)]
struct Loitering {
    dwell: f32,
}

/// Sends the smuggler's identity to visibly loiter at an authored
/// `"loiter"`-kind `crew_post` between scripted visits — a real behavioural
/// tell an attentive player can learn to notice, entirely outside the
/// antagonist/offer economy's own hidden invariant, since department minors
/// were never bound by "no visible tell" in the first place: their whole
/// fiction is already "you find out by looking," not "indistinguishable
/// from legitimate."
fn loiter_smuggler(
    mut commands: Commands,
    time: Res<Time>,
    script: Option<Res<Script>>,
    mut spawner: Option<ResMut<SmugglerLoiterSpawner>>,
    crew_posts: Res<CrewPosts>,
    shift: Res<Shift>,
    present: Query<&CrewMember, NotResident>,
) {
    let (Some(script), Some(spawner)) = (script, spawner.as_mut()) else {
        return;
    };
    if !shift.accepting_orders {
        return;
    }
    if !spawner.timer.tick(time.delta()).just_finished() {
        return;
    }

    let mut rng = rand::rng();
    spawner.timer = Timer::from_seconds(
        rng.random_range(LOITER_CHECK_SECONDS.0..=LOITER_CHECK_SECONDS.1),
        TimerMode::Once,
    );

    // One of them is plenty — if a scripted visit already has them at the
    // counter, skip this round rather than putting two of them in the room.
    if present.iter().any(|member| member.name == script.name) {
        return;
    }
    let Some(spot) = crew_posts.random_loiter() else {
        return;
    };

    let def = CrewDef {
        name: script.name.clone(),
        role: script.role.clone(),
        color: script.color,
    };
    let entity = spawn_crew_member(&mut commands, &def, 0.0);
    commands.entity(entity).insert((
        Loitering {
            dwell: LOITER_DWELL_SECONDS,
        },
        Interactable::new("Cargo tech, killing time off the manifest."),
    ));
    // Overwrite the default counter-bound arrival route: they are here to be
    // seen, not to be served.
    commands.entity(entity).insert(CrewRoute::to(spot));
}

/// Ticks the loiter dwell down once they've actually arrived, and sends them
/// off once it runs out.
fn expire_smuggler_loitering(time: Res<Time>, mut loitering: Query<(&mut Loitering, &mut CrewRoute)>) {
    let dt = time.delta_secs();
    for (mut loiter, mut route) in &mut loitering {
        if route.phase != CrewPhase::Waiting {
            continue;
        }
        loiter.dwell -= dt;
        if loiter.dwell <= 0.0 {
            route.leave();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn generate_smuggler_visit(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    script: Option<Res<Script>>,
    mut spawner: Option<ResMut<SmugglerSpawner>>,
    progress: Res<SmugglerProgress>,
    shift: Res<Shift>,
    mut radio: ResMut<RadioLog>,
    chemists: Query<(), With<Chemist>>,
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
        warn!("smuggler visit names unknown reagent '{}'", visit.reagent);
        return;
    };

    threat::spawn_scripted_visit(
        &mut commands,
        &db,
        &mut rng,
        &rules,
        threat::ScriptedVisit {
            name: &script.name,
            role: &script.role,
            color: script.color,
            reagent,
            amount_units: visit.amount,
            plea: visit.plea.clone(),
        },
    );

    radio.push(
        RadioEntry::new(channel_for(&script.role), visit.plea.clone())
            .speaker(&script.name)
            .negative(),
    );
}

/// Containers nobody is holding and nothing is holding.
///
/// Deliberately excludes anything held, slotted or shut in a locker: they lift
/// what is *unattended*, which is what makes "hold onto it" a real answer — and
/// putting it away is the same answer, with the locker as the thing that makes
/// it available to a chemist who has to walk off and do something else.
type LooseGlassware<'w, 's> = Query<
    'w,
    's,
    Entity,
    (
        With<Container>,
        Without<HeldBy>,
        Without<InSlot>,
        Without<Stored>,
    ),
>;

/// Advances the chain — and, on an expired visit, takes something.
///
/// Only [`Outcome::Expired`] triggers the theft, not a wrong delivery:
/// handing them the wrong thing is a mistake, ignoring them entirely is an
/// invitation.
#[allow(clippy::too_many_arguments)]
fn handle_smuggler_resolution(
    mut commands: Commands,
    script: Option<Res<Script>>,
    arc_script: Option<Res<crate::arc::Script>>,
    campaign: Option<ResMut<crate::arc::Campaign>>,
    instability: Option<ResMut<crate::instability::Instability>>,
    mut resolved: MessageReader<OrderResolved>,
    mut progress: ResMut<SmugglerProgress>,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
    loose: LooseGlassware,
) {
    let Some(script) = script else {
        resolved.clear();
        return;
    };
    let mut campaign = campaign;
    let mut instability = instability;

    // Ignored fires it, and a spent visit is spent however it graded.
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

        // A `ChainOfCustody` requisition absorbs one theft before it happens.
        if threat::ward_absorbed(
            &mut shift,
            &mut radio,
            threat::Ward::Smuggler,
            RadioEntry::new(
                channel_for(&script.role),
                "Cargo's paperwork actually matched, for once.",
            )
            .positive(),
        ) {
            continue;
        }

        let mut rng = rand::rng();
        if let Some(taken) = loose.iter().choose(&mut rng) {
            commands.entity(taken).despawn();
            let line = script
                .theft_lines
                .choose(&mut rng)
                .cloned()
                .unwrap_or_else(|| "Something has gone missing off the counter.".to_string());
            radio.push(RadioEntry::new(channel_for(&script.role), line).negative());
            info!("smuggler: {} lifted a container", script.name);
        }

        // A minor left to get on with it is a small gift to whoever the save
        // is really about — the thread that ties five department shenanigans
        // to the campaign without making any of them mandatory.
        if let (Some(arc_script), Some(campaign)) = (arc_script.as_deref(), campaign.as_mut()) {
            crate::arc::note_ignored_shenanigan(arc_script, campaign);
        }
        if let Some(instability) = instability.as_mut() {
            crate::instability::nudge_instability(
                instability,
                crate::instability::INCOMPETENCE_PER_IGNORED_SHENANIGAN,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orders::Outcome;
    use crate::containers::ContainerKind;
    use crate::orders::{Department, OrderKind};

    fn script() -> SmugglerScript {
        ron::from_str(include_str!("../../assets/data/station.smuggler.ron"))
            .expect("station.smuggler.ron should parse")
    }

    fn resolution_app() -> App {
        let mut app = App::new();
        app.insert_resource(threat::Authored(script()))
            .init_resource::<SmugglerProgress>()
            .init_resource::<Shift>()
            .init_resource::<RadioLog>()
            .init_resource::<crate::instability::Instability>()
            .add_message::<OrderResolved>()
            .add_systems(Update, handle_smuggler_resolution);
        app
    }

    fn loiter_app() -> App {
        let mut app = App::new();
        let mut posts = CrewPosts::default();
        posts.add_loiter(Vec3::new(3.0, 0.0, 4.0));
        app.insert_resource(threat::Authored(script()))
            .insert_resource(Shift {
                accepting_orders: true,
                ..Default::default()
            })
            .insert_resource(posts)
            .insert_resource(SmugglerLoiterSpawner {
                // Effectively due on the first real tick.
                timer: Timer::from_seconds(0.01, TimerMode::Once),
            })
            .init_resource::<Time>()
            .add_systems(Update, (loiter_smuggler, expire_smuggler_loitering).chain());
        app
    }

    #[test]
    fn a_loitering_visit_never_carries_an_order_or_wanders_off_before_its_dwell() {
        let mut app = loiter_app();
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(0.1));
        app.update();

        let mut spawned = app
            .world_mut()
            .query::<(&Loitering, &CrewMember, Option<&crate::orders::Order>)>();
        let (_, member, order) = spawned
            .iter(app.world())
            .next()
            .expect("a loiterer should have spawned once the spot exists and the timer is due");
        assert_eq!(member.name, app.world().resource::<Script>().0.name);
        assert!(
            order.is_none(),
            "a loitering visit must never carry an Order — no mechanical hook at all"
        );
    }

    #[test]
    fn nothing_loiters_without_an_authored_spot() {
        let mut app = loiter_app();
        app.insert_resource(CrewPosts::default()); // no loiter spots authored
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(0.1));
        app.update();

        let mut spawned = app.world_mut().query::<&Loitering>();
        assert_eq!(spawned.iter(app.world()).count(), 0);
    }

    /// A beaker sitting out on the counter with nobody holding it.
    fn loose_beaker(app: &mut App) -> Entity {
        app.world_mut()
            .spawn(Container {
                kind: ContainerKind::Beaker,
                solution: chem_sim::Solution::unbounded(),
            })
            .id()
    }

    fn resolve(app: &mut App, name: &str, outcome: Outcome) {
        app.world_mut().write_message(OrderResolved {
            name: name.to_string(),
            role: "Cargo".to_string(),
            reagent: None,
            category: None,
            outcome,
            kind: OrderKind::Normal,
        });
        app.update();
    }

    #[test]
    fn ignoring_them_costs_you_whatever_was_left_on_the_counter() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let beaker = loose_beaker(&mut app);

        resolve(&mut app, &name, Outcome::Expired);

        assert!(
            app.world().get_entity(beaker).is_err(),
            "an unattended beaker is exactly what a side business is for"
        );
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 1);
        assert_eq!(
            app.world().resource::<crate::instability::Instability>().level,
            crate::instability::INCOMPETENCE_PER_IGNORED_SHENANIGAN,
            "an ignored shenanigan is exactly the signal the instability meter watches for"
        );
    }

    #[test]
    fn a_held_beaker_is_never_taken() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let holder = app.world_mut().spawn_empty().id();
        let held = app
            .world_mut()
            .spawn((
                Container {
                    kind: ContainerKind::Beaker,
                    solution: chem_sim::Solution::unbounded(),
                },
                HeldBy(holder),
            ))
            .id();

        resolve(&mut app, &name, Outcome::Expired);

        assert!(
            app.world().get_entity(held).is_ok(),
            "holding onto it has to actually be the answer"
        );
    }

    #[test]
    fn filling_the_order_costs_nothing() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let beaker = loose_beaker(&mut app);

        resolve(&mut app, &name, Outcome::Success);

        assert!(app.world().get_entity(beaker).is_ok());
        assert_eq!(
            app.world().resource::<SmugglerProgress>().0,
            1,
            "the chain still advances — they came, they were served"
        );
    }

    #[test]
    fn a_banked_ward_absorbs_the_expiry_and_nothing_happens() {
        let mut app = resolution_app();
        let name = app.world().resource::<Script>().0.name.clone();
        let beaker = loose_beaker(&mut app);
        app.world_mut()
            .resource_mut::<Shift>()
            .requisition
            .smuggler_wards = 1;

        resolve(&mut app, &name, Outcome::Expired);

        assert!(
            app.world().get_entity(beaker).is_ok(),
            "a Chain of Custody requisition should have covered this one"
        );
        assert_eq!(
            app.world().resource::<Shift>().requisition.smuggler_wards,
            0,
            "the ward is spent, not banked indefinitely"
        );
        assert_eq!(
            app.world().resource::<SmugglerProgress>().0,
            1,
            "the chain still advances even though nothing was taken"
        );
        assert_eq!(app.world().resource::<RadioLog>().entries.len(), 1);
    }

    #[test]
    fn an_unrelated_expiry_never_costs_anything() {
        let mut app = resolution_app();
        let beaker = loose_beaker(&mut app);

        resolve(&mut app, "Dr. Vance", Outcome::Expired);

        assert!(app.world().get_entity(beaker).is_ok());
        assert_eq!(app.world().resource::<SmugglerProgress>().0, 0);
    }

    #[test]
    fn smuggler_ron_parses_and_stays_off_the_ordinary_roster() {
        let data = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap();
        let script = script();

        assert_eq!(
            Department::from_role(&script.role),
            Some(Department::Cargo),
            "this is Cargo's thread"
        );
        assert!(script.visits.len() >= 2);
        assert!(!script.theft_lines.is_empty());
        let roster: Vec<CrewDef> =
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
        }
    }
}
