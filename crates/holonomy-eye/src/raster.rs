//! TrueType `loca`/`glyf` outline decoder, 4x4 A8 scanline rasterizer, and embedded
//! [`FontSet`] (`Inter` + `JetBrains Mono`) for pixel-exact rendering under `holonomy-eye`.
//!
//! Both `advance`/`ascent`/`line_height` (`Shaper`) and rasterized A8 glyph bitmaps come from the
//! same parsed SFNT faces so the desktop canvas obeys `UI-SPEC.md` Law `U0` and GATES Law `L-8`.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use holonomy_text::FontFamily;

use crate::face::{Face, FaceError, ReadAt, TableDirectory};
use crate::geom::Fixed;
use crate::opentype::ShapedGlyph;
use crate::shaper::Shaper;
use crate::style::Style;

const INTER_REGULAR_TTF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../holonomy-old/assets/fonts/Inter-Regular.ttf"
));
const INTER_BOLD_TTF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../holonomy-old/assets/fonts/Inter-Bold.ttf"
));
const INTER_ITALIC_TTF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../holonomy-old/assets/fonts/Inter-Italic.ttf"
));
const JETBRAINS_MONO_TTF: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../holonomy-old/assets/fonts/JetBrainsMono-Regular.ttf"
));

const MAX_COMPOUND_DEPTH: usize = 8;
const MAX_GLYPH_CACHE_ENTRIES: usize = 4_096;

type RawContours = Vec<Vec<(f32, f32, bool)>>;

/// Which embedded font face corresponds to a styled run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FontVariant {
    /// `Inter-Regular` (body & serif fallback).
    Regular,
    /// `Inter-Bold` (bold body & headings).
    Bold,
    /// `Inter-Italic` (italic emphasis).
    Italic,
    /// `JetBrainsMono-Regular` (inline code & monospace runs).
    Mono,
}

/// Decoded quadratic-flattened outlines for a single glyph, in font design units (`y` upward).
#[derive(Debug, Clone, PartialEq)]
pub struct GlyphOutline {
    /// Closed contour loops in font design units.
    pub contours: Vec<Vec<(f32, f32)>>,
}

/// Alpha-8 rasterized glyph bitmap anchored relative to the baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlyphBitmap {
    /// Pixel width of `coverage`.
    pub width: usize,
    /// Pixel height of `coverage`.
    pub height: usize,
    /// Horizontal offset from pen `x` to bitmap left edge (`px`).
    pub bearing_x: i32,
    /// Distance upward from baseline `y` to bitmap top edge (`px`).
    pub bearing_y: i32,
    /// Row-major coverage `[0..=255]` (`width * height` bytes).
    pub coverage: Vec<u8>,
}

/// SFNT `loca` + `glyf` table accessor for extracting TrueType simple and compound glyph outlines.
#[derive(Clone)]
pub struct FontOutlines {
    loca_offsets: Vec<u32>,
    glyf: Cow<'static, [u8]>,
}

impl FontOutlines {
    /// Parses `head`, `maxp`, `loca`, and `glyf` from a `'static` font byte slice without copying
    /// `glyf`.
    ///
    /// # Errors
    ///
    /// Returns [`FaceError`] if any required TrueType table is missing or malformed.
    pub fn from_static(bytes: &'static [u8]) -> Result<Self, FaceError> {
        Self::parse_inner(bytes, Cow::Borrowed(bytes))
    }

    /// Parses `head`, `maxp`, `loca`, and `glyf` from an arbitrary borrowed slice, copying only the
    /// `glyf` slice.
    ///
    /// # Errors
    ///
    /// Returns [`FaceError`] if any required TrueType table is missing or malformed.
    pub fn parse(bytes: &[u8]) -> Result<Self, FaceError> {
        let tables = TableDirectory::parse(bytes)?;
        let glyf = tables.get(bytes, b"glyf")?.to_vec();
        Self::parse_with_glyf(bytes, &tables, Cow::Owned(glyf))
    }

    fn parse_inner(bytes: &'static [u8], whole: Cow<'static, [u8]>) -> Result<Self, FaceError> {
        let tables = TableDirectory::parse(bytes)?;
        let (off, len) = Self::glyf_span(bytes)?;
        let glyf = match whole {
            Cow::Borrowed(s) => Cow::Borrowed(&s[off..off + len]),
            Cow::Owned(v) => Cow::Owned(v[off..off + len].to_vec()),
        };
        Self::parse_with_glyf(bytes, &tables, glyf)
    }

