//! Native modal dialogs for the desktop driver. No terminal prompt and no toolkit dependency.
//!
//! Secrets are masked by the driver before a view reaches this module, and masked again here so a
//! caller that forgets cannot put a passphrase into the surface tree. The mask is ASCII `*`: the
//! atlas covers ASCII and Latin-1, and U+2022 BULLET is outside that coverage. A missing glyph
//! advances the pen without drawing, which is exactly how "typing shows nothing" happened.
//!
//! The geometry is responsive: it scales with the window and the chrome's measured cell, keeps a
//! minimum hit target, and clips long labels/values to the panel instead of drawing over the scrim.

use crate::tree::{Label, Node, Rect, RoundRect, Style, SurfaceTree};
use crate::{DamageRect, Layout};

/// The secret mask. ASCII so every atlas size/style carries it; see the module note on U+2022.
pub const MASK: char = '*';

// The framebuffer is opaque; use a soft neutral backdrop rather than pretending alpha blends.
const SCRIM: u32 = 0xFFE9_EEF5;
const PANEL: u32 = 0xFFFF_FFFF;
const FIELD_BG: u32 = 0xFFFF_FFFF;
const FIELD_BORDER: u32 = 0xFFD0_D5DE;
const ACCENT: u32 = 0xFF7A_A2F7;
const TEXT: u32 = 0xFF20_2124;
const TEXT_SECONDARY: u32 = 0xFF5F_6368;
const ERROR: u32 = 0xFFB3_261E;
const PRIMARY: u32 = 0xFF1A_73E8;
const SECONDARY: u32 = 0xFFF0_F4F9;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub label: String,
    pub value: String,
    pub secret: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogView {
    pub title: String,
    pub message: String,
    pub fields: Vec<Field>,
    pub focus: usize,
    pub error: Option<String>,
    pub confirm: String,
    pub cancel: Option<String>,
    pub choices: Vec<String>,
    pub choice: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    Field(usize),
    Confirm,
    Cancel,
    Choice(usize),
    None,
}

/// The panel and every hit target, computed once from the layout and the view.
///
/// `text_size` is an atlas size index: 1 is the larger of the two boot sizes when the window is big
/// enough to use it, 0 otherwise. The painter clamps it to the sizes the atlas actually holds.
#[derive(Debug, Clone, Copy)]
pub struct Geometry {
    pub panel: DamageRect,
    pub fields: usize,
    pub row_h: u32,
    pub confirm: DamageRect,
    pub cancel: DamageRect,
    pub choice_start: usize,
    pub visible_choices: usize,
    pub pad: u32,
    pub text_h: u32,
    pub field_h: u32,
    pub button_h: u32,
    pub header_h: u32,
    pub footer_h: u32,
    pub error_h: u32,
    pub field_offset: u32,
    pub text_size: u8,
    pub text_advance: u32,
    pub value_advance: u32,
}

impl Geometry {
    /// The input box for field `i`, in panel coordinates.
    pub fn field_rect(&self, i: usize) -> DamageRect {
        DamageRect::new(
            self.panel.x + self.pad,
            self.panel.y + self.header_h + i as u32 * self.row_h + self.field_offset,
            self.panel.width.saturating_sub(self.pad * 2),
            self.field_h,
        )
    }

    /// The row for visible choice `i` (an index into `visible_choices`, not `DialogView::choices`).
    pub fn choice_rect(&self, i: usize) -> DamageRect {
        DamageRect::new(
            self.panel.x + self.pad,
            self.panel.y + self.header_h + (self.fields + i) as u32 * self.row_h + self.field_offset,
            self.panel.width.saturating_sub(self.pad * 2),
            self.field_h,
        )
    }

    fn label_y(&self, i: usize) -> u32 {
        self.panel.y + self.header_h + i as u32 * self.row_h
    }
}

