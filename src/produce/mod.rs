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
use crate::crew::{spawn_crew_member, CrewAssets, CrewMember, CrewPhase, CrewRoute};
use crate::interaction::Interactable;
use crate::lab::COUNTER_SPOT;
use crate::orders::StationData;
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::AppState;

/// Radius of a produce item, in metres.
const ITEM_RADIUS: f32 = 0.07;
/// Height of the delivery counter's top surface.
const COUNTER_TOP: f32 = 1.15;
/// Gap between items laid out on the counter.
const ITEM_SPACING: f32 = 0.26;

pub struct ProducePlugin;

impl Plugin for ProducePlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(RonAssetPlugin::<ProduceConfig>::new(&["produce.ron"]))
            .add_systems(Startup, start_loading)
            .add_systems(
                Update,
                (
                    // The catalog is not server state: both ends need it to
                    // name what is in the hopper, so it is not authority-gated.
                    promote_produce_data,
                    // Who turns up and when is the server's business.
                    (deliver_produce, unload_produce)
                        .chain()
                        .run_if(in_state(ClientState::Disconnected)),
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
/// file, which replicon's protocol hash already enforces.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize,
)]
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
    commands.insert_resource(PendingProduceData(
        assets.load("data/station.produce.ron"),
    ));
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

    commands.insert_resource(DeliverySchedule {
        timer: Timer::from_seconds(config.first_delivery_delay, TimerMode::Once),
    });
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
pub fn spawn_produce(
    commands: &mut Commands,
    catalog: &ProduceCatalog,
    assets: &ProduceAssets,
    kind: ProduceId,
    position: Vec3,
) -> Entity {
    commands
        .spawn((
            Produce(kind),
            Mesh3d(assets.mesh.clone()),
            MeshMaterial3d(assets.materials[kind.index()].clone()),
            Transform::from_translation(position),
            Interactable::new(catalog.get(kind).name.clone()),
            // Which plants are on the counter is shared lab state; the mesh
            // and material are not, and each client builds those itself.
            Replicated,
        ))
        .id()
}

// ---------------------------------------------------------------------------
// Delivery
// ---------------------------------------------------------------------------

/// A courier walking in with an armful of produce.
#[derive(Component)]
struct ProduceDelivery {
    items: Vec<ProduceId>,
}

/// Sends the botanist in on a timer.
#[allow(clippy::too_many_arguments)]
fn deliver_produce(
    mut commands: Commands,
    time: Res<Time>,
    catalog: Option<Res<ProduceCatalog>>,
    station: Option<Res<StationData>>,
    assets: Option<Res<CrewAssets>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut schedule: Option<ResMut<DeliverySchedule>>,
    present: Query<&CrewMember>,
) {
    let (Some(catalog), Some(station), Some(assets), Some(schedule)) =
        (catalog, station, assets, schedule.as_mut())
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
    if present.iter().any(|member| member.name == catalog.courier) {
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
    let items: Vec<ProduceId> = (0..count)
        .filter_map(|_| catalog.iter().choose(&mut rng).map(|kind| kind.id))
        .collect();
    if items.is_empty() {
        return;
    }

    // Her own lane at the counter, clear of whoever is queuing for an order.
    let courier = spawn_crew_member(&mut commands, &mut materials, &assets, def, -1.1);
    commands.entity(courier).insert(ProduceDelivery { items });
}

/// Puts the delivery down once she reaches the counter, then sends her out.
///
/// No hand-off: she leaves it on the counter and goes, so collecting produce
/// never competes with a crew member waiting on an order.
fn unload_produce(
    mut commands: Commands,
    catalog: Option<Res<ProduceCatalog>>,
    assets: Option<Res<ProduceAssets>>,
    mut radio: ResMut<RadioLog>,
    mut couriers: Query<(Entity, &CrewMember, &ProduceDelivery, &mut CrewRoute)>,
) {
    let (Some(catalog), Some(assets)) = (catalog, assets) else {
        return;
    };

    for (entity, member, delivery, mut route) in &mut couriers {
        if route.phase != CrewPhase::Waiting {
            continue;
        }

        // Laid out in a row so several items do not stack in one spot.
        let span = (delivery.items.len() as f32 - 1.0) * ITEM_SPACING;
        for (index, kind) in delivery.items.iter().enumerate() {
            let x = COUNTER_SPOT.x - span * 0.5 + index as f32 * ITEM_SPACING;
            spawn_produce(
                &mut commands,
                &catalog,
                &assets,
                *kind,
                Vec3::new(x, COUNTER_TOP + ITEM_RADIUS, COUNTER_SPOT.z - 1.0),
            );
        }

        radio.push(RadioEntry {
            channel: channel_for(&member.role),
            text: format!(
                "{}: dropped {} off at the window. Grinder's all yours.",
                member.name,
                describe(&catalog, &delivery.items)
            ),
            good: true,
        });
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
    fn every_kind_grinds_dirty() {
        // The grinder exists to be fast and impure. A single-reagent yield
        // would be deliverable straight out of the machine and would make the
        // ChemMaster pointless for that chemical.
        for def in &config().kinds {
            assert!(
                def.yields.len() > 1,
                "'{}' grinds to a single reagent, so its output needs no cleaning",
                def.id
            );
        }
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
