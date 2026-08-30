//! Development-only character and chemistry reaction harness.
//!
//! A real [`Body`] using the ordinary injection/metabolism path is more useful
//! than a pose viewer: if a status looks wrong here, it will look wrong on a
//! crew member for the same reason. The subject and its pre-filled samples are
//! spawned only in debug builds, keeping test shortcuts out of a release.

use bevy::gltf::GltfAssetLabel;
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::{ReagentId, Solution, StatusKind, Units};
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::body::{Bloodstream, Body};
use crate::chem_data::ChemDb;
use crate::containers::{Container, ContainerKind};
use crate::interaction::{InteractRequested, Interactable};
use crate::machines::chemist_entity;
use crate::net::is_authority;
use crate::player::Chemist;
use crate::AppState;

const SUBJECT_MODEL: &str = "3dassets/glb/first_char_test.glb";
const SUBJECT_POSITION: Vec3 = Vec3::new(12.35, 0.94, -3.0);
const LOCOMOTION_START: Vec3 = Vec3::new(9.75, 0.94, -4.65);
const LOCOMOTION_END: Vec3 = Vec3::new(13.15, 0.94, -4.65);
const RACK_POSITION: Vec3 = Vec3::new(10.15, 0.40, -1.15);
const SAMPLE_Y: f32 = 0.86;
const SAMPLE_UNITS: i32 = 5;

/// Broad enough to exercise every current presentation channel without
/// turning the development rack into a duplicate of the chemistry book.
const TEST_DOSES: [(&str, &str); 8] = [
    ("hyperzine", "hastened"),
    ("chloral_hydrate", "sedated + blurred"),
    ("hooch", "drunk + unsteady"),
    ("phlogiston", "burning"),
    ("cryostylane", "chilled"),
    ("unstable_mutagen", "mutating + irradiated"),
    ("space_drugs", "euphoric + hallucinating"),
    ("bath_salts", "paranoid + hastened"),
];

pub struct CharacterLabPlugin;

impl Plugin for CharacterLabPlugin {
    fn build(&self, app: &mut App) {
        if !cfg!(debug_assertions) {
            return;
        }

        app.add_systems(
            OnEnter(AppState::Playing),
            (
                load_character_lab_assets,
                spawn_character_lab.run_if(is_authority),
            ),
        )
        .add_systems(
            Update,
            (
                dress_test_subjects,
                configure_test_subject_faces.after(dress_test_subjects),
                tag_test_subject_surfaces.after(dress_test_subjects),
                attach_test_subject_animation.after(dress_test_subjects),
                drive_test_subject_animation.after(attach_test_subject_animation),
                pace_locomotion_previews.run_if(is_authority),
                reset_subject_and_samples.run_if(is_authority),
            )
                .run_if(in_state(AppState::Playing)),
        );
    }
}

/// Replicated identity for a development body. The mesh remains local
/// presentation, like chemist and crew meshes.
#[derive(Component, Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct TestSubject;

/// Replicated marker for the second debug mannequin that continuously walks
/// the short Analysis-room test lane.
#[derive(Component, Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct LocomotionPreview;

/// Authority-only integration state. Position itself already replicates.
#[derive(Component)]
struct LocomotionPreviewState {
    progress: f32,
    direction: f32,
}

/// The movable visual child. Effects animate this, never the authoritative
/// root used by reach checks and replication.
#[derive(Component)]
pub(crate) struct TestSubjectVisual {
    pub(crate) subject: Entity,
    pub(crate) rest: Vec3,
    face_variant: u8,
}

#[derive(Component)]
struct TestSubjectFaceConfigured;

/// A material-bearing mesh nested inside the imported GLB scene.
#[derive(Component)]
pub(crate) struct TestSubjectSurface {
    pub(crate) subject: Entity,
    pub(crate) base_color: Color,
    pub(crate) base_alpha_mode: AlphaMode,
}

#[derive(Component)]
struct TestDose(ReagentId);

