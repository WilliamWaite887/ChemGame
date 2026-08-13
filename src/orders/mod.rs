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

use crate::body::{Body, Bloodstream};
use crate::chem_data::ChemDb;
use crate::containers::{spawn_container, Container, ContainerKind, HeldBy, InSlot};
use crate::crew::{spawn_crew_member, CrewDef, CrewMember, CrewPhase, CrewRoute};
use crate::interaction::{InteractRequested, Interactable};
use crate::knowledge::{Knowledge, RESEARCH_PER_SUCCESS};
use crate::lab::COUNTER_SPOT;
use crate::machines::{chemist_entity, slotted_container, Machine, MachineKind, TestBenchStock};
use crate::net::is_authority;
use crate::player::Chemist;
use crate::radio::{announce_request, channel_for, RadioEntry, RadioLog};
use crate::shift::{current_rules, weighted_pick, CurrentForecast};
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
        .add_server_message::<ShiftSync>(Channel::Ordered)
        .init_resource::<Shift>()
        .add_systems(Startup, start_loading)
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
                    generate_orders,
                    // Its rarer, exact-asking sibling — same sign, same
                    // queue cap, its own much slower clock.
                    generate_specific_orders,
                    // Deliberately not gated the same way: the sign stops new
                    // arrivals, not the clock on whoever is already waiting.
                    expire_orders,
                    handle_delivery,
                    handle_window_delivery,
                    leave_sample_vials,
                    broadcast_shift,
                )
                    .chain()
                    .run_if(is_authority),
                apply_shift.run_if(in_state(ClientState::Connected)),
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
}

/// How the difficulty tightens as the career goes on.
///
/// There is no shift boundary any more — crew arrive continuously — so the
/// tier that drives every field below comes from total orders resolved
/// (`shift.succeeded + shift.botched`) divided by `orders_per_tier`, computed
/// fresh wherever it's needed rather than frozen once per shift. See
/// [`current_rules`].
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
    /// How much harder a forecast leans on the requests it names. `2.0` makes a
    /// themed request three times as likely as an untagged one.
    pub forecast_boost: f64,
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
            stretch_base: 0.25,
            stretch_step: 0.02,
            stretch_cap: 0.4,
            forecast_boost: 2.0,
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
}

impl Default for SupplyDef {
    fn default() -> Self {
        SupplyDef {
            courier: "Miner Sato".to_string(),
            glassware_target: 6,
            crate_max: 4,
            large_every: 3,
            requisition_glassware_bonus: 2,
        }
    }
}

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

    commands.insert_resource(OrderSpawner {
        timer: Timer::from_seconds(config.first_order_delay, TimerMode::Once),
    });
    // The first specific ask is armed on the same ramp-scaled cadence its
    // own re-arm uses — `Shift` is already at its tier-0 default here, so
    // (unlike the antagonist thread's very first visit) this needs no
    // separate flat constant.
    let rules = current_rules(&config, &shift);
    let mut rng = rand::rng();
    let first_specific_gap = rng.random_range(rules.gap_seconds.0..=rules.gap_seconds.1)
        * rng.random_range(config.specific_gap_multiplier.0..=config.specific_gap_multiplier.1);
    commands.insert_resource(SpecificOrderSpawner {
        timer: Timer::from_seconds(first_specific_gap, TimerMode::Once),
    });
    commands.insert_resource(StationData {
        crew: crew.0,
        config,
    });
    commands.remove_resource::<PendingStationData>();
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
    /// doc comment). Always `false` on an `IllicitOrder` — that thread's
    /// exactness comes from the marker, not this field, and the two never
    /// stack.
    pub specific: bool,
    pub amount: Units,
    pub plea: String,
    /// Seconds before a waiting crew member gives up and leaves.
    pub patience: f32,
    /// Seconds actually waited so far. Drives [`reputation_delta`] — how much
    /// a resolution is worth, not whether one happens.
    pub waited: f32,
}

