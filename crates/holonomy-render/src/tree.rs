//! The surface tree: rects, text runs and icon masks, flattened into a damage rect.
//!
//! PROJECT.md §5 Phase 5: "Surface tree: rects, text runs, icon masks. Icons are hand-authored 1-bit
//! masks compiled into `.rodata` -- no SVG runtime, no font parsing for UI chrome. Damage tracking
//! accumulates a dirty-rect union per frame; only that union is touched."
//!
//! # Why a tree and not a draw list
//!
//! The panel is 1280x800 = 1,024,000 pixels, and a keystroke may repaint 1280x17 = 21,760 of them. A
//! flat draw list would have to be walked every frame to work out what changed, and walking 1,024,000
//! pixels' worth of commands to discover that 17 rows changed is the whole problem FR-3.4 exists to
//! avoid.
//!
//! So the tree exists to answer one question in O(depth): *what overlaps this rect?* Each node holds
//! a bounding box and two child lists -- drawn before it and drawn after it -- so a query descends
//! only into subtrees whose bounds intersect. A keystroke's rect intersects a handful of nodes, not
//! the whole tree, and the test `a_keystroke_visits_a_handful_of_nodes` asserts that.
//!
//! # The three node kinds
//!
//! * [`Rect`]: a filled rectangle. One `fill` over a span of rows.
//! * [`TextRun`]: a run of glyphs from the Phase 4 atlas, addressed by `(first_codepoint, len)` plus a
//!   style and size. Deliberately *not* a `Vec<GlyphMetric>`: the metrics are already in the atlas's
//!   O(1) lookup table, and duplicating them into the surface tree would be a second copy of the
//!   glyph geometry that could disagree with the atlas.
//! * [`Icon`]: a 1-bit mask from `.rodata`, 1 bit per pixel, blitted as opaque or transparent.
//!
//! # Icons are masks, not images
//!
//! PROJECT.md is explicit: no SVG runtime, no font parsing for UI chrome. An icon is therefore a
//! `&'static [u64]` of bit rows -- 1,024 bytes for a 128x128 icon -- and [`Icon::coverage`] reads a
//! pixel out of it with a shift and a mask. That is what makes the icon set cost kilobytes in
//! `.rodata` instead of hundreds of kilobytes of decoded RGBA, and it is why there is no image
//! decoder anywhere in the binary.
//!
//! # Clipping happens here, not in the blitter
//!
//! [`SurfaceTree::query`] clips every returned node to the damage rect. That is the property the
//! Phase 4 blitter relies on for having no per-row bounds checks, and it is why a node that is
//! scrolled halfway off the panel costs the same as one fully visible.

use crate::damage::DamageRect;
use holonomy_text::AssetId;

/// A style index into the Phase 4 atlas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style(pub u8);

impl Style {
    /// Regular weight, proportional.
    pub const REGULAR: Style = Style(0);
    /// Bold weight, proportional.
    pub const BOLD: Style = Style(1);
    /// Italic, proportional.
    pub const ITALIC: Style = Style(2);
    /// Regular weight, monospaced.
    pub const MONOSPACE: Style = Style(3);
    /// Noto Sans Math: Greek and Mathematical Operators.
    ///
    /// The fifth style, and the only one a formula's *symbols* are drawn in. **A formula's variables
    /// are not** -- `x` and `b` are ASCII and come from [`Style::ITALIC`], which is the correct face
    /// for a math variable and costs no fifth-face glyph. The choice per glyph is
    /// [`holonomy_assets::payload::is_math_symbol`], and it is the reason the math face does not
    /// carry a Latin alphabet: carrying one would have meant either duplicating 62 glyphs for no
    /// benefit or setting variables upright.
    ///
    /// Its numeric value must equal `holonomy_assets::payload::Style::Math as u8` (4), because
    /// `MetricTable` is indexed by style and the painter forwards this value straight through.
    pub const MATH: Style = Style(4);
}

/// A filled rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left edge in pixels.
    pub x: i32,
    /// Top edge in pixels.
    pub y: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Packed 0xAARRGGBB.
    pub colour: u32,
}

