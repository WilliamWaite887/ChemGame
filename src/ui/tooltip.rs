//! Hover-only explanatory bubbles shared by the recipe book's icon language.

use accesskit::{Node as AccessNode, Role};
use bevy::a11y::AccessibilityNode;
use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;

use super::{PANEL_BG, TEXT, TEXT_DIM};

const TOOLTIP_DELAY_SECONDS: f32 = 0.2;
const TOOLTIP_WIDTH: f32 = 286.0;
const TOOLTIP_ESTIMATED_HEIGHT: f32 = 104.0;
const TOOLTIP_CURSOR_GAP: f32 = 14.0;

#[derive(Component, Clone, Debug)]
pub(super) struct TooltipSource {
    pub(super) title: String,
    pub(super) body: String,
}

impl TooltipSource {
    pub(super) fn new(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
        }
    }
}

#[derive(Component)]
pub(super) struct TooltipRoot;

#[derive(Resource, Default)]
pub(super) struct TooltipState {
    hovered: Option<Entity>,
    elapsed: f32,
    visible_for: Option<Entity>,
}

pub(super) fn accessibility_label(label: impl Into<String>, role: Role) -> AccessibilityNode {
    let mut node = AccessNode::new(role);
    node.set_label(label.into());
    AccessibilityNode::from(node)
}

fn clear_tooltip(
    commands: &mut Commands,
    roots: &Query<Entity, With<TooltipRoot>>,
    state: &mut TooltipState,
) {
    for root in roots {
        commands.entity(root).despawn();
    }
    state.visible_for = None;
}

/// Shows at most one bubble, only after a stable 200 ms hover. The bubble is
/// pick-through, so appearing under the pointer cannot manufacture a hover
/// exit/re-entry flicker on its own source.
pub(super) fn update_tooltips(
    mut commands: Commands,
    time: Res<Time>,
    windows: Query<&Window>,
    mut wheel: MessageReader<MouseWheel>,
    sources: Query<(Entity, &Interaction, &TooltipSource)>,
    roots: Query<Entity, With<TooltipRoot>>,
    mut state: ResMut<TooltipState>,
) {
    if wheel.read().next().is_some() {
        state.hovered = None;
        state.elapsed = 0.0;
        clear_tooltip(&mut commands, &roots, &mut state);
        return;
    }

    let hovered = sources
        .iter()
        .find(|(_, interaction, _)| **interaction == Interaction::Hovered)
        .map(|(entity, _, _)| entity);

    if hovered != state.hovered {
        state.hovered = hovered;
        state.elapsed = 0.0;
        clear_tooltip(&mut commands, &roots, &mut state);
    }

    let Some(source_entity) = hovered else {
        return;
    };
    if state.visible_for == Some(source_entity) {
        return;
    }

    state.elapsed += time.delta_secs();
    if state.elapsed < TOOLTIP_DELAY_SECONDS {
        return;
    }

    let Ok((_, _, source)) = sources.get(source_entity) else {
        return;
    };
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };

    let left = if cursor.x + TOOLTIP_CURSOR_GAP + TOOLTIP_WIDTH <= window.width() {
        cursor.x + TOOLTIP_CURSOR_GAP
    } else {
        (cursor.x - TOOLTIP_CURSOR_GAP - TOOLTIP_WIDTH).max(8.0)
    };
    let top = if cursor.y + TOOLTIP_CURSOR_GAP + TOOLTIP_ESTIMATED_HEIGHT <= window.height() {
        cursor.y + TOOLTIP_CURSOR_GAP
    } else {
        (cursor.y - TOOLTIP_CURSOR_GAP - TOOLTIP_ESTIMATED_HEIGHT).max(8.0)
    };

    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(left),
                top: px(top),
                width: px(TOOLTIP_WIDTH),
                padding: UiRect::axes(px(12), px(10)),
                flex_direction: FlexDirection::Column,
                row_gap: px(4),
                border: UiRect::all(px(1)),
                border_radius: BorderRadius::all(px(6)),
                ..default()
            },
            BackgroundColor(PANEL_BG),
            BorderColor::all(Color::srgb(0.32, 0.55, 0.70)),
            GlobalZIndex(950),
            Pickable::IGNORE,
            TooltipRoot,
            super::super::until_we_leave_the_lab(),
        ))
        .with_children(|bubble| {
            bubble.spawn((
                Text::new(source.title.clone()),
                TextFont::from_font_size(14.0),
                TextColor(TEXT),
                Pickable::IGNORE,
            ));
            bubble.spawn((
                Text::new(source.body.clone()),
                TextFont::from_font_size(12.0),
                TextColor(TEXT_DIM),
                Pickable::IGNORE,
            ));
        });
    state.visible_for = Some(source_entity);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn tooltip_delay_is_short_but_not_instant() {
        assert_eq!(TOOLTIP_DELAY_SECONDS, 0.2);
    }

    #[test]
    fn edge_placement_has_a_safe_fallback() {
        let cursor = Vec2::new(1272.0, 712.0);
        let left = (cursor.x - TOOLTIP_CURSOR_GAP - TOOLTIP_WIDTH).max(8.0);
        let top = (cursor.y - TOOLTIP_CURSOR_GAP - TOOLTIP_ESTIMATED_HEIGHT).max(8.0);
        assert!(left >= 8.0);
        assert!(top >= 8.0);
        assert!(left + TOOLTIP_WIDTH <= 1280.0);
        assert!(top + TOOLTIP_ESTIMATED_HEIGHT <= 720.0);
    }

    #[test]
    fn hover_shows_one_bubble_and_exit_clears_it() {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<TooltipState>()
            .add_message::<MouseWheel>()
            .add_systems(Update, update_tooltips);
        let mut window = Window::default();
        window.set_cursor_position(Some(Vec2::new(320.0, 240.0)));
        app.world_mut().spawn(window);
        let source = app
            .world_mut()
            .spawn((
                Interaction::Hovered,
                TooltipSource::new("Catalyst", "Required, but not consumed."),
            ))
            .id();
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(Duration::from_millis(210));
        app.update();

        assert_eq!(
            app.world_mut()
                .query_filtered::<Entity, With<TooltipRoot>>()
                .iter(app.world())
                .count(),
            1
        );

        app.world_mut().entity_mut(source).insert(Interaction::None);
        app.update();
        assert_eq!(
            app.world_mut()
                .query_filtered::<Entity, With<TooltipRoot>>()
                .iter(app.world())
                .count(),
            0
        );
    }
}