    fn glyf_span(bytes: &[u8]) -> Result<(usize, usize), FaceError> {
        let count = usize::from(u16::at(bytes, 4).ok_or(FaceError::Truncated)?);
        for index in 0..count {
            let record = 12 + index * 16;
            let tag = bytes.get(record..record + 4).ok_or(FaceError::Truncated)?;
            if tag == b"glyf" {
                let offset =
                    usize::try_from(u32::at(bytes, record + 8).ok_or(FaceError::Truncated)?)
                        .map_err(|_| FaceError::Truncated)?;
                let length =
                    usize::try_from(u32::at(bytes, record + 12).ok_or(FaceError::Truncated)?)
                        .map_err(|_| FaceError::Truncated)?;
                let end = offset.checked_add(length).ok_or(FaceError::Truncated)?;
                if end > bytes.len() {
                    return Err(FaceError::Truncated);
                }
                return Ok((offset, length));
            }
        }
        Err(FaceError::MissingTable("glyf"))
    }

    fn parse_with_glyf(
        bytes: &[u8],
        tables: &TableDirectory,
        glyf: Cow<'static, [u8]>,
    ) -> Result<Self, FaceError> {
        let head = tables.get(bytes, b"head")?;
        let index_to_loc_format = i16::at(head, 50).ok_or(FaceError::BadTable("head"))?;
        if index_to_loc_format != 0 && index_to_loc_format != 1 {
            return Err(FaceError::BadTable("head"));
        }
        let maxp = tables.get(bytes, b"maxp")?;
        let num_glyphs = usize::from(u16::at(maxp, 4).ok_or(FaceError::BadTable("maxp"))?);
        if num_glyphs == 0 {
            return Err(FaceError::BadTable("maxp"));
        }
        let loca = tables.get(bytes, b"loca")?;
        let mut loca_offsets = Vec::with_capacity(num_glyphs + 1);
        if index_to_loc_format == 0 {
            for g in 0..=num_glyphs {
                let val = u16::at(loca, g * 2).ok_or(FaceError::BadTable("loca"))?;
                loca_offsets.push(u32::from(val) * 2);
            }
        } else {
            for g in 0..=num_glyphs {
                let val = u32::at(loca, g * 4).ok_or(FaceError::BadTable("loca"))?;
                loca_offsets.push(val);
            }
        }
        let glyf_len = glyf.len();
        for pair in loca_offsets.windows(2) {
            if pair[0] > pair[1] || (pair[1] as usize) > glyf_len {
                return Err(FaceError::BadTable("loca"));
            }
        }
        Ok(Self { loca_offsets, glyf })
    }

    /// Extracts flattened contour outlines for `glyph_id` in font design units.
    ///
    /// Returns `Ok(None)` for whitespace / empty glyphs (`loca[g] == loca[g+1]`).
    ///
    /// # Errors
    ///
    /// Returns [`FaceError`] if `glyph_id` is out of bounds or the `glyf` record is corrupt.
    pub fn outline(
        &self,
        glyph_id: u16,
        units_per_em: u16,
        px_hint: f32,
    ) -> Result<Option<GlyphOutline>, FaceError> {
        let raw_contours = self.decode_raw_contours(glyph_id, 0)?;
        if raw_contours.is_empty() {
            return Ok(None);
        }
        let scale = px_hint.max(8.0) / f32::from(units_per_em.max(1));
        let flatness_sq = (0.12_f32 / scale).powi(2);
        let mut contours = Vec::with_capacity(raw_contours.len());
        for raw in raw_contours {
            let flat = flatten_contour(&raw, flatness_sq);
            if flat.len() >= 3 {
                contours.push(flat);
            }
        }
        if contours.is_empty() {
            Ok(None)
        } else {
            Ok(Some(GlyphOutline { contours }))
        }
    }

    fn decode_raw_contours(&self, glyph_id: u16, depth: usize) -> Result<RawContours, FaceError> {
        if depth > MAX_COMPOUND_DEPTH {
            return Err(FaceError::BadTable("glyf"));
        }
        let idx = usize::from(glyph_id);
        let start = *self
            .loca_offsets
            .get(idx)
            .ok_or(FaceError::BadTable("glyf"))? as usize;
        let end = *self
            .loca_offsets
            .get(idx + 1)
            .ok_or(FaceError::BadTable("glyf"))? as usize;
        if start == end {
            return Ok(Vec::new());
        }
        let record = self
            .glyf
            .get(start..end)
            .ok_or(FaceError::BadTable("glyf"))?;
        let num_contours = i16::at(record, 0).ok_or(FaceError::BadTable("glyf"))?;
        if num_contours >= 0 {
            decode_simple_glyph(record, num_contours as usize)
        } else {
            self.decode_compound_glyph(record, depth)
        }
    }

