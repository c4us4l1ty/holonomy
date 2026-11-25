//! The Spine: the immutable, path-copied document tree.
//!
//! A [`Spine`] is a sequence of [`Slab`] references — geometry only, never plaintext. Every edit
//! returns a **new** spine and leaves the old one valid, so history is free: [`History`] keeps
//! roots and undo is a pointer swap (RFC-003 §2). Because nodes are shared through `Arc`, an
//! edit allocates O(log N) nodes regardless of how long the document has grown, and the wipe set
//! stays viewport-sized because the spine holds no readable bytes at all.
//!
//! Position math (byte offset → slab, viewport → spans) is a descent over cached subtree
//! aggregates: O(log N). RFC-003 named a Fenwick tree for this; the subtree aggregates carry the
//! same prefix sums with one index instead of two, which is the K-8 lesson applied to indexing —
//! a second structure that must be rebuilt and can drift is the two-geometry disease in disguise
//! (recorded in `AUDIT.md`, 2026-11-17).

use std::sync::Arc;

use crate::formatting::{DocStyle, DocStyleSpan, StyleMap, StylePatch, ToolbarState};
use crate::slab::{Slab, SlabHit, SlabSpan};
use crate::tree::{self, Node};

/// Structural diff classification between two [`Spine`] trees without reading plaintext.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpineDiffKind {
    /// Bytes were added in `other` that did not appear in `self`.
    Added,
    /// Bytes were removed from `self` and do not appear in `other`.
    Removed,
    /// Bytes at `old_range` in `self` were replaced by `new_range` in `other`.
    Replaced,
}

/// One structural diff hunk between two [`Spine`] revisions (`Block 8`).
///
/// Computed purely from [`Slab`] descriptors `(segment, start, len)` and document offsets —
/// zero plaintext segments are decrypted to compute diffs, keeping history views bounded under
/// `L-2`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpineDiffHunk {
    /// Whether this hunk is an addition, deletion, or replacement.
    pub kind: SpineDiffKind,
    /// Byte range `(start, end)` in the older spine.
    pub old_range: (u64, u64),
    /// Byte range `(start, end)` in the newer spine.
    pub new_range: (u64, u64),
    /// Slabs covered in the older spine (`empty` for [`SpineDiffKind::Added`]).
    pub old_slabs: Vec<Slab>,
    /// Slabs covered in the newer spine (`empty` for [`SpineDiffKind::Removed`]).
    pub new_slabs: Vec<Slab>,
}

/// Errors from structural spine operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpineError {
    /// An offset lies beyond the document's last byte.
    OffsetOutOfRange {
        /// The requested offset.
        offset: u64,
        /// The document length it was checked against.
        len: u64,
    },
    /// A range is inverted or extends beyond the document.
    RangeOutOfRange {
        /// The requested start.
        start: u64,
        /// The requested end (exclusive).
        end: u64,
        /// The document length they were checked against.
        len: u64,
    },
}

impl std::fmt::Display for SpineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OffsetOutOfRange { offset, len } => {
                write!(
                    formatter,
                    "offset {offset} is outside a {len}-byte document"
                )
            }
            Self::RangeOutOfRange { start, end, len } => write!(
                formatter,
                "range {start}..{end} is invalid for a {len}-byte document"
            ),
        }
    }
}

impl std::error::Error for SpineError {}

/// An immutable sequence of slabs describing a document, plus its path-copied style layer.
#[derive(Debug, Clone)]
pub struct Spine {
    root: Arc<Node>,
    styles: StyleMap,
}

impl Default for Spine {
    fn default() -> Self {
        Self::empty()
    }
}

