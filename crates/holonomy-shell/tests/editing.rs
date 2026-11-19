//! The loop: intents in, frames out — and the check OQ-15 said needed a shell to exist.
//!
//! Every test drives the editor the way a window would: type, click, scroll, resize, and look at
//! the frame that comes back. The shaper is a deterministic fake with uneven advances, so the
//! numbers are exact rather than approximate — which is what makes the caret check at the end a
//! proof about every caret a frame offers instead of a sample of them.

use holonomy_eye::{layout_styled, Clusters, Fixed, Point, Shaper, Style, StyledText, View};
use holonomy_shell::Editor;
use holonomy_text::{Appended, ResidentSet, SegmentId, SegmentSink, SegmentSource, Typing};
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

/// One logical unit per cluster, two for `m` and `中`.
#[derive(Debug, Default)]
struct Fake;

impl Shaper for Fake {
    fn advance(&self, cluster: &str, _style: Style) -> Fixed {
        match cluster.chars().next() {
            Some('m') | Some('中') => Fixed::units(2),
            _ => Fixed::ONE,
        }
    }

    fn line_height(&self, _style: Style) -> Fixed {
        Fixed::units(20)
    }

    fn ascent(&self, _style: Style) -> Fixed {
        Fixed::units(15)
    }
}

/// The world-line as a session uses it: one store, appended to through [`SegmentSink`] and read
/// back through [`SegmentSource`], counting the reads.
///
/// Both halves share it, which is what makes a **sealed** leaf readable: typing into an open buffer
/// never asks the world-line for anything, and the moment a seal commits a segment the reader must
/// be able to fault it in. A sink and a source that did not share state would make every test that
/// flushes a fake that disagrees with itself.
#[derive(Debug, Clone, Default)]
struct World {
    segments: Rc<RefCell<BTreeMap<u64, Vec<u8>>>>,
    next: Rc<Cell<u64>>,
    loads: Rc<Cell<usize>>,
}

impl World {
    fn loads(&self) -> usize {
        self.loads.get()
    }
}

impl SegmentSink for World {
    fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String> {
        let id = self.next.get();
        self.next.set(id + 1);
        self.segments.borrow_mut().insert(id, plaintext.to_vec());
        Ok(Appended {
            segment: SegmentId::new(id),
            witness_sequence: id + 1,
            segment_root: [0u8; 32],
            bytes_written: plaintext.len() as u64,
        })
    }
}

impl SegmentSource for World {
    fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
        self.loads.set(self.loads.get() + 1);
        let segments = self.segments.borrow();
        segments
            .get(&segment.get())
            .cloned()
            .ok_or_else(|| format!("no {segment}"))
    }
}

/// A sink that mints distinct segment ids and stores nothing — the commit half of the generated
/// world-line below, whose leaves do not depend on what was typed.
#[derive(Debug, Default)]
struct Recorder {
    next: u64,
}

impl SegmentSink for Recorder {
    fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String> {
        let id = self.next;
        self.next += 1;
        Ok(Appended {
            segment: SegmentId::new(id),
            witness_sequence: id + 1,
            segment_root: [0u8; 32],
            bytes_written: plaintext.len() as u64,
        })
    }
}

/// A world-line that generates its leaves, so a four-thousand-leaf document costs the residency
/// nothing: each leaf is `leaf_bytes` letters and no line break, which is one measured line per
/// leaf — the number the assertions below are written in.
#[derive(Debug)]
struct Repeated {
    leaf_bytes: usize,
    loads: usize,
}

impl Repeated {
    fn new(leaf_bytes: usize) -> Self {
        Self {
            leaf_bytes,
            loads: 0,
        }
    }
}

impl SegmentSource for Repeated {
    fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
        if segment.get() >= 4_096 {
            return Err(format!("no {segment}"));
        }
        self.loads += 1;
        Ok(vec![b'a'; self.leaf_bytes])
    }
}

const WIDTH: Fixed = Fixed::units(200);
const VIEWPORT: Fixed = Fixed::units(40);
/// Four leaf-sized buffers fit in the resident set; measuring a document is not holding it.
const BUDGET: usize = 256;

/// A session whose world-line can be sealed and read back.
fn session() -> Editor<World, World, Fake> {
    let world = World::default();
    let residency = ResidentSet::new(world.clone(), BUDGET, 0).expect("a workable budget");
    Editor::new(Typing::new(world, 64), residency, Fake, WIDTH, VIEWPORT)
}

