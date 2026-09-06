//! What a player can *say* to an illicit approach, and what they get for it.
//!
//! `covert.rs` answers "what does an antagonist do with a batch". This module
//! answers the question in front of it: an NPC has asked for something, and the
//! player has more than two answers.
//!
//! ## The rule that shapes everything here
//!
//! An illicit deal must never be authored as only a delayed punishment. If
//! supplying a batch bought nothing except a later poisoning, the "choice" would
//! be a trap with extra dialogue, and a player who learned the pattern once
//! would correctly never engage again. So every request declares at least one
//! concrete [`PlayerBenefit`], and [`IllicitRequest::validate`] *rejects*
//! authored data that declares none — the invariant lives in data validation,
//! not in a review checklist.
//!
//! Crucially the benefit is granted by [`grant`] at the moment the deal
//! resolves, from the deal's own terms. It does not consult custody, goals, or
//! anything downstream, so it arrives whether or not the antagonist ever finds
//! a covert action worth taking. Rewards and harms therefore use separate
//! fields and separate code paths, and balancing one cannot silently erase the
//! other.
//!
//! ## Why the responses are not one enum with a severity dial
//!
//! [`DealResponse`] variants differ in *kind*. Refusing preserves the stock and
//! cools the approach; delaying keeps it warm and supplies nothing; deceiving
//! hands over a batch that is physically not what was claimed; reporting trades
//! underworld access for lawful trust. Collapsing any two of those into "same
//! outcome, different number" is the failure mode the plan calls out by name,
//! and `cooperate_delay_refuse_and_report_do_not_collapse` pins it.

use bevy::prelude::*;
use bevy_replicon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::antagonist::{nudge_underworld, UnderworldStanding};
use crate::crew::CrewMember;
use crate::orders::Shift;

use super::covert::{CustodyState, IllicitCustody};

/// Underworld goodwill a fully cooperative deal is worth.
///
/// Deliberately larger than the deceive path's: a batch that survives
/// inspection is worth more to a contact than one that does not.
const COOPERATE_UNDERWORLD: i32 = 4;

/// What reporting an approach to Security is worth in lawful standing.
const REPORT_SECURITY_STANDING: i32 = 2;

/// What a report costs in underworld access.
///
/// Must exceed the *total* a single deal can earn — the flat
/// [`COOPERATE_UNDERWORLD`] bonus **plus** any authored `Underworld` benefit —
/// or a player could cooperate, collect, then report and come out ahead on both
/// meters at once. The first version of this constant only beat the flat bonus
/// and `reporting_costs_more_underworld_access_than_one_deal_earns` caught the
/// gap immediately.
const REPORT_UNDERWORLD_COST: i32 = -12;

/// The fraction of a requested amount a negotiated part-deal supplies.
const NEGOTIATED_FRACTION: f32 = 0.5;

/// A concrete, immediate upside a deal grants.
///
/// Every variant names something the player can *observe changing*. There is
/// deliberately no `Goodwill`-style variant that means "you will be looked upon
/// favourably", because that is precisely the vague promise the plan forbids.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlayerBenefit {
    /// Personal standing with the requesting character, by name.
    Standing { name: String, amount: i32 },
    /// Opens the existing off-book offer economy further.
    Underworld { amount: i32 },
    /// A physical reward: a reagent and a real volume of it.
    Supplies { reagent: String, amount: u32 },
    /// A banked favor. Qualitative on the Crew page, real when called in.
    Favor { kind: FavorKind },
}

/// The favors a deal can bank. Each one changes a real job fact when spent,
/// which is what keeps "they owe us" from being flavour text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FavorKind {
    /// A freight run arrives early.
    ExpeditedFreight,
    /// One inspection looks the other way.
    QuietAccess,
    /// A warning before the next incident touching this department.
    AdvanceWarning,
}

impl PlayerBenefit {
    /// Whether this benefit is worth anything at all.
    ///
    /// A `Standing { amount: 0 }` entry would satisfy a naive "the list is
    /// non-empty" check while granting nothing — the exact loophole that would
    /// let the upside rule be satisfied on paper. Validation uses this.
    fn is_concrete(&self) -> bool {
        match self {
            Self::Standing { name, amount } => !name.is_empty() && *amount > 0,
            Self::Underworld { amount } => *amount > 0,
            Self::Supplies { reagent, amount } => !reagent.is_empty() && *amount > 0,
            Self::Favor { .. } => true,
        }
    }
}

/// How a player answered an illicit approach.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DealResponse {
    /// Supply exactly what was asked. Strongest reward, highest exposure.
    Cooperate,
    /// Supply less than was asked, for less. Only where terms allow it.
    Negotiate,
    /// Supply a safer functional substitute instead of the named reagent.
    Counteroffer { substitute: String },
    /// Accept the conversation, supply nothing yet. The approach stays live.
    Delay,
    /// Supply nothing and cool the approach.
    Refuse,
    /// Supply a batch that is not what it is claimed to be.
    Deceive { method: DeceptionMethod },
    /// Take the approach to Security.
    Report,
}

/// How a deceptive handover differs physically from what was claimed.
///
/// Each is discoverable by a different means, which is why they are not one
/// "fake" flag: dilution shows up in the dose, substitution in the chemistry,
/// and a marker only when someone looks for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeceptionMethod {
    /// Real reagent, less of it than claimed.
    Diluted,
    /// A different reagent entirely, under the requested label.
    Substituted,
    /// The real thing, tagged so its use can be traced back.
    Marked,
}

