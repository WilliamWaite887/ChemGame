//! Local, disposable room screenshots for asset review.
//! `cargo run -- --solo --asset-tour target/asset-session/after`
//! No save slot is loaded or written. Cameras use the real station renderer.
use super::CaptureState;
use crate::{lab::MapReady, player::PlayerCamera, session::SessionKind, AppState};
use bevy::{
    prelude::*,
    render::view::screenshot::{Screenshot, ScreenshotCaptured},
};

#[derive(Resource)]
pub(super) struct AssetTour {
    output: std::path::PathBuf,
    elapsed: f32,
    pub(super) shot: usize,
    positioned: bool,
    saved: usize,
    failed: bool,
}

pub(crate) fn output_directory() -> Option<std::path::PathBuf> {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--asset-tour" {
            return Some(
                args.next()
                    .map(Into::into)
                    .unwrap_or_else(|| "target/asset-tour".into()),
            );
        }
    }
    None
}

pub(super) fn register(app: &mut App) {
    let Some(output) = output_directory() else {
        return;
    };
    std::fs::create_dir_all(&output).expect("create asset-tour output directory");
    super::cult_tour::register(app);
    app.insert_resource(AssetTour {
        output,
        elapsed: -12.0,
        shot: 0,
        positioned: false,
        saved: 0,
        failed: false,
    })
    .add_systems(
        Update,
        tour.after(super::fly_camera)
            .after(super::cult_tour::ReviewUpdate)
            .run_if(in_state(AppState::Playing))
            .run_if(resource_exists::<MapReady>)
            .run_if(resource_exists::<AssetTour>)
            .run_if(in_disposable_session),
    )
    .add_systems(OnExit(AppState::Playing), stop_tour);
}

fn in_disposable_session(kind: Res<SessionKind>) -> bool {
    *kind == SessionKind::Trailer
}

fn stop_tour(mut commands: Commands) {
    // A cancelled recording must never resume in the next career opened from
    // the menu. CapturePlugin separately restores the ordinary player camera.
    commands.remove_resource::<AssetTour>();
    commands.remove_resource::<super::cult_tour::CultReview>();
}

// Eye-height room views. Stable poses
// make before/after comparisons useful; no hidden replacement scene is rendered.
const VIEWS: &[(&str, [f32; 3], [f32; 3])] = &[
    ("security", [-88.0, 1.65, 5.0], [-99.0, 1.1, 2.0]),
    ("security-dispatch", [-88.5, 1.65, 1.4], [-85.0, 1.15, 0.0]),
    (
        "security-officer-desks",
        [-105.5, 1.65, 6.25],
        [-108.0, 1.1, 6.25],
    ),
    (
        "security-checkpoint",
        [-95.2, 1.65, 5.3],
        [-95.62, 1.15, 3.9],
    ),
    ("security-back", [-91.0, 1.65, -4.0], [-106.0, 1.1, -6.0]),
    ("medical", [-22.0, 1.65, 5.0], [-36.0, 1.0, 3.0]),
    ("medical-reception", [-23.0, 1.65, 3.8], [-20.5, 1.0, 7.6]),
    (
        "medical-nurse-workstation",
        [-20.5, 1.65, 8.45],
        [-20.5, 1.3, 7.1],
    ),
    ("medical-treatment", [-32.0, 1.65, 7.5], [-37.0, 1.0, 3.0]),
    ("engineering", [-101.0, 1.65, 25.5], [-107.0, 1.2, 27.0]),
    (
        "engineering-atmos",
        [-59.0, 1.65, 23.0],
        [-54.0, 0.85, 27.0],
    ),
    ("hallway", [-45.0, 1.65, 12.5], [-64.0, 1.2, 12.5]),
    ("chapel", [-36.5, 1.65, 47.5], [-24.0, 1.0, 47.5]),
    ("quiet-room", [-42.2, 1.65, 44.8], [-46.0, 1.0, 48.0]),
    ("ceiling", [-45.0, 1.65, 12.5], [-52.0, 3.0, 12.5]),
];

