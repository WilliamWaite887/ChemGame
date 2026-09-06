//! Private goals, physical illicit custody, and the food-poisoning reference
//! action.
//!
//! The rule this module exists to enforce, stated once and then obeyed
//! everywhere below: **a covert action is ordinary embodied behaviour that
//! happens to be selfish.** An antagonist is not a special controller, does not
//! get a private movement path, and does not bypass reservations, buckets, or
//! perception. It scores a candidate alongside its own job and loses to a
//! casualty like anyone else. That is what makes it possible to hide in plain
//! sight — and what stops "the antagonist" from becoming a scripted event
//! wearing a body.
//!
//! ## The supply invariant
//!
//! A `player_only` reagent never enters NPC hands except as a physical batch a
//! player handed over. This module holds the custody ledger that makes that
//! checkable rather than merely intended:
//!
//! - Stock is real `Solution` volume with recorded provenance, not a flag.
//! - Using it *removes* volume. No hidden copy remains, so the same batch
//!   cannot fund two incidents unless enough real volume is left for both.
//! - `PoisonFood` scores exactly zero when no matching stock remains, which is
//!   a veto rather than a penalty — see [`IllicitStock::usable`].
//!
//! ## What replicates
//!
//! Nothing here. Custody, goals, and scores are authority-only. A client can
//! see a body walk to a counter and handle food, because that is a public
//! physical act; it cannot see why, what was carried, or who is an antagonist.
//! Security conclusions have to come from witnesses and evidence, which is what
//! [`super::perception`] and [`super::interviews`] are for.

use bevy::prelude::*;

use super::{
    stable_text_key, ActionResult, ActionTarget, Normalized, ReservationKey, UtilityActionId,
    UtilityActionResolved, UtilityAgent, UtilityBucket, UtilityOpportunity,
    UtilityOpportunityBuffer,
};
use crate::crew::CrewMember;

/// How long the food-handling action takes. Long enough that a witness has a
/// real chance to be present, short enough to be plausible kitchen work.
const POISON_SECONDS: f32 = 4.0;

/// Minimum real volume of a contaminant, in mL, that counts as an effective
/// dose.
///
/// Below this the actor is not "poisoning a meal", it is wasting stock. Keeping
/// it a real volume rather than a boolean is what makes a diluted or
/// part-spent batch stop being usable at the honest point.
const EFFECTIVE_DOSE_ML: f64 = 4.0;

/// A meal with fewer servings than this is not worth the risk.
const WORTHWHILE_SERVINGS: u8 = 2;

/// How far an actor will travel to reach a meal, in metres.
const REACH: f32 = 24.0;

/// What an antagonist is privately trying to achieve.
///
/// Deliberately few and deliberately *motive-shaped* rather than action-shaped:
/// a goal explains why sabotage might be worth it, and leaves the choice of act
/// to scoring. A goal that named its own action would be a script.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize)]
pub enum PrivateGoal {
    /// Make a department look unsafe so its standing or staffing changes.
    DiscreditDepartment,
    /// Create enough disorder to move under cover of it.
    SowDisorder,
    /// Settle a personal grievance against one specific person.
    Grudge,
}

impl PrivateGoal {
    /// Whether poisoning a shared meal could plausibly serve this goal.
    ///
    /// A grudge is excluded on purpose: a communal batch cannot target one
    /// person, so poisoning it would be an act that does not serve the motive.
    /// This is the "non-zero expected goal benefit" gate, and it is why the
    /// same antagonist can be dangerous in one situation and harmless in
    /// another rather than always reaching for the same trick.
    fn served_by_poisoning(self) -> bool {
        matches!(self, Self::DiscreditDepartment | Self::SowDisorder)
    }
}

/// An agent's private motive. Authority-only: replicating this would tell a
/// client who the antagonist is, which is the one thing that must stay hidden.
#[derive(Component, Clone, Copy, Debug)]
pub struct CovertGoal {
    pub goal: PrivateGoal,
    /// How much risk this personality tolerates, 0..1. Scales the witness gate,
    /// so a cautious antagonist genuinely waits for a better moment instead of
    /// acting with a lower score.
    pub nerve: f32,
    /// Set once the goal has been served. A satisfied antagonist goes back to
    /// ordinary life rather than escalating forever.
    pub achieved: bool,
}

impl CovertGoal {
    pub fn new(goal: PrivateGoal, nerve: f32) -> Self {
        Self {
            goal,
            nerve: nerve.clamp(0.0, 1.0),
            achieved: false,
        }
    }
}

/// Where a player-supplied batch is in its life.
///
/// Receipt does not imply imminent harm. The intermediate states exist so a
/// player has time to notice behaviour, recover the batch, warn someone, or
/// change the situation before anything happens.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CustodyState {
    /// Held on the body, the most recoverable state.
    Carried,
    /// Stashed somewhere. Still physical, still findable.
    Cached,
    /// Committed to a specific action and no longer freely spendable.
    ReservedForAction,
    /// Terminal. The volume is gone from the world by this route.
    Consumed,
    Confiscated,
    Destroyed,
    Returned,
}

impl CustodyState {
    /// Whether stock in this state can still fund a covert action.
    fn spendable(self) -> bool {
        matches!(self, Self::Carried | Self::Cached)
    }
}

