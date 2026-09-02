use super::*;
use crate::{
    menu::{choice, MenuScreen},
    settings::{PauseScreen, Paused},
    ui::{button, label, row, PANEL_BG, TEXT, TEXT_DIM},
};
#[derive(Component)]
struct MenuRoot;
#[derive(Component)]
struct HudRoot;
#[derive(Component, Clone)]
pub(super) enum Action {
    Continue,
    Lesson(String),
    Topic(String),
    Lessons,
    Free,
    Back,
    Help,
    Hint,
    Details,
    Restart,
    Skip,
    Leave,
    Career,
    Clean,
}

pub(super) fn install(app: &mut App) {
    app.add_systems(OnEnter(MenuScreen::Training), training_menu)
        .add_systems(OnEnter(MenuScreen::TrainingLessons), lesson_menu)
        .add_systems(OnExit(MenuScreen::Training), clear_menu)
        .add_systems(OnExit(MenuScreen::TrainingLessons), clear_menu)
        .add_systems(
            Update,
            clicks.run_if(in_state(AppState::Playing).or_else(in_state(AppState::MainMenu))),
        )
        .add_systems(
            Update,
            hud.run_if(in_state(AppState::Playing))
                .run_if(crate::session::training_session),
        )
        .add_systems(
            PostUpdate,
            super::guidance::draw
                .after(bevy::transform::TransformSystems::Propagate)
                .after(super::advance)
                .run_if(in_state(AppState::Playing))
                .run_if(crate::session::training_session),
        );
}
pub fn pause_controls(panel: &mut ChildSpawnerCommands) {
    panel.spawn(row()).with_children(|row| {
        row.spawn(button("Training help", Action::Help));
        row.spawn(button("Restart exercise", Action::Restart));
        row.spawn(button("Leave training", Action::Leave));
    });
}
fn shell(commands: &mut Commands, title: &str, body: impl FnOnce(&mut ChildSpawnerCommands)) {
    commands
        .spawn((
            MenuRoot,
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                align_items: AlignItems::Center,
                justify_content: JustifyContent::Center,
                ..default()
            },
            BackgroundColor(PANEL_BG),
        ))
        .with_children(|root| {
            root.spawn((
                Node {
                    width: px(700),
                    max_width: percent(95),
                    max_height: percent(94),
                    padding: UiRect::all(px(24)),
                    flex_direction: FlexDirection::Column,
                    row_gap: px(8),
                    overflow: Overflow::scroll_y(),
                    ..default()
                },
                crate::ui::ScrollPane,
                ScrollPosition::default(),
            ))
            .with_children(|p| {
                p.spawn(label(title, 26.0, TEXT));
                p.spawn(label(
                    "Solo practice. Your careers and their discoveries are separate.",
                    14.0,
                    TEXT_DIM,
                ));
                body(p);
            });
        });
}
fn training_menu(mut commands: Commands, profile: Res<Profile>) {
    shell(&mut commands, "Training", |p| {
        p.spawn(choice(
            if profile.0.resume.is_some() {
                "Continue core course / last exercise"
            } else {
                "Start core course"
            },
            "About 10–15 minutes. Untimed, with optional hints.",
            Action::Continue,
        ));
        p.spawn(choice(
            "Restart core course",
            "Begin with glassware and your first request.",
            Action::Lesson("bearings".into()),
        ));
        p.spawn(choice(
            "Practice lessons",
            "Choose one system to practise with fresh supplies.",
            Action::Lessons,
        ));
        p.spawn(choice(
            "Free practice",
            "An open lab, free replacement supplies, and a textbook.",
            Action::Free,
        ));
        p.spawn(button("Back", Action::Back));
    });
}
fn lesson_menu(mut commands: Commands, lessons: Res<Lessons>, profile: Res<Profile>) {
    shell(&mut commands, "Practice lessons", |p| {
        for l in &lessons.0 {
            let status = if profile.0.completed.contains(&l.id) {
                "Completed"
            } else if profile.0.skipped.contains(&l.id) {
                "Skipped - replay anytime"
            } else if l.core {
                "Core exercise"
            } else {
                "Optional - about 3–5 minutes"
            };
            p.spawn(button(
                format!("{}   |   {}", l.title, status),
                Action::Lesson(l.id.clone()),
            ));
        }
        p.spawn(button("Back", Action::Back));
    });
}
fn clear_menu(mut commands: Commands, roots: Query<Entity, With<MenuRoot>>) {
    for root in &roots {
        commands.entity(root).try_despawn();
    }
}

