//! Shared boundary between authored department rosters and utility control.
//!
//! This module intentionally does not know any job names, work resources, or
//! consequences. A department declares who belongs to it and what they can do;
//! its own adapter still publishes tickets and resolves domain state.

use bevy::prelude::*;

use super::jobs::{JobCapability, JobDomain, NarrativeTier, NpcJobProfile};
use super::{
    stable_text_key, ControlOwner, LocomotionOwner, NpcActivity, UtilityAgent, UtilityControlBundle,
};
use crate::crew::CrewRoute;

#[derive(Clone, Copy, Debug)]
pub struct DepartmentRoster {
    pub domain: JobDomain,
    pub core: &'static [&'static str],
    pub support: &'static [&'static str],
    pub expected_support: usize,
    pub capabilities: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepartmentRosterError {
    CoreCount { actual: usize },
    SupportCount { expected: usize, actual: usize },
    EmptyName,
    DuplicateName(&'static str),
    EmptyCapability,
}

impl DepartmentRoster {
    pub fn validate(self) -> Result<(), DepartmentRosterError> {
        if self.core.len() != 2 {
            return Err(DepartmentRosterError::CoreCount {
                actual: self.core.len(),
            });
        }
        if self.support.len() != self.expected_support {
            return Err(DepartmentRosterError::SupportCount {
                expected: self.expected_support,
                actual: self.support.len(),
            });
        }
        let names: Vec<_> = self
            .core
            .iter()
            .chain(self.support.iter())
            .copied()
            .collect();
        for (index, name) in names.iter().enumerate() {
            if name.trim().is_empty() {
                return Err(DepartmentRosterError::EmptyName);
            }
            if names[..index].contains(name) {
                return Err(DepartmentRosterError::DuplicateName(name));
            }
        }
        if self.capabilities.is_empty()
            || self
                .capabilities
                .iter()
                .any(|capability| capability.trim().is_empty())
        {
            return Err(DepartmentRosterError::EmptyCapability);
        }
        Ok(())
    }

    pub fn profile_for(self, name: &str) -> Option<NpcJobProfile> {
        let narrative_tier = if self.core.contains(&name) {
            NarrativeTier::Core
        } else if self.support.contains(&name) {
            NarrativeTier::Support
        } else {
            return None;
        };
        Some(NpcJobProfile::new(
            self.domain,
            narrative_tier,
            self.capabilities
                .iter()
                .map(|capability| JobCapability::new(*capability)),
        ))
    }
}

/// Builds the shared controller state for a resident still owned by the legacy
/// ambient controller. The caller owns component insertion so department-local
/// markers and state never leak into this shared boundary.
pub fn control_for_resident(
    name: &str,
    route: &CrewRoute,
    owner: Option<&ControlOwner>,
    roster: DepartmentRoster,
) -> Option<(UtilityControlBundle, NpcJobProfile)> {
    let profile = roster.profile_for(name)?;
    if owner.is_some_and(|owner| *owner != ControlOwner::LegacyAmbient) {
        return None;
    }
    let seed = stable_text_key(name);
    let mut control = UtilityControlBundle::new(UtilityAgent::new(seed, (seed % 500) as u16));
    if route.is_moving() {
        control.locomotion = LocomotionOwner::CrewRoute;
        control.activity = NpcActivity::Traveling;
    }
    Some((control, profile))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORE: &[&str] = &["Core One", "Core Two"];
    const SUPPORT: &[&str] = &["Support One", "Support Two"];
    const CAPABILITIES: &[&str] = &["test.operations"];
    const ROSTER: DepartmentRoster = DepartmentRoster {
        domain: JobDomain::Botany,
        core: CORE,
        support: SUPPORT,
        expected_support: 2,
        capabilities: CAPABILITIES,
    };

    #[test]
    fn roster_validation_pins_two_core_people_and_declared_support_count() {
        assert_eq!(ROSTER.validate(), Ok(()));
        assert_eq!(
            DepartmentRoster {
                core: &["Only One"],
                ..ROSTER
            }
            .validate(),
            Err(DepartmentRosterError::CoreCount { actual: 1 }),
        );
        assert_eq!(
            DepartmentRoster {
                support: &["Support One"],
                ..ROSTER
            }
            .validate(),
            Err(DepartmentRosterError::SupportCount {
                expected: 2,
                actual: 1,
            }),
        );
        assert_eq!(
            DepartmentRoster {
                support: &["Core One", "Support Two"],
                ..ROSTER
            }
            .validate(),
            Err(DepartmentRosterError::DuplicateName("Core One")),
        );
    }

    #[test]
    fn profile_tier_changes_content_eligibility_not_simulation_contracts() {
        let core = ROSTER.profile_for("Core One").unwrap();
        let support = ROSTER.profile_for("Support One").unwrap();
        assert_eq!(core.primary, JobDomain::Botany);
        assert_eq!(core.narrative_tier, NarrativeTier::Core);
        assert_eq!(support.narrative_tier, NarrativeTier::Support);
        assert_eq!(core.capabilities, support.capabilities);
        assert!(ROSTER.profile_for("Visitor").is_none());
    }

    #[test]
    fn migration_preserves_motion_and_rejects_a_competing_owner() {
        let route = CrewRoute::to(Vec3::X);
        let (control, _) = control_for_resident("Core One", &route, None, ROSTER).unwrap();
        assert_eq!(control.locomotion, LocomotionOwner::CrewRoute);
        assert_eq!(control.activity, NpcActivity::Traveling);
        assert!(control_for_resident(
            "Core One",
            &route,
            Some(&ControlOwner::MedicalTransport),
            ROSTER,
        )
        .is_none());
    }
}
