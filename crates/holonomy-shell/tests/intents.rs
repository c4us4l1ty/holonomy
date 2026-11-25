//! Intents as data — `docs/TO-APP.md` **M2**.
//!
//! A window will speak to the editor through [`Intent`] and nothing else. That is only safe if the
//! door has no opinions: `apply(intent)` must do exactly what the method it names does. So this
//! harness builds **two** identical editors, drives one through intents and the other through the
//! direct calls, and compares their whole visible state after every single step — text, origin,
//! height, line count, caret, selection, document length, measured total, scroll, width, viewport
//! and view.
//!
//! The stream is randomised from a fixed seed, so a divergence names its step, and the intents
//! cover edits, selections, clicks, drags, scrolling, resizing, zooming and history. Alongside the
//! comparison, two invariants are asserted for every intent of the run: the caret never leaves the
//! document, and a frame always agrees with the caret the editor reports.
//!
//! The numbers print as `HOLONOMY_INTENTS intents= …`, and the counts are asserted: a green run
//! that applied no intents would be a green run that proved nothing.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use holonomy_eye::{Fixed, Point, Shaper, Style, View};
use holonomy_shell::{Editor, Effect, Intent, Move};
use holonomy_text::{
    Alignment, Appended, Heading, ResidentSet, SegmentId, SegmentSink, SegmentSource, StylePatch,
    Typing,
};

/// One logical unit per cluster, two for `m` and `中` — as in the other shell tests.
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

/// A sink that mints distinct segment ids and stores nothing.
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

/// A world-line that generates its leaves: each is `leaf_bytes` letters and no break.
#[derive(Debug)]
struct Repeated {
    leaf_bytes: usize,
}

impl SegmentSource for Repeated {
    fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
        if segment.get() >= 4_096 {
            return Err(format!("no {segment}"));
        }
        Ok(vec![b'a'; self.leaf_bytes])
    }
}

const WIDTH: Fixed = Fixed::units(200);
const VIEWPORT: Fixed = Fixed::units(40);
const BUDGET: usize = 256;
type Shell = Editor<Recorder, Repeated, Fake>;

fn editor() -> Shell {
    let residency =
        ResidentSet::new(Repeated { leaf_bytes: 64 }, BUDGET, 0).expect("a workable budget");
    Editor::new(
        Typing::new(Recorder::default(), 64),
        residency,
        Fake,
        WIDTH,
        VIEWPORT,
    )
}

/// Shared in-memory world-line store so sealed leaves fault back the exact bytes typed.
#[derive(Debug, Clone, Default)]
struct World {
    segments: Rc<RefCell<BTreeMap<u64, Vec<u8>>>>,
    next: Rc<Cell<u64>>,
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
        self.segments
            .borrow()
            .get(&segment.get())
            .cloned()
            .ok_or_else(|| format!("missing {segment}"))
    }
}

fn world_editor(slab_bytes: usize) -> Editor<World, World, Fake> {
    let store = World::default();
    let residency = ResidentSet::new(store.clone(), BUDGET, 0).expect("a workable budget");
    Editor::new(
        Typing::new(store, slab_bytes),
        residency,
        Fake,
        WIDTH,
        VIEWPORT,
    )
}

/// Everything a user (or a renderer) can see, in one comparable value.
#[derive(Debug, PartialEq, Eq)]
struct State {
    text: String,
    origin: u64,
    height: Fixed,
    lines: usize,
    caret: Option<u64>,
    selection: Option<(u64, u64)>,
    document_len: u64,
    total: Fixed,
    scroll: Fixed,
    width: Fixed,
    viewport: Fixed,
    view: View,
}

fn state(shell: &mut Shell) -> State {
    let document_len = shell.document_len();
    let total = shell.heights().total();
    let scroll = shell.scroll();
    let width = shell.width();
    let viewport = shell.viewport();
    let view = shell.view();
    let reported = shell.caret();
    let selection = shell.selection();
    let selection = (selection.anchor() != selection.focus())
        .then_some((selection.anchor(), selection.focus()));
    let (text, origin, height, lines, caret) = {
        let frame = shell.frame().expect("a frame");
        (
            frame.text().to_owned(),
            frame.origin(),
            frame.layout().height(),
            frame.layout().lines().len(),
            frame.caret().map(|caret| caret.byte),
        )
    };
    // The frame's caret and the editor's are the same position whenever the caret is on screen.
    if let Some(shown) = caret {
        assert_eq!(
            shown, reported,
            "the frame's caret does not agree with the editor's"
        );
    }
    assert!(
        reported <= document_len,
        "the caret {reported} is past the document's {document_len} bytes"
    );
    State {
        text,
        origin,
        height,
        lines,
        caret,
        selection,
        document_len,
        total,
        scroll,
        width,
        viewport,
        view,
    }
}

