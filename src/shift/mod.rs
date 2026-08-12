//! Difficulty, forecasts, requisitions, and the career save.
//!
//! There used to be a Prep/Service/Debrief phase machine here — a shift began,
//! crew arrived, and it ended at a quota with a shop to spend standing at.
//! That is gone. Crew arrive continuously for as long as the lab is open, the
//! difficulty tightens on career totals rather than a shift counter, and every
//! requisition is bought live against whichever department's standing pays
//! for it. The one thing carried over from the old "safe window" is the
//! player's own choice: [`Shift::accepting_orders`] is a sign the player can
//! flip when they need a break, not a clock nobody controls.
//!
//! Everything that used to be per-shift arithmetic — the difficulty ramp, the
//! forecast weighting, the restock arithmetic, the requisition costs — still
//! lives in this module as pure functions, because balance that can only be
//! checked by playing for an hour is balance nobody checks.

use std::collections::HashMap;

use bevy::ecs::entity::MapEntities;
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use rand::prelude::*;
use serde::{Deserialize, Serialize};

use crate::knowledge::Knowledge;
use crate::machines::{Machine, MachineKind};
use crate::net::is_authority;
use crate::orders::{
    Department, ForecastDef, OrderConfig, RampDef, RequestDef, Shift, StationData, SupplyDef,
};
use crate::produce::DeliverySchedule;
use crate::radio::RadioEntry;
use crate::radio::RadioLog;
use crate::saves::SaveSlot;
use crate::AppState;

mod restock;

pub use restock::RestockPlugin;

/// The whole cycle, minus the phases: difficulty, forecasts and requisitions.
///
/// Kept separate from [`RestockPlugin`], which spawns things into the world
/// and so needs meshes and materials — this one needs neither, which is what
/// lets it be driven headlessly in tests.
pub struct ShiftPlugin;

impl Plugin for ShiftPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CurrentForecast>()
            .add_mapped_client_message::<RequisitionRequested>(Channel::Ordered)
            .add_mapped_client_message::<ToggleAcceptingOrders>(Channel::Ordered)
            .add_systems(
                Update,
                (redraw_forecast, handle_requisition, handle_toggle_accepting)
                    .run_if(is_authority)
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

// ---------------------------------------------------------------------------
// Difficulty
// ---------------------------------------------------------------------------

/// The difficulty at a point in the career.
///
/// No longer frozen state — there is no shift boundary left to freeze it
/// against — just a pure value recomputed wherever [`current_rules`] is
/// called.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShiftRules {
    pub gap_seconds: (f32, f32),
    pub patience_seconds: (f32, f32),
    pub max_active: usize,
    pub stretch_chance: f64,
    pub forecast_boost: f64,
}

impl Default for ShiftRules {
    fn default() -> Self {
        ShiftRules {
            gap_seconds: (32.0, 58.0),
            patience_seconds: (135.0, 230.0),
            max_active: 3,
            stretch_chance: 0.25,
            forecast_boost: 2.0,
        }
    }
}

impl ShiftRules {
    /// The difficulty at tier `tier` (0-based — tier 0 must reproduce the base
    /// config exactly, or the ramp silently re-balances a game that was
    /// already tuned by hand). Everything else tightens monotonically and
    /// stops at a cap, because an unclamped ramp only reveals itself as
    /// unplayable somewhere nobody tests.
    pub fn for_tier(base: &OrderConfig, ramp: &RampDef, tier: u32) -> ShiftRules {
        let decay = ramp.gap_scale.powi(tier as i32);
        let gap_seconds = (
            (base.gap_seconds.0 * decay).max(ramp.gap_floor),
            (base.gap_seconds.1 * decay).max(ramp.gap_floor),
        );

        let patience_decay = ramp.patience_scale.powi(tier as i32);
        let patience_seconds = (
            (base.patience_seconds.0 * patience_decay).max(ramp.patience_floor),
            (base.patience_seconds.1 * patience_decay).max(ramp.patience_floor),
        );

        // A zero interval is a content bug, not a reason to divide by zero.
        let extra = tier.checked_div(ramp.max_active_every).unwrap_or(0) as usize;
        let max_active = (base.max_active + extra).min(ramp.max_active_cap);

        let stretch_chance =
            (ramp.stretch_base + ramp.stretch_step * tier as f64).min(ramp.stretch_cap);

        ShiftRules {
            gap_seconds,
            patience_seconds,
            max_active,
            stretch_chance,
            forecast_boost: ramp.forecast_boost,
        }
    }
}

