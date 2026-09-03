//! Machine control panels.
//!
//! Panels are pure presentation. Every button carries a [`PanelAction`] which
//! one system turns into a message; nothing here mutates a solution.
//!
//! The whole panel is rebuilt whenever the state it shows changes, rather than
//! patching individual nodes. At this size that is far simpler to keep correct,
//! and it only happens on user action.

use std::collections::{HashSet, VecDeque};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use chem_sim::{Category, ChemFamily, DamageKind, Kelvin, ReactionId, ReagentId, Units};

use crate::arc::{ArcScript, Campaign, Reveal};
use crate::audio::{PlaySfx, Sfx};
use crate::body::{Bloodstream, Body};
use crate::chem_data::ChemDb;
use crate::containers::{
    Container, ContainerKind, InSlot, InSlotB, InSlotC, InventorySlot, SelectedInventorySlot,
    Stored, INVENTORY_SLOTS,
};
use crate::crew::CrewMember;
use crate::interaction::{leave_machine, Interactable, InteractionMode, LeaveMachineRequested};
use crate::knowledge::{
    product_name, reaction_categories, BuyHintRequested, Knowledge, RecipeDiscovered,
    UnlockAllRequested, HINT_COST,
};
use crate::machines::{
    slotted_container, slotted_container_b, slotted_container_c, stored_in, AgitateDirection,
    AgitateRequested, AgitationRun, AnalyzeRequested, Buffer, BufferDirection,
    BufferTransferRequested, DispenseAmount, DispenseRequested, EjectRequested, EmptyRequested,
    GrindRequested, Hopper, HplcReport, Machine, MachineKind, MachineSlot, PackageRequested,
    PurifyRequested, SetHeaterPower, SetTargetTemperature, TakeRequested, Thermostat,
    HPLC_RECIPE_REQUIREMENT, LOCKER_CAPACITY, TEMPERATURE_MAX, TEMPERATURE_MIN,
};
use crate::orders::{Department, DevelopmentOrder, GlasswarePackId, Order, Shift, StationData};
use crate::player::LocalPlayer;
use crate::produce::{ProduceCatalog, ProduceId};
use crate::radio::{RadioChannel, RadioEntry, RadioLog, RadioPriority, RadioTone};
use crate::shift::{
    can_call_it, shift_report, CallItAShift, CareerStage, ConditionChange, NpcRequisitionKind,
    NpcRequisitionRequested, OpenUpAgain, RequisitionKind, RequisitionRequested, ShiftReport,
    ToggleAcceptingOrders, OVERCLOCK_COST, PRESSURE_SPRAYER_COST, SYRINGE_GUN_COST, WATER_GUN_COST,
};
use crate::social::{
    resident_department, ConversationHistory, FavorOutcome, PersonalHistory, PublicRelationship,
    RelationshipTier,
};
use crate::AppState;

mod book;
mod bookmarks;
pub(crate) mod icons;
pub(crate) mod mixing;
mod tooltip;

use accesskit::Role;
use book::{ProfileCoverage, RecipePresentation};
pub(crate) use icons::{icon_image, BookIcon, BookIconAssets};
use tooltip::{accessibility_label, TooltipSource, TooltipState};

/// How many orders the queue can show at once.
///
/// Must be at least the highest `max_active` the difficulty ramp can reach, not
/// the base value in `station.orders.ron` — a queue shorter than the ramp hides
/// the order that is about to expire, which is the one the player most needs.
/// `the_order_queue_has_a_slot_for_every_concurrent_order` holds the two together.
pub(crate) const ORDER_SLOTS: usize = 5;
const RADIO_PENDING_CAPACITY: usize = 6;

// Shared with the main menu, so the first screen the player sees and every
// panel afterwards are visibly the same game.
pub(crate) const PANEL_BG: Color = Color::srgba(0.07, 0.08, 0.10, 0.97);
pub(crate) const SECTION_BG: Color = Color::srgba(0.12, 0.13, 0.16, 0.9);
pub(crate) const TEXT: Color = Color::srgb(0.88, 0.90, 0.94);
pub(crate) const TEXT_DIM: Color = Color::srgb(0.55, 0.59, 0.66);
/// A failed connection attempt, shown on the mode screen after a bounce back
/// from `AppState::Connecting` — the one place in the menu that needs to say
/// something went wrong out loud rather than staying silent.
pub(crate) const ERROR_TEXT: Color = Color::srgb(0.85, 0.35, 0.35);
/// The counterpart to [`ERROR_TEXT`], for the one notice on the standing board
/// that can carry good news: an arc that ended with the station still standing.
pub(crate) const GOOD_TEXT: Color = Color::srgb(0.45, 0.80, 0.50);
/// Whatever the player wrote on a bottle themselves — see [`crate::labels`].
///
/// Its own colour, and warmer than anything the readouts use, because a label
/// is the one piece of text in this UI the *game* did not write. Every number
/// beside it is measured; this is a claim, and it needs to look like one.
pub(crate) const LABEL_INK: Color = Color::srgb(0.92, 0.82, 0.55);
pub(crate) const BUTTON_IDLE: Color = Color::srgb(0.17, 0.19, 0.23);
const BUTTON_HOVER: Color = Color::srgb(0.25, 0.29, 0.35);
/// The "this is the one that is currently set" tint, shared by the dispense
/// amount row, the book's open tab and the settings screen's presets.
///
/// Darker than it looks like it should be: at the lighter blue this used to be,
/// `TEXT` on top of it only hit 4.10:1 contrast, under the 4.5:1 WCAG AA floor
/// for normal-size text. This value keeps it reading as "the blue one" next to
/// `BUTTON_HOVER`/`BUTTON_IDLE` while landing at 7.78:1.
pub(crate) const BUTTON_ACTIVE: Color = Color::srgb(0.10, 0.28, 0.40);

/// Low-alpha tint pair for a "pay attention" callout box (a safety warning),
/// built from [`ERROR_TEXT`] so the tint and the icon/border read as the same
/// color family without the body text itself needing to turn red.
pub(crate) const WARNING_BG: Color = Color::srgba(0.85, 0.35, 0.35, 0.12);
pub(crate) const WARNING_BORDER: Color = Color::srgba(0.85, 0.35, 0.35, 0.5);
/// The same shape as [`WARNING_BG`]/[`WARNING_BORDER`], tinted toward
/// [`BOOK_ACCENT`]-family blue instead, for a "worth trying" callout rather
/// than a caution.
pub(crate) const TIP_BG: Color = Color::srgba(0.34, 0.66, 0.82, 0.12);
pub(crate) const TIP_BORDER: Color = Color::srgba(0.34, 0.66, 0.82, 0.5);

/// Named type scale, so a call site picks a size that means something instead
/// of a bare float. Not retrofitted across every existing `label()` call in
/// this file (that's a purely mechanical follow-up); used for new/changed
/// call sites going forward, starting with the ones fixed alongside it.
pub(crate) const FONT_SIZE_HEADING: f32 = 22.0;
pub(crate) const FONT_SIZE_TITLE: f32 = 17.0;
pub(crate) const FONT_SIZE_BODY: f32 = 16.0;
pub(crate) const FONT_SIZE_LABEL: f32 = 14.0;
pub(crate) const FONT_SIZE_CAPTION: f32 = 13.0;
pub(crate) const FONT_SIZE_LABEL_SMALL: f32 = 12.0;

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            OnEnter(AppState::Playing),
            (
                spawn_order_queue,
                reset_radio_dispatch,
                spawn_vitals_panel,
                spawn_hotbar,
                spawn_room_label,
                bookmarks::spawn_bookmarks,
                reset_social_view,
            ),
        )
        .add_systems(
            Update,
            (
                handle_panel_clicks,
                drag_thermostat_slider,
                button_feedback,
                // Before `sync_panel`: it reads `BookmarkFlight` to decide
                // whether the screen a tab is turning into may draw yet, and a
                // frame-stale answer there shows the panel one frame early.
                (
                    bookmarks::click_bookmarks,
                    bookmarks::update_bookmark_keys,
                    bookmarks::animate_bookmarks,
                ),
                sync_panel,
                // After the spawn, so a panel drawn this frame takes its first
                // entrance step now rather than flashing at full size for one
                // frame — the same reason `sync_thermostat_slider` sits here.
                bookmarks::animate_panel_entrance,
                finish_radio_auto_scroll,
                // After the rebuild, so a track drawn this frame has its fill
                // patched in this frame rather than sitting one frame stale —
                // the same ordering `settings::sync_sliders` uses and for the
                // same reason.
                sync_thermostat_slider,
                animate_beaker_previews,
                update_phase_banner,
                update_order_queue,
                (update_vitals_panel, update_hotbar, update_room_label),
                (update_radio_dispatch, animate_radio_dispatch),
                scroll_active_pane,
                announce_discoveries,
                announce_accepting_toggle,
                show_toasts,
                expire_toasts,
                tooltip::update_tooltips,
            )
                .chain()
                .run_if(in_state(AppState::Playing)),
        )
        // Reuses the one scroll idiom above for the main menu's own
        // Settings/Controls screens — see `ScrollPane`'s doc comment. A
        // separate registration rather than widening the chain above's
        // `run_if`: everything else in that chain is Playing-only gameplay
        // presentation with no business running in the menu at all.
        .add_systems(
            Update,
            scroll_active_pane.run_if(in_state(AppState::MainMenu)),
        )
        // The bundled bitmap font intentionally has a compact glyph set.
        // Normalize presentation text after every state-specific UI system so
        // authored prose and live readouts cannot render missing-glyph boxes.
        .add_systems(Last, normalize_changed_ui_text)
        .init_resource::<BookView>()
        .init_resource::<BookIconAssets>()
        .init_resource::<TooltipState>()
        .init_resource::<mixing::PackagingDraft>()
        .add_systems(
            Update,
            mixing::type_label
                .before(crate::inspection::input)
                .before(crate::interaction::panel_input)
                .before(handle_panel_clicks)
                .run_if(in_state(AppState::Playing)),
        )
        .init_resource::<HplcView>()
        .init_resource::<SocialView>()
        .init_resource::<LastPanel>()
        .init_resource::<LastSignState>()
        .init_resource::<ThermostatDrag>()
        .init_resource::<RadioDispatchQueue>()
        .add_message::<ShowToast>();
    }
}

/// Root of the currently open panel.
#[derive(Component)]
struct PanelRoot;

/// Marks the currently chosen option in a group of buttons.
///
/// `pub(crate)` only because it is part of `button_feedback`'s query, which the
/// menu runs too; nothing outside this module adds it.
#[derive(Component)]
pub(crate) struct Selected;

/// A button-shaped drag surface whose authored background is meaningful.
/// Generic hover/pressed feedback must not repaint the entire slider track.
#[derive(Component)]
pub(crate) struct PreserveButtonBackground;

/// What a button does when clicked.
#[derive(Component, Clone)]
enum PanelAction {
    SetAmount(Units),
    Dispense(ReagentId),
    UnlockAll,
    /// Which physical vessel to act on: mixers expose A/B and delivery
    /// windows expose all three tray positions.
    Eject(MachineSlot),
    /// Take one named item out of the open locker. Carries the item because a
    /// locker holds many, unlike every slot in the lab, which holds one.
    Take(Entity),
    Empty(MachineSlot),
    ToBuffer(ReagentId, Units, MachineSlot),
    ToContainer(ReagentId, Units, MachineSlot),
    Agitate(AgitateDirection),
    Package(ContainerKind),
    FinishPackage,
    FocusPackageLabel,
    PrintReport(u64),
    Analyze,
    /// Locally highlights a band in the analyzer. The actual separation is
    /// still requested by [`PanelAction::Purify`]'s single Start button.
    SelectHplc(ReagentId),
    Purify(ReagentId),
    Grind {
        all: bool,
    },
    TogglePower,
    BuyHint(ReactionId),
    ShowCategory(Option<Category>),
    ShowBookFilter(BookFilter),
    SetBookPage(usize),
    OpenRecipe(ReactionId),
    CloseRecipe,
    CloseBook,
    ToggleAcceptingOrders,
    CallItAShift,
    OpenUpAgain,
    Requisition(RequisitionKind),
    NpcPack(NpcRequisitionKind),
    OpenOrders,
    ShowSocialDepartment(Department),
    SelectSocialResident(String),
    CloseSocial,
    Close,
}

/// Local navigation inside the social directory. The relationships and heard
/// lines are save data; which department and person this player is currently
/// looking at are only presentation.
#[derive(Resource, Clone, Debug, PartialEq, Eq)]
struct SocialView {
    department: Department,
    resident: Option<String>,
}

impl Default for SocialView {
    fn default() -> Self {
        Self {
            department: Department::Medical,
            resident: None,
        }
    }
}

fn reset_social_view(mut view: ResMut<SocialView>) {
    *view = SocialView::default();
}

/// Which heading the reference book is open at. `None` is the "All" tab.
///
/// Local presentation state: it is neither replicated nor saved, because which
/// page a chemist happens to have open is nobody else's business and is not
/// worth a line in `save.ron`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum BookFilter {
    #[default]
    All,
    Recorded,
    Ready,
    Frontier,
    Locked,
}

impl BookFilter {
    const ALL: [Self; 5] = [
        Self::All,
        Self::Recorded,
        Self::Ready,
        Self::Frontier,
        Self::Locked,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Recorded => "Recorded",
            Self::Ready => "Ready",
            Self::Frontier => "Frontier",
            Self::Locked => "Locked",
        }
    }

    fn icon(self) -> BookIcon {
        match self {
            Self::All => BookIcon::All,
            Self::Recorded => BookIcon::Recorded,
            Self::Ready => BookIcon::Ready,
            Self::Frontier => BookIcon::Frontier,
            Self::Locked => BookIcon::Locked,
        }
    }

    fn explanation(self) -> &'static str {
        match self {
            Self::All => "Every method in the selected chemistry category.",
            Self::Recorded => "The complete formula and handling record is in your notebook.",
            Self::Ready => "Every required material is obtainable with your current knowledge.",
            Self::Frontier => "One nearby precursor discovery will bring this within reach.",
            Self::Locked => "The dependency chain has not reached your research frontier.",
        }
    }
}

/// How close a recipe is to being usable. This is deliberately based on the
/// player's actual notebook and obtainable reagents, not on an authored tier:
/// when they discover a precursor, its downstream methods move forward too.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum RecipeState {
    Recorded,
    Ready,
    Frontier,
    Locked,
}

impl RecipeState {
    fn label(self) -> &'static str {
        match self {
            Self::Recorded => "RECORDED",
            Self::Ready => "READY TO STUDY",
            Self::Frontier => "ON THE FRONTIER",
            Self::Locked => "LOCKED",
        }
    }

    fn color(self) -> Color {
        match self {
            Self::Recorded => GOOD_TEXT,
            Self::Ready => Color::srgb(0.42, 0.76, 0.94),
            Self::Frontier => Color::srgb(0.90, 0.72, 0.34),
            Self::Locked => TEXT_DIM,
        }
    }

    fn icon(self) -> BookIcon {
        match self {
            Self::Recorded => BookIcon::Recorded,
            Self::Ready => BookIcon::Ready,
            Self::Frontier => BookIcon::Frontier,
            Self::Locked => BookIcon::Locked,
        }
    }

    fn explanation(self) -> &'static str {
        match self {
            Self::Recorded => {
                "Complete method recorded. Formula, process and handling data are available."
            }
            Self::Ready => {
                "All inputs can be obtained now. Experiment or spend research to reveal the method."
            }
            Self::Frontier => "Discover one nearby precursor to bring this method within reach.",
            Self::Locked => "This method remains beyond the current dependency frontier.",
        }
    }
}

#[derive(Resource, Default)]
struct BookView {
    category: Option<Category>,
    filter: BookFilter,
    page: usize,
    /// The recipe whose tree screen is open, if any. `None` is the list.
    open_recipe: Option<ReactionId>,
}

/// The analyzer band this player has highlighted.
///
/// Selection is presentation state, like the open page of the research book:
/// it is neither saved nor replicated. The separation result itself remains
/// authoritative and replicated through [`HplcReport`].
#[derive(Resource, Default)]
struct HplcView {
    selected: Option<ReagentId>,
}

/// Local-only panel state bundled so [`sync_panel`] remains within Bevy's
/// system-parameter arity while the book and analyzer each keep independent
/// presentation state.
#[derive(SystemParam)]
struct PanelViews<'w, 's> {
    book: Res<'w, BookView>,
    hplc: Res<'w, HplcView>,
    icons: Res<'w, BookIconAssets>,
    packaging: Res<'w, mixing::PackagingDraft>,
    mixing_scroll: Query<'w, 's, (&'static mixing::ScrollArea, &'static ScrollPosition)>,
    reports: Query<'w, 's, &'static crate::analysis_reports::AnalyzerSnapshot>,
}

/// Everything the open panel displays, flattened for comparison.
///
/// Rebuilding is driven by comparing this against last frame's rather than by
/// change-detection filters. Change detection here has to span the mode, the
/// machine, a container that can be swapped out from under the panel, and the
/// buffer — and a missed signal shows the player stale contents, which in a
/// chemistry game means dosing off numbers that are no longer true. The
/// comparison is a few dozen integers; correctness is worth far more.
/// The last signature [`sync_panel`] drew, so an unchanged panel is not
/// rebuilt every frame.
///
/// A resource rather than the `Local<PanelSignature>` it was, so
/// `crate::session` can clear it: leaving a stale signature behind meant the
/// first panel opened in a *second* session could compare equal to one from
/// the first and simply never draw.
#[derive(Resource, Default)]
pub struct LastPanel(PanelSignature);

#[derive(PartialEq)]
struct PanelSignature {
    mode: InteractionMode,
    container: Option<Entity>,
    contents: Vec<(ReagentId, Units)>,
    /// Per-reagent purity and pH fingerprints. The HPLC renders these values,
    /// and two reagents can trade quality while leaving the aggregate average
    /// unchanged, so the aggregate `quality` tuple cannot invalidate it alone.
    profiles: Vec<(ReagentId, i16, i16)>,
    /// pH to two decimals and average purity to whole percent, matching the
    /// visible readout. Buffering may change pH without changing any amount,
    /// so `contents` alone cannot invalidate the panel.
    quality: Option<(i16, i16)>,
    /// The Mixing Chamber's second beaker. `None` for every other machine,
    /// which never has one.
    container_b: Option<Entity>,
    contents_b: Vec<(ReagentId, Units)>,
    profiles_b: Vec<(ReagentId, i16, i16)>,
    quality_b: Option<(i16, i16)>,
    /// Delivery windows alone have a third tray position.
    container_c: Option<Entity>,
    contents_c: Vec<(ReagentId, Units)>,
    profiles_c: Vec<(ReagentId, i16, i16)>,
    quality_c: Option<(i16, i16)>,
    buffer: Vec<(ReagentId, Units)>,
    hopper: Vec<ProduceId>,
    /// What the open locker holds, already rendered to the lines the panel
    /// draws. Kept as the finished strings rather than the entity ids because
    /// a beaker's *contents* can change while it sits on the shelf, and the ids
    /// alone would not notice.
    stored: Vec<StoredItem>,
    /// A batch is still running in the loaded container. Here so the readout
    /// stops saying so the moment it finishes; the *numbers* moving while it
    /// runs are already covered by `contents` above, which is what actually
    /// shows the batch progressing.
    reacting: bool,
    reacting_b: bool,
    reacting_c: bool,
    /// Replicated Mixing Chamber run, rounded to tenths so its visible timer
    /// updates smoothly without rebuilding the entire panel every frame.
    agitation: Option<(Entity, MachineSlot, i32, i32)>,
    packaging: mixing::PackagingDraft,
    report: Option<u64>,
    buffer_metrics: Option<(i32, i16, i16)>,
    security_text: String,
    amount: Option<Units>,
    /// Whole seconds of EMP lockout remaining. The coarse bucket keeps the
    /// warning live without rebuilding a complex instrument every frame.
    disabled_seconds: u16,
    known_recipes: usize,
    career_stage: CareerStage,
    /// The book's open heading. Here rather than tracked separately because
    /// switching tab is exactly the same kind of change as any other: it
    /// alters what the panel shows, so it rebuilds the panel.
    book_category: Option<Category>,
    book_filter: BookFilter,
    book_page: usize,
    /// The recipe whose tree screen is open, if any — same rationale as
    /// `book_category`: opening or closing it changes what the panel shows.
    book_recipe: Option<ReactionId>,
    /// The standing board draws entirely from these, so without them its
    /// panel would freeze on whatever it happened to show first.
    department_standing: Vec<(Department, i32)>,
    /// Botanist Ivy's own individual standing — what her personal shop's
    /// affordability dimming reads. Almost always moves in step with
    /// `department_standing`'s Service entry (an individual delta moves that
    /// department's shown average too), but tracked explicitly rather than
    /// relying on that correlation, the same way `arc` is tracked separately
    /// from the plot number it is derived from.
    ivy_standing: i32,
    accepting_orders: bool,
    /// Which of the board's two tabs is open — same rationale as
    /// `book_category`: switching tab changes what the panel shows, so it
    /// rebuilds the panel.
    /// Local social-directory navigation and the public, qualitative snapshot
    /// it renders. Exact impressions and secret roles never enter this type.
    social_view: SocialView,
    social_residents: Vec<ResidentSocialSnapshot>,
    /// Latest delivered transmission. Unlike the old snapshot-only radio tab,
    /// this makes an open board update as traffic arrives.
    radio_sequence: Option<u64>,
    /// Which of the board's three stages is drawn: open, wrapping up, or
    /// debriefing. Carries the whole report rather than just the flag, because
    /// the numbers on a debrief keep moving — the other chemist can still be
    /// delivering while this one reads it — and a stale debrief is exactly the
    /// stale readout this signature exists to prevent.
    board: BoardStage,
    research_points: u32,
    /// What the standing board says about the campaign. Same reasoning as
    /// `department_standing`: the board draws from it, so without it here the
    /// board would freeze on whatever the arc happened to be when it first
    /// opened. Deliberately *not* the plot number — that is never shown, and
    /// tracking it would rebuild the panel every time the meter ticked.
    arc: Option<ArcHeadline>,
    /// Chamber state. The sample's temperature is **rounded to 5K** for exactly
    /// the reason the countdown above is absent: while a chamber runs it moves
    /// every frame, and comparing the raw value would rebuild the panel every
    /// frame with it. Five kelvin is fine enough to watch a batch climb and
    /// coarse enough to cost one rebuild every second or so.
    ///
    /// The *target* is deliberately **not** here any more — it used to be,
    /// rounded the same way, but that meant every ~5K of dragging the slider
    /// rebuilt the whole panel out from under the mouse. The live target is
    /// patched in place by `sync_thermostat_slider` instead, the same
    /// "rebuild on structure, patch on value" split `settings::sync_sliders`
    /// uses for exactly the same reason.
    temperature: Option<i32>,
    powered: bool,
    /// Latest analyzer result, if this machine has run a separation.
    hplc_report: Option<HplcReport>,
    /// Local analyzer band selection. Included so choosing a row immediately
    /// repaints the chromatogram and Start button without touching chemistry.
    hplc_selection: Option<ReagentId>,
}

impl Default for PanelSignature {
    fn default() -> Self {
        PanelSignature {
            // Deliberately unreachable: the default must differ from any real
            // state, or the first frame with no panel open would compare equal
            // and skip the despawn of a panel left over from last frame. A
            // placeholder machine is the one mode no player can ever be in —
            // `ReadingBook(None)` is a real state a chemist can start a frame
            // in, so it would not do.
            mode: InteractionMode::UsingMachine(Entity::PLACEHOLDER),
            container: None,
            contents: Vec::new(),
            profiles: Vec::new(),
            quality: None,
            container_b: None,
            contents_b: Vec::new(),
            profiles_b: Vec::new(),
            quality_b: None,
            container_c: None,
            contents_c: Vec::new(),
            profiles_c: Vec::new(),
            quality_c: None,
            buffer: Vec::new(),
            hopper: Vec::new(),
            stored: Vec::new(),
            reacting: false,
            reacting_b: false,
            reacting_c: false,
            agitation: None,
            packaging: default(),
            report: None,
            buffer_metrics: None,
            security_text: String::new(),
            amount: None,
            disabled_seconds: u16::MAX,
            known_recipes: usize::MAX,
            career_stage: CareerStage::Mastery,
            book_category: None,
            book_filter: BookFilter::All,
            book_page: 0,
            book_recipe: None,
            department_standing: Vec::new(),
            ivy_standing: i32::MIN,
            // Neither `true` nor `false` alone is guaranteed to differ from
            // the first real frame's value, so the vector above carries the
            // "never compared yet" signal on its own — this field just needs
            // *a* starting value.
            accepting_orders: false,
            // Same reasoning again: the vector above is what guarantees a
            // difference on the first comparison, so this only needs a value.
            social_view: SocialView::default(),
            social_residents: Vec::new(),
            radio_sequence: None,
            board: BoardStage::Open,
            research_points: u32::MAX,
            arc: None,
            temperature: Some(i32::MAX),
            powered: true,
            hplc_report: None,
            hplc_selection: None,
        }
    }
}

/// Which of the standing board's three stages is showing.
///
/// The board is the only place a shift can be ended, and ending one is two
/// deliberate steps: put the sign down, then — once whoever is still at the
/// counter has been served or has given up — call it. Modelling that as one
/// value rather than a pair of booleans read at three call sites is what keeps
/// the panel, the signature and the click handler from ever disagreeing about
/// which buttons exist.
#[derive(Debug, PartialEq, Eq, Clone)]
enum BoardStage {
    /// Taking requests. One button: put the sign down.
    Open,
    /// The sign is down and the lab is closing up. `clear` is whether the
    /// counter has actually emptied, which is what makes "Call it a shift"
    /// live rather than merely drawn.
    WrappingUp { clear: bool },
    /// The shift has been called. This is the debrief, and it is a *screen*,
    /// not a state gate: the world is still running behind it, and the other
    /// chemist can still be at the window while this one reads.
    Debrief(ShiftReport),
}

/// Everything the standing board is allowed to say about the campaign.
///
/// Built once, in [`arc_headline`], so the rule about what may be shown at
/// which [`Reveal`] tier lives in exactly one place rather than being spread
/// through the panel-drawing code.
/// `pub(crate)` for `crate::ending`, which needs exactly this rule and must not
/// re-derive it: "may the screen name them?" has one correct answer and it is
/// this one.
#[derive(PartialEq, Eq, Clone)]
pub(crate) struct ArcHeadline {
    /// `None` until [`Reveal::Named`] — before that the board can say
    /// something is wrong, but not what.
    pub(crate) name: Option<String>,
    /// Counter-track progress, once the track has opened.
    pub(crate) countered: usize,
    pub(crate) total: usize,
    /// Cult-only case file: discovered anchors and the number neutralised.
    pub(crate) incidents: usize,
    pub(crate) treated_incidents: usize,
    support_only: bool,
    resolved: Option<bool>,
}

/// What the board may show, given how much the station has worked out.
///
/// `None` while the antagonist is still [`Reveal::Hidden`]: the board is a
/// public notice, and the whole arc depends on it not being one yet.
pub(crate) fn arc_headline(campaign: &Campaign, script: Option<&ArcScript>) -> Option<ArcHeadline> {
    // A physical ritual anchor is already evidence in the chemist's own lab.
    // It may not identify the Cult by name yet, but hiding the case file after
    // the player has seen it would make the new investigation unreadable.
    if campaign.reveal == Reveal::Hidden
        && campaign.outcome.is_none()
        && campaign.cult_incidents.is_empty()
    {
        return None;
    }
    let named = campaign.reveal == Reveal::Named || campaign.outcome.is_some();
    Some(ArcHeadline {
        name: named.then(|| {
            script
                .and_then(|script| script.antagonist(campaign.antag))
                .map(|def| def.display.clone())
                // The script is an asset; if it somehow is not loaded, the
                // short menu label still names the right thing.
                .unwrap_or_else(|| campaign.antag.label().to_string())
        }),
        countered: campaign.countered.iter().filter(|done| **done).count(),
        total: campaign.countered.len(),
        incidents: campaign.cult_incidents.len(),
        treated_incidents: campaign.cult_incidents.iter().filter(|done| **done).count(),
        support_only: script
            .and_then(|script| script.antagonist(campaign.antag))
            .is_some_and(|def| def.counter_role == crate::arc::CounterTrackRole::SupportOnly),
        resolved: campaign.player_won(),
    })
}

/// Rounds a temperature to the granularity [`PanelSignature`] compares at.
fn panel_temperature(kelvin: Kelvin) -> i32 {
    (kelvin.0 / 5.0).round() as i32
}

fn panel_quality(solution: &chem_sim::Solution) -> (i16, i16) {
    (
        (solution.ph() * 100.0).round() as i16,
        (solution.average_purity() * 100.0).round() as i16,
    )
}

fn panel_profiles(solution: &chem_sim::Solution) -> Vec<(ReagentId, i16, i16)> {
    solution
        .iter()
        .map(|(reagent, _)| {
            (
                reagent,
                (solution.purity_of(reagent) * 100.0).round() as i16,
                (solution.reagent_ph(reagent) * 100.0).round() as i16,
            )
        })
        .collect()
}

/// The optional fittings a panel draws from: not every machine has an amount
/// dial, a buffer or a hopper, and the panel body decides what to do with the
/// ones its machine happens to carry.
type MachineParts<'w, 's> = Query<
    'w,
    's,
    (
        &'static Machine,
        Option<&'static DispenseAmount>,
        Option<&'static Buffer>,
        Option<&'static Hopper>,
        Option<&'static Thermostat>,
        Option<&'static AgitationRun>,
        Option<&'static HplcReport>,
    ),
>;

/// Whatever a container is holding.
/// What is in the slot, and what is written on the outside of it.
///
/// `crate::labels::Label` rides along here rather than as a seventeenth
/// `sync_panel` parameter, which Bevy's sixteen-parameter ceiling has no room
/// for — and it belongs here anyway: a container's label is part of reading
/// the container, not a separate lookup.
type SlotContents<'w, 's> =
    Query<'w, 's, (&'static Container, Option<&'static crate::labels::Label>)>;

