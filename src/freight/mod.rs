//! Cargo's freight line: a conveyor that carries parcels in from off-map,
//! past a sorter, and down chutes labelled for the departments they feed.
//!
//! The station's first *moving* scenery. Everything decorative before this was
//! static geometry bolted to a wall, which is why an empty Cargo hall read as a
//! room nobody worked in.
//!
//! # It is scenery, and only scenery
//!
//! Nothing here is replicated and nothing is gated on [`crate::net::is_authority`].
//! Every peer runs the whole thing locally, the same "presentation everywhere,
//! authority nowhere" split [`crate::fx`] already draws — so two players will
//! be looking at *different* parcels, and neither can tell, because a parcel
//! has no effect on anything. That is the point: a dozen crates moving every
//! frame is exactly the kind of traffic that has no business on the wire.
//!
//! If parcels ever need to *mean* something — a requisition you ordered
//! actually riding the belt — the thing to replicate is the one order it
//! belongs to, not the box; [`Parcel::destination`] is already the whole
//! decision, and [`sort_destination`] is the only place it is made.
//!
//! # Where the geometry comes from
//!
//! The map. Each line is assembled from `conveyor_spot` markers ([`ConveyorSpot`])
//! sharing a `line` id: an `intake` hatch, `run` segments in `index` order, a
//! `sorter`, and one `chute` per destination. Nothing about the layout is
//! hard-coded here — this module knows how a conveyor behaves, and `lab.map`
//! knows where Cargo's is.
//!
//! A run states a **plan** length and a `rise`, and starts wherever the run
//! before it finished — height included. That is what lets a line climb over
//! something and come back down without this module knowing anything about
//! walkways: Cargo's belt does exactly that over the lane between its two
//! airlocks, and every consequence of it (legless spans instead of legged
//! modules, a trestle at each end of the bridge, parcels riding up a slope)
//! falls out of the path rather than being special-cased.
//!
//! # Why nothing carries a collider
//!
//! Because it does not need one. `WalkableAreas::contain_on_surface` confines
//! the player to `func_walkable` volumes and crew path from them exclusively,
//! so the map carving the belt's band out of Cargo's floor is the *entire*
//! collision story. A [`crate::lab::Solid`] here would be dead weight, and the
//! test that actually protects this is
//! `no_conveyor_marker_stands_in_walkable_floor` in `lab::tb_map`.

use bevy::asset::RenderAssetUsages;
use bevy::gltf::GltfAssetLabel;
use bevy::image::{ImageAddressMode, ImageLoaderSettings, ImageSampler, ImageSamplerDescriptor};
use bevy::math::Affine2;
use bevy::mesh::Indices;
use bevy::prelude::*;
use bevy::render::render_resource::PrimitiveTopology;

use crate::audio::{ConveyorHum, ParcelDrop};
use crate::lab::tb::{pixel_sign_text_mesh, ConveyorSpot};
use crate::lab::STATION_KIT;
use crate::AppState;

/// How fast the belt runs, in metres per second.
///
/// Slow. A parcel should be readable as an object crossing a room, not a blur —
/// at this speed the 51 m Cargo line takes about a minute end to end, which is
/// long enough that standing and watching it is a thing you can do.
const BELT_SPEED: f32 = 0.9;
/// Width of the belt surface.
const BELT_WIDTH: f32 = 0.9;
/// Height of the belt deck off the floor. Under `SETTLE_MAX`, so it reads as
/// something you could put a box down on rather than something you walk into.
const DECK_HEIGHT: f32 = 0.95;
/// Length of one authored frame module, in metres. Runs are tiled with these.
const BELT_MODULE: f32 = 2.0;
/// Metres of belt per repeat of `belt.png`.
const BELT_TILE: f32 = 0.5;
/// How far a parcel sinks once it reaches a chute mouth before it stops
/// existing. Enough to put the whole box under the floor from a deck at
/// [`DECK_HEIGHT`] — a shorter drop leaves it hanging in mid-air when it
/// despawns, which is only invisible because the chute hood happens to cover
/// it.
const DROP_DEPTH: f32 = 1.4;
/// Seconds between parcels, drawn fresh each time.
const PARCEL_GAP: (f32, f32) = (2.5, 5.0);
/// Metres between belt hum sources along a run. One point source on a 26 m line
/// leaves both ends silent; this is close enough that the hum follows you.
const HUM_SPACING: f32 = 8.0;
/// Size of the lettering plate over a chute.
const CHUTE_SIGN: (f32, f32) = (1.7, 0.28);

pub struct FreightPlugin;

