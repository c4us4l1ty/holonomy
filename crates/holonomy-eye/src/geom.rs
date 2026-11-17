//! Integer geometry: one arithmetic, exact comparisons.
//!
//! Layout is integer arithmetic in **1/64th of a logical unit** — the fixed-point grain FreeType
//! uses, chosen here for a different reason. GATES law L-8 compares positions for equality
//! (`hit_test(paint_point(glyph)) == glyph`, at every zoom and DPI), and floating point would make
//! "the same point" a question about rounding instead of a question about geometry: paint would
//! round one way and hit-test the other, and the seam would reopen in the arithmetic itself. Golden
//! tests have the same need — a coordinate is either the number in the file or the test fails,
//! never "close enough on this machine".
//!
//! Zoom and DPI are therefore exact scaling of integers (Block 5's later slice owns the transform),
//! and a 64th is finer than any metric a shaper will hand back.

use core::iter::Sum;
use core::ops::{Add, AddAssign, Sub, SubAssign};

/// A length or coordinate in 1/64th of a logical unit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Fixed(i64);

impl Fixed {
    /// Zero.
    pub const ZERO: Self = Self(0);

    /// One logical unit.
    pub const ONE: Self = Self(64);

    /// Builds a length from 64ths.
    #[must_use]
    pub const fn from_64ths(value: i64) -> Self {
        Self(value)
    }

    /// Builds a length from whole logical units.
    #[must_use]
    pub const fn units(value: i64) -> Self {
        Self(value * 64)
    }

    /// The raw 64ths.
    #[must_use]
    pub const fn sixtyfourths(self) -> i64 {
        self.0
    }

    /// Whether this length is zero.
    #[must_use]
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// The length as a float, for the paint layer that has to hand it to a GPU.
    ///
    /// This is the only float in the crate, it is lossy on purpose, and nothing compares against
    /// it: paint scales and draws, hit-test never sees it.
    #[must_use]
    pub fn to_f32(self) -> f32 {
        self.0 as f32 / 64.0
    }
}

impl Add for Fixed {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self(self.0 + other.0)
    }
}

impl Sub for Fixed {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self(self.0 - other.0)
    }
}

impl AddAssign for Fixed {
    fn add_assign(&mut self, other: Self) {
        self.0 += other.0;
    }
}

impl SubAssign for Fixed {
    fn sub_assign(&mut self, other: Self) {
        self.0 -= other.0;
    }
}

impl Sum for Fixed {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, Add::add)
    }
}

/// A point in logical space: `x` to the right, `y` downward.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Point {
    /// Horizontal position, from the left edge of the window.
    pub x: Fixed,
    /// Vertical position, from the top of the content laid out.
    pub y: Fixed,
}

impl Point {
    /// The origin.
    pub const ORIGIN: Self = Self {
        x: Fixed::ZERO,
        y: Fixed::ZERO,
    };

    /// A point at `(x, y)`.
    #[must_use]
    pub const fn new(x: Fixed, y: Fixed) -> Self {
        Self { x, y }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_and_64ths_agree() {
        assert_eq!(Fixed::units(3), Fixed::from_64ths(192));
        assert_eq!(Fixed::units(3).sixtyfourths(), 192);
        assert_eq!(Fixed::ONE, Fixed::units(1));
    }

    #[test]
    fn arithmetic_is_exact_and_ordered() {
        let a = Fixed::from_64ths(96);
        let b = Fixed::from_64ths(32);
        assert_eq!(a - b, Fixed::ONE);
        assert_eq!(b + b, Fixed::ONE);
        assert!(b < a);
        let mut total = Fixed::ZERO;
        for _ in 0..64 {
            total += Fixed::ONE;
        }
        assert_eq!(total, Fixed::units(64));
    }

    #[test]
    fn only_the_paint_side_converts_to_float() {
        assert!((Fixed::units(2).to_f32() - 2.0).abs() < f32::EPSILON);
        assert_eq!(Fixed::from_64ths(32).to_f32(), 0.5);
    }
}
