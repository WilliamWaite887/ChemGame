//! Looking at things and using them.
//!
//! Interaction never mutates machine state directly. It raycasts, works out
//! what is under the crosshair, and emits [`InteractRequested`]. Systems that
//! own the state apply it. That indirection is what lets co-op replicate
//! actions later instead of rewriting every panel.

use bevy::ecs::entity::MapEntities;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions};
use bevy_replicon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::chem_world::PuddleVisual;
use crate::containers::{Container, ContainerKind, HeldBy};
use crate::crew::{EvacuateCrewRequested, NeedsMedicalEvacuation};
use crate::hazards::{HazardVisual, SmokeVisual};
use crate::machines::Machine;
use crate::player::{LocalPlayer, PlayerCamera};
use crate::AppState;

/// How far a chemist can reach, in metres.
pub(crate) const REACH: f32 = 2.6;

/// A replicated target names its gameplay root, not the triangle the player's
/// camera hit. A person is rooted around their waist and a machine/container
/// can be rooted at its centre, so the authority allows this much extra when
/// checking an entity target. Floor points are exact ray hits and receive only
/// a much smaller allowance below.
const TARGET_CENTRE_TOLERANCE: f32 = 0.75;
const POINT_NETWORK_TOLERANCE: f32 = 0.15;

/// Server-side counterpart to the camera's [`REACH`] filter.
///
/// Client focus is presentation, not authority: a forged request can name an
/// entity the sender never looked at. This accepts the small difference
/// between a mesh hit and its gameplay root while still rejecting remote
/// interaction. Non-finite coordinates are rejected here as well so they can
/// never make distance comparisons fail open.
///
/// `reach` is the caller's own distance, not always [`REACH`] — a held item
/// can reach further (`ContainerKind::reach`); a bare-hands caller passes
/// [`REACH`] itself.
pub(crate) fn authority_target_in_reach(actor: Vec3, target_root: Vec3, reach: f32) -> bool {
    finite_within(actor, target_root, reach + TARGET_CENTRE_TOLERANCE)
}

/// Exact world hit used by a floor pour. Unlike an entity root, this should be
/// almost exactly where the client ray ended.
pub(crate) fn authority_point_in_reach(actor: Vec3, point: Vec3, reach: f32) -> bool {
    finite_within(actor, point, reach + POINT_NETWORK_TOLERANCE)
}

fn finite_within(actor: Vec3, endpoint: Vec3, distance: f32) -> bool {
    if !actor.is_finite() || !endpoint.is_finite() {
        return false;
    }
    (endpoint - actor).length_squared() <= distance * distance
}

/// Whether a lab [`crate::lab::Solid`] occludes a hand action.
///
/// "Authority" names where this was born, not a restriction on who may call
/// it: it is pure geometry with no authority semantics, and `speech` uses the
/// same test client-side to decide whether a speech bubble is behind a wall.
/// A wall blocking a reach and a wall blocking a line of sight are one
/// question, and two copies of the answer would drift.
///
/// Rendering raycasts do not exist on a headless authority. The lab already
/// describes walls, benches and machine cases as axis-aligned boxes for
/// movement, so a segment/AABB test supplies the same important guarantee
/// without a second physics or mesh-query architecture. Contact only at the
/// last sliver of the segment is allowed: that is the supporting bench/floor
/// surface the camera actually hit, rather than a wall between player and hit.
pub(crate) fn authority_segment_blocked(
    actor: Vec3,
    endpoint: Vec3,
    solid_center: Vec3,
    solid_half_extents: Vec3,
) -> bool {
    if !actor.is_finite()
        || !endpoint.is_finite()
        || !solid_center.is_finite()
        || !solid_half_extents.is_finite()
    {
        return true;
    }

    let half = solid_half_extents.abs();
    let min = solid_center - half;
    let max = solid_center + half;
    let direction = endpoint - actor;
    let mut enter = 0.0_f32;
    let mut exit = 1.0_f32;

    for axis in 0..3 {
        let start = actor[axis];
        let delta = direction[axis];
        if delta.abs() <= f32::EPSILON {
            if start < min[axis] || start > max[axis] {
                return false;
            }
            continue;
        }

        let inverse = delta.recip();
        let mut near = (min[axis] - start) * inverse;
        let mut far = (max[axis] - start) * inverse;
        if near > far {
            std::mem::swap(&mut near, &mut far);
        }
        enter = enter.max(near);
        exit = exit.min(far);
        if enter > exit {
            return false;
        }
    }

    // Ignore a supporting surface touched only at the endpoint and numerical
    // contact at the actor's own origin. A real intervening wall occupies a
    // measurable portion of the segment between those two margins.
    enter < 0.985 && exit > 0.015
}