/// All machine placement relations, bundled so the tertiary delivery tray
/// does not push panel synchronization over Bevy's parameter ceiling.
#[derive(SystemParam)]
struct SlottedView<'w, 's> {
    a: Query<'w, 's, (Entity, &'static InSlot)>,
    b: Query<'w, 's, (Entity, &'static InSlotB)>,
    c: Query<'w, 's, (Entity, &'static InSlotC)>,
}

/// What a locker's panel reads. Bundled because `sync_panel` is already close
/// to Bevy's sixteen-parameter ceiling, and because these two are only ever
/// used together.
#[derive(SystemParam)]
struct StorageView<'w, 's> {
    stored: Query<'w, 's, (Entity, &'static Stored)>,
    /// The crosshair label every pickable thing already carries. Reading the
    /// name from here rather than matching on the item's type is what lets a
    /// locker list something this module has never heard of.
    labels: Query<'w, 's, &'static Interactable>,
    written: Query<'w, 's, &'static crate::labels::Label>,
}

/// What the standing board reads on top of [`Shift`] itself.
///
/// Bundled for the same reason [`StorageView`] is — `sync_panel` is one
/// parameter off Bevy's sixteen-parameter ceiling — and because the query
/// exists solely to answer the board's one extra question: is anyone still
/// waiting at the counter?
#[derive(SystemParam)]
struct BoardView<'w, 's> {
    shift: Res<'w, Shift>,
    security: Option<Res<'w, crate::security_case::SecurityCaseSummary>>,
    /// Sato's/Lindqvist's own personal-pack catalogs live off `StationData.
    /// config.supply`, not a promoted resource of their own the way produce
    /// packs are — folded in here for the same "one off the ceiling" reason
    /// `radio`/`tab` already are.
    station: Option<Res<'w, StationData>>,
    /// Crew who walked in and are still holding an order. Residents are
    /// filtered out because they live here: they are never what a shift is
    /// waiting on, and counting them would mean the sign could never come
    /// down at all.
    waiting: Query<
        'w,
        's,
        (),
        (
            Or<(With<Order>, With<crate::order_intake::AwaitingConversation>)>,
            crate::crew::NotResident,
        ),
    >,
    /// The full chatter history, for the board's own scrollable section —
    /// folded in here rather than added to `sync_panel` directly, which is
    /// already at Bevy's sixteen-parameter ceiling.
    radio: Res<'w, RadioLog>,
    /// Which of the board's two tabs is open — same reason `radio` is here
    /// rather than a bare `sync_panel` parameter.
    radio_scroll:
        Query<'w, 's, (&'static ScrollPosition, &'static ComputedNode), With<RadioHistoryPane>>,
    social_view: Res<'w, SocialView>,
    social_residents: Query<
        'w,
        's,
        (
            &'static CrewMember,
            Option<&'static PublicRelationship>,
            Option<&'static ConversationHistory>,
        ),
    >,
}

impl BoardView<'_, '_> {
    fn stage(&self, knowledge: &Knowledge) -> BoardStage {
        board_stage(&self.shift, knowledge, self.waiting.iter().count())
    }

    fn radio_scroll_state(&self) -> RadioScrollState {
        let Some((position, computed)) = self.radio_scroll.iter().next() else {
            return RadioScrollState {
                offset: 0.0,
                at_bottom: true,
            };
        };
        let limit = (computed.content_size().y - computed.size().y).max(0.0);
        RadioScrollState {
            offset: position.y,
            at_bottom: position.y >= limit - 4.0,
        }
    }

    fn social_snapshot(&self) -> Vec<ResidentSocialSnapshot> {
        crate::social::RESIDENT_NAMES
            .into_iter()
            .map(|name| {
                let visible = self
                    .social_residents
                    .iter()
                    .find(|(member, _, _)| member.name == name);
                ResidentSocialSnapshot {
                    name: name.to_string(),
                    relationship: visible
                        .and_then(|(_, relationship, _)| relationship.cloned())
                        .unwrap_or_default(),
                    history: visible
                        .and_then(|(_, _, history)| history.cloned())
                        .unwrap_or_default(),
                }
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ResidentSocialSnapshot {
    name: String,
    relationship: PublicRelationship,
    history: ConversationHistory,
}

#[derive(Clone, Copy)]
struct RadioScrollState {
    offset: f32,
    at_bottom: bool,
}

/// Which stage the board is at right now.
///
/// `clear` comes from `shift::can_call_it` rather than from a local
/// `waiting == 0` so the button and the authority's own check are the same
/// rule — a button that lights up on a condition the server then refuses reads
/// as the game being broken.
///
/// Pure, and separate from [`BoardView::stage`], so the three-stage rule can be
/// checked without a world to hang a query off.
fn board_stage(shift: &Shift, knowledge: &Knowledge, waiting: usize) -> BoardStage {
    if shift.called {
        return BoardStage::Debrief(shift_report(shift, knowledge));
    }
    if shift.accepting_orders {
        return BoardStage::Open;
    }
    BoardStage::WrappingUp {
        clear: can_call_it(shift, waiting),
    }
}

/// One line of a locker's contents.
#[derive(Clone, PartialEq, Eq)]
struct StoredItem {
    item: Entity,
    name: String,
    /// What is in it, for glassware. Empty for anything that is not a
    /// container, which is most of what a locker will eventually hold.
    detail: String,
}

/// Reads a locker's contents into the lines its panel draws.
///
/// Glassware is the one special case, because "Beaker" on its own is useless
/// when there are six of them on the shelf. Everything else falls back to the
/// `Interactable` label it already needed to be pickable at all — so a new kind
/// of item shows up here correctly named without this function learning
/// anything about it.
fn stored_items(
    locker: Entity,
    _db: &ChemDb,
    view: &StorageView,
    containers: &SlotContents,
) -> Vec<StoredItem> {
    stored_in(locker, &view.stored)
        .into_iter()
        .map(|item| {
            let entry = containers.get(item).ok();
            let container = entry.map(|(container, _)| container);
            // A shelf of identical beakers is exactly what labelling is for,
            // so what is written on one wins over its kind here.
            let name = view
                .written
                .get(item)
                .ok()
                .filter(|marked| !marked.0.trim().is_empty())
                .map(|marked| format!("\"{}\"", marked.0))
                .or_else(|| container.map(|container| container.kind.label().to_string()))
                .or_else(|| view.labels.get(item).ok().map(|label| label.label.clone()))
                .unwrap_or_else(|| "Item".to_string());
            let detail = container.map_or_else(String::new, |container| {
                if entry.is_some_and(|(_, label)| label.is_some()) {
                    String::new()
                } else {
                    crate::inspection::container_description(container)
                }
            });
            StoredItem { item, name, detail }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
/// Whether these two modes are the *same screen*, for entrance purposes.
///
/// Coarser than `==`: a chemist opening the book over a machine and one opening
/// it on the floor are the same screen arriving, and the crew directory stepping
/// to its own order list is not a new arrival either. Only a change of screen
/// replays the entrance animation.
fn same_screen(before: InteractionMode, after: InteractionMode) -> bool {
    fn screen(mode: InteractionMode) -> u8 {
        match mode {
            InteractionMode::ReadingBook(_) => 1,
            InteractionMode::Social { .. } | InteractionMode::OrderDirectory { .. } => 2,
            _ => 0,
        }
    }
    screen(before) == screen(after)
}

fn sync_panel(
    mut commands: Commands,
    db: Res<ChemDb>,
    existing: Query<Entity, With<PanelRoot>>,
    modes: Query<&InteractionMode, With<LocalPlayer>>,
    machines: MachineParts,
    slotted: SlottedView,
    containers: SlotContents,
    storage: StorageView,
    knowledge: Res<Knowledge>,
    views: PanelViews,
    catalog: Option<Res<ProduceCatalog>>,
    campaign: Option<Res<Campaign>>,
    arc_script: Option<Res<crate::arc::Script>>,
    board: BoardView,
    previous: ResMut<LastPanel>,
    training: crate::tutorial::TrainingEquipment,
) {
    let shift = &board.shift;
    let mode = modes.iter().next().copied().unwrap_or_default();
    let open_machine = match mode {
        InteractionMode::UsingMachine(machine) => Some(machine),
        _ => None,
    };
    let loaded_entity = open_machine.and_then(|machine| slotted_container(machine, &slotted.a));
    let slot = loaded_entity.and_then(|entity| containers.get(entity).ok());
    let loaded = slot.map(|(container, _)| container);
    let marked = slot.and_then(|(_, marked)| marked);
    // The Mixing Chamber's second beaker. `slotted_container_b` simply never
    // matches for any other machine, since only the Mixing Chamber ever gets
    // an `InSlotB` in the first place.
    let loaded_entity_b = open_machine.and_then(|machine| slotted_container_b(machine, &slotted.b));
    let loaded_b = loaded_entity_b
        .and_then(|entity| containers.get(entity).ok())
        .map(|(container, _)| container);
    let marked_b = loaded_entity_b
        .and_then(|entity| containers.get(entity).ok())
        .and_then(|(_, marked)| marked);
    let loaded_entity_c = open_machine.and_then(|machine| slotted_container_c(machine, &slotted.c));
    let slot_c = loaded_entity_c.and_then(|entity| containers.get(entity).ok());
    let loaded_c = slot_c.map(|(container, _)| container);
    let marked_c = slot_c.and_then(|(_, marked)| marked);
    // Derived from the beaker and the chemistry rather than read off a marker
    // component, so a guest can answer it too. `machines::Reacting` is the
    // authority's own bookkeeping and is deliberately not on the wire; a
    // client holds the same solution and the same recipes, so it does not need
    // to be told.
    let reacting =
        loaded.is_some_and(|container| chem_sim::is_reacting(&container.solution, &db.reactions));
    let reacting_b =
        loaded_b.is_some_and(|container| chem_sim::is_reacting(&container.solution, &db.reactions));
    let reacting_c =
        loaded_c.is_some_and(|container| chem_sim::is_reacting(&container.solution, &db.reactions));
    let machine_parts = open_machine.and_then(|machine| machines.get(machine).ok());

    // Built before the signature so both the comparison and the panel body can
    // read it — the signature itself is moved into `previous` on the way past.
    let arc = campaign
        .as_deref()
        .and_then(|campaign| arc_headline(campaign, arc_script.as_deref().map(|s| &s.0)));
    // Same reasoning as `arc` above: built once, read by both the comparison
    // and the panel body, because the signature is moved into `previous`.
    let stage = board.stage(&knowledge);
    let career_stage = CareerStage::from_progress(
        board.shift.succeeded,
        knowledge.known_count(),
        db.reactions.recipe_count(),
    );
    let social_residents = board.social_snapshot();

    // Only for the locker, and only while its panel is open. Reading every
    // stored item on every frame of every other panel would be a scan of the
    // whole lab's contents to produce an empty vector.
    let stored = match (open_machine, machine_parts) {
        (Some(locker), Some((machine, ..))) if machine.kind == MachineKind::Locker => {
            stored_items(locker, &db, &storage, &containers)
        }
        _ => Vec::new(),
    };

    let signature = PanelSignature {
        packaging: views.packaging.clone(),
        buffer_metrics: machine_parts.and_then(|(_, _, b, ..)| b).map(|b| {
            let (ph, purity) = panel_quality(&b.0);
            ((b.0.temperature.0 * 10.0) as i32, ph, purity)
        }),
        security_text: board
            .security
            .as_deref()
            .map_or(String::new(), |s| s.text.clone()),
        report: open_machine
            .and_then(|e| views.reports.get(e).ok())
            .map(|s| s.report.id),
        mode,
        container: loaded_entity,
        contents: loaded
            .map(|container| container.solution.iter().collect())
            .unwrap_or_default(),
        profiles: loaded
            .map(|container| panel_profiles(&container.solution))
            .unwrap_or_default(),
        quality: loaded.map(|container| panel_quality(&container.solution)),
        container_b: loaded_entity_b,
        contents_b: loaded_b
            .map(|container| container.solution.iter().collect())
            .unwrap_or_default(),
        profiles_b: loaded_b
            .map(|container| panel_profiles(&container.solution))
            .unwrap_or_default(),
        quality_b: loaded_b.map(|container| panel_quality(&container.solution)),
        container_c: loaded_entity_c,
        contents_c: loaded_c
            .map(|container| container.solution.iter().collect())
            .unwrap_or_default(),
        profiles_c: loaded_c
            .map(|container| panel_profiles(&container.solution))
            .unwrap_or_default(),
        quality_c: loaded_c.map(|container| panel_quality(&container.solution)),
        buffer: machine_parts
            .and_then(|(_, _, buffer, _, _, _, _)| buffer)
            .map(|buffer| buffer.0.iter().collect())
            .unwrap_or_default(),
        hopper: machine_parts
            .and_then(|(_, _, _, hopper, _, _, _)| hopper)
            .map(|hopper| hopper.0.clone())
            .unwrap_or_default(),
        stored: stored.clone(),
        reacting,
        reacting_b,
        reacting_c,
        agitation: machine_parts
            .and_then(|(_, _, _, _, _, run, _)| run)
            .map(|run| {
                (
                    run.destination,
                    run.direction.destination(),
                    (run.elapsed_secs * 10.0).floor() as i32,
                    (run.expected_secs * 10.0).round() as i32,
                )
            }),
        amount: machine_parts
            .and_then(|(_, amount, _, _, _, _, _)| amount)
            .map(|a| a.0),
        disabled_seconds: machine_parts
            .map(|(machine, ..)| machine.disabled_for.ceil().max(0.0) as u16)
            .unwrap_or(0),
        known_recipes: knowledge.known_count(),
        career_stage,
        book_category: views.book.category,
        book_filter: views.book.filter,
        book_page: views.book.page,
        book_recipe: views.book.open_recipe,
        department_standing: Department::ALL
            .into_iter()
            .map(|dept| (dept, shift.standing(dept)))
            .collect(),
        ivy_standing: shift.npc_standing("Botanist Ivy"),
        accepting_orders: shift.accepting_orders,
        social_view: board.social_view.clone(),
        social_residents: social_residents.clone(),
        radio_sequence: board.radio.entries.back().map(|entry| entry.sequence),
        board: stage.clone(),
        research_points: knowledge.research_points,
        arc: arc.clone(),
        // Only tracked while a chamber panel is open, so no other machine pays
        // for the extra comparison.
        temperature: machine_parts
            .filter(|(machine, ..)| machine.kind == MachineKind::ReactionChamber)
            .and(loaded)
            .map(|container| panel_temperature(container.solution.temperature)),
        powered: machine_parts
            .and_then(|(_, _, _, _, thermostat, _, _)| thermostat)
            .is_some_and(|thermostat| thermostat.powered),
        hplc_report: machine_parts
            .filter(|(machine, ..)| machine.kind == MachineKind::Analyzer)
            .and_then(|(_, _, _, _, _, _, report)| report)
            .copied(),
        hplc_selection: views.hplc.selected,
    };

    if signature == previous.0 {
        return;
    }
    // An arrival, not a rebuild: the screen changed, so this panel is the one a
    // bookmark tab has just finished handing off to and should fade up. A page
    // turn or a live readout tick leaves the mode alone and must not replay the
    // entrance — `sync_panel` rebuilds far more often than the screen changes.
    let entrance = if same_screen(previous.0.mode, mode) {
        bookmarks::PanelEntrance::settled()
    } else {
        bookmarks::PanelEntrance::arriving()
    };
    previous.into_inner().0 = signature;

    for panel in &existing {
        commands.entity(panel).despawn();
    }

    // The book covers the machine panel rather than sitting beside it: there is
    // one screen's worth of room, and a chemist reading a recipe is reading,
    // not dispensing. The claim is still theirs, so it comes straight back.
    if let InteractionMode::ReadingBook(at_machine) = mode {
        spawn_reference_book(
            &mut commands,
            &db,
            &knowledge,
            &views.book,
            at_machine.is_some(),
            career_stage,
            board.shift.succeeded,
            &views.icons,
            entrance,
        );
        return;
    }
    if matches!(mode, InteractionMode::Social { .. }) {
        spawn_social_directory(
            &mut commands,
            &board.social_view,
            &social_residents,
            shift,
            catalog.as_deref(),
            board.station.as_deref(),
            &views.icons,
            entrance,
        );
        return;
    }
    if open_machine.is_none() {
        return;
    }
    let Some((machine, amount, buffer, hopper, thermostat, agitation, hplc_report)) = machine_parts
    else {
        return;
    };

    commands
        .spawn((
            // Full-screen flex wrapper so the panel stays centred at any
            // resolution without hardcoded offsets.
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            GlobalZIndex(30),
            PanelRoot,
            crate::until_we_leave_the_lab(),
        ))
        .with_children(|screen| {
            screen
                .spawn((
                    Node {
                        width: px(
                            if matches!(
                                machine.kind,
                                MachineKind::ChemMaster5000
                                    | MachineKind::StandingBoard
                                    | MachineKind::MixingChamber
                            ) {
                                1040
                            } else {
                                760
                            },
                        ),
                        max_width: percent(94),
                        max_height: percent(90),
                        flex_direction: FlexDirection::Column,
                        padding: UiRect::all(px(18)),
                        row_gap: px(10),
                        border: UiRect::all(px(2)),
                        border_radius: BorderRadius::all(px(8)),
                        ..default()
                    },
                    BackgroundColor(PANEL_BG),
                    BorderColor::all(Color::srgb(0.24, 0.40, 0.50)),
                ))
                .with_children(|panel| {
                    panel
                        .spawn(Node {
                            width: percent(100),
                            justify_content: JustifyContent::SpaceBetween,
                            align_items: AlignItems::Center,
                            ..default()
                        })
                        .with_children(|header| {
                            header.spawn(heading(machine.kind.label()));
                            header.spawn(button("Close  (Esc)", PanelAction::Close));
                        });
                    panel
                        .spawn((
                            Node {
                                width: percent(100),
                                max_height: vh(76),
                                flex_direction: FlexDirection::Column,
                                row_gap: px(10),
                                overflow: Overflow::scroll_y(),
                                ..default()
                            },
                            ScrollPosition::default(),
                            ScrollPane,
                        ))
                        .with_children(|body| {
                            if machine.disabled_for > 0.0 {
                                body.spawn(label(
                                    format!(
                                        "⚠ ELECTROMAGNETIC LOCKOUT — controls recovering in {:.0}s",
                                        machine.disabled_for.ceil()
                                    ),
                                    15.0,
                                    ERROR_TEXT,
                                ));
                            }

                            match machine.kind {
                                MachineKind::ChemMaster5000 => {
                                    chemmaster5000_body(
                                        body,
                                        &db,
                                        &knowledge,
                                        amount,
                                        loaded_entity,
                                        loaded,
                                        marked,
                                        reacting,
                                        &views.icons,
                                    );
                                }
                                MachineKind::MixingChamber => {
                                    mixing::body(
                                        body,
                                        &db,
                                        open_machine.unwrap(),
                                        buffer,
                                        loaded_entity,
                                        loaded,
                                        loaded_entity_b,
                                        loaded_b,
                                        agitation,
                                        &views.packaging,
                                        std::array::from_fn(|index| {
                                            views
                                                .mixing_scroll
                                                .iter()
                                                .find(|(area, _)| area.0 == index)
                                                .map_or(0.0, |(_, position)| position.y)
                                        }),
                                    );
                                }
                                MachineKind::Analyzer => {
                                    if let Some(snapshot) =
                                        open_machine.and_then(|e| views.reports.get(e).ok())
                                    {
                                        if loaded_entity == Some(snapshot.item)
                                            && loaded.is_some_and(|c| {
                                                snapshot.report.matches(&c.solution, &db)
                                            })
                                        {
                                            body.spawn(button(
                                                "Print Report",
                                                PanelAction::PrintReport(snapshot.report.id),
                                            ));
                                        }
                                    }
                                    analyzer_body(
                                        body,
                                        training.kind.as_deref(),
                                        open_machine
                                            .is_some_and(|e| training.calibrated.contains(e)),
                                        &db,
                                        &knowledge,
                                        loaded,
                                        hplc_report,
                                        views.hplc.selected,
                                    );
                                }
                                MachineKind::Grinder => {
                                    grinder_body(
                                        body,
                                        &db,
                                        catalog.as_deref(),
                                        hopper,
                                        loaded_entity,
                                        loaded,
                                        marked,
                                        reacting,
                                    );
                                }
                                MachineKind::DeliveryWindow => {
                                    delivery_window_body(
                                        body,
                                        &db,
                                        [
                                            (
                                                MachineSlot::A,
                                                loaded_entity,
                                                loaded,
                                                marked,
                                                reacting,
                                            ),
                                            (
                                                MachineSlot::B,
                                                loaded_entity_b,
                                                loaded_b,
                                                marked_b,
                                                reacting_b,
                                            ),
                                            (
                                                MachineSlot::C,
                                                loaded_entity_c,
                                                loaded_c,
                                                marked_c,
                                                reacting_c,
                                            ),
                                        ],
                                    );
                                }
                                MachineKind::StandingBoard => {
                                    if let Some(case) =
                                        board.security.as_deref().filter(|s| !s.text.is_empty())
                                    {
                                        body.spawn(label(&case.text, 14.0, TEXT));
                                    }
                                    let radio_scroll = board.radio_scroll_state();
                                    standing_board_body(
                                        body,
                                        shift,
                                        &stage,
                                        arc.as_ref(),
                                        &board.radio,
                                        radio_scroll,
                                    );
                                }
                                MachineKind::ReactionChamber => {
                                    heater_body(
                                        body,
                                        &db,
                                        &knowledge,
                                        thermostat,
                                        loaded_entity,
                                        loaded,
                                        reacting,
                                    );
                                }
                                MachineKind::Locker => {
                                    locker_body(body, &stored);
                                }
                            }
                        });
                });
        });
}

/// Lab service controls and the station's radio history share one screen. Relationship standing and shops live in the anytime Social screen.
///
/// One panel rather than a modal, exactly like the old shift board: this is
/// per-player `InteractionMode`, and a modal would trap one chemist on a
/// summary screen while the other was still working the counter.
fn standing_board_body(
    panel: &mut ChildSpawnerCommands,
    shift: &Shift,
    stage: &BoardStage,
    arc: Option<&ArcHeadline>,
    radio: &RadioLog,
    radio_scroll: RadioScrollState,
) {
    panel
        .spawn(Node {
            flex_wrap: FlexWrap::Wrap,
            column_gap: px(20),
            row_gap: px(16),
            width: percent(100),
            ..default()
        })
        .with_children(|columns| {
            columns
                .spawn(Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: px(10),
                    flex_grow: 1.0,
                    flex_basis: px(330),
                    min_width: px(280),
                    ..default()
                })
                .with_children(|service| {
                    service.spawn(heading("LAB SERVICE"));
                    match stage {
                        BoardStage::Debrief(report) => draw_debrief(service, report),
                        _ => draw_sign_controls(service, shift, stage),
                    }
                    service.spawn(label(
                        "Crew relationships and department shops: Social (Tab).",
                        12.0,
                        TEXT_DIM,
                    ));
                    if let Some(arc) = arc {
                        draw_arc_notice(service, arc);
                    }
                });
            columns
                .spawn(Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: px(10),
                    flex_grow: 1.4,
                    flex_basis: px(460),
                    min_width: px(280),
                    ..default()
                })
                .with_children(|radio_column| {
                    radio_column.spawn(heading("STATION RADIO"));
                    let mut pane = radio_column.spawn((
                        Node {
                            flex_direction: FlexDirection::Column,
                            row_gap: px(6),
                            max_height: vh(57),
                            overflow: Overflow::scroll_y(),
                            ..default()
                        },
                        ScrollPosition(Vec2::new(0.0, radio_scroll.offset)),
                        ScrollPane,
                        RadioHistoryPane,
                    ));
                    if radio_scroll.at_bottom {
                        pane.insert(ScrollToRadioBottom);
                    }
                    pane.with_children(|scroll| draw_radio_history(scroll, radio));
                });
        });
}

fn department_icon(department: Department) -> BookIcon {
    match department {
        Department::Medical => BookIcon::Heal,
        Department::Security => BookIcon::Controlled,
        Department::Engineering => BookIcon::ReactionChamber,
        Department::Cargo => BookIcon::Inputs,
        Department::Service => BookIcon::Orders,
    }
}

fn relationship_label(tier: RelationshipTier) -> &'static str {
    match tier {
        RelationshipTier::Burned => "Burned",
        RelationshipTier::Wary => "Wary",
        RelationshipTier::Neutral => "Neutral",
        RelationshipTier::Trusted => "Trusted",
    }
}

fn relationship_color(tier: RelationshipTier) -> Color {
    match tier {
        RelationshipTier::Burned => ERROR_TEXT,
        RelationshipTier::Wary => Color::srgb(0.92, 0.72, 0.34),
        RelationshipTier::Neutral => TEXT_DIM,
        RelationshipTier::Trusted => GOOD_TEXT,
    }
}

fn outcome_label(outcome: FavorOutcome) -> &'static str {
    match outcome {
        FavorOutcome::Unresolved => "No personal outcome yet",
        FavorOutcome::Helped => "You helped them",
        FavorOutcome::Compromised => "The last favor became complicated",
        FavorOutcome::Refused => "You refused their last request",
        FavorOutcome::Deceived => "Their last explanation did not hold up",
    }
}

fn personal_history_label(history: PersonalHistory) -> &'static str {
    match history {
        PersonalHistory::New => "No shared personal history yet",
        PersonalHistory::InProgress => "A personal matter is still unfolding",
        PersonalHistory::Established => "You have an established history together",
    }
}

fn standing_label(standing: i32) -> &'static str {
    if standing > 0 {
        "Good"
    } else if standing < 0 {
        "Strained"
    } else {
        "Unproven"
    }
}

fn signed_standing(standing: i32) -> String {
    if standing > 0 {
        format!("+{standing}")
    } else {
        standing.to_string()
    }
}

/// The anytime social directory. Its composition deliberately mirrors the
/// new field manual: a fixed category rail, one generous scrolling detail
/// pane, compact fact chips, inset cards, and the same blue active state.
fn spawn_social_directory(
    commands: &mut Commands,
    view: &SocialView,
    residents: &[ResidentSocialSnapshot],
    shift: &Shift,
    catalog: Option<&ProduceCatalog>,
    station: Option<&StationData>,
    icons: &BookIconAssets,
    // See `spawn_reference_book`: the same handoff, on the same shell.
    entrance: bookmarks::PanelEntrance,
) {
    let heard_lines: usize = residents
        .iter()
        .map(|resident| resident.history.lines.len())
        .sum();
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.015, 0.02, 0.025, 0.72)),
            PanelRoot,
            entrance,
            crate::until_we_leave_the_lab(),
        ))
        .with_children(|screen| {
            screen
                .spawn((
                    Node {
                        width: percent(95),
                        max_width: px(1360),
                        height: vh(92),
                        flex_direction: FlexDirection::Column,
                        padding: UiRect::all(px(16)),
                        row_gap: px(8),
                        border: UiRect::all(px(2)),
                        border_radius: BorderRadius::all(px(10)),
                        ..default()
                    },
                    BackgroundColor(PANEL_BG),
                    BorderColor::all(Color::srgb(0.24, 0.40, 0.50)),
                ))
                .with_children(|social| {
                    social
                        .spawn(Node {
                            width: percent(100),
                            align_items: AlignItems::Center,
                            justify_content: JustifyContent::SpaceBetween,
                            ..default()
                        })
                        .with_children(|header| {
                            header.spawn(row()).with_children(|title| {
                                title.spawn(icon_image(icons, BookIcon::Orders, 30.0, BOOK_ACCENT));
                                title.spawn(heading("CREW DIRECTORY"));
                                title.spawn(label(
                                    "RELATIONSHIPS  /  REQUISITIONS",
                                    10.0,
                                    TEXT_DIM,
                                ));
                            });
                            header.spawn(button("Orders", PanelAction::OpenOrders));
                            header.spawn(button("‹ Back to station", PanelAction::CloseSocial));
                        });

                    social
                        .spawn((
                            Node {
                                width: percent(100),
                                flex_wrap: FlexWrap::Wrap,
                                align_items: AlignItems::Center,
                                column_gap: px(6),
                                row_gap: px(5),
                                padding: UiRect::axes(px(8), px(6)),
                                border_radius: BorderRadius::all(px(6)),
                                ..default()
                            },
                            BackgroundColor(BOOK_PAPER),
                        ))
                        .with_children(|strip| {
                            fact_chip(
                                strip,
                                icons,
                                BookIcon::All,
                                residents.len().to_string(),
                                "Named crew",
                                "Persistent residents tracked during this save.",
                                BOOK_ACCENT,
                            );
                            fact_chip(
                                strip,
                                icons,
                                BookIcon::Book,
                                heard_lines.to_string(),
                                "Lines remembered",
                                "Only dialogue actually heard in this save appears here.",
                                GOOD_TEXT,
                            );
                            fact_chip(
                                strip,
                                icons,
                                BookIcon::Inputs,
                                "SHOPS",
                                "Department requisitions",
                                "Each department's catalog sits beside its crew records.",
                                Color::srgb(0.76, 0.68, 0.96),
                            );
                            fact_chip(
                                strip,
                                icons,
                                BookIcon::Key,
                                "TAB / ESC",
                                "Close directory",
                                "Return to the exact lab, machine, or manual screen underneath.",
                                TEXT_DIM,
                            );
                        });

                    social
                        .spawn(Node {
                            column_gap: px(16),
                            align_items: AlignItems::Start,
                            flex_grow: 1.0,
                            ..default()
                        })
                        .with_children(|columns| {
                            social_department_sidebar(columns, view, shift, icons);
                            social_department_page(
                                columns, view, residents, shift, catalog, station, icons,
                            );
                        });
                });
        });
}

fn social_department_sidebar(
    columns: &mut ChildSpawnerCommands,
    view: &SocialView,
    shift: &Shift,
    icons: &BookIconAssets,
) {
    columns
        .spawn((
            Node {
                width: px(154),
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                align_content: AlignContent::FlexStart,
                column_gap: px(4),
                row_gap: px(4),
                padding: UiRect::all(px(3)),
                flex_shrink: 0.0,
                border_radius: BorderRadius::all(px(7)),
                ..default()
            },
            BackgroundColor(BOOK_INSET),
        ))
        .with_children(|sidebar| {
            for department in Department::ALL {
                let mut entity = sidebar.spawn(icon_control(
                    PanelAction::ShowSocialDepartment(department),
                    department.label(),
                    department.blurb(),
                    145.0,
                    52.0,
                ));
                entity.with_children(|control| {
                    control.spawn(icon_image(
                        icons,
                        department_icon(department),
                        20.0,
                        if department == view.department {
                            BOOK_ACCENT
                        } else {
                            TEXT_DIM
                        },
                    ));
                    let balance = shift.standing(department);
                    control.spawn(label(
                        format!(
                            "{}  {}  {}",
                            department.label(),
                            signed_standing(balance),
                            standing_label(balance)
                        ),
                        11.0,
                        TEXT,
                    ));
                });
                if department == view.department {
                    entity.insert((Selected, BackgroundColor(BUTTON_ACTIVE)));
                }
            }
        });
}

#[allow(clippy::too_many_arguments)]
fn social_department_page(
    columns: &mut ChildSpawnerCommands,
    view: &SocialView,
    residents: &[ResidentSocialSnapshot],
    shift: &Shift,
    catalog: Option<&ProduceCatalog>,
    station: Option<&StationData>,
    icons: &BookIconAssets,
) {
    columns
        .spawn((
            Node {
                flex_grow: 1.0,
                flex_direction: FlexDirection::Column,
                row_gap: px(8),
                max_height: vh(69),
                overflow: Overflow::scroll_y(),
                ..default()
            },
            ScrollPosition::default(),
            ScrollPane,
        ))
        .with_children(|pane| {
            pane.spawn(row()).with_children(|heading_row| {
                heading_row
                    .spawn(icon_badge(
                        view.department.label(),
                        view.department.blurb(),
                        36.0,
                    ))
                    .with_children(|badge| {
                        badge.spawn(icon_image(
                            icons,
                            department_icon(view.department),
                            23.0,
                            BOOK_ACCENT,
                        ));
                    });
                let balance = shift.standing(view.department);
                heading_row.spawn(label(
                    format!(
                        "{}  {}  {}",
                        view.department.label(),
                        signed_standing(balance),
                        standing_label(balance)
                    ),
                    20.0,
                    TEXT,
                ));
                heading_row.spawn(label(
                    "Department goodwill",
                    12.0,
                    TEXT_DIM,
                ));
            });
            pane.spawn(label(view.department.blurb(), 14.0, TEXT_DIM));

            pane.spawn(wrap_row()).with_children(|cards| {
                for resident in residents.iter().filter(|resident| {
                    resident_department(&resident.name) == Some(view.department)
                }) {
                    let selected = view.resident.as_deref() == Some(resident.name.as_str());
                    let mut entity = cards.spawn(icon_control(
                        PanelAction::SelectSocialResident(resident.name.clone()),
                        resident.name.clone(),
                        "Open this crew member's relationship record and remembered dialogue.",
                        180.0,
                        64.0,
                    ));
                    entity.with_children(|card| {
                        card.spawn(label(resident.name.clone(), 14.0, TEXT));
                        card.spawn(label(
                            relationship_label(resident.relationship.tier),
                            11.0,
                            relationship_color(resident.relationship.tier),
                        ));
                    });
                    if selected {
                        entity.insert((Selected, BackgroundColor(BUTTON_ACTIVE)));
                    }
                }
            });

            if let Some(selected) = view
                .resident
                .as_deref()
                .and_then(|name| residents.iter().find(|resident| resident.name == name))
                .filter(|resident| resident_department(&resident.name) == Some(view.department))
            {
                draw_resident_record(pane, selected);
            } else {
                pane.spawn((section(), BackgroundColor(BOOK_INSET)))
                    .with_children(|card| {
                        card.spawn(label("SELECT A CREW MEMBER", 13.0, BOOK_ACCENT));
                        card.spawn(label(
                            "Choose a resident above to review your relationship and the dialogue heard in this save.",
                            13.0,
                            TEXT_DIM,
                        ));
                    });
            }

            pane.spawn(label("DEPARTMENT FAVORS", 13.0, BOOK_ACCENT));
            draw_department_shop(pane, shift, view.department);
            draw_department_sellers(pane, shift, view.department, catalog, station);
        });
}

fn draw_resident_record(panel: &mut ChildSpawnerCommands, resident: &ResidentSocialSnapshot) {
    panel
        .spawn((
            Node {
                width: percent(100),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(11)),
                row_gap: px(6),
                border: UiRect::left(px(3)),
                border_radius: BorderRadius::all(px(6)),
                ..default()
            },
            BackgroundColor(BOOK_INSET),
            BorderColor::all(relationship_color(resident.relationship.tier)),
        ))
        .with_children(|record| {
            record.spawn(row()).with_children(|header| {
                header.spawn(label(resident.name.clone(), 18.0, TEXT));
                header.spawn(label(
                    relationship_label(resident.relationship.tier).to_uppercase(),
                    11.0,
                    relationship_color(resident.relationship.tier),
                ));
            });
            record.spawn(label(
                personal_history_label(resident.relationship.personal_history),
                12.0,
                TEXT_DIM,
            ));
            record.spawn(label(
                outcome_label(resident.relationship.last_outcome),
                12.0,
                TEXT_DIM,
            ));
            record.spawn(label("REMEMBERED DIALOGUE", 13.0, BOOK_ACCENT));
            if resident.history.lines.is_empty() {
                record.spawn(label(
                    "No conversation with this resident has been recorded in this save.",
                    12.0,
                    TEXT_DIM,
                ));
            } else {
                for line in &resident.history.lines {
                    record
                        .spawn((
                            Node {
                                width: percent(100),
                                padding: UiRect::axes(px(9), px(6)),
                                border_radius: BorderRadius::all(px(4)),
                                ..default()
                            },
                            BackgroundColor(Color::srgba(0.11, 0.13, 0.16, 0.9)),
                        ))
                        .with_children(|entry| {
                            entry.spawn(label(
                                format!("{}: {}", line.speaker, line.text),
                                12.0,
                                TEXT,
                            ));
                        });
                }
            }
        });
}

/// The station's radio history, oldest first — the same order the small
/// always-on HUD feed reads, just the whole log instead of its last few
/// lines.
///
/// Rendered declaratively from whatever snapshot `BoardView` supplied this
/// rebuild, not patched in place: `sync_panel` resets `ScrollPosition` to
/// zero on every rebuild, and radio lines land on their own clock throughout
/// play, so wiring live updates in here would mean a player reading old
/// chatter gets yanked back to the top on almost every delivery anywhere in
/// the lab. `PanelSignature.department_standing` already forces a rebuild on
/// essentially every delivery anyway, so this is current whenever the board
/// legitimately redraws for any other reason.
#[derive(Component)]
struct RadioHistoryPane;

#[derive(Component)]
struct ScrollToRadioBottom;

fn finish_radio_auto_scroll(
    mut commands: Commands,
    mut panes: Query<(Entity, &mut ScrollPosition, &ComputedNode), With<ScrollToRadioBottom>>,
) {
    for (entity, mut position, computed) in &mut panes {
        let limit = (computed.content_size().y - computed.size().y).max(0.0);
        position.y = limit;
        // A zero limit may simply mean layout has not run for this new pane
        // yet. Leave the marker for one more frame in that case.
        if limit > 0.0 || computed.size().y > 0.0 {
            commands.entity(entity).remove::<ScrollToRadioBottom>();
        }
    }
}

fn draw_radio_history(panel: &mut ChildSpawnerCommands, radio: &RadioLog) {
    panel.spawn(label("Radio log", 15.0, TEXT));
    if radio.entries.is_empty() {
        panel.spawn(label("Nothing on the wire yet.", 12.0, TEXT_DIM));
        return;
    }
    for entry in &radio.entries {
        let accent = radio_channel_color(entry.channel);
        let message = match entry.tone {
            RadioTone::Positive => Color::srgb(0.70, 0.90, 0.72),
            RadioTone::Negative => Color::srgb(0.92, 0.76, 0.72),
            RadioTone::Neutral => TEXT,
        };
        let marker = match entry.priority {
            RadioPriority::RedAlert => "  ·  RED ALERT",
            RadioPriority::StationWide => "  ·  STATION-WIDE",
            RadioPriority::Urgent => "  ·  URGENT",
            _ if entry.announcement => "  ·  ANNOUNCEMENT",
            _ => "",
        };
        let speaker = entry.speaker.as_deref().unwrap_or("Open carrier");
        panel
            .spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    padding: UiRect::axes(px(10), px(7)),
                    row_gap: px(3),
                    border: UiRect::left(px(3)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.09, 0.10, 0.12, 0.86)),
                BorderColor::all(accent),
            ))
            .with_children(|row| {
                row.spawn(label(
                    format!(
                        "{}  ·  {}  ·  {}{}",
                        entry.channel.tag(),
                        entry.channel.label(),
                        speaker,
                        marker
                    ),
                    12.0,
                    accent,
                ));
                row.spawn(label(entry.text.clone(), 13.0, message));
            });
    }
}

/// One department's standing, values, and live requisitions. The caller has
/// already selected the department in the social directory's left rail.
fn draw_department_shop(panel: &mut ChildSpawnerCommands, shift: &Shift, department: Department) {
    let kinds: Vec<RequisitionKind> = RequisitionKind::ALL
        .into_iter()
        .filter(|kind| kind.department() == department)
        .collect();
    if kinds.is_empty() {
        panel.spawn(label("No general requisitions available.", 12.0, TEXT_DIM));
        return;
    }
    panel.spawn(label(
        format!(
            "Goodwill {}  ·  call in a favor now, repay it through successful work",
            signed_standing(shift.standing(department))
        ),
        12.0,
        TEXT_DIM,
    ));
    panel.spawn(wrap_row()).with_children(|row| {
        for &kind in &kinds {
            let caption = format!("{}  −{}", kind.label(), kind.cost());
            row.spawn(button(caption, PanelAction::Requisition(kind)))
                .insert(TooltipSource::new(kind.label(), kind.blurb()));
        }
    });
}

fn draw_department_sellers(
    panel: &mut ChildSpawnerCommands,
    shift: &Shift,
    department: Department,
    catalog: Option<&ProduceCatalog>,
    station: Option<&StationData>,
) {
    match department {
        Department::Service => {
            if let Some(catalog) = catalog {
                let items: Vec<_> = catalog
                    .packs()
                    .iter()
                    .map(|pack| {
                        (
                            pack.label.clone(),
                            pack.blurb.clone(),
                            pack.cost,
                            PanelAction::NpcPack(NpcRequisitionKind::IvyPack(pack.id)),
                        )
                    })
                    .collect();
                draw_seller_section(
                    panel,
                    "Botanist Ivy",
                    shift.npc_standing("Botanist Ivy"),
                    "Sells themed packs off her own trust, not the department's.",
                    &items,
                );
            }
        }
        Department::Cargo => {
            if let Some(station) = station {
                let mut items: Vec<_> = station
                    .config
                    .supply
                    .personal_packs
                    .iter()
                    .enumerate()
                    .map(|(index, pack)| {
                        (
                            pack.label.clone(),
                            pack.blurb.clone(),
                            pack.cost,
                            PanelAction::NpcPack(NpcRequisitionKind::SatoPack(GlasswarePackId(
                                index as u32,
                            ))),
                        )
                    })
                    .collect();
                items.push((
                    "Water Gun".to_string(),
                    "Cheap and weak, but hits everyone caught in its spray, not just one target."
                        .to_string(),
                    WATER_GUN_COST,
                    PanelAction::NpcPack(NpcRequisitionKind::SatoWaterGun),
                ));
                draw_seller_section(
                    panel,
                    "Miner Sato",
                    shift.npc_standing("Miner Sato"),
                    "Sells glassware directly, off his own trust — faster than waiting on the deficit check.",
                    &items,
                );
            }
        }
        Department::Engineering => draw_seller_section(
            panel,
            "Tech Lindqvist",
            shift.npc_standing("Tech Lindqvist"),
            "Sells a coil that overclocks the Reaction Chamber, and a pair of longer-reach confrontation tools.",
            &[
                (
                    "Overclock Coil".to_string(),
                    "Temporarily doubles the chamber's heating rate. Repurchasing tops up charges."
                        .to_string(),
                    OVERCLOCK_COST,
                    PanelAction::NpcPack(NpcRequisitionKind::LindqvistOverclock),
                ),
                (
                    "Syringe Gun".to_string(),
                    "A syringe with real range — draws and injects from well beyond arm's reach."
                        .to_string(),
                    SYRINGE_GUN_COST,
                    PanelAction::NpcPack(NpcRequisitionKind::LindqvistSyringeGun),
                ),
                (
                    "Pressure Sprayer".to_string(),
                    "A pressurized sprayer: longer reach than a hand bottle, and it catches everyone in its cone."
                        .to_string(),
                    PRESSURE_SPRAYER_COST,
                    PanelAction::NpcPack(NpcRequisitionKind::LindqvistPressureSprayer),
                ),
            ],
        ),
        Department::Medical | Department::Security => {}
    }
}

/// One NPC's own personal shop — the individual-standing sibling of
/// [`draw_department_shop`], spending [`Shift::npc_standing`] rather than a
/// department average. Phase 1 has exactly one seller; a second one
/// generalizes this into a loop the same way `draw_department_shop` loops
/// `Department::ALL`, rather than hardcoding Ivy by name here.
/// One NPC's own personal shop — the individual-standing sibling of
/// [`draw_department_shop`], spending [`Shift::npc_standing`] rather than a
/// department average. Generic over the seller: `items` is already resolved
/// to `(label, blurb, cost, action)` tuples by the caller, since each
/// seller's own catalog (Ivy's produce packs, Sato's glassware packs,
/// Lindqvist's single tool) has a genuinely different payload shape with no
/// shared cross-seller RON schema.
fn draw_seller_section(
    panel: &mut ChildSpawnerCommands,
    seller_name: &str,
    seller_standing: i32,
    seller_blurb: &str,
    items: &[(String, String, i32, PanelAction)],
) {
    if items.is_empty() {
        return;
    }

    panel.spawn(label(
        format!(
            "{seller_name}  ·  personal goodwill {}",
            signed_standing(seller_standing)
        ),
        15.0,
        TEXT,
    ));
    panel.spawn(wrap_row()).with_children(|row| {
        for (item_label, blurb, cost, action) in items {
            let caption = format!("{item_label}  −{cost}");
            row.spawn(button(caption, action.clone()))
                .insert(TooltipSource::new(
                    item_label.clone(),
                    format!("{blurb} {seller_blurb}"),
                ));
        }
    });
}

/// The sign, and the second stage that ends the shift.
fn draw_sign_controls(panel: &mut ChildSpawnerCommands, shift: &Shift, stage: &BoardStage) {
    panel.spawn(label(format!("Shift {}", shift.shift_number), 16.0, TEXT));
    panel.spawn(label(
        match stage {
            BoardStage::Open => "Taking requests.",
            BoardStage::WrappingUp { clear: false } => {
                "Sign is down. Someone is still at the counter — serve them, or wait them out."
            }
            BoardStage::WrappingUp { clear: true } => {
                "Sign is down and the counter is clear. Call it whenever you're ready."
            }
            // Drawn by `draw_debrief` instead; this function is never reached
            // with one.
            BoardStage::Debrief(_) => "",
        },
        13.0,
        TEXT_DIM,
    ));
    panel.spawn(row()).with_children(|row| {
        let caption = if shift.accepting_orders {
            "Stop taking requests"
        } else {
            "Start taking requests"
        };
        row.spawn(button(caption, PanelAction::ToggleAcceptingOrders));

        // Only live once the sign is down *and* nobody is left waiting. Drawn
        // dead rather than hidden while the counter is busy, for the same
        // reason an unaffordable requisition is: a button that appears out of
        // nowhere is a button the player never learns exists.
        if let BoardStage::WrappingUp { clear } = stage {
            let mut entity = row.spawn(button("Call it a shift", PanelAction::CallItAShift));
            if !clear {
                entity.insert(BackgroundColor(Color::srgb(0.11, 0.12, 0.14)));
            }
        }
    });
}