impl Order {
    /// Seconds left before patience runs out, for the HUD countdown.
    pub fn remaining(&self) -> f32 {
        (self.patience - self.waited).max(0.0)
    }
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
/// Follows [`CrisisOrder`], not [`IllicitOrder`]: a counter-track request is
/// public by design (the whole point is that the departments have worked out
/// what they need and are asking you for it), so it is replicated and free to
/// be queried anywhere.
#[derive(Component, Serialize, Deserialize)]
pub struct CounterOrder;

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
}

impl OrderKind {
    /// Reads the kind off the marker components an order entity carries.
    ///
    /// The markers are mutually exclusive by construction — each is inserted
    /// by exactly one spawner — so the order of these checks only decides what
    /// a content bug would degrade to, never ordinary behaviour.
    pub fn of(illicit: bool, crisis: bool, counter: bool) -> OrderKind {
        match (illicit, crisis, counter) {
            (true, _, _) => OrderKind::Illicit,
            (_, true, _) => OrderKind::Crisis,
            (_, _, true) => OrderKind::Counter,
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

    pub fn is_counter(self) -> bool {
        matches!(self, OrderKind::Counter)
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
}

/// A station department whose standing rises and falls with how you treat its
/// crew.
///
/// Mirrors the five roles `station.crew.ron` actually writes — the same
/// vocabulary `radio::channel_for` already matches, for the same reason:
/// departments are content, not architecture.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Department {
    Medical,
    Security,
    Engineering,
    Cargo,
    Service,
}

impl Department {
    pub const ALL: [Department; 5] = [
        Department::Medical,
        Department::Security,
        Department::Engineering,
        Department::Cargo,
        Department::Service,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Department::Medical => "Medical",
            Department::Security => "Security",
            Department::Engineering => "Engineering",
            Department::Cargo => "Cargo",
            Department::Service => "Service",
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
            Department::Service => {
                "Wants the bar and the kitchen kept stocked, not complaining."
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
            _ => None,
        }
    }
}

/// Supplies bought against a department's standing.
///
/// Only glassware banks anything: it still has to be carried in by the
/// courier, so a purchase lands at the next restock pass rather than
/// instantly. Every other requisition kind applies the moment it is bought.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requisition {
    /// Extra glassware on top of the standing target, banked until the next
    /// restock check consumes it.
    pub glassware: usize,
}

#[derive(Resource, Clone, Serialize, Deserialize)]
pub struct Shift {
    pub succeeded: u32,
    pub botched: u32,
    pub department_standing: HashMap<Department, i32>,
    pub requisition: Requisition,
    /// The "not accepting requests" sign. Either chemist can flip it; while
    /// it's down, nobody new walks in — but whoever is already at the
    /// counter keeps waiting, and their clock keeps running, exactly as if
    /// the sign were still up. It stops new traffic, not existing orders.
    pub accepting_orders: bool,
}

impl Default for Shift {
    fn default() -> Self {
        Shift {
            succeeded: 0,
            botched: 0,
            department_standing: HashMap::new(),
            requisition: Requisition::default(),
            accepting_orders: true,
        }
    }
}

impl Shift {
    pub fn adjust(&mut self, department: Department, delta: i32) {
        *self.department_standing.entry(department).or_insert(0) += delta;
    }