    fn decode_compound_glyph(&self, record: &[u8], depth: usize) -> Result<RawContours, FaceError> {
        const ARG_1_AND_2_ARE_WORDS: u16 = 0x0001;
        const ARGS_ARE_XY_VALUES: u16 = 0x0002;
        const WE_HAVE_A_SCALE: u16 = 0x0008;
        const MORE_COMPONENTS: u16 = 0x0020;
        const WE_HAVE_AN_X_AND_Y_SCALE: u16 = 0x0040;
        const WE_HAVE_A_TWO_BY_TWO: u16 = 0x0080;
        const SCALED_COMPONENT_OFFSET: u16 = 0x0800;

        let bad = || FaceError::BadTable("glyf");
        let mut off = 10usize;
        let mut combined: Vec<Vec<(f32, f32, bool)>> = Vec::new();
        let mut flat_points_so_far: Vec<(f32, f32)> = Vec::new();

        loop {
            let flags = u16::at(record, off).ok_or_else(bad)?;
            let comp_glyph = u16::at(record, off + 2).ok_or_else(bad)?;
            off += 4;

            let (arg1, arg2): (i32, i32) = if flags & ARG_1_AND_2_ARE_WORDS != 0 {
                let a = if flags & ARGS_ARE_XY_VALUES != 0 {
                    i32::from(i16::at(record, off).ok_or_else(bad)?)
                } else {
                    i32::from(u16::at(record, off).ok_or_else(bad)?)
                };
                let b = if flags & ARGS_ARE_XY_VALUES != 0 {
                    i32::from(i16::at(record, off + 2).ok_or_else(bad)?)
                } else {
                    i32::from(u16::at(record, off + 2).ok_or_else(bad)?)
                };
                off += 4;
                (a, b)
            } else {
                let b1 = *record.get(off).ok_or_else(bad)?;
                let b2 = *record.get(off + 1).ok_or_else(bad)?;
                off += 2;
                if flags & ARGS_ARE_XY_VALUES != 0 {
                    (i32::from(b1 as i8), i32::from(b2 as i8))
                } else {
                    (i32::from(b1), i32::from(b2))
                }
            };

            let (mut a, mut b, mut c, mut d) = (1.0_f32, 0.0_f32, 0.0_f32, 1.0_f32);
            if flags & WE_HAVE_A_SCALE != 0 {
                let s = f2dot14(i16::at(record, off).ok_or_else(bad)?);
                off += 2;
                a = s;
                d = s;
            } else if flags & WE_HAVE_AN_X_AND_Y_SCALE != 0 {
                a = f2dot14(i16::at(record, off).ok_or_else(bad)?);
                d = f2dot14(i16::at(record, off + 2).ok_or_else(bad)?);
                off += 4;
            } else if flags & WE_HAVE_A_TWO_BY_TWO != 0 {
                a = f2dot14(i16::at(record, off).ok_or_else(bad)?);
                b = f2dot14(i16::at(record, off + 2).ok_or_else(bad)?);
                c = f2dot14(i16::at(record, off + 4).ok_or_else(bad)?);
                d = f2dot14(i16::at(record, off + 6).ok_or_else(bad)?);
                off += 8;
            }

            let sub_contours = self.decode_raw_contours(comp_glyph, depth + 1)?;
            let mut sub_linear: Vec<(f32, f32)> = Vec::new();
            let mut transformed = Vec::with_capacity(sub_contours.len());
            for contour in &sub_contours {
                let mut pts = Vec::with_capacity(contour.len());
                for &(x, y, on) in contour {
                    let tx = a * x + c * y;
                    let ty = b * x + d * y;
                    sub_linear.push((tx, ty));
                    pts.push((tx, ty, on));
                }
                transformed.push(pts);
            }

            let (dx, dy) = if flags & ARGS_ARE_XY_VALUES != 0 {
                let raw_dx = arg1 as f32;
                let raw_dy = arg2 as f32;
                if flags & SCALED_COMPONENT_OFFSET != 0 {
                    (a * raw_dx + c * raw_dy, b * raw_dx + d * raw_dy)
                } else {
                    (raw_dx, raw_dy)
                }
            } else {
                let p_idx = usize::try_from(arg1).map_err(|_| bad())?;
                let c_idx = usize::try_from(arg2).map_err(|_| bad())?;
                let parent_pt = *flat_points_so_far.get(p_idx).ok_or_else(bad)?;
                let child_pt = *sub_linear.get(c_idx).ok_or_else(bad)?;
                (parent_pt.0 - child_pt.0, parent_pt.1 - child_pt.1)
            };

            for mut contour in transformed {
                for pt in &mut contour {
                    pt.0 += dx;
                    pt.1 += dy;
                    flat_points_so_far.push((pt.0, pt.1));
                }
                combined.push(contour);
            }

            if flags & MORE_COMPONENTS == 0 {
                break;
            }
        }
        Ok(combined)
    }
}

