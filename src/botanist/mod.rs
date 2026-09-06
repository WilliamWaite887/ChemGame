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
use crate::orders::{OrderResolved, Outcome, Shift};
use crate::radio::{channel_for, RadioEntry, RadioLog};
use crate::utility_ai::{
    CovertGoal, CustodyState, DealResponse, IllicitCustody, IllicitRequest, IllicitStock,
    PlayerBenefit, PrivateGoal,
};
use crate::AppState;

/// How much of the final ask must actually arrive before it is worth holding.
///
/// A short delivery is not a handover. Without this an almost-empty container
/// would install the goal and stock the antagonist with nothing usable, which
/// reads to a player as "I refused and it happened anyway".
const MINIMUM_USEFUL_FRACTION: f32 = 0.5;

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
            .add_systems(OnEnter(AppState::Playing), reset_botanist_thread)
            .add_systems(
                Update,
                (track_custody_identity, handle_botanist_resolution)
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

    for order in resolved.read() {
        if order.name != script.name {
            continue;
        }

        let delivered_final = order
            .reagent
            .zip(db.reagents.id_of(&final_ask.reagent))
            .is_some_and(|(got, want)| got == want);
        let succeeded = matches!(order.outcome, Outcome::Success);

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
            continue;
        }

        // Quality gate. A mostly-empty container is not a handover.
        let enough = order
            .quality
            .is_none_or(|quality| quality.remaining_fraction >= MINIMUM_USEFUL_FRACTION);
        if !enough {
            progress.refusals = progress.refusals.saturating_add(1);
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

        let mut solution = chem_sim::Solution::unbounded();
        let _ = solution.add(reagent, chem_sim::Units::from_f64(final_ask.amount as f64));
        // Record who this entity is *before* handing anything over, so the
        // batch can be saved against a name. Crew entities do not survive
        // walking offscreen, let alone a reload.
        names.0.insert(agent, script.name.clone());
        custody.receive_from_player(
            agent,
            IllicitStock {
                solution,
                state: CustodyState::Carried,
                source_player: None,
                received_at: time.elapsed_secs(),
                claimed_label: final_ask.plea.clone(),
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
}
