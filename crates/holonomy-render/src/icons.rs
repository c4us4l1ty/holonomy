//! **The icon set: hand-authored 1-bit masks, authored as ASCII art and packed at compile time.**
//!
//! # Why ASCII in the source and not `.rodata`
//!
//! [`Icon::bits`](crate::Icon::bits) is a `&'static [u64]`, which is the right *runtime* shape: the
//! painter reads it with a shift and a mask and there is no allocation and no decode step. It is a
//! miserable *authoring* shape, because a `u64` literal carries no information about which pixel it
//! sets — it carries information about which **bit**, and the distance between those is 32 bits in one
//! axis.
//!
//! So the masks are written the way a person draws them, as `#` and `.` rows, and [`pack`] turns them
//! into words with a `const fn`. **That is the whole reason this file can be reviewed.** A reviewer of
//! the icon set is looking at pictures, and a change to `undo` is a change to a picture rather than a
//! change to a hexadecimal constant.
//!
//! # Why 1-bit, and why these are not glyphs
//!
//! §2 of PROJECT.md records the decision: the chrome's glyphs must come from `.rodata` — no SVG runtime,
//! no font parsing for UI chrome — and a 1-bit mask is 1 bit per pixel instead of 32, so a 16x16 icon is
//! **32 bytes** rather than 1 KiB. Thirty icons is 960 bytes of masks.
//!
//! **They are not glyphs because the fonts do not contain them.** PROJECT.md §7 item 2 folded this in
//! explicitly: Inter has no Arrows block, so `◀` `▶` `✓` `⋯` are all outside every declared font range,
//! and the decision was a drawn 1-bit mask. The previous chrome worked around this with bracket
//! characters and box-drawing runes, which is why it renders as the row of antenna-like shapes a gate
//! never objected to — **the glyphs existed and looked wrong, and no test asked whether they looked
//! right.** See [`crate::Icon::coverage`] for the bit order and `chrome_paint_order.rs` for the sibling
//! finding.
//!
//! # The artwork convention
//!
//! * `#` is ink, `.` is transparent. A space is also transparent, so a row can be indented to line it up.
//! * Every icon is exactly [`SIZE`] x [`SIZE`] and nothing is centred or optimised. **Uniform boxes are
//!   what make a toolbar read as a toolbar** — an icon that is optically centred on its own and not on
//!   its box produces a row whose buttons do not line up, and the eye notices that long before it can
//!   say why.
//! * **No antialiasing, no greys.** 1 bit is the format, not a compromise. An icon that needs a soft
//!   edge is an icon that needs more than 16 pixels.
//! * **One pen weight for the whole set — strictly 1 px.** *This is the rule the second generation of
//!   artwork exists to enforce.* The first set mixed 1 px outlines with 2 px strokes and solid fills in
//!   the same box, so `Star` carried roughly three times the ink of `Minus` and the bar read as a row of
//!   unrelated marks rather than as one family. At 16 px a 2 px stroke also closes the counters of any
//!   letterform, which is why the old `Bold` and `TextColour` glyphs were unreadable blobs.
//! * **A diagonal is a staircase, not a blur.** Every slanted edge here is a Bresenham line, so a
//!   chevron is a clean 45° step. Rasterising antialiased vector art down to 16 px and then thresholding
//!   it — the first attempt at this rewrite — leaves 50 %-coverage grey on every diagonal, which
//!   thresholds to either a lost line or a doubled one.
//! * **Drawn, not typed.** §2 forbids an SVG runtime for chrome; this is still `&'static [u64]` built by
//!   a `const fn`. What changed is only the *authoring* step, which is geometry rasterised to rows
//!   rather than rows typed by hand.

use crate::Icon;

/// Icons are square, and this is their edge in pixels.
///
/// **16, and it is the font's size rather than a preference.** [`crate::Style::UI`] is a 16 ppem face
/// whose cap height is 11 px, so a 16 px icon sits on the same optical line as the chrome's own text and
/// a row of icons and a row of labels do not visibly disagree about where the middle is.
pub const SIZE: u32 = 16;

/// Pack `#`/`.` art into one `u64` per row, most significant bit leftmost.
///
/// **Bit 63 is pixel 0**, which is the order a person writes a bitmap in and the order a hex editor
/// dumps one in. [`Icon::coverage`] reads bit `63 - (x % 64)` for the same reason, and the two have to
/// agree or every icon is mirrored horizontally — which looks *almost* right, which is why the tests
/// assert the packing directly rather than by eye.
///
/// `const`, so the masks land in `.rodata` with no runtime construction and no `LazyLock`.
pub const fn pack(rows: &[&[u8; SIZE as usize]]) -> [u64; SIZE as usize] {
    let mut out = [0u64; SIZE as usize];
    let mut y = 0;
    while y < rows.len() && y < out.len() {
        let row = rows[y];
        let mut x = 0;
        while x < SIZE as usize {
            // `!= b'.'` rather than `== b'#'`, so a row padded with spaces works. Every row in this
            // file is authored with `.` because a stray space is invisible in a review and `#` is not.
            if row[x] != b'.' && row[x] != b' ' {
                out[y] |= 1u64 << (63 - x);
            }
            x += 1;
        }
        y += 1;
    }
    out
}

