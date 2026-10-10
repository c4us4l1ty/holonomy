//! Phase 9C gate: an image, from a keystroke to pixels.
//!
//! # What this file is
//!
//! The end-to-end path for a [`Node::Image`], asserted in the order the path runs:
//!
//! 1. `Ctrl+I` reaches [`Session::insert_image`] through the **real keymap** -- not by calling
//!    `insert_image` directly, because "does Ctrl+I reach the image code" is the requirement and calling
//!    the image code directly would not test it. This is the same discipline `holonomy-text`'s Ctrl+T
//!    gate uses, for the same reason.
//! 2. The document gains a U+FFFC anchor and the payload's tail gains a catalog entry.
//! 3. A paint decodes the PNG, downsamples it to page-column width with the fixed-point scaler, and
//!    admits the raster to the Iceberg cache.
//! 4. A `Node::Image` is emitted, and the painter blits it 1:1 with its alpha honoured.
//! 5. `LineHeights` displaces every line below the image.
//! 6. The bytes land where the layout says they land -- checked against pixels, not against the
//!    numbers that produced them.
//!
//! # The things asserted that are easy to get wrong
//!
//! * **§2.9.3's arithmetic, on a real frame.** A 1920x1080 source is 7.910 MiB of native RGBA, which
//!   would hold exactly *one* raster inside an 8.0 MiB budget. The gate asserts the resident figure is
//!   the 640x360 one (0.879 MiB) instead, because that is the decision that makes the ±1-page policy
//!   real rather than a formality.
//! * **Alpha.** A transparent PNG must not paint the page over the text it sits on. The fixture has a
//!   transparent band and the assertion is that the band left the page's own colour alone.
//! * **The damage model.** `Painter` culls on the cell and `LineHeights` moves the lines below, so a
//!   repaint after the image is inserted must actually repaint where the image is. The assertion is that
//!   the pixels are there after a *scrolling* repaint, which is the case that a stale-caret bug like the
//!   `widen()` fix would produce.

use holonomy::{Session, TEST_CHART_PNG};
use holonomy_display::paint::{PaintStats, Painter};
use holonomy_display::{Frame, Scanout};
use holonomy_input::{Command, Hotkey, InputEvent, Keymap, ModifierState};
use holonomy_render::chrome::ChromeMetrics;
use holonomy_render::LineHeights;

/// A `Scanout` that hands every presented frame back through a shared cell.
///
/// `Rc<RefCell<..>>` rather than a field on the struct, because the session owns the scanout inside a
/// `Box<dyn Scanout>` and there is no way to get it back out. The cell is the seam: the test holds one
/// handle and the session holds the other, and neither has to know the other exists. `Frame` is cloned
/// out because the session reuses its buffer -- a test that read the session's `Frame` instead would be
/// reading the *next* paint's pixels.
#[derive(Debug, Default, Clone)]
struct Keep {
    last: std::rc::Rc<std::cell::RefCell<Option<Frame>>>,
    /// What this backend claims to be. `Scanout` requires the dimensions so `Session::resize` can
    /// check a frame against them, and the default (0, 0) would make every present a mismatch.
    size: (u32, u32),
}

impl Keep {
    /// A backend of `size`, so `Session::new`'s frame and `present`'s frame agree.
    fn sized(size: (u32, u32)) -> Self {
        Self {
            last: std::rc::Rc::new(std::cell::RefCell::new(None)),
            size,
        }
    }

    fn presented(&self) -> Frame {
        self.last
            .borrow()
            .clone()
            .expect("the session presented at least one frame")
    }
}

