//! Voluntary player aid: offering a real container to a real department.
//!
//! This is not a second delivery economy. An order is a request the station
//! made and will pay for; aid is something the player decided to do unasked,
//! and the station's response to it is the whole point. Nothing here awards
//! research, closes an order, or assumes success at handoff time.
//!
//! Three separations carry the design and each has a test that fails when it
//! is removed:
//!
//! 1. **Claim versus content.** [`AidBatch`] stores the player's `claimed_label`
//!    and the real [`chem_sim::Solution`] side by side and never reconciles
//!    them. Staff read the claim; bodies and processes get the contents. A
//!    trusted player can talk a strained department into accepting something
//!    dangerous, and trust still does not change the chemistry.
//! 2. **Aid versus deal.** A batch in a department intake can never satisfy an
//!    illicit request or enter private illicit custody. Those move only through
//!    an embodied deal handoff — see [`super::deals`].
//! 3. **Handoff versus outcome.** Offering only produces `AwaitingAssessment`.
//!    Standing, suspicion, and campaign consequences are applied once, at the
//!    point of *observed outcome*, and [`AidBatch::consequences_applied`] is
//!    what stops a long-lived batch from paying out twice.

use bevy::prelude::*;
use bevy_replicon::prelude::*;
use serde::{Deserialize, Serialize};

use super::jobs::JobDomain;
use super::public::{DepartmentCondition, PublicDepartmentStatus};

/// Where a batch is in the department's handling of it.
///
/// Terminal states (`Used`, `Disposed`, `Returned`) are kept rather than
/// removed so the Crew menu can show "outcome observed" and so a late-arriving
/// consequence cannot resurrect a batch that is already gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AidState {
    AwaitingAssessment,
    Accepted,
    Stored,
    ReservedForUse,
    Used,
    Rejected,
    Returned,
    Quarantined,
    Disposed,
}

impl AidState {
    /// A batch nobody can act on any further.
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            AidState::Used | AidState::Returned | AidState::Disposed
        )
    }

    /// What the player is told. Deliberately coarse: `Quarantined` is visible
    /// because the department physically set the container aside, but *why*
    /// they were suspicious is not published.
    pub fn label(self) -> &'static str {
        match self {
            AidState::AwaitingAssessment => "Awaiting assessment",
            AidState::Accepted => "Accepted",
            AidState::Stored => "Stored",
            AidState::ReservedForUse => "In use",
            AidState::Used => "Used",
            AidState::Rejected => "Rejected",
            AidState::Returned => "Returned",
            AidState::Quarantined => "Quarantined",
            AidState::Disposed => "Disposed",
        }
    }
}

/// Why a department refused a batch.
///
/// Recorded on the authority so Security can later ask a coherent question.
/// Only the fact of rejection reaches the player, not the reason — an
/// antagonist who learns exactly which tell gave them away can dodge it next
/// time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AidRefusal {
    /// The container arrived with nothing usable in it.
    Empty,
    /// The department has no process or patient this could serve.
    NoUse,
    /// The claim and the container disagree in a way staff could see.
    LabelMismatch,
    /// Known-contraband contents. Donating it does not legitimize it.
    Contraband,
    /// The player is not trusted enough for an unlabelled offer.
    Untrusted,
}

/// Who gave what, when, and where. Stable for the life of the batch so a
/// consequence landing three shifts later still names the right player.
#[derive(Clone, Debug, PartialEq)]
pub struct AidProvenance {
    pub contributor: Entity,
    pub claimed_label: Option<String>,
    pub offered_at: f32,
    pub domain: JobDomain,
}

/// One voluntary contribution, authority-only.
///
/// The `Solution` lives here rather than on a container entity because the
/// container is removed from the world at handoff — the department took it.
#[derive(Clone, Debug)]
pub struct AidBatch {
    pub id: AidBatchId,
    pub provenance: AidProvenance,
    /// What is actually in it. Never derived from, and never reconciled with,
    /// `provenance.claimed_label`.
    pub contents: chem_sim::Solution,
    pub state: AidState,
    pub refusal: Option<AidRefusal>,
    /// Set once, when an outcome is observed and paid out. Guards against a
    /// second payout from a repeated or replayed observation.
    pub consequences_applied: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AidBatchId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AidError {
    UnknownBatch,
    /// The transition is not legal from the batch's current state.
    NotInState,
    /// A terminal batch cannot be acted on again.
    AlreadyFinished,
    /// The intake for this department already holds a batch.
    IntakeOccupied,
    /// Nothing usable was offered.
    Empty,
    /// This outcome has already paid out.
    AlreadyPaid,
}

/// Every department's intake, authority-only.
///
/// One slot per department, which is what makes the same-frame double-consume
/// impossible: two interaction handlers racing on one frame both try to fill
/// the same occupied slot and the second is refused.
#[derive(Resource, Default)]
pub struct AidIntakes {
    batches: std::collections::HashMap<AidBatchId, AidBatch>,
    /// The batch currently sitting in each department's intake, if any.
    occupied: std::collections::HashMap<JobDomain, AidBatchId>,
    next_id: u64,
}

impl AidIntakes {
    /// Accepts a physical offer into a department intake.
    ///
    /// Returns `IntakeOccupied` rather than queueing. The intake is a real
    /// surface with a real container on it; a second offer has nowhere to go
    /// until staff clear the first. That is also the same-frame guard.
    pub fn offer(
        &mut self,
        domain: JobDomain,
        provenance: AidProvenance,
        contents: chem_sim::Solution,
    ) -> Result<AidBatchId, AidError> {
        if contents.total_volume() <= chem_sim::Units::ZERO {
            return Err(AidError::Empty);
        }
        if self.occupied.contains_key(&domain) {
            return Err(AidError::IntakeOccupied);
        }
        self.next_id += 1;
        let id = AidBatchId(self.next_id);
        self.batches.insert(
            id,
            AidBatch {
                id,
                provenance,
                contents,
                state: AidState::AwaitingAssessment,
                refusal: None,
                consequences_applied: false,
            },
        );
        self.occupied.insert(domain, id);
        Ok(id)
    }

    pub fn get(&self, id: AidBatchId) -> Option<&AidBatch> {
        self.batches.get(&id)
    }

    /// Every batch this shift has seen, settled or not.
    ///
    /// Iteration order is unspecified — callers that apply consequences must
    /// be order-independent, which the once-only `claim_payout` guarantees.
    pub fn iter(&self) -> impl Iterator<Item = &AidBatch> {
        self.batches.values()
    }

    pub fn in_intake(&self, domain: JobDomain) -> Option<&AidBatch> {
        self.occupied
            .get(&domain)
            .and_then(|id| self.batches.get(id))
    }

    /// Records a department's assessment verdict.
    ///
    /// Only legal from `AwaitingAssessment`, so a second worker arriving at an
    /// already-judged batch cannot overturn the first verdict.
    pub fn assess(
        &mut self,
        id: AidBatchId,
        verdict: Result<(), AidRefusal>,
    ) -> Result<AidState, AidError> {
        let batch = self.batches.get_mut(&id).ok_or(AidError::UnknownBatch)?;
        if batch.state != AidState::AwaitingAssessment {
            return Err(AidError::NotInState);
        }
        batch.state = match verdict {
            Ok(()) => AidState::Accepted,
            Err(refusal) => {
                batch.refusal = Some(refusal);
                match refusal {
                    // Contraband is set aside for Security rather than handed
                    // back — returning it would put it straight back into the
                    // player's pocket with no record.
                    AidRefusal::Contraband => AidState::Quarantined,
                    _ => AidState::Rejected,
                }
            }
        };
        Ok(batch.state)
    }

    /// Moves an accepted batch through storage and reservation into use.
    pub fn advance(&mut self, id: AidBatchId, next: AidState) -> Result<(), AidError> {
        let batch = self.batches.get_mut(&id).ok_or(AidError::UnknownBatch)?;
        if batch.state.is_terminal() {
            return Err(AidError::AlreadyFinished);
        }
        let legal = matches!(
            (batch.state, next),
            (AidState::Accepted, AidState::Stored)
                | (AidState::Stored, AidState::ReservedForUse)
                | (AidState::ReservedForUse, AidState::Used)
                | (AidState::Rejected, AidState::Returned)
                | (AidState::Rejected, AidState::Disposed)
                | (AidState::Quarantined, AidState::Disposed)
                // Inspection can clear a quarantine, which is what makes a
                // Security hold recoverable rather than a silent confiscation.
                | (AidState::Quarantined, AidState::Accepted)
        );
        if !legal {
            return Err(AidError::NotInState);
        }
        batch.state = next;
        Ok(())
    }

    /// Takes the batch's real contents for a body or a process, exactly once.
    ///
    /// Draining is what makes a container un-reusable: the second caller finds
    /// an empty solution, so the same donation cannot treat two patients.
    pub fn take_contents(&mut self, id: AidBatchId) -> Result<chem_sim::Solution, AidError> {
        let batch = self.batches.get_mut(&id).ok_or(AidError::UnknownBatch)?;
        if batch.state != AidState::ReservedForUse {
            return Err(AidError::NotInState);
        }
        Ok(std::mem::replace(
            &mut batch.contents,
            chem_sim::Solution::unbounded(),
        ))
    }

    /// Claims the one-time right to apply this batch's consequences.
    ///
    /// Returns `AlreadyPaid` on every call after the first, which is what stops
    /// a repeated observation from paying standing twice.
    pub fn claim_payout(&mut self, id: AidBatchId) -> Result<(), AidError> {
        let batch = self.batches.get_mut(&id).ok_or(AidError::UnknownBatch)?;
        if batch.consequences_applied {
            return Err(AidError::AlreadyPaid);
        }
        batch.consequences_applied = true;
        Ok(())
    }

