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
use crate::lab::{self, Solid};
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
                    (
                        spawn_joining_chemists,
                        despawn_leaving_chemists,
                        (receive_move_input, apply_move_input).chain(),
                    )
                        .run_if(is_authority),
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

/// The most recent movement command a chemist's client has sent.
///
/// Server-only bookkeeping, not replicated: it exists so [`apply_move_input`]
/// can move every chemist once per server frame regardless of how many — or
/// how few — [`MoveInput`] messages actually arrived that frame. `MoveInput`
/// travels over an unreliable channel, so on a lossy connection some frames
/// get none at all; without something to fall back on, a dropped packet froze
/// that chemist in place until the next one landed, which is what made the
/// joining chemist visibly stall and lag behind the host. Latching instead of
/// integrating means a lost packet just means "keep doing what you were
/// told last", exactly like dead reckoning in any other server-authoritative
/// game.
#[derive(Component, Default)]
struct MoveIntent {
    direction: Vec2,
    yaw: f32,
}

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

/// Removes a chemist whose client has disconnected.
///
/// `ConnectedClient` is despawned by the networking backend the instant a
/// connection drops (see its own docs), so this is the mirror image of
/// [`spawn_joining_chemists`] rather than a poll for anything. Nothing else
/// ever removes a `Player` entity — without this, a chemist who quit stayed
/// in the lab forever, and the one who stayed behind went on seeing a
/// colleague who had gone home.
fn despawn_leaving_chemists(
    mut commands: Commands,
    mut gone: RemovedComponents<ConnectedClient>,
    chemists: Query<(Entity, &Chemist)>,
) {
    for client in gone.read() {
        let id = ClientId::Client(client);
        for (entity, chemist) in &chemists {
            if chemist.client == id {
                commands.entity(entity).despawn();
            }
        }
    }
}

