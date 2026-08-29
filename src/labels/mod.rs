//! Writing on the bottle.
//!
//! The chemist's oldest trick, and the one thing this lab could not do: a pill
//! marked *Painkiller* that is really something else entirely. Until now every
//! container announced its true contents to everyone who looked at it, which
//! made the chemist structurally incapable of lying.
//!
//! # What a label is, and is not
//!
//! **A label never changes what a chemical does.** Not one reaction, dose,
//! grade or metabolism path reads it — those all keep reading
//! [`Container::solution`](crate::containers::Container::solution), exactly as
//! they always have. A label changes what is *displayed*, and what someone
//! inspecting the bottle *believes*. That separation is the whole safety
//! property of the feature, and
//! `a_label_never_changes_what_a_chemical_does` pins it.
//!
//! # Two different deceptions
//!
//! Worth keeping straight, because they are independent and only one of them
//! involves lying to a person.
//!
//! 1. **The drug that genuinely works.** Methamphetamine really is a
//!    stimulant — see the double-life note in `chem.reagents.ron` — so a
//!    stimulant order filled with it satisfies the person who asked. No label
//!    required, and nobody is fooled. The price is the baggage: they get
//!    hooked, and `addiction::notice_the_high` turns an addict standing high
//!    in front of an officer into rising suspicion.
//! 2. **The label.** What keeps that bottle off the charge sheet when Security
//!    sweeps the lab, and what lets you hand someone something that does *not*
//!    do what they asked for at all.
//!
//! # Typing in a first-person game
//!
//! [`InteractionMode::Labelling`] exists for the same reason `ReadingBook`
//! does — so the mode *"inherits the cursor and camera handling machines
//! already have"*. That is not cosmetic: every gameplay keybind in the game
//! (move, look, sprint, drop, drink, apply, use) already gates on
//! `InteractionMode::is_roaming`, so becoming a non-roaming mode disables all
//! of them for free and stops WASD walking the chemist across the lab while
//! they write "Painkiller".
//!
//! The one keybind that was *not* gated is the reference book, which
//! `interaction::panel_input` reads before any mode check — so typing a `b`
//! would have opened the book mid-word. That guard lives there, beside the
//! read, rather than here.
//!
//! Typing itself follows `menu::type_address`: driven by the keypress's own
//! `text` rather than its key code, *"so the layout the player actually has
//! decides what a key produces"*.

use bevy::ecs::entity::MapEntities;
use bevy::input::keyboard::KeyboardInput;
use bevy::input::ButtonState;
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::containers::HeldBy;
use crate::interaction::InteractionMode;
use crate::machines::chemist_entity;
use crate::net::is_authority;
use crate::player::{Chemist, LocalPlayer};
use crate::AppState;

/// Longest a label may be.
///
/// Bounded for the same reason `menu::type_address` bounds its own field: an
/// unbounded string is a way to make the layout jump, and this one also
/// crosses the wire from a client that can say anything.
pub const MAX_LABEL: usize = 28;

pub struct LabelPlugin;

