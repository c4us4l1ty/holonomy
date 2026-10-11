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

use crate::counts::TextCounts;
use crate::doclines::{DocLines, Sync};
use holonomy_display::paint::Painter;
use holonomy_display::{Frame, FrameError, Scanout};
use holonomy_export::{Format, Report};
use holonomy_image::IcebergCache;
use holonomy_input::{Command, Hotkey, InputSource, Keymap, ModifierState};
use holonomy_render::chrome::{Blink, Caret, Chrome, ChromeMetrics, ChromeState};
use holonomy_render::math::{self, MathNode};
use holonomy_render::math_layout::{layout_boxed, MathLayout, MathMetrics, MathRun};
use holonomy_render::table::TableGrid;
use holonomy_render::DamageRect;
use holonomy_render::{widgets, DocRun, Node, Rect, SurfaceTree, TextRun, TextSource};
use holonomy_text::{Editor, EditorError, SpanPolicy, ANCHOR_BYTES, STYLE_BOLD, STYLE_ITALIC};
use holonomy_text::{MathSpan, Nav, ResolvedTable, TableCursor, TableSpan};

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
    /// Edits that changed exactly one line's length, so the geometry was a `O(log n)` point update.
    ///
    /// Phase 11. Paired with [`SessionStats::line_rebuilds`] this is the honest answer to "how much of
    /// the keystroke path is still `O(document)`": a rebuild is one per newline typed, and everything
    /// else is one point update.
    pub line_updates: u32,
    /// Edits that added or removed a newline, so both Fenwick trees were rebuilt. `O(n)`.
    ///
    /// `LineGeometry::resize_lines` rebuilds rather than inserting because a Fenwick tree supports point
    /// updates, not insertion, and it says so itself. Typing a letter is a point update; pressing Enter
    /// is a rebuild. That asymmetry is the design.
    pub line_rebuilds: u32,
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
    /// Document lines the body-text emitter drew in the last paint. Phase 12.
    ///
    /// **The number that answers "is anything on the page".** Zero with a non-empty document means the
    /// emitter is not running or every line was culled; a document with text on it says so here. Before
    /// Phase 12 this was always zero, because nothing drew the document's own words.
    pub lines_drawn: u32,
    /// Document *bytes* turned into [`DocRun`]s in the last paint. Phase 12.
    ///
    /// **Bytes, not glyphs**, and the distinction is deliberate. A line of `é` is two bytes per glyph, so
    /// a byte count overstates the glyph count by up to 4x. `PaintStats::doc_glyphs` holds the glyph
    /// figure from the blitter's side, and the two are different numbers answering different questions:
    /// "how much of the document was on the page" and "how many glyphs were drawn". Asserting them equal
    /// would be asserting that the document is ASCII.
    pub glyphs_drawn: u32,
    /// Border runs drawn in the last paint.
    pub table_borders_drawn: u32,
    /// Section-manifest updates, from [`crate::manifest::Sync::OneSection`]. Phase 13.
    ///
    /// **Two per keystroke is the expected steady state**, not a smell: [`Manifest::sync`] re-measures the
    /// caret's section and the last one, because a byte appended at the end belongs to the last section and
    /// no line count would say so. The gate that asserts this is `tests/session_manifest.rs`'s
    /// `a_keystroke_updates_sections_and_never_rebuilds_one`.
    pub section_updates: u64,
    /// Section-manifest rebuilds, from [`crate::manifest::Sync::Rebuilt`]. Phase 13.
    ///
    /// **Should be about one keystroke in forty**, on the newline. A number much above that means
    /// boundaries are moving on ordinary keystrokes, which is the fixed-size-section tax and would be worth
    /// a different section size.
    pub section_rebuilds: u64,
    /// Images inserted by `Ctrl+I` or [`Session::insert_image`].
    pub image_inserts: u32,
    /// Image nodes emitted in the last paint.
    pub images_drawn: u32,
    /// Rasters decoded into the Iceberg cache, across the session's life.
    ///
    /// The scaler ran on every one of them: §2.9.3's cache holds page-column-width rasters, so a
    /// 1920x1080 source is a downscale before it is ever resident. Counted rather than inferred from
    /// a timer, which is what §2.9.3 asks for.
    pub images_decoded: u32,
    /// Rasters the Iceberg cache scrubbed and released, across the session's life.
    pub images_evicted: u32,
    /// Formulas inserted by `Ctrl+M` or [`Session::insert_math`].
    pub math_inserts: u32,
    /// Formulas drawn as a *compiled* layout in the last paint.
    ///
    /// The compiled/raw split is the whole of the focused-vs-unfocused rule, so it is a count rather
    /// than a detail: a formula that should have compiled and did not is invisible on screen as
    /// anything except "it looks like source", which is also what a formula being edited looks like.
    pub math_compiled: u32,
    /// Formulas drawn as raw LaTeX in the last paint, because the caret is inside them.
    pub math_raw: u32,
    /// Procedural fills drawn in the last paint: fraction bars and radical overlies.
    pub math_rules: u32,
    /// Formulas that failed to parse and fell back to raw LaTeX while the caret was elsewhere.
    ///
    /// Counted because a parse error is otherwise *invisible by design*: `math.rs` renders an
    /// unparseable formula as its own source, which is exactly what an edited one looks like. Without
    /// this counter, a formula that stopped compiling three edits ago would sit there looking fine.
    pub math_parse_errors: u32,

    // ---------------------------------------------------------------- pointer, part 20
    //
    // **Four counters, and the reason there are four is that "clicks" is not a useful number.**
    //
    // A chrome with twenty-one toolbar buttons, eight menus and a sidebar in front of a session whose
    // document model has no bold, no colour and no font size is going to have a lot of buttons that
    // are drawn, hovered, pressed and do nothing. **A single counter would make that indistinguishable
    // from a broken routing path** -- both would read "clicks happened, nothing happened". These four
    // separate the three that work from the one that does not.
    /// Pointer events delivered, of every kind.
    ///
    /// **Every kind, including motion**, so this is the mouse's event rate rather than a click count.
    /// A 125 Hz mouse over the toolbar produces this number in the hundreds per second; the other
    /// three are what make that not alarming.
    pub pointer_events: u64,
    /// Presses that landed on a widget.
    pub pointer_presses: u32,
    /// Presses that produced a [`Command`] the session applied.
    ///
    /// **The routing works if this is non-zero.** Every gate in `tests/pointer.rs` that asserts a
    /// click did something asserts this moved.
    pub pointer_commands: u32,
    /// Presses on a widget that has no action behind it yet.
    ///
    /// **The honest number for this build, and it is large.** Bold, italic, underline, the font
    /// controls, print, spellcheck and most menu items are buttons with nothing behind them, because
    /// the document model has no representation for any of them. **Counted rather than swallowed**
    /// because a count that is zero would mean either that every button works or that nothing is
    /// being hit-tested, and those need to be distinguishable.
    pub pointer_inert: u32,
    /// Presses that changed the chrome's state and nothing else.
    ///
    /// # Why this exists, and what part 21 got wrong
    ///
    /// Part 21 routed the three dropdowns — zoom, style, font — through `apply_action` and counted each
    /// as `pointer_commands`. **That was wrong by that field's own definition**, which says *presses that
    /// produced a `Command` the session applied*: none of the three produces a `Command` at all, and
    /// `apply_action` handles them before `action_command` is ever reached.
    ///
    /// **So the counter was claiming a document edit that did not happen.** The four alternatives a press
    /// can have — a command, a chrome-state change, a toggle, or nothing — need four counters, and this
    /// is the third. Zeroing it would mean "no button changes anything about the window", which is a
    /// different and less useful claim than "no button edits the document".
    ///
    /// **The three that land here are not doing nothing**, which is the whole reason this is not
    /// `pointer_inert`: choosing a face changes the toolbar's label, choosing a style changes the style
    /// control's label, and choosing a zoom changes the zoom label — and **part 23 measured that last one
    /// and found it is the *only* pixel that moves.** `tests/zoom.rs` is the measurement.
    pub pointer_chrome: u32,
    /// Paints that asked the span map for the visible page's styled runs.
    ///
    /// **Part 26, and it exists because "once per paint" was a comment and not a number.** Part 24
    /// called `SpanMap::runs_in` once per *line*, so a forty-line page paid forty heap allocations and
    /// forty walks of the span list per frame. The call is hoisted out of the loop now, and this counts
    /// it, so `one_span_query_serves_the_whole_page` can assert the shape rather than trust it.
    ///
    /// **A paint increments this by one**, whatever the page holds and however deep in the document it
    /// is. A gate that watched it climb with the line count would pass on a one-line page, which is
    /// exactly the fixture mistake this project has now made four times.
    pub span_queries: u32,
    /// Caret moves whose column was computed by walking text rather than by subtracting offsets.
    ///
    /// **A measurement, not a warning.** Part 22 added a scan over the caret's line prefix to count
    /// UTF-8 scalars, and the first thing it had to answer was "does this fire on every keystroke, or
    /// only when the caret is not at a column boundary?" — so the number is there rather than asserted
    /// from reading the code. **A caret at the start of its line does no scan at all** and is not
    /// counted, which is what makes the difference visible: `DocumentStart`, `Home`, and typing at the
    /// beginning of a line all leave this at zero.
    pub caret_column_scans: u32,
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
    ///
    /// **Private as of part 16**, and that is the fix rather than a style choice. Assigning
    /// `session.editor = opened.editor` compiles, runs, and leaves a session whose `DocLines`,
    /// `Manifest` and `TextCounts` describe the *previous* document -- which is what the product did for
    /// its entire life, painting one empty line over a real document. See
    /// [`adopt_document`](Self::adopt_document); the only supported way to put a document in here.
    ///
    /// **Read access is unchanged**, because a dozen gates and the export path read it and none of them
    /// can corrupt the session by looking.
    editor: Editor,
    /// The style flags armed at the caret, or 0 for none. **Part 25.**
    ///
    /// **A field and not a zero-width span**, because `SpanMap::style_range` returns `Ok(())` for
    /// `start == end` and stores nothing — so "the next character typed is bold" has nowhere to live in
    /// the span map, and had to live here. The check was worth doing before designing around it.
    ///
    /// **`holonomy_text`'s `STYLE_BOLD` and `STYLE_ITALIC` are the values**, not `1` and `2`: the
    /// chrome's [`ChromeState::styles`](holonomy_render::ChromeState::styles) is a pair of bools and this
    /// is a bit set, and the two must agree. `armed_style` is the one function that reads both and
    /// `set_armed_style` the one that writes them, so the agreement is not a convention.
    ///
    /// # Why this is an `Option` and not a `u16`, which is the whole design of part 25
    ///
    /// **`Session::insert` uses `SpanPolicy::GrowIntoInsert`** — "a span ending exactly at the insertion
    /// point grows and inserted text inherits the *preceding* span's style" — and it does so for a good
    /// reason that predates any toggle: *typing at the end of a bold word should keep it bold.*
    ///
    /// **That and a Bold button cannot both be expressed by a flag.** With a plain `u16` that starts at
    /// 0, turning Bold off leaves the flag at 0, the insert inherits bold from the character before the
    /// caret, and **the button says off while the next character comes out bold.** The first version of
    /// this part had exactly that bug and `disarming_stops_the_next_character_being_bold` caught it.
    ///
    /// So there are **three** states and not two:
    ///
    /// | `pending_style` | meaning | the insert |
    /// |---|---|---|
    /// | `None` | the user has expressed nothing | inherits, `GrowIntoInsert` wins |
    /// | `Some(flags)` with flags | armed | restyled to `flags` |
    /// | `Some(0)` | **explicitly plain** | restyled to plain, overriding inheritance |
    ///
    /// **And `ChromeState::styles`' two bools can express it**, because the bools answer "is this on" and
    /// the `Option` answers "has the user said anything". `Some(0)` shows as both buttons off, which is
    /// correct: the user is looking at "not bold", and what they cannot see is the `Option` behind it.
    pending_style: Option<u16>,
    /// **The colour the next character typed will be, or `None` if the user has said nothing.**
    /// Part 29.
    ///
    /// **`Option<u32>` for the same reason [`pending_style`](Self) is an `Option<u16>`**, and with the
    /// same three states: `None` inherit, `Some(rgb)` arm, **`Some(0)` is *not* "plain"** -- `0` is
    /// what the span field calls "the document's default colour", so writing `Some(0)` and writing
    /// `None` mean the same thing and the distinction would be a lie.
    ///
    /// **Why a field and not a span parked at the caret**, which is the reasoning part 25 recorded for
    /// the same question: `SpanMap::style_range` returns `Ok(())` for `start == end` and stores
    /// nothing, so "the next character is crimson" cannot be a zero-width span.
    pending_colour: Option<u32>,
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
    /// **Whether a page drag is in progress.** Part 28.
    ///
    /// ## Why a flag and not a drag *state*
    ///
    /// **Because the anchor is already in the editor.** A drag needs to know two things — where it
    /// started, and whether it is still going — and `Editor::selection_anchor` is the first. **A second
    /// copy of the anchor here would be a second thing that can disagree with the first**, which is the
    /// defect part 22 found in `ChromeState` and part 24 nearly found again.
    ///
    /// ## Why the flag is a field and not `pressed.is_some()`
    ///
    /// **Because `pressed` is about the chrome and this is about the document.** They happen to both
    /// start on a left press, and coupling them would mean a press on a toolbar button that the user
    /// then dragged back over the page would start selecting text. **The flag is set only by a press
    /// that landed on the page**, and nothing else touches it.
    page_drag: bool,
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
    /// Line geometry in document byte coordinates, for `O(log n)` line lookup. Phase 11.
    ///
    /// Replaces the newline-counting scans in [`Session::line_start`] and [`Session::line_index`], which
    /// were `O(bytes before the caret)` and ran on every keystroke. See [`crate::doclines`] for why the
    /// terminator convention is this module's problem and not `LineGeometry`'s.
    lines: DocLines,
    /// The document's sections: where they are, how tall they are, and what they contain. Phase 13.
    ///
    /// **The reason this field earns 6 MiB.** `Manifest::span_total` answers *"does this document contain
    /// a formula, a table or an image?"* without reading it, and four emitters ask that question before
    /// deciding whether to read the document at all. A document with none of them — which is most
    /// documents, and all 97% of a book — answers zero from **1,164 bytes of structure**.
    ///
    /// Phase 12 could not remove `doc_scratch` because *"does this document contain any math"* could not
    /// be asked without reading the bytes to find the `$$`. This is the answer to that, and it is a
    /// question about structure rather than content, which is what a manifest is.
    manifest: crate::manifest::Manifest,
    /// Scratch for [`Manifest::sync`]'s two section reads. Phase 13.
    ///
    /// Sized to [`crate::manifest::SECTION_BYTES`] and allocated once, like `line_scratch`: a scratch that
    /// grows on the keystroke path is one allocation per growth, and two section reads per keystroke is
    /// exactly the place where that would show up. `sync_manifest` takes it with `mem::take` because
    /// `Manifest::sync` needs `&mut Vec<u8>` and `&self.editor` at the same time, and both are fields of
    /// `self` — a borrow-checker consequence worth naming rather than working around with a clone.
    manifest_scratch: Vec<u8>,
    /// The status bar's word and line totals, maintained as deltas. Phase 11 item 4.
    ///
    /// Built by a full scan at construction and folded forward by every edit whose bytes the caller
    /// knows. `undo` and `redo` rescan — see [`Session::undo`] for why that is deliberate.
    counts: TextCounts,
    /// Scratch for the whole document, reused across paints. Phase 11.
    ///
    /// Every paint-path function that needs the document's bytes -- `emit_tables`, `emit_math`,
    /// `emit_images`, `publish_line_heights`, `image_blocks` -- called `Editor::text()`, which
    /// allocates a `Vec` the size of the document. Five allocations of 6.4 MiB per keystroke, on the
    /// paint path, is what `tests/session_no_alloc.rs` measured before this field existed.
    ///
    /// A field rather than a local for the reason [`Session::table_scratch`] exists: a local array
    /// passed to `read_into` escapes and is promoted to the heap, which turns one allocation per
    /// session into one per paint. Grown only when the document outgrows it, so steady-state typing
    /// into a document does not reallocate.
    doc_scratch: Vec<u8>,
    /// Scratch for **one page's worth** of document text, for [`Session::emit_body_text`]. Phase 12.
    ///
    /// Sized to [`LINE_SCRATCH_BYTES`] = 48 KiB, which is `Layout::rows` (43) x `DocRun::MAX_BYTES`
    /// (1,024) rounded up. **This is the buffer that replaces reading the whole document to draw it.**
    ///
    /// `doc_scratch` above is whole-document and exists because `emit_tables`, `emit_math`,
    /// `emit_images`, `publish_line_heights` and `image_blocks` all want random access across the whole
    /// text. Body text does not: it wants the 43 lines that are on screen. So it gets its own buffer,
    /// sized to the page, and the 6 MiB that `doc_scratch` costs on a 6 MiB document becomes 48 KiB for
    /// the part of the paint that draws the document's own words.
    ///
    /// **Allocated once, at construction**, and never grown: a line longer than the page's measure is
    /// drawn truncated (see `emit_body_text`), so a longer line cannot make this grow. If a later phase
    /// makes wrapping work, this becomes the buffer that grows to the longest *wrapped* line, which is
    /// still bounded by the page measure and not by the document.
    line_scratch: Vec<u8>,
    /// How much of [`Session::line_scratch`] the last body-text emit filled. Phase 12.
    ///
    /// **A separate field rather than `line_scratch.len()`**, because `emit_body_text` truncates the
    /// buffer to what it read and the *capacity* has to stay at [`LINE_SCRATCH_BYTES`] for the buffer to
    /// be reusable. Slicing by `len()` would be right once and wrong after the next append, and the two
    /// are indistinguishable at the call site.
    page_used: usize,
    /// What the last [`Session::paint`] did, kept whole. Phase 12.
    ///
    /// `paint` folds three of `PaintStats`' fields into `SessionStats` as running totals and would drop
    /// the rest on the floor. The dropped ones are exactly the ones a gate needs —
    /// [`PaintStats::doc_glyphs`](holonomy_display::paint::PaintStats::doc_glyphs),
    /// `runs_missing`, `runs_truncated` — because they say what is *on the page now* rather than what
    /// has been drawn over the session's life.
    last_paint: holonomy_display::paint::PaintStats,
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
    /// Reusable buffer for the compiled formula's runs.
    ///
    /// Sized once and cleared per paint, for the reason [`Session::table_scratch`] exists and stated
    /// at greater length there: a local `MathLayout` would be promoted to the heap by the compiler and
    /// the allocation would then happen once per paint rather than once per session -- invisible in a
    /// rate, visible in a count. `tests/session_math.rs` asserts this field's capacity is unchanged
    /// across a compile.
    math_scratch: MathLayout,
    /// Page-column-width rasters for the pages near the viewport. Phase 9C.
    ///
    /// Owns every decoded pixel in the process, which is why it is here and not in the painter: the
    /// budget §2.9.4 charges 8.0 MiB to is this field's, and a gate asserts it through
    /// [`Session::image_cache_bytes`].
    ///
    /// A direct field rather than behind the `RasterSource` trait object the painter holds, because the
    /// painter borrows it *per frame* -- the cache is mutated between frames and the painter must not
    /// hold a borrow across a mutation. `Painter::paint_with_rasters` takes it per call for exactly
    /// that reason.
    images: IcebergCache,
    /// The native-size buffer a PNG decodes into, reused across decodes.
    ///
    /// **Transient, and deliberately outside the cache's accounting** -- the same bargain
    /// `holonomy_image::scale` makes for its horizontal-pass intermediate, and for the same reason: a
    /// 1920x1080 source needs 8.3 MB to decode into, it is freed before the frame is presented, and
    /// counting it against the 8.0 MiB image budget would double-count a buffer that never coexists
    /// with the resident rasters it feeds.
    ///
    /// A field rather than a local because a local `Vec` passed to `decode` **escapes** and is promoted
    /// to the heap, which is the same trap [`Session::table_scratch`] documents: the allocation would
    /// then happen once per decode rather than once per session -- invisible in a rate, visible in a
    /// count.
    image_decode_scratch: Vec<u8>,
    /// The page-column-width buffer a resample produces, reused across decodes.
    ///
    /// A field for the same reason as `image_decode_scratch`, and this one *is* resident: its contents
    /// are handed to `IcebergCache::insert`, which copies them into a `SecureBlock`. So it is a
    /// transient copy of a resident buffer, and it is sized to one raster.
    image_resample_scratch: Vec<u8>,
    /// The page column's width in pixels: the width every raster is downscaled to. Phase 9C, §2.9.3.
    image_column_width: u32,
    /// The compiled source a formula is compiled from, reused across paints.
    ///
    /// Separate from `math_scratch` because it is a different lifetime: this is the *input* to
    /// `math::parse`, which allocates an AST, and the AST is dropped at the end of each paint. The
    /// buffer avoids re-allocating the string; the AST itself is one allocation per formula per paint,
    /// which `math.rs`'s header argues is acceptable and which the zero-allocation requirement is
    /// explicitly about the *layout*, not the parse.
    math_source: Vec<u8>,
}

impl<'a> Session<'a> {
    /// A session over `editor`, painting through `painter`, presenting to `scanout`.
    pub fn new(
        editor: Editor,
        painter: Painter<'a>,
        scanout: Box<dyn Scanout>,
        metrics: ChromeMetrics,
    ) -> Self {
        Self::new_with(
            editor,
            painter,
            scanout,
            metrics,
            &mut crate::store::NoSource,
        )
    }

    /// Replace the editor with an opened one, and rebuild everything derived from the document.
    ///
    /// # Why this exists rather than assigning `session.editor = opened.editor`
    ///
    /// **Because that assignment is the bug.** A `Session` is built before the passphrase exists -- it has
    /// to be, because it is built before `seccomp` -- so it is constructed over `Editor::new()`, and the
    /// container's document is dropped in afterwards. `DocLines`, `Manifest` and `TextCounts` were all
    /// built from the **empty** editor at construction and were never rebuilt, so `total_lines` stayed 1
    /// and `emit_body_text` drew a single empty line.
    ///
    /// **It was invisible for the same reason it is easy to reintroduce**: `Editor::new()` and an opened
    /// editor are both `Editor`, the field is public, and every in-memory gate assigns documents through
    /// `Editor::insert_at` on a session that was built with the document already in it. **No gate was
    /// ever in the state this product is in**, which is constructed empty and given a document later.
    ///
    /// So the field is private now, and this is the only way in. **The whole-document read happens here**,
    /// once, through `bytes` -- which is why the two are one parameter rather than two steps a caller
    /// could get wrong.
    pub fn adopt_document(
        &mut self,
        mut editor: Editor,
        bytes: &mut dyn holonomy_text::LeafSource,
    ) -> Result<(), crate::store::StoreError> {
        let mut text = editor
            .text_faulting(bytes)
            .map_err(|_| crate::store::StoreError::Read)?;
        self.lines = DocLines::build(&text, holonomy_geometry::LineMetrics::default());
        self.manifest = crate::manifest::Manifest::from_text(&text);
        self.counts = TextCounts::scan_bytes(&text);
        text.fill(0);

        self.editor = editor;
        self.state.words = self.counts.words;
        self.state.total_lines = self.counts.lines();
        self.state.bytes = self.editor.text_len() as u32;
        self.state.scroll_line = 0;
        self.caret_drawn_at = None;
        // **A full repaint, not a damage-limited one.** The frame holds the *previous* document's
        // pixels and there is no relationship between them and this one's, so a damage-limited pass
        // would leave the old document's words on screen with the new one's geometry.
        self.damage = self.chrome.full_damage();
        Ok(())
    }

