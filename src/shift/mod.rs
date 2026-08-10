//! The shift cycle: prep, service, debrief.
//!
//! The game used to be one unbroken stream of orders, which meant the test
//! bench, the grinder and the reference book all competed with a live deadline
//! and so never got used. A shift is the frame that gives them room: nobody
//! asks for anything during prep, so stocking up and experimenting are things
//! you *choose* to spend a window on rather than things you steal time for.
//!
//! Everything that decides how a shift goes lives in this module as a pure
//! function — the difficulty ramp, the phase transitions, the quota, the
//! forecast weighting, the restock arithmetic, the requisition costs. Balance
//! that can only be checked by playing shift nine is balance nobody checks.

use bevy::ecs::entity::MapEntities;
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use rand::prelude::*;
use serde::{Deserialize, Serialize};

use crate::crew::{CrewMember, CrewRoute};
use crate::interaction::Interactable;
use crate::knowledge::Knowledge;
use crate::machines::{Machine, MachineKind};
use crate::orders::{
    ForecastDef, Order, OrderConfig, OrderResolved, OrderSpawner, RampDef, RequestDef, Shift,
    StationData, SupplyDef, WindowDef,
};
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::AppState;

mod restock;

pub use restock::RestockPlugin;

/// The phase machine on its own: states, the clock, and the board's messages.
///
/// Kept separate from [`RestockPlugin`], which spawns things into the world and
/// so needs meshes and materials. This one needs neither, which is what lets
/// the whole cycle be driven headlessly in tests.
pub struct ShiftPlugin;

impl Plugin for ShiftPlugin {
    fn build(&self, app: &mut App) {
        app.add_sub_state::<ShiftPhase>()
            .init_resource::<ShiftClock>()
            .add_message::<PrepOpened>()
            // Which phase the lab is in is the server's call, exactly like the
            // orders it gates. A client mirrors what it is told.
            .add_systems(
                OnEnter(ShiftPhase::Prep),
                enter_prep.run_if(in_state(ClientState::Disconnected)),
            )
            .add_systems(
                OnEnter(ShiftPhase::Service),
                open_service.run_if(in_state(ClientState::Disconnected)),
            )
            .add_systems(
                OnEnter(ShiftPhase::Debrief),
                (close_shift, dismiss_outstanding_orders)
                    .chain()
                    .run_if(in_state(ClientState::Disconnected)),
            )
            .add_systems(
                OnExit(ShiftPhase::Debrief),
                advance_shift.run_if(in_state(ClientState::Disconnected)),
            )
            .add_server_message::<ShiftClockSync>(Channel::Ordered)
            // Working the board is a request like every other player action:
            // the panel asks, the server decides.
            .add_mapped_client_message::<BeginShiftRequested>(Channel::Ordered)
            .add_mapped_client_message::<SignOffRequested>(Channel::Ordered)
            .add_mapped_client_message::<RequisitionRequested>(Channel::Ordered)
            .add_systems(
                Update,
                (
                    (
                        open_prep,
                        handle_begin_shift,
                        handle_sign_off,
                        handle_requisition,
                        tick_phase_countdown,
                        broadcast_shift_clock,
                    )
                        .chain()
                        .run_if(in_state(ClientState::Disconnected)),
                    // A client is told the phase and mirrors it. It never
                    // decides one.
                    (apply_shift_clock, tick_local_clock, mirror_phase_state)
                        .chain()
                        .run_if(in_state(ClientState::Connected)),
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// Where the lab is in its cycle.
///
/// A sub-state rather than a bare resource because the phase boundaries carry
/// exactly-once side effects — re-arming the spawner, snapshotting the tally,
/// sending the crew home, picking the forecast — and `OnEnter`/`OnExit` give
/// exactly-once for free. Hand-rolled edge detection would have to be right in
/// six places at once.
#[derive(
    Debug, Clone, Copy, Default, Eq, PartialEq, Hash, SubStates, Serialize, Deserialize,
)]
#[source(AppState = AppState::Playing)]
#[states(scoped_entities)]
pub enum ShiftPhase {
    /// No orders. Stock up, experiment, listen to the briefing.
    #[default]
    Prep,
    /// The crew are asking for things.
    Service,
    /// The shift is over. Read the report, spend your standing.
    Debrief,
}

impl ShiftPhase {
    pub fn label(self) -> &'static str {
        match self {
            ShiftPhase::Prep => "PREP",
            ShiftPhase::Service => "SERVICE",
            ShiftPhase::Debrief => "DEBRIEF",
        }
    }
}

// ---------------------------------------------------------------------------
// Difficulty
// ---------------------------------------------------------------------------

/// One shift's difficulty, resolved from the ramp and frozen for its duration.
///
/// Frozen rather than recomputed per order so the numbers cannot drift under
/// the player mid-shift, and so the whole set can be shipped to a client in one
/// snapshot for its HUD.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShiftRules {
    /// Orders that must resolve — delivered or expired — before the shift ends.
    pub quota: u32,
    pub gap_seconds: (f32, f32),
    pub patience_seconds: (f32, f32),
    pub max_active: usize,
    pub stretch_chance: f64,
    pub forecast_boost: f64,
}

impl Default for ShiftRules {
    fn default() -> Self {
        ShiftRules {
            quota: 5,
            gap_seconds: (32.0, 58.0),
            patience_seconds: (135.0, 230.0),
            max_active: 3,
            stretch_chance: 0.25,
            forecast_boost: 2.0,
        }
    }
}

impl ShiftRules {
    /// The difficulty for shift `shift` (1-based).
    ///
    /// Shift 1 must reproduce the base config exactly, or the ramp silently
    /// re-balances a game that was already tuned. Everything else tightens
    /// monotonically and stops at a cap, because an unclamped ramp only reveals
    /// itself as unplayable somewhere nobody tests.
    pub fn for_shift(base: &OrderConfig, ramp: &RampDef, shift: u32) -> ShiftRules {
        let elapsed = shift.saturating_sub(1);

        let quota = (ramp.quota_base + ramp.quota_per_shift * elapsed).min(ramp.quota_cap);

        let decay = ramp.gap_scale.powi(elapsed as i32);
        let gap_seconds = (
            (base.gap_seconds.0 * decay).max(ramp.gap_floor),
            (base.gap_seconds.1 * decay).max(ramp.gap_floor),
        );

        let patience_decay = ramp.patience_scale.powi(elapsed as i32);
        let patience_seconds = (
            (base.patience_seconds.0 * patience_decay).max(ramp.patience_floor),
            (base.patience_seconds.1 * patience_decay).max(ramp.patience_floor),
        );

        // A zero interval is a content bug, not a reason to divide by zero.
        let extra = elapsed.checked_div(ramp.max_active_every).unwrap_or(0) as usize;
        let max_active = (base.max_active + extra).min(ramp.max_active_cap);

        let stretch_chance =
            (ramp.stretch_base + ramp.stretch_step * elapsed as f64).min(ramp.stretch_cap);

        ShiftRules {
            quota,
            gap_seconds,
            patience_seconds,
            max_active,
            stretch_chance,
            forecast_boost: ramp.forecast_boost,
        }
    }
}

// ---------------------------------------------------------------------------
// The clock
// ---------------------------------------------------------------------------

/// The station's expectations for a shift, drawn at the start of prep.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForecastPick {
    pub id: String,
    /// Carried alongside the id so a client can show what to prepare for
    /// without needing the station data files.
    pub themes: Vec<String>,
    pub briefing: String,
}

/// Career tallies at a moment in time, so a shift can be reported on its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShiftSnapshot {
    pub succeeded: u32,
    pub botched: u32,
    pub reputation: i32,
    pub research_points: u32,
    pub known_recipes: usize,
}

/// What one shift actually did, as shown on the debrief board.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ShiftReport {
    pub delivered: u32,
    pub botched: u32,
    pub reputation: i32,
    pub research: u32,
    pub recipes: usize,
}

impl ShiftReport {
    /// The difference between two snapshots.
    ///
    /// Deltas rather than live totals: by the time the player reads this they
    /// may already have spent reputation at the board, and a report that
    /// rewrote itself as they spent would be worse than no report.
    pub fn between(opening: &ShiftSnapshot, closing: &ShiftSnapshot) -> ShiftReport {
        ShiftReport {
            delivered: closing.succeeded.saturating_sub(opening.succeeded),
            botched: closing.botched.saturating_sub(opening.botched),
            reputation: closing.reputation - opening.reputation,
            research: closing.research_points.saturating_sub(opening.research_points),
            recipes: closing.known_recipes.saturating_sub(opening.known_recipes),
        }
    }
}

