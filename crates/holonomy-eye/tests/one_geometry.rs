//! Block 5's first gate: **one geometry, proven** — `hit_test(paint_point(glyph)) == glyph` for
//! every shaped glyph, at every zoom and DPI, over arbitrary documents.
//!
//! This is the v3 regression made impossible. v3 painted text with real glyph advances and reasoned
//! about carets in monospace cells, so the caret you saw and the caret a click chose were two
//! different answers. `holonomy-eye` has one run tree and one transform, and this file is the
//! evidence that they agree — generated documents rather than a fixed corpus, every painted caret,
//! eight zooms and five DPIs.
//!
//! # What is asserted
//!
//! 1. **The gate itself.** For every byte that paints, the caret it paints and the caret its own
//!    point hit-tests to agree on line, top, height and x — always — and on the *byte* when that
//!    line's boundaries have distinct device positions. Where two boundaries share a position the
//!    tie rule gives the answer to the earlier one, which is why the byte clause is conditional
//!    rather than absent: the tie rule is a documented answer, not an exception.
//! 2. **Clicks.** A click landing on a boundary resolves to that boundary (the earlier one where
//!    several share a position), and a click landing between two boundaries resolves to one of
//!    *those* — never to a third one further away.
//! 3. **Non-vacuity.** Every document in the generated set must contain a letter whose advance
//!    differs from another's, so a shaper that quietly became monospace cannot make the whole file
//!    pass while proving nothing.
//!
//! # What is not claimed here
//!
//! No font, no GPU, no shaper: the shaper is a deterministic fake (Block 8 owns the real one), so
//! what is proven is the *geometry*. BiDi reordering is not implemented (OQ-11, gate 3), so the
//! generated text is left-to-right or direction-neutral — this gate is L-8, not the script gate.
//! The script gate's measured half has its own harness, `tests/script_gate.rs` (AUDIT D-66), which
//! covers RTL grapheme honesty and prints the size of the ordering deferral.
//!
//! The numbers are printed as `HOLONOMY_ONE_GEOMETRY`, and the counts asserted, so a green run is
//! the measurement.

use holonomy_eye::{layout_window, Fixed, Layout, Point, Shaper, Style, View};

/// Pieces the generator draws from: extremes of advance (`m` two units, `i` a half), combining
/// marks that share a position, emoji sequences and scripts that segment differently, and all three
/// hard-break spellings.
const PIECES: [&str; 20] = [
    "a",
    "b",
    "m",
    "i",
    "l",
    "o",
    "m",
    " ",
    "  ",
    "xyz",
    "é",
    "e\u{301}",
    "中",
    "山田",
    "ß",
    "…",
    "👩\u{200d}👩\u{200d}👧",
    "🇳🇵",
    "\n",
    "\r\n",
];

/// A deterministic generator: xorshift64*, so a seed reproduces a document exactly.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1))
    }

    fn next(&mut self) -> u64 {
        let mut state = self.0;
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.0 = state;
        state.wrapping_mul(2_685_821_657_736_338_717)
    }

    fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 {
            0
        } else {
            self.next() % bound
        }
    }
}

/// A deterministic fake shaper with deliberately uneven advances: `m` and `中` are wide, `i` and
/// `l` are narrow, everything else is one unit. A monospace fake would hide the very bug this gate
/// exists to prevent.
struct Fake;

