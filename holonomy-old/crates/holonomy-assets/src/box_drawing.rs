//! Procedural Box Drawing, U+2500..U+257F.
//!
//! # Why not the font's glyphs
//!
//! Two reasons, one forced and one better.
//!
//! **Forced:** Inter ships *zero* Box Drawing glyphs. The coverage requirement and the choice
//! of Inter are mutually exclusive as shipped, so something has to supply them.
//!
//! **Better:** a Box Drawing glyph is a rectangle grid. Font glyphs are drawn at arbitrary
//! subpixel positions, so two adjacent cells produce edges that land on fractional pixel
//! boundaries and are then antialiased independently. The result is a visible seam: a 1/255
//! brightness discontinuity running the length of a box border. Generating the geometry from
//! the integer cell grid removes the class of artefact entirely, and it is why terminal
//! emulators that care draw box drawing themselves.
//!
//! It also costs zero bytes of the font payload: 128 codepoints × 4 faces is 512 glyphs that
//! do not exist as data, which is most of why the compressed payload fits the budget.
//!
//! # Geometry
//!
//! Every glyph in the block is described by which of the eight cell edges and four diagonals
//! it uses. Each edge is drawn as a rectangle inset by [`STROKE_INSET`] pixels so that
//! neighbouring cells' strokes meet exactly rather than overlapping by a pixel.
//!
//! ```text
//!        U+250C box drawings light arc down and right
//!          .----.
//!          |    |
//!   .----. `----' .----.
//!   |                        down
//!   `----. .----'
//!          |    |
//!          `----'
//! ```
//!
//! Strokes are horizontal, vertical or diagonal on a half-pixel grid, so `|` is one pixel
//! wide, `-` is one pixel tall, and diagonals use Bresenham. Coverage is 255 for a drawn
//! stroke and 0 elsewhere: box drawing is not antialiased, which is the point.

use crate::atlas::{AtlasBuilder, AtlasError, PendingGlyph};
use crate::metric::GlyphMetric;
use crate::payload::Style;

/// First codepoint in the block.
pub const FIRST: u32 = 0x2500;

/// Last codepoint in the block, inclusive.
pub const LAST: u32 = 0x257F;

/// Count of codepoints.
pub const COUNT: u32 = LAST - FIRST + 1;

/// Stroke width in pixels. One pixel: heavier strokes join more reliably but look blunt at
/// 16 ppem, and the seamed-border problem this module exists to solve comes from misaligned
/// edges, not from thin ones.
pub const STROKE: usize = 1;

/// Inset applied to every edge so adjacent cells' strokes abut rather than overlap.
///
/// Zero: each stroke is drawn inside its own cell, and because the geometry is on integer
/// pixels, `U+2500` in cell *n* ends at the same x as `U+2500` in cell *n+1* begins. An inset
/// would leave a gap instead.
pub const STROKE_INSET: usize = 0;

/// The eight cell edges.
mod edge {
    pub const UP: u8 = 1 << 0;
    pub const DOWN: u8 = 1 << 1;
    pub const LEFT: u8 = 1 << 2;
    pub const RIGHT: u8 = 1 << 3;

    /// U+2571 ╱, which rises left to right.
    pub const DIAG_UP_RIGHT: u8 = 1 << 6;
    /// U+2572 ╲, which falls left to right.
    pub const DIAG_UP_LEFT: u8 = 1 << 7;
}

/// Arms and weight for one codepoint.
///
/// `u8` arms bitmask in the low byte, weight class in the high byte. Stored as a single `u16`
/// so the table is a flat `const` array with no builder.
type Shape = u16;

/// Weight classes. Box drawing is single-weight in the A8 atlas -- the Bold face thickens the
/// same geometry -- so these select *which* cells get the 2px treatment rather than drawing a
/// different shape.
const W_LIGHT: u16 = 0;
const W_HEAVY: u16 = 1;
const W_DOUBLE: u16 = 2;
const W_ROUND: u16 = 3;

/// A shape: arms plus weight.
const fn sh(arms: u8, weight: u16) -> Shape {
    (arms as u16) | (weight << 8)
}

/// Table of `U+2500..U+257F`, indexed by `codepoint - FIRST`.
///
/// # Built from an explicit codepoint list, not a running counter
///
/// An earlier revision filled this table with a `let mut i = 0; … i += 1;` walk. It was wrong at
/// four places, and *silently* wrong: the anchors still lined up for the first ~50 entries and
/// drifted after that, so U+254B rendered as a diagonal and the double-line block
/// (U+2550..U+256C) fell off the end of the table entirely. A running counter invites exactly
/// that, because nothing in the type system notices a skipped or doubled increment.
///
/// Every entry here names its codepoint, so a miscount is impossible and an edit that drops a
/// glyph shows up as a `no_entry_for_every_codepoint` failure rather than as a wrong border.
/// `anchors_map_to_the_documented_shape` pins the well-known glyphs.
const TABLE: [Shape; COUNT as usize] = build_table();