/// Every body within a cone from `actor` toward `aim_direction`, out to
/// `reach`, filtered by `half_angle_deg` and the same per-target occlusion
/// [`authority_segment_blocked`] already performs for a single endpoint — the
/// many-target counterpart of [`authority_target_in_reach`] for an aimed
/// area weapon (a cone-spray held item) rather than a single crosshair
/// target.
///
/// `aim_direction` must already be normalized; a zero vector matches nothing,
/// since `dot` against it can never clear `cos_half_angle`.
pub(crate) fn bodies_in_cone(
    actor: Vec3,
    aim_direction: Vec3,
    reach: f32,
    half_angle_deg: f32,
    candidates: impl Iterator<Item = (Entity, Vec3)>,
    solids: &Query<(&Transform, &crate::lab::Solid)>,
) -> Vec<Entity> {
    let cos_half_angle = half_angle_deg.to_radians().cos();
    candidates
        .filter(|(_, position)| {
            let offset = *position - actor;
            let distance = offset.length();
            distance > 0.0
                && distance <= reach
                && offset.normalize().dot(aim_direction) >= cos_half_angle
        })
        .filter(|(_, position)| {
            !solids.iter().any(|(transform, solid)| {
                authority_segment_blocked(
                    actor,
                    *position,
                    transform.translation,
                    solid.half_extents,
                )
            })
        })
        .map(|(entity, _)| entity)
        .collect()
}

pub struct InteractionPlugin;