impl Spine {
    /// The empty document.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            root: tree::empty(),
            styles: StyleMap::empty(),
        }
    }

    /// Builds a spine from slabs in document order, in O(n).
    ///
    /// Neighbours that name contiguous bytes of one segment become **one leaf**: nothing in the
    /// document separates them, and a leaf boundary is an accounting unit rather than a semantic
    /// one — every leaf costs a height-model row, a residency slot and a read. A document rebuilt
    /// from a log whose geometry earlier edits fragmented would otherwise carry that fragmentation
    /// forever, which is AUDIT OQ-9, and this is its load-time half.
    ///
    /// Merging cannot overflow a slab's range: the second slab is contiguous and already valid, so
    /// its `start + len` is the merged `start + len`. Nothing else may move — a different segment
    /// is a different plaintext, and a gap is a range the document does not name — so the pass only
    /// ever looks at the immediately preceding slab. A loaded spine therefore has the invariant
    /// that no two adjacent leaves name contiguous bytes of one segment.
    #[must_use]
    pub fn from_slabs<I>(slabs: I) -> Self
    where
        I: IntoIterator<Item = Slab>,
    {
        let mut coalesced: Vec<Slab> = Vec::new();
        let mut open: Option<Slab> = None;
        for slab in slabs {
            // A zero-byte leaf carries nothing but would inflate slab counts and path copies; the
            // document is unchanged by definition, so it is dropped at the door.
            if slab.is_empty() {
                continue;
            }
            open = match open {
                Some(tail) if tail.segment() == slab.segment() && tail.end() == slab.start() => {
                    Some(Slab::new(
                        tail.segment(),
                        tail.start(),
                        tail.len() + slab.len(),
                    ))
                }
                Some(tail) => {
                    coalesced.push(tail);
                    Some(slab)
                }
                None => Some(slab),
            };
        }
        coalesced.extend(open);
        Self {
            root: tree::build_balanced(&coalesced),
            styles: StyleMap::empty(),
        }
    }

    /// Builds a spine from slabs and an initial [`StyleMap`], clamping any out-of-range style runs
    /// to the resulting document length.
    #[must_use]
    pub fn from_slabs_and_styles<I>(slabs: I, styles: StyleMap) -> Self
    where
        I: IntoIterator<Item = Slab>,
    {
        let mut spine = Self::from_slabs(slabs);
        if !styles.is_empty() {
            let (clamped, _) = styles.split(spine.len());
            spine.styles = clamped;
        }
        spine
    }

    /// Whether `self` and `other` share the exact same root node and style map in O(1).
    #[must_use]
    pub fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.root, &other.root) && self.styles.ptr_eq(&other.styles)
    }

    /// The path-copied rich-text [`StyleMap`] carried by this spine.
    #[must_use]
    pub fn styles(&self) -> &StyleMap {
        &self.styles
    }

    /// Returns a copy of this spine carrying `styles` (clamped to `0..self.len()`).
    #[must_use]
    pub fn with_styles(&self, styles: StyleMap) -> Self {
        let clamped = if styles.is_empty() {
            StyleMap::empty()
        } else {
            styles.split(self.len()).0
        };
        Self {
            root: Arc::clone(&self.root),
            styles: clamped,
        }
    }

    /// Active [`DocStyle`] at caret `offset`.
    #[must_use]
    pub fn style_at(&self, offset: u64) -> DocStyle {
        self.styles.style_at(offset.min(self.len()))
    }

    /// Non-default [`DocStyleSpan`]s overlapping `[start, end)`, clipped to `[start, end)`.
    #[must_use]
    pub fn style_spans_in(&self, start: u64, end: u64) -> Vec<DocStyleSpan> {
        self.styles.spans_in(start, end.min(self.len()))
    }

    /// Computes [`ToolbarState`] for caret (`start == end`) or selection `[start, end)`.
    #[must_use]
    pub fn toolbar_state(&self, start: u64, end: u64) -> ToolbarState {
        self.styles.toolbar_state(self.len(), start, end)
    }

    /// Applies `patch` over `[start, end)` without cutting any underlying [`Slab`] leaf (`OQ-9`).
    pub fn apply_style(
        &self,
        start: u64,
        end: u64,
        patch: &StylePatch,
    ) -> Result<Self, SpineError> {
        let styles = self.styles.apply_patch(self.len(), start, end, patch)?;
        Ok(Self {
            root: Arc::clone(&self.root),
            styles,
        })
    }

    /// Sets or clears (`url = None`) a link target over `[start, end)`.
    pub fn set_link(&self, start: u64, end: u64, url: Option<&str>) -> Result<Self, SpineError> {
        let styles = self.styles.set_link(self.len(), start, end, url)?;
        Ok(Self {
            root: Arc::clone(&self.root),
            styles,
        })
    }

    /// The document length in bytes.
    #[must_use]
    pub fn len(&self) -> u64 {
        self.root.bytes()
    }

    /// Whether the document has no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.root.bytes() == 0
    }

    /// The number of slabs (not segments: an edited segment may span several slabs).
    #[must_use]
    pub fn slab_count(&self) -> u32 {
        self.root.slabs()
    }

    /// Appends one slab, path-copying O(log N) nodes.
    ///
    /// An empty slab is a no-op: it would occupy a node without carrying a byte.
    #[must_use]
    pub fn append(&self, slab: Slab) -> Self {
        if slab.is_empty() {
            return self.clone();
        }
        Self {
            root: tree::join(Arc::clone(&self.root), tree::leaf(slab)),
            styles: self.styles.clone(),
        }
    }

    /// Concatenates `other` after this spine, path-copying O(log N) nodes.
    ///
    /// Both spines stay valid afterwards, which is what makes split-and-rejoin (the shape an
    /// editor uses when it isolates the viewport, or when it pastes a cut selection back) cost
    /// the seam rather than the document.
    #[must_use]
    pub fn concat(&self, other: &Self) -> Self {
        Self {
            root: tree::join(Arc::clone(&self.root), Arc::clone(&other.root)),
            styles: self.styles.concat(self.len(), &other.styles),
        }
    }

    /// Inserts a slab at a byte offset, cutting the slab that contains that offset if necessary.
    ///
    /// Structurally a cut is legal — the two halves name adjacent ranges of the same segment —
    /// but whether the resulting document is *meaningful* is the caller's business: the Spine
    /// stores geometry, and it is the resident plaintext set that decides what the bytes mean.
    pub fn insert_slab(&self, at: u64, slab: Slab) -> Result<Self, SpineError> {
        if at > self.len() {
            return Err(SpineError::OffsetOutOfRange {
                offset: at,
                len: self.len(),
            });
        }
        if slab.is_empty() {
            return Ok(self.clone());
        }
        let (before, after) = tree::split(&self.root, at);
        Ok(Self {
            root: tree::join(tree::join(before, tree::leaf(slab)), after),
            styles: self.styles.insert_gap(at, u64::from(slab.len())),
        })
    }

    /// Removes the byte range `start..end`, path-copying O(log N) nodes.
    ///
    /// The removed slabs are not returned: call [`Self::spans_in`] first if the caller needs the
    /// bytes they name. Undo does not need them — it keeps the previous root.
    pub fn remove_range(&self, start: u64, end: u64) -> Result<Self, SpineError> {
        if start > end || end > self.len() {
            return Err(SpineError::RangeOutOfRange {
                start,
                end,
                len: self.len(),
            });
        }
        let (before, rest) = tree::split(&self.root, start);
        let (_removed, after) = tree::split(&rest, end - start);
        Ok(Self {
            root: tree::join(before, after),
            styles: self.styles.remove_range(start, end),
        })
    }

    /// Replaces the byte range `start..end` with one slab, path-copying O(log N) nodes.
    ///
    /// This is the commit boundary's structural half: the range a re-sealed slab occupied is
    /// swapped for a leaf naming the new segment, and no offset outside the range moves. An empty
    /// slab is equivalent to removing the range — the tree may not hold a leaf that carries
    /// nothing.
    pub fn replace_range(&self, start: u64, end: u64, slab: Slab) -> Result<Self, SpineError> {
        if start > end || end > self.len() {
            return Err(SpineError::RangeOutOfRange {
                start,
                end,
                len: self.len(),
            });
        }
        let (before, rest) = tree::split(&self.root, start);
        let (_replaced, after) = tree::split(&rest, end - start);
        let middle = if slab.is_empty() {
            tree::empty()
        } else {
            tree::leaf(slab)
        };
        Ok(Self {
            root: tree::join(tree::join(before, middle), after),
            styles: self.styles.replace_range(start, end, u64::from(slab.len())),
        })
    }

    /// Splits the document into `0..at` and `at..len`.
    pub fn split_at(&self, at: u64) -> Result<(Self, Self), SpineError> {
        if at > self.len() {
            return Err(SpineError::OffsetOutOfRange {
                offset: at,
                len: self.len(),
            });
        }
        let (before, after) = tree::split(&self.root, at);
        let (left_styles, right_styles) = self.styles.split(at);
        Ok((
            Self {
                root: before,
                styles: left_styles,
            },
            Self {
                root: after,
                styles: right_styles,
            },
        ))
    }

    /// Finds the slab containing `offset`, in O(log N), or `None` past the end.
    #[must_use]
    pub fn locate(&self, offset: u64) -> Option<SlabHit> {
        if offset >= self.len() {
            return None;
        }
        locate_in(&self.root, offset)
    }

    /// The slab at `index` in document order, with the document offset of its first byte.
    ///
    /// A height index is keyed by leaf position — `holonomy-eye`'s `HeightModel` records one
    /// measured height per leaf — so this is where a window of leaves becomes byte ranges: one
    /// descent over cached slab counts, O(log N) whatever the document's size. Without it the Eye
    /// would have to scan `spans_in` to learn which leaf sits at a given position, and the cost of
    /// finding the viewport would grow with the document, which is the thing Block 5's second gate
    /// forbids.
    #[must_use]
    pub fn slab_span(&self, index: u32) -> Option<SlabSpan> {
        if index >= self.slab_count() {
            return None;
        }
        slab_span_in(&self.root, index, 0)
    }

    /// The slabs overlapping `start..end`, clipped to it, with their document offsets.
    ///
    /// The returned vector is proportional to the **viewport's** span count, not the document's:
    /// this is the call the Eye makes for what is on screen.
    #[must_use]
    pub fn spans_in(&self, start: u64, end: u64) -> Vec<SlabSpan> {
        let mut spans = Vec::new();
        let end = end.min(self.len());
        if start < end {
            collect(&self.root, 0, start, end, &mut spans);
        }
        spans
    }

    /// Merges any adjacent leaves naming contiguous byte ranges of the same segment (`OQ-9`),
    /// returning `self.clone()` in O(1) when the tree is already maximally coalesced.
    #[must_use]
    pub fn coalesce(&self) -> Self {
        let count = self.slab_count();
        if count <= 1 {
            return self.clone();
        }
        let mut can_merge = false;
        let mut prev: Option<Slab> = None;
        let mut idx = 0u32;
        while idx < count {
            if let Some(span) = self.slab_span(idx) {
                let slab = span.slab();
                if let Some(p) = prev {
                    if p.segment() == slab.segment()
                        && p.end() == slab.start()
                        && p.len().checked_add(slab.len()).is_some()
                    {
                        can_merge = true;
                        break;
                    }
                }
                prev = Some(slab);
            }
            idx += 1;
        }
        if !can_merge {
            return self.clone();
        }
        let slabs = self
            .spans_in(0, self.len())
            .into_iter()
            .map(|span| span.slab());
        Self::from_slabs_and_styles(slabs, self.styles.clone())
    }

    /// Computes a structural diff between `self` (older) and `other` (newer) purely from slab
    /// identity and offsets without decrypting any segment plaintext (`Block 8`).
    #[must_use]
    pub fn diff_slabs(&self, other: &Self) -> Vec<SpineDiffHunk> {
        if self.ptr_eq(other) {
            return Vec::new();
        }
        let old_slabs = coalesced_slabs_with_offsets(self);
        let new_slabs = coalesced_slabs_with_offsets(other);

        // Trim identical prefix and suffix slabs first so localized edits across 100k-slab
        // documents stay proportional to the changed window.
        let mut prefix = 0usize;
        let min_len = old_slabs.len().min(new_slabs.len());
        while prefix < min_len && old_slabs[prefix].slab == new_slabs[prefix].slab {
            prefix += 1;
        }
        let mut old_end = old_slabs.len();
        let mut new_end = new_slabs.len();
        while old_end > prefix
            && new_end > prefix
            && old_slabs[old_end - 1].slab == new_slabs[new_end - 1].slab
        {
            old_end -= 1;
            new_end -= 1;
        }

        let old_mid = &old_slabs[prefix..old_end];
        let new_mid = &new_slabs[prefix..new_end];
        if old_mid.is_empty() && new_mid.is_empty() {
            return Vec::new();
        }

        // Fast exact LCS when the middle spans fit within a small matrix budget; otherwise emit
        // one coarse replacement/addition/removal hunk covering the unmatched middle.
        let ops = if old_mid.len().saturating_mul(new_mid.len()) <= 65_536 {
            lcs_ops(old_mid, new_mid)
        } else {
            vec![DiffOp::Mismatch(0..old_mid.len(), 0..new_mid.len())]
        };

        let mut hunks = Vec::new();
        let mut old_anchor = if prefix == 0 {
            0
        } else {
            old_slabs[prefix - 1].offset + u64::from(old_slabs[prefix - 1].slab.len())
        };
        let mut new_anchor = if prefix == 0 {
            0
        } else {
            new_slabs[prefix - 1].offset + u64::from(new_slabs[prefix - 1].slab.len())
        };

        for op in ops {
            match op {
                DiffOp::Match(o_idx, n_idx) => {
                    old_anchor = old_mid[o_idx].offset + u64::from(old_mid[o_idx].slab.len());
                    new_anchor = new_mid[n_idx].offset + u64::from(new_mid[n_idx].slab.len());
                }
                DiffOp::Mismatch(o_range, n_range) => {
                    let o_slice = &old_mid[o_range];
                    let n_slice = &new_mid[n_range];
                    let old_range = if o_slice.is_empty() {
                        (old_anchor, old_anchor)
                    } else {
                        let s = o_slice[0].offset;
                        let last = &o_slice[o_slice.len() - 1];
                        let e = last.offset + u64::from(last.slab.len());
                        old_anchor = e;
                        (s, e)
                    };
                    let new_range = if n_slice.is_empty() {
                        (new_anchor, new_anchor)
                    } else {
                        let s = n_slice[0].offset;
                        let last = &n_slice[n_slice.len() - 1];
                        let e = last.offset + u64::from(last.slab.len());
                        new_anchor = e;
                        (s, e)
                    };
                    let kind = match (o_slice.is_empty(), n_slice.is_empty()) {
                        (true, false) => SpineDiffKind::Added,
                        (false, true) => SpineDiffKind::Removed,
                        _ => SpineDiffKind::Replaced,
                    };
                    hunks.push(SpineDiffHunk {
                        kind,
                        old_range,
                        new_range,
                        old_slabs: o_slice.iter().map(|entry| entry.slab).collect(),
                        new_slabs: n_slice.iter().map(|entry| entry.slab).collect(),
                    });
                }
            }
        }
        hunks
    }
}

