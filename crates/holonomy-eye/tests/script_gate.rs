//! Block 5's third gate: **cluster-boundary honesty and UAX #9 visual ordering for
//! bi-directional and multi-script text** (AUDIT D-66, OQ-11).
//!
//! Both halves of the gate are asserted here:
//!
//! 1. **Grapheme honesty (`HOLONOMY_SCRIPT_GATE_HONESTY`).** Across seven right-to-left scripts —
//!    Hebrew, Arabic, Thaana, Syriac, N'Ko, Adlam, and the Arabic presentation forms — every
//!    offered caret sits at a UAX #29 extended-grapheme-cluster boundary, never inside a base+mark
//!    sequence, while unjoined letters stay distinct clusters.
//! 2. **UAX #9 visual ordering (`HOLONOMY_SCRIPT_GATE_ORDER`).** Pure RTL runs reverse
//!    right-to-left, mixed LTR/RTL runs reorder only the embedded run, Arabic numbers and paired
//!    brackets obey `W2`/`N0`/`I1–I2`/`L1–L2`, and `displaced` against expected visual order is
//!    **zero**.
//! 3. **Single-geometry round trip (`HOLONOMY_SCRIPT_GATE_L8`).** Across the 14-script corpus,
//!    mixed-direction bracket/number/isolate lines, and seeded generated bidirectional documents
//!    at three wrap widths and all 8 zooms × 5 DPIs, every painted caret hit-tests back to itself
//!    (`hit_test(paint_point(c)) == c`).

use holonomy_eye::{layout_window, Clusters, Fixed, Layout, Point, Shaper, Style, View};

/// A deterministic fake shaper with deliberately uneven RTL and LTR advances so a bug that
/// assumed uniform glyph widths cannot pass.
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

/// `(label, text, expected_cluster_count)` checked against UAX #29 over UCD 18.0.0.
const CORPUS: [(&str, &str, usize); 14] = [
    ("hebrew letters", "\u{5e9}\u{5dc}\u{5d5}\u{5dd}", 4),
    ("hebrew with point", "\u{5e9}\u{5b8}", 1),
    (
        "hebrew word + point",
        "\u{5e9}\u{5dc}\u{5d5}\u{5b9}\u{5dd}",
        4,
    ),
    ("hebrew accents stack", "\u{5d0}\u{5b8}\u{5bc}\u{5b8}", 1),
    (
        "arabic letters",
        "\u{627}\u{644}\u{639}\u{631}\u{628}\u{64a}\u{629}",
        7,
    ),
    ("arabic with fatha", "\u{628}\u{64e}", 1),
    ("arabic shadda+fatha", "\u{628}\u{651}\u{64e}", 1),
    ("arabic word", "\u{645}\u{631}\u{62d}\u{628}\u{627}", 5),
    ("thaana", "\u{780}\u{783}\u{7a9}", 2),
    ("syriac with point", "\u{710}\u{730}", 1),
    ("nko", "\u{7ca}\u{7d4}", 2),
    ("adlam", "\u{1e900}\u{1e922}", 2),
    (
        "mixed latin hebrew",
        "abc \u{5e9}\u{5dc}\u{5d5}\u{5dd} def",
        12,
    ),
    ("arabic presentation", "\u{fe8f}\u{fe9f}", 2),
];

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

        let begins: Vec<u64> = line.clusters().iter().map(|cluster| cluster.byte).collect();
        for (_, byte) in line.boundaries() {
            let at_boundary = begins.contains(&byte) || byte == line.end_byte();
            assert!(
                at_boundary,
                "{label}: caret at byte {byte} is not at a cluster boundary of {begins:?}"
            );
            carets_checked += 1;
        }

        // Visual clusters tile the line left to right, and sorted by byte they reconstruct the
        // logical string without dropping or duplicating a single byte.
        let mut cursor = Fixed::ZERO;
        for cluster in line.clusters() {
            assert_eq!(cluster.x, cursor, "{label}: visual gap at {cluster:?}");
            cursor = cluster.end();
        }
        assert_eq!(cursor, line.end_x(), "{label}: end_x mismatch");

        let mut by_byte: Vec<_> = line.clusters().iter().collect();
        by_byte.sort_by_key(|cluster| cluster.byte);
        let logical: String = by_byte.into_iter().map(|cluster| cluster.text).collect();
        assert_eq!(logical, text, "{label}: clusters do not cover the text");
    }

    println!(
        "HOLONOMY_SCRIPT_GATE_HONESTY scripts={scripts} clusters={clusters_checked} \
         carets={carets_checked} carets_inside_a_grapheme=0"
    );
    assert_eq!(scripts, CORPUS.len() as u64);
}

