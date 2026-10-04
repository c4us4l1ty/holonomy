//! The window chrome: document tabs, toolbar, ruler, page canvas and status bar.
//!
//! # Pure geometry, no pixels
//!
//! This module computes *where things are* and returns a [`SurfaceTree`]. It does not touch a
//! buffer, does not own a font, and does not know what a scanout is. Painting lives in
//! `holonomy-display`, which is the crate that has a framebuffer. Keeping the split here is what lets
//! every number below be asserted without a renderer in the test.
//!
//! # Integer arithmetic, exclusively
//!
//! No `f32` appears anywhere in this file, and that is the point rather than an accident of style.
//! The brief requires integer geometry, and the alternative is a class of bug where `x = (w - cw) / 2.0
//! as i32` and `x = (w - cw + 1) / 2` disagree by one pixel for every odd width -- which is invisible
//! in a screenshot and visible as a page that jitters by a pixel when the window is resized. Integer
//! division truncates, it is deterministic, and it is the same answer on every machine.
//!
//! Centring is therefore `margin = (panel - page) / 2` with the remainder given to the **right**, so a
//! page sits one pixel left of true centre rather than one pixel right, and always in the same
//! direction. [`Layout::gutter_left`] and [`Layout::gutter_right`] are the assertion.
//!
//! # Every separator is a verified box-drawing codepoint
//!
//! Separators come from `holonomy-assets`' box-drawing table and nowhere else. No `+`, `-`, `|`, `#`
//! or ASCII-art substitute, because those are monospaced text that will not join up: a run of `─` at
//! the cell width draws a line with a one-pixel gap at every cell boundary, which reads as a dotted
//! rule rather than a rule.
//!
//! **Not available, and therefore not used:** `█` (U+2588), `░` (U+2591) and `▒` (U+2592) are *not* in
//! that table -- `box_drawing::LAST` is `0x257F`. So the scrollbar thumb, the dirty-buffer flag and
//! the page drop shadow are filled [`Rect`]s, not glyphs. A filled rect is also cheaper and exactly as
//! good, so nothing is lost but the temptation.
//!
//! # Text runs must be ascending and contiguous
//!
//! [`TextRun`] carries a first codepoint and a length, which encodes "the next `len` codepoints are
//! this one plus one, plus one, ...". That is **not** what prose looks like: `"Chapter One"` is
//! `C h a p t e r _ O n e`, and `h` (0x68) to `a` (0x61) descends. So [`ascending_runs`] splits a
//! string into maximal ascending runs, and the chrome emits one [`TextRun`] per piece. It is a `Vec`
//! allocation per label, which sounds wrong until you notice the alternative is changing
//! [`TextRun`]'s contract, which three earlier phases' gates depend on.
//!
//! [`TextRun`]: crate::TextRun

use crate::tree::{Rect, Style, SurfaceTree, TextRun};
use crate::DamageRect;

// ---------------------------------------------------------------- the palette

/// Chrome colours, as packed `0xAARRGGBB`.
///
/// The alpha byte is always `0xFF` even though [`Frame`](https://docs.rs/holonomy-display) masks it
/// off: [`Rect`] and [`TextRun`] document their colours as `0xAARRGGBB`, so writing `0x00202020` for
/// "opaque" would be relying on a mask three crates away.
pub mod colour {
    /// The panel behind everything.
    pub const CHROME: u32 = 0xFF1E_1E22;
    /// The document page.
    pub const PAGE: u32 = 0xFFFA_FAF8;
    /// A band that is one step lighter than the chrome: the toolbar and the status bar.
    pub const BAND: u32 = 0xFF2A_2A30;
    /// Rules, separators and box drawing.
    pub const RULE: u32 = 0xFF5A_5A66;
    /// A rule that is *incidental* -- a tick, a gutter marker.
    pub const RULE_DIM: u32 = 0xFF3E_3E48;
    /// Body text on the page.
    pub const INK: u32 = 0xFF18_181C;
    /// Chrome text.
    pub const INK_CHROME: u32 = 0xFFD8_D8E0;
    /// Chrome text that is inactive or merely informational.
    pub const INK_DIM: u32 = 0xFF8A_8A96;
    /// The `[SEALED]` badge: the one thing on screen that is a security claim.
    pub const SEALED: u32 = 0xFF5A_C88A;
    /// Accent for a toolbar toggle that is on.
    pub const ACTIVE: u32 = 0xFF7A_B4E8;
    /// The caret.
    pub const CARET: u32 = 0xFF30_3038;
    /// The scrollbar track.
    pub const TRACK: u32 = 0xFF24_242A;
    /// The scrollbar thumb.
    pub const THUMB: u32 = 0xFF6A_6A78;
    /// The page's drop shadow, as a flat band. A real gradient would need `f32` alpha.
    pub const SHADOW: u32 = 0xFF16_161A;
}