/// A session over a generated world-line, for documents too large to type by hand.
fn editor(source: Repeated) -> Editor<Recorder, Repeated, Fake> {
    let residency = ResidentSet::new(source, BUDGET, 0).expect("a workable budget");
    Editor::new(
        Typing::new(Recorder::default(), 64),
        residency,
        Fake,
        WIDTH,
        VIEWPORT,
    )
}

/// One leaf of a [`Repeated`] document: sixty-four letters, one line, twenty units.
fn repeat_leaf() -> String {
    "a".repeat(64)
}

#[test]
fn typing_is_visible_in_the_next_frame() {
    let mut shell = session();
    shell.type_text("hello world").expect("typed");
    let frame = shell.frame().expect("a frame");
    assert_eq!(frame.text(), "hello world");
    assert_eq!(frame.origin(), 0);
    assert_eq!(frame.layout().lines().len(), 1);
    assert_eq!(frame.layout().lines()[0].clusters().len(), 11);
    assert_eq!(frame.shaped(), 11, "one cluster shaped per character");
    let caret = frame.caret().expect("the caret is on screen");
    assert_eq!(caret.byte, 11);
    assert_eq!(caret.x, Fixed::units(11));
}

#[test]
fn a_fresh_document_measures_itself_and_stops_being_provisional() {
    let mut shell = session();
    shell.type_text("one\ntwo\nthree\n").expect("typed");
    let (provisional, height) = {
        let first = shell.frame().expect("a frame");
        (first.provisional(), first.layout().height())
    };
    assert!(provisional, "nothing had been measured before it");
    assert_eq!(
        shell.heights().total(),
        height,
        "the frame wrote back what it laid out"
    );
    assert_eq!(shell.heights().len(), 1);

    let second = shell.frame().expect("a frame");
    assert!(!second.provisional(), "the first frame measured the screen");
    assert_eq!(second.text(), "one\ntwo\nthree\n");
    assert_eq!(
        second.layout().lines().len(),
        4,
        "the empty last line counts"
    );
}

#[test]
fn reflow_measures_every_leaf_one_at_a_time() {
    let mut shell = editor(Repeated::new(64));
    shell.type_text(&repeat_leaf().repeat(64)).expect("typed");
    assert_eq!(shell.document_len(), 64 * 64);
    // Seal, so every leaf has a segment of its own: a document left with an open buffer would be
    // measured through the reader's pending path instead of the world-line.
    shell.flush().expect("sealed");
    shell.reflow().expect("measured");

    assert_eq!(shell.heights().len(), 64, "one leaf per sealed segment");
    assert_eq!(
        shell.heights().total(),
        Fixed::units(21 * 20),
        "four thousand and ninety-six letters, twenty-one lines, measured leaf by leaf"
    );
    assert_eq!(
        shell.residency().source().loads,
        64,
        "every leaf was faulted exactly once to be measured"
    );
    assert!(
        shell.residency().resident_segments() <= BUDGET / 64,
        "measuring the document did not mean holding it"
    );

    shell.scroll_to(Fixed::units(21 * 20));
    assert_eq!(
        shell.scroll(),
        Fixed::units(21 * 20 - 40),
        "the scroll clamps to the last screen"
    );
    let frame = shell.frame().expect("a frame");
    assert!(!frame.provisional());
    assert_eq!(frame.origin(), 3_800, "the last screen starts at a line");
    assert!(frame.text().ends_with('a'), "{:?}", frame.text());
}

#[test]
fn reflow_agrees_with_one_continuous_layout_of_the_document() {
    // The claim the whole index rests on: a leaf's height is the height of the lines that **start**
    // in it, so the leaves sum to the height of the document laid out in one piece — even when a
    // paragraph runs straight through every leaf boundary. A shell may never lay the document out
    // in one piece; a test may, and did, and before the carry existed this failed by a line per
    // boundary.
    let paragraph = repeat_leaf().repeat(64);
    let mut split = editor(Repeated::new(64));
    split.type_text(&paragraph).expect("typed");
    split.flush().expect("sealed");
    split.reflow().expect("measured");
    let whole = layout_styled(StyledText::plain(&paragraph), 0, WIDTH, &Fake);
    assert_eq!(
        split.heights().total(),
        whole.height(),
        "one paragraph, 64 leaves"
    );

    // A document that ends every line, where the empty line after each break belongs to the leaf
    // that continues it — and the last one belongs to the document.
    let lines = "abc\n".repeat(1_024);
    let mut broken = session();
    broken.type_text(&lines).expect("typed");
    broken.flush().expect("sealed");
    broken.reflow().expect("measured");
    let whole = layout_styled(StyledText::plain(&lines), 0, WIDTH, &Fake);
    assert_eq!(broken.heights().total(), whole.height(), "1024 hard breaks");
}

