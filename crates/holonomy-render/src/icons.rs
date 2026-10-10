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
//! * Shapes are drawn on the **even pixel grid where the shape permits**, so a 16x16 stroke is 2 px and
//!   a 1 px stroke is used only where it must read at 1x.

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

/// One of the masks below, by name.
///
/// A `const` rather than a `static` per icon so the whole catalogue is one symbol and a caller cannot
/// accidentally hold a reference into a `Vec`.
pub mod mask {
    //! The masks. One `static` per icon, each `const`-evaluated so it lands in `.rodata`.

    use super::pack;

    /// A page with lines on it: the document, a sidebar tab, anything "open this".
    pub static DOC: [u64; 16] = pack(&[
        b"................",
        b"...##########...",
        b"...#........#...",
        b"...#.######.#...",
        b"...#........#...",
        b"...#.######.#...",
        b"...#........#...",
        b"...#.######.#...",
        b"...#........#...",
        b"...#.####...#...",
        b"...#........#...",
        b"...#.######.#...",
        b"...#........#...",
        b"...##########...",
        b"................",
        b"................",
    ]);

    /// An arrow curling left and down: undo.
    pub static UNDO: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"....#####.......",
        b"...#.....#......",
        b"..#.......#.....",
        b"..#.......#.....",
        b"...#.....#......",
        b"....#####.......",
        b"......#.........",
        b".....#.#........",
        b"....#...#.......",
        b"...#.....#......",
        b"..#.......#.....",
        b".#.........#....",
        b"................",
        b"................",
    ]);

    /// The mirror of [`UNDO`].
    pub static REDO: [u64; 16] = pack(&[
        b"................",
        b"................",
        b".......#####....",
        b"......#.....#...",
        b".....#.......#..",
        b".....#.......#..",
        b"......#.....#...",
        b".......#####....",
        b".........#......",
        b"........#.#.....",
        b".......#...#....",
        b"......#.....#...",
        b".....#.......#..",
        b"....#.........#.",
        b"................",
        b"................",
    ]);

    /// A printer: paper out the top, a body, a tray at the bottom.
    pub static PRINT: [u64; 16] = pack(&[
        b"................",
        b".....######.....",
        b".....#....#.....",
        b".....#....#.....",
        b"..############..",
        b"..#..........#..",
        b"..#..........#..",
        b"..#..........#..",
        b"..############..",
        b"..#..........#..",
        b".....#....#.....",
        b".....#....#.....",
        b".....######.....",
        b"................",
        b"................",
        b"................",
    ]);

    /// A tick with a long tail: the spellcheck mark.
    pub static SPELLCHECK: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"...........##...",
        b"..........#..#..",
        b"..#......#...#..",
        b"..#.....#....#..",
        b"...#...#.....#..",
        b"....#.#......#..",
        b".....#........#.",
        b"....###.........",
        b"................",
        b"................",
    ]);

    /// A roller above a stroke: the format painter.
    pub static PAINT_FORMAT: [u64; 16] = pack(&[
        b"................",
        b".........####...",
        b"........#....#..",
        b"........#.##.#..",
        b"........#.##.#..",
        b"........#....#..",
        b".....######.....",
        b".....#..........",
        b".....#..........",
        b".....#..........",
        b".....#..........",
        b".....#..........",
        b".....#..........",
        b"....###.........",
        b"................",
        b"................",
    ]);

    /// A down chevron: a dropdown, a popup's anchor, "this opens".
    pub static CHEVRON_DOWN: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"....##......##..",
        b".....##....##...",
        b"......##..##....",
        b".......####.....",
        b"................",
        b"................",
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
        b".......####.....",
        b"......##..##....",
        b".....##....##...",
        b"....##......##..",
        b"................",
        b"................",
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
        b"................",
        b".......##.......",
        b"......#..#......",
        b".....#....#.....",
        b"....#......#....",
        b".....#....#.....",
        b"......#..#......",
        b".......##.......",
        b"................",
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
        b".....##....##...",
        b"....#..#....#...",
        b"....#..#....#...",
        b"....#..#....#...",
        b"....#..#....#...",
        b".....##....##...",
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
        b".....##....##...",
        b"....#..#....#...",
        b"....#..#....#...",
        b"....#..#####....",
        b"....##..#..##...",
        b".....#..#..#....",
        b"....#..#####....",
        b"....#..#....#...",
        b"....#..#....#...",
        b".....##....##...",
        b"................",
        b"................",
        b"................",
    ]);

    /// A bold `B`.
    pub static BOLD: [u64; 16] = pack(&[
        b"................",
        b"...#####........",
        b"...#...#........",
        b"...#...#........",
        b"...#####........",
        b"...#...#........",
        b"...#...#........",
        b"...#...#........",
        b"...#####........",
        b"...#...#........",
        b"...#...#........",
        b"...#...#........",
        b"...#####........",
        b"................",
        b"................",
        b"................",
    ]);

    /// An italic `I`, slanted.
    pub static ITALIC: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"......#####.....",
        b".........#......",
        b"........#.......",
        b".......#........",
        b"......#.........",
        b".....#..........",
        b"....#...........",
        b"...#............",
        b"..#.............",
        b".##.............",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);

    /// A `U` with a rule under it.
    pub static UNDERLINE: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"...#......#.....",
        b"...#......#.....",
        b"...#......#.....",
        b"...#......#.....",
        b"...#......#.....",
        b"...#......#.....",
        b"...########.....",
        b"...#......#.....",
        b"...#......#.....",
        b"...########.....",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);

    /// An `A` over a filled rule: the text colour swatch.
    pub static TEXT_COLOUR: [u64; 16] = pack(&[
        b"................",
        b".......##.......",
        b"......#..#......",
        b".....#....#.....",
        b"....#......#....",
        b"....#......#....",
        b"...#........#...",
        b"...#........#...",
        b"..############..",
        b"...#........#...",
        b"....#......#....",
        b".....#....#.....",
        b"......#..#......",
        b".......##.......",
        b"..############..",
        b"..############..",
    ]);

    /// A marker laying a thick rule.
    pub static HIGHLIGHT: [u64; 16] = pack(&[
        b"................",
        b"..........####..",
        b".........#....#.",
        b".........#.##.#.",
        b".........#.##.#.",
        b".........#....#.",
        b"......#########.",
        b"......#.........",
        b"......#.........",
        b"......#.........",
        b"......#########.",
        b"......#.........",
        b"......#.........",
        b".....###........",
        b"................",
        b"................",
    ]);

    /// Two links of a chain.
    pub static LINK: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"....#####...##..",
        b"...#.....#.#..#.",
        b"..#.......##..#.",
        b"..#........#..#.",
        b"..#........#..#.",
        b"..#.......##..#.",
        b"...#.....#.#..#.",
        b"....#####...##..",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);

    /// A speech balloon with a tail.
    pub static COMMENT: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"..############..",
        b"..#..........#..",
        b"..#..........#..",
        b"..#..........#..",
        b"..#..........#..",
        b"..#..........#..",
        b"..#..........#..",
        b"..############..",
        b".....#..........",
        b"....#...........",
        b"...#............",
        b"..#.............",
        b"................",
        b"................",
    ]);

    /// A frame with a sun and a mountain.
    pub static IMAGE: [u64; 16] = pack(&[
        b"................",
        b"..############..",
        b"..#..........#..",
        b"..#.####....#.#.",
        b"..#.#..#....#.#.",
        b"..#.####....#.#.",
        b"..#..........#..",
        b"..#..........#..",
        b"..#..........#..",
        b"..#.........#.#.",
        b"..#.......##.#.#",
        b"..#......#....#.",
        b"..#..........#..",
        b"..############..",
        b"................",
        b"................",
    ]);

    /// Three vertical dots: overflow, and a row's own menu.
    pub static OVERFLOW: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"......##........",
        b"......##........",
        b"................",
        b"......##........",
        b"......##........",
        b"................",
        b"......##........",
        b"......##........",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);

    /// An arrow pointing left: back, and close the sidebar.
    pub static ARROW_LEFT: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"......##........",
        b".....##.........",
        b"....##..........",
        b"...##...........",
        b"..##............",
        b"..##############",
        b"..##............",
        b"...##...........",
        b"....##..........",
        b".....##.........",
        b"......##........",
        b"................",
        b"................",
    ]);

    /// A five-pointed star, filled. **Deliberately not larger** -- see `no_icon_is_mostly_ink`.
    pub static STAR: [u64; 16] = pack(&[
        b"................",
        b".......##.......",
        b"......####......",
        b"......####......",
        b".....##..##.....",
        b"....##....##....",
        b"...##########...",
        b"..##############",
        b"..############..",
        b"...##########...",
        b"....##....##....",
        b"...##......##...",
        b"..##........##..",
        b".##..........##.",
        b"................",
        b"................",
    ]);

    /// A folder: open, save-to.
    pub static FOLDER: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"..#######.......",
        b"..#.....####....",
        b"..#.........#...",
        b"..#.........#...",
        b"..###########...",
        b"..#.........#...",
        b"..#.........#...",
        b"..#.........#...",
        b"..#.........#...",
        b"..###########...",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);

    /// A cloud: "saved".
    pub static CLOUD: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"......####......",
        b"....##....##....",
        b"...#........#...",
        b"..#..........#..",
        b"..#..........#..",
        b"..#..........#..",
        b"...##......##...",
        b".....######.....",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);

    /// A clock with hands: version history.
    pub static HISTORY: [u64; 16] = pack(&[
        b"................",
        b".....######.....",
        b"...##......##...",
        b"..#..........#..",
        b".#............#.",
        b"#......#.......#",
        b"#......#.......#",
        b"#.......#......#",
        b".#............#.",
        b"..#..........#..",
        b"...##......##...",
        b".....######.....",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);

    /// A padlock: sealed, and share.
    pub static LOCK: [u64; 16] = pack(&[
        b"................",
        b"......####......",
        b".....#....#.....",
        b"....#......#....",
        b"....#......#....",
        b"....########....",
        b"..############..",
        b"..#..........#..",
        b"..#.########.#..",
        b"..#.#......#.#..",
        b"..#.########.#..",
        b"..#..........#..",
        b"..############..",
        b"................",
        b"................",
        b"................",
    ]);

    /// A four-pointed sparkle: the app mark, and "new".
    pub static SPARKLE: [u64; 16] = pack(&[
        b"................",
        b".......##.......",
        b".......##.......",
        b"......####......",
        b"......####......",
        b".....######.....",
        b"....########....",
        b"...##########...",
        b"...##########...",
        b"....########....",
        b".....######.....",
        b"......####......",
        b"......####......",
        b".......##.......",
        b".......##.......",
        b"................",
    ]);

    /// A tick: a menu item that is currently chosen.
    pub static CHECK: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"............##..",
        b"...........#..#.",
        b"..........#...#.",
        b"..#......#....#.",
        b"..#.....#.....#.",
        b"...#...#......#.",
        b"....#.#.......#.",
        b".....#........#.",
        b"................",
        b"................",
    ]);

    /// A grid.
    pub static TABLE: [u64; 16] = pack(&[
        b"................",
        b"..############..",
        b"..#..#..#..#..#.",
        b"..############..",
        b"..#..#..#..#..#.",
        b"..############..",
        b"..#..#..#..#..#.",
        b"..############..",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);

    /// A ribbon bookmark, with the notch at the foot.
    pub static BOOKMARK: [u64; 16] = pack(&[
        b"................",
        b"..##########....",
        b"..#........#....",
        b"..#........#....",
        b"..#........#....",
        b"..#........#....",
        b"..#........#....",
        b"..#..######.....",
        b"..#.#......#....",
        b"..#..........#..",
        b"..#........#....",
        b"..#........#....",
        b"..##########....",
        b"................",
        b"................",
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
        b"..############..",
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
        b"..............##",
        b"..######......##",
        b"......#.......##",
        b"..######......##",
        b"..............##",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
        b"................",
    ]);

    /// A magnifier.
    pub static SEARCH: [u64; 16] = pack(&[
        b"................",
        b"....#####.......",
        b"...#.....#......",
        b"..#.......#.....",
        b"..#..#.#..#.....",
        b"..#..#.#..#.....",
        b"..#..#.#..#.....",
        b"..#.......#.....",
        b"...#.....#......",
        b"....#####.......",
        b"..........###...",
        b"...........###..",
        b"............###.",
        b".............##.",
        b"................",
        b"................",
    ]);

    /// A pen nib: the mode switcher.
    pub static PEN: [u64; 16] = pack(&[
        b"................",
        b"................",
        b"............###.",
        b"...........#.##.",
        b"..........#.##..",
        b".........#.##...",
        b"........#.##....",
        b".......#.##.....",
        b"......#.##......",
        b".....#.##.......",
        b"....##.#........",
        b"..##.#..........",
        b".####...........",
        b"..##............",
        b"................",
        b"................",
    ]);
}

/// Every icon, by name.
///
/// **An enum rather than an index**, because the alternative is a `u8` that a caller can pass as
/// 37 and get a mask out of the bounds of a match. Every accessor goes through [`IconId::mask`], which
/// is exhaustive, so adding an icon is a compile error at every site that has to handle it — which is
/// the property that makes the set safe to extend while the toolbar is still being built.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum IconId {
    /// A page with lines on it.
    Doc,
    /// Undo.
    Undo,
    /// Redo.
    Redo,
    /// Print.
    Print,
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
    /// Comment.
    Comment,
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
}

impl IconId {
    /// Every icon, in declaration order.
    pub const ALL: &'static [IconId] = &[
        IconId::Doc,
        IconId::Undo,
        IconId::Redo,
        IconId::Print,
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
        IconId::Comment,
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
            IconId::Print => &mask::PRINT,
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
            IconId::Comment => &mask::COMMENT,
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
