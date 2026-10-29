//! **The chrome's widget list: one list, painted and hit-tested.** Added in Phase 14 part 19.
//!
//! # Why this file exists
//!
//! The chrome had **no hit testing at all**. `Chrome::tree` laid its bands out by index, emitted a
//! `[B][M][H][u]` row of toggles, and nothing in the codebase could say "what is under the pointer" — so
//! a pointer could not be routed to a widget even if the events arrived, which is what
//! PROJECT.md's "Phase 14 readiness" note measured and recorded.
//!
//! The obvious fix is to write `hit(x, y)` alongside the layout arithmetic. **That is the trap**, and
//! it is the trap every immediate-mode UI falls into: two pieces of code that each compute where a
//! button is, which agree on the day they are written and disagree on the day the toolbar gains a
//! separator. Nothing fails. The button is drawn in one place and pressed in another.
//!
//! **So there is one list.** [`Toolbar`] is a `const` of [`Tool`]s; the layout gives each one a rect;
//! the painter draws the rect and the hit test answers with the [`Tool`]. A widget that is not in the
//! list is not drawn, and a widget that is in the list is drawn and clickable, **by construction**.
//!
//! # What this file does *not* do
//!
//! **It does not decide what a click means.** That is [`Session`](crate)'s job, and it is deliberately
//! a different type: this file answers "which widget", the session answers "what command". A menu bar
//! that opened in the renderer would put chrome policy in a crate that has no idea what a document is.
//!
//! | what it proves | test |
//! | --- | --- |
//! | every widget has a rect, inside the panel | [`every_widget_has_a_rect_inside_the_panel`] |
//! | no two widgets overlap | [`no_two_widgets_overlap`] |
//! | `hit` agrees with where it is drawn | [`hit_agrees_with_where_the_widget_was_drawn`] |
//! | and a miss is a miss | [`a_point_on_no_widget_hits_nothing`] |
//! | the hit rect is the *tappable* box, not the icon | [`a_hit_is_the_button_not_the_icon`] |

use crate::chrome::{ChromeState, Layout};
use crate::icons::IconId;
use crate::DamageRect;

/// What a toolbar button is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tool {
    /// Search the document.
    Search,
    /// Undo.
    Undo,
    /// Redo.
    Redo,
    /// Print / export PDF.
    Print,
    /// Spellcheck.
    Spellcheck,
    /// Format painter.
    PaintFormat,
    /// The zoom control: a label and a chevron.
    Zoom,
    /// The paragraph-style control: a label and a chevron.
    Style,
    /// The font control: a label and a chevron.
    Font,
    /// Decrease the size.
    SizeDown,
    /// Increase the size.
    SizeUp,
    /// The font-size box.
    FontSize,
    /// Bold.
    Bold,
    /// Italic.
    Italic,
    /// Underline.
    Underline,
    /// Text colour.
    TextColour,
    /// Highlight.
    Highlight,
    /// Link.
    Link,
    /// Comment.
    Comment,
    /// Image.
    Image,
    /// Overflow: the rest of the toolbar.
    Overflow,
    /// The mode switcher, at the right.
    Mode,
    /// Collapse, at the far right.
    Collapse,
}

impl Tool {
    /// The icon this tool draws, if it draws one.
    ///
    /// **`None` for the three labelled controls** — `Zoom`, `Style` and `Font` are a text label and a
    /// chevron, not a glyph, because a number has no meaningful 16-pixel picture. That is the whole
    /// reason those three are the exception rather than being drawn with something: an icon for "11 pt"
    /// is either the text or a lie about the text.
    pub const fn icon(self) -> Option<IconId> {
        match self {
            Tool::Search => Some(IconId::Search),
            Tool::Undo => Some(IconId::Undo),
            Tool::Redo => Some(IconId::Redo),
            Tool::Print => Some(IconId::Print),
            Tool::Spellcheck => Some(IconId::Spellcheck),
            Tool::PaintFormat => Some(IconId::PaintFormat),
            Tool::SizeDown => Some(IconId::Minus),
            Tool::SizeUp => Some(IconId::Plus),
            Tool::Bold => Some(IconId::Bold),
            Tool::Italic => Some(IconId::Italic),
            Tool::Underline => Some(IconId::Underline),
            Tool::TextColour => Some(IconId::TextColour),
            Tool::Highlight => Some(IconId::Highlight),
            Tool::Link => Some(IconId::Link),
            Tool::Comment => Some(IconId::Comment),
            Tool::Image => Some(IconId::Image),
            Tool::Overflow => Some(IconId::Overflow),
            Tool::Mode => Some(IconId::Pen),
            Tool::Collapse => Some(IconId::ChevronUp),
            Tool::Zoom | Tool::Style | Tool::Font | Tool::FontSize => None,
        }
    }

    /// **Whether this tool carries a text label beside its chevron.**
    ///
    /// **A `const fn` taking no state, so it cannot disagree with [`label`](Self::label).** The painter
    /// branches on this and then draws `label`, and if the two could differ the painter would reserve
    /// space for a label it does not draw or draw one it did not reserve.
    pub const fn has_label(self) -> bool {
        matches!(self, Tool::Zoom | Tool::Style | Tool::Font | Tool::FontSize)
    }