#[test]
fn a_frame_begins_at_the_line_a_scroll_position_falls_in() {
    // The bug OQ-16 laid bare: a window that started at a leaf would begin in the middle of a line
    // and re-wrap the paragraph from there, so every cluster below it would be drawn where the
    // document does not put it. Line 19 of this document starts at byte 3,800 — inside leaf 59 —
    // and the frame has to begin *there*.
    let mut shell = editor(Repeated::new(64));
    shell.type_text(&repeat_leaf().repeat(64)).expect("typed");
    shell.flush().expect("sealed");
    shell.reflow().expect("measured");
    let measured = shell.heights().total();
    assert_eq!(
        measured,
        Fixed::units(420),
        "twenty-one lines of twenty units"
    );

    shell.scroll_to(Fixed::units(380));
    let (origin, clusters, leaves) = {
        let frame = shell.frame().expect("a frame");
        (
            frame.origin(),
            frame.layout().lines()[0].clusters().len(),
            frame.leaves().len(),
        )
    };
    assert_eq!(origin, 3_800, "the line, not the leaf it is drawn in");
    assert_eq!(
        clusters, 200,
        "the first line is a full line of the document"
    );
    assert_eq!(leaves, 4, "the bytes behind a two-line screen");
    assert_eq!(
        shell.heights().total(),
        measured,
        "a frame a leaf began inside leaves that leaf's measurement alone"
    );
}

#[test]
fn revealing_the_caret_scrolls_the_smallest_amount_that_shows_it() {
    let mut shell = editor(Repeated::new(64));
    shell
        .type_text(&repeat_leaf().repeat(4_000))
        .expect("typed");
    shell.flush().expect("sealed");
    shell.reflow().expect("measured");
    let bottom = Fixed::units(1_280 * 20 - 40);

    // The caret is at the end of a document nobody has scrolled, so showing it is one scroll — to
    // the last screen — and then nothing at all.
    assert!(
        shell.reveal_caret().expect("a reveal"),
        "the caret is off screen"
    );
    assert_eq!(shell.scroll(), bottom, "the last screen");
    assert!(
        !shell.reveal_caret().expect("a reveal"),
        "it is visible now"
    );

    // A caret above the screen. The frame at the bottom of the document does not hold it, and a
    // caret a frame does not hold has no frame-relative y to read: the answer has to come from the
    // document, or the reveal does nothing.
    shell.scroll_to(Fixed::ZERO);
    let clicked = shell
        .click(Point::new(Fixed::ZERO, Fixed::ZERO))
        .expect("clicked");
    assert_eq!(
        clicked.map(|caret| caret.byte),
        Some(0),
        "the document's start"
    );
    shell.scroll_to(bottom);
    assert!(
        shell.reveal_caret().expect("a reveal"),
        "the caret is above the screen"
    );
    assert_eq!(
        shell.scroll(),
        Fixed::ZERO,
        "the smallest scroll is up to its line"
    );

    // A click on the first line of a screen whose scroll position is not a line boundary. The caret
    // lands at the start of the line at 12,800 — above the scroll position, so revealing it is one
    // line up and **not** up to the top of the document, which is what a frame-relative y would do.
    shell.scroll_to(Fixed::units(12_805));
    let clicked = shell
        .click(Point::new(Fixed::ZERO, Fixed::ZERO))
        .expect("clicked");
    assert_eq!(
        clicked.map(|caret| caret.byte),
        Some(128_000),
        "the line's start"
    );
    assert_eq!(
        shell.scroll(),
        Fixed::units(12_805),
        "a click does not scroll"
    );
    assert!(shell.reveal_caret().expect("a reveal"));
    assert_eq!(shell.scroll(), Fixed::units(12_800), "one line up");
    assert!(!shell.reveal_caret().expect("a reveal"), "and then nothing");
}

