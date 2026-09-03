//! Isolated solo exercises driven by observed gameplay, never by requested actions.
#[cfg(debug_assertions)]
mod book_playtest;
mod guidance;
#[cfg(debug_assertions)]
mod lifecycle_playtest;
#[cfg(any(test, not(feature = "trenchbroom")))]
mod map;
#[cfg(debug_assertions)]
mod playtest;
#[cfg(test)]
mod tests;
pub(crate) mod ui;
use crate::{
    containers::{Container, ContainerKind, HeldBy, InSlot},
    interaction::InteractionMode,
    machines::{Machine, MachineKind},
    player::LocalPlayer,
    session::SessionKind,
    AppState,
};
use bevy::{ecs::system::SystemState, prelude::*};
use bevy_replicon::prelude::*;
use chem_sim::{Solution, Units};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
pub use ui::pause_controls;
pub const TRAINING_REQUEST_BIT: u64 = 1 << 63;

#[derive(Resource)]
pub struct StartCareer;
#[derive(Resource)]
struct Relaunch(pub String);
#[derive(Resource, Default)]
pub struct TrainingSpots(pub HashMap<String, Transform>);
#[derive(Component, Clone, Copy, PartialEq, Eq)]
enum Actor {
    Instructor,
    Customer,
    Patient,
    Cleanup,
}
#[derive(Component)]
struct MistakeSample;

#[derive(Debug, Clone, Deserialize)]
struct Stage {
    goal: Goal,
    objective: String,
    explanation: String,
    hints: [String; 3],
    marker: String,
    focus: String,
    article: String,
}
#[derive(Debug, Clone, Deserialize)]
struct Lesson {
    id: String,
    title: String,
    core: bool,
    stages: Vec<Stage>,
}
#[derive(Resource)]
struct Lessons(Vec<Lesson>);
impl Default for Lessons {
    fn default() -> Self {
        Self(
            ron::from_str(include_str!("../../assets/data/lab.training.ron"))
                .expect("valid training lessons"),
        )
    }
}
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
enum Goal {
    Meet,
    Pickup,
    Select,
    Load,
    Retrieve,
    Textbook,
    Converse,
    Accept,
    Directory,
    Book,
    Batch10,
    Analyze,
    Package,
    Inspect,
    Deliver,
    AnalyzeMistake,
    Repair,
    Batch20,
    Measured,
    Divided,
    Recombined,
    Printed,
    StaleReport,
    Reanalyzed,
    Warm,
    Cool,
    RemovedWarm,
    PhShift,
    PhStrip,
    PhReturn,
    Ground,
    Extracted,
    Purified,
    TwoForms,
    Treated,
    Recovered,
    Puddle,
    Residue,
    Clean,
}
#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct TrainingProgress {
    pub version: u32,
    pub completed: HashSet<String>,
    pub skipped: HashSet<String>,
    pub resume: Option<String>,
}
#[derive(Resource, Default)]
pub struct Profile(pub TrainingProgress);
impl Profile {
    fn path() -> std::path::PathBuf {
        crate::saves::saves_root().join("training.ron")
    }
    fn load() -> Self {
        let p = std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| ron::from_str::<TrainingProgress>(&s).ok())
            .filter(|p| p.version == 1)
            .unwrap_or_default();
        Self(p)
    }
    fn save(&self) {
        let Ok(text) = ron::ser::to_string_pretty(&self.0, default()) else {
            return;
        };
        let path = Self::path();
        if let Some(parent) = path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                warn!("training progress: {e}");
                return;
            }
        }
        if let Err(e) = std::fs::write(path, text) {
            warn!("training progress: {e}");
        }
    }
}
#[derive(Default)]
struct Evidence {
    goals: HashSet<GoalKey>,
    held: HashSet<Entity>,
    loaded: HashSet<Entity>,
    scans: HashMap<u64, crate::analysis_reports::AnalyzerSnapshot>,
    verified: Vec<crate::analysis_reports::AnalysisReport>,
    packages: HashSet<Entity>,
    forms: HashSet<ContainerKind>,
    warm: HashSet<Entity>,
    changed_ph: HashSet<Entity>,
    printed: HashSet<u64>,
    initial: HashSet<Entity>,
    deliveries: usize,
    handovers: HashSet<Entity>,
}
// A compact ordered key without making authored Goal part of save or wire formats.
#[derive(Hash, PartialEq, Eq)]
struct GoalKey(u8);
impl Evidence {
    fn set(&mut self, g: Goal) {
        self.goals.insert(GoalKey(g as u8));
    }
    fn has(&self, g: Goal) -> bool {
        self.goals.contains(&GoalKey(g as u8))
    }
}
#[derive(Resource)]
pub struct Runner {
    lesson: String,
    stage: usize,
    epoch: u64,
    ready: bool,
    finished: bool,
    evidence: Evidence,
    hint: usize,
    idle: f32,
    offered: bool,
    expanded: bool,
    revision: u64,
    last_goal_count: usize,
    last_explanation: String,
}
impl Runner {
    fn new(lesson: String) -> Self {
        Self {
            lesson,
            stage: 0,
            epoch: rand::random::<u64>() | TRAINING_REQUEST_BIT,
            ready: false,
            finished: false,
            evidence: default(),
            hint: 0,
            idle: 0.0,
            offered: false,
            expanded: false,
            revision: 0,
            last_goal_count: 0,
            last_explanation: String::new(),
        }
    }
}
#[derive(Message, Clone)]
pub struct DeliveryEvidence {
    pub request: u64,
    pub container: Entity,
    pub kind: ContainerKind,
    pub actual: Solution,
    pub outcome: crate::orders::Outcome,
}

