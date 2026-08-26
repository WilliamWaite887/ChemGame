//! What a reagent does to a body.
//!
//! Reactions have always had [`crate::ReactionEffect`] — what happens in the
//! beaker. This is the other half: what happens in the person. The two are
//! deliberately separate enums. A reaction heats its container; a reagent
//! poisons its host. Nothing does both.

use serde::{Deserialize, Serialize};

use crate::units::Units;

/// The four ways a body can be hurt.
///
/// Every medicine in the game already maps onto exactly one of these — that is
/// what the `treats` line in `chem.reagents.ron` has been describing all along.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum DamageKind {
    /// Physical trauma. Bicaridine.
    Brute,
    /// Heat and energy. Kelotane, dermaline.
    Burn,
    /// Poison. Dylovene.
    Toxin,
    /// Suffocation and blood loss. Dexalin.
    Oxygen,
}

impl DamageKind {
    pub const ALL: [DamageKind; 4] = [
        DamageKind::Brute,
        DamageKind::Burn,
        DamageKind::Toxin,
        DamageKind::Oxygen,
    ];

    pub fn label(self) -> &'static str {
        match self {
            DamageKind::Brute => "BRUTE",
            DamageKind::Burn => "BURN",
            DamageKind::Toxin => "TOX",
            DamageKind::Oxygen => "OXY",
        }
    }
}

/// Something a body is feeling that is not damage.
///
/// Duration and intensity are tracked separately: a reagent tops the timer up
/// while it is present, and intensity is what the presentation layer scales
/// sway, blur and walking speed by. That split is what lets two doses of the
/// same thing last longer without hitting harder.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub enum StatusKind {
    /// Slower on your feet.
    Sluggish,
    /// Faster.
    Hastened,
    /// Vision.
    Blurred,
    /// The room will not hold still.
    Unsteady,
    /// Sway, blur and a slur at once. Water works it off.
    Drunk,
    /// Deals toxin damage every tick. Hyronalin and arithrazine are the only
    /// counter, which is the entire reason those two exist.
    Irradiated,
    /// Raises the damage threshold at which a body collapses and softens
    /// incoming oxygen damage. Inaprovaline's defining effect.
    Stabilized,
    /// Chemical drowsiness. Strong intensities incapacitate without having to
    /// inflict enough damage to knock the patient down.
    Sedated,
    /// A conspicuously good mood. NPCs can expose it through relaxed or
    /// socially uninhibited behaviour rather than treating it as generic
    /// drunkenness.
    Euphoric,
    /// False sights and sounds. Presentation layers should add sensory decoys,
    /// never reverse or randomly discard player input.
    Hallucinating,
    /// A flight response. Crew AI can flee while players receive threat cues.
    Paranoid,
    /// Pain is masked rather than healed. It raises the collapse threshold,
    /// but the underlying damage remains when the status wears off.
    Analgesic,
    /// Ongoing fire damage after contact with an incendiary.
    Burning,
    /// Cold-impaired movement and motor control.
    Chilled,
    /// Reduces new irradiation while active.
    RadiationShield,
    /// Ongoing oxygen damage and impaired movement.
    Choking,
    /// Unstable mutation: toxin damage plus a visible body-change cue.
    Mutating,
    /// Clear perception and steadier motor control.
    Focused,
    /// A sustained positive mood. Crew linger socially while it is active.
    Happiness,
    /// A sustained negative mood. Strong intensity makes crew withdraw from
    /// visits rather than behaving like generic sedation or paranoia.
    Sadness,
    /// The body's outline is chemically suppressed. Presentation fades the
    /// wearer and hostile observers use the aggregate concealment value when
    /// deciding whether someone close enough has actually been noticed.
    Obscured,
    /// Speech and radio reporting are chemically suppressed without taking
    /// movement or interaction control away from the affected player.
    Muted,
    /// Prevents hostile NPC pursuit and attacks without sedating, slowing, or
    /// taking ordinary movement/control away from the affected character.
    Pacified,
}

impl StatusKind {
    /// Ordered wire schema for the enum's postcard representation.
    ///
    /// The network compatibility fingerprint includes this literal. Keep it
    /// in the same append-only order as [`Self::ALL`] so a status addition or
    /// reorder cannot silently connect peers that disagree about enum tags.
    pub const NETWORK_SCHEMA: &'static str = "Sluggish|Hastened|Blurred|Unsteady|Drunk|Irradiated|Stabilized|Sedated|Euphoric|Hallucinating|Paranoid|Analgesic|Burning|Chilled|RadiationShield|Choking|Mutating|Focused|Happiness|Sadness|Obscured|Muted|Pacified";