#[derive(Resource)]
struct CharacterLabAssets {
    subject: Handle<WorldAsset>,
    animation_graph: Handle<AnimationGraph>,
    animation_nodes: [AnimationNodeIndex; 9],
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
struct TestSubjectAnimationPlayer {
    subject: Entity,
    current: CharacterAnimation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CharacterAnimation {
    Idle = 0,
    Stimulated = 1,
    Sedated = 2,
    Unsteady = 3,
    Collapsed = 4,
    Walk = 5,
    WalkDrunk = 6,
    /// A looping upper-body gesture for a resident standing at their own
    /// work post. Never selected by [`desired_character_animation`] itself —
    /// it has no bloodstream signal to key off — `crew::drive_crew_animation`
    /// layers it on top of an otherwise-idle resident by comparing their own
    /// position against `crew::CrewPosts`.
    Working = 7,
    /// A held pose for a resident at a communal relax spot's seat. Selected
    /// the same way `Working` is — see its doc comment.
    Sitting = 8,
}

fn load_character_lab_assets(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut animation_graphs: ResMut<Assets<AnimationGraph>>,
) {
    // Blender exports Actions alphabetically: Collapsed, Idle, Sedated,
    // Sitting, Stimulated, Unsteady, Walk, WalkDrunk, Working. Keep
    // gameplay-facing node order explicit even though Blender stores the
    // clips alphabetically. Verified against the exported GLB's own
    // `animations` array, not guessed — see the station-kit/Bevy-postdates-
    // cutoff memory's rule for this exact pipeline.
    let (animation_graph, animation_nodes) = AnimationGraph::from_clips([
        asset_server.load(GltfAssetLabel::Animation(1).from_asset(SUBJECT_MODEL)), // Idle
        asset_server.load(GltfAssetLabel::Animation(4).from_asset(SUBJECT_MODEL)), // Stimulated
        asset_server.load(GltfAssetLabel::Animation(2).from_asset(SUBJECT_MODEL)), // Sedated
        asset_server.load(GltfAssetLabel::Animation(5).from_asset(SUBJECT_MODEL)), // Unsteady
        asset_server.load(GltfAssetLabel::Animation(0).from_asset(SUBJECT_MODEL)), // Collapsed
        asset_server.load(GltfAssetLabel::Animation(6).from_asset(SUBJECT_MODEL)), // Walk
        asset_server.load(GltfAssetLabel::Animation(7).from_asset(SUBJECT_MODEL)), // WalkDrunk
        asset_server.load(GltfAssetLabel::Animation(8).from_asset(SUBJECT_MODEL)), // Working
        asset_server.load(GltfAssetLabel::Animation(3).from_asset(SUBJECT_MODEL)), // Sitting
    ]);
    commands.insert_resource(CharacterLabAssets {
        subject: asset_server.load(GltfAssetLabel::Scene(0).from_asset(SUBJECT_MODEL)),
        animation_graph: animation_graphs.add(animation_graph),
        animation_nodes: animation_nodes
            .try_into()
            .expect("the character animation graph has exactly nine clips"),
    });

    // A deliberately plain local-only plinth for the sample row. The bottles
    // themselves are replicated ordinary containers; this is just enough
    // visual structure to keep them off the floor and easy to aim at.
    commands.spawn((
        Name::new("character effect sample rack"),
        Mesh3d(meshes.add(Cuboid::new(3.65, 0.80, 0.55))),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: Color::srgb(0.25, 0.29, 0.34),
            perceptual_roughness: 0.72,
            metallic: 0.18,
            ..default()
        })),
        Transform::from_translation(RACK_POSITION),
        crate::until_we_leave_the_lab(),
    ));
}