pub struct TutorialPlugin;
impl Plugin for TutorialPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Lessons>()
            .init_resource::<Profile>()
            .init_resource::<TrainingSpots>()
            .add_message::<DeliveryEvidence>()
            .add_systems(Startup, load_profile)
            .add_systems(Update, relaunch.run_if(in_state(AppState::MainMenu)))
            .add_systems(
                PostUpdate,
                (prepare, observe, advance)
                    .chain()
                    .run_if(in_state(AppState::Playing))
                    .run_if(crate::session::training_session),
            )
            .add_systems(
                Update,
                talk.run_if(in_state(AppState::Playing))
                    .run_if(crate::session::training_session),
            );
        #[cfg(not(feature = "trenchbroom"))]
        app.add_systems(
            OnEnter(AppState::Playing),
            map::spawn.run_if(crate::session::training_session),
        );
        ui::install(app);
        #[cfg(debug_assertions)]
        if std::env::args().any(|a| a == "--tutorial-playtest") {
            playtest::install(app);
        }
        #[cfg(debug_assertions)]
        if std::env::args().any(|a| a == "--textbook-playtest") {
            book_playtest::install(app);
        }
        #[cfg(debug_assertions)]
        if std::env::args().any(|a| a == "--tutorial-lifecycle-playtest") {
            lifecycle_playtest::install(app);
        }
    }
}
fn load_profile(mut profile: ResMut<Profile>) {
    *profile = Profile::load();
}
pub(crate) fn clear_session(world: &mut World) {
    world.remove_resource::<Runner>();
    world.insert_resource(TrainingSpots::default());
    // Messages can outlive a state transition. Never let the next exercise (or
    // a new career) consume a previous lab's processing or interaction results.
    fn clear<T: Message>(world: &mut World) {
        if let Some(mut messages) = world.get_resource_mut::<Messages<T>>() {
            messages.clear();
        }
    }
    macro_rules! requests {($($t:ty),* $(,)?)=>{$(clear::<FromClient<$t>>(world);)*};}
    requests!(
        crate::interaction::InteractRequested,
        crate::machines::DispenseRequested,
        crate::machines::AnalyzeRequested,
        crate::machines::PackageRequested,
        crate::machines::PurifyRequested,
        crate::machines::BufferTransferRequested,
        crate::machines::AgitateRequested,
        crate::machines::GrindRequested,
        crate::machines::EjectRequested,
        crate::machines::EmptyRequested,
        crate::machines::TakeRequested,
        crate::machines::SetTargetTemperature,
        crate::machines::SetHeaterPower,
        crate::body::ApplyHeldRequested,
        crate::body::ConsumeRequested,
        crate::order_intake::OpenOrderConversation,
        crate::order_intake::AcceptOrder,
        crate::analysis_reports::PrintReportRequested,
        crate::containers::DropRequested,
        crate::containers::SelectInventorySlotRequested,
        crate::knowledge::UnlockAllRequested,
        crate::knowledge::BuyHintRequested
    );
    clear::<DeliveryEvidence>(world);
    clear::<crate::machines::ReactionsFired>(world);
    clear::<crate::knowledge::RecipeDiscovered>(world);
    clear::<crate::orders::OrderResolved>(world);
    clear::<crate::chem_world::ChemicalExposure>(world);
}

