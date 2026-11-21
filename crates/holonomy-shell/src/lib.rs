//! The editor loop: intents in, a frame out — the headless half of Block 6's shell.
//!
//! Block 5 ended with a chain of parts that each had a test and none had a caller: the height index
//! knew where a viewport was, the window join turned that into bytes, the reader turned bytes into
//! text, and the run tree turned text into geometry that paint and hit-test both walk. This crate
//! is the loop that closes the ring — an **intent** (a keystroke, a click, a scroll, a resize)
//! changes one of the parts, and a **frame** is what a window would draw.
//!
//! # What this is not
//!
//! There is no window, no font, no GPU, no palette and no GPUI in this crate, and nothing here
//! claims any of Block 6's gates. Those gates are about a shipped app on reference hardware: a
//! budget table in seconds, kill-power recovery, a hand-off drill, zero idle repaint. What can be
//! built and *proven* before any of that exists is the loop itself, and that is what this is. The
//! shaper is still a trait the shell is generic over, so the fake stands where Block 8's pipeline
//! will stand, and gate 2's clock stays unclaimed (AUDIT OQ-12).
//!
//! # The height write-back, and why it is a loop
//!
//! The Eye's height index is *measured*, never known: leaf heights are zero until something lays
//! the leaf out. A shell is exactly the thing that lays leaves out, so [`Editor::frame`] does what
//! no earlier layer could — it writes what the layout measured back into the index. Three
//! consequences are worth stating rather than discovering:
//!
//! * A leaf's height is the height of the **lines that start in it**. That partitions the document
//!   exactly (every line starts once, so the heights sum to the height of one continuous layout of
//!   the document) which is why the index also stores **where each leaf's first cluster sits in its
//!   line, whether the leaf ended on a bare CR, and the segmentation state its text ended in**.
//!   Without the first, a paragraph split across two leaves would break where the leaf ends and the
//!   scroll bar would grow a line per boundary (AUDIT OQ-16, D-20); without the second, a `\r\n`
//!   whose two bytes fall either side of a leaf cut would be charged two lines instead of one
//!   (AUDIT D-21); without the third, a leaf that opens inside a cluster would invent a caret no
//!   renderer draws and would stop a backspace at its edge (AUDIT OQ-17, D-22).
//! * Because the carry is stored, a frame can begin at a **line** rather than at a leaf. A screen
//!   that began at a leaf boundary would re-wrap the paragraph from a point the document never
//!   breaks at, and every cluster below it would be drawn where the document does not put it.
//! * A **width change is a new geometry.** Every measured height and every carried offset is a
//!   claim about the old wrap width, so [`Editor::resize`] empties both rather than adjusting them,
//!   and the next frame is *provisional*: with nothing measured there is nothing to scroll by
//!   either, so it starts at the top and widens leaf by leaf until the screen is full.
//!   [`Frame::provisional`] says so.
//!
//! # The caret is the document's, the cluster is the Eye's
//!
//! Editing deletes clusters, not bytes and not characters: `backspace` asks the document for the
//! bytes of the leaf the caret is in, asks the Eye's segmentation where the last cluster begins,
//! and cuts there. The two layers agree because the caret only ever lands on a boundary the Eye
//! produced — which is the check OQ-15 said was missing, and which `tests/editing.rs` now performs
//! over every caret a frame offers.

mod intent;

pub use crate::intent::{Effect, Intent, Move};

use holonomy_eye::{
    byte_window, layout_styled, layout_styled_continuing, window_text, Caret, ClusterContext,
    Clusters, Fixed, HeightModel, Layout, Picture, Point, ShapedWindow, Shaper, StyledText, View,
    Window, WindowText, WindowTextError,
};
use holonomy_text::{
    LiveSpan, ResidentSet, SegmentSink, SegmentSource, Selection, Typing, TypingError,
};

/// Why an intent could not be carried out.
#[derive(Debug)]
pub enum ShellError {
    /// The document refused the edit — an offset inside a character, or a spine range it will not
    /// take.
    Typing(TypingError),
    /// The window's text could not be read: a source the register does not know, a refused
    /// fault-in, or a range whose edge is not a character boundary (OQ-15).
    Read(WindowTextError),
}

impl std::fmt::Display for ShellError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Typing(error) => write!(formatter, "{error}"),
            Self::Read(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for ShellError {}

impl From<TypingError> for ShellError {
    fn from(error: TypingError) -> Self {
        Self::Typing(error)
    }
}

impl From<WindowTextError> for ShellError {
    fn from(error: WindowTextError) -> Self {
        Self::Read(error)
    }
}

/// One frame: the text of the viewport, its layout, and what it cost.
#[derive(Debug)]
pub struct Frame<'a> {
    text: &'a str,
    origin: u64,
    layout: Layout<'a>,
    leaves: Window,
    reads: u64,
    shaped: usize,
    provisional: bool,
    caret: Option<Caret>,
    selection: Option<(u64, u64)>,
}

impl<'a> Frame<'a> {
    /// The viewport's text, in document coordinates starting at [`Self::origin`].
    #[must_use]
    pub fn text(&self) -> &'a str {
        self.text
    }

    /// The document offset of the frame's first byte — what the run tree was laid out from.
    #[must_use]
    pub fn origin(&self) -> u64 {
        self.origin
    }

    /// The run tree both consumers walk: paint through a [`View`], hit-test through the same one.
    #[must_use]
    pub fn layout(&self) -> &Layout<'a> {
        &self.layout
    }

    /// The leaves the height index said were on screen.
    #[must_use]
    pub fn leaves(&self) -> Window {
        self.leaves
    }

    /// How many height-index cells this frame read. The cost number, counted, not asserted.
    #[must_use]
    pub fn reads(&self) -> u64 {
        self.reads
    }

    /// How many clusters this frame shaped.
    #[must_use]
    pub fn shaped(&self) -> usize {
        self.shaped
    }

    /// Whether the geometry this frame was built on had been measured — false means the heights
    /// were empty and the frame widened from the top of the document instead of from the scroll
    /// position.
    #[must_use]
    pub fn provisional(&self) -> bool {
        self.provisional
    }

    /// Where the caret is, when it is inside this frame.
    #[must_use]
    pub fn caret(&self) -> Option<Caret> {
        self.caret
    }

    /// The selection this frame was drawn for, as ordered byte ends, when it is not empty.
    ///
    /// A frame carries the selection as well as the caret because a renderer paints both from one
    /// picture; it never re-derives a selection from the editor between a frame and its paint.
    #[must_use]
    pub fn selection(&self) -> Option<(u64, u64)> {
        self.selection
    }

    /// This frame as a picture: an owned, drawable view model (ROADMAP Block 5, `docs/TO-APP.md`
    /// M1).
    ///
    /// Nothing is recomputed here — the clusters' x and advances are the layout's, the caret is
    /// [`Layout::paint_point`]'s, and the selection's runs come from the same walk that places
    /// carets. A renderer drops the frame, keeps the picture, and draws it later.
    #[must_use]
    pub fn picture(&self) -> Picture {
        self.layout
            .picture(self.caret.map(|caret| caret.byte), self.selection)
    }
}

/// How many leaves one cluster is followed across by `cluster_before`/`cluster_after`.
///
/// A leaf cut can land inside a cluster, and a cluster — a base plus any number of marks — can
/// therefore span leaves. The walk follows it, but a document that is thousands of leaves of
/// nothing but combining marks would make one backspace a walk of the document, so it stops here
/// and the part of the cluster it has is the answer. Eight leaves of one grapheme is a guard
/// against a hostile document, not a claim about text.
const CLUSTER_LEAF_LIMIT: usize = 8;

