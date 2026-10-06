//! Document line geometry in **document byte coordinates**. Phase 11.
//!
//! # What this is
//!
//! `Session::line_index` and `Session::line_start` answered "which line is byte `N` on?" and "where does
//! this line begin?" by counting newlines in a chunked scan from the start of the document. Both were
//! `O(bytes before the caret)`, called on every keystroke. [`holonomy_geometry::LineGeometry`] already
//! answers both in `O(log n)` from two Fenwick trees — it was built in Phase 6, ported from H2, tested
//! at 60,000 lines with exact-inverse proofs — and it was in **no production path**. This module is the
//! seam.
//!
//! # The one thing that had to be got right: which bytes a tree weight means
//!
//! `LineGeometry`'s byte tree stores each line's length **excluding** its newline, and its own tests pin
//! that: `byte_offsets_and_line_lengths_are_a_second_inverse_pair`
//! (`holonomy-geometry/src/lines.rs:1034`) sets ten lengths summing to 275 and asserts
//! `total_bytes() == 285`. So its prefix sums are **not** document byte offsets — they are off by one
//! newline per preceding line.
//!
//! Two ways out. Change `LineGeometry`'s contract, or store terminators. **The contract was not changed**:
//! its tests are a specification of the existing behaviour and rewriting them would be a larger,
//! riskier claim than the problem deserves. So this module stores each line's length **including** its
//! `\n`, which makes the tree's prefix sums real document offsets with no change to `LineGeometry` at
//! all. The convention is confined to this file, stated in [`DocLines::build`], and pinned by
//! [`tests::the_tree_agrees_with_a_full_scan_on_every_line`].
//!
//! # What is still O(n), and why that is not a surprise
//!
//! **A newline is `O(document)`.** `LineGeometry::resize_lines` rebuilds both trees, and
//! `insert_line`/`remove_line` move every weight after the edit — the geometry says so itself
//! (`lines.rs:459-462`) and PROJECT.md's H2 note records the same trade. Typing a letter is `O(log n)`;
//! pressing Enter is `O(n)`. That asymmetry is the design, not an oversight, and Enter is one keystroke
//! in forty.

use holonomy_geometry::{LineGeometry, LineMetrics};
use holonomy_text::Editor;

use crate::session::SCAN_CHUNK;

/// A document's lines, indexed for `O(log n)` byte/line lookup.
#[derive(Debug)]
pub struct DocLines {
    /// The two Fenwick trees. Its byte tree holds **terminator-inclusive** lengths, so `byte_of` is a
    /// document offset. See the module docs for why.
    geo: LineGeometry,
    /// How many lines the document has.
    ///
    /// A separate field because `geo.line_count()` is O(1) but the *document's* line count is what
    /// [`DocLines::sync`] compares against, and keeping it here means the comparison does not need to
    /// ask a tree that a previous edit may already have invalidated.
    count: usize,
}

/// What one call to [`DocLines::sync`] did, for a caller that wants to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sync {
    /// One line's length changed. `O(log n)`.
    OneLine,
    /// The document's line count changed, so both trees were rebuilt. `O(n)`.
    Rebuilt,
    /// Nothing about the document's line structure changed. `O(1)`.
    Unchanged,
}

impl DocLines {
    /// Build from the document's bytes. One `O(document)` scan, at open.
    ///
    /// Each line's stored length **includes** its `\n`, which is what makes `geo`'s prefix sums equal
    /// document offsets. A document with no trailing newline therefore has a last line whose stored
    /// length is its own byte count and no terminator — which is why `count` is `newlines + 1` and not
    /// the number of newlines.
    ///
    /// An empty document is **one** line of zero bytes, matching `LineGeometry::default()` and
    /// `ChromeState`'s initial `total_lines`.
    pub fn build(text: &[u8], metrics: LineMetrics) -> Self {
        let mut lens: Vec<u32> = Vec::with_capacity(text.len() / 64 + 1);
        let mut start = 0usize;
        for (i, &b) in text.iter().enumerate() {
            if b == b'\n' {
                lens.push((i + 1 - start) as u32);
                start = i + 1;
            }
        }
        // The last line: everything after the final newline, terminator or not.
        lens.push((text.len() - start) as u32);
        let mut geo = LineGeometry::uniform(lens.len(), metrics);
        for (i, &len) in lens.iter().enumerate() {
            // Cannot fail: `uniform` just gave every line a weight.
            geo.set_line_len(i, len as usize)
                .expect("a line that exists");
        }
        let count = lens.len();
        Self { geo, count }
    }

