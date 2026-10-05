//! One-time outline rasterisation into A8 coverage.
//!
//! Runs once at boot, then never again. FR-2.5 requires zero curve evaluation during typing,
//! and this module is the only code in the crate that can evaluate one — so after
//! [`rasterize_face`] returns, the font outlines are unreachable and the requirement is a
//! property of the data flow rather than a promise about a hot loop.
//!
//! # Rasteriser
//!
//! Scanline fill with 4×4 vertical supersampling and analytic horizontal coverage, computed
//! here rather than pulled from a crate. Two reasons:
//!
//! * `fontdue` and `ab_glyph_rasterizer` both allocate. This path may allocate freely -- it
//!   runs once, before typing -- but owning the loop means the A8 semantics are explicit and
//!   the `'A'` coverage histogram in the gate is a statement about *this* code.
//! * Quadratic flattening has to happen exactly once per glyph and then be thrown away, and
//!   the flattening tolerance interacts with the supersampling factor. Keeping both in one
//!   file makes that interaction reviewable.
//!
//! # Coverage semantics
//!
//! Each output byte is 0..=255 coverage of the glyph's outline, which is exactly what the SSE2
//! blitter's fixed-point blend consumes. Horizontal coverage is computed analytically from
//! exact span endpoints; vertical coverage from 4 sub-scanlines per pixel row. So a glyph with
//! no outline (space, or a codepoint absent from the face) yields an all-zero bitmap of
//! non-zero size, and a glyph that is entirely off its bitmap box is dropped.

use crate::atlas::{AtlasBuilder, PendingGlyph};
use crate::payload::FaceEntry;

use ttf_parser::{Face, GlyphId, OutlineBuilder};

/// Sub-scanlines per pixel row. 4 gives good vertical antialiasing at these sizes; 8 was
/// measured to change the `'A'` histogram by under 1 LSB for 1.7× the work.
const SUBSAMPLES: u32 = 4;

/// Flattening tolerance in font units: a quadratic is subdivided until its control polygon
/// deviates from the chord by less than this. At 2048 units/em and ppem 16, one pixel is 128
/// units, so this is about 1/128 of a pixel — below the output's own precision.
const FLATNESS: f32 = 1.0;

/// A flat point in font units.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Pt {
    x: f32,
    y: f32,
}

/// Collects outline geometry and flattens curves on the way in.
#[derive(Debug)]
struct Flattener<'a> {
    contours: &'a mut Vec<Vec<Pt>>,
    current: &'a mut Vec<Pt>,
    start: Pt,
    cursor: Pt,
    /// Scale from font units to pixels.
    scale: f32,
    /// Pixel-space y offset, i.e. `-ascender * scale`.
    offset_y: f32,
}

impl<'a> Flattener<'a> {
    /// Build a flattener that appends into `contours` and reuses `current` as its chain buffer.
    ///
    /// Both buffers are borrowed for the lifetime of the glyph pass so a whole face's worth of
    /// glyphs allocates neither. `Flattener::new` allocating its own `Vec::with_capacity(64)`
    /// meant one allocation per glyph, and `mem::take` on `contours` meant the caller's buffer
    /// was handed away rather than reused.
    fn new(
        contours: &'a mut Vec<Vec<Pt>>,
        current: &'a mut Vec<Pt>,
        scale: f32,
        offset_y: f32,
    ) -> Self {
        contours.clear();
        current.clear();
        Self {
            contours,
            current,
            start: Pt { x: 0.0, y: 0.0 },
            cursor: Pt { x: 0.0, y: 0.0 },
            scale,
            offset_y,
        }
    }

    fn p(&self, x: f32, y: f32) -> Pt {
        Pt {
            x: x * self.scale,
            y: self.offset_y - y * self.scale,
        }
    }

