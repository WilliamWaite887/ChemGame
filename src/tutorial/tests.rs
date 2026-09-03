use super::*;
#[test]
fn training_characters_can_be_targeted_by_the_normal_interaction_system() {
    let mut world = World::new();
    for (name, kind) in [
        ("Lab Instructor", Actor::Instructor),
        ("Practice Patient", Actor::Patient),
        ("Practice Customer", Actor::Customer),
    ] {
        let entity = actor(&mut world, name, kind, Vec3::ZERO);
        assert_eq!(
            world
                .get::<crate::interaction::Interactable>(entity)
                .expect("visible training characters need an interactable root for mesh targeting")
                .label,
            name
        );
    }
}

#[test]
fn training_content_links_and_goals_are_valid() {
    let lessons = Lessons::default();
    let book = crate::textbook::Textbook::default();
    assert_eq!(lessons.0.len(), 13);
    assert_eq!(lessons.0.iter().filter(|l| l.core).count(), 6);
    let mut ids = HashSet::new();
    for l in &lessons.0 {
        assert!(ids.insert(&l.id));
        assert!(!l.stages.is_empty());
        for stage in &l.stages {
            assert!(book.0.iter().any(|a| a.id == stage.article));
            assert!(stage.hints.iter().all(|h| !h.is_empty()));
            assert!(!stage.objective.is_empty());
            assert!(!stage.marker.is_empty());
            for text in stage.hints.iter().chain([&stage.marker, &stage.objective]) {
                assert!(
                    !crate::textbook::expand(text, &crate::settings::Settings::default())
                        .contains('{')
                );
            }
        }
    }
    let inspections: Vec<_> = lessons
        .0
        .iter()
        .flat_map(|lesson| {
            lesson
                .stages
                .iter()
                .filter(|stage| stage.goal == Goal::Inspect)
                .map(|_| lesson.id.as_str())
        })
        .collect();
    assert_eq!(
        inspections,
        ["delivery"],
        "inspection is required only at its introduction"
    );
}
#[test]
fn progress_roundtrip_distinguishes_skipped_and_completed() {
    let mut p = TrainingProgress {
        version: 1,
        resume: Some("batch".into()),
        ..default()
    };
    p.completed.insert("bearings".into());
    p.skipped.insert("request".into());
    assert_eq!(
        ron::from_str::<TrainingProgress>(&ron::to_string(&p).unwrap()).unwrap(),
        p
    );
}
#[test]
fn curriculum_never_requires_an_advanced_recipe() {
    let text = include_str!("../../assets/data/lab.training.ron");
    for spoiler in [
        "Bicaridine",
        "Dexalin",
        "Libital",
        "Penthrite",
        "Space Cleaner",
    ] {
        assert!(!text.contains(spoiler));
    }
}

