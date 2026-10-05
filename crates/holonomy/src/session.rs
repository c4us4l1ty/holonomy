//! The session: an [`Editor`], a [`Chrome`], a [`Scanout`], an [`InputSource`], and the loop that
//! connects them.
//!
//! # Why this is a library and not just a `main`
//!
//! **The jail cannot be booted from a `#[test]`.** `unshare(CLONE_NEWUSER)` returns `EINVAL` in any
//! multi-threaded process, and libtest always spawns a thread. So the boot chain has to be entered
//! from a single-threaded `main`, which is [`crate::args`]'s and `main.rs`'s job.
//!
//! That split would normally mean the interesting half of the program is untestable. So the *loop*
//! lives here and takes its input source and its sinks as parameters: the integration gate drives
//! [`Session::run`] in-process with a [`ScriptedInputSource`] and pre-opened files, and `main.rs`
//! drives the identical code with an [`EvdevSource`] and descriptors opened during boot. The only
//! thing the gate does not cover is the `unshare` itself, which is covered by
//! `holonomy-jail`'s own tests.
//!
//! # The loop's shape
//!
//! ```text
//!   for each event from the source:
//!       dispatch to a Command          (code-based keymap; hotkeys first)
//!       apply the Command              (editor + geometry)
//!       repaint what the edit damaged  (one line, or one cell for a caret flip)
//!   then: export, commit, or tear down
//! ```
//!
//! Two properties the shape exists for:
//!
//! * **The repaint is damage-limited.** An edit damages its line's box; a caret flip damages one
//!   cell. Everything else on screen keeps the pixels it already had. FR-3.4.
//! * **A frame with no damage is not painted.** The loop asks the [`Blink`] for damage and skips
//!   the paint when there is none, which is what makes a 60fps loop cost nothing between blinks.
//!
//! # Sinks are pre-opened, always
//!
//! [`Session::export`] writes to a [`std::fs::File`] the *caller* opened. The session never opens
//! anything, because inside the jail there is no `open` -- see
//! [`holonomy_jail::seccomp::table::ALLOWLIST`]. An exporter that reached for a path would work in
//! every test and fail as a `SIGSYS` and an exit 137 the first time it ran sealed.

use std::fs::File;
use std::io::Write;

use holonomy_display::paint::Painter;
use holonomy_display::{Frame, FrameError, Scanout};
use holonomy_export::{Format, Report};
use holonomy_input::{Command, Hotkey, InputSource, Keymap, ModifierState};
use holonomy_render::chrome::{Blink, Caret, Chrome, ChromeMetrics, ChromeState};
use holonomy_render::table::TableGrid;
use holonomy_render::DamageRect;
use holonomy_render::{Node, SurfaceTree, TextRun};
use holonomy_text::{Editor, EditorError, SpanPolicy, STYLE_BOLD};
use holonomy_text::{Nav, ResolvedTable, TableCursor, TableSpan};

/// Why the session stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exit {
    /// The input stream ended and the caller asked to keep going.
    StreamEnded,
    /// Ctrl+Q.
    Quit,
}

/// Why a session operation failed.
#[derive(Debug)]
pub enum SessionError {
    /// The document refused the edit.
    Editor(EditorError),
    /// A frame or scanout operation failed.
    Display(holonomy_display::FrameError),
    /// Something asked for the cell the caret is in, and the caret is not in a table.
    ///
    /// Its own variant rather than a `TableError`, because the answer is not "the table is malformed"
    /// -- it is "there is no table here", which is a question about the caret and not about a span.
    NotInTable {
        /// Where the caret actually is.
        offset: u32,
    },
    /// A table's structure did not match the bytes it claims.
    Table(holonomy_text::TableError),
    /// An export failed.
    Export(holonomy_export::ExportError),
    /// A sink refused the write.
    Sink(std::io::Error),
}

impl std::fmt::Display for SessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Editor(e) => write!(f, "{e}"),
            Self::Display(e) => write!(f, "{e}"),
            Self::Export(e) => write!(f, "{e}"),
            Self::Sink(e) => write!(f, "{e}"),
            Self::NotInTable { offset } => write!(
                f,
                "the caret at byte {offset} is not inside a table, so there is no cell to act on"
            ),
            Self::Table(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SessionError {}

impl From<holonomy_text::TableError> for SessionError {
    fn from(e: holonomy_text::TableError) -> Self {
        Self::Table(e)
    }
}

impl From<holonomy_text::TableMapError> for SessionError {
    fn from(e: holonomy_text::TableMapError) -> Self {
        match e {
            holonomy_text::TableMapError::Table(t) => Self::Table(t),
            holonomy_text::TableMapError::Editor(ed) => Self::Editor(ed),
            // An overlap is a bug in the caller's span, and it is reported as one rather than as a
            // `TableError` the caller did not produce.
            holonomy_text::TableMapError::Overlap { start, after } => {
                Self::Table(holonomy_text::TableError::StructureMismatch {
                    expected: after,
                    found: start,
                })
            }
        }
    }
}

impl From<EditorError> for SessionError {
    fn from(e: EditorError) -> Self {
        Self::Editor(e)
    }
}
impl From<holonomy_display::FrameError> for SessionError {
    fn from(e: holonomy_display::FrameError) -> Self {
        Self::Display(e)
    }
}
impl From<holonomy_export::ExportError> for SessionError {
    fn from(e: holonomy_export::ExportError) -> Self {
        Self::Export(e)
    }
}
impl From<std::io::Error> for SessionError {
    fn from(e: std::io::Error) -> Self {
        Self::Sink(e)
    }
}
impl From<holonomy_input::InputError> for SessionError {
    fn from(e: holonomy_input::InputError) -> Self {
        // `Read` carries a bare `errno`, so it becomes the `io::Error` that errno describes rather
        // than a new variant saying the same thing twice. `Eof` is *not* an error for a source that
        // runs to exhaustion -- `InputSource::next_event` returns `None` for that -- so the `Eof`
        // arm here only fires when a record was truncated mid-read, which genuinely is a fault and
        // gets `UnexpectedEof`.
        session_io(e)
    }
}

/// What the loop did, for a status line and for the gate's assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SessionStats {
    /// Commands applied, including movement and navigation.
    pub commands: u32,
    /// Edits that changed the document.
    pub edits: u32,
    /// Frames painted.
    pub frames: u32,
    /// Pixels written across all frames.
    pub pixels: u64,
    /// Exports performed.
    pub exports: u32,
    /// `Ctrl+S` presses seen.
    pub saves: u32,
    /// Commands the keymap produced that the session had no action for.
    ///
    /// Counted rather than ignored: a keymap that starts producing commands the session does not
    /// understand is a keymap/session mismatch, and it should be visible rather than a silent no-op.
    pub unhandled: u32,
    /// Tables inserted by `Ctrl+T` or [`Session::insert_table`].
    pub table_inserts: u32,
    /// Cell navigations that actually moved.
    pub table_navs: u32,
    /// Navigations that asked for a cell that does not exist -- Up from the first row, and so on.
    ///
    /// Counted rather than ignored, because a number that is unexpectedly large means the navigation
    /// rules and the table's shape disagree, which is otherwise invisible: the keystroke simply does
    /// nothing.
    pub table_nav_nowhere: u32,
    /// Newlines inserted inside a cell.
    pub table_newlines: u32,
    /// Table cells drawn in the last paint.
    pub table_cells_drawn: u32,
    /// Border runs drawn in the last paint.
    pub table_borders_drawn: u32,
}

