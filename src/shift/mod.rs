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

use crate::addiction::CarriedSuspicion;
use crate::antagonist::SecuritySuspicion;
use crate::chem_data::ChemDb;
use crate::containers::{spawn_container, Container, ContainerKind};
use crate::crew::{CrewMember, NotResident};
use crate::knowledge::Knowledge;
use crate::lab::{DeliveryLane, DeliveryStation, DeliveryStations, COUNTER_DROP_Z, COUNTER_TOP};
use crate::machines::{self, Machine, MachineKind, Overclock};
use crate::net::is_authority;
use crate::orders::{
    Department, ForecastDef, GlasswarePackId, OrderConfig, RampDef, RequestDef, Requisition, Shift,
    ShiftSnapshot, StationData,
};
use crate::produce::{self, Produce, ProduceCatalog};
use crate::radio::channel_for;
use crate::radio::RadioEntry;
use crate::radio::RadioLog;
use crate::saves::SaveSlot;
use crate::AppState;

mod impatience;
mod restock;

pub use impatience::ImpatiencePlugin;
pub use restock::{PendingRestock, RestockPlugin};

/// The whole cycle, minus the phases: difficulty, forecasts and requisitions.
///
/// Kept separate from [`RestockPlugin`], which spawns things into the world
/// and so needs meshes and materials — this one needs neither, which is what
/// lets it be driven headlessly in tests.
pub struct ShiftPlugin;

impl Plugin for ShiftPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<CurrentForecast>()
            .init_resource::<ForecastClock>()
            .add_mapped_client_message::<RequisitionRequested>(Channel::Ordered)
            .add_mapped_client_message::<NpcRequisitionRequested>(Channel::Ordered)
            .add_mapped_client_message::<ToggleAcceptingOrders>(Channel::Ordered)
            .add_mapped_client_message::<CallItAShift>(Channel::Ordered)
            .add_mapped_client_message::<OpenUpAgain>(Channel::Ordered)
            .add_systems(
                Update,
                (
                    open_the_shift,
                    redraw_forecast,
                    handle_requisition,
                    handle_npc_requisition,
                    handle_toggle_accepting,
                    handle_call_it_a_shift,
                    handle_open_up_again,
                )
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

/// The player-facing chemistry curriculum stage.
///
/// Order difficulty itself remains continuous through [`ShiftRules`]. These
/// broad bands make that curve legible in the research book and give data
/// audits stable save milestones to simulate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CareerStage {
    Fundamentals,
    Intermediate,
    Advanced,
    Expert,
    Mastery,
}

impl CareerStage {
    pub fn from_progress(successes: u32, known_recipes: usize, total_recipes: usize) -> Self {
        if total_recipes > 0 && known_recipes >= total_recipes {
            CareerStage::Mastery
        } else {
            match successes {
                0..=9 => CareerStage::Fundamentals,
                10..=29 => CareerStage::Intermediate,
                30..=59 => CareerStage::Advanced,
                _ => CareerStage::Expert,
            }
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            CareerStage::Fundamentals => "Fundamentals",
            CareerStage::Intermediate => "Intermediate",
            CareerStage::Advanced => "Advanced",
            CareerStage::Expert => "Expert",
            CareerStage::Mastery => "Mastery",
        }
    }

    pub fn expectation(self) -> &'static str {
        match self {
            CareerStage::Fundamentals => "direct mixtures, safe handling, and first-line medicine",
            CareerStage::Intermediate => {
                "synthesized precursors, temperature control, catalysts, and grinder inputs"
            }
            CareerStage::Advanced => {
                "multi-stage work, pH control, purity targets, and volatile compounds"
            }
            CareerStage::Expert => {
                "tight operating ranges, rare sources, exact emergencies, and payloads"
            }
            CareerStage::Mastery => {
                "efficient production, purification, and complete chemical coverage"
            }
        }
    }

    pub fn next_success_threshold(self) -> Option<u32> {
        match self {
            CareerStage::Fundamentals => Some(10),
            CareerStage::Intermediate => Some(30),
            CareerStage::Advanced => Some(60),
            CareerStage::Expert | CareerStage::Mastery => None,
        }
    }
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

/// Live order pressure: capped station age supplies the baseline tier, co-op
/// scales shared capacity, and the qualitative stability band compresses gaps
/// and widens the queue. Stability never shortens patience on its own.
pub fn current_rules(config: &OrderConfig, shift: &Shift, chemist_count: usize) -> ShiftRules {
    const AGE_TIER_SECONDS: u32 = 10 * 60;
    const MAX_AGE_TIER: u32 = 6;
    let tier = (shift.station_age_seconds / AGE_TIER_SECONDS).min(MAX_AGE_TIER);
    let base = ShiftRules::for_tier(config, &config.ramp, tier);
    let scaled = scale_for_chemists(base, chemist_count, &config.ramp);
    let team_floor = config.ramp.gap_floor / chemist_count.max(1) as f32;
    let multiplier = shift.stability_band.order_gap_multiplier();
    let rules = ShiftRules {
        gap_seconds: (
            (scaled.gap_seconds.0 * multiplier).max(team_floor),
            (scaled.gap_seconds.1 * multiplier).max(team_floor),
        ),
        max_active: scaled.max_active + shift.stability_band.active_order_bonus(),
        ..scaled
    };
    rush_patience(rules)
}

/// Shortens how long a visitor waits, under `THREAT_RUSH` only.
///
/// The companion to the gap divisor `threat::rush_divisor` already applies to
/// arrivals: a saboteur who turns up in seconds is still no use for testing if
/// their visit then takes three minutes to *expire*, and expiry is what the
/// interesting half of a shenanigan thread hangs off. Deliberately applied
/// here rather than inside [`ShiftRules::for_tier`], which owns the difficulty
/// ramp and has an invariant (`tier_zero_matches_the_base_config`) that a
/// testing lever has no business appearing in.
///
/// Unset, this is the identity — see `threat::rush_divisor`.
fn rush_patience(rules: ShiftRules) -> ShiftRules {
    let divisor = crate::threat::rush_divisor();
    if divisor <= 1.0 {
        return rules;
    }
    ShiftRules {
        patience_seconds: (
            rules.patience_seconds.0 / divisor,
            rules.patience_seconds.1 / divisor,
        ),
        ..rules
    }
}

/// Tightens arrival gaps once per antagonist this career has already
/// thwarted — the third wrapping layer, after tier and chemist-count.
/// `Shift::defeated_count` only ever rises on a genuine chemist-mode win
/// (`shift::record_thwarting`), never on a loss, so failing an arc can never
/// make the lab harder the way failing an order already cannot (see
/// `failing_never_makes_the_lab_harder`).
///
/// Zero defeats reproduces the input unchanged, the same "solo/first-win is
/// a no-op" property `scale_for_chemists` already guarantees for its own
/// dimension. `chemist_count` is threaded through only to recompute the same
/// team-divided floor `scale_for_chemists` already applied — clamping
/// against the *undivided* `gap_floor` here would silently undo that team
/// floor's own division for a full table, since this pass runs after it.
fn scale_for_defeated(
    rules: ShiftRules,
    defeated_count: u32,
    chemist_count: usize,
    ramp: &RampDef,
) -> ShiftRules {
    let decay = ramp.defeated_gap_scale.powi(defeated_count as i32);
    let team_floor = ramp.gap_floor / chemist_count.max(1) as f32;
    ShiftRules {
        gap_seconds: (
            (rules.gap_seconds.0 * decay).max(team_floor),
            (rules.gap_seconds.1 * decay).max(team_floor),
        ),
        ..rules
    }
}

/// Widens the counter and shortens arrival gaps for however many chemists
/// are actually in the lab right now, independent of the difficulty tier
/// above.
///
/// `chemist_count` is read fresh from a live query at every call site, never
/// cached, so this responds within one gap interval of someone joining or
/// leaving mid-shift — the same way the tier above was already never frozen
/// against a shift boundary.
///
/// Solo (`chemist_count <= 1`) reproduces the input unchanged: `extra` is
/// `0`, `chemist_gap_scale.powi(0)` is `1.0`, and the `max_active` bonus is
/// `0`. Nothing about today's tuning moves for a chemist working alone.
fn scale_for_chemists(rules: ShiftRules, chemist_count: usize, ramp: &RampDef) -> ShiftRules {
    let extra = chemist_count.saturating_sub(1);
    let decay = ramp.chemist_gap_scale.powi(extra as i32);
    // `gap_floor` protects a solo career from an impossible late-game ramp.
    // A full team can service several orders in parallel, so it gets that
    // floor divided across its live headcount instead of every additional
    // chemist becoming inert as soon as the solo floor is reached.
    let team_floor = ramp.gap_floor / chemist_count.max(1) as f32;
    let gap_seconds = (
        (rules.gap_seconds.0 * decay).max(team_floor),
        (rules.gap_seconds.1 * decay).max(team_floor),
    );
    let bonus = (extra * ramp.max_active_per_chemist).min(ramp.max_active_chemist_cap);
    ShiftRules {
        gap_seconds,
        max_active: rules.max_active + bonus,
        ..rules
    }
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

/// When the next briefing is due. `None` reads as "now", which is what makes a
/// fresh session briefed immediately rather than silent for a full interval.
///
/// A resource rather than the `Local<Option<Timer>>` it was, for the reason
/// every other conversion in this file happened: `crate::session` has to be
/// able to put it back, and a `Local` cannot be reached from outside its own
/// system. Left as a `Local`, the second save opened in a session inherited a
/// part-elapsed timer and opened on silence instead of a briefing.
#[derive(Resource, Default)]
pub struct ForecastClock(pub Option<Timer>);

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
    mut clock: ResMut<ForecastClock>,
    station: Option<Res<StationData>>,
    mut forecast: ResMut<CurrentForecast>,
    mut radio: ResMut<RadioLog>,
) {
    let Some(station) = station else {
        return;
    };
    let due = match clock.0.as_mut() {
        Some(t) => t.tick(time.delta()).just_finished(),
        None => true,
    };
    if !due {
        return;
    }
    let window = station.config.forecast_seconds;
    let next = rand::rng().random_range(window.0..=window.1);
    clock.0 = Some(Timer::from_seconds(next, TimerMode::Once));

    let roll = rand::rng().random::<f64>();
    let Some(picked) = draw_forecast(&station.config.forecasts, roll) else {
        return;
    };

    radio.push(
        RadioEntry::new(
            crate::radio::RadioChannel::Bridge,
            format!("Station briefing: {}", picked.briefing),
        )
        .speaker("Duty Officer")
        .positive(),
    );
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
///
/// Most of the newer kinds are wards against a department's own minor
/// antagonist (`quack`/`smuggler`/`saboteur`/`security::schedule_raid`) —
/// standing spent there buys a concrete "this department has your back"
/// payoff, not just bookkeeping. `Service` has no ward of its own: its minor
/// (`obsessed`) is written to cost nothing mechanically, so there is nothing
/// real to ward against.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum RequisitionKind {
    Glassware,
    ProduceCrate,
    ResearchGrant,
    AntitoxinCrate,
    SecondOpinion,
    QuietWord,
    LookTheOtherWay,
    ChainOfCustody,
    SecondInspection,
    KitchenCarePackage,
    CompedRound,
}

impl RequisitionKind {
    pub const ALL: [RequisitionKind; 11] = [
        RequisitionKind::Glassware,
        RequisitionKind::ProduceCrate,
        RequisitionKind::ResearchGrant,
        RequisitionKind::AntitoxinCrate,
        RequisitionKind::SecondOpinion,
        RequisitionKind::QuietWord,
        RequisitionKind::LookTheOtherWay,
        RequisitionKind::ChainOfCustody,
        RequisitionKind::SecondInspection,
        RequisitionKind::KitchenCarePackage,
        RequisitionKind::CompedRound,
    ];