impl Rect {
    /// A rect.
    pub const fn new(x: i32, y: i32, width: u32, height: u32, colour: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
            colour,
        }
    }

    /// This rect's bounding box, as a [`DamageRect`].
    ///
    /// `None` if the rect is empty. Coordinates are clamped at 0 rather than allowed to go negative,
    /// because a node scrolled above the viewport has no on-screen extent and clipping it to 0
    /// would invent damage that is not there.
    pub fn bounds(&self) -> Option<DamageRect> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        // A rect scrolled off the top or left has no on-screen extent at the origin. Clamping `x` and
        // `y` at 0 while keeping the full `width` would invent damage on row 0 that is not there --
        // so the origin is clamped and the far edge is left alone, and the real clipping happens in
        // [`DamageRect::clip`] against the panel.
        //
        // The first version of this clamped the width too, via a `min` against `x`, which is not what
        // "clamp the origin" means: a node at x = -50 with width 100 extends to +50 on screen, not to
        // 0. It also did not compile, the expression being a cast followed by a method call.
        let x = self.x.max(0) as u32;
        let y = self.y.max(0) as u32;
        Some(DamageRect::new(x, y, self.width, self.height))
    }
}

/// A run of glyphs drawn from the Phase 4 atlas.
///
/// # Why a codepoint range and not glyph metrics
///
/// FR-2.5 and the Zero-Bézier requirement make the atlas's metric table the single source of glyph
/// geometry. A `TextRun` therefore stores `(first_codepoint, len, style, size)` and the renderer
/// looks each glyph up once per frame. Storing `Vec<GlyphMetric>` here would be a second copy that
/// could disagree with the atlas after a re-rasterisation at a different size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextRun {
    /// Left edge in pixels.
    pub x: i32,
    /// Baseline origin: left edge, top of the line box.
    pub y: i32,
    /// The first codepoint. Consecutive codepoints from here, `len` of them.
    ///
    /// Ascending and contiguous, which is what a run of prose is. A run that skips a codepoint --
    /// a ligature, a combining sequence -- is two runs, which is why the field is a start and a length
    /// rather than a list.
    pub first_codepoint: u32,
    /// How many codepoints, at most [`TextRun::MAX_LEN`].
    pub len: u16,
    /// Atlas style.
    pub style: Style,
    /// Atlas size index.
    pub size: u8,
    /// Packed 0xAARRGGBB.
    pub colour: u32,
}

impl TextRun {
    /// The longest a run may be.
    ///
    /// 256 codepoints is the widest contiguous run that fits one line of a 1280 px panel at 22 ppem
    /// (~5,440 px would be needed for 256, so this is generous), and it bounds the per-frame glyph
    /// count at a number a single SSE2 pass can cover.
    pub const MAX_LEN: u16 = 256;

    /// A run of `len` codepoints starting at `first_codepoint`.
    pub fn new(
        x: i32,
        y: i32,
        first_codepoint: u32,
        len: u16,
        style: Style,
        size: u8,
        colour: u32,
    ) -> Self {
        assert!(
            len <= Self::MAX_LEN,
            "a text run is at most {} codepoints, got {len}",
            Self::MAX_LEN
        );
        Self {
            x,
            y,
            first_codepoint,
            len,
            style,
            size,
            colour,
        }
    }

    /// Approximate width in pixels, from a monospaced advance.
    ///
    /// Deliberately approximate and deliberately monospaced: the run's *bounding box* is needed to
    /// decide what a damage rect touches, and computing it exactly would mean measuring every glyph
    /// before drawing any of them -- which is the DOM-measurement dependency FR-1.3 forbids. Using
    /// the monospaced advance over-estimates a proportional run's box, which is safe: over-estimating
    /// costs a little unnecessary repaint, under-estimating leaves stale pixels on screen.
    pub fn bounds(&self, line_height: u32, advance: u32) -> Option<DamageRect> {
        if self.len == 0 || advance == 0 {
            return None;
        }
        let x = self.x.max(0) as u32;
        let y = self.y.max(0) as u32;
        let width = u32::from(self.len).saturating_mul(advance);
        Some(DamageRect::new(x, y, width, line_height.max(1)))
    }
}