/// The end-of-shift debrief.
///
/// Everything on it is a *difference* — this shift against the one before,
/// never the career total, which the HUD already shows and which nobody needs
/// two readouts of. That is the whole point of the beat: a career total only
/// ever goes up, so it can never tell you whether the last hour went well.
///
/// A panel body rather than a full-screen modal, deliberately. The world is
/// still running behind it: crew who were already at the counter are still
/// waiting, the arc is still drifting, and in co-op the other chemist is very
/// possibly still working. Stopping the game to show a summary would make
/// reading it a cost.
fn draw_debrief(panel: &mut ChildSpawnerCommands, report: &ShiftReport) {
    panel.spawn(label(
        format!("SHIFT {} — DEBRIEF", report.number),
        18.0,
        TEXT,
    ));

    if report.is_quiet() {
        panel.spawn(label(
            "Nothing came in and nothing went out. Some shifts are like that.",
            13.0,
            TEXT_DIM,
        ));
    } else {
        panel.spawn(label(
            format!(
                "Delivered {}   ·   botched {}",
                report.delivered, report.botched
            ),
            15.0,
            if report.botched > report.delivered {
                ERROR_TEXT
            } else {
                TEXT
            },
        ));
    }

    if report.recipes > 0 {
        panel.spawn(label(
            format!(
                "{} new {} written up.",
                report.recipes,
                if report.recipes == 1 {
                    "recipe"
                } else {
                    "recipes"
                }
            ),
            13.0,
            GOOD_TEXT,
        ));
    }
    if report.research != 0 {
        // "Banked", not "earned": hints and dispenser tiers come out of the
        // same pot, so a productive shift that spent everything nets zero and
        // a shift that only shopped goes negative. See `ShiftReport::research`.
        panel.spawn(label(
            format!("Research banked  {:+}", report.research),
            13.0,
            if report.research < 0 { TEXT_DIM } else { TEXT },
        ));
    }

    if let Some(change) = report.condition_change {
        let (text, tone) = match change {
            ConditionChange::Improved => (
                "Station condition improved during this shift. Quality Chemistry support is making a difference.",
                GOOD_TEXT,
            ),
            ConditionChange::Deteriorated => (
                "Station condition deteriorated during this shift. Outstanding Chemistry support needs priority.",
                ERROR_TEXT,
            ),
        };
        panel.spawn(label(text, 13.0, tone));
    }

    panel.spawn(label("Standing", 15.0, TEXT));
    if report.standing.is_empty() {
        panel.spawn(label("  Nobody's opinion moved.", 12.0, TEXT_DIM));
    } else {
        for (department, delta) in &report.standing {
            panel.spawn(label(
                format!("  {}  {:+}", department.label(), delta),
                13.0,
                if *delta < 0 { ERROR_TEXT } else { GOOD_TEXT },
            ));
        }
    }

    panel.spawn(label(
        "Spend what you've earned before you open up again — requisitions are below.",
        12.0,
        TEXT_DIM,
    ));
    panel.spawn(row()).with_children(|row| {
        row.spawn(button("Open up again", PanelAction::OpenUpAgain));
    });
}

/// The campaign notice at the top of the standing board.
///
/// Says as little as the station actually knows. At [`Reveal::Suspected`] that
/// is only "something is wrong" — [`ArcHeadline::name`] is `None` and there is
/// nothing here that could give the answer away early.
fn draw_arc_notice(panel: &mut ChildSpawnerCommands, arc: &ArcHeadline) {
    let (heading, tone) = match (arc.resolved, &arc.name) {
        (Some(true), Some(name)) => (format!("STOOD DOWN — {name}"), GOOD_TEXT),
        (Some(false), Some(name)) => (format!("STATION LOST — {name}"), ERROR_TEXT),
        (Some(true), None) => ("STOOD DOWN".to_string(), GOOD_TEXT),
        (Some(false), None) => ("STATION LOST".to_string(), ERROR_TEXT),
        (None, Some(name)) => (format!("ALERT — {name}"), ERROR_TEXT),
        (None, None) => ("ALERT — SOMETHING IS ABOARD".to_string(), ERROR_TEXT),
    };
    panel.spawn(label(heading, 16.0, tone));

    let detail = match arc.resolved {
        Some(true) => {
            "Whatever they were building, it isn't happening. Command sends thanks.".to_string()
        }
        Some(false) => "Command has stopped answering. There is nothing left to fill.".to_string(),
        None if arc.name.is_none() => {
            "Command won't say what. Departments are filing requests they won't explain."
                .to_string()
        }
        None if arc.total > 0 && arc.support_only => format!(
            "Departments have filed {} of {} support reports. Their findings point toward direct intervention.",
            arc.countered, arc.total
        ),
        None if arc.total > 0 => format!(
            "Departments have {} of {} countermeasures in hand. They are asking you for the rest.",
            arc.countered, arc.total
        ),
        None => "Departments are working on it.".to_string(),
    };
    panel.spawn(label(detail, 12.0, TEXT_DIM));
    if arc.incidents > 0 {
        let status = if arc.treated_incidents >= crate::cult::FINALE_WARDS {
            "Source exposed — the Chapel focus can be confronted."
        } else if arc.treated_incidents >= 3 {
            "Outer wards failing — the pattern is drawing toward a source."
        } else if arc.treated_incidents > 0 {
            "Pattern emerging — direct intervention is weakening it."
        } else {
            "Ritual signs documented — none neutralised yet."
        };
        panel.spawn(label(format!("Cult case file: {status}"), 12.0, TEXT_DIM));
    }
}

/// Fixed cell width for every base-stock chip. One size rather than a size
/// parameter per panel: this is the only panel that calls [`chip_grid`] so
/// far, and a shared constant is one fewer thing for a future caller to get
/// wrong when it does too.
const BASE_STOCK_CHIP_WIDTH: f32 = 150.0;

/// Every dispensable reagent, grouped by [`ChemFamily`] in display order and
/// alphabetised within each group — the grid a chemist actually wants to
/// scan, instead of one alphabetised wall of ~30 names.
fn base_stock_groups(db: &ChemDb) -> Vec<(&'static str, Vec<GridChip<PanelAction>>)> {
    ChemFamily::ALL
        .into_iter()
        .filter_map(|family| {
            let mut reagents: Vec<&chem_sim::Reagent> = db
                .reagents
                .dispensable()
                .filter(|r| r.family == family)
                .collect();
            if reagents.is_empty() {
                return None;
            }
            reagents.sort_by(|a, b| a.name.cmp(&b.name));
            let chips = reagents
                .into_iter()
                .map(|r| GridChip {
                    label: r.name.clone(),
                    swatch: Color::srgb(r.color[0], r.color[1], r.color[2]),
                    action: PanelAction::Dispense(r.id),
                    selected: false,
                })
                .collect();
            Some((family.label(), chips))
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn chemmaster5000_body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    amount: Option<&DispenseAmount>,
    container_entity: Option<Entity>,
    loaded: Option<&Container>,
    marked: Option<&crate::labels::Label>,
    reacting: bool,
    icons: &BookIconAssets,
) {
    let selected = amount.map(|a| a.0).unwrap_or(Units::whole(10));

    panel.spawn(wrap_row()).with_children(|strip| {
        fact_chip(
            strip,
            icons,
            BookIcon::ChemMaster,
            "CM-5000",
            "ChemMaster compounder",
            "Station reagent supply and live sample workstation.",
            BOOK_ACCENT,
        );
        fact_chip(
            strip,
            icons,
            BookIcon::Inputs,
            selected.to_string(),
            "Transfer volume",
            "Each reagent control dispenses this exact amount into the loaded vessel.",
            BOOK_ACCENT,
        );
        fact_chip(
            strip,
            icons,
            BookIcon::Research,
            knowledge.research_points.to_string(),
            "Research bank",
            "Research is retained here for chemistry method development.",
            Color::srgb(0.76, 0.68, 0.96),
        );
        fact_chip(
            strip,
            icons,
            BookIcon::Recorded,
            format!(
                "{}/{}",
                knowledge.known_count(),
                db.reactions.recipe_count()
            ),
            "Recorded methods",
            "Methods currently documented in the chemistry field manual.",
            GOOD_TEXT,
        );
        if let Some(container) = loaded {
            fact_chip(
                strip,
                icons,
                BookIcon::Purity,
                format!("{:.0}%", container.solution.average_purity() * 100.0),
                "Average purity",
                "Volume-weighted purity of everything in the loaded vessel.",
                HPLC_CLEAN,
            );
            fact_chip(
                strip,
                icons,
                BookIcon::Temperature,
                container.solution.temperature.to_string(),
                "Sample temperature",
                "Live temperature of the loaded vessel.",
                Color::srgb(0.95, 0.55, 0.32),
            );
        }
    });

    panel
        .spawn(Node {
            width: percent(100),
            align_items: AlignItems::FlexStart,
            column_gap: px(12),
            ..default()
        })
        .with_children(|workspace| {
            workspace
                .spawn(Node {
                    flex_basis: percent(0),
                    flex_grow: 1.0,
                    flex_direction: FlexDirection::Column,
                    row_gap: px(8),
                    ..default()
                })
                .with_children(|controls| {
                    instrument_card(
                        controls,
                        icons,
                        BookIcon::Inputs,
                        "METERED TRANSFER",
                        "Choose the exact volume used by every stock control.",
                        |section| {
                            section.spawn(row()).with_children(|row| {
                                for step in [1, 5, 10, 25, 50] {
                                    let units = Units::whole(step);
                                    let mut entity = row.spawn(button(
                                        format!("{step}u"),
                                        PanelAction::SetAmount(units),
                                    ));
                                    if units == selected {
                                        entity.insert((Selected, BackgroundColor(BUTTON_ACTIVE)));
                                    }
                                }
                            });
                        },
                    );

                    instrument_card(
                        controls,
                        icons,
                        BookIcon::RawReagent,
                        "BASE STOCK MANIFOLD",
                        "Select a reagent by swatch and name; it enters the loaded vessel.",
                        |section| {
                            chip_grid(section, BASE_STOCK_CHIP_WIDTH, base_stock_groups(db));
                        },
                    );

                    instrument_card(
                        controls,
                        icons,
                        BookIcon::Research,
                        "METHOD ARCHIVE",
                        "Research controls remain secondary to live compounding.",
                        |section| {
                            section.spawn(label(
                                format!("{} research banked", knowledge.research_points),
                                13.0,
                                TEXT,
                            ));
                            // Debug-only: the handler on the other end
                            // (`knowledge::handle_unlock_all`) is compiled
                            // out of release builds too, so this would be a
                            // button that silently did nothing in a shipped
                            // game rather than the playtest shortcut it looks
                            // like.
                            #[cfg(debug_assertions)]
                            if knowledge.known_count() < db.reactions.recipe_count() {
                                section.spawn(button(
                                    "PLAYTEST: unlock all chemistry",
                                    PanelAction::UnlockAll,
                                ));
                            }
                        },
                    );
                });

            workspace
                .spawn(Node {
                    width: px(318),
                    flex_shrink: 0.0,
                    flex_direction: FlexDirection::Column,
                    ..default()
                })
                .with_children(|sample| {
                    chemmaster_sample_card(
                        sample,
                        db,
                        icons,
                        container_entity,
                        loaded,
                        marked,
                        reacting,
                    );
                });
        });
}

fn instrument_card(
    panel: &mut ChildSpawnerCommands,
    icons: &BookIconAssets,
    icon: BookIcon,
    title: &'static str,
    explanation: &'static str,
    build: impl FnOnce(&mut ChildSpawnerCommands),
) {
    panel
        .spawn(Node {
            flex_direction: FlexDirection::Row,
            flex_wrap: FlexWrap::Wrap,
            align_items: AlignItems::Center,
            column_gap: px(4),
            row_gap: px(2),
            ..default()
        })
        .with_children(|heading| {
            heading
                .spawn(icon_badge(title, explanation, 30.0))
                .with_children(|badge| {
                    badge.spawn(icon_image(icons, icon, 19.0, BOOK_ACCENT));
                });
            heading.spawn(label(title, 12.0, Color::srgb(0.66, 0.82, 0.94)));
            heading.spawn(label(explanation, 12.0, TEXT_DIM));
        });
    panel
        .spawn((
            Node {
                width: percent(100),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(9)),
                row_gap: px(5),
                border: UiRect::all(px(1)),
                border_radius: BorderRadius::all(px(6)),
                ..default()
            },
            BackgroundColor(BOOK_INSET),
            BorderColor::all(Color::srgba(0.25, 0.34, 0.42, 0.80)),
        ))
        .with_children(build);
}

#[allow(clippy::too_many_arguments)]
fn chemmaster_sample_card(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    icons: &BookIconAssets,
    container_entity: Option<Entity>,
    loaded: Option<&Container>,
    marked: Option<&crate::labels::Label>,
    reacting: bool,
) {
    instrument_card(
        panel,
        icons,
        BookIcon::MixingChamber,
        "LIVE SAMPLE",
        "The vessel is the primary ChemMaster 5000 instrument.",
        |section| {
            section
                .spawn(Node {
                    width: percent(100),
                    justify_content: JustifyContent::Center,
                    padding: UiRect::vertical(px(8)),
                    ..default()
                })
                .with_children(|center| {
                    beaker_preview_sized(center, container_entity, 146.0, 214.0);
                });

            let Some(container) = loaded else {
                section.spawn(label("NO VESSEL LOADED", 14.0, TEXT_DIM));
                section.spawn(label(
                    "Carry a beaker to the ChemMaster 5000 and press E.",
                    11.0,
                    TEXT_DIM,
                ));
                return;
            };

            section.spawn(label(
                format!(
                    "{}  |  {} / {}",
                    container.kind.label(),
                    container.solution.total_volume(),
                    container.kind.capacity()
                ),
                15.0,
                TEXT,
            ));

            if let Some(marked) = marked.filter(|marked| !marked.0.trim().is_empty()) {
                section.spawn(label(format!("MARKED: \"{}\"", marked.0), 12.0, LABEL_INK));
            }

            section.spawn(wrap_row()).with_children(|facts| {
                fact_chip(
                    facts,
                    icons,
                    BookIcon::Ph,
                    format!("{:.2}", container.solution.ph()),
                    "Sample pH",
                    "Live acidity or alkalinity of the full vessel.",
                    Color::srgb(0.58, 0.72, 0.96),
                );
                fact_chip(
                    facts,
                    icons,
                    BookIcon::Purity,
                    format!("{:.0}%", container.solution.average_purity() * 100.0),
                    "Average purity",
                    "Volume-weighted purity across the loaded mixture.",
                    HPLC_CLEAN,
                );
                fact_chip(
                    facts,
                    icons,
                    BookIcon::Temperature,
                    container.solution.temperature.to_string(),
                    "Sample temperature",
                    "Live vessel temperature; some reactions require an authored range.",
                    Color::srgb(0.95, 0.55, 0.32),
                );
            });

            if container.solution.is_empty() {
                section.spawn(label("VESSEL EMPTY", 12.0, TEXT_DIM));
            } else {
                section.spawn(label("REAGENT PROFILE", 12.0, TEXT_DIM));
                for (reagent, quantity) in container.solution.iter() {
                    let definition = db.reagents.get(reagent);
                    let [r, g, b] = definition.color;
                    section.spawn(row()).with_children(|token| {
                        token.spawn(swatch_chip(Color::srgb(r, g, b)));
                        token.spawn(label(definition.name.clone(), 12.0, TEXT));
                        token.spawn(label(quantity.to_string(), 12.0, BOOK_ACCENT));
                        token.spawn(label(
                            format!("{:.0}%", container.solution.purity_of(reagent) * 100.0),
                            11.0,
                            TEXT_DIM,
                        ));
                    });
                }
            }

            if reacting {
                section
                    .spawn(icon_badge(
                        "Reaction active",
                        "The mixture is still processing; its composition remains live.",
                        42.0,
                    ))
                    .with_children(|status| {
                        status.spawn(icon_image(icons, BookIcon::DirectMix, 18.0, GOOD_TEXT));
                        status.spawn(label("PROCESSING", 13.0, GOOD_TEXT));
                    });
            }

            section.spawn(row()).with_children(|actions| {
                actions.spawn(button("Eject vessel", PanelAction::Eject(MachineSlot::A)));
                actions.spawn(button("Empty vessel", PanelAction::Empty(MachineSlot::A)));
            });
        },
    );
}

/// The reaction chamber's target-temperature dial.
///
/// Only one is ever alive: a client only ever has one panel open, unlike
/// `settings::Knob`, which has to tell several simultaneous sliders apart.
#[derive(Component)]
struct TempSlider;

/// The filled portion of [`TempSlider`], resized to match the live target.
#[derive(Component)]
struct TempSliderFill;

/// The number printed above [`TempSlider`].
#[derive(Component)]
struct TempSliderReadout;

/// The value a chemist is currently dragging the dial to, before the server
/// has echoed it back — so their own view never fights their own input
/// waiting on a round trip. `None` the rest of the time, when the track just
/// shows whatever `Thermostat.target` has replicated.
#[derive(Resource, Default)]
struct ThermostatDrag(Option<f32>);

const TEMP_SLIDER_TRACK_HEIGHT: f32 = 18.0;

/// Where `kelvin` sits along the dial, as 0..=1.
fn temp_fraction_of(kelvin: f32) -> f32 {
    ((kelvin - TEMPERATURE_MIN) / (TEMPERATURE_MAX - TEMPERATURE_MIN)).clamp(0.0, 1.0)
}

/// The temperature `fraction` of the way along the dial.
fn temp_at_fraction(fraction: f32) -> f32 {
    TEMPERATURE_MIN + (TEMPERATURE_MAX - TEMPERATURE_MIN) * fraction.clamp(0.0, 1.0)
}

#[derive(Clone, Debug, PartialEq)]
struct ChamberForecast {
    product: String,
    target: String,
    ph_target: Option<PhTarget>,
    lines: Vec<String>,
    ready: bool,
    hazardous: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PhTarget {
    min: f32,
    max: f32,
    optimum: Option<f32>,
}

fn reaction_is_hazardous(reaction: &chem_sim::Reaction, temperature: Kelvin) -> bool {
    reaction.effects.iter().any(|effect| {
        matches!(
            effect,
            chem_sim::ReactionEffect::Explosion(_)
                | chem_sim::ReactionEffect::ExplosionProfile { .. }
                | chem_sim::ReactionEffect::Pulse { .. }
                | chem_sim::ReactionEffect::PulseProfile { .. }
                | chem_sim::ReactionEffect::Emp(_)
                | chem_sim::ReactionEffect::EmpProfile { .. }
                | chem_sim::ReactionEffect::Electric(_)
                | chem_sim::ReactionEffect::ElectricProfile { .. }
        )
    }) || (reaction.is_overheated(temperature)
        && matches!(reaction.overheat, chem_sim::Overheat::Detonate { .. }))
}

/// Whether `solution` currently satisfies every gating condition
/// (`Reaction::max_scale`) of some reaction with a destructive effect — the
/// same "which reactions can run right now" test `chem_sim::is_reacting`
/// uses internally, widened past rated reactions so an instantaneous
/// hazardous reaction is still caught for the one tick before it resolves.
///
/// Deliberately not gated on `Knowledge`, unlike the REACTION MONITOR's
/// forecast (`represented_chamber_reactions`/`chamber_forecast`): this is a
/// physical read of the beaker, the same category of fact as
/// `Solution::color` or `chem_sim::is_reacting`, not a spoiler of the recipe
/// book — it names no product or recipe, only "something in here is
/// dangerous."
fn solution_is_hazardous(solution: &chem_sim::Solution, reactions: &chem_sim::ReactionSet) -> bool {
    reactions.iter().any(|reaction| {
        reaction.max_scale(solution).is_some()
            && reaction_is_hazardous(reaction, solution.temperature)
    })
}

fn chamber_target(reaction: &chem_sim::Reaction) -> String {
    let mut controls = Vec::new();
    match (reaction.min_temp, reaction.max_temp) {
        (Some(minimum), Some(maximum)) => {
            controls.push(format!("{:.0}–{:.0}K", minimum.0, maximum.0));
        }
        (Some(minimum), None) => controls.push(format!("≥{:.0}K", minimum.0)),
        (None, Some(maximum)) => controls.push(format!("≤{:.0}K", maximum.0)),
        (None, None) => {}
    }
    match (reaction.min_ph, reaction.max_ph) {
        (Some(minimum), Some(maximum)) => controls.push(format!("pH {minimum:.1}–{maximum:.1}")),
        (Some(minimum), None) => controls.push(format!("pH ≥{minimum:.1}")),
        (None, Some(maximum)) => controls.push(format!("pH ≤{maximum:.1}")),
        (None, None) => {}
    }
    if let Some(minimum) = reaction.min_purity {
        controls.push(format!("≥{:.0}% pure", minimum * 100.0));
    }
    if controls.is_empty() {
        "ambient".to_string()
    } else {
        controls.join("  ·  ")
    }
}

fn ph_target(reaction: &chem_sim::Reaction) -> Option<PhTarget> {
    (reaction.min_ph.is_some() || reaction.max_ph.is_some()).then_some(PhTarget {
        min: reaction.min_ph.unwrap_or(0.0),
        max: reaction.max_ph.unwrap_or(14.0),
        optimum: reaction.optimal_ph,
    })
}

fn represented_chamber_reactions<'a>(
    db: &'a ChemDb,
    knowledge: &Knowledge,
    solution: &chem_sim::Solution,
) -> Vec<&'a chem_sim::Reaction> {
    let mut candidates: Vec<_> = db
        .reactions
        .iter()
        .filter(|reaction| {
            reaction.process.is_ambient()
                && knowledge.is_known(reaction.id)
                && reaction
                    .reactants
                    .iter()
                    .all(|(reagent, _)| solution.volume_of(*reagent).is_positive())
        })
        .collect();
    candidates.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| product_name(db, a.id).cmp(&product_name(db, b.id)))
    });
    candidates
}

fn highest_priority<'a>(reactions: &[&'a chem_sim::Reaction]) -> Option<&'a chem_sim::Reaction> {
    reactions.iter().copied().fold(None, |selected, reaction| {
        Some(match selected {
            Some(current) if current.priority >= reaction.priority => current,
            _ => reaction,
        })
    })
}

/// The best known ambient method represented by the loaded ingredients.
/// Locked methods remain absent: feedback helps execute earned knowledge
/// without disclosing the recipe book.
fn chamber_forecast(
    db: &ChemDb,
    knowledge: &Knowledge,
    solution: &chem_sim::Solution,
) -> Option<ChamberForecast> {
    let ingredients_present = |reaction: &chem_sim::Reaction| {
        reaction.process.is_ambient()
            && reaction
                .reactants
                .iter()
                .all(|(reagent, _)| solution.volume_of(*reagent).is_positive())
    };
    let candidates: Vec<&chem_sim::Reaction> =
        represented_chamber_reactions(db, knowledge, solution)
            .into_iter()
            .filter(|reaction| ingredients_present(reaction))
            .collect();
    let active: Vec<&chem_sim::Reaction> = candidates
        .iter()
        .copied()
        .filter(|reaction| reaction.max_scale(solution).is_some())
        .collect();
    let reaction = highest_priority(&active).or_else(|| highest_priority(&candidates))?;
    let ready = reaction.max_scale(solution).is_some();
    let mut lines = Vec::new();
    let temperature = solution.temperature;

    if let Some(minimum) = reaction.min_temp {
        if temperature < minimum {
            lines.push(format!(
                "Temperature low: {temperature}; heat to at least {minimum}."
            ));
        }
    }
    if let Some(maximum) = reaction.max_temp {
        if temperature > maximum {
            lines.push(format!(
                "Temperature high: {temperature}; cool to {maximum} or below."
            ));
        }
    }
    let ph = solution.ph();
    match (reaction.min_ph, reaction.max_ph) {
        (Some(minimum), _) if ph < minimum => lines.push(format!(
            "pH too acidic: {ph:.1}; add basic buffer to reach {minimum:.1} or above."
        )),
        (_, Some(maximum)) if ph > maximum => lines.push(format!(
            "pH too alkaline: {ph:.1}; add acidic buffer to reach {maximum:.1} or below."
        )),
        _ if reaction.min_ph.is_some() || reaction.max_ph.is_some() => lines.push(format!(
            "pH {ph:.1} is inside the operating range{}.",
            reaction
                .optimal_ph
                .map(|optimum| format!("; optimum {optimum:.1}"))
                .unwrap_or_default()
        )),
        _ => {}
    }
    let purity = solution.average_purity();
    if let Some(minimum) = reaction.min_purity {
        if purity < minimum {
            lines.push(format!(
                "Input purity too low: {:.0}%; requires {:.0}%.",
                purity * 100.0,
                minimum * 100.0
            ));
        } else {
            lines.push(format!(
                "Input purity {:.0}% meets the {:.0}% minimum.",
                purity * 100.0,
                minimum * 100.0
            ));
        }
    }
    for higher in candidates
        .iter()
        .copied()
        .filter(|other| other.priority > reaction.priority)
    {
        let missing: Vec<&str> = higher
            .catalysts
            .iter()
            .filter(|(reagent, amount)| !solution.contains_at_least(*reagent, *amount))
            .map(|(reagent, _)| db.reagents.get(*reagent).name.as_str())
            .collect();
        if !missing.is_empty() {
            lines.push(format!(
                "Safer {} route blocked: missing {}.",
                product_name(db, higher.id),
                missing.join(", ")
            ));
        }
    }
    if ready {
        let rate = reaction
            .step_limit(1.0, temperature)
            .map(|amount| format!("approximately {amount} reaction-u/s"))
            .unwrap_or_else(|| "instant once conditions are met".to_string());
        lines.push(format!(
            "Process ready: {rate}; predicted output purity {:.0}%.",
            reaction.product_purity(solution) * 100.0
        ));
    }
    if reaction.is_overheated(temperature) {
        lines.push("DANGER: batch is beyond its authored overheat threshold.".to_string());
    }
    let hazardous = reaction_is_hazardous(reaction, temperature);
    if hazardous {
        lines.push("DANGER: this method releases explosive energy.".to_string());
    }

    Some(ChamberForecast {
        product: product_name(db, reaction.id),
        target: chamber_target(reaction),
        ph_target: ph_target(reaction),
        lines,
        ready,
        hazardous,
    })
}

fn ph_fraction(ph: f32) -> f32 {
    (ph / 14.0).clamp(0.0, 1.0)
}

/// Keeps a fixed-width marker visible at both clipped ends of the pH track.
fn ph_marker_percent(ph: f32) -> f32 {
    1.0 + ph_fraction(ph) * 98.0
}

fn buffer_guidance(forecast: Option<&ChamberForecast>, ph: f32) -> String {
    if let Some(line) = forecast.and_then(|forecast| {
        forecast
            .lines
            .iter()
            .find(|line| line.contains("add acidic buffer") || line.contains("add basic buffer"))
    }) {
        return line.clone();
    }
    let Some(target) = forecast.and_then(|forecast| forecast.ph_target) else {
        return "Buffer: no adjustment indicated.".to_string();
    };
    let Some(optimum) = target.optimum else {
        return "Buffer: pH is inside the operating range.".to_string();
    };
    if ph > optimum + 0.05 {
        format!(
            "pH is usable; add acidic buffer toward the {optimum:.1} optimum for better purity."
        )
    } else if ph < optimum - 0.05 {
        format!("pH is usable; add basic buffer toward the {optimum:.1} optimum for better purity.")
    } else {
        format!("Buffer: pH is at the {optimum:.1} optimum.")
    }
}

fn ph_gauge(section: &mut ChildSpawnerCommands, ph: f32, target: Option<PhTarget>) {
    section
        .spawn((
            Node {
                position_type: PositionType::Relative,
                width: percent(100),
                height: px(14),
                margin: UiRect::vertical(px(5)),
                border_radius: BorderRadius::all(px(7)),
                overflow: Overflow::clip(),
                ..default()
            },
            BackgroundColor(Color::srgb(0.10, 0.11, 0.14)),
        ))
        .with_children(|track| {
            for (width, color) in [
                (43.0, Color::srgb(0.82, 0.30, 0.23)),
                (14.0, Color::srgb(0.30, 0.76, 0.39)),
                (43.0, Color::srgb(0.34, 0.38, 0.86)),
            ] {
                track.spawn((
                    Node {
                        width: percent(width),
                        height: percent(100),
                        ..default()
                    },
                    BackgroundColor(color),
                ));
            }
            if let Some(target) = target {
                track.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: percent(ph_fraction(target.min) * 100.0),
                        width: percent((ph_fraction(target.max) - ph_fraction(target.min)) * 100.0),
                        height: percent(100),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.95, 0.97, 1.0, 0.34)),
                ));
                if let Some(optimum) = target.optimum {
                    track.spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            left: percent(ph_marker_percent(optimum)),
                            width: px(2),
                            height: percent(100),
                            ..default()
                        },
                        BackgroundColor(Color::srgb(1.0, 1.0, 1.0)),
                    ));
                }
            }
            track.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: percent(ph_marker_percent(ph)),
                    width: px(4),
                    height: percent(100),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.06, 0.07, 0.09)),
            ));
        });
}

/// Reaction-chamber instrument panel. It exposes the actual process controls
/// and forecasts known methods without pretending the chamber owns buffer
/// reservoirs: buffers still have to be prepared and added as reagents.
fn heater_body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    thermostat: Option<&Thermostat>,
    container_entity: Option<Entity>,
    loaded: Option<&Container>,
    reacting: bool,
) {
    let thermostat = thermostat.copied().unwrap_or_default();
    let current = loaded.map(|container| container.solution.temperature);
    let forecast =
        loaded.and_then(|container| chamber_forecast(db, knowledge, &container.solution));

    panel
        .spawn(Node {
            width: percent(100),
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            ..default()
        })
        .with_children(|header| {
            header.spawn(label("PROCESS CONTROLS", 12.0, TEXT_DIM));
            let mut power = header.spawn(button(
                if thermostat.powered {
                    "● Chamber on"
                } else {
                    "○ Chamber off"
                },
                PanelAction::TogglePower,
            ));
            if thermostat.powered {
                power.insert(BackgroundColor(BUTTON_ACTIVE));
            }
        });

    panel
        .spawn(Node {
            width: percent(100),
            flex_direction: FlexDirection::Row,
            column_gap: px(8),
            ..default()
        })
        .with_children(|controls| {
            controls
                .spawn((
                    Node {
                        flex_basis: percent(0),
                        flex_grow: 1.15,
                        flex_direction: FlexDirection::Column,
                        padding: UiRect::all(px(10)),
                        row_gap: px(4),
                        border_radius: BorderRadius::all(px(5)),
                        ..default()
                    },
                    BackgroundColor(SECTION_BG),
                ))
                .with_children(|thermal| {
                    thermal.spawn(label("THERMAL CONTROL", 13.0, TEXT_DIM));
                    thermal
                        .spawn(Node {
                            width: percent(100),
                            justify_content: JustifyContent::SpaceBetween,
                            align_items: AlignItems::End,
                            ..default()
                        })
                        .with_children(|readout| {
                            readout.spawn(label(
                                current
                                    .map(|value| format!("Reading  {:.0} K", value.0))
                                    .unwrap_or_else(|| "Reading  — K".to_string()),
                                15.0,
                                Color::srgb(0.66, 0.78, 0.92),
                            ));
                            readout.spawn((
                                Text::new(format!("Target  {:.0} K", thermostat.target.0)),
                                TextFont::from_font_size(14.0),
                                TextColor(Color::srgb(0.52, 0.75, 0.96)),
                                TempSliderReadout,
                            ));
                        });

                    thermal
                        .spawn((
                            Button,
                            Node {
                                position_type: PositionType::Relative,
                                width: percent(100),
                                height: px(TEMP_SLIDER_TRACK_HEIGHT),
                                margin: UiRect::vertical(px(8)),
                                border_radius: BorderRadius::all(
                                    px(TEMP_SLIDER_TRACK_HEIGHT / 2.0),
                                ),
                                ..default()
                            },
                            BackgroundColor(Color::srgb(0.08, 0.09, 0.12)),
                            TempSlider,
                            PreserveButtonBackground,
                        ))
                        .with_children(|track| {
                            track.spawn((
                                Node {
                                    width: percent(temp_fraction_of(thermostat.target.0) * 100.0),
                                    height: percent(100),
                                    border_radius: BorderRadius::all(px(
                                        TEMP_SLIDER_TRACK_HEIGHT / 2.0
                                    )),
                                    ..default()
                                },
                                BackgroundColor(BUTTON_ACTIVE),
                                TempSliderFill,
                            ));
                        });
                    thermal.spawn(label(
                        format!("{TEMPERATURE_MIN:.0} K                                    {TEMPERATURE_MAX:.0} K"),
                        12.0,
                        TEXT_DIM,
                    ));
                });

            controls
                .spawn((
                    Node {
                        flex_basis: percent(0),
                        flex_grow: 1.0,
                        flex_direction: FlexDirection::Column,
                        padding: UiRect::all(px(10)),
                        row_gap: px(4),
                        border_radius: BorderRadius::all(px(5)),
                        ..default()
                    },
                    BackgroundColor(SECTION_BG),
                ))
                .with_children(|quality| {
                    quality.spawn(label("SOLUTION CONTROL", 13.0, TEXT_DIM));
                    if let Some(container) = loaded {
                        let ph = container.solution.ph();
                        quality.spawn(label(
                            format!(
                                "pH  {:.2}       purity  {:.0}%",
                                ph,
                                container.solution.average_purity() * 100.0
                            ),
                            15.0,
                            TEXT,
                        ));
                        ph_gauge(quality, ph, forecast.as_ref().and_then(|f| f.ph_target));
                        let guidance = buffer_guidance(forecast.as_ref(), ph);
                        quality.spawn(label(guidance, 13.0, TEXT_DIM));
                        quality.spawn(label(
                            "Acidic/basic buffer is added to the beaker as reagent.",
                            12.0,
                            TEXT_DIM,
                        ));
                    } else {
                        quality.spawn(label("pH  —       purity  —", 15.0, TEXT_DIM));
                        ph_gauge(quality, 7.0, None);
                        quality.spawn(label("Load a beaker to begin monitoring.", 13.0, TEXT_DIM));
                    }
                });
        });

    panel.spawn(label("REACTION MONITOR", 12.0, TEXT_DIM));
    panel
        .spawn((section(), BackgroundColor(SECTION_BG)))
        .with_children(|monitor| {
            monitor
                .spawn(Node {
                    width: percent(100),
                    padding: UiRect::horizontal(px(5)),
                    ..default()
                })
                .with_children(|header| {
                    header.spawn(hplc_cell("METHOD", 220.0, 12.0, TEXT_DIM));
                    header.spawn(hplc_cell("STATUS", 100.0, 12.0, TEXT_DIM));
                    header.spawn(hplc_cell("TARGET", 330.0, 12.0, TEXT_DIM));
                });
            match loaded {
                Some(container) => {
                    let represented =
                        represented_chamber_reactions(db, knowledge, &container.solution);
                    if represented.is_empty() {
                        monitor.spawn(label(
                            "No known ambient method is represented by this sample.",
                            12.0,
                            TEXT_DIM,
                        ));
                    }
                    for reaction in represented.into_iter().take(3) {
                        let ready = reaction.max_scale(&container.solution).is_some();
                        let hazardous =
                            reaction_is_hazardous(reaction, container.solution.temperature);
                        monitor
                            .spawn(Node {
                                width: percent(100),
                                min_height: px(26),
                                padding: UiRect::axes(px(5), px(3)),
                                ..default()
                            })
                            .with_children(|row| {
                                row.spawn(hplc_cell(
                                    product_name(db, reaction.id),
                                    220.0,
                                    12.0,
                                    TEXT,
                                ));
                                row.spawn(hplc_cell(
                                    if ready { "READY" } else { "BLOCKED" },
                                    100.0,
                                    11.0,
                                    if ready { GOOD_TEXT } else { HPLC_IMPURITY },
                                ));
                                row.spawn(hplc_cell(
                                    if hazardous {
                                        format!("⚠  {}", chamber_target(reaction))
                                    } else {
                                        chamber_target(reaction)
                                    },
                                    330.0,
                                    11.0,
                                    if hazardous { ERROR_TEXT } else { TEXT_DIM },
                                ));
                            });
                    }
                    if let Some(forecast) = &forecast {
                        monitor.spawn(label(
                            format!("PRIMARY   {}   •   {}", forecast.product, forecast.target),
                            12.0,
                            if forecast.hazardous {
                                ERROR_TEXT
                            } else if forecast.ready {
                                GOOD_TEXT
                            } else {
                                HPLC_IMPURITY
                            },
                        ));
                        for line in forecast.lines.iter().take(3) {
                            monitor.spawn(label(
                                line,
                                11.0,
                                if line.starts_with("DANGER:") {
                                    ERROR_TEXT
                                } else {
                                    TEXT_DIM
                                },
                            ));
                        }
                    }
                }
                None => {
                    monitor.spawn(label(
                        "Load a sample to forecast known methods.",
                        12.0,
                        TEXT_DIM,
                    ));
                }
            }
        });

    panel
        .spawn((section(), BackgroundColor(SECTION_BG)))
        .with_children(|section| {
            section.spawn(row()).with_children(|preview_row| {
                beaker_preview(preview_row, container_entity);
                preview_row
                    .spawn(Node {
                        flex_direction: FlexDirection::Column,
                        row_gap: px(3),
                        flex_grow: 1.0,
                        ..default()
                    })
                    .with_children(|beaker| {
                        beaker
                            .spawn(Node {
                                width: percent(100),
                                justify_content: JustifyContent::SpaceBetween,
                                align_items: AlignItems::Center,
                                ..default()
                            })
                            .with_children(|header| {
                                header.spawn(label(
                                    loaded
                                        .map(|container| {
                                            format!(
                                                "BEAKER   {} / {}",
                                                container.solution.total_volume(),
                                                container.kind.capacity()
                                            )
                                        })
                                        .unwrap_or_else(|| "BEAKER   — / —".to_string()),
                                    13.0,
                                    TEXT,
                                ));
                                header.spawn(button("Eject", PanelAction::Eject(MachineSlot::A)));
                            });
                        if let Some(container) = loaded {
                            if container.solution.is_empty() {
                                beaker.spawn(label("Empty.", 12.0, TEXT_DIM));
                            } else {
                                beaker.spawn(wrap_row()).with_children(|contents| {
                                    for (reagent, amount) in container.solution.iter() {
                                        contents.spawn(label(
                                            format!(
                                                "{amount} {}   ",
                                                db.reagents.get(reagent).name
                                            ),
                                            11.0,
                                            TEXT_DIM,
                                        ));
                                    }
                                });
                            }
                        } else {
                            beaker.spawn(label("Carry a beaker over and press E.", 12.0, TEXT_DIM));
                        }
                        if reacting {
                            beaker.spawn(label("◌ Reaction in progress", 13.0, GOOD_TEXT));
                        }
                    });
            });
        });
}

