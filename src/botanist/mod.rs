//! Botany's minor antagonist: a grower who thinks his work is being wasted.
//!
//! The last of the six department minors, and the one that was reserved longest
//! — because a Botany antagonist is the only thread that puts an illicit
//! chemical into an NPC's hands, and that has hard rules.
//!
//! ## The rule this module obeys
//!
//! Aleksy's final ask names `quiet_rot`, which is tagged `player_only`. Nothing
//! anywhere may spawn that reagent for NPC use — not this module, not a load
//! hook, not a campaign step. The *only* route by which he ever holds any is
//! [`handle_botanist_resolution`] observing a real, successful, player-delivered
//! order and moving actual solution volume into
//! [`crate::utility_ai::IllicitCustody`].
//!
//! That makes refusal a real choice rather than a delay:
//!
//! - Refuse, and the worst branch is closed outright. Aleksy sulks; Botany's
//!   standing slips because produce quietly stops arriving. Bad, recoverable,
//!   and completely different in kind from what compliance opens.
//! - Comply, and he holds a physical batch with recorded provenance. He is not
//!   compelled to use it, and the player has time to notice, recover it, warn
//!   someone, or change the situation first.
//!
//! ## What this module does *not* decide
//!
//! It does not decide that a meal gets poisoned. It installs a private goal and
//! hands over physical stock; `utility_ai::covert` scores the act against
//! Aleksy's ordinary work like any other candidate, and he loses to a casualty
//! or a busy kitchen the same as anyone. A thread that forced the outcome would
//! be a cutscene with extra steps.

use bevy::prelude::*;
use serde::Deserialize;

use crate::antagonist::UnderworldStanding;
use crate::crew::CrewMember;
use crate::chem_data::ChemDb;
use crate::orders::{OrderResolved, Outcome, RetainsDelivery, Shift};
use crate::threat;
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::utility_ai::{
    CovertGoal, CustodyState, DealResponse, IllicitCustody, IllicitRequest, IllicitStock,
    PlayerBenefit, PrivateGoal,
};
use crate::AppState;

// A short delivery is not a handover, but this module used to decide that by
// reading `DeliveryQuality::remaining_fraction` against a 0.5 floor. That field
// is computed from the requester's remaining *patience*, not from the volume in
// the container: a complete, correct batch handed over late scored below the
// floor and was recorded as a refusal, and the player who made exactly what was
// asked for watched it count against them.
//
// Order grading already answers the question this was reaching for. `Outcome::
// Short` is the graded verdict on an underfilled container, and it lands in the
// refusal branch with every other unsuccessful outcome. Nothing here needs to
// re-derive it.

#[derive(Clone, Debug, Deserialize)]
pub struct BotanistScript {
    /// Kept off `station.crew.ron`, like the other minors, so an ordinary order
    /// can never double-book them.
    pub name: String,
    pub role: String,
    /// Uniform tint and visit cadence, read by the spawner when this thread
    /// gains one. Parsed now so the authored data is validated from the start.
    #[allow(dead_code)]
    pub color: [f32; 3],
    #[allow(dead_code)]
    pub gap_multiplier: (f32, f32),
    pub visits: Vec<BotanistVisitDef>,
    /// Standing cost to Botany each time an ask goes unanswered.
    pub withheld_penalty: i32,
    pub withheld_lines: Vec<String>,
    /// Aired on the handover that completes the thread's final ask.
    pub supplied_line: String,
    pub goal: PrivateGoal,
    pub nerve: f32,
    /// What the player gets for the final handover. Required: see
    /// [`BotanistScript::request`] and the upside rule in `utility_ai::deals`.
    pub benefits: Vec<PlayerBenefit>,
    /// Which optional responses Aleksy offers beyond the four baseline ones.
    #[serde(default)]
    pub options: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BotanistVisitDef {
    pub reagent: String,
    pub amount: u32,
    pub plea: String,
}

impl BotanistScript {
    /// The ask that turns the thread. Always the last one authored.
    fn final_ask(&self) -> Option<&BotanistVisitDef> {
        self.visits.last()
    }