    /// Stable iteration order for audits, replication registration and UI.
    /// New variants are appended so existing binary enum discriminants remain
    /// stable for old statuses.
    pub const ALL: [StatusKind; 23] = [
        StatusKind::Sluggish,
        StatusKind::Hastened,
        StatusKind::Blurred,
        StatusKind::Unsteady,
        StatusKind::Drunk,
        StatusKind::Irradiated,
        StatusKind::Stabilized,
        StatusKind::Sedated,
        StatusKind::Euphoric,
        StatusKind::Hallucinating,
        StatusKind::Paranoid,
        StatusKind::Analgesic,
        StatusKind::Burning,
        StatusKind::Chilled,
        StatusKind::RadiationShield,
        StatusKind::Choking,
        StatusKind::Mutating,
        StatusKind::Focused,
        StatusKind::Happiness,
        StatusKind::Sadness,
        StatusKind::Obscured,
        StatusKind::Muted,
        StatusKind::Pacified,
    ];

    /// Damage this status deals per metabolism tick at the given intensity.
    ///
    /// Radiation, fire, choking and mutation harm on their own. Other statuses
    /// are *felt* rather than fatal: being unsteady never killed anyone, it
    /// just made them spill the beaker.
    pub fn tick_damage(self, intensity: f32) -> Damage {
        match self {
            StatusKind::Irradiated => Damage::of(
                DamageKind::Toxin,
                Units::from_f64((intensity as f64).max(0.0)),
            ),
            StatusKind::Burning => Damage::of(
                DamageKind::Burn,
                Units::from_f64((intensity as f64).max(0.0)),
            ),
            StatusKind::Choking => Damage::of(
                DamageKind::Oxygen,
                Units::from_f64((intensity as f64).max(0.0)),
            ),
            StatusKind::Mutating => Damage::of(
                DamageKind::Toxin,
                Units::from_f64(((intensity * 0.5) as f64).max(0.0)),
            ),
            _ => Damage::default(),
        }
    }

    /// Seconds of duration shed per second once nothing is topping it up.
    ///
    /// Drunkenness outlasts everything else by design — it is the one status
    /// you are expected to have to wait out rather than treat.
    pub fn decay(self) -> f32 {
        match self {
            StatusKind::Drunk => 0.5,
            StatusKind::Irradiated => 0.75,
            StatusKind::Burning => 0.6,
            StatusKind::Chilled => 0.7,
            StatusKind::Mutating => 0.5,
            _ => 1.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            StatusKind::Sluggish => "Sluggish",
            StatusKind::Hastened => "Hastened",
            StatusKind::Blurred => "Blurred",
            StatusKind::Unsteady => "Unsteady",
            StatusKind::Drunk => "Drunk",
            StatusKind::Irradiated => "Irradiated",
            StatusKind::Stabilized => "Stabilized",
            StatusKind::Sedated => "Sedated",
            StatusKind::Euphoric => "Euphoric",
            StatusKind::Hallucinating => "Hallucinating",
            StatusKind::Paranoid => "Paranoid",
            StatusKind::Analgesic => "Analgesic",
            StatusKind::Burning => "Burning",
            StatusKind::Chilled => "Chilled",
            StatusKind::RadiationShield => "Radiation shield",
            StatusKind::Choking => "Choking",
            StatusKind::Mutating => "Mutating",
            StatusKind::Focused => "Focused",
            StatusKind::Happiness => "Happiness",
            StatusKind::Sadness => "Sadness",
            StatusKind::Obscured => "Obscured",
            StatusKind::Muted => "Muted",
            StatusKind::Pacified => "Pacified",
        }
    }