/// One physical player-supplied batch in an NPC's possession.
///
/// The provenance fields are not decoration: they are what lets a later
/// investigation say *which* player supplied *what*, and what makes
/// confiscation and return real rather than bookkeeping.
#[derive(Clone, Debug)]
pub struct IllicitStock {
    /// The actual remaining solution. Spending draws this down.
    pub solution: chem_sim::Solution,
    pub state: CustodyState,
    /// Who handed it over. `None` only for test fixtures.
    pub source_player: Option<Entity>,
    pub received_at: f32,
    /// What the supplier claimed it was, which may not be what it is.
    pub claimed_label: String,
}

impl IllicitStock {
    /// Whether this batch can currently fund an effective dose of `reagent`.
    ///
    /// The veto that makes the supply invariant real. Three independent ways to
    /// be unusable — wrong state, wrong contents, not enough left — and all of
    /// them are checked against physical volume rather than a flag.
    pub fn usable(&self, reagent: chem_sim::ReagentId) -> bool {
        self.state.spendable()
            && self
                .solution
                .contains_at_least(reagent, chem_sim::Units::from_f64(EFFECTIVE_DOSE_ML))
    }
}

/// Every player-supplied batch currently in NPC hands.
///
/// Authority-only, and bounded by the fact that the only way to add an entry is
/// a player physically handing something over.
#[derive(Resource, Default)]
pub struct IllicitCustody {
    holdings: Vec<(Entity, IllicitStock)>,
}

impl IllicitCustody {
    /// Records a batch a player physically handed to an NPC.
    ///
    /// This is the **only** way stock enters the system. There is deliberately
    /// no other constructor, no spawn hook, and no load path that fabricates
    /// one: if this function is not called, no NPC has anything.
    pub fn receive_from_player(&mut self, holder: Entity, stock: IllicitStock) {
        self.holdings.push((holder, stock));
    }

    pub fn held_by(&self, holder: Entity) -> impl Iterator<Item = &IllicitStock> {
        self.holdings
            .iter()
            .filter(move |(who, _)| *who == holder)
            .map(|(_, stock)| stock)
    }

    fn spendable_for(&self, holder: Entity, reagent: chem_sim::ReagentId) -> Option<usize> {
        self.holdings
            .iter()
            .position(|(who, stock)| *who == holder && stock.usable(reagent))
    }

    /// Moves a real dose out of a holder's stock into `into`.
    ///
    /// Returns how much actually moved. Because this transfers rather than
    /// copies, a batch that funded one incident has visibly less left for the
    /// next — which is the whole point of tracking volume instead of a flag.
    pub fn spend(
        &mut self,
        holder: Entity,
        reagent: chem_sim::ReagentId,
        amount: chem_sim::Units,
        into: &mut chem_sim::Solution,
    ) -> chem_sim::Units {
        let Some(index) = self.spendable_for(holder, reagent) else {
            return chem_sim::Units::ZERO;
        };
        let stock = &mut self.holdings[index].1;
        // Remove-then-add rather than a bulk transfer: only the contaminant
        // moves, so the rest of a mixed batch stays in NPC custody and stays
        // recoverable as evidence.
        let taken = stock.solution.remove(reagent, amount);
        // `Solution::add` returns the *overflow it refused*, not the amount it
        // accepted. A meal at its volume cap therefore hands some back, and
        // that remainder must return to custody rather than vanishing — an
        // antagonist who tops up a full bowl has not spent their stock.
        let refused = into.add(reagent, taken);
        if refused.is_positive() {
            let _ = stock.solution.add(reagent, refused);
        }
        let landed = taken - refused;
        if stock.solution.volume_of(reagent) <= chem_sim::Units::ZERO {
            stock.state = CustodyState::Consumed;
        }
        landed
    }

    /// Takes a batch out of NPC hands entirely. Used by confiscation, return,
    /// and destruction, which differ in consequence but not in physics.
    pub fn resolve(&mut self, holder: Entity, state: CustodyState) -> usize {
        let mut resolved = 0;
        for (who, stock) in self.holdings.iter_mut() {
            if *who == holder && stock.state.spendable() {
                stock.state = state;
                resolved += 1;
            }
        }
        resolved
    }

    pub fn clear(&mut self) {
        self.holdings.clear();
    }

    /// Snapshots custody for `progress.ron`, resolving each holder to a name.
    ///
    /// Keyed by name rather than `Entity` for the reason `addiction` already
    /// documents: crew entities are despawned when they walk out, and a batch
    /// outlives the visit. An entity id would also be meaningless after a
    /// reload. `resolve` supplies the name for a holder; a holder it cannot
    /// name is dropped, because a batch nobody can be identified as holding is
    /// unrecoverable by the player and would otherwise persist forever as an
    /// invisible source of contaminant.
    ///
    /// Terminal states are not saved. A consumed, confiscated, destroyed or
    /// returned batch has already had its effect, and reloading it would let a
    /// player who confiscated something find it back in the same NPC's hands.
    pub fn snapshot(&self, resolve: impl Fn(Entity) -> Option<String>) -> Vec<CustodyRecord> {
        self.holdings
            .iter()
            .filter(|(_, stock)| stock.state.spendable())
            .filter_map(|(holder, stock)| {
                Some(CustodyRecord {
                    holder: resolve(*holder)?,
                    solution: stock.solution.clone(),
                    state: stock.state,
                    received_at: stock.received_at,
                    claimed_label: stock.claimed_label.clone(),
                })
            })
            .collect()
    }

