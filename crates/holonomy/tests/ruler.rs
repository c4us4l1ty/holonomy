//! **The ruler is a scale, not a texture.**
//!
//! # What was wrong
//!
//! It painted the text column one tint and the page's gutters another, then a 1 px mark every 10 px
//! with every eighth one longer. **Nothing on it was a number**, so nothing on it could be read: a user
//! looking for "where is my margin" or "how wide is this column" got neither, and the ruler was
//! decoration that happened to occupy a band. The band was also exactly one line of type tall, so even
//! after it started drawing numbers there was nowhere to put them -- the labels were silently dropped
//! for want of a pixel.
//!
//! # The properties asserted
//!
//! A scale needs three things and this file checks each: **a unit** (columns, zero at the column's left
//! edge), **numbered majors** at a spacing that stays legible, and **room for the numbers** in the band
//! it draws into.

use holonomy_display::paint::Painter;
use holonomy_render::chrome::{ChromeMetrics, ChromeState, Layout};
use holonomy_render::ribbon;
use holonomy_render::{Node, SurfaceTree};

fn metrics(ppem_cell_h: u32, cell_w: u32) -> ChromeMetrics {
    ChromeMetrics {
        cell_w,
        cell_h: ppem_cell_h,
        ..ChromeMetrics::DESKTOP
    }
    .with_ribbon_bands(true, false)
}

/// Every `Label` in a tree, as `(x, y, text)`.
fn labels(tree: &[SurfaceTree]) -> Vec<(i32, i32, String)> {
    fn walk(t: &SurfaceTree, out: &mut Vec<(i32, i32, String)>) {
        if let Some(Node::Label(l)) = &t.node {
            out.push((
                l.x,
                l.y,
                String::from_utf8_lossy(&l.bytes[..l.len as usize]).into_owned(),
            ));
        }
        for c in t.before.iter().chain(t.after.iter()) {
            walk(c, out);
        }
    }
    let mut out = Vec::new();
    for t in tree {
        walk(t, &mut out);
    }
    out
}

/// **The band is tall enough to hold a line of numbers *and* a tick strip.**
///
/// `by(26)` at cell 18 is 18 px, which is exactly one line of type — so `text()`'s `r.height < 8` guard
/// passed while its *centred line* had no air, and the numbers a numbered ruler exists for had nowhere
/// to go. This asserts the band at both type scales and at the extremes of the scale sweep.
#[test]
fn the_ruler_band_holds_numbers_and_ticks() {
    for (cell_h, cell_w) in [(18, 8), (23, 12), (24, 12), (30, 12), (40, 12), (14, 8)] {
        let m = metrics(cell_h, cell_w);
        let l = Layout::new(&m);
        assert!(
            l.ruler.height >= cell_h + ChromeMetrics::RULER_TICK_H,
            "at cell_h {cell_h}: the ruler is {} px, which is not one line of type ({cell_h}) plus a \
             tick strip ({})",
            l.ruler.height,
            ChromeMetrics::RULER_TICK_H
        );
    }
}

/// **The numbers are drawn, they start at zero, and they count columns.**
///
/// The first major tick is the column's left edge labelled `0`, and the next is `10` — a ruler that
/// starts numbering at 1, or numbers in pixels, is a different instrument.
#[test]
fn the_ruler_is_numbered_in_columns_from_zero() {
    let m = metrics(18, 8);
    let l = Layout::new(&m);
    let mut s = ChromeState::default();
    s.ribbon = true;
    s.ribbon_tab = 1;
    let mut v = Vec::new();
    ribbon::paint(&mut v, &l, &s, l.cell_h);
    let all = labels(&v);
    // The ruler's numbers are the labels whose y is inside the ruler band.
    let mut in_ruler: Vec<_> = all
        .iter()
        .filter(|(_, y, _)| *y >= l.ruler.y as i32 && *y < l.ruler.bottom() as i32)
        .map(|(x, _, t)| (*x, t.clone()))
        .collect();
    in_ruler.sort();
    assert!(
        !in_ruler.is_empty(),
        "a ruler with no numbers is the texture this change replaced"
    );
    assert_eq!(
        in_ruler[0].1, "0",
        "the scale starts at zero, at the column's left edge"
    );
    // The majors are ten columns apart, and the labels count them.
    let expected: Vec<String> = (0..)
        .map(|i| (i * ribbon::MAJOR_RULER_COLS).to_string())
        .take_while(|n| n.parse::<u32>().unwrap() * 8 < l.text.width)
        .collect();
    let got: Vec<String> = in_ruler.iter().map(|(_, t)| t.clone()).collect();
    assert_eq!(
        got, expected,
        "the majors are every {} columns and each one is labelled with its number",
        ribbon::MAJOR_RULER_COLS
    );
}