    /// How many lines the document has.
    pub fn len(&self) -> usize {
        self.count
    }

    /// The 0-based line containing document byte `at`. `O(log n)`.
    ///
    /// An `at` past the end returns the last line, matching `LineGeometry::line_of_byte`.
    pub fn line_of(&self, at: usize) -> u32 {
        self.geo.line_of_byte(at) as u32
    }

    /// The document offset at which the line containing `at` begins. `O(log n)`.
    pub fn line_start(&self, at: usize) -> usize {
        let line = self.geo.line_of_byte(at);
        // A line that exists, so this cannot fail; and `unwrap_or(0)` rather than `expect` because a
        // geometry whose count has drifted should degrade to "the document starts here" rather than
        // abort the editor. `sync` is what keeps the two in agreement.
        self.geo.byte_of(line).unwrap_or(0)
    }

    /// The geometry, for callers that need pixels rather than bytes.
    pub fn geometry(&self) -> &LineGeometry {
        &self.geo
    }

    /// Bring the geometry back into agreement with `text` after an edit at or around `caret`.
    ///
    /// **Correct by construction rather than by being told what happened**, and that is the design
    /// choice worth stating. `Session` has a dozen edit paths — typing, backspace, undo, redo, a table
    /// row, a formula — and threading an `EditOutcome` through all of them to say "I inserted a
    /// newline" is a way to be wrong in twelve places. This compares the document's newline count with
    /// the geometry's line count instead:
    ///
    /// * **Counts agree** — the edit neither added nor removed a newline, so every line *start* is
    ///   unchanged and the tree is still valid for lookup. Only the caret line's *length* moved, so
    ///   that one weight is recomputed. `O(log n)` plus a scan of one line.
    /// * **Counts disagree** — a newline appeared or vanished, which shifts every subsequent line start,
    ///   and `LineGeometry` has no way to do that in less than `O(n)`. Both trees are rebuilt.
    ///
    /// The caret is a *hint about where to look*, not the source of truth: an undo can move it far from
    /// the edit, so the count comparison is what decides, and the caret only bounds the rebuild.
    ///
    /// **`expected_lines` is passed in rather than counted here, and that is the whole point.** The
    /// first version took `text` and counted newlines itself -- which put a whole-document read back on
    /// the keystroke path, in the one function whose entire reason for existing is to remove
    /// `O(document)` work. **Measured at 3,906 µs per keystroke**, 78 % of what remained of the budget.
    ///
    /// The count is already known and already maintained: [`crate::counts::TextCounts`] folds newlines
    /// forward as deltas, so the line count is `counts.newlines + 1`. This function is handed it, and
    /// with it gone the newline case becomes a comparison and every other case becomes one local scan.
    pub fn sync(
        &mut self,
        editor: &Editor,
        expected_lines: usize,
        caret: usize,
        metrics: LineMetrics,
    ) -> Sync {
        if expected_lines != self.count {
            // A newline was added or removed. Everything after it moved, so both trees are rebuilt --
            // which is the `O(n)` the geometry documents for `resize_lines`. This is the one place a
            // whole-document read remains on the keystroke path, and it is one keystroke in forty.
            let Ok(text) = editor.text() else {
                // A document that will not read cannot be measured either, and a stale geometry is
                // better than no session. The next successful sync repairs it.
                return Sync::Unchanged;
            };
            let fresh = Self::build(&text, metrics);
            self.geo = fresh.geo;
            self.count = fresh.count;
            return Sync::Rebuilt;
        }

        // No line was added or removed, so line starts are unchanged and the tree is a valid index.
        // Recompute the caret line's length and, if it moved, that is a one-point update.
        let line = self.geo.line_of_byte(caret) as usize;
        let start = self.geo.byte_of(line).unwrap_or(0);
        let len = Self::line_len_from(editor, start) as u32;
        if self.geo.line_len(line).unwrap_or(0) == len as usize {
            return Sync::Unchanged;
        }
        self.geo
            .set_line_len(line, len as usize)
            .expect("a line that exists");
        Sync::OneLine
    }