#[test]
fn a_keystroke_on_a_long_document_shapes_only_the_screen() {
    let mut shell = editor(Repeated::new(64));
    shell
        .type_text(&repeat_leaf().repeat(4_000))
        .expect("typed");
    shell.flush().expect("sealed");
    shell.reflow().expect("measured");
    assert_eq!(shell.heights().len(), 4_000);
    let measured = shell.heights().total();
    assert_eq!(
        measured,
        Fixed::units(1_280 * 20),
        "256,000 letters, 1,280 lines"
    );

    // Halfway down a document this has never shown: the frame shapes two lines, not four thousand.
    shell.scroll_to(Fixed::units(1_280 * 20 / 2));
    let (origin, shaped, bytes, reads) = {
        let frame = shell.frame().expect("a frame");
        (
            frame.origin(),
            frame.shaped(),
            frame.text().len(),
            frame.reads(),
        )
    };
    assert_eq!(
        origin, 128_000,
        "halfway is a line, which here is a leaf boundary"
    );
    assert!(
        shaped <= 420,
        "{shaped} clusters shaped for a two-line screen"
    );
    assert!(bytes <= 420, "{bytes} bytes read for a two-line screen");
    assert!(
        reads <= 256,
        "{reads} index reads for 4000 leaves: searches"
    );

    // A keystroke at the document's end appends one leaf, and the index grows by one instead of
    // being rebuilt: every height measured a moment ago is still the truth. A rebuild would leave
    // four thousand zeroes and a total of zero, so the total is what tells the two apart.
    shell.type_text("z").expect("typed");
    let shaped = {
        let after = shell.frame().expect("a frame");
        after.shaped()
    };
    assert_eq!(shell.heights().len(), 4_001, "one appended leaf");
    assert_eq!(
        shell.heights().total(),
        measured,
        "a tail append invalidated no measured height"
    );
    assert!(shaped <= 420, "{shaped} clusters shaped");
}

#[test]
fn backspace_takes_the_whole_cluster() {
    let mut shell = session();
    // `e` + COMBINING ACUTE is one cluster of three bytes, and the caret sits after all three.
    shell.type_text("e\u{301}x").expect("typed");
    assert!(shell.backspace().expect("deleted"), "x goes");
    assert_eq!(shell.document_len(), 3, "`e` and its mark are three bytes");
    assert_eq!(shell.frame().expect("a frame").text(), "e\u{301}");
    assert!(shell.backspace().expect("deleted"));
    assert_eq!(shell.document_len(), 0);
    assert!(
        !shell.backspace().expect("nothing left"),
        "the document's start has no cluster before it"
    );
}

#[test]
fn a_click_lands_on_a_cluster_boundary_and_a_drag_selects() {
    let mut shell = session();
    // `m` is two units wide and `i` one: `mimi` puts boundaries at bytes 0-4 and x 0, 2, 3, 5, 6.
    shell.type_text("mimi").expect("typed");
    let clicked = shell
        .click(Point::new(Fixed::units(5), Fixed::ZERO))
        .expect("clicked")
        .expect("a caret");
    assert_eq!(clicked.byte, 3, "the boundary at x = 5 is byte 3");
    assert_eq!(shell.caret(), 3);
    assert!(shell.selection().is_empty(), "a click drops the selection");

    let dragged = shell
        .drag_to(Point::new(Fixed::units(1), Fixed::ZERO))
        .expect("dragged")
        .expect("a caret");
    assert_eq!(dragged.byte, 0);
    assert_eq!(shell.selection().normalised(), (0, 3));
    assert_eq!(shell.selection().caret_offset(), 0, "the caret is the drag");
    assert!(shell.delete_selection().expect("deleted"));
    assert_eq!(
        shell.document_len(),
        1,
        "one byte was left outside the selection"
    );
    assert_eq!(shell.frame().expect("a frame").text(), "i");
}

#[test]
fn a_taller_viewport_pulls_the_scroll_back_inside_the_document() {
    let mut shell = editor(Repeated::new(64));
    shell.type_text(&repeat_leaf().repeat(64)).expect("typed");
    shell.flush().expect("sealed");
    shell.reflow().expect("measured");
    assert_eq!(
        shell.heights().total(),
        Fixed::units(420),
        "twenty-one lines"
    );

    // The last screen of a 40-unit viewport, and then the window grows: the maximum scroll is a
    // function of the viewport, so 380 is past the end of a 200-unit one.
    shell.scroll_to(Fixed::units(380));
    assert_eq!(shell.scroll(), Fixed::units(380));
    shell.resize(WIDTH, Fixed::units(200));
    assert_eq!(
        shell.scroll(),
        Fixed::units(220),
        "420 units, less the 200 on screen"
    );

    // And the frame it produces is still the document's: 220 is the start of line eleven.
    let origin = shell.frame().expect("framed").origin();
    assert_eq!(origin, 11 * 200);
}

