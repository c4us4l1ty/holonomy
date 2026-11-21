//! Block 5's third gate, measured as far as it is true: **cluster-boundary honesty for
//! bi-directional and multi-script text**.
//!
//! The gate has two halves and they are not the same half. This file measures the one that does
//! not need a pixel:
//!
//! 1. **Grapheme honesty.** For text in scripts that are written right-to-left — Hebrew, Arabic,
//!    Thaana, Syriac, N'Ko, Adlam, the presentation forms — no caret may land inside a grapheme
//!    cluster. A caret inside a cluster is a position no renderer has, and `docs/UI-SPEC.md` §4
//!    promises carets exist only at cluster boundaries.
//! 2. **The displacement, printed.** Visual order for RTL text is *not* implemented (OQ-11). Rather
//!    than assert that silently, this file measures how far the placement is from what a bidi
//!    renderer would draw and prints it as `HOLONOMY_SCRIPT_GATE`. The number is the evidence for
//!    the deferral: it is large and it is stated, not hidden.
//!
//! # Why the first half is a real test and not a formality
//!
//! The property data is generated from UCD 18.0.0 (`cluster/tables.rs`, D-27), and the failure this
//! guards against is the one OQ-11 used to carry: a mark from a script nobody thought of splits
//! from its base, and the seam becomes a caret position. Arabic and Hebrew vowel marks are
//! `gcb=Extend`, so they must join; Hebrew and Arabic **letters** are their own clusters, because
//! the scripts are cursive-joined at paint time and not at cluster time. Both directions are
//! asserted here, because a table that over-joins is as wrong as one that under-joins.
//!
//! # What this file does not do
//!
//! It does not implement or imitate bidi reordering. It records that the placement is logical
//! order (left to right) and measures the consequence. When OQ-11's remaining half lands, the
//! displacement count below becomes zero and this file's assertion inverts — the point of
//! measuring it now.

use holonomy_eye::{layout_window, Clusters, Fixed, Layout, Shaper, Style, View};

/// A deterministic fake shaper. Uneven advances, and — deliberately — the RTL letters are *not*
/// one fixed width, so a bug that assumed a uniform advance cannot pass.
struct Fake;

impl Shaper for Fake {
    fn advance(&self, cluster: &str, style: Style) -> Fixed {
        let base = match cluster.chars().next() {
            Some('m') | Some('\u{5de}') | Some('\u{645}') => Fixed::units(2),
            Some('i') | Some('l') | Some('\u{5d9}') | Some('\u{627}') => Fixed::from_64ths(32),
            _ => Fixed::ONE,
        };
        Fixed::from_64ths(base.sixtyfourths() * style.size_milli() / 1000)
    }

    fn line_height(&self, style: Style) -> Fixed {
        Fixed::from_64ths(Fixed::units(20).sixtyfourths() * style.size_milli() / 1000)
    }

    fn ascent(&self, style: Style) -> Fixed {
        Fixed::from_64ths(Fixed::units(15).sixtyfourths() * style.size_milli() / 1000)
    }
}

/// (label, text, expected cluster count). The expected count is the answer the *standard* gives, so
/// the table is checked against UAX #29 rather than against whatever the code happens to do.
const CORPUS: [(&str, &str, usize); 14] = [
    // Hebrew: letters are separate clusters; points (niqqud) are Extend and join their base.
    ("hebrew letters", "\u{5e9}\u{5dc}\u{5d5}\u{5dd}", 4),
    ("hebrew with point", "\u{5e9}\u{5b8}", 1),
    (
        "hebrew word + point",
        "\u{5e9}\u{5dc}\u{5d5}\u{5b9}\u{5dd}",
        4,
    ),
    ("hebrew accents stack", "\u{5d0}\u{5b8}\u{5bc}\u{5b8}", 1),
    // Arabic: letters separate; harakat (0x64B-0x65F) are Extend and join.
    (
        "arabic letters",
        "\u{627}\u{644}\u{639}\u{631}\u{628}\u{64a}\u{629}",
        7,
    ),
    ("arabic with fatha", "\u{628}\u{64e}", 1),
    ("arabic shadda+fatha", "\u{628}\u{651}\u{64e}", 1),
    ("arabic word", "\u{645}\u{631}\u{62d}\u{628}\u{627}", 5),
    // Thaana, Syriac, N'Ko, Adlam: RTL scripts outside the two everyone remembers.
    // U+07A9 THAANA EEBEEFILI is Mn: it is Extend and joins U+0783, so three code
    // points are two clusters. This line first asserted 3 — the code was right and the
    // test was wrong, which is exactly what an independently-checked table is for.
    ("thaana", "\u{780}\u{783}\u{7a9}", 2),
    ("syriac with point", "\u{710}\u{730}", 1),
    ("nko", "\u{7ca}\u{7d4}", 2),
    ("adlam", "\u{1e900}\u{1e922}", 2),
    // A mixed line: the interesting case for ordering, and for the count.
    (
        "mixed latin hebrew",
        "abc \u{5e9}\u{5dc}\u{5d5}\u{5dd} def",
        12,
    ),
    // Arabic presentation forms are their own code points and must segment, not vanish.
    ("arabic presentation", "\u{fe8f}\u{fe9f}", 2),
];

/// Lays `text` out wide enough that it never wraps, at zoom 1 and 96 dpi.
fn lay(text: &str) -> (Layout<'_>, View) {
    let width = Fixed::units(200);
    (
        layout_window(text, 0, width, &Fake),
        View::new(1000, 96).expect("a valid view"),
    )
}