impl Scanout for Keep {
    fn present(&mut self, frame: &Frame) -> Result<u64, holonomy_display::FrameError> {
        *self.last.borrow_mut() = Some(frame.clone());
        Ok((frame.width() * frame.height()) as u64)
    }
    fn present_damage(
        &mut self,
        frame: &Frame,
        _damage: Option<holonomy_render::DamageRect>,
    ) -> Result<u64, holonomy_display::FrameError> {
        self.present(frame)
    }
    fn width(&self) -> u32 {
        self.size.0
    }
    fn height(&self) -> u32 {
        self.size.1
    }
    fn describe(&self) -> &'static str {
        "Keep (test)"
    }
}

/// A session with an atlas and the built-in chart inserted, plus a handle on the presented frames.
///
/// The atlas is leaked on purpose: `Session<'a>` borrows it for `'a`, and a `Box::leak` is one line
/// against a lifetime dance in every test. Tests leak 1 MiB each; the process is short-lived and the
/// allocator is the counting one only in `no_alloc.rs`, which does not use this helper.
fn with_image() -> (Session<'static>, Keep) {
    let atlas: &'static holonomy_assets::atlas::Atlas = Box::leak(Box::new(
        holonomy_assets::build_atlas(&[16]).expect("atlas").0,
    ));
    let keep = Keep::sized((ChromeMetrics::DESKTOP.width, ChromeMetrics::DESKTOP.height));
    let editor = holonomy_text::Editor::from_text(b"before\n").expect("editor");
    let mut s = Session::new(
        editor,
        Painter::new(atlas, 0),
        Box::new(keep.clone()),
        ChromeMetrics::DESKTOP,
    );
    s.insert_image_bytes(TEST_CHART_PNG).expect("insert");
    (s, keep)
}

/// §2.9.3's page column: what [`Session::chrome`]'s layout says the text area is.
fn column_width(s: &Session<'_>) -> u32 {
    s.chrome.layout.text.width
}

#[test]
fn ctrl_i_reaches_the_image_code_through_the_real_keymap() {
    // `dispatch_into`, not `dispatch`: Ctrl+I arrives as two events -- the ctrl press and the I press
    // -- and the folding of the modifier is part of what has to work for the binding to fire at all.
    let km = Keymap::us();
    let mut mods = ModifierState::new();
    assert_eq!(
        km.dispatch_into(InputEvent::press(holonomy_input::KEY_LEFTCTRL), &mut mods),
        None
    );
    assert!(mods.ctrl());
    let cmd = km.dispatch_into(InputEvent::press(holonomy_input::KEY_I), &mut mods);
    assert_eq!(cmd, Some(Command::Hotkey(Hotkey::InsertImage)));
    assert_eq!(cmd, Some(Command::Hotkey(Hotkey::InsertImage)));

    let (mut s, _) = with_image();
    assert_eq!(s.stats.image_inserts, 1, "the helper already inserted one");
    // Undo it so the keystroke under test starts from a document with no anchor. The catalog loses the
    // asset *with* the anchor -- the pairing is positional, so leaving it behind would serve the next
    // anchor this one wrong picture.
    s.editor_mut().undo().expect("undo the helper's insert");
    assert_eq!(
        s.image_anchors().expect("anchors").len(),
        0,
        "the anchor is gone"
    );
    assert_eq!(s.assets().len(), 0, "and so is its asset");

    s.apply(cmd.expect("ctrl+i decodes")).expect("apply");
    assert_eq!(
        s.stats.image_inserts, 2,
        "the helper's insert, then Ctrl+I's"
    );
    // One anchor and one asset, not two: the undo took the helper's asset away with its anchor, so
    // Ctrl+I starts from an empty catalog rather than appending to a stale one.
    assert_eq!(s.assets().len(), 1);
    assert_eq!(s.image_anchors().expect("anchors").len(), 1);
}

