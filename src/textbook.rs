//! Short, spoiler-free system reference. Reading never touches Knowledge.
use crate::{
    settings::{PauseScreen, Paused, Settings},
    ui::{button, label, row, ScrollPane, PANEL_BG, SECTION_BG, TEXT, TEXT_DIM},
    AppState,
};
use bevy::{
    input::{keyboard::KeyboardInput, ButtonState},
    prelude::*,
};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Debug, Deserialize)]
pub struct Article {
    pub id: String,
    pub group: String,
    pub title: String,
    pub keywords: String,
    pub summary: String,
    pub steps: Vec<String>,
    pub watch: String,
    pub experiment: String,
    pub diagram: String,
    pub related: Vec<String>,
    pub source: String,
}
#[derive(Resource)]
pub struct Textbook(pub Vec<Article>);
impl Default for Textbook {
    fn default() -> Self {
        let articles: Vec<Article> = ron::from_str(include_str!("../assets/data/lab.textbook.ron"))
            .expect("valid laboratory textbook");
        assert!(
            articles.iter().all(|article| !article.source.is_empty()),
            "textbook pages need a source map"
        );
        Self(articles)
    }
}
#[derive(Resource, Default)]
pub struct TextbookView {
    pub article: Option<usize>,
    pub query: String,
    pub searching: bool,
    pub visited: std::collections::HashSet<String>,
    offsets: HashMap<Option<usize>, f32>,
    revision: u64,
}
#[derive(Component)]
struct TextbookRoot;
#[derive(Component)]
struct ArticleScroll(Option<usize>);
#[derive(Component, Clone)]
enum Action {
    Article(usize),
    Index,
    Search,
    Clear,
    Trouble,
    Glossary,
}

pub struct TextbookPlugin;
impl Plugin for TextbookPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Textbook>()
            .init_resource::<TextbookView>()
            .add_systems(
                Update,
                (remember_scroll, clicks, search_input, draw)
                    .chain()
                    .run_if(in_state(AppState::Playing)),
            )
            .add_systems(OnExit(AppState::Playing), reset);
    }
}
fn reset(mut view: ResMut<TextbookView>) {
    *view = default();
}

pub fn open(view: &mut TextbookView, screen: &mut PauseScreen) {
    *screen = if view.article.is_some() {
        PauseScreen::TextbookArticle
    } else {
        PauseScreen::Textbook
    };
    view.searching = false;
}
pub fn expand(text: &str, settings: &Settings) -> String {
    let mut text = text.to_owned();
    for (token, key) in [
        ("{interact}", settings.bindings.interact),
        ("{inspect}", settings.bindings.inspect),
        ("{book}", settings.bindings.book),
        ("{social}", settings.bindings.social),
        ("{apply}", settings.bindings.apply),
        ("{drink}", settings.bindings.drink),
        ("{drop}", settings.bindings.drop),
    ] {
        text = text.replace(token, &crate::settings::key_label(key));
    }
    text
}
fn remember_scroll(
    panes: Query<(&ArticleScroll, &ScrollPosition)>,
    mut view: ResMut<TextbookView>,
) {
    for (pane, position) in &panes {
        view.bypass_change_detection()
            .offsets
            .insert(pane.0, position.y);
    }
}
fn clicks(
    buttons: Query<(&Interaction, &Action), Changed<Interaction>>,
    mut view: ResMut<TextbookView>,
    mut screen: ResMut<PauseScreen>,
    book: Res<Textbook>,
) {
    for (interaction, action) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        view.searching = false;
        match action {
            Action::Article(index) => {
                view.article = Some(*index);
                view.visited.insert(book.0[*index].id.clone());
                *screen = PauseScreen::TextbookArticle;
            }
            Action::Index => {
                *screen = PauseScreen::Textbook;
            }
            Action::Search => {
                view.searching = true;
            }
            Action::Clear => {
                view.query.clear();
            }
            Action::Trouble => {
                view.query = "troubleshoot".into();
                *screen = PauseScreen::Textbook;
            }
            Action::Glossary => {
                view.query = "glossary".into();
                *screen = PauseScreen::Textbook;
            }
        }
        view.revision += 1;
    }
}
fn search_input(
    mut events: MessageReader<KeyboardInput>,
    mut view: ResMut<TextbookView>,
    screen: Res<PauseScreen>,
    paused: Res<Paused>,
) {
    for event in events.read() {
        if !paused.0
            || *screen != PauseScreen::Textbook
            || !view.searching
            || event.state != ButtonState::Pressed
        {
            continue;
        }
        match event.key_code {
            KeyCode::Enter | KeyCode::Escape => view.searching = false,
            KeyCode::Backspace => {
                view.query.pop();
            }
            _ => {
                if let Some(text) = &event.text {
                    for c in text.chars().filter(|c| !c.is_control()) {
                        if view.query.chars().count() < 64 {
                            view.query.push(c);
                        }
                    }
                }
            }
        }
        view.revision += 1;
    }
}
fn matches(article: &Article, query: &str) -> bool {
    let haystack =
        format!("{} {} {}", article.title, article.keywords, article.group).to_lowercase();
    query
        .split_whitespace()
        .all(|word| haystack.contains(&word.to_lowercase()))
}