impl Plugin for LabelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<LabelDraft>()
            .add_mapped_client_message::<LabelRequested>(Channel::Ordered)
            .add_systems(
                Update,
                (
                    // Local: what this player is typing is their own business
                    // until they press Enter.
                    (start_labelling, type_label, draw_draft).chain(),
                    handle_label_request.run_if(is_authority),
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// What is written on a container.
///
/// Replicated, because the whole point is that *other people* read it — a
/// label only the host could see would deceive nobody and would be the silent
/// host/guest split every co-op bug in this project has been.
///
/// Absent rather than empty when unlabelled, so "has a label" is a component
/// query and not a string comparison.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Label(pub String);

/// A chemist writing on something they are holding.
///
/// Names the container rather than trusting the sender's mode, and is
/// validated authority-side against who actually holds it — a client is free
/// to name any entity at all.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct LabelRequested {
    #[entities]
    pub container: Entity,
    pub text: String,
}

/// What the local player has typed so far, before they commit it.
///
/// Local-only and deliberately not replicated: a half-written label is not
/// something the other chemist needs to watch appear a character at a time.
#[derive(Resource, Default)]
pub struct LabelDraft {
    pub text: String,
}

/// Opens the label field on whatever is in hand.
fn start_labelling(
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<crate::settings::Settings>,
    mut draft: ResMut<LabelDraft>,
    held: Query<(Entity, &HeldBy, Option<&Label>)>,
    mut players: Query<(Entity, &mut InteractionMode), With<LocalPlayer>>,
) {
    if !keys.just_pressed(settings.bindings.label) {
        return;
    }
    for (player, mut mode) in &mut players {
        if !mode.is_roaming() {
            continue;
        }
        let Some((container, _, existing)) = held.iter().find(|(_, holder, _)| holder.0 == player)
        else {
            continue;
        };
        // Seeded with whatever it already says, so correcting a label is an
        // edit rather than a retype.
        draft.text = existing.map(|label| label.0.clone()).unwrap_or_default();
        *mode = InteractionMode::Labelling(container);
    }
}

/// Collects typing into [`LabelDraft`], and commits it on Enter.
///
/// Escape is deliberately *not* handled here: `interaction::panel_input`
/// already takes Escape out of every non-roaming mode back to the floor, so
/// handling it here as well would be two systems racing to own one key.
fn type_label(
    mut typed: MessageReader<KeyboardInput>,
    mut draft: ResMut<LabelDraft>,
    mut requests: MessageWriter<LabelRequested>,
    mut players: Query<&mut InteractionMode, With<LocalPlayer>>,
) {
    let Some(mut mode) = players.iter_mut().next() else {
        typed.clear();
        return;
    };
    let InteractionMode::Labelling(container) = *mode else {
        // Not typing. Drop the frame's keys rather than banking them for the
        // next time the field opens.
        typed.clear();
        return;
    };

    for key in typed.read() {
        if key.state != ButtonState::Pressed {
            continue;
        }
        match key.key_code {
            KeyCode::Backspace => {
                draft.text.pop();
            }
            KeyCode::Enter | KeyCode::NumpadEnter => {
                requests.write(LabelRequested {
                    container,
                    text: draft.text.trim().to_string(),
                });
                *mode = InteractionMode::Roaming;
                return;
            }
            KeyCode::Escape => {}
            _ => {
                let Some(text) = &key.text else {
                    continue;
                };
                // Printable characters only. Without this, Tab and Enter
                // arrive as whitespace in the middle of the word — the same
                // filter `menu::type_address` applies for the same reason.
                for character in text.chars().filter(|c| !c.is_control()) {
                    if draft.text.len() < MAX_LABEL {
                        draft.text.push(character);
                    }
                }
            }
        }
    }
}

#[derive(Component)]
struct DraftField;

/// Shows the field while it is open, and takes it away when it is not.
fn draw_draft(
    mut commands: Commands,
    draft: Res<LabelDraft>,
    players: Query<&InteractionMode, With<LocalPlayer>>,
    field: Query<Entity, With<DraftField>>,
    mut text: Query<&mut Text, With<DraftField>>,
) {
    let writing = players
        .iter()
        .any(|mode| matches!(mode, InteractionMode::Labelling(_)));

    if !writing {
        for entity in &field {
            commands.entity(entity).despawn();
        }
        return;
    }

    let shown = format!("{}_", draft.text);
    if let Some(mut existing) = text.iter_mut().next() {
        if existing.0 != shown {
            existing.0 = shown;
        }
        return;
    }

    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            bottom: percent(34),
            width: percent(100),
            flex_direction: FlexDirection::Column,
            align_items: AlignItems::Center,
            row_gap: px(6),
            ..default()
        },
        GlobalZIndex(50),
        crate::until_we_leave_the_lab(),
        children![
            (
                Node {
                    padding: UiRect::axes(px(16), px(8)),
                    border_radius: BorderRadius::all(px(6)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.05, 0.06, 0.08, 0.94)),
                children![(
                    Text::new(String::new()),
                    TextFont::from_font_size(20.0),
                    TextColor(crate::ui::TEXT),
                    DraftField,
                )],
            ),
            (
                Text::new("Enter to write it on   ·   Esc to leave it as it was"),
                TextFont::from_font_size(12.0),
                TextColor(crate::ui::TEXT_DIM),
            ),
        ],
    ));
}

