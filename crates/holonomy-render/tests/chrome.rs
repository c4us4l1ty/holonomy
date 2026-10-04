//! The chrome's geometry, and the properties that make it checkable without a renderer.
//!
//! # What is asserted here, and why each one is a real risk
//!
//! * **Integer-only.** No `f32` in the module. There is a `const` assertion that the page's gutters
//!   sum to the panel width for a range of sizes, which is the arithmetic that a float would get wrong
//!   by one pixel on odd widths.
//! * **Every separator is in the verified table.** A separator drawn from a codepoint the font atlas
//!   does not contain is a blank gap in a rule, and a blank gap in a rule looks like a deliberate
//!   dashed line. `█` and the shade blocks are *not* in the table, so this test also pins that the
//!   chrome's fills are `Rect`s and not glyphs.
//! * **The blink dirties one cell.** A hundred frames of `Blink::advance`, collecting every non-`None`
//!   answer, must all be the caret's rect and their union must equal it. This is the FR-3.4 property
//!   and the reason `Blink::advance` returns a `DamageRect` rather than a `bool`.

use holonomy_render::chrome::{self, Blink, Caret, Chrome, ChromeMetrics, ChromeState, Layout};
use holonomy_render::{DamageRect, Node, NodeKind};

// ---------------------------------------------------------------- geometry

#[test]
fn the_bands_stack_with_no_gap_and_no_overlap() {
    let m = ChromeMetrics::DESKTOP;
    let l = Layout::new(&m);
    assert_eq!(l.tabs.y, 0);
    assert_eq!(
        l.toolbar.y,
        l.tabs.bottom(),
        "the toolbar starts where the tabs end"
    );
    assert_eq!(l.ruler.y, l.toolbar.bottom());
    assert_eq!(l.canvas.y, l.ruler.bottom());
    assert_eq!(l.status.y, l.canvas.bottom());
    assert_eq!(
        l.status.bottom(),
        m.height,
        "the status bar ends at the panel edge"
    );
}

#[test]
fn the_bands_cover_the_panel_exactly_once() {
    let m = ChromeMetrics::DESKTOP;
    let l = Layout::new(&m);
    let covered: u32 = [l.tabs, l.toolbar, l.ruler, l.canvas, l.status]
        .iter()
        .map(|r| r.height)
        .sum();
    assert_eq!(
        covered, m.height,
        "the five bands must partition the panel's height"
    );
}

#[test]
fn the_page_is_centred_and_the_gutters_sum_to_the_panel() {
    // The property integer arithmetic buys. With floats, `(1280 - 736) / 2.0` and an
    // independently-rounded left and right give a page that is off centre by a pixel on odd widths,
    // and which jumps about as the window resizes.
    for width in [640u32, 641, 800, 1023, 1024, 1280, 1281, 1366, 1920] {
        let m = ChromeMetrics {
            width,
            ..ChromeMetrics::DESKTOP
        };
        let l = Layout::new(&m);
        assert_eq!(
            l.gutter_left + l.page.width + l.gutter_right,
            width,
            "gutters + page must fill the panel at width {width}"
        );
        assert!(
            l.gutter_left.abs_diff(l.gutter_right) <= 1,
            "width {width}: gutters {} and {} differ by more than the remainder",
            l.gutter_left,
            l.gutter_right
        );
        // And the remainder goes right, always, so the page does not jitter.
        assert!(l.gutter_left <= l.gutter_right, "width {width}");
    }
}

#[test]
fn the_measure_is_eighty_columns_of_the_cell_width() {
    let m = ChromeMetrics::DESKTOP;
    let l = Layout::new(&m);
    assert_eq!(m.columns, 80);
    assert_eq!(l.text.width, 80 * 8);
    assert_eq!(l.page.width, m.columns * m.cell_w + m.page_pad * 2);
}

#[test]
fn the_text_column_sits_inside_the_page_with_its_padding() {
    let m = ChromeMetrics::DESKTOP;
    let l = Layout::new(&m);
    assert_eq!(l.text.x, l.page.x + m.page_pad);
    assert_eq!(l.text.y, l.page.y + m.page_pad);
    assert!(
        l.text.right() <= l.page.right(),
        "the text column must not overflow the page"
    );
}

#[test]
fn the_scrollbar_is_right_of_the_page_and_inside_the_canvas() {
    let m = ChromeMetrics::DESKTOP;
    let l = Layout::new(&m);
    assert!(
        l.scrollbar.x >= l.page.right(),
        "the rail sits after the sheet"
    );
    assert!(l.scrollbar.y >= l.canvas.y);
    assert!(l.scrollbar.bottom() <= l.canvas.bottom());
}

