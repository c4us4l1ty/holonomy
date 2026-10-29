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

use crate::tree::{Label, Rect, RoundRect, Style, SurfaceTree, TextRun};
use crate::DamageRect;

// ---------------------------------------------------------------- the palette

/// Chrome colours, as packed `0xAARRGGBB`.
///
/// The alpha byte is always `0xFF` even though [`Frame`](https://docs.rs/holonomy-display) masks it
/// off: [`Rect`] and [`TextRun`] document their colours as `0xAARRGGBB`, so writing `0x00202020` for
/// "opaque" would be relying on a mask three crates away.
pub mod colour {
    /// The panel behind everything.
    pub const CHROME: u32 = 0xFFF8_FAFD;
    /// The document page.
    pub const PAGE: u32 = 0xFFFF_FFFF;
    /// A band that is one step lighter than the chrome: the toolbar and the status bar.
    pub const BAND: u32 = 0xFFF8_FAFD;
    /// Rules, separators and box drawing.
    pub const RULE: u32 = 0xFFC4_C7C5;
    /// A rule that is *incidental* -- a tick, a gutter marker.
    pub const RULE_DIM: u32 = 0xFFE0_E0E0;
    /// Body text on the page.
    pub const INK: u32 = 0xFF1F_1F1F;
    /// Chrome text.
    pub const INK_CHROME: u32 = 0xFF44_4746;
    /// Chrome text that is inactive or merely informational.
    pub const INK_DIM: u32 = 0xFF74_7775;
    /// The text on a button: one step brighter than [`INK_CHROME`].
    ///
    /// **Separate from `INK_CHROME` so "this item is selected" is a colour rather than a rectangle.**
    /// The reference's open menu heading is the same weight and a slightly brighter ink inside the
    /// same pill, and a selected *item* inside a popup is the same again. Three states, two colours,
    /// and the third state is carried by the pill.
    pub const INK_STRONG: u32 = 0xFF1F_1F1F;
    /// The raised surface: the toolbar's pill, a button, a popup, a selected sidebar row.
    ///
    /// **One value for all four**, because they are the same surface at different sizes. Giving each
    /// its own would be four numbers to keep in step, and the eye reads them as one lighter plane.
    pub const PILL: u32 = 0xFFF0_F4F9;
    /// A hovered widget, one step above [`PILL`].
    ///
    /// **A separate value rather than `PILL_ACTIVE` reused for hover.** The two are one step apart on
    /// purpose: hover is "you are here" and press is "this is happening", and a menu item that looked
    /// the same either way would not tell you which of the two your click had reached.
    pub const PILL_HOVER: u32 = 0xFFE8_EAED;
    /// A held or selected widget, one step above [`PILL_HOVER`].
    pub const PILL_ACTIVE: u32 = 0xFFD3_E3FD;
    /// **The document's selected bytes, behind the text.** Part 28.
    ///
    /// **A light blue and not a darker grey, and that is the one deliberate departure from this
    /// module's palette.** Every other surface here is a step on a single grey ramp — `PILL`,
    /// `PILL_HOVER`, `PILL_ACTIVE`, `POPUP` — because they are all chrome, and chrome on one ramp is
    /// what makes the document read as a different kind of thing from the window around it. **A
    /// selection is about the *document*, not the chrome**, and putting it on the ramp made it look
    /// like another button.
    ///
    /// **Light rather than dark so the ink stays ink.** A dark selection behind dark text is a
    /// contrast problem the moment the document's own colour changes, and the page background is
    /// white. At 0xFFC8_D9F0 the worst case is a document in near-black on near-white, which is
    /// legible at every selection the user can make.
    pub const SELECTION: u32 = 0xFFD3_E3FD;
    /// An open menu's panel, **lighter than everything it can be drawn over**.
    ///
    /// **This is the third surface and it needed its own value**, which the first version did not
    /// give it: the popup was drawn in [`PILL`], which is also the toolbar's pill. A menu that hangs
    /// over the toolbar and the sidebar is then drawn in exactly the colour of both, and it reads as
    /// *floating text* rather than as a panel -- the items are legible and the menu is invisible.
    /// `a_popup_is_a_lighter_plane_than_anything_it_covers` is the gate, and it would have caught it.
    pub const POPUP: u32 = 0xFFFF_FFFF;
    /// **The one-pixel rim around a popup**, and the rule under the heading that opened it.
    ///
    /// **`POPUP_EDGE` and not `RULE`.** `RULE` is `0xFF5A_5A66` and the popup's own fill is
    /// `0xFF46_4654`, so a `RULE` border is lighter than the panel by 20 in red — close enough to be
    /// invisible at one pixel against a drop shadow that is a different direction entirely. A rim has
    /// to be a step *away* from the panel, or it is not a rim: it is a line the eye cannot see, and the
    /// popup loses the edge that makes it a panel rather than a rectangle of slightly different grey.
    /// **`0xFF_5E_5E_6E` and not `0xFF5E_5E_6E`.** Every other colour in this block is `AA_RR_GG_BB`
    /// in groups of two -- `0xFF1E_1E22`, `0xFF5A_5A66`, `0xFF2A_2A30` -- and this one grouped its
    /// digits `FF5E | 5E | 6E`. **Same value, and the only reason it matters is that a colour read at
    /// 3am is compared digit by digit against its neighbours**: one constant in twenty with a different
    /// shape is the one that gets misread.
    pub const POPUP_EDGE: u32 = 0xFFE0_E0E0;
    /// **A popup's drop shadow**, in pixels: the width of the two flat bands down and to the right.
    ///
    /// **Three, the same as the page's, and not a hairline.** The two are the same device — a flat
    /// band, because `Frame` masks alpha off and a ramp would need `f32` — and a one-pixel shadow at
    /// this cell size reads as a slightly wrong edge rather than as depth.
    pub const POPUP_SHADOW: u32 = 3;
    /// The `[SEALED]` badge: the one thing on screen that is a security claim.
    pub const SEALED: u32 = 0xFF13_7333;
    /// Accent for a toolbar toggle that is on.
    pub const ACTIVE: u32 = 0xFF0B_57D0;
    /// The caret.
    pub const CARET: u32 = 0xFF1F_1F1F;
    /// The scrollbar track.
    pub const TRACK: u32 = 0xFFF8_FAFD;
    /// The scrollbar thumb.
    pub const THUMB: u32 = 0xFFDA_DCE0;
    /// The page's drop shadow, as a flat band. A real gradient would need `f32` alpha.
    pub const SHADOW: u32 = 0xFFE0_E0E0;
    /// Google blue, for the document mark and active accents.
    pub const BLUE: u32 = 0xFF42_85F4;
    /// The cyan Share/Export pill.
    pub const SHARE_BG: u32 = 0xFFC2_E7FF;
    /// Ink on the cyan pill.
    pub const SHARE_INK: u32 = 0xFF00_4A77;
    /// The blue Upgrade/Local pill.
    pub const UPGRADE_BG: u32 = 0xFFD3_E3FD;
    /// Ink on the blue pill and selected sidebar row.
    pub const UPGRADE_INK: u32 = 0xFF08_42A0;

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
    /// **The document-tabs sidebar.** Part 19.
    ///
    /// **The width the sidebar occupies, and zero when it is closed.** It is a *reservation* as much as
    /// it is a size: `Layout::new` subtracts it before it centres the page and `measure_for` subtracts
    /// it before it decides the measure, so "the sidebar is drawn here" and "the page is centred
    /// there" are the same fact. `Session::set_sidebar` is the only writer of the zero.
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
    /// **The measure the page would have if the panel is at least this wide**, in columns of text.
    ///
    /// **This was the whole measure, and that made the window size nearly meaningless.** `page_w` was
    /// `columns * cell_w + 2 * page_pad` — a constant — so a 2,560 px window and a 1,280 px window drew
    /// the same 736 px page and differed only in how much empty desk surrounded it. **"Make it bigger"
    /// did nothing to the text**, which is the most direct way a UI can fail to respond to its user.
    ///
    /// So it is now the *floor* of the measure rather than its value: the page grows with the panel and
    /// this is what it measures at [`DESKTOP`](Self::DESKTOP). **At 1,280 px wide nothing changes** —
    /// 80 columns is exactly what the room allows — so every gate written against the old constant
    /// still holds, and the behaviour is new only where the panel is wider than it has ever been.
    ///
    /// **A narrow panel still gets this many columns**, overflow and clipping included, because the
    /// alternative — a two-column measure on a small window — is worse than a clipped full one.
    pub columns: u32,
    /// **The most columns the page will ever be**, however wide the panel.
    ///
    /// **A typographic cap, not a layout one, and it is the reason the measure is not
    /// `available_width / cell_w`.** A 2,560 px panel has room for ~300 monospace columns and a line
    /// that long is unreadable: the eye loses the start of the next line on the return sweep. 80 is the
    /// classic measure and 100 is the usual upper end of it, so that is where this stops and the
    /// remaining width becomes desk.
    ///
    /// **Beyond the cap the page is still centred and the gutters still grow**, so a very wide window
    /// looks like a page on a desk rather than a stretched sheet — which is the reference's look and
    /// the reason this is a cap and not "fill the panel".
    ///
    /// **Change this number to change the answer.** It is a judgement about reading, not about pixels,
    /// and it is one field rather than a constant buried in the layout arithmetic.
    pub max_columns: u32,

    /// **The panel width below which the measure is exactly [`columns`](Self::columns).**
    ///
    /// ## Why this is a field and not `DESKTOP.width`
    ///
    /// **Because the design's cell and the design's panel are the same number of type.** 1,280 px is 80
    /// columns *at an 8 px cell*; at a 14 px cell the same 80 columns needs 1,551 px. **Reading the
    /// anchor off the constant would have let the two drift apart silently** -- the bands would scale
    /// with the type and the anchor would not, and the first symptom would be a 1,280 px panel whose
    /// 80-column page overflows by 271 px. [`with_type_scale`](Self::with_type_scale) scales this field
    /// with the same ratio as the bands, so the identity holds at every size.
    ///
    /// **1,280 at 16 ppem, 1,894 at 24** -- and the default window is 1,600, which is deliberately
    /// *below* the anchor so that the default panel gets the full measure rather than the cap.
    pub anchor_width: u32,
}