/// Supplies bought at the debrief, waiting to land at the next prep.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requisition {
    /// Extra glassware on top of the standing target, for one shift.
    pub glassware: usize,
    /// Bring the botanist forward.
    pub produce_crate: bool,
}

/// Everything a client must be told about, coarse enough not to resend a
/// running countdown every frame. See [`ShiftClock::sync_key`].
pub type SyncKey = (u32, ShiftPhase, u32, u32, Option<String>, i32);

/// Where the lab is in the cycle, and everything a client needs to draw it.
///
/// The authority owns `State<ShiftPhase>` and mirrors it into `phase` here so
/// the whole thing can be shipped; a client receives this and mirrors `phase`
/// back into its own `NextState`. One-way on each end, which is what keeps the
/// two from fighting.
#[derive(Resource, Clone, Debug, Serialize, Deserialize)]
pub struct ShiftClock {
    /// 1-based. Shift 1 is the one you learn the lab on.
    pub shift: u32,
    pub phase: ShiftPhase,
    pub rules: ShiftRules,
    pub resolved: u32,
    /// Seconds left in the current window, or `None` for a window with no clock
    /// — which is shift 1's prep, and every phase that ends on something other
    /// than time.
    ///
    /// Deliberately an `f32` and not a `Timer`: `Timer` is not `Serialize`, and
    /// this struct is the co-op sync payload.
    pub remaining: Option<f32>,
    pub forecast: Option<ForecastPick>,
    /// Career tallies as service opened.
    pub opening: ShiftSnapshot,
    /// Career tallies as the shift closed, frozen at debrief entry.
    pub closing: ShiftSnapshot,
    pub requisition: Requisition,
    /// Prep has been entered but its opening work has not run yet.
    ///
    /// The station data files load in `Update`, so the very first
    /// `OnEnter(Prep)` fires a frame or more before there is a config to read a
    /// window length or a forecast out of. Rather than have four systems each
    /// cope with that, entering prep only raises this flag and
    /// [`open_prep`] does the work on the first frame it can.
    #[serde(skip)]
    pub pending_open: bool,
}

impl Default for ShiftClock {
    fn default() -> Self {
        ShiftClock {
            shift: 1,
            phase: ShiftPhase::Prep,
            rules: ShiftRules::default(),
            resolved: 0,
            remaining: None,
            forecast: None,
            opening: ShiftSnapshot::default(),
            closing: ShiftSnapshot::default(),
            requisition: Requisition::default(),
            pending_open: true,
        }
    }
}

impl ShiftClock {
    /// Runs the window down. Returns the phase to move to once it closes.
    ///
    /// A `None` countdown never closes on its own — that is the untimed first
    /// prep, which only the player can end. Service never ends on a clock
    /// either; it ends on the quota, in [`record_resolution`](Self::record_resolution).
    pub fn tick(&mut self, dt: f32) -> Option<ShiftPhase> {
        let remaining = self.remaining.as_mut()?;
        *remaining -= dt;
        if *remaining > 0.0 {
            return None;
        }
        // Cleared, so a second tick in the same window cannot fire the
        // transition twice while the state change is still in flight.
        self.remaining = None;
        match self.phase {
            ShiftPhase::Prep => Some(ShiftPhase::Service),
            ShiftPhase::Debrief => Some(ShiftPhase::Prep),
            ShiftPhase::Service => None,
        }
    }

    /// Runs the window down without ever transitioning.
    ///
    /// What a client does with the countdown: the number has to keep moving
    /// between snapshots, but deciding the phase is the server's job alone.
    pub fn tick_display(&mut self, dt: f32) {
        if let Some(remaining) = self.remaining.as_mut() {
            *remaining = (*remaining - dt).max(0.0);
        }
    }

    /// Books a resolved order against the quota. True when this was the one
    /// that closed the shift.
    ///
    /// Counting outside service would let an order that resolves during the
    /// debrief — a beaker already in the window when the bell went — end the
    /// next shift early.
    pub fn record_resolution(&mut self) -> bool {
        if self.phase != ShiftPhase::Service {
            return false;
        }
        self.resolved += 1;
        self.resolved == self.rules.quota
    }

    /// The themes this shift's forecast leans on, or nothing.
    pub fn forecast_themes(&self) -> &[String] {
        match &self.forecast {
            Some(pick) => &pick.themes,
            None => &[],
        }
    }

    /// Everything a client must be told, with the countdown rounded to whole
    /// seconds.
    ///
    /// `remaining` moves every frame, so change detection would push a snapshot
    /// per frame forever. Rounding means a running clock costs one small
    /// message a second, while any real change still goes out immediately.
    pub fn sync_key(&self) -> SyncKey {
        (
            self.shift,
            self.phase,
            self.resolved,
            self.rules.quota,
            self.forecast.as_ref().map(|pick| pick.id.clone()),
            self.remaining.map(|left| left.ceil() as i32).unwrap_or(-1),
        )
    }
}

// ---------------------------------------------------------------------------
// The phase machine
// ---------------------------------------------------------------------------

/// Raises the flag [`open_prep`] acts on.
///
/// Nothing else happens here because the station data files may not have landed
/// yet — this fires the moment `AppState::Playing` is entered, and
/// `promote_station_data` runs in `Update`.
fn enter_prep(mut clock: ResMut<ShiftClock>) {
    clock.phase = ShiftPhase::Prep;
    clock.resolved = 0;
    clock.remaining = None;
    clock.forecast = None;
    clock.pending_open = true;
}

/// Prep has actually opened: the config was there and the forecast is drawn.
///
/// Entering the phase is not the same moment as opening it — the station data
/// files load in `Update`, so the first `OnEnter(Prep)` of a session lands
/// before there is a config to read. Anything that needs the shift's numbers,
/// or needs the world to have finished spawning, has to hang off this rather
/// than off the transition.
#[derive(Message)]
pub struct PrepOpened;

/// Does prep's opening work on the first frame the config is available.
fn open_prep(
    clock: Option<ResMut<ShiftClock>>,
    station: Option<Res<StationData>>,
    mut radio: ResMut<RadioLog>,
    mut opened: MessageWriter<PrepOpened>,
) {
    let (Some(mut clock), Some(station)) = (clock, station) else {
        return;
    };
    if !clock.pending_open {
        return;
    }

    let windows = &station.config.windows;
    // The first prep of a session has no clock, so a chemist who has never seen
    // the lab can take it apart before anyone starts asking for anything.
    clock.remaining = if clock.shift <= 1 && windows.first_shift_untimed {
        None
    } else {
        Some(windows.prep_seconds)
    };

    // The briefing goes out over the radio rather than only onto the board, so
    // it lands the same way every other piece of station news does — and it
    // reaches a joined client for free, because the log is already
    // whole-snapshotted to clients whenever it changes.
    let roll = rand::rng().random::<f64>();
    if let Some(forecast) = draw_forecast(&station.config.forecasts, roll) {
        radio.push(RadioEntry {
            channel: "COM".to_string(),
            text: format!("Shift {} briefing: {}", clock.shift, forecast.briefing),
            good: true,
        });
        clock.forecast = Some(ForecastPick {
            id: forecast.id.clone(),
            themes: forecast.themes.clone(),
            briefing: forecast.briefing.clone(),
        });
    }

    clock.pending_open = false;
    opened.write(PrepOpened);

    info!(
        "shift {} — prep ({}), forecast {}",
        clock.shift,
        match clock.remaining {
            Some(seconds) => format!("{seconds:.0}s"),
            None => "untimed".to_string(),
        },
        clock
            .forecast
            .as_ref()
            .map(|pick| pick.id.as_str())
            .unwrap_or("none")
    );
}

