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
use chem_sim::{Category, DamageKind, Kelvin, ReactionId, ReagentId, Units};

use crate::arc::{ArcScript, Campaign, Reveal};
use crate::audio::{PlaySfx, Sfx};
use crate::body::{Bloodstream, Body};
use crate::chem_data::ChemDb;
use crate::containers::{Container, ContainerKind, InSlot, InSlotB, Stored};
use crate::crew::{AtCounter, CrewMember};
use crate::interaction::{leave_machine, Interactable, InteractionMode, LeaveMachineRequested};
use crate::knowledge::{
    product_name, reaction_categories, BuyHintRequested, Knowledge, RecipeDiscovered,
    UnlockAllRequested, UpgradeDispenserRequested, HINT_COST,
};
use crate::machines::{
    slotted_container, slotted_container_b, stored_in, AgitateDirection, AgitateRequested,
    AgitationRun, AnalyzeRequested, Buffer, BufferDirection, BufferTransferRequested,
    DispenseAmount, DispenseRequested, EjectRequested, EmptyRequested, GrindRequested, Hopper,
    Machine, MachineKind, MachineSlot, PackageRequested, SetHeaterPower, SetTargetTemperature,
    TakeRequested, Thermostat, LOCKER_CAPACITY, TEMPERATURE_MARKS, TEMPERATURE_MAX,
    TEMPERATURE_MIN,
};
use crate::orders::{reference_category, Department, DevelopmentOrder, Order, Shift};
use crate::player::LocalPlayer;
use crate::produce::{ProduceCatalog, ProduceId};
use crate::radio::{RadioChannel, RadioEntry, RadioLog, RadioPriority, RadioTone};
use crate::shift::{
    can_afford, can_call_it, shift_report, CallItAShift, OpenUpAgain, RequisitionKind,
    RequisitionRequested, ShiftReport, ToggleAcceptingOrders,
};
use crate::AppState;

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
pub(crate) const BUTTON_IDLE: Color = Color::srgb(0.17, 0.19, 0.23);
const BUTTON_HOVER: Color = Color::srgb(0.25, 0.29, 0.35);
/// The "this is the one that is currently set" tint, shared by the dispense
/// amount row, the book's open tab and the settings screen's presets.
pub(crate) const BUTTON_ACTIVE: Color = Color::srgb(0.20, 0.45, 0.62);

pub struct UiPlugin;

impl Plugin for UiPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            OnEnter(AppState::Playing),
            (
                spawn_order_queue,
                reset_radio_dispatch,
                spawn_vitals_panel,
                spawn_room_label,
            ),
        )
        .add_systems(
            Update,
            (
                handle_panel_clicks,
                drag_thermostat_slider,
                button_feedback,
                sync_panel,
                finish_radio_auto_scroll,
                // After the rebuild, so a track drawn this frame has its fill
                // patched in this frame rather than sitting one frame stale —
                // the same ordering `settings::sync_sliders` uses and for the
                // same reason.
                sync_thermostat_slider,
                update_phase_banner,
                update_order_queue,
                update_vitals_panel,
                update_room_label,
                update_radio_dispatch,
                animate_radio_dispatch,
                scroll_active_pane,
                announce_discoveries,
                announce_accepting_toggle,
                show_toasts,
                expire_toasts,
            )
                .chain()
                .run_if(in_state(AppState::Playing)),
        )
        .init_resource::<BookView>()
        .init_resource::<BoardTab>()
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

/// What a button does when clicked.
#[derive(Component, Clone)]
enum PanelAction {
    SetAmount(Units),
    Dispense(ReagentId),
    UpgradeDispenser,
    UnlockAll,
    /// Which slot to act on. Every machine but the Mixing Chamber only ever
    /// has slot `A`; the panel bodies for those simply never build a `B`
    /// button.
    Eject(MachineSlot),
    /// Take one named item out of the open locker. Carries the item because a
    /// locker holds many, unlike every slot in the lab, which holds one.
    Take(Entity),
    Empty(MachineSlot),
    ToBuffer(ReagentId, Units, MachineSlot),
    ToContainer(ReagentId, Units, MachineSlot),
    Agitate(AgitateDirection),
    Package(ContainerKind),
    Analyze,
    Grind {
        all: bool,
    },
    TogglePower,
    BuyHint(ReactionId),
    ShowCategory(Option<Category>),
    OpenRecipe(ReactionId),
    CloseRecipe,
    ToggleAcceptingOrders,
    CallItAShift,
    OpenUpAgain,
    Requisition(RequisitionKind),
    ShowBoardTab(BoardTab),
    Close,
}

/// Drawn on a [`PanelAction::Requisition`] button dimmed for insufficient
/// standing — see `draw_department_shop`. Read back at click time so a click
/// on a dead button plays [`Sfx::UiRefused`] and sends nothing, instead of
/// silently mailing a request the server was always going to reject.
#[derive(Component)]
struct Refused;

/// Which of the standing board's two tabs is open.
///
/// Local presentation state, same rationale as [`BookView`]: which tab a
/// chemist has open is nobody else's business and not worth a line in
/// `save.ron`. Split out once the radio history grew from a couple of lines
/// squeezed above the department shop into its own real scrollable section —
/// on its own tab it can be the whole panel instead of splitting the screen
/// with standing every time.
#[derive(Resource, Default, Clone, Copy, PartialEq, Eq)]
enum BoardTab {
    #[default]
    Standing,
    Radio,
}

/// Which heading the reference book is open at. `None` is the "All" tab.
///
/// Local presentation state: it is neither replicated nor saved, because which
/// page a chemist happens to have open is nobody else's business and is not
/// worth a line in `save.ron`.
#[derive(Resource, Default)]
struct BookView {
    category: Option<Category>,
    /// The recipe whose tree screen is open, if any. `None` is the list.
    open_recipe: Option<ReactionId>,
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
    /// The Mixing Chamber's second beaker. `None` for every other machine,
    /// which never has one.
    container_b: Option<Entity>,
    contents_b: Vec<(ReagentId, Units)>,
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
    /// Replicated Mixing Chamber run, rounded to tenths so its visible timer
    /// updates smoothly without rebuilding the entire panel every frame.
    agitation: Option<(Entity, MachineSlot, i32, i32)>,
    amount: Option<Units>,
    known_recipes: usize,
    /// The book's open heading. Here rather than tracked separately because
    /// switching tab is exactly the same kind of change as any other: it
    /// alters what the panel shows, so it rebuilds the panel.
    book_category: Option<Category>,
    /// The recipe whose tree screen is open, if any — same rationale as
    /// `book_category`: opening or closing it changes what the panel shows.
    book_recipe: Option<ReactionId>,
    /// The standing board draws entirely from these, so without them its
    /// panel would freeze on whatever it happened to show first.
    department_standing: Vec<(Department, i32)>,
    accepting_orders: bool,
    /// Which of the board's two tabs is open — same rationale as
    /// `book_category`: switching tab changes what the panel shows, so it
    /// rebuilds the panel.
    board_tab: BoardTab,
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
            container_b: None,
            contents_b: Vec::new(),
            buffer: Vec::new(),
            hopper: Vec::new(),
            stored: Vec::new(),
            reacting: false,
            agitation: None,
            amount: None,
            known_recipes: usize::MAX,
            book_category: None,
            book_recipe: None,
            department_standing: Vec::new(),
            // Neither `true` nor `false` alone is guaranteed to differ from
            // the first real frame's value, so the vector above carries the
            // "never compared yet" signal on its own — this field just needs
            // *a* starting value.
            accepting_orders: false,
            // Same reasoning again: the vector above is what guarantees a
            // difference on the first comparison, so this only needs a value.
            board_tab: BoardTab::Standing,
            radio_sequence: None,
            board: BoardStage::Open,
            research_points: u32::MAX,
            arc: None,
            temperature: Some(i32::MAX),
            powered: true,
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
        resolved: campaign.player_won(),
    })
}