fn spawn_chemist(commands: &mut Commands, client: ClientId, lane: f32) -> Entity {
    commands
        .spawn((
            Player,
            Chemist { client },
            MoveIntent::default(),
            Look::default(),
            InteractionMode::default(),
            Focus::default(),
            // A chemist is a person now. Both replicate, so each end can see
            // how the other is doing without asking.
            Body::default(),
            Bloodstream::default(),
            Transform::from_xyz(
                lab::SPAWN_SPOT.x + lane,
                EYE_HEIGHT,
                lab::SPAWN_SPOT.z,
            ),
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

/// Lab coat white, so the other chemist reads instantly against the grey
/// room and the coloured crew uniforms. Shared with [`animate_chemist_body`]
/// in `fx`, which blends a chemical-status tint on top of this rather than
/// whatever colour a mutated material happens to hold — see
/// [`ChemistBody::base_color`].
pub(crate) const COAT_COLOR: Color = Color::srgb(0.88, 0.90, 0.93);
pub(crate) const SKIN_COLOR: Color = Color::srgb(0.76, 0.62, 0.52);

/// Meshes every chemist in the lab is drawn from.
///
/// Materials are deliberately *not* here — M12 moved them into a fresh
/// [`StandardMaterial`] per chemist, spawned in [`dress_chemists`]. A single
/// shared `Handle<StandardMaterial>` for every chemist's coat would mean
/// tinting one chemist drunk-red would tint every chemist sharing that
/// handle, since a material handle is a reference to one asset, not a
/// per-entity copy.
#[derive(Resource)]
pub(crate) struct ChemistAssets {
    body: Handle<Mesh>,
    head: Handle<Mesh>,
}

pub(crate) fn load_chemist_assets(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>) {
    commands.insert_resource(ChemistAssets {
        body: meshes.add(Capsule3d::new(0.3, 0.9)),
        head: meshes.add(Sphere::new(0.2)),
    });
}

/// A body part, and the chemist it belongs to.
///
/// `rest` is the part's un-animated local offset and `base_color` its
/// un-tinted material colour — both needed so `fx::animate_chemist_body` can
/// compute this frame's wobble/tint fresh from a fixed reference point
/// instead of drifting by accumulating onto whatever the last frame left.
#[derive(Component)]
pub(crate) struct ChemistBody {
    pub(crate) chemist: Entity,
    pub(crate) rest: Vec3,
    pub(crate) base_color: Color,
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
    mut materials: ResMut<Assets<StandardMaterial>>,
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
        let body_rest = Vec3::new(0.0, -0.85, 0.0);
        commands.spawn((
            Mesh3d(assets.body.clone()),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: COAT_COLOR,
                perceptual_roughness: 0.8,
                ..default()
            })),
            Transform::from_translation(body_rest),
            Visibility::default(),
            ChemistBody {
                chemist,
                rest: body_rest,
                base_color: COAT_COLOR,
            },
            ChildOf(chemist),
        ));
        let head_rest = Vec3::new(0.0, -0.15, 0.0);
        commands.spawn((
            Mesh3d(assets.head.clone()),
            MeshMaterial3d(materials.add(StandardMaterial {
                base_color: SKIN_COLOR,
                perceptual_roughness: 0.8,
                ..default()
            })),
            Transform::from_translation(head_rest),
            Visibility::default(),
            ChemistBody {
                chemist,
                rest: head_rest,
                base_color: SKIN_COLOR,
            },
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

/// How long an unchanged movement command may go unsent.
///
/// `MoveInput` rides [`Channel::Unreliable`], which makes "only send it when it
/// changes" a trap on its own: the single packet carrying "I let go of W" can
/// be dropped, and the server would go on walking a chemist who has stopped,
/// with nothing to correct it. Re-asserting the current command at this cadence
/// bounds how long any such loss can last, while still cutting a genuinely idle
/// client — nobody moving, nobody looking around, someone reading the reference
/// book — from a packet every frame to ten a second.
const MOVE_INPUT_RESEND_SECONDS: f32 = 0.1;

fn send_move_input(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    players: Query<(&Look, &InteractionMode), With<LocalPlayer>>,
    mut outgoing: MessageWriter<MoveInput>,
    mut last: Local<Option<(Vec2, f32)>>,
    mut since_send: Local<f32>,
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
    let direction = direction.normalize_or_zero();

    // Any change goes out the same frame, so this costs no responsiveness —
    // note that includes mouse-look, since `yaw` is part of the command.
    *since_send += time.delta_secs();
    let command = (direction, look.yaw);
    if *last == Some(command) && *since_send < MOVE_INPUT_RESEND_SECONDS {
        return;
    }
    *last = Some(command);
    *since_send = 0.0;

    outgoing.write(MoveInput {
        direction,
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

/// Latches the newest movement command per chemist. Does not move anyone —
/// see [`apply_move_input`] for why that is a separate step.
fn receive_move_input(
    mut inputs: MessageReader<FromClient<MoveInput>>,
    mut chemists: Query<(&Chemist, &mut MoveIntent)>,
) {
    for input in inputs.read() {
        let Some((_, mut intent)) = chemists
            .iter_mut()
            .find(|(chemist, _)| chemist.client == input.client_id)
        else {
            continue;
        };
        intent.direction = input.direction;
        intent.yaw = input.yaw;
    }
}

/// Server-side movement. The only place a chemist's position changes.
///
/// Moves every chemist once per server frame from their latched
/// [`MoveIntent`], never once per message received. `MoveInput` rides an
/// unreliable channel, so message arrival is bursty: a frame that happens to
/// receive two queued packets must not walk the chemist twice as far, and a
/// frame that receives none — the ordinary cost of a dropped unreliable
/// packet — must not freeze them either. Both used to happen, because the
/// old version multiplied `time.delta_secs()` by however many `MoveInput`
/// messages arrived that frame instead of by frames elapsed: on a lossy
/// connection that reads as one chemist visibly lagging behind the other.
fn apply_move_input(
    time: Res<Time>,
    mut chemists: Query<
        (&mut Transform, &mut Look, &MoveIntent, &Body, &Bloodstream),
        With<Chemist>,
    >,
    solids: Query<(&Transform, &Solid), Without<Chemist>>,
    areas: Res<lab::WalkableAreas>,
) {
    for (mut transform, mut look, intent, body, blood) in &mut chemists {
        // Compared before writing. `Transform` is replicated and `Look` drives
        // the camera, so an unconditional write here re-broadcast a chemist
        // standing perfectly still at frame rate and re-ran transform
        // propagation for their whole body hierarchy every frame. This cannot
        // simply move below the idle guard instead — turning on the spot has
        // `direction == ZERO` and must still rotate.
        let facing = Quat::from_rotation_y(intent.yaw);
        if transform.rotation != facing {
            transform.rotation = facing;
        }
        if look.yaw != intent.yaw {
            look.yaw = intent.yaw;
        }
        if intent.direction == Vec2::ZERO {
            continue;
        }

        let speed = walk_speed(blood, body);
        if speed <= 0.0 {
            continue;
        }

        let local = Vec3::new(intent.direction.x, 0.0, intent.direction.y);
        let step = Quat::from_rotation_y(intent.yaw) * local;
        let mut position = transform.translation + step * speed * time.delta_secs();
        position.y = EYE_HEIGHT;

        for (solid_transform, solid) in &solids {
            position = push_out(position, solid_transform.translation, solid.half_extents);
        }
        // Backstop behind the walls, for corners and the seams where two wall
        // runs meet. It used to be a clamp to one rectangle, which was only ever
        // correct while the lab was a single room: against the five-room suite
        // it would have snapped anyone who stepped through a doorway straight
        // back into the hall.
        position = areas.contain(position, PLAYER_RADIUS);

        transform.translation = position;
    }
}

/// Keeps the camera on the chemist's shoulders, aimed by local yaw and pitch.
pub(crate) type LocalChemists<'w, 's> =
    Query<'w, 's, (&'static Transform, &'static Look), (With<LocalPlayer>, Without<PlayerCamera>)>;

/// `pub(crate)` since M12, so `fx`'s camera-effect systems can order
/// themselves `.after(follow_chemist)` from a separate plugin — Bevy resolves
/// `.after(some_fn)` by the system's type, not by which `add_systems` call it
/// came from, so this is enough to guarantee the shake/tint/sway layer always
/// composes on top of this frame's base camera placement rather than racing
/// it.
pub(crate) fn follow_chemist(
    chemists: LocalChemists,
    mut cameras: Query<(&mut Transform, &PlayerCamera)>,
) {
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

    // -- movement: once per frame, not once per packet -------------------

    fn move_app() -> App {
        let mut app = App::new();
        app.init_resource::<Time>()
            // Seeded from the const floor plan rather than left empty, so these
            // walk against the same containment the real game uses. Empty areas
            // are a no-op, which would quietly stop testing it at all.
            .insert_resource(lab::WalkableAreas::from_floor_plan())
            .add_message::<FromClient<MoveInput>>()
            .add_systems(Update, (receive_move_input, apply_move_input).chain());
        app
    }

    fn moving_chemist(app: &mut App, client: ClientId) -> Entity {
        app.world_mut()
            .spawn((
                Chemist { client },
                MoveIntent::default(),
                Look::default(),
                Body::default(),
                Bloodstream::default(),
                Transform::from_xyz(lab::SPAWN_SPOT.x, EYE_HEIGHT, lab::SPAWN_SPOT.z),
            ))
            .id()
    }

    fn send_input(app: &mut App, client: ClientId, direction: Vec2) {
        app.world_mut().write_message(FromClient {
            client_id: client,
            message: MoveInput { direction, yaw: 0.0 },
        });
    }

    fn tick(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(seconds));
        app.update();
    }

    #[test]
    fn a_dropped_move_packet_does_not_stall_the_chemist() {
        // The bug this pins: movement used to be applied once per received
        // `MoveInput` message rather than once per server frame. `MoveInput`
        // rides an unreliable channel, so a frame that receives none — the
        // ordinary cost of one dropped packet on a lossy link — used to
        // freeze that chemist in place until the next packet landed, which
        // is what made the joining chemist visibly stall and lag the host.
        let mut app = move_app();
        let client_entity = app.world_mut().spawn_empty().id();
        let client = ClientId::Client(client_entity);
        let chemist = moving_chemist(&mut app, client);

        send_input(&mut app, client, Vec2::new(0.0, -1.0));
        tick(&mut app, 0.1);
        let after_first = app.world().get::<Transform>(chemist).unwrap().translation;
        assert_ne!(after_first.z, 0.0, "the first frame should move the chemist");

        // No new `MoveInput` this frame — exactly what a dropped packet
        // looks like from the server's side.
        tick(&mut app, 0.1);
        let after_second = app.world().get::<Transform>(chemist).unwrap().translation;
        assert_ne!(
            after_second, after_first,
            "a missing packet must not freeze the chemist in place"
        );
    }

    #[test]
    fn a_burst_of_queued_packets_does_not_double_the_step() {
        // The other half of the same bug: two messages queued in one server
        // frame — the ordinary result of jitter delivering a small backlog
        // at once — used to move the chemist twice, once per message,
        // instead of once for the frame elapsed.
        let mut solo = move_app();
        let solo_client = ClientId::Client(solo.world_mut().spawn_empty().id());
        let solo_chemist = moving_chemist(&mut solo, solo_client);
        send_input(&mut solo, solo_client, Vec2::new(0.0, -1.0));
        tick(&mut solo, 0.1);
        let single_step = solo.world().get::<Transform>(solo_chemist).unwrap().translation;

        let mut bursty = move_app();
        let bursty_client = ClientId::Client(bursty.world_mut().spawn_empty().id());
        let bursty_chemist = moving_chemist(&mut bursty, bursty_client);
        send_input(&mut bursty, bursty_client, Vec2::new(0.0, -1.0));
        send_input(&mut bursty, bursty_client, Vec2::new(0.0, -1.0));
        tick(&mut bursty, 0.1);
        let bursty_step = bursty
            .world()
            .get::<Transform>(bursty_chemist)
            .unwrap()
            .translation;

        assert_eq!(
            bursty_step, single_step,
            "a frame that receives two queued messages must move the chemist \
             once, not twice"
        );
    }

    // -- disconnect --------------------------------------------------------

    #[test]
    fn a_chemist_disappears_when_their_client_disconnects() {
        // `ConnectedClient` is despawned by the networking backend the
        // instant a connection drops. Nothing else ever removes a `Player`
        // entity, so without this a chemist who quit stayed in the lab
        // forever — the one who stayed behind went on seeing a colleague
        // who had gone home.
        let mut app = App::new();
        app.add_systems(Update, despawn_leaving_chemists);

        let client_entity = app.world_mut().spawn(ConnectedClient { max_size: 0 }).id();
        let client = ClientId::Client(client_entity);
        let chemist = app.world_mut().spawn((Player, Chemist { client })).id();

        app.world_mut().despawn(client_entity);
        app.update();

        assert!(
            app.world().get_entity(chemist).is_err(),
            "the chemist must leave when their client disconnects"
        );
    }
}
