//! Local, non-destructive inspection. Descriptions deliberately distinguish claims
//! from measurements; this module never changes a solution or teaches a recipe.
use crate::{
    containers::{Container, ContainerKind, HeldBy},
    interaction::{Interactable, InteractionMode},
    labels::Label,
    player::LocalPlayer,
    AppState,
};
use bevy::{
    camera::{
        primitives::{Aabb, MeshAabb},
        visibility::RenderLayers,
        RenderTarget,
    },
    input::mouse::AccumulatedMouseMotion,
    prelude::*,
    render::render_resource::TextureFormat,
};

pub struct InspectionPlugin;
impl Plugin for InspectionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<InspectionState>()
            .add_systems(OnExit(AppState::Playing), reset)
            .add_systems(
                Update,
                (
                    input.before(crate::interaction::panel_input),
                    draw.after(crate::interaction::panel_input),
                    rotate,
                )
                    .chain()
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

/// Safe for a hand, shelf or pickup prompt. Instrument readouts do not use this.
pub fn container_description(container: &Container) -> String {
    match container.kind {
        ContainerKind::PhPaper => "Unused".into(),
        ContainerKind::PhPaperStrongAcid
        | ContainerKind::PhPaperAcid
        | ContainerKind::PhPaperNeutral
        | ContainerKind::PhPaperBase
        | ContainerKind::PhPaperStrongBase => container.kind.label().into(),
        _ if container.solution.is_empty() => "Empty".into(),
        _ => "Unknown contents".into(),
    }
}

pub(crate) fn input(
    keys: Res<ButtonInput<KeyCode>>,
    settings: Res<crate::settings::Settings>,
    paused: Option<Res<crate::settings::Paused>>,
    mut players: Query<(Entity, &mut InteractionMode), With<LocalPlayer>>,
    held: Query<(Entity, &HeldBy)>,
    machines: Query<&crate::machines::Machine>,
) {
    if paused.is_some_and(|p| p.0) {
        return;
    }
    for (player, mut mode) in &mut players {
        if let InteractionMode::Inspecting { item, machine } = *mode {
            if keys.just_pressed(settings.bindings.inspect)
                || held.get(item).map(|(_, h)| h.0) != Ok(player)
            {
                *mode = machine
                    .filter(|m| machines.get(*m).is_ok_and(|m| m.available_to(player)))
                    .map_or(InteractionMode::Roaming, InteractionMode::UsingMachine);
            }
            continue;
        }
        if !keys.just_pressed(settings.bindings.inspect) {
            continue;
        }
        if !matches!(
            *mode,
            InteractionMode::Roaming | InteractionMode::UsingMachine(_)
        ) {
            continue;
        }
        if let Some((item, _)) = held.iter().find(|(_, h)| h.0 == player) {
            *mode = InteractionMode::Inspecting {
                item,
                machine: mode.claimed_machine(),
            };
        }
    }
}

#[derive(Component)]
struct InspectionRoot;
#[derive(Component)]
struct InspectionModel;
#[derive(Component)]
struct InspectionViewport;
#[derive(Component)]
struct InspectionText;
#[derive(Resource, Default)]
struct InspectionState {
    item: Option<Entity>,
}

fn reset(mut state: ResMut<InspectionState>) {
    *state = InspectionState::default();
}

/// Transform all corners, not just a mesh origin: tall tools and offset parts
/// must fit when rotated, and their apparent size must not depend on world scale.
fn transformed_bounds(bounds: Aabb, transform: &Transform) -> (Vec3, Vec3) {
    let mut low = Vec3::splat(f32::INFINITY);
    let mut high = Vec3::splat(f32::NEG_INFINITY);
    for x in [-1.0, 1.0] {
        for y in [-1.0, 1.0] {
            for z in [-1.0, 1.0] {
                let point = transform.transform_point(
                    Vec3::from(bounds.center)
                        + Vec3::new(x, y, z) * Vec3::from(bounds.half_extents),
                );
                low = low.min(point);
                high = high.max(point);
            }
        }
    }
    (low, high)
}

fn chemical_or_document_description(
    label: Option<&Label>,
    container: Option<&Container>,
    report: Option<&crate::analysis_reports::AnalysisReport>,
) -> Option<String> {
    if let Some(label) = label {
        return Some(format!("LABEL\n\n{}", label.0));
    }
    if let Some(report) = report {
        return Some(report.read());
    }
    container.map(|container| {
        format!(
            "{}\n\n{}",
            container.kind.label(),
            container_description(container)
        )
    })
}

type Items<'w, 's> = Query<
    'w,
    's,
    (
        Option<&'static Label>,
        Option<&'static Container>,
        Option<&'static Interactable>,
        Option<&'static crate::analysis_reports::AnalysisReport>,
        Option<&'static crate::produce::Produce>,
        Option<&'static crate::rogue_security::Deterrent>,
        Option<&'static crate::machines::Overclock>,
        Option<&'static crate::social::SocialParcel>,
    ),
>;

#[allow(clippy::too_many_arguments)]
fn draw(
    mut commands: Commands,
    players: Query<&InteractionMode, With<LocalPlayer>>,
    items: Items,
    catalog: Option<Res<crate::produce::ProduceCatalog>>,
    db: Option<Res<crate::chem_data::ChemDb>>,
    mut images: ResMut<Assets<Image>>,
    roots: Query<Entity, With<InspectionRoot>>,
    meshes: Res<Assets<Mesh>>,
    cameras: Query<Entity, With<crate::player::PlayerCamera>>,
    children: Query<&Children>,
    globals: Query<&GlobalTransform>,
    surfaces: Query<(
        &Mesh3d,
        &MeshMaterial3d<StandardMaterial>,
        &GlobalTransform,
        Option<&Visibility>,
    )>,
    mut descriptions: Query<&mut Text, With<InspectionText>>,
    mut state: ResMut<InspectionState>,
) {
    let inspected = players.iter().find_map(|m| {
        if let InteractionMode::Inspecting { item, .. } = m {
            Some(*item)
        } else {
            None
        }
    });
    let description = inspected
        .and_then(|e| items.get(e).ok())
        .map(
            |(label, container, interactable, report, produce, deterrent, overclock, parcel)| {
                if let Some(description) =
                    chemical_or_document_description(label, container, report)
                {
                    return description;
                }
                if let (Some(produce), Some(catalog), Some(db)) =
                    (produce, catalog.as_deref(), db.as_deref())
                {
                    let kind = catalog.get(produce.0);
                    return format!(
                        "{}\n\nGrinder yields\n{}",
                        kind.name,
                        kind.yields
                            .iter()
                            .map(|(r, q)| format!("{}  {q}", db.reagents.get(*r).name))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                }
                if let Some(tool) = deterrent {
                    return format!(
                        "Deterrent\n\nRepels an aggressor.\n{} charges",
                        tool.charges
                    );
                }
                if let Some(tool) = overclock {
                    return format!(
                        "Overclock cartridge\n\nAccelerates compatible equipment.\n{} charges",
                        tool.charges
                    );
                }
                if let Some(parcel) = parcel {
                    return format!(
                        "Parcel for {}\n\n{}\nSeal: {:?}",
                        parcel.recipient,
                        if parcel.priority {
                            "Priority delivery"
                        } else {
                            "Standard delivery"
                        },
                        parcel.seal
                    );
                }
                interactable.map_or("Item".into(), |i| i.label.clone())
            },
        )
        .unwrap_or_default();
    if state.item == inspected && (inspected.is_none() || !roots.is_empty()) {
        for mut text in &mut descriptions {
            if text.0 != description {
                text.0 = description.clone();
            }
        }
        return;
    }
    for root in &roots {
        commands.entity(root).despawn();
    }
    state.item = None;
    let Some(item) = inspected else {
        return;
    };
    let Ok(ui_camera) = cameras.single() else {
        return;
    };
    let layer = RenderLayers::layer(17);
    let origin = globals
        .get(item)
        .map_or(Mat4::IDENTITY, |g| g.to_matrix().inverse());
    let mut parts = Vec::new();
    let mut low = Vec3::splat(f32::INFINITY);
    let mut high = Vec3::splat(f32::NEG_INFINITY);
    for entity in std::iter::once(item).chain(children.iter_descendants(item)) {
        if let Ok((mesh, material, global, visibility)) = surfaces.get(entity) {
            if visibility == Some(&Visibility::Hidden) {
                continue;
            }
            let transform = Transform::from_matrix(origin * global.to_matrix());
            let Some(bounds) = meshes.get(&mesh.0).and_then(MeshAabb::compute_aabb) else {
                continue;
            };
            let (part_low, part_high) = transformed_bounds(bounds, &transform);
            low = low.min(part_low);
            high = high.max(part_high);
            parts.push((mesh.clone(), material.clone(), transform));
        }
    }
    // A newly received item can precede its visual children by a frame. Retry
    // until geometry arrives rather than permanently presenting an empty box.
    if parts.is_empty() {
        return;
    }
    state.item = inspected;
    let center = (low + high) * 0.5;
    let radius = ((high - low).length() * 0.5).max(0.01);
    let texture = images.add(Image::new_target_texture(
        768,
        768,
        TextureFormat::Rgba8Unorm,
        Some(TextureFormat::Rgba8UnormSrgb),
    ));
    let model = commands
        .spawn((
            Transform::from_rotation(Quat::from_rotation_x(0.15)),
            Visibility::default(),
            InspectionRoot,
            InspectionModel,
            crate::until_we_leave_the_lab(),
        ))
        .id();
    for (mesh, material, mut transform) in parts {
        transform.translation -= center;
        commands.spawn((mesh, material, transform, layer.clone(), ChildOf(model)));
    }
    commands.spawn((
        Camera3d::default(),
        Camera {
            order: -1,
            clear_color: Color::srgb(0.065, 0.082, 0.1).into(),
            ..default()
        },
        RenderTarget::Image(texture.clone().into()),
        Projection::Perspective(PerspectiveProjection {
            near: (radius * 0.05).max(0.001),
            far: (radius * 12.0).max(10.0),
            ..default()
        }),
        Transform::from_xyz(0.0, radius * 0.6, radius * 3.4).looking_at(Vec3::ZERO, Vec3::Y),
        layer.clone(),
        InspectionRoot,
        crate::until_we_leave_the_lab(),
    ));
    commands.spawn((
        PointLight {
            intensity: 18000.0,
            shadow_maps_enabled: false,
            ..default()
        },
        Transform::from_xyz(-1.0, 2.0, 2.0),
        layer,
        InspectionRoot,
        crate::until_we_leave_the_lab(),
    ));
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(Color::srgba(0.01, 0.02, 0.025, 0.9)),
            UiTargetCamera(ui_camera),
            GlobalZIndex(60),
            InspectionRoot,
            crate::until_we_leave_the_lab(),
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    width: percent(88),
                    max_width: px(1150),
                    height: percent(80),
                    padding: UiRect::all(px(24)),
                    column_gap: px(24),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.08, 0.105, 0.12)),
            ))
            .with_children(|panel| {
                panel
                    .spawn(Node {
                        width: percent(50),
                        height: percent(100),
                        flex_direction: FlexDirection::Column,
                        row_gap: px(12),
                        ..default()
                    })
                    .with_children(|left| {
                        left.spawn((
                            ImageNode::new(texture),
                            Node {
                                width: percent(100),
                                max_height: percent(90),
                                aspect_ratio: Some(1.0),
                                flex_shrink: 1.0,
                                ..default()
                            },
                            Interaction::None,
                            InspectionViewport,
                        ));
                        left.spawn((
                            Text::new("Drag to rotate   |   Inspect / Esc to close"),
                            TextFont::from_font_size(14.0),
                        ));
                    });
                panel
                    .spawn((
                        Node {
                            width: percent(50),
                            height: percent(100),
                            overflow: Overflow::scroll_y(),
                            ..default()
                        },
                        ScrollPosition::default(),
                        crate::ui::ScrollPane,
                    ))
                    .with_children(|right| {
                        right.spawn((
                            Text::new(description),
                            Node {
                                flex_shrink: 0.0,
                                width: percent(100),
                                ..default()
                            },
                            TextFont::from_font_size(18.0),
                            TextColor(Color::srgb(0.9, 0.91, 0.86)),
                            InspectionText,
                        ));
                    });
            });
        });
}

