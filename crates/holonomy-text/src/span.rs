//! The parallel interval map: style spans, kept beside the UTF-8 bytes rather than inside them.
//!
//! PROJECT.md §5 Phase 6: "Style spans as a parallel interval map of the PRD's 16-byte
//! `TextIntervalSpan`." FR-1.4 keeps formatting in a separate structure so the rope stays
//! contiguous UTF-8 with no formatting metadata interleaved.
//!
//! # Why parallel and not embedded
//!
//! Three reasons, in order of how much they would hurt if embedded:
//!
//! 1. **The rope is destructive.** FR-1.2 requires a deleted byte to be overwritten and zeroed. If a
//!    style bit lived in the same byte, zeroing the byte would erase the formatting, and undo would
//!    need to reconstruct it from a copy that is by definition not kept.
//! 2. **The leaf layout is fixed.** Plan.md §3.1's node is `[u8; 4096]` plus three `u16`s and two
//!    pointers. There is no room for per-byte style bits, and a gap buffer's whole point is that the
//!    gap must be movable bytes with no per-byte side data to fix up alongside it.
//! 3. **The document is UTF-8, so "byte" is already the wrong unit for styling.** A colour applied to
//!    `é` must cover both of its bytes. Storing per-byte bits gets that wrong by construction;
//!    byte *offsets* as boundaries gets it right.
//!
//! # The insertion policy, which is a real choice
//!
//! FR-1.4's requirement is stated as "any insert at byte offset `B` shifts all span boundaries with
//! `offset > B`" -- boundaries *strictly* greater than the edit point move. That is
//! [`SpanPolicy::Strict`], and it means text typed exactly at a span's end boundary inherits the
//! *following* span's style.
//!
//! Word processors behave the other way: typing at the end of a bold word extends the bold. That is
//! [`SpanPolicy::GrowIntoInsert`], boundaries `>= B` move.
//!
//! [`SpanPolicy::Strict`] is the default because it is what the requirement says and it is the
//! reversible one -- text that stops a run is text the user typed deliberately. The other is
//! available and named, and [`SpanMap::apply_insert_with`] takes the policy per call so an editor can
//! use one at the caret and the other for a paste.
//!
//! `GrowIntoInsert` cannot be implemented by a post-pass over the strict result without re-splitting
//! spans, which is why it is a policy in the transform rather than a flag on the output.

use std::fmt;

/// Bold. Selects atlas style 1.
pub const STYLE_BOLD: u16 = 1 << 0;
/// Italic. Selects atlas style 2.
pub const STYLE_ITALIC: u16 = 1 << 1;
/// Code. Selects atlas style 3 (JetBrains Mono), the monospaced face.
pub const STYLE_CODE: u16 = 1 << 2;
/// Header. Selects the heading ppem, which is what makes a line's height differ from body text.
///
/// This bit is load-bearing for the geometry: `LineMetrics::from_font` is called with a size chosen by
/// this flag, so a line whose first character is a header gets a taller line box and every line below
/// it shifts. That shift is exactly what `LineGeometry::scroll_compensation` exists to absorb.
///
/// PRD.md §7.1 defines bits 0-3 as Bold, Italic, Code, Header. A Phase 6 directive proposed
/// Underline and Monospace for bits 2-3 instead; Code and Header are kept because they are the bits
/// with a face or a size behind them, and Underline has neither. See `atlas_style` for the mapping.
pub const STYLE_HEADER: u16 = 1 << 3;

/// One styled byte range.
///
/// PRD.md §7.1's field list, and its widths: `color_rgb` is a `u32` holding `0x00RRGGBB`, which is
/// what the Phase 4 SSE2 kernel takes (`_mm_set1_epi32(color_rgb as i32)`). The 16 bytes are
/// `4 + 4 + 2 + 2 padding + 4`; `repr(C)` keeps the padding where `C` puts it, and
/// `size_of_is_sixteen` pins it.
///
/// A `u16` RGB565 here would be 12 bytes, not the 16 the requirement states twice, and would need a
/// widening conversion on the blitter's hot path to become the `u32` the kernel wants.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextIntervalSpan {
    /// First byte of the run, inclusive.
    pub start_byte: u32,
    /// One past the last byte of the run, exclusive.
    pub end_byte: u32,
    /// [`STYLE_BOLD`] | [`STYLE_ITALIC`] | [`STYLE_CODE`] | [`STYLE_HEADER`], or 0 for plain.
    pub style_flags: u16,
    /// `0x00RRGGBB`. Zero means "the document's default colour", not opaque black.
    pub color_rgb: u32,
}

impl TextIntervalSpan {
    /// A plain span over `[start, end)`.
    pub const fn plain(start: u32, end: u32) -> Self {
        Self {
            start_byte: start,
            end_byte: end,
            style_flags: 0,
            color_rgb: 0,
        }
    }

    /// A styled span over `[start, end)`.
    pub const fn styled(start: u32, end: u32, style_flags: u16, color_rgb: u32) -> Self {
        Self {
            start_byte: start,
            end_byte: end,
            style_flags,
            color_rgb,
        }
    }

    /// Length in bytes. Saturating, so a corrupt span reports 0 rather than wrapping.
    #[inline]
    pub fn len(&self) -> u32 {
        self.end_byte.saturating_sub(self.start_byte)
    }

    /// Whether the span covers no bytes.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.end_byte <= self.start_byte
    }

    /// Whether `offset` is inside the span.
    ///
    /// Half-open: a boundary between two runs belongs to the run that starts there, so the last byte
    /// of a span is exclusive and the first of the next is inclusive. Any other convention makes two
    /// adjacent spans of different colours share a byte, and which colour wins becomes a coin flip.
    #[inline]
    pub fn contains(&self, offset: u32) -> bool {
        offset >= self.start_byte && offset < self.end_byte
    }

    /// The atlas style index this span's flags select.
    ///
    /// The atlas has exactly four faces (Inter Regular/Bold/Italic, JetBrains Mono Regular), so four
    /// flag combinations are addressable and the rest collapse:
    ///
    /// | flags | style |
    /// |---|---|
    /// | none | 0, Inter Regular |
    /// | [`STYLE_BOLD`] | 1, Inter Bold |
    /// | [`STYLE_ITALIC`] | 2, Inter Italic |
    /// | [`STYLE_CODE`] | 3, JetBrains Mono |
    ///
    /// Bold+Italic has no face in the atlas, so it resolves to Bold. `u8::MAX` is returned for an
    /// unknown request rather than a valid index, so a caller that adds a face later fails loudly
    /// instead of drawing the wrong glyphs.
    pub const fn atlas_style(&self) -> u8 {
        if self.style_flags & STYLE_CODE != 0 {
            3
        } else if self.style_flags & STYLE_BOLD != 0 {
            1
        } else if self.style_flags & STYLE_ITALIC != 0 {
            2
        } else {
            0
        }
    }

    /// Whether this span asks for a heading's size, which is what changes its line's height.
    #[inline]
    pub fn is_header(&self) -> bool {
        self.style_flags & STYLE_HEADER != 0
    }

    /// Whether the two spans are stylistically identical, ignoring their bounds.
    ///
    /// What the map uses to merge adjacent runs, and what a re-flow uses to decide two lines are the
    /// same kind of line.
    #[inline]
    pub fn same_style(&self, other: &TextIntervalSpan) -> bool {
        self.style_flags == other.style_flags && self.color_rgb == other.color_rgb
    }
}

impl fmt::Display for TextIntervalSpan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}..{})", self.start_byte, self.end_byte)?;
        if self.style_flags != 0 {
            write!(f, " {:#06x}", self.style_flags)?;
        }
        if self.color_rgb != 0 {
            write!(f, " #{:06x}", self.color_rgb & 0x00FF_FFFF)?;
        }
        Ok(())
    }
}