fn app() -> (App, Entity, Entity, Entity) {
    use crate::machines::*;
    let mut app = crate::machines::tests::test_app();
    app.insert_resource(SessionKind::Training)
        .init_resource::<crate::settings::Paused>()
        .init_resource::<crate::textbook::TextbookView>()
        .init_resource::<crate::orders::Shift>()
        .add_message::<DeliveryEvidence>()
        .add_message::<FromClient<AnalyzeRequested>>()
        .add_message::<crate::orders::OrderResolved>()
        .add_message::<crate::chem_world::ChemicalExposure>()
        .add_systems(
            Update,
            (handle_analyze, crate::analysis_reports::scan).chain(),
        )
        .add_systems(Update, crate::orders::handle_delivery)
        .add_systems(PostUpdate, observe);
    let client = ClientId::Client(app.world_mut().spawn_empty().id());
    let worker = app
        .world_mut()
        .spawn((
            LocalPlayer,
            crate::player::Chemist { client },
            crate::body::Body::default(),
            crate::body::Bloodstream::default(),
            InteractionMode::Roaming,
            crate::containers::SelectedInventorySlot(0),
            Transform::from_xyz(0.0, 1.7, 1.0),
        ))
        .id();
    let mut machine = Machine::new(MachineKind::ChemMaster5000);
    machine.in_use_by = Some(worker);
    let bench = app
        .world_mut()
        .spawn((
            machine,
            DispenseAmount(Units::whole(5)),
            Transform::default(),
            crate::lab::Solid {
                half_extents: Vec3::splat(0.5),
            },
            Buffer(Solution::unbounded()),
        ))
        .id();
    let beaker = app
        .world_mut()
        .spawn((
            Container::new(ContainerKind::Beaker),
            InSlot(bench),
            Transform::default(),
        ))
        .id();
    let mut runner = Runner::new("delivery".into());
    runner.ready = true;
    runner.evidence.initial.insert(beaker);
    app.insert_resource(runner);
    (app, worker, bench, beaker)
}
fn send<T: Message>(app: &mut App, message: T)
where
    FromClient<T>: Message,
{
    let client_id = app
        .world_mut()
        .query::<&crate::player::Chemist>()
        .iter(app.world())
        .next()
        .unwrap()
        .client;
    app.world_mut()
        .write_message(FromClient { client_id, message });
    app.update();
}
#[test]
fn actual_preparation_analysis_packaging_and_delivery_complete_only_after_verified_handoff() {
    use crate::machines::*;
    let (mut app, worker, bench, beaker) = app();
    let reagent = |app: &App, key: &str| {
        app.world()
            .resource::<crate::chem_data::ChemDb>()
            .reagent(key)
    };
    let silicon = reagent(&app, "silicon");
    let carbon = reagent(&app, "carbon");
    let kelotane = reagent(&app, "kelotane");
    send(
        &mut app,
        DispenseRequested {
            machine: bench,
            reagent: silicon,
        },
    );
    assert!(!app.world().resource::<Runner>().evidence.has(Goal::Batch10));
    send(
        &mut app,
        DispenseRequested {
            machine: bench,
            reagent: carbon,
        },
    );
    assert!(app.world().resource::<Runner>().evidence.has(Goal::Batch10));
    // A request sent to the wrong machine is rejected and cannot count as analysis.
    send(&mut app, AnalyzeRequested { machine: bench });
    assert!(!app.world().resource::<Runner>().evidence.has(Goal::Analyze));
    app.world_mut().get_mut::<Machine>(bench).unwrap().kind = MachineKind::Analyzer;
    send(&mut app, AnalyzeRequested { machine: bench });
    assert!(app.world().resource::<Runner>().evidence.has(Goal::Analyze));
    app.world_mut().get_mut::<Machine>(bench).unwrap().kind = MachineKind::MixingChamber;
    send(
        &mut app,
        PackageRequested {
            machine: bench,
            kind: ContainerKind::Bottle,
            label: None,
            nonce: 1,
        },
    );
    assert!(
        !app.world().resource::<Runner>().evidence.has(Goal::Package),
        "empty reservoir request must not pass"
    );
    send(
        &mut app,
        BufferTransferRequested {
            machine: bench,
            reagent: kelotane,
            amount: Units::whole(10),
            direction: BufferDirection::ToBuffer,
            slot: MachineSlot::A,
        },
    );
    send(
        &mut app,
        PackageRequested {
            machine: bench,
            kind: ContainerKind::Bottle,
            label: None,
            nonce: 2,
        },
    );
    let bottle = *app
        .world()
        .resource::<Runner>()
        .evidence
        .packages
        .iter()
        .next()
        .unwrap();
    app.world_mut()
        .entity_mut(worker)
        .insert(InteractionMode::Inspecting {
            item: bottle,
            machine: None,
        });
    app.update();
    assert!(app.world().resource::<Runner>().evidence.has(Goal::Inspect));
    assert!(app
        .world()
        .get::<Container>(beaker)
        .unwrap()
        .solution
        .is_empty());
    let epoch = app.world().resource::<Runner>().epoch;
    let customer = app
        .world_mut()
        .spawn((
            crate::crew::CrewMember {
                name: "Practice Customer".into(),
                role: "Medical".into(),
            },
            crate::orders::Order {
                reagent: kelotane,
                specific: true,
                minimum_purity: 0.0,
                amount: Units::whole(10),
                plea: "practice".into(),
                patience: 600.0,
                waited: 0.0,
            },
            crate::crew::CrewRoute::standing(),
            crate::body::Body::default(),
            crate::body::Bloodstream::default(),
            crate::order_intake::RequestContext {
                id: epoch,
                source: crate::order_intake::RequestSource::Specific,
                campaign: None,
                greeting: crate::order_intake::GreetingKind::Ordinary,
                step: None,
            },
        ))
        .id();
    assert!(!app.world().resource::<Runner>().evidence.has(Goal::Deliver));
    send(
        &mut app,
        crate::interaction::InteractRequested { target: customer },
    );
    assert!(app.world().resource::<Runner>().evidence.has(Goal::Deliver));
    assert!(app.world().get::<Container>(bottle).is_none());
    assert!(!app.world().contains_resource::<crate::saves::SaveSlot>());
}
#[test]
fn hplc_is_available_in_training_and_career_without_unlocking_recipes() {
    use crate::machines::*;
    let (mut app, _, bench, beaker) = app();
    let reagent = app
        .world()
        .resource::<crate::chem_data::ChemDb>()
        .reagent("kelotane");
    app.world_mut().get_mut::<Machine>(bench).unwrap().kind = MachineKind::Analyzer;
    let _ = app
        .world_mut()
        .get_mut::<Container>(beaker)
        .unwrap()
        .solution
        .add(reagent, Units::whole(10));
    send(
        &mut app,
        PurifyRequested {
            machine: bench,
            reagent,
        },
    );
    assert_eq!(
        app.world().get::<HplcReport>(bench).unwrap().product_amount,
        Units::whole(9)
    );
    assert_eq!(
        app.world()
            .resource::<crate::knowledge::Knowledge>()
            .known_count(),
        3
    );
    app.world_mut().entity_mut(bench).remove::<HplcReport>();
    app.insert_resource(SessionKind::Career);
    send(
        &mut app,
        PurifyRequested {
            machine: bench,
            reagent,
        },
    );
    assert!(app.world().get::<HplcReport>(bench).is_some());
    assert_eq!(
        app.world()
            .resource::<crate::knowledge::Knowledge>()
            .known_count(),
        3,
        "using HPLC must not bypass recipe discovery"
    );
}
#[test]
fn stale_delivery_generation_and_seeded_bottles_cannot_complete_an_exercise() {
    let (mut app, _, _, _) = app();
    let kel = app
        .world()
        .resource::<crate::chem_data::ChemDb>()
        .reagent("kelotane");
    let mut actual = Solution::unbounded();
    let _ = actual.add(kel, Units::whole(10));
    let seeded = app
        .world_mut()
        .spawn(Container {
            kind: ContainerKind::Bottle,
            solution: actual.clone(),
        })
        .id();
    app.world_mut()
        .resource_mut::<Runner>()
        .evidence
        .initial
        .insert(seeded);
    app.world_mut().write_message(DeliveryEvidence {
        request: 0,
        container: seeded,
        kind: ContainerKind::Bottle,
        actual,
        outcome: crate::orders::Outcome::Success,
    });
    app.update();
    assert!(!app.world().resource::<Runner>().evidence.has(Goal::Package));
    assert!(!app.world().resource::<Runner>().evidence.has(Goal::Deliver));
}