    /// Multiplicative contribution to walking speed at this intensity.
    /// Presentation/gameplay code can use the aggregate exposed by
    /// [`crate::Bloodstream::movement_multiplier`].
    pub fn movement_multiplier(self, intensity: f32) -> f32 {
        let intensity = intensity.max(0.0);
        match self {
            StatusKind::Sluggish => (1.0 - 0.20 * intensity).max(0.35),
            StatusKind::Hastened => (1.0 + 0.20 * intensity).min(1.65),
            StatusKind::Unsteady => (1.0 - 0.05 * intensity).max(0.75),
            StatusKind::Drunk => (1.0 - 0.08 * intensity).max(0.65),
            StatusKind::Sedated => (1.0 - 0.30 * intensity).max(0.0),
            StatusKind::Paranoid => (1.0 + 0.08 * intensity).min(1.35),
            StatusKind::Chilled => (1.0 - 0.18 * intensity).max(0.35),
            StatusKind::Choking => (1.0 - 0.08 * intensity).max(0.65),
            StatusKind::Focused => (1.0 + 0.03 * intensity).min(1.12),
            StatusKind::Sadness => (1.0 - 0.08 * intensity).max(0.65),
            _ => 1.0,
        }
    }

    /// Contribution to camera/sensory distortion. `Focused` is negative so
    /// it can visibly oppose perception-altering drugs without deleting their
    /// underlying status timers.
    pub fn perception_distortion(self, intensity: f32) -> f32 {
        let intensity = intensity.max(0.0);
        match self {
            StatusKind::Blurred => 0.50 * intensity,
            StatusKind::Unsteady => 0.20 * intensity,
            StatusKind::Drunk => 0.35 * intensity,
            StatusKind::Sedated => 0.15 * intensity,
            StatusKind::Euphoric => 0.10 * intensity,
            StatusKind::Hallucinating => 0.80 * intensity,
            StatusKind::Paranoid => 0.15 * intensity,
            StatusKind::Focused => -0.40 * intensity,
            StatusKind::Happiness => 0.05 * intensity,
            StatusKind::Sadness => 0.15 * intensity,
            _ => 0.0,
        }
    }

    /// Contribution to deterministic stumble/drop timing. The game layer can
    /// turn this scalar into a warning cadence; it must not use it for random
    /// input loss or control inversion.
    pub fn motor_instability(self, intensity: f32) -> f32 {
        let intensity = intensity.max(0.0);
        match self {
            StatusKind::Unsteady => 0.55 * intensity,
            StatusKind::Drunk => 0.35 * intensity,
            StatusKind::Sedated => 0.30 * intensity,
            StatusKind::Chilled => 0.25 * intensity,
            StatusKind::Mutating => 0.20 * intensity,
            StatusKind::Focused => -0.50 * intensity,
            _ => 0.0,
        }
    }

    /// Fraction of visual detection suppressed at this intensity.
    ///
    /// Capped below one so concealment helps rather than becoming an
    /// uncounterable immunity. Game layers can turn this into a shorter
    /// detection radius while the presentation layer fades the same body.
    pub fn concealment(self, intensity: f32) -> f32 {
        match self {
            StatusKind::Obscured => (0.45 * intensity.max(0.0)).min(0.80),
            _ => 0.0,
        }
    }

    /// Fraction of ordinary verbal reporting suppressed by this status.
    /// Witnesses remain able to report what they saw, so muting creates
    /// counterplay rather than erasing consequences in a populated room.
    pub fn communication_suppression(self, intensity: f32) -> f32 {
        match self {
            StatusKind::Muted => (0.5 * intensity.max(0.0)).min(1.0),
            _ => 0.0,
        }
    }

    /// Whether this status makes an otherwise non-damaging dose unsafe to
    /// administer without consent.
    pub fn is_harmful(self) -> bool {
        matches!(
            self,
            StatusKind::Sluggish
                | StatusKind::Blurred
                | StatusKind::Unsteady
                | StatusKind::Drunk
                | StatusKind::Irradiated
                | StatusKind::Sedated
                | StatusKind::Hallucinating
                | StatusKind::Paranoid
                | StatusKind::Burning
                | StatusKind::Chilled
                | StatusKind::Choking
                | StatusKind::Mutating
                | StatusKind::Sadness
                | StatusKind::Muted
                | StatusKind::Pacified
        )
    }
}