fn f2dot14(raw: i16) -> f32 {
    f32::from(raw) / 16_384.0
}

fn decode_simple_glyph(record: &[u8], num_contours: usize) -> Result<RawContours, FaceError> {
    let bad = || FaceError::BadTable("glyf");
    if num_contours == 0 {
        return Ok(Vec::new());
    }
    let mut off = 10usize;
    let mut end_pts = Vec::with_capacity(num_contours);
    let mut prev_end: Option<u16> = None;
    for _ in 0..num_contours {
        let ep = u16::at(record, off).ok_or_else(bad)?;
        if let Some(prev) = prev_end {
            if ep <= prev {
                return Err(bad());
            }
        }
        end_pts.push(ep);
        prev_end = Some(ep);
        off += 2;
    }
    let total_points = usize::from(*end_pts.last().ok_or_else(bad)?) + 1;

    let instr_len = usize::from(u16::at(record, off).ok_or_else(bad)?);
    off = off
        .checked_add(2 + instr_len)
        .filter(|&end| end <= record.len())
        .ok_or_else(bad)?;

    let mut flags = Vec::with_capacity(total_points);
    while flags.len() < total_points {
        let flag = *record.get(off).ok_or_else(bad)?;
        off += 1;
        flags.push(flag);
        if flag & 0x08 != 0 {
            let repeat = usize::from(*record.get(off).ok_or_else(bad)?);
            off += 1;
            if flags.len() + repeat > total_points {
                return Err(bad());
            }
            for _ in 0..repeat {
                flags.push(flag);
            }
        }
    }

    let mut xs = Vec::with_capacity(total_points);
    let mut x = 0i32;
    for &flag in &flags {
        let short = (flag & 0x02) != 0;
        let same_or_pos = (flag & 0x10) != 0;
        let dx = if short {
            let v = i32::from(*record.get(off).ok_or_else(bad)?);
            off += 1;
            if same_or_pos {
                v
            } else {
                -v
            }
        } else if same_or_pos {
            0
        } else {
            let v = i32::from(i16::at(record, off).ok_or_else(bad)?);
            off += 2;
            v
        };
        x += dx;
        xs.push(x);
    }

    let mut ys = Vec::with_capacity(total_points);
    let mut y = 0i32;
    for &flag in &flags {
        let short = (flag & 0x04) != 0;
        let same_or_pos = (flag & 0x20) != 0;
        let dy = if short {
            let v = i32::from(*record.get(off).ok_or_else(bad)?);
            off += 1;
            if same_or_pos {
                v
            } else {
                -v
            }
        } else if same_or_pos {
            0
        } else {
            let v = i32::from(i16::at(record, off).ok_or_else(bad)?);
            off += 2;
            v
        };
        y += dy;
        ys.push(y);
    }

    let mut contours = Vec::with_capacity(num_contours);
    let mut start = 0usize;
    for ep in end_pts {
        let end = usize::from(ep) + 1;
        let mut contour = Vec::with_capacity(end - start);
        for i in start..end {
            contour.push((xs[i] as f32, ys[i] as f32, (flags[i] & 0x01) != 0));
        }
        contours.push(contour);
        start = end;
    }
    Ok(contours)
}

