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

use crate::containers::{Container, ContainerKind, HeldBy};
use crate::hazards::{HazardVisual, SmokeVisual};
use crate::lab::MachineScreen;
use crate::machines::Machine;
use crate::player::{LocalPlayer, PlayerCamera};
use crate::AppState;

/// How far a chemist can reach, in metres.
const REACH: f32 = 2.6;

pub struct InteractionPlugin;

impl Plugin for InteractionPlugin {
    fn build(&self, app: &mut App) {
        app.add_mapped_client_message::<InteractRequested>(Channel::Ordered)
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
                    panel_input,
                    update_focus,
                    request_interaction,
                    update_prompt,
                )
                    .chain()
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// Set when the player deliberately frees the cursor to leave the window.
#[derive(Resource, Default)]
struct CursorReleased(bool);

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
fn panel_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    cursor: Single<&mut CursorOptions>,
    mut released: ResMut<CursorReleased>,
    mut players: Query<(Entity, &mut InteractionMode), With<LocalPlayer>>,
    mut machines: Query<&mut Machine>,
    mut leaving: MessageWriter<LeaveMachineRequested>,
) {
    let escape = keys.just_pressed(KeyCode::Escape);
    let book = keys.just_pressed(KeyCode::KeyB);
    let mut panel_open = false;

    for (player, mut mode) in &mut players {
        // The book opens and closes on the same key from anywhere it can be
        // read: on the floor, or over an open machine panel. Looking a recipe
        // up mid-batch is the common case, and having to close the dispenser
        // to do it — losing the claim, and the beaker's place in the queue —
        // was the wrong answer.
        if book {
            *mode = match *mode {
                InteractionMode::Roaming => InteractionMode::ReadingBook(None),
                InteractionMode::UsingMachine(machine) => {
                    InteractionMode::ReadingBook(Some(machine))
                }
                InteractionMode::ReadingBook(from) => {
                    from.map_or(InteractionMode::Roaming, InteractionMode::UsingMachine)
                }
            };
        }

        if mode.is_roaming() {
            // fall through to the roaming cursor handling below
        } else if escape {
            // Escape steps back one screen rather than straight to the floor:
            // out of the book onto the panel it was opened over, and only then
            // out of the machine. Closing both at once would silently drop a
            // claim the player only meant to stop reading over.
            if let InteractionMode::ReadingBook(Some(machine)) = *mode {
                *mode = InteractionMode::UsingMachine(machine);
                panel_open = true;
                continue;
            }
            leave_machine(player, &mut mode, &mut machines, &mut leaving);
            released.0 = false;
            continue;
        } else {
            panel_open = true;
            continue;
        }

        if escape {
            released.0 = true;
        } else if mouse.just_pressed(MouseButton::Left) {
            released.0 = false;
        }
    }

    let want_free = panel_open || released.0;
    let mut cursor = cursor.into_inner();
    let is_free = cursor.grab_mode == CursorGrabMode::None;
    if want_free != is_free {
        cursor.visible = want_free;
        cursor.grab_mode = if want_free {
            CursorGrabMode::None
        } else {
            CursorGrabMode::Locked
        };
    }
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
}

impl InteractionMode {
    pub fn is_roaming(&self) -> bool {
        matches!(self, InteractionMode::Roaming)
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
            InteractionMode::Roaming => None,
        }
    }
}

/// Something the player can look at and use.
#[derive(Component)]
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
    screens: Query<(), With<MachineScreen>>,
    held: Query<(), With<HeldBy>>,
    smoke: Query<(), With<SmokeVisual>>,
    hazards: Query<(), With<HazardVisual>>,
) {
    for (camera_transform, camera) in &cameras {
        let Ok((mut focus, mode)) = players.get_mut(camera.chemist) else {
            continue;
        };

        if !mode.is_roaming() {
            if focus.target.is_some() {
                focus.target = None;
            }
            continue;
        }

        let ray = Ray3d::new(camera_transform.translation(), camera_transform.forward());
        // Machine screens sit a hair proud of their casing, so without this
        // filter every machine would be permanently blocked by its own screen.
        // A carried beaker rides in front of the camera and would do the same.
        // A smoke cloud is a sphere metres wide and would block the entire room
        // for as long as it hung there — and a hazard sphere is bigger still,
        // 4.5m centred on the dispenser for a rad leak, so it would take the
        // dispenser and half the hall with it. Everything else stays in the
        // cast, so walls and benches still occlude properly.
        let filter = |entity: Entity| {
            !screens.contains(entity)
                && !held.contains(entity)
                && !smoke.contains(entity)
                && !hazards.contains(entity)
        };
        let settings = MeshRayCastSettings::default().with_filter(&filter);

        focus.target = ray_cast
            .cast_ray(ray, &settings)
            .first()
            .filter(|(entity, hit)| hit.distance <= REACH && interactables.contains(*entity))
            .map(|(entity, _)| *entity);
    }
}

fn request_interaction(
    keys: Res<ButtonInput<KeyCode>>,
    players: Query<(Entity, &Focus, &InteractionMode), With<LocalPlayer>>,
    mut requests: MessageWriter<InteractRequested>,
) {
    if !keys.just_pressed(KeyCode::KeyE) {
        return;
    }
    for (_player, focus, mode) in &players {
        if !mode.is_roaming() {
            continue;
        }
        if let Some(target) = focus.target {
            requests.write(InteractRequested { target });
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
    ));
}

fn update_prompt(
    players: Query<(Entity, &Focus), With<LocalPlayer>>,
    interactables: Query<&Interactable>,
    machines: Query<&Machine>,
    held: Query<(&HeldBy, &Container)>,
    prompt: Single<&mut Text, With<InteractionPrompt>>,
) {
    let mut text = prompt.into_inner();
    let message = players
        .iter()
        .find_map(|(player, focus)| {
            let looking_at = focus.target.and_then(|target| {
                let label = &interactables.get(target).ok()?.label;
                // Occupied machines still show a prompt, just an unusable one,
                // so the other chemist's activity is visible rather than
                // mysterious.
                Some(match machines.get(target) {
                    Ok(machine) if !machine.available_to(player) => {
                        format!("{label} — in use")
                    }
                    _ => format!("[E]  {label}"),
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
                            (ContainerKind::Syringe, true) => {
                                // Only useful pointed at something worth drawing
                                // from, so only offered then.
                                focus.target.map(|_| "[F]  draw".to_string())
                            }
                            (ContainerKind::Syringe, false) => Some("[F]  inject".to_string()),
                            (_, true) => None,
                            (ContainerKind::Pill, false) => Some("[R]  swallow".to_string()),
                            (kind, false) => {
                                format!("[R]  drink from {}", kind.label().to_lowercase()).into()
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