/// Writes the label, once the authority is satisfied the sender is holding it.
fn handle_label_request(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<LabelRequested>>,
    chemists: Query<(Entity, &Chemist)>,
    held: Query<&HeldBy>,
) {
    for request in requests.read() {
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        // Holding it is the whole authorization: no reach check is needed for
        // something already in your hands, and a forged request naming
        // somebody else's beaker fails here.
        if held.get(request.container).map(|holder| holder.0) != Ok(player) {
            continue;
        }
        let text: String = request
            .text
            .chars()
            .filter(|c| !c.is_control())
            .take(MAX_LABEL)
            .collect();
        let text = text.trim();
        if text.is_empty() {
            // Clearing the field peels the label off rather than leaving an
            // empty one, so `Has<Label>` stays a truthful question.
            commands.entity(request.container).remove::<Label>();
        } else {
            commands
                .entity(request.container)
                .insert(Label(text.to_string()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::containers::{Container, ContainerKind};

    fn labelling_app() -> (App, Entity, Entity) {
        let mut app = App::new();
        app.add_message::<FromClient<LabelRequested>>()
            .add_systems(Update, handle_label_request);
        let player = app
            .world_mut()
            .spawn(Chemist {
                client: ClientId::Server,
            })
            .id();
        let bottle = app
            .world_mut()
            .spawn((Container::new(ContainerKind::Bottle), HeldBy(player)))
            .id();
        (app, player, bottle)
    }

    fn write(app: &mut App, container: Entity, text: &str) {
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: LabelRequested {
                container,
                text: text.to_string(),
            },
        });
        app.update();
    }

    #[test]
    fn a_label_never_changes_what_a_chemical_does() {
        // The safety property the whole feature rests on. A label is display
        // and belief; the solution is truth. If writing on a bottle could
        // touch its contents, every reaction, dose and grade in the game would
        // become a lie rather than only the bottle.
        let (mut app, _, bottle) = labelling_app();
        let (before_solution, before_kind) = {
            let container = app.world().get::<Container>(bottle).unwrap();
            (container.solution.clone(), container.kind)
        };

        write(&mut app, bottle, "Painkiller");

        let after = app.world().get::<Container>(bottle).unwrap();
        assert_eq!(
            after.solution, before_solution,
            "writing a label altered the contents"
        );
        assert_eq!(after.kind, before_kind);
        assert_eq!(
            app.world().get::<Label>(bottle),
            Some(&Label("Painkiller".to_string()))
        );
    }

    #[test]
    fn you_cannot_write_on_someone_elses_bottle() {
        // The message names a container and a client may name anything at all.
        // Holding it is the authorization.
        let (mut app, _, bottle) = labelling_app();
        let connection = app.world_mut().spawn_empty().id();
        let stranger = app
            .world_mut()
            .spawn(Chemist {
                client: ClientId::Client(connection),
            })
            .id();
        app.world_mut().entity_mut(bottle).insert(HeldBy(stranger));

        write(&mut app, bottle, "Painkiller");

        assert!(
            app.world().get::<Label>(bottle).is_none(),
            "a chemist labelled a bottle out of someone else's hand"
        );
    }

    #[test]
    fn clearing_the_field_peels_the_label_off() {
        // Rather than leaving an empty one behind, so `Has<Label>` stays a
        // truthful question for everything that asks it.
        let (mut app, _, bottle) = labelling_app();
        write(&mut app, bottle, "Painkiller");
        assert!(app.world().get::<Label>(bottle).is_some());

        write(&mut app, bottle, "   ");

        assert!(app.world().get::<Label>(bottle).is_none());
    }

    #[test]
    fn a_forged_label_is_cut_down_to_size() {
        // `MAX_LABEL` is enforced client-side while typing, which is exactly
        // why it has to be enforced here too: the client doing the typing is
        // not necessarily the client sending the message.
        let (mut app, _, bottle) = labelling_app();
        write(&mut app, bottle, &"A".repeat(MAX_LABEL * 4));

        let label = app.world().get::<Label>(bottle).unwrap();
        assert_eq!(label.0.chars().count(), MAX_LABEL);
    }

    #[test]
    fn control_characters_never_reach_a_label() {
        // A label is drawn into the world and into the HUD. Newlines and tabs
        // in one are a layout break at best.
        let (mut app, _, bottle) = labelling_app();
        write(&mut app, bottle, "Pain\nkil\tler");

        assert_eq!(
            app.world().get::<Label>(bottle).unwrap().0,
            "Painkiller".to_string()
        );
    }
}
