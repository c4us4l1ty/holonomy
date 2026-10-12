//! The opening size of an unsized window.
//!
//! # Why this file exists at all
//!
//! **The rule used to be four lines of arithmetic inside `windowed::run`.** Parts 36 and 37 both got
//! it wrong in the same way and neither could be caught: part 36 changed it from a fixed 1,600 x 1,000
//! to "four fifths of the screen" in answer to *"that 1020x700 is WAY too small"*, part 37 changed
//! that to "ninety per cent of the screen" in answer to a screenshot of a 1,600 px window on a
//! 3,072 px desktop — **and the only way to find out what the app would open at was to open it and
//! look.** Two changes, two reports, one measurement each, and no gate in between.
//!
//! `windowed::opening_size` is now a pure function and this file is the gate that runs it on every
//! machine, with no display and no window manager involved.
//!
//! # The measurement that settled what "too small" meant
//!
//! With the window at ninety per cent of a 3,072 x 1,920 screen, the app's own startup line reported
//!
//! ```text
//! holonomy: window 0xc00001 is 2764x1728, ... 89% x 90% of the screen
//! ```
//!
//! — **correct, and still reported as "way too small".** The window was not the problem. The page
//! inside it was 896 px wide in a 2,764 px window, because `max_columns: 100` caps the measure and at
//! 16 ppem a column is 8 px, so the page is 896 px *however wide the window is*. Resizing wider only
//! grows the empty desk, which is the whole of *"I can resize it and it is way too small."*

// **`windowed` is behind the `desktop` feature**, so the whole file is. A gate that silently does
// nothing when the feature is off is worse than no gate, so it is gated explicitly and the reason is
// written where a reader meets it.
#![cfg(feature = "desktop")]

use holonomy::windowed::{opening_size, DESIGN_H, DESIGN_W, SCREEN_MARGIN};

/// **The window opens at the design size, and the design ratio holds.** 1 test.
///
/// The thing being held constant is not the pixel size, it is **the share of the window the page
/// occupies** — 58% at the 1,280 px design width and 56% at 1,600 px. At ninety per cent of a 3,072 px
/// screen the same window gave 32%, and that gap is the report.
#[test]
fn the_opening_size_is_the_design_size_on_a_big_screen() {
    use holonomy_render::chrome::{ChromeMetrics, Layout};

    for screen in [(3072u32, 1920u32), (2560, 1440), (1920, 1080), (3840, 2160)] {
        let (w, h) = opening_size(screen, None);
        assert_eq!(
            (w, h),
            (DESIGN_W, DESIGN_H),
            "{screen:?}: a screen this size opens at the design size, not a fraction of itself"
        );

        let l = Layout::new(&ChromeMetrics::for_size(w, h));
        let share = 100 * l.page.width / w;
        assert!(
            (50..=65).contains(&share),
            "{screen:?}: the page is {share}% of the {w}px window; below ~50% the window is mostly \\
             desk, which is what \"too small\" was"
        );
    }
}

/// **The window never exceeds the screen.** 1 test.
///
/// **This is the half that the fixed size got wrong, and it is a gate rather than a hope.** A
/// constant 1,600 px window on an 800 px screen is a window bigger than the display — the same class
/// of defect as the layout clamp in part 38, where the chrome was laid out wider than the window it
/// was drawn into. The screen is the ceiling; `SCREEN_MARGIN` is what is left of it.
#[test]
fn the_window_never_exceeds_the_screen() {
    for screen in [
        (3072u32, 1920u32),
        (1920, 1080),
        (1366, 768),
        (1024, 768),
        (800, 600),
        (400, 300),
        (200, 150),
        (64, 64),
        (1, 1),
    ] {
        let (w, h) = opening_size(screen, None);
        assert!(
            w <= screen.0 && h <= screen.1,
            "{screen:?}: opened at {w}x{h}, which is larger than the screen"
        );
        assert!(w >= 1 && h >= 1, "{screen:?}: and a window must have a positive extent");
    }
}

/// **A screen with no extent is a screen that was not read.** 1 test.
///
/// `screen_size` answers `None` when the connection fails and `Some((0, 0))` when the setup carries
/// nothing; both reach `opening_size`, and the window is about to be created blind. **The honest
/// answer to "I do not know how big this is" is the size the app wants** — a 1 x 1 window is the one
/// size that is certainly wrong, and it is what `min(..).max(1)` produced before this rule.
#[test]
fn an_unreadable_screen_opens_at_the_design_size_not_one_pixel() {
    assert_eq!(opening_size((0, 0), None), (DESIGN_W, DESIGN_H));
    assert_eq!(opening_size((0, 1920), None).0, DESIGN_W, "a zero width alone is enough");
    assert_eq!(opening_size((3072, 0), None).1, DESIGN_H, "and a zero height alone");
}

/// **The minimum is honoured, except where the screen cannot give it.** 1 test.
///
/// **`floor.min(room)`, not `floor`.** Asking a window manager for a 768 px minimum on a 400 px
/// screen is a request it will refuse, and the answer has to be the screen's — the same rule as
/// `ChromeMetrics::for_size`, where a minimum larger than the window is the bug rather than the floor.
#[test]
fn the_minimum_is_honoured_where_the_screen_allows_it() {
    use holonomy_render::chrome::ChromeMetrics;
    let (MIN_WIDTH, MIN_HEIGHT) = (ChromeMetrics::MIN_WIDTH, ChromeMetrics::MIN_HEIGHT);

    // A screen with room for the minimum: the minimum is the floor.
    let (w, h) = opening_size((1920, 1080), Some((MIN_WIDTH, MIN_HEIGHT)));
    assert_eq!((w, h), (DESIGN_W, DESIGN_H), "a big screen opens at the design size");

    // A screen without room: the screen wins, and the floor does not push past it.
    let (w, h) = opening_size((400, 300), Some((MIN_WIDTH, MIN_HEIGHT)));
    assert!(
        w <= 400 && h <= 300,
        "a 400x300 screen opened at {w}x{h}: **the minimum must not push the window past the \\
         display**, which is how it was clamped before"
    );
    assert!(w >= 1 && h >= 1);

    // Exactly the screen, no room for a margin: one pixel, not the minimum.
    let (w, h) = opening_size((SCREEN_MARGIN, SCREEN_MARGIN), Some((MIN_WIDTH, MIN_HEIGHT)));
    assert_eq!((w, h), (1, 1), "a screen the size of the margin leaves a 1px window, honestly");
}

/// **The margin is what it says it is, and it is not a fraction of the screen.** 1 test.
///
/// A margin that scales with the screen is a bigger margin on a bigger screen, **and the thing being
/// protected is a window manager's title bar, which is a fixed number of pixels everywhere.** 64 is
/// also a multiple of both the 8 px cell and the 16 px ppem, so the chrome's arithmetic stays on a
/// cell boundary.
#[test]
fn the_margin_is_a_fixed_number_of_pixels() {
    assert_eq!(SCREEN_MARGIN, 64);
    assert_eq!(SCREEN_MARGIN % 8, 0, "a whole number of text cells");
    assert_eq!(SCREEN_MARGIN % 16, 0, "and of ppem");

    // Below the margin the window is what is left of the screen, and the answer falls with it.
    let (big, _) = opening_size((2000, 2000), None);
    let (small, _) = opening_size((600, 2000), None);
    assert_eq!(big, DESIGN_W, "plenty of room: the design width");
    assert_eq!(small, 600 - SCREEN_MARGIN, "not enough: the screen minus the margin");
}
