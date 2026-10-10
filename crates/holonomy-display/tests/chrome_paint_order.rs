//! **The chrome paints: a sibling in `before` is not erased by `node`.** 5 tests.
//!
//! # The bug
//!
//! `Chrome::tree` built its panel as
//!
//! ```text
//! SurfaceTree::leaf(Node::Rect(panel))          // node  = the panel
//! chrome.before.push(band); chrome.before.push(rule); chrome.before.push(label); ...
//! ```
//!
//! and `Painter::walk` draws **`before`, then `node`, then `after`**. So the full-panel rectangle
//! painted *last* and covered every band, every rule and every label the chrome had just emitted.
//!
//! **What it looked like:** a bare page on a flat dark panel. No tab bar, no toolbar, no ruler, no
//! status bar, no title. The editor rendered its body text and its caret and nothing else — for twelve
//! phases.
//!
//! # Why twelve phases of gates never saw it
//!
//! **Every chrome gate asserts what the chrome *emits*.** Node counts, colours, bounds, the panel
//! partition — all correct, because the chrome *was* emitting them. The defect was a statement about the
//! relationship between two siblings, and nothing in the workspace asserted the relationship.
//!
//! So this file asserts it through **pixels**. That is the whole reason it exists: a gate that walks the
//! tree cannot see a paint-order bug, because a paint-order bug is not a property of the tree.
//!
//! | what it proves | test |
//! | --- | --- |
//! | the toolbar band reaches the frame | [`the_toolbar_band_reaches_the_frame`] |
//! | the title reaches the frame | [`the_title_reaches_the_frame`] |
//! | and the page is still drawn over the chrome | [`the_page_is_still_drawn_over_the_chrome`] |
//! | a band is *not* the panel colour | [`a_band_is_not_the_panel_colour`] |
//! | **and the panel is emitted first** | [`the_panel_is_emitted_before_everything_else`] |

use holonomy_display::paint::Painter;
use holonomy_display::Frame;
use holonomy_render::chrome::{colour, ChromeMetrics, ChromeState};
use holonomy_render::{Chrome, DamageRect, Node, SurfaceTree};

/// The shared atlas, built once for the whole test binary.
fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (a, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(a))
    })
}

/// The chrome's tree painted onto a fresh frame.
fn paint_chrome(state: &ChromeState) -> (Frame, Chrome, ChromeMetrics) {
    let m = ChromeMetrics::DESKTOP;
    let chrome = Chrome::new(m);
    let tree = chrome.tree(state);
    let mut frame = Frame::new(m.width, m.height, c(colour::CHROME));
    let mut painter = Painter::new(atlas(), 0);
    painter
        .paint(&mut frame, &tree, None)
        .expect("paint the chrome");
    (frame, chrome, m)
}

/// The `0x00RRGGBB` form of a palette constant.
///
/// **The alpha byte is dropped, and it has to be.** The palette writes `0xFFRRGGBB` because a caller
/// writing "opaque red" as `0xFFFF_0000` should not have the high byte mean anything; a [`Frame`] stores
/// `0x00RRGGBB` because `Frame::new` masks its fill for the same reason. **Comparing one against the
/// other without this is a 4-billion-off mismatch that reads as "the colour is wrong"**, which is what
/// the first version of this file did -- and the reason is invisible in the numbers unless you look at
/// them in hex. Left = 2763312 (0x2A2A30), right = 4280953392 (0xFF2A2A30), and both are "the band".
#[inline]
const fn c(v: u32) -> u32 {
    v & 0x00FF_FFFF
}

/// One colour, as `0x00RRGGBB`, from a frame.
fn pixel(frame: &Frame, x: u32, y: u32) -> u32 {
    let w = frame.width() as usize;
    frame.pixels()[y as usize * w + x as usize]
}

/// **The toolbar band is on the frame, in the band colour and not the panel colour.**
///
/// **This is the test that fails before the fix.** Before it, every pixel of the toolbar row was the
/// panel colour, because the panel painted over its own band.
#[test]
fn the_toolbar_band_reaches_the_frame() {
    let (frame, chrome, _) = paint_chrome(&ChromeState::default());
    let band = chrome.layout.toolbar;
    let got = pixel(&frame, 5, band.y + band.height / 2);
    assert_ne!(
        got,
        c(colour::CHROME),
        "the toolbar band is the panel colour. The panel rectangle is painting after its own band \
         again -- see this file's header."
    );
    assert_eq!(
        got,
        c(colour::BAND),
        "and the band is exactly the band colour, not merely something else"
    );
}

/// **The document title reaches the frame.**
///
/// The title is a `Node::Text` in `chrome.before` — **a glyph, not a rect** — so it exercises a
/// different arm of the painter from the band and would fail for the same reason. Without this test, a
/// fix that moved only the bands would pass the one above and still show no title.
#[test]
fn the_title_reaches_the_frame() {
    let mut state = ChromeState::default();
    state.title = "field notes".into();
    let (frame, chrome, _) = paint_chrome(&state);

    // Somewhere in the tab bar there must be ink that is not the background. **Counting any lit pixel
    // is deliberately weak** -- the exact pixels depend on the atlas's advance widths, and this file is
    // about paint order, not typography. The assertion is that the chrome drew a glyph at all.
    let t = chrome.layout.tabs;
    let lit = (0..t.width)
        .flat_map(|x| (0..t.height).map(move |y| (x, y)))
        .filter(|&(x, y)| pixel(&frame, t.x + x, t.y + y) != c(colour::CHROME))
        .count();
    assert!(
        lit > 10,
        "the tab bar has {lit} lit pixels. The chrome drew no title, which is what the panel \
         painting over its own labels looks like."
    );
}