/// The editor loop: a session, a resident set, a height index, a view and a selection.
///
/// One frame is a pure function of those five and the shaper; an intent is the smallest edit to one
/// of them that the user asked for. Nothing here holds a window's state — the shell's job is to be
/// the thing a window *calls*, not the thing it *is*.
pub struct Editor<S, R: SegmentSource, H> {
    typing: Typing<S>,
    residency: ResidentSet<R>,
    measured: Measured,
    shaper: H,
    window: Option<WindowText>,
    width: Fixed,
    viewport: Fixed,
    scroll: Fixed,
    view: View,
    selection: Selection,
    /// The spine's length when the measured index was last brought in step with it (D-50).
    ///
    /// Leaf *counts* cannot tell a tail append from a mid-document insert: both add one leaf. What
    /// tells them apart is where the new leaf begins — at the byte where the document used to end,
    /// or before it. This records that byte, so `sync_measured` can answer the question instead of
    /// guessing.
    len_synced: u64,
}

impl<S: SegmentSink, R: SegmentSource, H: Shaper> Editor<S, R, H> {
    /// A shell over a session and a resident set, with nothing measured and nothing scrolled.
    ///
    /// `width` is the wrap width and `viewport` the visible height, both in logical units: they are
    /// what the window would report, and the loop treats them as fixed until [`Self::resize`].
    #[must_use]
    pub fn new(
        typing: Typing<S>,
        residency: ResidentSet<R>,
        shaper: H,
        width: Fixed,
        viewport: Fixed,
    ) -> Self {
        let leaves = typing.spine().slab_count() as usize;
        let len = typing.spine().len();
        let measured = Measured::new(leaves);
        Self {
            typing,
            residency,
            measured,
            shaper,
            window: None,
            width,
            viewport,
            scroll: Fixed::ZERO,
            view: View::IDENTITY,
            selection: Selection::caret(0),
            len_synced: len,
        }
    }

    /// The typing session — the document's live geometry and its history.
    #[must_use]
    pub fn typing(&self) -> &Typing<S> {
        &self.typing
    }

    /// The resident set, for a caller that wants to report what is in memory.
    #[must_use]
    pub fn residency(&self) -> &ResidentSet<R> {
        &self.residency
    }

    /// The measured heights, one per leaf.
    #[must_use]
    pub fn heights(&self) -> &HeightModel {
        &self.measured.heights
    }

    /// The document's live length, drafts applied.
    #[must_use]
    pub fn document_len(&self) -> u64 {
        self.typing.document_len()
    }

    /// The caret, as a document offset.
    #[must_use]
    pub fn caret(&self) -> u64 {
        self.typing.caret()
    }

    /// The selection, whose focus is the caret.
    #[must_use]
    pub fn selection(&self) -> Selection {
        self.selection
    }

    /// The document coordinate of the viewport's top edge.
    #[must_use]
    pub fn scroll(&self) -> Fixed {
        self.scroll
    }

    /// The wrap width in logical units.
    #[must_use]
    pub fn width(&self) -> Fixed {
        self.width
    }

    /// The viewport's height in logical units.
    #[must_use]
    pub fn viewport(&self) -> Fixed {
        self.viewport
    }

    /// The zoom and DPI transform, shared by paint and hit-test.
    #[must_use]
    pub fn view(&self) -> View {
        self.view
    }

    /// Sets the zoom and DPI transform. Geometry is unaffected: the run tree is logical.
    pub fn set_view(&mut self, view: View) {
        self.view = view;
    }

    /// Scrolls so `top` is the viewport's first logical unit, clamped into the document.
    pub fn scroll_to(&mut self, top: Fixed) {
        let known = self.measured.total();
        self.scroll_to_within(top, known);
    }

    /// Sets the scroll position, clamped so `top` can be reached even when the index is short.
    ///
    /// `measured.total()` is the document's height only while every leaf has been measured. A
    /// tail leaf is pushed by `sync_measured` with no height and measured by the frame that draws
    /// it, so between a keystroke and that frame the total is **short by the leaf the user is
    /// typing into** — and clamping to it makes the reveal a no-op exactly when the caret is in
    /// that leaf (D-58). `known` is the furthest document position the caller has evidence for;
    /// the clamp is against whichever is larger.
    fn scroll_to_within(&mut self, top: Fixed, known: Fixed) {
        let document = self.measured.total().max(known);
        let max = document.sixtyfourths() - self.viewport.sixtyfourths();
        let max = if max < 0 {
            Fixed::ZERO
        } else {
            Fixed::from_64ths(max)
        };
        self.scroll = if top < Fixed::ZERO {
            Fixed::ZERO
        } else if top > max {
            max
        } else {
            top
        };
    }

    /// Scrolls by `delta` logical units, clamped like [`Self::scroll_to`].
    pub fn scroll_by(&mut self, delta: Fixed) {
        self.scroll_to(self.scroll + delta);
    }

    /// Reports the window's new size.
    ///
    /// A different **width** is a different geometry: every measured height described the old wrap,
    /// so the index is emptied and the scroll position goes with it — there is nothing to scroll to
    /// until something has been measured. A different height alone measures nothing new.
    pub fn resize(&mut self, width: Fixed, viewport: Fixed) {
        if width != self.width {
            // Every measured height and every carried line offset described the old wrap.
            let leaves = self.typing.spine().slab_count() as usize;
            self.measured = Measured::new(leaves);
            self.len_synced = self.typing.spine().len();
            self.scroll = Fixed::ZERO;
        }
        self.width = width;
        self.viewport = viewport;
        // The maximum scroll is a function of the viewport, not of the document: a taller window
        // can leave the current position past the end, showing the document's last line at the top
        // of an empty screen. `scroll_to` is the one place that clamp lives, so it is asked again.
        self.scroll_to(self.scroll);
    }

    /// Types `text` at the caret, replacing the selection first if there is one.
    ///
    /// # Errors
    ///
    /// As [`Typing::insert`], or a read that the loop needed to place the caret afterwards.
    pub fn type_text(&mut self, text: &str) -> Result<(), ShellError> {
        if text.is_empty() {
            return Ok(());
        }
        if !self.selection.is_empty() {
            self.delete_selection()?;
        }
        self.typing.insert(text.as_bytes())?;
        self.selection = Selection::caret(self.typing.caret());
        Ok(())
    }

    /// Deletes the selection, reporting whether there was one.
    ///
    /// # Errors
    ///
    /// As [`Typing::delete`].
    pub fn delete_selection(&mut self) -> Result<bool, ShellError> {
        let (start, end) = self.selection.normalised();
        if start >= end {
            return Ok(false);
        }
        self.typing.delete(start, end)?;
        self.selection = Selection::caret(start);
        Ok(true)
    }

    /// Deletes the grapheme cluster before the caret — what a backspace key means.
    ///
    /// Inside the open buffer this is one in-place edit; across sealed text it is a cut, and the
    /// bytes stay in the world-line (OQ-14). Either way the **cluster** is what goes, not a byte:
    /// `e` + U+0301 leaves together, because the caret can only have been after both.
    ///
    /// # Errors
    ///
    /// A read of the leaf the caret is in, or a cut the geometry refuses.
    pub fn backspace(&mut self) -> Result<bool, ShellError> {
        if !self.selection.is_empty() {
            return self.delete_selection();
        }
        let caret = self.typing.caret();
        let Some(start) = self.cluster_before(caret)? else {
            return Ok(false);
        };
        self.typing.delete(start, caret)?;
        self.selection = Selection::caret(start);
        Ok(true)
    }

    /// Deletes the grapheme cluster after the caret — what a delete key means.
    ///
    /// # Errors
    ///
    /// A read of the leaf the caret is in, or a cut the geometry refuses.
    pub fn delete_forward(&mut self) -> Result<bool, ShellError> {
        if !self.selection.is_empty() {
            return self.delete_selection();
        }
        let caret = self.typing.caret();
        let Some(end) = self.cluster_after(caret)? else {
            return Ok(false);
        };
        self.typing.delete(caret, end)?;
        self.selection = Selection::caret(caret);
        Ok(true)
    }

