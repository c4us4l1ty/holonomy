//! Painting the chrome into a frame, and the damage property that makes a blink cheap.
//!
//! # Why this needs a real atlas
//!
//! A painter tested against a stub atlas proves nothing about whether a glyph lands in the right
//! pixels. `holonomy_assets::payload::build_atlas` is the public path -- decompress the packed
//! faces, rasterise every codepoint at the requested sizes, add the procedural box-drawing table --
//! and it is the same call the session will make, so the fixture and the product cannot diverge.
//!
//! # What the damage assertions are for
//!
//! FR-3.4: a keystroke invalidates one line's box and a caret blink invalidates one cell. The blink
//! half is the one that is easy to get wrong and invisible when you do -- a frame that redraws
//! 1280 x 800 twice a second is still *correct*, it just flickers and wastes the frame budget. So
//! `painting_a_small_damage_rect_writes_fewer_pixels` and its siblings compare pixel counts and
//! skipped-node counts, not just the picture.

use holonomy_display::paint::Painter;
use holonomy_display::{Frame, FrameError, Scanout};
use holonomy_render::chrome::{Blink, Caret, Chrome, ChromeMetrics, ChromeState};
use holonomy_render::DamageRect;

/// The whole atlas at one size, which is what a session uses.
fn atlas() -> holonomy_assets::atlas::Atlas {
    holonomy_assets::build_atlas(&[16])
        .expect("build the atlas")
        .0
}

/// A frame the size of the chrome.
fn frame() -> Frame {
    Frame::black(ChromeMetrics::DESKTOP.width, ChromeMetrics::DESKTOP.height)
}

#[test]
fn the_atlas_builds_and_covers_the_chromes_glyphs() {
    let a = atlas();
    assert!(!a.coverage().is_empty(), "the atlas has no coverage at all");
    // Every codepoint the chrome draws: the letters of the labels and the separators.
    for cp in [
        'C' as u32, 'h' as u32, 'B' as u32, 'o' as u32, 'l' as u32, 'd' as u32, '[' as u32,
        ']' as u32, 'L' as u32, 'n' as u32, '1' as u32, '0' as u32, 0x2500, 0x2502, 0x250C, 0x256D,
    ] {
        let m = a.metric(cp, holonomy_assets::payload::Style::Monospace, 16);
        assert!(!m.is_blank(), "U+{cp:04X} has no glyph in the atlas");
    }
    // A space is the one codepoint that is *supposed* to be blank: a glyph with no ink and a real
    // advance. Asserted, because reading "blank" as "missing" would make every space in the chrome a
    // reported failure -- and then the count would be ignored, which is worse.
    assert!(
        a.metric(' ' as u32, holonomy_assets::payload::Style::Monospace, 16)
            .is_blank(),
        "a space must be blank"
    );
}

#[test]
fn the_chrome_paints_without_missing_a_single_glyph() {
    let a = atlas();
    let c = Chrome::new(ChromeMetrics::DESKTOP);
    let state = ChromeState {
        title: "quarterly.wavefunction".to_string(),
        sealed: true,
        words: 412,
        bytes: 2458,
        zoom_percent: 100,
        ..ChromeState::default()
    };
    let mut f = frame();
    let stats = Painter::new(&a, 0)
        .paint(&mut f, &c.tree(&state), None)
        .expect("paint");

    assert_eq!(stats.missing, 0, "a chrome label with no glyph: {stats:?}");
    assert!(
        stats.rects > 10,
        "the bands and the page should be filled: {stats:?}"
    );
    assert!(
        stats.box_glyphs > 4,
        "the separators should be drawn: {stats:?}"
    );
    assert!(stats.glyphs > 20, "the labels should be drawn: {stats:?}");
    assert!(stats.pixels > 0);
}

