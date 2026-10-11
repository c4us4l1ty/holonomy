//! A **selection**: a byte range of the document, and nothing else.
//!
//! # Why this type holds a range and not two endpoints
//!
//! **The obvious model is `(anchor, focus)` — two offsets, and "is there a selection" is
//! `anchor != focus`.** That model cannot represent a *collapsed* selection, which is a real state:
//! after a shift-click the range is empty and the caret is where it will grow from, and the drag has
//! to remember where it started. A third state, `None`, has to be invented for it, and then there are
//! three things that have to agree — `anchor`, `focus`, and whether an anchor exists — with no
//! compiler help when they disagree.
//!
//! **So the range is the whole model.** [`SelectionRange`] is `start..end` with `start <= end`, and
//! **an empty selection is `None`** rather than a degenerate range. One value, two numbers that are
//! ordered by construction, and no state that can disagree with itself.
//!
//! # Why `start <= end` and not the user's direction
//!
//! **A backwards selection is still a range.** A user dragging from the bottom-right to the
//! top-left gets `start < end` and a caret at `start`; dragging back the other way gets the same range
//! and a caret at `end`. **The direction is not in the range** — it is the difference between where
//! the caret is and where `end` is, both of which the editor already knows. Storing the direction as
//! well would be a third number that could disagree with the other two, which is the thing this design
//! exists to avoid.
//!
//! # Why this lives here and not in the chrome
//!
//! **Because it is a fact about the document, not about a picture of it.** The chrome needs to know
//! which bytes to highlight, and `holonomy-render` already depends on this crate, so `ChromeState`
//! carries an `Option<SelectionRange>` and names the same type. **What the chrome does not do is own
//! one** — a selection the renderer could invent would highlight bytes the user never chose, which is
//! the same class of bug as part 22's `ChromeState::caret_line` disagreeing with `editor.caret()`.

/// A non-empty half-open byte range `[start, end)` of a document.
///
/// **Ordered by construction and normalised on the way in**, so `start <= end` holds for every value
/// that exists — including one built from `(9, 4)` by a caller who had the direction backwards.
///
/// **`Option<SelectionRange>` is how "nothing is selected" is said**, and it is why this type needs no
/// `is_empty`: a zero-length range is not representable, so every `SelectionRange` in the program is a
/// real selection of at least one byte.
///
/// ## Why no byte-length field
///
/// **`len()` is `end - start` in a method rather than a field**, because a third stored number is a
/// third thing that can disagree. The subtraction is one instruction and the field would have to be
/// kept correct across every construction site, including the ones in the tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SelectionRange {
    /// The first selected byte, and `start <= end` always.
    pub start: u32,
    /// One past the last selected byte.
    pub end: u32,
}

impl SelectionRange {
    /// The range between two offsets, **normalised** so `start <= end` whichever way round they came.
    ///
    /// ## Why normalisation belongs here and not at every call site
    ///
    /// **The drag is the caller that gets it backwards.** A press at byte 40 and a release at byte 10
    /// arrive as `(40, 10)` in that order, and a selection that reported `start: 40, end: 10` would be
    /// unusable: every consumer — the highlight, the typed-over range, the styled range — has to
    /// remember to swap them first. Normalising in the constructor means **no consumer ever has a
    /// backwards range to handle**, and there is no code path where one exists.
    ///
    /// ## Why a zero-length range is `None`
    ///
    /// **A click is not a selection.** Press and release on the same byte — the common click, and the
    /// end of every drag that did not move — must leave nothing selected, so that typing replaces
    /// rather than inserts. Returning `None` for `start == end` makes that automatic and makes
    /// `Option<SelectionRange>` mean exactly "there is a selection to act on".
    pub fn between(a: u32, b: u32) -> Option<Self> {
        if a == b {
            return None;
        }
        Some(Self {
            start: a.min(b),
            end: a.max(b),
        })
    }

    /// How many bytes are selected.
    pub const fn len(&self) -> u32 {
        self.end - self.start
    }

    /// Whether `offset` is inside the range. **The end is exclusive**, so the caret at `end` is not in
    /// its own selection — which is what makes a collapsed caret the boundary of a selection rather
    /// than part of it.
    pub const fn contains(&self, offset: u32) -> bool {
        offset >= self.start && offset < self.end
    }

    /// The part of `self` that lies within the line `[from, to)`, or `None` if there is none.
    ///
    /// **This is the whole of selection painting**, and it exists as a method because the one caller
    /// — the body-text emitter — must not re-derive it: a highlight that started a line early or ended
    /// it late would be visible immediately, but the bug it is most likely to have is *clipping at
    /// both ends* when the selection starts above the viewport and ends below it, which is the common
    /// case and needs a gate of its own.
    ///
    /// **Returns offsets into the line's own range**, because the caller converts to cells and needs
    /// the answer relative to where the line starts.
    pub const fn clip_to(&self, from: u32, to: u32) -> Option<(u32, u32)> {
        let lo = if self.start > from { self.start } else { from };
        let hi = if self.end < to { self.end } else { to };
        if hi <= lo {
            None
        } else {
            Some((lo, hi))
        }
    }
}