    /// Seals whatever is open, so the document's geometry is final.
    ///
    /// A window would call this on an idle pause; a session that has been typed into and never
    /// sealed has no segment in the world-line, and nothing a reader can fault in.
    ///
    /// # Errors
    ///
    /// As [`Typing::flush`]: a refused append leaves the text in the buffer.
    pub fn flush(&mut self) -> Result<(), ShellError> {
        self.typing.flush()?;
        Ok(())
    }

    /// Selects everything.
    pub fn select_all(&mut self) {
        self.selection = Selection::new(0, self.typing.document_len());
    }

    /// Puts the caret where a click landed, dropping the selection, and reports the caret.
    ///
    /// The point is in **device** space and is resolved by the view transform — the same one paint
    /// uses — so a click can never land on a boundary the frame did not draw.
    ///
    /// # Errors
    ///
    /// A frame the document refused to read.
    pub fn click(&mut self, point: Point) -> Result<Option<Caret>, ShellError> {
        let caret = self.hit_test(point)?;
        let Some(caret) = caret else {
            return Ok(None);
        };
        self.move_caret(caret.byte)?;
        self.selection = Selection::caret(caret.byte);
        Ok(Some(caret))
    }

    /// Extends the selection to where a drag landed, keeping the anchor where it was.
    ///
    /// # Errors
    ///
    /// As [`Self::click`].
    pub fn drag_to(&mut self, point: Point) -> Result<Option<Caret>, ShellError> {
        let caret = self.hit_test(point)?;
        let Some(caret) = caret else {
            return Ok(None);
        };
        let anchor = self.selection.anchor();
        self.move_caret(caret.byte)?;
        self.selection = Selection::new(anchor, caret.byte).clamped(self.typing.document_len());
        Ok(Some(caret))
    }

    /// Closes the current undo step, so the next edit is a step of its own.
    ///
    /// Undo in this system is a stack of **marks**, not of keystrokes: `Typing` records a step when
    /// a caller marks, and returns to the last mark when it undoes. A window therefore marks at the
    /// start of a typing burst — consecutive keystrokes become one undo — and a headless caller
    /// that wants every edit to be its own step marks before each one. Nothing in the loop marks
    /// on its own; see AUDIT D-40 for what that cost when the keymap first used it.
    pub fn mark(&mut self) {
        self.typing.mark();
    }

    /// Undoes the last action, reporting whether there was one.
    ///
    /// # Errors
    ///
    /// As [`Typing::undo`].
    pub fn undo(&mut self) -> Result<bool, ShellError> {
        let undone = self.typing.undo()?;
        if undone {
            self.selection = Selection::caret(self.typing.caret());
        }
        Ok(undone)
    }

    /// Re-applies what undo took back, reporting whether there was anything.
    ///
    /// # Errors
    ///
    /// As [`Typing::redo`].
    pub fn redo(&mut self) -> Result<bool, ShellError> {
        let redone = self.typing.redo()?;
        if redone {
            self.selection = Selection::caret(self.typing.caret());
        }
        Ok(redone)
    }

    /// Scrolls the smallest amount that brings the caret into the viewport, reporting whether it
    /// moved.
    ///
    /// This is a policy, and the smallest one that is honest: a window would call it after every
    /// intent, and a caret off screen is the one scroll the loop must not leave to the user.
    ///
    /// It asks the **document** where the caret is drawn, not the frame. A frame holds the caret
    /// only when it is already on screen — which is exactly the case a scroll is not needed for —
    /// and its caret top is measured from the frame's first line, not from the document's top. Both
    /// mistakes look like a working reveal until the caret is one line above the scroll position or
    /// far below it: the first scrolls to the document's top to show something already visible, the
    /// second does nothing at all.
    ///
    /// # Errors
    ///
    /// A leaf the document could not read, with the same reasons as [`Self::frame`].
    pub fn reveal_caret(&mut self) -> Result<bool, ShellError> {
        let before = self.scroll;
        let Some((top, height)) = self.caret_place()? else {
            return Ok(false);
        };
        let bottom = top + height;
        // D-58: the caret's own line box is evidence the index may not have yet — the leaf holding
        // a just-typed byte is unmeasured until a frame draws it — so the clamp is told the extent
        // the caret establishes rather than trusting a total that is short by exactly that leaf.
        if top < self.scroll {
            self.scroll_to_within(top, bottom);
        } else if bottom > self.scroll + self.viewport {
            self.scroll_to_within(bottom - self.viewport, bottom);
        }
        Ok(self.scroll != before)
    }

    /// Moves the caret, extending the selection when `extend` is set.
    ///
    /// Left and right step one **cluster** through the document — the same clusters backspace and
    /// delete cut — so a mark or a joiner is one move, not two. Up, down, line start and line end
    /// ask the frame's own geometry where the target is: the point is built from the caret's own
    /// line box and handed to the same hit test a click goes through, which is what keeps a
    /// keyboard caret on boundaries the renderer actually drew.
    ///
    /// Reports whether the caret moved; a left at the document's start moves nothing.
    ///
    /// # Errors
    ///
    /// Whatever the frame or the document refuses to read.
    pub fn move_caret_to(&mut self, to: Move, extend: bool) -> Result<bool, ShellError> {
        let caret = self.typing.caret();
        let target = match to {
            Move::Left => self.cluster_before(caret)?.unwrap_or(0),
            Move::Right => self
                .cluster_after(caret)?
                .unwrap_or_else(|| self.typing.document_len()),
            Move::DocumentStart => 0,
            Move::DocumentEnd => self.typing.document_len(),
            Move::Up | Move::Down | Move::LineStart | Move::LineEnd => {
                let Some(point) = self.geometry_point(to)? else {
                    return Ok(false);
                };
                let Some(caret) = self.hit_test(point)? else {
                    return Ok(false);
                };
                caret.byte
            }
        };
        let before = self.typing.caret();
        self.place_caret(target, extend)?;
        Ok(self.typing.caret() != before)
    }

    /// The device point a geometry move asks about, with the caret brought on screen first.
    fn geometry_point(&mut self, to: Move) -> Result<Option<Point>, ShellError> {
        let view = self.view;
        for attempt in 0..2 {
            let point = {
                let frame = self.frame()?;
                frame.caret().map(|caret| {
                    let y = caret.top + Fixed::ONE;
                    let point = match to {
                        Move::Up => Point::new(caret.x, caret.top - Fixed::ONE),
                        Move::Down => Point::new(caret.x, caret.top + caret.height + Fixed::ONE),
                        Move::LineStart => Point::new(Fixed::ZERO, y),
                        _ => {
                            let past = frame.layout().width() + frame.layout().width();
                            Point::new(caret.x + past, y)
                        }
                    };
                    Point::new(view.to_device(point.x), view.to_device(point.y))
                })
            };
            if point.is_some() {
                return Ok(point);
            }
            if attempt == 0 {
                self.reveal_caret()?;
            }
        }
        Ok(None)
    }

    /// Puts the caret at `at`, keeping or moving the selection's anchor.
    fn place_caret(&mut self, at: u64, extend: bool) -> Result<(), ShellError> {
        let anchor = self.selection.anchor();
        let at = at.min(self.typing.document_len());
        self.move_caret(at)?;
        self.selection = if extend {
            Selection::new(anchor, at).clamped(self.typing.document_len())
        } else {
            Selection::caret(at)
        };
        Ok(())
    }

