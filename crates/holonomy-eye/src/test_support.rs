//! The fake shaper the Eye's tests share, so a property test needs no font and no GPU.
//!
//! Advances are deliberately uneven — wide `m` and `中`, narrow `i` and `l` — because proportional
//! advances are the only arithmetic the crate permits, and a monospace fake would hide exactly the
//! bug this block exists to prevent.

use crate::geom::Fixed;
use crate::layout::{layout_window, Layout};
use crate::shaper::Shaper;

/// A deterministic fake: `m` and `中` are wide, `i` and `l` narrow, everything else one unit.
pub(crate) struct Fake {
    line_height: Fixed,
}

impl Fake {
    pub(crate) fn new() -> Self {
        Self {
            line_height: Fixed::units(20),
        }
    }
}

impl Shaper for Fake {
    fn advance(&self, cluster: &str) -> Fixed {
        match cluster.chars().next() {
            Some('m') | Some('中') => Fixed::units(2),
            Some('i') | Some('l') => Fixed::from_64ths(32),
            _ => Fixed::ONE,
        }
    }

    fn line_height(&self) -> Fixed {
        self.line_height
    }

    fn ascent(&self) -> Fixed {
        Fixed::units(15)
    }
}

/// Lays `text` out at `width_units` with [`Fake`], starting at document byte zero.
pub(crate) fn layout(text: &str, width_units: i64) -> Layout<'_> {
    layout_window(text, 0, Fixed::units(width_units), &Fake::new())
}