    /// [`new`](Self::new) against a document whose bytes may not be resident. **The product's constructor.**
    ///
    /// # Why this exists, and the failure it fixes is worth the whole paragraph
    ///
    /// `new` built three things from the document's bytes -- [`DocLines`], [`Manifest`] and
    /// [`TextCounts`] -- and each read the **whole** document through `&self`. On a container-backed
    /// document larger than the resident window that read returns `LeafAbsent`, and the code's
    /// `unwrap_or_default()` turned it into **an empty geometry over a non-empty document**.
    ///
    /// **The symptom was a session that reported one line and painted no body text**, on a document that
    /// was in fact 450 KB of text sitting in the container. Every in-memory gate passed, because on an
    /// in-memory editor the whole-document read always succeeds -- so the entire test suite could be green
    /// and the product could not open a document at all. `tests/session_open_document.rs`'s
    /// `text_past_the_first_window_is_drawn_not_counted` is the gate, and it is the *first* test in this
    /// project that could have caught it, because it is the first one whose document does not fit in the
    /// window.
    ///
    /// **The failure was silent because a default is a plausible answer.** An empty `DocLines` is a valid
    /// `DocLines`; it is only wrong in combination with a non-empty `text_len`, and nothing checked that
    /// combination.
    pub fn new_with(
        mut editor: Editor,
        painter: Painter<'a>,
        scanout: Box<dyn Scanout>,
        metrics: ChromeMetrics,
        bytes: &mut dyn holonomy_text::LeafSource,
    ) -> Self {
        // # The line pitch comes from the faces, not from the constant
        //
        // `ChromeMetrics::DESKTOP.cell_h` is 18, which is `16 ppem + 2` -- an arithmetic identity with
        // nothing to do with the fonts. The packed faces need 20 px (Inter), 22 (JetBrains Mono) and
        // 24 (Noto Sans Math) of ascent-plus-descent at 16 ppem, so an 18 px line box cannot contain
        // their ink at *any* baseline placement. Every caller here -- `publish_line_heights`,
        // `emit_math`, the table grid, the damage rects -- positions and damages by `cell_h`, so the
        // one place that can set it correctly from real metrics is where the session first meets the
        // atlas it will paint from.
        //
        // The constant stays 18 as the **no-atlas** fallback, which is the same value
        // `Painter::cell_height` falls back to. A chrome-only frame draws procedural box-drawing
        // glyphs sized to the cell, so it needs the fallback to be self-consistent, not
        // typographically correct -- and the two agreeing is the point.
        //
        // Part 32 adds the second half. `with_line_pitch` only *raised* bands to fit the type; it left
        // the type at 16 ppem and the measure at `cell_w = 8`, so rasterising at a larger ppem grew the
        // glyphs into bands that did not grow with them. `with_type_scale` scales every dimension that
        // describes type, from the same measured pitch, and leaves the furniture alone.
        let metrics = match painter.atlas().map(|a| a.line_pitch()).filter(|&p| p > 0) {
            Some(pitch) => metrics
                // **The cell from the face, and not the 8 the constant carries.** `cell_width()` reads
                // the Monospace face's own advance out of the atlas -- 10 at 16 ppem, 14 at 24 -- which
                // is the measure's true unit and was 20 % narrow before part 32.
                .with_cell_width(painter.cell_width())
                .with_type_scale(u32::from(pitch))
                .with_line_pitch(u32::from(pitch)),
            None => metrics,
        };
        let chrome = Chrome::new(metrics);
        publish_atlas(painter.atlas(), painter.size_index());
        let frame = Frame::black(metrics.width, metrics.height);
        // Built before `editor` moves into the struct below, since it reads the document.
        //
        // From the *document*, not from an empty one. A rebuild is `O(document)` and `LineGeometry`'s
        // docs call it the cost of typing a newline; it must not also be the cost of the *first*
        // keystroke in a session, which is exactly what happens if the geometry starts as an empty
        // document's -- the first `sync_lines` sees a line-count mismatch and rebuilds.
        //
        // **One read at open, shared.** `DocLines` and `Manifest` are both built from the document's bytes
        // and both need all of them, so they share a single `editor.text()`. This is the *open* path, so
        // the whole-document read is correct here: it happens once, before the first frame, and it is what
        // lets neither structure ever need it again.
        //
        // **One read, faulting, shared by all three.** `DocLines` and `Manifest` both need the whole
        // document and `TextCounts::scan` wants to walk it too, so this is the one place that pays for
        // it. `text_faulting` rather than `text` is the part 16 change.
        let (lines, manifest, counts) = {
            let mut text = editor.text_faulting(bytes).unwrap_or_default();
            let built = (
                DocLines::build(&text, holonomy_geometry::LineMetrics::default()),
                crate::manifest::Manifest::from_text(&text),
                TextCounts::scan_bytes(&text),
            );
            // **Zeroed before it drops.** This is the entire document in plaintext and the block is
            // about to end, handing the page back to the allocator where a later allocation could read
            // it. Doing it here rather than behind a `Drop` keeps it visible at the one place that
            // holds the bytes.
            text.fill(0);
            built
        };
        // **The cross-check that would have caught the bug above.** A document with bytes has at least one
        // line, so `total_lines == 1 && text_len > 0` is the exact shape of "the whole-document read
        // failed and the default stood in". It is a `debug_assert` rather than a hard one because a
        // source that genuinely cannot produce bytes should degrade to an empty view rather than refuse
        // to start -- but **the product path is covered**, so in a release build the pair is only
        // visible through `total_lines()`, which `tests/session_open_document.rs` asserts on.
        debug_assert!(
            counts.lines() > 0 || editor.text_len() == 0,
            "a document with bytes has lines: total_lines={} text_len={}",
            counts.lines(),
            editor.text_len()
        );
        let state = ChromeState {
            // The document's first line is on screen at the caret's line.
            scroll_line: 0,
            // **The document's real totals, not `Default`'s.** `ChromeState::default()` has
            // `total_lines: 1` and `words: 0`, which is right for an empty document and wrong for every
            // other one -- and it stayed wrong until the first edit, because `publish_counts` runs from
            // `after_edit` and nothing called it at construction.
            //
            // Phase 12 made that visible for the first time: `emit_body_text` clamps its line loop to
            // `total_lines`, so a session opened on a three-line document drew **one** line and stopped.
            // Before Phase 12 nothing read `total_lines` for drawing, only for the status bar, so the bug
            // was a status bar reading "1 line" on a document with three -- wrong, and unnoticed, because
            // every gate started from an empty document or made an edit first.
            words: counts.words,
            total_lines: counts.lines(),
            bytes: editor.text_len() as u32,
            ..ChromeState::default()
        };
        let mut s = Self {
            editor,
            // **0, and the chrome's `styles` is `StyleFlags::default()`** — two copies of "nothing is
            // armed", and `armed_style_is_zero_on_a_fresh_session` is what says they are not drifting.
            pending_style: None,
            chrome,
            state,
            frame,
            scanout,
            painter,
            keymap: Keymap::us(),
            mods: ModifierState::new(),
            page_drag: false,
            pending_colour: None,
            blink: Blink::new(Blink::DEFAULT_PERIOD),
            stats: SessionStats::default(),
            damage: DamageRect::EMPTY,
            active_cell: None,
            tables_shape_dirty: false,
            // Built from the document; see the comment at the `DocLines::build` call above.
            lines,
            manifest,
            manifest_scratch: vec![0u8; crate::manifest::SECTION_BYTES],
            counts,
            doc_scratch: Vec::new(),
            // Allocated once at construction, not on first use: the first paint happens during
            // construction's caller, and a `Vec` that grows on the paint path is one allocation per
            // growth on a path that paints per keystroke. See the field's docs for why this is separate
            // from `doc_scratch`.
            line_scratch: vec![0u8; LINE_SCRATCH_BYTES],
            page_used: 0,
            last_paint: holonomy_display::paint::PaintStats::default(),
            table_scratch: Vec::new(),
            caret_drawn_at: None,
            math_scratch: MathLayout::with_capacity(MATH_RUN_CAPACITY),
            math_source: Vec::new(),
            images: IcebergCache::new(),
            image_decode_scratch: Vec::new(),
            image_resample_scratch: Vec::new(),
            // The page column is `metrics.width` minus the chrome's side bands, and `Layout::text`
            // has already worked it out. 640 is §2.9.3's figure for an 80-column page at a 1280 px
            // window, and `max(1)` keeps a degenerate layout from producing a zero-width resample --
            // which would be a division by zero inside `holonomy_image::scale::axis_map`.
            image_column_width: u32::max(chrome.layout.text.width, 1),
        };
        // **The chrome's caret is reconciled with the editor's, and this was a real desync.**
        //
        // `state` above is `..ChromeState::default()`, so `caret_line` and `caret_column` were 0 — while
        // `editor.caret()` is wherever the document builder left it, which for every gate that seeds a
        // document with `insert_at` is **the end of the document**. So a session opened on a three-line
        // document reported "line 0, column 0" and **drew the caret at the top-left of the page** while
        // the model said the end of the last line.
        //
        // **It was invisible for twelve phases because `caret_column` was a byte count and `Caret::locate`
        // multiplied it by `cell_w`; both were 0, so the arithmetic agreed with itself.** Part 22 started
        // computing the column from the editor's actual caret and it surfaced immediately, as
        // `a_space_advances_the_pen_rather_than_stacking_glyphs` failing by exactly one cell on `"a a"`
        // against `"aa"` — **the caret ink at the end of one line and not the other.** That gate was
        // measuring ink extent and found a caret it had never been able to see.
        //
        // **Reconcile, and do not move.** Part 28.
        //
        // **It used to be `caret_to(s.editor.caret())`,** which is the same reconciliation *plus* a
        // caret move -- and a move, as of part 28, scrolls the page to follow the caret. Since
        // `insert_at` leaves the caret at the end of the document, **a new session opened scrolled to
        // its last line.** `scrolling_moves_the_ink_to_the_row_the_geometry_says` caught it as a first
        // page of 63 glyphs against 66: the first page had quietly become the last.
        //
        // **`reconcile_caret`, which is the tail of `caret_to` without the move.** The editor's caret is
        // where it is and the chrome now agrees with it -- part 22's property, unchanged -- while the
        // page stays at the top, which is what opening a document means.
        s.reconcile_caret()
            .expect("reconciling at the editor's own position cannot fail");
        s
    }

    /// Capacity of the reusable run buffer, so a gate can assert it did not grow.
    ///
    /// Read-only, and for the same reason [`Session::damage`] is: the zero-allocation requirement is
    /// about the *count* of allocations, and a count can only be asserted through an accessor. Reading
    /// `runs.capacity()` from outside would mean exposing the buffer itself.
    pub fn math_run_capacity(&self) -> usize {
        self.math_scratch.runs.capacity()
    }

    /// The formula the caret is inside, if it is inside one.
    ///
    /// Read-only and derived, like [`Session::active_cell`]: the answer comes from scanning the
    /// document's bytes for `$$` pairs, so there is no stored state that could disagree with the text.
    /// A gate can ask "is the caret in a formula, and what is its LaTeX?" without the session being
    /// able to be told the answer.
    pub fn active_math(&self) -> Option<MathSpan> {
        let caret = self.editor.caret();
        let text = self.editor.text().ok()?;
        holonomy_text::math_span_at(&text, caret)
    }

    /// The LaTeX of the formula the caret is in.
    ///
    /// For a gate that wants to assert what was typed. Returns `None` when the caret is not in a
    /// formula, which is the ordinary case.
    pub fn active_math_source(&self) -> Result<Option<Vec<u8>>, SessionError> {
        let Some(span) = self.active_math() else {
            return Ok(None);
        };
        let text = self.editor.text()?;
        Ok(Some(text[span.inner()].to_vec()))
    }

    /// Insert an empty formula at the caret and put the caret inside it.
    ///
    /// This is `Ctrl+M`. It writes **four** bytes -- `$$$$` -- and leaves the caret at `start + 2`.
    ///
    /// **Four, not two.** Two would give the user an opening delimiter and no way to see where the
    /// formula ends; four makes the span exist the instant the key is pressed, so `Ctrl+M` then `\sqrt
    /// {x}` then Right shows a compiled radical rather than a `$$` that does nothing until someone
    /// remembers to close it. The alternative -- two bytes and a "closed by typing the next `$$`"
    /// rule -- makes the first keystroke's effect depend on a rule the user has not been told.
    ///
    /// The caret lands between the delimiters rather than after them, because a person who has just
    /// asked for a formula wants to type one. That is the same argument as
    /// [`Session::insert_table`]'s first-cell placement, and it is the reason this method moves the
    /// caret at all rather than inserting and returning.
    pub fn insert_math(&mut self) -> Result<(), SessionError> {
        let at = self.editor.caret() as usize;
        self.editor
            .insert_at(at as u32, b"$$$$", SpanPolicy::GrowIntoInsert)?;
        self.stats.math_inserts += 1;
        // **Phase 11 item 4: `TextCounts` is folded here too, and that is a bug this caught.** This
        // path called `editor.insert_at` directly rather than going through `Session::insert`, so the
        // word and line totals silently stopped tracking the document -- and the failure surfaced
        // later as an *underflow* in a delete, several keystrokes afterwards, in a different function
        // entirely. Every path that mutates the document must fold the counts; a new one that forgets
        // will fail the same way, which is why `tests/session_counts.rs` drives all of them.
        self.counts.after_insert(&self.editor, at, b"$$$$");
        self.editor.caret_to(at + 2)?;
        self.after_edit(4)
    }

    /// Insert an image at the caret, from the built-in chart.
    ///
    /// # Where the bytes come from, and why that is stated rather than hidden
    ///
    /// From a PNG committed to this repository and `include_bytes!`-ed. **Not** from a file the user
    /// chose, and not from the network, because FR-5.1's `unshare(CLONE_NEWNET)` forbids the second and
    /// the sealed 50-syscall allowlist forbids the first: there is no `openat` on a user-chosen path in
    /// the jail. A real "insert image from disk" is therefore a Phase 13 question, where §4's windowed
    /// document model brings a read path with it.
    ///
    /// Everything *else* is real end to end, and it is the part worth testing: a keystroke becomes a
    /// U+FFFC anchor, a BLAKE2b content address, a catalog entry in the payload's tail, a PNG decode, a
    /// fixed-point downscale to page-column width, a `SecureBlock` in the Iceberg cache, a
    /// `Node::Image`, an integer blit, a displacement of every line below it, and -- in
    /// `holonomy-export` -- a `<img>` and an `/XObject`. The only thing stubbed is the file dialog.
    ///
    /// The chart is 1920x1080 on purpose. §2.9.3's scaler runs on *every* image precisely because the
    /// cache holds page-column-width rasters, so a fixture at or below the column width would never
    /// exercise it at all.
    pub fn insert_image(&mut self) -> Result<(), SessionError> {
        self.insert_image_bytes(TEST_CHART_PNG)
    }

    /// Insert `png` as an image at the caret.
    ///
    /// The same path as [`insert_image`](Self::insert_image) with the bytes supplied, so a test can use
    /// a fixture of its own and a future file picker can use the same code without either being a
    /// special case.
    pub fn insert_image_bytes(&mut self, png: &[u8]) -> Result<(), SessionError> {
        let at = self.editor.caret();
        // `Editor::insert_image` writes the catalog entry *before* the anchor, so a malformed PNG
        // leaves nothing behind rather than an anchor that nothing serves.
        self.editor.insert_image(at, png)?;
        self.stats.image_inserts += 1;
        // Rescan rather than deltify: `Editor::insert_image` writes an anchor whose bytes are not
        // returned to the caller, and `TextCounts` needs them. One rescan for one Ctrl+Image is the
        // right trade -- see `Session::undo` for the same argument.
        self.counts = TextCounts::scan(&self.editor);
        // The caret moves past the anchor, so the next keystroke types after the image rather than
        // inside it. `insert_at` already moved it; this makes the intent explicit and survives a
        // change to how `insert_at` places the cursor.
        self.editor.caret_to(at as usize + ANCHOR_BYTES.len())?;
        // The *whole page* is invalidated, not one line: an image is hundreds of pixels tall and
        // `after_edit` damages `cell_h` by construction, which would leave the image's own rows stale.
        self.damage = self.chrome.full_damage();
        self.after_edit(ANCHOR_BYTES.len() as u32)
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
        // Rescan rather than deltify, for the reason `insert_image_bytes` does: `Editor::insert_table`
        // does not hand back the separator bytes it wrote, and `TextCounts` needs them.
        self.counts = TextCounts::scan(&self.editor);
        self.tables_shape_dirty = true;
        self.enter_cell(span, 0, 0, 0)?;
        self.after_edit(0)
    }

    /// What the last paint did, from the painter's side. Phase 12.
    ///
    /// **Separate from [`SessionStats`], which accumulates.** `SessionStats::pixels` and `glyphs_drawn` are
    /// totals across the session's life; `PaintStats` is one frame. A document with text on it answers
    /// different questions under each — "how much has this session drawn" and "what is on the screen right
    /// now" — and Phase 12's gate needs the second. `Session::paint` folds most of it into `SessionStats` and
    /// would otherwise drop the rest, so this keeps the whole struct.
    pub fn paint_stats(&self) -> &holonomy_display::paint::PaintStats {
        &self.last_paint
    }

    /// The page's geometry, for a caller that wants to look at it rather than at the whole chrome.
    ///
    /// `Chrome::layout` is a public field on a public field, so this is a convenience rather than a necessity
    /// — but `layout.text` is four chained field accesses, and a gate that measures ink inside the text
    /// column writes it nine times.
    pub fn text_rect(&self) -> DamageRect {
        self.chrome.layout.text
    }

    /// The document's section manifest. Phase 13.
    ///
    /// **Public so the gate can ask it the question four emitters ask.** [`crate::manifest::Manifest::span_total`]
    /// answers *"does this document contain a formula, a table or an image?"* without reading it, which is the
    /// capability Phase 12 named as the blocker it could not remove — and `tests/session_manifest.rs` holds
    /// both directions down: prose must not allocate the whole-document buffer, and a document with a formula
    /// must.
    pub fn manifest(&self) -> &crate::manifest::Manifest {
        &self.manifest
    }

    /// How many bytes of the page buffer the last body-text emit read. Phase 12.
    ///
    /// **Public so the gate can assert the emitter reads a page and not a document** -- the claim
    /// `the_body_text_emitter_reads_one_page_and_not_the_document` makes, and the one Phase 12's memory
    /// story rests on.
    pub fn page_used(&self) -> usize {
        self.page_used
    }

    /// Capacity of the body-text buffer, in bytes. Phase 12.
    ///
    /// **Public so the gate can assert it stays page-sized.** `tests/session_body_text.rs` paints a 1 MiB
    /// document and then checks this is still [`LINE_SCRATCH_BYTES`] — because a buffer that grows on the
    /// paint path is one allocation per paint, and RSS is `tests/session_rss.rs`'s business while this is the
    /// narrower claim that Phase 12 actually changed.
    pub fn line_scratch_capacity(&self) -> usize {
        self.line_scratch.capacity()
    }

    // ---------------------------------------------------------------------------
    // Read access to the document
    //
    // **These exist because `editor` is private, and the privacy is the point.** `session.editor =
    // opened.editor` compiled, ran, and left a session whose `DocLines`, `Manifest` and `TextCounts`
    // described an *empty* document — the product painted one blank line over a real file for its entire
    // life. See [`adopt_document`](Self::adopt_document).
    //
    // **Reads stay open.** A reader cannot leave the session inconsistent; a writer can and did. So the
    // field is closed and every use of it is a named accessor, which is also what lets each one say why
    // it is safe.
    // ---------------------------------------------------------------------------

    /// The document's bytes, for a caller that needs the whole thing — the export path, the window's
    /// title, a gate.
    ///
    /// **A read accessor rather than a public field, and that is the difference between the two.**
    /// [`adopt_document`](Self::adopt_document) exists because *assigning* the field left the geometry
    /// stale; reading it is harmless, and making it private would only have pushed every reader to add a
    /// method for the same thing. **The asymmetry is deliberate: reads open, writes are mediated.**
    pub fn text(&self) -> Result<Vec<u8>, holonomy_text::EditorError> {
        self.editor.text()
    }

    /// The document's length in bytes.
    ///
    /// **`u32`, not `Result`**, because `Editor::text_len` cannot fail — the rope's length is arithmetic
    /// over leaf lengths and every leaf has one whether resident or not. An earlier draft of this
    /// accessor returned `Result` and the compiler found four call sites doing arithmetic on it.
    pub fn text_len(&self) -> u32 {
        self.editor.text_len() as u32
    }

    /// The editor, mutably — for a gate that needs to drive the document directly.
    ///
    /// # This is an escape hatch, and it is narrow on purpose
    ///
    /// **Editing through it is fine. Replacing the document through it is what broke the product**, and
    /// that is `adopt_document`'s job because it rebuilds the three structures that describe the bytes.
    ///
    /// So the rule is: **mutate through this, never assign.** Every mutation goes through
    /// [`Editor`](holonomy_text::Editor)'s own paths, which record into the edit record and update the
    /// geometry -- the session's `after_edit` then reconciles what a raw editor call cannot. A gate that
    /// types into a document needs this; no production path does, which is why it is not used by one.
    pub fn editor_mut(&mut self) -> &mut Editor {
        &mut self.editor
    }

    /// The editor, for reading — **and part 24 is why this exists.**
    ///
    /// **A gate could not previously ask the session what styling it held.** `editor_mut` was the only
    /// accessor and it needs `&mut self`, so the only way to look was to borrow mutably and give the
    /// borrow back, which reads as a gate that wants to change something when it wants to read something.
    ///
    /// **Read-only, and the read-only part is the point.** [`Editor::spans`] hands out `&SpanMap`, and
    /// `SpanMap` has no interior mutability on its read paths — `style_at` and `runs_in` are `&self` — so
    /// this accessor cannot corrupt the session by looking. That is the same property
    /// [`editor_mut`](Self::editor_mut)'s own note gives for read access, stated here because this is the
    /// accessor that makes it true.
    pub fn editor(&self) -> &Editor {
        &self.editor
    }

    /// The caret's byte offset.
    pub fn caret(&self) -> u32 {
        self.editor.caret()
    }

    /// The caret's line and column, for a status bar and for a driver's hit testing.
    ///
    /// **Two accessors rather than one `Option<(u32, u32)>`,** because both halves are always
    /// available and a pair of `u32`s that can be `None` is a `None` that is never `Some`. A gate
    /// asserting a click landed on row 1 column 3 should not have to unwrap anything, and neither
    /// should a driver that wants to draw its own cursor.
    pub fn caret_line(&self) -> u32 {
        self.state.caret_line
    }

    /// The caret's column within its line. See [`Session::caret_line`].
    pub fn caret_column(&self) -> u32 {
        self.state.caret_column
    }

    /// The chrome's computed layout.
    ///
    /// **Public because a driver needs it to turn panel coordinates into something it can act on**,
    /// and part 20's `hit_rect` and `caret_at` are only half a feature without a way to ask where
    /// anything is. It is a copy of a `Copy` struct, so a caller that wants it per event pays
    /// thirty-two bytes rather than a heap allocation.
    pub fn chrome_layout(&self) -> holonomy_render::chrome::Layout {
        self.chrome.layout
    }

    /// The document's images, in document order.
    pub fn assets(&self) -> &holonomy_text::AssetCatalog {
        self.editor.assets()
    }

    /// The byte offset of every image anchor, in document order.
    ///
    /// **A `Result`, because the editor scans the document for the anchor bytes** and that scan is a
    /// whole-document read. On a container-backed document it needs a source, which is why this is a
    /// gate-level accessor and not something the paint path uses — `emit_images` goes through the
    /// manifest instead, which is O(styled runs).
    pub fn image_anchors(&self) -> Result<Vec<u32>, holonomy_text::EditorError> {
        self.editor.image_anchors()
    }

    /// How many leaves currently hold their bytes.
    pub fn leaf_count(&self) -> usize {
        self.editor.leaf_count()
    }

    /// How many lines the document has, as the geometry counts them.
    ///
    /// **The geometry's answer, not a scan.** `DocLines` is built at open for the whole document from
    /// offsets alone, so this is O(1) and available before a single byte has been read. That is what
    /// makes "scroll to the end" usable on a document whose bytes are not resident -- a line count that
    /// needed the bytes would defeat the point of windowing.
    pub fn total_lines(&self) -> u32 {
        self.state.total_lines
    }
    /// Scroll so that `line` is the first visible line. Phase 12's gate.
    pub fn scroll_to(&mut self, line: u32) -> Result<(), SessionError> {
        self.state.scroll_line = line.min(self.state.total_lines.saturating_sub(1));
        self.damage = self.chrome.full_damage();
        Ok(())
    }

    /// Capacity of the whole-document scratch buffer, in bytes.
    ///
    /// **Public as of Phase 11** so the RSS gate can name this consumer. It is the largest single
    /// allocation outside the framebuffer and the leaves, and §2.9.4's table did not have it at all —
    /// which is the same class of omission as the missing 3.906 MiB framebuffer, found the same way:
    /// by measuring instead of adding up. See [`Session::doc_scratch`] for what it is for and when it
    /// goes away.
    pub fn doc_scratch_capacity(&self) -> usize {
        self.doc_scratch.capacity()
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
        self.paint_with(damage, &mut crate::store::NoSource)
    }

    /// [`repaint_all`](Self::repaint_all) against a byte source. **The product's first paint.**
    ///
    /// **The `caret_drawn_at` reset is copied from `repaint_all` rather than shared, and it matters.**
    /// That field is the "the caret is already where I left it" cache; a full repaint erases the caret with
    /// the page's text, so the cache is a lie until it is cleared. Dropping the line here would make the
    /// first paint of a freshly opened document skip the caret blink -- a small bug, and exactly the kind
    /// that a copy-paste shim introduces.
    pub fn repaint_all_with(
        &mut self,
        bytes: &mut dyn holonomy_text::LeafSource,
    ) -> Result<(), SessionError> {
        let damage = Some(self.chrome.full_damage());
        self.caret_drawn_at = None;
        self.paint_with(damage, bytes)
    }

    /// Run `source` to exhaustion, or until Ctrl+Q.
    ///
    /// Returns why it stopped. Every command is counted, so a stream that produced nothing shows up
    /// as `stats.commands == 0` rather than as silence.
    pub fn run(&mut self, source: &mut dyn InputSource) -> Result<Exit, SessionError> {
        self.run_with(&mut crate::store::NoSource, source)
    }

    /// [`run`](Self::run) over a document whose bytes may not be resident.
    ///
    /// **This is the product's loop.** `run` is the same loop with a
    /// [`NoSource`](crate::store::NoSource), which is correct for an in-memory editor and cannot fetch.
    ///
    /// The name `run_with` rather than a parameter on `run` is deliberate: an input source is what the
    /// loop *reads*, and a byte source is what it *reads the document through*. Overloading one name for
    /// two unrelated roles is how a caller ends up passing the wrong one.
    pub fn run_with(
        &mut self,
        bytes: &mut dyn holonomy_text::LeafSource,
        source: &mut dyn InputSource,
    ) -> Result<Exit, SessionError> {
        // **The loop branches on the event's kind, and that is the whole of what part 20 changed
        // here.** A key goes to `handle_event_with`, which folds the modifier state and dispatches
        // through the keymap. A pointer event goes to `handle_pointer`, which hit-tests against the
        // chrome. **They share the `tick_with` afterwards**, because a hover highlight and a keystroke
        // both need the frame's damage drained and both need the blink advanced -- which is the
        // existing reason that call is outside the branch and not inside each arm.
        while let Some(event) = source.next_event().map_err(session_io)? {
            match event {
                holonomy_input::Event::Key(k) => {
                    if let Some(exit) = self.handle_event_with(bytes, k)? {
                        return Ok(exit);
                    }
                }
                pointer => {
                    self.handle_pointer(bytes, pointer)?;
                }
            }
            // The blink may want a repaint even with no input.
            self.tick_with(bytes)?;
        }
        Ok(Exit::StreamEnded)
    }

    /// Fold one pointer event into the chrome and act on it.
    ///
    /// # What this is, structurally
    ///
    /// **Three cases, in the order the hardware produces them**: a motion event moves the pointer and
    /// recomputes what is under it; a press marks the widget and acts; a release clears the mark. A
    /// wheel is a fourth and it does not go through the widget list at all -- it scrolls, which is a
    /// property of the document rather than of a control.
    ///
    /// # Why motion damages two rects and not one
    ///
    /// Moving the pointer changes *two* things on screen: the widget it left stops being hovered, and
    /// the one it arrived at starts. Damaging only the new one leaves a button lit under a pointer
    /// that is somewhere else, **and that stale highlight is permanent** because nothing else will
    /// ever invalidate it. So the damage is the union of the old hover's rect and the new one's, which
    /// is why [`widgets::hit_rect`] exists: without the inverse of `hit`, the only way to invalidate a
    /// widget you have just left is to repaint the whole panel, at 1,024,000 pixels, on every mouse
    /// movement.
    pub fn handle_pointer(
        &mut self,
        bytes: &mut dyn holonomy_text::LeafSource,
        event: holonomy_input::Event,
    ) -> Result<(), SessionError> {
        use holonomy_input::Event as P;
        self.stats.pointer_events += 1;
        let Some((x, y)) = event.position() else {
            // A key, which cannot reach here: `run_with` branches before calling. **And `position()`
            // returning `None` for a key is the assertion that they are different kinds of thing.**
            return Ok(());
        };

        match event {
            P::Motion { .. } => {
                // **A drag in progress extends the selection, before anything else here.** Part 28.
                //
                // **First, and outside the "nothing changed" early return below**, because that return
                // is about hover and cursor position and a drag moves the pointer without changing
                // either: dragging along one row of text keeps `hover` at `Page` and — after the
                // first event — `cursor` at the new x, so the early return is false; but the *first*
                // motion of a drag can land on the pixel the press was on, and then both are equal and
                // the return fires. **A selection that does not grow on the event where the pointer has
                // not yet moved is not a selection that can be dragged at all.**
                //
                // **And it returns, because hover is not what this event is for.** The pointer is
                // captured by the drag: highlighting a toolbar button the user is dragging past would
                // be a lie about what is being pressed. `tick_with` below still runs.
                if self.page_drag {
                    self.drag_to(x, y, bytes)?;
                    self.tick_with(bytes)?;
                    return Ok(());
                }
                let before = self.state.hover;
                // **`hover_at` returns `Hit`, and `Hit::None` becomes `None` here rather than being
                // stored as a `Some(Hit::None)`.** The `Option` answers "is the pointer over
                // anything", and a `Some(Hit::None)` would make every caller that compares against
                // `None` wrong in a way no type checker catches -- `state.hover == None` would be
                // false while the pointer was over nothing, and the emitters would all draw hover
                // states for nothing.
                let after = match self.hover_at(x, y) {
                    widgets::Hit::None => None,
                    h => widgets::hit_rect(&self.chrome.layout, &self.state, h).map(|_| h),
                };
                if before == after && self.state.cursor == Some((x, y)) {
                    return Ok(());
                }
                self.state.cursor = Some((x, y));
                self.state.hover = after;
                for h in [before, after].into_iter().flatten() {
                    if let Some(r) = widgets::hit_rect(&self.chrome.layout, &self.state, h) {
                        self.invalidate(r);
                    }
                }
                // **The popup, in full, whenever a menu is open.** Hover inside a popup changes one
                // row; hover *outside* one closes it, and closing it redraws the popup and the
                // heading it was hanging from. Neither is inside a widget's rect, so neither is
                // covered by the loop above.
                if let Some(open) = self.state.open {
                    let l = self.chrome.layout;
                    if let Some(pop) = widgets::popup(&l, &self.state, Some(open)) {
                        self.invalidate(pop.rect);
                    }
                    // **The opener is invalidated as well as the popup**, because a popup that closes
                    // takes its opener's highlight with it. Damaging only the popup would leave the
                    // heading lit with nothing drawn over it.
                    self.invalidate(opener_rect(&l, open));
                }
            }
            P::Wheel { dy, .. } => {
                // **Only over the page.** A wheel over a popup should scroll the popup, which does not
                // scroll, and a wheel over the sidebar should scroll the sidebar, which does not
                // either. Scrolling the document under an open popup would move the thing the user is
                // reading while the menu they are choosing from stays put.
                let over_page =
                    widgets::hit(&self.chrome.layout, &self.state, x, y) == widgets::Hit::Page;
                if over_page && dy != 0 {
                    self.scroll_by(i64::from(dy) * 3);
                }
            }
            P::Button {
                button: holonomy_input::Button::Left,
                pressed: true,
                ..
            } => {
                self.stats.pointer_presses += 1;
                let hit = self.hover_at(x, y);
                self.state.pressed = (hit != widgets::Hit::None).then_some(hit);
                // **Arm the drag before `press` runs**, because `press` is what plants the anchor and
                // the flag has to be set by the time the pointer starts moving. Only a press that
                // landed *on the page* arms it: a press on a toolbar button that the user then drags
                // back over the page must not start selecting text.
                self.page_drag = hit == widgets::Hit::Page;
                self.press(hit, x, y, bytes)?;
            }
            P::Button {
                button: holonomy_input::Button::Left,
                pressed: false,
                ..
            } => {
                // **The release clears `pressed` and does nothing else.** A click is decided on the
                // *press*, not the release, and that is what makes a drag off a button not fire it --
                // the button is pressed when the pointer is on it, `pressed` is cleared by the
                // release wherever it happened, and the command was already applied. **A
                // release-driven model would need a drag threshold**, and a threshold is a guess about
                // how far is far enough; deciding on the press needs no guess.
                self.state.pressed = None;
                // **The drag ends here and the selection is kept.** Part 28. No threshold, for the
                // reason the press decision has none: a drag that never moved planted an anchor equal
                // to the caret, which the model already reports as no selection, so "did the user
                // select anything" is answered by the selection rather than by a distance.
                self.page_drag = false;
                self.damage = self.chrome.full_damage();
            }
            // **Right and middle buttons are not bound, and are counted as nothing.**
            //
            // Not as `pointer_inert`: that counter is about a *widget* that has no action, and this is
            // a button that was never going to have one. A context menu is the obvious future and it
            // is not this build.
            _ => {}
        }
        self.tick_with(bytes)?;
        Ok(())
    }