fn flatten_contour(raw: &[(f32, f32, bool)], flatness_sq: f32) -> Vec<(f32, f32)> {
    let n = raw.len();
    if n < 2 {
        return Vec::new();
    }
    let mut on_curve_pts: Vec<(f32, f32, bool)> = Vec::with_capacity(n * 2);
    for i in 0..n {
        let curr = raw[i];
        let next = raw[(i + 1) % n];
        on_curve_pts.push(curr);
        if !curr.2 && !next.2 {
            on_curve_pts.push(((curr.0 + next.0) * 0.5, (curr.1 + next.1) * 0.5, true));
        }
    }

    let start_idx = match on_curve_pts.iter().position(|p| p.2) {
        Some(idx) => idx,
        None => return Vec::new(),
    };

    let m = on_curve_pts.len();
    let mut out = Vec::with_capacity(m * 2);
    let start = (on_curve_pts[start_idx].0, on_curve_pts[start_idx].1);
    out.push(start);

    let mut step = 1usize;
    while step <= m {
        let idx = (start_idx + step) % m;
        let pt = on_curve_pts[idx];
        if pt.2 {
            let p_next = (pt.0, pt.1);
            if step < m {
                out.push(p_next);
            }
            step += 1;
        } else {
            let p0 = *out.last().expect("non-empty contour");
            let ctrl = (pt.0, pt.1);
            let next_idx = (start_idx + step + 1) % m;
            let next_on = on_curve_pts[next_idx];
            let p2 = (next_on.0, next_on.1);
            subdivide_quad(p0, ctrl, p2, flatness_sq, 0, &mut out);
            if step + 1 >= m {
                // Closed back onto `start`; drop the final duplicate vertex if needed.
                break;
            }
            step += 2;
        }
    }
    out
}

fn subdivide_quad(
    p0: (f32, f32),
    ctrl: (f32, f32),
    p2: (f32, f32),
    flatness_sq: f32,
    depth: u8,
    out: &mut Vec<(f32, f32)>,
) {
    let dd_x = p0.0 - 2.0 * ctrl.0 + p2.0;
    let dd_y = p0.1 - 2.0 * ctrl.1 + p2.1;
    let dist_sq = dd_x * dd_x + dd_y * dd_y;
    if depth >= 6 || dist_sq <= flatness_sq {
        out.push(p2);
        return;
    }
    let m0 = ((p0.0 + ctrl.0) * 0.5, (p0.1 + ctrl.1) * 0.5);
    let m1 = ((ctrl.0 + p2.0) * 0.5, (ctrl.1 + p2.1) * 0.5);
    let mid = ((m0.0 + m1.0) * 0.5, (m0.1 + m1.1) * 0.5);
    subdivide_quad(p0, m0, mid, flatness_sq, depth + 1, out);
    subdivide_quad(mid, m1, p2, flatness_sq, depth + 1, out);
}

/// Rasterizes `outline` into an 8-bit antialiased bitmap (`0..=255`) using 4x4 vertical
/// supersampling and analytic horizontal span integration under the non-zero winding rule.
#[must_use]
pub fn rasterize_outline(outline: &GlyphOutline, units_per_em: u16, px_size: f32) -> GlyphBitmap {
    let scale = px_size.max(1.0) / f32::from(units_per_em.max(1));
    let mut scaled_contours: Vec<Vec<(f32, f32)>> = Vec::with_capacity(outline.contours.len());
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;

    for contour in &outline.contours {
        let mut sc = Vec::with_capacity(contour.len());
        for &(x, y) in contour {
            let sx = x * scale;
            // Convert font Y-up into screen Y-down relative to baseline y = 0.
            let sy = -y * scale;
            min_x = min_x.min(sx);
            max_x = max_x.max(sx);
            min_y = min_y.min(sy);
            max_y = max_y.max(sy);
            sc.push((sx, sy));
        }
        if sc.len() >= 3 {
            scaled_contours.push(sc);
        }
    }

    if scaled_contours.is_empty()
        || !min_x.is_finite()
        || !min_y.is_finite()
        || max_x <= min_x
        || max_y <= min_y
    {
        return GlyphBitmap {
            width: 0,
            height: 0,
            bearing_x: 0,
            bearing_y: 0,
            coverage: Vec::new(),
        };
    }

    let ix0 = min_x.floor() as i32 - 1;
    let ix1 = max_x.ceil() as i32 + 1;
    let iy0 = min_y.floor() as i32 - 1;
    let iy1 = max_y.ceil() as i32 + 1;
    let width = usize::try_from((ix1 - ix0).max(0)).unwrap_or(0);
    let height = usize::try_from((iy1 - iy0).max(0)).unwrap_or(0);
    if width == 0 || height == 0 || width > 4_096 || height > 4_096 {
        return GlyphBitmap {
            width: 0,
            height: 0,
            bearing_x: 0,
            bearing_y: 0,
            coverage: Vec::new(),
        };
    }

    let mut accum = vec![0.0_f32; width * height];
    let mut crossings: Vec<(f32, i32)> = Vec::with_capacity(64);
    const SUBSAMPLES: usize = 4;
    const SUB_WEIGHT: f32 = 1.0 / (SUBSAMPLES as f32);

    for row in 0..height {
        let row_slice = &mut accum[row * width..(row + 1) * width];
        for sub in 0..SUBSAMPLES {
            let y = iy0 as f32 + row as f32 + (sub as f32 + 0.5) * SUB_WEIGHT;
            crossings.clear();

            for contour in &scaled_contours {
                let n = contour.len();
                for i in 0..n {
                    let p0 = contour[i];
                    let p1 = contour[(i + 1) % n];
                    if p0.1 == p1.1 {
                        continue;
                    }
                    let (lo, hi, dir) = if p0.1 < p1.1 {
                        (p0, p1, 1_i32)
                    } else {
                        (p1, p0, -1_i32)
                    };
                    if y >= lo.1 && y < hi.1 {
                        let t = (y - lo.1) / (hi.1 - lo.1);
                        let x_hit = lo.0 + t * (hi.0 - lo.0);
                        crossings.push((x_hit, dir));
                    }
                }
            }

            if crossings.is_empty() {
                continue;
            }
            crossings.sort_by(|a, b| a.0.total_cmp(&b.0));

            let mut winding = 0_i32;
            let mut span_start = 0.0_f32;
            for &(x_hit, dir) in &crossings {
                let prev = winding;
                winding += dir;
                if prev == 0 && winding != 0 {
                    span_start = x_hit;
                } else if prev != 0 && winding == 0 && x_hit > span_start {
                    accumulate_span(row_slice, ix0, width, span_start, x_hit, SUB_WEIGHT);
                }
            }
        }
    }

    let coverage = accum
        .into_iter()
        .map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
        .collect();

    GlyphBitmap {
        width,
        height,
        bearing_x: ix0,
        bearing_y: -iy0,
        coverage,
    }
}

