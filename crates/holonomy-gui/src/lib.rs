//! The window's half of Block 6 (`docs/TO-APP.md` M4): keys in, intents out, a picture on a surface.
//!
//! This crate is deliberately **backend-free**. It owns the three things a window does that the
//! editor loop does not:
//!
//! - **a keymap** — [`Key`] and [`Modifiers`] to [`Intent`], the one place a keystroke becomes an
//!   edit (`Ctrl/Cmd-Z` is undo, `Shift-Left` extends, `PageDown` scrolls a screen);
//! - **an app loop** — [`App`] holds the [`Editor`], applies intents, and knows when a repaint is
//!   due;
//! - **a surface** — [`Surface`], the pixel sink, and [`CellSurface`], an implementation that
//!   draws the picture into a grid of characters so the whole stack can be run and looked at with
//!   no GPU at all.
//!
//! # Why there is no GPUI here yet
//!
//! `ROADMAP.md` names GPUI as the UI framework of record, and it is what the real window will be
//! built on. It cannot be compiled in the environment this crate was written in: the workspace
//! lockfile is committed and CI builds `--locked`, so adding a dependency needs registry access to
//! resolve and checksum roughly two hundred transitive crates, and this environment has none. The
//! honest shape is therefore what is here: the logic a window needs, provable headlessly, with the
//! GPU backend as one file behind [`Surface`] when a machine that can fetch crates writes it.
//!
//! Nothing in this crate assumes a grid, a font or a window: [`Surface`] hands a backend clusters at
//! the advances the Eye measured, so a GPU backend draws exactly the picture the tests assert.

pub mod canvas;
pub mod panel;
pub mod store;
pub mod term;
pub mod transfer;
pub mod x11;

use std::sync::Arc;

use holonomy_eye::{Fixed, Picture, Style};
use holonomy_seal::{ArenaSlot, SecretArenas};
use holonomy_shell::{Editor, Effect, Intent, Move};
use holonomy_text::{
    FontFamily, Heading, SegmentSink, SegmentSource, StylePatch, TriState, WipeRegistry,
};
use zeroize::Zeroizing;

/// Bridges [`holonomy_text::WipeRegistry`] onto [`holonomy_seal::SecretArenas`] so every live
/// plaintext allocation (`ResidentSet`, `Typing`, and `Editor` viewport `WindowText`) is armed for
/// lock-free volatile zeroing by the emergency reaper (`L-7`).
pub struct SealWipeBridge(pub Arc<SecretArenas>);

impl WipeRegistry for SealWipeBridge {
    unsafe fn register(&self, ptr: *mut u8, len: usize) -> Option<usize> {
        // SAFETY: callers hold `[ptr, ptr + len)` live and unmoved until `unregister` returns.
        unsafe { self.0.register(ptr, len) }.map(|slot| slot.index())
    }

    fn unregister(&self, slot: usize) {
        if let Some(handle) = ArenaSlot::from_index(slot) {
            self.0.unregister(handle);
        }
    }
}

/// The keyboard modifiers that change what a keystroke means.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    /// Shift: extend the selection instead of collapsing it.
    pub shift: bool,
    /// Control, or Command on a platform that maps it here: the shortcut modifier.
    pub control: bool,
    /// Alt/Option: reserved for word-wise motion, which the loop does not have yet.
    pub alt: bool,
}

impl Modifiers {
    /// No modifiers held.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            shift: false,
            control: false,
            alt: false,
        }
    }

    /// Shift alone.
    #[must_use]
    pub const fn shift() -> Self {
        Self {
            shift: true,
            control: false,
            alt: false,
        }
    }

    /// Control (or Command) alone.
    #[must_use]
    pub const fn control() -> Self {
        Self {
            shift: false,
            control: true,
            alt: false,
        }
    }

    /// Control and shift.
    #[must_use]
    pub const fn control_shift() -> Self {
        Self {
            shift: true,
            control: true,
            alt: false,
        }
    }
}

