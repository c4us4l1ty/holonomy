//! **What the zoom control actually does, measured rather than assumed.** 6 tests.
//!
//! # The claim
//!
//! **Every pixel that changes when the zoom changes lies inside the zoom button's own rect, and the page
//! is pixel-identical at 50% and at 200%.** `zoom_percent` is a number the toolbar prints and the
//! dropdown ticks. It does not scale the text, the page, the images, or the layout.
//!
//! # Why this needed measuring rather than reading
//!
//! Because there was a **false comment** saying otherwise. `Session::set_zoom` claimed:
//!
//! > **25..=400 because that is what `--zoom` takes and what §2.9.3's image-cache thresholds were
//! > measured against.**
//!
//! **No code reads `zoom_percent` except the label and the tick.** There is no image-cache coupling.
//! The sentence was plausible, specific, and attached to a real number, and it was wrong.
//!
//! # Why it matters beyond tidiness
//!
//! **Three paths reach this number and all three look like they work.** `--zoom 200` on the command
//! line. F11 and F12, which `Keymap::us` decodes to `ZoomIn`/`ZoomReset` and `Session::apply` drops on
//! the floor. And the zoom dropdown, which part 21 built and which counted its choices as
//! `pointer_commands` — presses that "produced a `Command` the session applied", when no `Command` was
//! produced and no document byte changed.
//!
//! **A control that changes only its own label is not a zoom, and a counter that calls it a command is
//! not honest.** [`every_pixel_that_changes_when_the_zoom_changes_is_the_zoom_label`] is the measurement;
//! [`a_zoom_choice_is_not_counted_as_a_command`] is the correction to the counter.
//!
//! | what it proves | test |
//! | --- | --- |
//! | the label follows the number | [`the_zoom_label_follows_the_number`] |
//! | **and nothing else does** | [`every_pixel_that_changes_when_the_zoom_changes_is_the_zoom_label`] |
//! | the page and the layout are untouched | [`the_page_and_its_layout_do_not_depend_on_the_zoom`] |
//! | and an image is not resampled | [`an_image_is_not_resampled_by_the_zoom`] |
//! | F11 and F12 are decoded, and dropped | [`the_zoom_keys_are_decoded_and_dropped`] |
//! | and the label fits at both clamp ends | [`the_zoom_label_fits_its_button_at_both_ends_of_the_clamp`] |

use holonomy::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_render::chrome::ChromeMetrics;
use holonomy_render::widgets;
use holonomy_text::{Editor, SpanPolicy};

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (a, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(a))
    })
}

fn session(text: &str) -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    if !text.is_empty() {
        ed.insert_at(0, text.as_bytes(), SpanPolicy::GrowIntoInsert)
            .expect("room");
    }
    Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    )
}

/// A session painted at `zoom`, with the caret parked at the start so it is not what differs.
///
/// **Through `Session::set_zoom`, not by assigning `state.zoom_percent`.** That is not a style choice
/// and this helper is where the reason lives: **the first version of this file assigned the field, and
/// the bite check caught it.** `set_zoom` was private, so the three `--zoom` paths in the product
/// assigned the field too — which meant the one function a zoom would live in was unreachable from
/// every gate. Making `set_zoom` public and giving it a real cell scale left all six tests green.
///
/// **A gate that assigns the field tests the field, not the feature.**
fn painted_at(zoom: u32, text: &str) -> Session<'static> {
    let mut s = session(text);
    s.caret_to(0).expect("park the caret");
    s.set_zoom(zoom);
    s.repaint_all().expect("paint");
    s
}

/// The zoom button's rect, which is the only thing the zoom is allowed to change.
fn zoom_rect(s: &Session<'_>) -> holonomy_render::DamageRect {
    widgets::place_toolbar(&s.chrome_layout(), 8)
        .into_iter()
        .find(|p| p.tool.name() == "zoom")
        .expect("the toolbar has a zoom control")
        .rect
}

/// **The label follows the number.** The one thing it does, asserted first so the rest is about the
/// difference rather than about nothing happening at all.
#[test]
fn the_zoom_label_follows_the_number() {
    let mut s = session("");
    assert_eq!(s.state.zoom_percent, 100, "the default");
    assert_eq!(s.state.zoom_label(), "100%");
    s.set_zoom(175);
    assert_eq!(s.state.zoom_label(), "175%");
}

