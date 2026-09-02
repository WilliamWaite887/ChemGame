//! Hearing a request is separate from accepting responsibility for it.
//! Pending payloads never replicate and cannot be consumed by delivery systems.

#[cfg(debug_assertions)]
mod playtest;
pub mod queue;
#[cfg(test)]
mod tests;
pub(crate) mod ui;

use crate::{
    arc::{Campaign, CampaignId},
    body::{Bloodstream, Body},
    crew::{Ambient, CrewMember, CrewRoute},
    interaction::Interactable,
    orders::{Order, Shift},
    player::Chemist,
    AppState,
};
use bevy::ecs::{entity::MapEntities, system::SystemParam};
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};

pub const WAITING_LIMIT: usize = 2;
pub const GREETING_SECONDS: f32 = 180.0;
pub const REMINDER_SECONDS: f32 = 120.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RequestSource {
    Ordinary,
    Specific,
    Antagonist,
    Addiction,
    Counter,
    Cult,
    Obsessed,
    Quack,
    Smuggler,
    Saboteur,
    BentGuard,
    Security,
}

#[derive(Component, Clone, Copy, Debug)]
pub struct RequestContext {
    pub id: u64,
    pub source: RequestSource,
    pub campaign: Option<CampaignId>,
    pub greeting: GreetingKind,
    pub step: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GreetingKind {
    Ordinary,
    Campaign,
}

#[derive(Resource, Default)]
pub struct IntakeState {
    next_id: u64,
    next_acceptance: u64,
    reserved: HashSet<String>,
    // A blocked source keeps its place, rather than losing to plugin order.
    due: VecDeque<(RequestSource, f64)>,
}

#[derive(SystemParam)]
#[allow(clippy::type_complexity)]
pub struct Intake<'w, 's> {
    state: Option<ResMut<'w, IntakeState>>,
    time: Res<'w, Time>,
    campaign: Option<Res<'w, Campaign>>,
    pending: Query<'w, 's, &'static CrewMember, With<PendingOrder>>,
    people: Query<
        'w,
        's,
        (
            &'static CrewMember,
            Has<Ambient>,
            Has<crate::social::NpcCommitment>,
            Option<&'static Body>,
            Option<&'static Bloodstream>,
        ),
    >,
}

impl Intake<'_, '_> {
    pub fn available(&self, name: &str) -> bool {
        !self
            .state
            .as_ref()
            .is_some_and(|s| s.reserved.contains(name))
            && self.people.iter().filter(|(p, ..)| p.name == name).all(
                |(_, ambient, committed, body, blood)| {
                    ambient
                        && !committed
                        && !body.is_some_and(|b| b.0.collapsed)
                        && !blood.is_some_and(|b| b.0.incapacitated())
                },
            )
    }

    /// Called only after a source has a due, feasible request. Retrying a blocked
    /// admission never advances content and never banks multiple missed visits.
    pub fn admit(
        &mut self,
        source: RequestSource,
        name: &str,
        timer: &mut Timer,
        campaign_related: bool,
    ) -> Option<RequestContext> {
        let campaign = campaign_related
            .then_some(self.campaign.as_deref())
            .flatten()
            .filter(|c| c.outcome.is_none())
            .map(|c| c.id);
        let greeting = if campaign.is_some() {
            GreetingKind::Campaign
        } else {
            GreetingKind::Ordinary
        };
        if !self.available(name) {
            *timer = Timer::from_seconds(0.5, TimerMode::Once);
            return None;
        }
        let live: HashSet<_> = self.pending.iter().map(|m| m.name.clone()).collect();
        let Some(state) = self.state.as_mut() else {
            // Isolated content tests need no scheduling service.
            return Some(RequestContext {
                id: 1,
                source,
                campaign,
                greeting,
                step: None,
            });
        };
        let now = self.time.elapsed_secs_f64();
        let eligibility_grace = (self.time.delta_secs_f64() * 4.0).max(2.0);
        state
            .due
            .retain(|(_, seen)| now - *seen < eligibility_grace);
        if let Some((_, seen)) = state.due.iter_mut().find(|(s, _)| *s == source) {
            *seen = now;
        } else {
            state.due.push_back((source, now));
        }
        let count = live.union(&state.reserved).count();
        if count >= WAITING_LIMIT || state.due.front().is_some_and(|(s, _)| *s != source) {
            *timer = Timer::from_seconds(0.5, TimerMode::Once);
            return None;
        }
        state.due.pop_front();
        state.reserved.insert(name.to_string());
        state.next_id += 1;
        Some(RequestContext {
            id: state.next_id,
            source,
            campaign,
            greeting,
            step: None,
        })
    }
}

#[derive(Component)]
#[require(AwaitingConversation)]
pub struct PendingOrder {
    pub order: Order,
    pub context: RequestContext,
    pub waited: f32,
    unroutable: f32,
    greeted: bool,
    reminded: bool,
    pub extra_dialogue: Vec<String>,
}

impl PendingOrder {
    pub fn new(order: Order, context: RequestContext) -> Self {
        Self {
            order,
            context,
            waited: 0.0,
            unroutable: 0.0,
            greeted: false,
            reminded: false,
            extra_dialogue: vec![],
        }
    }
}

#[derive(Component, Default, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct AwaitingConversation {
    pub id: u64,
    pub arrived: bool,
}