/// Drags the reaction chamber's dial while the mouse is held on it.
///
/// The same shape as `settings::drag_sliders`, but scoped to a specific
/// machine over the network rather than a local resource: dragging writes
/// `SetTargetTemperature` requests, and the server — not this system — is
/// what actually moves `Thermostat.target`. Only one [`TempSlider`] is ever
/// alive at once, so unlike `drag_sliders` this never has to work out *which*
/// track a press landed on.
fn drag_thermostat_slider(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    track: Query<(&Interaction, &ComputedNode, &UiGlobalTransform), With<TempSlider>>,
    modes: Query<&InteractionMode, With<LocalPlayer>>,
    mut held: Local<bool>,
    mut drag: ResMut<ThermostatDrag>,
    mut set_target: MessageWriter<SetTargetTemperature>,
) {
    if !mouse.pressed(MouseButton::Left) {
        *held = false;
        drag.0 = None;
        return;
    }
    let Ok((interaction, node, transform)) = track.single() else {
        // No slider on screen — the panel is closed, or showing a different
        // machine — so there is nothing to drag and nothing to keep held.
        *held = false;
        drag.0 = None;
        return;
    };
    if !*held {
        *held = *interaction != Interaction::None;
    }
    if !*held {
        return;
    }
    let Some(InteractionMode::UsingMachine(machine)) = modes.iter().next().copied() else {
        return;
    };
    let Ok(window) = windows.single() else {
        return;
    };
    let Some(cursor) = window.cursor_position() else {
        return;
    };
    let Some(local) = node.normalize_point(*transform, cursor) else {
        return;
    };
    let value = temp_at_fraction(local.x + 0.5);
    if drag.0 != Some(value) {
        drag.0 = Some(value);
        set_target.write(SetTargetTemperature {
            machine,
            target: Kelvin(value),
        });
    }
}

/// Keeps the dial's fill and printed value on the live target.
///
/// Reads the locally-held drag value first, so the dragging chemist's own
/// view never fights their own input while it waits on a round trip to the
/// server; falls back to the replicated `Thermostat.target` otherwise, which
/// is what draws it correctly the instant the panel opens, before any drag
/// has happened at all.
fn sync_thermostat_slider(
    drag: Res<ThermostatDrag>,
    thermostats: Query<&Thermostat>,
    modes: Query<&InteractionMode, With<LocalPlayer>>,
    mut fills: Query<&mut Node, With<TempSliderFill>>,
    mut readouts: Query<&mut Text, With<TempSliderReadout>>,
) {
    let value = match drag.0 {
        Some(value) => Some(value),
        None => match modes.iter().next().copied() {
            Some(InteractionMode::UsingMachine(machine)) => thermostats
                .get(machine)
                .ok()
                .map(|thermostat| thermostat.target.0),
            _ => None,
        },
    };
    let Some(value) = value else {
        return;
    };
    for mut node in &mut fills {
        node.width = percent(temp_fraction_of(value) * 100.0);
    }
    for mut text in &mut readouts {
        let wanted = format!("{value:.0}K");
        if text.0 != wanted {
            text.0 = wanted;
        }
    }
}

/// Two beaker slots sharing one buffer, rather than the single slot every
/// other machine has: pull a reagent from Beaker A into the buffer, then push
/// it into Beaker B, without ejecting one to swap the other in.
const HPLC_CLEAN: Color = Color::srgb(0.24, 0.86, 0.58);
const HPLC_IMPURITY: Color = Color::srgb(0.96, 0.62, 0.20);
const HPLC_INVERSE: Color = Color::srgb(0.88, 0.12, 0.48);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HplcProfile {
    Clean,
    Impure,
    Recoverable,
}

impl HplcProfile {
    fn of(purity: f32, recoverable: bool) -> Self {
        if recoverable {
            Self::Recoverable
        } else if purity < 0.98 {
            Self::Impure
        } else {
            Self::Clean
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Clean => "CLEAN",
            Self::Impure => "IMPURITY",
            Self::Recoverable => "INVERSE",
        }
    }

    fn color(self) -> Color {
        match self {
            Self::Clean => HPLC_CLEAN,
            Self::Impure => HPLC_IMPURITY,
            Self::Recoverable => HPLC_INVERSE,
        }
    }
}

fn selected_hplc_reagent(
    solution: &chem_sim::Solution,
    requested: Option<ReagentId>,
) -> Option<ReagentId> {
    requested
        .filter(|reagent| solution.volume_of(*reagent).is_positive())
        .or_else(|| solution.iter().next().map(|(reagent, _)| reagent))
}

fn hplc_cell(text: impl Into<String>, width: f32, size: f32, color: Color) -> impl Bundle {
    (
        Text::new(text.into()),
        TextFont::from_font_size(size),
        TextColor(color),
        Node {
            width: px(width),
            flex_shrink: 0.0,
            ..default()
        },
    )
}

/// A compact chromatogram. The x position is a stable presentation band, not
/// a fake scientific mass measurement; height is the reagent's share and the
/// orange cap is the measured impurity fraction. Authored inverse material is
/// magenta because the HPLC can recover its paired useful reagent.
fn hplc_graph(
    section: &mut ChildSpawnerCommands,
    db: &ChemDb,
    solution: &chem_sim::Solution,
    selected: ReagentId,
) {
    let bands: Vec<_> = solution.iter().collect();
    let total = solution.total_volume().as_f32().max(0.01);

    section
        .spawn((
            Node {
                position_type: PositionType::Relative,
                width: percent(100),
                height: px(164),
                border_radius: BorderRadius::all(px(3)),
                ..default()
            },
            BackgroundColor(Color::srgb(0.045, 0.052, 0.065)),
        ))
        .with_children(|graph| {
            graph.spawn((
                Text::new("ABSORBANCE"),
                TextFont::from_font_size(12.0),
                TextColor(TEXT_DIM),
                Node {
                    position_type: PositionType::Absolute,
                    left: px(8),
                    top: px(5),
                    ..default()
                },
            ));
            graph.spawn((
                Text::new("RETENTION  →"),
                TextFont::from_font_size(12.0),
                TextColor(TEXT_DIM),
                Node {
                    position_type: PositionType::Absolute,
                    right: px(8),
                    bottom: px(3),
                    ..default()
                },
            ));

            for bottom in [42.0, 82.0, 122.0] {
                graph.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: px(30),
                        bottom: px(bottom),
                        width: percent(94),
                        height: px(1),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.50, 0.56, 0.65, 0.14)),
                ));
            }
            graph.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(30),
                    bottom: px(22),
                    width: percent(94),
                    height: px(2),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.70, 0.74, 0.80)),
            ));
            graph.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: px(29),
                    bottom: px(22),
                    width: px(2),
                    height: px(126),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.70, 0.74, 0.80)),
            ));

            for (index, (reagent, amount)) in bands.iter().copied().enumerate() {
                let definition = db.reagents.get(reagent);
                let purity = solution.purity_of(reagent);
                let profile = HplcProfile::of(purity, definition.recovers_to.is_some());
                let position = 10.0 + 82.0 * (index as f32 + 0.5) / bands.len() as f32;
                let height = (34.0 + amount.as_f32() / total * 92.0).clamp(34.0, 126.0);

                if reagent == selected {
                    graph.spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            left: percent(position - 2.5),
                            bottom: px(22),
                            width: percent(5),
                            height: px(126),
                            ..default()
                        },
                        BackgroundColor(Color::srgba(0.25, 0.92, 0.58, 0.10)),
                    ));
                }

                // Total measured band. Impure and inverse-capable material
                // carry their diagnostic colour behind the clean fraction.
                graph.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: percent(position),
                        bottom: px(23),
                        width: px(9),
                        height: px(height),
                        ..default()
                    },
                    BackgroundColor(profile.color()),
                ));
                if profile == HplcProfile::Impure {
                    graph.spawn((
                        Node {
                            position_type: PositionType::Absolute,
                            left: percent(position),
                            bottom: px(23),
                            width: px(5),
                            height: px((height * purity).max(3.0)),
                            ..default()
                        },
                        BackgroundColor(HPLC_CLEAN),
                    ));
                }
                graph.spawn((
                    Text::new((index + 1).to_string()),
                    TextFont::from_font_size(12.0),
                    TextColor(if reagent == selected {
                        HPLC_CLEAN
                    } else {
                        TEXT_DIM
                    }),
                    Node {
                        position_type: PositionType::Absolute,
                        left: percent(position),
                        bottom: px(3),
                        ..default()
                    },
                ));
            }
        });
}

fn analyzer_body(
    panel: &mut ChildSpawnerCommands,
    training: Option<&crate::session::SessionKind>,
    training_calibrated: bool,
    db: &ChemDb,
    knowledge: &Knowledge,
    loaded: Option<&Container>,
    report: Option<&HplcReport>,
    requested_selection: Option<ReagentId>,
) {
    let calibrated =
        crate::tutorial::hplc_available(knowledge.known_count(), training, training_calibrated);
    let selected = loaded
        .and_then(|container| selected_hplc_reagent(&container.solution, requested_selection));

    panel
        .spawn(Node {
            width: percent(100),
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            ..default()
        })
        .with_children(|header| {
            header.spawn(label(
                if calibrated {
                    "HPLC / MASS PROFILE   •   CALIBRATED".to_string()
                } else {
                    format!(
                        "HPLC / MASS PROFILE   •   CALIBRATING {}/{}",
                        knowledge.known_count(),
                        HPLC_RECIPE_REQUIREMENT
                    )
                },
                12.0,
                if calibrated { GOOD_TEXT } else { HPLC_IMPURITY },
            ));
            if let Some(reagent) = selected.filter(|_| calibrated) {
                let definition = db.reagents.get(reagent);
                let text = definition
                    .recovers_to
                    .as_deref()
                    .and_then(|key| db.reagents.id_of(key))
                    .map(|recovered| {
                        format!("Recover {}", db.reagents.get(recovered).name)
                    })
                    .unwrap_or_else(|| format!("Purify {}", definition.name));
                header.spawn(button(text, PanelAction::Purify(reagent)));
            }
        });

    panel
        .spawn((section(), BackgroundColor(SECTION_BG)))
        .with_children(|section| {
            let Some(container) = loaded else {
                section.spawn(label(
                    "INPUT SAMPLE   —   no container loaded. Carry one over and press E.",
                    14.0,
                    TEXT_DIM,
                ));
                return;
            };
            if container.solution.is_empty() {
                section.spawn(label(
                    "INPUT SAMPLE   —   container is empty.",
                    14.0,
                    TEXT_DIM,
                ));
                section.spawn(button("Eject", PanelAction::Eject(MachineSlot::A)));
                return;
            }
            let selected = selected.expect("a non-empty solution has a selectable reagent");

            section.spawn(label(
                format!(
                    "INPUT SAMPLE   {} / {}   •   pH {:.2}   •   {:.0}% average purity",
                    container.solution.total_volume(),
                    container.kind.capacity(),
                    container.solution.ph(),
                    container.solution.average_purity() * 100.0,
                ),
                13.0,
                TEXT,
            ));
            hplc_graph(section, db, &container.solution, selected);
            section.spawn(wrap_row()).with_children(|legend| {
                for (name, color) in [
                    ("■ clean fraction", HPLC_CLEAN),
                    ("■ impurity", HPLC_IMPURITY),
                    ("■ recoverable inverse", HPLC_INVERSE),
                    ("▯ selected band", Color::srgb(0.45, 0.95, 0.68)),
                ] {
                    legend.spawn(label(name, 12.0, color));
                }
            });

            section
                .spawn(Node {
                    width: percent(100),
                    padding: UiRect::axes(px(8), px(3)),
                    ..default()
                })
                .with_children(|row| {
                    row.spawn(hplc_cell("BAND / REAGENT", 250.0, 12.0, TEXT_DIM));
                    row.spawn(hplc_cell("VOLUME", 100.0, 12.0, TEXT_DIM));
                    row.spawn(hplc_cell("PURITY", 90.0, 12.0, TEXT_DIM));
                    row.spawn(hplc_cell("PROFILE", 130.0, 12.0, TEXT_DIM));
                });

            section
                .spawn((
                    Node {
                        width: percent(100),
                        max_height: px(154),
                        flex_direction: FlexDirection::Column,
                        overflow: Overflow::scroll_y(),
                        ..default()
                    },
                    ScrollPosition::default(),
                    ScrollPane,
                ))
                .with_children(|table| {
                    for (index, (reagent, amount)) in container.solution.iter().enumerate() {
                        let definition = db.reagents.get(reagent);
                        let purity = container.solution.purity_of(reagent);
                        let profile = HplcProfile::of(purity, definition.recovers_to.is_some());
                        let is_selected = reagent == selected;
                        let mut row = table.spawn((
                            Button,
                            Node {
                                width: percent(100),
                                min_height: px(30),
                                padding: UiRect::axes(px(8), px(4)),
                                align_items: AlignItems::Center,
                                ..default()
                            },
                            BackgroundColor(if is_selected {
                                Color::srgb(0.12, 0.34, 0.27)
                            } else {
                                Color::srgba(0.05, 0.06, 0.08, 0.55)
                            }),
                            PanelAction::SelectHplc(reagent),
                        ));
                        if is_selected {
                            row.insert(Selected);
                        }
                        row.with_children(|row| {
                            let [r, g, b] = definition.color;
                            row.spawn(hplc_cell(
                                format!("{:02}   {}", index + 1, definition.name),
                                250.0,
                                13.0,
                                Color::srgb(0.45 + r * 0.55, 0.45 + g * 0.55, 0.45 + b * 0.55),
                            ));
                            row.spawn(hplc_cell(amount.to_string(), 100.0, 13.0, TEXT));
                            row.spawn(hplc_cell(
                                format!("{:.0}%", purity * 100.0),
                                90.0,
                                13.0,
                                if purity >= 0.98 {
                                    GOOD_TEXT
                                } else {
                                    HPLC_IMPURITY
                                },
                            ));
                            row.spawn(hplc_cell(profile.label(), 130.0, 12.0, profile.color()));
                        });
                        let material = definition.material.description();
                        if !material.is_empty() {
                            table.spawn(label(material, 12.0, HPLC_IMPURITY));
                        }
                    }
                });

            let unknown = db
                .reactions
                .iter()
                .filter(|reaction| !knowledge.is_known(reaction.id))
                .filter(|reaction| {
                    reaction
                        .product_ids()
                        .any(|id| container.solution.volume_of(id).is_positive())
                })
                .count();
            section.spawn(row()).with_children(|row| {
                row.spawn(button("Analyze", PanelAction::Analyze));
                row.spawn(button("Eject", PanelAction::Eject(MachineSlot::A)));
                row.spawn(label(
                    if unknown == 0 {
                        "No unrecorded signatures".to_string()
                    } else {
                        format!("{unknown} unrecorded signature(s)")
                    },
                    12.0,
                    if unknown == 0 { TEXT_DIM } else { GOOD_TEXT },
                ));
            });
        });

    if let Some(report) = report {
        let source = &db.reagents.get(report.source).name;
        let product = &db.reagents.get(report.product).name;
        panel.spawn(label(
            if report.recovered_inverse {
                format!(
                    "LAST RUN   {source} {:.0}% → {product} {} at {:.0}%   •   reject {}",
                    report.input_purity * 100.0,
                    report.product_amount,
                    report.product_purity * 100.0,
                    report.reject_amount,
                )
            } else {
                format!(
                    "LAST RUN   {source} {} at {:.0}% → {} at {:.0}%   •   reject {}",
                    report.input_amount,
                    report.input_purity * 100.0,
                    report.product_amount,
                    report.product_purity * 100.0,
                    report.reject_amount,
                )
            },
            12.0,
            GOOD_TEXT,
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn grinder_body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    catalog: Option<&ProduceCatalog>,
    hopper: Option<&Hopper>,
    container_entity: Option<Entity>,
    loaded: Option<&Container>,
    marked: Option<&crate::labels::Label>,
    reacting: bool,
) {
    panel.spawn(label(
        "Extracts produce straight into the beaker. Fast, and never clean — \
         what comes out still has to go through the Mixing Chamber.",
        13.0,
        TEXT_DIM,
    ));

    panel.spawn(label("Hopper", 13.0, TEXT_DIM));
    panel
        .spawn((section(), BackgroundColor(SECTION_BG)))
        .with_children(|section| {
            let (Some(catalog), Some(hopper)) = (catalog, hopper) else {
                return;
            };
            if hopper.0.is_empty() {
                section.spawn(label(
                    "Empty. Carry produce over from the counter and press E.",
                    14.0,
                    TEXT_DIM,
                ));
                return;
            }

            // Grouped by kind: five separate "Poppy" rows tell the player
            // nothing a count does not.
            for kind in catalog.iter() {
                let count = hopper.0.iter().filter(|id| **id == kind.id).count();
                if count == 0 {
                    continue;
                }
                let yields = kind
                    .yields
                    .iter()
                    .map(|(id, amount)| format!("{amount} {}", db.reagents.get(*id).name))
                    .collect::<Vec<_>>()
                    .join(" + ");
                let [r, g, b] = kind.color;
                section.spawn(label(
                    format!("{count} × {:<20} → {yields}", kind.name),
                    14.0,
                    Color::srgb(0.45 + r * 0.55, 0.45 + g * 0.55, 0.45 + b * 0.55),
                ));
            }
        });

    panel.spawn(row()).with_children(|row| {
        row.spawn(button("Grind one", PanelAction::Grind { all: false }));
        row.spawn(button("Grind all", PanelAction::Grind { all: true }));
    });

    container_readout(panel, db, container_entity, loaded, marked, reacting, true);
}

/// The shelf.
///
/// Every row is one stored item and a button to get it back. There is no
/// "put in" button on purpose: things go in the way everything else in the lab
/// goes in, by carrying them over and pressing E, and a panel button for it
/// would be a second way to do the thing the walk-up already does.
fn locker_body(panel: &mut ChildSpawnerCommands, stored: &[StoredItem]) {
    panel.spawn(label(
        "Shelf space. Carry anything over and press E to put it away; it comes \
         back out into your hand.",
        13.0,
        TEXT_DIM,
    ));

    panel.spawn(label(
        format!("Contents   {} / {LOCKER_CAPACITY}", stored.len()),
        13.0,
        TEXT_DIM,
    ));
    panel
        .spawn((section(), BackgroundColor(SECTION_BG)))
        .with_children(|section| {
            if stored.is_empty() {
                section.spawn(label("Empty.", 14.0, TEXT_DIM));
                return;
            }

            for item in stored {
                section.spawn(row()).with_children(|row| {
                    row.spawn(button("Take", PanelAction::Take(item.item)));
                    row.spawn(label(
                        if item.detail.is_empty() {
                            item.name.clone()
                        } else {
                            format!("{}   —   {}", item.name, item.detail)
                        },
                        14.0,
                        TEXT,
                    ));
                });
            }
        });

    if stored.len() >= LOCKER_CAPACITY {
        panel.spawn(label(
            "Full. Take something out before putting anything else away.",
            13.0,
            ERROR_TEXT,
        ));
    }
}

/// The counter tray.
///
/// There are no buttons to hand anything over: the window matches on its own
/// the moment somebody at the counter wants what is in it. So the panel's job
/// is to say what will happen and, when nothing does, why not.
type DeliverySlotView<'a> = (
    MachineSlot,
    Option<Entity>,
    Option<&'a Container>,
    Option<&'a crate::labels::Label>,
    bool,
);

fn delivery_window_body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    slots: [DeliverySlotView<'_>; 3],
) {
    panel.spawn(label(
        "INPUT  ·  THREE-POSITION DELIVERY TRAY",
        11.0,
        BOOK_ACCENT,
    ));
    panel.spawn(label(
        "Finished batches route automatically. Hover a control for details.",
        13.0,
        TEXT_DIM,
    ));
    panel
        .spawn(Node {
            width: percent(100),
            column_gap: px(8),
            align_items: AlignItems::Stretch,
            ..default()
        })
        .with_children(|tray| {
            for (slot, entity, loaded, marked, reacting) in slots {
                delivery_slot_card(tray, db, slot, entity, loaded, marked, reacting);
            }
        });
}

fn delivery_slot_card(
    tray: &mut ChildSpawnerCommands,
    db: &ChemDb,
    slot: MachineSlot,
    container_entity: Option<Entity>,
    loaded: Option<&Container>,
    marked: Option<&crate::labels::Label>,
    reacting: bool,
) {
    let slot_name = match slot {
        MachineSlot::A => "A",
        MachineSlot::B => "B",
        MachineSlot::C => "C",
    };
    tray.spawn((
        Node {
            width: percent(32),
            min_height: px(190),
            flex_direction: FlexDirection::Column,
            padding: UiRect::all(px(9)),
            row_gap: px(5),
            border: UiRect::all(px(1)),
            border_radius: BorderRadius::all(px(6)),
            ..default()
        },
        BackgroundColor(SECTION_BG),
        BorderColor::all(Color::srgb(0.20, 0.27, 0.32)),
    ))
    .with_children(|card| {
        card.spawn(label(format!("TRAY {slot_name}"), 13.0, TEXT_DIM));
        beaker_preview(card, container_entity);
        let Some(container) = loaded else {
            card.spawn(label("READY", 13.0, TEXT_DIM));
            card.spawn(label("Carry container + E", 12.0, TEXT_DIM));
            return;
        };

        card.spawn(label(
            format!(
                "{}  {} / {}",
                container.kind.label(),
                container.solution.total_volume(),
                container.kind.capacity()
            ),
            13.0,
            TEXT,
        ));
        if let Some(marked) = marked.filter(|marked| !marked.0.trim().is_empty()) {
            card.spawn(label(format!("Marked \"{}\"", marked.0), 13.0, LABEL_INK));
        }
        if !container.solution.is_empty() {
            card.spawn(label(
                format!(
                    "pH {:.2}  ·  {:.0}% pure",
                    container.solution.ph(),
                    container.solution.average_purity() * 100.0
                ),
                13.0,
                Color::srgb(0.66, 0.78, 0.92),
            ));
        }
        let (status, color) = if container.solution.is_empty() {
            ("EMPTY", TEXT_DIM)
        } else if reacting {
            ("REACTING · HELD", Color::srgb(0.95, 0.88, 0.45))
        } else {
            ("WAITING FOR MATCH", GOOD_TEXT)
        };
        card.spawn(label(status, 13.0, color));

        let contents = container
            .solution
            .iter()
            .map(|(reagent, quantity)| format!("{quantity} {}", db.reagents.get(reagent).name))
            .collect::<Vec<_>>()
            .join(" · ");
        if !contents.is_empty() {
            card.spawn(label(contents, 13.0, TEXT));
        }
        card.spawn(button("Eject", PanelAction::Eject(slot)))
            .insert(TooltipSource::new(
                format!("Eject tray {slot_name}"),
                "Return this container to the chemist without changing its contents.",
            ));
    });
}

/// Shared contents readout for whatever is sitting in the machine's slot.
#[allow(clippy::too_many_arguments)]
fn container_readout(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    container_entity: Option<Entity>,
    loaded: Option<&Container>,
    marked: Option<&crate::labels::Label>,
    reacting: bool,
    show_empty_button: bool,
) {
    card(panel, "Loaded container", |section| {
        section.spawn(row()).with_children(|preview_row| {
            beaker_preview(preview_row, container_entity);
            preview_row
                .spawn(Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: px(3),
                    flex_grow: 1.0,
                    ..default()
                })
                .with_children(|column| {
                    let Some(container) = loaded else {
                        column.spawn(label(
                            "No container loaded. Carry a beaker over and press E.",
                            14.0,
                            TEXT_DIM,
                        ));
                        return;
                    };

                    column.spawn(label(
                        format!(
                            "{}   {} / {}",
                            container.kind.label(),
                            container.solution.total_volume(),
                            container.kind.capacity()
                        ),
                        15.0,
                        TEXT,
                    ));

                    // Under the real readout, never instead of it. The
                    // chemist always knows what they made; the label is what
                    // everyone *else* will read, and seeing both at once is
                    // the point of showing it here at all.
                    if let Some(marked) = marked.filter(|marked| !marked.0.trim().is_empty()) {
                        column.spawn(label(format!("Marked \"{}\"", marked.0), 13.0, LABEL_INK));
                    }

                    if container.solution.is_empty() {
                        column.spawn(label("Empty.", 14.0, TEXT_DIM));
                    } else {
                        column.spawn(label(
                            format!(
                                "pH {:.2}   ·   purity {:.0}%   ·   {}",
                                container.solution.ph(),
                                container.solution.average_purity() * 100.0,
                                container.solution.temperature
                            ),
                            13.0,
                            Color::srgb(0.66, 0.78, 0.92),
                        ));
                        for (reagent, quantity) in container.solution.iter() {
                            column.spawn(label(
                                format!(
                                    "{}  {}   ({:.0}% pure)",
                                    quantity,
                                    db.reagents.get(reagent).name,
                                    container.solution.purity_of(reagent) * 100.0
                                ),
                                14.0,
                                TEXT,
                            ));
                        }
                    }

                    // Some recipes take real seconds. The numbers above are already
                    // moving while one runs — that is the actual readout — but a
                    // chemist watching them needs to know the difference between
                    // "not finished yet" and "this is all you are getting".
                    if reacting {
                        column.spawn(label("Still reacting…", 13.0, GOOD_TEXT));
                    }

                    column.spawn(row()).with_children(|row| {
                        row.spawn(button("Eject", PanelAction::Eject(MachineSlot::A)));
                        if show_empty_button {
                            row.spawn(button("Empty", PanelAction::Empty(MachineSlot::A)));
                        }
                    });
                });
        });
    });
}

// ---------------------------------------------------------------------------
// Reference book
// ---------------------------------------------------------------------------

pub(crate) const BOOK_ACCENT: Color = Color::srgb(0.34, 0.66, 0.82);
const BOOK_PAPER: Color = Color::srgba(0.10, 0.12, 0.14, 0.98);
const BOOK_INSET: Color = Color::srgba(0.075, 0.085, 0.105, 0.96);

fn icon_control<A: Component>(
    action: A,
    title: impl Into<String>,
    body: impl Into<String>,
    width: f32,
    height: f32,
) -> impl Bundle {
    let title = title.into();
    (
        Button,
        Node {
            width: px(width),
            min_height: px(height),
            padding: UiRect::all(px(7)),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border: UiRect::all(px(1)),
            border_radius: BorderRadius::all(px(6)),
            ..default()
        },
        BackgroundColor(BUTTON_IDLE),
        BorderColor::all(Color::srgba(0.30, 0.36, 0.43, 0.75)),
        TooltipSource::new(title.clone(), body),
        accessibility_label(title, Role::Button),
        action,
    )
}

fn icon_badge(title: impl Into<String>, body: impl Into<String>, min_width: f32) -> impl Bundle {
    let title = title.into();
    (
        Node {
            min_width: px(min_width),
            min_height: px(30),
            padding: UiRect::axes(px(7), px(5)),
            align_items: AlignItems::Center,
            column_gap: px(5),
            border: UiRect::all(px(1)),
            border_radius: BorderRadius::all(px(5)),
            ..default()
        },
        BackgroundColor(BOOK_INSET),
        BorderColor::all(Color::srgba(0.25, 0.31, 0.38, 0.75)),
        Interaction::default(),
        Pickable {
            should_block_lower: false,
            is_hoverable: true,
        },
        TooltipSource::new(title.clone(), body),
        accessibility_label(title, Role::Image),
    )
}

fn fact_chip(
    parent: &mut ChildSpawnerCommands,
    icons: &BookIconAssets,
    icon: BookIcon,
    value: impl Into<String>,
    title: impl Into<String>,
    body: impl Into<String>,
    color: Color,
) {
    parent
        .spawn(icon_badge(title, body, 42.0))
        .with_children(|chip| {
            chip.spawn(icon_image(icons, icon, 18.0, color));
            chip.spawn(label(value.into(), 12.0, TEXT));
        });
}

fn status_seal(parent: &mut ChildSpawnerCommands, icons: &BookIconAssets, state: RecipeState) {
    parent
        .spawn(icon_badge(state.label(), state.explanation(), 34.0))
        .with_children(|seal| {
            seal.spawn(icon_image(icons, state.icon(), 19.0, state.color()));
            seal.spawn(label(state.label(), 13.0, state.color()));
        });
}

fn reaction_process_icon(reaction: &chem_sim::Reaction) -> BookIcon {
    match reaction.process {
        chem_sim::ReactionProcess::Ambient => {
            if reaction.min_temp.is_some() || reaction.max_temp.is_some() {
                BookIcon::ReactionChamber
            } else {
                BookIcon::DirectMix
            }
        }
        chem_sim::ReactionProcess::Agitated { .. } => BookIcon::MixingChamber,
    }
}

fn reaction_process_label(reaction: &chem_sim::Reaction) -> &'static str {
    match reaction.process {
        chem_sim::ReactionProcess::Ambient => {
            if reaction.min_temp.is_some() || reaction.max_temp.is_some() {
                "Reaction Chamber"
            } else {
                "Direct mixture"
            }
        }
        chem_sim::ReactionProcess::Agitated { .. } => "Mixing Chamber",
    }
}

fn filter_color(filter: BookFilter) -> Color {
    match filter {
        BookFilter::All => BOOK_ACCENT,
        BookFilter::Recorded => RecipeState::Recorded.color(),
        BookFilter::Ready => RecipeState::Ready.color(),
        BookFilter::Frontier => RecipeState::Frontier.color(),
        BookFilter::Locked => RecipeState::Locked.color(),
    }
}

/// The chemist's notes. Known recipes show the full method; locked ones show
/// only what a chemist would plausibly remember — what it treats, how many
/// ingredients, and whatever they have worked out so far.
///
/// Laid out as a sidebar of headings beside a scrolling pane. The single
/// column this replaced was written when there were nine recipes; there are
/// thirty-five now, and a chemist hunting for a burn treatment should not have
/// to scroll past the explosives to find it.
fn spawn_reference_book(
    commands: &mut Commands,
    db: &ChemDb,
    knowledge: &Knowledge,
    view: &BookView,
    // Opened over a machine panel, which the same key closes back onto. Only
    // the header line differs, but it is the line that tells the player they
    // have not just walked away from the machine.
    at_machine: bool,
    career_stage: CareerStage,
    successes: u32,
    icons: &BookIconAssets,
    // Fades and settles the shell when a bookmark tab just handed off to it;
    // `settled` for an ordinary rebuild, which must not replay the entrance.
    entrance: bookmarks::PanelEntrance,
) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.015, 0.02, 0.025, 0.72)),
            PanelRoot,
            entrance,
            crate::until_we_leave_the_lab(),
        ))
        .with_children(|screen| {
            screen
                .spawn((
                    Node {
                        width: percent(95),
                        max_width: px(1360),
                        height: vh(92),
                        flex_direction: FlexDirection::Column,
                        padding: UiRect::all(px(16)),
                        row_gap: px(8),
                        border: UiRect::all(px(2)),
                        border_radius: BorderRadius::all(px(10)),
                        ..default()
                    },
                    BackgroundColor(PANEL_BG),
                    BorderColor::all(Color::srgb(0.24, 0.40, 0.50)),
                ))
                .with_children(|book| {
                    book.spawn(Node {
                        width: percent(100),
                        flex_direction: FlexDirection::Row,
                        align_items: AlignItems::Center,
                        justify_content: JustifyContent::SpaceBetween,
                        ..default()
                    })
                    .with_children(|header| {
                        header.spawn(row()).with_children(|title| {
                            title.spawn(icon_image(icons, BookIcon::Book, 30.0, BOOK_ACCENT));
                            title.spawn(heading("CHEMISTRY FIELD MANUAL"));
                            title.spawn(label("LAB COPY", 12.0, TEXT_DIM));
                        });
                        header.spawn(button(
                            if at_machine {
                                "‹ Back to machine"
                            } else {
                                "‹ Back to lab"
                            },
                            PanelAction::CloseBook,
                        ));
                    });
                    book.spawn((
                        Node {
                            width: percent(100),
                            flex_direction: FlexDirection::Row,
                            flex_wrap: FlexWrap::Wrap,
                            align_items: AlignItems::Center,
                            column_gap: px(6),
                            row_gap: px(5),
                            padding: UiRect::axes(px(8), px(6)),
                            border_radius: BorderRadius::all(px(6)),
                            ..default()
                        },
                        BackgroundColor(BOOK_PAPER),
                    ))
                    .with_children(|strip| {
                        fact_chip(
                            strip,
                            icons,
                            BookIcon::Book,
                            career_stage.label(),
                            "Career stage",
                            career_stage.expectation(),
                            BOOK_ACCENT,
                        );
                        fact_chip(
                            strip,
                            icons,
                            BookIcon::Orders,
                            successes.to_string(),
                            "Successful orders",
                            "Completed deliveries advance the lab's career expectations.",
                            GOOD_TEXT,
                        );
                        fact_chip(
                            strip,
                            icons,
                            BookIcon::Recorded,
                            format!(
                                "{} / {}",
                                knowledge.known_count(),
                                db.reactions.recipe_count()
                            ),
                            "Methods recorded",
                            "Complete methods currently written in this career's notebook.",
                            GOOD_TEXT,
                        );
                        fact_chip(
                            strip,
                            icons,
                            BookIcon::Research,
                            knowledge.research_points.to_string(),
                            "Research",
                            "Spend research on a locked method to reveal its next authored hint.",
                            Color::srgb(0.76, 0.68, 0.96),
                        );
                        fact_chip(
                            strip,
                            icons,
                            BookIcon::Key,
                            "B / ESC",
                            "Close manual",
                            if at_machine {
                                "Return to the machine panel without releasing your claim."
                            } else {
                                "Close the manual and return to the lab."
                            },
                            TEXT_DIM,
                        );
                    });
                    let stage_progress = career_stage
                        .next_success_threshold()
                        .map(|next| format!(" Next stage at {next} successful orders."))
                        .unwrap_or_default();
                    book.spawn(label(
                        format!(
                            "CURRENT FOCUS  /  {}.{stage_progress}",
                            career_stage.expectation()
                        ),
                        12.0,
                        Color::srgb(0.70, 0.81, 0.96),
                    ));

                    let progress = RecipeProgress::new(db, knowledge);
                    book.spawn(Node {
                        column_gap: px(16),
                        align_items: AlignItems::Start,
                        flex_grow: 1.0,
                        ..default()
                    })
                    .with_children(|columns| match view.open_recipe {
                        Some(id) => spawn_recipe_tree(
                            columns,
                            db,
                            knowledge,
                            &progress,
                            db.reactions.get(id),
                            icons,
                        ),
                        None => {
                            book_sidebar(columns, db, knowledge, view.category, icons);
                            book_entries(columns, db, knowledge, &progress, view, icons);
                        }
                    });
                });
        });
}

/// The column of headings, each with how much of it the chemist has recorded.
fn book_sidebar(
    columns: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    selected: Option<Category>,
    icons: &BookIconAssets,
) {
    columns
        .spawn((
            Node {
                width: px(154),
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                align_content: AlignContent::FlexStart,
                column_gap: px(4),
                row_gap: px(4),
                padding: UiRect::all(px(3)),
                flex_shrink: 0.0,
                border_radius: BorderRadius::all(px(7)),
                ..default()
            },
            BackgroundColor(BOOK_INSET),
        ))
        .with_children(|sidebar| {
            // "All" first, and it is what a fresh book opens on.
            let tabs = std::iter::once(None).chain(Category::ALL.map(Some));
            for tab in tabs {
                let (known, total) = category_counts(db, knowledge, tab);
                let name = match tab {
                    Some(category) => category.label(),
                    None => "All recipes",
                };
                let description = tab
                    .map(Category::blurb)
                    .unwrap_or("Every recorded and discoverable method in the manual.");
                let mut entity = sidebar.spawn(icon_control(
                    PanelAction::ShowCategory(tab),
                    name,
                    description,
                    72.0,
                    52.0,
                ));
                entity.with_children(|control| {
                    control.spawn(icon_image(
                        icons,
                        BookIcon::category(tab),
                        25.0,
                        if tab == selected {
                            BOOK_ACCENT
                        } else {
                            TEXT_DIM
                        },
                    ));
                    control.spawn(label(format!("{known}/{total}"), 12.0, TEXT));
                });
                // Same marker the dispense-amount row uses, so `button_feedback`
                // colours the open tab with no extra code.
                if tab == selected {
                    entity.insert((Selected, BackgroundColor(BUTTON_ACTIVE)));
                }
            }
        });
}

const BOOK_PAGE_SIZE: usize = 6;

/// Everything needed to classify the notebook without re-walking the reaction
/// graph once for every badge. `ready_products` are one successful experiment
/// away; permitting those as inputs defines the next visible frontier.
struct RecipeProgress {
    available: HashSet<ReagentId>,
    ready_products: HashSet<ReagentId>,
}

impl RecipeProgress {
    fn new(db: &ChemDb, knowledge: &Knowledge) -> Self {
        let available = knowledge.available_reagents(db);
        let ready_products = db
            .reactions
            .iter()
            .filter(|reaction| !knowledge.is_known(reaction.id))
            .filter(|reaction| reaction_inputs_are_in(reaction, &available))
            .flat_map(|reaction| reaction.product_ids())
            .collect();
        Self {
            available,
            ready_products,
        }
    }

    fn state(&self, knowledge: &Knowledge, reaction: &chem_sim::Reaction) -> RecipeState {
        if knowledge.is_known(reaction.id) {
            return RecipeState::Recorded;
        }
        if reaction_inputs_are_in(reaction, &self.available) {
            return RecipeState::Ready;
        }
        if reaction
            .reactants
            .iter()
            .chain(reaction.catalysts.iter())
            .all(|(id, _)| self.available.contains(id) || self.ready_products.contains(id))
        {
            return RecipeState::Frontier;
        }
        RecipeState::Locked
    }

    /// The smallest useful step from the current notebook: something ready to
    /// discover first, otherwise something one precursor beyond it. This is a
    /// recommendation, not a lock; every card remains browsable.
    fn recommendation<'a>(
        &self,
        db: &'a ChemDb,
        knowledge: &Knowledge,
    ) -> Option<(RecipeState, &'a chem_sim::Reaction)> {
        db.reactions
            .iter()
            .filter_map(|reaction| {
                let state = self.state(knowledge, reaction);
                matches!(state, RecipeState::Ready | RecipeState::Frontier)
                    .then_some((state, reaction))
            })
            .min_by_key(|(state, reaction)| {
                (
                    *state,
                    reaction.reactants.len() + reaction.catalysts.len(),
                    product_name(db, reaction.id),
                )
            })
    }
}

fn reaction_inputs_are_in(reaction: &chem_sim::Reaction, reagents: &HashSet<ReagentId>) -> bool {
    reaction
        .reactants
        .iter()
        .chain(reaction.catalysts.iter())
        .all(|(id, _)| reagents.contains(id))
}

fn filter_matches(filter: BookFilter, state: RecipeState) -> bool {
    matches!(filter, BookFilter::All)
        || matches!(
            (filter, state),
            (BookFilter::Recorded, RecipeState::Recorded)
                | (BookFilter::Ready, RecipeState::Ready)
                | (BookFilter::Frontier, RecipeState::Frontier)
                | (BookFilter::Locked, RecipeState::Locked)
        )
}