#[test]
fn rtl_scripts_never_offer_a_caret_inside_a_grapheme() {
    let mut clusters_checked = 0u64;
    let mut carets_checked = 0u64;
    let mut scripts = 0u64;

    for (label, text, expected) in CORPUS {
        // The count is the standard's answer, asserted before any geometry runs.
        let segmented: Vec<&str> = Clusters::new(text).map(|(_, cluster)| cluster).collect();
        assert_eq!(
            segmented.len(),
            expected,
            "{label}: {segmented:?} is not the UAX #29 segmentation"
        );
        clusters_checked += segmented.len() as u64;
        scripts += 1;

        let (laid, _) = lay(text);
        let line = laid.lines().first().expect("a laid-out line");

        // Every caret the layout offers must sit at a cluster boundary.
        let begins: Vec<u64> = line.clusters().iter().map(|cluster| cluster.byte).collect();
        for (_, byte) in line.boundaries() {
            let at_boundary = begins.contains(&byte) || byte == line.end_byte();
            assert!(
                at_boundary,
                "{label}: caret at byte {byte} is not at a cluster boundary of {begins:?}"
            );
            carets_checked += 1;
        }

        // And the placed clusters must cover the text exactly, in logical order — the property
        // that makes the segmentation reach the screen at all.
        let placed: String = line.clusters().iter().map(|cluster| cluster.text).collect();
        assert_eq!(
            placed, text,
            "{label}: placed clusters do not cover the text"
        );
    }

    println!(
        "HOLONOMY_SCRIPT_GATE_HONESTY scripts={scripts} clusters={clusters_checked} \
         carets={carets_checked} carets_inside_a_grapheme=0"
    );
    assert_eq!(
        scripts,
        CORPUS.len() as u64,
        "every corpus line was checked"
    );
}

#[test]
fn an_rtl_letter_input_segments_the_standard_way() {
    // The direction this test pins: an RTL letter is its own cluster (cursive joining is a paint
    // concern), and a mark on it is not. Over-joining is the failure mode a `Prepend`/`Extend` bug
    // would cause, and it would silently delete a caret position a user needed.
    let word = "\u{5e9}\u{5dc}\u{5d5}\u{5dd}"; // shalom
    let clusters: Vec<&str> = Clusters::new(word).map(|(_, c)| c).collect();
    assert_eq!(
        clusters,
        vec!["\u{5e9}", "\u{5dc}", "\u{5d5}", "\u{5dd}"],
        "Hebrew letters are four clusters, not one cursive run"
    );

    let pointed = "\u{5e9}\u{5b8}"; // shin + qamats
    let clusters: Vec<&str> = Clusters::new(pointed).map(|(_, c)| c).collect();
    assert_eq!(
        clusters,
        vec!["\u{5e9}\u{5b8}"],
        "a niqqud point is Extend: it joins its base and is never a caret position"
    );
}

#[test]
fn rtl_text_is_placed_in_logical_order_and_the_displacement_is_measured() {
    // This is the honest half. Bidi reordering is not implemented (OQ-11), so a pure RTL run is
    // placed left-to-right: the first logical character is the leftmost cluster, where a bidi
    // renderer would put it rightmost. The count below is the size of that deferral.
    let hebrew = "\u{5e9}\u{5dc}\u{5d5}\u{5dd}"; // shalom, 4 clusters
    let laid = layout_window(hebrew, 0, Fixed::units(200), &Fake);
    let line = laid.lines().first().expect("a laid-out line");

    let xs: Vec<Fixed> = line.clusters().iter().map(|cluster| cluster.x).collect();
    assert!(
        xs.windows(2).all(|pair| pair[0] < pair[1]),
        "clusters advance left to right: {xs:?}"
    );

    // A bidi renderer reverses the run. Count the clusters whose visual position therefore differs
    // from the placement, which is every one of them in a pure RTL run.
    let reversed: Vec<Fixed> = xs.iter().rev().copied().collect();
    let displaced = xs
        .iter()
        .zip(&reversed)
        .filter(|(logical, visual)| logical != visual)
        .count();

    println!(
        "HOLONOMY_SCRIPT_GATE_ORDER scripts_deferred=rtl clusters={} displaced={displaced} \
         note=logical-order-placement",
        xs.len()
    );
    assert_eq!(
        displaced,
        xs.len(),
        "every cluster in a pure RTL run sits where a bidi renderer would not put it"
    );
}

#[test]
fn an_rtl_caret_hit_tests_to_the_boundary_it_painted() {
    // Even without reordering, L-8 must hold for RTL text: the caret a click chooses is the caret
    // that paints. This is what keeps the deferral from being two bugs instead of one.
    let text = "abc \u{5e9}\u{5dc}\u{5d5}\u{5dd} def";
    let (laid, view) = lay(text);

    let mut checked = 0u64;
    for line in laid.lines() {
        for (_, byte) in line.boundaries() {
            // The round trip is paint's own point, not a boundary recomputed here: `hit_test` takes
            // device-space coordinates, and going through `paint_point` is what makes this an
            // actual inverse rather than the same arithmetic asserted twice.
            let Some(painted) = view.paint_point(&laid, byte) else {
                continue;
            };
            let hit = view
                .hit_test(&laid, painted.point())
                .unwrap_or_else(|| panic!("paint's own point must hit-test, byte {byte}"));
            assert_eq!(
                hit.byte, byte,
                "an RTL boundary {byte} did not hit-test back"
            );
            assert_eq!(
                hit.x, painted.x,
                "the hit-tested caret moved for byte {byte}"
            );
            checked += 1;
        }
    }

    println!("HOLONOMY_SCRIPT_GATE_L8 boundaries={checked} disagreements=0");
    assert!(checked > 0, "the corpus laid out no boundaries to check");
}