#[derive(Component, Clone, Copy, Debug, Serialize, Deserialize)]
pub struct AcceptedOrder {
    pub sequence: u64,
}

#[derive(Asset, TypePath, Deserialize)]
pub struct GreetingScript {
    pub ordinary: Vec<String>,
    pub campaign: Vec<String>,
    pub ordinary_reminders: Vec<String>,
    pub campaign_reminders: Vec<String>,
}
type Script = crate::threat::Authored<GreetingScript>;

fn greeting(script: &GreetingScript, campaign: bool, reminder: bool, id: u64) -> String {
    let pool = match (campaign, reminder) {
        (false, false) => &script.ordinary,
        (true, false) => &script.campaign,
        (false, true) => &script.ordinary_reminders,
        (true, true) => &script.campaign_reminders,
    };
    pool.get(id as usize % pool.len().max(1))
        .cloned()
        .unwrap_or_else(|| "Chemist, could you come to the window?".into())
}

#[derive(Message, Clone, Serialize, Deserialize, MapEntities)]
pub struct OpenOrderConversation {
    #[entities]
    pub target: Entity,
    pub id: u64,
}
#[derive(Message, Clone, Serialize, Deserialize, MapEntities)]
pub struct AcceptOrder {
    #[entities]
    pub target: Entity,
    pub id: u64,
}
#[derive(Message, Clone, Serialize, Deserialize, MapEntities)]
pub struct OrderConversationOpened {
    #[entities]
    pub target: Entity,
    pub id: u64,
    pub name: String,
    pub role: String,
    pub explanation: String,
    pub requirements: String,
    pub patience: f32,
}

#[derive(Component, Default)]
pub(crate) struct HeardRequests(HashSet<(Entity, u64)>);

pub struct OrderIntakePlugin;
impl Plugin for OrderIntakePlugin {
    fn build(&self, app: &mut App) {
        #[cfg(debug_assertions)]
        if std::env::args().any(|arg| arg == "--order-playtest") {
            playtest::install(app);
        }
        app.init_resource::<IntakeState>()
            .add_plugins(crate::threat::ScriptPlugin::<GreetingScript>::new(
                "data/station.greetings.ron",
                "greetings.ron",
            ))
            .add_mapped_client_message::<OpenOrderConversation>(Channel::Ordered)
            .add_mapped_client_message::<AcceptOrder>(Channel::Ordered)
            .add_mapped_server_message::<OrderConversationOpened>(Channel::Ordered)
            .add_systems(
                PreUpdate,
                clear_frame_reservations.run_if(crate::net::is_authority),
            )
            .add_systems(
                Update,
                (update_pending, open_conversations, accept_orders)
                    .chain()
                    .run_if(crate::net::is_authority)
                    .run_if(in_state(AppState::Playing)),
            )
            .add_plugins((queue::OrderQueuePlugin, ui::OrderConversationUiPlugin));
    }
}

fn clear_frame_reservations(mut state: ResMut<IntakeState>) {
    state.reserved.clear();
}

