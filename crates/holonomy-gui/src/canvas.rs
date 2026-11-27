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
    estimate_passphrase_strength, probe_capability, CanarySet, LockCapability, PassphraseInput,
    ReaperGuard, SchedClass, SecretArenas, StrengthTier, TripReason, Tripwire,
    MAX_PASSPHRASE_INPUT_BYTES,
};
use holonomy_shell::{
    DocumentMetrics, Editor, Effect, Intent, PageTheme, Rgba8, QUIET_MD_DARK_THEME, QUIET_MD_THEME,
};
use holonomy_text::{
    Alignment, FontFamily, Heading, ListKind, ResidentSet, SegmentSink, SpineDiffHunk,
    SpineDiffKind, StylePatch, TriState, Typing,
};
use zeroize::{Zeroize, Zeroizing};

use crate::panel::Facts;
use crate::platform::{
    load_ui_scale, open_desktop_window, save_ui_scale, DesktopEvent, DesktopWindow, ScaledWindow,
    UiScale, MAX_CLIPBOARD_BYTES,
};
use crate::store::{self, Document, HistorySearchHit};
use crate::{App, Key, Keymap, Modifiers, SealWipeBridge, Surface};

const STATUS_BAR_HEIGHT_PX: i32 = 24;
// Chrome geometry quoted from quiet.md's own window: a ~216 px folder rail and a 40 px
// breadcrumb row. Both collapse on narrow windows so the page keeps a usable measure.
const DOC_SIDEBAR_W_PX: i32 = 216;
const DOC_BREADCRUMB_H_PX: i32 = 40;
const DOC_CHROME_MIN_WINDOW_PX: i32 = 860;
const PAGE_PAD_X_PX: i32 = 44;
const PAGE_PAD_TOP_PX: i32 = 24;
const PAUSE_AUTOSAVE_MS: u64 = 1_500;
const CHROME_IDLE_FADE_MS: u64 = 1_200;
const DOUBLE_CLICK_MS: u64 = 450;

#[inline]
const fn is_dark_theme(theme: &PageTheme) -> bool {
    theme.bg_body.r < 128
}

#[inline]
const fn desk_bg(theme: &PageTheme) -> Rgba8 {
    if is_dark_theme(theme) {
        Rgba8::rgb(0x14, 0x14, 0x13)
    } else {
        Rgba8::rgb(0xf6, 0xf7, 0xf9)
    }
}

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

    /// Fills `rect` with a vertical two-colour wash. Used for the quiet.md-style frosted
    /// sidebar: the real app blurs the wallpaper behind it, and reading that wallpaper here
    /// would be an OS-surface dependency (and a screenshot of the user's desktop) for no
    /// security benefit, so the same two colours are interpolated in the framebuffer instead.
    fn fill_vertical_gradient(&mut self, rect: Rect, top: Rgba8, bottom: Rgba8, clip: Rect) {
        let span = rect.h.max(1);
        for y in rect.y..rect.y + rect.h {
            let t = ((y - rect.y) * 255 / span) as u16;
            let mix = |a: u8, b: u8| -> u8 {
                ((u16::from(a) * (255 - t) + u16::from(b) * t + 127) / 255) as u8
            };
            let row = Rgba8::rgb(
                mix(top.r, bottom.r),
                mix(top.g, bottom.g),
                mix(top.b, bottom.b),
            );
            self.fill_rect_clipped(Rect::new(rect.x, y, rect.w, 1), row, clip);
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

    /// Horizontal span of a rounded rectangle on one scanline, inset by `radius` at the corners.
    fn rounded_span(rect: Rect, radius: i32, y: i32) -> Option<(i32, i32)> {
        if y < rect.y || y >= rect.y + rect.h || rect.w <= 0 || rect.h <= 0 {
            return None;
        }
        let r = radius.min(rect.w / 2).min(rect.h / 2).max(0);
        let center_y = if y < rect.y + r {
            rect.y + r
        } else if y >= rect.y + rect.h - r {
            rect.y + rect.h - r - 1
        } else {
            y
        };
        let dy = (y - center_y).abs();
        let inset = if r == 0 {
            0
        } else {
            let inner = (r * r - dy * dy).max(0) as f64;
            r - (inner.sqrt() as i32)
        };
        Some((rect.x + inset, rect.x + rect.w - inset))
    }

    /// Fills a rectangle with circular corners — the soft cards used by the Home shell.
    fn fill_rounded_rect(&mut self, rect: Rect, radius: i32, color: Rgba8, clip: Rect) {
        for y in rect.y..rect.y + rect.h {
            if let Some((x0, x1)) = Self::rounded_span(rect, radius, y) {
                if x1 > x0 {
                    self.fill_rect_clipped(Rect::new(x0, y, x1 - x0, 1), color, clip);
                }
            }
        }
    }

    /// Strokes a 1 px rounded outline without obscuring the interior.
    fn stroke_rounded_rect(&mut self, rect: Rect, radius: i32, color: Rgba8, clip: Rect) {
        let inner = Rect::new(rect.x + 1, rect.y + 1, rect.w - 2, rect.h - 2);
        for y in rect.y..rect.y + rect.h {
            let (Some((ox0, ox1)), Some((ix0, ix1))) = (
                Self::rounded_span(rect, radius, y),
                Self::rounded_span(inner, radius - 1, y),
            ) else {
                continue;
            };
            if ox1 - ox0 <= 0 || ix1 - ix0 <= 0 {
                continue;
            }
            self.fill_rect_clipped(Rect::new(ox0, y, (ix0 - ox0).max(1), 1), color, clip);
            self.fill_rect_clipped(Rect::new(ix1, y, (ox1 - ix1).max(1), 1), color, clip);
        }
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
    theme: PageTheme,
    fb: FrameBuffer,
    page_origin_x: i32,
    page_origin_y: i32,
    page_measure_px: i32,
    canvas_clip: Rect,
    sidebar_w: i32,
    breadcrumb_h: i32,
    painted_clusters: usize,
    painted_selections: usize,
    caret_rect: Option<(i32, i32, i32, i32)>,
    last_list_kind: ListKind,
    last_list_y: i32,
    ordered_counter: u32,
}

impl PixelSurface {
    /// Creates a [`PixelSurface`] for a `width × height` window.
    #[must_use]
    pub fn new(fonts: FontSet, view: View, width: u16, height: u16) -> Self {
        let theme = QUIET_MD_THEME;
        let mut s = Self {
            fonts,
            view,
            theme,
            fb: FrameBuffer::new(width.max(320), height.max(240), theme.bg_body),
            page_origin_x: 0,
            page_origin_y: i32::from(theme.toolbar_height_px) + PAGE_PAD_TOP_PX,
            page_measure_px: i32::from(theme.page_max_width_px),
            canvas_clip: Rect::new(0, 0, i32::from(width), i32::from(height)),
            sidebar_w: 0,
            breadcrumb_h: 0,
            painted_clusters: 0,
            painted_selections: 0,
            caret_rect: None,
            last_list_kind: ListKind::None,
            last_list_y: i32::MIN,
            ordered_counter: 0,
        };
        s.recompute_geometry(width.max(320), height.max(240));
        s
    }

    /// Active visual paper theme (`QUIET_MD_THEME` light or `QUIET_MD_DARK_THEME` dark).
    #[must_use]
    pub const fn theme(&self) -> PageTheme {
        self.theme
    }

    /// Switches the active surface theme (`UI-SPEC.md` §2 light/dark paper) and updates geometry.
    pub fn set_theme(&mut self, theme: PageTheme) {
        self.theme = theme;
        let (w, h) = self.window_size();
        self.recompute_geometry(w, h);
    }

    /// Updates zoom/DPI and recomputes centered `820 px` page column margins.
    pub fn set_viewport(&mut self, view: View, width: u16, height: u16) {
        self.view = view;
        self.recompute_geometry(width.max(320), height.max(240));
        self.fb
            .resize_and_clear(width.max(320), height.max(240), self.theme.bg_body);
    }

    fn recompute_geometry(&mut self, width: u16, height: u16) {
        let w = i32::from(width);
        let h = i32::from(height);
        // Reserve the chrome before computing the page: paint and hit-test share this one
        // origin, so a click on the page still lands on the glyph that was drawn (L-8).
        self.sidebar_w = if w >= DOC_CHROME_MIN_WINDOW_PX {
            DOC_SIDEBAR_W_PX.min(w / 3)
        } else {
            0
        };
        self.breadcrumb_h = if self.sidebar_w > 0 {
            DOC_BREADCRUMB_H_PX
        } else {
            0
        };
        let top = i32::from(self.theme.toolbar_height_px) + self.breadcrumb_h;
        let canvas_h = (h - top - STATUS_BAR_HEIGHT_PX).max(60);
        let content_w = w - self.sidebar_w;
        let max_page = i32::from(self.theme.page_max_width_px);
        let measure = (content_w - 2 * PAGE_PAD_X_PX).clamp(240, max_page);
        let side_margin = ((content_w - measure) / 2).max(16);
        self.page_measure_px = measure;
        self.page_origin_x = self.sidebar_w + side_margin;
        self.page_origin_y = top + PAGE_PAD_TOP_PX;
        self.canvas_clip = Rect::new(self.sidebar_w, top, content_w, canvas_h);
    }

    /// Width of the quiet.md-style folder rail, or `0` when the window is too narrow for it.
    #[must_use]
    pub const fn sidebar_width(&self) -> i32 {
        self.sidebar_w
    }

    /// Height of the breadcrumb row above the page, or `0` when chrome is collapsed.
    #[must_use]
    pub const fn breadcrumb_height(&self) -> i32 {
        self.breadcrumb_h
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
            self.theme.text_placeholder,
            self.canvas_clip,
        );
    }

    fn render_scrollbar(&mut self, metrics: DocumentMetrics) {
        let total_h = metrics.total_height.sixtyfourths();
        let view_h = self.logical_editor_extent().1.sixtyfourths().max(1);
        if total_h <= view_h {
            return;
        }
        let bar_x = self.canvas_clip.x + self.canvas_clip.w - 8;
        let track_top = self.canvas_clip.y + 4;
        let track_h = (self.canvas_clip.h - 8).max(24);
        let thumb_h =
            ((i64::from(track_h) * view_h) / total_h).clamp(24, i64::from(track_h)) as i32;
        let max_scroll = (total_h - view_h).max(1);
        let top_off = metrics.scroll_top.sixtyfourths().clamp(0, max_scroll);
        let travel = i64::from(track_h - thumb_h).max(0);
        let thumb_y = track_top + ((top_off * travel) / max_scroll) as i32;
        let thumb_color = Rgba8::rgba_pct(
            self.theme.text_caption.r,
            self.theme.text_caption.g,
            self.theme.text_caption.b,
            40,
        );
        self.fb.fill_rect_clipped(
            Rect::new(bar_x, thumb_y, 5, thumb_h),
            thumb_color,
            self.canvas_clip,
        );
    }

    fn render_ime_preedit(&mut self, preedit: &str) {
        if preedit.is_empty() {
            return;
        }
        let style = Style::DEFAULT;
        let line_h = self.to_px(self.fonts.line_height(style)).max(18);
        let ascent = self.to_px(self.fonts.ascent(style));
        let (cx, cy, _, ch) =
            self.caret_rect
                .unwrap_or((self.page_origin_x, self.page_origin_y, 1, line_h));
        let h = ch.max(line_h);
        let mut text_w = 0i32;
        for (_, cl) in Clusters::new(preedit) {
            text_w += self.to_px(self.fonts.advance(cl, style)).max(4);
        }
        let box_w = (text_w + 8).max(16);
        let max_x = (self.canvas_clip.x + self.canvas_clip.w - box_w).max(self.canvas_clip.x);
        let max_y = (self.canvas_clip.y + self.canvas_clip.h - h).max(self.canvas_clip.y);
        let bx = cx.clamp(self.canvas_clip.x, max_x);
        let by = cy.clamp(self.canvas_clip.y, max_y);
        let box_rect = Rect::new(bx, by, box_w, h);
        self.fb
            .fill_rect_clipped(box_rect, self.theme.code_bg, self.canvas_clip);
        self.fb.fill_rect_clipped(
            Rect::new(bx, by + h - 2, box_w, 2),
            self.theme.caret,
            self.canvas_clip,
        );
        draw_ui_text(
            &mut self.fb,
            &self.fonts,
            bx + 4,
            by + ascent,
            preedit,
            style,
            self.theme.text,
            box_rect,
        );
        self.fb.fill_rect_clipped(
            Rect::new(bx + box_w - 2, by + 2, 2, (h - 4).max(4)),
            self.theme.caret,
            self.canvas_clip,
        );
    }
}

impl Surface for PixelSurface {
    fn begin(&mut self) {
        self.painted_clusters = 0;
        self.painted_selections = 0;
        self.caret_rect = None;
        self.last_list_kind = ListKind::None;
        self.last_list_y = i32::MIN;
        self.ordered_counter = 0;
        let bg_desk = desk_bg(&self.theme);
        self.fb.fill_rect_clipped(
            self.canvas_clip,
            if self.page_origin_x > 24 {
                bg_desk
            } else {
                self.theme.bg_body
            },
            self.canvas_clip,
        );
        // Distinct quiet-md page sheet when desk margins exist.
        if self.page_origin_x > 24 {
            let sheet = Rect::new(
                self.page_origin_x - 24,
                self.canvas_clip.y + 8,
                self.page_measure_px + 48,
                (self.canvas_clip.h - 16).max(32),
            );
            self.fb
                .fill_rect_clipped(sheet, self.theme.bg_body, self.canvas_clip);
            self.fb
                .stroke_rect(sheet, self.theme.line_border, self.canvas_clip);
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
            self.theme.selection,
            self.canvas_clip,
        );
    }

    fn cluster(&mut self, x: Fixed, y: Fixed, text: &str, advance: Fixed, style: Style) {
        let dy = self.page_origin_y + self.to_px(y);
        let line_h_px = self.to_px(self.fonts.line_height(style)).max(1);
        let clip_top = self.canvas_clip.y - 16;
        let clip_bot = self.canvas_clip.y + self.canvas_clip.h + 16;
        if dy + line_h_px <= clip_top || dy >= clip_bot {
            return;
        }
        self.painted_clusters += 1;
        let dx = self.page_origin_x + self.to_px(x);
        let adv_px = (self.to_px(x + advance) - self.to_px(x)).max(0);
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
                    self.theme.quote_bar,
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
                    self.theme.line_divider,
                    self.canvas_clip,
                );
            }
            return;
        }

        if style.is_code_span() && adv_px > 0 {
            self.fb.fill_rect_clipped(
                Rect::new(dx, dy + 1, adv_px, (line_h_px - 2).max(4)),
                self.theme.code_bg,
                self.canvas_clip,
            );
        }

        let ink = if style.link_id().is_some() {
            self.theme.link
        } else {
            self.theme.text
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
        let clip_top = self.canvas_clip.y - 16;
        let clip_bot = self.canvas_clip.y + self.canvas_clip.h + 16;
        if dy >= clip_bot {
            return;
        }
        let mut run_start = 0usize;
        while run_start < clusters.len() {
            let style = clusters[run_start].style;
            let mut run_end = run_start + 1;
            while run_end < clusters.len() && clusters[run_end].style == style {
                run_end += 1;
            }
            let run = &clusters[run_start..run_end];

            let line_h_px = self.to_px(self.fonts.line_height(style)).max(1);
            let visible = dy + line_h_px > clip_top;
            let ascent_px = self.to_px(self.fonts.ascent(style));
            let baseline_y = dy + ascent_px;

            if run_start == 0 {
                let list_kind = style.list();
                let is_para_start = if origin == 0 && clusters[0].byte == 0 {
                    true
                } else if let Ok(rel) = usize::try_from(clusters[0].byte.saturating_sub(origin)) {
                    rel.checked_sub(1)
                        .and_then(|idx| text.as_bytes().get(idx))
                        .is_some_and(|&b| b == b'\n' || b == b'\r')
                } else {
                    false
                };
                if list_kind == ListKind::None {
                    self.last_list_kind = ListKind::None;
                    self.ordered_counter = 0;
                } else if is_para_start && dy != self.last_list_y {
                    self.last_list_y = dy;
                    let run_dx_first = self.page_origin_x + self.to_px(clusters[0].x);
                    let marker_ink = self.theme.text_caption;
                    match list_kind {
                        ListKind::Bullet { depth } => {
                            self.ordered_counter = 0;
                            self.last_list_kind = list_kind;
                            if visible {
                                let glyph = if depth <= 1 {
                                    "\u{2022}"
                                } else if depth == 2 {
                                    "\u{25E6}"
                                } else {
                                    "\u{25AA}"
                                };
                                draw_ui_text(
                                    &mut self.fb,
                                    &self.fonts,
                                    (run_dx_first - 16).max(4),
                                    baseline_y,
                                    glyph,
                                    style,
                                    marker_ink,
                                    self.canvas_clip,
                                );
                            }
                        }
                        ListKind::Ordered { start, .. } => {
                            self.ordered_counter =
                                if matches!(self.last_list_kind, ListKind::Ordered { .. }) {
                                    self.ordered_counter.saturating_add(1)
                                } else {
                                    start
                                };
                            self.last_list_kind = list_kind;
                            if visible {
                                let label = format!("{}.", self.ordered_counter);
                                let label_w =
                                    (self.fonts.advance(&label, style).sixtyfourths() >> 6) as i32;
                                draw_ui_text(
                                    &mut self.fb,
                                    &self.fonts,
                                    (run_dx_first - label_w - 6).max(4),
                                    baseline_y,
                                    &label,
                                    style,
                                    marker_ink,
                                    self.canvas_clip,
                                );
                            }
                        }
                        ListKind::None => {}
                    }
                }
            }

            if !visible {
                run_start = run_end;
                continue;
            }
            self.painted_clusters += run.len();

            if style.blockquote() > 0 && run[0].x <= Fixed::units(24) {
                for level in 0..i32::from(style.blockquote().min(4)) {
                    self.fb.fill_rect_clipped(
                        Rect::new(
                            self.page_origin_x - 14 - level * 8,
                            dy + 2,
                            3,
                            (line_h_px - 4).max(4),
                        ),
                        self.theme.quote_bar,
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
                        self.theme.line_divider,
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
                    self.theme.code_bg,
                    self.canvas_clip,
                );
            }

            let ink = if style.link_id().is_some() {
                self.theme.link
            } else {
                self.theme.text
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
        let w = i32::from(self.theme.caret_width_pt).max(1);
        let h = self.to_px(height).max(8);
        let rect = Rect::new(dx, dy, w, h);
        self.fb
            .fill_rect_clipped(rect, self.theme.caret, self.canvas_clip);
        self.caret_rect = Some((dx, dy, w, h));
    }
}

/// Interactive action triggered by clicking a Ribbon button (`UI-SPEC.md` §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolbarAction {
    /// Save and return to the Home document library.
    GoHome,
    /// Cycle font family (`Sans` -> `Serif` -> `Mono`).
    CycleFamily,
    /// Cycle body font size (`13` -> `15` -> `17` -> `21` -> `25` -> `34` px).
    CycleSize,
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
    /// Open or edit hyperlink URL (`Ctrl+K`).
    OpenLinkDialog,
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
    /// Cycle whole-desktop display scale (`100/125/133/150/175/200%`).
    CycleUiScale,
    /// Toggle light/dark Quiet Markdown paper theme (`#FFFFFF` / `#1A1A19`).
    ToggleTheme,
    /// Toggle reduced-motion static chrome mode.
    ToggleReducedMotion,
    /// Toggle the `⋯` overflow toolbar menu when narrow.
    ToggleOverflow,
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
    theme: &PageTheme,
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
                // `D-101`: an active pill used the light Home wash over dark paper, which read
                // as a grey smudge; the wash follows the paper theme now.
                let (fill, border) = if is_dark_theme(theme) {
                    (
                        Rgba8::rgba_pct(0x7a, 0xb0, 0xff, 18),
                        Rgba8::rgba_pct(0x7a, 0xb0, 0xff, 55),
                    )
                } else {
                    (HOME_WASH, Rgba8::rgba_pct(0x2f, 0x6f, 0xeb, 55))
                };
                fb.fill_rounded_rect(rect, 7, fill, clip);
                fb.stroke_rounded_rect(rect, 7, border, clip);
            }
            ButtonHighlight::Mixed => {
                fb.fill_rounded_rect(rect, 7, theme.code_bg, clip);
                fb.stroke_rounded_rect(rect, 7, theme.line_border, clip);
            }
            ButtonHighlight::Off => {
                fb.fill_rounded_rect(rect, 7, soft_fill_for(theme), clip);
            }
        }
    }
    let ink = if disabled {
        theme.text_placeholder
    } else if matches!(highlight, ButtonHighlight::Active) {
        theme.link
    } else {
        theme.text
    };
    let text_w = label_w as i32;
    let tx = x + (w - text_w) / 2;
    let ty = y + 17;
    draw_ui_text(fb, fonts, tx, ty, label, style, ink, clip);
    if !disabled && matches!(highlight, ButtonHighlight::Mixed) {
        fb.fill_rect_clipped(
            Rect::new(x + w / 2 - 4, y + h - 4, 8, 2),
            theme.text_caption,
            clip,
        );
    }
    (rect, x + w + 4)
}