    /// This tool's label, if it has one.
    ///
    /// **A `String`, and that is the cost of the three labelled controls reading from `state`.**
    /// The zoom label is formatted, so it cannot be a `&'static str`, and one `String` per paint for
    /// three widgets is cheaper than a small-string cache and far easier to read. The eighteen icon
    /// tools get an empty `String`, which the painter never asks for because `has_label` gates it.
    pub fn label(self, state: &ChromeState) -> String {
        match self {
            Tool::Zoom => state.zoom_label(),
            Tool::Style => state.style_name().to_string(),
            Tool::Font => state.font_name().to_string(),
            Tool::FontSize => ChromeState::FONT_SIZE.to_string(),
            // **An empty `String` rather than a panic.** `label` is called from the painter inside a
            // `match` that has already established `has_label`, so this arm is unreachable in the
            // paint path; it is here because `label` is a total function over `Tool` and a total
            // function cannot fall off the end of one arm. **An allocation per call**, which the
            // painter does only for the three labelled controls -- see `Tool::has_label`.
            _ => String::new(),
        }
    }

    /// A name for the status bar, a log line, and the tests' failure messages.
    pub const fn name(self) -> &'static str {
        match self {
            Tool::Search => "search",
            Tool::Undo => "undo",
            Tool::Redo => "redo",
            Tool::Print => "print",
            Tool::Spellcheck => "spellcheck",
            Tool::PaintFormat => "paint-format",
            Tool::Zoom => "zoom",
            Tool::Style => "style",
            Tool::Font => "font",
            Tool::SizeDown => "size-down",
            Tool::SizeUp => "size-up",
            Tool::FontSize => "font-size",
            Tool::Bold => "bold",
            Tool::Italic => "italic",
            Tool::Underline => "underline",
            Tool::TextColour => "text-colour",
            Tool::Highlight => "highlight",
            Tool::Link => "link",
            Tool::Comment => "comment",
            Tool::Image => "image",
            Tool::Overflow => "overflow",
            Tool::Mode => "mode",
            Tool::Collapse => "collapse",
        }
    }

    /// **Whether this tool opens a dropdown.**
    ///
    /// **A property of the tool, not of whether a dropdown happens to exist for it**, so a caller
    /// asking "does this button open something?" gets an answer from the same list that decides where
    /// the button is. `menus::dropdown_items` is the authority on which tools have rows; this is the
    /// cheap answer, and the gate `the_two_dropdown_lists_agree` is what stops them drifting apart.
    /// **`Tool::TextColour` joined them in part 29**, which took the toolbar from three dropdowns and
    /// twelve inert buttons to four and eleven.
    ///
    /// **The chevron is drawn from this and not from a separate list**, which is the whole point of
    /// asking the tool: a button drawn with a chevron that the hit test does not know about is the
    /// "drawn one way, clicked another" bug this file exists to prevent.
    ///
    /// # Part 44: `Mode` and `Overflow` joined, and one of them is a different kind of answer
    ///
    /// **A tool opens a dropdown when `menus::dropdown_items` has rows for it.** That is a compile-time
    /// question for `Zoom`, `Style`, `Font`, `TextColour` and `Mode` -- their rows are `&'static`
    /// tables -- and a *runtime* one for `Overflow`, whose rows are the tools that did not fit and are
    /// therefore empty on a panel wide enough to hold all twenty.
    ///
    /// **`Overflow` is in the list anyway, and the emptiness check lives in the caller.** **This is
    /// the shape of the bug worth recording**: part 44 added `Overflow`'s dropdown *rows* and drew its
    /// chevron conditionally and wrote the session's emptiness check, and **left this predicate
    /// untouched** -- so the button looked right, had rows, and on a narrow panel did nothing.
    /// `the_overflow_menu_lists_the_tools_that_did_not_fit` failed with `pointer_chrome == 1` and
    /// `open == None`, which is the exact signature of "the emptiness branch ran instead of the
    /// dropdown branch".
    ///
    /// **So `has_dropdown` answers "is this button a menu?" and the session answers "is there
    /// anything in it?"** Two questions, two places, and the reason they are not one is that only one
    /// of them depends on the window.
    pub const fn has_dropdown(self) -> bool {
        matches!(
            self,
            Tool::Zoom
                | Tool::Style
                | Tool::Font
                | Tool::TextColour
                | Tool::Mode
                | Tool::Overflow
        )
    }

    /// How many cells this tool's **label** needs, or `0` for an icon.
    ///
    /// **CORRECTION, part 36: these were 44, 76 and 64 pixels, and a pixel is not a length here.**
    ///
    /// **Measured against the labels themselves:** `Zoom` shows `zoom_label()`, whose widest value is
    /// `"400%"` at 4 codepoints; `Style` shows [`ChromeState::style_name`], whose longest entry in
    /// [`STYLES`](crate::menus::STYLES) is `"Normal text"` at 11; `Font` shows `font_label()`, whose
    /// longest entry in [`FONTS`](crate::menus::FONTS) is `"JetBrains Mono"` at 14 — longer than the
    /// 10 cells the button holds, exactly as the previous list's `"IBM Plex Mono"` was, and fitted
    /// the same way every over-long label is.
    ///
    /// **The old numbers did not fit at 16 ppem either** — 76 px is 7.6 cells of an 11-character label
    /// — and part 32's 24 ppem made it a visible overlap: two toolbar items drawn on top of each other,
    /// visible in a dump of the running window as `tēnter" 🗌`.
    ///
    /// **In cells, and therefore measured against the same face the painter uses**, which is part 30's
    /// decision applied to the one place it had been left out.
    pub const fn label_cells(self) -> u32 {
        match self {
            Tool::Zoom => 4,
            Tool::Style => 11,
            Tool::Font => 10,
            Tool::FontSize => 2,
            _ => 0,
        }
    }

    /// The width this tool occupies, in pixels.
    ///
    /// **`Zoom`, `Style` and `Font` are wider than a button** because they carry a label: the button,
    /// plus the label in [`label_cells`](Self::label_cells) cells of `cell_w`, plus four pixels of
    /// inner padding. Everything else is [`BUTTON`](super::ChromeMetrics::button) square.
    ///
    /// **The widths are a function of the tool and the cell, not of the layout pass**, so the paint and
    /// hit paths cannot disagree about how wide `Font` is.
    ///
    /// **The four pixels are chosen so that `Zoom` is unchanged at the 16 ppem design** — 4 cells of 10
    /// plus 4 is 44, which is what it always was — so the one control that fitted is not resized by the
    /// correction.
    pub const fn width(self, button: u32, cell_w: u32) -> u32 {
        match self.label_cells() {
            0 => button,
            // **`Ord::max` is not yet stable as a `const` trait**, the same reason `measure_beyond`
            // writes its clamp out. A zero cell would make a labelled control zero wide, which is a
            // different way of not fitting.
            n => button + n * (if cell_w == 0 { 1 } else { cell_w }) + 4,
        }
    }

    /// **Whether a separator is drawn before this tool.**
    ///
    /// A toolbar without separators is a row of 21 identical boxes, and the eye has to work to find
    /// the groups. The reference's separators are cheap and this is where they are recorded, so that a
    /// tool added to the middle of a group gets its separator by position rather than by remembering.
    pub const fn separated(self) -> bool {
        matches!(
            self,
            Tool::Print | Tool::Zoom | Tool::Bold | Tool::Link | Tool::Mode | Tool::Collapse
        )
    }
}

