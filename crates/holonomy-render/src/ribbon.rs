//! Native WordCraft ribbon: shared geometry for painting and hit testing.
//!
//! Design tokens and icon artwork adapted from storytold/wordcraft, MIT. This is
//! deliberately not an egui port: Holonomy keeps its integer, damage-limited
//! framebuffer renderer and its sealed, offline execution model.
use crate::chrome::{ChromeMetrics, ChromeState, Layout};
use crate::icons::IconId;
use crate::menus::Action;
use crate::tree::{Label, Node, Rect, RoundRect, Style, SurfaceTree};
use crate::DamageRect;

pub use crate::wordcraft::tokens;
use crate::wordcraft::{gallery, ribbon_fit};

/// The native renderer's view of WordCraft's design tokens.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub title: u32, pub ribbon: u32, pub canvas: u32, pub border: u32,
    pub text: u32, pub dim: u32, pub disabled: u32, pub hover: u32,
    pub pressed: u32, pub checked: u32, pub accent: u32, pub on_accent: u32,
    pub input: u32, pub input_border: u32, pub group: u32,
    pub ruler: u32, pub ruler_margin: u32,
    pub icon: u32, pub panel: u32, pub border_strong: u32,
    pub accent_text: u32, pub tab_strip: u32, pub status_bar: u32, pub menu: u32,
}
impl Palette {
    pub fn get(dark: bool) -> Self {
        let t = crate::wordcraft::tokens::Tokens::get(dark);
        Self {
            title: t.title_bar, ribbon: t.ribbon, canvas: t.canvas, border: t.border,
            text: t.text, dim: t.text_dim, disabled: t.text_disabled, hover: t.hover,
            pressed: t.pressed, checked: t.checked, accent: t.accent, on_accent: t.on_accent,
            input: t.input, input_border: t.input_border, group: t.group_label,
            ruler: t.ruler, ruler_margin: t.ruler_margin, icon: t.icon, panel: t.panel,
            border_strong: t.border_strong, accent_text: t.accent_text,
            tab_strip: t.tab_strip, status_bar: t.status_bar, menu: t.menu,
        }
    }
}

pub const TABS: &[&str] = &[
    "File", "Home", "Insert", "Design", "Layout", "Review", "View", "Help",
];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Button {
    pub action: Action,
    pub label: &'static str,
    pub icon: Option<IconId>,
    pub rect: DamageRect,
    pub large: bool,
    /// **Whether this button draws its label, or only its icon.** See [`Row::fit`].
    ///
    /// A small button with an icon in a row that had to go compact draws the icon alone and keeps its
    /// name for the overflow menu and for a tooltip; **a button with no icon always draws its label**,
    /// because an icon-less button with no label is an empty square. That is the whole rule, and it is
    /// why this is a field rather than something `paint` re-derives: the *layout* decided to drop the
    /// label, so the layout has to be the one that says so.
    pub labelled: bool,
    /// Index of the owning group, including when a responsive layout hides this button.
    pub group: usize,
    /// A real paragraph-style specimen rather than an ordinary text button.
    pub style_preview: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct Group {
    pub label: &'static str,
    pub rect: DamageRect,
    pub level: u8,
}

fn specs(
    tab: usize,
) -> &'static [(
    &'static str,
    &'static [(Action, &'static str, Option<IconId>, bool)],
)] {
    match tab {
        1 => &[
            (
                "Clipboard",
                &[
                    (Action::Paste, "Paste", Some(IconId::Paste), true),
                    (Action::Cut, "Cut", Some(IconId::Cut), false),
                    (Action::Copy, "Copy", Some(IconId::Copy), false),
                ],
            ),
            (
                "Font",
                &[
                    (
                        Action::SelectTool(crate::widgets::Tool::Font),
                        "Inter",
                        None,
                        false,
                    ),
                    (Action::ToggleBold, "Bold", Some(IconId::Bold), false),
                    (Action::ToggleItalic, "Italic", Some(IconId::Italic), false),
                    (
                        Action::ToggleUnderline,
                        "Underline",
                        Some(IconId::Underline),
                        false,
                    ),
                    (
                        Action::ToggleStrike,
                        "Strikethrough",
                        Some(IconId::Strike),
                        false,
                    ),
                    (
                        Action::ToggleSubscript,
                        "Subscript",
                        Some(IconId::Subscript),
                        false,
                    ),
                    (
                        Action::ToggleSuperscript,
                        "Superscript",
                        Some(IconId::Superscript),
                        false,
                    ),
                    (
                        Action::SelectTool(crate::widgets::Tool::TextColour),
                        "Font color",
                        Some(IconId::TextColour),
                        false,
                    ),
                    (
                        Action::ClearFormatting,
                        "Clear formatting",
                        Some(IconId::Close),
                        false,
                    ),
                ],
            ),
            (
                "Paragraph",
                &[
                    (
                        Action::ToggleBullets,
                        "Bullets",
                        Some(IconId::Bullets),
                        false,
                    ),
                    (
                        Action::ToggleNumbering,
                        "Numbering",
                        Some(IconId::Numbering),
                        false,
                    ),
                    (
                        Action::Outdent,
                        "Decrease indent",
                        Some(IconId::Outdent),
                        false,
                    ),
                    (
                        Action::Indent,
                        "Increase indent",
                        Some(IconId::Indent),
                        false,
                    ),
                    (
                        Action::AlignLeft,
                        "Align left",
                        Some(IconId::AlignLeft),
                        false,
                    ),
                    (
                        Action::AlignCenter,
                        "Center",
                        Some(IconId::AlignCenter),
                        false,
                    ),
                    (
                        Action::AlignRight,
                        "Align right",
                        Some(IconId::AlignRight),
                        false,
                    ),
                ],
            ),
            (
                "Styles",
                &[
                    (Action::SetStyle(0), "Normal", None, true),
                    (Action::SetStyle(1), "Heading 1", None, true),
                    (Action::SetStyle(5), "Heading 2", None, true),
                    (Action::SetStyle(3), "Title", None, true),
                    (Action::SetStyle(2), "Quote", None, true),
                    (Action::SetStyle(4), "Subtitle", None, true),
                ],
            ),
            (
                "Editing",
                &[
                    (Action::Find, "Find", Some(IconId::Search), false),
                    (Action::Replace, "Replace", Some(IconId::Replace), false),
                    (Action::SelectAll, "Select all", Some(IconId::Select), false),
                ],
            ),
        ],
        2 => &[
            (
                "Pages",
                &[(
                    Action::InsertPageBreak,
                    "Page break",
                    Some(IconId::PageBreak),
                    true,
                )],
            ),
            (
                "Tables",
                &[(Action::InsertTable, "Table", Some(IconId::Table), true)],
            ),
            (
                "Illustrations",
                &[(Action::InsertImage, "Picture", Some(IconId::Image), true)],
            ),
            (
                "Symbols",
                &[(Action::InsertMath, "Equation", Some(IconId::Rule), true)],
            ),
            (
                "Text",
                &[(Action::Import, "Import text", Some(IconId::Doc), true)],
            ),
        ],
        3 => &[
            (
                "Document formatting",
                &[
                    (Action::SetStyle(0), "Normal", None, true),
                    (Action::SetStyle(1), "Heading", None, true),
                    (Action::SetStyle(3), "Title", None, true),
                    (Action::SetStyle(2), "Quote", None, true),
                ],
            ),
            (
                "Appearance",
                &[(
                    Action::ToggleTheme,
                    "Light / dark",
                    Some(IconId::Theme),
                    true,
                )],
            ),
        ],
        4 => &[
            (
                "Paragraph",
                &[
                    (
                        Action::AlignLeft,
                        "Align left",
                        Some(IconId::AlignLeft),
                        true,
                    ),
                    (
                        Action::AlignCenter,
                        "Center",
                        Some(IconId::AlignCenter),
                        true,
                    ),
                    (
                        Action::AlignRight,
                        "Align right",
                        Some(IconId::AlignRight),
                        true,
                    ),
                    (
                        Action::Indent,
                        "Increase indent",
                        Some(IconId::Indent),
                        true,
                    ),
                    (
                        Action::Outdent,
                        "Decrease indent",
                        Some(IconId::Outdent),
                        true,
                    ),
                ],
            ),
            (
                "Page setup",
                &[(
                    Action::InsertPageBreak,
                    "Page break",
                    Some(IconId::PageBreak),
                    true,
                )],
            ),
        ],
        5 => &[
            (
                "Proofing",
                &[(
                    Action::WordCount,
                    "Word count",
                    Some(IconId::Spellcheck),
                    true,
                )],
            ),
            (
                "Editing",
                &[
                    (Action::Find, "Find", Some(IconId::Search), true),
                    (Action::Replace, "Replace", Some(IconId::Replace), true),
                ],
            ),
            (
                "Mode",
                &[
                    (Action::SetMode(0), "Editing", Some(IconId::Pen), true),
                    (Action::SetMode(1), "Viewing", Some(IconId::Lock), true),
                ],
            ),
        ],
        6 => &[
            (
                "Show",
                &[
                    (
                        Action::ToggleNavigation,
                        "Navigation",
                        Some(IconId::Navigation),
                        true,
                    ),
                    (
                        Action::ToggleRibbon,
                        "Ribbon",
                        Some(IconId::ChevronUp),
                        true,
                    ),
                ],
            ),
            (
                "Zoom",
                &[
                    (Action::SetZoom(100), "100%", Some(IconId::Search), true),
                    (Action::SetZoom(125), "125%", Some(IconId::Plus), true),
                ],
            ),
            (
                "Appearance",
                &[(
                    Action::ToggleTheme,
                    "Light / dark",
                    Some(IconId::Theme),
                    true,
                )],
            ),
        ],
        7 => &[(
            "Help",
            &[
                (
                    Action::KeyboardShortcuts,
                    "Keyboard shortcuts",
                    Some(IconId::Bookmark),
                    true,
                ),
                (
                    Action::WordCount,
                    "Document statistics",
                    Some(IconId::Spellcheck),
                    true,
                ),
            ],
        )],
        _ => &[],
    }
}