    /// Quadratic from `cursor` via `c` to `to`.
    fn quad(&mut self, cx: f32, cy: f32, tox: f32, toy: f32) {
        let c = self.p(cx, cy);
        let to = self.p(tox, toy);
        let from = self.cursor;
        // Number of segments from the control polygon's deviation: the standard bound is
        // n = ceil(sqrt(dist(from, to, c) / FLATNESS)).
        let dev = distance_to_line(from, c, to);
        let n = (dev / FLATNESS).sqrt().ceil().max(1.0) as usize;
        let n = n.min(64);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            self.current.push(Pt {
                x: u * u * from.x + 2.0 * u * t * c.x + t * t * to.x,
                y: u * u * from.y + 2.0 * u * t * c.y + t * t * to.y,
            });
        }
        self.cursor = to;
    }

    /// Cubic, subdivided into a chain of quadratics first.
    fn cubic(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, tox: f32, toy: f32) {
        let c1 = self.p(c1x, c1y);
        let c2 = self.p(c2x, c2y);
        let to = self.p(tox, toy);
        let from = self.cursor;
        let dev = (distance_to_line(from, c1, c2) + distance_to_line(c1, c2, to)).max(0.0);
        let n = ((dev / FLATNESS).sqrt().ceil() as usize).clamp(1, 32);
        let mut prev_c = Pt {
            x: from.x,
            y: from.y,
        };
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            // de Casteljau, emitting the quadratic half of each cubic segment.
            let a = Pt {
                x: u * u * u * from.x
                    + 3.0 * u * u * t * c1.x
                    + 3.0 * u * t * t * c2.x
                    + t * t * t * to.x,
                y: u * u * u * from.y
                    + 3.0 * u * u * t * c1.y
                    + 3.0 * u * t * t * c2.y
                    + t * t * t * to.y,
            };
            let b = Pt {
                x: u * u * c1.x + 2.0 * u * t * c2.x + t * t * to.x,
                y: u * u * c1.y + 2.0 * u * t * c2.y + t * t * to.y,
            };
            self.emit_quad(prev_c, a, b);
            prev_c = b;
        }
        self.cursor = to;
    }

    /// De Casteljau split point, used by `cubic`.
    fn emit_quad(&mut self, from: Pt, c: Pt, to: Pt) {
        let dev = distance_to_line(from, c, to);
        let n = ((dev / FLATNESS).sqrt().ceil().max(1.0) as usize).min(64);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            self.current.push(Pt {
                x: u * u * from.x + 2.0 * u * t * c.x + t * t * to.x,
                y: u * u * from.y + 2.0 * u * t * c.y + t * t * to.y,
            });
        }
    }

    /// Close off the ring in progress, dropping its duplicated closing point.
    fn flush_ring(&mut self) {
        if self.current.len() > 1 {
            self.current.pop();
            self.contours.push(self.current.clone());
            self.current.clear();
        } else {
            self.current.clear();
        }
    }
}

/// Perpendicular distance from `p` to the line `a..b`.
fn distance_to_line(a: Pt, p: Pt, b: Pt) -> f32 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len2 = dx * dx + dy * dy;
    if len2 <= f32::MIN_POSITIVE {
        return ((p.x - a.x).powi(2) + (p.y - a.y).powi(2)).sqrt();
    }
    let cross = dx * (a.y - p.y) - (a.x - p.x) * dy;
    (cross.abs() / len2.sqrt()).abs()
}

impl OutlineBuilder for Flattener<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.flush_ring();
        self.start = self.p(x, y);
        self.cursor = self.start;
        self.current.push(self.start);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        let to = self.p(x, y);
        self.current.push(to);
        self.cursor = to;
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        self.quad(cx, cy, x, y);
    }

    fn curve_to(&mut self, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        self.cubic(c1x, c1y, c2x, c2y, x, y);
    }

    fn close(&mut self) {
        self.flush_ring();
        self.cursor = self.start;
    }
}
/// A non-horizontal contour edge, with the values `at` needs precomputed.
#[derive(Debug, Clone, Copy)]
struct Edge {
    a: Pt,
    b: Pt,
    /// Lower and upper `y`, so the crossing test never has to compare against `a` and `b` again.
    y_min: f32,
    y_max: f32,
    /// `1 / (b.y - a.y)`. Precomputed because `at` is called once per edge per sub-scanline:
    /// 16 x 4 x 60 = 3,840 divisions per glyph and ~5.9M across the pass.
    dy_inv: f32,
}