    /// Clears a finished batch out of the intake slot so the next offer fits.
    pub fn clear_intake(&mut self, domain: JobDomain) -> Option<AidBatchId> {
        let id = *self.occupied.get(&domain)?;
        let finished = self
            .batches
            .get(&id)
            .is_some_and(|batch| batch.state.is_terminal() || batch.state == AidState::Stored);
        if !finished {
            return None;
        }
        self.occupied.remove(&domain);
        Some(id)
    }

    pub fn len(&self) -> usize {
        self.batches.len()
    }

    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }
}

/// One donated batch as it is written to a save.
///
/// The contributor entity is deliberately *not* saved: player entities do not
/// survive a reload, and a stale id would attribute a consequence to whoever
/// happened to reuse it. What matters across sessions is that the department
/// holds a real batch with a real claim on it, so the standing it eventually
/// pays lands on the department rather than on a specific body.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AidRecord {
    pub domain: JobDomain,
    pub contents: chem_sim::Solution,
    pub state: AidState,
    #[serde(default)]
    pub claimed_label: Option<String>,
    #[serde(default)]
    pub refusal: Option<AidRefusal>,
    #[serde(default)]
    pub offered_at: f32,
    /// Whether this batch's consequences were already applied. Without it a
    /// reload would re-pay every settled donation still on the counter.
    #[serde(default)]
    pub consequences_applied: bool,
}

impl AidIntakes {
    /// The batches worth writing to a save.
    ///
    /// Terminal batches are skipped: a used, returned, or disposed-of donation
    /// is finished, and restoring it would put a container back on a counter
    /// that the department has already dealt with. Same reasoning as
    /// `IllicitCustody::snapshot` skipping confiscated stock.
    pub fn snapshot(&self) -> Vec<AidRecord> {
        let mut records: Vec<AidRecord> = self
            .batches
            .values()
            .filter(|batch| !batch.state.is_terminal())
            .map(|batch| AidRecord {
                domain: batch.provenance.domain,
                contents: batch.contents.clone(),
                state: batch.state,
                claimed_label: batch.provenance.claimed_label.clone(),
                refusal: batch.refusal,
                offered_at: batch.provenance.offered_at,
                consequences_applied: batch.consequences_applied,
            })
            .collect();
        // `HashMap` iteration order is unspecified, and a save file that
        // reshuffles itself every write is a diff nobody can read.
        records.sort_by(|a, b| {
            a.offered_at
                .total_cmp(&b.offered_at)
                .then_with(|| format!("{:?}", a.domain).cmp(&format!("{:?}", b.domain)))
        });
        records
    }

    /// Rebuilds the intakes from a save, replacing whatever is there.
    ///
    /// Replaces rather than appends, so loading twice cannot double a
    /// department's stock — the same trap `IllicitCustody::restore` avoids.
    /// A record whose department already has an occupant is dropped: the
    /// intake is one physical counter, and a save that somehow holds two for
    /// one department describes a station that cannot exist.
    pub fn restore(&mut self, records: &[AidRecord], contributor: Entity) -> usize {
        *self = AidIntakes::default();
        for record in records {
            if self.occupied.contains_key(&record.domain) {
                continue;
            }
            self.next_id += 1;
            let id = AidBatchId(self.next_id);
            self.batches.insert(
                id,
                AidBatch {
                    id,
                    provenance: AidProvenance {
                        contributor,
                        claimed_label: record.claimed_label.clone(),
                        offered_at: record.offered_at,
                        domain: record.domain,
                    },
                    contents: record.contents.clone(),
                    state: record.state,
                    refusal: record.refusal,
                    consequences_applied: record.consequences_applied,
                },
            );
            self.occupied.insert(record.domain, id);
        }
        self.batches.len()
    }
}

/// The replicated summary of one department's intake.
///
/// Carries the *claim*, never the contents. A hidden poison reads as "accepted"
/// here until a body, an inspection, or a credible report says otherwise —
/// which is the design, not an oversight.
#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PublicAidStatus {
    pub department: crate::orders::Department,
    pub state: Option<AidState>,
    pub claimed_label: Option<String>,
}

/// The authored intake spot for one department, by [`super::jobs::UtilitySpots`] id.
///
/// One per department. Named rather than derived so a map can move an intake
/// without a code change, and so a department with no authored intake simply
/// cannot receive aid rather than defaulting to somewhere arbitrary.
pub fn intake_spot(domain: JobDomain) -> &'static str {
    match domain {
        JobDomain::Medical => "medical.aid_intake",
        JobDomain::Security => "security.aid_intake",
        JobDomain::Engineering => "engineering.aid_intake",
        JobDomain::Cargo => "cargo.aid_intake",
        JobDomain::Service => "service.aid_intake",
        JobDomain::Botany => "botany.aid_intake",
        JobDomain::Bridge => "bridge.aid_intake",
    }
}

/// A player offering what they are holding to a department.
///
/// Names the department rather than a container: the authority decides which
/// container that is from who is actually holding one, so a forged message
/// cannot donate someone else's beaker. This is its own message rather than a
/// reuse of `InteractRequested` precisely so one keypress cannot both deliver
/// an order and donate the same container — see
/// `an_aid_offer_is_not_a_delivery`.
#[derive(Message, Serialize, Deserialize, Clone, Copy, Debug)]
pub struct OfferAidRequested {
    pub department: crate::orders::Department,
}

/// Announced when a batch actually lands in an intake.
///
/// Carries only what a *witness* could have registered: who did it and where.
/// Deliberately not the batch id or its department — those are authority
/// bookkeeping, and a message that carried them would invite a consumer that
/// reacts to a donation it never saw. Anything needing the batch itself reads
/// [`AidIntakes`] directly, which is authority-only by construction.
#[derive(Message, Clone, Copy, Debug)]
pub struct AidOffered {
    pub contributor: Entity,
    pub at: Vec3,
}

/// Validates and commits one player handoff.
///
/// Every gate here exists because skipping it produces a specific exploit:
/// without the reach check a player donates from across the station, without
/// the held check they donate a container they are not carrying, and without
/// despawning the container they keep it.
#[allow(clippy::too_many_arguments)]
fn handle_aid_offers(
    mut commands: Commands,
    mut requests: MessageReader<FromClient<OfferAidRequested>>,
    mut intakes: ResMut<AidIntakes>,
    mut offered: MessageWriter<AidOffered>,
    spots: Res<super::jobs::UtilitySpots>,
    time: Res<Time>,
    chemists: Query<(Entity, &crate::player::Chemist)>,
    positions: Query<&Transform>,
    held: Query<(
        Entity,
        &crate::containers::Container,
        &crate::containers::HeldBy,
    )>,
    labels: Query<&crate::labels::Label>,
) {
    for request in requests.read() {
        let Some(player) = crate::machines::chemist_entity(&chemists, request.client_id) else {
            continue;
        };
        let domain = JobDomain::from_department(request.department);
        // A department with no authored intake in the map cannot take aid.
        let Some(spot) = spots.get(intake_spot(domain)) else {
            continue;
        };
        let Ok(at) = positions.get(player).map(|at| at.translation) else {
            continue;
        };
        if !crate::interaction::authority_target_in_reach(at, spot.at, crate::interaction::REACH) {
            continue;
        }
        let Some((container, contents, _)) = held
            .iter()
            .find(|(_, _, holder)| holder.0 == player)
            .map(|(entity, container, holder)| (entity, container.solution.clone(), holder))
        else {
            continue;
        };

        let provenance = AidProvenance {
            contributor: player,
            // The claim is whatever they wrote on it, including nothing. An
            // unlabelled offer is not a lie, but it is also not a reassurance.
            claimed_label: labels.get(container).ok().map(|label| label.0.clone()),
            offered_at: time.elapsed_secs(),
            domain,
        };
        if intakes.offer(domain, provenance, contents).is_err() {
            continue;
        }

        // The container leaves the hand exactly once. Despawning rather than
        // emptying is what makes a second offer of the same beaker impossible.
        commands.entity(container).despawn();
        offered.write(AidOffered {
            contributor: player,
            at: spot.at,
        });
    }
}

/// The capability a department needs to judge a donation.
///
/// Assessment is a real skill, not a formality: a department with nobody who
/// holds this simply leaves the batch sitting in the intake, which is a visible
/// consequence rather than a silent acceptance.
///
/// `&'static str` rather than a formatted `String` because department rosters
/// declare their capabilities as `&'static [&str]` — a runtime-built name could
/// never be added to one, so nobody would ever be qualified.
pub const fn assessment_capability(domain: JobDomain) -> &'static str {
    match domain {
        JobDomain::Medical => "medical.assess_aid",
        JobDomain::Security => "security.assess_aid",
        JobDomain::Engineering => "engineering.assess_aid",
        JobDomain::Cargo => "cargo.assess_aid",
        JobDomain::Service => "service.assess_aid",
        JobDomain::Botany => "botany.assess_aid",
        JobDomain::Bridge => "bridge.assess_aid",
    }
}

fn domain_key(domain: JobDomain) -> &'static str {
    match domain {
        JobDomain::Medical => "medical",
        JobDomain::Security => "security",
        JobDomain::Engineering => "engineering",
        JobDomain::Cargo => "cargo",
        JobDomain::Service => "service",
        JobDomain::Botany => "botany",
        JobDomain::Bridge => "bridge",
    }
}