#[test]
fn measured_mistake_accepts_both_correction_and_fresh_preparation() {
    use crate::machines::*;
    for remake in [false, true] {
        let (mut app, _, bench, bottle) = app();
        let kel = app
            .world()
            .resource::<crate::chem_data::ChemDb>()
            .reagent("kelotane");
        let si = app
            .world()
            .resource::<crate::chem_data::ChemDb>()
            .reagent("silicon");
        let carbon = app
            .world()
            .resource::<crate::chem_data::ChemDb>()
            .reagent("carbon");
        app.world_mut().entity_mut(bottle).insert(MistakeSample);
        let mut c = app.world_mut().get_mut::<Container>(bottle).unwrap();
        let _ = c.solution.add(kel, Units::whole(10));
        let _ = c.solution.add(si, Units::whole(5));
        app.world_mut().get_mut::<Machine>(bench).unwrap().kind = MachineKind::Analyzer;
        send(&mut app, AnalyzeRequested { machine: bench });
        assert!(app
            .world()
            .resource::<Runner>()
            .evidence
            .has(Goal::AnalyzeMistake));
        assert!(!app.world().resource::<Runner>().evidence.has(Goal::Repair));
        app.world_mut().get_mut::<Machine>(bench).unwrap().kind = MachineKind::ChemMaster5000;
        if remake {
            app.world_mut().entity_mut(bottle).remove::<InSlot>();
            app.world_mut()
                .spawn((Container::new(ContainerKind::Beaker), InSlot(bench)));
            app.world_mut().get_mut::<DispenseAmount>(bench).unwrap().0 = Units::whole(10);
            send(
                &mut app,
                DispenseRequested {
                    machine: bench,
                    reagent: si,
                },
            );
        }
        send(
            &mut app,
            DispenseRequested {
                machine: bench,
                reagent: carbon,
            },
        );
        assert!(app.world().resource::<Runner>().evidence.has(Goal::Repair));
    }
}

