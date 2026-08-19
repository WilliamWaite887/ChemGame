//! Crew who come to the lab to collect what they ordered.
//!
//! Movement follows the station's portal graph. Callers provide destinations
//! such as the counter or a department, and navigation supplies safe doorway
//! and corridor waypoints.

use std::collections::HashSet;

use bevy::ecs::entity::MapEntities;
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::StatusKind;
use serde::{Deserialize, Serialize};

use crate::body::{Bloodstream, Body, COLLAPSE_PENALTY};
use crate::interaction::{Interactable, InteractionMode};
use crate::lab::{DeliveryLane, DeliveryStations, MapReady, COUNTER_SPOT, DOOR_MAX_X, DOOR_MIN_X};
use crate::machines::chemist_entity;
use crate::net::is_authority;
use crate::orders::{Department, Shift};
use crate::player::Chemist;
use crate::radio::{RadioEntry, RadioLog};
use crate::AppState;

/// Walking pace, metres per second.
const WALK_SPEED: f32 = 2.1;
/// Close enough to count as arrived.
const ARRIVE_EPSILON: f32 = 0.12;

pub struct CrewPlugin;

impl Plugin for CrewPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Departments>()
            .add_systems(OnEnter(AppState::Playing), load_crew_assets)
            .add_systems(
                Update,
                (
                    // The walk is simulated once, on the server; clients
                    // receive the resulting Transform. `handle_crew_collapse`
                    // reads `Body`, which only the server ever mutates
                    // (metabolism, smoke, a delivered dose), so it belongs on
                    // the same side.
                    (
                        start_crew_at_their_department,
                        // Ambient crew decide where to be, then everyone walks.
                        populate_departments.run_if(resource_exists_and_changed::<Departments>),
                        react_to_chemical_statuses,
                        sync_medical_evacuation_prompt,
                        handle_medical_evacuation,
                        ambient_behaviour,
                        walk_route,
                        handle_crew_collapse,
                    )
                        .chain()
                        .run_if(is_authority)
                        .run_if(resource_exists::<MapReady>),
                    // Runs everywhere: a crew member who arrived by
                    // replication needs a body drawing just as much as one
                    // spawned locally.
                    dress_crew,
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// Where each department's crew belong, from the map's `department_spot`
/// markers.
///
/// Empty in a build without the map, and every read here falls back — which is
/// what keeps crew arriving out of the patch of nothing south of the lobby
/// exactly as they always have. Keyed on the `role` that is already on every
/// [`CrewMember`], so a department needs no second roster to exist.
#[derive(Resource, Default)]
pub struct Departments {
    homes: std::collections::HashMap<String, Vec3>,
}

impl Departments {
    // Only the map backend has anywhere to put a department, so a plain build
    // fills this from nothing and never calls it outside tests.
    #[cfg_attr(not(feature = "trenchbroom"), allow(dead_code))]
    pub fn set(&mut self, department: String, at: Vec3) {
        self.homes.insert(department, at);
    }

    /// Where someone of this role lives, if the station has somewhere for them.
    pub fn home(&self, role: &str) -> Option<Vec3> {
        self.homes.get(role).copied()
    }

    /// Somewhere on the station to go next — another department if there is
    /// one, otherwise their own. Keeps idle crew crossing the corridor instead
    /// of pacing a single room.
    pub fn somewhere_else(&self, role: &str) -> Option<Vec3> {
        if self.homes.is_empty() {
            return None;
        }
        let elsewhere: Vec<&Vec3> = self
            .homes
            .iter()
            .filter(|(department, _)| department.as_str() != role)
            .map(|(_, at)| at)
            .collect();

        // Two thirds of the time go visiting, otherwise stay in your own
        // department — a station where everybody is always somewhere else reads
        // as odd as one where nobody moves.
        if elsewhere.is_empty() || rand::random_range(0..3) == 0 {
            return self.home(role);
        }
        elsewhere
            .get(rand::random_range(0..elsewhere.len()))
            .map(|at| **at)
    }
}

/// A crew member as written in `assets/data/station.crew.ron`.
#[derive(Clone, Debug, Deserialize)]
pub struct CrewDef {
    pub name: String,
    pub role: String,
    pub color: [f32; 3],
}

#[derive(Component, Clone, Serialize, Deserialize)]
pub struct CrewMember {
    pub name: String,
    pub role: String,
}

/// Where a crew member is in their visit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CrewPhase {
    Arriving,
    Waiting,
    Leaving,
}

#[derive(Component)]
pub struct CrewRoute {
    waypoints: Vec<Vec3>,
    index: usize,
    pub phase: CrewPhase,
    /// Where they are headed, before anyone has worked out how to get there.
    ///
    /// Destinations are set from `orders` and `addiction`, which have no
    /// business knowing about navigation, so the route records the *goal* and
    /// [`walk_route`] turns it into waypoints once — it is the system that can
    /// see the nav graph.
    pending: Option<Vec3>,
    /// True only for an order/visitor walking to a delivery window. Ambient
    /// residents also use `Arriving`, so phase alone cannot identify a queue.
    counter_bound: bool,
    pub delivery_lane: DeliveryLane,
    lane_offset: f32,
}

impl CrewRoute {
    /// The walk in: to their place at the counter.
    pub fn arrival(lane: f32) -> Self {
        Self::arrival_for(DeliveryLane::Public, lane)
    }

    pub fn arrival_for(delivery_lane: DeliveryLane, lane_offset: f32) -> Self {
        CrewRoute {
            waypoints: Vec::new(),
            index: 0,
            phase: CrewPhase::Arriving,
            pending: Some(Vec3::new(COUNTER_SPOT.x + lane_offset, 0.0, COUNTER_SPOT.z)),
            counter_bound: true,
            delivery_lane,
            lane_offset,
        }
    }

    /// Sends them back out to the station.
    pub fn leave(&mut self) {
        self.waypoints.clear();
        self.index = 0;
        self.phase = CrewPhase::Leaving;
        self.pending = Some(Vec3::new(door_x(), 0.0, spawn_z()));
        self.counter_bound = false;
    }
}

/// Marks a crew member as having reached the counter — the one bit of
/// [`CrewRoute::phase`] the order queue HUD needs. `CrewRoute` itself
/// deliberately stays server-side (see the comment in [`spawn_crew_member`]):
/// its waypoints and pending destination are simulation detail nobody else
/// needs, but a client still has to tell "on the way" from "waiting, and the
/// clock is running" to draw its own copy of the queue. Inserted the instant
/// [`walk_route`] flips `phase` to [`CrewPhase::Waiting`] and never removed —
/// once an order is delivered or expires its `Order` component goes with it,
/// which is what actually drops a crew member out of the queue, on both ends.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtCounter(pub DeliveryLane);

/// Authority-owned indication that pressing Use on this resident requests a
/// medical evacuation rather than delivering whatever happens to be held.
/// Replicated so a remote chemist routes the input to the dedicated request
/// and sees the same interaction prompt as the host.
#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct NeedsMedicalEvacuation;

/// A chemist asks Medical to remove an incapacitated resident.
///
/// The sender is deliberately absent. [`FromClient`] supplies the connection
/// identity, and [`handle_medical_evacuation`] resolves that to the authority's
/// chemist entity before validating consciousness and reach.
#[derive(Message, Clone, Debug, Serialize, Deserialize, MapEntities)]
pub struct EvacuateCrewRequested {
    #[entities]
    pub target: Entity,
}

/// Server-local copy of the prompt displaced by the temporary evacuation
/// action. If the sedative clears first, an order or incident interaction is
/// restored exactly; a successful evacuation despawns the complete entity and
/// therefore cleanly terminates that prior state.
#[derive(Component)]
struct EvacuationPromptState {
    previous: Option<String>,
}

fn door_x() -> f32 {
    (DOOR_MIN_X + DOOR_MAX_X) * 0.5
}

/// Far enough beyond the lobby's south wall to be out of sight, so crew are
/// never seen popping into existence.
fn spawn_z() -> f32 {
    crate::lab::ROOMS[crate::lab::LOBBY].max_z + 2.5
}

/// Shared meshes for crew bodies.
///
/// No materials here since M12 — every crew member's uniform and skin are a
/// fresh [`StandardMaterial`] per instance (see [`dress_crew`]), for the same
/// reason `player::ChemistAssets` dropped its shared `coat`/`skin` handles:
/// tinting a status onto a shared handle would tint everyone holding it.
#[derive(Resource)]
pub struct CrewAssets {
    body: Handle<Mesh>,
    head: Handle<Mesh>,
}

fn load_crew_assets(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>) {
    commands.insert_resource(CrewAssets {
        body: meshes.add(Capsule3d::new(0.28, 0.85)),
        head: meshes.add(Sphere::new(0.19)),
    });
}

/// A body part, and the crew member it belongs to.
///
/// The M12 twin of `player::ChemistBody`, and needed for the same reason:
/// `fx::animate_crew_body` has to wobble/tint a *child* mesh, never the root
/// `Transform`, because the root is replicated and authoritative —
/// `CrewRoute`'s movement and every interaction raycast depend on it staying
/// exactly where the server put it. `rest`/`base_color` are the part's
/// un-animated reference point, recomputed from every frame rather than
/// accumulated onto, so nothing drifts.
#[derive(Component)]
pub(crate) struct CrewBody {
    pub(crate) crew: Entity,
    pub(crate) rest: Vec3,
    pub(crate) base_color: Color,
}

/// Spawns a crew member outside the door, walking in.
///
/// No mesh: who is visiting is shared state and replicates, what they look
/// like is derived from the roster both ends already loaded. See
/// [`dress_crew`].
///
/// Carries a `Body`/`Bloodstream` since M12 — the same components a chemist
/// has, replicated the same way (`register_replication` marks the *type*,
/// not the entity, so inserting them here is all that is needed). This is
/// what lets a delivery or a smoke cloud actually land on the person it was
/// meant for, instead of a crew member being immune to their own chemistry by
/// construction.
pub fn spawn_crew_member(commands: &mut Commands, def: &CrewDef, lane: f32) -> Entity {
    let position = Vec3::new(door_x(), 0.93, spawn_z());
    commands
        .spawn((
            CrewMember {
                name: def.name.clone(),
                role: def.role.clone(),
            },
            CrewRoute::arrival_for(
                if def.role == "Medical" {
                    DeliveryLane::Medical
                } else {
                    DeliveryLane::Public
                },
                lane,
            ),
            Transform::from_translation(position),
            Body::default(),
            Bloodstream::default(),
            // The route stays server-side; clients see the resulting Transform.
            bevy_replicon::prelude::Replicated,
            crate::until_we_leave_the_lab(),
        ))
        .id()
}

/// Gives a crew member their body, head and uniform.
///
/// The uniform colour is looked up from the roster by name rather than sent,
/// because both ends load `station.crew.ron` anyway. A visitor whose name is
/// not in the roster still gets a body, in grey — an unrecognised name should
/// read as an oddity at the counter, not an invisible person holding an order.
///
/// Both meshes are children (`CrewBody`), not — as the body mesh used to be —
/// inserted straight onto the root. A wobble applied to the root would
/// corrupt the replicated, authoritative `Transform` `CrewRoute` and every
/// interaction raycast depend on; a child can be animated freely.
fn dress_crew(
    mut commands: Commands,
    assets: Option<Res<CrewAssets>>,
    station: Option<Res<crate::orders::StationData>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    crew: Query<(Entity, &CrewMember), Added<CrewMember>>,
) {
    let Some(assets) = assets else {
        return;
    };

    for (entity, member) in &crew {
        let color = station
            .as_ref()
            .and_then(|station| {
                station
                    .crew
                    .iter()
                    .find(|def| def.name == member.name)
                    .map(|def| def.color)
            })
            .unwrap_or([0.55, 0.55, 0.58]);

        let [r, g, b] = color;
        let uniform_color = Color::srgb(r, g, b);

        // A replicated crew member arrives without `Visibility` — presentation
        // is not on the wire — and a parent with none cannot propagate it to
        // the children below. Mirrors `player::dress_chemists`'s identical fix.
        commands.entity(entity).insert_if_new(Visibility::default());

        let body_rest = Vec3::ZERO;
        commands.spawn((
            Mesh3d(assets.body.clone()),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: uniform_color,
                perceptual_roughness: 0.75,
                ..default()
            })),
            Transform::from_translation(body_rest),
            Visibility::default(),
            CrewBody {
                crew: entity,
                rest: body_rest,
                base_color: uniform_color,
            },
            ChildOf(entity),
        ));

        let head_rest = Vec3::new(0.0, 0.62, 0.0);
        commands.spawn((
            Mesh3d(assets.head.clone()),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: crate::player::SKIN_COLOR,
                perceptual_roughness: 0.8,
                ..default()
            })),
            Transform::from_translation(head_rest),
            Visibility::default(),
            CrewBody {
                crew: entity,
                rest: head_rest,
                base_color: crate::player::SKIN_COLOR,
            },
            ChildOf(entity),
        ));
    }
}