#[test]
fn a_crlf_split_across_a_leaf_is_one_break() {
    let document = "abc\r\n".repeat(20);
    let mut shell = session();
    shell.type_text(&document).expect("typed");
    shell.flush().expect("sealed");
    shell.reflow().expect("measured");

    // A hundred bytes into sixty-four-byte leaves: the cut lands between the CR of the twenty-first
    // unit's break and its LF. Laid out apart, the LF starts a line the document does not have.
    assert_eq!(shell.heights().len(), 2, "two leaves");
    let whole = layout_styled(StyledText::plain(&document), 0, WIDTH, &Fake);
    assert_eq!(
        shell.heights().total(),
        whole.height(),
        "twenty-one lines, not twenty-two"
    );
    assert_eq!(shell.heights().total(), Fixed::units(21 * 20));
}

#[test]
fn a_backspace_crosses_a_leaf_boundary_to_take_its_cluster() {
    let mut document = "a".repeat(63);
    document.push_str("e\u{301}");
    let mut shell = session();
    shell.type_text(&document).expect("typed");
    shell.flush().expect("sealed");
    shell.reflow().expect("measured");
    assert_eq!(
        shell.document_len(),
        66,
        "sixty-three letters, then e and its mark"
    );

    // The caret is after the mark, and the leaf cut fell between the base and the mark — so the
    // cluster the backspace takes begins in the leaf before the caret's.
    assert!(
        shell.backspace().expect("a backspace"),
        "there is a cluster to take"
    );
    assert_eq!(shell.document_len(), 63, "the base goes with its mark");
    assert_eq!(shell.caret(), 63);
}

#[test]
fn a_delete_crosses_a_leaf_boundary_to_take_its_cluster() {
    let mut document = "a".repeat(63);
    document.push_str("e\u{301}");
    let mut shell = session();
    shell.type_text(&document).expect("typed");
    shell.flush().expect("sealed");
    shell.reflow().expect("measured");

    // The caret goes to the base, which is the last byte of the first leaf; its mark is the first
    // byte of the second. One cluster, so `delete` takes both.
    let clicked = shell
        .click(Point::new(Fixed::units(63), Fixed::ZERO))
        .expect("clicked");
    assert_eq!(
        clicked.map(|caret| caret.byte),
        Some(63),
        "the base's boundary"
    );
    assert!(shell.delete_forward().expect("a delete"));
    assert_eq!(shell.document_len(), 63, "the mark goes with its base");
    assert_eq!(shell.caret(), 63);
}

/// Every 64-byte boundary of this text falls inside an `e` + U+0301 cluster.
///
/// A block is a mark (finishing the previous block's `e`), sixty-one `a`, and an `e` in the
/// block's last byte — so whatever leaf a window ends on, its end is inside a grapheme.
fn a_cluster_straddling_every_leaf() -> String {
    let mut text = String::new();
    for _ in 0..7 {
        text.push('\u{301}');
        text.push_str(&"a".repeat(61));
        text.push('e');
    }
    text
}

#[test]
fn a_frame_hands_each_leaf_the_state_from_before_it() {
    // A frame writes back the carries it measured, and every later question about a leaf is
    // answered with the carry *into* it. Recording the state at a leaf's end as that leaf's own
    // carry made the leaf after a mid-cluster cut segment as though the cut were a boundary, so the
    // backspace below stopped at 64 instead of taking the `e` and its marks from 63 (AUDIT D-24).
    let mut document = "a".repeat(63);
    document.push('e');
    document.push_str("\u{301}\u{301}\n");
    document.push_str(&"a".repeat(58));
    document.push('\n');
    let mut shell = session();
    shell.type_text(&document).expect("typed");
    shell.flush().expect("sealed");
    shell.reflow().expect("measured");
    assert_eq!(shell.document_len(), 128, "two leaves exactly");

    // Line 0 is `a`×63 and the `e` with its two marks, sixty-four units of text, so byte 68 is the
    // boundary at x = 64 — the cluster's end, never a byte inside it.
    let caret = shell
        .click(Point::new(Fixed::units(64), Fixed::ZERO))
        .expect("clicked");
    assert_eq!(caret.map(|caret| caret.byte), Some(68));

    assert!(shell.backspace().expect("a backspace"));
    assert_eq!(shell.document_len(), 123, "the `e` and its marks go together");
}