    /// The caret's line box in **document** coordinates, whether or not it is on screen.
    ///
    /// The leaf the caret falls in is laid out from the carry its predecessor handed it — the same
    /// state [`Self::reflow`] carried — so the caret's y is the document's y. A caret at a soft
    /// wrap belongs to two lines at once; this answers with the line the caret starts on, which is
    /// the line a paint of the same byte would choose, and falls back to the previous cluster's
    /// leaf only when the caret's own leaf does not draw it.
    fn caret_place(&mut self) -> Result<Option<(Fixed, Fixed)>, ShellError> {
        // The index is brought in step with the spine **here**, not only in `frame`. This reads
        // `measured.heights.prefix(leaf)` with a leaf number that comes from the spine, and between
        // a keystroke and the frame that follows it the two disagree by design: a seal or an append
        // changed `slab_count` and nothing has measured the new leaf yet. `frame` calls this on its
        // own way in, so it always ran after a sync — until the keystroke path called a reveal
        // first (D-53), and then the prefix was asked for a leaf past the end of the model.
        self.sync_measured();
        let caret_at = self.typing.caret();
        for probe in [caret_at, caret_at.saturating_sub(1)] {
            let Some(leaf) = self.leaf_of(probe) else {
                continue;
            };
            let Some((start, end)) = self.leaf_bounds(leaf) else {
                continue;
            };
            let carried = self.carry_of(leaf)?;
            let read = self.read(start, end)?;
            let (text, from) = leaf_text(read.text(), start, carried);
            let layout = layout_styled_continuing(
                StyledText::plain(text),
                from,
                carried.x,
                carried.cluster,
                self.width,
                &self.shaper,
            );
            let base = if carried.x.is_zero() {
                Fixed::ZERO
            } else {
                layout
                    .lines()
                    .first()
                    .map_or(Fixed::ZERO, |line| line.height())
            };
            if let Some(caret) = layout.paint_point(caret_at) {
                return Ok(Some((
                    self.measured.heights.prefix(leaf) + (caret.top - base),
                    caret.height,
                )));
            }
        }
        Ok(None)
    }

    /// The carry `leaf` is measured from.
    ///
    /// D-57: a leaf the frame has not measured has no carry of its own, and `Carry::default()`
    /// says "this leaf begins a line" — true only when it really does. A tail leaf pushed by
    /// `sync_measured` is in exactly that state until the next frame measures it, so anything that
    /// asked before then laid it out from column zero: a reveal scrolled to a line the caret is not
    /// on, and a backspace could cut a `\n` away from the `\r` in front of it. The predecessor's
    /// answer is the leaf's, because that is what the carry chain means.
    fn carry_of(&mut self, leaf: usize) -> Result<Carry, ShellError> {
        let mut at = leaf;
        // The walk is bounded by the leaf count, not by the cluster limit: `Measured::push` can add
        // an arbitrarily long unmeasured run in one sync (a paste that crosses many leaves), and a
        // cap here would answer a *different* leaf's carry instead of reporting that it cannot. The
        // loop ends at leaf 0 at the latest, and `at` strictly decreases.
        while at > 0 && !self.measured.is_measured(at) {
            at -= 1;
        }
        if at == leaf {
            return Ok(self.measured.carry(leaf));
        }
        let mut carried = self.carry_after(at)?;
        // Every leaf after `at` begins where the one before it ended, and it trims nothing: the
        // carry the tail leaf would be measured with is the carry `at` ends at.
        carried.skip = 0;
        Ok(carried)
    }

    /// The carry `leaf` ended at: the state the leaf after it is measured from.
    ///
    /// Measured from the leaf's own carry and its text, because the index stores where a leaf
    /// *begins*, not where it ends — and a reveal on an unmeasured tail leaf needs the latter
    /// (D-57). The answer is this leaf's own carry when the text is empty (nothing here moved the
    /// line), and a fresh line when the text broke one (a break always starts the next).
    fn carry_after(&mut self, leaf: usize) -> Result<Carry, ShellError> {
        let Some((start, end)) = self.leaf_bounds(leaf) else {
            return Ok(Carry::default());
        };
        let carried = self.measured.carry(leaf);
        let read = self.read(start, end)?;
        let (text, _) = leaf_text(read.text(), start, carried);
        if text.is_empty() {
            return Ok(Carry {
                x: carried.x,
                after_cr: false,
                cluster: carried.cluster,
                skip: 0,
            });
        }
        let layout = layout_styled_continuing(
            StyledText::plain(text),
            start,
            carried.x,
            carried.cluster,
            self.width,
            &self.shaper,
        );
        let broken = text.ends_with('\n') || text.ends_with('\r');
        Ok(Carry {
            x: match layout.lines().last() {
                Some(line) if !broken => line.end_x(),
                _ => Fixed::ZERO,
            },
            after_cr: text.ends_with('\r'),
            cluster: layout.carry(),
            // The tail a leaf places for the next one is the look it took past its own end; a
            // reveal has no such look, so the next leaf trims nothing.
            skip: 0,
        })
    }

    /// The leaf whose bytes contain `at`, or the last leaf when `at` is the document's end.
    ///
    /// Leaves are contiguous and ordered, so this is a binary search over the spine, not a scan:
    /// a reveal on a four-thousand-leaf document must not cost a walk of it.
    fn leaf_of(&self, at: u64) -> Option<usize> {
        let count = self.typing.spine().slab_count();
        if count == 0 {
            return None;
        }
        let draft = self.typing.draft();
        let mut low = 0u32;
        let mut high = count;
        while low < high {
            let mid = low + (high - low) / 2;
            match self.typing.spine().slab_span(mid) {
                // `at` is a **live** offset, and a leaf's live extent is the draft's answer, not
                // the slab's: an open leaf holding more bytes than it registered moves every leaf
                // after it, and one holding fewer pulls them left (D-51). Comparing a live offset
                // with sealed geometry sent a caret in the open leaf to the wrong leaf, so
                // `reveal_caret` looked for it in a line that did not hold it.
                Some(span) => {
                    if draft.live_offset(span.offset() + span.len()) <= at {
                        low = mid + 1;
                    } else {
                        high = mid;
                    }
                }
                None => break,
            }
        }
        Some((low.min(count - 1)) as usize)
    }

    /// Measures every leaf, so the whole document can be scrolled before it has been seen.
    ///
    /// Lazy measurement is enough for the screen and not enough for the scroll bar: a viewport can
    /// only be found in territory whose heights are known, and a document nobody has scrolled has
    /// one measured screen. So opening a document runs this once, and it is the one place in the
    /// loop that touches every leaf.
    ///
    /// It does not read the document. Each leaf is faulted in **alone**, laid out at the current
    /// width, and released — so the resident set never holds more than the leaf being measured plus
    /// whatever the last frame left, and the loop cannot decrypt text it is not shaping.
    ///
    /// The leaves are measured **in order, carrying the line across the boundary**: the x where the
    /// next leaf's first cluster sits, and whether the line it lands on began in an earlier leaf.
    /// That carry is the whole difference between a document's geometry and a pile of leaves:
    /// without it a paragraph split across two leaves breaks where the leaf ends, and the total
    /// grows a line per boundary. With it, a leaf's height is the height of the lines that start
    /// in it, the heights sum to the height of one continuous layout of the document, and a frame
    /// can begin at a line instead of at a leaf (AUDIT D-20).
    ///
    /// # Errors
    ///
    /// A leaf the document could not read, with the same reasons as [`Self::frame`].
    pub fn reflow(&mut self) -> Result<(), ShellError> {
        let leaves = self.typing.spine().slab_count();
        self.measured = Measured::new(leaves as usize);
        self.len_synced = self.typing.spine().len();
        let mut carried = Carry::default();
        for leaf in 0..leaves {
            let (height, next) = self.measure_leaf(leaf, carried)?;
            self.measured.set(leaf as usize, height, carried);
            carried = next;
        }
        Ok(())
    }