/// **Every pixel that changes when the zoom changes is the zoom label.**
///
/// **This is the whole part.** The measurement is a bounding box over the differing pixels, and the
/// assertion is that the box is *inside the button*. Not "the page is unchanged" — that would pass for a
/// zoom that moved the page by one pixel — and not "some pixels changed" — that would pass for anything.
/// **The box is inside the zoom button's own rect, so the label moved and nothing else in the window
/// did.**
///
/// The fixture is three lines rather than one, because a one-line document leaves the sidebar and the
/// status bar empty and a bug that repainted either would have nothing to repaint.
#[test]
fn every_pixel_that_changes_when_the_zoom_changes_is_the_zoom_label() {
    let text = "hello world\nsecond line\nthird line\n";
    let a = painted_at(100, text);
    let b = painted_at(200, text);

    let (mut x0, mut y0) = (u32::MAX, u32::MAX);
    let (mut x1, mut y1) = (0u32, 0u32);
    let mut differing = 0u32;
    for y in 0..a.chrome.metrics.height {
        for x in 0..a.chrome.metrics.width {
            if a.frame().pixel(x, y) != b.frame().pixel(x, y) {
                differing += 1;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
        }
    }

    assert!(
        differing > 0,
        "nothing changed at all, so this is measuring nothing"
    );
    let z = zoom_rect(&a);
    assert!(
        x0 >= z.x && x1 < z.x + z.width && y0 >= z.y && y1 < z.y + z.height,
        "**{differing} pixels changed, and they are not all inside the zoom button.** The differing \
         box is x {x0}..={x1} y {y0}..={y1}; the zoom button is {z:?}. Anything outside it is a \
         pixel the zoom moved that this part does not know about."
    );
}

/// **The page and its layout do not depend on the zoom at all.**
///
/// **Asserted as equality of the whole `Layout`,** because a `Copy` struct comparing equal is a stronger
/// statement than checking the three or four fields a reader would guess to check — it says *no* field
/// of the layout is derived from the zoom, including the ones this part did not think of.
#[test]
fn the_page_and_its_layout_do_not_depend_on_the_zoom() {
    let text = "hello world\nsecond line\n";
    let a = painted_at(50, text);
    let b = painted_at(400, text);
    assert_eq!(
        a.chrome_layout(),
        b.chrome_layout(),
        "the layout is the same at 50% and at 400%: a real zoom would change `cell_w` and the page width"
    );
    assert_eq!(
        a.chrome.metrics.cell_w, b.chrome.metrics.cell_w,
        "and the text grid is the same width, which is the specific thing a zoom must change"
    );
    // **Zero differing pixels inside the page**, stated separately because "the layouts are equal" is
    // about geometry and this is about ink.
    let l = a.chrome_layout();
    let mut inside = 0u32;
    for y in l.page.y..l.page.y + l.page.height {
        for x in l.page.x..l.page.x + l.page.width {
            if a.frame().pixel(x, y) != b.frame().pixel(x, y) {
                inside += 1;
            }
        }
    }
    assert_eq!(inside, 0, "the page is pixel-identical at 50% and at 400%");
}

/// **An image is not resampled by the zoom.**
///
/// **The one a reader would assume is covered.** Zoom in a document editor means the pictures scale, and
/// `zoom_percent` sitting next to an image cache with resampling code in it is exactly the shape of a
/// coupling that *looks* real. **There is none**, and this is the gate that says so with a fixture rather
/// than with a grep.
#[test]
fn an_image_is_not_resampled_by_the_zoom() {
    // **The project's own `TEST_CHART_PNG`, not a hand-assembled one.** The first version of this gate
    // carried a 2x2 PNG written out by hand, and the fixture did not decode: `images_missing: 1`,
    // `image_pixels: 0`, and a comparison of two blank pages would have passed for the wrong reason.
    // **A fixture that does not draw is a fixture that cannot fail**, and the only defence is to reuse
    // a picture something else has already proved decodes.
    let png = holonomy::TEST_CHART_PNG;

    let mk = |zoom: u32| {
        let mut s = session("");
        s.insert_image_bytes(png).expect("insert the image");
        s.caret_to(0).expect("park the caret");
        s.set_zoom(zoom);
        s.repaint_all().expect("paint");
        s
    };
    let a = mk(50);
    let b = mk(400);
    let stats = a.paint_stats();
    assert!(
        stats.image_pixels > 0 && stats.images_missing == 0,
        "the fixture drew no image, so this is measuring an empty page: {stats:?}"
    );

    let l = a.chrome_layout();
    let mut differing = 0u32;
    for y in l.page.y..l.page.y + l.page.height {
        for x in l.page.x..l.page.x + l.page.width {
            if a.frame().pixel(x, y) != b.frame().pixel(x, y) {
                differing += 1;
            }
        }
    }
    assert_eq!(
        differing, 0,
        "the same picture is drawn at the same size at 50% and at 400%: a zoom that resampled would \
         differ, and this is what says it does not"
    );
}

/// **F11 and F12 are decoded into commands and then dropped, and that is the finding.**
///
/// **`Keymap::us` maps F11 to `ZoomIn`, Shift+F11 to `ZoomOut` and F12 to `ZoomReset`**, and
/// `Session::apply` has all three in one arm that does nothing. **A key that decodes and then vanishes
/// is the hardest kind of nothing to notice** — the keymap test passes, the command exists, and the
/// window ignores you.
///
/// **This test asserts the current behaviour deliberately**, and says so, because the alternative is
/// worse: a gate asserting "F11 does nothing" reads as an endorsement, and a gate asserting "F11 zooms"
/// cannot be written until zoom is real. **What can be written is the shape of the hole** — decoded,
/// dropped, and not counted as a press.
#[test]
fn the_zoom_keys_are_decoded_and_dropped() {
    use holonomy_input::keymap::Keymap;
    use holonomy_input::ModifierState;
    use holonomy_input::{Command, InputEvent, KEY_F11, KEY_F12};

    // **The keymap half, through the real entry point.** If this stops decoding, the failure is a
    // different bug and the rest of the test would be measuring a key that no longer exists.
    let km = Keymap::us();
    let mut mods = ModifierState::new();
    assert_eq!(
        km.dispatch(InputEvent::press(KEY_F11), &ModifierState::new()),
        Some(Command::ZoomIn),
        "F11"
    );
    assert_eq!(
        km.dispatch(InputEvent::press(KEY_F12), &ModifierState::new()),
        Some(Command::ZoomReset),
        "F12"
    );
    // **Shift first, then F11, through `dispatch_into` with the state that actually moved.** The first
    // version of this asserted `ZoomOut` from a bare F11 press and got `ZoomIn`, which is the keymap
    // being right and the gate being wrong: a modifier has to be *pressed*, not assumed.
    assert_eq!(
        km.dispatch_into(InputEvent::press(holonomy_input::KEY_LEFTSHIFT), &mut mods),
        None,
        "a modifier press produces no command"
    );
    assert_eq!(
        km.dispatch_into(InputEvent::press(KEY_F11), &mut mods),
        Some(Command::ZoomOut),
        "Shift+F11"
    );

    // **The session half, which is the finding.** Applying them changes nothing at all — not the
    // damage, not the state, not the stats.
    let mut s = session("hello\n");
    s.repaint_all().expect("paint");
    let before = (
        s.state.zoom_percent,
        s.damage(),
        s.stats.pointer_commands,
        s.stats.pointer_inert,
    );
    s.apply(Command::ZoomIn).expect("apply");
    s.apply(Command::ZoomOut).expect("apply");
    s.apply(Command::ZoomReset).expect("apply");
    assert_eq!(
        (
            s.state.zoom_percent,
            s.damage(),
            s.stats.pointer_commands,
            s.stats.pointer_inert
        ),
        before,
        "**the three zoom commands do nothing** — not the state, not the damage, not any counter. \
         They are decoded by the keymap and dropped by `apply`. That is the hole this file measures, \
         and wiring them to `set_zoom` would change a label rather than a page, so part 23 left them \
         dropped and said so."
    );
}

/// **The label fits its button at both ends of the clamp.**
///
/// **The clamp is the only thing `set_zoom` still earns, and this is why.** The number is printed inside
/// a fixed-width button, so a value wide enough to overflow would be drawn outside its own chrome —
/// **and that is a real, visible failure**, unlike the zoom not being a zoom. `set_zoom`'s doc says so
/// and this is the gate behind the sentence.
#[test]
fn the_zoom_label_fits_its_button_at_both_ends_of_the_clamp() {
    for percent in [25u32, 100, 400] {
        let mut s = session("");
        s.set_zoom(percent);
        let label = s.state.zoom_label();
        let z = zoom_rect(&s);
        // **Five characters for `400%`, and the button is 62 px of an 8 px cell.** The check is stated in
        // characters rather than pixels because the cell is what the label is laid out on and a pixel
        // comparison would depend on the cell width being 8.
        assert!(
            (label.len() as u32) * s.chrome.metrics.cell_w <= z.width,
            "`{label}` at {percent}% is {} px wide and the button is {} px",
            label.len() as u32 * s.chrome.metrics.cell_w,
            z.width
        );
    }
    // **And the clamp actually clamps**, through the one function that takes a value. `--zoom 10000` is
    // refused by the argument parser upstream, so this is the session-side belt -- and a belt that is
    // never tested is a belt that is not there.
    let mut s = session("");
    s.set_zoom(70_000);
    assert_eq!(s.state.zoom_percent, 400, "70000 clamps to 400");
    s.set_zoom(0);
    assert_eq!(s.state.zoom_percent, 25, "and 0 clamps up to 25");
}