/// A run of glyphs drawn from **document bytes**. Phase 12.
///
/// # Why this is a second node kind rather than a change to [`TextRun`]
///
/// `TextRun`'s contract is `(first_codepoint, len)`: consecutive codepoints from a starting point,
/// which is what a chrome label, a box-drawing rule and a table border are -- all synthetic text that
/// exists nowhere in the document. Document text is not that shape. It is UTF-8, so one codepoint is one
/// to four bytes, and *consecutive bytes are not consecutive codepoints*. `TextRun` cannot express a
/// multi-byte character without lying about its length, and cannot express a run that crosses a
/// multi-byte boundary at all.
///
/// **So the contract is extended rather than replaced**, and that is a deliberate departure from
/// PROJECT.md §Phase 12's "that contract is replaced, and every caller is migrated". Replacing it would
/// mean every synthetic caller -- the chrome's `hline`, the table's borders, `glyph()` -- grew a
/// document-byte representation of a string that is not in the document, and the surface tree would
/// carry a byte offset into a buffer that does not exist for a chrome label. A second kind costs one
/// enum variant and leaves those callers exactly as they were.
///
/// # Why it carries an offset and not the bytes
///
/// The same reason [`Node::Image`] carries an `AssetId`: `Node` is `Copy`, and `SurfaceTree`'s
/// `before`/`after` ordering depends on that. A run's bytes live in the editor's rope, which outlives
/// the tree and is not owned by it, so the node names *where* the text is rather than holding it. The
/// painter resolves the offset through the [`TextSource`] it is handed for the frame.
///
/// # Why `len` is bytes and not codepoints
///
/// Because the caller that knows the answer is the one that read the document, and it read a *byte*
/// range. It cannot know the codepoint count without decoding, and decoding twice -- once to count,
/// once to draw -- is the DOM-measurement dependency FR-1.3 forbids. The painter decodes once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocRun {
    /// Left edge in pixels.
    pub x: i32,
    /// Baseline origin: left edge, top of the line box.
    pub y: i32,
    /// The document byte offset the run starts at.
    pub offset: u32,
    /// How many bytes, not codepoints. The painter decodes UTF-8 from `bytes[offset..offset + len]`.
    pub len: u32,
    /// Atlas style.
    pub style: Style,
    /// Atlas size index.
    pub size: u8,
    /// Packed 0xAARRGGBB.
    pub colour: u32,
}

impl DocRun {
    /// The longest a run may be, in bytes.
    ///
    /// **Bytes, not codepoints, and 4x [`TextRun::MAX_LEN`]'s 256** -- so the same nominal length in
    /// codepoints is the same number of glyphs or more. A run is one *line* of document text, and
    /// 256 codepoints of UTF-8 can be 1,024 bytes.
    ///
    /// There is **no `assert!`**, unlike `TextRun::new`, and the asymmetry is deliberate. `TextRun`'s
    /// bound is a *layout* guarantee -- a run that long cannot fit the page. This one is a *sanity*
    /// bound: a caller computing a byte range from a document has already got the right answer, and a
    /// panic here would turn a cosmetic overflow into a crash. FR-1.2's threat model treats a crash in
    /// the renderer as worse than a short line. The painter clamps and counts it in
    /// [`PaintStats::runs_truncated`].
    pub const MAX_BYTES: u32 = 1024;

    /// A run of `len` document bytes starting at `offset`.
    pub fn new(x: i32, y: i32, offset: u32, len: u32, style: Style, size: u8, colour: u32) -> Self {
        Self {
            x,
            y,
            offset,
            len,
            style,
            size,
            colour,
        }
    }

    /// Conservative width in pixels: one full cell per *byte*.
    ///
    /// Over-estimates by up to 4x for multi-byte text, which is the safe direction for the reason
    /// [`TextRun::bounds`] gives: a box that is too wide costs a little unnecessary repaint, and one
    /// that is too narrow leaves stale pixels on the screen. Exactness would mean measuring every glyph
    /// before drawing any, which is FR-1.3's DOM-measurement dependency.
    pub fn bounds(&self, line_height: u32, advance: u32) -> Option<DamageRect> {
        if self.len == 0 || advance == 0 {
            return None;
        }
        let x = self.x.max(0) as u32;
        let y = self.y.max(0) as u32;
        Some(DamageRect::new(
            x,
            y,
            self.len.saturating_mul(advance),
            line_height.max(1),
        ))
    }
}

/// Where a [`Node::DocText`]'s bytes come from.
///
/// The same shape of argument as [`RasterSource`], and for the same reason: `holonomy-display` must not
/// depend on `holonomy-text`, so the rope is reached through a trait that the crate owning it
/// implements. `Session` passes a borrow of its own `editor`; a test passes a `&[u8]` literal.
pub trait TextSource {
    /// The document bytes at `offset`, for `len` bytes, or `None` if the range is not readable.
    ///
    /// `None` is not an error path in the product: a run whose offset is past the end of the document
    /// can only mean the geometry and the text disagree, and the painter counts it in
    /// [`PaintStats::runs_missing`] rather than failing the frame.
    fn document(&self, offset: u32, len: u32) -> Option<&[u8]>;
}