impl Plugin for FreightPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<ConveyorLines>()
            .add_systems(
                OnEnter(AppState::Playing),
                (load_freight_assets, forget_conveyor_lines),
            )
            .add_systems(
                Update,
                (
                    build_conveyor_lines,
                    dress_conveyors.run_if(resource_changed::<ConveyorLines>),
                    spawn_parcels,
                    advance_parcels,
                    scroll_belts,
                )
                    .chain()
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

// ---------------------------------------------------------------------------
// The lines themselves
// ---------------------------------------------------------------------------

/// Where a parcel leaves the world.
#[derive(Debug, Clone)]
pub struct Chute {
    /// Distance along the main path at which the diverter pushes a parcel off.
    pub at: f32,
    /// The mouth itself, at deck height — a parcel arrives here, then drops.
    pub mouth: Vec3,
    /// Length of the diverter stub, from the main line to the mouth.
    ///
    /// Measured rather than declared: the map decides how far off the line it
    /// puts a chute, and a constant here would silently disagree with it the
    /// first time one moved.
    pub stub: f32,
    /// Destination signage.
    pub label: String,
}

/// One belt, assembled from its markers.
#[derive(Debug, Clone, Default)]
pub struct Line {
    pub id: String,
    /// The main run's waypoints in travel order, at deck height.
    pub path: Vec<Vec3>,
    /// Total length of [`Line::path`].
    pub length: f32,
    /// Where the intake hatch is, and which way it faces.
    pub intake: Option<Transform>,
    /// Where the scanner arch stands.
    pub sorter: Option<Transform>,
    /// Destinations, in the order they appear along the path.
    pub chutes: Vec<Chute>,
}

impl Line {
    /// The point `distance` metres along the path, clamped at both ends.
    pub fn position_at(&self, distance: f32) -> Vec3 {
        let Some(&first) = self.path.first() else {
            return Vec3::ZERO;
        };
        let mut remaining = distance.max(0.0);
        for pair in self.path.windows(2) {
            let step = pair[1] - pair[0];
            let len = step.length();
            if len <= f32::EPSILON {
                continue;
            }
            if remaining <= len {
                return pair[0] + step / len * remaining;
            }
            remaining -= len;
        }
        self.path.last().copied().unwrap_or(first)
    }

    /// Which way the belt is heading `distance` metres along.
    pub fn direction_at(&self, distance: f32) -> Vec3 {
        let mut remaining = distance.max(0.0);
        let mut last = Vec3::Z;
        for pair in self.path.windows(2) {
            let step = pair[1] - pair[0];
            let len = step.length();
            if len <= f32::EPSILON {
                continue;
            }
            last = step / len;
            if remaining <= len {
                return last;
            }
            remaining -= len;
        }
        last
    }

    /// How far along the path the point nearest `target` is.
    ///
    /// This is what turns a chute marker — which sits *off* the line — into the
    /// distance at which its diverter fires, without the map having to state
    /// the same position twice.
    pub fn distance_of_nearest(&self, target: Vec3) -> f32 {
        let mut best = (f32::MAX, 0.0);
        let mut travelled = 0.0;
        for pair in self.path.windows(2) {
            let step = pair[1] - pair[0];
            let len = step.length();
            if len <= f32::EPSILON {
                continue;
            }
            let along = ((target - pair[0]).dot(step) / (len * len)).clamp(0.0, 1.0);
            let closest = pair[0] + step * along;
            let gap = closest.distance_squared(target);
            if gap < best.0 {
                best = (gap, travelled + len * along);
            }
            travelled += len;
        }
        best.1
    }
}

#[derive(Resource, Debug, Clone, Default)]
pub struct ConveyorLines {
    pub lines: Vec<Line>,
}

/// Drops the previous shift's layout on the way into a new one.
///
/// The props and parcels themselves go with `until_we_leave_the_lab`, but the
/// resource is not despawnable and would otherwise survive a quit to the menu.
/// Left in place, `spawn_parcels` would put boxes on the *old* station's belt
/// for the frames between entering `Playing` and the new map's markers
/// arriving — briefly, invisibly, and only in the one case nobody tests.
fn forget_conveyor_lines(mut lines: ResMut<ConveyorLines>) {
    lines.lines.clear();
}

/// One `conveyor_spot`, stripped of Bevy.
///
/// [`Line::assemble`] works from these rather than straight off the components
/// so the interesting half — chaining runs into a path and projecting chutes
/// onto it — can be tested without a `World`, a map load or a GPU. Getting
/// that half wrong produces a belt that looks fine standing still and
/// teleports parcels the moment one moves.
#[derive(Debug, Clone)]
pub struct Piece {
    pub index: i32,
    pub role: String,
    /// The marker's origin, at floor level.
    pub origin: Vec3,
    /// Unit direction of travel, horizontal.
    pub travel: Vec3,
    /// Metres of belt in plan, `run` only.
    pub length: f32,
    /// Metres climbed over that plan length, `run` only.
    pub rise: f32,
    pub label: String,
}

impl Line {
    /// Chains a line's pieces into a path, a sorter and a set of chutes.
    ///
    /// Returns `None` for a line with no belt or nowhere to send anything —
    /// half a line is worse than none, because it draws frames a parcel then
    /// rides off the end of.
    pub fn assemble(id: &str, pieces: &mut [Piece]) -> Option<Self> {
        pieces.sort_by_key(|piece| piece.index);

        let mut line = Self {
            id: id.to_string(),
            ..default()
        };

        // Runs first: everything else is positioned relative to the path they
        // describe, including the chutes' diverter distances.
        //
        // Height is carried between runs rather than stated per marker, so a
        // climb has to be undone before the belt can be at deck height again —
        // which is what makes "it goes up over the walkway and back down" a
        // property of the map rather than something this code assumes.
        let mut deck_y = DECK_HEIGHT;
        for piece in pieces.iter().filter(|piece| piece.role == "run") {
            let start = Vec3::new(piece.origin.x, deck_y, piece.origin.z);
            let end = start + piece.travel * piece.length + Vec3::Y * piece.rise;
            if line
                .path
                .last()
                .is_none_or(|previous| previous.distance(start) > 0.05)
            {
                line.path.push(start);
            }
            line.path.push(end);
            deck_y += piece.rise;
        }
        line.length = line
            .path
            .windows(2)
            .map(|pair| pair[0].distance(pair[1]))
            .sum();

        for piece in pieces.iter() {
            let placed = Transform::from_translation(piece.origin)
                .with_rotation(facing_yaw(piece.travel));
            match piece.role.as_str() {
                "intake" => line.intake = Some(placed),
                "sorter" => line.sorter = Some(placed),
                "chute" => {
                    let mouth = deck(piece.origin);
                    let at = line.distance_of_nearest(mouth);
                    line.chutes.push(Chute {
                        at,
                        mouth,
                        stub: line.position_at(at).distance(mouth),
                        label: piece.label.clone(),
                    });
                }
                "run" => {}
                other => warn!("conveyor_spot on line '{id}' has unknown role '{other}'"),
            }
        }
        line.chutes
            .sort_by(|a, b| a.at.partial_cmp(&b.at).unwrap_or(std::cmp::Ordering::Equal));

        if line.path.len() < 2 || line.chutes.is_empty() {
            warn!("conveyor line '{id}' has no run segments or no chutes; skipping");
            return None;
        }
        Some(line)
    }
}

/// Rebuilds every line whenever the map delivers new markers.
///
/// Wholesale rather than incrementally: markers arrive together as one scene
/// instantiation, and a map hot reload delivers a fresh set on top of the old
/// one. Rebuilding from scratch is the only version that is correct in both
/// cases, and it costs a handful of vector pushes once.
fn build_conveyor_lines(
    mut lines: ResMut<ConveyorLines>,
    arrived: Query<(), Added<ConveyorSpot>>,
    markers: Query<(&ConveyorSpot, &Transform)>,
) {
    if arrived.is_empty() {
        return;
    }

    let mut by_line: std::collections::BTreeMap<String, Vec<Piece>> = Default::default();
    for (spot, transform) in &markers {
        let id = spot.line.trim();
        if id.is_empty() {
            warn!("ignoring a conveyor_spot that names no line");
            continue;
        }
        by_line.entry(id.to_string()).or_default().push(Piece {
            index: spot.index,
            role: spot.role.trim().to_string(),
            origin: transform.translation,
            travel: travel(transform),
            length: spot.length,
            rise: spot.rise,
            label: spot.label.trim().to_string(),
        });
    }

    lines.lines = by_line
        .into_iter()
        .filter_map(|(id, mut pieces)| Line::assemble(&id, &mut pieces))
        .collect();
}

/// Lifts a marker's floor-level origin to the belt deck.
fn deck(origin: Vec3) -> Vec3 {
    Vec3::new(origin.x, DECK_HEIGHT, origin.z)
}

/// A marker's direction of travel.
///
/// Local `+Z`, matching every other point class in the map — `DeliveryStation`
/// reads its own facing the same way, and the decoration placement test treats
/// `"0 90 0"` as `+x` on the same basis.
fn travel(transform: &Transform) -> Vec3 {
    let forward = transform.rotation * Vec3::Z;
    Vec3::new(forward.x, 0.0, forward.z)
        .try_normalize()
        .unwrap_or(Vec3::Z)
}

/// A rotation putting local `+Z` along `direction`.
///
/// Deliberately **not** `Transform::looking_to`, which aims local `-Z`: every
/// point class in the map treats `+Z` as the facing (`lab::placement_transform`
/// builds exactly this quaternion, and the machine-placement test pins
/// `"0 90 0"` to `+x` on the same basis). Mixing the two conventions compiles,
/// looks reasonable, and points every prop backwards.
fn facing_yaw(direction: Vec3) -> Quat {
    Quat::from_rotation_y(direction.x.atan2(direction.z))
}

/// [`facing_yaw`] plus the pitch to follow a climb.
///
/// Built as yaw-then-pitch rather than `Quat::from_rotation_arc`, which is free
/// to pick any roll about the travel axis and would tip a belt sideways on the
/// slope.
fn facing(direction: Vec3) -> Quat {
    let flat = Vec3::new(direction.x, 0.0, direction.z);
    let run = flat.length();
    if run <= f32::EPSILON {
        return facing_yaw(Vec3::Z);
    }
    facing_yaw(flat) * Quat::from_rotation_x(-direction.y.atan2(run))
}

// ---------------------------------------------------------------------------
// Assets and dressing
// ---------------------------------------------------------------------------

#[derive(Resource)]
struct FreightAssets {
    /// Ground-level frame, with its own legs.
    belt_module: Handle<WorldAsset>,
    /// The same frame without legs, for anything climbing or already up in the
    /// air — a legged module on a slope either floats or buries its feet.
    belt_span: Handle<WorldAsset>,
    /// Holds an elevated span up. Modelled 1 m tall and scaled on Y to reach
    /// whatever height the map's rises add up to.
    belt_trestle: Handle<WorldAsset>,
    intake_port: Handle<WorldAsset>,
    sorter: Handle<WorldAsset>,
    chute: Handle<WorldAsset>,
    parcel: Handle<WorldAsset>,
    /// One shared material for every belt surface. The tiling lives in each
    /// mesh's own UVs, so a single scrolling material serves runs of any
    /// length — see [`belt_surface_mesh`].
    belt: Handle<StandardMaterial>,
    sign_plate: Handle<StandardMaterial>,
    lettering: Handle<StandardMaterial>,
    plate_mesh: Handle<Mesh>,
}

fn load_freight_assets(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let model = |file: &'static str| {
        asset_server.load(GltfAssetLabel::Scene(0).from_asset(format!("{STATION_KIT}/{file}")))
    };

    // Bevy clamps textures to their edges by default, which would stretch one
    // copy of the tread over the whole run instead of repeating it.
    let tread = asset_server
        .load_builder()
        .with_settings(|settings: &mut ImageLoaderSettings| {
            settings.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
                address_mode_u: ImageAddressMode::Repeat,
                address_mode_v: ImageAddressMode::Repeat,
                ..default()
            });
        })
        .load("textures/belt.png");

    commands.insert_resource(FreightAssets {
        belt_module: model("decor_cargo_belt_module.glb"),
        belt_span: model("decor_cargo_belt_span.glb"),
        belt_trestle: model("decor_cargo_belt_trestle.glb"),
        intake_port: model("decor_cargo_intake_port.glb"),
        sorter: model("decor_cargo_sorter.glb"),
        chute: model("decor_cargo_chute.glb"),
        parcel: model("decor_cargo_parcel.glb"),
        belt: materials.add(StandardMaterial {
            base_color: Color::srgb(0.34, 0.35, 0.38),
            base_color_texture: Some(tread),
            perceptual_roughness: 0.92,
            metallic: 0.0,
            ..default()
        }),
        sign_plate: materials.add(StandardMaterial {
            base_color: Color::srgb(0.025, 0.035, 0.045),
            perceptual_roughness: 0.82,
            metallic: 0.18,
            ..default()
        }),
        lettering: materials.add(StandardMaterial {
            base_color: Color::srgb(1.0, 0.86, 0.42),
            emissive: LinearRgba::new(1.9, 1.4, 0.4, 1.0),
            unlit: true,
            double_sided: true,
            cull_mode: None,
            ..default()
        }),
        plate_mesh: meshes.add(Cuboid::new(CHUTE_SIGN.0, CHUTE_SIGN.1 * 1.6, 0.06)),
    });
}

