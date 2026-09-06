//! Authored tuning for needs, breaks, and social actions.
//!
//! The last P5 item. Rates, thresholds, recovery amounts, and room-appeal
//! weights were deliberate placeholders in Rust while the behaviour was being
//! built; this moves them into `assets/data/station.needs.ron` so balancing a
//! station is a text edit and a restart rather than a rebuild.
//!
//! ## Validated at load, not trusted
//!
//! [`NeedsTuning::validate`] runs in the plugin's `build`, so a file that says
//! something impossible fails the game to start. That matters more here than in
//! most authored data: a negative rate or an out-of-range threshold does not
//! crash, it produces a station that behaves strangely for reasons no log line
//! would explain. The checks are deliberately about *coherence* rather than
//! taste — a designer may set any playable numbers they like, but a `starting`
//! need already past its own threshold means the shift opens with everyone
//! walking off the job, which is never what was meant.
//!
//! ## What is deliberately not here
//!
//! Anything the chemistry already decides. `social_disposition` and
//! `fit_for_leisure` read `Bloodstream` statuses directly, and a "sedated
//! multiplier" in this file would be a second mood model free to disagree with
//! the first. Seat capacities and positions stay in `lab.map`, because they are
//! geometry rather than balance.

use bevy::prelude::*;
use serde::Deserialize;

/// Every authored number the needs and social systems read.
#[derive(Resource, Clone, Debug, Deserialize)]
pub struct NeedsTuning {
    pub rates: NeedRates,
    pub starting: StartingNeeds,
    pub thresholds: BreakThresholds,
    pub actions: ActionTuning,
    pub appeal: AppealWeights,
}

/// How fast each pressure builds, per second, on a 0..1 scale.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct NeedRates {
    pub hunger_per_second: f32,
    pub fatigue_per_second: f32,
    pub social_per_second: f32,
}

/// Where each need starts when a shift opens.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct StartingNeeds {
    pub hunger: f32,
    pub fatigue: f32,
    pub social: f32,
}

/// The pressure at which a resident will *consider* the matching break.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct BreakThresholds {
    pub tired_enough_to_rest: f32,
    pub lonely_enough_to_talk: f32,
}

/// How long each break takes and what it is worth.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct ActionTuning {
    pub rest_seconds: f32,
    pub socialize_seconds: f32,
    pub rest_recovery: f32,
    pub social_recovery: f32,
    pub social_standing_gain: i32,
    pub pair_cooldown_seconds: f32,
}

/// Multipliers on a break offer, from what Service currently looks like.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct AppealWeights {
    pub food_bonus: f32,
    pub no_food_penalty: f32,
    pub dirty_penalty: f32,
    pub crowded_penalty: f32,
}