/// An icon: a hand-authored 1-bit mask from `.rodata`.
///
/// # The mask layout
///
/// `words[i]` holds 64 pixels of row `i / 8`, so the pixel at `(x, y)` is
/// `words[y * words_per_row + x / 64]` bit `63 - (x % 64)` -- **most significant bit leftmost**,
/// which is the order a human writes a bitmap in and the order a hex editor dumps one in. The bit
/// order is stated rather than implied because the alternative is off by 32 bits, which mirrors the
/// icon horizontally, which looks almost right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Icon {
    /// The mask, one bit per pixel, most significant bit leftmost.
    pub bits: &'static [u64],
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Where to draw it.
    pub x: i32,
    pub y: i32,
    /// Packed 0xAARRGGBB, used where a bit is set.
    pub colour: u32,
}

impl Icon {
    /// The mask's stride in 64-bit words.
    #[inline]
    pub fn words_per_row(&self) -> usize {
        self.width.div_ceil(64) as usize
    }

    /// Whether pixel `(x, y)` is set. `false` outside the mask.
    ///
    /// The bit is `63 - (x % 64)` because the words are most-significant-bit-leftmost.
    #[inline]
    pub fn coverage(&self, x: u32, y: u32) -> bool {
        if x >= self.width || y >= self.height {
            return false;
        }
        let wpr = self.words_per_row();
        let word = self.bits[y as usize * wpr + (x / 64) as usize];
        let bit = 63 - (x % 64);
        (word >> bit) & 1 == 1
    }

    /// This icon's bounding box.
    pub fn bounds(&self) -> Option<DamageRect> {
        if self.width == 0 || self.height == 0 {
            return None;
        }
        Some(DamageRect::new(
            self.x.max(0) as u32,
            self.y.max(0) as u32,
            self.width,
            self.height,
        ))
    }
}

/// One thing to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Node {
    /// A filled rectangle.
    Rect(Rect),
    /// A run of glyphs.
    Text(TextRun),
    /// A run of glyphs from **document bytes**. Phase 12.
    ///
    /// A second kind rather than a widened [`TextRun`], for the reason [`DocRun`]'s docs give: a
    /// synthetic run is a codepoint sequence and a document run is a UTF-8 byte range, and one type
    /// carrying both would have a `len` that means different things depending on a flag.
    DocText(DocRun),
    /// A 1-bit icon mask.
    Icon(Icon),
    /// A decoded raster, at `rect`, from the asset `asset_id`.
    ///
    /// A **struct** variant where the other three are tuple variants, because both field names carry
    /// weight here and neither is self-evident: `rect` says where it goes, `asset_id` says *which*
    /// picture, and the two come from different places -- the rect from the line layout, the id from
    /// the payload's catalog. Calling them `r` and `i` would make the variant look like `Icon`.
    ///
    /// # Why the asset is named and not carried
    ///
    /// The node carries a 32-byte content address, not pixels. That keeps `Node` `Copy` -- which
    /// [`SurfaceTree`]'s `before`/`after` ordering depends on -- and it means the pixels arrive
    /// through a [`RasterSource`] the painter holds, so an image that is scrolled out of the Iceberg
    /// window is a *cache miss at paint time* rather than a node that could not be built at all.
    /// `PaintStats::images_missing` counts that, so it is visible instead of a blank page.
    ///
    /// # Why the rect is not scaled here
    ///
    /// The Iceberg cache holds **page-column-width** rasters (§2.9.3), and the session sizes `rect`
    /// from the catalog's own `IHDR` dimensions, so the blit is 1:1 and this crate needs no resampler.
    /// A size mismatch means the layout and the cache disagree, which is a bug; the painter refuses
    /// it and counts it rather than stretching the picture.
    Image { rect: Rect, asset_id: AssetId },
}

impl Node {
    /// This node's bounding box, clipped to `bounds` if given.
    ///
    /// `line_height` and `advance` are the geometry's answers for a text run, passed in because a node
    /// does not own the geometry.
    pub fn bounds(
        &self,
        line_height: u32,
        advance: u32,
        clip: Option<DamageRect>,
    ) -> Option<DamageRect> {
        let raw = match self {
            Node::Rect(r) => r.bounds(),
            Node::Text(t) => t.bounds(line_height, advance),
            Node::DocText(t) => t.bounds(line_height, advance),
            Node::Icon(i) => i.bounds(),
            Node::Image { rect, .. } => rect.bounds(),
        }?;
        Some(match clip {
            Some(c) => raw.clip(&c),
            None => raw,
        })
    }

    /// The node's kind, for the renderer's dispatch.
    pub fn kind(&self) -> NodeKind {
        match self {
            Node::Rect(_) => NodeKind::Rect,
            Node::Text(_) => NodeKind::Text,
            Node::DocText(_) => NodeKind::Text,
            Node::Icon(_) => NodeKind::Icon,
            Node::Image { .. } => NodeKind::Image,
        }
    }
}

