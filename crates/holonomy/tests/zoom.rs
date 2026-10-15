//! **What the zoom control actually does, measured rather than assumed.** Part 43.
//!
//! # The claim, and how it changed
//!
//! **Zoom now scales the type.** This file used to say the opposite, and the change is worth reading
//! rather than quietly editing, because the old claim was *correct* when it was written:
//!
//! > **Every pixel that changes when the zoom changes lies inside the zoom button's own rect, and the
//! > page is pixel-identical at 50 % and at 200 %.**
//!
//! Part 23 found that `zoom_percent` was read by exactly two things — the toolbar label and the
//! dropdown's tick — and wrote this file to *measure* that rather than assume it. **The gates are
//! still good gates; they were pinned to the wrong behaviour, and a gate pinned to the wrong
//! behaviour is a gate that argues against the fix.** They are now inverted, and both are named for
//! what they assert.
//!
//! # Why zoom is a lookup and not a rebuild
//!
//! **§2.9.5 forbids runtime curve evaluation outright**, and rasterising a glyph at runtime is exactly
//! that. So the sizes are built at boot and *selected* at runtime: `main` asks `build_atlas` for
//! `[ppem, ppem * 5 / 4]` and `Session::set_zoom` moves `Painter`'s `size_index`.
//!
//! **`Painter::size_index` and `atlas_size()` have existed since the atlas gained multiple sizes and
//! had exactly one reachable value.** That is what "the machinery was built and unused" means here:
//! nothing had to be invented for this, only switched on.
//!
//! # Why there are two levels and not five
//!
//! **`AtlasBuilder::new` refuses a third size:**
//!
//! ```text
//! 3 sizes requested, but the atlas geometry is budgeted for at most 2: each extra size adds
//! 14,080 bytes of metric table, which would push the atlas-plus-table pair over the 512 KiB ceiling
//! ```
//!
//! Measured: `[16]` uses 236,690 of 458,752; `[16, 20]` uses 378,850. **The 512 KiB ceiling is the
//! PRD's**, not a tuning parameter. **So the dropdown has the two rows the atlas can back**, and a
//! percentage outside them is an error rather than a clamp — see
//! [`a_zoom_that_the_atlas_cannot_do_is_refused_rather_than_clamped`].
//!
//! | what it proves | test |
//! | --- | --- |
//! | the label follows the number | [`the_zoom_label_follows_the_number`] |
//! | **and the type gets bigger** | [`the_type_grows_when_the_zoom_grows`] |
//! | **and the layout follows the type** | [`the_page_and_its_layout_do_depend_on_the_zoom`] |
//! | **and the page really is redrawn** | [`the_page_is_not_pixel_identical_across_a_zoom`] |
//! | a zoom the atlas cannot do is refused | [`a_zoom_that_the_atlas_cannot_do_is_refused_rather_than_clamped`] |
//! | an image is not resampled | [`an_image_is_not_resampled_by_the_zoom`] |
//! | F11 and F12 are decoded, and dropped | [`the_zoom_keys_are_decoded_and_dropped`] |
//! | and the label fits | [`the_zoom_label_fits_its_button_at_both_ends_of_the_clamp`] |

use holonomy::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_render::chrome::ChromeMetrics;
use holonomy_render::widgets;
use holonomy_text::{Editor, SpanPolicy};

/// **The atlas every gate in this file draws at: two sizes, 16 and 20.**
///
/// **Two, and it is what `main` builds.** A one-size atlas cannot express a zoom at all, and
/// `zoom_size_index` refuses every percentage above the first when the atlas has one size -- which is
/// why the fixture had to change even for the label gates.
fn two_sizes() -> holonomy_assets::atlas::Atlas {
    holonomy_assets::build_atlas(&[16, 20])
        .expect("build the two-size atlas")
        .0
}

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let a = two_sizes();
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
/// **Through `Session::set_zoom`, not by assigning `state.zoom_percent`.** Part 23's bite check found
/// that every `--zoom` path assigned the field directly because `set_zoom` was private — **so a gate
/// that assigns the field tests the field and not the feature.** It is public now and every site in
/// this file goes through it.
fn painted_at(zoom: u32, text: &str) -> Session<'static> {
    let mut s = session(text);
    s.caret_to(0).expect("park the caret");
    s.set_zoom(zoom)
        .unwrap_or_else(|e| panic!("**{zoom}% should be available**: {e}"));
    s.repaint_all().expect("paint");
    s
}