/// The live difficulty, computed fresh from how much of the career has
/// actually happened — total orders resolved, successfully or not — rather
/// than a shift number nothing tracks any more.
pub fn current_rules(config: &OrderConfig, shift: &Shift) -> ShiftRules {
    let tier = (shift.succeeded + shift.botched) / config.ramp.orders_per_tier.max(1);
    ShiftRules::for_tier(config, &config.ramp, tier)
}

// ---------------------------------------------------------------------------
// Forecast
// ---------------------------------------------------------------------------

/// What the station is currently expecting, and the requests it makes
/// likelier.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ForecastPick {
    pub id: String,
    pub themes: Vec<String>,
    pub briefing: String,
}

/// The station's live briefing, redrawn periodically rather than once per
/// shift — there is no shift boundary left to redraw it against. `None`
/// before the first redraw or if the data carries no forecasts at all.
#[derive(Resource, Default)]
pub struct CurrentForecast(pub Option<ForecastPick>);

impl CurrentForecast {
    pub fn themes(&self) -> &[String] {
        match &self.0 {
            Some(pick) => &pick.themes,
            None => &[],
        }
    }
}

/// Redraws the briefing on its own clock, radioing whatever it lands on.
///
/// A `None` local timer reads as "due now", so a fresh session is briefed
/// immediately rather than sitting silent for a full interval.
fn redraw_forecast(
    time: Res<Time>,
    mut timer: Local<Option<Timer>>,
    station: Option<Res<StationData>>,
    mut forecast: ResMut<CurrentForecast>,
    mut radio: ResMut<RadioLog>,
) {
    let Some(station) = station else {
        return;
    };
    let due = match timer.as_mut() {
        Some(t) => t.tick(time.delta()).just_finished(),
        None => true,
    };
    if !due {
        return;
    }
    let window = station.config.forecast_seconds;
    let next = rand::rng().random_range(window.0..=window.1);
    *timer = Some(Timer::from_seconds(next, TimerMode::Once));

    let roll = rand::rng().random::<f64>();
    let Some(picked) = draw_forecast(&station.config.forecasts, roll) else {
        return;
    };

    radio.push(RadioEntry {
        channel: "COM".to_string(),
        text: format!("Station briefing: {}", picked.briefing),
        good: true,
    });
    forecast.0 = Some(ForecastPick {
        id: picked.id.clone(),
        themes: picked.themes.clone(),
        briefing: picked.briefing.clone(),
    });

    info!("briefing: {}", picked.id);
}

/// Draws the shift the station is expecting. `roll` is in `[0, 1)`.
///
/// Weighted so a quiet shift can be made rarer than a busy one without having
/// to write it out several times in the data file.
pub fn draw_forecast(forecasts: &[ForecastDef], roll: f64) -> Option<&ForecastDef> {
    let total: f64 = forecasts
        .iter()
        .map(|forecast| forecast.weight.max(0.0))
        .sum();
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

/// Picks a request from `pool`, leaning toward whatever the station was
/// briefed for. `roll` is in `[0, 1)`.
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

/// How many pieces of glassware cargo brings this check.
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

/// Something bought against a department's standing.
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

    /// In the department's standing.
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
            RequisitionKind::Glassware => "Cargo stock the lab deeper on the next resupply",
            RequisitionKind::ProduceCrate => "Botany bring the next haul forward",
            RequisitionKind::ResearchGrant => "Two research points, right now",
        }
    }

    /// Which department's standing this draws from.
    ///
    /// A content call, not an architectural one: Cargo fits glassware and
    /// produce because the courier who brings both is already Cargo;
    /// Engineering fits a research grant as R&D flavour, and could as easily
    /// be a different department if the roster grows one.
    pub fn department(self) -> Department {
        match self {
            RequisitionKind::Glassware | RequisitionKind::ProduceCrate => Department::Cargo,
            RequisitionKind::ResearchGrant => Department::Engineering,
        }
    }
}