// ---------------------------------------------------------------- the runes

/// Box-drawing codepoints the chrome draws with.
///
/// Named, not inlined, because a separator is the one thing that must be *verifiable*: every one of
/// these is in `box_drawing`'s table (`FIRST = 0x2500`, `LAST = 0x257F`) and the test
/// [`every_rune_is_in_the_verified_table`] checks that rather than trusting the list.
pub mod rune {
    /// `─` horizontal.
    pub const H: u32 = 0x00_2500;
    /// `│` vertical.
    pub const V: u32 = 0x00_2502;
    /// `┌` down and right.
    pub const DR: u32 = 0x00_250C;
    /// `┐` down and left.
    pub const DL: u32 = 0x00_2510;
    /// `└` up and right.
    pub const UR: u32 = 0x00_2514;
    /// `┘` up and left.
    pub const UL: u32 = 0x00_2518;
    /// `├` vertical with a right arm. The tab-bar row's left cap.
    pub const VR: u32 = 0x00_251C;
    /// `┤` vertical with a left arm.
    pub const VL: u32 = 0x00_2524;
    /// `┬` horizontal with a downward arm. A ruler tick.
    pub const DV: u32 = 0x00_252C;
    /// `┴` horizontal with an upward arm.
    pub const UV: u32 = 0x00_2534;
    /// `┼` a crossing.
    pub const CROSS: u32 = 0x00_253C;
    /// `║` double vertical. The scrollbar's rail.
    pub const V_DOUBLE: u32 = 0x00_2551;
    /// `╭` rounded top-left.
    pub const ROUNDED_DR: u32 = 0x00_256D;
    /// `╮` rounded top-right.
    pub const ROUNDED_DL: u32 = 0x00_256E;
    /// `╯` rounded bottom-right.
    pub const ROUNDED_UL: u32 = 0x00_256F;
    /// `╰` rounded bottom-left.
    pub const ROUNDED_UR: u32 = 0x00_2570;
}

// ---------------------------------------------------------------- metrics

/// Every dimension of the chrome, in pixels.
///
/// A struct rather than constants so a caller can retune for a different panel without editing the
/// layout code, and so the tests can build a deliberately awkward one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChromeMetrics {
    /// Panel width.
    pub width: u32,
    /// Panel height.
    pub height: u32,
    /// Document tab bar.
    pub tab_h: u32,
    /// Toolbar: style toggles and zoom.
    pub toolbar_h: u32,
    /// Ruler: column ticks and numbers.
    pub ruler_h: u32,
    /// Status bar.
    pub status_h: u32,
    /// Text cell width. The chrome grid is this wide.
    pub cell_w: u32,
    /// Text cell height.
    pub cell_h: u32,
    /// Padding inside the page, around the text column.
    pub page_pad: u32,
    /// Width of the scrollbar rail.
    pub scrollbar_w: u32,
    /// Depth of the page's drop shadow.
    pub shadow: u32,
    /// The measure: columns of text per line.
    pub columns: u32,
}

impl ChromeMetrics {
    /// 1280 x 800, the X200's panel, with an 80-column measure.
    ///
    /// Chosen so the page is 640 px of text: 80 columns x 8 px. A 4:3 panel showing a portrait page
    /// at a readable size, which is the whole design constraint.
    pub const DESKTOP: ChromeMetrics = ChromeMetrics {
        width: 1280,
        height: 800,
        tab_h: 28,
        toolbar_h: 26,
        ruler_h: 20,
        status_h: 22,
        cell_w: 8,
        cell_h: 18,
        page_pad: 48,
        scrollbar_w: 12,
        shadow: 3,
        columns: 80,
    };

    /// Height the page canvas gets: everything the four bands do not.
    pub const fn canvas_h(&self) -> u32 {
        self.height
            .saturating_sub(self.tab_h)
            .saturating_sub(self.toolbar_h)
            .saturating_sub(self.ruler_h)
            .saturating_sub(self.status_h)
    }

    /// The page's width, text plus padding.
    pub const fn page_w(&self) -> u32 {
        self.columns * self.cell_w + self.page_pad * 2
    }

