//! Local recording controls: F8 HUD, F9 camera, F7 panels, F6 clean camera,
//! F10 title, Shift+Numpad1..4 save views, Numpad1..4 restore views.
use crate::player::PlayerCamera;
use crate::AppState;
use bevy::input::mouse::MouseMotion;
use bevy::prelude::*;
use std::collections::HashMap;
#[cfg(debug_assertions)]
pub(crate) mod asset_tour;
#[cfg(debug_assertions)]
pub(crate) mod trailer;
pub struct CapturePlugin;
impl Plugin for CapturePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CaptureState>();
        #[cfg(debug_assertions)]
        asset_tour::register(app);
        #[cfg(debug_assertions)]
        app.add_plugins(trailer::TrailerPlugin)
            .add_systems(OnEnter(AppState::Playing), build_title)
            .add_systems(
                Update,
                (controls, fly_camera)
                    .chain()
                    .after(crate::player::follow_chemist)
                    .run_if(in_state(AppState::Playing)),
            )
            .add_systems(
                PostUpdate,
                (title_visibility, hide_hud)
                    .chain()
                    .before(bevy::camera::visibility::VisibilitySystems::VisibilityPropagate)
                    .run_if(in_state(AppState::Playing)),
            )
            .add_systems(OnExit(AppState::Playing), restore_capture);
    }
}
#[derive(Component)]
pub(crate) struct CapturePanel;
#[derive(Component)]
struct CaptureTitle;
#[derive(Resource)]
pub(crate) struct CaptureState {
    pub(crate) free_camera: bool,
    hide_hud: bool,
    keep_panels: bool,
    clean_camera: bool,
    title: bool,
    yaw: f32,
    pitch: f32,
    views: [Option<Transform>; 4],
    hidden: HashMap<Entity, Visibility>,
}
impl Default for CaptureState {
    fn default() -> Self {
        Self {
            free_camera: false,
            hide_hud: false,
            keep_panels: true,
            clean_camera: true,
            title: false,
            yaw: 0.0,
            pitch: 0.0,
            views: [None; 4],
            hidden: HashMap::new(),
        }
    }
}
pub fn player_follows_camera(state: Option<Res<CaptureState>>) -> bool {
    !state.is_some_and(|s| s.free_camera)
}
pub(crate) fn clean_camera(state: Option<&CaptureState>) -> bool {
    state.is_some_and(|s| s.free_camera && s.clean_camera)
}
fn controls(
    keys: Res<ButtonInput<KeyCode>>,
    mut state: ResMut<CaptureState>,
    mut cameras: Query<&mut Transform, With<PlayerCamera>>,
) {
    if keys.just_pressed(KeyCode::F8) {
        state.hide_hud = !state.hide_hud;
    }
    if keys.just_pressed(KeyCode::F7) {
        state.keep_panels = !state.keep_panels;
    }
    if keys.just_pressed(KeyCode::F6) {
        state.clean_camera = !state.clean_camera;
    }
    if keys.just_pressed(KeyCode::F10) {
        state.title = !state.title;
    }
    let Ok(mut camera) = cameras.single_mut() else {
        return;
    };
    if keys.just_pressed(KeyCode::F9) {
        state.free_camera = !state.free_camera;
        if state.free_camera {
            orient(&mut state, &camera);
        }
    }
    for (slot, key) in [
        KeyCode::Numpad1,
        KeyCode::Numpad2,
        KeyCode::Numpad3,
        KeyCode::Numpad4,
    ]
    .into_iter()
    .enumerate()
    {
        if !keys.just_pressed(key) {
            continue;
        }
        if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
            state.views[slot] = Some(*camera);
            info!("capture view {} saved", slot + 1);
        } else if let Some(view) = state.views[slot] {
            *camera = view;
            state.free_camera = true;
            orient(&mut state, &view);
        }
    }
}
fn orient(state: &mut CaptureState, transform: &Transform) {
    let (yaw, pitch, _) = transform.rotation.to_euler(EulerRot::YXZ);
    state.yaw = yaw;
    state.pitch = pitch;
}
fn hide_hud(
    mut state: ResMut<CaptureState>,
    mut roots: Query<
        (Entity, &mut Visibility, Has<CapturePanel>),
        (With<Node>, Without<ChildOf>, Without<CaptureTitle>),
    >,
) {
    let hide = state.hide_hud || state.free_camera || state.title;
    let panels = state.keep_panels && !state.free_camera && !state.title;
    state.hidden.retain(|entity, _| roots.contains(*entity));
    for (entity, mut visibility, panel) in &mut roots {
        if hide && !(panels && panel) {
            state.hidden.entry(entity).or_insert(*visibility);
            if *visibility != Visibility::Hidden {
                *visibility = Visibility::Hidden;
            }
        } else if let Some(previous) = state.hidden.remove(&entity) {
            *visibility = previous;
        }
    }
}
fn restore_capture(mut state: ResMut<CaptureState>, mut visibility: Query<&mut Visibility>) {
    for (entity, previous) in state.hidden.drain() {
        if let Ok(mut current) = visibility.get_mut(entity) {
            *current = previous;
        }
    }
    *state = CaptureState::default();
}
fn build_title(mut commands: Commands) {
    commands
        .spawn((
            CaptureTitle,
            Node {
                width: percent(100),
                height: percent(100),
                position_type: PositionType::Absolute,
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                row_gap: px(18),
                ..default()
            },
            Visibility::Hidden,
            GlobalZIndex(2000),
            BackgroundColor(Color::srgba(0.02, 0.03, 0.04, 0.5)),
            crate::until_we_leave_the_lab(),
        ))
        .with_children(|parent| {
            parent.spawn((
                Text::new("SPACE CHEM"),
                TextFont::from_font_size(86.0),
                TextColor(Color::srgb(0.90, 0.94, 0.96)),
            ));
            parent.spawn((
                Text::new("Wishlist on Steam"),
                TextFont::from_font_size(30.0),
                TextColor(Color::srgb(0.90, 0.94, 0.96)),
            ));
        });
}
fn title_visibility(
    state: Res<CaptureState>,
    mut titles: Query<&mut Visibility, With<CaptureTitle>>,
) {
    for mut v in &mut titles {
        *v = if state.title {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        };
    }
}
pub(crate) fn fly_camera(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut motion: MessageReader<MouseMotion>,
    mut state: ResMut<CaptureState>,
    mut cameras: Query<&mut Transform, With<PlayerCamera>>,
) {
    let delta: Vec2 = motion.read().map(|m| m.delta).sum();
    if !state.free_camera {
        return;
    }
    let Ok(mut camera) = cameras.single_mut() else {
        return;
    };
    state.yaw -= delta.x * 0.0025;
    state.pitch = (state.pitch - delta.y * 0.0025).clamp(-1.54, 1.54);
    camera.rotation = Quat::from_euler(EulerRot::YXZ, state.yaw, state.pitch, 0.0);
    let axis = |positive, negative| {
        (keys.pressed(positive) as u8 as f32) - (keys.pressed(negative) as u8 as f32)
    };
    let direction = Vec3::new(
        axis(KeyCode::KeyD, KeyCode::KeyA),
        axis(KeyCode::Space, KeyCode::ControlLeft),
        axis(KeyCode::KeyS, KeyCode::KeyW),
    )
    .normalize_or_zero();
    let speed = if keys.pressed(KeyCode::ShiftLeft) {
        12.0
    } else if keys.pressed(KeyCode::AltLeft) {
        0.75
    } else {
        4.0
    };
    camera.translation += Quat::from_rotation_y(state.yaw) * direction * speed * time.delta_secs();
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hide_restore_preserves_hidden_roots_and_new_panels() {
        let mut app = App::new();
        app.init_resource::<CaptureState>()
            .add_systems(Update, hide_hud);
        let hidden = app
            .world_mut()
            .spawn((Node::default(), Visibility::Hidden))
            .id();
        let visible = app
            .world_mut()
            .spawn((Node::default(), Visibility::Visible))
            .id();
        app.world_mut().resource_mut::<CaptureState>().hide_hud = true;
        app.update();
        let panel = app
            .world_mut()
            .spawn((Node::default(), CapturePanel, Visibility::Inherited))
            .id();
        app.update();
        assert_eq!(
            *app.world().get::<Visibility>(visible).unwrap(),
            Visibility::Hidden
        );
        assert_eq!(
            *app.world().get::<Visibility>(panel).unwrap(),
            Visibility::Inherited
        );
        app.world_mut().resource_mut::<CaptureState>().hide_hud = false;
        app.update();
        assert_eq!(
            *app.world().get::<Visibility>(hidden).unwrap(),
            Visibility::Hidden
        );
        assert_eq!(
            *app.world().get::<Visibility>(visible).unwrap(),
            Visibility::Visible
        );
    }
    #[test]
    fn hud_hiding_keeps_real_camera_effects() {
        let state = CaptureState {
            hide_hud: true,
            ..default()
        };
        assert!(!state.free_camera);
        assert!(!clean_camera(Some(&state)));
    }
}