    /// Restores a snapshot, mapping each name back to a live entity.
    ///
    /// Replaces rather than appends: `restore` is called on load into a
    /// resource that a fresh session has already default-initialised, and
    /// appending would double a batch for anyone who reloaded twice — the
    /// "without duplication" half of the plan's save/load requirement.
    ///
    /// A name with no live entity is skipped rather than queued. `source_player`
    /// is deliberately not restored: the supplying player may not be connected,
    /// and a stale entity id would attribute the batch to whoever now holds
    /// that id.
    pub fn restore(
        &mut self,
        records: &[CustodyRecord],
        locate: impl Fn(&str) -> Option<Entity>,
    ) -> usize {
        self.holdings.clear();
        for record in records {
            let Some(holder) = locate(&record.holder) else {
                continue;
            };
            self.holdings.push((
                holder,
                IllicitStock {
                    solution: record.solution.clone(),
                    state: record.state,
                    source_player: None,
                    received_at: record.received_at,
                    claimed_label: record.claimed_label.clone(),
                },
            ));
        }
        self.holdings.len()
    }
}

/// One saved batch, keyed by the holder's crew name.
///
/// Lives in `progress.ron` alongside the rest of the career. Like everything
/// else in that file this is readable by a player who opens it in a text
/// editor, which is the codebase's existing stance — see `ProgressSave`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CustodyRecord {
    pub holder: String,
    pub solution: chem_sim::Solution,
    pub state: CustodyState,
    #[serde(default)]
    pub received_at: f32,
    #[serde(default)]
    pub claimed_label: String,
}

/// Offers a covert food-handling action to an antagonist who can actually do it.
///
/// Every gate in the plan's list is here, and each one is a *veto* rather than
/// a score penalty, because "slightly too risky" and "impossible" are different
/// things and collapsing them produces an antagonist who eventually does
/// everything.
#[allow(clippy::type_complexity)]
fn offer_food_poisoning(
    time: Res<Time>,
    mut buffer: ResMut<UtilityOpportunityBuffer>,
    custody: Res<IllicitCustody>,
    contaminant: Option<Res<CovertContaminant>>,
    meals: Query<(
        Entity,
        &Transform,
        &super::service::MealBatch,
        &super::service::MealChemistry,
    )>,
    actors: Query<(Entity, &CrewMember, &Transform, &CovertGoal), With<UtilityAgent>>,
) {
    let Some(contaminant) = contaminant else {
        return;
    };
    let now = time.elapsed_secs();

    for (actor, member, at, goal) in &actors {
        // An achieved goal stops. An antagonist who has what they wanted goes
        // back to ordinary life instead of escalating without end.
        if goal.achieved || !goal.goal.served_by_poisoning() {
            continue;
        }
        // The supply veto: no physical player-supplied stock, no action. Not a
        // low score — no candidate at all.
        if !custody
            .held_by(actor)
            .any(|stock| stock.usable(contaminant.reagent))
        {
            continue;
        }

        let mut reachable: Vec<(Entity, Vec3, u8)> = meals
            .iter()
            .filter(|(_, _, batch, _)| {
                batch.stage == super::service::MealStage::Served
                    && batch.servings_remaining >= WORTHWHILE_SERVINGS
            })
            .filter(|(_, meal_at, _, _)| meal_at.translation.distance(at.translation) <= REACH)
            .map(|(meal, meal_at, batch, _)| (meal, meal_at.translation, batch.servings_remaining))
            .collect();
        if reachable.is_empty() {
            continue;
        }
        // Most servings first, then entity order, so the choice is
        // reproducible and prefers the meal that actually serves the goal.
        reachable.sort_by(|(a, _, a_servings), (b, _, b_servings)| {
            b_servings
                .cmp(a_servings)
                .then_with(|| a.to_bits().cmp(&b.to_bits()))
        });
        let (meal, meal_at, servings) = reachable[0];

        // Appeal rises with how much this would actually achieve and with the
        // actor's own nerve. It stays in `Routine`: a covert act is something
        // done *instead of* ordinary work, never something that outranks a
        // casualty. An antagonist who abandons a dying colleague to poison
        // soup is a worse antagonist and a worse simulation.
        let reward = (servings as f32 / 6.0).clamp(0.0, 1.0);
        let appeal = Normalized::new((reward * goal.nerve).clamp(0.0, 1.0))
            .expect("both factors are clamped to 0..1");
        buffer.offer(
            UtilityOpportunity::new(
                actor,
                UtilityActionId::PoisonFood,
                UtilityBucket::Routine,
                stable_text_key(&format!("covert.poison.{:016x}", meal.to_bits())),
                appeal,
            )
            .with_target(ActionTarget::Point(meal_at))
            .with_reservation(
                ReservationKey(format!("covert.meal.{:016x}", meal.to_bits())),
                1,
            )
            .with_timing(POISON_SECONDS, POISON_SECONDS + 30.0),
        );
        let _ = (member, now);
    }
}

/// Which reagent the current covert thread uses, resolved once from chem data.
///
/// A resource rather than a constant so the authored id is validated at load
/// instead of being re-looked-up per frame, and so a scenario can swap it.
#[derive(Resource, Clone, Copy, Debug)]
pub struct CovertContaminant {
    pub reagent: chem_sim::ReagentId,
}