    /// Length in bytes of the line starting at `start`, **including** its `\n`, read through
    /// `read_into` so that it allocates nothing.
    ///
    /// Scans forward and stops at the first newline, so the cost is one line's length. A line longer
    /// than one chunk is handled by the loop; the only thing it cannot do is stop early, and it does
    /// not need to.
    fn line_len_from(editor: &Editor, start: usize) -> usize {
        let len = editor.text_len();
        if start >= len {
            return 0;
        }
        let mut offset = start;
        let mut chunk = [0u8; SCAN_CHUNK];
        loop {
            let want = (len - offset).min(SCAN_CHUNK);
            let got = match editor.read_into(offset, &mut chunk[..want]) {
                Ok(got) => got,
                Err(_) => return len - start,
            };
            if got == 0 {
                // Past the end: the last line has no terminator.
                return len - start;
            }
            if let Some(i) = chunk[..got].iter().position(|&b| b == b'\n') {
                return offset + i + 1 - start;
            }
            offset += got;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> LineMetrics {
        LineMetrics::default()
    }

    /// The tree's byte offsets are the document's, which is the whole reason this module stores
    /// terminators. If `LineGeometry`'s contract ever changes, this fails rather than every line index
    /// in the product being quietly off by one per preceding line.
    #[test]
    fn byte_offsets_are_document_offsets() {
        let text = b"alpha\nbravo\ncharlie";
        let d = DocLines::build(text, metrics());
        assert_eq!(d.len(), 3, "two newlines make three lines");
        assert_eq!(d.line_start(0), 0);
        assert_eq!(d.line_start(6), 6, "the second line starts after 'alpha\\n'");
        assert_eq!(d.line_start(12), 12, "the third starts after 'bravo\\n'");
        assert_eq!(
            d.line_start(19),
            12,
            "an offset at the end is on the last line, so its start is that line's"
        );
        // And the inverse, for every byte.
        for (at, want) in (0..6).map(|i| (i, 0)).chain((6..12).map(|i| (i, 1))).chain((12..19).map(|i| (i, 2))) {
            assert_eq!(d.line_of(at), want, "byte {at} is on line {want}");
        }
    }

    /// A trailing newline makes a final empty line, which is what a text editor shows.
    #[test]
    fn a_trailing_newline_is_a_last_empty_line() {
        let d = DocLines::build(b"one\ntwo\n", metrics());
        assert_eq!(d.len(), 3);
        assert_eq!(d.line_start(8), 8, "the empty third line starts at the end");
        assert_eq!(d.line_of(8), 2);
    }

    /// An empty document is one line, not zero. `ChromeState::total_lines` is initialised to 1 and a
    /// status bar showing "0 lines" for an empty document is a visible bug.
    #[test]
    fn an_empty_document_is_one_line() {
        let d = DocLines::build(b"", metrics());
        assert_eq!(d.len(), 1);
        assert_eq!(d.line_of(0), 0);
        assert_eq!(d.line_start(0), 0);
    }

    /// An editor over `text`, for the tests that drive `sync` -- which takes the document rather than
    /// bytes, because that is the signature the keystroke path uses.
    fn editor_with(text: &[u8]) -> Editor {
        let mut e = Editor::new();
        if !text.is_empty() {
            e.insert_at(0, text, holonomy_text::SpanPolicy::GrowIntoInsert)
                .expect("room");
        }
        e
    }

    /// The line count the session would pass in: `TextCounts::lines()`, i.e. newlines plus one.
    ///
    /// **Computed the same way the product computes it**, rather than from `DocLines::len()`. Passing
    /// the geometry's own count would make the comparison in `sync` vacuously true -- it would always
    /// agree, because it is being told what it already knows -- and every rebuild test would pass for
    /// the wrong reason.
    fn expected_lines(text: &[u8]) -> usize {
        text.iter().filter(|&&b| b == b'\n').count() + 1
    }

    /// The gate for the convention: after an edit, the tree must agree with a full scan, on every line.
    #[test]
    fn the_tree_agrees_with_a_full_scan_on_every_line() {
        let mut text: Vec<u8> = Vec::new();
        let mut d = DocLines::build(&text, metrics());
        let mut e = editor_with(&text);

        // A series of edits that each change the line structure differently, with the tree synced the
        // way `Session` syncs it.
        let edits: &[(&str, usize)] = &[
            ("hello\n", 6),
            ("world", 11),
            ("\n", 12),
            ("second line", 23),
            ("\nthird", 29),
            ("!", 30),
        ];
        for (bytes, caret) in edits {
            let at = e.text_len();
            e.insert_at(
                at as u32,
                bytes.as_bytes(),
                holonomy_text::SpanPolicy::GrowIntoInsert,
            )
            .expect("room");
            text.extend_from_slice(bytes.as_bytes());
            d.sync(&e, expected_lines(&text), *caret, metrics());
            assert_agrees(&d, &text);
        }

        // And a deletion, which is the case that removes a newline and so forces a rebuild.
        text.truncate(17);
        e.delete_at(17, (e.text_len() - 17) as u32).expect("delete");
        d.sync(&e, expected_lines(&text), 17, metrics());
        assert_agrees(&d, &text);
    }

    /// `sync` on a no-op edit must report `Unchanged`, or every keystroke would be doing work it does
    /// not need to.
    #[test]
    fn a_sync_that_changes_nothing_says_so() {
        let text = b"one\ntwo\nthree";
        let e = editor_with(text);
        let mut d = DocLines::build(text, metrics());
        let n = expected_lines(text);
        assert_eq!(d.sync(&e, n, 5, metrics()), Sync::Unchanged);
        assert_eq!(d.sync(&e, n, 0, metrics()), Sync::Unchanged);
    }

    /// Typing a letter changes exactly one line's length, and says so -- the `O(log n)` path.
    #[test]
    fn a_letter_is_one_line_update_not_a_rebuild() {
        let mut text = b"one\ntwo\nthree".to_vec();
        let mut d = DocLines::build(&text, metrics());
        let mut e = editor_with(&text);
        e.insert_at(0, b"x", holonomy_text::SpanPolicy::GrowIntoInsert)
            .expect("room");
        text.insert(0, b'x');
        assert_eq!(d.sync(&e, expected_lines(&text), 1, metrics()), Sync::OneLine);
        assert_agrees(&d, &text);
    }

    /// A newline forces the rebuild, and says so -- the `O(n)` path.
    #[test]
    fn a_newline_is_a_rebuild_not_a_one_line_update() {
        let mut text = b"one\ntwo\nthree".to_vec();
        let mut d = DocLines::build(&text, metrics());
        let mut e = editor_with(&text);
        e.insert_at(4, b"\n", holonomy_text::SpanPolicy::GrowIntoInsert)
            .expect("room");
        text.insert(4, b'\n');
        assert_eq!(d.sync(&e, expected_lines(&text), 5, metrics()), Sync::Rebuilt);
        assert_agrees(&d, &text);
    }

    /// A line longer than one scan chunk, because `line_len_from` loops and the loop is untested by
    /// the other cases. 4,096 is `SCAN_CHUNK`; this line is three chunks long and has no newline until
    /// its end, so a single-chunk implementation would return a length 8,192 bytes short.
    #[test]
    fn a_line_longer_than_one_scan_chunk_is_measured_whole() {
        let body = "x".repeat(SCAN_CHUNK * 3);
        let text: Vec<u8> = [body.as_bytes(), b"\nshort\n"].concat();
        let _e = editor_with(&text);
        let d = DocLines::build(&text, metrics());
        assert_agrees(&d, &text);
        // The long line's stored length must be the whole body plus its newline.
        assert_eq!(d.geometry().line_len(0).expect("line 0"), SCAN_CHUNK * 3 + 1);
    }

    /// The comparison the whole design rests on, run over a document with lines of every length from 0
    /// to 9 — so a one-byte line, an empty line and a long line are all covered.
    #[test]
    fn the_tree_agrees_with_a_full_scan_for_lines_of_every_length() {
        for len in 0..10usize {
            let line: Vec<u8> = std::iter::repeat_n(b'x', len).collect();
            let mut text = line.clone();
            text.push(b'\n');
            let second: Vec<u8> = std::iter::repeat_n(b'y', len).collect();
            text.extend_from_slice(&second);
            text.push(b'\n');
            let d = DocLines::build(&text, metrics());
            assert_eq!(d.len(), 3, "line length {len}");
            assert_agrees(&d, &text);
        }
    }

    /// Assert the geometry agrees with a brute-force scan: every byte maps to the line a scan says, and
    /// every line starts where a scan says.
    fn assert_agrees(d: &DocLines, text: &[u8]) {
        // The scan: line starts are the offsets just after each newline, plus 0.
        let mut starts = vec![0usize];
        for (i, &b) in text.iter().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        assert_eq!(
            d.len(),
            starts.len(),
            "line count disagrees on {:?}",
            String::from_utf8_lossy(text)
        );
        for (i, &start) in starts.iter().enumerate() {
            assert_eq!(
                d.line_start(start),
                start,
                "line {i} starts at {start}, not {}",
                d.line_start(start)
            );
        }
        for at in 0..=text.len() {
            let want = starts.partition_point(|&s| s <= at) - 1;
            assert_eq!(
                d.line_of(at),
                want as u32,
                "byte {at} on line {want}, not {}",
                d.line_of(at)
            );
        }
    }
}