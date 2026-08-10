//! Requests from the crew, and grading what you hand back.
//!
//! Grading lives in [`grade`], a pure function with no ECS involvement, so the
//! rules that decide whether a shift went well can be tested exhaustively.

use std::collections::HashSet;

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use bevy_replicon::prelude::*;
use chem_sim::{ReagentId, Solution, Units};
use rand::prelude::*;
use serde::{Deserialize, Serialize};

use crate::chem_data::ChemDb;
use crate::containers::{
    spawn_container, Container, ContainerAssets, ContainerKind, HeldBy, InSlot,
};
use crate::crew::{spawn_crew_member, CrewAssets, CrewDef, CrewMember, CrewPhase, CrewRoute};
use crate::interaction::{InteractRequested, Interactable};
use crate::knowledge::{Knowledge, RESEARCH_PER_SUCCESS};
use crate::lab::COUNTER_SPOT;
use crate::machines::{chemist_entity, slotted_container, Machine, MachineKind, TestBenchStock};
use crate::player::Chemist;
use crate::radio::{announce_request, channel_for, RadioEntry, RadioLog};
use crate::shift::{count_resolved_orders, weighted_pick, ShiftClock, ShiftPhase};
use crate::AppState;

/// How often a clean delivery earns a sample vial of something unfamiliar.
const SAMPLE_VIAL_CHANCE: f64 = 0.35;

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
                (
                    promote_station_data,
                    // Gated: nobody new walks in during prep or the debrief.
                    // That window is the whole point — it is when the bench,
                    // the grinder and the book are free to actually use.
                    generate_orders.run_if(in_state(ShiftPhase::Service)),
                    // Deliberately *not* gated. Gating these would freeze an
                    // in-flight order's patience mid-countdown and strand the
                    // crew member at the counter with no way to resolve.
                    expire_orders,
                    handle_delivery,
                    handle_window_delivery,
                    leave_sample_vials,
                    // After all three resolution paths, so it sees this
                    // frame's resolutions rather than next frame's.
                    count_resolved_orders,
                    broadcast_shift,
                )
                    .chain()
                    .run_if(in_state(ClientState::Disconnected)),
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
    /// How long the prep and debrief windows run for.
    #[serde(default)]
    pub windows: WindowDef,
    /// How the numbers above tighten shift by shift.
    #[serde(default)]
    pub ramp: RampDef,
    /// What cargo keeps the lab stocked with.
    #[serde(default)]
    pub supply: SupplyDef,
    /// What the station expects of a shift, briefed during prep.
    #[serde(default)]
    pub forecasts: Vec<ForecastDef>,
    pub requests: Vec<RequestDef>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RequestDef {
    pub reagent: String,
    pub amounts: Vec<u32>,
    pub plea: String,
    /// What kind of shift this request belongs to, so a forecast can lean on
    /// it. An untagged request can never be forecast — see
    /// `every_request_carries_a_theme`.
    #[serde(default)]
    pub themes: Vec<String>,
}

/// Lengths of the two windows that are not the shift itself.
#[derive(Clone, Debug, Deserialize)]
pub struct WindowDef {
    pub prep_seconds: f32,
    pub debrief_seconds: f32,
    /// The first prep of a session has no clock, so a new chemist can take the
    /// lab apart before anyone starts asking for anything.
    pub first_shift_untimed: bool,
}

impl Default for WindowDef {
    fn default() -> Self {
        WindowDef {
            prep_seconds: 120.0,
            debrief_seconds: 90.0,
            first_shift_untimed: true,
        }
    }
}

/// How each shift tightens on the one before it.
#[derive(Clone, Debug, Deserialize)]
pub struct RampDef {
    pub quota_base: u32,
    pub quota_per_shift: u32,
    pub quota_cap: u32,
    /// Multiplied into the gap between orders, once per shift elapsed.
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
            quota_base: 5,
            quota_per_shift: 1,
            quota_cap: 10,
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
#[derive(Component)]
pub struct Order {
    pub reagent: ReagentId,
    pub amount: Units,
    pub plea: String,
    pub timer: Timer,
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
    pub fn reputation_delta(self) -> i32 {
        match self {
            Outcome::Success => 2,
            Outcome::Short | Outcome::Impure => -1,
            Outcome::Expired => -2,
            Outcome::Overdose | Outcome::Wrong => -3,
        }
    }

