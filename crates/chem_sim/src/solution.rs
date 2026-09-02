//! A mixture of reagents in a container.

use serde::{Deserialize, Serialize};

use crate::reagent::{ReagentId, ReagentRegistry};
use crate::units::{Kelvin, Units};

/// Contents of a beaker, machine buffer, pill or bottle.
///
/// Contents are kept sorted by `ReagentId` with no zero entries, so iteration
/// order — and therefore every downstream reaction — is deterministic.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Solution {
    contents: Vec<(ReagentId, Units)>,
    /// Per-reagent quality. Missing entries are legacy/default material at
    /// full purity and neutral pH, keeping old saves and callers compatible.
    #[serde(default)]
    qualities: Vec<(ReagentId, f32, f32)>,
    max_volume: Units,
    pub temperature: Kelvin,
}

impl Solution {
    pub fn new(max_volume: Units) -> Self {
        Solution {
            contents: Vec::new(),
            qualities: Vec::new(),
            max_volume,
            temperature: Kelvin::AMBIENT,
        }
    }

    /// A solution with no capacity limit, for machine buffers and tests.
    pub fn unbounded() -> Self {
        Solution::new(Units::from_raw(i32::MAX))
    }

    pub fn max_volume(&self) -> Units {
        self.max_volume
    }

    /// Changes capacity. Contents beyond the new capacity are spilled and
    /// returned, so callers cannot silently lose reagent.
    pub fn set_max_volume(&mut self, max_volume: Units) -> Units {
        self.max_volume = max_volume;
        let excess = self.total_volume() - max_volume;
        if excess.is_positive() {
            self.take(excess)
        } else {
            Units::ZERO
        }
    }

    pub fn total_volume(&self) -> Units {
        self.contents.iter().map(|(_, amount)| *amount).sum()
    }

    pub fn available_volume(&self) -> Units {
        (self.max_volume - self.total_volume()).clamp_non_negative()
    }

    pub fn is_empty(&self) -> bool {
        self.contents.is_empty()
    }

    pub fn volume_of(&self, id: ReagentId) -> Units {
        match self.contents.binary_search_by_key(&id, |(r, _)| *r) {
            Ok(index) => self.contents[index].1,
            Err(_) => Units::ZERO,
        }
    }

    /// Purity of one reagent portion, from 0.0 to 1.0.
    pub fn purity_of(&self, id: ReagentId) -> f32 {
        if self.volume_of(id).is_zero() {
            return 0.0;
        }
        self.qualities
            .binary_search_by_key(&id, |(reagent, _, _)| *reagent)
            .ok()
            .map(|index| self.qualities[index].1)
            .unwrap_or(1.0)
    }

    /// Volume-weighted purity of the complete mixture.
    pub fn average_purity(&self) -> f32 {
        let total = self.total_volume().as_f32();
        if total <= 0.0 {
            return 1.0;
        }
        self.contents
            .iter()
            .map(|(id, amount)| self.purity_of(*id) * amount.as_f32() / total)
            .sum::<f32>()
            .clamp(0.0, 1.0)
    }

    /// Accessible, volume-weighted pH used by authored reaction ranges.
    pub fn ph(&self) -> f32 {
        let total = self.total_volume().as_f32();
        if total <= 0.0 {
            return 7.0;
        }
        self.contents
            .iter()
            .map(|(id, amount)| self.reagent_ph(*id) * amount.as_f32() / total)
            .sum::<f32>()
            .clamp(0.0, 14.0)
    }

    pub fn reagent_ph(&self, id: ReagentId) -> f32 {
        self.qualities
            .binary_search_by_key(&id, |(reagent, _, _)| *reagent)
            .ok()
            .map(|index| self.qualities[index].2)
            .unwrap_or(7.0)
    }

    pub fn contains_at_least(&self, id: ReagentId, amount: Units) -> bool {
        self.volume_of(id) >= amount
    }