impl DealResponse {
    /// Whether this response physically hands anything over.
    ///
    /// The load-bearing distinction between the "no supply" responses. Delay
    /// and refuse both transfer nothing, but they differ in whether the
    /// approach stays live — see [`Self::cools_approach`].
    pub fn supplies_stock(&self) -> bool {
        matches!(
            self,
            Self::Cooperate | Self::Negotiate | Self::Counteroffer { .. } | Self::Deceive { .. }
        )
    }

    /// Whether the approach stops being live afterwards.
    pub fn cools_approach(&self) -> bool {
        matches!(self, Self::Refuse | Self::Report)
    }
}

/// One authored illicit request, and the terms the player can hear.
#[derive(Clone, Debug, Deserialize)]
pub struct IllicitRequest {
    /// Who is asking, by roster name.
    pub requester: String,
    pub reagent: String,
    pub amount: u32,
    /// At least one, enforced by [`Self::validate`].
    pub benefits: Vec<PlayerBenefit>,
    /// Which responses this character actually offers. Cooperate, delay,
    /// refuse and report are always available whether authored or not — see
    /// [`Self::allows`].
    #[serde(default)]
    pub options: Vec<String>,
    /// What a counteroffer may substitute, if this requester accepts one.
    #[serde(default)]
    pub accepts_substitute: Option<String>,
}

/// A request the player has actually been told about, and their answer so far.
///
/// The seam between the conversation and the physical handover. A player says
/// what they intend here; the delivery that follows is what makes it real, so
/// promising to cooperate and then never delivering is the same as delaying —
/// which is correct, and is why `answered` is a stance rather than a commitment.
///
/// Authority-only. A client learning which approaches are live would learn who
/// the antagonists are, which is the one thing that must stay hidden.
#[derive(Resource, Default)]
pub struct LiveApproaches {
    open: Vec<LiveApproach>,
}

/// One approach the player knows about.
#[derive(Clone, Debug)]
pub struct LiveApproach {
    pub request: IllicitRequest,
    /// `None` until the player says something. Distinct from `Delay`: not
    /// having answered yet is not the same as having chosen to stall, and only
    /// the latter is a decision the requester has heard.
    pub answered: Option<DealResponse>,
    /// Set once the deal has been resolved and its benefit granted, so a second
    /// delivery cannot collect the same reward twice.
    pub settled: bool,
}

impl LiveApproaches {
    /// Opens an approach, or returns the existing one for the same requester.
    ///
    /// Idempotent on purpose: an NPC who asks again across several visits is
    /// continuing one conversation, not starting a competing second one, and
    /// two live approaches from the same person would let a player collect the
    /// authored benefit twice.
    pub fn open(&mut self, request: IllicitRequest) {
        if self
            .open
            .iter()
            .any(|live| live.request.requester == request.requester)
        {
            return;
        }
        self.open.push(LiveApproach {
            request,
            answered: None,
            settled: false,
        });
    }

    pub fn get(&self, requester: &str) -> Option<&LiveApproach> {
        self.open
            .iter()
            .find(|live| live.request.requester == requester)
    }

    /// How many approaches this requester has open.
    ///
    /// Should only ever be 0 or 1. Exists because every other accessor finds
    /// the *first* match, which makes a duplicate invisible — the test that was
    /// supposed to pin `open`'s idempotence passed with the guard removed until
    /// it could count.
    pub fn count_for(&self, requester: &str) -> usize {
        self.open
            .iter()
            .filter(|live| live.request.requester == requester)
            .count()
    }

    /// What the player may currently say to this requester.
    ///
    /// Empty for a requester with no live approach — the options exist because
    /// someone asked, so there is nothing to answer before they do.
    pub fn options_for(&self, requester: &str) -> Vec<DealResponse> {
        let Some(live) = self.get(requester) else {
            return Vec::new();
        };
        if live.settled {
            return Vec::new();
        }
        [
            DealResponse::Cooperate,
            DealResponse::Negotiate,
            DealResponse::Delay,
            DealResponse::Refuse,
            DealResponse::Report,
            // Each deception is its own choice rather than a sub-menu: they
            // differ in *what physically changes hands*, and each is
            // discoverable by a different means — dilution in the dose,
            // substitution in the chemistry, a marker only if someone looks.
            // Folding them behind one "lie" button would hide the only
            // decision that matters.
            DealResponse::Deceive {
                method: DeceptionMethod::Diluted,
            },
            DealResponse::Deceive {
                method: DeceptionMethod::Substituted,
            },
            DealResponse::Deceive {
                method: DeceptionMethod::Marked,
            },
        ]
        .into_iter()
        .chain(
            live.request
                .accepts_substitute
                .clone()
                .map(|substitute| DealResponse::Counteroffer { substitute }),
        )
        .filter(|response| live.request.allows(response))
        .collect()
    }

    /// Records the player's answer. Returns the request it applies to.
    ///
    /// Refuses a response this requester does not offer rather than silently
    /// downgrading it, so a UI bug becomes a visible no-op instead of a
    /// different deal than the player chose.
    pub fn answer(&mut self, requester: &str, response: DealResponse) -> Option<&LiveApproach> {
        let live = self
            .open
            .iter_mut()
            .find(|live| live.request.requester == requester)?;
        if live.settled || !live.request.allows(&response) {
            return None;
        }
        live.answered = Some(response);
        Some(live)
    }

