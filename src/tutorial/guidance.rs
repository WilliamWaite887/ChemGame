//! One current-task callout, anchored to the real object rather than a fixed bench.
use super::*;
use crate::containers::{InventorySlot, Stored};

#[derive(Component)]
struct Callout;

fn nearest(
    world: &mut World,
    origin: Vec3,
    accepts: impl Fn(&World, Entity) -> bool,
) -> Option<Entity> {
    world
        .query::<(Entity, &Transform)>()
        .iter(world)
        .filter(|(e, _)| accepts(world, *e))
        // Prefer an item already carried/loaded over another unused supply.
        .min_by_key(|(e, at)| {
            (
                !(world.get::<InventorySlot>(*e).is_some()
                    || world.get::<HeldBy>(*e).is_some()
                    || world.get::<InSlot>(*e).is_some()),
                (at.translation.distance_squared(origin) * 1000.0) as u32,
                e.to_bits(),
            )
        })
        .map(|(e, _)| e)
}

fn anchor(world: &World, entity: Entity, text: String) -> Option<(Vec3, String)> {
    let at = world.get::<Transform>(entity)?.translation;
    let height = if world.get::<Actor>(entity).is_some() {
        0.9
    } else {
        0.35
    };
    Some((at + Vec3::Y * height, text))
}

/// Follow dropped items. A loaded sample points back to its machine for ejection;
/// a carried sample points onward to the next station. Names never read chemistry.
fn collect_item(
    world: &World,
    entity: Entity,
    name: &str,
    destination: Option<Entity>,
) -> Option<(Vec3, String)> {
    if let Some(slot) = world.get::<InSlot>(entity) {
        return (Some(slot.0) != destination)
            .then(|| anchor(world, slot.0, format!("Eject {name} · {{interact}}")))
            .flatten();
    }
    if world.get::<InventorySlot>(entity).is_some() || world.get::<HeldBy>(entity).is_some() {
        return None;
    }
    if let Some(stored) = world.get::<Stored>(entity) {
        return anchor(world, stored.0, format!("Collect {name} · {{interact}}"));
    }
    anchor(world, entity, format!("Pick up {name} · {{interact}}"))
}