impl Plugin for InteractionPlugin {
    fn build(&self, app: &mut App) {
        app.add_mapped_client_message::<InteractRequested>(Channel::Ordered)
            .add_mapped_client_message::<EvacuateCrewRequested>(Channel::Ordered)
            .add_client_message::<LeaveMachineRequested>(Channel::Ordered)
            .add_mapped_server_message::<MachineOpened>(Channel::Ordered)
            .init_resource::<CursorReleased>()
            .add_systems(OnEnter(AppState::Playing), spawn_hud)
            .add_systems(
                Update,
                (
                    // Before `panel_input`, so a panel the server has just
                    // granted is open for this frame's Escape to close.
                    apply_machine_opened,
                    // Not gated on `not_paused` — it is what *reads* Escape,
                    // so it has to keep running to let the player back out.
                    panel_input,
                    // Chained: `request_interaction` acts on the target
                    // `update_focus` picked this frame, and `update_prompt`
                    // describes it. Wrapping them in a tuple for the pause
                    // gate drops the ordering the outer `.chain()` used to
                    // give them unless it is restored here.
                    (update_focus, request_interaction, update_prompt)
                        .chain()
                        .run_if(crate::settings::not_paused),
                )
                    .chain()
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// Set when the player deliberately frees the cursor to leave the window.
#[derive(Resource, Default)]
pub(crate) struct CursorReleased(bool);

/// Closes whatever panel `player` has open and releases their claim on it.
///
/// Shared by the Escape key and the panel's own Close button so the two can
/// never drift apart and strand a machine marked in-use forever.
///
/// Closes the panel locally *and* tells the server. The local half keeps the
/// UI instant; the message is what actually frees the machine, because on a
/// client the claim lives in the server's copy of `Machine` and clearing the
/// replicated one here is only a prediction. Without it a client walking away
/// from the dispenser would lock it against the other chemist for the rest of
/// the shift.
pub fn leave_machine(
    player: Entity,
    mode: &mut InteractionMode,
    machines: &mut Query<&mut Machine>,
    leaving: &mut MessageWriter<LeaveMachineRequested>,
) {
    if mode.claimed_machine().is_some() {
        leaving.write(LeaveMachineRequested);
    }
    release_claim(player, mode, machines);
}

/// The local half of leaving: close the panel, let go of the machine.
///
/// Separate from [`leave_machine`] because the server also has to do this to
/// somebody — a collapsed chemist drops their claim whether they asked to or
/// not — and there it must *not* send a client message. A message has no
/// sender but the connection it came in on, so the server writing one on a
/// remote chemist's behalf would release the host's claim instead of theirs.
pub fn release_claim(
    player: Entity,
    mode: &mut InteractionMode,
    machines: &mut Query<&mut Machine>,
) {
    if let Some(machine) = mode.claimed_machine() {
        if let Ok(mut machine) = machines.get_mut(machine) {
            if machine.in_use_by == Some(player) {
                machine.in_use_by = None;
            }
        }
    }
    *mode = InteractionMode::Roaming;
}

/// A chemist has closed their panel and is done with the machine.
///
/// Carries no machine id: the server already knows which one it granted, and
/// a client naming one could release the other chemist's claim.
#[derive(Message, Serialize, Deserialize, Clone)]
pub struct LeaveMachineRequested;

/// The server granting a machine to the client that asked for it.
///
/// The claim is the server's to give, so the panel opens on its say-so rather
/// than optimistically. Replicon re-emits a message addressed to
/// `ClientId::Server` locally, so the host and singleplayer take this exact
/// path too — there is no second code path to keep in step.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct MachineOpened {
    #[entities]
    pub machine: Entity,
}

/// Opens the panel the server has just granted.
fn apply_machine_opened(
    mut opened: MessageReader<MachineOpened>,
    mut players: Query<&mut InteractionMode, With<LocalPlayer>>,
) {
    for message in opened.read() {
        for mut mode in &mut players {
            *mode = InteractionMode::UsingMachine(message.machine);
        }
    }
}

/// Owns every path that changes cursor grab, in one system on purpose.
///
/// Escape has to mean "close the panel" when one is open and "let go of the
/// cursor" when not. Split across two systems those race within a frame — the
/// panel closes and the same keypress immediately frees the cursor.
#[allow(clippy::too_many_arguments)]
/// Whether Escape is currently forbidden from closing the pause overlay.
///
/// A real evacuation (`ending::Ending::evacuated`) is not something Escape
/// gets to dismiss. Every *other* ending — a merely resolved, still-playable
/// arc — keeps its existing Escape-to-dismiss behaviour untouched; this is
/// deliberately the only new gate here. Pure, so the one keypress this
/// project cannot afford to get wrong is testable without a window, a cursor
/// or a full `App`.
pub(crate) fn escape_blocked_by_evacuation(
    screen: crate::settings::PauseScreen,
    finished: Option<&crate::ending::FinishedArc>,
) -> bool {
    screen == crate::settings::PauseScreen::Ending
        && finished
            .and_then(|finished| finished.showing())
            .is_some_and(|ending| ending.evacuated)
}

pub(crate) fn panel_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    cursor: Single<&mut CursorOptions>,
    settings: Res<crate::settings::Settings>,
    mut paused: ResMut<crate::settings::Paused>,
    mut screen: ResMut<crate::settings::PauseScreen>,
    rebinding: Res<crate::settings::Rebinding>,
    finished: Option<Res<crate::ending::FinishedArc>>,
    mut released: ResMut<CursorReleased>,
    mut players: Query<(Entity, &mut InteractionMode), With<LocalPlayer>>,
    mut machines: Query<&mut Machine>,
    mut leaving: MessageWriter<LeaveMachineRequested>,
) {
    let escape = keys.just_pressed(KeyCode::Escape);
    let book = keys.just_pressed(settings.bindings.book);
    let social = keys.just_pressed(settings.bindings.social);
    let blocks_escape = escape_blocked_by_evacuation(*screen, finished.as_deref());

    // The pause menu owns Escape whenever it is up, and nothing else here
    // should run underneath it — a keypress meant to close the menu must not
    // also toggle the book behind it.
    if paused.0 {
        // A single Escape steps back one screen, matching what the on-screen
        // "Back" button already does on Settings/Controls — it used to close
        // the whole overlay outright from any sub-screen instead. Suppressed
        // while a binding row is armed: that Escape belongs to
        // `settings::capture_rebind_key` alone, cancelling the capture, and
        // must not *also* step the pause screen back in the same frame.
        if escape && !blocks_escape && !rebinding.is_armed() {
            match crate::settings::escape_steps_pause_screen_back_to(*screen) {
                Some(next) => *screen = next,
                None => paused.0 = false,
            }
        }
        free_the_cursor(cursor, true);
        return;
    }

    let mut panel_open = false;

    for (player, mut mode) in &mut players {
        // The book opens and closes on the same key from anywhere it can be
        // read: on the floor, or over an open machine panel. Looking a recipe
        // up mid-batch is the common case, and having to close the dispenser
        // to do it — losing the claim, and the beaker's place in the queue —
        // was the wrong answer.
        // Not while the label field is open: `b` is a letter there, and the
        // reference book is the one keybind in the game that does not already
        // gate itself on `is_roaming`. Every other action key — move, look,
        // sprint, drop, drink, apply, use — is disabled for free by
        // `Labelling` simply not being `Roaming`.
        if social && !matches!(*mode, InteractionMode::Labelling(_)) {
            *mode = mode.toggled_social();
        } else if book && !matches!(*mode, InteractionMode::Labelling(_)) {
            *mode = mode.toggled_book();
        }

        if mode.is_roaming() {
            // fall through to the roaming cursor handling below
        } else if escape {
            // Escape steps back one screen rather than straight to the floor:
            // out of the book onto the panel it was opened over, and only then
            // out of the machine. Closing both at once would silently drop a
            // claim the player only meant to stop reading over.
            match *mode {
                InteractionMode::Inspecting { machine, .. } => {
                    *mode = machine
                        .filter(|entity| {
                            machines
                                .get(*entity)
                                .is_ok_and(|machine| machine.available_to(player))
                        })
                        .map_or(InteractionMode::Roaming, InteractionMode::UsingMachine);
                    panel_open = !mode.is_roaming();
                    continue;
                }
                InteractionMode::ReadingBook(Some(machine)) => {
                    *mode = InteractionMode::UsingMachine(machine);
                    panel_open = true;
                    continue;
                }
                InteractionMode::Social { .. } | InteractionMode::OrderDirectory { .. } => {
                    *mode = mode.toggled_social();
                    panel_open = !mode.is_roaming();
                    continue;
                }
                _ => {}
            }
            leave_machine(player, &mut mode, &mut machines, &mut leaving);
            released.0 = false;
            continue;
        } else {
            panel_open = true;
            continue;
        }

        // Escape from the floor opens the pause menu. It used to merely free
        // the cursor, which looked identical to the game having stopped
        // responding — there was no way out of the lab, and no settings to
        // reach, from anywhere inside it.
        if escape {
            paused.0 = true;
        } else if mouse.just_pressed(MouseButton::Left) {
            released.0 = false;
        }
    }

    free_the_cursor(cursor, panel_open || paused.0 || released.0);
}

/// Grabs or frees the mouse, only when that is actually a change.
///
/// Shared by the paused early-return above and the ordinary path below it, so
/// the two can never disagree about who owns the cursor.
fn free_the_cursor(cursor: Single<&mut CursorOptions>, want_free: bool) {
    let mut cursor = cursor.into_inner();
    if want_free == (cursor.grab_mode == CursorGrabMode::None) {
        return;
    }
    cursor.visible = want_free;
    cursor.grab_mode = if want_free {
        CursorGrabMode::None
    } else {
        CursorGrabMode::Locked
    };
}

/// What a player is currently doing. Per-player rather than a global state
/// resource, because in co-op one chemist can be at a panel while the other
/// walks around.
#[derive(Component, Default, Debug, Clone, Copy, PartialEq)]
pub enum InteractionMode {
    #[default]
    Roaming,
    UsingMachine(Entity),
    /// Reading the reference book. Modelled as a mode rather than a separate
    /// flag so it inherits the cursor and camera handling machines already
    /// have — otherwise the view keeps turning while you read.
    ///
    /// Carries the machine it was opened over, if any. A chemist checking a
    /// recipe halfway through a batch has not walked away from the dispenser,
    /// so the claim is deliberately kept while they read and the book closes
    /// back onto the panel they came from.
    ReadingBook(Option<Entity>),
    /// Reviewing crew relationships, remembered conversations, and the
    /// department shops. Tab can lay this screen over either the lab, a
    /// machine, or the reference book; both the machine claim and the exact
    /// screen underneath are restored when it closes.
    Social {
        machine: Option<Entity>,
        return_to_book: bool,
    },
    /// Writing on the container they are holding — see [`crate::labels`].
    ///
    /// A mode rather than a flag for exactly the reason `ReadingBook` is one:
    /// it inherits the released cursor and frozen camera, and — because every
    /// gameplay keybind in the game gates on [`InteractionMode::is_roaming`] —
    /// typing a label cannot also walk, drop, drink, inject or use anything.
    Labelling(Entity),
    Inspecting {
        item: Entity,
        machine: Option<Entity>,
    },
    OrderConversation(Entity, u64),
    SecurityConversation,
    OrderDirectory {
        machine: Option<Entity>,
        return_to_book: bool,
    },
}

impl InteractionMode {
    pub fn is_roaming(&self) -> bool {
        matches!(self, InteractionMode::Roaming)
    }

