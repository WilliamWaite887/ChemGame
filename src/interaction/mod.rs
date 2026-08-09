//! Looking at things and using them.
//!
//! Interaction never mutates machine state directly. It raycasts, works out
//! what is under the crosshair, and emits [`InteractRequested`]. Systems that
//! own the state apply it. That indirection is what lets co-op replicate
//! actions later instead of rewriting every panel.

use bevy::prelude::*;

use crate::lab::MachineScreen;
use crate::machines::Machine;
use crate::player::{LocalPlayer, PlayerCamera};
use crate::AppState;

/// How far a chemist can reach, in metres.
const REACH: f32 = 2.6;

pub struct InteractionPlugin;

impl Plugin for InteractionPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<InteractRequested>()
            .add_systems(OnEnter(AppState::Playing), spawn_hud)
            .add_systems(
                Update,
                (update_focus, request_interaction, update_prompt)
                    .chain()
                    .run_if(in_state(AppState::Playing)),
            );
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
}

impl InteractionMode {
    pub fn is_roaming(&self) -> bool {
        matches!(self, InteractionMode::Roaming)
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

/// A player pressed use on something. Handled in M3.
#[derive(Message)]
pub struct InteractRequested {
    pub player: Entity,
    pub target: Entity,
}

/// Casts a ray from each player's camera and records what it hits.
fn update_focus(
    mut ray_cast: MeshRayCast,
    cameras: Query<(&GlobalTransform, &ChildOf), With<PlayerCamera>>,
    mut players: Query<&mut Focus>,
    interactables: Query<(), With<Interactable>>,
    screens: Query<(), With<MachineScreen>>,
) {
    for (camera_transform, child_of) in &cameras {
        let Ok(mut focus) = players.get_mut(child_of.parent()) else {
            continue;
        };

        let ray = Ray3d::new(camera_transform.translation(), camera_transform.forward());
        // Machine screens sit a hair proud of their casing, so without this
        // filter every machine would be permanently blocked by its own screen.
        // Everything else stays in the cast, so walls and benches still
        // occlude properly.
        let filter = |entity: Entity| !screens.contains(entity);
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
    for (player, focus, mode) in &players {
        if !mode.is_roaming() {
            continue;
        }
        if let Some(target) = focus.target {
            requests.write(InteractRequested { player, target });
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
    prompt: Single<&mut Text, With<InteractionPrompt>>,
) {
    let mut text = prompt.into_inner();
    let message = players
        .iter()
        .find_map(|(player, focus)| {
            let target = focus.target?;
            let label = &interactables.get(target).ok()?.label;
            // Occupied machines still show a prompt, just an unusable one, so
            // the other chemist's activity is visible rather than mysterious.
            Some(match machines.get(target) {
                Ok(machine) if !machine.available_to(player) => {
                    format!("{label} — in use")
                }
                _ => format!("[E]  {label}"),
            })
        })
        .unwrap_or_default();

    if text.0 != message {
        text.0 = message;
    }
}
