//! Requests from the crew, and grading what you hand back.
//!
//! Grading lives in [`grade`], a pure function with no ECS involvement, so the
//! rules that decide whether a shift went well can be tested exhaustively.

use std::collections::{HashMap, HashSet};

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use bevy_replicon::prelude::*;
use chem_sim::{Category, ReagentId, Route, Solution, Units};
use rand::prelude::*;
use serde::{Deserialize, Serialize};

use crate::body::{Bloodstream, Body};
use crate::chem_data::ChemDb;
use crate::chem_world::{assess_exposure, ChemicalExposure, ExposureSource};
use crate::containers::{
    spawn_container, Container, ContainerKind, HeldBy, InSlot, InSlotB, InSlotC, InventorySlot,
    Stored,
};
use crate::crew::{
    recall_or_spawn_crew_member, AvailableResidents, CrewDef, CrewMember, CrewPhase, CrewRoute,
};
use crate::interaction::{InteractRequested, Interactable};
use crate::knowledge::{research_for_delivery_at_purity, Knowledge};
use crate::lab::{DeliveryLane, DeliveryStation, DeliveryStations, COUNTER_SPOT};
#[cfg(test)]
use crate::machines::MachineSlot;
use crate::machines::{
    chemist_entity, slotted_container, slotted_container_b, slotted_container_c, Machine,
    MachineKind,
};
use crate::net::is_authority;
use crate::player::Chemist;
use crate::produce::{Produce, ProduceCatalog};
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::shift::{current_rules, weighted_pick, CurrentForecast};
use crate::utility_ai::{ControlOwner, UtilityAgent};
use crate::AppState;

/// How often a clean delivery earns a sample vial of something unfamiliar.
///
/// Dropped from 0.35 (M11) alongside the reagent-unlock and hint-cost tuning
/// — at 35% this was a second full-strength unlock currency running in
/// parallel with research points, undermining the point of slowing the other
/// two down. Kept nonzero on purpose: it stays a nice surprise, just not the
/// primary route to a free recipe anymore.
const SAMPLE_VIAL_CHANCE: f64 = 0.15;

pub struct OrderPlugin;

impl Plugin for OrderPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            RonAssetPlugin::<CrewList>::new(&["crew.ron"]),
            RonAssetPlugin::<OrderConfig>::new(&["orders.ron"]),
        ))
        .add_message::<OrderResolved>()
        .add_message::<FulfillmentApplied>()
        .add_message::<ChemicalExposure>()
        .add_server_message::<ShiftSync>(Channel::Ordered)
        .init_resource::<Shift>()
        .add_systems(Startup, start_loading)
        .add_systems(OnEnter(AppState::Playing), rearm_arrival_clocks)
        .add_systems(
            Update,
            (
                // Orders, crew and grading are the server's business. A client
                // sees the crew arrive through replication.
                // The crew roster is not server state: both ends need it to
                // know what a crew member looks like at the counter, so it is
                // promoted everywhere, exactly like the produce catalog.
                promote_station_data,
                (
                    // Crew arrive continuously — the only gate left is the
                    // player's own "not accepting requests" toggle, checked
                    // inside `generate_orders` itself against `Shift`.
                    generate_orders.run_if(crate::session::career_session),
                    // Its rarer, exact-asking sibling — same sign, same
                    // queue cap, its own much slower clock.
                    generate_specific_orders.run_if(crate::session::career_session),
                    // Deliberately not gated the same way: the sign stops new
                    // arrivals, not the clock on whoever is already waiting.
                    expire_orders.run_if(crate::session::career_session),
                    handle_delivery,
                    handle_window_delivery,
                    leave_sample_vials.run_if(crate::session::career_session),
                    broadcast_shift,
                )
                    .chain()
                    .run_if(is_authority),
                apply_shift.run_if(in_state(ClientState::Connected)),
                apply_carried_fulfillments
                    .after(crate::crew::walk_route)
                    .run_if(is_authority),
            )
                .run_if(in_state(AppState::Playing)),
        );
    }
}

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

#[derive(Asset, TypePath, Deserialize)]
#[serde(transparent)]
pub struct CrewList(pub Vec<CrewDef>);

#[derive(Asset, TypePath, Deserialize)]
pub struct OrderConfig {
    pub first_order_delay: f32,
    pub gap_seconds: (f32, f32),
    pub patience_seconds: (f32, f32),
    pub max_active: usize,
    /// Chance that a request which offers both ordinary and bulk quantities
    /// draws from its above-30u pool. Most requests have no bulk pool at all,
    /// so the overall rate stays well below this value.
    #[serde(default = "default_bulk_amount_chance")]
    pub bulk_amount_chance: f64,
    /// How often the station briefing redraws. There is no shift boundary to
    /// redraw it against any more, so it runs on its own clock.
    #[serde(default = "default_forecast_seconds")]
    pub forecast_seconds: (f32, f32),
    /// Multiplied onto the *current* legitimate order gap to get the clock
    /// between "asks for it by name" orders — the same shape
    /// `antagonist::AntagonistScript::gap_multiplier` uses, and defaulted to
    /// the same range, so an honest specific ask is exactly as rare as an
    /// illicit one unless deliberately tuned apart. See
    /// [`generate_specific_orders`].
    #[serde(default = "default_specific_gap_multiplier")]
    pub specific_gap_multiplier: (f32, f32),
    /// How the numbers above tighten as the career goes on.
    #[serde(default)]
    pub ramp: RampDef,
    /// What cargo keeps the lab stocked with.
    #[serde(default)]
    pub supply: SupplyDef,
    /// What the station expects, briefed periodically over the radio.
    #[serde(default)]
    pub forecasts: Vec<ForecastDef>,
    pub requests: Vec<RequestDef>,
}

fn default_forecast_seconds() -> (f32, f32) {
    (180.0, 300.0)
}

fn default_bulk_amount_chance() -> f64 {
    0.05
}

/// Matches `antagonist.ron`'s own default — see
/// `OrderConfig::specific_gap_multiplier`.
fn default_specific_gap_multiplier() -> (f32, f32) {
    (6.0, 10.0)
}

#[derive(Clone, Debug, Deserialize)]
pub struct RequestDef {
    pub reagent: String,
    pub amounts: Vec<u32>,
    pub plea: String,
    /// The plea when this same request is drawn as a specific ask instead of
    /// a lenient category one — see [`Order::specific`]. Names the chemical
    /// directly, the way `plea` no longer does.
    pub specific_plea: String,
    /// What kind of briefing this request belongs to, so a forecast can lean
    /// on it. An untagged request can never be forecast — see
    /// `every_request_carries_a_theme`.
    #[serde(default)]
    pub themes: Vec<String>,
    /// Successful deliveries required before this request joins the pool.
    #[serde(default)]
    pub minimum_successes: u32,
    /// Recorded methods required before the requested chemistry is fair. This
    /// is independent of equipment availability: the analyzer and its HPLC
    /// purification are usable from the start.
    #[serde(default)]
    pub minimum_recipes_known: usize,
    /// Exact industrial/emergency asks do not accept a category substitute.
    #[serde(default)]
    pub exact: bool,
    /// Minimum purity accepted for the requested reagent. Zero preserves the
    /// existing forgiving behavior for ordinary service orders.
    #[serde(default)]
    pub minimum_purity: f32,
    /// Per-request time allowance for unusually involved syntheses.
    #[serde(default = "default_request_patience_scale")]
    pub patience_scale: f32,
}

fn default_request_patience_scale() -> f32 {
    1.0
}

/// How the difficulty tightens as the career goes on.
///
/// There is no shift boundary any more — crew arrive continuously — so the
/// tier that drives every field below comes from successful deliveries divided
/// by `orders_per_tier`, computed fresh wherever it is needed. Failures cost
/// standing but never accelerate difficulty. See [`current_rules`].
#[derive(Clone, Debug, Deserialize)]
pub struct RampDef {
    /// Orders resolved before the difficulty steps up once.
    pub orders_per_tier: u32,
    /// Multiplied into the gap between orders, once per tier elapsed.
    pub gap_scale: f32,
    pub gap_floor: f32,
    pub patience_scale: f32,
    pub patience_floor: f32,
    /// One more crew member at the counter every this many shifts.
    pub max_active_every: u32,
    pub max_active_cap: usize,
    /// How often the crew ask for something just past what the chemist knows.
    ///
    /// Every order being makeable is a treadmill; every order being impossible
    /// is a wall. A minority of stretch requests is what sends the player to
    /// the bench to work something out, and that minority grows as they get
    /// better at the job.
    pub stretch_base: f64,
    pub stretch_step: f64,
    pub stretch_cap: f64,
    /// Clean deliveries before optional development requests may appear.
    #[serde(default = "default_stretch_after_successes")]
    pub stretch_after_successes: u32,
    /// Extra time granted to an optional development request.
    #[serde(default = "default_stretch_patience_scale")]
    pub stretch_patience_scale: f32,
    /// Standing lost when an optional development request is ignored.
    #[serde(default = "default_stretch_expiry_standing")]
    pub stretch_expiry_standing: i32,
    /// How much harder a forecast leans on the requests it names. `2.0` makes a
    /// themed request three times as likely as an untagged one.
    pub forecast_boost: f64,
    /// Multiplied into the gap between orders once per chemist beyond the
    /// first, same shape as `gap_scale`'s tier decay but keyed on the live
    /// headcount instead — see [`current_rules`]. `1.0` at solo by
    /// construction (zero chemists beyond the first), so this is inert
    /// unless someone else is actually in the lab.
    #[serde(default = "default_chemist_gap_scale")]
    pub chemist_gap_scale: f32,
    /// How many more crew the counter can support per chemist beyond the
    /// first, on top of whatever the tier ramp already grants.
    #[serde(default = "default_max_active_per_chemist")]
    pub max_active_per_chemist: usize,
    /// Ceiling on the *player-driven* `max_active` bonus alone — independent
    /// of `max_active_cap`, which still governs the tier-driven ceiling a
    /// solo chemist eventually hits. Keeps a full table from ballooning the
    /// queue into something nobody can read.
    #[serde(default = "default_max_active_chemist_cap")]
    pub max_active_chemist_cap: usize,
    /// Multiplied into the gap between orders once per antagonist the career
    /// has already thwarted (`Shift::defeated_count`), same shape as
    /// `gap_scale`'s tier decay and `chemist_gap_scale`'s headcount decay —
    /// see `shift::current_rules`. `1.0` at zero defeats by construction, so
    /// this is inert until the career's first win.
    #[serde(default = "default_defeated_gap_scale")]
    pub defeated_gap_scale: f32,
}

/// Matches `station.orders.ron`'s own default — see
/// `RampDef::chemist_gap_scale`.
fn default_chemist_gap_scale() -> f32 {
    0.60
}

/// Matches `station.orders.ron`'s own default — see
/// `RampDef::max_active_per_chemist`.
fn default_max_active_per_chemist() -> usize {
    1
}

/// Matches `station.orders.ron`'s own default — see
/// `RampDef::max_active_chemist_cap`.
fn default_max_active_chemist_cap() -> usize {
    3
}

/// Matches `station.orders.ron`'s own default — see
/// `RampDef::defeated_gap_scale`. Gentler than `chemist_gap_scale`'s 0.60:
/// a career's first few wins should tighten things, not immediately double
/// the pace of everything else.
fn default_defeated_gap_scale() -> f32 {
    0.90
}

fn default_stretch_after_successes() -> u32 {
    5
}

fn default_stretch_patience_scale() -> f32 {
    1.5
}

fn default_stretch_expiry_standing() -> i32 {
    -1
}

impl Default for RampDef {
    fn default() -> Self {
        RampDef {
            orders_per_tier: 5,
            gap_scale: 0.92,
            gap_floor: 18.0,
            patience_scale: 0.95,
            patience_floor: 80.0,
            max_active_every: 2,
            max_active_cap: 5,
            stretch_base: 0.12,
            stretch_step: 0.01,
            stretch_cap: 0.22,
            stretch_after_successes: default_stretch_after_successes(),
            stretch_patience_scale: default_stretch_patience_scale(),
            stretch_expiry_standing: default_stretch_expiry_standing(),
            forecast_boost: 2.0,
            chemist_gap_scale: default_chemist_gap_scale(),
            max_active_per_chemist: default_max_active_per_chemist(),
            max_active_chemist_cap: default_max_active_chemist_cap(),
            defeated_gap_scale: default_defeated_gap_scale(),
        }
    }
}

/// What cargo brings, and how much of it.
#[derive(Clone, Debug, Deserialize)]
pub struct SupplyDef {
    /// Crew member who brings glassware, looked up in the roster by name.
    pub courier: String,
    /// How much beaker-class glassware the lab should have at the start of a
    /// shift. Supply tops up to this rather than shipping a fixed crate, so the
    /// lab can neither be starved nor flooded.
    pub glassware_target: usize,
    pub crate_max: usize,
    /// One in this many pieces is a large beaker.
    pub large_every: usize,
    /// How far a requisition raises the target for one shift.
    pub requisition_glassware_bonus: usize,
    /// Fixed-composition bundles the courier sells directly from his own
    /// personal standing — see `shift::NpcRequisitionKind::SatoPack`. Empty
    /// is legal, same reasoning as `produce::ProduceConfig.packs`: a
    /// `station.orders.ron` written before this existed still parses, it
    /// just has nothing personal to sell yet.
    #[serde(default)]
    pub personal_packs: Vec<GlasswarePackDef>,
}

impl SupplyDef {
    pub fn pack(&self, id: GlasswarePackId) -> Option<&GlasswarePackDef> {
        self.personal_packs.get(id.0 as usize)
    }
}

impl Default for SupplyDef {
    fn default() -> Self {
        SupplyDef {
            courier: "Miner Sato".to_string(),
            glassware_target: 6,
            crate_max: 4,
            large_every: 3,
            requisition_glassware_bonus: 2,
            personal_packs: Vec::new(),
        }
    }
}

/// One of Miner Sato's own glassware bundles, as authored: a fixed, known
/// composition — what the shop lists is exactly what shows up at the
/// counter, same as Botanist Ivy's packs.
#[derive(Clone, Debug, Deserialize)]
pub struct GlasswarePackDef {
    pub id: String,
    pub label: String,
    pub blurb: String,
    /// In Miner Sato's own standing — see `shift::NpcRequisitionKind`.
    pub cost: i32,
    pub beakers: usize,
    pub large: usize,
}

/// An interned pack handle, the same shape as `produce::ProducePackId`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct GlasswarePackId(pub u32);

/// A shift the station is expecting, and the requests it makes likelier.
#[derive(Clone, Debug, Deserialize)]
pub struct ForecastDef {
    pub id: String,
    pub themes: Vec<String>,
    #[serde(default = "unit_weight")]
    pub weight: f64,
    /// What comes over the radio at the start of prep.
    pub briefing: String,
}

fn unit_weight() -> f64 {
    1.0
}

#[derive(Resource)]
struct PendingStationData {
    crew: Handle<CrewList>,
    orders: Handle<OrderConfig>,
}

/// Crew roster and order settings, once loaded.
#[derive(Resource)]
pub struct StationData {
    pub crew: Vec<CrewDef>,
    pub config: OrderConfig,
}

/// The clock between arrivals.
///
/// Visible to `shift` because it has to be re-armed when service opens: this
/// timer is re-rolled *before* `generate_orders` checks `max_active`, so it is
/// always left holding a partial random remainder when generation stops.
#[derive(Resource)]
pub struct OrderSpawner {
    pub timer: Timer,
}

/// The clock between "asks for it by name" orders — separate from
/// `OrderSpawner`'s lenient/category cadence, and matched to
/// `antagonist::AntagonistSpawner`'s by default: an honest specific ask
/// happens exactly as often as an illicit one, unless deliberately tuned
/// apart. See [`generate_specific_orders`].
#[derive(Resource)]
pub struct SpecificOrderSpawner {
    pub timer: Timer,
}

fn start_loading(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(PendingStationData {
        crew: assets.load("data/station.crew.ron"),
        orders: assets.load("data/station.orders.ron"),
    });
}

fn promote_station_data(
    mut commands: Commands,
    pending: Option<Res<PendingStationData>>,
    mut crew_lists: ResMut<Assets<CrewList>>,
    mut configs: ResMut<Assets<OrderConfig>>,
    shift: Res<Shift>,
) {
    let Some(pending) = pending else {
        return;
    };
    let (Some(crew), Some(config)) = (
        crew_lists.remove(&pending.crew),
        configs.remove(&pending.orders),
    ) else {
        return;
    };

    arm_arrival_clocks(&mut commands, &config, &shift);
    commands.insert_resource(StationData {
        crew: crew.0,
        config,
    });
    commands.remove_resource::<PendingStationData>();
}

/// Sets both arrival clocks running from a standing start.
///
/// Shared by the first session's asset-promotion path and every later
/// session's [`rearm_arrival_clocks`], so the two can never disagree about how
/// long the lab waits for its first customer.
fn arm_arrival_clocks(commands: &mut Commands, config: &OrderConfig, shift: &Shift) {
    commands.insert_resource(OrderSpawner {
        timer: Timer::from_seconds(config.first_order_delay, TimerMode::Once),
    });
    // The first specific ask is armed on the same ramp-scaled cadence its
    // own re-arm uses — `Shift` is already at its tier-0 default here, so
    // (unlike the antagonist thread's very first visit) this needs no
    // separate flat constant. A flat `1` for the chemist count: this runs
    // once at session start, before the shift's own `accepting_orders` sign
    // is even up (it now starts closed — see `Shift::default`), so nothing
    // real can consume this timer's value before `generate_specific_orders`
    // re-arms it for real against however many chemists have actually
    // joined by then.
    let rules = current_rules(config, shift, 1);
    let mut rng = rand::rng();
    let first_specific_gap = rng.random_range(rules.gap_seconds.0..=rules.gap_seconds.1)
        * rng.random_range(config.specific_gap_multiplier.0..=config.specific_gap_multiplier.1);
    commands.insert_resource(SpecificOrderSpawner {
        timer: Timer::from_seconds(first_specific_gap, TimerMode::Once),
    });
}