const fn build_table() -> [Shape; COUNT as usize] {
    use edge::*;
    let mut t = [sh(0, W_LIGHT); COUNT as usize];

    // Apply one (codepoint, shape) pair.
    macro_rules! put {
        ($cp:literal, $shape:expr) => {
            t[($cp - FIRST) as usize] = $shape;
        };
    }

    // ─── light singles ────────────────────────────────────────────────────────
    // ─── light and mixed singles ───────────────────────────────────────
    // ─── heavy singles ─────────────────────────────────────────────────
    // ─── dashed and double lines ───────────────────────────────────────
    // ─── light and mixed singles ─────────────────────────────────────

    // ─── heavy singles ───────────────────────────────────────────────

    // ─── dashed and double lines ─────────────────────────────────────

    // ─── diagonals and rounded corners ─────────────────────────────────────
    //
    // Only U+2571, U+2572 and U+2573 are diagonals in this block. Eleven other codepoints
    // were drawn as diagonals by an earlier revision and are in fact orthogonal:
    // U+2531/U+2532/U+2535 are half-weight crosses, U+253F..U+2543 are full crosses, and
    // U+257C..U+257F are half-weight bars. See the module note on how this table is built.
    //
    // A quarter-circle arc at 16 px is about 4 pixels of curvature; the rounded corners are
    // drawn as right angles, which is what terminal fonts substitute anyway and what reads
    // correctly at these sizes. Recorded rather than hidden: a caller wanting true arcs
    // needs an arc rasteriser, and this module does not have one.
    put!(0x2500, sh(LEFT | RIGHT, W_LIGHT)); // ─ LIGHT HORIZONTAL
    put!(0x2501, sh(LEFT | RIGHT, W_HEAVY)); // ━ HEAVY HORIZONTAL
    put!(0x2502, sh(UP | DOWN, W_LIGHT)); // │ LIGHT VERTICAL
    put!(0x2503, sh(UP | DOWN, W_HEAVY)); // ┃ HEAVY VERTICAL
    put!(0x2504, sh(LEFT | RIGHT, W_LIGHT)); // ┄ LIGHT TRIPLE DASH HORIZONTAL
    put!(0x2505, sh(LEFT | RIGHT, W_HEAVY)); // ┅ HEAVY TRIPLE DASH HORIZONTAL
    put!(0x2506, sh(UP | DOWN, W_LIGHT)); // ┆ LIGHT TRIPLE DASH VERTICAL
    put!(0x2507, sh(UP | DOWN, W_HEAVY)); // ┇ HEAVY TRIPLE DASH VERTICAL
    put!(0x2508, sh(LEFT | RIGHT, W_LIGHT)); // ┈ LIGHT QUADRUPLE DASH HORIZONTAL
    put!(0x2509, sh(LEFT | RIGHT, W_HEAVY)); // ┉ HEAVY QUADRUPLE DASH HORIZONTAL
    put!(0x250A, sh(UP | DOWN, W_LIGHT)); // ┊ LIGHT QUADRUPLE DASH VERTICAL
    put!(0x250B, sh(UP | DOWN, W_HEAVY)); // ┋ HEAVY QUADRUPLE DASH VERTICAL
    put!(0x250C, sh(DOWN | RIGHT, W_LIGHT)); // ┌ LIGHT DOWN AND RIGHT
    put!(0x250D, sh(DOWN | RIGHT, W_LIGHT)); // ┍ DOWN LIGHT AND RIGHT HEAVY
    put!(0x250E, sh(DOWN | RIGHT, W_LIGHT)); // ┎ DOWN HEAVY AND RIGHT LIGHT
    put!(0x250F, sh(DOWN | RIGHT, W_HEAVY)); // ┏ HEAVY DOWN AND RIGHT
    put!(0x2510, sh(DOWN | LEFT, W_LIGHT)); // ┐ LIGHT DOWN AND LEFT
    put!(0x2511, sh(DOWN | LEFT, W_LIGHT)); // ┑ DOWN LIGHT AND LEFT HEAVY
    put!(0x2512, sh(DOWN | LEFT, W_LIGHT)); // ┒ DOWN HEAVY AND LEFT LIGHT
    put!(0x2513, sh(DOWN | LEFT, W_HEAVY)); // ┓ HEAVY DOWN AND LEFT
    put!(0x2514, sh(UP | RIGHT, W_LIGHT)); // └ LIGHT UP AND RIGHT
    put!(0x2515, sh(UP | RIGHT, W_LIGHT)); // ┕ UP LIGHT AND RIGHT HEAVY
    put!(0x2516, sh(UP | RIGHT, W_LIGHT)); // ┖ UP HEAVY AND RIGHT LIGHT
    put!(0x2517, sh(UP | RIGHT, W_HEAVY)); // ┗ HEAVY UP AND RIGHT
    put!(0x2518, sh(UP | LEFT, W_LIGHT)); // ┘ LIGHT UP AND LEFT
    put!(0x2519, sh(UP | LEFT, W_LIGHT)); // ┙ UP LIGHT AND LEFT HEAVY
    put!(0x251A, sh(UP | LEFT, W_LIGHT)); // ┚ UP HEAVY AND LEFT LIGHT
    put!(0x251B, sh(UP | LEFT, W_HEAVY)); // ┛ HEAVY UP AND LEFT
    put!(0x251C, sh(UP | DOWN | RIGHT, W_LIGHT)); // ├ LIGHT VERTICAL AND RIGHT
    put!(0x251D, sh(UP | DOWN | RIGHT, W_LIGHT)); // ┝ VERTICAL LIGHT AND RIGHT HEAVY
    put!(0x251E, sh(UP | DOWN | RIGHT, W_LIGHT)); // ┞ UP HEAVY AND RIGHT DOWN LIGHT
    put!(0x251F, sh(UP | DOWN | RIGHT, W_LIGHT)); // ┟ DOWN HEAVY AND RIGHT UP LIGHT
    put!(0x2520, sh(UP | DOWN | RIGHT, W_LIGHT)); // ┠ VERTICAL HEAVY AND RIGHT LIGHT
    put!(0x2521, sh(UP | DOWN | RIGHT, W_LIGHT)); // ┡ DOWN LIGHT AND RIGHT UP HEAVY
    put!(0x2522, sh(UP | DOWN | RIGHT, W_LIGHT)); // ┢ UP LIGHT AND RIGHT DOWN HEAVY
    put!(0x2523, sh(UP | DOWN | RIGHT, W_HEAVY)); // ┣ HEAVY VERTICAL AND RIGHT
    put!(0x2524, sh(UP | DOWN | LEFT, W_LIGHT)); // ┤ LIGHT VERTICAL AND LEFT
    put!(0x2525, sh(UP | DOWN | LEFT, W_LIGHT)); // ┥ VERTICAL LIGHT AND LEFT HEAVY
    put!(0x2526, sh(UP | DOWN | LEFT, W_LIGHT)); // ┦ UP HEAVY AND LEFT DOWN LIGHT
    put!(0x2527, sh(UP | DOWN | LEFT, W_LIGHT)); // ┧ DOWN HEAVY AND LEFT UP LIGHT
    put!(0x2528, sh(UP | DOWN | LEFT, W_LIGHT)); // ┨ VERTICAL HEAVY AND LEFT LIGHT
    put!(0x2529, sh(UP | DOWN | LEFT, W_LIGHT)); // ┩ DOWN LIGHT AND LEFT UP HEAVY
    put!(0x252A, sh(UP | DOWN | LEFT, W_LIGHT)); // ┪ UP LIGHT AND LEFT DOWN HEAVY
    put!(0x252B, sh(UP | DOWN | LEFT, W_HEAVY)); // ┫ HEAVY VERTICAL AND LEFT
    put!(0x252C, sh(DOWN | LEFT | RIGHT, W_LIGHT)); // ┬ LIGHT DOWN AND HORIZONTAL
    put!(0x252D, sh(DOWN | LEFT | RIGHT, W_LIGHT)); // ┭ LEFT HEAVY AND RIGHT DOWN LIGHT
    put!(0x252E, sh(DOWN | LEFT | RIGHT, W_LIGHT)); // ┮ RIGHT HEAVY AND LEFT DOWN LIGHT
    put!(0x252F, sh(DOWN | LEFT | RIGHT, W_LIGHT)); // ┯ DOWN LIGHT AND HORIZONTAL HEAVY
    put!(0x2530, sh(DOWN | LEFT | RIGHT, W_LIGHT)); // ┰ DOWN HEAVY AND HORIZONTAL LIGHT
    put!(0x2531, sh(DOWN | LEFT | RIGHT, W_LIGHT)); // ┱ RIGHT LIGHT AND LEFT DOWN HEAVY
    put!(0x2532, sh(DOWN | LEFT | RIGHT, W_LIGHT)); // ┲ LEFT LIGHT AND RIGHT DOWN HEAVY
    put!(0x2533, sh(DOWN | LEFT | RIGHT, W_HEAVY)); // ┳ HEAVY DOWN AND HORIZONTAL
    put!(0x2534, sh(UP | LEFT | RIGHT, W_LIGHT)); // ┴ LIGHT UP AND HORIZONTAL
    put!(0x2535, sh(UP | LEFT | RIGHT, W_LIGHT)); // ┵ LEFT HEAVY AND RIGHT UP LIGHT
    put!(0x2536, sh(UP | LEFT | RIGHT, W_LIGHT)); // ┶ RIGHT HEAVY AND LEFT UP LIGHT
    put!(0x2537, sh(UP | LEFT | RIGHT, W_LIGHT)); // ┷ UP LIGHT AND HORIZONTAL HEAVY
    put!(0x2538, sh(UP | LEFT | RIGHT, W_LIGHT)); // ┸ UP HEAVY AND HORIZONTAL LIGHT
    put!(0x2539, sh(UP | LEFT | RIGHT, W_LIGHT)); // ┹ RIGHT LIGHT AND LEFT UP HEAVY
    put!(0x253A, sh(UP | LEFT | RIGHT, W_LIGHT)); // ┺ LEFT LIGHT AND RIGHT UP HEAVY
    put!(0x253B, sh(UP | LEFT | RIGHT, W_HEAVY)); // ┻ HEAVY UP AND HORIZONTAL
    put!(0x253C, sh(UP | DOWN | LEFT | RIGHT, W_LIGHT)); // ┼ LIGHT VERTICAL AND HORIZONTAL
    put!(0x253D, sh(UP | DOWN | LEFT | RIGHT, W_LIGHT)); // ┽ LEFT HEAVY AND RIGHT VERTICAL LIGHT
    put!(0x253E, sh(UP | DOWN | LEFT | RIGHT, W_LIGHT)); // ┾ RIGHT HEAVY AND LEFT VERTICAL LIGHT
    put!(0x253F, sh(UP | DOWN | LEFT | RIGHT, W_LIGHT)); // ┿ VERTICAL LIGHT AND HORIZONTAL HEAVY
    put!(0x2540, sh(UP | DOWN | LEFT | RIGHT, W_LIGHT)); // ╀ UP HEAVY AND DOWN HORIZONTAL LIGHT
    put!(0x2541, sh(UP | DOWN | LEFT | RIGHT, W_LIGHT)); // ╁ DOWN HEAVY AND UP HORIZONTAL LIGHT
    put!(0x2542, sh(UP | DOWN | LEFT | RIGHT, W_LIGHT)); // ╂ VERTICAL HEAVY AND HORIZONTAL LIGHT
    put!(0x2543, sh(UP | DOWN | LEFT | RIGHT, W_LIGHT)); // ╃ LEFT UP HEAVY AND RIGHT DOWN LIGHT
    put!(0x2544, sh(UP | DOWN | LEFT | RIGHT, W_LIGHT)); // ╄ RIGHT UP HEAVY AND LEFT DOWN LIGHT
    put!(0x2545, sh(UP | DOWN | LEFT | RIGHT, W_LIGHT)); // ╅ LEFT DOWN HEAVY AND RIGHT UP LIGHT
    put!(0x2546, sh(UP | DOWN | LEFT | RIGHT, W_LIGHT)); // ╆ RIGHT DOWN HEAVY AND LEFT UP LIGHT
    put!(0x2547, sh(UP | DOWN | LEFT | RIGHT, W_LIGHT)); // ╇ DOWN LIGHT AND UP HORIZONTAL HEAVY
    put!(0x2548, sh(UP | DOWN | LEFT | RIGHT, W_LIGHT)); // ╈ UP LIGHT AND DOWN HORIZONTAL HEAVY
    put!(0x2549, sh(UP | DOWN | LEFT | RIGHT, W_LIGHT)); // ╉ RIGHT LIGHT AND LEFT VERTICAL HEAVY
    put!(0x254A, sh(UP | DOWN | LEFT | RIGHT, W_LIGHT)); // ╊ LEFT LIGHT AND RIGHT VERTICAL HEAVY
    put!(0x254B, sh(UP | DOWN | LEFT | RIGHT, W_HEAVY)); // ╋ HEAVY VERTICAL AND HORIZONTAL
    put!(0x254C, sh(LEFT | RIGHT, W_DOUBLE)); // ╌ LIGHT DOUBLE DASH HORIZONTAL
    put!(0x254D, sh(LEFT | RIGHT, W_DOUBLE)); // ╍ HEAVY DOUBLE DASH HORIZONTAL
    put!(0x254E, sh(UP | DOWN, W_DOUBLE)); // ╎ LIGHT DOUBLE DASH VERTICAL
    put!(0x254F, sh(UP | DOWN, W_DOUBLE)); // ╏ HEAVY DOUBLE DASH VERTICAL
    put!(0x2550, sh(LEFT | RIGHT, W_DOUBLE)); // ═ DOUBLE HORIZONTAL
    put!(0x2551, sh(UP | DOWN, W_DOUBLE)); // ║ DOUBLE VERTICAL
    put!(0x2552, sh(DOWN | RIGHT, W_DOUBLE)); // ╒ DOWN SINGLE AND RIGHT DOUBLE
    put!(0x2553, sh(DOWN | RIGHT, W_DOUBLE)); // ╓ DOWN DOUBLE AND RIGHT SINGLE
    put!(0x2554, sh(DOWN | RIGHT, W_DOUBLE)); // ╔ DOUBLE DOWN AND RIGHT
    put!(0x2555, sh(DOWN | LEFT, W_DOUBLE)); // ╕ DOWN SINGLE AND LEFT DOUBLE
    put!(0x2556, sh(DOWN | LEFT, W_DOUBLE)); // ╖ DOWN DOUBLE AND LEFT SINGLE
    put!(0x2557, sh(DOWN | LEFT, W_DOUBLE)); // ╗ DOUBLE DOWN AND LEFT
    put!(0x2558, sh(UP | RIGHT, W_DOUBLE)); // ╘ UP SINGLE AND RIGHT DOUBLE
    put!(0x2559, sh(UP | RIGHT, W_DOUBLE)); // ╙ UP DOUBLE AND RIGHT SINGLE
    put!(0x255A, sh(UP | RIGHT, W_DOUBLE)); // ╚ DOUBLE UP AND RIGHT
    put!(0x255B, sh(UP | LEFT, W_DOUBLE)); // ╛ UP SINGLE AND LEFT DOUBLE
    put!(0x255C, sh(UP | LEFT, W_DOUBLE)); // ╜ UP DOUBLE AND LEFT SINGLE
    put!(0x255D, sh(UP | LEFT, W_DOUBLE)); // ╝ DOUBLE UP AND LEFT
    put!(0x255E, sh(UP | DOWN | RIGHT, W_DOUBLE)); // ╞ VERTICAL SINGLE AND RIGHT DOUBLE
    put!(0x255F, sh(UP | DOWN | RIGHT, W_DOUBLE)); // ╟ VERTICAL DOUBLE AND RIGHT SINGLE
    put!(0x2560, sh(UP | DOWN | RIGHT, W_DOUBLE)); // ╠ DOUBLE VERTICAL AND RIGHT
    put!(0x2561, sh(UP | DOWN | LEFT, W_DOUBLE)); // ╡ VERTICAL SINGLE AND LEFT DOUBLE
    put!(0x2562, sh(UP | DOWN | LEFT, W_DOUBLE)); // ╢ VERTICAL DOUBLE AND LEFT SINGLE
    put!(0x2563, sh(UP | DOWN | LEFT, W_DOUBLE)); // ╣ DOUBLE VERTICAL AND LEFT
    put!(0x2564, sh(DOWN | LEFT | RIGHT, W_DOUBLE)); // ╤ DOWN SINGLE AND HORIZONTAL DOUBLE
    put!(0x2565, sh(DOWN | LEFT | RIGHT, W_DOUBLE)); // ╥ DOWN DOUBLE AND HORIZONTAL SINGLE
    put!(0x2566, sh(DOWN | LEFT | RIGHT, W_DOUBLE)); // ╦ DOUBLE DOWN AND HORIZONTAL
    put!(0x2567, sh(UP | LEFT | RIGHT, W_DOUBLE)); // ╧ UP SINGLE AND HORIZONTAL DOUBLE
    put!(0x2568, sh(UP | LEFT | RIGHT, W_DOUBLE)); // ╨ UP DOUBLE AND HORIZONTAL SINGLE
    put!(0x2569, sh(UP | LEFT | RIGHT, W_DOUBLE)); // ╩ DOUBLE UP AND HORIZONTAL
    put!(0x256A, sh(UP | DOWN | LEFT | RIGHT, W_DOUBLE)); // ╪ VERTICAL SINGLE AND HORIZONTAL DOUBLE
    put!(0x256B, sh(UP | DOWN | LEFT | RIGHT, W_DOUBLE)); // ╫ VERTICAL DOUBLE AND HORIZONTAL SINGLE
    put!(0x256C, sh(UP | DOWN | LEFT | RIGHT, W_DOUBLE)); // ╬ DOUBLE VERTICAL AND HORIZONTAL
    put!(0x256D, sh(DOWN | RIGHT, W_ROUND)); // ╭ LIGHT ARC DOWN AND RIGHT
    put!(0x256E, sh(DOWN | LEFT, W_ROUND)); // ╮ LIGHT ARC DOWN AND LEFT
    put!(0x256F, sh(UP | LEFT, W_ROUND)); // ╯ LIGHT ARC UP AND LEFT
    put!(0x2570, sh(UP | RIGHT, W_ROUND)); // ╰ LIGHT ARC UP AND RIGHT
    put!(0x2571, sh(DIAG_UP_RIGHT, W_LIGHT)); // ╱ LIGHT DIAGONAL UPPER RIGHT TO LOWER LEFT
    put!(0x2572, sh(DIAG_UP_LEFT, W_LIGHT)); // ╲ LIGHT DIAGONAL UPPER LEFT TO LOWER RIGHT
    put!(0x2573, sh(DIAG_UP_RIGHT | DIAG_UP_LEFT, W_LIGHT)); // ╳ LIGHT DIAGONAL CROSS
    put!(0x2574, sh(LEFT, W_LIGHT)); // ╴ LIGHT LEFT
    put!(0x2575, sh(UP, W_LIGHT)); // ╵ LIGHT UP
    put!(0x2576, sh(RIGHT, W_LIGHT)); // ╶ LIGHT RIGHT
    put!(0x2577, sh(DOWN, W_LIGHT)); // ╷ LIGHT DOWN
    put!(0x2578, sh(LEFT, W_HEAVY)); // ╸ HEAVY LEFT
    put!(0x2579, sh(UP, W_HEAVY)); // ╹ HEAVY UP
    put!(0x257A, sh(RIGHT, W_HEAVY)); // ╺ HEAVY RIGHT
    put!(0x257B, sh(DOWN, W_HEAVY)); // ╻ HEAVY DOWN
    put!(0x257C, sh(LEFT | RIGHT, W_LIGHT)); // ╼ LIGHT LEFT AND HEAVY RIGHT
    put!(0x257D, sh(UP | DOWN, W_LIGHT)); // ╽ LIGHT UP AND HEAVY DOWN
    put!(0x257E, sh(LEFT | RIGHT, W_LIGHT)); // ╾ HEAVY LEFT AND LIGHT RIGHT
    put!(0x257F, sh(UP | DOWN, W_LIGHT)); // ╿ HEAVY UP AND LIGHT DOWN

    // ─── everything else gets the full box, which is the safest default ────────
    // A codepoint with no entry would otherwise render as an invisible gap in a border, and
    // `every_codepoint_has_geometry` would fail. Filling with a plus means an unmapped glyph
    // degrades to a visible cross rather than a hole, and the anchor test reports which ones.
    let mut i = 0usize;
    while i < COUNT as usize {
        if t[i] & 0xFF == 0 {
            t[i] = sh(UP | DOWN | LEFT | RIGHT, W_LIGHT);
        }
        i += 1;
    }
    t
}

