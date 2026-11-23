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
//! # The text behind the window
//!
//! A byte range is not text: the bytes live in the resident set and in the pending buffer, and
//! which is which is the register's answer, not the leaf's (a seal renames a source without
//! touching a leaf). [`window_text`] is the other half of the join — it hands the range to the
//! document's own reader and returns the [`WindowText`] a layout takes, so the Eye never decrypts
//! anything itself and never holds text that is not on screen. What it costs is the window's
//! bytes: a three-leaf screen faults three segments whether the document has eight leaves or four
//! thousand and ninety-six, which is the caller Block 5's second gate names (AUDIT OQ-12). What is
//! still owed is that gate's clock, and that needs a real shaper (Block 6), not this join.
//!
//! Two edges are worth stating. A **pending leaf** is in the window like any other, because it is
//! a leaf: `window_text` reads it out of the pending buffer, which is what makes typing visible
//! without the Eye knowing that buffers exist. And a **window edge inside a character** is
//! refused rather than decoded, because leaf edges are only character boundaries where the cut
//! that made them was (OQ-15) — the reader is where that asymmetry stops being an assumption.

use holonomy_text::select::CopyError;
use holonomy_text::{copy, Draft, OpenSources, ResidentSet, SegmentSource, Selection, Spine};

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
    /// A window over `start..end` that claims **no** leaves.
    ///
    /// For a caller that derived the byte range itself: a bootstrap before any height has been
    /// measured, or a scan for the cluster boundary beside the caret. [`Self::leaves`] is then
    /// empty, so nothing should write heights back from it.
    #[must_use]
    pub fn unclaimed(start: u64, end: u64) -> Self {
        Self {
            leaves: Window::empty(),
            start,
            end,
        }
    }

    /// A window over the leaves `first..end`, and the bytes they hold.
    ///
    /// For a caller that has chosen leaves rather than a scroll position: the shell's bootstrap,
    /// before any height has been measured, knows only that a screen starts at the document's first
    /// leaf. The vertical edges are zero because nothing has measured them yet — which is the same
    /// thing the height index says about those leaves.
    #[must_use]
    pub fn leaves_of(spine: &Spine, first: u32, end: u32) -> Self {
        let leaves = Window::of_leaves(first as usize, end as usize);
        let Some(head) = spine.slab_span(first) else {
            return Self {
                leaves,
                start: spine.len(),
                end: spine.len(),
            };
        };
        let start = head.offset();
        let end_byte = if end > first {
            spine
                .slab_span(end - 1)
                .map_or(start, |span| span.offset() + span.len())
        } else {
            start
        };
        Self {
            leaves,
            start,
            end: end_byte.min(spine.len()),
        }
    }

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

/// Why a window's text could not be produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowTextError {
    /// The bytes behind the window could not be read: a leaf named a source the register does not
    /// know, the resident set refused a fault-in, or a leaf claims more bytes than its source has.
    Read(CopyError),
    /// The window's bytes are not valid UTF-8, because an edge falls inside a character.
    ///
    /// This is OQ-15's asymmetry, detected rather than assumed away: the window's edges come from
    /// leaf edges, and a leaf edge is only guaranteed to be a character boundary where the cut that
    /// made it happened to be one. `at` is the document offset of the first byte that cannot start
    /// a character.
    NotText {
        /// The document offset of the offending byte.
        at: u64,
    },
}

impl std::fmt::Display for WindowTextError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(error) => write!(formatter, "{error}"),
            Self::NotText { at } => write!(formatter, "the window's bytes at {at} are not text"),
        }
    }
}

impl std::error::Error for WindowTextError {}

impl From<CopyError> for WindowTextError {
    fn from(error: CopyError) -> Self {
        Self::Read(error)
    }
}

/// The text behind a [`ShapedWindow`], and the document offset it starts at.
///
/// Owned, and deliberately so: a window is contiguous in the document but not in memory — its bytes
/// are spread over the leaves the viewport covers, in the resident set and in the pending buffer —
/// so assembling them is a copy whose size is the window's, never the document's. That is the whole
/// bond: the cost of reading what is on screen is a function of the screen (L-2), which is why this
/// is the caller Block 5's second gate was missing, rather than a second place text can live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowText {
    text: String,
    origin: u64,
    leaves: Window,
}