/// How long a requisitioned produce crate is brought forward by.
///
/// Early enough to be worth grinding right away, late enough that the player
/// is not handed it the instant they buy it.
const EXPEDITED_PRODUCE_SECONDS: f32 = 25.0;

/// Standing is what you cash in for supplies.
///
/// Debt is allowed to exist — a bad run of deliveries should sting — but it
/// cannot be spent, or a department already dug under would dig itself
/// further.
pub fn can_afford(standing: i32, kind: RequisitionKind) -> bool {
    standing >= kind.cost()
}

/// Books a purchase, or does nothing at all.
///
/// Returns whether it went through, so the caller never has to guess: a
/// half-applied requisition that took the standing and delivered nothing is
/// the worst outcome available here. Every kind but glassware applies at
/// once — glassware still has to be carried in by hand, so it banks into
/// [`Requisition::glassware`] for the next restock check to consume.
pub fn apply_requisition(
    shift: &mut Shift,
    knowledge: &mut Knowledge,
    produce: Option<&mut DeliverySchedule>,
    supply: &SupplyDef,
    kind: RequisitionKind,
) -> bool {
    let department = kind.department();
    if !can_afford(shift.standing(department), kind) {
        return false;
    }
    shift.adjust(department, -kind.cost());

    match kind {
        RequisitionKind::Glassware => {
            shift.requisition.glassware += supply.requisition_glassware_bonus;
        }
        RequisitionKind::ProduceCrate => {
            if let Some(schedule) = produce {
                schedule.expedite(EXPEDITED_PRODUCE_SECONDS);
            }
        }
        RequisitionKind::ResearchGrant => {
            knowledge.award_research(RESEARCH_GRANT);
        }
    }
    true
}

/// What a research grant is worth.
pub const RESEARCH_GRANT: u32 = 2;

/// Spend standing on a department's supplies, any time affordability holds.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct RequisitionRequested {
    #[entities]
    pub board: Entity,
    pub kind: RequisitionKind,
}

/// Flips the "not accepting requests" sign. Either chemist can use it.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct ToggleAcceptingOrders {
    #[entities]
    pub board: Entity,
}

fn handle_requisition(
    mut requests: MessageReader<FromClient<RequisitionRequested>>,
    boards: Query<&Machine>,
    station: Option<Res<StationData>>,
    mut shift: ResMut<Shift>,
    mut knowledge: ResMut<Knowledge>,
    mut produce: Option<ResMut<DeliverySchedule>>,
    mut radio: ResMut<RadioLog>,
) {
    let Some(station) = station else {
        return;
    };
    for request in requests.read() {
        if !is_board(request.board, &boards) {
            continue;
        }
        if !apply_requisition(
            &mut shift,
            &mut knowledge,
            produce.as_deref_mut(),
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

fn handle_toggle_accepting(
    mut requests: MessageReader<FromClient<ToggleAcceptingOrders>>,
    boards: Query<&Machine>,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
) {
    for request in requests.read() {
        if !is_board(request.board, &boards) {
            continue;
        }
        shift.accepting_orders = !shift.accepting_orders;
        radio.push(RadioEntry {
            channel: "COM".to_string(),
            text: if shift.accepting_orders {
                "Chemistry: back open.".to_string()
            } else {
                "Chemistry: not accepting requests for a while.".to_string()
            },
            good: shift.accepting_orders,
        });
    }
}

/// Is this entity actually the standing board?
///
/// A message names the machine it acts on, and a client is free to name any
/// entity at all. Checking the kind here is what stops a crafted message from
/// buying a requisition by pointing at the grinder.
fn is_board(entity: Entity, boards: &Query<&Machine>) -> bool {
    boards
        .get(entity)
        .is_ok_and(|machine| machine.kind == MachineKind::StandingBoard)
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

// The career lives in its own file inside the save, apart from the notebook
// that `KnowledgePlugin` owns. `SaveData` is deliberately both the notebook's
// disk format *and* the `KnowledgeSync` payload, built from `Knowledge` alone.
// Threading the shift tally through it would either couple knowledge to orders
// or make the sync carry data it does not want.

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
            load_progress.run_if(is_authority),
        )
        .add_systems(
            Update,
            persist_progress
                .run_if(in_state(AppState::Playing))
                .run_if(is_authority),
        );
    }
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq, Eq)]
struct ProgressSave {
    #[serde(default)]
    succeeded: u32,
    #[serde(default)]
    botched: u32,
    #[serde(default)]
    department_standing: HashMap<Department, i32>,
    /// A career fact like any other — this game keeps no secrets from a
    /// player willing to open a save file in a text editor.
    #[serde(default)]
    underworld_standing: i32,
}