#[derive(Clone, Copy)]
struct SlabEntry {
    offset: u64,
    slab: Slab,
}

fn coalesced_slabs_with_offsets(spine: &Spine) -> Vec<SlabEntry> {
    let count = spine.slab_count();
    let mut entries: Vec<SlabEntry> = Vec::with_capacity(count as usize);
    for idx in 0..count {
        if let Some(span) = spine.slab_span(idx) {
            let slab = span.slab();
            if slab.is_empty() {
                continue;
            }
            if let Some(last) = entries.last_mut() {
                if last.slab.segment() == slab.segment()
                    && last.slab.end() == slab.start()
                    && last.slab.len().checked_add(slab.len()).is_some()
                {
                    last.slab = Slab::new(
                        last.slab.segment(),
                        last.slab.start(),
                        last.slab.len() + slab.len(),
                    );
                    continue;
                }
            }
            entries.push(SlabEntry {
                offset: span.offset(),
                slab,
            });
        }
    }
    entries
}

enum DiffOp {
    Match(usize, usize),
    Mismatch(std::ops::Range<usize>, std::ops::Range<usize>),
}

fn lcs_ops(old_mid: &[SlabEntry], new_mid: &[SlabEntry]) -> Vec<DiffOp> {
    let m = old_mid.len();
    let n = new_mid.len();
    if m == 0 || n == 0 {
        return vec![DiffOp::Mismatch(0..m, 0..n)];
    }
    let cols = n + 1;
    let mut dp = vec![0u32; (m + 1) * cols];
    for i in (0..m).rev() {
        for j in (0..n).rev() {
            dp[i * cols + j] = if old_mid[i].slab == new_mid[j].slab {
                dp[(i + 1) * cols + (j + 1)] + 1
            } else {
                dp[(i + 1) * cols + j].max(dp[i * cols + (j + 1)])
            };
        }
    }

    let mut ops = Vec::new();
    let mut i = 0;
    let mut j = 0;
    let mut cur_o = 0;
    let mut cur_n = 0;
    while i < m && j < n {
        if old_mid[i].slab == new_mid[j].slab {
            if cur_o < i || cur_n < j {
                ops.push(DiffOp::Mismatch(cur_o..i, cur_n..j));
            }
            ops.push(DiffOp::Match(i, j));
            i += 1;
            j += 1;
            cur_o = i;
            cur_n = j;
        } else if dp[(i + 1) * cols + j] >= dp[i * cols + (j + 1)] {
            i += 1;
        } else {
            j += 1;
        }
    }
    if cur_o < m || cur_n < n {
        ops.push(DiffOp::Mismatch(cur_o..m, cur_n..n));
    }
    ops
}