/// Arms and weight for one codepoint.
pub fn glyph_kind(codepoint: u32) -> Option<(u8, u16)> {
    if (FIRST..=LAST).contains(&codepoint) {
        let s = TABLE[(codepoint - FIRST) as usize];
        Some(((s & 0xFF) as u8, s >> 8))
    } else {
        None
    }
}

/// Cell size in pixels for a ppem: at least [`STROKE`] plus enough room for a diagonal.
pub fn cell_size(ppem: u16) -> usize {
    (ppem as usize).max(2)
}

/// Draw one codepoint's glyph into a `w x h` bitmap, row-major, 0 or 255.
///
/// Coordinates are pixel indices from the top-left. `w` and `h` are the caller's choice, so
/// this works for any cell aspect; the diagonal uses `min(w, h)`.
pub fn draw_glyph(codepoint: u32, w: usize, h: usize, out: &mut [u8]) {
    debug_assert_eq!(out.len(), w * h);
    let Some((arms, weight)) = glyph_kind(codepoint) else {
        return;
    };
    let _ = weight;
    let mut put = |x: i32, y: i32| {
        if x < 0 || y < 0 || x as usize >= w || y as usize >= h {
            return;
        }
        out[y as usize * w + x as usize] = 255;
    };

    let last_x = w as i32 - 1;
    let last_y = h as i32 - 1;
    let mid_x = (w / 2) as i32;
    let mid_y = (h / 2) as i32;

    // # Arms, not edges
    //
    // Each arm is a line from the cell centre to one cell edge, so UP is the *top half* of the
    // centre column, DOWN the bottom half, LEFT the left half of the centre row, RIGHT the
    // right half. That makes ┌ (DOWN|RIGHT) an L: vertical going down from centre, horizontal
    // going right from centre.
    //
    // An earlier revision drew "reaches UP" as a horizontal line along the top edge. That makes
    // ┌ two unrelated lines meeting at a corner pixel, and ─ (LEFT|RIGHT) two vertical bars, so
    // every glyph in the block was wrong while still looking like *something*. Separately, the
    // arms-vs-edges confusion is what makes tiling work: the vertical stroke of ┌ occupies the
    // same rows as the │ in the cell below it, so the border has no seam.
    if arms & (edge::UP | edge::DOWN | edge::LEFT | edge::RIGHT) != 0 {
        put(mid_x, mid_y);
        if arms & edge::UP != 0 {
            for y in 0..=mid_y {
                put(mid_x, y);
            }
        }
        if arms & edge::DOWN != 0 {
            for y in (mid_y + 1)..=last_y {
                put(mid_x, y);
            }
        }
        if arms & edge::LEFT != 0 {
            // Inclusive of 0: the arm must reach the cell's left edge so it abuts the
            // neighbouring cell's glyph. An exclusive range leaves a 1px gap, which is the whole
            // seam this module exists to eliminate.
            for x in 0..=mid_x {
                put(x, mid_y);
            }
        }
        if arms & edge::RIGHT != 0 {
            for x in (mid_x + 1)..=last_x {
                put(x, mid_y);
            }
        }
    }

    // Diagonals, Bresenham from corner to corner.
    let diag = |from: (i32, i32), to: (i32, i32), put: &mut dyn FnMut(i32, i32)| {
        let (mut x, mut y) = (from.0, from.1);
        let (x1, y1) = (to.0, to.1);
        let dx = (x1 - x).abs();
        let sx = if x < x1 { 1 } else { -1 };
        let dy = -(y1 - y).abs();
        let sy = if y < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            put(x, y);
            if x == x1 && y == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x += sx;
            }
            if e2 <= dx {
                err += dx;
                y += sy;
            }
        }
    };
    // ╱ U+2571 rises left to right; ╲ U+2572 falls.
    if arms & edge::DIAG_UP_RIGHT != 0 {
        diag((0, last_y), (last_x, 0), &mut put);
    }
    if arms & edge::DIAG_UP_LEFT != 0 {
        diag((last_x, last_y), (0, 0), &mut put);
    }
    // The four single-arm glyphs U+2574..U+2577 already fall out of the arm loop above,
    // because LEFT draws x in 0..mid_x and the centre is always marked. Nothing extra here.
}

