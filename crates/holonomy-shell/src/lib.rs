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
//! no earlier layer could — it writes what the layout measured back into the index. Two
//! consequences are worth stating rather than discovering:
//!
//! * A leaf's height is the height of the **lines that start in it**. That partitions the document
//!   exactly (every line starts once, so the heights sum to the document's height to the 64th) and
//!   it is the only attribution that stays exact when a line straddles two leaves. The cost is
//!   that a line spilling into the next leaf is charged to the leaf it began in, so a scroll
//!   position resolves to the leaf where its line begins — which is the leaf you need to shape.
//! * A **width change is a new geometry.** Every measured height is a claim about the old wrap
//!   width, so [`Editor::resize`] empties the index rather than adjusting it, and the next frame is
//!   *provisional*: with nothing measured there is nothing to scroll by either, so it starts at the
//!   top and widens leaf by leaf until the screen is full. [`Frame::provisional`] says so.
//!
//! # The caret is the document's, the cluster is the Eye's
//!
//! Editing deletes clusters, not bytes and not characters: `backspace` asks the document for the
//! bytes of the leaf the caret is in, asks the Eye's segmentation where the last cluster begins,
//! and cuts there. The two layers agree because the caret only ever lands on a boundary the Eye
//! produced — which is the check OQ-15 said was missing, and which `tests/editing.rs` now performs
//! over every caret a frame offers.

use holonomy_eye::{
    byte_window, layout_styled, window_text, Caret, Clusters, Fixed, HeightModel, Layout, Point,
    Shaper, ShapedWindow, StyledText, View, Window, WindowText, WindowTextError,
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
}

/// The editor loop: a session, a resident set, a height index, a view and a selection.
///
/// One frame is a pure function of those five and the shaper; an intent is the smallest edit to one
/// of them that the user asked for. Nothing here holds a window's state — the shell's job is to be
/// the thing a window *calls*, not the thing it *is*.
pub struct Editor<S, R, H> {
    typing: Typing<S>,
    residency: ResidentSet<R>,
    heights: HeightModel,
    shaper: H,
    window: Option<WindowText>,
    width: Fixed,
    viewport: Fixed,
    scroll: Fixed,
    view: View,
    selection: Selection,
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
        let heights = HeightModel::new(typing.spine().slab_count() as usize);
        Self {
            typing,
            residency,
            heights,
            shaper,
            window: None,
            width,
            viewport,
            scroll: Fixed::ZERO,
            view: View::IDENTITY,
            selection: Selection::caret(0),
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
        &self.heights
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
        let max = self.heights.total().sixtyfourths() - self.viewport.sixtyfourths();
        let max = if max < 0 { Fixed::ZERO } else { Fixed::from_64ths(max) };
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
            self.heights = HeightModel::new(self.heights.len());
            self.scroll = Fixed::ZERO;
        }
        self.width = width;
        self.viewport = viewport;
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
    /// intent, and a caret off screen is the one scroll the loop must not leave to the user. It
    /// needs a frame, so it is a call of its own rather than something [`Self::type_text`] hides
    /// inside — the caller decides whether the caret is worth a repaint.
    ///
    /// # Errors
    ///
    /// A frame the document refused to read.
    pub fn reveal_caret(&mut self) -> Result<bool, ShellError> {
        let before = self.scroll;
        let (top, bottom) = {
            let frame = self.frame()?;
            match frame.caret() {
                Some(caret) => (caret.top, caret.top + caret.height),
                None => return Ok(false),
            }
        };
        if top < self.scroll {
            self.scroll_to(top);
        } else if bottom > self.scroll + self.viewport {
            self.scroll_to(bottom - self.viewport);
        }
        Ok(self.scroll != before)
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
    /// whatever the last frame left, and the loop cannot decrypt text it is not shaping. The cost
    /// is one fault and one layout per leaf, once, and the convention that follows from it: a
    /// leaf's height is the height of its own layout, so a paragraph that would have continued
    /// across a leaf boundary is charged the line it breaks there (AUDIT OQ-16). A frame that
    /// covers the boundary corrects those leaves to the continuous measurement.
    ///
    /// # Errors
    ///
    /// A leaf the document could not read, with the same reasons as [`Self::frame`].
    pub fn reflow(&mut self) -> Result<(), ShellError> {
        self.heights = HeightModel::new(self.typing.spine().slab_count() as usize);
        for leaf in 0..self.typing.spine().slab_count() {
            let Some(span) = self.typing.spine().slab_span(leaf) else {
                continue;
            };
            let (start, end) = (span.offset(), span.offset() + span.len());
            let read = self.read(start, end)?;
            let layout =
                layout_styled(StyledText::plain(read.text()), start, self.width, &self.shaper);
            self.heights.set(leaf as usize, layout.height());
        }
        Ok(())
    }

    /// Builds the frame a window would draw: read the viewport, lay it out, measure it, report it.
    ///
    /// # Errors
    ///
    /// A window the document could not read — a source the register does not know, a refused
    /// fault-in, or an edge inside a character (OQ-15).
    pub fn frame(&mut self) -> Result<Frame<'_>, ShellError> {
        self.sync_heights();
        self.heights.reset_reads();
        let (window, provisional) = self.choose_window()?;

        let text = self.read(window.start(), window.end())?;
        self.window = Some(text);
        let origin = window.start();
        let text: &str = self.window.as_ref().map_or("", |text| text.text());
        let layout = layout_styled(StyledText::plain(text), origin, self.width, &self.shaper);

        let spans = self
            .typing
            .draft()
            .spans_in(self.typing.spine(), window.start(), window.end());
        for (leaf, height) in leaf_heights(&layout, &spans, window) {
            self.heights.set(leaf, height);
        }

        let reads = self.heights.reads();
        let shaped = layout.lines().iter().map(|line| line.clusters().len()).sum();
        let caret = layout.paint_point(self.typing.caret());
        Ok(Frame {
            text,
            origin,
            layout,
            leaves: window.leaves(),
            reads,
            shaped,
            provisional,
            caret,
        })
    }