#[test]
fn an_rtl_letter_input_segments_the_standard_way() {
    let word = "\u{5e9}\u{5dc}\u{5d5}\u{5dd}";
    let clusters: Vec<&str> = Clusters::new(word).map(|(_, c)| c).collect();
    assert_eq!(
        clusters,
        vec!["\u{5e9}", "\u{5dc}", "\u{5d5}", "\u{5dd}"],
        "Hebrew letters are four clusters, not one cursive run"
    );

    let pointed = "\u{5e9}\u{5b8}";
    let clusters: Vec<&str> = Clusters::new(pointed).map(|(_, c)| c).collect();
    assert_eq!(
        clusters,
        vec!["\u{5e9}\u{5b8}"],
        "a niqqud point is Extend: it joins its base and is never a caret position"
    );
}

#[test]
fn bidi_runs_reorder_visually_with_zero_displacement() {
    let cases: &[(&str, &str, &[&str])] = &[
        (
            "pure hebrew",
            "\u{5e9}\u{5dc}\u{5d5}\u{5dd}",
            &["\u{5dd}", "\u{5d5}", "\u{5dc}", "\u{5e9}"],
        ),
        (
            "hebrew with niqqud",
            "\u{5e9}\u{5dc}\u{5d5}\u{5b9}\u{5dd}",
            &["\u{5dd}", "\u{5d5}\u{5b9}", "\u{5dc}", "\u{5e9}"],
        ),
        (
            "mixed ltr+rtl+ltr",
            "abc \u{5e9}\u{5dc}\u{5d5}\u{5dd} def",
            &[
                "a", "b", "c", " ", "\u{5dd}", "\u{5d5}", "\u{5dc}", "\u{5e9}", " ", "d", "e",
                "f",
            ],
        ),
        (
            "arabic with digits",
            "\u{645}\u{631} 42",
            &["4", "2", " ", "\u{631}", "\u{645}"],
        ),
        (
            "paired brackets in rtl",
            "\u{5e9} (\u{5dc}) \u{5dd}",
            &["\u{5dd}", " ", ")", "\u{5dc}", "(", " ", "\u{5e9}"],
        ),
        (
            "rtl paragraph with embedded ltr",
            "\u{5e9}\u{5dc} hi \u{5d5}\u{5dd}",
            &[
                "\u{5dd}", "\u{5d5}", " ", "h", "i", " ", "\u{5dc}", "\u{5e9}",
            ],
        ),
    ];

    let mut total_clusters = 0usize;
    let mut displaced = 0usize;
    for &(label, text, expected_visual) in cases {
        let laid = layout_window(text, 0, Fixed::units(200), &Fake);
        let line = laid.lines().first().expect("a laid-out line");
        let got: Vec<&str> = line.clusters().iter().map(|cluster| cluster.text).collect();
        assert_eq!(
            got.len(),
            expected_visual.len(),
            "{label}: cluster count mismatch"
        );
        total_clusters += got.len();
        displaced += got
            .iter()
            .zip(expected_visual)
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(
            got.as_slice(),
            expected_visual,
            "{label}: wrong visual order"
        );
    }

    // Also assert selection spans on RTL and mixed BiDi cover the exact selected clusters.
    let mixed = "abc \u{5e9}\u{5dc}\u{5d5}\u{5dd} def";
    let (laid, _) = lay(mixed);
    // Selecting bytes 2..8 ("c " + "של") splits into two non-adjacent visual runs.
    let pic = laid.picture(Some(4), Some((2, 8)));
    assert_eq!(
        pic.selection.len(),
        2,
        "split bidi selection needs two spans"
    );
    for cluster in laid.lines()[0].clusters() {
        let selected = cluster.byte < 8 && cluster.byte_end() > 2;
        let covered = pic
            .selection
            .iter()
            .any(|span| span.x <= cluster.x && cluster.end() <= span.end);
        assert_eq!(
            covered,
            selected,
            "cluster {:?} selection mismatch",
            cluster
        );
    }

    println!(
        "HOLONOMY_SCRIPT_GATE_ORDER scripts={} clusters={total_clusters} displaced={displaced} \
         note=uax9-visual-order",
        cases.len()
    );
    assert_eq!(displaced, 0);
}

