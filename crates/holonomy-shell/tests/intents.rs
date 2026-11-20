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

use holonomy_eye::{Fixed, Point, Shaper, Style, View};
use holonomy_shell::{Effect, Editor, Intent};
use holonomy_text::{Appended, ResidentSet, SegmentId, SegmentSink, SegmentSource, Typing};

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
    match rng.below(14) {
        0 | 1 | 2 => {
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
        12 => Intent::Undo,
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
        Intent::Undo => {
            shell.undo().expect("undone");
        }
        Intent::Redo => {
            shell.redo().expect("redone");
        }
        Intent::Flush => shell.flush().expect("flushed"),
        Intent::Reflow => shell.reflow().expect("measured"),
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
        shell.apply(Intent::ScrollTo(at)).expect("applied").is_empty(),
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
        effects.iter().any(|effect| matches!(effect, Effect::Caret(_))),
        "typing did not report the caret"
    );
    // A flush says the bytes landed, and asks for no repaint of its own.
    let effects = shell.apply(Intent::Flush).expect("applied");
    assert_eq!(effects, vec![Effect::Saved], "a flush said the wrong thing");
}