    /// Marks an approach resolved so its benefit cannot be collected twice.
    pub fn settle(&mut self, requester: &str) {
        if let Some(live) = self
            .open
            .iter_mut()
            .find(|live| live.request.requester == requester)
        {
            live.settled = true;
        }
    }

    /// Closes an approach entirely — refusing or reporting cools it.
    pub fn close(&mut self, requester: &str) {
        self.open.retain(|live| live.request.requester != requester);
    }

    pub fn clear(&mut self) {
        self.open.clear();
    }
}

/// Why a request's authored data was rejected.
#[derive(Debug, PartialEq, Eq)]
pub enum RequestError {
    /// The upside rule: no benefit at all.
    NoBenefit,
    /// A benefit that is present but worth nothing.
    EmptyBenefit,
    NoRequester,
    NoAmount,
}

impl IllicitRequest {
    /// The data-validation half of the upside rule.
    ///
    /// Called at load, so an authored request with no real benefit fails to
    /// start the game rather than shipping as a trap.
    pub fn validate(&self) -> Result<(), RequestError> {
        if self.requester.is_empty() {
            return Err(RequestError::NoRequester);
        }
        if self.amount == 0 {
            return Err(RequestError::NoAmount);
        }
        if self.benefits.is_empty() {
            return Err(RequestError::NoBenefit);
        }
        if !self.benefits.iter().any(PlayerBenefit::is_concrete) {
            return Err(RequestError::EmptyBenefit);
        }
        Ok(())
    }

    /// Whether this requester offers `response`.
    ///
    /// The four baseline responses are unconditional: a player can always
    /// cooperate, stall, say no, or walk to Security, because a character who
    /// could refuse the player's refusal would not be a character.
    pub fn allows(&self, response: &DealResponse) -> bool {
        match response {
            DealResponse::Cooperate
            | DealResponse::Delay
            | DealResponse::Refuse
            | DealResponse::Report => true,
            DealResponse::Counteroffer { substitute } => self
                .accepts_substitute
                .as_ref()
                .is_some_and(|allowed| allowed == substitute),
            DealResponse::Negotiate => self.options.iter().any(|o| o == "negotiate"),
            DealResponse::Deceive { .. } => true,
        }
    }

    /// How much actually changes hands for a given response.
    pub fn supplied_amount(&self, response: &DealResponse) -> u32 {
        match response {
            DealResponse::Cooperate => self.amount,
            // A negotiated part-deal and a diluted one hand over the same
            // reduced volume. They differ in what the *requester believes* they
            // received, which is why deception is discoverable later and an
            // honest partial deal is not.
            DealResponse::Negotiate
            | DealResponse::Deceive {
                method: DeceptionMethod::Diluted,
            } => ((self.amount as f32 * NEGOTIATED_FRACTION).round() as u32).max(1),
            // A substitution or a marked batch is full volume — the deceit is
            // in what it is, not how much.
            DealResponse::Deceive { .. } | DealResponse::Counteroffer { .. } => self.amount,
            DealResponse::Delay | DealResponse::Refuse | DealResponse::Report => 0,
        }
    }
}

/// What a resolved deal changed. Returned so callers and tests can assert on
/// the outcome rather than re-deriving it from four separate resources.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DealOutcome {
    pub granted: Vec<PlayerBenefit>,
    pub underworld_delta: i32,
    pub supplied: u32,
    /// Set when the approach is closed and will not be re-offered.
    pub cooled: bool,
    /// Set when Security learned of the approach through this response.
    pub reported: bool,
}

