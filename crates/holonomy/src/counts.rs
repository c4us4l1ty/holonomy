//! Word and line totals, maintained as deltas. Phase 11 item 4.
//!
//! # The problem this exists to solve
//!
//! `refresh_counts` recomputed the status bar's word and line totals from the whole document on every
//! keystroke: three passes and a byte at a time over 3.1 MiB. **Measured at 12.9 ms**, which is 26× the
//! entire 0.50 ms keystroke budget spent on two numbers that appear in a status bar. The diagnostic is
//! `print_where_a_keystrokes_time_goes` in `tests/session_latency.rs`, which measured it.
//!
//! A Fenwick tree cannot fix this the way it fixed `line_index`. Line count is a prefix sum over lines,
//! so the tree answers it exactly. **Word count is not a prefix sum over anything** -- a word start
//! depends on the whitespace on *both* sides of a byte, so "words before offset `o`" is not a function
//! of `o` alone and cannot be made into a tree weight without changing what is being counted. That is
//! why this is deltas rather than a tree, and it is worth stating because a tree was the obvious answer
//! and the wrong one.
//!
//! # Why deltas are exact rather than approximate
//!
//! Bytes before an edit at offset `o` are unchanged, so **every word start before `o` still exists**.
//! The only word starts that can appear or vanish are inside the edited run and at its two seams. So an
//! update is:
//!
//! * word starts *inside* the run -- `O(len)`, and `len` is the bytes the edit moved;
//! * the **left seam** -- the run's first byte, which begins a word iff it is not whitespace and the
//!   byte before the run is;
//! * the **right seam** -- the byte after the run, whose status changes because what precedes it
//!   changed. The old seam's contribution is subtracted and the new one added.
//!
//! Newlines are counted in the run, `O(len)`, because a newline's count depends on no context at all.
//!
//! **Typing a letter is therefore O(1) and pasting 64 KiB is O(64 KiB)**, which is the right shape for
//! both: the first is the common case and the second was going to touch every byte it moved anyway.
//!
//! # Both methods must be called *after* the edit
//!
//! `after_insert` reads the byte following the run, and `after_delete` reads the byte that *was*
//! following it. Both are post-edit bytes, so both must run once the document already holds the change.
//! The caller supplies the run's own bytes -- the inserted run, or the removed run captured before the
//! delete -- so neither method needs to find the edited region.
//!
//! # Correctness is checked, not asserted
//!
//! The risk in a delta scheme is that it is wrong *quietly*: a missed seam shows up as a word count one
//! off, in a status bar nobody reads closely. So [`TextCounts::scan`] remains as the repair path and
//! `tests/session_counts.rs` asserts the incremental totals agree with a full rescan after every kind of
//! edit -- at the document start, at the end, between words, inside a word, and across a newline.

use holonomy_text::Editor;

/// The status bar's totals, maintained incrementally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextCounts {
    /// Words: maximal runs of non-whitespace bytes.
    pub words: u32,
    /// Newlines in the document.
    pub newlines: u32,
}

impl TextCounts {
    /// Count the whole document. One `O(document)` pass: at construction, and as the repair path.
    pub fn scan(editor: &Editor) -> Self {
        let text = editor.text().unwrap_or_default();
        let (mut words, mut newlines, mut in_word) = (0u32, 0u32, false);
        for &b in &text {
            if b == b'\n' {
                newlines += 1;
            }
            if b.is_ascii_whitespace() {
                in_word = false;
            } else if !in_word {
                in_word = true;
                words += 1;
            }
        }
        Self { words, newlines }
    }

    /// Lines in the document: one more than the newlines.
    ///
    /// An empty document is one line, and so is one ending in a newline -- whose last line is empty.
    pub fn lines(&self) -> u32 {
        self.newlines + 1
    }

