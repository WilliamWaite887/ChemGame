//! Botany produce: the grinder's supply chain.
//!
//! Produce is a carryable item rather than a container. That distinction
//! matters: a [`Container`](crate::containers::Container) holds a solution and
//! can be graded at the delivery window, and a plant that could be handed to a
//! waiting crew member as if it were medicine would be nonsense. Everything
//! else about carrying — the one-hand rule, dropping, the held-item visual —
//! keys off `HeldBy` and works on produce unchanged.
//!
//! Supply comes through the door like everything else in this lab: a botanist
//! walks in on a timer, leaves an armful on the counter, and goes.

use bevy::prelude::*;
use bevy_common_assets::ron::RonAssetPlugin;
use bevy_replicon::prelude::*;
use chem_sim::{ReagentId, ReagentRegistry, Units};
use rand::prelude::*;
use serde::{Deserialize, Serialize};

use crate::chem_data::ChemDb;
use crate::crew::{spawn_crew_member, CrewMember, CrewPhase, CrewRoute};
use crate::interaction::Interactable;
use crate::lab::{DeliveryLane, DeliveryStations, COUNTER_TOP};
use crate::net::is_authority;
use crate::orders::StationData;
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::AppState;

/// Radius of a produce item, in metres.
const ITEM_RADIUS: f32 = 0.07;
/// Gap between items laid out on the counter.
const ITEM_SPACING: f32 = 0.26;

pub struct ProducePlugin;