/// A key, as a window reports it once the platform's own layout has been applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A character the layout produced.
    Character(char),
    /// Return or Enter: a hard break.
    Enter,
    /// Backspace.
    Backspace,
    /// Delete (forward).
    Delete,
    /// The left arrow.
    Left,
    /// The right arrow.
    Right,
    /// The up arrow.
    Up,
    /// The down arrow.
    Down,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Tab: advances UI focus.
    Tab,
    /// Escape: dismisses, never edits.
    Escape,
}

/// What a keystroke means, in the editor's vocabulary.
///
/// A key with no meaning here is not an error — it is silence, which is what a window does with a
/// key it does not use.
#[derive(Debug, Clone)]
pub struct Keymap {
    /// One screenful, for Page Up and Page Down.
    page: Fixed,
}

impl Keymap {
    /// A keymap that pages by `page` logical units.
    #[must_use]
    pub fn new(page: Fixed) -> Self {
        Self { page }
    }

    /// Re-points the page size at the height the window is actually showing.
    ///
    /// D-60: `page` was set once, when the window was built, and never followed the viewport.
    /// The terminal's panel is the visible case — `Ctrl+G` takes rows *away* from the text area,
    /// so Page Down stepped a screen that no longer existed and skipped lines the user never saw —
    /// but a resized window has the same defect with no panel involved. The window is the thing
    /// that knows both numbers, so it sets this on every resize.
    pub fn set_page(&mut self, page: Fixed) {
        self.page = page;
    }

    /// The intent `key` means under `modifiers`, if any.
    #[must_use]
    pub fn intent(&self, key: Key, modifiers: Modifiers) -> Option<Intent> {
        let extend = modifiers.shift;
        match key {
            Key::Character(character) if modifiers.control => {
                let shortcut = character.to_ascii_lowercase();
                match shortcut {
                    'z' if extend => Some(Intent::Redo),
                    'z' => Some(Intent::Undo),
                    'y' => Some(Intent::Redo),
                    'a' => Some(Intent::SelectAll),
                    's' => Some(Intent::Flush),
                    _ => None,
                }
            }
            Key::Character(character) if !modifiers.alt => {
                Some(Intent::Type(character.to_string()))
            }
            Key::Enter => Some(Intent::Type("\n".to_owned())),
            Key::Backspace => Some(Intent::Backspace),
            Key::Delete => Some(Intent::DeleteForward),
            Key::Left => Some(Intent::Move {
                to: Move::Left,
                extend,
            }),
            Key::Right => Some(Intent::Move {
                to: Move::Right,
                extend,
            }),
            Key::Up => Some(Intent::Move {
                to: Move::Up,
                extend,
            }),
            Key::Down => Some(Intent::Move {
                to: Move::Down,
                extend,
            }),
            Key::Home if modifiers.control => Some(Intent::Move {
                to: Move::DocumentStart,
                extend,
            }),
            Key::End if modifiers.control => Some(Intent::Move {
                to: Move::DocumentEnd,
                extend,
            }),
            Key::Home => Some(Intent::Move {
                to: Move::LineStart,
                extend,
            }),
            Key::End => Some(Intent::Move {
                to: Move::LineEnd,
                extend,
            }),
            Key::PageUp => Some(Intent::ScrollBy(Fixed::ZERO - self.page)),
            Key::PageDown => Some(Intent::ScrollBy(self.page)),
            Key::Character(_) | Key::Tab | Key::Escape => None,
        }
    }
}

/// The pixel sink: what a backend has to implement to be a window.
///
/// Everything is in logical units at the advances the Eye measured, so a backend never decides how
/// wide a cluster is — it is told. That is the whole reason paint and hit-test cannot disagree.
pub trait Surface {
    /// A new frame begins: forget the last one.
    fn begin(&mut self);
    /// The viewport's size, in logical units.
    fn size(&mut self, width: Fixed, height: Fixed);
    /// One cluster, its left edge and top, and the advance it was measured with.
    fn cluster(&mut self, x: Fixed, y: Fixed, text: &str, advance: Fixed, style: Style);
    /// A selection run: a rectangle in logical units.
    fn selection(&mut self, x: Fixed, y: Fixed, width: Fixed, height: Fixed);
    /// The caret, when the frame has one on screen.
    fn caret(&mut self, x: Fixed, y: Fixed, height: Fixed);
}