    /// The final ask as a deal the player can answer.
    ///
    /// Aleksy accepts no substitute for the last ask — he knows what he wants,
    /// and a player offering something similar has guessed what it is for.
    pub fn request(&self) -> Option<IllicitRequest> {
        let ask = self.final_ask()?;
        Some(IllicitRequest {
            requester: self.name.clone(),
            reagent: ask.reagent.clone(),
            amount: ask.amount,
            benefits: self.benefits.clone(),
            options: self.options.clone(),
            accepts_substitute: None,
        })
    }
}

#[derive(Resource, Deref)]
pub struct Script(pub BotanistScript);

/// How far through the asks this save has got.
#[derive(Resource, Default, Debug)]
pub struct BotanistProgress {
    pub refusals: u32,
    /// Set once a player has actually handed over the final ask. Distinct from
    /// "the goal is achieved" — that lives on the agent, because the decision
    /// to act belongs to the utility scorer, not to this thread.
    pub supplied: bool,
    /// Which authored ask comes next. Advances once per resolved visit,
    /// however it graded, so the thread walks its list whether the player is
    /// helping or stonewalling.
    pub step: usize,
    /// The final ask was put and not met. Terminal, and deliberately distinct
    /// from `supplied`: both end the thread, but one ends it with a batch in
    /// Aleksy's hands and the other ends it with nothing. Without the
    /// distinction a refused thread would re-arm and ask again, which reads as
    /// the refusal never having registered.
    pub closed: bool,
}

#[derive(Resource)]
struct BotanistSpawner {
    timer: Timer,
}

/// See `threat::arm_first_visit` for why this has to re-run on
/// `OnEnter(AppState::Playing)` every session rather than only once at
/// process start.
fn arm_spawner(mut commands: Commands) {
    threat::arm_first_visit(&mut commands, threat::MINOR_FIRST_VISIT, |timer| {
        BotanistSpawner { timer }
    });
}

/// Puts Aleksy's next authored ask in front of the player.
///
/// Until now this thread had no spawner at all — its `color` and
/// `gap_multiplier` were parsed and then ignored, and the only way its final ask
/// reached anyone was the standing conversation `track_custody_identity` opens
/// whenever he happens to be on the floor. That made the escalation invisible:
/// the three ordinary horticulture requests that are supposed to establish him
/// as a working colleague never arrived, so the player met the turn without
/// having met the man.
///
/// The shape is the shared one every other minor thread uses, and the ordering
/// inside it is load-bearing: admission is acquired *before* dispatch, and
/// released again if dispatch fails, so a resident who is present but busy
/// delays the visit instead of being cloned.
#[allow(clippy::too_many_arguments)]
fn generate_botanist_visit(
    mut commands: Commands,
    time: Res<Time>,
    db: Res<ChemDb>,
    station: Option<Res<crate::orders::StationData>>,
    script: Option<Res<Script>>,
    mut spawner: Option<ResMut<BotanistSpawner>>,
    progress: Res<BotanistProgress>,
    shift: Res<Shift>,
    chemists: Query<(), With<crate::player::Chemist>>,
    mut intake: crate::order_intake::Intake,
    mut residents: crate::crew::AvailableResidents,
) {
    let (Some(station), Some(script), Some(spawner)) = (station, script, spawner.as_mut()) else {
        return;
    };
    // Both terminal states stop the asks. `supplied` means he got what he
    // wanted; `closed` means he asked and was turned down. Neither re-arms.
    if progress.supplied || progress.closed {
        return;
    }
    let mut rng = rand::rng();
    let rules = crate::shift::current_rules(&station.config, &shift, chemists.iter().count());
    let Some(visit) = threat::due_visit(
        &time,
        &shift,
        &mut spawner.timer,
        &rules,
        &mut rng,
        script.gap_multiplier,
        threat::ChainProgress(progress.step),
        &script.visits,
    ) else {
        return;
    };
    let Some(reagent) = db.reagents.id_of(&visit.reagent) else {
        warn!("botanist visit names unknown reagent '{}'", visit.reagent);
        return;
    };

    let Some(context) = intake.admit(
        crate::order_intake::RequestSource::Botanist,
        &script.name,
        &mut spawner.timer,
        false,
    ) else {
        return;
    };
    let Some(visitor) = threat::dispatch_scripted_visit(
        &mut commands,
        &db,
        &mut rng,
        &rules,
        &mut residents,
        threat::ScriptedVisit {
            context,
            name: &script.name,
            role: &script.role,
            color: script.color,
            reagent,
            amount_units: visit.amount,
            plea: visit.plea.clone(),
        },
    ) else {
        intake.cancel_admission(&script.name);
        return;
    };

    // Only the last ask is kept. The earlier three are ordinary horticulture
    // and are used the way any other delivered chemical is; this one is the
    // point of the whole thread, and it has to survive the counter intact.
    if progress.step + 1 >= script.visits.len() {
        commands.entity(visitor).insert(RetainsDelivery {
            request: script.name.clone(),
        });
    }
}

pub struct BotanistPlugin;

impl Plugin for BotanistPlugin {
    fn build(&self, app: &mut App) {
        let script: BotanistScript =
            ron::from_str(include_str!("../../assets/data/station.botanist.ron"))
                .expect("station.botanist.ron is valid");
        // The upside rule, enforced where it cannot be skipped: a thread whose
        // final ask promises the player nothing concrete fails to start the
        // game rather than shipping as a trap.
        script
            .request()
            .expect("the botanist thread authors a final ask")
            .validate()
            .expect("the botanist's final ask must promise a concrete player benefit");
        app.insert_resource(Script(script))
            .init_resource::<BotanistProgress>()
            .add_systems(OnEnter(AppState::Playing), (reset_botanist_thread, arm_spawner))
            .add_systems(
                Update,
                (
                    generate_botanist_visit,
                    track_custody_identity,
                    restore_covert_goal,
                    handle_botanist_resolution,
                )
                    .chain()
                    .run_if(crate::net::is_authority)
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

fn reset_botanist_thread(mut progress: ResMut<BotanistProgress>) {
    *progress = BotanistProgress::default();
}

/// Keeps the entity-to-name map current for anyone who can hold a batch.
///
/// Custody is saved against a name, so the mapping has to exist both when a
/// handover happens *and* after a reload, when the save names a holder whose
/// entity is brand new. Rebuilding from the live roster covers both, and covers
/// the ordinary case of an NPC being despawned and respawned mid-career.
///
/// Only characters this thread can hand something to are tracked, so the table
/// stays small rather than mirroring the whole station.
fn track_custody_identity(
    script: Option<Res<Script>>,
    progress: Res<BotanistProgress>,
    mut names: ResMut<crate::shift::CustodyNames>,
    mut approaches: ResMut<crate::utility_ai::LiveApproaches>,
    crew: Query<(Entity, &CrewMember)>,
) {
    let Some(script) = script else {
        return;
    };
    let mut present = false;
    for (entity, member) in &crew {
        if member.name == script.name {
            names.0.retain(|_, held| held != &script.name);
            names.0.insert(entity, script.name.clone());
            present = true;
        }
    }

    // The approach exists while Aleksy is on the floor and the final ask is
    // still unanswered. `open` is idempotent, so this does not reset a stance
    // the player already took — see
    // `asking_again_continues_the_same_approach_rather_than_opening_a_second`.
    if present && !progress.supplied {
        if let Some(request) = script.request() {
            approaches.open(request);
        }
    }
}

/// Puts the motive back on a reloaded body that still holds its batch.
///
/// The goal was never saved, and it could not usefully have been: it lives on a
/// crew entity, and crew entities do not survive a reload. What *is* saved is
/// the physical fact underneath it — `supplied`, plus the batch itself in
/// custody — so the motive is reconstructed from that rather than persisted
/// separately and risked drifting out of step with the material.
///
/// Without this, reloading a supplied save produced an Aleksy carrying illicit
/// stock he could never use: `offer_food_poisoning` requires a `CovertGoal` in
/// its query, and `supplied: true` blocks the only path that inserted one. The
/// threat quietly evaporated, and nothing said so.
///
/// Runs every frame rather than once on load because the body may not exist
/// yet at the moment the save is read — he could be off the floor entirely, and
/// walk back on ten minutes later. The `Without<CovertGoal>` filter makes the
/// repeat harmless: once the motive is on, this stops seeing him.
fn restore_covert_goal(
    mut commands: Commands,
    script: Option<Res<Script>>,
    progress: Res<BotanistProgress>,
    custody: Res<IllicitCustody>,
    growers: Query<(Entity, &CrewMember), Without<CovertGoal>>,
) {
    let Some(script) = script else {
        return;
    };
    if !progress.supplied {
        return;
    }
    for (entity, member) in &growers {
        if member.name != script.name {
            continue;
        }
        // The material is the authority. A batch that was confiscated,
        // destroyed, returned or spent leaves no spendable stock behind, and a
        // motive without means is not a threat — restoring one would resurrect
        // a danger the player had already dealt with.
        if custody
            .held_by(entity)
            .any(|stock| stock.state.is_spendable())
        {
            commands
                .entity(entity)
                .insert(CovertGoal::new(script.goal, script.nerve));
        }
    }
}

/// Reacts to a resolved order that belongs to this thread.
///
/// Two outcomes, deliberately different in kind rather than in degree:
///
/// - A refused or failed ask costs Botany a notch of standing and airs a line.
///   That is the whole consequence; nothing is stockpiled for later.
/// - A **successful final** delivery is the one place in the codebase that puts
///   a `player_only` reagent into NPC hands, and it does so by moving real
///   volume with recorded provenance.
#[allow(clippy::too_many_arguments)]
fn handle_botanist_resolution(
    db: Res<crate::chem_data::ChemDb>,
    time: Res<Time>,
    script: Option<Res<Script>>,
    mut resolved: MessageReader<OrderResolved>,
    mut receipts: MessageReader<crate::orders::DeliveryReceipt>,
    mut progress: ResMut<BotanistProgress>,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<RadioLog>,
    mut custody: ResMut<IllicitCustody>,
    mut names: ResMut<crate::shift::CustodyNames>,
    mut approaches: ResMut<crate::utility_ai::LiveApproaches>,
    mut underworld: ResMut<UnderworldStanding>,
    mut commands: Commands,
    growers: Query<(Entity, &CrewMember)>,
) {
    let Some(script) = script else {
        resolved.clear();
        return;
    };
    let Some(final_ask) = script.final_ask() else {
        resolved.clear();
        return;
    };

    // Drained once, before the resolutions that reference them. Both messages
    // are written in the same frame by `complete_delivery`, and a reader is a
    // cursor rather than a queue, so reading receipts inside the loop would
    // consume them on the first iteration and find nothing on the second.
    let handovers: Vec<crate::orders::DeliveryReceipt> = receipts.read().cloned().collect();

    for order in resolved.read() {
        if order.name != script.name {
            continue;
        }
        // Both terminal states are terminal *here* too, not only in the
        // spawner. The spawner stops asking at `supplied || closed`
        // (see `generate_botanist_visit`), but this handler used to keep
        // grading anything that arrived under his name afterwards — so a
        // resolution landing after the branch closed charged another refusal,
        // cost Botany standing again, and aired another withheld line for an
        // ask that had already been answered. `OrderResolved` has several
        // emitters (expiry, window delivery, counter handover) in different
        // systems, so "the last one already arrived" is not something this
        // reader can assume.
        //
        // `supplied` is handled below rather than here: that path still has a
        // receipt to reconcile.
        // Both terminal states are terminal *here* too, not only in the
        // spawner. The spawner stops asking at `supplied || closed`
        // (see `generate_botanist_visit`), but this handler used to keep
        // grading anything that arrived under his name afterwards — so a
        // resolution landing after the branch closed charged another refusal,
        // cost Botany standing again, and aired another withheld line for an
        // ask that had already been answered. `OrderResolved` has several
        // emitters (expiry, window delivery, counter handover) in different
        // systems, so "the last one already arrived" is not something this
        // reader can assume.
        //
        // `supplied` is handled below rather than here: that path still has a
        // receipt to reconcile.
        if progress.closed {
            continue;
        }

        let delivered_final = order
            .reagent
            .zip(db.reagents.id_of(&final_ask.reagent))
            .is_some_and(|(got, want)| got == want);
        let succeeded = matches!(order.outcome, Outcome::Success);
        // Which ask this resolution answers. Every resolution advances the
        // cursor, so a thread that is ignored still moves through its list.
        let was_final = progress.step + 1 >= script.visits.len();
        progress.step = (progress.step + 1).min(script.visits.len());

        if succeeded && !was_final {
            // An earlier ask, met. Aleksy got his soil chemistry and Botany is
            // no worse off, so this is not a refusal and must not be counted as
            // one — the whole first half of this thread is a player being
            // helpful, and charging it as a withholding made cooperation
            // indistinguishable from stonewalling.
            continue;
        }

        if !succeeded || !delivered_final {
            // Everything that is not the completed final handover lands here,
            // including a short or wrong delivery of the final ask. Refusal and
            // near-miss produce the same, milder consequence on purpose: only
            // a real batch of the real thing changes what Aleksy can do.
            progress.refusals = progress.refusals.saturating_add(1);
            shift.adjust(crate::orders::Department::Botany, script.withheld_penalty);
            if let Some(line) = script
                .withheld_lines
                .get(progress.refusals as usize % script.withheld_lines.len().max(1))
            {
                radio.push(RadioEntry::new(channel_for(&script.role), line.clone()).negative());
            }
            if was_final {
                // The last ask went unanswered. That branch is closed: no stock,
                // and no second chance to ask for it.
                progress.closed = true;
            }
            continue;
        }

        if progress.supplied {
            continue;
        }

        let Some(reagent) = db.reagents.id_of(&final_ask.reagent) else {
            continue;
        };
        // The invariant, enforced at the one site that could break it: this
        // path may only ever move a reagent the authored data marks player-only,
        // and it only runs because a player delivered one.
        debug_assert!(
            db.reagents.get(reagent).player_only,
            "the Botany thread's final ask must be a player-only reagent",
        );

        // Which deal this delivery counts as.
        //
        // Whatever the player told Aleksy, if anything. Delivering without ever
        // answering is cooperation — the container in his hands is the answer,
        // and a player who hands over the real thing while having said nothing
        // has still made the trade. A *supplying* stance is honoured as given;
        // a non-supplying one (they said refuse, then delivered anyway) is read
        // as a change of mind rather than trapping the goods in limbo.
        let answered = approaches
            .get(&script.name)
            .and_then(|live| live.answered.clone())
            .filter(|response| response.supplies_stock())
            .unwrap_or(DealResponse::Cooperate);

        // Pay the player first, and unconditionally.
        //
        // Deliberately *before* the embodiment check and before any custody or
        // goal write: the promised upside is the player's half of a completed
        // trade, so it must not be contingent on Aleksy being spawned, on him
        // keeping the batch, or on him ever finding a covert action worth
        // taking. Ordering it after any of those would quietly turn the deal
        // into the delayed-punishment-only shape the upside rule forbids.
        if let Some(request) = script.request() {
            crate::utility_ai::grant_deal(&request, &answered, &mut shift, &mut underworld);
        }
        // Settle before the embodiment check, for the same reason the payment
        // is: the deal is done whether or not Aleksy is on the floor, and a
        // second delivery must not collect the reward again.
        approaches.settle(&script.name);

        let Some((agent, _)) = growers
            .iter()
            .find(|(_, member)| member.name == script.name)
        else {
            // Aleksy is not embodied right now. The delivery still happened, so
            // the thread must not silently re-arm and take a second batch later.
            progress.supplied = true;
            continue;
        };

        // What he actually holds is what somebody actually carried to the
        // window: the real mixture, at its real purity, in its real volume.
        //
        // This used to build a fresh unbounded solution containing exactly the
        // authored amount of pure reagent — which meant a player who delivered
        // a half-strength batch cut with something else stocked him with
        // laboratory-grade material anyway, and the evidence later recovered
        // from him matched nothing anyone had made. A receipt is the physical
        // record of the handover; without one there is no batch to hold.
        let Some(receipt) = handovers
            .iter()
            .find(|receipt| receipt.recipient == script.name)
        else {
            // Graded a success with no retained delivery behind it. Nothing
            // physical changed hands, so nothing enters custody.
            progress.supplied = true;
            continue;
        };
        // Record who this entity is *before* handing anything over, so the
        // batch can be saved against a name. Crew entities do not survive
        // walking offscreen, let alone a reload.
        names.0.insert(agent, script.name.clone());
        custody.receive_from_player(
            agent,
            IllicitStock {
                solution: receipt.accepted.clone(),
                state: CustodyState::Carried,
                source_player: receipt.supplier,
                received_at: time.elapsed_secs(),
                claimed_label: receipt.claimed_label.clone(),
            },
        );
        // The motive arrives with the material. Until now Aleksy is an ordinary
        // resident with a grievance; the covert scorer has nothing to offer him
        // without both a goal and physical stock.
        commands
            .entity(agent)
            .insert(CovertGoal::new(script.goal, script.nerve));

        progress.supplied = true;
        radio.push(RadioEntry::new(
            channel_for(&script.role),
            script.supplied_line.clone(),
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script() -> BotanistScript {
        ron::from_str(include_str!("../../assets/data/station.botanist.ron"))
            .expect("station.botanist.ron parses")
    }

    /// The thread must belong to Botany and stay off the customer roster, the
    /// same two rules every other department minor follows.
    #[test]
    fn the_botanist_thread_is_botanys_and_is_not_a_customer() {
        let script = script();
        assert_eq!(
            crate::orders::Department::from_role(&script.role),
            Some(crate::orders::Department::Botany)
        );
        let roster = std::fs::read_to_string("assets/data/station.crew.ron").unwrap();
        assert!(
            !roster.contains(&script.name),
            "a minor antagonist on the customer roster would be double-booked by ordinary orders"
        );
        assert!(!script.visits.is_empty());
        assert!(script.withheld_penalty < 0);
        assert!(!script.withheld_lines.is_empty());
    }

    /// The load-bearing safety property of this whole thread: the escalation
    /// depends on a reagent that only a player can supply.
    #[test]
    fn the_final_ask_is_a_player_only_reagent_and_the_others_are_not() {
        let data = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap();
        let script = script();
        let final_ask = script.final_ask().expect("the thread authors a final ask");

        let id = data
            .reagents
            .id_of(&final_ask.reagent)
            .expect("the final ask names a real reagent");
        assert!(
            data.reagents.get(id).player_only,
            "'{}' must be player-only, or Aleksy could obtain it without the player",
            final_ask.reagent
        );

        // Every earlier ask must NOT be player-only. They are ordinary
        // horticulture and stay useful to a player who just wants a working
        // greenhouse; tagging them would make the whole thread a trap.
        for visit in &script.visits[..script.visits.len() - 1] {
            let id = data
                .reagents
                .id_of(&visit.reagent)
                .unwrap_or_else(|| panic!("'{}' names no real reagent", visit.reagent));
            assert!(
                !data.reagents.get(id).player_only,
                "'{}' is an ordinary ask and must not be player-only",
                visit.reagent
            );
        }
    }

    /// The upside rule, on the one thread that currently exercises it: the
    /// final ask must promise the player something concrete, or the whole
    /// choice is a trap dressed as a decision.
    #[test]
    fn the_final_ask_promises_a_concrete_player_benefit() {
        let request = script().request().expect("the thread authors a final ask");
        assert_eq!(request.validate(), Ok(()));
        assert!(
            !request.benefits.is_empty(),
            "supplying Aleksy must buy the player something"
        );

        // And it must be worth a non-zero amount, not a placeholder entry.
        assert!(
            request.benefits.iter().any(|benefit| match benefit {
                PlayerBenefit::Standing { amount, .. } => *amount > 0,
                PlayerBenefit::Underworld { amount } => *amount > 0,
                PlayerBenefit::Supplies { amount, .. } => *amount > 0,
                PlayerBenefit::Favor { .. } => true,
            }),
            "a benefit worth zero satisfies the letter of the rule and none of its point"
        );
    }

    /// Aleksy refuses substitutes for the last ask specifically. He will take
    /// less, but not something else — offering a lookalike is the player
    /// signalling they have worked out what it is for.
    #[test]
    fn the_final_ask_accepts_a_smaller_amount_but_no_substitute() {
        let request = script().request().expect("the thread authors a final ask");
        assert!(request.allows(&DealResponse::Negotiate));
        assert!(!request.allows(&DealResponse::Counteroffer {
            substitute: "ammonia".into()
        }));
        assert!(
            request.supplied_amount(&DealResponse::Negotiate) < request.amount,
            "a negotiated deal must hand over physically less"
        );
    }

    /// Refusal and compliance must differ in *kind*, not just in degree. If a
    /// refused thread eventually produced the same poisoning, the choice the
    /// player made would have been decorative.
    #[test]
    fn refusing_closes_the_worst_branch_rather_than_delaying_it() {
        let script = script();
        // The only authored consequence of refusal is standing and radio
        // chatter. Nothing in the script grants stock, a goal, or an action.
        assert!(script.withheld_penalty < 0);
        assert!(!script.withheld_lines.is_empty());
        assert!(
            script.goal == PrivateGoal::DiscreditDepartment,
            "the motive must be one poisoning can actually serve"
        );
        assert!(
            (0.0..=1.0).contains(&script.nerve),
            "nerve is a normalized risk tolerance"
        );
    }

    /// Drives the real resolution handler.
    ///
    /// Deliberately includes `DeliveryReceipt`: the handover path now reads the
    /// physical record rather than reconstructing one, so a harness without it
    /// would test the fallback instead of the fix.
    fn resolution_app() -> App {
        let mut app = App::new();
        let data = chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap();
        app.init_resource::<Time>()
            .insert_resource(crate::chem_data::ChemDb(data))
            .insert_resource(Script(script()))
            .init_resource::<BotanistProgress>()
            .init_resource::<Shift>()
            .init_resource::<RadioLog>()
            .init_resource::<IllicitCustody>()
            .init_resource::<crate::shift::CustodyNames>()
            .init_resource::<crate::utility_ai::LiveApproaches>()
            .init_resource::<UnderworldStanding>()
            .add_message::<OrderResolved>()
            .add_message::<crate::orders::DeliveryReceipt>()
            .add_systems(Update, handle_botanist_resolution);
        app
    }

    fn final_ask_reagent(app: &App) -> chem_sim::ReagentId {
        let db = app.world().resource::<crate::chem_data::ChemDb>();
        let script = app.world().resource::<Script>();
        db.reagents
            .id_of(&script.final_ask().unwrap().reagent)
            .expect("the final ask names a real reagent")
    }

    /// A resolution for the final ask, graded however the caller says.
    fn final_resolution(app: &App, outcome: Outcome, remaining_fraction: f32) -> OrderResolved {
        let script = app.world().resource::<Script>();
        OrderResolved {
            name: script.name.clone(),
            role: script.role.clone(),
            reagent: Some(final_ask_reagent(app)),
            category: None,
            outcome,
            kind: crate::orders::OrderKind::Normal,
            quality: Some(crate::orders::DeliveryQuality {
                purity: 1.0,
                potency: 1,
                remaining_fraction,
            }),
            development: false,
            campaign: None,
            counter_step: None,
        }
    }

    /// Walks the progress cursor to the final ask without going through the
    /// spawner, which needs a map and a station this harness has no use for.
    fn advance_to_final_ask(app: &mut App) {
        let steps = app.world().resource::<Script>().visits.len() - 1;
        app.world_mut().resource_mut::<BotanistProgress>().step = steps;
    }

    /// Finding 1, first half. `remaining_fraction` is remaining *patience*, so
    /// a complete batch delivered slowly used to fall under the old 0.5 floor
    /// and be recorded as a refusal — the player made exactly what was asked
    /// for and was charged for withholding it.
    #[test]
    fn a_late_but_valid_final_delivery_is_a_handover_not_a_refusal() {
        let mut app = resolution_app();
        advance_to_final_ask(&mut app);

        let reagent = final_ask_reagent(&app);
        let mut accepted = chem_sim::Solution::unbounded();
        let _ = accepted.add(reagent, chem_sim::Units::from_f64(12.0));
        let script_name = app.world().resource::<Script>().name.clone();

        // Almost out of patience, and completely correct.
        let resolution = final_resolution(&app, Outcome::Success, 0.02);
        app.world_mut().write_message(resolution);
        app.world_mut()
            .write_message(crate::orders::DeliveryReceipt {
                id: 1,
                recipient: script_name,
                supplier: None,
                accepted,
                claimed_label: "feed stock".into(),
                request: "Grower Aleksy".into(),
            });
        app.update();

        let progress = app.world().resource::<BotanistProgress>();
        assert!(
            progress.supplied,
            "a complete, correct, late batch is still a handover"
        );
        assert_eq!(
            progress.refusals, 0,
            "delivering what was asked for is not a refusal"
        );
    }

    /// Finding 1, second half. The handler used to build its own solution from
    /// the authored amount, so whatever the player actually made was discarded
    /// and replaced with pure reagent. What he holds must be what was carried
    /// to the window — mixture, purity, volume and all.
    #[test]
    fn custody_receives_the_delivered_mixture_not_an_authored_reconstruction() {
        let mut app = resolution_app();
        advance_to_final_ask(&mut app);

        let reagent = final_ask_reagent(&app);
        let db = app.world().resource::<crate::chem_data::ChemDb>();
        let filler = db
            .reagents
            .id_of("water")
            .expect("water exists in the reagent table");
        let script_name = app.world().resource::<Script>().name.clone();

        // Half the authored amount, cut with something else.
        let mut accepted = chem_sim::Solution::unbounded();
        let _ = accepted.add(reagent, chem_sim::Units::from_f64(6.0));
        let _ = accepted.add(filler, chem_sim::Units::from_f64(9.0));

        let grower = app
            .world_mut()
            .spawn(CrewMember {
                name: script_name.clone(),
                role: "Botany".into(),
            })
            .id();

        let resolution = final_resolution(&app, Outcome::Success, 0.9);
        app.world_mut().write_message(resolution);
        app.world_mut()
            .write_message(crate::orders::DeliveryReceipt {
                id: 2,
                recipient: script_name,
                supplier: None,
                accepted,
                claimed_label: "feed stock".into(),
                request: "Grower Aleksy".into(),
            });
        app.update();

        let custody = app.world().resource::<IllicitCustody>();
        let stock: Vec<_> = custody.held_by(grower).collect();
        assert_eq!(stock.len(), 1, "exactly one batch changed hands");
        let held = &stock[0].solution;
        assert_eq!(
            held.volume_of(reagent),
            chem_sim::Units::from_f64(6.0),
            "he holds what was delivered, not the authored amount"
        );
        assert_eq!(
            held.volume_of(filler),
            chem_sim::Units::from_f64(9.0),
            "the rest of the mixture stays with the batch as evidence"
        );
    }

    /// A success that never physically changed hands cannot stock him. Without
    /// this the receipt would be an optimisation rather than the authority on
    /// what he holds.
    #[test]
    fn a_graded_success_with_no_receipt_hands_over_nothing() {
        let mut app = resolution_app();
        advance_to_final_ask(&mut app);
        let script_name = app.world().resource::<Script>().name.clone();
        let grower = app
            .world_mut()
            .spawn(CrewMember {
                name: script_name,
                role: "Botany".into(),
            })
            .id();

        let resolution = final_resolution(&app, Outcome::Success, 0.9);
        app.world_mut().write_message(resolution);
        app.update();

        let custody = app.world().resource::<IllicitCustody>();
        assert_eq!(
            custody.held_by(grower).count(),
            0,
            "no physical handover, no stock"
        );
    }

    /// Earlier asks are ordinary horticulture. Meeting one is cooperation, and
    /// charging it as a withholding made helping indistinguishable from
    /// stonewalling.
    #[test]
    fn meeting_an_early_ask_is_not_charged_as_a_refusal() {
        let mut app = resolution_app();
        let script = app.world().resource::<Script>();
        let script_name = script.name.clone();
        let role = script.role.clone();
        let early = script.visits[0].reagent.clone();
        let reagent = {
            let db = app.world().resource::<crate::chem_data::ChemDb>();
            db.reagents.id_of(&early).expect("an authored reagent")
        };

        app.world_mut().write_message(OrderResolved {
            name: script_name,
            role,
            reagent: Some(reagent),
            category: None,
            outcome: Outcome::Success,
            kind: crate::orders::OrderKind::Normal,
            quality: None,
            development: false,
            campaign: None,
            counter_step: None,
        });
        app.update();

        let progress = app.world().resource::<BotanistProgress>();
        assert_eq!(progress.refusals, 0, "an early success is not a refusal");
        assert_eq!(progress.step, 1, "the thread moved on to the next ask");
        assert!(!progress.supplied, "the turn has not been reached yet");
        assert!(!progress.closed);
    }

    /// Finding 3. Nothing saved the motive, and `supplied: true` blocked the
    /// only path that inserted one — so reloading a supplied save produced a
    /// character carrying illicit stock he could never use. The threat
    /// evaporated silently, which is the worst way for one to end.
    #[test]
    fn a_reloaded_holder_gets_his_motive_back() {
        let mut app = resolution_app();
        app.add_systems(Update, restore_covert_goal);
        let script_name = app.world().resource::<Script>().name.clone();

        // The state a reload leaves behind: supplied, embodied, holding a real
        // batch, and with no goal component anywhere.
        app.world_mut().resource_mut::<BotanistProgress>().supplied = true;
        let grower = app
            .world_mut()
            .spawn(CrewMember {
                name: script_name,
                role: "Botany".into(),
            })
            .id();
        let reagent = final_ask_reagent(&app);
        let mut solution = chem_sim::Solution::unbounded();
        let _ = solution.add(reagent, chem_sim::Units::from_f64(12.0));
        app.world_mut()
            .resource_mut::<IllicitCustody>()
            .receive_from_player(
                grower,
                IllicitStock {
                    solution,
                    state: CustodyState::Carried,
                    source_player: None,
                    received_at: 0.0,
                    claimed_label: "feed stock".into(),
                },
            );
        assert!(app.world().get::<CovertGoal>(grower).is_none());

        app.update();

        assert!(
            app.world().get::<CovertGoal>(grower).is_some(),
            "a holder who kept his batch across a reload is still a threat"
        );
    }

    /// The material is the authority. Confiscating the batch has to end the
    /// motive with it, or restoring one would resurrect a danger the player
    /// already dealt with.
    #[test]
    fn a_confiscated_batch_does_not_restore_a_motive() {
        let mut app = resolution_app();
        app.add_systems(Update, restore_covert_goal);
        let script_name = app.world().resource::<Script>().name.clone();

        app.world_mut().resource_mut::<BotanistProgress>().supplied = true;
        let grower = app
            .world_mut()
            .spawn(CrewMember {
                name: script_name,
                role: "Botany".into(),
            })
            .id();
        let reagent = final_ask_reagent(&app);
        let mut solution = chem_sim::Solution::unbounded();
        let _ = solution.add(reagent, chem_sim::Units::from_f64(12.0));
        {
            let mut custody = app.world_mut().resource_mut::<IllicitCustody>();
            custody.receive_from_player(
                grower,
                IllicitStock {
                    solution,
                    state: CustodyState::Carried,
                    source_player: None,
                    received_at: 0.0,
                    claimed_label: "feed stock".into(),
                },
            );
            custody.resolve(grower, CustodyState::Confiscated);
        }

        app.update();

        assert!(
            app.world().get::<CovertGoal>(grower).is_none(),
            "taking the batch away is supposed to be the end of it"
        );
    }

    /// Refusing the last ask ends the thread for good. Without a terminal
    /// state distinct from `supplied` it would re-arm and ask again, which
    /// reads as the refusal never having registered.
    #[test]
    fn refusing_the_final_ask_closes_the_branch_without_stock() {
        let mut app = resolution_app();
        advance_to_final_ask(&mut app);

        let resolution = final_resolution(&app, Outcome::Expired, 0.0);
        app.world_mut().write_message(resolution);
        app.update();

        let progress = app.world().resource::<BotanistProgress>();
        assert!(progress.closed, "the final ask was put and not met");
        assert!(!progress.supplied, "nothing was handed over");
        assert_eq!(progress.refusals, 1);
    }

    // -----------------------------------------------------------------------
    // The sequence, walked rather than skipped
    // -----------------------------------------------------------------------
    //
    // Every test above that needs the last ask gets there with
    // `advance_to_final_ask`, which writes `progress.step` directly. That is
    // the right shortcut for testing what the *final* handler does, and it is
    // why those tests are short. But it means nothing in this file ever drove
    // ask 1 -> 2 -> 3 -> 4, and the cursor is the thread: it decides which
    // reagent is asked for, whether `RetainsDelivery` is attached, and whether
    // a resolution is graded as final. Skipping to the end tests the
    // destination and never the road.

    /// Resolves whichever ask the cursor is currently on, with the reagent
    /// that ask actually names.
    ///
    /// Deliberately reads the script rather than taking a reagent from the
    /// caller: a test that passed in the reagent it expected would keep
    /// passing if the cursor pointed somewhere else entirely, which is the one
    /// failure this section exists to catch.
    fn resolve_current_ask(app: &mut App, outcome: Outcome) {
        let (name, role, reagent_key) = {
            let script = app.world().resource::<Script>();
            let step = app.world().resource::<BotanistProgress>().step;
            let visit = script
                .visits
                .get(step)
                .expect("the cursor points at a real ask");
            (
                script.name.clone(),
                script.role.clone(),
                visit.reagent.clone(),
            )
        };
        let reagent = {
            let db = app.world().resource::<crate::chem_data::ChemDb>();
            db.reagents
                .id_of(&reagent_key)
                .expect("every authored ask names a real reagent")
        };
        app.world_mut().write_message(OrderResolved {
            name,
            role,
            reagent: Some(reagent),
            category: None,
            outcome,
            kind: crate::orders::OrderKind::Normal,
            quality: None,
            development: false,
            campaign: None,
            counter_step: None,
        });
        app.update();
    }

    /// Which reagent the thread would ask for right now.
    fn current_ask_reagent(app: &App) -> Option<String> {
        let step = app.world().resource::<BotanistProgress>().step;
        app.world()
            .resource::<Script>()
            .visits
            .get(step)
            .map(|visit| visit.reagent.clone())
    }

    /// Helping with every early ask walks the thread to the turn, and charges
    /// nothing along the way.
    ///
    /// The whole first half of this thread is a player being helpful. If any
    /// of those steps were graded as a withholding, cooperation would cost
    /// Botany standing and the player would be punished for the one behaviour
    /// the thread is trying to reward.
    #[test]
    fn helping_with_every_early_ask_reaches_the_turn_without_a_single_refusal() {
        let mut app = resolution_app();
        let asks = app.world().resource::<Script>().visits.len();
        assert!(asks >= 2, "the authored thread escalates over several asks");

        for step in 0..asks - 1 {
            assert_eq!(
                app.world().resource::<BotanistProgress>().step,
                step,
                "the cursor should be on ask {step}"
            );
            resolve_current_ask(&mut app, Outcome::Success);

            let progress = app.world().resource::<BotanistProgress>();
            assert_eq!(progress.refusals, 0, "helping is never a refusal");
            assert!(!progress.supplied, "the turn is not reached early");
            assert!(!progress.closed);
        }

        let progress = app.world().resource::<BotanistProgress>();
        assert_eq!(progress.step, asks - 1, "the cursor is on the final ask");
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .standing(crate::orders::Department::Botany),
            0,
            "a cooperative player has cost Botany nothing"
        );
    }

    /// The asks are put in the authored order, and the turn is last.
    ///
    /// Guards the ordering rather than the count. The escalation is the design:
    /// three requests that read as real horticulture, then one that does not.
    /// A cursor that skipped, repeated or reordered a step would still leave
    /// `step` arithmetic looking correct while asking for the player-only
    /// reagent second.
    #[test]
    fn the_asks_are_put_in_the_authored_order_and_the_turn_is_last() {
        let mut app = resolution_app();
        let authored: Vec<String> = app
            .world()
            .resource::<Script>()
            .visits
            .iter()
            .map(|visit| visit.reagent.clone())
            .collect();

        let mut seen = Vec::new();
        for _ in 0..authored.len() {
            seen.push(current_ask_reagent(&app).expect("an ask is pending"));
            resolve_current_ask(&mut app, Outcome::Success);
        }

        assert_eq!(seen, authored, "asks are put in the order they are written");

        let db = app.world().resource::<crate::chem_data::ChemDb>();
        let last = db
            .reagents
            .id_of(seen.last().expect("at least one ask"))
            .expect("a real reagent");
        assert!(
            db.reagents.get(last).player_only,
            "the turn must be the ask no NPC path could ever satisfy"
        );
        for early in &seen[..seen.len() - 1] {
            let id = db.reagents.id_of(early).expect("a real reagent");
            assert!(
                !db.reagents.get(id).player_only,
                "an early ask must be ordinary horticulture, not the turn"
            );
        }
    }

    /// Ignoring the thread still escalates it to the turn, and closes it there.
    ///
    /// The deliberate half of the cursor rule: *every* resolution advances,
    /// refusals included, so a player who ignores Aleksy still eventually gets
    /// asked the question that matters and still gets to answer it. The
    /// alternative — a thread that stalls on ask one forever — would mean
    /// ignoring it was strictly safer than engaging, and the choice the whole
    /// thread is built around would never be offered.
    #[test]
    fn ignoring_every_ask_still_reaches_the_turn_and_then_closes_the_branch() {
        let mut app = resolution_app();
        let asks = app.world().resource::<Script>().visits.len();

        for _ in 0..asks {
            resolve_current_ask(&mut app, Outcome::Expired);
        }

        let progress = app.world().resource::<BotanistProgress>();
        assert!(
            progress.closed,
            "the final ask was put, went unanswered, and the branch closed"
        );
        assert!(!progress.supplied, "nothing was ever handed over");
        assert_eq!(
            progress.refusals, asks as u32,
            "each ignored ask is charged once"
        );
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .standing(crate::orders::Department::Botany),
            app.world().resource::<Script>().withheld_penalty * asks as i32,
            "Botany's standing carries the cost of each withholding"
        );
        let name = app.world().resource::<Script>().name.clone();
        assert!(
            app.world()
                .resource::<crate::shift::CustodyNames>()
                .entity_of(&name)
                .is_none(),
            "a refused thread never puts a batch in his hands"
        );
    }

    /// A resolution arriving after the branch closed cannot reopen it or run
    /// the cursor past the end of the authored list.
    ///
    /// `progress.step` is clamped with `.min(script.visits.len())`, and
    /// `final_ask()` is looked up every frame. A stray late resolution — a
    /// duplicate message, an order graded after the thread ended — must be
    /// inert rather than indexing past the list or charging a fifth refusal
    /// against a thread that has nothing left to ask.
    #[test]
    fn a_stray_resolution_after_the_branch_closed_changes_nothing() {
        let mut app = resolution_app();
        let asks = app.world().resource::<Script>().visits.len();
        for _ in 0..asks {
            resolve_current_ask(&mut app, Outcome::Expired);
        }
        let settled = {
            let progress = app.world().resource::<BotanistProgress>();
            (progress.step, progress.refusals, progress.closed)
        };

        // The cursor is past the end now, so `resolve_current_ask` cannot read
        // an ask: send the final one again, as a duplicate would arrive.
        let resolution = final_resolution(&app, Outcome::Expired, 0.0);
        app.world_mut().write_message(resolution);
        app.update();

        let progress = app.world().resource::<BotanistProgress>();
        assert_eq!(
            (progress.step, progress.refusals, progress.closed),
            settled,
            "a late duplicate must not move the cursor or charge again"
        );
    }

    /// A save taken mid-sequence resumes on the same ask.
    ///
    /// `botanist_step` and `botanist_closed` are persisted, but nothing tested
    /// a reload from *between* asks — and the cursor is what decides which
    /// reagent is requested next. A save that resumed at step zero would put
    /// the whole escalation again from the beginning; one that resumed at the
    /// end would skip straight to the player-only ask.
    #[test]
    fn a_save_taken_between_asks_resumes_on_the_same_ask() {
        let mut app = resolution_app();
        resolve_current_ask(&mut app, Outcome::Success);
        resolve_current_ask(&mut app, Outcome::Success);
        let saved_step = app.world().resource::<BotanistProgress>().step;
        let expected = current_ask_reagent(&app).expect("an ask is still pending");

        // What a reload does: fresh progress, restored from the saved fields.
        let mut reloaded = resolution_app();
        {
            let mut progress = reloaded.world_mut().resource_mut::<BotanistProgress>();
            progress.step = saved_step;
            progress.closed = false;
        }

        assert_eq!(
            current_ask_reagent(&reloaded),
            Some(expected),
            "the reloaded thread asks for what it was about to ask for"
        );

        // And it still finishes correctly from there.
        let asks = reloaded.world().resource::<Script>().visits.len();
        for _ in saved_step..asks {
            resolve_current_ask(&mut reloaded, Outcome::Success);
        }
        assert!(
            reloaded.world().resource::<BotanistProgress>().step >= asks,
            "the resumed thread runs to the end of its list"
        );
    }
}