/// **The one place the ribbon measures a string.** Part 46.
///
/// # The defect this exists to end
///
/// **Every measurement in this file used `let advance = 8`** -- a number that is right only at the
/// 16 ppem design, and only because *there* the Monospace face's advance happens to be 8. The painter
/// does not read it: [`Painter::label`](../../holonomy_display/paint/struct.Painter.html) advances by
/// `atlas.metric(c, style, size).advance_x`, the face's own number. At 125 % -- which is what
/// `initial_desktop_zoom` picks on any screen 1,800 px or wider, so it is the **shipping default** --
/// that advance is 12, not 8.
///
/// **So the ribbon drew 12 px glyphs into boxes measured at 8 px each, and every symptom followed from
/// that one ratio:**
///
/// * `Italic` is six cells -- 48 px believed, **72 px drawn** -- in a 54 px label area, so it overflowed
///   its button and the next one's ink started before this one's ended. Two adjacent labels over a
///   background of the *same colour* read as one word, which is why the screenshot says `Int� Ital.`
///   and `Superscript` came out `Supe x`.
/// * `Home` + `Insert` were laid 54 + 70 px apart while their ink was 120 px wide, so the tab strip
///   read `HomeInsert` with no gap. There was no padding bug -- there was a *measurement* bug, and
///   padding is what a real 8 px cell would have needed.
/// * The `...` elision could not fire, because its guard `(width - 8) / 8` believed a 54 px area holds
///   six cells when it holds four. **A truncation guard measured in the wrong unit does not truncate,
///   it lies**, and the `Ital..`, `Subs..`, `Font...` in the screenshot are the *font's* renderer
///   clipping at the button edge rather than this code choosing to elide.
///
/// **The fix is not "widen the buttons".** Widening them at 8 px would leave them wrong at 24 ppem, and
/// it is the same mistake that produced this file's first pass: Wordcraft's 76/82/150 px were sized
/// for egui's *proportional* text, and porting the numbers without re-measuring them against a
/// monospace cell is what made them wrong in the first place. **The buttons are now sized from the
/// advance the painter will actually use**, so the same code is right at every ppem.
pub fn advance(l: &Layout) -> u32 {
    l.cell_w.max(1)
}

/// How many cells of `cell_w` a string occupies -- **the only measurement of text in this module**.
///
/// `chars().count()` and not `len()`: the byte count of `Font color` is 10 and its cell count is 10,
/// but of `Decrease indent` it is 15 against 15 -- and of any label with a multi-byte character the
/// two diverge, at which point a byte-measured box is wider than its ink. The ribbon's own labels are
/// ASCII today; a measurement that is only correct for ASCII is a trap for the first one that is not.
pub fn cells(s: &str) -> u32 {
    s.chars().count() as u32
}

/// The pixel width of `s` in this layout's cell.
pub fn text_w(l: &Layout, s: &str) -> u32 {
    let c = cells(s);
    let adv = advance(l);
    // Allocate proportional headroom so text never overflows its container.
    (c * adv * 5 / 4).max(c * adv + 4)
}

/// Horizontal padding either side of a tab's label, and the gap between two tabs.
pub const TAB_PAD: u32 = 12;
pub const TAB_GAP: u32 = 10;

pub fn tab_rects(l: &Layout) -> Vec<DamageRect> {
    // **Measured, padded and separated, and all three were missing.** It was `len() * 8 + 22` with no
    // gap at all, so at a 12 px cell `Home`'s 48 px of ink sat in a 54 px box that began 10 px after
    // `File`'s box ended -- which reads as one word. The `+ 22` was the padding *and* the gap rolled
    // into one, which works only while the ink is narrower than the box it is in, and at 12 px the ink
    // was wider than the box.
    let right = collapse_rect(l).x;
    let all_end = TABS.iter().fold(12u32, |x, name| {
        x.saturating_add(text_w(l, name) + TAB_PAD * 2 + TAB_GAP)
    });
    // Reserve a real, labelled way into the tabs that did not fit. Previously
    // `tab_rects` clipped View/Help at the window edge (and under Collapse):
    // they were neither readable nor selectable at laptop widths.
    let limit = if all_end.saturating_sub(TAB_GAP) <= right {
        right
    } else {
        right.saturating_sub(text_w(l, "More") + TAB_PAD * 2 + TAB_GAP)
    };
    let mut x = 12u32;
    TABS.iter()
        .map(|name| {
            let w = text_w(l, name) + TAB_PAD * 2;
            let r = if x.saturating_add(w) <= limit {
                DamageRect::new(x, l.menubar.y, w, l.menubar.height)
            } else {
                DamageRect::EMPTY
            };
            x = x.saturating_add(w).saturating_add(TAB_GAP);
            r
        })
        .collect()
}

/// The overflow tab uses the same measured text and tab height as its siblings.
/// An empty rectangle means every tab fits and there is no false affordance.
pub fn more_rect(l: &Layout) -> DamageRect {
    if tab_rects(l).iter().all(|r| !r.is_empty()) {
        return DamageRect::EMPTY;
    }
    let right = collapse_rect(l).x;
    let w = text_w(l, "More") + TAB_PAD * 2;
    let x = right.saturating_sub(w + TAB_GAP);
    DamageRect::new(x, l.menubar.y, right.saturating_sub(x + TAB_GAP), l.menubar.height)
}

/// **The gutter a small button spends on its icon before its label may begin.**
///
/// It is 28 and not 22 because the painter draws the 16 px mask at `x + 6` -- so the ink ends at 22
/// and the label starts at 28. A constant, because **[`IconId`] masks are 16 px at every type size**:
/// icons do not scale with the cell, so the space before a label does not either.
pub const ICON_GUTTER: u32 = 28;
/// Air after a label.
pub const SMALL_PAD: u32 = 10;
/// Air under a small button's row, on top of one line of type.
///
/// It was a hardcoded `29` px pitch, which is a 25 px line and 4 px of air at the design size and a
/// **5 px overlap** at 125 %, where a line is 30 px. **A row pitch written as a number instead of as
/// a line plus air is the same class of error as a width written in a cell that does not exist.**
pub const SMALL_ROW_AIR: u32 = 4;
/// The room a drop-down chevron needs at a button's right edge. `paint` draws it at
/// `rect.right() - 20`, so a label that ran to the edge would be under it.
pub const CHEVRON_W: u32 = 22;
/// A large button's minimum edge, icon or no icon.
pub const LARGE_MIN: u32 = 68;
/// Air between one group and the next, and the group's own inset.
pub const GROUP_PAD: u32 = 16;
/// Air around a group's caption, on top of one line of type.
///
/// **A separate constant because it is read by both `layout` and `paint`**, and the two disagreeing is
/// the defect that put the caption in the ruler: `paint` gave the strip a literal 20 px and `layout`
/// gave the buttons everything above `bottom - 20`.
pub const CAPTION_AIR: u32 = 4;
/// **Air between a group's buttons and its caption's strip**, over and above [`CAPTION_AIR`].
///
/// A large button stacks its icon over its label and its label box ran to the *bottom* of the
/// group, so at 16 ppem a group's last strip of type and its caption met with nothing between them:
/// `Paste` printed on `Clipboard`, `Page break` on `Pages`, `Normal` on `Document formatting`. Six
/// pixels is a cell's leading here -- enough that the two strips read as two, at both type scales.
pub const CAPTION_GAP: u32 = 6;