/// Rounds a temperature to the granularity [`PanelSignature`] compares at.
fn panel_temperature(kelvin: Kelvin) -> i32 {
    (kelvin.0 / 5.0).round() as i32
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
    ),
>;

/// Whatever a container is holding.
type SlotContents<'w, 's> = Query<'w, 's, &'static Container>;

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
    /// Crew who walked in and are still holding an order. Residents are
    /// filtered out because they live here: they are never what a shift is
    /// waiting on, and counting them would mean the sign could never come
    /// down at all.
    waiting: Query<'w, 's, (), (With<Order>, crate::crew::NotResident)>,
    /// The full chatter history, for the board's own scrollable section —
    /// folded in here rather than added to `sync_panel` directly, which is
    /// already at Bevy's sixteen-parameter ceiling.
    radio: Res<'w, RadioLog>,
    /// Which of the board's two tabs is open — same reason `radio` is here
    /// rather than a bare `sync_panel` parameter.
    tab: Res<'w, BoardTab>,
    radio_scroll:
        Query<'w, 's, (&'static ScrollPosition, &'static ComputedNode), With<RadioHistoryPane>>,
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
    db: &ChemDb,
    view: &StorageView,
    containers: &SlotContents,
) -> Vec<StoredItem> {
    stored_in(locker, &view.stored)
        .into_iter()
        .map(|item| {
            let container = containers.get(item).ok();
            let name = container
                .map(|container| container.kind.label().to_string())
                .or_else(|| view.labels.get(item).ok().map(|label| label.label.clone()))
                .unwrap_or_else(|| "Item".to_string());
            let detail = container.map_or_else(String::new, |container| {
                if container.solution.is_empty() {
                    "empty".to_string()
                } else {
                    container
                        .solution
                        .iter()
                        .map(|(reagent, amount)| {
                            format!("{amount} {}", db.reagents.get(reagent).name)
                        })
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            });
            StoredItem { item, name, detail }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn sync_panel(
    mut commands: Commands,
    db: Res<ChemDb>,
    existing: Query<Entity, With<PanelRoot>>,
    modes: Query<&InteractionMode, With<LocalPlayer>>,
    machines: MachineParts,
    slotted: Query<(Entity, &InSlot)>,
    slotted_b: Query<(Entity, &InSlotB)>,
    containers: SlotContents,
    storage: StorageView,
    knowledge: Res<Knowledge>,
    book: Res<BookView>,
    catalog: Option<Res<ProduceCatalog>>,
    campaign: Option<Res<Campaign>>,
    arc_script: Option<Res<crate::arc::Script>>,
    board: BoardView,
    previous: ResMut<LastPanel>,
) {
    let shift = &board.shift;
    let mode = modes.iter().next().copied().unwrap_or_default();
    let open_machine = match mode {
        InteractionMode::UsingMachine(machine) => Some(machine),
        _ => None,
    };
    let loaded_entity = open_machine.and_then(|machine| slotted_container(machine, &slotted));
    let loaded = loaded_entity.and_then(|entity| containers.get(entity).ok());
    // The Mixing Chamber's second beaker. `slotted_container_b` simply never
    // matches for any other machine, since only the Mixing Chamber ever gets
    // an `InSlotB` in the first place.
    let loaded_entity_b = open_machine.and_then(|machine| slotted_container_b(machine, &slotted_b));
    let loaded_b = loaded_entity_b.and_then(|entity| containers.get(entity).ok());
    // Derived from the beaker and the chemistry rather than read off a marker
    // component, so a guest can answer it too. `machines::Reacting` is the
    // authority's own bookkeeping and is deliberately not on the wire; a
    // client holds the same solution and the same recipes, so it does not need
    // to be told.
    let reacting =
        loaded.is_some_and(|container| chem_sim::is_reacting(&container.solution, &db.reactions));
    let machine_parts = open_machine.and_then(|machine| machines.get(machine).ok());

    // Built before the signature so both the comparison and the panel body can
    // read it — the signature itself is moved into `previous` on the way past.
    let arc = campaign
        .as_deref()
        .and_then(|campaign| arc_headline(campaign, arc_script.as_deref().map(|s| &s.0)));
    // Same reasoning as `arc` above: built once, read by both the comparison
    // and the panel body, because the signature is moved into `previous`.
    let stage = board.stage(&knowledge);

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
        mode,
        container: loaded_entity,
        contents: loaded
            .map(|container| container.solution.iter().collect())
            .unwrap_or_default(),
        container_b: loaded_entity_b,
        contents_b: loaded_b
            .map(|container| container.solution.iter().collect())
            .unwrap_or_default(),
        buffer: machine_parts
            .and_then(|(_, _, buffer, _, _, _)| buffer)
            .map(|buffer| buffer.0.iter().collect())
            .unwrap_or_default(),
        hopper: machine_parts
            .and_then(|(_, _, _, hopper, _, _)| hopper)
            .map(|hopper| hopper.0.clone())
            .unwrap_or_default(),
        stored: stored.clone(),
        reacting,
        agitation: machine_parts
            .and_then(|(_, _, _, _, _, run)| run)
            .map(|run| {
                (
                    run.destination,
                    run.direction.destination(),
                    (run.elapsed_secs * 10.0).floor() as i32,
                    (run.expected_secs * 10.0).round() as i32,
                )
            }),
        amount: machine_parts
            .and_then(|(_, amount, _, _, _, _)| amount)
            .map(|a| a.0),
        known_recipes: knowledge.known_count(),
        book_category: book.category,
        book_recipe: book.open_recipe,
        department_standing: Department::ALL
            .into_iter()
            .map(|dept| (dept, shift.standing(dept)))
            .collect(),
        accepting_orders: shift.accepting_orders,
        board_tab: *board.tab,
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
            .and_then(|(_, _, _, _, thermostat, _)| thermostat)
            .is_some_and(|thermostat| thermostat.powered),
    };

    if signature == previous.0 {
        return;
    }
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
            book.category,
            book.open_recipe,
            at_machine.is_some(),
        );
        return;
    }
    if open_machine.is_none() {
        return;
    }
    let Some((machine, amount, buffer, hopper, thermostat, agitation)) = machine_parts else {
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
            PanelRoot,
            crate::until_we_leave_the_lab(),
        ))
        .with_children(|screen| {
            screen
                .spawn((
                    Node {
                        width: px(760),
                        max_height: percent(86),
                        flex_direction: FlexDirection::Column,
                        padding: UiRect::all(px(18)),
                        row_gap: px(10),
                        border_radius: BorderRadius::all(px(8)),
                        ..default()
                    },
                    BackgroundColor(PANEL_BG),
                ))
                .with_children(|panel| {
                    panel.spawn(heading(machine.kind.label()));

                    match machine.kind {
                        MachineKind::ChemMaster5000 => {
                            dispenser_body(panel, &db, &knowledge, amount, loaded, reacting);
                        }
                        MachineKind::MixingChamber => {
                            mixing_chamber_body(panel, &db, buffer, loaded, loaded_b, agitation);
                        }
                        MachineKind::Analyzer => {
                            analyzer_body(panel, &db, &knowledge, loaded);
                        }
                        MachineKind::Grinder => {
                            grinder_body(panel, &db, catalog.as_deref(), hopper, loaded, reacting);
                        }
                        MachineKind::DeliveryWindow => {
                            delivery_window_body(panel, &db, loaded, reacting);
                        }
                        MachineKind::StandingBoard => {
                            let radio_scroll = board.radio_scroll_state();
                            standing_board_body(
                                panel,
                                shift,
                                &stage,
                                arc.as_ref(),
                                &board.radio,
                                *board.tab,
                                radio_scroll,
                            );
                        }
                        MachineKind::ReactionChamber => {
                            heater_body(panel, &db, thermostat, loaded, reacting);
                        }
                        MachineKind::Locker => {
                            locker_body(panel, &stored);
                        }
                    }

                    panel.spawn(row()).with_children(|row| {
                        row.spawn(button("Close  (Esc)", PanelAction::Close));
                    });
                });
        });
}