fn target(world: &mut World, stage: &Stage, eye: Vec3) -> Option<(Vec3, String)> {
    let destination = world
        .query::<(Entity, &crate::lab::MachineSpotId)>()
        .iter(world)
        .find(|(_, id)| id.0 == stage.focus)
        .map(|(e, _)| e);
    let lesson = world.resource::<Runner>().lesson.clone();
    let (item, name) = match stage.goal {
        Goal::Pickup | Goal::Load => (
            nearest(world, eye, |w, e| {
                w.get::<Container>(e)
                    .is_some_and(|c| c.kind == ContainerKind::Beaker && c.solution.is_empty())
            }),
            "empty beaker",
        ),
        Goal::Retrieve => (
            nearest(world, eye, |w, e| {
                w.resource::<Runner>().evidence.loaded.contains(&e)
            }),
            "beaker",
        ),
        Goal::AnalyzeMistake | Goal::StaleReport | Goal::Reanalyzed => (
            nearest(world, eye, |w, e| w.get::<MistakeSample>(e).is_some()),
            "labelled sample",
        ),
        Goal::Warm | Goal::Cool | Goal::RemovedWarm => (
            nearest(world, eye, |w, e| {
                w.get::<crate::labels::Label>(e)
                    .is_some_and(|l| l.0 == "Nitrogen practice sample")
            }),
            "Nitrogen sample",
        ),
        Goal::PhShift | Goal::PhReturn => (
            nearest(world, eye, |w, e| {
                w.get::<crate::labels::Label>(e)
                    .is_some_and(|l| l.0 == "Water practice sample")
            }),
            "Water sample",
        ),
        Goal::PhStrip => (
            nearest(world, eye, |w, e| {
                w.get::<Container>(e)
                    .is_some_and(|c| c.kind == ContainerKind::PhPaper)
            }),
            "pH paper",
        ),
        Goal::Analyze | Goal::TwoForms => (
            nearest(world, eye, |w, e| {
                w.get::<Container>(e).is_some_and(|c| {
                    // Visible volume and glassware, never unmeasured reagent identity.
                    matches!(
                        c.kind,
                        ContainerKind::Beaker | ContainerKind::Bottle | ContainerKind::LargeBeaker
                    ) && !c.solution.is_empty()
                        && (lesson != "independent"
                            || c.solution.total_volume() >= Units::whole(20))
                })
            }),
            "sample",
        ),
        Goal::Inspect | Goal::Deliver => (
            nearest(world, eye, |w, e| {
                w.resource::<Runner>().evidence.packages.contains(&e)
            }),
            "bottle",
        ),
        Goal::Treated => (
            nearest(world, eye, |w, e| {
                w.get::<Container>(e)
                    .is_some_and(|c| c.kind == ContainerKind::Syringe && !c.solution.is_empty())
            }),
            "syringe",
        ),
        Goal::Ground => (
            nearest(world, eye, |w, e| {
                w.get::<crate::produce::Produce>(e).is_some()
            }),
            "Aloe",
        ),
        Goal::Printed => (
            nearest(world, eye, |w, e| {
                w.get::<crate::analysis_reports::AnalysisReport>(e)
                    .is_some()
            }),
            "printed report",
        ),
        Goal::Puddle => (
            nearest(world, eye, |w, e| {
                w.get::<crate::labels::Label>(e)
                    .is_some_and(|l| l.0 == "Water - spill practice")
            }),
            "Water beaker",
        ),
        _ => (None, "item"),
    };
    if let Some(item) = item {
        if let Some(target) = collect_item(world, item, name, destination) {
            return Some(target);
        }
    }
    // Reading and inventory-only objectives have no world destination.
    if matches!(
        stage.goal,
        Goal::Select
            | Goal::Textbook
            | Goal::Book
            | Goal::Directory
            | Goal::Inspect
            | Goal::PhStrip
    ) {
        return None;
    }
    if let Some(entity) = destination {
        return anchor(world, entity, stage.marker.clone());
    }
    let actor = match stage.focus.as_str() {
        "instructor" => Some(Actor::Instructor),
        "customer" => Some(Actor::Customer),
        "patient" => Some(Actor::Patient),
        "cleanup" => Some(Actor::Cleanup),
        _ => None,
    };
    if let Some(kind) = actor {
        // Use the current person, so the marker follows the next practice customer.
        let entity = nearest(world, eye, |w, e| {
            w.get::<Actor>(e) == Some(&kind)
                && (kind != Actor::Customer
                    || w.get::<crate::orders::Order>(e).is_some()
                    || w.get::<crate::order_intake::PendingOrder>(e).is_some())
        })?;
        return anchor(world, entity, stage.marker.clone());
    }
    world
        .resource::<TrainingSpots>()
        .0
        .get(&stage.focus)
        .map(|at| (at.translation + Vec3::Y * 0.4, stage.marker.clone()))
}

