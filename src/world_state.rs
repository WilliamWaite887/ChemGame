//! Persistence for the physical, authority-owned lab.
//!
//! `progress.ron` records the career. This module deliberately owns a separate
//! `world.ron`: player positions and inventories, loose items, machine-loaded
//! containers, and the exact mixtures in those containers are live world state,
//! not career totals. Keeping the formats apart lets this snapshot grow toward
//! more entity kinds without coupling every gameplay module to `ProgressSave`.

use std::collections::{HashMap, VecDeque};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::{Kelvin, Solution, Units};
use serde::{Deserialize, Serialize};

use crate::chem_data::ChemDb;
use crate::containers::{
    Container, ContainerKind, HeldBy, InSlot, InSlotB, InventorySlot, SelectedInventorySlot,
    Stored, INVENTORY_SLOTS,
};
use crate::labels::Label;
use crate::machines::{Machine, MachineKind, Overclock};
use crate::net::{is_authority, AccountId};
use crate::player::{Chemist, Look, PlayerAccount};
use crate::produce::{Produce, ProduceId};
use crate::rogue_security::Deterrent;
use crate::saves::SaveSlot;
use crate::AppState;

const WORLD_FORMAT_VERSION: u32 = 2;
const AUTOSAVE_SECONDS: f32 = 2.0;

pub struct WorldStatePlugin;

/// Ordering shared with default entity spawners on `OnEnter(Playing)`.
///
/// Loading must finish before the starter glassware decides whether this is a
/// fresh lab. Player/item restoration itself happens in `Update`, after the
/// authority has spawned the chemists and the asynchronous map can add machines.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorldLoadSet {
    Read,
    SpawnDefaults,
}

impl Plugin for WorldStatePlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PendingWorldState>()
            .init_resource::<CachedWorldState>()
            .init_resource::<WrittenWorldState>()
            .init_resource::<WorldAutosaveClock>()
            .configure_sets(
                OnEnter(AppState::Playing),
                (WorldLoadSet::Read, WorldLoadSet::SpawnDefaults).chain(),
            )
            .add_systems(
                OnEnter(AppState::Playing),
                load_world.in_set(WorldLoadSet::Read).run_if(is_authority),
            )
            .add_systems(
                Update,
                (
                    restore_lobby_players,
                    restore_placed_items,
                    capture_world_cache,
                    autosave_world,
                )
                    .chain()
                    .run_if(in_state(AppState::Playing))
                    .run_if(is_authority),
            )
            .add_systems(
                OnExit(AppState::Playing),
                flush_world
                    .before(crate::session::clear_session_state)
                    .run_if(is_authority),
            );
    }
}

#[derive(Resource)]
struct WorldAutosaveClock(Timer);

impl Default for WorldAutosaveClock {
    fn default() -> Self {
        Self(Timer::from_seconds(AUTOSAVE_SECONDS, TimerMode::Repeating))
    }
}

/// Last complete in-memory snapshot. Captured every authority frame, while the
/// signed disk file is throttled; quitting can therefore flush the exact last
/// simulated frame without depending on whether entity teardown runs first.
#[derive(Resource, Default)]
struct CachedWorldState(Option<WorldSave>);

/// Last snapshot handed to the recoverable writer. Temperatures and player
/// positions can change often, but an idle lab should not fsync an identical
/// signed file every two seconds.
#[derive(Resource, Default)]
struct WrittenWorldState(Option<WorldSave>);

/// The unread portion of a loaded snapshot.
///
/// Guest state stays queued when a save opens with only the host connected. A
/// returning lobby member receives the next saved guest slot when their chemist
/// is created. Pending entries are also folded into autosaves, so merely waiting
/// for a guest or the map cannot erase their inventory or a slotted beaker.
#[derive(Resource, Default)]
pub struct PendingWorldState {
    loaded_from_disk: bool,
    write_blocked: bool,
    players: VecDeque<PlayerSave>,
    placed: Vec<PlacedItemSave>,
}