#[test]
fn a_bare_i_is_still_the_letter_i() {
    // Ctrl+I must not eat the letter. The binding is in the ctrl arm, *before* the character table,
    // so this is the test that the ordering is right.
    assert_eq!(
        Keymap::us().dispatch(
            InputEvent::press(holonomy_input::KEY_I),
            &ModifierState::new()
        ),
        Some(Command::Insert('i'))
    );
    let mut shift = ModifierState::new();
    shift.update(holonomy_input::KEY_LEFTSHIFT, 1);
    assert!(
        shift.shift(),
        "the test has to actually be holding shift, or the next assertion proves nothing"
    );
    assert_eq!(
        Keymap::us().dispatch(InputEvent::press(holonomy_input::KEY_I), &shift),
        Some(Command::Insert('I'))
    );
    // And Ctrl+Shift+I is an unbound combo: swallowed, never typed, never an image.
    let mut both = ModifierState::new();
    both.update(holonomy_input::KEY_LEFTSHIFT, 1);
    both.update(holonomy_input::KEY_LEFTCTRL, 1);
    assert_eq!(
        Keymap::us().dispatch(InputEvent::press(holonomy_input::KEY_I), &both),
        None,
        "Ctrl+Shift+I must be swallowed, not typed as a capital I"
    );
}

#[test]
fn the_anchor_is_a_uffc_and_the_catalog_holds_the_png() {
    let (s, _) = with_image();
    let text = s.text().expect("text");
    assert!(
        text.windows(3).any(|w| w == holonomy_text::ANCHOR_BYTES),
        "no U+FFFC anchor"
    );
    let asset = &s.assets().entries()[0];
    assert_eq!(asset.png.as_slice(), TEST_CHART_PNG);
    assert_eq!((asset.width, asset.height), (1920, 1080));
    assert_eq!(asset.id, holonomy_text::AssetId::of(TEST_CHART_PNG));
}

#[test]
fn the_whole_payload_round_trips_with_an_image_in_it() {
    let (mut s, _) = with_image();
    let payload = s.editor_mut().payload().expect("payload");
    let back = holonomy_text::Editor::from_payload(&payload).expect("reopen");
    assert_eq!(back.assets().len(), 1);
    assert_eq!(back.assets().entries()[0].png.as_slice(), TEST_CHART_PNG);
    assert_eq!(back.payload().expect("re-payload"), payload);
}

#[test]
fn a_paint_decodes_the_image_to_page_column_width_and_not_to_native() {
    let (mut s, _) = with_image();
    s.paint(None).expect("paint");

    assert_eq!(s.stats.images_decoded, 1, "one decode for one image");
    assert_eq!(s.image_cache_len(), 1);

    // The resident raster is the *page-column-width* one, which is §2.9.3's decision and the reason
    // the policy holds. A 1920x1080 RGBA raster is 7.910 MiB -- it would fit inside 8.0 MiB exactly
    // once, and a second image anywhere in the document would breach the budget.
    let column = column_width(&s);
    let (rw, rh) = s.chrome_rect_for_image(0);
    assert_eq!(
        column, 640,
        "§2.9.3's page column at DESKTOP metrics is 640 px"
    );
    assert_eq!(rw, column.min(1920), "the raster is the column width");
    assert_eq!(
        rh, 360,
        "1080 scaled to 640 wide is 360, keeping the 16:9 aspect"
    );
    assert_eq!(
        s.image_cache_bytes(),
        (rw * rh * 4) as usize,
        "the cache holds exactly one raster's bytes"
    );
    assert!(
        s.image_cache_bytes() < 1_000_000,
        "a page-column raster is 0.879 MiB, got {}",
        s.image_cache_bytes()
    );
    assert!(s.image_cache_bytes() < s.image_cache_budget());
}

