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
                tag_test_subject_surfaces.after(dress_test_subjects),
                attach_test_subject_animation.after(dress_test_subjects),
                drive_test_subject_animation.after(attach_test_subject_animation),
                reset_subject_and_samples.run_if(is_authority),
            )
                .run_if(in_state(AppState::Playing)),
        );
    }
}

/// Replicated identity for the stationary development body. The mesh remains
/// local presentation, like chemist and crew meshes.
#[derive(Component, Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct TestSubject;

/// The movable visual child. Effects animate this, never the authoritative
/// root used by reach checks and replication.
#[derive(Component)]
pub(crate) struct TestSubjectVisual {
    pub(crate) subject: Entity,
    pub(crate) rest: Vec3,
}

/// A material-bearing mesh nested inside the imported GLB scene.
#[derive(Component)]
pub(crate) struct TestSubjectSurface {
    pub(crate) subject: Entity,
    pub(crate) base_color: Color,
}

#[derive(Component)]
struct TestDose(ReagentId);

#[derive(Resource)]
struct CharacterLabAssets {
    subject: Handle<WorldAsset>,
    animation_graph: Handle<AnimationGraph>,
    animation_nodes: [AnimationNodeIndex; 4],
}

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
struct TestSubjectAnimationPlayer {
    subject: Entity,
    current: CharacterAnimation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CharacterAnimation {
    Idle = 0,
    Stimulated = 1,
    Sedated = 2,
    Unsteady = 3,
}

fn load_character_lab_assets(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut animation_graphs: ResMut<Assets<AnimationGraph>>,
) {
    // Blender exports Actions alphabetically: Idle, Sedated, Stimulated,
    // Unsteady. Keep the gameplay-facing node order explicit here.
    let (animation_graph, animation_nodes) = AnimationGraph::from_clips([
        asset_server.load(GltfAssetLabel::Animation(0).from_asset(SUBJECT_MODEL)),
        asset_server.load(GltfAssetLabel::Animation(2).from_asset(SUBJECT_MODEL)),
        asset_server.load(GltfAssetLabel::Animation(1).from_asset(SUBJECT_MODEL)),
        asset_server.load(GltfAssetLabel::Animation(3).from_asset(SUBJECT_MODEL)),
    ]);
    commands.insert_resource(CharacterLabAssets {
        subject: asset_server.load(GltfAssetLabel::Scene(0).from_asset(SUBJECT_MODEL)),
        animation_graph: animation_graphs.add(animation_graph),
        animation_nodes: animation_nodes
            .try_into()
            .expect("the character animation graph has exactly four clips"),
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
        refill_sample(&mut sample, reagent);
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

fn refill_sample(container: &mut Container, reagent: ReagentId) {
    container.solution = Solution::new(container.kind.capacity());
    let _ = container.solution.add(reagent, Units::whole(SAMPLE_UNITS));
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
            },
            ChildOf(subject),
        ));
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
        commands.entity(entity).insert((
            MeshMaterial3d(materials.add(source)),
            TestSubjectSurface {
                subject,
                base_color,
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
) {
    let Some(assets) = assets else {
        return;
    };
    for (entity, mut player) in &mut players {
        let Some(subject) = test_subject_ancestor(entity, &parents, &visuals) else {
            continue;
        };
        let mut transitions = AnimationTransitions::new();
        transitions
            .play(
                &mut player,
                assets.animation_nodes[CharacterAnimation::Idle as usize],
                Duration::ZERO,
            )
            .repeat();
        commands.entity(entity).insert((
            AnimationGraphHandle(assets.animation_graph.clone()),
            transitions,
            TestSubjectAnimationPlayer {
                subject,
                current: CharacterAnimation::Idle,
            },
        ));
    }
}

fn desired_character_animation(blood: &chem_sim::Bloodstream) -> CharacterAnimation {
    if blood.status(StatusKind::Sedated).intensity > 0.0
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

fn character_animation_speed(blood: &chem_sim::Bloodstream, animation: CharacterAnimation) -> f32 {
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
    }
}

fn drive_test_subject_animation(
    assets: Option<Res<CharacterLabAssets>>,
    bloods: Query<&Bloodstream>,
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
        let desired = desired_character_animation(&blood.0);
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

fn reset_subject_and_samples(
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
        if !crate::interaction::authority_target_in_reach(actor.translation, target.translation) {
            continue;
        }

        body.0 = chem_sim::Vitals::default();
        blood.0 = chem_sim::Bloodstream::default();
        for (dose, mut container) in &mut samples {
            refill_sample(&mut container, dose.0);
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
            ["Idle", "Sedated", "Stimulated", "Unsteady"]
        );
        assert_eq!(gltf.skins().count(), 1, "the runtime mesh must stay rigged");
        assert_eq!(
            gltf.materials().count(),
            6,
            "the authored character palette must survive export"
        );
        let node_names: Vec<_> = gltf.nodes().filter_map(|node| node.name()).collect();
        for detail in ["VisorGeometry", "ChemistryBadge", "Thumb.L", "Thumb.R"] {
            assert!(
                node_names.contains(&detail),
                "the second-pass detail {detail} must survive export"
            );
        }
    }

    #[test]
    fn drug_statuses_select_the_matching_authored_animation() {
        let mut blood = chem_sim::Bloodstream::default();
        assert_eq!(
            desired_character_animation(&blood),
            CharacterAnimation::Idle
        );
        blood.add_status(StatusKind::Hastened, 5.0, 1.0);
        assert_eq!(
            desired_character_animation(&blood),
            CharacterAnimation::Stimulated
        );
        blood.add_status(StatusKind::Unsteady, 5.0, 1.0);
        assert_eq!(
            desired_character_animation(&blood),
            CharacterAnimation::Unsteady
        );
        blood.add_status(StatusKind::Sedated, 5.0, 1.0);
        assert_eq!(
            desired_character_animation(&blood),
            CharacterAnimation::Sedated,
            "sedation wins when several drugs are active"
        );
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