/// How an insertion at a span boundary resolves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpanPolicy {
    /// Move boundaries strictly greater than the insertion point. Boundaries exactly at it stay, so
    /// inserted text inherits the *following* span's style.
    ///
    /// FR-1.4's wording. Default, because it is reversible: text that stops a run is text the user
    /// typed on purpose.
    #[default]
    Strict,
    /// Move boundaries at or after the insertion point, so a span ending exactly at the insertion
    /// point grows and inserted text inherits the *preceding* span's style.
    ///
    /// What a word processor does, and what an editor wants at the caret -- typing at the end of a
    /// bold word should keep it bold.
    GrowIntoInsert,
}

/// Why a span operation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpanError {
    /// An offset past the end of the document.
    OutOfBounds {
        /// Requested offset.
        offset: u64,
        /// Document length.
        text_len: u32,
    },
    /// A range whose length exceeds the document.
    RangeOutOfBounds {
        /// Start.
        start: u64,
        /// End, exclusive.
        end: u64,
        /// Document length.
        text_len: u32,
    },
    /// `start > end`, which is not a range.
    InvertedRange {
        /// Start.
        start: u32,
        /// End, exclusive.
        end: u32,
    },
    /// The spans do not satisfy the map's invariants. Never produced by correct code; present so a
    /// bug surfaces as a diagnosable error rather than a wrong render.
    Invariant(String),
}

impl fmt::Display for SpanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::OutOfBounds { offset, text_len } => {
                write!(f, "byte {offset} is past the document's {text_len} bytes")
            }
            Self::RangeOutOfBounds {
                start,
                end,
                text_len,
            } => {
                write!(
                    f,
                    "range {start}..{end} is past the document's {text_len} bytes"
                )
            }
            Self::InvertedRange { start, end } => write!(f, "{start}..{end} is not a range"),
            Self::Invariant(m) => write!(f, "span map invariant broken: {m}"),
        }
    }
}

impl std::error::Error for SpanError {}

/// A sorted, non-overlapping, gap-free map of style spans over a document's bytes.
///
/// # Invariants
///
/// These are what [`SpanMap::check_invariants`] asserts and what every mutator preserves:
///
/// * `start_byte <= end_byte` for every span -- a span is a range, not a pair of points.
/// * Spans are sorted by `start_byte` and **never overlap**.
/// * Spans are **gap-free**: they cover `[0, text_len)` exactly. There is no "unstyled region"
///   because an unstyled byte is a span with `style_flags == 0`.
/// * Every boundary is `<= text_len`.
///
/// The gap-free property is the one worth arguing for. An alternative design lets spans be sparse and
/// returns the default style for a gap, which makes [`SpanMap::style_at`] one comparison instead of a
/// walk -- but it makes every insert/delete responsible for noticing when a gap closes, and a missed
/// case is a run of text silently picking up or losing its formatting. A map that cannot represent a
/// gap cannot get that wrong.
#[derive(Debug, Clone, Default)]
pub struct SpanMap {
    /// Sorted, non-overlapping, gap-free. Capacity reserved up front -- see [`SPAN_HEADROOM`].
    spans: Vec<TextIntervalSpan>,
    /// The document length the spans describe. Kept so the map can validate its own bounds without
    /// being handed the length on every call.
    text_len: u32,
    /// The highest document offset whose **bytes have actually been examined**. Phase 13, part 6.
    ///
    /// # Why this exists, and why it is the whole design
    ///
    /// [`plain`](Self::plain) makes a claim about **content**: that every byte is plain-styled. For a
    /// document loaded from a container, that claim is **unverifiable until the bytes are read** — a
    /// document full of `**bold**` makes it false. So the map needs to be able to say *"I have not
    /// looked"*, distinctly from *"I looked and it is plain"*.
    ///
    /// **The invariant the whole of Phase 13 part 6 rests on: styling is a function of the document's
    /// bytes, never of which leaves happen to be resident.** Two designs were rejected against it:
    ///
    /// * **Correct the map when a leaf faults in.** Then a document's appearance depends on residency —
    ///   the same document renders differently before and after a scroll — and `style_at` is `&self`, so
    ///   the correction would have to be a side effect of an unrelated read. A semantic change in the
    ///   wrong place, producing output that varies without any visible cause.
    /// * **Never correct it.** Then `plain` is a promise the format has to keep, and every styled
    ///   document loaded from a container silently loses its styling.
    ///
    /// Both fail for the same reason: they make styling depend on *when* something was read rather than
    /// on *what* it says. The watermark separates the two — the map may record what it has learned, and
    /// a consumer may ask whether it has learned it yet.
    ///
    /// # Monotone, deliberately
    ///
    /// **`read_through` only ever increases.** A byte that has been examined does not become unexamined
    /// because its leaf was evicted, so the watermark is not a residency map and must not be confused with
    /// one. That is what makes `style_at_known` safe to call at any time: it can only ever report *less*
    /// certainty as the document grows, never more, and never oscillate.
    ///
    /// **It is not the same thing as "how much is resident", and conflating them would be the bug this
    /// field is easy to write.** Residency goes up and down; this does not.
    read_through: u32,
}

/// Spans reserved beyond the document's current run count, so an insert does not grow the `Vec`.
///
/// The span map sits on the keystroke path: every character typed shifts its boundaries and inserts a
/// span for the new region. `Vec::insert` reallocates when it runs past capacity, and that allocation is
/// on the same path as the gap-buffer write FR-1.2 measures. Reserving 16 slots covers any realistic
/// line -- sixteen styled runs on one line is already more than a reader will distinguish -- and a
/// document with more grows once, amortised.
///
/// 16 `TextIntervalSpan`s is 256 bytes, nothing beside the 64 KiB undo arena.
pub const SPAN_HEADROOM: usize = 16;

/// A fresh span vector with [`SPAN_HEADROOM`] slots reserved.
fn with_headroom() -> Vec<TextIntervalSpan> {
    Vec::with_capacity(SPAN_HEADROOM)
}

impl SpanMap {
    /// An empty map over an empty document.
    pub fn new() -> Self {
        Self {
            spans: with_headroom(),
            text_len: 0,
            // **Zero, not `text_len`.** An empty map has examined nothing, and `plain` is the only place
            // that knows otherwise -- because it is being handed a length by a caller who has *not* read
            // the bytes either. That is the whole point of the field.
            read_through: 0,
        }
    }

    /// A map over `text_len` bytes with every byte plain.
    pub fn plain(text_len: u32) -> Self {
        let mut m = Self::new();
        m.text_len = text_len;
        if text_len > 0 {
            m.spans.push(TextIntervalSpan::plain(0, text_len));
        }
        m
    }

    /// A map over `text_len` bytes with no spans at all.
    ///
    /// For a document whose length is not yet known. Every query returns the default style until the
    /// first span is added.
    pub fn empty_over(text_len: u32) -> Self {
        Self {
            spans: with_headroom(),
            text_len,
            read_through: 0,
        }
    }

    /// The document length the spans describe.
    #[inline]
    pub fn text_len(&self) -> u32 {
        self.text_len
    }

    /// How far the document has been **read**, in bytes. Phase 13, part 6.
    ///
    /// Offsets below this have been examined; offsets at or above it have not. **Not** a residency figure
    /// — see [`SpanMap::read_through`].
    #[inline]
    pub fn read_through(&self) -> u32 {
        self.read_through
    }

    /// Whether offset `offset` has been **read**, so its style is known rather than assumed.
    ///
    /// **The distinction the whole of Phase 13 part 6 exists to express.** `style_at` answers "what style
    /// does the map say here", which for an unread offset is `plain` *because nobody has looked*. This
    /// answers "has anyone looked", so a caller can tell those apart instead of acting on a guess.
    #[inline]
    pub fn is_read(&self, offset: u32) -> bool {
        offset < self.read_through
    }

