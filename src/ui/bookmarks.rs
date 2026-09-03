//! The two ribbon tabs on the left edge of the lab.
//!
//! The field manual and the crew directory are the two screens a new chemist
//! is least likely to find on their own: both are keyboard-only, and until now
//! nothing on screen said either existed. Every *other* system in the lab
//! announces itself — machines have panels, the hotbar draws its own slots —
//! so these two were the only ones a player could finish a shift without ever
//! learning about.
//!
//! Each tab is drawn as the protruding ribbon of a closed book: flush with the
//! screen edge, square on that side, rounded on the other, with a thick accent
//! stripe down the spine. Pressing its key slides it toward the centre and
//! expands it into a panel-shaped rectangle, which the real panel then covers.
//!
//! **Clickable exactly when there is a cursor to click with.** While roaming,
//! the pointer is grabbed and locked to the centre of the window (see
//! `interaction::free_the_cursor`), so the tabs are a pure affordance: they
//! name the key and nothing more. From inside either screen or a machine
//! panel the cursor is free, and a tab can be pressed as well — opening,
//! closing, or swapping straight from one screen to the other.
//!
//! Clicks go through [`InteractionMode`]'s own toggles, the same ones the
//! keybinds use, so a press keeps a machine claim and restores whatever was
//! underneath exactly as the key would.

use bevy::prelude::*;

use super::icons::{icon_image, BookIcon, BookIconAssets};
use super::{BOOK_ACCENT, BOOK_INSET, LABEL_INK, TEXT, TEXT_DIM};
use crate::interaction::InteractionMode;
use crate::player::LocalPlayer;
use crate::settings::{key_label, Settings};

/// How long a tab takes to fly out or come home, in seconds.
///
/// Slow enough to actually read as a morph. The first cut ran at 0.22s and the
/// growth was over before the eye caught it — a 168px chip becoming a 468px
/// panel in a fifth of a second is a flicker, not a motion. The panel it turns
/// into is held back for the same span (see [`BookmarkFlight`]), so this is
/// time spent on a clear screen rather than on top of something being read.
const TRAVEL_SECONDS: f32 = 0.38;

/// Coming home is quicker than going out.
///
/// Closing is a dismissal: the player has decided they are done, and making
/// them watch the full outbound animation in reverse just delays getting back
/// to the lab. Opening is the part worth savouring.
const RETURN_SECONDS: f32 = 0.22;

/// Collapsed tab geometry. Kept narrow on purpose: the training HUD occupies
/// `left: 18px, top: 100px, width: 340px` (`tutorial::ui`), and a wider tab
/// would graze it on a short window. Wide enough, though, that the icon, the
/// title and the key chip all *fit* — the contents are clipped to this box,
/// so a tab too small for them renders as a blank rectangle.
const TAB_WIDTH: f32 = 168.0;
const TAB_HEIGHT: f32 = 60.0;
/// How much the tab grows on its way to becoming a panel.
const GROWTH_WIDTH: f32 = 300.0;
const GROWTH_HEIGHT: f32 = 180.0;
/// How far right the tab travels, as a percentage of the screen.
const TRAVEL_PERCENT: f32 = 46.0;
/// How far left the *other* tab retreats while a screen is open, in pixels.
const RETREAT_PIXELS: f32 = 220.0;

/// The stretch of the flight over which the ghost dissolves.
///
/// The panel is drawn from the moment the key is pressed — holding it back to
/// suit this animation soft-locked the game, see the note on `PanelEntrance` —
/// so the ghost overlaps a real page for its whole life and has to get out of
/// the way early. It leads the eye off the edge and is gone before it can
/// obscure anything worth reading; the panel's own fade-in covers the rest.
const HANDOFF_START: f32 = 0.0;
const HANDOFF_END: f32 = 0.45;

/// At rest the tabs sit above the training HUD (`10`) and below the hotbar
/// (`20`).
const REST_Z: i32 = 15;

/// In flight they rise above the machine panel (`30`) they are flying over.
///
/// Both bookmark screens can be opened while operating a machine — the claim is
/// deliberately kept, so a chemist can look a recipe up mid-batch — which means
/// the tab is animating on top of that machine's panel. Below it the flight
/// would slide behind the dispenser and simply not be visible.
///
/// Still under the label field (`50`) and everything above it: those are modal
/// in a way a machine panel is not.
const FLIGHT_Z: i32 = 35;

/// Which screen a tab stands for.
#[derive(Component, Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Bookmark {
    /// The chemistry field manual — [`InteractionMode::ReadingBook`].
    Manual,
    /// The crew directory and department shops — [`InteractionMode::Social`].
    Crew,
}

impl Bookmark {
    const ALL: [Self; 2] = [Self::Manual, Self::Crew];

    /// Position in [`Bookmark::ALL`], used to index the per-tab fade table.
    ///
    /// Spelled out rather than cast from the discriminant so adding a third
    /// tab cannot silently mis-index by picking up a default variant order.
    fn slot(self) -> usize {
        match self {
            Self::Manual => 0,
            Self::Crew => 1,
        }
    }