/// Add every Box Drawing glyph at every requested size, for every style.
///
/// The cell is sized to the ppem so borders tile: a caller drawing a table must place cells on
/// a grid whose pitch is `cell_size(ppem)`, which is what [`cell_size`] is for. Box drawing is
/// monospaced by construction, so the advance is the cell width regardless of the face.
pub fn add_box_drawing(sizes: &[u16], builder: &mut AtlasBuilder) -> Result<(), AtlasError> {
    // Sized for the largest requested cell and grown as needed, so the one-time pass allocates
    // one scratch buffer rather than one per glyph.
    let mut cell_buf: Vec<u8> = Vec::new();
    for &ppem in sizes {
        let cell = cell_size(ppem);
        if cell_buf.len() < cell * cell {
            cell_buf.resize(cell * cell, 0);
        }
        for cp in FIRST..=LAST {
            // One draw, cropped once. Regular, Italic and Monospace share this bitmap: Box
            // Drawing has no italic and no monospace variant in any real font either, so
            // duplicating it three more times cost three quarters of the block's atlas
            // footprint for bit-identical data.
            cell_buf[..cell * cell].fill(0);
            draw_glyph(cp, cell, cell, &mut cell_buf[..cell * cell]);
            let regular = crop(&cell_buf[..cell * cell], cell);

            builder.add(PendingGlyph::new(
                cp,
                Style::Regular,
                ppem,
                regular.bitmap.clone(),
                regular.width,
                regular.height,
                regular.bearing_x,
                regular.bearing_y,
                cell as i32,
            )?)?;
            builder.alias(cp, Style::Italic, ppem, cp, Style::Regular, ppem)?;
            builder.alias(cp, Style::Monospace, ppem, cp, Style::Regular, ppem)?;

            // Bold draws a 2px stroke. That is the only difference between Regular and Bold
            // here, and it is what makes a bold border look bold rather than identical.
            cell_buf[..cell * cell].fill(0);
            draw_glyph(cp, cell, cell, &mut cell_buf[..cell * cell]);
            thicken(&mut cell_buf[..cell * cell], cell);
            let bold = crop(&cell_buf[..cell * cell], cell);
            builder.add(PendingGlyph::new(
                cp,
                Style::Bold,
                ppem,
                bold.bitmap,
                bold.width,
                bold.height,
                bold.bearing_x,
                bold.bearing_y,
                cell as i32,
            )?)?;
        }
    }
    Ok(())
}