    /// Where the book key takes this chemist next.
    ///
    /// Split out of [`panel_input`] so the round trip — floor to book and
    /// back, machine to book and back to the *same* machine — is testable
    /// without a window, a cursor and a raycast to hang them off.
    pub fn toggled_book(self) -> Self {
        match self {
            InteractionMode::Roaming => InteractionMode::ReadingBook(None),
            InteractionMode::UsingMachine(machine) => InteractionMode::ReadingBook(Some(machine)),
            InteractionMode::ReadingBook(from) => {
                from.map_or(InteractionMode::Roaming, InteractionMode::UsingMachine)
            }
            InteractionMode::Social { .. } => self,
            // Unreachable in practice — `panel_input` does not offer the book
            // key while the label field is open, because a `b` belongs in the
            // word being typed. Answering "no change" rather than panicking
            // keeps that a presentation decision rather than an invariant.
            InteractionMode::Inspecting { .. } => self,
            InteractionMode::SecurityConversation | InteractionMode::OrderConversation(..) => self,
            InteractionMode::OrderDirectory { .. } => self,
            InteractionMode::Labelling(container) => InteractionMode::Labelling(container),
        }
    }

    /// Where Tab takes this chemist next. Like [`Self::toggled_book`], this
    /// keeps any machine claim alive and restores the exact screen underneath.
    pub fn toggled_social(self) -> Self {
        match self {
            InteractionMode::Roaming => InteractionMode::Social {
                machine: None,
                return_to_book: false,
            },
            InteractionMode::UsingMachine(machine) => InteractionMode::Social {
                machine: Some(machine),
                return_to_book: false,
            },
            InteractionMode::ReadingBook(machine) => InteractionMode::Social {
                machine,
                return_to_book: true,
            },
            InteractionMode::Social {
                machine,
                return_to_book,
            } => {
                if return_to_book {
                    InteractionMode::ReadingBook(machine)
                } else {
                    machine.map_or(InteractionMode::Roaming, InteractionMode::UsingMachine)
                }
            }
            InteractionMode::Inspecting { .. } => self,
            InteractionMode::SecurityConversation | InteractionMode::OrderConversation(..) => self,
            InteractionMode::OrderDirectory {
                machine,
                return_to_book,
            } => InteractionMode::Social {
                machine,
                return_to_book,
            },
            InteractionMode::Labelling(container) => InteractionMode::Labelling(container),
        }
    }

    /// The machine this chemist is holding, whether they are working it or
    /// reading over the top of it.
    ///
    /// Every release path goes through this rather than matching
    /// `UsingMachine` directly: a book opened at a machine still owns the
    /// claim, and a path that forgot would strand it in-use for the rest of
    /// the shift.
    pub fn claimed_machine(&self) -> Option<Entity> {
        match *self {
            InteractionMode::UsingMachine(machine) => Some(machine),
            InteractionMode::ReadingBook(machine) => machine,
            InteractionMode::Inspecting { machine, .. } => machine,
            InteractionMode::Social { machine, .. }
            | InteractionMode::OrderDirectory { machine, .. } => machine,
            // A container, not a machine, and nothing to release.
            InteractionMode::Roaming
            | InteractionMode::Labelling(_)
            | InteractionMode::SecurityConversation
            | InteractionMode::OrderConversation(..) => None,
        }
    }
}

/// Something the player can look at and use.
#[derive(Component, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Interactable {
    pub label: String,
}