/// A pre-opened export sink.
pub struct ExportSink {
    /// The format to write.
    pub format: Format,
    /// Where to write it. Opened by the caller, before the jail.
    pub file: File,
    /// Human-readable name, for logs.
    pub path: String,
}

/// An editor, its chrome, and the loop.
pub struct Session<'a> {
    /// The document.
    pub editor: Editor,
    /// The chrome geometry.
    pub chrome: Chrome,
    /// What the chrome shows.
    pub state: ChromeState,
    /// The framebuffer.
    frame: Frame,
    /// The presentation target.
    ///
    /// A trait object rather than a `HeadlessScanout`, so the same loop can present to the PPM backend,
    /// to a DRM panel, or to the developer window. The cost is one pointer indirection per frame, which
    /// is nothing next to rasterising one, and the gain is that `main.rs` chooses a backend in one place
    /// instead of the session knowing what a panel is.
    ///
    /// `Box<dyn Scanout>` rather than `Box<dyn Scanout + 'a>`, because `Scanout: Any` and `Any` is
    /// `'static`. Every backend here owns its resources and outlives the session, so nothing is lost.
    scanout: Box<dyn Scanout>,
    /// The painter, borrowing the atlas.
    painter: Painter<'a>,
    /// The keymap. Zero-sized and stateless.
    keymap: Keymap,
    /// Modifiers currently held.
    mods: ModifierState,
    /// The caret blink.
    blink: Blink,
    /// Counters.
    pub stats: SessionStats,
    /// Damage accumulated since the last paint.
    damage: DamageRect,
    /// Which table cell the caret is in, if it is in one. Phase 9A.
    ///
    /// `None` means the caret is in ordinary text, which is the only thing that was possible before
    /// 9A and is the case every non-table keystroke takes. `Some` means Tab, Shift+Tab, Enter and the
    /// four arrows are being interpreted by `holonomy_text`'s navigation rules instead of by the
    /// document.
    ///
    /// It is a cursor rather than just the caret offset because the rules are about *cells*, and
    /// "the cell above" cannot be answered from a byte offset without re-walking the separators on
    /// every keystroke. Eight bytes, `Copy`, and the caret offset is recoverable from it.
    active_cell: Option<TableCursor>,
    /// Whether a table's *shape* changed since the last paint, as opposed to its content.
    ///
    /// This is what makes per-cell damage possible. Typing in a cell only changes that cell's pixels,
    /// so the next paint only needs that cell's rectangle; but inserting a table or appending a row
    /// moves every border line, so the whole grid is stale. One flag rather than a comparison of
    /// shapes, because comparing them would mean re-deriving every table twice per keystroke to
    /// discover what one `set` already knows.
    tables_shape_dirty: bool,
    /// Scratch for [`Session::with_table`], so reading a table's bytes does not copy the document.
    ///
    /// Sized to the widest table in the document, recomputed when a table is inserted or a row is
    /// appended. A field rather than a local because a local array passed to `read_into` **escapes**
    /// and is promoted to the heap by the compiler -- which is the same trap
    /// [`holonomy_text::Editor::delete_scratch`] documents, measured rather than assumed: the
    /// allocation then happens once per call instead of once per session, so it is invisible in a rate
    /// and visible in a count.
    table_scratch: Vec<u8>,
    /// Where the caret was last drawn, so it can be erased.
    caret_drawn_at: Option<DamageRect>,
}

impl<'a> Session<'a> {
    /// A session over `editor`, painting through `painter`, presenting to `scanout`.
    pub fn new(
        editor: Editor,
        painter: Painter<'a>,
        scanout: Box<dyn Scanout>,
        metrics: ChromeMetrics,
    ) -> Self {
        let chrome = Chrome::new(metrics);
        let frame = Frame::black(metrics.width, metrics.height);
        let state = ChromeState {
            // The document's first line is on screen at the caret's line.
            scroll_line: 0,
            ..ChromeState::default()
        };
        Self {
            editor,
            chrome,
            state,
            frame,
            scanout,
            painter,
            keymap: Keymap::us(),
            mods: ModifierState::new(),
            blink: Blink::new(Blink::DEFAULT_PERIOD),
            stats: SessionStats::default(),
            damage: DamageRect::EMPTY,
            active_cell: None,
            tables_shape_dirty: false,
            table_scratch: Vec::new(),
            caret_drawn_at: None,
        }
    }

    /// The chrome, for a gate that needs the page's geometry -- the text rectangle, the row pitch --
    /// to check where something was drawn.
    pub fn chrome(&self) -> &holonomy_render::chrome::Chrome {
        &self.chrome
    }

    /// Which table cell the caret is in, if it is.
    ///
    /// Read-only, for the same reason [`Session::damage`] is: a gate needs to ask where the caret is,
    /// and a caller that could *set* it would be able to put the caret somewhere the document's bytes
    /// disagree with.
    pub fn active_cell(&self) -> Option<TableCursor> {
        self.active_cell
    }

    /// The table containing the caret, if it is in one.
    pub fn active_table(&self) -> Option<TableSpan> {
        self.active_cell?;
        self.editor.table_at(self.editor.caret())
    }

