//! Department-neutral work tickets and authored utility affordances.
//!
//! A ticket says that work exists. It never chooses an actor, moves anybody,
//! or applies a department consequence. Those responsibilities stay in the
//! utility selector, the shared action lifecycle, and the owning department
//! adapter respectively.

use std::collections::HashMap;

use bevy::prelude::*;

use super::{
    ActionResult, ActionTarget, Normalized, ReservationKey, ReservationOwner, UtilityBucket,
};

/// The station work domains understood by the shared job board.
///
/// This is deliberately independent from `CrewMember.role`. A named character
/// can retain old dialogue, standing, and save identity while their job profile
/// moves to a more precise domain, as Botanist Ivy eventually will.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum JobDomain {
    Medical,
    Security,
    Engineering,
    Cargo,
    Service,
    Botany,
    Bridge,
}

impl JobDomain {
    pub const ALL: [JobDomain; 7] = [
        JobDomain::Medical,
        JobDomain::Security,
        JobDomain::Engineering,
        JobDomain::Cargo,
        JobDomain::Service,
        JobDomain::Botany,
        JobDomain::Bridge,
    ];

    /// The standing department that answers for this work domain.
    ///
    /// The two enums are deliberately separate — `Department` is the authored
    /// reputation and shop surface, `JobDomain` is the work board's — but the
    /// station has exactly one Medical, so this mapping is total and lives in
    /// one place rather than being re-matched by every adapter that needs it.
    pub fn department(self) -> crate::orders::Department {
        use crate::orders::Department;
        match self {
            JobDomain::Medical => Department::Medical,
            JobDomain::Security => Department::Security,
            JobDomain::Engineering => Department::Engineering,
            JobDomain::Cargo => Department::Cargo,
            JobDomain::Service => Department::Service,
            JobDomain::Botany => Department::Botany,
            JobDomain::Bridge => Department::Bridge,
        }
    }

    pub fn from_department(department: crate::orders::Department) -> Self {
        use crate::orders::Department;
        match department {
            Department::Medical => JobDomain::Medical,
            Department::Security => JobDomain::Security,
            Department::Engineering => JobDomain::Engineering,
            Department::Cargo => JobDomain::Cargo,
            Department::Service => JobDomain::Service,
            Department::Botany => JobDomain::Botany,
            Department::Bridge => JobDomain::Bridge,
        }
    }
}

/// Content eligibility, not simulation quality. Support crew use the same
/// bodies, navigation, health, utility scoring, and consequences as core cast.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NarrativeTier {
    Core,
    Support,
}

/// Work qualifications are string-backed so a department can introduce one
/// without adding conditionals to the utility kernel.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct JobCapability(pub String);

impl JobCapability {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

/// Authority-side qualifications for one utility-controlled resident.
#[derive(Component, Clone, Debug, PartialEq, Eq)]
pub struct NpcJobProfile {
    pub primary: JobDomain,
    /// Additional domains this resident may serve without changing their home
    /// department, relationship grouping, or public role.
    pub cross_trained: Vec<JobDomain>,
    pub capabilities: Vec<JobCapability>,
    pub narrative_tier: NarrativeTier,
}

impl NpcJobProfile {
    pub fn new(
        primary: JobDomain,
        narrative_tier: NarrativeTier,
        capabilities: impl IntoIterator<Item = JobCapability>,
    ) -> Self {
        Self {
            primary,
            cross_trained: Vec::new(),
            capabilities: capabilities.into_iter().collect(),
            narrative_tier,
        }
    }

    pub fn with_cross_training(mut self, domains: impl IntoIterator<Item = JobDomain>) -> Self {
        for domain in domains {
            if domain != self.primary && !self.cross_trained.contains(&domain) {
                self.cross_trained.push(domain);
            }
        }
        self
    }

    pub fn works_in(&self, domain: JobDomain) -> bool {
        self.primary == domain || self.cross_trained.contains(&domain)
    }