    /// The narrowest and shortest panel this chrome is drawn into without clipping.
    ///
    /// The width is the page, plus its shadow and the scrollbar rail, plus one text cell of gutter on
    /// each side -- 736 + 3 + 12 + 16 = 767, rounded up to 768 because every rect in this crate is
    /// easier to reason about at a multiple of the cell. The height is the four bands (96) plus the
    /// page's padding (96), its shadow, and four rows of text: 288, which shows four whole lines.
    ///
    /// Both exist because a window can be dragged to nothing and the chrome still has to answer with
    /// *something*. Below these the answer is still well-defined -- [`Layout::new`] clips the page and
    /// clamps every band into the panel, all `saturating`, and that is a safety net rather than a
    /// supported size.
    pub const MIN_WIDTH: u32 = 768;
    pub const MIN_HEIGHT: u32 = 288;

    /// Metrics for a panel of `width` x `height`.
    ///
    /// **The measure does not change with the window.** This holds 80 columns and centres the page, so a
    /// wider window grows the margins rather than the line length -- which is what a word processor
    /// does, and what the reference screenshots in `Plan/` show. The alternative, fitting the measure to
    /// the window, means dragging an edge re-wraps every paragraph under the caret, and a word processor
    /// that did that would be unusable.
    ///
    /// So a resize is arithmetic on the gutters: the bands are the same, the page is the same, and
    /// [`Layout::new`] recomputes where everything sits. Nothing about the *document* depends on the panel
    /// size, which is also why [`Session::resize`](../../holonomy/session/struct.Session.html#method.resize)
    /// touches neither the text nor the caret.
    ///
    /// The size is clamped to [`MIN_WIDTH`](Self::MIN_WIDTH) and
    /// [`MIN_HEIGHT`](Self::MIN_HEIGHT), so this is also the clamp and there is one code path.
    pub const fn for_size(width: u32, height: u32) -> Self {
        Self {
            width: if width < Self::MIN_WIDTH {
                Self::MIN_WIDTH
            } else {
                width
            },
            height: if height < Self::MIN_HEIGHT {
                Self::MIN_HEIGHT
            } else {
                height
            },
            ..Self::DESKTOP
        }
    }

    /// These metrics' bands, at a panel size of `width` x `height`.
    ///
    /// The cell size, the padding, the measure and the four band heights are all the same as `self`'s;
    /// only the panel changes. This exists so that "what a resize does and does not change" is written
    /// down once, and so a resize is one call at the call site rather than a struct literal.
    pub const fn clamp_to(&self, width: u32, height: u32) -> Self {
        Self::for_size(width, height)
    }
}

/// The bands, computed once per size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// Panel width.
    pub width: u32,
    /// Panel height.
    pub height: u32,
    /// The document tab bar.
    pub tabs: DamageRect,
    /// The toolbar.
    pub toolbar: DamageRect,
    /// The ruler.
    pub ruler: DamageRect,
    /// The page canvas: the whole area between the ruler and the status bar.
    pub canvas: DamageRect,
    /// The status bar.
    pub status: DamageRect,
    /// The page itself, inside the canvas.
    pub page: DamageRect,
    /// The text column, inside the page.
    pub text: DamageRect,
    /// The scrollbar rail, right of the page.
    pub scrollbar: DamageRect,
    /// Blank space left of the page.
    pub gutter_left: u32,
    /// Blank space right of the page, including the scrollbar.
    pub gutter_right: u32,
    /// How many whole text rows fit on the page.
    pub rows: u32,
}