/// What a reagent does while it is in a body.
///
/// Deliberately small. Healing, damage, one-off contact burns and every "felt"
/// effect remain direct data; `Purge` is the one systemic operation because it
/// must inspect the other reagents in the bloodstream.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum ReagentEffect {
    /// Repairs this much damage per tick.
    Heal(DamageKind, Units),
    /// Deals this much damage per tick.
    Harm(DamageKind, Units),
    /// Deals the authored damage for every unit of this reagent currently in
    /// active blood. This is reserved for concentration-driven poisons such
    /// as Venom, where dilution and prompt purging must reduce the danger
    /// continuously instead of only crossing a single overdose threshold.
    VolumeScaledHarm(DamageKind, Units),
    /// One-off, the moment the dose lands, scaled by the route it came in by.
    /// Acid is worse in a syringe than in a cup.
    Contact(DamageKind, Units),
    /// Tops a status up while present. `seconds` is how much duration one tick
    /// adds; `intensity` is how hard it is felt.
    Status {
        kind: StatusKind,
        seconds: f32,
        intensity: f32,
    },
    /// Burns a status off. Water on drunk, hyronalin on irradiated.
    Counter {
        kind: StatusKind,
        seconds: f32,
        intensity: f32,
    },
    /// Removes up to this many units of every currently harmful reagent per
    /// tick. Whether a reagent is harmful is evaluated at its current dose, so
    /// therapeutic medicine below its overdose threshold is left alone.
    Purge(Units),
    /// Removes this many units of every reagent filed under a legitimate
    /// treatment category. Used by counter-medicine toxins such as Anacea;
    /// unlike `Purge`, therapeutic doses are deliberately valid targets.
    MedicinePurge(Units),
    /// One-off repair when applied by splash or a dedicated patch. The
    /// magnitude is quoted per 10u absorbed, like `Contact`, but injections
    /// and ingestion do not receive the topical bonus.
    TopicalHeal(DamageKind, Units),
    /// Repairs damage per tick only while the named status is currently
    /// active. This makes authored emergency medicines depend on readable body
    /// state without putting recipe-specific conditionals in the simulator.
    ConditionalHeal {
        required: StatusKind,
        kind: DamageKind,
        amount: Units,
    },
    /// Deals damage only while the patient already has damage in the named
    /// channel. Heparin uses existing brute trauma as the readable proxy for
    /// an open bleeding wound in the current four-channel body model.
    ConditionalHarm {
        existing: DamageKind,
        kind: DamageKind,
        amount: Units,
    },
    /// Repairs damage per tick only while the patient's total damage is in
    /// the critical range. Emergency medicines can therefore be powerful
    /// without replacing ordinary treatment for a walking patient.
    CriticalHeal(DamageKind, Units),
    /// Begins applying a status only after the reagent has survived this many
    /// metabolism ticks in active blood. This makes delayed poisons readable
    /// and deterministic without recipe-specific timers in the game layer.
    DelayedStatus {
        after_ticks: u32,
        kind: StatusKind,
        seconds: f32,
        intensity: f32,
    },
    /// One-shot harm multiplied by the number of metabolism ticks the reagent
    /// survived. Used by poisons which remain quiet until they clear.
    AccumulatedHarm(DamageKind, Units),
    /// Deals damage every metabolism tick after the authored onset. Multiple
    /// tiers may stack, allowing a poison to have a quiet warning period and
    /// then escalate deterministically without bespoke simulator code.
    DelayedHarm {
        after_ticks: u32,
        kind: DamageKind,
        amount: Units,
    },
}

impl ReagentEffect {
    /// Whether this effect can hurt whoever takes it.
    ///
    /// Used to keep the game from handing the player an unsolicited bottle of
    /// something corrosive as a "here is what the last chemist made" sample.
    pub fn is_harmful(self) -> bool {
        match self {
            ReagentEffect::Harm(..)
            | ReagentEffect::VolumeScaledHarm(..)
            | ReagentEffect::Contact(..)
            | ReagentEffect::ConditionalHarm { .. }
            | ReagentEffect::AccumulatedHarm(..)
            | ReagentEffect::DelayedHarm { .. }
            | ReagentEffect::MedicinePurge(..) => true,
            ReagentEffect::Status { kind, .. } | ReagentEffect::DelayedStatus { kind, .. } => {
                kind.is_harmful()
            }
            ReagentEffect::Heal(..)
            | ReagentEffect::Counter { .. }
            | ReagentEffect::Purge(..)
            | ReagentEffect::TopicalHeal(..)
            | ReagentEffect::ConditionalHeal { .. }
            | ReagentEffect::CriticalHeal(..) => false,
        }
    }

