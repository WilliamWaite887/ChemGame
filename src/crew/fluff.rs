//! The people who make the station look staffed.
//!
//! The Bridge is the most heavily furnished room in the map — seventeen console
//! arcs, twelve viewscreens, a captain's chair — and until this module existed
//! nobody had ever stood in it. It has a `department_spot`, but no role on
//! `station.crew.ron` matches it, so `populate_departments` had nobody to put
//! there. The room was a set, not a place.
//!
//! These support workers are ordinary residents in every physical respect that
//! matters: a `Body`, a `Bloodstream`, navigation, and an `Interactable`. They
//! stay off the core customer roster for the reason [`FLUFF_CREW`] documents.

use bevy::prelude::*;

use super::{
    spawn_crew_member, Ambient, CrewDef, CrewMember, CrewPosts, CrewRoute, Departments,
    StationResident, DWELL_SECONDS,
};
use crate::interaction::Interactable;

pub(crate) const CARGO_SUPPORT_NAMES: [&str; 2] = ["Loader Bell", "Clerk Nwosu"];
pub(crate) const MEDICAL_SUPPORT_NAMES: [&str; 2] = ["Paramedic Hale", "Orderly Imani"];
pub(crate) const BOTANY_SUPPORT_NAMES: [&str; 2] = ["Grower Chen", "Technician Mbatha"];
pub(crate) const ENGINEERING_SUPPORT_NAMES: [&str; 2] = ["Mechanic Torres", "Systems Tech Adeyemi"];
pub(crate) const SERVICE_SUPPORT_NAMES: [&str; 2] = ["Cook Navarro", "Attendant Mensah"];
pub(crate) const SECURITY_SUPPORT_NAMES: [&str; 2] = ["Patrol Officer Dlamini", "Dispatcher Novak"];
pub(crate) const BRIDGE_SUPPORT_NAMES: [&str; 4] = [
    "Ensign Park",
    "Ensign Alvarez",
    "Operator Fenn",
    "Operator Ruiz",
];

/// The off-roster cast, as `(name, role)`.
///
/// **Deliberately not in `assets/data/station.crew.ron`.** That file is the
/// *customer* roster: `orders::generate_orders` and `generate_specific_orders`
/// draw from it, so anyone added there starts phoning the lab for chemistry.
/// Building the `CrewDef` here instead is the same thing `cult::place_guard`
/// does with its own off-roster guards, and it is safe for the same reasons:
///
/// - not on the roster, so never selected as an order customer;
/// - `social::RESIDENT_NAMES` is the core cast and `social::resident_department`
///   returns `None` for anyone else, so the favor and relationship systems never
///   see them;
/// - `Department::members()` lists only the two core names per department, and
///   `Shift::standing` averages over *that* list, so a support worker never
///   moves a department's standing. This used to read "`from_role` returns
///   `None` for Bridge" — no longer true now that Bridge is a real department,
///   which is exactly why the guarantee is stated against `members()` instead:
///   role strings stopped being the thing that keeps support workers out;
/// - `CrewAssets::theme_for` falls through to the neutral uniform.
///
/// Every role here **must** name a `department_spot` in the map. A role with no
/// [`Departments`] entry gets `home() == None`, which leaves that body inert
/// during a crisis and unroutable home by `somewhere_else` — it would stand
/// still while the station burned. `every_fluff_role_has_a_department_spot`
/// pins this. That is why the two Bridge Operations officers are `"Bridge"`
/// too: the room they stand in is decided by their duty post, not their role.
const FLUFF_CREW: &[(&str, &str)] = &[
    (BRIDGE_SUPPORT_NAMES[0], "Bridge"),
    (BRIDGE_SUPPORT_NAMES[1], "Bridge"),
    (BRIDGE_SUPPORT_NAMES[2], "Bridge"),
    (BRIDGE_SUPPORT_NAMES[3], "Bridge"),
    (CARGO_SUPPORT_NAMES[0], "Cargo"),
    (CARGO_SUPPORT_NAMES[1], "Cargo"),
    (MEDICAL_SUPPORT_NAMES[0], "Medical"),
    (MEDICAL_SUPPORT_NAMES[1], "Medical"),
    (BOTANY_SUPPORT_NAMES[0], "Botany"),
    (BOTANY_SUPPORT_NAMES[1], "Botany"),
    (ENGINEERING_SUPPORT_NAMES[0], "Engineering"),
    (ENGINEERING_SUPPORT_NAMES[1], "Engineering"),
    (SERVICE_SUPPORT_NAMES[0], "Service"),
    (SERVICE_SUPPORT_NAMES[1], "Service"),
    (SECURITY_SUPPORT_NAMES[0], "Security"),
    (SECURITY_SUPPORT_NAMES[1], "Security"),
];