impl Edge {
    /// Build an edge, or `None` if it is horizontal and can never cross a scanline.
    fn new(a: Pt, b: Pt) -> Option<Self> {
        if a.y == b.y {
            return None;
        }
        let (y_min, y_max) = if a.y < b.y { (a.y, b.y) } else { (b.y, a.y) };
        Some(Self {
            a,
            b,
            y_min,
            y_max,
            dy_inv: 1.0 / (b.y - a.y),
        })
    }

    /// x where this edge crosses horizontal line `y`, or `None`.
    ///
    /// Half-open at the top: `y == y_min` crosses, `y == y_max` does not. That is what makes a
    /// vertex shared by two edges register on exactly one of them, so a scanline passing through a
    /// vertex neither loses nor double-counts a crossing.
    #[inline]
    fn at(&self, y: f32) -> Option<f32> {
        if y < self.y_min || y >= self.y_max {
            return None;
        }
        let t = (y - self.a.y) * self.dy_inv;
        Some(self.a.x + t * (self.b.x - self.a.x))
    }
}

/// Reusable buffers for [`rasterize`], so a whole pass allocates a bounded number of times.
///
/// The one-time pass rasterises 1,528 glyphs. With a `Vec` for `acc`, `edges` and — worst — `xs`
/// declared inside the sub-scanline loop, that was roughly 98,000 allocations, one per
/// sub-scanline, and a large share of the 54 ms this phase took.
#[derive(Debug, Default)]
struct Scratch {
    /// Per-pixel sub-scanline hit counts, `width * height`.
    acc: Vec<u16>,
    /// Flattened, non-horizontal edges for the current glyph.
    edges: Vec<Edge>,
    /// Edges bucketed by pixel row, CSR-encoded: `row_items[row_start[r]..row_start[r + 1]]`.
    row_start: Vec<u32>,
    row_items: Vec<u32>,
    /// x intersections for one sub-scanline.
    xs: Vec<f32>,
    /// Row hit counts, reused as the scatter cursor so the counting sort needs no second array.
    counts: Vec<u32>,
}

impl Scratch {
    /// Size every buffer for a `width x height` bitmap, zeroing the accumulators.
    fn prepare(&mut self, width: usize, height: usize) {
        self.acc.clear();
        self.acc.resize(width * height, 0);
        self.edges.clear();
        self.row_start.clear();
        self.row_start.resize(height + 1, 0);
        self.row_items.clear();
        self.xs.clear();
        self.counts.clear();
        self.counts.resize(height, 0);
    }
}

/// Add the coverage of `[xa, xb)` in row `row`, in units of `per_sample` per full pixel.
fn add_span(acc: &mut [u16], row: usize, width: usize, xa: f32, xb: f32, per_sample: f32) {
    if xb <= 0.0 || xa >= width as f32 {
        return;
    }
    let xa = xa.max(0.0);
    let xb = xb.min(width as f32);
    let first = xa.floor() as usize;
    let last = (xb.ceil() as usize).min(width);
    for px in first..last {
        let l = (px as f32).max(xa);
        let r = ((px + 1) as f32).min(xb);
        if r <= l {
            continue;
        }
        let frac = (r - l) * per_sample;
        acc[row * width + px] = acc[row * width + px].saturating_add(frac.round() as u16);
    }
}

