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
    AssetId, DamageRect, DocRun, Node, RasterSource, Rect, SurfaceTree, TextRun, TextSource,
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
            Node::Text(run) => self.text(frame, run, damage),
            Node::DocText(run) => self.doc_text(frame, run, damage, text),
            Node::Image { rect, asset_id } => self.image(frame, &rect, asset_id, damage, rasters),
            Node::Icon(_) => {
                // Icons are hand-authored 1-bit masks from `.rodata`, and nothing in the chrome uses
                // one. Refusing to draw them beats drawing a wrong one: a silently blank icon is a bug
                // report with no reproduction, and a silently *wrong* one is worse. Counted, so it
                // appears in the stats rather than in nothing.
                self.scratch.stats.rects_skipped += 1;
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
            // One damage test for the whole glyph, box drawing included, because the damage model
            // and the cell are the same rectangle. See the note on the blit below for why this is
            // the cell and not the ink.
            let cell = DamageRect::new(x.max(0) as u32, run.y.max(0) as u32, cell_w, cell_h);
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
            let ascent = i32::from(self.vertical(run.style, size).0);
            let coverage = atlas.coverage();
            blit_coverage(
                &mut self.scratch,
                frame,
                x + i32::from(m.bearing_x),
                run.y + ascent - i32::from(m.bearing_y),
                u32::from(m.width),
                u32::from(m.height),
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
            pen += i32::from(m.advance_x).max(1);
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
/// Returns the pixels written. The row loop skips rows the frame does not have, and the inner loop
/// clips each row to the frame, so a raster hanging off the right or bottom edge is normal rather
/// than something to guard against.
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
        let dst_row = frame.row_mut(u32::try_from(sy).unwrap_or(u32::MAX));
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

/// Whether `rect` intersects `damage`; no damage rect means "everything".
#[inline]
fn intersects(rect: DamageRect, damage: Option<DamageRect>) -> bool {
    let Some(d) = damage else { return true };
    if rect.width == 0 || rect.height == 0 {
        return false;
    }
    rect.x < d.right() && d.x < rect.right() && rect.y < d.bottom() && d.y < rect.bottom()
}