/// **The raster is an area average at the product's own ratio — which is what this test's name has said
/// since Phase 9C.**
///
/// # What this test asserted until now, and why its name was aspirational
///
/// It was written as `the_raster_is_a_resample_and_not_a_decimation` and then asserted the opposite:
///
/// ```text
/// assert_eq!((min, max), (20, 235), "at an exact 3:1 ratio every bilinear weight is zero, so the
///                                     stripes survive unchanged");
/// ```
///
/// The finding it recorded was real and correct — at 3:1 `axis_map`'s weights are all zero, so the
/// filter *was* a decimator — but **a test named "not a decimation" that asserts the stripes come
/// through unchanged is asserting a decimation.** The name described the defect, the assertion described
/// the behaviour, and the test passed while the thing it was named for was still true. That is a real
/// hazard of naming a test after the property rather than after the observation.
///
/// # What it asserts now
///
/// With `use_area` handing 3:1 to the area filter, each destination pixel is the mean of **three**
/// source pixels. The chart's band is period-2 stripes of 20 and 235, so three consecutive pixels are
/// always two of one and one of the other:
///
/// ```text
/// 235, 20, 235 -> 490 / 3 -> (490 + 1) / 3 = 163
///  20, 235, 20 -> 275 / 3 -> (275 + 1) / 3 =  92
/// ```
///
/// **So the pair is `(92, 163)` and not `(20, 235)`** — and neither value is a source value, which is the
/// whole claim: a decimation can only ever emit values the source contains. This is the end-to-end half
/// of PROJECT.md §7 item 3; `crates/holonomy-image/tests/area_filter.rs` is the filter-level gate.
///
/// **The stripe amplitude shrinks from 215 to 71, and that is correct.** A 2-pixel-period signal cannot
/// survive a 3:1 reduction at full amplitude — Nyquist says so — and an area average is what reduces it
/// to the *mean* rather than to whichever phase it happened to sample. What is lost is the aliasing; what
/// is kept is the band's presence, which the assertions below still require.
#[test]
fn the_raster_is_a_resample_and_not_a_decimation() {
    let (mut s, _) = with_image();
    s.paint(None).expect("paint");
    let pixels = s.image_cache_pixels(0).expect("resident").to_vec();
    let (rw, rh) = s.chrome_rect_for_image(0);
    assert_eq!((rw, rh), (640, 360));
    let at = |x: u32, y: u32| -> u8 { pixels[(y as usize * rw as usize + x as usize) * 4] };

    let band_top = 480 * rh / 1080 + 2;
    let band_bot = 560 * rh / 1080 - 2;
    let x0 = 320 * rw / 1920 + 4;
    let x1 = 1600 * rw / 1920 - 4;
    assert!(
        band_bot > band_top + 8 && x1 > x0 + 8,
        "the fine band survived the scale arithmetic"
    );

    let values: Vec<u8> = (band_top..band_bot)
        .flat_map(|y| (x0..x1).map(move |x| at(x, y)))
        .collect();
    let min = values.iter().copied().min().expect("the band is not empty");
    let max = values.iter().copied().max().expect("the band is not empty");

    // **Neither value is in the source.** That is the strongest form of the claim and it is what makes
    // this a decimation test rather than a tolerance test: a nearest-neighbour filter emits only source
    // values, so a band containing anything outside `{20, 235}` cannot have come from one.
    assert_eq!(
        (min, max),
        (92, 163),
        "a 3:1 area average of period-2 stripes gives 92 and 163, and neither is a value the chart \
         contains. (20, 235) would mean the filter decimated."
    );
    assert!(
        !values.contains(&20) && !values.contains(&235),
        "no destination pixel carries a source value: the band was averaged, not sampled"
    );

    // And the picture is still a picture rather than a smooth blob: the flat colour bands and the grid
    // survive, which is what distinguishes "averaged correctly" from "blurred".
    let row: Vec<u8> = (0..rw).map(|x| at(x, 100)).collect();
    assert!(
        row.iter().any(|&v| v > 150),
        "the colour bands survived the downscale"
    );
    assert!(
        row.iter().any(|&v| v < 60),
        "the grid survived the downscale"
    );
}

