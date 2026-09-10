//! Bodies for the characters who only existed as scripts.
//!
//! Tech Boyle and Grower Aleksy each have an authored thread — a visit cadence,
//! a list of asks, a consequence for being ignored. What they did not have is a
//! life between those beats. They arrived, made their scene, and left. From the
//! player's chair a character who only ever appears to cause trouble is an
//! event, not an inhabitant, and an event cannot be watched, anticipated, or
//! caught in the act.
//!
//! This module gives them the same ordinary existence the department crew
//! already have: a standing post, a job profile, and a place in the utility
//! scheduler. Between their authored beats they do real work — the same tickets,
//! the same reservations, the same travel, the same needs. That is the whole
//! point. An antagonist who has been restocking the greenhouse all shift is
//! someone the player has already met when the ask finally comes.
//!
//! ## Why they are not on the roster
//!
//! Two registries already exist and neither one fits:
//!
//! - `station.crew.ron` is also the random customer pool. Adding these names
//!   would let an ordinary order be addressed to them, which their threads
//!   assume cannot happen — and existing tests assert they are absent.
//! - The department rosters pin exactly two core members and a declared support
//!   count ([`DepartmentRoster::validate`]). Adding a third core member to
//!   Engineering is a different change with different consequences.
//!
//! So this is a third, deliberately small registry. It borrows the *shape* of a
//! department roster — a name, a home domain, a profile — without joining one.
//!
//! ## Standing positions
//!
//! [`super::department::DepartmentRoster::standing_slot`] ranks a resident among
//! their department so everyone falling back to a shared point gets their own
//! place on a ring around it. A scripted resident is not in that listing, so it
//! would rank `None` and inherit slot zero — standing exactly where the
//! department's first core member stands. [`scripted_standing_slot`] therefore
//! allocates ranks *after* the department's own, extending the same ring rather
//! than colliding with it.

use bevy::prelude::*;

use super::department::DepartmentRoster;
use super::jobs::{JobCapability, JobDomain, NarrativeTier, NpcJobProfile};
use super::{
    stable_text_key, ControlOwner, LocomotionOwner, NpcActivity, UtilityAgent, UtilityControlBundle,
};
use crate::crew::{Ambient, CrewMember, CrewRoute, StationResident};

/// The shared assistant work profile, as capability names.
///
/// Both characters get the same list. They are not specialists borrowed by
/// other departments; they are the pair of hands a short-staffed station puts
/// wherever hands are needed, which is exactly what makes their presence in any
/// given room unremarkable.
///
/// Three deliberate omissions:
///
/// - **`botany.tend`** also authorises quarantine cleanup — `BotanyJobKind`
///   maps `TendPlot` and `CleanQuarantine` onto the same capability. Handling a
///   quarantined plot is not assistant work.
/// - **`cargo.operations`** is a single coarse capability covering the whole
///   freight loop, so granting it would hand over more than this profile should
///   acquire in one pass.
/// - **Medical, Security and Bridge** entirely. Those departments' work carries
///   authority the station has not given these two.
///
/// Allowed work keeps its ordinary accident risks. This profile is a set of
/// qualifications, not an exemption from station chemistry.
const ASSISTANT_BOTANY: &[&str] = &["botany.inspect", "botany.irrigate", "botany.harvest"];
const ASSISTANT_SERVICE: &[&str] = &[
    "service.ingredient_intake",
    "service.serve",
    "service.host",
    "service.clean",
];
const ASSISTANT_ENGINEERING: &[&str] = &["engineering.inspect", "engineering.maintain"];

/// One character who has an authored thread and now also has a life.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScriptedResident {
    /// Matched against `CrewMember.name`. The authored scripts own this string;
    /// this registry only borrows it.
    pub name: &'static str,
    /// Where they stand when there is nothing to do, and which department's
    /// ring their fallback position extends.
    pub home: JobDomain,
}

/// Every scripted character embodied by this module.
///
/// Bounded on purpose. This is not a general mechanism for turning scripts into
/// residents — it is two named characters whose threads were audited, and
/// adding a third is a decision with its own consequences, not a line item.
pub const SCRIPTED_RESIDENTS: &[ScriptedResident] = &[
    ScriptedResident {
        name: "Tech Boyle",
        home: JobDomain::Engineering,
    },
    ScriptedResident {
        name: "Grower Aleksy",
        home: JobDomain::Botany,
    },
];