    /// In the department's standing.
    pub fn cost(self) -> i32 {
        match self {
            RequisitionKind::Glassware => 3,
            RequisitionKind::ProduceCrate => 4,
            RequisitionKind::ResearchGrant => 6,
            RequisitionKind::AntitoxinCrate => 3,
            RequisitionKind::SecondOpinion => 6,
            RequisitionKind::QuietWord => 4,
            RequisitionKind::LookTheOtherWay => 6,
            RequisitionKind::ChainOfCustody => 5,
            RequisitionKind::SecondInspection => 5,
            RequisitionKind::KitchenCarePackage => 3,
            RequisitionKind::CompedRound => 5,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RequisitionKind::Glassware => "Glassware",
            RequisitionKind::ProduceCrate => "Produce crate",
            RequisitionKind::ResearchGrant => "Research grant",
            RequisitionKind::AntitoxinCrate => "Antitoxin crate",
            RequisitionKind::SecondOpinion => "Second opinion",
            RequisitionKind::QuietWord => "Quiet word",
            RequisitionKind::LookTheOtherWay => "Look the other way",
            RequisitionKind::ChainOfCustody => "Chain of custody",
            RequisitionKind::SecondInspection => "Second inspection",
            RequisitionKind::KitchenCarePackage => "Kitchen care package",
            RequisitionKind::CompedRound => "Comped round",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            RequisitionKind::Glassware => "Cargo stock the lab deeper on the next resupply",
            RequisitionKind::ProduceCrate => "Botany bring the next haul forward",
            RequisitionKind::ResearchGrant => "Two research points, right now",
            RequisitionKind::AntitoxinCrate => "A filled dose of antitoxin, dropped at the counter",
            RequisitionKind::SecondOpinion => {
                "Absorbs the next time the ward's own doctor doses a bystander"
            }
            RequisitionKind::QuietWord => "Clears whatever suspicion has already built, right now",
            RequisitionKind::LookTheOtherWay => "Absorbs the next raid before it's ever called in",
            RequisitionKind::ChainOfCustody => {
                "Absorbs the next time something walks off unwatched"
            }
            RequisitionKind::SecondInspection => {
                "Absorbs the next time the glassware gets \"checked\""
            }
            RequisitionKind::KitchenCarePackage => {
                "A filled bottle from the kitchen, dropped at the counter"
            }
            RequisitionKind::CompedRound => "The next order in arrives with extra patience",
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
            RequisitionKind::Glassware
            | RequisitionKind::ProduceCrate
            | RequisitionKind::ChainOfCustody => Department::Cargo,
            RequisitionKind::ResearchGrant | RequisitionKind::SecondInspection => {
                Department::Engineering
            }
            RequisitionKind::AntitoxinCrate | RequisitionKind::SecondOpinion => Department::Medical,
            RequisitionKind::QuietWord | RequisitionKind::LookTheOtherWay => Department::Security,
            RequisitionKind::KitchenCarePackage | RequisitionKind::CompedRound => {
                Department::Service
            }
        }
    }
}

/// How much extra patience a `CompedRound` buys the next generated order.
///
/// Roughly 15-20% of the base `patience_seconds` range in
/// `station.orders.ron` (135-230s) — meaningful without trivializing the
/// timer.
pub(crate) const COMPED_PATIENCE_BONUS_SECONDS: f32 = 30.0;

/// Books a purchase, or does nothing at all.
///
/// Returns whether it went through, so the caller never has to guess: a
/// half-applied requisition that took the standing and delivered nothing is
/// the worst outcome available here. Glassware, and every ward/bonus kind,
/// bank into a [`Requisition`] field for something else to consume later —
/// glassware still has to be carried in by hand, and the wards pay off
/// against something that hasn't happened yet. Everything else applies at
/// once.
///
/// A plain function, not a Bevy system, so widening its signature for the
/// newer kinds costs nothing against Bevy's 16-parameter system ceiling —
/// unlike splitting the match across two functions, which would leave two
/// places that both have to stay exhaustive over the same enum.
#[allow(clippy::too_many_arguments)]
pub fn apply_requisition(
    commands: &mut Commands,
    shift: &mut Shift,
    knowledge: &mut Knowledge,
    station: &StationData,
    catalog: Option<&ProduceCatalog>,
    already_present: bool,
    present_produce: impl IntoIterator<Item = produce::ProduceId>,
    rng: &mut impl Rng,
    db: &ChemDb,
    suspicion: Option<&mut SecuritySuspicion>,
    carried: Option<&mut CarriedSuspicion>,
    delivery_station: DeliveryStation,
    kind: RequisitionKind,
) -> bool {
    let department = kind.department();
    shift.spend_goodwill(department, kind.cost());

    match kind {
        RequisitionKind::Glassware => {
            shift.requisition.glassware += station.config.supply.requisition_glassware_bonus;
        }
        RequisitionKind::ProduceCrate => {
            // Cargo's institutional pull buys an impersonal, unthemed crate —
            // whatever's running low, balanced across the whole catalog —
            // unlike one of Botanist Ivy's own curated packs, which spends
            // her personal standing instead of Cargo's. Silently a no-op if
            // she's already in the room; the standing was already spent on
            // resolving affordability above, matching every other kind here.
            if let Some(catalog) = catalog {
                if !already_present {
                    let items = produce::balanced_crate(catalog, present_produce, rng);
                    produce::spawn_named_delivery(commands, station, catalog.courier_name(), items);
                }
            }
        }
        RequisitionKind::ResearchGrant => {
            knowledge.award_research(RESEARCH_GRANT);
        }
        RequisitionKind::AntitoxinCrate => {
            gift_container(commands, db, delivery_station, "dylovene");
        }
        RequisitionKind::KitchenCarePackage => {
            gift_container(commands, db, delivery_station, "saline_glucose");
        }
        RequisitionKind::SecondOpinion => {
            shift.requisition.quack_wards += 1;
        }
        RequisitionKind::LookTheOtherWay => {
            shift.requisition.raid_wards += 1;
        }
        RequisitionKind::ChainOfCustody => {
            shift.requisition.smuggler_wards += 1;
        }
        RequisitionKind::SecondInspection => {
            shift.requisition.saboteur_wards += 1;
        }
        RequisitionKind::CompedRound => {
            shift.requisition.patience_bonus_orders += 1;
        }
        RequisitionKind::QuietWord => {
            if let Some(suspicion) = suspicion {
                crate::antagonist::clear_suspicion(suspicion);
            }
            if let Some(carried) = carried {
                carried.0 = 0.0;
            }
        }
    }
    true
}

/// What a research grant is worth.
pub const RESEARCH_GRANT: u32 = 2;

/// Drops a filled bottle at the counter's vial-drop spot, as an instant
/// requisition's whole effect.
///
/// Reuses the exact "gift a filled container" idiom `obsessed::leaves_token`
/// already uses: `spawn_container` for the empty vessel, then a queued
/// command to fill it once it exists (`Container` isn't available to write
/// synchronously from a plain `&mut Commands`). Filled to the bottle's full
/// capacity rather than a token amount — this is a real crate, not a nudge.
fn gift_container(
    commands: &mut Commands,
    db: &ChemDb,
    station: DeliveryStation,
    reagent_key: &str,
) {
    let Some(reagent) = db.reagents.id_of(reagent_key) else {
        warn!("requisition gifts unknown reagent '{reagent_key}'");
        return;
    };
    let (_, height) = ContainerKind::Bottle.dimensions();
    let token = spawn_container(
        commands,
        ContainerKind::Bottle,
        station.drop_position(COUNTER_TOP + height * 0.5)
            - (station.transform.rotation * Vec3::X) * 0.6,
    );
    let amount = ContainerKind::Bottle.capacity();
    let ph = db.reagents.get(reagent).ph;
    commands.queue(move |world: &mut World| {
        if let Some(mut container) = world.get_mut::<Container>(token) {
            let _ = container.solution.add_profiled(reagent, amount, 1.0, ph);
        }
    });
}

/// Something bought from one specific crew member's own trust, rather than
/// their whole department's — the individual-standing sibling of
/// [`RequisitionKind`]. Where a department requisition is fixed, compiled
/// content, an NPC's own wares are mostly authored data (Ivy's packs live in
/// `station.produce.ron`, Sato's in `station.orders.ron`), so a payload
/// variant wraps an interned handle instead of being matched on directly for
/// cost/label/blurb — those need a catalog to resolve. `LindqvistOverclock`
/// is a unit variant instead: exactly one purchasable item today, same
/// reasoning `RequisitionKind::ResearchGrant` stays a unit variant for.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum NpcRequisitionKind {
    IvyPack(produce::ProducePackId),
    SatoPack(GlasswarePackId),
    LindqvistOverclock,
    /// A longer-reach syringe. Sold by Lindqvist alongside the Overclock —
    /// same "precision tool" flavor.
    LindqvistSyringeGun,
    /// The stronger of the two cone sprayers. Sold by Lindqvist.
    LindqvistPressureSprayer,
    /// The cheap cone sprayer. Sold by Sato — bulk/novelty goods, matching
    /// his glassware flavor — rather than Lindqvist, so the two Engineering
    /// items stay the pricier, more deliberate purchase.
    SatoWaterGun,
}

impl NpcRequisitionKind {
    /// Whose standing this spends — the `station.crew.ron` name
    /// [`Shift::npc_standing`]/[`Shift::adjust_npc`] key on.
    pub fn owner(self) -> &'static str {
        match self {
            NpcRequisitionKind::IvyPack(_) => "Botanist Ivy",
            NpcRequisitionKind::SatoPack(_) | NpcRequisitionKind::SatoWaterGun => "Miner Sato",
            NpcRequisitionKind::LindqvistOverclock
            | NpcRequisitionKind::LindqvistSyringeGun
            | NpcRequisitionKind::LindqvistPressureSprayer => "Tech Lindqvist",
        }
    }
}

/// In Tech Lindqvist's own standing. A plain constant, not authored data,
/// same reasoning `RequisitionKind::cost()` hardcodes its own — there is
/// exactly one item to price. `pub(crate)` so `ui::standing_board_body` can
/// show the same number on the button it draws.
pub(crate) const OVERCLOCK_COST: i32 = 5;

/// The three confrontation items' own costs — first-pass constants,
/// calibrated against `OVERCLOCK_COST`'s and the glassware packs' existing
/// 2-10 scale, not playtested. `pub(crate)` for the same reason
/// `OVERCLOCK_COST` is.
pub(crate) const SYRINGE_GUN_COST: i32 = 10;
pub(crate) const PRESSURE_SPRAYER_COST: i32 = 6;
pub(crate) const WATER_GUN_COST: i32 = 2;