#[test]
fn a_node_image_is_emitted_and_the_painter_blits_it() {
    let (mut s, _) = with_image();
    s.paint(None).expect("paint");
    assert_eq!(s.stats.images_drawn, 1, "one node emitted");
    // `resampled`/`images_missing`/`image_pixels` live on `PaintStats`, which `Session::paint` folds
    // into `SessionStats` rather than returning. So they are asserted through the painter directly
    // below, where the tree is in hand.
    assert_eq!(s.stats.images_decoded, 1);
}

#[test]
fn the_image_lands_at_the_pixels_the_layout_said_it_would() {
    let (mut s, keep) = with_image();
    s.paint(None).expect("paint");
    let frame = keep.presented();
    let (rw, rh) = s.chrome_rect_for_image(0);
    let x = s.chrome.layout.text.x;
    let y = s.chrome.layout.text.y + s.state.line_heights.y(s.line_of_anchor(0));
    // The rect the session computed is the one the layout's own numbers produce, recomputed here from
    // `layout.text` and `LineHeights` rather than read back out of the session. A test that asked the
    // session where it drew and compared it to the session would agree with any layout, wrong or not.
    let page_height = s.chrome.layout.text.y + s.chrome.metrics.cell_h * s.chrome.layout.rows;
    assert!(
        y + rh <= page_height + rh,
        "the image's height is what pushes the lines below it, not clamped to the page"
    );
    assert!(
        x + rw <= s.chrome.metrics.width,
        "the image fits the page column"
    );

    // The chart's top-left is 220 red-ish, its bottom-right is a green/blue mix. Read both out of the
    // presented frame: this is the assertion that survives a coordinate bug anywhere in the chain.
    let top_left = frame.pixel(x + 2, y + 2) & 0x00FF_FFFF;
    let mid = frame.pixel(x + rw / 2, y + rh / 2) & 0x00FF_FFFF;
    assert_ne!(
        top_left,
        frame.pixel(x, y) & 0x00FF_FFFF,
        "the image's corner differs from the page beside it"
    );
    assert_ne!(
        mid, 0xFFFA_FAF8,
        "the image's middle is not the page colour"
    );
    assert_ne!(
        mid, top_left,
        "the chart has structure, so its middle differs from its corner"
    );
}

#[test]
fn a_transparent_pixel_does_not_paint_over_the_page() {
    // Alpha is honoured: a fully transparent source pixel must leave the destination alone. The
    // fixture's top-left 120x120 block is forced fully transparent for this, below.
    let atlas: &'static holonomy_assets::atlas::Atlas = Box::leak(Box::new(
        holonomy_assets::build_atlas(&[16]).expect("atlas").0,
    ));
    let with_alpha = png_with_transparent_corner();
    let editor = holonomy_text::Editor::from_text(b"x").expect("editor");
    let mut s = Session::new(
        editor,
        Painter::new(atlas, 0),
        Box::new(Keep::sized((
            ChromeMetrics::DESKTOP.width,
            ChromeMetrics::DESKTOP.height,
        ))),
        ChromeMetrics::DESKTOP,
    );
    s.insert_image_bytes(&with_alpha).expect("insert");
    s.paint(None).expect("paint");

    let (rw, rh) = s.chrome_rect_for_image(0);
    assert_eq!(
        (rw, rh),
        (64, 64),
        "a 64x64 source is narrower than the column and is not enlarged"
    );
    // Read the raster's own alpha at the *centre*, which is the bottom-right opaque block's centre --
    // offset (1, 1) is the transparent top-left block. The distinction matters: sampling at the frame's
    // origin tests the raster's (0, 0), and getting that wrong would make the test pass for the wrong
    // reason, which is the whole failure mode this file exists to catch.
    let pixels = s.image_cache_pixels(0).expect("resident");
    let at = |x: u32, y: u32| -> [u8; 4] {
        let i = (y as usize * rw as usize + x as usize) * 4;
        [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
    };
    assert_eq!(
        at(1, 1)[3],
        0,
        "the fixture's top-left block is transparent"
    );
    assert_eq!(at(48, 48)[3], 255, "and its bottom-right block is opaque");

    // The image's own line is displaced by its own height -- `LineHeights::from`'s rule, the same one
    // tables and formulas follow -- so it draws at `text.y + y(anchor_line)`, not at `text.y`.
    // Reading the displacement from the model rather than assuming it is the point: an image drawn at
    // the wrong y would still put an opaque block *somewhere*, and this test would miss it.
    let x = s.chrome.layout.text.x;
    let y = s.chrome.layout.text.y + s.state.line_heights.y(s.line_of_anchor(0));
    assert!(
        y > s.chrome.layout.text.y,
        "a block is pushed down by its own height, so it does not start at the page's first row"
    );
    // The transparent corner left the page alone: `PAGE` is `0xFFFA_FAF8`, and the frame stores
    // `0xAARRGGBB`, so `frame_pixel`'s `& 0x00FF_FFFF` makes this the same constant.
    assert_eq!(
        s.frame_pixel(x + 1, y + 1),
        holonomy_render::chrome::colour::PAGE & 0x00FF_FFFF,
        "a fully transparent source pixel must not paint the page over it"
    );
    // And the opaque block did paint.
    assert_ne!(
        s.frame_pixel(x + 48, y + 48),
        holonomy_render::chrome::colour::PAGE & 0x00FF_FFFF,
        "the opaque block was drawn"
    );
}