/// The crew equivalent of `body::handle_collapse` — no dropped item (a crew
/// member never holds anything of the player's), no `MedbayRetrieval` (they
/// are a visitor, not staff on shift): they simply leave, early and rattled,
/// the same way the security officer's sweep already sends a crew entity back
/// out via `.leave()`.
///
/// Dinged against Medical, same as a chemist's own collapse — it is a medical
/// mishap regardless of whose lab it happened in.
fn handle_crew_collapse(
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
    mut crew: Query<(&Body, &CrewMember, &mut CrewRoute), Changed<Body>>,
) {
    for (body, member, mut route) in &mut crew {
        if !body.0.collapsed || route.phase == CrewPhase::Leaving {
            continue;
        }
        route.leave();
        shift.adjust(Department::Medical, COLLAPSE_PENALTY);
        radio.push(
            RadioEntry::new(
                crate::radio::RadioChannel::Medical,
                format!(
                    "{} just went down in the chem lab. Get them out of there.",
                    member.name
                ),
            )
            .speaker("Nurse Okonkwo")
            .negative()
            .urgent(),
        );
    }
}

/// Advances each crew member along their waypoints, and despawns them once
/// they are back outside.
/// A crew member who lives on the station rather than visiting the lab.
///
/// Ambient crew never queue and never carry an [`Order`](crate::orders::Order):
/// they walk between their department and the corridor, and they are what makes
/// the place feel inhabited rather than a shop with a door. In every other
/// respect they are ordinary [`CrewMember`]s — `dress_crew` draws them, smoke
/// reaches them, they can get hooked — which is the point. Reacting to a crisis
/// is only interesting if the people reacting were already there.
///
/// Every query that hunts for a customer at the counter excludes them by this
/// marker; see the `Without<Ambient>` filters in `orders`, `produce`, `quack`
/// and `rogue_security`.
#[derive(Component)]
pub struct Ambient {
    /// Seconds to stand still before picking somewhere new to be.
    dwell: f32,
}