    /// The style at `offset`, or `None` if those bytes have not been read.
    ///
    /// **The `&self` query a caller should reach for on a sparse document.** Where `style_at` has to
    /// answer something, `style_at_known` is allowed to answer "I don't know" — and a caller that cannot
    /// tolerate that should use the `&mut` faulting path instead of reading a guess.
    ///
    /// **`style_at` is deliberately *not* changed to return `Option`.** It is on the paint path, called
    /// from `&self` contexts, and the paint path's current behaviour — assume plain, count it in
    /// `PaintStats::runs_missing` — is the documented stopgap. Changing the signature would force every
    /// one of those call sites to handle absence at once, before anything knows whether the answer is
    /// needed. This is the additive step.
    #[inline]
    pub fn style_at_known(&self, offset: u32) -> Option<TextIntervalSpan> {
        self.is_read(offset).then(|| self.style_at(offset))
    }

    /// Record that the document's bytes up to `through` have been examined, and adopt whatever styling
    /// they carry.
    ///
    /// **This is the `&mut` half, and it is where a fault-in's findings belong.** A leaf that arrives
    /// carries its own bytes, so the caller computes the spans for that range and hands them over rather
    /// than having the map guess. The map never looks at document bytes itself.
    ///
    /// ## Why it is a *splice of the prefix*, not an insert
    ///
    /// The map's invariant is sorted, non-overlapping and **gap-free over `[0, text_len)`**. So the spans
    /// describing `[0, through)` are exactly the prefix that ends at or before `through`, and replacing
    /// them with `learned` is: drop the prefix, splice `learned` in front, keep the tail. **One
    /// representation, one way to hold a span.** An insert-style API here would give the map a second way
    /// to acquire spans, and the two would diverge invisibly until a merge disagreed.
    ///
    /// ## Monotone, and the watermark advances even with nothing learned
    ///
    /// `read_through` only ever increases, and **it is advanced even when `learned` is empty** — because an
    /// unstyled run *is* a finding. Refusing to advance on empty input would leave a genuinely plain region
    /// permanently unknown, and the map would never learn that a document with no styling is plain.
    /// `learned.is_empty()` is the common case, not a no-op.
    ///
    /// ## Why out-of-range is refused rather than clamped
    ///
    /// A clamped region would claim certainty about bytes nobody read, which is precisely the failure this
    /// design exists to prevent. Refusing is the only safe direction.
    pub fn observe(&mut self, through: u32, learned: &[TextIntervalSpan]) -> Result<(), SpanError> {
        if through > self.text_len {
            return Err(SpanError::OutOfBounds { offset: through as u64, text_len: self.text_len });
        }
        if through < self.read_through {
            // Re-observing a prefix already observed. **Allowed and idempotent**, because a leaf can be
            // evicted and faulted back in, and the second visit must reach the same state as the first --
            // that is the "indistinguishable from resident" property, and it is why this is not an error.
            return Ok(());
        }
        // `learned` must tile `[0, through)` exactly: gap-free, sorted, in bounds. Checked rather than
        // assumed, because a gap here would be a claim about a byte range the map was not told about.
        let mut want = 0u32;
        for s in learned {
            if s.start_byte != want {
                return Err(SpanError::RangeOutOfBounds {
                    start: s.start_byte as u64,
                    end: s.end_byte as u64,
                    text_len: want,
                });
            }
            if s.end_byte > through {
                return Err(SpanError::RangeOutOfBounds {
                    start: s.start_byte as u64,
                    end: s.end_byte as u64,
                    text_len: through,
                });
            }
            want = s.end_byte;
        }
        if want != through {
            return Err(SpanError::RangeOutOfBounds {
                start: want as u64,
                end: through as u64,
                text_len: through,
            });
        }

        // **Drop the prefix, keep the tail -- but the tail has to be *re-cut* at `through`, not just
        // sliced.**
        //
        // The first version of this did `split_off(partition_point(|s| s.end_byte <= through))` and
        // appended. That is wrong whenever a span straddles the boundary, and it is wrong *always* in
        // practice: `SpanMap::plain(len)` holds **one span covering the whole document**, so the slice
        // kept `plain(0, 1000)` and the result was `[bold(0, 200), plain(0, 1000)]` -- overlapping,
        // unsorted, and in violation of the map's own invariant. The gate caught it on the first run,
        // which is the argument for having the invariant asserted rather than described.
        let mut tail: Vec<TextIntervalSpan> = Vec::new();
        for s in self.spans.drain(..) {
            if s.end_byte <= through {
                continue; // wholly inside the re-observed prefix
            }
            if s.start_byte < through {
                // Straddles: keep only the part from `through` onward, as a *new* span.
                tail.push(TextIntervalSpan {
                    start_byte: through,
                    end_byte: s.end_byte,
                    style_flags: s.style_flags,
                    color_rgb: s.color_rgb,
                });
                continue;
            }
            tail.push(s);
        }
        if through < self.text_len && tail.is_empty() {
            // Everything was inside the prefix, so the unread remainder needs a run of its own or the
            // gap-free invariant breaks.
            tail.push(TextIntervalSpan::plain(through, self.text_len));
        }
        let mut next = Vec::with_capacity(learned.len() + tail.len());
        next.extend_from_slice(learned);
        next.extend_from_slice(&tail);
        self.spans = next;
        // **After** the splice, so a refused input leaves the map exactly as it was.
        self.read_through = through;
        Ok(())
    }

    /// The spans, sorted and non-overlapping.
    #[inline]
    pub fn spans(&self) -> &[TextIntervalSpan] {
        &self.spans
    }

    /// Number of spans.
    #[inline]
    pub fn len(&self) -> usize {
        self.spans.len()
    }