    /// **Extend the selection to wherever the pointer is now.** Part 28.
    ///
    /// ## Why this reuses `click_page` rather than duplicating it
    ///
    /// **Because a drag's far end is a click with shift held**, and the only difference between them
    /// is the `extend` flag and the fact that a drag must not close a menu or touch `pressed`. The
    /// hit test, `caret_at`, the column arithmetic and `offset_of` are all the same, and **a second
    /// copy of that chain would be four more lines that have to agree with the first.**
    ///
    /// ## Why a drag is not clipped to the page
    ///
    /// **Because `caret_at` already answers for any point**, including one above the first visible row
    /// — `Layout::caret_at` takes `scroll_line` into account — so dragging past the top or the bottom
    /// of the window selects to the top or the bottom of what is *visible* and no further. **That is
    /// the honest behaviour and it is recorded rather than fixed**: auto-scroll at the edge is a
    /// feature, and this is where it would go.
    ///
    /// ## Why the release does not clear the selection
    ///
    /// **Because the user chose it.** Everywhere else in this file a pointer event ends a gesture and
    /// leaves no trace — the release of a button applies it and forgets. A selection is the opposite:
    /// it outlives the gesture, because the next thing the user does is *type over it*. `page_drag`
    /// going false on the release is the end of the gesture; the selection is not part of it.
    fn drag_to(
        &mut self,
        x: i32,
        y: i32,
        bytes: &mut dyn holonomy_text::LeafSource,
    ) -> Result<(), SessionError> {
        // **`click_page(.., true)` — and `extend` unconditionally rather than reading shift.** A drag
        // extends whether or not shift is down, because that is what a drag *is*. Reading the
        // modifier here would mean shift-free drags silently select nothing.
        self.click_page(bytes, x, y, true)
    }

    /// What is under `(x, y)`, with the open menu's grab applied.
    ///
    /// # The grab
    ///
    /// **A menu that is open swallows everything that is not one of its rows.** While a popup is down,
    /// a pointer over the toolbar highlights nothing and a press there dismisses the menu without
    /// pressing the button. Without this, opening a menu and clicking away would also click whatever
    /// was underneath -- which is the single most irritating thing a menu can do, and it is invisible
    /// in every test that only ever clicks the popup.
    fn hover_at(&self, x: i32, y: i32) -> widgets::Hit {
        let l = &self.chrome.layout;
        if let Some(open) = self.state.open {
            match widgets::popup(l, &self.state, Some(open)).and_then(|p| p.row_at(x, y)) {
                Some(row) => return widgets::Hit::MenuItem { open, row },
                // The thing that opened it is still live: hovering it and pressing it toggles it.
                None => {
                    let opener = match open {
                        widgets::Open::Menu(i) => widgets::Hit::Menu(i),
                        widgets::Open::Tool(t) => widgets::Hit::Tool(t),
                    };
                    if widgets::hit(l, &self.state, x, y) == opener {
                        return opener;
                    }
                    return widgets::Hit::None;
                }
            }
        }
        widgets::hit(l, &self.state, x, y)
    }

    /// Act on a press.
    ///
    /// **The menu first, and it is the only branch that closes a menu.** Everything else either runs a
    /// command or leaves the menu alone; the dismissal lives here so there is exactly one place where
    /// "a menu is open and this is not a row" becomes "the menu is closed".
    ///
    /// **`x` and `y` are parameters rather than read off the event**, because the press is decided on
    /// the widget the *press* was over, and a `Hit` carries no position -- it is a small enum and
    /// giving every variant an `x, y` would put four numbers on the twenty-one `Tool` cases for the
    /// sake of one of them. The page is the only widget whose action needs a position, and it is the
    /// only branch that reads them.
    fn press(
        &mut self,
        hit: widgets::Hit,
        x: i32,
        y: i32,
        bytes: &mut dyn holonomy_text::LeafSource,
    ) -> Result<(), SessionError> {
        use widgets::{Hit, Tool};
        match hit {
            Hit::MenuItem { open, row } => {
                let Some(item) = open.items(&self.state).get(row).copied() else {
                    return Ok(());
                };
                let action = item.action;
                // **The popup closes before the action runs, and that order is load-bearing.** An
                // action that changes what the popup would contain -- choosing a zoom level leaves
                // the zoom control as the thing the user is holding -- would otherwise fight with the
                // rows still on screen. Close, damage, act: three lines, in that order.
                self.state.open = None;
                self.damage = self.chrome.full_damage();
                if let Some(action) = action {
                    self.apply_action(action)?;
                } else {
                    self.stats.pointer_inert += 1;
                }
            }
            Hit::Menu(index) => {
                // **A toggle, not "open".** Clicking the heading of the menu that is already down
                // closes it, which is the one behaviour a menu has that is not obvious from having
                // looked at a screenshot of it.
                let open = widgets::Open::Menu(index);
                self.state.open = (self.state.open != Some(open)).then_some(open);
                self.damage = self.chrome.full_damage();
            }
            Hit::Tool(tool) => {
                // **An armed toggle, before the dropdown check and before `tool_command`.** Part 25.
                //
                // **It goes first because the alternative is two special cases.** `tool_command` maps a
                // tool to a `Command` and `has_dropdown` says whether it opens a popup; a third
                // question — "does this tool set a flag?" — fits alongside them as one more arm rather
                // than as a special case inside either.
                //
                // **`armed_style()` then `set_armed_style()`, never a direct field write**, so the
                // model and the chrome cannot come apart. That is the whole discipline of this part and
                // it is one function pair.
                if let Some(slot) = holonomy_render::chrome::StyleFlags::slot_for(tool) {
                    let (bold, italic) = self.armed_style();
                    let (bold, italic) = match slot {
                        holonomy_render::chrome::StyleFlagsSlot::Bold => (!bold, italic),
                        holonomy_render::chrome::StyleFlagsSlot::Italic => (bold, !italic),
                    };
                    self.set_armed_style(bold, italic);
                    self.damage = self.chrome.full_damage();
                    self.stats.pointer_chrome += 1;
                    return Ok(());
                }
                // **A dropdown toggles, exactly as a menu does, and it is the same line of code for
                // the same reason: the user needs a way to close it with the gesture that opened it.**
                if tool.has_dropdown() {
                    let open = widgets::Open::Tool(tool);
                    self.state.open = (self.state.open != Some(open)).then_some(open);
                    self.damage = self.chrome.full_damage();
                    return Ok(());
                }
                self.state.open = None;
                self.damage = self.chrome.full_damage();
                match tool {
                    Tool::Collapse => {
                        self.state.sidebar_open = !self.state.sidebar_open;
                        self.damage = self.chrome.full_damage();
                    }
                    _ => {
                        if let Some(cmd) = tool_command(tool) {
                            self.run_clicked(cmd)?;
                        } else {
                            self.stats.pointer_inert += 1;
                        }
                    }
                }
            }
            Hit::Title(btn) => {
                self.state.open = None;
                self.damage = self.chrome.full_damage();
                if let Some(cmd) = title_command(btn) {
                    self.run_clicked(cmd)?;
                } else {
                    self.stats.pointer_inert += 1;
                }
            }
            Hit::Doc(index) => {
                // **Switching is a state change, not a command.** There is no `Command::OpenDocument`
                // because there is one document: `state.docs` is a list of *titles* the sidebar draws,
                // and part 19 was explicit that the models are not here. So the row is highlighted,
                // the active index moves, and `Session::adopt_document` is still the only way a
                // document gets in. **A second click on the already-active row is not an error** and
                // does nothing.
                self.state.open = None;
                self.state.active_doc = index;
                self.damage = self.chrome.full_damage();
            }
            Hit::NewDoc => {
                self.state.open = None;
                // **Counted, not faked.** `state.docs` is a list of titles and there is nowhere to put
                // a new one; inventing an "Untitled 2" row with no document behind it would make the
                // sidebar lie about what is open.
                self.stats.pointer_inert += 1;
                self.damage = self.chrome.full_damage();
            }
            Hit::SidebarBack => {
                self.state.open = None;
                self.state.sidebar_open = false;
                self.damage = self.chrome.full_damage();
            }
            Hit::Page => {
                self.state.open = None;
                // **`self.mods.shift()`, which this method has never read.** Part 28's finding: the
                // session already held a `ModifierState` and updated it on every key event, and
                // nothing looked at it for a pointer. Shift-click was therefore *available* the
                // whole time — the capability existed and was unused, which is the other kind of
                // thing this project goes looking for.
                self.click_page(bytes, x, y, self.mods.shift())?;
            }
            Hit::None => {
                // **The dismissal, and the only thing a press on nothing does.** `hover_at` has
                // already turned "the pointer is not over the popup" into `Hit::None`, so reaching
                // here with a menu open *is* the dismissal.
                if self.state.open.is_some() {
                    self.state.open = None;
                    self.damage = self.chrome.full_damage();
                }
            }
            Hit::Dismiss => {}
        }
        let _ = bytes;
        Ok(())
    }

    /// **Place the caret from a click on the page, or extend the selection if shift is held.** Part 28.
    ///
    /// ## CORRECTION — this doc used to say there is no selection
    ///
    /// It read:
    ///
    /// > A click places the caret and does not clear the selection, because there is no selection yet.
    /// > When one lands, the same line becomes the rule; a gate will be written then and not now,
    /// > because a gate written for a feature that does not exist tests nothing.
    ///
    /// **That was the right call at the time and it is now the thing being corrected.** The reasoning
    /// was honest — it named the absence rather than papering over it — and the promise was kept: no
    /// gate was written for a feature that did not exist. **The rule it promised is the one above.**
    ///
    /// ## Why the page is damaged and not the caret's two cells
    ///
    /// **Because a selection is not two cells.** A click with no shift moves the caret, and
    /// `caret_to`'s two-cell damage is exactly right — which is why that note is still here. **A click
    /// with shift, or a drag, changes a rectangle somewhere on the page**, and damaging only the caret
    /// would leave the old highlight drawn until something else happened to repaint over it. So the
    /// page is the damage unit for anything that moves the selection.
    ///
    /// ## Why `extend` is a parameter and not read here
    ///
    /// **Because the press and the drag need the same function with different arguments**, and the
    /// press already knows: it branches on `self.mods.shift()`. Reading it again inside would be the
    /// same fact read in two places, and the two would agree only until one of them changed.
    fn click_page(
        &mut self,
        bytes: &mut dyn holonomy_text::LeafSource,
        x: i32,
        y: i32,
        extend: bool,
    ) -> Result<(), SessionError> {
        let Some((line, grid_column)) = self.chrome.layout.caret_at(&self.state, x, y) else {
            return Ok(());
        };
        // **The column by the pen, and not the grid column.** Part 33.
        //
        // **`Layout::caret_at` divides `x - text.x` by `cell_w`**, and that was the right inverse of
        // the old `column * cell_w` — but the caret is now placed by the pen, so the grid column is no
        // longer the inverse of anything and **a click and the drawn caret disagree by up to a cell**.
        // `a_click_and_the_drawn_cell_agree_in_both_directions` is the gate that says so, and it caught
        // this on the first run of part 33: **249 against 250**.
        //
        // **`grid_column` is still read, and still used as the fallback**, for the case where the line's
        // bytes cannot be read: a click that cannot be resolved by the pen is a click on a line that is
        // not resident, and the grid column is one cell out rather than the click being dropped.
        let column = self
            .column_at_pen(line, x - self.chrome.layout.text.x as i32, bytes)
            .unwrap_or(grid_column);
        let at = self.offset_of(line, column, bytes)? as u32;
        if extend {
            self.extend_to(at)?;
        } else {
            // **`select(at, at)` and not `caret_to(at)`.** Both end up with the caret in the same
            // place, and only one of them *says* that a plain click drops a selection. `select` plants
            // an anchor at the click and then finds the anchor and the caret equal, which
            // `SelectionRange::between` reports as "no selection" — so the rule is a consequence of
            // the model rather than a line of code that has to be kept in step with it.
            self.select_at(at, at)?;
        }
        Ok(())
    }

    /// **The glyph column whose cell contains `dx` pixels into the text column.** Part 33.
    ///
    /// # Why this exists, and what was wrong before
    ///
    /// **The click was resolved by `dx / cell_w`**, which is the exact inverse of a caret placed by
    /// `column * cell_w` and **the wrong inverse of a caret placed by the pen.** Placing the caret on
    /// the pen while hit-testing on the grid is the worst of the two: correct for a monospaced face
    /// and wrong for every letter, in a direction the letters choose. **Measured, `dx` 10 landed the
    /// caret at pen 249 where the drawing puts the boundary at 250.**
    ///
    /// # What it does
    ///
    /// **Walks the line's glyphs, accumulating each one's real advance, and returns the index of the
    /// glyph whose half-open cell contains `dx`.** The boundary rule is **the midpoint**, so a click in
    /// the left half of a glyph puts the caret before it and a click in the right half puts it after —
    /// which is what every editor does and what a division cannot do, because a division has no way to
    /// know where a glyph's centre is.
    ///
    /// # The cost, and the fallback
    ///
    /// **One pass over one line, in 64-byte chunks through a stack buffer**, the same shape as
    /// [`refresh_caret_pen`](Self::refresh_caret_pen) and for the same reasons. **`None` when the line
    /// cannot be read in full**, and the caller falls back to the grid column: a click on a line that
    /// is not resident is better served by one cell out than by no click.
    fn column_at_pen(
        &mut self,
        line: u32,
        dx: i32,
        source: &mut dyn holonomy_text::LeafSource,
    ) -> Option<u32> {
        let dx = u32::try_from(dx).unwrap_or(0);
        let start = self.lines.line_begin(line as usize) as u32;
        let end = self.lines.line_end(line as usize) as u32;
        // **A click left of the text column is column 0**, which is what the grid arm answered and is
        // still right: there is no glyph before the first one.
        if dx == 0 || end <= start {
            return Some(0);
        }
        // **The slice, not `runs_in`, for the same reason as `refresh_caret_pen`.** A drag resolves a
        // column per pointer event and this is called once per event; the edit path's zero-allocation
        // gate would catch it eventually, and the paint path has no reason to allocate either.
        let span_count = self.editor.spans().spans().len();
        let mut si = self
            .editor
            .spans()
            .spans()
            .iter()
            .position(|s| s.end_byte > start)
            .unwrap_or(span_count);
        let mut run_end = self
            .editor
            .spans()
            .spans()
            .get(si)
            .map_or(end, |s| s.end_byte);
        const CHUNK: usize = 64;
        let mut buf = [0u8; CHUNK];
        let mut pen = 0u32;
        let mut column = 0u32;
        let mut at = start;
        while at < end {
            let n = CHUNK.min((end - at) as usize);
            let got = self
                .editor
                .read_into_faulting(source, at as usize, &mut buf[..n])
                .ok()?;
            if got < n {
                return None;
            }
            let mut from = 0usize;
            while from < n {
                while (at + from as u32) >= run_end && si < span_count {
                    si += 1;
                    run_end = self
                        .editor
                        .spans()
                        .spans()
                        .get(si)
                        .map_or(end, |s| s.end_byte);
                }
                let style = self
                    .editor
                    .spans()
                    .spans()
                    .get(si)
                    .filter(|_| (at + from as u32) < run_end)
                    .map_or(holonomy_render::Style::REGULAR, |sp| {
                        holonomy_render::Style(sp.atlas_style())
                    });
                // **One scalar at a time, because the boundary is per glyph.** A chunk-wide advance
                // would give the click the whole chunk's midpoint, which is not a column.
                let len = scalar_len(&buf[from..n]).min(n - from);
                let adv = pen_of(&self.painter, &buf[from..from + len], style);
                // **The glyph owns `[pen, pen + adv)`, and the midpoint rule splits it at
                // `pen + adv / 2`.** `>=` on the right edge is what puts a click past the last glyph on
                // the last column rather than one past it.
                if dx < pen.saturating_add(adv / 2) {
                    return Some(column);
                }
                pen = pen.saturating_add(adv);
                column += 1;
                from += len;
            }
            at += n as u32;
        }
        Some(column)
    }

    /// Select `[anchor, focus)`, collapsing anything already selected. Part 28.
    ///
    /// **The session twin of [`Editor::select`](holonomy_text::Editor::select), and it exists because
    /// `caret_to` is where the state and the damage are reconciled** — the same reason a `Session` has
    /// `caret_to` at all rather than calling the editor from four places.
    pub fn select_at(&mut self, anchor: u32, focus: u32) -> Result<(), SessionError> {
        self.editor.select(anchor, focus)?;
        self.after_selection_moves();
        Ok(())
    }

    /// Move the caret's far end of the selection, keeping its anchor. Part 28.
    pub fn extend_to(&mut self, focus: u32) -> Result<(), SessionError> {
        self.editor.extend_to(focus)?;
        self.after_selection_moves();
        Ok(())
    }

    /// **Reconcile the state and the damage after anything that moves the caret or the selection.**
    ///
    /// **The selection's own line is not tracked, and the page is damaged instead.** A selection can
    /// be forty rows tall and the state has no field for that — the highlight is computed per line by
    /// the emitter, from two byte offsets. **Damage would need the same arithmetic in reverse** (which
    /// rows the *previous* selection covered), and doing it approximately is worse than doing it
    /// simply: the page is 640 px by ~660, it is repainted on every scroll anyway, and one extra
    /// full-page rect costs a rectangle of work rather than a scan.
    fn after_selection_moves(&mut self) {
        self.sync_selection();
        self.state.caret_line = self.line_index(self.editor.caret() as usize);
        let _ = self.refresh_caret_column(None);
        // **And the pen, for the reason in `reconcile_caret`.** A drag moves the caret every pointer
        // event and a gate reads the state after one; the pen has to be current there too.
        let _ = self.refresh_caret_pen(None);
        // **Scroll too, because a shift-arrow to a line off the page needs it as much as a plain one.**
        self.scroll_line_into_view(self.state.caret_line);
        self.damage = self.chrome.full_damage();
    }

    /// Recompute [`ChromeState::caret_column`] as a count of UTF-8 scalars, and return it if complete.
    ///
    /// # This is the inverse of [`offset_of`](Self::offset_of), and it exists because of it
    ///
    /// `offset_of` was fixed in part 20: a column is characters, so a click in column 1 of a line
    /// starting with a two-byte `é` lands after the `é` and not between its bytes. **The other half
    /// of that fix was never made.** `caret_to` computed the column as `caret - line_start` — a *byte*
    /// count — so the same `éa` reported column 2 for a caret at byte 2, and `Caret::locate` drew the
    /// caret at `text.x + 2 * cell_w`, **one cell right of where the click had just put it.**
    ///
    /// That is not only a click bug: every keystroke goes through the same `caret_to`, so typing `é`
    /// has always moved the drawn caret one cell too far. `tests/pointer.rs` recorded the finding
    /// rather than asserting `== 1` (which would fail) or `== 2` (which would enshrine it).
    ///
    /// # Why scalars, and what that does and does not claim
    ///
    /// **`Painter::text` advances `cell_w` per UTF-8 scalar** — `TextRun`'s `k` indexes scalars — so
    /// the caret cell has to be the scalar index to land under the right glyph. **A combining mark is
    /// a second scalar at the same place**, and a CJK ideograph is two cells wide, and **this counter
    /// gets both wrong.** That is recorded rather than fixed: the fix is a display-width table, and
    /// adding one to make a counter correct in a case this build cannot produce would be the kind of
    /// untested machinery this project has declined four times.
    ///
    /// # Why one function with two capabilities
    ///
    /// **Because there are two callers with two different needs, and two implementations would be two
    /// things to keep in step.** `caret_to` has no source — see its note — and reads resident text.
    /// `paint_with` has one, reads whatever it likes, and runs immediately before `chrome.tree`, so the
    /// column the renderer reads is the authoritative one. Same function, same arithmetic, two
    /// capabilities — the split `Editor` already draws between `read_into` and `read_into_faulting`.
    ///
    /// # The cost, and the fast path
    ///
    /// **O(line length), not O(document)** — bounded by the line the caret is on, read in 64-byte
    /// chunks through a **stack** buffer so `tests/session_no_alloc.rs` stays green. **A caret at the
    /// start of its line returns 0 without reading anything**, which is the common case for
    /// `DocumentStart`, `Home`, and typing at the beginning of a line, and it is why the scan count in
    /// `SessionStats` exists rather than the count being merely asserted here.
    ///
    /// # The failure is a fallback and not an error
    ///
    /// **`None` when the range could not be read in full**, which on a resident rope means an absent
    /// leaf. The state keeps the byte count, and the paint path tries again with a source. **This is
    /// the one window in which `caret_column()` reports a byte count**, and it is a window in which
    /// the caret's line is not painted — the paint path fetches it — so no pixel is placed from it.
    fn refresh_caret_column(
        &mut self,
        mut source: Option<&mut dyn holonomy_text::LeafSource>,
    ) -> Option<u32> {
        let caret = self.editor.caret() as usize;
        let start = self.line_start(caret);
        let span = caret.checked_sub(start)?;
        if span == 0 {
            // **The fast path, and the reason it is a fast path rather than a special case.** Zero
            // bytes to count, so there is nothing to read and nothing to get wrong.
            self.state.caret_column = 0;
            return Some(0);
        }
        self.stats.caret_column_scans += 1;
        /// Bytes per rope call. The same chunk `offset_of` uses, for the same reason: a column scan
        /// must not be one rope walk per character.
        const CHUNK: usize = 64;
        let mut buf = [0u8; CHUNK];
        let mut scalars = 0u32;
        let mut at = start;
        while at < caret {
            let n = CHUNK.min(caret - at);
            // **One loop, two readers.** The difference is the `match` and nothing else, which is the
            // whole argument for not writing the walk twice.
            let got = match source.as_deref_mut() {
                Some(src) => self
                    .editor
                    .read_into_faulting(src, at as usize, &mut buf[..n])
                    .ok()?,
                None => self.editor.read_into(at as usize, &mut buf[..n]).ok()?,
            };
            // **A short read means the rest is not there**, and counting what arrived would be a
            // number that is wrong by an unknown amount -- worse than the byte count it replaces,
            // because the byte count at least has a stated meaning.
            if got < n {
                return None;
            }
            // **A UTF-8 scalar starts at any byte that is not a continuation byte**, which is the
            // whole test: `0b10xxxxxx` continues a scalar started earlier, everything else starts one.
            // This is the same rule `offset_of` walks the other way, and it needs no table and no
            // decode -- a scalar's *length* is only needed to skip it, and counting does not skip.
            scalars += buf[..n].iter().filter(|&&b| b & 0xC0 != 0x80).count() as u32;
            at += n;
        }
        self.state.caret_column = scalars;
        Some(scalars)
    }

    /// **Where the caret's pen reaches on its own line, in pixels from the text column's left edge.**
    /// Part 33.
    ///
    /// # Why this exists, and why it is not `refresh_caret_column`
    ///
    /// **`refresh_caret_column` counts scalars, and scalars are not a distance.** It answers "which
    /// glyph", which is what a click needs to convert a column into an offset; it does not answer "how
    /// far", because the document is proportional and `'a'` is 9 px where `'W'` is 16. **The caret was
    /// being placed by multiplying a glyph index by the chrome's monospace cell**, which is part 30's
    /// standing decision and the largest remaining item on the backlog.
    ///
    /// **So the two questions are two functions.** One counts glyphs and is called by the keystroke
    /// path that has no source; this one measures distance and is called only from `paint_with`,
    /// which has a source and draws. **Merging them would put a second concern into a function whose
    /// invariants are documented and load-bearing** — the stack buffer, the 64-byte chunk, the
    /// faulting read, the short-read fallback — for no saving, since the paint path reads the line
    /// anyway.
    ///
    /// # The cost, and why it is a second pass
    ///
    /// **O(line length), read in 64-byte chunks through a stack buffer**, exactly as
    /// [`refresh_caret_column`](Self::refresh_caret_column) does, so `tests/session_no_alloc.rs`
    /// stays green. **It is a second pass over one line, and that is a deliberate trade**: the two
    /// walks ask different questions of the same bytes, the column's is needed by the click path and
    /// the pen's by nothing else, and a single fused walk would have to answer both from the same
    /// read to save one pass over at most 80-odd bytes. **A caret at the start of its line returns
    /// `Some(0)` without reading anything**, which is `Home`, `DocumentStart` and every keystroke at
    /// the beginning of a line.
    ///
    /// # Why the styled runs are queried and not assumed Regular
    ///
    /// **Because a bold glyph advances by the bold face's metric**, and part 31 established that the
    /// emitter places a run using that face's advances. **A pen that assumed Regular would put the
    /// caret back on the grid for exactly the runs where the grid was wrong**, which is the worst of
    /// both: correct for an unstyled document and wrong for a styled one.
    fn refresh_caret_pen(
        &mut self,
        mut source: Option<&mut dyn holonomy_text::LeafSource>,
    ) -> Option<u32> {
        let caret = self.editor.caret();
        let start = self.line_start(caret as usize) as u32;
        let span = caret.checked_sub(start)?;
        if span == 0 {
            self.state.caret_pen_x = Some(0);
            return Some(0);
        }
        // **`SpanMap::spans()` and not `runs_in`, and that is forced.** `runs_in` returns a `Vec`, and
        // `refresh_caret_pen` is called from `reconcile_caret` — **which is the edit path**, where
        // `tests/session_no_alloc.rs` asserts that nothing allocates. The first version called
        // `runs_in` and `typing_through_a_session_allocates_nothing` failed on the first run.
        //
        // **The slice is the same information and costs nothing to walk.** An unstyled document's map
        // is one span over the whole text, and the walk below degenerates to a straight sum of Regular
        // advances without a special case.
        //
        // **The index, not the slice.** The walk reads the line through `&mut self.editor` and cannot
        // hold a borrow of it, so the slice is re-indexed per scalar and each span is taken as an owned
        // `(end_byte, atlas_style)` pair. **A bounds check per scalar on a slice that is already in
        // cache is cheaper than the `Vec` that holding it would have needed**, and the alternative --
        // `SpanMap::style_at` by value -- is O(spans) *per glyph*, which is quadratic on a styled
        // document and O(n) on an unstyled one.
        let span_count = self.editor.spans().spans().len();
        let mut si = self
            .editor
            .spans()
            .spans()
            .iter()
            .position(|s| s.end_byte > start)
            .unwrap_or(span_count);
        /// Bytes per rope call, and the same chunk `refresh_caret_column` uses.
        const CHUNK: usize = 64;
        let mut buf = [0u8; CHUNK];
        let mut pen = 0u32;
        let mut at = start;
        // **Which span the walk is inside, advanced as the walk passes span boundaries.** The spans
        // are ascending and gap-free, so one cursor answers "what face is this byte in" without a
        // search, and `SpanMap`'s own invariant is what makes that true.
        let mut run_end = self
            .editor
            .spans()
            .spans()
            .get(si)
            .map_or(caret, |s| s.end_byte);
        while at < caret {
            let n = CHUNK.min((caret - at) as usize);
            let got = match source.as_deref_mut() {
                Some(src) => self
                    .editor
                    .read_into_faulting(src, at as usize, &mut buf[..n])
                    .ok()?,
                None => self.editor.read_into(at as usize, &mut buf[..n]).ok()?,
            };
            // **A short read means the rest is not there**, and a pen summed over a prefix of a line
            // is a number that is wrong by an unknown amount. Same fallback as the column scan.
            if got < n {
                return None;
            }
            let mut from = 0usize;
            while from < n {
                while (at + from as u32) >= run_end && si < span_count {
                    si += 1;
                    run_end = self
                        .editor
                        .spans()
                        .spans()
                        .get(si)
                        .map_or(caret, |s| s.end_byte);
                }
                let style = self
                    .editor
                    .spans()
                    .spans()
                    .get(si)
                    .filter(|_| (at + from as u32) < run_end)
                    .map_or(holonomy_render::Style::REGULAR, |sp| {
                        holonomy_render::Style(sp.atlas_style())
                    });
                // **To the end of this run, or to the end of the chunk, whichever comes first.**
                let to = n.min((run_end - at) as usize).max(from + 1);
                pen = pen.saturating_add(pen_of(&self.painter, &buf[from..to], style));
                from = to;
            }
            at += n as u32;
        }
        self.state.caret_pen_x = Some(pen);
        Some(pen)
    }

