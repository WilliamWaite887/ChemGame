//! Authority-only stepping and newly produced material on existing puddles.
use super::*;
use std::collections::HashMap;

#[derive(Component)]
pub(super) struct PuddleActivation(pub Solution);

pub(super) fn finish_activations(
    mut commands: Commands,
    puddles: Query<Entity, With<PuddleActivation>>,
) {
    for entity in &puddles {
        commands.entity(entity).remove::<PuddleActivation>();
    }
}

pub(super) fn seat_puddles(
    areas: Option<Res<crate::lab::WalkableAreas>>,
    mut puddles: Query<&mut Transform, Added<ChemicalPuddle>>,
) {
    for mut transform in &mut puddles {
        let point = transform.translation;
        let floor = areas
            .as_ref()
            .and_then(|areas| {
                areas
                    .regions()
                    .iter()
                    .filter(|r| {
                        point.x >= r.bounds.min_x
                            && point.x <= r.bounds.max_x
                            && point.z >= r.bounds.min_z
                            && point.z <= r.bounds.max_z
                    })
                    .map(|r| r.floor_at(point))
                    .filter(|height| *height <= point.y + 0.05)
                    .max_by(f32::total_cmp)
            })
            .unwrap_or(0.0);
        transform.translation.y = floor + 0.018;
    }
}

pub(super) fn connected(
    a: Vec3,
    b: Vec3,
    solids: &Query<(Entity, &Transform, &crate::lab::Solid)>,
    left: Entity,
    right: Entity,
) -> bool {
    (a.y - b.y).abs() <= 0.15
        && !solids.iter().any(|(entity, transform, solid)| {
            entity != left
                && entity != right
                && crate::interaction::authority_segment_blocked(
                    a + Vec3::Y * 0.05,
                    b + Vec3::Y * 0.05,
                    transform.translation,
                    solid.half_extents,
                )
        })
}

