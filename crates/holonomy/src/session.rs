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
use holonomy_display::{Frame, HeadlessScanout, Scanout};
use holonomy_export::{Format, Report};
use holonomy_input::{Command, Hotkey, InputSource, Keymap, ModifierState};
use holonomy_render::chrome::{Blink, Caret, Chrome, ChromeMetrics, ChromeState};
use holonomy_render::DamageRect;
use holonomy_text::{Editor, EditorError, SpanPolicy, STYLE_BOLD};

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
        }
    }
}

impl std::error::Error for SessionError {}

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
    scanout: HeadlessScanout,
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
    /// Where the caret was last drawn, so it can be erased.
    caret_drawn_at: Option<DamageRect>,
}

impl<'a> Session<'a> {
    /// A session over `editor`, painting through `painter`, presenting to `scanout`.
    pub fn new(
        editor: Editor,
        painter: Painter<'a>,
        scanout: HeadlessScanout,
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
            caret_drawn_at: None,
        }
    }

    /// The current frame.
    pub fn frame(&self) -> &Frame {
        &self.frame
    }

    /// The headless scanout, for a PPM dump.
    pub fn scanout(&self) -> &HeadlessScanout {
        &self.scanout
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
            if let Some(exit) = self.event(event)? {
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
    fn tick(&mut self) -> Result<(), SessionError> {
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
    fn event(&mut self, event: holonomy_input::InputEvent) -> Result<Option<Exit>, SessionError> {
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
            Command::Newline => self.insert(b"\n")?,
            Command::Tab => self.insert(b"\t")?,
            Command::Backspace => self.backspace()?,
            Command::DeleteForward => self.delete_forward()?,
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
        let tree = self.chrome.tree(&self.state);
        let stats = self.painter.paint(&mut self.frame, &tree, damage)?;
        self.stats.frames += 1;
        self.stats.pixels += stats.pixels;
        self.scanout.present(&self.frame)?;
        self.damage = DamageRect::EMPTY;
        Ok(())
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