/// The scripted resident with this name, if this is one of them.
pub fn scripted_resident(name: &str) -> Option<ScriptedResident> {
    SCRIPTED_RESIDENTS
        .iter()
        .copied()
        .find(|resident| resident.name == name)
}

/// A standing slot that extends a department's ring instead of colliding with it.
///
/// Returns the same `(rank, total)` shape [`DepartmentRoster::standing_slot`]
/// does, with ranks allocated after every roster member and a total that counts
/// the scripted residents sharing that home. Without this both a scripted
/// resident and the department's first core member rank zero and stand on the
/// same ray — the bug `standing_slot`'s own doc records from the hashing era,
/// reintroduced by a different route.
pub fn scripted_standing_slot(name: &str, roster: DepartmentRoster) -> Option<(usize, usize)> {
    let resident = scripted_resident(name)?;
    if resident.home != roster.domain {
        return None;
    }
    let roster_size = roster.core.len() + roster.support.len();
    let sharing: Vec<&ScriptedResident> = SCRIPTED_RESIDENTS
        .iter()
        .filter(|other| other.home == roster.domain)
        .collect();
    let offset = sharing
        .iter()
        .position(|other| other.name == name)
        .expect("the resident was found in the same registry above");
    Some((roster_size + offset, roster_size + sharing.len()))
}

/// The shared assistant profile for one scripted resident.
///
/// Their home domain is primary, the other two are cross-training. The existing
/// [`NpcJobProfile::with_cross_training`] builder already dedupes and skips the
/// primary, so the same three-domain list is correct for both characters and
/// produces the right shape for each.
///
/// [`NarrativeTier::Support`] is content eligibility only — it keeps them out of
/// content that addresses a department's core cast. It changes no simulation
/// contract; they score, travel, tire and get hurt exactly like anyone else.
pub fn assistant_profile(name: &str) -> Option<NpcJobProfile> {
    let resident = scripted_resident(name)?;
    let capabilities = ASSISTANT_BOTANY
        .iter()
        .chain(ASSISTANT_SERVICE.iter())
        .chain(ASSISTANT_ENGINEERING.iter())
        .map(|capability| JobCapability::new(*capability));
    Some(
        NpcJobProfile::new(resident.home, NarrativeTier::Support, capabilities)
            .with_cross_training([JobDomain::Botany, JobDomain::Service, JobDomain::Engineering]),
    )
}

