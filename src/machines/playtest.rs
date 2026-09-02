//! Explicit debug-only rendered/network acceptance scenario. Use isolated appdata.
use super::*;
use crate::{
    analysis_reports::{AnalysisReport, AnalyzerSnapshot, PrintReportRequested, ReportOutput},
    net::LaunchMode,
    player::LocalPlayer,
};
use bevy::render::view::screenshot::{save_to_disk, Screenshot};

#[derive(Component, Clone, Serialize, Deserialize, MapEntities)]
struct Fixture {
    step: u8,
    cycle: u8,
    #[entities]
    worker: Entity,
    #[entities]
    receiver: Entity,
    #[entities]
    mixer: Entity,
    #[entities]
    analyzer: Entity,
    #[entities]
    a: Entity,
    #[entities]
    b: Entity,
    #[entities]
    products: Vec<Entity>,
    #[entities]
    report: Option<Entity>,
}
#[derive(Resource, Default)]
struct Scenario {
    started: Option<std::time::Instant>,
    phase_at: f32,
    phase: Option<(u8, u8)>,
    sent: HashSet<(u8, u8, u8)>,
    shots: HashSet<String>,
    checks: Vec<String>,
}
pub(super) fn install(app: &mut App) {
    app.replicate::<Fixture>()
        .init_resource::<Scenario>()
        .add_systems(
            Update,
            drive
                .run_if(in_state(AppState::Playing))
                .run_if(resource_exists::<crate::lab::MapReady>),
        );
}
fn path(world: &World) -> std::path::PathBuf {
    let role = match world.resource::<LaunchMode>() {
        LaunchMode::Singleplayer => "solo",
        LaunchMode::Join(_) => "client",
        _ => "host",
    };
    std::path::PathBuf::from("target/chemistry-playtest").join(role)
}
fn shot(world: &mut World, name: &str) {
    if !world.resource_mut::<Scenario>().shots.insert(name.into()) {
        return;
    }
    let output = path(world);
    let _ = std::fs::create_dir_all(&output);
    world
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(output.join(format!("{name}.png"))));
}
fn once(world: &mut World, fixture: &Fixture, action: u8) -> bool {
    world
        .resource_mut::<Scenario>()
        .sent
        .insert((fixture.cycle, fixture.step, action))
}
fn transition(world: &mut World, entity: Entity, fixture: &mut Fixture, next: u8, elapsed: f32) {
    fixture.step = next;
    world.entity_mut(entity).insert(fixture.clone());
    world.resource_mut::<Scenario>().phase_at = elapsed;
}
fn reset_sides(world: &mut World, fixture: &Fixture) {
    for (item, key) in [(fixture.a, "inaprovaline"), (fixture.b, "carbon")] {
        let reagent = world.resource::<ChemDb>().reagent(key);
        let mut container = Container::new(ContainerKind::LargeBeaker);
        let _ = container.solution.add(reagent, Units::whole(5));
        world.entity_mut(item).insert(container);
    }
    if fixture.cycle == 2 {
        for slot in 0..4 {
            let occupied = world
                .query::<&InventorySlot>()
                .iter(world)
                .any(|i| i.owner == fixture.worker && i.slot == slot);
            if !occupied {
                world.spawn((
                    Replicated,
                    Container::new(ContainerKind::Bottle),
                    InventorySlot {
                        owner: fixture.worker,
                        slot,
                    },
                    Transform::default(),
                    Visibility::default(),
                    crate::until_we_leave_the_lab(),
                ));
            }
        }
    }
}
fn seed(world: &mut World) -> Option<Fixture> {
    let solo = matches!(world.resource::<LaunchMode>(), LaunchMode::Singleplayer);
    let players: Vec<_> = world
        .query::<(Entity, &Chemist)>()
        .iter(world)
        .map(|(e, c)| (e, c.client))
        .collect();
    if !solo && players.len() < 2 {
        return None;
    }
    let receiver = players.iter().find(|(_, c)| *c == ClientId::Server)?.0;
    let worker = if solo {
        receiver
    } else {
        players.iter().find(|(_, c)| *c != ClientId::Server)?.0
    };
    let machines: Vec<_> = world
        .query::<(Entity, &Machine)>()
        .iter(world)
        .map(|(e, m)| (e, m.kind))
        .collect();
    let mixer = machines
        .iter()
        .find(|(_, k)| *k == MachineKind::MixingChamber)?
        .0;
    let analyzer = machines
        .iter()
        .find(|(_, k)| *k == MachineKind::Analyzer)?
        .0;
    for machine in [mixer, analyzer] {
        world.get_mut::<Machine>(machine).unwrap().in_use_by = Some(worker);
    }
    let inventory: Vec<_> = world
        .query::<(Entity, &InventorySlot)>()
        .iter(world)
        .filter(|(_, s)| s.owner == worker || s.owner == receiver)
        .map(|(e, _)| e)
        .collect();
    for item in inventory {
        world
            .entity_mut(item)
            .remove::<(HeldBy, InventorySlot)>()
            .insert(Transform::from_xyz(-1.0, 1.0, 0.0));
    }
    world.entity_mut(worker).insert(SelectedInventorySlot(0));
    world.entity_mut(receiver).insert(SelectedInventorySlot(0));
    let machine_at = world.get::<Transform>(mixer).unwrap().translation;
    for (player, offset) in [(worker, -0.4), (receiver, 0.4)] {
        world.get_mut::<Transform>(player).unwrap().translation = Vec3::new(
            machine_at.x + offset,
            crate::player::EYE_HEIGHT,
            machine_at.z + 1.3,
        );
    }
    let a = world
        .spawn((
            Replicated,
            Container::new(ContainerKind::LargeBeaker),
            InSlot(mixer),
            Transform::default(),
            Visibility::default(),
            crate::until_we_leave_the_lab(),
        ))
        .id();
    let b = world
        .spawn((
            Replicated,
            Container::new(ContainerKind::LargeBeaker),
            InSlotB(mixer),
            Transform::default(),
            Visibility::default(),
            crate::until_we_leave_the_lab(),
        ))
        .id();
    let fixture = Fixture {
        step: 0,
        cycle: 0,
        worker,
        receiver,
        mixer,
        analyzer,
        a,
        b,
        products: Vec::new(),
        report: None,
    };
    reset_sides(world, &fixture);
    Some(fixture)
}