    /// Add `r` to the pending damage.
    ///
    /// **`self.damage` is a [`DamageRect`], not a tracker**, so "add" is a union rather than an
    /// accumulation. A tracker would carry a list and answer "was this ever damaged", and nothing
    /// here asks that question -- the paint wants a bounding box and nothing else. An empty rect is
    /// skipped because [`DamageRect::union`] treats `EMPTY` as the identity but a caller that unions
    /// forty hover rects should not pay for the forty.
    #[inline]
    fn invalidate(&mut self, r: DamageRect) {
        if !r.is_empty() {
            self.damage = self.damage.union(&r);
        }
    }

    /// The byte offset of `(line, column)`, clamped into the line.
    ///
    /// # Why this walks characters and not bytes
    ///
    /// **Because `column` is a column and an offset is a byte count, and the two are the same only on
    /// ASCII.** The first version walked `column` *bytes*, so a click in column 1 of a line starting
    /// with a two-byte `é` landed between the two bytes of that character -- and `caret_to`, which
    /// rounds down to a codepoint boundary, then put the caret *before* it. The gate
    /// `clicking_a_column_puts_the_caret_after_that_character` is a line of `éa` for exactly this
    /// reason, and it caught it on the first run: caret 0, expected 2.
    ///
    /// **The walk is over UTF-8 scalar values, not grapheme clusters.** A combining sequence is two
    /// scalars and one glyph, so a click in the middle of one puts the caret between its parts -- which
    /// is what every editor without a full text-shaping layout does, and saying so is better than
    /// implying a cluster-aware implementation.
    ///
    /// **Clamped to the line's end rather than the document's**, because a click past the last
    /// character of a short line is a click at the end of that line and not a click on the next one.
    /// That is why clicking the white space right of a short paragraph does not move the caret down.
    fn offset_of(
        &mut self,
        line: u32,
        column: u32,
        source: &mut dyn holonomy_text::LeafSource,
    ) -> Result<usize, SessionError> {
        /// Bytes read per rope call. A click on column 80 must not be 80 rope walks, and this is the
        /// same chunk the document scan uses.
        const CHUNK: usize = 64;
        let mut buf = [0u8; CHUNK];
        // **`lines.line_begin(line)`, not `Session::line_start(at)`.** Those two are one argument and
        // two different questions: `line_begin` takes a *line index* and gives a byte offset,
        // `line_start` takes a *byte offset* and gives the offset of the line containing it. Passing a
        // line index to `line_start` silently returns the start of line 0 for every line below the
        // first, so every click on row 1 landed on row 0. **`a_click_on_the_page_puts_the_caret_under
        // _the_pointer` found it on the first run**, and the gate's fixture is a three-line document
        // specifically because a one-line document cannot tell the two apart.
        let start = self.lines.line_begin(line as usize);
        let mut at = start;
        let mut left = column;
        while left > 0 {
            // **A whole chunk, not `left` bytes.** The first version asked for exactly `left` bytes,
            // so a click in column 1 of a line starting with a two-byte `é` read *one* byte, saw
            // `0xC3`, decided the character was two wide, and advanced one byte anyway because only
            // one was in hand -- landing the caret between the two bytes of one character.
            // `caret_to` then rounded down and put it before the `é`. **You cannot tell how wide a
            // UTF-8 character is without reading all of it.**
            let room = self.editor.text_len().saturating_sub(at);
            let n = room.min(CHUNK);
            if n == 0 {
                break;
            }
            let got = self.editor.read_into_faulting(source, at, &mut buf[..n])?;
            if got == 0 {
                break;
            }
            let mut i = 0;
            while i < got && left > 0 {
                let b = buf[i];
                // **The newline ends the line, so `column` stops here.** Not "skip it and carry on" --
                // a click past the end of a line's text is a click at its end.
                if b == b'\n' {
                    return Ok(at + i);
                }
                // Advance one whole scalar. A continuation byte is 10xxxxxx; the width is the
                // leading byte's, and a malformed sequence is one byte so the loop cannot stall.
                let w = match b {
                    0x00..=0x7F => 1,
                    0xC0..=0xDF => 2,
                    0xE0..=0xEF => 3,
                    0xF0..=0xF7 => 4,
                    _ => 1,
                };
                i += w.min(got - i).max(1);
                left -= 1;
            }
            at += i;
        }
        Ok(at)
    }

    /// The armed style flags, for a gate that wants the model's own view rather than the chrome's.
    ///
    /// **`pending_style()` and [`armed_style`](Self::armed_style) are the same reader.** The public one
    /// exists because a gate must not have to ask the chrome what it believes is armed: **a test that reads
    /// both sides of a "these must agree" claim is the only kind that can check it.** Part 25's
    /// `the_model_and_the_chrome_agree_after_every_transition` is that test.
    pub fn pending_style(&self) -> Option<u16> {
        self.pending_style
    }

    /// Arm or disarm a style toggle at the caret, in the model and in the chrome at once.
    ///
    /// **One function for both halves, and that is the whole reason this part exists in this shape.**
    /// `pending_style` is a `u16` in the session and `ChromeState::styles` is a pair of bools in the render
    /// crate, which knows nothing about `u16` flags. **Two copies of one value, written in two places, is a
    /// value that will disagree** — and the disagreement would show as a Bold button that lights up and types
    /// ordinary text, which is a state no gate in this project would think to look for.
    ///
    /// **So `armed_style` is the only reader of `pending_style` and `set_armed_style` the only writer, and
    /// they are adjacent.** `apply_action` and the toolbar's press both go through the setter.
    fn set_armed_style(&mut self, bold: bool, italic: bool) {
        self.state.styles.bold = bold;
        self.state.styles.italic = italic;
        // **`Some(flags)` and never `None`, and that is the deliberate part.** Un-pressing Bold when nothing
        // else is armed writes `Some(0)` rather than `None`, because `None` means "inherit from the
        // character before the caret" and inheriting bold after the user turned bold off is the bug this
        // option type exists to prevent. **`None` is for a session nobody has toggled anything in.**
        let mut f = 0u16;
        if bold {
            f |= STYLE_BOLD;
        }
        if italic {
            f |= STYLE_ITALIC;
        }
        self.pending_style = Some(f);
    }

    /// The armed flags, read from the model rather than from the chrome.
    ///
    /// **`&self` and the session, not a `pub` accessor on the chrome**, so a gate cannot ask the chrome what
    /// it thinks is armed and conclude the two agree.
    fn armed_style(&self) -> (bool, bool) {
        // **Through `unwrap_or(0)`, so "the user said nothing" and "the user said plain" both read as
        // nothing armed** — which is right for the two bools and wrong for the model, and the difference is
        // the whole reason the model is an `Option`.
        let f = self.pending_style.unwrap_or(0);
        (f & STYLE_BOLD != 0, f & STYLE_ITALIC != 0)
    }

    /// **The colour armed for typed text, or 0 for "the document's default".** Part 29.
    ///
    /// **The colour twin of [`armed_style`](Self), and the `unwrap_or(0)` is the same shape and the
    /// same reason:** "the user said nothing" and "the user chose the default" both read as nothing
    /// armed, which is right for the span field -- `0` *is* the default -- and is why this returns a
    /// plain `u32` rather than an `Option` the way `armed_style` returns two `bool`s.
    fn armed_colour(&self) -> u32 {
        self.pending_colour.unwrap_or(0)
    }

    /// Act on an [`Action`](holonomy_render::menus::Action) a popup row carried.
    ///
    /// # Why the session, and not the renderer
    ///
    /// **Because `Action` has no meaning of its own.** `Action::Undo` is the sentence "this row says
    /// Undo"; turning it into an edit is the session's job, and `holonomy-render` does not know a
    /// `Command` exists. **Part 20 did this by comparing the row's label**, so renaming a menu entry
    /// changed the program's behaviour; now it is a `match` on an enum, and adding a variant is a
    /// compile error here rather than a silent no-op.
    ///
    /// **Three of the arms change chrome state rather than the document** -- zoom, style, font -- and
    /// routing them through here too is the point: there is exactly one function that says what every
    /// action in the product means, and a dropdown value is an action like any other.
    ///
    /// ## Public as of part 28
    ///
    /// **For the same reason [`Session::apply`] is public**: a driver -- or a gate -- needs to act on
    /// a menu row without inventing a pointer event that happens to land on the right row.
    /// `tests/selection.rs` needs it for the gate that says a selected range can be bolded, and the
    /// alternative was to reach into the chrome, work out the Bold button's rectangle and press it --
    /// **which would have tested the hit test as well as the styling, and failed for reasons that
    /// have nothing to do with what it claims.** A gate should fail for one reason.
    pub fn apply_action(
        &mut self,
        action: holonomy_render::menus::Action,
    ) -> Result<(), SessionError> {
        use holonomy_render::menus::Action;
        match action {
            Action::SetZoom(pct) => {
                self.set_zoom(pct);
                // **`pointer_chrome`, not `pointer_commands`.** Part 21 put it here, and this is the
                // correction: no `Command` was produced and nothing about the document changed. The
                // zoom label moves, and `tests/zoom.rs` measures that as the only pixel it moves.
                self.stats.pointer_chrome += 1;
            }
            Action::ToggleBold => {
                // **A selection means "style this range", and that takes precedence over arming.**
                // Part 28 — the thing part 24's own note said selection would unlock.
                if !self.style_selection(
                    !self.armed_style().0,
                    self.armed_style().1,
                    self.armed_colour(),
                )? {
                    self.set_armed_style(!self.armed_style().0, self.armed_style().1);
                }
                self.damage = self.chrome.full_damage();
                self.stats.pointer_chrome += 1;
            }
            Action::ToggleItalic => {
                if !self.style_selection(
                    self.armed_style().0,
                    !self.armed_style().1,
                    self.armed_colour(),
                )? {
                    self.set_armed_style(self.armed_style().0, !self.armed_style().1);
                }
                self.damage = self.chrome.full_damage();
                self.stats.pointer_chrome += 1;
            }
            // **Part 29: the fourth dropdown, and the first whose value reaches the document.**
            //
            // **`style_selection` with a colour rather than flags**, which is why it takes the value
            // and not an index: the session has already resolved the index through `COLOURS` and the
            // span field wants `0x00RRGGBB`. Everything else about the rule -- a selection styles the
            // range, no selection arms the next character -- is part 28's and is not restated here.
            Action::SetColour(i) => {
                let n = holonomy_render::menus::COLOURS.len().saturating_sub(1);
                let idx = usize::from(i).min(n);
                self.state.colour_index = idx;
                let rgb = holonomy_render::menus::COLOURS[idx].1;
                // **Arm *and* apply.** A selection is styled; with no selection the colour waits for
                // the next character, which is what the control means with nothing selected and is
                // part 25's rule for the two style toggles.
                self.pending_colour = Some(rgb);
                self.style_selection(self.armed_style().0, self.armed_style().1, rgb)?;
                self.damage = self.chrome.full_damage();
                self.stats.pointer_chrome += 1;
            }
            Action::SetStyle(i) => {
                self.state.style_index =
                    usize::from(i).min(holonomy_render::menus::STYLES.len().saturating_sub(1));
                self.damage = self.chrome.full_damage();
                self.stats.pointer_chrome += 1;
            }
            Action::SetFont(i) => {
                self.state.font_index =
                    usize::from(i).min(holonomy_render::menus::FONTS.len().saturating_sub(1));
                // **The face is recorded and nothing else happens.** §2.2.1 builds the atlas from one
                // body face, so choosing another name changes the toolbar's label and not one pixel of
                // the page. Saying so here rather than in the renderer is deliberate: the renderer
                // draws the choice, and the session admits what it costs.
                self.damage = self.chrome.full_damage();
                self.stats.pointer_chrome += 1;
            }
            other => {
                if let Some(cmd) = action_command(other) {
                    self.run_clicked(cmd)?;
                } else {
                    self.stats.pointer_inert += 1;
                }
            }
        }
        Ok(())
    }

    /// Set the zoom, and repaint.
    ///
    /// # CORRECTION, part 23: this does not zoom
    ///
    /// **The doc this replaced said `25..=400` because "§2.9.3's image-cache thresholds were measured
    /// against" it. That was false.** No code reads `zoom_percent` except the toolbar's label and the
    /// dropdown's tick — there is no image-cache coupling, and the claim was a plausible-sounding
    /// sentence attached to a number that meant nothing.
    ///
    /// **So: this sets a label.** It is kept, and `crates/holonomy/tests/zoom.rs` measures exactly how
    /// little it does — *every pixel that changes when the zoom changes lies inside the zoom button's
    /// own rect*, and the page is pixel-identical at 50% and at 200%.
    ///
    /// **Public as of part 23, and that is the second thing the bite check found.** It was private, so
    /// the three product paths that set the zoom from `--zoom` assigned `state.zoom_percent` directly
    /// and **bypassed this function entirely** — which meant a gate could not reach the one place a
    /// zoom would live, and `tests/zoom.rs` set the field directly too. **A gate that assigns the field
    /// tests the field, not the feature**, and it stayed green when `set_zoom` was given a real cell
    /// scale. The three `--zoom` sites now call this, so there is one writer, and it is the one the
    /// gates exercise.
    ///
    /// **The clamp stays, and is still worth having**, for the reason a clamp is worth having at all:
    /// the number is drawn, it comes from `--zoom` on a command line, and a value outside the range the
    /// chrome can lay out would make the label wider than the button that holds it. The gate
    /// `the_zoom_label_fits_its_button_at_both_ends_of_the_clamp` is what that is worth.
    ///
    /// **What real zoom needs, stated because the next person will otherwise assume this is a small
    /// omission:** the glyphs are rasterised at one size. Scaling `cell_w` and `cell_h` alone would
    /// space the text out without enlarging it. **A zoom needs the atlas rebuilt at the new ppem**, and
    /// then a decision about how many sizes stay resident, which is an atlas-budget question and not a
    /// chrome one. That is a phase, not a patch.
    pub fn set_zoom(&mut self, percent: u32) {
        self.state.zoom_percent = percent.clamp(25, 400);
        self.damage = self.chrome.full_damage();
    }

    /// Apply a command a click produced, and count it.
    ///
    /// **`self.apply` rather than a dispatch through the keymap**, because a click already *is* the
    /// dispatch: there is no chord to fold and no modifier state to read, and routing a button press
    /// back through the keymap would mean inventing a fake chord for it.
    fn run_clicked(&mut self, command: holonomy_input::Command) -> Result<(), SessionError> {
        self.stats.commands += 1;
        self.stats.pointer_commands += 1;
        self.apply(command)?;
        Ok(())
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
        self.tick_with(&mut crate::store::NoSource)
    }