type DrawSignature = (PauseScreen, u64, Option<usize>, bool);
#[allow(clippy::too_many_arguments)]
fn draw(
    mut commands: Commands,
    paused: Res<Paused>,
    screen: Res<PauseScreen>,
    mut view: ResMut<TextbookView>,
    book: Res<Textbook>,
    settings: Res<Settings>,
    mode: Option<Res<crate::net::LaunchMode>>,
    roots: Query<Entity, With<TextbookRoot>>,
    mut previous: Local<Option<DrawSignature>>,
) {
    let visible = paused.0
        && matches!(
            *screen,
            PauseScreen::Textbook | PauseScreen::TextbookArticle
        );
    let signature = (*screen, view.revision, view.article, visible);
    if *previous == Some(signature) && (!visible || !roots.is_empty()) {
        return;
    }
    *previous = Some(signature);
    for root in &roots {
        commands.entity(root).try_despawn();
    }
    if !visible {
        return;
    }
    let selected = if *screen == PauseScreen::TextbookArticle {
        view.article
    } else {
        None
    };
    if let Some(index) = selected {
        view.visited.insert(book.0[index].id.clone());
    }
    let multiplayer =
        mode.is_some_and(|mode| !matches!(*mode, crate::net::LaunchMode::Singleplayer));
    commands
        .spawn((
            TextbookRoot,
            crate::until_we_leave_the_lab(),
            GlobalZIndex(110),
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(PANEL_BG),
        ))
        .with_children(|root| {
            root.spawn(Node {
                width: px(880),
                max_width: percent(96),
                height: percent(92),
                padding: UiRect::all(px(22)),
                row_gap: px(12),
                flex_direction: FlexDirection::Column,
                ..default()
            })
            .with_children(|panel| {
                panel.spawn(label("LAB TEXTBOOK", 24.0, TEXT));
                panel.spawn(label(
                    if multiplayer {
                        "The lab keeps running while you read."
                    } else {
                        "The lab is paused while you read."
                    },
                    14.0,
                    TEXT_DIM,
                ));
                panel.spawn(row()).with_children(|r| {
                    r.spawn(button("Contents", Action::Index));
                    r.spawn(button("What went wrong?", Action::Trouble));
                    r.spawn(button("Glossary", Action::Glossary));
                    r.spawn(button("Pause menu", crate::settings::PauseAction::Back));
                });
                if selected.is_none() {
                    panel.spawn(row()).with_children(|r| {
                        r.spawn(button(
                            format!(
                                "{}{}",
                                if view.searching { "Type: " } else { "Search: " },
                                if view.query.is_empty() {
                                    "titles or keywords"
                                } else {
                                    &view.query
                                }
                            ),
                            Action::Search,
                        ));
                        r.spawn(button("Clear", Action::Clear));
                    });
                }
                panel
                    .spawn((
                        Node {
                            flex_grow: 1.0,
                            min_height: px(0),
                            width: percent(100),
                            overflow: Overflow::scroll_y(),
                            flex_direction: FlexDirection::Column,
                            row_gap: px(12),
                            padding: UiRect::right(px(14)),
                            ..default()
                        },
                        ScrollPane,
                        ArticleScroll(selected),
                        ScrollPosition(Vec2::new(
                            0.0,
                            *view.offsets.get(&selected).unwrap_or(&0.0),
                        )),
                    ))
                    .with_children(|body| {
                        if let Some(index) = selected {
                            let a = &book.0[index];
                            body.spawn(label(&a.title, 22.0, TEXT));
                            body.spawn(label(expand(&a.summary, &settings), 17.0, TEXT));
                            for (i, step) in a.steps.iter().enumerate() {
                                body.spawn(label(
                                    format!("{}. {}", i + 1, expand(step, &settings)),
                                    16.0,
                                    TEXT,
                                ));
                            }
                            body.spawn(label(
                                format!("Watch: {}", expand(&a.watch, &settings)),
                                16.0,
                                TEXT,
                            ));
                            if !a.experiment.is_empty() {
                                body.spawn(label(
                                    format!("Try this: {}", expand(&a.experiment, &settings)),
                                    16.0,
                                    TEXT,
                                ));
                            }
                            if !a.diagram.is_empty() {
                                body.spawn((
                                    Node {
                                        padding: UiRect::all(px(14)),
                                        ..default()
                                    },
                                    BackgroundColor(SECTION_BG),
                                ))
                                .with_children(|d| {
                                    d.spawn(label(&a.diagram, 18.0, TEXT));
                                });
                            }
                            for id in &a.related {
                                if let Some((i, related)) =
                                    book.0.iter().enumerate().find(|(_, p)| &p.id == id)
                                {
                                    body.spawn(button(
                                        format!("Read: {}", related.title),
                                        Action::Article(i),
                                    ));
                                }
                            }
                        } else {
                            let mut group = "";
                            let mut found = false;
                            if view.query == "troubleshoot" {
                                for (symptom, id) in [
                                    ("Nothing reacted.", "temperature"),
                                    ("Ingredients remain.", "ratios"),
                                    ("The order was rejected.", "orders"),
                                    ("The product is impure.", "purity"),
                                    ("Treatment made things worse.", "dose"),
                                    ("A mixture burned or exploded.", "materials"),
                                ] {
                                    if let Some(index) = book.0.iter().position(|a| a.id == id) {
                                        body.spawn(button(symptom, Action::Article(index)));
                                    }
                                }
                                return;
                            }
                            for (i, a) in book
                                .0
                                .iter()
                                .enumerate()
                                .filter(|(_, a)| matches(a, &view.query))
                            {
                                found = true;
                                if group != a.group {
                                    group = &a.group;
                                    body.spawn(label(group, 18.0, TEXT_DIM));
                                }
                                body.spawn(button(&a.title, Action::Article(i)));
                            }
                            if !found {
                                body.spawn(label(
                                    "No matching topics. Try fewer words or Clear.",
                                    16.0,
                                    TEXT_DIM,
                                ));
                            }
                        }
                    });
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn textbook_content_is_short_linked_and_complete() {
        let book = Textbook::default();
        assert_eq!(book.0.len(), 23);
        let ids: std::collections::HashSet<_> = book.0.iter().map(|a| &a.id).collect();
        assert_eq!(ids.len(), book.0.len());
        for a in &book.0 {
            let text = format!(
                "{} {} {} {} {}",
                a.summary,
                a.steps.join(" "),
                a.watch,
                a.experiment,
                a.diagram
            );
            assert!(text.split_whitespace().count() <= 140, "{} too long", a.id);
            assert!(!a.summary.is_empty() && !a.watch.is_empty() && !a.source.is_empty());
            assert!(a.steps.len() <= 3);
            for id in &a.related {
                assert!(ids.contains(id), "bad link {id}");
            }
            assert!(!text.contains("{unknown}"));
        }
    }
    #[test]
    fn search_and_bindings_are_live() {
        let b = Textbook::default();
        assert!(b.0.iter().any(|a| matches(a, "nothing reacted")));
        assert!(b.0.iter().any(|a| matches(a, "dose")));
        let mut s = Settings::default();
        s.bindings.inspect = KeyCode::KeyJ;
        assert_eq!(expand("Press {inspect}", &s), "Press J");
    }

    #[test]
    fn escape_visits_contents_then_pause_before_resuming() {
        let mut app = App::new();
        app.init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<ButtonInput<MouseButton>>()
            .init_resource::<Settings>()
            .init_resource::<crate::settings::Rebinding>()
            .init_resource::<crate::interaction::CursorReleased>()
            .insert_resource(Paused(true))
            .insert_resource(PauseScreen::TextbookArticle)
            .add_message::<crate::interaction::LeaveMachineRequested>()
            .add_systems(Update, crate::interaction::panel_input);
        app.world_mut()
            .spawn(bevy::window::CursorOptions::default());
        for wanted in [PauseScreen::Textbook, PauseScreen::Root, PauseScreen::Root] {
            let mut keys = app.world_mut().resource_mut::<ButtonInput<KeyCode>>();
            keys.reset_all();
            keys.press(KeyCode::Escape);
            app.update();
            assert_eq!(*app.world().resource::<PauseScreen>(), wanted);
            if wanted == PauseScreen::Textbook {
                assert!(app.world().resource::<Paused>().0);
            }
        }
        assert!(!app.world().resource::<Paused>().0);
    }

    #[test]
    fn reading_preserves_each_articles_scroll_and_resolves_all_control_tokens() {
        let mut app = App::new();
        app.init_resource::<TextbookView>()
            .add_systems(Update, remember_scroll);
        let pane = app
            .world_mut()
            .spawn((
                ArticleScroll(Some(1)),
                ScrollPosition(Vec2::new(0.0, 143.0)),
            ))
            .id();
        app.update();
        app.world_mut()
            .entity_mut(pane)
            .insert((ArticleScroll(Some(2)), ScrollPosition(Vec2::new(0.0, 38.0))));
        app.update();
        let view = app.world().resource::<TextbookView>();
        assert_eq!(view.offsets[&Some(1)], 143.0);
        assert_eq!(view.offsets[&Some(2)], 38.0);
        let settings = Settings::default();
        for article in Textbook::default().0 {
            for text in
                article
                    .steps
                    .iter()
                    .chain([&article.summary, &article.watch, &article.experiment])
            {
                assert!(
                    !expand(text, &settings).contains('{'),
                    "unresolved token on {}",
                    article.id
                );
            }
        }
    }
}
