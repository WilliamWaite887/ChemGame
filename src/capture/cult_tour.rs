//! Debug-only review in the existing no-save asset tour.
//! cargo run -- --solo --asset-tour target/cult-pass/review --cult-tour
//! Wave timing is staged; the cleanup requests and renderer are production code.
use super::asset_tour::AssetTour;
use crate::{
    arc::{AntagId, ArcOutcome, Campaign, Mode},
    containers::{Container, ContainerKind, HeldBy},
    cult::{
        aftermath::{self, Cleanup, CultResidue},
        CultScript,
    },
    interaction::{authority_segment_blocked, InteractRequested},
    lab::{MapReady, Solid, WalkableAreas},
    player::{Chemist, EYE_HEIGHT},
    session::SessionKind,
    AppState,
};
use bevy::prelude::*;
use bevy_replicon::prelude::*;
use chem_sim::Units;

#[derive(Resource, Default)]
pub(super) struct CultReview {
    phase: Option<usize>,
    pub ready: bool,
    pending: Option<String>,
}
#[derive(SystemSet, Clone, Debug, Hash, PartialEq, Eq)]
pub(super) struct ReviewUpdate;
#[derive(Component)]
struct ReviewBottle;

pub(super) fn register(app: &mut App) {
    if !std::env::args().any(|arg| arg == "--cult-tour") {
        return;
    }
    app.init_resource::<CultReview>().add_systems(
        Update,
        (
            advance_review,
            aftermath::reconcile_entities,
            cue_cleanup,
            aftermath::clean_residue,
        )
            .chain()
            .in_set(ReviewUpdate)
            .run_if(in_state(AppState::Playing))
            .run_if(resource_exists::<MapReady>)
            .run_if(resource_exists::<CultReview>)
            .run_if(resource_exists::<AssetTour>)
            .run_if(|kind: Res<SessionKind>| *kind == SessionKind::Trailer),
    );
}

fn advance_review(
    mut commands: Commands,
    tour: Res<AssetTour>,
    mut review: ResMut<CultReview>,
    campaign: Option<Res<Campaign>>,
) {
    let phase = match tour.shot {
        0..4 => 0,
        4..16 => 1,
        16..28 => 2,
        _ => 3,
    };
    if review.phase == Some(phase) {
        return;
    }
    let script: CultScript = ron::from_str(include_str!("../../assets/data/station.cult.ron"))
        .expect("Cult review content");
    let mut campaign = campaign
        .map(|c| c.clone())
        .unwrap_or_else(|| Campaign::new(AntagId::Cult, Mode::Chemist, 4));
    if phase == 0 {
        campaign = Campaign::new(AntagId::Cult, Mode::Chemist, 4);
    }
    campaign.cult_incidents.resize(
        script.guard_ward_index(if phase == 0 { 0 } else { 2 }) + 1,
        false,
    );
    if phase == 2 {
        // A valid five-ward resolution, deliberately leaving five original
        // manifestations untreated so their aftermath also gets reviewed.
        for index in [1, 2, 3, 6, 9] {
            campaign.cult_incidents[index] = true;
        }
        campaign.outcome = Some(ArcOutcome::StoppedDirectly);
    }
    let arc = campaign.active[0].clone();
    campaign.cult_aftermath.advance(&arc, &script);
    info!(
        "Cult review phase {phase}: {} sites, {} spent",
        campaign.cult_aftermath.pending(),
        campaign.cult_aftermath.spent_pending()
    );
    commands.insert_resource(campaign);
    review.phase = Some(phase);
    review.ready = phase != 3;
}

