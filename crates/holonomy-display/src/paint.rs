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

use core::fmt;
use holonomy_assets::atlas::Atlas;
use holonomy_assets::box_drawing;
use holonomy_assets::metric;
use holonomy_assets::payload::Style as AtlasStyle;

use holonomy_render::{
    AssetId, DamageRect, DocRun, Label, Node, RasterSource, Rect, RoundRect, SurfaceTree, TextRun,
    TextSource,
};

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
    /// Glyphs blitted from a [`Node::DocText`] run. Phase 12.
    ///
    /// **Separate from `glyphs`, so a frame's stats say whether the page had text on it.** With them
    /// merged, a session that drew nothing at all and one that drew a page of body text would report the
    /// same number, and "the document is blank" would be indistinguishable from "the document is short".
    /// That distinction is the entire point of Phase 12, so it is in the accounting from the first frame.
    pub doc_glyphs: u32,
    /// `Node::DocText` runs whose byte range was past the end of the document. Phase 12.
    ///
    /// Zero in a healthy session. Non-zero means the geometry and the text disagree -- a run was emitted
    /// for a line the document does not have -- and that is a bug worth a number rather than a blank
    /// region nobody can explain.
    pub runs_missing: u32,
    /// `Node::DocText` runs longer than [`DocRun::MAX_BYTES`], and so drawn short. Phase 12.
    ///
    /// Should be zero: the emitter splits lines at the page measure, so a run that long is a line wider
    /// than the page. Counted because "a very long line renders truncated with nothing in the stats" is
    /// the kind of thing that is discovered by a user rather than by a gate.
    pub runs_truncated: u32,
    /// Images blitted from decoded rasters.
    ///
    /// §2.9.3 requires `missing` to gain "a sibling, `resampled`, so a frame records how much work the
    /// scaler did rather than hiding it", and the honest accounting is that **the scaler runs at decode
    /// time, not at paint time**: §2.9.3's decision is that the cache holds page-column-width rasters,
    /// so a 1920x1080 source is downscaled before it is ever resident and every image in the product is
    /// a resample. A frame's share of that work is exactly the images it blitted, so this counts them
    /// rather than inferring anything from a timer.
    pub resampled: u32,
    /// `Node::Image` whose raster was not resident, or whose rect did not match the raster's size.
    ///
    /// Counted for the same reason [`PaintStats::missing`] is counted: a missing image is a blank
    /// rectangle, and a blank rectangle in a document is indistinguishable from a page break.
    pub images_missing: u32,
    /// Destination pixels written from decoded rasters.
    pub image_pixels: u64,
    /// Pixels written.
    pub pixels: u64,
}

impl PaintStats {
    /// **Fold one pass's counts into another's, for a frame made of more than one clipped pass.**
    ///
    /// # Why this exists
    ///
    /// **Because a frame stopped being one call.** Part 39 gave the session a second damaged region —
    /// the status bar's — which is painted as its own clipped pass over the same tree, because a single
    /// bounding box from the caret's cell to the bottom of the window is the whole window. So the frame
    /// is `main pass + status pass`, and **the stats have to be the sum or they are the first pass's
    /// alone**, which would report a frame that drew its status bar as one that did not.
    ///
    /// **Summed rather than replaced, and every field, because a field left out is a field that stops
    /// being true.** `rects_skipped` in particular: the status pass *skips* almost everything in the
    /// tree, and that is the number that says the clipping worked.
    pub fn merge(&mut self, other: PaintStats) {
        self.rects += other.rects;
        self.rects_skipped += other.rects_skipped;
        self.glyphs += other.glyphs;
        self.glyphs_skipped += other.glyphs_skipped;
        self.box_glyphs += other.box_glyphs;
        self.missing += other.missing;
        self.doc_glyphs += other.doc_glyphs;
        self.runs_missing += other.runs_missing;
        self.runs_truncated += other.runs_truncated;
        self.resampled += other.resampled;
        self.images_missing += other.images_missing;
        self.image_pixels += other.image_pixels;
        self.pixels += other.pixels;
    }
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
pub struct Painter<'a> {
    atlas: Option<&'a Atlas>,
    /// Size index for [`TextRun`]s that ask for one.
    size_index: u8,
    scratch: Scratch,
}