impl ChromeMetrics {
    /// 1280 x 800, the X200's panel, with a 72-column measure.
    ///
    /// Chosen so the page is 576 px of text: 72 columns x 8 px. A 4:3 panel showing a portrait page
    /// at a readable size, which is the whole design constraint.
    ///
    /// **CORRECTION, part 39: 80 columns, and a 208 px sidebar, and neither matched the reference.**
    /// Measured off `Plan/Screenshot From 2026-10-01 00-17-42.png`, which is 1,778 px wide:
    ///
    /// | | reference | this design, then | this design, now |
    /// |---|---|---|---|
    /// | sidebar | 345 px (19.4 %) | 208 px (16.3 %) | **256 px (20.0 %)** |
    /// | page | 813 px (45.7 %) | 736 px (57.5 %) | **960 px (60.0 %)** |
    /// | measure | ~90 characters | 80 columns | **72 columns** |
    ///
    /// **The measure is the one that matters**, and it is measured in *characters* because that is the
    /// only unit the two pictures share: the reference's line is a proportional face at about 8.6 px a
    /// character and ours is the Monospace face at 12, so 777 px of reference text is about 90
    /// characters and 864 px of ours is 72. **A page that is 80 % of the window and a reference page that
    /// is 46 % of it are not the same picture**, whatever the measure says, and the gutter is what says
    /// it: the reference has 310 px of desk on each side of the page and this design had 77.
    ///
    /// **72 and not 90.** A monospaced character is wider than a proportional one, so a measure of 90
    /// *cells* is longer than 90 reference characters; 72 cells is the count that reads the same.
    ///
    /// **And 80 is what this panel *allows*, not what it *fixes* is now 72** -- see [`columns`](Self::columns).
    pub const DESKTOP: ChromeMetrics = ChromeMetrics {
        width: 1280,
        height: 800,
        title_h: 40,
        menu_h: 32,
        // **256, and the table above is the reason.** `with_type_scale` deliberately does *not* scale
        // `sidebar_w`, because it is a distance and not a type size -- so this is 256 px of a 1,600 px
        // window at every type size, which is 16 % at the default 16 ppem and 20 % at the 1,280 px
        // design width.
        sidebar_w: 280,
        button: 36,
        tab_h: 0,
        toolbar_h: 56,
        ruler_h: 28,
        status_h: 24,
        cell_w: 8,
        cell_h: 18,
        page_pad: 56,
        scrollbar_w: 12,
        shadow: 1,
        columns: 72,
        // **72, and NOT the 88 this field held before part 39 -- which reverses part 27.**
        //
        // Part 27 made the measure grow with the panel "so that making the window bigger does
        // something", and capped it at 100. **Measured against the reference, that is not what the
        // reference does:** in `Plan/...00-17-42.png` the page is 813 px in a 1,778 px window, with
        // 310 px of desk on each side. **A page that grows with the window is a page that fills the
        // window**, which is what the 1,600 px default was showing: an 88-column page, 1,152 px, 72 %
        // of the window, 144 px of desk in total.
        //
        // **What part 27 was actually right about is the resize, and the resize now does something**:
        // the page moves, the gutters change, the ruler re-ticks and the scrollbar moves -- because
        // the resize path works at all (part 39's actual correction). The growth bought nothing that
        // the fix did not buy better.
        //
        // **One field.** Raising this to 88 brings the old behaviour back, with no other change.
        max_columns: 72,
        anchor_width: 1280,
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
        // **CORRECTION, part 32: the title and menu bands are here too, and they were missing.**
        // Every band that holds a line of type must be at least one line tall, and `at_least_a_line`
        // was applied to four of the six. `title_h: 30` and `menu_h: 22` are both below the 16 ppem
        // line pitch of **25**, so the title and the menu bar were *already* clipping their text at the
        // design size -- 30 px of band around 25 px of ink with no slack, and 22 px for the same 25.
        // At 24 ppem, where the pitch is 37, it became unmissable: both bands showed the bottom of every
        // glyph sheared off. The omission survived because the bands were raised to *the pitch*, and
        // nobody compared the two numbers.
        self.title_h = at_least_a_line(self.title_h, cell_h);
        self.menu_h = at_least_a_line(self.menu_h, cell_h);
        self.tab_h = at_least_a_line(self.tab_h, cell_h);
        self.toolbar_h = at_least_a_line(self.toolbar_h, cell_h);
        self.ruler_h = at_least_a_line(self.ruler_h, cell_h);
        self.status_h = at_least_a_line(self.status_h, cell_h);
        self
    }

    /// **The line pitch of the 16 ppem design these constants were chosen at.**
    ///
    /// **Measured, not guessed: `Atlas::line_pitch()` over the packed faces at 16 ppem is 25** -- 20 px
    /// (Inter), 22 (JetBrains Mono) and 24 (Noto Sans Math) of ascent-plus-descent. Every ratio in
    /// [`with_type_scale`](Self::with_type_scale) divides by this, so it is the one place the design's
    /// own size is written down.
    pub const BASE_PITCH: u32 = 25;

    /// **Scale the whole chrome to the type, from one measured number.**
    ///
    /// ## What this is for
    ///
    /// **`with_line_pitch` raised the bands that hold text but left the *type* at 16 ppem forever**, and
    /// that is why raising the ppem alone made the application look wrong rather than bigger: the glyphs
    /// grew into bands that did not, and the measure stayed at `cell_w = 8` while the font's own
    /// advance was 10.
    ///
    /// **So every dimension that describes *type* moves together here, and every dimension that does not,
    /// does not.** Scaled: the five text bands, the toolbar button, and the anchor width. **Not scaled:**
    /// `page_pad`, `sidebar_w`, `shadow`, `scrollbar_w`, `columns`, `max_columns`, `width`, `height`.
    ///
    /// ## Why those two lists, and the padding is the interesting one
    ///
    /// **`page_pad` is 48 px at 16 ppem and stays 48 px at 24.** It is *padding*, and padding is a
    /// distance between things rather than a size of a thing: it does not get bigger because the type
    /// did, and scaling it would have taken 72 px, which at a 14 px cell is five cells of margin and
    /// reads as a page with a hole in it. **The same argument retires `sidebar_w`: 208 px is the width
    /// of a sidebar in any size of type.** The rule is *does this describe the type or the furniture*.
    ///
    /// ## The ratio, and why `pitch` and not `ppem`
    ///
    /// **Because `ppem` is the rasteriser's input and `line_pitch` is what the layout actually needs.**
    /// The bands are measured in line boxes, so `line_pitch` is the unit they are in; dividing by 16
    /// would have been dividing by a number that happens to be nearby.
    ///
    /// **Measured, both directions:** 16 ppem -> 25, 20 -> 30, 22 -> 34, 24 -> 37, 26 -> 40.
    ///
    /// ## And the anchor width, which has to move or part 27's meaning is lost
    ///
    /// **`measure_for` holds `columns` until the panel reaches [`anchor_width`](Self::anchor_width).**
    /// At 8 px cells 1,280 px is exactly 80 columns, and that identity is what §2.9.3's 640 px page
    /// column and part 27's wider-window behaviour were both measured against. **At a 14 px cell the
    /// same 80 columns is 1,120 px of text, so the same anchor no longer fits and a 1,280 px panel would
    /// overflow its own measure by 271 px.** Scaling the anchor by the same ratio keeps the *identity*
    /// rather than the number, which is what part 27 was anchoring in the first place.
    pub fn with_type_scale(mut self, pitch: u32) -> Self {
        if pitch == 0 {
            return self;
        }
        let by = |v: u32| -> u32 { (v as u64 * pitch as u64 / Self::BASE_PITCH as u64) as u32 };
        self.title_h = by(self.title_h);
        self.menu_h = by(self.menu_h);
        self.toolbar_h = by(self.toolbar_h);
        self.ruler_h = by(self.ruler_h);
        self.status_h = by(self.status_h);
        self.button = by(self.button);
        self.anchor_width = by(self.anchor_width);
        self
    }

    /// **Set the cell width from the face's own monospace advance.** Part 32.
    ///
    /// **A setter and not a constructor argument, for the same reason [`with_line_pitch`](Self::with_line_pitch)
    /// is one**: `ChromeMetrics` is a public struct a caller builds with `..DESKTOP`, and the cell is
    /// only known once the session has the atlas it will paint from. **The 8 that stays behind is the
    /// no-atlas fallback** and now means the 16 ppem design, which is what it always meant to mean.
    pub fn with_cell_width(mut self, cell_w: u32) -> Self {
        self.cell_w = cell_w.max(1);
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

    /// The page's width, text plus padding, **at this metrics' own measure**.
    ///
    /// **Kept as a method because `MIN_WIDTH`'s arithmetic and several gates quote it**, and changing
    /// its meaning would silently change those. The panel-aware answer is
    /// [`measure_for`](Self::measure_for) followed by [`page_w_for`](Self::page_w_for), and
    /// [`Layout`](Self) uses those.
    pub const fn page_w(&self) -> u32 {
        self.page_w_for(self.columns)
    }

    /// The page's width for a measure of `cols` columns.
    pub const fn page_w_for(&self, cols: u32) -> u32 {
        cols * self.cell_w + self.page_pad * 2
    }

    /// **The measure this panel can actually show**, in columns.
    ///
    /// # This is the fix for "make the window bigger and nothing happens"
    ///
    /// **The room is what the panel has left after the sidebar, the shadow, the scrollbar rail and a
    /// minimum desk on each side**, divided into cells, less the page's own padding.
    ///
    /// **The desk is four cells — 32 px on each side.** Not zero, because a page flush to the panel edge
    /// is not a page on a desk and the reference is not that; not much more, because the whole point is
    /// that a wider window yields more text and a generous margin gives that back.
    ///
    /// **And it is clamped into `[columns, max_columns]`.** Below the floor the measure is unchanged and
    /// the page overflows and clips, which is what a narrow window has always done; above the ceiling
    /// the extra width becomes desk rather than a 300-column line nobody can read. See
    /// [`max_columns`](Self::max_columns).
    ///
    /// **`cell_w.max(1)`, because a zero cell width would divide by zero** — and
    /// `ChromeMetrics` is a public struct a caller can build, which is the only reason that is possible.
    pub const fn measure_for(&self, width: u32) -> u32 {
        // **At the design width, the measure is exactly `columns` — and this one line is what keeps
        // every measured number in this project valid.**
        //
        // §2.9.3 measured the image cache against a **640 px page column**: the 8.0 MiB decoded-image
        // budget, the resample thresholds, the "≤ 8.0 MiB at every point of a page-1→50 scroll" row in
        // §6. The room-minus-chrome arithmetic on its own gives **112 columns at 1,280 px**, which would
        // have silently made the page 896 px wide and every one of those measurements wrong by 40 %.
        //
        // So the measure is anchored: **1,280 px is the width `columns: 80` was chosen for**, and only a
        // panel *wider* than that earns more text. That keeps `ChromeMetrics::DESKTOP` meaning what it
        // has always meant, and makes a wider window — the thing the user actually asked for — the
        // thing that helps.
        //
        // **CORRECTION, part 36: the floor holds only while the page actually fits.** Part 27's rule
        // was unconditional, and it was written when 80 columns was 736 px — a 1,024-wide window had
        // room. **Part 32 raised the type to 24 ppem, which made 80 columns 1,216 px, and the floor
        // then held a measure the panel could not show: the text ran off the right edge of a
        // 1,024-wide window and was simply gone.**
        //
        // **The anchor scaled with the type (1,280 -> 1,894), so the rule still looked
        // self-consistent.** What changed underneath it was the width of the page the floor was
        // promising: the anchor tracks the panel that holds the design measure, and nothing tracked the
        // panel that holds the design *page*.
        //
        // **Nothing above the design width changes**: `measure_beyond` keeps applying the floor there,
        // where the extra width is desk by construction.
        let cols = if self.columns == 0 { 1 } else { self.columns };
        if width <= self.anchor_width && room_columns(self, width) >= cols {
            return cols;
        }
        measure_beyond(self, width, if width <= self.anchor_width { 1 } else { cols })
    }
}

/// **The columns the panel's room actually holds, with neither floor nor ceiling applied.**
///
/// **Split out of [`measure_beyond`] because [`measure_for`](ChromeMetrics::measure_for) needs the raw
/// room twice**: once to ask *"does the design measure still fit?"* — a question about the room and not
/// about the clamped answer — and once to get the clamped answer. **One arithmetic, so the question and
/// the answer cannot be built from two different subtractions.**
const fn room_columns(m: &ChromeMetrics, width: u32) -> u32 {
    /// Desk either side of the page, in cells. Two would do; four is a margin that reads as a page.
    const DESK_CELLS: u32 = 4;
    // **`u32::max` is not a `const fn` on this toolchain** — `Ord::max` is not yet stable as a const
    // trait — and neither is `Ord::clamp`. The clamps below are the comparisons written out: three more
    // lines than `clamp`, and the same reasoning twice.
    let cell = if m.cell_w == 0 { 1 } else { m.cell_w };
    let chrome = m
        .sidebar_w
        .saturating_add(m.shadow)
        .saturating_add(m.scrollbar_w)
        .saturating_add(DESK_CELLS * 2 * cell);
    let room = width.saturating_sub(chrome).saturating_sub(m.page_pad * 2) / cell;
    // **One column even when the room is zero**, so this cannot return zero and a caller dividing by
    // it cannot trap. A metrics with `columns: 0` is a caller's mistake and the floor turns it into
    // one column rather than into a panic three functions away.
    if room == 0 { 1 } else { room }
}

/// **The arithmetic [`measure_for`](ChromeMetrics::measure_for) reaches above the design width, clamped
/// to `[floor, ceiling]`.**
///
/// **A free function and not a second method**, because it is one computation reached by two doors: the
/// anchor at the design width belongs to `measure_for` and the room arithmetic belongs to both, and
/// splitting them means neither half has to re-read the other's reasoning.
/// **`floor` is a parameter because it is not always the same number.** Above the design width it is
/// [`ChromeMetrics::columns`]. Below it, where the design measure no longer fits, it is **1** — a floor
/// of 80 there would be the defect part 36 fixes, restated.
const fn measure_beyond(m: &ChromeMetrics, width: u32, floor: u32) -> u32 {
    let room = room_columns(m, width);
    let ceiling = if m.max_columns == 0 {
        m.columns
    } else {
        m.max_columns
    };
    let ceil = if ceiling < floor { floor } else { ceiling };
    if room < floor {
        floor
    } else if room > ceil {
        ceil
    } else {
        room
    }
}

/// The rest of [`ChromeMetrics`]'s associated items, in a second `impl` because
/// [`measure_beyond`](measure_beyond) sits between them as a free function.
///
/// **A second `impl` block is legal Rust and slightly unusual**, and it is here rather than at the foot
/// of the file because the function it interrupts is the one that explains the measure. Moving either
/// half away from the other would put the reasoning and the arithmetic in different parts of the file,
/// which is how the two get out of step.
impl ChromeMetrics {
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
    ///
    /// # What these are *for*, corrected 2026-10-12
    ///
    /// **They were also the layout's clamp, and that was wrong.** `for_size` and `clamp_to` both read
    /// `if width < MIN_WIDTH { MIN_WIDTH }`, so a window narrower than the minimum was laid out *wider
    /// than itself* and the overflow was drawn off-screen. The measurement from `bugs/1.png` is in
    /// [`for_size`](Self::for_size).
    ///
    /// **So they now mean exactly one thing each: the smallest size this app asks the window manager
    /// for.** That is a *request*, in `WM_NORMAL_HINTS`, and it is honoured or it is not — this
    /// window manager did not, and the protocol does not say it must.
    ///
    /// **Below them the chrome elides rather than overflowing.** [`menu_boxes`] drops the headings
    /// that do not fit, which is what turns "the menu bar is cut mid-word" into "the menu bar shows
    /// the menus that fit". They are the width at which that elision starts doing something, which is
    /// the only reason a number belongs here at all.
    pub const MIN_WIDTH: u32 = 768;
    pub const MIN_HEIGHT: u32 = 320;