/// Every department's standing, what each values, live requisitions, and the
/// station's radio history, split across two tabs.
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
    tab: BoardTab,
    radio_scroll: RadioScrollState,
) {
    // The campaign notice goes above everything else, on both tabs, because
    // once there is one it is the most important thing on the board — and
    // because a player who has just been told what they are dealing with
    // should not have to switch tabs to read it.
    if let Some(arc) = arc {
        draw_arc_notice(panel, arc);
    }

    panel.spawn(row()).with_children(|row| {
        for (caption, candidate) in [
            ("Standing", BoardTab::Standing),
            ("Radio log", BoardTab::Radio),
        ] {
            let mut entity = row.spawn(button(caption, PanelAction::ShowBoardTab(candidate)));
            // Same marker the dispense-amount row and the book's own tabs
            // use, so `button_feedback` colours the open one with no extra
            // code.
            if candidate == tab {
                entity.insert((Selected, BackgroundColor(BUTTON_ACTIVE)));
            }
        }
    });

    match tab {
        BoardTab::Standing => {
            // The debrief replaces the sign controls but *not* the
            // requisition table below it. Reading what the shift came to and
            // then spending what it earned is one thought, and it is the
            // thought the removed prep phase used to be for — putting the
            // debrief on a screen of its own would split it back in two.
            // Both stay pinned above the scroll pane below: the sign toggle
            // must always be reachable without scrolling.
            match stage {
                BoardStage::Debrief(report) => draw_debrief(panel, report),
                _ => draw_sign_controls(panel, shift, stage),
            }
            panel
                .spawn((
                    Node {
                        flex_direction: FlexDirection::Column,
                        row_gap: px(10),
                        max_height: vh(60),
                        overflow: Overflow::scroll_y(),
                        ..default()
                    },
                    ScrollPosition::default(),
                    ScrollPane,
                ))
                .with_children(|scroll| {
                    draw_department_shop(scroll, shift);
                });
        }
        BoardTab::Radio => {
            // The log is the whole tab — nothing else is pinned above it but
            // the tab row itself, so it gets the same generous height the
            // book's own top-level list does.
            let mut pane = panel.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: px(6),
                    max_height: vh(66),
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
            pane.with_children(|scroll| {
                draw_radio_history(scroll, radio);
            });
        }
    }
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

/// Every department's standing, what each values, and its live requisitions.
fn draw_department_shop(panel: &mut ChildSpawnerCommands, shift: &Shift) {
    for department in Department::ALL {
        panel.spawn(label(
            format!(
                "{}  ·  {:+} standing",
                department.label(),
                shift.standing(department)
            ),
            15.0,
            TEXT,
        ));
        panel.spawn(label(department.blurb(), 12.0, TEXT_DIM));

        let kinds: Vec<RequisitionKind> = RequisitionKind::ALL
            .into_iter()
            .filter(|kind| kind.department() == department)
            .collect();
        if kinds.is_empty() {
            continue;
        }
        panel.spawn(wrap_row()).with_children(|row| {
            for &kind in &kinds {
                let available = can_afford(shift.standing(department), kind);
                let caption = format!("{} ({})", kind.label(), kind.cost());
                let mut entity = row.spawn(button(caption, PanelAction::Requisition(kind)));
                // Drawn dead rather than drawn live and silently doing
                // nothing — a button that looks clickable but is refused
                // reads as the game being broken. `Refused` is what makes
                // that literal: `handle_panel_clicks` reads it back and
                // turns a click here into `Sfx::UiRefused`, not a request.
                if !available {
                    entity.insert((BackgroundColor(Color::srgb(0.11, 0.12, 0.14)), Refused));
                }
            }
        });
        for kind in kinds {
            panel.spawn(label(
                format!("  {} — {}", kind.label(), kind.blurb()),
                12.0,
                TEXT_DIM,
            ));
        }
    }
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
        None if arc.total > 0 => format!(
            "Departments have {} of {} countermeasures in hand. They are asking you for the rest.",
            arc.countered, arc.total
        ),
        None => "Departments are working on it.".to_string(),
    };
    panel.spawn(label(detail, 12.0, TEXT_DIM));
    if arc.incidents > 0 {
        panel.spawn(label(
            format!(
                "Cult case file: {} of {} manifestations neutralised. Each ward weakens the final breach.",
                arc.treated_incidents, arc.incidents
            ),
            12.0,
            TEXT_DIM,
        ));
    }
}

