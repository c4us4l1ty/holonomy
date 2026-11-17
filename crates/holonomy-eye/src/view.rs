//! Zoom and DPI as a pure view transform, shared by both consumers.
//!
//! RFC-004 decision 5: the run tree is logical-unit, and the paint layer scales. The failure this
//! file exists to prevent is the obvious wrong way to build that: a paint path that multiplies
//! logical coordinates by a scale factor, and a hit-test path that divides device coordinates by
//! it. Two roundings, two answers, and zoom becomes the seam the whole block exists to close.
//!
//! So there is **no inverse here**, on purpose. [`View::to_device`] is the only mapping in the
//! crate, and [`View::hit_test`] does not map a device point back into layout space — it compares
//! the device point against the device-space positions paint would draw, computed by that same
//! mapping. A caret produced by [`View::paint_point`] therefore hit-tests back to a caret that
//! paints at exactly the same device position, at every zoom and DPI, without any arithmetic
//! having to undo another arithmetic.
//!
//! The transform is exact rational integer scaling:
//!
//! ```text
//! device = round_half_away_from_zero(logical × zoom_milli × dpi / 96_000)
//! ```
//!
//! `zoom_milli` is zoom in thousandths (1000 = 100 %) and `dpi` is the device's dots per inch
//! against the 96 dpi reference the logical unit is defined at, so a 144 dpi display at 100 %
//! needs no zoom of its own to look right.
//!
//! Two consequences, both stated rather than hidden:
//!
//! * A **compressed** view can map two cluster boundaries onto one device position. That is not a
//!   seam — both consumers read the same collapsed position — but it does mean a click there
//!   cannot distinguish the two carets, and the tie rule picks the earlier one. Zero-advance
//!   clusters (marks, in a real font) do the same at every zoom.
//! * Advance rectangles are rounded independently, so a row of them can be off by one 64th from a
//!   single rounded rectangle. Text rendering snaps to pixels; the caret geometry, which is what
//!   L-8 quantifies over, is built from boundary positions and is unaffected.

use crate::geom::{Fixed, Point};
use crate::layout::{Caret, Layout, Line, Placed};

/// The reference DPI a logical unit is defined at.
const REFERENCE_DPI: i128 = 96;

/// The number of thousandths in one whole zoom step.
const ZOOM_ONE: i64 = 1000;

/// A zoom/DPI transform from logical units to device units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct View {
    zoom_milli: i64,
    dpi: i64,
}

impl View {
    /// The identity view: 100 % at the 96 dpi reference. `to_device` is exact for it, which is why
    /// the layout's own coordinate space can be expressed as a view rather than as a second
    /// implementation.
    pub const IDENTITY: Self = Self {
        zoom_milli: ZOOM_ONE,
        dpi: 96,
    };

    /// A view at `zoom_milli` thousandths of 100 % on a `dpi` display.
    ///
    /// Both must be positive; anything else is not a view of anything.
    #[must_use]
    pub fn new(zoom_milli: i64, dpi: i64) -> Option<Self> {
        if zoom_milli <= 0 || dpi <= 0 {
            return None;
        }
        Some(Self { zoom_milli, dpi })
    }

    /// Zoom in thousandths of 100 % (1000 = 100 %).
    #[must_use]
    pub fn zoom_milli(&self) -> i64 {
        self.zoom_milli
    }

    /// The display's dots per inch.
    #[must_use]
    pub fn dpi(&self) -> i64 {
        self.dpi
    }

    /// Maps a logical coordinate to device space.
    ///
    /// This is the entire transform. Both consumers call it, and neither calls anything else.
    ///
    /// Sizes that overflow the 64-bit range saturate rather than wrap: a coordinate that large is
    /// nowhere near a viewport, and a saturated number stays visibly absurd where a wrapped one
    /// would look like geometry.
    #[must_use]
    pub fn to_device(&self, value: Fixed) -> Fixed {
        let numerator = i128::from(self.zoom_milli) * i128::from(self.dpi);
        Fixed::from_64ths(round_ratio(value.sixtyfourths(), numerator))
    }

    /// The height of the laid-out content, in device space.
    #[must_use]
    pub fn height(&self, layout: &Layout<'_>) -> Fixed {
        self.to_device(layout.height())
    }

    /// The width the text was wrapped to, in device space.
    #[must_use]
    pub fn width(&self, layout: &Layout<'_>) -> Fixed {
        self.to_device(layout.width())
    }