    pub fn standing(&self, department: Department) -> i32 {
        self.department_standing
            .get(&department)
            .copied()
            .unwrap_or(0)
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

#[allow(clippy::too_many_arguments)]
fn generate_orders(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    mut spawner: Option<ResMut<OrderSpawner>>,
    knowledge: Option<Res<Knowledge>>,
    shift: Res<Shift>,
    forecast: Option<Res<CurrentForecast>>,
    mut radio: ResMut<RadioLog>,
    active: Query<&CrewMember>,
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
    // is no shift boundary left to freeze a snapshot against.
    let rules = current_rules(&station.config, &shift);
    let rules = &rules;

    if !spawner.timer.tick(time.delta()).just_finished() {
        return;
    }

    let waiting = active.iter().count();
    let mut rng = rand::rng();
    let gap = rng.random_range(rules.gap_seconds.0..=rules.gap_seconds.1);
    spawner.timer = Timer::from_seconds(gap, TimerMode::Once);

    if waiting >= rules.max_active {
        return;
    }

    let Some(crew_def) = station.crew.choose(&mut rng) else {
        return;
    };

    // Only ask for things the chemist could plausibly produce. Without this
    // the very first order can be a three-step chain the player has no way of
    // knowing, which reads as the game being broken rather than hard.
    let makeable = knowledge.available_reagents(&db);
    let stretch: HashSet<ReagentId> = knowledge
        .frontier(&db)
        .into_iter()
        .flat_map(|id| db.reactions.get(id).product_ids())
        .collect();

    // A request is reachable the moment *any* member of its category is
    // makeable — not only its specific reference reagent — since lenient
    // grading means the player could satisfy it right now with a different
    // chemical they already know.
    let request_category = |request: &RequestDef| {
        db.reagents
            .id_of(&request.reagent)
            .and_then(|id| reference_category(&db, id))
    };
    let in_reach: Vec<&RequestDef> = station
        .config
        .requests
        .iter()
        .filter(|request| {
            request_category(request).is_some_and(|cat| category_has_member_in(&db, cat, &makeable))
        })
        .collect();
    let just_beyond: Vec<&RequestDef> = station
        .config
        .requests
        .iter()
        .filter(|request| {
            request_category(request).is_some_and(|cat| category_has_member_in(&db, cat, &stretch))
        })
        .collect();

    let pool = if !just_beyond.is_empty() && rng.random_bool(rules.stretch_chance) {
        &just_beyond
    } else if !in_reach.is_empty() {
        &in_reach
    } else {
        &just_beyond
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
    let Some(&amount) = request.amounts.choose(&mut rng) else {
        return;
    };

    let patience = rng.random_range(rules.patience_seconds.0..=rules.patience_seconds.1);
    let crew = spawn_crew_member(&mut commands, crew_def, waiting as f32 * 0.95);

    // An ordinary order spawned here always describes what it needs, not
    // the exact chemical — naming one outright is `generate_specific_orders`'
    // job now, on its own separate clock. Falls back to the reagent's own
    // name only if it somehow has no category (a content bug guarded by
    // `every_legitimate_requests_reference_reagent_has_a_category`).
    let want_label = reference_category(&db, reagent)
        .map(|cat| cat.want_phrase().to_string())
        .unwrap_or_else(|| db.reagents.get(reagent).name.clone());
    commands.entity(crew).insert((
        Order {
            reagent,
            specific: false,
            amount: Units::whole(amount as i32),
            plea: request.plea.clone(),
            patience,
            waited: 0.0,
        },
        Interactable::new(format!(
            "{} — hand over {}u {}",
            crew_def.name, amount, want_label
        )),
    ));

    // The request goes out over the radio too, so the feed carries both halves
    // of the conversation rather than only the verdict.
    announce_request(&mut radio, &crew_def.name, &crew_def.role, &request.plea);

    info!(
        "{} ({}) wants {}u {}",
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
///   makeable), never `just_beyond`. A lenient stretch order can fall back to
///   a sibling category member if the reference reagent turns out to be the
///   unreached one; naming one exact reagent outright has no such fallback,
///   so offering one the chemist cannot yet make at all would be a
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
    mut radio: ResMut<RadioLog>,
    active: Query<&CrewMember>,
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

    let rules = current_rules(&station.config, &shift);
    let rules = &rules;

    if !spawner.timer.tick(time.delta()).just_finished() {
        return;
    }

    let waiting = active.iter().count();
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
    if waiting >= rules.max_active {
        return;
    }

    let Some(crew_def) = station.crew.choose(&mut rng) else {
        return;
    };

    let makeable = knowledge.available_reagents(&db);
    let in_reach: Vec<&RequestDef> = station
        .config
        .requests
        .iter()
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
    let Some(request) =
        weighted_pick(&in_reach, themes, rules.forecast_boost, rng.random::<f64>())
    else {
        return;
    };
    let Some(reagent) = db.reagents.id_of(&request.reagent) else {
        warn!("order requests unknown reagent '{}'", request.reagent);
        return;
    };
    let Some(&amount) = request.amounts.choose(&mut rng) else {
        return;
    };

    let patience = rng.random_range(rules.patience_seconds.0..=rules.patience_seconds.1);
    let crew = spawn_crew_member(&mut commands, crew_def, waiting as f32 * 0.95);

    let reagent_name = db.reagents.get(reagent).name.clone();
    commands.entity(crew).insert((
        Order {
            reagent,
            specific: true,
            amount: Units::whole(amount as i32),
            plea: request.specific_plea.clone(),
            patience,
            waited: 0.0,
        },
        Interactable::new(format!(
            "{} — hand over {}u {}",
            crew_def.name, amount, reagent_name
        )),
    ));

    announce_request(&mut radio, &crew_def.name, &crew_def.role, &request.specific_plea);

    info!(
        "{} ({}) specifically wants {}u {}",
        crew_def.name, crew_def.role, amount, reagent_name
    );
}

#[allow(clippy::type_complexity)]
fn expire_orders(
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
        Has<CounterOrder>,
    )>,
) {
    // Deliberately *not* gated on `accepting_orders`. The sign stops new
    // people walking in; it is not a pause button, so whoever is already at
    // the counter keeps waiting — and keeps costing you — exactly as if it
    // were still up.
    let dt = time.delta_secs();

    for (entity, mut order, crew, mut route, illicit, crisis, counter) in &mut orders {
        let kind = OrderKind::of(illicit, crisis, counter);
        // Patience only runs down once they have actually arrived, so a slow
        // walk in never counts against the player.
        if route.phase != CrewPhase::Waiting {
            continue;
        }
        order.waited += dt;
        if order.waited < order.patience {
            continue;
        }

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
        });
        shift.botched += 1;
        // An abandoned illicit order is not a chaos-causing success, so it
        // always falls through to the ordinary department penalty — the same
        // shape "declining isn't specially punished" takes on the delivery
        // side, just arrived at by giving up rather than choosing to.
        // Nothing was ever delivered, so there is nothing to grade for
        // quality — `0` is inert anyway, since `reputation_delta` only ever
        // applies a potency bonus on `Success`.
        adjust_for_role(&mut shift, &crew.role, Outcome::Expired, order.waited, order.patience, 0);

        commands.entity(entity).remove::<Order>();
        route.leave();
    }
}

/// Applies a resolution's reputation delta to the department the crew
/// member's role names, warning rather than panicking if it names none — a
/// content bug in `station.crew.ron` should not take the shift down with it.
fn adjust_for_role(
    shift: &mut Shift,
    role: &str,
    outcome: Outcome,
    waited: f32,
    patience: f32,
    potency: u32,
) {
    let Some(department) = Department::from_role(role) else {
        warn!("order resolved for unrecognised department role '{role}'");
        return;
    };
    shift.adjust(department, reputation_delta(outcome, waited, patience, potency));
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn handle_delivery(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut requests: MessageReader<FromClient<InteractRequested>>,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
    mut resolved: MessageWriter<OrderResolved>,
    mut crew: Query<(
        &CrewMember,
        &Order,
        &mut CrewRoute,
        Has<IllicitOrder>,
        Has<CrisisOrder>,
        Has<CounterOrder>,
    )>,
    mut bodies: Query<(&mut Body, &mut Bloodstream)>,
    containers: Query<(Entity, &Container, &HeldBy, Has<TestBenchStock>)>,
    chemists: Query<(Entity, &Chemist)>,
    mut knowledge: ResMut<Knowledge>,
) {
    for request in requests.read() {
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Ok((member, order, mut route, illicit, crisis, counter)) =
            crew.get_mut(request.target)
        else {
            continue;
        };
        let body = bodies
            .get_mut(request.target)
            .ok()
            .map(|(b, blood)| (b.into_inner(), blood.into_inner()));
        let Some((container_entity, container, _, test_stock)) = containers
            .iter()
            .find(|(_, _, holder, _)| holder.0 == player)
        else {
            continue;
        };

        // Practice stock is refused at the counter rather than graded. The
        // order stays open, so this is a correction rather than a punishment.
        if test_stock {
            radio.push(RadioEntry {
                channel: channel_for(&member.role),
                text: format!(
                    "{}: that's off the test bench. I need the real thing.",
                    member.name
                ),
                good: false,
            });
            continue;
        }

        complete_delivery(
            &mut commands,
            &db,
            &mut shift,
            &mut resolved,
            &mut knowledge,
            Handover {
                crew: request.target,
                member,
                order,
                route: &mut route,
                container_entity,
                container,
                kind: OrderKind::of(illicit, crisis, counter),
                body,
            },
        );
    }
}

/// One crew member, one order, one container being handed across.
struct Handover<'a> {
    crew: Entity,
    member: &'a CrewMember,
    order: &'a Order,
    route: &'a mut CrewRoute,
    container_entity: Entity,
    container: &'a Container,
    /// Which thread owns this order — see [`OrderResolved::kind`].
    kind: OrderKind,
    /// The recipient's body, so `complete_delivery` can route what was
    /// actually handed over into them — every crew member has had one since
    /// M12. `Option` because this struct already follows the "the caller
    /// fetched it, not this function" shape for everything else; a missing
    /// body just means the dose is never felt rather than a panic.
    body: Option<(&'a mut Body, &'a mut Bloodstream)>,
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
    knowledge: &mut Knowledge,
    handover: Handover,
) -> Outcome {
    let Handover {
        crew,
        member,
        order,
        route,
        container_entity,
        container,
        kind,
        body,
    } = handover;

    let (outcome, matched) = grade(
        wanted_for(order, kind, db),
        order.amount,
        &container.solution,
        container.kind,
        db,
    );

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
    resolved.write(OrderResolved {
        name: member.name.clone(),
        role: member.role.clone(),
        reagent: reported_reagent,
        category,
        outcome,
        kind,
    });

    if outcome.is_good() {
        shift.succeeded += 1;
        knowledge.award_research(RESEARCH_PER_SUCCESS);
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
        let potency = matched.map(|id| db.reagents.get(id).potency).unwrap_or(0);
        adjust_for_role(shift, &member.role, outcome, order.waited, order.patience, potency);
    }

    info!(
        "{} took {} — {:?}",
        member.name,
        container.kind.label(),
        outcome
    );

    // They actually drink what you handed them — whether that was the
    // medicine they asked for or, through carelessness or malice, something
    // else entirely. Deliberately independent of `outcome`: a `Wrong`
    // delivery still gets swallowed, which is exactly what makes handing
    // over the wrong beaker a real mistake and not just a graded number.
    if let Some((recipient_body, recipient_blood)) = body {
        let mut dose = container.solution.clone();
        if dose.total_volume().is_positive() {
            recipient_blood
                .0
                .receive(&mut dose, Route::Ingested, &mut recipient_body.0, db);
        }
    }

    // They walk off with the glassware. Getting it back is someone else's
    // problem, which is also true in the original.
    commands.entity(container_entity).despawn();
    commands
        .entity(crew)
        .remove::<Order>()
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
        .remove::<CounterOrder>();
    route.leave();
    outcome
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
        Some(cat) => contents
            .iter()
            .any(|(id, amount)| amount.is_positive() && db.reagents.get(id).categories.contains(&cat)),
        None => contents.volume_of(order.reagent).is_positive(),
    }
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
    waiting: impl Iterator<Item = (Entity, &'a Order, &'a CrewRoute, OrderKind)>,
    db: &ChemDb,
) -> Option<Entity> {
    waiting
        .filter(|(_, _, route, _)| route.phase == CrewPhase::Waiting)
        .filter(|(_, order, _, kind)| container_matches(contents, order, *kind, db))
        .min_by(|a, b| a.1.remaining().total_cmp(&b.1.remaining()))
        .map(|(entity, _, _, _)| entity)
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
    mut knowledge: ResMut<Knowledge>,
    windows: Query<(Entity, &Machine)>,
    slotted: Query<(Entity, &InSlot)>,
    containers: Query<(&Container, Has<TestBenchStock>)>,
    mut crew: Query<(
        Entity,
        &CrewMember,
        &Order,
        &mut CrewRoute,
        Has<IllicitOrder>,
        Has<CrisisOrder>,
        Has<CounterOrder>,
    )>,
    mut bodies: Query<(&mut Body, &mut Bloodstream)>,
) {
    for (window, machine) in &windows {
        if machine.kind != MachineKind::DeliveryWindow {
            continue;
        }
        let Some(container_entity) = slotted_container(window, &slotted) else {
            continue;
        };
        let Ok((container, test_stock)) = containers.get(container_entity) else {
            continue;
        };
        // Practice stock is refused by every route, or the test bench would
        // just be a dispenser that costs nothing. Refused quietly here rather
        // than over the radio — the panel explains it, and a line every frame
        // would bury the feed.
        if test_stock {
            continue;
        }

        let candidates = crew.iter().map(|(entity, _, order, route, illicit, crisis, counter)| {
            (entity, order, route, OrderKind::of(illicit, crisis, counter))
        });
        let Some(recipient) = window_recipient(&container.solution, candidates, &db) else {
            continue;
        };

        let Ok((crew_entity, member, order, mut route, illicit, crisis, counter)) =
            crew.get_mut(recipient)
        else {
            continue;
        };
        let body = bodies
            .get_mut(crew_entity)
            .ok()
            .map(|(b, blood)| (b.into_inner(), blood.into_inner()));
        complete_delivery(
            &mut commands,
            &db,
            &mut shift,
            &mut resolved,
            &mut knowledge,
            Handover {
                crew: crew_entity,
                member,
                order,
                route: &mut route,
                container_entity,
                container,
                kind: OrderKind::of(illicit, crisis, counter),
                body,
            },
        );
    }
}

/// Grateful crew occasionally leave a sample of something else they use.
///
/// Run through the analyzer it yields a recipe, which is the route into
/// anything the player cannot yet stumble onto by mixing. Tying it to clean
/// deliveries means the game opens up in response to doing the job well.
#[allow(clippy::too_many_arguments)]
fn leave_sample_vials(
    mut commands: Commands,
    db: Res<ChemDb>,
    knowledge: Res<Knowledge>,
    mut resolved: MessageReader<OrderResolved>,
    mut radio: ResMut<RadioLog>,
) {
    let mut rng = rand::rng();

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

        let (_, height) = ContainerKind::Bottle.dimensions();
        let vial = spawn_container(
            &mut commands,
            ContainerKind::Bottle,
            Vec3::new(
                COUNTER_SPOT.x,
                crate::lab::COUNTER_TOP + height * 0.5,
                crate::lab::COUNTER_DROP_Z,
            ),
        );
        let amount = ContainerKind::Bottle.capacity();
        commands.queue(move |world: &mut World| {
            if let Some(mut container) = world.get_mut::<Container>(vial) {
                let _ = container.solution.add(product, amount);
            }
        });

        let name = db.reagents.get(product).name.clone();
        radio.push(RadioEntry {
            channel: channel_for(&report.role),
            text: format!(
                "{}: left you a sample of {} on the counter. Might be useful.",
                report.name, name
            ),
            good: true,
        });
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

    // -- delivery window ----------------------------------------------------

    /// Just enough world to run the window: no renderer, no crew walking.
    fn window_app() -> App {
        let data = data();
        let mut app = App::new();
        app.insert_resource(Knowledge::new(&data))
            .insert_resource(ChemDb(data))
            .init_resource::<Shift>()
            .add_message::<OrderResolved>()
            .add_systems(Update, handle_window_delivery);
        app
    }

    fn reagent_id(app: &App, key: &str) -> ReagentId {
        app.world().resource::<ChemDb>().reagent(key)
    }

    /// A delivery window with `contents` sitting in its slot.
    fn window_with(app: &mut App, contents: &[(&str, i32)], practice: bool) -> (Entity, Entity) {
        let data = app.world().resource::<ChemDb>().0.clone();
        let window = app
            .world_mut()
            .spawn(Machine::new(MachineKind::DeliveryWindow))
            .id();

        let mut container = Container::new(ContainerKind::LargeBeaker);
        for (key, amount) in contents {
            let _ = container
                .solution
                .add(data.reagent(key), Units::whole(*amount));
        }
        let mut entity = app.world_mut().spawn((container, InSlot(window)));
        if practice {
            entity.insert(TestBenchStock);
        }
        (window, entity.id())
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
        let reagent = reagent_id(app, wants);
        let mut route = CrewRoute::arrival(0.0);
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
        let (_window, _beaker) = window_with(&mut app, &[("dylovene", 30)], false);
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
        let (_, beaker) = window_with(&mut app, &[("dylovene", 30)], false);
        let crew = waiting_crew(&mut app, "Dr. Vance", "dylovene", 30, 60.0, true);
        app.world_mut().entity_mut(crew).insert(IllicitOrder);

        app.update();

        assert_eq!(
            outcomes(&app),
            vec![("Dr. Vance".to_string(), Outcome::Success)]
        );
        assert!(app.world().get_entity(beaker).is_err());
        assert_eq!(
            app.world().resource::<Shift>().standing(Department::Medical),
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
            app.world().resource::<Shift>().standing(Department::Medical)
        };
        let legitimate_delta = {
            let mut app = expiry_app();
            waiting_crew(&mut app, "Dr. Vance", "kelotane", 20, 1.0, true);
            advance(&mut app, 1.5);
            app.world().resource::<Shift>().standing(Department::Medical)
        };
        assert_eq!(
            illicit_delta, legitimate_delta,
            "a declined illicit order must cost exactly what an ordinary one does"
        );
        assert_ne!(illicit_delta, 0, "an expired order should have cost something");
    }

    #[test]
    fn the_window_hands_over_to_whoever_is_waiting_for_it() {
        let mut app = window_app();
        let (_, beaker) = window_with(&mut app, &[("dylovene", 30)], false);
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
    fn a_delivered_solution_actually_lands_on_the_recipient() {
        // M12: a delivery is not just graded, it is drunk. Handing over
        // something should show up in the recipient's own Bloodstream, not
        // just the order's outcome. Ingested lands in the stomach first
        // (same as a chemist's own sip, `body::tests::drinking_takes_a_
        // mouthful_out_of_the_beaker_and_into_the_stomach`) — turning that
        // into a felt `Drunk` status is `run_metabolism`'s job, already
        // covered in `body::mod::tests`, not this module's to re-prove.
        let mut app = window_app();
        window_with(&mut app, &[("hooch", 10)], false);
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
    fn the_window_serves_the_most_urgent_matching_order() {
        // A batch that could satisfy two people goes to whoever is closest to
        // walking out, matching how the order queue itself is sorted.
        let mut app = window_app();
        window_with(&mut app, &[("dylovene", 30)], false);
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
        let (_, beaker) = window_with(&mut app, &[("dylovene", 30)], false);
        waiting_crew(&mut app, "En route", "dylovene", 30, 60.0, false);

        app.update();

        assert!(outcomes(&app).is_empty());
        assert!(
            app.world().get_entity(beaker).is_ok(),
            "the batch stays in the window until someone is there to take it"
        );
    }

    #[test]
    fn the_window_refuses_test_bench_stock() {
        // Otherwise the bench is just a dispenser that costs nothing, and the
        // window is the way round the refusal at the counter.
        let mut app = window_app();
        let (_, beaker) = window_with(&mut app, &[("dylovene", 30)], true);
        waiting_crew(&mut app, "Dr. Vance", "dylovene", 30, 60.0, true);

        app.update();

        assert!(outcomes(&app).is_empty());
        assert!(app.world().get_entity(beaker).is_ok());
    }

    #[test]
    fn the_window_leaves_a_batch_nobody_asked_for() {
        let mut app = window_app();
        let (_, beaker) = window_with(&mut app, &[("kelotane", 30)], false);
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
        window_with(&mut app, &[("dylovene", 30), ("plant_fibre", 20)], false);
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
        assert_eq!(matched, None, "nothing should be named back for an unmatched category order");
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

    // -- specific (exact-asking) orders -----------------------------------

    fn specific_order(db: &ChemDb, reagent: &str, amount: i32) -> Order {
        Order {
            reagent: db.reagent(reagent),
            specific: true,
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

    // -- content guardrails ----------------------------------------------

    fn station_orders() -> OrderConfig {
        ron::from_str(include_str!("../../assets/data/station.orders.ron"))
            .expect("station.orders.ron should parse")
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
                cat.is_legitimately_orderable(),
                "'{}' resolves to {:?}, which nobody legitimately orders",
                request.reagent,
                cat
            );
        }
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