#[test]
fn restarting_discards_pending_actions_and_evidence_but_keeps_profile() {
    let (mut app, _, bench, _) = app();
    let first_epoch = app.world().resource::<Runner>().epoch;
    let mut profile = TrainingProgress {
        version: 1,
        resume: Some("mistake".into()),
        ..default()
    };
    profile.completed.insert("delivery".into());
    app.insert_resource(Profile(profile));
    app.world_mut().write_message(FromClient {
        client_id: ClientId::Server,
        message: crate::machines::AnalyzeRequested { machine: bench },
    });
    app.world_mut()
        .resource_mut::<Runner>()
        .evidence
        .set(Goal::Analyze);
    clear_session(app.world_mut());
    assert!(app
        .world()
        .resource::<Messages<FromClient<crate::machines::AnalyzeRequested>>>()
        .is_empty());
    assert!(!app.world().contains_resource::<Runner>());
    assert!(app
        .world()
        .resource::<Profile>()
        .0
        .completed
        .contains("delivery"));
    for lesson in Lessons::default().0 {
        let resumed = Runner::new(lesson.id);
        assert_eq!(resumed.stage, 0);
        assert_ne!(resumed.epoch, first_epoch);
        assert!(!resumed.ready);
        assert!(resumed.evidence.goals.is_empty());
    }
}

#[test]
fn full_inventory_keeps_packaged_material_available_for_recovery() {
    use crate::{containers::*, machines::*};
    let (mut app, worker, bench, _) = app();
    for slot in 0..INVENTORY_SLOTS {
        app.world_mut().spawn((
            Container::new(ContainerKind::Beaker),
            InventorySlot {
                owner: worker,
                slot,
            },
        ));
    }
    let kel = app
        .world()
        .resource::<crate::chem_data::ChemDb>()
        .reagent("kelotane");
    app.world_mut().get_mut::<Machine>(bench).unwrap().kind = MachineKind::MixingChamber;
    let _ = app
        .world_mut()
        .get_mut::<Buffer>(bench)
        .unwrap()
        .0
        .add(kel, Units::whole(10));
    send(
        &mut app,
        PackageRequested {
            machine: bench,
            kind: ContainerKind::Bottle,
            label: None,
            nonce: 57,
        },
    );
    let bottle = *app
        .world()
        .resource::<Runner>()
        .evidence
        .packages
        .iter()
        .next()
        .expect("physical output remains available");
    assert_eq!(
        app.world()
            .get::<Container>(bottle)
            .unwrap()
            .solution
            .total_volume(),
        Units::whole(10)
    );
    assert!(app.world().get::<HeldBy>(bottle).is_none());
    assert!(!app.world().resource::<Runner>().evidence.has(Goal::Deliver));
}

#[test]
fn separating_the_measured_leftover_is_a_valid_recovery() {
    use crate::machines::*;
    let (mut app, _, bench, bottle) = app();
    let kel = app
        .world()
        .resource::<crate::chem_data::ChemDb>()
        .reagent("kelotane");
    let si = app
        .world()
        .resource::<crate::chem_data::ChemDb>()
        .reagent("silicon");
    app.world_mut().entity_mut(bottle).insert(MistakeSample);
    let mut c = app.world_mut().get_mut::<Container>(bottle).unwrap();
    let _ = c.solution.add(kel, Units::whole(10));
    let _ = c.solution.add(si, Units::whole(5));
    app.world_mut().get_mut::<Machine>(bench).unwrap().kind = MachineKind::Analyzer;
    send(&mut app, AnalyzeRequested { machine: bench });
    app.world_mut().get_mut::<Machine>(bench).unwrap().kind = MachineKind::MixingChamber;
    send(
        &mut app,
        BufferTransferRequested {
            machine: bench,
            reagent: kel,
            amount: Units::whole(10),
            direction: BufferDirection::ToBuffer,
            slot: MachineSlot::A,
        },
    );
    assert!(app.world().resource::<Runner>().evidence.has(Goal::Repair));
    assert_eq!(
        app.world()
            .get::<Container>(bottle)
            .unwrap()
            .solution
            .volume_of(si),
        Units::whole(5)
    );
    assert_eq!(
        app.world().get::<Buffer>(bench).unwrap().0.volume_of(kel),
        Units::whole(10)
    );
}