    /// Every placed cluster in the window, in device space, with its line's device-space top.
    ///
    /// This is what paint draws. It is built from the same run tree hit-testing walks, so a
    /// renderer cannot see a different set of glyphs from the one a click resolves against.
    #[must_use]
    pub fn clusters<'a>(&self, layout: &Layout<'a>) -> Vec<(Fixed, Placed<'a>)> {
        let mut placed = Vec::new();
        for line in layout.lines() {
            let top = self.to_device(line.top());
            for cluster in line.clusters() {
                placed.push((
                    top,
                    Placed {
                        byte: cluster.byte,
                        x: self.to_device(cluster.x),
                        advance: self.to_device(cluster.advance),
                        style: cluster.style,
                        text: cluster.text,
                    },
                ));
            }
        }
        placed
    }

    /// The caret at document byte `byte`, in device space — the paint side of L-8.
    ///
    /// `None` when `byte` is not a caret position in this window at all (inside a cluster, or
    /// outside the window).
    #[must_use]
    pub fn paint_point(&self, layout: &Layout<'_>, byte: u64) -> Option<Caret> {
        for (index, line) in layout.lines().iter().enumerate() {
            for (x, at) in device_boundaries(self, line) {
                if at == byte {
                    return Some(self.caret_at(byte, index, x, line));
                }
            }
        }
        None
    }

    /// The caret nearest to `point`, which is in **device** space — the click side of L-8.
    ///
    /// The point is never mapped back into layout space: the comparison happens between the point
    /// and the device positions paint would draw, so the two sides cannot round differently. Ties
    /// go to the earlier boundary, exactly as they do in layout space, and `y` is clamped into the
    /// content. A line box is mapped as a whole — `to_device(top + height)`, not
    /// `to_device(top) + to_device(height)` — so one line's bottom is exactly the next line's top
    /// and a caret's own `y` always selects its own line.
    #[must_use]
    pub fn hit_test(&self, layout: &Layout<'_>, point: Point) -> Option<Caret> {
        let lines = layout.lines();
        if lines.is_empty() {
            return None;
        }
        let mut index = lines.len() - 1;
        for (candidate, line) in lines.iter().enumerate() {
            let bottom = self.to_device(line.top() + line.height());
            debug_assert!(
                bottom > self.to_device(line.top()),
                "a line box that maps to zero device units cannot be hit-tested"
            );
            if point.y < bottom {
                index = candidate;
                break;
            }
        }
        let line = &lines[index];
        let mut best: Option<(Fixed, Fixed, u64)> = None;
        for (x, byte) in device_boundaries(self, line) {
            let distance = if point.x < x {
                x - point.x
            } else {
                point.x - x
            };
            match best {
                Some((closest, _, _)) if distance >= closest => {}
                _ => best = Some((distance, x, byte)),
            }
        }
        let (_, x, byte) = best?;
        Some(self.caret_at(byte, index, x, line))
    }

    /// A caret at `x` on `line`, with the line box mapped into device space the way paint draws it.
    ///
    /// The height is the distance between the mapped top and the mapped bottom, so the box a
    /// renderer draws is exactly the box [`View::hit_test`] tested against.
    fn caret_at(&self, byte: u64, line: usize, x: Fixed, rect: &Line<'_>) -> Caret {
        let top = self.to_device(rect.top());
        let bottom = self.to_device(rect.top() + rect.height());
        Caret {
            byte,
            line,
            x,
            top,
            height: bottom - top,
        }
    }
}

/// The line's caret positions in device space: the one list both directions walk.
fn device_boundaries(view: &View, line: &Line<'_>) -> Vec<(Fixed, u64)> {
    let mut boundaries = Vec::new();
    for (x, byte) in line.boundaries() {
        boundaries.push((view.to_device(x), byte));
    }
    boundaries
}

/// `value × numerator / 96_000`, rounded half away from zero, in exact integer arithmetic.
///
/// The denominator is the reference DPI times one whole zoom step, so the product always divides
/// a 64th of a logical unit by a number of device 64ths; `i128` holds every product the document
/// offsets can produce.
fn round_ratio(value: i64, numerator: i128) -> i64 {
    let denominator = REFERENCE_DPI * i128::from(ZOOM_ONE);
    let Some(product) = i128::from(value).checked_mul(numerator) else {
        return saturate(value);
    };
    let half = denominator / 2;
    let adjusted = if product < 0 {
        product.saturating_sub(half)
    } else {
        product.saturating_add(half)
    };
    let rounded = adjusted / denominator;
    let too_large = rounded > i128::from(i64::MAX);
    let too_small = rounded < i128::from(i64::MIN);
    if too_large {
        return i64::MAX;
    }
    if too_small {
        return i64::MIN;
    }
    rounded as i64
}

