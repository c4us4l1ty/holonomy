//! Painting a [`SurfaceTree`] into a [`Frame`].
//!
//! # The seam this fills
//!
//! `holonomy-render` produces geometry and knows nothing about pixels. `holonomy-display` owns the
//! buffer. This module is between them: it walks the tree and writes into a frame, so the same tree
//! can go to a headless buffer -- which the visual baseline hashes -- or to a DRM dumb buffer.
//!
//! # Everything below `Painter` is a free function taking its scratch explicitly
//!
//! [`Painter::paint`] recurses, and each glyph needs a coverage source (the atlas, or the procedural
//! scratch) *and* a destination scratch *and* the running stats, all at once. As methods taking
//! `&mut self`, each of those needs a simultaneous immutable borrow of one of `self`'s own fields --
//! a borrow error, not a design problem. So the glyph routines take `(&mut Vec<u8>, &mut Vec<u8>,
//! &mut PaintStats)` and the borrows are disjoint, and [`Painter`] is the thin owner that supplies
//! them. Three scratch buffers is a small price for a paint path that allocates nothing per frame.
//!
//! # Two kinds of glyph, two sources
//!
//! * **Box drawing is procedural.** [`holonomy_assets::box_drawing::draw_glyph`] emits a rune's arms
//!   from the verified table into a cell-sized scratch buffer. No atlas, no font. That is why the
//!   chrome's rules are unaffected by which faces are packed: a rule that comes out of the table is
//!   right by construction, and one that came out of a font would be right only if that font happened
//!   to have the glyph at the right weight.
//! * **Everything else comes from the atlas**, built once per session by
//!   [`holonomy_assets::payload::build_atlas`].
//!
//! A codepoint with no glyph in either is **counted, not silently skipped**:
//! [`PaintStats::missing`]. A missing glyph is invisible in a frame -- it is a gap where a character
//! should be, and a gap in prose is indistinguishable from a space.
//!
//! # Damage is honoured, not assumed
//!
//! [`Painter::paint`] takes an optional [`DamageRect`]. Rects that do not intersect it are not
//! rasterised and glyphs whose cell is outside it are not blitted. That is what makes the caret
//! blink cost one cell rather than a frame, and `painting_a_small_damage_rect_writes_fewer_pixels`
//! asserts it rather than leaving it claimed.

use holonomy_assets::atlas::Atlas;
use holonomy_assets::box_drawing;
use holonomy_assets::metric;
use holonomy_assets::payload::Style as AtlasStyle;
use holonomy_render::{DamageRect, Node, SurfaceTree, TextRun};

use crate::frame::{Frame, FrameError};

/// What a paint did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PaintStats {
    /// Filled rectangles.
    pub rects: u32,
    /// Rectangles skipped because they were outside the damage rect.
    pub rects_skipped: u32,
    /// Glyphs blitted from the atlas.
    pub glyphs: u32,
    /// Glyphs skipped because they were outside the damage rect.
    pub glyphs_skipped: u32,
    /// Box-drawing glyphs drawn procedurally.
    pub box_glyphs: u32,
    /// Codepoints with no glyph anywhere. See the module docs: not silently skipped.
    pub missing: u32,
    /// Pixels written.
    pub pixels: u64,
}

/// The painter's mutable working state, as one value.
///
/// Named rather than three separate `&mut` parameters because a row blitter with nine parameters is
/// a signature that has lost its shape -- and because it lets `box_glyph` hand the *same* `&mut
/// Scratch` to `blit_coverage` without an aliasing borrow, since the coverage alias is reborrowed
/// inside.
#[derive(Debug, Default)]
struct Scratch {
    /// Coverage for one procedural box-drawing glyph.
    cov: Vec<u8>,
    /// Coverage for one row.
    row: Vec<u8>,
    /// Running counts.
    stats: PaintStats,
}

/// Draws a [`SurfaceTree`] into a [`Frame`].
#[derive(Debug)]
pub struct Painter<'a> {
    atlas: Option<&'a Atlas>,
    /// Size index for [`TextRun`]s that ask for one.
    size_index: u8,
    scratch: Scratch,
}

impl<'a> Painter<'a> {
    /// A painter with an atlas.
    pub fn new(atlas: &'a Atlas, size_index: u8) -> Self {
        Self {
            atlas: Some(atlas),
            size_index,
            // Sized for the chrome's 18 px cell with headroom, so `box_glyph` never reallocates.
            scratch: Scratch {
                cov: vec![0u8; 64 * 64],
                ..Scratch::default()
            },
        }
    }

    /// A painter with no atlas. Only procedural glyphs and rects are drawn.
    ///
    /// Legitimate for a chrome-only frame and the right choice when the atlas would not fit: the
    /// rules and the bands still draw, and [`PaintStats::missing`] says how many characters did not.
    pub fn without_atlas(size_index: u8) -> Self {
        Self {
            atlas: None,
            size_index,
            scratch: Scratch {
                cov: vec![0u8; 64 * 64],
                ..Scratch::default()
            },
        }
    }

