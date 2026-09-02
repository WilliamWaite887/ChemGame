//! Opt-in rendered acceptance. Relocation is a harness shortcut; actions use real handlers.
use super::*;
use crate::containers::{InSlotB, InSlotC, InventorySlot, SelectedInventorySlot, Stored};
use crate::machines::*;
use crate::settings::Paused;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
#[derive(Resource, Default)]
struct Scenario {
    last: Option<(String, usize)>,
    sent: bool,
    since: f32,
    started: Option<std::time::Instant>,
    shots: HashSet<String>,
    notes: Vec<String>,
    begun: bool,
    finishing: bool,
    nonce: u64,
    input: Option<KeyCode>,
}
pub(super) fn install(app: &mut App) {
    app.init_resource::<Scenario>()
        .add_systems(
            PreUpdate,
            ignore_live_input.after(bevy::input::InputSystems),
        )
        .add_systems(PreUpdate, inject_input.after(ignore_live_input))
        .add_systems(PostUpdate, drive.after(super::advance));
}
fn inject_input(mut scenario: ResMut<Scenario>, mut keys: ResMut<ButtonInput<KeyCode>>) {
    if let Some(key) = scenario.input.take() {
        keys.press(key);
    }
}
pub(super) fn ignore_live_input(
    mut keys: ResMut<ButtonInput<KeyCode>>,
    mut mouse: ResMut<ButtonInput<MouseButton>>,
    mut motion: ResMut<bevy::input::mouse::AccumulatedMouseMotion>,
) {
    keys.reset_all();
    mouse.reset_all();
    motion.delta = Vec2::ZERO;
}
fn request<T: Message>(world: &mut World, message: T)
where
    FromClient<T>: Message,
{
    world.write_message(FromClient {
        client_id: ClientId::Server,
        message,
    });
}
fn worker(world: &mut World) -> Option<Entity> {
    world
        .query_filtered::<Entity, With<LocalPlayer>>()
        .iter(world)
        .next()
}
fn machine(world: &mut World, kind: MachineKind) -> Entity {
    world
        .query::<(Entity, &Machine)>()
        .iter(world)
        .find(|(_, m)| m.kind == kind)
        .unwrap()
        .0
}
fn near(world: &mut World, player: Entity, target: Entity) {
    let at = world.get::<Transform>(target).unwrap().translation;
    let safe = world
        .resource::<crate::nav::NavGraph>()
        .standable_goal(at + Vec3::Z * 1.4);
    world.get_mut::<Transform>(player).unwrap().translation =
        Vec3::new(safe.x, crate::player::EYE_HEIGHT, safe.z);
    world
        .get_mut::<InteractionMode>(player)
        .unwrap()
        .set_if_neq(InteractionMode::Roaming);
}
fn hold(world: &mut World, player: Entity, item: Entity) {
    let held: Vec<_> = world
        .query::<(Entity, &HeldBy)>()
        .iter(world)
        .filter(|(_, h)| h.0 == player)
        .map(|(e, _)| e)
        .collect();
    for id in held {
        world.entity_mut(id).remove::<HeldBy>();
    }
    let occupied: Vec<_> = world
        .query::<(Entity, &InventorySlot)>()
        .iter(world)
        .filter(|(_, s)| s.owner == player && s.slot == 0)
        .map(|(e, _)| e)
        .collect();
    for id in occupied {
        world.entity_mut(id).remove::<InventorySlot>();
    }
    world
        .entity_mut(item)
        .remove::<(InSlot, InSlotB, InSlotC, Stored)>()
        .insert((
            HeldBy(player),
            InventorySlot {
                owner: player,
                slot: 0,
            },
        ));
    world.entity_mut(player).insert(SelectedInventorySlot(0));
}
fn aim_at(world: &mut World, player: Entity, target: Entity) -> bool {
    near(world, player, target);
    let endpoint = world.get::<Transform>(target).unwrap().translation + Vec3::Y * 0.3;
    let direction = endpoint - world.get::<Transform>(player).unwrap().translation;
    let mut look = world.get_mut::<crate::player::Look>(player).unwrap();
    look.yaw = (-direction.x).atan2(-direction.z);
    look.pitch = direction.y.atan2(direction.xz().length());
    if world.resource::<Scenario>().since > 3.0
        && !world
            .resource::<Scenario>()
            .shots
            .contains("aim-diagnostic")
    {
        screenshot(world, "aim-diagnostic");
        let camera = world
            .query_filtered::<&GlobalTransform, With<crate::player::PlayerCamera>>()
            .iter(world)
            .next()
            .copied();
        let focus = world
            .get::<crate::interaction::Focus>(player)
            .map(|f| (f.target, f.point));
        let at = world.get::<Transform>(player).unwrap().translation;
        let target_at = world.get::<Transform>(target).unwrap().translation;
        let _ = std::fs::write("target/tutorial-playtest/aim-diagnostic.txt",
            format!("player={at:?}, target={target:?} at={target_at:?}, camera={camera:?}, focus={focus:?}"));
    }
    world
        .get::<crate::interaction::Focus>(player)
        .is_some_and(|focus| focus.target == Some(target))
}
fn load(world: &mut World, player: Entity, target: Entity, item: Entity) {
    near(world, player, target);
    for mut m in world.query::<&mut Machine>().iter_mut(world) {
        m.in_use_by = None;
    }
    world.get_mut::<Machine>(target).unwrap().in_use_by = Some(player);
    let old: Vec<_> = world
        .query::<(Entity, &InSlot)>()
        .iter(world)
        .filter(|(_, s)| s.0 == target)
        .map(|(e, _)| e)
        .collect();
    for id in old {
        world.entity_mut(id).remove::<InSlot>();
    }
    world
        .entity_mut(item)
        .remove::<(HeldBy, InventorySlot, InSlotB, InSlotC, Stored)>()
        .insert(InSlot(target));
    world
        .entity_mut(player)
        .insert(InteractionMode::UsingMachine(target));
}
fn empty(world: &mut World) -> Entity {
    world
        .query::<(Entity, &Container)>()
        .iter(world)
        .find(|(_, c)| c.kind == ContainerKind::Beaker && c.solution.is_empty())
        .unwrap()
        .0
}
fn sample(world: &mut World) -> Option<Entity> {
    let kel = world
        .resource::<crate::chem_data::ChemDb>()
        .reagent("kelotane");
    world
        .query::<(Entity, &Container)>()
        .iter(world)
        .find(|(_, c)| c.solution.volume_of(kel).is_positive())
        .map(|(e, _)| e)
}
fn screenshot(world: &mut World, name: &str) {
    if !world.resource_mut::<Scenario>().shots.insert(name.into()) {
        return;
    }
    let path = format!("target/tutorial-playtest/{name}.png");
    let _ = std::fs::create_dir_all("target/tutorial-playtest");
    world
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(path));
}
fn finish(world: &mut World, ok: bool, reason: &str) {
    let _ = std::fs::create_dir_all("target/tutorial-playtest");
    let scenario = world.resource::<Scenario>();
    let text = format!(
        "{}\n{}\n{}\nHuman comprehension and timing: pending human playtest.\n",
        if ok { "PASS" } else { "FAIL" },
        reason,
        scenario.notes.join("\n")
    );
    let _ = std::fs::write("target/tutorial-playtest/result.txt", text);
    world.write_message(AppExit::Success);
}
fn drive(world: &mut World) {
    if !world.contains_resource::<State<AppState>>() {
        return;
    }
    let state = *world.resource::<State<AppState>>().get();
    if state == AppState::MainMenu && !world.resource::<Scenario>().begun {
        world.resource_mut::<Scenario>().begun = true;
        world.resource_mut::<Scenario>().started = Some(std::time::Instant::now());
        let args: Vec<String> = std::env::args().collect();
        let lesson = args
            .iter()
            .position(|arg| arg == "--practice")
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_else(|| "bearings".into());
        launch(world, lesson);
        return;
    }
    if world
        .resource::<Scenario>()
        .started
        .is_some_and(|at| at.elapsed().as_secs() > 210)
    {
        let at = world
            .get_resource::<Runner>()
            .map(|r| format!("{} stage {}", r.lesson, r.stage))
            .unwrap_or_default();
        finish(world, false, &format!("Timed out: {at}"));
        return;
    }
    if state != AppState::Playing || !world.get_resource::<Runner>().is_some_and(|r| r.ready) {
        return;
    }
    let Some(player) = worker(world) else { return };
    if world
        .resource::<Scenario>()
        .started
        .unwrap()
        .elapsed()
        .as_secs_f32()
        < 10.0
    {
        return;
    }
    if world.resource::<Paused>().0
        && world
            .resource::<Scenario>()
            .last
            .as_ref()
            .is_some_and(|(id, _)| id == "bearings")
    {
        world.resource_mut::<Scenario>().since += world.resource::<Time<Real>>().delta_secs();
        if world.resource::<Scenario>().since < 3.0 {
            return;
        }
        if !world.resource::<Scenario>().shots.contains("textbook") {
            screenshot(world, "textbook");
            return;
        }
    }
    // Training must stay physically real without any career contamination.
    if world.contains_resource::<crate::saves::SaveSlot>()
        || world.contains_resource::<crate::arc::Campaign>()
    {
        finish(world, false, "Training gained career state");
        return;
    }
    if world.resource::<Runner>().finished {
        if !world.resource::<Scenario>().finishing {
            world.resource_mut::<Paused>().0 = true;
            *world.resource_mut::<crate::settings::PauseScreen>() =
                crate::settings::PauseScreen::Training;
            world.resource_mut::<Scenario>().finishing = true;
            world.resource_mut::<Scenario>().since = 0.0;
            return;
        }
        world.resource_mut::<Scenario>().since += world.resource::<Time<Real>>().delta_secs();
        if world.resource::<Scenario>().since > 1.0 {
            screenshot(world, "core-complete");
        }
        if world.resource::<Scenario>().since > 3.0 {
            finish(world,true,"Selected course: every objective verified through real actions. No career slot or campaign. Screenshots captured.");
        }
        return;
    }
    let (id, index) = {
        let r = world.resource::<Runner>();
        (r.lesson.clone(), r.stage)
    };
    let stage = world
        .resource::<Lessons>()
        .0
        .iter()
        .find(|l| l.id == id)
        .unwrap()
        .stages[index]
        .clone();
    if world.resource::<Scenario>().last.as_ref() != Some(&(id.clone(), index)) {
        world.resource_mut::<Scenario>().last = Some((id.clone(), index));
        world.resource_mut::<Scenario>().sent = false;
        world.resource_mut::<Scenario>().since = 0.0;
        world
            .resource_mut::<Scenario>()
            .notes
            .push(format!("Reached {id}/{index}: {}", stage.objective));
        let _ = std::fs::write(
            "target/tutorial-playtest/progress.txt",
            world.resource::<Scenario>().notes.join("\n"),
        );
        world.resource_mut::<Paused>().0 = false;
    }
    world.resource_mut::<Scenario>().since += world.resource::<Time<Real>>().delta_secs();
    if world.resource::<Scenario>().since < 1.0 {
        return;
    }
    if id == "bearings" && index == 0 {
        screenshot(world, "training-lab");
    }
    if world.resource::<Scenario>().sent {
        return;
    }
    let mut sent = true;
    match stage.goal {
        Goal::Meet => {
            let target = world
                .query::<(Entity, &Actor)>()
                .iter(world)
                .find(|(_, a)| **a == Actor::Instructor)
                .unwrap()
                .0;
            if aim_at(world, player, target) {
                screenshot(world, "instructor-prompt");
                world.resource_mut::<Scenario>().input = Some(
                    world
                        .resource::<crate::settings::Settings>()
                        .bindings
                        .interact,
                );
            } else {
                sent = false;
            }
        }
        Goal::Pickup => {
            let target = empty(world);
            near(world, player, target);
            request(world, crate::interaction::InteractRequested { target });
        }
        Goal::Select => {
            request(
                world,
                crate::containers::SelectInventorySlotRequested { slot: 1 },
            );
        }
        Goal::Load => {
            let target = machine(world, MachineKind::ChemMaster5000);
            let item = world
                .query::<(Entity, &InventorySlot)>()
                .iter(world)
                .find(|(_, s)| s.owner == player)
                .unwrap()
                .0;
            hold(world, player, item);
            near(world, player, target);
            request(world, crate::interaction::InteractRequested { target });
        }
        Goal::Retrieve => {
            let target = machine(world, MachineKind::ChemMaster5000);
            request(world, crate::interaction::InteractRequested { target });
            request(
                world,
                EjectRequested {
                    machine: target,
                    slot: MachineSlot::A,
                },
            );
        }
        Goal::Textbook => {
            world.resource_mut::<Paused>().0 = true;
            *world.resource_mut::<crate::settings::PauseScreen>() =
                crate::settings::PauseScreen::TextbookArticle;
            world
                .resource_mut::<crate::textbook::TextbookView>()
                .article = Some(0);
            // Captured after the article has had time to render.
        }
        Goal::Converse | Goal::Accept => {
            let target = world
                .query::<(Entity, &Actor)>()
                .iter(world)
                .find(|(e, a)| {
                    **a == Actor::Customer
                        && world.get::<crate::order_intake::PendingOrder>(*e).is_some()
                })
                .map(|(e, _)| e);
            if let Some(target) = target {
                if world
                    .get::<crate::order_intake::AwaitingConversation>(target)
                    .is_some_and(|v| v.arrived)
                {
                    near(world, player, target);
                    let id = world
                        .get::<crate::order_intake::PendingOrder>(target)
                        .unwrap()
                        .context
                        .id;
                    request(
                        world,
                        crate::order_intake::OpenOrderConversation { target, id },
                    );
                    if stage.goal == Goal::Accept {
                        request(world, crate::order_intake::AcceptOrder { target, id });
                    }
                } else {
                    sent = false;
                }
            } else {
                sent = false;
            }
        }
        Goal::Directory => {
            world
                .entity_mut(player)
                .insert(InteractionMode::OrderDirectory {
                    machine: None,
                    return_to_book: false,
                });
        }
        Goal::Book => {
            world
                .entity_mut(player)
                .insert(InteractionMode::ReadingBook(None));
        }
        Goal::Batch10 => {
            let target = machine(world, MachineKind::ChemMaster5000);
            let item = empty(world);
            load(world, player, target, item);
            world
                .entity_mut(target)
                .insert(DispenseAmount(Units::whole(5)));
            for key in ["silicon", "carbon"] {
                let reagent = world.resource::<crate::chem_data::ChemDb>().reagent(key);
                request(
                    world,
                    DispenseRequested {
                        machine: target,
                        reagent,
                    },
                );
            }
        }
        Goal::Analyze | Goal::AnalyzeMistake => {
            let item = if stage.goal == Goal::AnalyzeMistake {
                world
                    .query_filtered::<Entity, With<MistakeSample>>()
                    .iter(world)
                    .next()
            } else {
                sample(world)
            };
            if let Some(item) = item {
                let target = machine(world, MachineKind::Analyzer);
                load(world, player, target, item);
                request(world, AnalyzeRequested { machine: target });
            } else {
                sent = false;
            }
        }
        Goal::Package => {
            let target = machine(world, MachineKind::MixingChamber);
            let buffer = world.get::<Buffer>(target).unwrap().0.total_volume();
            if !buffer.is_positive() {
                if let Some(item) = sample(world) {
                    load(world, player, target, item);
                    let reagent = world
                        .resource::<crate::chem_data::ChemDb>()
                        .reagent("kelotane");
                    request(
                        world,
                        BufferTransferRequested {
                            machine: target,
                            reagent,
                            amount: Units::whole(if id == "independent" { 10 } else { 30 }),
                            direction: BufferDirection::ToBuffer,
                            slot: MachineSlot::A,
                        },
                    );
                }
                sent = false;
            } else {
                near(world, player, target);
                world.get_mut::<Machine>(target).unwrap().in_use_by = Some(player);
                world.resource_mut::<Scenario>().nonce += 1;
                let nonce = world.resource::<Scenario>().nonce;
                request(
                    world,
                    PackageRequested {
                        machine: target,
                        kind: ContainerKind::Bottle,
                        label: None,
                        nonce,
                    },
                );
            }
        }
        Goal::Inspect => {
            let item = world
                .resource::<Runner>()
                .evidence
                .packages
                .iter()
                .copied()
                .find(|e| world.get::<Container>(*e).is_some());
            if let Some(item) = item {
                hold(world, player, item);
                world
                    .entity_mut(player)
                    .insert(InteractionMode::Inspecting {
                        item,
                        machine: None,
                    });
            } else {
                sent = false;
            }
        }
        Goal::Deliver => {
            if id == "independent" && world.resource::<Runner>().evidence.deliveries == 1 {
                // The larger batch is two normal, safe single-dose handovers.
                let pending = world
                    .query::<(
                        Entity,
                        &crate::order_intake::PendingOrder,
                        &crate::order_intake::AwaitingConversation,
                    )>()
                    .iter(world)
                    .next()
                    .map(|(e, p, a)| (e, p.context.id, a.arrived));
                if let Some((target, request_id, arrived)) = pending {
                    if arrived {
                        near(world, player, target);
                        request(
                            world,
                            crate::order_intake::OpenOrderConversation {
                                target,
                                id: request_id,
                            },
                        );
                        request(
                            world,
                            crate::order_intake::AcceptOrder {
                                target,
                                id: request_id,
                            },
                        );
                    }
                    return;
                }
                let bottle = world
                    .resource::<Runner>()
                    .evidence
                    .packages
                    .iter()
                    .copied()
                    .find(|e| world.get::<Container>(*e).is_some());
                if bottle.is_none() {
                    let target = machine(world, MachineKind::MixingChamber);
                    if world.get::<Buffer>(target).unwrap().0.is_empty() {
                        if let Some(item) = sample(world) {
                            extract(world, player, item, 10);
                        }
                    } else {
                        world.resource_mut::<Scenario>().nonce += 1;
                        let nonce = world.resource::<Scenario>().nonce;
                        request(
                            world,
                            PackageRequested {
                                machine: target,
                                kind: ContainerKind::Bottle,
                                label: None,
                                nonce,
                            },
                        );
                    }
                    return;
                }
            }
            let target = world
                .query::<(Entity, &crate::orders::Order)>()
                .iter(world)
                .next()
                .map(|(e, _)| e);
            if let Some(target) = target {
                if world
                    .get::<crate::crew::CrewRoute>(target)
                    .is_some_and(|r| r.phase == crate::crew::CrewPhase::Waiting)
                {
                    let item = world
                        .resource::<Runner>()
                        .evidence
                        .packages
                        .iter()
                        .copied()
                        .find(|e| world.get::<Container>(*e).is_some());
                    if let Some(item) = item {
                        near(world, player, target);
                        hold(world, player, item);
                        request(world, crate::interaction::InteractRequested { target });
                    } else {
                        sent = false;
                    }
                } else {
                    sent = false;
                }
            } else {
                sent = false;
            }
        }
        Goal::Repair => {
            let item = world
                .query_filtered::<Entity, With<MistakeSample>>()
                .iter(world)
                .next()
                .unwrap();
            let target = machine(world, MachineKind::ChemMaster5000);
            load(world, player, target, item);
            world
                .entity_mut(target)
                .insert(DispenseAmount(Units::whole(5)));
            let reagent = world
                .resource::<crate::chem_data::ChemDb>()
                .reagent("carbon");
            request(
                world,
                DispenseRequested {
                    machine: target,
                    reagent,
                },
            );
        }
        _ => {
            sent = practice(world, player, stage.goal);
        }
    }
    world.resource_mut::<Scenario>().sent =
        sent && !(id == "independent" && stage.goal == Goal::Deliver);
}