/// The browse screen: category navigation stays fixed at the left while the
/// right side holds state filters, a bounded page of large cards and paging.
fn book_entries(
    columns: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    progress: &RecipeProgress,
    view: &BookView,
    icons: &BookIconAssets,
) {
    let (title, blurb) = view
        .category
        .map(|category| (category.label(), category.blurb()))
        .unwrap_or((
            "All chemistry",
            "Browse every recorded and discoverable method.",
        ));
    let all = recipes_in(db, knowledge, view.category);
    let state_count = |wanted| {
        all.iter()
            .filter(|reaction| progress.state(knowledge, reaction) == wanted)
            .count()
    };
    let recorded = state_count(RecipeState::Recorded);
    let ready = state_count(RecipeState::Ready);
    let frontier = state_count(RecipeState::Frontier);
    let locked = state_count(RecipeState::Locked);
    let mut visible: Vec<_> = all
        .into_iter()
        .filter(|reaction| filter_matches(view.filter, progress.state(knowledge, reaction)))
        .collect();
    visible.sort_by_key(|reaction| {
        (
            progress.state(knowledge, reaction),
            crate::knowledge::product_name(db, reaction.id),
        )
    });

    let (page, page_count, first, last) = book_page_window(visible.len(), view.page);

    columns
        .spawn((
            Node {
                flex_grow: 1.0,
                flex_direction: FlexDirection::Column,
                row_gap: px(8),
                max_height: vh(69),
                overflow: Overflow::scroll_y(),
                ..default()
            },
            ScrollPosition::default(),
            ScrollPane,
        ))
        .with_children(|pane| {
            pane.spawn(row()).with_children(|heading_row| {
                heading_row
                    .spawn(icon_badge(title, blurb, 36.0))
                    .with_children(|badge| {
                        badge.spawn(icon_image(
                            icons,
                            BookIcon::category(view.category),
                            23.0,
                            BOOK_ACCENT,
                        ));
                    });
                heading_row.spawn(label(title, 20.0, TEXT));
            });
            pane.spawn(label(blurb, 14.0, TEXT_DIM));

            if let Some((state, reaction)) = progress.recommendation(db, knowledge) {
                let guidance = match state {
                    RecipeState::Ready => "all inputs are obtainable now",
                    RecipeState::Frontier => "one precursor discovery away",
                    _ => unreachable!("recommendations only use actionable states"),
                };
                pane.spawn((
                    Node {
                        width: percent(100),
                        padding: UiRect::axes(px(10), px(7)),
                        align_items: AlignItems::Center,
                        column_gap: px(8),
                        border: UiRect::left(px(3)),
                        border_radius: BorderRadius::all(px(5)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.11, 0.20, 0.27, 0.88)),
                    BorderColor::from(state.color()),
                ))
                .with_children(|next| {
                    next.spawn(icon_image(icons, state.icon(), 24.0, state.color()));
                    next.spawn(label("NEXT EXPERIMENT", 13.0, state.color()));
                    next.spawn(label(product_name(db, reaction.id), 15.0, TEXT));
                    next.spawn(label(guidance, 12.0, TEXT_DIM));
                });
            }

            pane.spawn(row()).with_children(|filters| {
                for filter in BookFilter::ALL {
                    let count = match filter {
                        BookFilter::All => recorded + ready + frontier + locked,
                        BookFilter::Recorded => recorded,
                        BookFilter::Ready => ready,
                        BookFilter::Frontier => frontier,
                        BookFilter::Locked => locked,
                    };
                    let mut entity = filters.spawn(icon_control(
                        PanelAction::ShowBookFilter(filter),
                        filter.label(),
                        filter.explanation(),
                        62.0,
                        62.0,
                    ));
                    entity.with_children(|control| {
                        control.spawn(icon_image(
                            icons,
                            filter.icon(),
                            22.0,
                            if filter == view.filter {
                                filter_color(filter)
                            } else {
                                TEXT_DIM
                            },
                        ));
                        control.spawn(label(count.to_string(), 12.0, TEXT));
                    });
                    if filter == view.filter {
                        entity.insert((Selected, BackgroundColor(BUTTON_ACTIVE)));
                    }
                }
            });

            if visible.is_empty() {
                pane.spawn(label(
                    "No methods match this view. Try another status or category.",
                    14.0,
                    TEXT_DIM,
                ));
                return;
            }

            pane.spawn(Node {
                flex_direction: FlexDirection::Row,
                flex_wrap: FlexWrap::Wrap,
                align_content: AlignContent::FlexStart,
                column_gap: px(10),
                row_gap: px(10),
                ..default()
            })
            .with_children(|cards| {
                for reaction in &visible[first..last] {
                    book_entry(cards, db, knowledge, progress, reaction, icons);
                }
            });

            pane.spawn(row()).with_children(|pager| {
                if page > 0 {
                    pager.spawn(button(
                        "‹ Previous page",
                        PanelAction::SetBookPage(page - 1),
                    ));
                }
                pager.spawn(label(
                    format!(
                        "Page {} of {}   ·   showing {}–{} of {} methods",
                        page + 1,
                        page_count,
                        first + 1,
                        last,
                        visible.len()
                    ),
                    13.0,
                    TEXT_DIM,
                ));
                if page + 1 < page_count {
                    pager.spawn(button("Next page ›", PanelAction::SetBookPage(page + 1)));
                }
            });
        });
}

fn book_page_window(total: usize, requested_page: usize) -> (usize, usize, usize, usize) {
    let page_count = total.div_ceil(BOOK_PAGE_SIZE).max(1);
    let page = requested_page.min(page_count - 1);
    let first = page * BOOK_PAGE_SIZE;
    let last = (first + BOOK_PAGE_SIZE).min(total);
    (page, page_count, first, last)
}

/// One method card. It carries only the information needed to choose a recipe;
/// the exact formula and full effect audit belong on the detail screen.
fn book_entry(
    pane: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    progress: &RecipeProgress,
    reaction: &chem_sim::Reaction,
    icons: &BookIconAssets,
) {
    let state = progress.state(knowledge, reaction);
    let title = product_name(db, reaction.id);
    let product = reaction
        .products
        .first()
        .map(|(id, _)| db.reagents.get(*id));
    let presentation = RecipePresentation::new(reaction, product);
    let mut card = section();
    card.width = percent(48.5);
    card.min_height = px(148);
    card.border = UiRect::all(px(1));

    pane.spawn((
        Button,
        card,
        BackgroundColor(SECTION_BG),
        BorderColor::all(Color::srgba(0.25, 0.31, 0.38, 0.78)),
        accessibility_label(format!("Open {title}"), Role::Button),
        PanelAction::OpenRecipe(reaction.id),
    ))
    .with_children(|entry| {
        entry.spawn(row()).with_children(|top| {
            status_seal(top, icons, state);
            if let Some(product) = product {
                let [r, g, b] = product.color;
                top.spawn(swatch_chip(Color::srgb(r, g, b)));
            }
            top.spawn(label(title, 18.0, TEXT));
        });

        entry.spawn(wrap_row()).with_children(|categories| {
            for category in reaction_categories(db, reaction.id) {
                categories
                    .spawn(icon_badge(category.label(), category.blurb(), 30.0))
                    .with_children(|badge| {
                        badge.spawn(icon_image(
                            icons,
                            BookIcon::category(Some(*category)),
                            17.0,
                            BOOK_ACCENT,
                        ));
                    });
            }
        });

        if let Some(treats) = product.and_then(|p| p.treats.as_ref()) {
            entry.spawn(label(treats.clone(), 13.0, TEXT_DIM));
        }

        entry.spawn(wrap_row()).with_children(|facts| {
            fact_chip(
                facts,
                icons,
                BookIcon::Inputs,
                presentation.input_count.to_string(),
                "Inputs",
                "Number of reactants and catalysts required by this method.",
                TEXT_DIM,
            );
            fact_chip(
                facts,
                icons,
                reaction_process_icon(reaction),
                reaction_process_label(reaction),
                "Workstation",
                preparation_line(db, reaction),
                BOOK_ACCENT,
            );
            if presentation.catalyst_count > 0 {
                fact_chip(
                    facts,
                    icons,
                    BookIcon::Catalyst,
                    presentation.catalyst_count.to_string(),
                    "Catalyst",
                    "Required for the reaction but not consumed by it.",
                    Color::srgb(0.82, 0.70, 0.38),
                );
            }
            if reaction.min_temp.is_some() || reaction.max_temp.is_some() {
                fact_chip(
                    facts,
                    icons,
                    BookIcon::Temperature,
                    presentation.temperature.clone(),
                    "Temperature",
                    "The valid reaction temperature envelope. Exact limits remain printed here.",
                    Color::srgb(0.92, 0.52, 0.34),
                );
            }
            if let Some(ph) = &presentation.ph {
                fact_chip(
                    facts,
                    icons,
                    BookIcon::Ph,
                    ph.clone(),
                    "pH window",
                    "The batch must remain inside this acidity range.",
                    Color::srgb(0.64, 0.76, 0.96),
                );
            }
            if let Some(minimum) = &presentation.minimum_purity {
                fact_chip(
                    facts,
                    icons,
                    BookIcon::Purity,
                    minimum.clone(),
                    "Minimum purity",
                    "Consumed inputs below this purity prevent the method from starting.",
                    GOOD_TEXT,
                );
            }
            if let (Some(threshold), Some(overheat)) =
                (reaction.overheat_temp, presentation.overheat.as_ref())
            {
                fact_chip(
                    facts,
                    icons,
                    BookIcon::Explosive,
                    format!(">{threshold}"),
                    "Overheat hazard",
                    overheat.clone(),
                    Color::srgb(0.94, 0.38, 0.28),
                );
            }
            if let Some(profile) = &presentation.profile {
                if profile.controlled {
                    fact_chip(
                        facts,
                        icons,
                        BookIcon::Controlled,
                        "controlled",
                        "Controlled substance",
                        "Station policy treats this product as controlled material.",
                        Color::srgb(0.86, 0.60, 0.38),
                    );
                }
                if profile.explosive {
                    fact_chip(
                        facts,
                        icons,
                        BookIcon::Explosive,
                        "energetic",
                        "Energetic product",
                        "The product carries its own temperature-triggered explosive profile.",
                        Color::srgb(0.94, 0.38, 0.28),
                    );
                }
            }
        });
    });
}

fn temperature_value(reaction: &chem_sim::Reaction) -> String {
    match (reaction.min_temp, reaction.max_temp) {
        (Some(min), Some(max)) => format!("{min}–{max}"),
        (Some(min), None) => format!("≥{min}"),
        (None, Some(max)) => format!("≤{max}"),
        (None, None) => "ambient".to_string(),
    }
}

fn ph_value(reaction: &chem_sim::Reaction) -> String {
    match (reaction.min_ph, reaction.optimal_ph, reaction.max_ph) {
        (Some(min), Some(optimum), Some(max)) => {
            format!("{min:.1}–{max:.1}  ◇{optimum:.1}")
        }
        (Some(min), _, Some(max)) => format!("{min:.1}–{max:.1}"),
        _ => "unrestricted".to_string(),
    }
}

fn overheat_explanation(reaction: &chem_sim::Reaction) -> String {
    match reaction.overheat {
        chem_sim::Overheat::ReducedYield { .. } => {
            "Crossing the printed threshold reduces output yield.".to_string()
        }
        chem_sim::Overheat::Detonate { power } => {
            format!("Crossing the printed threshold detonates the batch at power {power:.1}.")
        }
        chem_sim::Overheat::Ruin => {
            "Crossing the printed threshold ruins the entire batch.".to_string()
        }
    }
}

fn recipe_complexity_line(reaction: &chem_sim::Reaction) -> String {
    let mut traits = vec![format!(
        "{} input{}",
        reaction.reactants.len() + reaction.catalysts.len(),
        if reaction.reactants.len() + reaction.catalysts.len() == 1 {
            ""
        } else {
            "s"
        }
    )];
    traits.push(match reaction.process {
        chem_sim::ReactionProcess::Ambient => {
            if reaction.min_temp.is_some() || reaction.max_temp.is_some() {
                "temperature-controlled".to_string()
            } else {
                "direct mixture".to_string()
            }
        }
        chem_sim::ReactionProcess::Agitated { .. } => "staged mixing".to_string(),
    });
    if !reaction.catalysts.is_empty() {
        traits.push("catalyst".to_string());
    }
    if reaction.min_ph.is_some() || reaction.max_ph.is_some() {
        traits.push("pH-sensitive".to_string());
    }
    if reaction.min_purity.is_some() {
        traits.push("purity-sensitive".to_string());
    }
    traits.join("  ·  ")
}

/// How many levels of "what feeds this" the tree draws before giving up and
/// saying so. Current data's deepest chain (arithrazine ← hyronalin ←
/// dylovene) is 2; this leaves generous headroom without being unbounded.
const MAX_TREE_DEPTH: usize = 8;
/// Horizontal shift per nesting level.
const TREE_INDENT: f32 = 22.0;

fn section_header(
    parent: &mut ChildSpawnerCommands,
    icons: &BookIconAssets,
    icon: BookIcon,
    title: &'static str,
    body: &'static str,
) {
    parent.spawn(row()).with_children(|header| {
        header
            .spawn(icon_badge(title, body, 34.0))
            .with_children(|badge| {
                badge.spawn(icon_image(icons, icon, 20.0, BOOK_ACCENT));
            });
        header.spawn(label(title, 13.0, Color::srgb(0.60, 0.74, 0.92)));
    });
}

fn reagent_token(
    parent: &mut ChildSpawnerCommands,
    db: &ChemDb,
    reagent: ReagentId,
    amount: Units,
) {
    let definition = db.reagents.get(reagent);
    let [r, g, b] = definition.color;
    parent
        .spawn((
            Node {
                min_height: px(36),
                padding: UiRect::axes(px(8), px(5)),
                align_items: AlignItems::Center,
                column_gap: px(6),
                border: UiRect::all(px(1)),
                border_radius: BorderRadius::all(px(5)),
                ..default()
            },
            BackgroundColor(BOOK_INSET),
            BorderColor::all(Color::srgb(
                0.28 + r * 0.42,
                0.28 + g * 0.42,
                0.28 + b * 0.42,
            )),
        ))
        .with_children(|token| {
            token.spawn(swatch_chip(Color::srgb(r, g, b)));
            token.spawn(label(amount.to_string(), 16.0, TEXT));
            token.spawn(label(definition.name.clone(), 13.0, TEXT));
        });
}

fn formula_strip(
    parent: &mut ChildSpawnerCommands,
    db: &ChemDb,
    reaction: &chem_sim::Reaction,
    icons: &BookIconAssets,
) {
    card(parent, "FORMULA", |formula| {
        formula.spawn(wrap_row()).with_children(|line| {
            for (index, &(reagent, amount)) in reaction.reactants.iter().enumerate() {
                if index > 0 {
                    line.spawn(label("+", 18.0, TEXT_DIM));
                }
                reagent_token(line, db, reagent, amount);
            }
            line.spawn(label("→", 24.0, BOOK_ACCENT));
            for (index, &(reagent, amount)) in reaction.products.iter().enumerate() {
                if index > 0 {
                    line.spawn(label("+", 18.0, TEXT_DIM));
                }
                reagent_token(line, db, reagent, amount);
            }
        });
        if !reaction.catalysts.is_empty() {
            formula.spawn(row()).with_children(|catalysts| {
                catalysts
                    .spawn(icon_badge(
                        "Catalyst",
                        "Required for the reaction but not consumed by it.",
                        34.0,
                    ))
                    .with_children(|badge| {
                        badge.spawn(icon_image(
                            icons,
                            BookIcon::Catalyst,
                            19.0,
                            Color::srgb(0.82, 0.70, 0.38),
                        ));
                    });
                catalysts.spawn(label("NOT CONSUMED", 12.0, Color::srgb(0.82, 0.70, 0.38)));
                for &(reagent, amount) in &reaction.catalysts {
                    reagent_token(catalysts, db, reagent, amount);
                }
            });
        }
    });
}

fn process_dashboard(
    parent: &mut ChildSpawnerCommands,
    db: &ChemDb,
    reaction: &chem_sim::Reaction,
    icons: &BookIconAssets,
) {
    let presentation = RecipePresentation::new(reaction, None);
    card(parent, "PROCESS & LIMITS", |process| {
        process.spawn(wrap_row()).with_children(|facts| {
            fact_chip(
                facts,
                icons,
                reaction_process_icon(reaction),
                reaction_process_label(reaction),
                "Workstation",
                preparation_line(db, reaction),
                BOOK_ACCENT,
            );
            fact_chip(
                facts,
                icons,
                match reaction.process {
                    chem_sim::ReactionProcess::Agitated { .. } => BookIcon::Agitate,
                    chem_sim::ReactionProcess::Ambient => BookIcon::DirectMix,
                },
                match reaction.process {
                    chem_sim::ReactionProcess::Agitated { .. } => "staged agitation",
                    chem_sim::ReactionProcess::Ambient => "combine",
                },
                "Procedure",
                preparation_line(db, reaction),
                Color::srgb(0.66, 0.80, 0.93),
            );
            fact_chip(
                facts,
                icons,
                BookIcon::Duration,
                presentation.processing.clone(),
                "Processing time",
                "Instant methods resolve on contact; timed methods consume reaction units each second.",
                Color::srgb(0.76, 0.68, 0.96),
            );
            fact_chip(
                facts,
                icons,
                BookIcon::Temperature,
                presentation.temperature.clone(),
                "Temperature",
                "Valid reaction temperature. Overheat limits are shown separately.",
                Color::srgb(0.92, 0.52, 0.34),
            );
            if let Some(ph) = &presentation.ph {
                fact_chip(
                    facts,
                    icons,
                    BookIcon::Ph,
                    ph.clone(),
                    "pH window",
                    "Range and optimum acidity for this method.",
                    Color::srgb(0.64, 0.76, 0.96),
                );
            }
            if let Some(minimum) = &presentation.minimum_purity {
                fact_chip(
                    facts,
                    icons,
                    BookIcon::Purity,
                    minimum.clone(),
                    "Minimum purity",
                    "Consumed inputs must meet this purity before the reaction begins.",
                    GOOD_TEXT,
                );
            }
            if let (Some(threshold), Some(overheat)) =
                (reaction.overheat_temp, presentation.overheat.as_ref())
            {
                fact_chip(
                    facts,
                    icons,
                    BookIcon::Explosive,
                    format!(">{threshold}"),
                    "Overheat consequence",
                    overheat.clone(),
                    Color::srgb(0.94, 0.38, 0.28),
                );
            }
        });

        if let chem_sim::ReactionProcess::Agitated { side_a, side_b } = &reaction.process {
            process.spawn(label("PREPARE SEPARATELY", 12.0, TEXT_DIM));
            process.spawn(wrap_row()).with_children(|sides| {
                sides.spawn(label("A", 13.0, BOOK_ACCENT));
                for &(reagent, amount) in side_a {
                    reagent_token(sides, db, reagent, amount);
                }
                sides.spawn(label("/  B", 13.0, BOOK_ACCENT));
                for &(reagent, amount) in side_b {
                    reagent_token(sides, db, reagent, amount);
                }
            });
        }
    });
}

fn processing_value(reaction: &chem_sim::Reaction) -> String {
    match (&reaction.process, reaction.rate) {
        (chem_sim::ReactionProcess::Agitated { .. }, Some(rate)) => {
            format!("4–8s  /  {rate}u·s⁻¹")
        }
        (_, Some(rate)) => format!("{rate}u·s⁻¹"),
        (_, None) => "instant".to_string(),
    }
}

fn profile_fact(
    parent: &mut ChildSpawnerCommands,
    icons: &BookIconAssets,
    icon: BookIcon,
    title: &'static str,
    value: String,
    body: &'static str,
    color: Color,
) {
    parent
        .spawn(icon_badge(title, body, 210.0))
        .with_children(|fact| {
            fact.spawn(icon_image(icons, icon, 22.0, color));
            fact.spawn(Node {
                flex_direction: FlexDirection::Column,
                flex_grow: 1.0,
                ..default()
            })
            .with_children(|text| {
                text.spawn(label(title, 13.0, color));
                text.spawn(label(value, 12.0, TEXT));
            });
        });
}

fn effects_dashboard(
    parent: &mut ChildSpawnerCommands,
    product: &chem_sim::Reagent,
    icons: &BookIconAssets,
) {
    let coverage = ProfileCoverage::new(product);
    card(parent, "EFFECTS & HANDLING", |effects| {
        effects.spawn(wrap_row()).with_children(|facts| {
            profile_fact(
                facts,
                icons,
                BookIcon::Ph,
                "CHEMICAL PROFILE",
                format!(
                    "pH {:.1}  /  {}",
                    coverage.ph,
                    if coverage.controlled {
                        "controlled"
                    } else {
                        "unrestricted"
                    }
                ),
                "Intrinsic acidity and station control classification.",
                BOOK_ACCENT,
            );
            if coverage.explosive {
                let explosive = product
                    .explosive
                    .expect("coverage tracks explosive profile");
                profile_fact(
                    facts,
                    icons,
                    BookIcon::Explosive,
                    "ENERGETIC HAZARD",
                    format!(
                        "{:.0} K  /  strength {:.1}  /  modifier {:.1}",
                        explosive.activation_temp.0, explosive.strength, explosive.modifier
                    ),
                    "Activation temperature, explosive strength and reagent modifier.",
                    Color::srgb(0.94, 0.38, 0.28),
                );
            }
            profile_fact(
                facts,
                icons,
                if product.effects.iter().any(|effect| {
                    matches!(
                        effect,
                        chem_sim::ReagentEffect::Heal(..)
                            | chem_sim::ReagentEffect::TopicalHeal(..)
                            | chem_sim::ReagentEffect::ConditionalHeal { .. }
                            | chem_sim::ReagentEffect::CriticalHeal(..)
                    )
                }) {
                    BookIcon::Heal
                } else {
                    BookIcon::Status
                },
                "BODILY EFFECTS",
                if coverage.bodily_effects == 0 {
                    if product.intentionally_inert {
                        "intentionally inert".to_string()
                    } else {
                        "no direct bloodstream effect".to_string()
                    }
                } else {
                    effect_list(&product.effects)
                },
                "Effects applied during each bloodstream tick.",
                GOOD_TEXT,
            );
            if coverage.targeted_purges > 0 {
                profile_fact(
                    facts,
                    icons,
                    BookIcon::Purge,
                    "TARGETED PURGE",
                    product
                        .targeted_purges
                        .iter()
                        .map(|(target, amount)| {
                            format!("{} {amount}/tick", target.replace('_', " "))
                        })
                        .collect::<Vec<_>>()
                        .join(", "),
                    "Specific bloodstream reagents removed on each tick.",
                    Color::srgb(0.55, 0.82, 0.90),
                );
            }
            profile_fact(
                facts,
                icons,
                BookIcon::Overdose,
                "OVERDOSE EFFECTS",
                if coverage.overdose_effects == 0 {
                    "none".to_string()
                } else {
                    effect_list(&product.overdose_effects)
                },
                "Effects applied while the bloodstream quantity exceeds its overdose threshold.",
                Color::srgb(0.93, 0.58, 0.36),
            );
            if coverage.critical_effects > 0 {
                profile_fact(
                    facts,
                    icons,
                    BookIcon::Critical,
                    "CRITICAL OVERDOSE",
                    effect_list(&product.critical_effects),
                    "Additional effects at the critical overdose boundary.",
                    Color::srgb(0.96, 0.34, 0.31),
                );
            }
            profile_fact(
                facts,
                icons,
                BookIcon::Aftereffect,
                "AFTEREFFECTS",
                if coverage.after_effects == 0 {
                    "none".to_string()
                } else {
                    effect_list(&product.after_effects)
                },
                "Effects applied when the reagent finally clears from the body.",
                Color::srgb(0.78, 0.68, 0.92),
            );
        });

        effects.spawn(label(
            "APPLICATION ROUTES",
            10.0,
            Color::srgb(0.60, 0.74, 0.92),
        ));
        effects.spawn(wrap_row()).with_children(|routes| {
            if !coverage.has_body_routes {
                fact_chip(
                    routes,
                    icons,
                    BookIcon::Contact,
                    "environment only",
                    "Application route",
                    "No therapeutic body route is available.",
                    TEXT_DIM,
                );
            } else {
                for (icon, value, title, body) in [
                    (
                        BookIcon::Inject,
                        "100% fast",
                        "Inject",
                        "Full dose delivered quickly.",
                    ),
                    (
                        BookIcon::Ingest,
                        "60% slow",
                        "Ingest",
                        "Reduced dose absorbed slowly.",
                    ),
                    (
                        BookIcon::Patch,
                        "100% topical",
                        "Patch",
                        "Full topical dose through a patch.",
                    ),
                    (
                        BookIcon::Spray,
                        "35% topical",
                        "Aimed spray",
                        "Partial topical dose from a directed spray.",
                    ),
                    (
                        BookIcon::Contact,
                        "15% contact",
                        "Splash / puddle",
                        "Small dose transferred by surface contact.",
                    ),
                    (
                        BookIcon::Smoke,
                        "40% direct",
                        "Smoke",
                        "Direct dose received by inhalation.",
                    ),
                ] {
                    fact_chip(routes, icons, icon, value, title, body, BOOK_ACCENT);
                }
            }
        });

        effects.spawn(label("WORLD BEHAVIOR", 13.0, Color::srgb(0.60, 0.74, 0.92)));
        effects.spawn(wrap_row()).with_children(|world| {
            if coverage.world_effects == 0 {
                fact_chip(
                    world,
                    icons,
                    BookIcon::Utility,
                    "none",
                    "World behavior",
                    "No direct environmental effect.",
                    TEXT_DIM,
                );
            } else {
                for effect in &product.world_effects {
                    let (icon, title, color) = world_effect_icon(effect);
                    fact_chip(
                        world,
                        icons,
                        icon,
                        world_effect_text(effect),
                        title,
                        "Environmental behavior when released into the station.",
                        color,
                    );
                }
            }
        });
    });
}

fn world_effect_icon(effect: &chem_sim::WorldEffect) -> (BookIcon, &'static str, Color) {
    match effect {
        chem_sim::WorldEffect::Clean { .. } => (BookIcon::Clean, "Clean", GOOD_TEXT),
        chem_sim::WorldEffect::Corrode { .. } => {
            (BookIcon::Corrode, "Corrode", Color::srgb(0.64, 0.82, 0.35))
        }
        chem_sim::WorldEffect::Ignite { .. } => {
            (BookIcon::Ignite, "Ignite", Color::srgb(0.94, 0.42, 0.25))
        }
        chem_sim::WorldEffect::ReleaseSmoke { .. } => (BookIcon::Smoke, "Release smoke", TEXT_DIM),
        chem_sim::WorldEffect::Slippery { .. } => (
            BookIcon::Slippery,
            "Slippery",
            Color::srgb(0.47, 0.76, 0.90),
        ),
        chem_sim::WorldEffect::Flammable { .. } => (
            BookIcon::Flammable,
            "Flammable",
            Color::srgb(0.94, 0.50, 0.24),
        ),
        chem_sim::WorldEffect::Chill { .. } => {
            (BookIcon::Chill, "Chill", Color::srgb(0.50, 0.76, 0.98))
        }
        chem_sim::WorldEffect::Flash { .. } => {
            (BookIcon::Flash, "Flash", Color::srgb(0.95, 0.90, 0.58))
        }
        chem_sim::WorldEffect::ExpandFoam { .. } => (BookIcon::Foam, "Expand foam", TEXT),
        chem_sim::WorldEffect::Extinguish { .. } => (
            BookIcon::Extinguish,
            "Extinguish",
            Color::srgb(0.44, 0.72, 0.96),
        ),
    }
}

/// The formula screen for one recipe: itself, then — only while known, so a
/// locked step's own ingredients stay the same spoiler the hint system
/// already withholds everywhere else — everything that feeds it, one level
/// deeper per step back through the chain.
fn spawn_recipe_tree(
    columns: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    progress: &RecipeProgress,
    root: &chem_sim::Reaction,
    icons: &BookIconAssets,
) {
    columns
        .spawn(Node {
            flex_direction: FlexDirection::Column,
            flex_grow: 1.0,
            row_gap: px(8),
            ..default()
        })
        .with_children(|screen| {
            screen.spawn(row()).with_children(|row| {
                row.spawn(button("‹ Back to list", PanelAction::CloseRecipe));
            });
            screen
                .spawn((
                    Node {
                        flex_direction: FlexDirection::Column,
                        row_gap: px(6),
                        max_height: vh(67),
                        overflow: Overflow::scroll_y(),
                        ..default()
                    },
                    ScrollPosition::default(),
                    ScrollPane,
                ))
                .with_children(|pane| {
                    let mut visited = HashSet::new();
                    render_recipe_node(pane, db, knowledge, progress, root, 0, &mut visited, icons);
                });
        });
}

/// One node in the formula tree: itself, then its ingredients one level
/// deeper if it's known.
///
/// `visited` tracks reactions open on the *current branch* only (inserted on
/// entry, removed before returning) — not the whole tree, since two branches
/// legitimately sharing an ingredient (bicaridine and tricordrazine both need
/// dylovene) must both still render it. Nothing in `chem_sim`'s types rules
/// out an actual cycle, so this must not panic if one appears; it prints a
/// line and stops instead.
fn render_recipe_node(
    pane: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    progress: &RecipeProgress,
    reaction: &chem_sim::Reaction,
    depth: usize,
    visited: &mut HashSet<ReactionId>,
    icons: &BookIconAssets,
) {
    if depth > MAX_TREE_DEPTH {
        pane.spawn(label("…chain too deep to show.", 12.0, TEXT_DIM));
        return;
    }
    if !visited.insert(reaction.id) {
        pane.spawn(label(
            "…already shown further up this branch.",
            12.0,
            TEXT_DIM,
        ));
        return;
    }

    let known = knowledge.is_known(reaction.id);
    let state = progress.state(knowledge, reaction);
    let title = product_name(db, reaction.id);
    let product = reaction
        .products
        .first()
        .map(|(id, _)| db.reagents.get(*id));

    let mut node = section();
    node.margin.left = px(depth as f32 * TREE_INDENT);
    node.border = UiRect::left(px(2.0));

    pane.spawn((
        node,
        BackgroundColor(SECTION_BG),
        BorderColor::from(TEXT_DIM),
    ))
    .with_children(|entry| {
        entry.spawn(row()).with_children(|heading| {
            status_seal(heading, icons, state);
            if let Some(product) = product {
                let [r, g, b] = product.color;
                heading.spawn(swatch_chip(Color::srgb(r, g, b)));
            }
            heading.spawn(label(
                title.clone(),
                if depth == 0 { 22.0 } else { 16.0 },
                TEXT,
            ));
            if depth > 0 {
                heading
                    .spawn(icon_badge(
                        "Dependency",
                        "This recorded method produces an ingredient required further up the chain.",
                        30.0,
                    ))
                    .with_children(|badge| {
                        badge.spawn(icon_image(
                            icons,
                            BookIcon::Dependency,
                            17.0,
                            BOOK_ACCENT,
                        ));
                    });
            }
        });

        if depth == 0 {
            if let Some(product) = product {
                let properties = product.material.description();
                if !properties.is_empty() { entry.spawn(label(properties, 13.0, TEXT_DIM)); }
                for behavior in db.reactions.iter().filter(|r| r.residue && !r.material_only && r.reactants.iter().any(|(id,_)| *id == product.id)) {
                    entry.spawn(label(format!("Reaction hazard: {}", behavior.hints.first().map_or("Leaves residue.", String::as_str)), 12.0, HPLC_IMPURITY));
                }
            }
            if let Some(treats) = product.and_then(|p| p.treats.as_ref()) {
                entry.spawn(label(treats.clone(), 15.0, TEXT_DIM));
            }
        }

        if known {
            if depth == 0 {
                formula_strip(entry, db, reaction, icons);
                process_dashboard(entry, db, reaction, icons);
                if let Some(overdose) = product.and_then(|p| p.overdose) {
                    entry.spawn(wrap_row()).with_children(|warning| {
                        fact_chip(
                            warning,
                            icons,
                            BookIcon::Overdose,
                            format!(">{overdose} / dose"),
                            "Overdose threshold",
                            "A single administered dose above this quantity activates overdose effects.",
                            Color::srgb(0.90, 0.62, 0.45),
                        );
                    });
                }
                if let Some(product) = product {
                    effects_dashboard(entry, product, icons);
                }
            } else {
                // Dependencies remain useful as a tree, but do not repeat the
                // full workstation, overdose and material profile at every
                // level. Their own detail screen is one click away from the
                // browse view when that information is needed.
                entry.spawn(label(recipe_line(db, reaction), 13.0, TEXT));
                entry.spawn(wrap_row()).with_children(|facts| {
                    fact_chip(
                        facts,
                        icons,
                        reaction_process_icon(reaction),
                        reaction_process_label(reaction),
                        "Dependency process",
                        preparation_line(db, reaction),
                        BOOK_ACCENT,
                    );
                    if !reaction.catalysts.is_empty() {
                        fact_chip(
                            facts,
                            icons,
                            BookIcon::Catalyst,
                            reaction.catalysts.len().to_string(),
                            "Catalyst",
                            "Required but not consumed.",
                            Color::srgb(0.82, 0.70, 0.38),
                        );
                    }
                });
            }
            return;
        }

        entry.spawn(label(
            state.explanation(),
            14.0,
            TEXT_DIM,
        ));
        entry.spawn(label(recipe_complexity_line(reaction), 13.0, TEXT_DIM));
        for hint in knowledge.visible_hints(db, reaction.id) {
            entry.spawn(label(
                format!("· {hint}"),
                13.0,
                Color::srgb(0.70, 0.78, 0.62),
            ));
        }
        // Only offer the purchase when it can actually go through; a button
        // that silently does nothing is worse than none.
        if knowledge.hint_available(db, reaction.id) {
            let affordable = knowledge.research_points >= HINT_COST;
            entry.spawn(row()).with_children(|row| {
                if affordable {
                    row.spawn(button(
                        format!("Study further  ({HINT_COST} research)"),
                        PanelAction::BuyHint(reaction.id),
                    ));
                } else {
                    row.spawn(label(
                        format!("Needs {HINT_COST} research to study further."),
                        12.0,
                        TEXT_DIM,
                    ));
                }
            });
        }
    });

    // A locked node's own ingredients stay hidden — the same spoiler
    // discipline the hint system already enforces everywhere else.
    if known {
        if depth == 0 {
            section_header(
                pane,
                icons,
                BookIcon::Dependency,
                "DEPENDENCY MAP",
                "Recorded precursor methods and raw materials feeding the selected formula.",
            );
        }
        for &(reagent_id, amount) in &reaction.reactants {
            render_ingredient_node(
                pane,
                db,
                knowledge,
                progress,
                reagent_id,
                amount,
                false,
                depth + 1,
                visited,
                icons,
            );
        }
        for &(reagent_id, amount) in &reaction.catalysts {
            render_ingredient_node(
                pane,
                db,
                knowledge,
                progress,
                reagent_id,
                amount,
                true,
                depth + 1,
                visited,
                icons,
            );
        }
    }

    visited.remove(&reaction.id);
}

/// One ingredient row under a known node: recurses one level deeper if
/// something produces it, otherwise a leaf naming the raw reagent and,
/// read-only, whether it's still locked at the ChemMaster 5000 — unlocking
/// still only happens there, so this is a note, not a button.
#[allow(clippy::too_many_arguments)]
fn render_ingredient_node(
    pane: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    progress: &RecipeProgress,
    reagent: ReagentId,
    amount: Units,
    catalyst: bool,
    depth: usize,
    visited: &mut HashSet<ReactionId>,
    icons: &BookIconAssets,
) {
    if let Some(producer) = db.reactions.producer_of(reagent) {
        if catalyst {
            let mut note = section();
            note.margin.left = px(depth as f32 * TREE_INDENT);
            pane.spawn(note).with_children(|entry| {
                entry.spawn(label("catalyst, not consumed:", 13.0, TEXT_DIM));
            });
        }
        render_recipe_node(
            pane, db, knowledge, progress, producer, depth, visited, icons,
        );
        return;
    }

    let definition = db.reagents.get(reagent);
    let mut line = format!("{amount} {}", definition.name);
    if catalyst {
        line.push_str("   (catalyst, not consumed)");
    }

    let mut node = section();
    node.margin.left = px(depth as f32 * TREE_INDENT);
    node.border = UiRect::left(px(2.0));

    pane.spawn((
        node,
        BackgroundColor(SECTION_BG),
        BorderColor::from(TEXT_DIM),
    ))
    .with_children(|entry| {
        entry.spawn(row()).with_children(|ingredient| {
            ingredient
                .spawn(icon_badge(
                    if catalyst {
                        "Raw catalyst"
                    } else {
                        "Raw reagent"
                    },
                    if catalyst {
                        "A base material required by this dependency chain and not consumed."
                    } else {
                        "A base material with no recorded precursor method."
                    },
                    32.0,
                ))
                .with_children(|badge| {
                    badge.spawn(icon_image(
                        icons,
                        if catalyst {
                            BookIcon::Catalyst
                        } else {
                            BookIcon::RawReagent
                        },
                        18.0,
                        if catalyst {
                            Color::srgb(0.82, 0.70, 0.38)
                        } else {
                            TEXT_DIM
                        },
                    ));
                });
            ingredient.spawn(label(line, 14.0, TEXT));
        });
        if definition.dispensable && !knowledge.is_reagent_unlocked(db, reagent) {
            entry.spawn(row()).with_children(|locked| {
                locked.spawn(icon_image(
                    icons,
                    BookIcon::Locked,
                    16.0,
                    Color::srgb(0.80, 0.60, 0.45),
                ));
                locked.spawn(label(
                    format!("locked at ChemMaster 5000  /  tier {}", definition.tier),
                    12.0,
                    Color::srgb(0.80, 0.60, 0.45),
                ));
            });
        }
    });
}

/// The recipes filed under a heading, or every recipe when none is chosen.
///
/// Recorded ones come first, then alphabetically: what you can actually make
/// belongs at the top of the page, and within that the list has to be somewhere
/// findable rather than in data-file order.
fn recipes_in<'a>(
    db: &'a ChemDb,
    knowledge: &Knowledge,
    category: Option<Category>,
) -> Vec<&'a chem_sim::Reaction> {
    let mut recipes: Vec<&chem_sim::Reaction> = db
        .reactions
        .iter()
        .filter(|reaction| !reaction.residue)
        .filter(|reaction| match category {
            Some(category) => reaction_categories(db, reaction.id).contains(&category),
            None => true,
        })
        .collect();
    recipes.sort_by_key(|reaction| {
        (
            !knowledge.is_known(reaction.id),
            crate::knowledge::product_name(db, reaction.id),
        )
    });
    recipes
}