#[test]
fn a_click_cannot_put_the_caret_inside_the_cluster_the_window_cut_open() {
    // The frame ends where the window ends, and a window ends at a leaf — a byte count (D-22), so
    // the cut can fall inside a cluster. The eye cannot tell that from the frame's text alone, so
    // a click past the last cluster would put the caret inside a grapheme, and typing there would
    // split it. The document is what knows, and the position it has is the one before the cluster.
    let mut shell = session();
    let text = a_cluster_straddling_every_leaf();
    shell.type_text(&text).expect("typed");
    shell.flush().expect("sealed");
    shell.reflow().expect("measured");

    let starts: Vec<u64> = Clusters::new(&text).map(|(at, _)| at as u64).collect();
    let mut boundaries = Vec::new();
    let end = {
        let frame = shell.frame().expect("a frame");
        for line in frame.layout().lines() {
            for (x, byte) in line.boundaries() {
                boundaries.push((x, line.top(), byte));
            }
        }
        frame.origin() + frame.text().len() as u64
    };
    assert!(end > 0 && end < text.len() as u64, "the window is a prefix");
    assert_eq!(end % 64, 0, "it ends at a leaf");
    assert!(
        !starts.contains(&end),
        "and that leaf cut is inside a grapheme"
    );
    let before = starts
        .iter()
        .copied()
        .filter(|at| *at < end)
        .max()
        .expect("a cluster before the cut");

    let mut carets = 0;
    for (x, top, byte) in boundaries {
        let caret = shell
            .click(Point::new(x, top))
            .expect("clicked")
            .expect("a caret");
        if byte == end {
            assert_eq!(
                caret.byte, before,
                "the cut is inside the cluster at {before}, and a caret goes there"
            );
        } else {
            assert_eq!(caret.byte, byte, "a legal caret round-trips");
        }
        assert!(
            starts.contains(&caret.byte) || caret.byte == text.len() as u64,
            "the caret at {} is inside a grapheme",
            caret.byte
        );
        carets += 1;
    }
    assert!(carets > 1, "the frame offered more than its own end");
}

#[test]
fn a_backspace_crosses_two_leaf_boundaries_to_take_its_cluster() {
    // A base at the last byte of one leaf and forty marks after it: the cluster spans three leaves.
    // One leaf of context (D-21) stopped at the second; the carry follows it to where it begins.
    let mut document = "a".repeat(63);
    document.push('e');
    document.push_str(&"\u{301}".repeat(40));
    let mut shell = session();
    shell.type_text(&document).expect("typed");
    shell.flush().expect("sealed");
    shell.reflow().expect("measured");
    assert_eq!(shell.heights().len(), 3, "64, 64 and 16 bytes");
    assert_eq!(shell.document_len(), 144);

    assert!(shell.backspace().expect("a backspace"));
    assert_eq!(
        shell.document_len(),
        63,
        "the base and all forty marks go together"
    );
    assert_eq!(shell.caret(), 63);
}

#[test]
fn a_cluster_split_by_a_leaf_costs_its_advance_twice() {
    // OQ-18's residual, measured rather than asserted as right. A cluster's advance is exact only
    // when the cluster is shaped whole, and the shell lays leaves out one at a time: `e` + U+0301
    // is one unit wide in the document and two when a leaf cut falls between them at a 64-unit
    // wrap, so the scroll bar here is a line long. Segmentation is exact (OQ-17); this is what is
    // left, and fixing it needs the two leaves shaped together.
    let mut document = "a".repeat(63);
    document.push_str("e\u{301}");
    let mut shell = session();
    shell.type_text(&document).expect("typed");
    shell.flush().expect("sealed");
    shell.resize(Fixed::units(64), VIEWPORT);
    shell.reflow().expect("measured");

    let whole = layout_styled(StyledText::plain(&document), 0, Fixed::units(64), &Fake);
    assert_eq!(whole.height(), Fixed::units(20), "one line in the document");
    assert_eq!(
        shell.heights().total(),
        Fixed::units(40),
        "two leaves, and one glyph of advance counted at each side of the cut"
    );
}