/// **How many columns apart the ruler's numbered ticks are.**
///
/// A ruler is a scale, and a scale's marks have to be readable. At the 16 ppem cell of 8 px a number
/// every ten columns is 80 px apart, which leaves room for the number and enough unlabelled minor ticks
/// between to read a position off the ruler without counting; at 20 ppem it is 120 px. **Ten is also a
/// round number in the unit the ruler is in**, which is the only property that makes a scale easy to use.
pub const MAJOR_RULER_COLS: u32 = 10;

/// **How far the pen moves between two unlabelled ruler ticks, in columns.**
///
/// Four minors between majors, which is the density a 26 px band holds without the ticks merging into a
/// grey smear — and the reason the old field's 1-px-every-10-px looked like a texture rather than a
/// scale: it had no *unit*, so a longer tick every eighth of them counted nothing.
pub const RULER_MINOR_STEP: u32 = 2;

/// **The height of the filled triangle at each end of the text column**, in pixels.
///
/// Word's indent markers are about this size relative to a line of type. It is a *pixel* height and not
/// a cell height on purpose: the marker is an ornament on the band, not type, so it must not grow with
/// the font — and at 7 px it clears the 4 px minor tick under it at both type scales.
pub const RULER_MARKER_H: u32 = 7;
/// A small button with a label, and a small button with only its icon, at this type size.
///
/// **Two numbers and not one, because the ribbon needs both to plan a row.** The labelled width is
/// what the user reads; the icon-only width is what the *same* button costs when the row will not
/// fit as labelled -- Word's own compact behaviour, and Wordcraft's `ribbon_fit::COMPACT`.
fn small_widths(
    l: &Layout,
    state: &ChromeState,
    action: Action,
    label: &str,
    icon: bool,
) -> (u32, u32) {
    // **The Font button draws `font_name()`, not `label`.** `layout` is handed the state, so the
    // measurement can be of the string that will actually be drawn -- `Inter` is five cells and
    // `JetBrains Mono` fourteen, and a width that fits one does not fit the other.
    let text = if action == Action::SelectTool(crate::widgets::Tool::Font) {
        state.font_name()
    } else {
        label
    };
    // An icon-less button's only content is its label, so dropping the label is not an option for it.
    let chevron = if matches!(action, Action::SelectTool(_)) {
        CHEVRON_W
    } else {
        0
    };
    let left = if icon { ICON_GUTTER } else { 6 };
    let labelled = text_w(l, text) + SMALL_PAD + left + chevron;
    let icon_only = if icon {
        ICON_GUTTER + SMALL_PAD
    } else {
        labelled
    };
    (labelled, icon_only)
}

/// One group of a tab, planned before anything is placed.
struct Row {
    name: &'static str,
    large: Vec<Button>,
    /// One small column: the buttons stacked in it, and their labelled and icon-only widths.
    cols: Vec<(Vec<Button>, u32, u32)>,
    /// **The caption's own width, because a group cannot be narrower than the name under it.**
    ///
    /// The group's width was the sum of its buttons, and a group whose *label* is longer than its
    /// content therefore centred that label in a box too small to hold it: `Illustrations` over a
    /// single 68 px `Picture` button is 104 px of type in a 92 px group, so the painter's honest
    /// elision produced `Illustr…` -- **the very symptom this pass set out to remove**, on a tab
    /// nobody had looked at yet. A group is its caption's width or its buttons' width, whichever is
    /// greater; that is the whole rule.
    caption_w: u32,
}

/// One layout pass is the authority for both ink and pointer targets.
///
/// # CORRECTION, part 46: every width in here was measured in a cell that does not exist
///
/// **`small_w: 76`, `82`, `150`, `92` and `68` are Wordcraft's numbers, and they are egui
/// *proportional*-text numbers.** Ported to a monospace framebuffer they are only right by accident,
/// and the accident happened at 16 ppem: 54 px of label area holds six 9 px glyphs, which is enough
/// for `Italic` and not enough for `Underline`. **At the 125 % the desktop boots into they are not
/// even that**, and the reported screenshot is what that looks like: labels longer than their buttons
/// (`Int` / `Ital` running together, `Supe` / `Subs` clipped), the elision guard never firing because
/// `(width - 8) / 8` believed six cells fitted in an area that holds four, the group row running
/// **40 px** past `limit` so the overflow square answered for a Font button, and the caption strip
/// in a literal 20 px box under a 30 px line.
///
/// **So nothing here is a constant any more except what is genuinely fixed** -- the icons, the
/// chevron and the padding. Every label's width is [`text_w`] of the string the painter will draw,
/// the Font button's width is `text_w` of [`ChromeState::font_name`] because `paint` draws the face's
/// name there, and the row is fitted by [`Row::fit`] when the honest widths do not fit the window.
pub fn layout(l: &Layout, state: &ChromeState) -> (Vec<Group>, Vec<Button>) {
    layout_with_limit(l, state, Some(l.width.saturating_sub(48)))
}