/// A small deterministic generator, so a divergence always names the same step.
struct Rng(u64);

impl Rng {
    fn bump(&mut self) -> u64 {
        // xorshift64*, chosen because it is four lines and has no dependencies.
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            return 0;
        }
        self.bump() % bound
    }

    fn units(&mut self, bound: i64) -> Fixed {
        Fixed::units((self.below(bound as u64) as i64).min(bound - 1))
    }

    fn point(&mut self) -> Point {
        // Bound first: two `&mut self` calls in one expression do not borrow-check.
        let x = self.units(200);
        let y = self.units(40);
        Point::new(x, y)
    }
}

/// The intent a seed step asks for, and the direct call that means the same thing.
fn step(seed: u64, shell_len: u64) -> Intent {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    match rng.below(17) {
        0..=2 => {
            let text = if rng.below(3) == 0 { "m中\n" } else { "ab" };
            Intent::Type(text.to_owned())
        }
        3 => Intent::Backspace,
        4 => Intent::DeleteForward,
        5 => Intent::SelectAll,
        6 => Intent::Click(rng.point()),
        7 => Intent::DragTo(rng.point()),
        8 => Intent::ScrollTo(rng.units(1_200)),
        9 => Intent::ScrollBy(Fixed::units(if rng.below(2) == 0 { 20 } else { -20 })),
        10 => {
            let width = Fixed::units(120 + rng.below(200) as i64);
            let viewport = Fixed::units(40 + rng.below(100) as i64);
            Intent::Resize { width, viewport }
        }
        11 => Intent::SetView(View::new(500 + rng.below(1_500) as i64, 96).expect("a view")),
        12 => Intent::Move {
            to: match rng.below(6) {
                0 => Move::Left,
                1 => Move::Right,
                2 => Move::WordLeft,
                3 => Move::WordRight,
                4 => Move::Up,
                _ => Move::Down,
            },
            extend: rng.below(2) == 0,
        },
        13 => Intent::Undo,
        14 => Intent::DeleteWordBefore,
        15 => Intent::DeleteWordAfter,
        _ => {
            // Redo, reveal and flush share the tail; flush only when there is something to write.
            if rng.below(2) == 0 && shell_len > 0 {
                Intent::Redo
            } else if rng.below(2) == 0 {
                Intent::RevealCaret
            } else {
                Intent::Flush
            }
        }
    }
}

/// The direct calls an intent names — the "second implementation" the intent must agree with.
fn directly(shell: &mut Shell, intent: &Intent) {
    match intent {
        Intent::Type(text) => shell.type_text(text).expect("typed"),
        Intent::Backspace => {
            shell.backspace().expect("backspaced");
        }
        Intent::DeleteForward => {
            shell.delete_forward().expect("deleted");
        }
        Intent::DeleteWordBefore => {
            shell.delete_word_before().expect("deleted word before");
        }
        Intent::DeleteWordAfter => {
            shell.delete_word_after().expect("deleted word after");
        }
        Intent::DeleteSelection => {
            shell.delete_selection().expect("deleted");
        }
        Intent::SelectAll => shell.select_all(),
        Intent::Click(point) => {
            shell.click(*point).expect("clicked");
        }
        Intent::DragTo(point) => {
            shell.drag_to(*point).expect("dragged");
        }
        Intent::ScrollTo(top) => shell.scroll_to(*top),
        Intent::ScrollBy(delta) => shell.scroll_by(*delta),
        Intent::Resize { width, viewport } => shell.resize(*width, *viewport),
        Intent::SetView(view) => shell.set_view(*view),
        Intent::RevealCaret => {
            shell.reveal_caret().expect("revealed");
        }
        Intent::Move { to, extend } => {
            shell.move_caret_to(*to, *extend).expect("moved");
        }
        Intent::Undo => {
            shell.undo().expect("undone");
        }
        Intent::Redo => {
            shell.redo().expect("redone");
        }
        Intent::Flush => shell.flush().expect("flushed"),
        Intent::Reflow => shell.reflow().expect("measured"),
        Intent::Format(patch) => {
            shell.format_selection(patch).expect("formatted");
        }
        Intent::SetLink(url) => {
            shell.set_selection_link(url.as_deref()).expect("linked");
        }
        Intent::ReplaceAll {
            needle,
            replacement,
        } => {
            shell.replace_all(needle, replacement).expect("replaced");
        }
    }
}