impl Interactable {
    pub fn new(label: impl Into<String>) -> Self {
        Interactable {
            label: label.into(),
        }
    }
}

/// What this player's crosshair is currently over.
#[derive(Component, Default)]
pub struct Focus {
    pub target: Option<Entity>,
    /// World-space point hit by the camera ray, even when the mesh itself is
    /// not interactable.  Chemical application uses this to pour a held
    /// container onto the floor without turning every floor brush into an
    /// `Interactable`.
    pub point: Option<Vec3>,
}

/// A chemist pressed use on something.
///
/// Deliberately carries no player field: the server takes the sender's
/// identity from the connection. A message that named its own actor would let
/// a client act as the other chemist.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct InteractRequested {
    #[entities]
    pub target: Entity,
}

/// Casts a ray from each player's camera and records what it hits.
///
/// Only while roaming. A player inside a machine panel or the reference book
/// has a released cursor and a frozen camera, so the cast can only ever return
/// the answer they already have — and every reader of [`Focus`] is either
/// roaming-gated already ([`request_interaction`], `body::request_apply_held`)
/// or presentational. Skipping it drops a triangle-level scene raycast per
/// camera per frame for the whole time a panel is open, and clearing the target
/// on the way in stops a stale "[E] …" prompt sitting behind the panel.
#[allow(clippy::too_many_arguments)]
fn update_focus(
    mut ray_cast: MeshRayCast,
    cameras: Query<(&GlobalTransform, &PlayerCamera)>,
    mut players: Query<(&mut Focus, &InteractionMode)>,
    interactables: Query<(), With<Interactable>>,
    parents: Query<&ChildOf>,
    held: Query<(), With<HeldBy>>,
    holding: Query<(&HeldBy, &Container)>,
    smoke: Query<(), With<SmokeVisual>>,
    hazards: Query<(), With<HazardVisual>>,
    puddles: Query<(), With<PuddleVisual>>,
) {
    for (camera_transform, camera) in &cameras {
        let Ok((mut focus, mode)) = players.get_mut(camera.chemist) else {
            continue;
        };

        if !mode.is_roaming() {
            if focus.target.is_some() || focus.point.is_some() {
                focus.target = None;
                focus.point = None;
            }
            continue;
        }

        // A longer-reach held item (a confrontation item) widens this ray's
        // own cutoff, not just the server's authority check. Client focus is
        // presentation, not the gate itself, but a hit past ordinary REACH
        // would otherwise be discarded before Focus is ever set, so F would
        // silently do nothing no matter how far the item actually reaches.
        let max_reach = holding
            .iter()
            .find(|(holder, _)| holder.0 == camera.chemist)
            .map(|(_, container)| container.kind.reach())
            .unwrap_or(REACH);

        let ray = Ray3d::new(camera_transform.translation(), camera_transform.forward());
        // A carried beaker rides in front of the camera and would otherwise
        // block whatever the player is deliberately aiming at behind it.
        // A smoke cloud is a sphere metres wide and would block the entire room
        // for as long as it hung there — and a hazard sphere is bigger still,
        // 4.5m centred on the dispenser for a rad leak, so it would take the
        // dispenser and half the hall with it. Everything else stays in the
        // cast, so walls and benches still occlude properly.
        let filter = |entity: Entity| {
            !has_ancestor(entity, &held, &parents)
                && !smoke.contains(entity)
                && !hazards.contains(entity)
                && !puddles.contains(entity)
        };
        let settings = MeshRayCastSettings::default().with_filter(&filter);

        let hit = ray_cast
            .cast_ray(ray, &settings)
            .first()
            .filter(|(_, hit)| hit.distance <= max_reach);
        focus.point = hit.map(|(_, hit)| hit.point);
        focus.target =
            hit.and_then(|(entity, _)| interactable_ancestor(*entity, &interactables, &parents));
    }
}

/// Whether `entity` or one of its gameplay ancestors matches `query`.
///
/// A carried container's glass mesh lives on its root, but the visible liquid
/// is a child mesh. Filtering only the root still lets that liquid intercept
/// the camera ray, making the held beaker block the person or container the
/// player is trying to apply it to. Walking the same bounded hierarchy used
/// for interactable lookup keeps the complete held object out of the cast.
fn has_ancestor<F: bevy::ecs::query::QueryFilter>(
    mut entity: Entity,
    query: &Query<(), F>,
    parents: &Query<&ChildOf>,
) -> bool {
    for _ in 0..64 {
        if query.contains(entity) {
            return true;
        }
        let Ok(parent) = parents.get(entity) else {
            return false;
        };
        entity = parent.parent();
    }
    false
}

/// Detailed GLB meshes are children of their gameplay entity. Preserve normal
/// occlusion by raycasting the real triangle first, then walk only that hit's
/// hierarchy until the nearest usable ancestor is found.
fn interactable_ancestor(
    mut entity: Entity,
    interactables: &Query<(), With<Interactable>>,
    parents: &Query<&ChildOf>,
) -> Option<Entity> {
    for _ in 0..64 {
        if interactables.contains(entity) {
            return Some(entity);
        }
        entity = parents.get(entity).ok()?.parent();
    }
    None
}

