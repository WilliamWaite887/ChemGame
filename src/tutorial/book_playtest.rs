//! Opt-in rendered textbook checks, also used by a real LAN host and client.
use crate::{
    settings::{PauseScreen, Paused},
    textbook::TextbookView,
    AppState,
};
use bevy::{
    prelude::*,
    render::view::screenshot::{save_to_disk, Screenshot},
};

#[derive(Resource, Default)]
struct Run {
    started: Option<std::time::Instant>,
    phase: u8,
    clock: f64,
}
pub(super) fn install(app: &mut App) {
    app.init_resource::<Run>()
        .add_systems(
            PreUpdate,
            super::playtest::ignore_live_input.after(bevy::input::InputSystems),
        )
        .add_systems(Update, drive);
}
fn shot(world: &mut World, role: &str, name: &str) {
    world
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(format!(
            "target/textbook-playtest/{role}/{name}.png"
        )));
}
fn drive(world: &mut World) {
    if *world.resource::<State<AppState>>().get() != AppState::Playing
        || !world.contains_resource::<crate::lab::MapReady>()
        || world
            .query_filtered::<Entity, With<crate::player::LocalPlayer>>()
            .iter(world)
            .next()
            .is_none()
    {
        return;
    }
    let mode = *world.resource::<crate::net::LaunchMode>();
    let (role, finish_after) = match mode {
        crate::net::LaunchMode::Singleplayer => ("solo", 12.0),
        crate::net::LaunchMode::Join(_) => ("client", 12.0),
        _ => ("host", 35.0),
    };
    if world.resource::<Run>().started.is_none() {
        let _ = std::fs::create_dir_all(format!("target/textbook-playtest/{role}"));
        world.resource_mut::<Run>().started = Some(std::time::Instant::now());
        world.resource_mut::<Paused>().0 = true;
        *world.resource_mut::<PauseScreen>() = PauseScreen::Textbook;
        return;
    }
    let elapsed = world
        .resource::<Run>()
        .started
        .unwrap()
        .elapsed()
        .as_secs_f32();
    match world.resource::<Run>().phase {
        0 if elapsed > 2.0 => {
            shot(world, role, "index");
            world.resource_mut::<Run>().clock =
                world.resource::<Time<Virtual>>().elapsed_secs_f64();
            world.resource_mut::<Run>().phase = 1;
        }
        1 if elapsed > 4.0 => {
            let progressed = world.resource::<Time<Virtual>>().elapsed_secs_f64()
                > world.resource::<Run>().clock + 0.1;
            let expected = role != "solo";
            if progressed != expected {
                let _ = std::fs::write(
                    format!("target/textbook-playtest/{role}/result.txt"),
                    "FAIL: incorrect simulation pause behavior",
                );
                world.write_message(AppExit::Success);
                return;
            }
            world.resource_mut::<TextbookView>().article = Some(1);
            *world.resource_mut::<PauseScreen>() = PauseScreen::TextbookArticle;
            world.resource_mut::<Run>().phase = 2;
        }
        2 if elapsed > 6.0 => {
            shot(world, role, "article");
            world.resource_mut::<Run>().phase = 3;
        }
        3 if elapsed > finish_after => {
            let _ = std::fs::write(format!("target/textbook-playtest/{role}/result.txt"),
                format!("PASS\nRole: {role}\nTextbook index and article rendered. World {} while reading.\n",if role=="solo"{"paused"}else{"continued"}));
            world.write_message(AppExit::Success);
        }
        _ => {}
    }
}