#[inline]
fn accumulate_span(row: &mut [f32], ix0: i32, width: usize, x_start: f32, x_end: f32, weight: f32) {
    let col_lo = ((x_start.floor() as i32) - ix0).max(0) as usize;
    let col_hi = (((x_end.ceil() as i32) - ix0).min(width as i32)).max(0) as usize;
    for (col, cell) in row.iter_mut().enumerate().take(col_hi).skip(col_lo) {
        let px0 = (ix0 + col as i32) as f32;
        let px1 = px0 + 1.0;
        let cov = (x_end.min(px1) - x_start.max(px0)).max(0.0);
        *cell += cov * weight;
    }
}

struct ParsedFont {
    face: Face,
    outlines: FontOutlines,
}

impl ParsedFont {
    fn from_static(bytes: &'static [u8]) -> Result<Self, FaceError> {
        Ok(Self {
            face: Face::parse(bytes)?,
            outlines: FontOutlines::from_static(bytes)?,
        })
    }
}

struct FontSetInner {
    regular: ParsedFont,
    bold: ParsedFont,
    italic: ParsedFont,
    mono: ParsedFont,
    body_px: u16,
    cache: Mutex<BTreeMap<(FontVariant, u16, u16), Arc<GlyphBitmap>>>,
}

/// Embedded proportional & monospace OpenType font set (`Inter-Regular`, `Inter-Bold`,
/// `Inter-Italic`, `JetBrainsMono-Regular`) providing both [`Shaper`] geometry and A8 glyph
/// rasterization from the exact same metrics.
#[derive(Clone)]
pub struct FontSet(Arc<FontSetInner>);

impl FontSet {
    /// Parses all four embedded OpenType faces at `body_px` CSS pixels (e.g. `15` for `quiet-md`).
    ///
    /// # Errors
    ///
    /// Returns [`FaceError`] only if the compiled-in font assets fail SFNT parsing.
    pub fn embedded(body_px: u16) -> Result<Self, FaceError> {
        Ok(Self(Arc::new(FontSetInner {
            regular: ParsedFont::from_static(INTER_REGULAR_TTF)?,
            bold: ParsedFont::from_static(INTER_BOLD_TTF)?,
            italic: ParsedFont::from_static(INTER_ITALIC_TTF)?,
            mono: ParsedFont::from_static(JETBRAINS_MONO_TTF)?,
            body_px: body_px.max(1),
            cache: Mutex::new(BTreeMap::new()),
        })))
    }

    /// Configured body pixel size (`15` for `UI-SPEC.md` `quiet-md`).
    #[must_use]
    pub fn body_px(&self) -> u16 {
        self.0.body_px
    }