    /// The magnitude a data file declared, for the guardrail test that catches
    /// an effect written with a zero or negative number.
    pub fn magnitude(self) -> f64 {
        match self {
            ReagentEffect::Heal(_, amount)
            | ReagentEffect::Harm(_, amount)
            | ReagentEffect::VolumeScaledHarm(_, amount)
            | ReagentEffect::Contact(_, amount)
            | ReagentEffect::TopicalHeal(_, amount)
            | ReagentEffect::CriticalHeal(_, amount)
            | ReagentEffect::AccumulatedHarm(_, amount)
            | ReagentEffect::Purge(amount)
            | ReagentEffect::MedicinePurge(amount) => amount.as_f64(),
            ReagentEffect::Status {
                seconds, intensity, ..
            }
            | ReagentEffect::Counter {
                seconds, intensity, ..
            }
            | ReagentEffect::DelayedStatus {
                seconds, intensity, ..
            } => (seconds.min(intensity)) as f64,
            ReagentEffect::ConditionalHeal { amount, .. }
            | ReagentEffect::ConditionalHarm { amount, .. } => amount.as_f64(),
            ReagentEffect::DelayedHarm { amount, .. } => amount.as_f64(),
        }
    }
}

/// What a reagent does to the station when released from a container.
///
/// These are intentionally declarative: `chem_sim` defines and validates the
/// data while the authority-owned game layer supplies positions, targets,
/// puddles and visual effects. All numeric fields must be positive.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum WorldEffect {
    /// Removes chemical residue/puddles. `strength` is a unitless multiplier.
    Clean { strength: f32 },
    /// Damages objects explicitly marked chemically reactive.
    Corrode { strength: f32 },
    /// Ignites bodies and flammable puddles for at least `seconds`.
    Ignite { intensity: f32, seconds: f32 },
    /// Releases a cloud which carries the source solution to touched bodies.
    ReleaseSmoke { radius: f32, seconds: f32 },
    /// Makes a puddle slippery for the given lifetime.
    Slippery { seconds: f32 },
    /// Marks a puddle as fuel without igniting it immediately. Contact with a
    /// flame turns this into a `Burning` exposure at the declared intensity.
    Flammable { intensity: f32, seconds: f32 },
    /// Removes this many kelvin per unit released, clamped by the game layer.
    Chill { kelvin_per_unit: f32 },
    /// Applies a sight/sensory flash within `radius` for `seconds`.
    Flash { radius: f32, seconds: f32 },
    /// Expands the released mixture across an area while preserving its
    /// contents, quality, and owner. Solid foam also becomes a temporary
    /// collision barrier until its carrier expires.
    ExpandFoam {
        radius: f32,
        seconds: f32,
        solid: bool,
    },
    /// Suppresses burning bodies and ignited chemical material in an area.
    Extinguish { radius: f32, seconds: f32 },
}

impl WorldEffect {
    /// Smallest declared magnitude, used by content guardrails to reject
    /// zero-strength effects (including half-configured radius/duration pairs).
    pub fn magnitude(self) -> f32 {
        match self {
            WorldEffect::Clean { strength } | WorldEffect::Corrode { strength } => strength,
            WorldEffect::Ignite { intensity, seconds } => intensity.min(seconds),
            WorldEffect::ReleaseSmoke { radius, seconds }
            | WorldEffect::Flash { radius, seconds }
            | WorldEffect::ExpandFoam {
                radius, seconds, ..
            }
            | WorldEffect::Extinguish { radius, seconds } => radius.min(seconds),
            WorldEffect::Slippery { seconds } => seconds,
            WorldEffect::Flammable { intensity, seconds } => intensity.min(seconds),
            WorldEffect::Chill { kelvin_per_unit } => kelvin_per_unit,
        }
    }

    pub fn is_harmful(self) -> bool {
        !matches!(
            self,
            WorldEffect::Clean { .. }
                | WorldEffect::Extinguish { .. }
                | WorldEffect::ExpandFoam { solid: false, .. }
        )
    }
}