fn clicks(world: &mut World) {
    let actions: Vec<_> = world
        .query_filtered::<(&Interaction, &Action), Changed<Interaction>>()
        .iter(world)
        .filter(|(i, _)| **i == Interaction::Pressed)
        .map(|(_, a)| a.clone())
        .collect();
    for action in actions {
        let playing = *world.resource::<State<AppState>>().get() == AppState::Playing;
        match action {
            Action::Continue => {
                let id = world
                    .resource::<Profile>()
                    .0
                    .resume
                    .clone()
                    .filter(|id| world.resource::<Lessons>().0.iter().any(|l| &l.id == id))
                    .unwrap_or_else(|| {
                        world
                            .resource::<Lessons>()
                            .0
                            .iter()
                            .find(|lesson| {
                                lesson.core
                                    && !world.resource::<Profile>().0.completed.contains(&lesson.id)
                                    && !world.resource::<Profile>().0.skipped.contains(&lesson.id)
                            })
                            .map_or_else(|| "bearings".into(), |lesson| lesson.id.clone())
                    });
                launch(world, id);
            }
            Action::Lesson(id) => {
                if playing {
                    restart(world, id);
                } else {
                    launch(world, id);
                }
            }
            Action::Free => {
                if playing {
                    // Keep the current lab and voluntary discoveries when the
                    // player chooses to continue experimenting after a lesson.
                    if let Some(mut runner) = world.get_resource_mut::<Runner>() {
                        runner.lesson = "free".into();
                        runner.finished = false;
                        runner.stage = 0;
                        runner.hint = 0;
                        runner.revision += 1;
                    }
                    world.resource_mut::<Paused>().0 = false;
                } else {
                    launch(world, "free".into());
                }
            }
            Action::Lessons => {
                if playing {
                    world.insert_resource(ShowLessons);
                    world
                        .resource_mut::<NextState<AppState>>()
                        .set(AppState::MainMenu);
                } else {
                    world
                        .resource_mut::<NextState<MenuScreen>>()
                        .set(MenuScreen::TrainingLessons);
                }
            }
            Action::Back => {
                let current = *world.resource::<State<MenuScreen>>().get();
                world.resource_mut::<NextState<MenuScreen>>().set(
                    if current == MenuScreen::TrainingLessons {
                        MenuScreen::Training
                    } else {
                        MenuScreen::Mode
                    },
                );
            }
            Action::Topic(id) => {
                if let Some(index) = world
                    .resource::<crate::textbook::Textbook>()
                    .0
                    .iter()
                    .position(|a| a.id == id)
                {
                    world
                        .resource_mut::<crate::textbook::TextbookView>()
                        .article = Some(index);
                    *world.resource_mut::<PauseScreen>() = PauseScreen::TextbookArticle;
                }
            }
            Action::Help => {
                world
                    .resource_mut::<PauseScreen>()
                    .set_if_neq(PauseScreen::Training);
            }
            Action::Hint => {
                if let Some(mut r) = world.get_resource_mut::<Runner>() {
                    r.hint = (r.hint + 1).min(3);
                    r.revision += 1;
                }
            }
            Action::Details => {
                if let Some(mut r) = world.get_resource_mut::<Runner>() {
                    r.expanded = !r.expanded;
                    r.revision += 1;
                }
            }
            Action::Restart => {
                if let Some(r) = world.get_resource::<Runner>() {
                    let id = r.lesson.clone();
                    restart(world, id);
                }
            }
            Action::Skip => {
                if let Some(r) = world.get_resource::<Runner>() {
                    let id = r.lesson.clone();
                    let next = world
                        .resource::<Lessons>()
                        .0
                        .iter()
                        .skip_while(|l| l.id != id)
                        .nth(1)
                        .filter(|l| l.core)
                        .map(|l| l.id.clone());
                    {
                        let mut p = world.resource_mut::<Profile>();
                        if !p.0.completed.contains(&id) {
                            p.0.skipped.insert(id);
                        }
                        p.0.resume = next.clone();
                        p.save();
                    }
                    if let Some(next) = next {
                        restart(world, next);
                    } else {
                        world.insert_resource(ShowLessons);
                        world
                            .resource_mut::<NextState<AppState>>()
                            .set(AppState::MainMenu);
                    }
                }
            }
            Action::Leave => {
                world
                    .resource_mut::<NextState<AppState>>()
                    .set(AppState::MainMenu);
            }
            Action::Career => {
                world.insert_resource(StartCareer);
                world
                    .resource_mut::<NextState<AppState>>()
                    .set(AppState::MainMenu);
            }
            Action::Clean => {
                let ids: Vec<_> = world
                    .query_filtered::<Entity, With<crate::chem_world::ChemicalPuddle>>()
                    .iter(world)
                    .collect();
                for id in ids {
                    world.despawn(id);
                }
                if let Some(mut r) = world.get_resource_mut::<Runner>() {
                    if r.evidence.has(Goal::Residue) {
                        r.evidence.set(Goal::Clean);
                    }
                    r.revision += 1;
                }
            }
        }
    }
}
#[derive(Resource)]
pub(crate) struct ShowLessons;

