//! The screen that says the arc is over.
//!
//! Winning used to produce exactly one line of radio chatter and then business
//! as usual — the whole hidden-antagonist spine landing on a payoff a player
//! could look away and miss. This module watches for the moment
//! [`Campaign::outcome`] is written and puts the run in front of them: who it
//! was, how it went, what the career came to, and whether it unlocked
//! anything.
//!
//! **Client-side, on every peer.** The campaign already reaches guests through
//! `arc::CampaignSync`, so this is deliberately *not* authority-gated: both
//! chemists watched the same arc and both get told how it ended.
//!
//! The screen itself is a [`crate::settings::PauseScreen`] variant rather than
//! machinery of its own. Everything an ending screen needs — a freed cursor,
//! input stopped, the clock held in singleplayer and running in co-op, and
//! teardown on the way out of the lab — is exactly what pausing already does,
//! and a second copy of all of it is a second copy to keep correct.

use bevy::prelude::*;

use crate::arc::{ArcOutcome, Campaign, Mode, ThwartedAntags};
use crate::chem_data::ChemDb;
use crate::knowledge::Knowledge;
use crate::menu::choice;
use crate::orders::Shift;
use crate::settings::{PauseAction, PauseScreen, Paused};
use crate::ui::{arc_headline, label, TEXT, TEXT_DIM};
use crate::AppState;

pub struct EndingPlugin;

impl Plugin for EndingPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<FinishedArc>().add_systems(
            Update,
            (notice_the_ending, watch_for_crew_collapse)
                .chain()
                .run_if(in_state(AppState::Playing)),
        );
    }
}

/// The finished arc, and whether this peer has been told about it.
#[derive(Resource, Default)]
pub struct FinishedArc {
    /// What the screen draws. Kept after the screen is dismissed — "Keep
    /// playing" hides the ending, it does not un-end the arc.
    showing: Option<Ending>,
    /// What the last frame saw the outcome to be.
    ///
    /// `None` on the **outer** option means this has not looked yet, which is
    /// deliberately not the same as having looked and found an unresolved arc.
    /// Without that distinction, loading a save whose campaign already resolved
    /// would replay its ending on the first frame — and a finished save stays
    /// playable in this game, so that is a real thing players will do.
    watched: Option<Option<ArcOutcome>>,
}

/// Everything the ending screen says, flattened.
///
/// Built once, when the arc resolves, rather than read live: the career keeps
/// moving behind the screen (the other chemist can still be working, and the
/// player themselves can dismiss it and carry on), and an ending that quietly
/// rewrote its own numbers afterwards would be reporting a different run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ending {
    /// `None` only for a crew-collapse evacuation
    /// (`watch_for_crew_collapse`) — that loss path is not an `ArcOutcome`
    /// at all, and can fire on a `Campaign` that is still fully live.
    outcome: Option<ArcOutcome>,
    /// What to call them. Always available here: `ui::arc_headline` names the
    /// antagonist unconditionally once the arc has an outcome, whatever the
    /// reveal tier reached — there is nothing left to spoil.
    name: String,
    won: bool,
    mode: Mode,
    countered: usize,
    counter_steps: usize,
    shifts: u32,
    delivered: u32,
    botched: u32,
    recipes: usize,
    total_recipes: usize,
    /// This run is what unlocked playing as this antagonist. Mirrors
    /// `shift::record_thwarting`'s rule — a chemist run, won — plus "and it was
    /// not already unlocked", which is what makes it worth saying out loud.
    unlocked: bool,
    /// Whether this ending is a real evacuation — a save that stops being
    /// resumable, not merely a resolved arc a player can dismiss and keep
    /// playing past. Set for a Chemist-mode `ArcOutcome::PlotSucceeded` loss,
    /// or for a crew-collapse ending (`outcome: None`) — never for a win of
    /// either kind. `ending::draw` omits "Keep playing" when this is set, and
    /// `Shift::evacuated` (persisted) is what makes the save itself unloadable
    /// afterward.
    pub(crate) evacuated: bool,
}

impl Ending {
    /// Whether the run this screen is reporting was a win *for the player*.
    ///
    /// Already baked in at construction from `Campaign::player_won`, so this
    /// is the settled answer rather than one re-derived from an [`ArcOutcome`]
    /// that means opposite things from the two chairs. `crate::audio` reads it
    /// to decide whether the shuttle is being called.
    pub(crate) fn won(&self) -> bool {
        self.won
    }