type CellSpan = Option<(Box<str>, usize)>;

/// A surface that draws into a grid of characters: the crudest window there is, and the only one
/// that needs no GPU.
///
/// It exists so the whole stack — keys, intents, frames, pictures — can be *run* and read back as
/// text. [`Self::render`] returns a screenful, which is what a headless demo prints and what the
/// tests assert against.
#[derive(Debug, Clone)]
pub struct CellSurface {
    /// Cells per logical unit: 8 makes `i` and `m` visibly different widths.
    scale: i64,
    columns: usize,
    rows: usize,
    cells: Vec<Vec<char>>,
    spans: Vec<Vec<CellSpan>>,
    styles: Vec<Vec<Style>>,
    caret: Option<(usize, usize)>,
    selections: Vec<(usize, usize, usize, usize)>,
    clusters: u64,
}

impl CellSurface {
    /// A surface `columns × rows` cells, at `scale` cells per logical unit.
    #[must_use]
    pub fn new(columns: usize, rows: usize, scale: i64) -> Self {
        Self {
            scale: scale.max(1),
            columns,
            rows,
            cells: vec![vec![' '; columns]; rows],
            spans: vec![vec![None; columns]; rows],
            styles: vec![vec![Style::DEFAULT; columns]; rows],
            caret: None,
            selections: Vec::new(),
            clusters: 0,
        }
    }

    /// How many columns the surface has.
    #[must_use]
    pub fn columns(&self) -> usize {
        self.columns
    }

    /// How many rows the surface has.
    #[must_use]
    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Resizes the grid, keeping whatever still fits — what a terminal does when its window is
    /// dragged larger.
    pub fn resize(&mut self, columns: usize, rows: usize) {
        let mut cells = vec![vec![' '; columns]; rows];
        let mut spans = vec![vec![None; columns]; rows];
        let mut styles = vec![vec![Style::DEFAULT; columns]; rows];
        for (row, line) in self.cells.iter().take(rows).enumerate() {
            for (column, cell) in line.iter().take(columns).enumerate() {
                cells[row][column] = *cell;
            }
        }
        for (row, line) in self.spans.iter().take(rows).enumerate() {
            for (column, span) in line.iter().take(columns).enumerate() {
                spans[row][column].clone_from(span);
            }
        }
        for (row, line) in self.styles.iter().take(rows).enumerate() {
            for (column, style) in line.iter().take(columns).enumerate() {
                styles[row][column] = *style;
            }
        }
        self.columns = columns;
        self.rows = rows;
        self.cells = cells;
        self.spans = spans;
        self.styles = styles;
    }

    /// The cell a logical coordinate falls in.
    fn cell(&self, value: Fixed) -> usize {
        let sixtyfourths = value.sixtyfourths();
        let cells = sixtyfourths * self.scale / 64;
        usize::try_from(cells).unwrap_or(0)
    }

    /// The screen as text, one line per row, trailing blanks kept so columns line up.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for row in &self.cells {
            let line: String = row.iter().collect();
            out.push_str(&line);
            out.push('\n');
        }
        out
    }

    /// Where the caret was drawn, in cells, if it was.
    #[must_use]
    pub fn caret_cell(&self) -> Option<(usize, usize)> {
        self.caret
    }

    /// How many clusters were drawn into this frame.
    #[must_use]
    pub fn clusters(&self) -> u64 {
        self.clusters
    }

    /// The character in a cell, or a space when the cell is empty.
    #[must_use]
    pub fn at(&self, column: usize, row: usize) -> char {
        self.cells
            .get(row)
            .and_then(|line| line.get(column))
            .copied()
            .unwrap_or(' ')
    }

    /// The full grapheme cluster anchored at `(column, row)` and its cell advance, when a cluster
    /// was placed there (OQ-25).
    #[must_use]
    pub fn cluster_at(&self, column: usize, row: usize) -> Option<(&str, usize)> {
        self.spans
            .get(row)
            .and_then(|line| line.get(column))
            .and_then(|entry| entry.as_ref())
            .map(|(text, span)| (text.as_ref(), *span))
    }

    /// The [`Style`] recorded for the cluster anchored at `(column, row)`.
    #[must_use]
    pub fn style_at(&self, column: usize, row: usize) -> Style {
        self.styles
            .get(row)
            .and_then(|line| line.get(column))
            .copied()
            .unwrap_or(Style::DEFAULT)
    }

    /// Whether a cell is inside a selection run.
    #[must_use]
    pub fn selected(&self, column: usize, row: usize) -> bool {
        self.selections.iter().any(|(x, y, width, height)| {
            column >= *x && column < x + width && row >= *y && row < y + height
        })
    }
}