/// WordCraft's full -> compact -> collapsed planner, using the same measured widths
/// for planning, painting and hit testing. Hidden commands remain in the returned
/// button list with empty rects, so the overflow menu can enumerate them.
fn layout_with_limit(l: &Layout, state: &ChromeState, available: Option<u32>) -> (Vec<Group>, Vec<Button>) {
    let mut groups = Vec::new();
    let mut buttons = Vec::new();
    if state.ribbon_collapsed {
        return (groups, buttons);
    }
    let tab = usize::from(state.ribbon_tab).min(TABS.len() - 1);
    let caption_h = l.cell_h.saturating_add(CAPTION_AIR);
    let row_h = l.cell_h.saturating_add(SMALL_ROW_AIR);
    let tallest = 3 + 2 * row_h + CAPTION_GAP + caption_h;
    let inset = l.toolbar.height.saturating_sub(tallest) / 2;
    let top = l.toolbar.y + inset;
    let h = l.toolbar.height.saturating_sub(inset * 2);
    let body_h = h.saturating_sub(caption_h + CAPTION_GAP);
    let limit = available.unwrap_or(l.width);
    let start = l.toolbar.x + 10;
    let mut rows = Vec::new();
    for (name, items) in specs(tab) {
        let small: Vec<_> = items.iter().filter(|i| !i.3).copied().collect();
        let rows_n = if small.len() > 5 { 2 } else { 1 };
        let mut large = Vec::new();
        for (action, label, icon, _) in items.iter().copied().filter(|i| i.3) {
            let style_preview = gallery::specimen(action).is_some();
            let bw = if style_preview { gallery::tile_width(l.cell_h) } else if icon.is_none() { 92 } else { LARGE_MIN };
            let bw = bw.max(text_w(l, label) + SMALL_PAD * 2);
            large.push(Button {
                action, label, icon,
                rect: DamageRect::new(0, top + 3, bw, body_h.saturating_sub(3)),
                large: true, labelled: true, group: 0, style_preview,
            });
        }
        let mut cols = Vec::new();
        for pair in small.chunks(rows_n) {
            let mut col = Vec::new();
            let (mut width, mut icon_only) = (0, 0);
            for (row, (action, label, icon, _)) in pair.iter().copied().enumerate() {
                let (bw, io) = small_widths(l, state, action, label, icon.is_some());
                width = width.max(bw);
                icon_only = icon_only.max(io);
                col.push(Button {
                    action, label, icon,
                    rect: DamageRect::new(0, top + 3 + row as u32 * row_h, bw, row_h),
                    large: false, labelled: true, group: 0, style_preview: false,
                });
            }
            cols.push((col, width, icon_only));
        }
        rows.push(Row { name, large, cols, caption_w: text_w(l, name) });
    }
    let column_width = |col: &[Button], full: u32, compact: u32, level: u8| {
        col.iter().map(|b| if level == ribbon_fit::COMPACT && b.icon.is_some() { compact } else { full }).max().unwrap_or(0)
    };
    let group_width = |row: &Row, level: u8| {
        let caption = row.caption_w + GROUP_PAD;
        if level == ribbon_fit::COLLAPSED {
            return caption.max(LARGE_MIN + GROUP_PAD);
        }
        let mut w = GROUP_PAD;
        let mut tiles = 0;
        for b in &row.large {
            let visible = !b.style_preview || level == ribbon_fit::FULL || {
                tiles += 1;
                tiles <= gallery::STYLE_TILES_COMPACT
            };
            if visible { w += b.rect.width; }
        }
        for (col, full, compact) in &row.cols {
            w += column_width(col, *full, *compact, level);
        }
        w.max(caption)
    };
    let measurements: Vec<_> = rows.iter().map(|row| ribbon_fit::GroupWidths {
        title: row.name,
        widths: [ribbon_fit::FULL, ribbon_fit::COMPACT, ribbon_fit::COLLAPSED].map(|level| Some(group_width(row, level))),
    }).collect();
    let levels = ribbon_fit::plan(&measurements, available.map(|w| w.saturating_sub(start)).unwrap_or(u32::MAX));
    let mut x = start;
    for (gi, (row, level)) in rows.iter_mut().zip(levels).enumerate() {
        let w = group_width(row, level);
        let compact = level == ribbon_fit::COMPACT;
        let gr = if available.is_some() && (x >= limit || w > limit - x) {
            DamageRect::EMPTY
        } else {
            DamageRect::new(x, top, w, h)
        };
        groups.push(Group { label: row.name, rect: gr, level });
        let mut bx = x + 6;
        let mut style_tile = 0;
        for mut b in row.large.drain(..) {
            b.group = gi;
            let gallery_hidden = b.style_preview && compact && {
                let hidden = style_tile >= gallery::STYLE_TILES_COMPACT;
                style_tile += 1;
                hidden
            };
            if level == ribbon_fit::COLLAPSED || gallery_hidden || gr.is_empty() || (available.is_some() && bx + b.rect.width > limit) {
                b.rect = DamageRect::EMPTY;
            } else {
                b.rect.x = bx;
                bx += b.rect.width;
            }
            buttons.push(b);
        }
        for (mut col, full, compact_w) in row.cols.drain(..) {
            let cw = column_width(&col, full, compact_w, level);
            for mut b in col {
                b.group = gi;
                b.rect.width = cw;
                if compact && b.icon.is_some() { b.labelled = false; }
                if level == ribbon_fit::COLLAPSED || gr.is_empty() || (available.is_some() && bx + cw > limit) {
                    b.rect = DamageRect::EMPTY;
                } else {
                    b.rect.x = bx;
                    if b.rect.bottom() > top + body_h {
                        b.rect.height = b.rect.height.saturating_sub(b.rect.bottom() - (top + body_h));
                    }
                    if b.rect.height == 0 { b.rect = DamageRect::EMPTY; }
                }
                buttons.push(b);
            }
            if level != ribbon_fit::COLLAPSED { bx += cw; }
        }
        x = x.saturating_add(w);
    }
    (groups, buttons)
}

/// The whole collapsed group, in its original order. Actions stay attached to the
/// group even when the group has no visible command buttons.
pub fn group_items(state: &ChromeState, group: usize) -> Vec<crate::menus::Item> {
    let tab = usize::from(state.ribbon_tab).min(TABS.len() - 1);
    specs(tab).get(group).map(|(_, items)| items.iter().map(|&(action, label, icon, _)| crate::menus::Item {
        label, icon, accelerator: None, submenu: false,
        action: Some(action), checked: active(action, state),
    }).collect()).unwrap_or_default()
}

/// The measured click target for a collapsed group's drop-down.
pub fn group_trigger(l: &Layout, group: &Group) -> DamageRect {
    if group.level != ribbon_fit::COLLAPSED || group.rect.is_empty() {
        return DamageRect::EMPTY;
    }
    let body = group.rect.height.saturating_sub(l.cell_h + CAPTION_AIR + CAPTION_GAP);
    DamageRect::new(group.rect.x + 4, group.rect.y + 3, group.rect.width.saturating_sub(8), body.saturating_sub(6))
}

pub fn group_icon(label: &str) -> IconId {
    match label {
        "Clipboard" => IconId::Paste,
        "Font" => IconId::Bold,
        "Paragraph" => IconId::Bullets,
        "Styles" | "Document formatting" => IconId::Doc,
        "Editing" => IconId::Search,
        "Zoom" => IconId::Search,
        "Appearance" => IconId::Theme,
        "Tables" => IconId::Table,
        "Illustrations" => IconId::Image,
        _ => IconId::Overflow,
    }
}

pub fn overflow(l: &Layout, state: &ChromeState) -> Vec<crate::menus::Item> {
    let (groups, buttons) = layout(l, state);
    buttons
        .into_iter()
        .filter(|b| {
            // A visible collapsed group already has its own measured dropdown.
            // The global overflow is for commands whose group was clipped away,
            // or for compact-gallery tiles that have no individual trigger.
            b.rect.is_empty()
                && groups.get(b.group).is_none_or(|g| {
                    g.level != ribbon_fit::COLLAPSED || g.rect.is_empty()
                })
        })
        .map(|b| crate::menus::Item {
            label: b.label,
            icon: b.icon,
            accelerator: None,
            submenu: false,
            action: Some(b.action),
            checked: active(b.action, state),
        })
        .collect()
}

/// The quick-access row: AutoSave, Save, Undo, Redo, left to right in the title band.
///
/// # CORRECTION, part 46: AutoSave's box was 76 px and its label is not
///
/// The first entry was a fixed 112 px with `paint` drawing the label into the first **76** of them --
/// and `AutoSave` is eight cells, so those 76 px held six at the 125 % this boots into. The screenshot
/// shows `AutoSa` with the toggle sitting on top of the `v`: **the switch was drawn at `r.x + 78`, which
/// is past the ink but not past the word**, so the two overlapped. It is `text_w` of the label plus the
/// switch now, so the switch starts where the label ends at any cell.
pub fn qat(l: &Layout) -> Vec<(Action, DamageRect)> {
    const SWITCH_W: u32 = 30;
    let autosave = text_w(l, "AutoSave") + SWITCH_W + 12;
    let icon_button = (l.cell_h + 6).max(l.cell_w * 3).min(40);
    let mut items = Vec::new();
    // Give the document name priority on small displays. Undo/Redo are still
    // available from the Edit menu and Ctrl+Z/Ctrl+Y; the save control stays.
    if l.width >= 560 {
        items.push((Action::ToggleAutosave, autosave));
    }
    items.push((Action::Save, icon_button));
    if l.width >= 1100 {
        items.extend([(Action::Undo, icon_button), (Action::Redo, icon_button)]);
    }
    let h = header_control_h(l);
    let y = l.title.y + l.title.height.saturating_sub(h) / 2;
    items
        .into_iter()
        .scan(12u32, |x, (action, width)| {
            let rect = DamageRect::new(*x, y, width, h);
            *x = x.saturating_add(width + 4);
            Some((action, rect))
        })
        .filter(|(_, r)| r.right() <= l.width && !r.is_empty())
        .collect()
}

/// All the controls in the header use the same vertical centring and height.
fn header_control_h(l: &Layout) -> u32 {
    (l.cell_h + 8).min(l.title.height.saturating_sub(10))
}

