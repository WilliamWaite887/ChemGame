//! Debug-only assertions for utility controller and locomotion ownership.
//!
//! Movement systems keep defensive guards so malformed state cannot move an
//! actor twice in release builds. In debug builds, however, silently leaving
//! that state inert would hide the broken handoff that created it. This module
//! therefore validates the settled ECS state after gameplay commands have
//! been applied and panics without attempting a repair.

use bevy::prelude::*;

use crate::crew::{CrewRoute, Errand};
use crate::showdown::Pursuit;

use super::medical::{MedicalPatient, MedicalTransportTask, TransportedPatient};
use super::{
    ActionPhase, ControlOwner, CurrentAction, LocomotionOwner, SuspendedUtilityControl,
    UtilityAgent,
};

pub(super) fn register(app: &mut App) {
    app.add_systems(
        PostUpdate,
        assert_utility_ownership_invariants.run_if(crate::net::is_authority),
    );
}

#[allow(clippy::type_complexity)]
fn assert_utility_ownership_invariants(
    agents: Query<
        (
            Entity,
            Option<&ControlOwner>,
            Option<&LocomotionOwner>,
            Has<CurrentAction>,
            Has<CrewRoute>,
            Has<Errand>,
            Has<Pursuit>,
            Has<MedicalPatient>,
            Has<TransportedPatient>,
            Has<MedicalTransportTask>,
            Option<&SuspendedUtilityControl>,
        ),
        With<UtilityAgent>,
    >,
    actions: Query<&CurrentAction>,
    routes: Query<&CrewRoute>,
    passengers: Query<
        (
            Entity,
            &MedicalPatient,
            &TransportedPatient,
            &ControlOwner,
            &LocomotionOwner,
        ),
        With<UtilityAgent>,
    >,
    responders: Query<
        (
            Entity,
            &MedicalTransportTask,
            &ControlOwner,
            &LocomotionOwner,
        ),
        With<UtilityAgent>,
    >,
) {
    for (
        entity,
        owner,
        locomotion,
        has_action,
        has_route,
        has_errand,
        has_pursuit,
        is_medical_patient,
        is_passenger,
        has_transport_task,
        suspended,
    ) in &agents
    {
        let owner = owner.unwrap_or_else(|| {
            panic!("utility ownership invariant: {entity:?} has no ControlOwner")
        });
        let locomotion = locomotion.unwrap_or_else(|| {
            panic!("utility ownership invariant: {entity:?} has no LocomotionOwner")
        });

        let locomotion_components =
            usize::from(has_route) + usize::from(has_errand) + usize::from(has_pursuit);
        assert!(
            locomotion_components <= 1,
            "utility ownership invariant: {entity:?} has incompatible locomotion components \
             (CrewRoute={has_route}, Errand={has_errand}, Pursuit={has_pursuit})"
        );
        assert!(
            !(is_medical_patient && has_transport_task),
            "utility ownership invariant: {entity:?} is both a Medical patient and responder"
        );
        assert!(
            !is_passenger || is_medical_patient,
            "utility ownership invariant: transported {entity:?} is not a Medical patient"
        );

        if has_action {
            assert_eq!(
                *owner,
                ControlOwner::UtilityAction,
                "utility ownership invariant: {entity:?} has CurrentAction under {owner:?}"
            );
            let action = actions
                .get(entity)
                .expect("Has<CurrentAction> and the action query must agree");
            match action.phase {
                ActionPhase::Traveling => assert!(
                    *locomotion == LocomotionOwner::Errand && has_errand,
                    "utility ownership invariant: traveling action on {entity:?} lacks its Errand owner"
                ),
                ActionPhase::Performing => assert!(
                    *locomotion == LocomotionOwner::None && !has_errand,
                    "utility ownership invariant: performing action on {entity:?} still owns Errand locomotion"
                ),
                phase => panic!(
                    "utility ownership invariant: {entity:?} retained unsettled CurrentAction phase {phase:?} after Update"
                ),
            }
        }

        match owner {
            ControlOwner::LegacyAmbient => {
                panic!("utility ownership invariant: {entity:?} is still owned by LegacyAmbient")
            }
            ControlOwner::UtilityAction => {
                assert!(
                    !is_medical_patient && !is_passenger && !has_transport_task && !has_pursuit,
                    "utility ownership invariant: UtilityAction actor {entity:?} carries another controller's state"
                );
                match locomotion {
                    LocomotionOwner::None => {
                        assert!(
                            !has_errand && !has_pursuit,
                            "utility ownership invariant: idle UtilityAction actor {entity:?} has active locomotion"
                        );
                        if let Ok(route) = routes.get(entity) {
                            assert!(
                                !route.is_moving(),
                                "utility ownership invariant: {entity:?} has a moving CrewRoute with no locomotion owner"
                            );
                        }
                    }
                    LocomotionOwner::CrewRoute => {
                        assert!(
                            has_route
                                && routes.get(entity).is_ok_and(CrewRoute::is_moving)
                                && !has_action,
                            "utility ownership invariant: CrewRoute-owned UtilityAction actor {entity:?} lacks an exclusive moving route"
                        );
                    }
                    LocomotionOwner::Errand => {
                        assert!(
                            has_errand && has_action,
                            "utility ownership invariant: Errand-owned UtilityAction actor {entity:?} lacks its action or errand"
                        );
                    }
                    other => panic!(
                        "utility ownership invariant: UtilityAction actor {entity:?} has illegal locomotion owner {other:?}"
                    ),
                }
            }
            ControlOwner::OrderVisit => {
                assert!(
                    *locomotion == LocomotionOwner::CrewRoute
                        && has_route
                        && !has_action
                        && !has_errand
                        && !has_pursuit
                        && !is_medical_patient
                        && !is_passenger
                        && !has_transport_task,
                    "utility ownership invariant: OrderVisit actor {entity:?} does not exclusively own CrewRoute"
                );
            }
            ControlOwner::ScriptedErrand => {
                let owns_route = *locomotion == LocomotionOwner::CrewRoute && has_route;
                let owns_errand = *locomotion == LocomotionOwner::Errand && has_errand;
                assert!(
                    (owns_route || owns_errand)
                        && !has_action
                        && !has_pursuit
                        && !is_medical_patient
                        && !is_passenger
                        && !has_transport_task,
                    "utility ownership invariant: ScriptedErrand actor {entity:?} has no exclusive route or errand"
                );
            }
            ControlOwner::Pursuit => {
                assert!(
                    *locomotion == LocomotionOwner::Pursuit
                        && has_pursuit
                        && !has_action
                        && !is_medical_patient
                        && !is_passenger
                        && !has_transport_task,
                    "utility ownership invariant: Pursuit actor {entity:?} does not exclusively own Pursuit locomotion"
                );
            }
            ControlOwner::MedicalTransport => {
                assert!(
                    !has_action && !has_pursuit,
                    "utility ownership invariant: Medical-owned actor {entity:?} carries unrelated action state"
                );
                if is_medical_patient {
                    assert!(
                        *locomotion == LocomotionOwner::None
                            && !has_route
                            && !has_errand
                            && !has_transport_task,
                        "utility ownership invariant: Medical patient {entity:?} has independent locomotion"
                    );
                } else {
                    assert!(
                        *locomotion == LocomotionOwner::MedicalTransport
                            && has_transport_task
                            && has_errand
                            && !has_route
                            && !is_passenger,
                        "utility ownership invariant: Medical responder {entity:?} lacks exclusive transport locomotion"
                    );
                }
            }
            ControlOwner::Incapacitated => {
                assert!(
                    *locomotion == LocomotionOwner::None && !has_action && !is_medical_patient,
                    "utility ownership invariant: incapacitated actor {entity:?} still has active control"
                );
                let suspended = suspended.unwrap_or_else(|| {
                    panic!(
                        "utility ownership invariant: incapacitated actor {entity:?} has no suspended controller"
                    )
                });
                assert_suspended_shape(
                    entity,
                    suspended,
                    has_route,
                    routes.get(entity).is_ok_and(CrewRoute::is_moving),
                    has_errand,
                    has_pursuit,
                    has_transport_task,
                );
            }
        }

        assert!(
            suspended.is_none() || *owner == ControlOwner::Incapacitated,
            "utility ownership invariant: active actor {entity:?} retains suspended control"
        );
    }

    for (patient_entity, patient, passenger, patient_owner, patient_locomotion) in &passengers {
        assert_eq!(
            *patient_owner,
            ControlOwner::MedicalTransport,
            "utility ownership invariant: transported patient {patient_entity:?} is not Medical-owned"
        );
        assert_eq!(
            *patient_locomotion,
            LocomotionOwner::None,
            "utility ownership invariant: transported patient {patient_entity:?} owns locomotion"
        );
        let Ok((responder_entity, task, responder_owner, responder_locomotion)) =
            responders.get(passenger.responder)
        else {
            panic!(
                "utility ownership invariant: transported patient {patient_entity:?} points to missing responder {:?}",
                passenger.responder
            );
        };
        assert_eq!(
            responder_entity, passenger.responder,
            "utility ownership invariant: responder query returned the wrong entity"
        );
        assert!(
            task.patient == patient_entity && task.case == patient.case,
            "utility ownership invariant: responder {responder_entity:?} does not point back to patient {patient_entity:?}"
        );
        assert!(
            (*responder_owner == ControlOwner::MedicalTransport
                && *responder_locomotion == LocomotionOwner::MedicalTransport)
                || (*responder_owner == ControlOwner::Incapacitated
                    && *responder_locomotion == LocomotionOwner::None),
            "utility ownership invariant: responder {responder_entity:?} has invalid suspended transport ownership"
        );
    }

    for (responder_entity, task, _, _) in &responders {
        let Ok((patient_entity, patient, passenger, _, _)) = passengers.get(task.patient) else {
            panic!(
                "utility ownership invariant: responder {responder_entity:?} points to missing transported patient {:?}",
                task.patient
            );
        };
        assert!(
            patient_entity == task.patient
                && passenger.responder == responder_entity
                && patient.case == task.case,
            "utility ownership invariant: responder {responder_entity:?} and patient {patient_entity:?} disagree"
        );
    }
}

