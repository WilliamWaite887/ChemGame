//! The menu's small, presentation-only chemistry bay.
//!
//! This is intentionally not the playable station.  Loading the whole map just
//! to stand behind five buttons would run unrelated scene hooks and make menu
//! startup depend on gameplay state.  The backdrop instead composes two of the
//! shipped showroom assets with a few bounded, code-owned effects.

use bevy::prelude::*;

use crate::AppState;

const STATION_KIT: &str = "3dassets/station_starter_kit/glb";
const REACTION_ORIGIN: Vec3 = Vec3::new(1.35, 0.0, 0.35);
const LIQUID_CENTER: Vec3 = Vec3::new(1.30, 0.91, 1.04);
const BUBBLE_COUNT: usize = 18;

#[derive(Resource)]
pub(super) struct MenuSceneAssets {
    bay: Handle<WorldAsset>,
    reaction_chamber: Handle<WorldAsset>,
}

#[derive(Component)]
pub(super) struct MenuEnvironment;

#[derive(Component)]
pub(super) struct MenuUiCamera;

#[derive(Component)]
pub(super) struct MenuSceneCamera {
    base: Vec3,
    target: Vec3,
}

#[derive(Component)]
pub(super) struct MenuLiquid;

#[derive(Component)]
pub(super) struct MenuBubble {
    phase: f32,
    radius: f32,
}

type MenuBubbleQuery<'w, 's> = Query<
    'w,
    's,
    (&'static MenuBubble, &'static mut Transform),
    (Without<MenuLiquid>, Without<MenuSceneCamera>),
>;

pub(super) fn load_assets(mut commands: Commands, assets: Res<AssetServer>) {
    let scene = |file: &'static str| {
        assets.load(GltfAssetLabel::Scene(0).from_asset(format!("{STATION_KIT}/{file}")))
    };
    commands.insert_resource(MenuSceneAssets {
        bay: scene("department_chemistry_starter.glb"),
        reaction_chamber: scene("machine_reaction_chamber.glb"),
    });
}

/// Builds the backdrop once and leaves it in place while screens change or a
/// connection attempt temporarily enters `AppState::Connecting`.
pub(super) fn ensure_environment(
    mut commands: Commands,
    assets: Option<Res<MenuSceneAssets>>,
    existing: Query<Entity, With<MenuEnvironment>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if !existing.is_empty() {
        return;
    }
    let Some(assets) = assets else {
        return;
    };

    let root = commands
        .spawn((
            Name::new("main menu chemistry vignette"),
            Transform::default(),
            Visibility::default(),
            MenuEnvironment,
        ))
        .id();

    commands.spawn((
        Name::new("main menu chemistry bay"),
        WorldAssetRoot(assets.bay.clone()),
        Transform::from_xyz(1.4, -0.03, -0.35),
        Visibility::default(),
        ChildOf(root),
    ));
    commands.spawn((
        Name::new("main menu reaction chamber"),
        WorldAssetRoot(assets.reaction_chamber.clone()),
        Transform::from_translation(REACTION_ORIGIN),
        Visibility::default(),
        ChildOf(root),
    ));

    let liquid_material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.12, 0.72, 0.78, 0.72),
        emissive: LinearRgba::new(0.05, 2.2, 2.7, 1.0),
        alpha_mode: AlphaMode::Blend,
        perceptual_roughness: 0.18,
        metallic: 0.0,
        ..default()
    });
    commands.spawn((
        Name::new("main menu chamber liquid"),
        Mesh3d(meshes.add(Sphere::new(0.36))),
        MeshMaterial3d(liquid_material.clone()),
        Transform::from_translation(LIQUID_CENTER).with_scale(Vec3::new(1.0, 0.72, 0.42)),
        Visibility::default(),
        MenuLiquid,
        ChildOf(root),
    ));

    let bubble_mesh = meshes.add(Sphere::new(0.032));
    let bubble_material = materials.add(StandardMaterial {
        base_color: Color::srgba(0.70, 0.95, 1.0, 0.62),
        emissive: LinearRgba::new(0.4, 1.5, 1.8, 1.0),
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        ..default()
    });
    for index in 0..BUBBLE_COUNT {
        let fraction = index as f32 / BUBBLE_COUNT as f32;
        let phase = fraction * std::f32::consts::TAU;
        let radius = 0.08 + (index % 5) as f32 * 0.045;
        commands.spawn((
            Name::new("main menu chamber bubble"),
            Mesh3d(bubble_mesh.clone()),
            MeshMaterial3d(bubble_material.clone()),
            Transform::from_translation(LIQUID_CENTER),
            Visibility::default(),
            MenuBubble { phase, radius },
            ChildOf(root),
        ));
    }

    commands.spawn((
        PointLight {
            color: Color::srgb(0.52, 0.88, 1.0),
            intensity: 260_000.0,
            range: 14.0,
            shadow_maps_enabled: true,
            ..default()
        },
        Transform::from_xyz(3.2, 4.2, 4.8),
        ChildOf(root),
    ));
    commands.spawn((
        PointLight {
            color: Color::srgb(1.0, 0.58, 0.24),
            intensity: 105_000.0,
            range: 10.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(-1.6, 2.0, 2.2),
        ChildOf(root),
    ));
    commands.spawn((
        PointLight {
            color: Color::srgb(0.22, 0.55, 0.82),
            intensity: 85_000.0,
            range: 9.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(3.8, 2.2, -2.4),
        ChildOf(root),
    ));

    // Framed for the uncovered right-hand two thirds of a widescreen window:
    // close enough that the bay reads as a place rather than a model, and
    // aimed just left of it so the reaction chamber lands off-centre right.
    let camera_base = Vec3::new(4.65, 2.72, 5.55);
    let camera_target = Vec3::new(0.55, 1.03, 0.52);
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: 0,
            clear_color: Color::srgb(0.025, 0.038, 0.055).into(),
            ..default()
        },
        Transform::from_translation(camera_base).looking_at(camera_target, Vec3::Y),
        MenuSceneCamera {
            base: camera_base,
            target: camera_target,
        },
        ChildOf(root),
    ));
    commands.spawn((
        Camera2d,
        Camera {
            order: 1,
            clear_color: bevy::camera::ClearColorConfig::None,
            ..default()
        },
        IsDefaultUiCamera,
        MenuUiCamera,
        ChildOf(root),
    ));

    commands.insert_resource(GlobalAmbientLight {
        color: Color::srgb(0.43, 0.52, 0.66),
        brightness: 260.0,
        ..default()
    });
}

