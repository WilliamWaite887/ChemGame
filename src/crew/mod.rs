//! Crew who come to the lab to collect what they ordered.
//!
//! Movement follows the station's portal graph. Callers provide destinations
//! such as the counter or a department, and navigation supplies safe doorway
//! and corridor waypoints.

use std::collections::HashSet;
use std::time::Duration;

use bevy::ecs::entity::MapEntities;
use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::StatusKind;
use serde::{Deserialize, Serialize};

use crate::body::{Bloodstream, Body, COLLAPSE_PENALTY};
use crate::character_lab::{
    character_animation_speed, desired_character_animation, CharacterAnimation,
};
use crate::interaction::{Interactable, InteractionMode};
use crate::lab::{DeliveryLane, DeliveryStations, MapReady, COUNTER_SPOT, DOOR_MAX_X, DOOR_MIN_X};
use crate::machines::chemist_entity;
use crate::nav::ProgressWatch;
use crate::net::is_authority;
use crate::orders::{Department, Shift};
use crate::player::Chemist;
use crate::radio::{RadioEntry, RadioLog};
use crate::AppState;

/// Walking pace, metres per second.
const WALK_SPEED: f32 = 2.1;
/// Close enough to count as arrived.
const ARRIVE_EPSILON: f32 = 0.12;
/// Close enough to a post to count as *standing at* it rather than merely
/// having walked nearby — used only for presentation (which animation to
/// play), so it stays generous relative to [`ARRIVE_EPSILON`]'s tight
/// walk-arrival tolerance. Shared by [`CrewPosts::is_near_relax`] and
/// `drive_crew_animation`'s own work-post check, and by
/// `ambient_behaviour`'s "already standing there" crisis check.
const POST_PROXIMITY: f32 = 1.5;
/// Distance from the floor a crew member is standing on to their entity
/// origin — the `body_offset` [`crate::lab::WalkableAreas::contain_on_surface`]
/// wants, and the height [`spawn_crew_member`] starts them at.
pub(crate) const BODY_OFFSET: f32 = 0.93;
/// How often a body whose steps are being eaten by the floor may plan again.
///
/// A scrape along a wall is common and usually harmless, so this is a rate
/// limit on the fix rather than the fix itself: often enough that a body
/// rounding a corner badly is put right before anyone notices, rarely enough
/// that pressing against a wall is not a route search every frame.
const BLOCKED_REPLAN_SECONDS: f32 = 0.5;

pub struct CrewPlugin;

impl Plugin for CrewPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Departments>()
            .init_resource::<CrewPosts>()
            .add_message::<ErrandResolved>()
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
                        react_to_chemical_statuses,
                        sync_medical_evacuation_prompt,
                        handle_medical_evacuation,
                        ambient_behaviour,
                        walk_route,
                        // After `walk_route`, so an errand set in reaction to
                        // an arrival this frame starts walking on the next one
                        // rather than half a frame late.
                        run_errands,
                        handle_crew_collapse,
                    )
                        .chain()
                        .run_if(is_authority)
                        .run_if(resource_exists::<MapReady>),
                    // Deliberately outside the `MapReady`-gated chain above:
                    // `collect_loaded_map` removes `MapReady` in the very same
                    // command batch that makes `Departments` newly changed,
                    // and nav only restores `MapReady` a frame later once it
                    // has rebuilt from the fresh `WalkableAreas`. A system
                    // gated on both at once — as this one used to be — can
                    // have that one real transition land on the exact frame
                    // `MapReady` is momentarily absent: Bevy still evaluates
                    // every run condition on the system regardless of
                    // whether an earlier one already failed, so the
                    // change-detection condition here "sees" and consumes
                    // the change on that frame even though the system's body
                    // never runs, and by the time `MapReady` returns
                    // `Departments` no longer looks newly changed to it.
                    // Silently, permanently, no ambient crew ever spawn.
                    // This system does not actually need nav to have
                    // finished — it only queues a destination on a fresh
                    // `CrewRoute`; `walk_route` (still gated on `MapReady`
                    // above) resolves the real path once nav is ready.
                    populate_departments
                        .run_if(resource_exists_and_changed::<Departments>)
                        .run_if(is_authority),
                    // Runs everywhere: a crew member who arrived by
                    // replication needs a body drawing just as much as one
                    // spawned locally.
                    (
                        dress_crew,
                        configure_crew_faces.after(dress_crew),
                        tag_crew_surfaces.after(dress_crew),
                        attach_crew_animation.after(dress_crew),
                        drive_crew_animation.after(attach_crew_animation),
                    )
                        .after(assign_crew_appearances),
                    assign_crew_appearances.run_if(is_authority),
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

/// Personal and communal spots an ambient resident can be sent to, layered
/// on top of [`Departments`]' one-point-per-department model rather than
/// replacing it: a named individual with no authored post here simply falls
/// back to their department's shared point exactly as before, which is what
/// lets posts be authored incrementally, one person at a time, without ever
/// leaving the station in a broken state.
///
/// Empty in a build without the map, same reasoning as [`Departments`].
/// Populated from the `crew_post` map marker — see `lab::tb::CrewPost`.
#[derive(Resource, Default)]
pub struct CrewPosts {
    /// Keyed by `station.crew.ron` *name*, not role — this is what actually
    /// fixes the stacking bug: giving each of the individuals in a
    /// multi-person department (Medical, Security, Service) their own
    /// destination instead of all pathing to the department's single shared
    /// point.
    work: std::collections::HashMap<String, Vec3>,
    /// Communal, not owned by any one person: any idle resident may be sent
    /// to one when they decide to relax, not just an author's intended
    /// occupant.
    relax: Vec<Vec3>,
    /// Kept as its own pool, distinct from `relax`, so the signal stays
    /// clean: a department minor's off-roster identity (see
    /// `smuggler::loiter_smuggler`) only ever appears here, never at an
    /// ordinary resident's relax spot — mixing the two would blur the exact
    /// behavioural tell this exists to give an attentive player.
    loiter: Vec<Vec3>,
}

impl CrewPosts {
    #[cfg_attr(not(feature = "trenchbroom"), allow(dead_code))]
    pub fn set_work(&mut self, occupant: String, at: Vec3) {
        self.work.insert(occupant, at);
    }

    #[cfg_attr(not(feature = "trenchbroom"), allow(dead_code))]
    pub fn add_relax(&mut self, at: Vec3) {
        self.relax.push(at);
    }

    #[cfg_attr(not(feature = "trenchbroom"), allow(dead_code))]
    pub fn add_loiter(&mut self, at: Vec3) {
        self.loiter.push(at);
    }

    /// Where this named individual's own post is, if the station has
    /// authored one for them yet.
    pub fn work(&self, occupant: &str) -> Option<Vec3> {
        self.work.get(occupant).copied()
    }

    /// A random communal relax spot, if the station has authored any.
    pub fn random_relax(&self) -> Option<Vec3> {
        Self::random_of(&self.relax)
    }

    /// Whether `at` is close enough to some relax spot to count as sitting
    /// there. Presentation-only — `Sitting` is chosen for a resident who has
    /// already arrived and is otherwise idle, this only decides whether
    /// their current position happens to be one of the seats rather than
    /// somewhere ordinary to stand.
    pub fn is_near_relax(&self, at: Vec3) -> bool {
        self.relax.iter().any(|spot| {
            let flat = Vec3::new(spot.x, at.y, spot.z);
            at.distance(flat) < POST_PROXIMITY
        })
    }

    /// A random illicit-flavoured loitering spot, if the station has
    /// authored any.
    pub fn random_loiter(&self) -> Option<Vec3> {
        Self::random_of(&self.loiter)
    }

    fn random_of(spots: &[Vec3]) -> Option<Vec3> {
        if spots.is_empty() {
            return None;
        }
        spots.get(rand::random_range(0..spots.len())).copied()
    }
}

/// A crew member as written in `assets/data/station.crew.ron`.
#[derive(Clone, Debug, Deserialize)]
pub struct CrewDef {
    pub name: String,
    pub role: String,
    /// Retained for compatibility with the authored roster; the current
    /// department outfit system supplies presentation color instead.
    #[allow(dead_code)]
    pub color: [f32; 3],
}

#[derive(Component, Clone, Serialize, Deserialize)]
pub struct CrewMember {
    pub name: String,
    pub role: String,
}

/// Replicated once-per-spawn appearance identity. Keeping the random choice
/// beside the NPC means every peer sees the same face and later presentation
/// rebuilds cannot silently reroll it.
#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct CrewAppearance {
    pub face_variant: u8,
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
    /// Watches for the walk going nowhere. See [`walk_route`]'s recovery leg.
    stall: ProgressWatch,
    /// Cooldown on replanning a route containment has shoved the body off.
    replan_in: f32,
    /// Recovery legs taken toward the *current* goal, so a body that wedges
    /// twice is sent somewhere new the second time. Reset when a fresh
    /// destination is resolved into waypoints.
    unstick: usize,
}