fn drive(world: &mut World) {
    let Some(local) = world
        .query_filtered::<Entity, With<LocalPlayer>>()
        .iter(world)
        .next()
    else {
        return;
    };
    let start = *world
        .resource_mut::<Scenario>()
        .started
        .get_or_insert_with(std::time::Instant::now);
    let elapsed = start.elapsed().as_secs_f32();
    let authority = !matches!(world.resource::<LaunchMode>(), LaunchMode::Join(_));
    let found = world
        .query::<(Entity, &Fixture)>()
        .iter(world)
        .next()
        .map(|(e, f)| (e, f.clone()));
    let Some((entity, mut f)) = found else {
        if authority && elapsed > 4.0 {
            if let Some(f) = seed(world) {
                world.spawn((Replicated, f, crate::until_we_leave_the_lab()));
                world.resource_mut::<Scenario>().phase_at = elapsed;
            }
        }
        return;
    };
    let worker = local == f.worker;
    let recipient = local == f.receiver;
    {
        let mut scenario = world.resource_mut::<Scenario>();
        if scenario.phase != Some((f.cycle, f.step)) {
            scenario.phase = Some((f.cycle, f.step));
            scenario.phase_at = elapsed;
        }
    }
    let age = elapsed - world.resource::<Scenario>().phase_at;
    let product_reagent = world.resource::<ChemDb>().reagent("bicaridine");
    if elapsed > 150.0 && f.step != 13 {
        let output = path(world);
        let _ = std::fs::create_dir_all(&output);
        let _ = std::fs::write(
            output.join("result.txt"),
            format!("FAILED: stalled at cycle {} step {}", f.cycle, f.step),
        );
        world.write_message(AppExit::Success);
        return;
    }
    if worker && f.step <= 4 {
        world
            .entity_mut(local)
            .insert(InteractionMode::UsingMachine(f.mixer));
    }
    match f.step {
        0 => {
            if worker && once(world, &f, 0) {
                world.write_message(AgitateRequested {
                    machine: f.mixer,
                    direction: [
                        AgitateDirection::AToB,
                        AgitateDirection::BToA,
                        AgitateDirection::ToMixer,
                    ][f.cycle as usize],
                });
            }
            if authority && world.get::<AgitationRun>(f.mixer).is_some() {
                transition(world, entity, &mut f, 1, elapsed);
            }
        }
        1 => {
            if authority && world.get::<AgitationRun>(f.mixer).is_none() {
                let solution = if f.cycle == 2 {
                    &world.get::<Buffer>(f.mixer).unwrap().0
                } else {
                    &world
                        .get::<Container>(if f.cycle == 0 { f.b } else { f.a })
                        .unwrap()
                        .solution
                };
                assert_eq!(solution.volume_of(product_reagent), Units::whole(10));
                world
                    .resource_mut::<Scenario>()
                    .checks
                    .push(format!("Agitation route {}: 10u product", f.cycle));
                let next = if f.cycle == 2 { 3 } else { 2 };
                transition(world, entity, &mut f, next, elapsed);
            }
        }
        2 => {
            if worker && once(world, &f, 0) {
                world.write_message(BufferTransferRequested {
                    machine: f.mixer,
                    reagent: product_reagent,
                    amount: Units::whole(10),
                    direction: BufferDirection::ToBuffer,
                    slot: if f.cycle == 0 {
                        MachineSlot::B
                    } else {
                        MachineSlot::A
                    },
                });
            }
            if authority
                && world
                    .get::<Buffer>(f.mixer)
                    .unwrap()
                    .0
                    .volume_of(product_reagent)
                    == Units::whole(10)
            {
                transition(world, entity, &mut f, 3, elapsed);
            }
        }
        3 => {
            if worker {
                {
                    let mut draft = world.resource_mut::<crate::ui::mixing::PackagingDraft>();
                    draft.text = if f.cycle == 2 {
                        String::new()
                    } else {
                        "Fresh water".into()
                    };
                    draft.cursor = draft.text.len();
                }
                if age > 0.5 {
                    shot(world, &format!("mixer-{}", f.cycle));
                }
            }
            if authority && age > 3.0 {
                transition(world, entity, &mut f, 4, elapsed);
            }
        }
        4 => {
            if worker && once(world, &f, 0) {
                let request = PackageRequested {
                    machine: f.mixer,
                    kind: ContainerKind::Bottle,
                    label: (f.cycle != 2).then(|| "Fresh water".into()),
                    nonce: 10000 + f.cycle as u64,
                };
                world.write_message(request.clone());
                world.write_message(request);
            }
            if authority {
                let package = world
                    .query::<(Entity, &Container, &crate::labels::Label)>()
                    .iter(world)
                    .find(|(e, c, _)| {
                        !f.products.contains(e)
                            && c.kind == ContainerKind::Bottle
                            && c.solution.volume_of(product_reagent) == Units::whole(10)
                    })
                    .map(|(e, _, _)| e);
                if let Some(package) = package {
                    match f.cycle {
                        0 => assert_eq!(world.get::<HeldBy>(package).unwrap().0, f.worker),
                        1 => {
                            assert_eq!(world.get::<InventorySlot>(package).unwrap().slot, 1);
                            assert!(world.get::<HeldBy>(package).is_none());
                        }
                        _ => {
                            assert!(world.get::<InventorySlot>(package).is_none());
                            assert!(world.get::<HeldBy>(package).is_none());
                        }
                    }
                    assert!(world.get::<Buffer>(f.mixer).unwrap().0.is_empty());
                    f.products.push(package);
                    world.resource_mut::<Scenario>().checks.push(format!(
                        "Packaging route {}: correct placement, no duplicate",
                        f.cycle
                    ));
                    transition(world, entity, &mut f, 5, elapsed);
                }
            }
        }
        5 => {
            if worker && f.cycle == 0 {
                world.entity_mut(local).insert(InteractionMode::Inspecting {
                    item: f.products[0],
                    machine: Some(f.mixer),
                });
                if age > 0.5 {
                    shot(world, "labeled-inspection");
                }
            }
            if authority && age > 3.0 {
                if f.cycle < 2 {
                    f.cycle += 1;
                    reset_sides(world, &f);
                    transition(world, entity, &mut f, 0, elapsed);
                } else {
                    let product = f.products[2];
                    world.entity_mut(product).insert(InSlot(f.analyzer));
                    let spare = world
                        .query::<(Entity, &InventorySlot)>()
                        .iter(world)
                        .find(|(_, i)| i.owner == f.worker && i.slot == 3)
                        .map(|(e, _)| e);
                    if let Some(spare) = spare {
                        world
                            .entity_mut(spare)
                            .remove::<InventorySlot>()
                            .insert(Transform::from_xyz(-1.0, 1.0, 0.0));
                    }
                    transition(world, entity, &mut f, 6, elapsed);
                }
            }
        }
        6 => {
            if worker {
                world
                    .entity_mut(local)
                    .insert(InteractionMode::UsingMachine(f.analyzer));
                if once(world, &f, 0) {
                    world.write_message(AnalyzeRequested {
                        machine: f.analyzer,
                    });
                }
            }
            if authority && world.get::<AnalyzerSnapshot>(f.analyzer).is_some() {
                transition(world, entity, &mut f, 7, elapsed);
            }
        }
        7 => {
            if worker {
                if let Some(snapshot) = world.get::<AnalyzerSnapshot>(f.analyzer).cloned() {
                    if once(world, &f, 0) {
                        world.write_message(PrintReportRequested {
                            machine: f.analyzer,
                            report_id: snapshot.report.id,
                        });
                    }
                }
            }
            if authority {
                if let Some(report) = world
                    .query::<(Entity, &ReportOutput)>()
                    .iter(world)
                    .find(|(_, o)| o.0 == f.analyzer)
                    .map(|(e, _)| e)
                {
                    f.report = Some(report);
                    assert!(world
                        .get::<AnalysisReport>(report)
                        .unwrap()
                        .read()
                        .contains("Bicaridine"));
                    transition(world, entity, &mut f, 8, elapsed);
                }
            }
        }
        8 => {
            if let Some(report) = f.report {
                if worker {
                    world.entity_mut(local).insert(InteractionMode::Roaming);
                    if once(world, &f, 0) {
                        world.write_message(InteractRequested { target: report });
                    }
                    if let Some(slot) = world.get::<InventorySlot>(report).copied() {
                        if once(world, &f, 1) {
                            world.write_message(crate::containers::SelectInventorySlotRequested {
                                slot: slot.slot,
                            });
                        }
                    }
                }
                if authority && world.get::<HeldBy>(report).is_some_and(|h| h.0 == f.worker) {
                    transition(world, entity, &mut f, 9, elapsed);
                }
            }
        }
        9 => {
            if worker {
                world.entity_mut(local).insert(InteractionMode::Inspecting {
                    item: f.report.unwrap(),
                    machine: None,
                });
                if age > 0.5 {
                    shot(world, "report-inspection");
                }
            }
            if authority && age > 3.0 {
                transition(world, entity, &mut f, 10, elapsed);
            }
        }
        10 => {
            if worker {
                world.entity_mut(local).insert(InteractionMode::Roaming);
                if once(world, &f, 0) {
                    world.write_message(crate::containers::DropRequested);
                }
            }
            if authority && world.get::<InventorySlot>(f.report.unwrap()).is_none() {
                transition(world, entity, &mut f, 11, elapsed);
            }
        }
        11 => {
            if let Some(report) = f.report {
                if recipient {
                    world.entity_mut(local).insert(InteractionMode::Roaming);
                    if once(world, &f, 0) {
                        world.write_message(InteractRequested { target: report });
                    }
                    if let Some(slot) = world.get::<InventorySlot>(report).copied() {
                        if slot.owner == local && once(world, &f, 1) {
                            world.write_message(crate::containers::SelectInventorySlotRequested {
                                slot: slot.slot,
                            });
                        }
                    }
                }
                if authority
                    && world
                        .get::<HeldBy>(report)
                        .is_some_and(|h| h.0 == f.receiver)
                {
                    world
                        .resource_mut::<Scenario>()
                        .checks
                        .push("Analyzer report printed, inspected and handed over".into());
                    transition(world, entity, &mut f, 12, elapsed);
                }
            }
        }
        12 => {
            if recipient {
                world.entity_mut(local).insert(InteractionMode::Inspecting {
                    item: f.report.unwrap(),
                    machine: None,
                });
                if age > 0.5 {
                    shot(world, "received-report");
                }
            }
            if authority && age > 3.0 {
                transition(world, entity, &mut f, 14, elapsed);
            }
        }
        14 => {
            if worker {
                world.entity_mut(local).insert(InteractionMode::Roaming);
                if let Some(slot) = world.get::<InventorySlot>(f.products[0]).copied() {
                    if once(world, &f, 0) {
                        world.write_message(crate::containers::SelectInventorySlotRequested {
                            slot: slot.slot,
                        });
                    }
                }
            }
            if authority
                && world
                    .get::<HeldBy>(f.products[0])
                    .is_some_and(|h| h.0 == f.worker)
            {
                transition(world, entity, &mut f, 15, elapsed);
            }
        }
        15 => {
            if worker && once(world, &f, 0) {
                world.write_message(crate::containers::DropRequested);
            }
            if authority && world.get::<InventorySlot>(f.products[0]).is_none() {
                transition(world, entity, &mut f, 16, elapsed);
            }
        }
        16 => {
            if recipient {
                world.entity_mut(local).insert(InteractionMode::Roaming);
                if once(world, &f, 0) {
                    world.write_message(InteractRequested {
                        target: f.products[0],
                    });
                }
                if let Some(slot) = world.get::<InventorySlot>(f.products[0]).copied() {
                    if slot.owner == local && once(world, &f, 1) {
                        world.write_message(crate::containers::SelectInventorySlotRequested {
                            slot: slot.slot,
                        });
                    }
                }
            }
            if authority
                && world
                    .get::<HeldBy>(f.products[0])
                    .is_some_and(|h| h.0 == f.receiver)
            {
                assert_eq!(
                    world.get::<crate::labels::Label>(f.products[0]).unwrap().0,
                    "Fresh water"
                );
                assert_eq!(
                    world
                        .get::<Container>(f.products[0])
                        .unwrap()
                        .solution
                        .volume_of(product_reagent),
                    Units::whole(10)
                );
                world
                    .resource_mut::<Scenario>()
                    .checks
                    .push("Mislabeled handoff preserves the claim and the real chemistry".into());
                transition(world, entity, &mut f, 17, elapsed);
            }
        }
        17 => {
            if recipient {
                world.entity_mut(local).insert(InteractionMode::Inspecting {
                    item: f.products[0],
                    machine: None,
                });
                if age > 0.5 {
                    shot(world, "received-mislabeled-product");
                }
            }
            if authority && age > 3.0 {
                transition(world, entity, &mut f, 13, elapsed);
            }
        }
        _ => {
            if once(world, &f, 0) {
                let output = path(world);
                let _ = std::fs::create_dir_all(&output);
                let checks = world.resource::<Scenario>().checks.join("\n");
                let _ = std::fs::write(
                    output.join("result.txt"),
                    format!(
                        "PASS\nProducts: {}\nReport shared: {}\n{checks}\n",
                        f.products.len(),
                        f.report.is_some()
                    ),
                );
            }
            if !authority || age > 3.0 {
                world.write_message(AppExit::Success);
            }
        }
    }
}