/// Books a purchase from one NPC's own shop, or does nothing at all — the
/// individual-standing sibling of [`apply_requisition`], with the same
/// "never half-apply" contract.
///
/// Refuses (spending nothing) if the owner is estranged, across every kind —
/// a locked shop is a coherent secondary effect of estrangement, not a
/// substitute for the suspicion consequence `estrangement::watch_estrangement`
/// already applies. `already_present` is checked only inside the two arms
/// that actually walk a courier in (`IvyPack`/`SatoPack`); `LindqvistOverclock`
/// materializes instantly with no crew visit at all, so it has nothing to
/// collide with.
#[allow(clippy::too_many_arguments)]
pub fn apply_npc_requisition(
    commands: &mut Commands,
    shift: &mut Shift,
    station: &StationData,
    catalog: Option<&ProduceCatalog>,
    pending: &mut PendingRestock,
    overclocks: &mut Query<&mut Overclock>,
    delivery_station: DeliveryStation,
    estranged: &crate::estrangement::Estranged,
    already_present: bool,
    kind: NpcRequisitionKind,
) -> bool {
    let owner = kind.owner();
    if estranged.0.contains(owner) {
        return false;
    }
    match kind {
        NpcRequisitionKind::IvyPack(pack_id) => {
            if already_present {
                return false;
            }
            let Some(catalog) = catalog else {
                return false;
            };
            let Some(pack) = catalog.pack(pack_id) else {
                return false;
            };
            shift.spend_npc_goodwill(owner, pack.cost);
            produce::spawn_named_delivery(commands, station, owner, pack.expand_items());
            true
        }
        NpcRequisitionKind::SatoPack(pack_id) => {
            if already_present {
                return false;
            }
            let Some(pack) = station.config.supply.pack(pack_id) else {
                return false;
            };
            shift.spend_npc_goodwill(owner, pack.cost);
            restock::queue_glassware_purchase(pending, pack.beakers, pack.large);
            true
        }
        NpcRequisitionKind::LindqvistOverclock => {
            shift.spend_npc_goodwill(owner, OVERCLOCK_COST);
            if let Some(mut existing) = overclocks.iter_mut().next() {
                existing.charges += machines::OVERCLOCK_CHARGES;
            } else {
                machines::spawn_overclock(commands, delivery_station);
            }
            true
        }
        NpcRequisitionKind::LindqvistSyringeGun => {
            shift.spend_npc_goodwill(owner, SYRINGE_GUN_COST);
            spawn_purchased_item(commands, ContainerKind::SyringeGun, delivery_station, 0.6);
            true
        }
        NpcRequisitionKind::LindqvistPressureSprayer => {
            shift.spend_npc_goodwill(owner, PRESSURE_SPRAYER_COST);
            spawn_purchased_item(
                commands,
                ContainerKind::PressureSprayer,
                delivery_station,
                0.9,
            );
            true
        }
        NpcRequisitionKind::SatoWaterGun => {
            shift.spend_npc_goodwill(owner, WATER_GUN_COST);
            spawn_purchased_item(commands, ContainerKind::WaterGun, delivery_station, 0.6);
            true
        }
    }
}

/// Materializes a bought confrontation item at the counter, empty and ready
/// to load — the `Container`-item counterpart of `machines::spawn_overclock`
/// ("arrives instantly, no courier"), reusing `gift_container`'s own
/// positioning idiom below it. `sideways` keeps two items bought close
/// together (or the same item bought twice) from spawning exactly on top of
/// one another.
fn spawn_purchased_item(
    commands: &mut Commands,
    kind: ContainerKind,
    station: DeliveryStation,
    sideways: f32,
) {
    let (_, height) = kind.dimensions();
    spawn_container(
        commands,
        kind,
        station.drop_position(COUNTER_TOP + height * 0.5)
            - (station.transform.rotation * Vec3::X) * sideways,
    );
}

/// Spend standing on a department's supplies, any time affordability holds.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct RequisitionRequested {
    #[entities]
    pub board: Entity,
    pub kind: RequisitionKind,
}

/// Spend one NPC's own standing on something only they sell.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct NpcRequisitionRequested {
    #[entities]
    pub board: Entity,
    pub kind: NpcRequisitionKind,
}

/// Flips the "not accepting requests" sign. Either chemist can use it.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct ToggleAcceptingOrders {
    #[entities]
    pub board: Entity,
}

#[allow(clippy::too_many_arguments)]
fn handle_requisition(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<RequisitionRequested>>,
    boards: Query<&Machine>,
    station: Option<Res<StationData>>,
    mut shift: ResMut<Shift>,
    mut knowledge: ResMut<Knowledge>,
    catalog: Option<Res<ProduceCatalog>>,
    present_crew: Query<&CrewMember, NotResident>,
    present_produce: Query<&Produce>,
    db: Res<ChemDb>,
    mut suspicion: Option<ResMut<SecuritySuspicion>>,
    mut carried: Option<ResMut<CarriedSuspicion>>,
    mut radio: ResMut<RadioLog>,
    delivery_stations: Option<Res<DeliveryStations>>,
) {
    let Some(station) = station else {
        return;
    };
    let mut rng = rand::rng();
    for request in requests.read() {
        if !is_board(request.board, &boards) {
            continue;
        }
        let delivery_station = delivery_stations
            .as_deref()
            .cloned()
            .unwrap_or_default()
            .station(DeliveryLane::Public);
        let already_present = catalog
            .as_deref()
            .is_some_and(|catalog| produce::courier_present(catalog.courier_name(), &present_crew));
        if !apply_requisition(
            &mut commands,
            &mut shift,
            &mut knowledge,
            &station,
            catalog.as_deref(),
            already_present,
            present_produce.iter().map(|produce| produce.0),
            &mut rng,
            &db,
            suspicion.as_deref_mut(),
            carried.as_deref_mut(),
            delivery_station,
            request.kind,
        ) {
            continue;
        }
        // Was hardcoded to "CGO" while every kind was Cargo/Engineering
        // flavoured; wrong the moment a Medical/Security/Service item exists.
        radio.push(
            RadioEntry::new(
                channel_for(request.kind.department().label()),
                format!("Requisition logged: {}.", request.kind.label()),
            )
            .positive(),
        );
    }
}

/// The individual-standing sibling of [`handle_requisition`] — same shape,
/// keyed to one NPC instead of a department.
#[allow(clippy::too_many_arguments)]
fn handle_npc_requisition(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<NpcRequisitionRequested>>,
    boards: Query<&Machine>,
    station: Option<Res<StationData>>,
    catalog: Option<Res<ProduceCatalog>>,
    mut shift: ResMut<Shift>,
    mut pending: ResMut<PendingRestock>,
    mut overclocks: Query<&mut Overclock>,
    delivery_stations: Option<Res<DeliveryStations>>,
    estranged: Option<Res<crate::estrangement::Estranged>>,
    present_crew: Query<&CrewMember, NotResident>,
    mut radio: ResMut<RadioLog>,
) {
    // Only `StationData` is unconditionally required — `catalog` is Ivy-only
    // and is threaded through as an `Option`, so Sato's/Lindqvist's own
    // purchases still work before (or without) the produce catalog ever
    // loading.
    let Some(station) = station else {
        return;
    };
    let empty = crate::estrangement::Estranged::default();
    let estranged = estranged.as_deref().unwrap_or(&empty);
    for request in requests.read() {
        if !is_board(request.board, &boards) {
            continue;
        }
        let delivery_station = delivery_stations
            .as_deref()
            .cloned()
            .unwrap_or_default()
            .station(DeliveryLane::Public);
        let already_present = produce::courier_present(request.kind.owner(), &present_crew);
        if !apply_npc_requisition(
            &mut commands,
            &mut shift,
            &station,
            catalog.as_deref(),
            &mut pending,
            &mut overclocks,
            delivery_station,
            estranged,
            already_present,
            request.kind,
        ) {
            continue;
        }
        radio.push(
            RadioEntry::new(
                channel_for(&find_role(&station, request.kind.owner())),
                format!("{} logged a personal order.", request.kind.owner()),
            )
            .speaker(request.kind.owner())
            .positive(),
        );
    }
}

/// The role a named crew member holds, for the radio channel their own
/// purchase should log against. Falls back to `Service` — every current
/// seller is Service's own — rather than guessing wrong silently.
fn find_role(station: &StationData, name: &str) -> String {
    station
        .crew
        .iter()
        .find(|member| member.name == name)
        .map(|member| member.role.clone())
        .unwrap_or_else(|| "Service".to_string())
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
        let entry = RadioEntry::new(
            crate::radio::RadioChannel::Bridge,
            if shift.accepting_orders {
                "Chemistry: back open.".to_string()
            } else {
                "Chemistry: not accepting requests for a while.".to_string()
            },
        )
        .speaker("Duty Officer");
        radio.push(if shift.accepting_orders {
            entry.positive()
        } else {
            entry
        });
    }
}

// ---------------------------------------------------------------------------
// Calling it a shift
// ---------------------------------------------------------------------------

// The old Prep/Service/Debrief machine is still gone and is not coming back:
// crew arrive continuously and no clock the player does not control ever ends
// anything. What went with it, though, was the *reflection beat* — a moment
// that says "that was a shift", tallies it, and gives the player somewhere to
// stop. That is what this section puts back, entirely on the player's say-so:
// they flip the sign, they wait out whoever is still at the counter, and they
// decide when to call it.
//
// Nothing here gates the simulation. Calling a shift sets a flag the standing
// board draws from; everything else in the lab carries on exactly as it was.

/// Ends the shift, once the sign is down and the counter has cleared.
///
/// Separate from [`ToggleAcceptingOrders`] because they are different verbs:
/// flipping the sign is reversible and means "no new traffic", while calling
/// the shift closes the books on it. Folding them into one two-meaning button
/// would make a mis-click on a busy counter unrecoverable.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct CallItAShift {
    #[entities]
    pub board: Entity,
}

/// Starts the next shift: snapshot reset, number up, sign back up.
#[derive(Message, Serialize, Deserialize, Clone, MapEntities)]
pub struct OpenUpAgain {
    #[entities]
    pub board: Entity,
}

/// Whether the shift can be called right now.
///
/// Pure, and checked on the authority as well as drawn from in the panel: the
/// button being hidden is presentation, and a client is free to send the
/// message whenever it likes. `waiting` is how many crew are still standing at
/// the counter holding an order — calling a shift out from under someone who
/// is still waiting would botch their order for them.
pub fn can_call_it(shift: &Shift, waiting: usize) -> bool {
    !shift.accepting_orders && !shift.called && waiting == 0
}

/// What one shift came to: the difference between now and
/// [`Shift::opened_at`].
///
/// A plain value with no `Entity` in it, built by [`shift_report`], so the
/// whole debrief can be checked without a world.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShiftReport {
    pub number: u32,
    pub delivered: u32,
    pub botched: u32,
    /// Net research, not research *earned*: hints and dispenser tiers are
    /// bought out of the same pot, so a shift that discovered plenty and spent
    /// it all reads as zero. That is the honest number — it is what the lab
    /// actually has to show for the shift — and it is why the board labels
    /// this "banked" rather than "earned".
    pub research: i64,
    pub recipes: usize,
    /// Only the departments that actually moved, in [`Department::ALL`] order.
    /// A debrief listing five unchanged zeroes is a debrief nobody reads.
    pub standing: Vec<(Department, i32)>,
    pub condition_change: Option<ConditionChange>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConditionChange {
    Improved,
    Deteriorated,
}

impl ShiftReport {
    /// Nothing happened this shift — no deliveries, no botches, no movement.
    pub fn is_quiet(&self) -> bool {
        self.delivered == 0
            && self.botched == 0
            && self.research == 0
            && self.recipes == 0
            && self.standing.is_empty()
            && self.condition_change.is_none()
    }
}

/// Takes the career totals as they stand right now.
fn snapshot(shift: &Shift, knowledge: &Knowledge) -> ShiftSnapshot {
    ShiftSnapshot {
        succeeded: shift.succeeded,
        botched: shift.botched,
        department_standing: Department::ALL
            .into_iter()
            .map(|department| (department, shift.standing(department)))
            .collect(),
        research_points: knowledge.research_points,
        recipes_known: knowledge.known_count(),
        stability_band: shift.stability_band,
    }
}

/// The debrief: everything that changed since the shift opened.
///
/// Saturating throughout. The career totals only ever climb, so a snapshot
/// that is somehow ahead of the present is a bug elsewhere — but it must
/// report zero rather than panic in a release build and wrap in a debug one.
pub fn shift_report(shift: &Shift, knowledge: &Knowledge) -> ShiftReport {
    let opened = shift.opened_at.clone().unwrap_or_default();
    ShiftReport {
        number: shift.shift_number,
        delivered: shift.succeeded.saturating_sub(opened.succeeded),
        botched: shift.botched.saturating_sub(opened.botched),
        research: knowledge.research_points as i64 - opened.research_points as i64,
        recipes: knowledge.known_count().saturating_sub(opened.recipes_known),
        standing: Department::ALL
            .into_iter()
            .filter_map(|department| {
                let before = opened
                    .department_standing
                    .get(&department)
                    .copied()
                    .unwrap_or(0);
                let delta = shift.standing(department) - before;
                (delta != 0).then_some((department, delta))
            })
            .collect(),
        condition_change: if shift.stability_band < opened.stability_band {
            Some(ConditionChange::Improved)
        } else if shift.stability_band > opened.stability_band {
            Some(ConditionChange::Deteriorated)
        } else {
            None
        },
    }
}