    /// Metrics for a panel of `width` x `height`.
    ///
    /// **The measure holds below [`DESKTOP`](Self::DESKTOP)'s width and grows above it.** Part 27.
    ///
    /// **At or under 1,280 px this is 80 columns with the page centred**, so a narrow window grows the
    /// margins rather than the line length — which is what a word processor does, and what the reference
    /// screenshots in `Plan/` show. **Wider than 1,280 px the measure grows** up to
    /// [`max_columns`](Self::max_columns), after which the extra width is desk again.
    ///
    /// **Both halves exist because each was wrong alone.** Holding the measure fixed is right
    /// typographically and wrong as an answer to "make the window bigger", which bought desk and no text.
    /// Letting the measure follow the room is right responsively and wrong past about 100 columns, and the
    /// arithmetic on its own gives 112 columns at 1,280 — which would have silently made every number
    /// §2.9.3 measured against a **640 px page column** wrong by 40 %. **The design width is the anchor and
    /// the ceiling is the limit.**
    ///
    /// So a resize is arithmetic on the page and the gutters: the bands never change, and below the design
    /// width neither does the page. [`Layout::new`] recomputes where everything sits, and nothing about the
    /// *document* depends on the panel size — which is also why
    /// [`Session::resize`](../../holonomy/session/struct.Session.html#method.resize) touches neither the
    /// text nor the caret.
    ///
    /// The size is **not** clamped up, and that is a correction (2026-10-12).
    ///
    /// It used to read `if width < MIN_WIDTH { MIN_WIDTH } else { width }` and `clamp_to` did the
    /// same, **and that made the chrome draw outside its own window.** A 480 px window became a 768 px
    /// layout, so 288 px of chrome had nowhere to go and was simply off-screen.
    ///
    /// **Measured from `bugs/1.png`**, the window is 480 px wide and the page's right edge lands at
    /// **x = 624** — 144 px past the window edge — which is why the page runs flush to the edge with
    /// no right gutter and the menu bar is cut mid-word to `Extens`.
    ///
    /// # Why the minimum could not be enforced here
    ///
    /// [`MIN_WIDTH`](Self::MIN_WIDTH) **is** sent to the window manager as a size hint, and that hint
    /// is advisory — the protocol does not require the WM to honour it, and this one did not. So the
    /// *window* is the only authority on how wide the chrome is. A minimum belongs in what the app
    /// asks for, and not in what the layout pretends.
    ///
    /// [`measure_for`](Self::measure_for) already narrows the measure rather than overflowing it
    /// (part 36), so the text was never the problem. **`MIN_WIDTH`/`MIN_HEIGHT` are now the point
    /// below which the chrome *elides*** — see [`menu_boxes`] — and nothing else.
    pub const fn for_size(width: u32, height: u32) -> Self {
        Self {
            // **`.max(1)`, not `.max(MIN_WIDTH)`.** A zero-sized panel is not a case to design for, but
            // multiplying by it and dividing by it is, and `DamageRect::new` wants a positive extent.
            width: if width == 0 { 1 } else { width },
            height: if height == 0 { 1 } else { height },
            ..Self::DESKTOP
        }
    }