/// Hand-written rather than derived, because `RasterSource` is a trait object and has no `Debug`.
/// The source is reported as *present or absent*, which is the only thing about it that is this
/// struct's business -- what it resolves to belongs to whoever implemented it.
impl fmt::Debug for Painter<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Painter")
            .field("atlas", &self.atlas.is_some())
            .field("size_index", &self.size_index)
            .field("stats", &self.scratch.stats)
            .finish()
    }
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

    /// The atlas this painter blits from, if it has one.
    ///
    /// For the session's math layout, which needs real per-glyph advances and gets them from here
    /// rather than from a second copy of the atlas. `Painter` already owns the only `&'a Atlas` in the
    /// session, and exposing it is cheaper than storing another reference that must be kept in step --
    /// two `&Atlas` in one struct is two chances to point at different ones.
    pub fn atlas(&self) -> Option<&'a Atlas> {
        self.atlas
    }

    /// The size index runs are painted at.
    pub fn size_index(&self) -> u8 {
        self.size_index
    }

    /// **How far the pen moves for one codepoint, in the face [`doc_text`](Self::doc_text) uses.**
    ///
    /// ## Why this exists, and why it is a method rather than a table lookup
    ///
    /// **Part 29 found that a line's runs were positioned by `Session::emit_body_text` on the 8 px cell
    /// grid while `doc_text` advanced the pen by the font's own metrics**, so a run boundary was a
    /// discontinuity and styling a word moved the text after it.
    ///
    /// **The fix is for the two to call one function**, and this is it. A caller that re-implements
    /// the rule gets it subtly wrong, and the two are wrong in the *same place* when it happens: the
    /// blank case. **A space takes the face's advance and a `.notdef` takes the cell** — two different
    /// rules for two kinds of nothing — and a caller that wrote `advance_x.max(1)` for both would place
    /// a space 1 px on and every space in the document would be wrong by a few pixels.
    ///
    /// ## The rule, verbatim from `doc_text`
    ///
    /// ```text
    /// blank   : advance_x if it is > 0, else cell_width() and count it missing
    /// inked   : advance_x.max(1)
    /// ```
    ///
    /// **No atlas and not the cell: `cell_width()`.** The chrome is a fixed grid and that is right for
    /// it; this is about the one place the grid is the fallback rather than the rule.
    pub fn advance_of(&self, cp: u32, style: holonomy_render::Style) -> u32 {
        let Some(atlas) = self.atlas else {
            return self.cell_width();
        };
        let m = atlas.metric(cp, atlas_style(style), self.atlas_size());
        if m.is_blank() {
            if m.advance_x > 0 {
                u32::from(m.advance_x)
            } else {
                self.cell_width()
            }
        } else {
            u32::from(m.advance_x).max(1)
        }
    }

    /// The last [`PaintStats`].
    pub fn stats(&self) -> PaintStats {
        self.scratch.stats
    }

    /// Paint `tree` into `frame`, restricted to `damage` if given.
    ///
    /// No [`RasterSource`], so every [`Node::Image`] counts in
    /// [`PaintStats::images_missing`]. The convenience form, for a document with no images and for the
    /// tests that predate them.
    pub fn paint(
        &mut self,
        frame: &mut Frame,
        tree: &SurfaceTree,
        damage: Option<DamageRect>,
    ) -> Result<PaintStats, FrameError> {
        self.paint_with_rasters(frame, tree, damage, None)
    }

    /// Paint `tree`, resolving [`Node::Image`]'s pixels through `rasters`.
    ///
    /// # Why the source is an argument and not a field
    ///
    /// The source in practice is the session's own `IcebergCache`, and the session *also* owns the
    /// painter. A field would mean the painter held a borrow of a struct it lives inside, for as long
    /// as the painter lives -- a self-referential `Session` that safe Rust cannot express, and that
    /// `Box`-ing the cache would only disguise. An argument has the lifetime of the call, which is
    /// exactly as long as the borrow needs to be.
    ///
    /// The cost is one extra parameter threaded through `walk` and `node`, and `walk` is recursive, so
    /// it is one `&dyn` pointer copied per node. That is measurable only in a benchmark of an empty
    /// tree, which is not a frame anyone paints.
    pub fn paint_with_rasters(
        &mut self,
        frame: &mut Frame,
        tree: &SurfaceTree,
        damage: Option<DamageRect>,
        rasters: Option<&dyn RasterSource>,
    ) -> Result<PaintStats, FrameError> {
        self.paint_with(frame, tree, damage, rasters, None)
    }

    /// Paint `tree`, resolving document text through `text` and images through `rasters`. Phase 12.
    ///
    /// **Two sources, one reason.** A [`Node::DocText`] names a byte offset in the document and a
    /// [`Node::Image`] names an asset, and neither payload can live in the node -- `Node` is `Copy`, and
    /// `SurfaceTree`'s `before`/`after` ordering depends on that. So both arrive as arguments with the
    /// lifetime of the call, for exactly the reason [`RasterSource`]'s docs give: a field would mean the
    /// painter holding a borrow of a struct it lives inside, which is a self-referential `Session` that
    /// safe Rust cannot express.
    ///
    /// Bundling them into one struct was considered and rejected: it would make every one of the ~30
    /// existing `paint`/`paint_with_rasters` call sites name a type with a field they do not have, to
    /// add a parameter that is `None` almost everywhere. The cost of two `Option<&dyn>` is one pointer
    /// per node per frame, copied in `walk`'s recursion.
    ///
    /// **`None` text source is a legitimate state**, not a degenerate one: a chrome-only frame has no
    /// document text in its tree at all, and every [`Node::DocText`] in one that does is counted in
    /// [`PaintStats::runs_missing`] rather than failing the paint.
    pub fn paint_with(
        &mut self,
        frame: &mut Frame,
        tree: &SurfaceTree,
        damage: Option<DamageRect>,
        rasters: Option<&dyn RasterSource>,
        text: Option<&dyn TextSource>,
    ) -> Result<PaintStats, FrameError> {
        self.scratch.stats = PaintStats::default();
        self.walk(frame, tree, damage, rasters, text);
        Ok(self.scratch.stats)
    }

    fn walk(
        &mut self,
        frame: &mut Frame,
        tree: &SurfaceTree,
        damage: Option<DamageRect>,
        rasters: Option<&dyn RasterSource>,
        text: Option<&dyn TextSource>,
    ) {
        for child in &tree.before {
            self.walk(frame, child, damage, rasters, text);
        }
        if let Some(node) = &tree.node {
            self.node(frame, *node, damage, rasters, text);
        }
        for child in &tree.after {
            self.walk(frame, child, damage, rasters, text);
        }
    }

    fn node(
        &mut self,
        frame: &mut Frame,
        node: Node,
        damage: Option<DamageRect>,
        rasters: Option<&dyn RasterSource>,
        text: Option<&dyn TextSource>,
    ) {
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
            Node::RoundRect(r) => self.round_rect(frame, r, damage),
            Node::Label(l) => self.label(frame, l, damage),
            Node::Text(run) => self.text(frame, run, damage),
            // **The same blit, with the codepoint held still.** `text` walks `first_codepoint + k`;
            // a repeat is `first_codepoint` at every `k`. One function rather than two, because the
            // two differ by that expression and every other line of the loop is identical — and a
            // second copy of a glyph blitter is a second thing to keep in step with the atlas.
            Node::Repeat(run) => self.text_repeated(frame, run, damage),
            Node::DocText(run) => self.doc_text(frame, run, damage, text),
            Node::Image { rect, asset_id } => self.image(frame, &rect, asset_id, damage, rasters),
            Node::Icon(icon) => self.icon(frame, &icon, damage),
        }
    }

    /// Fill a rounded rectangle by integer scanlines.
    fn round_rect(&mut self, frame: &mut Frame, r: RoundRect, damage: Option<DamageRect>) {
        let rect = DamageRect::new(r.rect.x.max(0) as u32, r.rect.y.max(0) as u32, r.rect.width, r.rect.height);
        if !intersects(rect, damage) { self.scratch.stats.rects_skipped += 1; return; }
        let fill = match damage { Some(d) => rect.clip(&d), None => rect };
        let rad = r.radius.min(r.rect.width / 2).min(r.rect.height / 2);
        let mut pixels = 0u64;
        for y in fill.y..fill.bottom() {
            let dy = if y < rect.y + rad { rect.y + rad - y }
                     else if y >= rect.bottom().saturating_sub(rad) { y + 1 - (rect.bottom() - rad) }
                     else { 0 };
            let inset = if dy == 0 { 0 } else {
                let d = dy.saturating_sub(1);
                let r2 = u64::from(rad) * u64::from(rad);
                let d2 = u64::from(d) * u64::from(d);
                rad - isqrt(r2.saturating_sub(d2)) as u32
            };
            let x0 = fill.x.max(rect.x + inset);
            let x1 = fill.right().min(rect.right().saturating_sub(inset));
            if x1 > x0 { pixels += frame.fill_rect(x0 as i64, y as i64, x1 - x0, 1, r.rect.colour); }
        }
        self.scratch.stats.pixels += pixels;
        self.scratch.stats.rects += 1;
    }

    /// Draw a proportional chrome label from inline bytes.
    fn label(&mut self, frame: &mut Frame, l: Label, damage: Option<DamageRect>) {
        let len = l.byte_len();
        if len == 0 { return; }
        let cell_w = self.cell_width();
        let cell_h = self.cell_height();
        let bytes = &l.bytes[..len];
        let whole = DamageRect::new(l.x.max(0) as u32, l.y.max(0) as u32,
            (len as u32).saturating_mul(cell_w), cell_h);
        if !intersects(whole, damage) {
            self.scratch.stats.glyphs_skipped += self.count_codepoints(bytes);
            return;
        }
        let Some(atlas) = self.atlas else {
            self.scratch.stats.missing += self.count_codepoints(bytes);
            return;
        };
        let size = self.atlas_size();
        let style = atlas_style(l.style);
        let ascent = i32::from(self.vertical(l.style, size).0);
        let coverage = atlas.coverage();
        let mut pen = l.x;
        let mut at = 0usize;
        while at < bytes.len() {
            let (c, used) = match std::str::from_utf8(&bytes[at..]) {
                Ok(s) => match s.chars().next() { Some(c) => (c, c.len_utf8()), None => break },
                Err(e) => {
                    self.scratch.stats.missing += 1;
                    let valid = e.valid_up_to();
                    if valid > 0 {
                        let s = std::str::from_utf8(&bytes[at..at + valid]).unwrap();
                        match s.chars().next() { Some(c) => (c, c.len_utf8()), None => break }
                    } else { ('\u{FFFD}', e.error_len().unwrap_or(1)) }
                }
            };
            at += used;
            let m = atlas.metric(u32::from(c), style, size);
            if m.is_blank() {
                if m.advance_x > 0 { pen += i32::from(m.advance_x); }
                else { pen += cell_w as i32; self.scratch.stats.missing += 1; }
                continue;
            }
            blit_coverage(&mut self.scratch, frame,
                pen + i32::from(m.bearing_x), l.y + ascent - i32::from(m.bearing_y),
                u32::from(m.width), u32::from(m.height), l.colour,
                |row, dst| {
                    let y = usize::from(m.atlas_y) + row;
                    let sx = usize::from(m.atlas_x);
                    let base = y * metric::ATLAS_STRIDE + sx;
                    for (i, out) in dst.iter_mut().enumerate() { *out = coverage.get(base + i).copied().unwrap_or(0); }
                });
            self.scratch.stats.glyphs += 1;
            pen += self.advance_of(u32::from(c), l.style) as i32;
        }
    }

    /// Blit a 1-bit icon mask.
    ///
    /// # This arm used to be a refusal, and the refusal is the story
    ///
    /// `Node::Icon` counted a skipped rect and drew nothing, with the comment *"nothing in the chrome
    /// uses one"*. **That was true when it was written and it stayed true for the whole of Phase 13**,
    /// because the chrome drew its toolbar with `[` and `]` box-drawing runes and its menus did not
    /// exist. So the icon path was never exercised by anything, and a whole node type sat in the tree
    /// doing nothing.
    ///
    /// **It is worth being clear about what the refusal cost.** The `Icon` type, the `coverage`
    /// accessor, the bit-order documentation and the tests all existed and were all correct, and the
    /// chrome still rendered its controls as `[B][M][H][u]` in bracket glyphs that no test objected
    /// to. **A refusal is a claim about the future, and this one was made about a future that had
    /// already been designed and not built.**
    ///
    /// # The blit
    ///
    /// **One bit per pixel, opaque, with no blending** — which is what a mask in a monochrome palette
    /// means. A set bit writes the icon's colour; a clear bit writes nothing at all. That is not an
    /// optimisation, it is the definition: blending a cleared pixel would put the foreground colour at
    /// some fraction over the background, and a 1-bit format has no fraction to blend by.
    ///
    /// **Clipped to the damage** like every other node, and the clip is applied to the *mask* rather
    /// than to the destination, so a partially damaged icon does the pixels that are damaged and
    /// leaves the rest for the next frame.
    fn icon(
        &mut self,
        frame: &mut Frame,
        icon: &holonomy_render::Icon,
        damage: Option<DamageRect>,
    ) {
        let Some(bounds) = icon.bounds() else {
            return;
        };
        // **Counted after the clip, and skipped when the clip is empty** -- which is what the `Rect`
        // arm does and what `a_damage_rect_outside_everything_paints_nothing` asserts for the whole
        // tree. The first version of this counted unconditionally, so painting with a damage rect
        // entirely off the panel reported 30 rectangles drawn when it had drawn none.
        let clip = match damage {
            Some(d) => {
                let c = bounds.clip(&d);
                if c.is_empty() {
                    self.scratch.stats.rects_skipped += 1;
                    return;
                }
                c
            }
            None => bounds,
        };
        self.scratch.stats.rects += 1;

        // **`row` is a count of `u32`s, not of bytes.** `Frame::pixels()` is `&[u32]`, so the stride
        // from one row to the next is `width` *pixels* -- and the first version of this used
        // `width * 4`, which is the stride in **bytes**. Every row therefore landed four times further
        // down than it should have: a toolbar icon at y = 89 was written at y = 355, which is on the
        // page. The icon was drawn, brightly, in the wrong place, and the toolbar looked empty.
        //
        // **It is worth naming why this was hard to see.** The factor of four is not a wild
        // displacement -- it is a displacement *to somewhere plausible*, inside the frame, past every
        // bounds check, so nothing complained. The visible symptom ("the icons are not where they
        // should be, and there are icons where there should not be") reads as a layout bug, and the
        // layout was fine.
        let row = frame.width() as usize;
        let colour = icon.colour & 0x00FF_FFFF;
        for y in clip.y..clip.bottom() {
            let mask_y = y - bounds.y;
            let dst_row = y as usize * row;
            for x in clip.x..clip.right() {
                if !icon.coverage(x - bounds.x, mask_y) {
                    continue;
                }
                if let Some(p) = frame.pixels_mut().get_mut(dst_row + x as usize) {
                    *p = colour;
                }
            }
        }
    }

    /// # The grid is the grid; the ink is the ink
    ///
    /// **Changed in Phase 9C.** This used to hand `blit_coverage` `cell_width()` -- `ppem / 2`, so
    /// 8 px at 16 ppem -- rather than the metric's own width, so **every glyph wider than 8 px had its
    /// right-hand columns clipped**. Measured at 16 ppem (`holonomy-assets/examples/math_advances.rs`
    /// prints the table): Latin letters are 9–10 px, Greek and the operators 10–11 px, and `\sum` is
    /// **14 px**, losing 6 of its columns.
    ///
    /// The old behaviour was self-consistent for body text, which is why it survived to 9B: the
    /// renderer is a **fixed-cell grid**, `cell_width` is the advance `TextRun` uses for every `k`, and
    /// a clipped glyph's missing columns were exactly overwritten by the next cell's leading columns.
    /// The page read as *tight*, not wrong. It stopped cancelling for a formula, because
    /// `math_layout` advances by the fonts' *real* advances (`MathMetrics::advance`) while the painter
    /// blitted one cell -- a gap of `advance - cell_width` px between glyphs, and a `\sum` missing its
    /// right-hand third.
    ///
    /// **What this still does *not* do: it does not make the advance proportional.** The advance stays
    /// `cell_w` for every `k`, because that is the page's text grid and 9A/9B laid tables and formulas
    /// out on it. So a proportional face is still *positioned* on an 8 px lattice; what changed is
    /// that its ink is no longer truncated to the lattice. Overhang is now possible and expected, which
    /// is why the damage test below is the ink rect rather than the cell: on an 8 px grid with 9–10 px
    /// glyphs, consecutive letters overlap by 1–2 px, and culling on the cell would leave a stale
    /// stripe wherever the overhang was the only thing damaged.
    ///
    /// The visual baseline moved with this. `crates/holonomy-display/tests/` and
    /// `crates/holonomy/tests/` pin the affected geometry; `phase4_gate.rs`'s atlas figures are
    /// unaffected because the atlas is unchanged -- this is purely how it is read.
    fn text(&mut self, frame: &mut Frame, run: TextRun, damage: Option<DamageRect>) {
        self.text_cells(frame, run.x, run.y, run.first_codepoint, run.len, run.style, run.colour, damage, false);
    }

    /// [`text`](Self::text) with the codepoint held still: `len` copies of one glyph.
    ///
    /// **The repeat case of the same loop, and it is a parameter rather than a second function** so
    /// that a rule and a label cannot drift apart in anything but the arithmetic that says which
    /// codepoint is at `k`. See [`RepeatRun`](holonomy_render::RepeatRun) for what that arithmetic
    /// was before this existed.
    fn text_repeated(
        &mut self,
        frame: &mut Frame,
        run: holonomy_render::RepeatRun,
        damage: Option<DamageRect>,
    ) {
        self.text_cells(
            frame,
            run.x,
            run.y,
            run.codepoint,
            run.len,
            run.style,
            run.colour,
            damage,
            true,
        );
    }

    /// The glyph loop, shared by [`text`](Self::text) and [`text_repeated`](Self::text_repeated).
    ///
    /// **`constant` is the whole difference and it is one expression**: the codepoint at step `k` is
    /// `first + k` for a run of text and `first` for a repeat. Everything else — the damage test, the
    /// box-drawing arm, the metric lookup, the blit — is identical, and a second copy of a blitter is
    /// a second thing to keep in step with the atlas.
    #[allow(clippy::too_many_arguments)]
    fn text_cells(
        &mut self,
        frame: &mut Frame,
        x0: i32,
        y0: i32,
        first: u32,
        len: u16,
        style: holonomy_render::Style,
        colour: u32,
        damage: Option<DamageRect>,
        constant: bool,
    ) {
        let cell_w = self.cell_width();
        let cell_h = self.cell_height();
        let size = self.atlas_size();
        let run_style = style;
        let style = atlas_style(style);
        for k in 0..u32::from(len) {
            // **No `try_from` here: `first` is a `u32` and so is `k`.** The conversion was left
            // from the `usize` codepoint days, and `unwrap_or(u32::MAX)` on a value that cannot
            // fail is a branch that can never be taken -- **which means the saturating behaviour it
            // appeared to promise for an out-of-range run was never there at all.** A run that
            // does run past `0x10FFFF` is caught further down, where the atlas lookup has
            // something to say about it, and `missing` is the counter that records it.
            let cp = if constant { first } else { first.saturating_add(k) };
            let x = x0 + (k as i32) * cell_w as i32;
            // One damage test for the whole glyph, box drawing included, because the damage model
            // and the cell are the same rectangle. See the note on the blit below for why this is
            // the cell and not the ink.
            let cell = DamageRect::new(x.max(0) as u32, y0.max(0) as u32, cell_w, cell_h);
            if !intersects(cell, damage) {
                self.scratch.stats.glyphs_skipped += 1;
                continue;
            }

            // Box drawing first: procedural, so it cannot be missing. Its damage rect is the whole
            // cell because `box_glyph` draws arms to the cell edges -- a `──` fills its cell's full
            // width by construction, not by accident. See `box_drawing::cell_metric`, which reports
            // `width == height == cell_size(ppem)`.
            if (box_drawing::FIRST..=box_drawing::LAST).contains(&cp) {
                if box_glyph(
                    &mut self.scratch,
                    frame,
                    cp,
                    x,
                    y0,
                    cell_w,
                    cell_h,
                    colour,
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

            // # The advance is the cell; the ink is the metric's
            //
            // `cell_w` is the *advance* -- the page's fixed text grid, and what every caller and the
            // whole line model position by -- so it stays. What changes is the blit: `m.width` and
            // `m.height` from the metric, at `x + bearing_x`, instead of `cell_w` x `cell_h` at the
            // cell origin.
            //
            // Why it matters, measured at 16 ppem by `holonomy-assets/examples/math_advances.rs`:
            // Inter Italic letters are 9-11 px of ink and `\sum` is 14, all in an 8 px cell. The old
            // blit truncated every one of them to 8 columns, so `\sum` lost 6 of its 14 and no glyph
            // in the product was drawn at its own width.
            //
            // # Why the damage test is still the *cell*
            //
            // Culling on the ink rect is the correct end state, and the precondition for it -- ink
            // inside the line box -- now holds, because `bearing_y` is a real top side bearing and
            // `cell_height()` is derived from the faces' own ascent and descent (see
            // `Atlas::line_pitch`).
            //
            // It is still the cell because the cell is what the *rest* of the system agrees on. Every
            // caller damages `cell_h`-tall rects (`emit_math`, the table grid, the chrome bands) and
            // positions lines by `LineHeights`, whose pitch is `cell_h`. Moving the cull to the ink
            // rect is a saving of a few bytes of damage area per glyph, and it would have to be
            // proven against every one of those callers. A cell that contains the ink culls
            // *conservatively* -- it can only ever paint more than strictly necessary, never less --
            // so it is the safe side to be on while the pitch is still changing.
            //
            // What the old version of this comment recorded is worth keeping: the previous
            // arrangement had ink *outside* the cell, so culling on the cell and culling on the ink
            // disagreed about which glyphs exist at all, and a formula stopped rendering because its
            // ink at y 126..138 did not intersect the damage at y 143..160. `probe_linebox.rs` is the
            // gate that says ink is now inside the box.
            // # The vertical placement, which used to be wrong by 17-21 px
            //
            // `run.y` is a **line box top**, not a baseline. A glyph's ink starts at
            // `baseline - bearing_y`, and the baseline is `run.y + ascent` for the run's own face --
            // so the blit belongs at `run.y + ascent - bearing_y`, not at `run.y + bearing_y`.
            //
            // It used to be the latter, and `raster.rs` stored `bearing_y` as the distance from the
            // *ascender line* down to the ink bottom rather than the distance from the baseline up to
            // the ink top, so the two errors compounded into ink landing 17-21 px above the box that
            // was supposed to contain it. `bearing_y` is now a true top side bearing; see
            // `holonomy-assets/src/raster.rs::top_side_bearing`.
            let ascent = i32::from(self.vertical(run_style, size).0);
            let coverage = atlas.coverage();
            blit_coverage(
                &mut self.scratch,
                frame,
                x + i32::from(m.bearing_x),
                y0 + ascent - i32::from(m.bearing_y),
                u32::from(m.width),
                u32::from(m.height),
                colour,
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

    /// Draw one line of document text. Phase 12.
    ///
    /// # What is different from [`Painter::text`], and why anything is
    ///
    /// **Decoding.** `TextRun` is `(first_codepoint, len)` -- consecutive codepoints -- so `text` can
    /// compute codepoint `k` by addition. A `DocRun` is a *byte* range, so the codepoints must come out
    /// of UTF-8 decoding, and that is the whole difference in the loop.
    ///
    /// **The advance is per-glyph, and this is the fix Phase 12 owns.** `text` advances by `cell_w` for
    /// every codepoint -- the page's fixed 8 px grid. Here it advances by `m.advance_x`, the font's own
    /// advance, so a proportional face is *positioned* proportionally. §9B measured the ink widths (9-10
    /// px for most Latin letters, 14 px for `\sum`) and 9C fixed the *blit* to use the metric's width;
    /// **the advance was still the grid**, which is why a 10 px glyph in an 8 px cell either overlapped
    /// its neighbour by 2 px or left a 2 px gap depending on the round face's rounding. PROJECT.md:943
    /// named this as "not done, and named", and it is done here.
    ///
    /// Wrapping is *not* done here and is not this function's job: the emitter splits a line at the page
    /// measure and emits one run per visual row, so every run here is already short enough to fit.
    ///
    /// # Invalid UTF-8 is counted, not skipped
    ///
    /// A byte that does not start a valid sequence is drawn as U+FFFD and counted in
    /// [`PaintStats::missing`]. The document's own bytes come from a container with a Blake2b digest, so
    /// invalid UTF-8 means the file was written by something else -- and it should look wrong on the page
    /// rather than vanish. A byte skipped silently is a hole in the text with nothing in the stats.
    fn doc_text(
        &mut self,
        frame: &mut Frame,
        run: DocRun,
        damage: Option<DamageRect>,
        text: Option<&dyn TextSource>,
    ) {
        if run.len == 0 {
            return;
        }
        let Some(source) = text else {
            self.scratch.stats.runs_missing += 1;
            return;
        };
        // Clamped rather than asserted; see `DocRun::MAX_BYTES` for why.
        let want = run.len.min(DocRun::MAX_BYTES);
        if run.len > DocRun::MAX_BYTES {
            self.scratch.stats.runs_truncated += 1;
        }
        let Some(bytes) = source.document(run.offset, want) else {
            self.scratch.stats.runs_missing += 1;
            return;
        };

        let cell_w = self.cell_width();
        let cell_h = self.cell_height();
        let size = self.atlas_size();
        let style = atlas_style(run.style);
        let ascent = i32::from(self.vertical(run.style, size).0);

        // **The line box is culled once, not per glyph.** A run's box is its byte count times the cell
        // width -- a conservative over-estimate, as `DocRun::bounds` says -- and if that box misses the
        // damage then every glyph in it does too. Culling per glyph would be the same answer at ~150x
        // the comparisons for a 150-codepoint line.
        let box_width = want.saturating_mul(cell_w);
        let whole = DamageRect::new(run.x.max(0) as u32, run.y.max(0) as u32, box_width, cell_h);
        if !intersects(whole, damage) {
            self.scratch.stats.glyphs_skipped += self.count_codepoints(bytes);
            return;
        }

        let Some(atlas) = self.atlas else {
            self.scratch.stats.missing += self.count_codepoints(bytes);
            return;
        };
        let coverage = atlas.coverage();

        let mut pen = run.x;
        let mut at = 0usize;
        while at < bytes.len() {
            // **One codepoint at a time, decoding by hand.** `str::from_utf8(bytes).unwrap_or("")` would
            // be shorter and wrong: it renders *nothing* for a document that is not UTF-8, which is the
            // one case where the user most needs to see that something is wrong. `chars()` cannot be
            // used directly either, because it yields `Err` for the bad byte and stops caring about the
            // rest -- so the resynchronisation point has to be chosen here.
            //
            // The rule is `Utf8Error`'s own: `valid_up_to` is the start of the bad sequence, and
            // `error_len` is how many bytes to skip over it. `error_len == None` means the sequence ran
            // off the end of the slice, so one byte is skipped instead -- consuming nothing would spin.
            let (c, used) = match std::str::from_utf8(&bytes[at..]) {
                Ok(s) => match s.chars().next() {
                    Some(c) => (c, c.len_utf8()),
                    None => break,
                },
                Err(e) => {
                    self.scratch.stats.missing += 1;
                    let valid = e.valid_up_to();
                    if valid > 0 {
                        // Decodable bytes *before* the bad one: draw them, and take no penalty. The next
                        // iteration starts at the bad byte.
                        let s = std::str::from_utf8(&bytes[at..at + valid])
                            .expect("valid_up_to is a char boundary by definition");
                        match s.chars().next() {
                            Some(c) => (c, c.len_utf8()),
                            None => break,
                        }
                    } else {
                        ('\u{FFFD}', e.error_len().unwrap_or(1))
                    }
                }
            };
            at += used;
            let m = atlas.metric(u32::from(c), style, size);
            if m.is_blank() {
                // Two cases, distinguished by whether the face had an advance for it. A **space** has
                // ink of zero and an advance of its width, and takes the face's advance -- that is what
                // makes word spacing proportional. `.notdef` has zero for both, and taking `cell_w` for
                // it would make a page of missing glyphs read as evenly spaced, which looks like a
                // document rather than like a font that lacks them.
                // **`advance_of`, so this arm and the caller cannot disagree.** The two rules for two
                // kinds of nothing -- a space takes the face's advance, `.notdef` takes the cell -- are
                // the whole of part 29's bug, and they are one call site now instead of two.
                if m.advance_x > 0 {
                    pen += i32::from(m.advance_x);
                } else {
                    pen += cell_w as i32;
                    self.scratch.stats.missing += 1;
                }
                continue;
            }
            blit_coverage(
                &mut self.scratch,
                frame,
                pen + i32::from(m.bearing_x),
                run.y + ascent - i32::from(m.bearing_y),
                u32::from(m.width),
                u32::from(m.height),
                run.colour,
                |row, dst| {
                    let y = usize::from(m.atlas_y) + row;
                    let sx = usize::from(m.atlas_x);
                    let base = y * metric::ATLAS_STRIDE + sx;
                    for (i, out) in dst.iter_mut().enumerate() {
                        *out = coverage.get(base + i).copied().unwrap_or(0);
                    }
                },
            );
            self.scratch.stats.doc_glyphs += 1;
            // **`advance_of`, and not the expression.** See the blank arm above: the rule has two
            // cases and this is one of them, and `Painter::advance_of` is the single definition.
            pen += self.advance_of(u32::from(c), run.style) as i32;
        }
    }

    /// How many codepoints `bytes` holds, for the skipped and missing counts.
    ///
    /// A full decode into a `Vec` would be the obvious way and it would allocate on the paint path,
    /// which is the thing Phase 12 is trying to remove. This walks the sequence counting start bytes,
    /// which is `O(bytes)` with no allocation and no `char` materialisation.
    fn count_codepoints(&mut self, bytes: &[u8]) -> u32 {
        let mut n = 0u32;
        let mut i = 0usize;
        while i < bytes.len() {
            // Continuation bytes are 0b10xxxxxx, so counting the non-continuations counts the
            // codepoints -- including an invalid sequence, which is counted as one, which is the
            // conservative direction for a "how much did we skip" figure.
            if bytes[i] & 0xC0 != 0x80 {
                n += 1;
            }
            i += 1;
        }
        n
    }

    /// **Select which of the atlas's sizes this painter draws at.** Part 43.
    ///
    /// ## Why this is a setter and not a constructor argument
    ///
    /// **`Painter::new` has taken an atlas and left the size at zero since the atlas gained multiple
    /// sizes**, which means `size_index` was a field with exactly one reachable value. **Zoom is
    /// what the field is for**, and the alternative — a second painter — would be a second scratch
    /// buffer and a second stats counter for the same atlas.
    ///
    /// ## The clamp, and why it is not a panic
    ///
    /// **`min(atlas.sizes().len() - 1)`**, because `atlas_size()` already falls back to the first
    /// size when the index is out of range and a panic here would be a second, stricter rule than
    /// the one the rest of the file obeys. **A zoom to a size the atlas does not hold becomes the
    /// largest size it does hold**, which is the same answer and is visible in the pixels.
    pub fn set_size_index(&mut self, index: u8) {
        let Some(atlas) = self.atlas else {
            self.size_index = index;
            return;
        };
        let last = u32::try_from(atlas.sizes().len().saturating_sub(1)).unwrap_or(0);
        self.size_index = u8::try_from(u32::from(index).min(last)).unwrap_or(0);
    }

    /// **The ppem this painter is drawing at, or `None` with no atlas.**
    ///
    /// **The session needs this and could not get it**: `atlas_size` is private, and a zoom that
    /// changed the size without being able to read it back would not be able to say whether it had
    /// worked. **`zoom.rs` reads this after every `set_zoom`.**
    #[must_use]
    pub fn size(&self) -> Option<u16> {
        self.atlas.map(|_| self.atlas_size())
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

    /// **The chrome's cell width: the monospace face's advance at the atlas size.**
    ///
    /// ## The false document, and part 32
    ///
    /// This was `ppem / 2`, described as *"half the ppem, which is a monospace advance for the packed
    /// faces."* **Both halves of that were wrong and the error was measurable.** Measured across the
    /// packed faces at 16 ppem:
    ///
    /// ```text
    /// cell_width() = ppem/2 =  8      Monospace advance = 10      Regular mean =  8.58
    /// ```
    ///
    /// **20 % narrow.** The claim was that `ppem/2` *is* a monospace advance; it is a number that
    /// happens to be near one. **The advance is a property of the face and this function had the atlas
    /// in hand the whole time** — it was reading `sizes()` and throwing away everything else about it.
    ///
    /// ## Why `'m'`
    ///
    /// **Because the measurement is the definition, not the division.** Every glyph in a monospace face
    /// has the same advance, so `'m'` is as good as any other and it is the one the face is widest on —
    /// which makes it the one least likely to be silently wrong for a face with a defect.
    ///
    /// ## And `with no atlas`, 8
    ///
    /// **The constant stays as the no-atlas fallback, for the same reason `cell_h`'s 18 does**: a
    /// chrome-only frame draws procedural box-drawing glyphs sized to the cell, and it needs the
    /// fallback to be *self-consistent*, not typographically correct. **It is also now the same 8 the
    /// design was drawn at**, so a frame with no atlas is the 16 ppem design and not a third thing.
    pub fn cell_width(&self) -> u32 {
        self.atlas.map_or(8, |atlas| {
            let size = self.atlas_size();
            u32::from(
                atlas
                    .metric(
                        b'm' as u32,
                        atlas_style(holonomy_render::Style::MONOSPACE),
                        size,
                    )
                    .advance_x,
            )
            .max(1)
        })
    }

    /// `(ascent_px, descent_px)` for a run's style and size, or `(0, 0)` with no atlas.
    fn vertical(&self, style: holonomy_render::Style, size: u16) -> (u16, u16) {
        self.atlas
            .map_or((0, 0), |a| a.vertical(atlas_style(style), size))
    }

    /// The text cell height: the line box a run occupies.
    ///
    /// **This was `ppem + 2`, which is 18 and was an arithmetic identity with nothing to do with the
    /// fonts.** The packed faces need 20 px (Inter), 22 (JetBrains Mono) and 24 (Noto Sans Math) of
    /// ascent-plus-descent at 16 ppem, so no placement of the baselines can make an 18 px box contain
    /// their ink. The value is now [`Atlas::line_pitch`] -- the tallest ascent plus descent in the
    /// atlas, plus the one pixel the rasteriser's antialiasing pad needs.
    ///
    /// With no atlas there are no faces and therefore no metrics, so this falls back to 18. A
    /// chrome-only frame draws box-drawing glyphs, which are procedural and sized to the cell, so the
    /// fallback only has to be self-consistent rather than typographically correct.
    fn cell_height(&self) -> u32 {
        self.atlas.map_or(18, |a| u32::from(a.line_pitch()))
    }

    /// Blit a decoded raster into `rect`.
    ///
    /// # Why the blit is 1:1 and no scaler lives here
    ///
    /// §2.9.3's decision is that the Iceberg cache holds **page-column-width** rasters rather than
    /// native ones, because a ±1-page policy with native decoding holds exactly one 1080p image and
    /// the second photo on facing pages breaches the budget. So the downscale has already happened by
    /// the time a pixel is here -- it happened when the raster was admitted -- and `rect` is sized by
    /// the session from the catalog's own `IHDR`, which is where the cache's dimensions came from too.
    ///
    /// A second resampler in this crate would then be a second implementation of §2.9.3's arithmetic
    /// and a second thing to disagree with it. So there is none: if `rect` and the raster agree, the
    /// blit is a copy; if they do not, the two callers have drifted and the painter says so.
    ///
    /// # Why a size mismatch is refused rather than stretched
    ///
    /// Nearest-neighbour stretching a photograph is not a degraded version of the image, it is a
    /// different image, and there is no reading of it that is correct. `PaintStats::images_missing`
    /// counts it, which makes the drift a number in a frame rather than a picture that is quietly
    /// wrong.
    fn image(
        &mut self,
        frame: &mut Frame,
        rect: &Rect,
        asset_id: AssetId,
        damage: Option<DamageRect>,
        rasters: Option<&dyn RasterSource>,
    ) {
        let area = rect.bounds();
        let Some(area) = area else {
            self.scratch.stats.images_missing += 1;
            return;
        };
        if !intersects(area, damage) {
            // Skipped for damage, not missing: the distinction is the same one `rects_skipped` makes
            // for a rect, and conflating them would make a scrolled-past image look like a lost one.
            self.scratch.stats.rects_skipped += 1;
            return;
        }
        let Some(source) = rasters else {
            // A painter with no raster source is the normal state for a document with no images, so
            // this is counted rather than treated as a failure -- but it is counted separately from a
            // cache miss so the two are distinguishable in a frame's stats.
            self.scratch.stats.images_missing += 1;
            return;
        };
        let Some(raster) = source.raster(asset_id) else {
            self.scratch.stats.images_missing += 1;
            return;
        };
        if raster.width != rect.width || raster.height != rect.height {
            self.scratch.stats.images_missing += 1;
            return;
        }

        // Alpha is honoured rather than ignored: a PNG with a transparent background must not paint
        // the frame's colour over the text it sits on. The blend is integer, per the Zero-Bézier
        // Invariant's spirit -- no float anywhere in this crate's blitters.
        let fill = match damage {
            Some(d) => area.clip(&d),
            None => area,
        };
        let written = blit_rgba(
            frame,
            &fill,
            rect.x,
            rect.y,
            raster.width,
            raster.height,
            raster.pixels,
        );
        self.scratch.stats.pixels += written;
        self.scratch.stats.image_pixels += written;
        self.scratch.stats.resampled += 1;
    }
}

/// Blit an RGBA raster at `(x, y)`, 1:1, into `dst`.
///
/// `dst` is a **clipped** rect: `paint` clips the node's bounds to the damage before calling, so the
/// raster's own coordinates have to be recovered by subtracting the node's origin. That is why `x` and
/// `y` are passed alongside `dst` rather than being read out of it.
///
/// Returns the pixels written. The row loop skips rows the frame does not have, and both the row loop
/// and the inner loop clip to the damage rect -- so a raster hanging off the frame, or straddling any
/// edge of the damage, paints only the pixels inside both, and nothing is written where the caller did
/// not ask for paint.
fn blit_rgba(
    frame: &mut Frame,
    dst: &DamageRect,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    pixels: &[u8],
) -> u64 {
    let mut written = 0u64;
    for r in 0..h {
        let sy = y + r as i32;
        if sy < 0 {
            continue;
        }
        let Ok(py) = u32::try_from(sy) else { continue };
        // **Clip to the damage rect, and this is the vertical twin of the column clip below.** The
        // horizontal axis has had it from the start (`px < dst.x || px >= dst.x + dst.width`), so an
        // image straddling the left or right edge of the damage painted only the damaged columns. The
        // vertical axis did not, so the same image straddling the *top or bottom* edge painted every
        // row the frame had -- which is a correctness bug (pixels written outside the damage rect the
        // caller asked for) and, in a damage-driven renderer, a performance one: the whole point of
        // `paint(..., Some(damage))` is that untouched rows are not touched. `dst.y`/`dst.height` are
        // `u32`, so the comparison is done after the `try_from` that the frame-bound check already
        // needs, keeping the two clips symmetrical.
        if py < dst.y || py >= dst.y.saturating_add(dst.height) {
            continue;
        }
        let dst_row = frame.row_mut(py);
        if dst_row.is_empty() {
            continue;
        }
        let src_row = r as usize * w as usize;
        for cx in 0..w {
            let sx = x + cx as i32;
            if sx < 0 {
                continue;
            }
            let Ok(px) = u32::try_from(sx) else { continue };
            if px >= dst_row.len() as u32 {
                break;
            }
            // The pixel's position inside the *clipped* rect, which is what decides whether it is
            // inside the damage at all.
            if px < dst.x || px >= dst.x.saturating_add(dst.width) {
                continue;
            }
            let at = (src_row + cx as usize).saturating_mul(4);
            let Some(px4) = pixels.get(at..at + 4) else {
                // A short raster means the cache admitted something that is not `w * h * 4`, which
                // `IcebergCache::insert` already refuses. Stopping here rather than reading past the
                // end is belt to that braces, and the count below makes it visible.
                break;
            };
            let (r8, g8, b8, a8) = (px4[0], px4[1], px4[2], px4[3]);
            if a8 == 0 {
                continue;
            }
            let under = dst_row[px as usize];
            // Integer source-over: `src * a + dst * (255 - a)`, divided by 255.
            //
            // **All three channels use `/ 255`, and that is not interchangeable with `>> 8`.** The first
            // version divided red by 255 and shifted the other two right by 8 -- the standard
            // "good enough" fast blend -- and it loses the low bit of green and blue, so an *opaque*
            // pixel came out as `(10, 19, 29)` where the source said `(10, 20, 30)`. An opaque blit has
            // to be the identity: at `a == 255` there is no blending to do, and a blit that alters the
            // colours of an opaque image reads as "the images look slightly wrong" rather than as a bug.
            // `>> 8` is off by up to 1 in 255 on *every* pixel -- invisible on a photograph, a visible
            // band on a flat one, and exactly wrong in a test that asserts a solid colour came through.
            //
            // `u32` intermediates because 255 * 255 * 2 is 130,050, which overflows a `u16` and would be
            // one wrap away from overflowing an `i32`.
            let a = u32::from(a8);
            let inv = 255 - a;
            let mix = |src: u8, dst: u32| (u32::from(src) * a + dst * inv) / 255;
            let out = 0xFF00_0000
                | (mix(b8, (under >> 16) & 0xFF) << 16)
                | (mix(g8, (under >> 8) & 0xFF) << 8)
                | mix(r8, under & 0xFF);
            dst_row[px as usize] = out;
            written += 1;
        }
    }
    written
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
        // The fifth face, and the one arm that must be written out rather than left to the `_`.
        //
        // `Style::Math` falls into `_ => Regular` correctly *today*, because `Regular` is 0 and the
        // matcher compares equality rather than indexing. It is still worth its own arm: the failure
        // mode of getting it wrong is that every `\alpha` and `\sum` is looked up in Inter, which has
        // neither, so `MetricTable::get` returns `GlyphMetric::BLANK` and the symbol draws as nothing.
        // A formula with a silent hole in it reads as a rendering bug rather than as a wrong style.
        x if x == holonomy_render::Style::MATH.0 => AtlasStyle::Math,
        _ => AtlasStyle::Regular,
    }
}

/// Integer square root (floor), for rounded-rect scanlines.
fn isqrt(n: u64) -> u64 {
    if n == 0 { return 0; }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x { x = y; y = (x + n / x) / 2; }
    x
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
