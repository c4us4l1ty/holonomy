//! Phase 9B: where a formula is, given nothing but the document's bytes.
//!
//! A math span is the text between a pair of `$$` delimiters. Ctrl+M inserts both and leaves the
//! caret between them, so the delimiters are real bytes the user can see, delete and type around.
//!
//! # Why the delimiters are in the document rather than in a side table
//!
//! Because a span *is* derivable from the bytes, and that is the whole argument. Phase 9A learned the
//! opposite lesson the hard way: a table's shape is **not** derivable, because a 2x3 grid and a 1x6
//! grid are the same six separator bytes, so the editor has to carry the shape alongside the text and
//! the session has to keep the two in step. A formula has no such ambiguity -- `$$` opens and the
//! next `$$` closes -- so a scanner is the entire implementation, and it cannot disagree with the
//! document because it is computed from the document.
//!
//! The payoff is that undo, redo, save, load, export and paste need no special case. A `Vec<MathSpan>`
//! beside the editor is a second source of truth, and every one of those operations would have to
//! re-derive it; a scan has nothing to re-derive.
//!
//! # What is deliberately not here
//!
//! No nesting, no escaping, and no `\[ \]` inline-vs-display distinction. A `$$` inside a span closes
//! it. That is a limitation rather than an oversight: nesting would need a stack, escaping would need a
//! rule for "is this dollar escaped", and neither is reachable from Ctrl+M, which always inserts a
//! balanced pair. Both are noted rather than half-built, because a half-built escape rule is worse
//! than none -- it makes `\$$` mean two different things depending on which function looked.
//!
//! # No allocation
//!
//! [`math_span_at`] returns one [`MathSpan`] and [`for_each_math_span`] takes a closure. Neither
//! collects into a `Vec`, because both run on the paint path and Phase 9A's `publish_line_heights`
//! already pays one `Vec` per paint that this module exists partly to avoid repeating.

/// A `$$`-delimited formula.
///
/// `start` points at the opening `$` and `end` is one past the closing `$`, so a closed span is at
/// least [`MIN_SPAN`] long and the inner LaTeX is [`MathSpan::inner`].
///
/// # Why `closed` is a field and not inferred
///
/// An unpaired `$$` — typed, not yet closed — has no closing delimiter, so `end` is one past the end
/// of the line rather than one past a `$`. The first version left `closed` out and computed `inner()`
/// as `start + 2 .. end - 2` unconditionally, which silently ate the last two bytes of every
/// half-written formula: `$$x^2` reported the LaTeX as `x` rather than `x^2`. A separate
/// `MathSpan::unpaired` constructor would have had the same bug in one place instead of in
/// `inner()`, which is the place every caller actually goes through, so the flag is on the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MathSpan {
    /// Offset of the opening `$`.
    pub start: u32,
    /// One past the closing `$`, or one past the end of the line if `closed` is false.
    pub end: u32,
    /// Whether a closing `$$` was found.
    pub closed: bool,
}

/// The delimiter that opens and closes a formula.
pub const DELIM: &[u8; 2] = b"$$";

/// The shortest possible *closed* span: `$$$$`, an empty formula.
pub const MIN_SPAN: u32 = 4;

impl MathSpan {
    /// The LaTeX between the delimiters.
    pub fn inner(&self) -> core::ops::Range<usize> {
        let lo = self.start as usize + DELIM.len();
        let hi = if self.closed {
            // A closed span always has both delimiters, so this cannot underflow: `end >= start + 4`.
            self.end as usize - DELIM.len()
        } else {
            self.end as usize
        };
        lo..hi.max(lo)
    }

    /// True when `offset` is a caret position *inside* the formula.
    ///
    /// Inclusive at both ends, and the reason is that a caret offset is "where the next character
    /// goes". At `start + 2` the next character would land at the first byte of the LaTeX; at the
    /// inner end it would land on the closing `$`. Both are "editing the formula", and treating
    /// either as outside would collapse the span the instant Ctrl+M ran — the caret starts at
    /// `start + 2`.
    pub fn contains(&self, offset: u32) -> bool {
        let inner = self.inner();
        let lo = inner.start as u32;
        let hi = inner.end as u32;
        offset >= lo && offset <= hi
    }

