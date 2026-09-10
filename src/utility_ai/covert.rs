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

/// How long an actor waits before reconsidering a covert act that fell through.
///
/// Without this, an attempt stopped by someone walking past is re-offered the
/// instant they walk on — so a player who interrupts something buys a second of
/// delay rather than a reprieve, and the actor reads as mechanically persistent
/// rather than cautious. Successful prevention should be worth something.
///
/// Shared by both covert actions on purpose: it is a property of having just
/// been spooked, not of a particular trick.
pub(super) const COVERT_RETRY_COOLDOWN: f32 = 30.0;

/// When each actor may next consider a covert act.
///
/// Authority-only and keyed by action as well as actor, so being scared off a
/// bowl of soup does not also postpone something unrelated.
#[derive(Resource, Default)]
pub(super) struct CovertCooldowns {
    until: Vec<(Entity, UtilityActionId, f32)>,
}

impl CovertCooldowns {
    pub(super) fn blocked(&self, actor: Entity, action: UtilityActionId, now: f32) -> bool {
        self.until
            .iter()
            .any(|(who, what, until)| *who == actor && *what == action && now < *until)
    }

    pub(super) fn begin(&mut self, actor: Entity, action: UtilityActionId, now: f32) {
        let until = now + COVERT_RETRY_COOLDOWN;
        if let Some(entry) = self
            .until
            .iter_mut()
            .find(|(who, what, _)| *who == actor && *what == action)
        {
            entry.2 = until;
        } else {
            self.until.push((actor, action, until));
        }
    }