    pub fn iter(&self) -> impl Iterator<Item = (ReagentId, Units)> + '_ {
        self.contents.iter().copied()
    }

    /// Number of distinct reagents present.
    pub fn len(&self) -> usize {
        self.contents.len()
    }

    /// The single reagent present, if the solution is pure.
    pub fn sole_reagent(&self) -> Option<(ReagentId, Units)> {
        match self.contents.as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    }

    /// Adds reagent, up to remaining capacity.
    ///
    /// Returns the amount that did **not** fit. Callers decide whether that is
    /// spillage on the floor or an error message — this type never discards
    /// reagent silently.
    #[must_use = "the returned overflow was not added and will be lost if ignored"]
    pub fn add(&mut self, id: ReagentId, amount: Units) -> Units {
        self.add_profiled(id, amount, 1.0, 7.0)
    }

    /// Adds material with authored purity and pH. When the same reagent is
    /// already present, both values blend by volume rather than allowing the
    /// newest drop to overwrite the whole batch.
    #[must_use = "the returned overflow was not added and will be lost if ignored"]
    pub fn add_profiled(&mut self, id: ReagentId, amount: Units, purity: f32, ph: f32) -> Units {
        if !amount.is_positive() {
            return Units::ZERO;
        }
        let accepted = amount.min(self.available_volume());
        if accepted.is_positive() {
            let previous = self.volume_of(id);
            match self.contents.binary_search_by_key(&id, |(r, _)| *r) {
                Ok(index) => self.contents[index].1 += accepted,
                Err(index) => self.contents.insert(index, (id, accepted)),
            }
            let incoming_purity = purity.clamp(0.0, 1.0);
            let incoming_ph = ph.clamp(0.0, 14.0);
            let combined = previous + accepted;
            let previous_purity = if previous.is_positive() {
                self.purity_of(id)
            } else {
                incoming_purity
            };
            let previous_ph = if previous.is_positive() {
                self.reagent_ph(id)
            } else {
                incoming_ph
            };
            let blend = |old: f32, new: f32| {
                ((old * previous.as_f32()) + (new * accepted.as_f32())) / combined.as_f32()
            };
            let quality = (
                id,
                blend(previous_purity, incoming_purity),
                blend(previous_ph, incoming_ph),
            );
            match self
                .qualities
                .binary_search_by_key(&id, |(reagent, _, _)| *reagent)
            {
                Ok(index) => self.qualities[index] = quality,
                Err(index) => self.qualities.insert(index, quality),
            }
        }
        amount - accepted
    }

    /// Removes up to `amount`. Returns how much was actually removed.
    pub fn remove(&mut self, id: ReagentId, amount: Units) -> Units {
        if !amount.is_positive() {
            return Units::ZERO;
        }
        let Ok(index) = self.contents.binary_search_by_key(&id, |(r, _)| *r) else {
            return Units::ZERO;
        };
        let present = self.contents[index].1;
        let removed = amount.min(present);
        if removed == present {
            self.contents.remove(index);
            if let Ok(quality) = self
                .qualities
                .binary_search_by_key(&id, |(reagent, _, _)| *reagent)
            {
                self.qualities.remove(quality);
            }
        } else {
            self.contents[index].1 -= removed;
        }
        removed
    }

    pub fn clear(&mut self) {
        self.contents.clear();
        self.qualities.clear();
    }

    /// Draws `amount` out of the solution and discards it, returning how much
    /// was drawn. Composition of what remains is unchanged.
    pub fn take(&mut self, amount: Units) -> Units {
        let mut sink = Solution::unbounded();
        self.transfer_to(&mut sink, amount)
    }

    /// Splits `amount` off into a new solution, preserving composition.
    pub fn split(&mut self, amount: Units) -> Solution {
        let mut out = Solution::unbounded();
        out.temperature = self.temperature;
        let _ = self.transfer_to(&mut out, amount);
        out
    }

    /// Pours `amount` into `other`, drawing **proportionally** across contents.
    ///
    /// This is the rule that makes mixed containers behave: you cannot pour
    /// just the oxygen out of a beaker that also holds sugar. Getting it wrong
    /// quietly breaks every multi-step recipe downstream.
    ///
    /// Transfers less than asked if the source is short or the destination is
    /// full. Returns the amount actually moved.
    pub fn transfer_to(&mut self, other: &mut Solution, amount: Units) -> Units {
        let total = self.total_volume();
        if total.is_zero() {
            return Units::ZERO;
        }
        let moving = amount.min(total).min(other.available_volume());
        if !moving.is_positive() {
            return Units::ZERO;
        }
        let old_volume = other.total_volume().as_f32();
        other.temperature.0 = (other.temperature.0 * old_volume
            + self.temperature.0 * moving.as_f32())
            / (old_volume + moving.as_f32());

        // Each reagent's share, floored. Flooring loses under one raw unit per
        // reagent, so the shares can sum to slightly less than requested.
        let mut shares: Vec<(ReagentId, Units)> = self
            .contents
            .iter()
            .map(|&(id, qty)| (id, qty.scaled(moving, total)))
            .collect();

        let allocated: Units = shares.iter().map(|(_, qty)| *qty).sum();
        let mut shortfall = (moving - allocated).raw();

        // Hand the lost remainder back out, one raw unit at a time, to the
        // largest components first. Bounded by the reagent count, and stable
        // ordering keeps it deterministic.
        if shortfall > 0 {
            let mut order: Vec<usize> = (0..shares.len()).collect();
            order.sort_by_key(|&i| std::cmp::Reverse(self.contents[i].1.raw()));
            for &i in &order {
                if shortfall == 0 {
                    break;
                }
                // Never hand back more than the source actually holds.
                if shares[i].1 < self.contents[i].1 {
                    shares[i].1 += Units::from_raw(1);
                    shortfall -= 1;
                }
            }
        }

        let mut moved = Units::ZERO;
        for (id, share) in shares {
            if !share.is_positive() {
                continue;
            }
            let purity = self.purity_of(id);
            let ph = self.reagent_ph(id);
            let removed = self.remove(id, share);
            let overflow = other.add_profiled(id, removed, purity, ph);
            // Destination capacity was checked up front, so this should not
            // trigger; put anything rejected back rather than vanish it.
            if overflow.is_positive() {
                let _ = self.add_profiled(id, overflow, purity, ph);
            }
            moved += removed - overflow;
        }
        moved
    }

    /// Moves the whole solution's pH by a fixed amount. Buffer handling uses
    /// this after consuming its buffer reagent; storing the adjustment on the
    /// portions means it survives splitting, pouring and replication.
    pub fn shift_ph(&mut self, delta: f32) {
        for (id, _) in self.contents.clone() {
            let purity = self.purity_of(id);
            let ph = (self.reagent_ph(id) + delta).clamp(0.0, 14.0);
            let quality = (id, purity, ph);
            match self
                .qualities
                .binary_search_by_key(&id, |(reagent, _, _)| *reagent)
            {
                Ok(index) => self.qualities[index] = quality,
                Err(index) => self.qualities.insert(index, quality),
            }
        }
    }

    /// Volume-weighted blend of the contents' colours. Drives the rendered
    /// liquid colour.
    pub fn color(&self, reagents: &ReagentRegistry) -> [f32; 3] {
        let total = self.total_volume();
        if !total.is_positive() {
            return [0.0, 0.0, 0.0];
        }
        let mut blend = [0.0f32; 3];
        for (id, qty) in self.iter() {
            let weight = qty.as_f32() / total.as_f32();
            let color = reagents.get(id).color;
            for channel in 0..3 {
                blend[channel] += color[channel] * weight;
            }
        }
        blend
    }
}