    pub fn is_good(self) -> bool {
        self == Outcome::Success
    }
}

/// Emitted when an order finishes, one way or another.
///
/// Nothing reads it yet: M5's radio chatter is built on top of exactly this,
/// which is why the outcome carries the requester's name and role rather than
/// just a score delta.
#[derive(Message)]
#[allow(dead_code)]
pub struct OrderResolved {
    pub name: String,
    pub role: String,
    pub reagent: ReagentId,
    pub outcome: Outcome,
}

#[derive(Resource, Default, Clone, Serialize, Deserialize)]
pub struct Shift {
    pub succeeded: u32,
    pub botched: u32,
    pub reputation: i32,
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

/// Decides how a delivery went.
///
/// Pure and ECS-free so every branch can be tested directly. Order matters:
/// the checks run worst-first, because a pill that is both overdosed and
/// contaminated should be reported as the overdose.
pub fn grade(
    requested_reagent: ReagentId,
    requested_amount: Units,
    delivered: &Solution,
    kind: ContainerKind,
    overdose_threshold: Option<Units>,
) -> Outcome {
    let supplied = delivered.volume_of(requested_reagent);
    if !supplied.is_positive() {
        return Outcome::Wrong;
    }

    // Only single-dose forms can overdose. A beaker is bulk supply that gets
    // measured out later; a pill is swallowed whole.
    let single_dose = matches!(kind, ContainerKind::Pill | ContainerKind::Bottle);
    if single_dose {
        if let Some(threshold) = overdose_threshold {
            if supplied > threshold {
                return Outcome::Overdose;
            }
        }
    }

    if supplied < requested_amount {
        return Outcome::Short;
    }
    if delivered.len() > 1 {
        return Outcome::Impure;
    }
    Outcome::Success
}

#[allow(clippy::too_many_arguments)]
fn generate_orders(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<StationData>>,
    assets: Option<Res<CrewAssets>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut spawner: Option<ResMut<OrderSpawner>>,
    knowledge: Option<Res<Knowledge>>,
    clock: Res<ShiftClock>,
    mut radio: ResMut<RadioLog>,
    active: Query<&CrewMember>,
) {
    let Some(knowledge) = knowledge else {
        return;
    };
    let (Some(station), Some(assets), Some(spawner)) = (station, assets, spawner.as_mut()) else {
        return;
    };

    // This shift's difficulty, frozen when service opened, rather than the
    // base config — see `ShiftRules::for_shift`.
    let rules = &clock.rules;

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
    let just_beyond: Vec<&RequestDef> = station
        .config
        .requests
        .iter()
        .filter(|request| {
            db.reagents
                .id_of(&request.reagent)
                .is_some_and(|id| stretch.contains(&id))
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
    // so a shift briefed for burns still only asks for things the chemist can
    // plausibly make.
    let Some(request) = weighted_pick(
        pool,
        clock.forecast_themes(),
        rules.forecast_boost,
        rng.random::<f64>(),
    ) else {
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

    let patience =
        rng.random_range(rules.patience_seconds.0..=rules.patience_seconds.1);
    let crew = spawn_crew_member(
        &mut commands,
        &mut materials,
        &assets,
        crew_def,
        waiting as f32 * 0.95,
    );

    let reagent_name = db.reagents.get(reagent).name.clone();
    commands.entity(crew).insert((
        Order {
            reagent,
            amount: Units::whole(amount as i32),
            plea: request.plea.clone(),
            timer: Timer::from_seconds(patience, TimerMode::Once),
        },
        Interactable::new(format!(
            "{} — hand over {}u {}",
            crew_def.name, amount, reagent_name
        )),
    ));

    // The request goes out over the radio too, so the feed carries both halves
    // of the conversation rather than only the verdict.
    announce_request(&mut radio, &crew_def.name, &crew_def.role, &request.plea);

    info!(
        "{} ({}) wants {}u {}",
        crew_def.name, crew_def.role, amount, reagent_name
    );
}

fn expire_orders(
    mut commands: Commands,
    time: Res<Time>,
    mut shift: ResMut<Shift>,
    mut resolved: MessageWriter<OrderResolved>,
    mut orders: Query<(Entity, &mut Order, &CrewMember, &mut CrewRoute)>,
) {
    for (entity, mut order, crew, mut route) in &mut orders {
        // Patience only runs down once they have actually arrived, so a slow
        // walk in never counts against the player.
        if route.phase != CrewPhase::Waiting {
            continue;
        }
        if !order.timer.tick(time.delta()).just_finished() {
            continue;
        }

        resolved.write(OrderResolved {
            name: crew.name.clone(),
            role: crew.role.clone(),
            reagent: order.reagent,
            outcome: Outcome::Expired,
        });
        shift.botched += 1;
        shift.reputation += Outcome::Expired.reputation_delta();

        commands.entity(entity).remove::<Order>();
        route.leave();
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_delivery(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut requests: MessageReader<FromClient<InteractRequested>>,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
    mut resolved: MessageWriter<OrderResolved>,
    mut crew: Query<(&CrewMember, &Order, &mut CrewRoute)>,
    containers: Query<(Entity, &Container, &HeldBy, Has<TestBenchStock>)>,
    chemists: Query<(Entity, &Chemist)>,
    mut knowledge: ResMut<Knowledge>,
) {
    for request in requests.read() {
        let Some(player) = chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let Ok((member, order, mut route)) = crew.get_mut(request.target) else {
            continue;
        };
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
    } = handover;

    let outcome = grade(
        order.reagent,
        order.amount,
        &container.solution,
        container.kind,
        db.reagents.get(order.reagent).overdose,
    );

    resolved.write(OrderResolved {
        name: member.name.clone(),
        role: member.role.clone(),
        reagent: order.reagent,
        outcome,
    });

    if outcome.is_good() {
        shift.succeeded += 1;
        knowledge.award_research(RESEARCH_PER_SUCCESS);
    } else {
        shift.botched += 1;
    }
    shift.reputation += outcome.reputation_delta();

    info!(
        "{} took {} — {:?}",
        member.name,
        container.kind.label(),
        outcome
    );

    // They walk off with the glassware. Getting it back is someone else's
    // problem, which is also true in the original.
    commands.entity(container_entity).despawn();
    commands
        .entity(crew)
        .remove::<Order>()
        .remove::<Interactable>();
    route.leave();
    outcome
}

/// Which order a container in the window should go to, if any.
///
/// Pulled out of the system so the matching rule can be tested directly. The
/// rule is deliberately narrow: a container matches an order when it holds
/// *any* of the reagent asked for. Whether it holds enough, whether it is
/// clean, and whether the dose is safe are [`grade`]'s business — the window
/// picks a recipient, it does not vet the delivery.
///
/// Ties go to whoever is closest to giving up, matching the order queue's own
/// sort. A beaker that could satisfy two people should go to the one about to
/// walk out.
fn window_recipient<'a>(
    contents: &Solution,
    waiting: impl Iterator<Item = (Entity, &'a Order, &'a CrewRoute)>,
) -> Option<Entity> {
    waiting
        .filter(|(_, _, route)| route.phase == CrewPhase::Waiting)
        .filter(|(_, order, _)| contents.volume_of(order.reagent).is_positive())
        .min_by(|a, b| {
            a.1.timer
                .remaining_secs()
                .total_cmp(&b.1.timer.remaining_secs())
        })
        .map(|(entity, _, _)| entity)
}

/// Hands over whatever is sitting in the delivery window.
///
/// The window is a tray rather than a button: a container left in it goes to
/// the first crew member at the counter who asked for something it holds. That
/// means a batch can be finished and parked before its requester has even
/// walked in, which is what makes the window a post one chemist can work while
/// the other mixes.
#[allow(clippy::too_many_arguments)]
fn handle_window_delivery(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut shift: ResMut<Shift>,
    mut resolved: MessageWriter<OrderResolved>,
    mut knowledge: ResMut<Knowledge>,
    windows: Query<(Entity, &Machine)>,
    slotted: Query<(Entity, &InSlot)>,
    containers: Query<(&Container, Has<TestBenchStock>)>,
    mut crew: Query<(Entity, &CrewMember, &Order, &mut CrewRoute)>,
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

        let candidates = crew
            .iter()
            .map(|(entity, _, order, route)| (entity, order, route));
        let Some(recipient) = window_recipient(&container.solution, candidates) else {
            continue;
        };

        let Ok((crew_entity, member, order, mut route)) = crew.get_mut(recipient) else {
            continue;
        };
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
    assets: Option<Res<ContainerAssets>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut resolved: MessageReader<OrderResolved>,
    mut radio: ResMut<RadioLog>,
) {
    let Some(assets) = assets else {
        resolved.clear();
        return;
    };
    let mut rng = rand::rng();

    for report in resolved.read() {
        if report.outcome != Outcome::Success || !rng.random_bool(SAMPLE_VIAL_CHANCE) {
            continue;
        }

        let unknown: Vec<_> = db
            .reactions
            .iter()
            .filter(|reaction| !knowledge.is_known(reaction.id))
            .collect();
        let Some(recipe) = unknown.choose(&mut rng) else {
            continue;
        };
        let Some(&(product, _)) = recipe.products.first() else {
            continue;
        };

        let vial = spawn_container(
            &mut commands,
            &mut meshes,
            &mut materials,
            &assets,
            ContainerKind::Bottle,
            Vec3::new(COUNTER_SPOT.x, 1.22, 2.9),
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
                    amount: Units::whole(amount),
                    plea: String::new(),
                    timer: Timer::from_seconds(patience, TimerMode::Once),
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
    fn the_window_hands_over_to_whoever_is_waiting_for_it() {
        let mut app = window_app();
        let (_, beaker) = window_with(&mut app, &[("dylovene", 30)], false);
        let crew = waiting_crew(&mut app, "Dr. Vance", "dylovene", 30, 60.0, true);

        app.update();

        assert_eq!(outcomes(&app), vec![("Dr. Vance".to_string(), Outcome::Success)]);
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

        assert_eq!(outcomes(&app), vec![("Dr. Vance".to_string(), Outcome::Impure)]);
        assert_eq!(app.world().resource::<Shift>().botched, 1);
    }

    #[test]
    fn exact_pure_delivery_succeeds() {
        let data = data();
        let bicaridine = data.reagent("bicaridine");
        let delivered = solution_of(&data, &[("bicaridine", 30)]);

        let outcome = grade(
            bicaridine,
            Units::whole(30),
            &delivered,
            ContainerKind::Beaker,
            Some(Units::whole(15)),
        );

        assert_eq!(outcome, Outcome::Success);
    }

    #[test]
    fn contamination_is_caught_even_when_the_amount_is_right() {
        // This is the common failure: a sloppy mix leaves leftovers that keep
        // reacting, so the beaker holds the right medicine plus something else.
        let data = data();
        let delivered = solution_of(&data, &[("bicaridine", 30), ("inaprovaline", 5)]);

        let outcome = grade(
            data.reagent("bicaridine"),
            Units::whole(30),
            &delivered,
            ContainerKind::Beaker,
            Some(Units::whole(15)),
        );

        assert_eq!(outcome, Outcome::Impure);
    }

    #[test]
    fn a_beaker_is_bulk_supply_and_cannot_overdose() {
        let data = data();
        let delivered = solution_of(&data, &[("bicaridine", 40)]);

        let outcome = grade(
            data.reagent("bicaridine"),
            Units::whole(30),
            &delivered,
            ContainerKind::Beaker,
            Some(Units::whole(15)),
        );

        assert_eq!(outcome, Outcome::Success);
    }

    #[test]
    fn a_pill_over_the_threshold_is_an_overdose() {
        let data = data();
        let delivered = solution_of(&data, &[("bicaridine", 20)]);

        let outcome = grade(
            data.reagent("bicaridine"),
            Units::whole(20),
            &delivered,
            ContainerKind::Pill,
            Some(Units::whole(15)),
        );

        assert_eq!(outcome, Outcome::Overdose);
    }

    #[test]
    fn overdose_outranks_contamination() {
        let data = data();
        let delivered = solution_of(&data, &[("bicaridine", 20), ("oxygen", 3)]);

        let outcome = grade(
            data.reagent("bicaridine"),
            Units::whole(20),
            &delivered,
            ContainerKind::Pill,
            Some(Units::whole(15)),
        );

        assert_eq!(outcome, Outcome::Overdose, "the worse problem must win");
    }

    #[test]
    fn too_little_is_short_not_success() {
        let data = data();
        let delivered = solution_of(&data, &[("dylovene", 10)]);

        let outcome = grade(
            data.reagent("dylovene"),
            Units::whole(30),
            &delivered,
            ContainerKind::Beaker,
            Some(Units::whole(20)),
        );

        assert_eq!(outcome, Outcome::Short);
    }

    #[test]
    fn the_wrong_chemical_entirely_is_wrong() {
        let data = data();
        let delivered = solution_of(&data, &[("kelotane", 40)]);

        let outcome = grade(
            data.reagent("bicaridine"),
            Units::whole(30),
            &delivered,
            ContainerKind::Beaker,
            Some(Units::whole(15)),
        );

        assert_eq!(outcome, Outcome::Wrong);
    }

    #[test]
    fn a_reagent_with_no_overdose_threshold_never_overdoses() {
        let data = data();
        let inaprovaline = data.reagent("inaprovaline");
        assert!(
            data.reagents.get(inaprovaline).overdose.is_none(),
            "inaprovaline is meant to be safe at any dose"
        );
        let delivered = solution_of(&data, &[("inaprovaline", 20)]);

        let outcome = grade(
            inaprovaline,
            Units::whole(20),
            &delivered,
            ContainerKind::Pill,
            None,
        );

        assert_eq!(outcome, Outcome::Success);
    }
}