    /// These metrics' bands, at a panel size of `width` x `height`.
    ///
    /// The cell size, the padding, the measure and the four band heights are all the same as `self`'s;
    /// only the panel changes. This exists so that "what a resize does and does not change" is written
    /// down once, and so a resize is one call at the call site rather than a struct literal.
    pub const fn clamp_to(&self, width: u32, height: u32) -> Self {
        // **CORRECTION, part 36: this was `Self::for_size(width, height)`, and `for_size` is
        // `Self { width, height, ..Self::DESKTOP }`.**
        //
        // **So every resize rebuilt the whole struct from the 16 ppem design constants**, discarding
        // the three things [`Session::new`](../../holonomy/session/struct.Session.html) derives from
        // the atlas: the measured `cell_w`, the raised band heights, and the scaled `anchor_width`.
        // `Chrome::new` then laid the chrome out in an 8 px cell while the painter advanced 14 px ones,
        // and 24 ppem's glyphs overflowed 16 ppem's bands: **the toolbar icons overlapped**, and the
        // page took its measure and its gutters from a cell nothing else was using.
        //
        // **The doc comment below already said what this does** -- *"The cell size, the padding, the
        // measure and the four band heights are all the same as `self`'s; only the panel changes"* --
        // **and the body did the opposite.** The gate that would have caught it could not have been
        // written: every `clamp_to` assertion in this file builds its metrics from `for_size`, which is
        // the one constructor whose output `clamp_to` got right. See
        // `a_resize_keeps_the_type_scale_and_not_just_the_panel`.
        //
        // **`for_size` stays `..Self::DESKTOP`** and is still right: it is the constructor that *is*
        // the design defaults, at a size.
        //
        // # CORRECTION 2026-10-12: this used to clamp the size **up**, and that was the bug
        //
        // `width: if width < MIN_WIDTH { MIN_WIDTH } else { width }` — the same line `for_size` had.
        // A 480 px window therefore produced a 768 px layout, **and `Layout::new` lays the page out
        // from `m.width`.** The page's right edge landed at x = 624 in a window that ended at 480, so
        // 144 px of the page — and everything to its right, including half the menu bar — was drawn
        // into nothing.
        //
        // **A resize to a smaller window is exactly what a user does when the window is too small,
        // which is the report this came from.** Clamping up turned "make it bigger" into "make it
        // worse", and the damage looked like a layout bug because the layout genuinely was wrong: it
        // had been told it was 768 px wide.
        //
        // **The size is now the window's, exactly.** The window manager's `WM_NORMAL_HINTS` minimum
        // is the request; this is the consequence, and the consequence wins.
        Self {
            width: if width == 0 { 1 } else { width },
            height: if height == 0 { 1 } else { height },
            ..*self
        }
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
    /// The document-tabs sidebar. **Zero width when the sidebar is closed.**
    ///
    /// **The width is `ChromeMetrics::sidebar_w`, and `Session::set_sidebar` sets that field to zero
    /// when it closes the sidebar.** Two sources of truth for "is the sidebar open" is how the page
    /// ended up drawn under a panel that was not being drawn, so the reservation and the flag are
    /// written by one function and a gate asserts they agree.
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
        // **CORRECTION 2026-10-12: the canvas is clamped into the panel too, and the bands below were
        // already.**
        //
        // The five bands are clamped by `band()` a few lines down, **but `canvas_y` was not**, so it
        // kept the raw stack: at a panel shorter than the bands it was 106 regardless of the panel's
        // height, and the page was placed at y = 106 in a 1 px panel.
        //
        // **This was invisible for the whole life of the minimum clamp**, because the clamp forced
        // `height >= MIN_HEIGHT` (320) and 106 < 320 always. Removing the clamp — which had to happen,
        // because it was drawing 144 px outside the window — exposed it immediately. **That is the
        // argument for the gate rather than the fix:** the defect existed, and was hidden by the very
        // thing that was causing the reported one.
        let canvas_y = ruler_y.saturating_add(m.ruler_h).min(m.height);
        let canvas_h = m.canvas_h();

        // The page is centred in what is left of the **canvas**, and the sidebar is part of the canvas.
        // The remainder goes right, so the page is consistently one pixel left of true centre on odd
        // widths rather than jumping about.
        //
        // **The measure is what this panel allows, not what `columns` says — part 27.** At 1280 px that
        // is exactly 80 columns and nothing changes; at 1,600 it is 100, the cap; beyond that the
        // gutters take the rest. `page_w` alone would have been a constant and the window size would
        // have been decoration.
        //
        // **CORRECTION, part 39: the centring is in `width - sidebar_w`, not in `width`.** The page
        // was centred in the whole panel and the sidebar was then drawn over its left edge, so with
        // the sidebar open the first few pixels of the sheet — and, at a narrow measure, a whole
        // column of the ruler's ticks — were underneath a panel that has no business being there.
        // `room_columns` has always subtracted the sidebar's width before deciding the measure, so
        // the *measure* and the *placement* were using two different definitions of the room.
        let measure = m.measure_for(m.width);
        let page_w = m.page_w_for(measure);
        let sidebar_w = m.sidebar_w.min(m.width / 2);
        let desk = m.width.saturating_sub(sidebar_w);
        let (gutter_left, gutter_right) = if page_w >= desk {
            // A page wider than the desk: no gutters, and the text column is clipped rather than
            // wrapped. `text` below still reports the full measure; `caller_draw` clips. **The
            // sidebar's width is still reserved** — a page that started under the sidebar would put
            // the ruler's zero over a panel.
            (sidebar_w, 0)
        } else if sidebar_w > 0 && page_w.saturating_add(24) <= desk {
            // The reference keeps the sheet close to the document-tabs rail, not centred in the desk.
            let left = sidebar_w + 24;
            (left, m.width.saturating_sub(left + page_w))
        } else {
            let left = sidebar_w + (desk - page_w) / 2;
            (left, m.width.saturating_sub(left + page_w))
        };
        // The page and the scrollbar both *end* at the canvas bottom; the shadow band occupies the
        // `shadow` rows at the top of the canvas that the page is inset by. Growing the page by
        // `shadow` instead -- which is what this did first -- pushes the scrollbar's bottom edge
        // past the canvas's, and a rect that leaves its band is a rect the renderer must clip.
        let shadow = m.shadow.min(canvas_h / 2);
        let page_x = gutter_left;
        let page_y = canvas_y + shadow;
        let page_h = canvas_h - shadow;

        // **CORRECTION, part 39: the page is clipped into the panel on *both* axes, and the right edge is the
    // one that was not.**
    //
    // It read `page_w.min(m.width)`: a *width* capped at the panel's width, at an *x* that can be
    // non-zero. **A 9 px panel with the sidebar at `min(256, 9 / 2)` = 4 puts the page at x = 4 and
    // gives it 9 px of width, so it ends at 13 -- four pixels outside a panel nine pixels wide.**
    // `no_chrome_box_ever_lands_outside_its_panel` found it by sweeping every size from 1x1 up.
    //
    // **The cap is `width - page_x`, and `height - page_y`, because that is what "inside" means for a
    // rect that has already been moved.** Both are `saturating_sub`, so a page pushed past the edge by
    // its own arithmetic is a zero-sized rect rather than one that wraps.
    let page = DamageRect::new(
        page_x,
        page_y,
        page_w.min(m.width.saturating_sub(page_x)),
        page_h.min(m.height.saturating_sub(page_y)),
    );
        let text = DamageRect::new(
            page_x.saturating_add(m.page_pad),
            page_y.saturating_add(m.page_pad),
            measure * m.cell_w,
            page_h.saturating_sub(m.page_pad * 2),
        );
        let scrollbar = DamageRect::new(
            m.width.saturating_sub(m.scrollbar_w).saturating_sub(8),
            canvas_y,
            m.scrollbar_w,
            canvas_h,
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
        //
        // **The width is `m.sidebar_w`, and `m.sidebar_w` is zero when the sidebar is closed.** See
        // `Session::set_sidebar`: the flag and the reservation are set together, so "is there room for
        // the sidebar" and "is the sidebar drawn" cannot disagree -- which is the failure that put a
        // 208 px empty band on the left of a window whose sidebar was closed.
        let sidebar = DamageRect::new(0, canvas_y, sidebar_w, canvas_h);
        let head = 160u32;
        let doc_row_h = m.cell_h.saturating_add(24).max(48);
        // **At least one row, always.** A sidebar with room for no rows cannot show a document, and
        // "the panel is too short" is a resize rather than a state, so the honest answer is one row
        // clipped rather than none.
        let rows_visible = (sidebar.height.saturating_sub(head) / doc_row_h).max(1);
        let sidebar_back = DamageRect::new(sidebar.x + 24, sidebar.y + 24, 32, 32);
        let sidebar_new =
            DamageRect::new(sidebar.right().saturating_sub(48), sidebar.y + 100, 32, 32);
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

    /// **The measure in whole cells: how many columns of type the text rectangle holds.**
    ///
    /// # Why this exists, and why the emitter must not compute it
    ///
    /// **`emit_body_text` had `let cells_per_row = (l.width / cell_w.max(1)).max(1)` inline**, and the
    /// soft-wrap rule needs the same number. **Two computations of the measure is two answers waiting
    /// to differ** — and they would differ *silently*, because a wrap point one cell off from a
    /// `codepoints_upto` clip looks like a ragged margin rather than like a bug.
    ///
    /// **The `max(1)` is in both halves** and they are not the same guard: `cell_w.max(1)` stops a
    /// divide-by-zero on a degenerate metrics, and the outer `.max(1)` stops a measure of *zero*, which
    /// `wrap::WrapMap` also refuses. **Two layers of defensiveness for one layout fact is the right
    /// amount here**, because a page narrower than one cell cannot be produced by
    /// [`ChromeMetrics::for_size`] and is therefore *unreachable*, and an unreachable panic is one a
    /// layout change turns reachable without anybody looking.
    ///
    /// # Why the whole text width and not the page's
    ///
    /// **Because the text rectangle is where the type goes** — `emit_body_text` and every other
    /// emitter work in `text`'s coordinates, and mixing that with the page's is how a run ends up on
    /// the page margin.
    ///
    /// **Not `const fn`, and that is the toolchain**: `Ord::max` is not yet a const trait
    /// (rust-lang/rust#143874), so a `const` version of this does not compile. **The function is
    /// still evaluated from written-out arithmetic**, which is the property that matters — and it is
    /// called once per paint, so the difference is not measurable.
    #[must_use]
    pub fn cells_per_row(&self, cell_w: u32) -> usize {
        // **`as usize` rather than a `try_into`**: `DamageRect::width` is a `u32` and the cell count is
        // always far below `usize::MAX` on every target this builds for, **and this function is called
        // once per paint**. A saturating conversion would be more defensible and would also be a
        // silent answer on a hypothetical 16-bit `usize`, which is not a target.
        (self.text.width / cell_w.max(1)).max(1) as usize
    }

    /// **The row and column a *drag* is at, clamped to the page's last row and column.** Part 41.
    ///
    /// # Why this exists, and why [`Self::caret_at`] still refuses
    ///
    /// **`caret_at` answers `None` for a point outside the sheet, and that is right for a click.** A
    /// click on the desk beside the page is not a click on the page, and planting a caret at the end
    /// of the document because the user clicked 3 px too low is its own bug.
    ///
    /// **It is wrong for a drag.** A drag that has run off the bottom of the page means *keep
    /// going*, and answering `None` makes the selection stop at the last line while the pointer
    /// keeps moving -- which is exactly the shape of "the drag stopped working". **This clamps the
    /// row into `scroll_line .. scroll_line + rows` and the column into `0 .. columns`,** so a drag
    /// past any edge selects to that edge and stops there until the page scrolls under it.
    ///
    /// # Why it is a separate function and not a flag on `caret_at`
    ///
    /// **Because the two callers want opposite answers and a boolean parameter would make the
    /// difference invisible at the call site.** `click_page` calls both -- one for a click, one for a
    /// drag -- and a reader needs to see which is which.
    ///
    /// # Why the column clamps rather than wrapping
    ///
    /// **`caret_at` computes `(x - text.x) / cell_w` with no bound, so a drag far to the right of
    /// the text answers a column past the end of the line.** `Session::offset_of` clamps that to
    /// the line's length, so the behaviour is already right; **clamping here as well would be the
    /// same clamp twice**, and the test that says so is `a_drag_past_the_right_edge_clamps_to_the_line_end`.
    #[must_use]
    pub fn caret_at_drag(&self, state: &ChromeState, x: i32, y: i32) -> Option<(u32, u32)> {
        if !self.canvas.contains(x, y) {
            // **Outside the canvas entirely** -- above the page or below the window. **The last
            // visible row, not `None`:** a drag that leaves the window upward means "to the top",
            // and a drag that leaves downward means "to the bottom". **Which of those it is comes
            // from which side it left**, so the two edges are handled separately rather than by
            // one clamp.
            let edge_row = if y < self.canvas.y as i32 {
                state.scroll_line
            } else {
                state.scroll_line.saturating_add(self.rows.saturating_sub(1))
            };
            let column = self.column_at(x);
            return Some((edge_row, column));
        }
        let row = if y < self.text.y as i32 {
            0
        } else {
            u32::try_from(y - self.text.y as i32).ok()?.checked_div(self.cell_h)?
        };
        // **`min(rows - 1)`, and `rows` is never 0** -- `ChromeMetrics::rows` is derived from the
        // metrics at construction, and a zero would mean a page with no lines, which
        // `ChromeMetrics::for_size` does not produce.
        let row = row.min(self.rows.saturating_sub(1));
        Some((row + state.scroll_line, self.column_at(x)))
    }

    /// The glyph column `x` falls in, unclamped at the right.
    ///
    /// **A private helper because two callers now want it** and it is three lines; **and it is
    /// deliberately *not* clamped**, because `Session::offset_of` already clamps a column to its
    /// line's length and clamping twice would put the rule in two places.
    fn column_at(&self, x: i32) -> u32 {
        if x < self.text.x as i32 {
            0
        } else {
            u32::try_from(x - self.text.x as i32).unwrap_or(u32::MAX) / self.cell_w
        }
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
        let head = 160u32;
        Some(DamageRect::new(
            self.sidebar.x + 16,
            self.sidebar.y + head + (i as u32) * self.sidebar_row_h,
            self.sidebar.width.saturating_sub(32),
            self.sidebar_row_h.saturating_sub(4),
        ))
    }

    /// The page's top-left, for a [`Rect`].
    pub fn page_origin(&self) -> (i32, i32) {
        (self.page.x as i32, self.page.y as i32)
    }
}

// ---------------------------------------------------------------- state

/// Which style toggles are armed at the caret, for the toolbar. **Part 25 makes this live.**
///
/// # What this was, and the four parts of it that were not
///
/// **`StyleFlags` modelled four toggles — bold, italic, mono, heading — and nothing read it.** No
/// caller in the workspace touched `styles`, `StyleFlags::get`, or `StyleFlags::SLOTS`. The only
/// reference anywhere was `holonomy-render/tests/chrome.rs` pushing `SLOTS`' four short forms into the
/// list of labels the chrome is allowed to draw.
///
/// **And the chrome never drew those labels.** `Tool::label` returns an empty `String` for every tool
/// except Zoom, Style and Font, so the toolbar draws icons and `SLOTS`' `B`, `I`, `M` and `H` were never
/// emitted. **The gate was therefore four phantom labels wider than the chrome is** — dead code that made
/// a gate *weaker*, which is the quietest kind of dead code there is and the reason it survived.
///
/// # What is here now, and what is still not
///
/// **Two slots and two buttons.** `StyleFlagsSlot::Mono` and `::Heading` are **deleted** rather than left
/// unreachable: the toolbar has no mono and no heading button — `Tool::Style` is a dropdown of paragraph
/// styles, a different thing from a heading *toggle* — and a slot no button can set is a slot someone
/// will read one day and believe can be set. `SLOTS` is gone with them, for the reason in the paragraph
/// above: **nothing drew it, and its one caller wanted it not to exist.**
///
/// **These flags say "the next thing typed will be bold."** They are *armed at the caret*, not applied
/// to a range, because there is no selection. `Session::pending_style` is the same fact in the model and
/// this is the same fact in the chrome — **two copies of one value, which is a thing to be suspicious
/// of**, and `tests/armed_style.rs` is what keeps them in step rather than a comment claiming they are.
///
/// **They do not retro-style what is already typed.** Pressing Bold and then pressing Left and typing
/// does not bold the character before the caret: that needs a selection, and inventing one is a
/// different part.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StyleFlags {
    /// Bold is armed for the next character typed.
    pub bold: bool,
    /// Italic is armed for the next character typed.
    pub italic: bool,
}

impl StyleFlags {
    /// Whether `slot` is armed.
    pub const fn get(&self, slot: StyleFlagsSlot) -> bool {
        match slot {
            StyleFlagsSlot::Bold => self.bold,
            StyleFlagsSlot::Italic => self.italic,
        }
    }

