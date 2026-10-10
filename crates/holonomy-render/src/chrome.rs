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
    /// The text on a button: one step brighter than [`INK_CHROME`].
    ///
    /// **Separate from `INK_CHROME` so "this item is selected" is a colour rather than a rectangle.**
    /// The reference's open menu heading is the same weight and a slightly brighter ink inside the
    /// same pill, and a selected *item* inside a popup is the same again. Three states, two colours,
    /// and the third state is carried by the pill.
    pub const INK_STRONG: u32 = 0xFFEA_EAF2;
    /// The raised surface: the toolbar's pill, a button, a popup, a selected sidebar row.
    ///
    /// **One value for all four**, because they are the same surface at different sizes. Giving each
    /// its own would be four numbers to keep in step, and the eye reads them as one lighter plane.
    pub const PILL: u32 = 0xFF33_333C;
    /// A hovered widget, one step above [`PILL`].
    ///
    /// **A separate value rather than `PILL_ACTIVE` reused for hover.** The two are one step apart on
    /// purpose: hover is "you are here" and press is "this is happening", and a menu item that looked
    /// the same either way would not tell you which of the two your click had reached.
    pub const PILL_HOVER: u32 = 0xFF38_3845;
    /// A held or selected widget, one step above [`PILL_HOVER`].
    pub const PILL_ACTIVE: u32 = 0xFF42_4252;
    /// An open menu's panel, **lighter than everything it can be drawn over**.
    ///
    /// **This is the third surface and it needed its own value**, which the first version did not
    /// give it: the popup was drawn in [`PILL`], which is also the toolbar's pill. A menu that hangs
    /// over the toolbar and the sidebar is then drawn in exactly the colour of both, and it reads as
    /// *floating text* rather than as a panel -- the items are legible and the menu is invisible.
    /// `a_popup_is_a_lighter_plane_than_anything_it_covers` is the gate, and it would have caught it.
    pub const POPUP: u32 = 0xFF46_4654;
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
    /// **The title band**: the app mark, the document name, the save state, and the right-hand
    /// buttons. Added in part 19.
    pub title_h: u32,
    /// **The menu band**: File / Edit / View / … Added in part 19.
    pub menu_h: u32,
    /// **The document-tabs sidebar**, when it is open. Added in part 19.
    pub sidebar_w: u32,
    /// A toolbar button's box, square. The icons are 16 px and this is the tappable target.
    pub button: u32,
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
        title_h: 30,
        menu_h: 22,
        sidebar_w: 208,
        button: 26,
        tab_h: 0,
        toolbar_h: 34,
        ruler_h: 20,
        status_h: 20,
        cell_w: 8,
        cell_h: 18,
        page_pad: 48,
        scrollbar_w: 12,
        shadow: 3,
        columns: 80,
    };

    /// Adapt these metrics to a line pitch read from the fonts.
    ///
    /// # Why this exists
    ///
    /// `cell_h` is a *derived* number: [`Session::new`](../../holonomy/index.html) overwrites it with
    /// the atlas's [`line_pitch`](https://docs.rs/holonomy-assets/latest/holonomy_assets/atlas/struct.Atlas.html#method.line_pitch),
    /// because the packed faces need 25 px of ascent-plus-descent at 16 ppem while this file shipped
    /// `cell_h: 18`. Four bands vertically centre a cell with a **bare subtraction**:
    ///
    /// ```text
    /// l.status.y + (l.status.height - m.cell_h) / 2
    /// ```
    ///
    /// and a bare `u32` subtraction underflows. That is not a hypothetical -- it panicked at
    /// `chrome.rs:992` in 20 of 22 tests the moment the pitch went from 18 to 25, because
    /// `status_h` was 22. A `saturating_sub` at each site would have hidden it and left the status
    /// text drawn off the top of its band, which is a *worse* bug than a panic.
    ///
    /// So the invariant is made structural instead: **every band that centres a cell is raised to at
    /// least `cell_h`.** A future face with a taller em box then costs one row of chrome height
    /// instead of a panic, and it cannot silently mis-draw.
    ///
    /// `cell_w`, `width`, `height`, `columns` and the padding are untouched -- only the vertical
    /// bands that contain text, and only upward.
    /// Set the line pitch, raising every band that must be at least one line tall.
    ///
    /// # Why zero is left alone, and that is a CORRECTION
    ///
    /// **The original was `band.max(cell_h)` on all five bands.** Part 19 set `tab_h: 0` — a band with
    /// nothing in it should not exist — and this turned it into 25 px in *every running session*, the
    /// moment `Session::new` called `with_line_pitch(atlas.line_pitch())`. The panel grew a 25 px empty
    /// strip between the menu bar and the toolbar, in the product, and the `shot` example rendered it
    /// without anybody reading it as a band at all.
    ///
    /// **`.max()` cannot tell "too small" from "deliberately absent".** It is the right operation for
    /// a band that must fit a line of text and the wrong one for a band that has been switched off,
    /// and the difference is only visible at zero — which is exactly why it survived part 19 and was
    /// caught by a *click* landing on the wrong row of the page.
    pub fn with_line_pitch(mut self, cell_h: u32) -> Self {
        self.cell_h = cell_h;
        self.tab_h = at_least_a_line(self.tab_h, cell_h);
        self.toolbar_h = at_least_a_line(self.toolbar_h, cell_h);
        self.ruler_h = at_least_a_line(self.ruler_h, cell_h);
        self.status_h = at_least_a_line(self.status_h, cell_h);
        self
    }

    /// Height the page canvas gets: everything the four bands do not.
    pub const fn canvas_h(&self) -> u32 {
        // **Every band above the canvas is subtracted, not just the ones that existed when this was
        // written.** Part 19 added the title and menu bands, and the first version of this still
        // subtracted four of the six.
        //
        // **Measured, because the failure was silent:** with `title_h: 30` and `menu_h: 22`, the old
        // formula gave a canvas height of 726 starting at y = 106, so `status.y` came out at **832** --
        // 32 px below the bottom of an 800 px panel. `Layout::new`'s band clamp then gave the status
        // bar a height of **zero**, so it vanished, with no error anywhere: the clamp is `saturating`
        // precisely so that a band which does not fit degrades rather than wrapping, and a zero-height
        // status bar *is* the correct behaviour for a band that does not fit.
        //
        // `Layout::new`'s comment already insists the bands partition the panel; this is where that
        // claim is actually kept, and it is why the fix was to subtract all six rather than to raise
        // `MIN_HEIGHT`.
        self.height
            .saturating_sub(self.title_h)
            .saturating_sub(self.menu_h)
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
    pub const MIN_HEIGHT: u32 = 320;

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
    /**The title band: the app mark, the document name, and the right-hand buttons. Part 19.*/
    pub title: DamageRect,
    /// The menu band: File / Edit / View / … Part 19.
    pub menubar: DamageRect,
    /// The document-tabs sidebar, when it is open. Part 19.
    pub sidebar: DamageRect,
    /// The back arrow at the sidebar's head.
    pub sidebar_back: DamageRect,
    /// The "+" beside the sidebar's heading.
    pub sidebar_new: DamageRect,
    /// How many document rows the sidebar has room for.
    ///
    /// **A count, not a `Vec<DamageRect>`, and that is a correction made inside part 19.** The first
    /// version stored the rows, which made [`Layout`] non-`Copy` -- and then three call sites in
    /// `session.rs` that did `let l = self.chrome.layout;` stopped compiling, because taking a
    /// reference to the layout and then calling `&mut self` is a borrow conflict, so the fix was to
    /// *clone* it. **That is a heap allocation per paint per emitter**, on the one path that has a
    /// latency budget. Storing a count and computing each row from arithmetic keeps `Layout` `Copy`,
    /// which is what the rest of the crate is shaped around.
    pub sidebar_rows: u32,
    /// One row's height, so [`sidebar_doc`](Self::sidebar_doc) is arithmetic and not a stored value.
    pub sidebar_row_h: u32,
    /// The text cell width, copied from [`ChromeMetrics::cell_w`].
    ///
    /// **Copied so that [`widgets::hit`] does not need the metrics.** Part 20's hit test has to
    /// answer "which menu item is this point in", and a popup row's height is a function of the cell
    /// height. Threading `&ChromeMetrics` into `hit` would mean every one of its eight gates grows a
    /// second argument and every caller has to keep the two lifetimes straight; three copied `u32`s on
    /// a `Copy` struct is cheaper than that, and the gate `the_layouts_cell_metrics_are_the_metrics`'
    /// is what stops them drifting apart.
    pub cell_w: u32,
    /// The text cell height, copied from [`ChromeMetrics::cell_h`].
    pub cell_h: u32,
    /// A toolbar button's edge, copied from [`ChromeMetrics::button`].
    pub button: u32,
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
        // **Five bands stack from the top: title, menu, tabs, toolbar, ruler.** Part 19 added the
        // first two. The order is the reference's and it is not arbitrary: the title carries the
        // document's identity, the menu carries the commands, the tabs carry the documents, and the
        // toolbar carries the formatting. A user looking for any of them looks top-down.
        let title_y = 0u32;
        let menu_y = title_y.saturating_add(m.title_h);
        let tab_y = menu_y.saturating_add(m.menu_h);
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
        // **The sidebar is inside the canvas**, so the page is centred in what is left of it rather
        // than in the panel. That is the reference's behaviour and it is the one that keeps the page
        // from jumping sideways when the sidebar opens -- a page that re-centres is a page the reader
        // loses.
        let sidebar_w = m.sidebar_w.min(m.width / 2);
        let sidebar = DamageRect::new(0, canvas_y, sidebar_w, canvas_h);
        let head = sidebar.height.min(40);
        let doc_row_h = m.cell_h.saturating_add(14).max(24);
        // **At least one row, always.** A sidebar with room for no rows cannot show a document, and
        // "the panel is too short" is a resize rather than a state, so the honest answer is one row
        // clipped rather than none.
        let rows_visible = (sidebar.height.saturating_sub(head + 16) / doc_row_h).max(1);
        let sidebar_back = DamageRect::new(sidebar.x + 8, sidebar.y + 8, 24, 24);
        let sidebar_new =
            DamageRect::new(sidebar.right().saturating_sub(32), sidebar.y + 10, 24, 24);
        Self {
            width: m.width,
            height: m.height,
            title: band(title_y, m.title_h),
            menubar: band(menu_y, m.menu_h),
            sidebar,
            sidebar_back,
            sidebar_new,
            sidebar_rows: rows_visible,
            sidebar_row_h: doc_row_h,
            cell_w: m.cell_w,
            cell_h: m.cell_h,
            button: m.button,
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

    /// The document position a click at `(x, y)` lands on, if it landed on the page.
    ///
    /// # This is the inverse of [`Caret::locate`], and it has to agree with it exactly
    ///
    /// `locate` answers "where is the caret"; this answers "where did the pointer go". A click
    /// places the caret, so the two are the same fact computed in two directions, and **a disagreement
    /// of one pixel puts the caret in the next cell** -- which on a click near the right edge of a line
    /// lands the caret one column past the end, and on a click below the last line lands it on a row
    /// that is not on screen.
    ///
    /// **Uniform rows only.** `locate` places row `r` at `text.y + line_heights.y(r)`, which accounts
    /// for table rows of varying height; this computes `row = (y - text.y) / cell_h`. With no tables
    /// — which is every document this session can hold today — they are the same arithmetic. **The
    /// gate `clicking_a_row_puts_the_caret_on_that_row` is what would fail first if a document with
    /// tables landed**, and it is written to fail rather than to quietly put the caret a row early.
    ///
    /// **Clamped to the text column rather than rejected.** A click to the left of the first character
    /// is a click on line 1, not a click nowhere: a pointer is a coarse instrument and refusing it
    /// would make the left third of the page unclickable.
    #[must_use]
    pub fn caret_at(&self, state: &ChromeState, x: i32, y: i32) -> Option<(u32, u32)> {
        if !self.page.contains(x, y) {
            return None;
        }
        let t = self.text;
        let row = if y < t.y as i32 {
            0
        } else {
            u32::try_from(y - t.y as i32).ok()? / self.cell_h
        };
        if row >= self.rows {
            return None;
        }
        let column = if x < t.x as i32 {
            0
        } else {
            u32::try_from(x - t.x as i32).ok()? / self.cell_w
        };
        Some((row + state.scroll_line, column))
    }

    /// Document row `i` of the sidebar, if the sidebar is that tall.
    ///
    /// **`None` past [`sidebar_rows`](Self::sidebar_rows), and not a clamped rect.** The caller asked
    /// for a row that is not on screen; answering with the last row would make row 40 clickable when
    /// row 5 is the last one drawn, which is the off-by-N that a hit test must not have.
    pub fn sidebar_doc(&self, i: usize) -> Option<DamageRect> {
        if i as u32 >= self.sidebar_rows {
            return None;
        }
        let head = self.sidebar.height.min(40);
        Some(DamageRect::new(
            self.sidebar.x + 8,
            self.sidebar.y + head + 16 + (i as u32) * self.sidebar_row_h,
            self.sidebar.width.saturating_sub(16),
            self.sidebar_row_h.saturating_sub(4),
        ))
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

/// Per-line vertical geometry, as a uniform pitch plus a sparse table of extra heights.
///
/// # Why this exists, and what it is not fixing
///
/// Phase 9A put a table on the page as a block anchored at its document line, and recorded a debt:
/// "lines *below* it are not offset by its height, so text after a tall table overlaps its last row."
/// **That claim was wrong about the present and right about the future.** `Chrome::tree` draws no
/// document body text at all -- the page behind the chrome is blank -- so there is nothing to overlap
/// yet. What is missing is not a fix but the shared model that a fix would need, built now so that
/// when body text arrives it lands in the right row for free.
///
/// So: every line is `pitch` pixels tall, except the lines listed in `extras`, which are `pitch + n`
/// tall. A table contributes its whole visual height at its anchor line, so everything below moves
/// down by the difference between what the table needs and the lines it was using.
///
/// # Why a sparse table rather than a `Vec<u32>` per line
///
/// A 2000-page document has tens of thousands of lines and, in scope, a handful of tables. A dense
/// vector is tens of thousands of `u32`s to answer a question that has one non-zero entry per table,
/// and it has to be rebuilt on every keystroke because an insertion moves every line index after it.
/// A sparse `(line, extra)` list is proportional to the number of *tables*, which is what actually
/// changes shape.
///
/// # Exactness
///
/// `u32` throughout, and `y` is a prefix sum of whole pixels. There is no rounding and no
/// interpolation, so "the line below a 4-row table starts at y" is a number rather than a tolerance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineHeights {
    /// Every line's base height in pixels. Normally the text cell height.
    pub pitch: u32,
    /// Extra pixels at `(line, pixels)`, ascending by line. Not required to be sorted by the caller:
    /// `from_tables` sorts, and `y` assumes sorted.
    pub extras: Vec<(u32, u32)>,
}

impl LineHeights {
    /// A uniform model: every line `pitch` tall.
    pub const fn uniform(pitch: u32) -> Self {
        Self {
            pitch,
            extras: Vec::new(),
        }
    }

    /// The height of `line` itself.
    pub fn height(&self, line: u32) -> u32 {
        self.pitch + self.extra_at(line)
    }

    /// The extra at `line`, or zero. Binary search, because `extras` is sorted.
    ///
    /// A linear scan would be three or four comparisons in scope and `O(n)` in a document with many
    /// tables, on a path that the caret lookup takes on **every** frame. The sort in [`Self::from`]
    /// is what makes the search valid; `LineHeights` is public, so a caller that constructs one by
    /// hand with unsorted `extras` gets wrong answers -- which is why `extras` is documented as
    /// ascending rather than left implicit.
    fn extra_at(&self, line: u32) -> u32 {
        match self.extras.binary_search_by_key(&line, |&(l, _)| l) {
            Ok(at) => self.extras[at].1,
            Err(_) => 0,
        }
    }

    /// The y of `line`'s top, relative to the text rectangle.
    ///
    /// `line * pitch` plus every extra at or before it. A table's extra is at its **anchor** line, so
    /// the table itself occupies that line's slot and everything below it moves down -- which is the
    /// whole point.
    pub fn y(&self, line: u32) -> u32 {
        let base = line.saturating_mul(self.pitch);
        let mut extra = 0u32;
        for &(l, px) in &self.extras {
            if l <= line {
                extra = extra.saturating_add(px);
            } else {
                break;
            }
        }
        base.saturating_add(extra)
    }

    /// The total height of `lines` lines.
    pub fn total(&self, lines: u32) -> u32 {
        self.y(lines)
    }

    /// The model a set of blocks implies.
    ///
    /// `blocks` is `(anchor_line, visual_height_px)`, one per table, in document order.
    ///
    /// # The extra **is** the height, and the net-subtracting version was wrong
    ///
    /// The obvious reading is that a table of `rows` rows was already using `rows` line slots and so
    /// only needs to report the difference. It cannot work, because `from` is handed **pixels**, not a
    /// row count: it has no way to know how many slots the table was notionally occupying, so it
    /// recomputes `ceil(height / pitch)` and subtracts that -- which is **zero** for every height that
    /// is a whole number of slots. The gate caught it: a 9-slot, 162 px table reported an extra of 0,
    /// and no line below it moved.
    ///
    /// The correct model is simpler and needs no row count. A table anchored at line `L` occupies
    /// `[L * pitch, L * pitch + height)`, so **every line from `L` onward moves down by the full
    /// height**, including `L` itself -- otherwise the table is drawn over the first line of text
    /// after it. [`Self::slots_for`] remains the right way to ask how many lines a block *covers*,
    /// which is a different question, used for scroll extents rather than displacement.
    ///
    /// Two blocks on one line add, rather than the second overwriting the first.
    pub fn from(pitch: u32, blocks: &[(u32, u32)]) -> Self {
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(blocks.len());
        for &(line, height) in blocks {
            if height == 0 {
                // A block with no height displaces nothing, and recording `(line, 0)` would put an
                // entry into a list that `extra_at` binary-searches on every caret lookup.
                continue;
            }
            match merged.iter_mut().find(|(l, _)| *l == line) {
                Some((_, px)) => *px = px.saturating_add(height),
                None => merged.push((line, height)),
            }
        }
        merged.sort_by_key(|&(l, _)| l);
        Self {
            pitch,
            extras: merged,
        }
    }

    /// How many line slots a block of `height` pixels covers, rounded up.
    ///
    /// Ceiling rather than floor, because a block that is 2.5 pitches tall still displaces three
    /// lines' worth of following text, not two. Rounding down would let the last line of text sit
    /// inside the block's last quarter.
    pub const fn slots_for(height: u32, pitch: u32) -> u32 {
        if pitch == 0 {
            return 0;
        }
        // `u32::from(bool)` is not a `const fn` on this toolchain (rust-lang/rust#143874), so the
        // ceiling is spelled out. Both operands are already `u32` and `pitch` is non-zero here.
        height / pitch + if height % pitch == 0 { 0 } else { 1 }
    }
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
    /// Whether the document-tabs sidebar is open.
    ///
    /// **A flag rather than a width, and the width is always [`ChromeMetrics::sidebar_w`].** A sidebar
    /// that animates its width needs a width in the state and a rectangle that moves every frame;
    /// this needs a boolean and a rectangle that is either there or is not. The animation is not worth
    /// a per-frame layout on a panel where the sidebar is 208 px wide.
    pub sidebar_open: bool,
    /// The documents in the sidebar: their titles, not their models.
    ///
    /// **Titles, and that is the whole of the boundary.** `Session` owns the documents; the chrome owns
    /// the list. Everything the chrome draws from a document is a title and an is-active bit, and
    /// putting a model here would make the renderer responsible for the thing it is a view of.
    pub docs: Vec<String>,
    /// Which entry of [`docs`](Self::docs) is active.
    pub active_doc: usize,
    /// The paragraph style's name, as the toolbar shows it.
    pub style_name: String,
    /// The font's name, as the toolbar shows it.
    pub font_name: String,
    /// The font size in points, as the toolbar shows it.
    pub font_size: u32,
    /// Which menu is open, if any. An index into [`MENUS`](crate::chrome::MENUS).
    pub open_menu: Option<usize>,
    /// Where the pointer is, once it has moved. `None` until the first motion event.
    ///
    /// **`None` and not `(0, 0)`.** The session's decoder starts the pointer at the origin so that
    /// the first `REL_X` moves it by its delta, and that is the right answer for *accumulating*; it
    /// is the wrong answer for *drawing*, because a chrome that has never seen a pointer should not
    /// draw one hovering over its first widget. **These are different questions with different right
    /// answers**, which is why there are two values rather than one.
    pub cursor: Option<(i32, i32)>,
    /// What the pointer is over, for the chrome's own highlight.
    pub hover: Option<crate::widgets::Hit>,
    /// What is held down, so a button reads as pressed while the pointer is on it.
    ///
    /// **Set on the press, cleared on the release, and never on a motion event** -- so a drag off a
    /// button leaves it looking pressed until the button comes up, which is what every native
    /// toolchain does and is why a drag off a button does not fire it.
    pub pressed: Option<crate::widgets::Hit>,
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
    /// Per-line vertical geometry, so a table pushes the lines below it down.
    ///
    /// Owned by the chrome rather than passed alongside it because the *caret* needs it: `Caret::locate`
    /// takes a `ChromeState` and nothing else, so a caret below a tall table would be placed at the
    /// uniform-pitch row -- the same overlap the model exists to prevent, one line up and one caret to
    /// the left of where it should be.
    ///
    /// # The default is a zero pitch, and that is a trap
    ///
    /// `Default` cannot know a pitch, so it uses `LineHeights::uniform(0)`, and a state left at the
    /// default puts **every** line at `y = 0` -- so `Caret::locate` puts the caret on the page's first
    /// row whatever `caret_line` says. That is not hypothetical: it broke
    /// `the_caret_sits_on_its_column_and_row` the moment this field was added, which is the only
    /// reason it is written down here. A state used for a caret lookup **must** carry a real pitch;
    /// `Session` does, from `ChromeMetrics::cell_h`, on every paint.
    pub line_heights: LineHeights,
}

impl Default for ChromeState {
    fn default() -> Self {
        Self {
            title: "untitled".to_string(),
            sidebar_open: true,
            docs: Vec::new(),
            active_doc: 0,
            style_name: "Normal text".to_string(),
            font_name: "Inter".to_string(),
            font_size: 11,
            open_menu: None,
            cursor: None,
            hover: None,
            pressed: None,
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
            line_heights: LineHeights::uniform(0),
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
        // `row * cell_h` plus whatever extras the model carries at or before this line, rather than
        // the uniform product. With no tables the extras are empty and this is the old expression, so
        // the caret is in the same place it always was.
        let y = layout.text.y.checked_add(state.line_heights.y(row))?;
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

/// The menu bar's headings, left to right.
///
/// **A `const`, so the order is fixed at compile time**, and `ChromeState::open_menu` is an index into
/// this. An index rather than an enum because a menu *is* a position in this list: painting heading `i`
/// and testing `open_menu == Some(i)` cannot disagree when both are `i`.
pub const MENUS: &[&str] = &[
    "File",
    "Edit",
    "View",
    "Insert",
    "Format",
    "Tools",
    "Extensions",
    "Help",
];

/// The menu bar's heading boxes, and where the next one starts.
///
/// **The width is measured, not tabulated.** A `const` table of eight widths would need a column per
/// menu and would be wrong the moment a heading was renamed, and the measurement is `str::len()`
/// because every heading is ASCII -- **which is a property of [`MENUS`] and not an accident**, and the
/// assertion in `menu_width_is_measured_not_tabulated` is what keeps it one.
pub fn menu_boxes(l: &Layout) -> (u32, Vec<DamageRect>) {
    let mut x = l.menubar.x + 12;
    let mut out = Vec::with_capacity(MENUS.len());
    for m in MENUS {
        let w = menu_width(m);
        out.push(DamageRect::new(x, l.menubar.y, w, l.menubar.height));
        x += w + MENU_GAP;
    }
    (x, out)
}

/// The width of one heading's box: the text plus its padding, in cells.
///
/// **Two cells of padding on each side.** One is not enough -- the boxes would touch and the seam
/// would be ambiguous, which is exactly what [`DamageRect::contains`]'s half-open rule is there to
/// resolve, and a rule that is needed because the layout is too tight is a sign the layout is too
/// tight.
pub const fn menu_width(m: &str) -> u32 {
    (m.len() as u32 + 4) * crate::chrome::CHROME_CELL_W
}

/// The gap between two heading boxes.
pub const MENU_GAP: u32 = 2;

/// `band`, unless it is zero, in which case it stays zero.
///
/// **A `const fn` of its own so the rule has one name.** There are four call sites and the rule is the
/// whole of the correction above; a helper makes "which bands does this touch" a one-line question.
const fn at_least_a_line(band: u32, cell_h: u32) -> u32 {
    if band == 0 {
        0
    } else if band > cell_h {
        band
    } else {
        cell_h
    }
}

/// The chrome's text cell width.
///
/// **Duplicated rather than reached through [`ChromeMetrics`], because [`menu_width`] is a `const`.**
/// A `const fn` cannot read a runtime metrics struct, and making the menu bar's geometry depend on a
/// runtime value would mean the headings move when the font metrics are recalibrated -- which is a
/// rearrangement nobody would ask for. The value is `ChromeMetrics::cell_w`'s and the test
/// `the_menu_cell_width_is_the_metrics_cell_width` says so, so the two cannot drift apart silently.
pub const CHROME_CELL_W: u32 = 8;

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
        //
        // **The panel is in `before`, not in `node`, and that is a CORRECTION.** It used to be
        // `SurfaceTree::leaf(Node::Rect(panel))`, which puts the panel in `node` and every band, rule
        // and label in `before` -- and `Painter::walk` draws `before`, then `node`, then `after`. **So
        // the panel painted last and erased the entire chrome.** The symptom was that the editor
        // rendered as a bare page on a flat panel with no tab bar, no toolbar, no ruler, no status bar
        // and no title: everything the chrome drew, the chrome's own background drew over. Only the
        // page and the caret survived, because they hang off `root`, whose `node` is `None`.
        //
        // **It was invisible to every gate for the same reason the paint path's fault was:** the gates
        // assert what the chrome *emits* -- node counts, colours, bounds -- and this was a statement
        // about *paint order between two siblings*, which nothing asserted. See
        // `crates/holonomy-render/tests/chrome_paint_order.rs`.
        let mut chrome = SurfaceTree::group();
        chrome
            .before
            .push(SurfaceTree::leaf(crate::Node::Rect(Rect::new(
                0,
                0,
                l.width,
                l.height,
                colour::CHROME,
            ))));
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

        // --- the title band, the menu band, the toolbar and the sidebar. Part 19.
        //
        // **All four read their geometry from `widgets::`**, so what is painted here and what
        // `widgets::hit` answers cannot drift. That is the entire reason `widgets.rs` exists: the
        // failure mode of a hit test is a button that is drawn in one place and pressed in another,
        // and the only cure is for there to be one place.
        paint_title(&mut chrome.before, m, l, state);
        paint_menubar(&mut chrome.before, m, l, state);
        paint_toolbar(&mut chrome.before, m, l, state);
        if state.sidebar_open {
            paint_sidebar(&mut chrome.before, m, l, state);
        }

        // **The tab band is gone** (`tab_h: 0` as of part 19). It drew the document's name at the
        // left of a 28 px band, and the title band now draws the same name at the left of a 30 px band
        // with the app mark beside it -- so for one phase the name appeared twice, with the second
        // copy's band empty. **A band that has nothing in it is a band that should not exist**, and the
        // reference has no such row either: its document name sits in the same line as the menus.
        //
        // **The `[SEALED]` badge moves to the title band** rather than disappearing: a sealed session
        // is the one state where the user most needs to be told, and it was never the title that
        // mattered for that.
        if state.sealed {
            push_text(
                &mut chrome.before,
                m,
                l.title.y + (m.title_h - m.cell_h) / 2,
                "[SEALED]",
                Style::MONOSPACE,
                colour::SEALED,
                (l.title.right() - 14 * m.cell_w) as i32,
            );
        }

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
// ---------------------------------------------------------------- pointer state
//
// **Three functions, and they are the whole of the chrome's response to a pointer.** Every emitter
// that draws a clickable thing asks them two questions and nothing else -- is this hovered, is this
// pressed -- and the answers come from `ChromeState`, which the session wrote.
//
// **They are functions rather than a field on `Hit` because `Hit` has to stay `Copy` and hashable for
// `hit()` to return it cheaply, and because a widget's look is a question about *state*, not a
// property of what it is.** Nothing about `Hit::Tool(Tool::Bold)` says whether it is currently under
// the pointer; that is a fact about the world.

/// Whether `h` is what the pointer is over.
#[inline]
fn hovered(state: &ChromeState, h: crate::widgets::Hit) -> bool {
    state.hover == Some(h)
}

/// Whether `h` is what is held down.
///
/// **Hover and press are different colours, and the difference is one step.** The reference dims a
/// button while it is held, which is the only affordance that tells you the press was received before
/// the release fires whatever it was going to fire.
#[inline]
fn pressed(state: &ChromeState, h: crate::widgets::Hit) -> bool {
    state.pressed == Some(h)
}

/// The surface a widget's hover or press state is drawn on.
///
/// **One call site per emitter rather than a shared `push_state`**, because what is drawn underneath
/// differs -- a toolbar button sits on the pill, a popup row sits on the popup, a menu heading sits on
/// the panel -- and a helper that did not know that would have to take the background as an argument
/// and then be wrong twice.
fn state_fill(state: &ChromeState, h: crate::widgets::Hit) -> Option<u32> {
    if pressed(state, h) {
        Some(colour::PILL_ACTIVE)
    } else if hovered(state, h) {
        Some(colour::PILL_HOVER)
    } else {
        None
    }
}

// ---------------------------------------------------------------- the new chrome bands
//
// **Four emitters, and they are the only place in the crate that knows the reference's layout.** Each
// takes its geometry from [`crate::widgets`], so the paint path and the hit-test path read the same
// numbers. A band that was drawn from `Layout` arithmetic of its own would be a band the pointer could
// not find, and the symptom -- "the button is there but clicking it does nothing" -- is the single most
// expensive kind of UI bug to diagnose because everything looks right.

/// The title band: the app mark, the document's name, and the buttons on the right.
///
/// **The name is left-aligned after the mark and the buttons are right-anchored**, so the name gets
/// whatever room is left rather than a fixed column. A fixed column is a fixed column that a long
/// document name eventually overflows, and overflow at the left of a title bar truncates the mark.
fn paint_title(into: &mut Vec<SurfaceTree>, m: &ChromeMetrics, l: &Layout, state: &ChromeState) {
    into.push(fill(0, l.title.y as i32, l.width, m.title_h, colour::BAND));

    // The app mark: a document icon, at the left, at the band's own size rather than the toolbar's.
    let icon_y = l.title.y + (m.title_h - crate::icons::SIZE) / 2;
    into.push(SurfaceTree::leaf(crate::Node::Icon(
        crate::icons::IconId::Doc.at(14, icon_y as i32, colour::INK_CHROME),
    )));
    push_text(
        into,
        m,
        l.title.y + (m.title_h - m.cell_h) / 2,
        &state.title,
        Style::MONOSPACE,
        colour::INK_CHROME,
        14 + crate::icons::SIZE as i32 + 10,
    );

    for (btn, r) in crate::widgets::place_title(l, 12) {
        let icon = match btn {
            crate::widgets::TitleButton::Star => crate::icons::IconId::Star,
            crate::widgets::TitleButton::Folder => crate::icons::IconId::Folder,
            crate::widgets::TitleButton::Cloud => crate::icons::IconId::Cloud,
            crate::widgets::TitleButton::History => crate::icons::IconId::History,
            crate::widgets::TitleButton::Comments => crate::icons::IconId::Comment,
            crate::widgets::TitleButton::Share => crate::icons::IconId::Lock,
        };
        // **Share is a button with a label**, so it is drawn as one; the rest are the icon alone,
        // centred in their own box.
        if let Some(c) = state_fill(state, crate::widgets::Hit::Title(btn)) {
            into.push(fill(r.x as i32, r.y as i32, r.width, r.height, c));
        }
        // **Share is the one button that is an icon *and* a label, and they are laid out left to right.**
        //
        // The first version centred the lock in the button and then drew "Share" at a fixed offset
        // from the left, so a 110 px button put the lock at x + 47 and the text at x + 30 -- the text
        // started *before* the icon and ran through it. The fix is not a bigger offset, it is that a
        // button with a label does not centre its icon: the icon goes where the label goes, and the
        // label follows it. That is why this is a branch and not a constant.
        if matches!(btn, crate::widgets::TitleButton::Share) {
            into.push(fill(
                r.x as i32,
                r.y as i32,
                r.width,
                r.height,
                colour::PILL,
            ));
            let cy = r.y + (r.height.saturating_sub(m.cell_h)) / 2;
            let ix = r.x + m.cell_w / 2;
            into.push(SurfaceTree::leaf(crate::Node::Icon(
                crate::icons::IconId::Lock.at(
                    ix as i32,
                    r.y as i32 + (r.height.saturating_sub(crate::icons::SIZE)) as i32 / 2,
                    colour::INK_CHROME,
                ),
            )));
            push_text(
                into,
                m,
                cy,
                "Share",
                Style::MONOSPACE,
                colour::INK_CHROME,
                ix as i32 + crate::icons::SIZE as i32 + m.cell_w as i32 / 2,
            );
            continue;
        }
        let ix = r.x + (r.width.saturating_sub(crate::icons::SIZE)) / 2;
        into.push(SurfaceTree::leaf(crate::Node::Icon(icon.at(
            ix as i32,
            r.y as i32 + (r.height.saturating_sub(crate::icons::SIZE)) as i32 / 2,
            colour::INK_CHROME,
        ))));
    }
}

/// The menu band: the headings, and the open menu's popup underneath.
///
/// **The popup is drawn here rather than in the session**, because the popup's geometry is a function of
/// the band it hangs from and of the heading that opened it, and both are the chrome's. **What a menu
/// item *does* is the session's**, and the session reads [`crate::chrome::MENUS`] for the same index.
fn paint_menubar(into: &mut Vec<SurfaceTree>, m: &ChromeMetrics, l: &Layout, state: &ChromeState) {
    // **No band fill here either, and for the same reason `paint_toolbar`'s gives.** The first version
    // pushed `fill(.., CHROME)` for the menu band, which is the panel's own colour -- so it painted the
    // panel over the chrome's toolbar band and made that band invisible. `Chrome::tree` owns the band
    // fills; an emitter that re-fills its band has to know the band is not the panel, and neither of
    // these two needs to know anything at all.
    let (_, boxes) = menu_boxes(l);
    for (i, r) in boxes.iter().enumerate() {
        let open = state.open_menu == Some(i);
        // **Open outranks hovered.** A heading whose menu is down is filled whether or not the pointer
        // is on it, because it stays filled after the pointer leaves -- and a heading that lost its
        // fill the moment the pointer left would tell the user the menu had closed.
        if open {
            into.push(fill(
                r.x as i32,
                r.y as i32,
                r.width,
                r.height,
                colour::PILL,
            ));
        } else if let Some(c) = state_fill(state, crate::widgets::Hit::Menu(i)) {
            into.push(fill(r.x as i32, r.y as i32, r.width, r.height, c));
        }
        push_text(
            into,
            m,
            r.y + (r.height.saturating_sub(m.cell_h)) / 2,
            MENUS[i],
            Style::MONOSPACE,
            if open {
                colour::INK_STRONG
            } else {
                colour::INK_CHROME
            },
            r.x as i32 + 2 * m.cell_w as i32,
        );
    }

    // The popup, if one is open.
    if let Some(i) = state.open_menu {
        paint_popup(into, m, l, state, i);
    }
}

/// An open menu's popup: a panel under its heading, with the items.
///
/// **Drawn at a fixed height per item and anchored under the heading's left edge**, which is what makes
/// a menu readable: the items start where the heading does, so the eye does not have to travel. The
/// width is the widest label plus padding, computed here rather than tabulated, because the items are
/// data.
fn paint_popup(
    into: &mut Vec<SurfaceTree>,
    m: &ChromeMetrics,
    l: &Layout,
    state: &ChromeState,
    index: usize,
) {
    // **The geometry comes from `widgets::popup`, not from here.** The first version computed the
    // popup's width, height and row positions inline, from `menus::items_for`, and the hit test had
    // nothing to test against -- which is the exact duplication `widgets.rs` exists to prevent, and
    // this function was the other half of it. **One function, two consumers**, so a row that is drawn
    // is a row that can be clicked, by construction.
    // **A menu with no items draws nothing, and that is not an error.** `open_menu` is set from a
    // heading index and every heading has items, so this arm is unreachable through the session -- but
    // `popup` is `Option` because a *caller* could pass a bad index, and the answer to that is a
    // silent no-op rather than a panic inside a paint.
    let Some(pop) = crate::widgets::popup(l, index) else {
        return;
    };
    let items = crate::menus::items_for(index);
    let pad = 2 * m.cell_w;
    let x = pop.rect.x as i32;
    let y = pop.rect.y as i32;

    into.push(fill(x, y, pop.rect.width, pop.rect.height, colour::POPUP));
    // The rule under the heading, which is what makes a popup read as *belonging to* that heading
    // rather than as a floating panel.
    into.push(SurfaceTree::leaf(crate::Node::Rect(crate::Rect::new(
        x,
        y,
        pop.rect.width,
        1,
        colour::RULE,
    ))));

    for (i, it) in items.iter().enumerate() {
        let Some(row) = pop.rows.get(i) else { break };
        // **Hover and press come from the session, not from a re-hit-test here.** The painter does not
        // know where the pointer is -- it is told. A painter that re-derived the hit would be a second
        // hit test, which is the thing that has just been removed.
        if row_is_live(state, index, i) {
            into.push(fill(
                row.x as i32,
                row.y as i32,
                row.width,
                row.height,
                colour::PILL_ACTIVE,
            ));
        }
        let cy = row.y + (row.height.saturating_sub(m.cell_h)) / 2;
        if let Some(icon) = it.icon {
            into.push(SurfaceTree::leaf(crate::Node::Icon(icon.at(
                x + pad as i32,
                row.y as i32 + (row.height.saturating_sub(crate::icons::SIZE)) as i32 / 2,
                colour::INK_CHROME,
            ))));
        }
        push_text(
            into,
            m,
            cy,
            it.label,
            Style::MONOSPACE,
            colour::INK_CHROME,
            x + pad as i32 + 2 * m.cell_w as i32,
        );
        // **The accelerator, right-aligned, because the reference puts it there** and a reader looking
        // for "Ctrl+K" looks at the right edge of the row.
        if let Some(acc) = it.accelerator {
            if !acc.is_empty() {
                push_text(
                    into,
                    m,
                    cy,
                    acc,
                    Style::MONOSPACE,
                    colour::INK_DIM,
                    x + (pop.rect.width - pad - acc.len() as u32 * m.cell_w) as i32,
                );
            }
        }
    }
}

/// Whether popup row `row` of `menu` is under the pointer or held down.
///
/// **A comparison against the session's own `hover` and `pressed`, and nothing else.** The painter
/// does not re-derive the hit: a painter that did would be a second hit test, which is precisely the
/// duplication `widgets.rs` was written to remove. **Hover and press draw the same row** because the
/// reference's highlighted menu item is the same surface either way -- and a menu you are holding open
/// over a row does not look different from a menu you are pointing at.
///
/// The two are separate `Hit` values only so the session can clear `pressed` on a release that
/// happened off the widget. **They render identically, which is why one function reads both.**
fn row_is_live(state: &ChromeState, menu: usize, row: usize) -> bool {
    let h = crate::widgets::Hit::MenuItem { menu, row };
    state.hover == Some(h) || state.pressed == Some(h)
}

/// The toolbar: a pill, then one button per [`TOOLBAR`](crate::widgets::TOOLBAR) entry.
///
/// **The pill is the whole width of the band minus its inset**, rather than being fitted to its
/// contents. It is what makes the toolbar read as one control surface rather than as a row of loose
/// icons, and fitting it would mean recomputing the pill's width every time a tool was added.
fn paint_toolbar(into: &mut Vec<SurfaceTree>, m: &ChromeMetrics, l: &Layout, state: &ChromeState) {
    // **No band fill, for the reason `paint_menubar`'s gives.** The first version of this emitter
    // pushed `fill(.., CHROME)` for the toolbar band -- the same colour as the panel -- so it painted
    // the panel's colour over the chrome's own toolbar band and the band became invisible. `Chrome::tree`
    // already fills that band; an emitter that re-fills its band has to know what colour it is, and
    // this one does not need to know anything. **The pill below is a different surface**: it is a
    // control, and it *is* this emitter's to draw.
    let pill = DamageRect::new(
        8,
        l.toolbar.y + 3,
        l.width.saturating_sub(16),
        l.toolbar.height.saturating_sub(6),
    );
    into.push(fill(
        pill.x as i32,
        pill.y as i32,
        pill.width,
        pill.height,
        colour::PILL,
    ));

    let mut prev_separated = false;
    for p in crate::widgets::place_toolbar(l, 8) {
        if p.tool.separated() && !prev_separated {
            // The separator is drawn on the tool's *left* edge, in the gap before it.
            let sx = p.rect.x as i32 - (crate::widgets::SEPARATOR / 2) as i32 - 1;
            into.push(fill(
                sx,
                p.rect.y as i32 + 4,
                1,
                p.rect.height.saturating_sub(8),
                colour::RULE,
            ));
        }
        prev_separated = p.tool.separated();

        // **Hover and press, drawn before the glyph.** The glyph goes on top of the state surface,
        // which is the point of the state surface -- it is behind the thing it is a state of. Drawing
        // it after would put a rectangle over the icon and make a hovered button look pressed-in.
        if let Some(c) = state_fill(state, crate::widgets::Hit::Tool(p.tool)) {
            into.push(fill(
                p.rect.x as i32,
                p.rect.y as i32,
                p.rect.width,
                p.rect.height,
                c,
            ));
        }

        let cx = p.rect.x as i32;
        let cy = p.rect.y as i32;
        let ih = crate::icons::SIZE as i32;
        let iy = cy + (p.rect.height as i32 - ih) / 2;

        // **The label is the outer branch, not the inner one.** The first version matched on `icon()`
        // and put the label inside the `Some` arm with `has_label` as a sub-condition, which sent the
        // three labelled controls -- the ones whose icon is `None` -- straight into the `None` arm and
        // its `unreachable!()`. **It compiled, and it panicked on the first paint**, because a `match`
        // that assumes two categories are really three has to put one of them somewhere it does not
        // belong. The order is now the order the reference draws in: icon, label, chevron.
        let ix = if p.tool.has_label() {
            cx + m.cell_w as i32
        } else {
            cx + (p.rect.width as i32 - ih) / 2
        };
        if let Some(icon) = p.tool.icon() {
            into.push(SurfaceTree::leaf(crate::Node::Icon(icon.at(
                ix,
                iy,
                colour::INK_CHROME,
            ))));
        }
        if p.tool.has_label() {
            push_text(
                into,
                m,
                p.rect.y + (p.rect.height.saturating_sub(m.cell_h)) / 2,
                &p.tool.label(state),
                Style::MONOSPACE,
                colour::INK_CHROME,
                ix + ih as i32 + 4,
            );
            // The chevron that says "this opens a popup".
            into.push(SurfaceTree::leaf(crate::Node::Icon(
                crate::icons::IconId::ChevronDown.at(
                    p.rect.right() as i32 - ih as i32 - 4,
                    iy,
                    colour::INK_DIM,
                ),
            )));
        }
    }
}

/// The sidebar: a back arrow, a heading, a "+", and one row per document.
fn paint_sidebar(into: &mut Vec<SurfaceTree>, m: &ChromeMetrics, l: &Layout, state: &ChromeState) {
    into.push(fill(
        0,
        l.sidebar.y as i32,
        l.sidebar.width,
        l.sidebar.height,
        colour::BAND,
    ));
    // The sidebar's right edge.
    into.push(SurfaceTree::leaf(crate::Node::Rect(crate::Rect::new(
        l.sidebar.right() as i32 - 1,
        l.sidebar.y as i32,
        1,
        l.sidebar.height,
        colour::RULE_DIM,
    ))));

    into.push(SurfaceTree::leaf(crate::Node::Icon(
        crate::icons::IconId::ArrowLeft.at(
            l.sidebar_back.x as i32 + 4,
            l.sidebar_back.y as i32 + 4,
            colour::INK_CHROME,
        ),
    )));
    push_text(
        into,
        m,
        l.sidebar_back.y,
        "Document tabs",
        Style::MONOSPACE,
        colour::INK_DIM,
        l.sidebar_back.right() as i32 + 8,
    );
    into.push(SurfaceTree::leaf(crate::Node::Icon(
        crate::icons::IconId::Plus.at(
            l.sidebar_new.x as i32 + 4,
            l.sidebar_new.y as i32 + 4,
            colour::INK_CHROME,
        ),
    )));

    for i in 0..l.sidebar_rows as usize {
        let (Some(title), Some(r)) = (state.docs.get(i), l.sidebar_doc(i)) else {
            break;
        };
        let active = i == state.active_doc;
        // **Active outranks hovered, for the same reason an open menu heading does.** The active row
        // is filled whether or not the pointer is on it; hovering *another* row shows that it is
        // there, which is the one piece of information a sidebar needs to offer.
        if active {
            into.push(fill(
                r.x as i32,
                r.y as i32,
                r.width,
                r.height,
                colour::PILL_ACTIVE,
            ));
        } else if let Some(c) = state_fill(state, crate::widgets::Hit::Doc(i)) {
            into.push(fill(r.x as i32, r.y as i32, r.width, r.height, c));
        }
        into.push(SurfaceTree::leaf(crate::Node::Icon(
            crate::icons::IconId::Doc.at(
                r.x as i32 + 8,
                r.y as i32 + (r.height as i32 - crate::icons::SIZE as i32) / 2,
                colour::INK_CHROME,
            ),
        )));
        let ty = r.y + (r.height.saturating_sub(m.cell_h)) / 2;
        push_text(
            into,
            m,
            ty,
            title,
            Style::MONOSPACE,
            colour::INK_CHROME,
            r.x as i32 + 8 + crate::icons::SIZE as i32 + 8,
        );
        if active {
            into.push(SurfaceTree::leaf(crate::Node::Icon(
                crate::icons::IconId::Overflow.at(
                    r.right() as i32 - crate::icons::SIZE as i32 - 8,
                    r.y as i32 + (r.height as i32 - crate::icons::SIZE as i32) / 2,
                    colour::INK_DIM,
                ),
            )));
        }
    }
}

fn fill(x: i32, y: i32, width: u32, height: u32, colour: u32) -> SurfaceTree {
    SurfaceTree::leaf(crate::Node::Rect(Rect::new(x, y, width, height, colour)))
}

/// A `TextRun` for one glyph at `x, y`.

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