/// Everything this module put in the world. Tagged so a rebuild can clear the
/// lot rather than trying to reconcile it piece by piece.
#[derive(Component)]
struct FreightProp;

/// A belt surface, whose shared material [`scroll_belts`] advances.
#[derive(Component)]
struct BeltSurface;

#[allow(clippy::type_complexity)]
fn dress_conveyors(
    mut commands: Commands,
    assets: Option<Res<FreightAssets>>,
    mut meshes: ResMut<Assets<Mesh>>,
    lines: Res<ConveyorLines>,
    existing: Query<Entity, Or<(With<FreightProp>, With<Parcel>)>>,
) {
    let Some(assets) = assets else {
        return;
    };
    for entity in &existing {
        commands.entity(entity).despawn();
    }

    for line in &lines.lines {
        if let Some(intake) = line.intake {
            spawn_prop(&mut commands, assets.intake_port.clone(), intake, &line.id, "intake");
        }
        if let Some(sorter) = line.sorter {
            spawn_prop(&mut commands, assets.sorter.clone(), sorter, &line.id, "sorter");
        }

        for pair in line.path.windows(2) {
            dress_run(&mut commands, &assets, &mut meshes, pair[0], pair[1]);
        }

        // A trestle wherever the path is off the ground. Those are exactly the
        // corners of a bridge — the top of the climb and the top of the
        // descent — so the supports land where a real one would put them
        // without the map having to place a single one by hand.
        for (index, point) in line.path.iter().enumerate() {
            let lift = point.y - DECK_HEIGHT;
            if lift <= 0.01 {
                continue;
            }
            let along = line
                .path
                .get(index + 1)
                .or_else(|| index.checked_sub(1).and_then(|before| line.path.get(before)))
                .map(|neighbour| *neighbour - *point)
                .unwrap_or(Vec3::Z);
            let flat = Vec3::new(along.x, 0.0, along.z)
                .try_normalize()
                .unwrap_or(Vec3::Z);
            commands.spawn((
                Name::new(format!("{} trestle", line.id)),
                FreightProp,
                WorldAssetRoot(assets.belt_trestle.clone()),
                Transform::from_translation(floor(*point))
                    .with_rotation(facing_yaw(flat))
                    // Modelled a metre tall so one asset serves any height the
                    // map's rises happen to add up to.
                    .with_scale(Vec3::new(1.0, lift, 1.0)),
                Visibility::default(),
                crate::until_we_leave_the_lab(),
            ));
        }

        for chute in &line.chutes {
            let diverter = line.position_at(chute.at);
            let across = (chute.mouth - diverter).try_normalize().unwrap_or(Vec3::Z);
            dress_run(&mut commands, &assets, &mut meshes, diverter, chute.mouth);
            // The hood is open on its `+Z` face, and a parcel arrives from the
            // line — so the open side has to face back the way the parcel came,
            // against `across`, not along it. Pointed the other way the hood is
            // a solid wall the parcel drives into.
            spawn_prop(
                &mut commands,
                assets.chute.clone(),
                Transform::from_translation(floor(chute.mouth)).with_rotation(facing_yaw(-across)),
                &line.id,
                "chute",
            );
            if !chute.label.is_empty() {
                spawn_chute_sign(&mut commands, &assets, &mut meshes, chute, across);
            }
        }
    }
}