#[test]
fn a_window_speaking_intents_and_a_caller_calling_methods_see_the_same_editor() {
    let mut through_intents = editor();
    let mut directly_called = editor();
    through_intents
        .type_text("the first line\nand the second\n")
        .expect("seeded");
    directly_called
        .type_text("the first line\nand the second\n")
        .expect("seeded");
    through_intents.reflow().expect("measured");
    directly_called.reflow().expect("measured");

    let mut redraws = 0u64;
    let mut carets = 0u64;
    let mut saves = 0u64;
    let steps = 240u64;
    for seed in 0..steps {
        let intent = step(seed, through_intents.document_len());
        let effects = through_intents.apply(intent.clone()).expect("applied");
        directly(&mut directly_called, &intent);
        for effect in effects {
            match effect {
                Effect::Redraw => redraws += 1,
                Effect::Caret(_) => carets += 1,
                Effect::Saved => saves += 1,
            }
        }
        let one = state(&mut through_intents);
        let two = state(&mut directly_called);
        assert_eq!(
            one, two,
            "step {seed}: {intent:?} left the editor in a different state than the direct call"
        );
    }
    println!(
        "HOLONOMY_INTENTS intents={steps} redraws={redraws} carets={carets} saves={saves} \
         bytes={}",
        through_intents.document_len()
    );
    assert_eq!(steps, 240, "the run applied every step");
    assert!(redraws > 100, "only {redraws} redraws were asked for");
    assert!(carets > 10, "only {carets} caret effects — nothing moved");
    assert!(saves > 0, "the run never reached the sink");
    assert!(
        through_intents.document_len() > 0,
        "the run emptied the document; it measured nothing"
    );
}

#[test]
fn a_redraw_is_asked_for_exactly_when_something_changed() {
    let mut shell = editor();
    shell.type_text("ab").expect("typed");
    shell.flush().expect("flushed");
    shell.reflow().expect("measured");

    // Scrolling to where we already are is not a change.
    let at = shell.scroll();
    assert!(
        shell
            .apply(Intent::ScrollTo(at))
            .expect("applied")
            .is_empty(),
        "a scroll that does not move asked for a repaint"
    );
    // Nor is a resize to the size we already have.
    let (width, viewport) = (shell.width(), shell.viewport());
    assert!(
        shell
            .apply(Intent::Resize { width, viewport })
            .expect("applied")
            .is_empty(),
        "a resize that changes nothing asked for a repaint"
    );
    // A backspace at the top of the document deletes nothing, so it paints nothing.
    shell.select_all();
    shell
        .click(Point::new(Fixed::ZERO, Fixed::ZERO))
        .expect("a click");
    let effects = shell.apply(Intent::Backspace).expect("applied");
    assert!(
        effects.is_empty(),
        "a backspace that deleted nothing repainted"
    );
    // Typing always repaints and always reports the caret.
    let effects = shell.apply(Intent::Type("c".to_owned())).expect("applied");
    assert!(effects.contains(&Effect::Redraw), "typing did not repaint");
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Caret(_))),
        "typing did not report the caret"
    );
    // A flush says the bytes landed, and asks for no repaint of its own.
    let effects = shell.apply(Intent::Flush).expect("applied");
    assert_eq!(effects, vec![Effect::Saved], "a flush said the wrong thing");
}