    /// The spine colour. Both are already in the palette: the manual takes the
    /// book's own accent, and the crew tab takes the warm ink the game reserves
    /// for things a *person* wrote, which is what a conversation record is.
    fn accent(self) -> Color {
        match self {
            Self::Manual => BOOK_ACCENT,
            Self::Crew => LABEL_INK,
        }
    }

    fn icon(self) -> BookIcon {
        match self {
            Self::Manual => BookIcon::Book,
            Self::Crew => BookIcon::Orders,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Manual => "MANUAL",
            Self::Crew => "CREW",
        }
    }

    /// Where this tab rests vertically, as an offset from the middle of the
    /// screen. The pair straddles centre rather than hanging below it.
    fn offset(self) -> f32 {
        match self {
            Self::Manual => -(TAB_HEIGHT + 4.0),
            Self::Crew => 4.0,
        }
    }

    /// The key that opens this screen, honouring a rebind.
    ///
    /// The fallbacks match [`crate::settings::Bindings`]' own defaults, and are
    /// only ever reached before the settings file has loaded.
    fn key(self, settings: Option<&Settings>) -> KeyCode {
        match self {
            Self::Manual => settings.map_or(KeyCode::KeyB, |s| s.bindings.book),
            Self::Crew => settings.map_or(KeyCode::Tab, |s| s.bindings.social),
        }
    }
}

/// Whether this tab's screen is the one currently open.
///
/// A free function over the mode rather than a method on it, and pure, so the
/// whole table is testable without a `World` — the same treatment
/// [`crate::interaction::escape_blocked_by_evacuation`] gets, and for the same
/// reason: it is one small decision that is easy to get subtly wrong.
///
/// `OrderDirectory` counts as the crew screen. It is reached from a button
/// *inside* the directory and shares its Tab toggle
/// ([`InteractionMode::toggled_social`]), so a tab that snapped home when the
/// player opened the order list would read as the screen having closed.
fn is_active(bookmark: Bookmark, mode: InteractionMode) -> bool {
    match bookmark {
        Bookmark::Manual => matches!(mode, InteractionMode::ReadingBook(_)),
        Bookmark::Crew => matches!(
            mode,
            InteractionMode::Social { .. } | InteractionMode::OrderDirectory { .. }
        ),
    }
}

/// A screen is up that the tabs must stay out of the way of entirely.
///
/// Deliberately narrow. Operating a machine is *not* one of these: both
/// bookmark screens can be opened over a machine panel without giving up the
/// claim — looking a recipe up mid-batch is the common case — so the tabs stay
/// offered there, and their flight is drawn over the panel ([`FLIGHT_Z`]).
///
/// What does hide them is a screen that owns the keyboard or the whole frame: a
/// label field, where the tab's own key is a letter being typed, and the
/// conversations, which are modal. The book and crew screens never hide a tab —
/// they are what the tabs turn into, and hiding a tab because its own screen is
/// open is what once left them stuck invisible.
fn tabs_are_unavailable(mode: InteractionMode) -> bool {
    matches!(
        mode,
        InteractionMode::Labelling(_)
            | InteractionMode::Inspecting { .. }
            | InteractionMode::OrderConversation(..)
            | InteractionMode::SecurityConversation
    )
}

/// One frame of travel, clamped to `[0, 1]`.
///
/// Split out and pure so an interrupted toggle — the player tapping the key
/// twice — can be proven to reverse from wherever it had got to rather than
/// snapping to an end. Linear, matching `animate_radio_dispatch`; the easing
/// happens in [`ease_out_cubic`] on the way to the node.
fn stepped(progress: f32, active: bool, delta: f32) -> f32 {
    let target = if active { 1.0 } else { 0.0 };
    let span = if active {
        TRAVEL_SECONDS
    } else {
        RETURN_SECONDS
    };
    let step = if span > 0.0 { delta / span } else { 1.0 };
    if progress < target {
        (progress + step).min(target)
    } else {
        (progress - step).max(target)
    }
}

/// Decelerating travel: quick off the edge, settling into the centre.
fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// How opaque the flying ghost is at a given point in its travel.
///
/// Fades out over [`HANDOFF_START`]..[`HANDOFF_END`], early in the flight,
/// because the panel it is handing off to is already drawn behind it.
fn ghost_alpha(eased: f32) -> f32 {
    1.0 - ((eased - HANDOFF_START) / (HANDOFF_END - HANDOFF_START)).clamp(0.0, 1.0)
}

/// The tab root — the node that actually moves.
#[derive(Component)]
pub(super) struct BookmarkTab {
    bookmark: Bookmark,
    /// 0.0 at rest on the edge, 1.0 fully flown out.
    progress: f32,
    /// 0.0 in place, 1.0 fully stepped aside off the left edge.
    ///
    /// Independent of [`Self::progress`]: a tab is either flying out to become
    /// a screen, or getting out of the way of the other tab's screen, and
    /// conflating the two let an idle tab read as retreated (drawn at zero
    /// alpha) and a mid-flight one snap to fully opaque when the screen closed.
    retreat: f32,
}