/// Re-arms the arrival clocks on the way into the lab.
///
/// Every session after the first: `promote_station_data` runs exactly once per
/// process, and both of these are `TimerMode::Once`. A spent `Once` timer never
/// reports `just_finished` again, so without this, quitting to the menu and
/// opening another save gave a lab that nobody ever walked into — no error, no
/// warning, just an empty counter forever.
///
/// No-ops on the first session, where the config has not finished loading yet
/// and `promote_station_data` is what arms them.
fn rearm_arrival_clocks(
    mut commands: Commands,
    station: Option<Res<StationData>>,
    shift: Res<Shift>,
) {
    let Some(station) = station else {
        return;
    };
    arm_arrival_clocks(&mut commands, &station.config, &shift);
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

/// An outstanding request, held on the crew member who made it.
///
/// `patience`/`waited` are plain `f32` rather than a `Timer` — `Timer` is not
/// `Serialize`, which is why `Order` was never replicated. Both chemists in
/// co-op now see the same queue with the same countdowns.
#[derive(Component, Clone, Serialize, Deserialize)]
pub struct Order {
    /// The reagent this request was authored around. For an [`IllicitOrder`]
    /// or a [`specific`](Order::specific) order this is still the one exact
    /// answer. For an ordinary lenient order it is only a *reference* —
    /// grading, the delivery prompt and the order-queue HUD all read the
    /// **category** this reagent belongs to (see [`reference_category`]) and
    /// accept any member of it, never the literal reagent named here. See
    /// [`grade`] and [`Wanted`].
    pub reagent: ReagentId,
    /// Whether this order wants exactly `reagent` rather than any member of
    /// its category — the same trade an [`IllicitOrder`] always makes, now
    /// something an ordinary crew member does too, on its own much rarer
    /// clock (see [`generate_specific_orders`]). Unlike `IllicitOrder` there
    /// is nothing to hide here: the exact reagent is right there in the
    /// prompt and the plea, so a specific order that resolves `Wrong` names
    /// it in the report exactly as it always has, and this field is freely
    /// replicated and queryable everywhere (contrast `IllicitOrder`'s own
    /// doc comment). Illicit requests also set this public recipe requirement;
    /// the private marker controls consequences, not what the chemist is told
    /// to prepare. Ordinary exact requests use the same presentation.
    pub specific: bool,
    /// Minimum purity accepted for the matched reagent. Kept on the order so
    /// replication and late joiners grade against the same authored target.
    #[serde(default)]
    pub minimum_purity: f32,
    pub amount: Units,
    pub plea: String,
    /// Seconds before a waiting crew member gives up and leaves.
    pub patience: f32,
    /// Seconds actually waited so far. Drives [`reputation_delta`] — how much
    /// a resolution is worth, not whether one happens.
    pub waited: f32,
}

/// Explicit destination-use context for an order whose requester is carrying
/// material on behalf of somebody or something else.
///
/// Absence preserves the legacy personal-consumption order. Presence is the
/// only authority for separating requester from beneficiary. A role such as
/// Medical never implies that the doctor at the counter is the patient.
#[derive(Component, Clone, Copy, Debug)]
pub struct OrderUse {
    pub beneficiary: Entity,
    pub source: Option<Entity>,
    pub use_destination: Vec3,
    pub route: Route,
    pub dose: Units,
}

impl OrderUse {
    pub fn medical(
        beneficiary: Entity,
        source: Option<Entity>,
        use_destination: Vec3,
        route: Route,
        dose: Units,
    ) -> Self {
        Self {
            beneficiary,
            source,
            use_destination,
            route,
            dose,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FulfillmentApplicationResult {
    Applied,
    Empty,
    TargetUnavailable,
}

/// Arrival-side outcome, intentionally separate from [`OrderResolved`]. The
/// latter grades Chemistry's preparation; this message reports what happened
/// only after the carrier reached the case and attempted application.
#[derive(Message, Clone, Copy, Debug)]
pub struct FulfillmentApplied {
    pub carrier: Entity,
    pub beneficiary: Entity,
    pub source: Option<Entity>,
    pub container: Entity,
    pub result: FulfillmentApplicationResult,
    pub helpful: bool,
    pub harmful: bool,
    pub illicit: bool,
    pub overdose: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FulfillmentTravel {
    ToUseDestination,
    ReturningUnresolved,
}

/// Authority-only custody and application state after Chemistry handoff.
#[derive(Component, Clone, Copy, Debug)]
pub(crate) struct CarryingFulfillment {
    container: Entity,
    beneficiary: Entity,
    source: Option<Entity>,
    use_destination: Vec3,
    route: Route,
    dose: Units,
    supplier: Option<Entity>,
    believed_label: bool,
    travel: FulfillmentTravel,
}

/// Revokes a linked-delivery custody leg when another controller takes the
/// carrier. The batch remains a physical object at the carrier's last known
/// location, while the owning Medical case observes that its requester is no
/// longer carrying and can publish a replacement request.
pub(crate) fn abandon_carried_fulfillment(
    commands: &mut Commands,
    carrier: Entity,
    fulfillment: &CarryingFulfillment,
    at: Vec3,
) {
    commands
        .entity(fulfillment.container)
        .remove::<HeldBy>()
        .insert(Transform::from_translation(at + Vec3::Y * 0.8));
    commands
        .entity(carrier)
        .remove::<CarryingFulfillment>()
        .remove::<crate::crew::AtCounter>();
}

impl Order {
    /// Seconds left before patience runs out, for the HUD countdown.
    pub fn remaining(&self) -> f32 {
        (self.patience - self.waited).max(0.0)
    }
}

/// An optional request deliberately just beyond the lab's current capability.
///
/// It previews at most one dispenser tier and one unknown reaction ahead,
/// receives extra patience, never stacks with another development request,
/// and does not count as a botched order when ignored. The small authored
/// standing loss preserves some urgency without making it a normal expiry.
#[derive(Component, Clone, Copy, Serialize, Deserialize)]
pub struct DevelopmentOrder {
    pub expiry_standing: i32,
}

/// Marks a crew visit as secretly an antagonist's — someone after an illicit
/// reagent under an ordinary-sounding pretext.
///
/// **Never** add this to `net::register_replication`, and never query
/// `Has<IllicitOrder>` from `src/ui/mod.rs`. Either would break the one
/// guarantee the whole mechanic depends on: nothing on screen ever marks a
/// visit as suspicious. The crew entity is spawned, dressed and routed
/// exactly like a legitimate one — this marker only changes what grading does
/// with the result, in [`complete_delivery`]/[`expire_orders`].
#[derive(Component)]
pub struct IllicitOrder;

/// Marks a crew visit as a `crisis::CrisisOrder` — someone the chemist needs
/// to treat, not just serve, before their `patience`/`waited` clock (reused
/// unchanged as the crisis deadline) runs out.
///
/// Unlike [`IllicitOrder`] this carries no secrecy rule: a crisis is meant to
/// be obvious, so it is fine to query `Has<CrisisOrder>` anywhere, including
/// `src/ui/mod.rs`, if a future pass wants to flag it in the queue. Also
/// unlike `IllicitOrder`, this one *is* replicated (`net::register_replication`)
/// — `crisis::pulse_alert_lighting` reads it on every peer to decide whether
/// to pull the lab's lighting toward red.
#[derive(Component, Serialize, Deserialize)]
pub struct CrisisOrder;

/// Marks a crew visit as a department's countermeasure against the save's
/// main antagonist — see `crate::arc`.
///
/// Authority-only consequence metadata. The conversation and accepted Order
/// expose the department's request without replicating a hidden campaign ID.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterOrder {
    pub campaign: crate::arc::CampaignId,
    pub step: usize,
}

/// A scripted request whose successful fulfilment advances a hostile plan.
/// It is distinct from illicit dealing because it carries no underworld or
/// secrecy behavior; the marker exists so station stability never rewards it.
#[derive(Component)]
pub struct HostileOrder;

/// Which thread, if any, owns an order's consequences.
///
/// Replaces the pair of `illicit`/`crisis` booleans this used to be. That pair
/// grew one flag per thread and could express states that were never
/// meaningful (`illicit && crisis`); with a third thread arriving there would
/// have been three mutually-exclusive bools travelling together. An order
/// belongs to exactly one thread, so it is one value.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum OrderKind {
    #[default]
    Normal,
    Illicit,
    Crisis,
    Counter,
    Hostile,
}

impl OrderKind {
    /// Reads the kind off the marker components an order entity carries.
    ///
    /// The markers are mutually exclusive by construction — each is inserted
    /// by exactly one spawner — so the order of these checks only decides what
    /// a content bug would degrade to, never ordinary behaviour.
    pub fn of(illicit: bool, crisis: bool, counter: bool) -> OrderKind {
        Self::of_with_hostile(illicit, crisis, counter, false)
    }

    pub fn of_with_hostile(illicit: bool, crisis: bool, counter: bool, hostile: bool) -> OrderKind {
        match (illicit, crisis, counter, hostile) {
            (true, _, _, _) => OrderKind::Illicit,
            (_, true, _, _) => OrderKind::Crisis,
            (_, _, true, _) => OrderKind::Counter,
            (_, _, _, true) => OrderKind::Hostile,
            _ => OrderKind::Normal,
        }
    }

    /// Whether the order's exact reagent was already named to the player up
    /// front, so repeating it in a report leaks nothing.
    ///
    /// True for an illicit order (the pretext named the substance) — the
    /// property [`is_named`] and [`wanted_for`] actually care about, which is
    /// why they take this rather than the whole kind.
    pub fn names_its_reagent(self) -> bool {
        matches!(self, OrderKind::Illicit)
    }

    pub fn is_illicit(self) -> bool {
        matches!(self, OrderKind::Illicit)
    }

    pub fn is_crisis(self) -> bool {
        matches!(self, OrderKind::Crisis)
    }
}

/// How a delivery went.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Deserialize)]
pub enum Outcome {
    Success,
    /// Right chemical, not enough of it.
    Short,
    /// Right chemical, but with something else mixed in.
    Impure,
    /// A single dose above the safe threshold.
    Overdose,
    /// The requested chemical was not in there at all.
    Wrong,
    /// Nobody came back with anything in time.
    Expired,
}

impl Outcome {
    /// Reputation at an instant hand-off (`t = 0`) and at patience fully
    /// spent (`t = 1`). See [`reputation_delta`].
    fn reputation_range(self) -> (i32, i32) {
        match self {
            Outcome::Success => (2, 1),
            Outcome::Short | Outcome::Impure => (-1, -3),
            Outcome::Overdose | Outcome::Wrong => (-3, -5),
            // No longer a separate hard-penalty event outside the curve — an
            // `Expired` resolution *is* the curve, evaluated at its worst
            // point, so both endpoints are the same number.
            Outcome::Expired => (-4, -4),
        }
    }

    pub fn is_good(self) -> bool {
        self == Outcome::Success
    }
}

/// How much standing a resolved order moves, scaled by how long it waited.
///
/// `t = 0` is an instant hand-off; `t = 1` is patience fully spent. The
/// principle behind the numbers in [`Outcome::reputation_range`]: the fresh
/// endpoint reproduces the flat constants this replaced exactly, so only the
/// stale endpoint is new — a fast delivery scores the same as it always did,
/// and everything to do with taking your time is the addition.
///
/// `potency` only ever adds — never subtracts — and only on `Success`
/// (`potency.saturating_sub(1)`, so the weakest member of any category
/// reproduces exactly today's flat reward, and a stronger choice earns more).
/// A short, impure, overdosed, wrong or expired delivery earns no quality
/// bonus regardless of what was in the beaker.
pub fn reputation_delta(outcome: Outcome, waited: f32, patience: f32, potency: u32) -> i32 {
    let t = if patience > 0.0 {
        (waited / patience).clamp(0.0, 1.0)
    } else {
        1.0
    };
    let (fresh, stale) = outcome.reputation_range();
    let base = (fresh as f32 + (stale - fresh) as f32 * t).round() as i32;
    if outcome == Outcome::Success {
        base + potency.saturating_sub(1) as i32
    } else {
        base
    }
}

/// Emitted when an order finishes, one way or another.
///
/// M5's radio chatter is built on top of exactly this, which is why the
/// outcome carries the requester's name and role rather than just a score
/// delta.
#[derive(Message)]
#[allow(dead_code)]
pub struct OrderResolved {
    pub name: String,
    pub role: String,
    /// The reagent to name in a report, if any. Always `Some` for an
    /// [`IllicitOrder`] (the pretext already named the substance up front, so
    /// there's nothing to protect) and for a legitimate order that matched
    /// something (`Success`/`Short`/`Impure`/`Overdose`) — there it names
    /// whatever the chemist actually delivered, which may differ from
    /// [`Order::reagent`]. `None` only for a legitimate order that resolved
    /// with nothing matching its category (`Wrong` or `Expired`): naming
    /// [`Order::reagent`] there would leak the one reagent the player was
    /// never told to look for.
    pub reagent: Option<ReagentId>,
    /// The category to name in a report instead, when `reagent` is `None`.
    pub category: Option<Category>,
    pub outcome: Outcome,
    /// Which thread owns this order's consequences.
    ///
    /// Each thread reads only its own variant, off its own cursor into this
    /// message queue: `antagonist` reacts to [`OrderKind::Illicit`], `crisis`
    /// to `Crisis`, `arc` to `Counter`. `Illicit` is never surfaced in the UI
    /// — that is what keeps the "no visible tell" guarantee intact downstream
    /// of grading. The other variants have nothing to hide.
    pub kind: OrderKind,
    /// Chemistry quality used by the station-stability ledger. `None` for an
    /// expiry because no batch was handed over.
    pub quality: Option<DeliveryQuality>,
    /// Optional development work has a deliberately lighter expiry cost.
    pub development: bool,
    /// Owner metadata for counter-track routing. `None` on every other order
    /// and on synthetic legacy test messages.
    pub campaign: Option<crate::arc::CampaignId>,
    pub counter_step: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeliveryQuality {
    pub purity: f32,
    pub potency: u32,
    pub remaining_fraction: f32,
}

/// A station department whose standing rises and falls with how you treat its
/// crew.
///
/// Mirrors the relationship roles `station.crew.ron` actually writes — the same
/// vocabulary `radio::channel_for` already matches, for the same reason:
/// departments are content, not architecture.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Department {
    Medical,
    Security,
    Engineering,
    Cargo,
    Service,
    Botany,
    Bridge,
}

impl Department {
    pub const ALL: [Department; 7] = [
        Department::Medical,
        Department::Security,
        Department::Engineering,
        Department::Cargo,
        Department::Service,
        Department::Botany,
        Department::Bridge,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Department::Medical => "Medical",
            Department::Security => "Security",
            Department::Engineering => "Engineering",
            Department::Cargo => "Cargo",
            Department::Service => "Service",
            Department::Botany => "Botany",
            Department::Bridge => "Bridge",
        }
    }

    /// What this department values, shown on the standing board.
    pub fn blurb(self) -> &'static str {
        match self {
            Department::Medical => {
                "Wants the dose they asked for, on time, and nothing extra in it."
            }
            Department::Security => {
                "Wants contraband kept off the shelves and orders filled honestly."
            }
            Department::Engineering => {
                "Wants burns treated fast and the lab not blowing the breaker."
            }
            Department::Cargo => "Wants glassware back in circulation and crates signed for.",
            Department::Service => "Wants the bar and the kitchen kept stocked, not complaining.",
            Department::Botany => {
                "Wants healthy plots, safe harvests, and useful specimens put to work."
            }
            Department::Bridge => {
                "Wants the station steady, the logs clean, and no surprises to report."
            }
        }
    }

    /// The department a crew role belongs to, or `None` for a name that is
    /// not on the roster — a content bug, not a reason to crash.
    pub fn from_role(role: &str) -> Option<Department> {
        match role {
            "Medical" => Some(Department::Medical),
            "Security" => Some(Department::Security),
            "Engineering" => Some(Department::Engineering),
            "Cargo" => Some(Department::Cargo),
            "Service" => Some(Department::Service),
            "Botany" => Some(Department::Botany),
            "Bridge" => Some(Department::Bridge),
            _ => None,
        }
    }

    /// The named individuals on `station.crew.ron` who belong to this
    /// department — hardcoded here the same way [`Department::from_role`]
    /// hardcodes the five role strings, and for the same reason: content the
    /// roster owns, mirrored rather than derived, so a roster edit that
    /// forgets to update this list fails a test instead of quietly averaging
    /// over the wrong headcount.
    ///
    /// This is what lets [`Shift::standing`] be an *average* of individual
    /// standing without changing its own signature: every existing call site
    /// that reads or writes a department's standing keeps working unchanged,
    /// because the department number was always allowed to be "one number
    /// covering everyone in it" — it just used to be stored that way instead
    /// of derived.
    pub fn members(self) -> &'static [&'static str] {
        match self {
            Department::Medical => &["Dr. Vance", "Nurse Okonkwo"],
            Department::Security => &["Officer Reyes", "Warden Bex"],
            Department::Engineering => &["Tech Lindqvist", "Chief Engineer Morrow"],
            Department::Cargo => &["Miner Sato", "Quartermaster Rhee"],
            Department::Service => &["Chef Dubois", "Steward Amari"],
            Department::Botany => &["Botanist Ivy", "Agronomist Vale"],
            Department::Bridge => &["Helmsman Odera", "Yeoman Sissel"],
        }
    }
}

/// Supplies bought against a department's standing.
///
/// Glassware banks because it still has to be carried in by the courier, so
/// that purchase lands at the next restock pass rather than instantly. The
/// ward/bonus fields below bank for the same underlying reason: each pays off
/// against something that hasn't happened yet — the next expired visit from a
/// department's own minor antagonist, or the next order generated — not the
/// instant it's bought. Every other requisition kind applies the moment it is
/// bought. None of these need `#[serde(default)]`: `Shift.requisition` is
/// never written to `ProgressSave`, so a banked purchase not yet spent when
/// the player quits is lost, same as an unspent glassware bonus always has
/// been.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requisition {
    /// Extra glassware on top of the standing target, banked until the next
    /// restock check consumes it.
    pub glassware: usize,
    /// Bought by `RequisitionKind::SecondOpinion`. Consumed by
    /// `quack::handle_quack_resolution`, which absorbs one instead of dosing
    /// a bystander.
    pub quack_wards: u32,
    /// Bought by `RequisitionKind::LookTheOtherWay`. Consumed by
    /// `security::schedule_raid`, which absorbs one instead of arming a
    /// warning.
    pub raid_wards: u32,
    /// Bought by `RequisitionKind::ChainOfCustody`. Consumed by
    /// `smuggler::handle_smuggler_resolution`, which absorbs one instead of
    /// taking a container.
    pub smuggler_wards: u32,
    /// Bought by `RequisitionKind::SecondInspection`. Consumed by
    /// `saboteur::handle_saboteur_resolution`, which absorbs one instead of
    /// contaminating a container.
    pub saboteur_wards: u32,
    /// Bought by `RequisitionKind::CompedRound`. Consumed by
    /// `orders::generate_orders`, which adds
    /// `shift::COMPED_PATIENCE_BONUS_SECONDS` to the next order's patience.
    pub patience_bonus_orders: u32,
    /// Owed by an illicit deal, not bought. Granted by
    /// `utility_ai::deals::grant` as `FavorKind::QuietAccess` and consumed by
    /// `security::schedule_raid`, which absorbs one instead of arming a raid.
    ///
    /// Deliberately a *separate* counter from `raid_wards` rather than adding
    /// to it: one was paid for at the standing board and the other was earned
    /// by dealing, and collapsing them would let a balance change to the shop
    /// silently reprice the underworld. They are spent by the same site, in a
    /// fixed order — see `Ward::QuietAccess`.
    #[serde(default)]
    pub quiet_access_favors: u32,
    /// `FavorKind::ExpeditedFreight`. Consumed by `freight`, which brings the
    /// next run in early.
    #[serde(default)]
    pub expedited_freight_favors: u32,
    /// `FavorKind::AdvanceWarning`. Consumed by the department-problem
    /// director, which airs a warning before the next incident it permits.
    #[serde(default)]
    pub advance_warning_favors: u32,
}

/// What the career looked like when this shift opened.
///
/// The debrief is the difference between this and now, which is why nothing
/// has to be tallied per-shift as it happens: one snapshot when the lab opens,
/// one subtraction when it closes. It rides on [`Shift`], which already
/// replicates whole through [`ShiftSync`], so both chemists read the same
/// debrief without a second sync to keep in step.
///
/// `research_points` and `recipes_known` belong to [`crate::knowledge::Knowledge`]
/// rather than to `Shift`, and are copied in here at open time on purpose:
/// the alternative is a second snapshot on a second resource that a second
/// message would have to replicate, for two numbers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShiftSnapshot {
    pub succeeded: u32,
    pub botched: u32,
    pub department_standing: HashMap<Department, i32>,
    pub research_points: u32,
    pub recipes_known: usize,
    /// Qualitative station condition at opening. The exact hidden stability
    /// value is never copied into shift state or debrief data.
    #[serde(default)]
    pub stability_band: crate::instability::StabilityBand,
}

#[derive(Resource, Clone, Serialize, Deserialize)]
pub struct Shift {
    pub succeeded: u32,
    pub botched: u32,
    /// One hidden standing per named crew member, keyed by
    /// `station.crew.ron`'s `name` — not per department. [`Shift::standing`]
    /// derives the department number the board shows as the rounded average
    /// of whichever names are `Department::members()` for it.
    ///
    /// Two things move this, deliberately kept separate: a *broad* action
    /// (an order delivered well or badly) goes through [`Shift::adjust`],
    /// which fans the same delta out to every member of the department —
    /// the whole department hears about a delivery even though it was
    /// handed to one specific person. An *individual* action (a purchase
    /// from one NPC's own shop) goes through [`Shift::adjust_npc`] and moves
    /// only that one person.
    pub npc_standing: HashMap<String, i32>,
    pub requisition: Requisition,
    /// The "not accepting requests" sign. Either chemist can flip it; while
    /// it's down, nobody new walks in — but whoever is already at the
    /// counter keeps waiting, and their clock keeps running, exactly as if
    /// the sign were still up. It stops new traffic, not existing orders.
    pub accepting_orders: bool,
    /// Which shift this is, 1-based. Nothing in the simulation branches on it
    /// — it is the number the board and the HUD put on the shift, and the
    /// thing that makes "shift 6" a different sentence from "shift 1".
    pub shift_number: u32,
    /// The career totals this shift opened at. `None` only before
    /// `shift::open_the_shift` has taken the first one, which happens on the
    /// authority's first frame in the lab.
    pub opened_at: Option<ShiftSnapshot>,
    /// The shift has been called: the sign is down, the counter is clear, and
    /// the standing board is showing the debrief.
    ///
    /// Deliberately **not** a state gate. Nothing stops running while this is
    /// set — it only changes what the board draws — because in co-op one
    /// chemist reading the debrief must not stop the other working.
    ///
    /// Persisted with the service sign, so a reload does not quietly reopen a
    /// shift the player has already closed. The board reconstructs the
    /// debrief from the opening snapshot and career totals.
    pub called: bool,
    /// Antagonists thwarted this career, across however many re-arcs
    /// `arc::reroll_campaign` has produced. Bumped by `shift::record_thwarting`
    /// at the exact moment a win is recorded — the same condition that feeds
    /// `arc::ThwartedAntags`' own draw-bias. Read by `current_rules`'s own
    /// internal scaling pass, the third wrapping layer after tier and
    /// chemist-count — every one of its ~12 call sites already takes `&shift`,
    /// so none of them need to change.
    pub defeated_count: u32,
    /// Director inputs mirrored from authority-owned station stability. The
    /// band is qualitative; the hidden exact value never rides `ShiftSync`.
    #[serde(default)]
    pub stability_band: crate::instability::StabilityBand,
    #[serde(default)]
    pub station_age_seconds: u32,
    /// How much standing this closure has already cost, in points per
    /// department. Zero whenever the lab is open — see `shift::impatience`.
    ///
    /// Lives on `Shift` rather than in its own replicated resource because
    /// `ShiftSync` already carries this whole struct to every peer, and the
    /// HUD banner has to draw the same warning for a guest as for the host.
    /// Safe to put here only because it moves at most once a minute: a field
    /// that changed every frame would re-replicate all of `Shift` every frame,
    /// which is the trap `Order::waited` and `AgitationRun` were both caught
    /// in.
    ///
    /// Deliberately **not** persisted — `ProgressSave` names its fields one by
    /// one and does not carry this. Reloading a save that was closed should
    /// not resume mid-grudge, the same reasoning `SecuritySuspicion` is not
    /// saved for.
    #[serde(default)]
    pub closure_pressure: u32,
    /// Set once, the moment `ending::notice_the_ending`/`ending::
    /// watch_for_crew_collapse` raises a real evacuation ending (see
    /// `ending::Ending::evacuated`). Persisted: this is what makes the save
    /// itself unloadable afterward — `saves::SlotSummary::evacuated` reads it
    /// straight off disk to dim the row in the load list, and `menu::
    /// handle_menu_clicks`'s `LoadSave` arm refuses the click as
    /// defense-in-depth even if that dimming were somehow bypassed.
    pub evacuated: bool,
}

impl Default for Shift {
    fn default() -> Self {
        Shift {
            succeeded: 0,
            botched: 0,
            npc_standing: HashMap::new(),
            requisition: Requisition::default(),
            // A brand new career starts closed; a resumed one restores its
            // saved sign state through `ProgressSave`. The player opens up
            // when they — and in co-op, their teammates —
            // are actually ready, rather than crew already walking in on
            // the very first frame.
            accepting_orders: false,
            shift_number: 1,
            opened_at: None,
            called: false,
            defeated_count: 0,
            stability_band: crate::instability::StabilityBand::Stable,
            station_age_seconds: 0,
            closure_pressure: 0,
            evacuated: false,
        }
    }
}

/// The shared department and personal-goodwill scale.
///
/// Keeping the stored per-person values on the same scale is important because
/// department standing is their average. It prevents hidden debt above or below
/// the visible bounds from making several future outcomes appear to do nothing.
pub const STANDING_FLOOR: i32 = -10;
pub const STANDING_CEILING: i32 = 10;

impl Shift {
    /// Broad: a department-wide event (an order's outcome) moves every one
    /// of its members by the same amount.
    pub fn adjust(&mut self, department: Department, delta: i32) {
        for name in department.members() {
            self.adjust_npc(name, delta);
        }
    }

    /// The department number the standing board shows: the rounded average
    /// of its members' individual standing. Departments with exactly one
    /// member (Engineering, Cargo) reproduce that member's own value
    /// exactly, by construction.
    pub fn standing(&self, department: Department) -> i32 {
        let members = department.members();
        if members.is_empty() {
            return 0;
        }
        let sum: i32 = members.iter().map(|name| self.npc_standing(name)).sum();
        ((sum as f32 / members.len() as f32).round() as i32).clamp(STANDING_FLOOR, STANDING_CEILING)
    }

    /// Individual reputation movement on the same bounded scale shown for
    /// departments.
    pub fn adjust_npc(&mut self, name: &str, delta: i32) {
        let entry = self.npc_standing.entry(name.to_string()).or_insert(0);
        *entry = entry
            .saturating_add(delta)
            .clamp(STANDING_FLOOR, STANDING_CEILING);
    }

    /// Voluntarily cashes in department goodwill, stopping at the same lower
    /// bound as every other standing change.
    pub fn spend_goodwill(&mut self, department: Department, cost: i32) {
        for name in department.members() {
            self.spend_npc_goodwill(name, cost);
        }
    }

    /// The personal-favor sibling of [`Shift::spend_goodwill`].
    pub fn spend_npc_goodwill(&mut self, name: &str, cost: i32) {
        debug_assert!(cost >= 0, "goodwill costs must not be negative");
        let entry = self.npc_standing.entry(name.to_string()).or_insert(0);
        *entry = entry
            .saturating_sub(cost.max(0))
            .clamp(STANDING_FLOOR, STANDING_CEILING);
    }

    /// One named crew member's own hidden standing. `0` for anyone not yet
    /// touched, same default `standing`/`adjust` have always used.
    pub fn npc_standing(&self, name: &str) -> i32 {
        self.npc_standing
            .get(name)
            .copied()
            .unwrap_or(0)
            .clamp(STANDING_FLOOR, STANDING_CEILING)
    }

    /// Migrates old saves or network snapshots whose values predate the
    /// bounded standing scale.
    pub fn clamp_standing(&mut self) {
        for standing in self.npc_standing.values_mut() {
            *standing = (*standing).clamp(STANDING_FLOOR, STANDING_CEILING);
        }
        if let Some(opened_at) = self.opened_at.as_mut() {
            for standing in opened_at.department_standing.values_mut() {
                *standing = (*standing).clamp(STANDING_FLOOR, STANDING_CEILING);
            }
        }
    }
}

/// The shift tally, pushed to clients. Both chemists share one score — the
/// lab succeeds or fails together, which is the point of co-op.
#[derive(Message, Serialize, Deserialize, Clone)]
pub struct ShiftSync(Shift);

fn broadcast_shift(shift: Res<Shift>, mut outgoing: MessageWriter<ToClients<ShiftSync>>) {
    if !shift.is_changed() {
        return;
    }
    outgoing.write(ToClients {
        targets: SendTargets::CLIENTS_ONLY,
        message: ShiftSync(shift.clone()),
    });
}

fn apply_shift(mut shift: ResMut<Shift>, mut incoming: MessageReader<ShiftSync>) {
    for sync in incoming.read() {
        *shift = sync.0.clone();
        shift.clamp_standing();
    }
}

/// What a delivery is graded against: the one exact answer an [`IllicitOrder`]
/// always wants, or the category a legitimate order now leniently accepts any
/// member of.
#[derive(Clone, Copy)]
pub enum Wanted {
    Exact(ReagentId),
    Category(Category),
}

/// The category a legitimate request's reference reagent actually requires —
/// its first-listed category, the same "first" convention `product_name` and
/// `reaction_categories` already use in `src/knowledge/mod.rs`. `None` only
/// for a content bug (a reference reagent with no category at all), guarded
/// against by `every_legitimate_requests_reference_reagent_has_a_category`.
pub fn reference_category(db: &ChemDb, reagent: ReagentId) -> Option<Category> {
    db.reagents.get(reagent).categories.first().copied()
}

