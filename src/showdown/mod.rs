//! The showdown: when the antagonist stops working through other people.
//!
//! One of the three ways a save can end (see `crate::arc`). Arms once
//! [`crate::arc::Campaign::plot`] crosses `showdown_at` and runs exactly once
//! per save: win it and the arc closes as
//! [`ArcOutcome::StoppedDirectly`](crate::arc::ArcOutcome::StoppedDirectly);
//! lose it — by running out of time or by collapsing — and the plot is pushed
//! to the top and they get what they wanted.
//!
//! Two forms, declared per antagonist in `station.arc.ron`:
//!
//! - [`ShowdownForm::Siege`] — the lab itself turns on you. A [`Breach`] opens
//!   and vents a harmful payload on a beat, through the **entirely unmodified**
//!   `hazards::SmokeCloud` pipeline, and periodically claims a machine through
//!   the same `Machine::in_use_by` field co-op already uses for two chemists
//!   reaching for the dispenser. You beat it by handing it enough of the
//!   authored counter-agent.
//! - [`ShowdownForm::Assailant`] — a body walks in and comes for you. Spawned
//!   through `spawn_crew_member` from a synthetic `CrewDef`, exactly like
//!   `security`'s raid officer and `rogue_security`'s shakedown officer already
//!   are, which means it is drawn on every peer by the existing `dress_crew`
//!   and carries a real `Body`/`Bloodstream`. **Every existing way to dose
//!   someone therefore works on it with no new code**: a syringe, a smoke cloud
//!   you set off yourself, or a spiked handover. You beat it by putting it
//!   down.
//!
//! The only genuinely new mechanic in the module is [`Pursuit`] — a walk
//! toward the nearest chemist, replacing the waypoint `CrewRoute` once the
//! assailant has finished pretending to be an ordinary visitor.

use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::{Damage, DamageKind, Solution, Units};
use serde::{Deserialize, Serialize};

use crate::arc::{ArcOutcome, Campaign, Script, ShowdownForm};
use crate::body::Body;
use crate::chem_data::ChemDb;
use crate::containers::{Container, HeldBy};
use crate::crew::{spawn_crew_member, CrewDef, CrewPhase, CrewRoute};
use crate::hazards::{HazardFelt, HazardKind, SmokeCloud, SmokePayload};
use crate::interaction::{InteractRequested, Interactable};
use crate::machines::chemist_entity;
use crate::net::is_authority;
use crate::orders::reference_category;
use crate::player::Chemist;
use crate::radio::{RadioEntry, RadioLog};
use crate::AppState;

/// Where a breach opens: across the room from the counter, so it is between
/// the chemist and nothing they need — it has to be walked *to*, not tripped
/// over.
const BREACH_SPOT: Vec3 = Vec3::new(-3.2, 1.0, 1.4);

/// How long a smoke cloud a breach vents lasts, relative to a reaction's own.
const BREACH_SMOKE_RADIUS: f32 = 2.6;
const BREACH_SMOKE_LIFETIME: f32 = 8.0;

/// How close an assailant has to be to land a hit.
const ASSAILANT_REACH: f32 = 1.2;

pub struct ShowdownPlugin;

impl Plugin for ShowdownPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (
                (
                    arm_showdown,
                    run_siege,
                    handle_breach_delivery,
                    turn_assailant_hostile,
                    run_assailant,
                    resolve_showdown,
                )
                    .chain()
                    .run_if(is_authority),
                // Presentation, on every peer. The assailant needs nothing
                // here — it is a `CrewMember`, so `crew::dress_crew` already
                // draws it — but a breach is not crew and has no mesh
                // otherwise.
                dress_breach,
            )
                .run_if(in_state(AppState::Playing)),
        );
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Marks the thing a siege opens in the lab wall.
///
/// Replicated — a breach is the least secret thing in the game, and both
/// chemists have to be able to see and treat the same one.
#[derive(Component, Serialize, Deserialize)]
pub struct Breach;

