//! What chemicals do to the person who takes them.
//!
//! Kept engine-free like the rest of this crate, which is what lets a whole
//! poisoning play out in a unit test with no `App` and no window. The game
//! layer owns entities, timers and rendering; everything here is arithmetic.
//!
//! The shape mirrors the beaker deliberately: a bloodstream *is* a
//! [`Solution`], so reagents react inside you exactly as they do in glassware.
//! Injecting two half-finished mixes is a real mistake with a real result, and
//! it costs nothing to support because the resolver already existed.

use serde::{Deserialize, Serialize};

use crate::effect::{Damage, DamageKind, ReagentEffect, Route, StatusKind};
use crate::reagent::ReagentId;
use crate::resolver::{resolve, ResolveReport};
use crate::solution::Solution;
use crate::units::Units;
use crate::ChemData;

/// Damage and healing are fixed-point for the same reason reagent quantities
/// are: they land 0.4 at a time over hundreds of ticks and get compared
/// against exact thresholds. Reusing [`Units`] also inherits its dual serde
/// encoding, which a body needs because it crosses the wire in co-op.
pub type Health = Units;

/// Seconds between metabolism ticks. /tg/station's life cycle.
pub const TICK_SECONDS: f32 = 2.0;

/// Absorbed out of the stomach per tick. Low on purpose: drinking a 50u beaker
/// is a long drip, which is the whole reason a syringe is worth making.
pub const DIGESTION_RATE: Units = Units::whole(2);

/// The dose a [`ReagentEffect::Contact`] magnitude is quoted against. A
/// `Contact(Burn, 2)` reagent does 2 burn per 10u that lands, before the route
/// multiplier.
pub const CONTACT_REFERENCE_DOSE: Units = Units::whole(10);

/// Ceiling on any single damage type.
pub const MAX_DAMAGE_PER_KIND: Health = Units::whole(100);

/// Total damage at which a chemist goes down.
pub const COLLAPSE: Health = Units::whole(100);

/// Collapse only clears below this.
///
/// Hysteresis, and not optional: without the gap a chemist hovering at the
/// threshold would stand up and fall over on alternating ticks.
pub const RECOVER: Health = Units::whole(80);

/// Oxygen debt a body clears on its own each tick.
///
/// Only oxygen. Brute, burn and toxin never heal without chemistry, which is
/// what makes the medicine cabinet matter.
pub const OXYGEN_RECOVERY: Health = Units::whole(1);

/// How hurt someone is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vitals {
    pub damage: Damage,
    pub collapsed: bool,
}

impl Vitals {
    /// Adds damage, clamping each type at [`MAX_DAMAGE_PER_KIND`].
    pub fn apply(&mut self, harm: Damage) {
        for kind in DamageKind::ALL {
            let slot = self.damage.get_mut(kind);
            *slot = (*slot + harm.get(kind)).min(MAX_DAMAGE_PER_KIND);
        }
        self.update_collapse();
    }

    /// Repairs damage, clamping at zero.
    pub fn heal(&mut self, healing: Damage) {
        for kind in DamageKind::ALL {
            let slot = self.damage.get_mut(kind);
            *slot = (*slot - healing.get(kind)).clamp_non_negative();
        }
        self.update_collapse();
    }

    pub fn total(&self) -> Health {
        self.damage.total()
    }

    /// This damage type as a 0..1 share of its ceiling, for a HUD bar.
    pub fn fraction(&self, kind: DamageKind) -> f32 {
        (self.damage.get(kind).as_f32() / MAX_DAMAGE_PER_KIND.as_f32()).clamp(0.0, 1.0)
    }

    pub fn is_hurt(&self) -> bool {
        self.total().is_positive()
    }

    fn update_collapse(&mut self) {
        let total = self.total();
        if total >= COLLAPSE {
            self.collapsed = true;
        } else if self.collapsed && total < RECOVER {
            self.collapsed = false;
        }
    }
}

/// A status's remaining duration and how hard it is being felt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StatusState {
    pub remaining: f32,
    pub intensity: f32,
}

/// Everything currently inside a person.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Bloodstream {
    /// Absorbed and active. A [`Solution`], so it reacts.
    pub blood: Solution,
    /// Swallowed but not absorbed yet. This is the whole of "ingest is slow".
    pub stomach: Solution,
    /// Kept sorted by kind so iteration — and therefore every tick — is
    /// deterministic, for the same reason `Solution` sorts its contents.
    statuses: Vec<(StatusKind, StatusState)>,
}

impl Default for Bloodstream {
    fn default() -> Self {
        Bloodstream {
            blood: Solution::unbounded(),
            stomach: Solution::unbounded(),
            statuses: Vec::new(),
        }
    }
}

impl Bloodstream {
    pub fn new() -> Self {
        Bloodstream::default()
    }