/// What an order is graded against: exact for an [`IllicitOrder`] or a
/// [`specific`](Order::specific) one, otherwise the category its reference
/// reagent belongs to — falling back to exact only if that reagent somehow
/// names no category at all, a content bug, not a reason to panic.
pub fn wanted_for(order: &Order, kind: OrderKind, db: &ChemDb) -> Wanted {
    if kind.names_its_reagent() || order.specific {
        return Wanted::Exact(order.reagent);
    }
    match reference_category(db, order.reagent) {
        Some(cat) => Wanted::Category(cat),
        None => Wanted::Exact(order.reagent),
    }
}

/// Whether an order's exact reagent is ever named in a report or a
/// container-matching check — true for an [`IllicitOrder`] (whose pretext
/// already named it up front) and for a [`specific`](Order::specific) order
/// (which named it in its own prompt/plea). Both have nothing left to
/// protect; only a lenient order's reference reagent must stay unnamed on an
/// unmatched resolution.
fn is_named(order: &Order, kind: OrderKind) -> bool {
    kind.names_its_reagent() || order.specific
}

/// Whether any reagent in `set` belongs to `cat` — the reachability test for
/// a category request: it counts as reachable the moment *any* member is
/// makeable, not only the specific reagent the request happens to be
/// authored around.
pub fn category_has_member_in(db: &ChemDb, cat: Category, set: &HashSet<ReagentId>) -> bool {
    db.reagents
        .iter()
        .any(|r| r.categories.contains(&cat) && set.contains(&r.id))
}

/// The most of `reagent` a crew member can be handed at once without [`grade`]
/// calling it an [`Outcome::Overdose`].
///
/// Requests are authored per reagent across `assets/data/station.*.ron`, and
/// were written without reference to the pharmacology: most of the medical
/// ones asked for more than their own reagent's overdose threshold — 40u of
/// bicaridine against a threshold of 15. Such an order could not be filled at
/// all. A pill, bottle or syringe holding exactly what was asked for graded
/// `Overdose`; anything smaller graded `Short`; only a beaker escaped, because
/// bulk glassware is exempt from the check — and nothing on screen ever said
/// so, which is what made it read as the game being broken rather than as a
/// dosing mistake. Every spawner runs its authored amount through here, so a
/// request stays freely authorable and the crew member still asks for a dose
/// that can actually be handed over in whatever glassware is to hand.
///
/// Clamped against the *named* reagent's threshold, not the lowest in its
/// category. A lenient order accepts any member, but reaching for a more
/// dangerous sibling — 15u of synaptizine, which overdoses at 5 — is the
/// chemist's own call, and grading it as an overdose is the feedback that
/// teaches the difference.
pub fn deliverable_amount(db: &ChemDb, reagent: ReagentId, asked: Units) -> Units {
    match db.reagents.get(reagent).overdose {
        Some(threshold) => asked.min(threshold),
        None => asked,
    }
}

/// Selects the ordinary or bulk half of an authored quantity list.
///
/// The roll chooses a pool before the amount itself is sampled, so adding a
/// second bulk size cannot accidentally make bulk work twice as common. If a
/// malformed definition has no ordinary option, it remains usable rather than
/// silently preventing that request from spawning; content validation pins the
/// real station data to having a <=30u fallback wherever a bulk option exists.
fn requested_amount_pool(amounts: &[u32], bulk_chance: f64, roll: f64) -> Vec<u32> {
    let ordinary: Vec<u32> = amounts
        .iter()
        .copied()
        .filter(|amount| *amount <= 30)
        .collect();
    let bulk: Vec<u32> = amounts
        .iter()
        .copied()
        .filter(|amount| *amount > 30)
        .collect();
    let wants_bulk = !bulk.is_empty() && roll < bulk_chance.clamp(0.0, 1.0);
    if wants_bulk || ordinary.is_empty() {
        bulk
    } else {
        ordinary
    }
}

fn choose_requested_amount(amounts: &[u32], bulk_chance: f64, rng: &mut impl Rng) -> Option<u32> {
    requested_amount_pool(amounts, bulk_chance, rng.random::<f64>())
        .choose(rng)
        .copied()
}

/// The step size a synthesized `reagent` can actually be produced in, or
/// `None` if it is dispensed directly and so has no such restriction.
///
/// A reaction's `products` entry is a ratio, not an absolute — running it at
/// scale 1.5 is exactly as valid to the resolver as scale 1. But a fractional
/// scale means a fractional, unpackageable dose, and a scale bounded by
/// anything other than an exact integer leaves an unconsumed remainder of
/// some reactant sitting in the same beaker as the product it just made,
/// which is contamination the moment it reaches [`grade`]. Excess upstream
/// ingredients dodge this (the Mixing Chamber's buffer can isolate a single
/// named reagent out of a dirty mix, so a batch can always be brewed with
/// room to spare and only the part that's needed pulled clean) but the
/// reaction's own product number cannot be dodged the same way: every whole,
/// uncontaminated batch of `reagent` is some integer multiple of it.
///
/// Only `assert_askable` reads this today, hence `cfg(test)` — nothing at
/// runtime needs a reagent's batch granularity yet.
#[cfg(test)]
pub fn synthesis_multiple(db: &ChemDb, reagent: ReagentId) -> Option<Units> {
    let reaction = db.reactions.producer_of(reagent)?;
    reaction
        .products
        .iter()
        .find(|(id, _)| *id == reagent)
        .map(|(_, amount)| *amount)
}

/// Decides how a delivery went, and which reagent it was actually judged
/// against.
///
/// Pure and ECS-free (beyond the `ChemDb` lookup) so every branch can be
/// tested directly. Order matters: the checks run worst-first, because a pill
/// that is both overdosed and contaminated should be reported as the
/// overdose. The returned `ReagentId` is `None` only for `Outcome::Wrong` —
/// nothing in the delivery matched what was wanted, so there is nothing to
/// name back.
pub fn grade(
    wanted: Wanted,
    requested_amount: Units,
    delivered: &Solution,
    kind: ContainerKind,
    db: &ChemDb,
) -> (Outcome, Option<ReagentId>) {
    let matched = match wanted {
        Wanted::Exact(id) => {
            let supplied = delivered.volume_of(id);
            supplied.is_positive().then_some((id, supplied))
        }
        // The dominant category member present, by volume — a beaker holding
        // more than one is still Impure below, exactly as a beaker holding
        // the exact reagent plus something unrelated always has been.
        Wanted::Category(cat) => delivered
            .iter()
            .filter(|(id, _)| db.reagents.get(*id).categories.contains(&cat))
            .max_by_key(|(_, amount)| *amount),
    };
    let Some((reagent, supplied)) = matched else {
        return (Outcome::Wrong, None);
    };

    // Only single-dose forms can overdose. A beaker is bulk supply that gets
    // measured out later; a pill is swallowed whole and a syringe goes straight
    // in, which makes it the least forgiving of the three.
    if kind.is_single_dose() {
        if let Some(threshold) = db.reagents.get(reagent).overdose {
            if supplied > threshold {
                return (Outcome::Overdose, Some(reagent));
            }
        }
    }

    if supplied < requested_amount {
        return (Outcome::Short, Some(reagent));
    }
    if delivered.len() > 1 {
        return (Outcome::Impure, Some(reagent));
    }
    (Outcome::Success, Some(reagent))
}

fn enforce_minimum_purity(
    outcome: Outcome,
    matched: Option<ReagentId>,
    minimum_purity: f32,
    delivered: &Solution,
) -> Outcome {
    if outcome == Outcome::Success
        && matched
            .is_some_and(|reagent| delivered.purity_of(reagent) + f32::EPSILON < minimum_purity)
    {
        Outcome::Impure
    } else {
        outcome
    }
}

#[allow(clippy::too_many_arguments)]
fn generate_orders(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    mut spawner: Option<ResMut<OrderSpawner>>,
    knowledge: Option<Res<Knowledge>>,
    mut shift: ResMut<Shift>,
    forecast: Option<Res<CurrentForecast>>,
    mut intake: crate::order_intake::Intake,
    mut residents: AvailableResidents,
    development_orders: Query<(), With<DevelopmentOrder>>,
    chemists: Query<(), With<Chemist>>,
    containers: Query<&Container>,
    produce: Query<&Produce>,
    produce_catalog: Option<Res<ProduceCatalog>>,
) {
    let Some(knowledge) = knowledge else {
        return;
    };
    let (Some(station), Some(spawner)) = (station, spawner.as_mut()) else {
        return;
    };
    // The player's own "not accepting requests" sign. Nobody new walks in
    // while it's down — the direct replacement for the old prep/debrief gate,
    // except it's a choice rather than a clock.
    if !shift.accepting_orders {
        return;
    }

    // Computed fresh from career totals rather than frozen per shift — there
    // is no shift boundary left to freeze a snapshot against. Also fresh
    // every call on how many chemists are actually in the lab right now, so
    // a mid-shift join or leave takes effect on the next re-arm below.
    let rules = current_rules(&station.config, &shift, chemists.iter().count());
    let rules = &rules;

    if !spawner.timer.tick(time.delta()).just_finished() {
        return;
    }

    let waiting = 0usize;
    let mut rng = rand::rng();
    let gap = rng.random_range(rules.gap_seconds.0..=rules.gap_seconds.1);
    spawner.timer = Timer::from_seconds(gap, TimerMode::Once);

    let eligible: Vec<_> = station
        .crew
        .iter()
        .filter(|p| intake.available(&p.name))
        .collect();
    let Some(&crew_def) = eligible.choose(&mut rng) else {
        return;
    };

    // Required work is authored around something the lab can produce now.
    // Development work is the bounded look ahead below, never an accidental
    // side effect of two medicines sharing a broad treatment category.
    let inventory = physical_reagent_inventory(&containers, &produce, produce_catalog.as_deref());
    let makeable = knowledge.available_reagents_with_inventory(&db, inventory.iter().copied());
    let development = knowledge.development_reagents_with_inventory(&db, inventory.iter().copied());

    // Keep pool membership tied to the authored reference reagent. Grading is
    // still lenient, but a late "best treatment" plea should not appear merely
    // because the player knows a weaker medicine in the same category.
    let request_reagent = |request: &RequestDef| db.reagents.id_of(&request.reagent);
    let request_category =
        |request: &RequestDef| request_reagent(request).and_then(|id| reference_category(&db, id));
    let in_reach: Vec<&RequestDef> = station
        .config
        .requests
        .iter()
        .filter(|request| request.minimum_successes <= shift.succeeded)
        .filter(|request| request.minimum_recipes_known <= knowledge.known_count())
        .filter(|request| request_reagent(request).is_some_and(|id| makeable.contains(&id)))
        .collect();
    let just_beyond: Vec<&RequestDef> = station
        .config
        .requests
        .iter()
        .filter(|request| request.minimum_successes <= shift.succeeded)
        .filter(|request| request.minimum_recipes_known <= knowledge.known_count())
        .filter(|request| {
            request_reagent(request).is_some_and(|id| development.contains(&id))
                && request_category(request)
                    .is_some_and(|cat| !category_has_member_in(&db, cat, &makeable))
        })
        .collect();

    let offer_development = shift.succeeded >= station.config.ramp.stretch_after_successes
        && development_orders.is_empty()
        && !just_beyond.is_empty()
        && rng.random_bool(rules.stretch_chance);
    let (pool, is_development) = if offer_development {
        (&just_beyond, true)
    } else {
        (&in_reach, false)
    };

    // The forecast leans on whichever pool was chosen; it never chooses for it,
    // so a briefing for burns still only asks for things the chemist can
    // plausibly make.
    let no_themes: &[String] = &[];
    let themes = forecast.as_ref().map(|f| f.themes()).unwrap_or(no_themes);
    let Some(request) = weighted_pick(pool, themes, rules.forecast_boost, rng.random::<f64>())
    else {
        return;
    };
    // A request naming a reagent that is not in the chemistry data is a
    // content bug, but it should not take the shift down with it.
    let Some(reagent) = db.reagents.id_of(&request.reagent) else {
        warn!("order requests unknown reagent '{}'", request.reagent);
        return;
    };
    let Some(asked) = choose_requested_amount(
        &request.amounts,
        station.config.bulk_amount_chance,
        &mut rng,
    ) else {
        return;
    };
    let amount = deliverable_amount(&db, reagent, Units::whole(asked as i32));

    let mut patience = rng.random_range(rules.patience_seconds.0..=rules.patience_seconds.1);
    if is_development {
        patience *= station.config.ramp.stretch_patience_scale.max(1.0);
    }
    patience *= request.patience_scale.max(0.25);
    // A `CompedRound` requisition buys exactly the next order some extra
    // patience — only this ordinary stream, never `generate_specific_orders`
    // or any minor-thread visit, each of which has its own authored identity
    // that "comped by the kitchen" doesn't fit.
    let Some(context) = intake.admit(
        crate::order_intake::RequestSource::Ordinary,
        &crew_def.name,
        &mut spawner.timer,
        false,
    ) else {
        return;
    };
    let lane_offset = waiting as f32 * 0.95;
    let Some(crew) =
        recall_or_spawn_crew_member(&mut commands, &mut residents, crew_def, lane_offset)
    else {
        intake.cancel_admission(&crew_def.name);
        return;
    };
    if !is_development && shift.requisition.patience_bonus_orders > 0 {
        shift.requisition.patience_bonus_orders -= 1;
        patience += crate::shift::COMPED_PATIENCE_BONUS_SECONDS;
    }

    // An ordinary order spawned here always describes what it needs, not
    // the exact chemical — naming one outright is `generate_specific_orders`'
    // job now, on its own separate clock. Falls back to the reagent's own
    // name only if it somehow has no category (a content bug guarded by
    // `every_legitimate_requests_reference_reagent_has_a_category`).
    let want_label = reference_category(&db, reagent)
        .map(|cat| cat.want_phrase().to_string())
        .unwrap_or_else(|| db.reagents.get(reagent).name.clone());
    commands.entity(crew).insert((
        crate::order_intake::PendingOrder::new(
            Order {
                reagent,
                specific: request.exact,
                minimum_purity: request.minimum_purity.clamp(0.0, 1.0),
                amount,
                plea: if request.exact {
                    request.specific_plea.clone()
                } else {
                    request.plea.clone()
                },
                patience,
                waited: 0.0,
            },
            context,
        ),
        crate::interaction::Interactable::new("Waiting to speak"),
    ));
    if is_development {
        commands.entity(crew).insert(DevelopmentOrder {
            expiry_standing: station.config.ramp.stretch_expiry_standing.min(0),
        });
    }

    info!(
        "{} ({}) wants {} {}",
        crew_def.name, crew_def.role, amount, want_label
    );
}

/// Same shape as `generate_orders`, but rarer and exact rather than lenient:
/// on its own clock (see [`SpecificOrderSpawner`]) it spawns a crew member
/// who names one chemical outright — the honest counterpart to an
/// [`IllicitOrder`]'s exactness, so a specific ask is not by itself a tell.
///
/// Two deliberate differences from the lenient path:
/// - Only drawn from `in_reach` (the reference reagent itself already
///   makeable), never the optional development pool. Naming one exact reagent
///   has no fallback, so offering one the chemist cannot yet make would be a
///   guaranteed, unfair failure.
/// - Uses [`RequestDef::specific_plea`] and shows the reagent's real name in
///   both the plea and the prompt, exactly as an antagonist's pretext does.
#[allow(clippy::too_many_arguments)]
fn generate_specific_orders(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    mut spawner: Option<ResMut<SpecificOrderSpawner>>,
    knowledge: Option<Res<Knowledge>>,
    shift: Res<Shift>,
    forecast: Option<Res<CurrentForecast>>,
    mut intake: crate::order_intake::Intake,
    mut residents: AvailableResidents,
    chemists: Query<(), With<Chemist>>,
    containers: Query<&Container>,
    produce: Query<&Produce>,
    produce_catalog: Option<Res<ProduceCatalog>>,
) {
    let Some(knowledge) = knowledge else {
        return;
    };
    let (Some(station), Some(spawner)) = (station, spawner.as_mut()) else {
        return;
    };
    if !shift.accepting_orders {
        return;
    }

    let rules = current_rules(&station.config, &shift, chemists.iter().count());
    let rules = &rules;

    if !spawner.timer.tick(time.delta()).just_finished() {
        return;
    }

    let waiting = 0usize;
    let mut rng = rand::rng();
    // Scaled off the *current* legitimate gap, exactly like
    // `antagonist::generate_antagonist_orders`'s own re-arm — the two rates
    // match by construction, not by coincidence.
    let legit_gap = rng.random_range(rules.gap_seconds.0..=rules.gap_seconds.1);
    let multiplier = rng.random_range(
        station.config.specific_gap_multiplier.0..=station.config.specific_gap_multiplier.1,
    );
    spawner.timer = Timer::from_seconds(legit_gap * multiplier, TimerMode::Once);

    // Respects the same concurrent-order cap as an ordinary lenient order —
    // this is still fundamentally an ordinary order, just a picky one, not a
    // second antagonist-style thread that ignores the queue's capacity.

    let eligible: Vec<_> = station
        .crew
        .iter()
        .filter(|p| intake.available(&p.name))
        .collect();
    let Some(&crew_def) = eligible.choose(&mut rng) else {
        return;
    };

    let inventory = physical_reagent_inventory(&containers, &produce, produce_catalog.as_deref());
    let makeable = knowledge.available_reagents_with_inventory(&db, inventory);
    let in_reach: Vec<&RequestDef> = station
        .config
        .requests
        .iter()
        .filter(|request| request.minimum_successes <= shift.succeeded)
        .filter(|request| request.minimum_recipes_known <= knowledge.known_count())
        .filter(|request| {
            db.reagents
                .id_of(&request.reagent)
                .is_some_and(|id| makeable.contains(&id))
        })
        .collect();
    if in_reach.is_empty() {
        return;
    }

    let no_themes: &[String] = &[];
    let themes = forecast.as_ref().map(|f| f.themes()).unwrap_or(no_themes);
    let Some(request) = weighted_pick(&in_reach, themes, rules.forecast_boost, rng.random::<f64>())
    else {
        return;
    };
    let Some(reagent) = db.reagents.id_of(&request.reagent) else {
        warn!("order requests unknown reagent '{}'", request.reagent);
        return;
    };
    let Some(asked) = choose_requested_amount(
        &request.amounts,
        station.config.bulk_amount_chance,
        &mut rng,
    ) else {
        return;
    };
    let amount = deliverable_amount(&db, reagent, Units::whole(asked as i32));

    let patience = rng.random_range(rules.patience_seconds.0..=rules.patience_seconds.1)
        * request.patience_scale.max(0.25);
    let Some(context) = intake.admit(
        crate::order_intake::RequestSource::Specific,
        &crew_def.name,
        &mut spawner.timer,
        false,
    ) else {
        return;
    };
    let lane_offset = waiting as f32 * 0.95;
    let Some(crew) =
        recall_or_spawn_crew_member(&mut commands, &mut residents, crew_def, lane_offset)
    else {
        intake.cancel_admission(&crew_def.name);
        return;
    };

    let reagent_name = db.reagents.get(reagent).name.clone();
    commands.entity(crew).insert((
        crate::order_intake::PendingOrder::new(
            Order {
                reagent,
                specific: true,
                minimum_purity: request.minimum_purity.clamp(0.0, 1.0),
                amount,
                plea: request.specific_plea.clone(),
                patience,
                waited: 0.0,
            },
            context,
        ),
        crate::interaction::Interactable::new("Waiting to speak"),
    ));

    info!(
        "{} ({}) specifically wants {} {}",
        crew_def.name, crew_def.role, amount, reagent_name
    );
}

pub(crate) fn physical_reagent_inventory(
    containers: &Query<&Container>,
    produce: &Query<&Produce>,
    catalog: Option<&ProduceCatalog>,
) -> HashSet<ReagentId> {
    let mut inventory = HashSet::new();
    for container in containers {
        inventory.extend(container.solution.iter().map(|(reagent, _)| reagent));
    }
    if let Some(catalog) = catalog {
        for item in produce {
            inventory.extend(
                catalog
                    .get(item.0)
                    .yields
                    .iter()
                    .map(|(reagent, _)| *reagent),
            );
        }
    }
    inventory
}

#[allow(clippy::type_complexity)]
pub(crate) fn expire_orders(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    mut shift: ResMut<Shift>,
    mut resolved: MessageWriter<OrderResolved>,
    mut orders: Query<(
        Entity,
        &mut Order,
        &CrewMember,
        &mut CrewRoute,
        Has<IllicitOrder>,
        Has<CrisisOrder>,
        Option<&CounterOrder>,
        Has<HostileOrder>,
        Option<&DevelopmentOrder>,
        Has<crate::order_intake::AcceptedOrder>,
        Has<crate::security_case::OrderHold>,
        Option<&Body>,
        Option<&Bloodstream>,
        Has<UtilityAgent>,
        Option<&ControlOwner>,
    )>,
) {
    // Deliberately *not* gated on `accepting_orders`. The sign stops new
    // people walking in; it is not a pause button, so whoever is already at
    // the counter keeps waiting — and keeps costing you — exactly as if it
    // were still up.
    let dt = time.delta_secs();

    for (
        entity,
        mut order,
        crew,
        mut route,
        illicit,
        crisis,
        counter,
        hostile,
        development,
        accepted,
        security_hold,
        body,
        blood,
        utility_agent,
        owner,
    ) in &mut orders
    {
        if !order_visit_can_act(body, blood, utility_agent, owner) {
            continue;
        }
        let kind = OrderKind::of_with_hostile(illicit, crisis, counter.is_some(), hostile);
        // Patience only runs down once they have actually arrived, so a slow
        // walk in never counts against the player.
        if security_hold || (!accepted && route.phase != CrewPhase::Waiting) {
            continue;
        }
        // The queue only ever shows `remaining()` truncated to whole seconds
        // (`update_order_queue`'s `MM:SS` readout), so replicate the tick
        // only when that displayed second actually moves — not on every one
        // of the many frames in between, which would otherwise resend the
        // whole `Order` (plea text included) for every crew member currently
        // waiting at the counter.
        let displayed_before = order.remaining() as u32;
        let quiet = order.bypass_change_detection();
        quiet.waited += dt;
        if quiet.waited < quiet.patience {
            if order.remaining() as u32 != displayed_before {
                order.set_changed();
            }
            continue;
        }
        order.set_changed();

        // Nobody ever delivered anything, so there is nothing to match — an
        // illicit or specific order still names its (already-known-to-the-
        // player) reagent, but a plain lenient order falls back to naming
        // only the category, never the reference reagent it never revealed.
        let (reagent, category) = if is_named(&order, kind) {
            (Some(order.reagent), None)
        } else {
            (None, reference_category(&db, order.reagent))
        };
        resolved.write(OrderResolved {
            name: crew.name.clone(),
            role: crew.role.clone(),
            reagent,
            category,
            outcome: Outcome::Expired,
            kind,
            quality: None,
            development: development.is_some(),
            campaign: counter.map(|counter| counter.campaign),
            counter_step: counter.map(|counter| counter.step),
        });
        if development.is_none() {
            shift.botched += 1;
        }
        // An abandoned illicit order is not a chaos-causing success, so it
        // always falls through to the ordinary department penalty — the same
        // shape "declining isn't specially punished" takes on the delivery
        // side, just arrived at by giving up rather than choosing to.
        // Nothing was ever delivered, so there is nothing to grade for
        // quality — `0` is inert anyway, since `reputation_delta` only ever
        // applies a potency bonus on `Success`.
        if let Some(development) = development {
            // Optional work costs a token amount of goodwill when ignored,
            // but never receives the ordinary four-point expiry penalty.
            if let Some(department) = Department::from_role(&crew.role) {
                shift.adjust(department, development.expiry_standing);
            }
        } else {
            adjust_for_role(
                &mut shift,
                &crew.role,
                Outcome::Expired,
                order.waited,
                order.patience,
                0,
                None,
            );
        }

        commands
            .entity(entity)
            .remove::<Order>()
            .remove::<crate::security_case::OrderHold>()
            .remove::<crate::order_intake::AcceptedOrder>()
            .remove::<DevelopmentOrder>()
            .remove::<OrderUse>();
        route.leave();
    }
}