    /// [`tick`](Self::tick) against a byte source. See [`run_with`](Self::run_with) for why the byte
    /// source is a separate parameter rather than part of the session.
    pub fn tick_with(
        &mut self,
        bytes: &mut dyn holonomy_text::LeafSource,
    ) -> Result<(), SessionError> {
        if !self.damage.is_empty() {
            // `paint` clears `damage` itself, so this does not need to.
            self.paint_with(Some(self.damage), bytes)?;
        }
        let caret = self.caret_cell();
        // `advance` reports only a transition *to visible*; see `Blink` for why the other
        // direction is free.
        if let Some(damage) = self.blink.advance(caret) {
            self.state.caret_visible = self.blink.visible();
            self.paint_with(Some(damage), bytes)?;
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
        self.handle_event_with(&mut crate::store::NoSource, event)
    }

    /// [`handle_event`](Self::handle_event) against a byte source. **The windowed path's call.**
    ///
    /// The X11 driver has no [`InputSource`], so it calls this one event at a time -- which is why the
    /// byte source has to reach this function and not only [`run_with`](Self::run_with). A driver that
    /// forgot it would compile, run, and draw a blank page; see
    /// [`NoSource`](crate::store::NoSource) for why the alternative was worse.
    pub fn handle_event_with(
        &mut self,
        bytes: &mut dyn holonomy_text::LeafSource,
        event: holonomy_input::InputEvent,
    ) -> Result<Option<Exit>, SessionError> {
        let Some(command) = self.dispatch(event) else {
            return Ok(None);
        };
        self.stats.commands += 1;
        if let Command::Hotkey(Hotkey::Quit) = command {
            return Ok(Some(Exit::Quit));
        }
        self.apply(command)?;
        self.tick_with(bytes)?;
        Ok(None)
    }

    /// Fold `event` into the modifier state and map it to a command, if it is one.
    ///
    /// Split out of [`Session::handle_event`] so that the *edit* path and the *paint* path can be
    /// driven apart, which is what `tests/session_no_alloc.rs` needs: Phase 11's zero-allocation
    /// claim is about the model change, and the paint's remaining allocations are Phase 12's work.
    /// Measured together they are one number that is neither claim.
    pub fn dispatch(&mut self, event: holonomy_input::InputEvent) -> Option<Command> {
        // The modifier state is folded *before* dispatch, because `Ctrl+Q` is "ctrl goes down" then
        // "Q goes down", and the second is only `Ctrl+Q` once the first has landed.
        self.keymap.dispatch_into(event, &mut self.mods)
    }

    /// Apply one command, damaging what it touched.
    pub fn apply(&mut self, command: Command) -> Result<(), SessionError> {
        match command {
            Command::Insert(c) => {
                // A stack buffer, not a `Vec`. Phase 11: this was `encode_utf8(&mut buf).as_bytes()
                // .to_vec()`, which allocated and freed a four-byte heap block on **every
                // keystroke** -- the last allocation on the edit path, and the one the
                // `tests/session_no_alloc.rs` gate found after the document copies were gone.
                //
                // `encode_utf8` writes into `buf` and borrows it, so the encoded bytes live as long as
                // this arm; `insert` takes `&[u8]` and does not retain them. Four bytes is the most a
                // `char` can encode to, so the buffer cannot be too small.
                let mut buf = [0u8; 4];
                let bytes = c.encode_utf8(&mut buf);
                self.insert(bytes.as_bytes())?;
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
            // --- Selection. Part 28.
            //
            // **Each one is the plain command with `extend` set, and none of them is a new kind of
            // movement.** That is the claim to hold: shift-arrow does what arrow does and then grows
            // the range, and the six arms above this line are the proof that they stay in step —
            // `SelectUp` is `move_line(-1, true)`, not a second implementation of "up one line".
            Command::SelectLeft => self.extend_caret(-1)?,
            Command::SelectRight => self.extend_caret(1)?,
            Command::SelectUp => self.move_by_line(-1, true)?,
            Command::SelectDown => self.move_by_line(1, true)?,
            // **`0` and `text_len()`, matching `Command::Home` and `Command::End` above — and not
            // the line's own start and end.**
            //
            // **That is deliberate, and it is agreeing with a defect.** `Command::Home`'s doc says
            // *"Move to the start of the line"* and its arm is `self.caret_to(0)`, which is the start
            // of the *document*; `End` likewise goes to the end of the document. **So Shift+Home
            // going somewhere different from Home would be the second wrong thing**, and the honest fix
            // is to change both in one part with their own gate. Recorded in PROJECT.md, not fixed
            // here, because it is a behaviour change with no bearing on selection.
            Command::SelectHome => self.extend_to(0)?,
            Command::SelectEnd => self.extend_to(self.editor.text_len() as u32)?,
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
            Command::Hotkey(Hotkey::InsertMath) => {
                self.insert_math()?;
            }
            Command::Hotkey(Hotkey::InsertImage) => {
                self.insert_image()?;
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
        // **A selected range is replaced, not typed into.** Part 28.
        //
        // **This is the first thing typing does with a selection and the only reason a selection is
        // more than a highlight.** The delete goes through [`delete_selection`](Editor::delete_selection)
        // — one undo record carrying every deleted byte — and then the insert lands at the range's
        // start, which `delete_at` has already parked the caret on.
        //
        // **Two undo records, and that is the honest cost.** A selection followed by one keystroke is
        // *one* thing the user did, and a user pressing Ctrl+Z once would expect the whole paragraph
        // back rather than the paragraph with one letter still deleted. **A single record would mean a
        // composite action in `UndoStack`, which is a new kind of thing in that file for a case this
        // part cannot test.** Recorded, not pretended away — and `undo_restores_a_typed_over_selection`
        // asserts the behaviour that *is* delivered: two undos, not one.
        //
        // **Skipped for an empty `bytes`,** because `Command::Insert` is only called with a non-empty
        // character, and a zero-length insert with a delete in front of it would be a selection
        // deleted by nothing.
        if !bytes.is_empty() {
            self.delete_selected()?;
        }
        let at = self.editor.caret() as usize;
        // `GrowIntoInsert` so typing at the end of a bold word keeps it bold, which is what a word
        // processor does and what `SpanPolicy`'s own docs argue for.
        self.editor
            .insert_at(self.editor.caret(), bytes, SpanPolicy::GrowIntoInsert)?;
        self.counts.after_insert(&self.editor, at, bytes);
        // **Arm, applied to what was just typed.** Part 25.
        //
        // **Not before the insert, because `style_range` drops a zero-width range** — `SpanMap::
        // style_range` returns `Ok(())` for `start == end` and stores nothing, so "bold the next
        // character" cannot be a span parked at the caret. It is a field on the session, and the only
        // place the document can show it is on bytes that exist.
        //
        // **After, and over `[at, at + len)`, which is the range this insert produced.** Applying it to
        // the caret alone would bold nothing on a newline — `insert` is called for `\n` too — and
        // applying it to the whole line would restyle text the user did not touch.
        //
        // **A style change is an edit**, so it goes through `after_edit` like one: it is in the undo
        // stack, it repaints the line, and `SessionStats::edits` counts it. **Two edits per typed
        // character when a toggle is armed, and one when none is** — which is the cost of the feature
        // and is stated here rather than left to be noticed on the keystroke benchmark.
        if let Some(armed) = self.pending_style.filter(|_| !bytes.is_empty()) {
            self.editor
                .style_range(at as u32, (at + bytes.len()) as u32, armed, 0)?;
            self.after_edit(0)?;
        }
        // **The armed colour, over the same range, and as a second edit.** Part 29.
        //
        // **Separate from the style arm above rather than merged into one `style_range` call**, because
        // they are armed independently: a user can have Bold on and no colour, or a colour and no
        // bold. **Merging them would make one of the two un-clearable**, which is exactly the bug
        // part 25 found when un-arming Bold left the flag at 0 and the next character came out bold.
        if let Some(rgb) = self.pending_colour.filter(|_| !bytes.is_empty()) {
            self.editor
                .style_range(at as u32, (at + bytes.len()) as u32, 0, rgb)?;
            self.after_edit(0)?;
        }
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
            // Rescan, for the reason `insert_table` does: the appended row's separators are not
            // returned to the caller and `TextCounts` needs them.
            self.counts = TextCounts::scan(&self.editor);
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
        // Phase 11 item 4: four bytes into a stack array rather than a whole-document `editor.text()`, so
        // this path allocates nothing and is O(4).
        let start = at as usize - INDENT.len();
        let mut indent = [0u8; 4];
        let n = self.editor.read_into(start, &mut indent).unwrap_or(0);
        if &indent[..n] != INDENT {
            // Nothing to remove. Not an error: Shift+Tab on a line that was never indented is a
            // keystroke with no effect, exactly as it is in any editor.
            self.stats.table_nav_nowhere += 1;
            return Ok(());
        }
        self.editor.delete_at(start as u32, INDENT.len() as u32)?;
        self.counts.after_delete(&self.editor, start, INDENT);
        self.after_edit(INDENT.len() as u32)
    }

    /// **Delete the selection, tell the counters about it, and report what went.** Part 28.
    ///
    /// ## Why this exists rather than three lines at each call site
    ///
    /// **`TextCounts::after_delete` needs the deleted bytes**, because FR-1.2 zeroes them and after the
    /// delete there is nothing left to count word starts from. Three call sites — Backspace, Delete,
    /// and typing over a selection — each needed the same capture-then-delete-then-report sequence,
    /// and **the capture is the part with a decision in it**, so three copies is three chances to make
    /// it differently.
    ///
    /// ## The decision: scratch when it fits, heap when it does not
    ///
    /// **Exactly the split [`Editor::anchors_in_range`](holonomy_text::Editor) already documents:**
    ///
    /// > A range longer than the scratch is not a keystroke — it is a multi-kilobyte selection — so it
    /// > takes one heap buffer, which is the honest cost and is documented rather than pretended away.
    ///
    /// **A selection can be a whole document**, so a fixed buffer would either panic or silently
    /// truncate the bytes and mis-count every word after the truncation point. One `Vec` on the
    /// selection path is the right cost, and the keystroke path — no selection, `sel.len() == 1` —
    /// **never allocates**, which is what `tests/session_no_alloc.rs` gates.
    fn delete_selected(&mut self) -> Result<Option<u32>, SessionError> {
        let Some(sel) = self.editor.selection() else {
            return Ok(None);
        };
        let start = sel.start as usize;
        let len = sel.len();
        // **`LINE_SCRATCH_BYTES` is this file's scratch and the selection can be longer than it.** The
        // branch is on the selection's own length, so a one-character selection — which is what the
        // no-alloc gate exercises — takes the stack-free path and a whole-document selection takes one
        // buffer that is freed when this returns.
        let big = len as usize > LINE_SCRATCH_BYTES;
        let mut heap: Vec<u8> = Vec::new();
        let n = if !big {
            // **`read_into` into the scratch, and then truncate it**: the scratch is a `Vec<u8>` whose
            // *capacity* is the invariant and whose *length* is meaningless between uses, exactly as
            // `line_scratch` is. `set_len` on uninitialised capacity would hand `after_delete` bytes
            // that were never written, so the read is what makes the first `n` bytes real.
            let got = self
                .editor
                .read_into(start, &mut self.line_scratch[..len as usize])
                .unwrap_or(0);
            got
        } else {
            heap = vec![0u8; len as usize];
            self.editor.read_into(start, &mut heap).unwrap_or(0)
        };
        self.editor.delete_selection()?;
        let removed: &[u8] = if big {
            &heap[..n]
        } else {
            &self.line_scratch[..n]
        };
        self.counts.after_delete(&self.editor, start, removed);
        self.after_edit(len)?;
        Ok(Some(len))
    }

    fn backspace(&mut self) -> Result<(), SessionError> {
        // **A selection is deleted whole, before the "is there a byte before the caret" question.**
        // Part 28. The check below is about *one character*; with a selection the answer is the range,
        // and asking it first would mean Backspace with a selection anchored at byte 0 doing nothing at
        // all because the caret happens to be at 0.
        if self.delete_selected()?.is_some() {
            return Ok(());
        }
        if self.editor.caret() == 0 {
            return Ok(());
        }
        let at = self.editor.caret() as usize - 1;
        // The byte about to be deleted, captured first: FR-1.2 zeroes it, so afterwards there is
        // nothing for `TextCounts` to count. `Editor::backspace` deletes exactly one byte
        // (`editor.rs:729`), so one byte is read -- no char-length lookup, because this editor's caret
        // is byte-addressed.
        //
        // A **stack** array, copied out before the delete. Holding a borrow of `self.editor` across
        // the delete is E0502, and the borrow has to end there because `TextCounts` must be told the
        // bytes *after* the document has changed. `editor.rs:778` reads a byte the same way.
        let mut one = [0u8; 1];
        let n = self.editor.read_into(at, &mut one).unwrap_or(0);
        self.editor.backspace()?;
        self.counts.after_delete(&self.editor, at, &one[..n]);
        self.after_edit(1)
    }

    fn delete_forward(&mut self) -> Result<(), SessionError> {
        // **The same rule as `backspace`, and the same reason.** A Delete with a selection deletes
        // the selection, which is what every editor does and is the only behaviour that makes
        // "select, then press Delete" work without holding a different key.
        if self.delete_selected()?.is_some() {
            return Ok(());
        }
        if self.editor.caret() as usize >= self.editor.text_len() {
            return Ok(());
        }
        let at = self.editor.caret() as usize;
        let mut one = [0u8; 1];
        let n = self.editor.read_into(at, &mut one).unwrap_or(0);
        self.editor.delete_forward()?;
        self.counts.after_delete(&self.editor, at, &one[..n]);
        self.after_edit(1)
    }

    fn undo(&mut self) -> Result<(), SessionError> {
        // `NothingToUndo` is a *keymap* condition, not a document failure: pressing undo with
        // nothing to undo should be swallowed, not reported. Same for redo.
        //
        // **Phase 11 item 4: undo and redo rescan rather than deltifying.** `Editor::undo` returns an
        // `EditOutcome` with an offset and a length but not the bytes, and the bytes are what the seam
        // arithmetic needs -- the run is either being put back or taken away, and which one is a
        // question only the undo stack can answer. So this is the one edit path that pays `O(document)`.
        //
        // That is a deliberate trade, not an oversight: undo is not the keystroke the 0.50 ms budget is
        // about, it happens perhaps once per ten keystrokes, and a rescan cannot be wrong. The
        // alternative -- threading the bytes out of `UndoStack` -- would put an undo-format detail into
        // the counting path, where a format change becomes a counting bug.
        match self.editor.undo() {
            Ok(_) => {
                self.recount_counts();
                self.after_edit(0)
            }
            Err(EditorError::NothingToUndo) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    fn redo(&mut self) -> Result<(), SessionError> {
        match self.editor.redo() {
            Ok(_) => {
                self.recount_counts();
                self.after_edit(0)
            }
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
    ///
    /// ## CORRECTION, part 28 — this moved nothing
    ///
    /// It was four assignments to chrome state and no movement of `editor.caret()`:
    ///
    /// ```text
    /// let line = (self.state.caret_line as i64 + delta).max(0) as u32;
    /// self.state.scroll_line = scroll.min(line);
    /// self.state.caret_line = line;
    /// self.damage = self.chrome.full_damage();
    /// ```
    ///
    /// **So `Up` and `Down` drew the caret on a line it did not occupy, and typing after either one
    /// inserted at the old offset.** `tests/caret_line_moves.rs` is the reproduction.
    ///
    /// ## Why it went unnoticed
    ///
    /// **Because every gate that used Up or Down asserted on `state.caret_line`** — the field the bug
    /// wrote and the document did not have. **The fixture and the defect were the same number**, which
    /// is part 22's lesson restated: *"a fixture whose two quantities coincide cannot tell a correct
    /// answer from a wrong one."*
    ///
    /// ## Why the column is not preserved
    ///
    /// **Preserving it needs the target line's length, which needs its bytes, and this path has no
    /// source.** `apply` takes no `&mut dyn LeafSource` — 43 gate sites depend on that — so clamping a
    /// column to a line that may not be resident would be a guess. **Down lands at the start of the
    /// line, which is correct and is not what every editor does.** Real editors keep the column; that
    /// is recorded as not done rather than done badly.
    fn move_line(&mut self, delta: i64) -> Result<(), SessionError> {
        self.move_by_line(delta, false)
    }

    /// `Up`/`Down`, or the shift variants. Part 28.
    ///
    /// **One function and a flag, where the keymap has six variants.** The flag is a parameter here
    /// rather than a variant because there is exactly one caller of each and they are three lines
    /// apart; `Command`'s own docs argue for variants at the *dispatch* boundary, where a caller has to
    /// destructure them, and not for duplicating a body.
    fn move_by_line(&mut self, delta: i64, extend: bool) -> Result<(), SessionError> {
        let line = self.line_index(self.editor.caret() as usize);
        // **From the document, not from `state.caret_line`.** The state is a view of this; reading the
        // view to decide where to move the thing it is a view of is the inversion that let the old
        // version drift. `saturating_sub(1)` because `total_lines` is a count and at least one line
        // always exists, so `last` cannot underflow.
        let last = self.state.total_lines.saturating_sub(1);
        let want = (line as i64 + delta).clamp(0, i64::from(last)) as u32;
        if want == line {
            // **A movement that cannot happen still collapses a plain selection.** Down on the last
            // line is a caret move like any other, and the model says caret moves collapse — leaving
            // it would make it the one arrow key that keeps a selection.
            if !extend {
                let at = self.editor.caret() as usize;
                self.caret_to(at)?;
            }
            return Ok(());
        }
        let at = self.lines.line_begin(want as usize) as u32;
        if extend {
            self.extend_to(at)?;
        } else {
            self.caret_to(at as usize)?;
        }
        Ok(())
    }

    /// **Extend the selection's far end by one character.** Part 28.
    ///
    /// **Separate from [`move_caret`](Self::move_caret) rather than a flag on it**, because the two
    /// differ in what happens at a document end: a plain move stops, and an extend stops too, but a
    /// plain move also *drops the selection* on the way — and `Editor::park` is what does that, so a
    /// shared body would have to clear the selection it is about to extend.
    fn extend_caret(&mut self, delta: i64) -> Result<(), SessionError> {
        let at = self.editor.caret() as i64;
        let want = at + delta;
        if !(0..=self.editor.text_len() as i64).contains(&want) {
            return Ok(());
        }
        self.extend_to(want as u32)
    }

    /// **Scroll the page so `line` is on it, if it is not already.** Part 28.
    ///
    /// **Extracted from `move_line`, and given to `caret_to` as well.** The old code had this
    /// arithmetic in one place and only one caller reached it, so **Right past the bottom of the
    /// window scrolled nothing** — the caret walked off the page and typing there scrolled only
    /// because `after_edit` happens to scroll. **That is the same defect as the one above**: a caret
    /// move that updates one thing and not the other.
    ///
    /// **`rows == 0` returns rather than subtracting**, because `rows - 1` would be the underflow the
    /// project's `for_size` docs describe, and a window with no text rows is reachable by dragging.
    fn scroll_line_into_view(&mut self, line: u32) {
        let rows = self.chrome.layout.rows;
        if rows == 0 {
            return;
        }
        let first = self.state.scroll_line;
        let last = first.saturating_add(rows);
        let scroll = if line < first {
            line
        } else if line >= last {
            line.saturating_sub(rows - 1)
        } else {
            first
        };
        self.state.scroll_line = scroll;
    }

    /// **Style the selected range, or return `false` if there is nothing selected.** Part 28.
    ///
    /// ## Why this is what part 24 said selection would unlock
    ///
    /// `emit_body_text`'s own comment records the debt:
    ///
    /// > `Session::pending_style` is the same fact in the model and the toolbar, and it is a *pending*
    /// > style — it is what the next character will be. Turning existing text bold needs a range, and
    /// > there is no range to apply it to.
    ///
    /// **And `Editor::style_range` has taken `(start, end, flags, colour)` since phase 6.** The
    /// function was there and correct; the model had nothing to hand it. This is the whole of what
    /// selection buys, and it is why it was worth building rather than a highlight.
    ///
    /// ## Why it returns `bool` rather than acting or not
    ///
    /// **Because "there was nothing to style" and "styled it" are two answers to one question**, and
    /// the caller needs to pick between arming and styling. A `()` return would force the caller to
    /// ask `editor.selection()` again, which is the same fact read twice and could differ if anything
    /// moved in between.
    ///
    /// ## Why the selection *stays* selected afterwards
    ///
    /// **Because restyling does not consume a selection.** Typing does; this does not. A user who
    /// selects a paragraph, bolds it and then selects the next one has not lost anything by the first
    /// selection being still there — and clearing it would make a second Bold press (to un-bold) do
    /// nothing, because there would be no range left to un-bold.
    fn style_selection(
        &mut self,
        bold: bool,
        italic: bool,
        colour: u32,
    ) -> Result<bool, SessionError> {
        let Some(sel) = self.editor.selection() else {
            return Ok(false);
        };
        let mut flags = 0u16;
        if bold {
            flags |= STYLE_BOLD;
        }
        if italic {
            flags |= STYLE_ITALIC;
        }
        // **Over the selected range only, never the arming flag.** `pending_style` stays where it was:
        // a user who has Bold armed and then selects a word and presses Bold has made the *next*
        // character bold too, which is what every editor does and is not a decision this should make
        // silently in either direction.
        // **`colour` and not `0`, which is part 29.** Part 24 wired this call with a literal zero
        // because nothing could produce anything else; `TextIntervalSpan::color_rgb` had been in the
        // record since phase 4 with nothing to put in it.
        self.editor.style_range(sel.start, sel.end, flags, colour)?;
        self.after_edit(0)?;
        Ok(true)
    }

    /// **Re-read everything about the caret that is derived from where it is.** Part 28.
    ///
    /// ## The tail of [`caret_to`](Self::caret_to), without the move
    ///
    /// A caret that has *moved* needs the state rebuilt. **A caret some other code has already
    /// moved -- an edit, say -- needs exactly the same rebuild, and must not be moved again**,
    /// because moving is what collapses a selection.
    ///
    /// **This exists because `after_edit` called `caret_to(self.editor.caret())`,** which was correct
    /// for as long as moving the caret had no side effect beyond moving it. It now does. See
    /// [`after_edit`](Self::after_edit) for the whole correction.
    ///
    /// **No scroll here, and the absence is the point.** `Session::new` calls this to make the chrome
    /// agree with an editor whose caret is already at the end of the document, and a new session must
    /// show the *top*. Every caller that moves the caret scrolls afterwards, explicitly, because
    /// "where is the caret" and "where should the page be" are different questions and only the second
    /// one has an exception.
    ///
    /// **`caret_to` is this function plus `editor.caret_to`, in that order** -- move, then rebuild --
    /// and the two halves are the same in both callers, which is the point of taking them apart.
    fn reconcile_caret(&mut self) -> Result<(), SessionError> {
        let _ = self.refresh_caret_column(None);
        // **The pen here as well as in `paint_with`, and that is the point of part 33.** The column
        // is refreshed on every caret move so that the state a gate reads after `caret_to` is the
        // state the frame would be drawn from -- part 22's argument. **The pen is a second field of
        // exactly that kind, so it is refreshed in exactly the same places**; leaving it to the paint
        // path alone made `Caret::locate` answer from a stale pen, which is the shape of part 22's
        // defect wearing a new hat: two fields, one refreshed and one not.
        let _ = self.refresh_caret_pen(None);
        self.sync_selection();
        self.state.caret_line = self.line_index(self.editor.caret() as usize);
        let cell = self.caret_cell();
        if let Some(rect) = cell {
            self.damage = self.damage.union(&rect);
        }
        Ok(())
    }

    /// **Copy the editor's selection into the state the chrome paints.** Part 28.
    ///
    /// ## Why a one-line sync and not a stored selection
    ///
    /// **Because `ChromeState.selection` is a *view* and `Editor` owns the fact**, and the two must not
    /// be allowed to disagree. Part 22 is the precedent: `ChromeState::caret_line` was built once by
    /// `Session::new` and then left alone for twelve phases, and the reason nothing noticed is the
    /// kind of thing that hides bugs — both halves of the arithmetic it fed were zero.
    ///
    /// **This is called from `caret_to` and from `paint_with`, and those are the only two places the
    /// caret moves or the frame is built.** Anything else that moved the caret and forgot this would
    /// be a third site, and the way to make that impossible is to have no way to move the caret that
    /// does not go through `Session::caret_to` — which is true today and is the property worth
    /// preserving.
    ///
    /// ## Why it is `Option<SelectionRange>` and the editor's own value
    ///
    /// **Because the chrome cannot author a selection**, and re-deriving the range here from an anchor
    /// and a focus would be authoring one. The editor's `SelectionRange` already normalises the
    /// direction and refuses a degenerate range, so `state.selection` is right or `None` and there is
    /// no third case to handle.
    fn sync_selection(&mut self) {
        self.state.selection = self.editor.selection();
    }

    /// Put the caret at `at`, clamped back to a codepoint boundary.
    ///
    /// **Public as of Phase 11**, for the same reason `tick` and `apply` are: a driver -- or a gate --
    /// that positions the caret needs to move it without inventing an edit to move it with. The latency
    /// gate in `tests/session_latency.rs` places the caret at three offsets and measures the keystroke
    /// that follows, and doing that through `Command::Insert` would have measured an insertion at a
    /// guessed offset rather than at the one being tested.
    pub fn caret_to(&mut self, at: usize) -> Result<(), SessionError> {
        // The caret's old cell, before anything moves. Read from the *state* rather than by locating
        // it, because the state is what `Caret::locate` will use and re-deriving it after the move
        // would give the new position, which is the one thing that is not yet stale.
        let old_cell = self.caret_cell();
        self.editor.caret_to(at)?;
        // The caret's *column* within its line, for the status bar and for `Caret::locate`'s cell.
        //
        // **This is a scalar count and not a byte offset, as of part 22**, and the call carries no
        // source because the keystroke path cannot take one: `Session::apply` is public *so that* a
        // driver with its own event source can drive it, and a `&mut dyn LeafSource` parameter would
        // undo that — 43 gate sites and the documented reason for the method's visibility.
        //
        // **`None`, so this is a resident read.** On a document that is entirely in memory — every
        // gate, and every document shorter than the resident budget — that is a memcpy and the count
        // is exact. On a container-backed document whose caret line is not resident it cannot be, and
        // `refresh_caret_column` falls back to the byte count. `paint_with` recomputes it with a
        // faulting source before the chrome's tree, so the value the *renderer* uses is exact
        // whenever the line is painted — which is whenever the caret can be drawn at all. See
        // `refresh_caret_column` for why there is one function and two capabilities rather than two
        // implementations.
        let _ = self.refresh_caret_column(None);
        // **The pen here as well as in `paint_with`, and that is the point of part 33.** The column
        // is refreshed on every caret move so that the state a gate reads after `caret_to` is the
        // state the frame would be drawn from -- part 22's argument. **The pen is a second field of
        // exactly that kind, so it is refreshed in exactly the same places**; leaving it to the paint
        // path alone made `Caret::locate` answer from a stale pen, which is the shape of part 22's
        // defect wearing a new hat: two fields, one refreshed and one not.
        let _ = self.refresh_caret_pen(None);
        self.sync_selection();
        self.state.caret_line = self.line_index(self.editor.caret() as usize);
        // **And the page follows the caret.** Part 28: `scroll_line_into_view` used to live inside
        // `move_line`, whose only other effect was to *not* move the caret -- so Right past the bottom
        // of the window scrolled nothing, and only typing scrolled, because `after_edit` happens to.
        self.scroll_line_into_view(self.state.caret_line);
        // **A caret move is a repaint, and this used not to be one.**
        //
        // `caret_to` set no damage, and `tick` only paints when damage is non-empty, so pressing an
        // arrow key moved the caret in the model and left the pixels alone. The old cell stayed drawn
        // and the new one did not appear until the *blink* happened to toggle, at
        // `Blink::DEFAULT_PERIOD`. Every scripted gate missed it because the gates assert on
        // `Caret::locate`'s arithmetic and never on a frame after a bare movement.
        //
        // Phase 9B found it in a live window, and in the worst possible way to find it: this is the
        // repaint that switches a formula from compiled to raw. Leaving a formula with Right changed
        // every number in `SessionStats` except the ones the frame was drawn from, so the window kept
        // showing the raw source while the log said the caret was outside.
        //
        // Both cells are damaged, old and new: moving within a line leaves a trail otherwise, which is
        // the same reasoning as `after_edit`'s comment about the old caret's line.
        let new_cell = self.caret_cell();
        let moved = union_opt(old_cell, new_cell);
        if let Some(rect) = moved {
            self.damage = self.damage.union(&rect);
        }
        Ok(())
    }

    /// The byte offset of the start of the line containing `at`. **O(log n).**
    ///
    /// **Phase 11.** Three implementations have stood here, and the history is the reason for the
    /// comment. First `self.editor.text()` — which allocates a `Vec` the size of the whole document and
    /// copies it byte-for-byte, so a function whose comment claimed "O(line length)" was in fact
    /// `O(document)` and allocating, twice per keystroke via [`Session::caret_to`]. Then a chunked
    /// backward scan through `read_into`, which removed the allocation and left the scan. Now two
    /// Fenwick trees, which remove the scan.
    ///
    /// `Editor` deliberately does not own line geometry, so the session holds it. See the layering note
    /// in `holonomy-input`: a `Command::Up` means "move up" and nothing about how many bytes that is.
    fn line_start(&self, at: usize) -> usize {
        self.lines.line_start(at)
    }

    /// The 0-based line index containing `at`. **O(log n).**
    ///
    /// Same three implementations as [`Session::line_start`], for the same reasons. The scan this
    /// replaced cost `O(bytes before the caret)` — at 3.5 MiB, ~900 `read_into` calls, which is
    /// microseconds and therefore most of the 0.50 ms keystroke budget spent on answering a question a
    /// tree answers in sixteen comparisons.
    fn line_index(&self, at: usize) -> u32 {
        self.lines.line_of(at)
    }

    /// Record an edit's consequences: counts, status bar, and the damaged line.
    ///
    /// **The counts are *not* refreshed here.** They are folded forward by the caller --
    /// `Session::insert`, `backspace`, `delete_forward` and `outdent` each call `TextCounts` with the
    /// bytes they moved, and `undo`/`redo` rescan. This function therefore does the two things that are
    /// position-independent: move the caret, and reconcile the line geometry.
    ///
    /// ## CORRECTION, part 28 -- `caret_to` here was a refresh written as a move
    ///
    /// It read:
    ///
    /// ```text
    /// self.caret_to(self.editor.caret() as usize)?;
    /// ```
    ///
    /// **Which is a caret move to where the caret already is**, and that is a refresh wearing a
    /// movement's clothes. The moment [`Editor::park`] made every real caret move collapse the
    /// selection, **every edit collapsed it too** -- including the ones that had no business touching
    /// it. `style_selection` bolded the selected range and then lost the selection, so a second Bold
    /// press could not un-bold: it armed bold for the next keystroke instead.
    ///
    /// **The caret is already where it should be when this runs** -- `insert`, `backspace` and
    /// `delete_selection` have all parked it. What is wanted is the *tail* of `caret_to`: re-read the
    /// column, re-read the line, scroll, damage. That is [`reconcile_caret`](Self::reconcile_caret),
    /// and saying so is cheaper than a flag on `caret_to` that exactly one caller would pass.
    fn after_edit(&mut self, inserted: u32) -> Result<(), SessionError> {
        self.stats.edits += 1;
        self.reconcile_caret()?;
        // **Scroll after reconciling, not inside it.** `Session::new` also reconciles and must not
        // scroll; an edit moves the caret like a keystroke does and must. See `reconcile_caret`.
        self.scroll_line_into_view(self.state.caret_line);
        // The line geometry must agree with the document before anything reads it, and `caret_to` has
        // already used it above — so the order here is not free. **This is the fix for a latent bug,
        // not only an optimisation**: `line_index` used to count newlines, so it was correct whatever
        // the geometry said; now it is a tree lookup, and a tree that a previous edit left stale
        // answers with the *previous* document's line numbers. Every edit syncs, so the tree is never
        // stale at the top of this function.
        self.sync_lines();
        // **And the section manifest**, for the same reason and at the same point: `DocLines` answers line
        // questions and `Manifest` answers *"does this document contain a formula, table or image?"*,
        // which is the question four emitters ask before deciding whether to read the whole document.
        // Synced here rather than lazily because a manifest that is stale about `span_total` would make
        // `publish_line_heights` skip a formula that exists, and the symptom would be a formula that
        // silently stops rendering.
        self.sync_manifest();
        // And the status bar's totals, from the deltas the caller folded. O(1): this is a three-field
        // copy out of `TextCounts`, not a recount. See `publish_counts` for why the two maintained
        // quantities are published at their own points rather than together.
        self.publish_counts();
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

    /// Bring the line geometry back into agreement with the document. Phase 11.
    ///
    /// Delegates to [`DocLines::sync`], which decides between a one-point update and a rebuild by
    /// comparing the document's newline count with the geometry's line count. **It is called from
    /// `after_edit` only** — every mutation in the session goes through there, including undo, redo,
    /// table rows and formulas, which is why the sync is driven by a comparison rather than by being
    /// told what each caller did.
    ///
    /// One thing this deliberately does *not* do: recompute the heights tree. `LineGeometry`'s heights
    /// are uniform per line today because a line's height is `cell_h` for ordinary text, and the
    /// variable-height blocks (tables, formulas, images) are handled by `LineHeights::from` on the
    /// paint path. Wiring real per-line heights is Phase 12's, with `Painter::text`.
    ///
    /// **Public as of Phase 11**, for the reason `tick` and `apply` are: the latency diagnostic in
    /// `tests/session_latency.rs` times this step on its own, and a diagnostic that cannot name the
    /// thing it is measuring has to be deleted rather than maintained.
    pub fn sync_lines(&mut self) {
        // **The line count comes from `TextCounts`, not from a scan.** This function used to read the
        // whole document to count newlines, which put a 3.1 MiB read back on the keystroke path inside
        // the one function whose purpose is to remove `O(document)` work -- measured at 3,906 µs.
        // `TextCounts` already maintains the newline count as a delta, so it is asked instead.
        let expected = self.counts.lines() as usize;
        let caret = self.editor.caret() as usize;
        let metrics = holonomy_geometry::LineMetrics::default();
        match self.lines.sync(&self.editor, expected, caret, metrics) {
            Sync::OneLine => self.stats.line_updates += 1,
            Sync::Rebuilt => self.stats.line_rebuilds += 1,
            Sync::Unchanged => {}
        }
    }

    /// The current word and line totals, from a full rescan. Phase 11 item 4.
    ///
    /// The repair path, and the one place `after_edit` no longer calls. See
    /// [`Session::recount_words_and_lines`] for why it stays.
    pub fn recount_counts(&mut self) {
        self.recount_words_and_lines();
    }

    /// Bring the section manifest back into agreement with the document. Phase 13.
    ///
    /// # Three sections per keystroke, and where the 6 MiB went
    ///
    /// [`Manifest::sync`] takes the [`Editor`] rather than the document's bytes **so that it can read three
    /// sections instead of the whole document**. That is the whole reason it is not `fn sync(&[u8])`: a
    /// slice-based version is correct and costs 6 MiB per keystroke, which is precisely the number Phase 11
    /// spent a phase removing. Three sections is 196 KiB — a 32x reduction — and `read_into` allocates
    /// nothing, so there is no `Vec` growth either.
    ///
    /// **The rebuild branch is the only whole-document read on the keystroke path**, and it happens when a
    /// line appears or vanishes, because fixed-size sections shift. Same trade as `DocLines::sync`, in a
    /// second currency, and one keystroke in forty.
    pub fn sync_manifest(&mut self) {
        let caret = self.editor.caret() as u32;
        let mut scratch = std::mem::take(&mut self.manifest_scratch);
        let out = self.manifest.sync(&self.editor, caret, &mut scratch);
        self.manifest_scratch = scratch;
        match out {
            crate::manifest::Sync::OneSection => self.stats.section_updates += 1,
            crate::manifest::Sync::Rebuilt => self.stats.section_rebuilds += 1,
            crate::manifest::Sync::Unchanged => {}
        }
    }

    /// Copy the maintained totals into the state the status bar draws from.
    ///
    /// **O(1)**, and the reason the status bar keeps working while the recount stopped running on the
    /// keystroke path. `ChromeState::words` and `total_lines` are what `Chrome::tree` reads; the
    /// maintained `TextCounts` is what is correct. Publishing is the one-way bridge between them, and it
    /// is a three-field copy rather than a recount.
    ///
    /// Kept separate from `sync_lines` so the two *independent* maintained quantities are published at
    /// their own points: if either is wrong the other does not mask it.
    fn publish_counts(&mut self) {
        self.state.bytes = self.editor.text_len() as u32;
        self.state.words = self.counts.words;
        self.state.total_lines = self.counts.lines();
    }

    /// Recount the words and bytes the status bar shows.
    ///
    /// **Phase 11 item 4, and this is the method item 4 exists to remove from the keystroke path.** It is
    /// `O(document)`: a byte at a time over the whole text. Measured at 12.9 ms of a 3.1 MiB document's
    /// keystroke, which is 26x the entire 0.50 ms budget spent on numbers that only appear in a status
    /// bar.
    ///
    /// Kept as the **repair path**, not the update path: [`Session::counts`] maintains the totals as
    /// deltas and calls this only to rebuild from scratch -- at construction, and from `undo`/`redo` and
    /// the editor-level inserts (`insert_table`, `insert_image`, `append_table_row`) that do not hand back
    /// the bytes they wrote. It is also what `counts.rs`'s own tests compare against, which is what makes
    /// it worth keeping at all.
    pub fn recount_words_and_lines(&mut self) {
        self.counts = TextCounts::scan(&self.editor);
        self.publish_counts();
    }

    // ---------------------------------------------------------------- paint

    /// Repaint `damage` and present, **with no byte source** -- a session over an in-memory editor.
    ///
    /// **Every gate in this workspace uses this**, because a resident document's `read_into` always
    /// succeeds and there is nothing to fault. The product path uses [`paint_with`](Self::paint_with).
    /// The two are the same function: this one passes a [`NoSource`](crate::store::NoSource), which
    /// refuses, which is a no-op on a document whose bytes are already held.
    pub fn paint(&mut self, damage: Option<DamageRect>) -> Result<(), SessionError> {
        self.paint_with(damage, &mut crate::store::NoSource)
    }

    /// Repaint `damage` and present, fetching bytes from `source` when a leaf is not resident.
    ///
    /// # This is part 16's whole change, and it is one argument
    ///
    /// The emitters used to read through `&self` and `break` on an absent leaf: `page_used` stopped
    /// growing, `PageText::document` answered `None` past that point, and each answer was counted in
    /// `runs_missing`. **A page of correct text with a blank tail past the resident window**, which is
    /// what a container-backed document showed on every page but the first.
    ///
    /// `&mut dyn LeafSource` is a parameter rather than a field because a `SectionStore` borrows the
    /// container and a store owned by the session would be a self-referential struct. **The product
    /// builds one at the call site** (`main.rs`) and holds the container beside the session -- two
    /// disjoint fields, so the borrow is legal and no lifetime surgery is needed.
    ///
    /// **Faulting a paint is correct and not a side effect**: the rope's record translates a window's
    /// saved bytes into current ones, and *fetching* a leaf changes nothing about the document. See
    /// `Rope::fault_leaf`.
    pub fn paint_with(
        &mut self,
        damage: Option<DamageRect>,
        source: &mut dyn holonomy_text::LeafSource,
    ) -> Result<(), SessionError> {
        // The caret goes *under* the page's text in paint order, so the caret's rect is erased by
        // repainting the page and then redrawn -- which is why the damage includes it whenever it
        // moves.
        // The line-height model is rebuilt from the tables *before* the chrome's tree, because the
        // chrome's tree and `Caret::locate` both read it: a table that pushed the lines below it down
        // but was published afterwards would move the text and leave the caret behind.
        self.publish_line_heights(source);
        // **The caret's column is recomputed here, with a faulting source, immediately before the
        // chrome's tree — and that position in this function is the whole argument for it.**
        //
        // `Caret::locate` runs inside `chrome.tree` and reads `state.caret_column` to place the
        // caret's cell, so **whatever this call leaves is what gets drawn.** The `None` read in
        // `caret_to` is best-effort because the keystroke path cannot take a source; this one is not,
        // and it runs on the frame that draws, so a caret on a line that was not resident when it
        // moved is still drawn under the right glyph.
        //
        // **A read error here does not fail the paint.** `None` leaves the previous value, which is
        // the byte count — a status bar number, not a reason to lose a frame over.
        let _ = self.refresh_caret_column(Some(&mut *source));
        // **The pen, in the same place and immediately after, because `chrome.tree` below reads it.**
        //
        // **`Caret::locate` runs inside `chrome.tree`**, so whatever this leaves is what gets drawn —
        // the same argument `refresh_caret_column` is called there for, and the reason neither is
        // hoisted out of this function. `None` leaves the previous value, and `Caret::locate` falls
        // back to the grid, so a failed read costs a caret 10 px out rather than a frame.
        let _ = self.refresh_caret_pen(Some(&mut *source));
        self.sync_selection();
        let mut tree = self.chrome.tree(&self.state);
        // Tables are emitted *into* the chrome's tree rather than into a tree of their own, because
        // they have to be painted in the page's coordinate space and clipped by the same damage the
        // chrome uses. A table that lands outside the viewport contributes nothing and is not visited.
        let mut damage = damage;
        // **Each emitter is passed the source by reborrow**, so all four see the same store and none of
        // them can hold it across the next one's call. `&mut *source` rather than `source`: the emitters
        // take `&mut dyn LeafSource`, and moving the `&mut` would spend it.
        self.emit_tables(&mut tree, &mut damage, &mut *source);
        self.emit_math(&mut tree, &mut damage, &mut *source);
        self.emit_images(&mut tree, &mut damage, &mut *source);
        // **Body text last among the emitters, so it draws over the others.** A table, a formula and an
        // image are all anchored to a document line, and the body text of that line is what a reader
        // expects to see *behind* them -- a table whose cells are empty still has the paragraph that
        // introduced it around it. Emitting the body first would put the paragraph's glyphs on top of
        // the table's borders.
        self.emit_body_text(&mut tree, &mut damage, source);
        // The raster source is borrowed *for this call* and not held: the cache is mutated by the next
        // frame's decode and eviction, so a borrow that outlived the paint would be a self-referential
        // `Session` -- the painter is a field and so is the cache it would have to point at.
        // **The page buffer is the text source**, and it is a field borrow rather than a `&self` borrow,
        // so `&mut self.frame` and `&self.images` are live at the same time. `PageText` is a one-field
        // wrapper over a slice; see its docs for why the trait exists at all.
        let stats = {
            let page = PageText(&self.line_scratch[..self.page_used]);
            self.painter.paint_with(
                &mut self.frame,
                &tree,
                damage,
                Some(&self.images),
                Some(&page),
            )?
        };
        self.last_paint = stats;
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
    fn publish_line_heights(&mut self, source: &mut dyn holonomy_text::LeafSource) {
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
        // Formulas join the tables in the *same* model, for the same reason: a fraction is 41 px tall
        // where a line is 18, so the lines below it have to move or the formula draws over them.
        // `LineHeights::from` merges two blocks that share a line by adding, so a table and a formula
        // starting on one line displace by the sum rather than one overwriting the other.
        blocks.extend(self.math_blocks_for(source));
        // Images join the tables and the formulas in the *same* model, for the same reason: an image
        // is taller than any line, so the lines below it have to move or it draws over them. The block
        // height is the *raster's* height, not the source image's -- §2.9.3 makes those different, and
        // using the source height would displace by a factor of three at 1920x1080.
        blocks.extend(self.image_blocks(source));
        self.state.line_heights = holonomy_render::LineHeights::from(pitch, &blocks);
    }

    /// Every visible image's `(anchor_line, raster_height_px)`, for [`Session::publish_line_heights`].
    ///
    /// Shares `emit_images`' scan rather than repeating it, because the displacement and the draw have
    /// to be derived from the same numbers: a line moved by one height and an image drawn at another is
    /// a bug that looks like a layout choice.
    ///
    /// Returns nothing when the document has no images, which is the common case and costs one `is_empty`
    /// rather than a scan.
    fn image_blocks(&mut self, source: &mut dyn holonomy_text::LeafSource) -> Vec<(u32, u32)> {
        if self.editor.assets().is_empty() {
            return Vec::new();
        }
        // **And the manifest's span count, which covers anchors as well as formulas.** `scan_anchors` is
        // the other byte cursor that forces a whole-document read, and it is looking for a marker the
        // manifest already counted. Same trade as `math_blocks_for`: free, exact, wrong only if the
        // manifest is stale.
        if self.manifest.span_total() == 0 {
            return Vec::new();
        }
        let Ok(read) = read_document(&mut self.editor, &mut self.doc_scratch, source) else {
            return Vec::new();
        };
        // **The bytes live in `doc_scratch`, and this borrows that field alone.**
        // `read_document` returns a *length* rather than a slice precisely so this borrow and the
        // `editor` borrows in the loops below stay disjoint -- a returned `&[u8]` tied to
        // `&mut Editor` would fuse them, and every `self.editor` use inside would be a conflict.
        let text = &self.doc_scratch[..read];
        let mut out = Vec::new();
        for (ordinal, at) in holonomy_text::scan_anchors(text).into_iter().enumerate() {
            let Ok(asset) = self.editor.assets().get(ordinal) else {
                continue;
            };
            let (w, h) = self.raster_size(asset.width as u32, asset.height as u32);
            let _ = w;
            out.push((self.line_index(at as usize), h));
        }
        out
    }

    /// The page-column-width raster size for a source of `src_w` by `src_h`.
    ///
    /// §2.9.3: the cache holds **page-column-width** rasters, not native ones, because a ±1-page policy
    /// with native decoding holds exactly one 1080p image and the second photo on facing pages breaches
    /// the budget. So a source is downscaled to `image_column_width` wide and its height follows the
    /// aspect ratio, rounded up so a raster is never one pixel short of the row it has to cover.
    ///
    /// A source already narrower than the column is **not** enlarged. Upscaling a 200 px GIF to 640 px
    /// spends 921,600 bytes of an 8.0 MiB budget on 16x more pixels of the same information, and the
    /// painter's 1:1 blit draws it at whatever size it is.
    fn raster_size(&self, src_w: u32, src_h: u32) -> (u32, u32) {
        let column = self.image_column_width.max(1);
        if src_w <= column {
            return (src_w.max(1), src_h.max(1));
        }
        // `u64` because `src_h * column` overflows `u32` for a 40000 px panorama, which a PNG's `u32`
        // IHDR permits and this must not panic on.
        let h = u64::from(src_h)
            .saturating_mul(u64::from(column))
            .saturating_add(u64::from(src_w) / 2)
            / u64::from(src_w);
        (column, h.max(1).min(u32::MAX as u64) as u32)
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
    fn emit_body_text(
        &mut self,
        tree: &mut SurfaceTree,
        damage: &mut Option<DamageRect>,
        source: &mut dyn holonomy_text::LeafSource,
    ) {
        self.stats.lines_drawn = 0;
        self.stats.glyphs_drawn = 0;

        let m = self.chrome.metrics;
        // The **text column**, not the layout: every other emitter works in the text rect's coordinates,
        // and mixing the two is how a run ends up on the page margin.
        let l = self.chrome.layout.text;
        let first = self.state.scroll_line;
        let last = (first + self.chrome.layout.rows).min(self.state.total_lines);
        if first >= last {
            return;
        }
        // The page's measure, in whole cells. The wrap point is by *cell count* rather than by measured
        // pixels because the cell is what every other part of the renderer agrees on: `Caret::locate`
        // puts the caret at `column * cell_w`, damage rects are `cell_h` tall, and the tables lay
        // themselves out on cells.
        let cells_per_row = (l.width / m.cell_w.max(1)).max(1) as usize;
        let ink = holonomy_render::chrome::colour::INK;
        // **The selection, read once.** Part 28.
        //
        // **Read from `state`, not from `self.editor`,** because this runs *after* `paint_with` synced
        // it and the state is what the rest of the frame agrees with. Reading the editor here would be
        // a second, independent source of truth for the same fact — the exact shape of part 22.
        let selection = self.state.selection;

        // --- Pass 1: read every visible line into one page-sized buffer.
        //
        // **The buffer holds the whole page, not one line, and that is the point.** A `DocRun` names a byte
        // offset that the painter resolves at *paint* time, so the bytes it names have to still be there
        // when the painter runs -- which is after every line has been emitted. A one-line buffer would be
        // correct for one run and wrong for the other 42, and the failure would be silent: the painter
        // would resolve each run against whichever line happened to be last.
        //
        // So the page is read once, contiguously, and every run's offset is into *this* buffer. It is
        // [`LINE_SCRATCH_BYTES`] = 48 KiB, against 6 MiB for `doc_scratch` on a 6 MiB document.
        let mut used = 0usize;
        let mut spans: Vec<(u32, u32)> = Vec::new();
        for line in first..last {
            // **The index-shaped accessors, not `line_start`.** `line_start` takes a byte offset and
            // answers "where does the line containing this byte begin"; a renderer has a line *index*
            // and wants "where does line N begin". Phase 12's first version passed the index to
            // `line_start`, which quietly answered with line 0's offset every time.
            let start = self.lines.line_begin(line as usize);
            let end = self.lines.line_end(line as usize);
            let len = end.saturating_sub(start);
            // The buffer's ceiling is the reason a line can be truncated: a document with one enormous
            // line would otherwise try to read the whole thing. 48 KiB of text is far more than the page
            // can show, so the cull below is what normally stops this, and this is the backstop.
            if used + len > self.line_scratch.len() {
                break;
            }
            // **Faulting, and that is the change.** `read_into` takes `&self` and cannot fetch, so a line
            // past the resident window ended the loop -- one absent leaf truncated the page and every
            // run after it counted as missing. `read_into_faulting` asks the record which saved bytes
            // belong in this line and fetches them, so a page anywhere in the document draws.
            //
            // **`break` still, and is still right**: it is the line-scratch ceiling (48 KiB), a backstop
            // for a document with one enormous line. A fetch failure here means the source could not
            // produce the bytes at all, and there is nothing further down the page to draw.
            if self
                .editor
                .read_into_faulting(source, start, &mut self.line_scratch[used..used + len])
                .is_err()
            {
                break;
            }
            spans.push((used as u32, len as u32));
            used += len;
        }
        self.line_scratch.truncate(used);
        self.page_used = used;

        // --- Pass 2: which lines another emitter already draws.
        //
        // **This is not an optimisation, it is the thing that makes the page readable.** A formula, a
        // table and an image are all *inline in a line of prose*: `emit_math` draws the raw LaTeX or the
        // compiled glyphs at that line's row, and `emit_tables` draws the grid there. If the body-text
        // emitter also drew the line, the two would be painted on top of each other and the reader would
        // see the document's own words and the formula's source superimposed.
        //
        // The failure is silent, and it is *how* the first version of Phase 12 broke two of Phase 9B's
        // math gates: their ink-extent assertions suddenly read 9 px wider, because the extent was the
        // union of the formula and the paragraph around it. Neither gate was wrong — the page was, and the
        // gates were the only things that noticed.
        //
        // **Scanned from the page buffer, not from the document, and that is sound** because a formula
        // span never crosses a line: `for_each_math_span`'s own docs say an unpaired `$$` "runs to the
        // end of its line", so every span lies inside one line, and an image anchor is a fixed byte
        // sequence. Tables are the exception — a table *is* several lines — and those come from the
        // editor's own span map rather than from a scan, so they cost nothing to enumerate.
        //
        // The first version scanned `doc_scratch`, which meant this emitter read the whole document and
        // undid the page buffer it had just built. That is the specific way the "1.000 bytes per
        // document byte" claim was false for its own emitter, and it is why the scan is here rather than
        // left as a call to `read_document`.
        let mut claimed: Vec<u32> = Vec::new();
        for span in self.editor.tables().spans() {
            let a = self.line_index(span.start_byte as usize);
            let b = self.line_index(span.end_byte.max(span.start_byte) as usize);
            for line in a..=b {
                claimed.push(line);
            }
        }
        holonomy_text::for_each_math_span(&self.line_scratch[..used], |span| {
            claimed.push(first + line_of_span(&spans, span.start as usize));
        });
        for at in holonomy_text::scan_anchors(&self.line_scratch[..used]) {
            claimed.push(first + line_of_span(&spans, at as usize));
        }
        claimed.sort_unstable();
        claimed.dedup();

        // --- Pass 3: one run per line, positioned by the line model.
        let mut extra: Option<DamageRect> = None;
        let line_heights = self.state.line_heights.clone();
        let mut lines = 0u32;
        let mut glyphs = 0u32;
        // --- Part 26: the page's styled runs, fetched once.
        //
        // **`spans` is the visible page's `(at, len)` pairs**, so its first and last entries bound the
        // byte range the page covers, and one `runs_in` over that range answers every line's question.
        // Before this the call sat inside the loop and a forty-line page paid for it forty times --
        // forty allocations and forty walks of the span list.
        //
        // **`take_while(...).filter(...)` per line is the partition, and it is over the *page's* runs
        // rather than the document's**, so it is a handful of comparisons per line with no allocation.
        // `take_while` rather than `filter` because both lists ascend: once a run starts past this
        // line, no later run can belong to it.
        self.stats.span_queries += 1;
        let page_runs = match (spans.first(), spans.last()) {
            // **The *last* line's end, and not the first line's start plus the last line's length.**
            // That was the first version, and it asked for a range barely past the first line — so no
            // styled run on any other line was found and four of part 24's gates went red at once with
            // "no differing pixels at all". A range built from the wrong two fields of a list is worse
            // than no hoist, because it looks correct.
            (Some(&(lo, _)), Some(&(hi_at, hi_len))) => {
                self.editor.spans().runs_in(lo, hi_at + hi_len)
            }
            _ => Vec::new(),
        };
        for (row, &(at, len)) in spans.iter().enumerate() {
            let line = first + row as u32;
            let y = l.y + line_heights.y(line - first);
            let rect = DamageRect::new(l.x, y, l.width, m.cell_h);
            // **Culled by row range, after the read.** Two comparisons rather than a rectangle
            // intersection: `DamageRect` has no `intersects`, and a general-purpose method used in exactly
            // one place is a method that will be used in a second place with the wrong semantics.
            if let Some(d) = damage {
                if rect.y >= d.bottom() || d.y >= rect.bottom() {
                    continue;
                }
            }
            // **Skip a line another emitter owns.** `claimed` is sorted and deduplicated, so this is a
            // binary search; the set is one entry per formula, table line or image on the page, which is
            // a handful, and a linear scan per row would be O(rows x claims) rather than O(rows log n).
            if claimed.binary_search(&line).is_ok() {
                continue;
            }
            lines += 1;

            let text = &self.line_scratch[at as usize..(at + len) as usize];
            let shown = codepoints_upto(text, 0, cells_per_row);

            // --- The selection highlight, behind the text. Part 28.
            //
            // **Pushed into `tree.before`, and that is what puts it behind the glyphs.** `SurfaceTree`
            // paints `before`, then `node`, then `after`, so a rect pushed here is under every `DocRun`
            // this loop emits — including the styled ones, which are pushed later. Drawn after, the
            // highlight would be a blue box *over* the words it is supposed to be marking.
            //
            // **The geometry is `clip_to` plus a cell count, and the cell count is the same scalar
            // count the caret uses.** `refresh_caret_column` counts scalars from a line start to a
            // caret; this counts scalars from a line start to a selection edge. Both are "how many
            // glyphs are between two offsets on this line", and **both answer from bytes the line
            // already has in `line_scratch`** — so a selection on a 40-line page costs two scans of a
            // buffer that was read anyway and **zero** rope reads.
            //
            // **Two scans and not one, because they are to different places.** `col_of(lo)` and
            // `col_of(hi)` each walk from the line start. Walking once and remembering the distance
            // between would need a mid-line restart anyway, and the second scan is over at most one
            // line's bytes.
            if let Some(sel) = selection {
                if let Some((lo, hi)) = sel.clip_to(at, at + len) {
                    // **The line's offset in the document, which `pen_at` needs** because `runs` is
                    // keyed on document offsets and `text` is a slice of the page buffer.
                    let doc_start = self.lines.line_begin(line as usize) as u32;
                    let lo = (lo - at) as usize;
                    let hi = (hi - at) as usize;
                    // **Two pens, not two column counts.** Part 33. `col0 * cell_w` was the grid and
                    // the caret is now at the pen, so a highlight placed on the grid would start up to
                    // a cell right of the caret at its left edge -- **the caret outside its own
                    // highlight**, which is the defect the caret fix would otherwise have made worse
                    // rather than better.
                    let pen0 = pen_at(&self.painter, text, doc_start, lo, &page_runs);
                    let pen1 = pen_at(&self.painter, text, doc_start, hi, &page_runs);
                    // **`shown` is the measure, and the highlight stops there too.** A line longer
                    // than the page is clipped by `codepoints_upto`, and a highlight running past the
                    // edge of the text would be a blue band over the page margin — which is not a
                    // cosmetic difference, it is a highlight on nothing.
                    // **`shown` is still the measure, and the highlight still stops there.** A line
                    // longer than the page is clipped by `codepoints_upto`, and a highlight running
                    // past the edge of the text would be a blue band over the page margin -- which is
                    // not a cosmetic difference, it is a highlight on nothing. **The clip is on bytes
                    // rather than on columns**, because the pen has no column to clip: the last glyph
                    // the page shows is the last one inside `shown`, and its advance is where the pen
                    // is.
                    let pen1 = pen1.min(pen_at(
                        &self.painter,
                        &text[..shown],
                        doc_start,
                        shown,
                        &page_runs,
                    ));
                    if pen1 > pen0 {
                        push_rect(
                            tree,
                            l.x + pen0,
                            y,
                            pen1 - pen0,
                            m.cell_h,
                            holonomy_render::chrome::colour::SELECTION,
                        );
                    }
                }
            }

            // The terminator is not drawn. A `\n` has a glyph in most faces, so drawing it would put a
            // visible mark at the end of every line.
            let seg = strip_terminator(&text[..shown]);
            if seg.is_empty() {
                extra = union_opt(extra, Some(rect));
                continue;
            }
            // Bytes, not cells: `SessionStats::glyphs_drawn`'s doc comment says why these two figures are
            // not the same number.
            glyphs += seg.len() as u32;
            let shown_len = seg.len() as u32;

            // --- Pass 3a: one run per *style*, not one per line. Part 24.
            //
            // **This used to emit one `DocRun` per line with `Style::REGULAR` hardcoded**, and
            // `Editor::style_at` was called from nowhere in the product -- only from `holonomy-text`'s own
            // tests. So `Session::toggle_bold` wrote style flags into the span map, the flags survived
            // undo, and **nothing ever drew them**: a document with every character set bold painted
            // pixel-for-pixel like the same document unstyled. `crates/holonomy/tests/style_render.rs`
            // measured that before this change and asserts the opposite now.
            //
            // **`SpanMap::runs_in` is what a renderer walks, and its own doc says so** -- "for each
            // visible line, the runs it must draw, each with a colour and an atlas style". It existed,
            // it was correct, and nothing called it.
            //
            // **The runs come from one query for the whole page, hoisted out of this loop. Part 26.**
            // The first version called `runs_in` per line, and that had two costs that a page of forty
            // lines pays forty times:
            //
            // * **One heap allocation per line**, on the path whose invariant is zero — see
            //   `tests/session_no_alloc.rs`, which asserts it for the *edit* path and is explicit that
            //   the paint path was out of scope. Part 24 moved it from out of scope to forty-per-frame.
            // * **A linear scan of the span list per line**, because `runs_in` walks from the start of
            //   the list and breaks only past the end of the range. On a document scrolled deep into a
            //   styled file, that is every span before the page, forty times over.
            //
            // **One query over the page's byte range is O(spans before the page) instead of
            // O(lines x spans before the page), and allocates once.** The per-line partition below is
            // over the *page's* runs -- a handful -- with an early break, so it is cheap and does not
            // allocate.
            //
            // **`SessionStats::span_queries` counts the queries**, so "one per paint" is a measured
            // claim and not a comment. `one_span_query_serves_the_whole_page` is the gate.
            // **A peeking iterator and not a collected `Vec`.** Collecting would be one allocation per
            // line again, which is the thing being removed; `peek` answers the "is this line
            // unstyled?" question without materialising anything.
            // **Skip, then take — and the order is the whole of it.** `SpanMap::style_range`
            // rebuilds its list in ascending offset order, so a line's runs are a contiguous run of it.
            // The first version used `take_while(|(_, _, hi)| *hi > at)` alone, which **stops at the
            // first span that ends at or before this line** — and for the line after a styled one that
            // is the first span in the list, so every line after the first styled run drew plain.
            // **Four of part 24's gates went red at once, all reporting "no differing pixels at all",**
            // which is the most informative failure in this project: the style was still in the map, it
            // was simply never looked at.
            let mut runs = page_runs
                .iter()
                .skip_while(|(_, _, hi)| *hi <= at)
                .take_while(|(_, lo, _)| *lo < at + shown_len)
                .map(|(s, lo, hi)| (*s, *lo, *hi))
                .peekable();
            if runs.peek().is_none() {
                tree.before
                    .push(SurfaceTree::leaf(Node::DocText(DocRun::new(
                        l.x as i32,
                        y as i32,
                        at,
                        shown_len,
                        holonomy_render::Style::REGULAR,
                        0,
                        ink,
                    ))));
                extra = union_opt(extra, Some(rect));
                continue;
            }

            // --- Pass 3b: the styled runs, each positioned by how many cells precede it on this line.
            //
            // **The x advances by the cell count, not the byte count,** because the grid is cells: a bold
            // run that started after two two-byte characters starts at cell 2, and starting it at byte 4
            // would put it two cells right of the character before it. Part 22 made the caret count
            // characters for the same reason and from the opposite direction.
            // **A pen in pixels, and not a cell count.** Part 31. This used to be `cell_x`, advanced by
            // `codepoint_count` and multiplied by `cell_w` at each call site -- the grid, while the
            // painter advanced by the font's metrics, so the two disagreed at every run boundary.
            //
            // **The name is the argument.** `cell_x` reads like a count of cells and is used like one;
            // `pen_x` reads like a distance and is, so the next reader cannot reintroduce the grid by
            // accident. See `pen_of`.
            let mut pen_x: u32 = 0;
            let mut cursor = 0usize; // byte index into `seg`
            for (span, lo, hi) in runs {
                // **Skip anything past the drawn prefix.** `runs_in` is asked for `[at, at + shown_len)`
                // so this cannot happen today; it is a bound rather than a belief, because `seg` was
                // truncated to the measure and a run clipped to a range the caller then shortened would
                // otherwise index out of it.
                let (lo, hi) = (lo.max(at).min(at + shown_len), hi.min(at + shown_len));
                if hi <= lo {
                    continue;
                }
                let (lo, hi) = ((lo - at) as usize, (hi - at) as usize);
                // **Advance over the plain text before this run**, one run's worth of `REGULAR`.
                if lo > cursor {
                    let gap = &seg[cursor..lo];
                    push_doc_run(
                        tree,
                        l.x + pen_x,
                        y,
                        at + cursor as u32,
                        gap.len() as u32,
                        holonomy_render::Style::REGULAR,
                        ink,
                    );
                    pen_x += pen_of(&self.painter, gap, holonomy_render::Style::REGULAR);
                    cursor = lo;
                }
                let run = &seg[cursor..hi];
                if !run.is_empty() {
                    // **`Style(span.atlas_style())`, and not a match on `span.style_flags`.** The span
                    // already knows how to resolve flags to an atlas index -- `atlas_style` is the
                    // function that collapses four flag combinations onto four faces -- and re-deciding
                    // that here would be a second answer to a question with one right one.
                    push_doc_run(
                        tree,
                        l.x + pen_x,
                        y,
                        at + cursor as u32,
                        run.len() as u32,
                        holonomy_render::Style(span.atlas_style()),
                        span_ink(&span, ink),
                    );
                    // **The run's own face, not `REGULAR`.** A bold glyph advances differently from a
                    // plain one, and measuring the gap with the wrong face is the same class of error as
                    // the grid was -- a second place where the position and the painting disagree.
                    pen_x += pen_of(
                        &self.painter,
                        run,
                        holonomy_render::Style(span.atlas_style()),
                    );
                    cursor = hi;
                }
            }
            // **Whatever is left is plain**, which is the tail after the last styled run. Without this a
            // line ending in styled-then-plain text loses its tail, and the symptom is text that vanishes
            // when you make the middle of a line bold.
            if cursor < seg.len() {
                let tail = &seg[cursor..];
                push_doc_run(
                    tree,
                    l.x + pen_x,
                    y,
                    at + cursor as u32,
                    tail.len() as u32,
                    holonomy_render::Style::REGULAR,
                    ink,
                );
            }
            // **A line longer than the measure is truncated, and that is a recorded gap rather than a
            // finished feature.** `LineHeights` is indexed by *document line*, so a line that wrapped onto
            // a second visual row would have no row to be on: `caret_line`, the scroll model, the table
            // anchors and the formula anchors are all document-line indices. Wrapping needs a second index
            // mapping document line to visual rows, and Phase 13's section manifest is where that belongs
            // -- it is the same "structure without content" problem, and building it twice would be worse
            // than building it once.
            extra = union_opt(extra, Some(rect));
        }

        self.stats.lines_drawn = lines;
        self.stats.glyphs_drawn = glyphs;
        widen(damage, extra);
    }

    fn emit_tables(
        &mut self,
        tree: &mut SurfaceTree,
        damage: &mut Option<DamageRect>,
        source: &mut dyn holonomy_text::LeafSource,
    ) {
        let spans = self.editor.tables().spans().to_vec();
        if spans.is_empty() {
            self.stats.table_cells_drawn = 0;
            self.stats.table_borders_drawn = 0;
            return;
        }
        let Ok(read) = read_document(&mut self.editor, &mut self.doc_scratch, source) else {
            // A table whose bytes will not read cannot be drawn, and reporting it here would turn a
            // rendering problem into a paint failure. The session's own table operations already
            // surface the same error through `with_table`.
            return;
        };
        // **The bytes live in `doc_scratch`, and this borrows that field alone.**
        // `read_document` returns a *length* rather than a slice precisely so this borrow and the
        // `editor` borrows in the loops below stay disjoint -- a returned `&[u8]` tied to
        // `&mut Editor` would fuse them, and every `self.editor` use inside would be a conflict.
        let text = &self.doc_scratch[..read];
        let m = self.chrome.metrics;
        let l = self.chrome.layout;
        let first = self.state.scroll_line;
        let last = first + l.rows;
        let mut cells_drawn = 0u32;
        let mut borders_drawn = 0u32;
        let mut extra: Option<DamageRect> = None;
        let active = self.active_cell;

        for span in spans.iter().copied() {
            let line = line_index_in(&mut self.editor, span.start_byte as usize, source);
            if line < first || line >= last {
                continue;
            }
            let Ok(resolved) = ResolvedTable::new(span, text) else {
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
        widen(damage, extra);
    }

    /// Draw every image whose line is on screen, and widen `damage` to cover them.
    ///
    /// # The order of the four things this does
    ///
    /// 1. **Evict**, by the ±1-page window, *before* deciding what is on screen. Evicting first means
    ///    the budget is already correct when a decode is attempted, so an image cannot be admitted by a
    ///    cache that was over its limit.
    /// 2. **Decode** what is visible and not resident. This is the expensive step and the one §2.9.3's
    ///    arithmetic is about: 9 rasters at 640x360 inside 8.0 MiB, or *one* at native 1080p.
    /// 3. **Emit** a `Node::Image` per visible anchor, whether or not its raster is resident. A node
    ///    with no pixels is a counted miss in the painter rather than an absent node, so "the image did
    ///    not draw" is a number in `PaintStats` and not an absence nobody can explain.
    /// 4. **Widen `damage`**, through the same `widen` the tables and formulas use.
    ///
    /// # Why the node carries the address and not the pixels
    ///
    /// `Node` is `Copy` and `SurfaceTree`'s before/self/after ordering depends on that. A node holding
    /// 921,600 bytes cannot be `Copy` and cannot be in a `Vec<Node>`, so it carries the 32-byte
    /// `AssetId` and the painter resolves it through `&IcebergCache`. The cost is one `Option<&Entry>`
    /// and a pointer chase per image per frame, which is nothing next to rasterising one.
    ///
    /// # Why the rect is the *raster's* size and not the source's
    ///
    /// The painter's blit is 1:1 by design -- there is no second scaler in `holonomy-display`, and a
    /// nearest-neighbour stretch is a different picture rather than a worse one. So the rect has to be
    /// the raster the cache holds, which is the page-column-width one. §2.9.3's downscaling is therefore
    /// not an optimisation that paint can skip: it is what makes the blit a copy.
    fn emit_images(
        &mut self,
        tree: &mut SurfaceTree,
        damage: &mut Option<DamageRect>,
        source: &mut dyn holonomy_text::LeafSource,
    ) {
        self.stats.images_drawn = 0;
        if self.editor.assets().is_empty() {
            return;
        }
        // **And the manifest's span count, which counts anchors as well as formulas.** `scan_anchors` is
        // the other byte cursor that forces a whole-document read. Free, exact, wrong only if the manifest
        // is stale -- and it is synced on every edit before anything paints.
        if self.manifest.span_total() == 0 {
            return;
        }
        let Ok(read) = read_document(&mut self.editor, &mut self.doc_scratch, source) else {
            return;
        };
        // **The bytes live in `doc_scratch`, and this borrows that field alone.**
        // `read_document` returns a *length* rather than a slice precisely so this borrow and the
        // `editor` borrows in the loops below stay disjoint -- a returned `&[u8]` tied to
        // `&mut Editor` would fuse them, and every `self.editor` use inside would be a conflict.
        let text = &self.doc_scratch[..read];
        let l = self.chrome.layout;
        let first = self.state.scroll_line;
        let last = first + l.rows;

        // --- 1. eviction. The window is the visible page plus one either side.
        let rows_per_page = l.rows.max(1);
        let first_page = first / rows_per_page;
        let last_page = last.saturating_sub(1) / rows_per_page;
        let evicted = self.images.set_window(
            &(first_page.saturating_sub(1)..=last_page.saturating_add(1)).collect::<Vec<_>>(),
        );
        self.stats.images_evicted += evicted.count;
        // `evicted.rasters` is dropped here, which releases the scrubbed memory. The *count* is kept
        // because the scrub itself is asserted in `holonomy-image`'s gate, where the victims can still
        // be read through a pointer captured before the eviction.

        let mut drawn = 0u32;
        let mut extra: Option<DamageRect> = None;
        let line_heights = self.state.line_heights.clone();
        let anchors = holonomy_text::scan_anchors(text);

        for (ordinal, at) in anchors.into_iter().enumerate() {
            let line = self.line_index(at as usize);
            if line < first || line >= last {
                continue;
            }
            let Ok(asset) = self.editor.assets().get(ordinal) else {
                // An anchor with no asset: the text says there is an image here and the catalog does
                // not agree. Counting it is the only honest response -- drawing a placeholder would be
                // inventing content.
                continue;
            };
            let id = asset.id;
            let (rw, rh) = self.raster_size(asset.width as u32, asset.height as u32);

            // --- 2. decode if absent. Only for what is on screen, which is the whole of the ±1 rule's
            // point: a document may hold a thousand images and only the visible ones cost anything.
            if !self.images.holds(id.as_bytes()) && self.decode_into_cache(ordinal, line) {
                // Decoded. `rw`/`rh` are recomputed by the decoder's own header, and a header that
                // disagreed with the catalog would already have been refused when the catalog was
                // loaded.
            }

            let top = l.text.y + line_heights.y(line - first);
            // The colour is never read: `Painter::image` blits the raster's own RGBA and ignores
            // `Rect::colour`. The page background is passed because `Rect` requires one, and because
            // it is the right answer for the `DamageRect` a caller derives from the rect.
            let rect = Rect::new(
                l.text.x as i32,
                top as i32,
                rw,
                rh,
                holonomy_render::chrome::colour::PAGE,
            );
            tree.before
                .push(SurfaceTree::leaf(Node::Image { rect, asset_id: id }));
            drawn += 1;
            extra = union_opt(extra, Some(rect.bounds().unwrap_or(DamageRect::EMPTY)));
        }
        self.stats.images_drawn = drawn;
        widen(damage, extra);
    }

    /// Decode asset `ordinal` and admit the page-column-width raster, reporting whether it worked.
    ///
    /// Three buffers, in this order, and each is the reason for the next:
    ///
    /// * `image_decode_scratch` at **native** size, because [`holonomy_image::decode`] produces the
    ///   source's own pixels. `read_header` sizes it first, so a header claiming 32 megapixels is
    ///   refused *before* anything is allocated from it.
    /// * `image_resample_scratch` at the **raster** size, because [`holonomy_image::resample`] cannot
    ///   write in place and the two sizes are different by construction at 1920x1080.
    /// * the cache's `SecureBlock`, which is where the result lives and what the budget counts.
    ///
    /// A failure at any step returns `false` and leaves the cache untouched. It is not an error the
    /// paint should propagate: one undecodable asset is a blank rectangle, and failing the whole frame
    /// over it would turn a bad picture into an unusable editor.
    fn decode_into_cache(&mut self, ordinal: usize, page: u32) -> bool {
        let Ok(asset) = self.editor.assets().get(ordinal) else {
            return false;
        };
        let png = asset.png.as_slice();
        let Ok(header) = holonomy_image::read_header(png) else {
            return false;
        };
        // `decoded_len` saturates through `u64`, so a header claiming 32 megapixels produces a length
        // rather than a wrap. The `resize` below then either succeeds or the allocation fails, and a
        // failed allocation is an abort rather than a silent 100 MB buffer -- which is the right
        // response to a hostile header in a process with a 16 MiB RSS budget.
        let native = holonomy_image::decoded_len(&header);
        if self.image_decode_scratch.len() < native {
            self.image_decode_scratch.resize(native, 0);
        }
        if holonomy_image::decode(png, &mut self.image_decode_scratch).is_err() {
            return false;
        }
        let (rw, rh) = self.raster_size(header.width, header.height);
        // `bytes_for` already saturates to `usize::MAX`, so there is no conversion left to make.
        let want = holonomy_image::scale::bytes_for(rw, rh);
        if self.image_resample_scratch.len() < want {
            self.image_resample_scratch.resize(want, 0);
        }
        if holonomy_image::scale::resample_pixels(
            header.width,
            header.height,
            &self.image_decode_scratch,
            rw,
            rh,
            &mut self.image_resample_scratch,
        )
        .is_err()
        {
            return false;
        }
        let decoded = &self.image_resample_scratch[..want];
        if self
            .images
            .insert(
                *asset.id.as_bytes(),
                page,
                u32::try_from(ordinal).unwrap_or(u32::MAX),
                rw,
                rh,
                u64::from(header.width) * u64::from(header.height),
                decoded,
            )
            .is_ok()
        {
            self.stats.images_decoded += 1;
            true
        } else {
            // A raster larger than the whole budget. §2.9.3's arithmetic says 9 fit; a 20th does not,
            // and the refusal is the cache's `Budget` error rather than an overflow.
            false
        }
    }

    /// Bytes of decoded image resident right now. The number §2.9.4's budget row is about.
    pub fn image_cache_bytes(&self) -> usize {
        self.images.resident_bytes()
    }

    /// Scroll the page by `delta` lines, without moving the caret.
    ///
    /// Separate from [`Session::move_line`] because moving the caret is not the same as scrolling the
    /// page, and a gate for the Iceberg window needs to do the second without the first: scrolling
    /// *with* the caret keeps the caret on screen, so it can never actually leave the image.
    ///
    /// Saturating at zero rather than refusing, because scrolling up past the first line is a thing a
    /// reader does, not an error.
    pub fn scroll_by(&mut self, delta: i64) -> u32 {
        let next = (self.state.scroll_line as i64 + delta).max(0) as u32;
        if next == self.state.scroll_line {
            return self.state.scroll_line;
        }
        self.state.scroll_line = next;
        // The whole page: a scroll moves every line, so there is no smaller honest rect.
        self.damage = self.chrome.full_damage();
        self.state.scroll_line
    }

    /// The first line on screen.
    pub fn scroll_line(&self) -> u32 {
        self.state.scroll_line
    }

    /// The document line image `ordinal` is anchored on.
    ///
    /// `ordinal` is the anchor's index among the document's anchors, which is also its index into the
    /// catalog -- the pairing `AssetCatalog`'s ordering contract is about.
    pub fn line_of_anchor(&self, ordinal: usize) -> u32 {
        let Ok(text) = self.editor.text() else {
            return 0;
        };
        holonomy_text::scan_anchors(&text)
            .get(ordinal)
            .map_or(0, |at| self.line_index(*at as usize))
    }

    /// The raster size `(width, height)` image `ordinal` is drawn at, in pixels.
    ///
    /// This is the **page-column-width** size, not the source image's, and the difference is §2.9.3's
    /// whole decision. Exposed because a gate asserting "the resident raster is the column-width one"
    /// needs the number the *layout* used, not the one the catalog recorded -- the two being equal is
    /// the property, and a test that recomputed the expectation from the catalog would agree with a
    /// broken layout.
    pub fn chrome_rect_for_image(&self, ordinal: usize) -> (u32, u32) {
        let Ok(asset) = self.editor.assets().get(ordinal) else {
            return (0, 0);
        };
        self.raster_size(asset.width as u32, asset.height as u32)
    }

    /// The resident raster's pixels for image `ordinal`, if it is resident.
    ///
    /// Read-only, and for the same reason every other accessor here is: the budget and the scrub are
    /// properties a gate must be able to observe, and a cache whose contents can be written from
    /// outside could not be asserted about.
    pub fn image_cache_pixels(&self, ordinal: usize) -> Option<&[u8]> {
        let asset = self.editor.assets().get(ordinal).ok()?;
        self.images
            .get_by_id(asset.id.as_bytes())
            .map(|e| e.pixels())
    }

    /// The resident raster's `(width, height)` for image `ordinal`.
    pub fn image_cache_size(&self, ordinal: usize) -> Option<(u32, u32)> {
        let asset = self.editor.assets().get(ordinal).ok()?;
        self.images
            .get_by_id(asset.id.as_bytes())
            .map(|e| (e.width, e.height))
    }

    /// One pixel of the current frame, as `0x00RRGGBB`.
    ///
    /// `Frame::pixel` returns the frame's own `0xAARRGGBB`; this normalises it so a test compares
    /// against the `chrome::colour` constants rather than re-deriving the alpha byte.
    pub fn frame_pixel(&self, x: u32, y: u32) -> u32 {
        self.frame.pixel(x, y) & 0x00FF_FFFF
    }

    /// The Iceberg cache's budget.
    pub fn image_cache_budget(&self) -> usize {
        self.images.budget()
    }

    /// Rasters resident right now.
    pub fn image_cache_len(&self) -> usize {
        self.images.len()
    }

    /// Draw every formula whose line is on screen, and widen `damage` to cover them.
    ///
    /// # The two modes, and why both are here
    ///
    /// A formula is drawn as one of two things:
    ///
    /// * **the caret is inside it** — the raw LaTeX, in the monospace face, exactly as typed. Editing
    ///   a formula means editing its source, and a compiled layout cannot be edited: there is no
    ///   position in `rac{-b}{2a}` that means "between the minus and the b", because the compiled
    ///   form has no such character.
    /// * **the caret is elsewhere** — the compiled layout: glyphs from the math face for the symbols,
    ///   Inter Italic for the variables, and *procedural fills* for the fraction bars and radical
    ///   overlies.
    ///
    /// The switch is on the caret alone, with no focus ring and no mode key, because the caret is
    /// already the thing that says what the user is doing. A separate "edit formula" mode would need
    /// its own state, its own exit, and its own way to be wrong.
    ///
    /// **A formula that does not parse draws as raw LaTeX even when the caret is elsewhere.** That is
    /// `math.rs`'s rule and it is the right one: a half-typed `ra` is not an error to show the user
    /// as a blank box, it is a formula being written. `math_parse_errors` counts it, because a
    /// deliberate fallback and a formula that silently stopped compiling look identical on screen.
    ///
    /// # Why the bars are not glyphs
    ///
    /// `MathRun::Rule` becomes a [`holonomy_render::Rect`] and `MathRun::RadicalTick` becomes a small
    /// stepped fill, both integer-aligned. A 1 px line drawn from a glyph outline is antialiased at
    /// both ends and so does not meet the glyph beside it exactly; at 1x that seam is visible. This is
    /// PROJECT.md §2.9.2 point 4 applied, and it is why the radical is a shape rather than U+221A.
    fn emit_math(
        &mut self,
        tree: &mut SurfaceTree,
        damage: &mut Option<DamageRect>,
        source: &mut dyn holonomy_text::LeafSource,
    ) {
        self.stats.math_compiled = 0;
        self.stats.math_raw = 0;
        self.stats.math_rules = 0;
        self.stats.math_parse_errors = 0;

        // **The manifest answers "is there anything here to draw?" for free.** Phase 12's remaining work,
        // and the reason this is the whole 6.00 MiB: `read_document` below is the only thing that grows
        // `doc_scratch`, and it was called unconditionally because the span scanners are byte cursors. The
        // manifest counted the same markers once at open. `tests/session_manifest.rs` holds this down.
        if self.manifest.span_total() == 0 {
            return;
        }
        let Ok(read) = read_document(&mut self.editor, &mut self.doc_scratch, source) else {
            return;
        };
        // **The bytes live in `doc_scratch`, and this borrows that field alone.**
        // `read_document` returns a *length* rather than a slice precisely so this borrow and the
        // `editor` borrows in the loops below stay disjoint -- a returned `&[u8]` tied to
        // `&mut Editor` would fuse them, and every `self.editor` use inside would be a conflict.
        let text = &self.doc_scratch[..read];
        let m = self.chrome.metrics;
        let l = self.chrome.layout;
        let mut mm = MathMetrics::new(m.cell_w, m.cell_h);
        // Real per-glyph advances, so the layout's boxes match the ink it is about to draw.
        //
        // Without this the layout sits on the page's 8 px text grid while the fonts are proportional:
        // measured at 16 ppem, JetBrains Mono advances 10 px and `\sum` 14, so every glyph overlapped
        // its neighbour by 1-2 px and a superscript landed *inside* the summation sign. See
        // `MathMetrics::advance` for the table and `crates/holonomy-assets/examples/adv.rs`, which
        // prints it.
        mm.advance = advance_shim(self.painter.atlas(), self.painter.size_index());
        let first = self.state.scroll_line;
        let last = first + l.rows;
        let caret = self.editor.caret();
        let ink = holonomy_render::chrome::colour::INK;
        let line_heights = self.state.line_heights.clone();
        // The closure below cannot touch `self`: it holds `&mut self.math_scratch`, and borrowing
        // `self.line_index` as well is E0500. So every field it needs is copied out first and the
        // counters come back out after. Copying a `ChromeMetrics` is four `u32`s and a `LineHeights`
        // is a `Vec` that is *cloned* -- which is why this is worth a comment rather than a shrug: it
        // is one allocation per paint, on the same path `publish_line_heights` already allocates on.
        // The alternative -- an explicit `for` loop with the borrows scoped per iteration -- is what
        // this should be, and `for_each_math_span`'s closure is the wrong tool for a body that mutates
        // the session. Recorded rather than left for someone to rediscover at three more call sites.
        let mut compiled = 0u32;
        let mut raw_count = 0u32;
        let mut rules = 0u32;
        let mut parse_errors = 0u32;
        let mut extra: Option<DamageRect> = None;
        let scratch = &mut self.math_scratch;
        let source = &mut self.math_source;
        let line_of = |at: usize| {
            text[..at.min(text.len())]
                .iter()
                .filter(|&&b| b == b'\n')
                .count() as u32
        };

        holonomy_text::for_each_math_span(text, |span| {
            let line = line_of(span.start as usize);
            if line < first || line >= last {
                return;
            }
            let inner = span.inner();
            let top = l.text.y + line_heights.y(line - first);

            // --- caret inside: the raw LaTeX, in the monospace face.
            if span.contains(caret) {
                let raw = &text[inner.clone()];
                let shown: String = raw
                    .iter()
                    .take(MAX_MATH_SOURCE)
                    .map(|&b| b as char)
                    .collect();
                if !shown.is_empty() {
                    // The source is drawn at the text column's left edge, which is where the formula
                    // *starts* -- not where the caret is. A formula that grew to 200 bytes would
                    // otherwise reflow the rest of the line every keystroke.
                    tree.before.push(SurfaceTree::leaf(Node::Text(TextRun::new(
                        l.text.x as i32,
                        top as i32,
                        shown.chars().next().map_or(0, |c| c as u32),
                        shown.chars().count() as u16,
                        holonomy_render::Style::MONOSPACE,
                        0,
                        ink,
                    ))));
                }
                raw_count += 1;
                let w = (shown.chars().count() as u32) * m.cell_w;
                let h = mm.cell_h;
                extra = union_opt(extra, Some(DamageRect::new(l.text.x, top, w, h)));
                // The caret inside a formula is drawn by the chrome's own line model at the column,
                // which for a formula that starts at the text column is the same place. Nothing extra
                // to do: the chrome already draws it, and it lands on the source.
                return;
            }

            // --- caret elsewhere: compile.
            source.clear();
            source.extend_from_slice(&text[inner]);
            let Ok(node) = math::parse(source) else {
                parse_errors += 1;
                // Fall back to the raw source at the text column. Same geometry as the raw mode above,
                // deliberately: a formula that stops compiling must not move.
                let shown: String = source
                    .iter()
                    .take(MAX_MATH_SOURCE)
                    .map(|&b| b as char)
                    .collect();
                if !shown.is_empty() {
                    tree.before.push(SurfaceTree::leaf(Node::Text(TextRun::new(
                        l.text.x as i32,
                        top as i32,
                        shown.chars().next().map_or(0, |c| c as u32),
                        shown.chars().count() as u16,
                        holonomy_render::Style::MONOSPACE,
                        0,
                        ink,
                    ))));
                }
                raw_count += 1;
                let w = (shown.chars().count() as u32) * m.cell_w;
                extra = union_opt(extra, Some(DamageRect::new(l.text.x, top, w, mm.cell_h)));
                return;
            };

            // The box is `above` the text column's top by half its extra height, so a fraction is
            // vertically centred on the line it sits in rather than hanging from its top edge. This is
            // a *centre*, not a `LineHeights` offset: `publish_line_heights` handles the displacement
            // of the lines below, and doing both here would double it.
            let box_ = measure_only(&node, &mm);
            let y = top + (m.cell_h.saturating_sub(box_.height)) / 2;
            scratch.clear();
            layout_boxed(&node, &mm, l.text.x, y, scratch);

            for run in &scratch.runs {
                match *run {
                    MathRun::Glyph { x, y: gy, cp, .. } => {
                        // The style is per glyph, not per formula: `\alpha` is in the math face, `x` is
                        // Inter Italic. `is_math_symbol` is the whole decision.
                        let style = if holonomy_assets::payload::is_math_symbol(cp) {
                            holonomy_render::Style::MATH
                        } else {
                            holonomy_render::Style::ITALIC
                        };
                        tree.before.push(SurfaceTree::leaf(Node::Text(TextRun::new(
                            x as i32, gy as i32, cp, 1, style, 0, ink,
                        ))));
                    }
                    MathRun::Rule { x: rx, y: ry, w, h } => {
                        tree.before.push(SurfaceTree::leaf(Node::Rect(
                            holonomy_render::Rect::new(rx as i32, ry as i32, w, h, ink),
                        )));
                        rules += 1;
                    }
                    MathRun::RadicalTick {
                        x: rx,
                        y: ry,
                        w,
                        tick_h,
                    } => {
                        // A vertical stub plus a diagonal, drawn as integer steps. See the module
                        // header on `MathRun::RadicalTick`: a diagonal as 1 px squares aliases, and
                        // this is the only place in the renderer that draws one.
                        let stub_w = (m.cell_w / 3).max(1);
                        tree.before.push(SurfaceTree::leaf(Node::Rect(
                            holonomy_render::Rect::new(rx as i32, ry as i32, stub_w, tick_h, ink),
                        )));
                        // The diagonal: `tick_h` steps of one pixel each, marching right.
                        let steps = tick_h.min(w);
                        for step in 0..steps {
                            tree.before.push(SurfaceTree::leaf(Node::Rect(
                                holonomy_render::Rect::new(
                                    (rx + stub_w + step) as i32,
                                    (ry + tick_h - 1 - step) as i32,
                                    1,
                                    2,
                                    ink,
                                ),
                            )));
                        }
                        rules += 1;
                    }
                }
            }
            compiled += 1;
            extra = union_opt(
                extra,
                Some(DamageRect::new(l.text.x, y, box_.width, box_.height)),
            );
        });

        self.stats.math_compiled = compiled;
        self.stats.math_raw = raw_count;
        self.stats.math_rules = rules;
        self.stats.math_parse_errors = parse_errors;
        widen(damage, extra);
    }

    /// Add every formula's height to the line-height model, so the lines below it move down.
    ///
    /// See the free function [`math_blocks`] for the model and why it is a free function. This method
    /// exists only to build the `MathMetrics` -- which **must match `emit_math` exactly**, because if
    /// the two disagree the displacement the line model applies is for one formula and the pixels are
    /// another, and a wrong displacement is invisible in the counters: the formula still draws.
    fn math_blocks_for(&mut self, source: &mut dyn holonomy_text::LeafSource) -> Vec<(u32, u32)> {
        // **The 6 MiB read, removed.** Phase 12 named this as the blocker it could not fix: it called
        // `read_document` unconditionally because `for_each_math_span` is a cursor over bytes and *"does
        // this document contain any math"* could not be asked without reading them to find the `$$`.
        // [`crate::manifest::Manifest`] answers it from structure — a `u32` per section, 12 bytes each,
        // built at open — and the answer is **zero for a document with no formula in it**.
        //
        // So the check is free, exact, and wrong only if the manifest is stale, and the manifest is synced
        // on every edit by [`Session::sync_manifest`] before anything paints. A stale manifest would make
        // this skip a formula that exists; `tests/session_manifest.rs`'s
        // `a_formula_appears_and_the_manifest_notices_within_one_keystroke` is what holds that down.
        if self.manifest.span_total() == 0 {
            return Vec::new();
        }
        let mut mm = MathMetrics::new(self.chrome.metrics.cell_w, self.chrome.metrics.cell_h);
        mm.advance = advance_shim(self.painter.atlas(), self.painter.size_index());
        let Ok(read) = read_document(&mut self.editor, &mut self.doc_scratch, source) else {
            return Vec::new();
        };
        // **The bytes live in `doc_scratch`, and this borrows that field alone.**
        // `read_document` returns a *length* rather than a slice precisely so this borrow and the
        // `editor` borrows in the loops below stay disjoint -- a returned `&[u8]` tied to
        // `&mut Editor` would fuse them, and every `self.editor` use inside would be a conflict.
        let text = &self.doc_scratch[..read];
        math_blocks(&mut self.editor, text, &mm, source)
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
/// A per-codepoint advance function over the atlas, or `None` for the fixed-grid model.
///
/// The signature is `fn(u32) -> u32` rather than a closure because `MathMetrics` is `Copy` and
/// `const`-constructible, and a closure capturing the atlas would make it neither. The cost of that
/// choice is the table below: the atlas and the size index have to be reachable from a `fn`, which
/// means they are stashed in statics.
///
/// # Why statics rather than a leak
///
/// The session holds `&'a Atlas` through its `Painter`, so it does *not* outlive the atlas, and a
/// leaked `&'static` would be a second reference that could outlive it -- and then point at a
/// dropped atlas. So the pointers are written once, when the atlas is built, and the advance function
/// asserts that what it finds is what was published. A formula laid out after the session's painter
/// was dropped would panic rather than read freed memory, which is the failure order that is safe.
///
/// This is a limitation of the `fn`-pointer design and it is the cost of the alternative: a
/// `dyn Fn` field on `MathMetrics` would make the layout allocate-capable and stop being `Copy`, which
/// costs every existing caller and buys nothing here.
fn advance_shim(
    atlas: Option<&holonomy_assets::atlas::Atlas>,
    size_index: u8,
) -> Option<fn(u32) -> u32> {
    let atlas = atlas?;
    if std::ptr::from_ref(atlas) as usize
        != PUBLISHED_ATLAS.load(std::sync::atomic::Ordering::Acquire)
    {
        return None;
    }
    // Compared in *pixels*, because that is what `publish_atlas` stored. See its comment: the two
    // sizes are different quantities and mixing them silently disables the whole feature.
    let px = atlas.sizes().get(size_index as usize).copied().unwrap_or(0);
    if px as usize != PUBLISHED_SIZE.load(std::sync::atomic::Ordering::Acquire) {
        return None;
    }
    Some(advance_from_published)
}

/// The published atlas pointer. Written by [`publish_atlas`].
static PUBLISHED_ATLAS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The published size, in **pixels**. Written by [`publish_atlas`].
static PUBLISHED_SIZE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Publish the atlas and the pixel size the advance shim will read.
///
/// Called from [`Session::new`]. A single global because the shim is a plain `fn`; a process that
/// built two atlases of different sizes would have the second session fall back to the fixed grid,
/// which [`advance_shim`] detects rather than reading the wrong metrics.
///
/// **The size is translated from an index to pixels here, and that is not cosmetic.**
/// `Painter::size_index` is an index into `Atlas::sizes`, while `Atlas::metric` takes a *pixel* size
/// and looks the index up itself. Passing the index straight through made every lookup ask for 0 ppem,
/// find no such size, and return `GlyphMetric::BLANK` -- whose advance is 0 -- so `adv` fell back to
/// `cell_w` and the whole feature was inert while still looking correct in the counters. The advance
/// is the one number whose absence changes no counter, which is why this needed a pixel-level
/// regression test rather than a smoke test: see
/// `a_formula_laid_out_on_real_advances_is_wider_than_the_fixed_grid_model`.
fn publish_atlas(atlas: Option<&holonomy_assets::atlas::Atlas>, size_index: u8) {
    PUBLISHED_ATLAS.store(
        atlas.map_or(0, |a| std::ptr::from_ref(a) as usize),
        std::sync::atomic::Ordering::Release,
    );
    let px = atlas
        .and_then(|a| a.sizes().get(size_index as usize).copied())
        .unwrap_or(0);
    PUBLISHED_SIZE.store(px as usize, std::sync::atomic::Ordering::Release);
}

/// The advance function itself: read `GlyphMetric::advance_x` for `cp`, choosing the face per glyph.
///
/// The face choice is the same one `emit_math` makes for *drawing*, and it has to be the same one:
/// measuring `lpha` in Inter, which has no Greek, would give a zero advance and reserve nothing.
fn advance_from_published(cp: u32) -> u32 {
    let ptr = PUBLISHED_ATLAS.load(std::sync::atomic::Ordering::Acquire)
        as *const holonomy_assets::atlas::Atlas;
    if ptr.is_null() {
        return 0;
    }
    // SAFETY: `ptr` was published from a `&'a Atlas` that some live `Session` owns. A `Session` that
    // owned it has not been dropped while another `Session` is being painted in this process, because
    // painting happens through a `&Session` borrow. The remaining hole is two sessions built over two
    // different atlases, which `advance_shim` rejects by comparing the pointer before publishing a
    // closure -- so only the *published* atlas is ever read here, and only while its owner lives.
    let atlas = unsafe { &*ptr };
    let size = PUBLISHED_SIZE.load(std::sync::atomic::Ordering::Acquire) as u16;
    let style = if holonomy_assets::payload::is_math_symbol(cp) {
        holonomy_assets::payload::Style::Math
    } else {
        holonomy_assets::payload::Style::Italic
    };
    atlas.metric(cp, style, size).advance_x.into()
}

/// The image `Ctrl+I` inserts: a 1920x1080 colour chart with a 64 px grid.
///
/// Deliberately **larger than the page column**, which is §2.9.3's claim made testable: because the
/// Iceberg cache holds page-column-width rasters, every image in this product is a downscale, and a
/// fixture at or below the column width would never run the scaler. Committed rather than generated
/// because `include_bytes!` needs bytes at compile time, and a build script that emitted one would put a
/// PNG *encoder* into the build -- more code than the fixture.
pub const TEST_CHART_PNG: &[u8] = include_bytes!("../assets/test-chart.png");

/// How many run slots [`Session::math_scratch`] is sized for.
///
/// The quadratic formula -- the gate's worked example -- lays out to 14 runs. 64 is the next power of
/// two above that and covers `rac{a}{b} + \sqrt{c}` with room for a superscript and a subscript, so
/// the common case never reallocates. A pathological formula with more than 64 runs grows the `Vec`
/// once and then stops, which is the same behaviour every other growable buffer here has.
const MATH_RUN_CAPACITY: usize = 64;

/// The most source bytes of a formula drawn as raw LaTeX.
///
/// A `TextRun` is capped at [`TextRun::MAX_LEN`] codepoints, and a `u32` advance times an unbounded
/// byte count would put the run's right edge past the page. Truncating is wrong in principle and right
/// in practice: the user is looking at the formula they are editing, which is at the front.
const MAX_MATH_SOURCE: usize = TextRun::MAX_LEN as usize;

/// The chunk size for the document scans that replaced whole-document copies. Phase 11.
///
/// `Session::line_start`, `Session::line_index` and `Session::refresh_counts` used to call
/// `Editor::text()`, which allocates a `Vec` the size of the document and copies it byte-for-byte --
/// six times per keystroke. They now stream through `Editor::read_into` in chunks of this size, so
/// they hold 4 KiB instead of the document and allocate nothing.
///
/// **4 KiB, not larger, and the reason is stack rather than throughput.** These are `&self` methods on
/// the keystroke path and the buffer is a local `[u8; SCAN_CHUNK]`, so it must live on the stack: the
/// alternative is a `Session` field, which then needs `&mut self` at every call site -- and
/// `line_index` is called from inside `emit_tables` and `emit_math` while they hold borrows of other
/// `Session` fields, so making it `&mut self` is a borrow-checker fight over five call sites rather
/// than a one-line change. `SCAN_CHUNK = LEAF_CAPACITY` so one chunk is exactly one rope leaf and
/// `read_into` walks the spine rather than re-splitting ranges.
///
/// The cost of the choice: a document scan is `document_bytes / 4096` calls into `read_into`. At 3.5
/// MiB that is ~900 calls for the line count, which is microseconds. Phase 11's third item -- the
/// Fenwick tree -- removes the scan itself rather than making it cheaper.
pub const SCAN_CHUNK: usize = 4096;

/// How many bytes one page's worth of document text can be. Phase 12.
///
/// **48 KiB, and `Layout::rows` is 23 rather than the 43 first written here.** At 1280x800 the text
/// column is 23 rows of `cell_h`, so the true ceiling is 23 x 1,024 = 23,552 bytes and 48 KiB is twice
/// that. The overshoot is deliberate: a `rows` figure that depends on the panel size would make this
/// constant wrong for every panel but this one, and a buffer that is sized from a constant is one that
/// cannot be resized by accident on the paint path.
///
/// **This is the number that replaces 6 MiB.** `doc_scratch` grows to the whole document because
/// `emit_tables`, `emit_math` and `emit_images` all want random access across the text. Body text does
/// not: it wants the 43 lines on screen. So it gets its own buffer, sized to the page.
///
/// A field rather than a local because a local array passed to `read_into` **escapes** and is promoted to
/// the heap -- the reason `Session::table_scratch` exists -- which on a path that paints per keystroke
/// would be one allocation per paint.
pub const LINE_SCRATCH_BYTES: usize = 48 * 1024;

/// `node`'s box, without emitting anything.
///
/// `math_layout::measure`, re-exported under a name that says what it is for at the call site. There is
/// a `measure` and a `layout_boxed` in `holonomy_render`, and this function needs the box *twice* --
/// once to centre the formula vertically before laying it out, and once for the damage rectangle --
/// so it is the measure-only entry point rather than the one that returns a box as a by-product.
#[inline]
/// The 0-based line index containing `at`. Phase 11.
///
/// **Still O(bytes before the caret), and Phase 11 does not pretend otherwise.** The allocation is
/// gone; the scan is not. The honest fix is the Fenwick tree over line heights, which answers this in
/// `O(log n)`, and wiring it is Phase 11's third item. What is fixed here is that the count is the
/// remaining cost rather than a 6.4 MiB copy that accompanied it.
fn line_index_in(
    editor: &mut Editor,
    at: usize,
    source: &mut dyn holonomy_text::LeafSource,
) -> u32 {
    let end = at.min(editor.text_len());
    let mut count = 0u32;
    let mut offset = 0usize;
    let mut chunk = [0u8; SCAN_CHUNK];
    while offset < end {
        let want = (end - offset).min(SCAN_CHUNK);
        // **Faulting, for the same reason `read_document` is.** A line index that stopped counting at the
        // resident window would number every table, formula and image after it as being on line 0 --
        // which is worse than not drawing it, because it draws it in the wrong place.
        let got = match editor.read_into_faulting(source, offset, &mut chunk[..want]) {
            Ok(got) => got,
            Err(_) => break,
        };
        // `read_into` returns 0 only past the end, and `offset < end <= text_len`, so a zero here
        // would be an infinite loop rather than a short read.
        if got == 0 {
            break;
        }
        count += chunk[..got].iter().filter(|&&b| b == b'\n').count() as u32;
        offset += got;
    }
    count
}

/// Every formula's `(anchor_line, height_px)`, for the line-height model.
///
/// A free function over its arguments rather than a `&self` method, for the reason
/// [`read_document`] is: its caller holds `&mut Session::doc_scratch` across the call, so a `&self`
/// method would borrow all of `Session` and conflict with it.
///
/// Same model as tables and the same arithmetic: the extra **is** the block's full height, because
/// [`LineHeights::from`] is handed pixels and displaces every line from the anchor onward. A formula
/// one line tall therefore contributes its `cell_h` and pushes the line below it down by a whole line
/// -- which is wrong for an inline formula and right for a displayed one, and the distinction is
/// Phase 9B's known limit rather than a bug to be argued about here. Recorded in `PROJECT.md` §9B
/// rather than silently approximated.
fn math_blocks(
    editor: &mut Editor,
    text: &[u8],
    mm: &MathMetrics,
    source: &mut dyn holonomy_text::LeafSource,
) -> Vec<(u32, u32)> {
    let mut blocks = Vec::new();
    holonomy_text::for_each_math_span(text, |span| {
        let inner = span.inner();
        // An unparseable formula has no box, so it contributes nothing and the raw text's own line
        // height stands. That is the same answer the caret-inside mode gives.
        let Ok(node) = math::parse(&text[inner]) else {
            return;
        };
        let line = line_index_in(editor, span.start as usize, source);
        blocks.push((line, measure_only(&node, mm).height));
    });
    blocks
}

/// Which visible line a page-buffer offset falls in, given the per-line `(offset, len)` spans.
///
/// **A binary search over `spans`, which is sorted by offset because it is built in line order.** An
/// offset past the last line returns the last line, which is the answer a caller that clamps wants: a
/// formula's closing `$$` on the line after the page has scrolled is attributed to the last visible line,
/// which is the conservative direction for a "this line is claimed" test.
///
/// The fallback is a linear scan rather than a `partition_point`, because `spans` is a dozen entries and
/// a branch-free binary search over something that small costs more to read than a loop.
fn line_of_span(spans: &[(u32, u32)], offset: usize) -> u32 {
    let mut row = 0u32;
    for &(at, len) in spans {
        let start = at as usize;
        if offset < start {
            return row.saturating_sub(1);
        }
        if offset < start + len as usize {
            return row;
        }
        row += 1;
    }
    spans.len().saturating_sub(1) as u32
}

/// Strip a line terminator from a byte slice: `\r\n`, `\n`, or `\r`.
///
/// **Both bytes, and CRLF is one terminator rather than two.** A document written on Windows ends its
/// lines with `\r\n`, and a renderer that draws the `\r` puts a visible mark at the end of every line —
/// because `\r` has a glyph in most faces, and it is not a blank one. The order matters: stripping `\n`
/// first and then `\r` handles all three forms with one pass and no lookahead.
fn strip_terminator(seg: &[u8]) -> &[u8] {
    let seg = seg.strip_suffix(b"\n").unwrap_or(seg);
    seg.strip_suffix(b"\r").unwrap_or(seg)
}

/// A [`TextSource`] over a contiguous run of document bytes. Phase 12.
///
/// # Why the session's source is a *page* and not the document
///
/// `Editor` cannot lend its text as a slice: its leaves are page-locked and a document is thousands of
/// them, so there is no contiguous `&[u8]` for the whole rope. **A rope has no slice.** So
/// [`Session::emit_body_text`] copies the visible page into `line_scratch` and hands *that* to the
/// painter, and every `DocRun`'s offset is into the page buffer rather than into the document.
///
/// That is the same "structure without content" split Phase 13's section manifest is about: what the
/// renderer needs is a *window*, and the window is contiguous. Phase 13's windowing makes the window
/// larger without making the copy larger, because a section's text is read once and read wide.
///
/// # Why a wrapper and not a bare `&[u8]`
///
/// `TextSource` is a trait so `holonomy-display` does not depend on `holonomy-text`, which is the same
/// division of labour `RasterSource` exists for. A three-line impl over a slice is the price of that, and
/// it is the same price `RasterSource` costs.
struct PageText<'a>(&'a [u8]);

impl TextSource for PageText<'_> {
    fn document(&self, offset: u32, len: u32) -> Option<&[u8]> {
        let start = usize::try_from(offset).ok()?;
        let len = usize::try_from(len).ok()?;
        let end = start.checked_add(len)?;
        self.0.get(start..end)
    }
}

/// How many codepoints `bytes` holds, which is how many cells wide it is on the page.
///
/// **The same rule as [`codepoints_upto`](Self) applied to a whole slice**, because a byte that is not
/// a continuation byte *is* a codepoint start. Part 24 needs it to advance the pen between the styled
/// runs of one line: **the grid is cells, so the next run's x is the number of codepoints before it and
/// not the number of bytes.** Using bytes there would put every run after the first multi-byte character
/// one cell too far right, which is the same arithmetic the caret got wrong in part 22 and the opposite
/// direction.
///
/// **`O(len)`, no decode and no allocation**, so calling it per run is a scan of the line rather than of
/// the document: a line is at most `cells_per_row` codepoints wide, and the runs tile it.
/// **The length in bytes of the UTF-8 scalar at the front of `bytes`, at least 1.** Part 33.
///
/// **A continuation byte at the front cannot start a scalar**, and a truncated tail cannot be decoded,
/// so the answer is clamped to one byte rather than refused: the walk in
/// [`Session::column_at_pen`] is over a chunk boundary it cannot see, and stopping there would put
/// every later glyph of a 64-byte-boundary line one column short.
///
/// **This is not the same as `std::str::from_utf8`, deliberately.** That answers "is this whole slice
/// valid UTF-8", which is the wrong question mid-chunk and costs a scan of the rest of the slice.
fn scalar_len(bytes: &[u8]) -> usize {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.chars().next().map_or(1, char::len_utf8),
        Err(e) => {
            let valid = e.valid_up_to();
            if valid > 0 {
                std::str::from_utf8(&bytes[..valid])
                    .ok()
                    .and_then(|s| s.chars().next())
                    .map_or(1, char::len_utf8)
            } else {
                // **An invalid byte is one scalar's worth of movement**, matching `pen_of`, which
                // charges one `.notdef` cell for it. Two functions disagreeing here would put the caret
                // a column from the glyph it points at, on a document that is not valid UTF-8.
                e.error_len().unwrap_or(1)
            }
        }
    }
}

fn codepoint_count(bytes: &[u8]) -> u32 {
    bytes.iter().filter(|&&b| b & 0xC0 != 0x80).count() as u32
}

/// Push one body-text run at `x`, `y`, in `style`.
///
/// **A free function rather than a method** because it holds no session state and takes seven arguments
/// that are all values — the same reason `strip_terminator` and `codepoints_upto` are free. **Part 24
/// added it because the styled path emits four runs per line** (the plain gaps and the tail) and writing
/// the `DocRun::new` call out four times is how four slightly different calls drift apart.
fn push_doc_run(
    tree: &mut SurfaceTree,
    x: u32,
    y: u32,
    at: u32,
    len: u32,
    style: holonomy_render::Style,
    ink: u32,
) {
    if len == 0 {
        return;
    }
    tree.before
        .push(SurfaceTree::leaf(Node::DocText(DocRun::new(
            x as i32, y as i32, at, len, style, 0, ink,
        ))));
}

/// **The ink a span draws in: its own colour, or the document's.** Part 29.
///
/// ## Why `color_rgb` needed a helper rather than an expression at the call site
///
/// **Because `0` means two things and they must not collapse.** `TextIntervalSpan::color_rgb`'s own
/// doc says *"Zero means the document's default colour, not opaque black"* — so `0` is not black, it
/// is *unspecified*. Passing `span.color_rgb` straight through would paint every uncoloured span in
/// `0x00000000`, and the alpha byte is what the blitter reads, so the page's text would disappear.
///
/// **The alpha byte is added here and nowhere else.** `DocRun::colour` is packed `0xAARRGGBB` and a
/// span's is `0x00RRGGBB`, so `| 0xFF00_0000` is the one conversion and it belongs in one function.
///
/// ## Why this was not written in phase 4 or 6
///
/// **Because nothing called it.** `color_rgb` is a documented field, an argument to `style_range`, a
/// term in `same_style` -- so it split runs and survived undo -- and **the renderer never read it.**
/// Part 24 is the same story for `style_flags`, one field away in the same record, and this is the
/// half that was still outstanding when part 28's selection made a range available to colour.
///
/// `crates/holonomy/tests/text_colour.rs` is the gate, and its first run reported **zero** differing
/// pixels.
fn span_ink(span: &holonomy_text::TextIntervalSpan, ink: u32) -> u32 {
    if span.color_rgb == 0 {
        ink
    } else {
        0xFF00_0000 | (span.color_rgb & 0x00FF_FFFF)
    }
}

/// **How far the pen has moved after laying out `bytes`, in the document's own faces.** Part 31.
///
/// ## Why this exists
///
/// **`emit_body_text` positioned each run at `text.x + cell_x * cell_w`** — the 8 px grid — **while
/// [`Painter::doc_text`] advanced by the font's own metrics.** So a line drawn as one run accumulated
/// the font's advances and a line drawn as three snapped back to the grid at every boundary: **a run
/// boundary was a discontinuity**, and bolding or colouring a word moved the text after it (part 29).
///
/// ## Why it goes through [`Painter::advance_of`] and not through the atlas
///
/// **Because that is where the rule lives, and the rule has two cases.** A space takes the face's
/// advance; a `.notdef` takes the cell and is counted missing. Re-implementing that here is how the two
/// sides drifted in the first place, and the failure is invisible — a document whose spaces are each a
/// pixel short still looks like a document.
///
/// ## Why the chrome's grid is still used elsewhere, and that is part 30
///
/// **[`Caret::locate`] and the selection highlight are still on the grid**, and they are still wrong.
/// Making them right is `part 30`'s decision and not this one, because it moves every x in the project
/// and §2.9.3's 640 px page column with it. **This change makes the *document* internally consistent**
/// — every run placed by the same rule the painter advances by — and leaves the caret question where
/// `tests/grid_vs_glyphs.rs` holds it.
///
/// ## A free function, because it needs one field
///
/// **`&Painter` and not `&self`**, so the borrow is one argument rather than the whole session — which
/// matters because the only two callers are inside `emit_body_text`, which holds `&mut self` for other
/// reasons. **`Painter` is all it reads**, and saying so in the signature means a second caller cannot
/// reach for `self` and quietly depend on something else.
///
/// ## `O(bytes)`, and no allocation
///
/// **A scan over bytes the caller already has.** `emit_body_text` passes the page's `line_scratch`, so
/// this reads a buffer that was read anyway; `refresh_caret_column` would pass one line. **Decoding is
/// `from_utf8().valid_up_to()`, the same walk `doc_text` does**, and an invalid byte counts as one
/// `.notdef` — the same direction, for the same reason.
fn pen_of(painter: &Painter<'_>, bytes: &[u8], style: holonomy_render::Style) -> u32 {
    let mut pen = 0u32;
    let mut at = 0usize;
    while at < bytes.len() {
        let (cp, used) = match std::str::from_utf8(&bytes[at..]) {
            Ok(s) => match s.chars().next() {
                Some(c) => (u32::from(c), c.len_utf8()),
                None => break,
            },
            Err(e) => {
                let valid = e.valid_up_to();
                if valid > 0 {
                    let s = std::str::from_utf8(&bytes[at..at + valid])
                        .expect("valid_up_to is a char boundary by definition");
                    match s.chars().next() {
                        Some(c) => (u32::from(c), c.len_utf8()),
                        None => break,
                    }
                } else {
                    (0xFFFD, e.error_len().unwrap_or(1))
                }
            }
        };
        pen = pen.saturating_add(painter.advance_of(cp, style));
        at += used;
    }
    pen
}

/// **The pen position `at` bytes into a line, using each styled run's own face.** Part 33.
///
/// ## Why this exists, and why it is not `pen_of`
///
/// **`pen_of` measures a whole run in one face.** A line with three runs needs three calls with three
/// faces, and getting the boundaries right by hand at each call site is how the run positions and the
/// caret came to disagree in the first place — so the boundary walk lives here once, and the caret,
/// the selection highlight and the text itself all get their positions from the same arithmetic.
///
/// ## `at` is bytes into `text`; `line_start` is the line's offset in the document
///
/// **Both, and the distinction is load-bearing.** `runs` is keyed on document offsets because that is
/// what `SpanMap` speaks, and `text` is a slice of the page buffer because that is what the emitter
/// already has. Conflating them would put a highlight on the wrong line, and the two differ by
/// everything above the page.
///
/// ## No allocation
///
/// **`runs` is the page's one query, already in hand** — this walks a handful of entries over at most
/// one line's bytes. The paint path's allocation invariant is asserted by `tests/session_no_alloc.rs`
/// for the edit path and is explicitly out of scope for paint, but a function on this path has no
/// business allocating when a walk answers it.
fn pen_at(
    painter: &Painter<'_>,
    text: &[u8],
    line_start: u32,
    at: usize,
    runs: &[(holonomy_text::TextIntervalSpan, u32, u32)],
) -> u32 {
    let at = at.min(text.len());
    let mut pen = 0u32;
    // **The cursor is a byte index into `text`, and every run boundary it lands on is converted from a
    // document offset before use.** The `- line_start` is `saturating_sub` because a run can start
    // before this line -- a styled run begun on the previous line and continuing here -- and a
    // subtraction that wrapped would put the highlight a page to the left.
    let mut cursor = 0usize;
    for (span, lo, hi) in runs {
        let lo = (*lo).saturating_sub(line_start) as usize;
        let hi = (*hi).saturating_sub(line_start) as usize;
        if lo >= at {
            break;
        }
        if hi <= cursor {
            continue;
        }
        if lo > cursor {
            pen = pen.saturating_add(pen_of(
                painter,
                &text[cursor..lo],
                holonomy_render::Style::REGULAR,
            ));
        }
        let end = hi.min(at);
        pen = pen.saturating_add(pen_of(
            painter,
            &text[lo..end],
            holonomy_render::Style(span.atlas_style()),
        ));
        cursor = end;
        if cursor >= at {
            return pen;
        }
    }
    if cursor < at {
        pen = pen.saturating_add(pen_of(
            painter,
            &text[cursor..at],
            holonomy_render::Style::REGULAR,
        ));
    }
    pen
}

/// Push a filled rectangle into `tree`'s **back** layer. Part 28.
///
/// **A free function beside [`push_doc_run`](Self) rather than a method, and for the reason that one
/// is a free function.** `SurfaceTree::leaf(Node::Rect(Rect::new(...)))` nests three ways and the `u32`
/// to `i32` conversion is inside the innermost one; a helper is where the conversion gets asserted
/// once instead of at every call site.
///
/// **Into `before`, so it paints under the text.** A selection highlight is the only surface in this
/// file that must be behind the glyphs rather than in front of them, and it is exactly the kind of
/// thing that gets pushed the wrong way once and is invisible until someone selects a word.
fn push_rect(tree: &mut SurfaceTree, x: u32, y: u32, width: u32, height: u32, colour: u32) {
    if width == 0 || height == 0 {
        return;
    }
    tree.before.push(SurfaceTree::leaf(Node::Rect(Rect::new(
        x as i32, y as i32, width, height, colour,
    ))));
}

/// Bytes of the segment starting at `from` that hold at most `limit` codepoints.
///
/// **Never splits a multi-byte character**, which is the entire job. Counting non-continuation bytes
/// finds the `limit`-th codepoint start; walking its continuation bytes finds where it ends. No decode,
/// no allocation, `O(limit)`.
///
/// A byte that is not a continuation byte is a codepoint start by definition, so the count is right even
/// for a malformed document — where the "codepoint" may not be a real one. That is the safe direction:
/// the painter draws the bytes and counts what it cannot decode, and this only decides where to cut.
fn codepoints_upto(bytes: &[u8], from: usize, limit: usize) -> usize {
    if limit == 0 || from >= bytes.len() {
        return 0;
    }
    let mut count = 0usize;
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] & 0xC0 != 0x80 {
            if count == limit {
                break;
            }
            count += 1;
        }
        i += 1;
    }
    if i >= bytes.len() {
        return bytes.len() - from;
    }
    let mut end = i + 1;
    while end < bytes.len() && bytes[end] & 0xC0 == 0x80 {
        end += 1;
    }
    end - from
}

/// Read the whole document into `out`, growing it if needed. Returns the bytes read.
///
/// Replaced five `Editor::text()` calls on the paint path -- `emit_tables`, `emit_math`, `emit_images`,
/// `publish_line_heights`, `image_blocks` -- each of which allocated a `Vec` the size of the document
/// and copied it. Five allocations of the whole document per keystroke is what
/// `tests/session_no_alloc.rs` measured before this existed.
///
/// **A free function taking its two arguments separately, not a `&mut self` method, and that is the
/// whole point.** `Editor::text()` returned an *owned* `Vec`, so callers held a borrow of nothing and
/// could reach every other field freely. A method returning `&[u8]` out of a `Session` field borrows
/// *all* of `self`, and every paint-path caller also needs `&self.chrome`, `&self.editor` and
/// `&mut self.math_scratch` while the bytes are live -- so the first version of this was five borrow
/// errors (E0502 and E0503) rather than five one-line changes. Taking `&Editor` and `&mut Vec<u8>` as
/// arguments borrows two disjoint fields, which is exactly what the callers need.
///
/// **This is still a whole-document copy per paint, and Phase 11 does not pretend otherwise.** What
/// this removes is the *allocation*. What removes the copy is a windowed read -- a paint only needs the
/// bytes of the lines it is about to draw -- which is Phase 12's change to `Painter::text`'s contract.
/// Until then the honest state is one copy per paint instead of five, none of them allocating after
/// the first.
///
/// Grown only, never shrunk: a document that stops growing stops allocating.
/// **Faulting**, as of part 16. It took `&Editor` and could not, so on a container-backed document every
/// whole-document read past the resident window returned `SourceUnavailable` and the emitters that need
/// one -- tables, math, images -- silently drew nothing.
///
/// **`&mut Editor` is the price of asking for the bytes**, and it is the right trade: the alternative was
/// a table, a formula or an image that is in the container and not on the screen.
fn read_document(
    editor: &mut Editor,
    out: &mut Vec<u8>,
    source: &mut dyn holonomy_text::LeafSource,
) -> Result<usize, holonomy_text::EditorError> {
    let len = editor.text_len();
    if out.len() < len {
        // Rounded up to the next multiple of [`SCAN_CHUNK`], because `resize` to exactly `len` means
        // the *next* keystroke -- which makes the document one byte longer -- reallocates again. That
        // is one `realloc` per keystroke for as long as the document grows, which is precisely what
        // the buffer exists to prevent. One extra allocation per 4,096 keystrokes instead.
        let want = len.next_multiple_of(SCAN_CHUNK);
        out.resize(want, 0);
    }
    editor.read_into_faulting(source, 0, out)
}

#[inline]
fn measure_only(node: &MathNode, m: &MathMetrics) -> holonomy_render::math_layout::MathBox {
    holonomy_render::math_layout::measure(node, m)
}

/// Widen `damage` by the rectangle an emitter had to draw, **without ever narrowing a full
/// repaint**.
///
/// # The bug this replaces
///
/// Both emitters used to inline
///
/// ```text
/// *damage = Some(match *damage { Some(d) => d.union(&rect), None => rect });
/// ```
///
/// which reads as "union" and is not one. `None` in this position does not mean *nothing to
/// repaint*: [`Painter::paint`] treats `None` as the whole frame, and `Session::paint(None)` is how
/// a test asks for a full repaint. Turning that `None` into `Some(rect)` silently converted a full
/// repaint into a repaint of one formula's box, and everything outside the box kept whatever pixels
/// it already had -- black, on a first paint.
///
/// It was found by tracing the vertical-placement fix in Phase 9C: after `Session::paint(None)`, a
/// caret drawn at the *old* cell (x 320, y 130) was still on screen four repaints later, because the
/// page fill that erases it was clipped away along with everything outside the formula's rect at
/// (320, 155). The caret's own lookup said (376, 155) and the frame said (320, 130) -- two answers to
/// the same question, which is exactly the class of disagreement this file exists to prevent.
///
/// So the union is now only performed when there is a rect to union into. A full frame cannot be
/// widened by a smaller rectangle, and that is the whole rule.
fn widen(damage: &mut Option<DamageRect>, extra: Option<DamageRect>) {
    if let (Some(d), Some(e)) = (*damage, extra) {
        *damage = Some(d.union(&e));
    }
}

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

// ---------------------------------------------------------------- what a click means
//
// **Three functions, one shape, and the shape is the point.** Each takes what was clicked and returns
// the [`Command`] it means, or `None` for "this button is drawn, hoverable, pressable, and has nothing
// behind it yet".
//
// # Why `None` and not a stub command
//
// **Because a stub command is a lie that survives.** `Command::Bold` that the session ignores would
// make a click on Bold look like it did something -- it would bump `stats.commands`, take a paint, and
// leave the document unchanged. `None` says the honest thing, and the caller counts it in
// [`SessionStats::pointer_inert`] so the number is visible rather than inferred.
//
// # The mapping is by *meaning*, not by position
//
// `tool_command` matches on the [`Tool`] variant, so moving a button in `TOOLBAR` cannot change what
// it does. A table indexed by list position would have that property inverted, and the first toolbar
// edit would have silently swapped two buttons' behaviour.
//
// # What is not here, and why
//
// **`Zoom`, `Style` and `Font` are the three that would obviously be next** -- they are dropdowns in
// the reference and they need a second popup each, which is the same `widgets::popup` machinery with
// different contents. They are `None` because *they* are not built, not because the table is
// incomplete, and a gate asserts the count so the two cannot be confused.

/// What a toolbar button means, or `None` for a button with no action behind it.
fn tool_command(tool: holonomy_render::widgets::Tool) -> Option<holonomy_input::Command> {
    use holonomy_input::{Command, Hotkey};
    use holonomy_render::widgets::Tool;
    Some(match tool {
        Tool::Undo => Command::Hotkey(Hotkey::Undo),
        Tool::Redo => Command::Hotkey(Hotkey::Redo),
        Tool::Image => Command::Hotkey(Hotkey::InsertImage),
        // `Tool::Collapse` is handled in `Session::press` rather than here, because it toggles a
        // chrome flag and is not a document command. **Listed as `None` here rather than omitted** so
        // that this function's arms are exactly the `Tool` variants -- adding a tool makes this
        // non-exhaustive and the compiler says so.
        _ => return None,
    })
}

/// What a title-bar button means, or `None`.
fn title_command(btn: holonomy_render::widgets::TitleButton) -> Option<holonomy_input::Command> {
    use holonomy_input::{Command, Hotkey};
    use holonomy_render::widgets::TitleButton;
    Some(match btn {
        TitleButton::Cloud => Command::Hotkey(Hotkey::Save),
        _ => return None,
    })
}

/// What an [`Action`](holonomy_render::menus::Action) means, as a [`Command`].
///
/// # This replaces part 20's label comparison, and that replacement is the point
///
/// Part 20 wrote:
///
/// ```text
/// Some(match (heading, item.label) { (_, "Undo") => …, ("Insert", "Table") => … })
/// ```
///
/// and noted that renaming a menu entry silently makes it inert. **The fix is that there is no
/// `item.label` here at all.** A row says `Action::Undo`; this function says what that means. The
/// renderer passes the value through without interpreting it and this function interprets it without
/// having seen the row — **neither half can get the other's detail wrong**, which is the property a
/// `match` on a string does not have.
///
/// **Public so a gate can call it with an `Action` and no menu at all** — which is how
/// `an_action_is_routed_without_a_menu_open_around_it` proves the decision does not depend on where
/// the row came from. A private function could only be reached by clicking, and every click would
/// carry a `label` with it, which is the coupling being removed.
///
/// **The `None`s are the honest remainder.** `Save` and `Close` have no action in this build, and
/// `SelectAll` maps to `DocumentEnd` because there is no selection to make. `SetZoom`, `SetStyle`
/// and `SetFont` never reach here — `Session::apply_action` handles those first, because they change
/// chrome state rather than issuing a document command.
pub fn action_command(action: holonomy_render::menus::Action) -> Option<holonomy_input::Command> {
    use holonomy_input::{Command, Hotkey};
    use holonomy_render::menus::Action;
    Some(match action {
        Action::Undo => Command::Hotkey(Hotkey::Undo),
        Action::Redo => Command::Hotkey(Hotkey::Redo),
        Action::SelectAll | Action::DocumentEnd => Command::Hotkey(Hotkey::DocumentEnd),
        Action::DocumentStart => Command::Hotkey(Hotkey::DocumentStart),
        Action::InsertTable => Command::Hotkey(Hotkey::InsertTable),
        Action::InsertImage => Command::Hotkey(Hotkey::InsertImage),
        Action::InsertMath => Command::Hotkey(Hotkey::InsertMath),
        // **The two with no action in this build**, listed so the compiler says so when they are
        // added rather than leaving them to be discovered as a dead row.
        Action::Save | Action::Close => return None,
        Action::SetZoom(_) | Action::SetStyle(_) | Action::SetFont(_) | Action::SetColour(_) => {
            return None
        }
        // **Arming a style is a chrome-state change, not a command** -- the same shape as the three
        // above, and `Session::apply_action` handles it before this function is reached.
        Action::ToggleBold | Action::ToggleItalic => return None,
    })
}

/// The rect of whatever opened `open`, for invalidating.
///
/// **A free function, not a method**, because it is geometry and not state — and because
/// [`widgets::hit_rect`] already has the machinery. This is `Hit`'s rect for the opener, and writing
/// it as a third spelling of "where is that widget" would be exactly the duplication part 19 was
/// about.
///
/// **`ChromeState::default()` and not the session's own state, deliberately.** The opener's rect does
/// not depend on any state — a menu heading is where the menu bar puts it — and taking `&self` here
/// would borrow the session for a value that is a constant function of the layout.
fn opener_rect(l: &holonomy_render::chrome::Layout, open: widgets::Open) -> DamageRect {
    let h = match open {
        widgets::Open::Menu(i) => widgets::Hit::Menu(i),
        widgets::Open::Tool(t) => widgets::Hit::Tool(t),
    };
    widgets::hit_rect(l, &holonomy_render::ChromeState::default(), h)
        .unwrap_or(DamageRect::new(0, 0, 0, 0))
}
