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
use chem_sim::StatusKind;
use serde::{Deserialize, Serialize};

use crate::body::{Bloodstream, Body};
use crate::interaction::{Focus, InteractionMode};
use crate::lab::{Solid, ROOM_HALF_X, ROOM_HALF_Z};
use crate::net::is_authority;
use crate::AppState;

pub const EYE_HEIGHT: f32 = 1.7;

const PLAYER_RADIUS: f32 = 0.35;
/// Unhurried, unmedicated walking pace.
const WALK_SPEED: f32 = 4.2;
const MOUSE_SENSITIVITY: f32 = 0.0022;
/// Just under 90°, so looking straight up or down never flips the view.
const PITCH_LIMIT: f32 = 1.54;

pub struct PlayerPlugin;

impl Plugin for PlayerPlugin {
    fn build(&self, app: &mut App) {
        app.add_mapped_server_message::<YouAreChemist>(Channel::Ordered)
            .add_client_message::<MoveInput>(Channel::Unreliable)
            .add_systems(
                OnEnter(AppState::Playing),
                (
                    grab_cursor,
                    load_chemist_assets,
                    spawn_host_chemist.run_if(is_authority),
                ),
            )
            .add_systems(
                Update,
                (
                    // Authority: runs on a dedicated server, a listen server,
                    // and in singleplayer — anywhere that is "not a remote
                    // client".
                    (spawn_joining_chemists, apply_move_input).run_if(is_authority),
                    // Local presentation, runs everywhere.
                    (
                        dress_chemists,
                        adopt_my_chemist,
                        hide_own_body,
                        mouse_look,
                        send_move_input,
                        follow_chemist,
                    )
                        .chain(),
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
fn spawn_host_chemist(mut commands: Commands, mut assign: MessageWriter<ToClients<YouAreChemist>>) {
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
            // A chemist is a person now. Both replicate, so each end can see
            // how the other is doing without asking.
            Body::default(),
            Bloodstream::default(),
            Transform::from_xyz(lane, EYE_HEIGHT, 2.6),
            Visibility::default(),
            Replicated,
        ))
        .id()
}

/// Attaches the camera once the server says which chemist is ours.
///
/// Also fits out the chemist with the components that drive a first-person
/// view. On the authority they are already there from [`spawn_chemist`], which
/// is what `insert_if_new` protects — re-inserting would wipe the yaw the
/// player is currently holding. On a joining client none of them exist: the
/// chemist arrived over the wire carrying only what is replicated, and
/// `Look`, `Focus` and `InteractionMode` are all deliberately local. Without
/// this a client can turn its head but cannot walk, aim or use anything,
/// because every system driving those filters on components it does not have.
fn adopt_my_chemist(mut commands: Commands, mut assigned: MessageReader<YouAreChemist>) {
    for message in assigned.read() {
        commands
            .entity(message.chemist)
            .insert(LocalPlayer)
            .insert_if_new((
                Look::default(),
                Focus::default(),
                InteractionMode::default(),
            ));
        commands.spawn((
            Camera3d::default(),
            PlayerCamera {
                chemist: message.chemist,
            },
            Transform::from_xyz(0.0, EYE_HEIGHT, 2.6),
        ));
    }
}

/// Meshes every chemist in the lab is drawn from.
#[derive(Resource)]
pub(crate) struct ChemistAssets {
    body: Handle<Mesh>,
    head: Handle<Mesh>,
    coat: Handle<StandardMaterial>,
    skin: Handle<StandardMaterial>,
}

pub(crate) fn load_chemist_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    commands.insert_resource(ChemistAssets {
        body: meshes.add(Capsule3d::new(0.3, 0.9)),
        head: meshes.add(Sphere::new(0.2)),
        coat: materials.add(StandardMaterial {
            // Lab coat white, so the other chemist reads instantly against the
            // grey room and the coloured crew uniforms.
            base_color: Color::srgb(0.88, 0.90, 0.93),
            perceptual_roughness: 0.8,
            ..default()
        }),
        skin: materials.add(StandardMaterial {
            base_color: Color::srgb(0.76, 0.62, 0.52),
            perceptual_roughness: 0.8,
            ..default()
        }),
    });
}

/// A body part, and the chemist it belongs to.
#[derive(Component)]
pub(crate) struct ChemistBody {
    pub(crate) chemist: Entity,
}

/// Gives every chemist something to look at.
///
/// Runs on both ends against `Added<Player>`, so it covers a chemist spawned
/// locally and one that arrived by replication without either being a special
/// case. The mesh is deliberately not replicated: it is presentation, and the
/// other end can build it from the marker alone.
pub(crate) fn dress_chemists(
    mut commands: Commands,
    assets: Option<Res<ChemistAssets>>,
    chemists: Query<Entity, Added<Player>>,
) {
    let Some(assets) = assets else {
        return;
    };
    for chemist in &chemists {
        // A replicated chemist arrives without `Visibility` — it is
        // presentation, so it is not on the wire — and a parent that has none
        // cannot propagate it to the parts below. Without this the other
        // chemist's body is built correctly and then never drawn.
        commands
            .entity(chemist)
            .insert_if_new(Visibility::default());

        // The chemist's own transform sits at eye height, so the body hangs
        // below it rather than centring on it.
        // `Visibility` is spelled out rather than left to `Mesh3d`'s required
        // components, because `hide_own_body` writes it: a part that only got
        // one implicitly is a part the hiding query cannot see.
        commands.spawn((
            Mesh3d(assets.body.clone()),
            MeshMaterial3d(assets.coat.clone()),
            Transform::from_xyz(0.0, -0.85, 0.0),
            Visibility::default(),
            ChemistBody { chemist },
            ChildOf(chemist),
        ));
        commands.spawn((
            Mesh3d(assets.head.clone()),
            MeshMaterial3d(assets.skin.clone()),
            Transform::from_xyz(0.0, -0.15, 0.0),
            Visibility::default(),
            ChemistBody { chemist },
            ChildOf(chemist),
        ));
    }
}

/// Hides your own body from your own camera.
///
/// Reconciled every frame rather than done once at adoption, because the two
/// halves arrive in either order: on a client the chemist can be replicated in
/// before the message naming it, or after.
fn hide_own_body(
    local: Query<Entity, With<LocalPlayer>>,
    mut parts: Query<(&ChemistBody, &mut Visibility)>,
) {
    let me = local.single().ok();
    for (part, mut visibility) in &mut parts {
        let wanted = if Some(part.chemist) == me {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
        if *visibility != wanted {
            *visibility = wanted;
        }
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

/// How fast this chemist is currently moving.
///
/// Read on the server inside [`apply_move_input`], never on the client. That is
/// not incidental: movement is server-authoritative with no prediction, so if
/// the client scaled its own speed the two would disagree and the player would
/// rubber-band every time they took a stimulant.
fn walk_speed(blood: &Bloodstream, body: &Body) -> f32 {
    if body.0.collapsed {
        return 0.0;
    }
    let hastened = blood.0.status(StatusKind::Hastened).intensity;
    let sluggish = blood.0.status(StatusKind::Sluggish).intensity;
    // Hastened and sluggish cancel rather than compete, so taking both is
    // simply a waste of two chemicals.
    let factor = (1.0 + 0.6 * hastened - 0.4 * sluggish).clamp(0.25, 2.0);
    WALK_SPEED * factor
}

/// Server-side movement. The only place a chemist's position changes.
fn apply_move_input(
    time: Res<Time>,
    mut inputs: MessageReader<FromClient<MoveInput>>,
    mut chemists: Query<(&mut Transform, &mut Look, &Chemist, &Body, &Bloodstream)>,
    solids: Query<(&Transform, &Solid), Without<Chemist>>,
) {
    for input in inputs.read() {
        let Some((mut transform, mut look, _, body, blood)) = chemists
            .iter_mut()
            .find(|(_, _, chemist, _, _)| chemist.client == input.client_id)
        else {
            continue;
        };

        look.yaw = input.yaw;
        transform.rotation = Quat::from_rotation_y(input.yaw);
        if input.direction == Vec2::ZERO {
            continue;
        }

        let speed = walk_speed(blood, body);
        if speed <= 0.0 {
            continue;
        }

        let local = Vec3::new(input.direction.x, 0.0, input.direction.y);
        let step = Quat::from_rotation_y(input.yaw) * local;
        let mut position = transform.translation + step * speed * time.delta_secs();
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
type LocalChemists<'w, 's> =
    Query<'w, 's, (&'static Transform, &'static Look), (With<LocalPlayer>, Without<PlayerCamera>)>;

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

#[cfg(test)]
mod tests {
    use super::*;

    /// A chemist as a joining client receives one: replication delivers the
    /// components on the wire and nothing else. `Look`, `Focus` and
    /// `InteractionMode` are all deliberately local, so they are absent here
    /// exactly as they are absent in a real client's world.
    fn replicated_chemist(app: &mut App) -> Entity {
        app.world_mut()
            .spawn((
                Player,
                Body::default(),
                Bloodstream::default(),
                Transform::from_xyz(0.0, EYE_HEIGHT, 2.6),
            ))
            .id()
    }

    fn adopt(app: &mut App, chemist: Entity) {
        app.world_mut().write_message(YouAreChemist { chemist });
        app.world_mut()
            .run_system_cached(adopt_my_chemist)
            .expect("adopt should run");
    }

    #[test]
    fn a_client_can_drive_the_chemist_the_server_gave_it() {
        // The bug this pins made co-op unplayable while looking like it worked:
        // the client connected, was assigned a chemist and could turn its head,
        // but every system that walks, aims or uses anything filters on
        // components that only ever existed on the server's copy. Nothing
        // errored — the queries simply matched nothing.
        let mut app = App::new();
        app.add_message::<YouAreChemist>();
        let chemist = replicated_chemist(&mut app);

        adopt(&mut app, chemist);

        let world = app.world();
        assert!(
            world.get::<LocalPlayer>(chemist).is_some(),
            "the assigned chemist must be marked as ours"
        );
        assert!(
            world.get::<Look>(chemist).is_some(),
            "without Look the client cannot aim or send a yaw"
        );
        assert!(
            world.get::<InteractionMode>(chemist).is_some(),
            "without InteractionMode no panel can ever open"
        );
        assert!(
            world.get::<Focus>(chemist).is_some(),
            "without Focus the crosshair never resolves a target"
        );
    }

    #[test]
    fn adopting_a_chemist_does_not_reset_where_they_are_looking() {
        // On a host the chemist is spawned locally and already has all three.
        // Re-inserting would snap the view back to centre the instant the
        // assignment message arrives.
        let mut app = App::new();
        app.add_message::<YouAreChemist>();
        let chemist = app
            .world_mut()
            .spawn((
                Player,
                Look {
                    yaw: 1.25,
                    pitch: -0.4,
                },
                InteractionMode::default(),
                Focus::default(),
            ))
            .id();

        adopt(&mut app, chemist);

        let look = app.world().get::<Look>(chemist).expect("Look survives");
        assert_eq!(look.yaw, 1.25);
        assert_eq!(look.pitch, -0.4);
    }

    #[test]
    fn every_chemist_gets_a_body_and_only_yours_is_hidden() {
        // Two chemists sharing a lab have to be able to see each other, and
        // neither wants to be looking at the inside of their own head.
        let mut app = App::new();
        app.add_plugins(AssetPlugin::default())
            .init_asset::<Mesh>()
            .init_asset::<StandardMaterial>()
            .add_message::<YouAreChemist>()
            // Driven as a real schedule rather than one-shot calls: these key
            // off `Added` and message readers, both of which are relative to a
            // system's own last run.
            .add_systems(Startup, load_chemist_assets)
            .add_systems(
                Update,
                (dress_chemists, adopt_my_chemist, hide_own_body).chain(),
            );

        let me = replicated_chemist(&mut app);
        let them = replicated_chemist(&mut app);
        app.world_mut().write_message(YouAreChemist { chemist: me });
        app.update();

        let mut parts = app.world_mut().query::<(&ChemistBody, &Visibility)>();
        let seen: Vec<(Entity, Visibility)> = parts
            .iter(app.world())
            .map(|(part, visibility)| (part.chemist, *visibility))
            .collect();

        assert_eq!(seen.len(), 4, "a body and a head each");
        // A replicated chemist has no `Visibility` of its own, and body parts
        // parented to one that lacks it are never drawn. Bevy reports this as
        // a B0004 warning at runtime rather than an error, so nothing else
        // would fail if it regressed.
        for chemist in [me, them] {
            assert!(
                app.world().get::<Visibility>(chemist).is_some(),
                "a chemist must be able to propagate visibility to their body"
            );
        }
        assert!(
            seen.iter()
                .filter(|(chemist, _)| *chemist == them)
                .all(|(_, visibility)| *visibility == Visibility::Inherited),
            "the other chemist must be visible, or co-op is a lab full of ghosts"
        );
        assert!(
            seen.iter()
                .filter(|(chemist, _)| *chemist == me)
                .all(|(_, visibility)| *visibility == Visibility::Hidden),
            "your own body would sit over your own camera"
        );
    }
}
