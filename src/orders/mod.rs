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
use serde::Deserialize;

use crate::chem_data::ChemDb;
use crate::containers::{spawn_container, Container, ContainerAssets, ContainerKind, HeldBy};
use crate::crew::{spawn_crew_member, CrewAssets, CrewDef, CrewMember, CrewPhase, CrewRoute};
use crate::interaction::{InteractRequested, Interactable};
use crate::knowledge::{Knowledge, RESEARCH_PER_SUCCESS};
use crate::lab::COUNTER_SPOT;
use crate::machines::{chemist_entity, TestBenchStock};
use crate::player::Chemist;
use crate::radio::{announce_request, channel_for, RadioEntry, RadioLog};
use crate::AppState;

/// How often the crew ask for something just past what the chemist knows.
///
/// Every order being makeable is a treadmill; every order being impossible is
/// a wall. A minority of stretch requests is what sends the player to the
/// bench to work something out.
const STRETCH_ORDER_CHANCE: f64 = 0.25;

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
        .init_resource::<Shift>()
        .add_systems(Startup, start_loading)
        .add_systems(
            Update,
            (
                promote_station_data,
                generate_orders,
                expire_orders,
                handle_delivery,
                leave_sample_vials,
            )
                .chain()
                .run_if(in_state(AppState::Playing))
                // Orders, crew and grading are the server's business. A client
                // sees the crew arrive through replication.
                .run_if(in_state(ClientState::Disconnected)),
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
    pub requests: Vec<RequestDef>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RequestDef {
    pub reagent: String,
    pub amounts: Vec<u32>,
    pub plea: String,
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

#[derive(Resource)]
struct OrderSpawner {
    timer: Timer,
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

#[derive(Resource, Default)]
pub struct Shift {
    pub succeeded: u32,
    pub botched: u32,
    pub reputation: i32,
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
    mut radio: ResMut<RadioLog>,
    active: Query<&CrewMember>,
) {
    let Some(knowledge) = knowledge else {
        return;
    };
    let (Some(station), Some(assets), Some(spawner)) = (station, assets, spawner.as_mut()) else {
        return;
    };

    if !spawner.timer.tick(time.delta()).just_finished() {
        return;
    }

    let waiting = active.iter().count();
    let mut rng = rand::rng();
    let gap = rng.random_range(station.config.gap_seconds.0..=station.config.gap_seconds.1);
    spawner.timer = Timer::from_seconds(gap, TimerMode::Once);

    if waiting >= station.config.max_active {
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

    let pool = if !just_beyond.is_empty() && rng.random_bool(STRETCH_ORDER_CHANCE) {
        &just_beyond
    } else if !in_reach.is_empty() {
        &in_reach
    } else {
        &just_beyond
    };

    let Some(request) = pool.choose(&mut rng).copied() else {
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

    let patience = rng.random_range(
        station.config.patience_seconds.0..=station.config.patience_seconds.1,
    );
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
            .entity(request.target)
            .remove::<Order>()
            .remove::<Interactable>();
        route.leave();
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