    /// Measures one leaf, given the carry its predecessor handed it.
    ///
    /// Returns the height of the lines that **start** in the leaf, and the carry to hand to the
    /// next one. This is the whole of the measurement: [`Self::reflow`] calls it for every leaf in
    /// order, and [`Self::bootstrap`] calls it until the screen is full — both hold one faulted
    /// leaf at a time, and neither hands the reader more than that.
    ///
    /// # Errors
    ///
    /// A leaf the document could not read, with the same reasons as [`Self::frame`].
    fn measure_leaf(&mut self, leaf: u32, carried: Carry) -> Result<(Fixed, Carry), ShellError> {
        let Some(span) = self.typing.spine().slab_span(leaf) else {
            return Ok((Fixed::ZERO, carried));
        };
        let (start, end) = (span.offset(), span.offset() + span.len());
        let read = self.read(start, end)?;
        let (text, from) = leaf_text(read.text(), start, carried);
        if text.is_empty() {
            // Every byte of this leaf was the tail of a cluster the leaf before it placed. There is
            // nothing here to shape and nothing to hand on but the position it ends at.
            return Ok((
                Fixed::ZERO,
                Carry {
                    x: carried.x,
                    after_cr: false,
                    cluster: carried.cluster,
                    skip: 0,
                },
            ));
        }
        // A leaf cut is a byte count, so this leaf can end inside a cluster — and then the
        // cluster's advance would be paid on both sides of the cut. The leaf the cluster
        // *starts* in places it whole: it looks a bounded way past its own end for the tail,
        // lays the tail out with the rest, and tells the next leaf to skip those bytes
        // (AUDIT D-25).
        let tail = self.cluster_tail(text, end, carried.cluster)?;
        let padded;
        let text = if tail.is_empty() {
            text
        } else {
            padded = format!("{text}{tail}");
            padded.as_str()
        };
        let layout = layout_styled_continuing(
            StyledText::plain(text),
            from,
            carried.x,
            carried.cluster,
            self.width,
            &self.shaper,
        );
        let lines = layout.lines();
        // The first line is the tail of a line that began in an earlier leaf: it was charged there,
        // and it is not a line of this one.
        let inherited = usize::from(!carried.x.is_zero());
        let last = leaf + 1 == self.typing.spine().slab_count();
        let mut height = Fixed::ZERO;
        for (index, line) in lines.iter().enumerate() {
            if index < inherited {
                continue;
            }
            // A line starting exactly at the leaf's end is the empty line after a trailing break:
            // it belongs to the next leaf — except at the end of the document, where nobody else
            // can claim it and the total would come up a line short.
            if line.start_byte() < end || (last && line.start_byte() == end) {
                height += line.height();
            }
        }
        let broken = text.ends_with('\n') || text.ends_with('\r');
        let next = Carry {
            x: match lines.last() {
                Some(line) if !broken => line.end_x(),
                _ => Fixed::ZERO,
            },
            after_cr: text.ends_with('\r'),
            cluster: layout.carry(),
            skip: tail.len(),
        };
        Ok((height, next))
    }

    /// Builds the frame a window would draw: read the viewport, lay it out, measure it, report it.
    ///
    /// The window **begins at a line**, not at a leaf. The height index finds the leaf a scroll
    /// position falls in, but a line can start inside a leaf — so the leaf is laid out from the
    /// carry its predecessor handed it, and the frame starts at the byte where the position's line
    /// begins. A screen that began at a leaf boundary instead would re-wrap the paragraph from a
    /// point the document never breaks at, and every cluster below it would be drawn somewhere the
    /// document does not put it.
    ///
    /// # Errors
    ///
    /// A window the document could not read — a source the register does not know, a refused
    /// fault-in, or an edge inside a character (OQ-15).
    pub fn frame(&mut self) -> Result<Frame<'_>, ShellError> {
        self.sync_measured();
        self.measured.heights.reset_reads();
        let (start, mut end, leaves, mut provisional) = self.choose_window()?;
        // The open tail is live document. A window that stopped at the sealed spine's end would
        // show nothing of what the user is typing — the bytes are in the draft, one leaf past the
        // last sealed one (AUDIT D-41) — so a window that reaches the sealed end reaches the
        // document's end instead, and the frame is provisional because the leaf holding that tail
        // has no measured height yet.
        let sealed = self.typing.spine().len();
        let document_end = self.typing.document_len();
        if end == sealed && document_end > sealed {
            end = document_end;
            provisional = true;
        }

        // The window ends at a leaf, and a leaf cut is a byte count, so the window can end inside
        // a cluster. A frame draws *and* hit tests, so it has to end where the document has a
        // position: the cluster's tail comes with the window and is painted with it, and the eye
        // is never asked to offer a boundary the document does not have (AUDIT D-26, OQ-19). A
        // cluster longer than the leaf after the cut cannot be finished here; that bound is
        // `legal_caret`'s, and D-25's residual.
        let look = self.leaf_range(end).map_or(end, |(_, next_end)| next_end);
        let read = self.read(start, look)?;
        self.window = Some(read);
        let full: &str = self.window.as_ref().map_or("", |text| text.text());
        let cut = usize::try_from(end.saturating_sub(start)).unwrap_or(0);
        let end = end + tail_bytes(full, cut, ClusterContext::START) as u64;
        let len = usize::try_from(end.saturating_sub(start)).unwrap_or(0);
        let text: &str = full.get(..len).unwrap_or(full);
        let layout = layout_styled(StyledText::plain(text), start, self.width, &self.shaper);

        let spans = self
            .typing
            .draft()
            .spans_in(self.typing.spine(), start, end);
        // A window that begins mid-leaf is handed that leaf **clipped**: `spans_in` returns the
        // overlapping slice, so the head span's offset is the window's own start rather than the
        // leaf's. Its carries describe a layout that begins in the middle of the leaf, so they say
        // nothing about the leaf, and writing them back overwrites the carry the leaf was measured
        // with: the next frame then lays the leaf out from x = 0, its first line starts at its own
        // first byte, and the screen's top line moves for a scroll offset that did not (AUDIT
        // D-39). Only a head that begins on the leaf's own first byte is written back; its heights
        // still are, because the lines it holds start inside it wherever the window began.
        let head_whole = self
            .typing
            .draft()
            .locate(self.typing.spine(), start)
            .is_some_and(|hit| hit.within() == 0);
        let first = leaves.first();
        let measured = measure_window(&layout, text, &spans, start, end, document_end, head_whole);
        for (index, height, carry) in measured {
            // The index has one entry per **sealed** leaf; the open tail's leaf has none until it
            // is sealed, so its measurement is dropped here and taken again next frame rather than
            // being written past the end of the index (AUDIT D-41).
            if first + index >= self.measured.len() {
                continue;
            }
            self.measured.set(first + index, height, carry);
        }

