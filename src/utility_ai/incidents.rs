//! Consequential station incidents shared by department adapters.
//!
//! Incidents describe something that happened to the world. They do not select
//! a responder or prescribe an action, which keeps Medical, Security, and
//! witness behavior on the same utility/ticket path as ordinary work.

use std::{cmp::Ordering, collections::HashMap};

use bevy::prelude::*;

use super::{mix64, JobDomain, Normalized};

const MIN_STABILITY_RISK_MULTIPLIER: f32 = 0.35;
const MAX_STABILITY_RISK_MULTIPLIER: f32 = 2.0;
const MAX_ROUTINE_INCIDENT_CHANCE: f32 = 0.50;

/// Shared station-pressure adjustment for every department's hazardous work.
///
/// Ticket risk remains the source of an incident. Station stability only
/// changes how often that authored risk becomes a problem, so a safe task does
/// not become dangerous merely because the station is struggling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WorkplaceRiskPressure {
    pub chance: Normalized,
    /// How much of a department's opening grace is consumed by one hazardous
    /// completion. Unstable stations exhaust that quiet window sooner.
    pub grace_cost: u32,
}

impl WorkplaceRiskPressure {
    pub fn from_stability(
        base_risk: Normalized,
        stability: &crate::instability::StationStability,
    ) -> Self {
        let health = (stability.value / crate::instability::STABILITY_MAX).clamp(0.0, 1.0);
        Self::from_health_and_band(base_risk, health, stability.band)
    }

    fn from_health_and_band(
        base_risk: Normalized,
        health: f32,
        band: crate::instability::StabilityBand,
    ) -> Self {
        let distress = 1.0 - health;
        let multiplier = MIN_STABILITY_RISK_MULTIPLIER
            + (MAX_STABILITY_RISK_MULTIPLIER - MIN_STABILITY_RISK_MULTIPLIER) * distress;
        let chance = (base_risk.get() * multiplier).min(MAX_ROUTINE_INCIDENT_CHANCE);
        let grace_cost = match band {
            crate::instability::StabilityBand::Stable
            | crate::instability::StabilityBand::Strained => 1,
            crate::instability::StabilityBand::Unstable => 2,
            crate::instability::StabilityBand::Critical
            | crate::instability::StabilityBand::Evacuating => 3,
        };
        Self {
            chance: Normalized::new(chance).expect("bounded workplace risk is normalized"),
            grace_cost,
        }
    }
}

/// The one station-health reading used while deciding a department problem.
///
/// Domain adapters receive this snapshot with a permit and must derive the
/// severity of that same outcome from it. This prevents one candidate from
/// sampling station stability for frequency and then sampling it again for a
/// consequence after other systems have changed the resource.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProblemStabilitySnapshot {
    health: f32,
    pub band: crate::instability::StabilityBand,
    pub pressure: WorkplaceRiskPressure,
}

impl ProblemStabilitySnapshot {
    fn capture(base_risk: Normalized, stability: &crate::instability::StationStability) -> Self {
        let health = (stability.value / crate::instability::STABILITY_MAX).clamp(0.0, 1.0);
        Self {
            health,
            band: stability.band,
            pressure: WorkplaceRiskPressure::from_health_and_band(
                base_risk,
                health,
                stability.band,
            ),
        }
    }

    pub fn health(self) -> f32 {
        self.health
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IncidentId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum IncidentKind {
    Burn,
    BruteInjury,
    Poisoning,
    Fire,
    EquipmentFailure,
    Tampering,
    Theft,
    Contamination,
}

/// Shared safety policy for one problem-producing department.
///
/// The director owns these counters. Department adapters still own candidate
/// facts and world consequences, but they must not implement parallel grace,
/// cooldown, active-case, or per-shift counters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DepartmentProblemPolicy {
    pub domain: JobDomain,
    pub opening_grace: u32,
    pub domain_cooldown: u32,
    pub actor_cooldown: u32,
    pub unresolved_cap: usize,
    pub shift_cap: u32,
}

impl DepartmentProblemPolicy {
    fn is_valid(self) -> bool {
        self.unresolved_cap > 0 && self.shift_cap > 0
    }
}

/// A real domain fact that may become a consequential station problem.
///
/// `entropy_key` must come from stable authored identity, never iteration
/// order or a process-wide random number generator.
#[derive(Clone, Debug, PartialEq)]
pub struct DepartmentProblemCandidate {
    pub problem: String,
    pub domain: JobDomain,
    pub incident_kind: IncidentKind,
    pub subject: Entity,
    pub base_risk: Normalized,
    pub entropy_key: u64,
}

impl DepartmentProblemCandidate {
    pub fn new(
        problem: impl Into<String>,
        domain: JobDomain,
        incident_kind: IncidentKind,
        subject: Entity,
        base_risk: Normalized,
        entropy_key: u64,
    ) -> Self {
        Self {
            problem: problem.into(),
            domain,
            incident_kind,
            subject,
            base_risk,
            entropy_key,
        }
    }