    pub fn can_do(&self, capability: &JobCapability) -> bool {
        self.capabilities.iter().any(|owned| owned == capability)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct JobTicketId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobTicketState {
    Available,
    Claimed(ReservationOwner),
    Completed(ReservationOwner),
}

/// A bounded unit of work published by a department adapter.
#[derive(Clone, Debug, PartialEq)]
pub struct JobTicket {
    pub id: JobTicketId,
    pub domain: JobDomain,
    pub kind: String,
    pub target: ActionTarget,
    /// Who or what this ticket is *about*, when that is a specific body — the
    /// casualty, not the bed. Distinct from `target`, which is only where the
    /// worker walks; the two diverge whenever travel is routed to a floor spot.
    ///
    /// This is what lets an `Emergency` ticket be gated on whether a worker
    /// actually knows about the thing. `None` means the work is common
    /// knowledge (a fault light, a delivery) and needs no witness.
    pub subject: Option<Entity>,
    pub reservation: ReservationKey,
    pub reservation_capacity: usize,
    pub bucket: UtilityBucket,
    pub urgency: Normalized,
    pub required_capability: JobCapability,
    pub created_at: f32,
    pub deadline: Option<f32>,
    pub risk: Normalized,
    pub perform_seconds: f32,
    pub state: JobTicketState,
}

impl JobTicket {
    pub fn available_to(&self, profile: &NpcJobProfile) -> bool {
        self.state == JobTicketState::Available
            && profile.works_in(self.domain)
            && profile.can_do(&self.required_capability)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobBoardError {
    DuplicateId(JobTicketId),
    UnknownTicket(JobTicketId),
    NotAvailable(JobTicketId),
    ClaimChanged(JobTicketId),
}

/// Authority-only station work registry.
#[derive(Resource, Default)]
pub struct JobBoard {
    tickets: HashMap<JobTicketId, JobTicket>,
}

impl JobBoard {
    pub fn publish(&mut self, ticket: JobTicket) -> Result<(), JobBoardError> {
        if self.tickets.contains_key(&ticket.id) {
            return Err(JobBoardError::DuplicateId(ticket.id));
        }
        self.tickets.insert(ticket.id, ticket);
        Ok(())
    }

    pub fn ticket(&self, id: JobTicketId) -> Option<&JobTicket> {
        self.tickets.get(&id)
    }

    pub fn available_for<'a>(
        &'a self,
        profile: &'a NpcJobProfile,
    ) -> impl Iterator<Item = &'a JobTicket> + 'a {
        self.tickets
            .values()
            .filter(move |ticket| ticket.available_to(profile))
    }

    pub fn iter(&self) -> impl Iterator<Item = &JobTicket> {
        self.tickets.values()
    }

    pub fn claim(&mut self, id: JobTicketId, owner: ReservationOwner) -> Result<(), JobBoardError> {
        let ticket = self
            .tickets
            .get_mut(&id)
            .ok_or(JobBoardError::UnknownTicket(id))?;
        match ticket.state {
            JobTicketState::Available => ticket.state = JobTicketState::Claimed(owner),
            JobTicketState::Claimed(current) if current == owner => return Ok(()),
            _ => return Err(JobBoardError::NotAvailable(id)),
        }
        Ok(())
    }

    /// Reopens a claimed ticket only when the same action still owns it. A late
    /// interruption from an older action cannot steal a newer worker's claim.
    pub fn release_claim(
        &mut self,
        id: JobTicketId,
        owner: ReservationOwner,
    ) -> Result<(), JobBoardError> {
        let ticket = self
            .tickets
            .get_mut(&id)
            .ok_or(JobBoardError::UnknownTicket(id))?;
        if ticket.state != JobTicketState::Claimed(owner) {
            return Err(JobBoardError::ClaimChanged(id));
        }
        ticket.state = JobTicketState::Available;
        Ok(())
    }

    /// Completes successful work and reopens recoverable failures. Department
    /// state is changed later by that department's resolution adapter.
    pub fn resolve(
        &mut self,
        id: JobTicketId,
        owner: ReservationOwner,
        result: ActionResult,
    ) -> Result<(), JobBoardError> {
        let ticket = self
            .tickets
            .get_mut(&id)
            .ok_or(JobBoardError::UnknownTicket(id))?;
        if ticket.state != JobTicketState::Claimed(owner) {
            return Err(JobBoardError::ClaimChanged(id));
        }
        ticket.state = if result == ActionResult::Completed {
            JobTicketState::Completed(owner)
        } else {
            JobTicketState::Available
        };
        Ok(())
    }

    /// Takes a terminal ticket only when the caller presents the exact action
    /// instance that completed it. An old completion message therefore cannot
    /// consume a stable ticket ID that has since been reused by another actor.
    pub fn take_completed(
        &mut self,
        id: JobTicketId,
        owner: ReservationOwner,
    ) -> Result<JobTicket, JobBoardError> {
        let ticket = self
            .tickets
            .get(&id)
            .ok_or(JobBoardError::UnknownTicket(id))?;
        if ticket.state != JobTicketState::Completed(owner) {
            return Err(JobBoardError::ClaimChanged(id));
        }
        Ok(self
            .tickets
            .remove(&id)
            .expect("the completed ticket was just observed"))
    }

    pub fn reopen_completed(
        &mut self,
        id: JobTicketId,
        owner: ReservationOwner,
    ) -> Result<(), JobBoardError> {
        let ticket = self
            .tickets
            .get_mut(&id)
            .ok_or(JobBoardError::UnknownTicket(id))?;
        if ticket.state != JobTicketState::Completed(owner) {
            return Err(JobBoardError::ClaimChanged(id));
        }
        ticket.state = JobTicketState::Available;
        Ok(())
    }

    /// Removes work whose owning world fact no longer exists. This is not a
    /// normal action outcome and therefore deliberately bypasses the completed
    /// state and owner requirement used by [`Self::take_completed`].
    pub fn cancel(&mut self, id: JobTicketId) -> Option<JobTicket> {
        self.tickets.remove(&id)
    }

    pub fn remove_domain(&mut self, domain: JobDomain) -> usize {
        let before = self.tickets.len();
        self.tickets.retain(|_, ticket| ticket.domain != domain);
        before - self.tickets.len()
    }

    pub fn reopen_orphaned_claims(&mut self, mut alive: impl FnMut(Entity) -> bool) -> usize {
        let mut reopened = 0;
        for ticket in self.tickets.values_mut() {
            let owner = match ticket.state {
                JobTicketState::Claimed(owner) | JobTicketState::Completed(owner) => owner,
                JobTicketState::Available => continue,
            };
            if !alive(owner.agent) {
                ticket.state = JobTicketState::Available;
                reopened += 1;
            }
        }
        reopened
    }

    pub fn len(&self) -> usize {
        self.tickets.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tickets.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UtilitySpotDef {
    pub at: Vec3,
    pub capacity: usize,
}

/// Authored work, bed, seat, and interaction slots keyed by stable map ID.
#[derive(Resource, Default)]
pub struct UtilitySpots {
    spots: HashMap<String, UtilitySpotDef>,
}

impl UtilitySpots {
    pub fn insert(&mut self, id: impl Into<String>, at: Vec3, capacity: usize) -> bool {
        let id = id.into();
        if id.trim().is_empty() || capacity == 0 || self.spots.contains_key(&id) {
            return false;
        }
        self.spots.insert(id, UtilitySpotDef { at, capacity });
        true
    }

    pub fn get(&self, id: &str) -> Option<UtilitySpotDef> {
        self.spots.get(id).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, UtilitySpotDef)> {
        self.spots
            .iter()
            .map(|(id, definition)| (id.as_str(), *definition))
    }
}

/// When each standing post may next be advertised.
///
/// Security and Bridge publish *watches* rather than finite work: a console
/// does not stop wanting to be staffed, so their tickets are republished as
/// soon as the board is empty of them. Without a cooldown that happens the very
/// frame the post is vacated, and the officer still standing on it re-claims it
/// immediately — legal, but it reads as a stuck loop and, worse, it means the
/// officer never walks anywhere. A department whose job is noticing has to move
/// through rooms to notice anything.
#[derive(Resource, Default)]
pub struct StandingPostCooldowns {
    until: HashMap<JobTicketId, f32>,
}

impl StandingPostCooldowns {
    /// Whether this post may be advertised again yet.
    pub fn ready(&self, id: JobTicketId, now: f32) -> bool {
        self.until.get(&id).is_none_or(|until| now >= *until)
    }

    pub fn begin(&mut self, id: JobTicketId, now: f32) {
        self.until.insert(id, now + STANDING_POST_COOLDOWN_SECONDS);
    }

    pub fn clear(&mut self) {
        self.until.clear();
    }
}

/// How long a worked post stays off the board.
///
/// Long enough that the vacating worker's decision clock (0.35-0.75s) fires
/// several times against a board without it, so they pick something else and
/// the rotation — and the travel — becomes visible.
const STANDING_POST_COOLDOWN_SECONDS: f32 = 8.0;

/// Takes completed standing-post tickets off the board so they can be
/// republished.
///
/// The half Security and Bridge were missing. Every other department has an
/// `apply_*_job_results` system that consumes its completions; those two had
/// none, so a finished ticket stayed `Completed(owner)` forever, the
/// `board.ticket(id).is_some()` guard in their publishers stayed true, and each
/// department published its posts exactly once per shift and then went silent.
/// Live, that read as `Security 3/0` and `Bridge 4/0` in every snapshot of a
/// 500-second run.
///
/// Unlike a department adapter there is no world state to change — the post is
/// simply released. Shared rather than copy-pasted into both files because they
/// need identical logic, and `security.rs` already records that Engineering
/// shipped a migration bug exactly that way.
pub(super) fn take_completed_standing_posts(
    domain: JobDomain,
    results: &[(JobTicketId, ReservationOwner)],
    board: &mut JobBoard,
    cooldowns: &mut StandingPostCooldowns,
    now: f32,
) -> usize {
    let mut taken = 0;
    for (id, claim) in results {
        let Some(ticket) = board.ticket(*id) else {
            continue;
        };
        if ticket.domain != domain || ticket.state != JobTicketState::Completed(*claim) {
            continue;
        }
        if board.take_completed(*id, *claim).is_ok() {
            cooldowns.begin(*id, now);
            taken += 1;
        }
    }
    taken
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticket(id: u64) -> JobTicket {
        JobTicket {
            id: JobTicketId(id),
            domain: JobDomain::Cargo,
            kind: "cargo.sort".into(),
            target: ActionTarget::Point(Vec3::ZERO),
            subject: None,
            reservation: ReservationKey("cargo.sort".into()),
            reservation_capacity: 1,
            bucket: UtilityBucket::Routine,
            urgency: Normalized::ONE,
            required_capability: JobCapability::new("cargo.freight"),
            created_at: 0.0,
            deadline: None,
            risk: Normalized::ZERO,
            perform_seconds: 1.0,
            state: JobTicketState::Available,
        }
    }

    fn worker() -> NpcJobProfile {
        NpcJobProfile::new(
            JobDomain::Cargo,
            NarrativeTier::Support,
            [JobCapability::new("cargo.freight")],
        )
    }

    #[test]
    fn tickets_are_filtered_by_domain_and_capability() {
        let mut board = JobBoard::default();
        board.publish(ticket(1)).unwrap();
        assert_eq!(board.available_for(&worker()).count(), 1);

        let medical = NpcJobProfile::new(
            JobDomain::Medical,
            NarrativeTier::Core,
            [JobCapability::new("cargo.freight")],
        );
        let cargo_without_skill = NpcJobProfile::new(JobDomain::Cargo, NarrativeTier::Core, []);
        assert_eq!(board.available_for(&medical).count(), 0);
        assert_eq!(board.available_for(&cargo_without_skill).count(), 0);
    }

    #[test]
    fn cross_training_expands_job_eligibility_without_changing_primary_department() {
        let mut board = JobBoard::default();
        board.publish(ticket(2)).unwrap();
        let bridge_specialist = NpcJobProfile::new(
            JobDomain::Bridge,
            NarrativeTier::Core,
            [JobCapability::new("cargo.freight")],
        )
        .with_cross_training([JobDomain::Cargo, JobDomain::Cargo]);

        assert_eq!(bridge_specialist.primary, JobDomain::Bridge);
        assert_eq!(bridge_specialist.cross_trained, vec![JobDomain::Cargo]);
        assert!(bridge_specialist.works_in(JobDomain::Bridge));
        assert!(bridge_specialist.works_in(JobDomain::Cargo));
        assert_eq!(board.available_for(&bridge_specialist).count(), 1);
    }

    #[test]
    fn compare_and_swap_claims_prevent_late_cleanup_from_stealing_work() {
        let mut board = JobBoard::default();
        board.publish(ticket(7)).unwrap();
        let first = ReservationOwner {
            agent: Entity::from_bits(1),
            action_instance: 1,
        };
        let second = ReservationOwner {
            agent: Entity::from_bits(2),
            action_instance: 2,
        };

        board.claim(JobTicketId(7), first).unwrap();
        assert_eq!(
            board.claim(JobTicketId(7), second),
            Err(JobBoardError::NotAvailable(JobTicketId(7)))
        );
        board.release_claim(JobTicketId(7), first).unwrap();
        board.claim(JobTicketId(7), second).unwrap();
        assert_eq!(
            board.release_claim(JobTicketId(7), first),
            Err(JobBoardError::ClaimChanged(JobTicketId(7)))
        );
        assert_eq!(
            board.ticket(JobTicketId(7)).unwrap().state,
            JobTicketState::Claimed(second)
        );
    }

    #[test]
    fn only_successful_resolution_makes_a_ticket_terminal() {
        let mut board = JobBoard::default();
        let owner = ReservationOwner {
            agent: Entity::from_bits(1),
            action_instance: 8,
        };
        board.publish(ticket(9)).unwrap();
        board.claim(JobTicketId(9), owner).unwrap();
        board
            .resolve(JobTicketId(9), owner, ActionResult::Unreachable)
            .unwrap();
        assert_eq!(
            board.ticket(JobTicketId(9)).unwrap().state,
            JobTicketState::Available
        );

        board.claim(JobTicketId(9), owner).unwrap();
        board
            .resolve(JobTicketId(9), owner, ActionResult::Completed)
            .unwrap();
        assert!(board.take_completed(JobTicketId(9), owner).is_ok());
        assert!(board.is_empty());
    }

    #[test]
    fn stale_completion_cannot_consume_a_reused_ticket_id() {
        let mut board = JobBoard::default();
        let stale = ReservationOwner {
            agent: Entity::from_bits(1),
            action_instance: 8,
        };
        let current = ReservationOwner {
            agent: Entity::from_bits(2),
            action_instance: 9,
        };
        board.publish(ticket(12)).unwrap();
        board.claim(JobTicketId(12), current).unwrap();
        board
            .resolve(JobTicketId(12), current, ActionResult::Completed)
            .unwrap();

        assert_eq!(
            board.take_completed(JobTicketId(12), stale),
            Err(JobBoardError::ClaimChanged(JobTicketId(12))),
        );
        assert_eq!(
            board.ticket(JobTicketId(12)).unwrap().state,
            JobTicketState::Completed(current),
        );
        assert!(board.take_completed(JobTicketId(12), current).is_ok());
    }

    #[test]
    fn despawned_workers_reopen_their_claimed_ticket() {
        let mut board = JobBoard::default();
        let owner = ReservationOwner {
            agent: Entity::from_bits(14),
            action_instance: 3,
        };
        board.publish(ticket(11)).unwrap();
        board.claim(JobTicketId(11), owner).unwrap();

        assert_eq!(board.reopen_orphaned_claims(|_| false), 1);
        assert_eq!(
            board.ticket(JobTicketId(11)).unwrap().state,
            JobTicketState::Available,
        );
        assert_eq!(board.reopen_orphaned_claims(|_| false), 0);
    }

    #[test]
    fn every_work_domain_answers_to_exactly_one_standing_department() {
        // Both directions, so adding an eighth department or domain without
        // pairing it fails here rather than silently landing every new
        // department's aid and pressure on whichever arm was copied last.
        for domain in JobDomain::ALL {
            assert_eq!(JobDomain::from_department(domain.department()), domain);
        }
        for department in crate::orders::Department::ALL {
            assert_eq!(
                JobDomain::from_department(department).department(),
                department,
            );
        }
    }

    #[test]
    fn utility_spots_reject_empty_duplicate_and_zero_capacity_entries() {
        let mut spots = UtilitySpots::default();
        assert!(!spots.insert("", Vec3::ZERO, 1));
        assert!(!spots.insert("cargo.bad", Vec3::ZERO, 0));
        assert!(spots.insert("cargo.sort", Vec3::X, 1));
        assert!(!spots.insert("cargo.sort", Vec3::Y, 2));
        assert_eq!(
            spots.get("cargo.sort"),
            Some(UtilitySpotDef {
                at: Vec3::X,
                capacity: 1
            })
        );
    }
}
