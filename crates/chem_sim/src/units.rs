//! Fixed-point reagent quantities.
//!
//! Solutions get split, transferred and compared against reaction thresholds
//! constantly. With floats those comparisons drift: a beaker that should hold
//! exactly 15u ends up at 14.999998u and a reaction silently fails to fire.
//! Everything here is stored as hundredths of a unit in an `i32` instead, so
//! equality and threshold checks are exact.

use std::fmt;
use std::iter::Sum;
use std::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};

use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A quantity of reagent, in hundredths of a unit.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Units(i32);

impl Units {
    /// Raw values per whole unit.
    pub const SCALE: i32 = 100;

    pub const ZERO: Units = Units(0);
    pub const ONE: Units = Units(Self::SCALE);

    /// A quantity in raw hundredths.
    pub const fn from_raw(raw: i32) -> Self {
        Units(raw)
    }

    /// A whole number of units.
    pub const fn whole(n: i32) -> Self {
        Units(n * Self::SCALE)
    }

    pub const fn raw(self) -> i32 {
        self.0
    }

    pub fn from_f64(v: f64) -> Self {
        Units((v * Self::SCALE as f64).round() as i32)
    }

    pub fn as_f64(self) -> f64 {
        self.0 as f64 / Self::SCALE as f64
    }

    pub fn as_f32(self) -> f32 {
        self.0 as f32 / Self::SCALE as f32
    }

    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    pub const fn is_positive(self) -> bool {
        self.0 > 0
    }

    pub fn min(self, other: Self) -> Self {
        Units(self.0.min(other.0))
    }

    pub fn max(self, other: Self) -> Self {
        Units(self.0.max(other.0))
    }

    /// Clamps negatives to zero. Handy after subtractions that may undershoot.
    pub fn clamp_non_negative(self) -> Self {
        Units(self.0.max(0))
    }

    /// `self * numerator / denominator`, computed in `i64` so intermediate
    /// products cannot overflow. Truncates toward zero.
    pub(crate) fn scaled(self, numerator: Units, denominator: Units) -> Self {
        debug_assert!(denominator.is_positive(), "scaling by a zero denominator");
        if denominator.is_zero() {
            return Units::ZERO;
        }
        let value = self.0 as i64 * numerator.0 as i64 / denominator.0 as i64;
        Units(value.clamp(i32::MIN as i64, i32::MAX as i64) as i32)
    }
}

impl Add for Units {
    type Output = Units;
    fn add(self, rhs: Units) -> Units {
        Units(self.0 + rhs.0)
    }
}

impl Sub for Units {
    type Output = Units;
    fn sub(self, rhs: Units) -> Units {
        Units(self.0 - rhs.0)
    }
}

impl Neg for Units {
    type Output = Units;
    fn neg(self) -> Units {
        Units(-self.0)
    }
}

impl Mul<i32> for Units {
    type Output = Units;
    fn mul(self, rhs: i32) -> Units {
        Units(self.0 * rhs)
    }
}

impl AddAssign for Units {
    fn add_assign(&mut self, rhs: Units) {
        self.0 += rhs.0;
    }
}

impl SubAssign for Units {
    fn sub_assign(&mut self, rhs: Units) {
        self.0 -= rhs.0;
    }
}

impl Sum for Units {
    fn sum<I: Iterator<Item = Units>>(iter: I) -> Units {
        iter.fold(Units::ZERO, |a, b| a + b)
    }
}

impl fmt::Display for Units {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.0 < 0 { "-" } else { "" };
        let magnitude = self.0.unsigned_abs();
        let whole = magnitude / Self::SCALE as u32;
        let frac = magnitude % Self::SCALE as u32;
        if frac == 0 {
            write!(f, "{sign}{whole}u")
        } else if frac.is_multiple_of(10) {
            write!(f, "{sign}{whole}.{}u", frac / 10)
        } else {
            write!(f, "{sign}{whole}.{frac:02}u")
        }
    }
}

// Test failures read far better as `15u` than `Units(1500)`.
impl fmt::Debug for Units {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

// Data files write plain numbers (`15` or `15.5`), not raw hundredths.
impl Serialize for Units {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.as_f64())
    }
}

impl<'de> Deserialize<'de> for Units {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UnitsVisitor;

        impl Visitor<'_> for UnitsVisitor {
            type Value = Units;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a reagent quantity, e.g. 15 or 0.5")
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Units, E> {
                Ok(Units::from_f64(v))
            }

            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Units, E> {
                Ok(Units::from_f64(v as f64))
            }

            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Units, E> {
                Ok(Units::from_f64(v as f64))
            }
        }

        deserializer.deserialize_any(UnitsVisitor)
    }
}

/// Temperature in kelvin. Only ever compared against reaction thresholds, so
/// it does not need the exactness `Units` does.
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug, Serialize, Deserialize)]
pub struct Kelvin(pub f32);

impl Kelvin {
    /// Room temperature: what a fresh beaker sits at.
    pub const AMBIENT: Kelvin = Kelvin(293.15);
}

impl Default for Kelvin {
    fn default() -> Self {
        Kelvin::AMBIENT
    }
}

impl fmt::Display for Kelvin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.1}K", self.0)
    }
}