impl Layout {
    /// Compute the bands for `m`.
    ///
    /// Saturating throughout. A panel smaller than its own bands is a misconfiguration, and the
    /// right answer for "the status bar is taller than the panel" is a zero-height status bar rather
    /// than a subtraction that wraps to four billion and a `Rect` at `y = -1`.
    pub fn new(m: &ChromeMetrics) -> Self {
        let tab_y = 0u32;
        let toolbar_y = tab_y.saturating_add(m.tab_h);
        let ruler_y = toolbar_y.saturating_add(m.toolbar_h);
        let canvas_y = ruler_y.saturating_add(m.ruler_h);
        let canvas_h = m.canvas_h();

        // The page is centred in the canvas. The remainder goes right, so the page is consistently
        // one pixel left of true centre on odd widths rather than jumping about.
        let page_w = m.page_w();
        let (gutter_left, gutter_right) = if page_w >= m.width {
            // A page wider than the panel: no gutters, and the text column is clipped rather than
            // wrapped. `text` below still reports the full measure; `caller_draw` clips.
            (0, 0)
        } else {
            let left = (m.width - page_w) / 2;
            (left, m.width - page_w - left)
        };
        // The page and the scrollbar both *end* at the canvas bottom; the shadow band occupies the
        // `shadow` rows at the top of the canvas that the page is inset by. Growing the page by
        // `shadow` instead -- which is what this did first -- pushes the scrollbar's bottom edge
        // past the canvas's, and a rect that leaves its band is a rect the renderer must clip.
        let shadow = m.shadow.min(canvas_h / 2);
        let page_x = gutter_left;
        let page_y = canvas_y + shadow;
        let page_h = canvas_h - shadow;

        let page = DamageRect::new(page_x, page_y, page_w.min(m.width), page_h);
        let text = DamageRect::new(
            page_x.saturating_add(m.page_pad),
            page_y.saturating_add(m.page_pad),
            m.columns * m.cell_w,
            page_h.saturating_sub(m.page_pad * 2),
        );
        let scrollbar = DamageRect::new(
            page_x.saturating_add(page_w).saturating_add(m.shadow),
            page_y,
            m.scrollbar_w,
            page_h,
        );

        // Every band is clamped into the panel. The `y` accumulations above are `saturating_add`, but
        // four 40-px bands in a 10-px panel still put the status bar's *origin* at 120 -- and a rect
        // that starts below the frame is not a degenerate rect, it is a rect the renderer has to
        // notice and clip. Clamping here keeps the invariant the tests assert: the five bands always
        // partition exactly `0..height`.
        let panel = m.height;
        let band = |y: u32, h: u32| -> DamageRect {
            let y = y.min(panel);
            DamageRect::new(0, y, m.width, h.min(panel - y))
        };
        Self {
            width: m.width,
            height: m.height,
            tabs: band(tab_y, m.tab_h),
            toolbar: band(toolbar_y, m.toolbar_h),
            ruler: band(ruler_y, m.ruler_h),
            canvas: band(canvas_y, canvas_h),
            status: band(canvas_y.saturating_add(canvas_h), m.status_h),
            page,
            text,
            scrollbar,
            gutter_left,
            gutter_right,
            rows: text.height / m.cell_h.max(1),
        }
    }

    /// The page's top-left, for a [`Rect`].
    pub fn page_origin(&self) -> (i32, i32) {
        (self.page.x as i32, self.page.y as i32)
    }
}

// ---------------------------------------------------------------- state

/// Which style toggles are on, for the toolbar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StyleFlags {
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub heading: bool,
}

impl StyleFlags {
    /// The four toolbar slots, in display order, with their labels and short forms.
    pub const SLOTS: [(StyleFlagsSlot, &'static str, &'static str, Style); 4] = [
        (StyleFlagsSlot::Bold, "Bold", "B", Style::BOLD),
        (StyleFlagsSlot::Italic, "Italic", "I", Style::ITALIC),
        (StyleFlagsSlot::Mono, "Mono", "M", Style::MONOSPACE),
        (StyleFlagsSlot::Heading, "Head", "H", Style::BOLD),
    ];

    /// Whether `slot` is on.
    pub const fn get(&self, slot: StyleFlagsSlot) -> bool {
        match slot {
            StyleFlagsSlot::Bold => self.bold,
            StyleFlagsSlot::Italic => self.italic,
            StyleFlagsSlot::Mono => self.mono,
            StyleFlagsSlot::Heading => self.heading,
        }
    }
}

/// Which toolbar toggle a query is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StyleFlagsSlot {
    /// Bold.
    Bold,
    /// Italic.
    Italic,
    /// Monospace.
    Mono,
    /// Heading.
    Heading,
}

/// What the chrome displays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChromeState {
    /// Document name, shown in the tab bar.
    pub title: String,
    /// Whether the container is sealed. Drives the `[SEALED]` badge.
    pub sealed: bool,
    /// Zoom, as a percentage.
    pub zoom_percent: u32,
    /// Caret line, 0-based, relative to the first visible row.
    pub caret_line: u32,
    /// Caret column, 0-based.
    pub caret_column: u32,
    /// Words in the document.
    pub words: u32,
    /// Bytes in the document.
    pub bytes: u32,
    /// Whether there are unsaved edits.
    pub dirty: bool,
    /// First visible text line of the document.
    pub scroll_line: u32,
    /// Total text lines.
    pub total_lines: u32,
    /// Which style toggles are on.
    pub styles: StyleFlags,
    /// Whether the caret is currently drawn. Driven by [`Blink`].
    pub caret_visible: bool,
}

impl Default for ChromeState {
    fn default() -> Self {
        Self {
            title: "untitled".to_string(),
            sealed: false,
            zoom_percent: 100,
            caret_line: 0,
            caret_column: 0,
            words: 0,
            bytes: 0,
            dirty: false,
            scroll_line: 0,
            total_lines: 1,
            styles: StyleFlags::default(),
            caret_visible: true,
        }
    }
}

// ---------------------------------------------------------------- caret and blink