impl Plugin for ProducePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RonAssetPlugin::<ProduceConfig>::new(&["produce.ron"]))
            .add_systems(Startup, start_loading)
            .add_systems(OnEnter(AppState::Playing), rearm_delivery_clock)
            .add_systems(
                Update,
                (
                    // The catalog is not server state: both ends need it to
                    // name what is in the hopper, so it is not authority-gated.
                    promote_produce_data,
                    // Who turns up and when is the server's business.
                    (deliver_produce, unload_produce)
                        .chain()
                        .run_if(is_authority),
                    // Drawing what turned up is everyone's.
                    dress_produce,
                )
                    .chain()
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

// ---------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------

/// `assets/data/station.produce.ron`, as written.
#[derive(Asset, TypePath, Deserialize)]
pub struct ProduceConfig {
    /// Crew member who brings it, looked up in the roster by name.
    pub courier: String,
    pub first_delivery_delay: f32,
    pub gap_seconds: (f32, f32),
    pub items_per_delivery: (u32, u32),
    pub kinds: Vec<ProduceDef>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ProduceDef {
    pub id: String,
    pub name: String,
    pub color: [f32; 3],
    /// Reagents released per item, in absolute units. Grinding is extraction,
    /// not a reaction, so these are quantities rather than ratios.
    pub yields: Vec<(String, Units)>,
}

/// An interned produce handle, the same shape as `ReagentId`.
///
/// A plain index keeps replication to a `u32`. Both ends load the same data
/// file, which the network compatibility fingerprint includes explicitly.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct ProduceId(pub u32);

impl ProduceId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// A loaded produce kind, with its yields resolved to reagent ids.
pub struct ProduceKind {
    pub id: ProduceId,
    pub name: String,
    pub color: [f32; 3],
    pub yields: Vec<(ReagentId, Units)>,
}

impl ProduceKind {
    /// Everything one item puts into the beaker. Checked against remaining
    /// capacity before grinding, so a full beaker refuses the item rather than
    /// swallowing it.
    pub fn total_yield(&self) -> Units {
        self.yields.iter().map(|(_, amount)| *amount).sum()
    }
}

/// Every produce kind in the game, plus the delivery schedule.
#[derive(Resource)]
pub struct ProduceCatalog {
    kinds: Vec<ProduceKind>,
    courier: String,
    gap_seconds: (f32, f32),
    items_per_delivery: (u32, u32),
    /// Carried so [`rearm_delivery_clock`] can arm a second session's clock
    /// exactly as the first one was armed. Without it the catalog knows the
    /// gap between hauls but not how long the lab waits for the first.
    first_delivery_delay: f32,
}

impl ProduceCatalog {
    /// Resolves yield keys to reagent ids.
    ///
    /// A key naming a reagent that does not exist is a content bug, but it
    /// should cost that one yield rather than the whole shift — so it warns
    /// and drops. `every_produce_yield_names_a_real_reagent` is what actually
    /// stops it reaching a player.
    pub fn from_config(config: &ProduceConfig, reagents: &ReagentRegistry) -> Self {
        let kinds = config
            .kinds
            .iter()
            .enumerate()
            .map(|(index, def)| ProduceKind {
                id: ProduceId(index as u32),
                name: def.name.clone(),
                color: def.color,
                yields: def
                    .yields
                    .iter()
                    .filter_map(|(key, amount)| match reagents.id_of(key) {
                        Some(id) => Some((id, *amount)),
                        None => {
                            warn!("produce '{}' yields unknown reagent '{key}'", def.id);
                            None
                        }
                    })
                    .collect(),
            })
            .collect();

        ProduceCatalog {
            kinds,
            courier: config.courier.clone(),
            gap_seconds: config.gap_seconds,
            items_per_delivery: config.items_per_delivery,
            first_delivery_delay: config.first_delivery_delay,
        }
    }

    pub fn get(&self, id: ProduceId) -> &ProduceKind {
        &self.kinds[id.index()]
    }

    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &ProduceKind> {
        self.kinds.iter()
    }
}

/// Shared mesh, and one material per kind so a poppy reads as a poppy across
/// the room.
#[derive(Resource)]
pub struct ProduceAssets {
    mesh: Handle<Mesh>,
    materials: Vec<Handle<StandardMaterial>>,
}

#[derive(Resource)]
struct PendingProduceData(Handle<ProduceConfig>);

#[derive(Resource)]
pub struct DeliverySchedule {
    timer: Timer,
}

impl DeliverySchedule {
    /// Brings the next haul forward.
    ///
    /// What a requisitioned produce crate buys: cargo lean on botany, and the
    /// grinder has something to work with at the start of the shift rather than
    /// somewhere in the middle of it.
    pub fn expedite(&mut self, seconds: f32) {
        if self.timer.remaining_secs() > seconds {
            self.timer = Timer::from_seconds(seconds, TimerMode::Once);
        }
    }
}

fn start_loading(mut commands: Commands, assets: Res<AssetServer>) {
    commands.insert_resource(PendingProduceData(assets.load("data/station.produce.ron")));
}

fn arm_delivery_clock(commands: &mut Commands, first_delivery_delay: f32) {
    commands.insert_resource(DeliverySchedule {
        timer: Timer::from_seconds(first_delivery_delay, TimerMode::Once),
    });
}

/// Re-arms botany's delivery clock on the way into the lab.
///
/// Same reasoning as `orders::rearm_arrival_clocks`: `promote_produce_data`
/// runs once per process and this is a `TimerMode::Once`, so without this the
/// second save opened in a session would never see a single plant arrive.
/// No-ops on the first pass, before the catalog has loaded.
fn rearm_delivery_clock(mut commands: Commands, catalog: Option<Res<ProduceCatalog>>) {
    let Some(catalog) = catalog else {
        return;
    };
    arm_delivery_clock(&mut commands, catalog.first_delivery_delay);
}

/// Turns the loaded RON into a catalog once the chemistry data is available.
///
/// Yields are written as reagent keys, so this cannot run before `ChemDb`
/// exists — the same ordering `promote_station_data` deals with.
fn promote_produce_data(
    mut commands: Commands,
    db: Option<Res<ChemDb>>,
    pending: Option<Res<PendingProduceData>>,
    mut configs: ResMut<Assets<ProduceConfig>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let (Some(db), Some(pending)) = (db, pending) else {
        return;
    };
    let Some(config) = configs.remove(&pending.0) else {
        return;
    };

    let catalog = ProduceCatalog::from_config(&config, &db.reagents);
    let handles = config
        .kinds
        .iter()
        .map(|def| {
            let [r, g, b] = def.color;
            materials.add(StandardMaterial {
                base_color: Color::srgb(r, g, b),
                perceptual_roughness: 0.75,
                ..default()
            })
        })
        .collect();

    info!("produce loaded: {} kinds", config.kinds.len());

    arm_delivery_clock(&mut commands, config.first_delivery_delay);
    commands.insert_resource(ProduceAssets {
        mesh: meshes.add(Sphere::new(ITEM_RADIUS)),
        materials: handles,
    });
    commands.insert_resource(catalog);
    commands.remove_resource::<PendingProduceData>();
}

// ---------------------------------------------------------------------------
// Items
// ---------------------------------------------------------------------------

/// A carryable plant, waiting to be ground.
#[derive(Component, Clone, Copy, Serialize, Deserialize)]
pub struct Produce(pub ProduceId);

/// Puts a produce item in the world and returns its entity.
///
/// Which plants are on the counter is shared lab state; the mesh and material
/// are not, and each end builds those itself in [`dress_produce`].
pub fn spawn_produce(commands: &mut Commands, kind: ProduceId, position: Vec3) -> Entity {
    commands
        .spawn((
            Produce(kind),
            Transform::from_translation(position),
            Replicated,
            crate::until_we_leave_the_lab(),
        ))
        .id()
}

/// Gives every produce item its mesh and its name.
///
/// Both come from the catalog, which both ends load from the same file, so
/// nothing about appearance has to cross the wire.
fn dress_produce(
    mut commands: Commands,
    catalog: Option<Res<ProduceCatalog>>,
    assets: Option<Res<ProduceAssets>>,
    items: Query<(Entity, &Produce), Added<Produce>>,
) {
    let (Some(catalog), Some(assets)) = (catalog, assets) else {
        return;
    };

    for (entity, produce) in &items {
        let kind = produce.0;
        commands.entity(entity).insert((
            Mesh3d(assets.mesh.clone()),
            MeshMaterial3d(assets.materials[kind.index()].clone()),
            Interactable::new(catalog.get(kind).name.clone()),
        ));
    }
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

/// A courier walking in with an armful of produce.
#[derive(Component)]
struct ProduceDelivery {
    items: Vec<ProduceId>,
}

/// Builds one haul from the least-stocked physical specimens.
fn balanced_delivery_items(
    catalog: &ProduceCatalog,
    present: impl IntoIterator<Item = ProduceId>,
    count: u32,
    rng: &mut impl Rng,
) -> Vec<ProduceId> {
    let mut stock = vec![0_u32; catalog.kinds.len()];
    for id in present {
        if let Some(amount) = stock.get_mut(id.index()) {
            *amount += 1;
        }
    }

    let mut delivery = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let Some(lowest) = stock.iter().copied().min() else {
            break;
        };
        let Some(kind) = catalog
            .iter()
            .filter(|kind| stock[kind.id.index()] == lowest)
            .choose(rng)
        else {
            break;
        };
        stock[kind.id.index()] += 1;
        delivery.push(kind.id);
    }
    delivery
}

/// Sends the botanist in on a timer.
///
/// Hauls favor the least-stocked physical specimens in the lab. This keeps a
/// large external-source catalog from starving one dependency through bad
/// luck, while consumed ingredients naturally rise back to the front of the
/// queue. Ties remain random, so deliveries do not become a fixed script.
#[allow(clippy::too_many_arguments)]
fn deliver_produce(
    mut commands: Commands,
    time: Res<Time>,
    catalog: Option<Res<ProduceCatalog>>,
    station: Option<Res<StationData>>,
    mut schedule: Option<ResMut<DeliverySchedule>>,
    present_crew: Query<&CrewMember, crate::crew::NotResident>,
    present_produce: Query<&Produce>,
) {
    let (Some(catalog), Some(station), Some(schedule)) = (catalog, station, schedule.as_mut())
    else {
        return;
    };
    if catalog.is_empty() {
        return;
    }

    if !schedule.timer.tick(time.delta()).just_finished() {
        return;
    }

    let mut rng = rand::rng();
    let gap = rng.random_range(catalog.gap_seconds.0..=catalog.gap_seconds.1);
    schedule.timer = Timer::from_seconds(gap, TimerMode::Once);

    // One of her is plenty. She is in the ordinary crew roster too, so without
    // this a delivery landing while she is already at the counter with an
    // order would put two of her in the room.
    if present_crew
        .iter()
        .any(|member| member.name == catalog.courier)
    {
        return;
    }

    let Some(def) = station
        .crew
        .iter()
        .find(|member| member.name == catalog.courier)
    else {
        warn!(
            "no crew member named '{}' to deliver produce",
            catalog.courier
        );
        return;
    };

    let count = rng.random_range(catalog.items_per_delivery.0..=catalog.items_per_delivery.1);
    let items = balanced_delivery_items(
        &catalog,
        present_produce.iter().map(|produce| produce.0),
        count,
        &mut rng,
    );
    if items.is_empty() {
        return;
    }

    // Her own lane at the counter, clear of whoever is queuing for an order.
    let courier = spawn_crew_member(&mut commands, def, -1.1);
    commands.entity(courier).insert(ProduceDelivery { items });
}

/// Puts the delivery down once she reaches the counter, then sends her out.
///
/// No hand-off: she leaves it on the counter and goes, so collecting produce
/// never competes with a crew member waiting on an order.
fn unload_produce(
    mut commands: Commands,
    // Still needed, but only to name the haul over the radio — the items
    // themselves are dressed from the catalog on each end.
    catalog: Option<Res<ProduceCatalog>>,
    mut radio: ResMut<RadioLog>,
    stations: Option<Res<DeliveryStations>>,
    mut couriers: Query<(Entity, &CrewMember, &ProduceDelivery, &mut CrewRoute)>,
) {
    let Some(catalog) = catalog else {
        return;
    };

    for (entity, member, delivery, mut route) in &mut couriers {
        if route.phase != CrewPhase::Waiting {
            continue;
        }

        // Laid out in a row so several items do not stack in one spot.
        let station = stations
            .as_deref()
            .cloned()
            .unwrap_or_default()
            .station(DeliveryLane::Public);
        let base = station.drop_position(COUNTER_TOP + ITEM_RADIUS);
        let across = station.transform.rotation * Vec3::X;
        let span = (delivery.items.len() as f32 - 1.0) * ITEM_SPACING;
        for (index, kind) in delivery.items.iter().enumerate() {
            let offset = -span * 0.5 + index as f32 * ITEM_SPACING;
            spawn_produce(&mut commands, *kind, base + across * offset);
        }

        radio.push(
            RadioEntry::new(
                channel_for(&member.role),
                format!(
                    "Dropped {} off at the window. Grinder's all yours.",
                    describe(&catalog, &delivery.items)
                ),
            )
            .speaker(&member.name)
            .positive(),
        );
        info!("{} delivered {} produce", member.name, delivery.items.len());

        commands.entity(entity).remove::<ProduceDelivery>();
        route.leave();
    }
}

/// "3 Poppy and 2 Aloe" — what she says she brought.
fn describe(catalog: &ProduceCatalog, items: &[ProduceId]) -> String {
    let mut parts = Vec::new();
    for kind in catalog.iter() {
        let count = items.iter().filter(|id| **id == kind.id).count();
        if count > 0 {
            parts.push(format!("{count} {}", kind.name));
        }
    }
    match parts.len() {
        0 => "nothing".to_string(),
        1 => parts.remove(0),
        _ => {
            let last = parts.pop().expect("checked non-empty");
            format!("{} and {last}", parts.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crew::CrewDef;
    use chem_sim::ChemData;

    fn config() -> ProduceConfig {
        ron::from_str(include_str!("../../assets/data/station.produce.ron"))
            .expect("produce data should parse")
    }

    fn chemistry() -> ChemData {
        ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap()
    }

    #[test]
    fn every_produce_yield_names_a_real_reagent() {
        // A typo here would drop the yield silently at load and leave the
        // grinder quietly producing less than the data says.
        let data = chemistry();
        for def in &config().kinds {
            assert!(!def.yields.is_empty(), "{} yields nothing", def.id);
            for (key, amount) in &def.yields {
                assert!(
                    data.reagents.id_of(key).is_some(),
                    "produce '{}' yields unknown reagent '{key}'",
                    def.id
                );
                assert!(amount.is_positive(), "{} yields {key} at {amount}", def.id);
            }
        }
    }

    #[test]
    fn repeated_hauls_evenly_cover_the_external_source_catalog() {
        let data = chemistry();
        let catalog = ProduceCatalog::from_config(&config(), &data.reagents);
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let items = balanced_delivery_items(
            &catalog,
            std::iter::empty(),
            (catalog.kinds.len() * 2) as u32,
            &mut rng,
        );
        let mut counts = vec![0_u32; catalog.kinds.len()];
        for item in items {
            counts[item.index()] += 1;
        }

        assert_eq!(counts.iter().copied().min(), counts.iter().copied().max());
    }

    #[test]
    fn deliveries_replace_consumed_sources_before_piling_up_more_stock() {
        let data = chemistry();
        let catalog = ProduceCatalog::from_config(&config(), &data.reagents);
        let overstocked = catalog.kinds[0].id;
        let mut rng = rand::rngs::StdRng::seed_from_u64(11);
        let next =
            balanced_delivery_items(&catalog, std::iter::repeat_n(overstocked, 4), 1, &mut rng);

        assert_ne!(next, vec![overstocked]);
    }

    #[test]
    fn every_kind_grinds_dirty() {
        // The grinder exists to be fast and impure. A single-reagent yield
        // would be deliverable straight out of the machine and would make the
        // Mixing Chamber pointless for that chemical.
        for def in &config().kinds {
            assert!(
                def.yields.len() > 1,
                "'{}' grinds to a single reagent, so its output needs no cleaning",
                def.id
            );
        }
    }

    #[test]
    fn koibeans_are_the_physical_source_of_carpotoxin() {
        let config = config();
        let koibean = config
            .kinds
            .iter()
            .find(|kind| kind.id == "koibean")
            .expect("the Rezadone branch needs a deliverable Koibean source");

        assert!(koibean
            .yields
            .iter()
            .any(|(reagent, amount)| reagent == "carpotoxin" && *amount >= Units::whole(12)));
        assert!(koibean
            .yields
            .iter()
            .any(|(reagent, _)| reagent == "plant_fibre"));
    }

    #[test]
    fn rare_botany_supplies_both_regenerative_jelly_inputs_dirty() {
        let config = config();
        for (produce, reagent) in [("ambrosia_deus", "omnizine"), ("glowshroom", "slime_jelly")] {
            let kind = config
                .kinds
                .iter()
                .find(|kind| kind.id == produce)
                .unwrap_or_else(|| panic!("missing {produce} source"));
            assert!(kind.yields.iter().any(|(key, _)| key == reagent));
            assert!(kind.yields.iter().any(|(key, _)| key == "plant_fibre"));
        }
    }

    #[test]
    fn tobacco_is_a_dirty_botanical_source_of_nicotine() {
        let config = config();
        let tobacco = config
            .kinds
            .iter()
            .find(|kind| kind.id == "tobacco")
            .expect("the Nicotine branch needs a deliverable Tobacco source");

        assert!(tobacco
            .yields
            .iter()
            .any(|(reagent, amount)| reagent == "nicotine" && *amount >= Units::whole(10)));
        assert!(tobacco
            .yields
            .iter()
            .any(|(reagent, _)| reagent == "plant_fibre"));
    }

    #[test]
    fn coffee_cherries_are_a_dirty_botanical_source_for_pump_up() {
        let config = config();
        let cherries = config
            .kinds
            .iter()
            .find(|kind| kind.id == "coffee_cherries")
            .expect("the Pump-Up branch needs a deliverable Coffee source");

        assert!(cherries
            .yields
            .iter()
            .any(|(reagent, amount)| reagent == "coffee" && *amount >= Units::whole(12)));
        assert!(cherries
            .yields
            .iter()
            .any(|(reagent, _)| reagent == "plant_fibre"));
    }

    #[test]
    fn psychedelic_mushrooms_are_a_dirty_source_of_slow_hallucinogen() {
        let config = config();
        let mushroom = config
            .kinds
            .iter()
            .find(|kind| kind.id == "psychedelic_mushroom")
            .expect("the narcotics table needs a deliverable Mushroom Hallucinogen source");

        assert!(mushroom
            .yields
            .iter()
            .any(|(reagent, amount)| reagent == "mushroom_hallucinogen"
                && *amount >= Units::whole(10)));
        assert!(mushroom
            .yields
            .iter()
            .any(|(reagent, _)| reagent == "plant_fibre"));
    }

    #[test]
    fn fermentation_culture_supplies_both_external_maintenance_inputs_dirty() {
        let config = config();
        let culture = config
            .kinds
            .iter()
            .find(|kind| kind.id == "fermentation_culture")
            .expect("the maintenance ladder needs Tea and Universal Enzyme");

        for reagent in ["tea", "universal_enzyme", "plant_fibre"] {
            assert!(
                culture.yields.iter().any(|(key, _)| key == reagent),
                "fermentation culture does not yield {reagent}"
            );
        }
    }

    #[test]
    fn kronkus_fruit_is_a_dirty_physical_source_for_kronkaine() {
        let config = config();
        let fruit = config
            .kinds
            .iter()
            .find(|kind| kind.id == "kronkus_fruit")
            .expect("the Kronkaine branch needs a deliverable Kronkus source");

        assert!(fruit
            .yields
            .iter()
            .any(|(reagent, amount)| reagent == "kronkus_extract" && *amount >= Units::whole(15)));
        assert!(fruit
            .yields
            .iter()
            .any(|(reagent, _)| reagent == "plant_fibre"));
    }

    #[test]
    fn specialist_poison_plants_supply_amanitin_and_curare_dirty() {
        let config = config();
        for (produce, reagent) in [("destroying_angel", "amanitin"), ("curare_vine", "curare")] {
            let kind = config
                .kinds
                .iter()
                .find(|kind| kind.id == produce)
                .unwrap_or_else(|| panic!("missing {produce} source"));
            assert!(kind
                .yields
                .iter()
                .any(|(key, amount)| key == reagent && *amount >= Units::whole(10)));
            assert!(kind.yields.iter().any(|(key, _)| key == "plant_fibre"));
        }
    }

    #[test]
    fn toxic_botany_and_pufferfish_supply_their_specialist_toxins_dirty() {
        let config = config();
        let berries = config
            .kinds
            .iter()
            .find(|kind| kind.id == "death_berries")
            .expect("Tirizene needs a Death Berry source");
        assert!(berries
            .yields
            .iter()
            .any(|(key, amount)| key == "tirizene" && *amount >= Units::whole(12)));
        assert!(berries
            .yields
            .iter()
            .any(|(key, amount)| key == "coniine" && *amount >= Units::whole(10)));
        assert!(berries.yields.iter().any(|(key, _)| key == "plant_fibre"));

        for (produce, reagent) in [("fly_amanita", "amatoxin"), ("omega_weed", "histamine")] {
            let kind = config
                .kinds
                .iter()
                .find(|kind| kind.id == produce)
                .unwrap_or_else(|| panic!("missing {produce} source"));
            assert!(kind
                .yields
                .iter()
                .any(|(key, amount)| key == reagent && *amount >= Units::whole(10)));
            assert!(kind.yields.iter().any(|(key, _)| key == "plant_fibre"));
        }

        for (produce, reagent) in [
            ("bungo_fruit", "bungotoxin"),
            ("giant_spider_venom_sac", "venom"),
        ] {
            let kind = config
                .kinds
                .iter()
                .find(|kind| kind.id == produce)
                .unwrap_or_else(|| panic!("missing {produce} source"));
            assert!(kind
                .yields
                .iter()
                .any(|(key, amount)| key == reagent && *amount >= Units::whole(10)));
            assert!(kind.yields.len() >= 2, "{produce} should grind dirty");
        }

        let fish = config
            .kinds
            .iter()
            .find(|kind| kind.id == "toxic_pufferfish")
            .expect("Tetrodotoxin needs a Toxic Pufferfish source");
        assert!(fish
            .yields
            .iter()
            .any(|(key, amount)| key == "tetrodotoxin" && *amount >= Units::whole(10)));
        assert!(fish.yields.iter().any(|(key, _)| key == "saltwater"));
    }

    #[test]
    fn the_courier_is_someone_on_the_crew_roster() {
        // The delivery is skipped with a warning if she is missing, which
        // would look like the grinder simply never getting any supply.
        let config = config();
        let roster: Vec<CrewDef> =
            ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();
        assert!(
            roster.iter().any(|member| member.name == config.courier),
            "no crew member named '{}' to deliver produce",
            config.courier
        );
    }
}