    /// The last [`PaintStats`].
    pub fn stats(&self) -> PaintStats {
        self.scratch.stats
    }

    /// Paint `tree` into `frame`, restricted to `damage` if given.
    pub fn paint(
        &mut self,
        frame: &mut Frame,
        tree: &SurfaceTree,
        damage: Option<DamageRect>,
    ) -> Result<PaintStats, FrameError> {
        self.scratch.stats = PaintStats::default();
        self.walk(frame, tree, damage);
        Ok(self.scratch.stats)
    }

    fn walk(&mut self, frame: &mut Frame, tree: &SurfaceTree, damage: Option<DamageRect>) {
        for child in &tree.before {
            self.walk(frame, child, damage);
        }
        if let Some(node) = &tree.node {
            self.node(frame, *node, damage);
        }
        for child in &tree.after {
            self.walk(frame, child, damage);
        }
    }

    fn node(&mut self, frame: &mut Frame, node: Node, damage: Option<DamageRect>) {
        match node {
            Node::Rect(r) => {
                let rect = DamageRect::new(r.x.max(0) as u32, r.y.max(0) as u32, r.width, r.height);
                if !intersects(rect, damage) {
                    self.scratch.stats.rects_skipped += 1;
                    return;
                }
                // **Clip to the damage rect, not just to the frame.**
                //
                // The panel background is one 1280x800 rect and it intersects *any* damage rect, so
                // intersecting is not sufficient: filling it unclipped means a one-cell caret blink
                // repaints the whole panel -- 16000 times the pixels, and the difference between a
                // still cursor and a flickering one.
                //
                // Clipping to the damage is also what is *correct* rather than merely cheap. The
                // frame already holds the previous frame's content, so the region outside the damage
                // already has the right pixels in it; repainting it can only cost time.
                let fill = match damage {
                    Some(d) => rect.clip(&d),
                    None => rect,
                };
                self.scratch.stats.pixels += frame.fill_rect(
                    fill.x as i64,
                    fill.y as i64,
                    fill.width,
                    fill.height,
                    r.colour,
                );
                self.scratch.stats.rects += 1;
            }
            Node::Text(run) => self.text(frame, run, damage),
            Node::Icon(_) => {
                // Icons are hand-authored 1-bit masks from `.rodata`, and nothing in the chrome uses
                // one. Refusing to draw them beats drawing a wrong one: a silently blank icon is a bug
                // report with no reproduction, and a silently *wrong* one is worse. Counted, so it
                // appears in the stats rather than in nothing.
                self.scratch.stats.rects_skipped += 1;
            }
        }
    }

    fn text(&mut self, frame: &mut Frame, run: TextRun, damage: Option<DamageRect>) {
        let cell_w = self.cell_width();
        let cell_h = self.cell_height();
        let size = self.atlas_size();
        let style = atlas_style(run.style);
        for k in 0..u32::from(run.len) {
            let cp = u64::from(run.first_codepoint) + u64::from(k);
            let Ok(cp) = u32::try_from(cp) else {
                self.scratch.stats.missing += 1;
                continue;
            };
            let x = run.x + (k as i32) * cell_w as i32;
            let cell = DamageRect::new(x.max(0) as u32, run.y.max(0) as u32, cell_w, cell_h);
            if !intersects(cell, damage) {
                self.scratch.stats.glyphs_skipped += 1;
                continue;
            }

            // Box drawing first: procedural, so it cannot be missing.
            if (box_drawing::FIRST..=box_drawing::LAST).contains(&cp) {
                if box_glyph(
                    &mut self.scratch,
                    frame,
                    cp,
                    x,
                    run.y,
                    cell_w,
                    cell_h,
                    run.colour,
                ) {
                    self.scratch.stats.box_glyphs += 1;
                } else {
                    self.scratch.stats.missing += 1;
                }
                continue;
            }

            let Some(atlas) = self.atlas else {
                self.scratch.stats.missing += 1;
                continue;
            };
            let m = atlas.metric(cp, style, size);
            if m.is_blank() {
                // A space: a glyph that legitimately has no ink. Not `missing`.
                self.scratch.stats.glyphs += 1;
                continue;
            }
            let coverage = atlas.coverage();
            blit_coverage(
                &mut self.scratch,
                frame,
                x,
                run.y + i32::from(m.bearing_y),
                cell_w,
                cell_h,
                run.colour,
                |row, dst| {
                    // Within one glyph the rows are contiguous, though the *skyline* packer does not
                    // guarantee contiguity between glyphs.
                    let y = usize::from(m.atlas_y) + row;
                    let sx = usize::from(m.atlas_x);
                    let base = y * metric::ATLAS_STRIDE + sx;
                    for (i, out) in dst.iter_mut().enumerate() {
                        *out = coverage.get(base + i).copied().unwrap_or(0);
                    }
                },
            );
            self.scratch.stats.glyphs += 1;
        }
    }