/// How much of a heading the chemist has written up, for the sidebar.
fn category_counts(
    db: &ChemDb,
    knowledge: &Knowledge,
    category: Option<Category>,
) -> (usize, usize) {
    let recipes = recipes_in(db, knowledge, category);
    let known = recipes
        .iter()
        .filter(|reaction| knowledge.is_known(reaction.id))
        .count();
    (known, recipes.len())
}

/// Marks whichever scrollable region the currently-open panel has — the
/// reference book's entry list or formula tree, or the standing board's
/// radio-history-plus-shop pane. Shared rather than one marker per panel:
/// `sync_panel` despawns the whole `PanelRoot` subtree before rebuilding it,
/// and only ever shows the book *or* one machine panel at a time, so there is
/// never more than one scrollable region alive at once for this to be
/// ambiguous about.
///
/// `pub(crate)`: `settings::settings_body`/`controls_body` reuse it for the
/// same reason a machine panel does — the pre-game Settings/Controls screens
/// and the pause-reached ones both got dense enough to outgrow a bare window,
/// and this is the one scroll idiom the UI already has. Still never more than
/// one alive at once: the pause overlay only exists while `Paused` is true,
/// which requires roaming to be false, so it can never coexist with a machine
/// panel's own pane; nothing spawns one during `AppState::MainMenu` otherwise.
#[derive(Component)]
pub(crate) struct ScrollPane;

/// Mouse wheel scrolls whichever panel is open.
///
/// Written straight onto the one scrollable node rather than through Bevy's
/// pointer-hover scroll events: the cursor is grabbed and invisible while a
/// panel is open, so there is no hover target to route through, and only one
/// [`ScrollPane`] ever exists at a time for this to be ambiguous about.
fn scroll_active_pane(
    mut wheel: MessageReader<bevy::input::mouse::MouseWheel>,
    windows: Query<&Window, With<bevy::window::PrimaryWindow>>,
    mut book: Query<
        (
            Entity,
            &mut ScrollPosition,
            &ComputedNode,
            &UiGlobalTransform,
        ),
        With<ScrollPane>,
    >,
) {
    let scrolled: f32 = wheel
        .read()
        .map(|event| match event.unit {
            bevy::input::mouse::MouseScrollUnit::Line => event.y * 24.0,
            bevy::input::mouse::MouseScrollUnit::Pixel => event.y,
        })
        .sum();
    if scrolled == 0.0 {
        return;
    }

    let cursor = windows.iter().next().and_then(Window::cursor_position);
    // A board contains a radio pane inside its page. Wheel input goes to the
    // smallest pane under the pointer, so scrolling the radio does not also
    // move the surrounding service controls.
    let hovered = cursor.and_then(|cursor| {
        book.iter()
            .filter(|(_, _, node, transform)| {
                node.normalize_point(**transform, cursor)
                    .is_some_and(|p| p.x.abs() <= 0.5 && p.y.abs() <= 0.5)
            })
            .min_by(|a, b| (a.2.size().x * a.2.size().y).total_cmp(&(b.2.size().x * b.2.size().y)))
            .map(|(entity, ..)| entity)
    });
    let only = (book.iter().count() == 1).then(|| book.iter().next().unwrap().0);
    for (entity, mut position, computed, _) in &mut book {
        if Some(entity) != hovered.or(only) {
            continue;
        }
        // Content taller than the box is exactly how far it can travel.
        let limit = (computed.content_size().y - computed.size().y).max(0.0);
        position.y = (position.y - scrolled).clamp(0.0, limit);
    }
}

/// Renders a recipe as `1 Oxygen + 1 Carbon + 1 Sugar  →  3 Inaprovaline`.
/// Workstation and provenance instructions for a known recipe.
fn preparation_line(db: &ChemDb, reaction: &chem_sim::Reaction) -> String {
    let ingredients = |side: &[(ReagentId, Units)]| {
        side.iter()
            .map(|(id, amount)| format!("{amount} {}", db.reagents.get(*id).name))
            .collect::<Vec<_>>()
            .join(" + ")
    };
    match &reaction.process {
        chem_sim::ReactionProcess::Ambient => {
            if reaction.min_temp.is_some() || reaction.max_temp.is_some() {
                "Workstation: Reaction Chamber; ordinary container mixing is allowed once the temperature is valid."
                    .to_string()
            } else {
                "Workstation: ordinary container or ChemMaster 5000; combines on contact.".to_string()
            }
        }
        chem_sim::ReactionProcess::Agitated { side_a, side_b } => format!(
            "Workstation: Mixing Chamber. Prepare separately: [{}]  /  [{}], then agitate either direction.",
            ingredients(side_a),
            ingredients(side_b)
        ),
    }
}

/// Temperature envelope and player-facing duration for a known recipe.
#[cfg(test)]
fn condition_line(reaction: &chem_sim::Reaction) -> String {
    let temperature = match (reaction.min_temp, reaction.max_temp) {
        (Some(min), Some(max)) => format!("Temperature: {min} to {max}"),
        (Some(min), None) => format!("Temperature: at least {min}"),
        (None, Some(max)) => format!("Temperature: no more than {max}"),
        (None, None) => "Temperature: ambient is fine".to_string(),
    };
    let processing = match (&reaction.process, reaction.rate) {
        (chem_sim::ReactionProcess::Agitated { .. }, Some(rate)) => {
            format!("Agitation: typically 4–8s for an order batch ({rate} reaction-u/s)")
        }
        (_, Some(rate)) => format!("Processing: timed at {rate} reaction-u/s"),
        (_, None) => "Processing: instant".to_string(),
    };
    let overheat = reaction
        .overheat_temp
        .map(|threshold| match reaction.overheat {
            chem_sim::Overheat::ReducedYield { .. } => {
                format!("; yield degrades above {threshold}")
            }
            chem_sim::Overheat::Detonate { power } => {
                format!("; detonates above {threshold} (power {power:.1})")
            }
            chem_sim::Overheat::Ruin => format!("; batch is ruined above {threshold}"),
        })
        .unwrap_or_default();
    let quality = match (reaction.min_ph, reaction.optimal_ph, reaction.max_ph) {
        (Some(min), Some(optimum), Some(max)) => {
            format!("  pH: {min:.1}–{max:.1}, optimum {optimum:.1}.")
        }
        (Some(min), _, Some(max)) => format!("  pH: {min:.1}–{max:.1}."),
        _ => String::new(),
    };
    let purity = reaction
        .min_purity
        .map(|minimum| format!("  Minimum purity: {:.0}%.", minimum * 100.0))
        .unwrap_or_default();
    format!("{temperature}{overheat}.  {processing}.{quality}{purity}")
}

/// Body, crash, route and station behavior for the product of a known recipe.
#[cfg(test)]
fn reagent_profile_lines(reagent: &chem_sim::Reagent) -> Vec<String> {
    let body = effect_list(&reagent.effects);
    let overdose = effect_list(&reagent.overdose_effects);
    let critical = effect_list(&reagent.critical_effects);
    let after = effect_list(&reagent.after_effects);
    let world = reagent
        .world_effects
        .iter()
        .map(world_effect_text)
        .collect::<Vec<_>>()
        .join(", ");

    let mut lines = Vec::new();
    lines.push(format!(
        "Chemical profile: pH {:.1}, {}.",
        reagent.ph,
        if reagent.controlled {
            "controlled substance"
        } else {
            "unrestricted"
        }
    ));
    if let Some(explosive) = reagent.explosive {
        lines.push(format!(
            "Energetic hazard: activates at {:.0} K (strength {:.1}, modifier {:.1}).",
            explosive.activation_temp.0, explosive.strength, explosive.modifier
        ));
    }
    lines.push(format!(
        "Bodily effects: {}",
        if body.is_empty() {
            if reagent.intentionally_inert {
                "intentionally inert".to_string()
            } else {
                "no direct bloodstream effect".to_string()
            }
        } else {
            body
        }
    ));
    if !reagent.targeted_purges.is_empty() {
        let targets = reagent
            .targeted_purges
            .iter()
            .map(|(target, amount)| format!("{} {amount}/tick", target.replace('_', " ")))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("Targeted bloodstream purge: {targets}."));
    }
    lines.push(format!(
        "Overdose effects: {}",
        if overdose.is_empty() {
            "none"
        } else {
            &overdose
        }
    ));
    if !critical.is_empty() {
        lines.push(format!("Critical overdose: {critical}"));
    }
    lines.push(format!(
        "Aftereffects when cleared: {}",
        if after.is_empty() { "none" } else { &after }
    ));
    lines.push(if reagent.effects.is_empty() && reagent.overdose_effects.is_empty() {
        "Application routes: environmental release; no therapeutic body route."
            .to_string()
    } else {
        "Application routes: inject (full/fast), ingest (slow/60%), patch (full/topical), aimed spray (35% topical), splash or puddle contact (15%), smoke inhalation (40% direct)."
            .to_string()
    });
    lines.push(format!(
        "World behavior: {}",
        if world.is_empty() {
            "none".to_string()
        } else {
            world
        }
    ));
    lines
}

fn effect_list(effects: &[chem_sim::ReagentEffect]) -> String {
    effects
        .iter()
        .map(|effect| match effect {
            chem_sim::ReagentEffect::Heal(kind, amount) => {
                format!("heal {} {amount}/tick", kind.label().to_lowercase())
            }
            chem_sim::ReagentEffect::Harm(kind, amount) => {
                format!("deal {} {amount}/tick", kind.label().to_lowercase())
            }
            chem_sim::ReagentEffect::VolumeScaledHarm(kind, amount) => format!(
                "deal {} {amount}/tick per unit in blood",
                kind.label().to_lowercase()
            ),
            chem_sim::ReagentEffect::Contact(kind, amount) => format!(
                "{} contact damage {amount}/10u",
                kind.label().to_lowercase()
            ),
            chem_sim::ReagentEffect::TopicalHeal(kind, amount) => format!(
                "heal {} {amount}/10u topically",
                kind.label().to_lowercase()
            ),
            chem_sim::ReagentEffect::ConditionalHeal {
                required,
                kind,
                amount,
            } => format!(
                "heal {} {amount}/tick while {}",
                kind.label().to_lowercase(),
                required.label().to_lowercase()
            ),
            chem_sim::ReagentEffect::ConditionalHarm {
                existing,
                kind,
                amount,
            } => format!(
                "deal {} {amount}/tick while {} damage is already present",
                kind.label().to_lowercase(),
                existing.label().to_lowercase()
            ),
            chem_sim::ReagentEffect::CriticalHeal(kind, amount) => format!(
                "heal {} {amount}/tick while critically injured",
                kind.label().to_lowercase()
            ),
            chem_sim::ReagentEffect::Status {
                kind,
                seconds,
                intensity,
            } => format!("{} ({seconds:.0}s, x{intensity:.1})", kind.label()),
            chem_sim::ReagentEffect::DelayedStatus {
                after_ticks,
                kind,
                seconds,
                intensity,
            } => format!(
                "after {:.0}s: {} ({seconds:.0}s, x{intensity:.1})",
                *after_ticks as f32 * chem_sim::TICK_SECONDS,
                kind.label()
            ),
            chem_sim::ReagentEffect::AccumulatedHarm(kind, amount) => format!(
                "on final clearance: deal {} {amount} per exposure tick",
                kind.label().to_lowercase()
            ),
            chem_sim::ReagentEffect::DelayedHarm {
                after_ticks,
                kind,
                amount,
            } => format!(
                "after {:.0}s: deal {} {amount}/tick",
                *after_ticks as f32 * chem_sim::TICK_SECONDS,
                kind.label().to_lowercase()
            ),
            chem_sim::ReagentEffect::Counter {
                kind,
                seconds,
                intensity,
            } => format!(
                "clear {} ({seconds:.0}s, x{intensity:.1})",
                kind.label().to_lowercase()
            ),
            chem_sim::ReagentEffect::Purge(amount) => {
                format!("purge {amount}u harmful reagents/tick")
            }
            chem_sim::ReagentEffect::MedicinePurge(amount) => {
                format!("purge {amount}u medicines/tick")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn world_effect_text(effect: &chem_sim::WorldEffect) -> String {
    match effect {
        chem_sim::WorldEffect::Clean { strength } => {
            format!("cleans residue/puddles x{strength:.1}")
        }
        chem_sim::WorldEffect::Corrode { strength } => {
            format!("corrodes reactive structures x{strength:.1}")
        }
        chem_sim::WorldEffect::Ignite { intensity, seconds } => {
            format!("ignites x{intensity:.1} for {seconds:.0}s")
        }
        chem_sim::WorldEffect::ReleaseSmoke { radius, seconds } => {
            format!("vents smoke {radius:.1}m for {seconds:.0}s")
        }
        chem_sim::WorldEffect::Slippery { seconds } => {
            format!("slippery surface for {seconds:.0}s")
        }
        chem_sim::WorldEffect::Flammable { intensity, seconds } => {
            format!("flammable fuel x{intensity:.1} for {seconds:.0}s")
        }
        chem_sim::WorldEffect::Chill { kelvin_per_unit } => {
            format!("chills surface {kelvin_per_unit:.1}K/u")
        }
        chem_sim::WorldEffect::Flash { radius, seconds } => {
            format!("blinding flash {radius:.1}m for {seconds:.0}s")
        }
        chem_sim::WorldEffect::ExpandFoam {
            radius,
            seconds,
            solid,
        } => {
            let kind = if *solid {
                "solid foam barrier"
            } else {
                "chemical foam"
            };
            format!("expands as {kind} {radius:.1}m for {seconds:.0}s")
        }
        chem_sim::WorldEffect::Extinguish { radius, seconds } => {
            format!("extinguishes a {radius:.1}m area by {seconds:.0}s")
        }
    }
}

fn recipe_line(db: &ChemDb, reaction: &chem_sim::Reaction) -> String {
    let part = |pairs: &[(ReagentId, Units)]| {
        pairs
            .iter()
            .map(|(id, amount)| format!("{} {}", amount, db.reagents.get(*id).name))
            .collect::<Vec<_>>()
            .join(" + ")
    };

    let mut line = format!(
        "{}  →  {}",
        part(&reaction.reactants),
        part(&reaction.products)
    );
    if !reaction.catalysts.is_empty() {
        // Catalysts read as ingredients unless they are called out, and a
        // player who consumes their only plasma has learned the wrong lesson.
        line.push_str(&format!(
            "     (catalyst: {}, not consumed)",
            part(&reaction.catalysts)
        ));
    }
    line
}

// ---------------------------------------------------------------------------
// Order queue
// ---------------------------------------------------------------------------

#[derive(Component)]
struct OrderSlot(usize);

#[derive(Component)]
struct ShiftReadout;

/// Compact, undisclosed call-over indicators for the two waiting visitors.
#[derive(Component)]
struct PleaLine;

/// Which phase the lab is in, and what ends it.
#[derive(Component)]
struct PhaseBanner;

// The disjointness filters are noisy inline; naming them keeps the queue
// system's signature readable.
type ShiftText<'w, 's> = Single<
    'w,
    's,
    &'static mut Text,
    (
        With<ShiftReadout>,
        Without<OrderSlot>,
        Without<PleaLine>,
        Without<PhaseBanner>,
    ),
>;
type PleaText<'w, 's> = Single<
    'w,
    's,
    &'static mut Text,
    (
        With<PleaLine>,
        Without<OrderSlot>,
        Without<ShiftReadout>,
        Without<PhaseBanner>,
    ),
>;
type BannerText<'w, 's> = Single<
    'w,
    's,
    &'static mut Text,
    (
        With<PhaseBanner>,
        Without<OrderSlot>,
        Without<ShiftReadout>,
        Without<PleaLine>,
    ),
>;

/// Fixed slots, filled in each frame.
///
/// The queue shows a live countdown, so rebuilding it on change would mean
/// rebuilding every frame. Writing into pre-spawned rows keeps it to a couple
/// of string comparisons instead.
fn spawn_order_queue(mut commands: Commands, icons: Res<BookIconAssets>) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: px(16),
                right: px(16),
                width: px(350),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(12)),
                row_gap: px(8),
                border_radius: BorderRadius::all(px(6)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.035, 0.055, 0.07, 0.94)),
            crate::until_we_leave_the_lab(),
        ))
        .with_children(|queue| {
            queue.spawn((
                Text::new(""),
                TextFont::from_font_size(13.0),
                TextColor(Color::srgb(0.95, 0.88, 0.45)),
                PhaseBanner,
            ));
            queue
                .spawn(Node {
                    column_gap: px(9),
                    align_items: AlignItems::Center,
                    ..default()
                })
                .with_children(|header| {
                    header.spawn(icon_image(&icons, BookIcon::Orders, 22.0, BOOK_ACCENT));
                    header.spawn(label("ORDER DESK", 16.0, Color::srgb(0.90, 0.94, 0.96)));
                });
            for index in 0..ORDER_SLOTS {
                queue.spawn((
                    Node {
                        padding: UiRect::all(px(10)),
                        border: UiRect::left(px(2)),
                        border_radius: BorderRadius::all(px(4)),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.08, 0.115, 0.14)),
                    BorderColor::all(BOOK_ACCENT),
                    Text::new(""),
                    TextFont::from_font_size(14.0),
                    TextColor(TEXT),
                    OrderSlot(index),
                ));
            }
            queue.spawn((
                Text::new(""),
                TextFont::from_font_size(13.0),
                TextColor(Color::srgb(0.72, 0.78, 0.70)),
                PleaLine,
            ));
            queue.spawn((
                Text::new(""),
                TextFont::from_font_size(13.0),
                TextColor(TEXT_DIM),
                ShiftReadout,
            ));
        });
}

/// The one line that tells the player which shift this is and whether the lab
/// is taking requests.
///
/// Pure so all three wordings can be checked at once. There is still no phase
/// clock — the sign and the shift boundary are both the player's own, worked
/// at the standing board — but the number is what turns "another order" into
/// "shift six", which is the whole reason shifts came back.
fn accepting_banner_line(shift: &Shift) -> String {
    let state = if shift.called {
        "CLOSED OUT - debrief at the board"
    } else if shift.accepting_orders {
        "OPEN - crew are coming in"
    } else {
        "CLOSED - not accepting requests"
    };
    // A drain nobody can see is indistinguishable from a bug, and this is the
    // one line already on screen saying the lab is shut. `closure_pressure` is
    // points already taken from every department, so it reads as a running
    // total rather than a warning the player has to interpret. Zero whenever
    // the lab is open, so the open banner is untouched — see
    // `shift::impatience`.
    let souring = match shift.closure_pressure {
        0 => String::new(),
        points => format!("  |  departments souring (-{points})"),
    };
    format!("SHIFT {}  |  {state}{souring}", shift.shift_number)
}

fn update_phase_banner(
    shift: Res<Shift>,
    banner: BannerText,
    session: Option<Res<crate::session::SessionKind>>,
) {
    let line = if session.as_deref() == Some(&crate::session::SessionKind::Training) {
        "TRAINING | UNTIMED PRACTICE".to_string()
    } else {
        accepting_banner_line(&shift)
    };
    let mut banner = banner.into_inner();
    if banner.0 != line {
        banner.0 = line;
    }
}

#[allow(clippy::too_many_arguments)]
fn update_order_queue(
    db: Res<ChemDb>,
    settings: Option<Res<crate::settings::Settings>>,
    shift: Res<Shift>,
    orders: Query<(
        &CrewMember,
        &Order,
        Has<DevelopmentOrder>,
        Has<crate::security_case::OrderHold>,
    )>,
    waiting: Query<(&CrewMember, &crate::order_intake::AwaitingConversation)>,
    mut slots: Query<(&OrderSlot, &mut Text, &mut TextColor, &mut Node), Without<ShiftReadout>>,
    readout: ShiftText,
    plea_line: PleaText,
) {
    let mut accepted: Vec<_> = orders.iter().collect();
    accepted.sort_by(|a, b| {
        a.1.remaining()
            .total_cmp(&b.1.remaining())
            .then(a.0.name.cmp(&b.0.name))
    });
    for (slot, mut text, mut color, mut node) in &mut slots {
        let entry = accepted.get(slot.0);
        let display = if entry.is_some() {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
        let next = entry
            .map(|(member, order, development, held)| {
                if *held {
                    return format!(
                        "{}  |  SECURITY HOLD\n{}",
                        member.name,
                        crate::order_intake::requirements(order, &db)
                    );
                }
                if member.name == "Practice Customer" {
                    return format!(
                        "{} | UNTIMED PRACTICE\n{}",
                        member.name,
                        crate::order_intake::requirements(order, &db)
                    );
                }
                let remaining = order.remaining().ceil() as u32;
                let kind = if *development { " · OPTIONAL R&D" } else { "" };
                format!(
                    "{}{}    {}:{:02}\n{}",
                    member.name,
                    kind,
                    remaining / 60,
                    remaining % 60,
                    crate::order_intake::requirements(order, &db)
                )
            })
            .unwrap_or_default();
        if text.0 != next {
            text.0 = next;
        }
        let wanted = if entry.is_some_and(|(_, o, d, held)| !d && !held && o.remaining() < 30.0) {
            ERROR_TEXT
        } else {
            Color::srgb(0.90, 0.94, 0.96)
        };
        if color.0 != wanted {
            color.0 = wanted;
        }
    }
    let mut visitors: Vec<_> = waiting.iter().collect();
    visitors.sort_by_key(|(_, p)| p.id);
    let calls = visitors
        .iter()
        .map(|(member, p)| {
            format!(
                "{} · {} window\n{}",
                member.name,
                if member.role == "Medical" {
                    "Medical"
                } else {
                    "Public"
                },
                if p.arrived {
                    "Waiting to speak"
                } else {
                    "Coming to the window"
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut plea_line = plea_line.into_inner();
    if plea_line.0 != calls {
        plea_line.0 = calls;
    }
    let extra = accepted.len().saturating_sub(ORDER_SLOTS);
    let summary = format!(
        "{} accepted{}  |  {}: Orders\nDelivered {}   /   Botched {}",
        accepted.len(),
        if extra > 0 {
            format!(" (+{extra} more)")
        } else {
            String::new()
        },
        crate::settings::key_label(
            settings
                .as_deref()
                .map_or(KeyCode::Tab, |settings| settings.bindings.social)
        ),
        shift.succeeded,
        shift.botched
    );
    let mut readout = readout.into_inner();
    if readout.0 != summary {
        readout.0 = summary;
    }
}

// ---------------------------------------------------------------------------
// Discovery toast
// ---------------------------------------------------------------------------

#[derive(Component)]
struct Toast(Timer);

/// Something worth interrupting for.
///
/// A message rather than a direct spawn so that [`show_toasts`] is the only
/// thing that ever puts one on screen. Despawning through `Commands` is
/// deferred, so two spawners in the same frame — a recipe discovered as the
/// shift ends, or a reaction cascade discovering two at once — would each see a
/// stale "no toasts exist" and stack their cards at the same position.
#[derive(Message)]
struct ShowToast {
    kicker: &'static str,
    title: String,
    subtitle: String,
    background: Color,
}

/// Puts up at most one card per frame.
///
/// One at a time on purpose: two moments worth interrupting for that land
/// together are still one interruption, and stacked cards would cover the room.
/// The newest wins, because it is the one describing what just happened.
fn show_toasts(
    mut commands: Commands,
    mut requests: MessageReader<ShowToast>,
    existing: Query<Entity, With<Toast>>,
) {
    let Some(request) = requests.read().last() else {
        return;
    };
    for toast in &existing {
        commands.entity(toast).despawn();
    }
    spawn_toast(
        &mut commands,
        request.kicker,
        request.title.clone(),
        request.subtitle.clone(),
        request.background,
    );
}

fn spawn_toast(
    commands: &mut Commands,
    kicker: &'static str,
    title: String,
    subtitle: String,
    background: Color,
) {
    let kicker_text = kicker.to_string();
    let kicker_color = Color::srgb(0.95, 0.88, 0.45);

    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: percent(22),
                width: percent(100),
                justify_content: JustifyContent::Center,
                ..default()
            },
            Toast(Timer::from_seconds(4.5, TimerMode::Once)),
            crate::until_we_leave_the_lab(),
        ))
        .with_children(|toast| {
            toast
                .spawn((
                    Node {
                        flex_direction: FlexDirection::Column,
                        align_items: AlignItems::Center,
                        padding: UiRect::axes(px(22), px(12)),
                        row_gap: px(3),
                        border_radius: BorderRadius::all(px(6)),
                        ..default()
                    },
                    BackgroundColor(background),
                ))
                .with_children(|card| {
                    card.spawn(label(kicker_text, 12.0, kicker_color));
                    card.spawn(label(title, 20.0, TEXT));
                    card.spawn(label(subtitle, 12.0, TEXT_DIM));
                });
        });
}

/// A brief banner when a recipe is worked out.
///
/// The radio carries it too, but the radio is a slow feed you might be looking
/// away from — and discovering a recipe is the one moment that deserves to
/// interrupt.
fn announce_discoveries(
    mut discovered: MessageReader<RecipeDiscovered>,
    mut toasts: MessageWriter<ShowToast>,
) {
    for event in discovered.read() {
        toasts.write(ShowToast {
            kicker: "RECIPE RECORDED",
            title: event.name.clone(),
            subtitle: "Added to your reference book (B)".to_string(),
            background: Color::srgba(0.10, 0.20, 0.13, 0.94),
        });
    }
}

/// What the sign toast last said, so it only fires on an actual change.
///
/// A resource rather than the `Local<Option<bool>>` it was, for the reason
/// `LastPanel` is one: a `Local` cannot be reset from outside, so a session
/// that ended with the sign down left `Some(false)` behind and the *next*
/// session announced itself as "back open" on its first frame.
#[derive(Resource, Default)]
pub struct LastSignState(Option<(bool, bool)>);

/// Toasts when the sign flips or the shift is called.
///
/// Watches `Shift` from `Update` rather than the messages themselves, because
/// this runs on both ends — a joined chemist needs to notice their partner
/// flipping the sign too, and only the replicated resource reaches them, not
/// the client message that caused it.
fn announce_accepting_toggle(
    shift: Res<Shift>,
    mut announced: ResMut<LastSignState>,
    mut toasts: MessageWriter<ShowToast>,
) {
    let now = (shift.accepting_orders, shift.called);
    if announced.0 == Some(now) {
        return;
    }
    // Don't toast the very first frame — that would just announce "open" the
    // instant every session starts.
    let first_run = announced.0.is_none();
    announced.0 = Some(now);
    if first_run {
        return;
    }

    let (kicker, subtitle, background) = match now {
        // Called takes precedence over the sign: the sign is necessarily down
        // for a shift to be called at all, so reporting "closed" here would be
        // announcing the lesser half of what just happened.
        (_, true) => (
            "SHIFT OVER",
            format!(
                "Shift {} closed out. Debrief at the board.",
                shift.shift_number
            ),
            Color::srgba(0.12, 0.16, 0.24, 0.94),
        ),
        (true, false) => (
            "OPEN",
            "Taking requests again.".to_string(),
            Color::srgba(0.10, 0.20, 0.13, 0.94),
        ),
        (false, false) => (
            "CLOSED",
            "Not accepting requests for a while.".to_string(),
            Color::srgba(0.18, 0.15, 0.09, 0.94),
        ),
    };

    toasts.write(ShowToast {
        kicker,
        title: "Chemistry".to_string(),
        subtitle,
        background,
    });
}

fn expire_toasts(mut commands: Commands, time: Res<Time>, mut toasts: Query<(Entity, &mut Toast)>) {
    for (entity, mut toast) in &mut toasts {
        if toast.0.tick(time.delta()).just_finished() {
            commands.entity(entity).despawn();
        }
    }
}

// ---------------------------------------------------------------------------
// Four-slot inventory hotbar
// ---------------------------------------------------------------------------

#[derive(Component)]
struct HotbarCell(u8);

#[derive(Component)]
enum HotbarText {
    Key(u8),
    Name(u8),
    Amount(u8),
}

/// Everything a hotbar cell needs off the thing in the slot: what kind of
/// glassware it is, what a non-container item calls itself, and whatever the
/// player wrote on it.
type HotbarItems<'w, 's> = Query<
    'w,
    's,
    (
        &'static InventorySlot,
        Option<&'static Container>,
        Option<&'static Interactable>,
        Option<&'static crate::labels::Label>,
    ),
>;

fn hotbar_container_name(kind: ContainerKind) -> &'static str {
    match kind {
        ContainerKind::ChemicalCharge5 => "Charge · 5s",
        ContainerKind::ChemicalCharge10 => "Charge · 10s",
        ContainerKind::ChemicalCharge20 => "Charge · 20s",
        ContainerKind::PhPaper
        | ContainerKind::PhPaperStrongAcid
        | ContainerKind::PhPaperAcid
        | ContainerKind::PhPaperNeutral
        | ContainerKind::PhPaperBase
        | ContainerKind::PhPaperStrongBase => "pH Paper",
        ContainerKind::SmokeProjector => "Smoke Projector",
        _ => kind.label(),
    }
}

/// How much of a label a hotbar cell can show.
///
/// `labels::MAX_LABEL` (28) is sized for a bottle read at the counter, not for
/// a slot this narrow. Cut short rather than wrapped, because the cell also
/// has to hold a key number and a volume, and a name that reflows would move
/// both of them.
const HOTBAR_LABEL_CHARS: usize = 13;

fn hotbar_label(marked: &crate::labels::Label) -> String {
    let text = marked.0.trim();
    if text.chars().count() <= HOTBAR_LABEL_CHARS {
        return format!("\"{text}\"");
    }
    let cut: String = text.chars().take(HOTBAR_LABEL_CHARS).collect();
    format!("\"{cut}…\"")
}

fn spawn_hotbar(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                bottom: px(18),
                width: percent(100),
                justify_content: JustifyContent::Center,
                ..default()
            },
            GlobalZIndex(20),
            crate::until_we_leave_the_lab(),
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    padding: UiRect::all(px(4)),
                    column_gap: px(4),
                    border_radius: BorderRadius::all(px(5)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.035, 0.04, 0.05, 0.88)),
            ))
            .with_children(|bar| {
                for slot in 0..INVENTORY_SLOTS {
                    bar.spawn((
                        Node {
                            width: px(96),
                            height: px(72),
                            flex_direction: FlexDirection::Column,
                            justify_content: JustifyContent::SpaceBetween,
                            padding: UiRect::all(px(6)),
                            border: UiRect::all(px(3)),
                            border_radius: BorderRadius::all(px(3)),
                            ..default()
                        },
                        BackgroundColor(Color::srgba(0.10, 0.11, 0.13, 0.94)),
                        BorderColor::all(Color::srgb(0.28, 0.30, 0.34)),
                        HotbarCell(slot),
                    ))
                    .with_children(|cell| {
                        cell.spawn((
                            Text::new(""),
                            TextFont::from_font_size(12.0),
                            TextColor(TEXT_DIM),
                            HotbarText::Key(slot),
                        ));
                        cell.spawn((
                            Text::new(""),
                            TextFont::from_font_size(13.0),
                            TextColor(TEXT),
                            HotbarText::Name(slot),
                        ));
                        cell.spawn((
                            Text::new(""),
                            TextFont::from_font_size(12.0),
                            TextColor(TEXT_DIM),
                            HotbarText::Amount(slot),
                        ));
                    });
                }
            });
        });
}

fn update_hotbar(
    local: Query<(Entity, &SelectedInventorySlot), With<LocalPlayer>>,
    items: HotbarItems,
    settings: Option<Res<crate::settings::Settings>>,
    mut cells: Query<(&HotbarCell, &mut BackgroundColor, &mut BorderColor)>,
    mut texts: Query<(&HotbarText, &mut Text, &mut TextColor)>,
) {
    let Ok((owner, selected)) = local.single() else {
        return;
    };

    for (cell, mut background, mut border) in &mut cells {
        let active = cell.0 == selected.0;
        let wanted_background = if active {
            Color::srgba(0.18, 0.20, 0.23, 0.98)
        } else {
            Color::srgba(0.10, 0.11, 0.13, 0.94)
        };
        let wanted_border = if active {
            Color::srgb(0.92, 0.80, 0.42)
        } else {
            Color::srgb(0.28, 0.30, 0.34)
        };
        if background.0 != wanted_background {
            background.0 = wanted_background;
        }
        let wanted_border = BorderColor::all(wanted_border);
        if *border != wanted_border {
            *border = wanted_border;
        }
    }

    for (part, mut text, mut color) in &mut texts {
        let slot = match part {
            HotbarText::Key(slot) | HotbarText::Name(slot) | HotbarText::Amount(slot) => *slot,
        };
        let item = items
            .iter()
            .find(|(entry, _, _, _)| entry.owner == owner && entry.slot == slot);
        let (wanted, wanted_color) = match part {
            HotbarText::Key(_) => (
                if slot == selected.0 && item.is_some() {
                    let key = settings
                        .as_deref()
                        .map_or(KeyCode::KeyI, |s| s.bindings.inspect);
                    format!(
                        "{}  {} inspect",
                        slot + 1,
                        format!("{key:?}").trim_start_matches("Key")
                    )
                } else {
                    format!("{}", slot + 1)
                },
                TEXT_DIM,
            ),
            HotbarText::Name(_) => match item {
                // The label wins over the kind here, and this is the place it
                // earns the whole feature: a hotbar of four identical beakers
                // is otherwise four cells reading "Beaker", and telling them
                // apart means opening each one.
                Some((_, _, _, Some(marked))) if !marked.0.trim().is_empty() => {
                    (hotbar_label(marked), LABEL_INK)
                }
                Some((_, Some(container), _, _)) => {
                    (hotbar_container_name(container.kind).to_string(), TEXT)
                }
                Some((_, None, Some(interactable), _)) => (interactable.label.clone(), TEXT),
                Some(_) => ("Item".to_string(), TEXT),
                None => ("—".to_string(), Color::srgb(0.34, 0.37, 0.42)),
            },
            HotbarText::Amount(_) => match item {
                Some((_, Some(container), _, _)) if container.kind.capacity().is_zero() => {
                    ("TOOL".to_string(), TEXT_DIM)
                }
                Some((_, Some(container), _, _)) if container.solution.is_empty() => {
                    ("EMPTY".to_string(), TEXT_DIM)
                }
                Some((_, Some(container), _, _)) => {
                    (format!("{}", container.solution.total_volume()), GOOD_TEXT)
                }
                Some(_) => ("ITEM".to_string(), TEXT_DIM),
                None => (String::new(), TEXT_DIM),
            },
        };
        if text.0 != wanted {
            text.0 = wanted;
        }
        if color.0 != wanted_color {
            color.0 = wanted_color;
        }
    }
}

// ---------------------------------------------------------------------------
// Vitals
// ---------------------------------------------------------------------------

/// Reagents listed in the bloodstream readout before it starts summarising.
const BLOOD_SLOTS: usize = 6;

/// Every piece of text in the vitals panel, tagged by what it says.
///
/// One marker with variants rather than five separate marker components: the
/// order queue needed a `Without<>` chain per marker to keep its queries
/// disjoint, and that grows quadratically. This panel writes all of its text
/// through a single query.
#[derive(Component, PartialEq)]
enum VitalsText {
    Damage(DamageKind),
    Blood(usize),
    Status,
    Collapse,
}

/// The coloured fill inside a damage bar.
#[derive(Component)]
struct DamageBar(DamageKind);

/// A damage type's colour is the colour of the medicine that treats it, taken
/// straight from `chem.reagents.ron`.
///
/// Brute is bicaridine red, burn is dermaline orange, toxin is dylovene green,
/// oxygen is dexalin blue. A player who has made those four learns the mapping
/// from the bars without a word of tutorial text.
fn damage_color(kind: DamageKind) -> Color {
    match kind {
        DamageKind::Brute => Color::srgb(0.85, 0.25, 0.25),
        DamageKind::Burn => Color::srgb(0.95, 0.62, 0.20),
        DamageKind::Toxin => Color::srgb(0.42, 0.70, 0.36),
        DamageKind::Oxygen => Color::srgb(0.40, 0.65, 0.95),
    }
}

/// Fixed slots, filled in each frame — the same pattern as the order queue and
/// for the same reason. Four bars that decay every tick would otherwise force a
/// full despawn-and-rebuild of the panel every frame.
fn spawn_vitals_panel(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                bottom: px(16),
                right: px(16),
                width: px(280),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(12)),
                row_gap: px(5),
                border_radius: BorderRadius::all(px(6)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.06, 0.07, 0.09, 0.82)),
            crate::until_we_leave_the_lab(),
        ))
        .with_children(|panel| {
            panel.spawn((
                Text::new(""),
                TextFont::from_font_size(13.0),
                TextColor(Color::srgb(0.95, 0.45, 0.45)),
                VitalsText::Collapse,
            ));
            panel.spawn(label("CONDITION", 12.0, TEXT_DIM));

            for kind in DamageKind::ALL {
                panel.spawn(row()).with_children(|line| {
                    line.spawn((
                        Text::new(kind.label()),
                        TextFont::from_font_size(12.0),
                        TextColor(damage_color(kind)),
                        Node {
                            min_width: px(46),
                            ..default()
                        },
                    ));
                    // Track, with the fill as a child. The fill's width is the
                    // only thing the update touches.
                    line.spawn((
                        Node {
                            width: px(150),
                            height: px(9),
                            border_radius: BorderRadius::all(px(4)),
                            ..default()
                        },
                        BackgroundColor(Color::srgba(0.16, 0.17, 0.20, 0.9)),
                        children![(
                            Node {
                                width: percent(0),
                                height: percent(100),
                                border_radius: BorderRadius::all(px(4)),
                                ..default()
                            },
                            BackgroundColor(damage_color(kind)),
                            DamageBar(kind),
                        )],
                    ));
                    line.spawn((
                        Text::new(""),
                        TextFont::from_font_size(12.0),
                        TextColor(TEXT_DIM),
                        VitalsText::Damage(kind),
                    ));
                });
            }

            panel.spawn((
                Text::new(""),
                TextFont::from_font_size(12.0),
                TextColor(Color::srgb(0.80, 0.72, 0.95)),
                VitalsText::Status,
            ));
            panel.spawn(label("BLOODSTREAM", 12.0, TEXT_DIM));
            for index in 0..BLOOD_SLOTS {
                panel.spawn((
                    Text::new(""),
                    TextFont::from_font_size(13.0),
                    TextColor(TEXT),
                    VitalsText::Blood(index),
                ));
            }
        });
}