fn check_l8(view: &View, laid: &Layout<'_>, label: &str) -> (u64, u64) {
    let mut boundaries_checked = 0u64;
    let mut disagreements = 0u64;
    let last = laid.lines().last().map_or(0, |line| line.end_byte());
    for byte in 0..=last {
        let Some(painted) = view.paint_point(laid, byte) else {
            continue;
        };
        let hit = view
            .hit_test(laid, painted.point())
            .unwrap_or_else(|| panic!("{label}: paint's own point must hit-test, byte {byte}"));
        assert_eq!(hit.line, painted.line, "{label}: line changed at {byte}");
        assert_eq!(hit.x, painted.x, "{label}: x changed at {byte}");
        let line = &laid.lines()[painted.line];
        let device_xs: Vec<Fixed> = line
            .boundaries()
            .map(|(x, _)| view.to_device(x))
            .collect();
        let distinct = device_xs.windows(2).all(|pair| pair[0] != pair[1]);
        if distinct && hit.byte != byte {
            disagreements += 1;
        }
        boundaries_checked += 1;
    }
    for line in laid.lines() {
        let bounds: Vec<(Fixed, u64)> = line
            .boundaries()
            .map(|(x, b)| (view.to_device(x), b))
            .collect();
        let top = view.to_device(line.top());
        for pair in bounds.windows(2) {
            let mid = pair[0].0.sixtyfourths() / 2 + pair[1].0.sixtyfourths() / 2;
            let hit = view
                .hit_test(laid, Point::new(Fixed::from_64ths(mid), top))
                .expect("click between bidi boundaries must hit-test");
            if hit.byte != pair[0].1 && hit.byte != pair[1].1 {
                disagreements += 1;
            }
        }
    }
    (boundaries_checked, disagreements)
}

#[test]
fn an_rtl_caret_hit_tests_to_the_boundary_it_painted() {
    const PIECES: [&str; 16] = [
        "a",
        "m",
        "i",
        " ",
        "\u{5e9}",
        "\u{5dc}\u{5b8}",
        "\u{5de}",
        "\u{5d9}",
        "\u{627}",
        "\u{645}\u{651}\u{64e}",
        "42",
        "(",
        ")",
        "\u{2068}",
        "\u{2069}",
        "\n",
    ];
    let zooms = [250, 500, 750, 1000, 1250, 1500, 2000, 3000];
    let dpis = [96, 120, 144, 192, 288];

    let mut checked = 0u64;
    let mut disagreements = 0u64;

    for (label, text, _) in CORPUS {
        let laid = layout_window(text, 0, Fixed::units(200), &Fake);
        for zoom in zooms {
            for dpi in dpis {
                let view = View::new(zoom, dpi).expect("valid view");
                let (c, d) = check_l8(&view, &laid, label);
                checked += c;
                disagreements += d;
            }
        }
    }

    // Seeded generated bidirectional documents across three wrap widths and all 40 views.
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    for doc_idx in 0..8 {
        let mut doc = String::new();
        for _ in 0..24 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            doc.push_str(PIECES[(state as usize) % PIECES.len()]);
        }
        for width in [4, 12, 40] {
            let laid = layout_window(&doc, 0, Fixed::units(width), &Fake);
            for zoom in zooms {
                for dpi in dpis {
                    let view = View::new(zoom, dpi).expect("valid view");
                    let label = format!("gen-{doc_idx}-w{width}");
                    let (c, d) = check_l8(&view, &laid, &label);
                    checked += c;
                    disagreements += d;
                }
            }
        }
    }

    println!("HOLONOMY_SCRIPT_GATE_L8 boundaries={checked} disagreements={disagreements}");
    assert_eq!(disagreements, 0, "L-8 disagreed on bidirectional text");
    assert!(checked > 10_000, "too few boundaries checked: {checked}");
}
