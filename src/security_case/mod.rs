//! Recorded Security custody and evidence-based, in-person appeals.
#[cfg(debug_assertions)]
mod playtest;
pub(crate) mod preparation;
#[cfg(test)]
mod tests;
mod ui;
use crate::{
    analysis_reports::{AnalysisReport, SampleId},
    body::{Bloodstream, Body},
    chem_data::ChemDb,
    containers::{
        Container, ContainerKind, HeldBy, InSlot, InSlotB, InSlotC, InventorySlot, Stored,
    },
    crew::{Ambient, CrewMember, CrewRoute},
    interaction::{InteractRequested, Interactable},
    order_intake::{
        AwaitingConversation, Intake, OpenOrderConversation, PendingOrder, RequestSource,
    },
    orders::{Order, OrderKind, OrderResolved, Shift},
    radio::{RadioEntry, RadioLog},
    social::{NpcCommitment, SocialState},
    AppState,
};
use bevy::{
    ecs::{entity::MapEntities, system::SystemParam},
    prelude::*,
};
use bevy_replicon::prelude::*;
use chem_sim::Units;
use serde::{Deserialize, Serialize};

const REYES: &str = crate::social::REYES;
const BEX: &str = "Warden Bex";
const GROUNDS: &str = "The composition of this customer's finished batch is disputed. Security is requesting a recorded temporary hold pending laboratory verification.";

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseSample(pub u64);
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaseCustody(pub u64);
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderHold(pub u64);
#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct CaseLocker;
#[derive(Component)]
struct CaseOfficer;
#[derive(Component)]
struct CaseWarden;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stage {
    Greeting,
    Notice,
    Inspecting,
    Custody,
    Released,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SecurityCase {
    pub id: u64,
    pub stage: Stage,
    pub customer: String,
    pub explanation: String,
    pub requirements: String,
    #[serde(default)]
    pub batch_description: String,
    pub batch: u64,
    pub sample: u64,
    pub original: AnalysisReport,
    pub reference: Option<AnalysisReport>,
    pub notice_left: f32,
    pub created_at: f64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CaseRecord {
    pub id: u64,
    pub customer: String,
    pub result: String,
}
#[derive(Resource, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SecurityCaseState {
    pub ordinary_deliveries: u32,
    pub cooperative_holds: u32,
    pub refusals: u32,
    pub complaints: u32,
    pub cooldown: f32,
    pub active: Option<SecurityCase>,
    pub history: Vec<CaseRecord>,
}
impl Default for SecurityCaseState {
    fn default() -> Self {
        Self {
            ordinary_deliveries: 0,
            cooperative_holds: 0,
            refusals: 0,
            complaints: 0,
            cooldown: 600.0,
            active: None,
            history: Vec::new(),
        }
    }
}
#[derive(Resource, Default)]
struct Runtime {
    customer: Option<Entity>,
    requester: Option<Entity>,
    target: Option<Entity>,
    initiator: Option<Entity>,
    inspection_target: Option<Vec3>,
    route_age: f32,
    retry: Option<Timer>,
}
#[derive(Component, Default)]
struct HeardCase(std::collections::HashSet<(u64, Entity)>);

#[derive(Resource, Message, Default, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SecurityCaseSummary {
    pub case: Option<u64>,
    pub stage: Option<Stage>,
    pub text: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaseAction {
    Grounds,
    RecordHold,
    Refuse,
    PresentReport,
    VerifyCustody,
    Collect,
    Abandon,
}
#[derive(Message, Clone, Serialize, Deserialize, MapEntities)]
pub struct CaseActionRequested {
    #[entities]
    pub target: Entity,
    pub case: u64,
    pub action: CaseAction,
}
#[derive(Message, Clone, Serialize, Deserialize, MapEntities)]
pub struct CaseConversationOpened {
    #[entities]
    pub target: Entity,
    pub case: u64,
    pub title: String,
    pub text: String,
    pub stage: Stage,
    pub actions: Vec<(CaseAction, String)>,
}
pub struct SecurityCasePlugin;
impl Plugin for SecurityCasePlugin {
    fn build(&self, app: &mut App) {
        #[cfg(debug_assertions)]
        if std::env::args().any(|arg| arg == "--security-case-playtest") {
            playtest::install(app);
        }
        preparation::install(app);
        app.init_resource::<SecurityCaseState>()
            .init_resource::<Runtime>()
            .init_resource::<SecurityCaseSummary>()
            .replicate::<CaseSample>()
            .replicate::<CaseCustody>()
            .replicate::<CaseLocker>()
            .replicate::<OrderHold>()
            .add_mapped_client_message::<CaseActionRequested>(Channel::Ordered)
            .add_mapped_server_message::<CaseConversationOpened>(Channel::Ordered)
            .add_server_message::<SecurityCaseSummary>(Channel::Ordered)
            .add_systems(OnExit(AppState::Playing), clear_runtime)
            .add_systems(
                Update,
                (
                    ensure_locker,
                    count_deliveries,
                    schedule,
                    open,
                    actions,
                    advance,
                    reconcile,
                    broadcast,
                )
                    .chain()
                    .before(crate::orders::expire_orders)
                    .run_if(crate::net::is_authority)
                    .run_if(in_state(AppState::Playing))
                    .run_if(crate::session::career_session),
            )
            .add_systems(
                Update,
                receive_summary
                    .run_if(in_state(AppState::Playing))
                    .run_if(crate::session::career_session),
            )
            .add_plugins(ui::SecurityCaseUiPlugin);
    }
}

fn clear_runtime(mut runtime: ResMut<Runtime>, mut summary: ResMut<SecurityCaseSummary>) {
    *runtime = Runtime::default();
    *summary = SecurityCaseSummary::default();
}
fn count_deliveries(
    mut requests: MessageReader<OrderResolved>,
    mut state: ResMut<SecurityCaseState>,
) {
    for request in requests.read() {
        if request.kind == OrderKind::Normal
            && request.outcome.is_good()
            && !request.development
            && request.campaign.is_none()
        {
            state.ordinary_deliveries = state.ordinary_deliveries.saturating_add(1);
        }
    }
}
fn interval() -> f32 {
    rand::random_range(480.0..=720.0)
}
fn close_case(state: &mut SecurityCaseState, result: &str, upheld: bool) {
    if let Some(case) = state.active.take() {
        if let Some(record) = state.history.iter_mut().find(|record| record.id == case.id) {
            record.result = result.into();
        } else {
            state.history.push(CaseRecord {
                id: case.id,
                customer: case.customer,
                result: result.into(),
            });
        }
    }
    state.cooldown = if upheld { 1200.0 } else { interval() };
}
fn say(commands: &mut Commands, radio: &mut RadioLog, npc: Entity, speaker: &str, line: &str) {
    crate::speech::say(commands, npc, line, crate::speech::SpeechTone::Neutral);
    radio.push(RadioEntry::new(crate::radio::RadioChannel::Security, line).speaker(speaker));
}
fn ensure_locker(
    mut commands: Commands,
    lockers: Query<Entity, With<CaseLocker>>,
    posts: Res<crate::crew::CrewPosts>,
    nav: Res<crate::nav::NavGraph>,
) {
    if !lockers.is_empty() {
        return;
    }
    let Some(post) = posts.work(BEX) else {
        return;
    };
    let floor = nav.standable_goal(post + Vec3::X * 1.2);
    commands.spawn((
        Replicated,
        CaseLocker,
        Interactable::new("Security evidence locker"),
        Transform::from_translation(floor + Vec3::Y * 0.45),
        Visibility::default(),
        crate::until_we_leave_the_lab(),
    ));
}

/// Whole batch suitability, independent of its player-written label.
fn eligible(container: &Container, order: &Order, db: &ChemDb) -> bool {
    order.remaining() >= 90.0
        && container.solution.total_volume() > Units::whole(1)
        && !chem_sim::is_reacting(&container.solution, &db.reactions)
        && container.solution.iter().all(|(r, _)| {
            !db.reagents.get(r).controlled
                && db.reagents.get(r).explosive.is_none()
                && !db
                    .reagents
                    .get(r)
                    .categories
                    .contains(&chem_sim::Category::Illicit)
        })
        && crate::orders::grade(
            crate::orders::wanted_for(order, OrderKind::Normal, db),
            order.amount,
            &container.solution,
            container.kind,
            db,
        )
        .0
        .is_good()
        && container.solution.average_purity() >= order.minimum_purity
}

#[derive(SystemParam)]
struct Advancing<'w, 's> {
    db: Res<'w, ChemDb>,
    items: Query<
        'w,
        's,
        (
            Entity,
            &'static mut Container,
            &'static Transform,
            Option<&'static SampleId>,
        ),
        (
            Without<HeldBy>,
            Without<InventorySlot>,
            Without<Stored>,
            Without<InSlot>,
            Without<InSlotB>,
            Without<InSlotC>,
        ),
    >,
    officers: Query<
        'w,
        's,
        (
            &'static mut CrewRoute,
            &'static Transform,
            &'static Body,
            &'static Bloodstream,
            Has<PendingOrder>,
        ),
        With<CaseOfficer>,
    >,
    orders: Query<'w, 's, &'static Order>,
    lockers: Query<'w, 's, (Entity, &'static Transform), With<CaseLocker>>,
    solids: Query<'w, 's, (&'static Transform, &'static crate::lab::Solid)>,
    inventory: Query<'w, 's, &'static InventorySlot>,
    selected: Query<'w, 's, &'static crate::containers::SelectedInventorySlot>,
    held: Query<'w, 's, &'static HeldBy>,
}
fn advance(
    mut commands: Commands,
    time: Res<Time>,
    mut state: ResMut<SecurityCaseState>,
    mut runtime: ResMut<Runtime>,
    mut world: Advancing,
    mut radio: ResMut<RadioLog>,
) {
    let Some(case) = state.active.as_ref().cloned() else {
        return;
    };
    if matches!(case.stage, Stage::Custody | Stage::Released) {
        return;
    }
    let Some(officer) = runtime.requester else {
        close_case(
            &mut state,
            "Uncompleted approach withdrawn after session change",
            false,
        );
        return;
    };
    let Ok((mut route, position, body, blood, pending)) = world.officers.get_mut(officer) else {
        close_case(
            &mut state,
            "Officer unavailable; proposed hold withdrawn",
            false,
        );
        return;
    };
    let withdrawn = if body.0.collapsed || blood.0.incapacitated() {
        Some("Officer incapacitated; proposed hold withdrawn")
    } else if route.routing_failed() {
        Some("Officer could not reach the inspection; proposed hold withdrawn")
    } else if case.stage == Stage::Greeting && !pending {
        Some("Greeting reservation ended; proposed hold withdrawn")
    } else {
        None
    };
    if let Some(reason) = withdrawn {
        debug!(
            "Security case {} withdrew: {reason}; at {:?}, route {:?}, moving {}, pending {}",
            case.id,
            position.translation,
            route.phase,
            route.is_moving(),
            pending
        );
        close_case(&mut state, reason, false);
        return;
    }
    if case.stage == Stage::Greeting {
        return;
    }
    if case.stage == Stage::Notice {
        let active = state.active.as_mut().unwrap();
        active.notice_left = (active.notice_left - time.delta_secs()).max(0.0);
        if active.notice_left > 0.0 {
            return;
        }
        active.stage = Stage::Inspecting;
    }
    runtime.route_age += time.delta_secs();
    let candidate = runtime
        .target
        .and_then(|item| world.items.get_mut(item).ok());
    let Some((target, mut container, at, sample_id)) = candidate else {
        close_case(
            &mut state,
            "Batch no longer accessible; proposed hold withdrawn",
            false,
        );
        return;
    };
    let order = runtime
        .customer
        .and_then(|customer| world.orders.get(customer).ok());
    if !sample_id.is_some_and(|id| id.0 == case.batch)
        || !case.original.matches(&container.solution, &world.db)
        || order.is_none()
        || runtime.route_age > 120.0
    {
        close_case(
            &mut state,
            "Batch or request changed; proposed hold withdrawn",
            false,
        );
        return;
    }
    let hand = position.translation + Vec3::Y * 0.65;
    if position.translation.xz().distance(at.translation.xz()) > 1.85
        || (hand.y - at.translation.y).abs() > 1.1
        || world.solids.iter().any(|(solid_at, solid)| {
            crate::interaction::authority_segment_blocked(
                hand,
                at.translation,
                solid_at.translation,
                solid.half_extents,
            )
        })
    {
        return;
    }
    let Some((locker, locker_at)) = world.lockers.iter().next() else {
        return;
    };
    let mut retained = container.solution.split(Units::whole(1));
    retained.set_max_volume(ContainerKind::Bottle.capacity());
    let reference = AnalysisReport::measure(
        &retained,
        &world.db,
        case.sample,
        time.elapsed_secs_f64(),
        Some(case.id),
    );
    let sample = crate::containers::spawn_container(
        &mut commands,
        ContainerKind::Bottle,
        at.translation + Vec3::X * 0.25,
    );
    commands.entity(sample).insert((
        Container {
            kind: ContainerKind::Bottle,
            solution: retained,
        },
        SampleId(case.sample),
        CaseSample(case.id),
        crate::labels::Label(format!("Reference sample — case {:016X}", case.id)),
    ));
    if let Some(player) = runtime.initiator {
        let preferred = world.selected.get(player).map_or(0, |s| s.0);
        let occupied_hand = if world.held.iter().any(|h| h.0 == player) {
            vec![(player, preferred)]
        } else {
            vec![]
        };
        if let Some(slot) = crate::containers::free_inventory_slot(
            player,
            preferred,
            &world.inventory,
            &occupied_hand,
        ) {
            commands.entity(sample).insert(InventorySlot {
                owner: player,
                slot,
            });
            if slot == preferred {
                commands.entity(sample).insert(HeldBy(player));
            }
        }
    }
    commands.entity(target).insert((
        CaseCustody(case.id),
        Stored(locker),
        Transform::from_translation(locker_at.translation),
        Visibility::Hidden,
    ));
    if let Some(customer) = runtime.customer {
        commands.entity(customer).insert(OrderHold(case.id));
    }
    let active = state.active.as_mut().unwrap();
    active.reference = Some(reference);
    active.stage = Stage::Custody;
    route.leave();
    say(&mut commands,&mut radio,officer,REYES,"The batch is in recorded Security custody. You have a 1u reference sample. That customer's clock is paused. Analyze the sample and bring the printed report to Warden Bex at Security.");
}

#[derive(SystemParam)]
struct ReconcileWorld<'w, 's> {
    posts: Res<'w, crate::crew::CrewPosts>,
    lockers: Query<'w, 's, (Entity, &'static Transform), With<CaseLocker>>,
    custody: Query<'w, 's, (Entity, &'static CaseCustody, Option<&'static Stored>)>,
    officers: Query<'w, 's, (Entity, &'static mut CrewRoute), With<CaseOfficer>>,
    wardens:
        Query<'w, 's, (Entity, &'static mut CrewRoute), (With<CaseWarden>, Without<CaseOfficer>)>,
    available: Query<
        'w,
        's,
        (
            Entity,
            &'static CrewMember,
            &'static Body,
            &'static Bloodstream,
        ),
        (With<Ambient>, Without<NpcCommitment>),
    >,
    holds: Query<'w, 's, (Entity, &'static OrderHold, Has<Order>)>,
}
fn reconcile(
    mut commands: Commands,
    mut state: ResMut<SecurityCaseState>,
    mut runtime: ResMut<Runtime>,
    mut world: ReconcileWorld,
) {
    let case = state.active.as_ref();
    let custody_phase =
        case.is_some_and(|case| matches!(case.stage, Stage::Custody | Stage::Released));
    if case.is_some() && world.wardens.is_empty() {
        if let Some((entity, ..)) = world.available.iter().find(|(_, member, body, blood)| {
            member.name == BEX && !body.0.collapsed && !blood.0.incapacitated()
        }) {
            if let Some(post) = world.posts.work(BEX) {
                commands.entity(entity).remove::<Ambient>().insert((
                    CaseWarden,
                    NpcCommitment,
                    crate::crew::ReturnsToDuty,
                    CrewRoute::to(post),
                ));
            }
        }
    }
    if let Some(case) = case.filter(|_| custody_phase) {
        let id = case.id;
        if let Some((locker, at)) = world.lockers.iter().next() {
            let mut found = false;
            for (item, custody, stored) in &world.custody {
                if custody.0 != id {
                    continue;
                }
                found = true;
                if stored.is_none_or(|stored| stored.0 != locker) {
                    commands.entity(item).insert((
                        Stored(locker),
                        Transform::from_translation(at.translation),
                        Visibility::Hidden,
                    ));
                }
            }
            if !found {
                close_case(&mut state, "Custody unavailable; order hold released", true);
            }
        }
    }
    // Legitimate raid custody is preserved under zero, outside a corruption case.
    if let Some((locker, at)) = world.lockers.iter().next() {
        for (item, custody, stored) in &world.custody {
            if custody.0 == 0 && stored.is_none_or(|s| s.0 != locker) {
                commands.entity(item).insert((
                    Stored(locker),
                    Transform::from_translation(at.translation),
                    Visibility::Hidden,
                ));
            }
        }
    }
    let live = state.active.as_ref().map(|case| case.id);
    for (entity, hold, has_order) in &world.holds {
        if !has_order || Some(hold.0) != live {
            commands.entity(entity).remove::<OrderHold>();
        }
    }
    if state.active.is_none() {
        for (entity, mut route) in &mut world.officers {
            commands.entity(entity).remove::<(
                CaseOfficer,
                PendingOrder,
                AwaitingConversation,
                crate::order_intake::queue::QueuePosition,
            )>();
            route.leave();
        }
        for (entity, mut route) in &mut world.wardens {
            commands.entity(entity).remove::<CaseWarden>();
            route.leave();
        }
        if runtime.requester.is_some() || runtime.customer.is_some() {
            *runtime = Runtime::default();
        }
    } else if custody_phase {
        for (entity, _) in &world.officers {
            commands.entity(entity).remove::<CaseOfficer>();
        }
    }
}
fn broadcast(
    time: Res<Time>,
    state: Res<SecurityCaseState>,
    mut timer: Local<f32>,
    mut messages: MessageWriter<ToClients<SecurityCaseSummary>>,
) {
    *timer -= time.delta_secs();
    if !state.is_changed() && *timer > 0.0 {
        return;
    }
    *timer = 2.0;
    let summary = state.active.as_ref().map_or_else(SecurityCaseSummary::default,|case| SecurityCaseSummary {
        case: Some(case.id), stage: Some(case.stage), text: format!("SECURITY CASE {:016X}\n{}\n{}",case.id,case.customer,match case.stage {
            Stage::Greeting => "Reyes is waiting to discuss a batch at the window.",
            Stage::Notice => "Inspection notice given. Reyes must reach the specified batch.",
            Stage::Inspecting => "Reyes is inspecting the specified batch in person.",
            Stage::Custody => "Batch in recorded custody. Analyze the retained sample; bring its printed report to Bex at Security.",
            Stage::Released => "Complaint upheld. Collect the released batch from the Security evidence locker.",
        }),
    });
    messages.write(ToClients {
        targets: SendTargets::All,
        message: summary,
    });
}
fn receive_summary(
    mut messages: MessageReader<SecurityCaseSummary>,
    mut state: ResMut<SecurityCaseSummary>,
) {
    for message in messages.read() {
        if *state != *message {
            *state = message.clone();
        }
    }
}

#[derive(SystemParam)]
struct Scheduling<'w, 's> {
    prepared: Res<'w, preparation::PreparedBatches>,
    labels: Query<'w, 's, &'static crate::labels::Label>,
    db: Res<'w, ChemDb>,
    social: Res<'w, SocialState>,
    shift: Res<'w, Shift>,
    nav: Res<'w, crate::nav::NavGraph>,
    areas: Res<'w, crate::lab::WalkableAreas>,
    posts: Res<'w, crate::crew::CrewPosts>,
    solids: Query<'w, 's, (&'static Transform, &'static crate::lab::Solid)>,
    orders: Query<
        'w,
        's,
        (
            Entity,
            &'static CrewMember,
            &'static Order,
            Option<&'static crate::order_intake::RequestContext>,
        ),
        (
            With<crate::order_intake::AcceptedOrder>,
            Without<crate::orders::CrisisOrder>,
            Without<crate::orders::CounterOrder>,
            Without<crate::orders::HostileOrder>,
            Without<crate::orders::IllicitOrder>,
            Without<crate::orders::DevelopmentOrder>,
        ),
    >,
    items: Query<
        'w,
        's,
        (Entity, &'static Container, &'static Transform),
        (
            Without<HeldBy>,
            Without<InventorySlot>,
            Without<Stored>,
            Without<InSlot>,
            Without<InSlotB>,
            Without<InSlotC>,
            Without<CaseCustody>,
        ),
    >,
    residents: crate::crew::AvailableResidents<'w, 's>,
}
fn bench_access(position: Vec3, s: &Scheduling) -> Option<Vec3> {
    // Require a real supporting worktop in Chemistry, not the floor or a remote room.
    let in_lab = s
        .areas
        .room_at(position)
        .is_some_and(|name| matches!(name, "Chemistry" | "Mixing Hall" | "Reaction Bay"));
    if !in_lab
        || !s.solids.iter().any(|(at, solid)| {
            let top = at.translation.y + solid.half_extents.y;
            top > 0.35
                && top < 1.6
                && (position.y - top).abs() < 0.5
                && (position.x - at.translation.x).abs() <= solid.half_extents.x + 0.04
                && (position.z - at.translation.z).abs() <= solid.half_extents.z + 0.04
        })
    {
        return None;
    }
    let goal = s.nav.standable_goal(position);
    ((goal.xz() - position.xz()).length() <= 1.75).then_some(goal)
}

fn schedule(
    mut commands: Commands,
    time: Res<Time>,
    mut state: ResMut<SecurityCaseState>,
    mut runtime: ResMut<Runtime>,
    mut intake: Intake,
    mut s: Scheduling,
) {
    if state.active.is_some()
        || !s.shift.accepting_orders
        || state.ordinary_deliveries < 3
        || !s
            .social
            .selected(crate::social::ResidentAntagonist::ReyesBentGuard)
        || !s
            .social
            .threat_runs(crate::social::ResidentAntagonist::ReyesBentGuard)
    {
        return;
    }
    if state.cooldown > 0.0 {
        state.cooldown = (state.cooldown - time.delta_secs()).max(0.0);
        return;
    }
    if runtime.retry.is_none() {
        runtime.retry = Some(Timer::from_seconds(0.5, TimerMode::Once));
    }
    let retry = runtime.retry.as_mut().unwrap();
    retry.tick(time.delta());
    if !retry.is_finished() || !intake.available(REYES) || !intake.available(BEX) {
        return;
    }
    let Some(reyes_at) = s
        .residents
        .iter()
        .find(|(_, member, body, blood, ..)| {
            member.name == REYES && !body.0.collapsed && !blood.0.incapacitated()
        })
        .map(|(entity, ..)| entity)
    else {
        return;
    };
    let chosen = s
        .orders
        .iter()
        .filter(|(_, _, _, context)| {
            context.is_some_and(|c| c.source == RequestSource::Ordinary && c.campaign.is_none())
        })
        .find_map(|(customer, member, order, context)| {
            s.items.iter().find_map(|(item, container, at)| {
                let prepared = s.prepared.get(item, &container.solution)?;
                if context.is_none_or(|c| c.id != prepared.request) {
                    return None;
                }
                let goal = bench_access(at.translation, &s)?;
                eligible(container, order, &s.db).then_some((
                    customer,
                    member.name.clone(),
                    order.clone(),
                    item,
                    at.translation,
                    goal,
                    AnalysisReport::measure(
                        &container.solution,
                        &s.db,
                        0,
                        time.elapsed_secs_f64(),
                        None,
                    ),
                ))
            })
        });
    let Some((customer, name, order, target, position, goal, mut original)) = chosen else {
        return;
    };
    let form = s
        .items
        .get(target)
        .map(|(_, c, _)| c.kind.label())
        .unwrap_or("Container");
    let claim = s
        .labels
        .get(target)
        .map(|label| format!(" labeled ‘{}’", label.0))
        .unwrap_or_else(|_| " without a label".into());
    let batch_description = format!(
        "{form}{claim}, on the {} worktop ({:.1}, {:.1}).",
        s.areas.room_at(position).unwrap_or("Chemistry"),
        position.x,
        position.z
    );
    let Some(context) = intake.admit(RequestSource::Security, REYES, retry, false) else {
        return;
    };
    let Some(officer) = crate::crew::recall_resident_for_order(
        &mut commands,
        &mut s.residents,
        REYES,
        "Security",
        0.0,
    ) else {
        return;
    };
    if let Some(post) = s.posts.work(BEX) {
        if let Some(warden) = crate::crew::recall_resident_for_order(
            &mut commands,
            &mut s.residents,
            BEX,
            "Security",
            0.0,
        ) {
            commands
                .entity(warden)
                .insert((CaseWarden, CrewRoute::to(post)));
        }
    }
    debug_assert_eq!(officer, reyes_at);
    let id = rand::random();
    let batch = rand::random();
    let sample = rand::random();
    original.sample = batch;
    original.case = Some(id);
    let pending = PendingOrder::new(order.clone(), context);
    commands.entity(officer).insert((
        pending,
        CaseOfficer,
        Interactable::new("Officer Reyes — waiting to speak"),
    ));
    commands.entity(target).insert(SampleId(batch));
    state.active = Some(SecurityCase {
        id,
        stage: Stage::Greeting,
        customer: name,
        explanation: order.plea.clone(),
        requirements: crate::order_intake::requirements(&order, &s.db),
        batch_description,
        batch,
        sample,
        original,
        reference: None,
        notice_left: 0.0,
        created_at: time.elapsed_secs_f64(),
    });
    runtime.customer = Some(customer);
    runtime.target = Some(target);
    runtime.requester = Some(officer);
    runtime.inspection_target = Some(goal);
    runtime.route_age = 0.0;
    runtime.retry = None;
}

fn menu(
    case: &SecurityCase,
    target: Entity,
    warden: bool,
    locker: bool,
    recovery: bool,
) -> CaseConversationOpened {
    let mut actions = vec![];
    let text = match case.stage {
        Stage::Greeting if !warden && !locker => {
            actions.extend([(CaseAction::Grounds, "Explain the grounds".into()), (CaseAction::RecordHold, "Record the hold and retain a sample".into()),
                (CaseAction::Refuse, "Refuse — 45s inspection notice".into())]);
            format!("{GROUNDS}\n\nBatch {:016X}: {}\n\nCustomer: {}\n{}\n\n{}\n\nNo batch is taken until a hold is recorded or an announced inspection occurs. Refusal may lead to a 45-second inspection notice.",
                case.batch,case.batch_description,case.customer, case.requirements, case.explanation)
        }
        Stage::Notice | Stage::Inspecting => "The inspection has been announced. Reyes must reach the same unchanged batch. The customer clock continues until actual seizure.".into(),
        Stage::Custody => {
            if warden || recovery {
                actions.extend([(CaseAction::PresentReport, "Present the reference sample report".into()),
                    (CaseAction::VerifyCustody, "Verify the preserved batch here".into())]);
            }
            actions.push((CaseAction::Abandon, "Abandon claim and forfeit the held batch".into()));
            format!("Case {:016X}\nCustomer: {}\n{}\n\nRecorded grounds: {GROUNDS}\n\nAny outstanding order associated with this hold is paused. Bring the reference sample's analyzer report to Bex at Security. If the sample is missing or altered, Bex can examine the preserved batch.", case.id, case.customer, case.requirements)
        }
        Stage::Released => {
            actions.push((CaseAction::Collect, "Collect released batch".into()));
            "The complaint is upheld. Collect the batch to resume the customer's clock. An unchanged reference sample in your inventory is returned to the batch automatically.".into()
        }
        _ => "Speak to Reyes at the greeting window about this proposed hold.".into(),
    };
    CaseConversationOpened {
        target,
        case: case.id,
        stage: case.stage,
        title: if locker {
            "Security evidence locker"
        } else if warden {
            BEX
        } else {
            REYES
        }
        .into(),
        text,
        actions,
    }
}
#[allow(clippy::too_many_arguments)]
fn open(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<OpenOrderConversation>>,
    mut interactions: MessageReader<FromClient<InteractRequested>>,
    access: crate::order_intake::ConversationAccess,
    state: Res<SecurityCaseState>,
    pending: Query<(&PendingOrder, &AwaitingConversation)>,
    targets: Query<(Option<&CrewMember>, Has<CaseLocker>)>,
    mut heard: Query<&mut HeardCase>,
    wardens: Query<(&Body, &Bloodstream, &Transform), With<CaseWarden>>,
    lockers: Query<&Transform, With<CaseLocker>>,
    mut replies: MessageWriter<ToClients<CaseConversationOpened>>,
) {
    let Some(case) = &state.active else {
        return;
    };
    let recovery = wardens.iter().all(|(body, blood, at)| {
        body.0.collapsed
            || blood.0.incapacitated()
            || lockers
                .iter()
                .all(|l| l.translation.distance(at.translation) > 5.0)
    });
    let mut candidates = Vec::new();
    for request in requests.read() {
        if case.stage == Stage::Greeting
            && pending.get(request.target).is_ok_and(|(p, v)| {
                p.context.source == RequestSource::Security
                    && p.context.id == request.id
                    && v.arrived
            })
        {
            candidates.push((request.client_id, request.target));
        }
    }
    for request in interactions.read() {
        if targets
            .get(request.target)
            .is_ok_and(|(m, locker)| locker || m.is_some_and(|m| m.name == BEX))
        {
            candidates.push((request.client_id, request.target));
        }
    }
    for (client, target) in candidates {
        let Some(player) = access.player(client, target) else {
            continue;
        };
        let Ok((member, locker)) = targets.get(target) else {
            continue;
        };
        let warden = member.is_some_and(|member| member.name == BEX);
        if warden && !matches!(case.stage, Stage::Custody | Stage::Released) {
            continue;
        }
        if let Ok(mut history) = heard.get_mut(player) {
            history.0.insert((case.id, target));
        } else {
            commands
                .entity(player)
                .insert(HeardCase(std::collections::HashSet::from([(
                    case.id, target,
                )])));
        }
        replies.write(ToClients {
            targets: SendTargets::Single(client),
            message: menu(case, target, warden, locker, recovery),
        });
    }
}

#[derive(SystemParam)]
struct ActionWorld<'w, 's> {
    db: Res<'w, ChemDb>,
    nav: Res<'w, crate::nav::NavGraph>,
    access: crate::order_intake::ConversationAccess<'w, 's>,
    heard: Query<'w, 's, &'static mut HeardCase>,
    targets: Query<'w, 's, (Option<&'static CrewMember>, Has<CaseLocker>)>,
    items: Query<
        'w,
        's,
        (
            Entity,
            &'static mut Container,
            Option<&'static SampleId>,
            Option<&'static CaseCustody>,
            Option<&'static InventorySlot>,
            Option<&'static HeldBy>,
        ),
    >,
    papers: Query<'w, 's, (&'static AnalysisReport, &'static InventorySlot)>,
    wardens:
        Query<'w, 's, (&'static Body, &'static Bloodstream, &'static Transform), With<CaseWarden>>,
    lockers: Query<'w, 's, (Entity, &'static Transform), With<CaseLocker>>,
    selected: Query<'w, 's, &'static crate::containers::SelectedInventorySlot>,
    inventory: Query<'w, 's, &'static InventorySlot>,
    held: Query<'w, 's, &'static HeldBy>,
    players: Query<'w, 's, (&'static Body, &'static Bloodstream), With<crate::player::Chemist>>,
    placements: Query<
        'w,
        's,
        (
            &'static Transform,
            Option<&'static InSlot>,
            Option<&'static InSlotB>,
            Option<&'static InSlotC>,
            Option<&'static Stored>,
        ),
    >,
    solids: Query<'w, 's, (Entity, &'static Transform, &'static crate::lab::Solid)>,
    routes: Query<'w, 's, &'static mut CrewRoute>,
    holds: Query<'w, 's, (Entity, &'static OrderHold)>,
}

/// A retained sample behind inaccessible geometry is a recoverable loss, not a
/// reason to hold a customer's batch forever. Machine/storage samples are
/// reached through their owning object; loose samples need a visible standing
/// position connected to Security on the actual floor graph.
fn sample_accessible(
    world: &ActionWorld,
    entity: Entity,
    inventory: Option<&InventorySlot>,
    held: Option<&HeldBy>,
    from: Vec3,
) -> bool {
    if let Some(owner) = inventory.map(|s| s.owner).or_else(|| held.map(|h| h.0)) {
        return world
            .players
            .get(owner)
            .is_ok_and(|(body, blood)| !body.0.collapsed && !blood.0.incapacitated());
    }
    let Ok((at, a, b, c, stored)) = world.placements.get(entity) else {
        return false;
    };
    let owner = a
        .map(|s| s.0)
        .or_else(|| b.map(|s| s.0))
        .or_else(|| c.map(|s| s.0))
        .or_else(|| stored.map(|s| s.0));
    let point = if let Some(owner) = owner {
        let Ok((at, _, _, _, _)) = world.placements.get(owner) else {
            return false;
        };
        at.translation
    } else {
        at.translation
    };
    [
        Vec3::ZERO,
        Vec3::X * 1.4,
        Vec3::NEG_X * 1.4,
        Vec3::Z * 1.4,
        Vec3::NEG_Z * 1.4,
    ]
    .into_iter()
    .any(|offset| {
        let floor = world.nav.standable_goal(point + offset);
        let hand = floor + Vec3::Y * 1.58;
        floor.xz().distance(point.xz()) <= 2.3
            && (point.y - floor.y).abs() <= 2.0
            && world.nav.path(from, floor).is_some()
            && !world.solids.iter().any(|(solid, at, bounds)| {
                Some(solid) != owner
                    && crate::interaction::authority_segment_blocked(
                        hand,
                        point,
                        at.translation,
                        bounds.half_extents,
                    )
            })
    })
}

fn valid_report(report: &AnalysisReport, case: &SecurityCase) -> bool {
    report.case == Some(case.id)
        && report.sample == case.sample
        && case.reference.as_ref().is_some_and(|reference| {
            report.chemicals == reference.chemicals && report.ph == reference.ph
        })
}
fn unchanged_sample(
    reference: &AnalysisReport,
    solution: &chem_sim::Solution,
    db: &ChemDb,
) -> bool {
    let current = AnalysisReport::measure(
        solution,
        db,
        reference.sample,
        reference.scanned_at,
        reference.case,
    );
    current.chemicals == reference.chemicals && current.ph == reference.ph
}
fn release_order(commands: &mut Commands, holds: &Query<(Entity, &OrderHold)>, id: u64) {
    for (entity, hold) in holds.iter().filter(|(_, h)| h.0 == id) {
        let _ = hold;
        commands.entity(entity).remove::<OrderHold>();
    }
}

#[allow(clippy::too_many_arguments)]
fn actions(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<CaseActionRequested>>,
    mut state: ResMut<SecurityCaseState>,
    mut runtime: ResMut<Runtime>,
    mut world: ActionWorld,
    mut radio: ResMut<RadioLog>,
    mut replies: MessageWriter<ToClients<CaseConversationOpened>>,
) {
    for request in requests.read() {
        let Some(case) = state
            .active
            .as_ref()
            .filter(|case| case.id == request.case)
            .cloned()
        else {
            continue;
        };
        let Some(player) = world.access.player(request.client_id, request.target) else {
            continue;
        };
        if !world
            .heard
            .get(player)
            .is_ok_and(|h| h.0.contains(&(case.id, request.target)))
        {
            continue;
        }
        let Ok((member, locker)) = world.targets.get(request.target) else {
            continue;
        };
        let reyes = member.is_some_and(|m| m.name == REYES);
        let warden = member.is_some_and(|m| m.name == BEX);
        let recovery = locker
            && world.wardens.iter().all(|(body, blood, at)| {
                body.0.collapsed
                    || blood.0.incapacitated()
                    || world
                        .lockers
                        .iter()
                        .all(|(_, l)| l.translation.distance(at.translation) > 5.0)
            });
        let security_desk = (warden || recovery)
            && world.lockers.iter().any(|(_, l)| {
                world
                    .wardens
                    .iter()
                    .any(|(_, _, at)| at.translation.distance(l.translation) < 5.0)
                    || recovery
            });
        match request.action {
            CaseAction::Grounds if reyes && case.stage == Stage::Greeting => {
                let mut response = menu(&case, request.target, false, false, false);
                response.text = format!("Reyes: I am questioning this batch's recorded composition. A laboratory scan and the customer's recorded request can be reviewed by Warden Bex.\n\n{}", response.text);
                replies.write(ToClients {
                    targets: SendTargets::Single(request.client_id),
                    message: response,
                });
                continue;
            }
            CaseAction::RecordHold | CaseAction::Refuse
                if reyes && case.stage == Stage::Greeting =>
            {
                let refuse = request.action == CaseAction::Refuse;
                state.active.as_mut().unwrap().stage = if refuse {
                    Stage::Notice
                } else {
                    Stage::Inspecting
                };
                state.active.as_mut().unwrap().notice_left = if refuse { 45.0 } else { 0.0 };
                if refuse {
                    state.refusals += 1;
                } else {
                    state.cooperative_holds += 1;
                }
                runtime.initiator = Some(player);
                runtime.route_age = 0.0;
                commands.entity(request.target).remove::<(
                    PendingOrder,
                    AwaitingConversation,
                    crate::order_intake::queue::QueuePosition,
                )>();
                if let Some(goal) = runtime.inspection_target {
                    if let Ok(mut route) = world.routes.get_mut(request.target) {
                        *route = CrewRoute::to(goal);
                    }
                }
                say(
                    &mut commands,
                    &mut radio,
                    request.target,
                    REYES,
                    if refuse {
                        "You have refused the proposed hold. This is notice of an inspection in 45 seconds. I must inspect that same batch in person."
                    } else {
                        "The hold is recorded. I will inspect the batch in person and leave you a reference sample. Your customer's clock pauses only when the batch enters custody."
                    },
                );
            }
            CaseAction::PresentReport | CaseAction::VerifyCustody
                if case.stage == Stage::Custody && security_desk =>
            {
                let has_batch = world
                    .items
                    .iter()
                    .any(|(_, _, _, held, _, _)| held.is_some_and(|c| c.0 == case.id));
                let available_sample =
                    world
                        .items
                        .iter()
                        .any(|(entity, container, id, _, inventory, held)| {
                            id.is_some_and(|id| id.0 == case.sample)
                                && world.lockers.iter().any(|(_, at)| {
                                    sample_accessible(
                                        &world,
                                        entity,
                                        inventory,
                                        held,
                                        at.translation,
                                    )
                                })
                                && case.reference.as_ref().is_some_and(|r| {
                                    unchanged_sample(r, &container.solution, &world.db)
                                })
                        });
                let evidence = if request.action == CaseAction::PresentReport {
                    world
                        .papers
                        .iter()
                        .any(|(report, slot)| slot.owner == player && valid_report(report, &case))
                } else {
                    has_batch && !available_sample
                };
                if !evidence {
                    let mut response = menu(&case, request.target, warden, locker, recovery);
                    response.text = "The retained sample is still available. Analyze it and bring its authentic printed report. I can verify custody directly if the sample has been lost or altered.".into();
                    replies.write(ToClients {
                        targets: SendTargets::Single(request.client_id),
                        message: response,
                    });
                    continue;
                }
                if has_batch {
                    state.active.as_mut().unwrap().stage = Stage::Released;
                    state.complaints += 1;
                    state.cooldown = 1200.0;
                    state.history.push(CaseRecord {
                        id: case.id,
                        customer: case.customer.clone(),
                        result: "Complaint upheld; Reyes cautioned; awaiting collection".into(),
                    });
                    say(&mut commands,&mut radio,request.target,BEX,"The measurements and recorded customer request do not support this hold. The complaint is upheld. Your batch is released at the evidence locker.");
                }
            }
            CaseAction::Collect if case.stage == Stage::Released && (locker || security_desk) => {
                let batch = world
                    .items
                    .iter()
                    .find(|(_, _, _, held, _, _)| held.is_some_and(|c| c.0 == case.id))
                    .map(|(e, ..)| e);
                let Some(batch) = batch else {
                    release_order(&mut commands, &world.holds, case.id);
                    close_case(&mut state, "Custody unavailable; hold released", true);
                    continue;
                };
                let returned_sample = world
                    .items
                    .iter()
                    .find(|(_, container, id, _, inventory, held)| {
                        id.is_some_and(|i| i.0 == case.sample)
                            && (inventory.is_some_and(|i| i.owner == player)
                                || held.is_some_and(|h| h.0 == player))
                            && case.reference.as_ref().is_some_and(|r| {
                                unchanged_sample(r, &container.solution, &world.db)
                            })
                    })
                    .map(|(e, ..)| e);
                if let Some(sample) = returned_sample {
                    if let Ok([(_, mut original, _, _, _, _), (_, mut retained, _, _, _, _)]) =
                        world.items.get_many_mut([batch, sample])
                    {
                        let amount = retained.solution.total_volume();
                        retained
                            .solution
                            .transfer_to(&mut original.solution, amount);
                    }
                }
                let preferred = world.selected.get(player).map_or(0, |s| s.0);
                commands
                    .entity(batch)
                    .remove::<(Stored, CaseCustody)>()
                    .insert(Visibility::Inherited);
                let occupied_hand = if world.held.iter().any(|held| held.0 == player) {
                    vec![(player, preferred)]
                } else {
                    vec![]
                };
                if let Some(slot) = crate::containers::free_inventory_slot(
                    player,
                    preferred,
                    &world.inventory,
                    &occupied_hand,
                ) {
                    commands.entity(batch).insert(InventorySlot {
                        owner: player,
                        slot,
                    });
                    if slot == preferred {
                        commands.entity(batch).insert(HeldBy(player));
                    }
                } else if let Some((_, at)) = world.lockers.iter().next() {
                    commands.entity(batch).insert(Transform::from_translation(
                        at.translation + Vec3::new(0.55, -0.35, 0.0),
                    ));
                }
                release_order(&mut commands, &world.holds, case.id);
                close_case(
                    &mut state,
                    "Complaint upheld; released batch collected",
                    true,
                );
            }
            CaseAction::Abandon if case.stage == Stage::Custody && (locker || security_desk) => {
                for (entity, _, _, held, _, _) in &world.items {
                    if held.is_some_and(|c| c.0 == case.id) {
                        commands.entity(entity).despawn();
                    }
                }
                release_order(&mut commands, &world.holds, case.id);
                close_case(&mut state, "Claim abandoned by chemist", false);
            }
            _ => continue,
        }
        if let Ok(mut heard) = world.heard.get_mut(player) {
            heard.0.remove(&(case.id, request.target));
        }
    }
}