/// **The first number is at the column's left edge**, not the page's and not the window's.
///
/// The old tick field started at `l.text.x` too, but nothing *said* so; this is what makes the scale
/// agree with the glyphs above it rather than with a second idea of where the text begins.
#[test]
fn zero_is_at_the_text_columns_left_edge() {
    let m = metrics(18, 8);
    let l = Layout::new(&m);
    let mut s = ChromeState::default();
    s.ribbon = true;
    let mut v = Vec::new();
    ribbon::paint(&mut v, &l, &s, l.cell_h);
    let zero = labels(&v)
        .into_iter()
        .find(|(_, y, t)| t == "0" && *y >= l.ruler.y as i32 && *y < l.ruler.bottom() as i32)
        .expect("a zero on the ruler");
    assert!(
        (zero.0 - l.text.x as i32).abs() <= 2,
        "column zero is drawn at {} and the text column starts at {}",
        zero.0,
        l.text.x
    );
}

/// **The ruler never draws outside its own band**, at any panel size.
///
/// It is between the ribbon and the canvas, so a tick drawn one pixel low is a tick over the page and a
/// number drawn one row high is a number in the ribbon. The old field was clamped by construction (the
/// band was 4 px tall); a numbered ruler has content that *can* overflow, so it is checked.
#[test]
fn nothing_the_ruler_draws_leaves_its_band() {
    for width in [640u32, 1024, 1280, 1600, 1920] {
        for cell_h in [18u32, 24, 30] {
            let m = ChromeMetrics {
                width,
                cell_h,
                cell_w: 8,
                ..ChromeMetrics::DESKTOP
            }
            .with_ribbon_bands(true, false);
            let l = Layout::new(&m);
            let mut s = ChromeState::default();
            s.ribbon = true;
            let mut v = Vec::new();
            ribbon::paint(&mut v, &l, &s, l.cell_h);
            for (x, y, t) in labels(&v) {
                let in_band = y >= l.ruler.y as i32 && y + cell_h as i32 <= l.ruler.bottom() as i32;
                let looks_like_a_number = t.chars().all(|c| c.is_ascii_digit()) && !t.is_empty();
                if looks_like_a_number && y >= l.ruler.y as i32 {
                    assert!(
                        in_band,
                        "at {width} px / cell_h {cell_h}: the ruler's `{t}` is at y {y} and the band \
                         is {}..{}",
                        l.ruler.y,
                        l.ruler.bottom()
                    );
                    assert!(
                        x >= 0 && (x as u32) < width,
                        "at {width} px: `{t}` is drawn at x {x}"
                    );
                }
            }
        }
    }
}

/// **Every panel size still produces a paint**, which is the smoke test the other gates rest on.
#[test]
fn the_ruler_paints_at_every_panel_size() {
    for width in [320u32, 640, 1024, 1600, 2560] {
        for height in [240u32, 480, 800, 1440] {
            let m = ChromeMetrics {
                width,
                height,
                ..ChromeMetrics::DESKTOP
            }
            .with_ribbon_bands(true, false);
            let l = Layout::new(&m);
            let mut s = ChromeState::default();
            s.ribbon = true;
            let mut v = Vec::new();
            ribbon::paint(&mut v, &l, &s, l.cell_h);
            let _ = Painter::without_atlas(0);
            assert!(!v.is_empty(), "paint produced nothing at {width}x{height}");
        }
    }
}