/// One bloodstream row: what it is, how much is left, and whether it is past
/// its overdose threshold.
///
/// Pure so the overdose wording can be tested without a running app.
fn blood_line(db: &ChemDb, reagent: ReagentId, quantity: Units) -> (String, Color) {
    let definition = db.reagents.get(reagent);
    let overdosing = matches!(definition.overdose, Some(threshold) if quantity > threshold);
    let line = if overdosing {
        format!("{:<15}{:>7}  OD", definition.name, quantity.to_string())
    } else {
        format!("{:<15}{:>7}", definition.name, quantity.to_string())
    };

    let [r, g, b] = definition.color;
    let color = if overdosing {
        // The book's own warning colour, so an overdose reads the same
        // everywhere it appears.
        Color::srgb(0.90, 0.62, 0.45)
    } else {
        Color::srgb(0.45 + r * 0.55, 0.45 + g * 0.55, 0.45 + b * 0.55)
    };
    (line, color)
}

fn update_vitals_panel(
    db: Res<ChemDb>,
    me: Query<(&Body, &Bloodstream), With<LocalPlayer>>,
    mut bars: Query<(&DamageBar, &mut Node)>,
    mut texts: Query<(&VitalsText, &mut Text, &mut TextColor)>,
) {
    let Ok((body, blood)) = me.single() else {
        return;
    };
    let contents = blood.0.contents();

    for (bar, mut node) in &mut bars {
        let wanted = percent(body.0.fraction(bar.0) * 100.0);
        if node.width != wanted {
            node.width = wanted;
        }
    }

    let statuses: Vec<&str> = blood
        .0
        .active_statuses()
        .map(|(kind, _)| kind.label())
        .collect();

    for (slot, mut text, mut color) in &mut texts {
        let (line, wanted) = match slot {
            VitalsText::Damage(kind) => {
                let amount = body.0.damage.get(*kind);
                let line = if amount.is_positive() {
                    format!("{amount}")
                } else {
                    String::new()
                };
                (line, TEXT_DIM)
            }
            VitalsText::Blood(index) => match contents.get(*index) {
                // The last slot summarises the overflow rather than silently
                // hiding it — a chemist needs to know there is more in them
                // than the panel has room for.
                Some(_) if *index == BLOOD_SLOTS - 1 && contents.len() > BLOOD_SLOTS => (
                    format!("+{} more", contents.len() - (BLOOD_SLOTS - 1)),
                    TEXT_DIM,
                ),
                Some((reagent, quantity)) => blood_line(&db, *reagent, *quantity),
                None => (String::new(), TEXT),
            },
            VitalsText::Status => {
                let line = if statuses.is_empty() {
                    String::new()
                } else {
                    statuses.join(" · ")
                };
                (line, Color::srgb(0.80, 0.72, 0.95))
            }
            VitalsText::Collapse => {
                let line = if body.0.collapsed {
                    "COLLAPSED".to_string()
                } else {
                    String::new()
                };
                (line, Color::srgb(0.95, 0.45, 0.45))
            }
        };

        if text.0 != line {
            text.0 = line;
        }
        if color.0 != wanted {
            color.0 = wanted;
        }
    }
}

// ---------------------------------------------------------------------------
// Radio feed
// ---------------------------------------------------------------------------

/// Which room the chemist is standing in.
///
/// Worth a line of screen for the same reason the rooms are tinted: the lab is
/// five rooms now, and a player who has walked through two doorways looking for
/// the grinder should not have to work out where they ended up from the wall
/// colour. Top-left is the one free corner — orders sit top-right, the radio
/// bottom-left and vitals bottom-right.
#[derive(Component)]
struct RoomLabel;

fn spawn_room_label(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: px(16),
                left: px(16),
                padding: UiRect::axes(px(10), px(6)),
                border_radius: BorderRadius::all(px(6)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.05, 0.06, 0.08, 0.70)),
            crate::until_we_leave_the_lab(),
        ))
        .with_children(|panel| {
            panel.spawn((
                Text::new(""),
                TextFont::from_font_size(13.0),
                TextColor(TEXT_DIM),
                RoomLabel,
            ));
        });
}

fn update_room_label(
    chemists: Query<&Transform, With<LocalPlayer>>,
    mut labels: Query<&mut Text, With<RoomLabel>>,
    areas: Res<crate::lab::WalkableAreas>,
) {
    let Ok(chemist) = chemists.single() else {
        return;
    };
    // Doorways fall outside every room rectangle, so mid-stride there is no
    // room to name. Holding the last one beats blinking the label off and on
    // every time the player crosses a threshold.
    let Some(room) = areas.room_at(chemist.translation) else {
        return;
    };
    for mut text in &mut labels {
        if text.0 != room {
            text.0 = room.to_string();
        }
    }
}

/// Slot 0 shows the oldest of the `slots` most recent lines.
///
/// The HUD only ever shows a fixed-size trailing window onto a log that can
/// now hold far more than that window — `log.entries.get(slot.0)` alone
/// only read as "the most recent lines" back when the log's own capacity
/// happened to equal the window size.
#[derive(Clone)]
struct DispatchItem {
    entry: RadioEntry,
    remaining: f32,
}

impl DispatchItem {
    fn new(entry: RadioEntry) -> Self {
        Self {
            remaining: dispatch_seconds(&entry),
            entry,
        }
    }
}

#[derive(Resource, Default)]
struct RadioDispatchQueue {
    baselined: bool,
    cursor: Option<u64>,
    pending: VecDeque<DispatchItem>,
    current: Option<DispatchItem>,
}

impl RadioDispatchQueue {
    fn ingest(&mut self, log: &RadioLog) {
        if !self.baselined {
            self.baselined = true;
            self.cursor = log.entries.back().map(|entry| entry.sequence);
            return;
        }
        let cursor = self.cursor;
        let arrivals: Vec<RadioEntry> = log
            .entries
            .iter()
            .filter(|entry| cursor.is_none_or(|cursor| entry.sequence > cursor))
            .cloned()
            .collect();
        for entry in arrivals {
            self.cursor = Some(entry.sequence);
            self.enqueue(entry);
        }
    }

    fn enqueue(&mut self, entry: RadioEntry) {
        let incoming = DispatchItem::new(entry);
        let preempts = incoming.entry.priority >= RadioPriority::Urgent
            && self
                .current
                .as_ref()
                .is_some_and(|current| incoming.entry.priority > current.entry.priority);
        if preempts {
            if let Some(interrupted) = self.current.take() {
                self.pending.push_front(interrupted);
            }
            self.current = Some(incoming);
        } else {
            self.pending.push_back(incoming);
        }
        self.trim_pending();
    }

    fn trim_pending(&mut self) {
        while self.pending.len() > RADIO_PENDING_CAPACITY {
            let discard = self
                .pending
                .iter()
                .position(|item| item.entry.priority == RadioPriority::Ambient)
                .or_else(|| {
                    self.pending
                        .iter()
                        .position(|item| item.entry.priority == RadioPriority::Routine)
                })
                .unwrap_or(0);
            self.pending.remove(discard);
        }
    }

    fn start_next(&mut self) {
        if self.current.is_some() || self.pending.is_empty() {
            return;
        }
        let highest = self
            .pending
            .iter()
            .map(|item| item.entry.priority)
            .max()
            .unwrap_or_default();
        let index = self
            .pending
            .iter()
            .position(|item| item.entry.priority == highest)
            .unwrap_or(0);
        self.current = self.pending.remove(index);
    }
}

fn dispatch_seconds(entry: &RadioEntry) -> f32 {
    let base: f32 = match entry.priority {
        RadioPriority::Ambient => 5.0,
        RadioPriority::Routine => 7.0,
        RadioPriority::Urgent => 9.0,
        RadioPriority::StationWide => 10.0,
        // The klaxon behind it runs a little over eight seconds; the card
        // should still be up when it finishes.
        RadioPriority::RedAlert => 12.0,
    };
    // A PA bulletin is the *least* urgent thing on the radio and the longest
    // thing to listen to — the fanfare in front of it runs nearly nine
    // seconds. Taking the card away four seconds into the jingle would leave
    // the player listening to a punchline they can no longer read.
    if entry.announcement {
        base.max(11.0)
    } else {
        base
    }
}

#[derive(Component)]
struct RadioDispatchCard(u64);

#[derive(Component)]
struct RadioCardBackground(Color);

#[derive(Component)]
struct RadioCardText(Color);

fn reset_radio_dispatch(mut queue: ResMut<RadioDispatchQueue>) {
    *queue = RadioDispatchQueue::default();
}

fn update_radio_dispatch(
    mut commands: Commands,
    time: Res<Time>,
    log: Res<RadioLog>,
    mut queue: ResMut<RadioDispatchQueue>,
    cards: Query<(Entity, &RadioDispatchCard)>,
) {
    queue.ingest(&log);

    if let Some(current) = queue.current.as_mut() {
        current.remaining -= time.delta_secs();
        if current.remaining <= 0.0 {
            queue.current = None;
        }
    }
    queue.start_next();

    let wanted = queue.current.as_ref().map(|item| item.entry.sequence);
    let visible = cards.iter().next().map(|(_, card)| card.0);
    if visible == wanted {
        return;
    }
    for (entity, _) in &cards {
        commands.entity(entity).despawn();
    }
    if let Some(current) = queue.current.as_ref() {
        spawn_radio_dispatch_card(&mut commands, &current.entry);
    }
}

fn spawn_radio_dispatch_card(commands: &mut Commands, entry: &RadioEntry) {
    let elevated = entry.priority >= RadioPriority::Urgent;
    let channel = radio_channel_color(entry.channel);
    let body = match entry.tone {
        RadioTone::Positive => Color::srgb(0.70, 0.93, 0.72),
        RadioTone::Negative => Color::srgb(0.96, 0.78, 0.70),
        RadioTone::Neutral => TEXT,
    };
    let heading = match entry.priority {
        RadioPriority::RedAlert => "RED ALERT".to_string(),
        RadioPriority::StationWide => "BRIDGE PRIORITY".to_string(),
        _ if entry.announcement => "STATION ANNOUNCEMENT".to_string(),
        _ => format!("{}  ·  {}", entry.channel.tag(), entry.channel.label()),
    };
    let speaker = entry.speaker.as_deref().unwrap_or("Open carrier");
    // `>=`, not `==`: a red alert is station-wide traffic too, and reading it
    // as an ordinary department line would be the one card in the game that
    // most needs the treatment losing it.
    let background = if entry.priority >= RadioPriority::StationWide {
        Color::srgba(0.20, 0.07, 0.06, 0.96)
    } else if entry.priority == RadioPriority::Urgent {
        Color::srgba(0.18, 0.10, 0.07, 0.95)
    } else {
        Color::srgba(0.05, 0.06, 0.08, 0.92)
    };

    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: if elevated { px(64) } else { Val::Auto },
                bottom: if elevated { Val::Auto } else { px(16) },
                left: if elevated { px(0) } else { px(16) },
                width: if elevated { percent(100) } else { px(560) },
                justify_content: if elevated {
                    JustifyContent::Center
                } else {
                    JustifyContent::FlexStart
                },
                ..default()
            },
            GlobalZIndex(40),
            RadioDispatchCard(entry.sequence),
            crate::until_we_leave_the_lab(),
        ))
        .with_children(|outer| {
            outer
                .spawn((
                    Node {
                        width: if elevated { px(680) } else { percent(100) },
                        flex_direction: FlexDirection::Column,
                        padding: UiRect::all(px(14)),
                        row_gap: px(4),
                        border: UiRect::left(px(if elevated { 6 } else { 4 })),
                        border_radius: BorderRadius::all(px(7)),
                        ..default()
                    },
                    BackgroundColor(background),
                    BorderColor::all(channel),
                    RadioCardBackground(background),
                ))
                .with_children(|card| {
                    card.spawn((
                        Text::new(heading),
                        TextFont::from_font_size(if elevated { 14.0 } else { 12.0 }),
                        TextColor(channel),
                        RadioCardText(channel),
                    ));
                    card.spawn((
                        Text::new(speaker.to_string()),
                        TextFont::from_font_size(15.0),
                        TextColor(TEXT),
                        RadioCardText(TEXT),
                    ));
                    card.spawn((
                        Text::new(entry.text.clone()),
                        TextFont::from_font_size(if elevated { 17.0 } else { 15.0 }),
                        TextColor(body),
                        RadioCardText(body),
                    ));
                });
        });
}

fn animate_radio_dispatch(
    queue: Res<RadioDispatchQueue>,
    mut cards: Query<(&RadioDispatchCard, &mut Node)>,
    mut backgrounds: Query<(&RadioCardBackground, &mut BackgroundColor)>,
    mut texts: Query<(&RadioCardText, &mut TextColor)>,
) {
    let Some(current) = queue.current.as_ref() else {
        return;
    };
    let Some((_, mut node)) = cards
        .iter_mut()
        .find(|(card, _)| card.0 == current.entry.sequence)
    else {
        return;
    };
    let duration = dispatch_seconds(&current.entry);
    let elapsed = duration - current.remaining;
    let alpha = (elapsed / 0.2).clamp(0.0, 1.0) * (current.remaining / 0.5).clamp(0.0, 1.0);
    if current.entry.priority >= RadioPriority::Urgent {
        node.top = px(64.0 - (1.0 - alpha) * 18.0);
    } else {
        node.left = px(16.0 - (1.0 - alpha) * 24.0);
    }
    for (base, mut color) in &mut backgrounds {
        color.0 = base.0.with_alpha(base.0.alpha() * alpha);
    }
    for (base, mut color) in &mut texts {
        color.0 = base.0.with_alpha(base.0.alpha() * alpha);
    }
}

fn radio_channel_color(channel: RadioChannel) -> Color {
    match channel {
        RadioChannel::Bridge => Color::srgb(0.96, 0.74, 0.25),
        RadioChannel::Medical => Color::srgb(0.45, 0.76, 0.96),
        RadioChannel::Security => Color::srgb(0.92, 0.34, 0.30),
        RadioChannel::Engineering => Color::srgb(0.94, 0.72, 0.24),
        RadioChannel::Cargo => Color::srgb(0.72, 0.52, 0.29),
        RadioChannel::Service => Color::srgb(0.48, 0.82, 0.43),
        RadioChannel::Lab => Color::srgb(0.65, 0.50, 0.90),
        RadioChannel::Common => Color::srgb(0.70, 0.73, 0.80),
    }
}

// ---------------------------------------------------------------------------
// Beaker preview
// ---------------------------------------------------------------------------
//
// A live visual of whatever is loaded into a machine's beaker, shared by the
// ChemMaster 5000, Mixing Chamber and Reaction Chamber panels. Every dynamic
// piece (fill height, blended colour, heat glow, hazard flash, bubble
// motion) is repainted every frame by `animate_beaker_previews`, entirely
// independent of `sync_panel`'s despawn/rebuild-on-signature-diff cycle:
// `PanelSignature` carries no temperature signal at all outside the
// Reaction Chamber (`panel_temperature`, tracked only while that panel is
// open), so nothing here can afford to rely on a rebuild to stay current.

const BEAKER_PREVIEW_WIDTH: f32 = 64.0;
const BEAKER_PREVIEW_HEIGHT: f32 = 96.0;
// The glass's own corners, echoed on `BeakerFill` (bottom only — a liquid's
// top edge is its flat surface, not a rounded lip) so the fill reads as
// poured into this exact vessel rather than an unrelated rectangle clipped
// inside it.
const BEAKER_TOP_RADIUS: f32 = 4.0;
const BEAKER_BOTTOM_RADIUS: f32 = 16.0;
const BEAKER_BUBBLE_COUNT: usize = 5;
const BEAKER_BUBBLE_RISE_SECS: f32 = 2.4;
const BEAKER_HAZARD_ALPHA_MIN: f32 = 0.12;
const BEAKER_HAZARD_ALPHA_MAX: f32 = 0.42;
const BEAKER_HAZARD_HZ: f32 = 2.2;

/// Which loaded container a beaker-preview piece belongs to, carried
/// directly on every per-frame-mutated part rather than looked up through
/// the hierarchy — keeps `animate_beaker_previews` a flat query per part,
/// like `DamageBar`/`TempSliderFill`. Mirrors `LiquidVisual { container }`
/// (`src/containers/mod.rs`), the 3D-mesh version of this same fill. Unlike
/// the always-alone `TempSlider`, more than one beaker preview can be alive
/// at once — the Mixing Chamber's beakers A and B.
#[derive(Component, Clone, Copy)]
struct BeakerOf(Entity);

/// The liquid fill: bottom-anchored, height = volume/capacity, colour =
/// [`chem_sim::Solution::color`]. The `bevy_ui` counterpart to
/// `update_liquid_visuals`'s mesh scale/tint (`src/containers/mod.rs`).
#[derive(Component)]
struct BeakerFill;

/// The outer glass, retinted from live temperature via `BoxShadow`.
#[derive(Component)]
struct BeakerGlow;

/// Full-cover translucent overlay, alpha-pulsed while the loaded solution is
/// primed for a hazardous reaction.
#[derive(Component)]
struct BeakerHazardFlash;

/// One rising bubble. `seed` (0..1, evenly spaced across
/// `BEAKER_BUBBLE_COUNT`) offsets its phase and horizontal column so a
/// beaker's bubbles never move in lockstep. Deterministic rather than
/// `rand`-seeded — nothing here needs to differ run to run, and `rand` is
/// not otherwise a dependency of this module.
#[derive(Component)]
struct BeakerBubble {
    seed: f32,
}

/// Centrepiece live preview of a loaded container, spawned beside — never
/// instead of — a panel's own compact text readout. `container` is the
/// loaded container's entity, already computed by `sync_panel` as
/// `loaded_entity`/`loaded_entity_b`, or `None` to draw a dim static
/// placeholder with no marker components at all, so an idle machine costs
/// `animate_beaker_previews` nothing to skip.
///
/// Deliberately takes no solution/reacting/hazard data of its own: every
/// dynamic pixel is repainted every frame by `animate_beaker_previews`
/// straight from a fresh container lookup, so nothing here can go stale
/// between `sync_panel` rebuilds.
fn beaker_preview(panel: &mut ChildSpawnerCommands, container: Option<Entity>) {
    beaker_preview_sized(
        panel,
        container,
        BEAKER_PREVIEW_WIDTH,
        BEAKER_PREVIEW_HEIGHT,
    );
}

/// Scaled form used when a machine treats the vessel as its main instrument
/// instead of a supporting thumbnail. Dynamic fill and hazard behavior remain
/// shared with every compact preview through the same marker components.
fn beaker_preview_sized(
    panel: &mut ChildSpawnerCommands,
    container: Option<Entity>,
    width: f32,
    height: f32,
) {
    let scale = (width / BEAKER_PREVIEW_WIDTH).clamp(1.0, 2.5);
    let top_radius = (BEAKER_TOP_RADIUS * scale).min(8.0);
    let bottom_radius = (BEAKER_BOTTOM_RADIUS * scale).min(30.0);
    let bubble_size = (6.0 * scale.sqrt()).min(9.0);
    let mut glass = panel.spawn((
        Node {
            position_type: PositionType::Relative,
            width: px(width),
            height: px(height),
            flex_shrink: 0.0,
            border: UiRect::all(px(2)),
            border_radius: BorderRadius {
                top_left: px(top_radius),
                top_right: px(top_radius),
                bottom_left: px(bottom_radius),
                bottom_right: px(bottom_radius),
            },
            overflow: Overflow::clip(),
            ..default()
        },
        BackgroundColor(Color::srgba(0.09, 0.10, 0.13, 0.65)),
        BorderColor::all(Color::srgba(1.0, 1.0, 1.0, 0.16)),
    ));
    let Some(entity) = container else {
        return; // Dim static glass outline; nothing to animate.
    };
    glass.insert((BeakerOf(entity), BeakerGlow, BoxShadow::default()));
    glass.with_children(|glass| {
        glass.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(0),
                right: px(0),
                bottom: px(0),
                height: percent(0),
                // Square top — a liquid's surface is flat, not a lip — but
                // rounded on the bottom to match the glass it is sitting in,
                // so it reads as poured into this vessel rather than an
                // unrelated rectangle merely clipped inside it.
                border_radius: BorderRadius {
                    top_left: px(0.0),
                    top_right: px(0.0),
                    bottom_left: px(bottom_radius),
                    bottom_right: px(bottom_radius),
                },
                ..default()
            },
            BackgroundColor(Color::NONE),
            BeakerOf(entity),
            BeakerFill,
        ));
        for i in 0..BEAKER_BUBBLE_COUNT {
            glass.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    width: px(bubble_size),
                    height: px(bubble_size),
                    left: percent(50.0),
                    bottom: px(0),
                    border_radius: BorderRadius::MAX,
                    ..default()
                },
                BackgroundColor(Color::NONE),
                BeakerOf(entity),
                BeakerBubble {
                    seed: i as f32 / BEAKER_BUBBLE_COUNT as f32,
                },
            ));
        }
        // Spawned last so it paints on top of the fill and bubbles, matching
        // the z-order convention `ph_gauge` already relies on for its needle.
        glass.spawn((
            Node {
                position_type: PositionType::Absolute,
                left: px(0),
                right: px(0),
                top: px(0),
                bottom: px(0),
                ..default()
            },
            BackgroundColor(ERROR_TEXT.with_alpha(0.0)),
            BeakerOf(entity),
            BeakerHazardFlash,
        ));
    });
}

/// Repaints every live [`beaker_preview`] from its container's current
/// state. Runs every frame, unconditionally, right after `sync_panel` in the
/// same `Update` chain — the same ordering `sync_thermostat_slider` already
/// documents: a widget spawned this frame is corrected before it is ever
/// drawn. Like `update_vitals_panel`/`sync_thermostat_slider`, this does not
/// gate on `Changed<Container>` — see the section banner above for why that
/// would be too coarse for this widget.
#[allow(clippy::too_many_arguments)]
fn animate_beaker_previews(
    time: Res<Time>,
    db: Res<ChemDb>,
    containers: Query<&Container>,
    buffers: Query<&Buffer>,
    agitations: Query<&AgitationRun>,
    mut fills: Query<
        (&BeakerOf, &mut Node, &mut BackgroundColor),
        (
            With<BeakerFill>,
            Without<BeakerBubble>,
            Without<BeakerHazardFlash>,
        ),
    >,
    mut glows: Query<(&BeakerOf, &mut BoxShadow)>,
    mut hazards: Query<
        (&BeakerOf, &mut BackgroundColor),
        (With<BeakerHazardFlash>, Without<BeakerFill>),
    >,
    mut bubbles: Query<
        (&BeakerOf, &BeakerBubble, &mut Node, &mut BackgroundColor),
        (Without<BeakerFill>, Without<BeakerHazardFlash>),
    >,
) {
    let t = time.elapsed_secs();
    let read = |entity| {
        containers
            .get(entity)
            .ok()
            .map(|c| Container {
                kind: c.kind,
                solution: c.solution.clone(),
            })
            .or_else(|| {
                buffers.get(entity).ok().map(|b| Container {
                    kind: ContainerKind::LargeBeaker,
                    solution: b.0.clone(),
                })
            })
    };

    for (of, mut node, mut background) in &mut fills {
        let Some(container) = read(of.0) else {
            continue;
        };
        let fill = fill_fraction(&container);
        let wanted_height = percent(fill * 100.0);
        if node.height != wanted_height {
            node.height = wanted_height;
        }
        let wanted_color = if fill > 0.0 {
            let [r, g, b] = container.solution.color(&db.reagents);
            Color::srgb(r, g, b)
        } else {
            Color::NONE
        };
        if background.0 != wanted_color {
            background.0 = wanted_color;
        }
    }

    for (of, mut shadow) in &mut glows {
        let Some(container) = read(of.0) else {
            continue;
        };
        let wanted = BoxShadow(vec![heat_glow(container.solution.temperature)]);
        if *shadow != wanted {
            *shadow = wanted;
        }
    }

    for (of, mut background) in &mut hazards {
        let Some(container) = read(of.0) else {
            continue;
        };
        let hazardous = solution_is_hazardous(&container.solution, &db.reactions);
        let alpha = if hazardous {
            let phase = (t * BEAKER_HAZARD_HZ * std::f32::consts::TAU).sin() * 0.5 + 0.5;
            BEAKER_HAZARD_ALPHA_MIN + (BEAKER_HAZARD_ALPHA_MAX - BEAKER_HAZARD_ALPHA_MIN) * phase
        } else {
            0.0
        };
        let wanted = ERROR_TEXT.with_alpha(alpha);
        if background.0 != wanted {
            background.0 = wanted;
        }
    }

    for (of, bubble, mut node, mut background) in &mut bubbles {
        let Some(container) = read(of.0) else {
            continue;
        };
        let fill = fill_fraction(&container);
        let reacting = chem_sim::is_reacting(&container.solution, &db.reactions);
        let agitating = agitations.iter().any(|run| run.destination == of.0);
        let active = (reacting || agitating) && fill > 0.04;

        let phase = (t / BEAKER_BUBBLE_RISE_SECS + bubble.seed).fract();
        let bottom_pct = (phase * (fill * 100.0 - 6.0).max(0.0)).clamp(0.0, 100.0);
        let wobble = (t * 3.0 + bubble.seed * std::f32::consts::TAU).sin() * 3.0;
        let column = 16.0 + bubble.seed * 68.0;
        let left_pct = (column + wobble).clamp(4.0, 92.0);

        node.bottom = percent(bottom_pct);
        node.left = percent(left_pct);

        let alpha = if active { 0.5 } else { 0.0 };
        let [r, g, b] = container.solution.color(&db.reagents);
        let lighten = |c: f32| c + (1.0 - c) * 0.6;
        let wanted = Color::srgba(lighten(r), lighten(g), lighten(b), alpha);
        if background.0 != wanted {
            background.0 = wanted;
        }
    }
}

fn fill_fraction(container: &Container) -> f32 {
    let volume = container.solution.total_volume();
    if !volume.is_positive() {
        return 0.0;
    }
    (volume.as_f32() / container.solution.max_volume().as_f32()).clamp(0.0, 1.0)
}

/// Colour/blur/spread for the beaker's ambient-temperature glow: fully
/// transparent at `Kelvin::AMBIENT`, warming toward red/orange at
/// `TEMPERATURE_MAX` or cooling toward blue at `TEMPERATURE_MIN` — the same
/// domain the Reaction Chamber's thermostat slider already sweeps.
fn heat_glow(temperature: Kelvin) -> ShadowStyle {
    let k = temperature.0;
    let ambient = Kelvin::AMBIENT.0;
    let (base, deviation) = if k >= ambient {
        (
            Color::srgb(0.95, 0.35, 0.15),
            ((k - ambient) / (TEMPERATURE_MAX - ambient)).clamp(0.0, 1.0),
        )
    } else {
        (
            Color::srgb(0.30, 0.55, 0.95),
            ((ambient - k) / (ambient - TEMPERATURE_MIN)).clamp(0.0, 1.0),
        )
    };
    ShadowStyle {
        color: base.with_alpha(deviation * 0.65),
        x_offset: px(0),
        y_offset: px(0),
        spread_radius: px(2.0 + deviation * 6.0),
        blur_radius: px(6.0 + deviation * 16.0),
    }
}

// ---------------------------------------------------------------------------
// Widgets
// ---------------------------------------------------------------------------

/// Converts the handful of typographic Unicode characters used by authored
/// copy into equivalents supported by the bundled UI font.
///
/// Keeping this at the presentation boundary preserves readable source prose
/// and accessibility labels while guaranteeing that dynamic strings from data
/// files receive the same treatment as hard-coded HUD readouts.
pub(crate) fn font_safe_text(text: impl AsRef<str>) -> String {
    let mut safe = String::with_capacity(text.as_ref().len());
    for character in text.as_ref().chars() {
        safe.push_str(match character {
            '\u{2014}' | '\u{2013}' | '\u{2212}' => "-",
            '\u{00b7}' => "|",
            '\u{2192}' => "->",
            '\u{2190}' => "<-",
            '\u{2026}' => "...",
            '\u{2022}' | '\u{25a0}' | '\u{25cf}' => "*",
            '\u{2265}' => ">=",
            '\u{2264}' => "<=",
            '\u{2039}' | '\u{25c2}' => "<",
            '\u{203a}' | '\u{25b8}' => ">",
            '\u{00b0}' => " deg",
            '\u{26a0}' => "!",
            '\u{207b}' => "^-",
            '\u{00b9}' => "1",
            '\u{201c}' | '\u{201d}' => "\"",
            '\u{2018}' | '\u{2019}' => "'",
            '\u{25c7}' => "OPT ",
            '\u{25af}' => "-",
            '\u{00b1}' => "+/-",
            '\u{25cb}' | '\u{25cc}' => "o",
            '\u{2248}' => "~",
            '\u{00d7}' => "x",
            _ => {
                safe.push(character);
                continue;
            }
        });
    }
    safe
}

fn normalize_changed_ui_text(mut text: Query<&mut Text, Changed<Text>>) {
    for mut text in &mut text {
        let safe = font_safe_text(&text.0);
        if safe != text.0 {
            text.0 = safe;
        }
    }
}

pub(crate) fn heading(text: impl Into<String>) -> impl Bundle {
    (
        Text::new(font_safe_text(text.into())),
        TextFont::from_font_size(FONT_SIZE_HEADING),
        TextColor(TEXT),
    )
}

pub(crate) fn label(text: impl Into<String>, size: f32, color: Color) -> impl Bundle {
    (
        Text::new(font_safe_text(text.into())),
        TextFont::from_font_size(size),
        TextColor(color),
    )
}

pub(crate) fn row() -> impl Bundle {
    Node {
        flex_direction: FlexDirection::Row,
        align_items: AlignItems::Center,
        column_gap: px(4),
        ..default()
    }
}

fn wrap_row() -> impl Bundle {
    Node {
        flex_direction: FlexDirection::Row,
        flex_wrap: FlexWrap::Wrap,
        align_items: AlignItems::Center,
        ..default()
    }
}

fn section() -> Node {
    Node {
        flex_direction: FlexDirection::Column,
        padding: UiRect::all(px(10)),
        row_gap: px(3),
        border_radius: BorderRadius::all(px(5)),
        ..default()
    }
}

/// Generic over the action so the menu's buttons look like the lab's without
/// the two sharing an action enum — a panel button dispenses a reagent, a menu
/// button opens a save, and neither wants the other's variants.
pub(crate) fn button<A: Component>(text: impl Into<String>, action: A) -> impl Bundle {
    (
        Button,
        Node {
            padding: UiRect::axes(px(11), px(6)),
            margin: UiRect::all(px(3)),
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            border_radius: BorderRadius::all(px(4)),
            ..default()
        },
        BackgroundColor(BUTTON_IDLE),
        action,
        children![(
            Text::new(font_safe_text(text.into())),
            TextFont::from_font_size(FONT_SIZE_LABEL),
            TextColor(TEXT),
        )],
    )
}

/// A small colour sample: makes a reagent recognisable by colour before a
/// player has read its name, using nothing but a coloured `Node` rectangle
/// since the lab has no icon or image assets. Bordered in `TEXT_DIM` rather
/// than left bare, because a near-black reagent's true colour (carbon's is
/// barely lighter than the panel background) would otherwise vanish into
/// whatever it sits on instead of reading as a discrete swatch.
fn swatch_chip(color: Color) -> impl Bundle {
    (
        Node {
            width: px(12),
            height: px(12),
            border: UiRect::all(px(1)),
            border_radius: BorderRadius::all(px(2)),
            flex_shrink: 0.0,
            ..default()
        },
        BackgroundColor(color),
        BorderColor::all(TEXT_DIM),
    )
}

/// One button inside a [`chip_grid`]: a label, the swatch colour that makes
/// it recognisable at a glance, and the action pressing it fires.
struct GridChip<A: Component> {
    label: String,
    swatch: Color,
    action: A,
    /// Painted `BUTTON_ACTIVE` by `button_feedback`, the same marker the
    /// dispense-amount row and book tabs use — most grids never set this,
    /// but a picker that remembers "last used" gets it for free.
    selected: bool,
}

/// A single grid cell: a swatch-and-label button at a fixed width, so a row
/// of these lines up like a grid instead of wrapping ragged, name-length
/// buttons.
fn chip_button<A: Component>(
    text: impl Into<String>,
    swatch: Color,
    width: f32,
    action: A,
) -> impl Bundle {
    let text = text.into();
    (
        Button,
        Node {
            width: px(width),
            padding: UiRect::axes(px(10), px(6)),
            margin: UiRect::all(px(3)),
            align_items: AlignItems::Center,
            column_gap: px(6),
            border_radius: BorderRadius::all(px(4)),
            ..default()
        },
        BackgroundColor(BUTTON_IDLE),
        TooltipSource::new(
            text.clone(),
            "Dispense the selected transfer volume of this base reagent.",
        ),
        accessibility_label(text.clone(), Role::Button),
        action,
        children![
            swatch_chip(swatch),
            (
                Text::new(font_safe_text(text)),
                TextFont::from_font_size(14.0),
                TextColor(TEXT),
            ),
        ],
    )
}

/// A same-width grid of swatch-and-label buttons, split into labelled
/// sub-groups — the shape every "pick one of many named, colour-coded
/// things" picker in the lab wants. Built for the ChemMaster 5000's chemical
/// list, but kept generic so another panel can group the same way later.
/// Empty groups are skipped rather than printing a bare heading over
/// nothing.
fn chip_grid<A: Component>(
    panel: &mut ChildSpawnerCommands,
    cell_width: f32,
    groups: Vec<(&str, Vec<GridChip<A>>)>,
) {
    for (heading, chips) in groups {
        if chips.is_empty() {
            continue;
        }
        panel.spawn(label(heading, 13.0, TEXT_DIM));
        panel.spawn(wrap_row()).with_children(|row| {
            for chip in chips {
                let mut entity = row.spawn(chip_button(
                    chip.label,
                    chip.swatch,
                    cell_width,
                    chip.action,
                ));
                if chip.selected {
                    entity.insert((Selected, BackgroundColor(BUTTON_ACTIVE)));
                }
            }
        });
    }
}

/// A titled card: a dim heading above a tinted [`section()`] box — the "this
/// is one coherent group of controls" unit the whole panel now uses.
/// `container_readout` was the only place this shape already existed; this
/// pulls it out so every group in a panel (amounts, stock, readouts) is
/// visibly the same kind of thing instead of four one-off layouts.
fn card(
    panel: &mut ChildSpawnerCommands,
    title: impl Into<String>,
    build: impl FnOnce(&mut ChildSpawnerCommands),
) {
    panel.spawn(label(title.into(), 13.0, TEXT_DIM));
    panel
        .spawn((section(), BackgroundColor(SECTION_BG)))
        .with_children(build);
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

pub(crate) type ChangedButtons<'w, 's> = Query<
    'w,
    's,
    (
        &'static Interaction,
        &'static mut BackgroundColor,
        Has<Selected>,
    ),
    (
        Changed<Interaction>,
        With<Button>,
        Without<PreserveButtonBackground>,
    ),
>;

pub(crate) fn button_feedback(mut buttons: ChangedButtons) {
    for (interaction, mut background, selected) in &mut buttons {
        background.0 = match interaction {
            Interaction::Pressed => BUTTON_ACTIVE,
            Interaction::Hovered => BUTTON_HOVER,
            // `Interaction` counts as changed on spawn, so without the
            // `Selected` check the highlighted amount button would be reset to
            // idle the frame the panel is built.
            Interaction::None if selected => BUTTON_ACTIVE,
            Interaction::None => BUTTON_IDLE,
        };
    }
}

#[allow(clippy::too_many_arguments)]
/// Every message a panel button can send.
///
/// Bundled into one `SystemParam` because Bevy caps a system at sixteen
/// parameters and the panel is the one place that can emit all of them. Adding
/// a machine means adding a writer here, not widening the system signature.
#[derive(SystemParam)]
struct PanelMessages<'w> {
    dispense: MessageWriter<'w, DispenseRequested>,
    agitate: MessageWriter<'w, AgitateRequested>,
    eject: MessageWriter<'w, EjectRequested>,
    take: MessageWriter<'w, TakeRequested>,
    empty: MessageWriter<'w, EmptyRequested>,
    transfer: MessageWriter<'w, BufferTransferRequested>,
    package: MessageWriter<'w, PackageRequested>,
    print: MessageWriter<'w, crate::analysis_reports::PrintReportRequested>,
    analyze: MessageWriter<'w, AnalyzeRequested>,
    purify: MessageWriter<'w, PurifyRequested>,
    grind: MessageWriter<'w, GrindRequested>,
    set_power: MessageWriter<'w, SetHeaterPower>,
    toggle_accepting: MessageWriter<'w, ToggleAcceptingOrders>,
    call_it: MessageWriter<'w, CallItAShift>,
    open_up: MessageWriter<'w, OpenUpAgain>,
    requisition: MessageWriter<'w, RequisitionRequested>,
    npc_requisition: MessageWriter<'w, NpcRequisitionRequested>,
    leave_machine: MessageWriter<'w, LeaveMachineRequested>,
    unlock_all: MessageWriter<'w, UnlockAllRequested>,
    buy_hint: MessageWriter<'w, BuyHintRequested>,
    play: MessageWriter<'w, PlaySfx>,
}