pub(super) fn react_puddles(
    mut commands: Commands,
    db: Res<ChemDb>,
    time: Res<Time>,
    mut clocks: Local<HashMap<Entity, f32>>,
    mut puddles: Query<(Entity, &Transform, &mut ChemicalPuddle)>,
    mut reports: MessageWriter<crate::machines::ReactionsFired>,
) {
    clocks.retain(|entity, _| puddles.contains(*entity));
    for (entity, transform, mut stored) in &mut puddles {
        let mut puddle = stored.clone();
        let dt = clocks.entry(entity).or_default();
        *dt = (*dt + time.delta_secs()).min(2.0);
        let before = puddle.solution.clone();
        let mut activation = Solution::unbounded();
        // Instant chemistry on merge, then the same 0.1s quantum as containers.
        let steps = (*dt / 0.1).floor() as usize;
        *dt -= steps as f32 * 0.1;
        for step in 0..=steps {
            let elapsed = if step == 0 { 0.0 } else { 0.1 };
            let mut environment = chem_sim::ReactionEnvironment::room(elapsed);
            environment.ignited = puddle.ignited;
            let report = chem_sim::resolve_in_environment(
                &mut puddle.solution,
                &db.reactions,
                elapsed,
                None,
                &mut environment,
            );
            if report
                .effects
                .iter()
                .any(|e| matches!(e, chem_sim::ReactionEffect::Burn(p) if *p > 0.0))
            {
                puddle.ignited = true;
                puddle.ignition_intensity = 0.75;
            }
            if let Some(mut message) = crate::machines::ReactionsFired::from_report(entity, &report)
            {
                message.source = Some(crate::hazards::ReactionOrigin {
                    kind: crate::hazards::ReactionSource::Puddle,
                    position: transform.translation,
                    owner: puddle.owner,
                });
                reports.write(message);
            }
        }
        for (reagent, amount) in puddle.solution.iter() {
            let added = (amount - before.volume_of(reagent)).clamp_non_negative();
            if added.is_positive() && !db.reagents.get(reagent).world_effects.is_empty() {
                let _ = activation.add_profiled(
                    reagent,
                    added,
                    puddle.solution.purity_of(reagent),
                    puddle.solution.reagent_ph(reagent),
                );
            }
        }
        if !activation.is_empty() {
            commands.entity(entity).insert(PuddleActivation(activation));
        }
        let has = |test: fn(&WorldEffect) -> bool| {
            puddle
                .solution
                .iter()
                .any(|(id, _)| db.reagents.get(id).world_effects.iter().any(test))
        };
        let fuel = has(|e| matches!(e, WorldEffect::Flammable { .. }))
            || puddle
                .solution
                .iter()
                .any(|(id, _)| db.reagents.get(id).material.fuel.is_some());
        if !fuel {
            puddle.flammable_intensity = 0.0;
            puddle.flammable_remaining = 0.0;
            if !has(|e| matches!(e, WorldEffect::Ignite { .. })) {
                puddle.ignited = false;
                puddle.ignition_intensity = 0.0;
            }
        }
        if !has(|e| matches!(e, WorldEffect::Slippery { .. })) {
            puddle.slippery = 0.0;
        }
        if !has(|e| matches!(e, WorldEffect::Chill { .. })) {
            puddle.chill_intensity = 0.0;
        }
        puddle.radius = radius_for(puddle.solution.total_volume()).max(puddle.foam_radius);
        if *stored != puddle {
            *stored = puddle;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::ReactionsFired;
    fn app() -> App {
        let mut app = App::new();
        app.insert_resource(ChemDb(
            chem_sim::ChemData::from_ron(
                include_str!("../../assets/data/chem.reagents.ron"),
                include_str!("../../assets/data/chem.reactions.ron"),
            )
            .unwrap(),
        ))
        .init_resource::<Time>()
        .add_message::<ReactionsFired>()
        .add_systems(Update, (merge_puddles, react_puddles).chain());
        app
    }
    fn puddle(app: &mut App, key: &str, amount: i32, position: Vec3) -> Entity {
        let db = app.world().resource::<ChemDb>();
        let id = db.reagent(key);
        let mut s = Solution::unbounded();
        let _ = s.add_profiled(id, Units::whole(amount), 1.0, db.reagents.get(id).ph);
        app.world_mut()
            .spawn((
                ChemicalPuddle::from_solution(s, None),
                Transform::from_translation(position),
            ))
            .id()
    }

    #[test]
    fn sb11_overlapping_pools_react_once_and_retain_ash() {
        let mut app = app();
        puddle(&mut app, "potassium", 5, Vec3::ZERO);
        puddle(&mut app, "water", 5, Vec3::new(0.3, 0.0, 0.0));
        app.update();
        let ash = app.world().resource::<ChemDb>().reagent("ash");
        let all: Vec<_> = app
            .world_mut()
            .query::<&ChemicalPuddle>()
            .iter(app.world())
            .collect();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].solution.volume_of(ash), Units::whole(10));
        assert_eq!(
            app.world_mut()
                .resource_mut::<Messages<ReactionsFired>>()
                .drain()
                .count(),
            1
        );
        app.update();
        assert_eq!(
            app.world_mut()
                .resource_mut::<Messages<ReactionsFired>>()
                .drain()
                .count(),
            0
        );
    }

    #[test]
    fn sb12_pools_cannot_merge_through_walls_or_between_levels() {
        for wall in [false, true] {
            let mut app = app();
            puddle(&mut app, "potassium", 5, Vec3::new(-0.2, 0.0, 0.0));
            puddle(
                &mut app,
                "water",
                5,
                Vec3::new(0.2, if wall { 0.0 } else { 2.0 }, 0.0),
            );
            if wall {
                app.world_mut().spawn((
                    Transform::from_xyz(0.0, 0.5, 0.0),
                    crate::lab::Solid {
                        half_extents: Vec3::new(0.05, 1.0, 2.0),
                    },
                ));
            }
            app.update();
            assert_eq!(
                app.world_mut()
                    .query::<&ChemicalPuddle>()
                    .iter(app.world())
                    .count(),
                2
            );
            assert_eq!(
                app.world_mut()
                    .resource_mut::<Messages<ReactionsFired>>()
                    .drain()
                    .count(),
                0
            );
        }
    }

    #[test]
    fn sb13_puddle_wire_roundtrip_preserves_composition_without_activation_replay() {
        let mut app = app();
        let id = puddle(&mut app, "water", 10, Vec3::ZERO);
        let original = app.world().get::<ChemicalPuddle>(id).unwrap();
        let bytes = postcard::to_allocvec(original).unwrap();
        let decoded: ChemicalPuddle = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(*original, decoded);
    }
}