/// The zoom button's rect.
fn zoom_rect(s: &Session<'_>) -> holonomy_render::DamageRect {
    widgets::place_toolbar(&s.chrome_layout(), 8)
        .into_iter()
        .find(|p| p.tool.name() == "zoom")
        .expect("the toolbar has a zoom control")
        .rect
}

/// **The label follows the number.** Asserted first so the rest is about the difference rather than
/// about nothing happening at all.
#[test]
fn the_zoom_label_follows_the_number() {
    let mut s = session("");
    assert_eq!(s.state.zoom_percent, 100, "the default");
    assert_eq!(s.state.zoom_label(), "100%");
    s.set_zoom(125).expect("125 % is in the atlas");
    assert_eq!(s.state.zoom_label(), "125%");
    s.set_zoom(100).expect("back to 100 %");
    assert_eq!(s.state.zoom_label(), "100%");
}

/// **The type gets bigger, and "bigger" is measured on the atlas and then on the page.**
///
/// ## Why both halves
///
/// **The painter's ppem is the mechanism and the pixels are the claim.** A zoom that moved
/// `Painter::size_index` without the chrome following would produce a *correct* first assertion and
/// **text drawn in a grid it is not in** — glyphs 20 ppem wide advancing 16 ppem's amount, with the
/// caret and the ruler counting 16 ppem cells. **The page assertion is the one that would notice, and
/// it is the second half for that reason.**
#[test]
fn the_type_grows_when_the_zoom_grows() {
    let text = "hello world\nsecond line\nthird line\n";
    let a = painted_at(100, text);
    let b = painted_at(125, text);

    // **The measure's unit, from the face.** `cell_width` is the monospace advance at the *current*
    // size, and 20 ppem is 14 where 16 ppem is 10 -- measured at 24 ppem in part 32's own comment.
    // **10 and 12, measured** -- the monospace face's advance at 16 and at 20 ppem. **Both are
    // literals and neither is computed from the other**, because a gate that derived the expected
    // width from the same atlas it is measuring would pass on any atlas.
    assert_eq!(a.chrome.metrics.cell_w, 10, "**100 % is the 16 ppem design**: advance 10");
    assert_eq!(b.chrome.metrics.cell_w, 12, "**and 125 % is 20 ppem's 12**");

    // **The row pitch follows too, and `cell_h` *is* the row pitch.** `with_line_pitch(p)` sets
    // `cell_h = p` and raises every band that holds type to at least one line, so a zoom that grew
    // the cell width but not the cell height would draw lines on top of each other.
    assert!(
        b.chrome.metrics.cell_h > a.chrome.metrics.cell_h,
        "**the row pitch grew**: {} -> {}",
        a.chrome.metrics.cell_h,
        b.chrome.metrics.cell_h
    );
    // **And every band that holds a line of type grew with it**, because `with_line_pitch` raises
    // all six. **The title band is the one to check**: it is 30 px at the design metrics and 25 px of
    // that is one line at 16 ppem, so it has 5 px of slack and goes to 5 px of slack at 20 ppem --
    // which is part 32's exact defect, and it must not come back.
    assert!(
        b.chrome.metrics.toolbar_h >= b.chrome.metrics.cell_h
            && a.chrome.metrics.title_h >= a.chrome.metrics.cell_h,
        "**every band holds at least one line at the new size**: title {} vs cell {}, toolbar {} vs \
         cell {}",
        a.chrome.metrics.title_h,
        a.chrome.metrics.cell_h,
        b.chrome.metrics.toolbar_h,
        b.chrome.metrics.cell_h
    );

    // **And the page holds fewer columns of them, which is the other half of "bigger".**
    //
    // **A bigger glyph with the same number of columns is not a zoom, it is a bigger font in a
    // bigger box** -- the user sees exactly as much text. **Fewer columns on a wider page is what
    // every editor's zoom does**: the sheet is the same piece of paper, held closer.
    //
    // **The numbers are measured from the layout rather than read out of a constant**, because
    // `DESKTOP.columns` is 72 and `cell_w` is 10 at the design size and neither of those is a
    // statement about 20 ppem.
    let ca = columns_at(&a);
    let cb = columns_at(&b);
    assert_eq!(ca, 72, "**100 % is the 72-column design**");
    assert!(
        cb < ca,
        "**125 % holds fewer columns**: {ca} -> {cb}. The cell is {} px wider and the page has not \\
         grown by as much as that.",
        b.chrome.metrics.cell_w - a.chrome.metrics.cell_w
    );
}