/// Department standing at or below this reads as "I do not take unmarked
/// chemicals from this person". Above it, an unlabelled offer is merely odd.
const TRUSTED_ENOUGH_FOR_UNLABELLED: i32 = 5;

/// Pressure at or above this makes a department take a chance it otherwise
/// would not. This is the lever the whole system turns on: a strained
/// department is the one that accepts something it should have refused.
const PRESSURE_OVERRIDES_CAUTION: DepartmentCondition = DepartmentCondition::Strained;

/// What one department decides about one donated batch.
///
/// **This function deliberately cannot see the contents.** It takes the claimed
/// label and nothing else about what is in the container, which is what makes
/// the whole deception layer work: staff judge the claim, and reality happens
/// later when a body or a process actually consumes it. Passing the `Solution`
/// in here would collapse the design into "NPCs can smell poison".
///
/// `known_contraband` is the one exception, and it is not chemistry — it is
/// whether the *claim itself* names something the department already knows is
/// forbidden. Writing "meth" on a jar gets it quarantined regardless of whether
/// the jar contains meth.
pub fn assess_offer(
    claimed_label: Option<&str>,
    standing: i32,
    condition: DepartmentCondition,
    known_contraband: bool,
) -> Result<(), AidRefusal> {
    if known_contraband {
        return Err(AidRefusal::Contraband);
    }
    let under_pressure = condition >= PRESSURE_OVERRIDES_CAUTION;
    match claimed_label {
        // An unmarked container from someone the department barely knows is
        // refused — unless they are desperate enough to stop asking.
        None if standing <= TRUSTED_ENOUGH_FOR_UNLABELLED && !under_pressure => {
            Err(AidRefusal::Untrusted)
        }
        _ => Ok(()),
    }
}

/// Whether a claimed label names something the department already treats as
/// contraband.
///
/// Reads the **claim**, never the contents, and keys off the same
/// `Category::Illicit` / `controlled` / `explosive` rule Security's sweep uses
/// — so "meth" written on a jar of water is quarantined, and a jar of meth
/// labelled "saline" sails through. That asymmetry is the point: this is what
/// staff can see, and the chemistry catches up later.
fn claim_reads_as_contraband(db: &crate::chem_data::ChemDb, label: Option<&str>) -> bool {
    let Some(label) = label else {
        return false;
    };
    let label = label.to_lowercase();
    db.reagents.iter().any(|reagent| {
        let forbidden = reagent.categories.contains(&chem_sim::Category::Illicit)
            || reagent.controlled
            || reagent.explosive.is_some();
        forbidden
            && (label.contains(&reagent.name.to_lowercase())
                || label.contains(&reagent.key.to_lowercase()))
    })
}

/// A stable per-department ticket id, so a batch waiting several frames does
/// not publish a second ticket every frame.
fn assess_ticket_id(domain: JobDomain) -> super::jobs::JobTicketId {
    super::jobs::JobTicketId(super::stable_text_key(&format!(
        "aid.assess.{}",
        domain_key(domain)
    )))
}

/// How urgently staff should look at a donation. Deliberately below emergency
/// work: an unexamined beaker on a counter is not a casualty.
const ASSESS_URGENCY: f32 = 0.4;
const ASSESS_SECONDS: f32 = 3.0;

/// Publishes one assessment ticket per department holding an unjudged batch,
/// and withdraws it once the batch has been judged or cleared.
fn publish_assessment_tickets(
    time: Res<Time>,
    intakes: Res<AidIntakes>,
    spots: Res<super::jobs::UtilitySpots>,
    mut board: ResMut<super::jobs::JobBoard>,
) {
    for domain in JobDomain::ALL {
        let id = assess_ticket_id(domain);
        let waiting = intakes
            .in_intake(domain)
            .is_some_and(|batch| batch.state == AidState::AwaitingAssessment);

        if !waiting {
            // Only withdraw work nobody has started. A worker already walking
            // over keeps their claim and resolves it the ordinary way.
            if board
                .ticket(id)
                .is_some_and(|ticket| ticket.state == super::jobs::JobTicketState::Available)
            {
                board.cancel(id);
            }
            continue;
        }
        if board.ticket(id).is_some() {
            continue;
        }
        let Some(spot) = spots.get(intake_spot(domain)) else {
            continue;
        };
        board
            .publish(super::jobs::JobTicket {
                id,
                domain,
                kind: "aid.assess".into(),
                target: super::ActionTarget::Point(spot.at),
                subject: None,
                reservation: super::ReservationKey(format!("utility.spot.{}", intake_spot(domain))),
                reservation_capacity: spot.capacity,
                bucket: super::UtilityBucket::Routine,
                urgency: super::Normalized::new(ASSESS_URGENCY).expect("authored constant"),
                required_capability: super::jobs::JobCapability::new(assessment_capability(domain)),
                created_at: time.elapsed_secs(),
                deadline: None,
                risk: super::Normalized::ZERO,
                perform_seconds: ASSESS_SECONDS,
                state: super::jobs::JobTicketState::Available,
            })
            .expect("the ticket id was just checked");
    }
}

/// Applies a worker's verdict once they have actually stood at the intake and
/// finished the assessment action.
///
/// The verdict is computed *here*, at resolution, not when the ticket was
/// published — so it reads the department's condition and standing as they are
/// when the worker looks at the container, not as they were when it arrived.
/// A department that got desperate while the batch sat there decides
/// differently, which is the behaviour the whole system is for.
fn apply_assessment_results(
    mut results: MessageReader<super::UtilityActionResolved>,
    mut intakes: ResMut<AidIntakes>,
    mut board: ResMut<super::jobs::JobBoard>,
    db: Option<Res<crate::chem_data::ChemDb>>,
    shift: Option<Res<crate::orders::Shift>>,
    status: Query<(&super::public::DepartmentStatusRow, &PublicDepartmentStatus)>,
) {
    for result in results.read() {
        if result.key.action != super::UtilityActionId::PerformJob
            || result.result != super::ActionResult::Completed
        {
            continue;
        }
        let id = super::jobs::JobTicketId(result.key.target_key);
        let Some(ticket) = board.ticket(id).cloned() else {
            continue;
        };
        if ticket.kind != "aid.assess"
            || ticket.state != super::jobs::JobTicketState::Completed(result.claim)
        {
            continue;
        }
        // The ticket is this department's; take it off the board whatever we
        // decide, so a batch that vanished mid-walk cannot leave dead work.
        board.cancel(id);

        let domain = ticket.domain;
        let Some(batch) = intakes.in_intake(domain) else {
            continue;
        };
        if batch.state != AidState::AwaitingAssessment {
            continue;
        }
        let batch_id = batch.id;
        let claimed = batch.provenance.claimed_label.clone();

        let contraband = db
            .as_deref()
            .is_some_and(|db| claim_reads_as_contraband(db, claimed.as_deref()));
        let department = domain.department();
        let standing = shift
            .as_deref()
            .map_or(0, |shift| shift.standing(department));
        let condition = status
            .iter()
            .find(|(row, _)| row.0 == department)
            .map_or(DepartmentCondition::OnSchedule, |(_, public)| {
                public.condition
            });

        let verdict = assess_offer(claimed.as_deref(), standing, condition, contraband);
        // Only fails if someone else judged it between the checks above, which
        // the `AwaitingAssessment` guard already excludes.
        let _ = intakes.assess(batch_id, verdict);
    }
}

/// What a department thinks of the person who donated, once it has an answer.
///
/// Accepting is worth less than being caught out is worth losing: helping is
/// what a decent colleague does, while handing a department something they had
/// to quarantine is a thing they remember.
const ACCEPTED_STANDING: i32 = 2;
const REJECTED_STANDING: i32 = -1;
const QUARANTINED_STANDING: i32 = -6;

/// A quarantined donation is a Security matter, not merely a social one.
const QUARANTINED_SUSPICION: i32 = 3;

/// The standing paid out for one settled batch, and the suspicion it raises.
///
/// Split out from the system so the numbers can be reasoned about without an
/// `App`. `None` means this state is not an outcome anybody has observed yet.
fn payout_for(state: AidState) -> Option<(i32, i32)> {
    match state {
        AidState::Accepted => Some((ACCEPTED_STANDING, 0)),
        AidState::Rejected => Some((REJECTED_STANDING, 0)),
        AidState::Quarantined => Some((QUARANTINED_STANDING, QUARANTINED_SUSPICION)),
        // Still in motion, or already settled by an earlier state.
        AidState::AwaitingAssessment
        | AidState::Stored
        | AidState::ReservedForUse
        | AidState::Used
        | AidState::Returned
        | AidState::Disposed => None,
    }
}

/// Applies each settled batch's consequences to standing and suspicion, once.
///
/// Runs on the *state*, not on a message, because the interesting outcomes
/// arrive by several routes — a worker's verdict, an inspection clearing a
/// quarantine, a body reacting later — and every one of them ends in the batch
/// simply being in a new state. `claim_payout` is what makes it exactly once
/// however many of those routes fire.
fn pay_out_aid_outcomes(
    mut intakes: ResMut<AidIntakes>,
    mut shift: Option<ResMut<crate::orders::Shift>>,
    mut suspicion: Option<ResMut<crate::antagonist::SecuritySuspicion>>,
) {
    let settled: Vec<(AidBatchId, AidState, JobDomain)> = intakes
        .iter()
        .filter(|batch| !batch.consequences_applied && payout_for(batch.state).is_some())
        .map(|batch| (batch.id, batch.state, batch.provenance.domain))
        .collect();

    for (id, state, domain) in settled {
        let Some((standing, suspicion_delta)) = payout_for(state) else {
            continue;
        };
        if intakes.claim_payout(id).is_err() {
            continue;
        }
        if let Some(shift) = shift.as_deref_mut() {
            shift.adjust(domain.department(), standing);
        }
        if suspicion_delta != 0 {
            if let Some(suspicion) = suspicion.as_deref_mut() {
                crate::antagonist::nudge_suspicion(suspicion, suspicion_delta);
            }
        }
    }
}