fn spawn_character_lab(mut commands: Commands, db: Res<ChemDb>) {
    commands.spawn((
        Name::new("chemical test subject"),
        TestSubject,
        Body::default(),
        Bloodstream::default(),
        Interactable::new("reset chemical test subject and samples"),
        Transform::from_translation(SUBJECT_POSITION)
            .with_rotation(Quat::from_rotation_y(-std::f32::consts::FRAC_PI_2)),
        Visibility::default(),
        Replicated,
        crate::until_we_leave_the_lab(),
    ));

    commands.spawn((
        Name::new("chemical locomotion test subject"),
        TestSubject,
        LocomotionPreview,
        LocomotionPreviewState {
            progress: 0.0,
            direction: 1.0,
        },
        Body::default(),
        Bloodstream::default(),
        Interactable::new("reset moving chemical test subject and samples"),
        Transform::from_translation(LOCOMOTION_START).with_rotation(locomotion_facing(1.0)),
        Visibility::default(),
        Replicated,
        crate::until_we_leave_the_lab(),
    ));

    commands.spawn((
        Name::new("character effect test syringe"),
        Container::new(ContainerKind::Syringe),
        Interactable::new("effect-test syringe"),
        Transform::from_xyz(11.90, SAMPLE_Y, -0.95),
        Replicated,
        crate::until_we_leave_the_lab(),
    ));

    let start_x = RACK_POSITION.x - 1.55;
    for (index, (key, effect)) in TEST_DOSES.into_iter().enumerate() {
        let reagent = db.reagent(key);
        let reagent_name = db.reagents.get(reagent).name.clone();
        let mut sample = Container::new(ContainerKind::Bottle);
        refill_sample(&mut sample, reagent, db.reagents.get(reagent).ph);
        commands.spawn((
            Name::new(format!("{reagent_name} effect sample")),
            sample,
            TestDose(reagent),
            Interactable::new(format!("{reagent_name} sample — {effect}")),
            Transform::from_xyz(start_x + index as f32 * 0.445, SAMPLE_Y, RACK_POSITION.z),
            Replicated,
            crate::until_we_leave_the_lab(),
        ));
    }
}

fn refill_sample(container: &mut Container, reagent: ReagentId, ph: f32) {
    container.solution = Solution::new(container.kind.capacity());
    let _ = container
        .solution
        .add_profiled(reagent, Units::whole(SAMPLE_UNITS), 1.0, ph);
}

fn dress_test_subjects(
    mut commands: Commands,
    assets: Option<Res<CharacterLabAssets>>,
    subjects: Query<Entity, Added<TestSubject>>,
) {
    let Some(assets) = assets else {
        return;
    };
    for subject in &subjects {
        commands
            .entity(subject)
            .insert_if_new(Visibility::default());
        commands.spawn((
            Name::new("first character model visual"),
            WorldAssetRoot(assets.subject.clone()),
            Transform::default(),
            Visibility::default(),
            TestSubjectVisual {
                subject,
                rest: Vec3::ZERO,
                face_variant: (subject.index().index() % 3) as u8,
            },
            ChildOf(subject),
        ));
    }
}

fn test_subject_face_variant(name: &str) -> Option<u8> {
    let prefix = name.strip_prefix("Face")?;
    prefix.get(..2)?.parse::<u8>().ok()?.checked_sub(1)
}