impl Ambient {
    /// A dwell of `0.0` is a legitimate value, not a footgun: `ambient_behaviour`
    /// needs `&mut CrewRoute` to do anything, so a caller that also strips
    /// `CrewRoute` (a stationed guard, say) gets a component that only ever
    /// does the one job callers actually want from it here — being excluded
    /// from [`NotResident`] — with zero risk of triggering wander behaviour.
    pub(crate) fn new(dwell: f32) -> Self {
        Self { dwell }
    }
}

/// Query filter for crew who are *visiting* the lab, excluding the residents
/// who simply live on the station.
///
/// Every gate that counts how busy the counter is has to use this. Counting
/// bare [`CrewMember`]s instead includes the residents — already more than
/// `orders::max_active_cap` on its own — and orders stop arriving entirely,
/// with no error and nothing in the log. That is exactly how it broke the first
/// time.
pub type NotResident = Without<Ambient>;

/// How long an idle crew member lingers before moving on.
const DWELL_SECONDS: (f32, f32) = (4.0, 11.0);

/// Gives every department someone to be in it.
///
/// Runs when [`Departments`] is filled, which only ever happens under the map —
/// a build without one has nowhere for anybody to live, and gets the old
/// visit-only crew exactly as before.
fn populate_departments(
    mut commands: Commands,
    departments: Res<Departments>,
    station: Option<Res<crate::orders::StationData>>,
    resident: Query<&CrewMember, With<Ambient>>,
) {
    let Some(station) = station else {
        return;
    };

    for def in &station.crew {
        let Some(home) = departments.home(&def.role) else {
            continue;
        };
        // One resident per person on the roster, not per department: the roster
        // is already the cast, and spawning a second Dr. Vance would put the
        // same named individual in two places.
        if resident.iter().any(|member| member.name == def.name) {
            continue;
        }

        let crew = spawn_crew_member(&mut commands, def, 0.0);
        commands.entity(crew).insert(Ambient {
            dwell: rand::random_range(DWELL_SECONDS.0..=DWELL_SECONDS.1),
        });
        // Overwrite the arrival route: they are not coming to the counter.
        commands.entity(crew).insert(CrewRoute {
            waypoints: Vec::new(),
            index: 0,
            phase: CrewPhase::Arriving,
            pending: Some(home),
            counter_bound: false,
            delivery_lane: DeliveryLane::Public,
            lane_offset: 0.0,
        });
    }
}

/// Sends idle crew somewhere new, and sends everyone to their post when a
/// casualty turns up.
///
/// The rule is the whole of the station's mood in one branch: if your
/// department could do something about it you go *to* it, and if it could not
/// you get out of the way. Nobody runs for the escape pod — that is the end of
/// a campaign, not a bad afternoon.
fn ambient_behaviour(
    time: Res<Time>,
    departments: Res<Departments>,
    crisis: Query<(&Transform, &crate::crisis::CrisisResponse)>,
    mut residents: Query<(&CrewMember, &mut Ambient, &mut CrewRoute, &Transform)>,
) {
    let emergency = crisis.iter().next();

    for (member, mut ambient, mut route, transform) in &mut residents {
        // Mid-walk. Leave them to it.
        if route.pending.is_some() || route.index < route.waypoints.len() {
            continue;
        }

        if let Some((casualty, response)) = emergency {
            let wanted = response.responders.iter().any(|role| role == &member.role);
            let post = if wanted {
                Some(casualty.translation)
            } else {
                departments.home(&member.role)
            };

            if let Some(post) = post {
                // Only walk if they are not already standing there, or they
                // shuffle on the spot for the whole crisis.
                let flat = Vec3::new(post.x, transform.translation.y, post.z);
                if transform.translation.distance(flat) > 1.5 {
                    route.pending = Some(post);
                    route.counter_bound = false;
                    route.phase = CrewPhase::Arriving;
                }
                continue;
            }
        }

        ambient.dwell -= time.delta_secs();
        if ambient.dwell > 0.0 {
            continue;
        }
        ambient.dwell = rand::random_range(DWELL_SECONDS.0..=DWELL_SECONDS.1);

        // Somewhere else on the station: another department, or their own.
        if let Some(next) = departments.somewhere_else(&member.role) {
            route.pending = Some(next);
            route.counter_bound = false;
            route.phase = CrewPhase::Arriving;
        }
    }
}

/// Puts a newly spawned crew member at their department, if the station has one
/// for them.
///
/// A system rather than an argument to [`spawn_crew_member`], which has fourteen
/// call sites across thirteen modules — none of which should have to learn about
/// the station's layout to ask for a customer. They spawn off-screen either way,
/// so moving them the same frame is invisible.
fn start_crew_at_their_department(
    departments: Res<Departments>,
    mut arriving: Query<(&CrewMember, &mut Transform), Added<CrewRoute>>,
) {
    for (member, mut transform) in &mut arriving {
        if let Some(home) = departments.home(&member.role) {
            transform.translation.x = home.x;
            transform.translation.z = home.z;
        }
    }
}

/// Crew respond to mind/body chemistry instead of merely carrying a hidden
/// bloodstream. Drowsy people abandon their visit, paranoid people flee, and
/// a chemically incapacitated person remains down until the sedative clears;
/// their already-marked leaving route then resumes toward help.
fn react_to_chemical_statuses(
    mut crew: Query<(&Bloodstream, &mut CrewRoute, Option<&mut Ambient>)>,
) {
    for (blood, mut route, ambient) in &mut crew {
        let sedated = blood.0.status(StatusKind::Sedated).intensity > 0.0;
        let paranoid = blood.0.status(StatusKind::Paranoid).intensity > 0.0;
        if route.phase != CrewPhase::Leaving && (sedated || paranoid || blood.0.incapacitated()) {
            route.leave();
        }

        // Euphoric residents linger instead of immediately resuming their
        // station circuit: benign and social, distinct from drunken
        // staggering or paranoia's flight response.
        if blood.0.status(StatusKind::Euphoric).intensity > 0.0
            && route.pending.is_none()
            && route.index >= route.waypoints.len()
        {
            if let Some(mut ambient) = ambient {
                ambient.dwell = ambient.dwell.max(2.5);
            }
        }
    }
}

fn evacuation_prompt(member: &CrewMember) -> String {
    format!("Evacuate {} to Medical", member.name)
}