/// How loudly a handoff registers to anyone nearby. Moderate on purpose: this
/// is somebody setting a container down on a counter, not a shout.
const HANDOFF_STRENGTH: f32 = 0.55;

/// Turns a completed handoff into something people can actually witness.
///
/// The kind is [`super::StimulusKind::SuspiciousHandling`] for *every* donation,
/// honest or not. Nobody watching a container change hands learns what is in
/// it — they learn that it happened. Separating the two would make the memory
/// itself a detector, which is precisely what
/// `a_witness_cannot_tell_an_honest_donation_from_a_covert_one` forbids.
fn witness_aid_offers(
    mut offers: MessageReader<AidOffered>,
    mut stimuli: MessageWriter<super::Stimulus>,
) {
    for offer in offers.read() {
        stimuli.write(
            super::Stimulus::new(super::StimulusKind::SuspiciousHandling, offer.at)
                .by(offer.contributor)
                .with_strength(HANDOFF_STRENGTH),
        );
    }
}

fn reset_aid(mut intakes: ResMut<AidIntakes>) {
    *intakes = AidIntakes::default();
}

/// Publishes each department's intake state for the Crew menu.
fn publish_aid_status(
    intakes: Res<AidIntakes>,
    mut rows: Query<(&super::public::DepartmentStatusRow, &mut PublicAidStatus)>,
) {
    for (row, mut status) in &mut rows {
        let domain = JobDomain::from_department(row.0);
        let batch = intakes.in_intake(domain);
        let next = PublicAidStatus {
            department: row.0,
            state: batch.map(|batch| batch.state),
            claimed_label: batch.and_then(|batch| batch.provenance.claimed_label.clone()),
        };
        if *status != next {
            *status = next;
        }
    }
}