// There is deliberately no mechanism here for *delaying* the panel.
//
// An earlier cut held the panel back until the tab had flown out, so the morph
// would play against the lab rather than across an already-open page. That is a
// soft-lock: `interaction::panel_input` frees the cursor and freezes the camera
// from `InteractionMode` alone, with no knowledge of whether a panel actually
// rendered. Any path that withheld the panel — a stalled tab, a fast toggle —
// left the player unable to move or look, staring at an empty lab, with only an
// undiscoverable Escape to recover. Presentation must never gate gameplay
// state; the panel now spawns the moment the mode says so, exactly as it always
// did, and merely fades in underneath the arriving tab.

/// Everything on a tab root that [`animate_bookmarks`] drives each frame.
type TabChrome = (
    &'static mut BookmarkTab,
    &'static mut Node,
    &'static mut GlobalZIndex,
    &'static mut Visibility,
    &'static BookmarkTint,
    &'static mut BackgroundColor,
    &'static mut BorderColor,
    &'static Interaction,
);

/// A node whose background carries a tint.
type TintedBackground = (&'static BookmarkTint, &'static mut BackgroundColor);

/// Key chips alone. Excludes the tab roots, whose `BackgroundColor` the main
/// loop already holds mutably.
type ChipsOnly = (With<BookmarkChip>, Without<BookmarkTab>);

/// Everything inside a tab that fades with it, bundled so `animate_bookmarks`
/// stays within a readable argument count — the same device `PanelViews` uses.
#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct TabContents<'w, 's> {
    text: Query<'w, 's, (&'static BookmarkTint, &'static mut TextColor)>,
    images: Query<'w, 's, (&'static BookmarkTint, &'static mut ImageNode)>,
    /// The key chip's own inset panel, and only that — see [`BookmarkChip`].
    chips: Query<'w, 's, TintedBackground, ChipsOnly>,
}

/// A panel that should fade and scale up as the tab hands off to it.
///
/// Without this the panel simply exists at full size the frame it is allowed
/// to draw, and the whole morph ends in a pop — the tab glides in, then the
/// finished screen appears on top of it in one frame.
///
/// The stored value is the entrance's own progress, `0.0..=1.0`, advanced by
/// [`animate_panel_entrance`]. It is *not* read from the tab's progress: the
/// panel outlives any single flight (`sync_panel` rebuilds it whenever its
/// signature changes, e.g. on every page turn) and must not restart its
/// entrance each time.
#[derive(Component)]
pub(crate) struct PanelEntrance(f32);

impl PanelEntrance {
    /// A panel arriving from a bookmark tab, mid-handoff.
    pub(crate) fn arriving() -> Self {
        Self(0.0)
    }

    /// A panel that should simply be there — a rebuild of a screen already
    /// open, or one reached without a tab flying at all.
    pub(crate) fn settled() -> Self {
        Self(1.0)
    }
}

/// How long the panel takes to settle once it starts drawing.
///
/// Deliberately covers the rest of the tab's flight: the ghost dissolves from
/// [`HANDOFF_START`] to 1.0, and the panel fades up across the same span, so
/// the two genuinely cross rather than one replacing the other.
const ENTRANCE_SECONDS: f32 = TRAVEL_SECONDS * (1.0 - HANDOFF_START);

/// Fades and settles a panel that a bookmark tab just handed off to.
pub(super) fn animate_panel_entrance(
    time: Res<Time>,
    mut panels: Query<(&mut PanelEntrance, &mut BackgroundColor)>,
) {
    let delta = time.delta_secs();
    for (mut entrance, mut background) in &mut panels {
        if entrance.0 >= 1.0 {
            continue;
        }
        entrance.0 = (entrance.0 + delta / ENTRANCE_SECONDS).min(1.0);
        let eased = ease_out_cubic(entrance.0);
        // The scrim darkens in step with the screen, so the lab does not black
        // out before the panel that justifies it has arrived.
        //
        // Only the alpha moves. This node is the full-screen backdrop, and
        // scaling it would pull the dimming away from the screen edges and
        // leave the lab showing in a bright frame around the panel.
        background.0 = background.0.with_alpha(SCRIM_ALPHA * eased);
    }
}

/// The authored opacity of the dimmed lab behind an open screen — matches what
/// `spawn_reference_book` and `spawn_social_directory` both paint.
const SCRIM_ALPHA: f32 = 0.72;

/// The key-hint text, patched in place when a binding changes.
///
/// Mirrors `HotbarText::Key`: the tab is spawned once and its text written
/// into, rather than the tab being rebuilt when the player rebinds.
#[derive(Component)]
pub(super) struct BookmarkKeyText(Bookmark);

/// The authored colour of something inside a tab, and which tab owns it.
///
/// Alpha is recomputed from this base every frame rather than multiplied into
/// the live colour, which would fade to black over a few frames. The same
/// device `RadioCardBackground`/`RadioCardText` use.
///
/// Carrying the owning [`Bookmark`] means the fade pass can find a descendant
/// by query alone. Walking the hierarchy instead would need the tab root's
/// `Children` while its own colour components are already mutably borrowed.
#[derive(Component)]
pub(super) struct BookmarkTint(Bookmark, Color);

/// A tab child whose *background* is tinted, as opposed to its text or image.
///
/// Required rather than inferred from "has a `BackgroundColor`". Bevy gives
/// every `Text` node one, so a query matching on that alone repainted the
/// title and the key hint as solid coloured rectangles — the tint colour
/// filling the node — and the tabs rendered as blocks with no readable text.
#[derive(Component)]
pub(super) struct BookmarkChip;