/// Temporarily replaces a resident's ordinary interaction with evacuation.
///
/// This is authority-owned rather than inferred only in the HUD: the marker
/// and [`Interactable`] both replicate, so a guest can focus the resident and
/// sends the same dedicated request as the host. The displaced prompt is
/// retained locally and restored if treatment wakes the resident first.
fn sync_medical_evacuation_prompt(
    mut commands: Commands,
    mut crew: Query<(
        Entity,
        &CrewMember,
        &Bloodstream,
        Option<&mut Interactable>,
        Option<&mut EvacuationPromptState>,
        Has<NeedsMedicalEvacuation>,
    )>,
) {
    for (entity, member, blood, interactable, state, marked) in &mut crew {
        let needs_evacuation = blood.0.incapacitated();
        let label = evacuation_prompt(member);
        let mut interactable = interactable;

        match (needs_evacuation, state) {
            (true, None) => {
                let previous = interactable.as_deref().map(|prompt| prompt.label.clone());
                if let Some(prompt) = interactable.as_deref_mut() {
                    prompt.label.clone_from(&label);
                } else {
                    commands.entity(entity).insert(Interactable::new(&label));
                }
                commands
                    .entity(entity)
                    .insert((NeedsMedicalEvacuation, EvacuationPromptState { previous }));
            }
            (true, Some(mut state)) => {
                // If another system changed or removed the displaced action
                // while the resident was down, preserve that newest truth
                // rather than resurrecting a stale order on recovery.
                if let Some(prompt) = interactable.as_deref_mut() {
                    if prompt.label != label {
                        state.previous = Some(prompt.label.clone());
                        prompt.label.clone_from(&label);
                    }
                } else {
                    state.previous = None;
                    commands.entity(entity).insert(Interactable::new(&label));
                }
                if !marked {
                    commands.entity(entity).insert(NeedsMedicalEvacuation);
                }
            }
            (false, Some(state)) => {
                if let Some(previous) = &state.previous {
                    if let Some(prompt) = interactable.as_deref_mut() {
                        prompt.label.clone_from(previous);
                    } else {
                        commands.entity(entity).insert(Interactable::new(previous));
                    }
                } else if interactable.is_some() {
                    commands.entity(entity).remove::<Interactable>();
                }
                commands
                    .entity(entity)
                    .remove::<NeedsMedicalEvacuation>()
                    .remove::<EvacuationPromptState>();
            }
            (false, None) if marked => {
                // Defensive repair for a partial snapshot: without the saved
                // prompt there is nothing safe to restore, but the action must
                // no longer route to evacuation once the resident is awake.
                if interactable
                    .as_deref()
                    .is_some_and(|prompt| prompt.label == label)
                {
                    commands.entity(entity).remove::<Interactable>();
                }
                commands.entity(entity).remove::<NeedsMedicalEvacuation>();
            }
            (false, None) => {}
        }
    }
}

/// Validates and completes a no-carry medical evacuation.
///
/// A client can forge the target field, so actual incapacity, sender identity,
/// player state and physical reach are all checked again on the authority.
/// Despawning the resident also removes any unresolved order/incident markers,
/// which cleanly closes the displaced interaction instead of leaving a ghost
/// queue entry behind.
fn handle_medical_evacuation(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<EvacuateCrewRequested>>,
    chemists: Query<(Entity, &Chemist)>,
    actors: Query<(&Transform, &InteractionMode, &Body, &Bloodstream), With<Chemist>>,
    residents: Query<(&CrewMember, &Transform, &Bloodstream), With<NeedsMedicalEvacuation>>,
    mut radio: ResMut<RadioLog>,
) {
    let mut evacuated = HashSet::new();
    for request in requests.read() {
        if evacuated.contains(&request.target) {
            continue;
        }
        let Some(actor) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Ok((actor_transform, mode, body, actor_blood)) = actors.get(actor) else {
            continue;
        };
        if !mode.is_roaming() || body.0.collapsed || actor_blood.0.incapacitated() {
            continue;
        }
        let Ok((member, resident_transform, resident_blood)) = residents.get(request.target) else {
            continue;
        };
        if !resident_blood.0.incapacitated()
            || !crate::interaction::authority_target_in_reach(
                actor_transform.translation,
                resident_transform.translation,
            )
        {
            continue;
        }

        evacuated.insert(request.target);
        radio.push(
            RadioEntry::new(
                crate::radio::RadioChannel::Medical,
                format!(
                    "{} was evacuated from Chemistry to Medical by lab staff.",
                    member.name
                ),
            )
            .speaker("Nurse Okonkwo")
            .positive(),
        );
        commands.entity(request.target).despawn();
    }
}

/// A deterministic hesitation at the peak of motor impairment. Entity phase
/// offsets keep a room of intoxicated residents from stepping in lockstep.
/// The route and destination never change, so this cannot become random input
/// loss or control inversion.
fn crew_stride_multiplier(entity: Entity, t: f32, blood: Option<&Bloodstream>) -> f32 {
    let Some(blood) = blood else {
        return 1.0;
    };
    let instability = blood.0.motor_instability();
    if instability <= 0.0 {
        return 1.0;
    }

    let period = (3.8 - instability.min(4.0) * 0.5).max(1.4);
    let offset = (entity.index().index() as f32 * 0.173).fract();
    let phase = ((t / period) + offset).fract();
    if (0.86..0.96).contains(&phase) {
        (1.0 - instability * 0.28).clamp(0.25, 0.9)
    } else {
        1.0
    }
}

fn walk_route(
    mut commands: Commands,
    time: Res<Time>,
    nav: Res<crate::nav::NavGraph>,
    departments: Res<Departments>,
    delivery_stations: Res<DeliveryStations>,
    mut crew: Query<(
        Entity,
        &mut Transform,
        &mut CrewRoute,
        Option<&CrewMember>,
        Option<&Bloodstream>,
    )>,
) {
    for (entity, mut transform, mut route, member, blood) in &mut crew {
        // Any chemical sedation stops a resident in place. Mild sedation is
        // still drowsiness rather than incapacity, but letting that resident
        // briskly walk home immediately after deciding to leave contradicts
        // both the status presentation and the treatment response. Their
        // already-marked Leaving route can resume once the sedative clears.
        if blood.is_some_and(|blood| {
            blood.0.status(StatusKind::Sedated).intensity > 0.0 || blood.0.incapacitated()
        }) {
            continue;
        }

        // Turn a new destination into a path, once, the frame it is set.
        if let Some(mut requested_goal) = route.pending {
            if route.counter_bound {
                requested_goal = delivery_stations
                    .station(route.delivery_lane)
                    .queue_position(route.lane_offset);
            }
            // Someone leaving heads for their own department when the station
            // has one, rather than the generic spot outside the lobby door.
            let goal = match (route.phase, member) {
                (CrewPhase::Leaving, Some(member)) => {
                    departments.home(&member.role).unwrap_or(requested_goal)
                }
                _ => requested_goal,
            };
            // If navigation has nothing to say, wait. The graph is empty for
            // a frame or two while the map loads, and walking directly to the
            // goal would cut through every intervening station wall.
            let Some(waypoints) = nav.path(transform.translation, goal) else {
                // Keep the request pending until a safe route exists. The map
                // graph is briefly empty while its scene is loading, and a
                // straight-line fallback would walk through station walls.
                continue;
            };
            route.pending = None;
            route.waypoints = waypoints;
            route.index = 0;
        }

        let Some(target) = route.waypoints.get(route.index).copied() else {
            // Route finished. Arriving crew wait; leaving crew are done.
            if route.phase == CrewPhase::Leaving {
                commands.entity(entity).despawn();
            } else if route.phase == CrewPhase::Arriving {
                route.phase = CrewPhase::Waiting;
                commands
                    .entity(entity)
                    .insert(AtCounter(route.delivery_lane));
            }
            continue;
        };

        let to_target = target - transform.translation;
        if to_target.length() <= ARRIVE_EPSILON {
            route.index += 1;
            continue;
        }

        let chemistry = blood.map_or(1.0, |blood| blood.0.movement_multiplier());
        let stumble = crew_stride_multiplier(entity, time.elapsed_secs(), blood);
        let step = to_target.normalize() * WALK_SPEED * chemistry * stumble * time.delta_secs();
        transform.translation += step;
        // Face the direction of travel so they read as people rather than
        // sliding props.
        transform.rotation = Quat::from_rotation_y(to_target.x.atan2(to_target.z));
    }
}