#[test]
fn independent_course_scales_twenty_but_requires_two_safe_verified_handoffs() {
    use crate::machines::*;
    let (mut app, _worker, bench, beaker) = app();
    app.world_mut().resource_mut::<Runner>().lesson = "independent".into();
    app.insert_resource(TrainingSpots(HashMap::from([(
        "customer".into(),
        Transform::default(),
    )])));
    let db = app.world().resource::<crate::chem_data::ChemDb>().0.clone();
    let kel = db.reagent("kelotane");
    app.world_mut().get_mut::<DispenseAmount>(bench).unwrap().0 = Units::whole(10);
    for key in ["silicon", "carbon"] {
        send(
            &mut app,
            DispenseRequested {
                machine: bench,
                reagent: db.reagent(key),
            },
        );
    }
    app.world_mut().get_mut::<Machine>(bench).unwrap().kind = MachineKind::Analyzer;
    send(&mut app, AnalyzeRequested { machine: bench });
    assert!(app.world().resource::<Runner>().evidence.has(Goal::Batch20));
    assert_eq!(
        crate::orders::grade(
            crate::orders::Wanted::Exact(kel),
            Units::whole(20),
            &app.world().get::<Container>(beaker).unwrap().solution,
            ContainerKind::Bottle,
            &crate::chem_data::ChemDb(db.clone())
        )
        .0,
        crate::orders::Outcome::Overdose
    );
    app.world_mut().get_mut::<Machine>(bench).unwrap().kind = MachineKind::MixingChamber;
    for round in 0..2 {
        send(
            &mut app,
            BufferTransferRequested {
                machine: bench,
                reagent: kel,
                amount: Units::whole(10),
                direction: BufferDirection::ToBuffer,
                slot: MachineSlot::A,
            },
        );
        send(
            &mut app,
            PackageRequested {
                machine: bench,
                kind: ContainerKind::Bottle,
                label: None,
                nonce: round + 1,
            },
        );
        let bottle = *app
            .world()
            .resource::<Runner>()
            .evidence
            .packages
            .iter()
            .find(|e| app.world().get::<Container>(**e).is_some())
            .unwrap();
        assert!(!app.world().resource::<Runner>().evidence.has(Goal::Inspect));
        assert!(app.world().get::<Container>(bottle).is_some());
        let epoch = app.world().resource::<Runner>().epoch;
        let customer = app
            .world_mut()
            .spawn((
                crate::crew::CrewMember {
                    name: "Practice Customer".into(),
                    role: "Training".into(),
                },
                crate::orders::Order {
                    reagent: kel,
                    specific: true,
                    minimum_purity: 0.0,
                    amount: Units::whole(10),
                    plea: "practice".into(),
                    patience: 600.0,
                    waited: 0.0,
                },
                crate::crew::CrewRoute::standing(),
                crate::body::Body::default(),
                crate::body::Bloodstream::default(),
                crate::order_intake::RequestContext {
                    id: epoch,
                    source: crate::order_intake::RequestSource::Specific,
                    campaign: None,
                    greeting: crate::order_intake::GreetingKind::Ordinary,
                    step: None,
                },
            ))
            .id();
        send(
            &mut app,
            crate::interaction::InteractRequested { target: customer },
        );
        assert_eq!(
            app.world().resource::<Runner>().evidence.deliveries,
            (round + 1) as usize
        );
        assert_eq!(
            app.world().resource::<Runner>().evidence.has(Goal::Deliver),
            round == 1
        );
    }
}