/// Takes the opening snapshot on the first frame the lab is running.
///
/// In `Update` rather than `OnEnter(AppState::Playing)` because
/// `knowledge::initialise_knowledge` inserts `Knowledge` through `Commands`:
/// it does not exist until the command queue flushes, so there is no ordering
/// inside `OnEnter` that could read it. Guarded on `is_none` rather than run
/// once, so resuming a save mid-shift keeps the snapshot it was saved with
/// instead of quietly re-opening the shift on load.
fn open_the_shift(mut shift: ResMut<Shift>, knowledge: Res<Knowledge>) {
    if shift.opened_at.is_some() {
        return;
    }
    let taken = snapshot(&shift, &knowledge);
    shift.opened_at = Some(taken);
}

fn handle_call_it_a_shift(
    mut requests: MessageReader<FromClient<CallItAShift>>,
    boards: Query<&Machine>,
    counter: Query<
        (),
        (
            Or<(
                With<crate::orders::Order>,
                With<crate::order_intake::PendingOrder>,
            )>,
            crate::crew::NotResident,
        ),
    >,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
) {
    for request in requests.read() {
        if !is_board(request.board, &boards) {
            continue;
        }
        if !can_call_it(&shift, counter.iter().count()) {
            continue;
        }
        shift.called = true;
        radio.push(
            RadioEntry::new(
                crate::radio::RadioChannel::Bridge,
                format!("Chemistry: that's shift {} closed out.", shift.shift_number),
            )
            .speaker("Duty Officer")
            .positive(),
        );
    }
}

fn handle_open_up_again(
    mut requests: MessageReader<FromClient<OpenUpAgain>>,
    boards: Query<&Machine>,
    knowledge: Res<Knowledge>,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
) {
    for request in requests.read() {
        if !is_board(request.board, &boards) {
            continue;
        }
        if !shift.called {
            continue;
        }
        let taken = snapshot(&shift, &knowledge);
        shift.opened_at = Some(taken);
        shift.shift_number += 1;
        shift.called = false;
        shift.accepting_orders = true;
        radio.push(
            RadioEntry::new(
                crate::radio::RadioChannel::Bridge,
                format!(
                    "Chemistry: shift {}, open for business.",
                    shift.shift_number
                ),
            )
            .speaker("Duty Officer")
            .positive(),
        );
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
        app.init_resource::<PersistedProgress>()
            .add_systems(
                OnEnter(AppState::Playing),
                load_progress.run_if(is_authority),
            )
            .add_systems(
                Update,
                (persist_progress, record_thwarting)
                    .run_if(in_state(AppState::Playing))
                    .run_if(is_authority),
            );
    }
}

/// `Eq` is deliberately absent: `addictions` carries the float weights that
/// make a habit build gradually, and `persist_progress` only ever needs
/// `PartialEq` — it compares a value against a clone of itself to decide
/// whether anything actually changed, which floats answer correctly.
#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
struct ProgressSave {
    #[serde(default)]
    succeeded: u32,
    #[serde(default)]
    botched: u32,
    /// One hidden standing per named crew member. See `orders::Shift::
    /// npc_standing`. Empty on a save written before per-NPC standing
    /// existed — `load_progress` backward-fills it from `department_standing`
    /// below in that case, so a returning career's crew do not silently
    /// reset to neutral.
    #[serde(default)]
    npc_standing: HashMap<String, i32>,
    /// Legacy, department-level standing from before per-NPC standing
    /// existed. Read-only from here on: `load_progress` only ever reads this
    /// to backward-fill `npc_standing` when a save has no `npc_standing` of
    /// its own, and `persist_progress` never writes anything into it again.
    #[serde(default)]
    department_standing: HashMap<Department, i32>,
    /// Which shift the career is on, and what it looked like when that shift
    /// opened — so closing the game halfway through a shift and coming back
    /// resumes *that* shift rather than silently starting a new one.
    ///
    /// `0` is not a legal shift number; a save written before shifts were
    /// numbered carries it and [`load_progress`] reads it as "shift 1".
    #[serde(default)]
    shift_number: u32,
    /// `Option` for the same reason `campaign` below is: a save written before
    /// this existed has no snapshot to resume, and inventing one from zeroes
    /// would report a whole career as this shift's work. `None` means
    /// [`open_the_shift`] takes a fresh one on the first frame instead.
    #[serde(default)]
    opened_at: Option<ShiftSnapshot>,
    /// Purchases that have been paid for but are waiting for their next use.
    /// This is live lab state, not merely a career total: losing a ward or a
    /// glassware requisition on reload makes closing the game an exploit.
    #[serde(default)]
    requisition: Requisition,
    /// The service sign is a player decision and must return exactly as it
    /// was, otherwise reloading can silently start traffic the player stopped.
    #[serde(default)]
    accepting_orders: bool,
    /// Whether the player had already called the current shift. The debrief
    /// itself is derived from `opened_at`, so restoring this does not need a
    /// separate UI snapshot.
    #[serde(default)]
    called: bool,
    /// A career fact like any other — this game keeps no secrets from a
    /// player willing to open a save file in a text editor.
    #[serde(default)]
    underworld_standing: i32,
    /// Whether Rogue Security's reward has already been earned — a one-shot
    /// career fact, same reasoning as `underworld_standing`. See
    /// `rogue_security::RogueRedeemed`.
    #[serde(default)]
    rogue_redeemed: bool,
    /// How far into the obsessed thread's authored visit sequence the
    /// career has reached. See `obsessed::ObsessedProgress`.
    #[serde(default)]
    obsessed_progress: usize,
    /// How far into the cult thread's authored ritual the career has
    /// reached. See `cult::CultProgress`.
    #[serde(default)]
    cult_progress: usize,
    /// Authority-only timed Cult state added after the original stage index.
    /// Separate defaulted fields keep the old scalar save format readable.
    #[serde(default)]
    cult_wave_remaining: f32,
    #[serde(default)]
    cult_offered_stage: Option<usize>,
    #[serde(default)]
    cult_banked_intel: usize,
    #[serde(default)]
    cult_finale_started: bool,
    /// How far into each department minor's authored chain the career has
    /// reached — Cargo's, Engineering's and Medical's respectively. Same
    /// reasoning as `obsessed_progress`: an escalation the player is meant to
    /// notice across a career has to survive closing the game.
    #[serde(default)]
    smuggler_progress: usize,
    #[serde(default)]
    saboteur_progress: usize,
    #[serde(default)]
    quack_progress: usize,
    #[serde(default)]
    bent_guard_progress: usize,
    /// This save's campaign — which main antagonist it drew, how far along
    /// they are, and how it ended if it has. See `arc::Campaign`.
    ///
    /// `Option` rather than a defaulted struct so that a `progress.ron`
    /// written before campaigns existed loads cleanly and simply rolls a
    /// fresh one, instead of resuming a career against a Cult it never met.
    #[serde(default)]
    campaign: Option<crate::arc::Campaign>,
    /// Who on the station has a habit. A career fact like the rest of this
    /// file: an addict is a person who remembers you, and forgetting them
    /// between sessions would quietly delete the only reason dealing pays.
    /// See `addiction::Addictions`.
    #[serde(default)]
    addictions: crate::addiction::Addictions,
    /// Which named crew members are currently estranged — a career fact
    /// like `rogue_redeemed`, not a shift-scoped one: burning a relationship
    /// should not un-burn itself just because the shift was called. See
    /// `estrangement::Estranged`.
    #[serde(default)]
    estranged: std::collections::HashSet<String>,
    /// Antagonists thwarted this career. See `orders::Shift::defeated_count`.
    #[serde(default)]
    defeated_count: u32,
    /// Legacy danger-increasing meter, retained only to migrate older saves.
    #[serde(default)]
    instability: LegacyInstability,
    /// The authoritative hidden station health and its persistent age.
    #[serde(default)]
    station_stability: crate::instability::StationStability,
    /// See `orders::Shift::evacuated`.
    #[serde(default)]
    evacuated: bool,
    /// Hidden personalities, personal favor progress, and the optional
    /// resident who replaced one department minor's outsider identity.
    #[serde(default)]
    social: crate::social::SocialState,
}

#[derive(Serialize, Deserialize, Default, Clone, PartialEq)]
struct LegacyInstability {
    #[serde(default)]
    level: i32,
    #[serde(default)]
    tier: LegacyInstabilityTier,
}

#[derive(Serialize, Deserialize, Default, Clone, Copy, PartialEq, Eq)]
enum LegacyInstabilityTier {
    #[default]
    Calm,
    Fraying,
    Breaking,
}

/// Whether the save at `path` has already evacuated — read straight off disk,
/// the same way `arc_standing` already reads a save's `Campaign` without a
/// live `World`. `saves::list_slots()` uses this to dim a save's row in the
/// load list; `menu::handle_menu_clicks`'s `LoadSave` arm uses it again as
/// defense-in-depth against a stale or forged click bypassing that dimming.
pub fn is_evacuated(path: &std::path::Path) -> bool {
    read_progress(path).is_some_and(|save| save.evacuated)
}

fn restored_station_stability(save: &ProgressSave) -> crate::instability::StationStability {
    let mut stability = save.station_stability.clone();
    if stability == crate::instability::StationStability::default()
        && (save.instability.level > 0 || save.instability.tier != LegacyInstabilityTier::Calm)
    {
        stability.value = (crate::instability::STABILITY_MAX - save.instability.level as f32)
            .clamp(0.0, crate::instability::STABILITY_MAX);
        stability.band = match stability.value {
            value if value <= 0.0 => crate::instability::StabilityBand::Evacuating,
            value if value <= 25.0 => crate::instability::StabilityBand::Critical,
            value if value <= 50.0 => crate::instability::StabilityBand::Unstable,
            value if value <= 75.0 => crate::instability::StabilityBand::Strained,
            _ => crate::instability::StabilityBand::Stable,
        };
    }
    stability
}