    /// Keeps the height index the same size as the document's leaf count.
    ///
    /// Three cases, three costs, and the difference is the whole point of the index. A **seal**
    /// replaces one leaf with one leaf, so nothing here runs and typing into a full buffer costs no
    /// geometry at all. A **tail append** — the first byte of a new leaf at the document's end — is
    /// one `push`, O(log n), because every measured height before it is still true. Anything else
    /// (a mid-document split, a cut) moved every offset after it, so the index is emptied: those
    /// heights described a document that no longer exists, and the next frame is provisional.
    fn sync_heights(&mut self) {
        let leaves = self.typing.spine().slab_count() as usize;
        if self.heights.len() == leaves {
            return;
        }
        if leaves == self.heights.len() + 1 {
            self.heights.push(Fixed::ZERO);
            return;
        }
        self.heights = HeightModel::new(leaves);
    }

    /// The window to shape, and whether it had to be guessed.
    fn choose_window(&mut self) -> Result<(ShapedWindow, bool), ShellError> {
        if self.heights.is_empty() {
            return Ok((ShapedWindow::unclaimed(0, 0), false));
        }
        let window = byte_window(
            self.typing.spine(),
            &self.heights,
            self.scroll,
            self.viewport,
        );
        if !window.is_empty() || self.heights.total() > Fixed::ZERO {
            // Either something is measured and on screen, or the viewport is past the end.
            return Ok((window, false));
        }
        // Nothing measured at all: there is no geometry to scroll by, so the frame starts at the
        // document's first leaf and widens until the screen is full — "widen the window" is what
        // an unmeasured window means.
        let mut leaf = 0u32;
        loop {
            let Some(span) = self.typing.spine().slab_span(leaf) else {
                let count = self.typing.spine().slab_count();
                let window = ShapedWindow::leaves_of(self.typing.spine(), 0, count);
                return Ok((window, true));
            };
            let end = span.offset() + span.len();
            let read = self.read(0, end)?;
            let layout = layout_styled(StyledText::plain(read.text()), 0, self.width, &self.shaper);
            if layout.height() >= self.viewport {
                let count = self.typing.spine().slab_count();
                let window = ShapedWindow::leaves_of(self.typing.spine(), 0, (leaf + 1).min(count));
                return Ok((window, true));
            }
            leaf += 1;
        }
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

    /// Where the cluster before `at` begins, or `None` at the document's start.
    fn cluster_before(&mut self, at: u64) -> Result<Option<u64>, ShellError> {
        if at == 0 {
            return Ok(None);
        }
        let (from, _) = match self.leaf_range(at - 1) {
            Some(range) => range,
            None => (0, at),
        };
        let from = from.min(at);
        if from == at {
            return Ok(None);
        }
        let read = self.read(from, at)?;
        let Some((offset, _)) = Clusters::new(read.text()).last() else {
            return Ok(None);
        };
        Ok(Some(from + offset as u64))
    }

    /// Where the cluster after `at` ends, or `None` at the document's end.
    fn cluster_after(&mut self, at: u64) -> Result<Option<u64>, ShellError> {
        let document = self.typing.document_len();
        if at >= document {
            return Ok(None);
        }
        let (start, end) = match self.leaf_range(at) {
            Some((start, end)) => (start, end.min(document)),
            None => (at, document),
        };
        let from = at.max(start);
        if from >= end {
            return Ok(None);
        }
        let read = self.read(from, end)?;
        let Some((_, cluster)) = Clusters::new(read.text()).next() else {
            return Ok(None);
        };
        Ok(Some(from + cluster.len() as u64))
    }

    /// Moves the caret, sealing whatever was open — the session does that itself.
    fn move_caret(&mut self, at: u64) -> Result<(), ShellError> {
        let at = at.min(self.typing.document_len());
        self.typing.insert_at(at, b"")?;
        Ok(())
    }

    /// The caret nearest to a device point, through the frame's own layout.
    fn hit_test(&mut self, point: Point) -> Result<Option<Caret>, ShellError> {
        let frame = self.frame()?;
        Ok(self.view.hit_test(frame.layout(), point))
    }
}

/// Which leaves each line's height belongs to.
///
/// A line is charged to the leaf its first byte falls in, which partitions the layout exactly:
/// every line starts once, so the heights sum to the layout's height to the 64th. A line straddling
/// two leaves therefore belongs to the leaf it began in, and the document coordinate of a line's
/// start is the coordinate a scroll position resolves to.
///
/// The last line may start exactly at the window's end — the empty line after a trailing newline —
/// so the lookup clamps into the window rather than dropping it, or the sum would not be exact.
fn leaf_heights(
    layout: &Layout<'_>,
    spans: &[LiveSpan],
    window: ShapedWindow,
) -> Vec<(usize, Fixed)> {
    if window.leaves().is_empty() || spans.len() != window.leaves().len() {
        return Vec::new();
    }
    let mut heights = vec![Fixed::ZERO; spans.len()];
    let end = window.end();
    for line in layout.lines() {
        let at = line.start_byte().min(end.saturating_sub(1));
        let found = spans
            .iter()
            .position(|span| at >= span.offset() && at < span.offset() + span.len());
        if let Some(index) = found {
            heights[index] += line.height();
        }
    }
    heights
        .into_iter()
        .enumerate()
        .map(|(index, height)| (window.leaves().first() + index, height))
        .collect()
}