pub(super) fn register(app: &mut App) {
    // Carries only a department, so there is no `Entity` for `MapEntities` to
    // translate — the same reasoning as `AnswerApproachRequested`.
    app.add_client_message::<OfferAidRequested>(Channel::Ordered)
        .add_message::<AidOffered>()
        .init_resource::<AidIntakes>()
        .replicate::<PublicAidStatus>()
        .add_systems(
            OnEnter(crate::AppState::Playing),
            reset_aid
                .in_set(super::UtilityResetSet)
                .run_if(crate::net::is_authority),
        )
        .add_systems(
            Update,
            (
                handle_aid_offers,
                witness_aid_offers,
                apply_assessment_results,
                pay_out_aid_outcomes,
                publish_assessment_tickets,
            )
                .chain()
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing)),
        )
        .add_systems(
            Update,
            publish_aid_status
                .in_set(super::UtilityAiSet::Publish)
                .run_if(crate::net::is_authority)
                .run_if(in_state(crate::AppState::Playing)),
        );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provenance(label: Option<&str>) -> AidProvenance {
        AidProvenance {
            contributor: Entity::from_bits(1),
            claimed_label: label.map(str::to_string),
            offered_at: 0.0,
            domain: JobDomain::Medical,
        }
    }

    fn solution(units: i32) -> chem_sim::Solution {
        let mut solution = chem_sim::Solution::unbounded();
        let _ = solution.add(chem_sim::ReagentId(1), chem_sim::Units::whole(units));
        solution
    }

    fn offered() -> (AidIntakes, AidBatchId) {
        let mut intakes = AidIntakes::default();
        let id = intakes
            .offer(JobDomain::Medical, provenance(Some("saline")), solution(10))
            .unwrap();
        (intakes, id)
    }

    /// Every `utility_spot` the shipped map authors, as `(id, x, y, z, capacity)`
    /// in raw map units.
    ///
    /// Read straight out of `lab.map` rather than through the asset pipeline:
    /// the pipeline needs a running App with a loaded scene, and what these
    /// tests are actually guarding is the *authoring*, which is text.
    fn authored_utility_spots() -> Vec<(String, [f32; 3], usize)> {
        let map = include_str!("../../assets/maps/lab.map");
        let mut spots = Vec::new();
        for block in map.split('{') {
            if !block.contains("\"classname\" \"utility_spot\"") {
                continue;
            }
            let field = |key: &str| -> Option<&str> {
                block
                    .lines()
                    .find_map(|line| line.trim().strip_prefix(&format!("\"{key}\" \"")))
                    .and_then(|rest| rest.strip_suffix('"'))
            };
            let (Some(id), Some(origin), Some(capacity)) =
                (field("id"), field("origin"), field("capacity"))
            else {
                continue;
            };
            let coords: Vec<f32> = origin
                .split_whitespace()
                .filter_map(|part| part.parse().ok())
                .collect();
            let [x, y, z] = coords[..] else { continue };
            spots.push((
                id.to_string(),
                [x, y, z],
                capacity.parse().expect("authored capacity is a number"),
            ));
        }
        spots
    }

    #[test]
    fn every_department_has_an_authored_intake_on_the_real_map() {
        // The offer handler refuses a department whose spot is missing, so a
        // typo in either the map or `intake_spot` disables aid for that
        // department in complete silence. Checking the shipped map rather than
        // a fixture is the only thing that surfaces it.
        //
        // Placement is *not* checked here:
        // `lab::tb_map::utility_spots_are_unique_walkable_and_routable` already
        // proves every authored spot stands on walkable floor and can route to
        // the counter, which is a stronger check than anything this module
        // could restate. This test only proves the two names agree.
        let spots = authored_utility_spots();
        for domain in JobDomain::ALL {
            let id = intake_spot(domain);
            let (_, _, capacity) = spots
                .iter()
                .find(|(spot, _, _)| spot == id)
                .unwrap_or_else(|| panic!("{domain:?} has no authored '{id}' in lab.map"));
            assert_eq!(*capacity, 1, "{id} must be a single counter surface");
        }
    }

    #[test]
    fn no_two_departments_share_one_intake_counter() {
        // Pointing two departments at one origin would let a donation to
        // Cargo be assessed by Medical, and would make the single-slot guard
        // meaningless because both slots are physically the same surface.
        let spots = authored_utility_spots();
        let mut origins = Vec::new();
        for domain in JobDomain::ALL {
            let id = intake_spot(domain);
            let (_, at, _) = spots
                .iter()
                .find(|(spot, _, _)| spot == id)
                .expect("checked above");
            assert!(
                !origins.contains(at),
                "{domain:?}'s intake reuses another department's counter at {at:?}",
            );
            origins.push(*at);
        }
    }

    #[test]
    fn a_labelled_offer_from_anyone_is_accepted_on_a_quiet_shift() {
        assert_eq!(
            assess_offer(Some("saline"), 0, DepartmentCondition::OnSchedule, false),
            Ok(()),
        );
    }

    #[test]
    fn an_unmarked_container_from_a_stranger_is_refused() {
        assert_eq!(
            assess_offer(None, 0, DepartmentCondition::OnSchedule, false),
            Err(AidRefusal::Untrusted),
        );
    }

    #[test]
    fn standing_buys_the_benefit_of_the_doubt_on_an_unmarked_container() {
        // Trust changes what staff *believe*. It is checked separately, in
        // `trust_never_changes_what_is_actually_in_the_container`, that it
        // changes nothing about the chemistry.
        assert_eq!(
            assess_offer(None, 40, DepartmentCondition::OnSchedule, false),
            Ok(()),
        );
    }

    #[test]
    fn a_strained_department_accepts_what_a_calm_one_would_refuse() {
        // The lever the entire system turns on. Same person, same unmarked
        // container, different day.
        let calm = assess_offer(None, 0, DepartmentCondition::Busy, false);
        let swamped = assess_offer(None, 0, DepartmentCondition::Strained, false);
        assert_eq!(calm, Err(AidRefusal::Untrusted));
        assert_eq!(swamped, Ok(()));
    }

    #[test]
    fn a_contraband_claim_is_refused_however_trusted_the_donor_and_however_desperate() {
        // No amount of standing or pressure legitimizes it — the plan is
        // explicit that contraband does not become legal because it was
        // donated.
        for standing in [-40, 0, 40] {
            for condition in [
                DepartmentCondition::OnSchedule,
                DepartmentCondition::Strained,
                DepartmentCondition::Emergency,
            ] {
                assert_eq!(
                    assess_offer(Some("meth"), standing, condition, true),
                    Err(AidRefusal::Contraband),
                    "standing {standing}, {condition:?}",
                );
            }
        }
    }

    #[test]
    fn trust_never_changes_what_is_actually_in_the_container() {
        // Two identical poisoned batches, one from a stranger and one from a
        // friend. The friend's is accepted and the stranger's refused — and
        // both still hold exactly what was poured in.
        let mut trusted = AidIntakes::default();
        let a = trusted
            .offer(JobDomain::Medical, provenance(None), solution(10))
            .unwrap();
        trusted
            .assess(
                a,
                assess_offer(None, 40, DepartmentCondition::OnSchedule, false),
            )
            .unwrap();

        let mut stranger = AidIntakes::default();
        let b = stranger
            .offer(JobDomain::Medical, provenance(None), solution(10))
            .unwrap();
        stranger
            .assess(
                b,
                assess_offer(None, 0, DepartmentCondition::OnSchedule, false),
            )
            .unwrap();

        assert_eq!(trusted.get(a).unwrap().state, AidState::Accepted);
        assert_eq!(stranger.get(b).unwrap().state, AidState::Rejected);
        assert_eq!(
            trusted
                .get(a)
                .unwrap()
                .contents
                .volume_of(chem_sim::ReagentId(1)),
            stranger
                .get(b)
                .unwrap()
                .contents
                .volume_of(chem_sim::ReagentId(1)),
            "the verdict changed, the chemistry did not",
        );
    }

    #[test]
    fn every_department_can_actually_judge_its_own_donations() {
        // The ticket requires this capability. A department whose roster
        // lacked it would publish work nobody in the station is qualified to
        // claim, and the batch would sit in the intake forever.
        for domain in JobDomain::ALL {
            let roster = super::super::roster_of(domain);
            let capability = assessment_capability(domain);
            assert!(
                roster.capabilities.contains(&capability),
                "{domain:?}'s roster cannot claim its own '{capability}' ticket",
            );
        }
    }

    fn payout_app() -> App {
        let mut app = App::new();
        app.init_resource::<AidIntakes>()
            .init_resource::<crate::orders::Shift>()
            .init_resource::<crate::antagonist::SecuritySuspicion>()
            .add_systems(Update, pay_out_aid_outcomes);
        app
    }

    fn settle(app: &mut App, verdict: Result<(), AidRefusal>) -> AidBatchId {
        let mut intakes = app.world_mut().resource_mut::<AidIntakes>();
        let id = intakes
            .offer(JobDomain::Medical, provenance(Some("saline")), solution(10))
            .unwrap();
        intakes.assess(id, verdict).unwrap();
        app.update();
        id
    }

    fn medical_standing(app: &App) -> i32 {
        app.world()
            .resource::<crate::orders::Shift>()
            .standing(crate::orders::Department::Medical)
    }

    #[test]
    fn a_helpful_donation_earns_standing_with_the_department_that_took_it() {
        let mut app = payout_app();
        settle(&mut app, Ok(()));
        assert_eq!(medical_standing(&app), ACCEPTED_STANDING);
    }

    #[test]
    fn a_quarantined_donation_costs_standing_and_draws_security_attention() {
        let mut app = payout_app();
        settle(&mut app, Err(AidRefusal::Contraband));
        assert_eq!(medical_standing(&app), QUARANTINED_STANDING);
        assert_eq!(
            app.world()
                .resource::<crate::antagonist::SecuritySuspicion>()
                .level(),
            QUARANTINED_SUSPICION,
        );
    }

    #[test]
    fn an_ordinary_rejection_is_a_smaller_matter_than_a_quarantine() {
        // Being turned down is awkward; being quarantined means Security has
        // your name. These must not be the same number.
        let mut refused = payout_app();
        settle(&mut refused, Err(AidRefusal::Untrusted));
        let mut held = payout_app();
        settle(&mut held, Err(AidRefusal::Contraband));

        assert!(medical_standing(&refused) > medical_standing(&held));
        assert_eq!(
            refused
                .world()
                .resource::<crate::antagonist::SecuritySuspicion>()
                .level(),
            0,
            "an ordinary refusal is not a Security matter",
        );
    }

    #[test]
    fn a_settled_batch_pays_out_exactly_once_however_many_frames_pass() {
        // The guard that matters. `pay_out_aid_outcomes` runs every frame and
        // reads state rather than a message, so without `claim_payout` a
        // single donation would pay standing forever.
        let mut app = payout_app();
        settle(&mut app, Ok(()));
        let after_first = medical_standing(&app);
        for _ in 0..30 {
            app.update();
        }
        assert_eq!(medical_standing(&app), after_first);
    }

    #[test]
    fn a_batch_still_being_handled_has_not_paid_out_yet() {
        // Standing is for outcomes, not for handing something over. A batch
        // sitting unassessed must move nothing.
        let mut app = payout_app();
        let id = app
            .world_mut()
            .resource_mut::<AidIntakes>()
            .offer(JobDomain::Medical, provenance(Some("saline")), solution(10))
            .unwrap();
        for _ in 0..5 {
            app.update();
        }
        assert_eq!(medical_standing(&app), 0);
        assert!(
            !app.world()
                .resource::<AidIntakes>()
                .get(id)
                .unwrap()
                .consequences_applied
        );
    }

    #[test]
    fn a_cleared_quarantine_does_not_refund_the_standing_it_cost() {
        // Inspection can clear a hold, but the department still remembers
        // being handed something they had to lock up. A second payout here
        // would let a player farm standing by donating contraband and then
        // waiting for it to be cleared.
        let mut app = payout_app();
        let id = settle(&mut app, Err(AidRefusal::Contraband));
        let after_quarantine = medical_standing(&app);

        app.world_mut()
            .resource_mut::<AidIntakes>()
            .advance(id, AidState::Accepted)
            .unwrap();
        app.update();
        assert_eq!(medical_standing(&app), after_quarantine);
    }

    /// The whole chain in one app: offer → ticket → claim → resolve → verdict
    /// → payout.
    ///
    /// Every other test in this module drives one seam. This one wires the
    /// systems in the order `register` does and lets a donation travel the
    /// full distance, because the seams passing individually does not prove
    /// the chain connects — a mismatched `target_key`, a ticket kind typo, or
    /// a system-order slip would leave every unit test green and the feature
    /// dead in the built game.
    fn end_to_end_app() -> App {
        let mut app = App::new();
        let mut spots = super::super::jobs::UtilitySpots::default();
        spots.insert(intake_spot(JobDomain::Medical), SPOT, 1);
        app.init_resource::<Time>()
            .init_resource::<AidIntakes>()
            .init_resource::<super::super::jobs::JobBoard>()
            .init_resource::<crate::orders::Shift>()
            .init_resource::<crate::antagonist::SecuritySuspicion>()
            .insert_resource(spots)
            .add_message::<FromClient<OfferAidRequested>>()
            .add_message::<AidOffered>()
            .add_message::<super::super::Stimulus>()
            .add_message::<super::super::UtilityActionResolved>()
            .add_systems(
                Update,
                (
                    handle_aid_offers,
                    witness_aid_offers,
                    apply_assessment_results,
                    pay_out_aid_outcomes,
                    publish_assessment_tickets,
                )
                    .chain(),
            );
        app
    }

    /// Stands in for the utility kernel finishing the job: the worker claimed
    /// the ticket, walked there, and performed it. Uses the same `target_key`
    /// the real `PerformJob` action carries — the ticket id — so a divergence
    /// between the two shows up here.
    fn worker_finishes_assessment(app: &mut App, worker: Entity) {
        let id = assess_ticket_id(JobDomain::Medical);
        let claim = super::super::ReservationOwner {
            agent: worker,
            action_instance: 1,
        };
        app.world_mut()
            .resource_mut::<super::super::jobs::JobBoard>()
            .claim(id, claim)
            .expect("the ticket was published and available");
        app.world_mut()
            .resource_mut::<super::super::jobs::JobBoard>()
            .resolve(id, claim, super::super::ActionResult::Completed)
            .expect("the worker completed it");
        app.world_mut()
            .write_message(super::super::UtilityActionResolved {
                agent: worker,
                key: super::super::ActionKey {
                    action: super::super::UtilityActionId::PerformJob,
                    target_key: id.0,
                },
                claim,
                result: super::super::ActionResult::Completed,
            });
        app.update();
    }

    #[test]
    fn a_real_medical_worker_is_eligible_for_the_assessment_ticket() {
        // The link my end-to-end harness fakes. The kernel turns a routine
        // ticket into a candidate purely through `available_to` — domain plus
        // capability — so if the roster, the published ticket, and the
        // capability constant ever disagree, no candidate is generated, no
        // warning is printed, and the batch sits in the intake forever.
        let mut app = end_to_end_app();
        spawn_player(&mut app, SPOT);
        request_offer(&mut app);

        let board = app.world().resource::<super::super::jobs::JobBoard>();
        let ticket = board
            .ticket(assess_ticket_id(JobDomain::Medical))
            .expect("published");

        let roster = super::super::roster_of(JobDomain::Medical);
        let nurse = roster
            .profile_for(roster.support[0])
            .expect("a support worker has a profile");
        assert!(
            ticket.available_to(&nurse),
            "Medical support cannot take Medical's own assessment ticket",
        );
        assert_eq!(board.available_for(&nurse).count(), 1);

        // And it is genuinely department-scoped: Cargo cannot wander in.
        let cargo = super::super::roster_of(JobDomain::Cargo);
        let hauler = cargo
            .profile_for(cargo.support[0])
            .expect("a Cargo worker has a profile");
        assert!(!ticket.available_to(&hauler));
    }

    #[test]
    fn an_assessment_ticket_needs_no_witness_so_it_is_never_perception_gated() {
        // Routine candidates pass through `may_respond_to(memory, subject)`. A
        // ticket carrying a `subject` would be invisible to anyone who had not
        // seen that body — correct for a casualty, fatal for a beaker sitting
        // in plain sight on a counter.
        let mut app = end_to_end_app();
        spawn_player(&mut app, SPOT);
        request_offer(&mut app);
        let ticket = app
            .world()
            .resource::<super::super::jobs::JobBoard>()
            .ticket(assess_ticket_id(JobDomain::Medical))
            .cloned()
            .expect("published");
        assert_eq!(ticket.subject, None);
        assert!(super::super::perception::may_respond_to(
            None,
            ticket.subject,
            0.0
        ));
    }

    #[test]
    fn a_labelled_donation_travels_the_whole_chain_and_earns_standing() {
        let mut app = end_to_end_app();
        let (_, container) = spawn_player(&mut app, SPOT);
        app.world_mut()
            .entity_mut(container)
            .insert(crate::labels::Label("saline".into()));
        let worker = app.world_mut().spawn_empty().id();

        request_offer(&mut app);
        // The ticket exists and names a capability a real Medical worker holds.
        let ticket = app
            .world()
            .resource::<super::super::jobs::JobBoard>()
            .ticket(assess_ticket_id(JobDomain::Medical))
            .cloned()
            .expect("the offer published assessment work");
        assert!(super::super::roster_of(JobDomain::Medical)
            .capabilities
            .contains(&ticket.required_capability.0.as_str()));

        worker_finishes_assessment(&mut app, worker);

        let intakes = app.world().resource::<AidIntakes>();
        let batch = intakes.in_intake(JobDomain::Medical).expect("still there");
        assert_eq!(batch.state, AidState::Accepted);
        assert!(batch.consequences_applied, "the outcome was paid out");
        assert_eq!(
            app.world()
                .resource::<crate::orders::Shift>()
                .standing(crate::orders::Department::Medical),
            ACCEPTED_STANDING,
        );
        // And the work is off the board rather than left for a second worker.
        assert!(app
            .world()
            .resource::<super::super::jobs::JobBoard>()
            .ticket(assess_ticket_id(JobDomain::Medical))
            .is_none());
    }

    #[test]
    fn an_unmarked_donation_to_a_calm_department_is_refused_end_to_end() {
        // Same chain, opposite verdict, driven only by what the department
        // could see: no label, no standing, no pressure.
        let mut app = end_to_end_app();
        spawn_player(&mut app, SPOT);
        let worker = app.world_mut().spawn_empty().id();
        request_offer(&mut app);
        worker_finishes_assessment(&mut app, worker);

        let intakes = app.world().resource::<AidIntakes>();
        let batch = intakes.in_intake(JobDomain::Medical).unwrap();
        assert_eq!(batch.state, AidState::Rejected);
        assert_eq!(batch.refusal, Some(AidRefusal::Untrusted));
        assert_eq!(
            app.world()
                .resource::<crate::orders::Shift>()
                .standing(crate::orders::Department::Medical),
            REJECTED_STANDING,
        );
    }

    #[test]
    fn a_contraband_claim_travels_the_chain_into_quarantine_and_security_notices() {
        let mut app = end_to_end_app();
        let (_, container) = spawn_player(&mut app, SPOT);
        // No ChemDb in this fixture, so drive the same path a known-contraband
        // claim takes by naming it in the verdict directly. The claim-matching
        // itself is covered by `a_contraband_claim_is_refused_however_...`.
        app.world_mut()
            .entity_mut(container)
            .insert(crate::labels::Label("saline".into()));
        request_offer(&mut app);
        let id = app
            .world()
            .resource::<AidIntakes>()
            .in_intake(JobDomain::Medical)
            .unwrap()
            .id;
        app.world_mut()
            .resource_mut::<AidIntakes>()
            .assess(id, Err(AidRefusal::Contraband))
            .unwrap();
        app.update();

        assert_eq!(
            app.world().resource::<AidIntakes>().get(id).unwrap().state,
            AidState::Quarantined,
        );
        assert_eq!(
            app.world()
                .resource::<crate::antagonist::SecuritySuspicion>()
                .level(),
            QUARANTINED_SUSPICION,
        );
    }

    #[test]
    fn a_second_donation_becomes_possible_once_the_first_is_stored_away() {
        // The loop has to close, or aid is a one-shot per shift. Offer,
        // assess, store, clear, offer again — all through the real systems.
        let mut app = end_to_end_app();
        let (player, container) = spawn_player(&mut app, SPOT);
        // Labelled, so the department accepts it — an unmarked container from
        // a stranger is rejected, and a rejected batch is returned or disposed
        // of rather than stored.
        app.world_mut()
            .entity_mut(container)
            .insert(crate::labels::Label("saline".into()));
        let worker = app.world_mut().spawn_empty().id();
        request_offer(&mut app);
        worker_finishes_assessment(&mut app, worker);

        let id = app
            .world()
            .resource::<AidIntakes>()
            .in_intake(JobDomain::Medical)
            .unwrap()
            .id;
        {
            let mut intakes = app.world_mut().resource_mut::<AidIntakes>();
            intakes.advance(id, AidState::Stored).unwrap();
            assert_eq!(intakes.clear_intake(JobDomain::Medical), Some(id));
        }

        // A fresh container in the same hand.
        let mut container = crate::containers::Container {
            kind: crate::containers::ContainerKind::Beaker,
            solution: chem_sim::Solution::unbounded(),
        };
        let _ = container
            .solution
            .add(chem_sim::ReagentId(1), chem_sim::Units::whole(5));
        app.world_mut()
            .spawn((container, crate::containers::HeldBy(player)));
        request_offer(&mut app);

        let intakes = app.world().resource::<AidIntakes>();
        assert_eq!(intakes.len(), 2, "the intake took a second donation");
        assert_eq!(
            intakes.in_intake(JobDomain::Medical).unwrap().state,
            AidState::AwaitingAssessment,
        );
        // And the second batch published its own assessment work.
        assert!(app
            .world()
            .resource::<super::super::jobs::JobBoard>()
            .ticket(assess_ticket_id(JobDomain::Medical))
            .is_some());
    }

    #[test]
    fn a_donation_survives_a_save_round_trip_with_its_real_contents() {
        let (mut intakes, id) = offered();
        intakes.assess(id, Ok(())).unwrap();

        let records = intakes.snapshot();
        let text = ron::ser::to_string(&records).unwrap();
        let decoded: Vec<AidRecord> = ron::from_str(&text).unwrap();

        let mut restored = AidIntakes::default();
        restored.restore(&decoded, Entity::PLACEHOLDER);
        let batch = restored
            .in_intake(JobDomain::Medical)
            .expect("the department still holds it");
        assert_eq!(batch.state, AidState::Accepted);
        assert_eq!(batch.provenance.claimed_label.as_deref(), Some("saline"));
        assert_eq!(
            batch.contents.volume_of(chem_sim::ReagentId(1)),
            chem_sim::Units::whole(10),
            "the real contents crossed the save, not just the claim",
        );
    }

    #[test]
    fn a_finished_donation_is_not_resurrected_by_a_reload() {
        // A used batch is gone. Writing it would put a container back on a
        // counter the department has already cleared, and — worse — hand a
        // second dose to whatever consumes it next.
        let (mut intakes, id) = offered();
        intakes.assess(id, Ok(())).unwrap();
        intakes.advance(id, AidState::Stored).unwrap();
        intakes.advance(id, AidState::ReservedForUse).unwrap();
        intakes.advance(id, AidState::Used).unwrap();
        assert!(intakes.snapshot().is_empty());
    }

    #[test]
    fn a_reload_does_not_re_pay_a_donation_that_already_settled() {
        // `consequences_applied` has to cross the save. Without it every
        // reload would pay standing again for the same accepted batch, and
        // closing the game would become a way to farm it.
        let (mut intakes, id) = offered();
        intakes.assess(id, Ok(())).unwrap();
        intakes.claim_payout(id).unwrap();

        let mut restored = AidIntakes::default();
        restored.restore(&intakes.snapshot(), Entity::PLACEHOLDER);
        let batch = restored.in_intake(JobDomain::Medical).unwrap();
        assert!(batch.consequences_applied);
        assert_eq!(restored.claim_payout(batch.id), Err(AidError::AlreadyPaid));
    }

    #[test]
    fn loading_a_save_replaces_the_live_state_rather_than_merging_into_it() {
        // `restore` clears first — the same trap `IllicitCustody::restore`
        // avoids. Loading into a session that already has donations must leave
        // the station holding exactly what the save described, not the union.
        // A merge would let a player load an old save to keep a batch the save
        // says they never gave.
        let mut live = AidIntakes::default();
        let mut cargo = provenance(None);
        cargo.domain = JobDomain::Cargo;
        live.offer(JobDomain::Cargo, cargo, solution(9)).unwrap();
        live.offer(JobDomain::Medical, provenance(Some("saline")), solution(4))
            .unwrap();
        assert_eq!(live.len(), 2);

        // The save knows only about Medical's.
        let saved = {
            let (mut other, _) = offered();
            let records = other.snapshot();
            other.clear_intake(JobDomain::Medical);
            records
        };
        live.restore(&saved, Entity::PLACEHOLDER);

        assert_eq!(live.len(), 1, "the Cargo batch was not in the save");
        assert!(live.in_intake(JobDomain::Cargo).is_none());
        assert!(live.in_intake(JobDomain::Medical).is_some());
    }

    #[test]
    fn loading_twice_leaves_one_donation_per_counter() {
        let (intakes, _) = offered();
        let records = intakes.snapshot();

        let mut restored = AidIntakes::default();
        restored.restore(&records, Entity::PLACEHOLDER);
        restored.restore(&records, Entity::PLACEHOLDER);
        assert_eq!(restored.len(), 1);
        assert!(restored.in_intake(JobDomain::Medical).is_some());
    }

    #[test]
    fn a_save_naming_one_department_twice_still_leaves_one_counter() {
        // The intake is one physical surface. A corrupt or hand-edited save
        // describing two batches for one department must not produce a station
        // that cannot exist.
        let record = AidRecord {
            domain: JobDomain::Medical,
            contents: solution(5),
            state: AidState::Accepted,
            claimed_label: None,
            refusal: None,
            offered_at: 0.0,
            consequences_applied: false,
        };
        let mut intakes = AidIntakes::default();
        intakes.restore(&[record.clone(), record], Entity::PLACEHOLDER);
        assert_eq!(intakes.len(), 1);
    }

    #[test]
    fn a_save_written_before_donations_existed_still_loads() {
        // Every `ProgressSave` field is `#[serde(default)]` so that adding one
        // never costs a player their career. This is that guard for the aid
        // list specifically: an older save has no `department_aid` key at all.
        let records: Vec<AidRecord> = ron::from_str("[]").expect("an empty list parses");
        assert!(records.is_empty());
        let mut intakes = AidIntakes::default();
        assert_eq!(intakes.restore(&records, Entity::PLACEHOLDER), 0);
        assert!(intakes.is_empty());
    }

    #[test]
    fn a_saved_donation_is_written_in_a_stable_order() {
        // `HashMap` iteration is unspecified, so an unsorted snapshot would
        // rewrite the save file on every tick with the same content shuffled —
        // defeating `PersistedProgress`'s change check and churning the disk.
        let mut intakes = AidIntakes::default();
        for (index, domain) in [JobDomain::Service, JobDomain::Cargo, JobDomain::Medical]
            .into_iter()
            .enumerate()
        {
            let mut provenance = provenance(Some("saline"));
            provenance.domain = domain;
            provenance.offered_at = index as f32;
            intakes.offer(domain, provenance, solution(5)).unwrap();
        }
        let first = intakes.snapshot();
        for _ in 0..8 {
            assert_eq!(intakes.snapshot(), first);
        }
        assert_eq!(
            first.iter().map(|r| r.domain).collect::<Vec<_>>(),
            vec![JobDomain::Service, JobDomain::Cargo, JobDomain::Medical],
        );
    }

    #[test]
    fn an_empty_container_is_not_an_offer() {
        let mut intakes = AidIntakes::default();
        assert_eq!(
            intakes.offer(
                JobDomain::Medical,
                provenance(None),
                chem_sim::Solution::unbounded()
            ),
            Err(AidError::Empty),
        );
        assert!(intakes.is_empty());
    }

    #[test]
    fn one_intake_holds_one_batch_so_a_same_frame_double_offer_is_refused() {
        // Two interaction handlers racing on one frame. The second must not
        // land, or one container has been donated twice.
        let (mut intakes, _) = offered();
        assert_eq!(
            intakes.offer(JobDomain::Medical, provenance(None), solution(5)),
            Err(AidError::IntakeOccupied),
        );
        assert_eq!(intakes.len(), 1);

        // A different department is a different physical surface.
        assert!(intakes
            .offer(JobDomain::Cargo, provenance(None), solution(5))
            .is_ok());
    }

    #[test]
    fn offering_never_assumes_success() {
        let (intakes, id) = offered();
        let batch = intakes.get(id).unwrap();
        assert_eq!(batch.state, AidState::AwaitingAssessment);
        assert!(!batch.consequences_applied);
        assert!(batch.refusal.is_none());
    }

    #[test]
    fn the_claim_and_the_contents_are_never_reconciled() {
        // The heart of the design: a player labels poison as saline and the
        // batch keeps both facts, unmodified, forever.
        let mut intakes = AidIntakes::default();
        let id = intakes
            .offer(JobDomain::Medical, provenance(Some("saline")), solution(10))
            .unwrap();
        intakes.assess(id, Ok(())).unwrap();
        let batch = intakes.get(id).unwrap();
        assert_eq!(batch.provenance.claimed_label.as_deref(), Some("saline"));
        assert_eq!(
            batch.contents.volume_of(chem_sim::ReagentId(1)),
            chem_sim::Units::whole(10),
            "accepting a claim must not alter the contents",
        );
    }

    #[test]
    fn a_second_worker_cannot_overturn_the_first_verdict() {
        let (mut intakes, id) = offered();
        assert_eq!(intakes.assess(id, Ok(())), Ok(AidState::Accepted));
        assert_eq!(
            intakes.assess(id, Err(AidRefusal::Contraband)),
            Err(AidError::NotInState),
        );
        assert_eq!(intakes.get(id).unwrap().state, AidState::Accepted);
    }

    #[test]
    fn contraband_is_quarantined_rather_than_handed_back() {
        // Returning it would put it straight back in the player's pocket with
        // no record, which is a strictly better outcome than never offering.
        let (mut intakes, id) = offered();
        assert_eq!(
            intakes.assess(id, Err(AidRefusal::Contraband)),
            Ok(AidState::Quarantined),
        );
        assert_eq!(
            intakes.get(id).unwrap().refusal,
            Some(AidRefusal::Contraband)
        );
        assert_eq!(
            intakes.advance(id, AidState::Returned),
            Err(AidError::NotInState)
        );
        assert!(intakes.advance(id, AidState::Disposed).is_ok());
    }

    #[test]
    fn a_quarantine_can_be_cleared_by_inspection() {
        let (mut intakes, id) = offered();
        intakes.assess(id, Err(AidRefusal::Contraband)).unwrap();
        assert!(intakes.advance(id, AidState::Accepted).is_ok());
    }

    #[test]
    fn the_lifecycle_cannot_be_skipped() {
        let (mut intakes, id) = offered();
        // Straight from awaiting to used would let a batch be consumed with no
        // assessment at all.
        assert_eq!(
            intakes.advance(id, AidState::Used),
            Err(AidError::NotInState)
        );
        intakes.assess(id, Ok(())).unwrap();
        assert_eq!(
            intakes.advance(id, AidState::Used),
            Err(AidError::NotInState)
        );
        intakes.advance(id, AidState::Stored).unwrap();
        intakes.advance(id, AidState::ReservedForUse).unwrap();
        intakes.advance(id, AidState::Used).unwrap();
        assert_eq!(
            intakes.advance(id, AidState::Stored),
            Err(AidError::AlreadyFinished),
        );
    }

    #[test]
    fn one_donation_cannot_treat_two_patients() {
        let (mut intakes, id) = offered();
        intakes.assess(id, Ok(())).unwrap();
        intakes.advance(id, AidState::Stored).unwrap();
        intakes.advance(id, AidState::ReservedForUse).unwrap();

        let first = intakes.take_contents(id).unwrap();
        assert_eq!(
            first.volume_of(chem_sim::ReagentId(1)),
            chem_sim::Units::whole(10),
        );
        let second = intakes.take_contents(id).unwrap();
        assert_eq!(
            second.total_volume(),
            chem_sim::Units::ZERO,
            "the batch was already drained",
        );
    }

    #[test]
    fn contents_cannot_be_taken_before_the_batch_is_reserved() {
        let (mut intakes, id) = offered();
        assert_eq!(intakes.take_contents(id), Err(AidError::NotInState));
        intakes.assess(id, Ok(())).unwrap();
        assert_eq!(intakes.take_contents(id), Err(AidError::NotInState));
    }

    #[test]
    fn consequences_are_paid_exactly_once() {
        let (mut intakes, id) = offered();
        assert_eq!(intakes.claim_payout(id), Ok(()));
        assert_eq!(intakes.claim_payout(id), Err(AidError::AlreadyPaid));
    }

    const SPOT: Vec3 = Vec3::new(4.0, 0.0, 4.0);

    fn offer_app() -> App {
        let mut app = App::new();
        let mut spots = super::super::jobs::UtilitySpots::default();
        spots.insert(intake_spot(JobDomain::Medical), SPOT, 1);
        app.init_resource::<Time>()
            .init_resource::<AidIntakes>()
            .insert_resource(spots)
            .add_message::<FromClient<OfferAidRequested>>()
            .add_message::<AidOffered>()
            .add_systems(Update, handle_aid_offers);
        app
    }

    fn spawn_player(app: &mut App, at: Vec3) -> (Entity, Entity) {
        let player = app
            .world_mut()
            .spawn((
                crate::player::Chemist {
                    client: ClientId::Server,
                },
                Transform::from_translation(at),
            ))
            .id();
        let mut container = crate::containers::Container {
            kind: crate::containers::ContainerKind::Beaker,
            solution: chem_sim::Solution::unbounded(),
        };
        let _ = container
            .solution
            .add(chem_sim::ReagentId(1), chem_sim::Units::whole(20));
        let held = app
            .world_mut()
            .spawn((container, crate::containers::HeldBy(player)))
            .id();
        (player, held)
    }

    fn request_offer(app: &mut App) {
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: OfferAidRequested {
                department: crate::orders::Department::Medical,
            },
        });
        app.update();
    }

    #[test]
    fn a_valid_offer_moves_the_real_container_into_the_intake() {
        let mut app = offer_app();
        let (_, container) = spawn_player(&mut app, SPOT);
        request_offer(&mut app);

        let intakes = app.world().resource::<AidIntakes>();
        let batch = intakes.in_intake(JobDomain::Medical).expect("it landed");
        assert_eq!(batch.state, AidState::AwaitingAssessment);
        assert_eq!(
            batch.contents.volume_of(chem_sim::ReagentId(1)),
            chem_sim::Units::whole(20),
            "the real contents arrived, not a copy of the claim",
        );
        assert!(
            app.world().get_entity(container).is_err(),
            "the container left the hand exactly once",
        );
    }

    #[test]
    fn a_player_cannot_donate_from_across_the_station() {
        let mut app = offer_app();
        spawn_player(&mut app, SPOT + Vec3::new(40.0, 0.0, 0.0));
        request_offer(&mut app);
        assert!(app
            .world()
            .resource::<AidIntakes>()
            .in_intake(JobDomain::Medical)
            .is_none());
    }

    #[test]
    fn an_empty_handed_player_donates_nothing() {
        let mut app = offer_app();
        app.world_mut().spawn((
            crate::player::Chemist {
                client: ClientId::Server,
            },
            Transform::from_translation(SPOT),
        ));
        request_offer(&mut app);
        assert!(app.world().resource::<AidIntakes>().is_empty());
    }

    #[test]
    fn a_department_with_no_authored_intake_cannot_receive_aid() {
        // Botany has no spot inserted in this fixture. It must refuse rather
        // than fall back to some other department's counter.
        let mut app = offer_app();
        spawn_player(&mut app, SPOT);
        app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: OfferAidRequested {
                department: crate::orders::Department::Botany,
            },
        });
        app.update();
        assert!(app.world().resource::<AidIntakes>().is_empty());
    }

    #[test]
    fn an_aid_offer_is_not_a_delivery() {
        // The separation the plan insists on. `OfferAidRequested` is its own
        // message; nothing in this app handles `InteractRequested`, so if aid
        // ever gets folded back onto the interact press this fixture stops
        // representing the real path and the offer below stops landing.
        let mut app = offer_app();
        spawn_player(&mut app, SPOT);
        app.world_mut()
            .write_message(crate::interaction::InteractRequested {
                target: Entity::from_bits(1),
            });
        app.update();
        assert!(
            app.world().resource::<AidIntakes>().is_empty(),
            "an interact press must not donate anything",
        );

        request_offer(&mut app);
        assert!(app
            .world()
            .resource::<AidIntakes>()
            .in_intake(JobDomain::Medical)
            .is_some());
    }

    #[test]
    fn the_written_label_is_carried_as_a_claim_alongside_the_real_contents() {
        let mut app = offer_app();
        let (_, container) = spawn_player(&mut app, SPOT);
        app.world_mut()
            .entity_mut(container)
            .insert(crate::labels::Label("saline".into()));
        request_offer(&mut app);

        let intakes = app.world().resource::<AidIntakes>();
        let batch = intakes.in_intake(JobDomain::Medical).unwrap();
        assert_eq!(batch.provenance.claimed_label.as_deref(), Some("saline"));
        assert_eq!(
            batch.contents.volume_of(chem_sim::ReagentId(1)),
            chem_sim::Units::whole(20),
            "the label did not become the contents",
        );
    }

    /// Mirrors the real registration: the two systems are `.chain()`ed, so an
    /// offer and its assessment ticket land on the same frame. An unordered
    /// fixture would pass or fail on system-order luck.
    fn witness_app() -> App {
        let mut app = offer_app();
        app.add_message::<super::super::Stimulus>()
            .add_systems(Update, witness_aid_offers.after(handle_aid_offers));
        app
    }

    fn stimuli(app: &mut App) -> Vec<super::super::Stimulus> {
        let messages = app.world().resource::<Messages<super::super::Stimulus>>();
        let mut cursor = messages.get_cursor();
        cursor.read(messages).copied().collect()
    }

    #[test]
    fn a_handoff_is_something_people_can_witness() {
        // Staff learn about a donation by seeing it happen, not because the
        // station told them. Without this the intake is omniscient.
        let mut app = witness_app();
        let (player, _) = spawn_player(&mut app, SPOT);
        request_offer(&mut app);

        let seen = stimuli(&mut app);
        assert_eq!(seen.len(), 1, "exactly one witnessable event");
        assert_eq!(seen[0].actor, Some(player));
        assert_eq!(seen[0].at, SPOT, "witnessed at the counter, not the player");
    }

    #[test]
    fn a_witness_cannot_tell_an_honest_donation_from_a_covert_one() {
        // Both produce `SuspiciousHandling` and nothing else. A witness learns
        // that a container changed hands, never what was in it — otherwise
        // seeing a handoff would be a free contraband detector.
        let mut app = witness_app();
        spawn_player(&mut app, SPOT);
        request_offer(&mut app);
        let honest = stimuli(&mut app);

        let mut covert_app = witness_app();
        let (_, container) = spawn_player(&mut covert_app, SPOT);
        covert_app
            .world_mut()
            .entity_mut(container)
            .insert(crate::labels::Label("definitely not poison".into()));
        request_offer(&mut covert_app);
        let covert = stimuli(&mut covert_app);

        assert_eq!(honest.len(), covert.len());
        assert_eq!(honest[0].kind, covert[0].kind);
        assert_eq!(
            honest[0].kind,
            super::super::StimulusKind::SuspiciousHandling,
        );
        assert_eq!(honest[0].strength, covert[0].strength);
    }

    #[test]
    fn a_refused_offer_is_never_witnessed() {
        // Nothing physically happened, so there is nothing to have seen.
        let mut app = witness_app();
        spawn_player(&mut app, SPOT + Vec3::new(40.0, 0.0, 0.0));
        request_offer(&mut app);
        assert!(stimuli(&mut app).is_empty());
    }

    fn ticket_app() -> App {
        let mut app = App::new();
        let mut spots = super::super::jobs::UtilitySpots::default();
        spots.insert(intake_spot(JobDomain::Medical), SPOT, 1);
        app.init_resource::<Time>()
            .init_resource::<AidIntakes>()
            .init_resource::<super::super::jobs::JobBoard>()
            .insert_resource(spots)
            .add_message::<FromClient<OfferAidRequested>>()
            .add_message::<AidOffered>()
            .add_systems(
                Update,
                (handle_aid_offers, publish_assessment_tickets).chain(),
            );
        app
    }

    #[test]
    fn an_offer_creates_real_work_and_finishing_it_withdraws_the_work() {
        let mut app = ticket_app();
        spawn_player(&mut app, SPOT);
        app.update();
        assert!(
            app.world()
                .resource::<super::super::jobs::JobBoard>()
                .ticket(assess_ticket_id(JobDomain::Medical))
                .is_none(),
            "no donation, no work",
        );

        request_offer(&mut app);
        let board = app.world().resource::<super::super::jobs::JobBoard>();
        let ticket = board
            .ticket(assess_ticket_id(JobDomain::Medical))
            .expect("a donation is work for somebody");
        assert_eq!(ticket.domain, JobDomain::Medical);
        assert_eq!(
            ticket.required_capability,
            super::super::jobs::JobCapability::new("medical.assess_aid"),
        );

        // Judging it takes the work off the board.
        let id = app
            .world()
            .resource::<AidIntakes>()
            .in_intake(JobDomain::Medical)
            .unwrap()
            .id;
        app.world_mut()
            .resource_mut::<AidIntakes>()
            .assess(id, Ok(()))
            .unwrap();
        app.update();
        assert!(app
            .world()
            .resource::<super::super::jobs::JobBoard>()
            .ticket(assess_ticket_id(JobDomain::Medical))
            .is_none());
    }

    #[test]
    fn a_waiting_batch_publishes_exactly_one_ticket_however_long_it_waits() {
        let mut app = ticket_app();
        spawn_player(&mut app, SPOT);
        request_offer(&mut app);
        for _ in 0..5 {
            app.update();
        }
        assert_eq!(
            app.world().resource::<super::super::jobs::JobBoard>().len(),
            1,
            "a stable ticket id must not republish every frame",
        );
    }

    #[test]
    fn a_worker_already_on_the_job_keeps_it_when_the_batch_is_judged() {
        // Withdrawing a claimed ticket would strand the worker mid-walk with a
        // reservation nobody will ever resolve.
        let mut app = ticket_app();
        spawn_player(&mut app, SPOT);
        request_offer(&mut app);
        let id = assess_ticket_id(JobDomain::Medical);
        let owner = super::super::ReservationOwner {
            agent: Entity::from_bits(7),
            action_instance: 1,
        };
        app.world_mut()
            .resource_mut::<super::super::jobs::JobBoard>()
            .claim(id, owner)
            .unwrap();

        let batch = app
            .world()
            .resource::<AidIntakes>()
            .in_intake(JobDomain::Medical)
            .unwrap()
            .id;
        app.world_mut()
            .resource_mut::<AidIntakes>()
            .assess(batch, Ok(()))
            .unwrap();
        app.update();

        assert_eq!(
            app.world()
                .resource::<super::super::jobs::JobBoard>()
                .ticket(id)
                .map(|ticket| ticket.state),
            Some(super::super::jobs::JobTicketState::Claimed(owner)),
            "a claimed ticket survives so its worker can resolve it",
        );
    }

    #[test]
    fn an_intake_is_only_freed_by_a_batch_that_is_actually_finished() {
        let (mut intakes, id) = offered();
        assert_eq!(intakes.clear_intake(JobDomain::Medical), None);
        intakes.assess(id, Ok(())).unwrap();
        assert_eq!(
            intakes.clear_intake(JobDomain::Medical),
            None,
            "an accepted batch is still sitting on the counter",
        );
        intakes.advance(id, AidState::Stored).unwrap();
        assert_eq!(intakes.clear_intake(JobDomain::Medical), Some(id));
        assert!(intakes
            .offer(JobDomain::Medical, provenance(None), solution(3))
            .is_ok());
    }
}