/// Every toolbar tool, left to right, with the gaps between them.
///
/// **`const` and `pub`, so this is the list.** Not a function, not a builder: a `const` cannot be
/// built at runtime, so the order is fixed at compile time and the only way to change it is to change
/// the source. That is the point.
pub const TOOLBAR: &[Tool] = &[
    Tool::Search,
    Tool::Undo,
    Tool::Redo,
    Tool::Print,
    Tool::Spellcheck,
    Tool::PaintFormat,
    Tool::Zoom,
    Tool::Style,
    Tool::Font,
    Tool::SizeDown,
    Tool::FontSize,
    Tool::SizeUp,
    Tool::Bold,
    Tool::Italic,
    Tool::Underline,
    Tool::TextColour,
    Tool::Highlight,
    Tool::Link,
    Tool::Comment,
    Tool::Image,
    Tool::Overflow,
    Tool::Mode,
    Tool::Collapse,
];

/// One laid-out widget: which tool, and where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    /// Which tool this is.
    pub tool: Tool,
    /// Where its tappable box is.
    pub rect: DamageRect,
}

/// Lay the toolbar out across `l.toolbar`, inset by `pad`.
///
/// **Right-anchored tools are placed from the right and everything else from the left**, because
/// `Mode` and `Collapse` belong to the right-hand end of the bar in the reference and a single
/// left-to-right pass would push them against the toolbar's left edge with a hundred pixels of dead
/// space in the middle. **`TOOLBAR` is still one list** — the anchor is a property of the tool, not of
/// the loop.
/// **The toolbar's horizontal padding, in pixels.** Part 44.
///
/// **A `const` and not the literal `8`, which was at five call sites.** Two of those sites -- the
/// painter's and the hit test's -- have to agree with the third, `Session::refresh_overflow`, which
/// asks the same question for a different consumer. **Three questions about the same row with the
/// same number written at each is three numbers that can drift**, and the drift would show as a tool
/// that is clickable on the toolbar and absent from its own overflow menu.
///
/// **It is a parameter of [`place_toolbar`] and not a field of `Layout`**, so a caller that genuinely
/// wants a different inset can pass one; this constant is what the project uses.
pub const TOOLBAR_PAD: u32 = 8;

/// **The title bar's right inset, in pixels.** The painter and the hit test must agree on it.
///
/// **It is a constant and not a literal at each of the three call sites because the two answers
/// disagreed.** The painter placed the buttons with an inset of 12 and the hit test with an inset
/// of 8, so every title-bar button was drawn four pixels to the left of where it was clicked:
/// the leftmost four pixels of a button were dead, and the four pixels just to its right
/// activated it.
///
/// **That is the exact failure this module exists to prevent** — "a widget is drawn in one place
/// and pressed in another" — and it happened because the number was written twice and one of the
/// two was wrong. One constant, three call sites, no drift.
pub const TITLE_PAD: u32 = 12;

