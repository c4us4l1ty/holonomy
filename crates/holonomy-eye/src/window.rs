//! Putting a viewport on the document: leaves in, bytes out.
//!
//! [`HeightModel`] answers "which leaves are on screen" from measured heights alone; the
//! [`Spine`] answers "where do those leaves live in the document". Neither knows about the other,
//! and this module is the join: one [`Spine::slab_span`] descent per edge, no scan of the document,
//! so finding what to shape costs O(log n) however long the document is. That is the caller Block
//! 5's second gate was missing (AUDIT OQ-12).
//!
//! # The byte range is contiguous, including unmeasured leaves
//!
//! A leaf's height is zero until it has been laid out, and [`HeightModel::leaf_at`] steps over
//! unmeasured leaves rather than pretending they occupy space. That could leave a hole in the
//! shaped text if the range were built leaf by leaf — so it is not: the range runs from the
//! **first** covered leaf's first byte to the **last** covered leaf's last byte. An unmeasured
//! leaf between two measured ones is inside that range and is therefore shaped, which is what
//! makes the window safe to hand to a layout. What the range does *not* cover is an unmeasured
//! prefix or suffix: those leaves are off-window until something measures them, and their scroll
//! positions are provisional until then.
//!
//! # What this does not decide
//!
//! A pending leaf — text that exists in the resident slab but in no spine yet — has no leaf index
//! and no `slab_span`, so it is not in any window this module produces. That is OQ-10, and the
//! decision it needs (commit-and-refault versus split at the caret) belongs to the editor's caret
//! model, not to this join.

use holonomy_text::Spine;

use crate::geom::Fixed;
use crate::heights::{HeightModel, Window};

/// The leaves a viewport covers, and the document bytes they hold.
///
/// `start..end` is the contiguous byte range to shape; `leaves` is the height index's window, kept
/// so a caller can report which leaves it touched and write their measured heights back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShapedWindow {
    leaves: Window,
    start: u64,
    end: u64,
}

impl ShapedWindow {
    /// The height index's window of leaves.
    #[must_use]
    pub fn leaves(&self) -> Window {
        self.leaves
    }

    /// The document offset of the first byte to shape.
    #[must_use]
    pub fn start(&self) -> u64 {
        self.start
    }

    /// The document offset one past the last byte to shape.
    #[must_use]
    pub fn end(&self) -> u64 {
        self.end
    }

    /// How many bytes the window covers.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.end - self.start
    }

    /// Whether the window covers no bytes — a viewport past the end of the document.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// The byte range to shape.
    #[must_use]
    pub fn bytes(&self) -> core::ops::Range<u64> {
        self.start..self.end
    }
}