/// Marks the crew entity that turned out not to be crew.
///
/// Replicated for the same reason as [`Breach`]: without it a guest would see
/// an ordinary crew member walking through the lab and taking chemical damage
/// for no visible reason.
#[derive(Component, Serialize, Deserialize)]
pub struct Assailant;

/// Walks toward the nearest chemist. Replaces [`CrewRoute`] rather than
/// coexisting with it — two systems writing one `Transform` would fight, and
/// the waypoint route has nothing useful to say about a moving target.
#[derive(Component)]
pub struct Pursuit {
    speed: f32,
    /// Seconds until this one can land another hit.
    cooldown: f32,
}

/// The live showdown. Absent means none is running.
#[derive(Resource)]
pub struct Showdown {
    /// Seconds left before the arc is lost.
    deadline: f32,
    /// Seconds until the next gas vent (siege only).
    next_vent: f32,
    /// Counter-agent landed so far (siege only).
    treated: Units,
    /// How much is needed (siege only).
    needed: Units,
}

// ---------------------------------------------------------------------------
// Arming
// ---------------------------------------------------------------------------

/// Opens the confrontation once the plot has climbed far enough.
///
/// Runs at most once per save: it refuses while a showdown is already live,
/// and the arc it belongs to resolves the moment this one ends either way, so
/// the `outcome.is_none()` check keeps it from re-arming afterwards.
#[allow(clippy::too_many_arguments)]
fn arm_showdown(
    mut commands: Commands,
    script: Option<Res<Script>>,
    campaign: Option<Res<Campaign>>,
    live: Option<Res<Showdown>>,
    mut radio: ResMut<RadioLog>,
) {
    let (Some(script), Some(campaign), None) = (script, campaign, live) else {
        return;
    };
    if campaign.outcome.is_some() || campaign.plot < script.showdown_at {
        return;
    }
    let Some(def) = script.antagonist(campaign.antag) else {
        return;
    };

    match &def.showdown {
        ShowdownForm::Siege { cure_reagent, .. } => {
            commands.spawn((
                Breach,
                Transform::from_translation(BREACH_SPOT),
                Visibility::default(),
                Interactable::new(format!("Treat it — {cure_reagent} or anything like it")),
                Replicated,
                crate::until_we_leave_the_lab(),
            ));
        }
        ShowdownForm::Assailant { name, color } => {
            // A synthetic identity, never drawn from `station.crew.ron` — the
            // same rule `security::RaidOfficer` and `rogue_security` follow,
            // so the ordinary roster's own picks can never select it as a
            // legitimate requester.
            let def = CrewDef {
                name: name.clone(),
                role: "Security".to_string(),
                color: *color,
            };
            let entity = spawn_crew_member(&mut commands, &def, 0.0);
            commands.entity(entity).insert((Assailant, Replicated));
        }
    }

    commands.insert_resource(Showdown {
        deadline: script.showdown.deadline_seconds,
        next_vent: script.showdown.gas_every_seconds,
        treated: Units::whole(0),
        needed: Units::whole(script.showdown.cure_units_needed as i32),
    });
    // Immediate, not delayed: this is the one moment in the game where the
    // player needs to know *now*.
    radio.push(RadioEntry {
        channel: "LAB".to_string(),
        text: def.showdown_line.clone(),
        good: false,
    });
    info!("showdown armed: {:?}", campaign.antag);
}

// ---------------------------------------------------------------------------
// Siege
// ---------------------------------------------------------------------------