fn launch(world: &mut World, id: String) {
    world.remove_resource::<crate::saves::SaveSlot>();
    world.remove_resource::<crate::arc::Campaign>();
    world.remove_resource::<crate::arc::CampaignChoice>();
    world.insert_resource(SessionKind::Training);
    world.insert_resource(crate::net::LaunchMode::Singleplayer);
    world.insert_resource(Runner::new(id.clone()));
    let mut profile = world.resource_mut::<Profile>();
    profile.0.version = 1;
    if id != "free" {
        profile.0.resume = Some(id);
    }
    profile.save();
    world
        .resource_mut::<NextState<AppState>>()
        .set(AppState::Playing);
    world
        .resource_mut::<NextState<crate::menu::MenuScreen>>()
        .set(crate::menu::MenuScreen::Hidden);
}
fn relaunch(world: &mut World) {
    if let Some(next) = world.remove_resource::<Relaunch>() {
        launch(world, next.0);
    }
}
fn restart(world: &mut World, id: String) {
    world.insert_resource(Relaunch(id));
    world
        .resource_mut::<NextState<AppState>>()
        .set(AppState::MainMenu);
}

fn fixture(
    world: &mut World,
    kind: ContainerKind,
    position: Vec3,
    contents: &[(&str, i32)],
    label: Option<&str>,
) -> Entity {
    let db = world.resource::<crate::chem_data::ChemDb>().0.clone();
    let mut state = SystemState::<Commands>::new(world);
    let id = crate::containers::spawn_container(
        &mut state.get_mut(world).expect("Commands are available"),
        kind,
        position,
    );
    state.apply(world);
    let mut solution = Solution::new(kind.capacity());
    for (key, amount) in contents {
        let reagent = db.reagent(key);
        let r = db.reagents.get(reagent);
        let _ = solution.add_profiled(reagent, Units::whole(*amount), 1.0, r.ph);
    }
    world.get_mut::<Container>(id).unwrap().solution = solution;
    if let Some(text) = label {
        world
            .entity_mut(id)
            .insert(crate::labels::Label(text.into()));
    }
    id
}
fn actor(world: &mut World, name: &str, kind: Actor, at: Vec3) -> Entity {
    world
        .spawn((
            crate::crew::CrewMember {
                name: name.into(),
                role: if kind == Actor::Customer {
                    "Training"
                } else {
                    "Medical"
                }
                .into(),
            },
            kind,
            crate::interaction::Interactable::new(name),
            crate::crew::CrewRoute::to(at),
            Transform::from_translation(at),
            crate::body::Body::default(),
            crate::body::Bloodstream::default(),
            crate::until_we_leave_the_lab(),
        ))
        .id()
}
fn spot(world: &World, id: &str) -> Vec3 {
    world
        .resource::<TrainingSpots>()
        .0
        .get(id)
        .expect("validated training marker")
        .translation
}
fn spawn_customer(world: &mut World, runner: &Runner, amount: i32, accepted: bool) {
    let at = spot(world, "customer");
    let id = actor(world, "Practice Customer", Actor::Customer, at);
    let order = crate::orders::Order {
        reagent: world
            .resource::<crate::chem_data::ChemDb>()
            .reagent("kelotane"),
        specific: true,
        minimum_purity: 0.0,
        amount: Units::whole(amount),
        plea: if runner.lesson == "independent" {
            format!("Prepare 20u Kelotane for two customers, in two 10u bottles. Mine is bottle {} of 2. This practice request is untimed.", runner.evidence.deliveries + 1)
        } else {
            format!("Please prepare {amount}u of Kelotane in a bottle. This practice request is untimed.")
        },
        patience: 600.0,
        waited: 0.0,
    };
    let context = crate::order_intake::RequestContext {
        id: runner.epoch,
        source: crate::order_intake::RequestSource::Specific,
        campaign: None,
        greeting: crate::order_intake::GreetingKind::Ordinary,
        step: None,
    };
    if accepted {
        world.entity_mut(id).insert((
            order,
            context,
            crate::order_intake::AcceptedOrder {
                sequence: runner.epoch,
            },
        ));
    } else {
        world
            .entity_mut(id)
            .insert(crate::order_intake::PendingOrder::new(order, context));
    }
}
fn mistake(world: &mut World) -> Entity {
    let id = fixture(
        world,
        ContainerKind::Bottle,
        spot(world, "sample"),
        &[("kelotane", 10), ("silicon", 5)],
        Some("Kelotane"),
    );
    world.entity_mut(id).insert(MistakeSample);
    id
}
fn prepare(world: &mut World) {
    if !world.contains_resource::<crate::lab::MapReady>()
        || !world.contains_resource::<crate::produce::ProduceCatalog>()
    {
        return;
    }
    let Some(mut runner) = world.remove_resource::<Runner>() else {
        return;
    };
    if runner.ready {
        world.insert_resource(runner);
        return;
    }
    let required = [
        "spawn",
        "supplies",
        "instructor",
        "customer",
        "customer_exit",
        "patient",
        "sample",
        "experiment",
        "cleanup",
    ];
    if required
        .iter()
        .any(|id| !world.resource::<TrainingSpots>().0.contains_key(*id))
    {
        world.insert_resource(runner);
        return;
    }
    let machines: Vec<_> = world
        .query::<(Entity, &Machine)>()
        .iter(world)
        .map(|(e, m)| (e, m.kind))
        .collect();
    if machines.len() < 7 {
        world.insert_resource(runner);
        return;
    }
    let start = spot(world, "spawn");
    for mut at in world
        .query_filtered::<&mut Transform, With<LocalPlayer>>()
        .iter_mut(world)
    {
        at.translation = start + Vec3::Y * crate::player::EYE_HEIGHT;
    }
    if world
        .query_filtered::<Entity, With<LocalPlayer>>()
        .iter(world)
        .next()
        .is_none()
    {
        world.insert_resource(runner);
        return;
    }
    let exit = spot(world, "customer_exit");
    world
        .resource_mut::<crate::crew::Departments>()
        .set("Training".into(), exit);
    actor(
        world,
        "Lab Instructor",
        Actor::Instructor,
        spot(world, "instructor"),
    );
    let supplies = spot(world, "supplies");
    for i in 0..6 {
        fixture(
            world,
            if i == 5 {
                ContainerKind::LargeBeaker
            } else {
                ContainerKind::Beaker
            },
            supplies + Vec3::X * (i as f32 * 0.35),
            &[],
            None,
        );
    }
    for i in 0..4 {
        fixture(
            world,
            ContainerKind::PhPaper,
            supplies + Vec3::Z * 0.35 + Vec3::X * (i as f32 * 0.18),
            &[],
            None,
        );
    }
    let patient = actor(
        world,
        "Practice Patient",
        Actor::Patient,
        spot(world, "patient"),
    );
    world
        .get_mut::<crate::body::Body>(patient)
        .unwrap()
        .0
        .damage
        .burn = Units::whole(12);
    let cleanup_mesh = world
        .resource_mut::<Assets<Mesh>>()
        .add(Cuboid::new(0.35, 0.7, 0.35));
    let cleanup_material = world
        .resource_mut::<Assets<StandardMaterial>>()
        .add(Color::srgb(0.2, 0.65, 0.58));
    world.spawn((
        Actor::Cleanup,
        crate::interaction::Interactable {
            label: "Reset spill bay".into(),
        },
        Mesh3d(cleanup_mesh),
        MeshMaterial3d(cleanup_material),
        Transform::from_translation(spot(world, "cleanup")),
        Visibility::default(),
        crate::until_we_leave_the_lab(),
    ));
    match runner.lesson.as_str() {
        "request" => spawn_customer(world, &runner, 10, false),
        "batch" => spawn_customer(world, &runner, 10, true),
        "delivery" => {
            spawn_customer(world, &runner, 10, true);
            fixture(
                world,
                ContainerKind::Beaker,
                spot(world, "sample"),
                &[("kelotane", 10)],
                None,
            );
        }
        "mistake" => {
            mistake(world);
        }
        "independent" => spawn_customer(world, &runner, 10, false),
        "reports" => {
            mistake(world);
        }
        "thermal" => {
            fixture(
                world,
                ContainerKind::Beaker,
                spot(world, "sample"),
                &[("nitrogen", 20)],
                Some("Nitrogen practice sample"),
            );
        }
        "ph" => {
            fixture(
                world,
                ContainerKind::Beaker,
                spot(world, "sample"),
                &[("water", 20)],
                Some("Water practice sample"),
            );
        }
        "extraction" | "free" => {
            let aloe = world
                .resource::<crate::produce::ProduceCatalog>()
                .iter()
                .find(|k| k.name == "Aloe")
                .unwrap()
                .id;
            let mut state = SystemState::<Commands>::new(world);
            let at = spot(world, "sample");
            {
                let mut commands = state.get_mut(world).expect("Commands are available");
                for i in 0..2 {
                    crate::produce::spawn_produce(
                        &mut commands,
                        aloe,
                        at + Vec3::X * (i as f32 * 0.4),
                    );
                }
            }
            state.apply(world);
        }
        "application" => {
            fixture(
                world,
                ContainerKind::Beaker,
                spot(world, "sample"),
                &[("kelotane", 20)],
                None,
            );
        }
        "spills" => {
            let at = spot(world, "experiment");
            fixture(
                world,
                ContainerKind::Beaker,
                at,
                &[("potassium", 1)],
                Some("Potassium - 1u"),
            );
            fixture(
                world,
                ContainerKind::Beaker,
                at + Vec3::X * 0.45,
                &[("water", 1)],
                Some("Water - 1u"),
            );
            fixture(
                world,
                ContainerKind::Beaker,
                at + Vec3::X * 0.9,
                &[("water", 10)],
                Some("Water - spill practice"),
            );
        }
        _ => {}
    }
    runner.evidence.initial = world
        .query::<(Entity, &Container)>()
        .iter(world)
        .map(|(e, _)| e)
        .collect();
    runner.ready = true;
    runner.revision += 1;
    world.insert_resource(runner);
}