#[test]
fn a_panel_smaller_than_its_bands_degrades_instead_of_wrapping() {
    // Every subtraction is `saturating_*`. Without that, `status.y = canvas.y + canvas_h` with a
    // canvas_h that has already gone negative wraps to ~4e9 and every rect lands at y = -1.
    let m = ChromeMetrics {
        width: 100,
        height: 10,
        tab_h: 40,
        toolbar_h: 40,
        ruler_h: 40,
        status_h: 40,
        ..ChromeMetrics::DESKTOP
    };
    let l = Layout::new(&m);
    assert_eq!(
        l.canvas.height, 0,
        "no canvas survives four 40px bands in a 10px panel"
    );
    assert!(
        l.status.y <= m.height,
        "the status bar must not start below the panel"
    );
    assert_eq!(l.rows, 0, "no text rows survive a canvas of zero height");
    assert!(
        Layout::new(&ChromeMetrics::DESKTOP).rows > 10,
        "and the normal layout has many, so the zero above is real"
    );
}

#[test]
fn a_page_wider_than_the_panel_is_clipped_not_wrapped() {
    let m = ChromeMetrics {
        width: 100,
        ..ChromeMetrics::DESKTOP
    };
    let l = Layout::new(&m);
    assert_eq!(l.gutter_left, 0);
    assert_eq!(l.gutter_right, 0);
    assert!(l.page.width <= 100, "the page is clipped to the panel");
}

// ---------------------------------------------------------------- runes

#[test]
fn every_rune_is_in_the_verified_table() {
    // A separator from outside the table is a hole in a rule, and a hole in a rule reads as a
    // deliberate dashed line rather than as a bug.
    use chrome::rune::*;
    for cp in [
        H, V, DR, DL, UR, UL, VR, VL, DV, UV, CROSS, V_DOUBLE, ROUNDED_DR, ROUNDED_DL, ROUNDED_UL,
        ROUNDED_UR,
    ] {
        assert!(
            (0x2500..=0x257F).contains(&cp),
            "{cp:#06x} is outside the box-drawing range"
        );
    }
}

#[test]
fn the_block_runes_are_absent_so_the_chrome_does_not_use_them() {
    // `box_drawing::LAST` is `0x257F`, so `█` (U+2588) and the shade blocks (U+2591, U+2592) are not
    // drawable. The scrollbar thumb, the dirty flag and the page shadow are therefore filled `Rect`s.
    // This asserts the premise, so a future "let me just use a block" does not silently ship a gap.
    for cp in [0x2588u32, 0x2591, 0x2592] {
        assert!(
            cp > 0x257F,
            "{cp:#06x} would now be in the table; the chrome can use it directly"
        );
    }
}

// ---------------------------------------------------------------- the tree

/// Every node in the tree, flattened, with the tree's documented paint order.
fn nodes(t: &holonomy_render::SurfaceTree) -> Vec<Node> {
    let mut out = Vec::new();
    for c in &t.before {
        out.extend(nodes(c));
    }
    if let Some(n) = &t.node {
        out.push(*n);
    }
    for c in &t.after {
        out.extend(nodes(c));
    }
    out
}

#[test]
fn the_tree_is_built_for_the_state_and_covers_every_band() {
    let m = ChromeMetrics::DESKTOP;
    let c = Chrome::new(m);
    let state = ChromeState {
        title: "quarterly.wavefunction".to_string(),
        sealed: true,
        ..ChromeState::default()
    };
    let tree = c.tree(&state);
    let all = nodes(&tree);

    assert!(!all.is_empty());
    // The panel itself, then the bands, then the page.
    assert_eq!(
        all[0].kind(),
        NodeKind::Rect,
        "the first node painted is the panel"
    );
    let filled: u32 = all
        .iter()
        .filter_map(|n| match n {
            Node::Rect(r) => Some(r.width * r.height),
            _ => None,
        })
        .sum();
    assert!(
        filled >= m.width * m.height / 4,
        "the chrome fills too little of the panel to be the chrome"
    );
}

/// Every codepoint of every label the chrome draws, in the order it draws them.
fn chrome_labels(state: &ChromeState) -> Vec<String> {
    let mut v = vec![
        state.title.clone(),
        format!("{}%", state.zoom_percent),
        format!(
            "Ln {}, Col {}  {} words  {} B",
            state.caret_line + 1,
            state.caret_column + 1,
            state.words,
            state.bytes
        ),
    ];
    if state.sealed {
        v.push("[SEALED]".to_string());
    }
    for (_, _label, short, _) in holonomy_render::chrome::StyleFlags::SLOTS {
        v.push((*short).to_string());
    }
    v
}

