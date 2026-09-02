//! Session-local production provenance. Existing stock is never assigned by a scan.
use super::*;
use crate::machines::{Buffer, ReactionsFired};
use crate::order_intake::{AcceptedOrder, RequestContext};
use chem_sim::{ReagentId, Solution};
use std::collections::HashMap;

#[derive(Clone)]
pub(crate) struct PreparedForOrder {
    pub request: u64,
    profile: Vec<(ReagentId, f32, f32, f32)>,
}
impl PreparedForOrder {
    fn includes_profiles(&self, solution: &Solution) -> bool {
        solution.iter().all(|(r, _)| {
            self.profile.iter().any(|(known, _, purity, ph)| {
                *known == r
                    && (solution.purity_of(r) - purity).abs() < 0.0001
                    && (solution.reagent_ph(r) - ph).abs() < 0.0001
            })
        })
    }
    fn new(request: u64, solution: &Solution) -> Self {
        let total = solution.total_volume().as_f32();
        Self {
            request,
            profile: solution
                .iter()
                .map(|(r, amount)| {
                    (
                        r,
                        amount.as_f32() / total,
                        solution.purity_of(r),
                        solution.reagent_ph(r),
                    )
                })
                .collect(),
        }
    }
    pub fn matches(&self, solution: &Solution) -> bool {
        let total = solution.total_volume().as_f32();
        total > 0.0
            && solution.iter().count() == self.profile.len()
            && self.profile.iter().all(|(r, fraction, purity, ph)| {
                (solution.volume_of(*r).as_f32() / total - fraction).abs() < 0.001
                    && (solution.purity_of(*r) - purity).abs() < 0.0001
                    && (solution.reagent_ph(*r) - ph).abs() < 0.0001
            })
    }
}

#[derive(Resource, Default)]
pub(crate) struct PreparedBatches(HashMap<Entity, PreparedForOrder>);
impl PreparedBatches {
    pub fn get(&self, entity: Entity, solution: &Solution) -> Option<PreparedForOrder> {
        self.0.get(&entity).filter(|p| p.matches(solution)).cloned()
    }
    pub fn set(&mut self, entity: Entity, prepared: Option<PreparedForOrder>, solution: &Solution) {
        if let Some(prepared) = prepared.filter(|p| p.matches(solution)) {
            self.0.insert(entity, prepared);
        } else {
            self.0.remove(&entity);
        }
    }
    pub fn transfer(
        &mut self,
        from: Entity,
        to: Entity,
        source: &Solution,
        destination: &Solution,
        source_before: Option<PreparedForOrder>,
        destination_before: Option<PreparedForOrder>,
        was_empty: bool,
    ) {
        let incoming = source_before.clone().filter(|p| {
            was_empty
                || destination_before
                    .as_ref()
                    .is_some_and(|old| old.request == p.request)
        });
        // A deliberate reagent transfer may change proportions. Preserve its
        // production identity only for unchanged constituent measurements.
        let outgoing = source_before
            .filter(|p| p.includes_profiles(source))
            .map(|p| PreparedForOrder::new(p.request, source));
        let incoming = incoming.and_then(|mut p| {
            if let Some(old) = destination_before {
                p.profile.extend(old.profile);
            }
            p.includes_profiles(destination)
                .then(|| PreparedForOrder::new(p.request, destination))
        });
        self.set(from, outgoing, source);
        self.set(to, incoming, destination);
    }
}

pub(super) fn install(app: &mut App) {
    app.init_resource::<PreparedBatches>()
        .add_message::<ReactionsFired>()
        .add_systems(
            PostUpdate,
            record
                .run_if(crate::net::is_authority)
                .run_if(in_state(AppState::Playing)),
        )
        .add_systems(
            OnExit(AppState::Playing),
            |mut batches: ResMut<PreparedBatches>| batches.0.clear(),
        );
}