pub fn place_toolbar(l: &Layout, pad: u32) -> Vec<Placed> {
    let m = &l;
    let button = chrome_button(m);
    let mut out: Vec<Placed> = TOOLBAR
        .iter()
        .map(|&t| Placed {
            tool: t,
            rect: DamageRect::new(0, 0, 0, 0),
        })
        .collect();

    // # CORRECTION 2026-10-12: every rect this returns is clamped into the panel
    //
    // `Layout` clamps its own bands and the menu bar now elides (`chrome::menu_boxes`), **but the
    // toolbar did neither** — and `bugs/1.png` is what it produced: a 480 px panel with toolbar icons
    // running off the right edge, which is one of the two symptoms in the part-36 report that was
    // attributed to the type scale.
    //
    // Two separate faults, both invisible while `ChromeMetrics::for_size` clamped the panel up to
    // `MIN_WIDTH` x `MIN_HEIGHT`:
    //
    // * **The vertical placement was a bare subtraction.** `toolbar.y + toolbar.height / 2 - button / 2`
    //   is `u32` arithmetic with no `saturating_` on it. At `opt-level=0` that is a panic and in
    //   release it is a four-billion-pixel offset, both of which are a button drawn at the top-left of
    //   the screen. It is `saturating_sub` now, which is what the four bands already did.
    // * **The left group had no budget.** `x` started at `pad` and grew by every tool's width with
    //   nothing stopping it, so in a narrow panel the row ran off the right edge and kept going.
    //
    // **Clamping rather than skipping is deliberate.** A rect clamped into the panel is still a rect
    // the hit test can see, so the button stays clickable where the user can see it; skipping would
    // leave a gap in the toolbar that looks like a missing feature.
    let panel_w = m.width;
    let panel_h = m.height;
    let fit = |x: u32, y: u32, w: u32, h: u32| -> DamageRect {
        let y = y.min(panel_h);
        let x = x.min(panel_w);
        DamageRect::new(x, y, w.min(panel_w - x), h.min(panel_h - y))
    };

    // ## How much room the right-hand group needs, measured *before* the left group is laid out
    //
    // **CORRECTION, part 44: the left group had no budget at all.** Its only stopping condition was
    // `x >= panel_w`, and the right-hand group was placed from the right afterwards — so on a 480 px
    // panel the left group ran to the edge *and* the right-hand group sat on top of it. Two
    // consequences, and the second is the one that is not visible:
    //
    // - **the last left-hand buttons were painted over**, which is a screenshot;
    // - **and the hit test disagreed with the picture.** `widgets::hit` walks `place_toolbar`'s
    //   output in `TOOLBAR` order and returns the *first* rect containing the point, so a point in
    //   the overlap resolved to whichever of the two came first in `TOOLBAR` — which is the left-hand
    //   one. `the_overflow_menu_lists_the_tools_that_did_not_fit` failed with `open == Some(Style)`
    //   while pressing `Overflow`: **the press landed on a button the user was not looking at.**
    //
    // **So the right group's width is reserved first and the left group is given what is left.**
    // Which of the two loses is not arbitrary: the right-hand group wins, because it holds the
    // overflow menu, and **a menu that cannot be reached is worse than a toolbar with a gap in it.**
    let right_w = [Tool::Collapse, Tool::Mode, Tool::Overflow]
        .iter()
        .fold(SEPARATOR, |acc, &t| {
            acc.saturating_add(t.width(button, m.cell_w))
        });
    let left_limit = m
        .width
        .saturating_sub(pad)
        .saturating_sub(right_w)
        // **The right-hand group never starts before `pad`,** so a panel narrower than the group
        // plus its own inset cannot produce a negative budget — and `saturating_sub` here is what
        // makes `x >= left_limit` true for every remaining tool rather than a wrap.
        .max(pad);

    // Left group.
    let mut x = pad;
    for p in out.iter_mut() {
        // **`Overflow` is in the right-hand group too, and that is a correction.** Part 44: it was
        // laid out with the left group, and the left group's rule is *"a tool with no room left gets
        // a zero-width rect"* — so at 480 px `Overflow` was dropped along with everything after it,
        // **and the menu that exists to reach the tools that did not fit could not itself be
        // reached.** `the_overflow_menu_lists_the_tools_that_did_not_fit` failed on its first run
        // with `open == None` for exactly that reason.
        //
        // **A menu button that is dropped before the items it lists is worse than no menu button**,
        // because the user sees a toolbar with a gap in it and no way to find out what went missing.
        // So it is placed from the right, with `Mode` and `Collapse`.
        if matches!(p.tool, Tool::Mode | Tool::Collapse | Tool::Overflow) {
            continue;
        }
        if p.tool.separated() {
            x += SEPARATOR;
        }
        // **A tool that does not *fit* gets a zero-width rect**, which `DamageRect::contains` answers
        // `false` for at every point — so a hidden tool is also an unclickable one, and the row does
        // not accumulate invisible targets past the edge.
        //
        // ## `x + w`, and not `x`
        //
        // **The original test was `x >= panel_w`, which asks whether the button's *left* edge has
        // passed the limit** — and a 136 px button whose left edge is 36 px inside the limit can still
        // stick 100 px out of it. Part 44's reservation made that visible at 480 px: `font` starts at
        // 313 with a `left_limit` of 379 and ends at 449, **drawing over the whole right-hand group**,
        // and the hit test — which takes the first match in `TOOLBAR` order — resolved a press on
        // `Overflow` to `Style`.
        //
        // **So the test is `x + w > left_limit`, and it is `>` rather than `>=` because a button that
        // ends exactly on the line has fit.**
        let w = p.tool.width(button, m.cell_w);
        if x.saturating_add(w) > left_limit || x.saturating_add(w) > panel_w {
            p.rect = DamageRect::new(0, 0, 0, 0);
            continue;
        }
        p.rect = fit(
            x,
            m.toolbar.y
                .saturating_add(m.toolbar.height / 2)
                .saturating_sub(button / 2),
            w,
            button,
        );
        x = x.saturating_add(w);
    }

    // Right group, from the right edge inwards: Collapse, then Mode, then Overflow.
    //
    // **In that order**, because `iter_mut().rev()` walks `TOOLBAR` backwards and `Collapse` is last
    // in it -- so Collapse is placed first at the rightmost slot and each of the others lands to its
    // left. **Overflow is before Mode in `TOOLBAR`, so it lands to the left of Mode**, which puts it
    // next to the left-hand group where it belongs and next to the gap it will fill.
    let mut r = l.toolbar.right().saturating_sub(pad);
    for p in out.iter_mut().rev() {
        if !matches!(p.tool, Tool::Mode | Tool::Collapse | Tool::Overflow) {
            continue;
        }
        let w = p.tool.width(button, m.cell_w);
        r = r.saturating_sub(w);
        // **The separator follows `Mode` and precedes `Overflow`**, which is where the reference puts
        // it: the left-hand group, then a gap, then the two controls that act on the window rather
        // than on the text.
        if p.tool == Tool::Mode {
            r = r.saturating_sub(SEPARATOR);
        }
        p.rect = fit(
            r,
            m.toolbar.y
                .saturating_add(m.toolbar.height / 2)
                .saturating_sub(button / 2),
            w,
            button,
        );
    }
    out
}