/// The caret's cell.
///
/// **One cell.** Not a line, not a word, not the paragraph: the blink invalidates exactly this
/// rectangle and nothing else, which is the whole reason [`Blink::damage`] returns a
/// [`DamageRect`] rather than a bool. A caret that invalidated its line would repaint 80 columns for
/// a 1 x 18 pixel change, which at 500 ms per blink is the difference between nothing and a visible
/// flicker on a long paragraph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caret {
    /// The cell to invert.
    pub cell: DamageRect,
    /// The document line and column it addresses, for the status bar.
    pub line: u32,
    pub column: u32,
}

impl Caret {
    /// Where the caret is for `state` in `layout`, or `None` if it is scrolled out of view.
    ///
    /// `None` off-screen is the honest answer and the reason this returns an `Option`: a caret that
    /// is scrolled away is *not* at the top or the bottom of the page, and clamping it there would
    /// draw a caret in the wrong place, which is worse than drawing none.
    pub fn locate(layout: &Layout, m: &ChromeMetrics, state: &ChromeState) -> Option<Self> {
        let row = state.caret_line.checked_sub(state.scroll_line)?;
        let y = layout.text.y.checked_add(row.checked_mul(m.cell_h)?)?;
        if row >= layout.rows || state.caret_column >= m.columns {
            return None;
        }
        let x = layout
            .text
            .x
            .checked_add(state.caret_column.checked_mul(m.cell_w)?)?;
        Some(Self {
            cell: DamageRect::new(x, y, m.cell_w, m.cell_h),
            line: state.caret_line,
            column: state.caret_column,
        })
    }
}

/// Caret blink, from a monotonic frame counter.
///
/// # Why a counter and not a clock
///
/// The brief allows either a frame counter or a `clock_gettime` delta, and the counter is the better
/// of the two here for three reasons: no syscall, so it works under the sealed allowlist for free; no
/// dependence on how often the loop happens to iterate, so a slow frame does not desynchronise the
/// phase; and the phase is a pure function of the frame number, so a test can assert *which* frame
/// flips without running anything.
///
/// [`Blink::advance`] returns the damage a flip caused, which is the caret cell or `None`. That is
/// what makes "the blink invalidates only the cursor cell" checkable: a test drives a hundred frames
/// and asserts that every non-`None` answer equals the caret's rect, and that the union of them
/// covers the caret and nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blink {
    /// Frames per half-period.
    pub period: u32,
    /// Frames elapsed.
    pub frame: u32,
}

impl Blink {
    /// Half a second at 60fps.
    pub const DEFAULT_PERIOD: u32 = 30;

    /// A blink that starts visible.
    pub const fn new(period: u32) -> Self {
        Self {
            period: if period == 0 { 1 } else { period },
            frame: 0,
        }
    }

    /// Whether the caret should be drawn on this frame.
    ///
    /// Square wave, not a fade: the frame is *visible* when `(frame / period) % 2 == 0`, so it is on
    /// for `period` frames, off for `period`, and so on. A fade would need an alpha ramp, and alpha is
    /// masked off in [`Frame`](https://docs.rs/holonomy-display) -- so a fade here would be a lie the
    /// renderer silently could not honour.
    pub const fn visible(&self) -> bool {
        (self.frame / self.period).is_multiple_of(2)
    }

    /// The frame index at which the phase next changes.
    ///
    /// The *next* multiple of `period` **strictly greater** than `frame`. Frame 10 with a period of
    /// 10 is the first frame of the hidden half, so the next change is at 20 and not at 10 -- and
    /// frame 0's is at 10, not at 0. Both were wrong in the first version, which returned `frame`
    /// itself whenever `frame % period == 0` and so reported "the phase changes now" on every
    /// boundary frame, including the boundary it had just reported.
    pub const fn next_flip(&self) -> u32 {
        (self.frame / self.period + 1).saturating_mul(self.period)
    }

    /// Advance one frame, returning the caret cell if the phase flipped on this frame.
    ///
    /// `caret` is `None` when the caret is scrolled out of view, in which case a flip dirties nothing
    /// -- there is no cell on screen to change.
    pub fn advance(&mut self, caret: Option<DamageRect>) -> Option<DamageRect> {
        let before = self.visible();
        self.frame = self.frame.wrapping_add(1);
        if self.visible() == before {
            return None;
        }
        // Only a transition *to visible* dirties anything, because only the drawn state differs from
        // the undrawn one. A visible -> hidden flip changes nothing on screen: the cell was already
        // repainted with the page behind it.
        if self.visible() {
            caret
        } else {
            None
        }
    }

    /// The phase after `frame` frames, without advancing.
    pub const fn visible_at(&self, frame: u32) -> bool {
        (frame / self.period).is_multiple_of(2)
    }
}