#[allow(clippy::type_complexity)]
#[allow(clippy::too_many_arguments)]
fn update_pending(
    mut commands: Commands,
    session: Option<Res<crate::session::SessionKind>>,
    time: Res<Time>,
    script: Option<Res<Script>>,
    campaign: Option<Res<Campaign>>,
    mut shift: ResMut<Shift>,
    mut cult: Option<ResMut<crate::cult::CultProgress>>,
    nav: Option<Res<crate::nav::NavGraph>>,
    mut radio: ResMut<crate::radio::RadioLog>,
    mut pending: Query<(
        Entity,
        &CrewMember,
        &mut PendingOrder,
        &mut AwaitingConversation,
        &mut CrewRoute,
        Option<&queue::QueuePosition>,
        Option<&Bloodstream>,
        &Transform,
    )>,
) {
    for (entity, member, mut request, mut visible, mut route, place, blood, at) in &mut pending {
        let obsolete = request.context.campaign.is_some_and(|id| {
            !campaign
                .as_deref()
                .is_some_and(|c| c.active_by_id(id).is_some_and(|a| a.outcome.is_none()))
        });
        let unable = blood.is_some_and(|b| b.0.incapacitated());
        let abandoned = route.phase == crate::crew::CrewPhase::Leaving;
        let ended_step = request.context.source == RequestSource::Cult
            && request
                .context
                .step
                .is_some_and(|step| cult.as_ref().is_some_and(|p| p.next_stage != step));
        let no_path = place.is_some_and(|p| {
            nav.as_ref()
                .is_some_and(|n| n.path(at.translation, p.target).is_none())
        });
        request.unroutable = if no_path {
            request.unroutable + time.delta_secs()
        } else {
            0.0
        };
        if obsolete
            || ended_step
            || unable
            || abandoned
            || route.routing_failed()
            || request.unroutable >= 10.0
        {
            if request.context.source == RequestSource::Security {
                warn!(
                    "Security approach withdrawn by intake: npc={} position={:?} target={:?} reached={} phase={:?} moving={} routing_failed={} unroutable={:.2} obsolete={} ended_step={} incapacitated={} leaving={}",
                    member.name, at.translation, place.map(|p| p.target),
                    place.is_some_and(|p| p.reached), route.phase, route.is_moving(),
                    route.routing_failed(), request.unroutable, obsolete, ended_step, unable, abandoned,
                );
            }
            release_script(&request.context, &mut cult);
            withdraw(&mut commands, entity, &mut route);
            continue;
        }
        let arrived = place.is_some_and(|p| p.reached);
        if visible.id != request.context.id || visible.arrived != arrived {
            *visible = AwaitingConversation {
                id: request.context.id,
                arrived,
            };
        }
        if !arrived && !request.greeted {
            continue;
        }
        if session.as_deref() != Some(&crate::session::SessionKind::Training)
            || request.context.id & crate::tutorial::TRAINING_REQUEST_BIT == 0
        {
            request.waited += time.delta_secs();
        }
        let remind = request.waited >= REMINDER_SECONDS && !request.reminded;
        if !request.greeted || remind {
            if let Some(script) = script.as_deref() {
                let line = greeting(
                    script,
                    request.context.greeting == GreetingKind::Campaign,
                    request.greeted,
                    request.context.id,
                );
                crate::speech::say(
                    &mut commands,
                    entity,
                    line.clone(),
                    crate::speech::SpeechTone::Neutral,
                );
                radio.push(
                    crate::radio::RadioEntry::new(crate::radio::channel_for(&member.role), line)
                        .speaker(&member.name),
                );
                if request.greeted {
                    request.reminded = true;
                }
                request.greeted = true;
            }
        }
        if request.waited >= GREETING_SECONDS {
            if request.context.source != RequestSource::Security {
                shift.adjust_npc(&member.name, -1);
            }
            radio.push(
                crate::radio::RadioEntry::new(
                    crate::radio::channel_for(&member.role),
                    "Couldn't get a moment at the window. I'll come back another time.",
                )
                .speaker(&member.name),
            );
            release_script(&request.context, &mut cult);
            withdraw(&mut commands, entity, &mut route);
        }
    }
}

fn release_script(context: &RequestContext, cult: &mut Option<ResMut<crate::cult::CultProgress>>) {
    if context.source == RequestSource::Cult {
        if let Some(cult) = cult.as_mut() {
            if cult.offered_stage == context.step {
                cult.offered_stage = None;
            }
        }
    }
}

fn withdraw(commands: &mut Commands, entity: Entity, route: &mut CrewRoute) {
    commands
        .entity(entity)
        .remove::<(
            PendingOrder,
            AwaitingConversation,
            crate::orders::DevelopmentOrder,
            crate::orders::CounterOrder,
            crate::orders::HostileOrder,
            crate::orders::IllicitOrder,
            queue::QueuePosition,
        )>()
        .insert(Interactable::new("Crew member"));
    route.leave();
}

pub fn requirements(order: &Order, db: &crate::chem_data::ChemDb) -> String {
    let want = if order.specific {
        db.reagents.get(order.reagent).name.clone()
    } else {
        crate::orders::reference_category(db, order.reagent)
            .filter(|c| c.is_legitimately_orderable())
            .map(|c| c.want_phrase().to_string())
            .unwrap_or_else(|| db.reagents.get(order.reagent).name.clone())
    };
    let quality = if order.minimum_purity > 0.0 {
        format!(" — at least {:.0}% purity", order.minimum_purity * 100.0)
    } else {
        String::new()
    };
    format!("{} {}{}", order.amount, want, quality)
}