// One of the masks below, by name.
//
// A `const` rather than a `static` per icon so the whole catalogue is one symbol and a caller cannot
// accidentally hold a reference into a `Vec`.
//
// **A comment and not a doc comment.** The `///` above `pub mod mask` documented the *module*, and the
// inner `//!` on its first line re-documented it -- so rustdoc saw one item described twice in two
// voices, the outer `///` won, and the inner `//!` became an error under
// `clippy::mixed_attributes_style`. **The outer one is the module's documentation and it is the one a
// reader of the module sees**, so the inner line is gone and this paragraph is a plain comment.
pub mod mask {
    //! The masks. One `static` per icon, each `const`-evaluated so it lands in `.rodata`.

    use super::pack;

    /// A page with lines on it: the document, a sidebar tab, anything "open this".
    pub static DOC: [u64; 16] = pack(&[
        b"................",
        b"...##########...",
        b"...#........#...",
        b"...#........#...",
        b"...#.######.#...",
        b"...#........#...",
        b"...#........#...",
        b"...#.######.#...",
        b"...#........#...",
        b"...#........#...",
        b"...#.####...#...",
        b"...#........#...",
        b"...#........#...",
        b"...#........#...",
        b"...##########...",
        b"................",
    ]);

    /// An arrow curling left and down: undo.
    pub static UNDO: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"............#...",
        b"............#...",
        b".....#......#...",
        b"...###......#...",
        b"..####......#...",
        b"...##########...",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);

    /// The mirror of [`UNDO`].
    pub static REDO: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"...#............",
        b"...#............",
        b"...#......#.....",
        b"...#......###...",
        b"...#......####..",
        b"...##########...",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);

    /// A tick with a long tail: the spellcheck mark.
    pub static SPELLCHECK: [u64; 16] = pack(&[
        b"................",
        b"...#............",
        b"...#............",
        b"..#.#...........",
        b"..#.#........#..",
        b"..#.#.......#...",
        b".#####......#...",
        b".#...#.....#....",
        b".#...#.....#....",
        b".#...#....#.....",
        b"..........#.....",
        b"......#..#......",
        b".......#.#......",
        b"........#.......",
        b"................",
        b"................",
    ]);

    /// A roller above a stroke: the format painter.
    pub static PAINT_FORMAT: [u64; 16] = pack(&[
        b"................",
        b"................",
        b".###########....",
        b".#.........#....",
        b".#.........###..",
        b".#.........#.#..",
        b".###########.#..",
        b".............#..",
        b".............#..",
        b"........######..",
        b"........#..#....",
        b"........#..#....",
        b"........#..#....",
        b"........#..#....",
        b"........####....",
        b"................",
    ]);

    /// A down chevron: a dropdown, a popup's anchor, "this opens".
    pub static CHEVRON_DOWN: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"....#.......#...",
        b".....#.....#....",
        b"......#...#.....",
        b".......#.#......",
        b"........#.......",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);

    /// The mirror of [`CHEVRON_DOWN`]: collapse.
    pub static CHEVRON_UP: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"........#.......",
        b".......#.#......",
        b"......#...#.....",
        b".....#.....#....",
        b"....#.......#...",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);

    /// A right chevron: a submenu.
    pub static CHEVRON_RIGHT: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"................",
        b"......#.........",
        b".......#........",
        b"........#.......",
        b".........#......",
        b"..........#.....",
        b".........#......",
        b"........#.......",
        b".......#........",
        b"......#.........",
        b"................",
        b"................",
        b"................",
    ]);

    /// A minus in a box: decrease the font size.
    pub static MINUS: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"...##########...",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);

    /// A plus in a box: increase the font size.
    pub static PLUS: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"........#.......",
        b"........#.......",
        b"........#.......",
        b"........#.......",
        b"........#.......",
        b"...##########...",
        b"........#.......",
        b"........#.......",
        b"........#.......",
        b"........#.......",
        b"........#.......",
        b"................",
        b"................",
    ]);

    /// A bold `B`.
    pub static BOLD: [u64; 16] = pack(&[
        b"................",
        b"....########....",
        b"....#......##...",
        b"....#.......#...",
        b"....#.......#...",
        b"....#.......#...",
        b"....#......##...",
        b"....#....###....",
        b"....########....",
        b"....#......##...",
        b"....#.......#...",
        b"....#.......#...",
        b"....#.......#...",
        b"....#......##...",
        b"....########....",
        b"................",
    ]);

    /// An italic `I`, slanted.
    pub static ITALIC: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"......#######...",
        b"..........#.....",
        b".........#......",
        b".........#......",
        b"........#.......",
        b"........#.......",
        b"........#.......",
        b".......#........",
        b".......#........",
        b"......#.........",
        b"...#######......",
        b"................",
        b"................",
    ]);

    /// A `U` with a rule under it.
    pub static UNDERLINE: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"....#.......#...",
        b"....#.......#...",
        b"....#.......#...",
        b"....#.......#...",
        b"....#.......#...",
        b"....#.......#...",
        b"....#.......#...",
        b"....#.......#...",
        b"....##.....##...",
        b".....##...##....",
        b"......#####.....",
        b"................",
        b"..#############.",
        b"................",
    ]);

    /// An `A` over a filled rule: the text colour swatch.
    pub static TEXT_COLOUR: [u64; 16] = pack(&[
        b"................",
        b"....#####.......",
        b"....#...#.......",
        b"...#.....#......",
        b"...#.....#......",
        b"...#.....#......",
        b"..#.......#.....",
        b"..#########.....",
        b"..#.......#.....",
        b".#.........#....",
        b".#.........#....",
        b"................",
        b"................",
        b".##############.",
        b".##############.",
        b"................",
    ]);

    /// A marker laying a thick rule.
    pub static HIGHLIGHT: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"..........#.....",
        b".........#.#....",
        b"........#...#...",
        b".......#.....#..",
        b".......#......#.",
        b"......#......#..",
        b".....#......#...",
        b"....#......#....",
        b"...#......#.....",
        b"...#.....#......",
        b"...#....#.......",
        b".##############.",
        b".##############.",
        b"................",
    ]);

    /// Two links of a chain.
    pub static LINK: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b".##############.",
        b".#......#.....#.",
        b".#......#.....#.",
        b".#......#.....#.",
        b".#......#.....#.",
        b".##############.",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);

    /// A frame with a sun and a mountain.
    pub static IMAGE: [u64; 16] = pack(&[
        b"................",
        b"................",
        b".##############.",
        b".#............#.",
        b".#........###.#.",
        b".#........###.#.",
        b".#........###.#.",
        b".#....#.......#.",
        b".#...#.#...#..#.",
        b".#..#...#.#.#.#.",
        b".#..#....#...##.",
        b".#.#..........#.",
        b".##...........#.",
        b".##############.",
        b"................",
        b"................",
    ]);

    /// Three vertical dots: overflow, and a row's own menu.
    pub static OVERFLOW: [u64; 16] = pack(&[
        b"................",
        b".......###......",
        b".......###......",
        b".......###......",
        b"................",
        b"................",
        b"................",
        b".......###......",
        b".......###......",
        b".......###......",
        b"................",
        b"................",
        b"................",
        b".......###......",
        b".......###......",
        b".......###......",
    ]);

    /// An arrow pointing left: back, and close the sidebar.
    pub static ARROW_LEFT: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b".......#........",
        b"......#.........",
        b".....#..........",
        b"....#...........",
        b"...#............",
        b"..#############.",
        b"...#............",
        b"....#...........",
        b".....#..........",
        b"......#.........",
        b".......#........",
        b"................",
        b"................",
    ]);

    /// A five-pointed star, filled. **Deliberately not larger** -- see `no_icon_is_mostly_ink`.
    pub static STAR: [u64; 16] = pack(&[
        b"................",
        b"........#.......",
        b"........#.......",
        b".......##.......",
        b".......###......",
        b"......####......",
        b".##############.",
        b"..############..",
        b"....########....",
        b".....######.....",
        b".....######.....",
        b"....#######.....",
        b"....###...##....",
        b"................",
        b"................",
        b"................",
    ]);

    /// A folder: open, save-to.
    pub static FOLDER: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b".######.........",
        b".#.....#........",
        b".#......#######.",
        b".#............#.",
        b".#............#.",
        b".##############.",
        b".#............#.",
        b".#............#.",
        b".#............#.",
        b".##############.",
        b"................",
        b"................",
        b"................",
    ]);

    /// A cloud: "saved".
    pub static CLOUD: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"........#####...",
        b"...######...##..",
        b"..#....#.....#..",
        b"..#....#.....#..",
        b"..############..",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);

    /// A clock with hands: version history.
    pub static HISTORY: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b".......###......",
        b"......#.#.#.....",
        b".....#..#..#....",
        b"....#...#...#...",
        b"...#....#....#..",
        b"...#....#....#..",
        b"...#.....##..#..",
        b"....#......##...",
        b".....#.....#....",
        b"......#...#.....",
        b".......###......",
        b"................",
        b"................",
    ]);

    /// A padlock: sealed, and share.
    pub static LOCK: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"......#####.....",
        b".....##...##....",
        b".....#.....#....",
        b"....##.....##...",
        b"....#.......#...",
        b"..############..",
        b"..#.#.......##..",
        b"..#....###...#..",
        b"..#....###...#..",
        b"..#....###...#..",
        b"..#..........#..",
        b"..############..",
        b"................",
    ]);

    /// A four-pointed sparkle: the app mark, and "new".
    pub static SPARKLE: [u64; 16] = pack(&[
        b"........#.......",
        b"........#.......",
        b".......###......",
        b".......###......",
        b".......###......",
        b"......#####.....",
        b"......#####.....",
        b"...###########..",
        b"################",
        b"...###########..",
        b"......#####.....",
        b"......#####.....",
        b".......###......",
        b".......###......",
        b".......###......",
        b"........#.......",
    ]);

    /// A tick: a menu item that is currently chosen.
    pub static CHECK: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"..............#.",
        b".............#..",
        b"............#...",
        b"............#...",
        b"...........#....",
        b"..#.......#.....",
        b"...#.....#......",
        b"....#...#.......",
        b"....#...#.......",
        b".....#.#........",
        b"......#.........",
        b"................",
        b"................",
    ]);

    /// A grid.
    pub static TABLE: [u64; 16] = pack(&[
        b"................",
        b"................",
        b".##############.",
        b".#...#....#...#.",
        b".#...#....#...#.",
        b".#...#....#...#.",
        b".#...#....#...#.",
        b".#...#....#...#.",
        b".##############.",
        b".#...#....#...#.",
        b".#...#....#...#.",
        b".#...#....#...#.",
        b".#...#....#...#.",
        b".##############.",
        b"................",
        b"................",
    ]);

    /// A ribbon bookmark, with the notch at the foot.
    pub static BOOKMARK: [u64; 16] = pack(&[
        b"................",
        b"....########....",
        b"....#......#....",
        b"....#......#....",
        b"....#......#....",
        b"....#......#....",
        b"....#......#....",
        b"....#......#....",
        b"....#......#....",
        b"....#......#....",
        b"....#......#....",
        b"....#...#..#....",
        b"....#..#.#.#....",
        b"....###...##....",
        b"....#......#....",
        b"................",
    ]);

    /// A full-width rule: a horizontal line.
    pub static RULE: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b".##############.",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);

    /// An arrow into a bar: a tab stop.
    pub static TAB: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b".########.......",
        b"................",
        b"..........#.....",
        b"...........#....",
        b"............#...",
        b".............#..",
        b"............#...",
        b"...........#....",
        b"..........#.....",
        b".########.......",
        b"................",
        b"................",
        b"................",
    ]);

    /// A magnifier.
    pub static SEARCH: [u64; 16] = pack(&[
        b"................",
        b".....###........",
        b"....#...#.......",
        b"...#.....#......",
        b"..#.......#.....",
        b".#.........#....",
        b".#.........#....",
        b".#.........#....",
        b"..#.......#.....",
        b"...#.....#......",
        b"....#...#.#.....",
        b".....###...#....",
        b"............#...",
        b".............#..",
        b"..............#.",
        b"................",
    ]);

    /// A pen nib: the mode switcher.
    pub static PEN: [u64; 16] = pack(&[
        b"................",
        b"...........#....",
        b"..........#.#...",
        b".........#...#..",
        b"........#.#...#.",
        b".......#...#.#..",
        b"......#.....#...",
        b".....#.....#....",
        b"....#.....#.....",
        b"...#.....#......",
        b"..#.....#.......",
        b"..#....#........",
        b"..#...#.........",
        b".#..##..........",
        b".###............",
        b"................",
    ]);

    /// Print: a page over a tray.
    pub static PRINT: [u64; 16] = pack(&[
        b"................",
        b"....########....",
        b"....#......#....",
        b"....#......#....",
        b"....#......#....",
        b".##############.",
        b".#............#.",
        b".#............#.",
        b".#............#.",
        b".#..########..#.",
        b".##############.",
        b"....#......#....",
        b"....#......#....",
        b"....#......#....",
        b"....########....",
        b"................",
    ]);
}