// Approach each actual entity on its loaded floor and send a real hand action.
// Fail visibly if fixtures leave no legal approach or cleanup rejects a request.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn cue_cleanup(
    mut commands: Commands,
    mut review: ResMut<CultReview>,
    campaign: Option<Res<Campaign>>,
    mut player: Query<(Entity, &Chemist, &mut Transform), With<Chemist>>,
    targets: Query<(Entity, &CultResidue, &Transform), Without<Chemist>>,
    solids: Query<(&Transform, &Solid), Without<Chemist>>,
    floor: Res<WalkableAreas>,
    db: Res<crate::chem_data::ChemDb>,
    bottles: Query<Entity, With<ReviewBottle>>,
    mut requests: MessageWriter<FromClient<InteractRequested>>,
    mut exit: MessageWriter<AppExit>,
) {
    if review.phase != Some(3) || review.ready {
        return;
    }
    let Some(campaign) = campaign else {
        return;
    };
    if let Some(spot) = review.pending.take() {
        if !campaign
            .cult_aftermath
            .sites
            .iter()
            .any(|r| r.site.spot == spot && r.cleared)
        {
            error!("Cult review cleanup rejected: {spot}");
            exit.write(AppExit::error());
            return;
        }
        info!("Cult review actual cleanup passed: {spot}");
    }
    for bottle in &bottles {
        commands.entity(bottle).despawn();
    }
    if campaign.cult_aftermath.pending() == 0 {
        review.ready = true;
        info!("Cult review cleanup complete: all sites cleared through authority interactions");
        return;
    }
    let Some((target, residue, at)) = targets.iter().find(|(_, r, _)| {
        campaign
            .cult_aftermath
            .sites
            .iter()
            .any(|s| s.site.spot == r.spot && !s.cleared)
    }) else {
        return;
    };
    let Ok((actor, chemist, mut transform)) = player.single_mut() else {
        return;
    };
    let approach = [1.2_f32, 1.8, 2.4]
        .into_iter()
        .flat_map(|radius| {
            (0..32).map(move |index| {
                let angle = index as f32 * std::f32::consts::TAU / 32.0;
                Vec3::new(
                    at.translation.x + radius * angle.sin(),
                    EYE_HEIGHT,
                    at.translation.z + radius * angle.cos(),
                )
            })
        })
        .find(|point| {
            floor
                .regions()
                .iter()
                .any(|r| r.bounds.inset(0.25).holds(*point))
                && solids.iter().all(|(center, solid)| {
                    !authority_segment_blocked(
                        *point,
                        at.translation,
                        center.translation,
                        solid.half_extents,
                    ) && !(center.translation.y + solid.half_extents.y > 0.15
                        && (point.x - center.translation.x).abs() < solid.half_extents.x + 0.25
                        && (point.z - center.translation.z).abs() < solid.half_extents.z + 0.25)
                })
        });
    let Some(approach) = approach else {
        error!(
            "Cult review has no legal cleanup approach: {}",
            residue.spot
        );
        exit.write(AppExit::error());
        return;
    };
    transform.translation = approach;
    let record = campaign
        .cult_aftermath
        .sites
        .iter()
        .find(|r| r.site.spot == residue.spot)
        .unwrap();
    if !record.spent || record.site.cleanup != Cleanup::Dismantle {
        let mut bottle = Container::new(ContainerKind::Bottle);
        let _ = bottle.solution.add(
            db.reagents.id_of("space_cleaner").unwrap(),
            Units::whole(record.site.amount as i32),
        );
        commands.spawn((
            bottle,
            HeldBy(actor),
            ReviewBottle,
            crate::until_we_leave_the_lab(),
        ));
    }
    review.pending = Some(residue.spot.clone());
    requests.write(FromClient {
        client_id: chemist.client,
        message: InteractRequested { target },
    });
}

