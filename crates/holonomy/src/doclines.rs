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

    /// Whether the document has no lines at all.
    ///
    /// **A fresh document does**, which is why this is not `len() == 0` written out at the call
    /// sites: `is_empty` is false for an empty document and `DocLines::len` is 1, so **an
    /// `is_empty` that meant "nothing to show" would be a different question and is not this one.**
    /// The honest answer for "is this page empty" is `line_begin(0) == line_end(0)`.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// The 0-based line containing document byte `at`. `O(log n)`.
    ///
    /// An `at` past the end returns the last line, matching `LineGeometry::line_of_byte`.
    pub fn line_of(&self, at: usize) -> u32 {
        self.geo.line_of_byte(at) as u32
    }

    /// One past the end of line `line`'s stored range, **including its `\n`** if it has one. `O(log n)`.
    ///
    /// **The emitter's other half.** `line_start` says where a line begins; a paint needs to know how
    /// long it is. For every line but the last, that is simply where the next one begins — which is why
    /// this is one tree lookup and not a scan. For the last line there is no next, so it falls back to
    /// its own stored length.
    ///
    /// **The terminator is included**, deliberately: the caller has the bytes anyway, and stripping a
    /// `\n` from a slice it already holds is free, whereas asking this method to exclude it would mean
    /// answering "does this line end in a newline?", which is a question about the *text* that a geometry
    /// struct has no business answering. Geometry says where a line is; the text says what is in it.
    pub fn line_end(&self, line: usize) -> usize {
        self.line_begin(line) + self.geo.line_len(line).unwrap_or(0)
    }

    /// The document offset at which **line `line`** begins. `O(log n)`.
    ///
    /// **The index-shaped twin of [`DocLines::line_start`], and the two being different is a trap worth
    /// naming.** `line_start(at)` takes a *byte offset* and answers "where does the line containing this
    /// byte begin", because that is what the caret needs. This takes a *line index* and answers "where
    /// does line N begin", which is what a renderer needs.
    ///
    /// Phase 12's first version of the body-text emitter called `line_start(line)`, and the symptom was
    /// not an error but a document that drew `one` on row 0, `one\ntwo` on row 1 and the whole document
    /// on row 2 — because `line_start(1)` means "line containing byte 1", which is line 0, and every
    /// subsequent line's offset was 0, so each line's read overlapped the previous one's. Both functions
    /// were correct; the call was not, and nothing in the type system said so because `usize` is `usize`.
    pub fn line_begin(&self, line: usize) -> usize {
        self.geo.byte_of(line).unwrap_or(0)
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
    /// * **Counts agree** — the edit neither added nor removed a newline, so exactly one line's *length*
    ///   moved and the tree is still a valid index for every other line. Both sides of the caret are
    ///   probed and each candidate that starts on a real boundary is remeasured. `O(log n)` plus a scan
    ///   of one or two lines.
    /// * **Counts disagree** — a newline appeared or vanished, which shifts every subsequent line start,
    ///   and `LineGeometry` has no way to do that in less than `O(n)`. Both trees are rebuilt.
    ///
    /// The caret is a *hint about where to look*, not the source of truth: an undo can move it far from
    /// the edit, so the count comparison is what decides, and what the probes cannot account for is
    /// caught by comparing the stored lengths against the document's and rebuilt.
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
            return self.rebuild(editor, metrics);
        }

        // **Counts agree, so exactly one line's length is wrong, and finding that line is the whole job.**
        //
        // This used to resolve it as `line_of_byte(caret)` and remeasure that one weight, which is wrong
        // in the most ordinary way to type. A keystroke leaves the caret one past the byte it inserted,
        // and an insertion at the *end* of a line — appending to it, which is what a caret sitting on the
        // `\n` does — puts that caret on the next line's first byte as the pre-edit tree counts it. So
        // the probe named the line after the one that grew, found its length unmoved, and did nothing:
        // the edited line stayed a byte short **for the rest of the session**, and because a Fenwick
        // tree's offsets are prefix sums of its weights, every line below it answered `line_start`
        // shifted by one from there on. One letter typed at the end of line 0 put a phantom `Ln 2` in
        // the status bar and drew the line below one byte out.
        //
        // So both sides of the caret are probed — `caret` and `caret - 1` — which bracket the edit in
        // every shape it takes. An insertion is caught by the last byte it added, a deletion by where
        // the removed bytes began, and undoing an insertion parks the caret on the *far* side of what it
        // removed, which is why one probe was never going to be enough.
        let mut out = Sync::Unchanged;
        for probe in [caret, caret.saturating_sub(1)] {
            let line = self.geo.line_of_byte(probe);
            let start = self.line_begin(line);
            // **A neighbour of the edited line is not measurable, so it is not measured.** Every line
            // after the edit is indexed from a stale prefix sum, and reading a line length from the
            // middle of a line returns a number that belongs to no line at all — writing that down is
            // how a one-point update becomes a corrupt tree. The byte in front of a genuine line start
            // is its terminator, so one byte decides whether this candidate's start is real, and a
            // candidate whose start is real names its own line correctly: the edit moved no newline, so
            // every boundary ahead of it kept its index.
            if start > 0 && !Self::byte_is_newline(editor, start - 1) {
                continue;
            }
            let len = Self::line_len_from(editor, start);
            if self.geo.line_len(line).unwrap_or(0) == len {
                continue;
            }
            self.geo
                .set_line_len(line, len)
                .expect("a line that exists");
            out = Sync::OneLine;
        }
        // **And then it is checked, because being wrong here is silent.** The probes assume the caret
        // brackets the edit, which holds for every path `Session` routes through here but is an
        // assumption about callers rather than a fact about the document. The stored lengths sum to the
        // document's length, so a disagreement means a line is still wrong, and one `O(n)` rebuild on
        // an undo is a good trade against a tree that quietly misplaces every line below the mistake.
        if self.bytes() != editor.text_len() {
            return self.rebuild(editor, metrics);
        }
        out
    }

    /// Rebuild both trees from the document. `O(document)`, and the answer whenever a one-point update
    /// cannot account for the difference it was asked to explain.
    fn rebuild(&mut self, editor: &Editor, metrics: LineMetrics) -> Sync {
        let Ok(text) = editor.text() else {
            // A document that will not read cannot be measured either, and a stale geometry is
            // better than no session. The next successful sync repairs it.
            return Sync::Unchanged;
        };
        let fresh = Self::build(&text, metrics);
        self.geo = fresh.geo;
        self.count = fresh.count;
        Sync::Rebuilt
    }

    /// Whether document byte `at` is a `\n`.
    fn byte_is_newline(editor: &Editor, at: usize) -> bool {
        let mut one = [0u8; 1];
        matches!(editor.read_into(at, &mut one), Ok(1) if one[0] == b'\n')
    }

    /// The document length these stored line lengths describe. `O(log n)`.
    ///
    /// **Not `LineGeometry::total_bytes`, which would be wrong by one per line here.** That method adds
    /// the line count to the sum of the weights because it was written for terminator-*exclusive*
    /// lengths; this module stores terminators (see the module docs), so on this tree its answer is
    /// `document length + lines`, and comparing it against `Editor::text_len` would report every
    /// document as too long by its own line count. The end of the last line says it directly.
    fn bytes(&self) -> usize {
        match self.count.checked_sub(1) {
            Some(last) => self.line_begin(last) + self.geo.line_len(last).unwrap_or(0),
            None => 0,
        }
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
        assert_eq!(
            d.line_start(6),
            6,
            "the second line starts after 'alpha\\n'"
        );
        assert_eq!(d.line_start(12), 12, "the third starts after 'bravo\\n'");
        assert_eq!(
            d.line_start(19),
            12,
            "an offset at the end is on the last line, so its start is that line's"
        );
        // And the inverse, for every byte.
        for (at, want) in (0..6)
            .map(|i| (i, 0))
            .chain((6..12).map(|i| (i, 1)))
            .chain((12..19).map(|i| (i, 2)))
        {
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
        assert_eq!(
            d.sync(&e, expected_lines(&text), 1, metrics()),
            Sync::OneLine
        );
        assert_agrees(&d, &text);
    }

    /// **Typing at the end of a line must update the line that grew.**
    ///
    /// `sync` used to resolve the edited line from the caret alone, and a keystroke leaves the caret one
    /// past the byte it inserted. Appending to a line — which is what a caret sitting on the `\n` does —
    /// puts that caret on the *next* line's first byte as the pre-edit tree counts it, so the probe
    /// named a line whose length had not moved, reported that nothing had changed, and left the edited
    /// line a byte short for the rest of the session. Because the tree's offsets are prefix sums of its
    /// weights, every line below it answered `line_start` shifted by one from there on.
    #[test]
    fn a_letter_typed_at_a_line_end_updates_the_line_that_grew() {
        let mut text = b"aa\nbb\ncc\n".to_vec();
        let mut d = DocLines::build(&text, metrics());
        let mut e = editor_with(&text);
        // The caret is on the '\n' closing line 0, which is where a person sits to append to that line.
        e.insert_at(2, b"x", holonomy_text::SpanPolicy::GrowIntoInsert)
            .expect("room");
        text.insert(2, b'x');
        // The caret after the insert is 3, which the *pre-edit* tree counts as line 1's first byte.
        assert_eq!(
            d.sync(&e, expected_lines(&text), 3, metrics()),
            Sync::OneLine
        );
        assert_agrees(&d, &text);
        assert_eq!(d.line_begin(1), 4, "line 1 still starts after 'aax\\n'");
        assert_eq!(d.line_begin(2), 7, "line 2 still starts after 'bb\\n'");
    }

    /// The same trap from the other two sides, because both put the caret and the edit in different
    /// lines: typing into an *empty* line leaves the caret on the following line's first byte, and
    /// undoing an insertion parks the caret on the far side of the bytes it removed.
    #[test]
    fn an_edit_that_straddles_a_line_boundary_still_finds_its_line() {
        // Into an empty line.
        let mut text = b"aa\n\ncc\n".to_vec();
        let mut d = DocLines::build(&text, metrics());
        let mut e = editor_with(&text);
        e.insert_at(3, b"x", holonomy_text::SpanPolicy::GrowIntoInsert)
            .expect("room");
        text.insert(3, b'x');
        assert_eq!(
            d.sync(&e, expected_lines(&text), 4, metrics()),
            Sync::OneLine
        );
        assert_agrees(&d, &text);

        // Removed from the end of a line, with the caret left on the far side of the hole — where
        // `Editor::undo` puts it, since it parks at `offset + bytes.len()`.
        let mut text = b"aax\nbb\ncc\n".to_vec();
        let mut d = DocLines::build(&text, metrics());
        let mut e = editor_with(&text);
        e.delete_at(2, 1).expect("delete");
        text.remove(2);
        e.caret_to(3).expect("a caret in range");
        assert_eq!(
            d.sync(&e, expected_lines(&text), 3, metrics()),
            Sync::OneLine
        );
        assert_agrees(&d, &text);
    }

    /// **A caret that is nowhere near the edit must not leave a stale tree behind.**
    ///
    /// The probes bracket the edit only if the caret does, and every `Session` edit path places it there
    /// — but that is a fact about callers, not about the document. A line's stored lengths sum to the
    /// document's length, so a caller that breaks the rule is caught by that and answered with a
    /// rebuild instead of a tree that misplaces every line below the mistake.
    #[test]
    fn an_edit_far_from_the_caret_rebuilds_rather_than_lying() {
        let mut text = b"aa\nbb\ncc\n".to_vec();
        let mut d = DocLines::build(&text, metrics());
        let mut e = editor_with(&text);
        e.delete_at(0, 1).expect("delete");
        text.remove(0);
        // The caret is parked at the very end of the document, nowhere near the byte that went.
        let far = text.len();
        e.caret_to(far).expect("a caret in range");
        d.sync(&e, expected_lines(&text), far, metrics());
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
        assert_eq!(
            d.sync(&e, expected_lines(&text), 5, metrics()),
            Sync::Rebuilt
        );
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
        assert_eq!(
            d.geometry().line_len(0).expect("line 0"),
            SCAN_CHUNK * 3 + 1
        );
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