/// Opens the shift: freezes this shift's difficulty and starts the clock over.
fn open_service(
    mut clock: ResMut<ShiftClock>,
    shift: Res<Shift>,
    knowledge: Option<Res<Knowledge>>,
    station: Option<Res<StationData>>,
    spawner: Option<ResMut<OrderSpawner>>,
) {
    clock.phase = ShiftPhase::Service;
    clock.resolved = 0;
    // Service ends on the quota, never on a clock.
    clock.remaining = None;

    if let Some(station) = station.as_ref() {
        clock.rules = ShiftRules::for_shift(&station.config, &station.config.ramp, clock.shift);
    }
    clock.opening = snapshot(&shift, knowledge.as_deref());

    // `generate_orders` re-rolls this timer *before* it checks `max_active`, so
    // it is always left holding a partial random remainder when generation
    // stops. Left alone, the first order of the shift would land at whatever
    // that remainder happened to be — possibly the instant the player walks out
    // of the briefing.
    if let (Some(station), Some(mut spawner)) = (station, spawner) {
        spawner.timer = Timer::from_seconds(station.config.first_order_delay, TimerMode::Once);
    }

    info!(
        "shift {} — service, {} orders at {:?}",
        clock.shift, clock.rules.quota, clock.rules.gap_seconds
    );
}

/// Freezes the report before the player can spend against it.
fn close_shift(
    mut clock: ResMut<ShiftClock>,
    shift: Res<Shift>,
    knowledge: Option<Res<Knowledge>>,
    station: Option<Res<StationData>>,
) {
    clock.phase = ShiftPhase::Debrief;
    clock.closing = snapshot(&shift, knowledge.as_deref());
    clock.remaining = Some(match station {
        Some(station) => station.config.windows.debrief_seconds,
        None => WindowDef::default().debrief_seconds,
    });

    let report = ShiftReport::between(&clock.opening, &clock.closing);
    info!(
        "shift {} closed — {} delivered, {} botched, {:+} standing",
        clock.shift, report.delivered, report.botched, report.reputation
    );
}

fn advance_shift(mut clock: ResMut<ShiftClock>) {
    clock.shift += 1;
}

fn snapshot(shift: &Shift, knowledge: Option<&Knowledge>) -> ShiftSnapshot {
    ShiftSnapshot {
        succeeded: shift.succeeded,
        botched: shift.botched,
        reputation: shift.reputation,
        research_points: knowledge.map(|k| k.research_points).unwrap_or(0),
        known_recipes: knowledge.map(|k| k.known_count()).unwrap_or(0),
    }
}

/// Sends anyone still at the counter home when the bell goes.
///
/// Deliberately writes no [`OrderResolved`] and touches no tally. The shift
/// ended; they were not ignored. A resolution here would re-enter the quota
/// count, fire a verdict line over the radio for a delivery that never
/// happened, and roll for a sample vial.
fn dismiss_outstanding_orders(
    mut commands: Commands,
    mut radio: ResMut<RadioLog>,
    mut waiting: Query<(Entity, &CrewMember, &mut CrewRoute), With<Order>>,
) {
    for (entity, member, mut route) in &mut waiting {
        commands
            .entity(entity)
            .remove::<Order>()
            .remove::<Interactable>();
        route.leave();
        radio.push(RadioEntry {
            channel: channel_for(&member.role),
            text: format!(
                "{}: shift's up. I'll put it back through next rotation.",
                member.name
            ),
            good: false,
        });
    }
}

fn tick_phase_countdown(
    time: Res<Time>,
    mut clock: ResMut<ShiftClock>,
    mut next: ResMut<NextState<ShiftPhase>>,
) {
    if let Some(phase) = clock.tick(time.delta_secs()) {
        next.set(phase);
    }
}

// ---------------------------------------------------------------------------
// The board
// ---------------------------------------------------------------------------

/// Start the shift now, without waiting out the rest of prep.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct BeginShiftRequested {
    #[entities]
    pub board: Entity,
}

/// Close the debrief and go straight to the next prep.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct SignOffRequested {
    #[entities]
    pub board: Entity,
}

/// Spend standing on next shift's supplies.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct RequisitionRequested {
    #[entities]
    pub board: Entity,
    pub kind: RequisitionKind,
}

/// Books a purchase, or does nothing at all.
///
/// Returns whether it went through, so the caller never has to guess: a
/// half-applied requisition that took the standing and delivered nothing is
/// the worst outcome available here.
pub fn apply_requisition(
    shift: &mut Shift,
    clock: &mut ShiftClock,
    knowledge: &mut Knowledge,
    supply: &SupplyDef,
    kind: RequisitionKind,
) -> bool {
    if !can_afford(shift.reputation, kind) {
        return false;
    }
    // Buying a second crate would charge again for a flag that is already set.
    // Glassware stacks, so only this one needs the guard.
    if kind == RequisitionKind::ProduceCrate && clock.requisition.produce_crate {
        return false;
    }
    shift.reputation -= kind.cost();

    match kind {
        RequisitionKind::Glassware => {
            clock.requisition.glassware += supply.requisition_glassware_bonus;
        }
        RequisitionKind::ProduceCrate => {
            clock.requisition.produce_crate = true;
        }
        // Lands immediately rather than next prep, because research is the one
        // thing you can spend during the debrief itself.
        RequisitionKind::ResearchGrant => {
            knowledge.award_research(RESEARCH_GRANT);
        }
    }
    true
}

/// What a research grant is worth.
pub const RESEARCH_GRANT: u32 = 2;

#[allow(clippy::too_many_arguments)]
fn handle_requisition(
    mut requests: MessageReader<FromClient<RequisitionRequested>>,
    boards: Query<&Machine>,
    phase: Res<State<ShiftPhase>>,
    station: Option<Res<StationData>>,
    mut shift: ResMut<Shift>,
    mut clock: ResMut<ShiftClock>,
    mut knowledge: ResMut<Knowledge>,
    mut radio: ResMut<RadioLog>,
) {
    let Some(station) = station else {
        return;
    };
    for request in requests.read() {
        // Only at the debrief. Requisitioning mid-shift would let a player
        // spend standing they are still in the middle of earning.
        if *phase.get() != ShiftPhase::Debrief || !is_board(request.board, &boards) {
            continue;
        }
        if !apply_requisition(
            &mut shift,
            &mut clock,
            &mut knowledge,
            &station.config.supply,
            request.kind,
        ) {
            continue;
        }
        radio.push(RadioEntry {
            channel: "CGO".to_string(),
            text: format!("Requisition logged: {}.", request.kind.label()),
            good: true,
        });
    }
}

/// Is this entity actually the shift board?
///
/// A message names the machine it acts on, and a client is free to name any
/// entity at all. Checking the kind here is what stops a crafted message from
/// starting a shift by pointing at the grinder.
fn is_board(entity: Entity, boards: &Query<&Machine>) -> bool {
    boards
        .get(entity)
        .is_ok_and(|machine| machine.kind == MachineKind::ShiftBoard)
}

fn handle_begin_shift(
    mut requests: MessageReader<FromClient<BeginShiftRequested>>,
    boards: Query<&Machine>,
    phase: Res<State<ShiftPhase>>,
    mut next: ResMut<NextState<ShiftPhase>>,
) {
    for request in requests.read() {
        if *phase.get() != ShiftPhase::Prep || !is_board(request.board, &boards) {
            continue;
        }
        next.set(ShiftPhase::Service);
    }
}

fn handle_sign_off(
    mut requests: MessageReader<FromClient<SignOffRequested>>,
    boards: Query<&Machine>,
    phase: Res<State<ShiftPhase>>,
    mut next: ResMut<NextState<ShiftPhase>>,
) {
    for request in requests.read() {
        if *phase.get() != ShiftPhase::Debrief || !is_board(request.board, &boards) {
            continue;
        }
        next.set(ShiftPhase::Prep);
    }
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/// Kept apart from `save.ron`, which `KnowledgePlugin` owns.
///
/// `SaveData` is deliberately both the disk format *and* the `KnowledgeSync`
/// payload, built from `Knowledge` alone. Threading the shift tally through it
/// would either couple knowledge to orders or make the sync carry data it does
/// not want.
const PROGRESS_PATH: &str = "progress.ron";

/// Reading and writing `progress.ron`.
///
/// A plugin of its own rather than part of [`ShiftPlugin`] because it touches
/// the disk, and [`ShiftPlugin`] is what the headless tests drive — folded
/// together, every test run would read and write the player's real save and the
/// tests would pollute each other through it.
pub struct ProgressPlugin;

impl Plugin for ProgressPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            OnEnter(AppState::Playing),
            load_progress.run_if(in_state(ClientState::Disconnected)),
        )
        .add_systems(
            Update,
            persist_progress
                .run_if(in_state(AppState::Playing))
                .run_if(in_state(ClientState::Disconnected)),
        );
    }
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Eq)]
struct ProgressSave {
    /// The shift to start on next launch.
    #[serde(default)]
    shift: u32,
    #[serde(default)]
    reputation: i32,
    #[serde(default)]
    succeeded: u32,
    #[serde(default)]
    botched: u32,
}