#[allow(clippy::too_many_arguments)]
fn hud(
    mut commands: Commands,
    runner: Res<Runner>,
    lessons: Res<Lessons>,
    settings: Res<crate::settings::Settings>,
    paused: Res<Paused>,
    screen: Res<PauseScreen>,
    book: Res<crate::textbook::Textbook>,
    players: Query<(&crate::body::Body, &crate::body::Bloodstream), With<LocalPlayer>>,
    roots: Query<Entity, With<HudRoot>>,
    mut previous: Local<Option<(u64, bool, bool, PauseScreen)>>,
) {
    let collapsed = players
        .iter()
        .any(|(b, blood)| b.0.collapsed || blood.0.incapacitated());
    let sig = (runner.revision, paused.0, collapsed, *screen);
    if *previous == Some(sig) && !roots.is_empty() {
        return;
    }
    *previous = Some(sig);
    for root in &roots {
        commands.entity(root).try_despawn();
    }
    let full = paused.0 && *screen == PauseScreen::Training;
    if paused.0 && !full {
        return;
    }
    let lesson = lessons.0.iter().find(|l| l.id == runner.lesson);
    let stage = lesson.and_then(|l| l.stages.get(runner.stage));
    commands.spawn((HudRoot,crate::until_we_leave_the_lab(),GlobalZIndex(if full{120}else{10}),
        Node{position_type:PositionType::Absolute,left:if full{percent(15)}else{px(18)},top:if full{percent(6)}else{px(100)},width:if full{percent(70)}else{px(340)},max_width:percent(94),max_height:percent(85),padding:UiRect::all(px(16)),flex_direction:FlexDirection::Column,row_gap:px(8),overflow:Overflow::scroll_y(),..default()},BackgroundColor(if full{PANEL_BG}else{Color::srgba(0.035,0.055,0.065,0.9)}),crate::ui::ScrollPane,ScrollPosition::default()))
        .with_children(|p|{
            p.spawn(label(if runner.finished{"Exercise complete"}else{lesson.map_or("Free practice",|l|l.title.as_str())},20.0,TEXT));
            if collapsed{p.spawn(label("You are incapacitated. Pause and restart this exercise to recover.",16.0,TEXT));}
            if runner.finished {
                p.spawn(label("Use Pause > Training help to choose what comes next.",15.0,TEXT_DIM));
                if full{p.spawn(button("Start a Career",Action::Career));p.spawn(button("Choose a Practice Lesson",Action::Lessons));p.spawn(button("Keep Experimenting",Action::Free));}
            }else if let Some(stage)=stage {
                p.spawn(label(crate::textbook::expand(&stage.objective,&settings),17.0,TEXT));
                if full||runner.expanded{p.spawn(label(crate::textbook::expand(&stage.explanation,&settings),16.0,TEXT_DIM));}
                if runner.offered&&runner.hint==0{p.spawn(label("Need a hint? Pause > Training help.",14.0,TEXT_DIM));}
                for hint in stage.hints.iter().take(runner.hint){p.spawn(label(crate::textbook::expand(hint,&settings),15.0,TEXT));}
                if full {
                    p.spawn(row()).with_children(|r|{if runner.hint<stage.hints.len(){r.spawn(button(if runner.hint==0{"Show instructions"}else{"More help"},Action::Hint));}r.spawn(button("Show / hide explanation",Action::Details));});
                    p.spawn(button("Lab Textbook",crate::settings::PauseAction::OpenTextbook));
                    if let Some(article)=book.0.iter().find(|a|a.id==stage.article){p.spawn(button(format!("Read: {}",article.title),Action::Topic(article.id.clone())));}
                }else{p.spawn(label("Pause: help, textbook, reset or leave.",13.0,TEXT_DIM));}
            }else{p.spawn(label("Choose a small experiment. Pause offers the textbook, more lessons, and fresh supplies.",16.0,TEXT_DIM));}
            if full {
                p.spawn(row()).with_children(|r|{r.spawn(button("Restart exercise / supplies",Action::Restart));if stage.is_some(){r.spawn(button("Skip exercise",Action::Skip));}});
                p.spawn(row()).with_children(|r|{r.spawn(button("Practice lessons",Action::Lessons));r.spawn(button("Free practice",Action::Free));r.spawn(button("Reset spill bay",Action::Clean));});
                p.spawn(button("Resume",crate::settings::PauseAction::Resume));p.spawn(button("Leave training",Action::Leave));
            }
        });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn keep_experimenting_retains_the_lab_and_resume_bookmark() {
        let mut world = World::new();
        world.insert_resource(State::new(AppState::Playing));
        world.insert_resource(Runner::new("mistake".into()));
        world.insert_resource(Paused(true));
        world.insert_resource(Profile(TrainingProgress {
            version: 1,
            resume: Some("mistake".into()),
            ..default()
        }));
        let item = world.spawn(Container::new(ContainerKind::Beaker)).id();
        world.spawn((Action::Free, Interaction::Pressed));
        clicks(&mut world);
        assert_eq!(world.resource::<Runner>().lesson, "free");
        assert_eq!(
            world.resource::<Profile>().0.resume.as_deref(),
            Some("mistake")
        );
        assert!(world.get::<Container>(item).is_some());
        assert!(!world.resource::<Paused>().0);
        assert!(!world.contains_resource::<Relaunch>());
    }
}