fn configure_test_subject_faces(
    mut commands: Commands,
    mut nodes: Query<(Entity, &Name, &mut Visibility), Without<TestSubjectFaceConfigured>>,
    parents: Query<&ChildOf>,
    visuals: Query<&TestSubjectVisual>,
) {
    for (entity, name, mut visibility) in &mut nodes {
        let Some(variant) = test_subject_face_variant(name.as_str()) else {
            continue;
        };
        let Some(subject) = test_subject_ancestor(entity, &parents, &visuals) else {
            continue;
        };
        let selected = visuals
            .iter()
            .find(|visual| visual.subject == subject)
            .is_some_and(|visual| visual.face_variant == variant);
        *visibility = if selected {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
        commands.entity(entity).insert(TestSubjectFaceConfigured);
    }
}

/// Imported scenes own their material handles several hierarchy levels below
/// `WorldAssetRoot`. Give each surface a private material before status tinting
/// it, so a future second mannequin cannot recolour the first one.
fn tag_test_subject_surfaces(
    mut commands: Commands,
    mut materials: ResMut<Assets<StandardMaterial>>,
    meshes: Query<
        (Entity, &MeshMaterial3d<StandardMaterial>),
        (With<Mesh3d>, Without<TestSubjectSurface>),
    >,
    parents: Query<&ChildOf>,
    visuals: Query<&TestSubjectVisual>,
) {
    for (entity, material) in &meshes {
        let Some(subject) = test_subject_ancestor(entity, &parents, &visuals) else {
            continue;
        };
        let Some(source) = materials.get(&material.0).cloned() else {
            continue;
        };
        let base_color = source.base_color;
        let base_alpha_mode = source.alpha_mode;
        commands.entity(entity).insert((
            MeshMaterial3d(materials.add(source)),
            TestSubjectSurface {
                subject,
                base_color,
                base_alpha_mode,
            },
        ));
    }
}

fn test_subject_ancestor(
    mut entity: Entity,
    parents: &Query<&ChildOf>,
    visuals: &Query<&TestSubjectVisual>,
) -> Option<Entity> {
    for _ in 0..64 {
        if let Ok(visual) = visuals.get(entity) {
            return Some(visual.subject);
        }
        entity = parents.get(entity).ok()?.parent();
    }
    None
}

fn attach_test_subject_animation(
    mut commands: Commands,
    assets: Option<Res<CharacterLabAssets>>,
    mut players: Query<(Entity, &mut AnimationPlayer), Added<AnimationPlayer>>,
    parents: Query<&ChildOf>,
    visuals: Query<&TestSubjectVisual>,
    locomotion_previews: Query<(), With<LocomotionPreview>>,
) {
    let Some(assets) = assets else {
        return;
    };
    for (entity, mut player) in &mut players {
        let Some(subject) = test_subject_ancestor(entity, &parents, &visuals) else {
            continue;
        };
        let initial = if locomotion_previews.contains(subject) {
            CharacterAnimation::Walk
        } else {
            CharacterAnimation::Idle
        };
        let mut transitions = AnimationTransitions::new();
        transitions
            .play(
                &mut player,
                assets.animation_nodes[initial as usize],
                Duration::ZERO,
            )
            .repeat();
        commands.entity(entity).insert((
            AnimationGraphHandle(assets.animation_graph.clone()),
            transitions,
            TestSubjectAnimationPlayer {
                subject,
                current: initial,
            },
        ));
    }
}

pub(crate) fn desired_character_animation(
    blood: &chem_sim::Bloodstream,
    locomotion_preview: bool,
) -> CharacterAnimation {
    if blood.incapacitated() || blood.appears_dead() {
        CharacterAnimation::Collapsed
    } else if locomotion_preview {
        if blood.status(StatusKind::Unsteady).intensity > 0.0
            || blood.status(StatusKind::Drunk).intensity > 0.0
        {
            CharacterAnimation::WalkDrunk
        } else {
            CharacterAnimation::Walk
        }
    } else if blood.status(StatusKind::Sedated).intensity > 0.0
        || blood.status(StatusKind::Sluggish).intensity > 0.0
    {
        CharacterAnimation::Sedated
    } else if blood.status(StatusKind::Unsteady).intensity > 0.0
        || blood.status(StatusKind::Drunk).intensity > 0.0
    {
        CharacterAnimation::Unsteady
    } else if blood.status(StatusKind::Hastened).intensity > 0.0
        || blood.status(StatusKind::Paranoid).intensity > 0.0
        || blood.status(StatusKind::Euphoric).intensity > 0.0
    {
        CharacterAnimation::Stimulated
    } else {
        CharacterAnimation::Idle
    }
}

pub(crate) fn character_animation_speed(
    blood: &chem_sim::Bloodstream,
    animation: CharacterAnimation,
) -> f32 {
    match animation {
        CharacterAnimation::Idle => 1.0,
        CharacterAnimation::Stimulated => {
            1.0 + blood.status(StatusKind::Hastened).intensity.min(3.0) * 0.16
        }
        CharacterAnimation::Sedated => {
            (0.82 - blood.status(StatusKind::Sedated).intensity * 0.08).max(0.45)
        }
        CharacterAnimation::Unsteady => {
            0.90 + blood.status(StatusKind::Unsteady).intensity.min(3.0) * 0.08
        }
        CharacterAnimation::Collapsed => 1.0,
        CharacterAnimation::Walk => {
            (1.0 - blood.status(StatusKind::Sedated).intensity.min(3.0) * 0.12).max(0.58)
        }
        CharacterAnimation::WalkDrunk => {
            (0.88 - blood.status(StatusKind::Drunk).intensity.min(3.0) * 0.06).max(0.58)
        }
        // Neither is ever selected while a chemical status would otherwise
        // apply — `drive_crew_animation` only reaches for them once
        // `desired_character_animation` has already settled on `Idle` — so
        // there is no bloodstream signal to scale either by.
        CharacterAnimation::Working => 1.0,
        CharacterAnimation::Sitting => 1.0,
    }
}

fn drive_test_subject_animation(
    assets: Option<Res<CharacterLabAssets>>,
    bloods: Query<&Bloodstream>,
    locomotion_previews: Query<(), With<LocomotionPreview>>,
    mut players: Query<(
        &mut AnimationPlayer,
        &mut AnimationTransitions,
        &mut TestSubjectAnimationPlayer,
    )>,
) {
    let Some(assets) = assets else {
        return;
    };
    for (mut player, mut transitions, mut controller) in &mut players {
        let Ok(blood) = bloods.get(controller.subject) else {
            continue;
        };
        let desired =
            desired_character_animation(&blood.0, locomotion_previews.contains(controller.subject));
        let node = assets.animation_nodes[desired as usize];
        let speed = character_animation_speed(&blood.0, desired);
        if desired != controller.current {
            transitions
                .play(&mut player, node, Duration::from_millis(320))
                .repeat()
                .set_speed(speed);
            controller.current = desired;
        } else if let Some(active) = player.animation_mut(node) {
            active.set_speed(speed);
        }
    }
}

fn pace_locomotion_previews(
    time: Res<Time>,
    mut previews: Query<
        (&Bloodstream, &mut Transform, &mut LocomotionPreviewState),
        With<LocomotionPreview>,
    >,
) {
    let lane_length = LOCOMOTION_START.distance(LOCOMOTION_END);
    for (blood, mut transform, mut state) in &mut previews {
        if blood.0.incapacitated() || blood.0.appears_dead() {
            continue;
        }

        let drunk = blood.0.status(StatusKind::Drunk).intensity
            + blood.0.status(StatusKind::Unsteady).intensity;
        let sedated = blood.0.status(StatusKind::Sedated).intensity
            + blood.0.status(StatusKind::Sluggish).intensity;
        let hastened = blood.0.status(StatusKind::Hastened).intensity;
        let speed =
            (0.82 + hastened.min(2.0) * 0.07 - drunk.min(3.0) * 0.07 - sedated.min(3.0) * 0.10)
                .clamp(0.34, 1.10);
        state.progress += state.direction * speed * time.delta_secs().min(0.1) / lane_length;
        if state.progress >= 1.0 {
            state.progress = 1.0;
            state.direction = -1.0;
        } else if state.progress <= 0.0 {
            state.progress = 0.0;
            state.direction = 1.0;
        }

        let mut position = LOCOMOTION_START.lerp(LOCOMOTION_END, state.progress);
        position.z += (state.progress * std::f32::consts::TAU * 2.0).sin() * drunk.min(3.0) * 0.018;
        transform.translation = position;
        transform.rotation = locomotion_facing(state.direction);
    }
}

// Blender's -Y front becomes +Z in the exported glTF. Rotate that axis
// toward the lane velocity instead of assuming Bevy's conventional -Z.
fn locomotion_facing(direction: f32) -> Quat {
    Quat::from_rotation_y(if direction > 0.0 {
        std::f32::consts::FRAC_PI_2
    } else {
        -std::f32::consts::FRAC_PI_2
    })
}

fn reset_subject_and_samples(
    db: Res<ChemDb>,
    mut requests: MessageReader<FromClient<InteractRequested>>,
    chemists: Query<(Entity, &Chemist)>,
    transforms: Query<&Transform>,
    mut subjects: Query<(&mut Body, &mut Bloodstream), With<TestSubject>>,
    mut samples: Query<(&TestDose, &mut Container)>,
) {
    for request in requests.read() {
        let Ok((mut body, mut blood)) = subjects.get_mut(request.target) else {
            continue;
        };
        let Some(chemist) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let (Ok(actor), Ok(target)) = (transforms.get(chemist), transforms.get(request.target))
        else {
            continue;
        };
        if !crate::interaction::authority_target_in_reach(
            actor.translation,
            target.translation,
            crate::interaction::REACH,
        ) {
            continue;
        }

        body.0 = chem_sim::Vitals::default();
        blood.0 = chem_sim::Bloodstream::default();
        for (dose, mut container) in &mut samples {
            refill_sample(&mut container, dose.0, db.reagents.get(dose.0).ph);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data() -> chem_sim::ChemData {
        chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .expect("character-lab chemistry should load")
    }

    #[test]
    fn every_effect_sample_names_a_real_reagent() {
        let db = data();
        for (key, _) in TEST_DOSES {
            assert!(db.reagents.id_of(key).is_some(), "unknown test dose {key}");
        }
    }

    #[test]
    fn exported_character_is_bevy_compatible_gltf() {
        let bytes = include_bytes!("../../assets/3dassets/glb/first_char_test.glb");
        let gltf =
            bevy::gltf::gltf::Gltf::from_slice(bytes).expect("test character must parse as glTF");
        let animation_names: Vec<_> = gltf
            .animations()
            .filter_map(|animation| animation.name())
            .collect();
        assert_eq!(
            animation_names,
            [
                "Collapsed",
                "Idle",
                "Sedated",
                "Sitting",
                "Stimulated",
                "Unsteady",
                "Walk",
                "WalkDrunk",
                "Working",
            ]
        );
        assert_eq!(gltf.skins().count(), 1, "the runtime mesh must stay rigged");
        assert_eq!(
            gltf.materials().count(),
            7,
            "the authored character palette must survive export"
        );
        let node_names: Vec<_> = gltf.nodes().filter_map(|node| node.name()).collect();
        for removed in [
            "VisorGeometry",
            "VisorStrap.L",
            "VisorStrap.R",
            "VisorStrap.Back",
        ] {
            assert!(
                !node_names.contains(&removed),
                "removed face obstruction {removed} must not return"
            );
        }
        for detail in [
            "ChemistryBadge",
            "Face01.Eye.L",
            "Face01.Eye.R",
            "Face01.Nose",
            "Face01.Mouth",
            "Face01.Hair.Cap",
            "Face02.Eye.L",
            "Face02.Eye.R",
            "Face02.Nose",
            "Face02.Mouth.L",
            "Face02.Mouth.R",
            "Face02.Hair.Cap",
            "Face03.Eye.L",
            "Face03.Eye.R",
            "Face03.Nose",
            "Face03.Mouth",
            "Face03.Hair.Crop",
            "Ear.L",
            "Ear.R",
            "Thumb.L",
            "Thumb.R",
            "LabCoatShell",
            "TrouserShell",
            "UndersleeveShell",
            "BootShell",
            "CoatPocket.L",
            "CoatPocket.R",
            "CoatPocketFlap.L",
            "CoatPocketFlap.R",
            "JacketCollarSeam.L",
            "JacketCollarSeam.R",
            "JacketPlacket.Top",
            "JacketPlacket.Bottom",
            "TrouserWaistband",
            "wrist.L",
            "wrist.R",
            "clavicle.L",
            "clavicle.R",
            "ankle.L",
            "ankle.R",
        ] {
            assert!(
                node_names.contains(&detail),
                "the authored rig node {detail} must survive export"
            );
        }
        for control in [
            "ik_hand.L",
            "ik_hand.R",
            "ik_foot.L",
            "ik_foot.R",
            "orient_foot.L",
            "orient_foot.R",
            "pole_elbow.L",
            "pole_elbow.R",
            "pole_knee.L",
            "pole_knee.R",
        ] {
            assert!(
                !node_names.contains(&control),
                "Blender-only control {control} must not enter the runtime skeleton"
            );
        }
    }

    #[test]
    fn every_department_export_keeps_the_shared_rig_and_three_faces() {
        let variants = [
            ("player", "ChemistryBadge"),
            ("medical", "MedicalBadge.Vertical"),
            ("security", "SecurityShoulder.L"),
            ("engineering", "EngineeringBeltBuckle"),
            ("cargo", "CargoHarnessBuckle"),
            ("service", "ServiceApron"),
        ];
        for (department, role_marker) in variants {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("assets")
                .join("3dassets")
                .join("glb")
                .join(format!("first_char_{department}.glb"));
            let bytes = std::fs::read(&path)
                .unwrap_or_else(|error| panic!("could not read {}: {error}", path.display()));
            let gltf = bevy::gltf::gltf::Gltf::from_slice(&bytes)
                .unwrap_or_else(|error| panic!("{} is not valid glTF: {error}", path.display()));
            assert_eq!(gltf.skins().count(), 1, "{department} lost the shared rig");
            assert_eq!(
                gltf.animations().count(),
                9,
                "{department} lost authored actions"
            );
            let nodes: Vec<_> = gltf.nodes().filter_map(|node| node.name()).collect();
            assert!(
                nodes.contains(&role_marker),
                "{department} export is missing {role_marker}"
            );
            for face in [
                "Face01.Eye.L",
                "Face01.Hair.Cap",
                "Face02.Eye.L",
                "Face02.Hair.Cap",
                "Face03.Eye.L",
                "Face03.Hair.Crop",
            ] {
                assert!(nodes.contains(&face), "{department} is missing {face}");
            }
            for ear in ["Ear.L", "Ear.R"] {
                assert!(nodes.contains(&ear), "{department} is missing {ear}");
            }
            assert!(
                !nodes.contains(&"VisorGeometry"),
                "{department} brought the removed visor back"
            );
        }
    }

    #[test]
    fn drug_statuses_select_the_matching_authored_animation() {
        let mut blood = chem_sim::Bloodstream::default();
        assert_eq!(
            desired_character_animation(&blood, false),
            CharacterAnimation::Idle
        );
        blood.add_status(StatusKind::Hastened, 5.0, 1.0);
        assert_eq!(
            desired_character_animation(&blood, false),
            CharacterAnimation::Stimulated
        );
        blood.add_status(StatusKind::Unsteady, 5.0, 1.0);
        assert_eq!(
            desired_character_animation(&blood, false),
            CharacterAnimation::Unsteady
        );
        blood.add_status(StatusKind::Sedated, 5.0, 1.0);
        assert_eq!(
            desired_character_animation(&blood, false),
            CharacterAnimation::Sedated,
            "sedation wins when several drugs are active"
        );
        blood.add_status(StatusKind::Sedated, 5.0, 4.0);
        assert_eq!(
            desired_character_animation(&blood, false),
            CharacterAnimation::Collapsed,
            "incapacitation switches to the authored collapsed posture"
        );
    }

    #[test]
    fn moving_preview_uses_walk_and_drunk_walk() {
        let mut blood = chem_sim::Bloodstream::default();
        assert_eq!(
            desired_character_animation(&blood, true),
            CharacterAnimation::Walk
        );
        blood.add_status(StatusKind::Drunk, 5.0, 1.0);
        assert_eq!(
            desired_character_animation(&blood, true),
            CharacterAnimation::WalkDrunk
        );
        blood.add_status(StatusKind::Sedated, 5.0, 4.0);
        assert_eq!(
            desired_character_animation(&blood, true),
            CharacterAnimation::Collapsed,
            "incapacitation still overrides locomotion"
        );
    }

    #[test]
    fn moving_preview_faces_its_direction_of_travel() {
        for (direction, travel) in [(1.0, Vec3::X), (-1.0, Vec3::NEG_X)] {
            let model_forward = locomotion_facing(direction) * Vec3::Z;
            assert!(
                model_forward.dot(travel) > 0.999,
                "exported +Z model forward must follow lane velocity"
            );
        }
    }

    #[test]
    fn a_sample_is_small_enough_for_one_non_overdose_test() {
        let db = data();
        for (key, _) in TEST_DOSES {
            let reagent = db.reagents.get(db.reagent(key));
            if let Some(overdose) = reagent.overdose {
                assert!(
                    Units::whole(SAMPLE_UNITS) < overdose,
                    "{key} sample starts at its overdose threshold"
                );
            }
        }
    }
}
