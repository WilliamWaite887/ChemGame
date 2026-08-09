//! Chemists: spawning, identity and the first-person controller.
//!
//! Authority sits with the server. Clients send [`MoveInput`]; the server moves
//! the body and replicates the result. Looking around is deliberately *not*
//! routed through the server — the camera is a separate entity driven by local
//! yaw and pitch, so turning your head never waits on a round trip. Only
//! walking does, which is the tolerable half of the trade.

use bevy::ecs::entity::MapEntities;
use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions};
use bevy_replicon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::interaction::{Focus, InteractionMode};
use crate::lab::{Solid, ROOM_HALF_X, ROOM_HALF_Z};
use crate::AppState;

pub const EYE_HEIGHT: f32 = 1.7;

const PLAYER_RADIUS: f32 = 0.35;
const WALK_SPEED: f32 = 4.2;
const MOUSE_SENSITIVITY: f32 = 0.0022;
/// Just under 90°, so looking straight up or down never flips the view.
const PITCH_LIMIT: f32 = 1.54;

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.add_mapped_server_message::<YouAreChemist>(Channel::Ordered)
            .add_client_message::<MoveInput>(Channel::Unreliable)
            .add_systems(OnEnter(AppState::Playing), (grab_cursor, spawn_host_chemist))
            .add_systems(
                Update,
                (
                    // Authority: runs on a dedicated server, a listen server,
                    // and in singleplayer — anywhere that is "not a remote
                    // client".
                    (spawn_joining_chemists, apply_move_input)
                        .run_if(in_state(ClientState::Disconnected)),
                    // Local presentation, runs everywhere.
                    (adopt_my_chemist, mouse_look, send_move_input, follow_chemist).chain(),
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// Anyone working the lab.
#[derive(Component, Serialize, Deserialize)]
pub struct Player;

/// Server-side link from a chemist back to the client driving them.
///
/// Not replicated: `ClientId` is not serialisable, and no client needs to know
/// another client's connection identity.
#[derive(Component)]
pub struct Chemist {
    pub client: ClientId,
}

/// The chemist this client controls.
#[derive(Component)]
pub struct LocalPlayer;

/// The camera. Not parented to the body, so head movement stays local.
#[derive(Component)]
pub struct PlayerCamera {
    pub chemist: Entity,
}

/// Yaw and pitch for the local view.
#[derive(Component, Default)]
pub struct Look {
    pub yaw: f32,
    pub pitch: f32,
}

/// Tells a client which chemist is theirs.
///
/// Sent rather than inferred: `ClientId` cannot cross the wire, and the entity
/// id means nothing to the client until replicon maps it.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct YouAreChemist {
    #[entities]
    pub chemist: Entity,
}

/// A client's movement intent for this frame.
#[derive(Message, Serialize, Deserialize, Clone)]
pub struct MoveInput {
    /// Forward/strafe, already normalised.
    pub direction: Vec2,
    /// Which way the body faces.
    pub yaw: f32,
}

/// Spawns the chemist for whoever is running the world: the singleplayer
/// chemist, or the host of a listen server.
fn spawn_host_chemist(
    mut commands: Commands,
    mut assign: MessageWriter<ToClients<YouAreChemist>>,
    client_state: Res<State<ClientState>>,
) {
    if *client_state.get() != ClientState::Disconnected {
        return;
    }
    let chemist = spawn_chemist(&mut commands, ClientId::Server, 0.0);
    assign.write(ToClients {
        targets: SendTargets::Single(ClientId::Server),
        message: YouAreChemist { chemist },
    });
}

/// Gives every newly joined client a chemist of their own.
///
/// Keyed on `AuthorizedClient`, not `ConnectedClient`. Replicon's default
/// auth waits for the client's protocol hash to match, and drops targeted
/// messages until it does — so spawning on connect means the chemist exists
/// but the client is never told which one is theirs.
///
/// The protocol check is worth keeping: reagent ids are positions in the data
/// files, so a client running different chemistry would silently mis-read
/// every solution.
fn spawn_joining_chemists(
    mut commands: Commands,
    joined: Query<Entity, Added<AuthorizedClient>>,
    existing: Query<(), With<Player>>,
    mut assign: MessageWriter<ToClients<YouAreChemist>>,
) {
    for client in &joined {
        // Offset each arrival so two chemists never spawn inside each other.
        let lane = existing.iter().count() as f32 * 0.9;
        let id = ClientId::Client(client);
        let chemist = spawn_chemist(&mut commands, id, lane);
        assign.write(ToClients {
            targets: SendTargets::Single(id),
            message: YouAreChemist { chemist },
        });
        info!("a second chemist joined the lab");
    }
}

fn spawn_chemist(commands: &mut Commands, client: ClientId, lane: f32) -> Entity {
    commands
        .spawn((
            Player,
            Chemist { client },
            Look::default(),
            InteractionMode::default(),
            Focus::default(),
            Transform::from_xyz(lane, EYE_HEIGHT, 2.6),
            Visibility::default(),
            Replicated,
        ))
        .id()
}

/// Attaches the camera once the server says which chemist is ours.
fn adopt_my_chemist(mut commands: Commands, mut assigned: MessageReader<YouAreChemist>) {
    for message in assigned.read() {
        commands.entity(message.chemist).insert(LocalPlayer);
        commands.spawn((
            Camera3d::default(),
            PlayerCamera {
                chemist: message.chemist,
            },
            Transform::from_xyz(0.0, EYE_HEIGHT, 2.6),
        ));
    }
}

fn grab_cursor(mut cursor: Single<&mut CursorOptions>) {
    cursor.visible = false;
    cursor.grab_mode = CursorGrabMode::Locked;
}

fn mouse_look(
    mut motion: MessageReader<MouseMotion>,
    cursor: Single<&CursorOptions>,
    mut players: Query<(&mut Look, &InteractionMode), With<LocalPlayer>>,
) {
    let delta: Vec2 = motion.read().map(|m| m.delta).sum();
    if cursor.grab_mode == CursorGrabMode::None || delta == Vec2::ZERO {
        return;
    }
    for (mut look, mode) in &mut players {
        if !mode.is_roaming() {
            continue;
        }
        look.yaw -= delta.x * MOUSE_SENSITIVITY;
        look.pitch = (look.pitch - delta.y * MOUSE_SENSITIVITY).clamp(-PITCH_LIMIT, PITCH_LIMIT);
    }
}

fn send_move_input(
    keys: Res<ButtonInput<KeyCode>>,
    players: Query<(&Look, &InteractionMode), With<LocalPlayer>>,
    mut outgoing: MessageWriter<MoveInput>,
) {
    let Ok((look, mode)) = players.single() else {
        return;
    };

    let mut direction = Vec2::ZERO;
    if mode.is_roaming() {
        if keys.pressed(KeyCode::KeyW) {
            direction.y -= 1.0;
        }
        if keys.pressed(KeyCode::KeyS) {
            direction.y += 1.0;
        }
        if keys.pressed(KeyCode::KeyA) {
            direction.x -= 1.0;
        }
        if keys.pressed(KeyCode::KeyD) {
            direction.x += 1.0;
        }
    }

    outgoing.write(MoveInput {
        direction: direction.normalize_or_zero(),
        yaw: look.yaw,
    });
}

/// Server-side movement. The only place a chemist's position changes.
fn apply_move_input(
    time: Res<Time>,
    mut inputs: MessageReader<FromClient<MoveInput>>,
    mut chemists: Query<(&mut Transform, &mut Look, &Chemist)>,
    solids: Query<(&Transform, &Solid), Without<Chemist>>,
) {
    for input in inputs.read() {
        let Some((mut transform, mut look, _)) = chemists
            .iter_mut()
            .find(|(_, _, chemist)| chemist.client == input.client_id)
        else {
            continue;
        };

        look.yaw = input.yaw;
        transform.rotation = Quat::from_rotation_y(input.yaw);
        if input.direction == Vec2::ZERO {
            continue;
        }

        let local = Vec3::new(input.direction.x, 0.0, input.direction.y);
        let step = Quat::from_rotation_y(input.yaw) * local;
        let mut position = transform.translation + step * WALK_SPEED * time.delta_secs();
        position.y = EYE_HEIGHT;

        for (solid_transform, solid) in &solids {
            position = push_out(position, solid_transform.translation, solid.half_extents);
        }
        let limit_x = ROOM_HALF_X - PLAYER_RADIUS;
        let limit_z = ROOM_HALF_Z - PLAYER_RADIUS;
        position.x = position.x.clamp(-limit_x, limit_x);
        position.z = position.z.clamp(-limit_z, limit_z);

        transform.translation = position;
    }
}

/// Keeps the camera on the chemist's shoulders, aimed by local yaw and pitch.
type LocalChemists<'w, 's> = Query<
    'w,
    's,
    (&'static Transform, &'static Look),
    (With<LocalPlayer>, Without<PlayerCamera>),
>;

fn follow_chemist(chemists: LocalChemists, mut cameras: Query<(&mut Transform, &PlayerCamera)>) {
    for (mut camera, target) in &mut cameras {
        let Ok((chemist, look)) = chemists.get(target.chemist) else {
            continue;
        };
        camera.translation = chemist.translation;
        camera.rotation = Quat::from_rotation_y(look.yaw) * Quat::from_rotation_x(look.pitch);
    }
}

/// Pushes `position` out of an axis-aligned box, along whichever horizontal
/// axis it has penetrated least.
///
/// Only the XZ plane matters: the floor is flat and everything in the lab is
/// tall enough to block at eye height, so vertical resolution would only add
/// ways to get stuck.
fn push_out(position: Vec3, center: Vec3, half_extents: Vec3) -> Vec3 {
    let dx = position.x - center.x;
    let dz = position.z - center.z;
    let overlap_x = half_extents.x + PLAYER_RADIUS - dx.abs();
    let overlap_z = half_extents.z + PLAYER_RADIUS - dz.abs();

    if overlap_x <= 0.0 || overlap_z <= 0.0 {
        return position;
    }

    let mut resolved = position;
    if overlap_x < overlap_z {
        resolved.x += overlap_x * dx.signum();
    } else {
        resolved.z += overlap_z * dz.signum();
    }
    resolved
}