    /// Nothing in the blood, nothing in the stomach, nothing being felt.
    ///
    /// Lets the game skip untouched bodies entirely rather than run a tick that
    /// cannot do anything.
    pub fn is_empty(&self) -> bool {
        self.blood.is_empty() && self.stomach.is_empty() && self.statuses.is_empty()
    }

    pub fn status(&self, kind: StatusKind) -> StatusState {
        self.statuses
            .binary_search_by_key(&kind, |(k, _)| *k)
            .map(|index| self.statuses[index].1)
            .unwrap_or_default()
    }

    pub fn active_statuses(&self) -> impl Iterator<Item = (StatusKind, StatusState)> + '_ {
        self.statuses.iter().copied()
    }

    /// Tops a status up. Duration accumulates; intensity takes the stronger of
    /// the two, so a second dose lasts longer without hitting harder.
    pub fn add_status(&mut self, kind: StatusKind, seconds: f32, intensity: f32) {
        if seconds <= 0.0 && intensity <= 0.0 {
            return;
        }
        match self.statuses.binary_search_by_key(&kind, |(k, _)| *k) {
            Ok(index) => {
                let state = &mut self.statuses[index].1;
                state.remaining += seconds.max(0.0);
                state.intensity = state.intensity.max(intensity);
            }
            Err(index) => self.statuses.insert(
                index,
                (
                    kind,
                    StatusState {
                        remaining: seconds.max(0.0),
                        intensity: intensity.max(0.0),
                    },
                ),
            ),
        }
    }

    /// Burns a status off. Water on drunk, hyronalin on irradiated.
    pub fn counter_status(&mut self, kind: StatusKind, seconds: f32, intensity: f32) {
        let Ok(index) = self.statuses.binary_search_by_key(&kind, |(k, _)| *k) else {
            return;
        };
        let state = &mut self.statuses[index].1;
        state.remaining -= seconds.max(0.0);
        state.intensity -= intensity.max(0.0);
        if state.remaining <= 0.0 || state.intensity <= 0.0 {
            self.statuses.remove(index);
        }
    }

    /// Everything a body has taken, blood and stomach together, largest first.
    ///
    /// What the HUD lists: a chemist wants to know what is in them, not which
    /// compartment it is sitting in.
    pub fn contents(&self) -> Vec<(ReagentId, Units)> {
        let mut merged: Vec<(ReagentId, Units)> = self.blood.iter().collect();
        for (id, amount) in self.stomach.iter() {
            match merged.binary_search_by_key(&id, |(r, _)| *r) {
                Ok(index) => merged[index].1 += amount,
                Err(index) => merged.insert(index, (id, amount)),
            }
        }
        merged.sort_by_key(|(id, amount)| (std::cmp::Reverse(amount.raw()), *id));
        merged
    }

    /// The one way anything gets into a body.
    ///
    /// Mirrors [`crate::Solution`]'s use through `Container::mutate` in the game
    /// layer: route handling, contact damage and the resolver can never be
    /// skipped, because there is nowhere else to add reagent.
    ///
    /// `dose` is consumed. What the route does not absorb went on the floor or
    /// straight through, and is gone either way — the caller has already split
    /// it off whatever container it came from.
    pub fn receive(
        &mut self,
        dose: &mut Solution,
        route: Route,
        vitals: &mut Vitals,
        data: &ChemData,
    ) -> ExposureReport {
        let offered = dose.total_volume();
        if !offered.is_positive() {
            return ExposureReport::default();
        }

        // Contact damage is charged on what actually lands, not what was
        // offered — a splash that mostly misses mostly does not burn.
        let landing = offered.scaled(route.absorbed(), Units::ONE);
        let mut contact = Damage::default();
        for (id, amount) in dose.iter() {
            let absorbed = amount.scaled(route.absorbed(), Units::ONE);
            for effect in &data.reagents.get(id).effects {
                if let ReagentEffect::Contact(kind, magnitude) = effect {
                    let scaled = magnitude
                        .scaled(absorbed, CONTACT_REFERENCE_DOSE)
                        .scaled(Units::from_f64(route.contact_scale() as f64), Units::ONE);
                    contact += Damage::of(*kind, scaled);
                }
            }
        }
        vitals.apply(contact);

        let destination = if route.digested() {
            &mut self.stomach
        } else {
            &mut self.blood
        };
        let absorbed = dose.transfer_to(destination, landing);
        dose.clear();

        // Only blood reacts. The stomach is a holding pen, and a reaction in
        // there would fire before the dose had a chance to be a mistake.
        let reactions = resolve(&mut self.blood, &data.reactions);

        ExposureReport {
            absorbed,
            contact,
            reactions,
        }
    }
}

/// What one exposure did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExposureReport {
    pub absorbed: Units,
    pub contact: Damage,
    pub reactions: ResolveReport,
}