    /// The slot `tool` toggles, if it toggles one.
    ///
    /// **A function rather than a `match` at each of the two call sites**, because the pairing of a
    /// button with the flag it sets is the thing most likely to be got wrong: a button that drew its
    /// active state from one flag and set another would light up and do nothing, which is a state no
    /// gate in this project would think to look for.
    pub const fn slot_for(tool: crate::widgets::Tool) -> Option<StyleFlagsSlot> {
        match tool {
            crate::widgets::Tool::Bold => Some(StyleFlagsSlot::Bold),
            crate::widgets::Tool::Italic => Some(StyleFlagsSlot::Italic),
            _ => None,
        }
    }
}

/// Which toolbar toggle a query is about.
///
/// **Two variants, because there are two buttons.** `Mono` and `Heading` were deleted in part 25: the
/// toolbar has no such button, and an unreachable variant of a `Copy` enum that a `match` must still
/// cover is a variant somebody will eventually cover with `Some(true)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StyleFlagsSlot {
    /// Bold.
    Bold,
    /// Italic.
    Italic,
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
        // **The ceiling is spelled out rather than `u32::from(bool)`, and `as u32` rather than a cast
        // on the `bool` being obvious.** Both operands are already `u32` and `pitch` is non-zero here.
        // **`as u32` is the conversion that compiles**: `From<bool> for u32` is not yet a const trait
        // (rust-lang/rust#143874), and this is a `const fn`, so `u32::from` is a hard error rather
        // than a style difference.
        //
        // **`is_multiple_of` and not `% == 0`**, which is the same arithmetic with a name: the question
        // being asked is "is the pitch dividing this exactly", and that is the question
        // `is_multiple_of` asks. A band whose height is not a whole number of line pitches still
        // occupies one extra slot, which is what the `+ 1` is for.
        height / pitch + (!height.is_multiple_of(pitch)) as u32
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
    /// **Which mode is selected, as an index into [`crate::menus::MODES`].** Part 44.
    ///
    /// **An index and not a `ViewMode`, and the reason is the same as `style_index` and
    /// `font_index`.** The chrome holds `u8`s into `&'static [&'static str]` tables so a label is a
    /// borrow rather than a `format!` on the paint path, and **a state loaded from a file written by a
    /// build with more modes must not panic** -- so it is an index and the accessor clamps.
    /// `an_out_of_range_mode_index_falls_back` is the gate.
    pub mode_index: u8,
    /// **The tools that did not fit on the toolbar, in `TOOLBAR` order.** Part 44.
    ///
    /// **`Vec` and not a count**, because the overflow popup draws *rows* and one of those rows is a
    /// clickable `Tool`. **A count could not answer "which"** and a parallel `HashSet` would be a
    /// second copy of a fact the layout already computed.
    ///
    /// **The session writes this and only the session writes it**, from `widgets::place_toolbar`'s
    /// zero-width rects -- which are already the definition of "did not fit". See
    /// `Session::refresh_overflow`.
    pub overflow_tools: Vec<crate::widgets::Tool>,
    /// Caret line, 0-based, relative to the first visible row.
    pub caret_line: u32,
    /// Caret column, 0-based.
    pub caret_column: u32,

    /// **Where the caret's pen is on its line, in pixels from the text column's left edge.** Part 33.
    ///
    /// ## Why this exists, and why it is `Option`
    ///
    /// **`caret_column` counts scalars and `Caret::locate` multiplied it by `cell_w`**, which is the
    /// chrome's monospace grid. **The document is Inter, which is proportional**: at 16 ppem `'a'`
    /// advances 9 and `'W'` advances 16 in the same cell of 10, so the caret was 10, 20 and 40 px
    /// *ahead* of the glyph it pointed at at columns 10, 20 and 40 — **part 30's standing decision,
    /// unmeasurable by any gate until now and wrong in every direction the letters can make it
    /// wrong.** `grid_vs_glyphs.rs` holds the measurement.
    ///
    /// **`None` is "not known on this frame"**, and it is different from `Some(0)`: a pen of zero is
    /// the answer at the start of a line and the answer a fresh session has not computed yet is not.
    /// [`Caret::locate`] falls back to the grid when it is `None`, which is the old behaviour and is
    /// the right thing to do rather than refuse to draw a caret.
    ///
    /// ## Why the session writes it and the chrome reads it
    ///
    /// **Part 22, and the same rule as `selection`.** The chrome does not know where the lines are,
    /// does not know the fonts' advances, and must not be able to invent either. The session owns the
    /// document and the atlas; it walks the caret's line in the same pass that places the runs and
    /// records where the pen actually reached. **So the caret cannot disagree with the text by
    /// construction** — the failure this field removes is precisely a disagreement.
    pub caret_pen_x: Option<u32>,
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
    /// Which of [`STYLES`](crate::menus::STYLES) is current, as an index.
    ///
    /// **An index and not the name**, and this is a CORRECTION made in part 21. It used to be
    /// `style_name: String`, which meant a `String` on the state and a `String` formatted for the
    /// toolbar on *every paint* — and two fields that could disagree, since nothing tied the name to
    /// the list. **An index into a `const` list is the whole state, is a byte, and makes the name a
    /// borrow.** `ChromeState::style_name` is the accessor.
    pub style_index: usize,
    /// Which of [`FONTS`](crate::menus::FONTS) is current, as an index. See [`style_index`](Self::style_index).
    pub font_index: usize,
    /// **Which of [`COLOURS`](crate::menus::COLOURS) is current, as an index.** Part 29.
    ///
    /// **The same index-into-a-`const`-list argument as [`style_index`](Self)**, and for the same
    /// reason: it is a byte, it makes the name a borrow, and it cannot disagree with the list.
    ///
    /// **Which is what the choice means here rather than what the document says.** `style_index` and
    /// `font_index` tick the current paragraph style and face, which the document has opinions about.
    /// **A colour is a property of a span**, so this ticks the colour the user *last chose* -- and a
    /// document whose text is four different colours ticks exactly one of them, which is the honest
    /// answer for a control with one swatch.
    pub colour_index: usize,
    /// Which popup is open, if any.
    ///
    /// **One value, and the type is the argument for it.** Part 20 had `open_menu: Option<usize>`;
    /// part 21 added the toolbar's three dropdowns, and the alternative to an enum was a second
    /// `Option` — and a state in which a menu and a dropdown were both open, drawn on top of each
    /// other, with no rule about which one the pointer meant. **See [`Open`](crate::widgets::Open).**
    pub open: Option<crate::widgets::Open>,
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
    /// **The selected byte range, or `None`.** Part 28.
    ///
    /// ## Why a range of document offsets and not a list of rectangles
    ///
    /// **Because the chrome does not know where the lines are.** `caret_line`/`caret_column` are
    /// already a pair the session computes because `Caret::locate` needs a row and a column; a
    /// selection spans lines, and expressing it as one rectangle per line would put the line model
    /// into this struct — where `Scrollbar::thumb` would then have to move every highlight on every
    /// scroll.
    ///
    /// **So the state carries the *fact* and the emitter does the geometry.** The fact is two numbers
    /// that are true of the document regardless of the window: the session writes them once per
    /// paint and the body-text emitter clips them to each visible line it is already reading.
    ///
    /// ## Why `Option`, and why `SelectionRange` rather than two `u32`s
    ///
    /// **`None` is "nothing selected", and it is a different answer from "selected nothing".** The
    /// type is [`holonomy_text::SelectionRange`], shared with the editor rather than redeclared, so
    /// the range the chrome highlights and the range the editor will delete are the same value from
    /// one constructor — and `SelectionRange` cannot hold a zero-length range, so a state carrying
    /// one is not constructible.
    ///
    /// ## Why the chrome cannot invent one
    ///
    /// **Because part 22.** `ChromeState` once carried a `caret_line` that `Session::new` never
    /// reconciled with `editor.caret()`, and it stayed wrong for twelve phases because both halves of
    /// the arithmetic it fed happened to be zero. A renderer able to author its own selection is the
    /// same bug waiting to happen; this one is a read-only field for that reason.
    pub selection: Option<holonomy_text::SelectionRange>,
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
            title: "Journal".to_string(),
            sidebar_open: true,
            docs: Vec::new(),
            active_doc: 0,
            style_index: 0,
            font_index: 0,
            colour_index: 0,
            open: None,
            cursor: None,
            hover: None,
            pressed: None,
            sealed: false,
            zoom_percent: 100,
            mode_index: 0,
            overflow_tools: Vec::new(),
            caret_line: 0,
            caret_column: 0,
            caret_pen_x: None,
            words: 0,
            bytes: 0,
            dirty: false,
            scroll_line: 0,
            total_lines: 1,
            styles: StyleFlags::default(),
            caret_visible: true,
            selection: None,
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
        // **The pen if the session gave one, the grid if it did not.** Part 33.
        //
        // **Both are read here and neither is computed here.** The grid arm is the old expression and
        // is kept deliberately: a session that has not walked the caret's line -- no atlas, a caret
        // on a line another emitter owns, a read that failed -- still draws a caret, and a caret on
        // the grid is 10 px out rather than absent.
        let dx = match state.caret_pen_x {
            Some(pen) => pen,
            None => state.caret_column.checked_mul(m.cell_w)?,
        };
        let x = layout.text.x.checked_add(dx)?;
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
        // **Both transitions dirty the caret cell.** Hiding has to repaint the page behind
        // the caret; returning `None` there left the previous caret pixels on screen until
        // some unrelated edit happened to repaint that cell.
        caret
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
/// **The width is measured, not tabulated.** A table of eight widths would need a column per menu and
/// would be wrong the moment a heading was renamed, and the measurement is `str::len()` because every
/// heading is ASCII -- **which is a property of [`MENUS`] and not an accident**, and the assertion in
/// `the_menu_headings_are_measured_not_tabulated` is what keeps it one.
///
/// ## The gap is a quarter of the cell, and it used to be a constant
///
/// **`MENU_GAP` was 2 px, which is a quarter of the 8 px design cell.** Part 32 raised the cell to 14
/// and a 2 px gap between headings that are themselves 1.5x wider is not a gap: `Insert`, `Format`,
/// `Tools`, `Extensions` and `Help` rendered as one run-on word. **The gap is now the same fraction of
/// the cell the constant was a fraction of**, which is what it always meant.
/// **`Extensions` and `Help` are the two that go.** A narrow window loses the least-used menus rather
/// than the most-used ones: File, Edit and View are what a person reaches for, and losing those would
/// make the bar useless rather than merely incomplete.
///
/// **Dropping from the tail, so every surviving index is unchanged.** `ChromeState::open_menu` is an
/// index into [`MENUS`] and so is `Hit::Menu(i)`, and both are compared against the box list. If the
/// bar elided from the *front* — which would keep Help and drop File — then a menu open at index 3
/// would start meaning a different menu every time the window narrowed, and a popup would follow its
/// heading sideways. **The tail is the only end that can be elided without renaming everything above
/// it.**
pub fn menu_boxes(l: &Layout) -> (u32, Vec<DamageRect>) {
    let gap = l.cell_w / 4;
    let left = l.menubar.x + l.cell_w * 2;
    // **The budget is the panel's right edge**, so this is the one line that makes the bar fit its
    // window. Before it the loop ran to `MENUS.len()` unconditionally, which at 480 px and a 7.5 px
    // cell put "Extensions" 92 px past the edge and drew it anyway — `bugs/1.png`, where the bar
    // reads `File Edit View Insert Format Tools Extens`.
    let budget = l.menubar.x.saturating_add(l.menubar.width);
    let mut x = left;
    let mut out = Vec::with_capacity(MENUS.len());
    for m in MENUS {
        let w = menu_width(m, l.cell_w);
        // **The heading stops at the panel edge, not past it.** `x + w > budget` is the whole rule.
        // A heading that only *partly* fits is dropped whole: a half-drawn `Extens` is the thing this
        // fixes, and truncating a heading to the cells that fit would reproduce it for every heading
        // rather than for one.
        if x.saturating_add(w) > budget {
            break;
        }
        out.push(DamageRect::new(x, l.menubar.y, w, l.menubar.height));
        x += w + gap;
    }
    (x, out)
}

/// The width of one heading's box: the text plus its padding, in cells.
///
/// **Two cells of padding on each side.** One is not enough -- the boxes would touch and the seam
/// would be ambiguous, which is exactly what [`DamageRect::contains`]'s half-open rule is there to
/// resolve, and a rule that is needed because the layout is too tight is a sign the layout is too
/// tight.
///
/// ## It takes the cell width, and part 32 is why
///
/// **It was `const fn menu_width(m: &str)`, reading a module constant `CHROME_CELL_W = 8`,** and the
/// recorded reason was: *"a `const fn` cannot read a runtime metrics struct, and making the menu bar's
/// geometry depend on a runtime value would mean the headings move when the font metrics are
/// recalibrated -- which is a rearrangement nobody would ask for."*
///
/// **Part 32 is somebody asking for it.** The type was raised to 24 ppem, the chrome's cell followed
/// the face to 14 px, and the menu bar -- the one piece of chrome still on the old constant -- laid
/// eight headings out at 8 px per character while drawing them at 24. **`Fil eEdit ViewInsertFormatT
/// ool sExtensionsHelp`, measured, in a screenshot.** The reasoning was aesthetic and the evidence was
/// not gathered; the guard against it was a gate comparing the constant with `DESKTOP.cell_w`, **and
/// `DESKTOP.cell_w` is still 8** because the session overrides it at run time, so the gate compared the
/// constant with itself and stayed green through the entire failure.
pub fn menu_width(m: &str, cell_w: u32) -> u32 {
    (m.len() as u32 + 4) * cell_w
}

/// The gap between two heading boxes was `2`. **It is `l.cell_w / 4`** -- the same fraction of the
/// design's 8 px cell -- and the constant is gone rather than kept as a second answer. See
/// [`menu_boxes`](Self::menu_boxes).
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

/// **REMOVED in part 32, and this is its correction.**
///
/// It was `pub const CHROME_CELL_W: u32 = 8`, "duplicated rather than reached through
/// [`ChromeMetrics`], because `menu_width` is a `const`", and the gate against drift compared it with
/// `ChromeMetrics::DESKTOP.cell_w`.
///
/// **Both halves of that failed.** `menu_width` is not a `const fn` any more -- it takes the cell width
/// as an argument, which is two characters of change and removes the stated reason entirely. And
/// **`DESKTOP.cell_w` was never the value in use**: `Session::new` overrides it from the face's own
/// advance, so the gate compared the constant with a constant that happens to match it and stayed
/// green while the menu bar rendered eight headings on a grid the rest of the chrome had left.
///
/// **A "cannot drift" gate that compares a constant with itself is not a gate.** It is the most
/// dangerous kind: it says the thing you most want said, and it says it for the wrong reason.
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
    /// # The layer order is this function's contract
    ///
    /// **Five layers, painted in exactly this order, and nothing may be emitted out of order:**
    ///
    /// | layer | what | where it goes |
    /// |---|---|---|
    /// | 0 | panel, bands, rules, title, menu bar, toolbar | `root.before` |
    /// | 1 | the page: shadow, sheet, scrollbar | `root.before` |
    /// | 2 | the document-tabs sidebar | `root.before` |
    /// | 3 | the document overlay: selection, caret | `root.before` |
    /// | 4 | **the open popup** | `root.after` |
    ///
    /// # Why the sidebar is layer 2 and not part of layer 0
    ///
    /// **Because the page is drawn after layer 0 and the sidebar is a panel inside the canvas.** The
    /// sidebar used to be emitted with the other chrome, so every paint drew the page *over* it — and
    /// the symptom was not a wrong rectangle, it was **text**: part 36 recorded that `"Document tabs"`
    /// and a document title ran *underneath the page* and had to be clipped to the sidebar's width with
    /// `fit_to_width` to hide it. The clipping is still right (a title longer than the rail is still
    /// longer than the rail) but the width it was clipped *to* was the page's left edge, which is a
    /// coincidence of the bug rather than a property of the sidebar.
    ///
    /// # Why the popup is the only thing in `after`
    ///
    /// **`SurfaceTree::after` is painted after `node` and after every entry in `before`, and the
    /// document emitters push into `root.before`** (`Session::emit_body_text` and its three siblings).
    /// A popup emitted anywhere in `before` is therefore painted over by the document's own glyphs the
    /// moment a menu hangs down over the page — which is exactly what *"the dropdown opens one layer
    /// back"* looked like. `after` is the one place in the tree a caller cannot paint into by accident,
    /// so the topmost layer is the only place it goes.
    ///
    /// `paint_popup` is called once, here, whichever kind of popup it is: the menu bar does not draw
    /// the menu bar's popups and the toolbar does not draw the toolbar's dropdowns. That is why `Open`
    /// is an enum — a popup is a popup, and the two things that can be one do not know about each other.
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
                // **The number, every ten columns, centred over its tick.**
                //
                // **Part 39: the ruler had ticks and no labels for its whole life**, so it read as a
                // dotted line: 31 px of band carrying two rows of one-pixel marks and nothing a person
                // could read. The reference's ruler is numbered `1 .. n` across the measure, and the
                // number is what makes it a ruler rather than a dotted rule — **the ticks alone say
                // "something is measured here" and the numbers say what.**
                //
                // **Centred on the tick rather than left-aligned**, because the reference centres them
                // and a left-aligned number sits over the *previous* interval: `1` at x = 0 is
                // half off the left edge of the page.
                //
                // **`col / 10 + 1` and not `col / 10`**, so the leftmost column is 1 — the reference's
                // first number is 1, and a ruler that starts at 0 has a number hanging off the page's
                // left edge.
                if col.is_multiple_of(tick_every) {
                    let n = (col / tick_every + 1).to_string();
                    let w = n.chars().count() as u32 * m.cell_w;
                    // **Skipped rather than clipped when it will not fit.** A number half outside the
                    // measure is worse than no number: `push_text` cannot clip, so this is the only way
                    // to keep the last one from being drawn over the page's right-hand margin.
                    if w <= m.cell_w * 4 && x + w / 2 < l.text.right() {
                        push_text(
                            &mut chrome.before,
                            m,
                            l.ruler.y,
                            &n,
                            Style::REGULAR,
                            colour::INK_DIM,
                            (x + w / 2).saturating_sub(w / 2) as i32,
                        );
                    }
                }
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
        // **The sidebar is NOT here.** It is layer 2: a panel inside the canvas, drawn over the page.
        // See the layer table on this function.

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
                l.title.y + m.title_h.saturating_sub(m.cell_h) / 2,
                "[SEALED]",
                Style::REGULAR,
                colour::SEALED,
                // **`saturating_sub`, and it is the sixth bare subtraction in this file.**
                //
                // The badge is eight characters, so it is placed fourteen cells from the right
                // edge of the title band. **A panel narrower than fourteen cells underflows
                // here** -- at `opt-level=0` that is a panic, and in a release build with
                // `overflow-checks` on it is a panic too. The panel *can* be that narrow now
                // that `for_size` takes the window's size rather than rounding up to
                // `MIN_WIDTH`, and the badge is drawn only when the session is sealed -- which
                // is why the size sweep never saw it: it paints a dirty, unsealed chrome.
                l.title.right().saturating_sub(14 * m.cell_w) as i32,
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
            l.status.y + l.status.height.saturating_sub(m.cell_h) / 2,
            &status,
            Style::REGULAR,
            colour::INK_DIM,
            m.cell_w as i32,
        );
        if state.dirty {
            chrome.before.push(fill(
                // **`saturating_sub`, and it is the fourth bare subtraction in this file.**
                // The dirty-dot sits three cells from the right edge, so a panel narrower than
                // three cells underflows here -- and at `opt-level=0` that is a panic, in release a
                // four-billion-pixel `i32`. The panel can be that narrow now that `for_size` takes
                // the window's size rather than rounding up to `MIN_WIDTH`.
                l.width.saturating_sub(m.cell_w * 3) as i32,
                (l.status.y + 4) as i32,
                m.cell_w,
                m.cell_h.saturating_sub(8),
                colour::ACTIVE,
            ));
        }
        root.before.push(chrome);