/// The end of the representable range in `value`'s direction.
///
/// A coordinate that saturates is already outside every viewport, and saturating keeps it obvious;
/// wrapping would hand back a plausible-looking number instead.
fn saturate(value: i64) -> i64 {
    if value < 0 {
        return i64::MIN;
    }
    i64::MAX
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::layout_window;
    use crate::shaper::Shaper;
    use crate::style::Style;
    use crate::test_support::layout;

    /// A shaper whose clusters can have zero width, as marks do in a real font.
    struct Marks;

    impl Shaper for Marks {
        fn advance(&self, cluster: &str, _style: Style) -> Fixed {
            if cluster == "i" {
                Fixed::ZERO
            } else {
                Fixed::ONE
            }
        }

        fn line_height(&self, _style: Style) -> Fixed {
            Fixed::units(20)
        }

        fn ascent(&self, _style: Style) -> Fixed {
            Fixed::units(15)
        }
    }

    fn corpus() -> Vec<&'static str> {
        vec![
            "abc",
            "hello world",
            "héllo",
            "e\u{301}x",
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
            "a\n\nb",
            "line one\nline two\n",
            "mimimimi",
            "山田太郎",
            "",
        ]
    }

    fn device(view: &View, sixtyfourths: i64) -> i64 {
        view.to_device(Fixed::from_64ths(sixtyfourths))
            .sixtyfourths()
    }

    /// The zoom tick and the DPI grid every property test sweeps.
    fn zooms() -> Vec<i64> {
        vec![250, 500, 750, 1000, 1250, 1500, 2000, 3000]
    }

    fn dpis() -> Vec<i64> {
        vec![96, 120, 144, 192, 288]
    }

    #[test]
    fn the_transform_rounds_half_away_from_zero() {
        let doubled = View::new(1000, 192).expect("a positive view");
        assert_eq!(device(&doubled, 1), 2);
        assert_eq!(device(&doubled, -1), -2);
        assert_eq!(device(&doubled, 0), 0);

        // 100 % at 144 dpi is exactly one and a half device units per logical unit.
        let three_halves = View::new(1000, 144).expect("a positive view");
        assert_eq!(device(&three_halves, 1), 2);
        assert_eq!(device(&three_halves, 3), 5);
        assert_eq!(device(&three_halves, -3), -5);

        let halved = View::new(500, 96).expect("a positive view");
        assert_eq!(device(&halved, 1), 1);
        assert_eq!(device(&halved, 3), 2);
        assert_eq!(device(&halved, -3), -2);
    }

    #[test]
    fn the_identity_view_is_exact() {
        for value in [-192, -1, 0, 1, 65, 4096] {
            let fixed = Fixed::from_64ths(value);
            assert_eq!(View::IDENTITY.to_device(fixed), fixed);
        }
    }

    #[test]
    fn an_absurd_zoom_saturates_instead_of_wrapping() {
        let absurd = View::new(i64::MAX, i64::MAX).expect("positive");
        assert_eq!(absurd.to_device(Fixed::ONE), Fixed::from_64ths(i64::MAX));
        let negative = Fixed::from_64ths(-64);
        assert_eq!(absurd.to_device(negative), Fixed::from_64ths(i64::MIN));
        // A zoom whose product still fits in 128 bits, but whose result does not fit in 64,
        // clamps rather than truncating the cast.
        let large = View::new(4_000_000_000_000_000_000, 4096).expect("positive");
        assert_eq!(large.to_device(Fixed::ONE), Fixed::from_64ths(i64::MAX));
    }

    #[test]
    fn a_view_needs_a_positive_zoom_and_dpi() {
        assert!(View::new(0, 96).is_none());
        assert!(View::new(1000, 0).is_none());
        assert!(View::new(-100, 96).is_none());
        assert!(View::new(1000, -96).is_none());
        assert!(View::new(1000, 96).is_some());
    }

    #[test]
    fn the_identity_view_answers_exactly_as_the_layout_does() {
        let text = "héllo world";
        let laid = layout(text, 4);
        for byte in 0..=text.len() as u64 {
            assert_eq!(
                View::IDENTITY.paint_point(&laid, byte),
                laid.paint_point(byte)
            );
        }
        for x in [-64, 0, 64, 200, 4096] {
            for y in [-64, 0, 640, 4096] {
                let point = Point::new(Fixed::from_64ths(x), Fixed::from_64ths(y));
                assert_eq!(View::IDENTITY.hit_test(&laid, point), laid.hit_test(point));
            }
        }
    }

    #[test]
    fn line_boxes_and_caret_heights_scale_together() {
        let laid = layout("ab\ncd", 10);
        let view = View::new(1000, 192).expect("a positive view");
        let caret = view.paint_point(&laid, 3).expect("the second line's start");
        assert_eq!(caret.line, 1);
        assert_eq!(caret.top, Fixed::units(40));
        assert_eq!(caret.height, Fixed::units(40));
        assert_eq!(view.height(&laid), Fixed::units(80));
        assert_eq!(laid.height(), Fixed::units(40));
    }

    #[test]
    fn placed_clusters_scale_without_being_reshaped() {
        let laid = layout("abc", 10);
        let view = View::new(1000, 192).expect("a positive view");
        let placed = view.clusters(&laid);
        assert_eq!(placed.len(), 3);
        let (top, cluster) = placed[1];
        assert_eq!(top, Fixed::ZERO);
        assert_eq!(cluster.text, "b");
        assert_eq!(cluster.x, Fixed::units(2));
        assert_eq!(cluster.advance, Fixed::units(2));
    }

    #[test]
    fn every_caret_round_trips_at_every_zoom_and_dpi() {
        for text in corpus() {
            for width in [3, 10, 40] {
                let laid = layout(text, width);
                for zoom in zooms() {
                    for dpi in dpis() {
                        let view = View::new(zoom, dpi).expect("a positive view");
                        assert_round_trip(&view, &laid, text);
                    }
                }
            }
        }
    }

    #[test]
    fn a_zero_width_cluster_answers_with_the_earliest_caret_that_paints_there() {
        // Two boundaries at one device position cannot be told apart by a click. The tie rule
        // picks the earlier one, so the answer is deterministic at every zoom — it is a caret
        // that paints exactly where the click landed, even when it is not the one you painted.
        let laid = layout_window("iii", 0, Fixed::units(10), &Marks);
        for zoom in zooms() {
            let view = View::new(zoom, 96).expect("a positive view");
            assert!(!device_boundaries_are_distinct(&view, &laid));
            let painted = view.paint_point(&laid, 2).expect("a caret");
            let hit = view.hit_test(&laid, painted.point()).expect("a hit");
            assert_eq!(hit.byte, 0);
            assert_eq!(hit.x, painted.x);
        }
    }

    #[test]
    fn the_corpus_is_long_enough_to_wrap_and_to_break() {
        // Guards the two property tests above: if the corpus ever stops producing multiple lines,
        // they would still pass while testing almost nothing.
        let mut wrapped = 0;
        for text in corpus() {
            for width in [3, 10] {
                if layout(text, width).lines().len() > 1 {
                    wrapped += 1;
                }
            }
        }
        assert!(wrapped > 4);
    }

    /// L-8 at one zoom and one DPI: every caret survives the trip through the other side.
    fn assert_round_trip(view: &View, laid: &Layout<'_>, text: &str) {
        let zoom = view.zoom_milli();
        let dpi = view.dpi();
        let distinct = device_boundaries_are_distinct(view, laid);
        let mut checked = 0;
        for byte in 0..=text.len() as u64 {
            let Some(caret) = view.paint_point(laid, byte) else {
                continue;
            };
            let back = view
                .hit_test(laid, caret.point())
                .expect("paint's own point must hit-test");
            assert_eq!(back.x, caret.x, "zoom {zoom} dpi {dpi} byte {byte}");
            assert_eq!(back.line, caret.line, "zoom {zoom} dpi {dpi}");
            assert_eq!(back.height, caret.height, "zoom {zoom} dpi {dpi}");
            if distinct {
                assert_eq!(back.byte, byte, "zoom {zoom} dpi {dpi}");
            }
            checked += 1;
        }
        assert!(checked > 0, "zoom {zoom} dpi {dpi} checked nothing");
    }

    /// Whether every caret position on every line has its own device position.
    ///
    /// When this holds, the round trip must return the *same byte*, not merely a caret that paints
    /// at the same place. It holds at zoom ≥ 100 % for the fake shaper's advances, and it is
    /// asserted for that range so a rounding regression cannot quietly weaken the property test.
    fn device_boundaries_are_distinct(view: &View, laid: &Layout<'_>) -> bool {
        let mut distinct = true;
        for line in laid.lines() {
            let boundaries = device_boundaries(view, line);
            for pair in boundaries.windows(2) {
                if pair[0].0 == pair[1].0 {
                    distinct = false;
                }
            }
        }
        distinct
    }

    #[test]
    fn distinctness_holds_from_one_hundred_percent_up() {
        for text in corpus() {
            let laid = layout(text, 10);
            for zoom in [1000, 1250, 2000, 3000] {
                for dpi in dpis() {
                    let view = View::new(zoom, dpi).expect("a positive view");
                    assert!(device_boundaries_are_distinct(&view, &laid));
                }
            }
        }
    }
}