impl WindowText {
    /// The window's text, to hand to a layout with [`Self::origin`] as its document offset.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The document offset of the first byte — what [`crate::layout_window`] wants as `origin`.
    #[must_use]
    pub fn origin(&self) -> u64 {
        self.origin
    }

    /// The document offset one past the last byte.
    #[must_use]
    pub fn end(&self) -> u64 {
        self.origin + self.len()
    }

    /// How many bytes the text holds.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.text.len() as u64
    }

    /// Whether the window covered no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The leaves the window covers, so a caller can write their measured heights back.
    #[must_use]
    pub fn leaves(&self) -> Window {
        self.leaves
    }

    /// Volatile-zeroes this window's assembled plaintext capacity in place (`L-7`).
    pub fn wipe_in_place(&mut self) -> usize {
        // SAFETY: `wipe_byte_vec` clears the vector to length `0`, which is valid UTF-8.
        wipe_byte_vec(unsafe { self.text.as_mut_vec() })
    }
}

impl Drop for WindowText {
    fn drop(&mut self) {
        let _ = self.wipe_in_place();
    }
}

fn wipe_byte_vec(buf: &mut Vec<u8>) -> usize {
    let cap = buf.capacity();
    let ptr = buf.as_mut_ptr();
    for offset in 0..cap {
        // SAFETY: `buf` owns `cap` allocated bytes.
        unsafe { ptr.add(offset).write_volatile(0) };
    }
    buf.clear();
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    cap
}

/// Reads the text behind `window`, faulting in exactly the leaves the viewport covers.
///
/// This is the join the Eye was missing: [`byte_window`] answers *which bytes* are on screen from
/// the height index and the spine alone, and this answers *what those bytes say*. Both halves are
/// viewport-sized, so the pair is the caller Block 5's second gate names — though not its clock,
/// which needs a real shaper (AUDIT OQ-12).
///
/// `residency`, `sources` and `open` are the session's: a leaf names a source, only the register
/// knows whether that source is the pending buffer or a segment in the world-line, and only the
/// resident set can fault a segment in. A leaf already resident is a hit, so scrolling back over
/// ground the viewport has covered costs no source call.
///
/// # Errors
///
/// [`WindowTextError::Read`] for anything [`copy`] refuses, and [`WindowTextError::NotText`] when
/// the window's bytes are not valid UTF-8 — an edge inside a character (OQ-15).
pub fn window_text<S: SegmentSource>(
    residency: &mut ResidentSet<S>,
    spine: &Spine,
    draft: &Draft,
    sources: &OpenSources,
    window: ShapedWindow,
    open: &[u8],
) -> Result<WindowText, WindowTextError> {
    let selection = Selection::new(window.start(), window.end());
    let bytes = copy(residency, spine, draft, sources, selection, open)?;
    match String::from_utf8(bytes) {
        Ok(text) => Ok(WindowText {
            text,
            origin: window.start(),
            leaves: window.leaves(),
        }),
        Err(error) => {
            let valid_up_to = error.utf8_error().valid_up_to() as u64;
            let mut raw = error.into_bytes();
            let _ = wipe_byte_vec(&mut raw);
            Err(WindowTextError::NotText {
                at: window.start() + valid_up_to,
            })
        }
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

    #[test]
    fn window_text_wipe_in_place_zeroes_backing_capacity() {
        let mut wt = WindowText {
            text: String::from("decrypted-viewport-plaintext"),
            origin: 0,
            leaves: Window::empty(),
        };
        let cap = wt.text.capacity();
        let ptr = wt.text.as_ptr();
        assert_eq!(wt.wipe_in_place(), cap);
        assert!(wt.is_empty());
        let mut acc = 0u8;
        for offset in 0..cap {
            // SAFETY: `wt.text` still owns the `cap`-byte allocation until `wt` drops.
            acc |= unsafe { core::ptr::read_volatile(ptr.add(offset)) };
        }
        assert_eq!(acc, 0);
    }
}