    fn clear(&mut self) {
        self.until.clear();
    }
}

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
    ///
    /// Public because possession is also what re-arms a motive across a
    /// reload: a batch that was confiscated, spent or returned leaves nothing
    /// to act with, and the owning thread has to be able to ask that question
    /// before restoring a goal. See `botanist::restore_covert_goal`.
    pub fn is_spendable(self) -> bool {
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
        self.state.is_spendable()
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
            if *who == holder && stock.state.is_spendable() {
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
            .filter(|(_, stock)| stock.state.is_spendable())
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

/// How long the glassware-handling action takes.
///
/// Matches [`POISON_SECONDS`]: both are "long enough that a witness has a real
/// chance to be present, short enough to be plausible work", and there is no
/// reason for the two to differ.
const TAMPER_SECONDS: f32 = 4.0;

/// How long an armed tampering authorization stays usable, in seconds.
///
/// The bound that makes this preventable by ordinary means. It elapses while
/// the actor does ordinary work, so watching them, guarding the bench, or
/// simply keeping the glassware in hand for two minutes defeats *this* attempt
/// — without needing the player to know an attempt existed.
const TAMPER_WINDOW_SECONDS: f32 = 120.0;

/// Base appeal of a tampering opportunity, before duty and ability weigh in.
///
/// Deliberately below the appeal of urgent department work and above idling: a
/// tech with a grudge and a free moment takes it, a tech with a real fault to
/// fix does the job.
///
/// **Measured, not assumed.** Swept against a real selector with a posted,
/// profiled actor and `MaintainPost` as the live competitor
/// (`acceptance::tests::the_authored_appeal_beats_standing_at_a_post`): the
/// crossover sits between 0.20 and 0.30, which is where `MAINTAIN_POST_WEIGHT`
/// (0.25) puts it. 0.65 therefore clears idling comfortably without approaching
/// the 0.30-0.50 band that Security and Bridge tickets occupy, so it needed no
/// adjustment. Anything at or below 0.25 makes this action unreachable in
/// practice — an antagonist armed, unobserved, and permanently inert.
const TAMPER_APPEAL: f32 = 0.65;

/// One bounded licence to adulterate one container.
///
/// The replacement for the old scripted errand, and the difference is what it
/// *does not* do: it does not choose a target, does not move anybody, and does
/// not guarantee an effect. It says only that this person, for the next two
/// minutes, would take the opportunity if a safe one presented itself. The
/// utility scorer decides whether one ever does.
///
/// Authority-only. A client that could see this component would know who the
/// antagonist is, which is the one thing that must stay hidden.
#[derive(Component, Clone, Debug)]
pub struct TamperAuthorization {
    /// The reagent this attempt would use.
    pub reagent: chem_sim::ReagentId,
    /// How much of it remains available to this authorization.
    ///
    /// Materialised once when arming and drawn down only on a real transfer, so
    /// an interrupted attempt does not silently refill. Discarded whole when the
    /// window closes.
    pub allotment: chem_sim::Units,
    /// When the window closes. Absolute session time.
    pub expires_at: f32,
}

impl TamperAuthorization {
    pub fn new(reagent: chem_sim::ReagentId, allotment: chem_sim::Units, now: f32) -> Self {
        Self {
            reagent,
            allotment,
            expires_at: now + TAMPER_WINDOW_SECONDS,
        }
    }

    pub fn expired(&self, now: f32) -> bool {
        now >= self.expires_at
    }
}

/// Everything a covert actor needs to judge one opportunity, gathered once.
///
/// A `SystemParam` rather than loose arguments because both covert actions ask
/// the same questions — can I see it, can anyone see me — and the answers must
/// come from one place. Two providers with their own geometry would eventually
/// disagree about what counts as visible, and the resulting behaviour would be
/// impossible to explain from the player's chair.
#[derive(bevy::ecs::system::SystemParam)]
pub(super) struct CovertSight<'w, 's> {
    areas: Option<Res<'w, crate::lab::WalkableAreas>>,
    solids: Query<'w, 's, (&'static Transform, &'static crate::lab::Solid)>,
    /// Everyone who could witness something: conscious crew and players alike.
    /// Deliberately not filtered to `UtilityAgent` — a player standing in the
    /// room is the most important observer there is.
    observers: Query<
        'w,
        's,
        (
            Entity,
            &'static Transform,
            Option<&'static crate::body::Bloodstream>,
        ),
        Or<(With<CrewMember>, With<crate::player::Chemist>)>,
    >,
    concealment: Query<'w, 's, &'static crate::body::Bloodstream>,
}

impl CovertSight<'_, '_> {
    /// The solid geometry, resolved once per use rather than per question.
    fn boxes(&self) -> Vec<(Vec3, Vec3)> {
        self.solids
            .iter()
            .map(|(transform, solid)| (transform.translation, solid.half_extents))
            .collect()
    }

    /// How far this actor's own body registers to someone looking for it.
    fn concealment_of(&self, actor: Entity) -> f32 {
        self.concealment
            .get(actor)
            .map_or(0.0, |blood| blood.0.concealment())
    }

    /// Whether `actor` can currently see `point`.
    ///
    /// Full strength: this is an actor looking deliberately at a thing it is
    /// considering, not a bystander catching a glimpse.
    fn can_see(&self, boxes: &[(Vec3, Vec3)], from: Vec3, point: Vec3) -> bool {
        let Some(areas) = self.areas.as_deref() else {
            return false;
        };
        super::perception::can_see(
            from,
            point,
            areas,
            boxes,
            super::perception::concealed_sight_range(0.0, 1.0),
        )
    }

    /// Whether anyone the actor can perceive has a view of `handling`.
    ///
    /// Two conditions, both required, and the asymmetry is the point:
    ///
    /// - The actor must be able to perceive the observer. Someone it cannot see
    ///   does not enter its risk estimate — it is not omniscient about who is
    ///   nearby, and pretending otherwise would make it hide from people it has
    ///   no way of knowing about.
    /// - The observer must have line of sight to where the act would happen,
    ///   tested with the actor's own concealment applied. A chemically concealed
    ///   actor really is harder to notice, and this is the same function the
    ///   witness side uses, so the two cannot drift apart.
    ///
    /// Someone the actor failed to notice can still witness the act through
    /// ordinary sensing afterwards. That is not a contradiction: it is what
    /// getting caught looks like.
    fn observed(&self, actor: Entity, from: Vec3, handling: Vec3) -> bool {
        let Some(areas) = self.areas.as_deref() else {
            // No map to reason about. Refuse rather than assume privacy.
            return true;
        };
        let boxes = self.boxes();
        let hidden = self.concealment_of(actor);
        let watched_range = super::perception::concealed_sight_range(hidden, 1.0);

        self.observers.iter().any(|(who, at, blood)| {
            if who == actor {
                return false;
            }
            // An unconscious body witnesses nothing, and a collapsed one is a
            // casualty rather than a lookout.
            if blood.is_some_and(|blood| blood.0.incapacitated()) {
                return false;
            }
            let them = at.translation;
            self.can_see(&boxes, from, them)
                && super::perception::can_see(them, handling, areas, &boxes, watched_range)
        })
    }
}

/// Whether a remembered object is worth walking to.
///
/// First discovery requires sight; after that a still-valid memory is enough to
/// set off. The location used is the one from the memory — where it was last
/// *seen* — never a live position, because an actor that re-read the current
/// transform of something it cannot see would be tracking it remotely.
fn known_target(
    memory: Option<&super::perception::NpcMemory>,
    subject: Entity,
    now: f32,
) -> Option<Vec3> {
    let memory = memory?;
    memory
        .best_about(subject, super::perception::StimulusKind::Food, now)
        .map(|fact| fact.at)
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
    cooldowns: Res<CovertCooldowns>,
    meals: Query<(
        Entity,
        &Transform,
        &super::service::MealBatch,
        &super::service::MealChemistry,
    )>,
    sight: CovertSight,
    actors: Query<
        (
            Entity,
            &CrewMember,
            &Transform,
            &CovertGoal,
            Option<&super::perception::NpcMemory>,
        ),
        With<UtilityAgent>,
    >,
) {
    let Some(contaminant) = contaminant else {
        return;
    };
    let now = time.elapsed_secs();

    for (actor, member, at, goal, memory) in &actors {
        // An achieved goal stops. An antagonist who has what they wanted goes
        // back to ordinary life instead of escalating without end.
        if goal.achieved || !goal.goal.served_by_poisoning() {
            continue;
        }
        // Recently spooked. Waiting is the point: an attempt that fell through
        // should cost more than the second it takes the witness to move on.
        if cooldowns.blocked(actor, UtilityActionId::PoisonFood, now) {
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

        let boxes = sight.boxes();
        let here = at.translation;

        // Candidates are what this actor *knows about*, not what exists.
        //
        // This used to read the world directly: every served meal within 24
        // metres, ranked by servings. That made an antagonist omniscient about
        // the contents of rooms it had never entered, and — worse — meant no
        // player action short of removing the meal could affect its choice.
        // Standing guard did nothing, because nothing was looking.
        //
        // Now a meal qualifies two ways: it is visible right now, or it was
        // seen recently enough to still be remembered. Both produce a *known*
        // location, which is what the actor walks to.
        let mut reachable: Vec<(Entity, Vec3, u8)> = meals
            .iter()
            .filter(|(_, _, batch, _)| {
                batch.stage == super::service::MealStage::Served
                    && batch.servings_remaining >= WORTHWHILE_SERVINGS
            })
            .filter_map(|(meal, meal_at, batch, _)| {
                let live = meal_at.translation;
                // Seeing it now is first-hand and gives current detail:
                // servings included, because the actor is looking at the bowl.
                if sight.can_see(&boxes, here, live) {
                    return (live.distance(here) <= REACH)
                        .then_some((meal, live, batch.servings_remaining));
                }
                // Otherwise a memory, at its remembered location. Servings are
                // deliberately not read here — ranking an unseen meal by its
                // true current contents is exactly the omniscience this gate
                // exists to remove, so a remembered meal is scored at the
                // threshold it had to clear to be worth remembering.
                let remembered = known_target(memory, meal, now)?;
                (remembered.distance(here) <= REACH)
                    .then_some((meal, remembered, WORTHWHILE_SERVINGS))
            })
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

        // The witness veto.
        //
        // `nerve`'s own doc has always described this — "scales the witness
        // gate, so a cautious antagonist genuinely waits for a better moment"
        // — but no witness gate existed and nerve was only ever a score
        // multiplier. For this phase the rule is simple and absolute: one
        // perceived observer with a view of the counter is enough to call it
        // off. These are cautious people, and a cautious person does not do
        // this in front of someone.
        //
        // A vetoed opportunity is skipped rather than scored low. "Too risky"
        // and "worth slightly less" are different things, and collapsing them
        // produces an antagonist who eventually does everything.
        let Some(&(meal, meal_at, servings)) = reachable
            .iter()
            .find(|(_, meal_at, _)| !sight.observed(actor, here, *meal_at))
        else {
            continue;
        };

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
    mut cooldowns: ResMut<CovertCooldowns>,
    mut witnessed: MessageWriter<super::Stimulus>,
    sight: CovertSight,
    actors: Query<&Transform, With<CovertGoal>>,
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
        if result.key.action != UtilityActionId::PoisonFood {
            continue;
        }
        if result.result != ActionResult::Completed {
            // Interrupted, unreachable, timed out, or the claim was lost. Any
            // of those means the moment has passed; wait before looking again.
            cooldowns.begin(result.agent, UtilityActionId::PoisonFood, now);
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
        // Reacquire before acting.
        //
        // Selection happened at least a walk ago, and the world moves. The
        // actor has to be able to see the bowl it is standing over — someone
        // may have carried it into another room, or closed a door between, or
        // simply be standing there now. Choosing a target is not permission to
        // act on it later.
        let Ok(actor_at) = actors.get(result.agent).map(|at| at.translation) else {
            continue;
        };
        let boxes = sight.boxes();
        if !sight.can_see(&boxes, actor_at, meal_at)
            || sight.observed(result.agent, actor_at, meal_at)
        {
            // The bowl moved out of view, or somebody arrived while they were
            // walking. This is the moment a player interrupting an act actually
            // interrupts it — and the actor waits before trying again rather
            // than hovering until the room happens to empty.
            cooldowns.begin(result.agent, UtilityActionId::PoisonFood, now);
            continue;
        }

        // Physical transfer. If custody has nothing left, nothing moves and the
        // action was simply wasted effort — no flag flips on an empty batch.
        let Ok((_, _, _, mut chemistry)) = meals.get_mut(meal) else {
            continue;
        };
        let moved = custody.spend(
            result.agent,
            contaminant.reagent,
            chem_sim::Units::from_f64(EFFECTIVE_DOSE_ML),
            &mut chemistry.solution,
        );
        // An effective dose or nothing.
        //
        // `spend` returns overflow to custody when the destination is at its
        // volume cap, so topping up a full bowl lands a token amount and leaves
        // the batch nearly intact. That is not a poisoning, and marking it as
        // one used to satisfy the motive permanently for a few drops.
        if moved < chem_sim::Units::from_f64(EFFECTIVE_DOSE_ML) {
            continue;
        }

        // The act happened. Note what this does *not* claim: that anybody was
        // hurt. The meal may be recovered before a single serving is eaten —
        // the actor took its risk and does not get to know how it turned out.
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

/// Glassware an actor can get at, and the geometry to reach it.
///
/// Held, slotted or stored containers are excluded, which is the same
/// definition `saboteur` and `smuggler` have always used: anything in a hand, a
/// machine or a locker is under someone's eye, and "keep hold of it" stays a
/// real answer.
type LooseGlassware<'w, 's> = Query<
    'w,
    's,
    (
        Entity,
        &'static crate::containers::Container,
        &'static Transform,
    ),
    (
        Without<crate::containers::HeldBy>,
        Without<crate::containers::InSlot>,
        Without<crate::containers::Stored>,
    ),
>;

/// Offers a container-tampering action to someone currently authorized for one.
///
/// The replacement for `saboteur`'s scripted errand. What changes is not the
/// outcome but the shape: the old path *dispatched* a body at a beaker the
/// moment a visit expired, and the splash landed on arrival because it had been
/// decided two minutes earlier. This offers a candidate that competes with the
/// actor's ordinary work, can lose, and is re-judged when it is time to act.
#[allow(clippy::type_complexity)]
fn offer_container_tampering(
    time: Res<Time>,
    mut buffer: ResMut<UtilityOpportunityBuffer>,
    cooldowns: Res<CovertCooldowns>,
    nav: Option<Res<crate::nav::NavGraph>>,
    glassware: LooseGlassware,
    sight: CovertSight,
    actors: Query<(Entity, &Transform, &TamperAuthorization), With<UtilityAgent>>,
) {
    let now = time.elapsed_secs();
    let Some(nav) = nav else {
        return;
    };

    for (actor, at, authorization) in &actors {
        // The window is the whole preventability story: it runs down while the
        // actor does ordinary work, so an attempt can simply never find its
        // moment.
        if authorization.expired(now) || !authorization.allotment.is_positive() {
            continue;
        }
        if cooldowns.blocked(actor, UtilityActionId::TamperContainer, now) {
            continue;
        }

        let here = at.translation;
        let boxes = sight.boxes();
        // Only containers with something in them. Splashing into an empty
        // beaker is not sabotage, it is a free ingredient — and the player
        // would never even notice.
        let candidates = glassware.iter().filter_map(|(beaker, container, at)| {
            let point = at.translation;
            (container.solution.total_volume().is_positive()
                && sight.can_see(&boxes, here, point)
                && !sight.observed(actor, here, point))
            .then_some(((beaker, point), point))
        });
        // Nearest by the walk, not the straight line: a beaker a metre away
        // through a wall is correctly the far one. `nearest_reachable` hands
        // back the route distance, so the position rides along in the payload.
        let Some(((beaker, beaker_at), _)) = nav.nearest_reachable(here, candidates) else {
            continue;
        };

        buffer.offer(
            UtilityOpportunity::new(
                actor,
                UtilityActionId::TamperContainer,
                UtilityBucket::Routine,
                stable_text_key(&format!("covert.tamper.{:016x}", beaker.to_bits())),
                Normalized::new(TAMPER_APPEAL).expect("the authored appeal is normalized"),
            )
            .with_target(ActionTarget::Point(beaker_at))
            // Exclusive: two actors must not converge on the same beaker.
            .with_reservation(
                ReservationKey(format!("covert.glassware.{:016x}", beaker.to_bits())),
                1,
            )
            .with_timing(TAMPER_SECONDS, TAMPER_SECONDS + 30.0),
        );
    }
}

/// Applies a completed tampering action: real reagent into the real container.
///
/// Mirrors [`apply_food_poisoning`], including the arrival rechecks. The
/// container is re-resolved from the action's own key rather than a stored
/// handle, and then re-verified: a beaker picked up mid-walk keeps its
/// `Transform` — it follows the hand holding it — so an actor that only checked
/// at the start would trail the chemist around the lab and then meddle with the
/// beaker they are holding. Reaching a target is not permission to touch it.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn apply_container_tampering(
    time: Res<Time>,
    db: Option<Res<crate::chem_data::ChemDb>>,
    mut results: MessageReader<UtilityActionResolved>,
    mut cooldowns: ResMut<CovertCooldowns>,
    mut witnessed: MessageWriter<super::Stimulus>,
    sight: CovertSight,
    actors: Query<&Transform>,
    mut authorizations: Query<&mut TamperAuthorization>,
    mut glassware: Query<
        (Entity, &mut crate::containers::Container, &Transform),
        (
            Without<crate::containers::HeldBy>,
            Without<crate::containers::InSlot>,
            Without<crate::containers::Stored>,
        ),
    >,
) {
    let Some(db) = db else {
        return;
    };
    let now = time.elapsed_secs();

    for result in results.read() {
        if result.key.action != UtilityActionId::TamperContainer {
            continue;
        }
        if result.result != ActionResult::Completed {
            cooldowns.begin(result.agent, UtilityActionId::TamperContainer, now);
            continue;
        }

        // Verify the authorization independently, so a stale or duplicated
        // result message cannot act on a licence that has since expired.
        let Ok(mut authorization) = authorizations.get_mut(result.agent) else {
            continue;
        };
        if authorization.expired(now) || !authorization.allotment.is_positive() {
            continue;
        }

        // Re-resolve the exact container this action set out for. A beaker now
        // held, slotted or stored is simply absent from this query — which is
        // the protection working, not a failure.
        let Some((beaker, beaker_at)) = glassware.iter().find_map(|(beaker, _, at)| {
            (stable_text_key(&format!("covert.tamper.{:016x}", beaker.to_bits()))
                == result.key.target_key)
                .then_some((beaker, at.translation))
        }) else {
            cooldowns.begin(result.agent, UtilityActionId::TamperContainer, now);
            continue;
        };

        let Ok(actor_at) = actors.get(result.agent).map(|at| at.translation) else {
            continue;
        };
        let boxes = sight.boxes();
        if !sight.can_see(&boxes, actor_at, beaker_at)
            || sight.observed(result.agent, actor_at, beaker_at)
        {
            // Somebody arrived, or the beaker went out of view, while they were
            // walking. This is the moment an interruption actually interrupts.
            cooldowns.begin(result.agent, UtilityActionId::TamperContainer, now);
            continue;
        }

        let Ok((_, mut container, _)) = glassware.get_mut(beaker) else {
            continue;
        };
        let amount = authorization.allotment;
        let reagent = authorization.reagent;
        // Through `mutate`, not a raw `solution.add`, so the splash resolves
        // reactions and re-tints the liquid exactly as if the player had poured
        // it in themselves. The tell is real and visible to anyone who looks.
        let ph = db.reagents.get(reagent).ph;
        container.mutate(&db, |solution| {
            let _ = solution.add_profiled(reagent, amount, 1.0, ph);
        });
        // Spent. One authorization, one dose — an interrupted attempt does not
        // refill it, and the window closing discards whatever is left.
        authorization.allotment = chem_sim::Units::ZERO;

        // Ambiguous by construction: `SuspiciousHandling` is the same kind
        // innocent handling produces. Separating the two is an investigation's
        // job. Note this is new — the old arrival path emitted nothing at all,
        // so a witness standing right there learned nothing.
        witnessed.write(
            super::Stimulus::new(super::StimulusKind::SuspiciousHandling, beaker_at)
                .about(beaker)
                .by(result.agent)
                .with_strength(0.8),
        );
    }
}

/// Drops authorizations whose window has closed.
///
/// The unused allotment goes with it. An expired attempt leaves nothing behind
/// to be picked up later, which is what makes "they never found a good moment"
/// a real ending rather than a postponement.
fn expire_tamper_authorizations(
    time: Res<Time>,
    mut commands: Commands,
    authorizations: Query<(Entity, &TamperAuthorization)>,
) {
    let now = time.elapsed_secs();
    for (actor, authorization) in &authorizations {
        if authorization.expired(now) || !authorization.allotment.is_positive() {
            commands.entity(actor).remove::<TamperAuthorization>();
        }
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
/// One person's exposure to one contaminated meal.
///
/// The record that replaced a permanent person-to-culprit association. The old
/// table put diners and meals in the same first-write-wins list with nothing
/// but two entity ids, which meant it could answer "was this person ever fed a
/// tampered meal" but not "is *this* poisoning that meal's doing" — so a diner
/// who ate a bad bowl on Monday and drank something unrelated on Friday had
/// Friday charged to Monday's culprit, forever.
///
/// Carrying the reagent and dose is what makes the link checkable rather than
/// merely asserted, and `bound_to` is what ends it.
#[derive(Clone, Debug)]
struct Exposure {
    diner: Entity,
    /// The act this came from.
    meal: Entity,
    culprit: Entity,
    /// What actually entered them, so a later poisoning by something else
    /// cannot borrow this exposure's provenance.
    reagent: chem_sim::ReagentId,
    dose: chem_sim::Units,
    at: f32,
    /// The incident this exposure was charged to, once one exists.
    ///
    /// `None` means still unbound: the contaminant is in them and no poisoning
    /// has been raised yet. Once bound, the exposure belongs to that incident
    /// and is retained only until it resolves.
    bound_to: Option<super::IncidentId>,
}

/// Meals a covert act contaminated, and who was exposed to them.
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
    exposures: Vec<Exposure>,
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

    /// Records that a real serving of a contaminated meal entered a real body.
    ///
    /// Only called where that actually happens, and only with what actually
    /// moved. Eating from an untampered meal records nothing, which is why a
    /// missing exposure is a meaningful answer rather than an absence of data.
    pub(super) fn carry_to(
        &mut self,
        diner: Entity,
        meal: Entity,
        reagent: chem_sim::ReagentId,
        dose: chem_sim::Units,
        at: f32,
    ) {
        let Some(culprit) = self.culprit(meal) else {
            return;
        };
        if !dose.is_positive() {
            return;
        }
        self.exposures.push(Exposure {
            diner,
            meal,
            culprit,
            reagent,
            dose,
            at,
            bound_to: None,
        });
    }

    /// Claims an unbound exposure for a poisoning incident, if this diner has
    /// one.
    ///
    /// Binding is what stops a single act paying for several incidents: the
    /// exposure is consumed by the first poisoning it explains, and a later
    /// unrelated one finds nothing. Returns the culprit so the caller can
    /// attribute without reaching into the record.
    fn bind(&mut self, diner: Entity, incident: super::IncidentId) -> Option<Entity> {
        // Already charged to this incident — a repeated event must not price it
        // twice.
        if let Some(bound) = self
            .exposures
            .iter()
            .find(|e| e.diner == diner && e.bound_to == Some(incident))
        {
            let _ = bound;
            return None;
        }
        let exposure = self
            .exposures
            .iter_mut()
            .find(|e| e.diner == diner && e.bound_to.is_none())?;
        exposure.bound_to = Some(incident);
        Some(exposure.culprit)
    }

    /// Whether this incident has an exposure explaining it, and what it was.
    ///
    /// Used to keep testimony relevant to the case being investigated rather
    /// than to whatever a witness most recently saw.
    pub(super) fn exposure_for(&self, incident: super::IncidentId) -> Option<(Entity, Entity)> {
        self.exposures
            .iter()
            .find(|e| e.bound_to == Some(incident))
            .map(|e| (e.meal, e.culprit))
    }

    /// How much of the contaminant this act actually put into this person, and
    /// when.
    ///
    /// The physical detail behind an attribution. `price_covert_harm`
    /// deliberately does *not* use it — severity there is the victim's, not the
    /// dose's, because what destabilises a station is how badly someone was
    /// hurt rather than how much was poured. This exists so a debug trace can
    /// say which act explains which casualty in terms a person can check
    /// against the chemistry, which is the difference between an attribution
    /// that is auditable and one that is merely asserted.
    pub(super) fn dose_for(
        &self,
        incident: super::IncidentId,
    ) -> Option<(chem_sim::ReagentId, chem_sim::Units, f32)> {
        self.exposures
            .iter()
            .find(|e| e.bound_to == Some(incident))
            .map(|e| (e.reagent, e.dose, e.at))
    }

    /// Drops exposures that can no longer explain anything.
    ///
    /// Two ways to stop mattering: an unbound exposure whose contaminant has
    /// cleared the body, and a bound one whose incident has been resolved.
    /// Without this an exposure is a permanent accusation waiting for a
    /// coincidence.
    fn forget_spent(
        &mut self,
        now: f32,
        cleared: impl Fn(Entity, chem_sim::ReagentId) -> bool,
        resolved: impl Fn(super::IncidentId) -> bool,
    ) {
        self.exposures.retain(|exposure| match exposure.bound_to {
            Some(incident) => !resolved(incident),
            None => {
                let _ = now;
                !cleared(exposure.diner, exposure.reagent)
            }
        });
    }

    fn clear(&mut self) {
        self.meals.clear();
        self.exposures.clear();
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
    mut tampered: ResMut<TamperedMeals>,
    mut ledger: ResMut<super::IncidentLedger>,
    mut stability: MessageWriter<crate::instability::StabilityEvent>,
    mut campaign: Option<ResMut<crate::arc::Campaign>>,
    mut shift: ResMut<crate::orders::Shift>,
) {
    for incident in created.read() {
        if incident.kind != super::IncidentKind::Poisoning {
            continue;
        }
        // Claim an exposure that can actually explain this poisoning.
        //
        // `bind` consumes it, so one act pays for one incident. A diner who was
        // fed a tampered meal last shift and drinks something unrelated today
        // finds nothing to claim — which used to be the bug: the old lookup was
        // a permanent "this person was once poisoned by someone" flag, and
        // every later poisoning of that body inherited the culprit.
        let Some(culprit) = tampered.bind(incident.subject, incident.id) else {
            // An ordinary poisoning — a Botany accident, a bad batch, or a
            // second helping of harm this act has already paid for. Not this
            // system's business.
            continue;
        };

        // Ground truth, recorded so a later investigation is checked against
        // something. `attribute` never overwrites, so this cannot contradict a
        // department adapter that already knew its own culprit.
        let _ = ledger.attribute(incident.id, culprit);

        // Authority-side evidence for debugging, in terms that can be checked
        // against the chemistry rather than taken on faith. Secret by
        // construction — this is `debug!` on the host, and nothing here reaches
        // a client or a player-facing surface.
        if let Some((reagent, dose, at)) = tampered.dose_for(incident.id) {
            debug!(
                "covert: incident {:?} explained by {:?}u of {:?} fed to {:?} at {:.1}s",
                incident.id, dose, reagent, incident.subject, at,
            );
        }

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

/// Retires exposures that can no longer explain a poisoning.
///
/// The half of attribution that used not to exist. An exposure is evidence that
/// *this* act put *this* contaminant in *this* body; once the body is clear of
/// it, or the incident it was charged to is closed, it explains nothing and
/// keeping it means a coincidence later can borrow it.
fn forget_spent_exposures(
    time: Res<Time>,
    mut tampered: ResMut<TamperedMeals>,
    ledger: Res<super::IncidentLedger>,
    bodies: Query<&crate::body::Bloodstream>,
) {
    let now = time.elapsed_secs();
    tampered.forget_spent(
        now,
        |diner, reagent| {
            // Cleared from both compartments, or the body is gone entirely.
            // The stomach counts: a dose swallowed and not yet absorbed is
            // still very much in them and still explains what happens next.
            bodies.get(diner).is_ok_and(|blood| {
                !blood.0.blood.volume_of(reagent).is_positive()
                    && !blood.0.stomach.volume_of(reagent).is_positive()
            }) || bodies.get(diner).is_err()
        },
        |incident| {
            ledger
                .get(incident)
                .is_none_or(|record| record.status == super::IncidentStatus::Resolved)
        },
    );
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

fn reset_covert_state(
    mut custody: ResMut<IllicitCustody>,
    mut tampered: ResMut<TamperedMeals>,
    mut cooldowns: ResMut<CovertCooldowns>,
) {
    custody.clear();
    tampered.clear();
    cooldowns.clear();
}

pub(super) fn register(app: &mut App) {
    app.init_resource::<IllicitCustody>()
        .init_resource::<TamperedMeals>()
        .init_resource::<CovertCooldowns>()
        .add_systems(
            OnEnter(crate::AppState::Playing),
            reset_covert_state
                .in_set(super::UtilityResetSet)
                .run_if(crate::net::is_authority),
        )
        .add_systems(
            Update,
            (offer_food_poisoning, offer_container_tampering)
                .in_set(super::OpportunityProviders)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            (apply_food_poisoning, apply_container_tampering)
                .after(super::resolve_reference_actions)
                .in_set(super::UtilityAiSet::Resolve)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            // After the resolve set, so an authorization spent this frame is
            // cleaned up this frame rather than lingering a tick.
            expire_tamper_authorizations
                .after(apply_container_tampering)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        )
        .add_systems(
            Update,
            (price_covert_harm, forget_spent_exposures)
                .chain()
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing))
                .run_if(crate::session::career_session),
        );
}

/// The covert selection and execution systems, without the state gates.
///
/// For tests in *other* modules that need the real covert path rather than a
/// stand-in — `saboteur`, whose thread arms an authorization here and whose
/// protections are only meaningful if something real can act on it.
///
/// Registers the same systems `register` does, minus the `AppState`/authority
/// run conditions a headless harness has no way to satisfy. Deliberately not a
/// second implementation: if these systems change, this changes with them.
#[cfg(test)]
pub struct CovertTestHarness;

/// The harness's systems, so a consumer can order itself after them.
///
/// A test that reads what the covert act *emitted* — a stimulus, a radio line —
/// has to run after it, or it sees the previous frame's messages and reports
/// everything one tick late.
#[cfg(test)]
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CovertTestSystems;

#[cfg(test)]
impl Plugin for CovertTestHarness {
    fn build(&self, app: &mut App) {
        app.init_resource::<IllicitCustody>()
            .init_resource::<TamperedMeals>()
            .init_resource::<CovertCooldowns>()
            .init_resource::<UtilityOpportunityBuffer>()
            .add_message::<UtilityActionResolved>()
            .add_systems(
                Update,
                (
                    super::clear_opportunity_buffer,
                    offer_container_tampering,
                    apply_container_tampering,
                    expire_tamper_authorizations,
                )
                    .chain()
                    .in_set(CovertTestSystems),
            );
    }
}

/// Whether the tampering opportunity offered to `actor` names this container.
///
/// A helper rather than an exposed key format: a test that rebuilt the
/// `covert.tamper.{bits}` string itself would keep passing if the real one
/// changed underneath it.
#[cfg(test)]
pub fn tampering_targets(app: &App, actor: Entity, beaker: Entity) -> bool {
    offered_tampering(app, actor).is_some_and(|key| {
        key.target_key == stable_text_key(&format!("covert.tamper.{:016x}", beaker.to_bits()))
    })
}

/// The tampering opportunity currently offered to `actor`, if any.
///
/// Lets a test in another module assert on what was *proposed* — the state
/// between "they would if they could" and "they did", which is exactly the
/// window the player acts in.
#[cfg(test)]
pub fn offered_tampering(app: &App, actor: Entity) -> Option<super::ActionKey> {
    app.world()
        .resource::<UtilityOpportunityBuffer>()
        .for_agent(actor)
        .find(|offer| offer.action == UtilityActionId::TamperContainer)
        .map(|offer| offer.key())
}

/// Drives the offered tampering action to completion, as the selector would.
///
/// Emits the resolution the real action lifecycle would emit on a finished
/// performance, so `apply_container_tampering` runs its own arrival rechecks
/// against whatever the test has arranged. It deliberately does **not** perform
/// the act itself: a fixture that injected a successful mutation in place of the
/// executor would prove nothing about the executor.
///
/// Returns `false` when nothing was offered, which is itself a meaningful
/// outcome — a vetoed or unreachable opportunity never becomes an action.
#[cfg(test)]
pub fn complete_offered_tampering(app: &mut App, actor: Entity) -> bool {
    let Some(key) = offered_tampering(app, actor) else {
        return false;
    };
    app.world_mut().write_message(UtilityActionResolved {
        agent: actor,
        key,
        claim: super::ReservationOwner {
            agent: actor,
            action_instance: 1,
        },
        result: ActionResult::Completed,
    });
    app.update();
    true
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

    /// Feeds `victim` a real serving of a meal `culprit` tampered with.
    ///
    /// Goes through the same two steps the game does — record the act, then
    /// record the exposure — because they are genuinely different facts now.
    /// A test that wrote the exposure directly could not tell the difference
    /// between "was fed a bad meal" and "was once near one".
    fn expose(app: &mut App, victim: Entity, culprit: Entity) {
        let data = chem();
        let reagent = contaminant(&data);
        let meal = app.world_mut().spawn_empty().id();
        let mut tampered = app.world_mut().resource_mut::<TamperedMeals>();
        tampered.record(meal, culprit);
        tampered.carry_to(
            victim,
            meal,
            reagent,
            chem_sim::Units::from_f64(EFFECTIVE_DOSE_ML),
            0.0,
        );
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
        expose(&mut app, victim, culprit);

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

    /// Finding 5, the bug this record replaced.
    ///
    /// The old table stored diners and meals together as bare entity pairs, so
    /// "was this person ever fed a tampered meal" was the only question it
    /// could answer — and it answered `true` forever. Someone poisoned by a
    /// bad batch weeks later had it charged to the earlier culprit, who may by
    /// then have been arrested, cleared, or standing on the other side of the
    /// station.
    #[test]
    fn a_later_unrelated_poisoning_is_not_charged_to_the_earlier_meal() {
        let mut app = pricing_app();
        let culprit = Entity::from_raw_u32(5).unwrap();
        let victim = Entity::from_raw_u32(7).unwrap();
        expose(&mut app, victim, culprit);

        // The poisoning that exposure explains. Pays once.
        app.world_mut().write_message(poisoning(victim));
        app.update();
        let plot_after_first = app.world().resource::<crate::arc::Campaign>().plot;
        assert!(plot_after_first > 0, "the real one is priced");

        // A second, unrelated poisoning of the same body — a bad batch, a
        // Botany accident. The earlier act has already been paid for and
        // cannot explain this one.
        let mut later = poisoning(victim);
        later.id = super::super::IncidentId(2);
        app.world_mut().write_message(later);
        app.update();

        assert_eq!(
            app.world().resource::<crate::arc::Campaign>().plot,
            plot_after_first,
            "an old meal cannot be blamed for whatever happens next",
        );
    }

    /// One incident, one charge. Reprocessing an event — a duplicate message, a
    /// system that reads the queue twice — must not move the meters again.
    #[test]
    fn the_same_incident_is_never_priced_twice() {
        let mut app = pricing_app();
        let culprit = Entity::from_raw_u32(5).unwrap();
        let victim = Entity::from_raw_u32(7).unwrap();
        expose(&mut app, victim, culprit);

        app.world_mut().write_message(poisoning(victim));
        app.update();
        let once = app.world().resource::<crate::arc::Campaign>().plot;

        // The exact same incident again.
        app.world_mut().write_message(poisoning(victim));
        app.update();

        assert_eq!(
            app.world().resource::<crate::arc::Campaign>().plot,
            once,
            "one incident is one consequence, however many times it is seen",
        );
    }

    /// Real harm reaches all three meters, and names the culprit as ground truth.
    #[test]
    fn a_covert_poisoning_moves_stability_campaign_and_department_standing() {
        let mut app = pricing_app();
        let culprit = Entity::from_raw_u32(5).unwrap();
        let victim = Entity::from_raw_u32(7).unwrap();
        expose(&mut app, victim, culprit);
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

        // One antagonist per run. The witness veto is real now, so four of them
        // standing in one room would each see the other three and all correctly
        // refuse — for reasons that have nothing to do with custody. Isolating
        // the cases keeps this test about what it says it is about.
        let offer_for = |goal: CovertGoal, stocked: bool| -> Option<(UtilityActionId, UtilityBucket)> {
            let mut app = App::new();
            app.init_resource::<Time>()
                .init_resource::<UtilityOpportunityBuffer>()
                .init_resource::<IllicitCustody>()
                .init_resource::<CovertCooldowns>()
                .insert_resource(CovertContaminant { reagent })
                // Real room geometry, because selection now depends on what an
                // actor can see. Without it nothing is visible and every
                // candidate is refused on sight rather than on supply.
                .insert_resource(crate::lab::WalkableAreas::from_floor_plan())
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

            let actor = app
                .world_mut()
                .spawn((
                    CrewMember {
                        name: "Grower".into(),
                        role: "Botany".into(),
                    },
                    Transform::default(),
                    UtilityControlBundle::new(UtilityAgent::new(1, 0)),
                    goal,
                ))
                .id();
            if stocked {
                app.world_mut()
                    .resource_mut::<IllicitCustody>()
                    .receive_from_player(actor, stock(reagent, EFFECTIVE_DOSE_ML * 4.0));
            }
            app.update();
            let found = app
                .world()
                .resource::<UtilityOpportunityBuffer>()
                .for_agent(actor)
                .next()
                .map(|offer| (offer.action, offer.bucket));
            found
        };

        let discredit = CovertGoal::new(PrivateGoal::DiscreditDepartment, 0.9);
        let (action, bucket) = offer_for(discredit, true).expect("a stocked antagonist can act");
        assert_eq!(action, UtilityActionId::PoisonFood);
        // The act must never outrank a real emergency.
        assert_eq!(bucket, UtilityBucket::Routine);

        assert!(
            offer_for(discredit, false).is_none(),
            "an identical antagonist with no player-supplied stock has no covert option at all"
        );
        assert!(
            offer_for(CovertGoal::new(PrivateGoal::Grudge, 0.9), true).is_none(),
            "stock is not enough: poisoning a shared meal cannot serve a grudge"
        );
        assert!(
            offer_for(
                CovertGoal {
                    achieved: true,
                    ..discredit
                },
                true
            )
            .is_none(),
            "an antagonist whose goal is met returns to ordinary life"
        );
    }

    /// Builds one covert selection scenario and reports whether the act was
    /// offered.
    ///
    /// `actor_at`, `meal_at` and `observers` are the three things the knowledge
    /// and witness gates actually read, so every case below is one call with
    /// different geometry.
    fn offered_with(
        actor_at: Vec3,
        meal_at: Vec3,
        observers: &[Vec3],
        walls: &[(Vec3, Vec3)],
    ) -> bool {
        let data = chem();
        let reagent = contaminant(&data);

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<UtilityOpportunityBuffer>()
            .init_resource::<IllicitCustody>()
            .init_resource::<CovertCooldowns>()
            .insert_resource(CovertContaminant { reagent })
            .insert_resource(crate::lab::WalkableAreas::from_floor_plan())
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
            Transform::from_translation(meal_at),
        ));

        for (center, half_extents) in walls {
            app.world_mut().spawn((
                Transform::from_translation(*center),
                crate::lab::Solid {
                    half_extents: *half_extents,
                },
            ));
        }
        for at in observers {
            app.world_mut().spawn((
                CrewMember {
                    name: "Bystander".into(),
                    role: "Service".into(),
                },
                Transform::from_translation(*at),
            ));
        }

        let actor = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Grower".into(),
                    role: "Botany".into(),
                },
                Transform::from_translation(actor_at),
                UtilityControlBundle::new(UtilityAgent::new(1, 0)),
                CovertGoal::new(PrivateGoal::DiscreditDepartment, 0.9),
            ))
            .id();
        app.world_mut()
            .resource_mut::<IllicitCustody>()
            .receive_from_player(actor, stock(reagent, EFFECTIVE_DOSE_ML * 4.0));
        app.update();
        let found = app
            .world()
            .resource::<UtilityOpportunityBuffer>()
            .for_agent(actor)
            .next()
            .is_some();
        found
    }

    /// A wall between the two, spanning the line of sight.
    fn wall_between(a: Vec3, b: Vec3) -> (Vec3, Vec3) {
        ((a + b) * 0.5, Vec3::new(0.25, 2.0, 4.0))
    }

    /// Finding 2, the positive control and its negative twin.
    ///
    /// Selection used to scan every served meal within 24 metres regardless of
    /// walls, rooms or knowledge. An identical meal behind a wall was chosen
    /// exactly as readily as one in plain view, which meant no amount of
    /// closing doors or watching rooms could change what an antagonist knew.
    #[test]
    fn a_meal_in_view_is_a_candidate_and_the_same_meal_behind_a_wall_is_not() {
        let actor = Vec3::ZERO;
        let meal = Vec3::new(2.0, 0.0, 0.0);
        assert!(
            offered_with(actor, meal, &[], &[]),
            "a meal in plain sight is a candidate"
        );
        assert!(
            !offered_with(actor, meal, &[], &[wall_between(actor, meal)]),
            "the identical meal behind a wall was never seen"
        );
    }

    /// The witness veto, which `nerve`'s doc has always described and which no
    /// code implemented. Selection read no observers at all, so an antagonist
    /// would reach for a bowl with someone standing over it.
    #[test]
    fn a_visible_observer_calls_the_act_off() {
        let actor = Vec3::ZERO;
        let meal = Vec3::new(2.0, 0.0, 0.0);
        assert!(
            offered_with(actor, meal, &[], &[]),
            "nobody watching: the act is available"
        );
        assert!(
            !offered_with(actor, meal, &[Vec3::new(2.5, 0.0, 1.0)], &[]),
            "someone standing at the counter is enough to call it off"
        );
    }

    /// The asymmetry that makes getting caught possible.
    ///
    /// An observer the actor cannot perceive does not enter its risk estimate —
    /// it is not omniscient about who is nearby. That same person can still
    /// witness the act through ordinary sensing, which is what being caught
    /// looks like from the other side.
    #[test]
    fn an_observer_the_actor_cannot_see_does_not_veto_the_act() {
        let actor = Vec3::ZERO;
        let meal = Vec3::new(2.0, 0.0, 0.0);
        let hidden_watcher = Vec3::new(-3.0, 0.0, 0.0);
        assert!(
            offered_with(
                actor,
                meal,
                &[hidden_watcher],
                &[wall_between(actor, hidden_watcher)],
            ),
            "an actor cannot hide from someone it has no way of knowing about"
        );
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
            .init_resource::<CovertCooldowns>()
            .init_resource::<TamperedMeals>()
            .insert_resource(CovertContaminant { reagent })
            // The act now re-verifies its target on arrival, so the completion
            // path needs the same geometry selection does. Without it the
            // actor cannot see the bowl it is standing over and correctly
            // declines to touch it.
            .insert_resource(crate::lab::WalkableAreas::from_floor_plan())
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

    /// Drives a completed action against a scene the caller arranges, and
    /// reports whether the contaminant actually landed.
    ///
    /// The point of the recheck is that selection and commitment are separated
    /// by a walk, so these cases arrange the world as it is *on arrival*.
    fn act_lands(observers: &[Vec3], walls: &[(Vec3, Vec3)], meal_capacity: Option<f64>) -> bool {
        let data = chem();
        let reagent = contaminant(&data);

        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<IllicitCustody>()
            .init_resource::<CovertCooldowns>()
            .init_resource::<TamperedMeals>()
            .insert_resource(CovertContaminant { reagent })
            .insert_resource(crate::lab::WalkableAreas::from_floor_plan())
            .add_message::<UtilityActionResolved>()
            .add_message::<super::super::Stimulus>()
            .add_systems(Update, apply_food_poisoning);

        let mut solution = match meal_capacity {
            // A bowl already at its volume cap. `spend` hands the overflow back
            // to custody, so only a token amount can land.
            Some(capacity) => {
                let mut full = chem_sim::Solution::new(chem_sim::Units::from_f64(capacity));
                let filler = data.reagents.id_of("water").expect("water exists");
                let _ = full.add(filler, chem_sim::Units::from_f64(capacity - 0.5));
                full
            }
            None => chem_sim::Solution::unbounded(),
        };
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

        for (center, half_extents) in walls {
            app.world_mut().spawn((
                Transform::from_translation(*center),
                crate::lab::Solid {
                    half_extents: *half_extents,
                },
            ));
        }
        for at in observers {
            app.world_mut().spawn((
                CrewMember {
                    name: "Bystander".into(),
                    role: "Service".into(),
                },
                Transform::from_translation(*at),
            ));
        }

        let actor = app
            .world_mut()
            .spawn((
                CrewMember {
                    name: "Grower".into(),
                    role: "Botany".into(),
                },
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
        // A real dose, not a trace. A few drops squeezed into a full bowl are
        // not a poisoning, so "landed" has to mean the same thing the code
        // means by it.
        let landed = after - before >= chem_sim::Units::from_f64(EFFECTIVE_DOSE_ML);
        // Whatever happened, the motive is only satisfied by an act that
        // actually happened.
        assert_eq!(
            app.world().get::<CovertGoal>(actor).unwrap().achieved,
            landed,
            "a prevented attempt must not satisfy the motive"
        );
        landed
    }

    /// Selection and commitment are separated by a walk, and the world moves in
    /// between. Someone arriving during that walk is the moment a player
    /// actually interrupts an act — before this, arrival was unconditional and
    /// the only way to stop one was to remove the meal.
    #[test]
    fn an_observer_who_arrives_during_the_walk_stops_the_act() {
        assert!(
            act_lands(&[], &[], None),
            "an unobserved arrival goes ahead"
        );
        assert!(
            !act_lands(&[Vec3::new(1.5, 0.0, 1.0)], &[], None),
            "somebody standing over the bowl on arrival stops it"
        );
    }

    /// A target that moved out of sight during the walk cannot be acted on
    /// from memory. Reaching a place is not the same as still having the thing
    /// you came for.
    #[test]
    fn a_target_that_went_out_of_sight_during_the_walk_is_not_acted_on() {
        let wall = wall_between(Vec3::ZERO, Vec3::new(1.0, 0.0, 0.0));
        assert!(
            !act_lands(&[], &[wall], None),
            "the actor cannot see the bowl it came for"
        );
    }

    /// Interrupting an act has to be worth more than the second it takes the
    /// witness to walk on. Without a cooldown the actor re-offers immediately
    /// and reads as mechanically persistent rather than cautious.
    #[test]
    fn a_prevented_attempt_buys_real_time_before_the_next_one() {
        let mut cooldowns = CovertCooldowns::default();
        let actor = Entity::from_raw_u32(7).unwrap();

        assert!(!cooldowns.blocked(actor, UtilityActionId::PoisonFood, 0.0));
        cooldowns.begin(actor, UtilityActionId::PoisonFood, 0.0);

        assert!(
            cooldowns.blocked(actor, UtilityActionId::PoisonFood, COVERT_RETRY_COOLDOWN - 0.1),
            "the whole point is that they wait"
        );
        assert!(
            !cooldowns.blocked(actor, UtilityActionId::PoisonFood, COVERT_RETRY_COOLDOWN + 0.1),
            "and that they eventually stop waiting"
        );
        // Being scared off one thing does not postpone an unrelated one.
        assert!(!cooldowns.blocked(actor, UtilityActionId::Sabotage, 1.0));
        // Nor does it postpone anyone else.
        assert!(!cooldowns.blocked(
            Entity::from_raw_u32(8).unwrap(),
            UtilityActionId::PoisonFood,
            1.0
        ));
    }

    /// `spend` returns refused overflow to custody, so topping up a full bowl
    /// lands a few drops. That is not a poisoning, and it used to satisfy the
    /// motive permanently anyway — the antagonist retired on a technicality.
    #[test]
    fn a_bowl_too_full_to_take_a_real_dose_is_not_a_poisoning() {
        assert!(
            !act_lands(&[], &[], Some(EFFECTIVE_DOSE_ML)),
            "a token overflow is not an effective dose"
        );
    }
}