fn dispenser_body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    amount: Option<&DispenseAmount>,
    loaded: Option<&Container>,
    reacting: bool,
) {
    let selected = amount.map(|a| a.0).unwrap_or(Units::whole(10));

    panel.spawn(label("Dispense amount", 13.0, TEXT_DIM));
    panel.spawn(row()).with_children(|row| {
        for step in [1, 5, 10, 25, 50] {
            let units = Units::whole(step);
            let mut entity = row.spawn(button(format!("{step}u"), PanelAction::SetAmount(units)));
            if units == selected {
                entity.insert((Selected, BackgroundColor(BUTTON_ACTIVE)));
            }
        }
    });

    panel.spawn(label("Reagents", 13.0, TEXT_DIM));

    // The balance, next to the thing it buys. Research is *spent* here, so
    // reading it should not mean closing the dispenser and opening the book to
    // check the header — by which point the tier cost below is off screen.
    panel.spawn(label(
        format!("{} research banked", knowledge.research_points),
        14.0,
        TEXT,
    ));

    // One upgrade purchase unlocks a whole tier at once, replacing the old
    // per-reagent unlock buttons below. Same "drawn dead rather than drawn
    // live and silently doing nothing" rule `standing_board_body` already
    // uses for an unaffordable requisition — clicking it either way sends
    // the same request, and `Knowledge::upgrade_dispenser` re-checks
    // affordability server-side, so a dead button is just inert, never wrong.
    if let Some(cost) = knowledge.next_upgrade_cost() {
        let affordable = knowledge.research_points >= cost;
        let caption = format!(
            "Upgrade dispenser to tier {}  ({cost} research)",
            knowledge.dispenser_tier() + 1
        );
        let mut entity = panel.spawn(button(caption, PanelAction::UpgradeDispenser));
        if !affordable {
            entity.insert(BackgroundColor(Color::srgb(0.11, 0.12, 0.14)));
        }
    }

    if knowledge.next_upgrade_cost().is_some() || knowledge.known_count() < db.reactions.len() {
        panel.spawn(button(
            "PLAYTEST: unlock all chemistry",
            PanelAction::UnlockAll,
        ));
    }

    // Grouped by tier rather than the old flat, unsorted list — locked and
    // unlocked reagents no longer interleave, and each tier reads as one
    // step of the dispenser's own progression rather than 17 separate
    // purchases.
    let mut reagents: Vec<&chem_sim::Reagent> = db.reagents.dispensable().collect();
    reagents.sort_by(|a, b| a.tier.cmp(&b.tier).then_with(|| a.name.cmp(&b.name)));

    let mut index = 0;
    while index < reagents.len() {
        let tier = reagents[index].tier;
        let end = reagents[index..].partition_point(|r| r.tier == tier) + index;
        let unlocked = knowledge.dispenser_tier() >= tier;

        panel.spawn(label(
            if tier == 0 {
                "Starting".to_string()
            } else {
                format!("Tier {tier}")
            },
            12.0,
            TEXT_DIM,
        ));
        // Only the very next tier previews its reagents by name — a chemist
        // can see what the *next* upgrade buys, but anything further out
        // stays a mystery until they've climbed that far, so the tier list
        // reads as a roadmap rather than a spoiler.
        let revealed = tier <= knowledge.dispenser_tier() + 1;
        panel.spawn(wrap_row()).with_children(|row| {
            for reagent in &reagents[index..end] {
                if unlocked {
                    row.spawn(button(
                        reagent.name.clone(),
                        PanelAction::Dispense(reagent.id),
                    ));
                } else {
                    // Not yet unlocked at this dispenser tier: shown so a
                    // chemist can see what's coming, but inert — upgrading
                    // is the only way to reach it, not a per-reagent buy.
                    let caption = if revealed {
                        reagent.name.clone()
                    } else {
                        "???".to_string()
                    };
                    let mut entity = row.spawn(button(caption, PanelAction::Dispense(reagent.id)));
                    entity.insert(BackgroundColor(Color::srgb(0.11, 0.12, 0.14)));
                }
            }
        });
        index = end;
    }

    container_readout(panel, db, loaded, reacting, true);
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

/// The reaction chamber: a dial, a switch, and a thermometer.
///
/// Deliberately spare. The machine does nothing on its own — everything
/// interesting happens because the chemistry noticed the temperature changed —
/// so the panel's whole job is telling you where you are and where you are
/// headed.
fn heater_body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    thermostat: Option<&Thermostat>,
    loaded: Option<&Container>,
    reacting: bool,
) {
    let thermostat = thermostat.copied().unwrap_or_default();

    panel.spawn(label(
        "Heats and cools whatever is loaded. Some reactions will not start until \
         it is hot enough; some come apart if it gets hotter still.",
        13.0,
        TEXT_DIM,
    ));

    let current = loaded.map(|container| container.solution.temperature);
    panel.spawn(label(
        match current {
            Some(temperature) => format!(
                "Sample  {temperature}          Target  {}          {}",
                thermostat.target,
                if thermostat.powered { "RUNNING" } else { "OFF" }
            ),
            None => "No container loaded. Carry a beaker over and press E.".to_string(),
        },
        15.0,
        // Warm as it climbs, so a chamber running away is visible without
        // reading the number.
        match current {
            Some(t) if t.0 >= 420.0 => Color::srgb(0.98, 0.45, 0.30),
            Some(t) if t.0 >= 350.0 => Color::srgb(0.95, 0.78, 0.40),
            _ => TEXT,
        },
    ));

    panel
        .spawn(Node {
            width: percent(100),
            justify_content: JustifyContent::SpaceBetween,
            margin: UiRect::top(px(10)),
            ..default()
        })
        .with_children(|row| {
            row.spawn(label("Target temperature", 14.0, TEXT));
            row.spawn((
                Text::new(format!("{:.0}K", thermostat.target.0)),
                TextFont::from_font_size(14.0),
                TextColor(TEXT_DIM),
                TempSliderReadout,
            ));
        });

    // `Button` so `Interaction` is tracked for it — that is what tells
    // `drag_thermostat_slider` a press started on the track at all.
    panel
        .spawn((
            Button,
            Node {
                width: percent(100),
                height: px(TEMP_SLIDER_TRACK_HEIGHT),
                margin: UiRect::bottom(px(4)),
                border_radius: BorderRadius::all(px(TEMP_SLIDER_TRACK_HEIGHT / 2.0)),
                ..default()
            },
            BackgroundColor(SECTION_BG),
            TempSlider,
        ))
        .with_children(|track| {
            track.spawn((
                Node {
                    width: percent(temp_fraction_of(thermostat.target.0) * 100.0),
                    height: percent(100),
                    border_radius: BorderRadius::all(px(TEMP_SLIDER_TRACK_HEIGHT / 2.0)),
                    ..default()
                },
                BackgroundColor(BUTTON_ACTIVE),
                TempSliderFill,
            ));
            // Non-interactive tick marks at the real recipe thresholds — not
            // buttons any more, just a hint of where the meaningful
            // temperatures sit along an otherwise plain dial.
            for kelvin in TEMPERATURE_MARKS {
                track.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: percent(temp_fraction_of(kelvin) * 100.0),
                        width: px(2),
                        height: percent(100),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.35)),
                ));
            }
        });

    panel.spawn(row()).with_children(|row| {
        let mut entity = row.spawn(button(
            if thermostat.powered {
                "Stop"
            } else {
                "Start heating"
            },
            PanelAction::TogglePower,
        ));
        if thermostat.powered {
            entity.insert(BackgroundColor(BUTTON_ACTIVE));
        }
        row.spawn(button(
            "Eject container",
            PanelAction::Eject(MachineSlot::A),
        ));
    });

    container_readout(panel, db, loaded, reacting, false);
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
fn mixing_chamber_body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    buffer: Option<&Buffer>,
    loaded_a: Option<&Container>,
    loaded_b: Option<&Container>,
    agitation: Option<&AgitationRun>,
) {
    let locked = agitation.is_some();
    if let Some(run) = agitation {
        panel.spawn(label(
            format!(
                "AGITATING {}   {:>3.0}%   {:.1}s remaining",
                run.direction.label(),
                run.progress() * 100.0,
                run.remaining_secs(),
            ),
            15.0,
            Color::srgb(0.48, 0.82, 0.96),
        ));
        panel.spawn(label(
            format!(
                "Batch locked in Beaker {}. Separation and ejection resume when it settles.",
                match run.direction.destination() {
                    MachineSlot::A => "A",
                    MachineSlot::B => "B",
                }
            ),
            13.0,
            TEXT_DIM,
        ));
    } else {
        let eligible = |source: Option<&Container>, destination: Option<&Container>| {
            let (Some(source), Some(destination)) = (source, destination) else {
                return false;
            };
            matches!(
                source.kind,
                ContainerKind::Beaker | ContainerKind::LargeBeaker
            ) && matches!(
                destination.kind,
                ContainerKind::Beaker | ContainerKind::LargeBeaker
            ) && source.solution.total_volume().is_positive()
                && destination.solution.available_volume() >= source.solution.total_volume()
                && !chem_sim::is_reacting(&source.solution, &db.reactions)
                && !chem_sim::is_reacting(&destination.solution, &db.reactions)
                && !db
                    .reactions
                    .activate_agitation(&source.solution, &destination.solution)
                    .is_empty()
        };
        let a_to_b = eligible(loaded_a, loaded_b);
        let b_to_a = eligible(loaded_b, loaded_a);
        panel.spawn(label(
            "Prepare recipe sides separately, then transfer one complete beaker under agitation.",
            13.0,
            TEXT_DIM,
        ));
        if a_to_b || b_to_a {
            panel.spawn(row()).with_children(|row| {
                if a_to_b {
                    row.spawn(button(
                        "Agitate A -> B",
                        PanelAction::Agitate(AgitateDirection::AToB),
                    ));
                }
                if b_to_a {
                    row.spawn(button(
                        "Agitate B -> A",
                        PanelAction::Agitate(AgitateDirection::BToA),
                    ));
                }
            });
        } else {
            panel.spawn(label(
                "No staged recipe matches these two beakers, or the destination lacks space.",
                13.0,
                Color::srgb(0.82, 0.62, 0.42),
            ));
        }
    }

    mixing_chamber_beaker(panel, db, "Beaker A", loaded_a, MachineSlot::A, locked);
    mixing_chamber_beaker(panel, db, "Beaker B", loaded_b, MachineSlot::B, locked);

    panel.spawn(label("Buffer", 13.0, TEXT_DIM));
    panel
        .spawn((section(), BackgroundColor(SECTION_BG)))
        .with_children(|section| {
            let Some(buffer) = buffer else {
                return;
            };
            if buffer.0.is_empty() {
                section.spawn(label("Empty.", 14.0, TEXT_DIM));
                return;
            }
            for (reagent, quantity) in buffer.0.iter() {
                section.spawn(row()).with_children(|row| {
                    row.spawn(reagent_name(db, reagent, quantity));
                    if !locked {
                        row.spawn(button(
                            "◂A",
                            PanelAction::ToContainer(reagent, quantity, MachineSlot::A),
                        ));
                        row.spawn(button(
                            "◂B",
                            PanelAction::ToContainer(reagent, quantity, MachineSlot::B),
                        ));
                    }
                });
            }
        });

    panel.spawn(label("Package from buffer", 13.0, TEXT_DIM));
    panel.spawn(row()).with_children(|row| {
        row.spawn(button(
            format!("Pill  ({})", ContainerKind::Pill.capacity()),
            PanelAction::Package(ContainerKind::Pill),
        ));
        row.spawn(button(
            format!("Bottle  ({})", ContainerKind::Bottle.capacity()),
            PanelAction::Package(ContainerKind::Bottle),
        ));
        // The only source of syringes in the lab. Cargo does not stock them,
        // so a chemist who wants one makes it.
        row.spawn(button(
            format!("Syringe  ({})", ContainerKind::Syringe.capacity()),
            PanelAction::Package(ContainerKind::Syringe),
        ));
    });
}