fn reagent(world: &World, key: &str) -> chem_sim::ReagentId {
    world.resource::<crate::chem_data::ChemDb>().reagent(key)
}
fn with_reagent(world: &mut World, key: &str) -> Option<Entity> {
    let r = reagent(world, key);
    world
        .query::<(Entity, &Container)>()
        .iter(world)
        .find(|(_, c)| c.solution.volume_of(r).is_positive())
        .map(|(e, _)| e)
}
fn dispense(world: &mut World, player: Entity, item: Entity, amount: i32, keys: &[&str]) {
    let target = machine(world, MachineKind::ChemMaster5000);
    load(world, player, target, item);
    world
        .entity_mut(target)
        .insert(DispenseAmount(Units::whole(amount)));
    for key in keys {
        let reagent = reagent(world, key);
        request(
            world,
            DispenseRequested {
                machine: target,
                reagent,
            },
        );
    }
}
fn extract(world: &mut World, player: Entity, item: Entity, amount: i32) {
    let target = machine(world, MachineKind::MixingChamber);
    load(world, player, target, item);
    let reagent = reagent(world, "kelotane");
    request(
        world,
        BufferTransferRequested {
            machine: target,
            reagent,
            amount: Units::whole(amount),
            direction: BufferDirection::ToBuffer,
            slot: MachineSlot::A,
        },
    );
}
fn grind(world: &mut World, player: Entity) {
    let target = machine(world, MachineKind::Grinder);
    let item = empty(world);
    load(world, player, target, item);
    if let Some(produce) = world
        .query_filtered::<Entity, With<crate::produce::Produce>>()
        .iter(world)
        .next()
    {
        hold(world, player, produce);
        request(world, crate::interaction::InteractRequested { target });
        request(
            world,
            GrindRequested {
                machine: target,
                all: true,
            },
        );
    }
}
fn practice(world: &mut World, player: Entity, goal: Goal) -> bool {
    match goal {
        Goal::Measured => {
            let small = world.resource::<Runner>().evidence.has(Goal::Batch10);
            let large = world.resource::<Runner>().evidence.has(Goal::Batch20);
            if !small || !large {
                let item = empty(world);
                dispense(
                    world,
                    player,
                    item,
                    if small { 10 } else { 5 },
                    &["silicon", "carbon"],
                );
            }
            false
        }
        Goal::Divided => {
            if let Some(item) = sample(world) {
                extract(world, player, item, 5);
            }
            true
        }
        Goal::Recombined => {
            if let Some(item) = sample(world) {
                extract(world, player, item, 30);
            }
            false
        }
        Goal::Printed => {
            let target = machine(world, MachineKind::Analyzer);
            if let Some(item) = world
                .query::<(Entity, &crate::analysis_reports::AnalysisReport)>()
                .iter(world)
                .next()
                .map(|(e, _)| e)
            {
                hold(world, player, item);
                return true;
            }
            if let Some(report) = world.get::<crate::analysis_reports::AnalyzerSnapshot>(target) {
                let report_id = report.report.id;
                request(
                    world,
                    crate::analysis_reports::PrintReportRequested {
                        machine: target,
                        report_id,
                    },
                );
            }
            false
        }
        Goal::StaleReport => {
            let item = sample(world).unwrap();
            dispense(world, player, item, 5, &["carbon"]);
            true
        }
        Goal::Reanalyzed => {
            let item = sample(world).unwrap();
            let target = machine(world, MachineKind::Analyzer);
            load(world, player, target, item);
            request(world, AnalyzeRequested { machine: target });
            true
        }
        Goal::Warm | Goal::Cool => {
            let item = with_reagent(world, "nitrogen").unwrap();
            let target = machine(world, MachineKind::ReactionChamber);
            load(world, player, target, item);
            request(
                world,
                SetTargetTemperature {
                    machine: target,
                    target: chem_sim::Kelvin(if goal == Goal::Warm { 330.0 } else { 290.0 }),
                },
            );
            request(
                world,
                SetHeaterPower {
                    machine: target,
                    on: true,
                },
            );
            true
        }
        Goal::RemovedWarm => {
            let target = machine(world, MachineKind::ReactionChamber);
            request(
                world,
                EjectRequested {
                    machine: target,
                    slot: MachineSlot::A,
                },
            );
            true
        }
        Goal::PhShift | Goal::PhReturn => {
            let item = with_reagent(world, "water").unwrap();
            dispense(
                world,
                player,
                item,
                5,
                &[if goal == Goal::PhShift {
                    "acidic_buffer"
                } else {
                    "basic_buffer"
                }],
            );
            true
        }
        Goal::PhStrip => {
            let target = with_reagent(world, "water").unwrap();
            let paper = world
                .query::<(Entity, &Container)>()
                .iter(world)
                .find(|(_, c)| c.kind == ContainerKind::PhPaper)
                .unwrap()
                .0;
            hold(world, player, paper);
            near(world, player, target);
            request(
                world,
                crate::body::ApplyHeldRequested {
                    target: Some(target),
                    point: None,
                },
            );
            true
        }
        Goal::Ground => {
            grind(world, player);
            true
        }
        Goal::Extracted => {
            if let Some(item) = sample(world) {
                extract(world, player, item, 30);
            }
            true
        }
        Goal::Purified => {
            if let Some(item) = sample(world) {
                let target = machine(world, MachineKind::Analyzer);
                load(world, player, target, item);
                let reagent = reagent(world, "kelotane");
                request(world, AnalyzeRequested { machine: target });
                request(
                    world,
                    PurifyRequested {
                        machine: target,
                        reagent,
                    },
                );
                true
            } else {
                grind(world, player);
                false
            }
        }
        Goal::TwoForms => {
            let target = machine(world, MachineKind::MixingChamber);
            if world.get::<Buffer>(target).unwrap().0.is_empty() {
                if let Some(item) = sample(world) {
                    extract(world, player, item, 5);
                }
                return false;
            }
            near(world, player, target);
            world.get_mut::<Machine>(target).unwrap().in_use_by = Some(player);
            let kind = if world
                .resource::<Runner>()
                .evidence
                .forms
                .contains(&ContainerKind::Patch)
            {
                ContainerKind::Syringe
            } else {
                ContainerKind::Patch
            };
            world.resource_mut::<Scenario>().nonce += 1;
            let nonce = world.resource::<Scenario>().nonce;
            request(
                world,
                PackageRequested {
                    machine: target,
                    kind,
                    label: None,
                    nonce,
                },
            );
            false
        }
        Goal::Treated => {
            let item = world
                .query::<(Entity, &Container)>()
                .iter(world)
                .find(|(_, c)| c.kind == ContainerKind::Syringe && !c.solution.is_empty())
                .unwrap()
                .0;
            let target = world
                .query::<(Entity, &Actor)>()
                .iter(world)
                .find(|(_, a)| **a == Actor::Patient)
                .unwrap()
                .0;
            hold(world, player, item);
            if aim_at(world, player, target) {
                screenshot(world, "patient-prompt");
                world.resource_mut::<Scenario>().input =
                    Some(world.resource::<crate::settings::Settings>().bindings.apply);
                true
            } else {
                false
            }
        }
        Goal::Recovered => true,
        Goal::Puddle | Goal::Residue => {
            let key = if goal == Goal::Puddle {
                "water"
            } else {
                "potassium"
            };
            if let Some(item) = with_reagent(world, key) {
                let point = spot(world, "experiment");
                world.get_mut::<Transform>(player).unwrap().translation =
                    Vec3::new(point.x, crate::player::EYE_HEIGHT, point.z + 1.4);
                world.entity_mut(player).insert(InteractionMode::Roaming);
                hold(world, player, item);
                request(
                    world,
                    crate::body::ApplyHeldRequested {
                        target: None,
                        point: Some(Vec3::new(point.x, 0.0, point.z)),
                    },
                );
            }
            true
        }
        Goal::Clean => {
            let target = world
                .query::<(Entity, &Actor)>()
                .iter(world)
                .find(|(_, a)| **a == Actor::Cleanup)
                .unwrap()
                .0;
            near(world, player, target);
            request(world, crate::interaction::InteractRequested { target });
            true
        }
        _ => {
            finish(world, false, "Unhandled practice objective");
            true
        }
    }
}