/// What one metabolism tick did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TickReport {
    pub healed: Damage,
    pub harmed: Damage,
    /// Reagents currently past their overdose threshold. The HUD flashes these.
    pub overdosing: Vec<ReagentId>,
    /// True only on the tick the body went down — an edge, not a level, so the
    /// game layer can fire a one-off response without tracking the previous
    /// state itself.
    pub collapsed: bool,
    pub reactions: ResolveReport,
}

/// Runs one 2-second tick.
///
/// The order is fixed and load-bearing:
///
/// 1. stomach into blood, proportionally, up to [`DIGESTION_RATE`]
/// 2. resolve the blood — reagents react inside you
/// 3. per reagent in id order: `effects`, plus `overdose_effects` past the
///    threshold, plus `critical_effects` past the critical threshold
/// 4. status damage, then decay every status
/// 5. oxygen debt recovers
/// 6. work off each reagent's metabolism rate
///
/// Step 6 comes last on purpose: a reagent with less left than its rate gets
/// one final full tick of effect before it disappears, which is what SS13 does
/// and what stops a 0.1u remainder being silently worthless.
pub fn metabolise(vitals: &mut Vitals, blood: &mut Bloodstream, data: &ChemData) -> TickReport {
    let was_collapsed = vitals.collapsed;
    let mut report = TickReport::default();

    // 1. Digestion.
    if !blood.stomach.is_empty() {
        let _ = blood.stomach.transfer_to(&mut blood.blood, DIGESTION_RATE);
    }

    // 2. Reagents react in you.
    report.reactions = resolve(&mut blood.blood, &data.reactions);

    // 3. Per-reagent effects.
    let present: Vec<(ReagentId, Units)> = blood.blood.iter().collect();
    for (id, volume) in &present {
        let reagent = data.reagents.get(*id);

        let overdosing = matches!(reagent.overdose, Some(threshold) if *volume > threshold);
        let critical = matches!(reagent.critical_overdose, Some(threshold) if *volume > threshold);
        if overdosing {
            report.overdosing.push(*id);
        }

        // Tiers stack rather than replace: the medicine is still working, it is
        // just also hurting you now.
        let mut tiers: Vec<&ReagentEffect> = reagent.effects.iter().collect();
        if overdosing {
            tiers.extend(&reagent.overdose_effects);
        }
        if critical {
            tiers.extend(&reagent.critical_effects);
        }

        for effect in tiers {
            match *effect {
                ReagentEffect::Heal(kind, amount) => report.healed += Damage::of(kind, amount),
                ReagentEffect::Harm(kind, amount) => report.harmed += Damage::of(kind, amount),
                // Charged once on arrival, in `receive`. Nothing to do per tick.
                ReagentEffect::Contact(..) => {}
                ReagentEffect::Status {
                    kind,
                    seconds,
                    intensity,
                } => blood.add_status(kind, seconds, intensity),
                ReagentEffect::Counter {
                    kind,
                    seconds,
                    intensity,
                } => blood.counter_status(kind, seconds, intensity),
            }
        }
    }

    // 4. Status damage, then decay. Damage first, so a status that expires this
    //    tick still gets its last word in.
    for (kind, state) in blood.statuses.clone() {
        report.harmed += kind.tick_damage(state.intensity);
    }
    for entry in &mut blood.statuses {
        entry.1.remaining -= TICK_SECONDS * entry.0.decay();
    }
    blood.statuses.retain(|(_, state)| state.remaining > 0.0);

    vitals.heal(report.healed);
    vitals.apply(report.harmed);

    // 5. Oxygen debt clears on its own.
    if vitals.damage.oxygen.is_positive() {
        vitals.heal(Damage::of(DamageKind::Oxygen, OXYGEN_RECOVERY));
    }

    // 6. Work off the doses.
    for (id, _) in present {
        let rate = data.reagents.get(id).rate();
        let _ = blood.blood.remove(id, rate);
    }

    report.collapsed = vitals.collapsed && !was_collapsed;
    report
}

/// Metres a blast of `power` reaches.
pub fn blast_radius(power: f32) -> f32 {
    (power * 1.6).max(0.0)
}

/// Brute and burn from a blast of `power` at `distance` metres.
///
/// Falls off with the square of the distance and reaches nothing past
/// [`blast_radius`]. Takes and returns plain numbers rather than a position, so
/// this stays engine-free — the caller knows what a metre is.
pub fn explosion_damage(power: f32, distance: f32) -> Damage {
    let radius = blast_radius(power);
    if power <= 0.0 || radius <= 0.0 || distance >= radius {
        return Damage::default();
    }
    let closeness = (1.0 - distance.max(0.0) / radius).clamp(0.0, 1.0);
    let falloff = closeness * closeness;
    Damage {
        brute: Units::from_f64((power * 6.0 * falloff) as f64),
        burn: Units::from_f64((power * 4.0 * falloff) as f64),
        ..Damage::default()
    }
}