/// Resolves a player's answer: grants the promised upside and reports what
/// changed.
///
/// The upside is computed from the request's own authored terms and granted
/// here, unconditionally for any supplying response. Nothing in this function
/// reads custody, goals, or covert scoring — that independence is what makes
/// the benefit arrive "even if no later covert action becomes worthwhile", and
/// `the_promised_upside_arrives_even_when_no_covert_action_follows` pins it.
pub fn grant(
    request: &IllicitRequest,
    response: &DealResponse,
    shift: &mut Shift,
    underworld: &mut UnderworldStanding,
) -> DealOutcome {
    let mut outcome = DealOutcome {
        supplied: request.supplied_amount(response),
        cooled: response.cools_approach(),
        reported: matches!(response, DealResponse::Report),
        ..Default::default()
    };

    if outcome.reported {
        // Lawful trust up, off-book access down. The cost is deliberately
        // larger than one cooperation's gain so this is a real trade rather
        // than a strictly-better option that makes every other path pointless.
        shift.adjust(
            crate::orders::Department::Security,
            REPORT_SECURITY_STANDING,
        );
        nudge_underworld(underworld, REPORT_UNDERWORLD_COST);
        outcome.underworld_delta = REPORT_UNDERWORLD_COST;
        return outcome;
    }

    if !response.supplies_stock() {
        // Delay and refuse grant nothing. They are still not the same: refuse
        // sets `cooled`, delay leaves the approach live for a later change of
        // circumstance.
        return outcome;
    }

    // The scale a supplying response earns. A partial or dishonest handover is
    // worth less than the real thing, but never nothing — a substitute that
    // bought literally zero would collapse counteroffer into refuse.
    let scale = match response {
        DealResponse::Cooperate => 1.0,
        DealResponse::Negotiate => 0.6,
        DealResponse::Counteroffer { .. } => 0.5,
        DealResponse::Deceive { .. } => 0.35,
        _ => 0.0,
    };

    for benefit in &request.benefits {
        let scaled = match benefit {
            PlayerBenefit::Standing { name, amount } => {
                let amount = scaled_amount(*amount, scale);
                shift.adjust_npc(name, amount);
                PlayerBenefit::Standing {
                    name: name.clone(),
                    amount,
                }
            }
            PlayerBenefit::Underworld { amount } => {
                let amount = scaled_amount(*amount, scale);
                nudge_underworld(underworld, amount);
                outcome.underworld_delta += amount;
                PlayerBenefit::Underworld { amount }
            }
            PlayerBenefit::Supplies { reagent, amount } => PlayerBenefit::Supplies {
                reagent: reagent.clone(),
                amount: scaled_amount(*amount as i32, scale).max(1) as u32,
            },
            // A favor is indivisible: it is owed or it is not. Scaling it would
            // produce "half an expedited freight run", which means nothing.
            //
            // Banked as a real counter on `Requisition`, alongside the wards
            // bought at the standing board, so "they owe us" is a thing that
            // gets spent rather than a line of text. Each is consumed by one
            // named site — see `FavorKind`.
            PlayerBenefit::Favor { kind } => {
                let banked = match kind {
                    FavorKind::QuietAccess => &mut shift.requisition.quiet_access_favors,
                    FavorKind::ExpeditedFreight => &mut shift.requisition.expedited_freight_favors,
                    FavorKind::AdvanceWarning => &mut shift.requisition.advance_warning_favors,
                };
                *banked = banked.saturating_add(1);
                PlayerBenefit::Favor { kind: *kind }
            }
        };
        outcome.granted.push(scaled);
    }

    if matches!(response, DealResponse::Cooperate) {
        nudge_underworld(underworld, COOPERATE_UNDERWORLD);
        outcome.underworld_delta += COOPERATE_UNDERWORLD;
    }

    outcome
}

/// Scales a benefit without letting it round away to nothing.
///
/// A positive benefit must stay positive: rounding `1` down to `0` on a
/// counteroffer would silently violate the upside rule for small rewards.
fn scaled_amount(amount: i32, scale: f32) -> i32 {
    if amount <= 0 {
        return amount;
    }
    ((amount as f32 * scale).round() as i32).max(1)
}

/// Takes a supplied batch back out of an NPC's hands.
///
/// Recovery, confiscation and destruction differ in consequence but not in
/// physics, so they share `IllicitCustody::resolve`. Returns how many batches
/// were actually taken — zero when there was nothing spendable left, which is
/// what makes "recover it before it is used" a real race rather than an
/// always-available undo.
pub fn recover(custody: &mut IllicitCustody, holder: Entity, state: CustodyState) -> usize {
    custody.resolve(holder, state)
}

/// The part of a live approach a client may see, mirrored onto the requester.
///
/// Replicated *because the player was told it to their face* — the terms, and
/// which answers this person will entertain. Nothing here is a secret: it is
/// the content of a conversation the player just had.
///
/// What stays authority-only is everything around it — whether the requester
/// has a `CovertGoal`, what is in custody, and how any covert action scores. So
/// a client can draw the buttons without being able to infer who is dangerous:
/// an approach means someone asked for something, not that they intend harm.
#[derive(Component, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicApproach {
    /// What was asked for, in the words the player heard.
    pub reagent: String,
    pub amount: u32,
    /// Exactly the responses this requester entertains.
    pub options: Vec<DealResponse>,
    /// What the player has said so far, echoed back so the panel can show the
    /// current stance rather than pretending nothing was said.
    pub answered: Option<DealResponse>,
}

/// Mirrors each live approach onto its requester so clients can draw it.
///
/// Runs on the authority and writes a component the client reads. A requester
/// with no live approach has the component removed rather than left stale —
/// otherwise a settled deal would keep offering buttons that do nothing.
fn publish_approaches(
    approaches: Res<LiveApproaches>,
    mut commands: Commands,
    crew: Query<(Entity, &CrewMember, Option<&PublicApproach>)>,
) {
    for (entity, member, existing) in &crew {
        let live = approaches.get(&member.name).filter(|live| !live.settled);
        let Some(live) = live else {
            if existing.is_some() {
                commands.entity(entity).remove::<PublicApproach>();
            }
            continue;
        };
        let published = PublicApproach {
            reagent: live.request.reagent.clone(),
            amount: live.request.amount,
            options: approaches.options_for(&member.name),
            answered: live.answered.clone(),
        };
        if existing != Some(&published) {
            commands.entity(entity).insert(published);
        }
    }
}