/// Applies a completed food-handling action: real reagent moves into the meal.
///
/// There is no "poisoned" flag anywhere. The contaminant becomes part of the
/// meal's ordinary `Solution`, so it reaches an eater through the same
/// bloodstream path as any other exposure and produces the same symptoms,
/// Medical case, and treatment. That is what makes the harm discoverable rather
/// than announced — and it is why `quality_percent`, which is fixed when the
/// legitimate ingredients cook, cannot leak it.
#[allow(clippy::type_complexity, clippy::too_many_arguments)]
fn apply_food_poisoning(
    time: Res<Time>,
    mut results: MessageReader<UtilityActionResolved>,
    mut custody: ResMut<IllicitCustody>,
    contaminant: Option<Res<CovertContaminant>>,
    mut tampered: ResMut<TamperedMeals>,
    mut witnessed: MessageWriter<super::Stimulus>,
    mut goals: Query<&mut CovertGoal>,
    mut meals: Query<(
        Entity,
        &Transform,
        &super::service::MealBatch,
        &mut super::service::MealChemistry,
    )>,
) {
    let Some(contaminant) = contaminant else {
        return;
    };
    let now = time.elapsed_secs();

    for result in results.read() {
        if result.key.action != UtilityActionId::PoisonFood
            || result.result != ActionResult::Completed
        {
            continue;
        }

        // Re-resolve the meal from the action's own key rather than trusting a
        // stored handle: the batch may have been eaten or cleared during the
        // walk, and a spent action on a gone meal must simply do nothing.
        let Some((meal, meal_at)) = meals.iter().find_map(|(meal, at, batch, _)| {
            (stable_text_key(&format!("covert.poison.{:016x}", meal.to_bits()))
                == result.key.target_key
                && batch.stage == super::service::MealStage::Served
                && batch.servings_remaining > 0)
                .then_some((meal, at.translation))
        }) else {
            continue;
        };
        let Ok((_, _, _, mut chemistry)) = meals.get_mut(meal) else {
            continue;
        };

        // Physical transfer. If custody has nothing left, nothing moves and the
        // action was simply wasted effort — no flag flips on an empty batch.
        let moved = custody.spend(
            result.agent,
            contaminant.reagent,
            chem_sim::Units::from_f64(EFFECTIVE_DOSE_ML),
            &mut chemistry.solution,
        );
        if moved <= chem_sim::Units::ZERO {
            continue;
        }

        if let Ok(mut goal) = goals.get_mut(result.agent) {
            goal.achieved = true;
        }

        // Ground truth for later pricing. Recorded here because this is the
        // only moment anything knows a person chose it: once the contaminant is
        // in the bowl it is ordinary solution, which is the whole point.
        tampered.record(meal, result.agent);

        // Only observers who could actually perceive it learn anything, and
        // what they learn is deliberately ambiguous: `SuspiciousHandling` is
        // the same kind innocent food handling would produce. Separating the
        // two is an investigation's job, not a stimulus's.
        witnessed.write(
            super::Stimulus::new(super::StimulusKind::SuspiciousHandling, meal_at)
                .about(meal)
                .by(result.agent)
                .with_strength(0.8),
        );
        let _ = now;
    }
}

/// Meals a covert act contaminated, and who did it.
///
/// The link between an act and the harm it later causes. Kept because the
/// contaminant becomes ordinary `Solution` — deliberately indistinguishable
/// from any other exposure once it is in the bowl — so by the time someone
/// collapses there is nothing left in the chemistry to say a person chose it.
///
/// Authority-only ground truth. It is *not* evidence: nothing reads it to
/// decide what Security knows, which still comes from witnesses and testimony
/// via `interviews`. It exists so consequences can be priced honestly, and so
/// an investigation that independently names the culprit is confirming
/// something rather than inventing it.
#[derive(Resource, Default)]
pub struct TamperedMeals {
    meals: Vec<(Entity, Entity)>,
}

impl TamperedMeals {
    fn record(&mut self, meal: Entity, actor: Entity) {
        if !self.meals.iter().any(|(known, _)| *known == meal) {
            self.meals.push((meal, actor));
        }
    }

    /// Who contaminated this meal, if anyone did.
    pub fn culprit(&self, meal: Entity) -> Option<Entity> {
        self.meals
            .iter()
            .find(|(known, _)| *known == meal)
            .map(|(_, actor)| *actor)
    }

    /// Marks a body as having eaten from a tampered meal, so the poisoning it
    /// develops later can be traced back.
    pub(super) fn carry_to(&mut self, diner: Entity, meal: Entity) {
        if let Some(actor) = self.culprit(meal) {
            self.record(diner, actor);
        }
    }

    fn clear(&mut self) {
        self.meals.clear();
    }
}