/// Renders the fixed `40 px` top Ribbon (`UI-SPEC.md` §3, Law `U0`) directly from `app.editor().toolbar_state()`.
#[cfg(test)]
fn render_toolbar<S, R>(
    app: &mut App<S, R, FontSet, PixelSurface>,
    dot_state: SecurityDotState,
    read_only_history: bool,
    history_open: bool,
    display_percent: u16,
) -> Vec<ToolbarHit>
where
    S: holonomy_text::SegmentSink,
    R: holonomy_text::SegmentSource,
{
    render_toolbar_with(
        app,
        dot_state,
        read_only_history,
        history_open,
        false,
        false,
        display_percent,
    )
}

/// Renders the fixed `40 px` top Ribbon plus optional `⋯` overflow popup (`UI-SPEC.md` §3, Law `U0`)
/// and returns clickable hit boxes.
#[allow(clippy::too_many_lines)]
fn render_toolbar_with<S, R>(
    app: &mut App<S, R, FontSet, PixelSurface>,
    dot_state: SecurityDotState,
    read_only_history: bool,
    history_open: bool,
    overflow_open: bool,
    reduced_motion: bool,
    display_percent: u16,
) -> Vec<ToolbarHit>
where
    S: holonomy_text::SegmentSink,
    R: holonomy_text::SegmentSource,
{
    let tb = app.editor().toolbar_state();
    let zoom_pct = (app.editor().view().zoom_milli() / 10).clamp(50, 300);
    let surface = app.surface_mut();
    let theme = surface.theme();
    let (win_w, win_h) = surface.window_size();
    let fonts = surface.fonts().clone();
    let fb = surface.framebuffer_mut();

    let bar_h = i32::from(theme.toolbar_height_px);
    let bar = Rect::new(0, 0, i32::from(win_w), bar_h);
    fb.fill_rect_clipped(bar, theme.bg_body, bar);
    fb.fill_rect_clipped(
        Rect::new(0, bar_h - 1, i32::from(win_w), 1),
        theme.line_divider,
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
    let size_label = match tb.size_milli {
        TriState::Uniform(m) => format!("{}px", (m * 15 + 500) / 1_000),
        TriState::Mixed => "\u{2014}".to_owned(),
    };
    let link_hl = match &tb.link {
        TriState::Uniform(Some(_)) => ButtonHighlight::Active,
        TriState::Uniform(None) => ButtonHighlight::Off,
        TriState::Mixed => ButtonHighlight::Mixed,
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
    let theme_label = if is_dark_theme(&theme) {
        "Dark"
    } else {
        "Light"
    };
    let motion_label = if reduced_motion { "Still" } else { "Anim" };
    let scale_label = format!("Scale {display_percent}%");

    let dot_w = 24;
    let hist_w = 42;
    let right_reserved = dot_w + hist_w + 18;
    let max_left_x = i32::from(win_w) - right_reserved;
    let narrow = win_w < theme.toolbar_overflow_width_px;

    let mut hits = Vec::new();
    let mut x = 12;
    let y = 7;
    let gap = i32::from(theme.toolbar_group_gap_px);

    let items: &[(ToolbarAction, &str, ButtonHighlight, bool, bool)] = &[
        (
            ToolbarAction::GoHome,
            "Home",
            ButtonHighlight::Off,
            true,
            false,
        ),
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
            ToolbarAction::CycleSize,
            size_label.as_str(),
            if matches!(tb.size_milli, TriState::Mixed) {
                ButtonHighlight::Mixed
            } else {
                ButtonHighlight::Off
            },
            false,
            true,
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
            false,
            false,
        ),
        (ToolbarAction::OpenLinkDialog, "Link", link_hl, true, true),
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
        (
            ToolbarAction::CycleUiScale,
            scale_label.as_str(),
            ButtonHighlight::Off,
            false,
            true,
        ),
        (
            ToolbarAction::ToggleTheme,
            theme_label,
            if is_dark_theme(&theme) {
                ButtonHighlight::Active
            } else {
                ButtonHighlight::Off
            },
            false,
            true,
        ),
        (
            ToolbarAction::ToggleReducedMotion,
            motion_label,
            if reduced_motion {
                ButtonHighlight::Active
            } else {
                ButtonHighlight::Off
            },
            false,
            true,
        ),
    ];

    let mut collapsed: Vec<(ToolbarAction, &str, ButtonHighlight)> = Vec::new();
    for &(action, label, hl, end_group, collapsible) in items {
        if narrow && collapsible {
            collapsed.push((action, label, hl));
            continue;
        }
        let est_w = (fonts.advance(label, Style::at(880)).sixtyfourths() >> 6) as i32 + 20;
        let overflow_reserve = if collapsed.is_empty() { 0 } else { 32 };
        if x + est_w + overflow_reserve > max_left_x {
            collapsed.push((action, label, hl));
            continue;
        }
        let (rect, next_x) = draw_toolbar_button(
            fb,
            &fonts,
            x,
            y,
            label,
            hl,
            read_only_history
                && !matches!(
                    action,
                    ToolbarAction::ZoomIn
                        | ToolbarAction::ZoomOut
                        | ToolbarAction::CycleUiScale
                        | ToolbarAction::GoHome
                ),
            &theme,
            bar,
        );
        hits.push(ToolbarHit { rect, action });
        x = next_x + if end_group { gap } else { 0 };
    }

    let mut overflow_anchor = None;
    if (!collapsed.is_empty() || narrow) && x + 28 <= max_left_x {
        let (ov_rect, ov_next) = draw_toolbar_button(
            fb,
            &fonts,
            x,
            y,
            "\u{22EF}",
            if overflow_open {
                ButtonHighlight::Active
            } else {
                ButtonHighlight::Off
            },
            false,
            &theme,
            bar,
        );
        hits.push(ToolbarHit {
            rect: ov_rect,
            action: ToolbarAction::ToggleOverflow,
        });
        overflow_anchor = Some(ov_rect);
        x = ov_next;
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
            theme.text_caption,
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
        &theme,
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
        theme.line_border,
        bar,
    );
    hits.push(ToolbarHit {
        rect: dot_rect,
        action: ToolbarAction::ToggleSecurity,
    });

    if overflow_open {
        let items_to_show = if collapsed.is_empty() {
            vec![
                (
                    ToolbarAction::CycleUiScale,
                    scale_label.as_str(),
                    ButtonHighlight::Off,
                ),
                (
                    ToolbarAction::ToggleTheme,
                    theme_label,
                    if is_dark_theme(&theme) {
                        ButtonHighlight::Active
                    } else {
                        ButtonHighlight::Off
                    },
                ),
                (
                    ToolbarAction::ToggleReducedMotion,
                    motion_label,
                    if reduced_motion {
                        ButtonHighlight::Active
                    } else {
                        ButtonHighlight::Off
                    },
                ),
            ]
        } else {
            collapsed
        };
        let menu_w = 240.min(i32::from(win_w) - 24);
        let anchor_x = overflow_anchor.map_or(12, |r| r.x);
        let menu_x = anchor_x.min(i32::from(win_w) - menu_w - 12).max(12);
        let mut mx = menu_x + 8;
        let mut my = bar_h + 10;
        let rows_est = ((items_to_show.len() as i32) * 52 / (menu_w - 16).max(40)) + 2;
        let menu_h = (rows_est * 32 + 12).min(i32::from(win_h) - bar_h - 40);
        let menu = Rect::new(menu_x, bar_h + 2, menu_w, menu_h.max(44));
        fb.fill_rect_clipped(menu, theme.bg_body, menu);
        fb.stroke_rect(menu, theme.line_border, menu);
        for (action, label, hl) in items_to_show {
            let est = (fonts.advance(label, Style::at(880)).sixtyfourths() >> 6) as i32 + 20;
            if mx + est > menu.x + menu.w - 8 {
                mx = menu.x + 8;
                my += 30;
            }
            if my + 26 > menu.y + menu.h - 4 {
                break;
            }
            let (rect, nx) = draw_toolbar_button(
                fb,
                &fonts,
                mx,
                my,
                label,
                hl,
                read_only_history
                    && !matches!(
                        action,
                        ToolbarAction::ZoomIn
                            | ToolbarAction::ZoomOut
                            | ToolbarAction::CycleUiScale
                            | ToolbarAction::GoHome
                            | ToolbarAction::ToggleTheme
                            | ToolbarAction::ToggleReducedMotion
                    ),
                &theme,
                menu,
            );
            hits.push(ToolbarHit { rect, action });
            mx = nx;
        }
    }

    hits
}

fn render_recovery_banner(
    surface: &mut PixelSurface,
    trailing_bytes: u64,
    offset_y: i32,
) -> Option<Rect> {
    if trailing_bytes == 0 {
        return None;
    }
    let theme = surface.theme();
    let (w, _) = surface.window_size();
    let fonts = surface.fonts().clone();
    let fb = surface.framebuffer_mut();
    let bar = Rect::new(0, offset_y, i32::from(w), 28);
    fb.fill_rect_clipped(bar, Rgba8::rgba_pct(0xe2, 0xa0, 0x3f, 22), bar);
    fb.fill_rect_clipped(
        Rect::new(0, offset_y + 27, i32::from(w), 1),
        theme.line_divider,
        bar,
    );
    let msg = format!(
        "Recovered to the last safe point \u{2014} nothing else was stored ({trailing_bytes} trailing B discarded)."
    );
    draw_ui_text(
        fb,
        &fonts,
        12,
        offset_y + 19,
        &msg,
        Style::at(820),
        theme.text,
        bar,
    );
    let (dismiss_rect, _) = draw_toolbar_button(
        fb,
        &fonts,
        (i32::from(w) - 78).max(12),
        offset_y + 1,
        "Dismiss",
        ButtonHighlight::Off,
        false,
        &theme,
        bar,
    );
    Some(dismiss_rect)
}

fn render_link_bar(surface: &mut PixelSurface, url: &str) {
    let theme = surface.theme();
    let (w, h) = surface.window_size();
    let fonts = surface.fonts().clone();
    let fb = surface.framebuffer_mut();
    let rect = Rect::new(
        0,
        i32::from(h) - STATUS_BAR_HEIGHT_PX - 30,
        i32::from(w),
        30,
    );
    fb.fill_rect_clipped(rect, theme.code_bg, rect);
    fb.stroke_rect(rect, theme.link, rect);
    let prompt_line = format!("Link URL: {url}|   (Enter set, empty clears, Esc cancel)");
    draw_ui_text(
        fb,
        &fonts,
        14,
        rect.y + 20,
        &prompt_line,
        Style::at(880),
        theme.text,
        rect,
    );
}

fn render_status_strip(surface: &mut PixelSurface, status_left: &str, status_right: &str) {
    let theme = surface.theme();
    let (w, h) = surface.window_size();
    let fonts = surface.fonts().clone();
    let fb = surface.framebuffer_mut();
    let y0 = i32::from(h) - STATUS_BAR_HEIGHT_PX;
    let strip = Rect::new(0, y0, i32::from(w), STATUS_BAR_HEIGHT_PX);
    fb.fill_rect_clipped(strip, theme.code_gutter, strip);
    fb.fill_rect_clipped(Rect::new(0, y0, i32::from(w), 1), theme.line_divider, strip);
    let st = Style::at(800);
    draw_ui_text(
        fb,
        &fonts,
        12,
        y0 + 16,
        status_left,
        st,
        theme.text_caption,
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
            theme.text_caption,
            strip,
        );
    }
}