fn record(
    mut events: MessageReader<ReactionsFired>,
    db: Res<ChemDb>,
    containers: Query<&Container>,
    buffers: Query<&Buffer>,
    orders: Query<(&Order, &RequestContext), With<AcceptedOrder>>,
    mut batches: ResMut<PreparedBatches>,
) {
    let active: Vec<_> = orders
        .iter()
        .filter(|(_, c)| c.source == RequestSource::Ordinary && c.campaign.is_none())
        .collect();
    batches.0.retain(|entity, p| {
        active.iter().any(|(_, c)| c.id == p.request)
            && containers
                .get(*entity)
                .map(|c| &c.solution)
                .or_else(|_| buffers.get(*entity).map(|b| &b.0))
                .is_ok_and(|s| p.matches(s))
    });
    for event in events.read() {
        if event.distinct_reagents == 0 || event.reactions.is_empty() {
            continue;
        }
        let Ok(solution) = containers
            .get(event.container)
            .map(|c| &c.solution)
            .or_else(|_| buffers.get(event.container).map(|b| &b.0))
        else {
            continue;
        };
        let container = Container {
            kind: ContainerKind::Beaker,
            solution: solution.clone(),
        };
        let chosen = active
            .iter()
            .filter(|(order, context)| {
                eligible(&container, order, &db)
                    && !batches.0.values().any(|p| p.request == context.id)
            })
            .min_by_key(|(_, context)| context.id);
        if let Some((_, context)) = chosen {
            batches
                .0
                .insert(event.container, PreparedForOrder::new(context.id, solution));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stock_and_scans_are_not_production_but_real_fresh_batches_bind_once() {
        let db = super::super::tests::db();
        let order = super::super::tests::order(&db);
        let c = super::super::tests::chemical(&db, "kelotane");
        let mut app = App::new();
        app.insert_resource(db)
            .init_resource::<PreparedBatches>()
            .add_message::<ReactionsFired>()
            .add_systems(Update, record);
        app.world_mut().spawn((
            order,
            AcceptedOrder { sequence: 7 },
            RequestContext {
                id: 7,
                source: RequestSource::Ordinary,
                campaign: None,
                greeting: crate::order_intake::GreetingKind::Ordinary,
                step: None,
            },
        ));
        let stock = app
            .world_mut()
            .spawn(Container {
                kind: c.kind,
                solution: c.solution.clone(),
            })
            .id();
        let fresh = app.world_mut().spawn(c).id();
        app.update();
        assert!(app.world().resource::<PreparedBatches>().0.is_empty());
        let reaction = app
            .world()
            .resource::<ChemDb>()
            .reactions
            .iter()
            .next()
            .unwrap()
            .id;
        app.world_mut().write_message(ReactionsFired {
            source: None,
            container: stock,
            reactions: vec![reaction],
            effects: vec![],
            distinct_reagents: 0,
        });
        app.update();
        assert!(app.world().resource::<PreparedBatches>().0.is_empty());
        app.world_mut().write_message(ReactionsFired {
            source: None,
            container: fresh,
            reactions: vec![reaction],
            effects: vec![],
            distinct_reagents: 2,
        });
        app.update();
        assert_eq!(
            app.world()
                .resource::<PreparedBatches>()
                .0
                .get(&fresh)
                .unwrap()
                .request,
            7
        );
        assert!(!app
            .world()
            .resource::<PreparedBatches>()
            .0
            .contains_key(&stock));
        let contaminant = app.world().resource::<ChemDb>().reagent("water");
        let _ = app
            .world_mut()
            .get_mut::<Container>(fresh)
            .unwrap()
            .solution
            .add(contaminant, Units::whole(5));
        app.update();
        assert!(app.world().resource::<PreparedBatches>().0.is_empty());
    }
    #[test]
    fn proportional_packaging_keeps_provenance_but_mixing_stock_does_not() {
        let db = super::super::tests::db();
        let mut solution = super::super::tests::chemical(&db, "kelotane").solution;
        let p = PreparedForOrder::new(1, &solution);
        let portion = solution.split(Units::whole(5));
        assert!(p.matches(&solution) && p.matches(&portion));
        let mut batches = PreparedBatches::default();
        let a = Entity::from_bits(1);
        let b = Entity::from_bits(2);
        batches.transfer(a, b, &solution, &portion, Some(p), None, false);
        assert!(batches.get(a, &solution).is_some());
        assert!(batches.get(b, &portion).is_none());
    }
}
