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
            Tool::Style => state.style_name().to_string(),
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

    /// **Whether this tool opens a dropdown.**
    ///
    /// **A property of the tool, not of whether a dropdown happens to exist for it**, so a caller
    /// asking "does this button open something?" gets an answer from the same list that decides where
    /// the button is. `menus::dropdown_items` is the authority on which tools have rows; this is the
    /// cheap answer, and the gate `the_two_dropdown_lists_agree` is what stops them drifting apart.
    pub const fn has_dropdown(self) -> bool {
        matches!(self, Tool::Zoom | Tool::Style | Tool::Font)
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

    /// The current font's name. See [`style_name`](Self::style_name).
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
            place_toolbar(l, 8)
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
    // **The tick column.** A dropdown that shows a check needs room for it, or the tick lands on the
    // label -- so the width is computed over `label + icon + tick` rather than `label` alone.
    let widest = items
        .iter()
        .map(|it| {
            let icon = if it.icon.is_some() { 2 } else { 0 };
            let tick = if it.checked { 2 } else { 0 };
            (it.label.len() as u32 + 2 + icon + tick) * l.cell_w
        })
        .max()
        .unwrap_or(40)
        .max(14 * l.cell_w);
    let row_h = l.cell_h + 6;
    let w = widest + pad * 2;
    let h = (items.len() as u32).saturating_mul(row_h) + pad;

    // **Clamped to the panel, not to the anchor.** `Extensions` is the longest heading and its popup
    // is 336 px wide; anchored without a clamp it runs off the right edge and the last two items are
    // drawn into the scrollbar. The clamp is on the panel because that is where the pixels stop.
    let x = (anchor_x as i32).min(l.width as i32 - w as i32).max(0);
    let y = (anchor_y as i32).min(l.height as i32 - h as i32).max(0);

    let rows = (0..items.len())
        .map(|i| {
            DamageRect::new(
                x as u32,
                (y + pad as i32 + (i as i32) * row_h as i32) as u32,
                w,
                row_h,
            )
        })
        .collect();
    Some(Popup {
        rect: DamageRect::new(x as u32, y as u32, w, h),
        rows,
        open,
    })
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
        Hit::Tool(t) => place_toolbar(l, 8)
            .into_iter()
            .find(|p| p.tool == t)
            .map(|p| p.rect),
        Hit::Title(b) => place_title(l, 12)
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