/// Restores the career on launch.
fn load_progress(
    mut shift: ResMut<Shift>,
    underworld: Option<ResMut<crate::antagonist::UnderworldStanding>>,
    slot: Option<Res<SaveSlot>>,
) {
    // No slot means a new game with nothing to restore, or a guest whose career
    // is the host's and arrives replicated.
    let Some(save) = slot.and_then(|slot| read_progress(&slot.progress_path())) else {
        return;
    };

    shift.succeeded = save.succeeded;
    shift.botched = save.botched;
    shift.department_standing = save.department_standing;
    if let Some(mut underworld) = underworld {
        underworld.0 = save.underworld_standing;
    }
    info!(
        "resuming with {} delivered, {} botched",
        shift.succeeded, shift.botched
    );
}

/// The career on disk, or `None` if there is not a readable one.
fn read_progress(path: &std::path::Path) -> Option<ProgressSave> {
    if !path.exists() {
        return None;
    }
    // A corrupt save should cost the player their progress, not the session.
    match std::fs::read_to_string(path).map(|text| ron::from_str::<ProgressSave>(&text)) {
        Ok(Ok(save)) => Some(save),
        Ok(Err(error)) => {
            warn!("ignoring unreadable {}: {error}", path.display());
            None
        }
        Err(error) => {
            warn!("could not read {}: {error}", path.display());
            None
        }
    }
}

/// The delivered/botched totals a save resumes at, for the menu's load list.
///
/// Read through the owning module rather than by parsing the file in the menu,
/// so `ProgressSave` stays the one description of this format.
pub fn progress_summary(path: &std::path::Path) -> Option<(u32, u32)> {
    let save = read_progress(path)?;
    Some((save.succeeded, save.botched))
}

