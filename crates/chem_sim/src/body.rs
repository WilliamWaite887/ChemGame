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

/// Total damage at which critical-care-only medicine activates.
///
/// Matching the recovery boundary makes the state easy to read: a patient
/// who is down, or only just stable enough to stand again, still qualifies.
pub const CRITICAL_DAMAGE: Health = RECOVER;

/// Extra collapse headroom per point of `Stabilized` intensity.
pub const STABILIZED_COLLAPSE_BONUS: Health = Units::whole(25);

/// Extra collapse headroom per point of `Analgesic` intensity. Analgesia masks
/// more injury than medical stabilization, but heals none of it.
pub const ANALGESIC_COLLAPSE_BONUS: Health = Units::whole(30);

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
    /// Consecutive metabolism ticks each reagent has survived in active
    /// blood. Kept sorted by id for deterministic delayed-effect onset.
    #[serde(default)]
    exposure_ticks: Vec<(ReagentId, u32)>,
}

impl Default for Bloodstream {
    fn default() -> Self {
        Bloodstream {
            blood: Solution::unbounded(),
            stomach: Solution::unbounded(),
            statuses: Vec::new(),
            exposure_ticks: Vec::new(),
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

    fn ticks_present(&self, id: ReagentId) -> u32 {
        self.exposure_ticks
            .binary_search_by_key(&id, |(reagent, _)| *reagent)
            .map(|index| self.exposure_ticks[index].1)
            .unwrap_or(0)
    }

    fn retain_present_exposure_ticks(&mut self) {
        self.exposure_ticks
            .retain(|(id, _)| self.blood.volume_of(*id).is_positive());
    }

    fn advance_exposure_ticks(&mut self) {
        self.retain_present_exposure_ticks();
        let present: Vec<ReagentId> = self.blood.iter().map(|(id, _)| id).collect();
        for id in present {
            match self
                .exposure_ticks
                .binary_search_by_key(&id, |(reagent, _)| *reagent)
            {
                Ok(index) => {
                    self.exposure_ticks[index].1 = self.exposure_ticks[index].1.saturating_add(1)
                }
                Err(index) => self.exposure_ticks.insert(index, (id, 1)),
            }
        }
    }

    /// Combined deterministic movement modifier for gameplay and crew AI.
    pub fn movement_multiplier(&self) -> f32 {
        self.statuses
            .iter()
            .fold(1.0, |speed, (kind, state)| {
                speed * kind.movement_multiplier(state.intensity)
            })
            .clamp(0.0, 1.8)
    }

    /// Combined sensory distortion. The focused status offsets, but cannot
    /// invert, other presentation effects.
    pub fn perception_distortion(&self) -> f32 {
        self.statuses
            .iter()
            .map(|(kind, state)| kind.perception_distortion(state.intensity))
            .sum::<f32>()
            .clamp(0.0, 5.0)
    }

    /// Scalar for deterministic stumble/drop cadence. A game layer should
    /// warn before acting on it and must not turn it into random input loss.
    pub fn motor_instability(&self) -> f32 {
        self.statuses
            .iter()
            .map(|(kind, state)| kind.motor_instability(state.intensity))
            .sum::<f32>()
            .clamp(0.0, 5.0)
    }

    /// Fraction of observer detection range hidden by chemistry.
    ///
    /// Statuses use the strongest contribution rather than stacking, so two
    /// concealment drugs cannot accidentally cross the intended 80% cap.
    pub fn concealment(&self) -> f32 {
        self.statuses
            .iter()
            .map(|(kind, state)| kind.concealment(state.intensity))
            .fold(0.0, f32::max)
            .clamp(0.0, 0.80)
    }

    /// Fraction of ordinary speech/reporting currently suppressed.
    pub fn communication_suppression(&self) -> f32 {
        self.statuses
            .iter()
            .map(|(kind, state)| kind.communication_suppression(state.intensity))
            .fold(0.0, f32::max)
            .clamp(0.0, 1.0)
    }

    /// Chemical incapacitation is separate from damage collapse: when the
    /// sedative clears the body can stand immediately if otherwise healthy.
    pub fn incapacitated(&self) -> bool {
        self.status(StatusKind::Sedated).intensity >= 2.0
    }

    /// The strongest sedative tier presents as apparent death while preserving
    /// actual vitals. Zombie powder uses this; chloral hydrate does not.
    pub fn appears_dead(&self) -> bool {
        self.status(StatusKind::Sedated).intensity >= 3.5
    }

    /// Current damage threshold for collapse after stabilization/analgesia.
    pub fn collapse_threshold(&self) -> Health {
        let stabilized = self.status(StatusKind::Stabilized).intensity.max(0.0);
        let analgesic = self.status(StatusKind::Analgesic).intensity.max(0.0);
        COLLAPSE
            + STABILIZED_COLLAPSE_BONUS.scaled(Units::from_f64(stabilized as f64), Units::ONE)
            + ANALGESIC_COLLAPSE_BONUS.scaled(Units::from_f64(analgesic as f64), Units::ONE)
    }

    /// Reconciles damage collapse with status-adjusted thresholds while
    /// retaining the same 20-point hysteresis as ordinary vitals.
    pub fn reconcile_collapse(&self, vitals: &mut Vitals, previously_collapsed: bool) {
        let collapse = self.collapse_threshold();
        let recover = (collapse - (COLLAPSE - RECOVER)).clamp_non_negative();
        vitals.collapsed = if previously_collapsed {
            vitals.total() >= recover
        } else {
            vitals.total() >= collapse
        };
    }

    /// Fraction of new radiation status blocked by potassium iodide-style
    /// protection. Capped so overwhelming exposure still has an effect.
    pub fn radiation_resistance(&self) -> f32 {
        (self.status(StatusKind::RadiationShield).intensity.max(0.0) * 0.75).min(0.95)
    }

    /// Fraction of oxygen harm softened by stabilization. Dexalin applies a
    /// stronger stabilizing status than inaprovaline.
    pub fn oxygen_resistance(&self) -> f32 {
        (self.status(StatusKind::Stabilized).intensity.max(0.0) * 0.25).min(0.75)
    }

    /// Tops a status up. Duration accumulates; intensity takes the stronger of
    /// the two, so a second dose lasts longer without hitting harder.
    pub fn add_status(&mut self, kind: StatusKind, seconds: f32, intensity: f32) {
        let (seconds, intensity) = if kind == StatusKind::Irradiated {
            let landing = 1.0 - self.radiation_resistance();
            (seconds * landing, intensity * landing)
        } else {
            (seconds, intensity)
        };
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
        let mut topical_healing = Damage::default();
        for (id, amount) in dose.iter() {
            let absorbed = amount.scaled(route.absorbed(), Units::ONE);
            let purity = Units::from_f64(dose.purity_of(id) as f64);
            for effect in &data.reagents.get(id).effects {
                if let ReagentEffect::Contact(kind, magnitude) = effect {
                    let scaled = magnitude
                        .scaled(absorbed, CONTACT_REFERENCE_DOSE)
                        .scaled(Units::from_f64(route.contact_scale() as f64), Units::ONE)
                        .scaled(purity, Units::ONE);
                    contact += Damage::of(*kind, scaled);
                }
                if let ReagentEffect::TopicalHeal(kind, magnitude) = effect {
                    let scaled = magnitude
                        .scaled(absorbed, CONTACT_REFERENCE_DOSE)
                        .scaled(Units::from_f64(route.topical_scale() as f64), Units::ONE)
                        .scaled(purity, Units::ONE);
                    topical_healing += Damage::of(*kind, scaled);
                }
            }
        }
        let was_collapsed = vitals.collapsed;
        vitals.apply(contact);
        vitals.heal(topical_healing);

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

        self.reconcile_collapse(vitals, was_collapsed);

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
    /// Reagents removed early by an antitoxin this tick.
    pub purged: Vec<(ReagentId, Units)>,
    /// Reagents whose one-shot `after_effects` fired this tick.
    pub after_effects: Vec<ReagentId>,
    pub reactions: ResolveReport,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PurgeRequest {
    harmful: Units,
    medicines: Units,
}

impl std::ops::AddAssign for PurgeRequest {
    fn add_assign(&mut self, rhs: Self) {
        self.harmful += rhs.harmful;
        self.medicines += rhs.medicines;
    }
}

#[derive(Clone, Copy, Debug)]
struct EffectContext {
    purity: f32,
    current_volume: Units,
    critically_injured: bool,
    existing_damage: Damage,
    ticks_present: u32,
}

/// Applies one data effect and returns any requested per-target purge amount.
fn apply_effect(
    effect: ReagentEffect,
    context: EffectContext,
    blood: &mut Bloodstream,
    report: &mut TickReport,
) -> PurgeRequest {
    let EffectContext {
        purity,
        current_volume,
        critically_injured,
        existing_damage,
        ticks_present,
    } = context;
    let scale = Units::from_f64(purity.clamp(0.0, 1.0) as f64);
    match effect {
        ReagentEffect::Heal(kind, amount) => {
            report.healed += Damage::of(kind, amount.scaled(scale, Units::ONE))
        }
        ReagentEffect::Harm(kind, amount) => {
            report.harmed += Damage::of(kind, amount.scaled(scale, Units::ONE))
        }
        ReagentEffect::VolumeScaledHarm(kind, amount) => {
            report.harmed += Damage::of(
                kind,
                amount
                    .scaled(current_volume, Units::ONE)
                    .scaled(scale, Units::ONE),
            )
        }
        // Charged once on arrival, in `receive`. Nothing to do per tick.
        ReagentEffect::Contact(..) | ReagentEffect::TopicalHeal(..) => {}
        ReagentEffect::ConditionalHeal {
            required,
            kind,
            amount,
        } => {
            if blood.status(required).intensity > 0.0 {
                report.healed += Damage::of(kind, amount.scaled(scale, Units::ONE));
            }
        }
        ReagentEffect::ConditionalHarm {
            existing,
            kind,
            amount,
        } => {
            if existing_damage.get(existing).is_positive() {
                report.harmed += Damage::of(kind, amount.scaled(scale, Units::ONE));
            }
        }
        ReagentEffect::CriticalHeal(kind, amount) => {
            if critically_injured {
                report.healed += Damage::of(kind, amount.scaled(scale, Units::ONE));
            }
        }
        ReagentEffect::Status {
            kind,
            seconds,
            intensity,
        } => blood.add_status(kind, seconds * purity, intensity),
        ReagentEffect::DelayedStatus {
            after_ticks,
            kind,
            seconds,
            intensity,
        } => {
            if ticks_present >= after_ticks {
                blood.add_status(kind, seconds * purity, intensity);
            }
        }
        ReagentEffect::AccumulatedHarm(kind, amount) => {
            let exposure = Units::whole(ticks_present.min(i32::MAX as u32) as i32);
            report.harmed += Damage::of(
                kind,
                amount
                    .scaled(exposure, Units::ONE)
                    .scaled(scale, Units::ONE),
            );
        }
        ReagentEffect::DelayedHarm {
            after_ticks,
            kind,
            amount,
        } => {
            if ticks_present >= after_ticks {
                report.harmed += Damage::of(kind, amount.scaled(scale, Units::ONE));
            }
        }
        ReagentEffect::Counter {
            kind,
            seconds,
            intensity,
        } => blood.counter_status(kind, seconds * purity, intensity),
        ReagentEffect::Purge(amount) => {
            return PurgeRequest {
                harmful: amount.scaled(scale, Units::ONE),
                ..Default::default()
            }
        }
        ReagentEffect::MedicinePurge(amount) => {
            return PurgeRequest {
                medicines: amount.scaled(scale, Units::ONE),
                ..Default::default()
            }
        }
    }
    PurgeRequest::default()
}

fn purge_harmful(
    blood: &mut Bloodstream,
    data: &ChemData,
    amount_per_reagent: Units,
    report: &mut TickReport,
) {
    if !amount_per_reagent.is_positive() {
        return;
    }
    let targets: Vec<ReagentId> = blood
        .blood
        .iter()
        .filter_map(|(id, volume)| data.reagents.get(id).is_harmful_at(volume).then_some(id))
        .collect();
    for id in targets {
        let removed = blood.blood.remove(id, amount_per_reagent);
        if removed.is_positive() {
            report.purged.push((id, removed));
        }
    }
}

fn purge_medicines(
    blood: &mut Bloodstream,
    data: &ChemData,
    amount_per_reagent: Units,
    report: &mut TickReport,
) {
    if !amount_per_reagent.is_positive() {
        return;
    }
    let targets: Vec<ReagentId> = blood
        .blood
        .iter()
        .filter_map(|(id, _)| {
            data.reagents
                .get(id)
                .categories
                .iter()
                .any(|category| category.is_legitimately_orderable())
                .then_some(id)
        })
        .collect();
    for id in targets {
        let removed = blood.blood.remove(id, amount_per_reagent);
        if removed.is_positive() {
            report.purged.push((id, removed));
        }
    }
}

fn purge_targets(
    blood: &mut Bloodstream,
    data: &ChemData,
    targets: &[(String, Units)],
    purity: f32,
    report: &mut TickReport,
) {
    let scale = Units::from_f64(purity.clamp(0.0, 1.0) as f64);
    for (target, amount) in targets {
        let Some(id) = data.reagents.id_of(target) else {
            continue;
        };
        let removed = blood.blood.remove(id, amount.scaled(scale, Units::ONE));
        if removed.is_positive() {
            report.purged.push((id, removed));
        }
    }
}

/// Runs one 2-second tick.
///
/// The order is fixed and load-bearing:
///
/// 1. stomach into blood, proportionally, up to [`DIGESTION_RATE`]
/// 2. resolve the blood — reagents react inside you
/// 3. per reagent in id order: `effects`, plus `overdose_effects` past the
///    threshold, plus `critical_effects` past the critical threshold; purge
///    effects remove active harmful chemicals
/// 4. status damage, then decay every status
/// 5. work off metabolism, firing `after_effects` when a whole dose clears
/// 6. apply damage/healing, oxygen recovery and status-adjusted collapse
///
/// Step 5 follows the active effects on purpose: a reagent with less left than its rate gets
/// one final full tick of effect before it disappears, which is what SS13 does
/// and what stops a 0.1u remainder being silently worthless.
pub fn metabolise(vitals: &mut Vitals, blood: &mut Bloodstream, data: &ChemData) -> TickReport {
    let was_collapsed = vitals.collapsed;
    let critically_injured = vitals.total() >= CRITICAL_DAMAGE;
    let existing_damage = vitals.damage;
    let mut report = TickReport::default();

    // 1. Digestion.
    if !blood.stomach.is_empty() {
        let _ = blood.stomach.transfer_to(&mut blood.blood, DIGESTION_RATE);
    }

    // 2. Reagents react in you.
    report.reactions = resolve(&mut blood.blood, &data.reactions);

    // 3. Per-reagent effects.
    blood.advance_exposure_ticks();
    let present: Vec<(ReagentId, Units)> = blood.blood.iter().collect();
    for (id, volume) in &present {
        let reagent = data.reagents.get(*id);
        let purity = blood.blood.purity_of(*id);
        let ticks_present = blood.ticks_present(*id);

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

        let mut purge = PurgeRequest::default();
        for effect in tiers {
            purge += apply_effect(
                *effect,
                EffectContext {
                    purity,
                    current_volume: *volume,
                    critically_injured,
                    existing_damage,
                    ticks_present,
                },
                blood,
                &mut report,
            );
        }
        purge_harmful(blood, data, purge.harmful, &mut report);
        purge_medicines(blood, data, purge.medicines, &mut report);
        purge_targets(blood, data, &reagent.targeted_purges, purity, &mut report);
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

    // 5. Work off the doses, then fire a reagent's comedown exactly once when
    // neither blood nor stomach contains any of it.
    for (id, _) in present {
        let rate = data.reagents.get(id).rate();
        let purity = blood.blood.purity_of(id);
        let ticks_present = blood.ticks_present(id);
        let metabolised = blood.blood.remove(id, rate);
        if metabolised.is_positive()
            && blood.blood.volume_of(id).is_zero()
            && blood.stomach.volume_of(id).is_zero()
        {
            let reagent = data.reagents.get(id);
            if !reagent.after_effects.is_empty() {
                report.after_effects.push(id);
                let mut purge = PurgeRequest::default();
                for effect in &reagent.after_effects {
                    purge += apply_effect(
                        *effect,
                        EffectContext {
                            purity,
                            current_volume: metabolised,
                            critically_injured,
                            existing_damage,
                            ticks_present,
                        },
                        blood,
                        &mut report,
                    );
                }
                purge_harmful(blood, data, purge.harmful, &mut report);
                purge_medicines(blood, data, purge.medicines, &mut report);
            }
        }
    }
    blood.retain_present_exposure_ticks();

    // Stabilization softens new oxygen damage without erasing existing debt.
    let oxygen_landing = 1.0 - blood.oxygen_resistance();
    report.harmed.oxygen = report
        .harmed
        .oxygen
        .scaled(Units::from_f64(oxygen_landing as f64), Units::ONE);

    vitals.heal(report.healed);
    vitals.apply(report.harmed);

    // 6. Oxygen debt clears on its own.
    if vitals.damage.oxygen.is_positive() {
        vitals.heal(Damage::of(DamageKind::Oxygen, OXYGEN_RECOVERY));
    }

    blood.reconcile_collapse(vitals, was_collapsed);
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