    /// The banner. Deliberately different per side: the same [`ArcOutcome`]
    /// is a victory from one chair and a defeat from the other, which is the
    /// one place the two modes actually diverge.
    pub fn headline(&self) -> String {
        let Some(outcome) = self.outcome else {
            return "EVACUATION".to_string();
        };
        match (self.mode, outcome) {
            (Mode::Chemist, ArcOutcome::StoppedDirectly) => "YOU STOPPED THEM".to_string(),
            (Mode::Chemist, ArcOutcome::StoppedByDepartments) => "THEY WERE STOPPED".to_string(),
            (Mode::Chemist, ArcOutcome::PlotSucceeded) => "THEY GOT WHAT THEY WANTED".to_string(),
            (Mode::Antagonist, ArcOutcome::PlotSucceeded) => "IT WORKED".to_string(),
            (Mode::Antagonist, _) => "YOU WERE STOPPED".to_string(),
        }
    }

    /// The line under the banner: what actually happened, in one sentence.
    pub fn blurb(&self) -> String {
        let Some(outcome) = self.outcome else {
            return "The crew could not hold together, whatever else was happening. \
                    Command has called the shuttle."
                .to_string();
        };
        match (self.mode, outcome) {
            (Mode::Chemist, ArcOutcome::StoppedDirectly) => format!(
                "{} came for the lab and the lab held. Command sends thanks.",
                self.name
            ),
            (Mode::Chemist, ArcOutcome::StoppedByDepartments) => format!(
                "Every countermeasure went out of your window. {} never got to move.",
                self.name
            ),
            (Mode::Chemist, ArcOutcome::PlotSucceeded) => format!(
                "{} got everything they needed. The station is reeling, but Chemistry is still operating.",
                self.name
            ),
            (Mode::Antagonist, ArcOutcome::PlotSucceeded) => format!(
                "{} got everything they needed, and you are the reason.",
                self.name
            ),
            (Mode::Antagonist, ArcOutcome::StoppedDirectly) => {
                "It came to the lab, and the lab held. Nothing you mixed was enough.".to_string()
            }
            (Mode::Antagonist, ArcOutcome::StoppedByDepartments) => {
                "The departments worked it out between them. There was nothing left to run."
                    .to_string()
            }
        }
    }
}

/// Raises the ending the frame the arc resolves.
#[allow(clippy::too_many_arguments)]
fn notice_the_ending(
    campaign: Option<Res<Campaign>>,
    script: Option<Res<crate::arc::Script>>,
    db: Option<Res<ChemDb>>,
    knowledge: Option<Res<Knowledge>>,
    shift: Res<Shift>,
    thwarted: Res<ThwartedAntags>,
    mut finished: ResMut<FinishedArc>,
    mut paused: ResMut<Paused>,
    mut screen: ResMut<PauseScreen>,
) {
    let outcome = campaign.as_deref().and_then(|campaign| campaign.outcome);
    // `replace` both records this frame and hands back the last one, so there
    // is no path that reads the old value without writing the new.
    let Some(previous) = finished.watched.replace(outcome) else {
        // The first look. Whatever it found is the baseline, resolved or not.
        return;
    };
    if previous.is_some() || outcome.is_none() {
        return;
    }
    // A crew-collapse evacuation (`watch_for_crew_collapse`) may already have
    // raised the screen this session, on a `Campaign` that only resolved
    // afterward — once any ending is up, nothing here may overwrite it.
    if finished.showing.is_some() {
        return;
    }

    let (Some(campaign), Some(db), Some(knowledge)) = (campaign, db, knowledge) else {
        // An outcome with no campaign is impossible, and the two assets are
        // loaded long before an arc can resolve — but a missing one is a
        // reason to skip the screen, not to panic in front of the player.
        return;
    };
    // The one rule about what may be named, borrowed rather than re-derived.
    let Some(headline) = arc_headline(&campaign, script.as_deref().map(|script| &script.0)) else {
        return;
    };

    let resolved_outcome = campaign.outcome.expect("checked above");
    // Arc outcomes are incident reports now. Only station stability reaching
    // zero is allowed to make a career unloadable.
    let evacuated = false;

    finished.showing = Some(Ending {
        outcome: Some(resolved_outcome),
        name: headline
            .name
            // `arc_headline` names the antagonist for any resolved arc, so this
            // is unreachable; falling back to the short menu label beats an
            // empty sentence if it ever is not.
            .unwrap_or_else(|| campaign.antag.label().to_string()),
        won: campaign.player_won().unwrap_or(false),
        mode: campaign.mode,
        countered: headline.countered,
        counter_steps: headline.total,
        shifts: shift.shift_number,
        delivered: shift.succeeded,
        botched: shift.botched,
        recipes: knowledge.known_count(),
        total_recipes: db.reactions.len(),
        unlocked: campaign.mode == Mode::Chemist
            && campaign.player_won() == Some(true)
            && !thwarted.0.contains(&campaign.antag),
        evacuated,
    });

    paused.0 = true;
    *screen = PauseScreen::Ending;
}