fn persist_progress(
    shift: Res<Shift>,
    underworld: Option<Res<crate::antagonist::UnderworldStanding>>,
    slot: Option<Res<SaveSlot>>,
    mut written: Local<Option<ProgressSave>>,
) {
    let Some(slot) = slot else {
        return;
    };
    let save = ProgressSave {
        succeeded: shift.succeeded,
        botched: shift.botched,
        department_standing: shift.department_standing.clone(),
        underworld_standing: underworld.map(|u| u.0).unwrap_or(0),
    };
    if written.as_ref() == Some(&save) {
        return;
    }
    let Ok(text) = ron::ser::to_string_pretty(&save, default()) else {
        return;
    };
    *written = Some(save);
    slot.write_progress(&text);
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::state::app::StatesPlugin;

    fn config() -> OrderConfig {
        ron::from_str(include_str!("../../assets/data/station.orders.ron"))
            .expect("order data should parse")
    }

    fn request(themes: &[&str]) -> RequestDef {
        RequestDef {
            reagent: "kelotane".into(),
            amounts: vec![20],
            plea: String::new(),
            specific_plea: String::new(),
            themes: themes.iter().map(|theme| theme.to_string()).collect(),
        }
    }

    // -- the ramp -------------------------------------------------------

    #[test]
    fn tier_zero_matches_the_base_config() {
        // The ramp must be a no-op at tier 0, or it silently re-balances
        // numbers that were already tuned by hand.
        let base = config();
        let rules = ShiftRules::for_tier(&base, &base.ramp, 0);

        assert_eq!(rules.gap_seconds, base.gap_seconds);
        assert_eq!(rules.patience_seconds, base.patience_seconds);
        assert_eq!(rules.max_active, base.max_active);
        assert_eq!(rules.stretch_chance, base.ramp.stretch_base);
    }

    #[test]
    fn the_ramp_only_ever_tightens() {
        let base = config();
        let mut previous = ShiftRules::for_tier(&base, &base.ramp, 0);

        for tier in 1..=30 {
            let rules = ShiftRules::for_tier(&base, &base.ramp, tier);
            assert!(
                rules.gap_seconds.0 <= previous.gap_seconds.0
                    && rules.gap_seconds.1 <= previous.gap_seconds.1,
                "orders got further apart at tier {tier}"
            );
            assert!(
                rules.patience_seconds.0 <= previous.patience_seconds.0,
                "the crew got more patient at tier {tier}"
            );
            assert!(
                rules.max_active >= previous.max_active,
                "the counter got quieter at tier {tier}"
            );
            assert!(
                rules.stretch_chance >= previous.stretch_chance,
                "stretch orders got rarer at tier {tier}"
            );
            previous = rules;
        }
    }

    #[test]
    fn the_ramp_stops_at_its_caps() {
        // Unclamped, tier 50 is a gap of zero seconds and a counter nobody can
        // work. That only shows up for a player who got that far.
        let base = config();
        let ramp = &base.ramp;
        let rules = ShiftRules::for_tier(&base, ramp, 50);

        assert_eq!(rules.max_active, ramp.max_active_cap);
        assert!((rules.stretch_chance - ramp.stretch_cap).abs() < f64::EPSILON);
        assert!(rules.gap_seconds.0 >= ramp.gap_floor);
        assert!(rules.patience_seconds.0 >= ramp.patience_floor);
        // The floor must not invert the range.
        assert!(rules.gap_seconds.0 <= rules.gap_seconds.1);
        assert!(rules.patience_seconds.0 <= rules.patience_seconds.1);
    }

    #[test]
    fn current_rules_reads_the_tier_off_career_totals() {
        let base = config();
        let shift = Shift {
            succeeded: base.ramp.orders_per_tier * 2,
            ..Shift::default()
        };

        let rules = current_rules(&base, &shift);
        let expected = ShiftRules::for_tier(&base, &base.ramp, 2);
        assert_eq!(rules, expected);
    }

    // -- forecast weighting -----------------------------------------------

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

    /// Enough app to run the forecast/requisition systems: states and
    /// resources, no renderer, no crew walking, no assets.
    fn shift_app() -> App {
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
        .insert_resource(Knowledge::new(&chemistry()))
        .insert_resource(station());
        app.finish();
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);
        app.update();
        app
    }

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

    #[test]
    fn a_briefing_reaches_the_radio_without_waiting_a_full_interval() {
        // A `None` timer reads as due now, so a fresh session is not left
        // silent for a full `forecast_seconds` before the first line.
        let mut app = shift_app();
        app.update();

        assert!(
            app.world().resource::<CurrentForecast>().0.is_some(),
            "the first redraw should not wait out a full interval"
        );
        let pick = app
            .world()
            .resource::<CurrentForecast>()
            .0
            .clone()
            .unwrap();
        assert!(
            app.world()
                .resource::<RadioLog>()
                .entries
                .iter()
                .any(|entry| entry.text.contains(&pick.briefing)),
            "the briefing never reached the feed"
        );
    }

    // -- supply -------------------------------------------------------------

    #[test]
    fn a_full_lab_gets_no_delivery() {
        // Otherwise every check adds glassware and the benches silt up.
        assert_eq!(restock_order(6, 6, 4), 0);
        assert_eq!(restock_order(9, 6, 4), 0);
    }

    #[test]
    fn the_lab_cannot_be_starved() {
        // Every counter hand-off despawns the container and nothing else makes
        // beakers, so any deficit at all has to draw a delivery.
        for live in 0..6 {
            assert!(
                restock_order(live, 6, 4) >= 1,
                "no delivery with {live} left"
            );
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

    // -- requisition ----------------------------------------------------

    #[test]
    fn you_cannot_requisition_on_credit() {
        for kind in RequisitionKind::ALL {
            assert!(!can_afford(kind.cost() - 1, kind));
            assert!(can_afford(kind.cost(), kind));
            assert!(!can_afford(-10, kind));
        }
    }

    fn board(app: &mut App) -> Entity {
        app.world_mut()
            .spawn(Machine::new(MachineKind::StandingBoard))
            .id()
    }

    fn requisition(app: &mut App, board: Entity, kind: RequisitionKind) {
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: RequisitionRequested { board, kind },
        });
        app.update();
    }

    fn with_standing(department: Department, standing: i32) -> (App, Entity) {
        let mut app = shift_app();
        let board = board(&mut app);
        app.world_mut()
            .resource_mut::<Shift>()
            .adjust(department, standing);
        (app, board)
    }

    #[test]
    fn a_requisition_can_be_bought_the_instant_standing_allows_it() {
        // No shift boundary left to wait for — affordability is the only
        // gate.
        let (mut app, board) = with_standing(Department::Cargo, 10);
        requisition(&mut app, board, RequisitionKind::Glassware);

        let world = app.world();
        assert_eq!(
            world.resource::<Shift>().standing(Department::Cargo),
            10 - RequisitionKind::Glassware.cost()
        );
        assert_eq!(
            world.resource::<Shift>().requisition.glassware,
            config().supply.requisition_glassware_bonus
        );
    }

    #[test]
    fn a_research_grant_lands_immediately() {
        let (mut app, board) = with_standing(Department::Engineering, 10);
        let before = app.world().resource::<Knowledge>().research_points;

        requisition(&mut app, board, RequisitionKind::ResearchGrant);

        assert_eq!(
            app.world().resource::<Knowledge>().research_points,
            before + RESEARCH_GRANT
        );
    }

    #[test]
    fn a_glassware_requisition_stacks() {
        let (mut app, board) = with_standing(Department::Cargo, 20);
        requisition(&mut app, board, RequisitionKind::Glassware);
        requisition(&mut app, board, RequisitionKind::Glassware);

        assert_eq!(
            app.world().resource::<Shift>().requisition.glassware,
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
        let (mut app, board) = with_standing(Department::Engineering, 1);
        requisition(&mut app, board, RequisitionKind::ResearchGrant);

        let world = app.world();
        assert_eq!(world.resource::<Shift>().standing(Department::Engineering), 1);
    }

    #[test]
    fn only_the_board_can_be_requisitioned_against() {
        // A message names the machine it acts on, and a client is free to
        // name any entity at all. Without the kind check a crafted message
        // could buy a requisition by pointing at the grinder.
        let (mut app, _) = with_standing(Department::Cargo, 20);
        let grinder = app
            .world_mut()
            .spawn(Machine::new(MachineKind::Grinder))
            .id();

        requisition(&mut app, grinder, RequisitionKind::Glassware);

        assert_eq!(app.world().resource::<Shift>().standing(Department::Cargo), 20);
    }

    // -- the accepting-orders toggle --------------------------------------

    #[test]
    fn either_chemist_can_flip_the_sign() {
        let mut app = shift_app();
        let board = board(&mut app);
        assert!(app.world().resource::<Shift>().accepting_orders);

        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: ToggleAcceptingOrders { board },
        });
        app.update();
        assert!(!app.world().resource::<Shift>().accepting_orders);

        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: ToggleAcceptingOrders { board },
        });
        app.update();
        assert!(app.world().resource::<Shift>().accepting_orders);
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
        // briefing promises something that never arrives, and there is no way
        // to notice from inside the game.
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
                "forecast '{}' briefs nothing, so a redraw is silent",
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
            "with no forecasts every redraw is silent"
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

    #[test]
    fn the_order_queue_has_a_slot_for_every_concurrent_order() {
        // The HUD queue is fixed-size. If the ramp can put more crew at the
        // counter than there are rows, the one about to expire is the one that
        // gets hidden.
        let base = config();
        let busiest = ShiftRules::for_tier(&base, &base.ramp, 99).max_active;
        assert!(
            crate::ui::ORDER_SLOTS >= busiest,
            "the ramp reaches {busiest} concurrent orders but the queue shows {}",
            crate::ui::ORDER_SLOTS
        );
    }
}