fn render_security_overlay(surface: &mut PixelSurface, facts: &Facts) {
    let theme = surface.theme();
    let (w, _) = surface.window_size();
    let fonts = surface.fonts().clone();
    let fb = surface.framebuffer_mut();
    let lines = facts.lines();
    let card_w = i32::from(w).min(600) - 24;
    let card_h = 40 + (lines.len() as i32) * 20;
    let card_x = (i32::from(w) - card_w - 14).max(12);
    let card_y = i32::from(theme.toolbar_height_px) + 8;
    let card = Rect::new(card_x, card_y, card_w, card_h);

    fb.fill_rect_clipped(card, theme.bg_body, card);
    fb.stroke_rect(card, theme.line_border, card);

    draw_ui_text(
        fb,
        &fonts,
        card_x + 14,
        card_y + 22,
        "Security & Residency (Esc to close)",
        Style::at(920).bold(),
        theme.text,
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
            theme.text_caption,
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
    preview_text: Zeroizing<String>,
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
    let theme = surface.theme();
    let (w, h) = surface.window_size();
    let fonts = surface.fonts().clone();
    let fb = surface.framebuffer_mut();
    let top = i32::from(theme.toolbar_height_px);
    let drawer_w = (i32::from(w) / 3).clamp(300, 410);
    let drawer_h = (i32::from(h) - top - STATUS_BAR_HEIGHT_PX).max(160);
    let drawer = Rect::new(i32::from(w) - drawer_w, top, drawer_w, drawer_h);

    fb.fill_rect_clipped(drawer, theme.bg_body, drawer);
    fb.stroke_rect(drawer, theme.line_border, drawer);

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
        theme.text,
        drawer,
    );
    y += 10;

    // Interactive scrubber track across all witnesses with bookmark + search-hit markers.
    let track = Rect::new(pad_x, y, drawer_w - 28, 22);
    let rail = Rect::new(track.x, track.y + 9, track.w, 4);
    fb.fill_rect_clipped(rail, theme.code_gutter, drawer);
    fb.stroke_rect(rail, theme.line_border, drawer);

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
            fb.fill_rect_clipped(Rect::new(hx, track.y + 3, 2, 16), theme.link, drawer);
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
    fb.fill_rect_clipped(knob, theme.link, drawer);
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
        &theme,
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
        &theme,
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
        &theme,
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
        &theme,
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
        &theme,
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
        &theme,
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
            &theme,
            drawer,
        );
        hits.push((nh_rect, HistoryButton::NextSearchHit));
    }
    y += 28;

    // Inline bookmark or history search input box when active.
    if let Some(ref draft_bm) = state.bookmark_input {
        let box_rect = Rect::new(pad_x, y, drawer_w - 28, 24);
        fb.fill_rect_clipped(box_rect, theme.code_bg, drawer);
        fb.stroke_rect(box_rect, theme.link, drawer);
        draw_ui_text(
            fb,
            &fonts,
            box_rect.x + 6,
            box_rect.y + 16,
            &format!("Bookmark name: {draft_bm}| (Enter save)"),
            Style::at(800),
            theme.text,
            box_rect,
        );
        y += 28;
    } else if let Some(ref query) = state.search_input {
        let box_rect = Rect::new(pad_x, y, drawer_w - 28, 24);
        fb.fill_rect_clipped(box_rect, theme.code_bg, drawer);
        fb.stroke_rect(box_rect, theme.link, drawer);
        draw_ui_text(
            fb,
            &fonts,
            box_rect.x + 6,
            box_rect.y + 16,
            &format!("Find across history: {query}| (Enter)"),
            Style::at(800),
            theme.text,
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
            theme.text_caption,
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
            theme.link,
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
                &theme,
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
                theme.link,
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
                theme.link
            } else {
                theme.text
            },
            drawer,
        );
        hits.push((row_rect, HistoryButton::SelectWitness(idx)));
        y += 21;
    }

    // Excerpt preview box
    let prev_rect = Rect::new(drawer.x + 10, drawer.y + drawer_h - 98, drawer_w - 20, 88);
    fb.fill_rect_clipped(prev_rect, theme.code_bg, drawer);
    fb.stroke_rect(prev_rect, theme.line_border, drawer);
    let hdr = format!("Snapshot ({} bytes):", state.preview_len);
    draw_ui_text(
        fb,
        &fonts,
        prev_rect.x + 8,
        prev_rect.y + 15,
        &hdr,
        Style::at(780).bold(),
        theme.text_caption,
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
            theme.text,
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
        ToolbarAction::CycleSize => {
            let cur_px = match app.editor().toolbar_state().size_milli {
                TriState::Uniform(m) => (m * 15 + 500) / 1_000,
                TriState::Mixed => 15,
            };
            let next_px = match cur_px {
                0..=13 => 15,
                14..=15 => 17,
                16..=17 => 21,
                18..=21 => 25,
                22..=25 => 34,
                _ => 13,
            };
            let next_milli = (next_px * 1_000) / 15;
            let _ = app.editor().apply(Intent::Format(StylePatch {
                size_milli: Some(next_milli),
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
        ToolbarAction::ToggleTheme => {
            let next = if is_dark_theme(&app.surface().theme()) {
                QUIET_MD_THEME
            } else {
                QUIET_MD_DARK_THEME
            };
            app.surface_mut().set_theme(next);
            app.mark_dirty();
        }
        ToolbarAction::GoHome
        | ToolbarAction::CycleUiScale
        | ToolbarAction::OpenLinkDialog
        | ToolbarAction::ToggleOverflow
        | ToolbarAction::ToggleReducedMotion
        | ToolbarAction::ToggleHistory
        | ToolbarAction::ToggleSecurity => {}
    }
    Ok(())
}

/// Visual tokens for the Home shell. They mirror the calm, low-contrast palette of the
/// `quiet-md` inspiration while staying independent of the document parity theme.
///
/// Chrome palette is quoted from quiet.md's own theme tokens where they exist (caret ink and
/// caption ink are `QUIET_MD_THEME.caret`/`.text_caption`); the frosted gradient is sampled from
/// its Home screenshot because the sidebar paints over the user's wallpaper, which this surface
/// cannot and must not read. No document plaintext enters any of these colours.
///
/// These are *light* tokens. Anything painted over the page theme must go through
/// [`ink_for`]/[`muted_ink_for`]/[`accent_ink_for`]/[`faint_for`]/[`soft_fill_for`] instead.
const HOME_SIDE_TOP: Rgba8 = Rgba8::rgb(0xc2, 0xd9, 0xea);
const HOME_SIDE_BOTTOM: Rgba8 = Rgba8::rgb(0xd8, 0xe4, 0xd6);
const HOME_CANVAS_BG: Rgba8 = Rgba8::rgb(0xff, 0xff, 0xff);
const HOME_SURFACE: Rgba8 = Rgba8::rgb(0xff, 0xff, 0xff);
const HOME_CARD: Rgba8 = Rgba8::rgb(0xf3, 0xf4, 0xf5);
const HOME_TAB: Rgba8 = Rgba8::rgb(0xe1, 0xe4, 0xe7);
const HOME_INK: Rgba8 = Rgba8::rgb(0x1f, 0x22, 0x25);
const HOME_MUTED: Rgba8 = Rgba8::rgb(0x73, 0x74, 0x77);
const HOME_FAINT: Rgba8 = Rgba8::rgb(0xe3, 0xe5, 0xe7);
const HOME_ACCENT: Rgba8 = Rgba8::rgb(0x3a, 0x85, 0xf7);
const HOME_WASH: Rgba8 = Rgba8::rgba_pct(0x3a, 0x85, 0xf7, 12);
const HOME_SELECTION: Rgba8 = Rgba8::rgba_pct(0xff, 0xff, 0xff, 34);

const HOME_SIDEBAR_W: i32 = 216;
const HOME_CARD_MIN_W: i32 = 208;
const HOME_CARD_H: i32 = 196;
const HOME_GAP: i32 = 20;

/// Chrome ink for surfaces that are painted over the *page* theme rather than over Home's fixed
/// white library (`D-101`). The Home palette above is quoted from quiet.md's light Home screen,
/// so reusing `HOME_INK` on `#1A1A19` dark paper produced ink at 1.09:1 contrast — effectively
/// invisible. Every writing-view surface selects ink through these helpers instead.
const HOME_ACCENT_ON_DARK: Rgba8 = Rgba8::rgb(0x7a, 0xb0, 0xff);

/// Primary chrome ink for `theme`'s paper.
#[inline]
const fn ink_for(theme: &PageTheme) -> Rgba8 {
    if is_dark_theme(theme) {
        theme.text
    } else {
        HOME_INK
    }
}

/// Secondary/caption chrome ink for `theme`'s paper.
#[inline]
const fn muted_ink_for(theme: &PageTheme) -> Rgba8 {
    if is_dark_theme(theme) {
        theme.text_caption
    } else {
        HOME_MUTED
    }
}

/// Accent (shortcut keys, links) ink with enough contrast on `theme`'s paper.
#[inline]
const fn accent_ink_for(theme: &PageTheme) -> Rgba8 {
    if is_dark_theme(theme) {
        HOME_ACCENT_ON_DARK
    } else {
        HOME_ACCENT
    }
}

/// Hairline/border token for `theme`'s paper.
#[inline]
const fn faint_for(theme: &PageTheme) -> Rgba8 {
    if is_dark_theme(theme) {
        theme.line_border
    } else {
        HOME_FAINT
    }
}

/// Soft control fill (tabs, inactive pills) for `theme`'s paper.
#[inline]
const fn soft_fill_for(theme: &PageTheme) -> Rgba8 {
    if is_dark_theme(theme) {
        theme.code_bg
    } else {
        HOME_CARD
    }
}

/// Selected-row / raised-control wash for `theme`'s paper.
#[inline]
const fn selection_for(theme: &PageTheme) -> Rgba8 {
    if is_dark_theme(theme) {
        Rgba8::rgba_pct(0xff, 0xff, 0xff, 10)
    } else {
        HOME_SELECTION
    }
}

/// Everything the writing-view chrome may show: file names and folder names only. The type
/// deliberately has no `Document`, `Spine`, `SegmentSource` or `String` of document bytes, so
/// adding a sidebar cannot accidentally widen the resident plaintext set (law L-2).
#[derive(Debug, Clone)]
struct DocumentChromeData {
    library_label: String,
    folder_label: Option<String>,
    document_label: String,
    folders: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocumentChromeHit {
    Home,
    Folder(usize),
    Find,
    History,
    Security,
    Help,
}

fn document_chrome_data(doc_path: &Path, folders: Vec<String>) -> DocumentChromeData {
    let document_label = doc_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document")
        .to_owned();
    let folder_label = doc_path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty() && *n != ".")
        .map(str::to_owned);
    DocumentChromeData {
        library_label: home_library_label(),
        folder_label,
        document_label,
        folders,
    }
}

/// Draws the quiet.md-style folder rail beside the page. Paint stays inside `rect`; the only
/// text drawn comes from [`DocumentChromeData`], which carries metadata, never document bytes.
///
/// The rail keeps quiet.md's frosted wash on light paper (`D-101`: the wash is quoted from the
/// light Home screenshot). On dark paper it would be a bright slab next to `#1A1A19`, so it uses
/// the theme's own chrome colours there and every ink token follows the paper underneath.
fn render_document_sidebar(
    fb: &mut FrameBuffer,
    fonts: &FontSet,
    theme: PageTheme,
    rect: Rect,
    data: &DocumentChromeData,
) -> Vec<(Rect, DocumentChromeHit)> {
    let mut hits = Vec::new();
    if rect.w <= 0 || rect.h <= 0 {
        return hits;
    }
    let dark = is_dark_theme(&theme);
    let ink = ink_for(&theme);
    let muted = muted_ink_for(&theme);
    let accent = accent_ink_for(&theme);
    let faint = faint_for(&theme);
    let selection = selection_for(&theme);
    if dark {
        fb.fill_rect_clipped(rect, theme.code_gutter, rect);
    } else {
        fb.fill_vertical_gradient(rect, HOME_SIDE_TOP, HOME_SIDE_BOTTOM, rect);
    }
    fb.fill_rect_clipped(
        Rect::new(rect.x + rect.w - 1, rect.y, 1, rect.h),
        faint,
        rect,
    );

    let x = rect.x + 16;
    let inner = rect.w - 32;
    draw_home_text(
        fb,
        fonts,
        x,
        rect.y + 32,
        &home_fit_text(fonts, &data.library_label, 940, inner),
        940,
        ink,
        rect,
        true,
    );

    let home_row = Rect::new(x, rect.y + 54, inner, 34);
    fb.fill_rounded_rect(home_row, 8, selection, rect);
    draw_home_folder_icon(fb, x + 12, rect.y + 62, ink, rect);
    draw_home_text(
        fb,
        fonts,
        x + 38,
        rect.y + 76,
        "Home",
        880,
        ink,
        rect,
        false,
    );
    hits.push((home_row, DocumentChromeHit::Home));

    draw_home_text(
        fb,
        fonts,
        x,
        rect.y + 122,
        "My documents",
        760,
        muted,
        rect,
        false,
    );

    let actions_top = rect.y + rect.h - 96;
    let mut sy = rect.y + 144;
    for (idx, folder) in data.folders.iter().enumerate() {
        if sy + 30 > actions_top - 52 {
            break;
        }
        let row = Rect::new(x, sy - 8, inner, 30);
        let active = data.folder_label.as_deref() == Some(folder.as_str());
        if active {
            fb.fill_rounded_rect(row, 7, selection, rect);
        }
        let icon_ink = if active { accent } else { muted };
        draw_home_folder_icon(fb, x + 12, sy - 4, icon_ink, rect);
        draw_home_text(
            fb,
            fonts,
            x + 38,
            sy + 12,
            &home_fit_text(fonts, folder, 820, inner - 48),
            820,
            ink,
            rect,
            false,
        );
        hits.push((row, DocumentChromeHit::Folder(idx)));
        sy += 32;
    }

    draw_home_text(
        fb,
        fonts,
        x,
        actions_top - 48,
        "This document",
        760,
        muted,
        rect,
        false,
    );
    let doc_row = Rect::new(x, actions_top - 42, inner, 30);
    fb.fill_rounded_rect(doc_row, 7, selection, rect);
    draw_home_text(
        fb,
        fonts,
        x + 12,
        actions_top - 22,
        &home_fit_text(fonts, &data.document_label, 820, inner - 24),
        820,
        ink,
        rect,
        true,
    );

    for (baseline, label, hit) in [
        (actions_top + 18, "History", DocumentChromeHit::History),
        (actions_top + 48, "Security", DocumentChromeHit::Security),
        (
            actions_top + 78,
            "Shortcuts  Ctrl /",
            DocumentChromeHit::Help,
        ),
    ] {
        let row = Rect::new(x, baseline - 20, inner, 28);
        draw_home_text(fb, fonts, x + 8, baseline, label, 760, ink, rect, false);
        hits.push((row, hit));
    }
    hits
}

/// Breadcrumb row above the page: library \u{203A} folder \u{203A} file, plus a quiet search
/// affordance. It is built from the path's metadata only; the page itself stays under
/// [`PixelSurface`]. Inks follow the active paper theme (`D-101`).
fn render_document_breadcrumb(
    fb: &mut FrameBuffer,
    fonts: &FontSet,
    theme: PageTheme,
    rect: Rect,
    data: &DocumentChromeData,
) -> Vec<(Rect, DocumentChromeHit)> {
    let mut hits = Vec::new();
    if rect.w <= 0 || rect.h <= 0 {
        return hits;
    }
    let ink = ink_for(&theme);
    let muted = muted_ink_for(&theme);
    fb.fill_rect_clipped(rect, theme.bg_body, rect);
    fb.fill_rect_clipped(
        Rect::new(rect.x, rect.y + rect.h - 1, rect.w, 1),
        theme.line_divider,
        rect,
    );
    let mut x = rect.x + 20;
    let baseline = rect.y + rect.h / 2 + 6;
    let home_w = home_text_width(fonts, &data.library_label, 840).min(180);
    draw_home_text(
        fb,
        fonts,
        x,
        baseline,
        &home_fit_text(fonts, &data.library_label, 840, 180),
        840,
        muted,
        rect,
        false,
    );
    hits.push((
        Rect::new(x - 4, rect.y + 4, home_w + 8, rect.h - 8),
        DocumentChromeHit::Home,
    ));
    x += home_w + 12;

    if let Some(folder) = &data.folder_label {
        draw_home_text(fb, fonts, x, baseline, "\u{203A}", 840, muted, rect, false);
        x += 14;
        let folder_shown = home_fit_text(fonts, folder, 840, 180);
        draw_home_text(
            fb,
            fonts,
            x,
            baseline,
            &folder_shown,
            840,
            muted,
            rect,
            false,
        );
        x += home_text_width(fonts, &folder_shown, 840) + 12;
    }
    draw_home_text(fb, fonts, x, baseline, "\u{203A}", 840, muted, rect, false);
    x += 14;
    let space = (rect.x + rect.w - 108 - x).max(0);
    draw_home_text(
        fb,
        fonts,
        x,
        baseline,
        &home_fit_text(fonts, &data.document_label, 900, space),
        900,
        ink,
        rect,
        true,
    );

    let find = Rect::new(rect.x + rect.w - 64, rect.y + 6, 48, rect.h - 12);
    draw_home_text(
        fb,
        fonts,
        find.x + 10,
        baseline,
        "Find",
        800,
        muted,
        find,
        false,
    );
    hits.push((find, DocumentChromeHit::Find));
    hits
}

/// Static shortcut sheet. It holds no document text and no live state, so it cannot add a
/// plaintext exposure or an idle repaint; it is painted only on the frame that opens it.
///
/// `D-101`: the first layout placed seven rows in two columns of `268 px` inside a `560 px`
/// panel, so ten of the fourteen entries were painted outside the panel and clipped away entirely
/// (the user-visible sheet listed four shortcuts). The grid is now derived from the panel width
/// and the row count, so it cannot silently overflow again.
fn render_shortcut_help(fb: &mut FrameBuffer, fonts: &FontSet, theme: PageTheme) -> (Rect, Rect) {
    let w = i32::from(fb.width);
    let h = i32::from(fb.height);
    let full = Rect::new(0, 0, w, h);
    let ink = ink_for(&theme);
    let muted = muted_ink_for(&theme);
    let accent = accent_ink_for(&theme);

    let rows: [(&str, &str); 14] = [
        ("Ctrl Z / Y", "Undo / redo"),
        ("Ctrl A", "Select all"),
        ("Ctrl F", "Find and replace"),
        ("Ctrl K", "Edit link"),
        ("Ctrl S", "Save and witness"),
        ("Ctrl H", "Witness history"),
        ("Ctrl M", "Bookmark witness"),
        ("Ctrl G", "Security panel"),
        ("Ctrl L", "Emergency wipe"),
        ("Ctrl Shift D", "Light / dark paper"),
        ("Ctrl Shift H", "Return Home"),
        ("Ctrl Alt + / -", "Display scale"),
        ("Ctrl /", "This sheet"),
        ("Ctrl Q", "Save and quit"),
    ];

    fb.fill_rect_clipped(full, Rgba8::rgba_pct(0x00, 0x00, 0x00, 22), full);
    // A fixed `&'static` array of 14 entries can never exceed `i32`.
    let entry_count = i32::try_from(rows.len()).unwrap_or(i32::MAX);
    let (panel, cols, row_h) = shortcut_help_geometry(w, h, entry_count);
    fb.fill_rounded_rect(panel, 16, theme.bg_body, full);
    fb.stroke_rounded_rect(panel, 16, theme.line_border, full);
    draw_home_text(
        fb,
        fonts,
        panel.x + 24,
        panel.y + 38,
        "Keyboard shortcuts",
        1_020,
        ink,
        panel,
        true,
    );
    let col_w = (panel.w - 48) / cols.max(1);
    let per_col = ceil_div(entry_count, cols.max(1)).max(1);
    for (i, (key, desc)) in rows.iter().enumerate() {
        let i = i32::try_from(i).unwrap_or(0);
        let rx = panel.x + 24 + (i / per_col) * col_w;
        let ry = panel.y + 84 + (i % per_col) * row_h;
        draw_home_text(fb, fonts, rx, ry, key, 780, accent, panel, true);
        draw_home_text(fb, fonts, rx, ry + 15, desc, 760, muted, panel, false);
    }
    let close = Rect::new(panel.x + panel.w - 44, panel.y + 12, 28, 28);
    draw_home_text(
        fb,
        fonts,
        close.x + 8,
        close.y + 20,
        "\u{00D7}",
        940,
        muted,
        close,
        false,
    );
    (panel, close)
}

/// Integer ceiling division for positive operands (`div_ceil` is still unstable for signed
/// integers on the pinned toolchain).
fn ceil_div(numerator: i32, denominator: i32) -> i32 {
    let denominator = denominator.max(1);
    let numerator = numerator.max(0);
    (numerator + denominator - 1) / denominator
}

/// Panel and grid geometry for the shortcut sheet.
///
/// The original layout hard-coded seven `268 px` columns inside a `560 px` panel, so ten of the
/// fourteen entries were painted outside the panel and clipped (`D-101`). Deriving the grid from
/// the panel width means a layout change cannot silently drop entries again.
fn shortcut_help_geometry(window_w: i32, window_h: i32, entries: i32) -> (Rect, i32, i32) {
    const ROW_H: i32 = 34;
    const HEADER_H: i32 = 84;
    const FOOTER_H: i32 = 24;
    let panel_w = 560.min((window_w - 32).max(280));
    // Two columns need ~250 px each for a key plus its description; below that, one column.
    let cols = if panel_w >= 520 { 2 } else { 1 };
    let per_col = ceil_div(entries, cols).max(1);
    let panel_h = (HEADER_H + per_col * ROW_H + FOOTER_H).min(window_h - 32);
    let panel = Rect::new(
        (window_w - panel_w).max(16) / 2,
        (window_h - panel_h).max(16) / 2,
        panel_w,
        panel_h,
    );
    (panel, cols, ROW_H)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HomeHit {
    Home,
    Search,
    NewDocument,
    Folder(usize),
    Document(usize),
    Back,
    Favorites,
    MyDocuments,
    ManualOrder,
    ListView,
    Settings,
    CloseOverlay,
}

#[derive(Debug, Clone)]
struct HomeEntry {
    name: String,
    folder: String,
    path: PathBuf,
}

#[derive(Debug, Clone)]
struct HomeFolder {
    name: String,
    entries: Vec<usize>,
}

/// Root of the Home library. `HOLONOMY_NOTES_DIR` lets a user point the shell at a synced folder;
/// otherwise the process's working directory is used, so no hidden database is introduced.
fn home_library_root() -> PathBuf {
    std::env::var_os("HOLONOMY_NOTES_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

/// Header label for the library root. A directory name is public metadata (never document
/// plaintext), so it can be painted without touching a sealed segment.
fn home_library_label() -> String {
    home_library_root()
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| !n.is_empty())
        .unwrap_or("My documents")
        .to_owned()
}

/// Suggests a non-existing filename inside the Home library so pressing Enter in a new-document
/// prompt creates a document rather than silently reopening an existing one.
fn home_new_document_path(root: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut candidate = root.join(format!("notes-{stamp}.wavefunction"));
    for suffix in 1..100u32 {
        if !candidate.exists() {
            break;
        }
        candidate = root.join(format!("notes-{stamp}-{suffix}.wavefunction"));
    }
    candidate
}

fn enumerate_home_entries(root: &Path) -> Vec<HomeEntry> {
    fn walk(dir: &Path, root: &Path, depth: usize, out: &mut Vec<HomeEntry>) {
        if depth > 3 || out.len() >= 240 {
            return;
        }
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        let mut paths: Vec<PathBuf> = read
            .filter_map(|item| item.ok().map(|e| e.path()))
            .collect();
        paths.sort();
        for path in paths {
            let hidden = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'));
            if hidden {
                continue;
            }
            if path.is_dir() {
                walk(&path, root, depth + 1, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("wavefunction") {
                let name = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Untitled".to_owned());
                let folder = path
                    .strip_prefix(root)
                    .ok()
                    .and_then(|p| p.parent())
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(|p| {
                        p.to_string_lossy()
                            .replace(std::path::MAIN_SEPARATOR, " / ")
                    })
                    .unwrap_or_else(|| "My documents".to_owned());
                out.push(HomeEntry { name, folder, path });
            }
        }
    }

    let mut entries = Vec::new();
    walk(root, root, 0, &mut entries);
    entries.sort_by(|a, b| a.folder.cmp(&b.folder).then(a.name.cmp(&b.name)));
    entries
}

fn group_home_folders(entries: &[HomeEntry]) -> Vec<HomeFolder> {
    let mut folders: Vec<HomeFolder> = Vec::new();
    for (idx, entry) in entries.iter().enumerate() {
        if let Some(folder) = folders.iter_mut().find(|f| f.name == entry.folder) {
            folder.entries.push(idx);
        } else {
            folders.push(HomeFolder {
                name: entry.folder.clone(),
                entries: vec![idx],
            });
        }
    }
    folders
}

#[derive(Debug)]
struct HomeState {
    entries: Vec<HomeEntry>,
    folders: Vec<HomeFolder>,
    active_folder: Option<usize>,
    query: String,
    search_focused: bool,
    favorites: bool,
    manual_order: bool,
    list_view: bool,
    settings_open: bool,
}

impl HomeState {
    /// Opens Home with a folder already selected — the writing-view sidebar hands back a
    /// folder *name* (metadata), never a document or a plaintext snippet.
    fn with_folder(initial_folder: Option<&str>) -> Self {
        let root = home_library_root();
        let entries = enumerate_home_entries(&root);
        let folders = group_home_folders(&entries);
        let active_folder = initial_folder
            .and_then(|wanted| folders.iter().position(|folder| folder.name == wanted));
        Self {
            entries,
            folders,
            active_folder,
            query: String::new(),
            search_focused: false,
            favorites: false,
            manual_order: false,
            list_view: false,
            settings_open: false,
        }
    }

    fn matching_entries(&self) -> Vec<usize> {
        let needle = self.query.trim().to_lowercase();
        let mut hits: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                if self.favorites {
                    return false;
                }
                if let Some(folder) = self.active_folder {
                    if entry.folder != self.folders[folder].name {
                        return false;
                    }
                }
                needle.is_empty()
                    || entry.name.to_lowercase().contains(&needle)
                    || entry.folder.to_lowercase().contains(&needle)
            })
            .map(|(idx, _)| idx)
            .collect();
        if self.manual_order {
            hits.reverse();
        }
        hits
    }
}

fn home_fit_text(fonts: &FontSet, text: &str, milli: i64, max_width: i32) -> String {
    if home_text_width(fonts, text, milli) <= max_width {
        return text.to_owned();
    }
    let mut out = String::new();
    for ch in text.chars() {
        let next = format!("{out}{ch}");
        if home_text_width(fonts, &next, milli) > max_width.saturating_sub(8) {
            break;
        }
        out = next;
    }
    out.push('\u{2026}');
    out
}

fn home_text_width(fonts: &FontSet, text: &str, milli: i64) -> i32 {
    (fonts.advance(text, Style::at(milli)).sixtyfourths() >> 6) as i32
}

#[allow(clippy::too_many_arguments)]
fn draw_home_text(
    fb: &mut FrameBuffer,
    fonts: &FontSet,
    x: i32,
    baseline: i32,
    text: &str,
    milli: i64,
    color: Rgba8,
    clip: Rect,
    bold: bool,
) {
    let style = if bold {
        Style::at(milli).bold()
    } else {
        Style::at(milli)
    };
    draw_ui_text(fb, fonts, x, baseline, text, style, color, clip);
}

fn draw_home_folder_icon(fb: &mut FrameBuffer, x: i32, y: i32, color: Rgba8, clip: Rect) {
    fb.fill_rect_clipped(Rect::new(x + 1, y + 2, 8, 4), color, clip);
    fb.fill_rounded_rect(Rect::new(x, y + 4, 14, 9), 2, color, clip);
}

/// Draws the Home shell into a logical-pixel framebuffer and returns its click targets. Every
/// surface paired with [`crate::platform::DesktopWindow`] pointer coordinates, so scaling stays
/// consistent with the editor.
fn render_home(
    fb: &mut FrameBuffer,
    fonts: &FontSet,
    state: &HomeState,
    scale_percent: u16,
) -> Vec<(Rect, HomeHit)> {
    let w = i32::from(fb.width);
    let h = i32::from(fb.height);
    let full = Rect::new(0, 0, w, h);
    let sidebar = Rect::new(0, 0, HOME_SIDEBAR_W.min(w / 3), h);
    let content_x = sidebar.w;
    let content = Rect::new(content_x, 0, w - content_x, h);
    let mut hits = Vec::new();

    fb.fill_rect_clipped(full, HOME_CANVAS_BG, full);
    fb.fill_vertical_gradient(sidebar, HOME_SIDE_TOP, HOME_SIDE_BOTTOM, full);
    fb.fill_rect_clipped(Rect::new(sidebar.w - 1, 0, 1, h), HOME_FAINT, full);

    // Sidebar: Home, library folders, and a settings row pinned to the bottom.
    let side_pad = 16;
    let side_w = sidebar.w - 2 * side_pad;
    let mut sy = 30;
    let home_row = Rect::new(side_pad, sy - 8, side_w, 36);
    if !state.favorites && state.active_folder.is_none() {
        fb.fill_rounded_rect(home_row, 8, HOME_SELECTION, full);
    }
    draw_home_folder_icon(fb, side_pad + 12, sy, HOME_INK, full);
    draw_home_text(
        fb,
        fonts,
        side_pad + 36,
        sy + 17,
        "Home",
        920,
        HOME_INK,
        full,
        false,
    );
    hits.push((home_row, HomeHit::Home));
    sy += 62;

    let lib_row = Rect::new(side_pad, sy - 8, side_w, 36);
    fb.fill_rounded_rect(lib_row, 8, HOME_SELECTION, full);
    draw_home_text(
        fb,
        fonts,
        side_pad + 12,
        sy + 17,
        "My documents",
        880,
        HOME_INK,
        full,
        false,
    );
    // Push the row before the `+` so rev-order hit resolution reaches the smaller target.
    hits.push((lib_row, HomeHit::MyDocuments));
    let plus_x = sidebar.w - side_pad - 22;
    draw_home_text(
        fb,
        fonts,
        plus_x,
        sy + 17,
        "+",
        1_040,
        HOME_INK,
        full,
        false,
    );
    hits.push((Rect::new(plus_x - 4, sy - 6, 26, 28), HomeHit::NewDocument));
    sy += 42;

    for (idx, folder) in state.folders.iter().enumerate().take(14) {
        if sy + 30 > h - 64 {
            break;
        }
        if Some(idx) == state.active_folder {
            fb.fill_rounded_rect(
                Rect::new(side_pad, sy - 7, side_w, 30),
                7,
                HOME_SELECTION,
                full,
            );
        }
        draw_home_folder_icon(fb, side_pad + 14, sy - 2, HOME_MUTED, full);
        let label = home_fit_text(fonts, &folder.name, 840, side_w - 48);
        draw_home_text(
            fb,
            fonts,
            side_pad + 38,
            sy + 14,
            &label,
            840,
            HOME_INK,
            full,
            false,
        );
        hits.push((
            Rect::new(side_pad, sy - 8, side_w, 30),
            HomeHit::Folder(idx),
        ));
        sy += 32;
    }

    let settings = Rect::new(side_pad, h - 52, side_w, 34);
    fb.fill_rounded_rect(settings, 8, HOME_SELECTION, full);
    draw_home_text(
        fb,
        fonts,
        side_pad + 12,
        h - 30,
        "Settings",
        860,
        HOME_MUTED,
        full,
        false,
    );
    hits.push((settings, HomeHit::Settings));

    // Header: quiet.md-style library breadcrumb, search field, quick actions.
    let header_x = content.x + 32;
    let lib_label = home_library_label();
    let breadcrumb = match state.active_folder.and_then(|idx| state.folders.get(idx)) {
        Some(folder) => format!("{lib_label} \u{203A} {}", folder.name),
        None => lib_label.clone(),
    };
    draw_home_text(
        fb,
        fonts,
        header_x,
        46,
        &home_fit_text(fonts, &breadcrumb, 1_000, (content.w - 480).max(160)),
        1_000,
        HOME_INK,
        content,
        true,
    );
    let search_w = (content.w - 420).clamp(180, 380);
    let search_x = content.x + (content.w - search_w) / 2;
    let search_rect = Rect::new(search_x, 20, search_w, 36);
    fb.fill_rounded_rect(search_rect, 10, HOME_SURFACE, content);
    fb.stroke_rounded_rect(search_rect, 10, HOME_FAINT, content);
    let search_text = if state.query.is_empty() {
        "Search documents\u{2026}".to_owned()
    } else {
        state.query.clone()
    };
    draw_home_text(
        fb,
        fonts,
        search_x + 14,
        43,
        &search_text,
        880,
        if state.query.is_empty() {
            HOME_MUTED
        } else {
            HOME_INK
        },
        search_rect,
        false,
    );
    if state.search_focused {
        fb.fill_rect_clipped(
            Rect::new(
                search_x + 14 + home_text_width(fonts, &search_text, 880),
                27,
                1,
                18,
            ),
            HOME_ACCENT,
            search_rect,
        );
    }
    let shortcut = if cfg!(target_os = "macos") {
        "\u{2318} J"
    } else {
        "Ctrl J"
    };
    draw_home_text(
        fb,
        fonts,
        search_x + search_w - home_text_width(fonts, shortcut, 760) - 12,
        43,
        shortcut,
        760,
        HOME_MUTED,
        search_rect,
        false,
    );
    hits.push((search_rect, HomeHit::Search));

    let action_new = Rect::new(content.x + content.w - 56, 20, 32, 32);
    fb.fill_rounded_rect(action_new, 8, HOME_SURFACE, content);
    fb.stroke_rounded_rect(action_new, 8, HOME_FAINT, content);
    draw_home_text(
        fb,
        fonts,
        action_new.x + 9,
        43,
        "+",
        1_080,
        HOME_INK,
        action_new,
        false,
    );
    hits.push((action_new, HomeHit::NewDocument));

    if content.w >= 480 {
        let action_menu = Rect::new(content.x + content.w - 100, 20, 32, 32);
        fb.fill_rounded_rect(action_menu, 8, HOME_SURFACE, content);
        fb.stroke_rounded_rect(action_menu, 8, HOME_FAINT, content);
        draw_home_text(
            fb,
            fonts,
            action_menu.x + 10,
            43,
            "...",
            900,
            HOME_MUTED,
            action_menu,
            false,
        );
        hits.push((action_menu, HomeHit::Settings));
    }

    // Segmented tabs and library controls.
    let tabs_y = 84;
    let my_tab = Rect::new(header_x, tabs_y, 150, 32);
    let fav_tab = Rect::new(my_tab.x + my_tab.w + 8, tabs_y, 112, 32);
    for (rect, active) in [(my_tab, !state.favorites), (fav_tab, state.favorites)] {
        fb.fill_rounded_rect(
            rect,
            9,
            if active { HOME_WASH } else { HOME_CANVAS_BG },
            content,
        );
        if active {
            fb.stroke_rounded_rect(rect, 9, HOME_ACCENT, content);
        }
    }
    draw_home_text(
        fb,
        fonts,
        my_tab.x + 16,
        tabs_y + 22,
        "My documents",
        840,
        HOME_INK,
        my_tab,
        !state.favorites,
    );
    draw_home_text(
        fb,
        fonts,
        fav_tab.x + 16,
        tabs_y + 22,
        "Favorites",
        840,
        HOME_MUTED,
        fav_tab,
        false,
    );
    hits.push((my_tab, HomeHit::MyDocuments));
    hits.push((fav_tab, HomeHit::Favorites));

    let controls_y = tabs_y + 48;
    let order = Rect::new(content.x + content.w - 236, controls_y, 116, 28);
    let view_toggle = Rect::new(content.x + content.w - 108, controls_y, 76, 28);
    fb.fill_rounded_rect(order, 7, HOME_WASH, content);
    fb.fill_rounded_rect(view_toggle, 7, HOME_WASH, content);
    draw_home_text(
        fb,
        fonts,
        order.x + 10,
        controls_y + 19,
        if state.manual_order {
            "Manual order"
        } else {
            "Name order"
        },
        780,
        HOME_INK,
        order,
        false,
    );
    draw_home_text(
        fb,
        fonts,
        view_toggle.x + 12,
        controls_y + 19,
        if state.list_view { "List" } else { "Grid" },
        780,
        HOME_INK,
        view_toggle,
        false,
    );
    hits.push((order, HomeHit::ManualOrder));
    hits.push((view_toggle, HomeHit::ListView));
    draw_home_text(
        fb,
        fonts,
        header_x,
        controls_y + 19,
        if state.active_folder.is_some() {
            "Documents"
        } else {
            "Manage"
        },
        800,
        HOME_MUTED,
        content,
        false,
    );
    if state.active_folder.is_some() {
        let back = Rect::new(header_x + 76, controls_y - 2, 56, 26);
        fb.fill_rounded_rect(back, 7, HOME_WASH, content);
        draw_home_text(
            fb,
            fonts,
            back.x + 12,
            controls_y + 17,
            "Back",
            780,
            HOME_INK,
            back,
            false,
        );
        hits.push((back, HomeHit::Back));
    }

    let grid_y = controls_y + 42;
    let hits_end = render_home_cards(fb, fonts, state, grid_y, content);
    hits.extend(hits_end);

    if state.settings_open {
        let overlay = Rect::new(
            content.x + (content.w - 440).max(20) / 2,
            150,
            440.min(content.w - 40),
            248,
        );
        fb.fill_rounded_rect(overlay, 14, HOME_SURFACE, full);
        fb.stroke_rounded_rect(overlay, 14, HOME_FAINT, full);
        draw_home_text(
            fb,
            fonts,
            overlay.x + 22,
            overlay.y + 38,
            "Settings",
            1_020,
            HOME_INK,
            overlay,
            true,
        );
        let scale_line = format!("Display scale {scale_percent}% (Ctrl Alt + / -)");
        for (row, line) in [
            "Ctrl J \u{2014} focus search",
            "Ctrl Shift H \u{2014} return Home from a document",
            "HOLONOMY_NOTES_DIR \u{2014} library folder",
            scale_line.as_str(),
        ]
        .iter()
        .enumerate()
        {
            draw_home_text(
                fb,
                fonts,
                overlay.x + 22,
                overlay.y + 78 + row as i32 * 30,
                line,
                820,
                HOME_MUTED,
                overlay,
                false,
            );
        }
        let close = Rect::new(overlay.x + overlay.w - 40, overlay.y + 12, 28, 28);
        draw_home_text(
            fb,
            fonts,
            close.x + 8,
            close.y + 20,
            "\u{00D7}",
            940,
            HOME_MUTED,
            close,
            false,
        );
        hits.push((close, HomeHit::CloseOverlay));
    }

    hits
}

fn render_home_cards(
    fb: &mut FrameBuffer,
    fonts: &FontSet,
    state: &HomeState,
    top: i32,
    content: Rect,
) -> Vec<(Rect, HomeHit)> {
    let mut hits = Vec::new();
    let pad = 32;
    let available = content.w - 2 * pad;
    if state.favorites || (state.query.is_empty() && state.folders.is_empty()) {
        let card = Rect::new(content.x + pad, top, available.min(560), 176);
        fb.fill_rounded_rect(card, 14, HOME_SURFACE, content);
        fb.stroke_rounded_rect(card, 14, HOME_TAB, content);
        draw_home_text(
            fb,
            fonts,
            card.x + 24,
            card.y + 46,
            "No documents yet",
            980,
            HOME_INK,
            card,
            true,
        );
        draw_home_text(
            fb,
            fonts,
            card.x + 24,
            card.y + 78,
            "Create an encrypted document to start your library.",
            840,
            HOME_MUTED,
            card,
            false,
        );
        let button = Rect::new(card.x + 24, card.y + 108, 156, 36);
        fb.fill_rounded_rect(button, 9, HOME_ACCENT, card);
        draw_home_text(
            fb,
            fonts,
            button.x + 20,
            button.y + 24,
            "+ New document",
            860,
            HOME_SURFACE,
            button,
            false,
        );
        hits.push((button, HomeHit::NewDocument));
        return hits;
    }

    let matching = state.matching_entries();
    if !state.query.is_empty() {
        return render_home_document_cards(fb, fonts, state, &matching, top, content);
    }
    if let Some(idx) = state.active_folder {
        if let Some(folder) = state.folders.get(idx) {
            let mut visible: Vec<usize> = folder.entries.clone();
            if state.manual_order {
                visible.reverse();
            }
            return render_home_document_cards(fb, fonts, state, &visible, top, content);
        }
    }

    let cols = ((available + HOME_GAP) / (HOME_CARD_MIN_W + HOME_GAP)).clamp(1, 4);
    let card_w = (available - HOME_GAP * (cols - 1)) / cols;
    for (i, folder) in state.folders.iter().enumerate() {
        let col = i % cols as usize;
        let row = i / cols as usize;
        let x = content.x + pad + col as i32 * (card_w + HOME_GAP);
        let y = top + row as i32 * (HOME_CARD_H + HOME_GAP);
        if y + HOME_CARD_H > content.y + content.h || x + card_w > content.x + content.w {
            break;
        }
        let card = Rect::new(x, y, card_w, HOME_CARD_H);
        fb.fill_rounded_rect(card, 14, HOME_SURFACE, content);
        fb.stroke_rounded_rect(card, 14, HOME_TAB, content);
        // A quiet.md folder card is a soft grey folder behind a stack of paper. The preview
        // lines are seeded from the folder name and entry index — never from document bytes.
        fb.fill_rounded_rect(
            Rect::new(x + 13, y + 16, card_w - 26, 122),
            10,
            HOME_CARD,
            card,
        );
        fb.fill_rounded_rect(
            Rect::new(x + 13, y + 16, (card_w - 26) / 2, 22),
            8,
            HOME_CARD,
            card,
        );
        for layer in (0..3).rev() {
            let page = Rect::new(
                x + 20 + layer * 6,
                y + 24 - layer * 5,
                card_w - 48 - layer * 12,
                104,
            );
            fb.fill_rounded_rect(page, 7, HOME_SURFACE, card);
            fb.stroke_rounded_rect(page, 7, HOME_FAINT, card);
        }
        let preview = Rect::new(x + 26, y + 30, card_w - 60, 82);
        let seed = folder.entries.first().copied().unwrap_or(0) + folder.name.len();
        for line in 0..5i32 {
            let width = ((seed * 17 + line as usize * 29) % 61 + 28) as i32;
            fb.fill_rounded_rect(
                Rect::new(
                    preview.x + 4 + (line % 3) * 3,
                    preview.y + 10 + line * 13,
                    (preview.w - 16).min(width * preview.w / 100),
                    3,
                ),
                1,
                if line == 0 { HOME_MUTED } else { HOME_TAB },
                preview,
            );
        }
        fb.fill_rounded_rect(
            Rect::new(preview.x + 4, preview.y + preview.h - 28, preview.w / 3, 20),
            3,
            HOME_TAB,
            preview,
        );
        let label = home_fit_text(fonts, &folder.name, 900, card_w - 28);
        draw_home_text(
            fb,
            fonts,
            x + 16,
            y + 154,
            &label,
            900,
            HOME_INK,
            card,
            true,
        );
        let count = format!(
            "{} item{}",
            folder.entries.len(),
            if folder.entries.len() == 1 { "" } else { "s" }
        );
        draw_home_text(
            fb,
            fonts,
            x + 16,
            y + 178,
            &count,
            800,
            HOME_MUTED,
            card,
            false,
        );
        hits.push((card, HomeHit::Folder(i)));
    }
    hits
}

fn render_home_document_cards(
    fb: &mut FrameBuffer,
    fonts: &FontSet,
    state: &HomeState,
    entries: &[usize],
    top: i32,
    content: Rect,
) -> Vec<(Rect, HomeHit)> {
    let mut hits = Vec::new();
    let pad = 32;
    let available = content.w - 2 * pad;
    if entries.is_empty() {
        draw_home_text(
            fb,
            fonts,
            content.x + pad,
            top + 40,
            "No matching documents",
            920,
            HOME_MUTED,
            content,
            false,
        );
        return hits;
    }
    if state.list_view {
        let row_h = 54;
        for (row, &idx) in entries.iter().enumerate().take(8) {
            let y = top + row as i32 * row_h;
            let card = Rect::new(content.x + pad, y, available.min(720), row_h - 8);
            fb.fill_rounded_rect(card, 9, HOME_SURFACE, content);
            fb.stroke_rounded_rect(card, 9, HOME_FAINT, content);
            draw_home_folder_icon(fb, card.x + 14, card.y + 12, HOME_ACCENT, card);
            draw_home_text(
                fb,
                fonts,
                card.x + 42,
                card.y + 30,
                &home_fit_text(fonts, &state.entries[idx].name, 880, card.w - 60),
                880,
                HOME_INK,
                card,
                false,
            );
            draw_home_text(
                fb,
                fonts,
                card.x + card.w - 118,
                card.y + 30,
                "Open",
                800,
                HOME_ACCENT,
                card,
                false,
            );
            hits.push((card, HomeHit::Document(idx)));
        }
        return hits;
    }
    let cols = ((available + HOME_GAP) / (HOME_CARD_MIN_W + HOME_GAP)).clamp(1, 4);
    let card_w = (available - HOME_GAP * (cols - 1)) / cols;
    for (i, &idx) in entries.iter().enumerate().take(12) {
        let col = i % cols as usize;
        let row = i / cols as usize;
        let x = content.x + pad + col as i32 * (card_w + HOME_GAP);
        let y = top + row as i32 * (168 + HOME_GAP);
        if y + 168 > content.y + content.h || x + card_w > content.x + content.w {
            break;
        }
        let card = Rect::new(x, y, card_w, 168);
        fb.fill_rounded_rect(card, 14, HOME_SURFACE, content);
        fb.stroke_rounded_rect(card, 14, HOME_TAB, content);
        fb.fill_rounded_rect(
            Rect::new(x + 14, y + 14, card_w - 28, 88),
            8,
            HOME_CARD,
            card,
        );
        let preview = Rect::new(x + 22, y + 22, card_w - 44, 72);
        for line in 0..4i32 {
            fb.fill_rounded_rect(
                Rect::new(
                    preview.x + 4,
                    preview.y + 10 + line * 15,
                    (preview.w - 12) * (60 + line * 8) / 100,
                    3,
                ),
                1,
                if line == 0 { HOME_FAINT } else { HOME_TAB },
                preview,
            );
        }
        draw_home_text(
            fb,
            fonts,
            x + 16,
            y + 128,
            &home_fit_text(fonts, &state.entries[idx].name, 900, card_w - 28),
            900,
            HOME_INK,
            card,
            true,
        );
        draw_home_text(
            fb,
            fonts,
            x + 16,
            y + 152,
            &home_fit_text(fonts, &state.entries[idx].folder, 780, card_w - 28),
            780,
            HOME_MUTED,
            card,
            false,
        );
        hits.push((card, HomeHit::Document(idx)));
    }
    hits
}

/// Shows the library and returns the document the user chose, already unlocked by the in-window
/// passphrase prompt. Returning `None` means the native window was closed.
fn show_home(
    win: &mut ScaledWindow,
    fonts: &FontSet,
    initial_folder: Option<&str>,
) -> Result<Option<(PathBuf, Document)>, Box<dyn std::error::Error>> {
    let mut state = HomeState::with_folder(initial_folder);
    let (mut w, mut h) = win.size();
    let mut dirty = true;
    let mut hits = Vec::new();

    loop {
        if dirty {
            let mut fb = FrameBuffer::new(w, h, HOME_CANVAS_BG);
            hits = render_home(&mut fb, fonts, &state, win.scale().percent());
            win.present_argb(&fb.pixels, w, h)?;
            dirty = false;
        }

        for event in win.poll_events(100)? {
            match event {
                DesktopEvent::CloseRequested => return Ok(None),
                DesktopEvent::Resize { width, height } => {
                    w = width;
                    h = height;
                    dirty = true;
                }
                DesktopEvent::MouseDown { x, y, .. } => {
                    let px = i32::from(x);
                    let py = i32::from(y);
                    state.search_focused = false;
                    if let Some((_, hit)) = hits.iter().rev().find(|(r, _)| r.contains(px, py)) {
                        match *hit {
                            HomeHit::NewDocument => {
                                let default_path = home_new_document_path(&home_library_root());
                                if let Some(chosen) = prompt_gui_document(
                                    win.as_window_mut(),
                                    fonts,
                                    None,
                                    Some(&default_path),
                                )? {
                                    return Ok(Some(chosen));
                                }
                            }
                            HomeHit::Document(idx) => {
                                if let Some(entry) = state.entries.get(idx) {
                                    let path = entry.path.clone();
                                    if let Some(chosen) = prompt_gui_document(
                                        win.as_window_mut(),
                                        fonts,
                                        Some(&path),
                                        None,
                                    )? {
                                        return Ok(Some(chosen));
                                    }
                                }
                            }
                            HomeHit::Folder(idx) => {
                                state.active_folder = Some(idx);
                                state.favorites = false;
                            }
                            HomeHit::Home | HomeHit::MyDocuments => {
                                state.active_folder = None;
                                state.favorites = false;
                                state.query.clear();
                            }
                            HomeHit::Back => state.active_folder = None,
                            HomeHit::Favorites => {
                                state.favorites = true;
                                state.active_folder = None;
                            }
                            HomeHit::Search => state.search_focused = true,
                            HomeHit::ManualOrder => state.manual_order = !state.manual_order,
                            HomeHit::ListView => state.list_view = !state.list_view,
                            HomeHit::Settings => state.settings_open = true,
                            HomeHit::CloseOverlay => state.settings_open = false,
                        }
                    }
                    dirty = true;
                }
                DesktopEvent::Key { key, modifiers } => {
                    if modifiers.control && modifiers.alt {
                        let steps = match key {
                            Key::Character('=' | '+') => Some(1),
                            Key::Character('-') => Some(-1),
                            Key::Character('0') => Some(0),
                            _ => None,
                        };
                        if let Some(steps) = steps {
                            let mut status = String::new();
                            apply_display_scale(win, steps, &mut status);
                            dirty = true;
                            continue;
                        }
                    }
                    if modifiers.control && !modifiers.alt && matches!(key, Key::Character('q')) {
                        return Ok(None);
                    }
                    if modifiers.control && !modifiers.alt && matches!(key, Key::Character('j')) {
                        state.search_focused = true;
                        dirty = true;
                        continue;
                    }
                    match key {
                        Key::Escape => {
                            if state.settings_open {
                                state.settings_open = false;
                            } else if state.search_focused || !state.query.is_empty() {
                                state.search_focused = false;
                                state.query.clear();
                            } else {
                                state.active_folder = None;
                            }
                        }
                        Key::Backspace if state.search_focused => {
                            state.query.pop();
                        }
                        Key::Enter if state.search_focused => {
                            if let Some(&idx) = state.matching_entries().first() {
                                let path = state.entries[idx].path.clone();
                                if let Some(chosen) = prompt_gui_document(
                                    win.as_window_mut(),
                                    fonts,
                                    Some(&path),
                                    None,
                                )? {
                                    return Ok(Some(chosen));
                                }
                            }
                        }
                        Key::Character(ch)
                            if state.search_focused
                                && !modifiers.control
                                && !modifiers.alt
                                && state.query.len() < 64 =>
                        {
                            state.query.push(ch);
                        }
                        _ => {}
                    }
                    dirty = true;
                }
                _ => {}
            }
        }
    }
}

fn recent_paths_file() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(dir).join("holonomy").join("recent.txt"));
    }
    std::env::var_os("HOME")
        .filter(|s| !s.is_empty())
        .map(|h| {
            PathBuf::from(h)
                .join(".config")
                .join("holonomy")
                .join("recent.txt")
        })
        .or_else(|| {
            std::env::var_os("APPDATA")
                .filter(|s| !s.is_empty())
                .map(|a| PathBuf::from(a).join("holonomy").join("recent.txt"))
        })
}

fn load_opt_in_recent_paths() -> (Vec<String>, bool) {
    let Some(file) = recent_paths_file() else {
        return (Vec::new(), false);
    };
    if !file.exists() {
        return (Vec::new(), false);
    }
    let Ok(raw) = std::fs::read_to_string(&file) else {
        return (Vec::new(), true);
    };
    let mut paths = Vec::new();
    for line in raw.lines() {
        let t = line.trim();
        if !t.is_empty() && !paths.iter().any(|p| p == t) {
            paths.push(t.to_owned());
            if paths.len() >= 5 {
                break;
            }
        }
    }
    (paths, true)
}

fn sync_recent_paths(target: &Path, remember: bool) {
    let Some(file) = recent_paths_file() else {
        return;
    };
    if !remember {
        if file.exists() {
            let _ = std::fs::remove_file(file);
        }
        return;
    }
    let chosen = target.to_string_lossy().into_owned();
    let mut entries = vec![chosen];
    if let Ok(existing) = std::fs::read_to_string(&file) {
        for line in existing.lines() {
            let t = line.trim();
            if !t.is_empty() && !entries.iter().any(|e| e == t) {
                entries.push(t.to_owned());
                if entries.len() >= 5 {
                    break;
                }
            }
        }
    }
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(file, format!("{}\n", entries.join("\n")));
}

/// Interactive graphical passphrase / file-path prompt rendered right inside the window when
/// `holonomy-gui` is launched without a pre-supplied stdin passphrase (`UI-SPEC.md` §5.1–§5.2).
#[allow(clippy::too_many_lines)]
fn prompt_gui_document(
    x11: &mut dyn DesktopWindow,
    fonts: &FontSet,
    initial_path: Option<&Path>,
    default_path: Option<&Path>,
) -> Result<Option<(PathBuf, Document)>, Box<dyn std::error::Error>> {
    let (recent_paths, mut remember_recent) = load_opt_in_recent_paths();
    let mut path_buf = initial_path
        .or(default_path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "notes.wavefunction".to_owned());
    let mut editing_path = initial_path.is_none();
    let mut pass_buf = Zeroizing::new(Vec::<u8>::with_capacity(MAX_PASSPHRASE_INPUT_BYTES));
    let mut error_msg = String::new();
    let mut dirty = true;
    let mut recent_hits: Vec<(Rect, String)> = Vec::new();
    let mut remember_box = Rect::new(0, 0, 0, 0);

    loop {
        if dirty {
            let (w, h) = x11.size();
            let mut fb = FrameBuffer::new(w, h, HOME_CANVAS_BG);
            let clip = Rect::new(0, 0, i32::from(w), i32::from(h));
            let recents_rows = recent_paths.len().min(4) as i32;
            let card_w = i32::from(w).min(520) - 32;
            let card_h = 294
                + if recents_rows > 0 {
                    26 + recents_rows * 22
                } else {
                    0
                };
            let cx = (i32::from(w) - card_w) / 2;
            let cy = (i32::from(h) - card_h) / 2;
            let card = Rect::new(cx, cy, card_w, card_h);
            fb.fill_rounded_rect(card, 16, HOME_SURFACE, clip);
            fb.stroke_rounded_rect(card, 16, HOME_FAINT, clip);

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
                cy + 36,
                heading,
                Style::at(1_250).bold(),
                HOME_INK,
                card,
            );
            draw_ui_text(
                &mut fb,
                fonts,
                cx + 24,
                cy + 62,
                "Path:",
                Style::at(850),
                HOME_MUTED,
                card,
            );
            let path_box = Rect::new(cx + 24, cy + 70, card_w - 48, 30);
            fb.fill_rounded_rect(
                path_box,
                8,
                if editing_path {
                    HOME_SURFACE
                } else {
                    HOME_CARD
                },
                card,
            );
            fb.stroke_rounded_rect(
                path_box,
                8,
                if editing_path {
                    HOME_ACCENT
                } else {
                    HOME_FAINT
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
                HOME_INK,
                path_box,
            );

            draw_ui_text(
                &mut fb,
                fonts,
                cx + 24,
                cy + 124,
                "Passphrase (memory-scrubbed; Esc cancels):",
                Style::at(850),
                HOME_MUTED,
                card,
            );
            let pass_box = Rect::new(cx + 24, cy + 132, card_w - 48, 32);
            fb.fill_rounded_rect(pass_box, 8, HOME_SURFACE, card);
            fb.stroke_rounded_rect(
                pass_box,
                8,
                if !editing_path {
                    HOME_ACCENT
                } else {
                    HOME_FAINT
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
                HOME_INK,
                pass_box,
            );

            // Live passphrase strength meter (`UI-SPEC.md` §5.2).
            let meter_track = Rect::new(cx + 24, cy + 172, card_w - 48, 5);
            fb.fill_rounded_rect(meter_track, 2, HOME_CARD, card);
            let pass_text = std::str::from_utf8(&pass_buf).unwrap_or("");
            let strength = estimate_passphrase_strength(pass_text);
            let (tier_color, fill_pct) = match strength.tier {
                StrengthTier::Empty => (Rgba8::rgb(0xd9, 0x30, 0x25), 0),
                StrengthTier::Weak => (Rgba8::rgb(0xd9, 0x77, 0x06), 35),
                StrengthTier::Fair => (Rgba8::rgb(0xca, 0x8a, 0x04), 60),
                StrengthTier::Strong => (Rgba8::rgb(0x25, 0x63, 0xeb), 82),
                StrengthTier::VeryStrong => (Rgba8::rgb(0x16, 0xa3, 0x4a), 100),
            };
            if char_count > 0 {
                let fill_w = (meter_track.w * fill_pct / 100).max(8);
                fb.fill_rect_clipped(
                    Rect::new(meter_track.x, meter_track.y, fill_w, meter_track.h),
                    tier_color,
                    card,
                );
            }
            let meter_label = if char_count == 0 {
                format!(
                    "Argon2id {}-{} MiB (t=3, p=2)",
                    strength.kdf_min_mib, strength.kdf_max_mib
                )
            } else {
                format!(
                    "{} (~{} bits) \u{00B7} Argon2id {}-{} MiB, t=3, p=2",
                    strength.tier.label(),
                    strength.estimated_bits,
                    strength.kdf_min_mib,
                    strength.kdf_max_mib,
                )
            };
            draw_ui_text(
                &mut fb,
                fonts,
                cx + 24,
                cy + 194,
                &meter_label,
                Style::at(780),
                if char_count == 0 {
                    HOME_MUTED
                } else {
                    tier_color
                },
                card,
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
                cy + 218,
                &footer,
                Style::at(820),
                if error_msg.is_empty() {
                    HOME_MUTED
                } else {
                    SecurityDotState::Tripped.color()
                },
                card,
            );

            // Opt-in recent world-lines toggle (default OFF; plaintext path list only when opted in).
            remember_box = Rect::new(cx + 24, cy + 234, card_w - 48, 22);
            let check = if remember_recent { "[x]" } else { "[ ]" };
            let opt_label =
                format!("{check} Remember recent paths (opt-in local path metadata; default OFF)");
            draw_ui_text(
                &mut fb,
                fonts,
                remember_box.x,
                remember_box.y + 16,
                &opt_label,
                Style::at(780),
                HOME_MUTED,
                card,
            );

            recent_hits.clear();
            if recents_rows > 0 {
                let mut ry = cy + 266;
                draw_ui_text(
                    &mut fb,
                    fonts,
                    cx + 24,
                    ry + 12,
                    "Recent world-lines:",
                    Style::at(780).bold(),
                    HOME_MUTED,
                    card,
                );
                ry += 20;
                for recent in recent_paths.iter().take(4) {
                    let row = Rect::new(cx + 24, ry, card_w - 48, 20);
                    fb.fill_rounded_rect(row, 6, HOME_CARD, card);
                    draw_ui_text(
                        &mut fb,
                        fonts,
                        row.x + 8,
                        row.y + 15,
                        recent,
                        Style::at(800),
                        HOME_ACCENT,
                        row,
                    );
                    recent_hits.push((row, recent.clone()));
                    ry += 22;
                }
            }

            x11.present_argb(&fb.pixels, w, h)?;
            dirty = false;
        }

        for ev in x11.poll_events(100)? {
            match ev {
                DesktopEvent::CloseRequested => return Ok(None),
                DesktopEvent::Expose | DesktopEvent::Resize { .. } => dirty = true,
                DesktopEvent::MouseDown { x, y, .. } => {
                    let px = i32::from(x);
                    let py = i32::from(y);
                    if remember_box.contains(px, py) {
                        remember_recent = !remember_recent;
                        if !remember_recent {
                            if let Some(f) = recent_paths_file() {
                                let _ = std::fs::remove_file(f);
                            }
                        }
                        dirty = true;
                        continue;
                    }
                    if let Some((_, chosen)) = recent_hits.iter().find(|(r, _)| r.contains(px, py))
                    {
                        path_buf = chosen.clone();
                        editing_path = false;
                        error_msg.clear();
                        dirty = true;
                    }
                }
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
                                            Ok(doc) => {
                                                sync_recent_paths(&target, remember_recent);
                                                return Ok(Some((target, doc)));
                                            }
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
#[derive(Debug, PartialEq, Eq)]
pub enum DesktopEnd {
    /// Window closed normally with all pending edits saved.
    Saved,
    /// Document had no unsaved changes on close.
    NothingToWrite,
    /// User cancelled unlock/open dialog cleanly.
    Cancelled,
    /// User asked to return to the Home library; the session saved and closed cleanly.
    GoHome,
    /// Final save refused on exit.
    SaveFailed,
    /// Emergency reaper wiped plaintext on deadman / `Ctrl+L`.
    Wiped(GuiWipeReceipt),
    /// Emergency reaper tripped, but wipe could not be verified (`reason`, in-place bytes scrubbed).
    WipeUnconfirmed(GuiWipeUnconfirmed, usize),
}

/// Steps the whole-window display scale, persists the choice, and reports it in the status strip.
fn apply_display_scale(win: &mut ScaledWindow, steps: i32, status_msg: &mut String) {
    let next = if steps == 0 {
        UiScale::DEFAULT
    } else {
        win.scale().step(steps)
    };
    if win.set_scale(next) {
        save_ui_scale(next);
    }
    *status_msg = format!("display scale {}%", win.scale().percent());
}

/// Reflows editor and read-only history viewports to a new logical client size. Used by both
/// native resize events and a display-scale change so the first frame after a scale step already
/// has the correct framebuffer dimensions.
fn resize_editor_viewport(
    app: &mut App<store::DocumentSink, store::DocumentSource, FontSet, PixelSurface>,
    history_drawer: &mut Option<HistoryDrawerState>,
    width: u16,
    height: u16,
) {
    let view = app.editor().view();
    app.surface_mut().set_viewport(view, width, height);
    let (lw, lh) = app.surface().logical_editor_extent();
    let _ = app.resize(lw, lh);
    if let Some(ref mut h) = history_drawer {
        if let Some(ref mut hed) = h.history_editor {
            hed.set_view(view);
            hed.resize(lw, lh);
        }
    }
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
    run_desktop_with_window(
        ScaledWindow::new(win, load_ui_scale()),
        initial_path,
        deadman,
    )
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
    run_desktop_with_window(
        ScaledWindow::new(Box::new(x11), load_ui_scale()),
        initial_path,
        deadman,
    )
}

fn run_desktop_with_window(
    mut x11: ScaledWindow,
    initial_path: Option<&Path>,
    deadman: Option<Duration>,
) -> Result<DesktopEnd, Box<dyn std::error::Error>> {
    let fonts = FontSet::embedded(QUIET_MD_THEME.body_size_px)?;
    let mut preselected = initial_path.map(PathBuf::from);
    // Folder *name* selected in the writing-view sidebar, carried back to Home on return.
    let mut home_folder: Option<String> = None;

    loop {
        let (doc_path, document) = if let Some(ref path) = preselected {
            let Some(chosen) = prompt_gui_document(x11.as_window_mut(), &fonts, Some(path), None)?
            else {
                return Ok(DesktopEnd::Cancelled);
            };
            chosen
        } else {
            let Some(chosen) = show_home(&mut x11, &fonts, home_folder.as_deref())? else {
                return Ok(DesktopEnd::Cancelled);
            };
            chosen
        };
        preselected = None;
        home_folder = None;
        let end = run_document_session(
            &mut x11,
            fonts.clone(),
            doc_path,
            document,
            deadman,
            &mut home_folder,
        )?;
        if end == DesktopEnd::GoHome {
            continue;
        }
        return Ok(end);
    }
}

fn run_document_session(
    x11: &mut ScaledWindow,
    fonts: FontSet,
    doc_path: PathBuf,
    document: Document,
    deadman: Option<Duration>,
    home_folder: &mut Option<String>,
) -> Result<DesktopEnd, Box<dyn std::error::Error>> {
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

    // Folder names are metadata enumerated by Home; no document is opened or decrypted to
    // paint the rail, and no snippet of the open document leaves its resident set.
    let chrome_folders = {
        let entries = enumerate_home_entries(&home_library_root());
        group_home_folders(&entries)
            .into_iter()
            .map(|folder| folder.name)
            .collect()
    };
    let chrome_data = document_chrome_data(&doc_path, chrome_folders);

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
    let mut verified_at: Option<Instant> = None;
    let mut show_security = false;
    let mut overflow_open = false;
    let mut reduced_motion = false;
    let mut idle_chrome_visible = true;
    let mut recovery_banner_bytes = document.trailing_bytes();
    let mut recovery_dismiss_rect: Option<Rect> = None;
    let mut history_drawer: Option<HistoryDrawerState> = None;
    let mut history_hits: Vec<(Rect, HistoryButton)> = Vec::new();
    let mut find_bar: Option<FindState> = None;
    let mut link_bar: Option<String> = None;
    let mut toolbar_hits: Vec<ToolbarHit> = Vec::new();
    let mut sidebar_hits: Vec<(Rect, DocumentChromeHit)> = Vec::new();
    let mut breadcrumb_hits: Vec<(Rect, DocumentChromeHit)> = Vec::new();
    let mut help_close: Option<Rect> = None;
    let mut show_help = false;
    let mut status_msg = String::new();
    let mut ime_preedit = Zeroizing::new(String::new());
    let mut last_click: Option<(Instant, i16, i16, u8)> = None;

    let mut last_activity = Instant::now();
    let mut pending_paint = true;
    let mut quit = false;
    let mut home_requested = false;

    loop {
        if reaper.has_tripped() || tripwire.evaluate().is_some() {
            ime_preedit.zeroize();
            ime_preedit.clear();
            return Ok(finish_gui_wipe(
                &mut app,
                &document,
                &arenas,
                reaper,
                x11.as_window_mut(),
            ));
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

            let theme = app.surface().theme();
            let chrome_fonts = app.surface().fonts().clone();
            let (cw, ch) = app.surface().window_size();
            let bar_h = i32::from(theme.toolbar_height_px);
            let (sw, sh) = {
                let surface = app.surface();
                (surface.sidebar_width(), surface.breadcrumb_height())
            };
            if sw > 0 {
                let fb = app.surface_mut().framebuffer_mut();
                sidebar_hits = render_document_sidebar(
                    fb,
                    &chrome_fonts,
                    theme,
                    Rect::new(0, bar_h, sw, i32::from(ch) - bar_h - STATUS_BAR_HEIGHT_PX),
                    &chrome_data,
                );
                breadcrumb_hits = render_document_breadcrumb(
                    fb,
                    &chrome_fonts,
                    theme,
                    Rect::new(sw, bar_h, i32::from(cw) - sw, sh),
                    &chrome_data,
                );
            } else {
                sidebar_hits.clear();
                breadcrumb_hits.clear();
            }
            let metrics = app.editor().status_metrics();
            if history_drawer.is_none() && (idle_chrome_visible || reduced_motion) {
                app.surface_mut().render_scrollbar(metrics);
            }
            if history_drawer.is_none() && !ime_preedit.is_empty() {
                app.surface_mut().render_ime_preedit(ime_preedit.as_str());
            }

            let dot_state = SecurityDotState::from_cap(lock_cap, reaper.has_tripped());
            let read_only = history_drawer.is_some();
            toolbar_hits = render_toolbar_with(
                &mut app,
                dot_state,
                read_only,
                read_only,
                overflow_open,
                reduced_motion,
                x11.scale().percent(),
            );
            recovery_dismiss_rect = if history_drawer.is_none() {
                render_recovery_banner(
                    app.surface_mut(),
                    recovery_banner_bytes,
                    i32::from(theme.toolbar_height_px),
                )
            } else {
                None
            };

            let save_tag = if app.has_pending() {
                "unsaved"
            } else {
                "saved"
            };
            let name = doc_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("document");
            let approx = if metrics.words_exact { "" } else { "~" };
            let words_label = if metrics.total_words == 1 && metrics.words_exact {
                "1 word".to_owned()
            } else {
                format!("{approx}{} words", metrics.total_words)
            };
            let note_tail = if status_msg.is_empty() {
                String::new()
            } else {
                format!(" \u{00B7} {status_msg}")
            };
            let left = if let Some(ref h) = history_drawer {
                format!(
                    "{name} \u{00B7} HISTORY READ-ONLY \u{00B7} {}",
                    h.diff_summary
                )
            } else {
                format!(
                    "{name} \u{00B7} {words_label} \u{00B7} Page {}/{} \u{00B7} {save_tag}{note_tail}",
                    metrics.current_page, metrics.total_pages
                )
            };
            let right = Facts::gather_with(app.residency(), &document, lock_cap, verified_segments)
                .with_cooperative_reaper(&arenas, &canaries, reaper.has_tripped())
                .with_window_security(
                    x11.security_info(),
                    verified_at.map(|t| t.elapsed().as_secs()),
                    false,
                )
                .resident_line();

            if let Some(find) = &find_bar {
                let surface = app.surface_mut();
                let active_theme = surface.theme();
                let (w, h) = surface.window_size();
                let f = surface.fonts().clone();
                let fb = surface.framebuffer_mut();
                let rect = Rect::new(
                    0,
                    i32::from(h) - STATUS_BAR_HEIGHT_PX - 30,
                    i32::from(w),
                    30,
                );
                fb.fill_rect_clipped(rect, active_theme.code_bg, rect);
                fb.stroke_rect(rect, active_theme.line_border, rect);
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
                    active_theme.text,
                    rect,
                );
            } else if let Some(ref url) = link_bar {
                render_link_bar(app.surface_mut(), url);
            }

            if show_security {
                let facts =
                    Facts::gather_with(app.residency(), &document, lock_cap, verified_segments)
                        .with_cooperative_reaper(&arenas, &canaries, reaper.has_tripped())
                        .with_window_security(
                            x11.security_info(),
                            verified_at.map(|t| t.elapsed().as_secs()),
                            false,
                        );
                render_security_overlay(app.surface_mut(), &facts);
            }

            if let Some(hist) = &history_drawer {
                let bms = document.bookmarks();
                history_hits = render_history_drawer(app.surface_mut(), hist, &bms);
            } else {
                history_hits.clear();
            }

            render_status_strip(app.surface_mut(), &left, &right);
            if show_help {
                let fonts = app.surface().fonts().clone();
                help_close = Some(
                    render_shortcut_help(app.surface_mut().framebuffer_mut(), &fonts, theme).1,
                );
            } else {
                help_close = None;
            }

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
            ime_preedit.zeroize();
            ime_preedit.clear();
            return Ok(finish_gui_wipe(
                &mut app,
                &document,
                &arenas,
                reaper,
                x11.as_window_mut(),
            ));
        }
        if events.is_empty() {
            // Autosave after 1.5s typing pause (`D-64`, matches TUI loop).
            if app.has_pending()
                && last_activity.elapsed() >= Duration::from_millis(PAUSE_AUTOSAVE_MS)
            {
                let _ = save_gui(&mut app);
                pending_paint = true;
            }
            // Fade transient scrollbar chrome after 1.2s of idle (unless reduced-motion or overlay open);
            // emits at most ONE fade transition frame so true idle stays zero-repaint (`L-5`).
            if !reduced_motion
                && idle_chrome_visible
                && !show_security
                && history_drawer.is_none()
                && find_bar.is_none()
                && link_bar.is_none()
                && !overflow_open
                && last_activity.elapsed() >= Duration::from_millis(CHROME_IDLE_FADE_MS)
            {
                idle_chrome_visible = false;
                pending_paint = true;
            }
            continue;
        }

        last_activity = Instant::now();
        tripwire.beat();
        if reaper.has_tripped() || tripwire.evaluate().is_some() {
            ime_preedit.zeroize();
            ime_preedit.clear();
            return Ok(finish_gui_wipe(
                &mut app,
                &document,
                &arenas,
                reaper,
                x11.as_window_mut(),
            ));
        }

        for ev in events {
            match ev {
                DesktopEvent::CloseRequested => {
                    quit = true;
                }
                DesktopEvent::SystemSuspend => {
                    ime_preedit.zeroize();
                    ime_preedit.clear();
                    tripwire.fire(TripReason::Suspend);
                    return Ok(finish_gui_wipe(
                        &mut app,
                        &document,
                        &arenas,
                        reaper,
                        x11.as_window_mut(),
                    ));
                }
                DesktopEvent::Preedit(mut text) => {
                    ime_preedit.zeroize();
                    ime_preedit.clear();
                    ime_preedit.push_str(&text);
                    text.zeroize();
                    pending_paint = true;
                }
                DesktopEvent::CommitText(mut text) => {
                    idle_chrome_visible = true;
                    ime_preedit.zeroize();
                    ime_preedit.clear();
                    if let Some(ref mut url) = link_bar {
                        for ch in text.chars().filter(|c| !c.is_control()) {
                            url.push(ch);
                        }
                    } else if let Some(ref mut find) = find_bar {
                        match find {
                            FindState::Find { needle, next_idx } => {
                                for ch in text.chars().filter(|c| !c.is_control()) {
                                    needle.push(ch);
                                }
                                *next_idx = 0;
                            }
                            FindState::Replace { replacement, .. } => {
                                for ch in text.chars().filter(|c| !c.is_control()) {
                                    replacement.push(ch);
                                }
                            }
                        }
                    } else if let Some(ref mut h) = history_drawer {
                        if let Some(ref mut bm) = h.bookmark_input {
                            for ch in text.chars().filter(|c| !c.is_control()) {
                                bm.push(ch);
                            }
                        } else if let Some(ref mut s) = h.search_input {
                            for ch in text.chars().filter(|c| !c.is_control()) {
                                s.push(ch);
                            }
                        }
                    } else {
                        status_msg.clear();
                        if app.press_text(&text).is_err() {
                            status_msg = "last edit refused".to_owned();
                        }
                    }
                    text.zeroize();
                    pending_paint = true;
                }
                DesktopEvent::FocusOut => {
                    ime_preedit.zeroize();
                    ime_preedit.clear();
                    x11.clear_clipboard();
                    pending_paint = true;
                }
                DesktopEvent::Expose => {
                    pending_paint = true;
                }
                DesktopEvent::Resize { width, height } => {
                    idle_chrome_visible = true;
                    resize_editor_viewport(&mut app, &mut history_drawer, width, height);
                    pending_paint = true;
                }
                DesktopEvent::Scroll { delta_lines } => {
                    idle_chrome_visible = true;
                    let line_px =
                        i64::from(app.surface().theme().body_line_px) * i64::from(delta_lines);
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
                    idle_chrome_visible = true;
                    let px = i32::from(x);
                    let py = i32::from(y);

                    if let Some(dismiss) = recovery_dismiss_rect {
                        if dismiss.contains(px, py) {
                            recovery_banner_bytes = 0;
                            recovery_dismiss_rect = None;
                            pending_paint = true;
                            continue;
                        }
                    }

                    if show_help {
                        if help_close.is_some_and(|rect| rect.contains(px, py)) {
                            show_help = false;
                        }
                        // The sheet is modal: no click behind it reaches the page or chrome.
                        pending_paint = true;
                        continue;
                    }

                    let chrome_hit = sidebar_hits
                        .iter()
                        .rev()
                        .find(|(rect, _)| rect.contains(px, py))
                        .copied()
                        .or_else(|| {
                            breadcrumb_hits
                                .iter()
                                .rev()
                                .find(|(rect, _)| rect.contains(px, py))
                                .copied()
                        });
                    if let Some((_, hit)) = chrome_hit {
                        match hit {
                            DocumentChromeHit::Home => {
                                home_requested = true;
                                quit = true;
                            }
                            DocumentChromeHit::Folder(idx) => {
                                if let Some(folder) = chrome_data.folders.get(idx).cloned() {
                                    *home_folder = Some(folder);
                                    home_requested = true;
                                    quit = true;
                                }
                            }
                            DocumentChromeHit::Find if history_drawer.is_none() => {
                                link_bar = None;
                                find_bar = Some(FindState::Find {
                                    needle: String::new(),
                                    next_idx: 0,
                                });
                            }
                            DocumentChromeHit::History => {
                                history_drawer = if history_drawer.is_some() {
                                    None
                                } else {
                                    let (lw, lh) = app.surface().logical_editor_extent();
                                    let fonts = app.surface().fonts().clone();
                                    let view = app.editor().view();
                                    Some(build_history_state(&document, fonts, lw, lh, view))
                                };
                            }
                            DocumentChromeHit::Security => {
                                show_security = !show_security;
                                if show_security {
                                    verified_segments =
                                        document.verify().ok().map(|r| r.segments_verified);
                                    verified_at = verified_segments.map(|_| Instant::now());
                                }
                            }
                            DocumentChromeHit::Help => {
                                show_help = !show_help;
                                if show_help {
                                    find_bar = None;
                                    link_bar = None;
                                }
                            }
                            DocumentChromeHit::Find => {}
                        }
                        pending_paint = true;
                        continue;
                    }

                    if let Some(hit) = toolbar_hits
                        .iter()
                        .find(|h| h.rect.contains(px, py))
                        .copied()
                    {
                        match hit.action {
                            ToolbarAction::GoHome => {
                                home_requested = true;
                                quit = true;
                                pending_paint = true;
                                continue;
                            }
                            ToolbarAction::CycleUiScale => {
                                overflow_open = false;
                                apply_display_scale(x11, 1, &mut status_msg);
                                let (w, h) = x11.size();
                                resize_editor_viewport(&mut app, &mut history_drawer, w, h);
                            }
                            ToolbarAction::ToggleOverflow => {
                                overflow_open = !overflow_open;
                            }
                            ToolbarAction::ToggleReducedMotion => {
                                reduced_motion = !reduced_motion;
                                overflow_open = false;
                            }
                            ToolbarAction::ToggleSecurity => {
                                overflow_open = false;
                                show_security = !show_security;
                                if show_security {
                                    verified_segments =
                                        document.verify().ok().map(|r| r.segments_verified);
                                    verified_at = verified_segments.map(|_| Instant::now());
                                }
                            }
                            ToolbarAction::ToggleHistory => {
                                overflow_open = false;
                                history_drawer = if history_drawer.is_some() {
                                    None
                                } else {
                                    let (lw, lh) = app.surface().logical_editor_extent();
                                    let fonts = app.surface().fonts().clone();
                                    let view = app.editor().view();
                                    Some(build_history_state(&document, fonts, lw, lh, view))
                                };
                            }
                            ToolbarAction::OpenLinkDialog if history_drawer.is_none() => {
                                overflow_open = false;
                                find_bar = None;
                                link_bar = Some(app.editor().link_at_caret().unwrap_or_default());
                            }
                            other if history_drawer.is_none() => {
                                overflow_open = false;
                                let _ = apply_toolbar_action(&mut app, other);
                            }
                            other @ (ToolbarAction::ZoomIn
                            | ToolbarAction::ZoomOut
                            | ToolbarAction::ToggleTheme) => {
                                overflow_open = false;
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
                            _ => {
                                overflow_open = false;
                            }
                        }
                        pending_paint = true;
                        continue;
                    }
                    if overflow_open {
                        overflow_open = false;
                        pending_paint = true;
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
                        if modifiers.shift {
                            last_click = None;
                            let _ = app.editor().apply(Intent::DragTo(pt));
                        } else {
                            let now = Instant::now();
                            let count = match last_click {
                                Some((prev_t, lx, ly, c))
                                    if now.duration_since(prev_t)
                                        <= Duration::from_millis(DOUBLE_CLICK_MS)
                                        && x.abs_diff(lx) <= 4
                                        && y.abs_diff(ly) <= 4 =>
                                {
                                    if c >= 4 {
                                        1
                                    } else {
                                        c + 1
                                    }
                                }
                                _ => 1,
                            };
                            last_click = Some((now, x, y, count));
                            match count {
                                1 => {
                                    let _ = app.editor().apply(Intent::Click(pt));
                                }
                                2 => {
                                    let _ = app.editor().select_word_at(pt);
                                }
                                3 => {
                                    let _ = app.editor().select_visual_line_at(pt);
                                }
                                _ => {
                                    let _ = app.editor().select_paragraph_at(pt);
                                }
                            }
                        }
                        app.mark_dirty();
                        pending_paint = true;
                    }
                }
                DesktopEvent::MouseDrag { x, y } => {
                    idle_chrome_visible = true;
                    if show_help {
                        continue;
                    }
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
                    idle_chrome_visible = true;
                    if history_drawer.is_none() {
                        let Ok(text) = std::str::from_utf8(&bytes) else {
                            continue;
                        };
                        let _ = app.press_paste_text(text);
                        pending_paint = true;
                    }
                }
                DesktopEvent::Key { key, modifiers } => {
                    idle_chrome_visible = true;
                    // `D-101`: typing with the sheet open edited the *document* underneath it.
                    // The sheet swallows every key except its own dismissals and quit, so it is
                    // modal for the keyboard as well as the pointer.
                    if show_help {
                        if matches!(key, Key::Escape) {
                            show_help = false;
                        } else if modifiers.control && !modifiers.alt {
                            match key {
                                Key::Character('/') => show_help = false,
                                Key::Character('q' | 'Q' | 'c' | 'C') => {
                                    show_help = false;
                                    quit = true;
                                }
                                _ => {}
                            }
                        }
                        pending_paint = true;
                        continue;
                    }
                    if modifiers.control && modifiers.alt {
                        let steps = match key {
                            Key::Character('=' | '+') => Some(1),
                            Key::Character('-') => Some(-1),
                            Key::Character('0') => Some(0),
                            _ => None,
                        };
                        if let Some(steps) = steps {
                            apply_display_scale(x11, steps, &mut status_msg);
                            let (w, h) = x11.size();
                            resize_editor_viewport(&mut app, &mut history_drawer, w, h);
                            pending_paint = true;
                            continue;
                        }
                    }
                    if modifiers.control && !modifiers.alt {
                        match key {
                            Key::Character('/') => {
                                show_help = true;
                                // `D-101`: opening the sheet over an anchored find/link bar left
                                // the bar live underneath it; the sheet is the only overlay now.
                                find_bar = None;
                                link_bar = None;
                                pending_paint = true;
                                continue;
                            }
                            Key::Character('q') => {
                                quit = true;
                                continue;
                            }
                            Key::Character('l') => {
                                tripwire.fire(TripReason::Manual);
                                return Ok(finish_gui_wipe(
                                    &mut app,
                                    &document,
                                    &arenas,
                                    reaper,
                                    x11.as_window_mut(),
                                ));
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
                            Key::Character('d' | 'D') if modifiers.shift => {
                                let _ = apply_toolbar_action(&mut app, ToolbarAction::ToggleTheme);
                                pending_paint = true;
                                continue;
                            }
                            Key::Character('h' | 'H') if modifiers.shift => {
                                home_requested = true;
                                quit = true;
                                pending_paint = true;
                                continue;
                            }
                            Key::Character('m' | 'M') if modifiers.shift => {
                                reduced_motion = !reduced_motion;
                                status_msg = if reduced_motion {
                                    "reduced motion on".to_owned()
                                } else {
                                    "reduced motion off".to_owned()
                                };
                                pending_paint = true;
                                continue;
                            }
                            Key::Character('k') if history_drawer.is_none() => {
                                find_bar = None;
                                link_bar = Some(app.editor().link_at_caret().unwrap_or_default());
                                pending_paint = true;
                                continue;
                            }
                            Key::Character('g') => {
                                show_security = !show_security;
                                if show_security {
                                    verified_segments =
                                        document.verify().ok().map(|r| r.segments_verified);
                                    verified_at = verified_segments.map(|_| Instant::now());
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
                                link_bar = None;
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
                        // The sheet is dismissed by the modal key guard above, so reaching here
                        // means it is closed and `Escape` belongs to the layers beneath.
                        if let Some(ref mut h) = history_drawer {
                            if h.bookmark_input.take().is_some() || h.search_input.take().is_some()
                            {
                                pending_paint = true;
                                continue;
                            }
                        }
                        if link_bar.take().is_some()
                            || find_bar.take().is_some()
                            || overflow_open
                            || show_security
                            || history_drawer.take().is_some()
                        {
                            overflow_open = false;
                            show_security = false;
                            pending_paint = true;
                            continue;
                        }
                        if recovery_banner_bytes != 0 {
                            recovery_banner_bytes = 0;
                            recovery_dismiss_rect = None;
                            pending_paint = true;
                            continue;
                        }
                    }

                    if let Some(ref mut url) = link_bar {
                        let close_link =
                            handle_link_key(&mut app, url, key, modifiers, &mut status_msg);
                        if close_link {
                            link_bar = None;
                        }
                        pending_paint = true;
                        continue;
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

    x11.clear_clipboard();
    if home_requested {
        return Ok(if !app.has_pending() || save_gui(&mut app) {
            DesktopEnd::GoHome
        } else {
            DesktopEnd::SaveFailed
        });
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
        self.paste_text(text)
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
    win: &mut dyn DesktopWindow,
) -> DesktopEnd {
    win.clear_clipboard();
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
        preview_text: Zeroizing::new(String::new()),
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
        state.preview_text = Zeroizing::new(
            document
                .read_witness_excerpt(w_seq, 0, sp.len().min(320))
                .unwrap_or_default(),
        );
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
            let next_len = next_spine.len();
            if app.editor().restore_spine(next_spine).is_ok() {
                if next_len <= 2 * 1024 * 1024 {
                    let _ = app.editor().reflow();
                }
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

fn handle_link_key(
    app: &mut App<store::DocumentSink, store::DocumentSource, FontSet, PixelSurface>,
    url: &mut String,
    key: Key,
    modifiers: Modifiers,
    status_msg: &mut String,
) -> bool {
    match key {
        Key::Backspace => {
            url.pop();
            false
        }
        Key::Enter => {
            let trimmed = url.trim().to_owned();
            let link_arg = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.clone())
            };
            match app.editor().apply(Intent::SetLink(link_arg.clone())) {
                Ok(_) => {
                    app.mark_dirty();
                    *status_msg = match link_arg {
                        Some(u) => format!("linked -> {u}"),
                        None => "link cleared".to_owned(),
                    };
                }
                Err(err) => {
                    *status_msg = format!("link refused: {err}");
                }
            }
            true
        }
        Key::Character(ch) if !modifiers.control && !modifiers.alt && url.len() < 512 => {
            url.push(ch);
            false
        }
        _ => false,
    }
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
        let hits = render_toolbar(&mut app, SecurityDotState::Protected, false, false, 100);
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
        assert!(
            hits.iter().any(|h| h.action == ToolbarAction::GoHome),
            "Home button hit-box must exist"
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
        let hits = render_toolbar(&mut app, SecurityDotState::Unprotected, false, false, 100);
        assert!(
            !hits.iter().any(|h| h.action == ToolbarAction::ZoomIn),
            "narrow width (< 720 px) must collapse overflow controls cleanly"
        );
        assert!(
            hits.iter()
                .any(|h| h.action == ToolbarAction::ToggleOverflow),
            "narrow toolbar must expose clickable overflow button"
        );
        let overflow_hits = render_toolbar_with(
            &mut app,
            SecurityDotState::Unprotected,
            false,
            false,
            true,
            false,
            100,
        );
        let has_zoom = overflow_hits
            .iter()
            .any(|h| h.action == ToolbarAction::ZoomIn);
        assert!(
            has_zoom,
            "opening overflow popup must reveal collapsed controls"
        );
        let has_theme = overflow_hits
            .iter()
            .any(|h| h.action == ToolbarAction::ToggleTheme);
        assert!(
            has_theme,
            "overflow popup must expose light/dark theme toggle"
        );
        assert!(
            overflow_hits
                .iter()
                .any(|h| h.action == ToolbarAction::CycleUiScale),
            "overflow popup must expose whole-window display scale control"
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

    #[test]
    fn dark_theme_size_cycle_lists_link_dialog_and_word_keys_work() {
        let mut app = open_mem_app(View::IDENTITY, 1_120, 760);
        for ch in "one two\nthree four".chars() {
            app.press(Key::Character(ch), Modifiers::none())
                .expect("type");
        }

        // Theme toggle switches paper to #1A1A19 dark mode cleanly.
        apply_toolbar_action(&mut app, ToolbarAction::ToggleTheme).expect("dark mode");
        assert_eq!(app.surface().theme(), QUIET_MD_DARK_THEME);
        assert!(app.draw().expect("dark paint"));
        let dark_px = rgba_to_xrgb(QUIET_MD_DARK_THEME.bg_body);
        assert!(app.surface().pixels().contains(&dark_px));

        // CycleSize advances 15px -> 17px -> 21px.
        app.editor().select_range(0, 3).expect("select one");
        apply_toolbar_action(&mut app, ToolbarAction::CycleSize).expect("17px");
        assert_eq!(
            app.editor().toolbar_state().size_milli,
            TriState::Uniform(1_133)
        );

        // Visual list markers render without mutating plaintext spine.
        apply_toolbar_action(&mut app, ToolbarAction::ToggleBullet).expect("bullet");
        app.editor().select_range(8, 13).expect("select three");
        apply_toolbar_action(&mut app, ToolbarAction::ToggleNumbered).expect("ordered");
        app.mark_dirty();
        assert!(app.draw().expect("draw lists"));

        // Ctrl+Backspace deletes word before caret; Ctrl+Left jumps word.
        app.editor().select_range(7, 7).expect("after two");
        app.press(Key::Backspace, Modifiers::control())
            .expect("ctrl+backspace");
        assert_eq!(app.frame_text().expect("frame"), "one \nthree four");
        app.press(Key::Left, Modifiers::control())
            .expect("ctrl+left");
        assert_eq!(app.editor().selection().anchor(), 0);

        // SetLink & link_at_caret roundtrip.
        app.editor().select_range(0, 3).expect("select one");
        let link = Intent::SetLink(Some("https://holonomy.example".to_owned()));
        app.editor().apply(link).expect("set link");
        assert_eq!(
            app.editor().link_at_caret().as_deref(),
            Some("https://holonomy.example")
        );

        // Scrollbar, recovery banner & IME preedit pill render without panics.
        let metrics = app.editor().status_metrics();
        app.surface_mut().render_scrollbar(metrics);
        let before_preedit = app.surface().pixels().to_vec();
        app.surface_mut().render_ime_preedit("日本´");
        assert_ne!(app.surface().pixels(), before_preedit.as_slice());

        // Committed IME / dead-key text coalesces into the active typing burst for single undo.
        app.press(Key::Character('c'), Modifiers::none())
            .expect("type c");
        app.press(Key::Character('a'), Modifiers::none())
            .expect("type a");
        app.press(Key::Character('f'), Modifiers::none())
            .expect("type f");
        app.press_text("é").expect("commit accented e");
        assert!(app.frame_text().expect("frame").contains("café"));
        app.press(Key::Character('z'), Modifiers::control())
            .expect("single undo across café burst");
        assert!(!app.frame_text().expect("frame").contains("café"));

        // Tiny canvas_clip height must not panic when clamping IME preedit or scrollbar.
        app.surface_mut().canvas_clip = Rect::new(0, 0, 120, 4);
        app.surface_mut().render_ime_preedit("あ");
        app.surface_mut().render_scrollbar(metrics);

        let bar_y = i32::from(QUIET_MD_THEME.toolbar_height_px);
        let dismiss = render_recovery_banner(app.surface_mut(), 42, bar_y);
        assert!(dismiss.is_some());
        render_link_bar(app.surface_mut(), "https://holonomy.example");
    }

    #[test]
    fn rounded_spans_inset_the_corner_rows() {
        let rect = Rect::new(0, 0, 20, 20);
        assert_eq!(FrameBuffer::rounded_span(rect, 6, 0), Some((6, 14)));
        assert_eq!(FrameBuffer::rounded_span(rect, 6, 6), Some((0, 20)));
        assert_eq!(FrameBuffer::rounded_span(rect, 6, 19), Some((6, 14)));
    }

    #[test]
    fn home_library_groups_documents_by_folder() {
        let root = std::env::temp_dir().join(format!(
            "holonomy-home-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        let blue = root.join("Blue");
        let city = root.join("Maps & Cities");
        std::fs::create_dir_all(&blue).expect("blue folder");
        std::fs::create_dir_all(&city).expect("city folder");
        std::fs::write(blue.join("Board.wavefunction"), b"").expect("board");
        std::fs::write(blue.join("Blueprint.wavefunction"), b"").expect("blueprint");
        std::fs::write(city.join("Snow.wavefunction"), b"").expect("snow");

        let entries = enumerate_home_entries(&root);
        assert_eq!(entries.len(), 3);
        let folders = group_home_folders(&entries);
        assert_eq!(folders.len(), 2);
        assert!(folders
            .iter()
            .any(|f| f.name == "Blue" && f.entries.len() == 2));
        assert!(folders
            .iter()
            .any(|f| f.name == "Maps & Cities" && f.entries.len() == 1));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn home_shell_exposes_search_and_document_hit_targets() {
        let fonts = FontSet::embedded(QUIET_MD_THEME.body_size_px).expect("fonts");
        let entries = vec![
            HomeEntry {
                name: "Board".to_owned(),
                folder: "Blue".to_owned(),
                path: PathBuf::from("Blue/Board.wavefunction"),
            },
            HomeEntry {
                name: "Snow".to_owned(),
                folder: "Maps".to_owned(),
                path: PathBuf::from("Maps/Snow.wavefunction"),
            },
        ];
        let folders = group_home_folders(&entries);
        let state = HomeState {
            entries,
            folders,
            active_folder: None,
            query: String::new(),
            search_focused: false,
            favorites: false,
            manual_order: false,
            list_view: false,
            settings_open: false,
        };
        let mut fb = FrameBuffer::new(1_120, 780, HOME_CANVAS_BG);
        let hits = render_home(&mut fb, &fonts, &state, 100);
        assert!(hits.iter().any(|(_, hit)| *hit == HomeHit::NewDocument));
        assert!(hits.iter().any(|(_, hit)| *hit == HomeHit::Search));
        assert!(hits
            .iter()
            .any(|(_, hit)| matches!(hit, HomeHit::Folder(_))));

        let mut search_state = HomeState {
            query: "snow".to_owned(),
            ..state
        };
        assert_eq!(search_state.matching_entries(), vec![1]);
        search_state.active_folder = Some(0);
        assert!(search_state.matching_entries().is_empty());
    }

    #[test]
    fn new_document_suggestion_stays_in_library_and_avoids_collisions() {
        let root = std::env::temp_dir().join(format!(
            "holonomy-new-doc-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).expect("library folder");
        let first = home_new_document_path(&root);
        assert_eq!(first.parent(), Some(root.as_path()));
        assert_eq!(
            first.extension().and_then(|e| e.to_str()),
            Some("wavefunction")
        );
        assert!(!first.exists());
        std::fs::write(&first, b"").expect("suggested path");
        let second = home_new_document_path(&root);
        assert_ne!(first, second);
        assert!(!second.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn quiet_chrome_tokens_match_the_drift_guarded_page_theme() {
        assert_eq!(HOME_ACCENT, QUIET_MD_THEME.caret);
        assert_eq!(HOME_INK, QUIET_MD_THEME.text);
        assert_eq!(HOME_MUTED, QUIET_MD_THEME.text_caption);
        assert_eq!(HOME_SURFACE, QUIET_MD_THEME.bg_body);
        assert_eq!(DOC_SIDEBAR_W_PX, HOME_SIDEBAR_W);
        assert_eq!(
            DOC_BREADCRUMB_H_PX,
            i32::from(QUIET_MD_THEME.toolbar_height_px)
        );
    }

    #[test]
    fn chrome_reserves_page_space_without_moving_the_hit_test_origin() {
        let fonts = FontSet::embedded(QUIET_MD_THEME.body_size_px).expect("embedded fonts");
        let wide = PixelSurface::new(fonts.clone(), View::IDENTITY, 1_280, 800);
        assert_eq!(wide.sidebar_width(), DOC_SIDEBAR_W_PX);
        assert_eq!(wide.breadcrumb_height(), DOC_BREADCRUMB_H_PX);
        let (ox, oy) = wide.page_origin();
        assert!(ox > wide.sidebar_width());
        assert!(oy > i32::from(QUIET_MD_THEME.toolbar_height_px));
        let mapped = wide.canvas_point(ox as i16, oy as i16).expect("page point");
        assert_eq!(mapped.x.sixtyfourths(), 0);
        assert_eq!(mapped.y.sixtyfourths(), 0);
        assert!(wide.canvas_point(10, 200).is_none());

        let narrow = PixelSurface::new(fonts, View::IDENTITY, 700, 600);
        assert_eq!(narrow.sidebar_width(), 0);
        assert_eq!(narrow.breadcrumb_height(), 0);
    }

    #[test]
    fn document_sidebar_uses_metadata_only_and_paints_inside_its_rect() {
        let fonts = FontSet::embedded(QUIET_MD_THEME.body_size_px).expect("embedded fonts");
        let mut fb = FrameBuffer::new(1_280, 800, HOME_CANVAS_BG);
        let before = fb.pixels.clone();
        let rect = Rect::new(0, 40, DOC_SIDEBAR_W_PX, 736);
        let data = DocumentChromeData {
            library_label: "My documents".to_owned(),
            folder_label: Some("Journal".to_owned()),
            document_label: "notes.wavefunction".to_owned(),
            folders: vec!["Journal".to_owned(), "Field Notes".to_owned()],
        };
        let hits = render_document_sidebar(&mut fb, &fonts, QUIET_MD_THEME, rect, &data);
        assert!(hits.iter().any(|(_, h)| *h == DocumentChromeHit::Home));
        assert!(hits
            .iter()
            .any(|(_, h)| matches!(h, DocumentChromeHit::Folder(_))));
        assert!(hits.iter().any(|(_, h)| *h == DocumentChromeHit::History));
        assert!(hits.iter().any(|(_, h)| *h == DocumentChromeHit::Security));
        assert!(hits.iter().any(|(_, h)| *h == DocumentChromeHit::Help));
        for (_, hit) in &hits {
            assert!(
                hit != &DocumentChromeHit::Find,
                "no search icon in the rail"
            );
        }

        for y in 0..i32::from(fb.height) {
            for x in 0..i32::from(fb.width) {
                if rect.contains(x, y) {
                    continue;
                }
                let i = (y as usize) * usize::from(fb.width) + x as usize;
                assert_eq!(
                    fb.pixels[i], before[i],
                    "rail painted outside its rect at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn home_sidebar_plus_is_not_swallowed_by_the_library_row() {
        let fonts = FontSet::embedded(QUIET_MD_THEME.body_size_px).expect("embedded fonts");
        let mut fb = FrameBuffer::new(1_120, 780, HOME_CANVAS_BG);
        let state = HomeState {
            entries: Vec::new(),
            folders: Vec::new(),
            active_folder: None,
            query: String::new(),
            search_focused: false,
            favorites: false,
            manual_order: false,
            list_view: false,
            settings_open: false,
        };
        let hits = render_home(&mut fb, &fonts, &state, 100);
        let plus = hits
            .iter()
            .find(|(_, hit)| *hit == HomeHit::NewDocument)
            .map(|(rect, _)| *rect)
            .expect("new-document target");
        let resolved = hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(plus.x + plus.w / 2, plus.y + plus.h / 2))
            .map(|(_, hit)| *hit);
        assert_eq!(resolved, Some(HomeHit::NewDocument));
    }

    /// WCAG relative-luminance contrast ratio, used only by these regression tests.
    fn contrast_ratio(a: Rgba8, b: Rgba8) -> f64 {
        fn channel(value: u8) -> f64 {
            let c = f64::from(value) / 255.0;
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        }
        fn luminance(c: Rgba8) -> f64 {
            0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
        }
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// `D-101`: the chrome ink helpers used to be the Home-only light palette, so the rail,
    /// breadcrumb and shortcut sheet were ~1.1:1 ink on `#1A1A19` dark paper — invisible.
    #[test]
    fn chrome_ink_stays_readable_on_both_papers() {
        for theme in [QUIET_MD_THEME, QUIET_MD_DARK_THEME] {
            assert!(
                contrast_ratio(ink_for(&theme), theme.bg_body) >= 7.0,
                "primary chrome ink too low on {theme:?}"
            );
            assert!(
                contrast_ratio(muted_ink_for(&theme), theme.bg_body) >= 4.0,
                "caption chrome ink too low on {theme:?}"
            );
            assert!(
                contrast_ratio(accent_ink_for(&theme), theme.bg_body) >= 3.0,
                "accent chrome ink too low on {theme:?}"
            );
        }
        // Light paper keeps exactly the quoted quiet.md tokens.
        assert_eq!(ink_for(&QUIET_MD_THEME), HOME_INK);
        assert_eq!(muted_ink_for(&QUIET_MD_THEME), HOME_MUTED);
        assert_eq!(accent_ink_for(&QUIET_MD_THEME), HOME_ACCENT);
        assert_eq!(ink_for(&QUIET_MD_DARK_THEME), QUIET_MD_DARK_THEME.text);
    }

    /// `D-101`: seven `268 px` columns overflowed the `560 px` sheet, so only 4 of the 14
    /// shortcuts were visible. Every entry must now land inside the panel at small and large
    /// window sizes alike.
    #[test]
    fn shortcut_sheet_shows_every_entry_inside_its_panel() {
        const ENTRIES: i32 = 14;
        for (w, h) in [(1_120, 780), (1_280, 800), (760, 520), (1_920, 1_080)] {
            let (panel, cols, row_h) = shortcut_help_geometry(w, h, ENTRIES);
            let per_col = ceil_div(ENTRIES, cols);
            let col_w = (panel.w - 48) / cols;
            for i in 0..ENTRIES {
                let col = i / per_col;
                let row = i % per_col;
                let rx = panel.x + 24 + col * col_w;
                let ry = panel.y + 84 + row * row_h;
                assert!(
                    rx + 200 <= panel.x + panel.w,
                    "entry {i} runs past the panel at {w}x{h}"
                );
                assert!(
                    ry + 15 <= panel.y + panel.h,
                    "entry {i} falls below the panel at {w}x{h}"
                );
            }
            assert!(
                panel.x >= 0 && panel.y >= 0,
                "panel starts off-screen at {w}x{h}"
            );
            assert!(
                panel.x + panel.w <= w && panel.y + panel.h <= h,
                "panel overflows the window at {w}x{h}"
            );
        }
        // A narrow window needs the single-column fallback rather than clipped columns.
        assert_eq!(shortcut_help_geometry(480, 600, ENTRIES).1, 1);
        assert_eq!(shortcut_help_geometry(1_120, 780, ENTRIES).1, 2);
    }

    /// `D-101`: the rail keeps quiet.md's light wash, which is wrong on `#1A1A19` paper. The
    /// dark paper rail must use theme chrome and still never paint outside its rect.
    #[test]
    fn dark_paper_rail_paints_inside_its_rect() {
        let fonts = FontSet::embedded(QUIET_MD_THEME.body_size_px).expect("embedded fonts");
        let mut fb = FrameBuffer::new(1_280, 800, QUIET_MD_DARK_THEME.bg_body);
        let before = fb.pixels.clone();
        let rect = Rect::new(0, 40, DOC_SIDEBAR_W_PX, 736);
        let data = DocumentChromeData {
            library_label: "My documents".to_owned(),
            folder_label: Some("Journal".to_owned()),
            document_label: "notes.wavefunction".to_owned(),
            folders: vec!["Journal".to_owned(), "Field Notes".to_owned()],
        };
        let hits = render_document_sidebar(&mut fb, &fonts, QUIET_MD_DARK_THEME, rect, &data);
        assert!(hits.iter().any(|(_, h)| *h == DocumentChromeHit::Home));
        for y in 0..i32::from(fb.height) {
            for x in 0..i32::from(fb.width) {
                if rect.contains(x, y) {
                    continue;
                }
                let i = (y as usize) * usize::from(fb.width) + x as usize;
                assert_eq!(
                    fb.pixels[i], before[i],
                    "dark rail painted outside its rect"
                );
            }
        }
    }
}
