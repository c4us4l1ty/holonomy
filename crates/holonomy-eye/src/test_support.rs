//! The fake shaper the Eye's tests share, so a property test needs no font and no GPU.
//!
//! Advances are deliberately uneven — wide `m` and `中`, narrow `i` and `l` — because proportional
//! advances are the only arithmetic the crate permits, and a monospace fake would hide exactly the
//! bug this block exists to prevent.

use crate::geom::Fixed;
use crate::layout::{layout_window, Layout};
use crate::shaper::Shaper;
use crate::style::Style;

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

impl Fake {
    /// A base metric at the style's size, exactly: `base × size / 1000` in 64ths.
    fn scaled(base: Fixed, style: Style) -> Fixed {
        let scaled = base.sixtyfourths() * style.size_milli() / 1000;
        Fixed::from_64ths(scaled)
    }
}

impl Shaper for Fake {
    fn advance(&self, cluster: &str, style: Style) -> Fixed {
        let base = match cluster.chars().next() {
            Some('m') | Some('中') => Fixed::units(2),
            Some('i') | Some('l') => Fixed::from_64ths(32),
            _ => Fixed::ONE,
        };
        Self::scaled(base, style)
    }

    fn line_height(&self, style: Style) -> Fixed {
        Self::scaled(self.line_height, style)
    }

    fn ascent(&self, style: Style) -> Fixed {
        Self::scaled(Fixed::units(15), style)
    }
}

/// Lays `text` out at `width_units` with [`Fake`], starting at document byte zero.
pub(crate) fn layout(text: &str, width_units: i64) -> Layout<'_> {
    layout_window(text, 0, Fixed::units(width_units), &Fake::new())
}