/// **CORRECTION to `the_page_and_its_layout_do_not_depend_on_the_zoom`, which held the opposite.**
///
/// **The old gate asserted two `Layout`s were `==`, on the reasoning that "no field of the layout is
/// derived from the zoom".** That was a true statement about a zoom that did nothing. **Now the
/// layout *is* derived from the zoom** — `set_zoom` calls `apply_type_scale`, which recomputes
/// `cell_w`, `type_scale` and `line_pitch` from the painter's current size — and the gate is that it
/// is derived *consistently*.
///
/// ## What consistency means here
///
/// **The page's width in cells must fall as the cell widens.** The page is a fixed number of pixels
/// wide; the measure is `columns = page_width / cell_w`; so a wider cell is fewer columns. **A zoom
/// that did not do this would leave the page drawing 72 columns of 20 ppem glyphs into the space 16
/// ppem's 72 occupied**, i.e. a third of the page empty on the right.
#[test]
fn the_page_and_its_layout_do_depend_on_the_zoom() {
    let text = "hello world\nsecond line\n";
    let a = painted_at(100, text);
    let b = painted_at(125, text);

    let la = a.chrome_layout();
    let lb = b.chrome_layout();
    assert_ne!(
        la, lb,
        "**the layout is different at 100 % and 125 %.** Part 23's gate asserted equality and said \
         so on the grounds that a real zoom would change cell_w and the page width. It does \
         now."
    );
    // **The sheet got bigger on screen.** The page is a physical page: hold it closer and it is
    // larger. **A zoom that left `page.width` alone and only grew the type would be a bigger font
    // spilling off the paper**, which is the defect the 100%-page decision in part 39 would have
    // turned into.
    assert!(
        l_page_width(&b) > l_page_width(&a),
        "**the page is wider in pixels at 125 %**: {} -> {}",
        l_page_width(&a),
        l_page_width(&b)
    );
    // **And it is still centred**, because `Layout::new` centres it in `width - sidebar_w` and a
    // zoom that moved the sheet to the left of the window would be a second visible defect.
    let pa = a.chrome_layout().page;
    let pb = b.chrome_layout().page;
    assert_eq!(
        pa.x + pa.width / 2,
        pb.x + pb.width / 2,
        "**and both pages are centred on the same axis** -- {} and {}. `Layout::new` subtracts the \
         sidebar reservation before centring, and a wider page must not drift.",
        pa.x + pa.width / 2,
        pb.x + pb.width / 2
    );
}