/// Responsive dialog metrics. The old panel was a fixed 620x(150+52*rows) with 34 px fields and
/// 100x36 buttons; on a high-DPI desktop that is a postage stamp. This scales the panel with the
/// window, the type with the chrome's measured cell, and keeps every target at least 34 px tall.
pub fn geometry(l: &Layout, d: &DialogView) -> Geometry {
    let margin = 16;
    let max_w = l.width.saturating_sub(margin * 2).max(1);
    let w = (l.width / 2).clamp(420, 840).min(max_w).max(280.min(max_w));

    // A short or narrow window gets the chrome size and compact spacing; a normal desktop window
    // gets the larger atlas size and roomier padding. The painter clamps index 1 to the last size
    // when the atlas was built with only one, so this never asks for a size that cannot be drawn.
    let large = l.width >= 700 && l.height >= 560;
    let compact = l.height < 560 || l.width < 700;
    let text_size = if large { 1 } else { 0 };
    let base_h = l.cell_h.max(18);
    let text_h = if large { (base_h * 5 / 4).max(24) } else { base_h };
    let pad = if compact { 10 } else { base_h.clamp(20, 32) };
    let field_h = if compact { text_h + 8 } else { (text_h + 18).max(44) };
    let button_h = if compact { text_h + 8 } else { (text_h + 16).max(44) };
    let gap = if compact { 4 } else { 14 };
    let field_offset = text_h + if compact { 4 } else { 6 };
    let row_h = field_offset + field_h + gap;
    let header_h = pad + text_h + if compact { 4 } else { 8 } + text_h + gap;
    let footer_h = pad + button_h + pad;
    let error_h = if d.error.is_some() { text_h + 12 } else { 0 };

    let wanted_rows = d.fields.len() + d.choices.len().min(6);
    let desired = header_h + wanted_rows as u32 * row_h + error_h + footer_h;
    let max_h = l.height.saturating_sub(margin * 2).max(1);
    let h = desired.min(max_h).max(160.min(max_h));

    let fixed = header_h + error_h + footer_h;
    let available_rows = (h.saturating_sub(fixed) / row_h.max(1)) as usize;
    let visible_choices = d
        .choices
        .len()
        .min(6)
        .min(available_rows.saturating_sub(d.fields.len()));
    let choice_start = if visible_choices == 0 {
        0
    } else {
        d.choice
            .saturating_sub(visible_choices - 1)
            .min(d.choices.len() - visible_choices)
    };

    let x = (l.width.saturating_sub(w)) / 2;
    let y = (l.height.saturating_sub(h)) / 2;
    let panel = DamageRect::new(x, y, w, h);

    let inner = w.saturating_sub(pad * 2);
    let button_w = (inner.saturating_sub(12) / 2).min(180).max(96).min(inner);
    let by = y + h.saturating_sub(pad + button_h);
    let confirm = DamageRect::new(x + w - pad - button_w, by, button_w, button_h);
    let cancel = DamageRect::new(
        confirm.x.saturating_sub(12 + button_w).max(x + pad),
        by,
        button_w,
        button_h,
    );

    let scale = if large { 5 } else { 4 };
    let text_advance = (l.cell_w.max(6) * scale / 4).max(6);
    let value_advance = text_advance;

    Geometry {
        panel,
        fields: d.fields.len(),
        row_h,
        confirm,
        cancel,
        choice_start,
        visible_choices,
        pad,
        text_h,
        field_h,
        button_h,
        header_h,
        footer_h,
        error_h,
        field_offset,
        text_size,
        text_advance,
        value_advance,
    }
}

pub fn hit(l: &Layout, d: &DialogView, x: i32, y: i32) -> Hit {
    let g = geometry(l, d);
    if g.confirm.contains(x, y) {
        return Hit::Confirm;
    }
    if d.cancel.is_some() && g.cancel.contains(x, y) {
        return Hit::Cancel;
    }
    for i in 0..g.fields {
        if g.field_rect(i).contains(x, y) {
            return Hit::Field(i);
        }
    }
    for i in 0..g.visible_choices {
        if g.choice_rect(i).contains(x, y) {
            return Hit::Choice(g.choice_start + i);
        }
    }
    Hit::None
}

fn label(out: &mut Vec<SurfaceTree>, x: u32, y: u32, s: &str, style: Style, size: u8, colour: u32) {
    out.push(SurfaceTree::leaf(Node::Label(Label::new(
        x as i32, y as i32, s, style, size, colour,
    ))));
}

/// Clip to `max_chars` on a character boundary, with an ASCII ellipsis. The atlas has no U+2026.
fn clip(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    if max_chars <= 3 {
        return text.chars().take(max_chars).collect();
    }
    let mut s: String = text.chars().take(max_chars - 3).collect();
    s.push_str("...");
    s
}

/// The tail of a value, so the caret stays visible when a path or passphrase is longer than the box.
fn tail(text: &str, max_chars: usize) -> String {
    let n = text.chars().count();
    if n <= max_chars {
        return text.to_string();
    }
    text.chars().skip(n - max_chars).collect()
}