    /// True when the formula has no LaTeX in it yet.
    pub fn is_empty(&self) -> bool {
        self.inner().is_empty()
    }
}

/// True when a `$$` opens at `at`.
#[inline]
fn opens(bytes: &[u8], at: usize) -> bool {
    bytes.len() >= at + 2 && bytes[at] == b'$' && bytes[at + 1] == b'$'
}

/// The span containing `offset`, if there is one.
///
/// No allocation, and it stops at the first match — a document with a thousand formulas is walked
/// only as far as the one under the caret.
pub fn math_span_at(bytes: &[u8], offset: u32) -> Option<MathSpan> {
    let mut found = None;
    for_each_math_span(bytes, |span| {
        if span.contains(offset) {
            found = Some(span);
        }
    });
    found
}

/// Call `f` for every span in the document, in order.
///
/// The allocation-free counterpart to collecting into a `Vec`. Passes every span rather than taking a
/// line range because filtering by line means re-deriving which line a byte is on, which is the
/// session's job and needs its own geometry; keeping that out of here is why this function is four
/// lines long.
///
/// # An unpaired `$$` runs to the end of its line
///
/// Typing `$$` and not closing it is the normal intermediate state while writing, and a scanner that
/// dropped it would make the formula blink out of existence at the moment the user is halfway through
/// it. So a `$$` with no partner closes at the next newline, or at the end of the document.
///
/// That is a rendering decision, not a parse: the span still carries both delimiters, so `end` is one
/// past the second `$` of the line's closing pair — or, for an unpaired opener, one past the `$` that
/// is there. Callers that need the LaTeX use [`MathSpan::inner`], which is correct either way.
pub fn for_each_math_span(bytes: &[u8], mut f: impl FnMut(MathSpan)) {
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if !opens(bytes, i) {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i + 2;
        // Find the closing `$$`.
        let close = loop {
            if j + 1 >= bytes.len() {
                break None;
            }
            if opens(bytes, j) {
                break Some(j);
            }
            // An unpaired opener stops at the line end rather than swallowing the rest of the document.
            if bytes[j] == b'\n' {
                break None;
            }
            j += 1;
        };
        match close {
            Some(c) => {
                f(MathSpan {
                    start: start as u32,
                    end: (c + 2) as u32,
                    closed: true,
                });
                i = c + 2;
            }
            None => {
                // No partner: the span runs to the end of this line, or to the end of the document.
                let line_end = bytes[start + 2..]
                    .iter()
                    .position(|&b| b == b'\n')
                    .map_or(bytes.len(), |p| start + 2 + p);
                f(MathSpan {
                    start: start as u32,
                    end: line_end as u32,
                    closed: false,
                });
                // `line_end` is at least `start + 2`, but the clamp is here so the loop's advance is
                // unconditional: a span that did not move `i` forward would spin forever, and the
                // cheapest way to make that impossible is to not rely on the arithmetic.
                i = line_end.max(start + 2);
            }
        }
    }
}