/// The floor under a point, for something that stands on the ground.
fn floor(point: Vec3) -> Vec3 {
    Vec3::new(point.x, 0.0, point.z)
}

/// Where a frame's origin goes so its deck lands on `point`.
///
/// For a ground-level run this is exactly [`floor`]; for one up in the air it
/// is not, which is the whole difference between a bridged belt and a bridged
/// belt with its frames still sitting on the floor underneath it.
fn frame_base(point: Vec3) -> Vec3 {
    Vec3::new(point.x, point.y - DECK_HEIGHT, point.z)
}

/// Whether a run is off the ground, either because it is climbing or because
/// it is already up. Decides between the legged module and the legless span.
fn is_elevated(start: Vec3, end: Vec3) -> bool {
    (start.y - DECK_HEIGHT).abs() > 0.01 || (end.y - DECK_HEIGHT).abs() > 0.01
}

fn spawn_prop(
    commands: &mut Commands,
    scene: Handle<WorldAsset>,
    transform: Transform,
    line: &str,
    what: &str,
) {
    commands.spawn((
        Name::new(format!("{line} {what}")),
        FreightProp,
        WorldAssetRoot(scene),
        transform,
        Visibility::default(),
        crate::until_we_leave_the_lab(),
    ));
}

/// Frames plus a scrolling surface, from `start` to `end`.
///
/// Modules are scaled to fill the span exactly rather than truncated to a whole
/// number of them: a 1.5 m diverter stub and a 16 m main run then look like the
/// same kit, and a future run of an awkward length does not end in a gap.
fn dress_run(
    commands: &mut Commands,
    assets: &FreightAssets,
    meshes: &mut Assets<Mesh>,
    start: Vec3,
    end: Vec3,
) {
    let span = end - start;
    let length = span.length();
    if length <= f32::EPSILON {
        return;
    }
    let direction = span / length;
    let heading = facing(direction);
    let frame = if is_elevated(start, end) {
        assets.belt_span.clone()
    } else {
        assets.belt_module.clone()
    };

    let modules = (length / BELT_MODULE).round().max(1.0);
    let stretch = length / (modules * BELT_MODULE);
    for step in 0..modules as usize {
        let at = frame_base(start) + direction * (step as f32 + 0.5) * BELT_MODULE * stretch;
        commands.spawn((
            Name::new("conveyor frame"),
            FreightProp,
            WorldAssetRoot(frame.clone()),
            Transform::from_translation(at)
                .with_rotation(heading)
                .with_scale(Vec3::new(1.0, 1.0, stretch)),
            Visibility::default(),
            crate::until_we_leave_the_lab(),
        ));
    }

    commands.spawn((
        Name::new("belt surface"),
        FreightProp,
        BeltSurface,
        Mesh3d(meshes.add(belt_surface_mesh(BELT_WIDTH, length))),
        MeshMaterial3d(assets.belt.clone()),
        Transform::from_translation(start).with_rotation(heading),
        crate::until_we_leave_the_lab(),
    ));

    // Hum sources every few metres. One per run would leave a 16 m belt silent
    // at both ends; the machinery you can hear should be the machinery you are
    // standing next to.
    let sources = (length / HUM_SPACING).round().max(1.0);
    for step in 0..sources as usize {
        let at = start + direction * (step as f32 + 0.5) * (length / sources);
        commands.spawn((
            Name::new("belt hum"),
            FreightProp,
            ConveyorHum,
            Transform::from_translation(at),
            crate::until_we_leave_the_lab(),
        ));
    }
}

