use super::*;

pub(super) fn db() -> ChemDb {
    ChemDb(
        chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap(),
    )
}

pub(super) fn order(db: &ChemDb) -> Order {
    Order {
        reagent: db.reagent("kelotane"),
        specific: true,
        minimum_purity: 0.7,
        amount: Units::whole(20),
        plea: "A patient needs treatment for burns.".into(),
        patience: 180.0,
        waited: 0.0,
    }
}
pub(super) fn chemical(db: &ChemDb, key: &str) -> Container {
    let mut c = Container::new(ContainerKind::Beaker);
    let _ = c.solution.add(db.reagent(key), Units::whole(20));
    c
}
struct Fixture {
    app: App,
    player: Entity,
    officer: Entity,
    customer: Entity,
    batch: Entity,
    locker: Entity,
    warden: Entity,
}
fn fixture() -> Fixture {
    let db = db();
    let order = order(&db);
    let batch_contents = chemical(&db, "kelotane");
    let original = AnalysisReport::measure(&batch_contents.solution, &db, 33, 0.0, Some(11));
    let mut app = App::new();
    app.insert_resource(db)
        .init_resource::<crate::nav::NavGraph>()
        .init_resource::<Time>()
        .init_resource::<RadioLog>()
        .add_message::<FromClient<OpenOrderConversation>>()
        .add_message::<FromClient<InteractRequested>>()
        .add_message::<FromClient<CaseActionRequested>>()
        .add_message::<ToClients<CaseConversationOpened>>()
        .add_systems(Update, (open, actions, advance).chain());
    let player = app
        .world_mut()
        .spawn((
            crate::player::Chemist {
                client: ClientId::Server,
            },
            Transform::from_xyz(0.0, 0.93, 1.0),
            Body::default(),
            Bloodstream::default(),
            crate::containers::SelectedInventorySlot(0),
        ))
        .id();
    let officer = app
        .world_mut()
        .spawn((
            CrewMember {
                name: REYES.into(),
                role: "Security".into(),
            },
            Transform::from_xyz(0.0, 0.93, 0.0),
            Body::default(),
            Bloodstream::default(),
            CrewRoute::standing(),
            CaseOfficer,
            PendingOrder::new(
                order.clone(),
                crate::order_intake::RequestContext {
                    id: 77,
                    source: RequestSource::Security,
                    campaign: None,
                    greeting: crate::order_intake::GreetingKind::Ordinary,
                    step: None,
                },
            ),
            AwaitingConversation {
                id: 77,
                arrived: true,
            },
        ))
        .id();
    let customer = app
        .world_mut()
        .spawn((
            order.clone(),
            CrewMember {
                name: "Dr. Vance".into(),
                role: "Medical".into(),
            },
        ))
        .id();
    let batch = app
        .world_mut()
        .spawn((
            batch_contents,
            SampleId(33),
            Transform::from_xyz(1.0, 1.0, 0.0),
        ))
        .id();
    let locker = app
        .world_mut()
        .spawn((CaseLocker, Transform::from_xyz(20.0, 0.45, 0.0)))
        .id();
    let warden = app
        .world_mut()
        .spawn((
            CaseWarden,
            CrewMember {
                name: BEX.into(),
                role: "Security".into(),
            },
            Transform::from_xyz(20.0, 0.93, 1.0),
            Body::default(),
            Bloodstream::default(),
        ))
        .id();
    app.insert_resource(SecurityCaseState {
        active: Some(SecurityCase {
            id: 11,
            stage: Stage::Greeting,
            customer: "Dr. Vance".into(),
            explanation: order.plea.clone(),
            requirements: "20u burn medication".into(),
            batch_description: "Beaker on Chemistry worktop".into(),
            batch: 33,
            sample: 22,
            original,
            reference: None,
            notice_left: 0.0,
            created_at: 0.0,
        }),
        ..default()
    });
    app.insert_resource(Runtime {
        customer: Some(customer),
        requester: Some(officer),
        target: Some(batch),
        inspection_target: Some(Vec3::new(0.0, 0.0, 0.0)),
        ..default()
    });
    Fixture {
        app,
        player,
        officer,
        customer,
        batch,
        locker,
        warden,
    }
}
fn tick(app: &mut App, seconds: f32) {
    app.world_mut()
        .resource_mut::<Time>()
        .advance_by(std::time::Duration::from_secs_f32(seconds));
    app.update();
}
fn talk(f: &mut Fixture) {
    f.app.world_mut().write_message(FromClient {
        client_id: ClientId::Server,
        message: OpenOrderConversation {
            target: f.officer,
            id: 77,
        },
    });
    tick(&mut f.app, 0.01);
}
fn action(f: &mut Fixture, target: Entity, action: CaseAction) {
    f.app.world_mut().write_message(FromClient {
        client_id: ClientId::Server,
        message: CaseActionRequested {
            target,
            case: 11,
            action,
        },
    });
    tick(&mut f.app, 0.01);
}
fn hold(f: &mut Fixture) {
    talk(f);
    action(f, f.officer, CaseAction::RecordHold);
}
fn at_warden(f: &mut Fixture) {
    f.app
        .world_mut()
        .get_mut::<Transform>(f.player)
        .unwrap()
        .translation = Vec3::new(20.0, 0.93, 2.0);
    f.app.world_mut().write_message(FromClient {
        client_id: ClientId::Server,
        message: InteractRequested { target: f.warden },
    });
    tick(&mut f.app, 0.01);
}
fn stage(f: &Fixture) -> Stage {
    f.app
        .world()
        .resource::<SecurityCaseState>()
        .active
        .as_ref()
        .unwrap()
        .stage
}