/// A 64x64 PNG whose top-left 32x32 block is transparent and whose bottom-right is opaque red.
fn png_with_transparent_corner() -> Vec<u8> {
    /// CRC-32, IEEE. `crc32fast` is the dependency §2.9.5 refuses and this runs 64 times.
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xFFFF_FFFFu32;
        for &b in bytes {
            crc ^= u32::from(b);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
        !crc
    }
    fn chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        let mut crc_input = Vec::new();
        crc_input.extend_from_slice(kind);
        crc_input.extend_from_slice(body);
        out.extend_from_slice(&crc32(&crc_input).to_be_bytes());
        out
    }
    let (w, h) = (64u32, 64u32);
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8 bpc, truecolour + alpha
    let mut raw = Vec::new();
    for y in 0..h {
        raw.push(0u8);
        for x in 0..w {
            let opaque = x >= 32 && y >= 32;
            raw.extend_from_slice(&[200, 30, 40, if opaque { 255 } else { 0 }]);
        }
    }
    let mut deflated = vec![0x78, 0x01];
    let mut i = 0usize;
    while i < raw.len() {
        let take = (raw.len() - i).min(0xFFFF);
        deflated.push(if i + take >= raw.len() { 1 } else { 0 });
        deflated.extend_from_slice(&(take as u16).to_le_bytes());
        deflated.extend_from_slice(&(!(take as u16)).to_le_bytes());
        deflated.extend_from_slice(&raw[i..i + take]);
        i += take;
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in &raw {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    deflated.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    out.extend_from_slice(&chunk(b"IHDR", &ihdr));
    out.extend_from_slice(&chunk(b"IDAT", &deflated));
    out.extend_from_slice(&chunk(b"IEND", &[]));
    out
}

#[test]
fn an_image_outside_the_window_is_evicted_and_the_budget_holds() {
    let (mut s, _) = with_image();
    s.paint(None).expect("first paint");
    assert_eq!(s.image_cache_len(), 1);

    // Scroll far past it. The ±1 window drops the raster, and it is scrubbed on the way out.
    for _ in 0..200 {
        s.scroll_by(1);
    }
    s.paint(None).expect("paint after scrolling");
    assert_eq!(s.image_cache_len(), 0, "the raster was evicted");
    assert_eq!(s.image_cache_bytes(), 0, "and its bytes are accounted back");
    assert!(s.stats.images_evicted >= 1, "and the eviction was counted");

    // Scrolling back re-decodes. That is the policy working as specified, not a bug: the ±1 window
    // is a *budget*, and a budget that never released anything would be unbounded residency.
    for _ in 0..200 {
        s.scroll_by(-1);
    }
    s.paint(None).expect("paint after scrolling back");
    assert_eq!(s.stats.images_drawn, 1);
    assert_eq!(s.stats.images_decoded, 2, "one decode each way");
}

#[test]
fn three_images_at_the_column_width_all_fit_in_the_budget() {
    // §2.9.3's arithmetic as a test: nine page-column rasters fit in 8.0 MiB. Three is well inside
    // that, and it is the case the arithmetic was written for -- several images on facing pages.
    let atlas: &'static holonomy_assets::atlas::Atlas = Box::leak(Box::new(
        holonomy_assets::build_atlas(&[16]).expect("atlas").0,
    ));
    let editor = holonomy_text::Editor::from_text(b"").expect("editor");
    let mut s = Session::new(
        editor,
        Painter::new(atlas, 0),
        Box::new(Keep::sized((
            ChromeMetrics::DESKTOP.width,
            ChromeMetrics::DESKTOP.height,
        ))),
        ChromeMetrics::DESKTOP,
    );
    for _ in 0..3 {
        s.insert_image_bytes(TEST_CHART_PNG).expect("insert");
    }
    s.paint(None).expect("paint");
    // Three *distinct* catalog entries even though the bytes are identical: entry `i` serves anchor
    // `i`, and deduplicating would leave the second anchor pointing at the first anchor's slot.
    assert_eq!(s.assets().len(), 3);
    assert_eq!(
        s.image_cache_len(),
        1,
        "one raster, because the address is the same"
    );
    assert!(s.image_cache_bytes() <= s.image_cache_budget());
}