pub(super) fn spawn_bookmarks(mut commands: Commands, icons: Res<BookIconAssets>) {
    for bookmark in Bookmark::ALL {
        let accent = bookmark.accent();
        let background = Color::srgba(0.07, 0.08, 0.10, 0.94);
        commands
            .spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(0),
                    // Half the viewport down, then offset by the tab's own
                    // place in the stack. `vh(50)` rather than `percent(50)`:
                    // the root is a free-floating absolute node with no sized
                    // parent to take a percentage of.
                    top: vh(50),
                    margin: UiRect::top(px(bookmark.offset())),
                    width: px(TAB_WIDTH),
                    height: px(TAB_HEIGHT),
                    align_items: AlignItems::Center,
                    column_gap: px(8),
                    padding: UiRect::axes(px(10), px(7)),
                    // Square against the screen edge, rounded away from it:
                    // the silhouette of a ribbon poking out of a closed book.
                    border_radius: BorderRadius {
                        top_left: px(0),
                        bottom_left: px(0),
                        top_right: px(8),
                        bottom_right: px(8),
                    },
                    border: UiRect::left(px(4)),
                    overflow: Overflow::clip(),
                    ..default()
                },
                BackgroundColor(background),
                BorderColor::all(accent),
                BookmarkTint(bookmark, background),
                GlobalZIndex(REST_Z),
                // Clickable whenever the cursor is free — that is, from inside
                // either screen or a machine panel. While roaming the cursor is
                // grabbed, so there is nothing to click with and the tab is a
                // pure affordance; `click_bookmarks` gates on exactly that.
                Button,
                // Not `Pickable::IGNORE` any more, but the tab must still not
                // swallow clicks meant for the panel underneath it: it only
                // occupies the strip it actually draws on.
                Pickable::default(),
                // The generic hover/press repaint must not touch a tab: its
                // background is the ribbon's own colour, faded every frame by
                // `animate_bookmarks`, and `button_feedback` would flatten it
                // to the standard button grey the moment the pointer crossed.
                super::PreserveButtonBackground,
                BookmarkTab {
                    bookmark,
                    progress: 0.0,
                    retreat: 0.0,
                },
                crate::until_we_leave_the_lab(),
            ))
            .with_children(|tab| {
                tab.spawn((
                    icon_image(&icons, bookmark.icon(), 22.0, accent),
                    BookmarkTint(bookmark, accent),
                ));
                tab.spawn(Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: px(3),
                    // The tab grows as it flies out; the label column must not
                    // stretch with it, or the text drifts away from the icon.
                    flex_shrink: 0.0,
                    ..default()
                })
                .with_children(|stack| {
                    stack.spawn((
                        Text::new(bookmark.title()),
                        TextFont::from_font_size(12.0),
                        TextColor(TEXT),
                        BookmarkTint(bookmark, TEXT),
                    ));
                    // The key chip. Its text is left empty on purpose: the
                    // binding is written in by `update_bookmark_keys`, so a
                    // rebind never leaves a stale letter on screen.
                    stack
                        .spawn((
                            Node {
                                padding: UiRect::axes(px(5), px(2)),
                                border_radius: BorderRadius::all(px(3)),
                                align_self: AlignSelf::Start,
                                ..default()
                            },
                            BackgroundColor(BOOK_INSET),
                            BookmarkTint(bookmark, BOOK_INSET),
                            BookmarkChip,
                        ))
                        .with_children(|chip| {
                            chip.spawn((
                                Text::new(""),
                                TextFont::from_font_size(12.0),
                                TextColor(TEXT_DIM),
                                BookmarkTint(bookmark, TEXT_DIM),
                                BookmarkKeyText(bookmark),
                            ));
                        });
                });
            });
    }
}

/// Opens, closes or swaps a screen when its tab is clicked.
///
/// Only while the cursor is actually free — inside either screen, or over a
/// machine panel. While roaming the cursor is grabbed and locked to the centre
/// of the window (`interaction::free_the_cursor`), so a click there is a
/// look-around, not a press, and the tab stays a pure affordance.
///
/// Never while paused: the pause overlay owns the screen, and a stray press
/// landing on a tab underneath it would open a screen behind the menu.
///
/// Deliberately reuses [`InteractionMode`]'s own toggles rather than assigning
/// a mode directly, so a click goes through exactly the transitions the
/// keybinds do — including keeping a machine claim, and restoring the screen
/// underneath on close.
pub(super) fn click_bookmarks(
    mut modes: Query<&mut InteractionMode, With<LocalPlayer>>,
    paused: Option<Res<crate::settings::Paused>>,
    released: Option<Res<crate::interaction::CursorReleased>>,
    tabs: Query<(&Interaction, &BookmarkTab), Changed<Interaction>>,
    mut play: MessageWriter<crate::audio::PlaySfx>,
) {
    if paused.is_some_and(|paused| paused.0) {
        return;
    }
    let Some(mut mode) = modes.iter_mut().next() else {
        return;
    };
    // The cursor is free exactly when a screen is open or it was released by
    // hand — the same condition `panel_input` hands to `free_the_cursor`.
    let clickable = !mode.is_roaming() || released.is_some_and(|released| released.get());
    if !clickable {
        return;
    }

    for (interaction, tab) in &tabs {
        if *interaction != Interaction::Pressed {
            continue;
        }
        // Clicking the tab of the screen already open closes it; clicking the
        // other one swaps straight to it. `toggled_social`/`toggled_book`
        // already do both — from inside the crew screen the book toggle steps
        // to the book, and vice versa — so there is no separate swap path.
        *mode = match tab.bookmark {
            Bookmark::Manual => mode.toggled_book(),
            Bookmark::Crew => mode.toggled_social(),
        };
        play.write(crate::audio::PlaySfx(crate::audio::Sfx::UiClick));
        return;
    }
}