/// The gap a separator occupies, in pixels.
pub const SEPARATOR: u32 = 9;

/// The button size, reached from a [`Layout`] without also threading a [`ChromeMetrics`].
///
/// **`Layout` does not keep a back-pointer to its metrics**, and adding one for this would make every
/// `Layout` two words larger for the benefit of one function. The button is derived from the toolbar's
/// own height, which is the honest source: **the button is as tall as the toolbar's content box.**
///
/// # CORRECTION, part 39: `toolbar.height / 2 + 1`, and a 38 px band gave a 20 px button
///
/// **`icons::SIZE` is 16**, so the button was **2 px of padding on each side of its icon** and the row
/// read as a strip of glyphs set shoulder to shoulder rather than as buttons. Measured in the running
/// window at 20 ppem: four icons in 80 px, each 16 of it ink.
///
/// **It is now the band's height less [`BUTTON_INSET`]**, which is 6: **32 px of button for 16 px of
/// icon**, so a pressed or hovered button has somewhere to be seen, and the reference's proportion —
/// an icon at about half its button — is roughly what this gives. **The cap is the old one, 40**, so a
/// scaled-up toolbar cannot grow buttons without bound.
///
/// **The padding is a constant rather than a fraction because the icon is not.** The icon is 16 px at
/// every type scale, so a button that is a *fraction* of the band gets tighter as the type grows, which
/// is the opposite of what the band growing was for.
fn chrome_button(l: &Layout) -> u32 {
    l.toolbar.height.saturating_sub(BUTTON_INSET).clamp(16, 40)
}

/// How much of the toolbar's height a button leaves empty above and below, in total.
///
/// **Six, so three on each side** — see [`chrome_button`] for the measurement this replaces.
const BUTTON_INSET: u32 = 6;

impl ChromeState {
    /// The current paragraph style's name, borrowed from the list rather than owned by the state.
    ///
    /// **A `&str` out of a `const`, so this allocates nothing.** Part 19 stored `style_name: String`
    /// and had three `format!`s on the paint path for the toolbar's labels; part 21 deleted both
    /// fields and made every label a borrow. **The paint path's zero-allocation claim is about the
    /// frame, and this is the frame.**
    pub fn style_name(&self) -> &'static str {
        crate::menus::STYLES
            .get(self.style_index)
            .copied()
            .unwrap_or("Normal text")
    }

    /// **The current mode's name.** Part 44. See [`style_name`](Self::style_name).
    ///
    /// **`.unwrap_or(MODES[0].0)` and not a panic**, for the reason that gate records: a state file
    /// written by a build with more modes must not take the session down.
    #[must_use]
    pub fn mode_name(&self) -> &'static str {
        crate::menus::MODES
            .get(usize::from(self.mode_index))
            .map_or(crate::menus::MODES[0].0, |&(label, _)| label)
    }

    /// **The current font's name. See [`style_name`](Self::style_name).
    pub fn font_name(&self) -> &'static str {
        crate::menus::FONTS
            .get(self.font_index)
            .copied()
            .unwrap_or("Inter")
    }

    /// The current font size, as the toolbar shows it beside the face.
    ///
    /// **A plain `11`, and the font size is not a state field.** Part 19 had `font_size: u32` and the
    /// toolbar showed `"Inter 11"`; there is no operation in this build that changes a size, so a
    /// field for it is a number nothing can move. **`Action::SetFont` changes the face, not the
    /// size**, and the day a size control exists this becomes a field again.
    pub const FONT_SIZE: u32 = 11;

    /// The font control's label: the face and the size, as the reference shows them.
    pub fn font_label(&self) -> String {
        format!("{} {}", self.font_name(), Self::FONT_SIZE)
    }

    /// The zoom control's label.
    pub fn zoom_label(&self) -> String {
        format!("{}%", self.zoom_percent)
    }
}