    /// Whether the map holds no spans.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.spans.is_empty()
    }

    /// The style in effect at `offset`.
    ///
    /// O(log n) over the sorted starts. An `offset` past the end returns the default style rather than
    /// an error: the caret sits at `text_len` and asks this on every keystroke.
    pub fn style_at(&self, offset: u32) -> TextIntervalSpan {
        if self.spans.is_empty() {
            return TextIntervalSpan::plain(offset, offset);
        }
        let i = match self.spans.binary_search_by_key(&offset, |s| s.start_byte) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        };
        let s = self.spans[i];
        if s.contains(offset) {
            TextIntervalSpan::styled(s.start_byte, s.end_byte, s.style_flags, s.color_rgb)
        } else {
            // Either `offset == text_len`, or the map is sparse because it came from
            // `empty_over`. Both mean "no styling here".
            TextIntervalSpan::plain(offset, offset)
        }
    }

    /// Whether `offset` lies inside a span carrying `flag`.
    ///
    /// The caret and the header bit: a line is a heading if its first character is one, and this is
    /// how that is asked without materialising a glyph.
    #[inline]
    pub fn has_style_at(&self, offset: u32, flag: u16) -> bool {
        self.style_at(offset).style_flags & flag != 0
    }

    /// Every span overlapping `[start, end)`, as `(span, clipped_start, clipped_end)`.
    ///
    /// What a renderer walks: for each visible line, the runs it must draw, each with a colour and an
    /// atlas style. Clipping is returned rather than applied so the caller can tell a partially
    /// visible run from a complete one.
    pub fn runs_in(&self, start: u32, end: u32) -> Vec<(TextIntervalSpan, u32, u32)> {
        if end <= start || self.spans.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for s in &self.spans {
            if s.end_byte <= start {
                continue;
            }
            if s.start_byte >= end {
                break;
            }
            let lo = s.start_byte.max(start);
            let hi = s.end_byte.min(end);
            if hi > lo {
                out.push((*s, lo, hi));
            }
        }
        out
    }

    /// Apply an insertion of `len` bytes at `offset`, using [`SpanPolicy::Strict`].
    ///
    /// See [`SpanMap::apply_insert_with`] for the policy and the reasoning.
    pub fn apply_insert(&mut self, offset: u32, len: u32) -> Result<(), SpanError> {
        self.apply_insert_with(offset, len, SpanPolicy::Strict)
    }

    /// Apply an insertion of `len` bytes at `offset`.
    ///
    /// Moves every span boundary according to `policy`, then re-establishes the invariants: spans that
    /// ended up empty are dropped, and the last span is extended or a new one appended so coverage
    /// reaches the new `text_len`.
    ///
    /// # Where a span gets split
    ///
    /// Growing an insertion point in the middle of a styled run makes that run two runs of the same
    /// style separated by the inserted text, which may be a different style. So a span straddling the
    /// insertion point is *split* at it, and the two halves are separate entries.
    ///
    /// That is the only structural change an insert can make beyond shifting, and it is why
    /// `apply_insert` can grow `spans` by more than one: one straddle becomes two halves plus
    /// whatever the inserted region's style is.
    pub fn apply_insert_with(
        &mut self,
        offset: u32,
        len: u32,
        policy: SpanPolicy,
    ) -> Result<(), SpanError> {
        if offset > self.text_len {
            return Err(SpanError::OutOfBounds {
                offset: u64::from(offset),
                text_len: self.text_len,
            });
        }
        if len == 0 {
            return Ok(());
        }
        let insert_end = offset + len;

        // The inserted region's own style, decided from the *pre-insertion* layout -- which is what the
        // user was looking at when they pressed the key. See `style_around`.
        let inserted = self.style_around(offset, insert_end, policy);

        // # In place, with a write cursor
        //
        // The first version built a fresh `Vec` every call and assigned it back: one allocation per
        // keystroke, on the path FR-1.2 measures. The gate reported exactly one alloc and one dealloc
        // per character across a 4,000-character burst.
        //
        // A rewrite pass is safe here because the input is sorted and non-overlapping, so the output is
        // too. Each input span yields at most two outputs, and **at most one span can straddle** any
        // offset -- spans do not overlap -- so the output is at most one element longer than the input.
        // A single `write` cursor therefore suffices, and `truncate` fixes the length.
        //
        // # The two directions, over *pre-shift* coordinates
        //
        // Splitting first and shifting afterwards cannot implement `GrowIntoInsert`: the shift moves a
        // run's end from `offset` to `offset + len`, the split test then sees a span straddling
        // `[offset, offset+len)` and cuts it back at `offset` -- discarding the growth the policy had just
        // performed. Typing at the end of a bold run produced plain text under the policy that exists to
        // keep it bold. Doing both in one pass removes the ordering problem.
        // # Two phases, because in-place writing clobbers
        //
        // A single pass writing through a `write` cursor looks right and is not: when one span yields
        // two outputs -- a straddle -- `write` passes `read` and overwrites an entry the loop has not
        // read yet. That showed up as "index out of bounds: the len is 1 but the index is 1" on the
        // first rewrite.
        //
        // The fix is to use the fact that **at most one span can straddle a given offset**, because
        // spans do not overlap. So:
        //
        // * **Phase A** rewrites every span in its own slot -- strictly 1:1, no reordering, no length
        //   change, so nothing is clobbered -- and records the straddle's right-hand half if there is
        //   one.
        // * **Phase B** is a single `Vec::insert` for that half, which shifts the tail. One `insert`,
        //   and allocation-free while `len() < capacity`, which [`SPAN_HEADROOM`] guarantees for any
        //   realistic number of runs on a line.
        let mut outputs = 0usize;
        let mut split: Option<(usize, TextIntervalSpan)> = None;
        for i in 0..self.spans.len() {
            let sp = self.spans[i];
            let (f, c) = (sp.style_flags, sp.color_rgb);

            // The part before the insertion point. Under `GrowIntoInsert` a run ending *exactly* at the
            // insertion point absorbs the new bytes, which is the word-processor behaviour the policy is
            // named for.
            let left = if sp.start_byte < offset {
                let stop = if policy == SpanPolicy::GrowIntoInsert && sp.end_byte == offset {
                    insert_end
                } else {
                    sp.end_byte.min(offset)
                };
                (stop > sp.start_byte).then(|| TextIntervalSpan::styled(sp.start_byte, stop, f, c))
            } else {
                None
            };

            // The part after, shifted right by `len`.
            let right = if sp.end_byte > offset {
                let from = sp.start_byte.max(offset) + len;
                let to = sp.end_byte + len;
                (to > from).then(|| TextIntervalSpan::styled(from, to, f, c))
            } else {
                None
            };

            match (left, right) {
                (Some(l), Some(r)) => {
                    // A straddle. Phase A keeps the left half; phase B splices the right one in after it.
                    self.spans[i] = l;
                    split = Some((i + 1, r));
                    outputs += 2;
                }
                (Some(l), None) => {
                    self.spans[i] = l;
                    outputs += 1;
                }
                (None, Some(r)) => {
                    self.spans[i] = r;
                    outputs += 1;
                }
                (None, None) => {}
            }
        }
        self.spans.truncate(outputs);

        if let Some((at, r)) = split {
            self.spans.insert(at, r);
        }

        self.text_len += len;

        // Add the new run unless a grown or existing span already covers it.
        //
        // Under `GrowIntoInsert` the preceding run was extended over `[offset, insert_end)` above, so the
        // inserted bytes already have a span and adding a second produced an overlap -- the map reported
        // spans `0..5` and `3..8` on a 12-byte document and `check_invariants` rejected it. Under `Strict`
        // nothing covers it and the run is needed.
        if !self.spans.iter().any(|sp| sp.contains(offset)) {
            let at = self
                .spans
                .iter()
                .position(|sp| sp.start_byte >= insert_end)
                .unwrap_or(self.spans.len());
            self.spans.insert(
                at,
                TextIntervalSpan::styled(offset, insert_end, inserted.0, inserted.1),
            );
        }

        self.normalise()
    }

    /// The style the inserted region `[offset, insert_end)` should take, given the policy.
    ///
    /// Looks at the *pre-insertion* layout. This is where the two policies actually differ, and it needs
    /// no per-span special case:
    ///
    /// | layout at `offset` | `Strict` picks | `GrowIntoInsert` picks |
    /// |---|---|---|
    /// | a run ends here | the run after it | the run before it |
    /// | a run starts here | the run at `offset` | the run before it |
    ///
    /// Looking *backwards* is what "continue the run I am in" means, and only `GrowIntoInsert` does it,
    /// because under `Strict` an insertion deliberately stops the run it follows.
    fn style_around(&self, offset: u32, insert_end: u32, policy: SpanPolicy) -> (u16, u32) {
        if let Some(sp) = self.spans.iter().find(|s| s.contains(offset)) {
            return (sp.style_flags, sp.color_rgb);
        }
        if policy == SpanPolicy::GrowIntoInsert && offset > 0 {
            if let Some(sp) = self.spans.iter().find(|s| s.contains(offset - 1)) {
                return (sp.style_flags, sp.color_rgb);
            }
        }
        if let Some(sp) = self.spans.iter().find(|s| s.contains(insert_end)) {
            return (sp.style_flags, sp.color_rgb);
        }
        (0, 0)
    }

    /// Apply a deletion of `len` bytes at `offset`, and return what was removed.
    ///
    /// The returned [`TextIntervalSpan`]s are the styling of the deleted region, which is what undo
    /// needs: restoring the bytes is not enough, the formatting has to come back with them.
    pub fn apply_delete(
        &mut self,
        offset: u32,
        len: u32,
    ) -> Result<Vec<TextIntervalSpan>, SpanError> {
        if offset + len > self.text_len {
            return Err(SpanError::RangeOutOfBounds {
                start: u64::from(offset),
                end: u64::from(offset + len),
                text_len: self.text_len,
            });
        }
        if len == 0 {
            return Ok(Vec::new());
        }
        let delete_end = offset + len;

        // Record the styling of the region about to go, clipped per span -- but only if any of it is
        // styled.
        //
        // The plain case is the overwhelmingly common one (every Backspace in an unstyled document) and
        // it must not allocate, because Backspace is a keystroke. `filter_map(...).collect()` allocates
        // its capacity from the iterator's upper size hint even when it yields nothing, so the
        // `any(...)` guard comes first.
        let any_styled = self.spans.iter().any(|s| {
            let lo = s.start_byte.max(offset);
            let hi = s.end_byte.min(delete_end);
            hi > lo && (s.style_flags != 0 || s.color_rgb != 0)
        });
        let removed: Vec<TextIntervalSpan> = if any_styled {
            self.spans
                .iter()
                .filter_map(|s| {
                    let lo = s.start_byte.max(offset);
                    let hi = s.end_byte.min(delete_end);
                    if hi > lo {
                        Some(TextIntervalSpan::styled(lo, hi, s.style_flags, s.color_rgb))
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            Vec::new()
        };

        // Shift or clamp every boundary.
        for s in &mut self.spans {
            s.start_byte = collapse(s.start_byte, offset, delete_end);
            s.end_byte = collapse(s.end_byte, offset, delete_end);
        }
        self.spans.retain(|s| !s.is_empty());
        self.text_len -= len;

        // Collapse the hole the deletion left: the span before it and the span after it may now be
        // adjacent, and if they are stylistically identical they must merge or `style_at` would have
        // two candidates at the seam.
        self.normalise()?;
        Ok(removed)
    }

    /// Rebuild the map for a new document length, dropping spans past the end.
    ///
    /// For loading: the text arrives whole and the spans are built from scratch. Not for editing,
    /// which goes through [`apply_insert`](Self::apply_insert) / [`apply_delete`](Self::apply_delete).
    pub fn reset_to(&mut self, text_len: u32) {
        self.spans.clear();
        self.text_len = text_len;
        if text_len > 0 {
            self.spans.push(TextIntervalSpan::plain(0, text_len));
        }
    }

    /// Set the whole document to one style, collapsing to a single span.
    pub fn set_uniform(&mut self, style_flags: u16, color_rgb: u32) {
        self.spans.clear();
        if self.text_len > 0 {
            self.spans.push(TextIntervalSpan::styled(
                0,
                self.text_len,
                style_flags,
                color_rgb,
            ));
        }
    }

    /// Assert the map's invariants. Test-only.
    ///
    /// `text_len` is taken from the map itself, so this cannot be called with a length the map does
    /// not agree with -- the point of storing it is that the bounds checks have something to check
    /// against.
    #[cfg(test)]
    pub(crate) fn check_invariants(&self) {
        if self.spans.is_empty() {
            assert_eq!(
                self.text_len, 0,
                "an empty span map must describe an empty document"
            );
            return;
        }
        assert_eq!(
            self.spans[0].start_byte, 0,
            "coverage must start at byte 0; the map is gap-free"
        );
        assert_eq!(
            self.spans[self.spans.len() - 1].end_byte,
            self.text_len,
            "coverage must reach text_len"
        );
        for (i, s) in self.spans.iter().enumerate() {
            assert!(s.start_byte <= s.end_byte, "span {i} is inverted: {:?}", s);
            assert!(
                s.end_byte <= self.text_len,
                "span {i} ends at {} past text_len {}: {:?}",
                s.end_byte,
                self.text_len,
                s
            );
            if i > 0 {
                let prev = self.spans[i - 1];
                assert_eq!(
                    prev.end_byte,
                    s.start_byte,
                    "spans {} ({:?}) and {} ({:?}) do not abut, so the map has a gap or an \\
                     overlap",
                    i - 1,
                    prev,
                    i,
                    s
                );
                assert!(
                    !prev.same_style(s),
                    "spans {} and {} are adjacent and identical, so they should have merged",
                    i - 1,
                    i
                );
            }
        }
    }
}

/// Move a boundary across a deleted range `[lo, hi)`.
///
/// * below the range: unchanged
/// * above the range: shifted left by the range's length
/// * inside the range: clamped to `lo`, which is where the content either side now meets
#[inline]
fn collapse(boundary: u32, lo: u32, hi: u32) -> u32 {
    if boundary <= lo {
        boundary
    } else if boundary >= hi {
        boundary - (hi - lo)
    } else {
        lo
    }
}

impl SpanMap {
    /// Merge adjacent identical spans and re-extend coverage to `text_len`.
    ///
    /// The invariant restorer. Every mutator calls it, so the invariants are established in one place
    /// rather than re-derived per operation.
    ///
    /// # In place, because this runs on the keystroke path
    ///
    /// The first version drained `spans` into a fresh `Vec`, merged, and assigned it back. That is
    /// correct and it allocates on *every* insert and delete -- two allocations per keystroke, in the
    /// middle of the burst FR-1.2 requires to allocate nothing. The gate reported "typing 4,000
    /// characters through the Editor performed 8001 allocations", which is this function's
    /// `with_capacity` plus `apply_insert_with`'s.
    ///
    /// So: compact in place with a write index, then `truncate`. `Vec::retain` is also allocation-free
    /// but cannot *merge* two spans, and merging is what keeps `len()` meaning "number of distinct
    /// styles in use".
    fn normalise(&mut self) -> Result<(), SpanError> {
        let mut write = 0usize;
        for read in 0..self.spans.len() {
            let sp = self.spans[read];
            if sp.is_empty() {
                continue;
            }
            if write > 0 {
                let last = self.spans[write - 1];
                if last.end_byte == sp.start_byte && last.same_style(&sp) {
                    self.spans[write - 1].end_byte = sp.end_byte;
                    continue;
                }
            }
            if write != read {
                self.spans[write] = sp;
            }
            write += 1;
        }
        self.spans.truncate(write);

        // Re-extend coverage.
        if self.text_len == 0 {
            self.spans.clear();
            return Ok(());
        }
        if self.spans.is_empty() {
            self.spans.push(TextIntervalSpan::plain(0, self.text_len));
        } else {
            if self.spans[0].start_byte > 0 {
                // A hole at the front: fill it plain.
                let start = self.spans[0].start_byte;
                self.spans[0].start_byte = 0;
                self.spans.insert(0, TextIntervalSpan::plain(0, start));
            }
            let last = self.spans.len() - 1;
            self.spans[last].end_byte = self.text_len;
        }

        for sp in &self.spans {
            if sp.start_byte > sp.end_byte {
                return Err(SpanError::Invariant(format!("inverted span {sp:?}")));
            }
        }
        Ok(())
    }

    /// Install `spans` verbatim as the map over `text_len` bytes.
    ///
    /// # Why this exists, and why it validates rather than trusts
    ///
    /// This is the **load** path: the payload carries the span list as bytes and hands it back here.
    /// The obvious way to reconstruct a map from a list is to call [`style_range`](Self::style_range)
    /// once per span, and that does not work: `style_range` ends in [`normalise`](Self::normalise),
    /// which merges adjacent identically-styled spans and re-extends coverage. A document whose spans
    /// came back merged is a document whose runs are *equivalent* -- and if the list came from a
    /// payload, "equivalent" is not good enough, because the bytes on disk and the bytes in memory
    /// must round-trip to the same thing. So the list is installed as it stands.
    ///
    /// What it *is* checked for is every invariant `normalise` would have established: sorted,
    /// non-empty, non-overlapping, gap-free, and ending exactly at `text_len`. A payload that
    /// violates any of them is refused rather than repaired, because a repaired span map is a span
    /// map whose styles do not match what was saved, and the only symptom would be text that is the
    /// wrong colour.
    ///
    /// One exception is made for the empty case: an empty `spans` over a non-empty `text_len` is
    /// accepted and filled with one plain span, since that is the representation of "no styling" and
    /// it is what `SpanMap::plain` produces. Every other gap is an error.
    pub fn from_spans(spans: Vec<TextIntervalSpan>, text_len: u32) -> Result<Self, SpanError> {
        let mut m = Self { spans, text_len, read_through: 0 };
        if m.spans.is_empty() {
            if text_len > 0 {
                m.spans.push(TextIntervalSpan::plain(0, text_len));
            }
            return Ok(m);
        }
        if m.spans[0].start_byte != 0 {
            return Err(SpanError::Invariant(format!(
                "the first span starts at {} not 0",
                m.spans[0].start_byte
            )));
        }
        for (i, sp) in m.spans.iter().enumerate() {
            if sp.is_empty() {
                return Err(SpanError::Invariant(format!(
                    "span {i} covers no bytes: {sp:?}"
                )));
            }
            if sp.end_byte > text_len {
                return Err(SpanError::RangeOutOfBounds {
                    start: u64::from(sp.start_byte),
                    end: u64::from(sp.end_byte),
                    text_len,
                });
            }
            if i > 0 && m.spans[i - 1].end_byte != sp.start_byte {
                return Err(SpanError::Invariant(format!(
                    "span {i} starts at {} but span {} ends at {}",
                    sp.start_byte,
                    i - 1,
                    m.spans[i - 1].end_byte
                )));
            }
        }
        if let Some(last) = m.spans.last() {
            if last.end_byte != text_len {
                return Err(SpanError::Invariant(format!(
                    "the last span ends at {} but the text is {text_len} bytes",
                    last.end_byte
                )));
            }
        }
        Ok(m)
    }

    /// Apply a styling to `[start, end)`, splitting whatever spans straddle it.
    ///
    /// The one *authoring* operation, as against the two *maintenance* ones. Splitting is inherent
    /// here -- styling a range that crosses a boundary has to cut the span -- whereas
    /// [`apply_insert`](Self::apply_insert) only splits when text lands inside a run.
    pub fn style_range(
        &mut self,
        start: u32,
        end: u32,
        style_flags: u16,
        color_rgb: u32,
    ) -> Result<(), SpanError> {
        if start > end {
            return Err(SpanError::InvertedRange { start, end });
        }
        if end > self.text_len {
            return Err(SpanError::RangeOutOfBounds {
                start: u64::from(start),
                end: u64::from(end),
                text_len: self.text_len,
            });
        }
        if start == end {
            return Ok(());
        }
        let mut rebuilt = Vec::with_capacity(self.spans.len() + 2);
        for s in self.spans.drain(..) {
            if s.end_byte <= start || s.start_byte >= end {
                rebuilt.push(s);
                continue;
            }
            // Left remnant.
            if s.start_byte < start {
                rebuilt.push(TextIntervalSpan::styled(
                    s.start_byte,
                    start,
                    s.style_flags,
                    s.color_rgb,
                ));
            }
            // The styled region itself.
            let lo = s.start_byte.max(start);
            let hi = s.end_byte.min(end);
            rebuilt.push(TextIntervalSpan::styled(lo, hi, style_flags, color_rgb));
            // Right remnant.
            if s.end_byte > end {
                rebuilt.push(TextIntervalSpan::styled(
                    end,
                    s.end_byte,
                    s.style_flags,
                    s.color_rgb,
                ));
            }
        }
        self.spans = rebuilt;
        // A styled range can leave two adjacent identical spans, and it can leave the map gap-free
        // already; `normalise` merges and re-extends.
        self.spans.sort_by_key(|s| s.start_byte);
        self.normalise()
    }

    /// Remove styling from `[start, end)`, making it plain.
    pub fn clear_range(&mut self, start: u32, end: u32) -> Result<(), SpanError> {
        self.style_range(start, end, 0, 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOLD_BLUE: u16 = STYLE_BOLD;
    const RED: u32 = 0x00FF_0000;
    const BLUE: u32 = 0x0000_00FF;

    /// PRD.md §7.1's field widths, and the reason the struct is 16 and not 12.
    #[test]
    fn size_of_is_sixteen() {
        assert_eq!(std::mem::size_of::<TextIntervalSpan>(), 16);
        assert_eq!(std::mem::align_of::<TextIntervalSpan>(), 4);
        // `4 + 4 + 2 + 2 padding + 4`.
        assert_eq!(std::mem::size_of::<u32>() * 2 + 8, 16);
    }

    /// A `u16` colour would be 12 bytes -- the field list in a Phase 6 directive asked for that, and
    /// it is not the 16 the same directive and PRD.md §7.1 both require.
    #[test]
    fn a_u16_colour_would_not_be_sixteen_bytes() {
        #[repr(C)]
        struct Narrow {
            start_byte: u32,
            end_byte: u32,
            style_flags: u16,
            color_rgb: u16,
        }
        assert_eq!(
            std::mem::size_of::<Narrow>(),
            12,
            "which is why color_rgb stays a u32"
        );
        assert_ne!(std::mem::size_of::<Narrow>(), 16);
    }

    #[test]
    fn a_new_map_is_gap_free_and_plain() {
        let m = SpanMap::plain(10);
        m.check_invariants();
        assert_eq!(m.len(), 1);
        assert_eq!(m.style_at(0).style_flags, 0);
        assert_eq!(m.style_at(9).style_flags, 0);
        assert_eq!(
            m.style_at(10).style_flags,
            0,
            "the caret's offset is not an error"
        );
        let empty = SpanMap::plain(0);
        empty.check_invariants();
        assert!(empty.is_empty());
    }

    #[test]
    fn styling_a_range_splits_the_span_it_crosses() {
        let mut m = SpanMap::plain(10);
        m.style_range(2, 6, BOLD_BLUE, RED).expect("style");
        m.check_invariants();
        assert_eq!(m.len(), 3, "plain, bold, plain");
        assert_eq!(m.spans()[1].start_byte, 2);
        assert_eq!(m.spans()[1].end_byte, 6);
        assert_eq!(m.spans()[1].style_flags, BOLD_BLUE);
        assert_eq!(m.spans()[1].color_rgb, RED);
        assert!(m.has_style_at(3, STYLE_BOLD));
        assert!(!m.has_style_at(1, STYLE_BOLD));
        assert!(
            !m.has_style_at(6, STYLE_BOLD),
            "the end boundary is exclusive"
        );
    }

    #[test]
    fn styling_an_entire_range_yields_one_span() {
        let mut m = SpanMap::plain(10);
        m.style_range(0, 10, STYLE_CODE, BLUE).expect("style");
        m.check_invariants();
        assert_eq!(m.len(), 1);
        assert_eq!(m.spans()[0].style_flags, STYLE_CODE);
    }

    /// Adjacent identical runs must merge, or `len()` counts one style twice and the seam has two
    /// candidates for `style_at`.
    #[test]
    fn adjacent_identical_spans_merge() {
        let mut m = SpanMap::plain(10);
        m.style_range(2, 5, STYLE_BOLD, RED).expect("a");
        assert_eq!(m.len(), 3, "plain, bold, plain");
        m.style_range(5, 8, STYLE_BOLD, RED).expect("b");
        m.check_invariants();
        // Three, not two: the two bold runs merged, and the plain prefix and suffix are still there.
        // An earlier version of this test expected 2, having forgotten that `plain(10)` starts as one
        // span and styling 2..5 cuts it into three.
        assert_eq!(m.len(), 3, "plain 0..2, bold 2..8, plain 8..10");
        assert_eq!(m.spans()[1].start_byte, 2);
        assert_eq!(
            m.spans()[1].end_byte,
            8,
            "the two bold runs merged into one"
        );
        assert!(m.has_style_at(4, STYLE_BOLD));
        assert!(m.has_style_at(5, STYLE_BOLD));
    }

    /// The core requirement: an insert shifts boundaries strictly past the edit point.
    #[test]
    fn an_insert_shifts_boundaries_past_the_edit_point() {
        let mut m = SpanMap::plain(10);
        m.style_range(4, 8, STYLE_BOLD, RED).expect("style");
        m.check_invariants();

        // Insert 3 bytes at 0, well before the span.
        m.apply_insert(0, 3).expect("insert");
        m.check_invariants();
        assert_eq!(m.text_len(), 13);
        assert_eq!(m.spans()[1].start_byte, 7, "4 -> 7");
        assert_eq!(m.spans()[1].end_byte, 11, "8 -> 11");

        // Insert 2 bytes at 5, inside the plain prefix.
        m.apply_insert(5, 2).expect("insert");
        m.check_invariants();
        assert_eq!(m.text_len(), 15);
        assert_eq!(m.spans()[1].start_byte, 9, "7 -> 9");
        assert_eq!(m.spans()[1].end_byte, 13);
    }

    /// The two policies differ only at a boundary, and the difference is exactly the word-processor
    /// behaviour vs the requirement's wording.
    #[test]
    fn the_insert_policy_decides_whether_a_run_continues() {
        // Strict: typing at the end of a bold run starts a plain run.
        let mut strict = SpanMap::plain(10);
        strict.style_range(0, 4, STYLE_BOLD, RED).expect("style");
        strict
            .apply_insert_with(4, 1, SpanPolicy::Strict)
            .expect("insert");
        strict.check_invariants();
        assert!(!strict.has_style_at(4, STYLE_BOLD), "strict stops the run");
        assert!(strict.has_style_at(3, STYLE_BOLD));

        // GrowIntoInsert: typing at the end of a bold run extends it.
        let mut grow = SpanMap::plain(10);
        grow.style_range(0, 4, STYLE_BOLD, RED).expect("style");
        grow.apply_insert_with(4, 1, SpanPolicy::GrowIntoInsert)
            .expect("insert");
        grow.check_invariants();
        assert!(grow.has_style_at(4, STYLE_BOLD), "grow continues the run");
    }

    /// An insert inside a styled run splits it, so the inserted text can carry its own style.
    #[test]
    fn an_insert_inside_a_run_splits_it() {
        let mut m = SpanMap::plain(10);
        m.style_range(0, 10, STYLE_BOLD, RED).expect("style");
        m.apply_insert(5, 2).expect("insert");
        m.check_invariants();
        assert_eq!(m.text_len(), 12);
        assert!(m.has_style_at(4, STYLE_BOLD), "before the insertion");
        assert!(m.has_style_at(7, STYLE_BOLD), "after it");
        // The inserted bytes are bold too, and that is what `Strict` means rather than a bug: it says
        // the inserted text inherits the *following* span's style, and here the following span is the
        // same bold run. `Strict` stops a run at a boundary, not in the middle of one -- there is no
        // boundary inside a single run to stop at.
        //
        // An earlier version of this test asserted the inserted region was plain, and failed with
        // "left: 1, right: 0". It was testing a behaviour no policy has.
        assert!(
            m.has_style_at(5, STYLE_BOLD),
            "Strict inherits the following span, which is bold"
        );
        assert!(m.has_style_at(6, STYLE_BOLD));
        assert_eq!(
            m.len(),
            1,
            "and a run split by its own continuation is still one run"
        );

        // The boundary case `Strict` does change: typing where a bold run *ends* starts a plain run.
        let mut b = SpanMap::plain(10);
        b.style_range(0, 5, STYLE_BOLD, RED).expect("style");
        b.apply_insert(5, 2).expect("insert at the boundary");
        b.check_invariants();
        assert!(b.has_style_at(4, STYLE_BOLD));
        assert_eq!(
            b.style_at(5).style_flags,
            0,
            "Strict stops the run at its end boundary"
        );
        assert_eq!(b.style_at(6).style_flags, 0);
    }

    /// The core requirement: a delete collapses boundaries and closes the gap it leaves.
    #[test]
    fn a_delete_collapses_boundaries_inside_the_removed_range() {
        let mut m = SpanMap::plain(10);
        m.style_range(0, 3, STYLE_BOLD, RED).expect("a");
        m.style_range(6, 10, STYLE_ITALIC, BLUE).expect("b");
        m.check_invariants();

        // Delete 4..8: entirely inside the italic run's front.
        m.apply_delete(4, 4).expect("delete");
        m.check_invariants();
        assert_eq!(m.text_len(), 6);
        // bold 0..3 untouched; plain 3..6 loses its tail 4..6; italic 6..10 collapses to 4..6.
        assert_eq!(m.spans()[0].start_byte, 0);
        assert_eq!(m.spans()[0].end_byte, 3, "bold is untouched");
        assert_eq!(m.spans()[1].start_byte, 3);
        assert_eq!(m.spans()[1].end_byte, 4, "plain 3..6, its tail deleted");
        assert_eq!(m.spans()[2].start_byte, 4, "italic 6 -> 4");
        assert_eq!(m.spans()[2].end_byte, 6, "italic 10 -> 6");
        assert!(m.has_style_at(1, STYLE_BOLD));
        assert!(m.has_style_at(5, STYLE_ITALIC));
    }

    /// A delete that straddles two differently-styled spans must not leave a gap or an overlap, and
    /// the survivors must abut.
    #[test]
    fn a_delete_across_a_style_boundary_keeps_the_map_gap_free() {
        let mut m = SpanMap::plain(10);
        m.style_range(0, 4, STYLE_BOLD, RED).expect("a");
        m.style_range(4, 10, STYLE_ITALIC, BLUE).expect("b");
        m.check_invariants();

        m.apply_delete(2, 4)
            .expect("delete bytes 2..6, crossing the seam");
        m.check_invariants();
        assert_eq!(m.text_len(), 6);
        // Bold 0..2, then italic from 2.
        assert_eq!(m.len(), 2);
        assert!(m.has_style_at(1, STYLE_BOLD));
        assert!(m.has_style_at(2, STYLE_ITALIC));
        assert_eq!(m.spans()[0].end_byte, m.spans()[1].start_byte, "they abut");
    }

    /// A delete spanning the *whole* of a span removes it, rather than leaving a zero-length entry.
    #[test]
    fn deleting_a_whole_span_removes_it() {
        let mut m = SpanMap::plain(10);
        m.style_range(2, 5, STYLE_BOLD, RED).expect("style");
        m.check_invariants();
        m.apply_delete(2, 3).expect("delete");
        m.check_invariants();
        assert_eq!(m.len(), 1, "only the plain run remains");
        assert_eq!(m.spans()[0].len(), 7);
    }

    /// Deleting across a seam where both sides share a style must merge them, not leave two adjacent
    /// identical spans.
    #[test]
    fn a_delete_that_joins_two_identical_runs_merges_them() {
        let mut m = SpanMap::plain(10);
        m.style_range(0, 3, STYLE_BOLD, RED).expect("a");
        m.style_range(3, 10, STYLE_CODE, RED).expect("b");
        // Same colour, different flags.
        assert_eq!(m.len(), 2);
        m.apply_delete(2, 2)
            .expect("delete 2..4, touching the seam");
        m.check_invariants();
        // The seam's neighbours now abut and both carry RED but not the same flags, so they must stay
        // separate.
        assert_eq!(m.len(), 2);

        // Now make them genuinely identical and delete between two identical runs.
        let mut n = SpanMap::plain(10);
        n.style_range(0, 3, STYLE_BOLD, RED).expect("a");
        n.apply_insert_with(3, 2, SpanPolicy::GrowIntoInsert)
            .expect("extend");
        // `GrowIntoInsert` at offset 3 extended the bold run from 0..3 to 0..5, so the document is now
        // 12 bytes. Styling 3..12 makes the whole document one bold run and the map collapses to a
        // single span.
        //
        // An earlier version styled only 3..10 -- the length before the insert -- and asserted one
        // span, getting two. The extra one was plain 10..12: correct, because those two bytes had
        // been inserted and never styled.
        n.style_range(3, 12, STYLE_BOLD, RED).expect("b");
        n.check_invariants();
        assert_eq!(n.len(), 1, "the whole document is one bold run");
        // Delete 9 of the document's 12 bytes, leaving 3. An earlier version deleted 8 and asserted
        // 2 remained, which is arithmetic: 12 - 8 = 4, and the assertion failed with "left: 4".
        n.apply_delete(1, 9).expect("delete most of it");
        n.check_invariants();
        assert_eq!(n.text_len(), 3, "12 bytes, 9 deleted");
        assert!(
            n.has_style_at(0, STYLE_BOLD),
            "the surviving head is still bold"
        );
        assert_eq!(m.len(), 2, "and the first half still has two distinct runs");
    }

    /// Undo needs the deleted region's styling back, not just its bytes.
    #[test]
    fn a_delete_returns_the_removed_regions_styling() {
        let mut m = SpanMap::plain(10);
        m.style_range(0, 4, STYLE_BOLD, RED).expect("a");
        m.style_range(4, 10, STYLE_ITALIC, BLUE).expect("b");

        let removed = m.apply_delete(2, 4).expect("delete");
        assert_eq!(removed.len(), 2, "one clip per straddled span");
        assert_eq!(removed[0].start_byte, 2);
        assert_eq!(removed[0].end_byte, 4, "clipped to the bold run");
        assert_eq!(removed[0].style_flags, STYLE_BOLD);
        assert_eq!(removed[0].color_rgb, RED);
        assert_eq!(removed[1].start_byte, 4);
        assert_eq!(removed[1].end_byte, 6, "clipped to the italic run");
        assert_eq!(removed[1].style_flags, STYLE_ITALIC);
    }

    #[test]
    fn out_of_range_edits_are_refused() {
        let mut m = SpanMap::plain(10);
        assert!(matches!(
            m.apply_insert(11, 1),
            Err(SpanError::OutOfBounds { .. })
        ));
        assert!(matches!(
            m.apply_delete(8, 5),
            Err(SpanError::RangeOutOfBounds { .. })
        ));
        assert!(matches!(
            m.style_range(5, 3, 0, 0),
            Err(SpanError::InvertedRange { start: 5, end: 3 })
        ));
        assert!(matches!(
            m.style_range(5, 20, 0, 0),
            Err(SpanError::RangeOutOfBounds { .. })
        ));
        m.check_invariants();
        // A zero-length edit is a no-op, not an error.
        m.apply_insert(5, 0).expect("no-op");
        m.apply_delete(5, 0).expect("no-op");
        m.style_range(5, 5, STYLE_BOLD, RED).expect("no-op");
        assert_eq!(m.text_len(), 10);
    }

    #[test]
    fn runs_in_returns_clipped_pieces() {
        let mut m = SpanMap::plain(20);
        m.style_range(0, 5, STYLE_BOLD, RED).expect("a");
        m.style_range(10, 15, STYLE_ITALIC, BLUE).expect("b");
        // Three runs overlap 3..12, not two: the map is gap-free, so plain 5..10 sits between the
        // bold and italic runs. An earlier version expected two and reported "left: 3, right: 2" --
        // it had forgotten its own gap-free invariant, which makes a plain region a run too.
        let runs = m.runs_in(3, 12);
        assert_eq!(runs.len(), 3);
        assert_eq!((runs[0].1, runs[0].2), (3, 5), "clipped at the left");
        assert_eq!((runs[1].1, runs[1].2), (5, 10), "the plain run between");
        assert_eq!((runs[1].0.style_flags, runs[1].0.color_rgb), (0, 0));
        assert_eq!((runs[2].1, runs[2].2), (10, 12), "clipped at the right");
        // 6..9 is the *plain* run between the two styled ones. The map is gap-free, so a plain region
        // is a run with `style_flags == 0`, not an absence of runs -- an earlier version of this test
        // asserted it was empty and contradicted the invariant it was written against.
        let plain = m.runs_in(6, 9);
        assert_eq!(plain.len(), 1, "plain text is still a run");
        assert_eq!((plain[0].1, plain[0].2), (6, 9));
        assert_eq!(plain[0].0.style_flags, 0);
        assert_eq!(plain[0].0.color_rgb, 0);
        assert!(m.runs_in(5, 5).is_empty(), "an empty range yields nothing");
    }

    /// The flag-to-face mapping, which is what ties the span map to the Phase 4 atlas.
    #[test]
    fn flags_map_onto_the_atlas_faces() {
        let s = |f| TextIntervalSpan::styled(0, 1, f, 0).atlas_style();
        assert_eq!(s(0), 0, "Inter Regular");
        assert_eq!(s(STYLE_BOLD), 1, "Inter Bold");
        assert_eq!(s(STYLE_ITALIC), 2, "Inter Italic");
        assert_eq!(s(STYLE_CODE), 3, "JetBrains Mono");
        // Bold wins over Italic because the atlas has no bold-italic face.
        assert_eq!(s(STYLE_BOLD | STYLE_ITALIC), 1);
        assert_eq!(
            s(STYLE_CODE | STYLE_BOLD),
            3,
            "code wins, so code is always monospaced"
        );
        assert!(TextIntervalSpan::styled(0, 1, STYLE_HEADER, 0).is_header());
    }

    /// A long randomised edit sequence, checking the invariants after every step. The property tests
    /// that a unit test per operation cannot give: that the operations compose.
    #[test]
    fn invariants_survive_a_random_edit_sequence() {
        // A deterministic xorshift, so a failure is reproducible from the seed.
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move |n: u32| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % u64::from(n.max(1))) as u32
        };

        let mut m = SpanMap::plain(64);
        m.check_invariants();
        for step in 0..2_000u32 {
            if m.text_len() < 8 || next(2) == 0 {
                let at = next(m.text_len());
                let len = 1 + next(4);
                m.apply_insert(at, len).unwrap_or_else(|e| {
                    panic!("insert at {at} of {} at step {step}: {e}", m.text_len())
                });
            } else {
                let at = next(m.text_len() - 4);
                let len = 1 + next(4);
                if at + len <= m.text_len() {
                    m.apply_delete(at, len).unwrap_or_else(|e| {
                        panic!("delete {at}+{len} of {} at step {step}: {e}", m.text_len())
                    });
                }
            }
            m.check_invariants();
            // And `style_at` must agree with a walk.
            for offset in 0..m.text_len() {
                let a = m.style_at(offset);
                let b = m
                    .spans()
                    .iter()
                    .find(|s| s.contains(offset))
                    .copied()
                    .unwrap_or_else(|| panic!("no span contains {offset}"));
                assert_eq!(
                    a.style_flags, b.style_flags,
                    "style_at({offset}) at step {step}"
                );
                assert_eq!(a.color_rgb, b.color_rgb, "colour at step {step}");
            }
        }
        assert!(
            m.text_len() > 0,
            "the sequence should not have emptied the document"
        );
    }

    /// The renderer-facing invariant: sum of span lengths equals the document length, always.
    #[test]
    fn span_lengths_sum_to_the_document_length() {
        let mut m = SpanMap::plain(100);
        for (i, (a, b)) in [(10u32, 20u32), (25, 30), (40, 90)].iter().enumerate() {
            m.style_range(*a, *b, STYLE_BOLD << (i % 4), RED)
                .expect("style");
            m.check_invariants();
            let sum: u32 = m.spans().iter().map(TextIntervalSpan::len).sum();
            assert_eq!(sum, 100, "after styling {a}..{b}");
        }
    }

    #[test]
    fn reset_and_uniform_replace_the_whole_map() {
        let mut m = SpanMap::plain(10);
        m.style_range(2, 5, STYLE_BOLD, RED).expect("style");
        m.set_uniform(STYLE_CODE, BLUE);
        m.check_invariants();
        assert_eq!(m.len(), 1);
        assert!(m.has_style_at(3, STYLE_CODE));

        m.reset_to(40);
        m.check_invariants();
        assert_eq!(m.text_len(), 40);
        assert_eq!(m.len(), 1);
        assert_eq!(m.style_at(0).style_flags, 0);
    }

    #[test]
    fn clear_range_makes_text_plain() {
        let mut m = SpanMap::plain(10);
        m.style_range(0, 10, STYLE_BOLD, RED).expect("style");
        m.clear_range(3, 6).expect("clear");
        m.check_invariants();
        assert!(!m.has_style_at(4, STYLE_BOLD));
        assert!(m.has_style_at(2, STYLE_BOLD));
        assert!(m.has_style_at(6, STYLE_BOLD));
    }
}