/// Uniform tint. Presentation only — `CrewAssets::theme_for` picks the actual
/// model, and an unrecognised role lands on the neutral one.
const FLUFF_COLOR: [f32; 3] = [0.62, 0.66, 0.74];

/// Gives the Bridge and each currently migrated department their support cast.
///
/// Registered beside `populate_departments` and under the *same* run conditions,
/// deliberately: the long comment on that registration describes a real bug
/// where pairing change-detection with `MapReady` meant no ambient crew ever
/// spawned at all. This system has the same shape and would have the same bug.
pub(super) fn populate_fluff_posts(
    mut commands: Commands,
    departments: Res<Departments>,
    crew_posts: Res<CrewPosts>,
    existing: Query<&CrewMember, With<StationResident>>,
) {
    for (name, role) in FLUFF_CREW {
        // Bridge support uses communal duty posts. Department support starts at
        // its department point until utility selection sends it to real work.
        // Falling back rather than skipping keeps the station populated even
        // in a build whose map authors no duty posts yet, exactly as
        // `populate_departments` falls back from a work post.
        let duty = (*role == "Bridge")
            .then(|| crew_posts.random_duty())
            .flatten();
        let Some(home) = duty.or_else(|| departments.home(role)) else {
            continue;
        };
        if existing.iter().any(|member| member.name == *name) {
            continue;
        }

        let def = CrewDef {
            name: (*name).to_string(),
            role: (*role).to_string(),
            color: FLUFF_COLOR,
        };
        let crew = spawn_crew_member(&mut commands, &def, 0.0);
        commands.entity(crew).insert((
            Ambient::new(rand::random_range(DWELL_SECONDS.0..=DWELL_SECONDS.1)),
            // They are not coming to the counter.
            CrewRoute::to(home),
            // Nothing else can re-create them: unlike a resident, no order flow
            // draws these names, so a body that walks off the station is gone
            // for the rest of the save and its department quietly empties out.
            StationResident,
            Interactable::new(format!("{name} — {role}")),
        ));
    }
}

#[cfg(test)]
mod tests {
    //! These all guard the same thing from different sides: fluff crew are
    //! inhabitants of the station, and nothing else.

    use super::*;

    #[test]
    fn no_fluff_officer_is_on_the_customer_roster() {
        // The canary for someone later "tidying" them into
        // `station.crew.ron`. That file is what `orders::generate_orders`
        // draws customers from, so a name in both places is a bridge officer
        // phoning the lab for chemistry.
        let roster: Vec<CrewDef> =
            ron::from_str(include_str!("../../assets/data/station.crew.ron")).unwrap();
        for (name, _) in FLUFF_CREW {
            assert!(
                !roster.iter().any(|member| member.name == *name),
                "{name} is fluff but also on the customer roster",
            );
        }
    }

    #[test]
    fn support_workers_never_join_the_core_social_roster() {
        for (name, role) in FLUFF_CREW {
            assert!(
                crate::social::resident_department(name).is_none(),
                "{name} is a social resident, so the favor system would target them",
            );
            assert!(
                !crate::social::RESIDENT_NAMES.contains(name),
                "{name} is in RESIDENT_NAMES, which is reserved for core characters",
            );
            assert!(!role.trim().is_empty());
        }
    }

    #[test]
    fn every_fluff_role_has_somewhere_to_call_home() {
        // A role with no `department_spot` gets `Departments::home() == None`,
        // which is not a crash but is worse than one: that body stands still
        // through a crisis while everyone else responds, and
        // `somewhere_else` can never route it home either. This is exactly
        // why the Bridge Operations pair are role "Bridge" and not
        // "Bridge Operations" — there is no spot by that name.
        // `authored_department_home` panics with the role name if the map has
        // no spot for it, which is the assertion — the message it already
        // carries is better than one restated here.
        for (_, role) in FLUFF_CREW {
            let home = crate::lab::tb_map::authored_department_home(role);
            assert!(
                home.is_finite(),
                "'{role}' has a department_spot but not a usable position",
            );
        }
    }

    #[test]
    fn a_fluff_officer_is_never_a_crisis_responder() {
        // Which is correct, not a gap: bridge officers holding the Bridge
        // during a medical emergency is the intended behaviour. This pins it
        // so a later crisis edit that added "Bridge" to a responder list would
        // have to be a deliberate choice.
        let script: crate::crisis::CrisisScript =
            ron::from_str(include_str!("../../assets/data/station.crisis.ron")).unwrap();
        for case in &script.cases {
            for (_, role) in FLUFF_CREW.iter().filter(|(_, role)| *role == "Bridge") {
                assert!(
                    !case.responders.iter().any(|listed| listed == role),
                    "'{role}' is listed as a crisis responder but has no medical duty",
                );
            }
        }
    }
}