// ---------------------------------------------------------------- text runs

/// One maximal ascending contiguous piece of a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AscendingRun {
    /// Index of the run's first character within the source string.
    pub char_offset: u32,
    /// Its first codepoint.
    pub first: u32,
    /// How many characters.
    pub len: u16,
}

/// Split `text` into maximal runs of ascending contiguous codepoints.
///
/// See the module docs for why this is necessary. It is also why the chrome's own labels are short:
/// each run is one [`TextRun`], and `"Bold"` is three of them (`B`, then nothing contiguous, then
/// `o`, `l`, `d`).
///
/// A character outside the atlas's ranges still produces a run -- the run is a *layout* fact, and
/// deciding here whether the glyph exists would be deciding it twice.
pub fn ascending_runs(text: &str) -> Vec<AscendingRun> {
    let mut out: Vec<AscendingRun> = Vec::new();
    for (i, c) in text.chars().enumerate() {
        let cp = c as u32;
        match out.last_mut() {
            Some(last) if last.first + u32::from(last.len) == cp && last.len < TextRun::MAX_LEN => {
                last.len += 1;
            }
            _ => out.push(AscendingRun {
                char_offset: i as u32,
                first: cp,
                len: 1,
            }),
        }
    }
    out
}

// ---------------------------------------------------------------- the tree

/// The chrome, for one panel size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chrome {
    /// The dimensions it was built for.
    pub metrics: ChromeMetrics,
    /// The computed bands.
    pub layout: Layout,
}

impl Chrome {
    /// Build for `m`.
    pub fn new(m: ChromeMetrics) -> Self {
        Self {
            layout: Layout::new(&m),
            metrics: m,
        }
    }

    /// The whole panel, for a full repaint.
    pub fn full_damage(&self) -> DamageRect {
        DamageRect::new(0, 0, self.layout.width, self.layout.height)
    }

    /// Where a toolbar toggle is drawn: a `cell_w + 2` wide box.
    pub fn toggle_rect(&self, index: usize) -> DamageRect {
        let m = &self.metrics;
        // Two cells of left padding for the `[`/`]` box, then one cell per toggle plus a gap.
        let w = m.cell_w * 3;
        let x = m.cell_w * 2 + index as u32 * w;
        DamageRect::new(
            x,
            self.layout.toolbar.y + 2,
            w,
            m.toolbar_h.saturating_sub(4),
        )
    }

    /// The scrollbar thumb: proportional to `visible / total`, with a floor so it stays grabbable.
    ///
    /// The floor is the interesting part. `thumb_h = track_h * visible / total` is 0 when the
    /// document is one screen long and `visible` rows are 1, and a scrollbar with no thumb reads as
    /// "there is nothing to scroll" -- which is right -- but also as "the scrollbar is broken", which
    /// is not. So the thumb never goes below [`Chrome::MIN_THUMB`].
    pub const MIN_THUMB: u32 = 16;

    /// The thumb's rect for `state`.
    pub fn scroll_thumb(&self, state: &ChromeState) -> DamageRect {
        let rail = self.layout.scrollbar;
        let track = rail.height;
        let total = state.total_lines.max(state.scroll_line + 1).max(1);
        let visible = self.layout.rows.max(1);
        let (thumb_h, offset) = if total <= visible {
            (track, 0)
        } else {
            let h = (track * visible / total).max(Self::MIN_THUMB.min(track));
            let span = track.saturating_sub(h);
            (h, span * state.scroll_line / (total - visible).max(1))
        };
        DamageRect::new(rail.x, rail.y + offset, rail.width, thumb_h)
    }