#[test]
fn the_page_is_the_lightest_region_and_the_bands_are_not() {
    // A structural check on the frame rather than on the tree: the page must actually be painted
    // white, or "the page canvas" is a claim about a `Rect` that never reached a pixel.
    let a = atlas();
    let m = ChromeMetrics::DESKTOP;
    let c = Chrome::new(m);
    let mut f = frame();
    Painter::new(&a, 0)
        .paint(&mut f, &c.tree(&ChromeState::default()), None)
        .expect("paint");

    // A point in the middle of the text column.
    let (px, py) = (
        c.layout.text.x + c.layout.text.width / 2,
        c.layout.text.y + 20,
    );
    let inside = f.pixel(px, py);
    let outside = f.pixel(c.layout.gutter_left / 2, c.layout.text.y + 20);
    assert_ne!(
        inside, outside,
        "the page interior and the gutter are the same colour at ({px},{py})"
    );
    // The page is light and the gutter is not.
    let lum = |c: u32| ((c >> 16) & 0xFF) + ((c >> 8) & 0xFF) + (c & 0xFF);
    assert!(
        lum(inside) > lum(outside),
        "expected the page lighter than the chrome: {inside:#x} vs {outside:#x}"
    );
}

#[test]
fn painting_the_same_tree_twice_gives_the_same_frame() {
    // The visual baseline rests on this: no time, no address, no iteration order in the output.
    let a = atlas();
    let c = Chrome::new(ChromeMetrics::DESKTOP);
    let state = ChromeState {
        title: "stable".to_string(),
        sealed: true,
        ..ChromeState::default()
    };
    let tree = c.tree(&state);

    let mut one = frame();
    let mut two = frame();
    Painter::new(&a, 0)
        .paint(&mut one, &tree, None)
        .expect("paint");
    Painter::new(&a, 0)
        .paint(&mut two, &tree, None)
        .expect("paint");

    let d = one.diff(&two).expect("same size");
    assert!(
        d.is_empty(),
        "two paints of one tree differ at {:?}",
        d.first
    );
}

#[test]
fn the_ppm_dump_is_byte_identical_across_runs() {
    // What the gate actually asserts, one level up: the file, not just the pixels.
    let a = atlas();
    let c = Chrome::new(ChromeMetrics::DESKTOP);
    let mut f = frame();
    Painter::new(&a, 0)
        .paint(&mut f, &c.tree(&ChromeState::default()), None)
        .expect("paint");

    let mut one = Vec::new();
    let mut two = Vec::new();
    f.to_ppm(&mut one).expect("dump");
    f.to_ppm(&mut two).expect("dump");
    assert_eq!(one, two);
    assert!(one.starts_with(b"P6\n1280 800\n255\n"));
}

#[test]
fn painting_a_small_damage_rect_writes_fewer_pixels() {
    // The property that makes a caret blink one cell instead of a frame.
    let a = atlas();
    let m = ChromeMetrics::DESKTOP;
    let c = Chrome::new(m);
    let state = ChromeState {
        title: "damage".to_string(),
        sealed: true,
        caret_line: 6,
        caret_column: 33,
        ..ChromeState::default()
    };
    let caret = Caret::locate(&c.layout, &m, &state)
        .expect("on screen")
        .cell;
    let tree = c.tree(&state);

    let mut whole = frame();
    let mut narrow = frame();
    let full_stats = Painter::new(&a, 0)
        .paint(&mut whole, &tree, None)
        .expect("paint");
    let cell_stats = Painter::new(&a, 0)
        .paint(&mut narrow, &tree, Some(caret))
        .expect("paint");

    assert!(
        cell_stats.pixels * 100 < full_stats.pixels,
        "painting one cell wrote {} of {} pixels",
        cell_stats.pixels,
        full_stats.pixels
    );
    assert!(
        cell_stats.glyphs_skipped > 0,
        "most glyphs should have been skipped entirely"
    );
    assert!(
        cell_stats.rects_skipped > 0,
        "the bands should not have been repainted for a caret"
    );
    // And the frame it produced is still the full frame: nothing was clipped away.
    assert_eq!(narrow.pixels().len(), whole.pixels().len());
}

