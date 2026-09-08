//! Shared, event-driven presentation for real chemistry actions and threats.
//! Cues are emitted only after authority validation. All motion targets visual
//! children; hit testing, navigation, damage and replication roots are unchanged.
use crate::{
    audio::{Sfx, WorldSfx},
    character_lab::CharacterAnimation,
    containers::{Container, ContainerVisual},
    AppState,
};
use bevy::{ecs::entity::MapEntities, prelude::*};
use bevy_replicon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PresentActions;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ActionKind {
    Apply,
    Charge,
    CultAttack,
    CultNotice,
    Pour,
    Stagger,
}
impl ActionKind {
    fn animation(self) -> CharacterAnimation {
        match self {
            Self::Apply => CharacterAnimation::Apply,
            Self::Charge => CharacterAnimation::Charge,
            Self::CultAttack => CharacterAnimation::CultAttack,
            Self::CultNotice => CharacterAnimation::CultNotice,
            Self::Pour => CharacterAnimation::Pour,
            Self::Stagger => CharacterAnimation::Stagger,
        }
    }
    fn duration(self) -> f32 {
        match self {
            Self::Apply => 0.9,
            Self::Charge => 1.0,
            Self::CultAttack => 0.8,
            Self::CultNotice => 1.0,
            Self::Pour => 1.05,
            Self::Stagger => 1.2,
        }
    }
}
#[derive(Message, Clone, Debug, Serialize, Deserialize, MapEntities)]
pub(crate) struct ActionCue {
    #[entities]
    pub actor: Entity,
    #[entities]
    pub item: Option<Entity>,
    pub kind: ActionKind,
    pub target: Vec3,
}
#[derive(Message, Clone, Debug, Serialize, Deserialize)]
pub(crate) struct BlastCue {
    pub position: Vec3,
    pub power: f32,
}
#[derive(Component)]
pub(crate) struct Performance {
    kind: ActionKind,
    remaining: f32,
}
impl Performance {
    pub(crate) fn animation(&self, blood: &chem_sim::Bloodstream) -> Option<CharacterAnimation> {
        (!blood.incapacitated() && self.remaining > 0.0).then(|| self.kind.animation())
    }
}
#[derive(Component)]
struct ItemGesture {
    kind: ActionKind,
    elapsed: f32,
}
#[derive(Component)]
struct EffectPart {
    elapsed: f32,
    life: f32,
    start: f32,
    end: f32,
    drift: Vec3,
    alpha: f32,
}
#[derive(Resource)]
struct EffectAssets {
    sphere: Handle<Mesh>,
    stream: Handle<Mesh>,
}
#[derive(Component)]
struct ChargeLamp(Entity);

