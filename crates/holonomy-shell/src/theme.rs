//! Canonical `quiet-md` 0.1.13 page, color, and heading token table (`docs/UI-SPEC.md` §3, §8).
//!
//! `docs/UI-SPEC.md` §8 ("What parity means operationally", rule 1) requires all quoted `quiet-md`
//! page metrics, RGBA palette tokens, and heading scales to live in a single table in
//! `holonomy-shell` with a drift-guard test asserting exact agreement with `docs/UI-SPEC.md`.

/// RGBA8 color token (`0xRRGGBBAA`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba8 {
    /// Red channel (`0..=255`).
    pub r: u8,
    /// Green channel (`0..=255`).
    pub g: u8,
    /// Blue channel (`0..=255`).
    pub b: u8,
    /// Alpha channel (`0..=255`, `255` = opaque; or percentage-derived).
    pub a_pct: u8,
}

impl Rgba8 {
    /// Opaque `#RRGGBB` token (`100%` alpha).
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self {
            r,
            g,
            b,
            a_pct: 100,
        }
    }

    /// `#RRGGBB` token at an explicit integer opacity percentage (`0..=100`).
    #[must_use]
    pub const fn rgba_pct(r: u8, g: u8, b: u8, a_pct: u8) -> Self {
        Self { r, g, b, a_pct }
    }

    /// Formats `#rrggbb` in lowercase hex.
    #[must_use]
    pub fn hex_rgb(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }
}

/// One row of the `docs/UI-SPEC.md` §8 heading scale table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadingToken {
    /// Spec row name (`"title"`, `"h1"`, `"h2"`, `"h3"`, `"h4"`, `"h5–h9"`).
    pub level: &'static str,
    /// Font size in `px`/`pt`.
    pub size_px: u16,
    /// Line height in quarter-pixels (`55.25` -> `221`, `27` -> `108`) so integer exactness holds.
    pub line_qpx: u16,
    /// Top margin in `px`.
    pub margin_top_px: u16,
    /// Bottom margin in `px`.
    pub margin_bottom_px: u16,
    /// CSS/OpenType font weight (`600` or `700`).
    pub weight: u16,
}

/// Single canonical `quiet-md` 0.1.13 parity token table (`docs/UI-SPEC.md` §3 & §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageTheme {
    /// Fixed toolbar height (`UI-SPEC.md` §3: `40 px`).
    pub toolbar_height_px: u16,
    /// Toolbar group gap (`UI-SPEC.md` §3: `8 px`).
    pub toolbar_group_gap_px: u16,
    /// Toolbar overflow collapse threshold (`UI-SPEC.md` §3: `760 px`).
    pub toolbar_overflow_width_px: u16,
    /// Page maximum measure (`page.width`: `820`).
    pub page_max_width_px: u16,
    /// Body font size (`BODY_SIZE`: `15`).
    pub body_size_px: u16,
    /// Body line height (`BODY_LINE`: `22`).
    pub body_line_px: u16,
    /// Paragraph vertical gap (`BLOCK_GAP`: `13`).
    pub paragraph_gap_px: u16,
    /// Caret stroke width in points (`2`).
    pub caret_width_pt: u8,
    /// Page background (`#ffffff`).
    pub bg_body: Rgba8,
    /// Primary body ink (`#1f2225`).
    pub text: Rgba8,
    /// Captions and metadata (`#737477`).
    pub text_caption: Rgba8,
    /// Placeholder text (`#9c9fa0`).
    pub text_placeholder: Rgba8,
    /// Caret accent (`#3a85f7`).
    pub caret: Rgba8,
    /// Selection wash (`#3a85f7` at `24%`).
    pub selection: Rgba8,
    /// Hyperlink ink (`#0f5492`).
    pub link: Rgba8,
    /// Subtle divider (`#000000` at `6%`).
    pub line_divider: Rgba8,
    /// Chrome border (`#000000` at `8%`).
    pub line_border: Rgba8,
    /// Blockquote bar (`#1f2225` at `82%`).
    pub quote_bar: Rgba8,
    /// Code block background (`#fbfbfb`).
    pub code_bg: Rgba8,
    /// Code block gutter (`#f8f8f8`).
    pub code_gutter: Rgba8,
    /// Six heading levels quoted in `UI-SPEC.md` §8.
    pub headings: [HeadingToken; 6],
}