pub(super) fn animate_environment(
    time: Res<Time>,
    mut camera: Query<(&MenuSceneCamera, &mut Transform), Without<MenuLiquid>>,
    mut liquid: Query<&mut Transform, (With<MenuLiquid>, Without<MenuSceneCamera>)>,
    mut bubbles: MenuBubbleQuery,
) {
    let seconds = time.elapsed_secs();
    for (scene, mut transform) in &mut camera {
        let drift = Vec3::new(
            (seconds * 0.10).sin() * 0.075,
            (seconds * 0.13).cos() * 0.035,
            0.0,
        );
        transform.translation = scene.base + drift;
        transform.look_at(scene.target + drift * 0.25, Vec3::Y);
    }
    for mut transform in &mut liquid {
        let pulse = 1.0 + (seconds * 0.85).sin() * 0.025;
        transform.scale = Vec3::new(pulse, 0.72 / pulse, 0.42 * pulse);
        transform.rotation = Quat::from_rotation_y(seconds * 0.08)
            * Quat::from_rotation_z((seconds * 0.31).sin() * 0.045);
    }
    for (bubble, mut transform) in &mut bubbles {
        let travel = (seconds * (0.10 + bubble.radius * 0.22) + bubble.phase).fract();
        let orbit = seconds * 0.27 + bubble.phase;
        transform.translation = LIQUID_CENTER
            + Vec3::new(
                orbit.cos() * bubble.radius,
                -0.23 + travel * 0.50,
                orbit.sin() * bubble.radius * 0.36 + 0.02,
            );
        let scale = 0.55 + travel * 0.8;
        transform.scale = Vec3::splat(scale);
    }
}

pub(super) fn clear_environment(
    mut commands: Commands,
    roots: Query<Entity, With<MenuEnvironment>>,
) {
    for root in &roots {
        commands.entity(root).despawn();
    }
}

pub(super) fn menu_or_connecting(state: Res<State<AppState>>) -> bool {
    matches!(state.get(), AppState::MainMenu | AppState::Connecting)
}