fn slab_span_in(node: &Arc<Node>, index: u32, base: u64) -> Option<SlabSpan> {
    match node.as_ref() {
        Node::Empty => None,
        Node::Leaf(slab) => Some(SlabSpan::new(base, *slab)),
        Node::Branch { left, right, .. } => {
            let left_slabs = left.slabs();
            if index < left_slabs {
                return slab_span_in(left, index, base);
            }
            slab_span_in(right, index - left_slabs, base + left.bytes())
        }
    }
}

fn locate_in(node: &Arc<Node>, at: u64) -> Option<SlabHit> {
    match node.as_ref() {
        Node::Empty => None,
        Node::Leaf(slab) => Some(SlabHit::new(*slab, at as u32)),
        Node::Branch { left, right, .. } => {
            if at < left.bytes() {
                locate_in(left, at)
            } else {
                locate_in(right, at - left.bytes())
            }
        }
    }
}

fn collect(node: &Arc<Node>, base: u64, start: u64, end: u64, out: &mut Vec<SlabSpan>) {
    let bytes = node.bytes();
    if bytes == 0 || base >= end || base + bytes <= start {
        return;
    }
    match node.as_ref() {
        Node::Empty => {}
        Node::Leaf(slab) => {
            let from = start.max(base);
            let to = end.min(base + bytes);
            let offset = (from - base) as u32;
            let len = (to - from) as u32;
            out.push(SlabSpan::new(from, slab.slice_clamped(offset, len)));
        }
        Node::Branch { left, right, .. } => {
            collect(left, base, start, end, out);
            collect(right, base + left.bytes(), start, end, out);
        }
    }
}

/// Undo and redo as root-pointer swaps.
///
/// Each state is one [`Spine`]; because edits share subtrees, recording an edit costs the O(log N)
/// nodes that edit already allocated and nothing else. No plaintext is stored here, so the wipe
/// set stays viewport-bounded no matter how deep the history goes (RFC-003 §2).
#[derive(Debug, Clone)]
pub struct History {
    states: Vec<Spine>,
    cursor: usize,
}