/// Spends an `AdvanceWarning` favor when an incident opens.
///
/// One system reacting to `IncidentCreated` rather than a check inside each of
/// the four adapters that raise incidents: the favor is about the player being
/// told, not about which department had the accident, and spreading it over
/// four call sites would give it four chances to drift.
///
/// The warning is deliberately *late* — the incident has already happened, and
/// what the favor buys is hearing about it from a friend before it reaches the
/// board, not precognition. Anything earlier would need the director to consult
/// a career resource before deciding, which is a much larger change for a
/// smaller payoff.
fn spend_advance_warning(
    mut created: MessageReader<super::IncidentCreated>,
    mut shift: ResMut<Shift>,
    mut radio: ResMut<crate::radio::RadioLog>,
) {
    for incident in created.read() {
        if shift.requisition.advance_warning_favors == 0 {
            continue;
        }
        shift.requisition.advance_warning_favors -= 1;
        radio.push(
            crate::radio::RadioEntry::new(
                crate::radio::RadioChannel::Common,
                format!(
                    "Quiet word from a friend: {:?} has a problem, and it is about to be official.",
                    incident.department
                ),
            )
            .positive(),
        );
    }
}

/// A player answering an illicit approach.
///
/// Carries the requester's name rather than an `Entity` for the same reason
/// custody is saved by name: the crew member may be despawned, and the answer
/// is about the person, not the body. Nothing hidden crosses the wire — the
/// requester's name and the response are both things the player just chose.
#[derive(Message, Serialize, Deserialize)]
pub struct AnswerApproachRequested {
    pub requester: String,
    pub response: DealResponse,
}

/// Applies a player's answer on the authority.
///
/// The trust boundary every other client message here sits behind:
/// `LiveApproaches::answer` re-checks that the approach exists and that the
/// requester actually offers that response, so a stale button or a hand-crafted
/// message commits nothing rather than closing a deal that was never offered.
///
/// Refusing and reporting close the approach outright; refusing is not merely
/// "no reward", it takes the offer off the table. Reporting additionally grants
/// its lawful-standing trade immediately, because unlike a supply it needs no
/// later delivery to become real.
fn handle_approach_answers(
    mut requests: MessageReader<FromClient<AnswerApproachRequested>>,
    mut approaches: ResMut<LiveApproaches>,
    mut shift: ResMut<Shift>,
    mut underworld: ResMut<UnderworldStanding>,
) {
    for request in requests.read() {
        let Some(live) = approaches.answer(&request.requester, request.response.clone()) else {
            continue;
        };
        let settled = match request.response {
            // A report is complete the moment it is made.
            DealResponse::Report => {
                let outcome = grant(
                    &live.request.clone(),
                    &DealResponse::Report,
                    &mut shift,
                    &mut underworld,
                );
                debug_assert!(outcome.reported);
                true
            }
            // Refusing settles nothing and grants nothing; it simply closes.
            DealResponse::Refuse => true,
            _ => false,
        };
        if settled {
            approaches.close(&request.requester);
        }
    }
}

pub(super) fn register(app: &mut App) {
    // Carries a name and a response, so there is no `Entity` for
    // `MapEntities` to translate — the same reasoning as `BuyHintRequested`.
    app.add_client_message::<AnswerApproachRequested>(Channel::Ordered)
        .replicate::<PublicApproach>()
        .init_resource::<LiveApproaches>()
        .add_systems(
            OnEnter(crate::AppState::Playing),
            reset_approaches
                .in_set(super::UtilityResetSet)
                .run_if(crate::net::is_authority),
        )
        .add_systems(
            Update,
            (
                handle_approach_answers,
                publish_approaches,
                spend_advance_warning,
            )
                .chain()
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing)),
        );
}

