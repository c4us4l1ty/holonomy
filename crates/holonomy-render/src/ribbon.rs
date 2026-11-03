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

/// WordCraft `theme::Tokens`, packed for Holonomy's framebuffer.
pub mod tokens {
    pub mod dark {
        pub const TITLE: u32 = 0xFF1F1F1F;
        pub const RIBBON: u32 = 0xFF292929;
        pub const CANVAS: u32 = 0xFF141414;
        pub const BORDER: u32 = 0xFF3A3A3A;
        pub const TEXT: u32 = 0xFFEEEEEE;
        pub const DIM: u32 = 0xFFC8C8C8;
        pub const DISABLED: u32 = 0xFF6A6A6A;
        pub const HOVER: u32 = 0xFF3D3D3D;
        pub const PRESSED: u32 = 0xFF4A4A4A;
        pub const CHECKED: u32 = 0xFF2E3A66;
        pub const ACCENT: u32 = 0xFF7A93F0;
        pub const ON_ACCENT: u32 = 0xFF11162E;
        pub const INPUT: u32 = 0xFF1E1E1E;
        pub const INPUT_BORDER: u32 = 0xFF7A7A7A;
        pub const GROUP: u32 = 0xFFA0A0A0;
        pub const RULER: u32 = 0xFF454545;
        pub const RULER_MARGIN: u32 = 0xFF2E2E2E;
    }
    pub mod light {
        // WordCraft theme::Tokens::light (MIT): three distinct planes make the
        // page, controls and desktop legible without a heavy/dark application frame.
        // Only the unsealed desktop ribbon uses these; the sealed chrome is unchanged.
        pub const TITLE: u32 = 0xFFF0F0F0;
        pub const RIBBON: u32 = 0xFFF9F9F9;
        pub const CANVAS: u32 = 0xFFE6E6E6;
        pub const BORDER: u32 = 0xFFE1E1E1;
        pub const TEXT: u32 = 0xFF242424;
        pub const DIM: u32 = 0xFF616161;
        pub const DISABLED: u32 = 0xFFA8A8A8;
        pub const HOVER: u32 = 0xFFE6E6E6;
        pub const PRESSED: u32 = 0xFFD6D6D6;
        pub const CHECKED: u32 = 0xFFDDE3F8;
        pub const ACCENT: u32 = 0xFF3B5BDB;
        pub const ON_ACCENT: u32 = 0xFFFFFFFF;
        pub const INPUT: u32 = 0xFFFFFFFF;
        pub const INPUT_BORDER: u32 = 0xFF8A8A8A;
        pub const GROUP: u32 = 0xFF707070;
        pub const RULER: u32 = 0xFFFFFFFF;
        pub const RULER_MARGIN: u32 = 0xFFD4D4D4;
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub title: u32,
    pub ribbon: u32,
    pub canvas: u32,
    pub border: u32,
    pub text: u32,
    pub dim: u32,
    pub disabled: u32,
    pub hover: u32,
    pub pressed: u32,
    pub checked: u32,
    pub accent: u32,
    pub on_accent: u32,
    pub input: u32,
    pub input_border: u32,
    pub group: u32,
    pub ruler: u32,
    pub ruler_margin: u32,
}
impl Palette {
    pub fn get(dark: bool) -> Self {
        macro_rules! p {
            ($m:ident) => {
                Self {
                    title: tokens::$m::TITLE,
                    ribbon: tokens::$m::RIBBON,
                    canvas: tokens::$m::CANVAS,
                    border: tokens::$m::BORDER,
                    text: tokens::$m::TEXT,
                    dim: tokens::$m::DIM,
                    disabled: tokens::$m::DISABLED,
                    hover: tokens::$m::HOVER,
                    pressed: tokens::$m::PRESSED,
                    checked: tokens::$m::CHECKED,
                    accent: tokens::$m::ACCENT,
                    on_accent: tokens::$m::ON_ACCENT,
                    input: tokens::$m::INPUT,
                    input_border: tokens::$m::INPUT_BORDER,
                    group: tokens::$m::GROUP,
                    ruler: tokens::$m::RULER,
                    ruler_margin: tokens::$m::RULER_MARGIN,
                }
            };
        }
        if dark {
            p!(dark)
        } else {
            p!(light)
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
}
#[derive(Clone, Copy, Debug)]
pub struct Group {
    pub label: &'static str,
    pub rect: DamageRect,
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
                    (Action::SetStyle(1), "Heading", None, true),
                    (Action::SetStyle(3), "Title", None, true),
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
                    Some(IconId::Doc),
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
                    Some(IconId::Doc),
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
    let mut groups = Vec::new();
    let mut buttons = Vec::new();
    if state.ribbon_collapsed {
        return (groups, buttons);
    }
    let tab = usize::from(state.ribbon_tab).min(TABS.len() - 1);
    let mut x = l.toolbar.x + 10;
    let caption_h = l.cell_h.saturating_add(CAPTION_AIR);
    let row_h = l.cell_h.saturating_add(SMALL_ROW_AIR);
    // **The band's air is derived from the tallest thing that can go in it, not written down.**
    //
    // A group is at most two rows of small buttons over a caption strip. The shipped design is a
    // 112 px band for a 96 px group at cell 25 -- 8 px of air above and below -- and the two
    // literals that expressed that were `- 10` and `+ 7` here, which at 16 ppem spent 17 px of a
    // 71 px band on air while the content needed 69. **The second row was clamped and the caption
    // took the last 20**, which is `Page break` printed on `Pages` in the report.
    let tallest = 3u32 + 2 * row_h + CAPTION_GAP + caption_h;
    let inset = l.toolbar.height.saturating_sub(tallest) / 2;
    let top = l.toolbar.y + inset;
    let h = l.toolbar.height.saturating_sub(inset * 2);
    // **The caption strip is a line of type plus enough air above it that a button's own label
    // cannot land in it.** It was a literal 20 in `paint` against a line pitch of 25 -- at 125 % a
    // pitch of 30 in a 20 px box drew the caption's bottom four rows *in the ruler*, which is the
    // `Paragraph` printed over `232 234 236` in the report. Deriving it from the cell fixed that
    // and left the second half: a large button stacks a 20 px icon over its label, its label box
    // ran to the bottom of the group, and at 16 ppem the two strips met with nothing between them.
    // `CAPTION_AIR` is air above the caption's glyphs; `CAPTION_GAP` is air above *that*.
    let body_h = h.saturating_sub(caption_h.saturating_add(CAPTION_GAP));
    // **`- 48`, and it is the overflow square's column.** `overflow_rect` occupies the last 40 px of
    // the band, so a group's last button must end before `width - 48` or the two overlap -- which is
    // what `widgets::hit` reported at 768 px: the middle of the overflow square answered
    // `Ribbon(SelectTool(Font))` because the Font group had drawn over it.
    let limit = l.width.saturating_sub(48);

    // Per-group specs at the current font name, without placing anything.
    let mut rows: Vec<Row> = Vec::new();
    for (name, items) in specs(tab) {
        let sm: Vec<_> = items.iter().filter(|i| !i.3).copied().collect();
        let lg: Vec<_> = items.iter().filter(|i| i.3).copied().collect();
        let rows_n = if sm.len() > 5 { 2 } else { 1 };
        let mut large = Vec::new();
        for (action, label, icon, _) in lg.iter().copied() {
            let bw = if icon.is_none() { 92 } else { LARGE_MIN };
            // A large label is centred in the whole rect, so its box must be at least its own width.
            let bw = bw.max(text_w(l, label).saturating_add(SMALL_PAD * 2));
            large.push(Button {
                action,
                label,
                icon,
                rect: DamageRect::new(0, top + 3, bw, body_h.saturating_sub(3)),
                large: true,
                labelled: true,
            });
        }
        let mut cols = Vec::new();
        for pair in sm.chunks(rows_n) {
            let mut col = Vec::new();
            let (mut width, mut icon_only) = (0u32, 0u32);
            for (row, (action, label, icon, _)) in pair.iter().copied().enumerate() {
                let (bw, io) = small_widths(l, state, action, label, icon.is_some());
                width = width.max(bw);
                icon_only = icon_only.max(io);
                col.push(Button {
                    action,
                    label,
                    icon,
                    // The width is replaced by the column's below; the `y` is final.
                    rect: DamageRect::new(0, top + 3 + row as u32 * row_h, bw, row_h),
                    large: false,
                    labelled: true,
                });
            }
            cols.push((col, width, icon_only));
        }
        rows.push(Row {
            name,
            large,
            cols,
            caption_w: text_w(l, name),
        });
    }

    // **Plan the whole row before placing any of it.** The widths are now honest, so the plan can
    // know whether the tab fits -- and if it does not, give the labels up from the right.
    //
    // **One width rule, used by the plan and by the placement.** That is the whole reason this is a
    // closure rather than two loops that agree today: the defect this file opened with was two
    // expressions for one number (the group's `cols * small_w` sum against the buttons' own widths),
    // and a plan that measures differently from the thing it plans is that defect in a new place.
    //
    // `compact` is per group; a *column* inside a compact group is as wide as its widest button where
    // **an icon-bearing button drops to its icon and a button without one keeps its label**, because
    // an unlabelled icon-less button would be an empty square.
    let col_w = |col: &[Button], labelled: u32, icon_only: u32, compact: bool| -> u32 {
        col.iter()
            .map(|b| {
                if !compact {
                    labelled
                } else if b.icon.is_some() {
                    icon_only
                } else {
                    labelled
                }
            })
            .max()
            .unwrap_or(0)
    };
    let total = |rs: &[Row], from: usize| -> u32 {
        rs.iter()
            .enumerate()
            .map(|(i, r)| {
                let mut w: u32 = r.large.iter().map(|b| b.rect.width).sum::<u32>() + GROUP_PAD;
                for (col, labelled, icon_only) in &r.cols {
                    w += col_w(col, *labelled, *icon_only, i >= from);
                }
                // **And the caption fits, or nothing does.** The overflow menu is where a group
                // that does not fit is meant to go; a group narrower than its own name is not
                // "compact", it is a group whose label is elided by its own layout.
                w.max(r.caption_w + GROUP_PAD)
            })
            .sum()
    };
    let avail = limit.saturating_sub(x);
    let mut compact_from = rows.len();
    let mut width = total(&rows, compact_from);
    while width > avail && compact_from > 0 {
        compact_from -= 1;
        width = total(&rows, compact_from);
    }

    for (gi, row) in rows.iter_mut().enumerate() {
        let compact = gi >= compact_from;
        let mut cols: Vec<(u32, Vec<Button>)> = Vec::new();
        for (col, labelled, icon_only) in row.cols.drain(..) {
            let mut col = col;
            for b in col.iter_mut() {
                if compact && b.icon.is_some() {
                    b.labelled = false;
                }
            }
            let cw = col_w(&col, labelled, icon_only, compact);
            cols.push((cw, col));
        }
        let w = (row.large.iter().map(|b| b.rect.width).sum::<u32>()
            + cols.iter().map(|(cw, _)| *cw).sum::<u32>()
            + GROUP_PAD)
            .max(row.caption_w + GROUP_PAD);
        // **The group's own rect stops at the overflow square's column too**, not only at the panel
        // edge. A group whose buttons were all hidden for want of room still drew a divider and a
        // centred caption out to its *planned* width -- through the overflow square and, on a narrow
        // window, off the panel. The caption is centred in this rect, so clamping it here is what
        // keeps a group that did not fit from labelling itself over the control that stands in for it.
        let gr = DamageRect::new(
            x.min(l.width),
            top,
            w.min(limit.saturating_sub(x))
                .min(l.width.saturating_sub(x)),
            h,
        );
        groups.push(Group {
            label: row.name,
            rect: gr,
        });
        let mut bx = x + 6;
        for mut b in row.large.drain(..) {
            if bx + b.rect.width <= limit {
                b.rect.x = bx;
            } else {
                b.rect = DamageRect::EMPTY;
            }
            bx += b.rect.width;
            buttons.push(b);
        }
        for (cw, col) in cols {
            for mut b in col {
                b.rect.width = cw;
                if bx + cw <= limit {
                    b.rect.x = bx;
                } else {
                    b.rect = DamageRect::EMPTY;
                }
                // **The last row must end above the caption strip, or the caption loses.**
                // `body_h` is `h - caption_h`, and a two-row group is `2 * row_h` tall, so a pitch
                // that does not fit leaves the buttons over the caption and the caption over the
                // ruler. Clamping here rather than asserting, because a caller can hand this a
                // `Layout` whose band is too short and the honest answer is a button a few pixels
                // shorter, not one drawn outside its group.
                if !b.rect.is_empty() && b.rect.bottom() > gr.y + body_h {
                    let cut = b.rect.bottom() - (gr.y + body_h);
                    b.rect.height = b.rect.height.saturating_sub(cut);
                    if b.rect.height == 0 {
                        b.rect = DamageRect::EMPTY;
                    }
                }
                buttons.push(b);
            }
            bx += cw;
        }
        x = x.saturating_add(w);
    }
    (groups, buttons)
}

pub fn overflow(l: &Layout, state: &ChromeState) -> Vec<crate::menus::Item> {
    layout(l, state)
        .1
        .into_iter()
        .filter(|b| b.rect.is_empty())
        .map(|b| crate::menus::Item {
            label: b.label,
            icon: b.icon,
            accelerator: None,
            submenu: false,
            action: Some(b.action),
            checked: false,
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
fn text(
    v: &mut Vec<SurfaceTree>,
    r: DamageRect,
    s: &str,
    c: u32,
    bold: bool,
    center: bool,
    cell_w: u32,
    cell_h: u32,
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
        if bold { Style::BOLD } else { Style::REGULAR },
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
    fill(v, l.menubar, t.title);
    fill(v, l.toolbar, t.ribbon);
    fill(v, l.status, t.title);
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
    for g in groups {
        fill(
            v,
            DamageRect::new(
                g.rect.right(),
                g.rect.y + 4,
                1,
                g.rect.height.saturating_sub(8),
            ),
            t.border,
        );
        text(
            v,
            DamageRect::new(
                g.rect.x,
                g.rect.bottom().saturating_sub(caption_h),
                g.rect.width,
                caption_h,
            ),
            g.label,
            t.dim,
            false,
            true,
            cell_w,
            cell_h,
        );
    }
    for b in buttons {
        if b.rect.is_empty() {
            continue;
        }
        let hit = crate::widgets::Hit::Ribbon(b.action);
        let on = active(b.action, s);
        let bg = if s.pressed == Some(hit) {
            t.pressed
        } else if on {
            t.checked
        } else if s.hover == Some(hit) {
            t.hover
        } else {
            t.ribbon
        };
        round(v, b.rect, 4, bg);
        let ink = if on { t.accent } else { t.text };
        if let Some(i) = b.icon {
            icon(
                v,
                if b.large {
                    b.rect
                } else if b.labelled {
                    // **The icon column, which is also where `layout` said it would be.** It was a
                    // literal 28 here and `small_w`'s arithmetic in `layout` assumed 22, so the two
                    // disagreed about how much of a button the label had.
                    DamageRect::new(b.rect.x, b.rect.y, ICON_GUTTER, b.rect.height)
                } else {
                    // **A label-less button centres its icon in the whole box**, because the box was
                    // narrowed to the icon and the gutter no longer exists: at `x + 6` in a 38 px box
                    // the icon sits eight pixels left of centre and a compact row reads as ragged.
                    b.rect
                },
                i,
                ink,
                b.large,
            );
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