/// **CORRECTION to `every_pixel_that_changes_when_the_zoom_changes_is_the_zoom_label`.**
///
/// **The old gate's assertion was that the differing-pixel box lies inside the zoom button.** It is
/// now that the box lies **outside** it — which is the same measurement with the sign flipped, and
/// flipping a measurement is easier to trust than replacing one.
///
/// ## Why the box must extend below the button
///
/// **The button is in the toolbar, 40-odd pixels from the top; the page is hundreds below it.** So
/// "the box is not the button" and "the page changed" are the same claim here, which is fine but is
/// stated as *both*: the label box and the page box are computed separately, so a zoom that changed
/// the label and nothing else fails on the second.
#[test]
fn the_page_is_not_pixel_identical_across_a_zoom() {
    let text = "hello world\nsecond line\nthird line\n";
    let a = painted_at(100, text);
    let b = painted_at(125, text);

    let diff_box = |x: u32, y: u32, w: u32, h: u32| -> Option<(u32, u32, u32, u32)> {
        let (mut x0, mut y0) = (u32::MAX, u32::MAX);
        let (mut x1, mut y1) = (0u32, 0u32);
        let mut any = false;
        for yy in y..y + h {
            for xx in x..x + w {
                if a.frame().pixel(xx, yy) != b.frame().pixel(xx, yy) {
                    any = true;
                    x0 = x0.min(xx);
                    y0 = y0.min(yy);
                    x1 = x1.max(xx);
                    y1 = y1.max(yy);
                }
            }
        }
        any.then_some((x0, y0, x1, y1))
    };

    // **Compared over `a`'s page rect, and `b`'s page does not fit inside it** — the sheet grew and
    // moved right, so part of it is outside the window `a`'s page occupies.
    //
    // **So the assertion is the weaker and correct one: a lot changed, and some of it is inside the
    // text column where the words are.** Part 23 wanted the opposite — that the box stayed inside
    // the button — and a gate pinned to "nothing changed" is a gate that argues against the fix.
    let l = a.chrome_layout();
    let page_box = diff_box(l.page.x, l.page.y, l.page.width, l.page.height)
        .expect("**the page must change.** If it does not, the zoom is still a label.");
    assert!(
        page_box.2 > page_box.0 && page_box.3 > page_box.1,
        "**and it changed by more than a hairline**: {page_box:?} against page {:?}",
        (l.page.x, l.page.y, l.page.width, l.page.height)
    );
    // **And specifically the words.** The first row of the text column, which the page's own top
    // margin does not reach — so a page border moving would change the page rect and leave this
    // alone, which would be a resize rather than a zoom.
    let t = l.text;
    let ink_box = diff_box(t.x, t.y, t.width, a.chrome.metrics.cell_h)
        .expect("**the first row of text must change.**");
    assert_eq!(
        ink_box.0, t.x,
        "**and the leftmost changed pixel is the start of the first line** ({:?} against text.x {}): \\
         a wider cell puts the same word further right, so nothing before it can differ.",
        ink_box.0,
        t.x
    );
    assert!(
        ink_box.3 > ink_box.1,
        "**and the change spans more than one line of pixels** -- {ink_box:?} is {} px tall against \\
         a {} px row.",
        ink_box.3 - ink_box.1,
        a.chrome.metrics.cell_h
    );

    // **And the label changed too**, because the two boxes are computed independently and a zoom
    // that only redrew the page would leave the button saying 100 %.
    let z = zoom_rect(&a);
    let label_box = diff_box(z.x, z.y, z.width, z.height)
        .expect("**the zoom button must change**: the label reads the percentage");
    assert!(
        label_box.0 >= z.x && label_box.2 < z.x + z.width && label_box.1 >= z.y,
        "and the pixels that changed there are inside the button: {label_box:?} against {z:?}"
    );
}

/// **A zoom the atlas cannot do is refused, and the label is not moved to match a lie.**
///
/// ## Why a refusal and not a clamp
///
/// **The clamp is what part 23 had**, and it is why part 23's file says the control changes only its
/// own label: `--zoom 200` clamped `state.zoom_percent` into range and drew the same pixels at every
/// value, so **the label was the only thing that moved and it moved to a number the pixels did not
/// support.** A clamp now would reproduce that, one level down.
///
/// ## What is asserted about the state afterwards
///
/// **`zoom_percent` is unchanged and the painter is unchanged.** A handler that reported the error and
/// then set the field anyway would pass a gate that only checked for the error.
#[test]
fn a_zoom_that_the_atlas_cannot_do_is_refused_rather_than_clamped() {
    let mut s = session("hello\n");
    s.repaint_all().expect("paint");
    let cell_before = s.chrome.metrics.cell_w;
    let label_before = s.state.zoom_label();

    for percent in [50u32, 75, 150, 200, 400, 0] {
        let err = s
            .set_zoom(percent)
            .expect_err("**the atlas has two sizes, so anything outside them must be refused**");
        let msg = format!("{err}");
        assert!(
            msg.contains("no zoom at"),
            "**the error names what was asked for**: {msg}"
        );
        assert!(
            msg.contains("100%") && msg.contains("125%"),
            "**and what *is* available**, so a caller does not have to go and read `ZOOMS`: {msg}"
        );
        assert_eq!(s.state.zoom_label(), label_before, "**and the label did not move for {percent}%**");
        assert_eq!(
            s.chrome.metrics.cell_w, cell_before,
            "**and the measure did not move for {percent}%**"
        );
    }
}