/// The window of leaves and bytes covering `[top, top + height)` of the laid-out document.
///
/// `top` and `height` are in the same logical units the layout produces, and the model's leaf *i*
/// must be the spine's leaf *i* — one height per leaf, in document order. Debug builds assert that
/// agreement, because a model built against a different spine would silently shape the wrong text.
///
/// A `top` past the document yields an empty window at the document's end, which is where a
/// scrolled caret would sit.
#[must_use]
pub fn byte_window(
    spine: &Spine,
    heights: &HeightModel,
    top: Fixed,
    height: Fixed,
) -> ShapedWindow {
    debug_assert_eq!(
        heights.len() as u32,
        spine.slab_count(),
        "one measured height per leaf, in document order"
    );
    let window = heights.window(top, height);
    if window.is_empty() {
        return ShapedWindow {
            leaves: window,
            start: spine.len(),
            end: spine.len(),
        };
    }
    let first = spine.slab_span(window.first() as u32);
    let last = spine.slab_span(window.end() as u32 - 1);
    let (Some(first), Some(last)) = (first, last) else {
        return ShapedWindow {
            leaves: window,
            start: spine.len(),
            end: spine.len(),
        };
    };
    let start = first.offset();
    let end = (last.offset() + last.len()).min(spine.len());
    ShapedWindow {
        leaves: window,
        start,
        end,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use holonomy_text::{SegmentId, Slab};

    /// A spine of `leaves` leaves, each `bytes` long, one per segment.
    fn spine_of(leaves: u64, bytes: u32) -> Spine {
        let mut spine = Spine::empty();
        for segment in 0..leaves {
            spine = spine.append(Slab::new(SegmentId::new(segment), 0, bytes));
        }
        spine
    }

    /// A model with the same leaf count, every leaf `units` tall.
    fn heights_of(leaves: usize, units: i64) -> HeightModel {
        let mut model = HeightModel::new(leaves);
        for leaf in 0..leaves {
            model.set(leaf, Fixed::units(units));
        }
        model
    }

    #[test]
    fn a_viewport_maps_to_the_leaves_it_covers_and_their_bytes() {
        // Five leaves of 100 bytes, each 10 units tall.
        let spine = spine_of(5, 100);
        let heights = heights_of(5, 10);
        let window = byte_window(&spine, &heights, Fixed::units(15), Fixed::units(10));
        assert_eq!(window.leaves().leaves(), 1..3);
        assert_eq!(window.start(), 100);
        assert_eq!(window.end(), 300);
        assert_eq!(window.len(), 200);
        assert_eq!(window.bytes(), 100..300);
        assert!(!window.is_empty());
    }

    #[test]
    fn a_viewport_past_the_end_is_an_empty_window_at_the_end() {
        let spine = spine_of(3, 40);
        let heights = heights_of(3, 10);
        let window = byte_window(&spine, &heights, Fixed::units(99), Fixed::units(5));
        assert!(window.is_empty());
        assert_eq!(window.start(), 120);
        assert_eq!(window.end(), 120);
        assert_eq!(window.len(), 0);
        // An empty document has no leaves and nothing to shape.
        let empty = byte_window(
            &Spine::empty(),
            &HeightModel::new(0),
            Fixed::ZERO,
            Fixed::ONE,
        );
        assert!(empty.is_empty());
        assert_eq!(empty.start(), 0);
    }

    #[test]
    fn unmeasured_leaves_inside_the_window_are_covered_not_skipped() {
        // Leaf 1 has not been laid out yet: it occupies no height, so a viewport that starts inside
        // leaf 0 and ends inside leaf 2 spans it. Its bytes must be in the range, or the shaped
        // text would have a hole in it.
        let spine = spine_of(3, 100);
        let mut heights = HeightModel::new(3);
        heights.set(0, Fixed::units(10));
        heights.set(2, Fixed::units(10));
        // Leaf 1 sits between the viewport's edges but has no measured height, so it contributes
        // nothing to the scroll position — and everything to the text, once the range spans it.
        let window = byte_window(&spine, &heights, Fixed::units(5), Fixed::units(10));
        assert_eq!(window.leaves().leaves(), 0..3);
        assert_eq!(window.start(), 0);
        assert_eq!(window.end(), 300);
        let unmeasured = spine.slab_span(1).expect("a leaf");
        assert!(unmeasured.offset() >= window.start());
        assert!(unmeasured.offset() + unmeasured.len() <= window.end());
    }

    #[test]
    fn the_window_agrees_with_a_scan_over_random_viewports() {
        let spine = spine_of(20, 50);
        let heights = heights_of(20, 4);
        for step in 0..40i64 {
            let top = Fixed::units(step * 3);
            let window = byte_window(&spine, &heights, top, Fixed::units(9));
            // 20 leaves of 4 units: a 9-unit viewport covers 3 leaves, or 4 if it straddles two
            // boundaries. Every covered leaf must lie inside the byte range, and every byte of the
            // range must belong to a covered leaf.
            for leaf in window.leaves().leaves() {
                let span = spine.slab_span(leaf as u32).expect("a leaf");
                assert!(span.offset() >= window.start());
                assert!(span.offset() + span.len() <= window.end());
            }
            assert!(window.end() <= spine.len());
            assert!(window.start() <= window.end());
        }
    }
}