        let reads = self.measured.heights.reads();
        let shaped = layout
            .lines()
            .iter()
            .map(|line| line.clusters().len())
            .sum();
        let caret = layout.paint_point(self.typing.caret());
        let (anchor, focus) = self.selection.normalised();
        let selection = (anchor != focus).then_some((anchor, focus));
        Ok(Frame {
            text,
            origin: start,
            layout,
            leaves,
            reads,
            shaped,
            provisional,
            caret,
            selection,
        })
    }

    /// Keeps the measured geometry the same size as the document's leaf count.
    ///
    /// Three cases, three costs, and the difference is the whole point of the index. A **seal**
    /// replaces one leaf with one leaf, so nothing here runs and typing into a full buffer costs no
    /// geometry at all. A **tail append** — the first byte of a new leaf at the document's end — is
    /// one `push`, O(log n), because every measured height before it is still true. Anything else
    /// (a mid-document split, a cut) moved every offset after it, so the index is emptied: those
    /// heights described a document that no longer exists, and the next frame is provisional.
    fn sync_measured(&mut self) {
        let leaves = self.typing.spine().slab_count() as usize;
        let len = self.typing.spine().len();
        if self.measured.len() == leaves {
            self.len_synced = len;
            return;
        }
        // D-50: an append is only a **tail** append when the new leaf is the document's last one
        // *and* it begins where the document used to end. Counting leaves cannot tell that from a
        // caret moved into sealed text and typed into — both add one leaf — and the index used to
        // take the second for the first: every measured height after the caret stayed on a leaf
        // that had moved, and the odd leaf past the end of the index was never measured at all.
        if leaves == self.measured.len() + 1 {
            let appended_at_tail = self
                .typing
                .spine()
                .slab_span((leaves - 1) as u32)
                .is_some_and(|span| span.offset() == self.len_synced);
            if appended_at_tail {
                self.measured.push();
                self.len_synced = len;
                return;
            }
        }
        self.measured = Measured::new(leaves);
        self.len_synced = len;
    }

    /// The window to shape: its byte range, the leaves the index says are on screen, and whether
    /// the geometry had to be guessed.
    fn choose_window(&mut self) -> Result<(u64, u64, Window, bool), ShellError> {
        if self.measured.is_empty() {
            return Ok((0, 0, Window::empty(), false));
        }
        if self.measured.total() <= Fixed::ZERO {
            return self.bootstrap();
        }
        let Some((leaf, into)) = self.measured.heights.leaf_at(self.scroll) else {
            // Past the end of the document: there is nothing to shape.
            let end = self.typing.document_len();
            return Ok((end, end, Window::empty(), false));
        };
        let window = byte_window(
            self.typing.spine(),
            &self.measured.heights,
            self.scroll,
            self.viewport,
        );
        let start = self.line_start(leaf, into)?;
        Ok((start, window.end(), window.leaves(), false))
    }

    /// Nothing measured at all: measure leaves from the top until the screen is full, and shape
    /// what that covered.
    ///
    /// One faulted leaf at a time, with the carry — so the screen it fills is the document's
    /// screen, not a leaf-by-leaf guess — and it writes what it measured back, which is what makes
    /// the *next* frame non-provisional. The `provisional` flag is true because the caller cannot
    /// know, before this runs, how much of the document is on screen.
    fn bootstrap(&mut self) -> Result<(u64, u64, Window, bool), ShellError> {
        let leaves = self.typing.spine().slab_count();
        let mut carried = Carry::default();
        let mut height = Fixed::ZERO;
        let mut leaf = 0u32;
        while leaf < leaves {
            let (measured, next) = self.measure_leaf(leaf, carried)?;
            self.measured.set(leaf as usize, measured, carried);
            height += measured;
            carried = next;
            leaf += 1;
            if height >= self.viewport {
                break;
            }
        }
        let window = ShapedWindow::leaves_of(self.typing.spine(), 0, leaf);
        Ok((window.start(), window.end(), window.leaves(), true))
    }

    /// The bytes past `end` that complete the cluster a leaf cut opened, or empty if it opened
    /// none.
    ///
    /// The answer is bounded twice: the look is one leaf's worth of bytes, and a cluster that does
    /// not end inside them is not completed at all — the geometry it had before the tail was read
    /// (whose advance was paid on both sides of the cut) stands, which is OQ-18's residual.
    ///
    /// # Errors
    ///
    /// A leaf the document refused to read.
    fn cluster_tail(
        &mut self,
        text: &str,
        end: u64,
        context: ClusterContext,
    ) -> Result<String, ShellError> {
        let document = self.typing.document_len();
        if end >= document {
            return Ok(String::new());
        }
        // Where the leaf's own text leaves the segmenter: a cluster is open at the cut exactly when
        // the byte after it would join what is already there.
        let mut clusters = Clusters::continuing(text, context);
        while clusters.next().is_some() {}
        let at_end = clusters.carry();
        // The look is the leaf after this one, whole: a range's edge has to be a character boundary
        // as well as a byte count, and a leaf boundary is the only one that is both — a seal cuts
        // on a character (AUDIT D-25).
        let Some((_, next_end)) = self.leaf_range(end) else {
            return Ok(String::new());
        };
        let look = self.read(end, next_end.min(document))?;
        let Some(first) = look.text().chars().next() else {
            return Ok(String::new());
        };
        if !at_end.joins(first) {
            return Ok(String::new());
        }
        let mut tail = Clusters::continuing(look.text(), at_end);
        match tail.next() {
            // The cluster ends inside the bytes looked at, or at the document's own end, which is a
            // boundary whatever a cut did: those bytes are the leaf's to place.
            Some((_, cluster))
                if cluster.len() < look.text().len()
                    || end + look.text().len() as u64 >= document =>
            {
                Ok(cluster.to_string())
            }
            _ => Ok(String::new()),
        }
    }

    /// The document offset of the line that begins at or before `into` units into `leaf`.
    ///
    /// The height index knows which *leaf* a scroll position falls in; only the leaf's text knows
    /// where its lines begin, because a line can start inside a leaf. So the leaf is laid out from
    /// the carry its predecessor handed it — the same state [`Self::reflow`] carried — and the line
    /// the position falls in is the answer.
    fn line_start(&mut self, leaf: usize, into: Fixed) -> Result<u64, ShellError> {
        let Some(span) = self.typing.spine().slab_span(leaf as u32) else {
            return Ok(0);
        };
        let (start, end) = (span.offset(), span.offset() + span.len());
        let carried = self.carry_of(leaf)?;
        let read = self.read(start, end)?;
        let (text, from) = leaf_text(read.text(), start, carried);
        let layout = layout_styled_continuing(
            StyledText::plain(text),
            from,
            carried.x,
            carried.cluster,
            self.width,
            &self.shaper,
        );
        let lines = layout.lines();
        let inherited = usize::from(!carried.x.is_zero());
        let base = if inherited == 1 {
            lines.first().map_or(Fixed::ZERO, |line| line.height())
        } else {
            Fixed::ZERO
        };
        let mut at = from;
        for (index, line) in lines.iter().enumerate() {
            if index < inherited {
                continue;
            }
            if line.top() - base <= into {
                at = line.start_byte();
            } else {
                break;
            }
        }
        Ok(at)
    }

    /// Reads `start..end` out of the session, through the document's own reader.
    fn read(&mut self, start: u64, end: u64) -> Result<WindowText, ShellError> {
        let window = ShapedWindow::unclaimed(start, end);
        window_text(
            &mut self.residency,
            self.typing.spine(),
            self.typing.draft(),
            self.typing.sources(),
            window,
            self.typing.pending(),
        )
        .map_err(ShellError::from)
    }

    /// The live byte range of the leaf containing `at`.
    fn leaf_range(&self, at: u64) -> Option<(u64, u64)> {
        let hit = self.typing.draft().locate(self.typing.spine(), at)?;
        let span = hit.span();
        Some((span.offset(), span.offset() + span.len()))
    }

    /// The **live** byte range of leaf `index`, or `None` past the end of the spine.
    ///
    /// Both ends are the live document's, not the spine's. The spine's offsets are sealed and do
    /// not move while a leaf is open, while every leaf after the open one *does* move by the
    /// difference between its live length and its registered one (D-51). A caller that asked this
    /// for "where does leaf 7 start" used to get a sealed offset and hand it to a live lookup,
    /// which is the same mixed arithmetic that put a revealing caret on the wrong line.
    fn leaf_bounds(&self, index: usize) -> Option<(u64, u64)> {
        let span = self.typing.spine().slab_span(index as u32)?;
        let draft = self.typing.draft();
        let start = draft.live_offset(span.offset());
        let end = match self.typing.spine().slab_span(index as u32 + 1) {
            Some(next) => draft.live_offset(next.offset()),
            // The last leaf runs to the document's end, which is where the open leaf's bytes are.
            None => self.typing.document_len(),
        };
        Some((start, end))
    }

    /// Where the cluster before `at` begins, or `None` at the document's start.
    fn cluster_before(&mut self, at: u64) -> Result<Option<u64>, ShellError> {
        if at == 0 {
            return Ok(None);
        }
        let Some(mut leaf) = self.leaf_of(at - 1) else {
            return Ok(None);
        };
        // A leaf is cut on a byte count, not on a cluster boundary: a mark or a joiner can be the
        // first byte of the leaf holding the caret, with its base at the last byte of the one
        // before, and a run of marks can straddle several leaves. The grapheme a backspace removes
        // starts where its *cluster* starts, so the walk steps back over every leaf whose geometry
        // begins at or after the caret — those bytes were placed by the leaf before, and the
        // cluster the caret is in is that leaf's to name (AUDIT D-22, D-25).
        let mut hops = 0;
        loop {
            let Some((leaf_start, _)) = self.leaf_bounds(leaf) else {
                return Ok(None);
            };
            let carried = self.carry_of(leaf)?;
            let from = leaf_start + carried.skip as u64;
            if from >= at {
                if leaf == 0 || hops >= CLUSTER_LEAF_LIMIT {
                    return Ok(None);
                }
                leaf -= 1;
                hops += 1;
                continue;
            }
            let read = self.read(from, at)?;
            let text = read.text();
            let clusters = Clusters::continuing(text, carried.cluster);
            let Some((offset, _)) = clusters.last() else {
                return Ok(None);
            };
            let opens_inside = offset == 0
                && (carried.skip > 0
                    || text
                        .chars()
                        .next()
                        .is_some_and(|first| carried.cluster.joins(first)));
            if opens_inside && leaf > 0 && hops < CLUSTER_LEAF_LIMIT {
                leaf -= 1;
                hops += 1;
                continue;
            }
            return Ok(Some(from + offset as u64));
        }
    }

    /// Where the cluster after `at` ends, or `None` at the document's end.
    fn cluster_after(&mut self, at: u64) -> Result<Option<u64>, ShellError> {
        let document = self.typing.document_len();
        if at >= document {
            return Ok(None);
        }
        let Some(mut leaf) = self.leaf_of(at) else {
            return Ok(None);
        };
        // The mirror of `cluster_before`: the cluster that starts at the caret can end in the leaf
        // after it, so the window is widened leaf by leaf until the cluster it opens with ends
        // inside it. A cluster that reaches the document's end is complete there. A leaf's extent
        // includes the bytes the leaf after it skipped — its predecessor placed them (AUDIT D-25).
        let mut hops = 0;
        loop {
            let Some((leaf_start, leaf_end)) = self.leaf_bounds(leaf) else {
                return Ok(None);
            };
            let carried = self.carry_of(leaf)?;
            let from = leaf_start + carried.skip as u64;
            let placed = (leaf_end + self.carry_of(leaf + 1)?.skip as u64).min(document);
            let start = at.max(from);
            if start >= placed {
                if placed >= document || hops >= CLUSTER_LEAF_LIMIT {
                    return Ok(Some(placed));
                }
                leaf += 1;
                hops += 1;
                continue;
            }
            let read = self.read(start, placed)?;
            let mut clusters = Clusters::continuing(read.text(), carried.cluster);
            let Some((_, cluster)) = clusters.next() else {
                return Ok(None);
            };
            let cluster_end = start + cluster.len() as u64;
            if cluster_end < placed || placed >= document || hops >= CLUSTER_LEAF_LIMIT {
                return Ok(Some(cluster_end));
            }
            leaf += 1;
            hops += 1;
        }
    }

    /// Moves the caret, sealing whatever was open — the session does that itself.
    fn move_caret(&mut self, at: u64) -> Result<(), ShellError> {
        let at = at.min(self.typing.document_len());
        self.typing.insert_at(at, b"")?;
        Ok(())
    }

    /// The caret nearest to a device point, through the frame's own layout.
    ///
    /// One boundary a frame offers can be a position the document does not have: the frame ends
    /// where the window ends, the window ends at a leaf, and a leaf cut is a byte count — so the
    /// cut can fall inside a cluster, and the eye, which only ever sees the frame's text, has no
    /// way to know what comes after it (AUDIT D-22, OQ-19). Every *other* boundary is a cluster
    /// start of the frame's own text, and that text begins at a line start, so the end is the only
    /// one that needs asking about — and the answer comes from the document, which is this
    /// editor's to ask.
    fn hit_test(&mut self, point: Point) -> Result<Option<Caret>, ShellError> {
        // The view is read before the frame is built: a frame borrows the editor for as long as it
        // lives, and the transform is a plain value.
        let view = self.view;
        let (caret, end) = {
            let frame = self.frame()?;
            let Some(caret) = view.hit_test(frame.layout(), point) else {
                return Ok(None);
            };
            (caret, frame.origin() + frame.text().len() as u64)
        };
        if caret.byte != end {
            return Ok(Some(caret));
        }
        let at = self.legal_caret(end)?;
        if at == end {
            return Ok(Some(caret));
        }
        // Re-placed in the frame that drew it, so the box a click reports is the box paint draws.
        let frame = self.frame()?;
        Ok(view.paint_point(frame.layout(), at))
    }

    /// `at` if the document has a caret there, or where the cluster containing it begins.
    ///
    /// Only the frame's end needs this (see [`Self::hit_test`]): the window's last cluster can be
    /// cut in two by the leaf the window stops at. The frame normally reaches past the cut and
    /// ends on the cluster (AUDIT D-26), so this guards what it cannot finish — a cluster longer
    /// than the leaf after the cut, which is D-25's residual. The position the document does have
    /// is the one before that cluster, which is on the same line, painted, and a boundary a click
    /// could have landed on had the cut fallen one byte earlier.
    ///
    /// # Errors
    ///
    /// A leaf the document refused to read.
    fn legal_caret(&mut self, at: u64) -> Result<u64, ShellError> {
        if at == 0 || at >= self.typing.document_len() {
            // The document's own start and end are boundaries of it, whatever a leaf did.
            return Ok(at);
        }
        let Some(start) = self.cluster_before(at)? else {
            return Ok(at);
        };
        match self.cluster_after(start)? {
            // The cluster that contains `at - 1` also contains `at`: the cut is inside a grapheme.
            Some(cluster_end) if cluster_end > at => Ok(start),
            _ => Ok(at),
        }
    }
}