/// Vents gas on a beat and leans on the machines.
///
/// The gas goes out as an ordinary `hazards::SmokeCloud` + `SmokePayload`,
/// which is what makes it dose chemists *and* crew, drive the camera kick, and
/// replicate — all through code that already existed and is not touched here.
fn run_siege(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    script: Option<Res<Script>>,
    campaign: Option<Res<Campaign>>,
    showdown: Option<ResMut<Showdown>>,
    breaches: Query<&Transform, With<Breach>>,
) {
    let (Some(script), Some(campaign), Some(mut showdown)) = (script, campaign, showdown) else {
        return;
    };
    let Some(def) = script.antagonist(campaign.antag) else {
        return;
    };
    let ShowdownForm::Siege { gas_reagent, .. } = &def.showdown else {
        return;
    };
    let Ok(origin) = breaches.single().map(|t| t.translation) else {
        return;
    };

    showdown.next_vent -= time.delta_secs();
    if showdown.next_vent > 0.0 {
        return;
    }
    showdown.next_vent = script.showdown.gas_every_seconds;

    let Some(gas) = db.reagents.id_of(gas_reagent) else {
        warn!("siege names unknown gas reagent '{gas_reagent}'");
        return;
    };
    let mut payload = Solution::unbounded();
    let _ = payload.add(gas, Units::whole(script.showdown.gas_units as i32));

    commands.spawn((
        SmokeCloud {
            radius: BREACH_SMOKE_RADIUS,
            remaining: BREACH_SMOKE_LIFETIME,
        },
        SmokePayload(payload),
        Transform::from_translation(origin),
        Visibility::default(),
        Replicated,
        crate::until_we_leave_the_lab(),
    ));
}

// The plan for this milestone also had a siege periodically claiming a machine
// through `Machine::in_use_by`, to make the lab stop feeling like somewhere
// calm to work. It is deliberately **not** built: that field is released by the
// chemist who took it, so a claim made on behalf of an entity that is about to
// be despawned has nothing to release it, and the failure mode is a dispenser
// that is locked for the rest of the career. The gas and the clock are pressure
// enough, and they cannot strand anything.

/// Hands the breach whatever the chemist is carrying.
///
/// Deliberately **not** routed through `orders::complete_delivery`: that needs
/// a `CrewMember` and a `CrewRoute` to grade a `Handover` against, and a hole
/// in the wall is neither. It reuses the same *functions* instead —
/// `reference_category` for the leniency rule — so a breach accepts any member
/// of the counter-agent's category exactly like an ordinary lenient order does.
#[allow(clippy::too_many_arguments)]
fn handle_breach_delivery(
    mut commands: Commands,
    db: Res<ChemDb>,
    script: Option<Res<Script>>,
    campaign: Option<Res<Campaign>>,
    showdown: Option<ResMut<Showdown>>,
    mut requests: MessageReader<FromClient<InteractRequested>>,
    breaches: Query<(), With<Breach>>,
    containers: Query<(Entity, &Container, &HeldBy)>,
    chemists: Query<(Entity, &Chemist)>,
    mut radio: ResMut<RadioLog>,
) {
    let (Some(script), Some(campaign), Some(mut showdown)) = (script, campaign, showdown) else {
        requests.clear();
        return;
    };
    let Some(def) = script.antagonist(campaign.antag) else {
        return;
    };
    let ShowdownForm::Siege { cure_reagent, .. } = &def.showdown else {
        return;
    };
    let Some(cure) = db.reagents.id_of(cure_reagent) else {
        warn!("siege names unknown cure reagent '{cure_reagent}'");
        return;
    };

    for request in requests.read() {
        if !breaches.contains(request.target) {
            continue;
        }
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Some((container_entity, container, _)) =
            containers.iter().find(|(_, _, holder)| holder.0 == player)
        else {
            continue;
        };

        let landed = cure_units(&db, &container.solution, cure);
        if !landed.is_positive() {
            radio.push(RadioEntry {
                channel: "LAB".to_string(),
                text: "That did nothing to it.".to_string(),
                good: false,
            });
            continue;
        }

        showdown.treated += landed;
        // The glassware goes with it, same as a delivery to a crew member.
        commands.entity(container_entity).despawn();
        radio.push(RadioEntry {
            channel: "LAB".to_string(),
            text: if showdown.treated >= showdown.needed {
                "It's stopped moving.".to_string()
            } else {
                "It recoiled from that. Keep going.".to_string()
            },
            good: true,
        });
    }
}