/// How many spans the document holds.
///
/// For tests and the status bar. The paint path uses [`for_each_math_span`].
pub fn math_span_count(bytes: &[u8]) -> usize {
    let mut n = 0;
    for_each_math_span(bytes, |_| n += 1);
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_balanced_pair_is_one_span() {
        let doc = b"before $$x^2$$ after";
        let mut seen = Vec::new();
        for_each_math_span(doc, |s| seen.push(s));
        assert_eq!(seen.len(), 1, "one `$$`-delimited run is one span");
        assert_eq!(seen[0].start, 7);
        assert_eq!(seen[0].start, 7, "the opening `$` is at 7");
        assert_eq!(seen[0].end, 14, "one past the closing `$$` at 12..14");
        assert!(seen[0].closed);
        assert_eq!(&doc[seen[0].inner()], b"x^2");
        assert_eq!(&doc[seen[0].inner()], b"x^2");
    }

    #[test]
    fn two_spans_on_one_line_are_separate() {
        let doc = b"$$a$$ and $$b$$";
        let mut seen = Vec::new();
        for_each_math_span(doc, |s| seen.push(s));
        assert_eq!(
            seen.len(),
            2,
            "the second `$$` closes the first, it does not open"
        );
        assert_eq!(&doc[seen[0].inner()], b"a");
        assert_eq!(&doc[seen[1].inner()], b"b");
    }

    #[test]
    fn the_caret_position_ctrl_m_leaves_counts_as_inside() {
        // Ctrl+M inserts `$$$$` and puts the caret at start + 2.
        let doc = b"$$$$";
        let span = math_span_at(doc, 2).expect("the caret Ctrl+M leaves is inside");
        assert!(span.is_empty(), "`$$$$` has no LaTeX yet");
        assert_eq!(span.start, 0);
        assert_eq!(span.end, 4);
    }

    #[test]
    fn an_unpaired_delimiter_runs_to_the_end_of_its_line() {
        let doc = b"$$x^2\nplain text\n";
        let mut seen = Vec::new();
        for_each_math_span(doc, |s| seen.push(s));
        assert_eq!(seen.len(), 1, "a half-written formula is still a formula");
        assert_eq!(seen[0].start, 0);
        assert!(
            !seen[0].closed,
            "there is no closing `$$`, and pretending otherwise is what made `inner()` eat the \
             last two bytes of every half-written formula"
        );
        assert_eq!(
            seen[0].end, 5,
            "it stops *at* the newline rather than swallowing the paragraph below"
        );
        assert_eq!(
            &doc[seen[0].inner()],
            b"x^2",
            "the whole tail of the line is LaTeX, because there is no delimiter to trim"
        );
        assert_eq!(&doc[seen[0].inner()], b"x^2");
    }

    #[test]
    fn a_single_dollar_is_not_a_delimiter() {
        let doc = b"it costs $5 and $6";
        assert_eq!(
            math_span_count(doc),
            0,
            "a lone `$` is currency, and a scanner that paired them would eat the sentence"
        );
    }

    #[test]
    fn deleting_the_closing_delimiter_leaves_the_formula_editable() {
        // The behavior that matters: backspacing over a `$$` must not make the formula vanish,
        // because that is what the user sees when they retype it.
        let doc = b"$$x^2$$";
        let closed = math_span_at(doc, 4).expect("the caret is inside");
        assert!(closed.closed);
        assert_eq!(&doc[closed.inner()], b"x^2");

        let doc = b"$$x^2$";
        let unpaired = math_span_at(doc, 4).expect("still inside after a backspace");
        assert!(!unpaired.closed, "the closing `$$` is gone");
        assert_eq!(
            &doc[unpaired.inner()],
            b"x^2$",
            "the surviving `$` is part of the LaTeX, not a delimiter"
        );
    }

    #[test]
    fn a_span_ends_when_its_line_does_even_with_text_after_it() {
        let doc = b"$$a$$ then $$b\nnext line $$c$$";
        let mut seen = Vec::new();
        for_each_math_span(doc, |s| seen.push(s));
        assert_eq!(seen.len(), 3, "two closed spans and one unpaired, per line");
        assert_eq!(&doc[seen[0].inner()], b"a");
        assert!(
            !seen[1].closed,
            "`$$b` opens on line 1 and never closes there"
        );
        assert_eq!(&doc[seen[1].inner()], b"b");
        assert_eq!(&doc[seen[2].inner()], b"c");
        assert!(
            seen[2].start > seen[1].start,
            "scanning resumed on the next line rather than re-reading line 1"
        );
    }
}