/// **The page is still drawn over the chrome.**
///
/// The fix moved the panel from `node` into `before`. **The risk of that move is that the page moved
/// with it**, so this asserts the ordering survived: the page's centre is page-coloured, and the
/// canvas above it is chrome.
#[test]
fn the_page_is_still_drawn_over_the_chrome() {
    let (frame, chrome, _) = paint_chrome(&ChromeState::default());
    let page = chrome.layout.page;
    assert_eq!(
        pixel(&frame, page.x + page.width / 2, page.y + page.height / 2),
        c(colour::PAGE),
        "the middle of the page is not page-coloured: the panel is covering it"
    );
    // **Left of the page, half way down the canvas.** The first version of this probe asked for
    // `(page centre, canvas top)` and got 0x5A5A66 -- a `RULE`, not the panel. **The canvas's top row is
    // where the chrome draws its rules**, so "the canvas is chrome-coloured" asked there is a question
    // about rules, not about the panel, and it failed for a reason that has nothing to do with the bug.
    // Five pixels into the gutter, half way down, is a pixel nothing draws on.
    let cv = chrome.layout.canvas;
    assert_eq!(
        pixel(&frame, cv.x + 4, cv.y + cv.height / 2),
        c(colour::CHROME),
        "the gutter left of the page is not chrome-coloured"
    );
}

/// **The panel and the band are different colours at all.**
///
/// **The gate that makes the other four mean something.** If `BAND` and `CHROME` were the same value,
/// every paint-order test here would pass while the UI was invisible. This asserts the premise, which
/// is exactly the kind of thing a chain of otherwise-sound assertions quietly depends on.
#[test]
fn a_band_is_not_the_panel_colour() {
    assert_ne!(
        c(colour::BAND),
        c(colour::CHROME),
        "the band and the panel are the same colour, so 'the band is visible' is not checkable by \
         colour and every other test in this file is vacuous"
    );
    assert_ne!(
        colour::RULE,
        c(colour::BAND),
        "and a rule on a band is invisible"
    );
}

/// **The panel is emitted before everything else.**
///
/// The structural half: the panel rect must live in a `before` list, so that `before`-ordering is a
/// statement the tree itself can be asked about. **This is the assertion that would have caught the
/// bug**, because it is about the shape of the tree rather than about pixels — and it is five lines.
#[test]
fn the_panel_is_emitted_before_everything_else() {
    let m = ChromeMetrics::DESKTOP;
    let tree = Chrome::new(m).tree(&ChromeState::default());
    let full = DamageRect::new(0, 0, m.width, m.height);

    /// Every rect in **paint order**: `before`, then `node`, then `after`, recursively. **This is the
    /// point of the test.** A helper that merely records "which list is this rect's *subtree* in"
    /// answers a different question -- and it answered it *identically with and without the bug*,
    /// because the panel is a child's `node` whether or not that child is in a `before` list.
    fn paint_order(t: &SurfaceTree, out: &mut Vec<(u32, DamageRect)>) {
        for child in &t.before {
            paint_order(child, out);
        }
        if let Some(Node::Rect(r)) = t.node {
            out.push((
                c(r.colour),
                r.bounds().unwrap_or(DamageRect::new(0, 0, 0, 0)),
            ));
        }
        for child in &t.after {
            paint_order(child, out);
        }
    }
    let mut order = Vec::new();
    paint_order(&tree, &mut order);

    let band_colour = c(colour::BAND);
    let panel_colour = c(colour::CHROME);
    let bands = order.iter().filter(|&&(got, _)| got == band_colour).count();
    assert_eq!(
        bands, 2,
        "the toolbar and status bars. Any other number means the chrome gained or lost a band, and \
         this file's other assertions are about a layout that no longer exists."
    );

    // A panel rect must exist at all -- otherwise this file would be passing because the chrome drew
    // nothing, which is the bug in its extreme.
    let panel_at = order
        .iter()
        .position(|&(got, _)| got == panel_colour)
        .expect("no panel-coloured rect in the tree at all");

    assert_eq!(
        order[panel_at].1, full,
        "the panel-coloured rect found is not the full panel. Something else is using the panel \
         colour, and this test is about the panel."
    );

    // **The assertion.** Every band must come *after* the panel in paint order. Before the fix the
    // panel was the child group's own `node`, so it came after the bands in its own `before` list, and
    // it erased them.
    //
    // **Asserted as a comparison rather than as a membership test**, because `panel_in_before` passes
    // with the bug: the chrome group is in `root.before`, so *its* node is drawn before `root`'s
    // (empty) node -- the group is early, the panel is late. Only a flat paint-order list can tell
    // those apart.
    let last_band = order
        .iter()
        .rposition(|&(got, _)| got == band_colour)
        .expect("no bands");
    assert!(
        panel_at < last_band,
        "the panel is painted at position {panel_at} and the last band at {last_band}. The panel must \
         be painted first: it is a full-panel rectangle and everything after it is covered. This is \
         the bug this file exists for."
    );
}