#[derive(SystemParam)]
#[allow(clippy::type_complexity)]
pub(crate) struct ConversationAccess<'w, 's> {
    actors: Query<
        'w,
        's,
        (
            Entity,
            &'static Chemist,
            &'static Transform,
            &'static Body,
            &'static Bloodstream,
        ),
    >,
    positions: Query<
        'w,
        's,
        (
            &'static Transform,
            Option<&'static Body>,
            Option<&'static Bloodstream>,
        ),
        Without<Chemist>,
    >,
    solids: Query<'w, 's, (&'static Transform, &'static crate::lab::Solid)>,
}
impl ConversationAccess<'_, '_> {
    pub(crate) fn player(&self, client: ClientId, target: Entity) -> Option<Entity> {
        let (entity, _, at, body, blood) =
            self.actors.iter().find(|(_, c, ..)| c.client == client)?;
        let (target, target_body, target_blood) = self.positions.get(target).ok()?;
        if body.0.collapsed
            || blood.0.incapacitated()
            || target_body.is_some_and(|b| b.0.collapsed)
            || target_blood.is_some_and(|b| b.0.incapacitated())
            || !crate::interaction::authority_target_in_reach(
                at.translation,
                target.translation,
                crate::interaction::REACH,
            )
            || self.solids.iter().any(|(t, s)| {
                crate::interaction::authority_segment_blocked(
                    at.translation + Vec3::Y * 0.65,
                    target.translation + Vec3::Y * 0.65,
                    t.translation,
                    s.half_extents,
                )
            })
        {
            return None;
        }
        Some(entity)
    }
}

pub(crate) fn open_conversations(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<OpenOrderConversation>>,
    access: ConversationAccess,
    pending: Query<(&CrewMember, &PendingOrder, &AwaitingConversation)>,
    mut heard: Query<&mut HeardRequests>,
    db: Res<crate::chem_data::ChemDb>,
    mut replies: MessageWriter<ToClients<OrderConversationOpened>>,
) {
    for request in requests.read() {
        let Some(player) = access.player(request.client_id, request.target) else {
            continue;
        };
        let Ok((member, pending, visible)) = pending.get(request.target) else {
            continue;
        };
        if pending.context.source == RequestSource::Security
            || pending.context.id != request.id
            || !visible.arrived
        {
            continue;
        }
        let key = (request.target, request.id);
        if let Ok(mut heard) = heard.get_mut(player) {
            heard.0.insert(key);
        } else {
            commands
                .entity(player)
                .insert(HeardRequests(HashSet::from([key])));
        }
        let explanation = std::iter::once(pending.order.plea.clone())
            .chain(pending.extra_dialogue.iter().cloned())
            .collect::<Vec<_>>()
            .join("\n\n");
        replies.write(ToClients {
            targets: SendTargets::Single(request.client_id),
            message: OrderConversationOpened {
                target: request.target,
                id: request.id,
                name: member.name.clone(),
                role: member.role.clone(),
                explanation,
                requirements: requirements(&pending.order, &db),
                patience: pending.order.patience,
            },
        });
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn accept_orders(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<AcceptOrder>>,
    mut intake: ResMut<IntakeState>,
    access: ConversationAccess,
    mut heard: Query<&mut HeardRequests>,
    mut pending: Query<(&CrewMember, &mut PendingOrder, &AwaitingConversation)>,
    db: Res<crate::chem_data::ChemDb>,
    mut social: Option<ResMut<crate::social::SocialState>>,
) {
    let mut accepted = HashSet::new();
    for request in requests.read() {
        let Some(player) = access.player(request.client_id, request.target) else {
            continue;
        };
        let Ok(mut history) = heard.get_mut(player) else {
            continue;
        };
        let key = (request.target, request.id);
        if !history.0.contains(&key) || accepted.contains(&request.target) {
            continue;
        }
        let Ok((member, pending, visible)) = pending.get_mut(request.target) else {
            continue;
        };
        if pending.context.source == RequestSource::Security
            || pending.context.id != request.id
            || !visible.arrived
            || pending.waited >= GREETING_SECONDS
        {
            continue;
        }
        accepted.insert(request.target);
        history.0.remove(&key);
        let mut order = pending.order.clone();
        order.waited = 0.0;
        order.plea = std::iter::once(order.plea)
            .chain(pending.extra_dialogue.iter().cloned())
            .collect::<Vec<_>>()
            .join("\n\n");
        intake.next_acceptance += 1;
        if let Some(social) = social.as_mut() {
            social.remember_dialogue(&member.name, &order.plea);
        }
        commands
            .entity(request.target)
            .remove::<(PendingOrder, AwaitingConversation, queue::QueuePosition)>()
            .insert((
                Interactable::new(format!(
                    "{} — hand over {}",
                    member.name,
                    requirements(&order, &db)
                )),
                order,
                AcceptedOrder {
                    sequence: intake.next_acceptance,
                },
                pending.context,
            ));
    }
}