fn request_interaction(
    keys: Res<ButtonInput<KeyCode>>,
    controls: Res<crate::settings::Settings>,
    players: Query<(Entity, &Focus, &InteractionMode), With<LocalPlayer>>,
    evacuations: Query<(), With<NeedsMedicalEvacuation>>,
    mut requests: MessageWriter<InteractRequested>,
    mut evacuation_requests: MessageWriter<EvacuateCrewRequested>,
    pending: Query<&crate::order_intake::AwaitingConversation>,
    mut conversations: MessageWriter<crate::order_intake::OpenOrderConversation>,
) {
    if !keys.just_pressed(controls.bindings.interact) {
        return;
    }
    for (_player, focus, mode) in &players {
        if !mode.is_roaming() {
            continue;
        }
        if let Some(target) = focus.target {
            if let Ok(waiting) = pending.get(target) {
                if waiting.arrived {
                    conversations.write(crate::order_intake::OpenOrderConversation {
                        target,
                        id: waiting.id,
                    });
                }
            } else if evacuations.contains(target) {
                evacuation_requests.write(EvacuateCrewRequested { target });
            } else {
                requests.write(InteractRequested { target });
            }
        }
    }
}

#[derive(Component)]
struct InteractionPrompt;

fn spawn_hud(mut commands: Commands) {
    // Crosshair.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            left: percent(50),
            top: percent(50),
            width: px(4),
            height: px(4),
            margin: UiRect::px(-2.0, 0.0, -2.0, 0.0),
            border_radius: BorderRadius::all(px(2)),
            ..default()
        },
        BackgroundColor(Color::srgba(1.0, 1.0, 1.0, 0.75)),
        crate::until_we_leave_the_lab(),
    ));

    // Interaction prompt.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            bottom: percent(16),
            width: percent(100),
            justify_content: JustifyContent::Center,
            ..default()
        },
        children![(
            Text::new(""),
            TextFont::from_font_size(19.0),
            TextColor(Color::srgb(0.94, 0.95, 0.98)),
            InteractionPrompt,
        )],
        crate::until_we_leave_the_lab(),
    ));
}