    fn atlas_size(&self) -> u16 {
        let Some(atlas) = self.atlas else { return 16 };
        atlas
            .sizes()
            .get(usize::from(self.size_index))
            .copied()
            .or_else(|| atlas.sizes().first().copied())
            .unwrap_or(16)
    }

    /// The cell width. Half the ppem, which is a monospace advance for the packed faces.
    fn cell_width(&self) -> u32 {
        self.atlas
            .and_then(|a| a.sizes().first().copied())
            .map_or(8, |ppem| u32::from(ppem) / 2)
    }

    fn cell_height(&self) -> u32 {
        self.atlas
            .and_then(|a| a.sizes().first().copied())
            .map_or(18, |ppem| u32::from(ppem) + 2)
    }
}

/// Draw one procedural box-drawing glyph. Returns whether the rune exists in the table.
#[allow(clippy::too_many_arguments)]
fn box_glyph(
    scratch: &mut Scratch,
    frame: &mut Frame,
    cp: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    colour: u32,
) -> bool {
    if box_drawing::glyph_kind(cp).is_none() {
        return false;
    }
    let ww = usize::try_from(w).unwrap_or(1).max(1);
    let hh = usize::try_from(h).unwrap_or(1).max(1);
    // The coverage buffer is *moved out* for the duration. The row closure needs to read it while
    // `blit_coverage` holds `&mut Scratch`, and a borrow of `scratch.cov` would still be live across
    // that call. A local `Vec` sidesteps it entirely, and it is put back so the next glyph reuses
    // the same allocation -- which is the point of having it in `Scratch` at all.
    let mut cov = std::mem::take(&mut scratch.cov);
    if cov.len() < ww * hh {
        cov.resize(ww * hh, 0);
    }
    // `draw_glyph` asserts the buffer is *exactly* `w * h`, not merely that long -- a deliberate
    // check, since a glyph rasterised into the wrong stride is silently the wrong shape. So hand it
    // the exact cell and keep the oversized buffer for reuse.
    box_drawing::draw_glyph(cp, ww, hh, &mut cov[..ww * hh]);
    blit_coverage(scratch, frame, x, y, w, h, colour, |r, dst| {
        dst.copy_from_slice(&cov[r * ww..r * ww + ww]);
    });
    scratch.cov = cov;
    true
}

/// Blend a `w x h` coverage block at `(x, y)` in `colour`, row by row.
///
/// The row span is clipped against the frame rather than trusting the caller, because a glyph at the
/// right edge of the panel has ink that runs off it and that is a normal occurrence, not a bug.
#[allow(clippy::too_many_arguments)]
fn blit_coverage(
    scratch: &mut Scratch,
    frame: &mut Frame,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    colour: u32,
    mut fill: impl FnMut(usize, &mut [u8]),
) {
    let Scratch { row, stats, .. } = scratch;
    let (row, stats) = (&mut *row, &mut *stats);
    let colour = colour & 0x00FF_FFFF;
    if row.len() < w as usize {
        row.resize(w as usize, 0);
    }
    for r in 0..h {
        let yy = y + r as i32;
        if yy < 0 {
            continue;
        }
        let dst = frame.row_mut(yy as u32);
        if dst.is_empty() {
            continue;
        }
        let span = &mut row[..w as usize];
        span.fill(0);
        fill(r as usize, span);
        let (x0, x1) = (x.max(0), (x + w as i32).min(dst.len() as i32));
        if x1 <= x0 {
            continue;
        }
        let off = (x0 - x) as usize;
        holonomy_assets::blit::blit_row_scalar(
            &mut dst[x0 as usize..x1 as usize],
            &span[off..off + (x1 - x0) as usize],
            (x1 - x0) as usize,
            colour,
        );
        stats.pixels += (x1 - x0) as u64;
    }
}

/// The atlas style for a render [`Style`](holonomy_render::Style).
///
/// `holonomy_render::Style` is a newtype over `u8`, not an enum, so this matches on the payload and
/// needs a fallback arm. `as u8` would be the same answer *today* and a silent corruption the day
/// either side's numbering changes -- which surfaces as every bold glyph rendering regular, far from
/// the cause.
fn atlas_style(style: holonomy_render::Style) -> AtlasStyle {
    match style.0 {
        x if x == holonomy_render::Style::BOLD.0 => AtlasStyle::Bold,
        x if x == holonomy_render::Style::ITALIC.0 => AtlasStyle::Italic,
        x if x == holonomy_render::Style::MONOSPACE.0 => AtlasStyle::Monospace,
        _ => AtlasStyle::Regular,
    }
}

/// Whether `rect` intersects `damage`; no damage rect means "everything".
#[inline]
fn intersects(rect: DamageRect, damage: Option<DamageRect>) -> bool {
    let Some(d) = damage else { return true };
    if rect.width == 0 || rect.height == 0 {
        return false;
    }
    rect.x < d.right() && d.x < rect.right() && rect.y < d.bottom() && d.y < rect.bottom()
}