fn talk(
    mut requests: MessageReader<FromClient<crate::interaction::InteractRequested>>,
    mut commands: Commands,
    actors: Query<(Entity, &Actor, &Transform)>,
    players: Query<(&Transform, &crate::player::Chemist)>,
    mut runner: ResMut<Runner>,
    lessons: Res<Lessons>,
    puddles: Query<Entity, With<crate::chem_world::ChemicalPuddle>>,
) {
    for request in requests.read() {
        let Ok((id, actor, at)) = actors.get(request.target) else {
            continue;
        };
        if !players.iter().any(|(p, c)| {
            c.client == request.client_id
                && crate::interaction::authority_target_in_reach(
                    p.translation,
                    at.translation,
                    crate::interaction::REACH,
                )
        }) {
            continue;
        }
        match actor {
            Actor::Instructor => {
                runner.evidence.set(Goal::Meet);
                let text=lessons.0.iter().find(|l|l.id==runner.lesson).and_then(|l|l.stages.get(runner.stage)).map(|s|s.explanation.clone()).unwrap_or_else(||"Choose a small question, predict, then measure. Pause offers more lessons and fresh supplies.".into());
                crate::speech::say(&mut commands, id, text, crate::speech::SpeechTone::Neutral);
            }
            Actor::Cleanup => {
                for entity in &puddles {
                    commands.entity(entity).try_despawn();
                }
                if runner.evidence.has(Goal::Residue) {
                    runner.evidence.set(Goal::Clean);
                }
            }
            _ => {}
        }
    }
}