fn order_visit_can_act(
    body: Option<&Body>,
    blood: Option<&Bloodstream>,
    utility_agent: bool,
    owner: Option<&ControlOwner>,
) -> bool {
    !body.is_some_and(|body| body.0.collapsed)
        && !blood.is_some_and(|blood| blood.0.incapacitated())
        && (!utility_agent || owner == Some(&ControlOwner::OrderVisit))
}

/// Applies a resolution's reputation delta to the department the crew
/// member's role names, warning rather than panicking if it names none — a
/// content bug in `station.crew.ron` should not take the shift down with it.
///
/// `instability` is `None` at the one call site (an expired order) that can
/// never carry `Outcome::Success` in the first place — [`misdelivered`]
/// would be a guaranteed no-op there regardless, so there is nothing to
/// thread through for it.
fn adjust_for_role(
    shift: &mut Shift,
    role: &str,
    outcome: Outcome,
    waited: f32,
    patience: f32,
    potency: u32,
    instability: Option<&crate::instability::Instability>,
) {
    let Some(department) = Department::from_role(role) else {
        if role == "Training" {
            return;
        }
        warn!("order resolved for unrecognised department role '{role}'");
        return;
    };
    shift.adjust(
        department,
        reputation_delta(
            misdelivered(outcome, instability),
            waited,
            patience,
            potency,
        ),
    );
}

/// A fraying crew occasionally does not notice a delivery went right — for
/// *standing* purposes only. Never changes what `OrderResolved` reports or
/// what the player's own delivered/botched totals count: those stay honest
/// even when this fires, which is what makes it a mood the player can grow
/// to suspect rather than a lie the game tells about their own record.
///
/// Gated on `instability::InstabilityTier::Fraying`+ — fully inert at
/// `Calm`, so ordinary early-career play is untouched. A flat probability
/// rather than one that keeps climbing with the raw meter: the *tier* is the
/// dial here, not the level underneath it.
fn misdelivered(
    outcome: Outcome,
    instability: Option<&crate::instability::Instability>,
) -> Outcome {
    if outcome != Outcome::Success {
        return outcome;
    }
    let Some(instability) = instability else {
        return outcome;
    };
    if instability.band < crate::instability::StabilityBand::Unstable {
        return outcome;
    }
    if rand::rng().random_bool(MISDELIVERY_CHANCE) {
        Outcome::Short
    } else {
        outcome
    }
}

/// First-pass constant, not tuned against real play.
const MISDELIVERY_CHANCE: f64 = 0.15;

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
pub(crate) fn handle_delivery(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut requests: MessageReader<FromClient<InteractRequested>>,
    mut shift: ResMut<Shift>,
    mut resolved: MessageWriter<OrderResolved>,
    mut exposures: MessageWriter<ChemicalExposure>,
    mut crew: Query<(
        &CrewMember,
        &Order,
        &mut CrewRoute,
        Option<&OrderUse>,
        Has<IllicitOrder>,
        Has<CrisisOrder>,
        Option<&CounterOrder>,
        Has<HostileOrder>,
        Has<DevelopmentOrder>,
        Has<UtilityAgent>,
        Option<&ControlOwner>,
    )>,
    mut bodies: Query<(&mut Body, &mut Bloodstream)>,
    containers: Query<(Entity, &Container, &HeldBy)>,
    chemists: Query<(Entity, &Chemist)>,
    mut knowledge: ResMut<Knowledge>,
    instability: Option<Res<crate::instability::Instability>>,
    labels: Query<&crate::labels::Label>,
    estranged: Option<Res<crate::estrangement::Estranged>>,
    mut suspicion: Option<ResMut<crate::antagonist::SecuritySuspicion>>,
) {
    for request in requests.read() {
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Ok((
            member,
            order,
            mut route,
            use_plan,
            illicit,
            crisis,
            counter,
            hostile,
            development,
            utility_agent,
            owner,
        )) = crew.get_mut(request.target)
        else {
            continue;
        };
        let body = bodies
            .get_mut(request.target)
            .ok()
            .map(|(b, blood)| (b.into_inner(), blood.into_inner()));
        if !order_visit_can_act(
            body.as_ref().map(|(body, _)| &**body),
            body.as_ref().map(|(_, blood)| &**blood),
            utility_agent,
            owner,
        ) {
            continue;
        }
        let Some((container_entity, container, _)) =
            containers.iter().find(|(_, _, holder)| holder.0 == player)
        else {
            continue;
        };

        let kind = OrderKind::of_with_hostile(illicit, crisis, counter.is_some(), hostile);
        let label = labels.get(container_entity).ok();
        let is_estranged = estranged
            .as_ref()
            .is_some_and(|estranged| estranged.0.contains(&member.name));

        // Face to face, they get to look at the bottle. Whether they bother
        // is what the relationship buys — see `trust_in_a_label`.
        if caught_lying(
            &db,
            order,
            kind,
            &container.solution,
            label,
            shift.npc_standing(&member.name),
            is_estranged,
            rand::rng().random::<f64>(),
        ) {
            // Refused, not graded. They keep their order and their patience
            // clock, and the chemist keeps the bottle — being caught costs
            // you the attempt and the relationship, not the glassware.
            shift.adjust_npc(&member.name, CAUGHT_LYING_PENALTY);
            if let Some(suspicion) = suspicion.as_mut() {
                crate::antagonist::nudge_suspicion(suspicion, CAUGHT_LYING_SUSPICION);
            }
            continue;
        }

        let believed = believed_a_lie(&db, order, kind, &container.solution, label)
            .then(|| claimed_reagent(&db, label))
            .flatten();

        complete_delivery(
            &mut commands,
            &db,
            &mut shift,
            &mut resolved,
            &mut exposures,
            &mut knowledge,
            instability.as_deref(),
            Handover {
                crew: request.target,
                actor: Some(player),
                member,
                order,
                route: &mut route,
                container_entity,
                container,
                kind,
                counter: counter.copied(),
                development,
                body,
                believed,
                use_plan: use_plan.copied(),
            },
        );
    }
}

/// What being caught out costs, with the person who caught you.
///
/// Individual rather than department-wide: they watched you try it. Deep
/// enough to matter — three of these walks someone from neutral into
/// `estrangement`'s range, and an estranged crew member never believes a
/// label again, which is the real punishment.
const CAUGHT_LYING_PENALTY: i32 = -5;
/// And they mention it. Comparable to `antagonist::SUSPICION_PER_DELIVERY`:
/// being caught passing something off is about as loud as one illicit sale.
const CAUGHT_LYING_SUSPICION: i32 = 5;

/// One crew member, one order, one container being handed across.
struct Handover<'a> {
    crew: Entity,
    actor: Option<Entity>,
    member: &'a CrewMember,
    order: &'a Order,
    route: &'a mut CrewRoute,
    container_entity: Entity,
    container: &'a Container,
    /// Which thread owns this order — see [`OrderResolved::kind`].
    kind: OrderKind,
    counter: Option<CounterOrder>,
    development: bool,
    /// Explicit linked destination use. Absence alone means the accepting NPC
    /// is the consumer.
    use_plan: Option<OrderUse>,
    /// The recipient's body, so `complete_delivery` can route what was
    /// actually handed over into them — every crew member has had one since
    /// M12. `Option` because this struct already follows the "the caller
    /// fetched it, not this function" shape for everything else; a missing
    /// body just means the dose is never felt rather than a panic.
    body: Option<(&'a mut Body, &'a mut Bloodstream)>,
    /// What the label claimed this was, when the recipient took it on that
    /// claim alone — `None` for every honest handover, and for a plain
    /// wrong-beaker mistake with nothing written on it.
    ///
    /// The caller has already decided they believed it (see
    /// [`caught_lying`]); this is what they believed.
    believed: Option<ReagentId>,
}

/// Whether a delivered batch is station supplies rather than something to take.
///
/// Majority-by-volume rather than "contains any", so a medicine carrying a
/// trace of cleaner is still a medicine and still gets taken — the player's
/// sloppy synthesis stays their problem. It flips only when the batch is
/// mostly, actually, a cleaning chemical.
fn is_station_chemical(container: &Container, db: &ChemDb) -> bool {
    let total = container.solution.total_volume();
    if !total.is_positive() {
        return false;
    }
    let station: Units = container
        .solution
        .iter()
        .filter(|(id, _)| db.0.reagents.get(*id).is_for_the_station_not_a_body())
        .map(|(_, volume)| volume)
        .fold(Units::ZERO, |sum, volume| sum + volume);
    // Doubled rather than halved: `Units` is a fixed-point integer with no
    // division, and this keeps the comparison exact at odd volumes.
    station * 2 > total
}

/// Grades a handover and closes the order out.
///
/// Shared by both delivery routes on purpose. The counter and the window have
/// to agree exactly on what counts as a good delivery — two copies of this
/// would drift, and the player would learn that where they stood mattered.
fn complete_delivery(
    commands: &mut Commands,
    db: &ChemDb,
    shift: &mut Shift,
    resolved: &mut MessageWriter<OrderResolved>,
    exposures: &mut MessageWriter<ChemicalExposure>,
    knowledge: &mut Knowledge,
    instability: Option<&crate::instability::Instability>,
    handover: Handover,
) -> Outcome {
    let Handover {
        crew,
        actor,
        member,
        order,
        route,
        container_entity,
        container,
        kind,
        body,
        believed,
        counter,
        development,
        use_plan,
    } = handover;

    let (mut outcome, mut matched) = grade(
        wanted_for(order, kind, db),
        order.amount,
        &container.solution,
        container.kind,
        db,
    );
    outcome = enforce_minimum_purity(outcome, matched, order.minimum_purity, &container.solution);

    if member.name == "Practice Customer" {
        let delivered = crate::tutorial::DeliveryEvidence {
            request: 0,
            container: container_entity,
            kind: container.kind,
            actual: container.solution.clone(),
            outcome,
        };
        commands.queue(move |world: &mut World| {
            if world.get_resource::<crate::session::SessionKind>()
                != Some(&crate::session::SessionKind::Training)
            {
                return;
            }
            let Some(context) = world.get::<crate::order_intake::RequestContext>(crew) else {
                return;
            };
            let request = context.id;
            if let Some(mut messages) =
                world.get_resource_mut::<Messages<crate::tutorial::DeliveryEvidence>>()
            {
                messages.write(crate::tutorial::DeliveryEvidence {
                    request,
                    ..delivered
                });
            }
        });
    }

    // They read the bottle, believed it, and are grading what they think they
    // were given. Nothing about the *contents* has changed — the real solution
    // still goes into them below, still reacts, still grades them later — but
    // at this counter, in this moment, the delivery is the one on the label.
    //
    // This is the whole reward side of lying: standing, research and a clean
    // report, for a batch that was never made. The bill arrives when the
    // chemistry does, through `authorized` further down.
    if let Some(claimed) = believed {
        outcome = Outcome::Success;
        matched = Some(claimed);
    }

    // An illicit or specific order always names its (already-known-to-the-
    // player) reagent. A plain lenient order names whatever was actually
    // delivered when something matched, and falls back to naming only the
    // category — never `order.reagent` itself — when nothing did.
    let (reported_reagent, category) = if is_named(order, kind) {
        (Some(order.reagent), None)
    } else {
        match matched {
            Some(id) => (Some(id), None),
            None => (None, reference_category(db, order.reagent)),
        }
    };
    // Whatever was actually handed over, not what the request was authored
    // around: a lenient order accepts any member of its category, and both
    // currencies below pay for the one the chemist chose to make.
    let potency = matched.map(|id| db.reagents.get(id).potency).unwrap_or(0);
    let delivered_purity = matched
        .map(|id| container.solution.purity_of(id))
        .unwrap_or(1.0);
    let remaining_fraction = if order.patience > 0.0 {
        (order.remaining() / order.patience).clamp(0.0, 1.0)
    } else {
        0.0
    };
    resolved.write(OrderResolved {
        name: member.name.clone(),
        role: member.role.clone(),
        reagent: reported_reagent,
        category,
        outcome,
        kind,
        quality: Some(DeliveryQuality {
            purity: delivered_purity,
            potency,
            remaining_fraction,
        }),
        development,
        campaign: counter.map(|counter| counter.campaign),
        counter_step: counter.map(|counter| counter.step),
    });

    if outcome.is_good() {
        shift.succeeded += 1;
        knowledge.award_research(research_for_delivery_at_purity(potency, delivered_purity));
    } else {
        shift.botched += 1;
    }
    // A successful illicit delivery is graded but never banked against the
    // pretext department — that department was never the real requester, and
    // crediting or blaming it would be a narrative contradiction. Its
    // consequences (underworld standing, the delayed chaos report, Security
    // suspicion) live entirely in `antagonist`, reacting to the same
    // `OrderResolved` this just wrote. Every other case — including a
    // declined illicit order — falls through to the ordinary path below,
    // unchanged.
    if !(kind.is_illicit() && outcome.is_good()) {
        adjust_for_role(
            shift,
            &member.role,
            outcome,
            order.waited,
            order.patience,
            potency,
            instability,
        );
    }

    info!(
        "{} took {} — {:?}",
        member.name,
        container.kind.label(),
        outcome
    );

    if let Some(use_plan) = use_plan {
        // A linked order transfers custody. It does not turn possession into
        // self-application and it does not destroy the batch at the counter.
        // The exact container and remaining solution now travel with the
        // requester, who is the initial carrier until a later handoff says
        // otherwise.
        commands
            .entity(container_entity)
            .remove::<InSlot>()
            .remove::<InSlotB>()
            .remove::<InSlotC>()
            .remove::<Stored>()
            .remove::<InventorySlot>()
            .insert(HeldBy(crew));
        let travel = if use_plan.use_destination.is_finite() {
            *route = CrewRoute::to(use_plan.use_destination);
            FulfillmentTravel::ToUseDestination
        } else {
            route.leave();
            FulfillmentTravel::ReturningUnresolved
        };
        commands.entity(crew).insert((
            CarryingFulfillment {
                container: container_entity,
                beneficiary: use_plan.beneficiary,
                source: use_plan.source,
                use_destination: use_plan.use_destination,
                route: use_plan.route,
                dose: use_plan.dose,
                supplier: actor,
                believed_label: believed.is_some(),
                travel,
            },
            crate::utility_ai::NpcActivity::Traveling,
        ));
    } else {
        // A personal-consumption order keeps the original behavior. They
        // actually drink what was handed over, including a wrong batch. This
        // branch exists only because the order has no explicit linked use.
        //
        // Except when what was handed over is not for a body at all. A bottle
        // of space cleaner is for a spill — its own reference entry says "Do
        // not drink it" — and swallowing it was this branch assuming every
        // delivery ends in somebody's stomach. Filling the wrong *medicine* is
        // still the player's mistake to make and still gets drunk; a cleaner
        // is not a wrong medicine, it is not medicine.
        if let Some((recipient_body, recipient_blood)) =
            body.filter(|_| !is_station_chemical(container, db))
        {
            let mut dose = container.solution.clone();
            if dose.total_volume().is_positive() {
                let snapshot = dose.clone();
                let assessment = assess_exposure(
                    &snapshot,
                    Route::Ingested,
                    recipient_body,
                    recipient_blood,
                    db,
                );
                recipient_blood
                    .0
                    .receive(&mut dose, Route::Ingested, &mut recipient_body.0, db);
                exposures.write(ChemicalExposure {
                    actor,
                    target: crew,
                    route: Route::Ingested,
                    source: ExposureSource::Direct,
                    solution: snapshot,
                    authorized: believed.is_none()
                        || !(assessment.harmful || assessment.illicit || assessment.overdose),
                    helpful: assessment.helpful,
                    harmful: assessment.harmful,
                    illicit: assessment.illicit,
                    overdose: assessment.overdose,
                });
            }
        }

        // Legacy personal orders still consume the one-way customer
        // glassware lifecycle. Linked orders above retain the real entity.
        commands.entity(container_entity).despawn();
        route.leave();
    }

    commands
        .entity(crew)
        .remove::<Order>()
        .remove::<OrderUse>()
        .remove::<crate::security_case::OrderHold>()
        .remove::<crate::order_intake::AcceptedOrder>()
        .remove::<DevelopmentOrder>()
        .remove::<Interactable>()
        // Comes off with the order it marks. Both `crisis::schedule_crisis`
        // and `crisis::pulse_alert_lighting` read `Has<CrisisOrder>` as "is a
        // crisis live", so leaving it on a cured victim kept the lab red-lit —
        // and blocked the next crisis from arming — for the whole walk to the
        // door. `IllicitOrder` is deliberately left in place: it is never read
        // after resolution, and the antagonist's own tests count it.
        .remove::<CrisisOrder>()
        // Same reasoning as `CrisisOrder`: `arc::generate_counter_orders`
        // reads `Has<CounterOrder>` as "a counter-track request is live", so
        // it has to come off when the request closes.
        .remove::<CounterOrder>()
        .remove::<crate::crew::AtCounter>();
    outcome
}

/// Carries a linked batch through the station and applies it only after the
/// requester reaches the beneficiary. The first leg honors the incident's
/// recorded use destination. If the beneficiary has moved, the carrier then
/// follows the current body rather than dosing themselves or applying at
/// empty floor.
#[allow(clippy::type_complexity)]
fn apply_carried_fulfillments(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut exposures: MessageWriter<ChemicalExposure>,
    mut applied: MessageWriter<FulfillmentApplied>,
    mut carriers: Query<
        (
            Entity,
            &Transform,
            Option<&mut CrewRoute>,
            &mut CarryingFulfillment,
            Option<&mut crate::utility_ai::NpcActivity>,
            Option<&Body>,
            Option<&Bloodstream>,
            Has<UtilityAgent>,
            Option<&ControlOwner>,
        ),
        Without<Container>,
    >,
    mut targets: Query<
        (&Transform, &mut Body, &mut Bloodstream),
        (Without<Container>, Without<CarryingFulfillment>),
    >,
    mut containers: Query<(&mut Container, Option<&mut Transform>), Without<CarryingFulfillment>>,
) {
    const APPLICATION_REACH: f32 = 1.35;

    for (
        carrier,
        carrier_transform,
        route,
        mut fulfillment,
        activity,
        body,
        blood,
        utility_agent,
        owner,
    ) in &mut carriers
    {
        let Ok((mut container, container_transform)) = containers.get_mut(fulfillment.container)
        else {
            if route
                .as_deref()
                .is_none_or(|route| route.phase == CrewPhase::Waiting)
            {
                applied.write(FulfillmentApplied {
                    carrier,
                    beneficiary: fulfillment.beneficiary,
                    source: fulfillment.source,
                    container: fulfillment.container,
                    result: FulfillmentApplicationResult::Empty,
                    helpful: false,
                    harmful: false,
                    illicit: false,
                    overdose: false,
                });
                commands
                    .entity(carrier)
                    .remove::<CarryingFulfillment>()
                    .remove::<crate::crew::AtCounter>();
                if let Some(mut route) = route {
                    route.leave();
                }
            }
            continue;
        };

        let carried_at = carrier_transform.translation + Vec3::Y * 0.8;
        if let Some(mut transform) = container_transform {
            transform.translation = carried_at;
        } else {
            commands
                .entity(fulfillment.container)
                .insert(Transform::from_translation(carried_at));
        }

        // A utility carrier can apply this batch only while the exact order
        // visit still owns them. Medical admission and every other handoff
        // revoke that authority. Return the physical container to the world
        // and report an unresolved arrival instead of letting a recovered
        // UtilityAction actor execute stale order intent.
        if utility_agent && owner != Some(&ControlOwner::OrderVisit) {
            applied.write(FulfillmentApplied {
                carrier,
                beneficiary: fulfillment.beneficiary,
                source: fulfillment.source,
                container: fulfillment.container,
                result: FulfillmentApplicationResult::TargetUnavailable,
                helpful: false,
                harmful: false,
                illicit: false,
                overdose: false,
            });
            abandon_carried_fulfillment(
                &mut commands,
                carrier,
                &fulfillment,
                carrier_transform.translation,
            );
            continue;
        }
        if body.is_some_and(|body| body.0.collapsed)
            || blood.is_some_and(|blood| blood.0.incapacitated())
        {
            continue;
        }
        let Some(mut route) = route else {
            applied.write(FulfillmentApplied {
                carrier,
                beneficiary: fulfillment.beneficiary,
                source: fulfillment.source,
                container: fulfillment.container,
                result: FulfillmentApplicationResult::TargetUnavailable,
                helpful: false,
                harmful: false,
                illicit: false,
                overdose: false,
            });
            abandon_carried_fulfillment(
                &mut commands,
                carrier,
                &fulfillment,
                carrier_transform.translation,
            );
            continue;
        };

        if route.phase != CrewPhase::Waiting {
            continue;
        }

        if fulfillment.travel == FulfillmentTravel::ReturningUnresolved {
            applied.write(FulfillmentApplied {
                carrier,
                beneficiary: fulfillment.beneficiary,
                source: fulfillment.source,
                container: fulfillment.container,
                result: FulfillmentApplicationResult::TargetUnavailable,
                helpful: false,
                harmful: false,
                illicit: false,
                overdose: false,
            });
            commands
                .entity(carrier)
                .remove::<CarryingFulfillment>()
                .remove::<crate::crew::AtCounter>();
            continue;
        }

        let Ok((target_transform, mut target_body, mut target_blood)) =
            targets.get_mut(fulfillment.beneficiary)
        else {
            fulfillment.travel = FulfillmentTravel::ReturningUnresolved;
            route.leave();
            commands.entity(carrier).remove::<crate::crew::AtCounter>();
            if let Some(mut activity) = activity {
                *activity = crate::utility_ai::NpcActivity::Traveling;
            }
            continue;
        };

        let separation = carrier_transform
            .translation
            .distance(target_transform.translation);
        if separation > APPLICATION_REACH {
            fulfillment.use_destination = target_transform.translation;
            *route = CrewRoute::to(target_transform.translation);
            commands.entity(carrier).remove::<crate::crew::AtCounter>();
            if let Some(mut activity) = activity {
                *activity = crate::utility_ai::NpcActivity::Traveling;
            }
            continue;
        }

        if let Some(mut activity) = activity {
            *activity = crate::utility_ai::NpcActivity::Treating;
        }
        let (mut dose, _) = container.mutate(&db, |solution| solution.split(fulfillment.dose));
        let (result, helpful, harmful, illicit, overdose) = if dose.total_volume().is_positive() {
            let snapshot = dose.clone();
            let assessment = assess_exposure(
                &snapshot,
                fulfillment.route,
                &target_body,
                &target_blood,
                &db,
            );
            target_blood
                .0
                .receive(&mut dose, fulfillment.route, &mut target_body.0, &db);
            exposures.write(ChemicalExposure {
                actor: fulfillment.supplier,
                target: fulfillment.beneficiary,
                route: fulfillment.route,
                source: ExposureSource::Direct,
                solution: snapshot,
                authorized: !fulfillment.believed_label
                    || !(assessment.harmful || assessment.illicit || assessment.overdose),
                helpful: assessment.helpful,
                harmful: assessment.harmful,
                illicit: assessment.illicit,
                overdose: assessment.overdose,
            });
            (
                FulfillmentApplicationResult::Applied,
                assessment.helpful,
                assessment.harmful,
                assessment.illicit,
                assessment.overdose,
            )
        } else {
            (
                FulfillmentApplicationResult::Empty,
                false,
                false,
                false,
                false,
            )
        };

        applied.write(FulfillmentApplied {
            carrier,
            beneficiary: fulfillment.beneficiary,
            source: fulfillment.source,
            container: fulfillment.container,
            result,
            helpful,
            harmful,
            illicit,
            overdose,
        });
        commands
            .entity(carrier)
            .remove::<CarryingFulfillment>()
            .remove::<crate::crew::AtCounter>();
        route.leave();
    }
}