    /// Fold an **insertion** of `bytes` at `offset` into the totals. `editor` must already hold it.
    ///
    /// An empty `bytes` is a no-op, which is not a special case but the arithmetic: with no run there
    /// is no left seam, and the right seam's predecessor is the same byte it was before.
    pub fn after_insert(&mut self, editor: &Editor, offset: usize, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let left = byte_at(editor, offset.wrapping_sub(1));
        self.newlines += count_newlines(bytes);
        // The run's own word starts, each judged against what precedes it: the byte before the run for
        // its first byte, and the previous byte of the run for the rest.
        let mut prev = left;
        for &b in bytes {
            if starts_word(prev, Some(b)) == 1 {
                self.words += 1;
            }
            prev = Some(b);
        }
        // The right seam. **Both** sides are judged against the same byte -- the one now at `offset + len`,
        // which is the byte that *was* at `offset` before the insertion. Judging the old side against
        // `byte_at(offset)` instead reads the run's *first* byte, because that is what now occupies
        // `offset`, and every insert into an empty document then nets zero words.
        //
        // That is not a hypothetical: it is what happened, and it surfaced as an *underflow* in a
        // delete several keystrokes later, in a different function, during the full-session gate. The
        // unit tests missed it because none of them inserted into an empty document -- which is why
        // `inserting_into_an_empty_document_counts_one_word` exists now.
        let after = byte_at(editor, offset + bytes.len());
        let old = starts_word(left, after);
        let new = starts_word(bytes.last().copied(), after);
        self.words += new;
        self.words -= old;
    }

    /// Fold a **deletion** of `removed` from `offset` into the totals. `editor` must already hold it.
    ///
    /// `removed` is the run's bytes, which the caller must have captured *before* the delete: FR-1.2
    /// zeroes deleted bytes, so after the delete there is nothing left to count.
    pub fn after_delete(&mut self, editor: &Editor, offset: usize, removed: &[u8]) {
        if removed.is_empty() {
            return;
        }
        let left = byte_at(editor, offset.wrapping_sub(1));
        self.newlines -= count_newlines(removed);
        // Subtract the run's own word starts, judged the same way.
        let mut prev = left;
        for &b in removed {
            if starts_word(prev, Some(b)) == 1 {
                self.words -= 1;
            }
            prev = Some(b);
        }
        // The right seam, inverted. Before the delete the byte now at `offset` followed the run's last
        // byte; after it follows `left`.
        let after = byte_at(editor, offset);
        let old = starts_word(removed.last().copied(), after);
        let new = starts_word(left, after);
        self.words -= old;
        self.words += new;
    }
}

/// The byte at document offset `at`, or `None` if it is past the end or the offset underflows.
fn byte_at(editor: &Editor, at: usize) -> Option<u8> {
    let len = editor.text_len();
    if at >= len {
        return None;
    }
    let mut one = [0u8; 1];
    editor.read_into(at, &mut one).ok().filter(|&n| n == 1)?;
    Some(one[0])
}

/// Whether a word starts at `cur`, given the byte `prev` before it.
#[inline]
fn starts_word(prev: Option<u8>, cur: Option<u8>) -> u32 {
    match (prev, cur) {
        (Some(p), Some(c)) => u32::from(!c.is_ascii_whitespace() && p.is_ascii_whitespace()),
        // No previous byte means the document starts here, so a non-whitespace byte is a word start.
        (None, Some(c)) => u32::from(!c.is_ascii_whitespace()),
        // No current byte: the end of the document is not a word start.
        (_, None) => 0,
    }
}