/// How much of `solution` counts toward a cure whose reference reagent is
/// `cure` — any member of its category, the same leniency an ordinary lenient
/// order grants.
fn cure_units(db: &ChemDb, solution: &Solution, cure: chem_sim::ReagentId) -> Units {
    let category = reference_category(db, cure);
    solution
        .iter()
        .filter(|(id, amount)| {
            amount.is_positive()
                && match category {
                    Some(category) => db.reagents.get(*id).categories.contains(&category),
                    None => *id == cure,
                }
        })
        .map(|(_, amount)| amount)
        .sum()
}

// ---------------------------------------------------------------------------
// Assailant
// ---------------------------------------------------------------------------

/// An assailant that has not stopped being an ordinary visitor yet.
type StillPretending<'w, 's> =
    Query<'w, 's, (Entity, &'static CrewRoute), (With<Assailant>, Without<Pursuit>)>;

/// Drops the act once it has walked in.
///
/// It arrives like any other visitor — through the door, across to the
/// counter, on the ordinary `CrewRoute` — and only then stops being one. That
/// beat is the whole point: for a few seconds it is indistinguishable from
/// the crew, which is exactly what it has been all game.
fn turn_assailant_hostile(
    mut commands: Commands,
    script: Option<Res<Script>>,
    arrived: StillPretending,
) {
    let Some(script) = script else {
        return;
    };
    for (entity, route) in &arrived {
        if route.phase != CrewPhase::Waiting {
            continue;
        }
        commands
            .entity(entity)
            .remove::<CrewRoute>()
            .insert(Pursuit {
                speed: script.showdown.speed,
                cooldown: script.showdown.hit_every_seconds,
            });
    }
}

/// Walks it at whoever is nearest and hits them when it gets there.
///
/// The damage call is the second site of the pattern
/// `rogue_security::expire_rogue_encounters` established — through the
/// replicated [`Body`], with a `HazardFelt` alongside so `fx` kicks the
/// victim's camera. Nothing about how a chemist takes damage is new here.
#[allow(clippy::too_many_arguments)]
fn run_assailant(
    time: Res<Time>,
    script: Option<Res<Script>>,
    mut assailants: Query<(&mut Transform, &mut Pursuit), With<Assailant>>,
    mut chemists: Query<(&Transform, &mut Body, &Chemist), Without<Assailant>>,
    mut felt: MessageWriter<ToClients<HazardFelt>>,
) {
    let Some(script) = script else {
        return;
    };
    let dt = time.delta_secs();

    for (mut transform, mut pursuit) in &mut assailants {
        pursuit.cooldown -= dt;

        // Nearest by squared distance — the square root would only be needed
        // to compare against `ASSAILANT_REACH`, which is cheaper to square.
        let mut nearest: Option<(f32, Vec3)> = None;
        for (chemist_transform, _, _) in &chemists {
            let offset = chemist_transform.translation - transform.translation;
            let distance = offset.length_squared();
            if nearest.is_none_or(|(best, _)| distance < best) {
                nearest = Some((distance, chemist_transform.translation));
            }
        }
        let Some((distance, target)) = nearest else {
            continue;
        };

        if distance > ASSAILANT_REACH * ASSAILANT_REACH {
            // Walk, on the floor plane only — it has no business changing
            // height, and normalising the full vector would make it climb.
            let mut step = target - transform.translation;
            step.y = 0.0;
            if step.length_squared() > f32::EPSILON {
                transform.translation += step.normalize() * pursuit.speed * dt;
            }
            continue;
        }
        if pursuit.cooldown > 0.0 {
            continue;
        }
        pursuit.cooldown = script.showdown.hit_every_seconds;

        // In reach and off cooldown: hit whoever is in reach, once.
        for (chemist_transform, mut body, chemist) in &mut chemists {
            let offset = chemist_transform.translation - transform.translation;
            if offset.length_squared() > ASSAILANT_REACH * ASSAILANT_REACH {
                continue;
            }
            body.0.apply(Damage::of(
                DamageKind::Brute,
                Units::whole(script.showdown.hit_brute),
            ));
            felt.write(ToClients {
                targets: SendTargets::Single(chemist.client),
                message: HazardFelt {
                    kind: HazardKind::Assault,
                    strength: 0.9,
                },
            });
            break;
        }
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Ends the showdown, and with it the arc.
///
/// Whichever way it goes, the entity is despawned and the resource removed.
/// That is deliberately unconditional: this project has already been bitten
/// once by a `security::RaidOfficer` that was never removed and sat in the lab
/// forever, and a showdown that leaves its breach behind would block every
/// future one on the `arm_showdown` guard.
#[allow(clippy::too_many_arguments)]
fn resolve_showdown(
    mut commands: Commands,
    time: Res<Time>,
    script: Option<Res<Script>>,
    campaign: Option<ResMut<Campaign>>,
    showdown: Option<ResMut<Showdown>>,
    breaches: Query<Entity, With<Breach>>,
    assailants: Query<(Entity, &Body), With<Assailant>>,
    chemists: Query<&Body, (With<Chemist>, Without<Assailant>)>,
    mut radio: ResMut<RadioLog>,
) {
    let (Some(script), Some(mut campaign), Some(mut showdown)) = (script, campaign, showdown)
    else {
        return;
    };

    showdown.deadline -= time.delta_secs();

    // Won: the breach is treated, or the assailant is down.
    let treated = showdown.treated >= showdown.needed && !breaches.is_empty();
    let put_down = assailants.iter().any(|(_, body)| body.0.collapsed);
    // Lost: the clock ran out, or every chemist in the lab is on the floor.
    let out_of_time = showdown.deadline <= 0.0;
    let overwhelmed = !chemists.is_empty() && chemists.iter().all(|body| body.0.collapsed);

    let outcome = if treated || put_down {
        ArcOutcome::StoppedDirectly
    } else if out_of_time || overwhelmed {
        ArcOutcome::PlotSucceeded
    } else {
        return;
    };

    crate::arc::finish(&script, &mut campaign, outcome, &mut radio);

    for breach in &breaches {
        commands.entity(breach).despawn();
    }
    for (assailant, _) in &assailants {
        commands.entity(assailant).despawn();
    }
    commands.remove_resource::<Showdown>();
}

// ---------------------------------------------------------------------------
// Presentation
// ---------------------------------------------------------------------------

/// Gives a breach something to look at, on whichever peer it appears for.
///
/// The assailant needs no equivalent: it is a `CrewMember`, so `dress_crew`
/// already draws it — which is most of why it is built that way.
fn dress_breach(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    new: Query<Entity, Added<Breach>>,
) {
    for entity in &new {
        commands.entity(entity).insert((
            Mesh3d(meshes.add(Sphere::new(0.55))),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: Color::srgb(0.20, 0.05, 0.08),
                emissive: LinearRgba::new(0.45, 0.05, 0.10, 1.0),
                perceptual_roughness: 0.9,
                ..default()
            })),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arc::{AntagId, ArcScript, Mode};
    use std::time::Duration;

    fn script() -> ArcScript {
        ron::from_str(include_str!("../../assets/data/station.arc.ron")).unwrap()
    }

    fn data() -> chem_sim::ChemData {
        chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap()
    }

    /// A world with the arc already at the showdown threshold.
    fn showdown_app(antag: AntagId) -> App {
        let script = script();
        let steps = script.antagonist(antag).unwrap().counter_steps.len();
        let mut campaign = Campaign::new(antag, Mode::Chemist, steps);
        campaign.plot = script.showdown_at;

        let mut app = App::new();
        app.insert_resource(ChemDb(data()))
            .insert_resource(Script(script))
            .insert_resource(campaign)
            .init_resource::<RadioLog>()
            .init_resource::<Time>()
            .add_message::<ToClients<HazardFelt>>()
            .add_message::<FromClient<InteractRequested>>()
            .add_systems(
                Update,
                (
                    arm_showdown,
                    run_siege,
                    turn_assailant_hostile,
                    run_assailant,
                    resolve_showdown,
                )
                    .chain(),
            );
        app
    }

    fn advance(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_secs_f32(seconds));
        app.update();
    }

    /// The Cult's showdown is a siege; the Syndicate's is an assailant.
    const SIEGE: AntagId = AntagId::Cult;
    const ASSAILANT: AntagId = AntagId::Spy;

    #[test]
    fn a_siege_opens_a_breach_the_chemist_can_reach() {
        let mut app = showdown_app(SIEGE);
        advance(&mut app, 0.1);

        let mut breaches = app.world_mut().query::<(&Breach, &Interactable)>();
        assert_eq!(
            breaches.iter(app.world()).count(),
            1,
            "a siege should put exactly one thing in the room to treat"
        );
    }

    #[test]
    fn an_assailant_walks_in_before_it_turns() {
        let mut app = showdown_app(ASSAILANT);
        advance(&mut app, 0.1);

        let mut spawned = app.world_mut().query::<(&Assailant, &CrewRoute)>();
        assert_eq!(
            spawned.iter(app.world()).count(),
            1,
            "it should arrive on the ordinary crew route, indistinguishable from a visitor"
        );
        assert_eq!(
            app.world_mut().query::<&Pursuit>().iter(app.world()).count(),
            0,
            "it must not start hunting before it has even got through the door"
        );
    }

    #[test]
    fn an_assailant_that_has_arrived_drops_the_act() {
        let mut app = showdown_app(ASSAILANT);
        advance(&mut app, 0.1);

        let entity = app
            .world_mut()
            .query_filtered::<Entity, With<Assailant>>()
            .iter(app.world())
            .next()
            .unwrap();
        app.world_mut()
            .get_mut::<CrewRoute>(entity)
            .unwrap()
            .phase = CrewPhase::Waiting;
        advance(&mut app, 0.1);

        assert!(
            app.world().get::<Pursuit>(entity).is_some(),
            "reaching the counter is what turns it hostile"
        );
        assert!(
            app.world().get::<CrewRoute>(entity).is_none(),
            "the waypoint route has to go, or two systems fight over one transform"
        );
    }

    #[test]
    fn the_showdown_arms_once_and_only_once() {
        let mut app = showdown_app(SIEGE);
        advance(&mut app, 0.1);
        advance(&mut app, 5.0);
        advance(&mut app, 5.0);

        assert_eq!(
            app.world_mut().query::<&Breach>().iter(app.world()).count(),
            1,
            "a second breach would mean the arc could be lost twice over"
        );
    }

    #[test]
    fn nothing_arms_below_the_threshold() {
        let mut app = showdown_app(SIEGE);
        app.world_mut().resource_mut::<Campaign>().plot = 0;

        advance(&mut app, 5.0);

        assert_eq!(app.world_mut().query::<&Breach>().iter(app.world()).count(), 0);
    }

    #[test]
    fn a_siege_vents_on_its_beat() {
        let mut app = showdown_app(SIEGE);
        advance(&mut app, 0.1);
        let every = app.world().resource::<Script>().showdown.gas_every_seconds;

        advance(&mut app, every + 0.1);

        let mut clouds = app.world_mut().query::<(&SmokeCloud, &SmokePayload)>();
        let (_, payload) = clouds
            .iter(app.world())
            .next()
            .expect("a siege should actually vent something");
        assert!(
            payload.0.total_volume().is_positive(),
            "an empty cloud is a light show — it has to carry a real dose"
        );
    }

    #[test]
    fn running_out_of_time_hands_them_the_win() {
        let mut app = showdown_app(SIEGE);
        advance(&mut app, 0.1);
        let deadline = app.world().resource::<Script>().showdown.deadline_seconds;

        advance(&mut app, deadline + 1.0);

        let campaign = app.world().resource::<Campaign>();
        assert_eq!(campaign.outcome, Some(ArcOutcome::PlotSucceeded));
        assert_eq!(campaign.player_won(), Some(false));
    }

    #[test]
    fn putting_the_assailant_down_wins_the_arc() {
        let mut app = showdown_app(ASSAILANT);
        advance(&mut app, 0.1);
        let entity = app
            .world_mut()
            .query_filtered::<Entity, With<Assailant>>()
            .iter(app.world())
            .next()
            .unwrap();

        // However the dose got there — syringe, smoke, a spiked handover — the
        // body is what this reads.
        app.world_mut().get_mut::<Body>(entity).unwrap().0.collapsed = true;
        advance(&mut app, 0.1);

        let campaign = app.world().resource::<Campaign>();
        assert_eq!(campaign.outcome, Some(ArcOutcome::StoppedDirectly));
        assert_eq!(campaign.player_won(), Some(true));
    }

    #[test]
    fn a_finished_showdown_never_leaves_anything_behind() {
        // The `RaidOfficer`-stuck-in-the-lab regression, guarded on both forms.
        for antag in [SIEGE, ASSAILANT] {
            let mut app = showdown_app(antag);
            advance(&mut app, 0.1);
            let deadline = app.world().resource::<Script>().showdown.deadline_seconds;

            advance(&mut app, deadline + 1.0);
            // A second update so the despawn commands have actually applied.
            advance(&mut app, 0.1);

            assert_eq!(
                app.world_mut().query::<&Breach>().iter(app.world()).count(),
                0,
                "{antag:?} left a breach in the wall after its arc ended"
            );
            assert_eq!(
                app.world_mut().query::<&Assailant>().iter(app.world()).count(),
                0,
                "{antag:?} left an assailant standing after its arc ended"
            );
            assert!(
                app.world().get_resource::<Showdown>().is_none(),
                "{antag:?} left its showdown resource live"
            );
        }
    }

    #[test]
    fn every_antagonist_declares_a_workable_showdown() {
        let data = data();
        for def in &script().antagonists {
            assert!(!def.showdown_line.trim().is_empty());
            match &def.showdown {
                ShowdownForm::Siege {
                    gas_reagent,
                    cure_reagent,
                } => {
                    let gas = data
                        .reagents
                        .id_of(gas_reagent.as_str())
                        .unwrap_or_else(|| panic!("'{gas_reagent}' names no real reagent"));
                    let cure = data
                        .reagents
                        .id_of(cure_reagent.as_str())
                        .unwrap_or_else(|| panic!("'{cure_reagent}' names no real reagent"));
                    assert!(
                        !data.reagents.get(cure).categories.is_empty(),
                        "'{cure_reagent}' has no category, so nothing could ever count as treating it"
                    );
                    assert_ne!(
                        gas, cure,
                        "a siege you cure with the thing it is gassing you with is a joke"
                    );
                }
                ShowdownForm::Assailant { name, .. } => {
                    assert!(!name.trim().is_empty());
                    let roster: Vec<CrewDef> =
                        ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();
                    assert!(
                        roster.iter().all(|member| member.name != *name),
                        "'{name}' is on the ordinary roster, so a legitimate order could pick it"
                    );
                }
            }
        }
    }

    #[test]
    fn the_showdown_tuning_is_survivable_and_winnable() {
        let showdown = script().showdown;
        assert!(showdown.deadline_seconds > 60.0, "no time to synthesise anything");
        assert!(showdown.gas_every_seconds > 0.0);
        assert!(showdown.cure_units_needed > 0);
        assert!(showdown.hit_every_seconds > 0.0);
        assert!(
            showdown.hit_brute > 0 && showdown.hit_brute < 100,
            "a single hit must not be lethal, and a harmless one is not a threat"
        );
        assert!(showdown.speed > 0.0);
    }
}