pub fn push_tree(tree: &mut SurfaceTree, l: &Layout, d: &DialogView) {
    let g = geometry(l, d);
    let p = g.panel;
    let inner = p.width.saturating_sub(g.pad * 2);
    let max_chars = (inner / g.text_advance).max(1) as usize;
    let value_chars = ((g.field_rect(0).width.saturating_sub(24)) / g.value_advance).max(1) as usize;

    tree.before.push(SurfaceTree::leaf(Node::Rect(Rect::new(
        0, 0, l.width, l.height, SCRIM,
    ))));
    tree.before.push(SurfaceTree::leaf(Node::RoundRect(RoundRect::new(
        p.x as i32, p.y as i32, p.width, p.height, 14, PANEL,
    ))));

    let title_y = p.y + g.pad;
    label(&mut tree.before, p.x + g.pad, title_y, &clip(&d.title, max_chars), Style::BOLD, g.text_size, TEXT);
    let message_y = title_y + g.text_h + if g.text_size == 1 { 8 } else { 4 };
    label(&mut tree.before, p.x + g.pad, message_y, &clip(&d.message, max_chars), Style::REGULAR, g.text_size, TEXT_SECONDARY);

    for (i, f) in d.fields.iter().enumerate() {
        let ly = g.label_y(i);
        label(&mut tree.before, p.x + g.pad, ly, &clip(&f.label, max_chars), Style::REGULAR, g.text_size, TEXT_SECONDARY);
        let r = g.field_rect(i);
        let border = if i == d.focus { ACCENT } else { FIELD_BORDER };
        tree.before.push(SurfaceTree::leaf(Node::RoundRect(RoundRect::new(
            r.x as i32, r.y as i32, r.width, r.height, 8, border,
        ))));
        tree.before.push(SurfaceTree::leaf(Node::RoundRect(RoundRect::new(
            r.x as i32 + 2, r.y as i32 + 2, r.width.saturating_sub(4), r.height.saturating_sub(4), 6, FIELD_BG,
        ))));

        let masked = if f.secret {
            MASK.to_string().repeat(f.value.chars().count())
        } else {
            f.value.clone()
        };
        let shown = tail(&masked, value_chars);
        let vx = r.x + 12;
        let vy = r.y + (r.height.saturating_sub(g.text_h)) / 2;
        label(&mut tree.before, vx, vy, &shown, Style::MONOSPACE, g.text_size, TEXT);
        if i == d.focus {
            let cx = (vx + shown.chars().count() as u32 * g.value_advance)
                .min(r.right().saturating_sub(12));
            tree.before.push(SurfaceTree::leaf(Node::Rect(Rect::new(
                cx as i32, vy as i32, 2, g.text_h.max(16), ACCENT,
            ))));
        }
    }

    for i in 0..g.visible_choices {
        let idx = g.choice_start + i;
        let r = g.choice_rect(i);
        let colour = if idx == d.choice { PRIMARY } else { SECONDARY };
        tree.before.push(SurfaceTree::leaf(Node::RoundRect(RoundRect::new(
            r.x as i32, r.y as i32, r.width, r.height, 8, colour,
        ))));
        let ty = r.y + (r.height.saturating_sub(g.text_h)) / 2;
        label(&mut tree.before, r.x + 12, ty, &clip(&d.choices[idx], max_chars), Style::REGULAR, g.text_size, if idx == d.choice { 0xFFFF_FFFF } else { TEXT });
    }

    if let Some(e) = &d.error {
        let ey = p.y + p.height.saturating_sub(g.footer_h + g.error_h) + 6;
        label(&mut tree.before, p.x + g.pad, ey, &clip(e, max_chars), Style::REGULAR, g.text_size, ERROR);
    }

    for (r, text, primary) in [
        (g.confirm, d.confirm.as_str(), true),
        (g.cancel, d.cancel.as_deref().unwrap_or(""), false),
    ] {
        if text.is_empty() {
            continue;
        }
        tree.before.push(SurfaceTree::leaf(Node::RoundRect(RoundRect::new(
            r.x as i32, r.y as i32, r.width, r.height, 8,
            if primary { PRIMARY } else { SECONDARY },
        ))));
        let button_chars = (r.width.saturating_sub(24) / g.text_advance).max(1) as usize;
        let text = clip(text, button_chars);
        // Centre the label in the button using the same conservative advance the clip used.
        let tw = text.chars().count() as u32 * g.text_advance;
        let tx = r.x + (r.width.saturating_sub(tw)) / 2;
        let ty = r.y + (r.height.saturating_sub(g.text_h)) / 2;
        label(&mut tree.before, tx, ty, &text, Style::BOLD, g.text_size, if primary { 0xFFFF_FFFF } else { TEXT });
    }
}