#[test]
fn every_caret_a_frame_offers_is_a_character_boundary() {
    // The check OQ-15 said was missing, and that only a shell could make: every byte the frame
    // offers as a caret position must be a boundary of the text the frame was laid out from, and
    // every one must hit-test back to itself.
    let mut shell = session();
    shell
        .type_text("e\u{301}山\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}i")
        .expect("typed");
    shell.reflow().expect("measured");
    let frame = shell.frame().expect("a frame");
    let text = frame.text();
    let mut carets = 0;
    for line in frame.layout().lines() {
        for (x, byte) in line.boundaries() {
            assert!(
                text.is_char_boundary((byte - frame.origin()) as usize),
                "the caret at {byte} splits a character of {text:?}"
            );
            let hit = frame
                .layout()
                .hit_test(Point::new(x, line.top()))
                .expect("the caret paints inside its line");
            assert_eq!(hit.byte, byte, "the round trip moved the caret");
            carets += 1;
        }
    }
    assert_eq!(carets, 5, "four clusters and the end of the line");
}

#[test]
fn undo_of_a_cut_shows_up_in_the_next_frame() {
    let mut shell = session();
    shell.type_text("0123456789").expect("typed");
    shell.flush().expect("flushed");
    shell.reflow().expect("measured");
    assert_eq!(
        shell.residency().source().loads(),
        1,
        "measuring one leaf faulted one segment"
    );
    assert_eq!(shell.frame().expect("a frame").text(), "0123456789");

    shell.select_all();
    assert!(shell.delete_selection().expect("deleted"));
    assert_eq!(shell.frame().expect("a frame").text(), "");

    assert!(shell.undo().expect("undone"));
    assert_eq!(shell.frame().expect("a frame").text(), "0123456789");
    assert!(shell.redo().expect("redone"));
    assert_eq!(shell.frame().expect("a frame").text(), "");
}

#[test]
fn resizing_the_width_is_a_new_geometry() {
    let mut shell = session();
    shell.type_text("one two three four").expect("typed");
    shell.reflow().expect("measured");
    assert!(shell.heights().total() > Fixed::ZERO);

    // A narrower page re-wraps every line, so every measured height describes an old width.
    shell.resize(Fixed::units(8), VIEWPORT);
    assert_eq!(
        shell.heights().total(),
        Fixed::ZERO,
        "a width change empties the index rather than adjusting it"
    );
    assert_eq!(shell.scroll(), Fixed::ZERO, "there is nothing to scroll to");
    let narrow = shell.frame().expect("a frame");
    assert!(narrow.provisional(), "the first frame after a re-wrap");
    assert!(narrow.layout().lines().len() > 1, "eight units is narrow");

    // A height change alone measures nothing new.
    let total = shell.heights().total();
    shell.resize(Fixed::units(8), Fixed::units(400));
    assert_eq!(shell.heights().total(), total);
}

#[test]
fn the_view_transform_is_the_one_paint_and_hit_test_share() {
    let mut shell = session();
    shell.type_text("mimi").expect("typed");
    let device = View::new(2_000, 96).expect("a 200 % view");
    shell.set_view(device);
    let frame = shell.frame().expect("a frame");
    let caret = frame.caret().expect("the caret is on screen");
    let painted = device
        .paint_point(frame.layout(), caret.byte)
        .expect("a caret");
    assert_eq!(painted.byte, caret.byte);
    assert_eq!(painted.x, Fixed::from_64ths(caret.x.sixtyfourths() * 2));
    assert_eq!(painted.top, Fixed::from_64ths(caret.top.sixtyfourths() * 2));
    let hit = device
        .hit_test(frame.layout(), Point::new(painted.x, painted.top))
        .expect("a caret");
    assert_eq!(hit.byte, caret.byte, "paint and hit-test agree at 200 %");
    assert_eq!(shell.view(), device);
}

#[test]
fn an_empty_document_frames_without_reading_anything() {
    let mut shell = session();
    let frame = shell.frame().expect("a frame");
    assert!(frame.text().is_empty());
    assert!(frame.leaves().is_empty());
    assert_eq!(frame.shaped(), 0);
    assert!(
        !frame.provisional(),
        "nothing to measure is not provisional"
    );
    assert!(!shell.backspace().expect("nothing to delete"));
    assert!(!shell.delete_forward().expect("nothing to delete"));
    assert!(!shell.undo().expect("nothing to undo"));
}
