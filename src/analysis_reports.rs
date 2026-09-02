//! Immutable measurements on physical paper. Claims (Label) never enter a scan.
use crate::{
    chem_data::ChemDb,
    containers::{Container, HeldBy, InSlot, InventorySlot},
    machines::{Machine, MachineKind},
    net::is_authority,
    AppState,
};
use bevy::{ecs::entity::MapEntities, prelude::*};
use bevy_replicon::prelude::*;
use chem_sim::Solution;
use serde::{Deserialize, Serialize};

#[derive(Component, Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SampleId(pub u64);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeasuredChemical {
    pub key: String,
    pub name: String,
    pub amount_raw: i32,
    pub purity: f32,
    pub ph: f32,
}

#[derive(Component, Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnalysisReport {
    pub id: u64,
    pub sample: u64,
    pub scanned_at: f64,
    pub chemicals: Vec<MeasuredChemical>,
    pub ph: f32,
    pub temperature: f32,
    pub case: Option<u64>,
}
impl AnalysisReport {
    pub fn measure(
        solution: &Solution,
        db: &ChemDb,
        sample: u64,
        time: f64,
        case: Option<u64>,
    ) -> Self {
        Self {
            id: rand::random(),
            sample,
            scanned_at: time,
            chemicals: solution
                .iter()
                .map(|(r, q)| MeasuredChemical {
                    key: db.reagents.get(r).key.clone(),
                    name: db.reagents.get(r).name.clone(),
                    amount_raw: q.raw(),
                    purity: solution.purity_of(r),
                    ph: solution.reagent_ph(r),
                })
                .collect(),
            ph: solution.ph(),
            temperature: solution.temperature.0,
            case,
        }
    }
    pub fn matches(&self, solution: &Solution, db: &ChemDb) -> bool {
        let measured = Self::measure(solution, db, self.sample, self.scanned_at, self.case);
        self.chemicals == measured.chemicals
            && self.ph == measured.ph
            && self.temperature == measured.temperature
    }
    pub fn read(&self) -> String {
        let lines = self
            .chemicals
            .iter()
            .map(|c| {
                format!(
                    "{}   {}   {:.1}% purity",
                    c.name,
                    chem_sim::Units::from_raw(c.amount_raw),
                    c.purity * 100.0
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "STATION LABORATORY\nANALYSIS REPORT\n\nReport {:016X}\nSample {:016X}\nScan time {:.1}s{}\n\n{}\n\npH {:.2}   Temperature {:.1}K\n\nMeasured composition at time of scan.",
            self.id,
            self.sample,
            self.scanned_at,
            self.case
                .map_or(String::new(), |id| format!("\nCase {id:016X}")),
            lines,
            self.ph,
            self.temperature
        )
    }
}

#[derive(Component, Clone, Debug, Serialize, Deserialize)]
pub struct AnalyzerSnapshot {
    #[entities]
    pub item: Entity,
    pub report: AnalysisReport,
}
#[derive(Component, Serialize, Deserialize)]
pub struct ReportOutput(#[entities] pub Entity);
#[derive(Message, Clone, Serialize, Deserialize, MapEntities)]
pub struct PrintReportRequested {
    #[entities]
    pub machine: Entity,
    pub report_id: u64,
}

pub struct AnalysisReportPlugin;
impl Plugin for AnalysisReportPlugin {
    fn build(&self, app: &mut App) {
        app.replicate::<SampleId>()
            .replicate::<AnalysisReport>()
            .replicate::<AnalyzerSnapshot>()
            .replicate::<ReportOutput>()
            .add_mapped_client_message::<PrintReportRequested>(Channel::Ordered)
            .add_observer(sample_unloaded)
            .add_observer(report_collected)
            .add_systems(
                Update,
                (
                    (
                        scan.after(crate::machines::handle_analyze),
                        print,
                        clear_output,
                    )
                        .chain()
                        .run_if(is_authority),
                    dress,
                )
                    .run_if(in_state(AppState::Playing)),
            );
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn scan(
    mut commands: Commands,
    db: Res<ChemDb>,
    time: Res<Time>,
    mut requests: MessageReader<FromClient<crate::machines::AnalyzeRequested>>,
    machines: Query<&Machine>,
    chemists: Query<(Entity, &crate::player::Chemist)>,
    bodies: Query<&crate::body::Body>,
    blood: Query<&crate::body::Bloodstream>,
    loaded: Query<(Entity, &InSlot)>,
    containers: Query<(
        &Container,
        Option<&SampleId>,
        Option<&crate::security_case::CaseSample>,
    )>,
) {
    let mut scanned = std::collections::HashSet::new();
    for request in requests.read() {
        if crate::machines::authorized_machine_actor(
            request.client_id,
            machines.get(request.machine).ok(),
            &[MachineKind::Analyzer],
            &chemists,
            &bodies,
            &blood,
        )
        .is_none()
            || !scanned.insert(request.machine)
        {
            continue;
        }
        let Some(item) = crate::machines::slotted_container(request.machine, &loaded) else {
            continue;
        };
        let Ok((container, id, case)) = containers.get(item) else {
            continue;
        };
        let id = id.copied().unwrap_or_else(|| SampleId(rand::random()));
        commands.entity(item).insert(id);
        let report = AnalysisReport::measure(
            &container.solution,
            &db,
            id.0,
            time.elapsed_secs_f64(),
            case.map(|case| case.0),
        );
        commands
            .entity(request.machine)
            .insert(AnalyzerSnapshot { item, report });
    }
}

// A snapshot belongs to one continuous loading of a sample. Merely putting
// the same unchanged bottle back must not resurrect an earlier analysis.
fn sample_unloaded(
    event: On<Discard, InSlot>,
    mut commands: Commands,
    snapshots: Query<(Entity, &AnalyzerSnapshot)>,
) {
    for (machine, snapshot) in &snapshots {
        if snapshot.item == event.entity {
            commands.entity(machine).try_remove::<AnalyzerSnapshot>();
        }
    }
}

fn report_collected(
    event: On<Insert, InventorySlot>,
    mut commands: Commands,
    reports: Query<(), With<ReportOutput>>,
) {
    if reports.contains(event.entity) {
        commands.entity(event.entity).try_remove::<ReportOutput>();
    }
}

#[allow(clippy::too_many_arguments)]
fn print(
    mut commands: Commands,
    db: Res<ChemDb>,
    mut requests: MessageReader<FromClient<PrintReportRequested>>,
    machines: Query<(
        &Machine,
        &Transform,
        &crate::lab::Solid,
        Option<&crate::machines::Facing>,
        &AnalyzerSnapshot,
    )>,
    chemists: Query<(Entity, &crate::player::Chemist)>,
    bodies: Query<&crate::body::Body>,
    blood: Query<&crate::body::Bloodstream>,
    samples: Query<(&Container, &InSlot)>,
    outputs: Query<&ReportOutput>,
) {
    let mut occupied: std::collections::HashSet<_> = outputs.iter().map(|o| o.0).collect();
    for request in requests.read() {
        if occupied.contains(&request.machine) {
            continue;
        }
        let Ok((machine, transform, solid, facing, snapshot)) = machines.get(request.machine)
        else {
            continue;
        };
        if crate::machines::authorized_machine_actor(
            request.client_id,
            Some(machine),
            &[MachineKind::Analyzer],
            &chemists,
            &bodies,
            &blood,
        )
        .is_none()
        {
            continue;
        }
        let Ok((sample, slot)) = samples.get(snapshot.item) else {
            continue;
        };
        if slot.0 != request.machine
            || snapshot.report.id != request.report_id
            || !snapshot.report.matches(&sample.solution, &db)
        {
            continue;
        }
        let position = crate::machines::front_of(transform, solid, facing, 0.02);
        let report = spawn(&mut commands, snapshot.report.clone(), position);
        commands
            .entity(report)
            .insert(ReportOutput(request.machine));
        occupied.insert(request.machine);
    }
}
fn clear_output(
    mut commands: Commands,
    collected: Query<Entity, (With<ReportOutput>, Or<(With<InventorySlot>, With<HeldBy>)>)>,
) {
    for item in &collected {
        commands.entity(item).remove::<ReportOutput>();
    }
}
pub fn spawn(commands: &mut Commands, report: AnalysisReport, position: Vec3) -> Entity {
    commands
        .spawn((
            Replicated,
            report,
            Transform::from_translation(position),
            Visibility::default(),
            crate::interaction::Interactable::new("Analysis report"),
            crate::until_we_leave_the_lab(),
        ))
        .id()
}
fn dress(
    mut commands: Commands,
    papers: Query<Entity, Added<AnalysisReport>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    for entity in &papers {
        commands.entity(entity).insert((
            Mesh3d(meshes.add(Cuboid::new(0.16, 0.003, 0.22))),
            MeshMaterial3d(materials.add(Color::srgb(0.91, 0.9, 0.81))),
        ));
        commands.entity(entity).with_children(|paper| {
            for line in 0..11 {
                paper.spawn((
                    Mesh3d(meshes.add(Cuboid::new(
                        if line < 2 {
                            0.11
                        } else {
                            0.09 + (line % 3) as f32 * 0.008
                        },
                        0.001,
                        0.002,
                    ))),
                    MeshMaterial3d(materials.add(Color::srgb(0.12, 0.18, 0.2))),
                    Transform::from_xyz(-0.005, 0.0025, -0.075 + line as f32 * 0.013),
                ));
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::containers::ContainerKind;
    use chem_sim::{ChemData, Units};

    fn fixture() -> (App, Entity, Entity, ClientId) {
        let db = ChemDb(
            ChemData::from_ron(
                include_str!("../assets/data/chem.reagents.ron"),
                include_str!("../assets/data/chem.reactions.ron"),
            )
            .unwrap(),
        );
        let mut sample = Container::new(ContainerKind::Bottle);
        let _ = sample.solution.add(db.reagent("water"), Units::whole(8));
        let mut app = App::new();
        app.insert_resource(db)
            .init_resource::<Time>()
            .add_message::<FromClient<crate::machines::AnalyzeRequested>>()
            .add_message::<FromClient<PrintReportRequested>>()
            .add_observer(sample_unloaded)
            .add_observer(report_collected)
            .add_systems(Update, (scan, print, clear_output).chain());
        let client = ClientId::Client(app.world_mut().spawn_empty().id());
        let player = app
            .world_mut()
            .spawn(crate::player::Chemist { client })
            .id();
        let mut machine = Machine::new(MachineKind::Analyzer);
        machine.in_use_by = Some(player);
        let machine = app
            .world_mut()
            .spawn((
                machine,
                Transform::default(),
                crate::lab::Solid {
                    half_extents: Vec3::splat(0.5),
                },
            ))
            .id();
        let sample = app
            .world_mut()
            .spawn((
                sample,
                InSlot(machine),
                SampleId(123),
                crate::security_case::CaseSample(456),
            ))
            .id();
        (app, machine, sample, client)
    }

    fn analyze(app: &mut App, machine: Entity, client: ClientId) -> AnalysisReport {
        app.world_mut().write_message(FromClient {
            client_id: client,
            message: crate::machines::AnalyzeRequested { machine },
        });
        app.update();
        app.world()
            .get::<AnalyzerSnapshot>(machine)
            .unwrap()
            .report
            .clone()
    }

    fn request_print(app: &mut App, machine: Entity, id: u64, client: ClientId) {
        app.world_mut().write_message(FromClient {
            client_id: client,
            message: PrintReportRequested {
                machine,
                report_id: id,
            },
        });
    }

    fn reports(app: &mut App) -> Vec<(Entity, AnalysisReport)> {
        app.world_mut()
            .query::<(Entity, &AnalysisReport)>()
            .iter(app.world())
            .map(|(e, r)| (e, r.clone()))
            .collect()
    }

    #[test]
    fn repeated_prints_wait_for_collection_and_keep_an_immutable_identity() {
        let (mut app, machine, sample, client) = fixture();
        let measurement = analyze(&mut app, machine, client);
        assert_eq!(measurement.sample, 123);
        assert_eq!(measurement.case, Some(456));
        request_print(&mut app, machine, measurement.id, client);
        request_print(&mut app, machine, measurement.id, client);
        app.update();
        let papers = reports(&mut app);
        assert_eq!(papers.len(), 1);
        assert_eq!(papers[0].1, measurement);
        let player = app
            .world()
            .get::<Machine>(machine)
            .unwrap()
            .in_use_by
            .unwrap();
        app.world_mut().entity_mut(papers[0].0).insert((
            InventorySlot {
                owner: player,
                slot: 0,
            },
            HeldBy(player),
        ));
        request_print(&mut app, machine, measurement.id, client);
        app.update();
        assert_eq!(reports(&mut app).len(), 2);
        assert!(reports(&mut app).iter().all(|(_, r)| *r == measurement));
        app.world_mut()
            .get_mut::<Container>(sample)
            .unwrap()
            .solution
            .clear();
        assert_eq!(
            app.world().get::<AnalysisReport>(papers[0].0).unwrap(),
            &measurement
        );
        assert!(!measurement.matches(
            &app.world().get::<Container>(sample).unwrap().solution,
            app.world().resource::<ChemDb>()
        ));
    }

    #[test]
    fn unloading_and_reinserting_the_same_sample_requires_a_new_scan() {
        let (mut app, machine, sample, client) = fixture();
        let old = analyze(&mut app, machine, client);
        app.world_mut().entity_mut(sample).remove::<InSlot>();
        app.world_mut().entity_mut(sample).insert(InSlot(machine));
        request_print(&mut app, machine, old.id, client);
        app.update();
        assert!(app.world().get::<AnalyzerSnapshot>(machine).is_none());
        assert!(reports(&mut app).is_empty());
        let current = analyze(&mut app, machine, client);
        assert_eq!(current.sample, old.sample);
        assert_ne!(current.id, old.id);
        request_print(&mut app, machine, current.id, client);
        app.update();
        assert_eq!(reports(&mut app).len(), 1);
    }

    #[test]
    fn stale_measurements_and_non_owners_cannot_print() {
        let (mut app, machine, sample, client) = fixture();
        let measurement = analyze(&mut app, machine, client);
        let stranger = ClientId::Client(app.world_mut().spawn_empty().id());
        app.world_mut()
            .spawn(crate::player::Chemist { client: stranger });
        request_print(&mut app, machine, measurement.id, stranger);
        request_print(&mut app, machine, measurement.id.wrapping_add(1), client);
        app.update();
        assert!(reports(&mut app).is_empty());
        app.world_mut()
            .get_mut::<Container>(sample)
            .unwrap()
            .solution
            .temperature
            .0 += 10.0;
        request_print(&mut app, machine, measurement.id, client);
        app.update();
        assert!(reports(&mut app).is_empty());
    }

    #[test]
    fn an_unauthorized_scan_cannot_block_the_owner_in_the_same_frame() {
        let (mut app, machine, _, owner) = fixture();
        let stranger = ClientId::Client(app.world_mut().spawn_empty().id());
        app.world_mut()
            .spawn(crate::player::Chemist { client: stranger });
        for client in [stranger, owner] {
            app.world_mut().write_message(FromClient {
                client_id: client,
                message: crate::machines::AnalyzeRequested { machine },
            });
        }
        app.update();
        let snapshot = app
            .world()
            .get::<AnalyzerSnapshot>(machine)
            .expect("the authorized scan must still be processed");
        assert_eq!(snapshot.report.sample, 123);
        assert_eq!(snapshot.report.case, Some(456));
    }

    #[test]
    fn report_and_print_request_survive_actual_wire_serialization() {
        let (mut app, machine, _, client) = fixture();
        let report = analyze(&mut app, machine, client);
        let encoded = postcard::to_allocvec(&report).unwrap();
        assert_eq!(
            postcard::from_bytes::<AnalysisReport>(&encoded).unwrap(),
            report
        );
        let request = PrintReportRequested {
            machine,
            report_id: report.id,
        };
        let encoded = postcard::to_allocvec(&request).unwrap();
        let mut decoded = postcard::from_bytes::<PrintReportRequested>(&encoded).unwrap();
        assert_eq!(decoded.machine, machine);
        assert_eq!(decoded.report_id, report.id);
        let remote = app.world_mut().spawn_empty().id();
        decoded.map_entities(&mut (machine, remote));
        assert_eq!(decoded.machine, remote);
        assert_eq!(decoded.report_id, report.id);
        let encoded = postcard::to_allocvec(&AnalyzerSnapshot {
            item: machine,
            report: report.clone(),
        })
        .unwrap();
        let mut snapshot = postcard::from_bytes::<AnalyzerSnapshot>(&encoded).unwrap();
        assert_eq!(snapshot.report, report);
        <AnalyzerSnapshot as Component>::map_entities(&mut snapshot, &mut (machine, remote));
        assert_eq!(snapshot.item, remote);
        assert_eq!(
            snapshot.report, report,
            "mapping live entities must not alter stable sample or case IDs"
        );
        let bytes = postcard::to_allocvec(&ReportOutput(machine)).unwrap();
        let mut output: ReportOutput = postcard::from_bytes(&bytes).unwrap();
        <ReportOutput as Component>::map_entities(&mut output, &mut (machine, remote));
        assert_eq!(output.0, remote);
    }
}
