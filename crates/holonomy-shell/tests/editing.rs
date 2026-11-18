//! The loop: intents in, frames out — and the check OQ-15 said needed a shell to exist.
//!
//! Every test drives the editor the way a window would: type, click, scroll, resize, and look at
//! the frame that comes back. The shaper is a deterministic fake with uneven advances, so the
//! numbers are exact rather than approximate — which is what makes the caret check at the end a
//! proof about every caret a frame offers instead of a sample of them.

use holonomy_eye::{Fixed, Point, Shaper, Style, View};
use holonomy_shell::Editor;
use holonomy_text::{Appended, ResidentSet, SegmentId, SegmentSink, SegmentSource, Typing};
use std::collections::BTreeMap;

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

/// A sink that mints distinct segment ids, so a session can run without a world-line.
#[derive(Debug, Default)]
struct Recorder {
    next: u64,
}

impl SegmentSink for Recorder {
    fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String> {
        let segment = SegmentId::new(self.next);
        self.next += 1;
        Ok(Appended {
            segment,
            witness_sequence: self.next,
            segment_root: [0u8; 32],
            bytes_written: plaintext.len() as u64,
        })
    }
}

/// The world-line, as plaintext by segment id, counting what it is asked for.
#[derive(Debug, Default)]
struct Segments {
    segments: BTreeMap<u64, Vec<u8>>,
    loads: usize,
}

impl Segments {
    fn with(entries: &[(u64, &str)]) -> Self {
        let segments = entries
            .iter()
            .map(|(id, text)| (*id, text.as_bytes().to_vec()))
            .collect();
        Self { segments, loads: 0 }
    }
}

impl SegmentSource for Segments {
    fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
        self.loads += 1;
        self.segments
            .get(&segment.get())
            .cloned()
            .ok_or_else(|| format!("no {segment}"))
    }
}

/// A world-line that generates its leaves, so a four-thousand-leaf document costs the residency
/// nothing: each leaf is `"a" * (leaf_bytes - 1) + "\n"`.
#[derive(Debug)]
struct Repeated {
    leaf_bytes: usize,
}

impl SegmentSource for Repeated {
    fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
        if segment.get() >= 4_096 {
            return Err(format!("no {segment}"));
        }
        let mut bytes = vec![b'a'; self.leaf_bytes];
        if let Some(last) = bytes.last_mut() {
            *last = b'\n';
        }
        Ok(bytes)
    }
}

const WIDTH: Fixed = Fixed::units(200);
const VIEWPORT: Fixed = Fixed::units(40);
/// Four leaf-sized buffers fit in the resident set; measuring a document is not holding it.
const BUDGET: usize = 256;

fn editor<R: SegmentSource>(source: R) -> Editor<Recorder, R, Fake> {
    let residency = ResidentSet::new(source, BUDGET, 0).expect("a workable budget");
    Editor::new(Typing::new(Recorder::default(), 64), residency, Fake, WIDTH, VIEWPORT)
}

/// One leaf of a [`Repeated`] document.
fn repeat_leaf() -> String {
    "a".repeat(63) + "\n"
}

#[test]
fn typing_is_visible_in_the_next_frame() {
    let mut shell = editor(Segments::default());
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
    let mut shell = editor(Segments::default());
    shell.type_text("one\ntwo\nthree\n").expect("typed");
    let first = shell.frame().expect("a frame");
    assert!(first.provisional(), "nothing had been measured before it");
    assert_eq!(
        shell.heights().total(),
        first.layout().height(),
        "the frame wrote back what it laid out"
    );
    assert_eq!(shell.heights().len(), 1);

    let second = shell.frame().expect("a frame");
    assert!(!second.provisional(), "the first frame measured the screen");
    assert_eq!(second.text(), "one\ntwo\nthree\n");
    assert_eq!(second.layout().lines().len(), 4, "the empty last line counts");
}