/// How far past the mouth a chute's sign hangs, clearing the hood's back panel.
const SIGN_STANDOFF: f32 = 0.82;
/// Height of the lettering above the floor. Just over the hood, which stands
/// 1.62 m to the top of its lid.
const SIGN_HEIGHT: f32 = 1.86;

/// A lit plate over a chute saying where its parcels are going.
///
/// `toward_reader` continues *past* the mouth, away from the line. The map
/// carves the whole belt band out of both rooms' floors, so the only place
/// anyone can stand is beyond the chutes — the sign faces the walkable side and
/// hangs on the hood's closed back, not its open mouth.
fn spawn_chute_sign(
    commands: &mut Commands,
    assets: &FreightAssets,
    meshes: &mut Assets<Mesh>,
    chute: &Chute,
    toward_reader: Vec3,
) {
    let mounted = chute.mouth + toward_reader * SIGN_STANDOFF;
    let at = Vec3::new(mounted.x, SIGN_HEIGHT, mounted.z);
    let lettering = meshes.add(pixel_sign_text_mesh(&chute.label, CHUTE_SIGN.0, CHUTE_SIGN.1));

    commands
        .spawn((
            Name::new(format!("chute sign: {}", chute.label)),
            FreightProp,
            Transform::from_translation(at).with_rotation(facing_yaw(toward_reader)),
            Visibility::default(),
            crate::until_we_leave_the_lab(),
        ))
        .with_children(|parent| {
            parent.spawn((
                Mesh3d(assets.plate_mesh.clone()),
                MeshMaterial3d(assets.sign_plate.clone()),
                Transform::default(),
            ));
            parent.spawn((
                Mesh3d(lettering),
                MeshMaterial3d(assets.lettering.clone()),
                Transform::from_xyz(0.0, 0.0, 0.04),
            ));
        });
}

/// A flat quad at the belt deck, `width` across and `length` along local `+Z`,
/// with UVs already tiled so one shared material fits every run.
fn belt_surface_mesh(width: f32, length: f32) -> Mesh {
    let half = width * 0.5;
    let (u, v) = (width / BELT_TILE, length / BELT_TILE);
    Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
    )
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_POSITION,
        vec![
            [-half, 0.0, 0.0],
            [half, 0.0, 0.0],
            [half, 0.0, length],
            [-half, 0.0, length],
        ],
    )
    .with_inserted_attribute(Mesh::ATTRIBUTE_NORMAL, vec![[0.0, 1.0, 0.0]; 4])
    .with_inserted_attribute(
        Mesh::ATTRIBUTE_UV_0,
        vec![[0.0, 0.0], [u, 0.0], [u, v], [0.0, v]],
    )
    .with_inserted_indices(Indices::U32(vec![0, 2, 1, 0, 3, 2]))
}