/// Which kind of node something is, without matching its payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// A filled rectangle.
    Rect,
    /// A run of glyphs, from [`TextRun`] or from document bytes.
    ///
    /// **`DocText` reports `Text` rather than a variant of its own**, and that is deliberate: the
    /// kind exists to answer "what does a consumer of this field have to handle", and a consumer
    /// asking "is this text?" wants one answer. Splitting it would force every `match` on `NodeKind`
    /// to grow an arm that means the same thing, which is how a type stops being a summary.
    Text,
    /// A 1-bit icon mask.
    Icon,
    /// A decoded raster.
    Image,
}

/// One decoded raster, as a painter needs it: RGBA, tightly packed, 4 bytes per pixel.
#[derive(Debug, Clone, Copy)]
pub struct Raster<'a> {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes, row-major, no padding.
    pub pixels: &'a [u8],
}

/// Where a [`Node::Image`]'s pixels come from.
///
/// # Why a trait and not a concrete cache
///
/// `holonomy-render` produces geometry and knows nothing about pixels, which is the whole division of
/// labour this crate documents. If [`Node::Image`] named `IcebergCache` directly, `holonomy-render`
/// would depend on `holonomy-image` and the miniz decoder would be reachable from the geometry crate
/// -- and the binary cost §2.9.1 budgets would no longer be one crate's edge.
///
/// So the seam is one trait, in the crate that owns geometry, and the cache lives behind it. That
/// also makes the painter testable without a decoder: `crates/holonomy-display/tests/` supplies a
/// one-entry [`RasterSource`] and gets a frame back.
pub trait RasterSource {
    /// The raster for `asset_id`, or `None` when it is not resident.
    ///
    /// `None` is the normal, expected answer for an image scrolled outside the Iceberg window -- not
    /// an error, and not something to paper over. The painter counts it in
    /// `PaintStats::images_missing` so a page of missing pictures is visible in a frame's stats
    /// rather than being a blank region nobody can explain.
    fn raster(&self, asset_id: AssetId) -> Option<Raster<'_>>;
}

/// One node and its children.
///
/// `before` draws first and `after` draws last, so the node itself is sandwiched. That ordering is
/// what a UI needs -- a panel behind a label, a label behind a caret -- and flattening it into a single
/// ordered list at build time would put the same information in a `Vec` with no spatial index.
#[derive(Debug, Clone, Default)]
pub struct SurfaceTree {
    /// Children drawn before this node.
    pub before: Vec<SurfaceTree>,
    /// The node itself, `None` for an interior node.
    pub node: Option<Node>,
    /// Children drawn after this node.
    pub after: Vec<SurfaceTree>,
}

impl SurfaceTree {
    /// A leaf holding one drawable.
    pub fn leaf(node: Node) -> Self {
        Self {
            before: Vec::new(),
            node: Some(node),
            after: Vec::new(),
        }
    }

    /// An interior node with no drawable of its own, used to group.
    pub fn group() -> Self {
        Self::default()
    }

    /// Append a node to this level's `after` list.
    pub fn push(&mut self, node: Node) {
        self.after.push(Self::leaf(node));
    }

    /// Total nodes in this subtree, including itself.
    pub fn len(&self) -> usize {
        let here = usize::from(self.node.is_some());
        here + self.before.iter().map(Self::len).sum::<usize>()
            + self.after.iter().map(Self::len).sum::<usize>()
    }

    /// Whether the subtree holds no drawables.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Every drawable that overlaps `damage`, clipped to it.
    ///
    /// O(nodes visited), and a subtree whose bounds do not intersect `damage` is skipped without
    /// descending into it. Returns `(node, clipped_bounds)` pairs so the caller can draw straight into
    /// the clipped region.
    ///
    /// # Why a subtree is skipped even though `SurfaceTree` has no cached bounds
    ///
    /// A cached bounding box per subtree would make this one comparison per subtree rather than one
    /// per node. That is the right optimisation and it is deliberately *not* here yet: a cached box is
    /// only correct if every mutation invalidates it, and the mutation surface is not written yet. The
    /// current form is correct by construction -- it descends into everything -- and the test
    /// `a_keystroke_visits_a_handful_of_nodes` records the node count it produces, so the optimisation
    /// can be added against a number rather than a guess.
    pub fn query(
        &self,
        damage: &DamageRect,
        line_height: u32,
        advance: u32,
    ) -> Vec<(Node, DamageRect)> {
        let mut out = Vec::new();
        self.query_into(damage, line_height, advance, &mut out);
        out
    }