impl Surface for CellSurface {
    fn begin(&mut self) {
        for line in &mut self.cells {
            line.fill(' ');
        }
        for line in &mut self.spans {
            line.fill(None);
        }
        for line in &mut self.styles {
            line.fill(Style::DEFAULT);
        }
        self.caret = None;
        self.selections.clear();
        self.clusters = 0;
    }

    fn size(&mut self, _width: Fixed, _height: Fixed) {}

    fn cluster(&mut self, x: Fixed, y: Fixed, text: &str, advance: Fixed, style: Style) {
        let column = self.cell(x);
        let row = self.cell(y);
        if column >= self.columns || row >= self.rows {
            return;
        }
        // One cell per cluster's first character on `cells` for grid probes; `spans` carries the
        // complete cluster and its cell advance so a terminal renderer emits marks and wide glyphs
        // at their true column width (OQ-25).
        if let Some(character) = text.chars().next() {
            self.cells[row][column] = character;
            self.spans[row][column] = Some((text.into(), self.cell(advance).max(1)));
            self.styles[row][column] = style;
            self.clusters += 1;
        }
    }

    fn selection(&mut self, x: Fixed, y: Fixed, width: Fixed, height: Fixed) {
        let column = self.cell(x);
        let row = self.cell(y);
        let columns = self.cell(width).max(1);
        let rows = self.cell(height).max(1);
        self.selections.push((column, row, columns, rows));
    }

    fn caret(&mut self, x: Fixed, y: Fixed, _height: Fixed) {
        self.caret = Some((self.cell(x), self.cell(y)));
    }
}

/// Why a frame could not be drawn.
#[derive(Debug)]
pub enum GuiError {
    /// The editor refused the frame: a leaf the resident set could not read.
    Shell(holonomy_shell::ShellError),
}

impl core::fmt::Display for GuiError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Shell(error) => write!(formatter, "the editor refused a frame: {error}"),
        }
    }
}

impl std::error::Error for GuiError {}

impl From<holonomy_shell::ShellError> for GuiError {
    fn from(error: holonomy_shell::ShellError) -> Self {
        Self::Shell(error)
    }
}

/// The application: an editor, a keymap, a surface, and whether a repaint is owed.
pub struct App<S: SegmentSink, R: SegmentSource, H: holonomy_eye::Shaper, T: Surface> {
    editor: Editor<S, R, H>,
    keymap: Keymap,
    surface: T,
    dirty: bool,
    /// Whether the last intent typed, so a run of keystrokes is one undo step.
    typing: bool,
}