/// Slides the tread. One material, so this touches one asset however many runs
/// there are — the tiling that would otherwise force a material per length is
/// baked into each mesh's UVs instead.
fn scroll_belts(
    time: Res<Time>,
    assets: Option<Res<FreightAssets>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    surfaces: Query<(), With<BeltSurface>>,
) {
    let Some(assets) = assets else {
        return;
    };
    if surfaces.is_empty() {
        return;
    }
    let Some(mut belt) = materials.get_mut(&assets.belt) else {
        return;
    };
    // Negative because the mesh's `v` grows along travel: sliding the lookup
    // backwards makes the pattern appear to move forwards.
    let offset = belt.uv_transform.translation.y - time.delta_secs() * BELT_SPEED / BELT_TILE;
    belt.uv_transform = Affine2::from_translation(Vec2::new(0.0, offset.rem_euclid(1.0)));
}

// ---------------------------------------------------------------------------
// Parcels
// ---------------------------------------------------------------------------

/// A box on the belt. Purely cosmetic — see the module doc.
#[derive(Component, Debug)]
pub struct Parcel {
    /// Index into [`ConveyorLines::lines`]. Every parcel is despawned whenever
    /// those are rebuilt, so this can never be stale.
    pub line: usize,
    /// Index into that line's `chutes`.
    pub destination: usize,
    /// Metres travelled along the main path.
    pub travelled: f32,
    /// Metres travelled down the diverter stub, once past the diverter. Zero
    /// while still on the main line.
    pub diverted: f32,
    /// Spin, so a wall of identical crates does not read as a texture.
    pub yaw: f32,
}

/// Which chute a new parcel is bound for.
///
/// Uniform, and its own function purely so there is one obvious place to make
/// it mean something later.
fn sort_destination(chutes: usize) -> usize {
    if chutes == 0 {
        return 0;
    }
    rand::random_range(0..chutes)
}

fn spawn_parcels(
    mut commands: Commands,
    time: Res<Time>,
    assets: Option<Res<FreightAssets>>,
    lines: Res<ConveyorLines>,
    mut countdown: Local<Vec<f32>>,
) {
    let Some(assets) = assets else {
        return;
    };
    countdown.resize(lines.lines.len(), 0.0);

    for (index, line) in lines.lines.iter().enumerate() {
        countdown[index] -= time.delta_secs();
        if countdown[index] > 0.0 {
            continue;
        }
        countdown[index] = rand::random_range(PARCEL_GAP.0..=PARCEL_GAP.1);

        let destination = sort_destination(line.chutes.len());
        commands.spawn((
            Name::new("parcel"),
            Parcel {
                line: index,
                destination,
                travelled: 0.0,
                diverted: 0.0,
                yaw: rand::random_range(-0.25..=0.25),
            },
            WorldAssetRoot(assets.parcel.clone()),
            Transform::from_translation(line.position_at(0.0)),
            Visibility::default(),
            crate::until_we_leave_the_lab(),
        ));
    }
}

fn advance_parcels(
    mut commands: Commands,
    time: Res<Time>,
    lines: Res<ConveyorLines>,
    mut parcels: Query<(Entity, &mut Parcel, &mut Transform)>,
) {
    let step = time.delta_secs() * BELT_SPEED;
    for (entity, mut parcel, mut transform) in &mut parcels {
        let Some(line) = lines.lines.get(parcel.line) else {
            commands.entity(entity).despawn();
            continue;
        };
        let Some(chute) = line.chutes.get(parcel.destination) else {
            commands.entity(entity).despawn();
            continue;
        };

        if parcel.travelled < chute.at {
            parcel.travelled = (parcel.travelled + step).min(chute.at);
            let heading = line.direction_at(parcel.travelled);
            transform.translation = line.position_at(parcel.travelled);
            transform.rotation = facing_yaw(heading) * Quat::from_rotation_y(parcel.yaw);
            continue;
        }

        // Past the diverter: across onto the stub, then down out of the world.
        let was = parcel.diverted;
        parcel.diverted += step;
        if was < chute.stub && parcel.diverted >= chute.stub {
            commands.spawn((
                Name::new("parcel drop"),
                ParcelDrop,
                Transform::from_translation(chute.mouth),
                crate::until_we_leave_the_lab(),
            ));
        }
        if parcel.diverted >= chute.stub + DROP_DEPTH {
            commands.entity(entity).despawn();
            continue;
        }

        let diverter = line.position_at(chute.at);
        let across = (chute.mouth - diverter).try_normalize().unwrap_or(Vec3::Z);
        let along = parcel.diverted.min(chute.stub);
        let sink = (parcel.diverted - chute.stub).max(0.0);
        transform.translation = diverter + across * along - Vec3::Y * sink;
        transform.rotation = facing_yaw(across) * Quat::from_rotation_y(parcel.yaw);
    }
}

#[cfg(test)]
mod tests {
    use bevy::mesh::VertexAttributeValues;

    use super::*;

    /// A right-angled line, so the corner cases in the path walkers are real.
    fn bent_line() -> Line {
        let mut line = Line {
            id: "test".into(),
            path: vec![
                Vec3::new(0.0, DECK_HEIGHT, 0.0),
                Vec3::new(10.0, DECK_HEIGHT, 0.0),
                Vec3::new(10.0, DECK_HEIGHT, 6.0),
            ],
            length: 16.0,
            ..default()
        };
        let mouth = Vec3::new(12.5, DECK_HEIGHT, 4.0);
        let at = line.distance_of_nearest(mouth);
        line.chutes.push(Chute {
            at,
            mouth,
            stub: line.position_at(at).distance(mouth),
            label: "TEST".into(),
        });
        line
    }

