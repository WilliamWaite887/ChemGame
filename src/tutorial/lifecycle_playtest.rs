//! Opt-in desktop checks of actual state transitions and session teardown.
use super::*;

const CORE: [&str; 6] = [
    "bearings",
    "request",
    "batch",
    "delivery",
    "mistake",
    "independent",
];
#[derive(Resource, Default)]
struct Run {
    started: Option<std::time::Instant>,
    since: f32,
    phase: u8,
    lesson: usize,
    epoch: u64,
    old: Vec<Entity>,
    slots: Vec<String>,
    files: HashMap<std::path::PathBuf, Vec<u8>>,
    notes: Vec<String>,
}
pub(super) fn install(app: &mut App) {
    app.init_resource::<Run>()
        .add_systems(
            PreUpdate,
            super::playtest::ignore_live_input.after(bevy::input::InputSystems),
        )
        .add_systems(PostUpdate, drive.after(super::advance));
}
fn finish(world: &mut World, error: Option<&str>) {
    let text = format!(
        "{}\n{}\n{}\n",
        if error.is_some() { "FAIL" } else { "PASS" },
        error.unwrap_or("Exercise reconstruction and career isolation verified."),
        world.resource::<Run>().notes.join("\n")
    );
    let _ = std::fs::create_dir_all("target/tutorial-playtest");
    let _ = std::fs::write("target/tutorial-playtest/lifecycle-result.txt", text);
    world.write_message(AppExit::Success);
}
fn step(world: &mut World, phase: u8) {
    let mut run = world.resource_mut::<Run>();
    run.phase = phase;
    run.since = 0.0;
}
fn note(world: &mut World, text: String) {
    world.resource_mut::<Run>().notes.push(text);
    let _ = std::fs::write(
        "target/tutorial-playtest/lifecycle-progress.txt",
        world.resource::<Run>().notes.join("\n"),
    );
}
fn saved_files(root: &std::path::Path) -> HashMap<std::path::PathBuf, Vec<u8>> {
    let mut files = HashMap::new();
    for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(saved_files(&path));
        } else if path
            .file_name()
            .is_some_and(|name| name != "training.ron" && name != "settings.ron")
        {
            files.insert(path.clone(), std::fs::read(path).unwrap());
        }
    }
    files
}
fn drive(world: &mut World) {
    if world.resource::<Run>().started.is_none() {
        world.resource_mut::<Run>().started = Some(std::time::Instant::now());
    }
    if world.resource::<Run>().started.unwrap().elapsed().as_secs() > 180 {
        let at = format!(
            "Lifecycle timeout in phase {}",
            world.resource::<Run>().phase
        );
        finish(world, Some(&at));
        return;
    }
    let state = *world.resource::<State<AppState>>().get();
    let ready =
        state == AppState::Playing && world.get_resource::<Runner>().is_some_and(|r| r.ready);
    let phase = world.resource::<Run>().phase;
    world.resource_mut::<Run>().since += world.resource::<Time<Real>>().delta_secs();
    if world.resource::<Run>().since < 1.0 {
        return;
    }
    match phase {
        0 if state == AppState::MainMenu => {
            world
                .resource_mut::<Profile>()
                .0
                .completed
                .insert("measurement".into());
            launch(world, CORE[0].into());
            step(world, 1);
        }
        1 if ready => {
            let player = world
                .query_filtered::<Entity, With<LocalPlayer>>()
                .iter(world)
                .next()
                .unwrap();
            let machine = world
                .query::<(Entity, &Machine)>()
                .iter(world)
                .find(|(_, m)| m.kind == MachineKind::ReactionChamber)
                .unwrap()
                .0;
            let sample = fixture(
                world,
                ContainerKind::Beaker,
                Vec3::ZERO,
                &[("nitrogen", 10)],
                None,
            );
            world.entity_mut(sample).insert(InSlot(machine));
            world.get_mut::<Machine>(machine).unwrap().in_use_by = Some(player);
            world
                .get_mut::<crate::machines::Thermostat>(machine)
                .unwrap()
                .powered = true;
            world
                .get_mut::<crate::machines::Thermostat>(machine)
                .unwrap()
                .target = chem_sim::Kelvin(400.0);
            let interaction = if world.resource::<Run>().lesson.is_multiple_of(2) {
                InteractionMode::UsingMachine(machine)
            } else {
                InteractionMode::OrderConversation(machine, 7)
            };
            world.entity_mut(player).insert(interaction);
            world
                .get_mut::<crate::body::Body>(player)
                .unwrap()
                .0
                .damage
                .burn = Units::whole(80);
            world
                .get_mut::<crate::body::Body>(player)
                .unwrap()
                .0
                .collapsed = true;
            let solution = world.get::<Container>(sample).unwrap().solution.clone();
            world.spawn((
                crate::chem_world::ChemicalPuddle::from_solution(solution, Some(player)),
                Transform::from_xyz(14.0, 0.0, 0.0),
                crate::until_we_leave_the_lab(),
            ));
            world.write_message(FromClient {
                client_id: ClientId::Server,
                message: crate::machines::AnalyzeRequested { machine },
            });
            let old = world
                .query_filtered::<Entity, With<DespawnOnExit<AppState>>>()
                .iter(world)
                .collect();
            let epoch = world.resource::<Runner>().epoch;
            world.resource_mut::<Run>().old = old;
            world.resource_mut::<Run>().epoch = epoch;
            let id = CORE[world.resource::<Run>().lesson].to_string();
            restart(world, id);
            step(world, 2);
        }
        2 if ready && world.resource::<Runner>().epoch != world.resource::<Run>().epoch => {
            let old_survived = world
                .resource::<Run>()
                .old
                .iter()
                .any(|id| world.get_entity(*id).is_ok());
            let dirty = world
                .query::<&crate::chem_world::ChemicalPuddle>()
                .iter(world)
                .next()
                .is_some()
                || world
                    .query_filtered::<&crate::body::Body, With<LocalPlayer>>()
                    .iter(world)
                    .any(|b| b.0.collapsed || b.0.damage.burn.is_positive())
                || world.resource::<Runner>().stage != 0
                || !world
                    .resource::<Profile>()
                    .0
                    .completed
                    .contains("measurement")
                || world.contains_resource::<crate::saves::SaveSlot>();
            if old_survived || dirty {
                finish(
                    world,
                    Some("Restart retained old entities, injuries, evidence or career state"),
                );
                return;
            }
            note(world, format!("Reconstructed {} at its first objective; discarded processing, injuries, spills and open interaction.", CORE[world.resource::<Run>().lesson]));
            world.resource_mut::<Run>().lesson += 1;
            if world.resource::<Run>().lesson < CORE.len() {
                restart(world, CORE[world.resource::<Run>().lesson].into());
                step(world, 1);
            } else {
                world.insert_resource(StartCareer);
                world
                    .resource_mut::<NextState<AppState>>()
                    .set(AppState::MainMenu);
                step(world, 3);
            }
        }
        3 if state == AppState::MainMenu => {
            if world.contains_resource::<Runner>()
                || world
                    .query_filtered::<Entity, With<TrainingCalibrated>>()
                    .iter(world)
                    .next()
                    .is_some()
            {
                finish(world, Some("Training capability survived leaving"));
                return;
            }
            world.spawn((
                crate::menu::MenuAction::NewSave,
                Interaction::Pressed,
                crate::until_we_leave_the_lab(),
            ));
            step(world, 4);
        }
        4 if state == AppState::Playing && world.contains_resource::<crate::arc::Campaign>() => {
            if world.resource::<SessionKind>() != &SessionKind::Career
                || world.contains_resource::<Runner>()
            {
                finish(world, Some("New career inherited training state"));
                return;
            }
            let name = world
                .resource::<crate::saves::SaveSlot>()
                .name()
                .to_string();
            if world.resource::<Run>().slots.contains(&name) {
                finish(world, Some("Second career reused the first slot"));
                return;
            }
            world.resource_mut::<Run>().slots.push(name.clone());
            note(
                world,
                format!(
                    "Opened normal career {name} with its own campaign and no training capability."
                ),
            );
            world
                .resource_mut::<NextState<AppState>>()
                .set(AppState::MainMenu);
            step(world, 5);
        }
        5 if state == AppState::MainMenu => {
            let files = saved_files(&crate::saves::saves_root());
            world.resource_mut::<Run>().files = files;
            launch(world, "free".into());
            step(world, 6);
        }
        6 if ready => {
            if saved_files(&crate::saves::saves_root()) != world.resource::<Run>().files {
                finish(
                    world,
                    Some("Training changed career or global account files"),
                );
                return;
            }
            world.insert_resource(StartCareer);
            world
                .resource_mut::<NextState<AppState>>()
                .set(AppState::MainMenu);
            step(world, 7);
        }
        7 if state == AppState::MainMenu => {
            if saved_files(&crate::saves::saves_root()) != world.resource::<Run>().files {
                finish(
                    world,
                    Some("Leaving training changed career or global files"),
                );
                return;
            }
            note(
                world,
                "Career and global account bytes unchanged across training and teardown.".into(),
            );
            if world.resource::<Run>().slots.len() < 2 {
                step(world, 3);
            } else {
                finish(world, None);
            }
        }
        _ => {}
    }
}