/// Which popup is open, if any.
///
/// # Why this is one value and not two
///
/// **Part 20 had `ChromeState::open_menu: Option<usize>`, and part 21 needed a second kind of popup**
/// — the zoom, style and font dropdowns on the toolbar. The obvious move was a second `Option`, and the
/// obvious result was a state in which a menu *and* a dropdown were both open, two overlays drawn on
/// top of each other, and a hit test with no rule about which one the pointer meant.
///
/// **So it is one enum, and that is the whole argument for it:** there is at most one popup, and the
/// type says so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Open {
    /// A menu-bar heading's popup, by index into [`MENUS`](crate::chrome::MENUS).
    Menu(usize),
    /// A labelled toolbar control's dropdown.
    Tool(Tool),
}

impl Open {
    /// The popup's rows for `state`, or an empty slice when there are none.
    ///
    /// **The one place "what is in this popup" is decided**, for the same reason
    /// [`crate::menus::items_for`] is for the menus. A caller that asked `Tool::Zoom` and a caller that
    /// asked `Open::Menu(0)` get different things, and neither of them knows how the other is spelled.
    pub fn items(self, state: &ChromeState) -> Vec<crate::menus::Item> {
        match self {
            Open::Menu(i) => crate::menus::items_for(i).to_vec(),
            Open::Tool(t) => crate::menus::dropdown_items(t, state),
        }
    }
}

/// An open popup: the panel and one row per item.
///
/// # Why this is a function and not painted inline
///
/// Because the painter and the hit test both need it, and **a widget drawn one way and clicked
/// another is the single most expensive bug in a UI** — the symptom is "the button is there but
/// clicking it does nothing", which reads as a routing problem and is a geometry problem. The
/// discipline is the one the whole of this file exists for: **one list, painted and hit-tested.**
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Popup {
    /// The popup's panel.
    pub rect: DamageRect,
    /// One row per item, in [`crate::menus::MENUS`] order.
    pub rows: Vec<DamageRect>,
    /// Which popup this is.
    pub open: Open,
}

impl Popup {
    /// The row at `(x, y)`, if the point is in one.
    ///
    /// **The rows, not `rect`.** A popup's panel has padding, and a click on the padding is a click on
    /// nothing — so testing `rect` would make a strip of every popup a clickable row.
    pub fn row_at(&self, x: i32, y: i32) -> Option<usize> {
        self.rows.iter().position(|r| r.contains(x, y))
    }
}

/// Where an open popup is, and its rows.
///
/// **`None` when there is no popup or it has no rows**, which is what makes the caller's job total: a
/// bad index is a paint-time mistake in a caller and the answer is "draw nothing", not "panic inside
/// a paint". **A panic in a paint takes the session with it.**
#[must_use]
pub fn popup(l: &Layout, state: &ChromeState, open: Option<Open>) -> Option<Popup> {
    let open = open?;
    let items = open.items(state);
    if items.is_empty() {
        return None;
    }
    // **A menu hangs from its heading; a dropdown hangs from its button.** That is the whole of the
    // difference between the two, and it is why this is an `Open` rather than two functions — the
    // rows differ, the clamp is the same, and the anchor is one line.
    let anchor_x = match open {
        Open::Menu(index) => crate::chrome::menu_boxes(l).1.get(index)?.x,
        Open::Tool(tool) => {
            place_toolbar(l, TOOLBAR_PAD)
                .into_iter()
                .find(|p| p.tool == tool)?
                .rect
                .x
        }
    };
    let anchor_y = match open {
        Open::Menu(_) => l.menubar.bottom(),
        Open::Tool(_) => l.toolbar.bottom(),
    };

    let pad = 2 * l.cell_w;
    // **The width is computed over the whole row, and the whole row is more than the label.**
    //
    // # CORRECTION, part 39: the accelerator was not in it, and a menu row is not a label
    //
    // ```text
    // | tick |   icon |  Save  .......  Ctrl+S |     <- what is drawn
    // | tick |   icon |  Save  |                 <- what the panel was sized for
    // ```
    //
    // **Measured in the running window, at 20 ppem:** `Save` ends at x = 144 and `Ctrl+S` starts at
    // x = 143, so the two are drawn **on top of each other** and the row reads `SaveCtrl+S`. The same
    // arithmetic put `Ctrl+O` through the middle of `Page Setup`. **Nothing clipped and nothing
    // overflowed** -- each label fitted the panel on its own, so every gate that checked "is this text
    // inside the popup" was green while the two texts were inside each other.
    //
    // **The rule is that a row is `leading + label + gap + accelerator`,** and `leading` is a constant
    // four cells whether or not the row has a tick and an icon, because the painter puts the tick in
    // column 0, the icon in column 2 and the label in column 4 -- it does not move the label left to
    // fill a gap. The old formula counted the tick and the icon *conditionally* and so disagreed with
    // the painter by up to two cells as well.
    let widest = items
        .iter()
        .map(|it| row_cells(it, l.cell_w))
        .max()
        .unwrap_or(40)
        .max(14 * l.cell_w);
    let row_h = l.cell_h + 6;
    let w = widest + pad * 2;

    // **Clamped to the panel, not to the anchor.** `Extensions` is the longest heading and its popup
    // is 336 px wide; anchored without a clamp it runs off the right edge and the last two items are
    // drawn into the scrollbar. The clamp is on the panel because that is where the pixels stop.
    //
    // **And the height is clamped too, which it was not.** `y` was `min(height - h).max(0)`, so a popup
    // taller than the panel had its *top* pushed up to 0 and its rows — which were laid out from the
    // clamped top — ran off the bottom of the window, where they were drawn and clipped by the frame.
    // **The rows are now the thing that is cut**, so a row that is on screen is a row that can be
    // clicked, which is the one invariant `Popup` exists to keep.
    let y = anchor_y.min(l.height.saturating_sub(1));
    let room = l.height.saturating_sub(y).saturating_sub(pad);
    let fits = usize::try_from(room / row_h.max(1)).unwrap_or(0);
    let shown = items.len().min(fits);
    let x = (anchor_x as i32).min(l.width as i32 - w as i32).max(0);
    let h = ((shown as u32).saturating_mul(row_h) + pad).min(l.height.saturating_sub(y));

    let rows = (0..shown)
        .map(|i| {
            // **All `u32`, and the arithmetic cannot underflow because `shown` was bounded by the room
            // above**: `pad + shown * row_h <= room`, so the last row's bottom is inside the panel.
            DamageRect::new(
                x as u32,
                y + pad + (i as u32).saturating_mul(row_h),
                w,
                row_h,
            )
        })
        .collect();
    Some(Popup {
        // **`x as u32` and `y` as they are.** `x` is the toolbar's left-to-right accumulator and is
        // `i32` so the `min(panel_w)` clamp above it can go negative without wrapping; this is where it
        // becomes a geometry again, and a `u32` accumulator would need that clamp rewritten as a
        // subtraction -- which is the `opt-level=0` panic `place_toolbar`'s own comment records.
        // **`y` is already a `u32`**, computed from the toolbar band's own `y`.
        rect: DamageRect::new(x as u32, y, w, h),
        rows,
        open,
    })
}