/// Where the AutoSave switch sits inside the AutoSave rect, and the label's box.
///
/// **One function, because `paint` needs both and they must agree**: the label ends where the switch
/// begins, and the switch is the part that is *clickable* along with the label.
pub fn autosave_switch(l: &Layout, r: DamageRect) -> (DamageRect, DamageRect) {
    let label_w = text_w(l, "AutoSave");
    let sw = DamageRect::new(
        r.x + label_w + 4,
        r.y + r.height.saturating_sub(14) / 2,
        30,
        14,
    );
    let text_box = DamageRect::new(r.x, r.y, label_w.min(r.width), r.height);
    (text_box, sw)
}
/// The title's entire usable interval. Paint and hit geometry both use the
/// quick-access row and search field, so a long title cannot run under either.
pub fn title_rect(l: &Layout) -> DamageRect {
    let left = qat(l)
        .last()
        .map_or(12, |(_, r)| r.right())
        .saturating_add(12);
    let right = search_rect(l).x.saturating_sub(12);
    DamageRect::new(left.min(l.width), l.title.y, right.saturating_sub(left), l.title.height)
}

/// A full text search field where it fits; an icon-only search button at
/// narrower widths. Never draw a clipped placeholder or sacrifice the title.
pub fn search_rect(l: &Layout) -> DamageRect {
    const PLACEHOLDER: &str = "Find in document  Ctrl+F";
    let want = text_w(l, PLACEHOLDER) + 40;
    let left = qat(l)
        .last()
        .map_or(12, |(_, r)| r.right())
        .saturating_add(12);
    let right = lock_rect(l).x.saturating_sub(12);
    let room = right.saturating_sub(left);
    let title_min = text_w(l, "Untitled").saturating_add(20);
    let width = if room >= want.saturating_add(title_min) {
        want
    } else {
        // WordCraft's compact control level: the icon remains actionable and
        // gives the document name the available space.
        (l.cell_h + 10).min(room.saturating_sub(title_min))
    };
    let h = header_control_h(l);
    DamageRect::new(right.saturating_sub(width), l.title.y + l.title.height.saturating_sub(h) / 2, width, h)
}

pub fn lock_rect(l: &Layout) -> DamageRect {
    let width = text_w(l, "Lock") + 20;
    let x = l.width.saturating_sub(width + 14);
    let h = header_control_h(l);
    DamageRect::new(
        x,
        l.title.y + l.title.height.saturating_sub(h) / 2,
        width.min(l.width.saturating_sub(x)),
        h,
    )
}
/// **The ribbon's overflow button sits in the group row's top right, not on the ribbon's floor.**
///
/// It was `l.toolbar.y + 8` with a height of 30, which is the top of a 112 px band -- a 30 px square
/// floating above the buttons it is supposed to stand in for, and, at 768 px, overlapping the last
/// group's large buttons, which is why `widgets::hit` answered `Ribbon(SelectTool(Font))` for the
/// middle of the overflow square. **Anchored to the group's own top and height** so the two are the
/// same row and cannot overlap: the row starts 7 px into the band and its buttons are `h - 10` tall.
pub fn overflow_rect(l: &Layout) -> DamageRect {
    DamageRect::new(
        l.width.saturating_sub(40),
        l.toolbar.y + 7,
        30,
        l.toolbar.height.saturating_sub(10).min(46).max(27),
    )
}
pub fn collapse_rect(l: &Layout) -> DamageRect {
    DamageRect::new(
        l.width.saturating_sub(40),
        l.menubar.y + 3,
        30,
        l.menubar.height.saturating_sub(6),
    )
}

fn fill(v: &mut Vec<SurfaceTree>, r: DamageRect, c: u32) {
    if !r.is_empty() {
        v.push(SurfaceTree::leaf(Node::Rect(Rect::new(
            r.x as i32, r.y as i32, r.width, r.height, c,
        ))));
    }
}
fn round(v: &mut Vec<SurfaceTree>, r: DamageRect, radius: u32, c: u32) {
    if !r.is_empty() {
        v.push(SurfaceTree::leaf(Node::RoundRect(RoundRect::new(
            r.x as i32, r.y as i32, r.width, r.height, radius, c,
        ))));
    }
}
/// Draw a label into `r`, eliding it **only when it genuinely will not fit**.
///
/// # CORRECTION, part 46: the guard was measured in a cell that does not exist
///
/// It was `let advance = 8;` and `let max = (r.width - 8) / advance` -- *"how many cells fit"* asked of
/// a number that is only the cell at the 16 ppem design. **The painter does not use it**; it advances
/// by `atlas.metric(c, ..).advance_x`, which is 12 at the 125 % this product boots into. So the guard
/// believed a 54 px area holds six cells when it holds four, declined to elide a six-cell word, and
/// the *font's* renderer then clipped it at the button edge: that is the `Ital..` and `Subs..` in the
/// report. **A truncation guard in the wrong unit does not truncate -- it lies, and the pixels clip.**
///
/// `cell_w` is now an argument rather than a literal, and every caller passes [`Layout::cell_w`].
/// **A ruler's number: left-aligned on the tick it labels, with no button gutter.**
///
/// `text` adds `SMALL_PAD / 2` at the left, because the rect a small button hands over begins at its
/// icon's gutter -- see the comment there. A ruler has no icon, so that pad drew every number seven
/// pixels to the right of its own tick: the scale then disagreed with the column above it, which is the
/// one thing a ruler may not do. This is the same label without the gutter, and it is a separate
/// function rather than a flag on `text` so the buttons keep the pad their comment argues for.
///
/// Drawn only where it fits: the last major of a narrow page is a tick without a number, which is what
/// a printed ruler does rather than running its number into the gutter.
fn ruler_number(
    v: &mut Vec<SurfaceTree>,
    x: u32,
    band: DamageRect,
    s: &str,
    c: u32,
    cell_w: u32,
    cell_h: u32,
) {
    let advance = cell_w.max(1);
    let w = cells(s) * advance;
    // The band, not the text column: a number that fits in the ruler's own width is fine even if the
    // column ends a pixel earlier, and one that does not would be clipped by the canvas below.
    if w == 0 || x.saturating_add(w) > band.x + band.width || band.height <= cell_h / 4 {
        return;
    }
    // **The baseline sits on the tick strip**, so the numbers are one line of type with the ticks under
    // them rather than floating in the band's middle -- `text`'s centring would put a 2-digit label
    // half a line lower than a 1-digit one at some type scales.
    let y = band.y;
    v.push(SurfaceTree::leaf(Node::Label(Label::new(
        x as i32,
        y as i32,
        s,
        Style::REGULAR,
        0,
        c,
    ))));
}
pub(crate) fn text(
    v: &mut Vec<SurfaceTree>, r: DamageRect, s: &str, c: u32,
    bold: bool, center: bool, cell_w: u32, cell_h: u32,
) {
    text_styled(v, r, s, c, if bold { Style::BOLD } else { Style::REGULAR }, center, cell_w, cell_h);
}