/// Why an authored tuning file was rejected.
#[derive(Debug, PartialEq, Eq)]
pub enum TuningError {
    /// A rate that is zero, negative, or not finite. A need that never builds
    /// means the matching action is dead code the player would never see fire.
    BadRate(&'static str),
    /// A 0..1 value outside 0..1.
    OutOfRange(&'static str),
    /// A duration that is zero or negative — an action that takes no time
    /// completes the frame it begins, skipping the walk, the reservation, and
    /// any chance of being witnessed.
    BadDuration(&'static str),
    /// A need that starts at or above the threshold that triggers its break, so
    /// the shift opens with everyone already leaving their post.
    StartsPastThreshold(&'static str),
    /// Recovery that does not actually relieve anything, which would leave a
    /// resident looping the same action forever.
    NoRecovery(&'static str),
}

fn positive_rate(value: f32, name: &'static str) -> Result<(), TuningError> {
    (value.is_finite() && value > 0.0)
        .then_some(())
        .ok_or(TuningError::BadRate(name))
}

fn unit_range(value: f32, name: &'static str) -> Result<(), TuningError> {
    (value.is_finite() && (0.0..=1.0).contains(&value))
        .then_some(())
        .ok_or(TuningError::OutOfRange(name))
}

fn positive_duration(value: f32, name: &'static str) -> Result<(), TuningError> {
    (value.is_finite() && value > 0.0)
        .then_some(())
        .ok_or(TuningError::BadDuration(name))
}

impl NeedsTuning {
    /// Reads the authored file, or fails loudly.
    ///
    /// Called from the plugin's `build`, so bad tuning is a startup failure
    /// rather than a station that quietly behaves wrongly.
    ///
    /// Parsed once and cached. `NpcNeeds::default` runs on every crew spawn,
    /// and re-reading the file each time would turn a startup cost into a
    /// per-arrival one for no benefit — the file cannot change while the game
    /// is running, since it is embedded with `include_str!`.
    pub fn authored() -> &'static Self {
        static AUTHORED: std::sync::OnceLock<NeedsTuning> = std::sync::OnceLock::new();
        AUTHORED.get_or_init(|| {
            let tuning: NeedsTuning =
                ron::from_str(include_str!("../../assets/data/station.needs.ron"))
                    .expect("station.needs.ron is valid RON");
            tuning
                .validate()
                .expect("station.needs.ron must describe a coherent station");
            tuning
        })
    }

    /// Checks the file describes something a station can actually do.
    ///
    /// Coherence, not taste: any playable numbers are allowed, but a
    /// combination that guarantees nonsense is refused.
    pub fn validate(&self) -> Result<(), TuningError> {
        positive_rate(self.rates.hunger_per_second, "hunger_per_second")?;
        positive_rate(self.rates.fatigue_per_second, "fatigue_per_second")?;
        positive_rate(self.rates.social_per_second, "social_per_second")?;

        unit_range(self.starting.hunger, "starting.hunger")?;
        unit_range(self.starting.fatigue, "starting.fatigue")?;
        unit_range(self.starting.social, "starting.social")?;

        unit_range(self.thresholds.tired_enough_to_rest, "tired_enough_to_rest")?;
        unit_range(
            self.thresholds.lonely_enough_to_talk,
            "lonely_enough_to_talk",
        )?;

        positive_duration(self.actions.rest_seconds, "rest_seconds")?;
        positive_duration(self.actions.socialize_seconds, "socialize_seconds")?;
        positive_duration(self.actions.pair_cooldown_seconds, "pair_cooldown_seconds")?;

        unit_range(self.actions.rest_recovery, "rest_recovery")?;
        unit_range(self.actions.social_recovery, "social_recovery")?;
        if self.actions.rest_recovery <= 0.0 {
            return Err(TuningError::NoRecovery("rest_recovery"));
        }
        if self.actions.social_recovery <= 0.0 {
            return Err(TuningError::NoRecovery("social_recovery"));
        }

        unit_range(self.appeal.food_bonus, "food_bonus")?;
        unit_range(self.appeal.no_food_penalty, "no_food_penalty")?;
        unit_range(self.appeal.dirty_penalty, "dirty_penalty")?;
        unit_range(self.appeal.crowded_penalty, "crowded_penalty")?;

        // The cross-field checks — the ones a per-value range test cannot see.
        if self.starting.fatigue >= self.thresholds.tired_enough_to_rest {
            return Err(TuningError::StartsPastThreshold("fatigue"));
        }
        if self.starting.social >= self.thresholds.lonely_enough_to_talk {
            return Err(TuningError::StartsPastThreshold("social"));
        }
        Ok(())
    }
}

impl Default for NeedsTuning {
    /// The authored file. Deliberately *not* a second set of hardcoded numbers:
    /// a `Default` that differed from the RON would be a silent fallback that
    /// masks a broken file, which is exactly what moving tuning into data is
    /// meant to stop.
    fn default() -> Self {
        Self::authored().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shipped file must be valid. This is the test that makes the whole
    /// arrangement safe to edit: a designer who breaks the numbers finds out
    /// here rather than from a station that behaves oddly.
    #[test]
    fn the_authored_tuning_is_coherent() {
        let tuning = NeedsTuning::authored().clone();
        assert_eq!(tuning.validate(), Ok(()));
    }

    /// Rates must actually build. A need frozen at its starting value makes the
    /// matching action unreachable, and nothing else in the game would say so.
    #[test]
    fn a_need_that_never_builds_is_rejected() {
        let mut tuning = NeedsTuning::authored().clone();
        tuning.rates.hunger_per_second = 0.0;
        assert_eq!(
            tuning.validate(),
            Err(TuningError::BadRate("hunger_per_second"))
        );

        tuning = NeedsTuning::authored().clone();
        tuning.rates.social_per_second = -0.1;
        assert_eq!(
            tuning.validate(),
            Err(TuningError::BadRate("social_per_second"))
        );
    }

    /// The cross-field check a range test cannot make: opening the shift with
    /// everyone already past their break threshold.
    #[test]
    fn a_shift_cannot_open_with_everyone_already_leaving_their_post() {
        let mut tuning = NeedsTuning::authored().clone();
        tuning.starting.fatigue = tuning.thresholds.tired_enough_to_rest;
        assert_eq!(
            tuning.validate(),
            Err(TuningError::StartsPastThreshold("fatigue"))
        );

        tuning = NeedsTuning::authored().clone();
        tuning.starting.social = 0.99;
        tuning.thresholds.lonely_enough_to_talk = 0.5;
        assert_eq!(
            tuning.validate(),
            Err(TuningError::StartsPastThreshold("social"))
        );
    }

    /// An action that takes no time completes the frame it begins, skipping the
    /// walk, the reservation, and any chance of being witnessed.
    #[test]
    fn an_instant_action_is_rejected() {
        let mut tuning = NeedsTuning::authored().clone();
        tuning.actions.rest_seconds = 0.0;
        assert_eq!(
            tuning.validate(),
            Err(TuningError::BadDuration("rest_seconds"))
        );
    }

    /// Recovery that relieves nothing would leave a resident repeating the same
    /// action forever, which reads as a stuck NPC rather than a tuning choice.
    #[test]
    fn an_action_that_relieves_nothing_is_rejected() {
        let mut tuning = NeedsTuning::authored().clone();
        tuning.actions.social_recovery = 0.0;
        assert_eq!(
            tuning.validate(),
            Err(TuningError::NoRecovery("social_recovery"))
        );
    }

    /// Appeal weights are multipliers on 0..1, not free-floating scores.
    #[test]
    fn an_out_of_range_appeal_weight_is_rejected() {
        let mut tuning = NeedsTuning::authored().clone();
        tuning.appeal.food_bonus = 1.5;
        assert_eq!(
            tuning.validate(),
            Err(TuningError::OutOfRange("food_bonus"))
        );
    }

    /// `Default` must be the authored file, not a second set of numbers that
    /// could silently mask a broken one.
    #[test]
    fn the_default_is_the_authored_file() {
        let authored = NeedsTuning::authored();
        let defaulted = NeedsTuning::default();
        assert_eq!(
            authored.rates.hunger_per_second,
            defaulted.rates.hunger_per_second
        );
        assert_eq!(
            authored.thresholds.tired_enough_to_rest,
            defaulted.thresholds.tired_enough_to_rest
        );
        assert_eq!(authored.appeal.food_bonus, defaulted.appeal.food_bonus);
    }
}