impl History {
    /// Starts a history at `initial`, which becomes the first undoable state.
    #[must_use]
    pub fn new(initial: Spine) -> Self {
        Self {
            states: vec![initial],
            cursor: 0,
        }
    }

    /// The state currently under the cursor.
    #[must_use]
    pub fn current(&self) -> &Spine {
        &self.states[self.cursor]
    }

    /// Records a new state after the cursor, discarding any redo branch.
    pub fn record(&mut self, next: Spine) {
        self.states.truncate(self.cursor + 1);
        self.states.push(next);
        self.cursor = self.states.len() - 1;
    }

    /// Steps back one state, or `None` at the oldest one.
    pub fn undo(&mut self) -> Option<&Spine> {
        if self.cursor == 0 {
            return None;
        }
        self.cursor -= 1;
        Some(&self.states[self.cursor])
    }

    /// Steps forward one state, or `None` at the newest one.
    pub fn redo(&mut self) -> Option<&Spine> {
        if self.cursor + 1 >= self.states.len() {
            return None;
        }
        self.cursor += 1;
        Some(&self.states[self.cursor])
    }

    /// How many states the history holds, including discarded redo branches.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.states.len()
    }

    /// The cursor's index among the states.
    #[must_use]
    pub fn position(&self) -> usize {
        self.cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slab::SegmentId;

    /// 30,000,000 two-byte words in 64 KiB segments: the lifetime document, as geometry only.
    fn lifetime_spine() -> Spine {
        const BYTES: u64 = 60_000_000;
        const SEGMENT_BYTES: u64 = 65_536;
        let full = BYTES / SEGMENT_BYTES;
        let remainder = BYTES % SEGMENT_BYTES;
        let mut slabs: Vec<Slab> = (0..full)
            .map(|segment| Slab::new(SegmentId::new(segment), 0, SEGMENT_BYTES as u32))
            .collect();
        slabs.push(Slab::new(SegmentId::new(full), 0, remainder as u32));
        Spine::from_slabs(slabs)
    }

    fn slab(segment: u64, len: u32) -> Slab {
        Slab::new(SegmentId::new(segment), 0, len)
    }

    #[test]
    fn an_empty_spine_is_empty_and_has_no_hits() {
        let spine = Spine::empty();
        assert!(spine.is_empty());
        assert_eq!(spine.len(), 0);
        assert_eq!(spine.slab_count(), 0);
        assert!(spine.locate(0).is_none());
        assert!(spine.spans_in(0, 10).is_empty());
    }

    #[test]
    fn slabs_keep_document_order_and_offsets() {
        let spine = Spine::from_slabs(vec![slab(0, 10), slab(1, 20), slab(2, 30)]);
        assert_eq!(spine.len(), 60);
        assert_eq!(spine.slab_count(), 3);

        let first = spine.locate(0).expect("in range");
        assert_eq!(first.slab().segment(), SegmentId::new(0));
        assert_eq!(first.segment_offset(), 0);

        let boundary = spine.locate(10).expect("in range");
        assert_eq!(boundary.slab().segment(), SegmentId::new(1));
        assert_eq!(boundary.within(), 0);

        let last = spine.locate(59).expect("in range");
        assert_eq!(last.slab().segment(), SegmentId::new(2));
        assert_eq!(last.within(), 29);
        assert!(spine.locate(60).is_none());
    }

    #[test]
    fn contiguous_fragments_of_one_segment_become_one_leaf() {
        // A log whose geometry edits fragmented names the same plaintext in runs; loading it merges
        // each run, so the leaf count is the document's shape and not its history (AUDIT OQ-9).
        let spine = Spine::from_slabs([
            Slab::new(SegmentId::new(0), 0, 10),
            Slab::new(SegmentId::new(0), 10, 20),
            Slab::new(SegmentId::new(1), 0, 5),
        ]);
        assert_eq!(spine.slab_count(), 2);
        assert_eq!(spine.len(), 35);
        let spans = spine.spans_in(0, 35);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].slab(), Slab::new(SegmentId::new(0), 0, 30));
        assert_eq!(spans[1].offset(), 30);
        assert_eq!(spans[1].slab(), Slab::new(SegmentId::new(1), 0, 5));
    }

    #[test]
    fn fragments_of_different_segments_or_with_a_gap_stay_apart() {
        // Merging is contiguity inside one segment and nothing else: a different segment is a
        // different plaintext, and a gap is a range the document does not name.
        let spine = Spine::from_slabs([
            Slab::new(SegmentId::new(0), 0, 10),
            Slab::new(SegmentId::new(1), 0, 10),
            Slab::new(SegmentId::new(0), 10, 10),
            Slab::new(SegmentId::new(0), 30, 10),
        ]);
        assert_eq!(spine.slab_count(), 4);
    }

    #[test]
    fn a_split_and_rejoin_stays_two_leaves_until_the_document_is_rebuilt() {
        // The residual OQ-9 keeps: a structural cut fragments the spine and the tree does not heal
        // itself. Loading the geometry again is where it heals, and the bytes do not move.
        let spine = Spine::from_slabs([Slab::new(SegmentId::new(0), 0, 20)]);
        let (before, after) = spine.split_at(10).expect("a split inside the document");
        let rejoined = before.concat(&after);
        assert_eq!(rejoined.slab_count(), 2);
        let geometry: Vec<Slab> = rejoined
            .spans_in(0, 20)
            .iter()
            .map(|span| span.slab())
            .collect();
        let rebuilt = Spine::from_slabs(geometry);
        assert_eq!(rebuilt.slab_count(), 1);
        assert_eq!(rebuilt.len(), 20);
        // The geometry is coarser and the bytes are where they were: byte 15 of segment 0 is byte
        // 15 of segment 0, whichever slab names it.
        let byte = |spine: &Spine| spine.locate(15).map(|hit| hit.segment_offset());
        assert_eq!(byte(&rebuilt), byte(&rejoined));
        assert_eq!(byte(&rebuilt), Some(15));
    }

    #[test]
    fn slab_spans_follow_document_order_and_offsets() {
        let spine = Spine::from_slabs(vec![slab(0, 10), slab(1, 20), slab(2, 30)]);
        let expected = [(0u64, 10u64), (10, 20), (30, 30)];
        for (index, (offset, len)) in expected.iter().enumerate() {
            let span = spine.slab_span(index as u32).expect("a leaf");
            assert_eq!(span.offset(), *offset);
            assert_eq!(span.len(), *len);
            assert_eq!(span.slab().segment(), SegmentId::new(index as u64));
        }
        assert!(spine.slab_span(3).is_none());
        assert!(Spine::empty().slab_span(0).is_none());
    }

    #[test]
    fn slab_spans_agree_with_a_full_scan_of_the_same_spine() {
        let mut spine = Spine::from_slabs(vec![slab(0, 10)]);
        for segment in 1..8u64 {
            spine = spine.append(slab(segment, 3u32 * (segment as u32)));
        }
        let scanned = spine.spans_in(0, spine.len());
        assert_eq!(scanned.len() as u32, spine.slab_count());
        for (index, span) in scanned.iter().enumerate() {
            let found = spine.slab_span(index as u32).expect("a leaf");
            assert_eq!(found.offset(), span.offset());
            assert_eq!(found.len(), span.len());
            assert_eq!(found.slab(), span.slab());
        }
        assert!(spine.slab_span(spine.slab_count()).is_none());
    }

    #[test]
    fn a_split_moves_the_order_statistic_with_the_leaves() {
        let spine = Spine::from_slabs(vec![slab(0, 10), slab(1, 10)]);
        let (left, right) = spine.split_at(10).expect("in range");
        assert_eq!(left.slab_count(), 1);
        assert_eq!(right.slab_count(), 1);
        // The split spine's coordinates restart at zero, and the leaf it starts with is the second
        // one of the document it came from.
        let first = right.slab_span(0).expect("a leaf");
        assert_eq!(first.offset(), 0);
        assert_eq!(first.slab().segment(), SegmentId::new(1));
        // The lifetime document's order statistic lands on the last leaf rather than scanning.
        let lifetime = lifetime_spine();
        let last = lifetime
            .slab_span(lifetime.slab_count() - 1)
            .expect("the final leaf");
        assert_eq!(last.offset() + last.len(), lifetime.len());
    }

    #[test]
    fn appending_leaves_the_previous_spine_untouched() {
        let before = Spine::from_slabs(vec![slab(0, 10)]);
        let after = before.append(slab(1, 10));
        assert_eq!(before.len(), 10);
        assert_eq!(before.slab_count(), 1);
        assert_eq!(after.len(), 20);
        assert_eq!(after.slab_count(), 2);
    }

    #[test]
    fn insertion_inside_a_slab_cuts_it_into_adjacent_ranges() {
        let spine = Spine::from_slabs(vec![slab(0, 10)]);
        let edited = spine.insert_slab(4, slab(7, 2)).expect("offset in range");
        let spans = edited.spans_in(0, edited.len());
        let geometry: Vec<(u64, u64, u32, u32)> = spans
            .iter()
            .map(|span| {
                let slab = span.slab();
                (
                    span.offset(),
                    slab.segment().get(),
                    slab.start(),
                    slab.len(),
                )
            })
            .collect();
        assert_eq!(
            geometry,
            vec![(0, 0, 0, 4), (4, 7, 0, 2), (6, 0, 4, 6)],
            "the cut halves must name adjacent ranges of segment 0"
        );
        assert_eq!(edited.len(), 12);
    }

    #[test]
    fn removal_is_exact_at_boundaries_and_inside_a_slab() {
        let spine = Spine::from_slabs(vec![slab(0, 10), slab(1, 10)]);
        let whole = spine.remove_range(0, 20).expect("covers the document");
        assert!(whole.is_empty());

        let middle = spine.remove_range(5, 15).expect("valid range");
        assert_eq!(middle.len(), 10);
        assert_eq!(middle.slab_count(), 2, "the cut leaves both halves");
        let head = middle.locate(0).expect("in range");
        assert_eq!((head.slab().segment().get(), head.within()), (0, 0));
        let tail = middle.locate(5).expect("in range");
        assert_eq!(tail.slab().segment().get(), 1);
        assert_eq!(tail.segment_offset(), 5);
    }

    #[test]
    fn bad_offsets_and_ranges_are_refused() {
        let spine = Spine::from_slabs(vec![slab(0, 10)]);
        // Spine deliberately has no `PartialEq`: two spines with equal bytes are not the same
        // document, and comparing roots would invite a structural equality that history makes
        // meaningless. Errors are compared directly instead.
        assert_eq!(
            spine.insert_slab(11, slab(1, 1)).unwrap_err(),
            SpineError::OffsetOutOfRange {
                offset: 11,
                len: 10
            }
        );
        assert_eq!(
            spine.split_at(11).unwrap_err(),
            SpineError::OffsetOutOfRange {
                offset: 11,
                len: 10
            }
        );
        assert_eq!(
            spine.remove_range(4, 2).unwrap_err(),
            SpineError::RangeOutOfRange {
                start: 4,
                end: 2,
                len: 10
            }
        );
        assert_eq!(
            spine.remove_range(0, 11).unwrap_err(),
            SpineError::RangeOutOfRange {
                start: 0,
                end: 11,
                len: 10
            }
        );
    }

    #[test]
    fn splitting_produces_two_spines_that_cover_the_document() {
        let spine = Spine::from_slabs(vec![slab(0, 10), slab(1, 10), slab(2, 10)]);
        let (head, tail) = spine.split_at(15).expect("in range");
        assert_eq!(head.len(), 15);
        assert_eq!(tail.len(), 15);
        assert_eq!(tail.locate(0).expect("in range").slab().segment().get(), 1);
        assert_eq!(tail.locate(0).expect("in range").segment_offset(), 5);
        assert_eq!(tail.locate(14).expect("in range").slab().segment().get(), 2);
    }

    #[test]
    fn viewport_spans_are_clipped_and_counted_at_the_window_not_the_document() {
        let spine = Spine::from_slabs(vec![slab(0, 10), slab(1, 10), slab(2, 10)]);
        let window = spine.spans_in(8, 12);
        assert_eq!(
            window.len(),
            2,
            "only two slabs overlap an eight-byte window"
        );
        assert_eq!(window[0].offset(), 8);
        assert_eq!(window[0].len(), 2);
        assert_eq!(window[1].offset(), 10);
        assert_eq!(window[1].len(), 2);
        assert_eq!(window[1].slab().segment().get(), 1);
    }

    #[test]
    fn spans_past_the_end_are_clamped_not_invented() {
        let spine = Spine::from_slabs(vec![slab(0, 10)]);
        let window = spine.spans_in(5, 1_000);
        assert_eq!(window.len(), 1);
        assert_eq!(window[0].offset(), 5);
        assert_eq!(window[0].len(), 5);
    }

    #[test]
    fn a_thirty_million_word_spine_is_geometry_and_stays_logarithmic() {
        let spine = lifetime_spine();
        assert_eq!(spine.len(), 60_000_000);
        assert_eq!(spine.slab_count(), 916);

        let first = spine.locate(0).expect("in range");
        assert_eq!(first.slab().segment().get(), 0);

        let last = spine.locate(59_999_999).expect("in range");
        assert_eq!(last.slab().segment().get(), 915);
        assert_eq!(last.within(), 34_559);
        assert_eq!(last.segment_offset(), 34_559);
        assert!(spine.locate(60_000_000).is_none());

        // One page-sized window touches a couple of slabs, never the 916-slab document.
        let window = spine.spans_in(30_010_000, 30_030_000);
        assert_eq!(window.len(), 2);
        assert_eq!(window[0].offset(), 30_010_000);
        assert_eq!(window[0].len(), 30_015_488 - 30_010_000);
    }

    #[test]
    fn empty_slabs_cannot_enter_the_tree() {
        let spine = Spine::from_slabs(vec![slab(0, 10), Slab::new(SegmentId::new(1), 0, 0)]);
        assert_eq!(
            spine.slab_count(),
            1,
            "a zero-byte slab must not become a leaf"
        );
        assert_eq!(spine.len(), 10);

        let appended = spine.append(Slab::new(SegmentId::new(2), 0, 0));
        assert_eq!(appended.len(), spine.len());
        assert_eq!(appended.slab_count(), spine.slab_count());

        let inserted = spine
            .insert_slab(5, Slab::new(SegmentId::new(3), 0, 0))
            .expect("offset in range");
        assert_eq!(inserted.len(), spine.len());
        assert_eq!(inserted.slab_count(), spine.slab_count());
        crate::tree::check_invariants(&inserted.root);
    }

    #[test]
    fn concat_undoes_a_split_exactly() {
        let spine = Spine::from_slabs(vec![slab(0, 10), slab(1, 10), slab(2, 10)]);
        for at in [0u64, 1, 10, 15, 29, 30] {
            let (left, right) = spine.split_at(at).expect("offset in range");
            let rejoined = left.concat(&right);
            assert_eq!(rejoined.len(), spine.len(), "split at {at}");
            crate::tree::check_invariants(&rejoined.root);
            let pairs = |value: &Spine| -> Vec<(u64, u32)> {
                let mut out = Vec::new();
                for span in value.spans_in(0, value.len()) {
                    let slab = span.slab();
                    for offset in 0..slab.len() {
                        out.push((slab.segment().get(), slab.start() + offset));
                    }
                }
                out
            };
            assert_eq!(pairs(&rejoined), pairs(&spine), "split at {at}");
        }
    }

    #[test]
    fn random_edits_match_a_byte_level_model() {
        use crate::test_support::XorShift;

        for seed in [1u64, 9, 0x00C0_FFEE, 123_456_789] {
            let mut rng = XorShift::new(seed);
            let mut spine = Spine::empty();
            // (segment, offset in segment) for every document byte, in order.
            let mut model: Vec<(u64, u32)> = Vec::new();
            let mut next_segment = 0u64;

            for _ in 0..250 {
                match rng.below(4) {
                    0 => {
                        let len = 1 + rng.below(16) as u32;
                        let slab = Slab::new(SegmentId::new(next_segment), 0, len);
                        next_segment += 1;
                        spine = spine.append(slab);
                        model.extend((0..len).map(|offset| (slab.segment().get(), offset)));
                    }
                    1 => {
                        let len = 1 + rng.below(8) as u32;
                        let at = rng.below(model.len() as u64 + 1) as usize;
                        let slab = Slab::new(SegmentId::new(next_segment), 0, len);
                        next_segment += 1;
                        spine = spine.insert_slab(at as u64, slab).expect("offset in range");
                        for (index, offset) in (0..len).enumerate() {
                            model.insert(at + index, (slab.segment().get(), offset));
                        }
                    }
                    2 => {
                        if model.is_empty() {
                            continue;
                        }
                        let start = rng.below(model.len() as u64);
                        let end = start + rng.below(model.len() as u64 - start + 1);
                        spine = spine.remove_range(start, end).expect("valid range");
                        model.drain(start as usize..end as usize);
                    }
                    _ => {
                        // Split and rejoin: shape churn with no content change.
                        let at = rng.below(model.len() as u64 + 1);
                        let (left, right) = spine.split_at(at).expect("offset in range");
                        spine = left.concat(&right);
                    }
                }

                crate::tree::check_invariants(&spine.root);
                assert_eq!(spine.len(), model.len() as u64, "seed {seed}");

                // The whole document, byte for byte: catches a tree that keeps the right length
                // while scrambling the geometry.
                let mut produced = Vec::with_capacity(model.len());
                let mut covered = 0u64;
                for span in spine.spans_in(0, spine.len()) {
                    assert_eq!(
                        span.offset(),
                        covered,
                        "spans must tile the document, seed {seed}"
                    );
                    let slab = span.slab();
                    for offset in 0..slab.len() {
                        produced.push((slab.segment().get(), slab.start() + offset));
                    }
                    covered += span.len();
                }
                assert_eq!(covered, spine.len(), "seed {seed}");
                assert_eq!(produced, model, "seed {seed}");

                if !model.is_empty() {
                    let offset = rng.below(model.len() as u64);
                    let hit = spine.locate(offset).expect("in range");
                    assert_eq!(
                        (hit.slab().segment().get(), hit.segment_offset()),
                        model[offset as usize],
                        "seed {seed} at offset {offset}"
                    );
                }
            }
        }
    }

    #[test]
    fn coalesce_merges_only_contiguous_same_segment_neighbors_and_preserves_styles() {
        let s0 = SegmentId::new(0);
        let s1 = SegmentId::new(1);
        let raw = Spine {
            root: crate::tree::join(
                crate::tree::join(
                    crate::tree::leaf(Slab::new(s0, 0, 5)),
                    crate::tree::leaf(Slab::new(s0, 5, 6)),
                ),
                crate::tree::join(
                    crate::tree::leaf(Slab::new(s1, 0, 4)),
                    crate::tree::leaf(Slab::new(s0, 11, 3)),
                ),
            ),
            styles: StyleMap::empty()
                .apply_patch(18, 2, 9, &StylePatch::new().with_bold(true))
                .unwrap(),
        };
        assert_eq!(raw.slab_count(), 4);
        let merged = raw.coalesce();
        assert_eq!(merged.slab_count(), 3);
        let spans = merged.spans_in(0, merged.len());
        assert_eq!(spans[0].slab(), Slab::new(s0, 0, 11));
        assert_eq!(spans[1].slab(), Slab::new(s1, 0, 4));
        assert_eq!(spans[2].slab(), Slab::new(s0, 11, 3));
        assert_eq!(merged.style_spans_in(0, 18), raw.style_spans_in(0, 18));
        assert!(merged.coalesce().ptr_eq(&merged));
    }

    #[test]
    fn diff_slabs_reports_added_removed_and_replaced_without_decrypting() {
        let s0 = SegmentId::new(0);
        let s1 = SegmentId::new(1);
        let s2 = SegmentId::new(2);
        let old = Spine::from_slabs(vec![Slab::new(s0, 0, 10), Slab::new(s1, 0, 8)]);
        assert!(old.diff_slabs(&old).is_empty());

        let added = Spine::from_slabs(vec![
            Slab::new(s0, 0, 10),
            Slab::new(s2, 0, 5),
            Slab::new(s1, 0, 8),
        ]);
        let h_add = old.diff_slabs(&added);
        assert_eq!(h_add.len(), 1);
        assert_eq!(h_add[0].kind, SpineDiffKind::Added);
        assert_eq!(h_add[0].old_range, (10, 10));
        assert_eq!(h_add[0].new_range, (10, 15));
        assert_eq!(h_add[0].new_slabs, vec![Slab::new(s2, 0, 5)]);

        let h_rem = added.diff_slabs(&old);
        assert_eq!(h_rem.len(), 1);
        assert_eq!(h_rem[0].kind, SpineDiffKind::Removed);
        assert_eq!(h_rem[0].old_range, (10, 15));
        assert_eq!(h_rem[0].new_range, (10, 10));

        let replaced = Spine::from_slabs(vec![
            Slab::new(s0, 0, 10),
            Slab::new(s2, 0, 4),
            Slab::new(s1, 0, 8),
        ]);
        let h_rep = added.diff_slabs(&replaced);
        assert_eq!(h_rep.len(), 1);
        assert_eq!(h_rep[0].kind, SpineDiffKind::Replaced);
        assert_eq!(h_rep[0].old_slabs, vec![Slab::new(s2, 0, 5)]);
        assert_eq!(h_rep[0].new_slabs, vec![Slab::new(s2, 0, 4)]);
    }

    #[test]
    fn history_undoes_and_redoes_by_swapping_roots() {
        let base = Spine::empty();
        let mut history = History::new(base);
        assert_eq!(history.current().len(), 0);

        let one = history.current().append(slab(0, 10));
        history.record(one);
        assert_eq!(history.current().len(), 10);

        let two = history.current().append(slab(1, 10));
        history.record(two);
        assert_eq!(history.current().len(), 20);
        assert_eq!(history.depth(), 3);
        assert_eq!(history.position(), 2);

        assert_eq!(history.undo().map(Spine::len), Some(10));
        assert_eq!(history.undo().map(Spine::len), Some(0));
        assert!(history.undo().is_none());
        assert_eq!(history.position(), 0);

        assert_eq!(history.redo().map(Spine::len), Some(10));
        assert_eq!(history.redo().map(Spine::len), Some(20));
        assert!(history.redo().is_none());
    }

    #[test]
    fn recording_after_undo_discards_the_redo_branch() {
        let mut history = History::new(Spine::empty());
        let one = history.current().append(slab(0, 10));
        history.record(one);
        let two = history.current().append(slab(1, 10));
        history.record(two);

        let _ = history.undo();
        let rewritten = history.current().append(slab(9, 5));
        history.record(rewritten);

        assert_eq!(history.depth(), 3, "the discarded future is gone");
        assert_eq!(history.position(), 2);
        assert!(history.redo().is_none());
        assert_eq!(history.current().len(), 15);
    }

    #[test]
    fn errors_describe_the_failure() {
        let message = SpineError::OffsetOutOfRange { offset: 9, len: 3 }.to_string();
        assert!(message.contains("offset 9"));
        assert!(message.contains("3-byte"));
    }
}