impl PendingWorldState {
    pub fn loaded_from_disk(&self) -> bool {
        self.loaded_from_disk
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct WorldSave {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    players: Vec<PlayerSave>,
    #[serde(default)]
    placed: Vec<PlacedItemSave>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct PlayerSave {
    /// Stable account owner. Version-one saves do not have this and use the
    /// legacy host/join-order fallback once before the next autosave migrates
    /// them.
    #[serde(default)]
    account: Option<AccountId>,
    /// Version-one migration hint only.
    #[serde(default)]
    host: bool,
    position: [f32; 3],
    #[serde(default)]
    yaw: f32,
    #[serde(default)]
    selected_slot: u8,
    #[serde(default)]
    items: Vec<InventoryItemSave>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct InventoryItemSave {
    slot: u8,
    #[serde(default)]
    active: bool,
    item: ItemSave,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct PlacedItemSave {
    transform: TransformSave,
    #[serde(default)]
    placement: PlacementSave,
    item: ItemSave,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
enum PlacementSave {
    #[default]
    World,
    Machine {
        machine: MachineLocator,
        slot: MachineSlot,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct MachineLocator {
    kind: MachineKind,
    position: [f32; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum MachineSlot {
    Primary,
    Secondary,
    Stored,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
enum ItemSave {
    Container(ContainerSave),
    Produce(ProduceId),
    Deterrent { charges: u32 },
    Overclock { charges: u32 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ContainerSave {
    kind: ContainerKind,
    solution: SolutionSave,
    #[serde(default)]
    label: Option<String>,
}

/// Key-based solution data keeps a beaker meaningful if reagent ordering moves
/// between builds. Raw `Solution` serialization uses positional `ReagentId`s,
/// which is correct on the network but too brittle for a long-lived save file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SolutionSave {
    #[serde(default = "ambient_kelvin")]
    temperature: f32,
    #[serde(default)]
    reagents: Vec<ReagentPortionSave>,
}

fn ambient_kelvin() -> f32 {
    Kelvin::AMBIENT.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct ReagentPortionSave {
    key: String,
    amount_raw: i32,
    #[serde(default = "full_purity")]
    purity: f32,
    #[serde(default = "neutral_ph")]
    ph: f32,
}

fn full_purity() -> f32 {
    1.0
}

fn neutral_ph() -> f32 {
    7.0
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
struct TransformSave {
    translation: [f32; 3],
    rotation: [f32; 4],
}

impl TransformSave {
    fn from_transform(transform: &Transform) -> Self {
        Self {
            translation: transform.translation.to_array(),
            rotation: transform.rotation.to_array(),
        }
    }

    fn into_transform(self) -> Transform {
        Transform::from_translation(Vec3::from_array(self.translation))
            .with_rotation(Quat::from_array(self.rotation).normalize())
    }
}

impl ContainerSave {
    fn from_live(container: &Container, label: Option<&Label>, db: &ChemDb) -> Self {
        let reagents = container
            .solution
            .iter()
            .map(|(id, amount)| ReagentPortionSave {
                key: db.reagents.get(id).key.clone(),
                amount_raw: amount.raw(),
                purity: container.solution.purity_of(id),
                ph: container.solution.reagent_ph(id),
            })
            .collect();
        Self {
            kind: container.kind,
            solution: SolutionSave {
                temperature: container.solution.temperature.0,
                reagents,
            },
            label: label.map(|label| label.0.clone()),
        }
    }

    fn into_live(self, db: &ChemDb) -> (Container, Option<Label>) {
        let mut solution = Solution::new(self.kind.capacity());
        solution.temperature = Kelvin(self.solution.temperature.max(0.0));
        for portion in self.solution.reagents {
            let Some(id) = db.reagents.id_of(&portion.key) else {
                warn!("saved container contains unknown reagent '{}'", portion.key);
                continue;
            };
            let overflow = solution.add_profiled(
                id,
                Units::from_raw(portion.amount_raw.max(0)),
                portion.purity,
                portion.ph,
            );
            if overflow.is_positive() {
                warn!(
                    "saved {} contained more than its capacity; discarded {:.2}u",
                    self.kind.label(),
                    overflow.as_f32()
                );
            }
        }
        (
            Container {
                kind: self.kind,
                solution,
            },
            self.label.map(Label),
        )
    }
}

fn load_world(mut commands: Commands, slot: Option<Res<SaveSlot>>) {
    // Never let a rapid load-then-exit write the previous session's cached
    // entities into the newly selected slot before its first Update.
    commands.insert_resource(CachedWorldState::default());
    commands.insert_resource(WrittenWorldState::default());
    commands.insert_resource(WorldAutosaveClock::default());
    let Some(slot) = slot else {
        commands.insert_resource(PendingWorldState::default());
        return;
    };
    let path = slot.world_path();
    let Some(text) = crate::saves::read_slot_text(&path) else {
        commands.insert_resource(PendingWorldState::default());
        return;
    };
    match ron::from_str::<WorldSave>(&text) {
        Ok(save) => {
            if save.version > WORLD_FORMAT_VERSION {
                warn!(
                    "ignoring {} written by newer world format {}",
                    path.display(),
                    save.version
                );
                commands.insert_resource(PendingWorldState {
                    write_blocked: true,
                    ..default()
                });
                return;
            }
            commands.insert_resource(PendingWorldState {
                loaded_from_disk: true,
                write_blocked: false,
                players: save.players.into(),
                placed: save.placed,
            });
        }
        Err(error) => {
            warn!("ignoring unreadable {}: {error}", path.display());
            commands.insert_resource(PendingWorldState {
                write_blocked: true,
                ..default()
            });
        }
    }
}

fn restore_lobby_players(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut pending: ResMut<PendingWorldState>,
    added: Query<(Entity, &PlayerAccount, &Chemist), Added<PlayerAccount>>,
    mut players: Query<(&mut Transform, &mut Look, &mut SelectedInventorySlot)>,
) {
    let mut arrivals: Vec<(Entity, AccountId, bool)> = added
        .iter()
        .map(|(entity, account, chemist)| (entity, account.0, chemist.client == ClientId::Server))
        .collect();
    arrivals.sort_by_key(|(_, _, host)| !*host);

    for (entity, account, host) in arrivals {
        let index = pending
            .players
            .iter()
            .position(|saved| saved.account == Some(account))
            .or_else(|| {
                pending
                    .players
                    .iter()
                    .position(|saved| saved.account.is_none() && saved.host == host)
            })
            .or_else(|| {
                (!host).then(|| {
                    pending
                        .players
                        .iter()
                        .position(|saved| saved.account.is_none())
                })?
            });
        let Some(index) = index else {
            continue;
        };
        let Some(saved) = pending.players.remove(index) else {
            continue;
        };
        let Ok((mut transform, mut look, mut selected)) = players.get_mut(entity) else {
            continue;
        };
        transform.translation = Vec3::from_array(saved.position);
        transform.rotation = Quat::from_rotation_y(saved.yaw);
        look.yaw = saved.yaw;
        selected.0 = saved.selected_slot.min(INVENTORY_SLOTS - 1);
        let player_position = transform.translation;
        for inventory in saved.items {
            let slot = inventory.slot.min(INVENTORY_SLOTS - 1);
            let item = spawn_item(
                &mut commands,
                inventory.item,
                Transform::from_translation(player_position),
                &db,
            );
            let mut entity_commands = commands.entity(item);
            entity_commands.insert(InventorySlot {
                owner: entity,
                slot,
            });
            if inventory.active {
                entity_commands.insert(HeldBy(entity));
            }
        }
    }
}

fn restore_placed_items(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut pending: ResMut<PendingWorldState>,
    machines: Query<(Entity, &Machine, &Transform)>,
) {
    let mut waiting = Vec::new();
    for saved in std::mem::take(&mut pending.placed) {
        let relation = match &saved.placement {
            PlacementSave::World => None,
            PlacementSave::Machine { machine, slot } => {
                let target = machines
                    .iter()
                    .filter(|(_, state, _)| state.kind == machine.kind)
                    .min_by(|(_, _, left), (_, _, right)| {
                        left.translation
                            .distance_squared(Vec3::from_array(machine.position))
                            .total_cmp(
                                &right
                                    .translation
                                    .distance_squared(Vec3::from_array(machine.position)),
                            )
                    })
                    .filter(|(_, _, transform)| {
                        transform
                            .translation
                            .distance_squared(Vec3::from_array(machine.position))
                            < 1.0
                    })
                    .map(|(entity, _, _)| (entity, *slot));
                let Some(target) = target else {
                    waiting.push(saved);
                    continue;
                };
                Some(target)
            }
        };
        let item = spawn_item(
            &mut commands,
            saved.item,
            saved.transform.into_transform(),
            &db,
        );
        if let Some((machine, slot)) = relation {
            match slot {
                MachineSlot::Primary => commands.entity(item).insert(InSlot(machine)),
                MachineSlot::Secondary => commands.entity(item).insert(InSlotB(machine)),
                MachineSlot::Stored => commands.entity(item).insert(Stored(machine)),
            };
        }
    }
    pending.placed = waiting;
}

fn spawn_item(
    commands: &mut Commands,
    saved: ItemSave,
    transform: Transform,
    db: &ChemDb,
) -> Entity {
    let mut entity = commands.spawn((transform, Replicated, crate::until_we_leave_the_lab()));
    match saved {
        ItemSave::Container(saved) => {
            let (container, label) = saved.into_live(db);
            entity.insert(container);
            if let Some(label) = label {
                entity.insert(label);
            }
        }
        ItemSave::Produce(id) => {
            entity.insert(Produce(id));
        }
        ItemSave::Deterrent { charges } => {
            entity.insert(Deterrent { charges });
        }
        ItemSave::Overclock { charges } => {
            entity.insert(Overclock { charges });
        }
    }
    entity.id()
}

type PlayerQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static PlayerAccount,
        Option<&'static Chemist>,
        &'static Transform,
        &'static Look,
        &'static SelectedInventorySlot,
    ),
>;

type ItemQuery<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static Transform,
        Option<&'static Container>,
        Option<&'static Produce>,
        Option<&'static Deterrent>,
        Option<&'static Overclock>,
        Option<&'static Label>,
        Option<&'static InventorySlot>,
        Option<&'static HeldBy>,
        Option<&'static InSlot>,
        Option<&'static InSlotB>,
        Option<&'static Stored>,
    ),
    Or<(
        With<Container>,
        With<Produce>,
        With<Deterrent>,
        With<Overclock>,
    )>,
>;

#[derive(SystemParam)]
struct SnapshotSource<'w, 's> {
    players: PlayerQuery<'w, 's>,
    items: ItemQuery<'w, 's>,
    machines: Query<'w, 's, (&'static Machine, &'static Transform)>,
}

fn item_save(
    container: Option<&Container>,
    produce: Option<&Produce>,
    deterrent: Option<&Deterrent>,
    overclock: Option<&Overclock>,
    label: Option<&Label>,
    db: &ChemDb,
) -> Option<ItemSave> {
    if let Some(container) = container {
        Some(ItemSave::Container(ContainerSave::from_live(
            container, label, db,
        )))
    } else if let Some(produce) = produce {
        Some(ItemSave::Produce(produce.0))
    } else if let Some(deterrent) = deterrent {
        Some(ItemSave::Deterrent {
            charges: deterrent.charges,
        })
    } else {
        overclock.map(|overclock| ItemSave::Overclock {
            charges: overclock.charges,
        })
    }
}

fn capture_world(source: &SnapshotSource, db: &ChemDb, pending: &PendingWorldState) -> WorldSave {
    let mut live: Vec<(Entity, AccountId, PlayerSave)> = source
        .players
        .iter()
        .map(|(entity, account, chemist, transform, look, selected)| {
            let host = chemist.is_some_and(|chemist| chemist.client == ClientId::Server);
            (
                entity,
                account.0,
                PlayerSave {
                    account: Some(account.0),
                    host,
                    position: transform.translation.to_array(),
                    yaw: look.yaw,
                    selected_slot: selected.0,
                    items: Vec::new(),
                },
            )
        })
        .collect();
    live.sort_by_key(|(_, account, _)| *account);
    let owners: HashMap<Entity, usize> = live
        .iter()
        .enumerate()
        .map(|(index, (entity, _, _))| (*entity, index))
        .collect();
    let mut placed = Vec::new();

    for (
        _entity,
        transform,
        container,
        produce,
        deterrent,
        overclock,
        label,
        inventory,
        held,
        in_slot,
        in_slot_b,
        stored,
    ) in &source.items
    {
        let Some(item) = item_save(container, produce, deterrent, overclock, label, db) else {
            continue;
        };
        let owner = inventory
            .and_then(|inventory| owners.get(&inventory.owner).copied())
            .or_else(|| held.and_then(|held| owners.get(&held.0).copied()));
        if let Some(owner) = owner {
            let slot = inventory
                .map(|inventory| inventory.slot)
                .unwrap_or(live[owner].2.selected_slot);
            let active = held.is_some_and(|held| held.0 == live[owner].0);
            live[owner]
                .2
                .items
                .push(InventoryItemSave { slot, active, item });
            continue;
        }

        let relation = in_slot
            .map(|slot| (slot.0, MachineSlot::Primary))
            .or_else(|| in_slot_b.map(|slot| (slot.0, MachineSlot::Secondary)))
            .or_else(|| stored.map(|stored| (stored.0, MachineSlot::Stored)));
        let placement = relation
            .and_then(|(target, slot)| source.machines.get(target).ok().map(|m| (m, slot)))
            .map_or(PlacementSave::World, |((machine, transform), slot)| {
                PlacementSave::Machine {
                    machine: MachineLocator {
                        kind: machine.kind,
                        position: transform.translation.to_array(),
                    },
                    slot,
                }
            });
        placed.push(PlacedItemSave {
            transform: TransformSave::from_transform(transform),
            placement,
            item,
        });
    }

    let mut players: Vec<PlayerSave> = live.into_iter().map(|(_, _, saved)| saved).collect();
    for player in &mut players {
        player.items.sort_by_key(|item| item.slot);
    }
    players.extend(pending.players.iter().cloned());
    placed.extend(pending.placed.iter().cloned());
    WorldSave {
        version: WORLD_FORMAT_VERSION,
        players,
        placed,
    }
}

fn autosave_world(
    time: Res<Time>,
    mut clock: ResMut<WorldAutosaveClock>,
    slot: Option<Res<SaveSlot>>,
    pending: Res<PendingWorldState>,
    cached: Res<CachedWorldState>,
    mut written: ResMut<WrittenWorldState>,
) {
    clock.0.tick(time.delta());
    if !clock.0.just_finished() {
        return;
    }
    write_cached_world(slot.as_deref(), &pending, &cached, &mut written);
}

fn flush_world(
    slot: Option<Res<SaveSlot>>,
    pending: Res<PendingWorldState>,
    cached: Res<CachedWorldState>,
    mut written: ResMut<WrittenWorldState>,
) {
    write_cached_world(slot.as_deref(), &pending, &cached, &mut written);
}

fn capture_world_cache(
    db: Res<ChemDb>,
    pending: Res<PendingWorldState>,
    source: SnapshotSource,
    mut cached: ResMut<CachedWorldState>,
) {
    cached.0 = Some(capture_world(&source, &db, &pending));
}

fn write_cached_world(
    slot: Option<&SaveSlot>,
    pending: &PendingWorldState,
    cached: &CachedWorldState,
    written: &mut WrittenWorldState,
) {
    let Some(slot) = slot else {
        return;
    };
    if pending.write_blocked {
        return;
    }
    let Some(save) = &cached.0 else {
        return;
    };
    if written.0.as_ref() == Some(save) {
        return;
    }
    match ron::ser::to_string_pretty(&save, ron::ser::PrettyConfig::default()) {
        Ok(text) => {
            slot.write_world(&text);
            written.0 = Some(save.clone());
        }
        Err(error) => warn!("could not serialize live lab state: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::system::RunSystemOnce;
    use chem_sim::ChemData;

    fn db() -> ChemDb {
        let reagents: crate::chem_data::ReagentList =
            ron::from_str(include_str!("../assets/data/chem.reagents.ron")).unwrap();
        let reactions: crate::chem_data::ReactionList =
            ron::from_str(include_str!("../assets/data/chem.reactions.ron")).unwrap();
        ChemDb(ChemData::from_defs(reagents.0, reactions.0).unwrap())
    }

    #[test]
    fn solution_round_trip_uses_reagent_keys_and_preserves_quality() {
        let db = db();
        let water = db.reagents.id_of("water").unwrap();
        let mut container = Container::new(ContainerKind::Beaker);
        container.solution.temperature = Kelvin(412.5);
        let _ = container
            .solution
            .add_profiled(water, Units::from_f64(17.25), 0.73, 5.4);
        let saved = ContainerSave::from_live(&container, Some(&Label("sample".into())), &db);
        assert_eq!(saved.solution.reagents[0].key, "water");

        let (restored, label) = saved.into_live(&db);
        assert_eq!(restored.solution.volume_of(water), Units::from_f64(17.25));
        assert_eq!(restored.solution.purity_of(water), 0.73);
        assert_eq!(restored.solution.reagent_ph(water), 5.4);
        assert_eq!(restored.solution.temperature, Kelvin(412.5));
        assert_eq!(label.unwrap().0, "sample");
    }

    #[test]
    fn sparse_older_world_snapshot_remains_readable() {
        let save: WorldSave = ron::from_str("(players: [], placed: [])").unwrap();
        assert_eq!(save.version, 0);
    }

    #[test]
    fn version_one_player_without_account_identity_remains_readable() {
        let save: WorldSave =
            ron::from_str("(version:1,players:[(host:true,position:(1.0,1.7,2.0))],placed:[])")
                .unwrap();
        assert_eq!(save.players[0].account, None);
        assert!(save.players[0].host);
    }

    #[test]
    fn every_saved_lobby_player_keeps_their_active_hand() {
        let mut world = World::new();
        world.insert_resource(db());
        world.insert_resource(PendingWorldState::default());
        let host = world
            .spawn((
                PlayerAccount(AccountId::from_bytes([1; 16])),
                Chemist {
                    client: ClientId::Server,
                },
                Transform::from_xyz(1.0, 1.7, 2.0),
                Look {
                    yaw: 0.4,
                    pitch: 0.0,
                },
                SelectedInventorySlot(2),
            ))
            .id();
        let connection = world.spawn_empty().id();
        let guest = world
            .spawn((
                PlayerAccount(AccountId::from_bytes([2; 16])),
                Chemist {
                    client: ClientId::Client(connection),
                },
                Transform::from_xyz(3.0, 1.7, 4.0),
                Look {
                    yaw: -0.8,
                    pitch: 0.0,
                },
                SelectedInventorySlot(1),
            ))
            .id();
        for (owner, slot) in [(host, 2), (guest, 1)] {
            world.spawn((
                Container::new(ContainerKind::Beaker),
                Transform::default(),
                InventorySlot { owner, slot },
                HeldBy(owner),
            ));
        }

        #[derive(Resource)]
        struct Captured(WorldSave);
        fn capture(
            mut commands: Commands,
            db: Res<ChemDb>,
            pending: Res<PendingWorldState>,
            source: SnapshotSource,
        ) {
            commands.insert_resource(Captured(capture_world(&source, &db, &pending)));
        }
        world.run_system_once(capture).unwrap();
        world.flush();
        let captured = world.resource::<Captured>();
        assert_eq!(captured.0.players.len(), 2);
        assert!(captured
            .0
            .players
            .iter()
            .all(|player| player.items.len() == 1 && player.items[0].active));
    }

    #[test]
    fn host_and_returning_guest_receive_their_saved_positions_and_hands() {
        let mut world = World::new();
        world.insert_resource(db());
        let host_account = AccountId::from_bytes([1; 16]);
        let guest_account = AccountId::from_bytes([2; 16]);
        let held = |account, host, position, slot, kind| PlayerSave {
            account: Some(account),
            host,
            position,
            yaw: if host { 0.25 } else { -0.75 },
            selected_slot: slot,
            items: vec![InventoryItemSave {
                slot,
                active: true,
                item: ItemSave::Container(ContainerSave {
                    kind,
                    solution: SolutionSave {
                        temperature: Kelvin::AMBIENT.0,
                        reagents: Vec::new(),
                    },
                    label: None,
                }),
            }],
        };
        world.insert_resource(PendingWorldState {
            loaded_from_disk: true,
            write_blocked: false,
            // Deliberately wrong legacy host flags: exact account identity
            // must win once a version-two save has it.
            players: [
                held(
                    host_account,
                    false,
                    [1.0, 1.7, 2.0],
                    2,
                    ContainerKind::LargeBeaker,
                ),
                held(
                    guest_account,
                    true,
                    [5.0, 1.7, 6.0],
                    1,
                    ContainerKind::Bottle,
                ),
            ]
            .into(),
            placed: Vec::new(),
        });
        let host = world
            .spawn((
                PlayerAccount(host_account),
                Chemist {
                    client: ClientId::Server,
                },
                Transform::default(),
                Look::default(),
                SelectedInventorySlot::default(),
            ))
            .id();
        let connection = world.spawn_empty().id();
        let guest = world
            .spawn((
                PlayerAccount(guest_account),
                Chemist {
                    client: ClientId::Client(connection),
                },
                Transform::default(),
                Look::default(),
                SelectedInventorySlot::default(),
            ))
            .id();

        world.run_system_once(restore_lobby_players).unwrap();
        world.flush();

        assert_eq!(world.get::<Transform>(host).unwrap().translation.x, 1.0);
        assert_eq!(world.get::<Transform>(guest).unwrap().translation.x, 5.0);
        assert_eq!(world.get::<SelectedInventorySlot>(host).unwrap().0, 2);
        assert_eq!(world.get::<SelectedInventorySlot>(guest).unwrap().0, 1);
        let mut inventory = world.query::<(&InventorySlot, &HeldBy, &Container)>();
        let held_by: Vec<(Entity, u8, ContainerKind)> = inventory
            .iter(&world)
            .map(|(slot, held, container)| (held.0, slot.slot, container.kind))
            .collect();
        assert!(held_by.contains(&(host, 2, ContainerKind::LargeBeaker)));
        assert!(held_by.contains(&(guest, 1, ContainerKind::Bottle)));
        assert!(world.resource::<PendingWorldState>().players.is_empty());
    }
}