/// Whether `contents` holds anything that would satisfy `order` — the exact
/// reagent for an illicit or specific order, any member of its category for
/// a plain lenient one. Whether it holds *enough*, whether it is clean, and
/// whether the dose is safe are [`grade`]'s business — this only decides
/// whether the window should offer the beaker to this order at all.
fn container_matches(contents: &Solution, order: &Order, kind: OrderKind, db: &ChemDb) -> bool {
    if is_named(order, kind) {
        return contents.volume_of(order.reagent).is_positive();
    }
    match reference_category(db, order.reagent) {
        Some(cat) => contents.iter().any(|(id, amount)| {
            amount.is_positive() && db.reagents.get(id).categories.contains(&cat)
        }),
        None => contents.volume_of(order.reagent).is_positive(),
    }
}

// ---------------------------------------------------------------------------
// Believing the bottle
// ---------------------------------------------------------------------------
//
// A label is a *claim about which chemical this is* — see `crate::labels`.
// Writing "Bicaridine" on a bottle of krokodil is the whole trick, and these
// three pure functions are the entirety of whether it works.
//
// Note what is deliberately absent: the honest path never reaches any of this.
// If the contents genuinely satisfy the order — including a double-life drug
// like meth answering a stimulant request — nobody is being lied to, nothing
// is rolled, and the delivery grades exactly as it always has.

/// Standing at or above which someone simply takes your word.
///
/// The same number `speech`'s `Knows::LikesYou` uses, because it is the same
/// judgement: a person pleased with you does not audit you.
const TRUSTS_YOU: i32 = 6;

/// What a label claims the container *is*, if it names a real chemical.
///
/// Matched against the reagent's display name rather than its key, because
/// the player is writing what a crew member would expect to read on a bottle.
/// Free text that names nothing — "Painkiller", "do not drink", an empty
/// label — claims nothing and therefore deceives nobody: a crew member who
/// asked for trauma treatment and is handed a bottle marked "Painkiller" is
/// not being told it *is* the medicine, only that somebody thinks so.
pub fn claimed_reagent(db: &ChemDb, label: Option<&crate::labels::Label>) -> Option<ReagentId> {
    let claim = label?.0.trim();
    if claim.is_empty() {
        return None;
    }
    db.reagents
        .iter()
        .find(|reagent| reagent.name.eq_ignore_ascii_case(claim))
        .and_then(|reagent| db.reagents.id_of(&reagent.key))
}

/// Whether a chemical of that name would have satisfied this order.
pub fn claim_satisfies(db: &ChemDb, claimed: ReagentId, wanted: Wanted) -> bool {
    match wanted {
        Wanted::Exact(id) => claimed == id,
        Wanted::Category(cat) => db.reagents.get(claimed).categories.contains(&cat),
    }
}

/// How readily this crew member takes a label at its word, `0.0` to `1.0`.
///
/// The relationship *is* the mechanic. Someone pleased with you does not read
/// the bottle; someone you have burned reads it properly; someone estranged
/// has stopped accepting anything from you on trust at all. Which means
/// burning a relationship costs you the ability to lie to that person — and
/// `Shift::npc_standing`, a meter the game has always tracked per named crew
/// member and never shown, becomes something the player can feel.
///
/// Pure, and takes no RNG, so the whole ladder is testable without a world.
pub fn trust_in_a_label(standing: i32, estranged: bool) -> f64 {
    // Bounded by `estrangement`'s own thresholds rather than fresh numbers, so
    // "cool towards you" means one thing across both systems.
    if estranged || standing <= crate::estrangement::RECONCILED_AT {
        return 0.0;
    }
    if standing >= TRUSTS_YOU {
        return 1.0;
    }
    let span = (TRUSTS_YOU - crate::estrangement::RECONCILED_AT) as f64;
    ((standing - crate::estrangement::RECONCILED_AT) as f64 / span).clamp(0.0, 1.0)
}

/// Whether this handover is a lie that its recipient sees through.
///
/// `roll` is the caller's own `0.0..1.0`, kept as a parameter so every band of
/// [`trust_in_a_label`] can be tested without reaching for a seeded RNG.
///
/// `false` for an honest delivery, and `false` for a plain mistake — handing
/// over the wrong beaker with nothing written on it is not deception, it is a
/// `Wrong` delivery, and it grades exactly as it always did.
#[allow(clippy::too_many_arguments)]
pub fn caught_lying(
    db: &ChemDb,
    order: &Order,
    kind: OrderKind,
    contents: &Solution,
    label: Option<&crate::labels::Label>,
    standing: i32,
    estranged: bool,
    roll: f64,
) -> bool {
    if container_matches(contents, order, kind, db) {
        return false;
    }
    let Some(claimed) = claimed_reagent(db, label) else {
        return false;
    };
    if !claim_satisfies(db, claimed, wanted_for(order, kind, db)) {
        return false;
    }
    roll >= trust_in_a_label(standing, estranged)
}

/// Whether a handover only went through because the label lied for it.
///
/// The counterpart of [`caught_lying`]: same conditions, opposite side of the
/// roll. What it gates is *consequence*, not grading — see the `authorized`
/// computation in [`complete_delivery`].
pub fn believed_a_lie(
    db: &ChemDb,
    order: &Order,
    kind: OrderKind,
    contents: &Solution,
    label: Option<&crate::labels::Label>,
) -> bool {
    !container_matches(contents, order, kind, db)
        && claimed_reagent(db, label)
            .is_some_and(|claimed| claim_satisfies(db, claimed, wanted_for(order, kind, db)))
}

/// Which order a container in the window should go to, if any.
///
/// Pulled out of the system so the matching rule can be tested directly.
///
/// Ties go to whoever is closest to giving up, matching the order queue's own
/// sort. A beaker that could satisfy two people should go to the one about to
/// walk out.
fn window_recipient<'a>(
    contents: &Solution,
    waiting: impl Iterator<Item = (Entity, &'a Order, &'a CrewRoute, OrderKind, bool)>,
    reserved: &HashSet<Entity>,
    lane: DeliveryLane,
    db: &ChemDb,
) -> Option<Entity> {
    waiting
        .filter(|(entity, ..)| !reserved.contains(entity))
        .filter(|(_, _, route, _, _)| route.delivery_lane == lane)
        .filter(|(_, _, route, _, accepted)| {
            route.phase != CrewPhase::Leaving && (route.phase == CrewPhase::Waiting || *accepted)
        })
        .filter(|(_, order, _, kind, _)| container_matches(contents, order, *kind, db))
        .min_by(|a, b| a.1.remaining().total_cmp(&b.1.remaining()))
        .map(|(entity, ..)| entity)
}

/// Hands over whatever is sitting in the delivery window.
///
/// The window is a tray rather than a button: a container left in it goes to
/// the first crew member at the counter who asked for something it holds. That
/// means a batch can be finished and parked before its requester has even
/// walked in, which is what makes the window a post one chemist can work while
/// the other mixes.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn handle_window_delivery(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut shift: ResMut<Shift>,
    mut resolved: MessageWriter<OrderResolved>,
    mut exposures: MessageWriter<ChemicalExposure>,
    mut knowledge: ResMut<Knowledge>,
    windows: Query<(Entity, &Machine, Option<&DeliveryLane>)>,
    slotted: Query<(Entity, &InSlot)>,
    slotted_b: Query<(Entity, &InSlotB)>,
    slotted_c: Query<(Entity, &InSlotC)>,
    containers: Query<&Container>,
    mut crew: Query<(
        Entity,
        &CrewMember,
        &Order,
        &mut CrewRoute,
        Option<&OrderUse>,
        Has<IllicitOrder>,
        Has<CrisisOrder>,
        Option<&CounterOrder>,
        Has<HostileOrder>,
        Has<DevelopmentOrder>,
        Has<crate::order_intake::AcceptedOrder>,
        Has<UtilityAgent>,
        Option<&ControlOwner>,
    )>,
    mut bodies: Query<(Entity, &mut Body, &mut Bloodstream)>,
    instability: Option<Res<crate::instability::Instability>>,
) {
    // `complete_delivery` removes the order through deferred commands. Keep
    // an immediate reservation too, otherwise two tray containers that both
    // match the same urgent request can be handed to it in this one system
    // pass before the removal becomes visible.
    let mut reserved_recipients = HashSet::new();
    let unavailable_bodies: HashSet<_> = bodies
        .iter_mut()
        .filter_map(|(entity, body, blood)| {
            (body.0.collapsed || blood.0.incapacitated()).then_some(entity)
        })
        .collect();
    for (window, machine, lane) in &windows {
        if machine.kind != MachineKind::DeliveryWindow {
            continue;
        }
        let lane = lane.copied().unwrap_or(DeliveryLane::Public);
        let tray = [
            slotted_container(window, &slotted),
            slotted_container_b(window, &slotted_b),
            slotted_container_c(window, &slotted_c),
        ];
        for container_entity in tray.into_iter().flatten() {
            let Ok(container) = containers.get(container_entity) else {
                continue;
            };
            // A batch still running is refused: this tray hands over the instant
            // *anything* in the beaker matches, and a rated recipe passes through
            // a stage where it is half reactant and half product — so without
            // this, parking a beaker here and walking away would deliver a
            // half-made batch and grade it `Impure`, which reads as the window
            // having stolen it early.
            if chem_sim::is_reacting(&container.solution, &db.reactions) {
                continue;
            }

            let candidates = crew.iter().filter_map(
                |(
                    entity,
                    _,
                    order,
                    route,
                    _,
                    illicit,
                    crisis,
                    counter,
                    hostile,
                    _,
                    accepted,
                    utility_agent,
                    owner,
                )| {
                    (!unavailable_bodies.contains(&entity)
                        && (!utility_agent || owner == Some(&ControlOwner::OrderVisit)))
                    .then_some((
                        entity,
                        order,
                        route,
                        OrderKind::of_with_hostile(illicit, crisis, counter.is_some(), hostile),
                        accepted,
                    ))
                },
            );
            let Some(recipient) = window_recipient(
                &container.solution,
                candidates,
                &reserved_recipients,
                lane,
                &db,
            ) else {
                continue;
            };

            let Ok((
                crew_entity,
                member,
                order,
                mut route,
                use_plan,
                illicit,
                crisis,
                counter,
                hostile,
                development,
                _,
                utility_agent,
                owner,
            )) = crew.get_mut(recipient)
            else {
                continue;
            };
            reserved_recipients.insert(recipient);
            let body = bodies
                .get_mut(crew_entity)
                .ok()
                .map(|(_, b, blood)| (b.into_inner(), blood.into_inner()));
            if !order_visit_can_act(
                body.as_ref().map(|(body, _)| &**body),
                body.as_ref().map(|(_, blood)| &**blood),
                utility_agent,
                owner,
            ) {
                continue;
            }
            complete_delivery(
                &mut commands,
                &db,
                &mut shift,
                &mut resolved,
                &mut exposures,
                &mut knowledge,
                instability.as_deref(),
                Handover {
                    crew: crew_entity,
                    actor: None,
                    member,
                    order,
                    route: &mut route,
                    container_entity,
                    container,
                    kind: OrderKind::of_with_hostile(illicit, crisis, counter.is_some(), hostile),
                    counter: counter.copied(),
                    development,
                    body,
                    // The delivery window is a drop box, not a conversation.
                    // Nobody is standing there to read the bottle.
                    believed: None,
                    use_plan: use_plan.copied(),
                },
            );
        }
    }
}

/// Gap between things set down at the vial drop.
///
/// A bottle is 0.07 m across, so this is generous — the point is that two
/// vials read as two objects from across the room, not that they merely fail
/// to intersect.
const VIAL_SPACING: f32 = 0.24;

/// How many lanes there are before the layout gives up and stacks.
///
/// The lobby's east wall is at x = 7.5 and the drop starts at
/// `COUNTER_SPOT.x` (4.0), so twelve lanes at [`VIAL_SPACING`] stay well
/// inside the room. Lanes run **east**, away from the glassware crate, which
/// `restock::CRATE_X_OFFSET` puts to the west.
const VIAL_LANES: usize = 12;

/// How close in z something has to be to count as sitting at the drop rather
/// than somewhere else in the lab entirely.
const VIAL_LANE_Z_TOLERANCE: f32 = 0.35;

/// The next free spot along the counter for something set down at the vial
/// drop, given where loose containers already are.
///
/// Pure so the layout can be tested without spawning anything. Vials used to
/// be dropped at exactly `COUNTER_SPOT.x` every time, so a second one landed
/// *inside* the first — invisible, and indistinguishable from the game having
/// forgotten to give it to you. Same bug the glassware crate already avoids;
/// this is the same fix.
///
/// Falls back to lane zero when every lane is taken: overlapping is bad, but
/// spawning through the lobby's east wall is worse.
pub fn free_vial_lane(occupied: &[Vec3]) -> Vec3 {
    free_vial_lane_at(
        occupied,
        DeliveryStations::default().station(DeliveryLane::Public),
    )
}

fn free_vial_lane_at(occupied: &[Vec3], station: DeliveryStation) -> Vec3 {
    let (_, height) = ContainerKind::Bottle.dimensions();
    let base = station.drop_position(crate::lab::COUNTER_TOP + height * 0.5);
    let across = -(station.transform.rotation * Vec3::X);
    let toward_crew = -(station.transform.rotation * Vec3::Z);
    let at_the_drop: Vec<f32> = occupied
        .iter()
        .filter(|spot| ((*spot - base).dot(toward_crew)).abs() <= VIAL_LANE_Z_TOLERANCE)
        .map(|spot| (*spot - base).dot(across))
        .collect();

    for lane in 0..VIAL_LANES {
        let offset = lane as f32 * VIAL_SPACING;
        let clear = at_the_drop
            .iter()
            .all(|taken| (taken - offset).abs() > VIAL_SPACING * 0.5);
        if clear {
            return base + across * offset;
        }
    }
    base
}