/// Prices the consequences of a covert act, when and only when it hurts someone.
///
/// Deliberately triggered by `IncidentCreated` rather than by the act itself. A
/// contaminated meal nobody eats destabilises nothing and damages no
/// relationship — the antagonist took a risk and got away with it, which is a
/// real outcome and should not be indistinguishable from a casualty. This is
/// also why `covert.rs` does not price its own action: the act is not the harm.
///
/// Three consequences, each on the meter that already owns it:
///
/// - **Instability** via `StabilityEvent::HostileSucceeded`, the variant that
///   was priced and tested but had no emitter until now.
/// - **Campaign pressure** via `arc::nudge_plot`, which no-ops on a resolved
///   arc so a finished campaign cannot be reopened by a late poisoning.
/// - **Relationships**: the victim's own department loses standing, because
///   from the inside this reads as a department that cannot keep its people
///   safe. The *culprit's* standing is untouched — nobody knows yet, and moving
///   it here would leak private guilt into a public number.
fn price_covert_harm(
    mut created: MessageReader<super::IncidentCreated>,
    tampered: Res<TamperedMeals>,
    mut ledger: ResMut<super::IncidentLedger>,
    mut stability: MessageWriter<crate::instability::StabilityEvent>,
    mut campaign: Option<ResMut<crate::arc::Campaign>>,
    mut shift: ResMut<crate::orders::Shift>,
) {
    for incident in created.read() {
        if incident.kind != super::IncidentKind::Poisoning {
            continue;
        }
        let Some(culprit) = tampered.culprit(incident.subject) else {
            // An ordinary poisoning — a Botany accident, a bad batch. Not this
            // system's business.
            continue;
        };

        // Ground truth, recorded so a later investigation is checked against
        // something. `attribute` never overwrites, so this cannot contradict a
        // department adapter that already knew its own culprit.
        let _ = ledger.attribute(incident.id, culprit);

        // Severity is the victim's, not the dose's: what destabilises a station
        // is how badly someone was hurt, and a large dose that barely landed
        // should not read as a catastrophe.
        let severity = if incident.severity.get() >= MAJOR_HARM {
            crate::instability::HostileSeverity::Major
        } else {
            crate::instability::HostileSeverity::Minor
        };
        stability.write(crate::instability::StabilityEvent::HostileSucceeded(
            severity,
        ));

        if let Some(campaign) = campaign.as_deref_mut() {
            crate::arc::nudge_plot(campaign, COVERT_PLOT_PRESSURE);
        }

        shift.adjust(
            department_of(incident.department),
            DEPARTMENT_CONFIDENCE_COST,
        );
    }
}

/// Above this victim severity a poisoning reads as a major hostile success.
const MAJOR_HARM: f32 = 0.6;

/// How much a successful covert harm advances the campaign.
const COVERT_PLOT_PRESSURE: i32 = 2;

/// What the victim's department loses when one of its own is poisoned on shift.
const DEPARTMENT_CONFIDENCE_COST: i32 = -1;

/// Maps a job domain onto the standing department that answers for it.
fn department_of(domain: super::JobDomain) -> crate::orders::Department {
    domain.department()
}