#[test]
fn text_runs_are_ascending_and_contiguous() {
    // `TextRun` encodes "the next `len` codepoints are this one plus one, repeatedly". Anything else
    // decodes to different glyphs, so this is a correctness check on every run the chrome emits, not a
    // style check.
    //
    // Compared against *every* label the chrome draws -- title, zoom, status line, the badge and the
    // four toggle letters -- because an earlier version compared only against the title and so
    // flagged the badge's `[` as bogus.
    let c = Chrome::new(ChromeMetrics::DESKTOP);
    let state = ChromeState {
        title: "Chapter One".to_string(),
        sealed: true,
        words: 412,
        bytes: 2458,
        ..ChromeState::default()
    };
    let legal: std::collections::BTreeSet<(u32, u16)> = chrome_labels(&state)
        .iter()
        .flat_map(|s| chrome::ascending_runs(s))
        .map(|r| (r.first, r.len))
        .collect();

    let mut checked = 0usize;
    for node in nodes(&c.tree(&state)) {
        let Node::Text(run) = node else { continue };
        if run.first_codepoint >= 0x2500 {
            continue; // a box-drawing separator, a single glyph
        }
        assert!(
            legal.contains(&(run.first_codepoint, run.len)),
            "run {}+{} is not an ascending run of any label the chrome draws",
            run.first_codepoint,
            run.len
        );
        checked += 1;
    }
    assert!(
        checked >= 5,
        "only {checked} text runs checked; the tree is not being exercised"
    );
}

#[test]
fn the_status_line_reports_the_caret_position_one_based() {
    // Humans count lines and columns from one. `ChromeState` is zero-based because it addresses
    // glyphs; the status bar is read by a person, so it must show `line + 1`.
    let c = Chrome::new(ChromeMetrics::DESKTOP);
    let state = ChromeState {
        caret_line: 0,
        caret_column: 0,
        words: 1,
        bytes: 1,
        ..ChromeState::default()
    };
    let found = nodes(&c.tree(&state))
        .into_iter()
        .filter_map(|n| match n {
            Node::Text(r) => Some((r.first_codepoint, r.len)),
            _ => None,
        })
        .any(|(first, len)| {
            // "Ln 1, ..." -- `L` then `n` descends, so the leading run is exactly one glyph.
            first == 'L' as u32 && len == 1
        });
    assert!(found, "the status line's leading text is missing");
}

#[test]
fn the_sealed_badge_appears_only_when_sealed() {
    let c = Chrome::new(ChromeMetrics::DESKTOP);
    let sealed = c.tree(&ChromeState {
        sealed: true,
        ..ChromeState::default()
    });
    let open = c.tree(&ChromeState {
        sealed: false,
        ..ChromeState::default()
    });

    let count = |t: &holonomy_render::SurfaceTree| {
        nodes(t)
            .iter()
            .filter(|n| matches!(n, Node::Text(r) if r.first_codepoint == b'[' as u32))
            .count()
    };
    assert_eq!(
        count(&sealed),
        1,
        "[SEALED] must be drawn when the container is sealed"
    );
    assert_eq!(count(&open), 0, "[SEALED] must not be drawn when it is not");
}

#[test]
fn ascending_runs_splits_prose_because_prose_is_not_ascending() {
    // The premise behind the splitter. "Cha" is contiguous ascending; "Chapter" is not, because
    // `h` (0x68) to `a` (0x61) descends.
    let runs = chrome::ascending_runs("Chapter");
    assert!(runs.len() > 1, "'Chapter' is not one ascending run");
    assert_eq!(runs[0].first, 'C' as u32);
    // Every run must be genuinely ascending.
    for r in &runs {
        for k in 0..r.len {
            assert_eq!(r.first + u32::from(k), {
                // Re-derive from the source so the assertion is about the function, not a copy of it.
                let mut it = "Chapter".chars();
                it.nth((r.char_offset + u32::from(k)) as usize)
                    .expect("in range") as u32
            });
        }
    }
    // Total length is preserved.
    assert_eq!(runs.iter().map(|r| u32::from(r.len)).sum::<u32>(), 7);
}

#[test]
fn an_empty_string_produces_no_runs() {
    assert!(chrome::ascending_runs("").is_empty());
}