/// What one leaf hands the next when the document is measured leaf by leaf.
///
/// A leaf cut is a byte count, not a line or a cluster boundary, so a leaf's layout is only
/// meaningful together with this: where its predecessor's last line had got to, whether that line
/// ended on a bare carriage return, the segmentation state where its own geometry begins, and how
/// many of its first bytes the predecessor already placed.
#[derive(Debug, Clone, Copy, Default)]
struct Carry {
    /// How wide the predecessor's last line was when it ended; zero means the leaf begins a line.
    x: Fixed,
    /// The predecessor's last byte is a CR, so a LF at the first byte of this leaf is the second
    /// half of **one** break rather than a line of its own.
    after_cr: bool,
    /// The UAX #29 state where this leaf's geometry begins — after `skip` bytes, so a leaf that
    /// begins inside a cluster is told the state the document had there (AUDIT OQ-17, D-25).
    cluster: ClusterContext,
    /// Bytes at the leaf's start the predecessor already placed, because they were the tail of a
    /// cluster that began there. One cluster, one advance, at the leaf that starts it (AUDIT D-25).
    skip: usize,
}

/// The measured geometry of the document: one height per leaf, and the carry each leaf was measured
/// with.
///
/// The two travel together because they are invalidated by the same things — an edit or a width
/// change makes both describe a document that no longer exists — and because a leaf is measured
/// with both at once. The height is the height of the **lines that start** in the leaf; the carry
/// is what lets a leaf be laid out as the continuation of the line before it, which is what makes a
/// frame begin at a line rather than at a leaf (AUDIT D-20).
#[derive(Debug)]
struct Measured {
    heights: HeightModel,
    /// The carry each leaf was **measured with**, or `None` while it has not been measured.
    ///
    /// Presence is the record, not the height: a leaf whose bytes its predecessor all placed
    /// measures zero height and is still measured, and telling the two apart is what D-57 needs.
    carries: Vec<Option<Carry>>,
}

impl Measured {
    fn new(leaves: usize) -> Self {
        Self {
            heights: HeightModel::new(leaves),
            carries: vec![None; leaves],
        }
    }

    fn len(&self) -> usize {
        self.heights.len()
    }

    fn is_empty(&self) -> bool {
        self.heights.is_empty()
    }

    fn total(&self) -> Fixed {
        self.heights.total()
    }

    /// The carry `leaf` was measured with; the document's own start for an unmeasured leaf.
    fn carry(&self, leaf: usize) -> Carry {
        self.carries
            .get(leaf)
            .copied()
            .flatten()
            .unwrap_or_default()
    }

