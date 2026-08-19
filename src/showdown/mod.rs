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
use crate::audio::{EmitWorldSfx, Sfx};
use crate::body::Body;
use crate::chem_data::ChemDb;
use crate::containers::{Container, HeldBy};
use crate::crew::{spawn_crew_member, CrewDef, CrewPhase, CrewRoute};
use crate::hazards::{HazardFelt, HazardKind, SmokeCloud, SmokePayload};
use crate::interaction::{InteractRequested, Interactable};
use crate::lab::{CrisisSpots, MapReady, WalkableAreas};
use crate::machines::chemist_entity;
use crate::nav::{NavGraph, NAV_RADIUS};
use crate::net::is_authority;
use crate::orders::reference_category;
use crate::player::Chemist;
use crate::radio::{RadioEntry, RadioLog};
use crate::AppState;

/// How long a smoke cloud a breach vents lasts, relative to a reaction's own.
const BREACH_SMOKE_RADIUS: f32 = 2.6;
const BREACH_SMOKE_LIFETIME: f32 = 8.0;

/// How close an assailant has to be to land a hit.
const ASSAILANT_REACH: f32 = 1.2;
const ASSAILANT_REPATH_SECONDS: f32 = 0.25;

pub struct ShowdownPlugin;

impl Plugin for ShowdownPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<EmitWorldSfx>().add_systems(
            Update,
            (
                (
                    arm_showdown,
                    run_siege,
                    handle_breach_delivery,
                    turn_hostile_on_arrival,
                    run_pursuers,
                    resolve_showdown,
                )
                    .chain()
                    .run_if(is_authority)
                    .run_if(resource_exists::<MapReady>),
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
///
/// Shared by every hostile-NPC thread in the game, not just the showdown's
/// own [`Assailant`] — [`crate::cult::Cultist`] rides this exact component
/// too. Nothing about walking toward the nearest chemist and hitting them is
/// antagonist-specific; only what inserts `Pursuit` differs per caller.
#[derive(Component)]
pub struct Pursuit {
    speed: f32,
    /// Seconds until this one can land another hit.
    cooldown: f32,
    /// Seconds between hits once in reach — also what `cooldown` resets to.
    hit_every: f32,
    /// Brute damage dealt per hit.
    hit_brute: i32,
    /// Cached portal route to the target's last sampled position.
    path: Vec<Vec3>,
    waypoint: usize,
    /// Seconds until the moving target is sampled and the route is rebuilt.
    repath_in: f32,
}

impl Pursuit {
    /// `cooldown` starts equal to `hit_every` — the "off cooldown the moment
    /// it arrives" behaviour `turn_hostile_on_arrival` always had.
    pub(crate) fn new(speed: f32, hit_every: f32, hit_brute: i32) -> Self {
        Self {
            speed,
            cooldown: hit_every,
            hit_every,
            hit_brute,
            path: Vec::new(),
            waypoint: 0,
            repath_in: 0.0,
        }
    }
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
    spots: Res<CrisisSpots>,
    mut missing_spot_reported: Local<bool>,
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
        ShowdownForm::Siege {
            cure_reagent,
            spot,
            guard_count,
            guard_name,
            ..
        } => {
            let Some(transform) = spots.get(spot) else {
                if !*missing_spot_reported {
                    error!("siege names missing crisis spot '{spot}'; skipping showdown");
                    *missing_spot_reported = true;
                }
                return;
            };
            commands.spawn((
                Breach,
                transform,
                Visibility::default(),
                Interactable::new(format!("Treat it — {cure_reagent} or anything like it")),
                Replicated,
                crate::until_we_leave_the_lab(),
            ));
            // Cult-only in practice — every other siege-form antagonist ships
            // `guard_count: 0` — but the check is on the count, not the
            // identity, so any future siege-form antagonist gets this for
            // free just by authoring one. Ward-scaled the same way the gas
            // and the cure are, but `.max(1)`: however well the base was
            // raided, the finale still has "someone to punch."
            if *guard_count > 0 {
                let wards = campaign.cult_incidents.iter().filter(|done| **done).count() as i32;
                let count = (*guard_count as i32 - wards / 3).max(1);
                for _ in 0..count {
                    let guard_def = CrewDef {
                        name: guard_name.clone(),
                        role: "Cult".to_string(),
                        color: [0.42, 0.05, 0.10],
                    };
                    let entity = spawn_crew_member(&mut commands, &guard_def, 0.0);
                    commands.entity(entity).insert((
                        crate::cult::Cultist {
                            wards_incident: None,
                        },
                        Replicated,
                    ));
                }
            }
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

    let wards = campaign.cult_incidents.iter().filter(|done| **done).count() as f32;
    let cult_siege = campaign.antag == crate::arc::AntagId::Cult;
    commands.insert_resource(Showdown {
        deadline: script.showdown.deadline_seconds + if cult_siege { wards * 8.0 } else { 0.0 },
        next_vent: script.showdown.gas_every_seconds + if cult_siege { wards * 1.5 } else { 0.0 },
        treated: Units::whole(0),
        needed: Units::whole(
            (script.showdown.cure_units_needed as i32
                - if cult_siege { wards as i32 * 2 } else { 0 })
            .max(6),
        ),
    });
    // Immediate, not delayed: this is the one moment in the game where the
    // player needs to know *now*.
    radio.push(
        RadioEntry::new(
            crate::radio::RadioChannel::Bridge,
            def.showdown_line.clone(),
        )
        .speaker("Duty Officer")
        .negative()
        .station_wide(),
    );
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
    let wards = campaign.cult_incidents.iter().filter(|done| **done).count() as f32;
    showdown.next_vent = script.showdown.gas_every_seconds
        + if campaign.antag == crate::arc::AntagId::Cult {
            wards * 1.5
        } else {
            0.0
        };

    let Some(gas) = db.reagents.id_of(gas_reagent) else {
        warn!("siege names unknown gas reagent '{gas_reagent}'");
        return;
    };
    let mut payload = Solution::unbounded();
    let wards = campaign.cult_incidents.iter().filter(|done| **done).count() as i32;
    let units = (script.showdown.gas_units as i32
        - if campaign.antag == crate::arc::AntagId::Cult {
            wards
        } else {
            0
        })
    .max(1);
    let _ = payload.add(gas, Units::whole(units));

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
            radio.push(
                RadioEntry::new(crate::radio::RadioChannel::Lab, "That did nothing to it.")
                    .negative()
                    .urgent(),
            );
            continue;
        }

        showdown.treated += landed;
        // The glassware goes with it, same as a delivery to a crew member.
        commands.entity(container_entity).despawn();
        radio.push(
            RadioEntry::new(
                crate::radio::RadioChannel::Lab,
                if showdown.treated >= showdown.needed {
                    "It's stopped moving.".to_string()
                } else {
                    "It recoiled from that. Keep going.".to_string()
                },
            )
            .positive()
            .urgent(),
        );
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

/// A hostile-to-be that has not stopped being an ordinary visitor yet —
/// either the showdown's own [`Assailant`] or one of [`crate::cult::Cultist`]'s
/// finale muscle. Base-guard cultists never match this: they are stationed
/// directly with `Pursuit` absent and no `CrewRoute` to arrive on, and get
/// their own proximity-based aggro in `cult::aggro_cultists` instead.
type StillPretending<'w, 's> = Query<
    'w,
    's,
    (Entity, &'static CrewRoute),
    (
        Or<(With<Assailant>, With<crate::cult::Cultist>)>,
        Without<Pursuit>,
    ),
>;

/// Drops the act once it has walked in.
///
/// It arrives like any other visitor — through the door, across to the
/// counter, on the ordinary `CrewRoute` — and only then stops being one. That
/// beat is the whole point: for a few seconds it is indistinguishable from
/// the crew, which is exactly what it has been all game.
fn turn_hostile_on_arrival(
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
            .insert(Pursuit::new(
                script.showdown.speed,
                script.showdown.hit_every_seconds,
                script.showdown.hit_brute,
            ));
    }
}

/// Walks every pursuer at whoever is nearest and hits them when it gets
/// there. Shared by both [`Assailant`] and [`crate::cult::Cultist`] — nothing
/// here is antagonist-specific; only what inserted the [`Pursuit`] differs.
///
/// The damage call is the second site of the pattern
/// `rogue_security::expire_rogue_encounters` established — through the
/// replicated [`Body`], with a `HazardFelt` alongside so `fx` kicks the
/// victim's camera. Nothing about how a chemist takes damage is new here.
#[allow(clippy::too_many_arguments)]
fn run_pursuers(
    time: Res<Time>,
    nav: Option<Res<NavGraph>>,
    areas: Option<Res<WalkableAreas>>,
    mut pursuers: Query<(&mut Transform, &mut Pursuit)>,
    mut chemists: Query<(Entity, &Transform, &mut Body, &Chemist), Without<Pursuit>>,
    mut felt: MessageWriter<ToClients<HazardFelt>>,
    mut sounds: Option<ResMut<Messages<EmitWorldSfx>>>,
) {
    let dt = time.delta_secs();

    for (mut transform, mut pursuit) in &mut pursuers {
        pursuit.cooldown -= dt;
        pursuit.repath_in -= dt;

        // Nearest by squared distance — the square root would only be needed
        // to compare against `ASSAILANT_REACH`, which is cheaper to square.
        let mut nearest: Option<(f32, Entity, Vec3)> = None;
        for (entity, chemist_transform, _, _) in &chemists {
            let offset = chemist_transform.translation - transform.translation;
            let distance = offset.length_squared();
            if nearest.is_none_or(|(best, _, _)| distance < best) {
                nearest = Some((distance, entity, chemist_transform.translation));
            }
        }
        let Some((distance, target_entity, target)) = nearest else {
            continue;
        };

        if pursuit.repath_in <= 0.0 {
            pursuit.repath_in = ASSAILANT_REPATH_SECONDS;
            pursuit.waypoint = 0;
            pursuit.path = nav
                .as_deref()
                .and_then(|graph| graph.path(transform.translation, target))
                .unwrap_or_default();
        }

        // Euclidean proximity alone is not enough: two people can be less
        // than arm's reach apart on opposite sides of a wall. The cached route
        // must both end at this sampled target and be short enough to hit.
        let route_targets_chemist = pursuit.path.last().is_some_and(|sampled| {
            Vec2::new(sampled.x - target.x, sampled.z - target.z).length_squared() <= 0.0001
        });
        let route_in_reach = remaining_route_length(transform.translation, &pursuit)
            .is_some_and(|walk| walk <= ASSAILANT_REACH);
        if distance > ASSAILANT_REACH * ASSAILANT_REACH || !route_targets_chemist || !route_in_reach
        {
            // Follow portal waypoints rather than walking straight through
            // department walls. No route deliberately means no motion: a
            // disconnected graph must not revive the old wall-phasing path.
            let mut remaining = pursuit.speed * dt;
            while remaining > 0.0 {
                let Some(waypoint) = pursuit.path.get(pursuit.waypoint).copied() else {
                    break;
                };
                let step = waypoint - transform.translation;
                let waypoint_distance = step.length();
                if waypoint_distance <= 0.02 {
                    pursuit.waypoint += 1;
                    continue;
                }
                let walked = remaining.min(waypoint_distance);
                let candidate = transform.translation + step / waypoint_distance * walked;
                transform.translation = areas.as_deref().map_or(candidate, |areas| {
                    areas.contain_on_surface(candidate, NAV_RADIUS, 0.93)
                });
                remaining -= walked;
                if walked >= waypoint_distance {
                    pursuit.waypoint += 1;
                } else {
                    break;
                }
            }
            continue;
        }
        if pursuit.cooldown > 0.0 {
            continue;
        }
        pursuit.cooldown = pursuit.hit_every;

        // In reach and off cooldown: hit whoever is in reach, once.
        for (entity, chemist_transform, mut body, chemist) in &mut chemists {
            if entity != target_entity {
                continue;
            }
            let offset = chemist_transform.translation - transform.translation;
            if offset.length_squared() > ASSAILANT_REACH * ASSAILANT_REACH {
                continue;
            }
            body.0.apply(Damage::of(
                DamageKind::Brute,
                Units::whole(pursuit.hit_brute),
            ));
            felt.write(ToClients {
                targets: SendTargets::Single(chemist.client),
                message: HazardFelt {
                    kind: HazardKind::Assault,
                    strength: 0.9,
                },
            });
            if let Some(sounds) = &mut sounds {
                sounds.write(EmitWorldSfx::new(Sfx::AssaultImpact, transform.translation));
            }
            break;
        }
    }
}

/// Length of the unwalked part of a cached route, measured on the floor plane.
fn remaining_route_length(from: Vec3, pursuit: &Pursuit) -> Option<f32> {
    if pursuit.path.is_empty() {
        return None;
    }
    let mut previous = from;
    let mut total = 0.0;
    for waypoint in pursuit.path.iter().skip(pursuit.waypoint) {
        let mut leg = *waypoint - previous;
        leg.y = 0.0;
        total += leg.length();
        previous = *waypoint;
    }
    Some(total)
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
    cultists: Query<(Entity, &Body, &crate::cult::Cultist)>,
    chemists: Query<&Body, (With<Chemist>, Without<Assailant>)>,
    mut radio: ResMut<RadioLog>,
) {
    let (Some(script), Some(mut campaign), Some(mut showdown)) = (script, campaign, showdown)
    else {
        return;
    };

    showdown.deadline -= time.delta_secs();

    // Won: the breach is treated, the assailant is down, or every piece of
    // finale muscle is (`wards_incident: None` — a base guard still standing
    // does not block this; see the cleanup below for why that is fine).
    let treated = showdown.treated >= showdown.needed && !breaches.is_empty();
    let put_down = assailants.iter().any(|(_, body)| body.0.collapsed);
    let mut finale_muscle_exists = false;
    let mut finale_muscle_down = true;
    for (_, body, cultist) in &cultists {
        if cultist.wards_incident.is_some() {
            continue;
        }
        finale_muscle_exists = true;
        finale_muscle_down &= body.0.collapsed;
    }
    let cultists_down = finale_muscle_exists && finale_muscle_down;
    // Lost: the clock ran out, or every chemist in the lab is on the floor.
    let out_of_time = showdown.deadline <= 0.0;
    let overwhelmed = !chemists.is_empty() && chemists.iter().all(|body| body.0.collapsed);

    let outcome = if treated || put_down || cultists_down {
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
    // Unconditional, and not filtered to finale muscle: once the arc ends,
    // any base guard from earlier escalation that was never put down goes
    // too — the Cult's presence in the lab is over either way.
    for (cultist, _, _) in &cultists {
        commands.entity(cultist).despawn();
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
    use crate::lab::Bounds;
    use std::time::Duration;

    const TEST_BREACH_SPOT: Vec3 = Vec3::new(13.0, 1.0, -7.0);

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
        let mut spots = CrisisSpots::default();
        spots.insert(
            "showdown.breach",
            Transform::from_translation(TEST_BREACH_SPOT),
        );

        let mut app = App::new();
        app.insert_resource(ChemDb(data()))
            .insert_resource(Script(script))
            .insert_resource(campaign)
            .insert_resource(spots)
            .init_resource::<RadioLog>()
            .init_resource::<Time>()
            .add_message::<ToClients<HazardFelt>>()
            .add_message::<FromClient<InteractRequested>>()
            .add_systems(
                Update,
                (
                    arm_showdown,
                    run_siege,
                    turn_hostile_on_arrival,
                    run_pursuers,
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

        let mut breaches = app
            .world_mut()
            .query::<(&Breach, &Interactable, &Transform)>();
        let spawned: Vec<Vec3> = breaches
            .iter(app.world())
            .map(|(_, _, transform)| transform.translation)
            .collect();
        assert_eq!(spawned, vec![TEST_BREACH_SPOT]);
    }

    #[test]
    fn a_siege_with_a_missing_spot_skips_until_the_map_supplies_it() {
        let mut app = showdown_app(SIEGE);
        app.insert_resource(CrisisSpots::default());

        advance(&mut app, 0.1);
        assert_eq!(
            app.world_mut().query::<&Breach>().iter(app.world()).count(),
            0
        );
        assert!(app.world().get_resource::<Showdown>().is_none());

        app.world_mut().resource_mut::<CrisisSpots>().insert(
            "showdown.breach",
            Transform::from_translation(TEST_BREACH_SPOT),
        );
        advance(&mut app, 0.1);
        assert_eq!(
            app.world_mut().query::<&Breach>().iter(app.world()).count(),
            1,
            "a transiently incomplete map registry must not poison the showdown for the save"
        );
    }

    #[test]
    fn cult_wards_make_the_siege_more_survivable() {
        let mut unwarded = showdown_app(SIEGE);
        let mut warded = showdown_app(SIEGE);
        warded.world_mut().resource_mut::<Campaign>().cult_incidents = vec![true; 6];
        advance(&mut unwarded, 0.1);
        advance(&mut warded, 0.1);
        let plain = unwarded.world().resource::<Showdown>();
        let safer = warded.world().resource::<Showdown>();
        assert!(safer.deadline > plain.deadline);
        assert!(safer.next_vent > plain.next_vent);
        assert!(safer.needed < plain.needed);
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
            app.world_mut()
                .query::<&Pursuit>()
                .iter(app.world())
                .count(),
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
        app.world_mut().get_mut::<CrewRoute>(entity).unwrap().phase = CrewPhase::Waiting;
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

        assert_eq!(
            app.world_mut().query::<&Breach>().iter(app.world()).count(),
            0
        );
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
                app.world_mut()
                    .query::<&Assailant>()
                    .iter(app.world())
                    .count(),
                0,
                "{antag:?} left an assailant standing after its arc ended"
            );
            assert_eq!(
                app.world_mut()
                    .query::<&crate::cult::Cultist>()
                    .iter(app.world())
                    .count(),
                0,
                "{antag:?} left a cultist standing after its arc ended"
            );
            assert!(
                app.world().get_resource::<Showdown>().is_none(),
                "{antag:?} left its showdown resource live"
            );
        }
    }

    #[test]
    fn only_cult_gets_a_finale_cultist() {
        let mut cult_app = showdown_app(SIEGE);
        advance(&mut cult_app, 0.1);
        assert!(
            cult_app
                .world_mut()
                .query::<&crate::cult::Cultist>()
                .iter(cult_app.world())
                .count()
                > 0,
            "the Cult's own siege authors guard_count > 0"
        );

        let mut assailant_app = showdown_app(ASSAILANT);
        advance(&mut assailant_app, 0.1);
        assert_eq!(
            assailant_app
                .world_mut()
                .query::<&crate::cult::Cultist>()
                .iter(assailant_app.world())
                .count(),
            0,
            "an Assailant-form antagonist has no Siege block to author guard_count on"
        );
    }

    #[test]
    fn putting_the_finale_cultists_down_wins_the_siege_without_curing_it() {
        let mut app = showdown_app(SIEGE);
        advance(&mut app, 0.1);

        let cultists: Vec<Entity> = app
            .world_mut()
            .query_filtered::<Entity, With<crate::cult::Cultist>>()
            .iter(app.world())
            .collect();
        assert!(!cultists.is_empty());
        for entity in cultists {
            app.world_mut().get_mut::<Body>(entity).unwrap().0.collapsed = true;
        }
        advance(&mut app, 0.1);

        let campaign = app.world().resource::<Campaign>();
        assert_eq!(campaign.outcome, Some(ArcOutcome::StoppedDirectly));
        assert_eq!(campaign.player_won(), Some(true));
    }

    #[test]
    fn wards_still_reduce_the_finale_cultist_count() {
        let mut unwarded = showdown_app(SIEGE);
        let mut warded = showdown_app(SIEGE);
        warded.world_mut().resource_mut::<Campaign>().cult_incidents = vec![true; 10];
        advance(&mut unwarded, 0.1);
        advance(&mut warded, 0.1);

        let plain = unwarded
            .world_mut()
            .query::<&crate::cult::Cultist>()
            .iter(unwarded.world())
            .count();
        let safer = warded
            .world_mut()
            .query::<&crate::cult::Cultist>()
            .iter(warded.world())
            .count();
        assert!(safer <= plain);
        assert!(
            safer >= 1,
            "a fully-warded save must still have someone to fight"
        );
    }

    #[test]
    fn every_antagonist_declares_a_workable_showdown() {
        let data = data();
        for def in &script().antagonists {
            assert!(!def.showdown_line.trim().is_empty());
            match &def.showdown {
                ShowdownForm::Siege {
                    spot,
                    gas_reagent,
                    cure_reagent,
                    guard_count,
                    guard_name,
                } => {
                    assert!(
                        !spot.trim().is_empty(),
                        "a siege needs an authored map spot"
                    );
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
                    if *guard_count > 0 {
                        assert!(
                            !guard_name.trim().is_empty(),
                            "a siege with guards needs a name for them"
                        );
                        let roster: Vec<CrewDef> =
                            ron::from_str(include_str!("../../assets/data/station.crew.ron"))
                                .unwrap();
                        assert!(
                            roster.iter().all(|member| member.name != *guard_name),
                            "'{guard_name}' is on the ordinary roster, so a legitimate order could pick it"
                        );
                    }
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
        assert!(
            showdown.deadline_seconds > 60.0,
            "no time to synthesise anything"
        );
        assert!(showdown.gas_every_seconds > 0.0);
        assert!(showdown.cure_units_needed > 0);
        assert!(showdown.hit_every_seconds > 0.0);
        assert!(
            showdown.hit_brute > 0 && showdown.hit_brute < 100,
            "a single hit must not be lethal, and a harmless one is not a threat"
        );
        assert!(showdown.speed > 0.0);
    }

    fn pursuit_app(areas: WalkableAreas) -> App {
        let graph = NavGraph::build(&areas, 0.0);
        let mut app = App::new();
        app.insert_resource(Script(script()))
            .insert_resource(graph)
            .insert_resource(areas)
            .init_resource::<Time>()
            .add_message::<ToClients<HazardFelt>>()
            .add_systems(Update, run_pursuers);
        app
    }

    fn spawn_pursuit_pair(app: &mut App, hunter: Vec3, target: Vec3) -> (Entity, Entity) {
        let assailant = app
            .world_mut()
            .spawn((
                Assailant,
                Transform::from_translation(hunter),
                Pursuit::new(3.0, 10.0, 8),
            ))
            .id();
        let chemist = app
            .world_mut()
            .spawn((
                Chemist {
                    client: ClientId::Server,
                },
                Body::default(),
                Transform::from_translation(target),
            ))
            .id();
        (assailant, chemist)
    }

    #[test]
    fn an_assailant_follows_portals_and_repaths_to_a_moving_chemist() {
        let mut areas = WalkableAreas::default();
        areas.push(
            Bounds {
                min_x: -1.0,
                max_x: 2.0,
                min_z: -1.0,
                max_z: 1.0,
            },
            None,
        );
        areas.push(
            Bounds {
                min_x: 1.0,
                max_x: 3.0,
                min_z: -1.0,
                max_z: 5.0,
            },
            None,
        );
        areas.push(
            Bounds {
                min_x: 1.0,
                max_x: 7.0,
                min_z: 4.0,
                max_z: 6.0,
            },
            None,
        );
        let mut app = pursuit_app(areas);
        let original_target = Vec3::new(6.0, 0.0, 5.0);
        let moved_target = Vec3::new(5.0, 0.0, 5.0);
        let (assailant, chemist) = spawn_pursuit_pair(&mut app, Vec3::ZERO, original_target);

        advance(&mut app, 0.1);
        let transform = app.world().get::<Transform>(assailant).unwrap();
        assert!(transform.translation.x > 0.0);
        assert_eq!(
            transform.translation.z, 0.0,
            "the first leg should aim for the corridor portal, not cut diagonally through the wall"
        );
        assert_eq!(
            app.world().get::<Pursuit>(assailant).unwrap().path.last(),
            Some(&original_target)
        );

        app.world_mut()
            .get_mut::<Transform>(chemist)
            .unwrap()
            .translation = moved_target;
        advance(&mut app, 0.1);
        assert_eq!(
            app.world().get::<Pursuit>(assailant).unwrap().path.last(),
            Some(&original_target),
            "the route should be cached between quarter-second samples"
        );
        advance(&mut app, 0.16);
        assert_eq!(
            app.world().get::<Pursuit>(assailant).unwrap().path.last(),
            Some(&moved_target.with_y(0.93))
        );
    }

    #[test]
    fn an_assailant_stops_when_the_nav_graph_has_no_route() {
        let mut areas = WalkableAreas::default();
        for x in [0.0, 10.0] {
            areas.push(
                Bounds {
                    min_x: x - 1.0,
                    max_x: x + 1.0,
                    min_z: -1.0,
                    max_z: 1.0,
                },
                None,
            );
        }
        let mut app = pursuit_app(areas);
        let (assailant, _) = spawn_pursuit_pair(&mut app, Vec3::ZERO, Vec3::new(10.0, 0.0, 0.0));

        advance(&mut app, 0.5);

        assert_eq!(
            app.world().get::<Transform>(assailant).unwrap().translation,
            Vec3::ZERO,
            "no route must not fall back to walking through station walls"
        );
        assert!(app
            .world()
            .get::<Pursuit>(assailant)
            .unwrap()
            .path
            .is_empty());
    }

    #[test]
    fn an_assailant_cannot_hit_a_chemist_through_a_nearby_wall() {
        let mut areas = WalkableAreas::default();
        areas.push(
            Bounds {
                min_x: -1.0,
                max_x: 0.0,
                min_z: -1.0,
                max_z: 1.0,
            },
            None,
        );
        areas.push(
            Bounds {
                min_x: 0.5,
                max_x: 1.5,
                min_z: -1.0,
                max_z: 1.0,
            },
            None,
        );
        let mut app = pursuit_app(areas);
        let (assailant, chemist) = spawn_pursuit_pair(
            &mut app,
            Vec3::new(-0.25, 0.0, 0.0),
            Vec3::new(0.75, 0.0, 0.0),
        );
        app.world_mut()
            .get_mut::<Pursuit>(assailant)
            .unwrap()
            .cooldown = 0.0;

        advance(&mut app, 0.1);

        assert_eq!(
            app.world().get::<Body>(chemist).unwrap().0.total(),
            Units::ZERO,
            "straight-line reach must not punch through a wall when navigation has no route"
        );
    }
}
