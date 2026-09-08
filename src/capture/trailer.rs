//! Disposable recording fixtures. All controls are local to the authority;
//! guests receive ordinary replicated actors, items, chemistry and transforms.
use crate::{
    body::{Bloodstream, Body},
    containers::{Container, ContainerKind},
    crew::{CrewDef, CrewMember, CrewRoute},
    lab::{CrisisSpots, MapReady},
    machines::{Machine, MachineKind},
    net::is_authority,
    AppState,
};
use bevy::prelude::*;
use chem_sim::Units;

#[derive(Resource)]
pub(crate) struct TrailerSession {
    shot: usize,
    reset: bool,
    running: bool,
}
impl Default for TrailerSession {
    fn default() -> Self {
        Self {
            shot: 0,
            reset: true,
            running: false,
        }
    }
}
#[derive(Component)]
struct FixtureActor;
#[derive(Component)]
struct FixtureProp;
#[derive(Component)]
struct Slate;
const TITLES: [&str; 4] = [
    "Medicine",
    "Advanced chemistry",
    "NPC dose",
    "Cult and charge",
];
pub struct TrailerPlugin;
impl Plugin for TrailerPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (select_shot, reset_shot, start_action)
                .chain()
                .run_if(resource_exists::<TrailerSession>)
                .run_if(resource_exists::<MapReady>)
                .run_if(is_authority)
                .run_if(in_state(AppState::Playing)),
        )
        .add_systems(OnExit(AppState::Playing), |mut commands: Commands| {
            commands.remove_resource::<TrailerSession>();
        });
    }
}
fn select_shot(keys: Res<ButtonInput<KeyCode>>, mut session: ResMut<TrailerSession>) {
    for (i, key) in [KeyCode::F1, KeyCode::F2, KeyCode::F3, KeyCode::F4]
        .into_iter()
        .enumerate()
    {
        if keys.just_pressed(key) {
            session.shot = i;
            session.reset = true;
        }
    }
    if keys.just_pressed(KeyCode::F5) {
        session.reset = true;
    }
}
fn sample(
    db: &crate::chem_data::ChemDb,
    kind: ContainerKind,
    ingredients: &[(&str, i32)],
) -> Container {
    let mut item = Container::new(kind);
    for &(name, units) in ingredients {
        let id = db.reagents.id_of(name).expect("fixture reagent exists");
        let overflow =
            item.solution
                .add_profiled(id, Units::whole(units), 1.0, db.reagents.get(id).ph);
        assert!(overflow.is_zero(), "fixture fits its container");
    }
    item
}
fn reset_shot(world: &mut World) {
    if !world.resource::<TrailerSession>().reset {
        return;
    }
    let machines: Vec<_> = world
        .query::<(Entity, &Machine, &Transform, &crate::machines::Facing)>()
        .iter(world)
        .map(|(e, m, t, f)| (e, m.kind, t.translation, f.0))
        .collect();
    let Some(&(_, _, origin, front)) = machines
        .iter()
        .find(|(_, k, _, _)| *k == MachineKind::MixingChamber)
    else {
        return;
    };
    if !world.contains_resource::<crate::chem_data::ChemDb>() {
        return;
    }
    let shot = world.resource::<TrailerSession>().shot;
    // This session has no SaveSlot and cannot load or overwrite a career.
    assert!(!world.contains_resource::<crate::saves::SaveSlot>());
    crate::stagecraft::clear_transients(world);
    let corrosion: Vec<_> = world
        .query_filtered::<Entity, With<crate::door::Corroded>>()
        .iter(world)
        .collect();
    for entity in corrosion {
        world.entity_mut(entity).remove::<crate::door::Corroded>();
    }
    let surfaces: Vec<_> = world
        .query::<(&Transform, &crate::lab::Solid)>()
        .iter(world)
        .map(|(t, s)| (t.translation, s.half_extents))
        .collect();
    let bench = surfaces
        .iter()
        .filter(|(p, h)| (0.65..1.2).contains(&(p.y + h.y)) && h.x >= 0.50 && h.z >= 0.22)
        .min_by(|(a, _), (b, _)| {
            a.distance_squared(origin)
                .total_cmp(&b.distance_squared(origin))
        })
        .map(|(p, h)| Vec3::new(p.x, p.y + h.y, p.z))
        .unwrap_or_else(|| crate::lab::resting_place(origin + front, origin + front, &surfaces));
    let remove: Vec<_> = world
        .query_filtered::<Entity, Or<(
            With<Container>,
            With<CrewMember>,
            With<crate::hazards::SmokeCloud>,
            With<crate::hazards::ActiveHazard>,
            With<crate::chem_world::ChemicalPuddle>,
            With<FixtureProp>,
            With<Slate>,
        )>>()
        .iter(world)
        .collect();
    for entity in remove {
        world.despawn(entity);
    }
    for (e, _, _, _) in &machines {
        world
            .entity_mut(*e)
            .remove::<crate::machines::AgitationRun>();
        if let Some(mut buffer) = world.get_mut::<crate::machines::Buffer>(*e) {
            buffer.0.clear();
        }
        if let Some(mut thermostat) = world.get_mut::<crate::machines::Thermostat>(*e) {
            *thermostat = default();
        }
        if let Some(mut machine) = world.get_mut::<Machine>(*e) {
            machine.in_use_by = None;
            machine.disabled_for = 0.0;
        }
    }
    let guard_at = world.resource::<CrisisSpots>().get("cult.base_guard_1");
    let stage = if shot == 3 {
        guard_at.map(|t| t.translation).unwrap_or(origin)
    } else {
        origin
    };
    let actor_at = world
        .resource::<crate::lab::WalkableAreas>()
        .contain_on_surface(
            stage
                + if shot == 3 {
                    Vec3::Z * 3.5
                } else {
                    front * 2.0
                },
            0.35,
            1.0,
        );
    let player_at = world
        .resource::<crate::lab::WalkableAreas>()
        .contain_on_surface(
            actor_at
                + if shot == 3 {
                    Vec3::Z * 2.0
                } else {
                    front * 1.6
                },
            0.35,
            1.0,
        );
    let players: Vec<_> = world
        .query_filtered::<Entity, With<crate::player::Chemist>>()
        .iter(world)
        .collect();
    for (index, entity) in players.iter().enumerate() {
        let position = world
            .resource::<crate::lab::WalkableAreas>()
            .contain_on_surface(player_at + Vec3::X * index as f32 * 0.8, 0.35, 1.0);
        let mut player = world.entity_mut(*entity);
        player.insert((
            Body::default(),
            Bloodstream::default(),
            crate::interaction::InteractionMode::Roaming,
        ));
        player.remove::<crate::player::Predicted>();
        if let Some(mut t) = player.get_mut::<Transform>() {
            t.translation.x = position.x;
            t.translation.z = position.z;
        }
        if let Some(mut look) = player.get_mut::<crate::player::Look>() {
            let d = actor_at - position;
            look.yaw = (-d.x).atan2(-d.z);
            look.pitch = -0.1;
        }
    }
    world.resource_mut::<super::CaptureState>().free_camera = false;
    let ritual_at = world
        .resource::<CrisisSpots>()
        .get("cult.bleeding_offering_bowl");
    world.resource_scope(|world, db: Mut<crate::chem_data::ChemDb>| {
        if let Some(mut knowledge) = world.get_resource_mut::<crate::knowledge::Knowledge>() { knowledge.unlock_all(&db); }
        let mut commands = world.commands();

        let mut add = |kind: ContainerKind, parts: &[(&str, i32)], label: &str, index: usize| {
            let p = if shot == 3 { crate::lab::resting_place(player_at + Vec3::new(index as f32 * 0.25, 0.0, -0.4),player_at,&surfaces) + Vec3::Y * 0.05 }
                else { bench + Vec3::Y * (kind.dimensions().1 * 0.5) + Vec3::X * (index as f32 - 1.5) * 0.25 };
            commands.spawn((sample(&db, kind, parts), crate::labels::Label(label.into()),
                Transform::from_translation(p), bevy_replicon::prelude::Replicated, crate::until_we_leave_the_lab()));
        };
        match shot {
            0 => { add(ContainerKind::Beaker, &[], "Clean beaker", 0); add(ContainerKind::Bottle, &[], "Medicine bottle", 1); }
            1 => {
                add(ContainerKind::LargeBeaker, &[("acetone",20)], "Intermediate A", 0);
                add(ContainerKind::LargeBeaker, &[("hydrogen_peroxide",10),("oxygen",10)], "Intermediate B", 1);
                // Cooled, stabilized synthesis input. Heating it is a real player action.
                let mut input = sample(&db, ContainerKind::LargeBeaker, &[("phenol",40),("acetone_oxide",20),("nitric_acid",20),("stabilizing_agent",1)]);
                input.solution.shift_ph(2.0 - input.solution.ph());
                commands.spawn((input, crate::labels::Label("Stabilized RDX input - heat to 410 K".into()),
                    Transform::from_translation(bench + Vec3::new(0.125,0.085,0.0)), bevy_replicon::prelude::Replicated, crate::until_we_leave_the_lab()));
            }
            2 => { add(ContainerKind::Syringe, &[("hooch",5)], "5u Hooch", 0); }
            _ => { add(ContainerKind::ChemicalCharge5, &[("rdx",20)], "RDX - 5 second fuse", 0); }
        }
        if shot == 3 {
            if let Some(at) = ritual_at {
                commands.spawn((FixtureProp, crate::cult::CultVisual(crate::cult::CultVisualId::BleedingOfferingBowl),
                    at, Visibility::Inherited, bevy_replicon::prelude::Replicated, crate::until_we_leave_the_lab()));
            }
        }
        if shot >= 2 {
            let def = CrewDef { name: if shot == 3 { "A Watching Acolyte" } else { "Crew Volunteer" }.into(),
                role: if shot == 3 { "Cult" } else { "Service" }.into(), color: [0.5,0.5,0.5] };
            let npc = crate::crew::spawn_crew_member(&mut commands, &def, 0.0);
            commands.entity(npc).remove::<(CrewRoute, crate::crew::NeedsDepartmentPlacement)>()
                .insert((FixtureActor, Transform::from_translation(if shot == 3 { stage } else { actor_at }), crate::crew::Ambient::new(0.0)));
            if shot == 3 {
                commands.entity(npc).insert(crate::cult::Cultist { wards_incident: None, tier: crate::cult::CultistTier::Watching });
            }
        }
        commands.spawn((Slate, Text::new(format!("TRAILER: {}\nF1-F4 scene | F5 reset | Numpad0 action | F8 clean HUD\nF9 camera | F10 title | Shift+Numpad1-4 save view", TITLES[shot])),
            TextFont::from_font_size(17.0), TextColor(Color::WHITE),
            Node { position_type: PositionType::Absolute, left: px(20), top: px(20), ..default() },
            GlobalZIndex(1500), Visibility::Inherited, crate::until_we_leave_the_lab()));
    });
    world.flush();
    let mut session = world.resource_mut::<TrailerSession>();
    session.reset = false;
    session.running = false;
    info!(
        "trailer setup: {}. Numpad0 starts actor action; F5 resets.",
        TITLES[shot]
    );
}
fn start_action(
    keys: Res<ButtonInput<KeyCode>>,
    mut session: ResMut<TrailerSession>,
    mut commands: Commands,
    actors: Query<(Entity, &Transform), With<FixtureActor>>,
    areas: Res<crate::lab::WalkableAreas>,
) {
    if session.running || !keys.just_pressed(KeyCode::Numpad0) {
        return;
    }
    session.running = true;
    for (entity, at) in &actors {
        if session.shot == 3 {
            commands
                .entity(entity)
                .insert(crate::showdown::Pursuit::new(1.6, 1.5, 5).with_notice());
            crate::stagecraft::action(
                &mut commands,
                crate::stagecraft::ActionCue {
                    actor: entity,
                    item: None,
                    kind: crate::stagecraft::ActionKind::CultNotice,
                    target: at.translation,
                },
            );
        } else {
            let destination = areas.contain_on_surface(at.translation + Vec3::X * 4.0, 0.35, 1.0);
            crate::crew::send_on_errand(
                &mut commands,
                entity,
                crate::crew::ErrandGoal::Point(destination),
            );
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trailer_and_live_npc_plugins_initialize_one_unambiguous_update_schedule() {
        let mut app = App::new();
        app.add_plugins((
            crate::crew::CrewPlugin,
            crate::showdown::ShowdownPlugin,
            crate::npc_motion::NpcMotionPlugin,
            TrailerPlugin,
        ));
        app.world_mut().schedule_scope(Update, |world, schedule| {
            schedule.initialize(world).unwrap();
        });
    }

    #[test]
    fn resetting_disposable_fixtures_replaces_items_and_does_not_create_a_save_slot() {
        let mut world = World::new();
        world.init_resource::<TrailerSession>();
        world.init_resource::<super::super::CaptureState>();
        world.init_resource::<CrisisSpots>();
        world.init_resource::<crate::lab::WalkableAreas>();
        world.init_resource::<Messages<crate::stagecraft::ActionCue>>();
        world.init_resource::<Messages<crate::stagecraft::BlastCue>>();
        world.insert_resource(crate::chem_data::ChemDb(
            chem_sim::ChemData::from_ron(
                include_str!("../../assets/data/chem.reagents.ron"),
                include_str!("../../assets/data/chem.reactions.ron"),
            )
            .unwrap(),
        ));
        world.spawn((
            Machine::new(MachineKind::MixingChamber),
            Transform::default(),
            crate::machines::Facing(Vec3::Z),
        ));
        reset_shot(&mut world);
        let initial: Vec<_> = world
            .query_filtered::<Entity, With<Container>>()
            .iter(&world)
            .collect();
        assert_eq!(initial.len(), 2);
        world.resource_mut::<TrailerSession>().reset = true;
        reset_shot(&mut world);
        assert!(initial.iter().all(|e| !world.entities().contains(*e)));
        assert_eq!(world.query::<&Container>().iter(&world).count(), 2);
        world.resource_mut::<TrailerSession>().shot = 2;
        world.resource_mut::<TrailerSession>().reset = true;
        reset_shot(&mut world);
        assert_eq!(world.query::<&FixtureActor>().iter(&world).count(), 1);
        assert_eq!(world.query::<&Container>().iter(&world).count(), 1);
        assert!(!world.contains_resource::<crate::saves::SaveSlot>());
        assert_eq!(world.query::<&Slate>().iter(&world).count(), 1);
    }
    #[test]
    fn prepared_rdx_really_synthesizes_and_detonates_at_the_charge_temperature() {
        let db = crate::chem_data::ChemDb(
            chem_sim::ChemData::from_ron(
                include_str!("../../assets/data/chem.reagents.ron"),
                include_str!("../../assets/data/chem.reactions.ron"),
            )
            .unwrap(),
        );
        let mut input = sample(
            &db,
            ContainerKind::LargeBeaker,
            &[
                ("phenol", 40),
                ("acetone_oxide", 20),
                ("nitric_acid", 20),
                ("stabilizing_agent", 1),
            ],
        );
        input.solution.shift_ph(2.0 - input.solution.ph());
        let (_, cold) = input.mutate(&db, |_| {});
        assert!(cold.effects.is_empty());
        let (_, warm) = input.mutate(&db, |s| s.temperature = chem_sim::Kelvin(410.0));
        assert!(warm.effects.is_empty());
        assert_eq!(
            input.solution.volume_of(db.reagents.id_of("rdx").unwrap()),
            Units::whole(20)
        );
        let (_, hot) = input.mutate(&db, |s| s.temperature = chem_sim::Kelvin(600.0));
        assert!(!hot.effects.is_empty());
    }
}