#[allow(clippy::too_many_arguments)]
fn update_prompt(
    players: Query<(Entity, &Focus), With<LocalPlayer>>,
    interactables: Query<&Interactable>,
    machines: Query<&Machine>,
    held: Query<(&HeldBy, &Container)>,
    containers: Query<(), With<Container>>,
    bodies: Query<(), With<crate::body::Body>>,
    crew: Query<&crate::crew::CrewMember>,
    written: Query<&crate::labels::Label>,
    prompt: Single<&mut Text, With<InteractionPrompt>>,
    pending: Query<&crate::order_intake::AwaitingConversation>,
) {
    let mut text = prompt.into_inner();
    let message = players
        .iter()
        .find_map(|(player, focus)| {
            let empty_handed = !held.iter().any(|(holder, _)| holder.0 == player);
            let looking_at = focus.target.and_then(|target| {
                let label = &interactables.get(target).ok()?.label;
                if let Ok(waiting) = pending.get(target) {
                    let name = crew.get(target).map_or("Crew member", |c| c.name.as_str());
                    return Some(if waiting.arrived {
                        format!("[E]  speak to {name}")
                    } else {
                        format!("{name} — heading to the window")
                    });
                }
                // Occupied machines still show a prompt, just an unusable one,
                // so the other chemist's activity is visible rather than
                // mysterious.
                Some(match machines.get(target) {
                    Ok(machine) if !machine.available_to(player) => {
                        format!("{label} — in use")
                    }
                    // An empty hand aimed at a person is the one press nothing
                    // else in the game claims, and `speech::handle_talk` takes
                    // it. Saying so here is what makes the mechanic findable:
                    // the label alone ("Dr. Vance — Medical") reads as an
                    // affordance the player has no reason to think does
                    // anything without a beaker in hand.
                    _ => match crew.get(target) {
                        Ok(member) if empty_handed => {
                            format!("[E]  speak to {}", member.name)
                        }
                        // Reading a label reveals its claim. Only laboratory
                        // equipment identifies the actual contents.
                        _ => match written.get(target) {
                            Ok(written) => format!("[E]  “{}”", written.0),
                            Err(_) => format!("[E]  {label}"),
                        },
                    },
                })
            });

            // What is in your hand is worth saying too. Taking a chemical is a
            // keypress with no button and no panel behind it, so without this
            // the whole mechanic is invisible.
            let carrying =
                held.iter()
                    .find(|(holder, _)| holder.0 == player)
                    .and_then(|(_, container)| {
                        let empty = container.solution.total_volume().is_zero();
                        match (container.kind, empty) {
                            (ContainerKind::PhPaper, _) => focus
                                .target
                                .filter(|target| containers.contains(*target))
                                .map(|_| "[F]  test approximate pH".to_string()),
                            (ContainerKind::Syringe | ContainerKind::SyringeGun, true) => focus
                                .target
                                .filter(|target| containers.contains(*target))
                                .map(|_| "[F]  draw".to_string()),
                            (ContainerKind::Syringe | ContainerKind::SyringeGun, false) => {
                                Some("[F]  inject".to_string())
                            }
                            (_, true) => None,
                            (ContainerKind::Pill, false) => Some("[R]  swallow".to_string()),
                            (ContainerKind::Patch, false) => Some("[R]  apply patch".to_string()),
                            (
                                ContainerKind::SprayBottle
                                | ContainerKind::PressureSprayer
                                | ContainerKind::WaterGun,
                                false,
                            ) => Some("[F]  spray chemical".to_string()),
                            (ContainerKind::SmokeProjector, false) => {
                                Some("[R]  project smoke payload".to_string())
                            }
                            (kind, false) if kind.charge_fuse().is_some() => {
                                Some("[R]  arm and place charge".to_string())
                            }
                            (kind, false) => {
                                let application = match focus.target {
                                    Some(target) if containers.contains(target) => {
                                        "[F]  pour 10u".to_string()
                                    }
                                    Some(target) if bodies.contains(target) => {
                                        "[F]  splash 10u".to_string()
                                    }
                                    Some(_) => String::new(),
                                    None if focus.point.is_some() => {
                                        "[F]  pour 10u on floor".to_string()
                                    }
                                    None => String::new(),
                                };
                                let drink =
                                    format!("[R]  drink from {}", kind.label().to_lowercase());
                                Some(if application.is_empty() {
                                    drink
                                } else {
                                    format!("{application}    {drink}")
                                })
                            }
                        }
                    });

            match (looking_at, carrying) {
                (Some(target), Some(hand)) => Some(format!("{target}      {hand}")),
                (Some(target), None) => Some(target),
                (None, hand) => hand,
            }
        })
        .unwrap_or_default();

    if text.0 != message {
        text.0 = message;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::SystemState;

    #[test]
    fn authority_reach_rejects_remote_and_non_finite_requests() {
        let actor = Vec3::new(0.0, 1.7, 0.0);
        assert!(authority_target_in_reach(
            actor,
            Vec3::new(3.0, 1.0, 0.0),
            REACH
        ));
        assert!(!authority_target_in_reach(
            actor,
            Vec3::new(8.0, 1.0, 0.0),
            REACH
        ));
        assert!(authority_point_in_reach(
            actor,
            Vec3::new(1.0, 0.0, 0.0),
            REACH
        ));
        assert!(!authority_point_in_reach(
            actor,
            Vec3::new(4.0, 0.0, 0.0),
            REACH
        ));
        assert!(!authority_point_in_reach(
            actor,
            Vec3::new(f32::NAN, 0.0, 0.0),
            REACH
        ));
    }

    #[test]
    fn a_wider_reach_actually_changes_the_outcome() {
        // Proves the parameterization changes the answer, not just compiles:
        // a target 5m out is beyond ordinary hand reach but within a Syringe
        // Gun's own 6.0m.
        let actor = Vec3::new(0.0, 1.7, 0.0);
        let target = Vec3::new(5.0, 1.7, 0.0);
        assert!(!authority_target_in_reach(actor, target, REACH));
        assert!(authority_target_in_reach(actor, target, 6.0));
    }

    #[test]
    fn bodies_in_cone_filters_by_angle_reach_and_occlusion() {
        let mut world = World::new();
        let actor = Vec3::ZERO;
        let aim = Vec3::new(1.0, 0.0, 0.0);

        // Dead ahead, within reach and angle: hits.
        let ahead = world.spawn_empty().id();
        // Off to the side, outside the 20 degree half-angle: misses.
        let off_angle = world.spawn_empty().id();
        // Dead ahead but past the reach: misses.
        let too_far = world.spawn_empty().id();
        // Dead ahead, in reach and angle, but behind a wall: misses.
        let occluded = world.spawn_empty().id();
        world.spawn((
            Transform::from_xyz(2.0, 0.0, 0.0),
            crate::lab::Solid {
                half_extents: Vec3::new(0.1, 1.0, 1.0),
            },
        ));

        let mut state = SystemState::<Query<(&Transform, &crate::lab::Solid)>>::new(&mut world);
        let solids = state.get(&world).unwrap();

        let candidates = [
            (ahead, Vec3::new(1.0, 0.0, 0.0)),
            (off_angle, Vec3::new(1.0, 0.0, 2.0)),
            (too_far, Vec3::new(10.0, 0.0, 0.0)),
            (occluded, Vec3::new(3.0, 0.0, 0.0)),
        ];

        let hits = bodies_in_cone(actor, aim, 4.5, 20.0, candidates.into_iter(), &solids);

        assert_eq!(hits, vec![ahead]);
    }

    #[test]
    fn bodies_in_cone_hits_every_body_it_catches() {
        let mut world = World::new();
        let mut state = SystemState::<Query<(&Transform, &crate::lab::Solid)>>::new(&mut world);
        let solids = state.get(&world).unwrap();

        let a = Entity::from_raw_u32(1).unwrap();
        let b = Entity::from_raw_u32(2).unwrap();
        let candidates = [
            (a, Vec3::new(1.0, 0.0, 0.1)),
            (b, Vec3::new(1.0, 0.0, -0.1)),
        ];

        let mut hits = bodies_in_cone(
            Vec3::ZERO,
            Vec3::new(1.0, 0.0, 0.0),
            4.5,
            20.0,
            candidates.into_iter(),
            &solids,
        );
        hits.sort();
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(hits, expected);
    }

    #[test]
    fn authority_line_of_sight_ignores_endpoint_contact_but_blocks_a_wall() {
        let actor = Vec3::new(0.0, 1.7, 0.0);
        let endpoint = Vec3::new(2.0, 0.0, 0.0);
        assert!(authority_segment_blocked(
            actor,
            endpoint,
            Vec3::new(1.0, 0.85, 0.0),
            Vec3::new(0.1, 1.0, 1.0),
        ));
        assert!(!authority_segment_blocked(
            actor,
            endpoint,
            Vec3::new(2.0, -0.1, 0.0),
            Vec3::new(1.0, 0.1, 1.0),
        ));
    }

    #[test]
    fn use_routes_an_evacuation_prompt_to_the_dedicated_request() {
        let mut app = App::new();
        app.init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<crate::settings::Settings>()
            .add_message::<InteractRequested>()
            .add_message::<EvacuateCrewRequested>()
            .add_message::<crate::order_intake::OpenOrderConversation>()
            .add_systems(Update, request_interaction);
        let target = app.world_mut().spawn(NeedsMedicalEvacuation).id();
        app.world_mut().spawn((
            LocalPlayer,
            Focus {
                target: Some(target),
                point: None,
            },
            InteractionMode::Roaming,
        ));
        let interact = app
            .world()
            .resource::<crate::settings::Settings>()
            .bindings
            .interact;
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(interact);

        app.update();

        assert!(app
            .world_mut()
            .resource_mut::<Messages<InteractRequested>>()
            .drain()
            .next()
            .is_none());
        let requests: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<EvacuateCrewRequested>>()
            .drain()
            .collect();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].target, target);
    }

    #[test]
    fn a_scene_mesh_resolves_to_its_interactable_machine_ancestor() {
        let mut world = World::new();
        let machine = world.spawn(Interactable::new("machine")).id();
        let scene_root = world.spawn(ChildOf(machine)).id();
        let mesh = world.spawn(ChildOf(scene_root)).id();
        let wall = world.spawn_empty().id();

        let mut state =
            SystemState::<(Query<(), With<Interactable>>, Query<&ChildOf>)>::new(&mut world);
        let (interactables, parents) = state.get(&world).unwrap();

        assert_eq!(
            interactable_ancestor(mesh, &interactables, &parents),
            Some(machine)
        );
        assert_eq!(interactable_ancestor(wall, &interactables, &parents), None);
    }

    #[test]
    fn every_mesh_under_a_held_container_is_excluded_from_focus() {
        let mut world = World::new();
        let player = world.spawn_empty().id();
        let container = world.spawn(HeldBy(player)).id();
        let liquid = world.spawn(ChildOf(container)).id();
        let nested_mesh = world.spawn(ChildOf(liquid)).id();
        let floor = world.spawn_empty().id();

        let mut state = SystemState::<(Query<(), With<HeldBy>>, Query<&ChildOf>)>::new(&mut world);
        let (held, parents) = state.get(&world).unwrap();

        assert!(has_ancestor(container, &held, &parents));
        assert!(has_ancestor(liquid, &held, &parents));
        assert!(has_ancestor(nested_mesh, &held, &parents));
        assert!(!has_ancestor(floor, &held, &parents));
    }

    #[test]
    fn the_book_key_returns_a_chemist_to_wherever_they_opened_it() {
        let dispenser = Entity::from_raw_u32(7).unwrap();

        // From the floor, and back to it.
        let reading = InteractionMode::Roaming.toggled_book();
        assert_eq!(reading, InteractionMode::ReadingBook(None));
        assert_eq!(reading.toggled_book(), InteractionMode::Roaming);

        // From a machine, and back to that same machine rather than to the
        // floor — the claim was never released, so dropping the player out to
        // roaming here would leave them standing at a dispenser the server
        // still believes they are working.
        let at_machine = InteractionMode::UsingMachine(dispenser);
        let reading = at_machine.toggled_book();
        assert_eq!(reading, InteractionMode::ReadingBook(Some(dispenser)));
        assert_eq!(reading.toggled_book(), at_machine);
    }

    #[test]
    fn a_book_open_over_a_machine_still_counts_as_holding_it() {
        // Every release path keys off this, so a `None` here would strand the
        // machine in use for the rest of the shift.
        let dispenser = Entity::from_raw_u32(7).unwrap();
        assert_eq!(
            InteractionMode::ReadingBook(Some(dispenser)).claimed_machine(),
            Some(dispenser)
        );
        assert_eq!(InteractionMode::ReadingBook(None).claimed_machine(), None);
        assert_eq!(InteractionMode::Roaming.claimed_machine(), None);
        assert_eq!(
            InteractionMode::UsingMachine(dispenser).claimed_machine(),
            Some(dispenser)
        );
    }

    #[test]
    fn the_social_screen_returns_to_the_exact_screen_underneath() {
        let machine = Entity::from_raw_u32(11).unwrap();

        let from_floor = InteractionMode::Roaming.toggled_social();
        assert_eq!(from_floor.claimed_machine(), None);
        assert_eq!(from_floor.toggled_social(), InteractionMode::Roaming);

        let from_machine = InteractionMode::UsingMachine(machine).toggled_social();
        assert_eq!(from_machine.claimed_machine(), Some(machine));
        assert_eq!(
            from_machine.toggled_social(),
            InteractionMode::UsingMachine(machine)
        );

        let from_book = InteractionMode::ReadingBook(Some(machine)).toggled_social();
        assert_eq!(from_book.claimed_machine(), Some(machine));
        assert_eq!(
            from_book.toggled_social(),
            InteractionMode::ReadingBook(Some(machine))
        );
    }
}