    /// The bytes of the cell the caret is in, for a gate to read what was typed into it.
    pub fn active_cell_text(&self) -> Result<Vec<u8>, SessionError> {
        let cur = self.active_cell.ok_or_else(|| SessionError::NotInTable {
            offset: self.editor.caret(),
        })?;
        let span = self
            .editor
            .table_at(self.editor.caret())
            .ok_or(SessionError::NotInTable {
                offset: self.editor.caret(),
            })?;
        let text = self.editor.text()?;
        let t = ResolvedTable::new(span, &text)?;
        let cell = t.cell_at(span.cell_index(cur.row, cur.col))?;
        let start = cell.start_byte + cur.offset_in_cell.min(cell.len());
        Ok(text[start as usize..cell.end_byte as usize].to_vec())
    }

    /// Insert a table of `rows` by `cols` at the caret, and put the caret in cell `(0, 0)`.
    ///
    /// This is `Ctrl+T`, and the dimensions come from the caller rather than being written here: the
    /// keymap says *how many* columns, and the measure -- which only this layer knows -- says how wide
    /// they are. `col_widths_for` does that arithmetic.
    ///
    /// The caret goes to the first cell rather than staying where the insertion started, because a
    /// person who has just asked for a table wants to type into it, and the insertion point is the
    /// table's first *separator*, which is not inside any cell.
    pub fn insert_table(&mut self, rows: u16, cols: u16) -> Result<(), SessionError> {
        let span = self
            .editor
            .insert_table(rows, cols, self.chrome.metrics.columns)?;
        self.stats.table_inserts += 1;
        self.tables_shape_dirty = true;
        self.enter_cell(span, 0, 0, 0)?;
        self.after_edit(0)
    }

    /// The damage accumulated since the last paint: what the next `paint` will touch.
    ///
    /// Read-only. A caller that wants a repaint asks for one with [`Session::repaint_all`]; a caller
    /// that wants to *narrow* the next paint has no business doing it, because the accumulated damage
    /// is the union of every edit's damage and dropping part of it drops a repaint with it.
    pub fn damage(&self) -> DamageRect {
        self.damage
    }

    /// The current frame.
    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    /// The presentation target, for a caller that wants to describe it.
    pub fn scanout(&self) -> &dyn Scanout {
        self.scanout.as_ref()
    }

    /// The presentation target, mutably.
    ///
    /// For a driver that both presents *through* the backend and *reads from* it: the developer window's
    /// events arrive on the same socket its frames go out on, and the session owns the socket. The
    /// driver downcasts this to its own backend type and reads the events between frames.
    pub fn scanout_mut(&mut self) -> &mut dyn Scanout {
        self.scanout.as_mut()
    }

    /// Write the current frame to `sink` as a binary PPM. Returns the bytes written.
    ///
    /// **On the session, not on the scanout**, because the frame is the session's. `HeadlessScanout`
    /// keeps its own copy so a caller can inspect what was presented, and its `dump` reads that copy --
    /// which for a window or a panel does not exist. Writing `self.frame` is the same bytes for every
    /// backend, and it means the gate's visual baseline does not depend on which backend the test chose.
    pub fn dump_ppm<W: Write>(&self, sink: &mut W) -> Result<u64, SessionError> {
        Ok(self.frame.to_ppm(sink)?)
    }

    /// Write the current frame to a pre-opened file, for a sealed session that cannot open anything.
    pub fn dump_ppm_to_file(&self, file: &mut File) -> Result<u64, SessionError> {
        let mut w = std::io::BufWriter::new(file);
        let n = self.dump_ppm(&mut w)?;
        w.flush()?;
        Ok(n)
    }