/// Restores the career on launch.
///
/// The phase, the quota progress, the countdown, the forecast and any unspent
/// requisition are all deliberately absent: resuming always drops you into prep
/// of the next shift. Restoring a half-finished service would put the player in
/// an empty room with a partly-met quota and a forecast they never heard.
fn load_progress(mut clock: ResMut<ShiftClock>, mut shift: ResMut<Shift>) {
    if !std::path::Path::new(PROGRESS_PATH).exists() {
        return;
    }
    // A corrupt save should cost the player their progress, not the session.
    let save = match std::fs::read_to_string(PROGRESS_PATH)
        .map(|text| ron::from_str::<ProgressSave>(&text))
    {
        Ok(Ok(save)) => save,
        Ok(Err(error)) => {
            warn!("ignoring unreadable {PROGRESS_PATH}: {error}");
            return;
        }
        Err(error) => {
            warn!("could not read {PROGRESS_PATH}: {error}");
            return;
        }
    };

    clock.shift = save.shift.max(1);
    shift.succeeded = save.succeeded;
    shift.botched = save.botched;
    shift.reputation = save.reputation;
    info!(
        "resuming at shift {} with {:+} standing",
        clock.shift, shift.reputation
    );
}

fn persist_progress(
    clock: Res<ShiftClock>,
    shift: Res<Shift>,
    mut written: Local<Option<ProgressSave>>,
) {
    let save = ProgressSave {
        shift: clock.shift,
        reputation: shift.reputation,
        succeeded: shift.succeeded,
        botched: shift.botched,
    };
    // Compared against what is actually on disk rather than gated on
    // `is_changed()`: the countdown mutates `ShiftClock` every frame, so change
    // detection here would rewrite the save sixty times a second for a file
    // that only moves a handful of times a shift.
    if written.as_ref() == Some(&save) {
        return;
    }
    let Ok(text) = ron::ser::to_string_pretty(&save, default()) else {
        return;
    };
    *written = Some(save);
    if let Err(error) = std::fs::write(PROGRESS_PATH, text) {
        warn!("could not write {PROGRESS_PATH}: {error}");
    }
}

// ---------------------------------------------------------------------------
// Co-op
// ---------------------------------------------------------------------------

/// The whole clock, pushed to clients.
///
/// `ShiftClock` is a resource, and replicon does not replicate resources — so
/// it goes as a snapshot, the same shape as `ShiftSync`, `KnowledgeSync` and
/// `RadioSync`. Kept separate from `ShiftSync` because the two move at very
/// different rates: the tally changes per order, this changes per second.
#[derive(Message, Serialize, Deserialize, Clone)]
pub struct ShiftClockSync(ShiftClock);

fn broadcast_shift_clock(
    clock: Res<ShiftClock>,
    mut last: Local<Option<SyncKey>>,
    mut outgoing: MessageWriter<ToClients<ShiftClockSync>>,
) {
    // Change detection is useless here: `remaining` moves every frame, so
    // `is_changed()` would push a snapshot per frame forever. The key rounds
    // the countdown to whole seconds instead.
    let key = clock.sync_key();
    if last.as_ref() == Some(&key) {
        return;
    }
    *last = Some(key);

    outgoing.write(ToClients {
        // Never the host: it holds the authoritative copy, and echoing to it
        // would overwrite the original with a copy of itself.
        targets: SendTargets::CLIENTS_ONLY,
        message: ShiftClockSync(clock.clone()),
    });
}

fn apply_shift_clock(mut clock: ResMut<ShiftClock>, mut incoming: MessageReader<ShiftClockSync>) {
    for sync in incoming.read() {
        *clock = sync.0.clone();
    }
}

/// Smooths the client's countdown between snapshots.
///
/// Without this the HUD clock would visibly step once a second. It is allowed
/// to drift, because the next snapshot corrects it and nothing on a client
/// acts on the number.
fn tick_local_clock(time: Res<Time>, mut clock: ResMut<ShiftClock>) {
    clock.tick_display(time.delta_secs());
}

/// Follows the server's phase.
///
/// The asymmetry is deliberate: on the authority `State<ShiftPhase>` is primary
/// and `clock.phase` mirrors it so it can be shipped; on a client the clock
/// arrives over the wire and is primary. One-way on each end, so the two can
/// never fight over which is right.
fn mirror_phase_state(
    clock: Res<ShiftClock>,
    current: Res<State<ShiftPhase>>,
    mut next: ResMut<NextState<ShiftPhase>>,
) {
    if *current.get() != clock.phase {
        next.set(clock.phase);
    }
}

/// Books resolved orders against the quota and closes the shift on the last.
///
/// Registered inside `OrderPlugin`'s chain rather than here so it is guaranteed
/// to run after all three resolution paths in the same frame they write.
pub fn count_resolved_orders(
    mut clock: ResMut<ShiftClock>,
    mut resolved: MessageReader<OrderResolved>,
    mut next: ResMut<NextState<ShiftPhase>>,
) {
    for _ in resolved.read() {
        if clock.record_resolution() {
            next.set(ShiftPhase::Debrief);
        }
    }
}

// ---------------------------------------------------------------------------
// Forecast weighting
// ---------------------------------------------------------------------------

/// Draws the shift the station is expecting. `roll` is in `[0, 1)`.
///
/// Weighted so a quiet shift can be made rarer than a busy one without having
/// to write it out several times in the data file.
pub fn draw_forecast(forecasts: &[ForecastDef], roll: f64) -> Option<&ForecastDef> {
    let total: f64 = forecasts.iter().map(|forecast| forecast.weight.max(0.0)).sum();
    if total <= 0.0 {
        return None;
    }
    let mut cursor = roll.clamp(0.0, 1.0) * total;
    for forecast in forecasts {
        cursor -= forecast.weight.max(0.0);
        if cursor < 0.0 {
            return Some(forecast);
        }
    }
    forecasts.last()
}

/// Picks a request from `pool`, leaning toward whatever the shift was briefed
/// for. `roll` is in `[0, 1)`.
///
/// This layers on top of the knowledge gate rather than replacing it: `pool`
/// has already been filtered to things the chemist can plausibly make, so a
/// "burns" forecast with no kelotane in the book still asks for something
/// reachable. The forecast leans on the pool; it never chooses for it.
///
/// Takes the roll rather than an `Rng` so the bias can be asserted exactly
/// instead of sampled.
pub fn weighted_pick<'a>(
    pool: &[&'a RequestDef],
    themes: &[String],
    boost: f64,
    roll: f64,
) -> Option<&'a RequestDef> {
    if pool.is_empty() {
        return None;
    }

    let weight = |request: &RequestDef| {
        if !themes.is_empty() && request.themes.iter().any(|theme| themes.contains(theme)) {
            1.0 + boost
        } else {
            1.0
        }
    };

    let total: f64 = pool.iter().map(|request| weight(request)).sum();
    let mut cursor = roll.clamp(0.0, 1.0) * total;
    for request in pool {
        cursor -= weight(request);
        if cursor < 0.0 {
            return Some(request);
        }
    }
    // Only reachable on float slop at roll ~= 1.0.
    pool.last().copied()
}

// ---------------------------------------------------------------------------
// Supply
// ---------------------------------------------------------------------------

/// How many pieces of glassware cargo brings this prep.
///
/// Supply is a function of the deficit, never a flat crate. That is what makes
/// the lab impossible to starve — every hand-off over the counter takes the
/// glassware with it, and nothing else replaces it — and equally impossible to
/// flood: a bench still covered in beakers means no delivery arrives at all.
pub fn restock_order(live: usize, target: usize, crate_max: usize) -> usize {
    target.saturating_sub(live).min(crate_max)
}