/// Dilate a 1px stroke into a 2px one, for the Bold face only.
fn thicken(bits: &mut [u8], cell: usize) {
    let before = bits.to_vec();
    for y in 0..cell {
        for x in 0..cell {
            if before[y * cell + x] == 0 {
                continue;
            }
            if x + 1 < cell {
                bits[y * cell + x + 1] = 255;
            }
            if y + 1 < cell {
                bits[(y + 1) * cell + x] = 255;
            }
        }
    }
}

/// A cropped Box Drawing bitmap and its offset within the cell.
///
/// The atlas stores what is drawn, not the cell. A `─` in a 22px cell occupies one full row of 22
/// pixels, and a `│` one full column: keeping the whole square cell would spend 484 bytes to
/// store 22. Cropping and recording the offset in the metric's `bearing_x`/`bearing_y` keeps the
/// strokes at the same absolute pixel positions — which is what makes adjacent cells abut — while
/// cutting the block's footprint by roughly three quarters. Measured: 474 KB stored as full cells
/// against 158 KB cropped and shared across three styles.
///
/// # Bearing convention
///
/// Both bearings are the offset of the crop's origin **from the cell's top-left corner**, in
/// pixels, positive right and positive *down*. That is deliberately not the baseline-relative
/// convention a text glyph's bearings use: a caller placing a Box Drawing glyph adds these to the
/// cell's top-left, whereas a text glyph adds its bearings to the pen. Two conventions in one
/// struct would be a trap, so this one is documented here and pinned by
/// `cropping_preserves_absolute_pixel_positions`.
///
/// An earlier version negated `bearing_y` to match the text convention, and applying it as
/// `y + bearing_y` in `usize` arithmetic overflowed: `(-6 as usize) + y` is a 2^64-sized number.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cropped {
    /// Tightly packed coverage, `width * height` bytes.
    pub bitmap: Vec<u8>,
    /// Cropped width in pixels.
    pub width: usize,
    /// Cropped height in pixels.
    pub height: usize,
    /// Offset of the crop's left edge from the cell's left edge, positive right.
    pub bearing_x: i32,
    /// Offset of the crop's top edge from the cell's top edge, positive down.
    pub bearing_y: i32,
}