fn clean_amount(solution: &Solution, db: &crate::chem_data::ChemDb, amount: i32) -> bool {
    solution.len() == 1 && solution.volume_of(db.reagent("kelotane")) == Units::whole(amount)
}
fn observe(world: &mut World) {
    let Some(mut runner) = world.remove_resource::<Runner>() else {
        return;
    };
    if !runner.ready || runner.finished {
        world.insert_resource(runner);
        return;
    }
    let db = world.resource::<crate::chem_data::ChemDb>().0.clone();
    let db = crate::chem_data::ChemDb(db);
    let local = world
        .query_filtered::<(
            Entity,
            &InteractionMode,
            &crate::containers::SelectedInventorySlot,
        ), With<LocalPlayer>>()
        .iter(world)
        .next()
        .map(|(e, m, s)| (e, *m, s.0));
    let Some((player, mode, selected)) = local else {
        world.insert_resource(runner);
        return;
    };
    let evidence = &mut runner.evidence;
    if selected != 0 {
        evidence.set(Goal::Select);
    }
    match mode {
        InteractionMode::ReadingBook(_) => evidence.set(Goal::Book),
        InteractionMode::OrderConversation(..) => evidence.set(Goal::Converse),
        InteractionMode::OrderDirectory { .. } => evidence.set(Goal::Directory),
        InteractionMode::Inspecting { item, .. } if evidence.packages.contains(&item) => {
            evidence.set(Goal::Inspect);
        }
        _ => {}
    }
    if world
        .get_resource::<crate::textbook::TextbookView>()
        .is_some_and(|v| !v.visited.is_empty())
    {
        evidence.set(Goal::Textbook);
    }
    if world.query_filtered::<&crate::order_intake::RequestContext,With<crate::order_intake::AcceptedOrder>>().iter(world).any(|c|c.id==runner.epoch){evidence.set(Goal::Accept);}
    let containers: Vec<_> = world
        .query::<(Entity, &Container, Option<&HeldBy>, Option<&InSlot>)>()
        .iter(world)
        .map(|(e, c, h, s)| {
            (
                e,
                Container {
                    kind: c.kind,
                    solution: c.solution.clone(),
                },
                h.map(|h| h.0),
                s.map(|s| s.0),
            )
        })
        .collect();
    for (id, c, held, slot) in &containers {
        let sol = &c.solution;
        if held.is_some_and(|h| h == player) {
            evidence.held.insert(*id);
            evidence.set(Goal::Pickup);
            if evidence.loaded.contains(id) {
                evidence.set(Goal::Retrieve);
            }
        }
        if slot.is_some() {
            evidence.loaded.insert(*id);
            evidence.set(Goal::Load);
        }
        if clean_amount(sol, &db, 10) {
            evidence.set(Goal::Batch10);
            if evidence.has(Goal::AnalyzeMistake) {
                evidence.set(Goal::Repair);
            }
        }
        if clean_amount(sol, &db, 20) {
            evidence.set(Goal::Batch20);
            if evidence.has(Goal::AnalyzeMistake) {
                evidence.set(Goal::Repair);
            }
        }
        if !evidence.initial.contains(id)
            && !sol.is_empty()
            && matches!(
                c.kind,
                ContainerKind::Bottle
                    | ContainerKind::Pill
                    | ContainerKind::Syringe
                    | ContainerKind::Patch
                    | ContainerKind::SprayBottle
            )
        {
            evidence.packages.insert(*id);
            evidence.forms.insert(c.kind);
            evidence.set(Goal::Package);
        }
        if sol.volume_of(db.reagent("nitrogen")).is_positive() && sol.temperature.0 >= 325.0 {
            evidence.warm.insert(*id);
            evidence.set(Goal::Warm);
        }
        if evidence.warm.contains(id) {
            if slot.is_none() {
                evidence.set(Goal::RemovedWarm);
            }
            if sol.temperature.0 < 300.0 {
                evidence.set(Goal::Cool);
            }
        }
        if sol.volume_of(db.reagent("water")).is_positive() && (sol.ph() - 7.0).abs() >= 1.0 {
            evidence.changed_ph.insert(*id);
            evidence.set(Goal::PhShift);
        }
        if evidence.changed_ph.contains(id) && (sol.ph() - 7.0).abs() < 0.3 {
            evidence.set(Goal::PhReturn);
        }
        if matches!(
            c.kind,
            ContainerKind::PhPaperStrongAcid
                | ContainerKind::PhPaperAcid
                | ContainerKind::PhPaperNeutral
                | ContainerKind::PhPaperBase
                | ContainerKind::PhPaperStrongBase
        ) {
            evidence.set(Goal::PhStrip);
        }
        if sol.volume_of(db.reagent("kelotane")).is_positive()
            && sol.volume_of(db.reagent("plant_fibre")).is_positive()
        {
            evidence.set(Goal::Ground);
        }
        if sol.volume_of(db.reagent("ash")).is_positive() {
            evidence.set(Goal::Residue);
        }
    }
    if evidence.forms.len() >= 2 {
        evidence.set(Goal::TwoForms);
    }
    if evidence.has(Goal::Batch10) && evidence.has(Goal::Batch20) {
        evidence.set(Goal::Measured);
    }
    let buffers: Vec<_> = world
        .query::<&crate::machines::Buffer>()
        .iter(world)
        .map(|b| b.0.clone())
        .collect();
    if evidence.has(Goal::AnalyzeMistake)
        && buffers
            .iter()
            .any(|solution| clean_amount(solution, &db, 10) || clean_amount(solution, &db, 20))
    {
        evidence.set(Goal::Repair);
    }
    let portions = containers
        .iter()
        .filter(|(_, c, ..)| c.solution.volume_of(db.reagent("kelotane")).is_positive())
        .count()
        + buffers
            .iter()
            .filter(|b| b.volume_of(db.reagent("kelotane")).is_positive())
            .count();
    if evidence.has(Goal::Measured)
        && portions >= 2
        && buffers
            .iter()
            .any(|b| b.volume_of(db.reagent("kelotane")).is_positive())
    {
        evidence.set(Goal::Divided);
    }
    if evidence.has(Goal::Divided)
        && (buffers.iter().any(|s| clean_amount(s, &db, 30))
            || containers
                .iter()
                .any(|(_, c, ..)| clean_amount(&c.solution, &db, 30)))
    {
        evidence.set(Goal::Recombined);
    }
    if evidence.has(Goal::Ground)
        && buffers
            .iter()
            .any(|s| s.len() == 1 && s.volume_of(db.reagent("kelotane")).is_positive())
    {
        evidence.set(Goal::Extracted);
    }
    for snapshot in world
        .query::<&crate::analysis_reports::AnalyzerSnapshot>()
        .iter(world)
    {
        if evidence.scans.contains_key(&snapshot.report.id) {
            continue;
        }
        if world.get::<MistakeSample>(snapshot.item).is_some()
            && snapshot.report.chemicals.iter().any(|c| c.key == "silicon")
        {
            evidence.set(Goal::AnalyzeMistake);
        }
        if snapshot
            .report
            .chemicals
            .iter()
            .any(|c| c.key == "kelotane")
        {
            evidence.verified.push(snapshot.report.clone());
            evidence.set(Goal::Analyze);
        }
        if evidence.has(Goal::StaleReport) {
            evidence.set(Goal::Reanalyzed);
        }
        evidence.scans.insert(snapshot.report.id, snapshot.clone());
    }
    for (report, inventory) in world
        .query::<(
            &crate::analysis_reports::AnalysisReport,
            Option<&crate::containers::InventorySlot>,
        )>()
        .iter(world)
    {
        if inventory.is_some_and(|slot| slot.owner == player)
            && evidence.scans.contains_key(&report.id)
        {
            evidence.printed.insert(report.id);
            evidence.set(Goal::Printed);
        }
    }
    let stale = evidence.printed.iter().any(|id| {
        evidence.scans.get(id).is_some_and(|s| {
            world
                .get::<Container>(s.item)
                .is_some_and(|c| !s.report.matches(&c.solution, &db))
        })
    });
    if stale {
        evidence.set(Goal::StaleReport);
    }
    if world
        .query::<&crate::machines::HplcReport>()
        .iter(world)
        .any(|r| r.product_amount < r.input_amount)
    {
        evidence.set(Goal::Purified);
    }
    for (actor, body, blood) in world
        .query::<(&Actor, &crate::body::Body, &crate::body::Bloodstream)>()
        .iter(world)
    {
        if *actor == Actor::Patient {
            if blood
                .0
                .blood
                .volume_of(db.reagent("kelotane"))
                .is_positive()
            {
                evidence.set(Goal::Treated);
            }
            if body.0.damage.burn < Units::whole(12) {
                evidence.set(Goal::Treated);
                evidence.set(Goal::Recovered);
            }
        }
    }
    for puddle in world
        .query::<&crate::chem_world::ChemicalPuddle>()
        .iter(world)
    {
        evidence.set(Goal::Puddle);
        if puddle.solution.volume_of(db.reagent("ash")).is_positive() {
            evidence.set(Goal::Residue);
        }
    }
    // Consume only committed delivery records for this exercise generation.
    let deliveries: Vec<_> = world
        .resource_mut::<Messages<DeliveryEvidence>>()
        .drain()
        .collect();
    let mut retry_delivery = false;
    let mut next_customer = false;
    for event in deliveries {
        if event.request != runner.epoch || !evidence.handovers.insert(event.container) {
            continue;
        }
        let wanted = 10;
        let measured_batch = if runner.lesson == "independent" {
            20
        } else {
            10
        };
        if event.outcome.is_good()
            && event.kind == ContainerKind::Bottle
            && clean_amount(&event.actual, &db, wanted)
            && evidence.packages.contains(&event.container)
            // Inspection is taught once. Later bottles, including replacements
            // after that first inspection, can be handed over directly.
            && (runner.lesson != "delivery" || evidence.has(Goal::Inspect))
            && evidence.verified.iter().any(|report| {
                report.chemicals.len() == 1
                    && report.chemicals[0].key == "kelotane"
                    && report.chemicals[0].amount_raw >= Units::whole(measured_batch).raw()
                    && (report.chemicals[0].purity - event.actual.purity_of(db.reagent("kelotane")))
                        .abs()
                        < 0.001
            })
        {
            evidence.deliveries += 1;
            if runner.lesson != "independent" || evidence.deliveries >= 2 {
                evidence.set(Goal::Deliver);
            } else {
                next_customer = true;
            }
        } else {
            #[cfg(debug_assertions)]
            if std::env::args().any(|a| a == "--tutorial-playtest") {
                let _ = std::fs::write("target/tutorial-playtest/delivery-retry.txt", format!("outcome={:?}, kind={:?}, amount={}, clean={}, packaged={}, inspection_taught={}, actual={:?}, reports={:?}", event.outcome, event.kind, event.actual.total_volume(), clean_amount(&event.actual, &db, wanted), evidence.packages.contains(&event.container), evidence.has(Goal::Inspect), event.actual, evidence.verified));
            }
            retry_delivery = true;
        }
    }
    let reading = world.resource::<crate::settings::Paused>().0
        || matches!(
            mode,
            InteractionMode::ReadingBook(_)
                | InteractionMode::Inspecting { .. }
                | InteractionMode::OrderDirectory { .. }
                | InteractionMode::OrderConversation(..)
        );
    let heating = containers.iter().any(|(_, c, _, slot)| {
        slot.is_some_and(|machine| {
            world
                .get::<crate::machines::Thermostat>(machine)
                .is_some_and(|t| t.powered && (t.target.0 - c.solution.temperature.0).abs() > 1.0)
        })
    });
    let processing = heating
        || world
            .query::<&crate::machines::AgitationRun>()
            .iter(world)
            .next()
            .is_some()
        || containers
            .iter()
            .any(|(_, c, ..)| chem_sim::is_reacting(&c.solution, &db.reactions));
    if evidence.goals.len() != runner.last_goal_count {
        runner.last_goal_count = evidence.goals.len();
        runner.idle = 0.0;
    } else if !reading && !processing {
        runner.idle += world.resource::<Time>().delta_secs();
    }
    if runner.idle >= 45.0 && !runner.offered {
        runner.offered = true;
        runner.revision += 1;
    }
    if retry_delivery || next_customer {
        spawn_customer(world, &runner, 10, retry_delivery);
    }
    world.insert_resource(runner);
}