    /// Picks the primary font face and character-fallback variant for `style` + `ch`.
    #[must_use]
    pub fn resolve_variant(&self, style: Style, ch: char) -> (FontVariant, u16) {
        let preferred = match style.family() {
            FontFamily::Mono => FontVariant::Mono,
            FontFamily::Sans | FontFamily::Serif => {
                if style.is_bold() {
                    FontVariant::Bold
                } else if style.is_italic() {
                    FontVariant::Italic
                } else {
                    FontVariant::Regular
                }
            }
        };
        if let Some(gid) = self.font(preferred).face.glyph(ch) {
            return (preferred, gid);
        }
        if preferred != FontVariant::Regular {
            if let Some(gid) = self.font(FontVariant::Regular).face.glyph(ch) {
                return (FontVariant::Regular, gid);
            }
        }
        (preferred, 0)
    }

    /// Returns the parsed [`Face`] for `variant`.
    #[must_use]
    pub fn face(&self, variant: FontVariant) -> &Face {
        &self.font(variant).face
    }

    /// Scales font design `units` for `variant` to logical units at `style`.
    #[must_use]
    pub fn scale_units(&self, variant: FontVariant, units: i32, style: Style) -> Fixed {
        self.font(variant)
            .face
            .scaled_signed(units, self.scaled_style(style))
    }

    /// Picks the best single [`FontVariant`] covering a cluster.
    fn cluster_variant(&self, style: Style, cluster: &str) -> FontVariant {
        let mut chosen = match style.family() {
            FontFamily::Mono => FontVariant::Mono,
            FontFamily::Sans | FontFamily::Serif => {
                if style.is_bold() {
                    FontVariant::Bold
                } else if style.is_italic() {
                    FontVariant::Italic
                } else {
                    FontVariant::Regular
                }
            }
        };
        for ch in cluster.chars() {
            if ch == '\n' || ch == '\r' || ch.is_control() {
                continue;
            }
            if self.font(chosen).face.glyph(ch).is_none()
                && self.font(FontVariant::Regular).face.glyph(ch).is_some()
            {
                chosen = FontVariant::Regular;
                break;
            }
        }
        chosen
    }

    /// Shapes a contiguous run of `clusters` at `style` using each face's OpenType
    /// `GDEF`/`GSUB`/`GPOS` tables, returning `(variant, glyph)` pairs in run order.
    #[must_use]
    pub fn shape_run(&self, clusters: &[&str], style: Style) -> Vec<(FontVariant, ShapedGlyph)> {
        if clusters.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(clusters.len());
        let mut i = 0usize;
        while i < clusters.len() {
            let v = self.cluster_variant(style, clusters[i]);
            let start = i;
            i += 1;
            while i < clusters.len() && self.cluster_variant(style, clusters[i]) == v {
                i += 1;
            }
            for mut g in self.font(v).face.shape(&clusters[start..i]) {
                g.cluster += start;
                out.push((v, g));
            }
        }
        out
    }

    /// Extracts a glyph outline in logical coordinates for a native vector renderer.
    ///
    /// `device_scale` controls curve flattening accuracy only: layout advances and the returned
    /// outline remain logical. This lets a GPU rasterize directly at the current monitor scale
    /// without sending document strings into a framework's text cache.
    ///
    /// # Errors
    ///
    /// Returns [`FaceError`] if the embedded glyph outline cannot be decoded.
    pub fn vector_glyph(
        &self,
        variant: FontVariant,
        gid: u16,
        style: Style,
        device_scale: f32,
    ) -> Result<Option<GlyphOutline>, FaceError> {
        let font = self.font(variant);
        let logical_px = style.size_milli() as f32 * f32::from(self.0.body_px) / 1_000.0;
        let device_scale = if device_scale.is_finite() {
            device_scale.clamp(0.5, 8.0)
        } else {
            1.0
        };
        let Some(mut outline) =
            font.outlines
                .outline(gid, font.face.units_per_em(), logical_px * device_scale)?
        else {
            return Ok(None);
        };
        let factor = logical_px / f32::from(font.face.units_per_em());
        for contour in &mut outline.contours {
            for (x, y) in contour {
                *x *= factor;
                *y *= factor;
            }
        }
        Ok(Some(outline))
    }

    /// Returns the cached or newly rasterized A8 bitmap for `ch` at `style` and device scale
    /// `(zoom_milli, dpi)`.
    #[must_use]
    pub fn rasterize_char(
        &self,
        style: Style,
        zoom_milli: i64,
        dpi: i64,
        ch: char,
    ) -> Option<Arc<GlyphBitmap>> {
        if ch.is_control() {
            return None;
        }
        let (variant, gid) = self.resolve_variant(style, ch);
        self.rasterize_glyph(variant, gid, style, zoom_milli, dpi)
    }