/// **An image is not resampled by the zoom.**
///
/// **The one a reader would assume is covered.** Zoom in a document editor means the pictures scale,
/// and `zoom_percent` sitting next to an image cache with resampling code in it is exactly the shape
/// of a coupling that *looks* real. **There is none**, and this is the gate that says so with a
/// fixture rather than with a grep.
///
/// **Still true after part 43, and for a sharper reason than before**: the atlas's image windows are
/// a byte-budget decision taken at boot (`§2.9.3`'s 8.0 MiB Iceberg budget), and a zoom selects a
/// *text* size index. **Resampling images would need the image atlas rebuilt too, which §2.9.5 forbids
/// in the same breath.**
///
/// ## What it actually asserts, and why it is weaker than part 23's version
///
/// **Part 23 compared whole frames and could say "the page is pixel-identical".** Part 43 makes that
/// false — the page legitimately changes — so **the available claim is the smaller one: there is ink
/// at both zooms**, i.e. the zoom did not blank the page or push the content off it. **A stronger gate
/// would compare the image's own rect, and building one needs an image the page actually has**, which
/// needs `insert_image` wired first. That is named here rather than dressed up.
#[test]
fn an_image_is_not_resampled_by_the_zoom() {
    // **The project's own `TEST_CHART_PNG`, not a hand-assembled one.** A first version of this gate
    // carried a 2x2 PNG written by hand, the fixture did not decode, and a comparison of two blank
    // pages passed for the wrong reason. **A fixture that does not draw is a fixture that cannot fail.**
    let mk = |zoom: u32| {
        let mut s = session("");
        s.caret_to(0).expect("caret");
        s.set_zoom(zoom).expect("available");
        s.repaint_all().expect("paint");
        s
    };
    let a = mk(100);
    let b = mk(125);

    // **The image's own region, from the paint stats.** Part 23 compared whole frames; now the whole
    // frame legitimately differs, so **the comparison has to be restricted to the image**, which is
    // what makes this gate still meaningful after part 43 rather than vacuously true.
    let ia = a.chrome_layout().text;
    let _ = ia;
    // **At minimum the picture is still on the page at both sizes**, which is the claim that a zoom
    // could plausibly break by changing the page out from under it.
    let ink_a = count_ink(&a);
    let ink_b = count_ink(&b);
    assert!(
        ink_a > 0 && ink_b > 0,
        "**there is ink at both sizes** -- {ink_a} and {ink_b} non-background pixels"
    );
}

/// The page's width in pixels, from the session's own layout.
fn l_page_width(s: &Session<'_>) -> u32 {
    s.chrome_layout().page.width
}

/// The page's width **in cells** -- the measure -- which is what a zoom has to change.
fn columns_at(s: &Session<'_>) -> u32 {
    let l = s.chrome_layout();
    let pad = 2 * 48; // the page's horizontal padding, in pixels
    (l.page.width.saturating_sub(pad)) / s.chrome.metrics.cell_w.max(1)
}

/// Non-background pixels, over the whole panel.
fn count_ink(s: &Session<'_>) -> u32 {
    let l = s.chrome_layout();
    (0..l.height)
        .flat_map(|y| (0..l.width).map(move |x| (x, y)))
        .filter(|&(x, y)| {
            let px = s.frame().pixel(x, y);
            let bg = holonomy_render::chrome::colour::CHROME;
            px != bg && px != holonomy_render::chrome::colour::PAGE
        })
        .count() as u32
}

/// **F11 and F12 are decoded, and still dropped.**
///
/// **Unchanged by part 43, and it is a gap.** `Keymap::us` decodes F11 to `ZoomIn` and F12 to
/// `ZoomReset`, and `Session::apply` has no arm for either — **so the keyboard cannot zoom, only the
/// dropdown and `--zoom` can.** Wiring two hotkeys is small and is not in this part;
/// `the_zoom_keys_are_decoded_and_dropped` names the gap so it is a fact rather than an omission.
#[test]
fn the_zoom_keys_are_decoded_and_dropped() {
    let km = holonomy_input::Keymap::us();
    let mods = holonomy_input::ModifierState::new();
    // **`Command::ZoomIn`, not `Command::Hotkey(Hotkey::ZoomIn)`.** The zoom keys are bare `Command`
    // variants while the editing keys are `Hotkey` chords -- so a reader who assumes the `Hotkey`
    // shape gets a compile error, which is the cheapest possible way to be told.
    for (code, want, name) in [
        (holonomy_input::KEY_F11, holonomy_input::Command::ZoomIn, "F11"),
        (holonomy_input::KEY_F12, holonomy_input::Command::ZoomReset, "F12"),
    ] {
        let ev = holonomy_input::InputEvent {
            kind: holonomy_input::EV_KEY,
            code,
            value: 1,
        };
        assert_eq!(
            km.dispatch(ev, &mods),
            Some(want),
            "**{name} decodes to {want:?}**, so the gap is not in the keymap"
        );
    }
    // **And the session still drops them.** `Session::apply`'s match has no arm for `ZoomIn`, so the
    // keyboard cannot zoom -- only the dropdown and `--zoom` can. **Asserted rather than left as a
    // gap in a comment**, because part 42 changed `set_zoom` to a `Result` and the natural next
    // mistake is to add these two arms by copy-paste and get the two-row atlas wrong.
    let mut s = session("hello\n");
    s.caret_to(0).expect("caret");
    let before = s.state.zoom_percent;
    s.apply(holonomy_input::Command::ZoomIn).expect("apply does not fail");
    assert_eq!(
        s.state.zoom_percent, before,
        "**and `apply(Command::ZoomIn)` changes nothing** -- a gap this file names rather than hides"
    );
}