#[cfg(test)]
mod tests {
    //! Headless: a body goes down, a route and a standing change out.

    use super::*;
    use chem_sim::{Damage, DamageKind, Units};

    fn collapse_app() -> App {
        let mut app = App::new();
        app.init_resource::<Shift>()
            .init_resource::<RadioLog>()
            .add_systems(Update, handle_crew_collapse);
        app
    }

    /// A crew member walking a real lab, with the nav graph they route on.
    fn walking_app() -> App {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<Departments>()
            .init_resource::<DeliveryStations>()
            .insert_resource(crate::nav::NavGraph::build(
                &crate::lab::WalkableAreas::from_floor_plan(),
                crate::nav::NAV_RADIUS,
            ))
            .add_systems(
                Update,
                (
                    start_crew_at_their_department,
                    react_to_chemical_statuses,
                    sync_medical_evacuation_prompt,
                    walk_route,
                )
                    .chain(),
            );
        app
    }

    fn walker(app: &mut App, at: Vec3, route: CrewRoute) -> Entity {
        app.world_mut()
            .spawn((
                CrewMember {
                    name: "Tester".into(),
                    role: "Medical".into(),
                },
                Transform::from_translation(at),
                route,
            ))
            .id()
    }

    fn tick(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(seconds));
        app.update();
    }

    #[test]
    fn a_crew_member_walks_all_the_way_to_the_counter() {
        // The end-to-end of the whole navigation change: set a destination,
        // walk, arrive. Before pathfinding this was two hardcoded waypoints; the
        // observable behaviour must not have changed.
        let mut app = walking_app();
        let crew = walker(
            &mut app,
            Vec3::new(door_x(), 0.93, spawn_z()),
            CrewRoute::arrival(0.0),
        );

        for _ in 0..400 {
            tick(&mut app, 0.05);
            if app.world().get::<CrewRoute>(crew).unwrap().phase == CrewPhase::Waiting {
                break;
            }
        }

        let route = app.world().get::<CrewRoute>(crew).unwrap();
        assert_eq!(
            route.phase,
            CrewPhase::Waiting,
            "never finished the walk to the counter",
        );

        let at = app.world().get::<Transform>(crew).unwrap().translation;
        let counter = Vec3::new(COUNTER_SPOT.x, at.y, COUNTER_SPOT.z);
        assert!(
            at.distance(counter) < 0.5,
            "stopped {:.2}m short of the counter, at {at:?}",
            at.distance(counter),
        );
    }

    #[test]
    fn a_department_visitor_opens_the_shut_entrance_and_reaches_the_counter() {
        // Regression: the automatic entrance used to disappear from the nav
        // graph while shut. A newly scheduled visitor began at their department
        // and could not plan close enough to trip the door sensor, so the order
        // waited forever unless a chemist happened to approach the door first.
        let authored = crate::lab::tb_map::authored_walkable_areas();
        let home = crate::lab::tb_map::authored_department_home("Medical");

        // Run the actual door and nav plugins. On the buggy implementation the
        // newly spawned, closed Door collapsed `lab_entrance` during these
        // updates and the graph rebuilt with Medical disconnected from the
        // counter. Building an already-open graph directly would let that bug
        // escape the test.
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::state::app::StatesPlugin))
            .init_state::<AppState>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .init_resource::<Departments>()
            .init_resource::<DeliveryStations>()
            .init_resource::<crate::lab::DoorSpots>()
            .insert_resource(authored)
            .insert_resource(bevy::time::TimeUpdateStrategy::ManualDuration(
                std::time::Duration::from_secs_f32(0.05),
            ))
            .add_plugins((crate::nav::NavPlugin, crate::door::DoorPlugin))
            .add_systems(
                Update,
                (
                    start_crew_at_their_department,
                    react_to_chemical_statuses,
                    sync_medical_evacuation_prompt,
                    walk_route,
                )
                    .chain()
                    .run_if(in_state(AppState::Playing)),
            );
        let (run, center) = crate::lab::doorways()
            .find(|(run, center)| {
                let at = run.point(*center);
                (at.x - crate::lab::CREW_DOOR_X).abs() < 0.001
                    && (at.z - crate::lab::ROOMS[crate::lab::LOBBY].max_z).abs() < 0.001
            })
            .expect("legacy lab entrance doorway");
        let rotation = if run.along_x {
            Quat::IDENTITY
        } else {
            Quat::from_rotation_y(std::f32::consts::FRAC_PI_2)
        };
        app.world_mut()
            .resource_mut::<crate::lab::DoorSpots>()
            .insert(
                "door.chemistry.public",
                crate::lab::LAB_ENTRANCE_BRIDGE_ID,
                Transform::from_translation(run.point(center)).with_rotation(rotation),
            );
        app.finish();
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);
        // First update enters Playing and creates the Door; the following two
        // settle Changed<Door> and any resulting navigation rebuild.
        app.update();
        app.update();
        app.update();

        assert!(
            app.world()
                .resource::<crate::nav::NavGraph>()
                .path(home, COUNTER_SPOT)
                .is_some(),
            "a shut powered entrance disconnected Medical from the counter",
        );
        let door = {
            let world = app.world_mut();
            let mut doors = world.query_filtered::<Entity, With<crate::door::Door>>();
            doors.single(world).expect("the powered lab entrance")
        };
        assert!(!app.world().get::<crate::door::Door>(door).unwrap().open);
        assert!(
            app.world().get::<crate::lab::Solid>(door).is_some(),
            "route planning must not make the closed door non-solid to players",
        );
        {
            let world = app.world_mut();
            let mut chemists = world.query_filtered::<Entity, With<Chemist>>();
            assert_eq!(chemists.iter(world).count(), 0, "the visitor must open it");
        }

        app.world_mut()
            .resource_mut::<Departments>()
            .set("Medical".into(), home);
        let visitor = walker(
            &mut app,
            Vec3::new(door_x(), 0.93, spawn_z()),
            CrewRoute::arrival(0.0),
        );
        app.world_mut().entity_mut(visitor).insert((
            Body::default(),
            Bloodstream::default(),
            crate::orders::Order {
                reagent: chem_sim::ReagentId(0),
                specific: false,
                amount: Units::whole(5),
                plea: "Regression test order".to_string(),
                patience: 60.0,
                waited: 0.0,
            },
            Interactable::new("Tester — hand over 5u medicine"),
        ));

        app.update();
        let planned = &app.world().get::<CrewRoute>(visitor).unwrap().waypoints;
        let door_at = app.world().get::<Transform>(door).unwrap().translation;
        assert!(
            planned.windows(2).any(|segment| {
                segment
                    .iter()
                    .all(|at| (at.x - door_at.x).abs() < crate::lab::DOOR_WIDTH * 0.5)
                    && (segment[0].z - door_at.z) * (segment[1].z - door_at.z) <= 0.0
            }),
            "authored route did not cross both sides of the real entrance sensor: {planned:?}",
        );

        let mut opened_for_visitor = false;
        for _ in 0..1_200 {
            app.update();
            opened_for_visitor |= app.world().get::<crate::door::Door>(door).unwrap().open;
            if app.world().get::<CrewRoute>(visitor).unwrap().phase == CrewPhase::Waiting {
                break;
            }
        }

        let transform = app.world().get::<Transform>(visitor).unwrap();
        assert!(
            opened_for_visitor,
            "the closed entrance never opened for its approaching visitor",
        );
        assert_eq!(
            app.world().get::<CrewRoute>(visitor).unwrap().phase,
            CrewPhase::Waiting,
            "visitor remained stranded despite the powered automatic entrance",
        );
        assert!(
            transform.translation.distance(Vec3::new(
                COUNTER_SPOT.x,
                transform.translation.y,
                COUNTER_SPOT.z,
            )) < 0.2,
            "visitor stopped at {:?} instead of the delivery counter",
            transform.translation,
        );
    }

    #[test]
    fn crew_walk_speed_uses_the_shared_bloodstream_modifier() {
        let mut app = walking_app();
        let start = Vec3::new(door_x(), 0.93, spawn_z());
        let clear = walker(&mut app, start, CrewRoute::arrival(-0.2));
        let slow = walker(&mut app, start, CrewRoute::arrival(0.2));
        app.world_mut()
            .entity_mut(clear)
            .insert(Bloodstream::default());
        let mut sluggish = Bloodstream::default();
        sluggish.0.add_status(StatusKind::Sluggish, 10.0, 1.0);
        app.world_mut().entity_mut(slow).insert(sluggish);

        tick(&mut app, 0.1);

        let clear_distance = app
            .world()
            .get::<Transform>(clear)
            .unwrap()
            .translation
            .distance(start);
        let slow_distance = app
            .world()
            .get::<Transform>(slow)
            .unwrap()
            .translation
            .distance(start);
        assert!(
            clear_distance > slow_distance,
            "the sluggish resident walked {slow_distance}m while the clear resident walked {clear_distance}m",
        );
    }

    #[test]
    fn sedation_marks_crew_for_removal_and_incapacitation_stops_them() {
        let mut app = walking_app();
        let room = crate::lab::ROOMS[crate::lab::HALL].center();
        let start = Vec3::new(room.x, 0.93, room.z);
        let crew = walker(&mut app, start, CrewRoute::arrival(0.0));
        let mut blood = Bloodstream::default();
        blood.0.add_status(StatusKind::Sedated, 10.0, 2.0);
        app.world_mut().entity_mut(crew).insert(blood);

        tick(&mut app, 0.1);

        assert!(app.world().get_entity(crew).is_ok());
        assert_eq!(
            app.world().get::<CrewRoute>(crew).unwrap().phase,
            CrewPhase::Leaving,
        );
        assert_eq!(
            app.world().get::<Transform>(crew).unwrap().translation,
            start,
            "an incapacitated person must wait for removal or recovery, not walk out",
        );
        assert!(app.world().get::<NeedsMedicalEvacuation>(crew).is_some());
        assert_eq!(
            app.world().get::<Interactable>(crew).unwrap().label,
            "Evacuate Tester to Medical",
        );
    }

    #[test]
    fn even_mild_sedation_stops_a_resident_in_place() {
        let mut app = walking_app();
        let room = crate::lab::ROOMS[crate::lab::HALL].center();
        let start = Vec3::new(room.x, 0.93, room.z);
        let crew = walker(&mut app, start, CrewRoute::arrival(0.0));
        let mut blood = Bloodstream::default();
        blood.0.add_status(StatusKind::Sedated, 10.0, 0.5);
        app.world_mut().entity_mut(crew).insert(blood);

        tick(&mut app, 0.1);

        assert_eq!(
            app.world().get::<CrewRoute>(crew).unwrap().phase,
            CrewPhase::Leaving,
        );
        assert_eq!(
            app.world().get::<Transform>(crew).unwrap().translation,
            start,
            "a sedated resident should stop rather than walk themselves out",
        );
    }

    fn evacuation_app() -> App {
        let mut app = App::new();
        app.init_resource::<RadioLog>()
            .add_message::<FromClient<EvacuateCrewRequested>>()
            .add_systems(
                Update,
                (sync_medical_evacuation_prompt, handle_medical_evacuation).chain(),
            );
        app
    }

    fn incapacitated_resident(app: &mut App, at: Vec3) -> Entity {
        let mut blood = Bloodstream::default();
        blood.0.add_status(StatusKind::Sedated, 10.0, 2.0);
        app.world_mut()
            .spawn((
                CrewMember {
                    name: "Down Patient".into(),
                    role: "Engineering".into(),
                },
                Transform::from_translation(at),
                blood,
                Interactable::new("Down Patient — hand over 5u medicine"),
            ))
            .id()
    }

    #[test]
    fn evacuation_requires_a_real_conscious_nearby_sender() {
        let mut app = evacuation_app();
        let client_entity = app.world_mut().spawn_empty().id();
        let client = ClientId::Client(client_entity);
        let chemist = app
            .world_mut()
            .spawn((
                Chemist { client },
                Transform::from_xyz(10.0, 0.93, 0.0),
                InteractionMode::Roaming,
                Body::default(),
                Bloodstream::default(),
            ))
            .id();
        let patient = incapacitated_resident(&mut app, Vec3::new(0.0, 0.93, 0.0));
        app.update();

        let forged = ClientId::Client(app.world_mut().spawn_empty().id());
        app.world_mut().write_message(FromClient {
            client_id: forged,
            message: EvacuateCrewRequested { target: patient },
        });
        app.update();
        assert!(
            app.world().get_entity(patient).is_ok(),
            "a request without an authority-owned chemist must do nothing",
        );

        app.world_mut().write_message(FromClient {
            client_id: client,
            message: EvacuateCrewRequested { target: patient },
        });
        app.update();
        assert!(
            app.world().get_entity(patient).is_ok(),
            "a real chemist cannot evacuate someone from across the station",
        );

        app.world_mut()
            .get_mut::<Transform>(chemist)
            .unwrap()
            .translation = Vec3::new(1.0, 0.93, 0.0);
        app.world_mut().write_message(FromClient {
            client_id: client,
            message: EvacuateCrewRequested { target: patient },
        });
        app.update();

        assert!(
            app.world().get_entity(patient).is_err(),
            "a nearby valid chemist should complete the evacuation",
        );
        let report = app.world().resource::<RadioLog>().entries.back().unwrap();
        assert_eq!(report.channel, crate::radio::RadioChannel::Medical);
        assert!(report.text.contains("Down Patient was evacuated"));
    }

    #[test]
    fn recovery_restores_the_prompt_displaced_by_evacuation() {
        let mut app = App::new();
        app.add_systems(Update, sync_medical_evacuation_prompt);
        let original = "Down Patient — hand over 5u medicine";
        let patient = incapacitated_resident(&mut app, Vec3::ZERO);

        app.update();
        assert_eq!(
            app.world().get::<Interactable>(patient).unwrap().label,
            "Evacuate Down Patient to Medical",
        );

        app.world_mut()
            .get_mut::<Bloodstream>(patient)
            .unwrap()
            .0
            .counter_status(StatusKind::Sedated, 100.0, 100.0);
        app.update();

        assert!(app.world().get::<NeedsMedicalEvacuation>(patient).is_none());
        assert_eq!(
            app.world().get::<Interactable>(patient).unwrap().label,
            original,
        );
    }

    #[test]
    fn paranoia_makes_crew_flee() {
        let mut app = walking_app();
        let room = crate::lab::ROOMS[crate::lab::HALL].center();
        let crew = walker(
            &mut app,
            Vec3::new(room.x, 0.93, room.z),
            CrewRoute::arrival(0.0),
        );
        let mut blood = Bloodstream::default();
        blood.0.add_status(StatusKind::Paranoid, 10.0, 1.0);
        app.world_mut().entity_mut(crew).insert(blood);

        tick(&mut app, 0.05);

        assert_eq!(
            app.world().get::<CrewRoute>(crew).unwrap().phase,
            CrewPhase::Leaving,
        );
    }

    #[test]
    fn euphoria_makes_an_idle_resident_linger() {
        let mut app = App::new();
        app.add_systems(Update, react_to_chemical_statuses);
        let mut blood = Bloodstream::default();
        blood.0.add_status(StatusKind::Euphoric, 10.0, 1.0);
        let resident = app
            .world_mut()
            .spawn((
                blood,
                CrewRoute {
                    waypoints: Vec::new(),
                    index: 0,
                    phase: CrewPhase::Waiting,
                    pending: None,
                    counter_bound: false,
                    delivery_lane: DeliveryLane::Public,
                    lane_offset: 0.0,
                },
                Ambient { dwell: 0.1 },
            ))
            .id();

        app.update();

        assert_eq!(app.world().get::<Ambient>(resident).unwrap().dwell, 2.5);
    }

    #[test]
    fn motor_stumbles_use_a_repeatable_cadence() {
        let mut app = App::new();
        let entity = app.world_mut().spawn_empty().id();
        let mut blood = Bloodstream::default();
        blood.0.add_status(StatusKind::Unsteady, 10.0, 2.0);

        let sample = (0..200)
            .map(|frame| frame as f32 * 0.05)
            .find(|t| crew_stride_multiplier(entity, *t, Some(&blood)) < 1.0)
            .expect("the deterministic cadence should contain a stumble");
        let first = crew_stride_multiplier(entity, sample, Some(&blood));
        assert_eq!(first, crew_stride_multiplier(entity, sample, Some(&blood)));
        assert!(
            first >= 0.25,
            "a stumble must slow, not freeze, the crew member"
        );
    }

    #[test]
    fn a_walk_across_the_suite_is_routed_rather_than_straight_through_walls() {
        // What pathfinding buys that the hardcoded route could not: someone
        // standing in the reaction bay is two rooms and two doorways from the
        // counter. A straight line there crosses three walls, so the route must
        // contain intermediate waypoints — and none of them may be the goal.
        let mut app = walking_app();
        let from = crate::lab::ROOMS[crate::lab::REACTION_BAY].center();
        let crew = walker(
            &mut app,
            Vec3::new(from.x, 0.93, from.z),
            CrewRoute::arrival(0.0),
        );

        tick(&mut app, 0.01);

        let route = app.world().get::<CrewRoute>(crew).unwrap();
        assert!(
            route.waypoints.len() >= 3,
            "expected a route through the hall and lobby, got {:?}",
            route.waypoints,
        );
        assert!(
            route.pending.is_none(),
            "the destination should have been resolved on the first update",
        );
    }

    #[test]
    fn crew_come_from_and_return_to_their_own_department() {
        // What the station's wings are for. A Medical crew member starts in
        // Medical rather than the void south of the lobby, and heads back there
        // when they are done — the whole visible difference between a lab with
        // a corridor attached and a station with people in it.
        let mut app = walking_app();
        let medical = Vec3::new(-21.0, 0.0, 18.0);
        app.world_mut()
            .resource_mut::<Departments>()
            .set("Medical".into(), medical);

        let crew = walker(
            &mut app,
            Vec3::new(door_x(), 0.93, spawn_z()),
            CrewRoute::arrival(0.0),
        );
        tick(&mut app, 0.01);

        // Loose on purpose: they are placed and then immediately take their
        // first step towards the counter in the same tick. Medical is thirty
        // metres from the old spawn spot, so a quarter of a metre still tells
        // the two apart unambiguously.
        let at = app.world().get::<Transform>(crew).unwrap().translation;
        assert!(
            at.distance(Vec3::new(medical.x, at.y, medical.z)) < 0.25,
            "started at {at:?}, not in Medical",
        );

        app.world_mut().get_mut::<CrewRoute>(crew).unwrap().leave();
        tick(&mut app, 0.01);

        let heading_for = *app
            .world()
            .get::<CrewRoute>(crew)
            .unwrap()
            .waypoints
            .last()
            .expect("a route home");
        assert!(
            (heading_for.x - medical.x).abs() < 0.01 && (heading_for.z - medical.z).abs() < 0.01,
            "left towards {heading_for:?} instead of back to Medical",
        );
    }

    #[test]
    fn a_department_the_station_has_no_room_for_changes_nothing() {
        // Every build without the map has an empty `Departments`, and so does a
        // map missing a wing. Crew must keep arriving and leaving the way they
        // did before any of this existed.
        let mut app = walking_app();
        let start = Vec3::new(door_x(), 0.93, spawn_z());
        let crew = walker(&mut app, start, CrewRoute::arrival(0.0));

        tick(&mut app, 0.01);
        let at = app.world().get::<Transform>(crew).unwrap().translation;
        assert!(
            (at.x - start.x).abs() < 0.5,
            "moved to a department that does not exist",
        );

        app.world_mut().get_mut::<CrewRoute>(crew).unwrap().leave();
        tick(&mut app, 0.01);

        let heading_for = *app
            .world()
            .get::<CrewRoute>(crew)
            .unwrap()
            .waypoints
            .last()
            .expect("a route out");
        assert!(
            (heading_for.z - spawn_z()).abs() < 0.01,
            "left towards {heading_for:?} instead of off-station",
        );
    }

    /// A station with two departments and residents idling in them.
    fn station_app() -> App {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<Departments>()
            .init_resource::<DeliveryStations>()
            .insert_resource(crate::nav::NavGraph::build(
                &crate::lab::WalkableAreas::from_floor_plan(),
                crate::nav::NAV_RADIUS,
            ))
            .add_systems(Update, (ambient_behaviour, walk_route).chain());

        let mut departments = app.world_mut().resource_mut::<Departments>();
        departments.set("Medical".into(), Vec3::new(-21.0, 0.0, 18.0));
        departments.set("Cargo".into(), Vec3::new(-5.0, 0.0, 18.0));
        app
    }

    /// Someone standing around in the corridor, with nothing to do.
    fn resident(app: &mut App, role: &str, at: Vec3) -> Entity {
        app.world_mut()
            .spawn((
                CrewMember {
                    name: format!("{role} Resident"),
                    role: role.into(),
                },
                Transform::from_translation(at),
                CrewRoute {
                    waypoints: Vec::new(),
                    index: 0,
                    phase: CrewPhase::Waiting,
                    pending: None,
                    counter_bound: false,
                    delivery_lane: DeliveryLane::Public,
                    lane_offset: 0.0,
                },
                // Long dwell, so any movement in these tests is the crisis
                // talking and never the idle wander.
                Ambient { dwell: 1_000.0 },
            ))
            .id()
    }

    fn casualty(app: &mut App, at: Vec3, responders: &[&str]) {
        app.world_mut().spawn((
            Transform::from_translation(at),
            crate::crisis::CrisisResponse {
                responders: responders.iter().map(|role| role.to_string()).collect(),
            },
        ));
    }

    fn destination(app: &App, crew: Entity) -> Vec3 {
        *app.world()
            .get::<CrewRoute>(crew)
            .unwrap()
            .waypoints
            .last()
            .expect("somewhere to be")
    }

    #[test]
    fn residents_never_count_towards_how_busy_the_counter_is() {
        // The regression this exists to stop happening twice: `generate_orders`
        // counts crew to decide whether there is room for another customer, and
        // `max_active_cap` is 5. Eight residents living on the station is
        // already over it, so every order-spawning gate in the game closed
        // permanently — no error, nothing in the log, orders simply stopped.
        let mut app = App::new();

        for role in ["Medical", "Cargo", "Security"] {
            app.world_mut().spawn((
                CrewMember {
                    name: format!("{role} Resident"),
                    role: role.into(),
                },
                Ambient { dwell: 1.0 },
            ));
        }
        let visitor = app.world_mut().spawn(CrewMember {
            name: "Customer".into(),
            role: "Service".into(),
        });
        let visitor = visitor.id();

        let mut all = app.world_mut().query::<&CrewMember>();
        assert_eq!(all.iter(app.world()).count(), 4, "four crew exist");

        let mut visiting = app
            .world_mut()
            .query_filtered::<Entity, (With<CrewMember>, NotResident)>();
        let counted: Vec<Entity> = visiting.iter(app.world()).collect();
        assert_eq!(
            counted,
            vec![visitor],
            "only the customer may count towards the counter being busy",
        );
    }

    #[test]
    fn a_department_that_can_help_walks_towards_the_casualty() {
        // The user's rule, first half: if your department would be any use, you
        // go to it.
        let mut app = station_app();
        let medic = resident(&mut app, "Medical", Vec3::new(-21.0, 0.93, 18.0));
        let hurt = Vec3::new(4.0, 0.0, 5.6);
        casualty(&mut app, hurt, &["Medical"]);

        tick(&mut app, 0.01);

        let heading_for = destination(&app, medic);
        assert!(
            heading_for.distance(hurt.with_y(0.93)) < 0.01,
            "a medic headed for {heading_for:?} instead of the casualty at {hurt:?}",
        );
    }

    #[test]
    fn a_department_that_cannot_help_goes_home_instead() {
        // The second half: if you would be no use, you get out of the way. Note
        // *not* the escape pod — that is the end of a campaign, not a bad shift.
        let mut app = station_app();
        let hauler = resident(&mut app, "Cargo", Vec3::new(0.0, 0.93, 0.0));
        casualty(&mut app, Vec3::new(4.0, 0.0, 5.6), &["Medical"]);

        tick(&mut app, 0.01);

        let heading_for = destination(&app, hauler);
        let home = Vec3::new(-5.0, 0.93, 18.0);
        assert!(
            heading_for.distance(home) < 0.01,
            "a hauler headed for {heading_for:?} instead of home to Cargo",
        );
    }

    #[test]
    fn a_responder_already_at_the_casualty_does_not_shuffle_on_the_spot() {
        // Without the "close enough" check, a responder standing over the
        // casualty re-routes to their own position every frame, which reads as
        // twitching and never stops.
        let mut app = station_app();
        let hurt = Vec3::new(4.0, 0.0, 5.6);
        let medic = resident(&mut app, "Medical", Vec3::new(hurt.x, 0.93, hurt.z));
        casualty(&mut app, hurt, &["Medical"]);

        tick(&mut app, 0.01);

        assert!(
            app.world()
                .get::<CrewRoute>(medic)
                .unwrap()
                .waypoints
                .is_empty(),
            "re-routed despite already standing on the casualty",
        );
    }

    #[test]
    fn with_no_casualty_nobody_is_summoned_anywhere() {
        // Idle crew wander on their own clock; a long dwell means they stay put.
        let mut app = station_app();
        let medic = resident(&mut app, "Medical", Vec3::new(0.0, 0.93, 0.0));

        tick(&mut app, 0.01);

        assert!(
            app.world()
                .get::<CrewRoute>(medic)
                .unwrap()
                .waypoints
                .is_empty(),
            "went somewhere with no crisis to go to",
        );
    }

    #[test]
    fn a_crew_member_waits_when_the_station_has_no_nav_graph() {
        // Under the map backend the graph is empty for a frame or two while the
        // scene loads. Crew must wait rather than head through a wall.
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<Departments>()
            .init_resource::<DeliveryStations>()
            .init_resource::<crate::nav::NavGraph>()
            .add_systems(Update, walk_route);

        let start = Vec3::new(door_x(), 0.93, spawn_z());
        let crew = walker(&mut app, start, CrewRoute::arrival(0.0));

        for _ in 0..20 {
            tick(&mut app, 0.05);
        }

        let at = app.world().get::<Transform>(crew).unwrap().translation;
        assert!(at.distance(start) < 0.001, "walked without a safe route",);
        assert!(
            app.world()
                .get::<CrewRoute>(crew)
                .unwrap()
                .pending
                .is_some(),
            "discarded the destination while waiting for navigation",
        );
    }

    fn flatten(app: &mut App, crew: Entity) {
        app.world_mut()
            .get_mut::<Body>(crew)
            .unwrap()
            .0
            .apply(Damage::of(DamageKind::Brute, Units::whole(120)));
        app.update();
    }

    #[test]
    fn a_collapsed_crew_member_leaves_early_and_costs_medical_standing() {
        let mut app = collapse_app();
        let crew = app
            .world_mut()
            .spawn((
                Body::default(),
                CrewMember {
                    name: "Test Subject".to_string(),
                    role: "Medical".to_string(),
                },
                CrewRoute::arrival(0.0),
            ))
            .id();

        flatten(&mut app, crew);

        assert_eq!(
            app.world().get::<CrewRoute>(crew).unwrap().phase,
            CrewPhase::Leaving,
            "a crew member who goes down should be sent for the door, not left standing"
        );
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .standing(Department::Medical),
            COLLAPSE_PENALTY
        );

        // Still down several frames later — must not keep charging, exactly
        // like the player's own `going_down_costs_standing_once`.
        app.update();
        app.update();
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .standing(Department::Medical),
            COLLAPSE_PENALTY,
            "the penalty is for going down, not for staying down"
        );
    }

    #[test]
    fn an_unhurt_crew_member_is_left_alone() {
        let mut app = collapse_app();
        let crew = app
            .world_mut()
            .spawn((
                Body::default(),
                CrewMember {
                    name: "Fine".to_string(),
                    role: "Medical".to_string(),
                },
                CrewRoute::arrival(0.0),
            ))
            .id();

        app.update();

        assert_eq!(
            app.world().get::<CrewRoute>(crew).unwrap().phase,
            CrewPhase::Arriving,
            "nothing collapsed, so nothing should have sent them for the door"
        );
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .standing(Department::Medical),
            0
        );
    }
}