fn text_styled(
    v: &mut Vec<SurfaceTree>, r: DamageRect, s: &str, c: u32,
    face: Style, center: bool, cell_w: u32, cell_h: u32,
) {
    let advance = cell_w.max(1);
    if r.width < advance || r.height < 8 {
        return;
    }
    let size = 0;
    // Every cell the rect can hold, then three of them given back for the ellipsis if one is needed.
    let fits = (r.width / advance).max(1);
    let total = cells(s);
    let shown: String = if total <= fits {
        s.to_string()
    } else if fits > 4 {
        // **Cells, not bytes.** `t.len()` in the loop below was a byte count, so a label with one
        // multi-byte character in it got one dot too many and the ellipsis itself overflowed the box.
        let mut t: String = s.chars().take((fits - 3) as usize).collect();
        for _ in cells(&t)..fits {
            t.push('.');
        }
        t
    } else {
        // **Too narrow for an ellipsis and a single character**, so the last cell is dropped rather
        // than the ellipsis being drawn as a single `..` that reads as punctuation.
        s.chars().take(fits as usize).collect()
    };
    let w = cells(&shown) * advance;
    // **Centred, or left-aligned in the *ink* column the caller handed over.** The rect a small
    // button passes in already begins at its icon's gutter (`layout` sized the button that way), so
    // the only air this adds is a cell's worth at the left -- `SMALL_PAD / 2`. **It used to clamp
    // that to `r.width - w`, which is zero whenever the label exactly fills its box**, so a label
    // that fitted was drawn flush against its icon and one that did not was drawn over it.
    let pad = (SMALL_PAD / 2).min(r.width.saturating_sub(w));
    let x = r.x
        + if center {
            r.width.saturating_sub(w) / 2
        } else {
            pad
        };
    let y = r.y + r.height.saturating_sub(cell_h.min(r.height)) / 2;
    v.push(SurfaceTree::leaf(Node::Label(Label::new(
        x as i32,
        y as i32,
        &shown,
        face,
        size,
        c,
    ))));
}
fn icon(v: &mut Vec<SurfaceTree>, r: DamageRect, i: IconId, c: u32, large: bool) {
    let edge = if large { 20 } else { 16 };
    v.push(SurfaceTree::leaf(Node::Icon(if large {
        i.at_large(
            r.x as i32 + (r.width.saturating_sub(edge) / 2) as i32,
            r.y as i32 + 5,
            c,
        )
    } else {
        i.at(
            r.x as i32 + (r.width.saturating_sub(edge) / 2) as i32,
            r.y as i32 + (r.height.saturating_sub(edge) / 2) as i32,
            c,
        )
    })));
}
fn active(a: Action, s: &ChromeState) -> bool {
    match a {
        Action::ToggleBold => s.styles.bold,
        Action::ToggleItalic => s.styles.italic,
        Action::ToggleUnderline => s.styles.underline,
        Action::SetStyle(i) => s.style_index == usize::from(i),
        Action::SetMode(i) => s.mode_index == i,
        Action::SetZoom(z) => s.zoom_percent == z,
        Action::ToggleAutosave => s.autosave,
        Action::ToggleNavigation => s.sidebar_open,
        Action::ToggleRibbon => !s.ribbon_collapsed,
        _ => false,
    }
}

/// WordCraft's Navigation pane: a search affordance, real tab targets and compact
/// heading/page rows. Geometry is shared with `widgets::hit` and the session.
pub fn nav_close(l: &Layout) -> DamageRect {
    DamageRect::new(l.sidebar.right().saturating_sub(40), l.sidebar.y + 12, 28.min(l.sidebar.width), 28)
}
pub fn nav_search(l: &Layout) -> DamageRect {
    DamageRect::new(l.sidebar.x + 12, l.sidebar.y + 52, l.sidebar.width.saturating_sub(24), l.cell_h + 10)
}
pub fn nav_tabs(l: &Layout) -> Vec<DamageRect> {
    let w = l.sidebar.width.saturating_sub(24) / 2;
    (0..2).map(|i| DamageRect::new(l.sidebar.x + 12 + i * w, l.sidebar.y + 98, w, l.cell_h + 8)).collect()
}
pub fn nav_entry_count(l: &Layout, s: &ChromeState) -> usize {
    if s.nav_tab == 1 { l.page_count(s.total_lines) as usize } else { s.navigation.len() }
}
pub fn nav_rows(l: &Layout, s: &ChromeState) -> Vec<(usize, DamageRect)> {
    let row_h = l.cell_h + 12;
    let capacity = l.sidebar.height.saturating_sub(154) / row_h.max(1);
    (0..nav_entry_count(l, s).min(capacity as usize)).map(|i| (i, DamageRect::new(l.sidebar.x + 8, l.sidebar.y + 146 + i as u32 * row_h, l.sidebar.width.saturating_sub(16), row_h - 2))).collect()
}

/// Three disjoint regions in the status bar. When a notice needs space,
/// shorten the counters; never paint an error underneath the mode label.
pub fn status_regions(
    l: &Layout,
    s: &ChromeState,
    summary: &str,
    mode: &str,
) -> (DamageRect, DamageRect, DamageRect) {
    let mode_w = text_w(l, mode).saturating_add(20);
    let mode_x = l.width.saturating_sub(mode_w.saturating_add(12));
    let mode_rect = DamageRect::new(
        mode_x,
        l.status.y,
        mode_w.min(l.width.saturating_sub(mode_x)),
        l.status.height,
    );
    let summary_x = 12.min(mode_x);
    let available = mode_x.saturating_sub(summary_x.saturating_add(8));
    let notice = s.notice.as_deref().is_some_and(|n| !n.is_empty());
    let summary_w = text_w(l, summary)
        .saturating_add(12)
        .min(if notice { available / 2 } else { available });
    let summary_rect = DamageRect::new(summary_x, l.status.y, summary_w, l.status.height);
    let notice_x = summary_rect.right().saturating_add(12).min(mode_x);
    let notice_rect = DamageRect::new(
        notice_x,
        l.status.y,
        if notice { mode_x.saturating_sub(notice_x.saturating_add(8)) } else { 0 },
        l.status.height,
    );
    (summary_rect, notice_rect, mode_rect)
}