/// Restores the career on launch.
#[allow(clippy::too_many_arguments)]
fn load_progress(
    mut commands: Commands,
    mut shift: ResMut<Shift>,
    underworld: Option<ResMut<crate::antagonist::UnderworldStanding>>,
    rogue_redeemed: Option<ResMut<crate::rogue_security::RogueRedeemed>>,
    obsessed_progress: Option<ResMut<crate::obsessed::ObsessedProgress>>,
    cult_progress: Option<ResMut<crate::cult::CultProgress>>,
    smuggler_progress: Option<ResMut<crate::smuggler::SmugglerProgress>>,
    saboteur_progress: Option<ResMut<crate::saboteur::SaboteurProgress>>,
    quack_progress: Option<ResMut<crate::quack::QuackProgress>>,
    bent_guard_progress: Option<ResMut<crate::bent_guard::BentGuardProgress>>,
    mut thwarted: ResMut<crate::arc::ThwartedAntags>,
    mut addictions: ResMut<crate::addiction::Addictions>,
    estranged: Option<ResMut<crate::estrangement::Estranged>>,
    instability: Option<ResMut<crate::instability::Instability>>,
    social: Option<ResMut<crate::social::SocialState>>,
    slot: Option<Res<SaveSlot>>,
) {
    // Cross-save, so it is read whether or not this session has a slot at all
    // — and before the `let else` below, which returns early for a brand new
    // save, the exact case where knowing what has already been beaten decides
    // which antagonist gets drawn.
    thwarted.0 = crate::saves::thwarted_antags();

    // No slot means a new game with nothing to restore, or a guest whose career
    // is the host's and arrives replicated.
    let Some(slot) = slot else {
        return;
    };
    let Some(save) = read_progress(&slot.progress_path()) else {
        // A selected slot with no progress file is a genuinely fresh career.
        // Guests have no SaveSlot and returned above.
        if let Some(mut social) = social {
            *social = crate::social::SocialState::fresh();
        }
        return;
    };

    shift.succeeded = save.succeeded;
    shift.botched = save.botched;
    shift.defeated_count = save.defeated_count;
    shift.evacuated = save.evacuated;
    if let Some(mut stability) = instability {
        *stability = restored_station_stability(&save);
        shift.stability_band = stability.band;
        shift.station_age_seconds = stability.station_age.max(0.0) as u32;
    }
    if !save.npc_standing.is_empty() {
        shift.npc_standing = save.npc_standing;
    } else if !save.department_standing.is_empty() {
        // Backward-fill: a save from before per-NPC standing existed only
        // knows one scalar per department. Seed every member from it so a
        // returning career's crew do not silently reset to neutral.
        for (department, value) in &save.department_standing {
            for name in department.members() {
                shift.npc_standing.insert((*name).to_string(), *value);
            }
        }
    }
    // Shifts are 1-based; `0` is what a save written before they were numbered
    // deserialises to, and reading it as "shift 1" is exactly right for one.
    shift.shift_number = save.shift_number.max(1);
    shift.opened_at = save.opened_at;
    // Saves written before the bounded scale may contain deeper favor debt or
    // standing above the new ceiling. Normalize once on load so current values
    // and the opening snapshot used by the debrief agree immediately.
    shift.clamp_standing();
    shift.requisition = save.requisition;
    shift.accepting_orders = save.accepting_orders;
    shift.called = save.called;
    if let Some(mut underworld) = underworld {
        underworld.restore(save.underworld_standing);
    }
    if let Some(mut redeemed) = rogue_redeemed {
        redeemed.0 = save.rogue_redeemed;
    }
    if let Some(mut progress) = obsessed_progress {
        progress.0 = save.obsessed_progress;
    }
    if let Some(mut progress) = cult_progress {
        progress.next_stage = save.cult_progress;
        progress.wave_remaining = save.cult_wave_remaining;
        progress.offered_stage = save.cult_offered_stage;
        progress.banked_intel = save.cult_banked_intel;
        progress.finale_started = save.cult_finale_started;
    }
    if let Some(mut progress) = smuggler_progress {
        progress.0 = save.smuggler_progress;
    }
    if let Some(mut progress) = saboteur_progress {
        progress.0 = save.saboteur_progress;
    }
    if let Some(mut progress) = quack_progress {
        progress.0 = save.quack_progress;
    }
    if let Some(mut progress) = bent_guard_progress {
        progress.0 = save.bent_guard_progress;
    }
    // Inserted rather than assigned: this runs `OnEnter(Playing)`, before the
    // first `Update`, so `arc::assign_campaign` sees a campaign already here
    // and leaves it alone. A save from before campaigns existed carries `None`
    // and gets a fresh one rolled instead.
    if let Some(campaign) = save.campaign {
        commands.insert_resource(campaign);
    }
    *addictions = save.addictions;
    if let Some(mut estranged) = estranged {
        estranged.0 = save.estranged;
    }
    if let Some(mut social) = social {
        *social = if save.social.initialized {
            save.social
        } else {
            crate::social::SocialState::migrate_legacy()
        };
    }
    info!(
        "resuming with {} delivered, {} botched",
        shift.succeeded, shift.botched
    );
}