fn advance(world: &mut World) {
    let Some(mut runner) = world.remove_resource::<Runner>() else {
        return;
    };
    if !runner.ready || runner.finished || runner.lesson == "free" {
        world.insert_resource(runner);
        return;
    }
    let lesson = world
        .resource::<Lessons>()
        .0
        .iter()
        .find(|l| l.id == runner.lesson)
        .cloned()
        .unwrap();
    while lesson
        .stages
        .get(runner.stage)
        .is_some_and(|s| runner.evidence.has(s.goal))
    {
        runner.stage += 1;
        runner.hint = 0;
        runner.idle = 0.0;
        runner.offered = false;
        runner.revision += 1;
    }
    if runner.stage == lesson.stages.len() {
        let next = if lesson.core {
            world
                .resource::<Lessons>()
                .0
                .iter()
                .filter(|l| l.core)
                .skip_while(|l| l.id != lesson.id)
                .nth(1)
                .map(|l| l.id.clone())
        } else {
            None
        };
        {
            let mut profile = world.resource_mut::<Profile>();
            profile.0.completed.insert(lesson.id.clone());
            profile.0.skipped.remove(&lesson.id);
            profile.0.resume = next.clone();
            profile.save();
        }
        if let Some(next) = next {
            runner.lesson = next.clone();
            runner.stage = 0;
            runner.evidence = default();
            runner.evidence.initial = world
                .query::<(Entity, &Container)>()
                .iter(world)
                .map(|(e, _)| e)
                .collect();
            runner.epoch = rand::random::<u64>() | TRAINING_REQUEST_BIT;
            // Keep the accepted first request's identity through preparation and delivery.
            for mut context in world
                .query::<&mut crate::order_intake::RequestContext>()
                .iter_mut(world)
            {
                context.id = runner.epoch;
            }
            match next.as_str() {
                "request" => spawn_customer(world, &runner, 10, false),
                "mistake" => {
                    mistake(world);
                }
                "independent" => spawn_customer(world, &runner, 10, false),
                _ => {}
            }
            runner.evidence.initial = world
                .query::<(Entity, &Container)>()
                .iter(world)
                .map(|(e, _)| e)
                .collect();
            runner.evidence.scans = world
                .query::<&crate::analysis_reports::AnalyzerSnapshot>()
                .iter(world)
                .map(|s| (s.report.id, s.clone()))
                .collect();
        } else {
            runner.finished = true;
        }
        runner.revision += 1;
    }
    if let Some(stage) = world
        .resource::<Lessons>()
        .0
        .iter()
        .find(|l| l.id == runner.lesson)
        .and_then(|l| l.stages.get(runner.stage))
        .cloned()
    {
        if runner.last_explanation != stage.explanation && runner.lesson != "independent" {
            runner.last_explanation = stage.explanation.clone();
            let instructor = world
                .query::<(Entity, &Actor)>()
                .iter(world)
                .find(|(_, a)| **a == Actor::Instructor)
                .map(|(e, _)| e);
            if let Some(id) = instructor {
                let mut state = SystemState::<Commands>::new(world);
                crate::speech::say(
                    &mut state.get_mut(world).expect("Commands are available"),
                    id,
                    stage.explanation,
                    crate::speech::SpeechTone::Neutral,
                );
                state.apply(world);
            }
        }
    }
    world.insert_resource(runner);
}