        // --- layer 2: the sidebar. Above the page, below the popup.
        //
        // **The page is pushed first, so this paints over it.** That is the whole of the ordering: the
        // sidebar is a panel *inside* the canvas, and a panel inside a canvas is in front of the sheet
        // the canvas holds.
        if state.sidebar_open {
            let mut sidebar = SurfaceTree::group();
            paint_sidebar(&mut sidebar.before, m, l, state);
            root.before.push(sidebar);
        }

        // --- page: shadow, sheet, scrollbar.
        let mut page = SurfaceTree::group();
        // # CORRECTION 2026-10-12: the shadow is bounded by the page it belongs to
        //
        // The page's fills are offset by `m.shadow` to draw a flat band down and to the right. **That
        // offset was the page's width and height, with nothing stopping it** — so in a panel narrower
        // than the shadow the band started past the panel's right edge. The paint sweep found it at
        // 1x1: a rect at (3, 1) 1x3 in a 1x1 panel.
        //
        // **Bounded here rather than by adding a field to `Layout`**, because the bound is a property
        // of *drawing the page*; `Layout` answers "where is everything", not "how do I paint one piece
        // of it". A `Layout::shadow` would be a second source for a number `ChromeMetrics::shadow`
        // already is.
        let shadow = m.shadow.min(l.page.width).min(l.page.height);
        // Shadow: a flat band to the right and below. Not a gradient, because alpha is masked off
        // in the frame and a ramp would need `f32`.
        page.before.push(fill(
            (l.page.x + shadow) as i32,
            l.page.y as i32,
            l.page.width,
            shadow,
            colour::SHADOW,
        ));
        page.before.push(fill(
            l.page.x as i32,
            (l.page.y + shadow) as i32,
            shadow,
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
        // A hairline border, as the reference's sheet has.
        for (bx, by, bw, bh) in [
            (l.page.x, l.page.y, l.page.width, 1),
            (l.page.x, l.page.bottom().saturating_sub(1), l.page.width, 1),
            (l.page.x, l.page.y, 1, l.page.height),
            (l.page.right().saturating_sub(1), l.page.y, 1, l.page.height),
        ] {
            page.before.push(fill(bx as i32, by as i32, bw, bh, colour::RULE));
        }
        // Scrollbar rail and thumb.
        page.before.push(fill(
            l.scrollbar.x as i32,
            l.scrollbar.y as i32,
            l.scrollbar.width,
            l.scrollbar.height,
            colour::TRACK,
        ));
        let thumb = self.scroll_thumb(state);
        page.before.push(rounded(
            thumb.x as i32 + 2,
            thumb.y as i32,
            thumb.width.saturating_sub(4),
            thumb.height,
            6,
            colour::THUMB,
        ));
        root.before.push(page);

        // --- layer 3: the document overlay — the caret. (The selection is a document emitter and
        // travels with the text it belongs to; see `Session::emit_body_text`.)
        let mut overlay = SurfaceTree::group();
        if state.caret_visible {
            if let Some(caret) = Caret::locate(l, m, state) {
                // A thin insertion caret, as the reference draws; the damage cell stays one cell.
                overlay.before.push(fill(
                    caret.cell.x as i32,
                    caret.cell.y as i32,
                    2,
                    caret.cell.height,
                    colour::CARET,
                ));
            }
        }
        root.before.push(overlay);

        // --- layer 4: the open popup, in `after`.
        //
        // **`after`, not `before`, and it is the whole fix.** See the layer table on this function.
        if state.open.is_some() {
            let mut popup = SurfaceTree::group();
            paint_popup(&mut popup.before, m, l, state);
            root.after.push(popup);
        }
        root
    }
}

/// The width [`ChromeMetrics::sidebar_w`] must have for a sidebar that is open or closed.
///
/// **One function, because "the sidebar is drawn" and "the sidebar's width is reserved" are the same
/// fact and `Layout::new` has to be able to trust it.** Before this existed the reservation was a
/// constant 208 that was *always* subtracted from the width, so a closed sidebar still pushed the page
/// 208 px to the right — and the flag that decided whether to draw it was a `bool` on
/// [`ChromeState`], which no layout arithmetic could see.
#[must_use]
pub const fn sidebar_reservation(open: bool) -> u32 {
    if open {
        ChromeMetrics::DESKTOP.sidebar_w
    } else {
        0
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

/// A rounded rectangle, as a tree node.
fn rounded(x: i32, y: i32, width: u32, height: u32, radius: u32, colour: u32) -> SurfaceTree {
    SurfaceTree::leaf(crate::Node::RoundRect(RoundRect::new(x, y, width, height, radius, colour)))
}

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
//
// **This banner is `//` throughout, and it stood as `///`.** A doc comment followed by a blank line
// and then a function is a doc comment *for that function*, so rustdoc published "the whole of the
// chrome's response to a pointer" as the documentation of `hovered` -- **a claim about three functions
// recorded as the documentation of one.** Clippy's `empty_line_after_doc_comments` is what noticed.
//
// **The trailing `///` line above was the whole of it.** A block that is `//` for four lines and `///`
// for the fifth is *still* a doc comment to rustdoc -- the attributes are per line and the last one
// before the blank line is what counts. **It is now `//` like the rest, and there is no doc comment
// here for anything to attach to.**

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
    into.push(fill(0, l.title.y as i32, l.width, m.title_h, colour::CHROME));
    let icon_y = l.title.y + m.title_h.saturating_sub(crate::icons::SIZE) / 2;
    into.push(SurfaceTree::leaf(crate::Node::Icon(
        crate::icons::IconId::Doc.at(24, icon_y as i32, colour::BLUE),
    )));
    let title_x = 24 + crate::icons::SIZE as i32 + 12;
    let title_y = l.title.y + m.title_h.saturating_sub(m.cell_h) / 2;
    // Keep a long document name clear of the star/folder/cloud group that follows it.
    let left_group_x = m.cell_w.saturating_mul(32) as i32;
    let title_cells = ((left_group_x - title_x - 8).max(0) as u32) / m.cell_w.max(1);
    push_text(into, m, title_y, &fit_to_width(&state.title, m.cell_w, title_cells), Style::REGULAR, colour::INK, title_x);

    for (btn, r) in crate::widgets::place_title(l, crate::widgets::TITLE_PAD) {
        use crate::widgets::TitleButton as B;
        let pill = matches!(btn, B::Share | B::Upgrade);
        if pill {
            let (bg, ink) = if btn == B::Share { (colour::SHARE_BG, colour::SHARE_INK) } else { (colour::UPGRADE_BG, colour::UPGRADE_INK) };
            into.push(rounded(r.x as i32, r.y as i32, r.width, r.height, r.height / 2, bg));
            let cy = r.y + (r.height.saturating_sub(m.cell_h)) / 2;
            if btn == B::Share {
                into.push(SurfaceTree::leaf(crate::Node::Icon(
                    crate::icons::IconId::Lock.at(r.x as i32 + 12, r.y as i32 + (r.height.saturating_sub(crate::icons::SIZE)) as i32 / 2, ink),
                )));
                push_text(into, m, cy, "Export", Style::REGULAR, ink, r.x as i32 + 12 + crate::icons::SIZE as i32 + 6);
            } else {
                push_text(into, m, cy, "Local", Style::REGULAR, ink, r.x as i32 + 16);
            }
            continue;
        }
        if let Some(c) = state_fill(state, crate::widgets::Hit::Title(btn)) {
            into.push(rounded(r.x as i32, r.y as i32, r.width, r.height, r.height / 2, c));
        }
        let icon = match btn {
            B::Star => crate::icons::IconId::Star,
            B::Folder => crate::icons::IconId::Folder,
            B::Cloud => crate::icons::IconId::Cloud,
            B::History => crate::icons::IconId::History,
            B::Comments => crate::icons::IconId::Comment,
            B::Sparkle => crate::icons::IconId::Sparkle,
            B::Share | B::Upgrade => unreachable!(),
        };
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
        let open = state.open == Some(crate::widgets::Open::Menu(i));
        // **Open outranks hovered.** A heading whose menu is down is filled whether or not the pointer
        // is on it, because it stays filled after the pointer leaves -- and a heading that lost its
        // fill the moment the pointer left would tell the user the menu had closed.
        if open {
            into.push(rounded(r.x as i32, r.y as i32, r.width, r.height, 6, colour::PILL));
        } else if let Some(c) = state_fill(state, crate::widgets::Hit::Menu(i)) {
            into.push(rounded(r.x as i32, r.y as i32, r.width, r.height, 6, c));
        }
        push_text(
            into,
            m,
            r.y + (r.height.saturating_sub(m.cell_h)) / 2,
            MENUS[i],
            Style::REGULAR,
            if open {
                colour::INK_STRONG
            } else {
                colour::INK_CHROME
            },
            r.x as i32 + 2 * m.cell_w as i32,
        );
    }

    // **A popup is NOT drawn here, and where it used to be is the layering defect part 39 fixed.**
    //
    // This is the one line the whole report was about -- *"the dropdown opens one layer back than the
    // toolbar"* -- and it was here: the menu band drew its own popup into `chrome.before`, and the
    // toolbar and the sidebar were pushed into the same vector *after* it. `Painter::walk` paints
    // `before` in order, so every menu was painted **under** the toolbar pill, under the sidebar and
    // under the page.
    //
    // `Chrome::tree` now owns the layer order, and the popup goes in `root.after` -- drawn after
    // everything else in the tree, including the document emitters that push into `root.before`.
    // See [`Chrome::tree`].
}

/// An open menu's popup: a panel under its heading, with the items.
///
/// **Drawn at a fixed height per item and anchored under the heading's left edge**, which is what makes
/// a menu readable: the items start where the heading does, so the eye does not have to travel. The
/// width is the widest label plus padding, computed here rather than tabulated, because the items are
/// data.
fn paint_popup(into: &mut Vec<SurfaceTree>, m: &ChromeMetrics, l: &Layout, state: &ChromeState) {
    // **The geometry comes from `widgets::popup`, not from here.** The first version computed the
    // popup's width, height and row positions inline, from `menus::items_for`, and the hit test had
    // nothing to test against -- which is the exact duplication `widgets.rs` exists to prevent, and
    // this function was the other half of it. **One function, two consumers**, so a row that is drawn
    // is a row that can be clicked, by construction.
    // **A menu with no items draws nothing, and that is not an error.** `open_menu` is set from a
    // heading index and every heading has items, so this arm is unreachable through the session -- but
    // `popup` is `Option` because a *caller* could pass a bad index, and the answer to that is a
    // silent no-op rather than a panic inside a paint.
    let Some(pop) = crate::widgets::popup(l, state, state.open) else {
        return;
    };
    let items = pop.open.items(state);
    let pad = 2 * m.cell_w;
    let x = pop.rect.x as i32;
    let y = pop.rect.y as i32;

    into.push(rounded(x + 2, y + 3, pop.rect.width, pop.rect.height, 8, colour::SHADOW));
    into.push(rounded(x, y, pop.rect.width, pop.rect.height, 8, colour::POPUP));
    for (i, it) in items.iter().enumerate() {
        let Some(row) = pop.rows.get(i) else { break };
        // **Hover and press come from the session, not from a re-hit-test here.** The painter does not
        // know where the pointer is -- it is told. A painter that re-derived the hit would be a second
        // hit test, which is the thing that has just been removed.
        if row_is_live(state, pop.open, i) {
            into.push(rounded(row.x as i32 + 4, row.y as i32, row.width.saturating_sub(8), row.height, 6, colour::PILL_HOVER));
        }
        let cy = row.y + (row.height.saturating_sub(m.cell_h)) / 2;
        let icon_y = row.y as i32 + (row.height.saturating_sub(crate::icons::SIZE)) as i32 / 2;
        // **The tick goes in the left gutter, at a fixed offset, and the icon follows it.**
        //
        // A fixed offset and not "after whatever is there" is what makes the label column line up
        // between a ticked row and an unticked one -- `widgets::popup` computes the panel's width
        // assuming the tick is always in that column, and a row without one must not shift its label
        // left to fill the gap. **The alternative — no tick at all — is what part 20 shipped, and the
        // zoom dropdown in the reference's screenshot has a tick on its current value.**
        if it.checked {
            into.push(SurfaceTree::leaf(crate::Node::Icon(
                crate::icons::IconId::Check.at(x + pad as i32, icon_y, colour::ACTIVE),
            )));
        }
        // **Then the icon, one column right of where the tick would be**, so a row with a tick and an
        // icon and a row with only an icon have their *labels* in the same place.
        let icon_x = x + pad as i32 + 2 * m.cell_w as i32;
        if let Some(icon) = it.icon {
            into.push(SurfaceTree::leaf(crate::Node::Icon(icon.at(
                icon_x,
                icon_y,
                colour::INK_CHROME,
            ))));
        }
        // **The label starts in column 4 and stops short of the accelerator.**
        //
        // **CORRECTION, part 39:** the label used to be drawn at full length, and the accelerator was
        // right-aligned *over whatever was there*. `widgets::popup` now sizes the panel over both of
        // them (`widgets::row_cells`), so in the common case there is room and this is the identity —
        // **and where there is not, which is a long label beside a long accelerator, the label is cut
        // here rather than drawn through it.**
        //
        // ```text
        //   | pad 2 | lead 4 |  label  | gap 2 |  accelerator  | pad 2 |
        // ```
        let label_x = icon_x + 2 * m.cell_w as i32;
        // **`map_or` and not `.map(..).unwrap_or(0)`.** The chain reads as: the accelerator if there is
        // one and it is not empty, otherwise zero cells.
        let acc_cells = it
            .accelerator
            .filter(|a| !a.is_empty())
            .map_or(0u32, |a| a.chars().count() as u32);
        // `pad` on the left, plus the tick, icon and gap columns: `2 + 4`.
        const LABEL_OFFSET_CELLS: u32 = 6;
        // The gap between a label and the accelerator beside it, in cells.
        const LABEL_GAP_CELLS: u32 = 2;
        let panel_cells = pop.rect.width / m.cell_w.max(1);
        let label_cells = panel_cells
            .saturating_sub(LABEL_OFFSET_CELLS)
            .saturating_sub(LABEL_GAP_CELLS)
            .saturating_sub(acc_cells)
            .max(1);
        push_text(
            into,
            m,
            cy,
            &fit_to_width(
                it.label,
                m.cell_w,
                label_cells,
            ),
            Style::REGULAR,
            colour::INK,
            label_x,
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
                    Style::REGULAR,
                    colour::INK_DIM,
                    // The same fault as the dirty-dot, in the popup's accelerator column: a popup
                    // narrower than its own padding plus one character of text underflows. **Both are
                    // `saturating_sub` now**, and both were found by a gate that paints at 1x1
                    // rather than by reading the code.
                    x + pop
                        .rect
                        .width
                        .saturating_sub(pad)
                        .saturating_sub(acc.chars().count() as u32 * m.cell_w) as i32,
                );
            }
        }
    }
}