#[test]
fn formatting_links_and_replace_all_intents_agree_with_direct_calls() {
    let mut through_intents = editor();
    let mut directly_called = editor();
    for sh in [&mut through_intents, &mut directly_called] {
        sh.type_text("Heading line\nBody paragraph with target and target.\n")
            .expect("seeded");
        sh.reflow().expect("measured");
    }

    let sequence = [
        Intent::SelectAll,
        Intent::Format(StylePatch {
            bold: Some(true),
            heading: Some(Heading::H2),
            alignment: Some(Alignment::Center),
            ..StylePatch::default()
        }),
        Intent::SetLink(Some("https://example.org/spec".to_owned())),
        Intent::ReplaceAll {
            needle: "target".to_owned(),
            replacement: "witness".to_owned(),
        },
        Intent::Undo,
        Intent::Undo,
        Intent::Redo,
        Intent::Redo,
    ];

    for (idx, intent) in sequence.iter().enumerate() {
        through_intents.apply(intent.clone()).expect("applied");
        directly(&mut directly_called, intent);
        assert_eq!(
            state(&mut through_intents),
            state(&mut directly_called),
            "rich-text step {idx} ({intent:?}) diverged from direct call"
        );
        assert_eq!(
            through_intents.toolbar_state(),
            directly_called.toolbar_state(),
            "toolbar_state diverged at step {idx}"
        );
    }
}

#[test]
fn word_navigation_multi_click_and_status_metrics_work_across_leaves() {
    // Verify unsealed open-buffer metrics & cluster helpers before any slab is sealed.
    let mut unsealed = world_editor(256);
    unsealed
        .type_text("alpha beta gamma\nsecond paragraph line")
        .expect("seeded unsealed");
    let unsealed_metrics = unsealed.status_metrics();
    assert!(unsealed_metrics.words_exact);
    assert_eq!(unsealed_metrics.total_words, 6);
    unsealed
        .apply(Intent::DeleteWordBefore)
        .expect("unsealed del word before");
    assert_eq!(unsealed.status_metrics().total_words, 5);

    // 12-byte slabs split "alpha beta gamma\nsecond paragraph line" across multiple leaves.
    let mut shell = world_editor(12);
    shell
        .type_text("alpha beta gamma\nsecond paragraph line")
        .expect("seeded");
    shell.reflow().expect("reflowed");

    // WordRight / WordLeft navigation & DeleteWordBefore / DeleteWordAfter.
    let home = Intent::Move {
        to: Move::DocumentStart,
        extend: false,
    };
    shell.apply(home).expect("home");
    let word_r = Intent::Move {
        to: Move::WordRight,
        extend: false,
    };
    shell.apply(word_r).expect("word right");
    assert_eq!(shell.caret(), 5);
    let word_r_ext = Intent::Move {
        to: Move::WordRight,
        extend: true,
    };
    shell.apply(word_r_ext).expect("word right extend");
    assert_eq!(shell.selection().normalised(), (5, 10));
    let word_l = Intent::Move {
        to: Move::WordLeft,
        extend: false,
    };
    shell.apply(word_l).expect("word left");
    assert_eq!(shell.caret(), 6);

    // Double-click word, triple-click line, quad-click paragraph via painted geometry.
    let frame_pt = {
        let f = shell.frame().expect("frame");
        let p = f.layout().paint_point(7).expect("paint");
        Point::new(p.x, p.top + Fixed::ONE)
    };
    assert!(shell.select_word_at(frame_pt).expect("double click"));
    assert_eq!(shell.selection().normalised(), (6, 10));
    assert!(shell.select_visual_line_at(frame_pt).expect("triple click"));
    assert_eq!(shell.selection().normalised().0, 0);
    assert!(shell.select_paragraph_at(frame_pt).expect("quad click"));
    assert_eq!(shell.selection().normalised(), (0, 16));

    // DeleteWordAfter / DeleteWordBefore parity with intents.
    shell.select_range(0, 0).expect("reset");
    shell.apply(Intent::DeleteWordAfter).expect("del after");
    assert!(shell.frame().expect("frame").text().starts_with(" beta"));
    shell.apply(Intent::Undo).expect("undo");
    shell.select_range(5, 5).expect("at end of alpha");
    shell.apply(Intent::DeleteWordBefore).expect("del before");
    assert!(shell.frame().expect("frame").text().starts_with(" beta"));

    // Link inspection and status metrics.
    shell.select_range(1, 5).expect("select beta");
    let set_link = Intent::SetLink(Some("https://holonomy.invalid/word".to_owned()));
    shell.apply(set_link).expect("link");
    assert_eq!(
        shell.link_at_caret().as_deref(),
        Some("https://holonomy.invalid/word")
    );
    let metrics = shell.status_metrics();
    assert!(metrics.words_exact);
    assert!(metrics.total_words >= 4);
    assert_eq!(metrics.current_page, 1);
    assert!(metrics.total_pages >= 1);
}