fn reset_covert_state(mut custody: ResMut<IllicitCustody>, mut tampered: ResMut<TamperedMeals>) {
    custody.clear();
    tampered.clear();
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<IllicitCustody>()
        .init_resource::<TamperedMeals>()
        .add_systems(
            OnEnter(crate::AppState::Playing),
            reset_covert_state
                .in_set(super::UtilityResetSet)
                .run_if(crate::net::is_authority),
        )
        .add_systems(
            Update,
            offer_food_poisoning
                .in_set(super::OpportunityProviders)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            apply_food_poisoning
                .after(super::resolve_reference_actions)
                .in_set(super::UtilityAiSet::Resolve)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            price_covert_harm
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utility_ai::service::{MealBatch, MealChemistry, MealStage, ServiceRecipe};
    use crate::utility_ai::{ActionKey, ReservationOwner, UtilityControlBundle};

    fn chem() -> chem_sim::ChemData {
        chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap()
    }

    fn contaminant(data: &chem_sim::ChemData) -> chem_sim::ReagentId {
        data.reagents
            .id_of("quiet_rot")
            .expect("the authored contaminant exists")
    }

    fn stock(reagent: chem_sim::ReagentId, ml: f64) -> IllicitStock {
        let mut solution = chem_sim::Solution::unbounded();
        let _ = solution.add(reagent, chem_sim::Units::from_f64(ml));
        IllicitStock {
            solution,
            state: CustodyState::Carried,
            source_player: None,
            received_at: 0.0,
            claimed_label: "for the beds".into(),
        }
    }

    /// Builds an app with just the consequence-pricing system.
    fn pricing_app() -> App {
        let mut app = App::new();
        app.init_resource::<TamperedMeals>()
            .init_resource::<super::super::IncidentLedger>()
            .init_resource::<crate::orders::Shift>()
            .insert_resource(crate::arc::Campaign::new(
                crate::arc::AntagId::Spy,
                crate::arc::Mode::Chemist,
                3,
            ))
            .add_message::<super::super::IncidentCreated>()
            .add_message::<crate::instability::StabilityEvent>()
            .add_systems(Update, price_covert_harm);
        app
    }

    fn poisoning(subject: Entity) -> super::super::IncidentCreated {
        super::super::IncidentCreated {
            id: super::super::IncidentId(1),
            kind: super::super::IncidentKind::Poisoning,
            department: super::super::JobDomain::Service,
            subject,
            location: Vec3::ZERO,
            severity: Normalized::new(0.8).unwrap(),
        }
    }

    /// The rule that keeps consequences honest: the *act* is not the harm.
    ///
    /// A contaminated meal nobody eats destabilises nothing. If pricing fired
    /// when the reagent went into the bowl, an antagonist who took a risk and
    /// got away with it would be indistinguishable from one who put someone in
    /// Medical — and the player could never affect the outcome by recovering
    /// the batch or clearing the meal.
    #[test]
    fn a_covert_act_that_hurts_nobody_costs_the_station_nothing() {
        let mut app = pricing_app();
        let culprit = Entity::from_raw_u32(5).unwrap();
        let meal = Entity::from_raw_u32(9).unwrap();
        app.world_mut()
            .resource_mut::<TamperedMeals>()
            .record(meal, culprit);
        app.update();

        assert_eq!(app.world().resource::<crate::arc::Campaign>().plot, 0);
        assert_eq!(
            app.world()
                .resource::<crate::orders::Shift>()
                .standing(crate::orders::Department::Service),
            0,
            "tampering alone must not move a department's standing"
        );
    }

    /// An ordinary poisoning is not this system's business.
    #[test]
    fn a_poisoning_with_no_tampered_source_is_left_alone() {
        let mut app = pricing_app();
        let victim = Entity::from_raw_u32(7).unwrap();
        app.world_mut().write_message(poisoning(victim));
        app.update();

        assert_eq!(
            app.world().resource::<crate::arc::Campaign>().plot,
            0,
            "a Botany accident must not advance the campaign"
        );
    }

    /// The taint follows the body, so the *kind* of harm still has to match.
    ///
    /// Someone who ate a tampered meal and later burns their hand on an
    /// unrelated fault has not been poisoned by anyone. Without the kind check
    /// that burn would be priced as covert harm and attributed to the
    /// antagonist — inventing both a crime and a culprit from a coincidence.
    #[test]
    fn an_unrelated_injury_to_someone_who_ate_a_tampered_meal_is_not_covert_harm() {
        let mut app = pricing_app();
        let culprit = Entity::from_raw_u32(5).unwrap();
        let victim = Entity::from_raw_u32(7).unwrap();
        app.world_mut()
            .resource_mut::<TamperedMeals>()
            .record(victim, culprit);

        let mut burn = poisoning(victim);
        burn.kind = super::super::IncidentKind::Burn;
        app.world_mut().write_message(burn);
        app.update();

        assert_eq!(
            app.world().resource::<crate::arc::Campaign>().plot,
            0,
            "a burn is not a poisoning, whoever the victim had lunch with"
        );
        assert_eq!(
            app.world()
                .resource::<crate::orders::Shift>()
                .standing(crate::orders::Department::Service),
            0
        );
    }

    /// Real harm reaches all three meters, and names the culprit as ground truth.
    #[test]
    fn a_covert_poisoning_moves_stability_campaign_and_department_standing() {
        let mut app = pricing_app();
        let culprit = Entity::from_raw_u32(5).unwrap();
        let victim = Entity::from_raw_u32(7).unwrap();
        app.world_mut()
            .resource_mut::<TamperedMeals>()
            .record(victim, culprit);
        app.world_mut().write_message(poisoning(victim));
        app.update();

        assert!(
            app.world().resource::<crate::arc::Campaign>().plot > 0,
            "a successful covert harm must advance the campaign"
        );
        assert!(
            app.world()
                .resource::<crate::orders::Shift>()
                .standing(crate::orders::Department::Service)
                < 0,
            "the victim's department looks unable to keep its people safe"
        );

        // The culprit's own standing is untouched: nobody knows yet, and moving
        // it would leak private guilt into a public number.
        assert_eq!(
            app.world()
                .resource::<crate::orders::Shift>()
                .npc_standing
                .len(),
            crate::orders::Department::Service.members().len(),
            "only the victim's department moved"
        );
    }

    /// Save/load preserves unused stock and its provenance, and — the half that
    /// actually needs guarding — does not duplicate it.
    #[test]
    fn custody_survives_a_reload_with_its_exact_volume_and_no_duplication() {
        let data = chem();
        let reagent = contaminant(&data);
        let holder = Entity::from_raw_u32(4).unwrap();

        let mut custody = IllicitCustody::default();
        custody.receive_from_player(holder, stock(reagent, EFFECTIVE_DOSE_ML * 2.5));
        // Spend part of it first: the interesting thing to preserve is a
        // part-used batch, because that is what stops funding a second incident.
        let mut meal = chem_sim::Solution::unbounded();
        let spent = custody.spend(
            holder,
            reagent,
            chem_sim::Units::from_f64(EFFECTIVE_DOSE_ML),
            &mut meal,
        );
        assert!(spent.is_positive());
        let before = custody
            .held_by(holder)
            .map(|held| held.solution.volume_of(reagent))
            .fold(chem_sim::Units::ZERO, |a, b| a + b);

        let saved = custody.snapshot(|_| Some("Grower Aleksy".to_string()));
        assert_eq!(saved.len(), 1, "one live batch, one record");
        assert_eq!(
            saved[0].claimed_label, "for the beds",
            "provenance survives"
        );

        // A fresh session: new entity id for the same person.
        let reloaded_holder = Entity::from_raw_u32(91).unwrap();
        let mut fresh = IllicitCustody::default();
        fresh.restore(&saved, |name| {
            (name == "Grower Aleksy").then_some(reloaded_holder)
        });
        let after = fresh
            .held_by(reloaded_holder)
            .map(|held| held.solution.volume_of(reagent))
            .fold(chem_sim::Units::ZERO, |a, b| a + b);
        assert_eq!(after, before, "the exact remaining volume comes back");

        // Restoring twice must not double the batch — a player who reloads
        // twice would otherwise hand the antagonist a second dose.
        fresh.restore(&saved, |name| {
            (name == "Grower Aleksy").then_some(reloaded_holder)
        });
        assert_eq!(
            fresh.held_by(reloaded_holder).count(),
            1,
            "restore replaces, it must never append"
        );
    }

    /// A batch the player already dealt with must not come back.
    #[test]
    fn a_confiscated_batch_is_not_restored_by_a_reload() {
        let data = chem();
        let reagent = contaminant(&data);
        let holder = Entity::from_raw_u32(4).unwrap();

        let mut custody = IllicitCustody::default();
        custody.receive_from_player(holder, stock(reagent, EFFECTIVE_DOSE_ML * 2.0));
        assert_eq!(custody.resolve(holder, CustodyState::Confiscated), 1);

        let saved = custody.snapshot(|_| Some("Grower Aleksy".to_string()));
        assert!(
            saved.is_empty(),
            "reloading must not undo a confiscation the player earned"
        );
    }

    /// A holder who cannot be named is dropped rather than saved under a
    /// placeholder, and a saved name with nobody to match is skipped.
    #[test]
    fn custody_with_no_identifiable_holder_does_not_persist() {
        let data = chem();
        let reagent = contaminant(&data);
        let holder = Entity::from_raw_u32(4).unwrap();

        let mut custody = IllicitCustody::default();
        custody.receive_from_player(holder, stock(reagent, EFFECTIVE_DOSE_ML * 2.0));
        assert!(
            custody.snapshot(|_| None).is_empty(),
            "a batch nobody can be identified as holding is unrecoverable and must not persist"
        );

        // And the reverse: a record naming someone absent restores nothing.
        let saved = custody.snapshot(|_| Some("Grower Aleksy".to_string()));
        let mut fresh = IllicitCustody::default();
        assert_eq!(fresh.restore(&saved, |_| None), 0);
    }

    /// The supply invariant, stated as a test against the authored data rather
    /// than trusted: the contaminant this thread depends on is player-only, so
    /// nothing in the game can spawn it for an NPC.
    #[test]
    fn the_contaminant_is_player_only() {
        let data = chem();
        let id = contaminant(&data);
        assert!(
            data.reagents.get(id).player_only,
            "a covert action must depend on a reagent only a player can supply"
        );
        assert!(!data.reagents.get(id).dispensable);
    }

    /// Spending draws down real volume, so a batch cannot fund two incidents
    /// unless enough remains for both. This is the difference between tracking
    /// a physical batch and tracking a flag.
    #[test]
    fn spending_removes_real_volume_and_no_hidden_copy_remains() {
        let data = chem();
        let reagent = contaminant(&data);
        let holder = Entity::from_raw_u32(1).unwrap();
        let mut custody = IllicitCustody::default();
        // Exactly enough for one effective dose, not two.
        custody.receive_from_player(holder, stock(reagent, EFFECTIVE_DOSE_ML * 1.5));

        let mut meal = chem_sim::Solution::unbounded();
        let moved = custody.spend(
            holder,
            reagent,
            chem_sim::Units::from_f64(EFFECTIVE_DOSE_ML),
            &mut meal,
        );
        assert!(moved > chem_sim::Units::ZERO, "the first dose is funded");
        assert_eq!(meal.volume_of(reagent), moved, "the volume really moved");

        // The remainder is below an effective dose, so a second action is
        // vetoed rather than merely scored lower.
        assert!(
            !custody.held_by(holder).any(|held| held.usable(reagent)),
            "a part-spent batch must stop funding a second incident"
        );
        let mut second = chem_sim::Solution::unbounded();
        assert_eq!(
            custody.spend(
                holder,
                reagent,
                chem_sim::Units::from_f64(EFFECTIVE_DOSE_ML),
                &mut second
            ),
            chem_sim::Units::ZERO
        );
    }

    /// Confiscation, return, and destruction differ in consequence but not in
    /// physics: all of them take the batch out of NPC hands for good.
    #[test]
    fn resolved_custody_can_no_longer_fund_an_action() {
        let data = chem();
        let reagent = contaminant(&data);
        let holder = Entity::from_raw_u32(1).unwrap();

        for state in [
            CustodyState::Confiscated,
            CustodyState::Destroyed,
            CustodyState::Returned,
        ] {
            let mut custody = IllicitCustody::default();
            custody.receive_from_player(holder, stock(reagent, EFFECTIVE_DOSE_ML * 4.0));
            // Positive control: it was usable a moment ago.
            assert!(custody.held_by(holder).any(|held| held.usable(reagent)));

            assert_eq!(custody.resolve(holder, state), 1);
            assert!(
                !custody.held_by(holder).any(|held| held.usable(reagent)),
                "{state:?} stock must not fund an action"
            );
        }
    }

    /// A motive that poisoning cannot serve produces no offer. This is what
    /// stops "the antagonist" collapsing into one trick performed on sight.
    #[test]
    fn a_grudge_is_not_served_by_poisoning_a_shared_meal() {
        assert!(PrivateGoal::DiscreditDepartment.served_by_poisoning());
        assert!(PrivateGoal::SowDisorder.served_by_poisoning());
        assert!(
            !PrivateGoal::Grudge.served_by_poisoning(),
            "a communal batch cannot target one person, so poisoning it serves no grudge"
        );
    }

    /// The reference scenario, end to end through the real systems: a stocked
    /// antagonist is offered the act, and an identical one without stock is
    /// not. The offer half of "it cannot happen without player supply".
    #[test]
    fn only_an_antagonist_holding_player_supplied_stock_is_offered_the_act() {
        let data = chem();
        let reagent = contaminant(&data);

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<UtilityOpportunityBuffer>()
            .init_resource::<IllicitCustody>()
            .insert_resource(CovertContaminant { reagent })
            .add_systems(Update, offer_food_poisoning);

        let mut solution = chem_sim::Solution::unbounded();
        let _ = solution.add(reagent, chem_sim::Units::from_f64(1.0));
        app.world_mut().spawn((
            MealBatch {
                id: 1,
                recipe: ServiceRecipe::GardenPlate,
                stage: MealStage::Served,
                servings_remaining: 4,
                hosted: true,
                quality_percent: 80,
            },
            MealChemistry {
                solution,
                ingredients: Vec::new(),
                prepared_by: Entity::from_raw_u32(90).unwrap(),
                prepared_at: 0.0,
            },
            Transform::from_xyz(1.0, 0.0, 0.0),
        ));

        let antagonist = |app: &mut App, seed: u64| {
            app.world_mut()
                .spawn((
                    CrewMember {
                        name: format!("Grower {seed}"),
                        role: "Botany".into(),
                    },
                    Transform::default(),
                    UtilityControlBundle::new(UtilityAgent::new(seed, 0)),
                    CovertGoal::new(PrivateGoal::DiscreditDepartment, 0.9),
                ))
                .id()
        };
        let supplied = antagonist(&mut app, 1);
        let unsupplied = antagonist(&mut app, 2);
        // Fully stocked, but holding a motive poisoning cannot serve, and one
        // whose goal is already met. Both must be offered nothing.
        let grudge = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Grower 3".into(),
                    role: "Botany".into(),
                },
                Transform::default(),
                UtilityControlBundle::new(UtilityAgent::new(3, 0)),
                CovertGoal::new(PrivateGoal::Grudge, 0.9),
            ))
            .id();
        let satisfied = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Grower 4".into(),
                    role: "Botany".into(),
                },
                Transform::default(),
                UtilityControlBundle::new(UtilityAgent::new(4, 0)),
                CovertGoal {
                    achieved: true,
                    ..CovertGoal::new(PrivateGoal::DiscreditDepartment, 0.9)
                },
            ))
            .id();

        for holder in [supplied, grudge, satisfied] {
            app.world_mut()
                .resource_mut::<IllicitCustody>()
                .receive_from_player(holder, stock(reagent, EFFECTIVE_DOSE_ML * 4.0));
        }
        app.update();

        let buffer = app.world().resource::<UtilityOpportunityBuffer>();
        assert!(
            buffer
                .for_agent(supplied)
                .any(|offer| offer.action == UtilityActionId::PoisonFood),
            "an antagonist holding a player-supplied batch can act"
        );
        assert!(
            buffer.for_agent(unsupplied).next().is_none(),
            "an identical antagonist with no player-supplied stock has no covert option at all"
        );
        assert!(
            buffer.for_agent(grudge).next().is_none(),
            "stock is not enough: poisoning a shared meal cannot serve a grudge"
        );
        assert!(
            buffer.for_agent(satisfied).next().is_none(),
            "an antagonist whose goal is met returns to ordinary life"
        );
        // The act must never outrank a real emergency.
        let bucket = buffer.for_agent(supplied).next().unwrap().bucket;
        assert_eq!(bucket, UtilityBucket::Routine);
    }

    /// The other half: performing the act moves real reagent into the meal's
    /// ordinary solution, so harm travels the same chemistry path as any other
    /// exposure — and an observer learns only an *ambiguous* handling stimulus.
    #[test]
    fn completing_the_act_contaminates_the_real_meal_solution() {
        let data = chem();
        let reagent = contaminant(&data);

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<IllicitCustody>()
            .init_resource::<TamperedMeals>()
            .insert_resource(CovertContaminant { reagent })
            .add_message::<UtilityActionResolved>()
            .add_message::<super::super::Stimulus>()
            .add_systems(Update, apply_food_poisoning);

        let mut solution = chem_sim::Solution::unbounded();
        let _ = solution.add(reagent, chem_sim::Units::ZERO);
        let meal = app
            .world_mut()
            .spawn((
                MealBatch {
                    id: 1,
                    recipe: ServiceRecipe::GardenPlate,
                    stage: MealStage::Served,
                    servings_remaining: 4,
                    hosted: true,
                    quality_percent: 80,
                },
                MealChemistry {
                    solution,
                    ingredients: Vec::new(),
                    prepared_by: Entity::from_raw_u32(90).unwrap(),
                    prepared_at: 0.0,
                },
                Transform::from_xyz(1.0, 0.0, 0.0),
            ))
            .id();
        let actor = app
            .world_mut()
            .spawn((
                Transform::default(),
                UtilityControlBundle::new(UtilityAgent::new(1, 0)),
                CovertGoal::new(PrivateGoal::DiscreditDepartment, 0.9),
            ))
            .id();
        app.world_mut()
            .resource_mut::<IllicitCustody>()
            .receive_from_player(actor, stock(reagent, EFFECTIVE_DOSE_ML * 4.0));

        let before = app
            .world()
            .get::<MealChemistry>(meal)
            .unwrap()
            .solution
            .volume_of(reagent);
        app.world_mut().write_message(UtilityActionResolved {
            agent: actor,
            key: ActionKey {
                action: UtilityActionId::PoisonFood,
                target_key: stable_text_key(&format!("covert.poison.{:016x}", meal.to_bits())),
            },
            claim: ReservationOwner {
                agent: actor,
                action_instance: 1,
            },
            result: ActionResult::Completed,
        });
        app.update();

        let after = app
            .world()
            .get::<MealChemistry>(meal)
            .unwrap()
            .solution
            .volume_of(reagent);
        assert!(
            after > before,
            "the contaminant must become part of the meal's ordinary solution"
        );
        // The meal's public appearance is fixed when the legitimate ingredients
        // cook, so it cannot have changed to advertise the contaminant.
        assert_eq!(
            app.world().get::<MealBatch>(meal).unwrap().quality_percent,
            80
        );
        // The goal is served, so the antagonist returns to ordinary life.
        assert!(app.world().get::<CovertGoal>(actor).unwrap().achieved);
    }
}