impl<S, R, H, T> App<S, R, H, T>
where
    S: SegmentSink,
    R: SegmentSource,
    H: holonomy_eye::Shaper,
    T: Surface,
{
    /// A window over `editor`, drawing into `surface`.
    #[must_use]
    pub fn new(editor: Editor<S, R, H>, keymap: Keymap, surface: T) -> Self {
        Self {
            editor,
            keymap,
            surface,
            dirty: true,
            typing: false,
        }
    }

    /// The editor, for a caller that needs a number the window does not show.
    pub fn editor(&mut self) -> &mut Editor<S, R, H> {
        &mut self.editor
    }

    /// The resident set, for a window that reports on what it holds (the security panel).
    #[must_use]
    pub fn residency(&self) -> &holonomy_text::ResidentSet<R> {
        self.editor.residency()
    }

    /// The surface, for a caller that wants to read back what was drawn.
    #[must_use]
    pub fn surface(&self) -> &T {
        &self.surface
    }

    /// The surface, mutably — for a window that has resized itself, or is about to be replaced.
    pub fn surface_mut(&mut self) -> &mut T {
        &mut self.surface
    }

    /// Whether a repaint is owed.
    #[must_use]
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Marks the surface as needing a repaint on the next loop tick.
    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Renders a compact Ribbon readout (`UI-SPEC.md` §3, Law `U0`) for the current caret or
    /// selection, using `—` (`\u{2014}`) whenever an attribute is mixed across the selection.
    #[must_use]
    pub fn toolbar_summary(&self) -> String {
        let tb = self.editor.toolbar_state();
        let fam = tb.family.display_or_dash(|f| match f {
            FontFamily::Sans => "Sans".to_owned(),
            FontFamily::Serif => "Serif".to_owned(),
            FontFamily::Mono => "Mono".to_owned(),
        });
        let hdg = tb.heading.display_or_dash(|h| match h {
            Heading::None => "Body".to_owned(),
            Heading::Title => "Title".to_owned(),
            Heading::H1 => "H1".to_owned(),
            Heading::H2 => "H2".to_owned(),
            Heading::H3 => "H3".to_owned(),
        });
        let flag = |st: &TriState<bool>, on: &'static str| match st {
            TriState::Uniform(true) => on,
            TriState::Uniform(false) => "",
            TriState::Mixed => "\u{2014}",
        };
        let mut marks = Vec::new();
        for (st, tag) in [
            (&tb.bold, "B"),
            (&tb.italic, "I"),
            (&tb.underline, "U"),
            (&tb.strikethrough, "S"),
            (&tb.code_span, "`"),
        ] {
            let s = flag(st, tag);
            if !s.is_empty() {
                marks.push(s);
            }
        }
        if marks.is_empty() {
            format!("{fam} {hdg}")
        } else {
            format!("{fam} {hdg} [{}]", marks.join(""))
        }
    }

    fn format_toggle(&mut self, patch: StylePatch) -> Result<Vec<Effect>, GuiError> {
        self.typing = false;
        let effects = self.editor.apply(Intent::Format(patch))?;
        if !effects.is_empty() {
            self.dirty = true;
        }
        Ok(effects)
    }

    /// Toggles bold across the active selection (`Ctrl+B`).
    pub fn toggle_bold(&mut self) -> Result<Vec<Effect>, GuiError> {
        let next = !matches!(self.editor.toolbar_state().bold, TriState::Uniform(true));
        self.format_toggle(StylePatch {
            bold: Some(next),
            ..StylePatch::default()
        })
    }

    /// Toggles italic across the active selection (`Ctrl+I` / `Ctrl+T`).
    pub fn toggle_italic(&mut self) -> Result<Vec<Effect>, GuiError> {
        let next = !matches!(self.editor.toolbar_state().italic, TriState::Uniform(true));
        self.format_toggle(StylePatch {
            italic: Some(next),
            ..StylePatch::default()
        })
    }

    /// Toggles underline across the active selection (`Ctrl+U`).
    pub fn toggle_underline(&mut self) -> Result<Vec<Effect>, GuiError> {
        let next = !matches!(
            self.editor.toolbar_state().underline,
            TriState::Uniform(true)
        );
        self.format_toggle(StylePatch {
            underline: Some(next),
            ..StylePatch::default()
        })
    }

    /// Toggles inline code span across the active selection (`Ctrl+E`).
    pub fn toggle_code_span(&mut self) -> Result<Vec<Effect>, GuiError> {
        let next = !matches!(
            self.editor.toolbar_state().code_span,
            TriState::Uniform(true)
        );
        self.format_toggle(StylePatch {
            code_span: Some(next),
            ..StylePatch::default()
        })
    }

    /// Toggles `heading` (`H1`..`H3`) across the active selection (`Ctrl+1`..`3`).
    pub fn toggle_heading(&mut self, heading: Heading) -> Result<Vec<Effect>, GuiError> {
        let next = if self.editor.toolbar_state().heading == TriState::Uniform(heading) {
            Heading::None
        } else {
            heading
        };
        self.format_toggle(StylePatch {
            heading: Some(next),
            ..StylePatch::default()
        })
    }

    /// A key press. Returns the effects the window still owes the user.
    ///
    /// # Errors
    ///
    /// [`GuiError`] when the editor refuses the edit.
    pub fn press(&mut self, key: Key, modifiers: Modifiers) -> Result<Vec<Effect>, GuiError> {
        if modifiers.control && !modifiers.alt {
            if let Key::Character(ch) = key {
                match ch.to_ascii_lowercase() {
                    'b' => return self.toggle_bold(),
                    'i' | 't' => return self.toggle_italic(),
                    'u' => return self.toggle_underline(),
                    'e' => return self.toggle_code_span(),
                    '1' => return self.toggle_heading(Heading::H1),
                    '2' => return self.toggle_heading(Heading::H2),
                    '3' => return self.toggle_heading(Heading::H3),
                    _ => {}
                }
            }
        }
        let Some(intent) = self.keymap.intent(key, modifiers) else {
            return Ok(Vec::new());
        };
        // One undo step per typing **burst**, the way every editor coalesces keystrokes: the mark
        // is laid when a run of typing begins, so Ctrl+Z takes back the run rather than one letter.
        let typing = matches!(intent, Intent::Type(_));
        if typing && !self.typing {
            self.editor.mark();
        }
        self.typing = typing;
        // Decided before `intent` is moved into `apply`, which is where it is consumed.
        let moves_the_caret = matches!(
            intent,
            Intent::Type(_)
                | Intent::Backspace
                | Intent::DeleteForward
                | Intent::Move { .. }
                | Intent::Undo
                | Intent::Redo
        );
        let mut effects = self.editor.apply(intent)?;
        // D-53/D-57: the caret a keystroke leaves behind is the window's to keep on screen, and
        // nothing was doing it. A document longer than a screen scrolled nowhere, so typing past
        // the last visible line moved the caret to a row that was never painted and the screen
        // stopped following the person typing. The shell already has the policy (the smallest
        // scroll that shows the caret); the loop just never asked for it.
        //
        // It is asked through `Intent::RevealCaret` rather than by calling `reveal_caret` here:
        // the intent layer is the one place that turns a state change into the effects a backend
        // renders from, and a window that reached past it returned no `Redraw` for a scroll it had
        // performed. Best effort on the error: the edit has already landed, and a leaf the reader
        // could not lay out is the *frame's* problem — propagating it would report a keystroke that
        // reached the document as a refused edit, the same lie D-47 was about, one layer down.
        if moves_the_caret && !effects.is_empty() {
            if let Ok(revealed) = self.editor.apply(Intent::RevealCaret) {
                if !revealed.is_empty() && !effects.contains(&Effect::Redraw) {
                    effects.push(Effect::Redraw);
                }
            }
        }
        if !effects.is_empty() {
            self.dirty = true;
        }
        Ok(effects)
    }

    /// The window changed size.
    ///
    /// # Errors
    ///
    /// [`GuiError`] when the editor refuses the resize.
    pub fn resize(&mut self, width: Fixed, height: Fixed) -> Result<Vec<Effect>, GuiError> {
        // A different **width** is a different geometry: the shell empties its measured index,
        // because every height described the old wrap. It does not re-measure — that is a caller's
        // policy — so a window that resized once could scroll no further than its first screen for
        // the rest of the session (D-53). The window asks for the measurement it invalidated.
        let width_changed = self.editor.width() != width;
        let mut effects = self.editor.apply(Intent::Resize {
            width,
            viewport: height,
        })?;
        // D-60: a page is a screen, and the screen just changed. The keymap was told the height
        // it had when the window was built and was never told again, so a panel opening (which
        // shrinks the surface) or a resized terminal made Page Down skip lines.
        let page = self.editor.viewport();
        self.keymap.set_page(page);
        if width_changed {
            // Through the intent, for the reason above: a re-measure is a state change and owes
            // the caller its effects. Best effort on the error — a document the reader refuses is
            // a provisional frame, not a dead window, and the refusal is reported by whatever drew.
            if let Ok(reflowed) = self.editor.apply(Intent::Reflow) {
                if !reflowed.is_empty() && !effects.contains(&Effect::Redraw) {
                    effects.push(Effect::Redraw);
                }
            }
        }
        if !effects.is_empty() {
            self.dirty = true;
        }
        Ok(effects)
    }

    /// The text of the current frame — what is on screen, not what is in the document.
    ///
    /// # Errors
    ///
    /// [`GuiError`] when the editor refuses a frame.
    pub fn frame_text(&mut self) -> Result<String, GuiError> {
        let frame = self.editor.frame()?;
        Ok(frame.text().to_owned())
    }

    /// The caret's document offset.
    pub fn caret(&mut self) -> u64 {
        self.editor.caret()
    }

    /// How many bytes the document holds — what is *in* the document, which a frame may not yet
    /// show (AUDIT D-41).
    pub fn document_len(&mut self) -> u64 {
        self.editor.document_len()
    }

    /// Whether the document holds bytes or structural edits the file does not have yet — what
    /// "unsaved" means.
    ///
    /// This is read from the session, not remembered in a flag: a caret move **seals** the open
    /// leaf, so an arrow key commits what was typed, while a deletion across sealed text or an
    /// undo of a sealed root keeps the session dirty until a flush commits its spine manifest
    /// (D-47, OQ-30).
    #[must_use]
    pub fn has_pending(&self) -> bool {
        !self.editor.typing().pending().is_empty()
            || self.editor.typing().has_uncommitted_structure()
    }

    /// The selection, as ordered ends, when it is not empty.
    pub fn selection(&mut self) -> Option<(u64, u64)> {
        let selection = self.editor.selection();
        (selection.anchor() != selection.focus()).then(|| (selection.anchor(), selection.focus()))
    }

    /// Paints the current frame into the surface, and only paints when a repaint is owed.
    ///
    /// The answer is whether anything was painted, and callers are expected to use it: L-5 is
    /// "zero repaint while the view is unchanged", and the terminal front end broke it by
    /// discarding this bool and rewriting the whole screen every loop iteration — a full-screen
    /// escape dump every 1.5 s on an idle window (AUDIT D-68). `false` means the surface still
    /// holds the last frame, so a backend that trusts it writes nothing.
    ///
    /// # Errors
    ///
    /// [`GuiError`] when the editor refuses a frame.
    pub fn draw(&mut self) -> Result<bool, GuiError> {
        if !self.dirty {
            return Ok(false);
        }
        let (picture, origin, text) = {
            let frame = self.editor.frame()?;
            let picture: Picture = frame.picture();
            (
                picture,
                frame.origin(),
                Zeroizing::new(frame.text().to_owned()),
            )
        };
        self.surface.begin();
        self.surface.size(picture.width, picture.height);
        for (index, line) in picture.lines.iter().enumerate() {
            for span in picture.selections_on(index) {
                self.surface
                    .selection(span.x, line.top, span.end - span.x, line.height);
            }
            for cluster in picture.clusters_on(index) {
                let from = usize::try_from(cluster.byte.saturating_sub(origin)).unwrap_or(0);
                let to = from + cluster.len;
                let text = text.get(from..to).unwrap_or_default();
                self.surface
                    .cluster(cluster.x, line.top, text, cluster.advance, cluster.style);
            }
        }
        if let Some(caret) = picture.caret {
            self.surface.caret(caret.x, caret.top, caret.height);
        }
        self.dirty = false;
        Ok(true)
    }
}