/// Canonical `quiet-md` 0.1.13 token table pinned against `docs/UI-SPEC.md` §3 & §8.
pub const QUIET_MD_THEME: PageTheme = PageTheme {
    toolbar_height_px: 40,
    toolbar_group_gap_px: 8,
    toolbar_overflow_width_px: 760,
    page_max_width_px: 820,
    body_size_px: 15,
    body_line_px: 22,
    paragraph_gap_px: 13,
    caret_width_pt: 2,
    bg_body: Rgba8::rgb(0xff, 0xff, 0xff),
    text: Rgba8::rgb(0x1f, 0x22, 0x25),
    text_caption: Rgba8::rgb(0x73, 0x74, 0x77),
    text_placeholder: Rgba8::rgb(0x9c, 0x9f, 0xa0),
    caret: Rgba8::rgb(0x3a, 0x85, 0xf7),
    selection: Rgba8::rgba_pct(0x3a, 0x85, 0xf7, 24),
    link: Rgba8::rgb(0x0f, 0x54, 0x92),
    line_divider: Rgba8::rgba_pct(0x00, 0x00, 0x00, 6),
    line_border: Rgba8::rgba_pct(0x00, 0x00, 0x00, 8),
    quote_bar: Rgba8::rgba_pct(0x1f, 0x22, 0x25, 82),
    code_bg: Rgba8::rgb(0xfb, 0xfb, 0xfb),
    code_gutter: Rgba8::rgb(0xf8, 0xf8, 0xf8),
    headings: [
        HeadingToken {
            level: "title",
            size_px: 34,
            line_qpx: 221,
            margin_top_px: 0,
            margin_bottom_px: 0,
            weight: 700,
        },
        HeadingToken {
            level: "h1",
            size_px: 21,
            line_qpx: 108,
            margin_top_px: 48,
            margin_bottom_px: 16,
            weight: 700,
        },
        HeadingToken {
            level: "h2",
            size_px: 19,
            line_qpx: 100,
            margin_top_px: 36,
            margin_bottom_px: 16,
            weight: 700,
        },
        HeadingToken {
            level: "h3",
            size_px: 18,
            line_qpx: 96,
            margin_top_px: 28,
            margin_bottom_px: 12,
            weight: 600,
        },
        HeadingToken {
            level: "h4",
            size_px: 16,
            line_qpx: 100,
            margin_top_px: 16,
            margin_bottom_px: 16,
            weight: 600,
        },
        HeadingToken {
            level: "h5–h9",
            size_px: 15,
            line_qpx: 88,
            margin_top_px: 13,
            margin_bottom_px: 13,
            weight: 600,
        },
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    fn format_qpx(qpx: u16) -> String {
        if qpx % 4 == 0 {
            format!("{}", qpx / 4)
        } else {
            format!("{}.{:02}", qpx / 4, (qpx % 4) * 25)
        }
    }

    #[test]
    fn quiet_md_theme_table_matches_ui_spec_without_drift() {
        let spec = include_str!("../../../docs/UI-SPEC.md");
        let t = QUIET_MD_THEME;

        assert!(spec.contains(&format!("Fixed top bar, {} px tall", t.toolbar_height_px)));
        assert!(spec.contains(&format!(
            "| page max width | {} | `page.width` |",
            t.page_max_width_px
        )));
        assert!(spec.contains(&format!(
            "| body size / line height | {} / {} | `BODY_SIZE`, `BODY_LINE` |",
            t.body_size_px, t.body_line_px
        )));
        assert!(spec.contains(&format!(
            "| paragraph gap | {} | `BLOCK_GAP`, `p.gap` |",
            t.paragraph_gap_px
        )));
        assert!(spec.contains(&format!("| page background | `{}` |", t.bg_body.hex_rgb())));
        assert!(spec.contains(&format!("| body text | `{}` |", t.text.hex_rgb())));
        assert!(spec.contains(&format!(
            "| captions, meta | `{}` |",
            t.text_caption.hex_rgb()
        )));
        assert!(spec.contains(&format!(
            "| placeholders | `{}` |",
            t.text_placeholder.hex_rgb()
        )));
        assert!(spec.contains(&format!(
            "| caret | `{}`, {}pt wide |",
            t.caret.hex_rgb(),
            t.caret_width_pt
        )));
        assert!(spec.contains(&format!(
            "| selection | `{}` at {}% |",
            t.selection.hex_rgb(),
            t.selection.a_pct
        )));
        assert!(spec.contains(&format!("| link | `{}` |", t.link.hex_rgb())));
        assert!(spec.contains(&format!(
            "| dividers | black at {}% / {}% |",
            t.line_divider.a_pct, t.line_border.a_pct
        )));
        assert!(spec.contains(&format!("| quote bar | `N900` at {}% |", t.quote_bar.a_pct)));
        assert!(spec.contains(&format!(
            "| code background / gutter | `{}` / `{}` |",
            t.code_bg.hex_rgb(),
            t.code_gutter.hex_rgb()
        )));

        for h in t.headings {
            let expected_row = format!(
                "| {} | {} | {} | {} | {} | {} |",
                h.level,
                h.size_px,
                format_qpx(h.line_qpx),
                h.margin_top_px,
                h.margin_bottom_px,
                h.weight
            );
            assert!(
                spec.contains(&expected_row),
                "docs/UI-SPEC.md drifted from QUIET_MD_THEME row: {expected_row}"
            );
        }
    }
}