/// Splits a crate into (plain beakers, large beakers), roughly matching the mix
/// the lab starts the game with.
pub fn crate_contents(count: usize, large_every: usize) -> (usize, usize) {
    let large = count / large_every.max(1);
    (count - large, large)
}

// ---------------------------------------------------------------------------
// Requisition
// ---------------------------------------------------------------------------

/// Something bought at the debrief with the standing you earned.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RequisitionKind {
    Glassware,
    ProduceCrate,
    ResearchGrant,
}

impl RequisitionKind {
    pub const ALL: [RequisitionKind; 3] = [
        RequisitionKind::Glassware,
        RequisitionKind::ProduceCrate,
        RequisitionKind::ResearchGrant,
    ];

    /// In reputation.
    pub fn cost(self) -> i32 {
        match self {
            RequisitionKind::Glassware => 3,
            RequisitionKind::ProduceCrate => 4,
            RequisitionKind::ResearchGrant => 6,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RequisitionKind::Glassware => "Glassware",
            RequisitionKind::ProduceCrate => "Produce crate",
            RequisitionKind::ResearchGrant => "Research grant",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            RequisitionKind::Glassware => "Cargo stock the lab deeper next shift",
            RequisitionKind::ProduceCrate => "Botany come early with the next haul",
            RequisitionKind::ResearchGrant => "Two research points, right now",
        }
    }
}