#[test]
fn reflow_measures_every_leaf_one_at_a_time() {
    let mut shell = editor(Repeated { leaf_bytes: 64 });
    shell.type_text(&repeat_leaf().repeat(64)).expect("typed");
    assert_eq!(shell.document_len(), 64 * 64);
    shell.reflow().expect("measured");

    assert_eq!(shell.heights().len(), 64, "one leaf per sealed segment");
    assert_eq!(
        shell.heights().total(),
        Fixed::units(64 * 20),
        "sixty-four lines of twenty units, measured leaf by leaf"
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

    shell.scroll_to(Fixed::units(64 * 20));
    assert_eq!(
        shell.scroll(),
        Fixed::units(64 * 20 - 40),
        "the scroll clamps to the last screen"
    );
    let frame = shell.frame().expect("a frame");
    assert!(!frame.provisional());
    assert_eq!(frame.leaves().len(), 2, "a two-line screen");
    assert!(frame.text().ends_with("a\n"), "{:?}", frame.text());
}

#[test]
fn a_keystroke_on_a_long_document_shapes_only_the_screen() {
    let mut shell = editor(Repeated { leaf_bytes: 64 });
    shell.type_text(&repeat_leaf().repeat(4_000)).expect("typed");
    shell.reflow().expect("measured");
    assert_eq!(shell.heights().len(), 4_000);
    let measured = shell.heights().total();
    assert_eq!(measured, Fixed::units(4_000 * 20));

    // Halfway down a document this has never shown: the frame shapes two leaves, not four thousand.
    shell.scroll_to(Fixed::units(4_000 * 20 / 2));
    let frame = shell.frame().expect("a frame");
    assert_eq!(frame.leaves().len(), 2);
    assert!(
        frame.shaped() <= 130,
        "{} clusters shaped for a two-line screen",
        frame.shaped()
    );
    assert!(
        frame.reads() <= 8 * 12 + 32,
        "{} index reads for 4000 leaves",
        frame.reads()
    );

    // A keystroke at the document's end appends one leaf, and the index grows by one instead of
    // being rebuilt: every height measured a moment ago is still the truth.
    shell.type_text("z").expect("typed");
    let after = shell.frame().expect("a frame");
    assert_eq!(shell.heights().len(), 4_001, "one appended leaf");
    assert_eq!(
        shell.heights().total(),
        measured,
        "a tail append invalidated no measured height"
    );
    assert!(after.shaped() <= 130, "the screen is still the screen");
}

#[test]
fn backspace_takes_the_whole_cluster() {
    let mut shell = editor(Segments::default());
    // `e` + COMBINING ACUTE is one cluster of three bytes, and the caret sits after all three.
    shell.type_text("e\u{301}x").expect("typed");
    assert!(shell.backspace().expect("deleted"));
    assert_eq!(shell.document_len(), 1);
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
    let mut shell = editor(Segments::default());
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
    assert_eq!(shell.document_len(), 1, "one byte was left outside the selection");
    assert_eq!(shell.frame().expect("a frame").text(), "i");
}

#[test]
fn every_caret_a_frame_offers_is_a_character_boundary() {
    // The check OQ-15 said was missing, and that only a shell could make: every byte the frame
    // offers as a caret position must be a boundary of the text the frame was laid out from, and
    // every one must hit-test back to itself.
    let mut shell = editor(Segments::default());
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
    let mut shell = editor(Segments::with(&[(0, "0123456789")]));
    shell.type_text("0123456789").expect("typed");
    shell.flush().expect("flushed");
    shell.reflow().expect("measured");
    assert_eq!(
        shell.residency().source().loads,
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
    let mut shell = editor(Segments::default());
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
    let mut shell = editor(Segments::default());
    shell.type_text("mimi").expect("typed");
    let device = View::new(2_000, 96).expect("a 200 % view");
    shell.set_view(device);
    let frame = shell.frame().expect("a frame");
    let caret = frame.caret().expect("the caret is on screen");
    let painted = device.paint_point(frame.layout(), caret.byte).expect("a caret");
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
    let mut shell = editor(Segments::default());
    let frame = shell.frame().expect("a frame");
    assert!(frame.text().is_empty());
    assert!(frame.leaves().is_empty());
    assert_eq!(frame.shaped(), 0);
    assert!(!frame.provisional(), "nothing to measure is not provisional");
    assert!(!shell.backspace().expect("nothing to delete"));
    assert!(!shell.delete_forward().expect("nothing to delete"));
    assert!(!shell.undo().expect("nothing to undo"));
}