/// Live approaches belong to a career, not to the process.
///
/// Restored from `progress.ron` by the botanist thread rather than kept here:
/// an approach is only meaningful alongside the progress that opened it.
fn reset_approaches(mut approaches: ResMut<LiveApproaches>) {
    approaches.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> IllicitRequest {
        IllicitRequest {
            requester: "Grower Aleksy".into(),
            reagent: "quiet_rot".into(),
            amount: 12,
            benefits: vec![
                PlayerBenefit::Standing {
                    name: "Grower Aleksy".into(),
                    amount: 4,
                },
                PlayerBenefit::Underworld { amount: 3 },
            ],
            options: vec!["negotiate".into()],
            accepts_substitute: Some("ammonia".into()),
        }
    }

    fn world() -> (Shift, UnderworldStanding) {
        (Shift::default(), UnderworldStanding::default())
    }

    /// A favor is a banked counter, not a line of text.
    ///
    /// The half of the upside rule that was outstanding longest: a deal that
    /// "grants" a favor nothing can spend is exactly the vague promise the rule
    /// forbids, just with a struct around it.
    #[test]
    fn a_granted_favor_is_banked_where_something_can_spend_it() {
        let mut req = request();
        req.benefits = vec![
            PlayerBenefit::Favor {
                kind: FavorKind::QuietAccess,
            },
            PlayerBenefit::Favor {
                kind: FavorKind::ExpeditedFreight,
            },
            PlayerBenefit::Favor {
                kind: FavorKind::AdvanceWarning,
            },
        ];
        let (mut shift, mut underworld) = world();
        grant(&req, &DealResponse::Cooperate, &mut shift, &mut underworld);

        assert_eq!(shift.requisition.quiet_access_favors, 1);
        assert_eq!(shift.requisition.expedited_freight_favors, 1);
        assert_eq!(shift.requisition.advance_warning_favors, 1);

        // Owed twice means owed twice; they accumulate like every other ward.
        grant(&req, &DealResponse::Cooperate, &mut shift, &mut underworld);
        assert_eq!(shift.requisition.quiet_access_favors, 2);
    }

    /// A favor is indivisible. Scaling it would produce "half an expedited
    /// freight run", which means nothing — so a partial deal still owes a whole
    /// favor or none at all.
    #[test]
    fn a_favor_is_not_scaled_down_by_a_partial_deal() {
        let mut req = request();
        req.benefits = vec![PlayerBenefit::Favor {
            kind: FavorKind::QuietAccess,
        }];
        let (mut shift, mut underworld) = world();
        grant(
            &req,
            &DealResponse::Deceive {
                method: DeceptionMethod::Diluted,
            },
            &mut shift,
            &mut underworld,
        );
        assert_eq!(
            shift.requisition.quiet_access_favors, 1,
            "a favor is owed or it is not; 0.35 of one is not a thing"
        );
    }

    /// An `AdvanceWarning` is spent by a real incident, exactly once.
    #[test]
    fn an_advance_warning_is_spent_by_one_incident_and_not_banked_forever() {
        let mut app = App::new();
        app.init_resource::<Shift>()
            .init_resource::<crate::radio::RadioLog>()
            .add_message::<super::super::IncidentCreated>()
            .add_systems(Update, spend_advance_warning);
        app.world_mut()
            .resource_mut::<Shift>()
            .requisition
            .advance_warning_favors = 1;

        let incident = |app: &mut App| {
            app.world_mut()
                .write_message(super::super::IncidentCreated {
                    id: super::super::IncidentId(1),
                    kind: super::super::IncidentKind::Burn,
                    department: super::super::JobDomain::Botany,
                    subject: Entity::from_raw_u32(3).unwrap(),
                    location: Vec3::ZERO,
                    severity: super::super::Normalized::new(0.5).unwrap(),
                });
            app.update();
        };

        incident(&mut app);
        assert_eq!(
            app.world()
                .resource::<Shift>()
                .requisition
                .advance_warning_favors,
            0,
            "the favor is spent, not banked indefinitely"
        );
        assert_eq!(
            app.world()
                .resource::<crate::radio::RadioLog>()
                .entries
                .len(),
            1,
            "spending it must actually tell the player something"
        );

        // A second incident with nothing banked says nothing.
        incident(&mut app);
        assert_eq!(
            app.world()
                .resource::<crate::radio::RadioLog>()
                .entries
                .len(),
            1,
            "an unowed warning must not fire"
        );
    }

    /// The player is offered every response the requester actually supports,
    /// and none that it does not.
    #[test]
    fn a_live_approach_offers_exactly_the_authored_responses() {
        let mut live = LiveApproaches::default();
        assert!(
            live.options_for("Grower Aleksy").is_empty(),
            "there is nothing to answer before anyone asks"
        );

        live.open(request());
        let options = live.options_for("Grower Aleksy");
        for expected in [
            DealResponse::Cooperate,
            DealResponse::Delay,
            DealResponse::Refuse,
            DealResponse::Report,
            DealResponse::Negotiate,
        ] {
            assert!(options.contains(&expected), "{expected:?} must be offered");
        }
        assert!(
            options.contains(&DealResponse::Counteroffer {
                substitute: "ammonia".into()
            }),
            "the authored substitute must be offered"
        );

        // A requester who accepts no substitute offers no counteroffer.
        let mut plain = request();
        plain.accepts_substitute = None;
        plain.options.clear();
        let mut live = LiveApproaches::default();
        live.open(plain);
        let options = live.options_for("Grower Aleksy");
        assert!(!options
            .iter()
            .any(|o| matches!(o, DealResponse::Counteroffer { .. })));
        assert!(!options.contains(&DealResponse::Negotiate));

        // The four baseline responses, plus the three deceptions — lying is
        // always available, because a character cannot stop the player from
        // handing over something other than what they promised.
        assert_eq!(options.len(), 7);
    }

    /// The three deceptions are offered as three distinct choices.
    ///
    /// They differ in what physically changes hands and in how each is caught,
    /// so collapsing them behind one "lie" button would hide the only decision
    /// that matters.
    #[test]
    fn each_deception_is_its_own_choice() {
        let mut live = LiveApproaches::default();
        live.open(request());
        let options = live.options_for("Grower Aleksy");

        for method in [
            DeceptionMethod::Diluted,
            DeceptionMethod::Substituted,
            DeceptionMethod::Marked,
        ] {
            assert!(
                options.contains(&DealResponse::Deceive { method }),
                "{method:?} must be offered on its own"
            );
        }

        // And they are not interchangeable: only dilution reduces the volume
        // that actually changes hands.
        let req = request();
        assert!(
            req.supplied_amount(&DealResponse::Deceive {
                method: DeceptionMethod::Diluted
            }) < req.supplied_amount(&DealResponse::Deceive {
                method: DeceptionMethod::Marked
            }),
            "a watered-down batch is physically smaller; a marked one is not"
        );
    }

    /// One person asking repeatedly is one conversation, not several.
    ///
    /// Without this a requester who visits three times would open three live
    /// approaches, and a player could collect the authored benefit once per
    /// copy for a single delivery.
    #[test]
    fn asking_again_continues_the_same_approach_rather_than_opening_a_second() {
        let mut live = LiveApproaches::default();
        live.open(request());
        live.answer("Grower Aleksy", DealResponse::Delay);
        live.open(request());

        // Counting, not just reading the first match: `get`, `answer` and
        // `settle` all find the *first* entry, so a duplicate appended behind
        // it is invisible to them. An earlier version of this test asserted
        // only on `answered` and passed with the idempotence guard deleted.
        assert_eq!(
            live.count_for("Grower Aleksy"),
            1,
            "re-asking must not open a second approach"
        );
        assert_eq!(
            live.get("Grower Aleksy").unwrap().answered,
            Some(DealResponse::Delay),
            "re-asking must not wipe the answer the player already gave"
        );

        live.settle("Grower Aleksy");
        live.open(request());
        assert_eq!(
            live.count_for("Grower Aleksy"),
            1,
            "a settled deal must not be joined by a fresh duplicate"
        );
        assert!(
            live.options_for("Grower Aleksy").is_empty(),
            "a settled deal cannot be re-opened for a second reward"
        );
    }

    /// A response the requester does not offer is rejected, not downgraded.
    #[test]
    fn an_unoffered_response_is_refused_rather_than_silently_changed() {
        let mut plain = request();
        plain.accepts_substitute = None;
        let mut live = LiveApproaches::default();
        live.open(plain);

        assert!(live
            .answer(
                "Grower Aleksy",
                DealResponse::Counteroffer {
                    substitute: "ammonia".into()
                }
            )
            .is_none());
        assert!(
            live.get("Grower Aleksy").unwrap().answered.is_none(),
            "a rejected answer must leave the stance untouched"
        );
        // Positive control: an offered response is accepted.
        assert!(live.answer("Grower Aleksy", DealResponse::Refuse).is_some());
    }

    /// Not having answered is not the same as choosing to stall.
    #[test]
    fn an_unanswered_approach_is_distinct_from_a_deliberate_delay() {
        let mut live = LiveApproaches::default();
        live.open(request());
        assert_eq!(live.get("Grower Aleksy").unwrap().answered, None);

        live.answer("Grower Aleksy", DealResponse::Delay);
        assert_eq!(
            live.get("Grower Aleksy").unwrap().answered,
            Some(DealResponse::Delay),
            "a heard refusal-to-decide is a real answer the requester received"
        );
    }

    /// The upside rule, enforced where it cannot be forgotten: in validation.
    #[test]
    fn a_request_with_no_benefit_is_rejected_by_validation() {
        let mut req = request();
        req.benefits.clear();
        assert_eq!(req.validate(), Err(RequestError::NoBenefit));

        // And the loophole: a benefit that exists but is worth nothing must
        // not satisfy the rule either.
        req.benefits = vec![PlayerBenefit::Standing {
            name: "Grower Aleksy".into(),
            amount: 0,
        }];
        assert_eq!(req.validate(), Err(RequestError::EmptyBenefit));

        // Positive control: the real authored shape passes.
        assert_eq!(request().validate(), Ok(()));
    }

    /// The independence property. `grant` never consults custody or goals, so
    /// the promised benefit lands whether or not harm ever follows.
    #[test]
    fn the_promised_upside_arrives_even_when_no_covert_action_follows() {
        let (mut shift, mut underworld) = world();
        let req = request();
        let outcome = grant(&req, &DealResponse::Cooperate, &mut shift, &mut underworld);

        assert_eq!(outcome.granted.len(), 2, "both authored benefits granted");
        assert_eq!(shift.npc_standing.get("Grower Aleksy").copied(), Some(4));
        assert!(
            underworld.level() > 0,
            "cooperation must open off-book access immediately"
        );
        // No covert action has occurred, no custody exists, no goal is set —
        // and the player is still paid.
        assert_eq!(outcome.supplied, 12);
    }

    /// The plan's explicit anti-collapse requirement.
    #[test]
    fn cooperate_delay_refuse_and_report_do_not_collapse() {
        let req = request();
        let run = |response: DealResponse| {
            let (mut shift, mut underworld) = world();
            let outcome = grant(&req, &response, &mut shift, &mut underworld);
            (
                outcome.supplied,
                outcome.cooled,
                outcome.reported,
                outcome.underworld_delta,
                shift.standing(crate::orders::Department::Security),
            )
        };

        let cooperate = run(DealResponse::Cooperate);
        let delay = run(DealResponse::Delay);
        let refuse = run(DealResponse::Refuse);
        let report = run(DealResponse::Report);

        // Every pair differs in at least one observable.
        let all = [cooperate, delay, refuse, report];
        for (i, a) in all.iter().enumerate() {
            for b in all.iter().skip(i + 1) {
                assert_ne!(a, b, "two response paths produced identical outcomes");
            }
        }

        // And the specific distinctions that matter:
        assert!(cooperate.0 > 0 && delay.0 == 0 && refuse.0 == 0);
        assert!(!delay.1, "delay leaves the approach live");
        assert!(refuse.1, "refuse cools it");
        assert!(report.2 && report.4 > 0, "report reaches Security");
        assert!(report.3 < 0, "report costs underworld access");
    }

    /// Reporting must be a trade, not a free win.
    ///
    /// Checked against the *real authored* request rather than only the test
    /// fixture: the exploit this guards against is "cooperate, collect, then
    /// report", and whether it is available depends on how generous the
    /// shipped data is. A constant tuned against a fixture would silently stop
    /// holding the moment someone authored a richer deal.
    #[test]
    fn reporting_costs_more_underworld_access_than_one_deal_earns() {
        let authored: crate::botanist::BotanistScript =
            ron::from_str(include_str!("../../assets/data/station.botanist.ron"))
                .expect("station.botanist.ron parses");

        for req in [request(), authored.request().expect("a final ask")] {
            let (mut shift, mut underworld) = world();
            let cooperate = grant(&req, &DealResponse::Cooperate, &mut shift, &mut underworld);

            let (mut shift, mut underworld) = world();
            let report = grant(&req, &DealResponse::Report, &mut shift, &mut underworld);

            assert!(
                report.underworld_delta.abs() > cooperate.underworld_delta,
                "'{}': report costs {} but the deal earns {} — dealing then reporting is free",
                req.requester,
                report.underworld_delta.abs(),
                cooperate.underworld_delta,
            );
        }
    }

    /// A substitute must be a different trade, not a secret alias for the
    /// requested reagent.
    #[test]
    fn a_counteroffer_supplies_the_substitute_and_earns_less() {
        let req = request();
        let (mut shift, mut underworld) = world();
        let full = grant(&req, &DealResponse::Cooperate, &mut shift, &mut underworld);

        let (mut shift, mut underworld) = world();
        let counter = grant(
            &req,
            &DealResponse::Counteroffer {
                substitute: "ammonia".into(),
            },
            &mut shift,
            &mut underworld,
        );

        assert!(
            counter.underworld_delta < full.underworld_delta,
            "a safer substitute must earn less than the real thing"
        );
        assert!(
            counter.underworld_delta > 0 || !counter.granted.is_empty(),
            "but it must still buy something, or it collapses into refuse"
        );
        // Only the authored substitute is accepted.
        assert!(req.allows(&DealResponse::Counteroffer {
            substitute: "ammonia".into()
        }));
        assert!(!req.allows(&DealResponse::Counteroffer {
            substitute: "phenol".into()
        }));
    }

    /// The four baseline responses are always available; the optional ones are
    /// gated by authored terms.
    #[test]
    fn the_baseline_responses_are_always_offered() {
        let mut req = request();
        req.options.clear();
        req.accepts_substitute = None;

        for response in [
            DealResponse::Cooperate,
            DealResponse::Delay,
            DealResponse::Refuse,
            DealResponse::Report,
        ] {
            assert!(req.allows(&response), "{response:?} must always be offered");
        }
        assert!(
            !req.allows(&DealResponse::Negotiate),
            "negotiation is authored, not universal"
        );
    }

    /// Deception is physically less than it claims, and earns accordingly.
    #[test]
    fn a_diluted_handover_supplies_less_than_it_claims() {
        let req = request();
        let diluted = DealResponse::Deceive {
            method: DeceptionMethod::Diluted,
        };
        assert!(
            req.supplied_amount(&diluted) < req.amount,
            "a diluted batch must physically contain less"
        );

        let (mut shift, mut underworld) = world();
        let honest = grant(&req, &DealResponse::Cooperate, &mut shift, &mut underworld);
        let (mut shift, mut underworld) = world();
        let deceived = grant(&req, &diluted, &mut shift, &mut underworld);
        assert!(
            deceived.underworld_delta < honest.underworld_delta,
            "a contact pays less for a batch that will not hold up"
        );
    }

    /// A benefit small enough to round away must not silently vanish.
    #[test]
    fn a_scaled_benefit_never_rounds_down_to_nothing() {
        let mut req = request();
        req.benefits = vec![PlayerBenefit::Standing {
            name: "Grower Aleksy".into(),
            amount: 1,
        }];
        let (mut shift, mut underworld) = world();
        grant(
            &req,
            &DealResponse::Deceive {
                method: DeceptionMethod::Diluted,
            },
            &mut shift,
            &mut underworld,
        );
        assert_eq!(
            shift.npc_standing.get("Grower Aleksy").copied(),
            Some(1),
            "0.35 * 1 rounds to 0; the upside rule requires it stay positive"
        );
    }

    /// Recovery removes the option rather than bookkeeping around it.
    #[test]
    fn recovering_a_batch_removes_it_from_spendable_stock() {
        let mut custody = IllicitCustody::default();
        let holder = Entity::from_raw_u32(7).unwrap();
        let mut solution = chem_sim::Solution::unbounded();
        let reagent = chem_sim::ReagentId(0);
        let _ = solution.add(reagent, chem_sim::Units::from_f64(50.0));
        custody.receive_from_player(
            holder,
            super::super::covert::IllicitStock {
                solution,
                state: CustodyState::Carried,
                source_player: None,
                received_at: 0.0,
                claimed_label: "feed stock".into(),
            },
        );
        assert!(custody.held_by(holder).any(|s| s.usable(reagent)));

        let taken = recover(&mut custody, holder, CustodyState::Confiscated);
        assert_eq!(taken, 1);
        assert!(
            !custody.held_by(holder).any(|s| s.usable(reagent)),
            "a confiscated batch must stop funding actions"
        );

        // A second recovery finds nothing — the race is real, not repeatable.
        assert_eq!(recover(&mut custody, holder, CustodyState::Confiscated), 0);
    }
}
