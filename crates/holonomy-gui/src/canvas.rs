//! `quiet-md` pixel framebuffer surface (`PixelSurface`), toolbar/chrome layout driven by
//! [`holonomy_text::ToolbarState`] (`Law U0`), and interactive pure-`libc` X11 desktop runner.
//!
//! All coordinates painted onto the document canvas are mapped from logical units to device pixels
//! solely through [`holonomy_eye::View::to_device`] so pointer hit-testing and painted glyph
//! positions share one arithmetic (`Law L-8`, `UI-SPEC.md` §3 & §8).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use holonomy_eye::{
    Clusters, Fixed, FontSet, GlyphBitmap, Picture, PictureCluster, Point, Shaper, Style, View,
};
use holonomy_line::WitnessSummary;
use holonomy_seal::{
    probe_capability, CanarySet, LockCapability, PassphraseInput, ReaperGuard, SchedClass,
    SecretArenas, TripReason, Tripwire, MAX_PASSPHRASE_INPUT_BYTES,
};
use holonomy_shell::{Editor, Effect, Intent, Rgba8, QUIET_MD_THEME};
use holonomy_text::{
    Alignment, FontFamily, Heading, ListKind, ResidentSet, SegmentSink, SpineDiffHunk,
    SpineDiffKind, StylePatch, TriState, Typing,
};
use zeroize::{Zeroize, Zeroizing};

use crate::panel::Facts;
use crate::platform::{open_desktop_window, DesktopEvent, DesktopWindow, MAX_CLIPBOARD_BYTES};
use crate::store::{self, Document, HistorySearchHit};
use crate::{App, Key, Keymap, Modifiers, SealWipeBridge, Surface};

const STATUS_BAR_HEIGHT_PX: i32 = 24;
const PAGE_PAD_X_PX: i32 = 44;
const PAGE_PAD_TOP_PX: i32 = 24;
const PAUSE_AUTOSAVE_MS: u64 = 1_500;

#[inline]
const fn rgba_to_xrgb(c: Rgba8) -> u32 {
    ((c.r as u32) << 16) | ((c.g as u32) << 8) | (c.b as u32)
}