pub(super) fn draw(world: &mut World) {
    let stage = {
        let runner = world.resource::<Runner>();
        (!runner.finished && !world.resource::<crate::settings::Paused>().0)
            .then(|| {
                world
                    .resource::<Lessons>()
                    .0
                    .iter()
                    .find(|l| l.id == runner.lesson)
                    .and_then(|l| l.stages.get(runner.stage))
                    .cloned()
            })
            .flatten()
    };
    let player = world
        .query_filtered::<(&Transform, &InteractionMode), With<LocalPlayer>>()
        .iter(world)
        .next()
        .map(|(at, mode)| (at.translation, mode.is_roaming()));
    let selected = stage.and_then(|stage| {
        player
            .filter(|(_, roaming)| *roaming)
            .and_then(|(eye, _)| target(world, &stage, eye))
    });
    let screen = selected.and_then(|(at, text)| {
        let (camera, transform) = world
            .query_filtered::<(&Camera, &GlobalTransform), With<crate::player::PlayerCamera>>()
            .iter(world)
            .next()?;
        let size = camera.logical_viewport_size()?;
        let expanded =
            crate::textbook::expand(&text, world.resource::<crate::settings::Settings>());
        let (point, text) = match camera.world_to_viewport(transform, at) {
            Ok(point) => (point, format!("{expanded}\n▼")),
            Err(_) => (
                Vec2::new(size.x * 0.5, size.y - 110.0),
                format!("Turn around · {expanded}"),
            ),
        };
        Some((
            Vec2::new(
                (point.x - 120.0).clamp(8.0, (size.x - 248.0).max(8.0)),
                (point.y - 62.0).clamp(8.0, (size.y - 120.0).max(8.0)),
            ),
            text,
        ))
    });
    let existing = world
        .query_filtered::<Entity, With<Callout>>()
        .iter(world)
        .next();
    let Some((point, text)) = screen else {
        if let Some(entity) = existing {
            world.entity_mut(entity).insert(Visibility::Hidden);
        }
        return;
    };
    let entity = existing.unwrap_or_else(|| {
        world
            .spawn((
                Callout,
                crate::until_we_leave_the_lab(),
                GlobalZIndex(11),
                Node {
                    position_type: PositionType::Absolute,
                    width: px(240),
                    padding: UiRect::all(px(8)),
                    border: UiRect::all(px(1)),
                    ..default()
                },
                BackgroundColor(crate::ui::PANEL_BG),
                BorderColor::all(Color::srgb(0.4, 0.95, 0.8)),
                Text::default(),
                TextFont {
                    font_size: FontSize::Px(16.0),
                    ..default()
                },
                TextColor(crate::ui::TEXT),
                TextLayout::justify(Justify::Center),
                bevy::ui::FocusPolicy::Pass,
            ))
            .id()
    });
    world
        .get_mut::<Text>(entity)
        .unwrap()
        .set_if_neq(Text::new(text));
    world.entity_mut(entity).insert(Visibility::Visible);
    let mut node = world.get_mut::<Node>(entity).unwrap();
    node.left = px(point.x);
    node.top = px(point.y);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(lesson: &str, goal: Goal) -> (World, Stage) {
        let mut world = World::new();
        let lessons = Lessons::default();
        let stage = lessons
            .0
            .iter()
            .find(|l| l.id == lesson)
            .unwrap()
            .stages
            .iter()
            .find(|s| s.goal == goal)
            .unwrap()
            .clone();
        world.insert_resource(lessons);
        world.insert_resource(Runner::new(lesson.into()));
        world.init_resource::<TrainingSpots>();
        (world, stage)
    }

    #[test]
    fn marker_follows_dropped_glassware_then_points_to_its_destination() {
        let (mut world, stage) = setup("bearings", Goal::Load);
        let chemmaster5000 = world
            .spawn((
                crate::lab::MachineSpotId("training.chemmaster5000".into()),
                Transform::from_xyz(4.0, 1.0, 0.0),
            ))
            .id();
        let beaker = world
            .spawn((
                Container::new(ContainerKind::Beaker),
                Transform::from_xyz(1.0, 1.0, 0.0),
            ))
            .id();
        let first = target(&mut world, &stage, Vec3::ZERO).unwrap();
        assert_eq!(first.0.x, 1.0);
        assert!(first.1.starts_with("Pick up empty beaker"));
        world.get_mut::<Transform>(beaker).unwrap().translation.x = 2.0;
        assert_eq!(target(&mut world, &stage, Vec3::ZERO).unwrap().0.x, 2.0);
        let owner = world.spawn_empty().id();
        world
            .entity_mut(beaker)
            .insert(InventorySlot { owner, slot: 0 });
        let carrying = target(&mut world, &stage, Vec3::ZERO).unwrap();
        assert_eq!(
            carrying.0.x,
            world.get::<Transform>(chemmaster5000).unwrap().translation.x
        );
        assert!(carrying.1.starts_with("Use ChemMaster 5000"));
        let other = world.spawn(Transform::from_xyz(6.0, 1.0, 0.0)).id();
        world
            .entity_mut(beaker)
            .remove::<InventorySlot>()
            .insert(InSlot(other));
        let loaded_elsewhere = target(&mut world, &stage, Vec3::ZERO).unwrap();
        assert_eq!(loaded_elsewhere.0.x, 6.0);
        assert!(loaded_elsewhere.1.starts_with("Eject empty beaker"));
    }

    #[test]
    fn sample_marker_never_reveals_unmeasured_contents() {
        let (mut world, stage) = setup("mistake", Goal::AnalyzeMistake);
        world.spawn((
            crate::lab::MachineSpotId("training.analyzer".into()),
            Transform::from_xyz(4.0, 1.0, 0.0),
        ));
        world.spawn((
            MistakeSample,
            Container::new(ContainerKind::Bottle),
            crate::labels::Label("Kelotane".into()),
            Transform::from_xyz(1.0, 1.0, 0.0),
        ));
        let (_, text) = target(&mut world, &stage, Vec3::ZERO).unwrap();
        assert_eq!(text, "Pick up labelled sample · {interact}");
        let mut settings = crate::settings::Settings::default();
        settings.bindings.interact = KeyCode::KeyJ;
        assert_eq!(
            crate::textbook::expand(&text, &settings),
            "Pick up labelled sample · J"
        );
    }
}