    #[test]
    fn a_parcel_walks_straight_through_a_corner() {
        let line = bent_line();
        assert_eq!(line.position_at(0.0), Vec3::new(0.0, DECK_HEIGHT, 0.0));
        assert_eq!(line.position_at(4.0), Vec3::new(4.0, DECK_HEIGHT, 0.0));
        // Exactly on the corner, and then around it.
        assert_eq!(line.position_at(10.0), Vec3::new(10.0, DECK_HEIGHT, 0.0));
        assert_eq!(line.position_at(13.0), Vec3::new(10.0, DECK_HEIGHT, 3.0));
        // Past the end it clamps rather than running on into nothing.
        assert_eq!(line.position_at(99.0), Vec3::new(10.0, DECK_HEIGHT, 6.0));
    }

    #[test]
    fn direction_turns_with_the_path() {
        let line = bent_line();
        assert_eq!(line.direction_at(4.0), Vec3::X);
        assert_eq!(line.direction_at(13.0), Vec3::Z);
    }

    #[test]
    fn a_chute_off_the_line_finds_its_diverter_and_measures_its_own_stub() {
        // The mouth is 2.5 m off the second leg, level with the point 14 m
        // along a 16 m path. Nothing in the map states either number; both are
        // derived from where the chute was put, which is the point.
        let line = bent_line();
        let chute = &line.chutes[0];
        assert!((chute.at - 14.0).abs() < 0.001, "{chute:?}");
        assert!((chute.stub - 2.5).abs() < 0.001, "{chute:?}");
    }

    #[test]
    fn a_degenerate_line_answers_rather_than_panicking() {
        let empty = Line::default();
        assert_eq!(empty.position_at(5.0), Vec3::ZERO);
        assert_eq!(empty.direction_at(5.0), Vec3::Z);
        assert_eq!(empty.distance_of_nearest(Vec3::X), 0.0);
    }

    #[test]
    fn every_parcel_gets_a_real_chute() {
        for chutes in 1..=4 {
            for _ in 0..64 {
                assert!(sort_destination(chutes) < chutes);
            }
        }
        // Zero is not a legal line -- `build_conveyor_lines` drops those --
        // but the sorter still has to answer rather than divide by nothing.
        assert_eq!(sort_destination(0), 0);
    }

    #[test]
    fn the_belt_surface_tiles_by_length() {
        let mesh = belt_surface_mesh(BELT_WIDTH, 8.0);
        let VertexAttributeValues::Float32x2(uvs) = mesh
            .attribute(Mesh::ATTRIBUTE_UV_0)
            .expect("belt surface has UVs")
        else {
            panic!("belt surface UVs should be a pair of floats per vertex");
        };
        let longest = uvs
            .iter()
            .map(|uv| uv[1])
            .fold(0.0f32, |best, v| best.max(v));
        assert!(
            (longest - 8.0 / BELT_TILE).abs() < 0.001,
            "an 8 m run should repeat the tread {} times, got {longest}",
            8.0 / BELT_TILE,
        );
    }

    /// Cargo's line exactly as `assets/maps/lab.map` authors it, in world
    /// coordinates. `lab::tb_map::conveyor_markers_form_continuous_lines` is
    /// what keeps the map saying this; here it is the input, so the two halves
    /// of the feature are pinned from both ends.
    fn authored_cargo_line() -> Line {
        let mut pieces = authored_cargo_line_pieces();
        Line::assemble("cargo.main", &mut pieces).expect("the authored line assembles")
    }

    fn authored_cargo_line_pieces() -> Vec<Piece> {
        let east = Vec3::X;
        let piece = |index, role: &str, x, z, length, rise, label: &str| Piece {
            index,
            role: role.into(),
            origin: Vec3::new(x, 0.0, z),
            travel: east,
            length,
            rise,
            label: label.into(),
        };
        vec![
            piece(0, "intake", -109.375, 49.8, 0.0, 0.0, ""),
            piece(1, "run", -109.375, 49.8, 16.0, 0.0, ""),
            piece(2, "run", -93.375, 49.8, 7.175, 0.0, ""),
            // Up over the cross lane and back down.
            piece(3, "run", -86.2, 49.8, 4.0, 1.55, ""),
            piece(4, "run", -82.2, 49.8, 4.4, 0.0, ""),
            piece(5, "run", -77.8, 49.8, 4.0, -1.55, ""),
            piece(6, "run", -73.8, 49.8, 14.8, 0.0, ""),
            piece(7, "sorter", -92.0, 49.8, 0.0, 0.0, ""),
            piece(8, "chute", -90.0, 48.5, 0.0, 0.0, "MEDICAL / CHEM"),
            piece(9, "chute", -87.0, 48.5, 0.0, 0.0, "BRIDGE / SECURITY"),
            piece(10, "chute", -71.0, 48.5, 0.0, 0.0, "ENGINEERING"),
            piece(11, "chute", -66.0, 48.5, 0.0, 0.0, "SERVICE / CHAPEL"),
            piece(12, "chute", -61.0, 48.5, 0.0, 0.0, "BOTANY"),
        ]
    }

    #[test]
    fn cargos_runs_chain_into_one_unbroken_path() {
        let line = authored_cargo_line();
        // Six runs that meet become seven waypoints, not twelve: each join is
        // collapsed. A duplicated point would stall a parcel for a frame.
        assert_eq!(line.path.len(), 7, "{:?}", line.path);
        assert_eq!(line.position_at(0.0), Vec3::new(-109.375, DECK_HEIGHT, 49.8));
        // The far end is back at deck height: every metre climbed is given
        // back, which is what stops the belt ending in mid-air.
        let end = line.position_at(line.length);
        assert!((end.x - -59.0).abs() < 0.001, "{end:?}");
        assert!((end.y - DECK_HEIGHT).abs() < 0.001, "{end:?}");
        // The belt has to span the whole thickness of the party wall, whose
        // brushes run x -94.125..-93.875 either side of the opening the map
        // cuts for it. A run that stopped inside that band would leave the
        // belt visibly severed in the hole.
        assert!(line.position_at(15.2).x < -94.125);
        assert!(line.position_at(15.6).x > -93.875);
    }