#[allow(clippy::too_many_arguments)]
fn handle_panel_clicks(
    buttons: Query<(&Interaction, &PanelAction), Changed<Interaction>>,
    mut modes: Query<(Entity, &mut InteractionMode), With<LocalPlayer>>,
    mut machine_views: ParamSet<(Query<&mut Machine>, Query<(Entity, &Machine)>)>,
    mut amounts: Query<&mut DispenseAmount>,
    mut out: PanelMessages,
    thermostats: Query<&Thermostat>,
    // Read-only mirrors of `handle_eject`'s own occupancy check — client-side,
    // so `Sfx::Eject` (and the request itself) only fires when there is
    // actually something to eject, not on every press of a button that is
    // drawn live regardless of whether the slot is empty.
    slotted: Query<(Entity, &InSlot)>,
    slotted_b: Query<(Entity, &InSlotB)>,
    slotted_c: Query<(Entity, &InSlotC)>,
    // No `ResMut<Knowledge>`/`Res<ChemDb>` here any more: buying a hint was the
    // only thing that needed them, and it now goes through the authority like
    // every other career-wide purchase. Holding a `ResMut` every frame for one
    // rare branch was also an exclusive-access constraint against every system
    // that merely reads the notebook.
    mut book: ResMut<BookView>,
    mut hplc_view: ResMut<HplcView>,
    mut social_view: ResMut<SocialView>,
    mut packaging: ResMut<mixing::PackagingDraft>,
    mouse: Option<Res<ButtonInput<MouseButton>>>,
) {
    let Some((player, mut mode)) = modes.iter_mut().next() else {
        return;
    };
    let open_machine = match *mode {
        InteractionMode::UsingMachine(machine) => Some(machine),
        _ => None,
    };
    let shop_board = machine_views
        .p1()
        .iter()
        .find(|(_, machine)| machine.kind == MachineKind::StandingBoard)
        .map(|(entity, _)| entity);

    if mouse.is_some_and(|mouse| !mouse.just_pressed(MouseButton::Left)) {
        return;
    }
    for (interaction, action) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }

        // Actions available without a machine panel open: the book's own
        // controls, and closing whatever is up.
        match action {
            PanelAction::BuyHint(reaction) => {
                out.buy_hint.write(BuyHintRequested {
                    reaction: *reaction,
                });
                continue;
            }
            PanelAction::UnlockAll => {
                out.unlock_all.write(UnlockAllRequested);
                continue;
            }
            PanelAction::ShowCategory(category) => {
                book.category = *category;
                book.page = 0;
                book.open_recipe = None;
                continue;
            }
            PanelAction::ShowBookFilter(filter) => {
                book.filter = *filter;
                book.page = 0;
                book.open_recipe = None;
                continue;
            }
            PanelAction::SetBookPage(page) => {
                book.page = *page;
                continue;
            }
            PanelAction::OpenRecipe(reaction) => {
                book.open_recipe = Some(*reaction);
                continue;
            }
            PanelAction::CloseRecipe => {
                book.open_recipe = None;
                continue;
            }
            PanelAction::CloseBook => {
                *mode = mode.toggled_book();
                return;
            }
            PanelAction::OpenOrders => {
                if let InteractionMode::Social {
                    machine,
                    return_to_book,
                } = *mode
                {
                    *mode = InteractionMode::OrderDirectory {
                        machine,
                        return_to_book,
                    };
                }
                continue;
            }
            PanelAction::ShowSocialDepartment(department) => {
                social_view.department = *department;
                if social_view
                    .resident
                    .as_deref()
                    .and_then(resident_department)
                    != Some(*department)
                {
                    social_view.resident = None;
                }
                continue;
            }
            PanelAction::SelectSocialResident(resident) => {
                if let Some(department) = resident_department(resident) {
                    social_view.department = department;
                    social_view.resident = Some(resident.clone());
                }
                continue;
            }
            PanelAction::CloseSocial => {
                *mode = mode.toggled_social();
                return;
            }
            PanelAction::Requisition(kind) => {
                if let Some(board) = shop_board {
                    out.requisition
                        .write(RequisitionRequested { board, kind: *kind });
                    out.play.write(PlaySfx(Sfx::RequisitionConfirm));
                }
                continue;
            }
            PanelAction::NpcPack(kind) => {
                if let Some(board) = shop_board {
                    out.npc_requisition
                        .write(NpcRequisitionRequested { board, kind: *kind });
                    out.play.write(PlaySfx(Sfx::RequisitionConfirm));
                }
                continue;
            }
            PanelAction::Close => {
                let mut machines = machine_views.p0();
                leave_machine(player, &mut mode, &mut machines, &mut out.leave_machine);
                return;
            }
            _ => {}
        }

        let Some(machine) = open_machine else {
            continue;
        };
        if !matches!(action, PanelAction::FocusPackageLabel) {
            packaging.editing = false;
        }
        match action {
            PanelAction::SetAmount(units) => {
                if let Ok(mut amount) = amounts.get_mut(machine) {
                    amount.0 = *units;
                }
            }
            PanelAction::Dispense(reagent) => {
                out.dispense.write(DispenseRequested {
                    machine,
                    reagent: *reagent,
                });
            }
            PanelAction::Eject(slot) => {
                let occupied = match slot {
                    MachineSlot::A => slotted_container(machine, &slotted).is_some(),
                    MachineSlot::B => slotted_container_b(machine, &slotted_b).is_some(),
                    MachineSlot::C => slotted_container_c(machine, &slotted_c).is_some(),
                };
                if occupied {
                    out.eject.write(EjectRequested {
                        machine,
                        slot: *slot,
                    });
                }
            }
            PanelAction::Take(item) => {
                out.take.write(TakeRequested {
                    machine,
                    item: *item,
                });
            }
            PanelAction::Empty(slot) => {
                out.empty.write(EmptyRequested {
                    machine,
                    slot: *slot,
                });
            }
            PanelAction::ToBuffer(reagent, amount, slot) => {
                out.transfer.write(BufferTransferRequested {
                    machine,
                    reagent: *reagent,
                    amount: *amount,
                    direction: BufferDirection::ToBuffer,
                    slot: *slot,
                });
            }
            PanelAction::ToContainer(reagent, amount, slot) => {
                out.transfer.write(BufferTransferRequested {
                    machine,
                    reagent: *reagent,
                    amount: *amount,
                    direction: BufferDirection::ToContainer,
                    slot: *slot,
                });
            }
            PanelAction::Agitate(direction) => {
                out.agitate.write(AgitateRequested {
                    machine,
                    direction: *direction,
                });
            }
            PanelAction::Package(kind) => {
                packaging.kind = *kind;
                packaging.editing = false;
            }
            PanelAction::FocusPackageLabel => {
                packaging.editing = true;
            }
            PanelAction::FinishPackage => {
                packaging.nonce += 1;
                out.package.write(PackageRequested {
                    machine,
                    kind: packaging.kind,
                    label: (!packaging.text.trim().is_empty()).then(|| packaging.text.clone()),
                    nonce: packaging.nonce,
                });
                packaging.editing = false;
            }
            PanelAction::PrintReport(report_id) => {
                out.print
                    .write(crate::analysis_reports::PrintReportRequested {
                        machine,
                        report_id: *report_id,
                    });
            }
            PanelAction::Analyze => {
                out.analyze.write(AnalyzeRequested { machine });
            }
            PanelAction::SelectHplc(reagent) => {
                hplc_view.selected = Some(*reagent);
            }
            PanelAction::Purify(reagent) => {
                out.purify.write(PurifyRequested {
                    machine,
                    reagent: *reagent,
                });
            }
            PanelAction::Grind { all } => {
                out.grind.write(GrindRequested { machine, all: *all });
            }
            PanelAction::TogglePower => {
                let on = thermostats
                    .get(machine)
                    .is_ok_and(|thermostat| !thermostat.powered);
                out.set_power.write(SetHeaterPower { machine, on });
            }
            // The board's own. Requests, not writes: the server owns the
            // standing, the sign and the shift, and a client that moved any of
            // them locally would be corrected out from under the player a
            // frame later.
            PanelAction::ToggleAcceptingOrders => {
                out.toggle_accepting
                    .write(ToggleAcceptingOrders { board: machine });
            }
            PanelAction::CallItAShift => {
                out.call_it.write(CallItAShift { board: machine });
            }
            PanelAction::OpenUpAgain => {
                out.open_up.write(OpenUpAgain { board: machine });
            }
            // Handled above, before the machine guard.
            PanelAction::BuyHint(_)
            | PanelAction::UnlockAll
            | PanelAction::ShowCategory(_)
            | PanelAction::ShowBookFilter(_)
            | PanelAction::SetBookPage(_)
            | PanelAction::OpenRecipe(_)
            | PanelAction::CloseRecipe
            | PanelAction::CloseBook
            | PanelAction::OpenOrders
            | PanelAction::ShowSocialDepartment(_)
            | PanelAction::SelectSocialResident(_)
            | PanelAction::CloseSocial
            | PanelAction::Requisition(_)
            | PanelAction::NpcPack(_)
            | PanelAction::Close => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressed_slider_tracks_keep_their_authored_dark_background() {
        let track_color = Color::srgb(0.08, 0.09, 0.12);
        let mut app = App::new();
        app.add_systems(Update, button_feedback);
        let slider = app
            .world_mut()
            .spawn((
                Button,
                Interaction::Pressed,
                BackgroundColor(track_color),
                PreserveButtonBackground,
            ))
            .id();
        let ordinary = app
            .world_mut()
            .spawn((Button, Interaction::Pressed, BackgroundColor(BUTTON_IDLE)))
            .id();

        app.update();

        assert_eq!(
            app.world().get::<BackgroundColor>(slider).unwrap().0,
            track_color
        );
        assert_eq!(
            app.world().get::<BackgroundColor>(ordinary).unwrap().0,
            BUTTON_ACTIVE
        );
    }

    #[test]
    fn social_standing_uses_signed_balances_and_plain_statuses() {
        assert_eq!(signed_standing(12), "+12");
        assert_eq!(signed_standing(0), "0");
        assert_eq!(signed_standing(-12), "-12");
        assert_eq!(standing_label(-12), "Strained");
    }

    fn spawn_social_screen_fixture(mut commands: Commands, icons: Res<BookIconAssets>) {
        let residents = crate::social::RESIDENT_NAMES
            .into_iter()
            .map(|name| ResidentSocialSnapshot {
                name: name.into(),
                relationship: PublicRelationship::default(),
                history: ConversationHistory::default(),
            })
            .collect::<Vec<_>>();
        spawn_social_directory(
            &mut commands,
            &SocialView::default(),
            &residents,
            &Shift::default(),
            None,
            None,
            &icons,
            // This fixture asserts on the finished screen's contents, not on
            // how it arrives, so it starts settled.
            bookmarks::PanelEntrance::settled(),
        );
    }

    #[test]
    fn social_screen_uses_a_visible_department_directory_without_secret_labels() {
        let mut app = App::new();
        app.add_plugins((TaskPoolPlugin::default(), AssetPlugin::default()))
            .init_asset::<Image>()
            .init_resource::<BookIconAssets>()
            .add_systems(Startup, spawn_social_screen_fixture);
        app.update();

        let mut text = app.world_mut().query::<&Text>();
        let visible = text
            .iter(app.world())
            .map(|text| text.0.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(visible.contains("CREW DIRECTORY"));
        for department in Department::ALL {
            assert!(
                visible.contains(department.label()),
                "{} needs a visible directory label",
                department.label()
            );
        }
        assert!(visible.contains("Dr. Vance"));
        assert!(visible.contains("Nurse Okonkwo"));
        assert!(visible.contains("DEPARTMENT FAVORS"));
        assert!(visible.contains("Medical  0  Unproven"));
        for secret in ["ANTAGONIST", "WARM", "BLUNT", "CAUTIOUS", "EXACTING"] {
            assert!(
                !visible.to_uppercase().contains(secret),
                "the social screen leaked secret state: {secret}"
            );
        }
    }

    #[test]
    fn social_navigation_and_shops_do_not_require_an_open_machine_panel() {
        let mut app = App::new();
        app.init_resource::<BookView>()
            .init_resource::<mixing::PackagingDraft>()
            .init_resource::<HplcView>()
            .init_resource::<SocialView>()
            .add_message::<DispenseRequested>()
            .add_message::<AgitateRequested>()
            .add_message::<EjectRequested>()
            .add_message::<TakeRequested>()
            .add_message::<EmptyRequested>()
            .add_message::<BufferTransferRequested>()
            .add_message::<PackageRequested>()
            .add_message::<AnalyzeRequested>()
            .add_message::<crate::analysis_reports::PrintReportRequested>()
            .add_message::<PurifyRequested>()
            .add_message::<GrindRequested>()
            .add_message::<SetHeaterPower>()
            .add_message::<ToggleAcceptingOrders>()
            .add_message::<CallItAShift>()
            .add_message::<OpenUpAgain>()
            .add_message::<RequisitionRequested>()
            .add_message::<NpcRequisitionRequested>()
            .add_message::<LeaveMachineRequested>()
            .add_message::<UnlockAllRequested>()
            .add_message::<BuyHintRequested>()
            .add_message::<PlaySfx>()
            .add_systems(Update, handle_panel_clicks);
        let board = app
            .world_mut()
            .spawn(Machine::new(MachineKind::StandingBoard))
            .id();
        app.world_mut().spawn((
            LocalPlayer,
            InteractionMode::Social {
                machine: None,
                return_to_book: false,
            },
        ));
        app.world_mut().spawn((
            Interaction::Pressed,
            PanelAction::ShowSocialDepartment(Department::Cargo),
        ));
        app.world_mut().spawn((
            Interaction::Pressed,
            PanelAction::Requisition(RequisitionKind::Glassware),
        ));

        app.update();

        assert_eq!(
            app.world().resource::<SocialView>().department,
            Department::Cargo
        );
        let messages = app.world().resource::<Messages<RequisitionRequested>>();
        let mut cursor = messages.get_cursor();
        let sent: Vec<_> = cursor.read(messages).collect();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].board, board);
        assert_eq!(sent[0].kind, RequisitionKind::Glassware);
    }

    #[test]
    fn a_hotbar_cell_shows_a_short_label_whole_and_quotes_it() {
        // Quoted so it can never be mistaken for something the game wrote —
        // a bottle marked "Bicaridine" must not read like a bottle the game
        // is telling you contains bicaridine.
        let marked = crate::labels::Label("Bicaridine".to_string());
        assert_eq!(hotbar_label(&marked), "\"Bicaridine\"");
    }

    #[test]
    fn a_hotbar_cell_cuts_a_long_label_short_rather_than_reflowing_the_row() {
        // `labels::MAX_LABEL` is sized for a bottle, not for this cell, and a
        // name that wrapped would move the key number and the volume with it.
        let marked = crate::labels::Label("x".repeat(crate::labels::MAX_LABEL));
        let shown = hotbar_label(&marked);
        assert!(
            shown.ends_with("…\""),
            "a cut label has to look cut: {shown}"
        );
        assert!(
            shown.chars().count() < crate::labels::MAX_LABEL,
            "the whole point was not to print all {} characters: {shown}",
            crate::labels::MAX_LABEL
        );
    }

    #[test]
    fn cutting_a_label_short_never_splits_a_character() {
        // `MAX_LABEL` counts characters and so does this, but the two used to
        // be easy to write as byte slices — which panics on any label a
        // player types with an accent in it.
        let marked = crate::labels::Label("é".repeat(crate::labels::MAX_LABEL));
        assert_eq!(
            hotbar_label(&marked).chars().filter(|c| *c == 'é').count(),
            HOTBAR_LABEL_CHARS
        );
    }

    #[test]
    fn urgent_dispatch_preempts_and_resumes_routine_traffic() {
        let mut queue = RadioDispatchQueue {
            current: Some(DispatchItem::new(RadioEntry::new(
                RadioChannel::Cargo,
                "routine",
            ))),
            ..default()
        };
        queue.enqueue(
            RadioEntry::new(RadioChannel::Medical, "urgent")
                .negative()
                .urgent(),
        );
        assert_eq!(
            queue.current.as_ref().unwrap().entry.priority,
            RadioPriority::Urgent
        );
        assert_eq!(queue.pending.front().unwrap().entry.text, "routine");
    }

    #[test]
    fn first_arrival_after_an_empty_baseline_is_presented() {
        let mut queue = RadioDispatchQueue::default();
        let mut log = RadioLog::default();
        queue.ingest(&log);
        assert!(queue.pending.is_empty());
        log.push(RadioEntry::new(RadioChannel::Cargo, "first arrival"));
        queue.ingest(&log);
        assert_eq!(queue.pending.front().unwrap().entry.text, "first arrival");
    }

    #[test]
    fn dispatch_congestion_discards_ambient_before_routine() {
        let mut queue = RadioDispatchQueue::default();
        queue.pending.push_back(DispatchItem::new(
            RadioEntry::new(RadioChannel::Bridge, "ambient").ambient(),
        ));
        for index in 0..RADIO_PENDING_CAPACITY {
            queue.pending.push_back(DispatchItem::new(RadioEntry::new(
                RadioChannel::Common,
                format!("routine {index}"),
            )));
        }
        queue.trim_pending();
        assert_eq!(queue.pending.len(), RADIO_PENDING_CAPACITY);
        assert!(queue
            .pending
            .iter()
            .all(|item| item.entry.text != "ambient"));
    }

    #[test]
    fn a_dial_reads_back_what_was_dragged_onto_it() {
        // Mirrors `settings::a_dial_reads_back_what_was_dragged_onto_it`: the
        // reaction chamber's dial is the same fraction-of-a-range shape, just
        // scoped to a machine over the network instead of a local resource.
        for fraction in [0.0, 0.25, 0.5, 1.0] {
            let value = temp_at_fraction(fraction);
            assert!(
                (temp_fraction_of(value) - fraction).abs() < 1e-5,
                "the dial lost the value at {fraction}"
            );
        }
    }

    #[test]
    fn dragging_past_either_end_of_the_dial_clamps() {
        // The drag reads a raw cursor position, which is routinely outside
        // the track — pulling toward an end is how you reach it.
        assert_eq!(temp_at_fraction(-3.0), TEMPERATURE_MIN);
        assert_eq!(temp_at_fraction(4.0), TEMPERATURE_MAX);
    }

    #[test]
    fn ph_gauge_clamps_to_the_instrument_scale() {
        assert_eq!(ph_fraction(-1.0), 0.0);
        assert_eq!(ph_fraction(7.0), 0.5);
        assert_eq!(ph_fraction(15.0), 1.0);
        assert_eq!(ph_marker_percent(0.0), 1.0);
        assert_eq!(ph_marker_percent(14.0), 99.0);
    }

    #[test]
    fn chamber_recommends_the_optimum_even_inside_a_legal_ph_range() {
        let forecast = ChamberForecast {
            product: "Test medicine".to_string(),
            target: "pH 5.0–9.0".to_string(),
            ph_target: Some(PhTarget {
                min: 5.0,
                max: 9.0,
                optimum: Some(7.0),
            }),
            lines: vec!["pH 6.0 is inside the operating range; optimum 7.0.".to_string()],
            ready: true,
            hazardous: false,
        };

        assert!(buffer_guidance(Some(&forecast), 6.0).contains("basic buffer"));
        assert!(buffer_guidance(Some(&forecast), 8.0).contains("acidic buffer"));
        assert!(buffer_guidance(Some(&forecast), 7.0).contains("at the 7.0 optimum"));
    }

    #[test]
    fn hplc_selection_falls_back_when_the_sample_changes() {
        let (db, _) = book_fixture();
        let water = db.reagent("water");
        let oxygen = db.reagent("oxygen");
        let mut sample = chem_sim::Solution::unbounded();
        let definition = db.reagents.get(water);
        let _ = sample.add_profiled(water, Units::whole(5), 1.0, definition.ph);

        assert_eq!(selected_hplc_reagent(&sample, Some(water)), Some(water));
        assert_eq!(
            selected_hplc_reagent(&sample, Some(oxygen)),
            Some(water),
            "a stale local selection must not leave Start pointed at absent material"
        );
    }

    #[test]
    fn hplc_profiles_distinguish_quality_and_recoverable_inverse_material() {
        assert_eq!(HplcProfile::of(1.0, false), HplcProfile::Clean);
        assert_eq!(HplcProfile::of(0.97, false), HplcProfile::Impure);
        assert_eq!(HplcProfile::of(1.0, true), HplcProfile::Recoverable);
    }

    #[test]
    fn chamber_targets_compact_all_authored_operating_constraints() {
        let (db, _) = book_fixture();
        let reaction = db.reactions.find("nitric_acid").unwrap();
        let target = chamber_target(reaction);

        assert!(target.contains("480K"));
        if reaction.min_ph.is_some() || reaction.max_ph.is_some() {
            assert!(target.contains("pH"));
        }
        if reaction.min_purity.is_some() {
            assert!(target.contains("pure"));
        }
    }

    #[test]
    fn panel_quality_notices_buffering_without_an_amount_change() {
        let (db, _) = book_fixture();
        let water = db.reagent("water");
        let mut sample = chem_sim::Solution::unbounded();
        let _ = sample.add_profiled(water, Units::whole(10), 1.0, 7.0);
        let before_contents: Vec<_> = sample.iter().collect();
        let before_quality = panel_quality(&sample);

        sample.shift_ph(-2.0);

        assert_eq!(before_contents, sample.iter().collect::<Vec<_>>());
        assert_ne!(before_quality, panel_quality(&sample));
    }

    #[test]
    fn panel_profiles_notice_quality_swaps_hidden_by_the_average() {
        let (db, _) = book_fixture();
        let water = db.reagent("water");
        let oxygen = db.reagent("oxygen");
        let mut first = chem_sim::Solution::unbounded();
        let mut second = chem_sim::Solution::unbounded();
        for (solution, water_purity, oxygen_purity) in
            [(&mut first, 0.9, 0.7), (&mut second, 0.7, 0.9)]
        {
            let _ = solution.add_profiled(
                water,
                Units::whole(10),
                water_purity,
                db.reagents.get(water).ph,
            );
            let _ = solution.add_profiled(
                oxygen,
                Units::whole(10),
                oxygen_purity,
                db.reagents.get(oxygen).ph,
            );
        }

        assert_eq!(panel_quality(&first), panel_quality(&second));
        assert_ne!(panel_profiles(&first), panel_profiles(&second));
    }

    #[test]
    fn chamber_forecast_explains_a_known_blocked_temperature_without_leaking_methods() {
        let (db, mut knowledge) = book_fixture();
        let chemistry = db.0.clone();
        knowledge.unlock_all(&chemistry);
        let mut sample = chem_sim::Solution::unbounded();
        for key in ["fluorosulfuric_acid", "hydrogen_peroxide", "nitrogen"] {
            let reagent = db.reagent(key);
            let definition = db.reagents.get(reagent);
            let _ = sample.add_profiled(reagent, Units::ONE, 1.0, definition.ph);
        }

        let forecast = chamber_forecast(&db, &knowledge, &sample).unwrap();
        assert_eq!(forecast.product, "Nitric Acid");
        assert!(!forecast.ready);
        assert!(forecast
            .lines
            .iter()
            .any(|line| line.contains("heat to at least 480.0K")));

        let fresh_knowledge = Knowledge::new(&chemistry);
        assert!(
            chamber_forecast(&db, &fresh_knowledge, &sample).is_none(),
            "the equipment must not reveal an unrecorded recipe"
        );
    }

    #[test]
    fn chamber_forecast_calls_out_an_unstabilized_explosive_route() {
        let (db, mut knowledge) = book_fixture();
        let chemistry = db.0.clone();
        knowledge.unlock_all(&chemistry);
        let mut sample = chem_sim::Solution::unbounded();
        for key in ["glycerol", "sulphuric_acid", "nitric_acid"] {
            let reagent = db.reagent(key);
            let definition = db.reagents.get(reagent);
            let _ = sample.add_profiled(reagent, Units::ONE, 1.0, definition.ph);
        }

        let forecast = chamber_forecast(&db, &knowledge, &sample).unwrap();
        assert_eq!(forecast.product, "Ash");
        assert!(forecast.ready);
        assert!(forecast.hazardous);
        assert!(forecast
            .lines
            .iter()
            .any(|line| line.contains("missing Stabilizing Agent")));
        assert!(forecast
            .lines
            .iter()
            .any(|line| line.starts_with("DANGER:")));
    }

    #[test]
    fn solution_is_hazardous_flags_the_same_explosive_route_the_forecast_does() {
        let (db, _knowledge) = book_fixture();
        let mut sample = chem_sim::Solution::unbounded();
        for key in ["glycerol", "sulphuric_acid", "nitric_acid"] {
            let reagent = db.reagent(key);
            let definition = db.reagents.get(reagent);
            let _ = sample.add_profiled(reagent, Units::ONE, 1.0, definition.ph);
        }
        assert!(solution_is_hazardous(&sample, &db.reactions));
        assert!(!solution_is_hazardous(
            &chem_sim::Solution::unbounded(),
            &db.reactions
        ));
    }

    #[test]
    fn the_banner_reads_the_accepting_sign() {
        let mut shift = Shift {
            accepting_orders: true,
            ..default()
        };
        assert!(accepting_banner_line(&shift).contains("OPEN"));
        // The number is the point of the line: it is what turns "another
        // order" into "shift six".
        assert!(accepting_banner_line(&shift).contains("SHIFT 1"));

        shift.accepting_orders = false;
        let closed = accepting_banner_line(&shift);
        assert!(closed.contains("CLOSED"));
        assert!(!closed.contains("debrief"));
        assert!(
            closed.is_ascii(),
            "the HUD font only supports ASCII punctuation"
        );

        shift.called = true;
        shift.shift_number = 6;
        let called = accepting_banner_line(&shift);
        assert!(
            called.is_ascii(),
            "the HUD font only supports ASCII punctuation"
        );
        assert!(called.contains("SHIFT 6"));
        assert!(
            called.contains("debrief"),
            "a called shift has somewhere to go, and the banner has to say where"
        );
    }

    #[test]
    fn font_safe_text_replaces_the_missing_glyphs_seen_in_instrument_readouts() {
        assert_eq!(font_safe_text("400.0K–473.0K"), "400.0K-473.0K");
        assert_eq!(font_safe_text("pH 4.0–10.0  ◇7.2"), "pH 4.0-10.0  OPT 7.2");
        assert_eq!(font_safe_text("≥60%"), ">=60%");
        assert_eq!(
            font_safe_text("SHIFT 4  ·  CLOSED — not accepting requests"),
            "SHIFT 4  |  CLOSED - not accepting requests"
        );
    }

    #[test]
    fn font_safe_text_covers_every_typographic_symbol_authored_in_the_ui() {
        let authored = "—·–→…•≥‹°■≤−⚠▸⁻¹←○“”‘’›◇▯±●≈◌×";
        let safe = font_safe_text(authored);
        assert!(
            safe.is_ascii(),
            "normalization left a non-ASCII glyph: {safe}"
        );
    }

    #[test]
    fn the_board_offers_call_it_a_shift_only_once_the_counter_is_clear() {
        let (_, knowledge) = book_fixture();

        let open = Shift {
            accepting_orders: true,
            ..default()
        };
        assert!(matches!(
            board_stage(&open, &knowledge, 0),
            BoardStage::Open
        ));

        // The sign is down but somebody is still waiting: the button is drawn,
        // dead, rather than appearing out of nowhere the moment they leave.
        let closing = Shift {
            accepting_orders: false,
            ..default()
        };
        assert!(matches!(
            board_stage(&closing, &knowledge, 2),
            BoardStage::WrappingUp { clear: false }
        ));
        assert!(matches!(
            board_stage(&closing, &knowledge, 0),
            BoardStage::WrappingUp { clear: true }
        ));

        let called = Shift {
            accepting_orders: false,
            called: true,
            shift_number: 3,
            ..default()
        };
        let BoardStage::Debrief(report) = board_stage(&called, &knowledge, 0) else {
            panic!("a called shift shows its debrief");
        };
        assert_eq!(report.number, 3);
    }

    #[test]
    fn a_debrief_that_is_still_moving_rebuilds_the_panel() {
        // The report is in `PanelSignature` rather than just the `called` flag
        // because the world keeps running behind the debrief: in co-op the
        // other chemist can still be delivering, and a debrief frozen on the
        // numbers it opened with is the stale readout the signature exists to
        // prevent.
        let (_, knowledge) = book_fixture();
        let mut shift = Shift {
            accepting_orders: false,
            called: true,
            succeeded: 4,
            opened_at: Some(crate::orders::ShiftSnapshot::default()),
            ..default()
        };
        let before = board_stage(&shift, &knowledge, 0);

        shift.succeeded = 5;
        assert_ne!(before, board_stage(&shift, &knowledge, 0));
    }

    // -- the reference book's grouping --------------------------------------

    fn book_fixture() -> (ChemDb, Knowledge) {
        let data = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .expect("chemistry data should load");
        let knowledge = Knowledge::new(&data);
        (ChemDb(data), knowledge)
    }

    #[test]
    fn sb16_residue_reactions_do_not_occupy_cards_or_research_progress() {
        let (db, knowledge) = book_fixture();
        let cards = recipes_in(&db, &knowledge, None);
        assert!(cards.iter().all(|r| !r.residue));
        assert_eq!(db.reactions.len() - cards.len(), 17);
        assert_eq!(knowledge.known_count(), 3);
        let ash = db.reagent("ash");
        assert!(knowledge.available_reagents(&db).contains(&ash));
        assert!(db
            .reactions
            .iter()
            .filter(|r| r.residue)
            .all(|r| knowledge.is_known(r.id)));
        assert!(db
            .reactions
            .iter()
            .any(|r| !r.residue && r.reactants.iter().any(|(id, _)| *id == ash)));
    }

    #[test]
    fn every_recipe_is_reachable_from_some_tab() {
        // The "All" tab is a convenience, not the only way in. A recipe that
        // appears under no heading is one a player browsing by ailment can
        // never find, and nothing on screen would say so.
        let (db, knowledge) = book_fixture();
        let mut filed: Vec<&str> = Vec::new();
        for category in Category::ALL {
            for reaction in recipes_in(&db, &knowledge, Some(category)) {
                filed.push(&reaction.key);
            }
        }

        for reaction in db.reactions.iter().filter(|r| !r.residue) {
            assert!(
                filed.contains(&reaction.key.as_str()),
                "'{}' appears under no heading",
                reaction.key
            );
        }
    }

    #[test]
    fn a_tab_leads_with_what_the_chemist_can_already_make() {
        // Recorded first, then alphabetical. A fresh chemist knows kelotane and
        // not dermaline, so Burns must open on kelotane.
        let (db, knowledge) = book_fixture();
        let burns = recipes_in(&db, &knowledge, Some(Category::Burns));
        let keys: Vec<&str> = burns.iter().map(|r| r.key.as_str()).collect();

        assert_eq!(keys.first(), Some(&"kelotane"), "{keys:?}");
        assert!(keys.contains(&"dermaline"), "{keys:?}");
        // Tricordrazine treats all four types, so it is filed here as well as
        // under trauma — the count columns deliberately overlap.
        assert!(keys.contains(&"tricordrazine"), "{keys:?}");

        let (known, total) = category_counts(&db, &knowledge, Some(Category::Burns));
        assert_eq!(known, 1, "only kelotane is known at the start");
        assert_eq!(total, keys.len());
    }

    #[test]
    fn the_all_tab_counts_every_recipe_exactly_once() {
        // The header line reads "N of M recorded" off `Knowledge`, and the All
        // tab has to agree with it or one of the two is lying.
        let (db, knowledge) = book_fixture();
        let (known, total) = category_counts(&db, &knowledge, None);

        assert_eq!(total, db.reactions.recipe_count());
        assert_eq!(known, knowledge.known_count());
    }

    #[test]
    fn ready_badges_match_the_actual_experiment_frontier() {
        let (db, knowledge) = book_fixture();
        let progress = RecipeProgress::new(&db, &knowledge);
        let badged: HashSet<_> = db
            .reactions
            .iter()
            .filter(|reaction| progress.state(&knowledge, reaction) == RecipeState::Ready)
            .map(|reaction| reaction.id)
            .collect();
        let reachable: HashSet<_> = knowledge.frontier(&db).into_iter().collect();

        assert_eq!(badged, reachable);
    }

    #[test]
    fn the_book_always_recommends_an_actionable_next_method() {
        let (db, knowledge) = book_fixture();
        let progress = RecipeProgress::new(&db, &knowledge);
        let (state, reaction) = progress
            .recommendation(&db, &knowledge)
            .expect("a fresh notebook needs a next method");

        assert_eq!(state, RecipeState::Ready);
        assert!(!knowledge.is_known(reaction.id));
        assert!(reaction_inputs_are_in(reaction, &progress.available));
    }

    #[test]
    fn book_progress_states_partition_every_recipe() {
        let (db, knowledge) = book_fixture();
        let progress = RecipeProgress::new(&db, &knowledge);
        let counts = [
            RecipeState::Recorded,
            RecipeState::Ready,
            RecipeState::Frontier,
            RecipeState::Locked,
        ]
        .map(|state| {
            db.reactions
                .iter()
                .filter(|reaction| {
                    !reaction.residue && progress.state(&knowledge, reaction) == state
                })
                .count()
        });

        assert_eq!(
            counts.into_iter().sum::<usize>(),
            db.reactions.recipe_count()
        );
        assert_eq!(counts[0], knowledge.known_count());
    }

    #[test]
    fn book_pages_never_grow_back_into_an_unbounded_list() {
        let (db, _) = book_fixture();
        let total = db.reactions.recipe_count();
        let (page, pages, first, last) = book_page_window(total, usize::MAX);
        assert_eq!(pages, total.div_ceil(BOOK_PAGE_SIZE));
        assert_eq!(
            page,
            pages - 1,
            "a stale page selection clamps to the final page"
        );
        assert_eq!(last, total);
        assert!(last - first <= BOOK_PAGE_SIZE);

        assert_eq!(book_page_window(0, 5), (0, 1, 0, 0));
    }

    #[test]
    fn the_known_method_names_agitation_sides_station_and_time() {
        let (db, _) = book_fixture();
        let bicaridine = db.reactions.find("bicaridine").unwrap();
        let preparation = preparation_line(&db, bicaridine);
        let conditions = condition_line(bicaridine);

        assert!(preparation.contains("Mixing Chamber"));
        assert!(preparation.contains("Inaprovaline"));
        assert!(preparation.contains("Carbon"));
        assert!(conditions.contains("4–8s"));

        let starter = db.reactions.find("inaprovaline").unwrap();
        assert!(preparation_line(&db, starter).contains("ChemMaster"));
        assert!(condition_line(starter).contains("instant"));

        let phlogiston = db.reactions.find("phlogiston").unwrap();
        assert!(condition_line(phlogiston).contains("detonates above 420.0K"));
    }

    #[test]
    fn the_known_profile_lists_body_crash_routes_and_world_behavior() {
        let (db, _) = book_fixture();
        let meth = db.reagents.get(db.reagent("methamphetamine"));
        let meth_lines = reagent_profile_lines(meth).join("\n");
        assert!(meth_lines.contains("Bodily effects"));
        assert!(meth_lines.contains("Aftereffects"));
        assert!(meth_lines.contains("Application routes"));

        let napalm = db.reagents.get(db.reagent("napalm"));
        let napalm_lines = reagent_profile_lines(napalm).join("\n");
        assert!(napalm_lines.contains("World behavior"));
        assert!(napalm_lines.contains("flammable"));
        assert!(napalm_lines.contains("slippery"));
    }

    #[test]
    fn every_crafted_profile_explicitly_covers_the_reference_book_audit_fields() {
        let (db, _) = book_fixture();
        for reaction in db.reactions.iter() {
            for product in reaction.product_ids() {
                let reagent = db.reagents.get(product);
                let lines = reagent_profile_lines(reagent).join("\n");
                for heading in [
                    "Bodily effects:",
                    "Overdose effects:",
                    "Aftereffects when cleared:",
                    "Application routes:",
                    "World behavior:",
                ] {
                    assert!(
                        lines.contains(heading),
                        "{} has no {heading} reference-book line",
                        reagent.key
                    );
                }
            }
        }
    }

    #[test]
    fn every_authored_recipe_reaches_the_visual_presentation_model() {
        let (db, _) = book_fixture();
        assert_eq!(
            db.reactions.recipe_count(),
            145,
            "update the visual audit when recipe breadth changes"
        );

        for reaction in db.reactions.iter() {
            let product = reaction
                .products
                .first()
                .map(|(id, _)| db.reagents.get(*id));
            let view = RecipePresentation::new(reaction, product);

            assert_eq!(
                view.input_count,
                reaction.reactants.len() + reaction.catalysts.len()
            );
            assert_eq!(view.catalyst_count, reaction.catalysts.len());
            assert_eq!(
                view.ph.is_some(),
                reaction.min_ph.is_some() || reaction.max_ph.is_some()
            );
            assert_eq!(view.minimum_purity.is_some(), reaction.min_purity.is_some());
            assert_eq!(view.overheat.is_some(), reaction.overheat_temp.is_some());
            assert!(!view.temperature.is_empty());
            assert!(!view.processing.is_empty());

            if let (Some(reagent), Some(profile)) = (product, view.profile.as_ref()) {
                assert_eq!(profile.ph, reagent.ph);
                assert_eq!(profile.controlled, reagent.controlled);
                assert_eq!(profile.explosive, reagent.explosive.is_some());
                assert_eq!(profile.bodily_effects, reagent.effects.len());
                assert_eq!(profile.targeted_purges, reagent.targeted_purges.len());
                assert_eq!(profile.overdose_effects, reagent.overdose_effects.len());
                assert_eq!(profile.critical_effects, reagent.critical_effects.len());
                assert_eq!(profile.after_effects, reagent.after_effects.len());
                assert_eq!(profile.world_effects, reagent.world_effects.len());
            }
        }
    }

    // -- the standing board's campaign notice ------------------------------

    fn campaign_at(reveal: Reveal) -> Campaign {
        let mut campaign = Campaign::new(crate::arc::AntagId::Cult, crate::arc::Mode::Chemist, 4);
        campaign.reveal = reveal;
        campaign
    }

    #[test]
    fn the_board_says_nothing_while_the_antagonist_is_hidden() {
        // The board is a public notice. Before the station has worked anything
        // out, there is nothing public to post — and posting early would hand
        // the player the answer the whole arc is built around.
        assert!(arc_headline(&campaign_at(Reveal::Hidden), None).is_none());
    }

    #[test]
    fn a_suspected_antagonist_is_announced_without_being_named() {
        let headline = arc_headline(&campaign_at(Reveal::Suspected), None)
            .expect("something is on the board once the station suspects");
        assert!(
            headline.name.is_none(),
            "suspecting something is not the same as knowing what it is"
        );
    }

    #[test]
    fn a_named_antagonist_is_named() {
        let headline = arc_headline(&campaign_at(Reveal::Named), None)
            .expect("a named antagonist belongs on the board");
        assert_eq!(
            headline.name.as_deref(),
            Some("the Cult"),
            "with no script loaded it should still fall back to the short label"
        );
    }

    #[test]
    fn a_resolved_arc_is_always_safe_to_post() {
        // Even one that ended while still officially hidden: it is over, and
        // the board has to be able to say how it went.
        let mut lost = campaign_at(Reveal::Hidden);
        lost.outcome = Some(crate::arc::ArcOutcome::PlotSucceeded);

        let headline = arc_headline(&lost, None).expect("a finished arc is public");
        assert_eq!(headline.resolved, Some(false));
        assert!(headline.name.is_some(), "there is nothing left to protect");
    }

    #[test]
    fn the_board_counts_the_counter_track() {
        let mut campaign = campaign_at(Reveal::Named);
        campaign.countered = vec![true, true, false, false];

        let headline = arc_headline(&campaign, None).unwrap();
        assert_eq!((headline.countered, headline.total), (2, 4));
    }
}