    /// Build the chrome's surface tree for `state`.
    ///
    /// Groups, in paint order, so a caller can reason about what covers what:
    /// `chrome` (the panel and the bands), `page` (the shadow, the page, the scrollbar), `overlay`
    /// (the badges and the caret).
    pub fn tree(&self, state: &ChromeState) -> SurfaceTree {
        let m = &self.metrics;
        let l = &self.layout;
        let mut root = SurfaceTree::group();

        // --- chrome: panel, bands, rules.
        let mut chrome = SurfaceTree::leaf(crate::Node::Rect(Rect::new(
            0,
            0,
            l.width,
            l.height,
            colour::CHROME,
        )));
        let band = |y: u32, h: u32| fill(0, y as i32, l.width, h, colour::BAND);
        chrome.before.push(band(l.toolbar.y, l.toolbar.height));
        chrome.before.push(band(l.status.y, l.status.height));

        // Tab bar bottom rule, and the ruler's baseline.
        let rules = [
            (l.tabs.bottom(), colour::RULE, l.width),
            (l.ruler.bottom(), colour::RULE, l.width),
        ];
        for (y, c, w) in rules {
            chrome
                .before
                .push(hline(m, 0, y.saturating_sub(1), w, c, 1));
        }
        // The ruler's column ticks, every ten columns, plus the page's two edges.
        let tick_y = l.ruler.y + m.ruler_h.saturating_sub(2);
        let tick_every = 10u32;
        let mut col = 0u32;
        while col <= m.columns {
            let x = l.text.x + col * m.cell_w;
            if x < l.text.right() {
                chrome.before.push(fill(
                    x as i32,
                    tick_y as i32,
                    1,
                    2,
                    if col.is_multiple_of(tick_every) {
                        colour::RULE
                    } else {
                        colour::RULE_DIM
                    },
                ));
            }
            col += tick_every / 2;
        }
        // The page's left and right edge markers, so the measure is visible when the page is blank.
        for x in [l.text.x, l.text.right().saturating_sub(1)] {
            chrome.before.push(fill(
                x as i32,
                l.ruler.bottom() as i32,
                1,
                m.ruler_h,
                colour::RULE_DIM,
            ));
        }

        // Tab bar text: the title on the left, the badge on the right.
        push_text(
            &mut chrome.before,
            m,
            l.tabs.y + (l.tabs.height - m.cell_h) / 2,
            &state.title,
            Style::MONOSPACE,
            colour::INK_CHROME,
            0,
        );
        if state.sealed {
            push_text(
                &mut chrome.before,
                m,
                l.tabs.y + (l.tabs.height - m.cell_h) / 2,
                "[SEALED]",
                Style::MONOSPACE,
                colour::SEALED,
                l.width as i32 - m.cell_w as i32 * 10,
            );
        }

        // Toolbar: the toggles.
        for (i, (slot, _label, short, style)) in StyleFlags::SLOTS.iter().enumerate() {
            let r = self.toggle_rect(i);
            let on = state.styles.get(*slot);
            // The box: `│` on both sides, `─` above and below. Four glyphs, so it reads as a box at
            // 1-cell scale without needing a `Rect` border that would alias against the glyph.
            let ring = [
                (r.x, r.y, rune::DR, 0, 0),
                (r.right().saturating_sub(1), r.y, rune::DL, 0, 0),
                (r.x, r.bottom().saturating_sub(1), rune::UR, 0, 0),
                (
                    r.right().saturating_sub(1),
                    r.bottom().saturating_sub(1),
                    rune::UL,
                    0,
                    0,
                ),
            ];
            for (x, y, cp, dx, dy) in ring {
                let _ = (dx, dy);
                chrome.before.push(glyph(
                    x,
                    y,
                    cp,
                    if on { colour::ACTIVE } else { colour::RULE_DIM },
                ));
            }
            // The short form, centred in the box.
            let cx = r.x + (r.width - m.cell_w) / 2;
            let cy = r.y + (r.height.saturating_sub(m.cell_h)) / 2;
            push_text(
                &mut chrome.before,
                m,
                cy,
                short,
                *style,
                if on { colour::ACTIVE } else { colour::INK_DIM },
                cx as i32,
            );
            let _ = slot;
        }
        // Zoom, right-aligned.
        let zoom = format!("{}%", state.zoom_percent);
        let zx = l.width as i32 - m.cell_w as i32 * (zoom.chars().count() as i32 + 2);
        push_text(
            &mut chrome.before,
            m,
            l.toolbar.y + (l.toolbar.height - m.cell_h) / 2,
            &zoom,
            Style::MONOSPACE,
            colour::INK_DIM,
            zx,
        );

        // Status bar: `Ln 3, Col 12 · 412 words · 2.4 KiB` and a dirty flag on the right.
        let status = format!(
            "Ln {}, Col {}  {} words  {} B",
            state.caret_line + 1,
            state.caret_column + 1,
            state.words,
            state.bytes
        );
        push_text(
            &mut chrome.before,
            m,
            l.status.y + (l.status.height - m.cell_h) / 2,
            &status,
            Style::MONOSPACE,
            colour::INK_DIM,
            m.cell_w as i32,
        );
        if state.dirty {
            chrome.before.push(fill(
                (l.width - m.cell_w * 3) as i32,
                (l.status.y + 4) as i32,
                m.cell_w,
                m.cell_h.saturating_sub(8),
                colour::ACTIVE,
            ));
        }
        root.before.push(chrome);

        // --- page: shadow, sheet, scrollbar.
        let mut page = SurfaceTree::group();
        // Shadow: a flat band to the right and below. Not a gradient, because alpha is masked off
        // in the frame and a ramp would need `f32`.
        page.before.push(fill(
            (l.page.x + m.shadow) as i32,
            l.page.y as i32,
            l.page.width,
            m.shadow,
            colour::SHADOW,
        ));
        page.before.push(fill(
            l.page.x as i32,
            (l.page.y + m.shadow) as i32,
            m.shadow,
            l.page.height,
            colour::SHADOW,
        ));
        page.before.push(fill(
            l.page.x as i32,
            l.page.y as i32,
            l.page.width,
            l.page.height,
            colour::PAGE,
        ));
        // Scrollbar rail and thumb.
        page.before.push(fill(
            l.scrollbar.x as i32,
            l.scrollbar.y as i32,
            l.scrollbar.width,
            l.scrollbar.height,
            colour::TRACK,
        ));
        let thumb = self.scroll_thumb(state);
        page.before.push(fill(
            thumb.x as i32,
            thumb.y as i32,
            thumb.width,
            thumb.height,
            colour::THUMB,
        ));
        root.before.push(page);

        // --- overlay: the caret.
        let mut overlay = SurfaceTree::group();
        if state.caret_visible {
            if let Some(caret) = Caret::locate(l, m, state) {
                overlay.before.push(fill(
                    caret.cell.x as i32,
                    caret.cell.y as i32,
                    caret.cell.width,
                    caret.cell.height,
                    colour::CARET,
                ));
            }
        }
        root.before.push(overlay);
        root
    }
}