    #[test]
    fn the_bridge_clears_head_height_over_the_whole_cross_lane() {
        // The lane between Cargo's two long-wall airlocks is walkable floor
        // from x -81.5 to -78.5, and the belt crosses it. If the deck ever
        // drops toward walking height there, the hall's only route between
        // those two doors becomes a thing you walk face-first into.
        let line = authored_cargo_line();
        let mut samples = 0;
        for step in 0..=200 {
            let at = line.position_at(line.length * step as f32 / 200.0);
            if !(-81.5..=-78.5).contains(&at.x) {
                continue;
            }
            samples += 1;
            assert!(
                at.y >= 2.4,
                "the belt is only {} m up at x {}; a body stands 1.7 m tall",
                at.y,
                at.x,
            );
        }
        assert!(samples > 0, "the sampling never crossed the lane at all");
    }

    #[test]
    fn every_cargo_chute_diverts_downstream_of_the_sorter() {
        let line = authored_cargo_line();
        let sorter = line.sorter.expect("the authored line has a sorter");
        let sorter_at = line.distance_of_nearest(deck(sorter.translation));
        assert_eq!(line.chutes.len(), 5);
        for chute in &line.chutes {
            assert!(
                chute.at > sorter_at,
                "{} diverts at {} m, upstream of the sorter at {sorter_at} m",
                chute.label,
                chute.at,
            );
            // 1.3 m off the line, and measured rather than assumed.
            assert!((chute.stub - 1.3).abs() < 0.001, "{chute:?}");
            // Every diverter is on a flat stretch. Pushing a parcel sideways
            // off a slope is not a thing a conveyor does.
            assert!(
                (line.position_at(chute.at).y - DECK_HEIGHT).abs() < 0.001,
                "{} diverts on the bridge, {} m up",
                chute.label,
                line.position_at(chute.at).y,
            );
        }
        // In path order, so the labels a player reads left to right are the
        // order parcels actually reach them.
        let labels: Vec<&str> = line.chutes.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "MEDICAL / CHEM",
                "BRIDGE / SECURITY",
                "ENGINEERING",
                "SERVICE / CHAPEL",
                "BOTANY",
            ],
        );
    }

    #[test]
    fn a_parcel_ends_up_under_the_floor_rather_than_hanging_in_the_air() {
        // The drop has to clear the deck, or a parcel despawns in mid-air and
        // only the chute hood hides it.
        let line = authored_cargo_line();
        let sink = line.chutes[0].stub + DROP_DEPTH - line.chutes[0].stub;
        assert!(
            DECK_HEIGHT - sink < 0.0,
            "a {DROP_DEPTH} m drop from a {DECK_HEIGHT} m deck leaves the parcel above the floor",
        );
    }

    #[test]
    fn a_line_with_nowhere_to_send_anything_is_dropped() {
        let mut pieces = vec![Piece {
            index: 0,
            role: "run".into(),
            origin: Vec3::ZERO,
            travel: Vec3::X,
            length: 8.0,
            rise: 0.0,
            label: String::new(),
        }];
        assert!(Line::assemble("chuteless", &mut pieces).is_none());

        let mut chute_only = vec![Piece {
            index: 0,
            role: "chute".into(),
            origin: Vec3::ZERO,
            travel: Vec3::X,
            length: 0.0,
            rise: 0.0,
            label: "NOWHERE".into(),
        }];
        assert!(Line::assemble("beltless", &mut chute_only).is_none());
    }

    #[test]
    fn pieces_are_chained_in_index_order_not_map_order() {
        // TrenchBroom writes entities in whatever order they were created, so
        // the path has to come from `index`. This matters far more now the line
        // has a bridge in it: heights are carried from one run to the next, so
        // reading them out of order does not merely reorder the waypoints, it
        // puts the climb in the wrong place and leaves the far end in the air.
        let forward = authored_cargo_line();
        let mut shuffled = {
            let mut pieces = Vec::new();
            let mut source = authored_cargo_line_pieces();
            while let Some(piece) = source.pop() {
                pieces.insert(pieces.len() / 2, piece);
            }
            pieces
        };
        let built = Line::assemble("cargo.main", &mut shuffled).expect("assembles");
        assert_eq!(built.path, forward.path);
        assert_eq!(built.chutes.len(), forward.chutes.len());
    }

    #[test]
    fn the_belt_texture_is_on_disk() {
        let path = std::path::Path::new("assets/textures/belt.png");
        assert!(
            path.is_file(),
            "the belt tread is missing -- run tools/gen_belt_texture.py",
        );
        let png = std::fs::read(path).expect("reading the belt tread");
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n", "{path:?} is not a PNG");
        let width = u32::from_be_bytes(png[16..20].try_into().unwrap());
        let height = u32::from_be_bytes(png[20..24].try_into().unwrap());
        assert_eq!((width, height), (128, 128));
        assert!(width.is_power_of_two() && height.is_power_of_two());
    }

    #[test]
    fn every_conveyor_glb_is_exported() {
        for file in [
            "decor_cargo_belt_module.glb",
            "decor_cargo_intake_port.glb",
            "decor_cargo_sorter.glb",
            "decor_cargo_chute.glb",
            "decor_cargo_parcel.glb",
        ] {
            let path = format!("assets/{STATION_KIT}/{file}");
            assert!(
                std::path::Path::new(&path).is_file(),
                "{path} is missing -- re-run generate_decor_modules.py",
            );
        }
    }
}