/// How a reagent got into a body. Route decides how much lands and how fast.
///
/// This is the whole reason the syringe is worth making: the same 15u does
/// very different things depending on how it gets in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Route {
    /// Straight into the blood. All of it, immediately.
    Injected,
    /// Swallowed. Most of it, slowly, through the stomach.
    Ingested,
    /// Splashed or breathed. Very little of it.
    Touched,
    /// A sealed patch. The whole dose lands without becoming an injection,
    /// preserving topical effects and route-specific gameplay.
    Patched,
    /// A fine spray aimed directly at exposed skin. More controlled than a
    /// thrown splash, but still a topical route rather than an injection.
    Sprayed,
    /// Aerosol taken through the lungs. It reaches blood directly without
    /// applying skin contact or topical-healing effects.
    Inhaled,
}

impl Route {
    /// Share of the dose that gets in at all.
    pub fn absorbed(self) -> Units {
        match self {
            Route::Injected => Units::ONE,
            Route::Ingested => Units::from_raw(60),
            Route::Touched => Units::from_raw(15),
            Route::Patched => Units::ONE,
            Route::Sprayed => Units::from_raw(35),
            Route::Inhaled => Units::from_raw(40),
        }
    }

    /// Whether it goes through the stomach first. This is what makes drinking
    /// a slow drip rather than a hit.
    pub fn digested(self) -> bool {
        matches!(self, Route::Ingested)
    }

    /// Multiplier on [`ReagentEffect::Contact`] damage. A needle drives acid
    /// past the skin that a mouthful mostly does not.
    pub fn contact_scale(self) -> f32 {
        match self {
            Route::Injected => 2.0,
            Route::Ingested => 1.0,
            Route::Touched => 0.5,
            Route::Patched => 1.0,
            Route::Sprayed => 0.75,
            Route::Inhaled => 0.0,
        }
    }

    /// Multiplier for one-off topical repair. Splashing can trigger it, but
    /// only the small absorbed share lands; patches deliver the full dose.
    pub fn topical_scale(self) -> f32 {
        match self {
            Route::Touched | Route::Patched | Route::Sprayed => 1.0,
            Route::Injected | Route::Ingested | Route::Inhaled => 0.0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Route::Injected => "injected",
            Route::Ingested => "swallowed",
            Route::Touched => "splashed",
            Route::Patched => "patched",
            Route::Sprayed => "sprayed",
            Route::Inhaled => "inhaled",
        }
    }
}

/// Damage across the four types.
///
/// Also used for healing, where the fields mean "repair this much" — the sign
/// lives in which method you call, not in the value, so a negative never
/// silently becomes a heal.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Damage {
    pub brute: Units,
    pub burn: Units,
    pub toxin: Units,
    pub oxygen: Units,
}

impl Damage {
    pub fn of(kind: DamageKind, amount: Units) -> Damage {
        let mut damage = Damage::default();
        *damage.get_mut(kind) = amount;
        damage
    }

    pub fn get(&self, kind: DamageKind) -> Units {
        match kind {
            DamageKind::Brute => self.brute,
            DamageKind::Burn => self.burn,
            DamageKind::Toxin => self.toxin,
            DamageKind::Oxygen => self.oxygen,
        }
    }

    pub fn get_mut(&mut self, kind: DamageKind) -> &mut Units {
        match kind {
            DamageKind::Brute => &mut self.brute,
            DamageKind::Burn => &mut self.burn,
            DamageKind::Toxin => &mut self.toxin,
            DamageKind::Oxygen => &mut self.oxygen,
        }
    }

    pub fn total(&self) -> Units {
        self.brute + self.burn + self.toxin + self.oxygen
    }

    pub fn is_zero(&self) -> bool {
        *self == Damage::default()
    }

    /// Scales every field by `numerator / denominator`, in the same fixed-point
    /// way reaction quantities scale.
    pub fn scaled(self, numerator: Units, denominator: Units) -> Damage {
        Damage {
            brute: self.brute.scaled(numerator, denominator),
            burn: self.burn.scaled(numerator, denominator),
            toxin: self.toxin.scaled(numerator, denominator),
            oxygen: self.oxygen.scaled(numerator, denominator),
        }
    }
}

impl std::ops::Add for Damage {
    type Output = Damage;
    fn add(self, rhs: Damage) -> Damage {
        Damage {
            brute: self.brute + rhs.brute,
            burn: self.burn + rhs.burn,
            toxin: self.toxin + rhs.toxin,
            oxygen: self.oxygen + rhs.oxygen,
        }
    }
}

impl std::ops::AddAssign for Damage {
    fn add_assign(&mut self, rhs: Damage) {
        *self = *self + rhs;
    }
}