/// Paint the ribbon. **`cell_w` is not optional and not a constant.**
///
/// # CORRECTION, part 46: this took only the height
///
/// It took `cell_h` and measured every horizontal extent in a hardcoded `8`, so the caller passed the
/// one metric the file did not need and omitted the one it did. [`text`] now takes both, and every
/// call site here passes `l.cell_w`.
pub fn paint(v: &mut Vec<SurfaceTree>, l: &Layout, s: &ChromeState, cell_h: u32) {
    let cell_w = advance(l);
    let t = Palette::get(s.dark);
    fill(v, DamageRect::new(0, 0, l.width, l.height), t.canvas);
    fill(v, l.title, t.title);
    fill(v, l.menubar, t.tab_strip);
    fill(v, l.toolbar, t.ribbon);
    fill(v, l.status, t.status_bar);
    fill(
        v,
        DamageRect::new(0, l.toolbar.bottom().saturating_sub(1), l.width, 1),
        t.border,
    );
    // Quick access, centred document title, command search and local vault lock.
    for (a, r) in qat(l) {
        if s.hover == Some(crate::widgets::Hit::Ribbon(a)) {
            round(v, r, 4, t.hover);
        }
        if a == Action::ToggleAutosave {
            let (tb, sw) = autosave_switch(l, r);
            text(v, tb, "AutoSave", t.dim, false, false, cell_w, cell_h);
            round(v, sw, 7, if s.autosave { t.accent } else { t.input_border });
            round(
                v,
                DamageRect::new(sw.x + if s.autosave { 17 } else { 3 }, sw.y + 3, 8, 8),
                4,
                t.on_accent,
            );
        } else if let Some(i) = match a {
            Action::Save => Some(IconId::Save),
            Action::Undo => Some(IconId::Undo),
            Action::Redo => Some(IconId::Redo),
            _ => None,
        } {
            icon(v, r, i, t.dim, false);
        }
    }
    // "Saved" was previously painted even for an untouched document that had
    // never been persisted. Reserve that claim for the driver's save notice.
    let title = if s.dirty {
        format!("{} - Unsaved changes", s.title)
    } else {
        s.title.clone()
    };
    text(v, title_rect(l), &title, t.text, true, true, cell_w, cell_h);
    let sr = search_rect(l);
    if !sr.is_empty() {
        round(v, sr, 6, t.input_border);
        round(
            v,
            DamageRect::new(sr.x + 1, sr.y + 1, sr.width.saturating_sub(2), sr.height.saturating_sub(2)),
            5,
            t.input,
        );
        let icon_box = DamageRect::new(sr.x, sr.y, 28.min(sr.width), sr.height);
        icon(v, icon_box, IconId::Search, t.dim, false);
        let placeholder = "Find in document  Ctrl+F";
        if sr.width >= text_w(l, placeholder) + 40 {
            text(
                v,
                DamageRect::new(sr.x + 28, sr.y, sr.width - 28, sr.height),
                placeholder,
                t.dim,
                false,
                false,
                cell_w,
                cell_h,
            );
        }
    }
    let lock = lock_rect(l);
    round(v, lock, 6, t.accent);
    text(v, lock, "Lock", t.on_accent, true, true, cell_w, cell_h);
    let tabs = tab_rects(l);
    for (i, r) in tabs.iter().copied().enumerate() {
        if r.is_empty() {
            continue;
        }
        let selected = i == usize::from(s.ribbon_tab);
        if selected {
            round(v, r, 5, t.ribbon);
            fill(
                v,
                DamageRect::new(
                    r.x + 8,
                    r.bottom().saturating_sub(3),
                    r.width.saturating_sub(16),
                    3,
                ),
                t.accent,
            );
        } else if s.hover == Some(crate::widgets::Hit::RibbonTab(i)) {
            round(v, r, 5, t.hover);
        }
        text(
            v,
            r,
            TABS[i],
            if selected { t.text } else { t.dim },
            selected,
            true,
            cell_w,
            cell_h,
        );
    }
    let more = more_rect(l);
    if !more.is_empty() {
        let selected = tabs
            .get(usize::from(s.ribbon_tab))
            .is_some_and(DamageRect::is_empty);
        if selected || s.hover == Some(crate::widgets::Hit::RibbonMore) {
            round(v, more, 5, if selected { t.ribbon } else { t.hover });
        }
        text(v, more, "More", if selected { t.text } else { t.dim }, selected, true, cell_w, cell_h);
    }
    let cr = collapse_rect(l);
    icon(
        v,
        cr,
        if s.ribbon_collapsed {
            IconId::ChevronDown
        } else {
            IconId::ChevronUp
        },
        t.dim,
        false,
    );
    // **The caption strip, derived the way `layout` derived it -- and it was not.**
    //
    // The caption was drawn in a literal `20` px box at `g.rect.bottom() - 20`, while `layout` had
    // already given the bottom `20` of the same rect to the buttons: a small button is
    // `top + 3 + row * 29` and 27 tall, so in a 102 px group the second row ends at 79 and the caption
    // began at 92. At the design pitch of 25 that is 7 px of clearance and it just about worked.
    // **At 125 % the pitch is 30**, so a 20 px box under a caption whose glyphs are 24 px tall drew its
    // bottom four rows *below* `g.rect.bottom()` -- that is, **into the ruler**, which is exactly the
    // `Paragraph` printed over `232 234 236` in the report. The band below is not a coincidence: it
    // is the next band.
    //
    // **So the strip is one line of type plus its air** ([`CAPTION_AIR`]), the buttons stop above it,
    // and the check at the end of `layout` is what keeps the two from meeting again.
    let caption_h = l.cell_h.saturating_add(CAPTION_AIR);
    let (groups, buttons) = layout(l, s);
    for (index, g) in groups.iter().enumerate() {
        if g.rect.is_empty() { continue; }
        fill(v, DamageRect::new(g.rect.right(), g.rect.y + 4, 1, g.rect.height.saturating_sub(8)), t.border);
        if g.level == ribbon_fit::COLLAPSED {
            let r = group_trigger(l, g);
            let hit = crate::widgets::Hit::RibbonGroup(index);
            let bg = if s.pressed == Some(hit) { t.pressed }
                else if s.open == Some(crate::widgets::Open::RibbonGroup(index)) { t.checked }
                else if s.hover == Some(hit) { t.hover } else { t.ribbon };
            round(v, r, 4, bg);
            icon(v, DamageRect::new(r.x, r.y, r.width, r.height.saturating_sub(l.cell_h)), group_icon(g.label), t.icon, true);
            text(v, DamageRect::new(r.x, r.bottom().saturating_sub(l.cell_h), r.width, l.cell_h), g.label, t.text, false, true, cell_w, cell_h);
        }
        text(v, DamageRect::new(g.rect.x, g.rect.bottom().saturating_sub(caption_h), g.rect.width, caption_h), g.label, t.group, false, true, cell_w, cell_h);
    }
    for b in buttons {
        if b.rect.is_empty() {
            continue;
        }
        let hit = crate::widgets::Hit::Ribbon(b.action);
        let on = active(b.action, s);
        let bg = if s.pressed == Some(hit) { t.pressed }
            else if on { t.checked }
            else if s.hover == Some(hit) { t.hover } else { t.ribbon };
        round(v, b.rect, 4, bg);
        let ink = if on { t.accent_text } else { t.text };
        if b.style_preview {
            if let Some((sample, face, colour)) = gallery::specimen(b.action) {
                let tile = DamageRect::new(b.rect.x + 2, b.rect.y + 2, b.rect.width.saturating_sub(4), b.rect.height.saturating_sub(4));
                round(v, tile, 3, if on { t.accent } else { t.border_strong });
                round(v, DamageRect::new(tile.x + 1, tile.y + 1, tile.width.saturating_sub(2), tile.height.saturating_sub(2)), 2, if on { t.checked } else { t.input });
                let specimen_box = DamageRect::new(tile.x + 3, tile.y + 3, tile.width.saturating_sub(6), tile.height.saturating_sub(l.cell_h + 6));
                let specimen_ink = if s.dark && colour == 0xFF242424 { t.text } else if s.dark { t.accent_text } else { colour };
                text_styled(v, specimen_box, sample, specimen_ink, face, true, cell_w, cell_h);
                text(v, DamageRect::new(tile.x, tile.bottom().saturating_sub(l.cell_h + 2), tile.width, l.cell_h), b.label, t.text, false, true, cell_w, cell_h);
            }
            continue;
        }
        if let Some(i) = b.icon {
            let icon_box = if b.large || !b.labelled { b.rect } else { DamageRect::new(b.rect.x, b.rect.y, ICON_GUTTER, b.rect.height) };
            icon(v, icon_box, i, if on { t.accent_text } else { t.icon }, b.large);
        }
        let has_menu = matches!(b.action, Action::SelectTool(_));
        let tr = if b.large {
            let icon_h = 25;
            DamageRect::new(b.rect.x, b.rect.y + icon_h.min(b.rect.height), b.rect.width, b.rect.height.saturating_sub(icon_h))
        } else {
            let gutter = if b.icon.is_some() { ICON_GUTTER } else { 0 };
            DamageRect::new(b.rect.x + gutter, b.rect.y, b.rect.width.saturating_sub(gutter).saturating_sub(if has_menu { CHEVRON_W } else { 0 }), b.rect.height)
        };
        let label = if b.action == Action::SelectTool(crate::widgets::Tool::Font) { s.font_name() } else { b.label };
        if b.labelled {
            text(v, tr, label, ink, false, b.large, cell_w, cell_h);
        }
        // **The chevron is measured before the label box is, because it owns the right edge.**
        let has_menu = matches!(b.action, Action::SelectTool(_));
        let tr = if b.large {
            // A large button stacks: a 20 px icon at the top, the label under it. **The split is
            // derived from the cell, not the literal 27** -- a 30 px line in a 27 px offset drew the
            // label over the icon.
            let icon_h = 20 + 5;
            DamageRect::new(
                b.rect.x,
                b.rect.y + icon_h.min(b.rect.height),
                b.rect.width,
                b.rect.height.saturating_sub(icon_h),
            )
        } else if b.icon.is_some() {
            DamageRect::new(
                b.rect.x + ICON_GUTTER,
                b.rect.y,
                b.rect
                    .width
                    .saturating_sub(ICON_GUTTER)
                    .saturating_sub(if has_menu { CHEVRON_W } else { 0 }),
                b.rect.height,
            )
        } else {
            DamageRect::new(
                b.rect.x,
                b.rect.y,
                b.rect
                    .width
                    .saturating_sub(if has_menu { CHEVRON_W } else { 0 }),
                b.rect.height,
            )
        };
        // **The Font button draws the face's name, not its static label** -- and `layout` sized it from
        // the same string, which is why `JetBrains Mono` does not print itself into the Bold column.
        let label = if b.action == Action::SelectTool(crate::widgets::Tool::Font) {
            s.font_name()
        } else {
            b.label
        };
        // **A compact button keeps its name for the overflow menu and loses it here**, which is the
        // whole of `Row`'s compact level: the icon remains and the label does not.
        if b.labelled {
            text(
                v,
                tr,
                label,
                ink,
                b.large && b.action == Action::SetStyle(3),
                // **A large label is centred; a small one is left-aligned after its icon.** `b.icon.is_none()`
                // used to centre a small label that had no icon, which put the plain-text styles in a
                // column whose labels each started somewhere different.
                b.large,
                cell_w,
                cell_h,
            );
        }
        if matches!(b.action, Action::SelectTool(_)) {
            icon(
                v,
                DamageRect::new(
                    b.rect.right().saturating_sub(20),
                    b.rect.y,
                    20,
                    b.rect.height,
                ),
                IconId::ChevronDown,
                t.dim,
                false,
            );
        }
    }
    if !s.ribbon_collapsed && !s.ribbon_overflow.is_empty() {
        let overflow = overflow_rect(l);
        if s.hover == Some(crate::widgets::Hit::Tool(crate::widgets::Tool::Overflow)) {
            round(v, overflow, 5, t.hover);
        }
        icon(v, overflow, IconId::Overflow, t.dim, false);
    }
    // # The ruler
    //
    // **A ruler in a word processor is a *measurement scale*, and this one was a decorative tick field.**
    // It drew: the page's text column tinted one colour, the gutters beside it another, and a 1 px mark
    // every 10 px with every eighth one longer. Nothing on it was a number, so nothing on it could be
    // read -- a ruler whose marks have no labels is a texture, and a user looking for "where is my
    // margin" or "how wide is this column" is answered by neither.
    //
    // **What every real ruler shows, and what this now draws** (Google Docs, Word and Wordcraft all
    // agree on the first three):
    //
    // 1. **The text column as the light band**, and the page's padding either side as the darker gutter --
    //    which is the geometry the page already has, so the ruler finally says where the column *is*
    //    rather than drawing its own idea of it.
    // 2. **Numbered major ticks**, zero at the column's left edge, labelled in **columns** -- this
    //    document model's unit. A monospaced page has a real, exact unit; a ruler that measured in
    //    centimetres would need a physical DPI this program does not have and cannot honestly invent.
    //    The label is the number, so the scale is readable.
    // 3. **Minor ticks** between them, **numbered every ten columns** with four minors between, which is
    //    the density that stays legible at a 26 px band and a 12 px cell.
    // 4. **The margin markers at both ends of the column**, filled triangles like Word's indent handles,
    //    so the column's edges are findable at a glance instead of being where the tint changes.
    //
    // **Ticks are drawn from the column's left edge, not from the page's**, so column 0 is where text
    // starts -- which is the only origin a writer cares about and the one the old tick field did not use.
    let col = l.text.x;
    let col_w = l.text.width.min(l.width.saturating_sub(col));
    // The honest band: the page's padding to the left and right of the column, not the whole window.
    fill(v, l.ruler, t.ruler_margin);
    fill(
        v,
        DamageRect::new(col, l.ruler.y, col_w, l.ruler.height),
        t.ruler,
    );
    // **The scale is in cells**, so the spacing of a major tick is `MAJOR_COLS * cell_w` and the number
    // under it is a column count. `cell_w` is the face's own advance, which is why the labels line up
    // with the glyphs above them rather than with a guess.
    let major_px = MAJOR_RULER_COLS * cell_w;
    if major_px > 0 && col_w > 0 {
        let mut c = 0u32;
        while c * cell_w <= col_w {
            let x = col + c * cell_w;
            if x >= col + col_w {
                break;
            }
            let major = c % MAJOR_RULER_COLS == 0;
            // A 1 px tick, from the band's bottom, four logical pixels tall between majors and seven
            // on them -- the proportions a printed ruler uses, and enough that the major is findable
            // without reading its number.
            fill(
                v,
                DamageRect::new(
                    x,
                    l.ruler.bottom().saturating_sub(if major { 7 } else { 4 }),
                    1,
                    if major { 7 } else { 4 },
                ),
                t.group,
            );
            // **The number, on the major only**, and above the tick rather than on it. `cell_h - 8`
            // keeps it inside a 26 px band at both type scales; a number drawn from the band's top with
            // a full line of type would be clipped by the canvas below it.
            if major {
                // **The label is the column number**, and it has to be a `String` because it is
                // computed -- so it is built here, once per major tick. A major every ten columns over
                // a 72-column page is eight allocations per frame; the alternative is a static table of
                // 0..=70, which is 71 strings and *wrong* the day the measure changes.
                //
                // The line it is drawn on is the band minus the tick strip, so the number's own line
                // never overlaps the ticks standing under it.
                let numbers = DamageRect::new(
                    l.ruler.x,
                    l.ruler.y,
                    l.ruler.width,
                    l.ruler.height.saturating_sub(ChromeMetrics::RULER_TICK_H),
                );
                ruler_number(v, x, numbers, &format!("{c}"), t.text, cell_w, cell_h);
            }
            c += RULER_MINOR_STEP;
        }
    }
    // Margin boundaries and draggable indent markers matching Google Docs and Word.
    if col_w > 0 {
        // Vertical boundary lines between page margin gutters and body text
        fill(
            v,
            DamageRect::new(col, l.ruler.y, 1, l.ruler.height),
            t.border,
        );
        fill(
            v,
            DamageRect::new(col + col_w, l.ruler.y, 1, l.ruler.height),
            t.border,
        );

        // First Line Indent: downward triangle at top of left margin
        let fl_x = if s.pressed == Some(crate::widgets::Hit::RulerFirstLineIndent) {
            s.ruler_drag_x.map(|dx| dx.max(0) as u32).unwrap_or(col)
        } else {
            col
        };
        let fl_h = 7u32;
        for row in 0..fl_h {
            let w = (fl_h - row) * 2 - 1;
            let left = fl_x.saturating_sub(w / 2);
            fill(v, DamageRect::new(left, l.ruler.y + row, w, 1), t.accent);
        }

        // Hanging Indent: upward triangle at bottom of left margin
        let li_x = if s.pressed == Some(crate::widgets::Hit::RulerLeftIndent) {
            s.ruler_drag_x.map(|dx| dx.max(0) as u32).unwrap_or(col)
        } else {
            col
        };
        let hi_h = 6u32;
        let hi_bottom = l.ruler.bottom().saturating_sub(5);
        for row in 0..hi_h {
            let w = (row + 1) * 2 - 1;
            let left = li_x.saturating_sub(w / 2);
            let y = hi_bottom.saturating_sub(hi_h - row);
            fill(v, DamageRect::new(left, y, w, 1), t.accent);
        }

        // Left Indent: base rectangle under hanging indent triangle
        let rect_w = 11u32;
        let rect_h = 4u32;
        let rect_left = li_x.saturating_sub(rect_w / 2);
        let rect_top = l.ruler.bottom().saturating_sub(rect_h);
        fill(
            v,
            DamageRect::new(rect_left, rect_top, rect_w, rect_h),
            t.accent,
        );

        // Right Indent: upward triangle at bottom of right margin
        let ri_x = if s.pressed == Some(crate::widgets::Hit::RulerRightIndent) {
            s.ruler_drag_x
                .map(|dx| dx.max(0) as u32)
                .unwrap_or(col + col_w)
        } else {
            col + col_w
        };
        let ri_h = 8u32;
        let ri_bottom = l.ruler.bottom().saturating_sub(1);
        for row in 0..ri_h {
            let w = (row + 1) * 2 - 1;
            let left = ri_x.saturating_sub(w / 2);
            let y = ri_bottom.saturating_sub(ri_h - row);
            fill(v, DamageRect::new(left, y, w, 1), t.accent);
        }

        // Active drag guide line extending through the page
        if let Some(drag_x) = s.ruler_drag_x {
            if drag_x >= 0 && (drag_x as u32) < l.width {
                let gx = drag_x as u32;
                fill(
                    v,
                    DamageRect::new(
                        gx,
                        l.ruler.bottom(),
                        1,
                        l.height.saturating_sub(l.ruler.bottom()),
                    ),
                    t.accent,
                );
            }
        }
    }
    // Status messages must not collide with the counters or the view/zoom label.
    // On a narrow window the counters give way to a save/error notice first.
    let rows = l.page_rows();
    let summary = format!(
        "Page {} of {}   {} words",
        s.scroll_line / rows + 1,
        l.page_count(s.total_lines),
        s.words
    );
    let mode_zoom = format!("{}   {}%", s.mode_name(), s.zoom_percent);
    let (summary_box, notice_box, mode_box) = status_regions(l, s, &summary, &mode_zoom);
    text(v, summary_box, &summary, t.dim, false, false, cell_w, cell_h);
    if let Some(notice) = s.notice.as_deref().filter(|n| !n.is_empty()) {
        text(v, notice_box, notice, t.text, false, false, cell_w, cell_h);
    }
    text(v, mode_box, &mode_zoom, t.dim, false, true, cell_w, cell_h);
}