fn assert_suspended_shape(
    entity: Entity,
    suspended: &SuspendedUtilityControl,
    has_route: bool,
    route_is_moving: bool,
    has_errand: bool,
    has_pursuit: bool,
    has_transport_task: bool,
) {
    let valid = match (suspended.owner, suspended.locomotion) {
        (ControlOwner::UtilityAction, LocomotionOwner::None) => {
            !route_is_moving && !has_errand && !has_pursuit && !has_transport_task
        }
        (
            ControlOwner::UtilityAction | ControlOwner::OrderVisit | ControlOwner::ScriptedErrand,
            LocomotionOwner::CrewRoute,
        ) => has_route && !has_transport_task,
        (ControlOwner::ScriptedErrand, LocomotionOwner::Errand) => {
            has_errand && !has_transport_task
        }
        (ControlOwner::Pursuit, LocomotionOwner::Pursuit) => has_pursuit && !has_transport_task,
        (ControlOwner::MedicalTransport, LocomotionOwner::MedicalTransport) => {
            has_transport_task && has_errand
        }
        _ => false,
    };
    assert!(
        valid,
        "utility ownership invariant: incapacitated actor {entity:?} cannot resume suspended {:?}/{:?}",
        suspended.owner, suspended.locomotion
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crew::{send_on_errand, ErrandGoal};
    use crate::utility_ai::{
        ActionKey, ActionResult, InterruptPolicy, NpcActivity, ReservationOwner, UtilityActionId,
        UtilityBucket, UtilityControlBundle,
    };

    fn app() -> App {
        let mut app = App::new();
        register(&mut app);
        app
    }

    fn control(owner: ControlOwner, locomotion: LocomotionOwner) -> UtilityControlBundle {
        let mut control = UtilityControlBundle::new(UtilityAgent::new(7, 0));
        control.control = owner;
        control.locomotion = locomotion;
        control.activity = if locomotion == LocomotionOwner::None {
            NpcActivity::Idle
        } else {
            NpcActivity::Traveling
        };
        control
    }

    fn action(phase: ActionPhase) -> CurrentAction {
        CurrentAction {
            key: ActionKey {
                action: UtilityActionId::MaintainPost,
                target_key: 9,
            },
            bucket: UtilityBucket::Routine,
            phase,
            target: None,
            reservation: None,
            reservation_capacity: 1,
            elapsed: 0.0,
            phase_elapsed: 0.0,
            perform_for: 1.0,
            minimum_commitment: 0.0,
            timeout: 10.0,
            interrupt_policy: InterruptPolicy::Emergency,
            instance: 1,
            result: (phase == ActionPhase::Resolving).then_some(ActionResult::Completed),
        }
    }

    fn give_errand(app: &mut App, entity: Entity) {
        let mut commands = app.world_mut().commands();
        send_on_errand(
            &mut commands,
            entity,
            ErrandGoal::Point(Vec3::new(4.0, 0.0, 0.0)),
        );
        app.world_mut().flush();
    }

    #[test]
    fn valid_controller_and_medical_transport_shapes_pass() {
        let mut app = app();
        app.world_mut().spawn((
            control(ControlOwner::UtilityAction, LocomotionOwner::None),
            CrewRoute::standing(),
            super::super::PendingEmergencyReplacement {
                action: action(ActionPhase::Selected),
            },
        ));
        app.world_mut().spawn((
            control(ControlOwner::UtilityAction, LocomotionOwner::None),
            action(ActionPhase::Performing),
        ));
        app.world_mut().spawn((
            control(ControlOwner::UtilityAction, LocomotionOwner::CrewRoute),
            CrewRoute::to(Vec3::X),
        ));
        app.world_mut().spawn((
            control(ControlOwner::OrderVisit, LocomotionOwner::CrewRoute),
            CrewRoute::standing(),
        ));
        app.world_mut().spawn((
            control(ControlOwner::ScriptedErrand, LocomotionOwner::CrewRoute),
            CrewRoute::to(Vec3::X),
        ));
        app.world_mut().spawn((
            control(ControlOwner::Pursuit, LocomotionOwner::Pursuit),
            Pursuit::new(2.0, 1.0, 5),
        ));
        app.world_mut().spawn((
            control(ControlOwner::Incapacitated, LocomotionOwner::None),
            CrewRoute::to(Vec3::X),
            SuspendedUtilityControl {
                owner: ControlOwner::OrderVisit,
                locomotion: LocomotionOwner::CrewRoute,
            },
        ));
        app.world_mut().spawn((
            control(ControlOwner::Incapacitated, LocomotionOwner::None),
            CrewRoute::standing(),
            SuspendedUtilityControl {
                owner: ControlOwner::UtilityAction,
                locomotion: LocomotionOwner::None,
            },
        ));

        let traveling = app
            .world_mut()
            .spawn((
                control(ControlOwner::UtilityAction, LocomotionOwner::Errand),
                action(ActionPhase::Traveling),
            ))
            .id();
        give_errand(&mut app, traveling);

        let scripted_errand = app
            .world_mut()
            .spawn(control(
                ControlOwner::ScriptedErrand,
                LocomotionOwner::Errand,
            ))
            .id();
        give_errand(&mut app, scripted_errand);

        app.world_mut().spawn((
            control(ControlOwner::MedicalTransport, LocomotionOwner::None),
            MedicalPatient {
                case: super::super::medical::MedicalCaseId(40),
            },
        ));

        let responder = app
            .world_mut()
            .spawn(control(
                ControlOwner::MedicalTransport,
                LocomotionOwner::MedicalTransport,
            ))
            .id();
        let patient = app
            .world_mut()
            .spawn((
                control(ControlOwner::MedicalTransport, LocomotionOwner::None),
                MedicalPatient {
                    case: super::super::medical::MedicalCaseId(41),
                },
                TransportedPatient { responder },
            ))
            .id();
        app.world_mut()
            .entity_mut(responder)
            .insert(MedicalTransportTask {
                case: super::super::medical::MedicalCaseId(41),
                patient,
                bed: "medical.bed.1".into(),
                bed_at: Vec3::ZERO,
                bed_claim: ReservationOwner {
                    agent: responder,
                    action_instance: 41,
                },
            });
        give_errand(&mut app, responder);

        app.update();
    }

    #[test]
    #[should_panic(expected = "has incompatible locomotion components")]
    fn route_and_errand_on_one_agent_panics() {
        let mut app = app();
        let entity = app
            .world_mut()
            .spawn((
                control(ControlOwner::UtilityAction, LocomotionOwner::Errand),
                action(ActionPhase::Traveling),
            ))
            .id();
        give_errand(&mut app, entity);
        app.world_mut()
            .entity_mut(entity)
            .insert(CrewRoute::standing());

        app.update();
    }

    #[test]
    #[should_panic(expected = "has CurrentAction under OrderVisit")]
    fn action_under_order_control_panics() {
        let mut app = app();
        app.world_mut().spawn((
            control(ControlOwner::OrderVisit, LocomotionOwner::CrewRoute),
            CrewRoute::standing(),
            action(ActionPhase::Performing),
        ));

        app.update();
    }

    #[test]
    #[should_panic(expected = "does not point back to patient")]
    fn mismatched_medical_transport_pair_panics() {
        let mut app = app();
        let responder = app
            .world_mut()
            .spawn(control(
                ControlOwner::MedicalTransport,
                LocomotionOwner::MedicalTransport,
            ))
            .id();
        let patient = app
            .world_mut()
            .spawn((
                control(ControlOwner::MedicalTransport, LocomotionOwner::None),
                MedicalPatient {
                    case: super::super::medical::MedicalCaseId(8),
                },
                TransportedPatient { responder },
            ))
            .id();
        let other_patient = app
            .world_mut()
            .spawn((
                control(ControlOwner::MedicalTransport, LocomotionOwner::None),
                MedicalPatient {
                    case: super::super::medical::MedicalCaseId(8),
                },
                TransportedPatient { responder },
            ))
            .id();
        app.world_mut()
            .entity_mut(responder)
            .insert(MedicalTransportTask {
                case: super::super::medical::MedicalCaseId(8),
                patient: other_patient,
                bed: "medical.bed.1".into(),
                bed_at: Vec3::ZERO,
                bed_claim: ReservationOwner {
                    agent: responder,
                    action_instance: 8,
                },
            });
        give_errand(&mut app, responder);

        assert_ne!(patient, other_patient);
        app.update();
    }

    #[test]
    #[should_panic(expected = "has no ControlOwner")]
    fn utility_agent_without_controller_metadata_panics() {
        let mut app = app();
        app.world_mut().spawn(UtilityAgent::new(1, 0));

        app.update();
    }

    #[test]
    #[should_panic(expected = "retained unsettled CurrentAction phase Resolving")]
    fn unresolved_action_phase_panics_after_update() {
        let mut app = app();
        app.world_mut().spawn((
            control(ControlOwner::UtilityAction, LocomotionOwner::None),
            action(ActionPhase::Resolving),
        ));

        app.update();
    }

    #[test]
    #[should_panic(expected = "lacks an exclusive moving route")]
    fn standing_route_cannot_retain_an_active_route_lease() {
        let mut app = app();
        app.world_mut().spawn((
            control(ControlOwner::UtilityAction, LocomotionOwner::CrewRoute),
            CrewRoute::standing(),
        ));

        app.update();
    }

    #[test]
    #[should_panic(expected = "cannot resume suspended UtilityAction/Errand")]
    fn action_errand_cannot_be_suspended_without_its_action() {
        let mut app = app();
        let actor = app
            .world_mut()
            .spawn((
                control(ControlOwner::Incapacitated, LocomotionOwner::None),
                SuspendedUtilityControl {
                    owner: ControlOwner::UtilityAction,
                    locomotion: LocomotionOwner::Errand,
                },
            ))
            .id();
        give_errand(&mut app, actor);

        app.update();
    }

    #[test]
    #[should_panic(expected = "cannot resume suspended UtilityAction/None")]
    fn nonmedical_suspension_cannot_retain_a_transport_task() {
        let mut app = app();
        let missing_patient = app.world_mut().spawn_empty().id();
        let actor = app
            .world_mut()
            .spawn((
                control(ControlOwner::Incapacitated, LocomotionOwner::None),
                SuspendedUtilityControl {
                    owner: ControlOwner::UtilityAction,
                    locomotion: LocomotionOwner::None,
                },
            ))
            .id();
        app.world_mut()
            .entity_mut(actor)
            .insert(MedicalTransportTask {
                case: super::super::medical::MedicalCaseId(77),
                patient: missing_patient,
                bed: "medical.bed.1".into(),
                bed_at: Vec3::ZERO,
                bed_claim: ReservationOwner {
                    agent: actor,
                    action_instance: 77,
                },
            });

        app.update();
    }

    #[test]
    fn client_does_not_validate_authority_only_control_state() {
        let mut app = app();
        app.insert_resource(crate::net::LaunchMode::Join(
            "127.0.0.1:7777".parse().expect("test address is valid"),
        ));
        app.world_mut().spawn(UtilityAgent::new(1, 0));

        app.update();
    }
}