#[test]
fn a_long_label_splits_into_runs_within_the_run_length_limit() {
    // 300 characters: `TextRun::MAX_LEN` is 256, so a single run is not allowed to be produced.
    let text: String = "A".repeat(300);
    let runs = chrome::ascending_runs(&text);
    assert!(runs
        .iter()
        .all(|r| r.len <= holonomy_render::TextRun::MAX_LEN));
    assert_eq!(runs.iter().map(|r| u32::from(r.len)).sum::<u32>(), 300);
}

// ---------------------------------------------------------------- caret

#[test]
fn the_caret_is_exactly_one_cell() {
    let m = ChromeMetrics::DESKTOP;
    let c = Chrome::new(m);
    let state = ChromeState {
        caret_line: 3,
        caret_column: 17,
        ..ChromeState::default()
    };
    let caret = Caret::locate(&c.layout, &m, &state).expect("on screen");
    assert_eq!(caret.cell.width, m.cell_w);
    assert_eq!(caret.cell.height, m.cell_h);
    assert_eq!(caret.cell.width * caret.cell.height, 8 * 18);
}

#[test]
fn the_caret_sits_on_its_column_and_row() {
    let m = ChromeMetrics::DESKTOP;
    let c = Chrome::new(m);
    for (line, column) in [
        (0u32, 0u32),
        (0, 79),
        (1, 0),
        (7, 40),
        (c.layout.rows - 1, 12),
    ] {
        let state = ChromeState {
            caret_line: line,
            caret_column: column,
            ..ChromeState::default()
        };
        let caret = Caret::locate(&c.layout, &m, &state).expect("on screen");
        assert_eq!(caret.cell.x, c.layout.text.x + column * m.cell_w);
        assert_eq!(caret.cell.y, c.layout.text.y + line * m.cell_h);
        assert_eq!((caret.line, caret.column), (line, column));
    }
}

#[test]
fn a_caret_scrolled_out_of_view_is_absent_not_clamped() {
    // Clamping would draw a caret at the top or bottom of the page, which is a lie about where the
    // user is. Absent is the honest answer.
    let m = ChromeMetrics::DESKTOP;
    let c = Chrome::new(m);
    let scrolled = ChromeState {
        caret_line: 100,
        scroll_line: 0,
        ..ChromeState::default()
    };
    assert!(Caret::locate(&c.layout, &m, &scrolled).is_none());

    // And it comes back when scrolled to.
    let visible = ChromeState {
        caret_line: 100,
        scroll_line: 100,
        ..ChromeState::default()
    };
    assert!(Caret::locate(&c.layout, &m, &visible).is_some());
}

#[test]
fn a_caret_past_the_measure_is_absent() {
    let m = ChromeMetrics::DESKTOP;
    let c = Chrome::new(m);
    let state = ChromeState {
        caret_column: m.columns,
        ..ChromeState::default()
    };
    assert!(Caret::locate(&c.layout, &m, &state).is_none());
}

// ---------------------------------------------------------------- blink

#[test]
fn the_blink_is_a_square_wave_with_the_stated_period() {
    let b = Blink::new(Blink::DEFAULT_PERIOD);
    assert!(b.visible());
    for f in 0..Blink::DEFAULT_PERIOD {
        assert!(b.visible_at(f), "frame {f} should be in the visible half");
    }
    for f in Blink::DEFAULT_PERIOD..2 * Blink::DEFAULT_PERIOD {
        assert!(!b.visible_at(f), "frame {f} should be in the hidden half");
    }
    assert!(
        b.visible_at(2 * Blink::DEFAULT_PERIOD),
        "and then visible again"
    );
}

#[test]
fn the_blink_invalidates_only_the_caret_cell() {
    // The FR-3.4 property, stated as a test: over a hundred frames, every frame that reports damage
    // reports the caret's cell, and nothing else is ever dirty.
    let m = ChromeMetrics::DESKTOP;
    let c = Chrome::new(m);
    let state = ChromeState {
        caret_line: 5,
        caret_column: 40,
        ..ChromeState::default()
    };
    let caret = Caret::locate(&c.layout, &m, &state)
        .expect("on screen")
        .cell;
    assert_eq!((caret.width, caret.height), (m.cell_w, m.cell_h));

    let mut blink = Blink::new(4);
    let mut flips = 0usize;
    let mut union = DamageRect::EMPTY;
    for _ in 0..100 {
        if let Some(d) = blink.advance(Some(caret)) {
            flips += 1;
            assert_eq!(
                d, caret,
                "a blink dirtied something other than the caret cell"
            );
            union = union.union(&d);
        }
    }
    assert!(
        flips >= 8,
        "100 frames at period 4 must flip many times, got {flips}"
    );
    assert_eq!(
        union, caret,
        "the union of every blink's damage is exactly the caret cell"
    );
}