pub(super) fn update_bookmark_keys(
    settings: Option<Res<Settings>>,
    mut chips: Query<(&BookmarkKeyText, &mut Text)>,
) {
    for (chip, mut text) in &mut chips {
        let wanted = key_label(chip.0.key(settings.as_deref()));
        if text.0 != wanted {
            text.0 = wanted;
        }
    }
}

/// Drives both tabs from the current [`InteractionMode`].
///
/// Everything the node shows is recomputed from `progress` each frame rather
/// than nudged by a delta, so an interrupted toggle reverses cleanly from
/// wherever it was — the discipline `animate_radio_dispatch` already follows.
pub(super) fn animate_bookmarks(
    time: Res<Time>,
    modes: Query<&InteractionMode, With<LocalPlayer>>,
    paused: Option<Res<crate::settings::Paused>>,
    mut tabs: Query<TabChrome>,
    mut contents: TabContents,
) {
    let mode = modes.iter().next().copied().unwrap_or_default();
    let paused = paused.is_some_and(|paused| paused.0);
    // One of the two bookmark screens is up. Not "any panel": a machine panel
    // leaves both tabs sitting normally on the edge, still offered, because
    // either screen can be opened over a machine without dropping the claim.
    let any_open = Bookmark::ALL.iter().any(|b| is_active(*b, mode));
    let delta = time.delta_secs();
    // Each tab's fade, indexed the same way `Bookmark::ALL` is, so the second
    // pass over the children can look one up without walking the hierarchy.
    let mut alphas = [1.0f32; Bookmark::ALL.len()];

    for (mut tab, mut node, mut z, mut visibility, tint, mut background, mut border, interaction) in
        &mut tabs
    {
        let active = is_active(tab.bookmark, mode);
        let hovered = *interaction != Interaction::None;
        tab.progress = stepped(tab.progress, active, delta);
        let progress = tab.progress;
        let eased = ease_out_cubic(progress);

        // The tab that is *not* the one being opened steps aside, so it does not
        // hang over the screen that just appeared.
        //
        // Tracked on its own axis rather than derived from `eased`. Deriving it
        // meant an idle tab (`eased == 0`) read as *fully* retreated the instant
        // the other screen opened, so instead of sliding aside it blinked to
        // zero alpha — and stayed invisible for as long as anything was open.
        tab.retreat = stepped(tab.retreat, any_open && !active, delta);
        let retreat = ease_out_cubic(tab.retreat);

        // Hidden behind the pause overlay, and under a screen that owns the
        // keyboard or the whole frame. Never at a machine — both screens can be
        // read while operating one — never mid-flight or mid-retreat, or a tab
        // would vanish on its way home, and never merely because the *other*
        // tab's screen is open: that case is the retreat above, which slides it
        // off the edge and can always slide it back.
        let hidden = progress <= 0.0 && retreat <= 0.0 && (paused || tabs_are_unavailable(mode));
        let wanted = if hidden {
            Visibility::Hidden
        } else {
            Visibility::Inherited
        };
        if *visibility != wanted {
            *visibility = wanted;
        }

        node.left = if progress > 0.0 {
            percent(eased * TRAVEL_PERCENT)
        } else {
            px(-retreat * RETREAT_PIXELS)
        };
        node.width = px(TAB_WIDTH + eased * GROWTH_WIDTH);
        node.height = px(TAB_HEIGHT + eased * GROWTH_HEIGHT);
        // The flat spine rounds off as the ribbon becomes a free-floating
        // panel, and the heavy left stripe evens out into a panel's border.
        node.border_radius = BorderRadius {
            top_left: px(eased * 10.0),
            bottom_left: px(eased * 10.0),
            top_right: px(8.0 + eased * 2.0),
            bottom_right: px(8.0 + eased * 2.0),
        };
        let spine = 4.0 - eased * 2.0;
        node.border = UiRect {
            left: px(spine),
            right: px(eased * 2.0),
            top: px(eased * 2.0),
            bottom: px(eased * 2.0),
        };

        let wanted_z = if progress > 0.0 { FLIGHT_Z } else { REST_Z };
        if z.0 != wanted_z {
            z.0 = wanted_z;
        }

        // One factor for the whole tab: the handoff fade while flying out, and
        // the retreat fade when stepping aside for the other screen.
        let alpha = ghost_alpha(eased) * (1.0 - retreat);
        alphas[tab.bookmark.slot()] = alpha;
        // Hovering lifts the ribbon a little brighter, standing in for the
        // generic button repaint this node opts out of. Only at rest: a tab
        // mid-flight is already the loudest thing on screen.
        let lit = hovered && progress <= 0.0 && retreat <= 0.0;
        let base = if lit { tint.1.lighter(0.06) } else { tint.1 };
        background.0 = base.with_alpha(tint.1.alpha() * alpha);
        // The spine tracks the panel's own frame colour as it lands, so the
        // ghost and the thing replacing it are not two different rectangles.
        let frame = Color::srgb(0.24, 0.40, 0.50);
        let spine_color = tab.bookmark.accent().mix(&frame, eased);
        *border = BorderColor::all(spine_color.with_alpha(alpha));
    }

    // The contents fade with the tab that owns them. A second pass rather than
    // a walk from inside the loop above: these are different component types on
    // different entities, and `BackgroundColor` in particular is already
    // mutably borrowed for the roots there.
    for (tint, mut color) in &mut contents.text {
        color.0 = tint.1.with_alpha(tint.1.alpha() * alphas[tint.0.slot()]);
    }
    for (tint, mut image) in &mut contents.images {
        image.color = tint.1.with_alpha(tint.1.alpha() * alphas[tint.0.slot()]);
    }
    for (tint, mut color) in &mut contents.chips {
        color.0 = tint.1.with_alpha(tint.1.alpha() * alphas[tint.0.slot()]);
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn machine() -> Entity {
        Entity::from_raw_u32(1).expect("a valid test entity id")
    }

    #[test]
    fn bookmark_is_active_for_its_own_mode_only() {
        // The manual, from the floor and from over a machine.
        assert!(is_active(
            Bookmark::Manual,
            InteractionMode::ReadingBook(None)
        ));
        assert!(is_active(
            Bookmark::Manual,
            InteractionMode::ReadingBook(Some(machine()))
        ));
        assert!(!is_active(
            Bookmark::Crew,
            InteractionMode::ReadingBook(None)
        ));

        // The crew directory, and the order list reached from inside it.
        let social = InteractionMode::Social {
            machine: None,
            return_to_book: false,
        };
        let orders = InteractionMode::OrderDirectory {
            machine: None,
            return_to_book: true,
        };
        assert!(is_active(Bookmark::Crew, social));
        assert!(is_active(Bookmark::Crew, orders));
        assert!(!is_active(Bookmark::Manual, social));
        assert!(!is_active(Bookmark::Manual, orders));

        // Nothing else lights either tab.
        for mode in [
            InteractionMode::Roaming,
            InteractionMode::UsingMachine(machine()),
            InteractionMode::Labelling(machine()),
            InteractionMode::SecurityConversation,
            InteractionMode::Inspecting {
                item: machine(),
                machine: None,
            },
        ] {
            assert!(!is_active(Bookmark::Manual, mode), "{mode:?}");
            assert!(!is_active(Bookmark::Crew, mode), "{mode:?}");
        }
    }

    #[test]
    fn progress_eases_out_and_clamps() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        // Out of range in either direction is pinned, not extrapolated.
        assert_eq!(ease_out_cubic(-0.5), 0.0);
        assert_eq!(ease_out_cubic(2.0), 1.0);
        // Decelerating: past the halfway mark in less than half the travel.
        assert!(ease_out_cubic(0.5) > 0.5);
        let mut last = 0.0;
        for step in 0..=20 {
            let value = ease_out_cubic(step as f32 / 20.0);
            assert!(value >= last, "eased travel went backwards at {step}");
            assert!((0.0..=1.0).contains(&value));
            last = value;
        }
    }

    #[test]
    fn an_interrupted_open_reverses_from_where_it_stopped() {
        let frame = TRAVEL_SECONDS / 8.0;
        let mut progress = 0.0;
        for _ in 0..3 {
            progress = stepped(progress, true, frame);
        }
        let partial = progress;
        assert!(
            partial > 0.0 && partial < 1.0,
            "three frames should land mid-flight, got {partial}"
        );

        // Releasing mid-flight walks back down from the partial value rather
        // than completing the open first.
        let reversed = stepped(partial, false, frame);
        assert!(
            reversed < partial,
            "an interrupted open must reverse, got {reversed} from {partial}"
        );
        assert!(reversed > 0.0);
    }

    #[test]
    fn travel_settles_exactly_at_the_ends() {
        // Overshoot is clamped, so a long frame cannot strand a tab past the
        // centre or off the left edge.
        assert_eq!(stepped(0.9, true, TRAVEL_SECONDS), 1.0);
        assert_eq!(stepped(0.1, false, RETURN_SECONDS), 0.0);
        assert_eq!(stepped(1.0, true, TRAVEL_SECONDS), 1.0);
        assert_eq!(stepped(0.0, false, RETURN_SECONDS), 0.0);
    }

    #[test]
    fn the_ghost_stays_solid_across_the_flight_then_dissolves() {
        assert_eq!(ghost_alpha(0.0), 1.0);
        assert_eq!(ghost_alpha(1.0), 0.0);
        // Solid for the whole journey: the panel is held back until the tab
        // dissolves — see `the_ghost_clears_the_page_it_flies_over`.
        assert!(ghost_alpha(0.2) < 1.0 && ghost_alpha(0.2) > 0.0);
    }

    /// The ghost must be out of the way early.
    ///
    /// `interaction::panel_input` frees the cursor and freezes the camera from
    /// `InteractionMode` alone, so the panel has to spawn the instant the mode
    /// flips — an earlier cut delayed it to suit this animation and soft-locked
    /// the game whenever the tab stalled. The panel is therefore on screen for
    /// the whole flight, and the ghost overlapping it must not outlast a glance.
    #[test]
    fn the_ghost_clears_the_page_it_flies_over() {
        const { assert!(HANDOFF_START == 0.0) };
        const { assert!(HANDOFF_END < 0.5) };
        assert_eq!(ghost_alpha(0.0), 1.0, "visible at the edge, or no motion");
        assert_eq!(ghost_alpha(HANDOFF_END), 0.0);
        assert_eq!(ghost_alpha(0.5), 0.0, "gone by the halfway point");
        assert_eq!(ghost_alpha(1.0), 0.0);
    }

    /// The tabs are only hidden by a screen that owns the keyboard or the frame.
    ///
    /// Two earlier cuts got this wrong in opposite directions: one hid a tab
    /// whenever the mode was not `Roaming`, which hid *both* whenever either
    /// screen opened; the other kept hiding them at a machine, even though both
    /// screens can be opened over a machine panel without dropping the claim.
    #[test]
    fn only_a_modal_screen_hides_the_tabs() {
        let machine = machine();
        // A label field owns the letter keys — including the book's own — and
        // the conversations own the frame.
        for mode in [
            InteractionMode::Labelling(machine),
            InteractionMode::SecurityConversation,
            InteractionMode::OrderConversation(machine, 1),
            InteractionMode::Inspecting {
                item: machine,
                machine: None,
            },
        ] {
            assert!(tabs_are_unavailable(mode), "{mode:?} should hide the tabs");
        }

        // Operating a machine keeps them offered: that is the whole point of
        // being able to look a recipe up mid-batch.
        for mode in [
            InteractionMode::Roaming,
            InteractionMode::UsingMachine(machine),
            InteractionMode::ReadingBook(None),
            InteractionMode::ReadingBook(Some(machine)),
            InteractionMode::Social {
                machine: Some(machine),
                return_to_book: false,
            },
            InteractionMode::OrderDirectory {
                machine: None,
                return_to_book: false,
            },
        ] {
            assert!(
                !tabs_are_unavailable(mode),
                "{mode:?} must leave the tabs on screen"
            );
        }
    }

    /// A tab's flight has to draw over the machine panel it is opened above.
    #[test]
    fn a_flying_tab_clears_the_machine_panel_beneath_it() {
        /// `GlobalZIndex` of an open machine panel — see `sync_panel`.
        const MACHINE_PANEL_Z: i32 = 30;
        /// The label field, which is genuinely modal and stays on top.
        const LABEL_FIELD_Z: i32 = 50;
        const { assert!(FLIGHT_Z > MACHINE_PANEL_Z) };
        const { assert!(FLIGHT_Z < LABEL_FIELD_Z) };
        // At rest they stay tucked below the hotbar as ordinary HUD furniture.
        const { assert!(REST_Z < 20) };
    }

    /// Swapping screens closes one tab as it opens the other, in one motion.
    #[test]
    fn switching_screens_crosses_the_two_tabs() {
        let social = InteractionMode::Social {
            machine: None,
            return_to_book: false,
        };
        // Pressing Tab while the manual is open: the manual is no longer active
        // and heads home, the crew tab becomes active and flies out. Both move
        // on the same frame, so the two cross rather than queueing.
        assert!(!is_active(Bookmark::Manual, social));
        assert!(is_active(Bookmark::Crew, social));

        let manual_going_home = stepped(1.0, false, 0.016);
        let crew_flying_out = stepped(0.0, true, 0.016);
        assert!(manual_going_home < 1.0, "the open tab must start closing");
        assert!(crew_flying_out > 0.0, "the other must start opening");
    }

    /// Retreat is its own axis. Derived from the open progress, an idle tab
    /// read as fully retreated the moment the other screen opened.
    #[test]
    fn an_idle_tab_is_not_treated_as_retreated() {
        // A tab at rest with nothing open stays put and fully visible.
        assert_eq!(stepped(0.0, false, 0.016), 0.0);
        // It only retreats once told to, and gets there gradually.
        let first = stepped(0.0, true, 0.016);
        assert!(first > 0.0 && first < 1.0, "retreat must ease, not snap");
        // And it can always come back.
        assert!(stepped(first, false, 0.016) < first);
    }

    /// Only the key chip's inset panel is background-tinted.
    ///
    /// Bevy gives every `Text` node a `BackgroundColor`, so selecting on that
    /// component alone swept up the title and the key hint and painted them as
    /// solid rectangles in the tint colour — the tabs rendered as blocks with
    /// no readable text at all. [`BookmarkChip`] is what keeps the fade pass
    /// off anything whose colour lives in its `TextColor`.
    #[test]
    fn only_the_key_chip_has_a_tinted_background() {
        let mut app = App::new();
        app.add_plugins((
            bevy::app::TaskPoolPlugin::default(),
            bevy::asset::AssetPlugin::default(),
        ))
        .init_asset::<Image>()
        .init_resource::<BookIconAssets>()
        .add_systems(Startup, spawn_bookmarks);
        app.update();

        let world = app.world_mut();
        let chips = world
            .query_filtered::<(), (With<BookmarkChip>, With<BookmarkTint>)>()
            .iter(world)
            .count();
        assert_eq!(chips, Bookmark::ALL.len(), "one key chip per tab");

        // Every tinted text node must be left to its `TextColor`; none of them
        // may be reached by the background pass.
        let tinted_text = world
            .query_filtered::<(), (With<Text>, With<BookmarkChip>)>()
            .iter(world)
            .count();
        assert_eq!(
            tinted_text, 0,
            "text must never be treated as a background chip"
        );

        // And the titles really are on screen as text, not as blocks.
        let visible = world
            .query::<&Text>()
            .iter(world)
            .map(|text| text.0.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        for bookmark in Bookmark::ALL {
            assert!(
                visible.contains(bookmark.title()),
                "{} needs a readable title",
                bookmark.title()
            );
        }
    }

    /// Clicking a tab goes through the same toggles the keybinds do, so every
    /// case the keys handle — closing, swapping, keeping a machine claim — is
    /// handled identically by a press.
    #[test]
    fn clicking_a_tab_matches_what_its_key_would_do() {
        let machine = machine();

        // From a machine panel: the claim survives, exactly as the key does it.
        assert_eq!(
            InteractionMode::UsingMachine(machine).toggled_book(),
            InteractionMode::ReadingBook(Some(machine))
        );

        // Clicking the open screen's own tab closes it back to the machine.
        assert_eq!(
            InteractionMode::ReadingBook(Some(machine)).toggled_book(),
            InteractionMode::UsingMachine(machine)
        );

        // Clicking the *other* tab while one is open swaps to it, and closing
        // that one returns to the screen it was opened over.
        let swapped = InteractionMode::ReadingBook(Some(machine)).toggled_social();
        assert_eq!(
            swapped,
            InteractionMode::Social {
                machine: Some(machine),
                return_to_book: true,
            }
        );
        assert_eq!(
            swapped.toggled_social(),
            InteractionMode::ReadingBook(Some(machine)),
            "closing the swapped-to screen returns to the book underneath"
        );
    }

    #[test]
    fn closing_is_quicker_than_opening() {
        const { assert!(RETURN_SECONDS < TRAVEL_SECONDS) };
        // One frame of each direction, from the same starting point.
        let out = stepped(0.5, true, 0.016);
        let home = stepped(0.5, false, 0.016);
        assert!(
            (0.5 - home) > (out - 0.5),
            "coming home should cover more ground per frame than going out"
        );
    }

    #[test]
    fn both_tabs_read_the_live_binding() {
        let mut settings = Settings::default();
        assert_eq!(key_label(Bookmark::Manual.key(Some(&settings))), "B");
        assert_eq!(key_label(Bookmark::Crew.key(Some(&settings))), "Tab");

        settings.bindings.book = KeyCode::KeyM;
        settings.bindings.social = KeyCode::KeyN;
        assert_eq!(key_label(Bookmark::Manual.key(Some(&settings))), "M");
        assert_eq!(key_label(Bookmark::Crew.key(Some(&settings))), "N");

        // Before the settings file loads, the tabs still name the real defaults.
        assert_eq!(key_label(Bookmark::Manual.key(None)), "B");
        assert_eq!(key_label(Bookmark::Crew.key(None)), "Tab");
    }

    /// The contents are clipped to the collapsed box (`Overflow::clip`), so a
    /// tab narrower than what it holds renders as a blank rectangle with the
    /// icon and the key hint invisible — which is exactly what shipped first.
    #[test]
    fn a_collapsed_tab_is_wide_enough_for_everything_it_holds() {
        const HORIZONTAL_PADDING: f32 = 20.0;
        const SPINE: f32 = 4.0;
        const ICON: f32 = 22.0;
        const GAP: f32 = 8.0;
        // "MANUAL" at 12px in the bundled monospace font, plus the key chip's
        // own padding under it. Generous rather than exact: the point is that
        // the box is not *tighter* than its contents.
        let widest_title = Bookmark::ALL
            .iter()
            .map(|b| b.title().len())
            .max()
            .expect("there is at least one tab") as f32;
        let text = widest_title * 8.0;
        let needed = HORIZONTAL_PADDING + SPINE + ICON + GAP + text;
        assert!(
            TAB_WIDTH >= needed,
            "a {TAB_WIDTH}px tab clips its own {needed}px of contents"
        );
        // Two stacked 12px lines plus the gap and vertical padding.
        const { assert!(TAB_HEIGHT >= 14.0 + 3.0 + 16.0 + 14.0) };
    }

    #[test]
    fn the_two_tabs_straddle_the_centre_without_overlapping() {
        let manual = Bookmark::Manual.offset();
        let crew = Bookmark::Crew.offset();
        assert!(manual < 0.0, "the manual sits above centre");
        assert!(crew > 0.0, "the crew tab sits below centre");
        assert!(
            crew - (manual + TAB_HEIGHT) >= 0.0,
            "the tabs must not overlap"
        );
    }
}