    /// Whether `leaf` has been measured — which a pushed tail leaf has not, until `frame` draws
    /// it.
    ///
    /// A caller that lays such a leaf out before then — a reveal asked for between the keystroke
    /// and the frame — would otherwise begin it at the line start `Carry::default()` describes,
    /// and scroll to a line the caret is not on (D-57).
    fn is_measured(&self, leaf: usize) -> bool {
        matches!(self.carries.get(leaf), Some(Some(_)))
    }

    /// Records what one leaf measured.
    fn set(&mut self, leaf: usize, height: Fixed, carry: Carry) {
        let Some(slot) = self.carries.get_mut(leaf) else {
            return;
        };
        *slot = Some(carry);
        self.heights.set(leaf, height);
    }

    /// Appends a leaf nothing has measured, O(log n).
    fn push(&mut self) {
        self.heights.push(Fixed::ZERO);
        self.carries.push(None);
    }
}

/// A leaf's text as its layout must see it, and the document offset that text starts at.
///
/// Two things are trimmed. Bytes the predecessor already placed, because they were the tail of a
/// cluster that started in the leaf before: placing them again is what made one grapheme cost two
/// advances (AUDIT OQ-18, D-25). And a LF at the start of a leaf whose predecessor ended on a CR:
/// the eye treats `\r\n` as one break, and a leaf cut can fall between the two bytes — a cut is on
/// byte counts, not on line boundaries. Laid out on its own the LF starts a line the document does
/// not have, which is a phantom line in the scroll geometry and a blank line on screen.
fn leaf_text(text: &str, start: u64, carry: Carry) -> (&str, u64) {
    let (text, start) = match text.get(carry.skip..) {
        Some(rest) => (rest, start + carry.skip as u64),
        None => (text, start),
    };
    match text.strip_prefix('\n') {
        Some(rest) if carry.after_cr => (rest, start + 1),
        _ => (text, start),
    }
}

/// How many bytes past `at` belong to the cluster that straddles it, or zero when one starts there.
///
/// `at` is a byte offset into `text`, which is segmented from `context`. The answer is bounded by
/// `text`'s own end: a cluster that needs more bytes than the caller read is reported as far as the
/// text goes, which is a bound the caller chose, not a claim that the cluster ended (AUDIT D-26).
fn tail_bytes(text: &str, at: usize, context: ClusterContext) -> usize {
    if at == 0 || at >= text.len() {
        return 0;
    }
    Clusters::continuing(text, context)
        .find(|&(start, cluster)| start + cluster.len() > at)
        .map_or(0, |(start, cluster)| start + cluster.len() - at)
}

/// What one window measured, for the leaves it measured **in full**: their heights and the carry
/// each one hands the next, indexed from the window's first leaf.
///
/// A leaf the window began inside is left out, and its measurement stands: the frame has the lines
/// from the line it started at, not the leaf's. A leaf the window ended inside was charged only the
/// lines it could see, so it is left out too. What remains is exact — except that a line continuing
/// past the window's end is charged the height it has here, which is right while every line is the
/// same height and is the one thing a styled document would have to revisit.
fn measure_window(
    layout: &Layout<'_>,
    text: &str,
    spans: &[LiveSpan],
    start: u64,
    end: u64,
    document_end: u64,
    head_whole: bool,
) -> Vec<(usize, Fixed, Carry)> {
    if spans.is_empty() {
        return Vec::new();
    }
    let mut heights = vec![Fixed::ZERO; spans.len()];
    let mut offsets: Vec<Option<Fixed>> = vec![None; spans.len()];
    // The segmentation state each leaf hands the next. The window is already text, so this is one
    // pass over it with no read and no shaping: a leaf that begins inside a cluster is recorded
    // with the state that says so, which is what keeps a later leaf-by-leaf layout of it segmenting
    // as the document does (AUDIT OQ-17).
    let mut context = ClusterContext::default();
    let mut contexts: Vec<ClusterContext> = Vec::with_capacity(spans.len());
    let mut skips: Vec<usize> = Vec::with_capacity(spans.len());
    for (index, span) in spans.iter().enumerate() {
        let low = usize::try_from(span.offset().saturating_sub(start)).unwrap_or(0);
        let high = usize::try_from((span.offset() + span.len()).saturating_sub(start))
            .unwrap_or(0)
            .min(text.len());
        // The carry is the one a leaf is **measured with**: the state where its own geometry
        // begins — after any bytes its predecessor placed — and the count of those bytes. A leaf
        // begins inside a cluster only when the cut opened one, and then the leaf before it placed
        // the tail unless the tail was longer than the look it takes (AUDIT D-24, D-25).
        let mut opens = context;
        let mut skip = 0usize;
        if let Some(slice) = text.get(low..high) {
            let mut clusters = Clusters::continuing(slice, context);
            if let Some((at, cluster)) = clusters.next() {
                let joins_before = at == 0
                    && slice
                        .chars()
                        .next()
                        .is_some_and(|first| context.joins(first));
                // The predecessor can only have placed the tail if it fitted inside the look it
                // takes — the leaf after it, whole — or if the cluster ends at the document's end
                // (AUDIT D-25).
                let look = spans
                    .get(index + 1)
                    .map_or(0, |next| usize::try_from(next.len()).unwrap_or(0));
                let reaches_end = span.offset() + span.len() >= document_end;
                if joins_before && (cluster.len() < look || reaches_end) {
                    skip = cluster.len().min(high - low);
                    opens = clusters.carry();
                }
            }
            while clusters.next().is_some() {}
            context = clusters.carry();
        }
        contexts.push(opens);
        skips.push(skip);
    }
    for line in layout.lines() {
        let at = line.start_byte();
        let found = spans
            .iter()
            .position(|span| at >= span.offset() && at < span.offset() + span.len())
            .or_else(|| {
                // The empty line after a trailing break starts at the document's end, where no
                // leaf covers a byte: the last leaf the window shows owns it.
                (at == document_end && at == end).then(|| spans.len() - 1)
            });
        if let Some(index) = found {
            heights[index] += line.height();
        }
    }
    // The x where each leaf's geometry begins: the first cluster at or after the byte the leaf
    // starts its own layout at. That is not always a cluster inside the span — a leaf whose bytes
    // were all placed by its predecessor begins at the cluster after them (AUDIT D-25).
    let clusters: Vec<(u64, Fixed)> = layout
        .lines()
        .iter()
        .flat_map(|line| line.clusters())
        .map(|cluster| (cluster.byte, cluster.x))
        .collect();
    let end_x = layout
        .lines()
        .last()
        .map_or(Fixed::ZERO, |line| line.end_x());
    for (index, span) in spans.iter().enumerate() {
        let at = span.offset() + skips[index] as u64;
        offsets[index] = clusters
            .iter()
            .find(|(byte, _)| *byte >= at)
            .map(|(_, x)| *x)
            .or_else(|| (at >= end).then_some(end_x));
    }
    spans
        .iter()
        .enumerate()
        // A span at the window's end is clipped to it and left alone; so is the head span, unless
        // it begins on the leaf's own first byte (`head_whole`). See `frame` for why the head's
        // carries are not the leaf's (AUDIT D-39).
        .filter(|(index, span)| {
            (*index > 0 || head_whole)
                && span.offset() >= start
                && span.offset() + span.len() <= end
        })
        .map(|(index, span)| {
            // The **predecessor's** last byte, read out of the window the frame already holds: a CR
            // there is what the leaf before this one hands it, so it is the byte before the leaf's
            // start — the window's own start has none, and it is a line start, so it needs none.
            let before = usize::try_from(span.offset().saturating_sub(start))
                .ok()
                .and_then(|at| at.checked_sub(1))
                .and_then(|at| text.as_bytes().get(at));
            (
                index,
                heights[index],
                Carry {
                    x: offsets[index].unwrap_or(Fixed::ZERO),
                    after_cr: before == Some(&b'\r'),
                    cluster: contexts[index],
                    skip: skips[index],
                },
            )
        })
        .collect()
}