    /// Total ordering used by adapters before they ask for permits. It makes a
    /// same-frame batch independent of message-reader or hash-map order.
    pub fn deterministic_cmp(&self, other: &Self) -> Ordering {
        domain_order(self.domain)
            .cmp(&domain_order(other.domain))
            .then_with(|| self.problem.cmp(&other.problem))
            .then_with(|| self.incident_kind.cmp(&other.incident_kind))
            .then_with(|| self.subject.to_bits().cmp(&other.subject.to_bits()))
            .then_with(|| self.entropy_key.cmp(&other.entropy_key))
    }
}

fn domain_order(domain: JobDomain) -> u8 {
    match domain {
        JobDomain::Medical => 0,
        JobDomain::Security => 1,
        JobDomain::Engineering => 2,
        JobDomain::Cargo => 3,
        JobDomain::Service => 4,
        JobDomain::Botany => 5,
        JobDomain::Bridge => 6,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProblemSuppression {
    UnconfiguredDomain,
    UnresolvedCap,
    ShiftCap,
    Ledger(IncidentError),
    OpeningGrace,
    DomainCooldown,
    ActorCooldown,
    RollMiss,
}

/// Opaque authorization for exactly one candidate consequence.
///
/// A domain adapter must pass every permit to either
/// [`DepartmentProblemDirector::commit`] after applying the consequence, or
/// [`DepartmentProblemDirector::abort`] after an apply failure.
#[derive(Debug)]
#[must_use = "a department problem permit must be committed or aborted"]
pub struct DepartmentProblemPermit {
    token: u64,
    candidate: DepartmentProblemCandidate,
    stability: ProblemStabilitySnapshot,
    forced: bool,
}

impl DepartmentProblemPermit {
    pub fn candidate(&self) -> &DepartmentProblemCandidate {
        &self.candidate
    }

    pub fn stability(&self) -> ProblemStabilitySnapshot {
        self.stability
    }

    pub fn is_forced(&self) -> bool {
        self.forced
    }
}

#[derive(Debug)]
pub enum ProblemPermitDecision {
    Permit(DepartmentProblemPermit),
    Suppressed(ProblemSuppression),
}

#[derive(Clone, Debug)]
struct DepartmentProblemState {
    policy: DepartmentProblemPolicy,
    candidates_seen: u64,
    grace_remaining: u32,
    domain_cooldown_remaining: u32,
    committed_this_shift: u32,
}

impl DepartmentProblemState {
    fn new(policy: DepartmentProblemPolicy) -> Self {
        Self {
            policy,
            candidates_seen: 0,
            grace_remaining: policy.opening_grace,
            domain_cooldown_remaining: 0,
            committed_this_shift: 0,
        }
    }
}

#[derive(Clone, Debug)]
struct PendingProblemPermit {
    problem: String,
    domain: JobDomain,
    incident_kind: IncidentKind,
    subject: Entity,
    forced: bool,
}

/// The authority-owned gate for ordinary department problems.
///
/// Candidate facts enter here exactly once. A permit reserves the relevant
/// director capacity but does not consume a forced outcome or start cooldowns
/// until its domain reports that the world consequence was applied.
#[derive(Resource)]
pub struct DepartmentProblemDirector {
    domains: HashMap<JobDomain, DepartmentProblemState>,
    actor_cooldowns: HashMap<(JobDomain, Entity), u32>,
    forced: HashMap<(JobDomain, String), u32>,
    pending: HashMap<u64, PendingProblemPermit>,
    next_permit: u64,
}

impl Default for DepartmentProblemDirector {
    fn default() -> Self {
        Self {
            domains: HashMap::new(),
            actor_cooldowns: HashMap::new(),
            forced: HashMap::new(),
            pending: HashMap::new(),
            next_permit: 1,
        }
    }
}

impl DepartmentProblemDirector {
    /// Registers a policy without disturbing live counters. Re-registering a
    /// different policy is a content bug; shift transitions must use reset.
    pub fn ensure_policy(&mut self, policy: DepartmentProblemPolicy) {
        assert!(
            policy.is_valid(),
            "department problem policy must be bounded"
        );
        if let Some(current) = self.domains.get(&policy.domain) {
            assert_eq!(
                current.policy, policy,
                "a department problem policy changed without a shift reset"
            );
            return;
        }
        self.domains
            .insert(policy.domain, DepartmentProblemState::new(policy));
    }

    /// Begins a fresh shift for one domain without touching other adapters.
    pub fn reset_domain(&mut self, policy: DepartmentProblemPolicy) {
        assert!(
            policy.is_valid(),
            "department problem policy must be bounded"
        );
        self.domains
            .insert(policy.domain, DepartmentProblemState::new(policy));
        self.actor_cooldowns
            .retain(|(domain, _), _| *domain != policy.domain);
        self.forced
            .retain(|(domain, _), _| *domain != policy.domain);
        self.pending
            .retain(|_, permit| permit.domain != policy.domain);
    }

    pub fn force_next(&mut self, domain: JobDomain, problem: impl Into<String>) {
        let count = self.forced.entry((domain, problem.into())).or_default();
        *count = count.saturating_add(1);
    }

    pub fn forced_count(&self, domain: JobDomain, problem: &str) -> u32 {
        self.forced
            .get(&(domain, problem.to_owned()))
            .copied()
            .unwrap_or_default()
    }

    /// Applies shared policy and reserves capacity for one domain consequence.
    /// Stability is captured once and travels with a successful permit.
    pub fn request_permit(
        &mut self,
        candidate: DepartmentProblemCandidate,
        stability: &crate::instability::StationStability,
        incidents: &IncidentLedger,
    ) -> ProblemPermitDecision {
        let snapshot = ProblemStabilitySnapshot::capture(candidate.base_risk, stability);
        let Some(current) = self.domains.get(&candidate.domain) else {
            return ProblemPermitDecision::Suppressed(ProblemSuppression::UnconfiguredDomain);
        };
        let policy = current.policy;
        let pending_in_domain = self
            .pending
            .values()
            .filter(|permit| permit.domain == candidate.domain)
            .count();
        if incidents.active_in_domain(candidate.domain).count() + pending_in_domain
            >= policy.unresolved_cap
        {
            return ProblemPermitDecision::Suppressed(ProblemSuppression::UnresolvedCap);
        }
        if current
            .committed_this_shift
            .saturating_add(pending_in_domain as u32)
            >= policy.shift_cap
        {
            return ProblemPermitDecision::Suppressed(ProblemSuppression::ShiftCap);
        }
        if let Err(error) = incidents.can_create(candidate.incident_kind, candidate.subject) {
            return ProblemPermitDecision::Suppressed(ProblemSuppression::Ledger(error));
        }
        if incidents.remaining_active_capacity() <= self.pending.len() {
            return ProblemPermitDecision::Suppressed(ProblemSuppression::Ledger(
                IncidentError::CapacityReached,
            ));
        }
        if self.pending.values().any(|permit| {
            permit.subject == candidate.subject && permit.incident_kind == candidate.incident_kind
        }) {
            return ProblemPermitDecision::Suppressed(ProblemSuppression::Ledger(
                IncidentError::DuplicateActiveCase,
            ));
        }

        let queued_forced = self.forced_count(candidate.domain, &candidate.problem);
        let reserved_forced = self
            .pending
            .values()
            .filter(|permit| {
                permit.forced
                    && permit.domain == candidate.domain
                    && permit.problem == candidate.problem
            })
            .count() as u32;
        let forced = queued_forced > reserved_forced;
        let state = self
            .domains
            .get_mut(&candidate.domain)
            .expect("the policy was checked above");
        state.candidates_seen = state.candidates_seen.wrapping_add(1);

        if !forced {
            if state.grace_remaining > 0 {
                state.grace_remaining = state
                    .grace_remaining
                    .saturating_sub(snapshot.pressure.grace_cost);
                return ProblemPermitDecision::Suppressed(ProblemSuppression::OpeningGrace);
            }

            let domain_on_cooldown = state.domain_cooldown_remaining > 0;
            state.domain_cooldown_remaining = state.domain_cooldown_remaining.saturating_sub(1);
            let actor_key = (candidate.domain, candidate.subject);
            let actor_on_cooldown = self
                .actor_cooldowns
                .get(&actor_key)
                .copied()
                .unwrap_or_default()
                > 0;
            if let Some(remaining) = self.actor_cooldowns.get_mut(&actor_key) {
                *remaining = remaining.saturating_sub(1);
                if *remaining == 0 {
                    self.actor_cooldowns.remove(&actor_key);
                }
            }
            if domain_on_cooldown {
                return ProblemPermitDecision::Suppressed(ProblemSuppression::DomainCooldown);
            }
            if actor_on_cooldown {
                return ProblemPermitDecision::Suppressed(ProblemSuppression::ActorCooldown);
            }

            let entropy = mix64(candidate.entropy_key ^ state.candidates_seen.rotate_left(19));
            let roll = (entropy & 0xffff) as f32 / u16::MAX as f32;
            if roll >= snapshot.pressure.chance.get() {
                return ProblemPermitDecision::Suppressed(ProblemSuppression::RollMiss);
            }
        }

        let token = self.next_permit;
        self.next_permit = self.next_permit.wrapping_add(1).max(1);
        self.pending.insert(
            token,
            PendingProblemPermit {
                problem: candidate.problem.clone(),
                domain: candidate.domain,
                incident_kind: candidate.incident_kind,
                subject: candidate.subject,
                forced,
            },
        );
        ProblemPermitDecision::Permit(DepartmentProblemPermit {
            token,
            candidate,
            stability: snapshot,
            forced,
        })
    }

    /// Finalizes a successfully applied consequence. This is the only point
    /// that consumes a forced outcome or starts either cooldown.
    pub fn commit(&mut self, permit: DepartmentProblemPermit) {
        let pending = self
            .pending
            .remove(&permit.token)
            .expect("department problem permit was not pending at commit");
        debug_assert_eq!(pending.domain, permit.candidate.domain);
        debug_assert_eq!(pending.subject, permit.candidate.subject);
        let state = self
            .domains
            .get_mut(&pending.domain)
            .expect("a pending permit retains its department policy");
        state.committed_this_shift = state.committed_this_shift.saturating_add(1);
        state.domain_cooldown_remaining = state.policy.domain_cooldown;
        if state.policy.actor_cooldown > 0 {
            self.actor_cooldowns.insert(
                (pending.domain, pending.subject),
                state.policy.actor_cooldown,
            );
        }
        if pending.forced {
            let key = (pending.domain, pending.problem);
            let remaining = self
                .forced
                .get_mut(&key)
                .expect("a forced permit retains its queued outcome");
            *remaining -= 1;
            if *remaining == 0 {
                self.forced.remove(&key);
            }
        }
    }

    /// Releases a permit after the domain could not apply its consequence.
    /// No cooldown, shift count, or forced-outcome count changes.
    pub fn abort(&mut self, permit: DepartmentProblemPermit) {
        let pending = self
            .pending
            .remove(&permit.token)
            .expect("department problem permit was not pending at abort");
        debug_assert_eq!(pending.domain, permit.candidate.domain);
        debug_assert_eq!(pending.subject, permit.candidate.subject);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncidentStatus {
    Active,
    Resolved,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IncidentRecord {
    pub id: IncidentId,
    pub kind: IncidentKind,
    pub department: JobDomain,
    pub subject: Entity,
    pub source: Option<Entity>,
    pub location: Vec3,
    pub severity: Normalized,
    pub created_at: f32,
    pub status: IncidentStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IncidentError {
    DuplicateActiveCase,
    CapacityReached,
    UnknownIncident,
    AlreadyResolved,
}

/// Bounded authority-side truth about active consequential situations.
#[derive(Resource)]
pub struct IncidentLedger {
    incidents: HashMap<IncidentId, IncidentRecord>,
    next_id: u64,
    active_capacity: usize,
}

impl Default for IncidentLedger {
    fn default() -> Self {
        Self {
            incidents: HashMap::new(),
            next_id: 1,
            active_capacity: 32,
        }
    }
}

impl IncidentLedger {
    pub fn can_create(&self, kind: IncidentKind, subject: Entity) -> Result<(), IncidentError> {
        if self.incidents.values().any(|incident| {
            incident.status == IncidentStatus::Active
                && incident.subject == subject
                && incident.kind == kind
        }) {
            return Err(IncidentError::DuplicateActiveCase);
        }
        if self.active().count() >= self.active_capacity {
            return Err(IncidentError::CapacityReached);
        }
        Ok(())
    }

    pub fn remaining_active_capacity(&self) -> usize {
        self.active_capacity.saturating_sub(self.active().count())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create(
        &mut self,
        kind: IncidentKind,
        department: JobDomain,
        subject: Entity,
        source: Option<Entity>,
        location: Vec3,
        severity: Normalized,
        created_at: f32,
    ) -> Result<IncidentId, IncidentError> {
        self.can_create(kind, subject)?;
        let id = IncidentId(self.next_id);
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.incidents.insert(
            id,
            IncidentRecord {
                id,
                kind,
                department,
                subject,
                source,
                location,
                severity,
                created_at,
                status: IncidentStatus::Active,
            },
        );
        Ok(id)
    }

    pub fn get(&self, id: IncidentId) -> Option<&IncidentRecord> {
        self.incidents.get(&id)
    }

    pub fn active(&self) -> impl Iterator<Item = &IncidentRecord> {
        self.incidents
            .values()
            .filter(|incident| incident.status == IncidentStatus::Active)
    }

    pub fn active_for(&self, subject: Entity) -> impl Iterator<Item = &IncidentRecord> {
        self.active()
            .filter(move |incident| incident.subject == subject)
    }

    pub fn active_in_domain(&self, domain: JobDomain) -> impl Iterator<Item = &IncidentRecord> {
        self.active()
            .filter(move |incident| incident.department == domain)
    }

    pub fn resolve(&mut self, id: IncidentId) -> Result<(), IncidentError> {
        let incident = self
            .incidents
            .get_mut(&id)
            .ok_or(IncidentError::UnknownIncident)?;
        if incident.status == IncidentStatus::Resolved {
            return Err(IncidentError::AlreadyResolved);
        }
        incident.status = IncidentStatus::Resolved;
        Ok(())
    }

    /// Records who an investigation concluded was responsible.
    ///
    /// Separate from [`Self::create`] because attribution is *earned later*:
    /// a covert incident is created with no known source, and only witness
    /// testimony can fill it in. An existing attribution is never overwritten,
    /// so a system that already knew the culprit at creation time keeps its
    /// ground truth and a later investigation cannot contradict it.
    pub fn attribute(&mut self, id: IncidentId, source: Entity) -> Result<(), IncidentError> {
        let incident = self
            .incidents
            .get_mut(&id)
            .ok_or(IncidentError::UnknownIncident)?;
        if incident.source.is_none() {
            incident.source = Some(source);
        }
        Ok(())
    }

    pub fn retain_active(&mut self) -> usize {
        let before = self.incidents.len();
        self.incidents
            .retain(|_, incident| incident.status == IncidentStatus::Active);
        before - self.incidents.len()
    }

    pub fn remove_domain(&mut self, domain: JobDomain) -> usize {
        let before = self.incidents.len();
        self.incidents
            .retain(|_, incident| incident.department != domain);
        before - self.incidents.len()
    }
}

#[derive(Message, Clone, Copy, Debug, PartialEq)]
pub struct IncidentCreated {
    pub id: IncidentId,
    pub kind: IncidentKind,
    pub department: JobDomain,
    pub subject: Entity,
    pub location: Vec3,
    pub severity: Normalized,
}

#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub struct IncidentResolved {
    pub id: IncidentId,
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PROBLEM: &str = "cargo.test.problem";

    fn stability(
        value: f32,
        band: crate::instability::StabilityBand,
    ) -> crate::instability::StationStability {
        crate::instability::StationStability {
            value,
            band,
            station_age: 0.0,
            decay_accumulator: 0.0,
        }
    }

    fn test_policy() -> DepartmentProblemPolicy {
        DepartmentProblemPolicy {
            domain: JobDomain::Cargo,
            opening_grace: 0,
            domain_cooldown: 2,
            actor_cooldown: 2,
            unresolved_cap: 1,
            shift_cap: 3,
        }
    }

    fn candidate(subject: Entity) -> DepartmentProblemCandidate {
        DepartmentProblemCandidate::new(
            TEST_PROBLEM,
            JobDomain::Cargo,
            IncidentKind::Burn,
            subject,
            Normalized::new(0.12).unwrap(),
            47,
        )
    }

    #[test]
    fn station_stability_monotonically_controls_routine_incident_frequency() {
        let base = Normalized::new(0.12).unwrap();
        let stable = WorkplaceRiskPressure::from_stability(
            base,
            &stability(100.0, crate::instability::StabilityBand::Stable),
        );
        let strained = WorkplaceRiskPressure::from_stability(
            base,
            &stability(65.0, crate::instability::StabilityBand::Strained),
        );
        let unstable = WorkplaceRiskPressure::from_stability(
            base,
            &stability(40.0, crate::instability::StabilityBand::Unstable),
        );
        let critical = WorkplaceRiskPressure::from_stability(
            base,
            &stability(10.0, crate::instability::StabilityBand::Critical),
        );

        assert!(stable.chance.get() < strained.chance.get());
        assert!(strained.chance.get() < unstable.chance.get());
        assert!(unstable.chance.get() < critical.chance.get());
        assert_eq!(stable.grace_cost, 1);
        assert_eq!(strained.grace_cost, 1);
        assert_eq!(unstable.grace_cost, 2);
        assert_eq!(critical.grace_cost, 3);
    }

    #[test]
    fn routine_incident_frequency_has_a_hard_cap_even_at_zero_stability() {
        let pressure = WorkplaceRiskPressure::from_stability(
            Normalized::ONE,
            &stability(0.0, crate::instability::StabilityBand::Evacuating),
        );
        assert_eq!(pressure.chance.get(), MAX_ROUTINE_INCIDENT_CHANCE);
        assert_eq!(pressure.grace_cost, 3);
    }

    #[test]
    fn aborted_forced_permit_stays_queued_and_does_not_start_cooldowns() {
        let mut director = DepartmentProblemDirector::default();
        director.reset_domain(test_policy());
        director.force_next(JobDomain::Cargo, TEST_PROBLEM);
        let ledger = IncidentLedger::default();
        let subject = Entity::from_bits(10);
        let stable = stability(100.0, crate::instability::StabilityBand::Stable);

        let ProblemPermitDecision::Permit(first) =
            director.request_permit(candidate(subject), &stable, &ledger)
        else {
            panic!("the forced candidate should receive a permit");
        };
        assert!(first.is_forced());
        assert_eq!(first.stability().health(), 1.0);
        director.abort(first);

        assert_eq!(director.forced_count(JobDomain::Cargo, TEST_PROBLEM), 1);
        let ProblemPermitDecision::Permit(retry) =
            director.request_permit(candidate(subject), &stable, &ledger)
        else {
            panic!("abort must leave the same forced outcome immediately retryable");
        };
        director.commit(retry);
        assert_eq!(director.forced_count(JobDomain::Cargo, TEST_PROBLEM), 0);
    }

    #[test]
    fn apply_failure_after_a_permit_preserves_the_forced_outcome() {
        let mut director = DepartmentProblemDirector::default();
        director.reset_domain(test_policy());
        director.force_next(JobDomain::Cargo, TEST_PROBLEM);
        let mut ledger = IncidentLedger::default();
        let subject = Entity::from_bits(11);
        let stable = stability(100.0, crate::instability::StabilityBand::Stable);
        let ProblemPermitDecision::Permit(permit) =
            director.request_permit(candidate(subject), &stable, &ledger)
        else {
            panic!("the forced candidate should receive a permit");
        };

        let competing = ledger
            .create(
                IncidentKind::Burn,
                JobDomain::Medical,
                subject,
                None,
                Vec3::ZERO,
                Normalized::ONE,
                0.0,
            )
            .unwrap();
        assert_eq!(
            ledger.create(
                IncidentKind::Burn,
                JobDomain::Cargo,
                subject,
                None,
                Vec3::ZERO,
                Normalized::ONE,
                0.0,
            ),
            Err(IncidentError::DuplicateActiveCase)
        );
        director.abort(permit);
        assert_eq!(director.forced_count(JobDomain::Cargo, TEST_PROBLEM), 1);

        ledger.resolve(competing).unwrap();
        let ProblemPermitDecision::Permit(retry) =
            director.request_permit(candidate(subject), &stable, &ledger)
        else {
            panic!("the forced outcome should remain retryable after apply failure");
        };
        director.abort(retry);
    }

    #[test]
    fn active_case_cap_blocks_without_consuming_a_forced_outcome() {
        let mut director = DepartmentProblemDirector::default();
        director.reset_domain(test_policy());
        director.force_next(JobDomain::Cargo, TEST_PROBLEM);
        let mut ledger = IncidentLedger::default();
        let active = ledger
            .create(
                IncidentKind::Burn,
                JobDomain::Cargo,
                Entity::from_bits(20),
                None,
                Vec3::ZERO,
                Normalized::ONE,
                0.0,
            )
            .unwrap();
        let stable = stability(100.0, crate::instability::StabilityBand::Stable);

        assert!(matches!(
            director.request_permit(candidate(Entity::from_bits(21)), &stable, &ledger),
            ProblemPermitDecision::Suppressed(ProblemSuppression::UnresolvedCap)
        ));
        assert_eq!(director.forced_count(JobDomain::Cargo, TEST_PROBLEM), 1);

        ledger.resolve(active).unwrap();
        let ProblemPermitDecision::Permit(retry) =
            director.request_permit(candidate(Entity::from_bits(21)), &stable, &ledger)
        else {
            panic!("resolving the active case should make the forced outcome eligible");
        };
        director.abort(retry);
    }

    #[test]
    fn committed_permits_start_shared_domain_and_actor_cooldowns() {
        let mut director = DepartmentProblemDirector::default();
        director.reset_domain(test_policy());
        director.force_next(JobDomain::Cargo, TEST_PROBLEM);
        let ledger = IncidentLedger::default();
        let subject = Entity::from_bits(30);
        let stable = stability(100.0, crate::instability::StabilityBand::Stable);
        let ProblemPermitDecision::Permit(permit) =
            director.request_permit(candidate(subject), &stable, &ledger)
        else {
            panic!("the forced candidate should receive a permit");
        };
        director.commit(permit);

        assert!(matches!(
            director.request_permit(candidate(Entity::from_bits(31)), &stable, &ledger),
            ProblemPermitDecision::Suppressed(ProblemSuppression::DomainCooldown)
        ));
        assert!(matches!(
            director.request_permit(candidate(Entity::from_bits(32)), &stable, &ledger),
            ProblemPermitDecision::Suppressed(ProblemSuppression::DomainCooldown)
        ));
        assert!(matches!(
            director.request_permit(candidate(subject), &stable, &ledger),
            ProblemPermitDecision::Suppressed(ProblemSuppression::ActorCooldown)
        ));
    }

    #[test]
    fn forced_outcomes_cannot_bypass_the_per_shift_cap() {
        let mut policy = test_policy();
        policy.domain_cooldown = 0;
        policy.actor_cooldown = 0;
        policy.shift_cap = 1;
        let mut director = DepartmentProblemDirector::default();
        director.reset_domain(policy);
        director.force_next(JobDomain::Cargo, TEST_PROBLEM);
        let ledger = IncidentLedger::default();
        let stable = stability(100.0, crate::instability::StabilityBand::Stable);
        let ProblemPermitDecision::Permit(first) =
            director.request_permit(candidate(Entity::from_bits(40)), &stable, &ledger)
        else {
            panic!("the first forced outcome should receive a permit");
        };
        director.commit(first);

        director.force_next(JobDomain::Cargo, TEST_PROBLEM);
        assert!(matches!(
            director.request_permit(candidate(Entity::from_bits(41)), &stable, &ledger),
            ProblemPermitDecision::Suppressed(ProblemSuppression::ShiftCap)
        ));
        assert_eq!(director.forced_count(JobDomain::Cargo, TEST_PROBLEM), 1);
    }

    #[test]
    fn one_forced_outcome_cannot_back_two_outstanding_permits() {
        let mut policy = test_policy();
        policy.domain_cooldown = 0;
        policy.actor_cooldown = 0;
        policy.unresolved_cap = 2;
        let mut director = DepartmentProblemDirector::default();
        director.reset_domain(policy);
        director.force_next(JobDomain::Cargo, TEST_PROBLEM);
        let ledger = IncidentLedger::default();
        let stable = stability(100.0, crate::instability::StabilityBand::Stable);
        let mut first_candidate = candidate(Entity::from_bits(50));
        first_candidate.base_risk = Normalized::ZERO;
        let ProblemPermitDecision::Permit(first) =
            director.request_permit(first_candidate, &stable, &ledger)
        else {
            panic!("the queued forced outcome should back the first permit");
        };
        let mut second_candidate = candidate(Entity::from_bits(51));
        second_candidate.base_risk = Normalized::ZERO;
        assert!(matches!(
            director.request_permit(second_candidate, &stable, &ledger),
            ProblemPermitDecision::Suppressed(ProblemSuppression::RollMiss)
        ));
        director.abort(first);
        assert_eq!(director.forced_count(JobDomain::Cargo, TEST_PROBLEM), 1);
    }

    #[test]
    fn candidate_order_is_stable_independent_of_submission_order() {
        let low = candidate(Entity::from_bits(2));
        let high = candidate(Entity::from_bits(9));
        let mut candidates = vec![high.clone(), low.clone()];
        candidates.sort_by(DepartmentProblemCandidate::deterministic_cmp);
        assert_eq!(candidates, vec![low, high]);
    }

    #[test]
    fn duplicate_active_incidents_for_one_subject_are_rejected() {
        let mut ledger = IncidentLedger::default();
        let subject = Entity::from_bits(4);
        let first = ledger
            .create(
                IncidentKind::Burn,
                JobDomain::Cargo,
                subject,
                None,
                Vec3::X,
                Normalized::new(0.4).unwrap(),
                12.0,
            )
            .unwrap();
        assert_eq!(
            ledger.create(
                IncidentKind::Burn,
                JobDomain::Cargo,
                subject,
                None,
                Vec3::Y,
                Normalized::new(0.8).unwrap(),
                13.0,
            ),
            Err(IncidentError::DuplicateActiveCase),
        );
        ledger.resolve(first).unwrap();
        assert!(ledger
            .create(
                IncidentKind::Burn,
                JobDomain::Cargo,
                subject,
                None,
                Vec3::Y,
                Normalized::new(0.8).unwrap(),
                13.0,
            )
            .is_ok());
    }

    #[test]
    fn resolved_history_can_be_compacted_without_touching_active_cases() {
        let mut ledger = IncidentLedger::default();
        let first = ledger
            .create(
                IncidentKind::Burn,
                JobDomain::Cargo,
                Entity::from_bits(1),
                None,
                Vec3::ZERO,
                Normalized::ONE,
                0.0,
            )
            .unwrap();
        ledger
            .create(
                IncidentKind::EquipmentFailure,
                JobDomain::Engineering,
                Entity::from_bits(2),
                None,
                Vec3::ZERO,
                Normalized::ONE,
                0.0,
            )
            .unwrap();
        ledger.resolve(first).unwrap();

        assert_eq!(ledger.retain_active(), 1);
        assert_eq!(ledger.active().count(), 1);
        assert_eq!(ledger.active_in_domain(JobDomain::Engineering).count(), 1);
        assert_eq!(ledger.active_in_domain(JobDomain::Cargo).count(), 0);
        assert!(ledger.get(first).is_none());
    }
}