#[inline]
fn blend_pixel(dst: u32, r: u8, g: u8, b: u8, alpha255: u8) -> u32 {
    if alpha255 == 0 {
        return dst;
    }
    if alpha255 == 255 {
        return ((r as u32) << 16) | ((g as u32) << 8) | (b as u32);
    }
    let a = u32::from(alpha255);
    let inv = 255 - a;
    let dr = (dst >> 16) & 0xff;
    let dg = (dst >> 8) & 0xff;
    let db = dst & 0xff;
    let nr = (u32::from(r) * a + dr * inv) / 255;
    let ng = (u32::from(g) * a + dg * inv) / 255;
    let nb = (u32::from(b) * a + db * inv) / 255;
    (nr << 16) | (ng << 8) | nb
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Rect {
    const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    const fn contains(self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// CPU `0x00RRGGBB` ARGB pixel canvas with clipped solid fills, alpha washes, and A8 glyph
/// compositing.
#[derive(Debug, Clone)]
pub struct FrameBuffer {
    /// Width in pixels.
    pub width: u16,
    /// Height in pixels.
    pub height: u16,
    /// Row-major `0x00RRGGBB` pixel buffer (`width * height` entries).
    pub pixels: Vec<u32>,
}

impl FrameBuffer {
    /// Allocates a `width × height` buffer cleared to `bg`.
    #[must_use]
    pub fn new(width: u16, height: u16, bg: Rgba8) -> Self {
        let len = usize::from(width) * usize::from(height);
        Self {
            width,
            height,
            pixels: vec![rgba_to_xrgb(bg); len],
        }
    }

    /// Resizes the backing pixel array if dimensions changed and clears to `bg`.
    pub fn resize_and_clear(&mut self, width: u16, height: u16, bg: Rgba8) {
        self.width = width;
        self.height = height;
        let len = usize::from(width) * usize::from(height);
        self.pixels.resize(len, 0);
        self.pixels.fill(rgba_to_xrgb(bg));
    }

    /// Fills `rect` clipped to `clip` and window bounds using `color`'s alpha percentage.
    fn fill_rect_clipped(&mut self, rect: Rect, color: Rgba8, clip: Rect) {
        let x0 = rect.x.max(clip.x).max(0);
        let y0 = rect.y.max(clip.y).max(0);
        let x1 = (rect.x + rect.w)
            .min(clip.x + clip.w)
            .min(i32::from(self.width));
        let y1 = (rect.y + rect.h)
            .min(clip.y + clip.h)
            .min(i32::from(self.height));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let a255 = ((u16::from(color.a_pct.min(100)) * 255 + 50) / 100) as u8;
        if a255 == 0 {
            return;
        }
        let stride = usize::from(self.width);
        let solid = rgba_to_xrgb(color);
        for row in y0..y1 {
            let base = (row as usize) * stride;
            let row_slice = &mut self.pixels[base + (x0 as usize)..base + (x1 as usize)];
            if a255 == 255 {
                row_slice.fill(solid);
            } else {
                for px in row_slice {
                    *px = blend_pixel(*px, color.r, color.g, color.b, a255);
                }
            }
        }
    }

    /// Draws a 1 px stroked outline inside `rect`.
    fn stroke_rect(&mut self, rect: Rect, color: Rgba8, clip: Rect) {
        self.fill_rect_clipped(Rect::new(rect.x, rect.y, rect.w, 1), color, clip);
        self.fill_rect_clipped(
            Rect::new(rect.x, rect.y + rect.h - 1, rect.w, 1),
            color,
            clip,
        );
        self.fill_rect_clipped(Rect::new(rect.x, rect.y, 1, rect.h), color, clip);
        self.fill_rect_clipped(
            Rect::new(rect.x + rect.w - 1, rect.y, 1, rect.h),
            color,
            clip,
        );
    }

    /// Alpha-blends a rasterized A8 glyph bitmap at `(dst_x, dst_y)`.
    fn blend_glyph(&mut self, dst_x: i32, dst_y: i32, bmp: &GlyphBitmap, color: Rgba8, clip: Rect) {
        if bmp.width == 0 || bmp.height == 0 {
            return;
        }
        let x0 = dst_x.max(clip.x).max(0);
        let y0 = dst_y.max(clip.y).max(0);
        let x1 = (dst_x + bmp.width as i32)
            .min(clip.x + clip.w)
            .min(i32::from(self.width));
        let y1 = (dst_y + bmp.height as i32)
            .min(clip.y + clip.h)
            .min(i32::from(self.height));
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let color_a = u16::from(color.a_pct.min(100));
        let stride = usize::from(self.width);
        for py in y0..y1 {
            let gy = (py - dst_y) as usize;
            let row_off = (py as usize) * stride;
            let g_off = gy * bmp.width;
            for px in x0..x1 {
                let gx = (px - dst_x) as usize;
                let cov = bmp.coverage[g_off + gx];
                if cov == 0 {
                    continue;
                }
                let eff_a = if color_a == 100 {
                    cov
                } else {
                    ((u16::from(cov) * color_a) / 100) as u8
                };
                self.pixels[row_off + (px as usize)] = blend_pixel(
                    self.pixels[row_off + (px as usize)],
                    color.r,
                    color.g,
                    color.b,
                    eff_a,
                );
            }
        }
    }
}

/// Draws a single line of text at `baseline_y` using `FontSet` OpenType `GSUB`/`GPOS` shaping at
/// `(zoom_milli = 1000, dpi = 96)`.
#[allow(clippy::too_many_arguments)]
fn draw_ui_text(
    fb: &mut FrameBuffer,
    fonts: &FontSet,
    x: i32,
    baseline_y: i32,
    text: &str,
    style: Style,
    color: Rgba8,
    clip: Rect,
) -> i32 {
    let clusters: Vec<&str> = Clusters::new(text).map(|(_, c)| c).collect();
    if clusters.is_empty() {
        return x;
    }
    let shaped = fonts.shape_run(&clusters, style);
    let mut pen_64ths = i64::from(x) * 64;
    for (variant, g) in shaped {
        let xo = fonts.scale_units(variant, g.x_offset, style).sixtyfourths();
        let yo = (fonts.scale_units(variant, g.y_offset, style).sixtyfourths() + 32) >> 6;
        let adv = fonts
            .scale_units(variant, g.x_advance, style)
            .sixtyfourths();
        let px = ((pen_64ths + xo + 32) >> 6) as i32;
        if let Some(bmp) = fonts.rasterize_glyph(variant, g.glyph_id, style, 1_000, 96) {
            fb.blend_glyph(
                px + bmp.bearing_x,
                baseline_y - (yo as i32) - bmp.bearing_y,
                &bmp,
                color,
                clip,
            );
        }
        pen_64ths += adv;
    }
    ((pen_64ths + 32) >> 6) as i32
}

/// Pixel-exact implementation of [`Surface`] rendering the Eye's [`Picture`](holonomy_eye::Picture)
/// onto a `0x00RRGGBB` [`FrameBuffer`] using embedded OpenType outlines from [`FontSet`].
pub struct PixelSurface {
    fonts: FontSet,
    view: View,
    fb: FrameBuffer,
    page_origin_x: i32,
    page_origin_y: i32,
    page_measure_px: i32,
    canvas_clip: Rect,
    painted_clusters: usize,
    painted_selections: usize,
    caret_rect: Option<(i32, i32, i32, i32)>,
}

impl PixelSurface {
    /// Creates a [`PixelSurface`] for a `width × height` window.
    #[must_use]
    pub fn new(fonts: FontSet, view: View, width: u16, height: u16) -> Self {
        let mut s = Self {
            fonts,
            view,
            fb: FrameBuffer::new(width.max(320), height.max(240), QUIET_MD_THEME.bg_body),
            page_origin_x: 0,
            page_origin_y: i32::from(QUIET_MD_THEME.toolbar_height_px) + PAGE_PAD_TOP_PX,
            page_measure_px: i32::from(QUIET_MD_THEME.page_max_width_px),
            canvas_clip: Rect::new(0, 0, i32::from(width), i32::from(height)),
            painted_clusters: 0,
            painted_selections: 0,
            caret_rect: None,
        };
        s.recompute_geometry(width.max(320), height.max(240));
        s
    }

    /// Updates zoom/DPI and recomputes centered `820 px` page column margins.
    pub fn set_viewport(&mut self, view: View, width: u16, height: u16) {
        self.view = view;
        self.recompute_geometry(width.max(320), height.max(240));
        self.fb
            .resize_and_clear(width.max(320), height.max(240), QUIET_MD_THEME.bg_body);
    }

    fn recompute_geometry(&mut self, width: u16, height: u16) {
        let w = i32::from(width);
        let h = i32::from(height);
        let top = i32::from(QUIET_MD_THEME.toolbar_height_px);
        let canvas_h = (h - top - STATUS_BAR_HEIGHT_PX).max(60);
        let max_page = i32::from(QUIET_MD_THEME.page_max_width_px);
        let measure = (w - 2 * PAGE_PAD_X_PX).clamp(240, max_page);
        let side_margin = ((w - measure) / 2).max(16);
        self.page_measure_px = measure;
        self.page_origin_x = side_margin;
        self.page_origin_y = top + PAGE_PAD_TOP_PX;
        self.canvas_clip = Rect::new(0, top, w, canvas_h);
    }

    /// Width and height of the full window framebuffer in pixels.
    #[must_use]
    pub const fn window_size(&self) -> (u16, u16) {
        (self.fb.width, self.fb.height)
    }

    /// Returns `(page_origin_x, page_origin_y)` in window pixels for pointer hit-test conversion.
    #[must_use]
    pub const fn page_origin(&self) -> (i32, i32) {
        (self.page_origin_x, self.page_origin_y)
    }

    /// Returns the logical document wrap width and canvas height matching this surface and `view`.
    #[must_use]
    pub fn logical_editor_extent(&self) -> (Fixed, Fixed) {
        let num_w = i64::from(self.page_measure_px) * 96_000;
        let den = (self.view.zoom_milli() * self.view.dpi()).max(1);
        let canvas_h = (self.canvas_clip.h - PAGE_PAD_TOP_PX).max(32);
        let num_h = i64::from(canvas_h) * 96_000;
        (Fixed::units(num_w / den), Fixed::units(num_h / den))
    }

    /// Translates a window-space pointer pixel `(wx, wy)` inside the canvas into the device point
    /// expected by [`Editor::click`] / [`Editor::drag_to`], or `None` if outside the canvas.
    #[must_use]
    pub fn canvas_point(&self, wx: i16, wy: i16) -> Option<Point> {
        let px = i32::from(wx);
        let py = i32::from(wy);
        if !self.canvas_clip.contains(px, py) {
            return None;
        }
        Some(Point::new(
            Fixed::units(i64::from(px - self.page_origin_x)),
            Fixed::units(i64::from(py - self.page_origin_y)),
        ))
    }

    /// Raw `0x00RRGGBB` pixel framebuffer slice.
    #[must_use]
    pub fn pixels(&self) -> &[u32] {
        &self.fb.pixels
    }

    /// Mutable access to the backing [`FrameBuffer`] for overlaying chrome/panels.
    pub fn framebuffer_mut(&mut self) -> &mut FrameBuffer {
        &mut self.fb
    }

    /// Embedded [`FontSet`] shared with layout.
    #[must_use]
    pub const fn fonts(&self) -> &FontSet {
        &self.fonts
    }

    /// Number of text clusters painted during the most recent frame.
    #[must_use]
    pub const fn painted_clusters(&self) -> usize {
        self.painted_clusters
    }

    /// Number of selection spans painted during the most recent frame.
    #[must_use]
    pub const fn painted_selections(&self) -> usize {
        self.painted_selections
    }

    /// Device-space rectangle `(x, y, w, h)` of the caret painted in the most recent frame.
    #[must_use]
    pub const fn caret_rect(&self) -> Option<(i32, i32, i32, i32)> {
        self.caret_rect
    }

    #[inline]
    fn to_px(&self, logical: Fixed) -> i32 {
        let px = (self.view.to_device(logical).sixtyfourths() + 32) >> 6;
        i32::try_from(px.clamp(i64::from(i32::MIN), i64::from(i32::MAX))).unwrap_or(0)
    }

    fn draw_empty_placeholder(&mut self) {
        let style = Style::DEFAULT;
        let ascent = self.to_px(self.fonts.ascent(style));
        let baseline = self.page_origin_y + ascent;
        draw_ui_text(
            &mut self.fb,
            &self.fonts,
            self.page_origin_x,
            baseline,
            "Begin writing\u{2026}",
            style,
            QUIET_MD_THEME.text_placeholder,
            self.canvas_clip,
        );
    }
}

impl Surface for PixelSurface {
    fn begin(&mut self) {
        self.painted_clusters = 0;
        self.painted_selections = 0;
        self.caret_rect = None;
        let bg_desk = Rgba8::rgb(0xf6, 0xf7, 0xf9);
        self.fb.fill_rect_clipped(
            self.canvas_clip,
            if self.page_origin_x > 24 {
                bg_desk
            } else {
                QUIET_MD_THEME.bg_body
            },
            self.canvas_clip,
        );
        // Distinct quiet-md white page sheet when desk margins exist.
        if self.page_origin_x > 24 {
            let sheet = Rect::new(
                self.page_origin_x - 24,
                self.canvas_clip.y + 8,
                self.page_measure_px + 48,
                (self.canvas_clip.h - 16).max(32),
            );
            self.fb
                .fill_rect_clipped(sheet, QUIET_MD_THEME.bg_body, self.canvas_clip);
            self.fb
                .stroke_rect(sheet, QUIET_MD_THEME.line_border, self.canvas_clip);
        }
    }

    fn size(&mut self, _width: Fixed, _height: Fixed) {}

    fn selection(&mut self, x: Fixed, y: Fixed, width: Fixed, height: Fixed) {
        self.painted_selections += 1;
        let dx = self.page_origin_x + self.to_px(x);
        let dy = self.page_origin_y + self.to_px(y);
        let dw = self.to_px(x + width) - self.to_px(x);
        let dh = self.to_px(height).max(1);
        self.fb.fill_rect_clipped(
            Rect::new(dx, dy, dw.max(1), dh),
            QUIET_MD_THEME.selection,
            self.canvas_clip,
        );
    }

    fn cluster(&mut self, x: Fixed, y: Fixed, text: &str, advance: Fixed, style: Style) {
        self.painted_clusters += 1;
        let dx = self.page_origin_x + self.to_px(x);
        let dy = self.page_origin_y + self.to_px(y);
        let adv_px = (self.to_px(x + advance) - self.to_px(x)).max(0);
        let line_h_px = self.to_px(self.fonts.line_height(style)).max(1);
        let ascent_px = self.to_px(self.fonts.ascent(style));
        let baseline_y = dy + ascent_px;

        if style.blockquote() > 0 && x <= Fixed::units(24) {
            for level in 0..i32::from(style.blockquote().min(4)) {
                self.fb.fill_rect_clipped(
                    Rect::new(
                        self.page_origin_x - 14 - level * 8,
                        dy + 2,
                        3,
                        (line_h_px - 4).max(4),
                    ),
                    QUIET_MD_THEME.quote_bar,
                    self.canvas_clip,
                );
            }
        }

        if style.is_horizontal_rule() {
            if x <= Fixed::units(16) {
                self.fb.fill_rect_clipped(
                    Rect::new(
                        self.page_origin_x,
                        dy + line_h_px / 2,
                        self.page_measure_px,
                        1,
                    ),
                    QUIET_MD_THEME.line_divider,
                    self.canvas_clip,
                );
            }
            return;
        }

        if style.is_code_span() && adv_px > 0 {
            self.fb.fill_rect_clipped(
                Rect::new(dx, dy + 1, adv_px, (line_h_px - 2).max(4)),
                QUIET_MD_THEME.code_bg,
                self.canvas_clip,
            );
        }

        let ink = if style.link_id().is_some() {
            QUIET_MD_THEME.link
        } else {
            QUIET_MD_THEME.text
        };

        let mut pen_x_64 = self.view.to_device(x).sixtyfourths();
        let mut tmp = [0u8; 4];
        for ch in text.chars() {
            if ch == '\n' || ch == '\r' {
                continue;
            }
            let s = ch.encode_utf8(&mut tmp);
            let ch_adv = self
                .view
                .to_device(self.fonts.advance(s, style))
                .sixtyfourths();
            let gx = self.page_origin_x + (((pen_x_64 + 32) >> 6) as i32);
            if let Some(bmp) =
                self.fonts
                    .rasterize_char(style, self.view.zoom_milli(), self.view.dpi(), ch)
            {
                self.fb.blend_glyph(
                    gx + bmp.bearing_x,
                    baseline_y - bmp.bearing_y,
                    &bmp,
                    ink,
                    self.canvas_clip,
                );
            }
            pen_x_64 += ch_adv;
        }

        if style.is_underline() && adv_px > 0 {
            self.fb.fill_rect_clipped(
                Rect::new(dx, baseline_y + 2, adv_px, 1),
                ink,
                self.canvas_clip,
            );
        }
        if style.is_strikethrough() && adv_px > 0 {
            let mid = baseline_y - (ascent_px / 3).max(2);
            self.fb
                .fill_rect_clipped(Rect::new(dx, mid, adv_px, 1), ink, self.canvas_clip);
        }
    }

    fn line_clusters(&mut self, y: Fixed, text: &str, origin: u64, clusters: &[PictureCluster]) {
        if clusters.is_empty() {
            return;
        }
        let dy = self.page_origin_y + self.to_px(y);
        let mut run_start = 0usize;
        while run_start < clusters.len() {
            let style = clusters[run_start].style;
            let mut run_end = run_start + 1;
            while run_end < clusters.len() && clusters[run_end].style == style {
                run_end += 1;
            }
            let run = &clusters[run_start..run_end];
            self.painted_clusters += run.len();

            let line_h_px = self.to_px(self.fonts.line_height(style)).max(1);
            let ascent_px = self.to_px(self.fonts.ascent(style));
            let baseline_y = dy + ascent_px;

            if style.blockquote() > 0 && run[0].x <= Fixed::units(24) {
                for level in 0..i32::from(style.blockquote().min(4)) {
                    self.fb.fill_rect_clipped(
                        Rect::new(
                            self.page_origin_x - 14 - level * 8,
                            dy + 2,
                            3,
                            (line_h_px - 4).max(4),
                        ),
                        QUIET_MD_THEME.quote_bar,
                        self.canvas_clip,
                    );
                }
            }

            if style.is_horizontal_rule() {
                if run[0].x <= Fixed::units(16) {
                    self.fb.fill_rect_clipped(
                        Rect::new(
                            self.page_origin_x,
                            dy + line_h_px / 2,
                            self.page_measure_px,
                            1,
                        ),
                        QUIET_MD_THEME.line_divider,
                        self.canvas_clip,
                    );
                }
                run_start = run_end;
                continue;
            }

            let run_left = run[0].x;
            let run_right = run.last().map_or(run_left, PictureCluster::end);
            let run_dx = self.page_origin_x + self.to_px(run_left);
            let run_adv_px = (self.to_px(run_right) - self.to_px(run_left)).max(0);

            if style.is_code_span() && run_adv_px > 0 {
                self.fb.fill_rect_clipped(
                    Rect::new(run_dx, dy + 1, run_adv_px, (line_h_px - 2).max(4)),
                    QUIET_MD_THEME.code_bg,
                    self.canvas_clip,
                );
            }

            let ink = if style.link_id().is_some() {
                QUIET_MD_THEME.link
            } else {
                QUIET_MD_THEME.text
            };

            let is_rtl = run.len() >= 2 && run[0].byte > run[run.len() - 1].byte;
            let slices: Vec<&str> = if is_rtl {
                run.iter()
                    .rev()
                    .map(|c| {
                        let from = usize::try_from(c.byte.saturating_sub(origin)).unwrap_or(0);
                        text.get(from..from + c.len).unwrap_or_default()
                    })
                    .collect()
            } else {
                run.iter()
                    .map(|c| {
                        let from = usize::try_from(c.byte.saturating_sub(origin)).unwrap_or(0);
                        text.get(from..from + c.len).unwrap_or_default()
                    })
                    .collect()
            };

            let shaped = self.fonts.shape_run(&slices, style);
            let mut last_cluster = usize::MAX;
            let mut pen_in_cluster_64 = 0i64;
            for (variant, g) in shaped {
                if g.cluster >= run.len() {
                    continue;
                }
                if g.cluster != last_cluster {
                    last_cluster = g.cluster;
                    pen_in_cluster_64 = 0;
                }
                let span = g.cluster_span.max(1).min(run.len() - g.cluster);
                let vis_idx = if is_rtl {
                    run.len() - (g.cluster + span)
                } else {
                    g.cluster
                };
                let base_x = run[vis_idx].x;
                let xo_log = self.fonts.scale_units(variant, g.x_offset, style);
                let yo_log = self.fonts.scale_units(variant, g.y_offset, style);
                let adv_log = self.fonts.scale_units(variant, g.x_advance, style);

                let gx_64 = self.view.to_device(base_x + xo_log).sixtyfourths() + pen_in_cluster_64;
                let gy_off = ((self.view.to_device(yo_log).sixtyfourths() + 32) >> 6) as i32;
                let gx = self.page_origin_x + (((gx_64 + 32) >> 6) as i32);

                if let Some(bmp) = self.fonts.rasterize_glyph(
                    variant,
                    g.glyph_id,
                    style,
                    self.view.zoom_milli(),
                    self.view.dpi(),
                ) {
                    self.fb.blend_glyph(
                        gx + bmp.bearing_x,
                        baseline_y - gy_off - bmp.bearing_y,
                        &bmp,
                        ink,
                        self.canvas_clip,
                    );
                }
                pen_in_cluster_64 += self.view.to_device(adv_log).sixtyfourths();
            }

            if style.is_underline() && run_adv_px > 0 {
                self.fb.fill_rect_clipped(
                    Rect::new(run_dx, baseline_y + 2, run_adv_px, 1),
                    ink,
                    self.canvas_clip,
                );
            }
            if style.is_strikethrough() && run_adv_px > 0 {
                let mid = baseline_y - (ascent_px / 3).max(2);
                self.fb.fill_rect_clipped(
                    Rect::new(run_dx, mid, run_adv_px, 1),
                    ink,
                    self.canvas_clip,
                );
            }
            run_start = run_end;
        }
    }

    fn caret(&mut self, x: Fixed, y: Fixed, height: Fixed) {
        let dx = self.page_origin_x + self.to_px(x);
        let dy = self.page_origin_y + self.to_px(y);
        let w = i32::from(QUIET_MD_THEME.caret_width_pt).max(1);
        let h = self.to_px(height).max(8);
        let rect = Rect::new(dx, dy, w, h);
        self.fb
            .fill_rect_clipped(rect, QUIET_MD_THEME.caret, self.canvas_clip);
        self.caret_rect = Some((dx, dy, w, h));
    }
}

/// Interactive action triggered by clicking a Ribbon button (`UI-SPEC.md` §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarAction {
    /// Cycle font family (`Sans` -> `Serif` -> `Mono`).
    CycleFamily,
    /// Toggle bold.
    ToggleBold,
    /// Toggle italic.
    ToggleItalic,
    /// Toggle underline.
    ToggleUnderline,
    /// Toggle strikethrough.
    ToggleStrike,
    /// Toggle inline code span.
    ToggleCode,
    /// Toggle `Heading::H1`.
    Heading1,
    /// Toggle `Heading::H2`.
    Heading2,
    /// Toggle `Heading::H3`.
    Heading3,
    /// Cycle alignment.
    CycleAlign,
    /// Toggle bullet list.
    ToggleBullet,
    /// Toggle ordered list.
    ToggleNumbered,
    /// Cycle blockquote depth.
    CycleQuote,
    /// Insert / toggle horizontal rule.
    ToggleHr,
    /// Undo edit burst.
    Undo,
    /// Redo edit burst.
    Redo,
    /// Decrease zoom (`-10%`, floor `50%`).
    ZoomOut,
    /// Increase zoom (`+10%`, cap `250%`).
    ZoomIn,
    /// Toggle Block 8 read-only history timeline drawer.
    ToggleHistory,
    /// Toggle live Security Panel (`Ctrl+G`).
    ToggleSecurity,
}

#[derive(Debug, Clone, Copy)]
struct ToolbarHit {
    rect: Rect,
    action: ToolbarAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SecurityDotState {
    Protected,
    Unprotected,
    Tripped,
}

impl SecurityDotState {
    fn from_cap(cap: LockCapability, tripped: bool) -> Self {
        if tripped {
            Self::Tripped
        } else if matches!(
            cap,
            LockCapability::DirectMapExcluded
                | LockCapability::ResidentNoDump
                | LockCapability::Resident
        ) {
            Self::Protected
        } else {
            Self::Unprotected
        }
    }

    const fn color(self) -> Rgba8 {
        match self {
            Self::Protected => Rgba8::rgb(0x22, 0xa0, 0x6b),
            Self::Unprotected => Rgba8::rgb(0xe2, 0xa0, 0x3f),
            Self::Tripped => Rgba8::rgb(0xd9, 0x30, 0x25),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ButtonHighlight {
    Off,
    Active,
    Mixed,
}

#[allow(clippy::too_many_arguments)]
fn draw_toolbar_button(
    fb: &mut FrameBuffer,
    fonts: &FontSet,
    x: i32,
    y: i32,
    label: &str,
    highlight: ButtonHighlight,
    disabled: bool,
    clip: Rect,
) -> (Rect, i32) {
    let style = Style::at(880);
    let label_w = fonts.advance(label, style).sixtyfourths() >> 6;
    let w = (label_w as i32 + 16).max(26);
    let h = 26;
    let rect = Rect::new(x, y, w, h);
    if !disabled {
        match highlight {
            ButtonHighlight::Active => {
                fb.fill_rect_clipped(rect, Rgba8::rgba_pct(0x3a, 0x85, 0xf7, 16), clip);
                fb.stroke_rect(rect, Rgba8::rgba_pct(0x3a, 0x85, 0xf7, 45), clip);
            }
            ButtonHighlight::Mixed => {
                fb.fill_rect_clipped(rect, Rgba8::rgba_pct(0x00, 0x00, 0x00, 5), clip);
                fb.stroke_rect(rect, QUIET_MD_THEME.line_border, clip);
            }
            ButtonHighlight::Off => {}
        }
    }
    let ink = if disabled {
        QUIET_MD_THEME.text_placeholder
    } else if matches!(highlight, ButtonHighlight::Active) {
        QUIET_MD_THEME.link
    } else {
        QUIET_MD_THEME.text
    };
    let text_w = label_w as i32;
    let tx = x + (w - text_w) / 2;
    let ty = y + 17;
    draw_ui_text(fb, fonts, tx, ty, label, style, ink, clip);
    if !disabled && matches!(highlight, ButtonHighlight::Mixed) {
        fb.fill_rect_clipped(
            Rect::new(x + w / 2 - 4, y + h - 4, 8, 2),
            QUIET_MD_THEME.text_caption,
            clip,
        );
    }
    (rect, x + w + 4)
}

/// Renders the fixed `40 px` top Ribbon (`UI-SPEC.md` §3, Law `U0`) directly from `app.editor().toolbar_state()`
/// and returns hit boxes for every clickable toolbar control.
fn render_toolbar<S, R>(
    app: &mut App<S, R, FontSet, PixelSurface>,
    dot_state: SecurityDotState,
    read_only_history: bool,
    history_open: bool,
) -> Vec<ToolbarHit>
where
    S: holonomy_text::SegmentSink,
    R: holonomy_text::SegmentSource,
{
    let tb = app.editor().toolbar_state();
    let zoom_pct = (app.editor().view().zoom_milli() / 10).clamp(50, 300);
    let surface = app.surface_mut();
    let (win_w, _) = surface.window_size();
    let fonts = surface.fonts().clone();
    let fb = surface.framebuffer_mut();

    let bar_h = i32::from(QUIET_MD_THEME.toolbar_height_px);
    let bar = Rect::new(0, 0, i32::from(win_w), bar_h);
    fb.fill_rect_clipped(bar, QUIET_MD_THEME.bg_body, bar);
    fb.fill_rect_clipped(
        Rect::new(0, bar_h - 1, i32::from(win_w), 1),
        QUIET_MD_THEME.line_divider,
        bar,
    );

    let bool_hl = |st: &TriState<bool>| match st {
        TriState::Uniform(true) => ButtonHighlight::Active,
        TriState::Uniform(false) => ButtonHighlight::Off,
        TriState::Mixed => ButtonHighlight::Mixed,
    };
    let fam_label = match tb.family {
        TriState::Uniform(FontFamily::Sans) => "Sans",
        TriState::Uniform(FontFamily::Serif) => "Serif",
        TriState::Uniform(FontFamily::Mono) => "Mono",
        TriState::Mixed => "\u{2014}",
    };
    let h_hl = |target: Heading| match tb.heading {
        TriState::Uniform(h) if h == target => ButtonHighlight::Active,
        TriState::Mixed => ButtonHighlight::Mixed,
        _ => ButtonHighlight::Off,
    };
    let align_label = match tb.alignment {
        TriState::Uniform(Alignment::Start) => "L",
        TriState::Uniform(Alignment::Center) => "C",
        TriState::Uniform(Alignment::End) => "R",
        TriState::Uniform(Alignment::Justify) => "J",
        TriState::Mixed => "\u{2014}",
    };
    let bullet_hl = match tb.list {
        TriState::Uniform(ListKind::Bullet { .. }) => ButtonHighlight::Active,
        TriState::Mixed => ButtonHighlight::Mixed,
        _ => ButtonHighlight::Off,
    };
    let num_hl = match tb.list {
        TriState::Uniform(ListKind::Ordered { .. }) => ButtonHighlight::Active,
        TriState::Mixed => ButtonHighlight::Mixed,
        _ => ButtonHighlight::Off,
    };
    let quote_hl = match tb.blockquote {
        TriState::Uniform(d) if d > 0 => ButtonHighlight::Active,
        TriState::Mixed => ButtonHighlight::Mixed,
        _ => ButtonHighlight::Off,
    };

    let dot_w = 24;
    let hist_w = 42;
    let right_reserved = dot_w + hist_w + 18;
    let max_left_x = i32::from(win_w) - right_reserved;
    let narrow = win_w < QUIET_MD_THEME.toolbar_overflow_width_px;

    let mut hits = Vec::new();
    let mut x = 12;
    let y = 7;
    let gap = i32::from(QUIET_MD_THEME.toolbar_group_gap_px);

    let items: &[(ToolbarAction, &str, ButtonHighlight, bool, bool)] = &[
        (
            ToolbarAction::CycleFamily,
            fam_label,
            if matches!(tb.family, TriState::Mixed) {
                ButtonHighlight::Mixed
            } else {
                ButtonHighlight::Off
            },
            false,
            false,
        ),
        (
            ToolbarAction::Heading1,
            "H1",
            h_hl(Heading::H1),
            false,
            false,
        ),
        (
            ToolbarAction::Heading2,
            "H2",
            h_hl(Heading::H2),
            false,
            false,
        ),
        (
            ToolbarAction::Heading3,
            "H3",
            h_hl(Heading::H3),
            true,
            false,
        ),
        (
            ToolbarAction::ToggleBold,
            "B",
            bool_hl(&tb.bold),
            false,
            false,
        ),
        (
            ToolbarAction::ToggleItalic,
            "I",
            bool_hl(&tb.italic),
            false,
            false,
        ),
        (
            ToolbarAction::ToggleUnderline,
            "U",
            bool_hl(&tb.underline),
            false,
            false,
        ),
        (
            ToolbarAction::ToggleStrike,
            "S",
            bool_hl(&tb.strikethrough),
            false,
            false,
        ),
        (
            ToolbarAction::ToggleCode,
            "</>",
            bool_hl(&tb.code_span),
            true,
            false,
        ),
        (
            ToolbarAction::ToggleBullet,
            "\u{2022}",
            bullet_hl,
            false,
            false,
        ),
        (ToolbarAction::ToggleNumbered, "1.", num_hl, false, false),
        (
            ToolbarAction::CycleQuote,
            "\u{275D}",
            quote_hl,
            false,
            false,
        ),
        (
            ToolbarAction::CycleAlign,
            align_label,
            if matches!(tb.alignment, TriState::Mixed) {
                ButtonHighlight::Mixed
            } else {
                ButtonHighlight::Off
            },
            false,
            true,
        ),
        (
            ToolbarAction::ToggleHr,
            "\u{2014}",
            bool_hl(&tb.horizontal_rule),
            true,
            true,
        ),
        (
            ToolbarAction::Undo,
            "Undo",
            ButtonHighlight::Off,
            false,
            true,
        ),
        (
            ToolbarAction::Redo,
            "Redo",
            ButtonHighlight::Off,
            true,
            true,
        ),
        (
            ToolbarAction::ZoomOut,
            "-",
            ButtonHighlight::Off,
            false,
            true,
        ),
        (
            ToolbarAction::ZoomIn,
            "+",
            ButtonHighlight::Off,
            false,
            true,
        ),
    ];

    for &(action, label, hl, end_group, collapsible) in items {
        if narrow && collapsible {
            continue;
        }
        if x + 36 > max_left_x {
            break;
        }
        let (rect, next_x) = draw_toolbar_button(
            fb,
            &fonts,
            x,
            y,
            label,
            hl,
            read_only_history && !matches!(action, ToolbarAction::ZoomIn | ToolbarAction::ZoomOut),
            bar,
        );
        hits.push(ToolbarHit { rect, action });
        x = next_x + if end_group { gap } else { 0 };
    }
    if narrow && x + 28 < max_left_x {
        draw_ui_text(
            fb,
            &fonts,
            x + 4,
            y + 17,
            "\u{22EF}",
            Style::at(880),
            QUIET_MD_THEME.text_caption,
            bar,
        );
    }

    // Zoom percentage readout beside right controls if room allows.
    let zoom_text = format!("{zoom_pct}%");
    let right_hist_x = i32::from(win_w) - dot_w - hist_w - 16;
    if right_hist_x - x > 48 {
        draw_ui_text(
            fb,
            &fonts,
            right_hist_x - 42,
            y + 17,
            &zoom_text,
            Style::at(820),
            QUIET_MD_THEME.text_caption,
            bar,
        );
    }

    let (hist_rect, _) = draw_toolbar_button(
        fb,
        &fonts,
        right_hist_x,
        y,
        "Hist",
        if history_open {
            ButtonHighlight::Active
        } else {
            ButtonHighlight::Off
        },
        false,
        bar,
    );
    hits.push(ToolbarHit {
        rect: hist_rect,
        action: ToolbarAction::ToggleHistory,
    });

    // Security indicator dot (right edge).
    let dot_rect = Rect::new(i32::from(win_w) - 28, 12, 16, 16);
    fb.fill_rect_clipped(
        Rect::new(dot_rect.x + 3, dot_rect.y + 3, 10, 10),
        dot_state.color(),
        bar,
    );
    fb.stroke_rect(
        Rect::new(dot_rect.x + 2, dot_rect.y + 2, 12, 12),
        QUIET_MD_THEME.line_border,
        bar,
    );
    hits.push(ToolbarHit {
        rect: dot_rect,
        action: ToolbarAction::ToggleSecurity,
    });

    hits
}

fn render_status_strip(surface: &mut PixelSurface, status_left: &str, status_right: &str) {
    let (w, h) = surface.window_size();
    let fonts = surface.fonts().clone();
    let fb = surface.framebuffer_mut();
    let y0 = i32::from(h) - STATUS_BAR_HEIGHT_PX;
    let strip = Rect::new(0, y0, i32::from(w), STATUS_BAR_HEIGHT_PX);
    fb.fill_rect_clipped(strip, QUIET_MD_THEME.code_gutter, strip);
    fb.fill_rect_clipped(
        Rect::new(0, y0, i32::from(w), 1),
        QUIET_MD_THEME.line_divider,
        strip,
    );
    let st = Style::at(800);
    draw_ui_text(
        fb,
        &fonts,
        12,
        y0 + 16,
        status_left,
        st,
        QUIET_MD_THEME.text_caption,
        strip,
    );
    let rw = (fonts.advance(status_right, st).sixtyfourths() >> 6) as i32;
    let rx = (i32::from(w) - rw - 12).max(16);
    if rx > i32::from(w) / 2 {
        draw_ui_text(
            fb,
            &fonts,
            rx,
            y0 + 16,
            status_right,
            st,
            QUIET_MD_THEME.text_caption,
            strip,
        );
    }
}

fn render_security_overlay(surface: &mut PixelSurface, facts: &Facts) {
    let (w, _) = surface.window_size();
    let fonts = surface.fonts().clone();
    let fb = surface.framebuffer_mut();
    let lines = facts.lines();
    let card_w = i32::from(w).min(540) - 24;
    let card_h = 40 + (lines.len() as i32) * 20;
    let card_x = (i32::from(w) - card_w - 14).max(12);
    let card_y = i32::from(QUIET_MD_THEME.toolbar_height_px) + 8;
    let card = Rect::new(card_x, card_y, card_w, card_h);

    fb.fill_rect_clipped(card, QUIET_MD_THEME.bg_body, card);
    fb.stroke_rect(card, QUIET_MD_THEME.line_border, card);

    draw_ui_text(
        fb,
        &fonts,
        card_x + 14,
        card_y + 22,
        "Security & Residency (Esc to close)",
        Style::at(920).bold(),
        QUIET_MD_THEME.text,
        card,
    );
    for (idx, line) in lines.iter().enumerate() {
        draw_ui_text(
            fb,
            &fonts,
            card_x + 14,
            card_y + 44 + (idx as i32) * 20,
            line,
            Style::at(840),
            QUIET_MD_THEME.text_caption,
            card,
        );
    }
}

/// Read-only segment sink used by historical timeline viewports (`Block 8`) so historical
/// `Editor` instances never allocate or append segments.
#[derive(Debug, Default, Clone, Copy)]
struct ReadOnlySink;

impl SegmentSink for ReadOnlySink {
    fn append_segment(&mut self, _plaintext: &[u8]) -> Result<holonomy_text::Appended, String> {
        Err("history view is read-only".to_owned())
    }
}

/// Interactive state for the Block 8 read-only history timeline scrubber & diff viewer.
struct HistoryDrawerState {
    witnesses: Vec<WitnessSummary>,
    selected_idx: usize,
    diff_base_idx: usize,
    diff_hunks: Vec<SpineDiffHunk>,
    diff_summary: String,
    preview_text: String,
    preview_len: u64,
    bookmark_msg: String,
    bookmark_input: Option<String>,
    search_input: Option<String>,
    search_hits: Vec<HistorySearchHit>,
    search_cursor: usize,
    history_editor: Option<Editor<ReadOnlySink, store::DocumentSource, FontSet>>,
    scrubbing: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryButton {
    ScrubTrack(Rect),
    SelectWitness(usize),
    SelectBookmark(u64),
    SelectSearchHit(usize),
    StartBookmark,
    StartSearch,
    NextSearchHit,
    SetDiffBase,
    DiffPrev,
    RestoreSelected,
    CloseLive,
}

fn scrub_index_for_x(track: Rect, x: i32, count: usize) -> usize {
    if count <= 1 {
        return 0;
    }
    let span = (track.w - 1).max(1);
    let rel = (x - track.x).clamp(0, span);
    ((rel as i64 * (count as i64 - 1) + i64::from(span) / 2) / i64::from(span)) as usize
}

fn render_history_drawer(
    surface: &mut PixelSurface,
    state: &HistoryDrawerState,
    bookmarks: &[holonomy_line::Bookmark],
) -> Vec<(Rect, HistoryButton)> {
    let (w, h) = surface.window_size();
    let fonts = surface.fonts().clone();
    let fb = surface.framebuffer_mut();
    let top = i32::from(QUIET_MD_THEME.toolbar_height_px);
    let drawer_w = (i32::from(w) / 3).clamp(300, 410);
    let drawer_h = (i32::from(h) - top - STATUS_BAR_HEIGHT_PX).max(160);
    let drawer = Rect::new(i32::from(w) - drawer_w, top, drawer_w, drawer_h);

    fb.fill_rect_clipped(drawer, QUIET_MD_THEME.bg_body, drawer);
    fb.stroke_rect(drawer, QUIET_MD_THEME.line_border, drawer);

    let mut hits = Vec::new();
    let pad_x = drawer.x + 14;
    let mut y = drawer.y + 24;
    let total = state.witnesses.len();
    let cur_seq = state
        .witnesses
        .get(state.selected_idx)
        .map_or(0, |w| w.sequence);
    let title = format!("Timeline Scrubber (#{cur_seq} / {total})");
    draw_ui_text(
        fb,
        &fonts,
        pad_x,
        y,
        &title,
        Style::at(940).bold(),
        QUIET_MD_THEME.text,
        drawer,
    );
    y += 10;

    // Interactive scrubber track across all witnesses with bookmark + search-hit markers.
    let track = Rect::new(pad_x, y, drawer_w - 28, 22);
    let rail = Rect::new(track.x, track.y + 9, track.w, 4);
    fb.fill_rect_clipped(rail, QUIET_MD_THEME.code_gutter, drawer);
    fb.stroke_rect(rail, QUIET_MD_THEME.line_border, drawer);

    let seq_to_x = |seq: u64| -> Option<i32> {
        if total <= 1 {
            return Some(track.x + track.w / 2);
        }
        let idx = state
            .witnesses
            .binary_search_by_key(&seq, |w| w.sequence)
            .ok()?;
        let rel = (idx as i64 * i64::from((track.w - 4).max(1))) / (total as i64 - 1);
        Some(track.x + rel as i32)
    };
    for hit in &state.search_hits {
        if let Some(hx) = seq_to_x(hit.witness_sequence) {
            fb.fill_rect_clipped(
                Rect::new(hx, track.y + 3, 2, 16),
                QUIET_MD_THEME.link,
                drawer,
            );
        }
    }
    for bm in bookmarks {
        if let Some(bx) = seq_to_x(bm.witness_sequence) {
            fb.fill_rect_clipped(
                Rect::new(bx, track.y + 2, 3, 18),
                Rgba8::rgb(0xb4, 0x53, 0x09),
                drawer,
            );
        }
    }
    let knob_x = if total <= 1 {
        track.x
    } else {
        track.x
            + ((state.selected_idx as i64 * i64::from((track.w - 10).max(1))) / (total as i64 - 1))
                as i32
    };
    let knob = Rect::new(knob_x, track.y + 3, 10, 16);
    fb.fill_rect_clipped(knob, QUIET_MD_THEME.link, drawer);
    hits.push((track, HistoryButton::ScrubTrack(track)));
    y += 28;

    // Action row 1: +Bookmark, Search, Restore, Live
    let (bm_rect, nx) = draw_toolbar_button(
        fb,
        &fonts,
        pad_x,
        y,
        "+Mark",
        if state.bookmark_input.is_some() {
            ButtonHighlight::Active
        } else {
            ButtonHighlight::Off
        },
        false,
        drawer,
    );
    hits.push((bm_rect, HistoryButton::StartBookmark));
    let (sr_rect, nx2) = draw_toolbar_button(
        fb,
        &fonts,
        nx + 4,
        y,
        "Search",
        if state.search_input.is_some() {
            ButtonHighlight::Active
        } else {
            ButtonHighlight::Off
        },
        false,
        drawer,
    );
    hits.push((sr_rect, HistoryButton::StartSearch));
    let (rs_rect, nx3) = draw_toolbar_button(
        fb,
        &fonts,
        nx2 + 4,
        y,
        "Restore",
        ButtonHighlight::Active,
        false,
        drawer,
    );
    hits.push((rs_rect, HistoryButton::RestoreSelected));
    let (lv_rect, _) = draw_toolbar_button(
        fb,
        &fonts,
        nx3 + 4,
        y,
        "Live",
        ButtonHighlight::Off,
        false,
        drawer,
    );
    hits.push((lv_rect, HistoryButton::CloseLive));
    y += 28;

    // Action row 2: Diff base selectors
    let (sb_rect, dx1) = draw_toolbar_button(
        fb,
        &fonts,
        pad_x,
        y,
        "PinBase",
        ButtonHighlight::Off,
        false,
        drawer,
    );
    hits.push((sb_rect, HistoryButton::SetDiffBase));
    let (dp_rect, dx2) = draw_toolbar_button(
        fb,
        &fonts,
        dx1 + 4,
        y,
        "DiffPrev",
        ButtonHighlight::Off,
        false,
        drawer,
    );
    hits.push((dp_rect, HistoryButton::DiffPrev));
    if !state.search_hits.is_empty() {
        let (nh_rect, _) = draw_toolbar_button(
            fb,
            &fonts,
            dx2 + 4,
            y,
            "NextHit",
            ButtonHighlight::Active,
            false,
            drawer,
        );
        hits.push((nh_rect, HistoryButton::NextSearchHit));
    }
    y += 28;

    // Inline bookmark or history search input box when active.
    if let Some(ref draft_bm) = state.bookmark_input {
        let box_rect = Rect::new(pad_x, y, drawer_w - 28, 24);
        fb.fill_rect_clipped(box_rect, QUIET_MD_THEME.code_bg, drawer);
        fb.stroke_rect(box_rect, QUIET_MD_THEME.link, drawer);
        draw_ui_text(
            fb,
            &fonts,
            box_rect.x + 6,
            box_rect.y + 16,
            &format!("Bookmark name: {draft_bm}| (Enter save)"),
            Style::at(800),
            QUIET_MD_THEME.text,
            box_rect,
        );
        y += 28;
    } else if let Some(ref query) = state.search_input {
        let box_rect = Rect::new(pad_x, y, drawer_w - 28, 24);
        fb.fill_rect_clipped(box_rect, QUIET_MD_THEME.code_bg, drawer);
        fb.stroke_rect(box_rect, QUIET_MD_THEME.link, drawer);
        draw_ui_text(
            fb,
            &fonts,
            box_rect.x + 6,
            box_rect.y + 16,
            &format!("Find across history: {query}| (Enter)"),
            Style::at(800),
            QUIET_MD_THEME.text,
            box_rect,
        );
        y += 28;
    }

    if !state.diff_summary.is_empty() {
        draw_ui_text(
            fb,
            &fonts,
            pad_x,
            y + 12,
            &state.diff_summary,
            Style::at(780),
            QUIET_MD_THEME.text_caption,
            drawer,
        );
        y += 18;
    }

    if !state.bookmark_msg.is_empty() {
        draw_ui_text(
            fb,
            &fonts,
            pad_x,
            y + 12,
            &state.bookmark_msg,
            Style::at(780),
            QUIET_MD_THEME.link,
            drawer,
        );
        y += 18;
    }

    // Named bookmark quick-jump strip (up to 3 recent bookmarks).
    if !bookmarks.is_empty() {
        let mut bx = pad_x;
        for bm in bookmarks.iter().rev().take(3) {
            let tag = format!("@{}:#{}", bm.name, bm.witness_sequence);
            let (bm_hit, nx_bm) = draw_toolbar_button(
                fb,
                &fonts,
                bx,
                y,
                &tag,
                if bm.witness_sequence == cur_seq {
                    ButtonHighlight::Active
                } else {
                    ButtonHighlight::Off
                },
                false,
                drawer,
            );
            hits.push((bm_hit, HistoryButton::SelectBookmark(bm.witness_sequence)));
            bx = nx_bm + 4;
            if bx + 70 >= drawer.x + drawer_w {
                break;
            }
        }
        y += 26;
    }

    // Matching history search hits when present (up to 4 compact rows).
    if !state.search_hits.is_empty() {
        for (h_idx, hit) in state.search_hits.iter().enumerate().take(4) {
            let row_rect = Rect::new(drawer.x + 8, y, drawer_w - 16, 18);
            if h_idx == state.search_cursor {
                fb.fill_rect_clipped(row_rect, Rgba8::rgba_pct(0x3a, 0x85, 0xf7, 16), drawer);
            }
            let txt = format!(
                "Hit #{} · {}x @ byte {}",
                hit.witness_sequence,
                hit.count,
                hit.first_offset.unwrap_or(0)
            );
            draw_ui_text(
                fb,
                &fonts,
                pad_x,
                y + 13,
                &txt,
                Style::at(780),
                QUIET_MD_THEME.link,
                drawer,
            );
            hits.push((row_rect, HistoryButton::SelectSearchHit(h_idx)));
            y += 19;
        }
    }

    let max_rows = ((drawer_h - (y - drawer.y) - 108) / 21).max(2) as usize;
    let start = state.selected_idx.saturating_sub(max_rows / 2);
    let end = (start + max_rows).min(total);
    for (idx, w_sum) in state.witnesses.iter().enumerate().take(end).skip(start) {
        let row_rect = Rect::new(drawer.x + 8, y, drawer_w - 16, 20);
        if idx == state.selected_idx {
            fb.fill_rect_clipped(row_rect, Rgba8::rgba_pct(0x3a, 0x85, 0xf7, 16), drawer);
        } else if idx == state.diff_base_idx {
            fb.fill_rect_clipped(row_rect, Rgba8::rgba_pct(0x16, 0xa3, 0x4a, 12), drawer);
        }
        let bm_tag = bookmarks
            .iter()
            .rev()
            .find(|b| b.witness_sequence == w_sum.sequence)
            .map(|b| format!(" [{}]", b.name))
            .unwrap_or_default();
        let base_tag = if idx == state.diff_base_idx {
            " (base)"
        } else {
            ""
        };
        let label = format!(
            "#{} · {} segs{}{}",
            w_sum.sequence, w_sum.segment_count, base_tag, bm_tag
        );
        draw_ui_text(
            fb,
            &fonts,
            pad_x,
            y + 15,
            &label,
            Style::at(820),
            if idx == state.selected_idx {
                QUIET_MD_THEME.link
            } else {
                QUIET_MD_THEME.text
            },
            drawer,
        );
        hits.push((row_rect, HistoryButton::SelectWitness(idx)));
        y += 21;
    }

    // Excerpt preview box
    let prev_rect = Rect::new(drawer.x + 10, drawer.y + drawer_h - 98, drawer_w - 20, 88);
    fb.fill_rect_clipped(prev_rect, QUIET_MD_THEME.code_bg, drawer);
    fb.stroke_rect(prev_rect, QUIET_MD_THEME.line_border, drawer);
    let hdr = format!("Snapshot ({} bytes):", state.preview_len);
    draw_ui_text(
        fb,
        &fonts,
        prev_rect.x + 8,
        prev_rect.y + 15,
        &hdr,
        Style::at(780).bold(),
        QUIET_MD_THEME.text_caption,
        prev_rect,
    );
    for (line_no, line) in state.preview_text.lines().take(3).enumerate() {
        draw_ui_text(
            fb,
            &fonts,
            prev_rect.x + 8,
            prev_rect.y + 34 + (line_no as i32) * 17,
            line,
            Style::at(800),
            QUIET_MD_THEME.text,
            prev_rect,
        );
    }

    hits
}

/// Applies `action` to `app` and updates view zoom when requested (`UI-SPEC.md` §3).
pub fn apply_toolbar_action<S, R>(
    app: &mut App<S, R, FontSet, PixelSurface>,
    action: ToolbarAction,
) -> Result<(), crate::GuiError>
where
    S: holonomy_text::SegmentSink,
    R: holonomy_text::SegmentSource,
{
    match action {
        ToolbarAction::CycleFamily => {
            let next = match app.editor().toolbar_state().family {
                TriState::Uniform(FontFamily::Sans) => FontFamily::Serif,
                TriState::Uniform(FontFamily::Serif) => FontFamily::Mono,
                _ => FontFamily::Sans,
            };
            let _ = app.editor().apply(Intent::Format(StylePatch {
                family: Some(next),
                ..StylePatch::default()
            }))?;
            app.mark_dirty();
        }
        ToolbarAction::ToggleBold => {
            let _ = app.toggle_bold()?;
        }
        ToolbarAction::ToggleItalic => {
            let _ = app.toggle_italic()?;
        }
        ToolbarAction::ToggleUnderline => {
            let _ = app.toggle_underline()?;
        }
        ToolbarAction::ToggleStrike => {
            let next = !matches!(
                app.editor().toolbar_state().strikethrough,
                TriState::Uniform(true)
            );
            let _ = app.editor().apply(Intent::Format(StylePatch {
                strikethrough: Some(next),
                ..StylePatch::default()
            }))?;
            app.mark_dirty();
        }
        ToolbarAction::ToggleCode => {
            let _ = app.toggle_code_span()?;
        }
        ToolbarAction::Heading1 => {
            let _ = app.toggle_heading(Heading::H1)?;
        }
        ToolbarAction::Heading2 => {
            let _ = app.toggle_heading(Heading::H2)?;
        }
        ToolbarAction::Heading3 => {
            let _ = app.toggle_heading(Heading::H3)?;
        }
        ToolbarAction::CycleAlign => {
            let next = match app.editor().toolbar_state().alignment {
                TriState::Uniform(Alignment::Start) => Alignment::Center,
                TriState::Uniform(Alignment::Center) => Alignment::End,
                TriState::Uniform(Alignment::End) => Alignment::Justify,
                _ => Alignment::Start,
            };
            let _ = app.editor().apply(Intent::Format(StylePatch {
                alignment: Some(next),
                ..StylePatch::default()
            }))?;
            app.mark_dirty();
        }
        ToolbarAction::ToggleBullet => {
            let next = if matches!(
                app.editor().toolbar_state().list,
                TriState::Uniform(ListKind::Bullet { .. })
            ) {
                ListKind::None
            } else {
                ListKind::Bullet { depth: 1 }
            };
            let _ = app.editor().apply(Intent::Format(StylePatch {
                list: Some(next),
                ..StylePatch::default()
            }))?;
            app.mark_dirty();
        }
        ToolbarAction::ToggleNumbered => {
            let next = if matches!(
                app.editor().toolbar_state().list,
                TriState::Uniform(ListKind::Ordered { .. })
            ) {
                ListKind::None
            } else {
                ListKind::Ordered { depth: 1, start: 1 }
            };
            let _ = app.editor().apply(Intent::Format(StylePatch {
                list: Some(next),
                ..StylePatch::default()
            }))?;
            app.mark_dirty();
        }
        ToolbarAction::CycleQuote => {
            let next = match app.editor().toolbar_state().blockquote {
                TriState::Uniform(d) if d > 0 => 0,
                _ => 1,
            };
            let _ = app.editor().apply(Intent::Format(StylePatch {
                blockquote: Some(next),
                ..StylePatch::default()
            }))?;
            app.mark_dirty();
        }
        ToolbarAction::ToggleHr => {
            let next = !matches!(
                app.editor().toolbar_state().horizontal_rule,
                TriState::Uniform(true)
            );
            let _ = app.editor().apply(Intent::Format(StylePatch {
                horizontal_rule: Some(next),
                ..StylePatch::default()
            }))?;
            app.mark_dirty();
        }
        ToolbarAction::Undo => {
            let _ = app.editor().apply(Intent::Undo)?;
            app.mark_dirty();
        }
        ToolbarAction::Redo => {
            let _ = app.editor().apply(Intent::Redo)?;
            app.mark_dirty();
        }
        ToolbarAction::ZoomOut | ToolbarAction::ZoomIn => {
            let cur = app.editor().view();
            let delta = if matches!(action, ToolbarAction::ZoomOut) {
                -100
            } else {
                100
            };
            let next_zoom = (cur.zoom_milli() + delta).clamp(500, 2_500);
            if let Some(next_view) = View::new(next_zoom, cur.dpi()) {
                app.editor().set_view(next_view);
                let (w, h) = app.surface().window_size();
                app.surface_mut().set_viewport(next_view, w, h);
                let (lw, lh) = app.surface().logical_editor_extent();
                let _ = app.resize(lw, lh);
                app.mark_dirty();
            }
        }
        ToolbarAction::ToggleHistory | ToolbarAction::ToggleSecurity => {}
    }
    Ok(())
}

/// Interactive graphical passphrase / file-path prompt rendered right inside the window when
/// `holonomy-gui` is launched without a pre-supplied stdin passphrase (`UI-SPEC.md` §5.1–§5.2).
fn prompt_gui_document(
    x11: &mut dyn DesktopWindow,
    fonts: &FontSet,
    initial_path: Option<&Path>,
) -> Result<Option<(PathBuf, Document)>, Box<dyn std::error::Error>> {
    let mut path_buf = initial_path
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "notes.wavefunction".to_owned());
    let mut editing_path = initial_path.is_none();
    let mut pass_buf = Zeroizing::new(Vec::<u8>::with_capacity(MAX_PASSPHRASE_INPUT_BYTES));
    let mut error_msg = String::new();
    let mut dirty = true;

    loop {
        if dirty {
            let (w, h) = x11.size();
            let mut fb = FrameBuffer::new(w, h, Rgba8::rgb(0xf6, 0xf7, 0xf9));
            let clip = Rect::new(0, 0, i32::from(w), i32::from(h));
            let card_w = i32::from(w).min(480) - 32;
            let card_h = 260;
            let cx = (i32::from(w) - card_w) / 2;
            let cy = (i32::from(h) - card_h) / 2;
            let card = Rect::new(cx, cy, card_w, card_h);
            fb.fill_rect_clipped(card, QUIET_MD_THEME.bg_body, clip);
            fb.stroke_rect(card, QUIET_MD_THEME.line_border, clip);

            let path_obj = Path::new(&path_buf);
            let exists = !path_buf.trim().is_empty() && path_obj.exists();
            let heading = if editing_path {
                "Choose document file"
            } else if exists {
                "Unlock encrypted document"
            } else {
                "Create new encrypted document"
            };
            draw_ui_text(
                &mut fb,
                fonts,
                cx + 24,
                cy + 38,
                heading,
                Style::at(1_250).bold(),
                QUIET_MD_THEME.text,
                card,
            );
            draw_ui_text(
                &mut fb,
                fonts,
                cx + 24,
                cy + 64,
                "Path:",
                Style::at(850),
                QUIET_MD_THEME.text_caption,
                card,
            );
            let path_box = Rect::new(cx + 24, cy + 72, card_w - 48, 30);
            fb.fill_rect_clipped(
                path_box,
                if editing_path {
                    QUIET_MD_THEME.bg_body
                } else {
                    QUIET_MD_THEME.code_bg
                },
                card,
            );
            fb.stroke_rect(
                path_box,
                if editing_path {
                    QUIET_MD_THEME.caret
                } else {
                    QUIET_MD_THEME.line_border
                },
                card,
            );
            draw_ui_text(
                &mut fb,
                fonts,
                path_box.x + 8,
                path_box.y + 20,
                &path_buf,
                Style::at(920),
                QUIET_MD_THEME.text,
                path_box,
            );

            draw_ui_text(
                &mut fb,
                fonts,
                cx + 24,
                cy + 128,
                "Passphrase (memory-scrubbed; Esc cancels):",
                Style::at(850),
                QUIET_MD_THEME.text_caption,
                card,
            );
            let pass_box = Rect::new(cx + 24, cy + 136, card_w - 48, 32);
            fb.fill_rect_clipped(pass_box, QUIET_MD_THEME.bg_body, card);
            fb.stroke_rect(
                pass_box,
                if !editing_path {
                    QUIET_MD_THEME.caret
                } else {
                    QUIET_MD_THEME.line_border
                },
                card,
            );
            let char_count = std::str::from_utf8(&pass_buf).map_or(0, |s| s.chars().count());
            let dots = "\u{2022}".repeat(char_count.min(48));
            draw_ui_text(
                &mut fb,
                fonts,
                pass_box.x + 10,
                pass_box.y + 21,
                &dots,
                Style::at(1_000),
                QUIET_MD_THEME.text,
                pass_box,
            );

            let footer = if error_msg.is_empty() {
                if editing_path {
                    "Enter path and press Enter \u{00B7} Tab switches field".to_owned()
                } else if exists {
                    "Enter to open \u{00B7} Argon2id + ML-KEM-1024 + X25519".to_owned()
                } else {
                    "Enter to calibrate Argon2id and seal new .wavefunction".to_owned()
                }
            } else {
                error_msg.clone()
            };
            draw_ui_text(
                &mut fb,
                fonts,
                cx + 24,
                cy + 196,
                &footer,
                Style::at(820),
                if error_msg.is_empty() {
                    QUIET_MD_THEME.text_caption
                } else {
                    SecurityDotState::Tripped.color()
                },
                card,
            );

            x11.present_argb(&fb.pixels, w, h)?;
            dirty = false;
        }

        for ev in x11.poll_events(100)? {
            match ev {
                DesktopEvent::CloseRequested => return Ok(None),
                DesktopEvent::Expose | DesktopEvent::Resize { .. } => dirty = true,
                DesktopEvent::Key { key, modifiers } => {
                    dirty = true;
                    match key {
                        Key::Escape => return Ok(None),
                        Key::Tab => editing_path = !editing_path,
                        Key::Backspace => {
                            if editing_path {
                                path_buf.pop();
                            } else {
                                while let Some(b) = pass_buf.pop() {
                                    if (b & 0b1100_0000) != 0b1000_0000 {
                                        break;
                                    }
                                }
                            }
                        }
                        Key::Enter => {
                            if editing_path {
                                if !path_buf.trim().is_empty() {
                                    editing_path = false;
                                    error_msg.clear();
                                }
                            } else {
                                let text = std::str::from_utf8(&pass_buf).unwrap_or("");
                                match PassphraseInput::new(text) {
                                    Err(err) => {
                                        error_msg = format!("passphrase refused: {err}");
                                        pass_buf.zeroize();
                                        pass_buf.clear();
                                    }
                                    Ok(input) => {
                                        let target = PathBuf::from(path_buf.trim());
                                        let open_res = if target.exists() {
                                            Document::open_input(&target, &input)
                                        } else {
                                            Document::create_input(&target, &input)
                                        };
                                        drop(input);
                                        pass_buf.zeroize();
                                        pass_buf.clear();
                                        match open_res {
                                            Ok(doc) => return Ok(Some((target, doc))),
                                            Err(err) => {
                                                error_msg = err.to_string();
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Key::Character(ch) if !modifiers.control && !modifiers.alt => {
                            error_msg.clear();
                            if editing_path {
                                path_buf.push(ch);
                            } else {
                                let mut enc = [0u8; 4];
                                let s = ch.encode_utf8(&mut enc);
                                if pass_buf.len() + s.len() <= MAX_PASSPHRASE_INPUT_BYTES {
                                    pass_buf.extend_from_slice(s.as_bytes());
                                }
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

#[derive(Debug, Clone)]
enum FindState {
    Find { needle: String, next_idx: u64 },
    Replace { needle: String, replacement: String },
}

/// Verified emergency-wipe receipt captured after cooperative reaper quiescence (`L-7`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GuiWipeReceipt {
    /// Registered secret bytes volatile-zeroed by the reaper.
    pub bytes_wiped: usize,
    /// Number of registered arenas scrubbed.
    pub arenas_wiped: usize,
    /// Number of canary buffers checked after scrub (`0` ghost bytes survived).
    pub canaries: usize,
    /// Microseconds from tripwire fire to zero-canary verification.
    pub trip_to_verified_us: u64,
    /// Whether the reaper thread ran under real-time scheduling.
    pub real_time: bool,
    /// Plaintext bytes scrubbed in-place inside editor/residency buffers.
    pub in_place_bytes: usize,
}

/// Reason a triggered emergency wipe could not be cryptographically/canary confirmed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiWipeUnconfirmed {
    /// Reaper did not yield an outcome after join.
    NoOutcome,
    /// No secret arenas were armed at trip time.
    NothingArmed,
    /// At least one canary still held non-zero bytes after scrub.
    Ghosts {
        /// Canary allocations that still held non-zero bytes.
        ghost_canaries: usize,
        /// Total non-zero ghost bytes remaining.
        ghost_bytes: usize,
    },
}

/// Final outcome of a graphical desktop editing session.
#[derive(Debug)]
pub enum DesktopEnd {
    /// Window closed normally with all pending edits saved.
    Saved,
    /// Document had no unsaved changes on close.
    NothingToWrite,
    /// User cancelled unlock/open dialog cleanly.
    Cancelled,
    /// Final save refused on exit.
    SaveFailed,
    /// Emergency reaper wiped plaintext on deadman / `Ctrl+L`.
    Wiped(GuiWipeReceipt),
    /// Emergency reaper tripped, but wipe could not be verified (`reason`, in-place bytes scrubbed).
    WipeUnconfirmed(GuiWipeUnconfirmed, usize),
}

/// Runs the native desktop window for Holonomy (`X11` on Linux, `Cocoa`/`X11` on macOS,
/// `Win32` on Windows): `quiet-md` layout, live `ToolbarState`, embedded `Inter`/`JetBrains Mono`
/// A8 rasterization, clipboard, security dot/panel, Block 8 timeline scrubber, autosave, and
/// emergency wipe reaper.
///
/// # Errors
///
/// Returns an error if the platform display subsystem cannot be opened or communicates an error.
pub fn run_desktop(
    initial_path: Option<&Path>,
    deadman: Option<Duration>,
) -> Result<DesktopEnd, Box<dyn std::error::Error>> {
    let title = match initial_path {
        Some(p) => format!("Holonomy \u{2014} {}", p.display()),
        None => "Holonomy".to_owned(),
    };
    let win = open_desktop_window(&title, 1_120, 780)?;
    run_desktop_with_window(win, initial_path, deadman)
}

/// Runs the pure-`libc` X11 desktop window for Holonomy (`quiet-md` layout, live `ToolbarState`,
/// embedded `Inter`/`JetBrains Mono` A8 rasterization, clipboard, security dot/panel, Block 8
/// timeline scrubber, autosave, and emergency wipe reaper).
///
/// # Errors
///
/// Returns an error if the X11 display cannot be opened or communicates an error.
#[cfg(unix)]
pub fn run_x11_desktop(
    initial_path: Option<&Path>,
    deadman: Option<Duration>,
) -> Result<DesktopEnd, Box<dyn std::error::Error>> {
    let title = match initial_path {
        Some(p) => format!("Holonomy \u{2014} {}", p.display()),
        None => "Holonomy".to_owned(),
    };
    let x11 = crate::x11::X11Window::open(None, &title, 1_120, 780)?;
    run_desktop_with_window(Box::new(x11), initial_path, deadman)
}

fn run_desktop_with_window(
    mut x11: Box<dyn DesktopWindow>,
    initial_path: Option<&Path>,
    deadman: Option<Duration>,
) -> Result<DesktopEnd, Box<dyn std::error::Error>> {
    let fonts = FontSet::embedded(QUIET_MD_THEME.body_size_px)?;

    let Some((doc_path, document)) = prompt_gui_document(x11.as_mut(), &fonts, initial_path)?
    else {
        return Ok(DesktopEnd::Cancelled);
    };

    let arenas = SecretArenas::new();
    let mut canary_set = CanarySet::new(8, 256);
    let _ = canary_set.arm_all(&arenas);
    let canaries = Arc::new(canary_set);
    let tripwire = Tripwire::new(deadman);
    tripwire.beat();
    let reaper = ReaperGuard::spawn_cooperative(
        Arc::clone(&arenas),
        Arc::clone(&canaries),
        Arc::clone(&tripwire),
        Duration::from_millis(50),
    );

    let spine = document.spine()?;
    let residency = ResidentSet::new(document.source(), store::BUDGET, store::SLACK)
        .map_err(|error| std::io::Error::other(error.to_string()))?;

    let (win_w, win_h) = x11.size();
    let initial_view = View::new(1_000, x11.dpi()).unwrap_or(View::IDENTITY);
    let surface = PixelSurface::new(fonts.clone(), initial_view, win_w, win_h);
    let (lw, lh) = surface.logical_editor_extent();

    let spine_len = spine.len();
    let mut editor = Editor::new(
        Typing::resume(document.sink(), spine, store::CAPACITY),
        residency,
        fonts,
        lw,
        lh,
    );
    editor.set_view(initial_view);
    if spine_len <= 2 * 1024 * 1024 {
        let _ = editor.reflow();
    }

    let keymap = Keymap::new(lh);
    let mut app = App::new(editor, keymap, surface);
    let _ = app.editor().frame();

    let lock_cap = probe_capability();
    let mut verified_segments: Option<u64> = None;
    let mut show_security = false;
    let mut history_drawer: Option<HistoryDrawerState> = None;
    let mut history_hits: Vec<(Rect, HistoryButton)> = Vec::new();
    let mut find_bar: Option<FindState> = None;
    let mut toolbar_hits: Vec<ToolbarHit> = Vec::new();
    let mut status_msg = if document.trailing_bytes() != 0 {
        format!(
            "recovered: discarded {} trailing uncommitted bytes",
            document.trailing_bytes()
        )
    } else {
        String::new()
    };

    let mut last_activity = Instant::now();
    let mut pending_paint = true;
    let mut quit = false;

    loop {
        if reaper.has_tripped() || tripwire.evaluate().is_some() {
            return Ok(finish_gui_wipe(&mut app, &document, &arenas, reaper));
        }

        if pending_paint || app.dirty() {
            if let Some(ref mut hist) = history_drawer {
                render_history_viewport(app.surface_mut(), hist);
            } else {
                app.mark_dirty();
                let _ = app.draw();

                if app.document_len() == 0 {
                    app.surface_mut().draw_empty_placeholder();
                }
            }

            let dot_state = SecurityDotState::from_cap(lock_cap, reaper.has_tripped());
            let read_only = history_drawer.is_some();
            toolbar_hits = render_toolbar(&mut app, dot_state, read_only, read_only);

            let segs = document.segment_count();
            let wits = document.witness_count();
            let save_tag = if app.has_pending() {
                "unsaved"
            } else {
                "saved"
            };
            let name = doc_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("document");
            let left = if !status_msg.is_empty() {
                format!("{name} \u{00B7} {status_msg}")
            } else if let Some(ref h) = history_drawer {
                format!(
                    "{name} \u{00B7} HISTORY READ-ONLY \u{00B7} {}",
                    h.diff_summary
                )
            } else {
                format!(
                    "{name} \u{00B7} {} B \u{00B7} {segs} segs \u{00B7} {wits} witnesses \u{00B7} {save_tag}",
                    app.document_len()
                )
            };
            let right = Facts::gather_with(app.residency(), &document, lock_cap, verified_segments)
                .with_cooperative_reaper(&arenas, &canaries, reaper.has_tripped())
                .resident_line();

            if let Some(find) = &find_bar {
                let surface = app.surface_mut();
                let (w, h) = surface.window_size();
                let f = surface.fonts().clone();
                let fb = surface.framebuffer_mut();
                let rect = Rect::new(
                    0,
                    i32::from(h) - STATUS_BAR_HEIGHT_PX - 30,
                    i32::from(w),
                    30,
                );
                fb.fill_rect_clipped(rect, QUIET_MD_THEME.code_bg, rect);
                fb.stroke_rect(rect, QUIET_MD_THEME.line_border, rect);
                let prompt_line = match find {
                    FindState::Find { needle, .. } => {
                        format!("Find: {needle}   (Enter next, Tab replace, Esc close)")
                    }
                    FindState::Replace {
                        needle,
                        replacement,
                    } => {
                        format!("Replace '{needle}' -> '{replacement}'   (Enter apply, Esc close)")
                    }
                };
                draw_ui_text(
                    fb,
                    &f,
                    14,
                    rect.y + 20,
                    &prompt_line,
                    Style::at(880),
                    QUIET_MD_THEME.text,
                    rect,
                );
            }

            if show_security {
                let facts =
                    Facts::gather_with(app.residency(), &document, lock_cap, verified_segments)
                        .with_cooperative_reaper(&arenas, &canaries, reaper.has_tripped());
                render_security_overlay(app.surface_mut(), &facts);
            }

            if let Some(hist) = &history_drawer {
                let bms = document.bookmarks();
                history_hits = render_history_drawer(app.surface_mut(), hist, &bms);
            } else {
                history_hits.clear();
            }

            render_status_strip(app.surface_mut(), &left, &right);

            let (w, h) = app.surface().window_size();
            x11.present_argb(app.surface().pixels(), w, h)?;
            pending_paint = false;
        }

        let poll_ms = deadman.map_or(100, |d| {
            i32::try_from(d.as_millis() / 2)
                .unwrap_or(100)
                .clamp(10, 100)
        });
        let events = x11.poll_events(poll_ms)?;
        if reaper.has_tripped() || tripwire.evaluate().is_some() {
            return Ok(finish_gui_wipe(&mut app, &document, &arenas, reaper));
        }
        if events.is_empty() {
            // Autosave after 1.5s typing pause (`D-64`, matches TUI loop).
            if app.has_pending()
                && last_activity.elapsed() >= Duration::from_millis(PAUSE_AUTOSAVE_MS)
            {
                let _ = save_gui(&mut app);
                pending_paint = true;
            }
            continue;
        }

        last_activity = Instant::now();
        tripwire.beat();
        if reaper.has_tripped() || tripwire.evaluate().is_some() {
            return Ok(finish_gui_wipe(&mut app, &document, &arenas, reaper));
        }

        for ev in events {
            match ev {
                DesktopEvent::CloseRequested => {
                    quit = true;
                }
                DesktopEvent::FocusOut => {
                    x11.clear_clipboard();
                }
                DesktopEvent::Expose => {
                    pending_paint = true;
                }
                DesktopEvent::Resize { width, height } => {
                    let cur_view = app.editor().view();
                    app.surface_mut().set_viewport(cur_view, width, height);
                    let (lw, lh) = app.surface().logical_editor_extent();
                    let _ = app.resize(lw, lh);
                    if let Some(ref mut h) = history_drawer {
                        if let Some(ref mut hed) = h.history_editor {
                            hed.set_view(cur_view);
                            hed.resize(lw, lh);
                        }
                    }
                    pending_paint = true;
                }
                DesktopEvent::Scroll { delta_lines } => {
                    let line_px = i64::from(QUIET_MD_THEME.body_line_px) * i64::from(delta_lines);
                    if let Some(ref mut h) = history_drawer {
                        if let Some(ref mut hed) = h.history_editor {
                            hed.scroll_by(Fixed::units(line_px));
                        }
                    } else {
                        app.editor().scroll_by(Fixed::units(line_px));
                        app.mark_dirty();
                    }
                    pending_paint = true;
                }
                DesktopEvent::MouseDown { x, y, modifiers } => {
                    let px = i32::from(x);
                    let py = i32::from(y);

                    if let Some(hit) = toolbar_hits
                        .iter()
                        .find(|h| h.rect.contains(px, py))
                        .copied()
                    {
                        match hit.action {
                            ToolbarAction::ToggleSecurity => {
                                show_security = !show_security;
                                if show_security {
                                    verified_segments =
                                        document.verify().ok().map(|r| r.segments_verified);
                                }
                            }
                            ToolbarAction::ToggleHistory => {
                                history_drawer = if history_drawer.is_some() {
                                    None
                                } else {
                                    let (lw, lh) = app.surface().logical_editor_extent();
                                    let fonts = app.surface().fonts().clone();
                                    let view = app.editor().view();
                                    Some(build_history_state(&document, fonts, lw, lh, view))
                                };
                            }
                            other if history_drawer.is_none() => {
                                let _ = apply_toolbar_action(&mut app, other);
                            }
                            other @ (ToolbarAction::ZoomIn | ToolbarAction::ZoomOut) => {
                                let _ = apply_toolbar_action(&mut app, other);
                                if let Some(ref mut h) = history_drawer {
                                    let view = app.editor().view();
                                    let (lw, lh) = app.surface().logical_editor_extent();
                                    if let Some(ref mut hed) = h.history_editor {
                                        hed.set_view(view);
                                        hed.resize(lw, lh);
                                    }
                                }
                            }
                            _ => {}
                        }
                        pending_paint = true;
                        continue;
                    }

                    if let Some((_, btn)) = history_hits
                        .iter()
                        .find(|(r, _)| r.contains(px, py))
                        .copied()
                    {
                        let (lw, lh) = app.surface().logical_editor_extent();
                        let fonts = app.surface().fonts().clone();
                        let view = app.editor().view();
                        if let HistoryButton::ScrubTrack(track) = btn {
                            if let Some(ref mut h) = history_drawer {
                                h.scrubbing = true;
                                let idx = scrub_index_for_x(track, px, h.witnesses.len());
                                select_history_index(&document, h, idx, &fonts, lw, lh, view);
                            }
                        } else {
                            handle_history_button(
                                &mut app,
                                &document,
                                &mut history_drawer,
                                btn,
                                &fonts,
                                lw,
                                lh,
                                view,
                                &mut status_msg,
                            );
                        }
                        pending_paint = true;
                        continue;
                    }

                    if history_drawer.is_none() {
                        let Some(pt) = app.surface().canvas_point(x, y) else {
                            continue;
                        };
                        let intent = if modifiers.shift {
                            Intent::DragTo(pt)
                        } else {
                            Intent::Click(pt)
                        };
                        let _ = app.editor().apply(intent);
                        app.mark_dirty();
                        pending_paint = true;
                    }
                }
                DesktopEvent::MouseDrag { x, y } => {
                    if let Some(ref mut h) = history_drawer {
                        if h.scrubbing {
                            if let Some((_, HistoryButton::ScrubTrack(track))) = history_hits
                                .iter()
                                .find(|(_, b)| matches!(b, HistoryButton::ScrubTrack(_)))
                                .copied()
                            {
                                let idx = scrub_index_for_x(track, i32::from(x), h.witnesses.len());
                                let (lw, lh) = app.surface().logical_editor_extent();
                                let fonts = app.surface().fonts().clone();
                                let view = app.editor().view();
                                select_history_index(&document, h, idx, &fonts, lw, lh, view);
                                pending_paint = true;
                            }
                        }
                        let _ = y;
                    } else {
                        let Some(pt) = app.surface().canvas_point(x, y) else {
                            continue;
                        };
                        let _ = app.editor().apply(Intent::DragTo(pt));
                        app.mark_dirty();
                        pending_paint = true;
                    }
                }
                DesktopEvent::MouseUp => {
                    if let Some(ref mut h) = history_drawer {
                        h.scrubbing = false;
                    }
                }
                DesktopEvent::ClipboardPasted(bytes) => {
                    if history_drawer.is_none() {
                        let Ok(text) = std::str::from_utf8(&bytes) else {
                            continue;
                        };
                        let _ = app.press_paste_text(text);
                        pending_paint = true;
                    }
                }
                DesktopEvent::Key { key, modifiers } => {
                    if modifiers.control && !modifiers.alt {
                        match key {
                            Key::Character('q') => {
                                quit = true;
                                continue;
                            }
                            Key::Character('l') => {
                                tripwire.fire(TripReason::Manual);
                                return Ok(finish_gui_wipe(&mut app, &document, &arenas, reaper));
                            }
                            Key::Character('s') => {
                                status_msg = if save_gui(&mut app) {
                                    "saved".to_owned()
                                } else {
                                    "save refused".to_owned()
                                };
                                pending_paint = true;
                                continue;
                            }
                            Key::Character('g') => {
                                show_security = !show_security;
                                if show_security {
                                    verified_segments =
                                        document.verify().ok().map(|r| r.segments_verified);
                                }
                                pending_paint = true;
                                continue;
                            }
                            Key::Character('h') => {
                                history_drawer = if history_drawer.is_some() {
                                    None
                                } else {
                                    let (lw, lh) = app.surface().logical_editor_extent();
                                    let fonts = app.surface().fonts().clone();
                                    let view = app.editor().view();
                                    Some(build_history_state(&document, fonts, lw, lh, view))
                                };
                                pending_paint = true;
                                continue;
                            }
                            Key::Character('m') => {
                                if let Some(ref mut h) = history_drawer {
                                    h.search_input = None;
                                    h.bookmark_input = Some(String::new());
                                } else {
                                    let ts = SystemTime::now()
                                        .duration_since(UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs()
                                        as i64;
                                    let w_seq = document.witness_count().saturating_sub(1);
                                    let name = format!("bookmark-{w_seq}");
                                    let _ = document.add_bookmark(w_seq, ts, &name);
                                    status_msg = format!("bookmarked witness #{w_seq}");
                                }
                                pending_paint = true;
                                continue;
                            }
                            Key::Character('f') if history_drawer.is_none() => {
                                find_bar = Some(FindState::Find {
                                    needle: String::new(),
                                    next_idx: 0,
                                });
                                pending_paint = true;
                                continue;
                            }
                            Key::Character('f') if history_drawer.is_some() => {
                                if let Some(ref mut h) = history_drawer {
                                    h.bookmark_input = None;
                                    h.search_input = Some(String::new());
                                }
                                pending_paint = true;
                                continue;
                            }
                            Key::Character('c') => {
                                if let Ok(Some(text)) =
                                    app.editor().copy_selection(MAX_CLIPBOARD_BYTES)
                                {
                                    let z = Zeroizing::new(text);
                                    let _ = x11.set_clipboard(z.as_bytes());
                                    status_msg = format!("copied {} bytes", z.len());
                                }
                                pending_paint = true;
                                continue;
                            }
                            Key::Character('x') if history_drawer.is_none() => {
                                if let Ok(Some(text)) =
                                    app.editor().copy_selection(MAX_CLIPBOARD_BYTES)
                                {
                                    let z = Zeroizing::new(text);
                                    let _ = x11.set_clipboard(z.as_bytes());
                                    status_msg = format!("cut {} bytes", z.len());
                                    let _ = app.press(Key::Delete, Modifiers::none());
                                }
                                pending_paint = true;
                                continue;
                            }
                            Key::Character('v') if history_drawer.is_none() => {
                                let _ = x11.request_clipboard();
                                continue;
                            }
                            Key::Character('=') | Key::Character('+') => {
                                let _ = apply_toolbar_action(&mut app, ToolbarAction::ZoomIn);
                                pending_paint = true;
                                continue;
                            }
                            Key::Character('-') => {
                                let _ = apply_toolbar_action(&mut app, ToolbarAction::ZoomOut);
                                pending_paint = true;
                                continue;
                            }
                            _ => {}
                        }
                    }

                    if key == Key::Escape {
                        if let Some(ref mut h) = history_drawer {
                            if h.bookmark_input.take().is_some() || h.search_input.take().is_some()
                            {
                                pending_paint = true;
                                continue;
                            }
                        }
                        if find_bar.take().is_some()
                            || show_security
                            || history_drawer.take().is_some()
                        {
                            show_security = false;
                            pending_paint = true;
                            continue;
                        }
                    }

                    let close_find = if let Some(ref mut find) = find_bar {
                        handle_find_key(&mut app, find, key, modifiers, &mut status_msg)
                    } else {
                        false
                    };
                    if close_find {
                        find_bar = None;
                    }
                    if close_find || find_bar.is_some() {
                        pending_paint = true;
                        continue;
                    }

                    if let Some(ref mut h) = history_drawer {
                        let (lw, lh) = app.surface().logical_editor_extent();
                        let fonts = app.surface().fonts().clone();
                        let view = app.editor().view();
                        handle_history_key(&document, h, key, modifiers, &fonts, lw, lh, view);
                        pending_paint = true;
                        continue;
                    }

                    status_msg.clear();
                    if app.press(key, modifiers).is_err() {
                        status_msg = "last edit refused".to_owned();
                    }
                    pending_paint = true;
                }
            }
        }

        if quit {
            break;
        }
    }

    Ok(if !app.has_pending() {
        DesktopEnd::NothingToWrite
    } else if save_gui(&mut app) {
        DesktopEnd::Saved
    } else {
        DesktopEnd::SaveFailed
    })
}

impl<S, R, H, T> App<S, R, H, T>
where
    S: holonomy_text::SegmentSink,
    R: holonomy_text::SegmentSource,
    H: Shaper,
    T: Surface,
{
    fn press_paste_text(&mut self, text: &str) -> Result<(), crate::GuiError> {
        self.editor().mark();
        self.editor().apply(Intent::Type(text.to_owned()))?;
        let _ = self.editor().apply(Intent::RevealCaret);
        self.mark_dirty();
        Ok(())
    }
}

fn save_gui(
    app: &mut App<store::DocumentSink, store::DocumentSource, FontSet, PixelSurface>,
) -> bool {
    match app.press(Key::Character('s'), Modifiers::control()) {
        Ok(effects) => effects.iter().any(|effect| matches!(effect, Effect::Saved)),
        Err(_) => false,
    }
}

fn finish_gui_wipe(
    app: &mut App<store::DocumentSink, store::DocumentSource, FontSet, PixelSurface>,
    document: &Document,
    arenas: &Arc<SecretArenas>,
    reaper: ReaperGuard,
) -> DesktopEnd {
    // SAFETY: GUI loop stops reading/modifying document & editor allocations before joining reaper.
    let _ = unsafe { document.arm_reaper(arenas) };
    // SAFETY: GUI loop does not touch editor buffers after this until reaper joins.
    unsafe {
        app.editor()
            .set_wipe_registry(Arc::new(SealWipeBridge(Arc::clone(arenas))));
    }
    // SAFETY: single GUI thread holds the only document/editor references.
    unsafe { reaper.acknowledge_quiescence() };
    let outcome = reaper.wait_outcome();
    let in_place_bytes = app.editor().wipe_all_in_place();
    match outcome {
        Some(o) if o.canaries.ghost_canaries > 0 => DesktopEnd::WipeUnconfirmed(
            GuiWipeUnconfirmed::Ghosts {
                ghost_canaries: o.canaries.ghost_canaries,
                ghost_bytes: o.canaries.ghost_bytes,
            },
            in_place_bytes,
        ),
        Some(o) if o.wipe.arenas_wiped == 0 || o.wipe.bytes_wiped == 0 => {
            DesktopEnd::WipeUnconfirmed(GuiWipeUnconfirmed::NothingArmed, in_place_bytes)
        }
        Some(o) => DesktopEnd::Wiped(GuiWipeReceipt {
            bytes_wiped: o.wipe.bytes_wiped,
            arenas_wiped: o.wipe.arenas_wiped,
            canaries: o.canaries.canaries_sampled,
            trip_to_verified_us: o.trip_to_verified_ns / 1_000,
            real_time: o.sched_class == SchedClass::RealTime,
            in_place_bytes,
        }),
        None => DesktopEnd::WipeUnconfirmed(GuiWipeUnconfirmed::NoOutcome, in_place_bytes),
    }
}

fn build_history_state(
    document: &Document,
    fonts: FontSet,
    lw: Fixed,
    lh: Fixed,
    view: View,
) -> HistoryDrawerState {
    let witnesses = document.witness_history();
    let selected_idx = witnesses.len().saturating_sub(1);
    let diff_base_idx = selected_idx.saturating_sub(1);
    let mut state = HistoryDrawerState {
        witnesses,
        selected_idx,
        diff_base_idx,
        diff_hunks: Vec::new(),
        diff_summary: String::new(),
        preview_text: String::new(),
        preview_len: 0,
        bookmark_msg: String::new(),
        bookmark_input: None,
        search_input: None,
        search_hits: Vec::new(),
        search_cursor: 0,
        history_editor: None,
        scrubbing: false,
    };
    refresh_history_state(document, &mut state, &fonts, lw, lh, view);
    state
}

fn select_history_index(
    document: &Document,
    state: &mut HistoryDrawerState,
    idx: usize,
    fonts: &FontSet,
    lw: Fixed,
    lh: Fixed,
    view: View,
) {
    if idx >= state.witnesses.len() {
        return;
    }
    let was_prev = state.diff_base_idx + 1 == state.selected_idx;
    state.selected_idx = idx;
    if was_prev {
        state.diff_base_idx = idx.saturating_sub(1);
    }
    refresh_history_state(document, state, fonts, lw, lh, view);
}

fn refresh_history_state(
    document: &Document,
    state: &mut HistoryDrawerState,
    fonts: &FontSet,
    lw: Fixed,
    lh: Fixed,
    view: View,
) {
    let Some(w) = state.witnesses.get(state.selected_idx) else {
        return;
    };
    let w_seq = w.sequence;
    let base_seq = state
        .witnesses
        .get(state.diff_base_idx)
        .map_or(w_seq, |b| b.sequence);
    if let Ok(sp) = document.spine_at_witness(w_seq) {
        state.preview_len = sp.len();
        state.preview_text = document
            .read_witness_excerpt(w_seq, 0, sp.len().min(320))
            .unwrap_or_default();
        if let Ok(residency) = ResidentSet::new(
            document.source(),
            (store::BUDGET / 2).max(2 * 1024 * 1024),
            store::SLACK,
        ) {
            let mut hed = Editor::new(
                Typing::resume(ReadOnlySink, sp, store::CAPACITY),
                residency,
                fonts.clone(),
                lw,
                lh,
            );
            hed.set_view(view);
            state.history_editor = Some(hed);
        }
    }
    if let Ok(hunks) = document.diff_witnesses(base_seq, w_seq) {
        let mut added = 0u64;
        let mut removed = 0u64;
        for h in &hunks {
            added += h.new_range.1.saturating_sub(h.new_range.0);
            removed += h.old_range.1.saturating_sub(h.old_range.0);
        }
        state.diff_summary = if hunks.is_empty() {
            format!("no diff vs #{base_seq}")
        } else {
            format!(
                "+{added} B / -{removed} B · {} hunks (vs #{base_seq})",
                hunks.len()
            )
        };
        state.diff_hunks = hunks;
    } else {
        state.diff_hunks.clear();
        state.diff_summary = format!("witness #{w_seq}");
    }
}

fn render_history_viewport(surface: &mut PixelSurface, hist: &mut HistoryDrawerState) {
    surface.begin();
    let Some(ref mut hed) = hist.history_editor else {
        return;
    };
    let Ok(frame) = hed.frame() else {
        return;
    };
    let picture: Picture = frame.picture();
    let origin = frame.origin();
    let text = Zeroizing::new(frame.text().to_owned());

    surface.size(picture.width, picture.height);
    for (index, line) in picture.lines.iter().enumerate() {
        for cluster in picture.clusters_on(index) {
            let c_start = cluster.byte;
            let c_end = cluster.byte + cluster.len as u64;
            for hunk in &hist.diff_hunks {
                if hunk.kind == SpineDiffKind::Removed {
                    continue;
                }
                if c_end > hunk.new_range.0 && c_start < hunk.new_range.1 {
                    let dx = surface.page_origin_x + surface.to_px(cluster.x);
                    let dy = surface.page_origin_y + surface.to_px(line.top);
                    let dw = (surface.to_px(cluster.end()) - surface.to_px(cluster.x)).max(1);
                    let dh = surface.to_px(line.height).max(1);
                    let tint = match hunk.kind {
                        SpineDiffKind::Added => Rgba8::rgba_pct(0x16, 0xa3, 0x4a, 20),
                        SpineDiffKind::Replaced => Rgba8::rgba_pct(0xd9, 0x77, 0x06, 20),
                        SpineDiffKind::Removed => continue,
                    };
                    surface.fb.fill_rect_clipped(
                        Rect::new(dx, dy, dw, dh),
                        tint,
                        surface.canvas_clip,
                    );
                    break;
                }
            }
        }
        surface.line_clusters(line.top, &text, origin, picture.clusters_on(index));
    }
}

fn jump_to_search_hit(
    document: &Document,
    h: &mut HistoryDrawerState,
    cursor: usize,
    fonts: &FontSet,
    lw: Fixed,
    lh: Fixed,
    view: View,
) {
    let Some(hit) = h.search_hits.get(cursor).copied() else {
        return;
    };
    h.search_cursor = cursor;
    if let Ok(idx) = h
        .witnesses
        .binary_search_by_key(&hit.witness_sequence, |w| w.sequence)
    {
        select_history_index(document, h, idx, fonts, lw, lh, view);
        if let Some(offset) = hit.first_offset {
            if let Some(ref mut hed) = h.history_editor {
                let _ = hed.select_range(offset, offset);
                let _ = hed.apply(Intent::RevealCaret);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_history_key(
    document: &Document,
    h: &mut HistoryDrawerState,
    key: Key,
    modifiers: Modifiers,
    fonts: &FontSet,
    lw: Fixed,
    lh: Fixed,
    view: View,
) {
    if let Some(mut name) = h.bookmark_input.take() {
        match key {
            Key::Enter => {
                let final_name = if name.trim().is_empty() {
                    let w_seq = h.witnesses.get(h.selected_idx).map_or(0, |w| w.sequence);
                    format!("mark-w{w_seq}")
                } else {
                    name.trim().to_owned()
                };
                if let Some(w) = h.witnesses.get(h.selected_idx) {
                    let ts = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64;
                    match document.add_bookmark(w.sequence, ts, &final_name) {
                        Ok(_) => h.bookmark_msg = format!("saved bookmark '{final_name}'"),
                        Err(e) => h.bookmark_msg = format!("bookmark refused: {e}"),
                    }
                }
            }
            Key::Backspace => {
                name.pop();
                h.bookmark_input = Some(name);
            }
            Key::Character(ch) if !modifiers.control && !modifiers.alt && name.len() < 48 => {
                name.push(ch);
                h.bookmark_input = Some(name);
            }
            _ => {
                h.bookmark_input = Some(name);
            }
        }
        return;
    }

    if let Some(mut query) = h.search_input.take() {
        match key {
            Key::Enter => {
                if !query.is_empty() {
                    match document.search_history(&query) {
                        Ok(hits) => {
                            h.bookmark_msg = format!("{} witnesses match '{query}'", hits.len());
                            h.search_hits = hits;
                            h.search_cursor = 0;
                            if !h.search_hits.is_empty() {
                                jump_to_search_hit(document, h, 0, fonts, lw, lh, view);
                            }
                        }
                        Err(e) => h.bookmark_msg = format!("search error: {e}"),
                    }
                }
            }
            Key::Backspace => {
                query.pop();
                h.search_input = Some(query);
            }
            Key::Character(ch) if !modifiers.control && !modifiers.alt && query.len() < 96 => {
                query.push(ch);
                h.search_input = Some(query);
            }
            _ => {
                h.search_input = Some(query);
            }
        }
        return;
    }

    let step = if modifiers.shift { 10 } else { 1 };
    match key {
        Key::Up | Key::Left if h.selected_idx > 0 => {
            let next = h.selected_idx.saturating_sub(step);
            select_history_index(document, h, next, fonts, lw, lh, view);
        }
        Key::Down | Key::Right if h.selected_idx + 1 < h.witnesses.len() => {
            let next = (h.selected_idx + step).min(h.witnesses.len() - 1);
            select_history_index(document, h, next, fonts, lw, lh, view);
        }
        Key::PageUp => {
            if let Some(ref mut hed) = h.history_editor {
                hed.scroll_by(Fixed::ZERO - lh);
            }
        }
        Key::PageDown => {
            if let Some(ref mut hed) = h.history_editor {
                hed.scroll_by(lh);
            }
        }
        Key::Character('/') => {
            h.search_input = Some(String::new());
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_history_button(
    app: &mut App<store::DocumentSink, store::DocumentSource, FontSet, PixelSurface>,
    document: &Document,
    drawer: &mut Option<HistoryDrawerState>,
    btn: HistoryButton,
    fonts: &FontSet,
    lw: Fixed,
    lh: Fixed,
    view: View,
    status: &mut String,
) {
    match btn {
        HistoryButton::CloseLive => *drawer = None,
        HistoryButton::ScrubTrack(_) => {}
        HistoryButton::SelectWitness(idx) => {
            if let Some(h) = drawer {
                select_history_index(document, h, idx, fonts, lw, lh, view);
            }
        }
        HistoryButton::SelectBookmark(seq) => {
            if let Some(h) = drawer {
                if let Ok(idx) = h.witnesses.binary_search_by_key(&seq, |w| w.sequence) {
                    select_history_index(document, h, idx, fonts, lw, lh, view);
                }
            }
        }
        HistoryButton::SelectSearchHit(cursor) => {
            if let Some(h) = drawer {
                jump_to_search_hit(document, h, cursor, fonts, lw, lh, view);
            }
        }
        HistoryButton::StartBookmark => {
            if let Some(h) = drawer {
                h.search_input = None;
                h.bookmark_input = Some(String::new());
            }
        }
        HistoryButton::StartSearch => {
            if let Some(h) = drawer {
                h.bookmark_input = None;
                h.search_input = Some(String::new());
            }
        }
        HistoryButton::NextSearchHit => {
            if let Some(h) = drawer {
                if !h.search_hits.is_empty() {
                    let next = (h.search_cursor + 1) % h.search_hits.len();
                    jump_to_search_hit(document, h, next, fonts, lw, lh, view);
                }
            }
        }
        HistoryButton::SetDiffBase => {
            if let Some(h) = drawer {
                h.diff_base_idx = h.selected_idx;
                refresh_history_state(document, h, fonts, lw, lh, view);
            }
        }
        HistoryButton::DiffPrev => {
            if let Some(h) = drawer {
                h.diff_base_idx = h.selected_idx.saturating_sub(1);
                refresh_history_state(document, h, fonts, lw, lh, view);
            }
        }
        HistoryButton::RestoreSelected => {
            let Some(w_seq) = drawer
                .as_ref()
                .and_then(|h| h.witnesses.get(h.selected_idx))
                .map(|w| w.sequence)
            else {
                return;
            };
            let Ok(next_spine) = document.spine_at_witness(w_seq) else {
                return;
            };
            if app.editor().restore_spine(next_spine).is_ok() {
                let _ = app.editor().reflow();
                app.mark_dirty();
                *status = format!("restored from witness #{w_seq}");
                *drawer = None;
            }
        }
    }
}

fn handle_find_key(
    app: &mut App<store::DocumentSink, store::DocumentSource, FontSet, PixelSurface>,
    state: &mut FindState,
    key: Key,
    modifiers: Modifiers,
    status_msg: &mut String,
) -> bool {
    let mut close = false;
    let mut switch_to_replace = false;
    let mut trigger_enter = false;

    match key {
        Key::Backspace => match state {
            FindState::Find { needle, next_idx } => {
                needle.pop();
                *next_idx = 0;
            }
            FindState::Replace { replacement, .. } => {
                replacement.pop();
            }
        },
        Key::Tab if !modifiers.control => {
            switch_to_replace = true;
        }
        Key::Enter => {
            trigger_enter = true;
        }
        Key::Character(ch) if !modifiers.control && !modifiers.alt => match state {
            FindState::Find { needle, next_idx } => {
                needle.push(ch);
                *next_idx = 0;
            }
            FindState::Replace { replacement, .. } => {
                replacement.push(ch);
            }
        },
        _ => {}
    }

    if switch_to_replace {
        if let FindState::Find { needle, .. } = state {
            *state = FindState::Replace {
                needle: needle.clone(),
                replacement: String::new(),
            };
        }
        return false;
    }

    if trigger_enter {
        match state {
            FindState::Find { needle, next_idx } => {
                if needle.is_empty() {
                    close = true;
                } else if let Ok(summary) = app.editor().find_nth(needle, *next_idx) {
                    if summary.count == 0 {
                        *status_msg = format!("0 matches for '{needle}'");
                    } else {
                        let idx = if *next_idx < summary.count {
                            *next_idx
                        } else {
                            0
                        };
                        if let Some(hit) = summary.selected {
                            let _ = app.editor().select_range(hit.start, hit.end);
                            let _ = app.editor().apply(Intent::RevealCaret);
                            app.mark_dirty();
                        }
                        *status_msg = format!("match {}/{}", idx + 1, summary.count);
                        *next_idx = idx + 1;
                    }
                }
            }
            FindState::Replace {
                needle,
                replacement,
            } => {
                if !needle.is_empty() {
                    match app.editor().replace_all(needle, replacement) {
                        Ok(count) => {
                            let _ = app.editor().apply(Intent::RevealCaret);
                            app.mark_dirty();
                            *status_msg = format!("replaced {count} matches");
                        }
                        Err(err) => *status_msg = format!("replace refused: {err}"),
                    }
                }
                close = true;
            }
        }
    }

    close
}

#[cfg(test)]
mod tests {
    use super::*;
    use holonomy_text::{Appended, SegmentId, SegmentSource};
    use std::sync::Mutex;

    #[derive(Debug, Clone, Default)]
    struct MemStore(Arc<Mutex<Vec<Vec<u8>>>>);

    impl SegmentSink for MemStore {
        fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String> {
            let mut segments = self.0.lock().map_err(|_| "poisoned")?;
            let id = segments.len() as u64;
            segments.push(plaintext.to_vec());
            Ok(Appended {
                segment: SegmentId::new(id),
                witness_sequence: id + 1,
                segment_root: [0u8; 32],
                bytes_written: plaintext.len() as u64,
            })
        }
    }

    impl SegmentSource for MemStore {
        fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
            let segments = self.0.lock().map_err(|_| "poisoned")?;
            segments
                .get(usize::try_from(segment.get()).unwrap_or(usize::MAX))
                .cloned()
                .ok_or_else(|| format!("no {segment}"))
        }
    }

    type MemApp = App<MemStore, MemStore, FontSet, PixelSurface>;

    fn open_mem_app(view: View, w: u16, h: u16) -> MemApp {
        let store = MemStore::default();
        let fonts = FontSet::embedded(QUIET_MD_THEME.body_size_px).expect("embedded fonts");
        let residency =
            ResidentSet::new(store.clone(), store::BUDGET, store::SLACK).expect("residency");
        let surface = PixelSurface::new(fonts.clone(), view, w, h);
        let (lw, lh) = surface.logical_editor_extent();
        let typing = Typing::new(store, store::CAPACITY);
        let mut editor = Editor::new(typing, residency, fonts, lw, lh);
        editor.set_view(view);
        let _ = editor.reflow();
        App::new(editor, Keymap::new(lh), surface)
    }

    #[cfg(unix)]
    type GuiApp = App<store::DocumentSink, store::DocumentSource, FontSet, PixelSurface>;

    #[cfg(unix)]
    struct TempDoc(PathBuf);

    #[cfg(unix)]
    impl Drop for TempDoc {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[cfg(unix)]
    fn temp_doc(label: &str) -> (TempDoc, Document) {
        let id = holonomy_seal::generate_container_id().expect("entropy");
        let hex: String = id.iter().map(|b| format!("{b:02x}")).collect();
        let path = std::env::temp_dir().join(format!("holonomy-canvas-{label}-{hex}.wavefunction"));
        let doc = Document::create_with(&path, b"canvas-test-pass", store::test_params())
            .expect("creates encrypted test doc");
        (TempDoc(path), doc)
    }

    #[cfg(unix)]
    fn open_gui_app(doc: &Document, view: View, w: u16, h: u16) -> GuiApp {
        let fonts = FontSet::embedded(QUIET_MD_THEME.body_size_px).expect("embedded fonts");
        let spine = doc.spine().expect("spine");
        let residency =
            ResidentSet::new(doc.source(), store::BUDGET, store::SLACK).expect("residency");
        let surface = PixelSurface::new(fonts.clone(), view, w, h);
        let (lw, lh) = surface.logical_editor_extent();
        let mut editor = Editor::new(
            Typing::resume(doc.sink(), spine, store::CAPACITY),
            residency,
            fonts,
            lw,
            lh,
        );
        editor.set_view(view);
        let _ = editor.reflow();
        App::new(editor, Keymap::new(lh), surface)
    }

    #[test]
    fn pixel_surface_renders_placeholder_and_styled_clusters() {
        let mut app = open_mem_app(View::IDENTITY, 1_120, 760);
        assert!(app.draw().expect("initial empty draw"));
        app.surface_mut().draw_empty_placeholder();
        let bg = rgba_to_xrgb(QUIET_MD_THEME.bg_body);
        assert!(
            app.surface().pixels().iter().any(|&p| p != bg),
            "placeholder + desk sheet should paint non-background pixels"
        );
        assert_eq!(app.surface().painted_clusters(), 0);
        assert!(app.surface().caret_rect().is_some());

        for ch in "Hello Quiet Markdown".chars() {
            app.press(Key::Character(ch), Modifiers::none())
                .expect("type");
        }
        assert!(app.draw().expect("text draw"));
        assert!(app.surface().painted_clusters() >= 20);
        assert!(app.surface().caret_rect().is_some());
        // L-5: idle second draw skips repaint.
        assert!(!app.draw().expect("idle"));
    }

    #[test]
    fn toolbar_reflects_mixed_selection_and_applies_actions() {
        let mut app = open_mem_app(View::IDENTITY, 1_120, 760);
        for ch in "hello world".chars() {
            app.press(Key::Character(ch), Modifiers::none())
                .expect("type");
        }
        // Bold only "world" (bytes 6..11), then select all ("hello world").
        app.editor().select_range(6, 11).expect("select tail");
        apply_toolbar_action(&mut app, ToolbarAction::ToggleBold).expect("bold tail");
        app.editor().select_range(0, 11).expect("select mixed");
        assert_eq!(app.editor().toolbar_state().bold, TriState::Mixed);

        assert!(app.draw().expect("draw selection"));
        assert!(app.surface().painted_selections() > 0);
        let hits = render_toolbar(&mut app, SecurityDotState::Protected, false, false);
        assert!(
            hits.iter().any(|h| h.action == ToolbarAction::ToggleBold),
            "bold button hit-box must exist"
        );
        assert!(
            hits.iter()
                .any(|h| h.action == ToolbarAction::ToggleHistory),
            "history button hit-box must exist"
        );
        assert!(
            hits.iter()
                .any(|h| h.action == ToolbarAction::ToggleSecurity),
            "security dot hit-box must exist"
        );

        // Clicking ToggleBold on a mixed selection promotes all bytes to Bold (Uniform(true)).
        apply_toolbar_action(&mut app, ToolbarAction::ToggleBold).expect("promote bold");
        assert_eq!(app.editor().toolbar_state().bold, TriState::Uniform(true));

        // Zoom step updates both Editor::view and PixelSurface geometry together.
        apply_toolbar_action(&mut app, ToolbarAction::ZoomIn).expect("zoom in");
        assert_eq!(app.editor().view().zoom_milli(), 1_100);
    }

    #[test]
    fn pixel_surface_pointer_hit_test_matches_painted_caret_across_zooms_and_dpis() {
        let view = View::new(1_250, 144).expect("valid view");
        let mut app = open_mem_app(view, 1_120, 760);
        for ch in "Single geometry L-8 check".chars() {
            app.press(Key::Character(ch), Modifiers::none())
                .expect("type");
        }
        // Move caret to mid-line (byte 7) and render through PixelSurface.
        app.editor().select_range(7, 7).expect("caret at 7");
        app.mark_dirty();
        assert!(app.draw().expect("draw at 125% @ 144dpi"));

        let (cx, cy, _w, h) = app.surface().caret_rect().expect("painted caret rect");
        // Move caret away to end of doc, then click at the exact rendered caret pixel.
        app.editor().select_range(25, 25).expect("move to end");
        let pt = app
            .surface()
            .canvas_point(cx as i16, (cy + h / 2) as i16)
            .expect("canvas point inside viewport");
        let landed = app
            .editor()
            .click(pt)
            .expect("hit-test")
            .expect("caret hit");
        assert_eq!(
            landed.byte, 7,
            "clicking rendered caret at zoom 1.25 / 144 DPI must resolve back to byte 7"
        );
    }

    #[cfg(unix)]
    #[test]
    fn narrow_toolbar_collapses_overflow_and_overlays_render_cleanly() {
        let (_tmp, doc) = temp_doc("overlays");
        let mut app = open_gui_app(&doc, View::IDENTITY, 560, 480);
        for ch in "hello encrypted world".chars() {
            app.press(Key::Character(ch), Modifiers::none())
                .expect("type");
        }
        assert!(app
            .press(Key::Character('s'), Modifiers::control())
            .expect("save")
            .iter()
            .any(|e| matches!(e, Effect::Saved)));

        assert!(app.draw().expect("draw"));
        let hits = render_toolbar(&mut app, SecurityDotState::Unprotected, false, false);
        assert!(
            !hits.iter().any(|h| h.action == ToolbarAction::ZoomIn),
            "narrow width (< 720 px) must collapse overflow controls cleanly"
        );

        let facts = Facts::gather_with(app.residency(), &doc, LockCapability::None, None);
        render_security_overlay(app.surface_mut(), &facts);
        let (lw, lh) = app.surface().logical_editor_extent();
        let fonts = app.surface().fonts().clone();
        let mut hist = build_history_state(&doc, fonts, lw, lh, View::IDENTITY);
        render_history_viewport(app.surface_mut(), &mut hist);
        let hist_hits = render_history_drawer(app.surface_mut(), &hist, &doc.bookmarks());
        assert!(
            hist_hits
                .iter()
                .any(|(_, b)| *b == HistoryButton::RestoreSelected),
            "history drawer must expose Restore button"
        );
        assert!(
            hist_hits
                .iter()
                .any(|(_, b)| matches!(b, HistoryButton::ScrubTrack(_))),
            "history drawer must expose interactive scrubber track"
        );
        render_status_strip(
            app.surface_mut(),
            "doc.wavefunction",
            facts.resident_line().as_str(),
        );
    }

    #[cfg(unix)]
    #[test]
    fn block8_timeline_scrubber_diffs_bookmarks_history_search_and_restores() {
        let (_tmp, doc) = temp_doc("block8-scrubber");
        let mut app = open_gui_app(&doc, View::IDENTITY, 1_120, 760);

        for ch in "chapter one draft ".chars() {
            app.press(Key::Character(ch), Modifiers::none())
                .expect("type 1");
        }
        assert!(app
            .press(Key::Character('s'), Modifiers::control())
            .expect("save 1")
            .iter()
            .any(|e| matches!(e, Effect::Saved)));

        for ch in "added secret clue ".chars() {
            app.press(Key::Character(ch), Modifiers::none())
                .expect("type 2");
        }
        assert!(app
            .press(Key::Character('s'), Modifiers::control())
            .expect("save 2")
            .iter()
            .any(|e| matches!(e, Effect::Saved)));

        for ch in "final epilogue".chars() {
            app.press(Key::Character(ch), Modifiers::none())
                .expect("type 3");
        }
        assert!(app
            .press(Key::Character('s'), Modifiers::control())
            .expect("save 3")
            .iter()
            .any(|e| matches!(e, Effect::Saved)));

        let (lw, lh) = app.surface().logical_editor_extent();
        let fonts = app.surface().fonts().clone();
        let view = app.editor().view();
        let mut hist = build_history_state(&doc, fonts.clone(), lw, lh, view);
        assert!(hist.witnesses.len() >= 3);

        // Render drawer & scrub via ScrubTrack back to witness #1 ("chapter one draft ").
        render_history_viewport(app.surface_mut(), &mut hist);
        let hits = render_history_drawer(app.surface_mut(), &hist, &doc.bookmarks());
        let track = hits
            .iter()
            .find_map(|(_, b)| match *b {
                HistoryButton::ScrubTrack(r) => Some(r),
                _ => None,
            })
            .expect("scrubber track present");
        let target_idx = 1.min(hist.witnesses.len() - 1);
        select_history_index(&doc, &mut hist, target_idx, &fonts, lw, lh, view);
        let scrub_x = track.x;
        assert_eq!(scrub_index_for_x(track, scrub_x, hist.witnesses.len()), 0);

        // Inline bookmark naming saves directly to authenticated Document bookmarks.
        let mut drawer_opt = Some(hist);
        let mut status = String::new();
        handle_history_button(
            &mut app,
            &doc,
            &mut drawer_opt,
            HistoryButton::StartBookmark,
            &fonts,
            lw,
            lh,
            view,
            &mut status,
        );
        {
            let h = drawer_opt.as_mut().expect("drawer active");
            assert!(h.bookmark_input.is_some());
            for ch in "milestone".chars() {
                handle_history_key(
                    &doc,
                    h,
                    Key::Character(ch),
                    Modifiers::none(),
                    &fonts,
                    lw,
                    lh,
                    view,
                );
            }
            handle_history_key(&doc, h, Key::Enter, Modifiers::none(), &fonts, lw, lh, view);
        }
        assert!(
            doc.bookmarks().iter().any(|b| b.name == "milestone"),
            "inline bookmark name must persist"
        );

        // Inline search_history locates "clue" across historical witnesses and jumps scrubber.
        handle_history_button(
            &mut app,
            &doc,
            &mut drawer_opt,
            HistoryButton::StartSearch,
            &fonts,
            lw,
            lh,
            view,
            &mut status,
        );
        {
            let h = drawer_opt.as_mut().expect("drawer active");
            for ch in "clue".chars() {
                handle_history_key(
                    &doc,
                    h,
                    Key::Character(ch),
                    Modifiers::none(),
                    &fonts,
                    lw,
                    lh,
                    view,
                );
            }
            handle_history_key(&doc, h, Key::Enter, Modifiers::none(), &fonts, lw, lh, view);
            assert!(!h.search_hits.is_empty(), "search_history must find 'clue'");
            assert!(
                !h.diff_hunks.is_empty(),
                "diff_witnesses must populate hunks"
            );
            render_history_viewport(app.surface_mut(), h);
            assert!(app.surface().painted_clusters() > 0);
        }

        // Restore selected witness (contains "clue") and verify live editor updates cleanly.
        let selected_len = drawer_opt.as_ref().unwrap().preview_len;
        handle_history_button(
            &mut app,
            &doc,
            &mut drawer_opt,
            HistoryButton::RestoreSelected,
            &fonts,
            lw,
            lh,
            view,
            &mut status,
        );
        assert!(drawer_opt.is_none());
        assert_eq!(app.document_len(), selected_len);
    }

    #[test]
    fn desktop_app_idle_draw_performs_zero_repaints() {
        let mut app = open_mem_app(View::IDENTITY, 1_120, 760);
        for ch in "idle invariance check".chars() {
            app.press(Key::Character(ch), Modifiers::none())
                .expect("type");
        }
        assert!(app.draw().expect("paint changed frame"));
        let snapshot = app.surface().pixels().to_vec();
        for _ in 0..32 {
            assert!(
                !app.draw().expect("idle tick"),
                "idle desktop tick must skip repaint (`L-5`)"
            );
            assert!(!app.dirty());
        }
        assert_eq!(app.surface().pixels(), snapshot.as_slice());
    }

    #[test]
    fn selection_cut_copies_and_removes_selected_range() {
        let mut app = open_mem_app(View::IDENTITY, 1_120, 760);
        for ch in "alpha beta".chars() {
            app.press(Key::Character(ch), Modifiers::none())
                .expect("type");
        }
        app.editor().select_range(0, 6).expect("select 'alpha '");
        let cut = app
            .editor()
            .copy_selection(MAX_CLIPBOARD_BYTES)
            .expect("copy")
            .expect("selection non-empty");
        assert_eq!(cut, "alpha ");
        app.press(Key::Delete, Modifiers::none()).expect("delete");
        assert_eq!(app.frame_text().expect("frame"), "beta");
    }
}
