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
}

impl StatusKind {
    /// Damage this status deals per metabolism tick at the given intensity.
    ///
    /// Only radiation harms on its own. Everything else is *felt* rather than
    /// fatal: being unsteady never killed anyone, it just made them spill the
    /// beaker.
    pub fn tick_damage(self, intensity: f32) -> Damage {
        match self {
            StatusKind::Irradiated => Damage::of(
                DamageKind::Toxin,
                Units::from_f64((intensity as f64).max(0.0)),
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
        }
    }
}

/// What a reagent does while it is in a body.
///
/// Deliberately small. Five variants cover healing, damage, one-off contact
/// burns and every "felt" effect, because speed, blur and sway are all just
/// [`StatusKind`]s the presentation layer reads. Adding a variant here means
/// adding a match arm in the tick, so the bar for a sixth is high.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub enum ReagentEffect {
    /// Repairs this much damage per tick.
    Heal(DamageKind, Units),
    /// Deals this much damage per tick.
    Harm(DamageKind, Units),
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
}

impl ReagentEffect {
    /// Whether this effect can hurt whoever takes it.
    ///
    /// Used to keep the game from handing the player an unsolicited bottle of
    /// something corrosive as a "here is what the last chemist made" sample.
    pub fn is_harmful(self) -> bool {
        match self {
            ReagentEffect::Harm(..) | ReagentEffect::Contact(..) => true,
            ReagentEffect::Status { kind, .. } => kind == StatusKind::Irradiated,
            ReagentEffect::Heal(..) | ReagentEffect::Counter { .. } => false,
        }
    }

    /// The magnitude a data file declared, for the guardrail test that catches
    /// an effect written with a zero or negative number.
    pub fn magnitude(self) -> f64 {
        match self {
            ReagentEffect::Heal(_, amount)
            | ReagentEffect::Harm(_, amount)
            | ReagentEffect::Contact(_, amount) => amount.as_f64(),
            ReagentEffect::Status {
                seconds, intensity, ..
            }
            | ReagentEffect::Counter {
                seconds, intensity, ..
            } => (seconds.min(intensity)) as f64,
        }
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
}

impl Route {
    /// Share of the dose that gets in at all.
    pub fn absorbed(self) -> Units {
        match self {
            Route::Injected => Units::ONE,
            Route::Ingested => Units::from_raw(60),
            Route::Touched => Units::from_raw(15),
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
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Route::Injected => "injected",
            Route::Ingested => "swallowed",
            Route::Touched => "splashed",
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