/// Raises the chemist-run ending the moment station stability reaches zero.
/// Arc reports never evacuate the station; this is the authoritative loss
/// path and therefore takes precedence over a report already on screen.
fn watch_for_crew_collapse(
    instability: Option<Res<crate::instability::Instability>>,
    campaign: Option<Res<Campaign>>,
    db: Option<Res<ChemDb>>,
    knowledge: Option<Res<Knowledge>>,
    mut shift: ResMut<Shift>,
    mut finished: ResMut<FinishedArc>,
    mut paused: ResMut<Paused>,
    mut screen: ResMut<PauseScreen>,
) {
    let Some(instability) = instability else {
        return;
    };
    if campaign
        .as_deref()
        .is_some_and(|campaign| campaign.mode == Mode::Antagonist)
    {
        return;
    }
    if instability.band != crate::instability::StabilityBand::Evacuating {
        return;
    }
    // Evacuation has precedence over a dismissible arc report, including when
    // an arc's climax penalty crosses zero one frame after that report opens.
    if finished
        .showing
        .as_ref()
        .is_some_and(|ending| ending.evacuated)
    {
        return;
    }
    let (Some(db), Some(knowledge)) = (db, knowledge) else {
        return;
    };

    shift.evacuated = true;
    finished.showing = Some(Ending {
        outcome: None,
        name: campaign
            .as_deref()
            .map(|c| c.antag.label().to_string())
            .unwrap_or_else(|| "whoever was really behind it".to_string()),
        won: false,
        mode: campaign.as_deref().map(|c| c.mode).unwrap_or_default(),
        countered: 0,
        counter_steps: 0,
        shifts: shift.shift_number,
        delivered: shift.succeeded,
        botched: shift.botched,
        recipes: knowledge.known_count(),
        total_recipes: db.reactions.len(),
        unlocked: false,
        evacuated: true,
    });

    paused.0 = true;
    *screen = PauseScreen::Ending;
}

/// Draws the ending over the lab.
///
/// Called by `settings::sync_pause_overlay` rather than by a system here, so
/// there is exactly one thing on screen deciding what the overlay is — two
/// systems both spawning full-screen panels is how you end up with a dead menu
/// underneath a live one.
pub(crate) fn draw(commands: &mut Commands, ending: &Ending, root: impl Bundle) {
    let headline = ending.headline();
    let subtitle = match ending.mode {
        Mode::Chemist => "Chemistry, station shift log — final entry.",
        Mode::Antagonist => "Chemistry, station shift log — recovered.",
    };
    crate::menu::menu_shell(commands, root, &headline, subtitle, |panel| {
        // Tinted rather than left in the shell's dim subtitle slot, because
        // which way it went is the one thing on this screen that has to be
        // readable from across the room.
        panel.spawn(label(
            ending.blurb(),
            15.0,
            if ending.won {
                crate::ui::GOOD_TEXT
            } else {
                crate::ui::ERROR_TEXT
            },
        ));

        // The counter track only ever existed if the antagonist had one, and
        // saying "0 of 0 countermeasures" is worse than saying nothing.
        if ending.counter_steps > 0 {
            panel.spawn(label(
                format!(
                    "Countermeasures delivered  {} of {}",
                    ending.countered, ending.counter_steps
                ),
                14.0,
                TEXT,
            ));
        }
        panel.spawn(label(
            format!(
                "Shifts worked  {}   ·   delivered {}   ·   botched {}",
                ending.shifts, ending.delivered, ending.botched
            ),
            14.0,
            TEXT,
        ));
        panel.spawn(label(
            format!(
                "Recipes recorded  {} of {}",
                ending.recipes, ending.total_recipes
            ),
            14.0,
            TEXT,
        ));

        if ending.unlocked {
            panel.spawn(label(
                format!(
                    "Unlocked: you can start a new save as {} from the campaign screen.",
                    ending.name
                ),
                14.0,
                crate::ui::GOOD_TEXT,
            ));
        }
        panel.spawn(label("", 8.0, TEXT_DIM));

        // A real evacuation is not something to dismiss and carry on past —
        // see `Ending::evacuated`'s own doc for exactly which endings these are.
        if !ending.evacuated {
            panel.spawn(choice(
                "Keep playing",
                "The save stays open. The counter is still there, and so is the career.",
                PauseAction::Resume,
            ));
        }
        panel.spawn(choice(
            "Main menu",
            "Start a new career against somebody else, or load another save.",
            PauseAction::QuitToMenu,
        ));
        panel.spawn(choice(
            "Quit to desktop",
            "Everything is already saved.",
            PauseAction::QuitToDesktop,
        ));
    });
}