    /// Returns the cached or newly rasterized A8 bitmap for an already-shaped `(variant, gid)` at
    /// `style` and device scale `(zoom_milli, dpi)`.
    #[must_use]
    pub fn rasterize_glyph(
        &self,
        variant: FontVariant,
        gid: u16,
        style: Style,
        zoom_milli: i64,
        dpi: i64,
    ) -> Option<Arc<GlyphBitmap>> {
        if gid == 0 {
            return None;
        }
        let px_size = (style.size_milli() as f32 * f32::from(self.0.body_px) / 1_000.0)
            * ((zoom_milli.max(100) as f32) * (dpi.max(48) as f32) / 96_000.0);
        let px_quarter = (px_size * 4.0).round().clamp(4.0, 2_400.0) as u16;
        let key = (variant, gid, px_quarter);

        if let Some(hit) = self
            .0
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
        {
            return Some(Arc::clone(hit));
        }

        let font = self.font(variant);
        let outline = font
            .outlines
            .outline(gid, font.face.units_per_em(), px_size)
            .ok()??;
        let bmp = Arc::new(rasterize_outline(
            &outline,
            font.face.units_per_em(),
            f32::from(px_quarter) / 4.0,
        ));
        if bmp.width == 0 || bmp.height == 0 {
            return None;
        }
        let mut cache = self
            .0
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if cache.len() >= MAX_GLYPH_CACHE_ENTRIES {
            cache.clear();
        }
        cache.insert(key, Arc::clone(&bmp));
        Some(bmp)
    }

    fn font(&self, variant: FontVariant) -> &ParsedFont {
        match variant {
            FontVariant::Regular => &self.0.regular,
            FontVariant::Bold => &self.0.bold,
            FontVariant::Italic => &self.0.italic,
            FontVariant::Mono => &self.0.mono,
        }
    }

    fn style_face(&self, style: Style) -> &Face {
        let variant = match style.family() {
            FontFamily::Mono => FontVariant::Mono,
            FontFamily::Sans | FontFamily::Serif => {
                if style.is_bold() {
                    FontVariant::Bold
                } else if style.is_italic() {
                    FontVariant::Italic
                } else {
                    FontVariant::Regular
                }
            }
        };
        &self.font(variant).face
    }

    fn scaled_style(&self, style: Style) -> Style {
        Style::at(style.size_milli() * i64::from(self.0.body_px))
    }
}

impl Shaper for FontSet {
    fn advance(&self, cluster: &str, style: Style) -> Fixed {
        if Face::is_break(cluster) {
            return Fixed::ZERO;
        }
        let effective = self.scaled_style(style);
        let mut chars = cluster.chars();
        let Some(first) = chars.next() else {
            return Fixed::ZERO;
        };
        if chars.next().is_none() && (first as u32) < 0x0600 {
            let (variant, gid) = self.resolve_variant(style, first);
            let face = &self.font(variant).face;
            let adv = if gid != 0 {
                i64::from(face.advance_units(first))
            } else {
                i64::from(self.0.regular.face.advance_units(first))
            };
            return face.scaled(adv, effective);
        }
        let variant = self.cluster_variant(style, cluster);
        self.font(variant).face.advance(cluster, effective)
    }

    fn shape_clusters(&self, clusters: &[&str], style: Style, out: &mut Vec<Fixed>) {
        out.clear();
        if clusters.is_empty() {
            return;
        }
        out.reserve(clusters.len());
        let effective = self.scaled_style(style);
        let mut sub_out = Vec::new();
        let mut i = 0usize;
        while i < clusters.len() {
            let v = self.cluster_variant(style, clusters[i]);
            let start = i;
            i += 1;
            while i < clusters.len() && self.cluster_variant(style, clusters[i]) == v {
                i += 1;
            }
            self.font(v)
                .face
                .shape_clusters(&clusters[start..i], effective, &mut sub_out);
            out.extend_from_slice(&sub_out);
        }
    }

    fn line_height(&self, style: Style) -> Fixed {
        let face = self.style_face(style);
        let design = (i64::from(face.ascender()) - i64::from(face.descender())) * 121 / 100;
        face.scaled(design, self.scaled_style(style))
    }

    fn ascent(&self, style: Style) -> Fixed {
        let face = self.style_face(style);
        face.scaled(i64::from(face.ascender()), self.scaled_style(style))
    }
}