pub struct StagecraftPlugin;
impl Plugin for StagecraftPlugin {
    fn build(&self, app: &mut App) {
        app.add_mapped_server_message::<ActionCue>(Channel::Ordered)
            .add_server_message::<BlastCue>(Channel::Ordered)
            .add_systems(Startup, load_assets)
            .add_systems(
                Update,
                (receive_actions, notice_stagger, expire_performances)
                    .chain()
                    .in_set(PresentActions)
                    .run_if(in_state(AppState::Playing)),
            )
            .add_systems(
                Update,
                (
                    receive_blasts,
                    machine_cues,
                    tag_hand_anchors,
                    dress_charge_lamps,
                    animate_charge_lamps,
                    animate_items,
                    animate_effects,
                    animate_rituals,
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}
/// Deferred writes keep action handlers below Bevy's system parameter limit,
/// and keep small headless gameplay tests independent of presentation plugins.
pub(crate) fn action(commands: &mut Commands, cue: ActionCue) {
    commands.queue(move |world: &mut World| {
        if let Some(mut local) = world.get_resource_mut::<Messages<ActionCue>>() {
            local.write(cue.clone());
        }
        if let Some(mut network) = world.get_resource_mut::<Messages<ToClients<ActionCue>>>() {
            network.write(ToClients {
                targets: SendTargets::CLIENTS_ONLY,
                message: cue,
            });
        }
    });
}
pub(crate) fn blast(commands: &mut Commands, position: Vec3, power: f32) {
    commands.queue(move |world: &mut World| {
        let cue = BlastCue { position, power };
        if let Some(mut local) = world.get_resource_mut::<Messages<BlastCue>>() {
            local.write(cue.clone());
        }
        if let Some(mut network) = world.get_resource_mut::<Messages<ToClients<BlastCue>>>() {
            network.write(ToClients {
                targets: SendTargets::CLIENTS_ONLY,
                message: cue,
            });
        }
    });
}
fn load_assets(mut commands: Commands, mut meshes: ResMut<Assets<Mesh>>) {
    commands.insert_resource(EffectAssets {
        sphere: meshes.add(Sphere::new(1.0).mesh().ico(1).unwrap()),
        stream: meshes.add(Cylinder::new(1.0, 1.0)),
    });
}
fn receive_actions(
    mut commands: Commands,
    mut cues: MessageReader<ActionCue>,
    visuals: Query<(Entity, &ContainerVisual)>,
) {
    for cue in cues.read() {
        if !cue.target.is_finite() {
            continue;
        }
        if let Ok(mut actor) = commands.get_entity(cue.actor) {
            actor.insert(Performance {
                kind: cue.kind,
                remaining: cue.kind.duration(),
            });
        }
        if let Some(item) = cue.item {
            for (entity, visual) in &visuals {
                if visual.0 == item {
                    commands.entity(entity).insert(ItemGesture {
                        kind: cue.kind,
                        elapsed: 0.0,
                    });
                }
            }
        }
    }
}
fn expire_performances(
    mut commands: Commands,
    time: Res<Time>,
    mut performances: Query<(Entity, &mut Performance)>,
) {
    for (e, mut p) in &mut performances {
        p.remaining -= time.delta_secs();
        if p.remaining <= 0.0 {
            commands.entity(e).remove::<Performance>();
        }
    }
}
fn animate_items(
    mut commands: Commands,
    time: Res<Time>,
    mut visuals: Query<(
        Entity,
        &ContainerVisual,
        &mut Transform,
        Option<&mut ItemGesture>,
    )>,
    slots: Query<&crate::containers::InSlot>,
    mixing: Query<(), With<crate::machines::AgitationRun>>,
    held: Query<(&crate::containers::HeldBy, &GlobalTransform)>,
    hands: Query<(&HandAnchor, &GlobalTransform)>,
    local: Query<Entity, With<crate::player::LocalPlayer>>,
    capture: Option<Res<crate::capture::CaptureState>>,
) {
    for (e, visual, mut t, gesture) in &mut visuals {
        *t = Transform::default();
        let mut anchored = false;
        if let Ok((holder, root)) = held.get(visual.0) {
            let first_person = local.single().ok() == Some(holder.0)
                && !capture.as_ref().is_some_and(|s| s.free_camera);
            if !first_person {
                if let Some((_, hand)) = hands.iter().find(|(anchor, _)| anchor.0 == holder.0) {
                    let point = hand.transform_point(Vec3::new(0.0, 0.035, 0.0));
                    t.translation = root.affine().inverse().transform_point3(point);
                    anchored = true;
                }
            }
        }
        if let Some(mut g) = gesture {
            g.elapsed += time.delta_secs();
            let phase = (g.elapsed / g.kind.duration()).clamp(0.0, 1.0);
            let weight = (phase * std::f32::consts::PI).sin();
            match g.kind {
                ActionKind::Apply => {
                    if !anchored {
                        t.translation += Vec3::new(-0.10, 0.045, -0.16) * weight;
                    }
                    t.rotation = Quat::from_rotation_x(1.57 * weight);
                }
                ActionKind::Pour => {
                    if !anchored {
                        t.translation += Vec3::new(-0.07, 0.04, -0.08) * weight;
                    }
                    t.rotation = Quat::from_rotation_z(0.9 * weight);
                }
                ActionKind::Charge => {
                    t.translation.y = -0.045 * weight;
                    t.rotation = Quat::from_rotation_x(-0.25 * weight);
                }
                _ => {}
            }
            if phase >= 1.0 {
                commands.entity(e).remove::<ItemGesture>();
            }
        } else if slots
            .get(visual.0)
            .is_ok_and(|slot| mixing.contains(slot.0))
        {
            t.rotation = Quat::from_rotation_z((time.elapsed_secs() * 17.0).sin() * 0.035);
        }
    }
}
fn part(
    commands: &mut Commands,
    assets: &EffectAssets,
    materials: &mut Assets<StandardMaterial>,
    position: Vec3,
    color: Color,
    life: f32,
    start: f32,
    end: f32,
    drift: Vec3,
    alpha: f32,
) {
    commands.spawn((
        Mesh3d(assets.sphere.clone()),
        MeshMaterial3d(materials.add(StandardMaterial {
            base_color: color.with_alpha(alpha),
            alpha_mode: AlphaMode::Blend,
            perceptual_roughness: 1.0,
            unlit: true,
            ..default()
        })),
        Transform::from_translation(position).with_scale(Vec3::splat(start)),
        EffectPart {
            elapsed: 0.0,
            life,
            start,
            end,
            drift,
            alpha,
        },
        crate::until_we_leave_the_lab(),
    ));
}
fn receive_blasts(
    mut commands: Commands,
    mut cues: MessageReader<BlastCue>,
    assets: Res<EffectAssets>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    parts: Query<(), With<EffectPart>>,
) {
    let mut budget = 160usize.saturating_sub(parts.iter().count());
    for cue in cues.read() {
        if !cue.position.is_finite() || !cue.power.is_finite() || cue.power <= 0.0 || budget < 14 {
            continue;
        }
        budget -= 14;
        let radius = chem_sim::body::blast_radius(cue.power).clamp(0.15, 8.0);
        part(
            &mut commands,
            &assets,
            &mut materials,
            cue.position,
            Color::srgb(1.0, 0.78, 0.36),
            0.20,
            0.05,
            radius * 0.45,
            Vec3::ZERO,
            0.9,
        );
        for i in 0..12 {
            let angle = i as f32 * std::f32::consts::TAU / 12.0;
            let direction = Vec3::new(angle.cos(), 0.25 + (i % 3) as f32 * 0.10, angle.sin());
            part(
                &mut commands,
                &assets,
                &mut materials,
                cue.position,
                Color::srgb(0.25, 0.28, 0.30),
                1.8 + (i % 3) as f32 * 0.25,
                0.05,
                radius * 0.20,
                direction * radius * 0.32,
                0.42,
            );
        }
    }
}
fn machine_cues(
    mut commands: Commands,
    mut cues: MessageReader<WorldSfx>,
    assets: Res<EffectAssets>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    machines: Query<(&Transform, &crate::machines::ContainerSlot), With<crate::machines::Machine>>,
    parts: Query<(), With<EffectPart>>,
) {
    if parts.iter().count() > 150 {
        cues.clear();
        return;
    }
    for cue in cues.read() {
        if !matches!(
            cue.sound,
            Sfx::DispensePour | Sfx::BufferTransfer | Sfx::ReactionOccurred | Sfx::PackagePop
        ) {
            continue;
        }
        let target = machines.iter().min_by(|(a, _), (b, _)| {
            a.translation
                .distance_squared(cue.position)
                .total_cmp(&b.translation.distance_squared(cue.position))
        });
        let Some((machine, slot)) = target else {
            continue;
        };
        if machine.translation.distance(cue.position) > 2.0 {
            continue;
        }
        let position = machine.translation + slot.offset;
        if cue.sound == Sfx::DispensePour {
            commands.spawn((
                Mesh3d(assets.stream.clone()),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: Color::srgba(0.48, 0.80, 0.82, 0.8),
                    alpha_mode: AlphaMode::Blend,
                    ..default()
                })),
                Transform::from_translation(position + Vec3::Y * 0.09)
                    .with_scale(Vec3::new(0.003, 0.10, 0.003)),
                Stream { remaining: 0.4 },
                crate::until_we_leave_the_lab(),
            ));
        } else {
            for i in 0..3 {
                part(
                    &mut commands,
                    &assets,
                    &mut materials,
                    position + Vec3::new((i as f32 - 1.0) * 0.018, 0.06, 0.0),
                    Color::srgb(0.53, 0.77, 0.76),
                    0.45,
                    0.008,
                    0.012,
                    Vec3::Y * 0.05,
                    0.45,
                );
            }
        }
    }
}
#[derive(Component)]
struct Stream {
    remaining: f32,
}
fn animate_effects(
    mut commands: Commands,
    time: Res<Time>,
    mut parts: Query<(
        Entity,
        &mut EffectPart,
        &mut Transform,
        &MeshMaterial3d<StandardMaterial>,
    )>,
    mut streams: Query<(Entity, &mut Stream)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (e, mut p, mut t, handle) in &mut parts {
        p.elapsed += time.delta_secs();
        let age = (p.elapsed / p.life).clamp(0.0, 1.0);
        t.translation += p.drift * time.delta_secs();
        t.scale = Vec3::splat(p.start + (p.end - p.start) * (1.0 - (1.0 - age).powi(3)));
        if let Some(mut m) = materials.get_mut(&handle.0) {
            m.base_color = m.base_color.with_alpha(p.alpha * (1.0 - age));
        }
        if age >= 1.0 {
            commands.entity(e).despawn();
        }
    }
    for (e, mut s) in &mut streams {
        s.remaining -= time.delta_secs();
        if s.remaining <= 0.0 {
            commands.entity(e).despawn();
        }
    }
}
fn dress_charge_lamps(
    mut commands: Commands,
    assets: Res<EffectAssets>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    visuals: Query<(Entity, &ContainerVisual), Added<ContainerVisual>>,
    items: Query<&Container>,
) {
    for (visual, owner) in &visuals {
        let Ok(item) = items.get(owner.0) else {
            continue;
        };
        if item.kind.charge_fuse().is_some() {
            commands.spawn((
                ChargeLamp(owner.0),
                Mesh3d(assets.sphere.clone()),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: Color::srgb(0.24, 0.33, 0.30),
                    unlit: true,
                    ..default()
                })),
                Transform::from_xyz(0.0, 0.011, 0.046).with_scale(Vec3::new(0.016, 0.004, 0.002)),
                ChildOf(visual),
            ));
        }
    }
}
fn animate_charge_lamps(
    time: Res<Time>,
    charges: Query<&crate::containers::ArmedCharge>,
    lamps: Query<(&ChargeLamp, &MeshMaterial3d<StandardMaterial>)>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for (lamp, handle) in &lamps {
        if let Some(mut material) = materials.get_mut(&handle.0) {
            material.base_color = if let Ok(armed) = charges.get(lamp.0) {
                let phase = time.elapsed_secs()
                    * if armed.remaining_secs < 2.0 {
                        16.0
                    } else {
                        7.0
                    };
                if phase.sin() > 0.0 {
                    Color::srgb(1.0, 0.40, 0.12)
                } else {
                    Color::srgb(0.30, 0.09, 0.04)
                }
            } else {
                Color::srgb(0.24, 0.33, 0.30)
            };
        }
    }
}
fn animate_rituals(
    time: Res<Time>,
    roots: Query<(), With<crate::cult::CultVisual>>,
    mut children: Query<(&ChildOf, &mut Transform), Without<crate::cult::CultVisual>>,
) {
    for (parent, mut t) in &mut children {
        if roots.contains(parent.parent()) {
            t.scale = Vec3::splat(1.0 + 0.012 * (time.elapsed_secs() * 2.0).sin());
        }
    }
}