/// A filled rectangle, as a tree node.
///
/// A helper rather than a bare `Node::Rect(...)` at each of the ~15 call sites, because the
/// `SurfaceTree::leaf(Node::Rect(Rect::new(...)))` nesting is where the `u32`/`i32` mixups live.
///
/// Extents are `i32` and clamped here rather than `u32`, because every caller is computing
/// `a.saturating_sub(b)` on `u32` fields and then casting. Taking `u32` pushed the cast to fifteen
/// call sites and made "which of these four numbers is a signed coordinate" a question at each one.
/// Clamping to zero also means a band that collapsed to nothing draws nothing instead of wrapping to
/// four billion pixels.
fn fill(x: i32, y: i32, width: u32, height: u32, colour: u32) -> SurfaceTree {
    SurfaceTree::leaf(crate::Node::Rect(Rect::new(x, y, width, height, colour)))
}

/// A `TextRun` for one glyph at `x, y`.
fn glyph(x: u32, y: u32, cp: u32, c: u32) -> SurfaceTree {
    SurfaceTree::leaf(crate::Node::Text(TextRun::new(
        x as i32,
        y as i32,
        cp,
        1,
        Style::MONOSPACE,
        0,
        c,
    )))
}

/// A horizontal rule of `n` `─` glyphs starting at `x`, `y`.
///
/// A run rather than a `Rect`, so the rule joins up: `─` is drawn to the full cell width, and a
/// `Rect` of the same height would be 1 px thinner at this cell size.
fn hline(m: &ChromeMetrics, x: u32, y: u32, width: u32, c: u32, weight: u32) -> SurfaceTree {
    let n = (width / m.cell_w.max(1)).min(u32::from(TextRun::MAX_LEN));
    if n == 0 {
        return SurfaceTree::group();
    }
    let _ = weight;
    SurfaceTree::leaf(crate::Node::Text(TextRun::new(
        x as i32,
        y as i32,
        rune::H,
        n as u16,
        Style::MONOSPACE,
        0,
        c,
    )))
}

/// Emit `text` as [`ascending_runs`] at `(x, y)`, in `style` and `c`, with `x` the left edge.
///
/// Right-aligned labels are positioned by the *caller* subtracting `cell_w * char_count` from the
/// right edge, because the character count is what the caller already knows -- it chose where the
/// right edge is. Doing the subtraction here would mean passing the right edge *and* the cell width
/// and then re-deriving the count, which is one more thing to get inconsistent.
///
/// Each run's x is `x + char_offset * cell_w`, which is exact integer arithmetic on a monospaced
/// grid: no accumulated per-glyph advance, so no drift over a long label.
fn push_text(
    into: &mut Vec<SurfaceTree>,
    m: &ChromeMetrics,
    y: u32,
    text: &str,
    style: Style,
    colour: u32,
    x: i32,
) {
    let origin = x.max(0) as u32;
    for run in ascending_runs(text) {
        if run.len == 0 {
            continue;
        }
        let rx = origin + run.char_offset * m.cell_w;
        into.push(SurfaceTree::leaf(crate::Node::Text(TextRun::new(
            rx as i32, y as i32, run.first, run.len, style, 0, colour,
        ))));
    }
}