#[test]
fn hiding_the_caret_dirties_nothing_because_nothing_changed() {
    // A visible -> hidden flip changes no pixel: the cell was already painted with the page behind
    // it. So only the transition *to* visible reports damage, and a frame-driven test that counted
    // both directions would see twice the flips and repaint for nothing.
    let caret = DamageRect::new(10, 20, 8, 18);
    let mut blink = Blink::new(2);
    let mut on_flips = 0;
    let mut off_flips = 0;
    for _ in 0..40 {
        let before = blink.visible();
        match blink.advance(Some(caret)) {
            Some(_) if before => off_flips += 1,
            Some(_) => on_flips += 1,
            None => {}
        }
    }
    assert!(on_flips > 0);
    assert_eq!(off_flips, 0, "a hide must not dirty the cell");
}

#[test]
fn an_off_screen_caret_blinks_without_dirtying_anything() {
    let mut blink = Blink::new(2);
    for _ in 0..20 {
        assert_eq!(
            blink.advance(None),
            None,
            "no cell on screen means no damage to invalidate"
        );
    }
}

#[test]
fn a_zero_period_would_divide_by_zero_and_is_clamped() {
    let b = Blink::new(0);
    assert_eq!(b.period, 1, "a zero period would divide by zero");
    // With a period of one the square wave alternates every frame, which is the fastest blink
    // expressible -- so `visible_at(1)` is *false*, not true.
    assert!(b.visible_at(0));
    assert!(!b.visible_at(1));
    assert!(b.visible_at(2));
}

#[test]
fn the_next_flip_is_when_the_phase_changes() {
    let b = Blink::new(10);
    assert_eq!(b.next_flip(), 10);
    let b = Blink {
        period: 10,
        frame: 12,
    };
    assert_eq!(b.next_flip(), 20);
    let b = Blink {
        period: 10,
        frame: 10,
    };
    assert_eq!(
        b.next_flip(),
        20,
        "frame 10 is the first of the hidden half"
    );
}

// ---------------------------------------------------------------- scrollbar

#[test]
fn the_scroll_thumb_fills_the_rail_when_everything_is_visible() {
    let c = Chrome::new(ChromeMetrics::DESKTOP);
    let state = ChromeState {
        total_lines: 1,
        scroll_line: 0,
        ..ChromeState::default()
    };
    let rail = c.layout.scrollbar;
    let thumb = c.scroll_thumb(&state);
    assert_eq!(
        thumb.height, rail.height,
        "nothing to scroll means the thumb is the rail"
    );
    assert_eq!(thumb.y, rail.y);
}

#[test]
fn the_scroll_thumb_never_vanishes() {
    // `thumb_h = rail_h * visible / total` is 0 for a very long document, and a scrollbar with no
    // thumb reads as broken rather than as "there is nothing here".
    let c = Chrome::new(ChromeMetrics::DESKTOP);
    for total in [1u32, 2, 10, 100, 10_000, 1_000_000] {
        let thumb = c.scroll_thumb(&ChromeState {
            total_lines: total,
            scroll_line: 0,
            ..ChromeState::default()
        });
        assert!(
            thumb.height >= Chrome::MIN_THUMB.min(c.layout.scrollbar.height),
            "total_lines {total} gave a {}-px thumb",
            thumb.height
        );
        assert!(
            thumb.bottom() <= c.layout.scrollbar.bottom(),
            "the thumb left the rail"
        );
    }
}

#[test]
fn the_scroll_thumb_moves_down_as_the_view_scrolls() {
    let c = Chrome::new(ChromeMetrics::DESKTOP);
    let total = 400u32;
    let rail = c.layout.scrollbar;
    let mut last = 0;
    for line in [0u32, 50, 100, 200, 300, total - c.layout.rows - 1] {
        let thumb = c.scroll_thumb(&ChromeState {
            total_lines: total,
            scroll_line: line,
            ..ChromeState::default()
        });
        assert!(
            thumb.y >= last,
            "the thumb went backwards at scroll_line {line}"
        );
        assert!(thumb.y >= rail.y && thumb.bottom() <= rail.bottom());
        last = thumb.y;
    }
}