/// Every icon, by name.
///
/// **An enum rather than an index**, because the alternative is a `u8` that a caller can pass as
/// 37 and get a mask out of the bounds of a match. Every accessor goes through [`IconId::mask`], which
/// is exhaustive, so adding an icon is a compile error at every site that has to handle it — which is
/// the property that makes the set safe to extend while the toolbar is still being built.
// WordCraft-inspired line icons, pre-rasterised into the same 1-bit masks as the
// existing set. No SVG parser, allocator or curve evaluator enters the UI path.
pub(crate) mod wordcraft_masks {
    use super::{pack, SIZE};
    const fn rows(v: &[&[u8; SIZE as usize]; SIZE as usize]) -> [u64; SIZE as usize] {
        // **Art rows, not `0`/`1` rows.** `pack` inks every byte that is not `.` or a space, so a
        // row written as `[0, 1, 1, 0]` was *every cell inked* -- every Wordcraft icon drew as a
        // solid block, and the two gates that caught it were `no_icon_is_mostly_ink` and
        // `no_two_icons_share_a_mask`.
        pack(v)
    }
    pub const CUT: [u64; SIZE as usize] = rows(&[
        b"................",
        b"................",
        b"...##.......#...",
        b"....#......#....",
        b"....###...#.....",
        b"......#..#......",
        b"......###.......",
        b".......##.......",
        b"......#.###.....",
        b"...###....###...",
        b"..##.##..##.##..",
        b"..#...#..#...#..",
        b"..##.##..##.##..",
        b"...###....###...",
        b"................",
        b"................",
    ]);
    pub const COPY: [u64; SIZE as usize] = rows(&[
        b"................",
        b"................",
        b".....#########..",
        b".....#.......#..",
        b".....#.......#..",
        b"..#########..#..",
        b"..#..#....#..#..",
        b"..#..#....#..#..",
        b"..#..#....#..#..",
        b"..#..#....#..#..",
        b"..#..#....#..#..",
        b"..#..#########..",
        b"..#.......#.....",
        b"..#.......#.....",
        b"..#########.....",
        b"................",
    ]);
    pub const PASTE: [u64; SIZE as usize] = rows(&[
        b"................",
        b".....######.....",
        b".....#....#.....",
        b"...##########...",
        b"...#.######.#...",
        b"...#........#...",
        b"...#........#...",
        b"...#........#...",
        b"...#.######.#...",
        b"...#........#...",
        b"...#........#...",
        b"...#.######.#...",
        b"...#........#...",
        b"...#........#...",
        b"...##########...",
        b"................",
    ]);
    pub const BULLETS: [u64; SIZE as usize] = rows(&[
        b"................",
        b"................",
        b"................",
        b"..#...#########.",
        b"................",
        b"................",
        b"................",
        b"................",
        b"..#...#########.",
        b"................",
        b"................",
        b"................",
        b"................",
        b"..#...#########.",
        b"................",
        b"................",
    ]);
    pub const NUMBERING: [u64; SIZE as usize] = rows(&[
        b"................",
        b"................",
        b"..#.............",
        b"..#...#########.",
        b"..#.............",
        b".###............",
        b"................",
        b"................",
        b"................",
        b".###............",
        b"...#..#########.",
        b".###............",
        b"................",
        b"................",
        b".###............",
        b"................",
    ]);
    pub const ALIGNLEFT: [u64; SIZE as usize] = rows(&[
        b"................",
        b"................",
        b".##############.",
        b"................",
        b"................",
        b"................",
        b".##########.....",
        b"................",
        b"................",
        b"................",
        b".##############.",
        b"................",
        b"................",
        b"................",
        b".##########.....",
        b"................",
    ]);
    pub const ALIGNCENTER: [u64; SIZE as usize] = rows(&[
        b"................",
        b"................",
        b".##############.",
        b"................",
        b"................",
        b"................",
        b"...##########...",
        b"................",
        b"................",
        b"................",
        b".##############.",
        b"................",
        b"................",
        b"................",
        b"...##########...",
        b"................",
    ]);
    pub const ALIGNRIGHT: [u64; SIZE as usize] = rows(&[
        b"................",
        b"................",
        b".##############.",
        b"................",
        b"................",
        b"................",
        b".....##########.",
        b"................",
        b"................",
        b"................",
        b".##############.",
        b"................",
        b"................",
        b"................",
        b".....##########.",
        b"................",
    ]);
    pub const INDENT: [u64; SIZE as usize] = rows(&[
        b"................",
        b"................",
        b".......########.",
        b"................",
        b"................",
        b".#..............",
        b"..#....########.",
        b"...#............",
        b"....#...........",
        b"...#............",
        b"..#....########.",
        b".#..............",
        b"................",
        b"................",
        b".......########.",
        b"................",
    ]);
    pub const OUTDENT: [u64; SIZE as usize] = rows(&[
        b"................",
        b"................",
        b".......########.",
        b"................",
        b"................",
        b"....#...........",
        b"..###..########.",
        b"..#.............",
        b".##.............",
        b"..#.............",
        b"..###..########.",
        b"....#...........",
        b"................",
        b"................",
        b".......########.",
        b"................",
    ]);
    pub const STRIKE: [u64; SIZE as usize] = rows(&[
        b"................",
        b"................",
        b"......#####.....",
        b".....#....##....",
        b"....#...........",
        b".....#..........",
        b"......#.........",
        b".......#........",
        b".##############.",
        b".........#......",
        b"..........#.....",
        b"..........#.....",
        b"..........#.....",
        b"....###..#......",
        b"......###.......",
        b"................",
    ]);
    /// A speech bubble: comment.
    pub const COMMENT: [u64; SIZE as usize] = rows(&[
        b"................",
        b"...#######......",
        b"..#......#......",
        b".#...........#..",
        b".#...........#..",
        b".#...........#..",
        b".#...........#..",
        b".#...........#..",
        b".#...........#..",
        b".#############..",
        b"............#...",
        b"..............#.",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);
    /// Three connected nodes: share.
    pub const SHARE: [u64; SIZE as usize] = rows(&[
        b"................",
        b"..##......##....",
        b".#..#....#..#...",
        b".#..#....#..#...",
        b".#...........#..",
        b"..##......##....",
        b"................",
        b"..##......##....",
        b".#..#....#..#...",
        b".#...........#..",
        b".#...........#..",
        b"..##......##....",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);
    pub const SUBSCRIPT: [u64; SIZE as usize] = rows(&[
        b"................",
        b"................",
        b"................",
        b"..#......#......",
        b"..##....##......",
        b"....#..#........",
        b"....####........",
        b".....##.........",
        b".....##.........",
        b"....#..#........",
        b"...##..##.......",
        b"...#....#..####.",
        b"..#......#...##.",
        b"............#...",
        b"...........##...",
        b"...........####.",
    ]);
    pub const SUPERSCRIPT: [u64; SIZE as usize] = rows(&[
        b"................",
        b"...........####.",
        b".............##.",
        b"............#...",
        b"...........##...",
        b"..#......#.####.",
        b"..##....##......",
        b"....#..#........",
        b"....####........",
        b".....##.........",
        b".....##.........",
        b"....#..#........",
        b"...##..##.......",
        b"...#....#.......",
        b"..#......#......",
        b"................",
    ]);
    pub const REPLACE: [u64; SIZE as usize] = rows(&[
        b"................",
        b"................",
        b".########.#####.",
        b".#......#.....#.",
        b".#......#...#.#.",
        b".#......#....##.",
        b".#......#.....#.",
        b".#......#.......",
        b".##############.",
        b".......#......#.",
        b".#####.#......#.",
        b".#.....#......#.",
        b".#.....#......#.",
        b".#.....#......#.",
        b".#.....########.",
        b"................",
    ]);
    pub const NAVIGATION: [u64; SIZE as usize] = rows(&[
        b"................",
        b"................",
        b".##############.",
        b".#...#........#.",
        b".#...#........#.",
        b".#...#..#####.#.",
        b".#...#........#.",
        b".#...#........#.",
        b".#...#..#####.#.",
        b".#...#........#.",
        b".#...#........#.",
        b".#...#..#####.#.",
        b".#...#........#.",
        b".#...#........#.",
        b".##############.",
        b"................",
    ]);
    pub const THEME: [u64; SIZE as usize] = rows(&[
        b"................",
        b"................",
        b"......#####.....",
        b"....##..#.###...",
        b"...##...#...##..",
        b"...#....###..#..",
        b"..#.....#.###.#.",
        b"..#.....#.....#.",
        b"..#.....###...#.",
        b"..#.....#..####.",
        b"..#.....#.....#.",
        b"...#....##...#..",
        b"...##...#.####..",
        b"....##..#..##...",
        b"......#####.....",
        b"................",
    ]);
    pub const CLOSE: [u64; SIZE as usize] = rows(&[
        b"................",
        b"................",
        b"................",
        b"...#........#...",
        b"....#.....###...",
        b".....#....#.....",
        b"......#.###.....",
        b".......##.......",
        b"......###.......",
        b"......#..#......",
        b"....###...#.....",
        b"....#......#....",
        b"...##.......#...",
        b"................",
        b"................",
        b"................",
    ]);
    /// **A selection marquee, not a tick.** `Select all` was drawn with `IconId::Check`, which in every
    /// toolbar in the world means *done* or *applied* -- so the Editing group read `Find`, `Replace`,
    /// then a tick that appeared to confirm the two before it. A dashed rect with a corner handle is
    /// what "this is selected" looks like, and it is what the screenshot was missing.
    pub const SELECT: [u64; SIZE as usize] = rows(&[
        b"................",
        b"................",
        b"..##.##.##.##...",
        b"................",
        b"..#..........#..",
        b"................",
        b"..#..........#..",
        b"................",
        b"..#..........#..",
        b"................",
        b"..#..........#..",
        b"................",
        b"..##.##.##.##...",
        b"................",
        b"................",
        b"................",
    ]);
}
use wordcraft_masks as wc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum IconId {
    /// A page with lines on it.
    Doc,
    /// Undo.
    Undo,
    /// Redo.
    Redo,
    /// Spellcheck.
    Spellcheck,
    /// Format painter.
    PaintFormat,
    /// Down chevron.
    ChevronDown,
    /// Up chevron.
    ChevronUp,
    /// Right chevron.
    ChevronRight,
    /// Decrease.
    Minus,
    /// Increase.
    Plus,
    /// Bold.
    Bold,
    /// Italic.
    Italic,
    /// Underline.
    Underline,
    /// Text colour.
    TextColour,
    /// Highlight.
    Highlight,
    /// Link.
    Link,
    /// Image.
    Image,
    /// Overflow.
    Overflow,
    /// Arrow left.
    ArrowLeft,
    /// Star.
    Star,
    /// Folder.
    Folder,
    /// Cloud.
    Cloud,
    /// History.
    History,
    /// Lock.
    Lock,
    /// Sparkle.
    Sparkle,
    /// Check.
    Check,
    /// Table.
    Table,
    /// Bookmark.
    Bookmark,
    /// Horizontal rule.
    Rule,
    /// Tab stop.
    Tab,
    /// Search.
    Search,
    /// Pen.
    Pen,
    /// Print.
    Print,
    Cut,
    Copy,
    Paste,
    Bullets,
    Numbering,
    AlignLeft,
    AlignCenter,
    AlignRight,
    Indent,
    Outdent,
    Strike,
    Comment,
    Share,
    Subscript,
    Superscript,
    Replace,
    /// A dashed selection marquee.
    Select,
    Navigation,
    Theme,
    Close,
}

// A slightly larger set for the desktop's higher zoom. Masks remain static and
// one-bit; no vector rasterizer or allocation is needed on the paint path.
const LARGE_SIZE: u32 = 20;
const fn enlarged_masks() -> [[u64; LARGE_SIZE as usize]; IconId::ALL.len()] {
    let mut out = [[0u64; LARGE_SIZE as usize]; IconId::ALL.len()];
    let mut icon = 0;
    while icon < IconId::ALL.len() {
        let src = IconId::ALL[icon].mask();
        let mut y = 0;
        while y < LARGE_SIZE {
            let mut x = 0;
            while x < LARGE_SIZE {
                let sx = x * SIZE / LARGE_SIZE;
                let sy = y * SIZE / LARGE_SIZE;
                if src[sy as usize] & (1u64 << (63 - sx)) != 0 {
                    out[icon][y as usize] |= 1u64 << (63 - x);
                }
                x += 1;
            }
            y += 1;
        }
        icon += 1;
    }
    out
}
static LARGE_MASKS: [[u64; LARGE_SIZE as usize]; IconId::ALL.len()] = enlarged_masks();

impl IconId {
    /// Every icon, in declaration order.
    pub const ALL: &'static [IconId] = &[
        IconId::Doc,
        IconId::Undo,
        IconId::Redo,
        IconId::Spellcheck,
        IconId::PaintFormat,
        IconId::ChevronDown,
        IconId::ChevronUp,
        IconId::ChevronRight,
        IconId::Minus,
        IconId::Plus,
        IconId::Bold,
        IconId::Italic,
        IconId::Underline,
        IconId::TextColour,
        IconId::Highlight,
        IconId::Link,
        IconId::Image,
        IconId::Overflow,
        IconId::ArrowLeft,
        IconId::Star,
        IconId::Folder,
        IconId::Cloud,
        IconId::History,
        IconId::Lock,
        IconId::Sparkle,
        IconId::Check,
        IconId::Table,
        IconId::Bookmark,
        IconId::Rule,
        IconId::Tab,
        IconId::Search,
        IconId::Pen,
        IconId::Print,
        IconId::Cut,
        IconId::Copy,
        IconId::Paste,
        IconId::Bullets,
        IconId::Numbering,
        IconId::AlignLeft,
        IconId::AlignCenter,
        IconId::AlignRight,
        IconId::Indent,
        IconId::Outdent,
        IconId::Strike,
        IconId::Comment,
        IconId::Share,
        IconId::Subscript,
        IconId::Superscript,
        IconId::Replace,
        IconId::Select,
        IconId::Navigation,
        IconId::Theme,
        IconId::Close,
    ];

    /// The mask for this icon, and its edge in pixels.
    ///
    /// **Every icon is [`SIZE`] square**, so the height is not returned separately and a caller cannot
    /// get a mask whose height disagrees with its words. If a non-square icon is ever needed this
    /// returns `(mask, width, height)` instead, and that is the day the uniform box assumption in the
    /// module docs stops being free.
    pub const fn mask(self) -> &'static [u64] {
        match self {
            IconId::Doc => &mask::DOC,
            IconId::Undo => &mask::UNDO,
            IconId::Redo => &mask::REDO,
            IconId::Spellcheck => &mask::SPELLCHECK,
            IconId::PaintFormat => &mask::PAINT_FORMAT,
            IconId::ChevronDown => &mask::CHEVRON_DOWN,
            IconId::ChevronUp => &mask::CHEVRON_UP,
            IconId::ChevronRight => &mask::CHEVRON_RIGHT,
            IconId::Minus => &mask::MINUS,
            IconId::Plus => &mask::PLUS,
            IconId::Bold => &mask::BOLD,
            IconId::Italic => &mask::ITALIC,
            IconId::Underline => &mask::UNDERLINE,
            IconId::TextColour => &mask::TEXT_COLOUR,
            IconId::Highlight => &mask::HIGHLIGHT,
            IconId::Link => &mask::LINK,
            IconId::Image => &mask::IMAGE,
            IconId::Overflow => &mask::OVERFLOW,
            IconId::ArrowLeft => &mask::ARROW_LEFT,
            IconId::Star => &mask::STAR,
            IconId::Folder => &mask::FOLDER,
            IconId::Cloud => &mask::CLOUD,
            IconId::History => &mask::HISTORY,
            IconId::Lock => &mask::LOCK,
            IconId::Sparkle => &mask::SPARKLE,
            IconId::Check => &mask::CHECK,
            IconId::Table => &mask::TABLE,
            IconId::Bookmark => &mask::BOOKMARK,
            IconId::Rule => &mask::RULE,
            IconId::Tab => &mask::TAB,
            IconId::Search => &mask::SEARCH,
            IconId::Pen => &mask::PEN,
            IconId::Print => &mask::PRINT,
            IconId::Cut => &wc::CUT,
            IconId::Copy => &wc::COPY,
            IconId::Paste => &wc::PASTE,
            IconId::Bullets => &wc::BULLETS,
            IconId::Numbering => &wc::NUMBERING,
            IconId::AlignLeft => &wc::ALIGNLEFT,
            IconId::AlignCenter => &wc::ALIGNCENTER,
            IconId::AlignRight => &wc::ALIGNRIGHT,
            IconId::Indent => &wc::INDENT,
            IconId::Outdent => &wc::OUTDENT,
            IconId::Strike => &wc::STRIKE,
            IconId::Comment => &wc::COMMENT,
            IconId::Share => &wc::SHARE,
            IconId::Subscript => &wc::SUBSCRIPT,
            IconId::Superscript => &wc::SUPERSCRIPT,
            IconId::Replace => &wc::REPLACE,
            IconId::Select => &wc::SELECT,
            IconId::Navigation => &wc::NAVIGATION,
            IconId::Theme => &wc::THEME,
            IconId::Close => &wc::CLOSE,
        }
    }

