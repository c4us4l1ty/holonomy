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
use holonomy_render::{Node, Rect, SurfaceTree, TextRun};
use holonomy_text::{Editor, EditorError, SpanPolicy, ANCHOR_BYTES, STYLE_BOLD};
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
    /// Border runs drawn in the last paint.
    pub table_borders_drawn: u32,
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
    /// Line geometry in document byte coordinates, for `O(log n)` line lookup. Phase 11.
    ///
    /// Replaces the newline-counting scans in [`Session::line_start`] and [`Session::line_index`], which
    /// were `O(bytes before the caret)` and ran on every keystroke. See [`crate::doclines`] for why the
    /// terminator convention is this module's problem and not `LineGeometry`'s.
    lines: DocLines,
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
        let metrics = match painter.atlas().map(|a| a.line_pitch()).filter(|&p| p > 0) {
            Some(pitch) => metrics.with_line_pitch(u32::from(pitch)),
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
        let lines = {
            let text = editor.text().unwrap_or_default();
            DocLines::build(&text, holonomy_geometry::LineMetrics::default())
        };
        let counts = TextCounts::scan(&editor);
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
            // Built from the document; see the comment at the `DocLines::build` call above.
            lines,
            counts,
            doc_scratch: Vec::new(),
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
        }
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
        let Some(command) = self.dispatch(event) else {
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
        let at = self.editor.caret() as usize;
        // `GrowIntoInsert` so typing at the end of a bold word keeps it bold, which is what a word
        // processor does and what `SpanPolicy`'s own docs argue for.
        self.editor
            .insert_at(self.editor.caret(), bytes, SpanPolicy::GrowIntoInsert)?;
        self.counts.after_insert(&self.editor, at, bytes);
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
        self.counts
            .after_delete(&self.editor, start, INDENT);
        self.after_edit(INDENT.len() as u32)
    }

    fn backspace(&mut self) -> Result<(), SessionError> {
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
        // The caret's *column* within its line, for the status bar.
        let line_start = self.line_start(self.editor.caret() as usize);
        self.state.caret_column =
            ((self.editor.caret() as usize).saturating_sub(line_start)) as u32;
        self.state.caret_line = self.line_index(self.editor.caret() as usize);
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
    fn after_edit(&mut self, inserted: u32) -> Result<(), SessionError> {
        self.stats.edits += 1;
        self.caret_to(self.editor.caret() as usize)?;
        // The line geometry must agree with the document before anything reads it, and `caret_to` has
        // already used it above — so the order here is not free. **This is the fix for a latent bug,
        // not only an optimisation**: `line_index` used to count newlines, so it was correct whatever
        // the geometry said; now it is a tree lookup, and a tree that a previous edit left stale
        // answers with the *previous* document's line numbers. Every edit syncs, so the tree is never
        // stale at the top of this function.
        self.sync_lines();
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
        self.emit_math(&mut tree, &mut damage);
        self.emit_images(&mut tree, &mut damage);
        // The raster source is borrowed *for this call* and not held: the cache is mutated by the next
        // frame's decode and eviction, so a borrow that outlived the paint would be a self-referential
        // `Session` -- the painter is a field and so is the cache it would have to point at.
        let stats =
            self.painter
                .paint_with_rasters(&mut self.frame, &tree, damage, Some(&self.images))?;
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
        // Formulas join the tables in the *same* model, for the same reason: a fraction is 41 px tall
        // where a line is 18, so the lines below it have to move or the formula draws over them.
        // `LineHeights::from` merges two blocks that share a line by adding, so a table and a formula
        // starting on one line displace by the sum rather than one overwriting the other.
        blocks.extend(self.math_blocks_for());
        // Images join the tables and the formulas in the *same* model, for the same reason: an image
        // is taller than any line, so the lines below it have to move or it draws over them. The block
        // height is the *raster's* height, not the source image's -- §2.9.3 makes those different, and
        // using the source height would displace by a factor of three at 1920x1080.
        blocks.extend(self.image_blocks());
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
    fn image_blocks(&mut self) -> Vec<(u32, u32)> {
        if self.editor.assets().is_empty() {
            return Vec::new();
        }
        let Ok(text) = read_document(&self.editor, &mut self.doc_scratch) else {
            return Vec::new();
        };
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
    fn emit_tables(&mut self, tree: &mut SurfaceTree, damage: &mut Option<DamageRect>) {
        let spans = self.editor.tables().spans().to_vec();
        if spans.is_empty() {
            self.stats.table_cells_drawn = 0;
            self.stats.table_borders_drawn = 0;
            return;
        }
        let Ok(text) = read_document(&self.editor, &mut self.doc_scratch) else {
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
            let line = line_index_in(&self.editor, span.start_byte as usize);
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
    fn emit_images(&mut self, tree: &mut SurfaceTree, damage: &mut Option<DamageRect>) {
        self.stats.images_drawn = 0;
        if self.editor.assets().is_empty() {
            return;
        }
        let Ok(text) = read_document(&self.editor, &mut self.doc_scratch) else {
            return;
        };
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
    fn emit_math(&mut self, tree: &mut SurfaceTree, damage: &mut Option<DamageRect>) {
        self.stats.math_compiled = 0;
        self.stats.math_raw = 0;
        self.stats.math_rules = 0;
        self.stats.math_parse_errors = 0;

        let Ok(text) = read_document(&self.editor, &mut self.doc_scratch) else {
            return;
        };
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
    fn math_blocks_for(&mut self) -> Vec<(u32, u32)> {
        let mut mm = MathMetrics::new(self.chrome.metrics.cell_w, self.chrome.metrics.cell_h);
        mm.advance = advance_shim(self.painter.atlas(), self.painter.size_index());
        let Ok(text) = read_document(&self.editor, &mut self.doc_scratch) else {
            return Vec::new();
        };
        math_blocks(&self.editor, text, &mm)
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
fn line_index_in(editor: &Editor, at: usize) -> u32 {
    let end = at.min(editor.text_len());
    let mut count = 0u32;
    let mut offset = 0usize;
    let mut chunk = [0u8; SCAN_CHUNK];
    while offset < end {
        let want = (end - offset).min(SCAN_CHUNK);
        let got = match editor.read_into(offset, &mut chunk[..want]) {
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
fn math_blocks(editor: &Editor, text: &[u8], mm: &MathMetrics) -> Vec<(u32, u32)> {
    let mut blocks = Vec::new();
    holonomy_text::for_each_math_span(text, |span| {
        let inner = span.inner();
        // An unparseable formula has no box, so it contributes nothing and the raw text's own line
        // height stands. That is the same answer the caret-inside mode gives.
        let Ok(node) = math::parse(&text[inner]) else {
            return;
        };
        let line = line_index_in(editor, span.start as usize);
        blocks.push((line, measure_only(&node, mm).height));
    });
    blocks
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
fn read_document<'e>(
    editor: &'e Editor,
    out: &'e mut Vec<u8>,
) -> Result<&'e [u8], holonomy_text::EditorError> {
    let len = editor.text_len();
    if out.len() < len {
        // Rounded up to the next multiple of [`SCAN_CHUNK`], because `resize` to exactly `len` means
        // the *next* keystroke -- which makes the document one byte longer -- reallocates again. That
        // is one `realloc` per keystroke for as long as the document grows, which is precisely what
        // the buffer exists to prevent. One extra allocation per 4,096 keystrokes instead.
        let want = len.next_multiple_of(SCAN_CHUNK);
        out.resize(want, 0);
    }
    let got = editor.read_into(0, out)?;
    Ok(&out[..got])
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