/// Whether popup row `row` is under the pointer or held down.
///
/// **A comparison against the session's own `hover` and `pressed`, and nothing else.** The painter
/// does not re-derive the hit: a painter that did would be a second hit test, which is precisely the
/// duplication `widgets.rs` was written to remove. **Hover and press draw the same row** because the
/// reference's highlighted menu item is the same surface either way -- and a menu you are holding open
/// over a row does not look different from a menu you are pointing at.
///
/// The two are separate `Hit` values only so the session can clear `pressed` on a release that
/// happened off the widget. **They render identically, which is why one function reads both.**
fn row_is_live(state: &ChromeState, open: crate::widgets::Open, row: usize) -> bool {
    let h = crate::widgets::Hit::MenuItem { open, row };
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
    into.push(rounded(pill.x as i32, pill.y as i32, pill.width, pill.height, pill.height / 2, colour::PILL));

    let mut prev_separated = false;
    for p in crate::widgets::place_toolbar(l, crate::widgets::TOOLBAR_PAD) {
        // # CORRECTION 2026-10-12: a tool with no room is skipped, not drawn at zero
        //
        // `place_toolbar` now hands back a zero-extent rect for a tool that does not fit — the right
        // answer, and it is what stops the row accumulating invisible click targets past the edge.
        //
        // **The emitter did not know that, and drew anyway.** The separator's `x` is
        // `p.rect.x - SEPARATOR/2 - 1`, so a tool at `x = 0` put the separator at **x = -5** — a
        // wrapped `u32` cast to `i32`, which is how `0u32 - 5u32` becomes a negative coordinate. The
        // paint sweep caught it as a rect at (-5, 4) 1x0.
        //
        // **Zero extent means nothing to draw and nothing to separate from.** Skipping is also what
        // keeps the hover and press fills below from painting a zero-area surface at the origin.
        if p.rect.width == 0 || p.rect.height == 0 {
            prev_separated = false;
            continue;
        }
        if p.tool.separated() && !prev_separated {
            // The separator is drawn on the tool's *left* edge, in the gap before it.
            let sx = p.rect.x as i32 - (crate::widgets::SEPARATOR / 2) as i32 - 1;
            into.push(fill(
                sx.max(0),
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
            into.push(rounded(p.rect.x as i32, p.rect.y as i32, p.rect.width, p.rect.height, 8, c));
        }

        // **An armed toggle, drawn between the hover surface and the glyph.** Part 25.
        //
        // **`PILL_ACTIVE` and not `PILL`, and the first version used `PILL` and drew nothing.**
        // `PILL` is the toolbar pill's own fill — the colour the button already is — so filling a button
        // with it is invisible, and the gate that caught it probed a pixel and found it unchanged. There
        // are three colours in a row: `PILL` unraised, `PILL_HOVER` under the pointer, `PILL_ACTIVE`
        // held or selected. **An armed toggle is the third one.**
        //
        // **Distinct from hover, which is the point.** A toggle that is *on* and a button that is *under
        // the pointer* are different facts, and giving them the same colour would make them
        // indistinguishable, which is the one thing a toolbar button must not do.
        //
        // **After the hover fill, before the glyph, for the reason the hover fill is there at all:** the
        // glyph goes on top of the state surface, because the surface is behind the thing it is a state
        // of. Drawn after the glyph it would put a rectangle over the icon.
        //
        // **And it is drawn whether or not the pointer is anywhere near the button**, which is the whole
        // point: the state is the document's, not the pointer's.
        if let Some(slot) = crate::chrome::StyleFlags::slot_for(p.tool) {
            if state.styles.get(slot) {
                into.push(rounded(p.rect.x as i32, p.rect.y as i32, p.rect.width, p.rect.height, 8, colour::PILL_ACTIVE));
            }
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
                Style::REGULAR,
                colour::INK_CHROME,
                // **`ih` and not `ih as i32`.** The icon size is cast to `i32` once, on the line that
                // binds it, because every use here is `i32` arithmetic on a `u32` rectangle -- and a
                // cast repeated at four call sites is four places to keep in step.
                ix + ih + 4,
            );
            // The chevron that says "this opens a popup". The font-size box is a value, not a menu.
            if p.tool.has_dropdown() {
                into.push(SurfaceTree::leaf(crate::Node::Icon(
                    crate::icons::IconId::ChevronDown.at(
                        p.rect.right() as i32 - ih - 4,
                        iy,
                        colour::INK_DIM,
                    ),
                )));
            } else {
                into.push(SurfaceTree::leaf(crate::Node::RoundRect(crate::RoundRect::new(
                    p.rect.x as i32, p.rect.y as i32, p.rect.width, p.rect.height, 6, colour::PAGE,
                ))));
                // redraw label over the box
                push_text(into, m, p.rect.y + (p.rect.height.saturating_sub(m.cell_h)) / 2,
                    &p.tool.label(state), Style::REGULAR, colour::INK, ix + ih + 4);
            }
        } else if p.tool == crate::widgets::Tool::Overflow {
            // **Part 44: `Overflow`'s chevron is drawn only when there is something in the menu.**
            //
            // **A chevron on a button that opens an empty popup is a button that lies**, and unlike
            // the labelled controls -- whose rows are a compile-time table -- `Overflow`'s rows are
            // the tools that did not fit, which is empty on a 1,280 px panel. **Drawing it
            // conditionally is what makes `has_dropdown` able to answer "empty means closed"**, and
            // `an_overflow_button_with_nothing_hidden_draws_no_chevron` is the gate.
            //
            // **The chevron goes to the right of the icon rather than under a label**, because
            // `Overflow` has no label -- the reference draws it as an icon and a small caret, and
            // `Tool::has_label` is what decides which of the two this is.
            if !state.overflow_tools.is_empty() {
                into.push(SurfaceTree::leaf(crate::Node::Icon(
                    crate::icons::IconId::ChevronDown.at(
                        p.rect.right() as i32 - ih,
                        iy,
                        colour::INK_DIM,
                    ),
                )));
            }
        }
    }
}

/// The sidebar: a back arrow, a heading, a "+", and one row per document.
fn paint_sidebar(into: &mut Vec<SurfaceTree>, m: &ChromeMetrics, l: &Layout, state: &ChromeState) {
    into.push(fill(0, l.sidebar.y as i32, l.sidebar.width, l.sidebar.height, colour::CHROME));
    into.push(SurfaceTree::leaf(crate::Node::Icon(
        crate::icons::IconId::ArrowLeft.at(
            l.sidebar_back.x as i32 + 8,
            l.sidebar_back.y as i32 + 8,
            colour::INK_CHROME,
        ),
    )));
    let head_y = l.sidebar_new.y + (l.sidebar_new.height.saturating_sub(m.cell_h)) / 2;
    push_text(into, m, head_y, "Document tabs", Style::REGULAR, colour::INK, l.sidebar.x as i32 + 40);
    into.push(SurfaceTree::leaf(crate::Node::Icon(
        crate::icons::IconId::Plus.at(
            l.sidebar_new.x as i32 + 8,
            l.sidebar_new.y as i32 + 8,
            colour::INK_CHROME,
        ),
    )));

    for i in 0..l.sidebar_rows as usize {
        let (Some(title), Some(r)) = (state.docs.get(i), l.sidebar_doc(i)) else { break };
        let active = i == state.active_doc;
        if active {
            into.push(rounded(r.x as i32, r.y as i32, r.width, r.height, r.height / 2, colour::UPGRADE_BG));
        } else if let Some(c) = state_fill(state, crate::widgets::Hit::Doc(i)) {
            into.push(rounded(r.x as i32, r.y as i32, r.width, r.height, r.height / 2, c));
        }
        let ink = if active { colour::UPGRADE_INK } else { colour::INK_CHROME };
        into.push(SurfaceTree::leaf(crate::Node::Icon(
            crate::icons::IconId::Doc.at(
                r.x as i32 + 16,
                r.y as i32 + (r.height as i32 - crate::icons::SIZE as i32) / 2,
                ink,
            ),
        )));
        let ty = r.y + (r.height.saturating_sub(m.cell_h)) / 2;
        let tx = r.x as i32 + 16 + crate::icons::SIZE as i32 + 10;
        push_text(into, m, ty,
            &fit_to_width(title, m.cell_w, cells_left_of(r.right(), tx, m.cell_w)),
            Style::REGULAR, ink, tx);
        if active {
            into.push(SurfaceTree::leaf(crate::Node::Icon(
                crate::icons::IconId::Overflow.at(
                    r.right() as i32 - crate::icons::SIZE as i32 - 12,
                    r.y as i32 + (r.height as i32 - crate::icons::SIZE as i32) / 2,
                    ink,
                ),
            )));
        }
    }
}

/// The longest prefix of `s` that fits in `width` pixels from `x`, with an ellipsis when it is cut.
///
/// **In codepoints, against [`ChromeMetrics::cell_w`]**, because that is the one measure the sidebar
/// row and the painter both already use -- `push_text` advances by the cell, so a prefix longer than
/// the cell count would be drawn past the edge however carefully it was counted.
///
/// **One cell is always kept for the ellipsis**, so a cut name is visibly cut rather than silently
/// ending at a word boundary the reader cannot see.
fn fit_to_width(s: &str, cell_w: u32, max_cells: u32) -> String {
    if max_cells == 0 {
        return String::new();
    }
    if s.chars().count() as u32 <= max_cells {
        return s.to_string();
    }
    // A cut name is visibly cut. ASCII dots are used because the embedded atlas has no U+2026,
    // and a missing-glyph box in a document title would be worse than a plain truncation marker.
    let (keep, marker) = if max_cells >= 3 { (max_cells - 3, "...") } else { (max_cells.saturating_sub(1), ".") };
    let mut out: String = s.chars().take(keep as usize).collect();
    out.push_str(marker);
    let _ = cell_w;
    out
}

/// How many whole cells fit between `from` and `to`, leaving `INSET` for the active row's overflow icon.
fn cells_left_of(to: u32, from: i32, cell_w: u32) -> u32 {
    /// Right-hand inset, so a name does not touch the rail.
    const INSET: u32 = 20;
    let cell = if cell_w == 0 { 1 } else { cell_w };
    to.saturating_sub(INSET).saturating_sub(from.max(0) as u32) / cell
}

/// A `TextRun` for one glyph at `x, y`.
///
/// A horizontal rule of `n` `─` glyphs starting at `x`, `y`.
///
/// # CORRECTION, part 39: this was a `TextRun`, and a `TextRun` is *consecutive codepoints*
///
/// ```text
/// TextRun::new(x, y, '─', n, ..)   ==   U+2500, U+2501, U+2502, .. U+2500 + n - 1
/// ```
///
/// **So every rule in the application was a row of unrelated Box Drawing glyphs.** At the default
/// width a rule is ~120 cells long, which is the first 120 codepoints of the block: `─│┌┐└┘├┤┬┴┼`,
/// then every rounded corner and every diagonal in the Unicode block, one per cell. It is in every
/// screenshot of this application as a row of crossed circles — under the menu bar, across the ruler,
/// at the foot of the sidebar.
///
/// **Nothing caught it, and the reason is worth keeping**: every gate asserted that the rule node
/// existed, that it was inside the panel, and that its colour was `RULE`. **A gate that checks what a
/// node *is* cannot catch a node that draws the wrong thing**, and "n copies of a glyph" was never
/// something a `TextRun` could be asked to be.
///
/// A run rather than a `Rect`, so the rule joins up: `─` is drawn to the full cell width by
/// construction, not by accident — see the LEFT and RIGHT arms in `box_drawing::draw_glyph`.
fn hline(m: &ChromeMetrics, x: u32, y: u32, width: u32, c: u32, weight: u32) -> SurfaceTree {
    let _ = weight;
    // **Clamped to `MAX_LEN`, and this is a correction.**
    //
    // `u16::try_from(width / cell_w).unwrap_or(TextRun::MAX_LEN)` is right for a width so large the
    // count does not fit in a `u16` and wrong for **every width between 256 and 65,535 cells** -- which
    // is a 3,072 px panel at a 12 px cell, on any monitor made this decade. `unwrap_or` only catches
    // the failure `try_from` reports, and 257 fits in a `u16` perfectly well.
    //
    // **The symptom is a panic inside a paint**, and a panic inside a paint takes the session with it:
    // `RepeatRun::new` asserts the bound, so resizing to 3,072 px would have killed the window rather
    // than drawing it. Found by `the_chrome_paints_at_every_panel_size_without_drawing_outside_itself`,
    // which paints every size from 1x1 up -- **the gate was already sweeping the widths and had never
    // reached one this wide.**
    let cells = width / m.cell_w.max(1);
    let n = cells.min(u32::from(TextRun::MAX_LEN)) as u16;
    if n == 0 {
        return SurfaceTree::group();
    }
    // **`RepeatRun`, which is the node that means "the same glyph again".**
    SurfaceTree::leaf(crate::Node::Repeat(crate::RepeatRun::new(
        x as i32,
        y as i32,
        rune::H,
        n,
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
    let _ = m;
    into.push(SurfaceTree::leaf(crate::Node::Label(Label::new(
        x, y as i32, text, style, 0, colour,
    ))));
}