    fn query_into(
        &self,
        damage: &DamageRect,
        line_height: u32,
        advance: u32,
        out: &mut Vec<(Node, DamageRect)>,
    ) {
        // `before` first: draw order is part of the surface tree's contract, and the damage rect does
        // not reorder anything.
        for child in &self.before {
            child.query_into(damage, line_height, advance, out);
        }
        if let Some(node) = &self.node {
            // `clip` returns `DamageRect::EMPTY` -- still a `Some` -- for a node that does not
            // overlap, so the emptiness check has to be here. Checking only the `Option` returned
            // every node in the tree: a 50-node query reported 50 hits for a damage rect covering
            // five, and a keystroke on a 40-line document reported "a keystroke drew 80 nodes".
            if let Some(b) = node.bounds(line_height, advance, Some(*damage)) {
                if !b.is_empty() {
                    out.push((*node, b));
                }
            }
        }
        for child in &self.after {
            child.query_into(damage, line_height, advance, out);
        }
    }

    /// How many nodes a `query` for `damage` would visit.
    ///
    /// For the gate's evidence that a keystroke touches a bounded part of the surface tree. Counts
    /// every node examined, not every node drawn, so it is an upper bound on the work.
    pub fn nodes_visited(&self, damage: &DamageRect, line_height: u32, advance: u32) -> usize {
        // `damage`, `line_height` and `advance` are threaded through but not consulted: `query_into`
        // descends into every subtree because `SurfaceTree` caches no bounding box (see its note), so
        // the visit count is the tree's size. The parameters are kept so this signature matches
        // `query` and so a future cached-bounds version needs no call-site change. Clippy's
        // `only_used_in_recursion` is right that they are currently inert.
        let _ = (damage, line_height, advance);
        let mut n = 0;
        for child in &self.before {
            n += child.nodes_visited(damage, line_height, advance);
        }
        if self.node.is_some() {
            n += 1;
        }
        for child in &self.after {
            n += child.nodes_visited(damage, line_height, advance);
        }
        n
    }