    /// This icon as an [`Icon`] at `(x, y)` in `colour`.
    ///
    /// The placement is absolute rather than a box-plus-inset, because **the caller already knows the
    /// pixel it wants** — it just laid out a toolbar and knows where this button sits. A rect-and-inset
    /// API would push the rounding decision to every call site, and fifteen call sites rounding
    /// differently is a toolbar whose icons do not line up.
    #[must_use]
    pub const fn at(self, x: i32, y: i32, colour: u32) -> Icon {
        Icon {
            bits: self.mask(),
            width: SIZE,
            height: SIZE,
            x,
            y,
            colour,
        }
    }

    /// A 20 px desktop icon, using a compile-time enlarged mask.
    #[must_use]
    pub fn at_large(self, x: i32, y: i32, colour: u32) -> Icon {
        Icon {
            bits: &LARGE_MASKS[self as usize],
            width: LARGE_SIZE,
            height: LARGE_SIZE,
            x,
            y,
            colour,
        }
    }

    /// This icon's bounds at `(x, y)`.
    #[must_use]
    pub fn bounds_at(self, x: i32, y: i32) -> Option<crate::DamageRect> {
        self.at(x, y, 0).bounds()
    }

    /// How many distinct pixels are inked. **A gate, and not trivia** — see `icons.rs`'s own tests.
    pub fn ink_count(self) -> u32 {
        let m = self.mask();
        let mut n = 0u32;
        for word in m.iter().take(SIZE as usize) {
            let mut w = *word;
            while w != 0 {
                n += (w & 1) as u32;
                w >>= 1;
            }
        }
        n
    }
}