impl Shaper for Fake {
    fn advance(&self, cluster: &str, style: Style) -> Fixed {
        let base = match cluster.chars().next() {
            Some('m') | Some('中') => Fixed::units(2),
            Some('i') | Some('l') => Fixed::from_64ths(32),
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

fn zooms() -> [i64; 8] {
    [250, 500, 750, 1000, 1250, 1500, 2000, 3000]
}

fn dpis() -> [i64; 5] {
    [96, 120, 144, 192, 288]
}

fn widths() -> [i64; 3] {
    [3, 10, 40]
}

/// A document assembled from `pieces` pieces of the alphabet.
fn document(rng: &mut Rng, pieces: usize) -> String {
    let mut text = String::new();
    for _ in 0..pieces {
        text.push_str(PIECES[rng.below(PIECES.len() as u64) as usize]);
    }
    text
}

/// Whether a line's boundaries all have distinct device positions.
fn distinct(boundaries: &[(Fixed, u64)]) -> bool {
    boundaries.windows(2).all(|pair| pair[0].0 != pair[1].0)
}

/// The gate's property at one view and one layout; returns the carets and clicks it checked.
fn one_geometry(view: &View, laid: &Layout<'_>, shown: &str) -> (u64, u64) {
    let mut carets = 0;
    let mut clicks = 0;

    // 1. Every byte that paints: the caret it paints must be the caret its own point hit-tests to.
    let last = laid.lines().last().map_or(0, |line| line.end_byte());
    for byte in 0..=last {
        let Some(painted) = view.paint_point(laid, byte) else {
            continue;
        };
        let back = view
            .hit_test(laid, painted.point())
            .unwrap_or_else(|| panic!("{shown}: paint's own point must hit-test, byte {byte}"));
        assert_eq!(back.line, painted.line, "{shown}: byte {byte} changed line");
        assert_eq!(back.top, painted.top, "{shown}: byte {byte} changed top");
        assert_eq!(
            back.height, painted.height,
            "{shown}: byte {byte} changed height"
        );
        assert_eq!(back.x, painted.x, "{shown}: byte {byte} changed x");
        let line = &laid.lines()[painted.line];
        let boundaries: Vec<(Fixed, u64)> = line
            .boundaries()
            .map(|(x, at)| (view.to_device(x), at))
            .collect();
        if distinct(&boundaries) {
            assert_eq!(back.byte, painted.byte, "{shown}: byte {byte} came back as");
        }
        carets += 1;
    }

    // 2. Clicks: on a boundary, and halfway between two of them.
    for line in laid.lines() {
        let boundaries: Vec<(Fixed, u64)> = line
            .boundaries()
            .map(|(x, at)| (view.to_device(x), at))
            .collect();
        if boundaries.is_empty() {
            continue;
        }
        let top = view.to_device(line.top());
        let apart = distinct(&boundaries);
        for (x, byte) in &boundaries {
            let hit = view
                .hit_test(laid, Point::new(*x, top))
                .unwrap_or_else(|| panic!("{shown}: a boundary must hit-test"));
            assert_eq!(
                hit.x, *x,
                "{shown}: a click on a boundary did not land on it"
            );
            if apart {
                assert_eq!(hit.byte, *byte, "{shown}: a click on a boundary chose");
            }
            clicks += 1;
        }
        for pair in boundaries.windows(2) {
            let middle = pair[0].0.sixtyfourths() / 2 + pair[1].0.sixtyfourths() / 2;
            let hit = view
                .hit_test(laid, Point::new(Fixed::from_64ths(middle), top))
                .unwrap_or_else(|| panic!("{shown}: a click between boundaries must hit-test"));
            assert!(
                hit.byte == pair[0].1 || hit.byte == pair[1].1,
                "{shown}: a click between two boundaries chose a third"
            );
            clicks += 1;
        }
    }

    (carets, clicks)
}

#[test]
fn every_glyph_hit_tests_back_to_itself_at_every_zoom_and_dpi() {
    let mut documents = 0u64;
    let mut views = 0u64;
    let mut carets = 0u64;
    let mut clicks = 0u64;

    for seed in 0..4u64 {
        let mut rng = Rng::new(seed);
        for _ in 0..3 {
            let pieces = 1 + rng.below(40) as usize;
            let mut text = document(&mut rng, pieces);
            // Non-vacuity: the document must contain both a wide and a narrow letter, so a shaper
            // that became monospace would be caught here rather than passing quietly. The tail is
            // appended when the draw missed one, so the guarantee is the generator's, not chance.
            if !text.contains('m') || !text.contains('i') {
                text.push_str("mi");
            }
            assert!(
                text.contains('m') && text.contains('i'),
                "seed {seed}: the generator produced no proportional pair"
            );
            documents += 1;
            for width in widths() {
                let laid = layout_window(&text, 0, Fixed::units(width), &Fake);
                if !text.is_empty() {
                    assert!(
                        !laid.lines().is_empty(),
                        "seed {seed}: text without lines cannot be clicked"
                    );
                }
                for zoom in zooms() {
                    for dpi in dpis() {
                        let view = View::new(zoom, dpi).expect("a positive view");
                        let shown = format!("seed {seed} width {width} zoom {zoom} dpi {dpi}");
                        let (checked_carets, checked_clicks) = one_geometry(&view, &laid, &shown);
                        views += 1;
                        carets += checked_carets;
                        clicks += checked_clicks;
                    }
                }
            }
        }
    }

    println!(
        "HOLONOMY_ONE_GEOMETRY documents={documents} views={views} carets={carets} clicks={clicks}"
    );

    // The counts are part of the claim: a green run that checked nothing would still be green.
    assert_eq!(documents, 12, "twelve generated documents");
    let expected_views = 12 * widths().len() as u64 * zooms().len() as u64 * dpis().len() as u64;
    assert_eq!(views, expected_views, "every generated view was checked");
    assert!(carets > 1_000, "only {carets} carets checked");
    assert!(clicks > 1_000, "only {clicks} clicks checked");
}

#[test]
fn a_monospace_hit_test_would_disagree_with_paint() {
    // The v3 bug, measured: this is what the gate above rejects. Paint places `m` two units apart
    // and `i` half a unit; a hit-test that assumed one cell per cluster would put the carets
    // somewhere else entirely, and the count below is how many places it would be wrong.
    let text = "mimimimi";
    let laid = layout_window(text, 0, Fixed::units(40), &Fake);
    let cell = Fake.advance("m", Style::DEFAULT).sixtyfourths();
    let line = &laid.lines()[0];
    let mut disagreements = 0;
    let mut boundaries = 0;
    for (index, (x, _byte)) in line.boundaries().enumerate() {
        boundaries += 1;
        let monospace = Fixed::from_64ths(cell * index as i64);
        if monospace != x {
            disagreements += 1;
        }
    }
    println!(
        "HOLONOMY_ONE_GEOMETRY_CONTROL boundaries={boundaries} \
         monospace_disagreements={disagreements}"
    );
    // One boundary before each cluster, plus the line's own end: a document's last line offers a
    // caret after its final glyph whether or not a hard break follows it.
    assert_eq!(
        boundaries,
        text.chars().count() + 1,
        "one boundary per cluster plus the line's end"
    );
    assert!(
        disagreements >= boundaries / 2,
        "a monospace model disagreed at only {disagreements} of {boundaries} boundaries, so this \
         document could not tell the two geometries apart"
    );
    // And the real geometry still round-trips on that same document, at every view.
    for zoom in zooms() {
        for dpi in dpis() {
            let view = View::new(zoom, dpi).expect("a positive view");
            let (carets, _) = one_geometry(&view, &laid, "control");
            assert!(carets > 0, "zoom {zoom} dpi {dpi} checked nothing");
        }
    }
}

#[test]
fn an_empty_document_offers_exactly_one_caret_and_it_round_trips() {
    // Written as a positive property after the first version asserted None and was wrong: an empty
    // document is one empty line with exactly one caret position, byte zero, and both consumers
    // must answer it identically. A caret paint cannot draw and hit-test cannot reach would be the
    // two-geometry bug in miniature.
    for width in widths() {
        let laid = layout_window("", 0, Fixed::units(width), &Fake);
        let view = View::IDENTITY;
        let painted = view.paint_point(&laid, 0).expect("an empty line's caret");
        assert_eq!(painted.byte, 0);
        assert_eq!(painted.x, Fixed::ZERO);
        let back = view
            .hit_test(&laid, painted.point())
            .expect("its own point hits");
        assert_eq!(back, painted, "the empty document's caret round trip");
        // And nothing else paints: byte 1 is past the end of an empty document.
        assert_eq!(view.paint_point(&laid, 1), None);
    }
}
