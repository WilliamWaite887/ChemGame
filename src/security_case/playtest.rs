//! Explicit opt-in rendered acceptance scenario; normal games never install it.
use super::*;
use crate::{
    interaction::InteractionMode,
    machines::{Machine, MachineKind},
    net::LaunchMode,
    player::{Chemist, LocalPlayer},
};
use bevy::render::view::screenshot::{save_to_disk, Screenshot};

#[derive(Component, Clone, Serialize, Deserialize, MapEntities)]
struct Fixture {
    step: u8,
    case: u64,
    #[entities]
    worker: Entity,
    #[entities]
    customer: Entity,
    #[entities]
    batch: Entity,
    #[entities]
    officer: Entity,
    #[entities]
    warden: Entity,
    #[entities]
    analyzer: Entity,
    #[entities]
    sample: Option<Entity>,
    #[entities]
    paper: Option<Entity>,
}
#[derive(Resource, Default)]
struct Scenario {
    started: Option<std::time::Instant>,
    phase_at: f32,
    phase: Option<u8>,
    panel_at: Option<f32>,
    debug_at: f32,
    diagnostics: String,
    sent: std::collections::HashSet<(u8, u8)>,
    shots: std::collections::HashSet<String>,
}
pub(super) fn install(app: &mut App) {
    app.replicate::<Fixture>()
        .init_resource::<Scenario>()
        .add_systems(
            Update,
            drive
                .run_if(in_state(AppState::Playing))
                .run_if(resource_exists::<crate::lab::MapReady>),
        );
}
fn output(world: &World) -> std::path::PathBuf {
    let role = match world.resource::<LaunchMode>() {
        LaunchMode::Singleplayer => "solo",
        LaunchMode::Join(_) => "client",
        _ => "host",
    };
    std::path::PathBuf::from("target/security-case-playtest").join(role)
}
fn shot(world: &mut World, name: &str) {
    if !world.resource_mut::<Scenario>().shots.insert(name.into()) {
        return;
    }
    let output = output(world);
    let _ = std::fs::create_dir_all(&output);
    world
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(output.join(format!("{name}.png"))));
}
fn once(world: &mut World, step: u8, id: u8) -> bool {
    world.resource_mut::<Scenario>().sent.insert((step, id))
}
fn next(world: &mut World, entity: Entity, f: &mut Fixture, step: u8, elapsed: f32) {
    f.step = step;
    world.entity_mut(entity).insert(f.clone());
    world.resource_mut::<Scenario>().phase_at = elapsed;
}
fn panel_age(world: &mut World, elapsed: f32) -> f32 {
    elapsed
        - *world
            .resource_mut::<Scenario>()
            .panel_at
            .get_or_insert(elapsed)
}
fn place_actor(world: &mut World, actor: Entity, target: Entity) {
    let Some(target) = world.get::<Transform>(target).map(|t| t.translation) else {
        return;
    };
    let candidates = [Vec3::X, Vec3::NEG_X, Vec3::Z, Vec3::NEG_Z];
    for side in candidates {
        let raw = world
            .resource::<crate::nav::NavGraph>()
            .standable_goal(target + side * 1.6);
        let position = world
            .resource::<crate::lab::WalkableAreas>()
            .contain_on_surface(raw, crate::nav::NAV_RADIUS, crate::crew::BODY_OFFSET);
        let blocked = world
            .query::<(&Transform, &crate::lab::Solid)>()
            .iter(world)
            .any(|(t, s)| {
                crate::interaction::authority_segment_blocked(
                    position + Vec3::Y * 0.65,
                    target + Vec3::Y * 0.65,
                    t.translation,
                    s.half_extents,
                )
            });
        if !blocked && position.distance(target) < 2.7 {
            world.get_mut::<Transform>(actor).unwrap().translation = position;
            return;
        }
    }
}
fn seed(world: &mut World) -> Option<Fixture> {
    let solo = matches!(world.resource::<LaunchMode>(), LaunchMode::Singleplayer);
    let players: Vec<_> = world
        .query::<(Entity, &Chemist)>()
        .iter(world)
        .map(|(e, c)| (e, c.client))
        .collect();
    if !solo && players.len() < 2 {
        return None;
    }
    let worker = players
        .iter()
        .find(|(_, c)| {
            if solo {
                *c == ClientId::Server
            } else {
                *c != ClientId::Server
            }
        })?
        .0;
    let people: Vec<_> = world
        .query::<(Entity, &CrewMember)>()
        .iter(world)
        .map(|(e, m)| (e, m.name.clone()))
        .collect();
    let officer = people.iter().find(|(_, n)| n == REYES)?.0;
    let warden = people.iter().find(|(_, n)| n == BEX)?.0;
    let customer = people.iter().find(|(_, n)| n == "Dr. Vance")?.0;
    let analyzer = world
        .query::<(Entity, &Machine)>()
        .iter(world)
        .find(|(_, m)| m.kind == MachineKind::Analyzer)?
        .0;
    let tops: Vec<_> = world
        .query::<(&Transform, &crate::lab::Solid)>()
        .iter(world)
        .filter(|(t, s)| {
            let top = t.translation.y + s.half_extents.y;
            top > 0.6 && top < 1.4 && s.half_extents.x > 0.45 && s.half_extents.z > 0.3
        })
        .map(|(t, s)| {
            t.translation
                + Vec3::new(
                    s.half_extents.x - 0.15,
                    s.half_extents.y + 0.13,
                    s.half_extents.z - 0.15,
                )
        })
        .collect();
    let position = tops.into_iter().find(|p| {
        world.resource::<crate::lab::WalkableAreas>().room_at(*p) == Some("Mixing Hall")
            && world
                .resource::<crate::nav::NavGraph>()
                .standable_goal(*p)
                .xz()
                .distance(p.xz())
                < 1.6
    })?;
    let clear: Vec<_> = world
        .query_filtered::<Entity, Or<(With<Order>, With<PendingOrder>)>>()
        .iter(world)
        .collect();
    for e in clear {
        world.entity_mut(e).remove::<(
            Order,
            PendingOrder,
            AwaitingConversation,
            crate::order_intake::AcceptedOrder,
            crate::order_intake::queue::QueuePosition,
        )>();
    }
    for e in [officer, warden] {
        world.entity_mut(e).remove::<NpcCommitment>().insert((
            Ambient::new(600.0),
            Body::default(),
            Bloodstream::default(),
        ));
    }
    let reagent = world.resource::<ChemDb>().reagent("kelotane");
    let order = Order {
        reagent,
        specific: true,
        minimum_purity: 0.7,
        amount: Units::whole(20),
        plea: "The patient has chemical burns. Please prepare treatment while we clean the ward."
            .into(),
        patience: 360.0,
        waited: 0.0,
    };
    world.entity_mut(customer).remove::<Ambient>().insert((
        order,
        crate::order_intake::AcceptedOrder { sequence: 9000 },
        crate::order_intake::RequestContext {
            id: 9000,
            source: RequestSource::Ordinary,
            campaign: None,
            greeting: crate::order_intake::GreetingKind::Ordinary,
            step: None,
        },
        NpcCommitment,
        crate::crew::ReturnsToDuty,
        CrewRoute::arrival_for(crate::lab::DeliveryLane::Medical, 0.0),
    ));
    let mut container = Container::new(ContainerKind::Beaker);
    let _ = container.solution.add(reagent, Units::whole(20));
    let batch = world
        .spawn((
            Replicated,
            container,
            crate::labels::Label("Routine burn treatment".into()),
            Transform::from_translation(position),
            Visibility::default(),
            Interactable::new("Bottle"),
            crate::until_we_leave_the_lab(),
        ))
        .id();
    // The scenario seeds the result of a real preparation, not pre-existing stock.
    let reaction = world
        .resource::<ChemDb>()
        .reactions
        .iter()
        .find(|r| r.product_ids().any(|p| p == reagent))
        .unwrap()
        .id;
    world.write_message(crate::machines::ReactionsFired {
        source: None,
        container: batch,
        reactions: vec![reaction],
        effects: vec![],
        distinct_reagents: 2,
    });
    world.resource_mut::<SocialState>().resident_antagonist =
        Some(crate::social::ResidentAntagonist::ReyesBentGuard);
    *world.resource_mut::<SecurityCaseState>() = SecurityCaseState {
        ordinary_deliveries: 3,
        cooldown: 0.0,
        ..default()
    };
    world.resource_mut::<Shift>().accepting_orders = true;
    Some(Fixture {
        step: 0,
        case: 0,
        worker,
        customer,
        batch,
        officer,
        warden,
        analyzer,
        sample: None,
        paper: None,
    })
}
fn drive(world: &mut World) {
    let Some(local) = world
        .query_filtered::<Entity, With<LocalPlayer>>()
        .iter(world)
        .next()
    else {
        return;
    };
    let started = *world
        .resource_mut::<Scenario>()
        .started
        .get_or_insert_with(std::time::Instant::now);
    let elapsed = started.elapsed().as_secs_f32();
    let authority = !matches!(world.resource::<LaunchMode>(), LaunchMode::Join(_));
    let found = world
        .query::<(Entity, &Fixture)>()
        .iter(world)
        .next()
        .map(|(e, f)| (e, f.clone()));
    let Some((entity, mut f)) = found else {
        if authority && elapsed > 4.0 {
            if let Some(f) = seed(world) {
                world.spawn((Replicated, f, crate::until_we_leave_the_lab()));
            }
        }
        return;
    };
    let worker = local == f.worker;
    if world.resource::<Scenario>().phase != Some(f.step) {
        let mut scenario = world.resource_mut::<Scenario>();
        scenario.phase = Some(f.step);
        scenario.phase_at = elapsed;
        scenario.panel_at = None;
    }
    if elapsed >= world.resource::<Scenario>().debug_at {
        let people:Vec<_>=world.query::<(Entity,&CrewMember,&Transform,Option<&CrewRoute>,Option<&crate::order_intake::queue::QueuePosition>,Option<&AwaitingConversation>,&Body,&Bloodstream)>()
            .iter(world).map(|(entity,member,at,route,queue,pending,body,blood)|format!("{} {entity:?}: {:?}; route {:?}, moving {}, failed {}, details {:?}; queue {:?}, pending {:?}, collapsed {}, incapacitated {}",member.name,at.translation,route.map(|r|r.phase),route.is_some_and(|r|r.is_moving()),route.is_some_and(|r|r.routing_failed()),route.map(|r|r.routing_diagnostic()),queue.map(|q|(q.target,q.reached,q.pickup)),pending,body.0.collapsed,blood.0.incapacitated())).collect();
        let path = world
            .get::<Transform>(f.officer)
            .zip(world.get::<crate::order_intake::queue::QueuePosition>(f.officer))
            .map(|(at, queue)| {
                world
                    .resource::<crate::nav::NavGraph>()
                    .path(at.translation, queue.target)
            });
        let mode = world.get::<InteractionMode>(local).cloned();
        let local_at = world.get::<Transform>(local).map(|t| t.translation);
        let status = if authority {
            format!("{:?}", world.resource::<SecurityCaseState>())
        } else {
            format!("{:?}", world.resource::<SecurityCaseSummary>())
        };
        let out = output(world);
        let _ = std::fs::create_dir_all(&out);
        let mut scenario = world.resource_mut::<Scenario>();
        scenario.debug_at = elapsed + 10.0;
        scenario.diagnostics.push_str(&format!("\n{elapsed:.1}s step {}; local {local:?} at {local_at:?}, mode {mode:?}; {status}\nReyes planned path {path:?}\n{}\n",f.step,people.join("\n")));
        let _ = std::fs::write(out.join("route-diagnostics.txt"), &scenario.diagnostics);
    }
    if authority
        && f.case != 0
        && f.step < 7
        && world.resource::<SecurityCaseState>().active.is_none()
    {
        let status = format!(
            "FAILED: case withdrawn at step {}\n{:?}",
            f.step,
            world.resource::<SecurityCaseState>()
        );
        let out = output(world);
        let _ = std::fs::create_dir_all(&out);
        let _ = std::fs::write(out.join("result.txt"), status);
        world.write_message(AppExit::Success);
        return;
    }
    if elapsed > 260.0 && f.step != 10 {
        let out = output(world);
        let _ = std::fs::create_dir_all(&out);
        let status = if authority {
            format!("{:?}", world.resource::<SecurityCaseState>())
        } else {
            format!("{:?}", world.resource::<SecurityCaseSummary>())
        };
        let _ = std::fs::write(
            out.join("result.txt"),
            format!("FAILED at step {}\n{status}", f.step),
        );
        world.write_message(AppExit::Success);
        return;
    }
    let phase_age = elapsed - world.resource::<Scenario>().phase_at;
    match f.step {
        0 => {
            if authority {
                if let Some(case) = world.resource::<SecurityCaseState>().active.as_ref() {
                    f.case = case.id;
                    world.entity_mut(entity).insert(f.clone());
                }
                if world
                    .get::<AwaitingConversation>(f.officer)
                    .is_some_and(|p| p.arrived)
                {
                    place_actor(world, f.worker, f.officer);
                    world.resource_mut::<Shift>().accepting_orders = false;
                    next(world, entity, &mut f, 1, elapsed);
                }
            }
        }
        1 => {
            if worker
                && world
                    .get::<InteractionMode>(local)
                    .is_some_and(|m| m.is_roaming())
            {
                if let Some(id) = world
                    .get::<AwaitingConversation>(f.officer)
                    .filter(|pending| pending.arrived)
                    .map(|pending| pending.id)
                {
                    if once(world, f.step, 0) {
                        world.write_message(OpenOrderConversation {
                            target: f.officer,
                            id,
                        });
                    }
                }
            }
            if worker
                && world.get::<InteractionMode>(local)
                    == Some(&InteractionMode::SecurityConversation)
            {
                let age = panel_age(world, elapsed);
                if age > 0.75 {
                    shot(world, "01-recorded-hold-conversation");
                }
                if age > 3.0 && once(world, f.step, 1) {
                    world.write_message(CaseActionRequested {
                        target: f.officer,
                        case: f.case,
                        action: CaseAction::RecordHold,
                    });
                    world.entity_mut(local).insert(InteractionMode::Roaming);
                }
            }
            if authority
                && world
                    .resource::<SecurityCaseState>()
                    .active
                    .as_ref()
                    .is_some_and(|c| c.stage == Stage::Inspecting)
            {
                next(world, entity, &mut f, 2, elapsed);
            }
        }
        2 => {
            if authority && world.get::<CaseCustody>(f.batch).is_some() {
                assert!(world.get::<OrderHold>(f.customer).is_some());
                assert_eq!(
                    world
                        .get::<Container>(f.batch)
                        .unwrap()
                        .solution
                        .total_volume(),
                    Units::whole(19)
                );
                f.sample = world
                    .query::<(Entity, &CaseSample)>()
                    .iter(world)
                    .find(|(_, s)| s.0 == f.case)
                    .map(|(e, _)| e);
                let Some(sample) = f.sample else {
                    return;
                };
                world
                    .entity_mut(sample)
                    .remove::<(HeldBy, InventorySlot)>()
                    .insert(InSlot(f.analyzer));
                world.get_mut::<Machine>(f.analyzer).unwrap().in_use_by = Some(f.worker);
                next(world, entity, &mut f, 3, elapsed);
            }
        }
        3 => {
            if worker && once(world, f.step, 0) {
                world
                    .entity_mut(local)
                    .insert(InteractionMode::UsingMachine(f.analyzer));
                world.write_message(crate::machines::AnalyzeRequested {
                    machine: f.analyzer,
                });
            }
            if authority
                && world
                    .get::<crate::analysis_reports::AnalyzerSnapshot>(f.analyzer)
                    .is_some()
            {
                next(world, entity, &mut f, 4, elapsed);
            }
        }
        4 => {
            if worker {
                if phase_age > 0.75 {
                    shot(world, "02-reference-analysis");
                }
                if let Some(report) = world
                    .get::<crate::analysis_reports::AnalyzerSnapshot>(f.analyzer)
                    .map(|s| s.report.id)
                {
                    if phase_age > 2.0 && once(world, f.step, 0) {
                        world.write_message(crate::analysis_reports::PrintReportRequested {
                            machine: f.analyzer,
                            report_id: report,
                        });
                    }
                }
            }
            if authority {
                f.paper = world
                    .query::<(Entity, &AnalysisReport)>()
                    .iter(world)
                    .find(|(_, r)| r.case == Some(f.case))
                    .map(|(e, _)| e);
                if f.paper.is_some() {
                    next(world, entity, &mut f, 5, elapsed);
                }
            }
        }
        5 => {
            if worker && once(world, f.step, 0) {
                world.entity_mut(local).insert(InteractionMode::Roaming);
                world.write_message(InteractRequested {
                    target: f.paper.unwrap(),
                });
            }
            if authority && world.get::<InventorySlot>(f.paper.unwrap()).is_some() {
                let sample = f.sample.unwrap();
                world
                    .entity_mut(sample)
                    .remove::<InSlot>()
                    .insert(InventorySlot {
                        owner: f.worker,
                        slot: 3,
                    });
                world.get_mut::<Machine>(f.analyzer).unwrap().in_use_by = None;
                place_actor(world, f.worker, f.warden);
                next(world, entity, &mut f, 6, elapsed);
            }
        }
        6 => {
            if worker && once(world, f.step, 0) {
                world.entity_mut(local).insert(InteractionMode::Roaming);
                world.write_message(InteractRequested { target: f.warden });
            }
            if worker
                && world.get::<InteractionMode>(local)
                    == Some(&InteractionMode::SecurityConversation)
            {
                let age = panel_age(world, elapsed);
                if age > 0.75 {
                    shot(world, "03-bex-appeal");
                }
                if age > 3.0 && once(world, f.step, 1) {
                    world.write_message(CaseActionRequested {
                        target: f.warden,
                        case: f.case,
                        action: CaseAction::PresentReport,
                    });
                    world.entity_mut(local).insert(InteractionMode::Roaming);
                }
            }
            if authority
                && world
                    .resource::<SecurityCaseState>()
                    .active
                    .as_ref()
                    .is_some_and(|c| c.stage == Stage::Released)
            {
                assert!(world.get::<OrderHold>(f.customer).is_some());
                next(world, entity, &mut f, 7, elapsed);
            }
        }
        7 => {
            if worker && once(world, f.step, 0) {
                world.write_message(InteractRequested { target: f.warden });
            }
            if worker
                && world.get::<InteractionMode>(local)
                    == Some(&InteractionMode::SecurityConversation)
            {
                let age = panel_age(world, elapsed);
                if age > 0.75 {
                    shot(world, "04-released-batch");
                }
                if age > 3.0 && once(world, f.step, 1) {
                    let request = CaseActionRequested {
                        target: f.warden,
                        case: f.case,
                        action: CaseAction::Collect,
                    };
                    world.write_message(request.clone());
                    world.write_message(request);
                    world.entity_mut(local).insert(InteractionMode::Roaming);
                }
            }
            if authority && world.resource::<SecurityCaseState>().active.is_none() {
                assert!(world.get::<OrderHold>(f.customer).is_none());
                assert!(world.get::<CaseCustody>(f.batch).is_none());
                assert_eq!(
                    world
                        .get::<Container>(f.batch)
                        .unwrap()
                        .solution
                        .total_volume(),
                    Units::whole(20)
                );
                next(world, entity, &mut f, 8, elapsed);
            }
        }
        8 => {
            if worker && phase_age > 0.75 {
                shot(world, "05-collected-batch");
            }
            if authority && phase_age > 3.0 {
                next(world, entity, &mut f, 10, elapsed);
            }
        }
        10 => {
            let out = output(world);
            let _ = std::fs::create_dir_all(&out);
            let held = world.get::<OrderHold>(f.customer).is_some();
            let quantity = world
                .get::<Container>(f.batch)
                .map(|c| c.solution.total_volume());
            let _=std::fs::write(out.join("result.txt"),format!("Security physical case completed\nHeld order resumed: {}\nRestored batch: {:?}\nDuplicate collection rejected: true\nAuthority: {}\n",!held,quantity,authority));
            if !authority || phase_age > 2.0 {
                world.write_message(AppExit::Success);
            }
        }
        _ => {}
    }
}