fn rotate(
    mouse: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    viewports: Query<&Interaction, With<InspectionViewport>>,
    mut models: Query<&mut Transform, With<InspectionModel>>,
) {
    if mouse.pressed(MouseButton::Left) && viewports.iter().any(|i| *i != Interaction::None) {
        for mut model in &mut models {
            model.rotate_y(-motion.delta.x * 0.012);
            model.rotate_local_x(-motion.delta.y * 0.012);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inspecting_unlabelled_chemistry_does_not_identify_it() {
        let mut c = Container::new(ContainerKind::Bottle);
        let _ = c
            .solution
            .add(chem_sim::ReagentId(0), chem_sim::Units::whole(5));
        assert_eq!(container_description(&c), "Unknown contents");
        c.solution.clear();
        assert_eq!(container_description(&c), "Empty");
    }

    #[test]
    fn labels_override_measurements_without_changing_the_record() {
        let report = crate::analysis_reports::AnalysisReport {
            id: 7,
            sample: 9,
            scanned_at: 10.0,
            chemicals: vec![],
            ph: 12.0,
            temperature: 420.0,
            case: None,
        };
        let label = Label("Definitely drinking water".into());
        assert_eq!(
            chemical_or_document_description(Some(&label), None, Some(&report)).unwrap(),
            "LABEL\n\nDefinitely drinking water"
        );
        assert!(chemical_or_document_description(None, None, Some(&report))
            .unwrap()
            .contains("pH 12.00"));
        assert_eq!(report.ph, 12.0);
        let paper = Container::new(ContainerKind::PhPaperAcid);
        assert_eq!(
            chemical_or_document_description(Some(&label), Some(&paper), None).unwrap(),
            "LABEL\n\nDefinitely drinking water"
        );
        assert!(chemical_or_document_description(None, Some(&paper), None)
            .unwrap()
            .contains(ContainerKind::PhPaperAcid.label()));
    }

    #[test]
    fn framing_includes_scaled_rotated_mesh_extents() {
        let bounds = Aabb::from_min_max(Vec3::new(-1.0, -2.0, -0.1), Vec3::new(1.0, 2.0, 0.1));
        let transform = Transform::from_translation(Vec3::new(3.0, 4.0, 5.0))
            .with_rotation(Quat::from_rotation_z(std::f32::consts::FRAC_PI_2))
            .with_scale(Vec3::splat(2.0));
        let (low, high) = transformed_bounds(bounds, &transform);
        assert!((low - Vec3::new(-1.0, 2.0, 4.8)).length() < 0.001);
        assert!((high - Vec3::new(7.0, 6.0, 5.2)).length() < 0.001);
    }

    #[test]
    fn inspecting_preserves_held_item_and_machine_claim_and_closes_on_loss() {
        let mut app = App::new();
        app.init_resource::<ButtonInput<KeyCode>>()
            .init_resource::<crate::settings::Settings>()
            .add_systems(Update, input);
        let player = app
            .world_mut()
            .spawn((LocalPlayer, InteractionMode::Roaming))
            .id();
        let mut machine = crate::machines::Machine::new(crate::machines::MachineKind::Analyzer);
        machine.in_use_by = Some(player);
        let machine = app.world_mut().spawn(machine).id();
        *app.world_mut().get_mut::<InteractionMode>(player).unwrap() =
            InteractionMode::UsingMachine(machine);
        let item = app.world_mut().spawn(HeldBy(player)).id();
        let key = app
            .world()
            .resource::<crate::settings::Settings>()
            .bindings
            .inspect;
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .press(key);
        app.update();
        assert!(
            matches!(*app.world().get::<InteractionMode>(player).unwrap(), InteractionMode::Inspecting { item: current, machine: Some(m) } if current == item && m == machine)
        );
        assert_eq!(app.world().get::<HeldBy>(item).unwrap().0, player);
        assert_eq!(
            app.world()
                .get::<crate::machines::Machine>(machine)
                .unwrap()
                .in_use_by,
            Some(player)
        );
        app.world_mut()
            .resource_mut::<ButtonInput<KeyCode>>()
            .reset_all();
        app.world_mut().entity_mut(item).remove::<HeldBy>();
        app.update();
        assert_eq!(
            *app.world().get::<InteractionMode>(player).unwrap(),
            InteractionMode::UsingMachine(machine)
        );
    }
}