/// One of the Mixing Chamber's two beaker slots: its contents, a way to pull
/// each reagent into the shared buffer, and its own eject button — ejecting
/// is per slot, so pulling one beaker never disturbs the other.
fn mixing_chamber_beaker(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    heading_text: &str,
    loaded: Option<&Container>,
    slot: MachineSlot,
    locked: bool,
) {
    panel.spawn(label(heading_text, 13.0, TEXT_DIM));
    panel
        .spawn((section(), BackgroundColor(SECTION_BG)))
        .with_children(|section| {
            if let Some(container) = loaded {
                section.spawn(label(
                    format!(
                        "{} / {}   •   {:.0}K",
                        container.solution.total_volume(),
                        container.solution.max_volume(),
                        container.solution.temperature.0,
                    ),
                    12.0,
                    Color::srgb(0.60, 0.72, 0.82),
                ));
            }
            match loaded {
                None => {
                    section.spawn(label(
                        "No container loaded. Carry a beaker over and press E.",
                        14.0,
                        TEXT_DIM,
                    ));
                }
                Some(container) if container.solution.is_empty() => {
                    section.spawn(label("Empty.", 14.0, TEXT_DIM));
                }
                Some(container) => {
                    for (reagent, quantity) in container.solution.iter() {
                        section.spawn(row()).with_children(|row| {
                            row.spawn(reagent_name(db, reagent, quantity));
                            if !locked {
                                for step in [5, 10] {
                                    let units = Units::whole(step);
                                    row.spawn(button(
                                        format!("▸{step}"),
                                        PanelAction::ToBuffer(reagent, units, slot),
                                    ));
                                }
                                row.spawn(button(
                                    "▸All",
                                    PanelAction::ToBuffer(reagent, quantity, slot),
                                ));
                            }
                        });
                    }
                }
            }
            if !locked {
                section.spawn(row()).with_children(|row| {
                    row.spawn(button("Eject", PanelAction::Eject(slot)));
                });
            }
        });
}

fn analyzer_body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    loaded: Option<&Container>,
) {
    panel.spawn(label(
        "Breaks a sample down and works out how it was put together.",
        13.0,
        TEXT_DIM,
    ));

    panel
        .spawn((section(), BackgroundColor(SECTION_BG)))
        .with_children(|section| {
            let Some(container) = loaded else {
                section.spawn(label(
                    "No sample loaded. Carry a container over and press E.",
                    14.0,
                    TEXT_DIM,
                ));
                return;
            };
            if container.solution.is_empty() {
                section.spawn(label("Sample is empty.", 14.0, TEXT_DIM));
                return;
            }

            // Percentages rather than raw units: composition is what the
            // machine measures, and it is what identifies a mixture.
            let total = container.solution.total_volume().as_f32().max(0.01);
            for (reagent, quantity) in container.solution.iter() {
                let share = quantity.as_f32() / total * 100.0;
                section.spawn(label(
                    format!(
                        "{:<16} {:>8}   {:>5.1}%",
                        db.reagents.get(reagent).name,
                        quantity.to_string(),
                        share
                    ),
                    14.0,
                    TEXT,
                ));
            }

            let unknown: Vec<&str> = db
                .reactions
                .iter()
                .filter(|reaction| !knowledge.is_known(reaction.id))
                .filter(|reaction| {
                    reaction
                        .product_ids()
                        .any(|id| container.solution.volume_of(id).is_positive())
                })
                .map(|reaction| reaction.key.as_str())
                .collect();

            section.spawn(row()).with_children(|row| {
                row.spawn(button("Identify method", PanelAction::Analyze));
                row.spawn(button("Eject", PanelAction::Eject(MachineSlot::A)));
            });

            section.spawn(label(
                if unknown.is_empty() {
                    "Nothing here you have not already written up.".to_string()
                } else {
                    format!("{} unrecorded method(s) present.", unknown.len())
                },
                13.0,
                if unknown.is_empty() {
                    TEXT_DIM
                } else {
                    Color::srgb(0.70, 0.85, 0.60)
                },
            ));
        });
}