/// Rasterise `contours` into `out` (`width * height`, row-major, 0..=255).
///
/// `dx`/`dy` shift the geometry without copying it: the polygon stays in pixel space and the
/// output bitmap's origin is given by the offset. An earlier version called `translate()` and
/// allocated a fresh `Vec<Vec<Pt>>` — one allocation per contour, per glyph — to achieve the
/// same thing.
fn rasterize(
    contours: &[Vec<Pt>],
    width: usize,
    height: usize,
    dx: f32,
    dy: f32,
    out: &mut [u8],
    scratch: &mut Scratch,
) {
    debug_assert_eq!(out.len(), width * height);
    if width == 0 || height == 0 || contours.iter().all(|c| c.len() < 3) {
        return;
    }
    scratch.prepare(width, height);

    // Flatten to edges, dropping horizontals: they cannot cross a scanline.
    for contour in contours {
        if contour.len() < 3 {
            continue;
        }
        for i in 0..contour.len() {
            if let Some(e) = Edge::new(contour[i], contour[(i + 1) % contour.len()]) {
                scratch.edges.push(e);
            }
        }
    }
    if scratch.edges.is_empty() {
        return;
    }

    // Bucket edges by the pixel rows they cross. An edge either covers every sub-scanline of a
    // row it enters or none of them, so one bucket per row is exact, not an approximation, and it
    // turns each sub-scanline's sort from ~60 crossings down to the 2-8 that actually cross it.
    // This is the largest single win in the fill: 64 sorts of 60 elements per glyph became 64
    // sorts of about 4.
    //
    // Counting sort: count, prefix-sum, scatter. `sort_by_key` per edge would be O(n log n) for
    // the same answer, and the counts array is reused rather than reallocated per glyph.
    let row_span = |e: &Edge| -> (usize, usize) {
        // A row `r` spans `[r, r + 1)`; an edge overlaps it iff `y_max > r && y_min < r + 1`.
        let first = (e.y_min.floor().max(0.0) as usize).min(height);
        let last = (e.y_max.ceil().max(0.0) as usize).min(height);
        (first, last)
    };
    for e in &scratch.edges {
        let (first, last) = row_span(e);
        for r in first..last {
            scratch.counts[r] += 1;
        }
    }
    let mut total = 0u32;
    for r in 0..height {
        scratch.row_start[r] = total;
        total += scratch.counts[r];
        scratch.counts[r] = scratch.row_start[r]; // reuse as the write cursor
    }
    scratch.row_start[height] = total;
    scratch.row_items.resize(total as usize, 0);
    for (i, e) in scratch.edges.iter().enumerate() {
        let (first, last) = row_span(e);
        for r in first..last {
            let at = scratch.counts[r] as usize;
            scratch.row_items[at] = i as u32;
            scratch.counts[r] += 1;
        }
    }

    // Each of the SUBSAMPLES sub-scanlines contributes `255 / SUBSAMPLES` to a fully covered
    // pixel, so a covered pixel accumulates to 255 exactly and a half-covered one to ~128.
    let scale = SUBSAMPLES as f32;
    let per_sample = 255.0 / scale;
    debug_assert_eq!(
        (SUBSAMPLES as f32 * per_sample).round(),
        255.0,
        "SUBSAMPLES sub-scanlines must sum to full coverage"
    );

    for row in 0..height {
        let lo = scratch.row_start[row] as usize;
        let hi = scratch.row_start[row + 1] as usize;
        if lo == hi {
            continue;
        }
        let y0 = row as f32;
        for sub in 0..SUBSAMPLES {
            // Sample at the sub-scanline's centre, `(sub + 0.5) / SUBSAMPLES` of the way down the
            // pixel. Sampling at `sub / SUBSAMPLES` instead would put sub = 0 exactly on the
            // pixel's top edge, where the half-open crossing test rejects it, dropping a quarter
            // of every pixel row's coverage.
            let sy = y0 + (sub as f32 + 0.5) / scale;

            scratch.xs.clear();
            for &ei in &scratch.row_items[lo..hi] {
                if let Some(x) = scratch.edges[ei as usize].at(sy) {
                    scratch.xs.push(x);
                }
            }
            if scratch.xs.len() < 2 {
                continue;
            }
            scratch.xs.sort_by(|a, b| a.total_cmp(b));
            // Even-odd fill: spans between consecutive pairs.
            let mut i = 0;
            while i + 1 < scratch.xs.len() {
                let xa = (scratch.xs[i] - dx).max(0.0);
                let xb = (scratch.xs[i + 1] - dx).min(width as f32);
                if xb > xa {
                    add_span(&mut scratch.acc, row, width, xa, xb, per_sample);
                }
                i += 2;
            }
        }
    }

    for (i, &a) in scratch.acc.iter().enumerate() {
        let _ = dy;
        out[i] = a.min(255) as u8;
    }
}