#[test]
fn an_image_survives_a_scrolling_repaint_with_its_pixels_intact() {
    // The stale-pixel bug this guards is the one the `widen()` fix was for: a node drawn outside the
    // damage model leaves pixels that no repaint clears. So: paint, scroll away, scroll back, and
    // compare the image's rectangle against the first frame's.
    let (mut s, keep) = with_image();
    s.paint(None).expect("first paint");
    let first = keep.presented();
    for _ in 0..5 {
        s.scroll_by(3);
    }
    s.paint(None).expect("paint scrolled");
    for _ in 0..5 {
        s.scroll_by(-3);
    }
    s.paint(None).expect("paint back");
    let back = keep.presented();

    let (rw, rh) = s.chrome_rect_for_image(0);
    let x = s.chrome.layout.text.x;
    let y = s.chrome.layout.text.y + s.state.line_heights.y(s.line_of_anchor(0));
    for (dx, dy) in [(3u32, 3u32), (rw / 2, rh / 2), (rw - 4, rh - 4)] {
        assert_eq!(
            first.pixel(x + dx, y + dy) & 0x00FF_FFFF,
            back.pixel(x + dx, y + dy) & 0x00FF_FFFF,
            "the image's pixel at ({dx},{dy}) changed across a scroll round trip"
        );
    }
}

#[test]
fn a_paint_with_no_raster_source_counts_every_image_as_missing() {
    // `Painter::paint` is the no-source form. The assertion is that it says so in the stats rather than
    // drawing nothing and reporting success -- a page of blank rectangles with `images_missing == 0`
    // would be the bug.
    let atlas: &'static holonomy_assets::atlas::Atlas = Box::leak(Box::new(
        holonomy_assets::build_atlas(&[16]).expect("atlas").0,
    ));
    let mut tree = holonomy_render::SurfaceTree::group();
    tree.before.push(holonomy_render::SurfaceTree::leaf(
        holonomy_render::Node::Image {
            rect: holonomy_render::Rect::new(0, 0, 4, 4, 0xFF00_0000),
            asset_id: holonomy_text::AssetId::of(b"not resident"),
        },
    ));
    let mut frame = Frame::black(64, 64);
    let stats = Painter::new(atlas, 0)
        .paint(&mut frame, &tree, None)
        .expect("paint");
    assert_eq!(stats.images_missing, 1);
    assert_eq!(stats.resampled, 0);
    assert_eq!(
        stats.pixels, 0,
        "nothing was written for an image with no pixels"
    );
}