#[test]
fn a_damage_rect_outside_everything_paints_nothing() {
    let a = atlas();
    let c = Chrome::new(ChromeMetrics::DESKTOP);
    let mut f = frame();
    let stats = Painter::new(&a, 0)
        .paint(
            &mut f,
            &c.tree(&ChromeState::default()),
            // Entirely off the panel. A damage rect *on* the panel would legitimately intersect the
            // panel background and the status band and so would draw -- which is correct, not a bug:
            // those pixels are damaged and need repainting.
            Some(DamageRect::new(4000, 4000, 8, 4)),
        )
        .expect("paint");
    assert_eq!(stats.pixels, 0, "nothing should have been drawn: {stats:?}");
    assert_eq!(stats.rects, 0);
    assert_eq!(stats.glyphs, 0);
    assert_eq!(stats.box_glyphs, 0);
}

/// **An image straddling a damage edge paints only its damaged half — on *both* axes.**
///
/// `blit_rgba` clipped columns to the (already intersected) damage rect from the start but never
/// clipped rows, so an image whose rect crossed the *top* or *bottom* edge of the damage painted
/// every row it had. Two failures in one: pixels were written where the caller had not asked for
/// paint — a correctness bug in a damage-driven renderer — and the whole point of
/// `paint(..., Some(damage))` (which this file's other tests measure) is that undamaged work is not
/// done at all, so a caret blink over a page with a picture repainted the picture's every row.
///
/// The horizontal case is asserted alongside it, not instead of it: the two clips live in one
/// function, and a fix that added the vertical clip by rewriting the loop could easily have dropped
/// the horizontal one. The `image_pixels` count is the assertion for both halves at once, because a
/// double-write or a missed half move it.
#[test]
fn an_image_straddling_a_damage_edge_paints_only_its_damaged_half() {
    /// One opaque white 8 x 8 raster, which is what an admitted RGBA image looks like before it is
    /// laid out. Opaque so the blend is the identity and the frame's own colour never appears.
    ///
    /// Owned rather than `const`: a `const` array is built by const-evaluated slice indexing, which
    /// is not stable, and the raster has to be a field of a `struct` the trait impl can borrow.
    struct One {
        px: Vec<u8>,
    }
    impl One {
        fn white() -> Self {
            let mut px = vec![0xFFu8; 8 * 8 * 4];
            for p in px.chunks_exact_mut(4) {
                p.copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
            }
            Self { px }
        }
    }
    impl holonomy_render::RasterSource for One {
        fn raster(
            &self,
            _asset_id: holonomy_render::AssetId,
        ) -> Option<holonomy_render::Raster<'_>> {
            Some(holonomy_render::Raster {
                width: 8,
                height: 8,
                pixels: &self.px,
            })
        }
    }
    let rasters = One::white();

    // A 32 x 32 frame with one 8 x 8 image at (8, 8), so its rows are 8..16 and its columns 8..16.
    let id = holonomy_render::AssetId::of(b"one image");
    let tree = holonomy_render::SurfaceTree::leaf(holonomy_render::Node::Image {
        rect: holonomy_render::Rect::new(8, 8, 8, 8, 0),
        asset_id: id,
    });

    // Damage is the image's top half: rows 8..12, all eight columns. `image()` intersects the rect
    // with the damage before blitting, so `dst` is 8 x 4 while the raster is 8 x 8 — precisely the
    // case the row loop used to ignore.
    let mut half = Frame::black(32, 32);
    let half_stats = Painter::new(&atlas(), 0)
        .paint_with_rasters(&mut half, &tree, Some(DamageRect::new(8, 8, 8, 4)), Some(&rasters))
        .expect("paint the top half");
    assert_eq!(
        half_stats.image_pixels, 32,
        "only the four damaged rows may be painted: {half_stats:?}"
    );
    for y in 8..12 {
        assert!(
            half.row(y)[8..16].iter().all(|&p| p == 0xFFFF_FFFF),
            "damaged row {y} is painted: {:x?}",
            &half.row(y)[8..16]
        );
    }
    for y in 12..16 {
        assert!(
            half.row(y).iter().all(|&p| p == 0),
            "undamaged row {y} must be untouched, but it holds {:x?}",
            half.row(y)
        );
    }

    // The mirror: damage is the image's left half, which is the axis that already worked. Asserted
    // because the two clips are one function, and this is what fails if a rewrite drops one.
    let mut side = Frame::black(32, 32);
    let side_stats = Painter::new(&atlas(), 0)
        .paint_with_rasters(&mut side, &tree, Some(DamageRect::new(8, 8, 4, 8)), Some(&rasters))
        .expect("paint the left half");
    assert_eq!(
        side_stats.image_pixels, 32,
        "only the four damaged columns may be painted: {side_stats:?}"
    );
    assert!(
        side.row(9)[8..12].iter().all(|&p| p == 0xFFFF_FFFF),
        "the damaged columns are painted"
    );
    assert!(
        side.row(9)[12..16].iter().all(|&p| p == 0),
        "the undamaged columns are untouched: {:x?}",
        &side.row(9)[12..16]
    );

    // And a full-damage paint is the whole 64 pixels, so the clip is not over-eager: an image fully
    // inside the damage still arrives intact.
    let mut whole = Frame::black(32, 32);
    let whole_stats = Painter::new(&atlas(), 0)
        .paint_with_rasters(&mut whole, &tree, Some(DamageRect::new(8, 8, 8, 8)), Some(&rasters))
        .expect("paint the whole image");
    assert_eq!(whole_stats.image_pixels, 64, "the whole image lands: {whole_stats:?}");
    for y in 8..16 {
        assert!(
            whole.row(y)[8..16].iter().all(|&p| p == 0xFFFF_FFFF),
            "row {y} is fully painted"
        );
    }
}