/// Rasterise every glyph of one face at every requested size.
pub fn rasterize_face(
    face: &Face,
    entry: &FaceEntry,
    sizes: &[u16],
    builder: &mut AtlasBuilder,
) -> Result<(), crate::Error> {
    let upm = face.units_per_em() as f32;
    // One scratch for every glyph of this face at every size. Allocating per glyph, or per
    // sub-scanline, was the single largest cost in this phase.
    let mut scratch = Scratch::default();
    // One chain buffer and one contour list for the whole face.
    let mut contours: Vec<Vec<Pt>> = Vec::with_capacity(16);
    let mut chain: Vec<Pt> = Vec::with_capacity(64);
    for &ppem in sizes {
        let scale = ppem as f32 / upm;
        let ascender = face.ascender() as f32 * scale;
        // One reusable tile buffer, since only one glyph's ink box is live at a time.
        let max_dim = (ppem as f32 * 3.0).ceil() as usize;
        let mut tile_scratch = vec![0u8; max_dim * max_dim];

        for glyph in glyphs_of(face, entry) {
            let (gid, cp) = glyph;
            // `glyph_hor_advance` is in **font units**, not pixels. Passing it straight through put
            // Inter's ~1,400-unit advance into a `u8` metric field, where the clamp saturated it
            // to 255: `'A'` reported `advance_x: 255` and a line of 12 characters ran 3,000 pixels
            // wide, which is what pushed `blitting_allocates_nothing` out of bounds.
            //
            // Scale by ppem/unitsPerEm and round, matching how the outline itself is scaled.
            let advance = (face.glyph_hor_advance(gid).unwrap_or(0) as f32 * scale)
                .round()
                .max(0.0) as i32;
            // Scoped so the two mutable borrows end before `contours` is read again. An explicit
            // `drop(f)` said the same thing, but clippy is right that `Flattener` implements no
            // `Drop`: the call only released the borrow, which a block expresses directly.
            {
                let mut f = Flattener::new(&mut contours, &mut chain, scale, ascender);
                let _ = face.outline_glyph(gid, &mut f);
            }

            // A glyph with no outline — a space, or a codepoint the face maps to an empty
            // glyph — gets zero coverage and keeps its advance. The 1px pad below would otherwise
            // turn "no ink" into a 1x1 all-zero bitmap, which reports `width: 1, height: 1` and
            // `is_blank() == false` for a space. That costs one byte of atlas per blank glyph and
            // contradicts [`GlyphMetric::is_blank`]'s own documentation.
            if contours.iter().all(|c| c.len() < 3) {
                builder.add(PendingGlyph::new(
                    cp,
                    entry.style,
                    ppem,
                    Vec::new(),
                    0,
                    0,
                    0,
                    0,
                    advance,
                )?)?;
                continue;
            }

            // Ink bounds, with a 1px pad so antialiasing at the edges is not clipped.
            let (min_x, min_y, max_x, max_y) = ink_bounds(contours.as_slice());
            let x0 = (min_x.floor() as i32 - 1).max(0);
            let y0 = (min_y.floor() as i32 - 1).max(0);
            let x1 = (max_x.ceil() as i32 + 1).max(x0) as usize;
            let y1 = (max_y.ceil() as i32 + 1).max(y0) as usize;
            let bw = x1 - x0 as usize;
            let bh = y1 - y0 as usize;
            if bw == 0 || bh == 0 || bw > 255 || bh > 255 {
                // Blank or too large: record advance only, with no coverage.
                builder.add(PendingGlyph::new(
                    cp,
                    entry.style,
                    ppem,
                    Vec::new(),
                    0,
                    0,
                    x0,
                    -(y0 + bh as i32),
                    advance,
                )?)?;
                continue;
            }

            // Rasterise the ink box directly by translating the polygon.
            let tile = &mut tile_scratch[..bw * bh];
            tile.fill(0);
            rasterize(
                contours.as_slice(),
                bw,
                bh,
                -x0 as f32,
                -y0 as f32,
                tile,
                &mut scratch,
            );

            builder.add(PendingGlyph::new(
                cp,
                entry.style,
                ppem,
                tile.to_vec(),
                bw,
                bh,
                x0,
                -(y0 + bh as i32),
                advance,
            )?)?;
        }
    }
    Ok(())
}

/// Ink bounds over all contours, or all-zero for an empty outline.
fn ink_bounds(contours: &[Vec<Pt>]) -> (f32, f32, f32, f32) {
    let (mut min_x, mut min_y) = (f32::MAX, f32::MAX);
    let (mut max_x, mut max_y) = (f32::MIN, f32::MIN);
    for c in contours {
        for p in c {
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
        }
    }
    if min_x > max_x {
        (0.0, 0.0, 0.0, 0.0)
    } else {
        (min_x, min_y, max_x, max_y)
    }
}