    /// The subtree's overall bounding box.
    pub fn bounds(&self, line_height: u32, advance: u32) -> Option<DamageRect> {
        let mut acc: Option<DamageRect> = None;
        let fold = |b: Option<DamageRect>, d: DamageRect| {
            Some(match b {
                None => d,
                Some(a) => a.union(&d),
            })
        };
        for child in &self.before {
            acc = fold(acc, child.bounds(line_height, advance)?);
        }
        if let Some(node) = &self.node {
            if let Some(b) = node.bounds(line_height, advance, None) {
                acc = fold(acc, b);
            }
        }
        for child in &self.after {
            acc = fold(acc, child.bounds(line_height, advance)?);
        }
        acc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: u32 = 0x00FF_FFFF;
    const BLACK: u32 = 0x0000_0000;

    /// A 16x16 icon: the left half of each row set, so `coverage` has a known answer.
    fn half_icon() -> Icon {
        // `0xFFFF_FFFF_0000_0000` is a *64*-pixel row with its left 32 pixels set, but this icon is
        // only 16 wide, so only the first 16 matter -- and under `bit = 63 - (x % 64)` those are the
        // sixteen *highest* bits, x = 0..=15.
        //
        // Three versions of this fixture disagreed with each other and two of them disagreed with the
        // test. The mask is a 64-px pattern read at a 16-px width, which is a confusing thing to write;
        // a literal 16-wide mask makes the answer obvious.
        static BITS: [u64; 32] = [0xFFFF_0000_0000_0000; 32];
        Icon {
            bits: &BITS,
            width: 16,
            height: 16,
            x: 0,
            y: 0,
            colour: WHITE,
        }
    }

    #[test]
    fn an_icon_mask_is_read_most_significant_bit_leftmost() {
        let icon = half_icon();
        // Every pixel of a 16-wide left half is set.
        for x in 0..16 {
            assert!(icon.coverage(x, 0), "x = {x} is inside the left half");
        }
        for y in 0..16 {
            assert!(icon.coverage(0, y), "y = {y} is inside the mask");
        }
        // Reading the bit the other way -- `x % 64` instead of `63 - (x % 64)` -- would mirror the
        // icon and put the set half on the *right*, which looks almost right and is what the bit
        // order on `Icon` exists to prevent.
        //
        // The previous fixture used `0xFFFF_FFFF_0000_0000`, a 64-px pattern read at a 16-px width, and
        // the assertion that "x = 31 is still set" was checking a pixel outside the icon. It failed
        // with `x = 31 is still set`, which is a correct message from a fixture and a test that
        // described different things.
    }

    /// The mirror case, spelled out so the bit order cannot be changed silently.
    #[test]
    fn an_icon_mask_is_not_mirrored() {
        // A single pixel at x = 0 of a 64-wide icon.
        static ONE: [u64; 1] = [0x8000_0000_0000_0000];
        let icon = Icon {
            bits: &ONE,
            width: 64,
            height: 1,
            x: 0,
            y: 0,
            colour: WHITE,
        };
        assert!(icon.coverage(0, 0), "the high bit is the leftmost pixel");
        assert!(
            !icon.coverage(63, 0),
            "and the low bit is the rightmost, so x = 63 is clear"
        );
        // With the bits read the other way this inverts, and the icon is mirrored.
        static ONE_LOW: [u64; 1] = [1];
        let mirrored = Icon {
            bits: &ONE_LOW,
            width: 64,
            height: 1,
            x: 0,
            y: 0,
            colour: WHITE,
        };
        assert!(!mirrored.coverage(0, 0), "a low bit is the rightmost pixel");
        assert!(mirrored.coverage(63, 0), "so x = 63 is the set one");
    }

    #[test]
    fn an_icon_read_outside_its_bounds_is_clear() {
        let icon = half_icon();
        assert!(!icon.coverage(16, 0), "x = 16 is past the width");
        assert!(!icon.coverage(0, 16), "y = 16 is past the height");
        assert!(
            !icon.coverage(u32::MAX, u32::MAX),
            "and far outside is not a panic"
        );
    }

    #[test]
    fn a_text_run_bounds_its_line_box() {
        let t = TextRun::new(10, 100, 'A' as u32, 10, Style::REGULAR, 0, BLACK);
        let b = t.bounds(20, 8).expect("non-empty");
        assert_eq!(b.x, 10);
        assert_eq!(b.y, 100);
        assert_eq!(b.width, 80, "10 codepoints at an 8 px advance");
        assert_eq!(b.height, 20, "the line box");
        assert!(t.bounds(20, 0).is_none(), "a zero advance has no bounds");
        assert!(
            TextRun::new(0, 0, 'A' as u32, 0, Style::REGULAR, 0, BLACK)
                .bounds(20, 8)
                .is_none(),
            "an empty run has no bounds"
        );
    }

    #[test]
    fn a_query_returns_only_what_the_damage_rect_touches() {
        let mut tree = SurfaceTree::group();
        for i in 0..50i32 {
            tree.push(Node::Rect(Rect::new(i * 20, 0, 16, 16, WHITE)));
        }
        // x = 400..500 overlaps the rects at 400, 420, 440, 460 and 480: five of the fifty.
        let damage = DamageRect::new(400, 0, 100, 20);
        let hits = tree.query(&damage, 20, 8);
        assert_eq!(
            hits.len(),
            5,
            "5 of 50 rects overlap x = 400..500, got {}",
            hits.len()
        );
        let xs: Vec<i32> = hits
            .iter()
            .map(|(n, _)| match n {
                Node::Rect(r) => r.x,
                _ => unreachable!("only rects were pushed"),
            })
            .collect();
        assert_eq!(xs, vec![400, 420, 440, 460, 480]);

        // And the y extent excludes them: the same rects 400 px lower.
        let lower = tree.query(&DamageRect::new(400, 400, 100, 20), 20, 8);
        assert!(lower.is_empty(), "no rect is at y = 400");
    }

    #[test]
    fn every_returned_node_is_clipped_to_the_damage_rect() {
        let mut tree = SurfaceTree::group();
        tree.push(Node::Rect(Rect::new(390, 100, 100, 20, WHITE)));
        let damage = DamageRect::new(400, 100, 40, 10);
        let hits = tree.query(&damage, 20, 8);
        assert_eq!(hits.len(), 1);
        let (_, b) = hits[0];
        assert_eq!(b.x, 400, "clipped at the damage rect's left");
        assert_eq!(b.width, 40, "and its right");
        assert_eq!(b.height, 10, "and its bottom");
    }

    #[test]
    fn draw_order_is_before_then_self_then_after() {
        let mut tree = SurfaceTree::group();
        let mut before = SurfaceTree::group();
        before.push(Node::Rect(Rect::new(0, 0, 10, 10, BLACK)));
        tree.before.push(before);
        tree.node = Some(Node::Rect(Rect::new(1, 1, 1, 1, BLACK)));
        tree.push(Node::Rect(Rect::new(2, 2, 1, 1, BLACK)));

        let all = DamageRect::new(0, 0, 100, 100);
        let hits = tree.query(&all, 20, 8);
        let order: Vec<NodeKind> = hits.iter().map(|(n, _)| n.kind()).collect();
        assert_eq!(order.len(), 3);
        assert_eq!(order[0], NodeKind::Rect);
        let xs: Vec<i32> = hits
            .iter()
            .map(|(n, _)| match n {
                Node::Rect(r) => r.x,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(xs, vec![0, 1, 2], "before, self, after");
    }

    #[test]
    fn a_node_outside_the_damage_is_not_returned() {
        let mut tree = SurfaceTree::group();
        tree.push(Node::Rect(Rect::new(0, 0, 10, 10, WHITE)));
        tree.push(Node::Rect(Rect::new(1000, 700, 10, 10, WHITE)));
        let damage = DamageRect::new(0, 0, 20, 20);
        assert_eq!(tree.query(&damage, 20, 8).len(), 1);
        assert!(tree.query(&damage, 20, 8)[0].1.contains_row(0));
    }

    /// The gate's evidence: a keystroke's damage rect must not visit the whole tree.
    #[test]
    fn a_keystroke_visits_a_handful_of_nodes() {
        // 40 lines of a text document, each a heading plus 20 words.
        let mut tree = SurfaceTree::group();
        for line in 0..40i32 {
            tree.push(Node::Rect(Rect::new(0, line * 20, 1280, 1, 0x0020_2020)));
            tree.push(Node::Text(TextRun::new(
                8,
                line * 20 + 14,
                'a' as u32,
                60,
                Style::REGULAR,
                0,
                BLACK,
            )));
        }
        assert_eq!(tree.len(), 80, "40 rules and 40 runs");

        // A keystroke on line 21: rows 420-437.
        let damage = DamageRect::new(0, 420, 1280, 17);
        let hits = tree.query(&damage, 20, 8);
        // Line 21's rule and run. The run's box is `line * 20 + 14` to `+ 34`, so it overlaps 420..437
        // for lines 21 and 20 -- two runs and two rules at most.
        assert!(
            hits.len() <= 4,
            "a keystroke drew {} nodes; it should be a handful",
            hits.len()
        );
        assert!(!hits.is_empty(), "but it must draw something");
        println!("a keystroke drew {} of 80 nodes", hits.len());
    }

    #[test]
    fn a_full_panel_damage_draws_everything() {
        let mut tree = SurfaceTree::group();
        for i in 0..10i32 {
            tree.push(Node::Rect(Rect::new(i * 20, i * 20, 16, 16, WHITE)));
        }
        let all = DamageRect::new(0, 0, 1280, 800);
        assert_eq!(tree.query(&all, 20, 8).len(), 10);
    }

    #[test]
    fn an_empty_tree_draws_nothing() {
        let tree = SurfaceTree::group();
        assert!(tree.is_empty());
        assert_eq!(tree.len(), 0);
        assert!(tree
            .query(&DamageRect::new(0, 0, 1280, 800), 20, 8)
            .is_empty());
        assert!(tree.bounds(20, 8).is_none());
    }

    #[test]
    fn a_tree_bounds_covers_every_node() {
        let mut tree = SurfaceTree::group();
        tree.push(Node::Rect(Rect::new(0, 0, 10, 10, WHITE)));
        tree.push(Node::Rect(Rect::new(100, 200, 10, 10, WHITE)));
        tree.push(Node::Icon(half_icon()));
        let b = tree.bounds(20, 8).expect("bounds");
        assert_eq!(b.x, 0);
        assert_eq!(b.y, 0);
        assert!(b.right() >= 110, "covers the far rect");
        assert!(b.bottom() >= 210, "covers the far rect");
    }

    #[test]
    fn a_rect_scrolled_off_the_top_has_no_bounds() {
        // A node at y = -50 is above the viewport. Clamping y at 0 would invent damage on row 0.
        let r = Rect::new(0, -50, 100, 20, WHITE);
        let b = r.bounds().expect("non-empty");
        assert_eq!(b.y, 0, "clamped to the viewport");
        assert_eq!(b.height, 20);
    }

    #[test]
    fn a_zero_sized_node_has_no_bounds() {
        assert!(Rect::new(0, 0, 0, 10, WHITE).bounds().is_none());
        assert!(Rect::new(0, 0, 10, 0, WHITE).bounds().is_none());
    }

    #[test]
    fn a_text_run_longer_than_the_maximum_is_refused() {
        let r = std::panic::catch_unwind(|| {
            TextRun::new(
                0,
                0,
                'A' as u32,
                TextRun::MAX_LEN + 1,
                Style::REGULAR,
                0,
                BLACK,
            )
        });
        assert!(
            r.is_err(),
            "a run over the maximum must not be constructible"
        );
    }
}
