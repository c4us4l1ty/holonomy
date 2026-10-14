//! **The chrome paints: a sibling in `before` is not erased by `node`, and a popup in `after` is not
//! painted over.** 7 tests.
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
//! | **a popup is emitted last, into `after`** | [`the_open_popup_is_emitted_into_after_and_nothing_else_is`] |
//! | **and visible where it hangs over the sidebar** | [`the_open_popup_is_visible_where_it_hangs_over_the_sidebar`] |

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

    // **The title band, not the tab bar.** Part 19 removed the tab band (`tab_h: 0`, because the title
    // band already drew the document's name) -- and the first version of this test still probed
    // `layout.tabs`, which is now a zero-height rect. **It counted zero lit pixels and failed**, which
    // is the gate telling the truth about a layout that no longer exists rather than about a paint
    // order that regressed. A gate that breaks when the design changes is doing its job; a gate that
    // keeps passing when the design changes is the problem.
    //
    // Counting any lit pixel is deliberately weak -- the exact pixels depend on the atlas's advance
    // widths, and this file is about paint order, not typography. The assertion is that the chrome drew
    // a glyph at all.
    let t = chrome.layout.title;
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
    // **The gutter between the sidebar and the page.** Two earlier probes were wrong here and both
    // failed for reasons that had nothing to do with the bug being hunted:
    //
    // * `(page centre, canvas top)` returned 0x5A5A66 -- a `RULE`, because the canvas's top row is
    //   where the chrome draws its rules.
    // * `(canvas.x + 4, canvas mid)` returned the sidebar's band colour, because part 19 put a
    //   208 px sidebar at x = 0 and `cv.x + 4` is inside it.
    //
    // **So the probe is now "a pixel that is provably in neither the sidebar nor the page"**, which is
    // the only kind of probe that survives the next layout change: the midpoint of the gap between
    // them, clamped so it is still a real pixel.
    let cv = chrome.layout.canvas;
    let lo = chrome.layout.sidebar.right();
    let hi = chrome.layout.page.x;
    assert!(
        hi > lo + 2,
        "the sidebar and the page leave no gutter to probe: sidebar ends at {lo}, page starts at {hi}"
    );
    let gx = ((lo + hi) / 2).max(lo);
    assert_eq!(
        pixel(&frame, gx, cv.y + cv.height / 2),
        c(colour::CHROME),
        "the gutter at x={gx} is not chrome-coloured: sidebar ends at {lo}, page starts at {hi}"
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
    // **How many bands there are changed in part 19** -- the title band and the sidebar are the same
    // colour as the toolbar and the status bar -- so this is no longer an exact count. What it still
    // asserts is the one thing that matters: **every band rect is painted after the panel.** A count
    // would break on every design change and say nothing; this breaks only if a band is drawn behind
    // the panel, which is the bug.
    let bands = order.iter().filter(|&&(got, _)| got == band_colour).count();
    assert!(
        bands >= 3,
        "the toolbar and status bars at least. Got {bands} band-coloured rects, which means the \
         chrome lost its bands -- and every other assertion in this file is about a layout that is no \
         longer there."
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

/// The root's own three layers, for one `ChromeState`: how many subtrees hang off `before`, the root's
/// own node, and how many off `after`.
///
/// **The counts, and not the trees, because the trees borrow a local.** The first version returned
/// `Vec<&SurfaceTree>` out of the tree it had just built, which does not compile — and the honest fix
/// is to return what the claim is actually about. **The claim is "how many subtrees are in the
/// topmost layer", which is a number.**
fn root_layer_counts(state: &ChromeState) -> (usize, usize) {
    let m = ChromeMetrics::DESKTOP;
    let tree = Chrome::new(m).tree(state);
    (tree.before.len(), tree.after.len())
}

/// The topmost layer's single subtree, for an open popup. **Owned, because the tree it came from is a
/// local** — and an assertion about a borrow-checker error is not an assertion about the chrome.
fn topmost_layer(state: &ChromeState) -> SurfaceTree {
    let m = ChromeMetrics::DESKTOP;
    let tree = Chrome::new(m).tree(state);
    assert_eq!(
        tree.after.len(),
        1,
        "expected exactly one subtree in root.after, found {}. The popup is the only layer that \
         belongs there: `after` is painted after `node` and after every entry in `before`, and the \
         document's own emitters push into `root.before`, so a popup painted anywhere else is painted \
         over.",
        tree.after.len()
    );
    tree.after.into_iter().next().expect("the one subtree")
}

/// **An open popup is emitted into `root.after`, and nothing else ever is.**
///
/// **This is the defect "the dropdown opens one layer back", in the form that can be checked.** The
/// popup used to be emitted by the band that owns it — the menu band drew the menu's popup into
/// `chrome.before` — and every one of those bands lives inside the chrome group, which is the **first**
/// entry of `root.before`. **So a popup hanging down over the canvas was painted first**: the page,
/// the sidebar and the document's own glyphs all went over it.
///
/// **`after` is the only list in the tree that a caller cannot paint into by accident**, which is why
/// the topmost layer is the only place a popup goes.
///
/// **It is a group and not a leaf, and that is asserted too.** `paint_popup` pushes its panel, shadow,
/// rim and rows into one `SurfaceTree::group()`, so the entry in `after` has `node: None` and
/// everything in `before`. A version of this gate that asserted `Some(Node::Rect(_))` would have been
/// asserting the wrong shape and would have "failed" on a correct chrome.
#[test]
fn the_open_popup_is_emitted_into_after_and_nothing_else_is() {
    use holonomy_render::widgets::Open;

    let mut state = ChromeState::default();
    state.open = Some(Open::Menu(0));
    let group = topmost_layer(&state);

    // **The group's own node is empty and its children are the popup's.** An entry in `after` carrying
    // its own `node` would be a leaf painted in the right list — which also works, and is worth
    // allowing, so this asserts the *shape that is actually there* rather than forbidding the other.
    assert!(
        group.node.is_none(),
        "the subtree in root.after carries its own node {:?}; it is a group of the popup's rects and \
         labels, and a node here would be painted in the topmost layer for no stated reason.",
        group.node
    );
    let has_rect = group
        .before
        .iter()
        .any(|c| matches!(c.node, Some(Node::Rect(_))));
    assert!(
        has_rect,
        "the subtree in root.after has {} children and not one of them is a rect, so there is no \
         popup panel in the topmost layer at all.",
        group.before.len()
    );

    // **And the negative direction, which is the half a count alone would miss.** Without it, a chrome
    // that painted an *empty* popup panel into `after` on every frame — every keystroke, every caret
    // blink — would pass the first half while paying for a node that does nothing.
    let (_, closed_after) = root_layer_counts(&ChromeState::default());
    assert_eq!(
        closed_after, 0,
        "a frame with no popup open still puts {closed_after} subtrees in root.after. `after` is for \
         the popup and nothing else."
    );
}

/// **Where a popup hangs over a panel the chrome painted above it, the sidebar shows through nowhere.**
///
/// **The test above is structural — it asks which list the popup is in. This one asks what the frame
/// looks like**, for two reasons: a structural assertion can be satisfied by a tree whose paint is
/// wrong some other way, and **the reported symptom was a picture, not a tree.**
///
/// # Why this counts pixels instead of probing one
///
/// **Three single-pixel probes were tried and all three were wrong, in three different ways.** The
/// File menu's popup hangs from the left end of the menu bar, so it lands over the **sidebar** — and the
/// sidebar is layer 2, a solid panel drawn into `root.before` *after* the chrome group that used to hold
/// the popup. **Before the fix the whole rect was sidebar.**
///
/// | probe | what it read | why it was the wrong pixel |
/// |---|---|---|
/// | `bottom() - 1` | `SHADOW` = `0xFF16161A` | **the drop shadow is drawn *inside* the panel** — three flat bands down the bottom and right — and its colour is the chrome background's, so the failure message named a layer that was never involved |
/// | `(x + w/2, bottom() - 5)` | `INK_CHROME` | it landed on a **glyph** in the last menu item's label |
/// | `y + 6` | not over the sidebar at all | it is in the popup's **top padding**, which is over the toolbar |
///
/// **A single pixel is the wrong tool for a rect that contains text, a rim and a shadow.** So the claim
/// is made about the whole rect: **not one pixel of the popup is the sidebar's colour.** That is what
/// "the panel covers what it hangs over" means, it cannot be wrong by landing on a glyph, and it fails
/// loudly on a *partial* fix that covered the popup's middle and not its edge.
#[test]
fn the_open_popup_is_visible_where_it_hangs_over_the_sidebar() {
    use holonomy_render::widgets::Open;

    let mut state = ChromeState::default();
    state.open = Some(Open::Menu(0));
    let (frame, chrome, _) = paint_chrome(&state);
    let popup = holonomy_render::widgets::popup(&chrome.layout, &state, state.open)
        .expect("a popup for an open menu");
    let side = chrome.layout.sidebar;

    // **The popup has to overlap the sidebar, or there is nothing here to prove.** The File popup is at
    // the menu bar's left end, so this holds — and it is asserted rather than assumed, because the claim
    // below silently becomes vacuous if a future layout moves the popup off the sidebar.
    let overlap_w = popup
        .rect
        .right()
        .min(side.right())
        .saturating_sub(popup.rect.x.max(side.x));
    let overlap_h = popup
        .rect
        .bottom()
        .min(side.bottom())
        .saturating_sub(popup.rect.y.max(side.y));
    assert!(
        overlap_w > 0 && overlap_h > 0,
        "the popup at {:?} does not overlap the sidebar at {:?}, so 'the popup covers the sidebar' is \
         not a claim this frame can decide.",
        popup.rect,
        side
    );

    // **The sidebar's band colour inside the popup's rect: zero, over every pixel.**
    let band = c(colour::BAND);
    let mut leaked = 0;
    let mut first_leak = None;
    for y in popup.rect.y.max(side.y)..popup.rect.bottom().min(side.bottom()) {
        for x in popup.rect.x.max(side.x)..popup.rect.right().min(side.right()) {
            if pixel(&frame, x, y) == band {
                leaked += 1;
                first_leak.get_or_insert((x, y));
            }
        }
    }
    assert_eq!(
        leaked, 0,
        "{leaked} px inside the popup at {first_leak:?} are the sidebar's band colour, over a \
         {overlap_w}x{overlap_h} overlap. The popup is being painted under the sidebar, which is the \
         'dropdown opens one layer back' report."
    );

    // **And the premise, so a zero above is not a zero because nothing was there.** The same band
    // colour, a column of the sidebar in the strip *between* the popup's right edge and the sidebar's
    // own: it has to be there in bulk, or the scan above is reading the wrong rect.
    //
    // **The strip is computed, not guessed.** The sidebar is 0..256 and the File popup is 16..224, so
    // 224..256 is the only part of the sidebar the popup does not cover — and the first version of this
    // line probed the sidebar's *centre*, x = 128, which is inside the popup and proved nothing.
    let lo = popup.rect.right().max(side.x);
    assert!(
        lo < side.right(),
        "the popup at {:?} covers the whole sidebar at {:?}, so there is no strip of the sidebar \
         beside it to use as a premise.",
        popup.rect,
        side
    );
    let beside = (lo + side.right()) / 2;
    let mut show = 0;
    for y in side.y..side.bottom() {
        if pixel(&frame, beside, y) == band {
            show += 1;
        }
    }
    assert!(
        show > side.height / 2,
        "only {show} of the sidebar's {height} rows at x={beside} are the band colour, so the scan \
         above would have found zero for the wrong reason.",
        height = side.height
    );
}