impl CrewRoute {
    /// Whether they are actually walking somewhere right now.
    ///
    /// `pub` for `speech`, which pairs off idle residents for an overheard
    /// exchange. The obvious alternative — reading `phase == CrewPhase::
    /// Waiting` — would have been a seventh reader of a flag `docs/npc-ai.md`
    /// already lists as overloaded ("six systems across five modules read it
    /// as 'arrived and stopped'"), and this is the precise question those six
    /// are all approximating.
    pub fn is_moving(&self) -> bool {
        self.pending.is_some() || self.waypoints.get(self.index).is_some()
    }

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
            stall: ProgressWatch::default(),
            replan_in: 0.0,
            unstick: 0,
        }
    }

    /// A plain walk to an arbitrary destination, not bound for the counter —
    /// for ambient/loitering spawns that aren't fulfilling an order. Fields
    /// stay private to this module, so callers elsewhere (`smuggler::
    /// loiter_smuggler`, for one) go through this rather than a struct
    /// literal.
    pub fn to(destination: Vec3) -> Self {
        CrewRoute {
            waypoints: Vec::new(),
            index: 0,
            phase: CrewPhase::Arriving,
            pending: Some(destination),
            counter_bound: false,
            delivery_lane: DeliveryLane::Public,
            lane_offset: 0.0,
            stall: ProgressWatch::default(),
            replan_in: 0.0,
            unstick: 0,
        }
    }

    /// A fresh route straight back out of the station.
    ///
    /// For a body that has no `CrewRoute` at all to send away — one that has
    /// just finished an [`Errand`], which took its route off it. Built through
    /// [`CrewRoute::leave`] rather than beside it so there is still exactly one
    /// description of what leaving means.
    pub fn leaving() -> Self {
        let mut route = Self::to(Vec3::ZERO);
        route.leave();
        route
    }

    /// A route that has already finished: standing exactly where they are.
    ///
    /// Test-only, because production never builds one — every real route is
    /// created with somewhere to go and *becomes* this by having `walk_route`
    /// consume its waypoints. `speech`'s exchange tests need a resident who is
    /// demonstrably not walking, which is otherwise only reachable by standing
    /// up the whole nav stack to walk one there.
    #[cfg(test)]
    pub(crate) fn standing() -> Self {
        CrewRoute {
            waypoints: Vec::new(),
            index: 0,
            phase: CrewPhase::Waiting,
            pending: None,
            counter_bound: false,
            delivery_lane: DeliveryLane::Public,
            lane_offset: 0.0,
            stall: ProgressWatch::default(),
            replan_in: 0.0,
            unstick: 0,
        }
    }

    /// Sends them back out to the station.
    pub fn leave(&mut self) {
        self.waypoints.clear();
        self.index = 0;
        self.phase = CrewPhase::Leaving;
        self.pending = Some(Vec3::new(door_x(), 0.0, spawn_z()));
        self.counter_bound = false;
        self.stall.restart();
        self.unstick = 0;
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

/// The five department variants share geometry, skeleton and animations; only
/// their authored palette and small role accessories differ.
#[derive(Resource)]
pub struct CrewAssets {
    models: [CrewModelAsset; 5],
}

impl CrewAssets {
    fn theme_for(role: &str) -> usize {
        match role {
            "Medical" => 0,
            "Security" => 1,
            "Engineering" => 2,
            "Cargo" => 3,
            _ => 4,
        }
    }

    fn model_for(&self, role: &str) -> (usize, &CrewModelAsset) {
        let theme = Self::theme_for(role);
        (theme, &self.models[theme])
    }
}

struct CrewModelAsset {
    scene: Handle<WorldAsset>,
    animation_graph: Handle<AnimationGraph>,
    animation_nodes: [AnimationNodeIndex; 9],
}

fn load_crew_model(
    path: &'static str,
    asset_server: &AssetServer,
    animation_graphs: &mut Assets<AnimationGraph>,
) -> CrewModelAsset {
    // Blender exports Actions alphabetically: Collapsed, Idle, Sedated,
    // Sitting, Stimulated, Unsteady, Walk, WalkDrunk, Working. Mirrors
    // `character_lab::load_character_lab_assets`'s own mapping — the two
    // load the same rig's clips, just per-department GLBs instead of one
    // shared test-subject GLB.
    let (graph, nodes) = AnimationGraph::from_clips([
        asset_server.load(GltfAssetLabel::Animation(1).from_asset(path)), // Idle
        asset_server.load(GltfAssetLabel::Animation(4).from_asset(path)), // Stimulated
        asset_server.load(GltfAssetLabel::Animation(2).from_asset(path)), // Sedated
        asset_server.load(GltfAssetLabel::Animation(5).from_asset(path)), // Unsteady
        asset_server.load(GltfAssetLabel::Animation(0).from_asset(path)), // Collapsed
        asset_server.load(GltfAssetLabel::Animation(6).from_asset(path)), // Walk
        asset_server.load(GltfAssetLabel::Animation(7).from_asset(path)), // WalkDrunk
        asset_server.load(GltfAssetLabel::Animation(8).from_asset(path)), // Working
        asset_server.load(GltfAssetLabel::Animation(3).from_asset(path)), // Sitting
    ]);
    CrewModelAsset {
        scene: asset_server.load(GltfAssetLabel::Scene(0).from_asset(path)),
        animation_graph: animation_graphs.add(graph),
        animation_nodes: nodes
            .try_into()
            .expect("the shared crew rig has exactly nine clips"),
    }
}

fn load_crew_assets(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut animation_graphs: ResMut<Assets<AnimationGraph>>,
) {
    commands.insert_resource(CrewAssets {
        models: [
            load_crew_model(
                "3dassets/glb/first_char_medical.glb",
                &asset_server,
                &mut animation_graphs,
            ),
            load_crew_model(
                "3dassets/glb/first_char_security.glb",
                &asset_server,
                &mut animation_graphs,
            ),
            load_crew_model(
                "3dassets/glb/first_char_engineering.glb",
                &asset_server,
                &mut animation_graphs,
            ),
            load_crew_model(
                "3dassets/glb/first_char_cargo.glb",
                &asset_server,
                &mut animation_graphs,
            ),
            load_crew_model(
                "3dassets/glb/first_char_service.glb",
                &asset_server,
                &mut animation_graphs,
            ),
        ],
    });
}

/// The imported visual root, and the crew member it belongs to.
///
/// The M12 twin of `player::ChemistBody`, and needed for the same reason:
/// `fx::animate_crew_body` has to wobble/tint a *child* mesh, never the root
/// `Transform`, because the root is replicated and authoritative —
/// `CrewRoute`'s movement and every interaction raycast depend on it staying
/// exactly where the server put it. `rest`/`rest_rotation` are its un-animated
/// reference pose, recomputed every frame rather than accumulated, so nothing
/// drifts.
#[derive(Component)]
pub(crate) struct CrewBody {
    pub(crate) crew: Entity,
    pub(crate) rest: Vec3,
    pub(crate) rest_rotation: Quat,
    pub(crate) face_variant: u8,
    theme: usize,
}

/// A privately cloned material below an imported crew scene.
#[derive(Component)]
pub(crate) struct CrewSurface {
    pub(crate) crew: Entity,
    pub(crate) base_color: Color,
    pub(crate) base_alpha_mode: AlphaMode,
}

#[derive(Component)]
struct CrewFaceConfigured;

#[derive(Component)]
struct CrewAnimationController {
    crew: Entity,
    theme: usize,
    current: CharacterAnimation,
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
    let position = Vec3::new(door_x(), BODY_OFFSET, spawn_z());
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

/// Gives a crew member the shared rig in their department theme.
///
/// The imported scene is a child (`CrewBody`), not inserted straight onto the
/// replicated root. A wobble applied to the root would
/// corrupt the replicated, authoritative `Transform` `CrewRoute` and every
/// interaction raycast depend on; a child can be animated freely.
fn assign_crew_appearances(
    mut commands: Commands,
    crew: Query<Entity, (Added<CrewMember>, Without<CrewAppearance>)>,
) {
    for entity in &crew {
        commands.entity(entity).insert(CrewAppearance {
            face_variant: rand::random_range(0..3),
        });
    }
}

fn dress_crew(
    mut commands: Commands,
    assets: Option<Res<CrewAssets>>,
    crew: Query<(Entity, &CrewMember, &CrewAppearance), Added<CrewAppearance>>,
) {
    let Some(assets) = assets else {
        return;
    };

    for (entity, member, appearance) in &crew {
        // A replicated crew member arrives without `Visibility` — presentation
        // is not on the wire — and a parent with none cannot propagate it to
        // the children below. Mirrors `player::dress_chemists`'s identical fix.
        commands.entity(entity).insert_if_new(Visibility::default());

        let body_rest = Vec3::ZERO;
        let (theme, model) = assets.model_for(&member.role);
        commands.spawn((
            Name::new(format!("{} department character", member.role)),
            WorldAssetRoot(model.scene.clone()),
            Transform::from_translation(body_rest),
            Visibility::default(),
            CrewBody {
                crew: entity,
                rest: body_rest,
                rest_rotation: Quat::IDENTITY,
                face_variant: appearance.face_variant.min(2),
                theme,
            },
            ChildOf(entity),
        ));
    }
}

fn attach_crew_animation(
    mut commands: Commands,
    assets: Option<Res<CrewAssets>>,
    mut players: Query<(Entity, &mut AnimationPlayer), Added<AnimationPlayer>>,
    parents: Query<&ChildOf>,
    visuals: Query<&CrewBody>,
    routes: Query<&CrewRoute>,
) {
    let Some(assets) = assets else {
        return;
    };
    for (entity, mut player) in &mut players {
        let Some(visual_entity) = crew_visual_ancestor(entity, &parents, &visuals) else {
            continue;
        };
        let Ok(visual) = visuals.get(visual_entity) else {
            continue;
        };
        let walking = routes.get(visual.crew).is_ok_and(CrewRoute::is_moving);
        let initial = if walking {
            CharacterAnimation::Walk
        } else {
            CharacterAnimation::Idle
        };
        let model = &assets.models[visual.theme];
        let mut transitions = AnimationTransitions::new();
        transitions
            .play(
                &mut player,
                model.animation_nodes[initial as usize],
                Duration::ZERO,
            )
            .repeat();
        commands.entity(entity).insert((
            AnimationGraphHandle(model.animation_graph.clone()),
            transitions,
            CrewAnimationController {
                crew: visual.crew,
                theme: visual.theme,
                current: initial,
            },
        ));
    }
}

/// What an otherwise-idle resident's position says about what they should be
/// shown doing — a relax spot beats a work post so a resident who has wandered
/// to sit down is never shown standing at attention over an empty post
/// nearby. `None` means: keep whatever `desired_character_animation` already
/// picked, ordinarily `Idle`. Pure and Bevy-free on purpose, so it is testable
/// without an `App`, `AssetServer`, or `AnimationGraph` — see
/// `machines::effective_chamber_rate` for the same pattern.
fn post_presentation_animation(
    crew_posts: &CrewPosts,
    member: &CrewMember,
    at: Vec3,
) -> Option<CharacterAnimation> {
    if crew_posts.is_near_relax(at) {
        return Some(CharacterAnimation::Sitting);
    }
    let post = crew_posts.work(&member.name)?;
    let flat = Vec3::new(post.x, at.y, post.z);
    (at.distance(flat) < POST_PROXIMITY).then_some(CharacterAnimation::Working)
}

#[allow(clippy::too_many_arguments)]
fn drive_crew_animation(
    assets: Option<Res<CrewAssets>>,
    crew_posts: Res<CrewPosts>,
    bloods: Query<&Bloodstream>,
    routes: Query<&CrewRoute>,
    errands: Query<&Errand>,
    pursuits: Query<&crate::showdown::Pursuit>,
    members: Query<&CrewMember>,
    transforms: Query<&Transform>,
    mut players: Query<(
        &mut AnimationPlayer,
        &mut AnimationTransitions,
        &mut CrewAnimationController,
    )>,
) {
    let Some(assets) = assets else {
        return;
    };
    for (mut player, mut transitions, mut controller) in &mut players {
        let Ok(blood) = bloods.get(controller.crew) else {
            continue;
        };
        // All three ways a body can be walking, not just the ordinary one.
        // `Errand` and `showdown::Pursuit` both *replace* `CrewRoute` — that
        // is deliberate, since two systems writing one `Transform` fight — so
        // reading the route alone meant anyone on an errand or a pursuit was
        // animated as standing still while gliding across the floor. That had
        // been true of every assailant and cultist in the game since `Pursuit`
        // was written; errands would have made it a daylight problem.
        let moving = routes.get(controller.crew).is_ok_and(CrewRoute::is_moving)
            || errands.get(controller.crew).is_ok_and(Errand::is_moving)
            || pursuits
                .get(controller.crew)
                .is_ok_and(crate::showdown::Pursuit::is_moving);
        let mut desired = desired_character_animation(&blood.0, moving);
        // Neither `Working` nor `Sitting` has a bloodstream signal of its
        // own — both are a presentation choice layered on top of an
        // otherwise-plain `Idle`, derived from comparing the resident's own
        // (replicated) `Transform` against the (locally-known) `CrewPosts`
        // marker positions. No new network state: both peers already have
        // everything this needs.
        if desired == CharacterAnimation::Idle {
            if let (Ok(member), Ok(transform)) =
                (members.get(controller.crew), transforms.get(controller.crew))
            {
                if let Some(post_animation) =
                    post_presentation_animation(&crew_posts, member, transform.translation)
                {
                    desired = post_animation;
                }
            }
        }
        let model = &assets.models[controller.theme];
        let node = model.animation_nodes[desired as usize];
        let speed = character_animation_speed(&blood.0, desired);
        if desired != controller.current {
            transitions
                .play(&mut player, node, Duration::from_millis(260))
                .repeat()
                .set_speed(speed);
            controller.current = desired;
        } else if let Some(active) = player.animation_mut(node) {
            active.set_speed(speed);
        }
    }
}

fn crew_visual_ancestor(
    mut entity: Entity,
    parents: &Query<&ChildOf>,
    visuals: &Query<&CrewBody>,
) -> Option<Entity> {
    for _ in 0..64 {
        if visuals.contains(entity) {
            return Some(entity);
        }
        entity = parents.get(entity).ok()?.parent();
    }
    None
}

fn face_variant_from_name(name: &str) -> Option<u8> {
    let prefix = name.strip_prefix("Face")?;
    let digits = prefix.get(..2)?;
    digits.parse::<u8>().ok()?.checked_sub(1)
}

fn configure_crew_faces(
    mut commands: Commands,
    mut nodes: Query<(Entity, &Name, &mut Visibility), Without<CrewFaceConfigured>>,
    parents: Query<&ChildOf>,
    visuals: Query<&CrewBody>,
) {
    for (entity, name, mut visibility) in &mut nodes {
        let Some(variant) = face_variant_from_name(name.as_str()) else {
            continue;
        };
        let Some(visual_entity) = crew_visual_ancestor(entity, &parents, &visuals) else {
            continue;
        };
        let selected = visuals
            .get(visual_entity)
            .is_ok_and(|visual| visual.face_variant == variant);
        *visibility = if selected {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        commands.entity(entity).insert(CrewFaceConfigured);
    }
}

fn tag_crew_surfaces(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    meshes: Query<
        (Entity, &MeshMaterial3d<StandardMaterial>),
        (With<Mesh3d>, Without<CrewSurface>),
    >,
    parents: Query<&ChildOf>,
    visuals: Query<&CrewBody>,
) {
    for (entity, material) in &meshes {
        let Some(visual_entity) = crew_visual_ancestor(entity, &parents, &visuals) else {
            continue;
        };
        let Ok(visual) = visuals.get(visual_entity) else {
            continue;
        };
        let Some(source) = materials.get(&material.0).cloned() else {
            continue;
        };
        let base_color = source.base_color;
        let base_alpha_mode = source.alpha_mode;
        commands.entity(entity).insert((
            MeshMaterial3d(materials.add(source)),
            CrewSurface {
                crew: visual.crew,
                base_color,
                base_alpha_mode,
            },
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
    mut commands: Commands,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
    speech: Option<Res<crate::threat::Authored<crate::speech::SpeechScript>>>,
    mut crew: Query<(Entity, &Body, &CrewMember, &mut CrewRoute), Changed<Body>>,
) {
    for (entity, body, member, mut route) in &mut crew {
        if !body.0.collapsed || route.phase == CrewPhase::Leaving {
            continue;
        }
        route.leave();
        // Said in the room, on the way down, before the radio's report of it
        // reaches anyone. `speech` owns the pool and the picking; this system
        // owns *when* someone falls over, and there is deliberately only one
        // query watching for that.
        if let Some((line, tone)) =
            crate::speech::collapse_line(speech.as_ref().map(|script| &script.0), member)
        {
            crate::speech::say(&mut commands, entity, line, tone);
        }
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
    crew_posts: Res<CrewPosts>,
    station: Option<Res<crate::orders::StationData>>,
    resident: Query<&CrewMember, With<Ambient>>,
) {
    let Some(station) = station else {
        return;
    };

    for def in &station.crew {
        // Their own post if the station has authored one, otherwise the
        // shared department point exactly as before posts existed.
        let Some(home) = crew_posts.work(&def.name).or_else(|| departments.home(&def.role))
        else {
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
        commands.entity(crew).insert(CrewRoute::to(home));
        // A resident's "ordinary interaction" `sync_medical_evacuation_prompt`
        // already talks about displacing — without this they have no
        // `Interactable` at all, so `Focus` never locks onto them and no
        // verb reaches them, apply-held (inject/splash) included, since it
        // reads the same focused target every other interaction does.
        commands
            .entity(crew)
            .insert(Interactable::new(format!("{} — {}", def.name, def.role)));
    }
}

/// Marks a visiting customer as actually a station resident temporarily
/// pulled off ambient duty — see [`recall_resident_for_order`]. Read once, by
/// [`walk_route`], at the exact moment an ordinary visit would despawn: this
/// one resumes [`Ambient`] duty instead of vanishing for the rest of the
/// shift.
#[derive(Component)]
pub(crate) struct ReturnsToDuty;

/// Sends an existing off-duty resident to the counter to collect an order,
/// instead of spawning a second, unlinked entity under the same name.
///
/// [`populate_departments`]'s own dedupe comment already states the rule —
/// "spawning a second Dr. Vance would put the same named individual in two
/// places" — but only guarded the resident side of it. `orders::
/// generate_orders` and `generate_specific_orders` draw customers from the
/// exact same roster, and until this existed did precisely that: the named
/// resident kept wandering the station exactly as before while a second,
/// identical-looking body under the same name separately walked to the
/// counter to collect their order. From the player's chair the two are
/// indistinguishable, so it read as "the order never switches over to
/// walking to the window" — whichever one they were watching genuinely never
/// did, because it was never the one carrying the order.
///
/// Reused rather than respawned so the walk starts from wherever the
/// resident actually is this instant, not a fresh off-screen door spawn.
/// `route` is written through the live query rather than via
/// `Commands::insert`, which would re-trigger [`start_crew_at_their_department`]'s
/// Added-`CrewRoute` teleport and snap them home first.
///
/// `None` if nobody by that name is currently free to be pulled off duty —
/// no map loaded at all (residents never populate without one), or the one
/// resident by that name is down. The caller falls back to spawning an
/// ordinary, disposable customer in that case, exactly as before this
/// existed.
pub fn recall_resident_for_order(
    commands: &mut Commands,
    residents: &mut Query<
        (Entity, &CrewMember, &Body, &Bloodstream, &mut CrewRoute),
        With<Ambient>,
    >,
    name: &str,
    role: &str,
    lane_offset: f32,
) -> Option<Entity> {
    for (entity, member, body, blood, mut route) in residents.iter_mut() {
        if member.name != name || body.0.collapsed || blood.0.incapacitated() {
            continue;
        }
        let lane = if role == "Medical" {
            DeliveryLane::Medical
        } else {
            DeliveryLane::Public
        };
        *route = CrewRoute::arrival_for(lane, lane_offset);
        commands
            .entity(entity)
            .remove::<Ambient>()
            .insert(ReturnsToDuty);
        return Some(entity);
    }
    None
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
    crew_posts: Res<CrewPosts>,
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
                crew_posts
                    .work(&member.name)
                    .or_else(|| departments.home(&member.role))
            };

            if let Some(post) = post {
                // Only walk if they are not already standing there, or they
                // shuffle on the spot for the whole crisis.
                let flat = Vec3::new(post.x, transform.translation.y, post.z);
                if transform.translation.distance(flat) > POST_PROXIMITY {
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

        // A chance to relax at a communal spot first — independent of the
        // ordinary elsewhere/home choice below, so relax spots
        // (concentrated in the Service hall to start) draw residents
        // station-wide, not just Service's own.
        let relaxing = (rand::random_range(0..4) == 0)
            .then(|| crew_posts.random_relax())
            .flatten();

        // Somewhere else on the station: another department, or their own —
        // preferring their own post over the shared department point when
        // they stay home, the same fix `populate_departments` already gets.
        let destination = relaxing.or_else(|| {
            let next = departments.somewhere_else(&member.role)?;
            Some(if Some(next) == departments.home(&member.role) {
                crew_posts.work(&member.name).unwrap_or(next)
            } else {
                next
            })
        });

        if let Some(destination) = destination {
            route.pending = Some(destination);
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
    crew_posts: Res<CrewPosts>,
    mut arriving: Query<(&CrewMember, &mut Transform), Added<CrewRoute>>,
) {
    for (member, mut transform) in &mut arriving {
        let home = crew_posts
            .work(&member.name)
            .or_else(|| departments.home(&member.role));
        if let Some(home) = home {
            transform.translation.x = home.x;
            transform.translation.z = home.z;
        }
    }
}

/// Crew respond to mind/body chemistry instead of merely carrying a hidden
/// bloodstream. Drowsy or paranoid people abandon their visit, Happiness
/// makes residents linger socially, and strong Sadness makes them withdraw.
/// A chemically incapacitated person remains down until the sedative clears;
/// their already-marked leaving route then resumes toward help.
fn react_to_chemical_statuses(
    mut crew: Query<(&Bloodstream, &mut CrewRoute, Option<&mut Ambient>)>,
) {
    for (blood, mut route, ambient) in &mut crew {
        let sedated = blood.0.status(StatusKind::Sedated).intensity > 0.0;
        let paranoid = blood.0.status(StatusKind::Paranoid).intensity > 0.0;
        let sadness = blood.0.status(StatusKind::Sadness).intensity;
        if route.phase != CrewPhase::Leaving
            && (sedated || paranoid || sadness >= 0.75 || blood.0.incapacitated())
        {
            route.leave();
        }

        // Euphoric residents linger instead of immediately resuming their
        // station circuit: benign and social, distinct from drunken
        // staggering or paranoia's flight response.
        let euphoria = blood.0.status(StatusKind::Euphoric).intensity;
        let happiness = blood.0.status(StatusKind::Happiness).intensity;
        let positive_mood = euphoria.max(happiness);
        if positive_mood > 0.0 && route.pending.is_none() && route.index >= route.waypoints.len() {
            if let Some(mut ambient) = ambient {
                let linger = if happiness > 0.0 {
                    2.5 + happiness.min(2.0)
                } else {
                    2.5
                };
                ambient.dwell = ambient.dwell.max(linger);
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
                crate::interaction::REACH,
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

pub(crate) fn walk_route(
    mut commands: Commands,
    time: Res<Time>,
    nav: Res<crate::nav::NavGraph>,
    areas: Option<Res<crate::lab::WalkableAreas>>,
    departments: Res<Departments>,
    delivery_stations: Res<DeliveryStations>,
    mut crew: Query<(
        Entity,
        &mut Transform,
        &mut CrewRoute,
        Option<&CrewMember>,
        Option<&Bloodstream>,
        Has<ReturnsToDuty>,
    )>,
) {
    for (entity, mut transform, mut route, member, blood, returns_to_duty) in &mut crew {
        // Any chemical sedation stops a resident in place. Mild sedation is
        // still drowsiness rather than incapacity, but letting that resident
        // briskly walk home immediately after deciding to leave contradicts
        // both the status presentation and the treatment response. Their
        // already-marked Leaving route can resume once the sedative clears.
        if blood.is_some_and(|blood| {
            blood.0.status(StatusKind::Sedated).intensity > 0.0 || blood.0.incapacitated()
        }) {
            // Standing still on purpose is not being stuck, and the watchdog
            // below cannot tell the two apart on its own.
            route.stall.restart();
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
            // Anywhere but out of the station, the destination is pulled onto
            // floor a body can stand on. Nobody authoring a delivery window, a
            // department spot or a work post measures it against
            // `nav::NAV_RADIUS`, and a goal a few centimetres inside the
            // furniture is one containment holds them off of forever — the
            // crew member a metre from the window whose order times out. A
            // leaver is the exception on purpose: walking off the floor is how
            // they exit, and clamping that would pin them at the threshold.
            let goal = if route.phase == CrewPhase::Leaving {
                goal
            } else {
                nav.standable_goal(goal)
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
            // A new route is longer than whatever was left of the old one, so
            // the watchdog's running measure no longer means anything.
            route.stall.restart();
            route.unstick = 0;
        }

        let Some(target) = route.waypoints.get(route.index).copied() else {
            // Route finished. Arriving crew wait; leaving crew are done —
            // unless they were only ever a resident recalled for this one
            // order (see `recall_resident_for_order`), in which case "done"
            // means back to ambient duty, not gone for the rest of the shift.
            if route.phase == CrewPhase::Leaving {
                if returns_to_duty {
                    commands
                        .entity(entity)
                        .remove::<ReturnsToDuty>()
                        .insert(Ambient::new(rand::random_range(
                            DWELL_SECONDS.0..=DWELL_SECONDS.1,
                        )));
                    route.phase = CrewPhase::Arriving;
                } else {
                    commands.entity(entity).despawn();
                }
            } else if route.phase == CrewPhase::Arriving {
                route.phase = CrewPhase::Waiting;
                commands
                    .entity(entity)
                    .insert(AtCounter(route.delivery_lane));
            }
            continue;
        };

        // Getting nowhere: send them to open floor, then back to the same
        // goal.
        //
        // Following waypoints is not the same as arriving at them. A body
        // whose next waypoint lies past a corner it cannot round, or whose
        // destination is authored a few centimetres inside a counter, walks
        // straight at it and is held off by `contain_on_surface` — heading
        // into the wall, animating a walk cycle, and not moving a millimetre,
        // for the rest of the shift. That is the crew member standing beside
        // the vending machine while their order at the delivery window times
        // out, and it is invisible to every check the route already makes:
        // the path is valid, the waypoint is standable, nothing has failed.
        //
        // So measure the one thing that fails — the route getting shorter.
        // When it stops, the fix is not to plan the same route again (it was
        // never the problem) but to walk somewhere with room around it first;
        // from open floor the way on is usually clear. The goal is kept, so
        // this costs a detour rather than the errand.
        let remaining =
            crate::nav::floor_length(transform.translation, &route.waypoints[route.index..]);
        if route.stall.stalled(time.delta_secs(), remaining) {
            let goal = *route.waypoints.last().expect("a target implies a route");
            let detour = nav
                .recovery_point(transform.translation, route.unstick)
                .and_then(|recovery| Some((recovery, nav.path(recovery, goal)?)));
            route.unstick += 1;
            // No recovery point, or no way on from it: leave the route alone
            // rather than replace it with something that ends nowhere near
            // the goal — the phase logic above treats a finished route as an
            // arrival, and a false arrival is worse than a stuck body.
            if let Some((recovery, onward)) = detour {
                route.waypoints = std::iter::once(recovery).chain(onward).collect();
                route.index = 0;
                route.stall.restart();
                continue;
            }
        }

        // Horizontal, on both counts, and that is load-bearing rather than a
        // simplification.
        //
        // `contain_on_surface` below owns the vertical axis outright: it
        // rewrites y to `floor + BODY_OFFSET` on every single step. A body
        // therefore *cannot* close a vertical gap, so measuring one is asking
        // a question the answer to which is permanently no.
        //
        // That is not hypothetical. `nav::MAX_PORTAL_STEP` deliberately joins
        // regions whose floors differ by up to 0.45 m — a stair run meeting
        // its landing — and the portal between them carries that difference
        // into its waypoint. With a 3D test against [`ARRIVE_EPSILON`] (0.12)
        // a body standing exactly on such a portal in XZ was still 0.24 m
        // short in y, stepped straight up at it, got snapped back down by
        // containment, and did that forever: stationary, facing a wall, with
        // a live order nobody could fill. Twenty-odd such portals exist around
        // the maintenance stairs.
        //
        // Height is the floor's business, which is exactly how slopes already
        // work — walk horizontally, and let containment put the feet down.
        let to_target = target - transform.translation;
        let flat = Vec2::new(to_target.x, to_target.z);
        if flat.length() <= ARRIVE_EPSILON {
            route.index += 1;
            continue;
        }

        let chemistry = blood.map_or(1.0, |blood| blood.0.movement_multiplier());
        let stumble = crew_stride_multiplier(entity, time.elapsed_secs(), blood);
        let heading = Vec3::new(to_target.x, 0.0, to_target.z).normalize_or_zero();
        let step = heading * WALK_SPEED * chemistry * stumble * time.delta_secs();
        // Confined to the walkable floor, exactly like the chemist
        // (`player::apply_movement`) and a hostile pursuer
        // (`showdown::run_pursuers`). Following waypoints is not on its own
        // enough to stay inside the station: `NavGraph::locate` falls back to
        // the *nearest* region when a point is in none, so the first leg of a
        // route out of the spawn point and the last leg onto a destination
        // marker authored inside a wall both leave the floor. Every other body
        // in the game was already clamped; crew were the ones that were not,
        // and they are the ones the player watches all shift.
        //
        // The one exemption is the last leg of a route out: leaving the
        // station means walking off the walkable floor on purpose, and that
        // arrival is what despawns them. Clamping it would pin every leaver
        // at the threshold, never reaching the waypoint, never despawning —
        // a leak of exactly the kind
        // `showdown::a_finished_showdown_never_leaves_anything_behind`
        // exists to catch. They are still confined for the whole walk
        // *through* the building; only the step out of the door is free.
        let walked_from = transform.translation;
        let candidate = walked_from + step;
        let leaving_the_station =
            route.phase == CrewPhase::Leaving && route.index + 1 == route.waypoints.len();
        transform.translation = match (leaving_the_station, areas.as_deref()) {
            (false, Some(areas)) => {
                areas.contain_on_surface(candidate, crate::nav::NAV_RADIUS, BODY_OFFSET)
            }
            _ => candidate,
        };
        // Face the direction of travel so they read as people rather than
        // sliding props.
        transform.rotation = Quat::from_rotation_y(to_target.x.atan2(to_target.z));

        // Held back by the floor: the route is stale, so plan it again from
        // where the body actually ended up.
        //
        // Containment does not merely stop a body, it *moves* it — a step into
        // a wall comes back as a step along it, because `Bounds::nearest`
        // clamps each axis on its own. So a body kept from walking its route
        // is also being pushed off the line that route was planned for, and
        // the waypoint it is still aiming at can end up behind a corner it
        // cannot cut. One Dijkstra from its real position answers with the
        // portal that suits the region it is really in, and it costs a corner
        // scrape rather than the two seconds the watchdog above needs to
        // notice — which is the difference between a walk that looks slightly
        // clumsy and a crew member who visibly gives up.
        //
        // Deliberately does *not* touch the stall watchdog. A replan that
        // hands back the same unwalkable route is precisely what the watchdog
        // is for, and restarting its window here would let a body flip between
        // two stale routes forever without ever being rescued.
        route.replan_in -= time.delta_secs();
        let held_back =
            crate::nav::flat_distance(transform.translation, walked_from) < step.length() * 0.5;
        if held_back && !leaving_the_station && route.replan_in <= 0.0 {
            route.replan_in = BLOCKED_REPLAN_SECONDS;
            let goal = *route.waypoints.last().expect("a target implies a route");
            if let Some(fresh) = nav.path(transform.translation, goal) {
                route.waypoints = fresh;
                route.index = 0;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Errands
// ---------------------------------------------------------------------------

/// How often an errand-runner re-samples its goal and rebuilds its route.
/// Matches `showdown`'s pursuit cadence: an errand goal can move (a beaker in
/// someone's hand) for exactly the same reason a chemist can.
const ERRAND_REPLAN_SECONDS: f32 = 0.25;
/// How close an errand-runner has to get to count as having arrived. The same
/// distance an assailant needs to land a hit, and for the same reason: it is
/// arm's length, and everything an errand does on arrival is something a
/// person does with their hands.
const ERRAND_REACH: f32 = 1.2;
/// How long an errand may take before it is written off.
///
/// The backstop, not the mechanism. Nav failing outright already ends an
/// errand immediately; this catches the slower failures that have no single
/// frame to point at — a goal drifting away as fast as it is chased, or a
/// route that exists but leads somewhere the walker can never quite reach.
/// Without it an errand-runner walks into a corner forever.
const ERRAND_DEADLINE_SECONDS: f32 = 45.0;

/// What an errand is aimed at.
///
/// A moving [`ErrandGoal::Target`] rather than only a fixed point because the
/// interesting errands are aimed at *objects*, and an object can be picked up
/// mid-walk. That is not a failure to handle defensively — it is the player
/// beating the saboteur to the beaker, and the errand has to be able to say so.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ErrandGoal {
    /// Somewhere on the station. No consumer yet — `saboteur`, the first
    /// thread migrated onto errands, is aimed at an object — but `security`'s
    /// sweep is a walk to a *place*, and the tests below cover this arm today.
    #[cfg_attr(not(test), allow(dead_code))]
    Point(Vec3),
    Target(Entity),
}

/// Walk somewhere, then do something when you get there.
///
/// The verb the station's NPCs were missing. Until this, a body could stand at
/// the counter holding an [`crate::orders::Order`], chase the chemist
/// (`showdown::Pursuit`), or wander ([`Ambient`]) — so any antagonist whose
/// payoff was neither of those three had nowhere to put it and reached for a
/// global query instead, acting on the far side of the station without ever
/// walking there.
///
/// **Replaces [`CrewRoute`] rather than coexisting with it**, exactly as
/// `Pursuit` does and for the same reason: two systems writing one `Transform`
/// fight, and a fixed waypoint list has nothing useful to say about a goal that
/// can move or vanish. Use [`send_on_errand`], which does both halves; the
/// query in [`run_errands`] additionally refuses to walk a body that still has
/// a `CrewRoute`, so forgetting is inert rather than a body shuddering between
/// two destinations.
///
/// Ends exactly once, by removing itself and writing one [`ErrandResolved`].
/// Whoever set the errand decides what that means — this component has no
/// opinion about what happens on arrival, which is what lets one primitive
/// serve a theft, a contamination and a sweep.
#[derive(Component)]
pub struct Errand {
    goal: ErrandGoal,
    speed: f32,
    arrive_within: f32,
    /// Seconds before the errand is written off. See
    /// [`ERRAND_DEADLINE_SECONDS`].
    expires_in: f32,
    /// Whether the last tick actually moved them. Read by
    /// [`drive_crew_animation`] to pick the walk cycle — see
    /// [`Errand::is_moving`].
    moving: bool,
    trail: crate::nav::Trail,
}

impl Errand {
    fn new(goal: ErrandGoal) -> Self {
        Self {
            goal,
            // An errand is an ordinary walk, at an ordinary walking pace —
            // the same `WALK_SPEED` every other crew member on the station
            // moves at. Only a pursuit has a reason to be faster.
            speed: WALK_SPEED,
            arrive_within: ERRAND_REACH,
            expires_in: ERRAND_DEADLINE_SECONDS,
            moving: false,
            trail: crate::nav::Trail::default(),
        }
    }

    /// Whether they are actually walking right now.
    ///
    /// Recorded per tick rather than inferred from the component existing,
    /// which would be the easy version and wrong in both directions that
    /// matter: a body sedated mid-errand, or held at a wall by containment
    /// chasing a goal it cannot reach, still *has* an errand — and would walk
    /// briskly on the spot for as long as it lasted.
    pub fn is_moving(&self) -> bool {
        self.moving
    }
}

/// How an errand ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrandOutcome {
    /// Got there. Whatever the errand was for can happen now.
    Arrived,
    /// Did not, and will not: the goal was picked up, drunk or despawned, no
    /// route to it exists, or it took too long. Deliberately one outcome
    /// rather than three — every caller so far does the same thing with all of
    /// them, and splitting them would invite a caller to handle one and
    /// silently drop the others.
    Unreachable,
}

/// One errand, finished.
///
/// A message rather than a component mutation so a thread module can react to
/// an arrival without owning any locomotion — the same separation
/// [`crate::orders::OrderResolved`] already gives the order pipeline, and the
/// reason `saboteur` can be rebuilt around walking without gaining a single
/// line about waypoints.
/// Deliberately identifies the *walker* rather than the goal. A thread that
/// sent someone on an errand marks them with its own component (`saboteur`'s
/// `Meddling`), so it can tell its own arrivals from another module's and
/// carry whatever else it needs to know alongside — which the goal alone could
/// not do anyway.
#[derive(Message, Clone, Copy, Debug)]
pub struct ErrandResolved {
    pub walker: Entity,
    pub outcome: ErrandOutcome,
}

/// Sends a body on an errand, taking them off their ordinary route.
///
/// The `CrewRoute` removal is not the caller's to remember — see [`Errand`].
pub fn send_on_errand(commands: &mut Commands, walker: Entity, goal: ErrandGoal) {
    commands
        .entity(walker)
        .remove::<CrewRoute>()
        .insert(Errand::new(goal));
}

/// Walks everyone who is on an errand, and reports the ones that ended.
///
/// The locomotion itself is [`crate::nav::Trail`], shared with
/// `showdown::run_pursuers` — including the three-part arrival test, which is
/// not paranoia: Euclidean proximity alone counts a body as having arrived at
/// something on the far side of a wall it is standing against.
pub(crate) fn run_errands(
    mut commands: Commands,
    time: Res<Time>,
    nav: Option<Res<crate::nav::NavGraph>>,
    areas: Option<Res<crate::lab::WalkableAreas>>,
    mut resolved: MessageWriter<ErrandResolved>,
    mut runners: Query<
        (Entity, &mut Transform, &mut Errand, Option<&Bloodstream>),
        Without<CrewRoute>,
    >,
    goals: Query<&Transform, Without<Errand>>,
) {
    let dt = time.delta_secs();

    for (entity, mut transform, mut errand, blood) in &mut runners {
        // Sedation stops a body mid-errand, exactly as `walk_route` already
        // stops a sedated crew member on an ordinary route. A saboteur you
        // put down on their way to the beaker does not keep walking, and the
        // errand is not cancelled either — it resumes if they come round.
        if blood.is_some_and(|blood| {
            blood.0.status(StatusKind::Sedated).intensity > 0.0 || blood.0.incapacitated()
        }) {
            errand.moving = false;
            continue;
        }
        // Cleared up front so every path out of this loop that does not reach
        // the walk below — no goal, no route, arrived — leaves them standing
        // still rather than holding last tick's stride.
        errand.moving = false;

        // Where the goal is *now*, not where it was when the errand was set.
        let at = match errand.goal {
            ErrandGoal::Point(at) => Some(at),
            ErrandGoal::Target(target) => goals.get(target).ok().map(|at| at.translation),
        };

        errand.expires_in -= dt;
        let give_up = |commands: &mut Commands, resolved: &mut MessageWriter<ErrandResolved>| {
            commands.entity(entity).remove::<Errand>();
            resolved.write(ErrandResolved {
                walker: entity,
                outcome: ErrandOutcome::Unreachable,
            });
        };

        // The goal went away mid-walk — picked up, drunk, despawned — or the
        // errand has simply run out of time.
        let (Some(at), true) = (at, errand.expires_in > 0.0) else {
            give_up(&mut commands, &mut resolved);
            continue;
        };

        if errand.trail.due_for_replan(dt, ERRAND_REPLAN_SECONDS) {
            errand.trail.plan(nav.as_deref(), transform.translation, at);
        }
        // Nav has nothing to say, and this system only runs once the map is
        // ready — so this is not a graph still loading, it is genuinely no way
        // there. Stop rather than straight-lining through the wall between.
        if errand.trail.is_empty() {
            give_up(&mut commands, &mut resolved);
            continue;
        }

        let reach = errand.arrive_within;
        let arrived = transform.translation.distance_squared(at) <= reach * reach
            && errand.trail.ends_at(at)
            && errand
                .trail
                .remaining(transform.translation)
                .is_some_and(|walk| walk <= reach);
        if arrived {
            commands.entity(entity).remove::<Errand>();
            resolved.write(ErrandResolved {
                walker: entity,
                outcome: ErrandOutcome::Arrived,
            });
            continue;
        }

        let step = errand.speed * dt;
        if let Some(heading) = errand
            .trail
            .walk(&mut transform, areas.as_deref(), step, BODY_OFFSET)
        {
            // Face the way they are going, the same as `walk_route` — an
            // errand-runner is a person crossing the room, and the whole point
            // of the primitive is that the player can watch them do it.
            transform.rotation = Quat::from_rotation_y(heading.x.atan2(heading.z));
            errand.moving = true;
        }
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
            .init_resource::<CrewPosts>()
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

    fn named_at(app: &mut App, name: &str, role: &str, at: Vec3) -> Entity {
        app.world_mut()
            .spawn((
                CrewMember {
                    name: name.to_string(),
                    role: role.to_string(),
                },
                Transform::from_translation(at),
                CrewRoute::arrival(0.0),
            ))
            .id()
    }

    #[test]
    fn standing_at_a_work_post_selects_working() {
        let mut posts = CrewPosts::default();
        posts.set_work("Tech Lindqvist".to_string(), Vec3::new(-1040.0, 0.0, 3120.0));
        let member = CrewMember {
            name: "Tech Lindqvist".to_string(),
            role: "Engineering".to_string(),
        };
        assert_eq!(
            post_presentation_animation(&posts, &member, Vec3::new(-1040.0, 0.93, 3120.0)),
            Some(CharacterAnimation::Working),
            "close enough to their own post, ignoring the Y difference",
        );
    }

    #[test]
    fn standing_near_someone_elses_work_post_stays_idle() {
        let mut posts = CrewPosts::default();
        posts.set_work("Tech Lindqvist".to_string(), Vec3::new(-1040.0, 0.0, 3120.0));
        let visitor = CrewMember {
            name: "Miner Sato".to_string(),
            role: "Cargo".to_string(),
        };
        assert_eq!(
            post_presentation_animation(&posts, &visitor, Vec3::new(-1040.0, 0.93, 3120.0)),
            None,
            "Lindqvist's post is not Sato's — only your own post reads as work",
        );
    }

    #[test]
    fn walking_toward_a_work_post_stays_idle_until_close() {
        let mut posts = CrewPosts::default();
        posts.set_work("Tech Lindqvist".to_string(), Vec3::new(-1040.0, 0.0, 3120.0));
        let member = CrewMember {
            name: "Tech Lindqvist".to_string(),
            role: "Engineering".to_string(),
        };
        assert_eq!(
            post_presentation_animation(&posts, &member, Vec3::new(-1040.0, 0.93, 3116.0)),
            None,
            "still 4 units short of the post — must not switch early",
        );
    }

    #[test]
    fn sitting_at_a_relax_spot_beats_a_nearby_work_post() {
        // A resident's relax spot and their own work post can end up close
        // together in a small room; relax must win, or someone who has
        // actually sat down would be shown standing at attention instead.
        let mut posts = CrewPosts::default();
        posts.set_work("Chef Dubois".to_string(), Vec3::new(-740.0, 0.0, 1400.0));
        posts.add_relax(Vec3::new(-800.0, 0.0, 1400.0));
        let member = CrewMember {
            name: "Chef Dubois".to_string(),
            role: "Service".to_string(),
        };
        assert_eq!(
            post_presentation_animation(&posts, &member, Vec3::new(-800.0, 0.93, 1400.0)),
            Some(CharacterAnimation::Sitting),
        );
    }

    #[test]
    fn an_authored_post_that_falls_back_to_nothing_stays_idle() {
        let posts = CrewPosts::default();
        let member = CrewMember {
            name: "Dr. Vance".to_string(),
            role: "Medical".to_string(),
        };
        assert_eq!(
            post_presentation_animation(&posts, &member, Vec3::new(-260.0, 0.93, 1080.0)),
            None,
            "no posts authored at all — nothing to select",
        );
    }

    #[test]
    fn a_resident_with_no_authored_post_falls_back_to_the_department_home() {
        // CrewPosts is empty; start_crew_at_their_department must behave
        // exactly as it did before individual posts existed.
        let mut app = walking_app();
        app.world_mut()
            .resource_mut::<Departments>()
            .set("Medical".into(), Vec3::new(-21.0, 0.0, 18.0));
        let crew = named_at(&mut app, "Dr. Vance", "Medical", Vec3::ZERO);

        app.update();

        let at = app.world().get::<Transform>(crew).unwrap().translation;
        assert_eq!((at.x, at.z), (-21.0, 18.0));
    }

    #[test]
    fn two_department_mates_with_distinct_posts_end_up_in_different_places() {
        // The direct regression guard for the bug this phase fixes: before
        // per-person posts, both of Medical's two residents landed on the
        // exact same department point.
        let mut app = walking_app();
        app.world_mut()
            .resource_mut::<Departments>()
            .set("Medical".into(), Vec3::new(-21.0, 0.0, 18.0));
        {
            let mut posts = app.world_mut().resource_mut::<CrewPosts>();
            posts.set_work("Dr. Vance".to_string(), Vec3::new(-20.0, 0.0, 15.0));
            posts.set_work("Nurse Okonkwo".to_string(), Vec3::new(-22.0, 0.0, 19.0));
        }
        let vance = named_at(&mut app, "Dr. Vance", "Medical", Vec3::ZERO);
        let okonkwo = named_at(&mut app, "Nurse Okonkwo", "Medical", Vec3::ZERO);

        app.update();

        let vance_at = app.world().get::<Transform>(vance).unwrap().translation;
        let okonkwo_at = app.world().get::<Transform>(okonkwo).unwrap().translation;
        assert_eq!((vance_at.x, vance_at.z), (-20.0, 15.0));
        assert_eq!((okonkwo_at.x, okonkwo_at.z), (-22.0, 19.0));
        assert_ne!(
            (vance_at.x, vance_at.z),
            (okonkwo_at.x, okonkwo_at.z),
            "two department mates must not stack on one shared point"
        );
    }

    fn crew_roster() -> Vec<CrewDef> {
        ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap()
    }

    fn order_config() -> crate::orders::OrderConfig {
        ron::from_str(include_str!("../../assets/data/station.orders.ron")).unwrap()
    }

    #[test]
    fn ambient_residents_still_populate_when_map_ready_is_not_yet_back() {
        // Regression guard for a real, silent, permanent bug: `lab::tb::
        // collect_loaded_map` removes `MapReady` in the very same command
        // batch that gives `Departments` its real, non-default data, and
        // `nav::rebuild_graph` only reinserts `MapReady` a frame later. Bevy
        // evaluates every run condition attached to a system regardless of
        // whether an earlier one already failed, so a system gated on both
        // `resource_exists::<MapReady>` *and*
        // `resource_exists_and_changed::<Departments>` — as
        // `populate_departments` briefly was, in `CrewPlugin::build` — can
        // have that one real transition land on exactly the frame
        // `MapReady` is absent: the change-detection condition "sees" and
        // consumes it that frame even though the system's body never runs,
        // and by the time `MapReady` comes back `Departments` no longer
        // looks newly changed to it. No ambient crew ever spawned, silently,
        // forever, until a hot map reload — never actually witnessed in play
        // until someone stood in a department and watched nobody arrive.
        // This mirrors `CrewPlugin::build`'s real registration for
        // `populate_departments`; keep the two in sync.
        let roster = crew_roster();
        let mut app = App::new();
        app.insert_resource(crate::orders::StationData {
            crew: roster.clone(),
            config: order_config(),
        })
        .init_resource::<Departments>()
        .init_resource::<CrewPosts>()
        .add_systems(
            Update,
            populate_departments
                .run_if(resource_exists_and_changed::<Departments>)
                .run_if(is_authority),
        );

        // Frame 1: nothing authored yet — spends the trivial "changed" that
        // `init_resource` itself counts as, matching the instant before any
        // map has loaded. Deliberately no `MapReady` resource exists
        // anywhere in this app: the fixed system must not need one.
        app.update();
        assert!(
            app.world_mut()
                .query::<&CrewMember>()
                .iter(app.world())
                .next()
                .is_none(),
            "nobody to spawn before the map has said where anyone lives",
        );

        // The map "loads": exactly what `collect_loaded_map` does to
        // `Departments` — every role on the roster gets somewhere real.
        {
            let mut departments = app.world_mut().resource_mut::<Departments>();
            let roles: std::collections::HashSet<String> =
                roster.iter().map(|def| def.role.clone()).collect();
            for role in roles {
                departments.set(role, Vec3::new(-1.0, 0.0, -1.0));
            }
        }

        app.update();

        let spawned: std::collections::HashSet<String> = app
            .world_mut()
            .query::<&CrewMember>()
            .iter(app.world())
            .map(|member| member.name.clone())
            .collect();
        assert_eq!(
            spawned,
            roster.iter().map(|def| def.name.clone()).collect(),
            "populate_departments must spawn everyone on the same real \
             change that gave Departments its data — not some later update \
             that never actually comes",
        );
    }

    #[test]
    fn an_ambient_resident_can_be_focused_and_injected_or_splashed() {
        // `Focus` (interaction/mod.rs) only ever locks onto an entity with
        // `Interactable`, and every apply-held route — pour, spray, syringe —
        // reads that same focused target. A resident with no `Interactable`
        // is standing in the room but is not actually *there* as far as the
        // player's crosshair or held container is concerned: nothing to
        // click, nothing to inject, nothing to splash. This guards
        // `populate_departments` giving every ambient resident their
        // "ordinary interaction", the counterpart
        // `sync_medical_evacuation_prompt`'s own doc comment already
        // describes displacing.
        let roster = crew_roster();
        let mut app = App::new();
        app.insert_resource(crate::orders::StationData {
            crew: roster.clone(),
            config: order_config(),
        })
        .init_resource::<Departments>()
        .init_resource::<CrewPosts>()
        .add_systems(
            Update,
            populate_departments
                .run_if(resource_exists_and_changed::<Departments>)
                .run_if(is_authority),
        );
        app.update();
        {
            let mut departments = app.world_mut().resource_mut::<Departments>();
            let roles: std::collections::HashSet<String> =
                roster.iter().map(|def| def.role.clone()).collect();
            for role in roles {
                departments.set(role, Vec3::new(-1.0, 0.0, -1.0));
            }
        }
        app.update();

        let mut query = app.world_mut().query::<(&CrewMember, &Interactable)>();
        let by_name: std::collections::HashMap<String, String> = query
            .iter(app.world())
            .map(|(member, tag)| (member.name.clone(), tag.label.clone()))
            .collect();

        for def in &roster {
            let label = by_name
                .get(&def.name)
                .unwrap_or_else(|| panic!("{} has no Interactable — cannot be focused at all", def.name));
            assert_eq!(label, &format!("{} — {}", def.name, def.role));
        }
    }

    #[test]
    fn a_crew_member_gets_one_persistent_bounded_face_choice() {
        let mut app = App::new();
        app.add_systems(Update, assign_crew_appearances);
        let crew = app
            .world_mut()
            .spawn(CrewMember {
                name: "Face Tester".into(),
                role: "Service".into(),
            })
            .id();
        app.update();
        let first = *app.world().get::<CrewAppearance>(crew).unwrap();
        assert!(first.face_variant < 3);
        app.update();
        assert_eq!(
            app.world()
                .get::<CrewAppearance>(crew)
                .unwrap()
                .face_variant,
            first.face_variant,
            "presentation updates must not reroll an established NPC"
        );
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
    fn a_recalled_resident_walks_from_wherever_they_already_are_to_the_counter() {
        // The actual bug this fixes: before `recall_resident_for_order`
        // existed, an order always spawned a brand new customer under the
        // drawn name, leaving the *real* resident of that name still
        // wandering the station untouched — the player was watching the
        // wrong body, and it read as "the order never switches over to
        // walking to the window". Reusing the resident directly means the
        // body already on screen is the one that turns towards the counter.
        use bevy::ecs::system::RunSystemOnce;

        let mut app = walking_app();
        let elsewhere = Vec3::new(-6.0, BODY_OFFSET, -6.0);
        let resident = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Dr. Vance".into(),
                    role: "Medical".into(),
                },
                Transform::from_translation(elsewhere),
                CrewRoute::to(elsewhere),
                Body::default(),
                Bloodstream::default(),
                Ambient::new(5.0),
            ))
            .id();

        fn recall_dr_vance(
            mut commands: Commands,
            mut residents: Query<
                (Entity, &CrewMember, &Body, &Bloodstream, &mut CrewRoute),
                With<Ambient>,
            >,
        ) -> Option<Entity> {
            recall_resident_for_order(&mut commands, &mut residents, "Dr. Vance", "Medical", 0.0)
        }
        let recalled = app.world_mut().run_system_once(recall_dr_vance).unwrap();
        assert_eq!(
            recalled,
            Some(resident),
            "the existing resident should have been reused, not left untouched",
        );
        assert!(
            app.world().get::<Ambient>(resident).is_none(),
            "a recalled resident must give up ambient duty for the length of the visit",
        );

        for _ in 0..400 {
            tick(&mut app, 0.05);
            if app.world().get::<CrewRoute>(resident).unwrap().phase == CrewPhase::Waiting {
                break;
            }
        }
        assert_eq!(
            app.world().get::<CrewRoute>(resident).unwrap().phase,
            CrewPhase::Waiting,
            "the same body that was wandering must be the one that reaches the counter",
        );
    }

    #[test]
    fn a_recalled_resident_resumes_ambient_duty_instead_of_vanishing_when_the_visit_ends() {
        use bevy::ecs::system::RunSystemOnce;

        let mut app = walking_app();
        let from = crate::lab::ROOMS[crate::lab::REACTION_BAY].center();
        let start = Vec3::new(from.x, BODY_OFFSET, from.z);
        let resident = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Dr. Vance".into(),
                    role: "Medical".into(),
                },
                Transform::from_translation(start),
                CrewRoute::to(start),
                Body::default(),
                Bloodstream::default(),
                Ambient::new(5.0),
            ))
            .id();

        fn recall_dr_vance(
            mut commands: Commands,
            mut residents: Query<
                (Entity, &CrewMember, &Body, &Bloodstream, &mut CrewRoute),
                With<Ambient>,
            >,
        ) {
            recall_resident_for_order(&mut commands, &mut residents, "Dr. Vance", "Medical", 0.0);
        }
        app.world_mut().run_system_once(recall_dr_vance).unwrap();
        app.world_mut()
            .get_mut::<CrewRoute>(resident)
            .unwrap()
            .leave();

        for _ in 0..400 {
            tick(&mut app, 0.05);
            if app.world().get::<Ambient>(resident).is_some() {
                break;
            }
        }

        assert!(
            app.world().get::<CrewMember>(resident).is_some(),
            "a recalled resident must not despawn when their visit ends",
        );
        assert!(
            app.world().get::<Ambient>(resident).is_some(),
            "they must resume ambient duty rather than vanish for the rest of the shift",
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
        app.add_plugins((
            MinimalPlugins,
            bevy::state::app::StatesPlugin,
            AssetPlugin::default(),
        ))
        .init_state::<AppState>()
        .init_asset::<Image>()
        .init_resource::<Assets<Mesh>>()
        .init_resource::<Assets<StandardMaterial>>()
        .init_resource::<Departments>()
        .init_resource::<CrewPosts>()
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
                minimum_purity: 0.0,
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
            .spawn((blood, CrewRoute::standing(), Ambient { dwell: 0.1 }))
            .id();

        app.update();

        assert_eq!(app.world().get::<Ambient>(resident).unwrap().dwell, 2.5);
    }

    #[test]
    fn happiness_and_sadness_change_npc_social_behavior() {
        let mut app = App::new();
        app.add_systems(Update, react_to_chemical_statuses);

        let route = CrewRoute::standing;
        let mut happy_blood = Bloodstream::default();
        happy_blood.0.add_status(StatusKind::Happiness, 10.0, 1.5);
        let happy = app
            .world_mut()
            .spawn((happy_blood, route(), Ambient { dwell: 0.1 }))
            .id();

        let mut sad_blood = Bloodstream::default();
        sad_blood.0.add_status(StatusKind::Sadness, 10.0, 1.0);
        let sad = app
            .world_mut()
            .spawn((sad_blood, route(), Ambient { dwell: 8.0 }))
            .id();

        app.update();

        assert_eq!(app.world().get::<Ambient>(happy).unwrap().dwell, 4.0);
        assert_eq!(
            app.world().get::<CrewRoute>(sad).unwrap().phase,
            CrewPhase::Leaving
        );
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

    /// [`walking_app`] plus the floor itself.
    ///
    /// `walking_app` builds a `NavGraph` *from* a floor plan but never inserts
    /// the `WalkableAreas` resource, so `walk_route`'s containment is inert
    /// there — which is exactly why every routing test in this module passed
    /// throughout the years crew were walking through walls. A test that means
    /// to exercise containment has to hand it the floor.
    fn contained_walking_app() -> App {
        let mut app = walking_app();
        app.insert_resource(crate::lab::WalkableAreas::from_floor_plan());
        app
    }

    /// Two flat regions at *different heights*, overlapping enough to be
    /// joined after the nav inset — a stair run meeting its landing, which the
    /// station has twenty-odd of around the maintenance decks.
    ///
    /// `from_floor_plan`'s five rooms are all at y = 0, so nothing built on it
    /// can reproduce this at all. That is precisely why the bug below survived
    /// every existing routing test.
    fn stepped_floor_app(step_height: f32) -> App {
        use crate::lab::{Bounds, FloorProfile};
        let mut areas = crate::lab::WalkableAreas::default();
        areas.push_surface(
            Bounds { min_x: -6.0, max_x: 0.5, min_z: -3.0, max_z: 3.0 },
            Some("Lower".to_string()),
            None,
            FloorProfile::Flat(0.0),
        );
        areas.push_surface(
            Bounds { min_x: -0.5, max_x: 6.0, min_z: -3.0, max_z: 3.0 },
            Some("Upper".to_string()),
            None,
            FloorProfile::Flat(step_height),
        );

        let mut app = walking_app();
        app.insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS));
        app.insert_resource(areas);
        app
    }

    #[test]
    fn a_crew_member_crosses_a_step_between_two_floors_at_different_heights() {
        // The bug this pins put twenty-odd permanent traps around the
        // maintenance stairs, and it is worth stating exactly, because the
        // shape of it is not obvious from any one file.
        //
        // `nav::MAX_PORTAL_STEP` deliberately joins regions whose floors
        // differ by up to 0.45 m. The portal between them inherits that
        // difference. `contain_on_surface` then owns the vertical axis
        // outright — it rewrites y to `floor + BODY_OFFSET` every step — so a
        // body physically cannot move vertically. Measured in 3D against
        // `ARRIVE_EPSILON` (0.12), a body standing dead on such a portal in XZ
        // was still short in y, stepped up at it, got snapped back down, and
        // repeated that forever: motionless, facing a wall, holding an order
        // no chemist could ever fill.
        //
        // The step here is larger than `ARRIVE_EPSILON` and smaller than
        // `MAX_PORTAL_STEP`, which is exactly the band the real map lands in.
        let step_height = 0.24;
        assert!(step_height > ARRIVE_EPSILON, "a smaller step would not bite");

        let mut app = stepped_floor_app(step_height);
        let start = Vec3::new(-4.0, BODY_OFFSET, 0.0);
        let goal = Vec3::new(4.0, step_height + BODY_OFFSET, 0.0);
        let walker = walker(&mut app, start, CrewRoute::to(goal));

        for _ in 0..600 {
            tick(&mut app, 0.05);
        }

        let at = app.world().get::<Transform>(walker).unwrap().translation;
        assert!(
            at.x > 3.0,
            "the body stopped at x={:.2} (started at {:.2}) — it is stuck on \
             the step between the two floors, which is what every crew member \
             on the maintenance stairs used to do, permanently",
            at.x,
            start.x
        );
    }

    /// How far off the walkable floor a body is standing, in metres.
    ///
    /// Zero means containment is a no-op on them — they are already somewhere
    /// they could legitimately stand.
    fn off_the_floor(app: &App, at: Vec3) -> f32 {
        let areas = app.world().resource::<crate::lab::WalkableAreas>();
        at.distance(areas.contain_on_surface(at, crate::nav::NAV_RADIUS, BODY_OFFSET))
    }

    #[test]
    fn a_crew_member_walking_across_the_station_is_never_outside_the_walkable_floor() {
        // The test `a_walk_across_the_suite_is_routed_rather_than_straight_
        // through_walls` above should have been this one. It asserts a route
        // with intermediate waypoints was *produced* and then never walks
        // anybody, so it passed the whole time crew were phasing through
        // walls between those waypoints. This walks the route.
        let mut app = contained_walking_app();
        let from = crate::lab::ROOMS[crate::lab::REACTION_BAY].center();
        let crew = walker(
            &mut app,
            Vec3::new(from.x, BODY_OFFSET, from.z),
            CrewRoute::arrival(0.0),
        );

        for step in 0..600 {
            tick(&mut app, 0.05);
            let Some(at) = app.world().get::<Transform>(crew) else {
                break;
            };
            let strayed = off_the_floor(&app, at.translation);
            assert!(
                strayed < 0.01,
                "step {step}: stood {strayed:.3}m off the walkable floor at {:?}",
                at.translation,
            );
        }
    }

    #[test]
    fn a_crew_member_sent_to_a_spot_off_the_walkable_floor_stops_at_the_edge() {
        // The failure mode with teeth, and the one a cross-station stroll does
        // not reach: the destination itself is not standable. A `crew_post` or
        // `department_spot` authored a little into a wall does this, and
        // `NavGraph::path` faithfully returns the unstandable point as the
        // final waypoint — it has to, because walking off the floor is also
        // how a crew member legitimately *leaves*. Containment is what tells
        // the two apart, so this is the test that fails without it.
        let void = Vec3::new(-10.0, BODY_OFFSET, 0.0);
        let mut app = contained_walking_app();
        assert!(
            off_the_floor(&app, void) > 1.0,
            "the fixture point must really be off the floor, or this proves nothing",
        );

        let from = crate::lab::ROOMS[crate::lab::REACTION_BAY].center();
        let crew = walker(
            &mut app,
            Vec3::new(from.x, BODY_OFFSET, from.z),
            CrewRoute::to(void),
        );

        for _ in 0..400 {
            tick(&mut app, 0.05);
            let at = app.world().get::<Transform>(crew).unwrap().translation;
            let strayed = off_the_floor(&app, at);
            assert!(
                strayed < 0.01,
                "walked {strayed:.3}m off the floor at {at:?}, chasing a goal in the void",
            );
        }
    }

    #[test]
    fn a_destination_authored_off_the_floor_is_one_crew_can_still_arrive_at() {
        // The other half of the test above, and the fix rather than the
        // backstop. Stopping at the edge keeps a body out of the wall but
        // leaves it *travelling* forever: never within `ARRIVE_EPSILON`, never
        // `Waiting`, never `AtCounter` — which for an order is a customer who
        // stands a metre from the window until their order times out. A goal
        // nobody measured against `nav::NAV_RADIUS` is common enough (a
        // delivery window, a work post, a department spot) that it has to be
        // survivable, so the destination is pulled onto standable floor before
        // anyone walks at it.
        let void = Vec3::new(-10.0, BODY_OFFSET, 0.0);
        let mut app = contained_walking_app();
        assert!(
            off_the_floor(&app, void) > 1.0,
            "the fixture point must really be off the floor",
        );

        let from = crate::lab::ROOMS[crate::lab::REACTION_BAY].center();
        let crew = walker(
            &mut app,
            Vec3::new(from.x, BODY_OFFSET, from.z),
            CrewRoute::to(void),
        );

        for _ in 0..600 {
            tick(&mut app, 0.05);
            if app.world().get::<CrewRoute>(crew).unwrap().phase == CrewPhase::Waiting {
                break;
            }
        }

        let at = app.world().get::<Transform>(crew).unwrap().translation;
        assert_eq!(
            app.world().get::<CrewRoute>(crew).unwrap().phase,
            CrewPhase::Waiting,
            "never arrived — still walking at a point off the floor, from {at:?}",
        );
        assert!(
            off_the_floor(&app, at) < 0.01,
            "arrived somewhere they cannot stand, at {at:?}",
        );
        assert!(
            app.world().get::<AtCounter>(crew).is_some(),
            "arrival has to be the one the order queue can see",
        );
    }

    #[test]
    fn a_body_wedged_on_a_goal_it_can_never_reach_is_still_moved_off_the_wall() {
        // The backstop behind the backstop. Planning again only helps when a
        // better route exists; when the destination *itself* cannot be stood
        // on, the route is correct and still unwalkable, and every replan
        // hands back the same wall. The progress watchdog is what covers that:
        // it does not look at the route at all, only at whether the body is
        // getting anywhere, so it fires on causes nobody has diagnosed.
        let mut app = contained_walking_app();
        let from = crate::lab::ROOMS[crate::lab::REACTION_BAY].center();
        let crew = walker(
            &mut app,
            Vec3::new(from.x, BODY_OFFSET, from.z),
            CrewRoute::to(Vec3::new(from.x, BODY_OFFSET, from.z)),
        );
        tick(&mut app, 0.01);

        // Somewhere out in the void, west of the station: reachable by no
        // route, and therefore proof against replanning.
        {
            let mut route = app.world_mut().get_mut::<CrewRoute>(crew).unwrap();
            route.waypoints = vec![Vec3::new(-40.0, BODY_OFFSET, 0.0)];
            route.index = 0;
        }
        for _ in 0..40 {
            tick(&mut app, 0.05);
        }
        let wedged = app.world().get::<Transform>(crew).unwrap().translation;

        for _ in 0..100 {
            tick(&mut app, 0.05);
        }

        let route = app.world().get::<CrewRoute>(crew).unwrap();
        assert!(
            route.unstick > 0,
            "the watchdog never noticed a body walking into a wall for five seconds",
        );
        let at = app.world().get::<Transform>(crew).unwrap().translation;
        assert!(
            crate::nav::flat_distance(at, wedged) > 0.5,
            "still standing where they wedged, at {at:?}",
        );
        assert!(
            off_the_floor(&app, at) < 0.01,
            "the rescue put them off the walkable floor at {at:?}",
        );
    }

    #[test]
    fn a_crew_member_on_a_leg_it_cannot_walk_still_reaches_its_destination() {
        // The complaint this exists for: crew standing motionless against a
        // wall somewhere on the station while the order they came for times
        // out at a delivery window they never reached. Every check the route
        // makes says it is fine — the path is valid, the waypoints are
        // standable — because the failure is not in the route at all. It is a
        // body held off its next waypoint by containment, walking into
        // geometry forever.
        //
        // The fixture is a leg that cannot be walked: a waypoint out in the
        // void west of the station, with the real destination behind it. Every
        // other guard in the module passes it — the route is a list of points,
        // the destination is standable, nothing errors — and the body walks at
        // the impossible one until the shift ends. Sabotaging the waypoints
        // directly is the only way to hold that shape still; in the wild it
        // comes from geometry, and the whole difficulty is that geometry does
        // not announce itself.
        let mut app = contained_walking_app();
        let from = crate::lab::ROOMS[crate::lab::REACTION_BAY].center();
        let to = crate::lab::ROOMS[crate::lab::ANALYSIS].center();
        let goal = Vec3::new(to.x, BODY_OFFSET, to.z);
        let crew = walker(
            &mut app,
            Vec3::new(from.x, BODY_OFFSET, from.z),
            CrewRoute::to(goal),
        );
        tick(&mut app, 0.01);

        let void = Vec3::new(-40.0, BODY_OFFSET, 0.0);
        {
            let mut route = app.world_mut().get_mut::<CrewRoute>(crew).unwrap();
            route.waypoints = vec![void, goal];
            route.index = 0;
        }

        for _ in 0..600 {
            tick(&mut app, 0.05);
            let at = app.world().get::<Transform>(crew).unwrap().translation;
            let strayed = off_the_floor(&app, at);
            assert!(
                strayed < 0.01,
                "the rescue walked them {strayed:.3}m off the floor at {at:?}",
            );
            if app.world().get::<CrewRoute>(crew).unwrap().phase == CrewPhase::Waiting {
                break;
            }
        }

        let route = app.world().get::<CrewRoute>(crew).unwrap();
        assert_eq!(
            route.phase,
            CrewPhase::Waiting,
            "never got past the impossible leg — this is the crew member \
             standing at a wall while their order times out",
        );
        let at = app.world().get::<Transform>(crew).unwrap().translation;
        assert!(
            crate::nav::flat_distance(at, goal) < 1.0,
            "gave up somewhere else entirely, at {at:?} instead of {goal:?}",
        );
    }

    #[test]
    fn an_ordinary_walk_across_the_station_never_takes_a_recovery_leg() {
        // The other half of the watchdog, and the half that costs something if
        // it is wrong: a crew member walking normally — slowly, round corners,
        // through two doorways — must never be mistaken for a stuck one and
        // sent back to the middle of the room they just left.
        let mut app = contained_walking_app();
        let from = crate::lab::ROOMS[crate::lab::REACTION_BAY].center();
        let to = crate::lab::ROOMS[crate::lab::ANALYSIS].center();
        let crew = walker(
            &mut app,
            Vec3::new(from.x, BODY_OFFSET, from.z),
            CrewRoute::to(Vec3::new(to.x, BODY_OFFSET, to.z)),
        );

        for _ in 0..600 {
            tick(&mut app, 0.05);
            if app.world().get::<CrewRoute>(crew).unwrap().phase == CrewPhase::Waiting {
                break;
            }
        }

        let route = app.world().get::<CrewRoute>(crew).unwrap();
        assert_eq!(route.phase, CrewPhase::Waiting, "never finished the walk");
        assert_eq!(
            route.unstick, 0,
            "an unobstructed walk was interrupted by {} recovery legs",
            route.unstick,
        );
    }

    #[test]
    fn a_crew_member_spawned_outside_the_station_walks_in_rather_than_through_the_wall() {
        // Crew spawn at the door, outside the floor: `spawn_crew_member` puts
        // them there and `NavGraph::locate` used to silently resolve that to
        // the nearest region, so the first leg ran from outside the building
        // straight to a portal deep inside it.
        let mut app = contained_walking_app();
        let start = Vec3::new(door_x(), BODY_OFFSET, spawn_z());
        let crew = walker(&mut app, start, CrewRoute::arrival(0.0));

        tick(&mut app, 0.01);

        // One step in they are still outside — that is legitimate, it is where
        // they spawn — but from the moment they are on the floor they stay on
        // it.
        let mut ever_arrived = false;
        for _ in 0..600 {
            tick(&mut app, 0.05);
            let Some(at) = app.world().get::<Transform>(crew) else {
                break;
            };
            if off_the_floor(&app, at.translation) < 0.01 {
                ever_arrived = true;
            } else if ever_arrived {
                panic!("stepped back off the floor at {:?}", at.translation);
            }
        }
        assert!(ever_arrived, "never made it onto the station floor at all");
    }

    #[test]
    fn a_leaving_crew_member_still_walks_out_of_the_door_and_despawns() {
        // The counterweight to containment, and the reason it is not applied
        // to a leaver's final leg. Walking off the walkable floor is how a
        // crew member exits; clamping that step would pin them at the
        // threshold, never reaching the waypoint and never despawning.
        let mut app = contained_walking_app();
        let from = crate::lab::ROOMS[crate::lab::REACTION_BAY].center();
        let crew = walker(
            &mut app,
            Vec3::new(from.x, BODY_OFFSET, from.z),
            CrewRoute::arrival(0.0),
        );
        tick(&mut app, 0.01);
        app.world_mut().get_mut::<CrewRoute>(crew).unwrap().leave();

        for _ in 0..600 {
            tick(&mut app, 0.05);
            if app.world().get::<Transform>(crew).is_none() {
                return;
            }
        }
        let at = app.world().get::<Transform>(crew).unwrap().translation;
        panic!("still on the station at {at:?} instead of having left");
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
            .init_resource::<CrewPosts>()
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
                CrewRoute::standing(),
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
        // As far toward Cargo as the floor goes. This fixture's departments sit
        // at z = 18, well beyond the five-room test lab, and a destination off
        // the walkable floor is now pulled onto it — otherwise the hauler walks
        // to the same spot and then stands there "still travelling" forever.
        // Cargo is at x = -5 and the casualty at x = 4, so this still tells the
        // two apart, which is the whole point of the test.
        let home = app
            .world()
            .resource::<crate::nav::NavGraph>()
            .standable_goal(Vec3::new(-5.0, 0.93, 18.0));
        assert!(
            heading_for.distance(home) < 0.01,
            "a hauler headed for {heading_for:?} instead of home to Cargo at {home:?}",
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

    // -----------------------------------------------------------------------
    // Errands
    // -----------------------------------------------------------------------

    /// Every errand resolution the app has produced, accumulated as it goes.
    ///
    /// `Messages` keeps two frames' worth, and walking an errand to its end
    /// takes hundreds of ticks — reading the buffer at the finish would find
    /// nothing at all.
    #[derive(Resource, Default)]
    struct ErrandLog(Vec<ErrandResolved>);

    fn record_errands(mut log: ResMut<ErrandLog>, mut resolved: MessageReader<ErrandResolved>) {
        log.0.extend(resolved.read().copied());
    }

    /// [`contained_walking_app`] plus the errand machinery.
    fn errand_app() -> App {
        let mut app = contained_walking_app();
        app.init_resource::<ErrandLog>()
            .add_message::<ErrandResolved>()
            .add_systems(Update, (run_errands, record_errands).chain());
        app
    }

    /// A body standing at `at` with no route of its own, ready to be sent.
    fn errand_runner(app: &mut App, at: Vec3) -> Entity {
        app.world_mut()
            .spawn((
                CrewMember {
                    name: "Tester".into(),
                    role: "Engineering".into(),
                },
                Transform::from_translation(at),
            ))
            .id()
    }

    fn send(app: &mut App, walker: Entity, goal: ErrandGoal) {
        let mut commands = app.world_mut().commands();
        send_on_errand(&mut commands, walker, goal);
        app.world_mut().flush();
    }

    fn errands(app: &App) -> &[ErrandResolved] {
        &app.world().resource::<ErrandLog>().0
    }

    /// Walks until the errand ends, or gives up after `ticks`.
    fn walk_errand(app: &mut App, walker: Entity, ticks: usize) {
        for _ in 0..ticks {
            tick(app, 0.05);
            if app.world().get::<Errand>(walker).is_none() {
                return;
            }
        }
    }

    fn in_reaction_bay() -> Vec3 {
        let at = crate::lab::ROOMS[crate::lab::REACTION_BAY].center();
        Vec3::new(at.x, BODY_OFFSET, at.z)
    }

    #[test]
    fn an_errand_runner_is_never_outside_the_walkable_floor() {
        // The same guarantee `a_crew_member_walking_across_the_station_is_
        // never_outside_the_walkable_floor` gives an ordinary route. An errand
        // is a *second* system writing `Transform`, so it needs its own proof:
        // sharing `nav::Trail` with `walk_route`'s containment is the intent,
        // not evidence.
        let mut app = errand_app();
        let start = in_reaction_bay();
        let walker = errand_runner(&mut app, start);
        let lobby = crate::lab::ROOMS[crate::lab::LOBBY].center();
        send(
            &mut app,
            walker,
            ErrandGoal::Point(Vec3::new(lobby.x, BODY_OFFSET, lobby.z)),
        );

        for step in 0..600 {
            tick(&mut app, 0.05);
            let Some(at) = app.world().get::<Transform>(walker) else {
                break;
            };
            let strayed = off_the_floor(&app, at.translation);
            assert!(
                strayed < 0.01,
                "step {step}: stood {strayed:.3}m off the walkable floor at {:?}",
                at.translation,
            );
            if app.world().get::<Errand>(walker).is_none() {
                break;
            }
        }
    }

    #[test]
    fn an_errand_ends_once_it_arrives_and_never_twice() {
        let mut app = errand_app();
        let start = in_reaction_bay();
        let walker = errand_runner(&mut app, start);
        let goal = start + Vec3::new(1.5, 0.0, 0.0);
        send(&mut app, walker, ErrandGoal::Point(goal));

        walk_errand(&mut app, walker, 400);

        assert!(
            app.world().get::<Errand>(walker).is_none(),
            "an arrived errand takes itself off, which is what makes 'once' structural",
        );
        assert_eq!(
            errands(&app).len(),
            1,
            "exactly one resolution: {:?}",
            errands(&app),
        );
        assert_eq!(errands(&app)[0].outcome, ErrandOutcome::Arrived);
        assert_eq!(errands(&app)[0].walker, walker);

        // And it stays ended — nothing re-fires now the component is gone.
        for _ in 0..20 {
            tick(&mut app, 0.05);
        }
        assert_eq!(errands(&app).len(), 1);
    }

    #[test]
    fn an_errand_follows_a_target_that_moves_rather_than_where_it_started() {
        // The reason a goal is re-sampled at all. A fixed point would send the
        // saboteur to where the beaker *was* when the visit expired, and have
        // them meddle with the empty counter it has since left.
        let mut app = errand_app();
        let start = in_reaction_bay();
        let walker = errand_runner(&mut app, start);
        let beaker = app
            .world_mut()
            .spawn(Transform::from_translation(start + Vec3::new(3.0, 0.0, 0.0)))
            .id();
        send(&mut app, walker, ErrandGoal::Target(beaker));

        tick(&mut app, 0.05);
        let moved = start + Vec3::new(0.0, 0.0, 2.0);
        app.world_mut().get_mut::<Transform>(beaker).unwrap().translation = moved;

        walk_errand(&mut app, walker, 400);

        assert_eq!(
            errands(&app).first().map(|ended| ended.outcome),
            Some(ErrandOutcome::Arrived),
            "the walk should have followed the beaker to where it went",
        );
        let at = app.world().get::<Transform>(walker).unwrap().translation;
        assert!(
            at.distance(moved) <= ERRAND_REACH,
            "ended {:.2}m from the target it was chasing",
            at.distance(moved),
        );
    }

    #[test]
    fn an_errand_whose_target_vanishes_ends_rather_than_walking_at_nothing() {
        // The player getting to the beaker first. Not a defensive edge case —
        // it is the counterplay, and the errand has to be able to report it.
        let mut app = errand_app();
        let start = in_reaction_bay();
        let walker = errand_runner(&mut app, start);
        let beaker = app
            .world_mut()
            .spawn(Transform::from_translation(start + Vec3::new(4.0, 0.0, 0.0)))
            .id();
        send(&mut app, walker, ErrandGoal::Target(beaker));

        tick(&mut app, 0.05);
        app.world_mut().entity_mut(beaker).despawn();

        walk_errand(&mut app, walker, 100);

        assert!(app.world().get::<Errand>(walker).is_none());
        assert_eq!(
            errands(&app).first().map(|ended| ended.outcome),
            Some(ErrandOutcome::Unreachable),
        );
    }

    #[test]
    fn a_goal_that_cannot_be_reached_never_walks_a_body_off_the_floor() {
        // The counterpart to `an_assailant_stops_when_the_nav_graph_has_no_
        // route`, and the half that matters most: whatever else an impossible
        // goal does, it must never be pursued through a wall.
        //
        // Note what it does *not* do — `NavGraph::path` falls back to the
        // nearest region for a goal inside no region at all, so a route to
        // the void exists and is walked, right up to the wall. Containment is
        // what stops it there.
        let mut app = errand_app();
        let start = in_reaction_bay();
        let walker = errand_runner(&mut app, start);
        let void = Vec3::new(-400.0, BODY_OFFSET, -400.0);
        send(&mut app, walker, ErrandGoal::Point(void));

        for _ in 0..200 {
            tick(&mut app, 0.05);
            let at = app.world().get::<Transform>(walker).unwrap().translation;
            let strayed = off_the_floor(&app, at);
            assert!(
                strayed < 0.01,
                "chasing an impossible goal walked them {strayed:.3}m off the floor",
            );
        }
    }

    #[test]
    fn an_errand_that_never_gets_there_is_written_off_rather_than_run_forever() {
        // The deadline, and the only thing that ends this case. The body above
        // is stopped at the wall by containment but still *wants* the goal:
        // its route is non-empty, it just never shrinks. Nothing in the
        // geometry will ever resolve that, so the clock has to.
        let mut app = errand_app();
        let start = in_reaction_bay();
        let walker = errand_runner(&mut app, start);
        send(
            &mut app,
            walker,
            ErrandGoal::Point(Vec3::new(-400.0, BODY_OFFSET, -400.0)),
        );

        // Past `ERRAND_DEADLINE_SECONDS`, at the tick rate the others use.
        walk_errand(&mut app, walker, (ERRAND_DEADLINE_SECONDS / 0.05) as usize + 20);

        assert!(
            app.world().get::<Errand>(walker).is_none(),
            "the errand should have been written off by now",
        );
        assert_eq!(
            errands(&app).first().map(|ended| ended.outcome),
            Some(ErrandOutcome::Unreachable),
            "and reported, so the caller can stop waiting on it",
        );
    }

    #[test]
    fn a_body_still_carrying_a_crew_route_is_never_walked_by_two_systems_at_once() {
        // `send_on_errand` removes the route, so this state is only reachable
        // by inserting `Errand` by hand. The query filter is what makes that
        // mistake inert rather than a body shuddering between two
        // destinations — `walk_route` and `run_errands` both write `Transform`.
        let mut app = errand_app();
        let start = in_reaction_bay();
        let walker = errand_runner(&mut app, start);
        let goal = start + Vec3::new(1.5, 0.0, 0.0);
        app.world_mut()
            .entity_mut(walker)
            .insert((CrewRoute::to(start), Errand::new(ErrandGoal::Point(goal))));

        for _ in 0..40 {
            tick(&mut app, 0.05);
        }

        assert!(
            app.world().get::<Errand>(walker).is_some(),
            "the errand should have been left entirely alone, not half-run",
        );
        assert!(errands(&app).is_empty());
    }

    #[test]
    fn an_errand_runner_reports_moving_so_the_walk_cycle_plays() {
        // `drive_crew_animation` picks the walk animation from this, and an
        // `Errand` *replaces* `CrewRoute` — so without it the saboteur crosses
        // the lab in a standing pose. The animation itself needs `CrewAssets`,
        // an `AnimationPlayer` and a live `AnimationGraph`, none of which exist
        // headless; this asserts the signal those read, which is the half that
        // was actually missing.
        let mut app = errand_app();
        let start = in_reaction_bay();
        let walker = errand_runner(&mut app, start);
        let lobby = crate::lab::ROOMS[crate::lab::LOBBY].center();
        send(
            &mut app,
            walker,
            ErrandGoal::Point(Vec3::new(lobby.x, BODY_OFFSET, lobby.z)),
        );

        tick(&mut app, 0.05);
        tick(&mut app, 0.05);

        assert!(
            app.world().get::<Errand>(walker).unwrap().is_moving(),
            "walking across the station has to read as walking",
        );
    }

    #[test]
    fn an_errand_runner_who_is_not_actually_walking_reports_still() {
        // The other direction, and the reason this is recorded per tick rather
        // than inferred from the component existing: a sedated body still
        // *has* an errand, and would otherwise march briskly on the spot.
        let mut app = errand_app();
        let start = in_reaction_bay();
        let walker = errand_runner(&mut app, start);
        let lobby = crate::lab::ROOMS[crate::lab::LOBBY].center();
        send(
            &mut app,
            walker,
            ErrandGoal::Point(Vec3::new(lobby.x, BODY_OFFSET, lobby.z)),
        );
        tick(&mut app, 0.05);
        tick(&mut app, 0.05);

        let mut blood = Bloodstream(chem_sim::Bloodstream::default());
        blood.0.add_status(StatusKind::Sedated, 20.0, 2.0);
        app.world_mut().entity_mut(walker).insert(blood);
        tick(&mut app, 0.05);

        assert!(
            !app.world().get::<Errand>(walker).unwrap().is_moving(),
            "a body that has been put down must not animate as walking",
        );
    }

    #[test]
    fn a_sedated_errand_runner_stops_where_they_are() {
        // The same rule `walk_route` already applies to an ordinary route: a
        // body you have put down does not keep walking. Putting the saboteur
        // to sleep on their way to the beaker has to actually stop them, or
        // sedation reads as cosmetic.
        let mut app = errand_app();
        let start = in_reaction_bay();
        let walker = errand_runner(&mut app, start);
        let lobby = crate::lab::ROOMS[crate::lab::LOBBY].center();
        send(
            &mut app,
            walker,
            ErrandGoal::Point(Vec3::new(lobby.x, BODY_OFFSET, lobby.z)),
        );

        for _ in 0..10 {
            tick(&mut app, 0.05);
        }
        let mut blood = Bloodstream(chem_sim::Bloodstream::default());
        blood.0.add_status(StatusKind::Sedated, 20.0, 2.0);
        app.world_mut().entity_mut(walker).insert(blood);
        let stopped_at = app.world().get::<Transform>(walker).unwrap().translation;

        for _ in 0..40 {
            tick(&mut app, 0.05);
        }

        assert_eq!(
            app.world().get::<Transform>(walker).unwrap().translation,
            stopped_at,
            "a sedated body must not keep walking its errand",
        );
        assert!(
            app.world().get::<Errand>(walker).is_some(),
            "and the errand is held, not cancelled — it resumes if they come round",
        );
    }
}
