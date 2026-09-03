//! Short, spoiler-free system reference. Reading never touches Knowledge.
use crate::{
    settings::{PauseScreen, Paused, Settings},
    ui::{
        icon_image, icons::BookIcon, icons::BookIconAssets, button, label, row, ScrollPane,
        BOOK_ACCENT, BUTTON_IDLE, FONT_SIZE_BODY, FONT_SIZE_CAPTION, FONT_SIZE_LABEL_SMALL,
        FONT_SIZE_TITLE, GOOD_TEXT, PANEL_BG, SECTION_BG, TEXT, TEXT_DIM, TIP_BG, TIP_BORDER,
        WARNING_BG, WARNING_BORDER,
    },
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
    /// A [`BookIcon`] slug (e.g. `"temperature"`) picking the icon shown on
    /// this article's index card and title. Empty means [`icon_for`] should
    /// pick a sensible default from the article's group/id instead — most
    /// entries leave this empty; only set it where the group default would be
    /// wrong or bland.
    #[serde(default)]
    pub icon: String,
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
/// The icon shown on an article's index card and title: an explicit
/// `icon:` slug from the RON entry if it set one, otherwise a sensible
/// default guessed from its group (with a few per-id refinements for
/// articles a bare group icon would leave bland or ambiguous).
fn icon_for(article: &Article) -> BookIcon {
    if !article.icon.is_empty() {
        if let Some(icon) = BookIcon::ALL.into_iter().find(|i| i.slug() == article.icon) {
            return icon;
        }
    }
    match article.id.as_str() {
        "chemmaster5000" => return BookIcon::Inputs,
        "chamber" => return BookIcon::ReactionChamber,
        "mixer" => return BookIcon::MixingChamber,
        "analyzer" => return BookIcon::Research,
        "grinder" => return BookIcon::Utility,
        "hplc" => return BookIcon::Purity,
        "storage" => return BookIcon::RawReagent,
        "temperature" => return BookIcon::Temperature,
        "buffers" => return BookIcon::Ph,
        "purity" => return BookIcon::Purity,
        "catalysts" => return BookIcon::Catalyst,
        "agitation" => return BookIcon::Agitate,
        "dose" => return BookIcon::Heal,
        "hazards" | "materials" => return BookIcon::Harm,
        "glossary" => return BookIcon::Key,
        _ => {}
    }
    group_icon(&article.group)
}
/// The icon shown beside a section header in the index.
fn group_icon(group: &str) -> BookIcon {
    match group {
        "Getting started" => BookIcon::Book,
        "Equipment" => BookIcon::ChemMaster,
        "Chemistry concepts" => BookIcon::Research,
        "Delivery and recovery" => BookIcon::Orders,
        "Reference" => BookIcon::Key,
        _ => BookIcon::Book,
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

/// A small bordered square holding one icon, tinted `color` — the badge used
/// beside section headers, article titles, and callout boxes.
fn icon_chip(icons: &BookIconAssets, icon: BookIcon, color: Color, border: Color) -> impl Bundle {
    (
        Node {
            width: px(30),
            height: px(30),
            min_width: px(30),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border: UiRect::all(px(1)),
            border_radius: BorderRadius::all(px(6)),
            ..default()
        },
        BackgroundColor(SECTION_BG),
        BorderColor::all(border),
        children![icon_image(icons, icon, 17.0, color)],
    )
}
/// A group heading in the index: an icon chip beside an accent-colored label,
/// replacing the old bare dim-gray text so groups read as distinct sections
/// rather than more list text.
fn section_header(parent: &mut ChildSpawnerCommands, icons: &BookIconAssets, group: &str) {
    parent
        .spawn(Node {
            flex_direction: FlexDirection::Row,
            align_items: AlignItems::Center,
            column_gap: px(8),
            margin: UiRect::top(px(6)),
            ..default()
        })
        .with_children(|header| {
            header.spawn(icon_chip(
                icons,
                group_icon(group),
                BOOK_ACCENT,
                Color::srgba(0.25, 0.31, 0.38, 0.78),
            ));
            header.spawn(label(group, 18.0, BOOK_ACCENT));
        });
}
/// One topic in the index (or a "Read: ..." related link): a bordered card
/// with the article's icon and title, replacing the flat gray `button()`.
/// Carries the same `Action::Article` component the click handler expects.
fn topic_card(
    parent: &mut ChildSpawnerCommands,
    icons: &BookIconAssets,
    index: usize,
    article: &Article,
    visited: bool,
) {
    parent
        .spawn((
            Button,
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::Center,
                column_gap: px(10),
                padding: UiRect::axes(px(12), px(10)),
                margin: UiRect::all(px(3)),
                border: UiRect::all(px(1)),
                border_radius: BorderRadius::all(px(6)),
                ..default()
            },
            BackgroundColor(BUTTON_IDLE),
            BorderColor::all(Color::srgba(0.25, 0.31, 0.38, 0.78)),
            Action::Article(index),
        ))
        .with_children(|card| {
            card.spawn(icon_image(icons, icon_for(article), 20.0, BOOK_ACCENT));
            card.spawn(label(&article.title, FONT_SIZE_TITLE, TEXT));
            if visited {
                card.spawn(label("Read", FONT_SIZE_CAPTION, GOOD_TEXT));
            }
        });
}
/// A tinted, bordered box for a "pay attention" or "worth trying" aside —
/// gives `Watch:`/`Try this:` a distinct look instead of plain body text.
fn callout(
    parent: &mut ChildSpawnerCommands,
    icons: &BookIconAssets,
    icon: BookIcon,
    heading: &str,
    body: String,
    bg: Color,
    border: Color,
) {
    parent
        .spawn((
            Node {
                flex_direction: FlexDirection::Row,
                align_items: AlignItems::FlexStart,
                column_gap: px(10),
                padding: UiRect::all(px(12)),
                border: UiRect::all(px(1)),
                border_radius: BorderRadius::all(px(6)),
                ..default()
            },
            BackgroundColor(bg),
            BorderColor::all(border),
        ))
        .with_children(|c| {
            c.spawn(icon_image(icons, icon, 18.0, border));
            c.spawn(Node {
                flex_direction: FlexDirection::Column,
                row_gap: px(2),
                flex_grow: 1.0,
                min_width: px(0),
                ..default()
            })
            .with_children(|col| {
                col.spawn(label(heading, FONT_SIZE_CAPTION, border));
                col.spawn(label(body, FONT_SIZE_BODY, TEXT));
            });
        });
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
    icons: Res<BookIconAssets>,
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
                            body.spawn(row()).with_children(|title_row| {
                                title_row.spawn(icon_chip(
                                    &icons,
                                    icon_for(a),
                                    BOOK_ACCENT,
                                    Color::srgba(0.25, 0.31, 0.38, 0.78),
                                ));
                                title_row.spawn(label(&a.title, 22.0, TEXT));
                            });
                            body.spawn(label(expand(&a.summary, &settings), FONT_SIZE_TITLE, TEXT));
                            if !a.steps.is_empty() {
                                body.spawn((
                                    Node {
                                        flex_direction: FlexDirection::Column,
                                        row_gap: px(8),
                                        padding: UiRect::all(px(12)),
                                        border_radius: BorderRadius::all(px(6)),
                                        ..default()
                                    },
                                    BackgroundColor(SECTION_BG),
                                ))
                                .with_children(|steps| {
                                    for (i, step) in a.steps.iter().enumerate() {
                                        steps.spawn(row()).with_children(|r| {
                                            r.spawn((
                                                Node {
                                                    width: px(22),
                                                    height: px(22),
                                                    min_width: px(22),
                                                    justify_content: JustifyContent::Center,
                                                    align_items: AlignItems::Center,
                                                    border_radius: BorderRadius::all(px(11)),
                                                    ..default()
                                                },
                                                BackgroundColor(BOOK_ACCENT),
                                                children![label(
                                                    (i + 1).to_string(),
                                                    FONT_SIZE_LABEL_SMALL,
                                                    PANEL_BG
                                                )],
                                            ));
                                            r.spawn(label(
                                                expand(step, &settings),
                                                FONT_SIZE_BODY,
                                                TEXT,
                                            ));
                                        });
                                    }
                                });
                            }
                            callout(
                                body,
                                &icons,
                                BookIcon::Critical,
                                "Watch",
                                expand(&a.watch, &settings),
                                WARNING_BG,
                                WARNING_BORDER,
                            );
                            if !a.experiment.is_empty() {
                                callout(
                                    body,
                                    &icons,
                                    BookIcon::Research,
                                    "Try this",
                                    expand(&a.experiment, &settings),
                                    TIP_BG,
                                    TIP_BORDER,
                                );
                            }
                            if !a.diagram.is_empty() {
                                body.spawn((
                                    Node {
                                        flex_direction: FlexDirection::Column,
                                        row_gap: px(4),
                                        padding: UiRect::all(px(14)),
                                        border: UiRect::all(px(1)),
                                        border_radius: BorderRadius::all(px(6)),
                                        ..default()
                                    },
                                    BackgroundColor(SECTION_BG),
                                    BorderColor::all(Color::srgba(0.25, 0.31, 0.38, 0.78)),
                                ))
                                .with_children(|d| {
                                    d.spawn(label("Flow", FONT_SIZE_CAPTION, TEXT_DIM));
                                    d.spawn(label(&a.diagram, 18.0, TEXT));
                                });
                            }
                            for id in &a.related {
                                if let Some((i, related)) =
                                    book.0.iter().enumerate().find(|(_, p)| &p.id == id)
                                {
                                    let visited = view.visited.contains(&related.id);
                                    topic_card(body, &icons, i, related, visited);
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
                                    section_header(body, &icons, group);
                                }
                                let visited = view.visited.contains(&a.id);
                                topic_card(body, &icons, i, a, visited);
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
            assert!(
                a.icon.is_empty() || BookIcon::ALL.into_iter().any(|i| i.slug() == a.icon),
                "unknown icon slug {:?} on {}",
                a.icon,
                a.id
            );
        }
    }
    #[test]
    fn every_article_resolves_an_icon() {
        // icon_for must never panic and should honor an explicit override.
        for a in &Textbook::default().0 {
            let icon = icon_for(a);
            if !a.icon.is_empty() {
                assert_eq!(icon.slug(), a.icon, "override ignored on {}", a.id);
            }
        }
    }
    /// Actually runs the `draw` system for the index and for every article in
    /// turn, headless. This is the system that spawns the styled UI, so it's
    /// the one place a bad bundle (e.g. two `Node`s spawned in one tuple)
    /// would panic — a bug the other tests here, which only inspect `Article`
    /// data, cannot catch.
    #[test]
    fn drawing_the_index_and_every_article_does_not_panic() {
        let mut app = App::new();
        app.add_plugins((TaskPoolPlugin::default(), AssetPlugin::default()))
            .init_asset::<Image>()
            .init_resource::<BookIconAssets>()
            .init_resource::<Textbook>()
            .init_resource::<TextbookView>()
            .init_resource::<Settings>()
            .insert_resource(Paused(true))
            .insert_resource(PauseScreen::Textbook)
            .add_systems(Update, draw);
        app.update();

        let article_count = app.world().resource::<Textbook>().0.len();
        for index in 0..article_count {
            let mut view = app.world_mut().resource_mut::<TextbookView>();
            view.article = Some(index);
            view.revision += 1;
            *app.world_mut().resource_mut::<PauseScreen>() = PauseScreen::TextbookArticle;
            app.update();
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