/// How many cells one popup row needs: `leading + label + gap + accelerator`.
///
/// **The one definition of a popup row's width.** `widgets::popup` sizes the panel with it and
/// `chrome::paint_popup` cuts a label with it, so a label can never be drawn wider than the room the
/// panel was built for -- which is the defect part 39 corrected, where each side had its own arithmetic
/// and the label fitted the panel while the accelerator did not.
///
/// * **leading, four cells: tick in column 0, icon in column 2, label in column 4.** Constant,
///   because a row without a tick still starts its label in column 4: the columns are *reserved*, not
///   packed, which is what makes every label in a popup line up with every other.
/// * **gap, two cells,** so a label and the accelerator beside it do not read as one word.
/// * **accelerator, its own length, or nothing** -- a row with no accelerator is not padded out to
///   make one.
pub fn row_cells(it: &crate::menus::Item, cell_w: u32) -> u32 {
    const LEADING: u32 = 4;
    const GAP: u32 = 2;
    // **`as_deref()` removed and not replaced.** `Item::accelerator` is `Option<&'static str>`, so
    // `as_deref()` asks for `Option<&str>` from a value that is already one -- **the deref was a no-op
    // that read as a conversion**, which is how a reader comes to believe the field is an owned `String`
    // somewhere below.
    let acc = it.accelerator.unwrap_or("");
    (LEADING + it.label.chars().count() as u32 + GAP + acc.chars().count() as u32) * cell_w
}

/// The rect a [`Hit`] is drawn at, or `None` if it is not drawn.
///
/// # Why the inverse lookup exists
///
/// **Because a hover highlight has to be invalidated when the pointer leaves, and the only way to know
/// where it was drawn is to ask the same code that drew it.** The alternative -- repaint the whole
/// panel on every mouse movement -- is a million pixels per event at 125 Hz, which is 128 megapixels
/// a second on a machine whose entire premise is that it does not do that.
///
/// # It is a search, and that is the honest cost
///
/// `Hit` has eight arms and no rect, so this walks the widget list looking for the one that matches.
/// **Eight comparisons on a mouse move, against a million-pixel repaint.** The reverse -- storing the
/// rect in `Hit` -- would make it O(1) at the cost of making `Hit` non-`Copy` and four words wide,
/// which every `hover == Some(h)` comparison in the painters would then pay for. **The search is the
/// cheaper mistake.**
pub fn hit_rect(l: &Layout, state: &ChromeState, h: Hit) -> Option<DamageRect> {
    match h {
        Hit::Tool(t) => place_toolbar(l, TOOLBAR_PAD)
            .into_iter()
            .find(|p| p.tool == t)
            .map(|p| p.rect),
        Hit::Title(b) => place_title(l, TITLE_PAD)
            .into_iter()
            .find(|(k, _)| *k == b)
            .map(|(_, r)| r),
        Hit::Menu(i) => crate::chrome::menu_boxes(l).1.get(i).copied(),
        Hit::MenuItem { open, row } => popup(l, state, Some(open))?.rows.get(row).copied(),
        Hit::Doc(i) => l.sidebar_doc(i),
        Hit::NewDoc => state.sidebar_open.then_some(l.sidebar_new),
        Hit::SidebarBack => state.sidebar_open.then_some(l.sidebar_back),
        // **A widget that is not drawn has no rect.** `Page` is the body text -- a hover there would
        // mean outlining the whole page, which is not an affordance any reference has -- and `Dismiss`
        // is the chrome's own chevron, drawn as part of the collapse button rather than on its own.
        // Returning `None` means a pointer moving over the page damages nothing, which is correct:
        // nothing changed.
        Hit::Page | Hit::Dismiss | Hit::None => None,
    }
}