#[derive(Component)]
struct WasUnsteady(bool);
fn notice_stagger(
    mut commands: Commands,
    bodies: Query<
        (Entity, &crate::body::Bloodstream, Option<&WasUnsteady>),
        Changed<crate::body::Bloodstream>,
    >,
) {
    for (entity, blood, previous) in &bodies {
        let unsteady = blood.0.status(chem_sim::StatusKind::Drunk).intensity > 0.0;
        if unsteady && previous.is_some_and(|p| !p.0) && !blood.0.incapacitated() {
            commands.entity(entity).insert(Performance {
                kind: ActionKind::Stagger,
                remaining: 1.2,
            });
        }
        if previous.is_none_or(|p| p.0 != unsteady) {
            commands.entity(entity).insert(WasUnsteady(unsteady));
        }
    }
}

/// Both player and crew GLBs now share the nine-clip base rig. Keeping the
/// semantic mapping here prevents a later added Blender Action shifting gait.
pub(crate) fn character_clips(
    server: &AssetServer,
    path: &'static str,
) -> Vec<Handle<AnimationClip>> {
    [1, 4, 2, 5, 0, 6, 7, 8, 3]
        .into_iter()
        .map(|i| server.load(GltfAssetLabel::Animation(i).from_asset(path)))
        .chain((0..6).map(|i| {
            server.load(GltfAssetLabel::Animation(i).from_asset("3dassets/glb/chem_actions.glb"))
        }))
        .collect()
}