/// Builds utility control for a scripted resident the legacy controller still owns.
///
/// Mirrors [`super::department::control_for_resident`], including its refusal to
/// take a body some other controller is already driving. It is a separate
/// function rather than a parameter on that one because the department version
/// derives its profile from a roster this character is deliberately absent from.
pub fn control_for_scripted_resident(
    name: &str,
    route: &CrewRoute,
    owner: Option<&ControlOwner>,
) -> Option<(UtilityControlBundle, NpcJobProfile)> {
    let profile = assistant_profile(name)?;
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

/// Puts the scripted residents under utility control once they have bodies.
///
/// Deliberately the same shape as the department activation systems: it adopts
/// whatever body already exists rather than spawning one. The spawning is the
/// authored threads' job — they recall or spawn through
/// `crew::recall_or_spawn_crew_member`, which already refuses to clone a name
/// that is present but busy. This system only decides that an existing body
/// should now be making its own decisions.
///
/// Inserting `UtilityAgent` is also what removes them from the legacy
/// controller: `crew::ambient_behaviour` queries `Without<UtilityAgent>`, so the
/// entity-disjoint rule holds without a second exclusion list to keep in sync.
///
/// Inserting `StationResident` is the part that had to be sequenced. It is what
/// stops `walk_route` despawning them when they are sent away, but it also
/// changes what other queries can see: `saboteur`'s trigger filtered on
/// `Without<StationResident>` and would have stopped matching Boyle here, with
/// no error and no failing test. That query was fixed in the same change that
/// made this live — see `saboteur::handle_saboteur_resolution` and its guard,
/// `the_thread_still_fires_for_a_tech_who_lives_here`.
fn activate_scripted_residents(
    mut commands: Commands,
    residents: Query<
        (
            Entity,
            &CrewMember,
            &crate::crew::CrewRoute,
            Option<&ControlOwner>,
        ),
        (
            With<Ambient>,
            Without<UtilityAgent>,
            Without<crate::social::NpcCommitment>,
        ),
    >,
) {
    for (entity, member, route, owner) in &residents {
        let Some((control, profile)) = control_for_scripted_resident(&member.name, route, owner)
        else {
            continue;
        };
        commands
            .entity(entity)
            .insert((control, profile, StationResident));
    }
}

pub(super) fn register(app: &mut App) {
    app.add_systems(
        PreUpdate,
        activate_scripted_residents
            .run_if(crate::net::is_authority)
            .run_if(in_state(crate::AppState::Playing))
            .run_if(crate::session::career_session),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_scripted_characters_share_one_assistant_profile() {
        let boyle = assistant_profile("Tech Boyle").expect("Boyle is a scripted resident");
        let aleksy = assistant_profile("Grower Aleksy").expect("Aleksy is a scripted resident");

        // Same qualifications, different homes: the profile is shared, the
        // identity is not.
        assert_eq!(boyle.capabilities, aleksy.capabilities);
        assert_eq!(boyle.primary, JobDomain::Engineering);
        assert_eq!(aleksy.primary, JobDomain::Botany);

        for profile in [&boyle, &aleksy] {
            for domain in [JobDomain::Botany, JobDomain::Service, JobDomain::Engineering] {
                assert!(
                    profile.works_in(domain),
                    "an assistant should work in {domain:?}",
                );
            }
        }
        assert!(assistant_profile("Dr. Vance").is_none());
    }

    /// The three omissions are load-bearing, so they are asserted rather than
    /// left to the const list to imply.
    #[test]
    fn the_assistant_profile_withholds_quarantine_cargo_and_authority_work() {
        let profile = assistant_profile("Tech Boyle").expect("Boyle is a scripted resident");
        for withheld in [
            // Also authorises CleanQuarantine.
            "botany.tend",
            // One coarse capability covering the whole freight loop.
            "cargo.operations",
            "medical.response",
            "security.patrol",
            "security.interview",
            "bridge.helm",
        ] {
            assert!(
                !profile.can_do(&JobCapability::new(withheld)),
                "an assistant must not hold {withheld}",
            );
        }
        for domain in [JobDomain::Medical, JobDomain::Security, JobDomain::Bridge] {
            assert!(
                !profile.works_in(domain),
                "an assistant must not work in {domain:?}",
            );
        }
    }

    /// The collision `standing_slot`'s own doc warns about, arriving by a
    /// different route: a name absent from the roster ranks `None`, and every
    /// caller that treats `None` as zero puts them on the first core member's
    /// ray.
    #[test]
    fn a_scripted_resident_stands_past_the_department_roster_not_on_top_of_it() {
        let botany = super::super::roster_of(JobDomain::Botany);
        let roster_size = botany.core.len() + botany.support.len();

        assert_eq!(botany.standing_slot("Grower Aleksy"), None);
        let (rank, total) =
            scripted_standing_slot("Grower Aleksy", botany).expect("Aleksy calls Botany home");
        assert_eq!(rank, roster_size, "ranks continue past the roster");
        assert!(total > roster_size, "the ring grew to fit him");

        for member in botany.core.iter().chain(botany.support.iter()) {
            let (member_rank, _) = botany
                .standing_slot(member)
                .expect("a roster member ranks within their own department");
            assert_ne!(member_rank, rank, "{member} would share Aleksy's place");
        }
    }

    #[test]
    fn a_scripted_resident_only_extends_its_own_departments_ring() {
        let engineering = super::super::roster_of(JobDomain::Engineering);
        assert!(scripted_standing_slot("Tech Boyle", engineering).is_some());
        // Boyle works in Botany but does not stand there when idle.
        assert!(
            scripted_standing_slot("Tech Boyle", super::super::roster_of(JobDomain::Botany))
                .is_none()
        );
        assert!(scripted_standing_slot("Dr. Vance", engineering).is_none());
    }

    #[test]
    fn activation_never_takes_a_body_another_controller_is_driving() {
        let route = CrewRoute::to(Vec3::X);
        let (control, _) = control_for_scripted_resident("Tech Boyle", &route, None)
            .expect("an unowned body can be adopted");
        assert_eq!(control.locomotion, LocomotionOwner::CrewRoute);
        assert_eq!(control.activity, NpcActivity::Traveling);

        for owner in [
            ControlOwner::MedicalTransport,
            ControlOwner::Pursuit,
            ControlOwner::OrderVisit,
            ControlOwner::Incapacitated,
        ] {
            assert!(
                control_for_scripted_resident("Tech Boyle", &route, Some(&owner)).is_none(),
                "{owner:?} still owns the body",
            );
        }
    }
}
