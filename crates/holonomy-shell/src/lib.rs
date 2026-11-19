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

use holonomy_eye::{
    byte_window, layout_styled, layout_styled_continuing, window_text, Caret, ClusterContext,
    Clusters, Fixed, HeightModel, Layout, Point, ShapedWindow, Shaper, StyledText, View, Window,
    WindowText, WindowTextError,
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
        let measured = Measured::new(typing.spine().slab_count() as usize);
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
        let max = self.measured.total().sixtyfourths() - self.viewport.sixtyfourths();
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
            self.measured = Measured::new(self.typing.spine().slab_count() as usize);
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
        if top < self.scroll {
            self.scroll_to(top);
        } else if bottom > self.scroll + self.viewport {
            self.scroll_to(bottom - self.viewport);
        }
        Ok(self.scroll != before)
    }

    /// The caret's line box in **document** coordinates, whether or not it is on screen.
    ///
    /// The leaf the caret falls in is laid out from the carry its predecessor handed it — the same
    /// state [`Self::reflow`] carried — so the caret's y is the document's y. A caret at a soft
    /// wrap belongs to two lines at once; this answers with the line the caret starts on, which is
    /// the line a paint of the same byte would choose, and falls back to the previous cluster's
    /// leaf only when the caret's own leaf does not draw it.
    fn caret_place(&mut self) -> Result<Option<(Fixed, Fixed)>, ShellError> {
        let caret_at = self.typing.caret();
        for probe in [caret_at, caret_at.saturating_sub(1)] {
            let Some(leaf) = self.leaf_of(probe) else {
                continue;
            };
            let Some(span) = self.typing.spine().slab_span(leaf as u32) else {
                continue;
            };
            let (start, end) = (span.offset(), span.offset() + span.len());
            let carried = self.measured.carry(leaf);
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

    /// The leaf whose bytes contain `at`, or the last leaf when `at` is the document's end.
    ///
    /// Leaves are contiguous and ordered, so this is a binary search over the spine, not a scan:
    /// a reveal on a four-thousand-leaf document must not cost a walk of it.
    fn leaf_of(&self, at: u64) -> Option<usize> {
        let count = self.typing.spine().slab_count();
        if count == 0 {
            return None;
        }
        let mut low = 0u32;
        let mut high = count;
        while low < high {
            let mid = low + (high - low) / 2;
            match self.typing.spine().slab_span(mid) {
                Some(span) if span.offset() + span.len() <= at => low = mid + 1,
                Some(_) => high = mid,
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
        let (start, end, leaves, provisional) = self.choose_window()?;

        let text = self.read(start, end)?;
        self.window = Some(text);
        let text: &str = self.window.as_ref().map_or("", |text| text.text());
        let layout = layout_styled(StyledText::plain(text), start, self.width, &self.shaper);

        let spans = self
            .typing
            .draft()
            .spans_in(self.typing.spine(), start, end);
        let document_end = self.typing.document_len();
        let first = leaves.first();
        let measured = measure_window(&layout, text, &spans, start, end, document_end);
        for (index, height, carry) in measured {
            self.measured.set(first + index, height, carry);
        }

        let reads = self.measured.heights.reads();
        let shaped = layout
            .lines()
            .iter()
            .map(|line| line.clusters().len())
            .sum();
        let caret = layout.paint_point(self.typing.caret());
        Ok(Frame {
            text,
            origin: start,
            layout,
            leaves,
            reads,
            shaped,
            provisional,
            caret,
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
        if self.measured.len() == leaves {
            return;
        }
        if leaves == self.measured.len() + 1 {
            self.measured.push();
            return;
        }
        self.measured = Measured::new(leaves);
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
        let carried = self.measured.carry(leaf);
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
        let mut at = start;
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

    /// The live byte range of leaf `index`, or `None` past the end of the spine.
    ///
    /// The open leaf's length is the draft's, not the registered one, so the range is asked for by
    /// a byte inside it and only falls back to the registered geometry.
    fn leaf_bounds(&self, index: usize) -> Option<(u64, u64)> {
        let span = self.typing.spine().slab_span(index as u32)?;
        let start = span.offset();
        self.leaf_range(start)
            .or(Some((start, span.offset() + span.len())))
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
        // starts where its *cluster* starts, so the walk steps back over every leaf whose first
        // cluster is a continuation of the one before it — which the leaf's carried state says
        // without reading anything.
        let mut hops = 0;
        loop {
            let Some((from, _)) = self.leaf_bounds(leaf) else {
                return Ok(None);
            };
            if from >= at {
                return Ok(None);
            }
            let context = self.measured.carry(leaf).cluster;
            let read = self.read(from, at)?;
            let text = read.text();
            let clusters = Clusters::continuing(text, context);
            let Some((offset, _)) = clusters.last() else {
                return Ok(None);
            };
            let opens_inside = offset == 0
                && text
                    .chars()
                    .next()
                    .is_some_and(|first| context.joins(first));
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
        // inside it. A cluster that reaches the document's end is complete there.
        let mut hops = 0;
        loop {
            let Some((_, end)) = self.leaf_bounds(leaf) else {
                return Ok(None);
            };
            let end = end.min(document);
            if end <= at {
                return Ok(None);
            }
            let context = self.measured.carry(leaf).cluster;
            let read = self.read(at, end)?;
            let mut clusters = Clusters::continuing(read.text(), context);
            let Some((_, cluster)) = clusters.next() else {
                return Ok(None);
            };
            let cluster_end = at + cluster.len() as u64;
            if cluster_end < end || end >= document || hops >= CLUSTER_LEAF_LIMIT {
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
    /// cut in two by the leaf the window stops at. The position the document does have is the one
    /// before that cluster, which is on the same line, painted, and a boundary a click could have
    /// landed on had the cut fallen one byte earlier.
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
/// ended on a bare carriage return, and the segmentation state at the end of the leaf's text.
#[derive(Debug, Clone, Copy, Default)]
struct Carry {
    /// How wide the predecessor's last line was when it ended; zero means the leaf begins a line.
    x: Fixed,
    /// The predecessor's last byte is a CR, so a LF at the first byte of this leaf is the second
    /// half of **one** break rather than a line of its own.
    after_cr: bool,
    /// The UAX #29 state at the end of the predecessor's text: a leaf can begin inside a cluster,
    /// and a piece handed this state segments as the document does (AUDIT OQ-17).
    cluster: ClusterContext,
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
    carries: Vec<Carry>,
}

impl Measured {
    fn new(leaves: usize) -> Self {
        Self {
            heights: HeightModel::new(leaves),
            carries: vec![Carry::default(); leaves],
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
        self.carries.get(leaf).copied().unwrap_or_default()
    }

    /// Records what one leaf measured.
    fn set(&mut self, leaf: usize, height: Fixed, carry: Carry) {
        let Some(slot) = self.carries.get_mut(leaf) else {
            return;
        };
        *slot = carry;
        self.heights.set(leaf, height);
    }

    /// Appends a leaf nothing has measured, O(log n).
    fn push(&mut self) {
        self.heights.push(Fixed::ZERO);
        self.carries.push(Carry::default());
    }
}

/// A leaf's text as its layout must see it, and the document offset that text starts at.
///
/// One thing is trimmed: a LF at the start of a leaf whose predecessor ended on a CR. The eye
/// treats `\r\n` as one break, and a leaf cut can fall between the two bytes — a cut is on byte
/// counts, not on line boundaries. Laid out on its own the LF starts a line the document does not
/// have, which is a phantom line in the scroll geometry and a blank line on screen.
fn leaf_text(text: &str, start: u64, carry: Carry) -> (&str, u64) {
    match text.strip_prefix('\n') {
        Some(rest) if carry.after_cr => (rest, start + 1),
        _ => (text, start),
    }
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
    for span in spans {
        let low = usize::try_from(span.offset().saturating_sub(start)).unwrap_or(0);
        let high = usize::try_from((span.offset() + span.len()).saturating_sub(start))
            .unwrap_or(0)
            .min(text.len());
        if let Some(slice) = text.get(low..high) {
            let mut clusters = Clusters::continuing(slice, context);
            while clusters.next().is_some() {}
            context = clusters.carry();
        }
        contexts.push(context);
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
    for line in layout.lines() {
        for cluster in line.clusters() {
            let found = spans.iter().position(|span| {
                cluster.byte >= span.offset() && cluster.byte < span.offset() + span.len()
            });
            if let Some(index) = found {
                if offsets[index].is_none() {
                    offsets[index] = Some(cluster.x);
                }
            }
        }
    }
    spans
        .iter()
        .enumerate()
        .filter(|(_, span)| span.offset() >= start && span.offset() + span.len() <= end)
        .map(|(index, span)| {
            // The **predecessor's** last byte, read out of the window the frame already holds: a CR
            // there is what the leaf before this one hands it, so it is the byte before the leaf's
            // start — the window's own start has none, and it is a line start, so it needs none.
            let within = usize::try_from(span.offset() + span.len() - start).ok();
            let before = within
                .and_then(|at| at.checked_sub(1))
                .and_then(|at| text.as_bytes().get(at));
            (
                index,
                heights[index],
                Carry {
                    x: offsets[index].unwrap_or(Fixed::ZERO),
                    after_cr: before == Some(&b'\r'),
                    cluster: contexts[index],
                },
            )
        })
        .collect()
}