    /// Change the size of everything, and mark all of it stale.
    ///
    /// # One method, because there are two halves and the order is load-bearing
    ///
    /// The frame is the session's; the target is the backend's; and [`Scanout::present`] refuses a frame
    /// whose size differs from the backend's. So a resize has to change both, and it has to change the
    /// backend *first* -- and this is the only place that knows so. A caller that resized the session
    /// alone would get every subsequent paint refused as a `SizeMismatch`, which is the exact failure
    /// this had before the order lived here.
    ///
    /// Afterwards the two are checked against each other. A backend with a genuinely fixed size -- a DRM
    /// panel -- declines to resize, and then the session must *not* resize either, so the frame and the
    /// panel stay the size they were and the caller gets `false`. That is the answer for a target whose
    /// size is its mode, and it is better than rebuilding a 4 MiB frame that can never be presented.
    ///
    /// What is rebuilt, when it happens: a frame of the new size, `ChromeMetrics` at the new size, and
    /// the `SurfaceTree` the painter walks. The text does not move, the caret does not move, and no edit
    /// is undone -- only the picture of the document changes, and the measure is fixed, so even the line
    /// breaks do not move. See [`ChromeMetrics::for_size`].
    ///
    /// The whole new frame is marked stale, because a resize is one of the two events (the other is an
    /// `Expose`) where diffing damage rectangles is not cheaper than redrawing: the damage from before
    /// the resize describes the *old* geometry, so none of it covers the new pixels.
    ///
    /// A size smaller than the chrome needs is clamped by [`ChromeMetrics::for_size`] rather than
    /// refused: a window dragged to nothing should show the smallest thing it can, not an error.
    ///
    /// Returns whether the size actually changed.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<bool, SessionError> {
        let metrics = self.chrome.metrics.clamp_to(width, height);
        let (w, h) = (metrics.width, metrics.height);
        if (w, h) == (self.frame.width(), self.frame.height()) {
            return Ok(false);
        }
        // The backend first, so that a backend which declines leaves the session untouched.
        if !self.scanout.resize(w, h)? {
            return Ok(false);
        }
        if (self.scanout.width(), self.scanout.height()) != (w, h) {
            // A backend that claims to have resized and then disagrees is worse than one that
            // declined, because every paint from here on fails with a `SizeMismatch` and the reason is
            // not visible anywhere. So it is reported, and the session stays at the old size.
            return Err(SessionError::Display(FrameError::SizeMismatch {
                want: (w, h),
                got: (self.scanout.width(), self.scanout.height()),
            }));
        }
        self.chrome = Chrome::new(metrics);
        self.frame = Frame::black(w, h);
        self.damage = DamageRect::new(0, 0, w, h);
        Ok(true)
    }

    /// Paint everything and present. The first frame has no damage yet, so it is forced.
    pub fn repaint_all(&mut self) -> Result<(), SessionError> {
        let damage = Some(self.chrome.full_damage());
        self.caret_drawn_at = None;
        self.paint(damage)
    }

    /// Run `source` to exhaustion, or until Ctrl+Q.
    ///
    /// Returns why it stopped. Every command is counted, so a stream that produced nothing shows up
    /// as `stats.commands == 0` rather than as silence.
    pub fn run(&mut self, source: &mut dyn InputSource) -> Result<Exit, SessionError> {
        while let Some(event) = source.next_event().map_err(session_io)? {
            if let Some(exit) = self.handle_event(event)? {
                return Ok(exit);
            }
            // The blink may want a repaint even with no input.
            self.tick()?;
        }
        Ok(Exit::StreamEnded)
    }

    /// One pass of the loop's frame work: repaint what the last edit damaged, then blink.
    ///
    /// **Two things can ask for a paint, and conflating them is a real bug.** An edit accumulates
    /// damage in [`Session::damage`]; a blink flip asks for one cell. The first version of this did
    /// only the second, so a keystroke recorded its damaged line and *nothing ever repainted it* --
    /// the editor accepted the text and the screen stayed blank until a blink happened to fire. The
    /// loop has to drain the accumulated damage first, because the blink's damage is a strict subset
    /// of the region that is already stale.
    ///
    /// Public because the window's driver does not have an [`InputSource`]: it reads X11 events and
    /// therefore calls [`Session::handle_event`] and this in turn. The Phase 8 loop is unchanged --
    /// `run` is exactly these two calls.
    pub fn tick(&mut self) -> Result<(), SessionError> {
        if !self.damage.is_empty() {
            // `paint` clears `damage` itself, so this does not need to.
            self.paint(Some(self.damage))?;
        }
        let caret = self.caret_cell();
        // `advance` reports only a transition *to visible*; see `Blink` for why the other
        // direction is free.
        if let Some(damage) = self.blink.advance(caret) {
            self.state.caret_visible = self.blink.visible();
            self.paint(Some(damage))?;
        }
        Ok(())
    }

    /// The caret's cell, if it is on screen.
    fn caret_cell(&self) -> Option<DamageRect> {
        Caret::locate(&self.chrome.layout, &self.chrome.metrics, &self.state).map(|c| c.cell)
    }

    /// Fold one event into the modifier state and dispatch it. Returns `Some` on a global hotkey.
    ///
    /// Public for the same reason as [`Session::tick`]: a driver with its own event source calls this
    /// directly, one event at a time.
    pub fn handle_event(
        &mut self,
        event: holonomy_input::InputEvent,
    ) -> Result<Option<Exit>, SessionError> {
        // The modifier state is folded *before* dispatch, because `Ctrl+Q` is "ctrl goes down" then
        // "Q goes down", and the second is only `Ctrl+Q` once the first has landed.
        let command = self.keymap.dispatch_into(event, &mut self.mods);
        let Some(command) = command else {
            return Ok(None);
        };
        self.stats.commands += 1;
        if let Command::Hotkey(Hotkey::Quit) = command {
            return Ok(Some(Exit::Quit));
        }
        self.apply(command)?;
        self.tick()?;
        Ok(None)
    }

    /// Apply one command, damaging what it touched.
    pub fn apply(&mut self, command: Command) -> Result<(), SessionError> {
        match command {
            Command::Insert(c) => {
                let mut buf = [0u8; 4];
                let bytes = c.encode_utf8(&mut buf).as_bytes().to_vec();
                self.insert(&bytes)?;
            }
            // Enter is a newline in ordinary text and a line break *inside* a cell in a table, and
            // the difference is the whole of what makes a table usable: a newline that ended the row
            // would add a document line, and a cell is not a line.
            Command::Newline => {
                if self.active_cell.is_some() {
                    self.newline_in_cell()?;
                } else {
                    self.insert(b"\n")?;
                }
            }
            // Tab is a literal tab outside a table and cell navigation inside one. Outside, it is four
            // spaces rather than `\t`: a tab character in the document moves the *caret* to the next
            // tab stop while contributing nothing to any column count, so a status bar reporting
            // "Col 9" for four spaces would be describing a tab stop rather than a column.
            Command::Tab => {
                if self.active_cell.is_some() {
                    self.nav(holonomy_text::tab)?;
                } else {
                    self.insert(b"    ")?;
                }
            }
            // Shift+Tab outside a table removes one level of the indentation Tab added, which is what
            // makes the two a pair rather than two unrelated keys.
            Command::ShiftTab => {
                if self.active_cell.is_some() {
                    self.nav(holonomy_text::shift_tab)?;
                } else {
                    self.outdent()?;
                }
            }
            Command::InsertTable { rows, cols } => self.insert_table(rows, cols)?,
            Command::Backspace => self.backspace()?,
            Command::DeleteForward => self.delete_forward()?,
            // Inside a table the four arrows are cell navigation, not caret movement: "down" means
            // the cell below, which for a one-line cell is a jump of a whole row rather than a line.
            // The rules report `Nav::Nowhere` when there is no such cell, and that is swallowed -- a
            // movement that cannot happen is not a document failure.
            Command::Left if self.active_cell.is_some() => self.nav(holonomy_text::left)?,
            Command::Right if self.active_cell.is_some() => self.nav(holonomy_text::right)?,
            Command::Up if self.active_cell.is_some() => self.nav(holonomy_text::up)?,
            Command::Down if self.active_cell.is_some() => self.nav(holonomy_text::down)?,
            Command::Left => self.move_caret(-1)?,
            Command::Right => self.move_caret(1)?,
            Command::Up => self.move_line(-1)?,
            Command::Down => self.move_line(1)?,
            Command::Home => self.caret_to(0)?,
            Command::End => self.caret_to(self.editor.text_len())?,
            Command::PageUp
            | Command::PageDown
            | Command::Escape
            | Command::ZoomIn
            | Command::ZoomOut
            | Command::ZoomReset => {}
            Command::Hotkey(Hotkey::Undo) => self.undo()?,
            Command::Hotkey(Hotkey::Redo) => self.redo()?,
            Command::Hotkey(Hotkey::Save) => {
                // Ctrl+S is a *request*: committing is the container's business, and the session has
                // no container. Counted so the caller can see it happened.
                self.stats.saves += 1;
            }
            Command::Hotkey(Hotkey::InsertTable) => {
                // 3 by 3: the directive's default, and the shape that divides an 80-column measure
                // into three readable columns of 23 with the borders and padding accounted for.
                self.insert_table(3, 3)?;
            }
            Command::Hotkey(Hotkey::DocumentStart) => self.caret_to(0)?,
            Command::Hotkey(Hotkey::DocumentEnd) => self.caret_to(self.editor.text_len())?,
            _ => {
                self.stats.unhandled += 1;
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------------- edits

    fn insert(&mut self, bytes: &[u8]) -> Result<(), SessionError> {
        let at = self.editor.caret();
        // `GrowIntoInsert` so typing at the end of a bold word keeps it bold, which is what a word
        // processor does and what `SpanPolicy`'s own docs argue for.
        self.editor
            .insert_at(at, bytes, SpanPolicy::GrowIntoInsert)?;
        self.after_edit(bytes.len() as u32)
    }

    /// Run `f` with the table the caret is in, or report that there is none.
    ///
    /// A closure rather than a returned `ResolvedTable` because that borrows the document bytes, and
    /// the bytes come from [`Editor::text`], which hands over an owned `Vec`. Returning the resolved
    /// table would mean returning a borrow of a local -- so the text has to stay inside this frame,
    /// and the only way to do that with the borrow checker is to scope it with a closure.
    ///
    /// Every table operation goes through here, and each re-derives the table from the editor rather
    /// than caching one, because a byte typed into a previous cell moves every cell boundary after it.
    /// A cached `ResolvedTable` would be stale after a single keystroke, which is the whole editing
    /// session.
    ///
    /// **Cost, stated rather than hidden:** `Editor::text` copies the whole document, so this
    /// allocates once per table keystroke. That is a real departure from the "no heap allocation while
    /// editing" invariant and it is not yet fixed; `Editor::read_into` reads a byte range into a
    /// caller-supplied buffer without allocating, and the fix is to hold the table's bytes in a
    /// session-owned scratch buffer sized to the widest table. It is not done here because a wrong
    /// buffer size is worse than a measurable allocation.
    fn with_table<R>(
        &mut self,
        f: impl FnOnce(TableSpan, &ResolvedTable<'_>) -> Result<R, SessionError>,
    ) -> Result<R, SessionError> {
        // The span is handed to the closure as well as the resolved table because a caller that has
        // just moved the caret needs the *old* span's `start_byte` to find the table again afterwards,
        // and re-deriving it from the new caret is exactly what `table_at` would refuse to do.
        let caret = self.editor.caret();
        let span = self
            .editor
            .table_at(caret)
            .ok_or(SessionError::NotInTable { offset: caret })?;
        let need = (span.end_byte - span.start_byte) as usize;
        if self.table_scratch.len() < need {
            // Only ever grows, and only when a table is bigger than any seen before. An allocation
            // *per keystroke* was what the first version did, by copying the whole document with
            // `Editor::text` -- which is a departure from the "no heap allocation while editing"
            // invariant, and the reason this exists.
            self.table_scratch.resize(need, 0);
        }
        let got = self
            .editor
            .read_into(span.start_byte as usize, &mut self.table_scratch)?;
        if got < need {
            return Err(SessionError::Table(
                holonomy_text::TableError::InvertedRange {
                    start: span.start_byte,
                    end: span.start_byte + got as u32,
                },
            ));
        }
        let resolved = ResolvedTable::new(span, &self.table_scratch[..got])?;
        f(span, &resolved)
    }

    /// Put the caret into `(row, col)` at `offset_in_cell` of `span`, and start table-editing.
    ///
    /// # Why `span` is a parameter and not looked up from the caret
    ///
    /// The first version resolved the table with [`Session::with_table`], which finds it *from the
    /// caret*. That is the right rule for every keystroke inside a table and the wrong one for the
    /// keystroke that creates it: `Editor::insert_at` leaves the caret one past the inserted bytes,
    /// which is the newline after the table's last row -- outside the table, so the lookup failed with
    /// `NotInTable` for the very operation that had just made one. It is also the wrong rule after an
    /// append, where the caret is about to be somewhere new.
    ///
    /// So the caller passes the span it already has. There is no window in which the table has to be
    /// found by guessing where it is.
    fn enter_cell(
        &mut self,
        span: TableSpan,
        row: u16,
        col: u16,
        offset_in_cell: u32,
    ) -> Result<(), SessionError> {
        let text = self.editor.text()?;
        let t = ResolvedTable::new(span, &text)?;
        let cell = t.cell_at(span.cell_index(row, col))?;
        let offset = offset_in_cell.min(cell.len());
        self.active_cell = Some(TableCursor {
            row,
            col,
            offset_in_cell: offset,
        });
        self.caret_to((cell.start_byte + offset) as usize)
    }

    /// The table containing `offset`, or an error saying there is none.
    fn span_of(&self, offset: u32) -> Result<TableSpan, SessionError> {
        self.editor
            .table_at(offset)
            .ok_or(SessionError::NotInTable { offset })
    }

    /// Run one navigation rule and act on its answer.
    ///
    /// The rule is a function rather than an enum so that "which rule" is a type error rather than a
    /// match arm somebody forgets. Each returns `Result<Nav, TableError>` because the rules have to
    /// read the neighbouring cell to know where "the previous cell's end" is.
    fn nav(
        &mut self,
        rule: fn(&ResolvedTable<'_>, TableCursor) -> Result<Nav, holonomy_text::TableError>,
    ) -> Result<(), SessionError> {
        let Some(cur) = self.active_cell else {
            return Ok(());
        };
        let start = self.editor.caret();
        let (span, dest, append_row) = self.with_table(|span, t| match rule(t, cur)? {
            Nav::Nowhere => Ok((span, None, false)),
            Nav::NewlineInCell => Ok((span, None, false)),
            Nav::Move {
                row,
                col,
                offset_in_cell,
                append_row,
            } => Ok((span, Some((row, col, offset_in_cell)), append_row)),
        })?;
        let _ = start;
        let Some((row, col, offset_in_cell)) = dest else {
            self.stats.table_nav_nowhere += 1;
            return Ok(());
        };
        let span = if append_row {
            // An append is an **edit**: new bytes, an undo action, a new row of cells. The navigation
            // rule only said it wanted one; this is where the separators go in. The rule asked to land
            // in row `row`, which is `span.rows` -- the row the append creates -- so the two are
            // asserted against each other rather than assumed to agree.
            let new_row = self.editor.append_table_row(span)?;
            self.tables_shape_dirty = true;
            debug_assert_eq!(
                new_row, row,
                "the rule asked to land in the row the append created, and the append made row \
                 {new_row}; if these disagree the navigation rule and the edit have drifted apart"
            );
            // The grown span, not the one the rule was handed: `row` is one past the old `rows`, so
            // resolving it against the old span is `OutOfRange`. That was the second version's bug --
            // the first passed the old span through and reported `OutOfRange { row: 3, rows: 3 }` for
            // the keystroke that had just created row 3.
            self.span_of(span.start_byte)?
        } else {
            span
        };
        self.stats.table_navs += 1;
        self.enter_cell(span, row, col, offset_in_cell)
    }

    /// A newline inside the caret's cell, which makes that cell's row taller.
    ///
    /// The row height is derived from the cell's own line count rather than assumed to be one, so a
    /// cell holding three lines is three lines tall and the borders below it move down. That is the
    /// "recompute row height in the Fenwick geometry" of the directive: the Fenwick mapper maps byte
    /// offsets to line indices, and a cell's height is the number of `\n` bytes in it.
    fn newline_in_cell(&mut self) -> Result<(), SessionError> {
        let Some(cur) = self.active_cell else {
            return self.insert(b"\n");
        };
        let cell_start = self.with_table(|_span, t| {
            Ok(t.cell_at(t.span.cell_index(cur.row, cur.col))?.start_byte)
        })?;
        self.insert(b"\n")?;
        // The insertion is inside the table, so the span grew; re-read it and re-enter the cell one
        // byte past the newline that was just inserted.
        // The insertion is inside the table, so the table grew; the span is re-read at the cell's old
        // start, which is the check that would catch a `TableMap` that stopped tracking the edit.
        self.stats.table_newlines += 1;
        self.enter_cell(
            self.span_of(cell_start)?,
            cur.row,
            cur.col,
            cur.offset_in_cell + 1,
        )
    }

    /// Remove one level of indentation, for Shift+Tab outside a table.
    fn outdent(&mut self) -> Result<(), SessionError> {
        const INDENT: &[u8] = b"    ";
        let at = self.editor.caret();
        if at < INDENT.len() as u32 {
            return Ok(());
        }
        let text = self.editor.text()?;
        let start = at as usize - INDENT.len();
        if text.get(start..at as usize) != Some(INDENT) {
            // Nothing to remove. Not an error: Shift+Tab on a line that was never indented is a
            // keystroke with no effect, exactly as it is in any editor.
            self.stats.table_nav_nowhere += 1;
            return Ok(());
        }
        self.editor.delete_at(start as u32, INDENT.len() as u32)?;
        self.after_edit(INDENT.len() as u32)
    }

    fn backspace(&mut self) -> Result<(), SessionError> {
        if self.editor.caret() == 0 {
            return Ok(());
        }
        self.editor.backspace()?;
        self.after_edit(1)
    }

    fn delete_forward(&mut self) -> Result<(), SessionError> {
        if self.editor.caret() as usize >= self.editor.text_len() {
            return Ok(());
        }
        self.editor.delete_forward()?;
        self.after_edit(1)
    }

    fn undo(&mut self) -> Result<(), SessionError> {
        // `NothingToUndo` is a *keymap* condition, not a document failure: pressing undo with
        // nothing to undo should be swallowed, not reported. Same for redo.
        match self.editor.undo() {
            Ok(_) => self.after_edit(0),
            Err(EditorError::NothingToUndo) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn redo(&mut self) -> Result<(), SessionError> {
        match self.editor.redo() {
            Ok(_) => self.after_edit(0),
            Err(EditorError::NothingToRedo) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Move the caret by whole codepoints.
    fn move_caret(&mut self, delta: i64) -> Result<(), SessionError> {
        let at = self.editor.caret() as i64;
        let want = at + delta;
        if !(0..=self.editor.text_len() as i64).contains(&want) {
            // At an end. A movement that cannot happen is not a document failure.
            return Ok(());
        }
        self.caret_to(want as usize)
    }

    /// Move the caret by lines, using the page's own row count to decide scrolling.
    fn move_line(&mut self, delta: i64) -> Result<(), SessionError> {
        let line = (self.state.caret_line as i64 + delta).max(0) as u32;
        // Keep the caret on screen, scrolling the page if it would leave.
        let first = self.state.scroll_line;
        let last = first + self.chrome.layout.rows;
        let scroll = if line < first {
            line
        } else if line >= last {
            line.saturating_sub(self.chrome.layout.rows.saturating_sub(1))
        } else {
            first
        };
        self.state.scroll_line = scroll.min(line);
        self.state.caret_line = line;
        self.damage = self.chrome.full_damage();
        Ok(())
    }

    /// Put the caret at `at`, clamped back to a codepoint boundary.
    fn caret_to(&mut self, at: usize) -> Result<(), SessionError> {
        self.editor.caret_to(at)?;
        // The caret's *column* within its line, for the status bar.
        let line_start = self.line_start(self.editor.caret() as usize);
        self.state.caret_column =
            ((self.editor.caret() as usize).saturating_sub(line_start)) as u32;
        self.state.caret_line = self.line_index(self.editor.caret() as usize);
        Ok(())
    }

    /// The byte offset of the start of the line containing `at`.
    ///
    /// Scans backwards for the last newline. O(line length), which is bounded by the wrap width and
    /// not by the document -- and `Editor` deliberately does not own line geometry, so the session
    /// computes it. See the layering note in `holonomy-input`: a `Command::Up` means "move up" and
    /// nothing about how many bytes that is.
    fn line_start(&self, at: usize) -> usize {
        let text = self.editor.text().unwrap_or_default();
        let at = at.min(text.len());
        text[..at]
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |i| i + 1)
    }

    /// The 0-based line index containing `at`.
    fn line_index(&self, at: usize) -> u32 {
        let text = self.editor.text().unwrap_or_default();
        let at = at.min(text.len());
        text[..at].iter().filter(|&&b| b == b'\n').count() as u32
    }

    /// Record an edit's consequences: counts, status bar, and the damaged line.
    fn after_edit(&mut self, inserted: u32) -> Result<(), SessionError> {
        self.stats.edits += 1;
        self.caret_to(self.editor.caret() as usize)?;
        self.refresh_counts();
        // FR-3.4: an edit invalidates one line's box. The damaged region is the caret's line, and
        // the *old* caret's line if the edit moved it -- both, or a line that got longer would keep
        // a tail of stale pixels.
        let line = self.state.caret_line;
        self.damage = self
            .line_box(line)
            .unwrap_or_else(|| self.chrome.full_damage());
        let _ = inserted;
        Ok(())
    }

    /// The text row box for line `line`, if it is on screen.
    fn line_box(&self, line: u32) -> Option<DamageRect> {
        let m = &self.chrome.metrics;
        let text = self.chrome.layout.text;
        let row = line.checked_sub(self.state.scroll_line)?;
        if row >= self.chrome.layout.rows {
            return None;
        }
        Some(DamageRect::new(
            text.x,
            text.y + row * m.cell_h,
            text.width,
            m.cell_h,
        ))
    }

    /// Recount the words and bytes the status bar shows.
    fn refresh_counts(&mut self) {
        let text = self.editor.text().unwrap_or_default();
        self.state.bytes = text.len() as u32;
        self.state.words = text
            .split(|b| b.is_ascii_whitespace())
            .filter(|w| !w.is_empty())
            .count() as u32;
        self.state.total_lines = text.iter().filter(|&&b| b == b'\n').count().max(1) as u32;
    }

    // ---------------------------------------------------------------- paint

    /// Repaint `damage` and present.
    pub fn paint(&mut self, damage: Option<DamageRect>) -> Result<(), SessionError> {
        // The caret goes *under* the page's text in paint order, so the caret's rect is erased by
        // repainting the page and then redrawn -- which is why the damage includes it whenever it
        // moves.
        // The line-height model is rebuilt from the tables *before* the chrome's tree, because the
        // chrome's tree and `Caret::locate` both read it: a table that pushed the lines below it down
        // but was published afterwards would move the text and leave the caret behind.
        self.publish_line_heights();
        let mut tree = self.chrome.tree(&self.state);
        // Tables are emitted *into* the chrome's tree rather than into a tree of their own, because
        // they have to be painted in the page's coordinate space and clipped by the same damage the
        // chrome uses. A table that lands outside the viewport contributes nothing and is not visited.
        let mut damage = damage;
        self.emit_tables(&mut tree, &mut damage);
        let stats = self.painter.paint(&mut self.frame, &tree, damage)?;
        self.stats.frames += 1;
        self.stats.pixels += stats.pixels;
        // The damage goes to the backend as well as to the rasteriser. For the PPM backend that changes
        // nothing -- `present_damage` defaults to the whole frame -- and for a backend on a socket it is
        // the difference between 4 MiB per keystroke and 92 KiB.
        self.scanout.present_damage(&self.frame, damage)?;
        self.damage = DamageRect::EMPTY;
        Ok(())
    }

    /// Rebuild the chrome's line-height model from the tables, and park it in the state.
    ///
    /// Each table contributes `(anchor_line, its visual height in pixels)`; `LineHeights::from` turns
    /// that into a sparse `(line, extra)` list by subtracting the line slots the table was already
    /// using, so a table that happens to be exactly as tall as the rows it replaces contributes
    /// nothing and every line below stays put.
    ///
    /// This is the debt Phase 9A recorded, paid. It is cheap: proportional to the number of tables,
    /// not to the number of lines, and it does not allocate after the tables stop growing because the
    /// `Vec` is reused across paints by `LineHeights::from`'s caller... which it currently is not --
    /// `from` allocates a fresh `Vec` per paint. That is one allocation per paint, on a path that
    /// paints per keystroke, and it is the next thing to fix. It is called out here rather than left to
    /// be discovered, because the whole point of this function is to stop pretending the geometry is
    /// free.
    fn publish_line_heights(&mut self) {
        let pitch = self.chrome.metrics.cell_h.max(1);
        let spans: Vec<TableSpan> = self.editor.tables().spans().to_vec();
        let mut blocks: Vec<(u32, u32)> = Vec::with_capacity(spans.len());
        for span in &spans {
            let grid = holonomy_render::table::TableGrid::new(
                span,
                0,
                0,
                self.chrome.metrics.cell_w,
                self.chrome.metrics.cell_h,
                self.chrome.metrics.cell_h,
            );
            let line = self.line_index(span.start_byte as usize);
            blocks.push((line, grid.height_px()));
        }
        self.state.line_heights = holonomy_render::LineHeights::from(pitch, &blocks);
    }

    /// Draw every table whose first line is on screen, and widen `damage` to cover them.
    ///
    /// # Why the damage is the active cell and not the whole grid
    ///
    /// The directive's requirement is that typing in a cell invalidates only that cell's rectangle.
    /// That holds because of the split between *content* and *shape*: a keystroke inside a cell
    /// changes only that cell's glyphs, and the borders around it are in different rectangles that
    /// did not move. So when `tables_shape_dirty` is false, the added damage is the active cell's
    /// content rect. When it is true -- a table was inserted or a row appended -- every border line
    /// below moved, and the added damage is the whole grid.
    ///
    /// # Why the viewport bounds it
    ///
    /// A table's `TableGrid` is built from integer arithmetic over the span and costs nothing to skip,
    /// so a document with a hundred tables pays for the ones on screen. That is the same policy the
    /// image cache will need in 9C, and it is here first because a table's geometry is cheap: the
    /// expensive thing in 9C is a decoded raster.
    ///
    /// Known limit, recorded rather than hidden: a table is anchored at its document line and the
    /// lines *below* it are not offset by its height, so text after a tall table overlaps its last
    /// row. A full block layout is a larger change than this, and pretending otherwise by drawing the
    /// table somewhere else would be worse.
    fn emit_tables(&mut self, tree: &mut SurfaceTree, damage: &mut Option<DamageRect>) {
        let spans = self.editor.tables().spans();
        if spans.is_empty() {
            self.stats.table_cells_drawn = 0;
            self.stats.table_borders_drawn = 0;
            return;
        }
        let Ok(text) = self.editor.text() else {
            // A table whose bytes will not read cannot be drawn, and reporting it here would turn a
            // rendering problem into a paint failure. The session's own table operations already
            // surface the same error through `with_table`.
            return;
        };
        let m = self.chrome.metrics;
        let l = self.chrome.layout;
        let first = self.state.scroll_line;
        let last = first + l.rows;
        let mut cells_drawn = 0u32;
        let mut borders_drawn = 0u32;
        let mut extra: Option<DamageRect> = None;
        let active = self.active_cell;

        for span in spans.iter().copied() {
            let line = self.line_index(span.start_byte as usize) as u32;
            if line < first || line >= last {
                continue;
            }
            let Ok(resolved) = ResolvedTable::new(span, &text) else {
                continue;
            };
            // From the model, not from `row_pitch`: this is the line the model has reserved for the
            // table, so the table and the chrome agree on where that line is by construction.
            let top = l.text.y + self.state.line_heights.y(line - first);
            let grid = TableGrid::new(&span, l.text.x, top, m.cell_w, m.cell_h, m.cell_h);

            // --- borders, as procedural box-drawing glyph runs on the cell grid.
            for run in grid.borders() {
                let node = Node::Text(TextRun::new(
                    run.x as i32,
                    run.y as i32,
                    run.codepoint,
                    run.len,
                    holonomy_render::Style::MONOSPACE,
                    0,
                    holonomy_render::chrome::colour::INK,
                ));
                // A border run knows its own rectangle -- `x`, `y`, `width`, `height` are its fields,
                // because a run is *only* a rectangle of one repeated glyph.
                extra = union_opt(
                    extra,
                    Some(DamageRect::new(run.x, run.y, run.width, run.height)),
                );
                tree.before.push(SurfaceTree::leaf(node));
                borders_drawn += 1;
            }

            // --- cell text, clipped to each cell's content rect.
            for index in 0..span.cell_count() {
                let Ok(cell) = resolved.cell_at(index) else {
                    continue;
                };
                let Some((cx, cy, cw, ch)) = grid.cell_content_rect(cell.row, cell.col) else {
                    continue;
                };
                let content = &text[cell.start_byte as usize..cell.end_byte as usize];
                if !content.is_empty() {
                    // Clipped to the column width: a cell holding more characters than its column can
                    // show is truncated rather than allowed to run into its neighbour, because a
                    // `TextRun` has no clip rect and would otherwise overwrite the border.
                    let fits = (cw / m.cell_w.max(1)).min(u32::from(TextRun::MAX_LEN)) as usize;
                    let shown: String = content.iter().take(fits).map(|&b| b as char).collect();
                    tree.before.push(SurfaceTree::leaf(Node::Text(TextRun::new(
                        cx as i32,
                        cy as i32,
                        shown.chars().next().map_or(0, |c| c as u32),
                        shown.chars().count() as u16,
                        holonomy_render::Style::REGULAR,
                        0,
                        holonomy_render::chrome::colour::INK,
                    ))));
                }
                cells_drawn += 1;
                if active.is_some_and(|a| a.row == cell.row && a.col == cell.col) {
                    extra = union_opt(extra, Some(DamageRect::new(cx, cy, cw, ch)));
                }
            }

            // --- the caret, drawn inside the cell rather than by the chrome's line model, which has
            // no idea a cell exists.
            if let Some(a) = active.filter(|a| a.row < span.rows && a.col < span.cols) {
                if let Some((cx, cy, cw, _)) = grid.cell_content_rect(a.row, a.col) {
                    let cx = cx + a.offset_in_cell * m.cell_w;
                    tree.before
                        .push(SurfaceTree::leaf(Node::Rect(holonomy_render::Rect::new(
                            cx as i32,
                            cy as i32,
                            m.cell_w.min(cw),
                            m.cell_h,
                            holonomy_render::chrome::colour::CARET,
                        ))));
                    extra = union_opt(extra, Some(DamageRect::new(cx, cy, m.cell_w, m.cell_h)));
                }
            }

            if self.tables_shape_dirty {
                extra = union_opt(
                    extra,
                    Some(DamageRect::new(
                        grid.origin_x,
                        grid.origin_y,
                        grid.width_px(),
                        grid.height_px(),
                    )),
                );
            }
        }
        self.stats.table_cells_drawn = cells_drawn;
        self.stats.table_borders_drawn = borders_drawn;
        self.tables_shape_dirty = false;
        if let Some(rect) = extra {
            *damage = Some(match *damage {
                Some(d) => d.union(&rect),
                None => rect,
            });
        }
    }

    // ---------------------------------------------------------------- export

    /// Write the document to a pre-opened sink in `format`.
    ///
    /// The caller opened `sink.file` during boot. See the module docs for why.
    pub fn export(&mut self, sink: &mut ExportSink, title: &str) -> Result<Report, SessionError> {
        let report = holonomy_export::write(&self.editor, &mut sink.file, sink.format, title)?;
        sink.file.flush()?;
        self.stats.exports += 1;
        Ok(report)
    }

    /// Toggle bold over the word under the caret, or apply it at the caret.
    ///
    /// Exposed because the gate needs a way to *style* something deterministically; a keystroke
    /// binding for it would be a keymap change, and the keymap is already gated.
    pub fn toggle_bold(&mut self, from: u32, to: u32) -> Result<(), SessionError> {
        self.editor.style_range(from, to, STYLE_BOLD, 0)?;
        self.after_edit(0)
    }
}

/// Union two optional rectangles.
///
/// `DamageRect::union` takes two rectangles, and "no damage yet" is `None` rather than an empty
/// rectangle, so the optionality has to be lifted out of the way at every step. A `None` on either
/// side yields the other; two `None`s yield `None`.
fn union_opt(a: Option<DamageRect>, b: Option<DamageRect>) -> Option<DamageRect> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.union(&b)),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

/// Map an [`InputError`](holonomy_input::InputError) onto a [`SessionError`].
fn session_io(e: holonomy_input::InputError) -> SessionError {
    // `InputError`'s only I/O-shaped variant carries an errno, so it becomes an `io::Error` rather
    // than a new variant that says the same thing twice.
    match e {
        holonomy_input::InputError::Read(e) => {
            SessionError::Sink(std::io::Error::from_raw_os_error(e))
        }
        holonomy_input::InputError::Eof => SessionError::Sink(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "the scripted input stream ended mid-record",
        )),
    }
}