fn tour(
    mut commands: Commands,
    time: Res<Time<Real>>,
    mut tour: ResMut<AssetTour>,
    mut capture: ResMut<CaptureState>,
    mut cameras: Query<&mut Transform, With<PlayerCamera>>,
    mut exit: MessageWriter<AppExit>,
    cult: Option<Res<super::cult_tour::CultReview>>,
) {
    if cult.as_ref().is_some_and(|review| !review.ready) {
        return;
    }
    let views = if cult.is_some() {
        super::cult_tour::VIEWS
    } else {
        VIEWS
    };
    tour.elapsed += time.delta_secs();
    if tour.elapsed < 0.0 {
        return;
    }
    if tour.shot == views.len() {
        if tour.saved == views.len() {
            info!(
                "asset tour complete: {} views in {}",
                views.len(),
                tour.output.display()
            );
            exit.write(AppExit::Success);
        } else if tour.failed || tour.elapsed > 60.0 {
            error!(
                "asset tour incomplete: saved {} of {} screenshots",
                tour.saved,
                views.len()
            );
            exit.write(AppExit::error());
        }
        return;
    }
    let Ok(mut camera) = cameras.single_mut() else {
        return;
    };
    let (name, eye, aim) = views[tour.shot];
    // Reapply on every tick so incidental mouse movement cannot drift a pose.
    *camera = Transform::from_translation(Vec3::from(eye)).looking_at(Vec3::from(aim), Vec3::Y);
    super::orient(&mut capture, &camera);
    if !tour.positioned {
        capture.free_camera = true;
        capture.hide_hud = true;
        capture.clean_camera = true;
        super::orient(&mut capture, &camera);
        tour.positioned = true;
        tour.elapsed = 0.0;
    } else if tour.elapsed >= 3.0 {
        let path = tour.output.join(format!("{name}.png"));
        info!("asset tour capture {}", path.display());
        commands.spawn(Screenshot::primary_window()).observe(
            move |event: On<ScreenshotCaptured>, tour: Option<ResMut<AssetTour>>| {
                // GPU readback can finish after the user has left the tour.
                let Some(mut tour) = tour else { return };
                let saved = event
                    .image
                    .clone()
                    .try_into_dynamic()
                    .map_err(|error| format!("{error:?}"))
                    .and_then(|image| {
                        image
                            .to_rgb8()
                            .save(&path)
                            .map_err(|error| error.to_string())
                    });
                match saved {
                    Ok(()) => {
                        tour.saved += 1;
                        info!("asset tour saved {}", path.display());
                    }
                    Err(error) => {
                        tour.failed = true;
                        error!("asset tour {}: {error}", path.display());
                    }
                }
            },
        );
        tour.shot += 1;
        tour.positioned = false;
        tour.elapsed = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_tour_cannot_resume_in_a_career() {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, bevy::state::app::StatesPlugin))
            .init_state::<AppState>()
            .add_message::<AppExit>()
            .init_resource::<CaptureState>()
            .insert_resource(SessionKind::Trailer)
            .insert_resource(MapReady)
            .insert_resource(AssetTour {
                output: "target/asset-tour-test-unused".into(),
                elapsed: 0.0,
                shot: 0,
                positioned: false,
                saved: 0,
                failed: false,
            })
            .add_systems(
                Update,
                tour.run_if(in_state(AppState::Playing))
                    .run_if(resource_exists::<MapReady>)
                    .run_if(resource_exists::<AssetTour>)
                    .run_if(in_disposable_session),
            )
            .add_systems(OnExit(AppState::Playing), stop_tour);
        let camera = app
            .world_mut()
            .spawn((
                PlayerCamera {
                    chemist: Entity::PLACEHOLDER,
                },
                Transform::default(),
            ))
            .id();
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);
        app.update();
        assert!(app.world().resource::<AssetTour>().positioned);
        assert!(app.world().resource::<CaptureState>().free_camera);

        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::MainMenu);
        app.update();
        assert!(!app.world().contains_resource::<AssetTour>());

        let career_view = Transform::from_xyz(5.0, 1.7, 3.0);
        app.world_mut().entity_mut(camera).insert(career_view);
        app.world_mut().insert_resource(SessionKind::Career);
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);
        app.update();
        assert_eq!(app.world().get::<Transform>(camera), Some(&career_view));
        assert!(!app.world().contains_resource::<AssetTour>());
    }
}