#[cfg(debug_assertions)]
pub(crate) fn clear_transients(world: &mut World) {
    let parts: Vec<_> = world
        .query_filtered::<Entity, Or<(With<EffectPart>, With<Stream>)>>()
        .iter(world)
        .collect();
    for entity in parts {
        world.despawn(entity);
    }
    let actors: Vec<_> = world
        .query_filtered::<Entity, Or<(With<Performance>, With<WasUnsteady>)>>()
        .iter(world)
        .collect();
    for entity in actors {
        world
            .entity_mut(entity)
            .remove::<(Performance, WasUnsteady)>();
    }
    world.resource_mut::<Messages<ActionCue>>().clear();
    world.resource_mut::<Messages<BlastCue>>().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app() -> App {
        let mut app = App::new();
        app.init_resource::<Time>()
            .init_resource::<Assets<Mesh>>()
            .init_resource::<Assets<StandardMaterial>>()
            .add_message::<ActionCue>()
            .add_message::<BlastCue>()
            .add_message::<WorldSfx>()
            .add_systems(Startup, load_assets)
            .add_systems(
                Update,
                (
                    receive_actions,
                    expire_performances,
                    receive_blasts,
                    animate_items,
                    animate_effects,
                )
                    .chain(),
            );
        app
    }
    #[test]
    fn accepted_action_animates_child_without_moving_authoritative_root() {
        let mut app = app();
        let actor = app.world_mut().spawn_empty().id();
        let root_pose = Transform::from_xyz(2.0, 1.0, 3.0);
        let item = app.world_mut().spawn(root_pose).id();
        let visual = app
            .world_mut()
            .spawn((ContainerVisual(item), Transform::default(), ChildOf(item)))
            .id();
        app.world_mut().write_message(ActionCue {
            actor,
            item: Some(item),
            kind: ActionKind::Apply,
            target: Vec3::ZERO,
        });
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(0.2));
        app.update();
        assert_eq!(*app.world().get::<Transform>(item).unwrap(), root_pose);
        assert_ne!(
            *app.world().get::<Transform>(visual).unwrap(),
            Transform::default()
        );
        assert!(app.world().get::<Performance>(actor).is_some());
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs_f32(2.0));
        app.update();
        assert!(app.world().get::<Performance>(actor).is_none());
    }
    #[test]
    fn blast_effects_are_bounded_and_expire() {
        let mut app = app();
        for _ in 0..100 {
            app.world_mut().write_message(BlastCue {
                position: Vec3::ZERO,
                power: 10.0,
            });
        }
        app.update();
        let count = app
            .world_mut()
            .query::<&EffectPart>()
            .iter(app.world())
            .count();
        assert!(count > 0 && count <= 160);
        app.world_mut()
            .resource_mut::<Time>()
            .advance_by(std::time::Duration::from_secs(5));
        app.update();
        assert_eq!(
            app.world_mut()
                .query::<&EffectPart>()
                .iter(app.world())
                .count(),
            0
        );
    }
    #[test]
    fn presentation_relay_preserves_the_validated_actor_and_target() {
        use bevy::ecs::system::RunSystemOnce;
        let mut world = World::new();
        world.init_resource::<Messages<ActionCue>>();
        world.init_resource::<Messages<ToClients<ActionCue>>>();
        let actor = world.spawn_empty().id();
        world
            .run_system_once(move |mut commands: Commands| {
                action(
                    &mut commands,
                    ActionCue {
                        actor,
                        item: None,
                        kind: ActionKind::Pour,
                        target: Vec3::new(1.0, 2.0, 3.0),
                    },
                )
            })
            .unwrap();
        world.flush();
        let local: Vec<_> = world
            .resource_mut::<Messages<ActionCue>>()
            .drain()
            .collect();
        let remote: Vec<_> = world
            .resource_mut::<Messages<ToClients<ActionCue>>>()
            .drain()
            .collect();
        assert_eq!(local.len(), 1);
        assert_eq!(remote.len(), 1);
        assert_eq!(local[0].actor, remote[0].message.actor);
        assert_eq!(local[0].target, remote[0].message.target);
    }
}

#[derive(Component)]
struct HandAnchor(Entity);
fn tag_hand_anchors(
    mut commands: Commands,
    new: Query<(Entity, &Name), Added<Name>>,
    parents: Query<&ChildOf>,
    actors: Query<(), Or<(With<crate::player::Chemist>, With<crate::crew::CrewMember>)>>,
    alive: Query<()>,
    mut pending: Local<Vec<Entity>>,
) {
    for (entity, name) in &new {
        if name.as_str() == "hand.R" {
            pending.push(entity);
        }
    }
    pending.retain(|&joint| {
        if !alive.contains(joint) {
            return false;
        }
        let mut cursor = joint;
        for _ in 0..32 {
            if actors.contains(cursor) {
                commands.entity(joint).insert(HandAnchor(cursor));
                return false;
            }
            let Ok(parent) = parents.get(cursor) else {
                break;
            };
            cursor = parent.parent();
        }
        true
    });
}