/// Standing is what you cash in for supplies.
///
/// Debt is allowed to exist — a bad shift should sting — but it cannot be
/// spent, or a run that went badly would dig itself further under.
pub fn can_afford(reputation: i32, kind: RequisitionKind) -> bool {
    reputation >= kind.cost()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> OrderConfig {
        ron::from_str(include_str!("../../assets/data/station.orders.ron"))
            .expect("order data should parse")
    }

    fn request(themes: &[&str]) -> RequestDef {
        RequestDef {
            reagent: "kelotane".into(),
            amounts: vec![20],
            plea: String::new(),
            themes: themes.iter().map(|theme| theme.to_string()).collect(),
        }
    }

    // -- the ramp -----------------------------------------------------------

    #[test]
    fn shift_one_matches_the_base_config() {
        // The ramp must be a no-op on the first shift, or it silently
        // re-balances numbers that were already tuned by hand.
        let base = config();
        let rules = ShiftRules::for_shift(&base, &base.ramp, 1);

        assert_eq!(rules.gap_seconds, base.gap_seconds);
        assert_eq!(rules.patience_seconds, base.patience_seconds);
        assert_eq!(rules.max_active, base.max_active);
        assert_eq!(rules.stretch_chance, base.ramp.stretch_base);
        assert_eq!(rules.quota, base.ramp.quota_base);
    }

    #[test]
    fn the_ramp_only_ever_tightens() {
        let base = config();
        let mut previous = ShiftRules::for_shift(&base, &base.ramp, 1);

        for shift in 2..=30 {
            let rules = ShiftRules::for_shift(&base, &base.ramp, shift);
            assert!(rules.quota >= previous.quota, "quota fell at shift {shift}");
            assert!(
                rules.gap_seconds.0 <= previous.gap_seconds.0
                    && rules.gap_seconds.1 <= previous.gap_seconds.1,
                "orders got further apart at shift {shift}"
            );
            assert!(
                rules.patience_seconds.0 <= previous.patience_seconds.0,
                "the crew got more patient at shift {shift}"
            );
            assert!(
                rules.max_active >= previous.max_active,
                "the counter got quieter at shift {shift}"
            );
            assert!(
                rules.stretch_chance >= previous.stretch_chance,
                "stretch orders got rarer at shift {shift}"
            );
            previous = rules;
        }
    }

    #[test]
    fn the_ramp_stops_at_its_caps() {
        // Unclamped, shift 50 is a gap of zero seconds and a counter nobody can
        // work. That only shows up for a player who got that far.
        let base = config();
        let ramp = &base.ramp;
        let rules = ShiftRules::for_shift(&base, ramp, 50);

        assert_eq!(rules.quota, ramp.quota_cap);
        assert_eq!(rules.max_active, ramp.max_active_cap);
        assert!((rules.stretch_chance - ramp.stretch_cap).abs() < f64::EPSILON);
        assert!(rules.gap_seconds.0 >= ramp.gap_floor);
        assert!(rules.patience_seconds.0 >= ramp.patience_floor);
        // The floor must not invert the range.
        assert!(rules.gap_seconds.0 <= rules.gap_seconds.1);
        assert!(rules.patience_seconds.0 <= rules.patience_seconds.1);
    }

    // -- the clock ----------------------------------------------------------

    fn clock(phase: ShiftPhase, remaining: Option<f32>) -> ShiftClock {
        ShiftClock {
            phase,
            remaining,
            ..default()
        }
    }

    #[test]
    fn an_untimed_prep_never_starts_on_its_own() {
        // Shift 1. The only way out is the player pressing the button.
        let mut clock = clock(ShiftPhase::Prep, None);
        for _ in 0..3600 {
            assert_eq!(clock.tick(1.0), None);
        }
    }

    #[test]
    fn a_timed_prep_opens_service_when_the_window_closes() {
        let mut clock = clock(ShiftPhase::Prep, Some(2.0));
        assert_eq!(clock.tick(1.0), None);
        assert_eq!(clock.tick(1.5), Some(ShiftPhase::Service));
    }

    #[test]
    fn a_debrief_rolls_into_the_next_prep() {
        let mut clock = clock(ShiftPhase::Debrief, Some(1.0));
        assert_eq!(clock.tick(1.5), Some(ShiftPhase::Prep));
    }

    #[test]
    fn service_never_ends_on_a_clock() {
        // Even with a countdown set, service only ends on the quota. A shift
        // that ended mid-order would strand the crew member at the counter.
        let mut clock = clock(ShiftPhase::Service, Some(1.0));
        assert_eq!(clock.tick(90.0), None);
    }

    #[test]
    fn a_closed_window_clears_its_countdown() {
        // The state change takes a frame to land, so the window has to stop
        // firing the moment it closes or it transitions twice.
        let mut clock = clock(ShiftPhase::Prep, Some(0.5));
        assert_eq!(clock.tick(1.0), Some(ShiftPhase::Service));
        assert_eq!(clock.remaining, None);
        assert_eq!(clock.tick(1.0), None);
    }

    // -- the quota ----------------------------------------------------------

    #[test]
    fn the_shift_ends_on_the_quota_and_not_before() {
        let mut clock = clock(ShiftPhase::Service, None);
        clock.rules.quota = 3;
        assert!(!clock.record_resolution());
        assert!(!clock.record_resolution());
        assert!(clock.record_resolution());
    }

    #[test]
    fn two_orders_landing_in_one_frame_still_only_end_the_shift_once() {
        // Both delivery routes run in the same frame, so a double resolution is
        // ordinary. A second `true` would push a second phase transition.
        let mut clock = clock(ShiftPhase::Service, None);
        clock.rules.quota = 1;
        assert!(clock.record_resolution());
        assert!(!clock.record_resolution());
    }

    #[test]
    fn resolutions_outside_service_are_not_counted() {
        // A beaker already sitting in the window when the bell goes resolves
        // during the debrief. Counting it would end the next shift early.
        let mut clock = clock(ShiftPhase::Debrief, None);
        clock.rules.quota = 1;
        assert!(!clock.record_resolution());
        assert_eq!(clock.resolved, 0);
    }

    // -- forecast weighting -------------------------------------------------

    #[test]
    fn the_forecast_leans_on_the_pool_without_replacing_it() {
        // Weights are 1 / 3 / 1: the boosted request owns [0.2, 0.8) of the
        // roll space, so a roll that would have landed on the third request
        // uniformly lands on the boosted one instead.
        let plain = request(&[]);
        let burns = request(&["burns"]);
        let other = request(&[]);
        let pool = [&plain, &burns, &other];
        let themes = vec!["burns".to_string()];

        assert_eq!(
            weighted_pick(&pool, &themes, 2.0, 0.7).map(|r| r as *const _),
            Some(&burns as *const _)
        );
        // Unthemed requests must remain reachable — a forecast is a lean, not
        // a filter.
        assert_eq!(
            weighted_pick(&pool, &themes, 2.0, 0.05).map(|r| r as *const _),
            Some(&plain as *const _)
        );
        assert_eq!(
            weighted_pick(&pool, &themes, 2.0, 0.95).map(|r| r as *const _),
            Some(&other as *const _)
        );
    }

    #[test]
    fn no_forecast_is_a_uniform_pick() {
        // With no themes the distribution has to be exactly what it was before
        // forecasts existed.
        let a = request(&["burns"]);
        let b = request(&["trauma"]);
        let pool = [&a, &b];

        assert_eq!(
            weighted_pick(&pool, &[], 2.0, 0.4).map(|r| r as *const _),
            Some(&a as *const _)
        );
        assert_eq!(
            weighted_pick(&pool, &[], 2.0, 0.6).map(|r| r as *const _),
            Some(&b as *const _)
        );
    }

    #[test]
    fn an_empty_pool_picks_nothing() {
        assert!(weighted_pick(&[], &[], 2.0, 0.5).is_none());
    }

    #[test]
    fn every_forecast_is_reachable() {
        // A forecast that no roll can land on is content nobody will ever see.
        let config = config();
        let mut seen = std::collections::HashSet::new();
        for step in 0..1000 {
            if let Some(forecast) = draw_forecast(&config.forecasts, step as f64 / 1000.0) {
                seen.insert(forecast.id.clone());
            }
        }
        assert_eq!(seen.len(), config.forecasts.len());
    }

    #[test]
    fn a_heavier_forecast_is_drawn_more_often() {
        let forecasts = vec![
            ForecastDef {
                id: "rare".into(),
                themes: vec![],
                weight: 1.0,
                briefing: "rare".into(),
            },
            ForecastDef {
                id: "common".into(),
                themes: vec![],
                weight: 3.0,
                briefing: "common".into(),
            },
        ];
        // "rare" owns [0, 0.25), "common" the rest.
        assert_eq!(draw_forecast(&forecasts, 0.1).unwrap().id, "rare");
        assert_eq!(draw_forecast(&forecasts, 0.5).unwrap().id, "common");
    }

    #[test]
    fn a_prep_briefs_the_shift_over_the_radio() {
        // The briefing is what turns prep from idle time into a decision, and
        // it reaches a joined client only because the radio log already syncs.
        let mut app = phase_app();
        app.update();

        let clock = app.world().resource::<ShiftClock>();
        let pick = clock.forecast.clone().expect("prep should draw a forecast");
        assert!(
            app.world()
                .resource::<RadioLog>()
                .entries
                .iter()
                .any(|entry| entry.text.contains(&pick.briefing)),
            "the briefing never reached the feed"
        );
    }

    #[test]
    fn each_shift_is_briefed_afresh() {
        let mut app = phase_app();
        app.update();
        move_to(&mut app, ShiftPhase::Service);
        assert!(app.world().resource::<ShiftClock>().forecast.is_some());

        move_to(&mut app, ShiftPhase::Debrief);
        move_to(&mut app, ShiftPhase::Prep);
        app.update();

        assert!(
            app.world().resource::<ShiftClock>().forecast.is_some(),
            "shift 2 opened with no briefing"
        );
    }

    // -- supply -------------------------------------------------------------

    #[test]
    fn a_full_lab_gets_no_delivery() {
        // Otherwise every prep adds glassware and the benches silt up.
        assert_eq!(restock_order(6, 6, 4), 0);
        assert_eq!(restock_order(9, 6, 4), 0);
    }

    #[test]
    fn the_lab_cannot_be_starved() {
        // Every counter hand-off despawns the container and nothing else makes
        // beakers, so any deficit at all has to draw a delivery.
        for live in 0..6 {
            assert!(restock_order(live, 6, 4) >= 1, "no delivery with {live} left");
        }
    }

    #[test]
    fn an_empty_lab_gets_at_most_a_crate() {
        assert_eq!(restock_order(0, 6, 4), 4);
    }

    #[test]
    fn a_crate_is_mostly_plain_beakers() {
        assert_eq!(crate_contents(4, 3), (3, 1));
        assert_eq!(crate_contents(1, 3), (1, 0));
        // A zero divisor would be a content bug, not a panic.
        assert_eq!(crate_contents(3, 0), (0, 3));
    }

    // -- requisition --------------------------------------------------------

    #[test]
    fn you_cannot_requisition_on_credit() {
        for kind in RequisitionKind::ALL {
            assert!(!can_afford(kind.cost() - 1, kind));
            assert!(can_afford(kind.cost(), kind));
            assert!(!can_afford(-10, kind));
        }
    }

    // -- the report ---------------------------------------------------------

    #[test]
    fn the_debrief_reports_this_shift_and_not_the_career() {
        let opening = ShiftSnapshot {
            succeeded: 10,
            botched: 4,
            reputation: 7,
            research_points: 10,
            known_recipes: 5,
        };
        let closing = ShiftSnapshot {
            succeeded: 14,
            botched: 5,
            reputation: 12,
            research_points: 14,
            known_recipes: 6,
        };
        let report = ShiftReport::between(&opening, &closing);

        assert_eq!(report.delivered, 4);
        assert_eq!(report.botched, 1);
        assert_eq!(report.reputation, 5);
        assert_eq!(report.research, 4);
        assert_eq!(report.recipes, 1);
    }

    #[test]
    fn a_shift_that_lost_standing_reports_it_as_a_loss() {
        let opening = ShiftSnapshot {
            reputation: 4,
            ..default()
        };
        let closing = ShiftSnapshot {
            reputation: -3,
            ..default()
        };
        assert_eq!(ShiftReport::between(&opening, &closing).reputation, -7);
    }

    // -- sync ---------------------------------------------------------------

    #[test]
    fn a_running_countdown_costs_one_message_a_second() {
        let mut clock = clock(ShiftPhase::Prep, Some(10.0));
        let key = clock.sync_key();
        clock.remaining = Some(9.4);
        assert_eq!(clock.sync_key(), key, "sub-second ticks should not resend");
        clock.remaining = Some(8.9);
        assert_ne!(clock.sync_key(), key, "a whole second should resend");
    }

    #[test]
    fn a_phase_change_goes_out_at_once() {
        let clock = clock(ShiftPhase::Prep, Some(10.0));
        let mut moved = clock.clone();
        moved.phase = ShiftPhase::Service;
        assert_ne!(clock.sync_key(), moved.sync_key());
    }

    // -- data integrity -----------------------------------------------------

    #[test]
    fn every_request_carries_a_theme() {
        // An untagged request can never be forecast, so it would quietly sit
        // outside the whole briefing system.
        for request in &config().requests {
            assert!(
                !request.themes.is_empty(),
                "request for '{}' has no theme, so no forecast can reach it",
                request.reagent
            );
        }
    }

    #[test]
    fn every_forecast_theme_is_asked_for_by_some_request() {
        // A forecast naming a theme nothing carries biases nothing: the
        // briefing promises a shift that never arrives, and there is no way to
        // notice from inside the game.
        let config = config();
        for forecast in &config.forecasts {
            for theme in &forecast.themes {
                assert!(
                    config
                        .requests
                        .iter()
                        .any(|request| request.themes.contains(theme)),
                    "forecast '{}' promises '{theme}', which no request carries",
                    forecast.id
                );
            }
        }
    }

    #[test]
    fn every_forecast_has_a_briefing_and_a_positive_weight() {
        for forecast in &config().forecasts {
            assert!(
                !forecast.briefing.trim().is_empty(),
                "forecast '{}' briefs nothing, so prep opens in silence",
                forecast.id
            );
            assert!(
                forecast.weight > 0.0,
                "forecast '{}' can never be drawn",
                forecast.id
            );
        }
    }

    #[test]
    fn there_is_a_forecast_to_draw() {
        assert!(
            !config().forecasts.is_empty(),
            "with no forecasts every prep opens with no briefing"
        );
    }

    #[test]
    fn the_glassware_courier_is_on_the_crew_roster() {
        // Missing, the restock is skipped with a warning, which looks exactly
        // like the lab simply never getting any glassware back.
        let supply = config().supply;
        let roster: Vec<crate::crew::CrewDef> =
            ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();
        assert!(
            roster.iter().any(|member| member.name == supply.courier),
            "no crew member named '{}' to bring glassware",
            supply.courier
        );
    }

    // -- the phase machine, headless ----------------------------------------

    use crate::crew::{CrewPhase, CrewRoute};
    use crate::orders::Outcome;
    use bevy::state::app::StatesPlugin;
    use bevy_replicon::test_app::ServerTestAppExt;
    use chem_sim::{ReagentId, Units};

    fn chemistry() -> chem_sim::ChemData {
        chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap()
    }

    fn station() -> StationData {
        StationData {
            crew: ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap(),
            config: config(),
        }
    }

    /// Enough app to run the phase machine: states and resources, no renderer,
    /// no crew walking, no assets.
    ///
    /// Replicon comes along because the phase systems are authority-gated on
    /// `ClientState` exactly like the orders they gate. Its default is
    /// `Disconnected`, which is singleplayer, so this is the real code path
    /// rather than a stand-in.
    fn phase_app() -> App {
        let mut app = App::new();
        app.add_plugins((
            MinimalPlugins,
            StatesPlugin,
            RepliconPlugins.set(ServerPlugin::new(PostUpdate)),
        ))
        .init_state::<AppState>()
        .add_plugins(ShiftPlugin)
        .init_resource::<Shift>()
        .init_resource::<RadioLog>()
        .add_message::<OrderResolved>()
        .insert_resource(Knowledge::new(&chemistry()))
        .insert_resource(station())
        .insert_resource(OrderSpawner {
            timer: Timer::from_seconds(999.0, TimerMode::Once),
        })
        .add_systems(Update, count_resolved_orders);
        app.finish();

        // `ShiftPhase` only exists once `AppState` reaches `Playing`, so the
        // sub-state has to be brought into being before anything can be asked
        // of it.
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);
        app.update();
        app
    }

    fn phase_of(app: &App) -> ShiftPhase {
        *app.world().resource::<State<ShiftPhase>>().get()
    }

    fn move_to(app: &mut App, phase: ShiftPhase) {
        app.world_mut()
            .resource_mut::<NextState<ShiftPhase>>()
            .set(phase);
        app.update();
    }

    #[test]
    fn a_session_opens_in_prep() {
        // The player gets the lab to themselves before anyone asks for
        // anything. That is the whole feature.
        let app = phase_app();
        assert_eq!(phase_of(&app), ShiftPhase::Prep);
        assert_eq!(app.world().resource::<ShiftClock>().shift, 1);
    }

    #[test]
    fn the_first_prep_of_a_session_has_no_clock() {
        // `first_shift_untimed` — a chemist who has never seen the lab should
        // not be racing a countdown while they find the dispenser.
        let mut app = phase_app();
        app.update();
        let clock = app.world().resource::<ShiftClock>();
        assert_eq!(clock.remaining, None);
        assert!(!clock.pending_open, "prep never opened");
    }

    #[test]
    fn later_preps_are_timed() {
        let mut app = phase_app();
        app.update();

        move_to(&mut app, ShiftPhase::Service);
        move_to(&mut app, ShiftPhase::Debrief);
        move_to(&mut app, ShiftPhase::Prep);
        app.update();

        let clock = app.world().resource::<ShiftClock>();
        assert_eq!(clock.shift, 2, "signing off should advance the shift");
        let remaining = clock.remaining.expect("prep after the first is timed");
        // A frame or two of countdown has already run by now.
        assert!(
            (remaining - config().windows.prep_seconds).abs() < 1.0,
            "prep opened with {remaining}s"
        );
    }

    #[test]
    fn the_order_spawner_is_re_armed_when_service_opens() {
        // `generate_orders` re-rolls its timer before it checks `max_active`,
        // so it always stops holding a partial remainder. Without this the
        // first order of a shift lands at a stale random offset — possibly the
        // instant the player walks out of the briefing.
        let mut app = phase_app();
        app.world_mut()
            .resource_mut::<OrderSpawner>()
            .timer
            .tick(std::time::Duration::from_secs_f32(998.5));

        move_to(&mut app, ShiftPhase::Service);

        let remaining = app
            .world()
            .resource::<OrderSpawner>()
            .timer
            .remaining_secs();
        assert!(
            (remaining - config().first_order_delay).abs() < 0.001,
            "service opened with {remaining}s on a stale clock"
        );
    }

    #[test]
    fn service_freezes_this_shifts_difficulty() {
        let mut app = phase_app();
        app.world_mut().resource_mut::<ShiftClock>().shift = 4;
        move_to(&mut app, ShiftPhase::Service);

        let base = config();
        let expected = ShiftRules::for_shift(&base, &base.ramp, 4);
        assert_eq!(app.world().resource::<ShiftClock>().rules, expected);
    }

    #[test]
    fn the_quota_closes_the_shift() {
        let mut app = phase_app();
        move_to(&mut app, ShiftPhase::Service);
        app.world_mut().resource_mut::<ShiftClock>().rules.quota = 2;

        resolve_one(&mut app);
        assert_eq!(phase_of(&app), ShiftPhase::Service, "closed a shift early");

        resolve_one(&mut app);
        assert_eq!(phase_of(&app), ShiftPhase::Debrief);
    }

    fn resolve_one(app: &mut App) {
        app.world_mut().write_message(OrderResolved {
            name: "Dr. Vance".into(),
            role: "Medical".into(),
            reagent: ReagentId(0),
            outcome: Outcome::Success,
        });
        app.update();
        // `NextState` written during `Update` is applied at the top of the
        // following frame, so the phase change needs one more turn of the crank.
        app.update();
    }

    #[test]
    fn the_debrief_sends_waiting_crew_home_without_a_penalty() {
        let mut app = phase_app();
        move_to(&mut app, ShiftPhase::Service);

        let crew = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Dr. Vance".into(),
                    role: "Medical".into(),
                },
                Order {
                    reagent: ReagentId(0),
                    amount: Units::whole(20),
                    plea: "please".into(),
                    timer: Timer::from_seconds(60.0, TimerMode::Once),
                },
                Interactable::new("Dr. Vance"),
                CrewRoute::arrival(0.0),
            ))
            .id();

        move_to(&mut app, ShiftPhase::Debrief);

        let world = app.world();
        assert!(
            world.get::<Order>(crew).is_none(),
            "the request should be withdrawn, not left hanging"
        );
        assert!(world.get::<Interactable>(crew).is_none());
        assert_eq!(
            world.get::<CrewRoute>(crew).unwrap().phase,
            CrewPhase::Leaving
        );

        // They were not ignored — the shift ended. Charging them as a failure
        // would punish the player for the quota's own boundary.
        let shift = world.resource::<Shift>();
        assert_eq!(shift.botched, 0);
        assert_eq!(shift.reputation, 0);
    }

    #[test]
    fn dismissed_crew_do_not_count_against_the_next_shifts_quota() {
        // `dismiss_outstanding_orders` deliberately writes no `OrderResolved`.
        // One that leaked through would be booked against the next shift, and
        // would fire a radio verdict for a delivery that never happened.
        let mut app = phase_app();
        move_to(&mut app, ShiftPhase::Service);
        app.world_mut().spawn((
            CrewMember {
                name: "Warden Bex".into(),
                role: "Security".into(),
            },
            Order {
                reagent: ReagentId(0),
                amount: Units::whole(20),
                plea: "please".into(),
                timer: Timer::from_seconds(60.0, TimerMode::Once),
            },
            CrewRoute::arrival(0.0),
        ));

        move_to(&mut app, ShiftPhase::Debrief);
        move_to(&mut app, ShiftPhase::Prep);
        move_to(&mut app, ShiftPhase::Service);
        app.update();

        assert_eq!(app.world().resource::<ShiftClock>().resolved, 0);
    }

    // -- the board ----------------------------------------------------------

    fn board(app: &mut App) -> Entity {
        app.world_mut()
            .spawn(Machine::new(MachineKind::ShiftBoard))
            .id()
    }

    #[test]
    fn the_board_starts_the_shift() {
        let mut app = phase_app();
        let board = board(&mut app);

        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: BeginShiftRequested { board },
        });
        app.update();
        app.update();

        assert_eq!(phase_of(&app), ShiftPhase::Service);
    }

    #[test]
    fn only_the_board_can_start_a_shift() {
        // A message names the machine it acts on, and a client is free to name
        // any entity at all. Without the kind check a crafted message could
        // start a shift by pointing at the grinder.
        let mut app = phase_app();
        let grinder = app
            .world_mut()
            .spawn(Machine::new(MachineKind::Grinder))
            .id();

        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: BeginShiftRequested { board: grinder },
        });
        app.update();
        app.update();

        assert_eq!(phase_of(&app), ShiftPhase::Prep);
    }

    #[test]
    fn the_board_only_starts_a_shift_from_prep() {
        let mut app = phase_app();
        let board = board(&mut app);
        move_to(&mut app, ShiftPhase::Service);

        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: BeginShiftRequested { board },
        });
        app.update();
        app.update();

        assert_eq!(phase_of(&app), ShiftPhase::Service);
    }

    #[test]
    fn signing_off_opens_the_next_prep() {
        let mut app = phase_app();
        let board = board(&mut app);
        move_to(&mut app, ShiftPhase::Service);
        move_to(&mut app, ShiftPhase::Debrief);

        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: SignOffRequested { board },
        });
        app.update();
        app.update();

        assert_eq!(phase_of(&app), ShiftPhase::Prep);
        assert_eq!(app.world().resource::<ShiftClock>().shift, 2);
    }

    // -- requisition --------------------------------------------------------

    fn requisition(app: &mut App, board: Entity, kind: RequisitionKind) {
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: RequisitionRequested { board, kind },
        });
        app.update();
    }

    fn at_debrief_with(reputation: i32) -> (App, Entity) {
        let mut app = phase_app();
        let board = board(&mut app);
        move_to(&mut app, ShiftPhase::Service);
        app.world_mut().resource_mut::<Shift>().reputation = reputation;
        move_to(&mut app, ShiftPhase::Debrief);
        (app, board)
    }

    #[test]
    fn a_requisition_takes_the_standing_and_lands_next_prep() {
        let (mut app, board) = at_debrief_with(10);
        requisition(&mut app, board, RequisitionKind::Glassware);

        let world = app.world();
        assert_eq!(
            world.resource::<Shift>().reputation,
            10 - RequisitionKind::Glassware.cost()
        );
        assert_eq!(
            world.resource::<ShiftClock>().requisition.glassware,
            config().supply.requisition_glassware_bonus
        );
    }

    #[test]
    fn a_research_grant_lands_immediately() {
        // The one thing you can spend standing on and use before the next prep.
        let (mut app, board) = at_debrief_with(10);
        let before = app.world().resource::<Knowledge>().research_points;

        requisition(&mut app, board, RequisitionKind::ResearchGrant);

        assert_eq!(
            app.world().resource::<Knowledge>().research_points,
            before + RESEARCH_GRANT
        );
    }

    #[test]
    fn a_second_produce_crate_is_refused_rather_than_charged_for() {
        // The crate is a flag, not a count. Charged twice it would take the
        // standing and order nothing extra.
        let (mut app, board) = at_debrief_with(20);
        requisition(&mut app, board, RequisitionKind::ProduceCrate);
        let after_first = app.world().resource::<Shift>().reputation;

        requisition(&mut app, board, RequisitionKind::ProduceCrate);

        assert_eq!(app.world().resource::<Shift>().reputation, after_first);
    }

    #[test]
    fn a_glassware_requisition_stacks() {
        // Unlike the crate, this one is a quantity, so buying twice should buy
        // twice as much.
        let (mut app, board) = at_debrief_with(20);
        requisition(&mut app, board, RequisitionKind::Glassware);
        requisition(&mut app, board, RequisitionKind::Glassware);

        assert_eq!(
            app.world().resource::<ShiftClock>().requisition.glassware,
            config().supply.requisition_glassware_bonus * 2
        );
    }

    #[test]
    fn a_glassware_requisition_is_not_capped_away() {
        // The bonus raises the crate as well as the target. Raising only the
        // target makes the purchase a no-op in exactly the case it is bought
        // for — a lab already short by more than one crate.
        let supply = config().supply;
        let bonus = supply.requisition_glassware_bonus;
        let starved = 1;

        let without = restock_order(starved, supply.glassware_target, supply.crate_max);
        let with = restock_order(
            starved,
            supply.glassware_target + bonus,
            supply.crate_max + bonus,
        );
        assert!(
            with > without,
            "a requisition bought {with} where none bought {without}"
        );
    }

    #[test]
    fn a_requisition_you_cannot_afford_changes_nothing() {
        // Not "takes what it can" — a half-applied purchase that took the
        // standing and delivered nothing is the worst outcome available.
        let (mut app, board) = at_debrief_with(1);
        requisition(&mut app, board, RequisitionKind::ResearchGrant);

        let world = app.world();
        assert_eq!(world.resource::<Shift>().reputation, 1);
        assert!(!world.resource::<ShiftClock>().requisition.produce_crate);
    }

    #[test]
    fn you_cannot_requisition_mid_shift() {
        // Otherwise standing could be spent while it is still being earned.
        let mut app = phase_app();
        let board = board(&mut app);
        move_to(&mut app, ShiftPhase::Service);
        app.world_mut().resource_mut::<Shift>().reputation = 20;

        requisition(&mut app, board, RequisitionKind::Glassware);

        assert_eq!(app.world().resource::<Shift>().reputation, 20);
    }

    #[test]
    fn spending_at_the_debrief_does_not_rewrite_the_report() {
        // `closing` is frozen on entry for exactly this reason: a report that
        // shrank as you spent against it would be unreadable.
        let (mut app, board) = at_debrief_with(10);
        let before = {
            let clock = app.world().resource::<ShiftClock>();
            ShiftReport::between(&clock.opening, &clock.closing)
        };

        requisition(&mut app, board, RequisitionKind::Glassware);

        let clock = app.world().resource::<ShiftClock>();
        assert_eq!(ShiftReport::between(&clock.opening, &clock.closing), before);
    }

    // -- co-op --------------------------------------------------------------

    #[test]
    fn a_joined_chemist_is_told_which_phase_the_lab_is_in() {
        // `ShiftClock` is a resource, and replicon does not replicate
        // resources. Without the snapshot a client's HUD would sit on shift 1
        // prep forever while the host worked a shift around them.
        let mut server = phase_app();
        let mut client = phase_app();
        server.connect_client(&mut client);

        server
            .world_mut()
            .resource_mut::<NextState<ShiftPhase>>()
            .set(ShiftPhase::Service);
        server.update();
        server.update();
        server.exchange_with_client(&mut client);
        client.update();
        client.update();

        assert_eq!(phase_of(&client), ShiftPhase::Service);
        assert_eq!(
            client.world().resource::<ShiftClock>().rules.quota,
            server.world().resource::<ShiftClock>().rules.quota
        );
    }

    #[test]
    fn the_host_does_not_overwrite_its_own_clock() {
        // `CLIENTS_ONLY`. A snapshot echoed back to the host would land a frame
        // late and undo whatever the host had just decided.
        let mut server = phase_app();
        let mut client = phase_app();
        server.connect_client(&mut client);

        server.world_mut().resource_mut::<ShiftClock>().shift = 7;
        server.update();
        server.update();

        assert_eq!(server.world().resource::<ShiftClock>().shift, 7);
    }

    #[test]
    fn the_report_covers_the_shift_that_just_ended() {
        let mut app = phase_app();
        // Two shifts' worth of career, one shift's worth of work.
        app.world_mut().resource_mut::<Shift>().succeeded = 6;
        move_to(&mut app, ShiftPhase::Service);
        app.world_mut().resource_mut::<Shift>().succeeded = 9;
        move_to(&mut app, ShiftPhase::Debrief);

        let clock = app.world().resource::<ShiftClock>();
        let report = ShiftReport::between(&clock.opening, &clock.closing);
        assert_eq!(report.delivered, 3);
    }

    #[test]
    fn the_order_queue_has_a_slot_for_every_concurrent_order() {
        // The HUD queue is fixed-size. If the ramp can put more crew at the
        // counter than there are rows, the one about to expire is the one that
        // gets hidden.
        let base = config();
        let busiest = ShiftRules::for_shift(&base, &base.ramp, 99).max_active;
        assert!(
            crate::ui::ORDER_SLOTS >= busiest,
            "the ramp reaches {busiest} concurrent orders but the queue shows {}",
            crate::ui::ORDER_SLOTS
        );
    }
}