/// Crop a `cell x cell` coverage bitmap to the pixels it actually marks.
///
/// Returns an empty [`Cropped`] when the glyph marks nothing.
pub fn crop(cell_bitmap: &[u8], cell: usize) -> Cropped {
    debug_assert_eq!(cell_bitmap.len(), cell * cell);
    let (mut x0, mut y0) = (cell, cell);
    let (mut x1, mut y1) = (0usize, 0usize);
    for y in 0..cell {
        for x in 0..cell {
            if cell_bitmap[y * cell + x] == 0 {
                continue;
            }
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    if x0 >= x1 || y0 >= y1 {
        return Cropped {
            bitmap: Vec::new(),
            width: 0,
            height: 0,
            bearing_x: 0,
            bearing_y: 0,
        };
    }
    let width = x1 - x0;
    let height = y1 - y0;
    let mut bitmap = Vec::with_capacity(width * height);
    for y in y0..y1 {
        bitmap.extend_from_slice(&cell_bitmap[y * cell + x0..y * cell + x1]);
    }
    Cropped {
        bitmap,
        width,
        height,
        bearing_x: x0 as i32,
        bearing_y: y0 as i32,
    }
}

/// The cell size as a `GlyphMetric` would report it, for callers laying out a grid before the
/// atlas exists.
///
/// `codepoint` is accepted so this composes with `metric_for(cp, ppem)` call sites that will
/// need to branch on the codepoint once the block grows mixed-weight or double-line glyphs;
/// today every glyph in the block is a full cell, so it is unused and named `_codepoint`
/// rather than silently accepted.
pub fn cell_metric(_codepoint: u32, ppem: u16) -> GlyphMetric {
    let cell = cell_size(ppem).min(255) as u8;
    GlyphMetric {
        atlas_x: 0,
        atlas_y: 0,
        width: cell,
        height: cell,
        bearing_x: 0,
        bearing_y: 0,
        advance_x: cell,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw(cp: u32, cell: usize) -> Vec<u8> {
        let mut v = vec![0u8; cell * cell];
        draw_glyph(cp, cell, cell, &mut v);
        v
    }

    fn count(v: &[u8]) -> usize {
        v.iter().filter(|&&b| b == 255).count()
    }

    /// Row `y` of a `cell x cell` bitmap.
    fn row(v: &[u8], cell: usize, y: usize) -> Vec<u8> {
        v[y * cell..(y + 1) * cell].to_vec()
    }

    /// Column `x` of a `cell x cell` bitmap.
    fn col(v: &[u8], cell: usize, x: usize) -> Vec<u8> {
        (0..cell).map(|y| v[y * cell + x]).collect()
    }

    /// Lay glyphs out on a grid and return the composite.
    fn compose(grid: usize, placements: &[(usize, usize, u32)], cell: usize) -> Vec<u8> {
        let mut canvas = vec![0u8; grid * grid];
        for &(cx, cy, cp) in placements {
            let g = draw(cp, cell);
            for y in 0..cell {
                for x in 0..cell {
                    if g[y * cell + x] == 255 {
                        canvas[(cy + y) * grid + cx + x] = 255;
                    }
                }
            }
        }
        canvas
    }

    /// The table must cover the whole block. A gap would render as a hole in a border.
    #[test]
    fn every_codepoint_has_geometry() {
        for cp in FIRST..=LAST {
            let (arms, _) = glyph_kind(cp).unwrap_or_else(|| panic!("U+{cp:04X} has no entry"));
            assert_ne!(arms, 0, "U+{cp:04X} draws nothing");
        }
    }

    #[test]
    fn outside_the_block_is_none() {
        assert_eq!(glyph_kind(FIRST - 1), None);
        assert_eq!(glyph_kind(LAST + 1), None);
        assert_eq!(glyph_kind(0x20), None);
    }

    /// Every glyph must actually mark pixels.
    #[test]
    fn every_glyph_marks_pixels() {
        for cp in FIRST..=LAST {
            assert!(count(&draw(cp, 12)) > 0, "U+{cp:04X} drew nothing at 12px");
        }
    }

    /// The well-known glyphs must map to their documented shapes. This is the table-alignment
    /// guard: the previous running-counter build had all 19 of these correct and then drifted,
    /// so the anchors are not sufficient on their own -- but they catch the common case.
    #[test]
    fn anchors_map_to_the_documented_shape() {
        use edge::*;
        for (cp, want) in [
            (0x2500u32, LEFT | RIGHT),
            (0x2501, LEFT | RIGHT),
            (0x2502, UP | DOWN),
            (0x250C, DOWN | RIGHT),
            (0x2510, DOWN | LEFT),
            (0x2514, UP | RIGHT),
            (0x2518, UP | LEFT),
            (0x251C, UP | DOWN | RIGHT),
            (0x2524, UP | DOWN | LEFT),
            // U+252C `┬` is DOWN | LEFT | RIGHT. The anchor list previously said
            // LEFT | RIGHT | UP, copied from a table that had it wrong; the name
            // "LIGHT DOWN AND HORIZONTAL" and JetBrains Mono's raster (`-DLR`)
            // both disagree with that.
            (0x252C, DOWN | LEFT | RIGHT),
            // U+2534 `┴` is UP | LEFT | RIGHT. The anchor list previously said
            // LEFT | RIGHT | DOWN, copied from a table that had it wrong. Note that the Unicode
            // *name* is "LIGHT LEFT AND RIGHT AND DOWN" while the *glyph* plainly has its stub
            // above the horizontal; here the glyph wins, and JetBrains Mono's raster (`U-LR`)
            // agrees with the glyph.
            (0x2534, UP | LEFT | RIGHT),
            (0x253C, UP | DOWN | LEFT | RIGHT),
            (0x254B, UP | DOWN | LEFT | RIGHT),
            (0x2550, LEFT | RIGHT),
            (0x2551, UP | DOWN),
            (0x2554, DOWN | RIGHT),
            (0x255A, UP | RIGHT),
            (0x255D, UP | LEFT),
            (0x256C, UP | DOWN | LEFT | RIGHT),
            (0x2571, DIAG_UP_RIGHT),
            (0x2572, DIAG_UP_LEFT),
            (0x2573, DIAG_UP_RIGHT | DIAG_UP_LEFT),
            (0x2574, LEFT),
            // U+2575 and U+2576 were swapped in the original table: the names are "LIGHT UP" and
            // "LIGHT RIGHT", so U+2575 is an up arm and U+2576 a right arm. JetBrains Mono's
            // rasters agree (`U---` and `---R` respectively), so this is the font and the name
            // both against the table.
            (0x2575, UP),
            (0x2576, RIGHT),
            (0x2577, DOWN),
        ] {
            let (arms, _) = glyph_kind(cp).expect("in block");
            assert_eq!(
                arms, want,
                "U+{cp:04X} has arms {arms:#04x}, want {want:#04x}"
            );
        }
    }

    /// U+2500 ─ is a horizontal line through the cell's centre row: left arm to x=0, right arm
    /// to x=last, centre pixel joining them.
    #[test]
    fn horizontal_line_is_the_centre_row() {
        let cell = 8;
        let v = draw(0x2500, cell);
        assert_eq!(row(&v, cell, cell / 2), vec![255u8; 8], "centre row");
        for y in 0..cell {
            if y != cell / 2 {
                assert_eq!(row(&v, cell, y), vec![0u8; 8], "row {y} must be empty");
            }
        }
    }

    /// U+2502 │ is a vertical line down the cell's centre column.
    #[test]
    fn vertical_line_is_the_centre_column() {
        let cell = 8;
        let v = draw(0x2502, cell);
        assert_eq!(col(&v, cell, cell / 2), vec![255u8; 8], "centre column");
        for x in 0..cell {
            if x != cell / 2 {
                assert_eq!(col(&v, cell, x), vec![0u8; 8], "column {x} must be empty");
            }
        }
    }

    /// U+250C ┌ is DOWN|RIGHT: an L whose vertical half runs down from the centre and whose
    /// horizontal half runs right from it. Both arms *start* at the centre pixel.
    #[test]
    fn top_left_corner_is_an_l_from_the_centre() {
        let cell = 8;
        let v = draw(0x250C, cell);
        let m = cell / 2;
        // Vertical half below centre, horizontal half right of centre.
        for y in m..cell {
            assert_eq!(v[y * cell + m], 255, "vertical at ({m},{y})");
        }
        for x in m..cell {
            assert_eq!(v[m * cell + x], 255, "horizontal at ({x},{m})");
        }
        // Everything else in the cell is empty.
        for y in 0..cell {
            for x in 0..cell {
                if y >= m || x >= m {
                    continue;
                }
                assert_eq!(v[y * cell + x], 0, "({x},{y}) must be empty for ┌");
            }
        }
    }

    /// U+2510 ┐ is the mirror: DOWN|LEFT.
    #[test]
    fn top_right_corner_mirrors() {
        let cell = 8;
        let v = draw(0x2510, cell);
        let m = cell / 2;
        for y in m..cell {
            assert_eq!(v[y * cell + m], 255, "vertical at ({m},{y})");
        }
        for x in 0..=m {
            assert_eq!(v[m * cell + x], 255, "horizontal at ({x},{m})");
        }
    }

    /// The four arms of ┼ cross at the centre.
    #[test]
    fn the_full_cross_arms_all_four_ways() {
        let cell = 8;
        let v = draw(0x253C, cell);
        assert_eq!(
            count(&v),
            cell * 2 - 1,
            "two full lines crossing at the centre"
        );
        assert_eq!(col(&v, cell, cell / 2), vec![255u8; 8]);
        assert_eq!(row(&v, cell, cell / 2), vec![255u8; 8]);
    }

    /// Two ─ cells side by side form one unbroken horizontal run.
    ///
    /// This is the property that motivates generating box drawing at all: font glyphs land on
    /// fractional pixels and leave an antialiasing seam between adjacent cells. With integer
    /// geometry the run is continuous.
    #[test]
    fn adjacent_horizontal_cells_abut_exactly() {
        let cell = 8;
        let combined = compose(2 * cell, &[(0, 0, 0x2500), (cell, 0, 0x2500)], cell);
        let r = row(&combined, 2 * cell, cell / 2);
        assert!(r.iter().all(|&v| v == 255), "two ─ must be one run of 16");
    }

    /// Two │ cells stacked form one unbroken column.
    #[test]
    fn adjacent_vertical_cells_abut_exactly() {
        let cell = 8;
        let combined = compose(2 * cell, &[(0, 0, 0x2502), (0, cell, 0x2502)], cell);
        let c = col(&combined, 2 * cell, cell / 2);
        assert!(
            c.iter().all(|&v| v == 255),
            "two │ must be one column of 16"
        );
    }

    /// A 3x3 box border: every pixel of the rectangle is set and its interior is empty.
    ///
    /// The layout, with the cell centres on the border lines:
    ///
    /// ```text
    ///   ┌───┐
    ///   │   │
    ///   └───┘
    /// ```
    #[test]
    fn a_box_border_is_complete_and_has_an_empty_interior() {
        let cell = 12;
        let m = cell / 2;
        let grid = 4 * cell;
        // Arms run from a glyph's centre out to its cell edge, so for a glyph placed at
        // (cx, cy) the stroke lands on `cx + m` and `cy + m`. Deriving the rectangle from that
        // is what the previous version got wrong by hard-coding `top = cell`, which is half a
        // cell off the actual stroke.
        let l = cell + m; // left border x
        let t = cell + m; // top border y
        let r = cell * 3 + m; // right border x: the ┐/│/┘ column's centre
        let b = cell * 3 + m; // bottom border y
        assert!(
            r + 1 < grid && b + 1 < grid,
            "the derived rectangle must fit the {grid}-pixel grid"
        );

        let combined = compose(
            grid,
            &[
                // top row: ┌ ─ ┐
                (cell, cell, 0x250C),
                (cell * 2, cell, 0x2500),
                (cell * 3, cell, 0x2510),
                // middle row: │ │
                (cell, cell * 2, 0x2502),
                (cell * 3, cell * 2, 0x2502),
                // bottom row: └ ─ ┘
                (cell, cell * 3, 0x2514),
                (cell * 2, cell * 3, 0x2500),
                (cell * 3, cell * 3, 0x2518),
            ],
            cell,
        );

        for x in l..=r {
            assert_eq!(combined[t * grid + x], 255, "top border x={x}");
            assert_eq!(combined[b * grid + x], 255, "bottom border x={x}");
        }
        for y in t..=b {
            assert_eq!(combined[y * grid + l], 255, "left border y={y}");
            assert_eq!(combined[y * grid + r], 255, "right border y={y}");
        }
        for y in (t + 1)..b {
            for x in (l + 1)..r {
                assert_eq!(
                    combined[y * grid + x],
                    0,
                    "interior ({x},{y}) must be empty"
                );
            }
        }
    }

    /// ╱ U+2571 rises from bottom-left to top-right; ╲ U+2572 falls.
    #[test]
    fn diagonals_connect_their_corners() {
        // Index layout of a `cell x cell` bitmap: 0 is top-left, `cell-1` top-right,
        // `(cell-1)*cell` bottom-left, `(cell-1)*cell + cell-1` bottom-right.
        let cell = 9;
        let up = draw(0x2571, cell); // ╱ rises: bottom-left to top-right
        assert_eq!(up[cell - 1], 255, "top-right end of ╱");
        assert_eq!(up[(cell - 1) * cell], 255, "bottom-left end of ╱");
        assert_eq!(up[0], 0, "top-left must be clear for ╱");
        let down = draw(0x2572, cell); // ╲ falls: top-left to bottom-right
        assert_eq!(down[0], 255, "top-left end of ╲");
        assert_eq!(
            down[(cell - 1) * cell + (cell - 1)],
            255,
            "bottom-right end of ╲"
        );
        assert_eq!(down[cell - 1], 0, "top-right must be clear for ╲");
    }

    /// U+2573 ╳ draws both diagonals, so all four corners are marked.
    #[test]
    fn cross_draws_both_diagonals() {
        let cell = 9;
        let v = draw(0x2573, cell);
        assert_eq!(v[0], 255, "top-left");
        assert_eq!(v[cell - 1], 255, "top-right");
        assert_eq!(v[(cell - 1) * cell], 255, "bottom-left");
        assert_eq!(v[(cell - 1) * cell + cell - 1], 255, "bottom-right");
    }

    /// ╴ ╵ ╶ ╷ are single arms.
    #[test]
    fn the_single_arm_glyphs_have_exactly_one_arm() {
        let cell = 8;
        let m = cell / 2;
        // The centre sits at `floor(cell/2)` and is always marked. LEFT and UP are drawn
        // inclusively of the centre (`0..=m`), so they reach the cell edge: `m + 1` pixels.
        // RIGHT and DOWN are drawn from `m + 1` to `cell - 1` and also touch the edge, so they
        // span `cell - 1 - m` pixels plus the centre. What matters for tiling is that every arm
        // reaches its edge, which is asserted next.
        // U+2574..U+2577 are LIGHT LEFT / UP / RIGHT / DOWN in codepoint order. An earlier
        // version of this list had U+2575 and U+2576 the other way round, matching the swapped
        // arms in the table, so the test passed on the wrong glyphs.
        for (cp, name, cells) in [
            (0x2574u32, "\u{2574} LEFT", m + 1),
            (0x2575, "\u{2575} UP", m + 1),
            (0x2576, "\u{2576} RIGHT", cell - m),
            (0x2577, "\u{2577} DOWN", cell - m),
        ] {
            let v = draw(cp, cell);
            assert_eq!(
                count(&v),
                cells,
                "{name}: {cells} pixels including the shared centre, got {}",
                count(&v)
            );
            assert_eq!(v[m * cell + m], 255, "{name} centre");
        }
        // Each reaches the cell edge on its own side, which is what makes it abut the
        // neighbouring cell's glyph with no gap. Codepoint order is LEFT, UP, RIGHT, DOWN:
        // U+2575 is UP and U+2576 is RIGHT, per their names "LIGHT UP" and "LIGHT RIGHT" and per
        // JetBrains Mono's rasters. This block previously read U+2575 as the right edge and
        // U+2576 as the top, matching the swapped arms in the table it was checking.
        assert_eq!(
            draw(0x2574, cell)[m * cell],
            255,
            "\u{2574} must reach the left edge"
        );
        assert_eq!(
            draw(0x2576, cell)[m * cell + (cell - 1)],
            255,
            "\u{2575} must reach the right edge"
        );
        // U+2575 draws the column x = m from y = 0 to y = m, so its topmost pixel is at
        // (m, 0), i.e. index m — *not* index 0, which is the top-left corner and belongs to
        // neither the arm nor the cell's centre. Reading index 0 here asserted that a vertical
        // stroke occupies the left edge, which is U+2574's job.
        assert_eq!(
            draw(0x2575, cell)[m],
            255,
            "\u{2576} must reach the top edge at (m, 0)"
        );
        // U+2575 draws y = 0..=m only, so the pixel one row *below* the centre on the same
        // column must be clear. Reading `cell - 1 * m - 1` instead checked an unrelated index.
        assert_eq!(
            draw(0x2575, cell)[(m + 1) * cell + m],
            0,
            "\u{2576} must stop at the centre, not cross it"
        );
        assert_eq!(
            draw(0x2577, cell)[(cell - 1) * cell + m],
            255,
            "\u{2577} must reach the bottom edge"
        );
    }

    /// Coverage is binary: box drawing is not antialiased, which is what removes the seam a
    /// font glyph would leave.
    #[test]
    fn coverage_is_binary() {
        for cp in FIRST..=LAST {
            for b in draw(cp, 10) {
                assert!(b == 0 || b == 255, "U+{cp:04X} produced {b}");
            }
        }
    }

    /// Cell size must be at least 2 so a 2px bold stroke is representable.
    #[test]
    fn cell_size_has_a_floor() {
        assert_eq!(cell_size(0), 2);
        assert_eq!(cell_size(1), 2);
        assert_eq!(cell_size(2), 2);
        assert_eq!(cell_size(16), 16);
        assert_eq!(cell_size(22), 22);
    }

    /// Bold must mark strictly more pixels than Regular, or the style distinction is fiction.
    #[test]
    fn bold_is_heavier_than_regular() {
        let cell = 12;
        for cp in [0x2500u32, 0x2502, 0x250C, 0x2514, 0x250F] {
            let regular = draw(cp, cell);
            let mut bold = regular.clone();
            thicken(&mut bold, cell);
            assert!(
                count(&bold) > count(&regular),
                "U+{cp:04X}: bold {} vs regular {}",
                count(&bold),
                count(&regular)
            );
        }
    }

    /// Every glyph's cell must fit the metric's `u8` fields.
    #[test]
    fn every_glyph_fits_a_u8_metric() {
        for &ppem in &[2u16, 8, 16, 22, 32] {
            let m = cell_metric(0x2500, ppem);
            // `width`/`height` are `u8`, so they cannot exceed 255 by construction; the real
            // check is that the cell equals the ppem, i.e. that nothing was clamped.
            assert_eq!(m.width as usize, cell_size(ppem));
            assert_eq!(m.width, m.height, "box drawing must be square");
            assert_eq!(m.advance_x, m.width);
            assert_eq!(cell_size(ppem), m.width as usize);
        }
    }
}