/// Anything the pointer can be over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// A toolbar button.
    Tool(Tool),
    /// A menu-bar heading, by index into [`MENUS`](crate::chrome::MENUS).
    Menu(usize),
    /// A row of an open popup: which popup, which row.
    MenuItem {
        /// Which popup is open.
        open: Open,
        /// Row within that popup's items.
        row: usize,
    },
    /// A title-bar button.
    Title(TitleButton),
    /// A document in the sidebar, by index.
    Doc(usize),
    /// The "+" in the sidebar's header.
    NewDoc,
    /// The sidebar's back arrow.
    SidebarBack,
    /// The toolbar's dismiss affordance, i.e. the chevron on the collapse button.
    Dismiss,
    /// The page's text column: the caret goes here.
    Page,
    /// Nothing. **A miss is a value, not an error**, because "the pointer is over the ruler" is a
    /// perfectly ordinary place to be and has to be answerable.
    None,
}

/// A title-bar button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleButton {
    /// Star the document.
    Star,
    /// Move it.
    Folder,
    /// The save state: cloud.
    Cloud,
    /// Version history.
    History,
    /// Share / export.
    Share,
    /// Upgrade / local badge.
    Upgrade,
    /// Comments.
    Comments,
    /// Sparkle.
    Sparkle,
}

/// Lay the title bar's buttons out, right-anchored.
pub fn place_title(l: &Layout, pad: u32) -> Vec<(TitleButton, DamageRect)> {
    let b = l.title.height.saturating_sub(8).min(32);
    let y = l.title.y + (l.title.height - b) / 2;
    let mut out = Vec::new();
    // Left group: star, folder, cloud, just after the document title.
    let mut x = l.cell_w.saturating_mul(18);
    for btn in [TitleButton::Star, TitleButton::Folder, TitleButton::Cloud] {
        out.push((btn, DamageRect::new(x, y, b, b)));
        x = x.saturating_add(b + 4);
    }
    // Right group: sparkle, upgrade, share, comments, history.
    let mut x = l.title.right().saturating_sub(pad);
    for (btn, w) in [
        (TitleButton::Sparkle, b),
        (TitleButton::Upgrade, b * 4),
        (TitleButton::Share, b * 4),
        (TitleButton::Comments, b),
        (TitleButton::History, b),
    ] {
        x = x.saturating_sub(w + 4);
        out.push((btn, DamageRect::new(x, y, w, b)));
    }
    out
}

/// What the pointer is over, at `(x, y)`.
///
/// **`None` rather than a `Result`.** A pointer can be over the ruler, the gutter, the scrollbar --
/// all ordinary places with no widget -- and modelling that as an error would mean every caller
/// handles a case that is the normal case.
pub fn hit(l: &Layout, state: &ChromeState, x: i32, y: i32) -> Hit {
    let at = |r: &DamageRect| r.contains(x, y);

    // **The open popup is tested first, and it is the only thing that can be tested first.**
    //
    // A popup is an overlay: it covers the toolbar and part of the sidebar. If the bands were tested
    // in their paint order the pointer over a popup's rows would come back as `Hit::Tool` for
    // whatever was underneath, and the menu would be unclickable while looking perfectly clickable.
    //
    // **This is also the grab.** A click that lands on a popup row chooses it, and a click that lands
    // anywhere else — *including on the widget underneath* — falls through and is dismissed by the
    // session. The fall-through is deliberate and is what every native menu does: clicking the
    // toolbar button you used to dismiss the menu must not also press that button.
    if let Some(pop) = popup(l, state, state.open) {
        if let Some(row) = pop.row_at(x, y) {
            return Hit::MenuItem {
                open: pop.open,
                row,
            };
        }
    }

    // **Title bar first, then the menu bar, then the toolbar**: the bands do not overlap, so the order
    // is for readability rather than for correctness, and the tests assert that it does not matter.
    for (btn, r) in &place_title(l, TITLE_PAD) {
        if at(r) {
            return Hit::Title(*btn);
        }
    }
    if at(&l.title) {
        return Hit::None;
    }

    for (i, r) in crate::chrome::menu_boxes(l).1.iter().enumerate() {
        if r.contains(x, y) {
            return Hit::Menu(i);
        }
    }
    if l.menubar.contains(x, y) {
        return Hit::None;
    }

    for p in place_toolbar(l, TOOLBAR_PAD) {
        if at(&p.rect) {
            return Hit::Tool(p.tool);
        }
    }
    if l.toolbar.contains(x, y) {
        return Hit::None;
    }

    if state.sidebar_open {
        if at(&l.sidebar_back) {
            return Hit::SidebarBack;
        }
        if at(&l.sidebar_new) {
            return Hit::NewDoc;
        }
        for i in 0..l.sidebar_rows as usize {
            if let Some(r) = l.sidebar_doc(i) {
                if r.contains(x, y) {
                    return Hit::Doc(i);
                }
            }
        }
        if at(&l.sidebar) {
            return Hit::None;
        }
    }

    if at(&l.page) {
        Hit::Page
    } else {
        Hit::None
    }
}