/// The career on disk, or `None` if there is not a readable one.
fn read_progress(path: &std::path::Path) -> Option<ProgressSave> {
    // A corrupt save should cost the player their progress, not the session.
    match crate::saves::read_slot_text(path).map(|text| ron::from_str::<ProgressSave>(&text)) {
        Some(Ok(save)) => Some(save),
        Some(Err(error)) => {
            warn!("ignoring unreadable {}: {error}", path.display());
            None
        }
        None => None,
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

/// What the load list may say about a save's campaign, read the same way and
/// for the same reason as [`progress_summary`].
///
/// Returns `None` for a save whose antagonist is still
/// [`crate::arc::Reveal::Hidden`], not just for one with no campaign. The menu
/// is the one place that could hand the player the answer before they have
/// even loaded the save, so the filter lives here rather than being left to
/// each caller to remember.
pub fn arc_standing(path: &std::path::Path) -> Option<crate::saves::ArcStanding> {
    let (antag, won) = read_progress(path)?.campaign?.menu_standing()?;
    Some(crate::saves::ArcStanding { antag, won })
}

#[allow(clippy::too_many_arguments)]
fn persist_progress(
    shift: Res<Shift>,
    underworld: Option<Res<crate::antagonist::UnderworldStanding>>,
    rogue_redeemed: Option<Res<crate::rogue_security::RogueRedeemed>>,
    obsessed_progress: Option<Res<crate::obsessed::ObsessedProgress>>,
    cult_progress: Option<Res<crate::cult::CultProgress>>,
    smuggler_progress: Option<Res<crate::smuggler::SmugglerProgress>>,
    saboteur_progress: Option<Res<crate::saboteur::SaboteurProgress>>,
    quack_progress: Option<Res<crate::quack::QuackProgress>>,
    bent_guard_progress: Option<Res<crate::bent_guard::BentGuardProgress>>,
    campaign: Option<Res<crate::arc::Campaign>>,
    addictions: Res<crate::addiction::Addictions>,
    estranged: Option<Res<crate::estrangement::Estranged>>,
    instability: Option<Res<crate::instability::Instability>>,
    social: Option<Res<crate::social::SocialState>>,
    slot: Option<Res<SaveSlot>>,
    mut written: ResMut<PersistedProgress>,
) {
    let Some(slot) = slot else {
        return;
    };
    let save = ProgressSave {
        succeeded: shift.succeeded,
        botched: shift.botched,
        npc_standing: shift.npc_standing.clone(),
        // Never written again once a save has moved to `npc_standing` above
        // — kept as a field only so an older save file still parses.
        department_standing: HashMap::new(),
        shift_number: shift.shift_number,
        opened_at: shift.opened_at.clone(),
        requisition: shift.requisition,
        accepting_orders: shift.accepting_orders,
        called: shift.called,
        underworld_standing: underworld.map(|u| u.level()).unwrap_or(0),
        rogue_redeemed: rogue_redeemed.map(|r| r.0).unwrap_or(false),
        obsessed_progress: obsessed_progress.map(|p| p.0).unwrap_or(0),
        cult_progress: cult_progress.as_ref().map(|p| p.next_stage).unwrap_or(0),
        cult_wave_remaining: cult_progress
            .as_ref()
            .map(|p| p.wave_remaining)
            .unwrap_or(0.0),
        cult_offered_stage: cult_progress.as_ref().and_then(|p| p.offered_stage),
        cult_banked_intel: cult_progress.as_ref().map(|p| p.banked_intel).unwrap_or(0),
        cult_finale_started: cult_progress
            .as_ref()
            .map(|p| p.finale_started)
            .unwrap_or(false),
        smuggler_progress: smuggler_progress.map(|p| p.0).unwrap_or(0),
        saboteur_progress: saboteur_progress.map(|p| p.0).unwrap_or(0),
        quack_progress: quack_progress.map(|p| p.0).unwrap_or(0),
        bent_guard_progress: bent_guard_progress.map(|p| p.0).unwrap_or(0),
        campaign: campaign.map(|c| c.clone()),
        addictions: addictions.clone(),
        estranged: estranged.map(|e| e.0.clone()).unwrap_or_default(),
        defeated_count: shift.defeated_count,
        instability: LegacyInstability::default(),
        station_stability: instability.map(|i| i.clone()).unwrap_or_default(),
        evacuated: shift.evacuated,
        social: social.map(|state| state.clone()).unwrap_or_default(),
    };
    if written.0.as_ref() == Some(&save) {
        return;
    }
    let Ok(text) = ron::ser::to_string_pretty(&save, default()) else {
        return;
    };
    written.0 = Some(save);
    slot.write_progress(&text);
}

/// Writes a beaten antagonist into the cross-save unlock file, once per arc.
///
/// Only a **chemist** run counts. Winning an antagonist run means the
/// antagonist got what they wanted, which is the opposite of having stopped
/// them — unlocking on that would let a player unlock the whole roster by
/// losing on purpose from the one side that rewards it.
///
/// Its own system rather than a branch inside [`persist_progress`] because it
/// writes a different file, on a different trigger (once, on resolution)
/// rather than continuously.
///
/// The guard lives on `Campaign` itself (`thwarting_recorded`), not a
/// session-global resource — a global bool meant a career that re-rolled its
/// arc (`arc::reroll_campaign`) could only ever have its *first* win recorded
/// in the same process's lifetime, silently dropping every later one. Also
/// pushes straight into the live [`crate::arc::ThwartedAntags`] and bumps
/// [`Shift::defeated_count`] at the exact moment a win is recorded — the
/// correct, single condition both a same-session re-roll's draw-bias and the
/// career's own difficulty scaling should react to.
fn record_thwarting(
    campaign: Option<ResMut<crate::arc::Campaign>>,
    mut thwarted: ResMut<crate::arc::ThwartedAntags>,
    mut shift: ResMut<Shift>,
) {
    let Some(mut campaign) = campaign else {
        return;
    };
    if campaign.thwarting_recorded {
        return;
    }
    if campaign.mode != crate::arc::Mode::Chemist || campaign.player_won() != Some(true) {
        return;
    }
    crate::saves::record_thwarted(campaign.antag);
    if !thwarted.0.contains(&campaign.antag) {
        thwarted.0.push(campaign.antag);
    }
    shift.defeated_count += 1;
    campaign.thwarting_recorded = true;
}

/// The last career written to disk, so an unchanged one is not rewritten every
/// frame.
///
/// **Was a `Local`, and that was a data-loss bug waiting to happen.** A career
/// opened after another one in the same process inherited the previous save's
/// last-written snapshot; a new career whose totals happened to match it would
/// compare equal and never be written to disk at all.
#[derive(Resource, Default)]
pub struct PersistedProgress(Option<ProgressSave>);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instability::StabilityBand;
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
            minimum_successes: 0,
            minimum_recipes_known: 0,
            exact: false,
            minimum_purity: 0.0,
            patience_scale: 1.0,
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
    fn curriculum_stages_follow_the_authored_progression_curve() {
        let total = 100;
        assert_eq!(
            CareerStage::from_progress(0, 3, total),
            CareerStage::Fundamentals
        );
        assert_eq!(
            CareerStage::from_progress(9, 20, total),
            CareerStage::Fundamentals
        );
        assert_eq!(
            CareerStage::from_progress(10, 20, total),
            CareerStage::Intermediate
        );
        assert_eq!(
            CareerStage::from_progress(29, 40, total),
            CareerStage::Intermediate
        );
        assert_eq!(
            CareerStage::from_progress(30, 40, total),
            CareerStage::Advanced
        );
        assert_eq!(
            CareerStage::from_progress(59, 70, total),
            CareerStage::Advanced
        );
        assert_eq!(
            CareerStage::from_progress(60, 70, total),
            CareerStage::Expert
        );
        assert_eq!(
            CareerStage::from_progress(2, total, total),
            CareerStage::Mastery
        );
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
    fn current_rules_reads_the_tier_off_station_age() {
        let base = config();
        let shift = Shift {
            station_age_seconds: 20 * 60,
            ..Shift::default()
        };

        let rules = current_rules(&base, &shift, 1);
        let expected = ShiftRules::for_tier(&base, &base.ramp, 2);
        assert_eq!(rules, expected);
    }

    #[test]
    fn defeating_antagonists_no_longer_accelerates_station_difficulty() {
        let base = config();
        let solo_zero = current_rules(&base, &Shift::default(), 1);
        let solo_three = current_rules(
            &base,
            &Shift {
                defeated_count: 3,
                ..Shift::default()
            },
            1,
        );
        assert_eq!(solo_three, solo_zero);
    }

    #[test]
    fn critical_condition_has_more_traffic_but_not_less_patience() {
        let base = config();
        let stable = current_rules(&base, &Shift::default(), 1);
        let critical = current_rules(
            &base,
            &Shift {
                stability_band: StabilityBand::Critical,
                ..Shift::default()
            },
            1,
        );
        assert!(critical.gap_seconds.0 < stable.gap_seconds.0);
        assert!(critical.gap_seconds.1 < stable.gap_seconds.1);
        assert_eq!(critical.max_active, stable.max_active + 3);
        assert_eq!(critical.patience_seconds, stable.patience_seconds);
    }

    #[test]
    fn station_age_pressure_caps_after_one_hour() {
        let base = config();
        let one_hour = current_rules(
            &base,
            &Shift {
                station_age_seconds: 60 * 60,
                ..Shift::default()
            },
            1,
        );
        let one_day = current_rules(
            &base,
            &Shift {
                station_age_seconds: 24 * 60 * 60,
                ..Shift::default()
            },
            1,
        );
        assert_eq!(one_hour, one_day);
    }

    #[test]
    fn zero_defeats_is_a_true_no_op_for_current_rules() {
        // Regression guard for the bug this test caught during development:
        // `scale_for_defeated`'s own floor clamp must not silently redo
        // `scale_for_chemists`' team-divided floor with the full, undivided
        // one — which would have widened a full table's gap back up even at
        // `defeated_count == 0`.
        let base = config();
        let shift = Shift::default();
        assert_eq!(
            current_rules(&base, &shift, 1),
            scale_for_chemists(ShiftRules::for_tier(&base, &base.ramp, 0), 1, &base.ramp)
        );
        assert_eq!(
            current_rules(&base, &shift, 4),
            scale_for_chemists(ShiftRules::for_tier(&base, &base.ramp, 0), 4, &base.ramp)
        );
    }

    #[test]
    fn failing_never_makes_the_lab_harder() {
        // The ramp used to read `succeeded + botched`, which meant a chemist
        // who was already drowning got shorter gaps and less patience for it,
        // with no way back. A bad run costs standing, not difficulty.
        let base = config();
        let drowning = Shift {
            succeeded: 0,
            botched: base.ramp.orders_per_tier * 20,
            ..Shift::default()
        };

        assert_eq!(
            current_rules(&base, &drowning, 1),
            ShiftRules::for_tier(&base, &base.ramp, 0),
            "a career of nothing but failures must stay at tier 0"
        );
    }

    #[test]
    fn botching_alongside_success_does_not_accelerate_the_ramp() {
        let base = config();
        let clean = Shift {
            succeeded: base.ramp.orders_per_tier * 3,
            ..Shift::default()
        };
        let messy = Shift {
            botched: 40,
            ..clean.clone()
        };

        assert_eq!(
            current_rules(&base, &clean, 1),
            current_rules(&base, &messy, 1)
        );
    }

    #[test]
    fn a_solo_chemist_sees_todays_numbers_exactly() {
        // `chemist_count <= 1` must be a no-op — solo pacing does not move a
        // single second or slot for this feature, by construction rather
        // than by a separate tuning pass.
        let base = config();
        let shift = Shift::default();

        assert_eq!(
            current_rules(&base, &shift, 1),
            current_rules(&base, &shift, 0),
            "zero and one connected chemist must read identically"
        );
        assert_eq!(
            current_rules(&base, &shift, 1),
            ShiftRules::for_tier(&base, &base.ramp, 0)
        );
    }

    #[test]
    fn more_chemists_shorten_the_gap_and_widen_the_counter() {
        let base = config();
        let shift = Shift::default();

        let solo = current_rules(&base, &shift, 1);
        let duo = current_rules(&base, &shift, 2);
        let quad = current_rules(&base, &shift, 4);

        assert!(
            duo.gap_seconds.0 < solo.gap_seconds.0 && duo.gap_seconds.1 < solo.gap_seconds.1,
            "a second chemist should shorten the gap, not just the tier ramp"
        );
        assert!(
            quad.gap_seconds.0 < duo.gap_seconds.0,
            "each additional chemist should keep shortening the gap"
        );
        assert!(
            quad.max_active > duo.max_active && duo.max_active > solo.max_active,
            "the counter should widen as more chemists join"
        );
    }

    #[test]
    fn the_player_driven_bonus_stops_at_its_own_cap() {
        let base = config();
        let shift = Shift::default();

        let quad = current_rules(&base, &shift, 4);
        let ridiculous = current_rules(&base, &shift, 40);

        assert_eq!(
            quad.max_active, ridiculous.max_active,
            "the player-count bonus must not grow without bound"
        );
        assert_eq!(
            quad.max_active,
            base.max_active + base.ramp.max_active_chemist_cap
        );
    }

    #[test]
    fn a_team_uses_the_solo_floor_as_shared_capacity() {
        let base = config();
        // A late-tier shift, so the tier ramp has already pushed the gap
        // down near `gap_floor` on its own. A team shares that protected solo
        // capacity across its live headcount, but the decay may not push it
        // below that per-person share.
        let shift = Shift {
            succeeded: base.ramp.orders_per_tier * 50,
            ..Shift::default()
        };

        let rules = current_rules(&base, &shift, 4);
        let team_floor = base.ramp.gap_floor / 4.0;
        assert!(rules.gap_seconds.0 >= team_floor);
        assert!(rules.gap_seconds.1 >= team_floor);
    }

    #[test]
    fn advanced_batch_load_hits_the_solo_and_four_player_targets() {
        // A representative advanced order occupies one pair of hands for
        // about thirty seconds including fetching, precursor work, agitation
        // and packaging. Arrival gaps should turn that into the design's
        // target utilization without changing recipes by player count.
        let base = config();
        let shift = Shift::default();
        let mean_gap = |rules: ShiftRules| (rules.gap_seconds.0 + rules.gap_seconds.1) * 0.5;
        let work = 30.0;
        let solo = work / mean_gap(current_rules(&base, &shift, 1));
        let quad_per_player = work / mean_gap(current_rules(&base, &shift, 4)) / 4.0;

        assert!(
            (0.55..=0.70).contains(&solo),
            "solo utilization was {solo:.1}%",
            solo = solo * 100.0
        );
        assert!(
            (0.70..=0.85).contains(&quad_per_player),
            "four-player utilization was {quad_per_player:.1}% per player",
            quad_per_player = quad_per_player * 100.0
        );
    }

    #[test]
    fn the_service_pool_includes_medicine_and_progressive_demolition_emergencies() {
        let base = config();
        let actual: std::collections::BTreeSet<&str> = base
            .requests
            .iter()
            .map(|request| request.reagent.as_str())
            .collect();
        let expected: std::collections::BTreeSet<&str> = [
            "inaprovaline",
            "dylovene",
            "kelotane",
            "bicaridine",
            "hyronalin",
            "tricordrazine",
            "dermaline",
            "dexalin",
            "arithrazine",
            "potassium_iodide",
            "mannitol",
            "saline_glucose",
            "firefighting_foam",
            "chemical_foam",
            "metal_foam",
            "carbon_dioxide",
            "pax",
            "pyrosium",
            "sonic_powder",
            "sorium",
            "liquid_dark_matter",
            "gunpowder",
            "nitroglycerin",
            "rdx",
            "teslium",
            "tatp",
            "libital",
            "lenturi",
            "granibitaluri",
            "rezadone",
            "oculine",
            "diphenhydramine",
            "antihol",
            "modafinil",
            "naloxone",
            "hercuri",
            "syriniver",
            "pyroxadone",
            "regenerative_jelly",
            "penthrite",
            "psicodine",
            "ammoniated_mercury",
            "neurine",
            "seiver",
        ]
        .into_iter()
        .collect();

        assert_eq!(actual, expected);
        assert_eq!(
            base.requests.len(),
            44,
            "order-facing medicines must be unique"
        );
    }

    #[test]
    fn service_demand_covers_every_supported_treatment_family() {
        let orders = config();
        let chemistry = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .expect("chemistry data should parse");
        let demanded: std::collections::BTreeSet<chem_sim::Category> = orders
            .requests
            .iter()
            .flat_map(|request| {
                chemistry
                    .reagents
                    .get(chemistry.reagent(&request.reagent))
                    .categories
                    .iter()
                    .copied()
            })
            .filter(|category| category.is_legitimately_orderable())
            .collect();
        let expected: std::collections::BTreeSet<chem_sim::Category> = [
            chem_sim::Category::Trauma,
            chem_sim::Category::Burns,
            chem_sim::Category::Antitoxins,
            chem_sim::Category::Airloss,
            chem_sim::Category::Radiation,
            chem_sim::Category::Stimulants,
        ]
        .into_iter()
        .collect();

        assert_eq!(
            demanded, expected,
            "every supported treatment family needs recurring service demand"
        );
    }

    #[test]
    fn simulated_career_stages_always_have_reachable_required_work() {
        let orders = config();
        let chemistry = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .expect("chemistry data should parse");
        let mut knowledge = Knowledge::new(&chemistry);
        let saves = [
            (0, 3, CareerStage::Fundamentals),
            (10, 12, CareerStage::Intermediate),
            (30, 24, CareerStage::Advanced),
            (60, 36, CareerStage::Expert),
            (80, chemistry.reactions.len(), CareerStage::Mastery),
        ];
        let mut previous_pool = 0;

        for (successes, target_known, expected_stage) in saves {
            while knowledge.known_count() < target_known {
                let mut frontier = knowledge.frontier(&chemistry);
                frontier.sort_by_key(|id| chemistry.reactions.get(*id).key.clone());
                let next = frontier.first().copied().unwrap_or_else(|| {
                    panic!(
                        "stage simulation stalled at {} methods",
                        knowledge.known_count()
                    )
                });
                knowledge.learn(next);
            }

            assert_eq!(
                CareerStage::from_progress(
                    successes,
                    knowledge.known_count(),
                    chemistry.reactions.len()
                ),
                expected_stage
            );
            let makeable = knowledge.available_reagents(&chemistry);
            let pool = orders
                .requests
                .iter()
                .filter(|request| request.minimum_successes <= successes)
                .filter(|request| request.minimum_recipes_known <= knowledge.known_count())
                .filter(|request| {
                    chemistry
                        .reagents
                        .id_of(&request.reagent)
                        .is_some_and(|id| makeable.contains(&id))
                })
                .count();
            assert!(
                pool > 0,
                "{} save has no required work",
                expected_stage.label()
            );
            assert!(
                pool >= previous_pool,
                "request pool shrank from {previous_pool} to {pool} at {}",
                expected_stage.label()
            );
            previous_pool = pool;
        }
    }

    #[test]
    fn exact_purity_demands_form_a_staged_mastery_curve() {
        let base = config();
        let maximum_quality_at = |successes: u32, recipes_known: usize| {
            base.requests
                .iter()
                .filter(|request| {
                    request.exact
                        && request.minimum_successes <= successes
                        && request.minimum_recipes_known <= recipes_known
                })
                .map(|request| request.minimum_purity)
                .fold(0.0_f32, f32::max)
        };

        for (successes, expected) in [(0, 0.0), (10, 0.0), (15, 0.75), (30, 0.85), (45, 0.90)] {
            assert!(
                (maximum_quality_at(successes, usize::MAX) - expected).abs() < 0.001,
                "after {successes} successes the quality frontier should be {expected:.0}%"
            );
        }
        assert!((maximum_quality_at(60, 23) - 0.90).abs() < 0.001);
        assert!((maximum_quality_at(60, 24) - 0.95).abs() < 0.001);

        let mastery = base
            .requests
            .iter()
            .find(|request| request.reagent == "lenturi")
            .unwrap();
        assert!(mastery.patience_scale >= 2.0);
        assert_eq!(
            mastery.minimum_recipes_known,
            crate::machines::HPLC_RECIPE_REQUIREMENT,
            "the mastery request must not precede analyzer calibration"
        );
        assert!(mastery.amounts.iter().all(|amount| amount % 4 == 0));
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
        .init_resource::<PendingRestock>()
        .insert_resource(Knowledge::new(&chemistry()))
        .insert_resource(ChemDb(chemistry()))
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
        let pick = app.world().resource::<CurrentForecast>().0.clone().unwrap();
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

    /// The individual-NPC sibling of [`with_standing`].
    fn with_npc_standing(name: &str, standing: i32) -> (App, Entity) {
        let mut app = shift_app();
        let board = board(&mut app);
        app.world_mut()
            .resource_mut::<Shift>()
            .adjust_npc(name, standing);
        (app, board)
    }

    /// The individual-NPC sibling of [`requisition`].
    fn npc_requisition(app: &mut App, board: Entity, kind: NpcRequisitionKind) {
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: NpcRequisitionRequested { board, kind },
        });
        app.update();
    }

    #[test]
    fn each_confrontation_item_is_bought_from_the_right_seller_at_the_right_price() {
        let cases = [
            (
                NpcRequisitionKind::LindqvistSyringeGun,
                "Tech Lindqvist",
                SYRINGE_GUN_COST,
                ContainerKind::SyringeGun,
            ),
            (
                NpcRequisitionKind::LindqvistPressureSprayer,
                "Tech Lindqvist",
                PRESSURE_SPRAYER_COST,
                ContainerKind::PressureSprayer,
            ),
            (
                NpcRequisitionKind::SatoWaterGun,
                "Miner Sato",
                WATER_GUN_COST,
                ContainerKind::WaterGun,
            ),
        ];
        for (kind, seller, cost, spawned_kind) in cases {
            let (mut app, board) = with_npc_standing(seller, cost);
            npc_requisition(&mut app, board, kind);

            assert_eq!(
                app.world().resource::<Shift>().npc_standing(seller),
                0,
                "{seller} should be paid exactly {cost} for {spawned_kind:?}"
            );
            let world = app.world_mut();
            let mut query = world.query::<&Container>();
            assert!(
                query
                    .iter(world)
                    .any(|container| container.kind == spawned_kind),
                "buying {spawned_kind:?} from {seller} should spawn one"
            );
        }
    }

    #[test]
    fn a_confrontation_item_can_be_called_in_on_personal_goodwill_debt() {
        let (mut app, board) = with_npc_standing("Tech Lindqvist", SYRINGE_GUN_COST - 1);
        npc_requisition(&mut app, board, NpcRequisitionKind::LindqvistSyringeGun);

        assert_eq!(
            app.world()
                .resource::<Shift>()
                .npc_standing("Tech Lindqvist"),
            -1,
            "the full favor cost should become personal goodwill debt"
        );
        let world = app.world_mut();
        let mut query = world.query::<&Container>();
        assert!(
            query
                .iter(world)
                .any(|container| container.kind == ContainerKind::SyringeGun),
            "the favor should still provide the item"
        );
    }

    #[test]
    fn an_estranged_seller_wont_sell_a_confrontation_item_either() {
        // The blanket estrangement gate in `apply_npc_requisition` applies to
        // every kind, not just the packs it was originally built for.
        let (mut app, board) = with_npc_standing("Miner Sato", WATER_GUN_COST);
        app.insert_resource(crate::estrangement::Estranged(
            [String::from("Miner Sato")].into_iter().collect(),
        ));

        npc_requisition(&mut app, board, NpcRequisitionKind::SatoWaterGun);

        assert_eq!(
            app.world().resource::<Shift>().npc_standing("Miner Sato"),
            WATER_GUN_COST,
            "an estranged seller's shop is locked, standing untouched"
        );
        let world = app.world_mut();
        let mut query = world.query::<&Container>();
        assert!(!query
            .iter(world)
            .any(|container| container.kind == ContainerKind::WaterGun));
    }

    #[test]
    fn a_requisition_can_be_called_in_immediately() {
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
    fn a_department_requisition_records_its_full_cost_as_debt() {
        let (mut app, board) = with_standing(Department::Engineering, 1);
        let before = app.world().resource::<Knowledge>().research_points;
        requisition(&mut app, board, RequisitionKind::ResearchGrant);

        let world = app.world();
        assert_eq!(
            world.resource::<Shift>().standing(Department::Engineering),
            1 - RequisitionKind::ResearchGrant.cost()
        );
        assert_eq!(
            world.resource::<Knowledge>().research_points,
            before + RESEARCH_GRANT
        );
    }

    #[test]
    fn repeated_department_favors_stop_at_the_standing_floor() {
        let (mut app, board) = with_standing(Department::Cargo, crate::orders::STANDING_FLOOR);
        requisition(&mut app, board, RequisitionKind::Glassware);
        requisition(&mut app, board, RequisitionKind::Glassware);

        assert_eq!(
            app.world().resource::<Shift>().standing(Department::Cargo),
            crate::orders::STANDING_FLOOR
        );
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

        assert_eq!(
            app.world().resource::<Shift>().standing(Department::Cargo),
            crate::orders::STANDING_CEILING
        );
    }

    #[test]
    fn an_instant_gift_drops_a_filled_bottle_at_the_counter() {
        let (mut app, board) = with_standing(Department::Medical, 10);
        requisition(&mut app, board, RequisitionKind::AntitoxinCrate);

        let world = app.world_mut();
        let mut query = world.query::<(&Container, &Transform)>();
        let (container, transform) = query
            .iter(world)
            .find(|(container, _)| container.kind == ContainerKind::Bottle)
            .expect("an Antitoxin Crate should drop a bottle");
        assert!(
            !container.solution.is_empty(),
            "the bottle should arrive already filled, not just spawned"
        );
        assert_eq!(
            transform.translation.z, COUNTER_DROP_Z,
            "should land at the counter's drop spot"
        );
    }

    #[test]
    fn quiet_word_zeroes_both_suspicion_meters() {
        let (mut app, board) = with_standing(Department::Security, 10);
        app.insert_resource({
            let mut m = SecuritySuspicion::default();
            m.restore(7);
            m
        });
        app.insert_resource(CarriedSuspicion(0.5));

        requisition(&mut app, board, RequisitionKind::QuietWord);

        assert_eq!(app.world().resource::<SecuritySuspicion>().level(), 0);
        assert_eq!(app.world().resource::<CarriedSuspicion>().0, 0.0);
    }

    /// Reads one field off a banked [`crate::orders::Requisition`].
    type WardField = fn(&crate::orders::Requisition) -> u32;

    #[test]
    fn banked_ward_and_bonus_requisitions_stack() {
        // All five follow the same shape as `a_glassware_requisition_stacks`:
        // buy twice, the bank holds two.
        let cases: [(RequisitionKind, Department, WardField); 5] = [
            (RequisitionKind::SecondOpinion, Department::Medical, |r| {
                r.quack_wards
            }),
            (
                RequisitionKind::LookTheOtherWay,
                Department::Security,
                |r| r.raid_wards,
            ),
            (RequisitionKind::ChainOfCustody, Department::Cargo, |r| {
                r.smuggler_wards
            }),
            (
                RequisitionKind::SecondInspection,
                Department::Engineering,
                |r| r.saboteur_wards,
            ),
            (RequisitionKind::CompedRound, Department::Service, |r| {
                r.patience_bonus_orders
            }),
        ];
        for (kind, department, field) in cases {
            let (mut app, board) = with_standing(department, 20);
            requisition(&mut app, board, kind);
            requisition(&mut app, board, kind);
            assert_eq!(
                field(&app.world().resource::<Shift>().requisition),
                2,
                "{kind:?} should stack like every other banked requisition"
            );
        }
    }

    // -- the accepting-orders toggle --------------------------------------

    #[test]
    fn either_chemist_can_flip_the_sign() {
        let mut app = shift_app();
        let board = board(&mut app);
        assert!(
            !app.world().resource::<Shift>().accepting_orders,
            "the sign starts down"
        );

        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: ToggleAcceptingOrders { board },
        });
        app.update();
        assert!(app.world().resource::<Shift>().accepting_orders);

        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: ToggleAcceptingOrders { board },
        });
        app.update();
        assert!(!app.world().resource::<Shift>().accepting_orders);
    }

    // -- calling a shift --------------------------------------------------

    /// Somebody at the counter holding an order. Nothing else about them
    /// matters here — `handle_call_it_a_shift` only counts.
    fn waiting_crew(app: &mut App) -> Entity {
        app.world_mut()
            .spawn(crate::orders::Order {
                reagent: chem_sim::ReagentId(0),
                specific: false,
                minimum_purity: 0.0,
                amount: chem_sim::Units::whole(20),
                plea: String::new(),
                patience: 120.0,
                waited: 0.0,
            })
            .id()
    }

    fn call_it(app: &mut App, board: Entity) {
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: CallItAShift { board },
        });
        app.update();
    }

    fn open_up(app: &mut App, board: Entity) {
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: OpenUpAgain { board },
        });
        app.update();
    }

    /// Ensures the sign is down, regardless of which way `Shift::default`
    /// currently starts it — a plain toggle would silently flip the wrong
    /// way if that default ever changes again, exactly as it did once
    /// already (the sign now starts down; this used to be a bare toggle).
    fn put_the_sign_down(app: &mut App, board: Entity) {
        if app.world().resource::<Shift>().accepting_orders {
            app.world_mut().write_message(FromClient {
                client_id: ClientId::Server,
                message: ToggleAcceptingOrders { board },
            });
            app.update();
        }
    }

    /// The other absolute direction — see [`put_the_sign_down`].
    fn put_the_sign_up(app: &mut App, board: Entity) {
        if !app.world().resource::<Shift>().accepting_orders {
            app.world_mut().write_message(FromClient {
                client_id: ClientId::Server,
                message: ToggleAcceptingOrders { board },
            });
            app.update();
        }
    }

    #[test]
    fn a_shift_cannot_be_called_over_someone_still_at_the_counter() {
        // The rule the button draws from, checked here on the authority: a
        // client is free to send the message whenever it likes, and calling a
        // shift out from under a waiting crew member would botch their order
        // for them.
        let mut app = shift_app();
        let board = board(&mut app);
        let customer = waiting_crew(&mut app);
        put_the_sign_down(&mut app, board);

        call_it(&mut app, board);
        assert!(
            !app.world().resource::<Shift>().called,
            "the counter was not clear"
        );

        app.world_mut().entity_mut(customer).despawn();
        call_it(&mut app, board);
        assert!(app.world().resource::<Shift>().called);
    }

    #[test]
    fn a_shift_cannot_be_called_with_the_sign_still_up() {
        // Two stages on purpose. Ending a shift while crew are still walking
        // in would strand whoever arrives the next second.
        let mut app = shift_app();
        let board = board(&mut app);
        put_the_sign_up(&mut app, board);

        call_it(&mut app, board);
        assert!(!app.world().resource::<Shift>().called);
    }

    #[test]
    fn only_the_board_can_end_a_shift() {
        // Same trust boundary as `only_the_board_can_be_requisitioned_against`.
        let mut app = shift_app();
        let board = board(&mut app);
        let grinder = app
            .world_mut()
            .spawn(Machine::new(MachineKind::Grinder))
            .id();
        put_the_sign_down(&mut app, board);

        call_it(&mut app, grinder);
        assert!(!app.world().resource::<Shift>().called);
    }

    #[test]
    fn opening_up_again_numbers_the_next_shift_and_resnapshots() {
        let mut app = shift_app();
        let board = board(&mut app);
        assert_eq!(app.world().resource::<Shift>().shift_number, 1);

        // A shift's work.
        app.world_mut().resource_mut::<Shift>().succeeded = 6;
        put_the_sign_down(&mut app, board);
        call_it(&mut app, board);

        let knowledge = app.world().resource::<Knowledge>().known_count();
        let report = shift_report(app.world().resource::<Shift>(), app.world().resource());
        assert_eq!(report.number, 1);
        assert_eq!(report.delivered, 6);

        open_up(&mut app, board);
        let shift = app.world().resource::<Shift>();
        assert_eq!(shift.shift_number, 2);
        assert!(shift.accepting_orders, "the sign goes back up");
        assert!(!shift.called);
        assert_eq!(
            shift.opened_at,
            Some(ShiftSnapshot {
                succeeded: 6,
                botched: 0,
                // Dense now, not sparse: `snapshot()` derives every
                // department's average fresh rather than cloning whichever
                // ones had ever been directly touched — untouched members
                // read as 0 either way, so `shift_report`'s own comparison
                // is unaffected.
                department_standing: Department::ALL
                    .into_iter()
                    .map(|department| (department, 0))
                    .collect(),
                research_points: 0,
                recipes_known: knowledge,
                stability_band: StabilityBand::Stable,
            }),
            "shift two starts from where shift one finished"
        );

        // And the second shift's debrief is its own, not a running total.
        let report = shift_report(shift, app.world().resource());
        assert_eq!(report.number, 2);
        assert_eq!(report.delivered, 0);
    }

    #[test]
    fn a_debrief_reports_the_shift_rather_than_the_career() {
        // The whole point of the beat. A career total only ever climbs, so it
        // can never say whether the last hour went well.
        let mut shift = Shift {
            succeeded: 40,
            botched: 9,
            shift_number: 4,
            ..default()
        };
        shift.adjust(Department::Medical, 6);
        shift.adjust(Department::Cargo, -3);
        shift.opened_at = Some(ShiftSnapshot {
            succeeded: 33,
            botched: 8,
            department_standing: [(Department::Medical, 4)].into_iter().collect(),
            research_points: 12,
            recipes_known: 5,
            stability_band: StabilityBand::Stable,
        });

        let mut knowledge = Knowledge::new(&chemistry());
        knowledge.award_research(20);

        let report = shift_report(&shift, &knowledge);
        assert_eq!(report.number, 4);
        assert_eq!(report.delivered, 7);
        assert_eq!(report.botched, 1);
        assert_eq!(report.research, 8, "20 banked against 12 at open");
        // Only what moved, and Medical moved by the difference, not to 6.
        assert_eq!(
            report.standing,
            vec![(Department::Medical, 2), (Department::Cargo, -3)]
        );
        assert!(!report.is_quiet());
    }

    #[test]
    fn a_debrief_reports_qualitative_station_condition_change() {
        let mut shift = Shift {
            stability_band: StabilityBand::Stable,
            ..default()
        };
        shift.opened_at = Some(ShiftSnapshot {
            stability_band: StabilityBand::Unstable,
            ..default()
        });
        let knowledge = Knowledge::new(&chemistry());
        assert_eq!(
            shift_report(&shift, &knowledge).condition_change,
            Some(ConditionChange::Improved)
        );

        shift.stability_band = StabilityBand::Critical;
        assert_eq!(
            shift_report(&shift, &knowledge).condition_change,
            Some(ConditionChange::Deteriorated)
        );
    }

    #[test]
    fn a_shift_with_no_snapshot_reports_nothing_rather_than_everything() {
        // `opened_at` is `None` for one frame at startup and for a save
        // written before shifts were numbered. Falling back to a zero snapshot
        // would report the entire career as this shift's work.
        let shift = Shift {
            succeeded: 40,
            opened_at: None,
            ..default()
        };
        let report = shift_report(&shift, &Knowledge::new(&chemistry()));
        assert_eq!(
            report.delivered, 40,
            "a zero snapshot is the honest reading of 'no snapshot'"
        );
        // ...which is exactly why `open_the_shift` takes one immediately.
    }

    #[test]
    fn the_opening_snapshot_is_taken_once_and_not_retaken_every_frame() {
        // Retaking it would make every debrief empty: the difference against a
        // snapshot taken this frame is always zero.
        let mut app = shift_app();
        assert!(
            app.world().resource::<Shift>().opened_at.is_some(),
            "taken on the first frame in the lab"
        );

        app.world_mut().resource_mut::<Shift>().succeeded = 3;
        app.update();
        app.update();

        let shift = app.world().resource::<Shift>();
        assert_eq!(shift.opened_at.as_ref().unwrap().succeeded, 0);
        assert_eq!(shift_report(shift, app.world().resource()).delivered, 3);
    }

    #[test]
    fn the_call_it_rule_is_the_one_the_button_draws_from() {
        let open = Shift {
            accepting_orders: true,
            ..default()
        };
        assert!(!can_call_it(&open, 0), "the sign is still up");

        let closing = Shift {
            accepting_orders: false,
            ..default()
        };
        assert!(!can_call_it(&closing, 1), "somebody is still waiting");
        assert!(can_call_it(&closing, 0));

        let already = Shift {
            accepting_orders: false,
            called: true,
            ..default()
        };
        assert!(!can_call_it(&already, 0), "already called");
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

    // -- the save format ---------------------------------------------------

    #[test]
    fn a_progress_file_written_before_campaigns_still_loads() {
        // Every field on `ProgressSave` is `#[serde(default)]` precisely so
        // that adding one never costs an existing player their career. This is
        // the guard on that: the literal shape a save had before the arc
        // existed must still parse, and must roll a fresh campaign rather than
        // resuming one it never had.
        let legacy = r#"(
            succeeded: 41,
            botched: 7,
            department_standing: {},
            underworld_standing: 6,
            rogue_redeemed: true,
            obsessed_progress: 2,
            cult_progress: 1,
        )"#;

        let save: ProgressSave = ron::from_str(legacy).expect("an older save must still parse");

        assert_eq!(save.succeeded, 41);
        assert_eq!(save.underworld_standing, 6);
        assert_eq!(save.cult_progress, 1);
        assert!(
            save.campaign.is_none(),
            "an older save has no campaign, so `arc::assign_campaign` should roll one"
        );
        assert!(
            !save.social.initialized,
            "a pre-social save must retain the migration marker"
        );
    }

    #[test]
    fn social_opinions_round_trip_only_inside_the_owning_save() {
        let mut social = crate::social::SocialState::fresh();
        let relationship = social.relationship_mut(crate::social::SATO);
        relationship.tier = crate::social::RelationshipTier::Trusted;
        relationship.helpful = 2;
        social.dialogue_history.insert(
            crate::social::SATO.into(),
            vec![crate::social::DialogueLine {
                speaker: crate::social::SATO.into(),
                text: "Shared save transcript.".into(),
            }],
        );
        let save = ProgressSave {
            social: social.clone(),
            ..default()
        };

        let text = ron::ser::to_string(&save).unwrap();
        let restored: ProgressSave = ron::from_str(&text).unwrap();
        assert_eq!(restored.social, social);

        let separate_new_save = crate::social::SocialState::fresh();
        assert_eq!(
            separate_new_save.relationships[crate::social::SATO].tier,
            crate::social::RelationshipTier::Neutral
        );
        assert_eq!(
            separate_new_save.relationships[crate::social::SATO].helpful,
            0
        );
        assert!(separate_new_save.dialogue_history.is_empty());
    }

    #[test]
    fn a_campaign_survives_a_round_trip_through_the_save_file() {
        let mut campaign =
            crate::arc::Campaign::new(crate::arc::AntagId::Blob, crate::arc::Mode::Antagonist, 3);
        campaign.plot = 62;
        campaign.reveal = crate::arc::Reveal::Named;
        campaign.countered = vec![true, false, true];

        let save = ProgressSave {
            campaign: Some(campaign.clone()),
            ..default()
        };
        let text = ron::ser::to_string_pretty(&save, default()).unwrap();
        let back: ProgressSave = ron::from_str(&text).unwrap();

        assert_eq!(back.campaign, Some(campaign));
    }

    #[test]
    fn timed_cult_state_survives_a_save_round_trip() {
        let save = ProgressSave {
            cult_progress: 2,
            cult_wave_remaining: 317.5,
            cult_offered_stage: Some(2),
            cult_banked_intel: 1,
            cult_finale_started: true,
            ..default()
        };
        let text = ron::ser::to_string(&save).unwrap();
        let restored: ProgressSave = ron::from_str(&text).unwrap();
        assert_eq!(restored.cult_progress, 2);
        assert_eq!(restored.cult_wave_remaining, 317.5);
        assert_eq!(restored.cult_offered_stage, Some(2));
        assert_eq!(restored.cult_banked_intel, 1);
        assert!(restored.cult_finale_started);
    }

    #[test]
    fn legacy_instability_migrates_to_inverse_station_stability() {
        let save: ProgressSave = ron::from_str(
            r#"(
                instability: (level: 68, tier: Breaking),
            )"#,
        )
        .unwrap();
        let stability = restored_station_stability(&save);
        assert_eq!(stability.value, 32.0);
        assert_eq!(stability.band, crate::instability::StabilityBand::Unstable);
        assert_eq!(stability.station_age, 0.0);
    }

    #[test]
    fn station_stability_and_age_survive_a_save_round_trip() {
        let expected = crate::instability::StationStability {
            value: 43.0,
            band: crate::instability::StabilityBand::Unstable,
            station_age: 7_200.0,
            decay_accumulator: 0.75,
        };
        let save = ProgressSave {
            station_stability: expected.clone(),
            ..default()
        };
        let text = ron::ser::to_string(&save).unwrap();
        let restored: ProgressSave = ron::from_str(&text).unwrap();
        assert_eq!(restored_station_stability(&restored), expected);
    }

    #[test]
    fn a_shift_resume_keeps_its_live_service_state() {
        let save = ProgressSave {
            requisition: Requisition {
                glassware: 2,
                quack_wards: 1,
                patience_bonus_orders: 1,
                ..default()
            },
            accepting_orders: true,
            called: true,
            ..default()
        };
        let text = ron::ser::to_string(&save).unwrap();
        let restored: ProgressSave = ron::from_str(&text).unwrap();

        assert!(restored.accepting_orders);
        assert!(restored.called);
        assert_eq!(restored.requisition.glassware, 2);
        assert_eq!(restored.requisition.quack_wards, 1);
        assert_eq!(restored.requisition.patience_bonus_orders, 1);
    }

    #[test]
    fn the_load_list_never_names_an_antagonist_the_player_has_not_worked_out() {
        // `menu_standing` is the one place the menu could hand the player the
        // answer before they have even loaded the save.
        let hidden =
            crate::arc::Campaign::new(crate::arc::AntagId::Cult, crate::arc::Mode::Chemist, 2);
        assert!(
            hidden.menu_standing().is_none(),
            "a save whose antagonist is still hidden must say nothing"
        );

        let mut suspected = hidden.clone();
        suspected.reveal = crate::arc::Reveal::Suspected;
        assert!(suspected.menu_standing().is_some());

        // A resolved arc is always safe to name, whatever the reveal reached:
        // it is over, and the load list has to be able to say so.
        let mut lost = hidden.clone();
        lost.outcome = Some(crate::arc::ArcOutcome::PlotSucceeded);
        assert_eq!(
            lost.menu_standing(),
            Some((crate::arc::AntagId::Cult, Some(false)))
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