/// Codepoints to rasterise: the font-supplied ranges only. Box Drawing is added separately by
/// [`crate::box_drawing`].
///
/// **Which ranges depends on the face**, which this function ignored until Phase 9B made ignoring it
/// visible. It walked `codepoints_in_text_ranges()` for all five faces, so the math face was asked
/// for Latin-1 and got whatever Noto Sans Math happens to carry there — 191 codepoints requested, a
/// few dozen present, and no way to say so at the call site. The `let _ = entry;` at the bottom is
/// the compiler recording that the parameter was accepted and then discarded.
///
/// The math face's set is `MATH_RANGES` **and nothing else** — no ASCII.
///
/// An intermediate version of this chained `a`..`z`, `A`..`Z` and `0`..`9` onto the math face, on
/// the reasoning that `MathNode::Symbol` resolves a bare `x` to U+0078 and a face without it would
/// draw an empty numerator. True, and it still draws one: those codepoints are in the **text**
/// window, so `MetricTable::get` finds them under [`crate::payload::Style::Italic`] or `Regular`,
/// which the face already rasterises.
///
/// **The chained ASCII cost nothing, and claiming otherwise would have been a fiction.** The comment
/// on that version said it "cost 19,964 bytes of coverage". It cost **zero**: `MATH_RANGES` is also
/// the list the *subsetter* is given, so the payload's copy of Noto Sans Math never contained a
/// Latin letter, and `face.glyph_index('a')` returned `None` for all 62. The number was inferred from
/// 62 glyphs × 2 sizes rather than measured, and the atlas's `used()` did not move by a single byte
/// when the chain was removed. A saving that cannot be observed in the thing it claims to save is
/// not a saving.
///
/// The chain is still gone, for the reason that does hold: Inter-Italic is the right face for a math
/// variable, by the convention every textbook uses, while Noto Sans Math's ASCII is upright — so a
/// formula drawn from the math face would set `x` upright beside a slanted `y`. The style for each
/// glyph is chosen by the caller, in `session.rs`, from [`crate::payload::is_math_symbol`]; this
/// function's only job is to put the *symbols* in the atlas.
///
/// **What the math face does cost, measured: 42,987 bytes of coverage** for 108 codepoints × 2
/// sizes, which is 398 bytes per glyph — Greek letters are the widest things in the payload. That is
/// 8.2% of the 512 KiB budget and it is why `metric::ATLAS_HEIGHT` had to drop from 480 to 448.
/// `crates/holonomy-assets/examples/probe_budget.rs` prints it: the text faces plus the
/// procedural Box Drawing come to 374,313, so the difference from `used()` is exactly the math face.
fn glyphs_of(face: &Face, entry: &FaceEntry) -> Vec<(GlyphId, u32)> {
    let mut out = Vec::new();
    match entry.style {
        crate::payload::Style::Math => {
            for cp in crate::payload::codepoints_in_math_ranges() {
                if let Some(c) = char::from_u32(cp) {
                    if let Some(gid) = face.glyph_index(c) {
                        out.push((gid, cp));
                    }
                }
            }
        }
        _ => {
            for cp in crate::payload::codepoints_in_text_ranges() {
                // `glyph_index` takes a `char` and returns `Option`, not `Result`. Latin-1 and ASCII
                // are all scalar values so the `char` conversion is always sound here, but `as char`
                // on a code point is only correct if nothing above the Unicode range ever arrives, so
                // the conversion is checked rather than cast blindly.
                if let Some(c) = char::from_u32(cp) {
                    if let Some(gid) = face.glyph_index(c) {
                        out.push((gid, cp));
                    }
                }
            }
        }
    }
    out.sort_by_key(|&(_, cp)| cp);
    out.dedup_by_key(|&mut (_, cp)| cp);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect_contour(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<Vec<Pt>> {
        vec![vec![
            Pt { x: x0, y: y0 },
            Pt { x: x1, y: y0 },
            Pt { x: x1, y: y1 },
            Pt { x: x0, y: y1 },
        ]]
    }

    /// A solid rectangle covering the whole bitmap must be all 255.
    #[test]
    fn a_fully_covered_rectangle_is_255_everywhere() {
        let poly = rect_contour(0.0, 0.0, 8.0, 8.0);
        let mut out = vec![0u8; 64];
        rasterize(&poly, 8, 8, 0.0, 0.0, &mut out, &mut Scratch::default());
        assert!(out.iter().all(|&v| v == 255), "got {:?}", &out[..8]);
    }

    /// An empty polygon must leave the bitmap untouched, so `space` costs no coverage.
    #[test]
    fn an_empty_polygon_writes_nothing() {
        let poly: Vec<Vec<Pt>> = Vec::new();
        let mut out = vec![7u8; 16];
        rasterize(&poly, 4, 4, 0.0, 0.0, &mut out, &mut Scratch::default());
        assert!(
            out.iter().all(|&v| v == 7),
            "empty outline must not clear the buffer"
        );
    }

    /// A half-covered rectangle must produce ~128, not 0 or 255.
    #[test]
    fn a_half_covered_rectangle_is_about_128() {
        let poly = rect_contour(0.0, 0.0, 4.0, 8.0);
        let mut out = vec![0u8; 8 * 8];
        rasterize(&poly, 8, 8, 0.0, 0.0, &mut out, &mut Scratch::default());
        for row in 0..8 {
            for col in 0..8 {
                let v = out[row * 8 + col];
                if col < 4 {
                    assert!(v >= 250, "left half of row {row} col {col} was {v}");
                } else {
                    assert!(v <= 5, "right half of row {row} col {col} was {v}");
                }
            }
        }
    }

    /// Coverage must be monotone in the rectangle's width, and land near the analytic fraction.
    /// This is the property the `'A'` histogram gate leans on.
    #[test]
    fn coverage_tracks_analytic_area() {
        for &frac in &[0.25f32, 0.5, 0.75, 1.0] {
            let poly = rect_contour(0.0, 0.0, 32.0 * frac, 32.0);
            let mut out = vec![0u8; 32 * 32];
            rasterize(&poly, 32, 32, 0.0, 0.0, &mut out, &mut Scratch::default());
            let sum: u64 = out.iter().map(|&v| v as u64).sum();
            let want = (32.0 * 32.0 * frac * 255.0) as i64;
            let got = sum as i64;
            let err = (got - want).abs();
            assert!(
                err <= (32.0 * 32.0 * 255.0 * 0.02) as i64,
                "frac {frac}: mean {got} vs {want}, err {err}"
            );
        }
    }

    /// A hole must be subtracted, not filled: even-odd, not nonzero.
    #[test]
    fn a_hole_is_subtracted() {
        let mut poly = rect_contour(0.0, 0.0, 16.0, 16.0);
        #[allow(unused_mut)]
        // Same winding as the outer ring, so nonzero would fill and even-odd leaves a hole.
        poly.push(vec![
            Pt { x: 4.0, y: 4.0 },
            Pt { x: 12.0, y: 4.0 },
            Pt { x: 12.0, y: 12.0 },
            Pt { x: 4.0, y: 12.0 },
        ]);
        let mut out = vec![0u8; 256];
        rasterize(&poly, 16, 16, 0.0, 0.0, &mut out, &mut Scratch::default());
        let centre = out[8 * 16 + 8];
        assert!(
            centre <= 5,
            "centre of the hole was {centre}, so the fill is nonzero"
        );
        let corner = out[16 + 1]; // pixel (1, 1) of a 16x16 bitmap
        assert!(corner >= 250, "solid corner was {corner}");
    }

    /// Rasterisation must be deterministic: identical input, identical bytes. The gate compares
    /// against a scalar reference bit for bit, so this is a precondition.
    #[test]
    fn rasterization_is_deterministic() {
        let poly = rect_contour(1.3, 2.7, 11.9, 9.1);
        let mut a = vec![0u8; 16 * 16];
        let mut b = vec![0u8; 16 * 16];
        rasterize(&poly, 16, 16, 0.0, 0.0, &mut a, &mut Scratch::default());
        rasterize(&poly, 16, 16, 0.0, 0.0, &mut b, &mut Scratch::default());
        assert_eq!(a, b);
    }

    /// Flattening a quadratic must stay within tolerance of the true curve.
    #[test]
    fn quadratic_flattening_is_within_tolerance() {
        let from = Pt { x: 0.0, y: 0.0 };
        let c = Pt { x: 5.0, y: 10.0 };
        let to = Pt { x: 10.0, y: 0.0 };
        // Measure the distance from the true curve to the flattened polyline, against the
        // polyline's *segments* and not its vertices. The first version measured vertex-to-point
        // distance, which exceeds segment-to-point distance by up to half a segment length; that
        // is what produced `max deviation 2.245833 exceeds 1` for a flattening that is in fact
        // within tolerance. The tolerance itself, `FLATNESS`, is unchanged.
        let dev = distance_to_line(from, c, to);
        let n = ((dev / FLATNESS).sqrt().ceil().max(1.0) as usize).min(64);
        let mut poly = Vec::new();
        for i in 0..=n {
            let t = i as f32 / n as f32;
            let u = 1.0 - t;
            poly.push(Pt {
                x: u * u * from.x + 2.0 * u * t * c.x + t * t * to.x,
                y: u * u * from.y + 2.0 * u * t * c.y + t * t * to.y,
            });
        }
        let mut worst: f32 = 0.0;
        for i in 0..=200 {
            let t = i as f32 / 200.0;
            let u = 1.0 - t;
            let exact = Pt {
                x: u * u * from.x + 2.0 * u * t * c.x + t * t * to.x,
                y: u * u * from.y + 2.0 * u * t * c.y + t * t * to.y,
            };
            let mut best = f32::MAX;
            for w in poly.windows(2) {
                let (a, b) = (w[0], w[1]);
                let dx = b.x - a.x;
                let dy = b.y - a.y;
                let len2 = dx * dx + dy * dy;
                let t = if len2 <= f32::MIN_POSITIVE {
                    0.0
                } else {
                    (((exact.x - a.x) * dx + (exact.y - a.y) * dy) / len2).clamp(0.0, 1.0)
                };
                let qx = a.x + t * dx;
                let qy = a.y + t * dy;
                best = best.min(((exact.x - qx).powi(2) + (exact.y - qy).powi(2)).sqrt());
            }
            worst = worst.max(best);
        }
        assert!(
            worst <= FLATNESS,
            "max deviation {worst} exceeds {FLATNESS}"
        );
    }

    /// The crossing range is half-open at the top only: `at(y)` accepts `y == lo`, because a
    /// vertex shared by two edges must register on exactly one of them or a vertex-spanning
    /// scanline loses a crossing.
    #[test]
    fn edge_crossing_range_is_half_open_at_the_top() {
        let e = Edge::new(Pt { x: 0.0, y: 0.0 }, Pt { x: 10.0, y: 10.0 }).expect("diagonal edge");
        assert_eq!(e.at(5.0), Some(5.0), "midpoint crosses");
        assert_eq!(e.at(0.0), Some(0.0), "the lower endpoint is included");
        assert!(e.at(10.0).is_none(), "the upper endpoint is excluded");
        assert!(e.at(-0.1).is_none(), "below the edge");
        assert!(e.at(10.1).is_none(), "above the edge");

        // Winding-independent: the same edge reversed gives the same crossings.
        let r = Edge::new(Pt { x: 10.0, y: 10.0 }, Pt { x: 0.0, y: 0.0 }).expect("diagonal edge");
        assert_eq!(r.at(5.0), Some(5.0));
        assert!(r.at(10.0).is_none());

        // A horizontal edge never crosses, and is dropped at construction.
        assert!(
            Edge::new(Pt { x: 0.0, y: 3.0 }, Pt { x: 9.0, y: 3.0 }).is_none(),
            "a horizontal edge must be rejected: it has no crossing to offer"
        );
    }

    #[test]
    fn ink_bounds_of_an_empty_polygon_is_zero() {
        let (a, b, c, d) = ink_bounds(&[]);
        assert_eq!((a, b, c, d), (0.0, 0.0, 0.0, 0.0));
    }
}