/// Grateful crew occasionally leave a sample of something else they use.
///
/// Run through the analyzer it yields a recipe, which is the route into
/// anything the player cannot yet stumble onto by mixing. Tying it to clean
/// deliveries means the game opens up in response to doing the job well.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn leave_sample_vials(
    mut commands: Commands,
    db: Res<ChemDb>,
    knowledge: Res<Knowledge>,
    mut resolved: MessageReader<OrderResolved>,
    mut radio: ResMut<RadioLog>,
    stations: Option<Res<DeliveryStations>>,
    loose: Query<
        &Transform,
        (
            With<Container>,
            Without<HeldBy>,
            Without<InSlot>,
            Without<Stored>,
        ),
    >,
) {
    let mut rng = rand::rng();
    // Spawns are deferred, so anything placed earlier in this same call is not
    // in the query yet. Two deliveries can resolve in one frame — the counter
    // and the window both run here — so claimed lanes are carried by hand.
    let mut occupied: Vec<Vec3> = loose.iter().map(|spot| spot.translation).collect();

    for report in resolved.read() {
        if report.outcome != Outcome::Success || !rng.random_bool(SAMPLE_VIAL_CHANCE) {
            continue;
        }

        // Unknown, and safe to hand somebody unasked.
        //
        // The second half matters now that the book holds toxins: a grateful
        // crew member leaving you thirty units of sulphuric acid "in case it's
        // useful" is a funny idea and a miserable one to be on the end of,
        // especially since the obvious thing to do with a mystery vial is
        // analyse it — and the second most obvious is drink it.
        let unknown: Vec<_> = db
            .reactions
            .iter()
            .filter(|reaction| !knowledge.is_known(reaction.id))
            .filter(|reaction| {
                reaction
                    .product_ids()
                    .all(|product| !db.reagents.get(product).is_harmful())
            })
            .collect();
        let Some(recipe) = unknown.choose(&mut rng) else {
            continue;
        };
        let Some(&(product, _)) = recipe.products.first() else {
            continue;
        };

        let station = stations
            .as_deref()
            .cloned()
            .unwrap_or_default()
            .station(DeliveryLane::Public);
        let spot = free_vial_lane_at(&occupied, station);
        occupied.push(spot);
        let vial = spawn_container(&mut commands, ContainerKind::Bottle, spot);
        let amount = ContainerKind::Bottle.capacity();
        let ph = db.reagents.get(product).ph;
        commands.queue(move |world: &mut World| {
            if let Some(mut container) = world.get_mut::<Container>(vial) {
                let _ = container.solution.add_profiled(product, amount, 1.0, ph);
            }
        });

        let name = db.reagents.get(product).name.clone();
        radio.push(
            RadioEntry::new(
                channel_for(&report.role),
                format!("Left you a sample of {name} on the counter. Might be useful."),
            )
            .speaker(&report.name)
            .positive(),
        );
        info!("{} left a sample of {}", report.name, name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chem_sim::ChemData;

    fn data() -> ChemData {
        ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap()
    }

    fn solution_of(data: &ChemData, contents: &[(&str, i32)]) -> Solution {
        let mut solution = Solution::new(Units::whole(200));
        for (key, amount) in contents {
            let _ = solution.add(data.reagent(key), Units::whole(*amount));
        }
        solution
    }

    // -- misdelivery ----------------------------------------------------

    #[test]
    fn misdelivery_is_inert_at_calm_and_with_no_meter_at_all() {
        let calm = crate::instability::Instability::default();
        for _ in 0..200 {
            assert_eq!(
                misdelivered(Outcome::Success, Some(&calm)),
                Outcome::Success
            );
            assert_eq!(misdelivered(Outcome::Success, None), Outcome::Success);
        }
    }

    #[test]
    fn misdelivery_never_touches_an_outcome_that_was_never_a_success() {
        let breaking = crate::instability::Instability {
            value: 0.0,
            band: crate::instability::StabilityBand::Critical,
            ..default()
        };
        for outcome in [
            Outcome::Short,
            Outcome::Impure,
            Outcome::Overdose,
            Outcome::Wrong,
            Outcome::Expired,
        ] {
            for _ in 0..50 {
                assert_eq!(
                    misdelivered(outcome, Some(&breaking)),
                    outcome,
                    "misdelivery only ever downgrades a real Success"
                );
            }
        }
    }

    #[test]
    fn misdelivery_is_a_real_probabilistic_roll_once_the_crew_is_fraying() {
        let fraying = crate::instability::Instability {
            value: 50.0,
            band: crate::instability::StabilityBand::Unstable,
            ..default()
        };
        let downgraded = (0..500)
            .filter(|_| misdelivered(Outcome::Success, Some(&fraying)) != Outcome::Success)
            .count();
        assert!(downgraded > 0, "some deliveries should misfire at Fraying");
        assert!(
            downgraded < 500,
            "not every delivery should misfire — this is a mood, not a wall"
        );
    }

    // -- delivery window ----------------------------------------------------

    /// Just enough world to run the window: no renderer, no crew walking.
    fn window_app() -> App {
        let data = data();
        let mut app = App::new();
        app.insert_resource(Knowledge::new(&data))
            .insert_resource(ChemDb(data))
            .init_resource::<Shift>()
            .add_message::<OrderResolved>()
            .add_message::<ChemicalExposure>()
            .add_systems(Update, handle_window_delivery);
        app
    }

    fn linked_fulfillment_app() -> App {
        let data = data();
        let areas = crate::lab::WalkableAreas::from_floor_plan();
        let mut app = App::new();
        app.insert_resource(Knowledge::new(&data))
            .insert_resource(ChemDb(data))
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS))
            .init_resource::<Shift>()
            .init_resource::<Time>()
            .init_resource::<crate::crew::Departments>()
            .init_resource::<crate::lab::DeliveryStations>()
            .add_message::<OrderResolved>()
            .add_message::<ChemicalExposure>()
            .add_message::<FulfillmentApplied>()
            .add_systems(
                Update,
                (
                    handle_window_delivery,
                    ApplyDeferred,
                    crate::crew::walk_route,
                    ApplyDeferred,
                    apply_carried_fulfillments,
                    ApplyDeferred,
                )
                    .chain(),
            );
        app
    }

    #[test]
    fn linked_treatment_is_carried_to_the_patient_instead_of_dosing_the_doctor() {
        let mut app = linked_fulfillment_app();
        let kelotane = reagent_id(&app, "kelotane");
        let incident = crate::lab::ROOMS[crate::lab::REACTION_BAY].center();
        let patient = app
            .world_mut()
            .spawn((
                Transform::from_xyz(incident.x, crate::crew::BODY_OFFSET, incident.z),
                Body::default(),
                Bloodstream::default(),
            ))
            .id();
        app.world_mut()
            .get_mut::<Body>(patient)
            .unwrap()
            .0
            .damage
            .burn = Units::whole(10);
        let source = app.world_mut().spawn_empty().id();
        let doctor = waiting_crew_in_lane(
            &mut app,
            "Dr. Vance",
            "kelotane",
            10,
            120.0,
            true,
            DeliveryLane::Medical,
        );
        app.world_mut().entity_mut(doctor).insert((
            Transform::from_xyz(COUNTER_SPOT.x, crate::crew::BODY_OFFSET, COUNTER_SPOT.z),
            Body::default(),
            Bloodstream::default(),
            crate::crew::AtCounter(DeliveryLane::Medical),
            crate::crew::ReturnsToDuty,
            OrderUse::medical(
                patient,
                Some(source),
                Vec3::new(incident.x, crate::crew::BODY_OFFSET, incident.z),
                Route::Patched,
                Units::whole(5),
            ),
        ));
        let (_, beaker) = window_with_lane(&mut app, &[("kelotane", 30)], DeliveryLane::Medical);

        app.update();

        assert!(app.world().get_entity(beaker).is_ok());
        assert_eq!(
            app.world().get::<HeldBy>(beaker).map(|held| held.0),
            Some(doctor)
        );
        assert!(app.world().get::<CarryingFulfillment>(doctor).is_some());
        assert!(
            app.world()
                .get::<Bloodstream>(doctor)
                .unwrap()
                .0
                .blood
                .volume_of(kelotane)
                .is_zero(),
            "accepting a treatment order must not make the doctor consume it",
        );
        assert!(
            app.world()
                .get::<Bloodstream>(patient)
                .unwrap()
                .0
                .blood
                .volume_of(kelotane)
                .is_zero(),
            "the patient must not receive treatment before the carrier arrives",
        );

        for _ in 0..800 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.05));
            app.update();
            if app.world().get::<CarryingFulfillment>(doctor).is_none() {
                break;
            }
        }

        assert!(
            app.world().get::<CarryingFulfillment>(doctor).is_none(),
            "the doctor never reached and applied the linked treatment",
        );
        assert_eq!(
            app.world()
                .get::<Bloodstream>(doctor)
                .unwrap()
                .0
                .blood
                .volume_of(kelotane),
            Units::ZERO,
        );
        assert_eq!(
            app.world()
                .get::<Bloodstream>(patient)
                .unwrap()
                .0
                .blood
                .volume_of(kelotane),
            Units::whole(5),
            "only the bounded arrival-side dose belongs in the patient's bloodstream",
        );
        assert_eq!(
            app.world()
                .get::<Container>(beaker)
                .unwrap()
                .solution
                .volume_of(kelotane),
            Units::whole(25),
            "the carried container must retain the unapplied remainder",
        );
        let reports: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<FulfillmentApplied>>()
            .drain()
            .collect();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].carrier, doctor);
        assert_eq!(reports[0].beneficiary, patient);
        assert_eq!(reports[0].source, Some(source));
        assert_eq!(reports[0].container, beaker);
        assert_eq!(reports[0].result, FulfillmentApplicationResult::Applied);
        assert!(reports[0].helpful);
        assert!(!reports[0].harmful);
        assert!(!reports[0].illicit);
        assert!(!reports[0].overdose);
    }

    #[test]
    fn a_missing_linked_patient_never_falls_back_to_dosing_the_carrier() {
        let mut app = linked_fulfillment_app();
        let kelotane = reagent_id(&app, "kelotane");
        let missing_patient = app.world_mut().spawn_empty().id();
        let doctor = waiting_crew_in_lane(
            &mut app,
            "Dr. Vance",
            "kelotane",
            10,
            120.0,
            true,
            DeliveryLane::Medical,
        );
        app.world_mut().entity_mut(doctor).insert((
            Transform::from_xyz(COUNTER_SPOT.x, crate::crew::BODY_OFFSET, COUNTER_SPOT.z),
            Body::default(),
            Bloodstream::default(),
            crate::crew::AtCounter(DeliveryLane::Medical),
            crate::crew::ReturnsToDuty,
            OrderUse::medical(
                missing_patient,
                None,
                Vec3::new(COUNTER_SPOT.x, crate::crew::BODY_OFFSET, COUNTER_SPOT.z),
                Route::Patched,
                Units::whole(5),
            ),
        ));
        let (_, beaker) = window_with_lane(&mut app, &[("kelotane", 30)], DeliveryLane::Medical);

        for _ in 0..800 {
            app.world_mut()
                .resource_mut::<Time>()
                .advance_by(std::time::Duration::from_secs_f32(0.05));
            app.update();
            if app.world().get::<CarryingFulfillment>(doctor).is_none()
                && app.world().get::<Order>(doctor).is_none()
            {
                break;
            }
        }

        assert_eq!(
            app.world()
                .get::<Bloodstream>(doctor)
                .unwrap()
                .0
                .blood
                .volume_of(kelotane),
            Units::ZERO,
        );
        assert_eq!(
            app.world()
                .get::<Container>(beaker)
                .unwrap()
                .solution
                .volume_of(kelotane),
            Units::whole(30),
            "an unresolved delivery must return with the physical batch intact",
        );
        let reports: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<FulfillmentApplied>>()
            .drain()
            .collect();
        assert_eq!(reports.len(), 1);
        assert_eq!(
            reports[0].result,
            FulfillmentApplicationResult::TargetUnavailable
        );
    }

    #[test]
    fn a_down_or_wrongly_owned_resident_cannot_accept_a_window_delivery() {
        let mut app = window_app();
        let crew = waiting_crew(&mut app, "Dr. Vance", "kelotane", 20, 180.0, true);
        let (_, beaker) = window_with(&mut app, &[("kelotane", 20)]);
        let mut body = Body::default();
        body.0.collapsed = true;
        app.world_mut().entity_mut(crew).insert((
            body,
            Bloodstream::default(),
            crate::utility_ai::UtilityControlBundle::new(crate::utility_ai::UtilityAgent::new(
                71, 0,
            )),
        ));

        app.update();
        assert!(app.world().get::<Order>(crew).is_some());
        assert!(app.world().get_entity(beaker).is_ok());

        app.world_mut().get_mut::<Body>(crew).unwrap().0.collapsed = false;
        app.update();
        assert!(
            app.world().get::<Order>(crew).is_some(),
            "consciousness alone cannot revive an order whose controller no longer owns the NPC",
        );
        assert!(app.world().get_entity(beaker).is_ok());
    }

    #[test]
    fn a_revoked_linked_carrier_drops_the_real_batch_and_reports_unresolved() {
        let data = data();
        let mut app = App::new();
        app.insert_resource(ChemDb(data.clone()))
            .add_message::<ChemicalExposure>()
            .add_message::<FulfillmentApplied>()
            .add_systems(Update, apply_carried_fulfillments);
        let carrier = app.world_mut().spawn_empty().id();
        let mut container = Container::new(ContainerKind::Bottle);
        let reagent = data.reagent("kelotane");
        let _ = container.solution.add(reagent, Units::whole(10));
        let batch = app.world_mut().spawn((container, HeldBy(carrier))).id();
        let beneficiary = app.world_mut().spawn_empty().id();
        app.world_mut().entity_mut(carrier).insert((
            Transform::from_xyz(3.0, crate::crew::BODY_OFFSET, 2.0),
            CrewRoute::standing(),
            Body::default(),
            Bloodstream::default(),
            crate::utility_ai::UtilityControlBundle::new(crate::utility_ai::UtilityAgent::new(
                72, 0,
            )),
            CarryingFulfillment {
                container: batch,
                beneficiary,
                source: None,
                use_destination: Vec3::ZERO,
                route: Route::Patched,
                dose: Units::whole(5),
                supplier: None,
                believed_label: false,
                travel: FulfillmentTravel::ToUseDestination,
            },
        ));

        app.update();

        assert!(app.world().get::<CarryingFulfillment>(carrier).is_none());
        assert!(app.world().get::<HeldBy>(batch).is_none());
        assert!(app.world().get::<Container>(batch).is_some());
        let reports: Vec<_> = app
            .world_mut()
            .resource_mut::<Messages<FulfillmentApplied>>()
            .drain()
            .collect();
        assert_eq!(reports.len(), 1);
        assert_eq!(
            reports[0].result,
            FulfillmentApplicationResult::TargetUnavailable
        );
    }

    #[test]
    fn replacement_delivery_removes_security_hold_before_resident_can_take_another_order() {
        let mut app = window_app();
        let crew = waiting_crew(&mut app, "Dr. Vance", "kelotane", 20, 180.0, true);
        app.world_mut()
            .entity_mut(crew)
            .insert(crate::security_case::OrderHold(42));
        window_with(&mut app, &[("kelotane", 20)]);
        app.update();
        assert!(app.world().get::<Order>(crew).is_none());
        assert!(app
            .world()
            .get::<crate::security_case::OrderHold>(crew)
            .is_none());
        let reagent = reagent_id(&app, "kelotane");
        app.world_mut().entity_mut(crew).insert(Order {
            reagent,
            specific: true,
            minimum_purity: 0.0,
            amount: Units::whole(10),
            plea: "A different request after the replacement was delivered.".into(),
            patience: 180.0,
            waited: 0.0,
        });
        assert!(app
            .world()
            .get::<crate::security_case::OrderHold>(crew)
            .is_none());
    }

    fn reagent_id(app: &App, key: &str) -> ReagentId {
        app.world().resource::<ChemDb>().reagent(key)
    }

    /// A delivery window with `contents` sitting in its slot.
    fn window_with(app: &mut App, contents: &[(&str, i32)]) -> (Entity, Entity) {
        window_with_lane(app, contents, DeliveryLane::Public)
    }

    fn window_with_lane(
        app: &mut App,
        contents: &[(&str, i32)],
        lane: DeliveryLane,
    ) -> (Entity, Entity) {
        let data = app.world().resource::<ChemDb>().0.clone();
        let window = app
            .world_mut()
            .spawn((Machine::new(MachineKind::DeliveryWindow), lane))
            .id();

        let mut container = Container::new(ContainerKind::LargeBeaker);
        for (key, amount) in contents {
            let _ = container
                .solution
                .add(data.reagent(key), Units::whole(*amount));
        }
        let entity = app.world_mut().spawn((container, InSlot(window)));
        (window, entity.id())
    }

    fn add_window_container(
        app: &mut App,
        window: Entity,
        contents: &[(&str, i32)],
        slot: MachineSlot,
    ) -> Entity {
        let data = app.world().resource::<ChemDb>().0.clone();
        let mut container = Container::new(ContainerKind::LargeBeaker);
        for (key, amount) in contents {
            let _ = container
                .solution
                .add(data.reagent(key), Units::whole(*amount));
        }
        let mut item = app.world_mut().spawn(container);
        match slot {
            MachineSlot::A => item.insert(InSlot(window)),
            MachineSlot::B => item.insert(InSlotB(window)),
            MachineSlot::C => item.insert(InSlotC(window)),
        };
        item.id()
    }

    /// A crew member at the counter, or still on their way in.
    fn waiting_crew(
        app: &mut App,
        name: &str,
        wants: &str,
        amount: i32,
        patience: f32,
        arrived: bool,
    ) -> Entity {
        waiting_crew_in_lane(
            app,
            name,
            wants,
            amount,
            patience,
            arrived,
            DeliveryLane::Public,
        )
    }

    fn waiting_crew_in_lane(
        app: &mut App,
        name: &str,
        wants: &str,
        amount: i32,
        patience: f32,
        arrived: bool,
        lane: DeliveryLane,
    ) -> Entity {
        let reagent = reagent_id(app, wants);
        let mut route = CrewRoute::arrival_for(lane, 0.0);
        route.phase = if arrived {
            CrewPhase::Waiting
        } else {
            CrewPhase::Arriving
        };
        app.world_mut()
            .spawn((
                CrewMember {
                    name: name.to_string(),
                    role: "Medical".to_string(),
                },
                Order {
                    reagent,
                    specific: false,
                    minimum_purity: 0.0,
                    amount: Units::whole(amount),
                    plea: String::new(),
                    patience,
                    waited: 0.0,
                },
                route,
            ))
            .id()
    }

    fn outcomes(app: &App) -> Vec<(String, Outcome)> {
        let messages = app.world().resource::<Messages<OrderResolved>>();
        let mut cursor = messages.get_cursor();
        cursor
            .read(messages)
            .map(|report| (report.name.clone(), report.outcome))
            .collect()
    }

    #[test]
    fn curing_a_crisis_victim_takes_the_crisis_marker_off_with_the_order() {
        // `crisis::schedule_crisis` and `crisis::pulse_alert_lighting` both read
        // `Has<CrisisOrder>` as "is a crisis live". `complete_delivery` used to
        // strip only `Order` and `Interactable`, so a cured victim went on
        // reading as a live crisis for the whole walk to the door — keeping the
        // lab red-lit and blocking the next crisis from arming.
        let mut app = window_app();
        let (_window, _beaker) = window_with(&mut app, &[("dylovene", 30)]);
        let victim = waiting_crew(&mut app, "Dr. Vance", "dylovene", 20, 90.0, true);
        app.world_mut().entity_mut(victim).insert(CrisisOrder);

        app.update();

        assert_eq!(
            outcomes(&app),
            vec![("Dr. Vance".to_string(), Outcome::Success)],
            "the cure itself should still land"
        );
        assert!(
            app.world().get::<CrisisOrder>(victim).is_none(),
            "the crisis marker must come off with the order it marks"
        );
    }

    // -- expiry, and the accepting-orders sign -------------------------

    fn expiry_app() -> App {
        let data = data();
        let mut app = App::new();
        app.insert_resource(ChemDb(data))
            .init_resource::<Shift>()
            .init_resource::<Time>()
            .add_message::<OrderResolved>()
            .add_systems(Update, expire_orders);
        app
    }

    fn advance(app: &mut App, seconds: f32) {
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(seconds));
        app.update();
    }

    #[test]
    fn a_closed_sign_does_not_pause_an_orders_clock() {
        // The sign stops new arrivals; it is not a pause button. Whoever is
        // already at the counter keeps waiting, and keeps costing you,
        // exactly as if the sign were still up.
        let mut app = expiry_app();
        app.world_mut().resource_mut::<Shift>().accepting_orders = false;
        let crew = waiting_crew(&mut app, "Dr. Vance", "kelotane", 20, 1.0, true);

        advance(&mut app, 1.5);

        assert!(
            app.world().get::<Order>(crew).is_none(),
            "the order should still expire even with the sign down"
        );
        assert_eq!(app.world().resource::<Shift>().botched, 1);
        assert_eq!(
            outcomes(&app),
            vec![("Dr. Vance".to_string(), Outcome::Expired)]
        );
    }

    #[test]
    fn an_open_sign_behaves_exactly_the_same_way() {
        // Same clock, same outcome — the sign has no effect on an order that
        // is already in progress, only on whether a new one can start.
        let mut app = expiry_app();
        let crew = waiting_crew(&mut app, "Dr. Vance", "kelotane", 20, 1.0, true);

        advance(&mut app, 1.5);

        assert!(app.world().get::<Order>(crew).is_none());
        assert_eq!(app.world().resource::<Shift>().botched, 1);
    }

    #[test]
    fn incapacity_freezes_a_utility_order_until_the_exact_visit_owner_returns() {
        let mut app = expiry_app();
        let crew = waiting_crew(&mut app, "Dr. Vance", "kelotane", 20, 1.0, true);
        let mut body = Body::default();
        body.0.collapsed = true;
        let mut control = crate::utility_ai::UtilityControlBundle::new(
            crate::utility_ai::UtilityAgent::new(73, 0),
        );
        control.control = ControlOwner::Incapacitated;
        app.world_mut()
            .entity_mut(crew)
            .insert((body, Bloodstream::default(), control));

        advance(&mut app, 2.0);
        assert_eq!(app.world().get::<Order>(crew).unwrap().waited, 0.0);

        app.world_mut().get_mut::<Body>(crew).unwrap().0.collapsed = false;
        *app.world_mut().get_mut::<ControlOwner>(crew).unwrap() = ControlOwner::UtilityAction;
        advance(&mut app, 2.0);
        assert_eq!(
            app.world().get::<Order>(crew).unwrap().waited,
            0.0,
            "a stale order remains frozen while UtilityAction owns the resident",
        );

        *app.world_mut().get_mut::<ControlOwner>(crew).unwrap() = ControlOwner::OrderVisit;
        advance(&mut app, 1.5);
        assert!(app.world().get::<Order>(crew).is_none());
    }

    #[test]
    fn ignoring_optional_development_is_a_small_cost_not_a_botch() {
        let mut app = expiry_app();
        let crew = waiting_crew(&mut app, "Dr. Vance", "bicaridine", 10, 1.0, true);
        app.world_mut().entity_mut(crew).insert(DevelopmentOrder {
            expiry_standing: -1,
        });

        advance(&mut app, 1.5);

        let shift = app.world().resource::<Shift>();
        assert_eq!(shift.botched, 0);
        assert_eq!(shift.standing(Department::Medical), -1);
        assert!(app.world().get::<DevelopmentOrder>(crew).is_none());
    }

    #[test]
    fn a_slow_arrival_is_never_charged_for_the_walk() {
        let mut app = expiry_app();
        let crew = waiting_crew(&mut app, "Dr. Vance", "kelotane", 20, 1.0, false);

        advance(&mut app, 5.0);

        assert!(
            app.world().get::<Order>(crew).is_some(),
            "patience must not run while still walking in"
        );
    }

    // -- the antagonist thread's grading fall-through --------------------

    #[test]
    fn a_successful_illicit_delivery_never_touches_the_pretext_departments_standing() {
        // The department was never the real requester — crediting it would
        // be a narrative contradiction. `antagonist::handle_illicit_resolutions`
        // is where the real consequences live; this only proves grading
        // itself stays out of the way.
        let mut app = window_app();
        let (_, beaker) = window_with(&mut app, &[("dylovene", 30)]);
        let crew = waiting_crew(&mut app, "Dr. Vance", "dylovene", 30, 60.0, true);
        app.world_mut().entity_mut(crew).insert(IllicitOrder);

        app.update();

        assert_eq!(
            outcomes(&app),
            vec![("Dr. Vance".to_string(), Outcome::Success)]
        );
        assert!(app.world().get_entity(beaker).is_err());
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .standing(Department::Medical),
            0,
            "a successful illicit delivery must not move the pretext department"
        );
    }

    #[test]
    fn a_declined_illicit_order_grades_like_any_other_bad_outcome() {
        // Nothing special happens on a decline — it falls through to the
        // exact same expiry path a legitimate order takes, dollar for dollar.
        let illicit_delta = {
            let mut app = expiry_app();
            let crew = waiting_crew(&mut app, "Dr. Vance", "kelotane", 20, 1.0, true);
            app.world_mut().entity_mut(crew).insert(IllicitOrder);
            advance(&mut app, 1.5);
            app.world()
                .resource::<Shift>()
                .standing(Department::Medical)
        };
        let legitimate_delta = {
            let mut app = expiry_app();
            waiting_crew(&mut app, "Dr. Vance", "kelotane", 20, 1.0, true);
            advance(&mut app, 1.5);
            app.world()
                .resource::<Shift>()
                .standing(Department::Medical)
        };
        assert_eq!(
            illicit_delta, legitimate_delta,
            "a declined illicit order must cost exactly what an ordinary one does"
        );
        assert_ne!(
            illicit_delta, 0,
            "an expired order should have cost something"
        );
    }

    #[test]
    fn the_window_hands_over_to_whoever_is_waiting_for_it() {
        let mut app = window_app();
        let (_, beaker) = window_with(&mut app, &[("dylovene", 30)]);
        let crew = waiting_crew(&mut app, "Dr. Vance", "dylovene", 30, 60.0, true);

        app.update();

        assert_eq!(
            outcomes(&app),
            vec![("Dr. Vance".to_string(), Outcome::Success)]
        );
        assert!(
            app.world().get_entity(beaker).is_err(),
            "they walk off with the glassware"
        );
        assert!(
            app.world().get::<Order>(crew).is_none(),
            "the order should be closed out"
        );
        assert_eq!(app.world().resource::<Shift>().succeeded, 1);
    }

    #[test]
    fn three_tray_positions_deliver_to_three_distinct_orders_in_one_frame() {
        let mut app = window_app();
        let (window, first) = window_with(&mut app, &[("dylovene", 30)]);
        let second = add_window_container(&mut app, window, &[("kelotane", 30)], MachineSlot::B);
        let third = add_window_container(&mut app, window, &[("bicaridine", 30)], MachineSlot::C);
        for (name, reagent) in [
            ("Dylovene patient", "dylovene"),
            ("Kelotane patient", "kelotane"),
            ("Bicaridine patient", "bicaridine"),
        ] {
            waiting_crew(&mut app, name, reagent, 30, 60.0, true);
        }

        app.update();

        assert_eq!(outcomes(&app).len(), 3);
        assert_eq!(app.world().resource::<Shift>().succeeded, 3);
        for container in [first, second, third] {
            assert!(app.world().get_entity(container).is_err());
        }
    }

    #[test]
    fn one_order_cannot_consume_two_matching_tray_containers_before_commands_apply() {
        let mut app = window_app();
        let (window, first) = window_with(&mut app, &[("dylovene", 30)]);
        let second = add_window_container(&mut app, window, &[("dylovene", 30)], MachineSlot::B);
        waiting_crew(&mut app, "Only patient", "dylovene", 30, 60.0, true);

        app.update();

        assert_eq!(outcomes(&app).len(), 1);
        let remaining = [first, second]
            .into_iter()
            .filter(|entity| app.world().get_entity(*entity).is_ok())
            .count();
        assert_eq!(
            remaining, 1,
            "the unmatched spare batch must remain on its tray"
        );
    }

    #[test]
    fn neither_handover_nor_tray_can_consume_an_unaccepted_request() {
        use crate::order_intake::{GreetingKind, PendingOrder, RequestContext, RequestSource};
        let mut app = window_app();
        app.add_message::<FromClient<InteractRequested>>()
            .add_systems(Update, handle_delivery.before(handle_window_delivery));
        let (_, tray) = window_with(&mut app, &[("dylovene", 30)]);
        let npc = waiting_crew(&mut app, "Unheard", "dylovene", 30, 100.0, true);
        let order = app.world().get::<Order>(npc).unwrap().clone();
        app.world_mut()
            .entity_mut(npc)
            .remove::<Order>()
            .insert(PendingOrder::new(
                order,
                RequestContext {
                    id: 1,
                    source: RequestSource::Ordinary,
                    campaign: None,
                    greeting: GreetingKind::Ordinary,
                    step: None,
                },
            ));
        let player = app
            .world_mut()
            .spawn(Chemist {
                client: ClientId::Server,
            })
            .id();
        let source = app.world().get::<Container>(tray).unwrap();
        let container = Container {
            kind: source.kind,
            solution: source.solution.clone(),
        };
        let hand = app.world_mut().spawn((container, HeldBy(player))).id();
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: InteractRequested { target: npc },
        });
        app.update();
        assert!(outcomes(&app).is_empty());
        assert!(app.world().get::<Container>(hand).is_some());
        assert!(app.world().get::<Container>(tray).is_some());
        assert!(app.world().get::<PendingOrder>(npc).is_some());
    }

    #[test]
    fn tray_can_serve_an_accepted_customer_while_they_step_into_line() {
        let mut app = window_app();
        let (_, beaker) = window_with(&mut app, &[("dylovene", 30)]);
        let npc = waiting_crew(&mut app, "Accepted", "dylovene", 30, 100.0, false);
        app.world_mut()
            .entity_mut(npc)
            .insert(crate::order_intake::AcceptedOrder { sequence: 1 });
        app.update();
        assert!(app.world().get::<Container>(beaker).is_none());
        assert_eq!(outcomes(&app).len(), 1);
    }

    #[test]
    fn recipient_reservation_excludes_an_order_before_deferred_removal() {
        let db = db();
        let reagent = db.reagent("dylovene");
        let contents = solution_of(&db, &[("dylovene", 30)]);
        let order = Order {
            reagent,
            specific: false,
            minimum_purity: 0.0,
            amount: Units::whole(30),
            plea: String::new(),
            patience: 60.0,
            waited: 0.0,
        };
        let mut route = CrewRoute::arrival_for(DeliveryLane::Public, 0.0);
        route.phase = CrewPhase::Waiting;
        let recipient = Entity::PLACEHOLDER;
        let reserved = HashSet::from([recipient]);

        assert_eq!(
            window_recipient(
                &contents,
                std::iter::once((recipient, &order, &route, OrderKind::Normal, false)),
                &reserved,
                DeliveryLane::Public,
                &db,
            ),
            None
        );
    }

    #[test]
    fn each_window_only_serves_orders_assigned_to_its_lane() {
        let mut app = window_app();
        let (_, beaker) = window_with_lane(&mut app, &[("dylovene", 30)], DeliveryLane::Public);
        let patient = waiting_crew_in_lane(
            &mut app,
            "Clinical patient",
            "dylovene",
            30,
            10.0,
            true,
            DeliveryLane::Medical,
        );
        let visitor = waiting_crew_in_lane(
            &mut app,
            "Public visitor",
            "dylovene",
            30,
            60.0,
            true,
            DeliveryLane::Public,
        );

        app.update();

        assert!(app.world().get::<Order>(patient).is_some());
        assert!(app.world().get::<Order>(visitor).is_none());
        assert!(app.world().get_entity(beaker).is_err());
        assert_eq!(
            outcomes(&app),
            vec![("Public visitor".to_string(), Outcome::Success)]
        );
    }

    #[test]
    fn a_delivered_solution_actually_lands_on_the_recipient() {
        // M12: a delivery is not just graded, it is drunk. Handing over
        // something should show up in the recipient's own Bloodstream, not
        // just the order's outcome. Ingested lands in the stomach first
        // (same as a chemist's own sip, `body::tests::drinking_takes_a_
        // mouthful_out_of_the_beaker_and_into_the_stomach`) — turning that
        // into a felt `Drunk` status is `run_metabolism`'s job, already
        // covered in `body::mod::tests`, not this module's to re-prove.
        let mut app = window_app();
        window_with(&mut app, &[("hooch", 10)]);
        let crew = waiting_crew(&mut app, "Mx. Sample", "hooch", 10, 60.0, true);
        app.world_mut()
            .entity_mut(crew)
            .insert((Body::default(), Bloodstream::default()));

        app.update();

        assert_eq!(
            outcomes(&app),
            vec![("Mx. Sample".to_string(), Outcome::Success)]
        );
        let hooch = reagent_id(&app, "hooch");
        let blood = app.world().get::<Bloodstream>(crew).unwrap();
        assert!(
            blood.0.stomach.volume_of(hooch).is_positive(),
            "the recipient should actually have swallowed something, not just been graded a success"
        );
    }

    #[test]
    fn nobody_drinks_the_space_cleaner_they_were_handed() {
        // Reported from live play: "i gave a tech guy space cleaner and he
        // drank it himself instead of using it where he needed it."
        //
        // `complete_delivery`'s personal-consumption branch assumed every
        // delivery ends in somebody's stomach, so it swallowed a utility
        // chemical whose own reference entry reads "Do not drink it."
        let mut app = window_app();
        window_with(&mut app, &[("space_cleaner", 20)]);
        let tech = waiting_crew(&mut app, "Tech Lindqvist", "space_cleaner", 20, 60.0, true);
        app.world_mut()
            .entity_mut(tech)
            .insert((Body::default(), Bloodstream::default()));

        app.update();

        // The delivery still succeeds — they asked for cleaner and got
        // cleaner. Only the swallowing is wrong.
        assert_eq!(
            outcomes(&app),
            vec![("Tech Lindqvist".to_string(), Outcome::Success)]
        );
        let cleaner = reagent_id(&app, "space_cleaner");
        let blood = app.world().get::<Bloodstream>(tech).unwrap();
        assert!(
            blood.0.stomach.volume_of(cleaner).is_zero(),
            "the technician drank the space cleaner instead of cleaning with it"
        );
    }

    #[test]
    fn a_wrong_medicine_is_still_swallowed() {
        // The other side of the guard, and the reason it keys on what the
        // chemical *is* rather than on whether the order matched. Filling an
        // order with the wrong medicine is a real mistake with real
        // consequences, and softening that would remove the stakes from every
        // delivery in the game.
        let mut app = window_app();
        window_with(&mut app, &[("hooch", 10)]);
        let crew = waiting_crew(&mut app, "Mx. Sample", "hooch", 10, 60.0, true);
        app.world_mut()
            .entity_mut(crew)
            .insert((Body::default(), Bloodstream::default()));

        app.update();

        let hooch = reagent_id(&app, "hooch");
        let blood = app.world().get::<Bloodstream>(crew).unwrap();
        assert!(
            blood.0.stomach.volume_of(hooch).is_positive(),
            "guarding cleaners must not stop an ordinary delivery being taken"
        );
    }

    #[test]
    fn the_window_serves_the_most_urgent_matching_order() {
        // A batch that could satisfy two people goes to whoever is closest to
        // walking out, matching how the order queue itself is sorted.
        let mut app = window_app();
        window_with(&mut app, &[("dylovene", 30)]);
        waiting_crew(&mut app, "Patient", "dylovene", 30, 200.0, true);
        waiting_crew(&mut app, "Desperate", "dylovene", 30, 12.0, true);

        app.update();

        assert_eq!(
            outcomes(&app),
            vec![("Desperate".to_string(), Outcome::Success)]
        );
    }

    #[test]
    fn the_window_waits_for_crew_who_have_not_reached_the_counter() {
        // Handing a beaker through the window to someone still coming in the
        // door would be nonsense; the tray just holds it until they arrive.
        let mut app = window_app();
        let (_, beaker) = window_with(&mut app, &[("dylovene", 30)]);
        waiting_crew(&mut app, "En route", "dylovene", 30, 60.0, false);

        app.update();

        assert!(outcomes(&app).is_empty());
        assert!(
            app.world().get_entity(beaker).is_ok(),
            "the batch stays in the window until someone is there to take it"
        );
    }

    #[test]
    fn the_window_leaves_a_batch_nobody_asked_for() {
        let mut app = window_app();
        let (_, beaker) = window_with(&mut app, &[("kelotane", 30)]);
        waiting_crew(&mut app, "Dr. Vance", "dylovene", 30, 60.0, true);

        app.update();

        assert!(outcomes(&app).is_empty());
        assert!(app.world().get_entity(beaker).is_ok());
    }

    #[test]
    fn the_window_matches_on_the_reagent_but_still_grades_honestly() {
        // The window picks a recipient; it does not vet the delivery. This is
        // where ground produce gets caught — right chemical, plant fibre still
        // in it — so putting a dirty batch in the tray is a real mistake and
        // not silently prevented.
        let mut app = window_app();
        window_with(&mut app, &[("dylovene", 30), ("plant_fibre", 20)]);
        waiting_crew(&mut app, "Dr. Vance", "dylovene", 30, 60.0, true);

        app.update();

        assert_eq!(
            outcomes(&app),
            vec![("Dr. Vance".to_string(), Outcome::Impure)]
        );
        assert_eq!(app.world().resource::<Shift>().botched, 1);
    }

    fn db() -> ChemDb {
        ChemDb(data())
    }

    #[test]
    fn exact_pure_delivery_succeeds() {
        let db = db();
        let bicaridine = db.reagent("bicaridine");
        let delivered = solution_of(&db, &[("bicaridine", 30)]);

        let (outcome, matched) = grade(
            Wanted::Exact(bicaridine),
            Units::whole(30),
            &delivered,
            ContainerKind::Beaker,
            &db,
        );

        assert_eq!(outcome, Outcome::Success);
        assert_eq!(matched, Some(bicaridine));
    }

    #[test]
    fn a_stimulant_order_is_satisfied_by_methamphetamine() {
        // The double life, working. A lenient order asks for a *category*, and
        // meth is genuinely in `Stimulants` — the same `Hastened`-then-
        // `Sluggish` shape as the perfectly legal hyperzine — so the person
        // who asked for something to keep them sharp is not being fooled at
        // the counter. They got what they wanted.
        //
        // The price is entirely downstream and entirely real: it is Illicit,
        // so a sweep still finds it; it is `addictive`, so
        // `addiction::note_doses` hooks them off the bloodstream alone; and
        // `notice_the_high` turns them standing high in front of an officer
        // into rising suspicion. That is the whole bargain in one delivery.
        let db = db();
        let meth = db.reagent("methamphetamine");
        let delivered = solution_of(&db, &[("methamphetamine", 8)]);

        let (outcome, matched) = grade(
            Wanted::Category(Category::Stimulants),
            Units::whole(8),
            &delivered,
            ContainerKind::Beaker,
            &db,
        );

        assert_eq!(
            outcome,
            Outcome::Success,
            "meth is a stimulant; a stimulant order filled with it should satisfy"
        );
        assert_eq!(matched, Some(meth));
        assert!(
            db.reagents.get(meth).addictive > 0.0,
            "the whole point is that they come back for it"
        );
        assert!(
            db.reagents
                .get(meth)
                .categories
                .contains(&Category::Illicit),
            "and that a raid would still find it"
        );
    }

    #[test]
    fn an_honest_stimulant_still_beats_meth_when_both_are_in_the_beaker() {
        // Sanity on the substitution: `grade` picks the dominant category
        // member by volume, so this is not a back door that lets a trace of
        // meth hijack an otherwise honest delivery. A clean batch grades
        // against the clean reagent.
        let db = db();
        let hyperzine = db.reagent("hyperzine");
        let delivered = solution_of(&db, &[("hyperzine", 20), ("methamphetamine", 2)]);

        let (_, matched) = grade(
            Wanted::Category(Category::Stimulants),
            Units::whole(20),
            &delivered,
            ContainerKind::Beaker,
            &db,
        );

        assert_eq!(matched, Some(hyperzine));
    }

    #[test]
    fn contamination_is_caught_even_when_the_amount_is_right() {
        // This is the common failure: a sloppy mix leaves leftovers that keep
        // reacting, so the beaker holds the right medicine plus something else.
        let db = db();
        let delivered = solution_of(&db, &[("bicaridine", 30), ("inaprovaline", 5)]);

        let (outcome, _) = grade(
            Wanted::Exact(db.reagent("bicaridine")),
            Units::whole(30),
            &delivered,
            ContainerKind::Beaker,
            &db,
        );

        assert_eq!(outcome, Outcome::Impure);
    }

    #[test]
    fn a_beaker_is_bulk_supply_and_cannot_overdose() {
        let db = db();
        let delivered = solution_of(&db, &[("bicaridine", 40)]);

        let (outcome, _) = grade(
            Wanted::Exact(db.reagent("bicaridine")),
            Units::whole(30),
            &delivered,
            ContainerKind::Beaker,
            &db,
        );

        assert_eq!(outcome, Outcome::Success);
    }

    #[test]
    fn a_pill_over_the_threshold_is_an_overdose() {
        let db = db();
        let delivered = solution_of(&db, &[("bicaridine", 20)]);

        let (outcome, _) = grade(
            Wanted::Exact(db.reagent("bicaridine")),
            Units::whole(20),
            &delivered,
            ContainerKind::Pill,
            &db,
        );

        assert_eq!(outcome, Outcome::Overdose);
    }

    #[test]
    fn overdose_outranks_contamination() {
        let db = db();
        let delivered = solution_of(&db, &[("bicaridine", 20), ("oxygen", 3)]);

        let (outcome, _) = grade(
            Wanted::Exact(db.reagent("bicaridine")),
            Units::whole(20),
            &delivered,
            ContainerKind::Pill,
            &db,
        );

        assert_eq!(outcome, Outcome::Overdose, "the worse problem must win");
    }

    #[test]
    fn too_little_is_short_not_success() {
        let db = db();
        let delivered = solution_of(&db, &[("dylovene", 10)]);

        let (outcome, _) = grade(
            Wanted::Exact(db.reagent("dylovene")),
            Units::whole(30),
            &delivered,
            ContainerKind::Beaker,
            &db,
        );

        assert_eq!(outcome, Outcome::Short);
    }

    #[test]
    fn the_wrong_chemical_entirely_is_wrong() {
        let db = db();
        let delivered = solution_of(&db, &[("kelotane", 40)]);

        let (outcome, matched) = grade(
            Wanted::Exact(db.reagent("bicaridine")),
            Units::whole(30),
            &delivered,
            ContainerKind::Beaker,
            &db,
        );

        assert_eq!(outcome, Outcome::Wrong);
        assert_eq!(matched, None);
    }

    #[test]
    fn a_reagent_with_no_overdose_threshold_never_overdoses() {
        let db = db();
        let inaprovaline = db.reagent("inaprovaline");
        assert!(
            db.reagents.get(inaprovaline).overdose.is_none(),
            "inaprovaline is meant to be safe at any dose"
        );
        let delivered = solution_of(&db, &[("inaprovaline", 20)]);

        let (outcome, _) = grade(
            Wanted::Exact(inaprovaline),
            Units::whole(20),
            &delivered,
            ContainerKind::Pill,
            &db,
        );

        assert_eq!(outcome, Outcome::Success);
    }

    // -- lenient category grading --------------------------------------

    #[test]
    fn a_category_order_accepts_a_different_member_than_the_reference_reagent() {
        // Kelotane and Dermaline are both `Category::Burns`. A legitimate
        // order authored around Kelotane must still succeed on Dermaline —
        // that substitution is the entire point of the feature.
        let db = db();
        let delivered = solution_of(&db, &[("dermaline", 20)]);

        let (outcome, matched) = grade(
            Wanted::Category(Category::Burns),
            Units::whole(20),
            &delivered,
            ContainerKind::Beaker,
            &db,
        );

        assert_eq!(outcome, Outcome::Success);
        assert_eq!(matched, Some(db.reagent("dermaline")));
    }

    #[test]
    fn a_category_order_with_nothing_matching_is_wrong_and_names_no_reagent() {
        let db = db();
        let delivered = solution_of(&db, &[("kelotane", 20)]);

        let (outcome, matched) = grade(
            Wanted::Category(Category::Trauma),
            Units::whole(20),
            &delivered,
            ContainerKind::Beaker,
            &db,
        );

        assert_eq!(outcome, Outcome::Wrong);
        assert_eq!(
            matched, None,
            "nothing should be named back for an unmatched category order"
        );
    }

    #[test]
    fn a_category_order_picks_the_dominant_member_present() {
        // Two different Burns treatments in the same beaker: the resolver
        // grades against whichever one dominates by volume, and still flags
        // Impure because something else was present regardless.
        let db = db();
        let delivered = solution_of(&db, &[("kelotane", 5), ("dermaline", 25)]);

        let (outcome, matched) = grade(
            Wanted::Category(Category::Burns),
            Units::whole(20),
            &delivered,
            ContainerKind::Beaker,
            &db,
        );

        assert_eq!(outcome, Outcome::Impure);
        assert_eq!(matched, Some(db.reagent("dermaline")));
    }

    #[test]
    fn exact_quality_requirement_rejects_a_usable_but_impure_batch() {
        let db = db();
        let reagent = db.reagent("bicaridine");
        let mut delivered = Solution::new(Units::whole(50));
        let _ = delivered.add_profiled(reagent, Units::whole(20), 0.72, 7.0);
        let (outcome, matched) = grade(
            Wanted::Exact(reagent),
            Units::whole(20),
            &delivered,
            ContainerKind::Beaker,
            &db,
        );

        assert_eq!(outcome, Outcome::Success, "the medicine remains usable");
        assert_eq!(
            enforce_minimum_purity(outcome, matched, 0.85, &delivered),
            Outcome::Impure,
            "a precision order may still reject it"
        );
    }

    // -- specific (exact-asking) orders -----------------------------------

    fn specific_order(db: &ChemDb, reagent: &str, amount: i32) -> Order {
        Order {
            reagent: db.reagent(reagent),
            specific: true,
            minimum_purity: 0.0,
            amount: Units::whole(amount),
            plea: String::new(),
            patience: 60.0,
            waited: 0.0,
        }
    }

    #[test]
    fn a_specific_order_refuses_a_different_member_of_the_same_category() {
        // Kelotane and Dermaline are both `Category::Burns` — a lenient order
        // accepts either, but a specific one asked for Kelotane by name and
        // means it.
        let db = db();
        let order = specific_order(&db, "kelotane", 20);
        let delivered = solution_of(&db, &[("dermaline", 20)]);

        let (outcome, matched) = grade(
            wanted_for(&order, OrderKind::Normal, &db),
            order.amount,
            &delivered,
            ContainerKind::Beaker,
            &db,
        );

        assert_eq!(outcome, Outcome::Wrong);
        assert_eq!(matched, None);
    }

    #[test]
    fn a_specific_order_succeeds_on_its_own_named_reagent() {
        let db = db();
        let order = specific_order(&db, "kelotane", 20);
        let delivered = solution_of(&db, &[("kelotane", 20)]);

        let (outcome, matched) = grade(
            wanted_for(&order, OrderKind::Normal, &db),
            order.amount,
            &delivered,
            ContainerKind::Beaker,
            &db,
        );

        assert_eq!(outcome, Outcome::Success);
        assert_eq!(matched, Some(db.reagent("kelotane")));
    }

    #[test]
    fn a_specific_order_names_itself_even_when_wrong() {
        // Nothing about a specific ask is secret — it named its reagent in
        // the prompt already, so a Wrong report should still name it, unlike
        // a plain lenient order's fall-through to a bare category.
        let db = db();
        let order = specific_order(&db, "kelotane", 20);
        assert!(is_named(&order, OrderKind::Normal));
    }

    // -- order kind -------------------------------------------------------

    #[test]
    fn an_order_belongs_to_exactly_one_thread() {
        // The whole reason this is an enum and not three booleans: the pair it
        // replaced could express `illicit && crisis`, which never meant
        // anything. Reading the markers can only ever produce one answer.
        assert_eq!(OrderKind::of(false, false, false), OrderKind::Normal);
        assert_eq!(OrderKind::of(true, false, false), OrderKind::Illicit);
        assert_eq!(OrderKind::of(false, true, false), OrderKind::Crisis);
        assert_eq!(OrderKind::of(false, false, true), OrderKind::Counter);
    }

    #[test]
    fn only_an_illicit_order_has_already_named_its_reagent() {
        // `is_named`/`wanted_for` hang off this, and it is what keeps a
        // lenient order's reference reagent unnamed on a bad resolution. A
        // crisis or counter request is public but still *lenient* — it accepts
        // any member of its category, so it must not name the reagent it was
        // built from.
        assert!(OrderKind::Illicit.names_its_reagent());
        for kind in [OrderKind::Normal, OrderKind::Crisis, OrderKind::Counter] {
            assert!(
                !kind.names_its_reagent(),
                "{kind:?} would leak the reference reagent it was never told to reveal"
            );
        }
    }

    // -- standing ---------------------------------------------------------

    #[test]
    fn standing_scale_is_exactly_minus_ten_to_ten() {
        assert_eq!(STANDING_FLOOR, -10);
        assert_eq!(STANDING_CEILING, 10);
    }

    #[test]
    fn standing_is_clamped_to_the_shared_scale() {
        let mut shift = Shift::default();
        for _ in 0..50 {
            shift.adjust(Department::Medical, -4);
        }
        assert_eq!(shift.standing(Department::Medical), STANDING_FLOOR);

        for _ in 0..50 {
            shift.adjust(Department::Medical, 4);
        }
        assert_eq!(shift.standing(Department::Medical), STANDING_CEILING);
    }

    #[test]
    fn the_floor_does_not_interfere_with_ordinary_movement() {
        let mut shift = Shift::default();
        shift.adjust(Department::Cargo, 7);
        shift.adjust(Department::Cargo, -3);
        assert_eq!(shift.standing(Department::Cargo), 4);

        // And climbing back out of the floor still works normally.
        shift.adjust(Department::Cargo, STANDING_FLOOR * 4);
        shift.adjust(Department::Cargo, 5);
        assert_eq!(shift.standing(Department::Cargo), STANDING_FLOOR + 5);
    }

    #[test]
    fn voluntary_goodwill_spending_stops_at_the_floor() {
        let mut shift = Shift::default();
        shift.spend_goodwill(Department::Cargo, 30);
        shift.spend_goodwill(Department::Cargo, 7);

        assert_eq!(shift.standing(Department::Cargo), STANDING_FLOOR);
    }

    #[test]
    fn work_repays_goodwill_immediately_from_the_floor() {
        let mut shift = Shift::default();
        shift.spend_goodwill(Department::Cargo, 37);
        shift.adjust(Department::Cargo, -4);
        assert_eq!(shift.standing(Department::Cargo), STANDING_FLOOR);

        shift.adjust(Department::Cargo, 5);
        assert_eq!(shift.standing(Department::Cargo), STANDING_FLOOR + 5);
    }

    #[test]
    fn rogue_security_thresholds_fit_inside_the_standing_scale() {
        let rogue: crate::rogue_security::RogueSecurityScript =
            ron::from_str(include_str!("../../assets/data/station.rogue_security.ron")).unwrap();
        assert!(
            STANDING_FLOOR < rogue.hostile_below,
            "the floor at {STANDING_FLOOR} sits at or above Security's own \
             threshold of {}, so they could never turn",
            rogue.hostile_below
        );
        assert!(
            rogue.redeemed_at <= STANDING_CEILING,
            "redemption at {} exceeds the standing ceiling of {STANDING_CEILING}",
            rogue.redeemed_at
        );
    }

    #[test]
    fn old_out_of_range_standing_is_normalized() {
        let mut shift = Shift::default();
        shift.npc_standing.insert("Quartermaster Reyes".into(), -40);
        shift.npc_standing.insert("Tech Lindqvist".into(), 25);
        shift.opened_at = Some(ShiftSnapshot {
            department_standing: [(Department::Cargo, -40), (Department::Engineering, 25)]
                .into_iter()
                .collect(),
            ..default()
        });

        shift.clamp_standing();

        assert_eq!(shift.npc_standing("Quartermaster Reyes"), STANDING_FLOOR);
        assert_eq!(shift.npc_standing("Tech Lindqvist"), STANDING_CEILING);
        let snapshot = shift.opened_at.as_ref().unwrap();
        assert_eq!(
            snapshot.department_standing[&Department::Cargo],
            STANDING_FLOOR
        );
        assert_eq!(
            snapshot.department_standing[&Department::Engineering],
            STANDING_CEILING
        );
    }

    #[test]
    fn every_roster_member_is_exactly_one_departments_own() {
        // `Department::members()` mirrors `station.crew.ron` by hand, the
        // same way `Department::from_role` already hardcodes the relationship role
        // strings. A roster edit that forgets to update it would otherwise
        // silently average over the wrong headcount instead of failing loud.
        let roster: Vec<CrewDef> =
            ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();
        let total_members: usize = Department::ALL.iter().map(|d| d.members().len()).sum();
        let recognised = roster
            .iter()
            .filter(|member| Department::from_role(&member.role).is_some())
            .count();
        assert_eq!(
            total_members, recognised,
            "Department::members() headcount does not match the roster"
        );
        for member in &roster {
            let Some(department) = Department::from_role(&member.role) else {
                continue;
            };
            assert!(
                department.members().contains(&member.name.as_str()),
                "'{}' is on the roster as {:?} but missing from Department::members()",
                member.name,
                department
            );
        }
    }

    #[test]
    fn adjust_moves_every_department_member_by_the_same_delta() {
        let mut shift = Shift::default();
        shift.adjust(Department::Medical, 6);
        for name in Department::Medical.members() {
            assert_eq!(shift.npc_standing(name), 6);
        }
        assert_eq!(shift.standing(Department::Medical), 6);
    }

    #[test]
    fn adjust_npc_moves_only_that_one_member_and_the_department_average_follows() {
        // The one genuinely new behaviour: an individual action (a personal
        // shop purchase) must not bleed onto a colleague in the same
        // department, but the displayed department number still reflects it.
        let mut shift = Shift::default();
        let [ivy, vale] = Department::Botany.members() else {
            panic!("Botany should have exactly two members");
        };
        shift.adjust_npc(ivy, -10);

        assert_eq!(shift.npc_standing(ivy), -10);
        assert_eq!(shift.npc_standing(vale), 0, "Vale must be untouched");
        assert_eq!(
            shift.standing(Department::Botany),
            -5,
            "the shown average moves by half the individual delta"
        );

        let [dubois, amari] = Department::Service.members() else {
            panic!("Service should have exactly two members");
        };
        shift.adjust_npc(dubois, 6);

        assert_eq!(shift.npc_standing(dubois), 6);
        assert_eq!(shift.npc_standing(amari), 0, "Amari must be untouched");
        assert_eq!(
            shift.standing(Department::Service),
            3,
            "the shown average moves by half the individual delta"
        );
    }

    // -- where a sample vial lands ----------------------------------------

    #[test]
    fn a_second_vial_does_not_land_inside_the_first() {
        // Vials used to drop at exactly `COUNTER_SPOT.x` every time, so the
        // second one was invisible inside the first and read as the game
        // having simply not given it to you.
        let first = free_vial_lane(&[]);
        let second = free_vial_lane(&[first]);
        let third = free_vial_lane(&[first, second]);

        assert!((first.x - second.x).abs() >= VIAL_SPACING * 0.5);
        assert!((second.x - third.x).abs() >= VIAL_SPACING * 0.5);
        assert!((first.x - third.x).abs() >= VIAL_SPACING * 0.5);
    }

    #[test]
    fn vials_reuse_a_lane_that_has_been_cleared() {
        // Picking one up should not leave a permanent hole — the next vial
        // takes the nearest free spot, not the next index.
        let first = free_vial_lane(&[]);
        let second = free_vial_lane(&[first]);

        assert_eq!(free_vial_lane(&[second]).x, first.x);
    }

    #[test]
    fn a_full_counter_stacks_rather_than_spawning_through_a_wall() {
        let mut taken: Vec<Vec3> = Vec::new();
        for _ in 0..VIAL_LANES {
            taken.push(free_vial_lane(&taken));
        }

        let lobby = &crate::lab::ROOMS[crate::lab::LOBBY];
        for spot in &taken {
            assert!(
                spot.x < lobby.max_x,
                "lane at {} reaches past the lobby's east wall at {}",
                spot.x,
                lobby.max_x
            );
        }
        // Overlapping is bad; spawning outside the room is worse.
        assert_eq!(free_vial_lane(&taken).x, COUNTER_SPOT.x);
    }

    #[test]
    fn vials_never_land_in_the_glassware_crate() {
        // The crate is laid out west of the drop; lanes run east. If they ever
        // ran the other way a vial would land inside a beaker.
        let lanes: Vec<Vec3> = {
            let mut taken = Vec::new();
            for _ in 0..VIAL_LANES {
                taken.push(free_vial_lane(&taken));
            }
            taken
        };
        assert!(lanes.iter().all(|spot| spot.x >= COUNTER_SPOT.x));
    }

    #[test]
    fn something_sitting_elsewhere_in_the_lab_does_not_take_a_lane() {
        // The occupancy check is scoped to the drop's own z, or a beaker left
        // on a bench across the room would push vials down the counter.
        let bench = Vec3::new(COUNTER_SPOT.x, crate::lab::COUNTER_TOP, -3.0);
        assert_eq!(free_vial_lane(&[bench]).x, COUNTER_SPOT.x);
    }

    // -- order generation --------------------------------------------------

    #[test]
    fn bulk_quantities_use_their_own_five_percent_pool() {
        let amounts = [30, 40, 50];
        assert_eq!(requested_amount_pool(&amounts, 0.05, 0.049), [40, 50]);
        assert_eq!(requested_amount_pool(&amounts, 0.05, 0.05), [30]);
        assert_eq!(requested_amount_pool(&amounts, 0.05, 0.99), [30]);
    }

    #[test]
    fn every_bulk_capable_station_request_has_an_ordinary_fallback() {
        for request in &station_orders().requests {
            if request.amounts.iter().any(|amount| *amount > 30) {
                assert!(
                    request.amounts.iter().any(|amount| *amount <= 30),
                    "{} offers bulk quantities but no <=30u fallback",
                    request.reagent
                );
            }
        }
    }

    /// Enough world to run `generate_orders` for real, headless: a fresh
    /// `Knowledge`, the real crew roster and order requests, and a spawner
    /// timer already due. `patience_seconds` is pinned to a single value
    /// (rather than the real file's range) so the roll it drives is
    /// deterministic — `ShiftRules::for_tier` reproduces the base config
    /// exactly at tier 0, which a freshly-opened `Shift` always is.
    fn generate_orders_app() -> App {
        let mut config = station_orders();
        config.patience_seconds = (PINNED_PATIENCE, PINNED_PATIENCE);
        let crew: Vec<CrewDef> =
            ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();

        let mut app = App::new();
        app.insert_resource(ChemDb(data()))
            .insert_resource(Knowledge::new(&data()))
            .insert_resource(StationData { crew, config })
            .insert_resource(OrderSpawner {
                timer: Timer::from_seconds(0.0, TimerMode::Once),
            })
            .insert_resource(Shift {
                accepting_orders: true,
                ..Default::default()
            })
            .init_resource::<Time>()
            .init_resource::<RadioLog>()
            .add_systems(Update, generate_orders);
        app
    }

    /// Pinned so a comped roll's result is checkable exactly, not just
    /// "bigger than before". Above `station.orders.ron`'s own
    /// `ramp.patience_floor` (80.0) — `ShiftRules::for_tier` clamps up to
    /// that floor, so anything below it would silently stop testing what it
    /// says it does.
    const PINNED_PATIENCE: f32 = 100.0;

    #[test]
    fn a_comped_round_adds_patience_to_exactly_the_next_order() {
        let mut app = generate_orders_app();
        app.world_mut()
            .resource_mut::<Shift>()
            .requisition
            .patience_bonus_orders = 1;

        advance(&mut app, 1.0);

        let mut query = app
            .world_mut()
            .query::<&crate::order_intake::PendingOrder>();
        let orders: Vec<&Order> = query.iter(app.world()).map(|p| &p.order).collect();
        assert_eq!(orders.len(), 1, "exactly one order should have spawned");
        assert_eq!(
            orders[0].patience,
            PINNED_PATIENCE + crate::shift::COMPED_PATIENCE_BONUS_SECONDS,
            "a banked Comped Round should add its bonus to this order's patience"
        );
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .requisition
                .patience_bonus_orders,
            0,
            "the bonus is spent, not banked indefinitely"
        );
    }

    #[test]
    fn without_a_comped_round_patience_is_unmodified() {
        let mut app = generate_orders_app();

        advance(&mut app, 1.0);

        let mut query = app
            .world_mut()
            .query::<&crate::order_intake::PendingOrder>();
        let orders: Vec<&Order> = query.iter(app.world()).map(|p| &p.order).collect();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].patience, PINNED_PATIENCE);
    }

    #[test]
    fn the_opening_run_only_asks_for_chemistry_the_lab_can_make_now() {
        let mut app = generate_orders_app();
        {
            let mut station = app.world_mut().resource_mut::<StationData>();
            station.config.ramp.stretch_base = 1.0;
            station.config.ramp.stretch_cap = 1.0;
        }

        advance(&mut app, 1.0);

        let mut orders = app
            .world_mut()
            .query::<(Entity, &crate::order_intake::PendingOrder)>();
        let (entity, reagent) = orders
            .single(app.world())
            .map(|(entity, order)| (entity, order.order.reagent))
            .unwrap();
        let world = app.world();
        assert!(world
            .resource::<Knowledge>()
            .available_reagents(world.resource::<ChemDb>())
            .contains(&reagent));
        assert!(world.get::<DevelopmentOrder>(entity).is_none());
    }

    #[test]
    fn later_development_requests_are_optional_longer_and_cannot_stack() {
        let mut app = generate_orders_app();
        {
            let mut station = app.world_mut().resource_mut::<StationData>();
            station.config.ramp.orders_per_tier = 100;
            station.config.ramp.stretch_base = 1.0;
            station.config.ramp.stretch_cap = 1.0;
            station.config.ramp.stretch_after_successes = 5;
            station.config.ramp.stretch_patience_scale = 1.5;
        }
        {
            let mut shift = app.world_mut().resource_mut::<Shift>();
            shift.succeeded = 5;
            shift.requisition.patience_bonus_orders = 1;
        }

        advance(&mut app, 1.0);

        let mut orders = app
            .world_mut()
            .query::<(Entity, &crate::order_intake::PendingOrder)>();
        let (development, order) = orders.single(app.world()).unwrap();
        assert!(app.world().get::<DevelopmentOrder>(development).is_some());
        assert_eq!(order.order.patience, PINNED_PATIENCE * 1.5);
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .requisition
                .patience_bonus_orders,
            1,
            "optional work must not consume a bonus bought for required work"
        );

        // Make the normal clock due again while the first opportunity is live.
        app.world_mut().resource_mut::<OrderSpawner>().timer =
            Timer::from_seconds(0.0, TimerMode::Once);
        advance(&mut app, 1.0);

        let mut query = app.world_mut().query::<&DevelopmentOrder>();
        assert_eq!(query.iter(app.world()).count(), 1);
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .requisition
                .patience_bonus_orders,
            0,
            "the required order behind it should consume the saved bonus"
        );
    }

    // -- content guardrails ----------------------------------------------

    fn station_orders() -> OrderConfig {
        ron::from_str(include_str!("../../assets/data/station.orders.ron"))
            .expect("station.orders.ron should parse")
    }

    #[test]
    fn personal_glassware_packs_have_positive_cost_and_composition() {
        // A typo here would sell a free pack, or one that arrives empty —
        // mirrors `produce::every_pack_item_names_a_real_produce`'s guard.
        let config = station_orders();
        let mut ids = std::collections::HashSet::new();
        assert!(
            !config.supply.personal_packs.is_empty(),
            "station.orders.ron sells no personal glassware packs"
        );
        for pack in &config.supply.personal_packs {
            assert!(!pack.id.is_empty(), "a personal pack has no id");
            assert!(
                ids.insert(pack.id.clone()),
                "duplicate personal pack id '{}'",
                pack.id
            );
            assert!(pack.cost > 0, "pack '{}' costs nothing", pack.id);
            assert!(
                pack.beakers + pack.large > 0,
                "pack '{}' contains nothing",
                pack.id
            );
        }
    }

    #[test]
    fn every_legitimate_requests_reference_reagent_has_a_category() {
        // A request with no category could never be gated into `in_reach`/
        // `just_beyond`, nor shown a want-phrase — it would be offered as an
        // order and then be impossible to display or fulfil.
        let db = db();
        let config = station_orders();
        for request in &config.requests {
            let reagent = db
                .reagents
                .id_of(&request.reagent)
                .unwrap_or_else(|| panic!("'{}' names no real reagent", request.reagent));
            assert!(
                reference_category(&db, reagent).is_some(),
                "'{}' has no category at all",
                request.reagent
            );
        }
    }

    #[test]
    fn every_legitimate_requests_category_is_orderable() {
        // Poisons/Pyrotechnics/Precursors/Utility/Illicit are never something
        // legitimate crew ask for — Illicit specifically is the antagonist
        // thread's exclusive domain.
        let db = db();
        let config = station_orders();
        for request in &config.requests {
            let reagent = db.reagents.id_of(&request.reagent).unwrap();
            let cat = reference_category(&db, reagent).unwrap();
            assert!(
                request.exact || cat.is_legitimately_orderable(),
                "'{}' resolves to {:?}, which nobody legitimately orders",
                request.reagent,
                cat
            );
        }
    }

    fn painkiller_order(db: &ChemDb) -> Order {
        Order {
            reagent: db.reagent("bicaridine"),
            specific: false,
            minimum_purity: 0.0,
            amount: Units::whole(20),
            plea: "Something for the pain".to_string(),
            patience: 60.0,
            waited: 0.0,
        }
    }

    fn marked(text: &str) -> crate::labels::Label {
        crate::labels::Label(text.to_string())
    }

    #[test]
    fn a_label_only_claims_something_when_it_names_a_real_chemical() {
        let db = db();
        assert_eq!(
            claimed_reagent(&db, Some(&marked("Bicaridine"))),
            Some(db.reagent("bicaridine")),
            "a bottle marked with a real chemical's name claims to be it"
        );
        // Case and whitespace are the player typing, not a different claim.
        assert_eq!(
            claimed_reagent(&db, Some(&marked("  bicaridine  "))),
            Some(db.reagent("bicaridine"))
        );
        // Free text names nothing, so it deceives nobody. Someone handed a
        // bottle marked "Painkiller" has not been told what is in it.
        assert_eq!(claimed_reagent(&db, Some(&marked("Painkiller"))), None);
        assert_eq!(claimed_reagent(&db, Some(&marked("  "))), None);
        assert_eq!(claimed_reagent(&db, None), None);
    }

    #[test]
    fn the_relationship_is_what_lets_you_lie_to_them() {
        // The trust ladder, and the reason `Shift::npc_standing` — hidden
        // since it was written — suddenly matters to the player.
        assert_eq!(trust_in_a_label(20, false), 1.0, "a friend takes your word");
        assert_eq!(trust_in_a_label(TRUSTS_YOU, false), 1.0);
        assert_eq!(
            trust_in_a_label(20, true),
            0.0,
            "estrangement outranks any amount of standing"
        );
        assert_eq!(
            trust_in_a_label(crate::estrangement::RECONCILED_AT, false),
            0.0,
            "someone you have burned reads the bottle"
        );
        assert_eq!(trust_in_a_label(-100, false), 0.0);
        // In between, it really is in between — neither a certainty nor a
        // coin that always lands the same way.
        let middling = trust_in_a_label(0, false);
        assert!(middling > 0.0 && middling < 1.0, "got {middling}");
    }

    #[test]
    fn an_honest_delivery_is_never_a_lie_however_it_is_labelled() {
        // The honest path must not touch any of this. A chemist who made the
        // real thing and wrote the real name on it — or wrote nothing, or
        // wrote something silly — is not deceiving anyone, and no roll should
        // ever be able to refuse them.
        let db = db();
        let order = painkiller_order(&db);
        let honest = solution_of(&db, &[("bicaridine", 20)]);

        for label in [None, Some(&marked("Bicaridine")), Some(&marked("lunch"))] {
            for roll in [0.0, 0.5, 1.0] {
                assert!(
                    !caught_lying(
                        &db,
                        &order,
                        OrderKind::Normal,
                        &honest,
                        label,
                        -100,
                        true,
                        roll
                    ),
                    "an honest delivery was refused as a lie"
                );
            }
            assert!(!believed_a_lie(
                &db,
                &order,
                OrderKind::Normal,
                &honest,
                label
            ));
        }
    }

    #[test]
    fn meth_answering_a_stimulant_order_is_honest_and_needs_no_label() {
        // The case that started all this. Meth genuinely is a stimulant, so
        // nobody is being lied to — no claim, no roll, no refusal, and (in
        // `complete_delivery`) no incident. The price lives entirely in
        // `addiction`.
        let db = db();
        let order = Order {
            reagent: db.reagent("hyperzine"),
            ..painkiller_order(&db)
        };
        let swapped = solution_of(&db, &[("methamphetamine", 8)]);

        assert!(
            !believed_a_lie(&db, &order, OrderKind::Normal, &swapped, None),
            "meth for a stimulant order is a substitution, not a deception"
        );
        assert!(
            !caught_lying(
                &db,
                &order,
                OrderKind::Normal,
                &swapped,
                None,
                -100,
                true,
                1.0
            ),
            "there is nothing here for even an estranged crew member to catch"
        );
    }

    #[test]
    fn a_forged_label_fools_a_friend_and_never_an_enemy() {
        // Krokodil marked as the trauma medicine it is not. Whether it lands
        // is entirely the relationship.
        let db = db();
        let order = painkiller_order(&db);
        // Space drugs: purely `Illicit`, no legitimate category at all, so
        // it cannot answer a trauma order on its own merits the way krokodil
        // now can. Only the bottle says otherwise.
        let forged = solution_of(&db, &[("space_drugs", 20)]);
        let label = Some(&marked("Bicaridine"));

        assert!(
            believed_a_lie(&db, &order, OrderKind::Normal, &forged, label),
            "the bottle claims to be the medicine they asked for"
        );
        assert!(
            !caught_lying(
                &db,
                &order,
                OrderKind::Normal,
                &forged,
                label,
                20,
                false,
                0.99
            ),
            "someone who trusts you does not read the bottle"
        );
        assert!(
            caught_lying(
                &db,
                &order,
                OrderKind::Normal,
                &forged,
                label,
                crate::estrangement::ESTRANGED_BELOW,
                true,
                0.0
            ),
            "someone estranged reads it however lucky the roll"
        );
    }

    #[test]
    fn krokodil_answers_a_painkiller_order_honestly_and_that_is_the_horror() {
        // Its authored comment has always said it "heals just enough that
        // someone can tell themselves it is medicine". Now that is mechanical:
        // krokodil is `Trauma`, so a painkiller order accepts it on its own
        // merits. No label, no lie, no roll to catch — and because the
        // delivery is honest, `complete_delivery` marks it authorized and no
        // incident ever fires.
        //
        // The bill still arrives. It is `addictive`, so they come back; it
        // does real Toxin and Burn damage every dose; and it is `Illicit`, so
        // a sweep still finds it on your shelf. Nothing about that needed a
        // forged label, which is exactly what makes it the nastiest option in
        // the game.
        let db = db();
        let order = painkiller_order(&db);
        let numbing = solution_of(&db, &[("krokodil", 20)]);

        assert!(
            container_matches(&numbing, &order, OrderKind::Normal, &db),
            "krokodil is trauma treatment now, and the counter should take it"
        );
        assert!(
            !believed_a_lie(&db, &order, OrderKind::Normal, &numbing, None),
            "nobody is being deceived — it genuinely is what they asked for"
        );
        assert!(
            db.reagents
                .get(db.reagent("krokodil"))
                .effects
                .iter()
                .any(|effect| effect.is_harmful()),
            "and it is still doing them harm while it does it"
        );
    }

    #[test]
    fn a_wrong_beaker_with_nothing_written_on_it_is_a_mistake_not_a_lie() {
        // Handing over the wrong thing has always been a `Wrong` delivery and
        // stays one. Deception requires an actual claim — otherwise every
        // fumbled order would start reading as an assault.
        let db = db();
        let order = painkiller_order(&db);
        let wrong = solution_of(&db, &[("space_drugs", 20)]);

        assert!(!believed_a_lie(
            &db,
            &order,
            OrderKind::Normal,
            &wrong,
            None
        ));
        assert!(!caught_lying(
            &db,
            &order,
            OrderKind::Normal,
            &wrong,
            None,
            -100,
            true,
            1.0
        ));
        // ...and a label that names something which would not have satisfied
        // the order either is not a lie about *this* order.
        let useless = Some(&marked("Water"));
        assert!(!believed_a_lie(
            &db,
            &order,
            OrderKind::Normal,
            &wrong,
            useless
        ));
    }

    #[test]
    fn no_legitimate_request_names_an_illicit_reagent() {
        // The guarantee that lets illicit drugs lead a double life at all.
        //
        // Several of them now carry a legitimate category as well — meth
        // really is a stimulant — so that filling a stimulant order with one
        // genuinely satisfies the person who asked. That is only a *choice*
        // for as long as nobody is ever asked for one directly: orders are
        // drawn from this authored pool naming a reference reagent, never
        // generated from categories, and this is what holds that line.
        //
        // It is also what makes `addiction::ordinary_medicine_is_never_addictive`
        // safe to exempt Illicit reagents from. If a request ever named one,
        // filling an honest order could build a habit by accident, which is
        // the exact trap that test exists to prevent.
        let db = db();
        let config = station_orders();
        for request in &config.requests {
            let reagent = db.reagents.id_of(&request.reagent).unwrap();
            assert!(
                !db.reagents
                    .get(reagent)
                    .categories
                    .contains(&Category::Illicit),
                "'{}' is on the legitimate request pool but is Illicit — crew \
                 must never be asked for one directly",
                request.reagent
            );
        }
    }

    #[test]
    fn every_illicit_reagent_keeps_its_illicit_category() {
        // `security::run_sweep`'s contraband check keys off `Category::Illicit`
        // alone. Now that several of these carry a legitimate category beside
        // it, dropping the `Illicit` half while adding the other would quietly
        // legalise a drug — a raid would walk past it, and the whole risk side
        // of dealing would evaporate with nothing failing anywhere.
        //
        // Named explicitly rather than derived, the same way
        // `Department::members` hardcodes its roster: these are exactly the
        // reagents carrying a legitimate category *and* Illicit, so they are
        // the only ones where tidying the category list could plausibly drop
        // the wrong half. A derived check cannot express that — `addictive`
        // does not identify them (hyperzine is legal, orderable and habit-
        // forming), and "has a legitimate category" is the very thing under
        // test.
        let db = db();
        for key in [
            "methamphetamine",
            "bath_salts",
            "zombie_powder",
            "krokodil",
            "aranesp",
            "pump_up",
            "kronkaine",
            "fentanyl",
        ] {
            let reagent = db.reagents.get(db.reagents.id_of(key).unwrap());
            assert!(
                reagent.categories.contains(&Category::Illicit),
                "'{key}' builds a habit but is not Illicit, so a contraband \
                 sweep would walk straight past it"
            );
            assert!(
                reagent
                    .categories
                    .iter()
                    .any(|category| category.is_legitimately_orderable()),
                "'{key}' is listed here as leading a double life but no longer \
                 satisfies any legitimate order"
            );
        }
    }

    /// Panics unless `amount` of `reagent` can be handed over in *any*
    /// glassware without [`grade`] calling it an overdose.
    fn assert_askable(db: &ChemDb, reagent: &str, amount: u32, whose: &str) {
        let id = db
            .reagents
            .id_of(reagent)
            .unwrap_or_else(|| panic!("{whose} names no real reagent '{reagent}'"));
        let asked = Units::whole(amount as i32);
        assert_eq!(
            deliverable_amount(db, id, asked),
            asked,
            "{whose} asks for {asked} of {reagent}, past its overdose threshold of {:?} — \
             a pill, bottle or syringe holding exactly that grades Overdose and anything \
             smaller grades Short, so a beaker would be the only container that could \
             ever fill it",
            db.reagents.get(id).overdose
        );

        // `asked` itself being under the threshold is not enough if the
        // recipe cannot land on it exactly (see `synthesis_multiple`): a
        // chemist who cannot hit `amount` precisely has to round up to the
        // next multiple their recipe actually produces, and if that
        // overshoots the overdose threshold, no pill, bottle or syringe
        // could ever fill this order either — only rounding it down would,
        // which grades `Short` instead. Only the smallest such multiple
        // matters; anything bigger that still clears the threshold is a
        // safe overshoot, exactly as an authored amount below the ceiling
        // already is.
        if let (Some(multiple), Some(threshold)) =
            (synthesis_multiple(db, id), db.reagents.get(id).overdose)
        {
            let remainder = asked.raw() % multiple.raw();
            if remainder != 0 {
                let nearest = asked + Units::from_raw(multiple.raw() - remainder);
                assert!(
                    nearest <= threshold,
                    "{whose} asks for {asked} of {reagent}, but its recipe only ever yields \
                     multiples of {multiple} — the nearest exact, uncontaminated batch is \
                     {nearest}, past the {threshold} overdose threshold, so every dose a pill, \
                     bottle or syringe could actually be filled with either falls short of \
                     {asked} or overdoses"
                );
            }
        }
    }

    #[test]
    fn no_authored_request_asks_for_more_than_can_be_handed_over_safely() {
        // Every thread that authors its own amounts, checked in one place
        // rather than seven near-identical tests in seven modules.
        //
        // `deliverable_amount` clamps all of these at spawn, so a slip here is
        // never unfillable in play — it only means the RON no longer says what
        // the crew member will actually ask for. `station.addiction.ron` is
        // deliberately absent: its `amounts` are shared across every habit and
        // so cannot be authored against any one reagent's threshold, which is
        // exactly the case the spawn-time clamp exists for.
        let db = db();

        for request in &station_orders().requests {
            for &amount in &request.amounts {
                assert_askable(&db, &request.reagent, amount, "station.orders.ron");
            }
        }

        let antagonist: crate::antagonist::AntagonistScript =
            ron::from_str(include_str!("../../assets/data/station.antagonist.ron")).unwrap();
        for request in &antagonist.requests {
            for &amount in &request.amounts {
                assert_askable(&db, &request.reagent, amount, "station.antagonist.ron");
            }
        }

        let cult: crate::cult::CultScript =
            ron::from_str(include_str!("../../assets/data/station.cult.ron")).unwrap();
        for stage in &cult.stages {
            assert_askable(&db, &stage.reagent, stage.amount, "station.cult.ron");
        }

        let quack: crate::quack::QuackScript =
            ron::from_str(include_str!("../../assets/data/station.quack.ron")).unwrap();
        for visit in &quack.visits {
            assert_askable(&db, &visit.reagent, visit.amount, "station.quack.ron");
        }

        let obsessed: crate::obsessed::ObsessedScript =
            ron::from_str(include_str!("../../assets/data/station.obsessed.ron")).unwrap();
        for visit in &obsessed.visits {
            assert_askable(&db, &visit.reagent, visit.amount, "station.obsessed.ron");
        }

        let saboteur: crate::saboteur::SaboteurScript =
            ron::from_str(include_str!("../../assets/data/station.saboteur.ron")).unwrap();
        for visit in &saboteur.visits {
            assert_askable(&db, &visit.reagent, visit.amount, "station.saboteur.ron");
        }

        let smuggler: crate::smuggler::SmugglerScript =
            ron::from_str(include_str!("../../assets/data/station.smuggler.ron")).unwrap();
        for visit in &smuggler.visits {
            assert_askable(&db, &visit.reagent, visit.amount, "station.smuggler.ron");
        }

        let arc: crate::arc::ArcScript =
            ron::from_str(include_str!("../../assets/data/station.arc.ron")).unwrap();
        for antag in &arc.antagonists {
            for step in &antag.counter_steps {
                assert_askable(&db, &step.reagent, step.amount, "station.arc.ron");
            }
        }

        let crisis: crate::crisis::CrisisScript =
            ron::from_str(include_str!("../../assets/data/station.crisis.ron")).unwrap();
        for case in &crisis.cases {
            // Only the cure. `harm_amount` is what the casualty was dosed with
            // before they walked in, and a poisoning past the threshold is the
            // whole crisis.
            assert_askable(
                &db,
                &case.cure_reagent,
                case.cure_amount,
                "station.crisis.ron",
            );
        }
    }

    #[test]
    fn an_ask_is_clamped_to_the_dose_its_own_reagent_can_carry() {
        let db = db();
        let bicaridine = db.reagent("bicaridine");
        // Overdoses at 15u, so 40u of it is not something anyone can be handed.
        assert_eq!(
            deliverable_amount(&db, bicaridine, Units::whole(40)),
            Units::whole(15)
        );
        // Under the threshold, the authored ask stands untouched.
        assert_eq!(
            deliverable_amount(&db, bicaridine, Units::whole(10)),
            Units::whole(10)
        );
        // Nothing to clamp against: inaprovaline has no threshold at all, and
        // the big bulk asks depend on staying that way.
        let inaprovaline = db.reagent("inaprovaline");
        assert_eq!(
            deliverable_amount(&db, inaprovaline, Units::whole(45)),
            Units::whole(45)
        );
    }

    #[test]
    fn every_request_has_a_specific_plea() {
        // Drawn rarely but not never — a blank plea on that one occasion
        // reads as broken rather than quiet.
        let config = station_orders();
        for request in &config.requests {
            assert!(
                !request.specific_plea.trim().is_empty(),
                "'{}' has no specific_plea",
                request.reagent
            );
        }
    }
}