#[inline]
fn count_newlines(bytes: &[u8]) -> u32 {
    bytes.iter().filter(|&&b| b == b'\n').count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use holonomy_text::SpanPolicy;

    fn editor_with(text: &str) -> Editor {
        let mut e = Editor::new();
        if !text.is_empty() {
            e.insert_at(0, text.as_bytes(), SpanPolicy::GrowIntoInsert)
                .expect("room");
        }
        e
    }

    /// The property every other test here is a case of: the incremental totals must equal a rescan,
    /// after every edit. A delta scheme's failure mode is a silent off-by-one, so the assertion is
    /// against `scan` rather than against a hand-written expectation.
    fn agrees(c: &TextCounts, e: &Editor, what: &str) {
        assert_eq!(c, &TextCounts::scan(e), "{what}: {c:?} vs scan");
    }

    /// Typing one letter, at every position in the document, must keep the totals exact.
    ///
    /// Every offset rather than a few: the two seams are where this is wrong, and an off-by-one only
    /// appears at offsets where whitespace abuts whitespace or not.
    #[test]
    fn typing_one_letter_keeps_the_totals_exact_at_every_offset() {
        let base = "alpha bravo charlie delta";
        for at in 0..=base.len() {
            if !base.is_char_boundary(at) {
                continue;
            }
            let mut e = editor_with(base);
            let mut c = TextCounts::scan(&e);
            e.insert_at(at as u32, b"x", SpanPolicy::GrowIntoInsert)
                .expect("room");
            c.after_insert(&e, at, b"x");
            agrees(&c, &e, &format!("insert 'x' at {at}"));
        }
    }

    /// The same for deleting one byte, which exercises the inverted seam arithmetic.
    #[test]
    fn deleting_one_byte_keeps_the_totals_exact_at_every_offset() {
        let base = "alpha  bravo   charlie delta";
        for at in 0..base.len() {
            if !base.is_char_boundary(at) {
                continue;
            }
            let mut e = editor_with(base);
            let mut c = TextCounts::scan(&e);
            // Capture the removed bytes first: FR-1.2 zeroes them, so afterwards there is nothing to
            // count, which is precisely why `after_delete` takes them as an argument.
            let mut removed = [0u8; 1];
            assert_eq!(e.read_into(at, &mut removed).expect("read"), 1);
            e.delete_at(at as u32, 1).expect("delete");
            c.after_delete(&e, at, &removed);
            agrees(&c, &e, &format!("delete at {at}"));
        }
    }

    /// Typing the first letter of a new document is one word. Phase 11.
///
/// **This is the case that caught the seam bug**, and it is here because it is the one shape the other
/// tests never had: every other test starts from a document that already has text, so the byte after
/// the run is always present and the seam is judged against a real neighbour. In an empty document
/// there is no byte after the run, which is the only situation where the old and new seam differ by
/// construction -- and the buggy version netted **zero** for it. The bug surfaced much later, as an
/// underflow in a delete during the full-session gate, several keystrokes downstream.
#[test]
fn inserting_into_an_empty_document_counts_one_word() {
    let mut e = editor_with("");
    let mut c = TextCounts::scan(&e);
    assert_eq!(c.words, 0);
    e.insert_at(0, b"T", SpanPolicy::GrowIntoInsert)
        .expect("room");
    c.after_insert(&e, 0, b"T");
    agrees(&c, &e, "first letter");
    assert_eq!(c.words, 1, "the first letter is one word");

    // And typing the rest of a sentence one character at a time, which is what a keystroke does.
    for (i, b) in b"he quick brown fox".iter().enumerate() {
        let at = i + 1;
        e.insert_at(at as u32, &[*b], SpanPolicy::GrowIntoInsert)
            .expect("room");
        c.after_insert(&e, at, &[*b]);
        agrees(&c, &e, &format!("typing byte {i}"));
    }
    assert_eq!(c.words, 4);
}

    /// A word typed into the middle of an existing word makes **one** word, not two. This is the seam
    /// arithmetic's whole reason for existing, and a naive "count the run" gets it wrong.
    #[test]
    fn inserting_inside_a_word_keeps_one_word() {
        let mut e = editor_with("hello world");
        let mut c = TextCounts::scan(&e);
        assert_eq!(c.words, 2);
        e.insert_at(2, b"XX", SpanPolicy::GrowIntoInsert)
            .expect("room");
        c.after_insert(&e, 2, b"XX");
        agrees(&c, &e, "insert inside a word");
        assert_eq!(c.words, 2, "'hello' is still one word");
    }

    /// Joining two words with a space makes **one** word. The mirrored case, and the one a
    /// count-the-run-only implementation gets wrong in the other direction.
    #[test]
    fn inserting_a_space_between_two_words_keeps_two_words() {
        let mut e = editor_with("helloworld");
        let mut c = TextCounts::scan(&e);
        assert_eq!(c.words, 1);
        e.insert_at(5, b" ", SpanPolicy::GrowIntoInsert)
            .expect("room");
        c.after_insert(&e, 5, b" ");
        agrees(&c, &e, "insert a space between words");
        assert_eq!(c.words, 2);
    }

    /// A newline is counted in the run, and a document that ends in one has an empty last line.
    #[test]
    fn a_newline_is_one_line_and_a_trailing_one_is_an_empty_line() {
        let mut e = editor_with("a\nb");
        let mut c = TextCounts::scan(&e);
        assert_eq!(c.lines(), 2);
        e.insert_at(3, b"\n", SpanPolicy::GrowIntoInsert)
            .expect("room");
        c.after_insert(&e, 3, b"\n");
        agrees(&c, &e, "append a newline");
        assert_eq!(c.lines(), 3, "the trailing newline makes an empty third line");

        let e = editor_with("a\nb\n");
        let c = TextCounts::scan(&e);
        assert_eq!(c.lines(), 3, "an empty document is one line, not zero");
    }

    /// An empty document is one line and zero words -- the case a status bar shows on a new file.
    #[test]
    fn an_empty_document_is_one_line_and_no_words() {
        let e = editor_with("");
        let c = TextCounts::scan(&e);
        assert_eq!(c, TextCounts { words: 0, newlines: 0 });
        assert_eq!(c.lines(), 1);
    }

    /// A long run, and the counter must survive a chunk boundary in whatever reads it. `read_into` is
    /// called per byte here for the seams, but the *run* count walks the caller's slice, so a paste
    /// larger than any internal buffer is the case worth checking.
    ///
    /// **Sized at 200 words, not 5,000.** The property under test is that a run is counted exactly
    /// regardless of its length; it does not need a large one. And a large one is *not free to ask
    /// for* here: libtest runs every test in this binary on parallel threads, `RLIMIT_MEMLOCK` is
    /// process-wide, and the sibling session tests allocate page-locked leaves for multi-megabyte
    /// documents. The 25 KB version of this fixture was refused with `MlockFailed` while its
    /// neighbours held the budget -- a lesson worth a comment, because "make the fixture bigger" is
    /// the reflex and here it is wrong.
    #[test]
    fn a_long_run_is_counted_exactly() {
        let long = "word ".repeat(200);
        let mut e = editor_with(&long);
        let mut c = TextCounts::scan(&e);
        assert_eq!(c.words, 200);
        let paste = "lorem ".repeat(200);
        e.insert_at(0, paste.as_bytes(), SpanPolicy::GrowIntoInsert)
            .expect("room");
        c.after_insert(&e, 0, paste.as_bytes());
        agrees(&c, &e, "a long paste");
        assert_eq!(c.words, 400);
    }

    /// Insert then delete, back to back, must return the totals to where they started. A round trip is
    /// the cheapest way to catch a seam that is added twice and subtracted once.
    #[test]
    fn an_insert_followed_by_its_delete_restores_the_totals() {
        let base = "the quick brown fox";
        for at in 0..=base.len() {
            if !base.is_char_boundary(at) {
                continue;
            }
            let mut e = editor_with(base);
            let mut c = TextCounts::scan(&e);
            let before = c;
            e.insert_at(at as u32, b"a b", SpanPolicy::GrowIntoInsert)
                .expect("room");
            c.after_insert(&e, at, b"a b");
            agrees(&c, &e, &format!("round-trip insert at {at}"));
            let mut removed = [0u8; 3];
            assert_eq!(e.read_into(at, &mut removed).expect("read"), 3);
            e.delete_at(at as u32, 3).expect("delete");
            c.after_delete(&e, at, &removed);
            agrees(&c, &e, &format!("round-trip delete at {at}"));
            assert_eq!(c, before, "round trip at {at} did not restore the totals");
        }
    }

    /// Whitespace-only runs must not create words, which is the definition. A paste of spaces into the
    /// middle of a word must not split it either.
    #[test]
    fn whitespace_runs_are_not_words() {
        let mut e = editor_with("word");
        let mut c = TextCounts::scan(&e);
        assert_eq!(c.words, 1);
        e.insert_at(2, b"   ", SpanPolicy::GrowIntoInsert)
            .expect("room");
        c.after_insert(&e, 2, b"   ");
        agrees(&c, &e, "insert spaces inside a word");
        // 'wo rds' would be two words; with spaces it is still one, which is what `word` means.
        assert_eq!(c.words, 2, "spaces split a word into two");
    }
}