impl FinishedArc {
    /// What the overlay should draw, if anything.
    pub(crate) fn showing(&self) -> Option<&Ending> {
        self.showing.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arc::{AntagId, Reveal};
    use crate::threat;
    use bevy::state::app::StatesPlugin;

    fn chemistry() -> chem_sim::ChemData {
        chem_sim::ChemData::from_ron(
            include_str!("../../assets/data/chem.reagents.ron"),
            include_str!("../../assets/data/chem.reactions.ron"),
        )
        .unwrap()
    }

    /// Just enough world to run [`notice_the_ending`]: no renderer, no arc
    /// systems, no crew — the campaign is written by hand.
    fn ending_app(campaign: Campaign) -> App {
        let mut app = App::new();
        app.add_plugins((MinimalPlugins, StatesPlugin))
            .init_state::<AppState>()
            .add_plugins(EndingPlugin)
            .init_resource::<Paused>()
            .init_resource::<PauseScreen>()
            .init_resource::<Shift>()
            .init_resource::<ThwartedAntags>()
            .insert_resource(Knowledge::new(&chemistry()))
            .insert_resource(ChemDb(chemistry()))
            .insert_resource(threat::Authored::<crate::arc::ArcScript>(
                ron::from_str(include_str!("../../assets/data/station.arc.ron")).unwrap(),
            ))
            .insert_resource(campaign);
        app.world_mut()
            .resource_mut::<NextState<AppState>>()
            .set(AppState::Playing);
        app.update();
        app
    }

    fn live_campaign() -> Campaign {
        let mut campaign = Campaign::new(AntagId::Cult, Mode::Chemist, 3);
        campaign.reveal = Reveal::Named;
        campaign
    }

    fn is_up(app: &App) -> bool {
        app.world().resource::<Paused>().0
            && *app.world().resource::<PauseScreen>() == PauseScreen::Ending
    }

    #[test]
    fn an_arc_that_resolves_while_you_play_puts_the_ending_up() {
        let mut app = ending_app(live_campaign());
        assert!(!is_up(&app), "nothing has ended yet");

        app.world_mut().resource_mut::<Campaign>().outcome = Some(ArcOutcome::StoppedDirectly);
        app.update();

        assert!(is_up(&app));
        let finished = app.world().resource::<FinishedArc>();
        let shown = finished.showing().expect("the screen has content");
        assert_eq!(shown.outcome, Some(ArcOutcome::StoppedDirectly));
        assert!(shown.won);
        assert!(
            shown.headline().contains("STOPPED"),
            "got {:?}",
            shown.headline()
        );
    }

    #[test]
    fn loading_a_finished_save_does_not_replay_its_ending() {
        // The load-bearing one. A resolved save stays playable in this game,
        // so opening one is an ordinary thing to do — and being handed the
        // ending again every time you did it would make finishing an arc a
        // reason not to open the save.
        let mut campaign = live_campaign();
        campaign.outcome = Some(ArcOutcome::PlotSucceeded);
        let mut app = ending_app(campaign);

        app.update();
        app.update();

        assert!(!is_up(&app), "the arc ended before this session started");
        assert!(app.world().resource::<FinishedArc>().showing().is_none());
    }

    #[test]
    fn keeping_playing_does_not_bring_the_ending_back() {
        let mut app = ending_app(live_campaign());
        app.world_mut().resource_mut::<Campaign>().outcome = Some(ArcOutcome::StoppedByDepartments);
        app.update();
        assert!(is_up(&app));

        // What "Keep playing" and Escape both do.
        app.world_mut().resource_mut::<Paused>().0 = false;
        *app.world_mut().resource_mut::<PauseScreen>() = PauseScreen::Root;
        app.update();
        app.update();

        assert!(
            !is_up(&app),
            "it is raised once per arc, not once per frame"
        );
    }

    #[test]
    fn beating_an_antagonist_for_the_first_time_says_so() {
        let mut app = ending_app(live_campaign());
        app.world_mut().resource_mut::<Campaign>().outcome = Some(ArcOutcome::StoppedDirectly);
        app.update();
        assert!(
            app.world()
                .resource::<FinishedArc>()
                .showing()
                .unwrap()
                .unlocked,
            "this run is what opened the antagonist side up"
        );

        // ...and does not claim credit for one that was already unlocked, which
        // is the same rule `shift::record_thwarting` writes the file under.
        let mut app = ending_app(live_campaign());
        app.world_mut()
            .insert_resource(ThwartedAntags(vec![AntagId::Cult]));
        app.world_mut().resource_mut::<Campaign>().outcome = Some(ArcOutcome::StoppedDirectly);
        app.update();
        assert!(
            !app.world()
                .resource::<FinishedArc>()
                .showing()
                .unwrap()
                .unlocked
        );
    }

    // -- evacuation ----------------------------------------------------

    #[test]
    fn a_chemist_plot_loss_is_an_incident_report_not_an_evacuation() {
        let mut app = ending_app(live_campaign());
        app.world_mut().resource_mut::<Campaign>().outcome = Some(ArcOutcome::PlotSucceeded);
        app.update();

        let shown = app.world().resource::<FinishedArc>().showing().unwrap();
        assert!(!shown.evacuated, "only zero station stability may evacuate");
        assert!(!app.world().resource::<Shift>().evacuated);
    }

    #[test]
    fn a_chemist_win_never_evacuates_even_at_the_meters_own_ceiling() {
        // Guards against a false positive keyed off the numeric threshold
        // alone rather than the actual outcome variant.
        for outcome in [
            ArcOutcome::StoppedDirectly,
            ArcOutcome::StoppedByDepartments,
        ] {
            let mut app = ending_app(live_campaign());
            app.world_mut().resource_mut::<Campaign>().outcome = Some(outcome);
            app.update();

            let shown = app.world().resource::<FinishedArc>().showing().unwrap();
            assert!(!shown.evacuated, "{outcome:?} is a win, not an evacuation");
            assert!(!app.world().resource::<Shift>().evacuated);
        }
    }

    #[test]
    fn an_antagonist_runs_plot_succeeding_never_evacuates() {
        // From the antagonist's own chair, `PlotSucceeded` is the player's
        // win — nothing about the station gets evacuated over it.
        let mut campaign = live_campaign();
        campaign.mode = Mode::Antagonist;
        let mut app = ending_app(campaign);
        app.world_mut().resource_mut::<Campaign>().outcome = Some(ArcOutcome::PlotSucceeded);
        app.update();

        let shown = app.world().resource::<FinishedArc>().showing().unwrap();
        assert!(!shown.evacuated);
        assert!(!app.world().resource::<Shift>().evacuated);
    }

    #[test]
    fn the_crew_instability_meter_reaching_breaking_evacuates_independently_of_the_arc() {
        // The second loss path — fires on a `Campaign` that is still fully
        // live, with no `outcome` of its own at all.
        let mut app = ending_app(live_campaign());
        app.insert_resource(crate::instability::Instability {
            value: 0.0,
            band: crate::instability::StabilityBand::Evacuating,
            ..default()
        });
        app.update();

        assert!(is_up(&app));
        let shown = app.world().resource::<FinishedArc>().showing().unwrap();
        assert!(shown.evacuated);
        assert_eq!(shown.outcome, None);
        assert_eq!(shown.headline(), "EVACUATION");
        assert!(app.world().resource::<Shift>().evacuated);
        assert!(
            app.world().resource::<Campaign>().outcome.is_none(),
            "the arc itself is untouched — this loss path does not resolve it"
        );
    }

    #[test]
    fn a_crew_collapse_ending_never_gets_overwritten_by_the_arc_resolving_afterward() {
        let mut app = ending_app(live_campaign());
        app.insert_resource(crate::instability::Instability {
            value: 0.0,
            band: crate::instability::StabilityBand::Evacuating,
            ..default()
        });
        app.update();
        let first = app.world().resource::<FinishedArc>().showing().cloned();

        // The arc resolves in a later frame — must not clobber the screen
        // already up.
        app.world_mut().resource_mut::<Campaign>().outcome = Some(ArcOutcome::StoppedDirectly);
        app.update();

        assert_eq!(
            app.world().resource::<FinishedArc>().showing().cloned(),
            first,
            "once any ending is showing, nothing here may overwrite it"
        );
    }

    #[test]
    fn losing_an_antagonist_run_never_unlocks_anything() {
        // Winning an antagonist run means the antagonist got what they wanted,
        // which is the opposite of having stopped them.
        let mut campaign = live_campaign();
        campaign.mode = Mode::Antagonist;
        let mut app = ending_app(campaign);
        app.world_mut().resource_mut::<Campaign>().outcome = Some(ArcOutcome::PlotSucceeded);
        app.update();

        let shown = app.world().resource::<FinishedArc>().showing().unwrap();
        assert!(shown.won, "the antagonist got what they came for");
        assert!(!shown.unlocked, "only a chemist run unlocks");
    }

    fn ending(mode: Mode, outcome: ArcOutcome) -> Ending {
        let mut campaign = Campaign::new(AntagId::Cult, mode, 0);
        campaign.outcome = Some(outcome);
        Ending {
            outcome: Some(outcome),
            name: "the Cult".to_string(),
            won: campaign.player_won().unwrap(),
            mode,
            countered: 0,
            counter_steps: 3,
            shifts: 4,
            delivered: 30,
            botched: 2,
            recipes: 9,
            total_recipes: 41,
            unlocked: false,
            evacuated: false,
        }
    }

    #[test]
    fn the_same_outcome_reads_opposite_ways_from_the_two_chairs() {
        // The one place the modes diverge, and the whole reason the ending
        // screen cannot just print the `ArcOutcome`.
        let stopped = ArcOutcome::StoppedDirectly;
        assert_ne!(
            ending(Mode::Chemist, stopped).headline(),
            ending(Mode::Antagonist, stopped).headline()
        );

        let succeeded = ArcOutcome::PlotSucceeded;
        assert_ne!(
            ending(Mode::Chemist, succeeded).headline(),
            ending(Mode::Antagonist, succeeded).headline()
        );

        // And each side's win is the other side's loss, so the winning
        // headline for one outcome must be the losing one for the other.
        assert!(ending(Mode::Chemist, stopped).won);
        assert!(!ending(Mode::Antagonist, stopped).won);
        assert!(!ending(Mode::Chemist, succeeded).won);
        assert!(ending(Mode::Antagonist, succeeded).won);
    }

    #[test]
    fn escape_is_blocked_only_on_the_ending_screen_and_only_when_evacuated() {
        use crate::interaction::escape_blocked_by_evacuation;
        use crate::settings::PauseScreen;

        let mut evacuated_ending = ending(Mode::Chemist, ArcOutcome::PlotSucceeded);
        evacuated_ending.evacuated = true;
        let evacuated = FinishedArc {
            showing: Some(evacuated_ending),
            watched: None,
        };
        let ordinary = FinishedArc {
            showing: Some(ending(Mode::Chemist, ArcOutcome::StoppedDirectly)),
            watched: None,
        };
        let nothing_showing = FinishedArc::default();

        assert!(
            escape_blocked_by_evacuation(PauseScreen::Ending, Some(&evacuated)),
            "an evacuated ending must block Escape"
        );
        assert!(
            !escape_blocked_by_evacuation(PauseScreen::Ending, Some(&ordinary)),
            "an ordinary, merely-resolved ending must keep dismissing normally"
        );
        assert!(
            !escape_blocked_by_evacuation(PauseScreen::Root, Some(&evacuated)),
            "the block only ever applies on the Ending screen itself"
        );
        assert!(!escape_blocked_by_evacuation(
            PauseScreen::Ending,
            Some(&nothing_showing)
        ));
        assert!(!escape_blocked_by_evacuation(PauseScreen::Ending, None));
    }

    #[test]
    fn every_outcome_names_the_antagonist_or_says_why_not() {
        // A blurb that forgot the name would leave the payoff of the entire
        // hidden-antagonist spine as "it is over".
        for mode in [Mode::Chemist, Mode::Antagonist] {
            for outcome in [
                ArcOutcome::PlotSucceeded,
                ArcOutcome::StoppedDirectly,
                ArcOutcome::StoppedByDepartments,
            ] {
                let ending = ending(mode, outcome);
                assert!(!ending.headline().is_empty());
                assert!(!ending.blurb().is_empty());
            }
        }
    }
}