pub(super) const VIEWS: &[(&str, [f32; 3], [f32; 3])] = &[
    (
        "early-hall-marks",
        [-51.00, 1.70, 14.00],
        [-52.00, 0.20, 12.50],
    ),
    (
        "early-medical-etching",
        [-18.70, 1.70, 8.80],
        [-16.60, 1.30, 8.00],
    ),
    (
        "early-chapel-shrine",
        [-26.50, 1.70, 48.90],
        [-25.40, 0.35, 50.00],
    ),
    (
        "early-quiet-cache",
        [-44.80, 1.70, 48.70],
        [-46.00, 0.20, 50.30],
    ),
    (
        "late-hall-marks",
        [-51.00, 1.70, 14.00],
        [-52.00, 0.20, 12.50],
    ),
    (
        "late-medical-etching",
        [-18.70, 1.70, 8.80],
        [-16.60, 1.30, 8.00],
    ),
    (
        "late-chapel-shrine",
        [-26.50, 1.70, 48.90],
        [-25.40, 0.35, 50.00],
    ),
    (
        "late-quiet-cache",
        [-44.80, 1.70, 48.70],
        [-46.00, 0.20, 50.30],
    ),
    (
        "late-engineering-vent",
        [-102.80, 1.70, 17.30],
        [-104.00, 1.30, 15.15],
    ),
    (
        "late-security-roots",
        [-93.40, 1.70, 6.90],
        [-94.50, 0.10, 8.50],
    ),
    (
        "late-hall-banner",
        [-73.80, 1.70, 12.80],
        [-75.00, 1.40, 14.80],
    ),
    (
        "late-cargo-shrine",
        [-60.80, 1.70, 37.00],
        [-62.00, 0.30, 35.20],
    ),
    (
        "late-chapel-fracture",
        [-37.50, 1.70, 46.20],
        [-36.00, 0.10, 47.50],
    ),
    (
        "late-atmos-vent",
        [-57.00, 1.70, 17.30],
        [-56.00, 1.30, 15.15],
    ),
    (
        "late-medical-banner",
        [-25.40, 1.70, 7.80],
        [-24.00, 1.30, 9.80],
    ),
    (
        "late-maintenance-fracture",
        [-48.20, 1.70, -10.50],
        [-50.00, 0.15, -10.50],
    ),
    (
        "spent-hall-marks",
        [-51.00, 1.70, 14.00],
        [-52.00, 0.20, 12.50],
    ),
    (
        "spent-medical-etching",
        [-18.70, 1.70, 8.80],
        [-16.60, 1.30, 8.00],
    ),
    (
        "spent-chapel-shrine",
        [-26.50, 1.70, 48.90],
        [-25.40, 0.35, 50.00],
    ),
    (
        "spent-quiet-cache",
        [-44.80, 1.70, 48.70],
        [-46.00, 0.20, 50.30],
    ),
    (
        "spent-engineering-vent",
        [-102.80, 1.70, 17.30],
        [-104.00, 1.30, 15.15],
    ),
    (
        "spent-security-roots",
        [-93.40, 1.70, 6.90],
        [-94.50, 0.10, 8.50],
    ),
    (
        "spent-hall-banner",
        [-73.80, 1.70, 12.80],
        [-75.00, 1.40, 14.80],
    ),
    (
        "spent-cargo-shrine",
        [-60.80, 1.70, 37.00],
        [-62.00, 0.30, 35.20],
    ),
    (
        "spent-chapel-fracture",
        [-37.50, 1.70, 46.20],
        [-36.00, 0.10, 47.50],
    ),
    (
        "spent-atmos-vent",
        [-57.00, 1.70, 17.30],
        [-56.00, 1.30, 15.15],
    ),
    (
        "spent-medical-banner",
        [-25.40, 1.70, 7.80],
        [-24.00, 1.30, 9.80],
    ),
    (
        "spent-maintenance-fracture",
        [-48.20, 1.70, -10.50],
        [-50.00, 0.15, -10.50],
    ),
    (
        "cleaned-hall-marks",
        [-51.00, 1.70, 14.00],
        [-52.00, 0.20, 12.50],
    ),
    (
        "cleaned-medical-etching",
        [-18.70, 1.70, 8.80],
        [-16.60, 1.30, 8.00],
    ),
    (
        "cleaned-chapel-shrine",
        [-26.50, 1.70, 48.90],
        [-25.40, 0.35, 50.00],
    ),
    (
        "cleaned-quiet-cache",
        [-44.80, 1.70, 48.70],
        [-46.00, 0.20, 50.30],
    ),
];