fn grinder_body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    catalog: Option<&ProduceCatalog>,
    hopper: Option<&Hopper>,
    loaded: Option<&Container>,
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

    container_readout(panel, db, loaded, reacting, true);
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
fn delivery_window_body(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    loaded: Option<&Container>,
    reacting: bool,
) {
    panel.spawn(label(
        "Anything left here goes to the next crew member at the counter who \
         asked for something in it. No need to wait around for them.",
        13.0,
        TEXT_DIM,
    ));

    container_readout(panel, db, loaded, reacting, false);

    let Some(container) = loaded else {
        return;
    };

    // Worst news first, same order the grading runs in.
    let (message, color) = if container.solution.is_empty() {
        (
            "Empty. Put a finished batch in and it will go out on its own.".to_string(),
            TEXT_DIM,
        )
    } else if reacting {
        // The tray hands over the instant anything in the beaker matches, and
        // a batch part-way through is half reactant and half product. So it is
        // held back rather than sent out impure — and said out loud here,
        // because a window that has quietly stopped working is worse than one
        // that says why.
        (
            "Still reacting. It will go out as soon as the batch settles.".to_string(),
            Color::srgb(0.95, 0.88, 0.45),
        )
    } else {
        (
            format!(
                "Waiting for someone who needs {}.",
                container
                    .solution
                    .iter()
                    .map(|(reagent, _)| db.reagents.get(reagent).name.clone())
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
            Color::srgb(0.70, 0.85, 0.60),
        )
    };
    panel.spawn(label(message, 13.0, color));
}

/// Shared contents readout for whatever is sitting in the machine's slot.
fn container_readout(
    panel: &mut ChildSpawnerCommands,
    db: &ChemDb,
    loaded: Option<&Container>,
    reacting: bool,
    show_empty_button: bool,
) {
    panel.spawn(label("Loaded container", 13.0, TEXT_DIM));
    panel
        .spawn((section(), BackgroundColor(SECTION_BG)))
        .with_children(|section| {
            let Some(container) = loaded else {
                section.spawn(label(
                    "No container loaded. Carry a beaker over and press E.",
                    14.0,
                    TEXT_DIM,
                ));
                return;
            };

            section.spawn(label(
                format!(
                    "{}   {} / {}",
                    container.kind.label(),
                    container.solution.total_volume(),
                    container.kind.capacity()
                ),
                15.0,
                TEXT,
            ));

            if container.solution.is_empty() {
                section.spawn(label("Empty.", 14.0, TEXT_DIM));
            } else {
                for (reagent, quantity) in container.solution.iter() {
                    section.spawn(reagent_name(db, reagent, quantity));
                }
            }

            // Some recipes take real seconds. The numbers above are already
            // moving while one runs — that is the actual readout — but a
            // chemist watching them needs to know the difference between "not
            // finished yet" and "this is all you are getting".
            if reacting {
                section.spawn(label("Still reacting…", 13.0, GOOD_TEXT));
            }

            section.spawn(row()).with_children(|row| {
                row.spawn(button("Eject", PanelAction::Eject(MachineSlot::A)));
                if show_empty_button {
                    row.spawn(button("Empty", PanelAction::Empty(MachineSlot::A)));
                }
            });
        });
}

// ---------------------------------------------------------------------------
// Reference book
// ---------------------------------------------------------------------------

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
    selected: Option<Category>,
    open_recipe: Option<ReactionId>,
    // Opened over a machine panel, which the same key closes back onto. Only
    // the header line differs, but it is the line that tells the player they
    // have not just walked away from the dispenser.
    at_machine: bool,
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
            PanelRoot,
            crate::until_we_leave_the_lab(),
        ))
        .with_children(|screen| {
            screen
                .spawn((
                    Node {
                        width: px(1040),
                        flex_direction: FlexDirection::Column,
                        padding: UiRect::all(px(20)),
                        row_gap: px(8),
                        border_radius: BorderRadius::all(px(8)),
                        ..default()
                    },
                    BackgroundColor(PANEL_BG),
                ))
                .with_children(|book| {
                    book.spawn(row()).with_children(|header| {
                        header.spawn(heading("Reference Book"));
                    });
                    book.spawn(label(
                        format!(
                            "{} of {} recipes recorded   ·   {} research   ·   \
                             scroll to read   ·   B or Esc to {}",
                            knowledge.known_count(),
                            db.reactions.len(),
                            knowledge.research_points,
                            if at_machine {
                                "go back to the machine"
                            } else {
                                "close"
                            }
                        ),
                        13.0,
                        TEXT_DIM,
                    ));

                    book.spawn(Node {
                        column_gap: px(16),
                        align_items: AlignItems::Start,
                        ..default()
                    })
                    .with_children(|columns| match open_recipe {
                        Some(id) => spawn_recipe_tree(columns, db, knowledge, db.reactions.get(id)),
                        None => {
                            book_sidebar(columns, db, knowledge, selected);
                            book_entries(columns, db, knowledge, selected);
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
) {
    columns
        .spawn(Node {
            width: px(240),
            flex_direction: FlexDirection::Column,
            row_gap: px(4),
            flex_shrink: 0.0,
            ..default()
        })
        .with_children(|sidebar| {
            // "All" first, and it is what a fresh book opens on.
            let tabs = std::iter::once(None).chain(Category::ALL.map(Some));
            for tab in tabs {
                let (known, total) = category_counts(db, knowledge, tab);
                let name = match tab {
                    Some(category) => category.label(),
                    None => "All recipes",
                };
                let mut entity = sidebar.spawn(button(
                    format!("{name}   {known}/{total}"),
                    PanelAction::ShowCategory(tab),
                ));
                // Same marker the dispense-amount row uses, so `button_feedback`
                // colours the open tab with no extra code.
                if tab == selected {
                    entity.insert((Selected, BackgroundColor(BUTTON_ACTIVE)));
                }
            }
        });
}

/// The scrolling pane of entries for whichever heading is open.
fn book_entries(
    columns: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    selected: Option<Category>,
) {
    let heading_text = selected.map(|category| (category.label(), category.blurb()));
    let visible = recipes_in(db, knowledge, selected);

    columns
        .spawn((
            Node {
                flex_grow: 1.0,
                flex_direction: FlexDirection::Column,
                row_gap: px(8),
                max_height: vh(66),
                overflow: Overflow::scroll_y(),
                ..default()
            },
            ScrollPosition::default(),
            ScrollPane,
        ))
        .with_children(|pane| {
            if let Some((name, blurb)) = heading_text {
                pane.spawn(label(name.to_string(), 15.0, TEXT));
                pane.spawn(label(blurb.to_string(), 13.0, TEXT_DIM));
            }

            if visible.is_empty() {
                pane.spawn(label(
                    "Nothing under this heading yet.".to_string(),
                    13.0,
                    TEXT_DIM,
                ));
                return;
            }

            for reaction in visible {
                book_entry(pane, db, knowledge, reaction);
            }
        });
}

/// One recipe, as a compact clickable row. Tap it to open the full formula
/// tree — that is where the method, hints and "Study further" purchase now
/// live, so this only needs enough to help a chemist recognise what they're
/// looking for.
fn book_entry(
    pane: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    reaction: &chem_sim::Reaction,
) {
    let known = knowledge.is_known(reaction.id);
    let title = product_name(db, reaction.id);
    let product = reaction
        .products
        .first()
        .map(|(id, _)| db.reagents.get(*id));

    pane.spawn((Button, section(), PanelAction::OpenRecipe(reaction.id)))
        .with_children(|entry| {
            entry.spawn(label(
                if known {
                    title.clone()
                } else {
                    format!("{title}   —   not yet worked out")
                },
                16.0,
                if known { TEXT } else { TEXT_DIM },
            ));

            if let Some(treats) = product.and_then(|p| p.treats.as_ref()) {
                entry.spawn(label(treats.clone(), 13.0, TEXT_DIM));
            }

            entry.spawn(label(
                if known {
                    "Tap to see the formula.".to_string()
                } else {
                    format!(
                        "{} ingredients — tap to research.",
                        reaction.reactants.len()
                    )
                },
                12.0,
                TEXT_DIM,
            ));
        });
}

/// How many levels of "what feeds this" the tree draws before giving up and
/// saying so. Current data's deepest chain (arithrazine ← hyronalin ←
/// dylovene) is 2; this leaves generous headroom without being unbounded.
const MAX_TREE_DEPTH: usize = 8;
/// Horizontal shift per nesting level.
const TREE_INDENT: f32 = 22.0;

/// The formula screen for one recipe: itself, then — only while known, so a
/// locked step's own ingredients stay the same spoiler the hint system
/// already withholds everywhere else — everything that feeds it, one level
/// deeper per step back through the chain.
fn spawn_recipe_tree(
    columns: &mut ChildSpawnerCommands,
    db: &ChemDb,
    knowledge: &Knowledge,
    root: &chem_sim::Reaction,
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
                        max_height: vh(60),
                        overflow: Overflow::scroll_y(),
                        ..default()
                    },
                    ScrollPosition::default(),
                    ScrollPane,
                ))
                .with_children(|pane| {
                    let mut visited = HashSet::new();
                    render_recipe_node(pane, db, knowledge, root, 0, &mut visited);
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
    reaction: &chem_sim::Reaction,
    depth: usize,
    visited: &mut HashSet<ReactionId>,
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
        entry.spawn(label(
            if known {
                title.clone()
            } else {
                format!("{title}   —   not yet worked out")
            },
            16.0,
            if known { TEXT } else { TEXT_DIM },
        ));

        if depth == 0 {
            if let Some(treats) = product.and_then(|p| p.treats.as_ref()) {
                entry.spawn(label(treats.clone(), 13.0, TEXT_DIM));
            }
        }

        if known {
            entry.spawn(label(recipe_line(db, reaction), 14.0, TEXT));
            entry.spawn(label(
                preparation_line(db, reaction),
                13.0,
                Color::srgb(0.66, 0.78, 0.92),
            ));
            entry.spawn(label(condition_line(reaction), 12.0, TEXT_DIM));
            if let Some(overdose) = product.and_then(|p| p.overdose) {
                entry.spawn(label(
                    format!("Overdoses above {overdose} in a single dose."),
                    13.0,
                    Color::srgb(0.90, 0.62, 0.45),
                ));
            }
            if depth == 0 {
                if let Some(product) = product {
                    for line in reagent_profile_lines(product) {
                        entry.spawn(label(line, 12.0, TEXT_DIM));
                    }
                }
            }
            return;
        }

        entry.spawn(label(
            format!("{} ingredients.", reaction.reactants.len()),
            13.0,
            TEXT_DIM,
        ));
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
        for &(reagent_id, amount) in &reaction.reactants {
            render_ingredient_node(
                pane,
                db,
                knowledge,
                reagent_id,
                amount,
                false,
                depth + 1,
                visited,
            );
        }
        for &(reagent_id, amount) in &reaction.catalysts {
            render_ingredient_node(
                pane,
                db,
                knowledge,
                reagent_id,
                amount,
                true,
                depth + 1,
                visited,
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
    reagent: ReagentId,
    amount: Units,
    catalyst: bool,
    depth: usize,
    visited: &mut HashSet<ReactionId>,
) {
    if let Some(producer) = db.reactions.producer_of(reagent) {
        if catalyst {
            let mut note = section();
            note.margin.left = px(depth as f32 * TREE_INDENT);
            pane.spawn(note).with_children(|entry| {
                entry.spawn(label("catalyst, not consumed:", 11.0, TEXT_DIM));
            });
        }
        render_recipe_node(pane, db, knowledge, producer, depth, visited);
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
        entry.spawn(label(line, 14.0, TEXT_DIM));
        if definition.dispensable && !knowledge.is_reagent_unlocked(db, reagent) {
            entry.spawn(label(
                format!("locked at dispenser  (tier {})", definition.tier),
                12.0,
                Color::srgb(0.80, 0.60, 0.45),
            ));
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
#[derive(Component)]
struct ScrollPane;

/// Mouse wheel scrolls whichever panel is open.
///
/// Written straight onto the one scrollable node rather than through Bevy's
/// pointer-hover scroll events: the cursor is grabbed and invisible while a
/// panel is open, so there is no hover target to route through, and only one
/// [`ScrollPane`] ever exists at a time for this to be ambiguous about.
fn scroll_active_pane(
    mut wheel: MessageReader<bevy::input::mouse::MouseWheel>,
    mut book: Query<(&mut ScrollPosition, &ComputedNode), With<ScrollPane>>,
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

    for (mut position, computed) in &mut book {
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
                "Workstation: ordinary container or ChemMaster; combines on contact.".to_string()
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
    format!("{temperature}{overheat}.  {processing}.")
}

/// Body, crash, route and station behavior for the product of a known recipe.
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
        "Application routes: inject (full/fast), ingest (slow/60%), splash, smoke or puddle contact (15%)."
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
            chem_sim::ReagentEffect::Contact(kind, amount) => format!(
                "{} contact damage {amount}/10u",
                kind.label().to_lowercase()
            ),
            chem_sim::ReagentEffect::Status {
                kind,
                seconds,
                intensity,
            } => format!("{} ({seconds:.0}s, x{intensity:.1})", kind.label()),
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

/// What the most urgent requester actually said.
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
fn spawn_order_queue(mut commands: Commands) {
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                top: px(16),
                right: px(16),
                width: px(310),
                flex_direction: FlexDirection::Column,
                padding: UiRect::all(px(12)),
                row_gap: px(6),
                border_radius: BorderRadius::all(px(6)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.06, 0.07, 0.09, 0.82)),
            crate::until_we_leave_the_lab(),
        ))
        .with_children(|queue| {
            queue.spawn((
                Text::new(""),
                TextFont::from_font_size(13.0),
                TextColor(Color::srgb(0.95, 0.88, 0.45)),
                PhaseBanner,
            ));
            queue.spawn(label("ORDERS", 13.0, TEXT_DIM));
            for index in 0..ORDER_SLOTS {
                queue.spawn((
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
        "CLOSED OUT — debrief at the board"
    } else if shift.accepting_orders {
        "OPEN — crew are coming in"
    } else {
        "CLOSED — not accepting requests"
    };
    format!("SHIFT {}  ·  {state}", shift.shift_number)
}

fn update_phase_banner(shift: Res<Shift>, banner: BannerText) {
    let line = accepting_banner_line(&shift);
    let mut banner = banner.into_inner();
    if banner.0 != line {
        banner.0 = line;
    }
}

fn update_order_queue(
    db: Res<ChemDb>,
    shift: Res<Shift>,
    orders: Query<(&CrewMember, &Order, Has<AtCounter>, Has<DevelopmentOrder>)>,
    mut slots: Query<(&OrderSlot, &mut Text, &mut TextColor), Without<ShiftReadout>>,
    readout: ShiftText,
    plea_line: PleaText,
) {
    // Most urgent first, so the one about to expire is always at the top.
    let mut pending: Vec<(&CrewMember, &Order, bool, bool)> = orders.iter().collect();
    pending.sort_by(|a, b| a.1.remaining().total_cmp(&b.1.remaining()));

    for (slot, mut text, mut color) in &mut slots {
        let line = pending
            .get(slot.0)
            .map(|(member, order, at_counter, development)| {
                // `order.specific` is freely queryable (nothing secret about it —
                // see its own doc comment) and always wins when set. Otherwise
                // this never queries `Has<IllicitOrder>` — see that marker's own
                // doc comment — and instead reads purely from the reagent's own
                // category: a lenient legitimate order's category is always one
                // of the six orderable ones, so it shows a want-phrase; anything
                // else (every antagonist reagent is `Illicit`) falls back to the
                // real name, which is no more a tell than the pretext already is.
                let want = if order.specific {
                    db.reagents.get(order.reagent).name.clone()
                } else {
                    reference_category(&db, order.reagent)
                        .filter(|cat| cat.is_legitimately_orderable())
                        .map(|cat| cat.want_phrase().to_string())
                        .unwrap_or_else(|| db.reagents.get(order.reagent).name.clone())
                };
                let reagent = &want;
                let heading = if *development {
                    format!("OPTIONAL R&D \u{2014} {}", member.name)
                } else {
                    member.name.clone()
                };
                if *at_counter {
                    let remaining = order.remaining() as u32;
                    format!(
                        "{}\n  {} {}  ·  {}:{:02}",
                        heading,
                        order.amount,
                        reagent,
                        remaining / 60,
                        remaining % 60
                    )
                } else {
                    format!("{}\n  {} {}  ·  on the way", heading, order.amount, reagent)
                }
            });

        let urgent = pending
            .get(slot.0)
            .map(|(_, order, _, development)| !development && order.remaining() < 30.0)
            .unwrap_or(false);
        let development = pending
            .get(slot.0)
            .map(|(_, _, _, development)| *development)
            .unwrap_or(false);
        let wanted = if development {
            Color::srgb(0.50, 0.82, 0.92)
        } else if urgent {
            Color::srgb(0.95, 0.55, 0.45)
        } else {
            TEXT
        };
        if color.0 != wanted {
            color.0 = wanted;
        }

        let line = line.unwrap_or_default();
        if text.0 != line {
            text.0 = line;
        }
    }

    // Only the most urgent request gets its words shown; three at once is a
    // wall of text nobody reads mid-shift.
    let plea = pending
        .first()
        .map(|(_, order, _, development)| {
            if *development {
                format!(
                    "Optional development request \u{2014} \u{201c}{}\u{201d}",
                    order.plea
                )
            } else {
                format!("\u{201c}{}\u{201d}", order.plea)
            }
        })
        .unwrap_or_default();
    let mut plea_line = plea_line.into_inner();
    if plea_line.0 != plea {
        plea_line.0 = plea;
    }

    let summary = format!("delivered {}   botched {}", shift.succeeded, shift.botched);
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
// Widgets
// ---------------------------------------------------------------------------

pub(crate) fn heading(text: impl Into<String>) -> impl Bundle {
    (
        Text::new(text.into()),
        TextFont::from_font_size(22.0),
        TextColor(TEXT),
    )
}

pub(crate) fn label(text: impl Into<String>, size: f32, color: Color) -> impl Bundle {
    (
        Text::new(text.into()),
        TextFont::from_font_size(size),
        TextColor(color),
    )
}

fn reagent_name(db: &ChemDb, reagent: ReagentId, quantity: Units) -> impl Bundle {
    let definition = db.reagents.get(reagent);
    let [r, g, b] = definition.color;
    (
        Text::new(format!(
            "{:<16} {:>8}",
            definition.name,
            quantity.to_string()
        )),
        TextFont::from_font_size(14.0),
        // Tinting toward the reagent's own colour makes a mixed beaker
        // scannable at a glance instead of a wall of identical text.
        TextColor(Color::srgb(
            0.45 + r * 0.55,
            0.45 + g * 0.55,
            0.45 + b * 0.55,
        )),
        Node {
            min_width: px(230),
            ..default()
        },
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
            Text::new(text.into()),
            TextFont::from_font_size(14.0),
            TextColor(TEXT),
        )],
    )
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
    (Changed<Interaction>, With<Button>),
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
    analyze: MessageWriter<'w, AnalyzeRequested>,
    grind: MessageWriter<'w, GrindRequested>,
    set_power: MessageWriter<'w, SetHeaterPower>,
    toggle_accepting: MessageWriter<'w, ToggleAcceptingOrders>,
    call_it: MessageWriter<'w, CallItAShift>,
    open_up: MessageWriter<'w, OpenUpAgain>,
    requisition: MessageWriter<'w, RequisitionRequested>,
    leave_machine: MessageWriter<'w, LeaveMachineRequested>,
    upgrade_dispenser: MessageWriter<'w, UpgradeDispenserRequested>,
    unlock_all: MessageWriter<'w, UnlockAllRequested>,
    buy_hint: MessageWriter<'w, BuyHintRequested>,
    play: MessageWriter<'w, PlaySfx>,
}

#[allow(clippy::too_many_arguments)]
fn handle_panel_clicks(
    buttons: Query<(Entity, &Interaction, &PanelAction), Changed<Interaction>>,
    refused: Query<(), With<Refused>>,
    mut modes: Query<(Entity, &mut InteractionMode), With<LocalPlayer>>,
    mut machines: Query<&mut Machine>,
    mut amounts: Query<&mut DispenseAmount>,
    mut out: PanelMessages,
    thermostats: Query<&Thermostat>,
    // Read-only mirrors of `handle_eject`'s own occupancy check — client-side,
    // so `Sfx::Eject` (and the request itself) only fires when there is
    // actually something to eject, not on every press of a button that is
    // drawn live regardless of whether the slot is empty.
    slotted: Query<(Entity, &InSlot)>,
    slotted_b: Query<(Entity, &InSlotB)>,
    // No `ResMut<Knowledge>`/`Res<ChemDb>` here any more: buying a hint was the
    // only thing that needed them, and it now goes through the authority like
    // every other career-wide purchase. Holding a `ResMut` every frame for one
    // rare branch was also an exclusive-access constraint against every system
    // that merely reads the notebook.
    mut book: ResMut<BookView>,
    mut board_tab: ResMut<BoardTab>,
) {
    let Some((player, mut mode)) = modes.iter_mut().next() else {
        return;
    };
    let open_machine = match *mode {
        InteractionMode::UsingMachine(machine) => Some(machine),
        _ => None,
    };

    for (entity, interaction, action) in &buttons {
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
            PanelAction::UpgradeDispenser => {
                out.upgrade_dispenser.write(UpgradeDispenserRequested);
                continue;
            }
            PanelAction::UnlockAll => {
                out.unlock_all.write(UnlockAllRequested);
                continue;
            }
            PanelAction::ShowCategory(category) => {
                book.category = *category;
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
            PanelAction::ShowBoardTab(tab) => {
                *board_tab = *tab;
                continue;
            }
            PanelAction::Close => {
                leave_machine(player, &mut mode, &mut machines, &mut out.leave_machine);
                return;
            }
            _ => {}
        }

        let Some(machine) = open_machine else {
            continue;
        };
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
                out.package.write(PackageRequested {
                    machine,
                    kind: *kind,
                });
            }
            PanelAction::Analyze => {
                out.analyze.write(AnalyzeRequested { machine });
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
            PanelAction::Requisition(kind) => {
                if refused.contains(entity) {
                    // Dimmed for insufficient standing (`draw_department_shop`)
                    // — a click here plays the refusal and sends nothing,
                    // rather than mailing a request the server would only
                    // reject.
                    out.play.write(PlaySfx(Sfx::UiRefused));
                } else {
                    out.requisition.write(RequisitionRequested {
                        board: machine,
                        kind: *kind,
                    });
                    out.play.write(PlaySfx(Sfx::RequisitionConfirm));
                }
            }
            // Handled above, before the machine guard.
            PanelAction::BuyHint(_)
            | PanelAction::UpgradeDispenser
            | PanelAction::UnlockAll
            | PanelAction::ShowCategory(_)
            | PanelAction::OpenRecipe(_)
            | PanelAction::CloseRecipe
            | PanelAction::ShowBoardTab(_)
            | PanelAction::Close => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_dial_range_covers_every_recipe_threshold_with_margin() {
        // The whole reason 173-600K was chosen: 100K of headroom past the
        // lowest and highest temperatures anything in the data gates on.
        for kelvin in TEMPERATURE_MARKS {
            assert!(
                kelvin > TEMPERATURE_MIN && kelvin < TEMPERATURE_MAX,
                "{kelvin}K threshold sits outside the dial's own range"
            );
        }
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
        assert!(accepting_banner_line(&shift).contains("CLOSED"));
        assert!(!accepting_banner_line(&shift).contains("debrief"));

        shift.called = true;
        shift.shift_number = 6;
        let called = accepting_banner_line(&shift);
        assert!(called.contains("SHIFT 6"));
        assert!(
            called.contains("debrief"),
            "a called shift has somewhere to go, and the banner has to say where"
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

        for reaction in db.reactions.iter() {
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

        assert_eq!(total, db.reactions.len());
        assert_eq!(known, knowledge.known_count());
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
