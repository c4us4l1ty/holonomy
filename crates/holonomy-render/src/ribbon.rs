//! Native WordCraft ribbon: shared geometry for painting and hit testing.
//!
//! Design tokens and icon artwork adapted from storytold/wordcraft, MIT. This is
//! deliberately not an egui port: Holonomy keeps its integer, damage-limited
//! framebuffer renderer and its sealed, offline execution model.
use crate::chrome::{ChromeState, Layout};
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
                    title: $m::TITLE,
                    ribbon: $m::RIBBON,
                    canvas: $m::CANVAS,
                    border: $m::BORDER,
                    text: $m::TEXT,
                    dim: $m::DIM,
                    disabled: $m::DISABLED,
                    hover: $m::HOVER,
                    pressed: $m::PRESSED,
                    checked: $m::CHECKED,
                    accent: $m::ACCENT,
                    on_accent: $m::ON_ACCENT,
                    input: $m::INPUT,
                    input_border: $m::INPUT_BORDER,
                    group: $m::GROUP,
                    ruler: $m::RULER,
                    ruler_margin: $m::RULER_MARGIN,
                }
            };
        }
        if dark {
            p!(tokens::dark)
        } else {
            p!(tokens::light)
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
                    (Action::SetStyle(1), "Heading 1", None, true),
                    (Action::SetStyle(3), "Title", None, true),
                    (Action::SetStyle(4), "Subtitle", None, true),
                ],
            ),
            (
                "Editing",
                &[
                    (Action::Find, "Find", Some(IconId::Search), false),
                    (Action::Replace, "Replace", Some(IconId::Replace), false),
                    (Action::SelectAll, "Select all", Some(IconId::Check), false),
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
                    (Action::SetStyle(1), "Heading 1", None, true),
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

pub fn tab_rects(l: &Layout) -> Vec<DamageRect> {
    let mut x = 12;
    TABS.iter()
        .map(|t| {
            let w = (*t as &str).len() as u32 * 8 + 22;
            let r = DamageRect::new(
                x.min(l.width),
                l.menubar.y,
                w.min(l.width.saturating_sub(x)),
                l.menubar.height,
            );
            x = x.saturating_add(w);
            r
        })
        .collect()
}

/// One layout pass is the authority for both ink and pointer targets.
pub fn layout(l: &Layout, state: &ChromeState) -> (Vec<Group>, Vec<Button>) {
    let mut groups = Vec::new();
    let mut buttons = Vec::new();
    if state.ribbon_collapsed {
        return (groups, buttons);
    }
    let tab = usize::from(state.ribbon_tab).min(TABS.len() - 1);
    let mut x = l.toolbar.x + 10;
    let top = l.toolbar.y + 7;
    let h = l.toolbar.height.saturating_sub(10);
    let limit = l.width.saturating_sub(48);
    for (name, items) in specs(tab) {
        let small: Vec<_> = items.iter().filter(|i| !i.3).collect();
        let large: Vec<_> = items.iter().filter(|i| i.3).collect();
        let rows = if small.len() > 5 { 2 } else { 1 };
        let cols = small.len().div_ceil(rows.max(1));
        let small_w = if tab == 1 && *name == "Font" { 82 } else { 76 };
        let w = large
            .iter()
            .map(|i| if i.2.is_none() { 92 } else { 68 })
            .sum::<u32>()
            + cols as u32 * small_w
            + 16;
        let gr = DamageRect::new(x.min(l.width), top, w.min(l.width.saturating_sub(x)), h);
        groups.push(Group {
            label: name,
            rect: gr,
        });
        let mut bx = x + 6;
        for (_, label, icon, _) in large.iter().copied() {
            let bw = if icon.is_none() { 92 } else { 68 };
            let r = if bx + bw <= limit {
                DamageRect::new(bx, top + 3, bw, h.saturating_sub(22))
            } else {
                DamageRect::EMPTY
            };
            buttons.push(Button {
                action: items.iter().find(|i| i.1 == label).unwrap().0,
                label,
                icon,
                rect: r,
                large: true,
            });
            bx += bw;
        }
        for (i, (action, label, icon, _)) in small.iter().copied().enumerate() {
            let col = i / rows;
            let row = i % rows;
            let bw = if action == Action::SelectTool(crate::widgets::Tool::Font) {
                150
            } else if rows == 1 {
                small_w
            } else {
                small_w
            };
            let by = top + 3 + row as u32 * 29;
            let r = if bx + bw <= limit {
                DamageRect::new(bx, by, bw, 27)
            } else {
                DamageRect::EMPTY
            };
            buttons.push(Button {
                action: action,
                label,
                icon,
                rect: r,
                large: false,
            });
            if row + 1 == rows {
                bx += bw;
            }
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

pub fn qat(l: &Layout) -> Vec<(Action, DamageRect)> {
    let y = l.title.y + (l.title.height.saturating_sub(26)) / 2;
    [
        (Action::ToggleAutosave, 112),
        (Action::Save, 30),
        (Action::Undo, 30),
        (Action::Redo, 30),
    ]
    .into_iter()
    .scan(12u32, |x, (a, w)| {
        let r = DamageRect::new(*x, y, w, 26);
        *x += w + 4;
        Some((a, r))
    })
    .collect()
}
pub fn search_rect(l: &Layout) -> DamageRect {
    DamageRect::new(
        l.width.saturating_sub(340),
        l.title.y + 6,
        244,
        l.title.height.saturating_sub(12).min(28),
    )
}
pub fn lock_rect(l: &Layout) -> DamageRect {
    DamageRect::new(
        l.width.saturating_sub(84),
        l.title.y + 6,
        70,
        l.title.height.saturating_sub(12).min(28),
    )
}
pub fn overflow_rect(l: &Layout) -> DamageRect {
    DamageRect::new(l.width.saturating_sub(40), l.toolbar.y + 8, 30, 30)
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
fn text(
    v: &mut Vec<SurfaceTree>,
    r: DamageRect,
    s: &str,
    c: u32,
    bold: bool,
    center: bool,
    cell_h: u32,
) {
    if r.width < 8 || r.height < 8 {
        return;
    }
    let size = 0;
    let advance = 8;
    let max = (r.width.saturating_sub(8) / advance) as usize;
    let mut shown: String = s.chars().take(max).collect();
    if shown.len() < s.len() && shown.len() > 3 {
        shown.pop();
        shown.push_str("...");
    }
    let w = shown.len() as u32 * advance;
    let x = r.x
        + if center {
            r.width.saturating_sub(w) / 2
        } else {
            6
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
            r.x as i32 + 6,
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

pub fn paint(v: &mut Vec<SurfaceTree>, l: &Layout, s: &ChromeState, cell_h: u32) {
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
            text(
                v,
                DamageRect::new(r.x, r.y, 76, r.height),
                "AutoSave",
                t.dim,
                false,
                false,
                cell_h,
            );
            let sw = DamageRect::new(r.x + 78, r.y + 6, 30, 14);
            round(v, sw, 7, if s.autosave { t.accent } else { t.input_border });
            round(
                v,
                DamageRect::new(sw.x + if s.autosave { 17 } else { 3 }, sw.y + 3, 8, 8),
                4,
                t.on_accent,
            );
        } else if let Some(i) = match a {
            Action::Save => Some(IconId::Doc),
            Action::Undo => Some(IconId::Undo),
            Action::Redo => Some(IconId::Redo),
            _ => None,
        } {
            icon(v, r, i, t.dim, false);
        }
    }
    let title = format!("{} — {}", s.title, if s.dirty { "Edited" } else { "Saved" });
    text(
        v,
        DamageRect::new(260, l.title.y, l.width.saturating_sub(600), l.title.height),
        &title,
        t.text,
        true,
        true,
        cell_h,
    );
    round(v, search_rect(l), 5, t.input);
    icon(
        v,
        DamageRect::new(
            search_rect(l).x,
            search_rect(l).y,
            28,
            search_rect(l).height,
        ),
        IconId::Search,
        t.dim,
        false,
    );
    text(
        v,
        DamageRect::new(
            search_rect(l).x + 28,
            search_rect(l).y,
            search_rect(l).width - 28,
            search_rect(l).height,
        ),
        "Find in document  Ctrl+F",
        t.dim,
        false,
        false,
        cell_h,
    );
    round(v, lock_rect(l), 5, t.accent);
    text(v, lock_rect(l), "Lock", t.on_accent, true, true, cell_h);
    for (i, r) in tab_rects(l).into_iter().enumerate() {
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
            cell_h,
        );
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
                g.rect.bottom().saturating_sub(20),
                g.rect.width,
                20,
            ),
            g.label,
            t.group,
            false,
            true,
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
                } else {
                    DamageRect::new(b.rect.x, b.rect.y, 28, b.rect.height)
                },
                i,
                ink,
                b.large,
            );
        }
        let tr = if b.large {
            DamageRect::new(
                b.rect.x,
                b.rect.y + 27,
                b.rect.width,
                b.rect.height.saturating_sub(27),
            )
        } else if b.icon.is_some() {
            DamageRect::new(
                b.rect.x + 28,
                b.rect.y,
                b.rect.width.saturating_sub(28),
                b.rect.height,
            )
        } else {
            b.rect
        };
        let label = if b.action == Action::SelectTool(crate::widgets::Tool::Font) {
            s.font_name()
        } else {
            b.label
        };
        text(
            v,
            tr,
            label,
            ink,
            b.large && b.action == Action::SetStyle(3),
            b.large || b.icon.is_none(),
            cell_h,
        );
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
    if !s.ribbon_collapsed {
        icon(v, overflow_rect(l), IconId::Overflow, t.dim, false);
    }
    // Ruler: white text measure, darker margins, integer ticks.
    fill(v, l.ruler, t.ruler_margin);
    fill(v, l.text_bound_x(), t.ruler);
    let mut x = l.text.x;
    while x < l.text.right() {
        let major = (x - l.text.x) % 80 == 0;
        fill(
            v,
            DamageRect::new(
                x,
                l.ruler.bottom().saturating_sub(if major { 8 } else { 4 }),
                1,
                if major { 8 } else { 4 },
            ),
            t.group,
        );
        x += 10;
    }
    // Status bar. The page/word counters remain visible alongside save notices.
    let rows = l.page_rows();
    let status = format!(
        "Page {} of {}   {} words   UTF-8",
        s.scroll_line / rows + 1,
        l.page_count(s.total_lines),
        s.words
    );
    text(
        v,
        DamageRect::new(8, l.status.y, l.width.saturating_sub(360), l.status.height),
        &status,
        t.dim,
        false,
        false,
        cell_h,
    );
    if let Some(n) = &s.notice {
        text(
            v,
            DamageRect::new(l.width / 2, l.status.y, l.width / 2 - 180, l.status.height),
            n,
            t.dim,
            false,
            true,
            cell_h,
        );
    }
    text(
        v,
        DamageRect::new(
            l.width.saturating_sub(170),
            l.status.y,
            150,
            l.status.height,
        ),
        &format!("{}   {}%", s.mode_name(), s.zoom_percent),
        t.dim,
        false,
        true,
        cell_h,
    );
}

impl Layout {
    fn text_bound_x(self) -> DamageRect {
        DamageRect::new(
            self.text.x,
            self.ruler.y,
            self.text.width.min(self.width.saturating_sub(self.text.x)),
            self.ruler.height,
        )
    }
}
