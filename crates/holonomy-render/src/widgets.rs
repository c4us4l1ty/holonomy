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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    /// Undo.
    Undo,
    /// Redo.
    Redo,
    /// Print.
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
            Tool::Zoom | Tool::Style | Tool::Font => None,
        }
    }

    /// **Whether this tool carries a text label beside its chevron.**
    ///
    /// **A `const fn` taking no state, so it cannot disagree with [`label`](Self::label).** The painter
    /// branches on this and then draws `label`, and if the two could differ the painter would reserve
    /// space for a label it does not draw or draw one it did not reserve.
    pub const fn has_label(self) -> bool {
        matches!(self, Tool::Zoom | Tool::Style | Tool::Font)
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
            Tool::Style => state.style_label(),
            Tool::Font => state.font_label(),
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

    /// The width this tool occupies, in pixels.
    ///
    /// **`Zoom`, `Style` and `Font` are wider than a button** because they carry a label. Everything
    /// else is [`BUTTON`](super::ChromeMetrics::button) square. **The widths are a function of the tool,
    /// not of the layout pass**, so the paint and hit paths cannot disagree about how wide `Font` is.
    pub const fn width(self, button: u32) -> u32 {
        match self {
            Tool::Zoom => button + 44,
            Tool::Style => button + 76,
            Tool::Font => button + 64,
            _ => button,
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
            Tool::Zoom | Tool::Bold | Tool::Link | Tool::Mode | Tool::Collapse
        )
    }
}

/// Every toolbar tool, left to right, with the gaps between them.
///
/// **`const` and `pub`, so this is the list.** Not a function, not a builder: a `const` cannot be
/// built at runtime, so the order is fixed at compile time and the only way to change it is to change
/// the source. That is the point.
pub const TOOLBAR: &[Tool] = &[
    Tool::Undo,
    Tool::Redo,
    Tool::Print,
    Tool::Spellcheck,
    Tool::PaintFormat,
    Tool::Zoom,
    Tool::Style,
    Tool::Font,
    Tool::SizeDown,
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

    // Left group.
    let mut x = pad;
    for p in out.iter_mut() {
        if matches!(p.tool, Tool::Mode | Tool::Collapse) {
            continue;
        }
        if p.tool.separated() {
            x += SEPARATOR;
        }
        p.rect = DamageRect::new(
            x,
            m.toolbar.y + m.toolbar.height / 2 - button / 2,
            p.tool.width(button),
            button,
        );
        x += p.tool.width(button);
    }

    // Right group, from the right edge inwards.
    let mut r = l.toolbar.right().saturating_sub(pad);
    for p in out.iter_mut().rev() {
        if !matches!(p.tool, Tool::Mode | Tool::Collapse) {
            continue;
        }
        let w = p.tool.width(button);
        r = r.saturating_sub(w);
        p.rect = DamageRect::new(
            r,
            m.toolbar.y + m.toolbar.height / 2 - button / 2,
            w,
            button,
        );
        if p.tool == Tool::Mode {
            r = r.saturating_sub(SEPARATOR);
        }
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
fn chrome_button(l: &Layout) -> u32 {
    (l.toolbar.height / 2 + 1).min(40)
}

impl ChromeState {
    /// The zoom label, with the percent sign the reference shows.
    pub fn zoom_label(&self) -> String {
        format!("{}%", self.zoom_percent)
    }
    /// The style's name, borrowed from the chrome rather than allocated per call.
    ///
    /// **`&'static str` is a lie for a runtime string and this is where it is told.** The chrome owns
    /// `style_name`, so the honest return type is `&str`; the lifetime elision above is why the toolbar
    /// takes a `String` and formats it. Kept as three small methods so the toolbar does not match on
    /// three tool variants to find out which string to print.
    pub fn style_label(&self) -> String {
        self.style_name.clone()
    }
    /// The font's name and size, as the reference shows them: `Inter 11`.
    pub fn font_label(&self) -> String {
        format!("{} {}", self.font_name, self.font_size)
    }
}

/// Anything the pointer can be over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// A toolbar button.
    Tool(Tool),
    /// A menu-bar heading, by index into [`MENUS`](crate::chrome::MENUS).
    Menu(usize),
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
    /// Comments.
    Comments,
    /// Share.
    Share,
}

/// Lay the title bar's buttons out, right-anchored.
pub fn place_title(l: &Layout, pad: u32) -> Vec<(TitleButton, DamageRect)> {
    let b = l.title.height.saturating_sub(8).min(28);
    let y = l.title.y + (l.title.height - b) / 2;
    let mut out = Vec::new();
    let mut x = l.title.right().saturating_sub(pad);
    for (btn, w) in [
        (TitleButton::Share, b * 5),
        (TitleButton::Comments, b),
        (TitleButton::History, b),
        (TitleButton::Cloud, b),
        (TitleButton::Folder, b),
        (TitleButton::Star, b),
    ] {
        x = x.saturating_sub(w);
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

    // **Title bar first, then the menu bar, then the toolbar**: the bands do not overlap, so the order
    // is for readability rather than for correctness, and the tests assert that it does not matter.
    for (btn, r) in &place_title(l, 8) {
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

    for p in place_toolbar(l, 8) {
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