#[test]
fn the_caret_is_actually_drawn_into_the_frame() {
    // The chrome's caret node is a filled rect; if the painter skipped it the frame would look fine
    // and have no caret, which is the worst possible failure for an editor.
    let a = atlas();
    let m = ChromeMetrics::DESKTOP;
    let c = Chrome::new(m);
    let state = ChromeState {
        caret_line: 4,
        caret_column: 20,
        caret_visible: true,
        ..ChromeState::default()
    };
    let caret = Caret::locate(&c.layout, &m, &state)
        .expect("on screen")
        .cell;

    let with = {
        let mut f = frame();
        Painter::new(&a, 0)
            .paint(&mut f, &c.tree(&state), None)
            .expect("paint");
        f
    };
    let without = {
        let mut f = frame();
        let hidden = ChromeState {
            caret_visible: false,
            ..state.clone()
        };
        Painter::new(&a, 0)
            .paint(&mut f, &c.tree(&hidden), None)
            .expect("paint");
        f
    };

    let d = with.diff(&without).expect("same size");
    assert!(
        !d.is_empty(),
        "toggling the caret changed nothing, so the caret is not being drawn"
    );
    // And the difference is the thin insertion caret: 2 px wide, one cell tall.
    let only = DamageRect::new(
        d.first.expect("a difference").0,
        d.first.expect("a difference").1,
        0,
        0,
    );
    let _ = only;
    assert_eq!(
        d.differing,
        2 * u64::from(caret.height)
    );
}

#[test]
fn a_blink_cycle_repaints_exactly_the_caret_cell_each_time() {
    // The whole point, end to end: over a second of frames, the caret's cell is repainted twice and
    // the rest of the frame is not touched at all.
    let a = atlas();
    let m = ChromeMetrics::DESKTOP;
    let c = Chrome::new(m);
    let base = ChromeState {
        title: "blink".to_string(),
        sealed: true,
        caret_line: 3,
        caret_column: 12,
        caret_visible: true,
        ..ChromeState::default()
    };
    let caret = Caret::locate(&c.layout, &m, &base)
        .expect("the caret is on screen")
        .cell;
    let cell_px = u64::from(caret.width) * u64::from(caret.height);
    let full_frame = u64::from(m.width) * u64::from(m.height);

    let mut blink = Blink::new(Blink::DEFAULT_PERIOD);
    let mut total_pixels = 0u64;
    let mut frames = 0u32;
    let mut repaints = 0u32;
    while frames < 60 {
        // The loop's actual shape: **a frame with no damage is not painted at all.** Painting it and
        // discarding the result is not what a session does, and counting those pixels would measure
        // this test rather than the renderer.
        let Some(damage) = blink.advance(Some(caret)) else {
            frames += 1;
            continue;
        };
        let mut f = frame();
        let stats = Painter::new(&a, 0)
            .paint(
                &mut f,
                &c.tree(&ChromeState {
                    caret_visible: blink.visible(),
                    ..base.clone()
                }),
                Some(damage),
            )
            .expect("paint");
        total_pixels += stats.pixels;
        repaints += 1;
        // A repaint draws every node that *intersects* the cell, clipped to it: the page, its border,
        // any nearby glyph run, and the caret itself. The bound is a small multiple of the caret cell
        // rather than the cell exactly -- what matters is that it is nowhere near a frame.
        assert!(
            stats.pixels <= cell_px * 8,
            "a repaint wrote {} pixels for a {cell_px}-px cell",
            stats.pixels
        );
        frames += 1;
    }
    assert_eq!(
        repaints, 2,
        "one second at a 30-frame period flips twice: visible -> hidden and hidden -> visible"
    );
    assert!(
        total_pixels <= cell_px * 8,
        "a second of blinking cost {total_pixels} pixels; one full repaint is {full_frame}"
    );
}