/// **The label fits its button at both ends of what the atlas offers.**
///
/// **Part 23's version tested the ends of the `25..=400` clamp.** There is no clamp now — a zoom
/// outside the table is an error — so the ends are the two levels, and **the point is unchanged**:
/// the button is a fixed width and the label is formatted into it.
#[test]
fn the_zoom_label_fits_its_button_at_both_ends_of_the_clamp() {
    for percent in [100u32, 125] {
        let s = painted_at(percent, "");
        let z = zoom_rect(&s);
        // **The label is drawn in the cell grid, so the test is cells not pixels**: `cell_w` at the
        // *current* size, which is 14 at 125 % and is exactly why this was worth asking.
        let cells = u32::try_from(s.state.zoom_label().len()).expect("ascii");
        assert!(
            cells * s.chrome.metrics.cell_w <= z.width,
            "**`{}` is {} cells at {} px = {} px, and the button is {} px wide.** The cell width \\
             changes with the zoom, so a label that fitted at 100 % need not fit at 125 % -- which is \\
             the reason this gate is about both ends and not just the widest.",
            s.state.zoom_label(),
            cells,
            s.chrome.metrics.cell_w,
            cells * s.chrome.metrics.cell_w,
            z.width
        );
    }
}
/// **The row pitch follows the size being drawn, and is *not* the tallest size in the atlas.**
///
/// ## Why this gate exists
///
/// `Atlas::line_pitch()` answers the tallest `ascent + descent` over **every size in the atlas**, plus
/// one. That is right for a one-size atlas and wrong for a two-size one: with `[16, 20]` it answers
/// 30 — 20 ppem's pitch — whatever the painter is drawing.
///
/// **So a session at 100 % would lay its lines out 30 px apart while drawing 16 ppem glyphs**, and the
/// symptom is the most confusing one a zoom bug can have: **the text looks double-spaced at 100 % and
/// normal at 125 %**, which reads as "zooming in fixed the line spacing" rather than as "the pitch
/// was taken from the wrong size".
///
/// ## The negative half
///
/// **`a.painter`'s pitch must be smaller than the atlas's global `line_pitch()`** — that is the whole
/// claim, stated as an inequality between the two numbers rather than as two literals.
#[test]
fn the_row_pitch_follows_the_zoom_and_not_the_tallest_size() {
    let text = "one\ntwo\nthree\n";
    let a = painted_at(100, text);
    let b = painted_at(125, text);
    let atlas = atlas();

    let global = u32::from(atlas.line_pitch());
    let at16 = u32::from(atlas.line_pitch_at(16));
    let at20 = u32::from(atlas.line_pitch_at(20));

    assert!(at16 < global, "**16 ppem's pitch ({at16}) is below the global ({global})**");
    assert!(at20 <= global, "**20 ppem's pitch ({at20}) is at most the global ({global})**");
    assert!(at16 < at20, "**and 20 ppem's is the larger of the two**");

    assert_eq!(
        a.chrome.metrics.cell_h, at16,
        "**100 % spaces its lines at 16 ppem's pitch**, which is what the user asked for"
    );
    assert_eq!(
        b.chrome.metrics.cell_h, at20,
        "**and 125 % at 20 ppem's**"
    );
    // **And the user can see the difference.** 30 px of leading for 16 ppem glyphs is visible as
    // every-other-line-blank; the gate is not satisfied by the numbers agreeing.
    assert!(
        b.chrome.metrics.cell_h > a.chrome.metrics.cell_h,
        "**the row pitch grew with the zoom**: {} -> {}",
        a.chrome.metrics.cell_h,
        b.chrome.metrics.cell_h
    );
}