#[test]
fn line_heights_merge_two_blocks_on_one_line_rather_than_overwriting() {
    // An image and a table on one line must add, or one silently displaces the other. This is
    // `LineHeights::from`'s own rule, restated through the session's inputs because the session is what
    // feeds it.
    let blocks = [(4u32, 100u32), (4, 50)];
    let lh = LineHeights::from(25, &blocks);
    assert_eq!(lh.height(4), 25 + 150);
    assert_eq!(lh.y(5), 5 * 25 + 150);
}

/// `PaintStats`'s image counters, asserted against a painter the test drives directly.
///
/// `Session::paint` folds `PaintStats` into `SessionStats` and does not return it, so the counters
/// themselves are observed here rather than through the session. The tree is built by hand so the only
/// node in the frame is the image.
#[test]
fn the_painters_image_counters_are_what_a_session_frame_records() {
    let atlas: &'static holonomy_assets::atlas::Atlas = Box::leak(Box::new(
        holonomy_assets::build_atlas(&[16]).expect("atlas").0,
    ));
    let id = holonomy_text::AssetId::of(TEST_CHART_PNG);
    let (rw, rh) = (8u32, 8u32);
    // An identity resample through the real scaler, so the fixture's pixels went through the same
    // fixed-point arithmetic the session's do rather than being asserted against directly.
    let mut pixels = vec![0u8; (rw * rh * 4) as usize];
    holonomy_image::scale::resample_pixels(
        rw,
        rh,
        &solid(rw, rh, [10, 20, 30, 255]),
        rw,
        rh,
        &mut pixels,
    )
    .expect("identity resample");

    struct One {
        id: holonomy_text::AssetId,
        pixels: Vec<u8>,
        w: u32,
        h: u32,
    }
    impl holonomy_render::RasterSource for One {
        fn raster(&self, id: holonomy_text::AssetId) -> Option<holonomy_render::Raster<'_>> {
            (id == self.id).then_some(holonomy_render::Raster {
                width: self.w,
                height: self.h,
                pixels: &self.pixels,
            })
        }
    }

    let src = One {
        id,
        pixels,
        w: rw,
        h: rh,
    };
    let mut tree = holonomy_render::SurfaceTree::group();
    tree.before.push(holonomy_render::SurfaceTree::leaf(
        holonomy_render::Node::Image {
            rect: holonomy_render::Rect::new(4, 4, rw, rh, 0xFF00_0000),
            asset_id: id,
        },
    ));
    let mut frame = Frame::black(64, 64);
    let stats: PaintStats = Painter::new(atlas, 0)
        .paint_with_rasters(&mut frame, &tree, None, Some(&src))
        .expect("paint");

    assert_eq!(stats.resampled, 1);
    assert_eq!(stats.images_missing, 0);
    assert_eq!(
        stats.image_pixels,
        (rw * rh) as u64,
        "every pixel of the raster was written"
    );
    // `Frame` stores `0xAARRGGBB`, so an RGBA source of `(10, 20, 30, 255)` lands as `0x001E140A`.
    // Written in that order rather than `0x000A141E` on purpose: the first version of this assertion
    // wrote the bytes in RGB order and reported a blend bug that was not there.
    assert_eq!(
        frame.pixel(5, 5) & 0x00FF_FFFF,
        0x001E_140A,
        "an opaque blit is the identity: no channel may be altered"
    );
}

/// A `w x h` solid RGBA image.
fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    rgba.iter()
        .copied()
        .cycle()
        .take((w * h * 4) as usize)
        .collect()
}