#[test]
fn a_frame_of_the_wrong_size_is_refused_by_the_scanout_not_the_painter() {
    // The painter writes into whatever frame it is given; the *scanout* is what enforces the size.
    // Both halves, so the boundary is where it is claimed to be.
    let a = atlas();
    let c = Chrome::new(ChromeMetrics::DESKTOP);
    let tree = c.tree(&ChromeState::default());

    let mut small = Frame::black(64, 64);
    // Must not panic, must not panic, must not panic: a small frame is a legitimate target.
    let stats = Painter::new(&a, 0)
        .paint(&mut small, &tree, None)
        .expect("painting into a small frame does not fail");
    assert_eq!(small.pixels().len(), 64 * 64);

    let mut scanout = holonomy_display::HeadlessScanout::new(64, 64);
    assert_eq!(
        scanout.present(&frame()).err(),
        Some(FrameError::SizeMismatch {
            want: (64, 64),
            got: (ChromeMetrics::DESKTOP.width, ChromeMetrics::DESKTOP.height)
        })
    );
    let _ = stats;
}

#[test]
fn a_painter_without_an_atlas_still_draws_the_chrome_and_counts_the_text() {
    // Legitimate for a chrome-only frame, and the honest behaviour: rules and bands draw, characters
    // do not, and `missing` says so rather than leaving gaps that look like spaces.
    let c = Chrome::new(ChromeMetrics::DESKTOP);
    let mut f = frame();
    let stats = Painter::without_atlas(0)
        .paint(&mut f, &c.tree(&ChromeState::default()), None)
        .expect("paint");
    assert!(stats.missing > 0, "without an atlas the labels cannot draw");
    assert!(stats.rects > 10, "but the bands and the page still draw");
    assert!(stats.pixels > 0);
}

#[test]
fn the_separators_are_drawn_from_the_table_and_not_from_the_font() {
    // A box-drawing run draws procedurally, so it works with no atlas at all. If the painter ever
    // routes separators through the atlas this fails, which is the point: the chrome's rules must not
    // depend on a font's box glyphs being present at the right weight.
    let c = Chrome::new(ChromeMetrics::DESKTOP);
    let mut f = frame();
    let stats = Painter::without_atlas(0)
        .paint(&mut f, &c.tree(&ChromeState::default()), None)
        .expect("paint");
    assert!(
        stats.box_glyphs > 4,
        "the rules should draw with no atlas: {stats:?}"
    );
}


#[test]
fn a_round_rect_fills_its_centre_and_leaves_its_corners() {
    use holonomy_render::{Node, RoundRect, SurfaceTree};
    let mut f = frame();
    let tree = SurfaceTree::leaf(Node::RoundRect(RoundRect::new(0, 0, 32, 32, 8, 0xFFFF_FFFF)));
    Painter::without_atlas(0).paint(&mut f, &tree, None).expect("paint");
    assert_eq!(f.pixel(16, 16), 0x00FF_FFFF, "the centre is filled");
    assert_eq!(f.pixel(16, 0), 0x00FF_FFFF, "the top centre is filled");
    assert_eq!(f.pixel(0, 0), 0x0000_0000, "the rounded corner is not");
    assert_eq!(f.pixel(31, 31), 0x0000_0000, "and neither is the opposite corner");
}