#[test]
fn legitimate_finished_batch_selection_excludes_urgency_contamination_and_controlled_chemistry() {
    let db = db();
    let mut order = order(&db);
    let mut c = chemical(&db, "kelotane");
    assert!(eligible(&c, &order, &db));
    order.waited = 91.0;
    assert!(!eligible(&c, &order, &db));
    order.waited = 0.0;
    let _ = c.solution.add(db.reagent("space_drugs"), Units::ONE);
    assert!(!eligible(&c, &order, &db));
    let mut illicit = order;
    illicit.reagent = db.reagent("space_drugs");
    assert!(!eligible(&chemical(&db, "space_drugs"), &illicit, &db));
}
#[test]
fn opening_and_explaining_never_seizes_or_pauses() {
    let mut f = fixture();
    talk(&mut f);
    let officer = f.officer;
    action(&mut f, officer, CaseAction::Grounds);
    assert_eq!(stage(&f), Stage::Greeting);
    assert!(f.app.world().get::<CaseCustody>(f.batch).is_none());
    assert!(f.app.world().get::<OrderHold>(f.customer).is_none());
}
#[test]
fn acknowledgment_without_opening_cannot_seize() {
    let mut f = fixture();
    let officer = f.officer;
    action(&mut f, officer, CaseAction::RecordHold);
    assert_eq!(stage(&f), Stage::Greeting);
}
#[test]
fn physical_hold_conserves_liquid_and_only_holds_affected_order() {
    let mut f = fixture();
    hold(&mut f);
    assert_eq!(stage(&f), Stage::Custody);
    assert_eq!(
        f.app
            .world()
            .get::<Container>(f.batch)
            .unwrap()
            .solution
            .total_volume(),
        Units::whole(19)
    );
    assert_eq!(
        f.app.world().get::<CaseCustody>(f.batch),
        Some(&CaseCustody(11))
    );
    assert_eq!(
        f.app.world().get::<OrderHold>(f.customer),
        Some(&OrderHold(11))
    );
    let mut q = f.app.world_mut().query::<(&Container, &CaseSample)>();
    assert_eq!(
        q.iter(f.app.world())
            .map(|(c, _)| c.solution.total_volume())
            .sum::<Units>(),
        Units::ONE
    );
}
#[test]
fn refusal_gives_full_notice_without_instant_custody() {
    let mut f = fixture();
    talk(&mut f);
    let officer = f.officer;
    action(&mut f, officer, CaseAction::Refuse);
    assert_eq!(stage(&f), Stage::Notice);
    tick(&mut f.app, 44.0);
    assert_eq!(stage(&f), Stage::Notice);
    assert!(f.app.world().get::<OrderHold>(f.customer).is_none());
    tick(&mut f.app, 1.1);
    assert_eq!(stage(&f), Stage::Custody);
    assert_eq!(f.app.world().resource::<SecurityCaseState>().refusals, 1);
}
#[test]
fn inaccessible_or_changed_target_withdraws_without_substitution() {
    for removed in [false, true] {
        let mut f = fixture();
        talk(&mut f);
        if removed {
            f.app.world_mut().entity_mut(f.batch).insert(InventorySlot {
                owner: f.player,
                slot: 2,
            });
        } else {
            f.app
                .world_mut()
                .get_mut::<Container>(f.batch)
                .unwrap()
                .solution
                .take(Units::ONE);
        }
        let officer = f.officer;
        action(&mut f, officer, CaseAction::RecordHold);
        assert!(f
            .app
            .world()
            .resource::<SecurityCaseState>()
            .active
            .is_none());
        assert!(f.app.world().get::<OrderHold>(f.customer).is_none());
    }
}
#[test]
fn officer_cannot_seize_through_a_wall_or_across_a_room() {
    for wall in [false, true] {
        let mut f = fixture();
        talk(&mut f);
        if wall {
            f.app.world_mut().spawn((
                Transform::from_xyz(0.5, 1.0, 0.0),
                crate::lab::Solid {
                    half_extents: Vec3::new(0.1, 2.0, 2.0),
                },
            ));
        } else {
            f.app
                .world_mut()
                .get_mut::<Transform>(f.batch)
                .unwrap()
                .translation
                .x = 12.0;
        }
        let officer = f.officer;
        action(&mut f, officer, CaseAction::RecordHold);
        assert!(f.app.world().get::<CaseCustody>(f.batch).is_none());
        assert!(f.app.world().get::<OrderHold>(f.customer).is_none());
    }
}
#[test]
fn authentic_report_releases_custody_but_timer_waits_for_collection() {
    let mut f = fixture();
    hold(&mut f);
    at_warden(&mut f);
    let report = f
        .app
        .world()
        .resource::<SecurityCaseState>()
        .active
        .as_ref()
        .unwrap()
        .reference
        .clone()
        .unwrap();
    f.app.world_mut().spawn((
        report,
        InventorySlot {
            owner: f.player,
            slot: 1,
        },
        crate::labels::Label("this is a prank".into()),
    ));
    let warden = f.warden;
    action(&mut f, warden, CaseAction::PresentReport);
    assert_eq!(stage(&f), Stage::Released);
    assert!(f.app.world().get::<OrderHold>(f.customer).is_some());
    at_warden(&mut f);
    action(&mut f, warden, CaseAction::Collect);
    assert!(f
        .app
        .world()
        .resource::<SecurityCaseState>()
        .active
        .is_none());
    assert!(f.app.world().get::<OrderHold>(f.customer).is_none());
    assert_eq!(
        f.app
            .world()
            .get::<Container>(f.batch)
            .unwrap()
            .solution
            .total_volume(),
        Units::whole(20)
    );
    assert_eq!(
        f.app.world().resource::<SecurityCaseState>().cooldown,
        1200.0
    );
}
#[test]
fn mismatched_report_and_remote_appeal_are_rejected() {
    let mut f = fixture();
    hold(&mut f);
    at_warden(&mut f);
    let mut report = f
        .app
        .world()
        .resource::<SecurityCaseState>()
        .active
        .as_ref()
        .unwrap()
        .reference
        .clone()
        .unwrap();
    report.sample += 1;
    f.app.world_mut().spawn((
        report,
        InventorySlot {
            owner: f.player,
            slot: 1,
        },
    ));
    let warden = f.warden;
    action(&mut f, warden, CaseAction::PresentReport);
    assert_eq!(stage(&f), Stage::Custody);
    f.app
        .world_mut()
        .get_mut::<Transform>(f.player)
        .unwrap()
        .translation = Vec3::ZERO;
    action(&mut f, warden, CaseAction::VerifyCustody);
    assert_eq!(stage(&f), Stage::Custody);
}
#[test]
fn lost_sample_has_honest_verification_without_recreating_liquid() {
    let mut f = fixture();
    hold(&mut f);
    let mut q = f
        .app
        .world_mut()
        .query_filtered::<Entity, With<CaseSample>>();
    let sample = q.single(f.app.world()).unwrap();
    f.app.world_mut().despawn(sample);
    at_warden(&mut f);
    let warden = f.warden;
    action(&mut f, warden, CaseAction::VerifyCustody);
    assert_eq!(stage(&f), Stage::Released);
    at_warden(&mut f);
    action(&mut f, warden, CaseAction::Collect);
    assert_eq!(
        f.app
            .world()
            .get::<Container>(f.batch)
            .unwrap()
            .solution
            .total_volume(),
        Units::whole(19)
    );
}
#[test]
fn duplicate_collection_cannot_duplicate_batch_or_close_another_case() {
    let mut f = fixture();
    hold(&mut f);
    at_warden(&mut f);
    let warden = f.warden;
    let report = f
        .app
        .world()
        .resource::<SecurityCaseState>()
        .active
        .as_ref()
        .unwrap()
        .reference
        .clone()
        .unwrap();
    f.app.world_mut().spawn((
        report,
        InventorySlot {
            owner: f.player,
            slot: 1,
        },
    ));
    action(&mut f, warden, CaseAction::PresentReport);
    at_warden(&mut f);
    for _ in 0..2 {
        f.app.world_mut().write_message(FromClient {
            client_id: ClientId::Server,
            message: CaseActionRequested {
                target: warden,
                case: 11,
                action: CaseAction::Collect,
            },
        });
    }
    tick(&mut f.app, 0.01);
    assert_eq!(
        f.app.world().resource::<SecurityCaseState>().history.len(),
        1
    );
    assert_eq!(
        f.app
            .world()
            .get::<Container>(f.batch)
            .unwrap()
            .solution
            .total_volume(),
        Units::whole(20)
    );
}
#[test]
fn abandon_is_explicit_and_resumes_order() {
    let mut f = fixture();
    hold(&mut f);
    at_warden(&mut f);
    let warden = f.warden;
    action(&mut f, warden, CaseAction::Abandon);
    assert!(f.app.world().get_entity(f.batch).is_err());
    assert!(f.app.world().get::<OrderHold>(f.customer).is_none());
}
#[test]
fn case_snapshot_contains_stable_identifiers_not_runtime_order_entities() {
    let mut f = fixture();
    hold(&mut f);
    let state = f.app.world().resource::<SecurityCaseState>();
    let bytes = postcard::to_allocvec(state).unwrap();
    let loaded: SecurityCaseState = postcard::from_bytes(&bytes).unwrap();
    assert_eq!(*state, loaded);
}
#[test]
fn report_requires_case_identity_and_unaltered_measurements() {
    let mut f = fixture();
    hold(&mut f);
    let case = f
        .app
        .world()
        .resource::<SecurityCaseState>()
        .active
        .as_ref()
        .unwrap();
    let report = case.reference.as_ref().unwrap();
    assert!(valid_report(report, case));
    let mut wrong = report.clone();
    wrong.case = None;
    assert!(!valid_report(&wrong, case));
    let mut wrong = report.clone();
    wrong.chemicals[0].amount_raw += 1;
    assert!(!valid_report(&wrong, case));
}
#[test]
fn preserved_sample_requires_analysis_before_appeal() {
    let mut f = fixture();
    hold(&mut f);
    at_warden(&mut f);
    let warden = f.warden;
    action(&mut f, warden, CaseAction::VerifyCustody);
    assert_eq!(stage(&f), Stage::Custody);
    assert_eq!(f.app.world().resource::<SecurityCaseState>().complaints, 0);
}
#[test]
fn reference_sample_outside_reachable_floor_allows_custody_verification() {
    let mut f = fixture();
    hold(&mut f);
    let mut areas = crate::lab::WalkableAreas::default();
    areas.push(
        crate::lab::Bounds {
            min_x: -5.0,
            max_x: 25.0,
            min_z: -5.0,
            max_z: 5.0,
        },
        None,
    );
    f.app
        .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS));
    let sample = f
        .app
        .world_mut()
        .query_filtered::<Entity, With<CaseSample>>()
        .single(f.app.world())
        .unwrap();
    f.app
        .world_mut()
        .entity_mut(sample)
        .remove::<(InventorySlot, HeldBy)>()
        .insert(Transform::from_xyz(1000.0, -20.0, 1000.0));
    at_warden(&mut f);
    let warden = f.warden;
    action(&mut f, warden, CaseAction::VerifyCustody);
    assert_eq!(stage(&f), Stage::Released);
}
#[test]
fn visible_loose_reference_requires_report_but_sealed_wall_sample_can_be_recovered() {
    for blocked in [false, true] {
        let mut f = fixture();
        hold(&mut f);
        let mut areas = crate::lab::WalkableAreas::default();
        areas.push(
            crate::lab::Bounds {
                min_x: -5.0,
                max_x: 25.0,
                min_z: -5.0,
                max_z: 5.0,
            },
            None,
        );
        f.app
            .insert_resource(crate::nav::NavGraph::build(&areas, crate::nav::NAV_RADIUS));
        let sample = f
            .app
            .world_mut()
            .query_filtered::<Entity, With<CaseSample>>()
            .single(f.app.world())
            .unwrap();
        f.app
            .world_mut()
            .entity_mut(sample)
            .remove::<(InventorySlot, HeldBy)>()
            .insert(Transform::from_xyz(0.0, 1.0, 2.0));
        if blocked {
            f.app.world_mut().spawn((
                Transform::from_xyz(0.0, 1.0, 2.0),
                crate::lab::Solid {
                    half_extents: Vec3::splat(0.5),
                },
            ));
        }
        at_warden(&mut f);
        let warden = f.warden;
        action(&mut f, warden, CaseAction::VerifyCustody);
        assert_eq!(
            stage(&f),
            if blocked {
                Stage::Released
            } else {
                Stage::Custody
            }
        );
    }
}
#[test]
fn ordinary_cooling_does_not_invalidate_evidence_identity() {
    let mut f = fixture();
    hold(&mut f);
    let case = f
        .app
        .world()
        .resource::<SecurityCaseState>()
        .active
        .as_ref()
        .unwrap();
    let mut report = case.reference.clone().unwrap();
    report.temperature += 10.0;
    assert!(valid_report(&report, case));
}
#[test]
fn no_case_reconciliation_preserves_pending_admission_retry() {
    let mut app = App::new();
    app.init_resource::<SecurityCaseState>()
        .init_resource::<crate::crew::CrewPosts>()
        .insert_resource(Runtime {
            retry: Some(Timer::from_seconds(0.5, TimerMode::Once)),
            ..default()
        })
        .add_systems(Update, reconcile);
    app.world_mut()
        .resource_mut::<Runtime>()
        .retry
        .as_mut()
        .unwrap()
        .tick(std::time::Duration::from_millis(300));
    app.update();
    assert_eq!(
        app.world()
            .resource::<Runtime>()
            .retry
            .as_ref()
            .unwrap()
            .elapsed_secs(),
        0.3
    );
    app.world_mut()
        .resource_mut::<Runtime>()
        .retry
        .as_mut()
        .unwrap()
        .tick(std::time::Duration::from_millis(300));
    app.update();
    assert!(app
        .world()
        .resource::<Runtime>()
        .retry
        .as_ref()
        .unwrap()
        .is_finished());
}
#[test]
fn incapacitated_warden_allows_local_locker_recovery_for_lost_sample() {
    let mut f = fixture();
    hold(&mut f);
    let mut q = f
        .app
        .world_mut()
        .query_filtered::<Entity, With<CaseSample>>();
    let sample = q.single(f.app.world()).unwrap();
    f.app.world_mut().despawn(sample);
    f.app
        .world_mut()
        .get_mut::<Body>(f.warden)
        .unwrap()
        .0
        .collapsed = true;
    f.app
        .world_mut()
        .get_mut::<Transform>(f.player)
        .unwrap()
        .translation = Vec3::new(20.0, 0.93, 1.0);
    let locker = f.locker;
    f.app.world_mut().write_message(FromClient {
        client_id: ClientId::Server,
        message: InteractRequested { target: locker },
    });
    tick(&mut f.app, 0.01);
    action(&mut f, locker, CaseAction::VerifyCustody);
    assert_eq!(stage(&f), Stage::Released);
}

#[test]
fn complaint_history_is_separate_from_department_standing_and_persists() {
    let state = SecurityCaseState {
        ordinary_deliveries: 3,
        cooperative_holds: 1,
        refusals: 2,
        complaints: 1,
        cooldown: 1200.0,
        ..default()
    };
    let saved = ron::to_string(&state).unwrap();
    let loaded: SecurityCaseState = ron::from_str(&saved).unwrap();
    assert_eq!(state, loaded);
}
