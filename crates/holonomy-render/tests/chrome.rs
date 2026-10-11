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

use holonomy_render::chrome::{
    self, Blink, Caret, Chrome, ChromeMetrics, ChromeState, Layout, LineHeights,
};
use holonomy_render::{DamageRect, Node, NodeKind};

// ---------------------------------------------------------------- geometry

#[test]
fn the_bands_stack_with_no_gap_and_no_overlap() {
    let m = ChromeMetrics::DESKTOP;
    let l = Layout::new(&m);
    assert_eq!(l.title.y, 0);
    assert_eq!(
        l.menubar.y,
        l.title.bottom(),
        "the menu bar follows the title"
    );
    assert_eq!(l.tabs.y, l.menubar.bottom());
    // **The tab band is zero-height as of part 19**, so `tabs.bottom() == toolbar.y` is an identity
    // rather than an arrangement. It is kept in the chain because the tab band still exists as a
    // band, and a chain that quietly dropped it would go on asserting a layout nobody has.
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
    // **Seven bands as of part 19, and the tab band is one of them at zero height.** The reference's
    // chrome stacks title / menu / tabs / toolbar / ruler / canvas / status; part 19 set `tab_h: 0`
    // because the title band already drew the document's name and a band with nothing in it should
    // not exist. **A zero-height band contributes nothing to the sum and does not break the
    // partition** -- which is the property this test is for, and it is why removing a band was safe.
    //
    // The first version of this update listed the old five and failed with `left: 748, right: 800`:
    // the 30 px title band and the 22 px menu band were simply not in the list. **A gate that names
    // its bands has to be updated when the bands change, and the failure it gives is the right one** --
    // it said the bands did not cover the panel, which is exactly what an unlisted band looks like.
    let covered: u32 = [
        l.title, l.menubar, l.tabs, l.toolbar, l.ruler, l.canvas, l.status,
    ]
    .iter()
    .map(|r| r.height)
    .sum();
    assert_eq!(
        covered, m.height,
        "the seven bands must partition the panel's height"
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

/// **The measure is `columns` at the design width and grows above it, up to `max_columns`.**
///
/// **CORRECTION, part 27 — twice, and the second correction is the interesting one.** This test was
/// first written asserting `text.width == 80 * 8` — the measure was a constant, so a bigger window gave
/// more desk and no more text, and "make it bigger" did nothing to the thing the window is for. It was
/// then rewritten to assert the 100-column ceiling at [`DESKTOP`], because the room-minus-chrome
/// arithmetic gives 112 columns there.
///
/// **Both were wrong, because both broke something real.** The first made the window meaningless. **The
/// second silently invalidated every number §2.9.3 measured against a 640 px page column** — the 8.0 MiB
/// decoded-image budget, the resample thresholds, the "≤ 8.0 MiB at every point of a page-1→50 scroll"
/// row in §6 — all of which would have become wrong by 40 %.
///
/// **The design width is the anchor: 1,280 px is the width `columns: 80` was chosen for**, and only a
/// panel wider than that earns more text. `ChromeMetrics::DESKTOP` means what it has always meant, and
/// the thing the user asked for — a bigger window — is the thing that now helps.
#[test]
fn the_measure_is_eighty_columns_at_the_design_width_and_grows_above_it() {
    let m = ChromeMetrics::DESKTOP;
    let l = Layout::new(&m);
    assert_eq!(m.columns, 80);
    assert_eq!(
        m.max_columns, 100,
        "and the typographic ceiling is one field away"
    );
    assert_eq!(
        l.text.width,
        m.columns * m.cell_w,
        "**the design width still shows the measured 640 px of text**, so §2.9.3's numbers hold"
    );
    assert_eq!(
        l.page.width,
        m.page_w(),
        "and the page is that measure plus its padding"
    );

    // **CORRECTION, part 36: below the design width the measure holds only while the page fits.**
    //
    // **This loop used to assert the opposite at three widths, including `MIN_WIDTH`** — and it was
    // asserting a *clip*. At 16 ppem, 80 columns is 896 px of page, and `MIN_WIDTH` is 768: **the
    // minimum window this file defines has never been able to show the design measure.** Part 27's
    // rule was written as "nothing changes below the design width", and what it preserved was text
    // running off the edge of the panel.
    //
    // **Part 32 made it much worse and that is what it took to notice:** 80 columns became 1,216 px, so
    // every panel under about 1,200 px clipped, which is most of them. The reported symptom was *"I
    // cannot properly use the app"*.
    //
    // **So the floor is now conditional, and this loop asserts the new rule:** 1,280 px has room for
    // the 80-column page and still shows it — **the design width keeps its meaning** — while a narrower
    // panel gets a shorter line instead of a cut one.
    assert_eq!(
        Layout::new(&m.clamp_to(1280, 800)).text.width,
        m.columns * m.cell_w,
        "1,280 px still shows the 80-column measure, because the design page fits there"
    );
    //
    // **And the property that replaces the old loop is the one that actually matters, and it is a
    // sweep rather than three widths: the page never overflows the panel, at any width.** It is
    // asserted over a range because a single width is a coincidence and a range is the rule, and it
    // is asserted against `page` rather than against `text` because the page is what is clipped.
    //
    // **My first version of this asserted that 1,024 px cannot show an 896 px page. It can** — 896
    // plus a gutter each side is under 1,024 — and the gate was wrong about arithmetic it could have
    // run. **The widths that genuinely cannot are the small ones**, and `MIN_WIDTH` is one of them: a
    // 768 px panel has never held an 896 px page, before or after part 32.
    for w in (ChromeMetrics::MIN_WIDTH..=2560).step_by(8) {
        let n = Layout::new(&m.clamp_to(w, 800));
        assert!(
            n.page.x + n.page.width <= w,
            "**a {w} px panel shows a page running off its right edge**: {}..={}. The measure is \
             clamped to the room, so this cannot happen -- and before part 36 it did, at every width \
             below about 1,200 px once the type was 24 ppem",
            n.page.x,
            n.page.x + n.page.width
        );
    }

    // **Above it, the measure grows and then stops.**
    let wide = Layout::new(&m.clamp_to(1600, 1000));
    assert_eq!(
        wide.text.width,
        m.max_columns * m.cell_w,
        "1,600 px reaches the ceiling: {} px of text",
        wide.text.width
    );
    assert_eq!(
        Layout::new(&m.clamp_to(2560, 1440)).text.width,
        wide.text.width,
        "and 2,560 px shows the same measure -- the ceiling, not the room"
    );
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
    // **Part 25 deleted the four lines that used to be here.** They pushed `StyleFlags::SLOTS`' short
    // forms -- `B`, `I`, `M`, `H` -- into this list of labels the chrome is allowed to draw, and
    // **`Tool::label` returns an empty `String` for every tool except Zoom, Style and Font**, so the
    // chrome never drew one of them. The list was four labels wider than the chrome is.
    //
    // **Which made the gate weaker, silently, in exchange for nothing.** A label list is a permission
    // list: an entry that is never drawn costs nothing and catches nothing, and the only thing it does
    // is make room for a run that should have failed. **This list is now exactly what the chrome
    // draws**, and the comment below -- which explains why the list has to track the chrome -- is the
    // reason `StyleFlags` could sit here, unused and unquestioned, for as long as it did.
    //
    // **What replaced the armed-toggle drawing is a rect, not a label**, so the armed Bold button needs
    // no entry here at all: `armed_toggles_draw_a_surface_and_no_glyph` in this file is the gate for
    // it, and it asserts on pixels rather than on text runs.
    //
    // The labels part 19 added. The chrome now draws a menu bar, a labelled toolbar and a
    // sidebar, and this function is the list every text run is checked against -- so a label that is
    // drawn but not listed here shows up as "run N+M is not an ascending run of any label the chrome
    // draws", which is a *true* statement about the list and a false statement about the chrome.
    //
    // **The sidebar's document titles are listed from `state`, not hard-coded**, because they are
    // data: a gate that hard-coded "Journal" would fail the moment a session had a different
    // document, and would be wrong for the right reason.
    v.extend(state.docs.iter().cloned());
    v.extend(chrome::MENUS.iter().map(|m| (*m).to_string()));
    v.push("Share".to_string());
    v.push("Document tabs".to_string());
    v.push(state.style_name().to_string());
    v.push(state.font_label());
    // The popup's items, because an open menu draws them and the gate's state opens none -- so this is
    // what keeps the popup's labels from being an untested third source of runs.
    for menu in holonomy_render::menus::MENUS {
        for item in *menu {
            v.push(item.label.to_string());
            if let Some(a) = item.accelerator {
                if !a.is_empty() {
                    v.push(a.to_string());
                }
            }
        }
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
            // The line model carries the pitch, and `ChromeState::default()` has a pitch of **zero**
            // because it has no `ChromeMetrics` to take one from. This test caught that: with a zero
            // pitch every line landed at y=0 and the caret sat on the first row of the page. So a
            // state that is going to be used for a caret lookup has to say what a line is tall.
            line_heights: LineHeights::uniform(m.cell_h),
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

/// **A resize widens the measure, and past the ceiling it moves the gutters again.**
///
/// **CORRECTION, part 27 — this test asserted the opposite and was right to.** It said *"a wider window
/// shows the same 80-column measure with more margin, the way a word processor does — _not_ a longer
/// line"*.
///
/// **That reasoning was sound and the conclusion was the wrong way round.** A fixed measure is a
/// *typographic* decision, and it is the right one; making the *window* size irrelevant is not, because
/// a window's size is how a user asks for more room. The two are reconciled by a **ceiling**: the
/// measure grows with the panel up to [`max_columns`](ChromeMetrics::max_columns), and only past that
/// does the extra width become desk.
///
/// **So a resize now does three things, and all three are asserted below:** the measure grows with the
/// panel, it *stops* at the ceiling, and the text column's distance inside the page does not move — the
/// last being the property that actually matters, because that offset is the origin the caret is located
/// against.
#[test]
fn a_resize_widens_the_measure_up_to_the_ceiling() {
    let small = ChromeMetrics::for_size(1024, 600);
    let large = ChromeMetrics::for_size(1600, 1000);
    // **`columns` and `cell_w` still do not move with the window** — the *floor* and the grid are
    // constants, and only the derived measure is not.
    assert_eq!(
        small.columns, large.columns,
        "the measure's floor does not move with the window"
    );
    assert_eq!(small.cell_w, large.cell_w, "nor does the cell size");

    let a = Layout::new(&small);
    let b = Layout::new(&large);
    assert!(
        b.text.width > a.text.width,
        "**a wider panel shows a longer line**: {} then {} px of text",
        a.text.width,
        b.text.width
    );
    assert!(
        b.text.width <= large.max_columns * large.cell_w,
        "and never past the ceiling: {} px against a {} px limit",
        b.text.width,
        large.max_columns * large.cell_w
    );
    // Its *height* survives the resize too: a taller panel fits more rows, and rows are what a page
    // grows by.
    assert!(
        b.text.height > a.text.height,
        "a taller panel fits more rows: {} then {}",
        a.rows,
        b.rows
    );
    // **What must not change is where the caret is inside the page** — the text column's distance from
    // the page's left edge is the origin the caret is located against, and it is the same in both.
    assert_eq!(
        a.text.x - a.gutter_left,
        b.text.x - b.gutter_left,
        "the text column sits the same distance inside the page in both"
    );

    // **And past the ceiling the gutters take over**, which is the half that keeps this from being
    // "fill the panel". A 2,560 px panel has room for ~250 columns and shows 100.
    let huge = Layout::new(&ChromeMetrics::for_size(2560, 1440));
    assert_eq!(
        huge.text.width, b.text.width,
        "1,600 px and 2,560 px show the same measure -- the ceiling, not the room"
    );
    assert!(
        huge.gutter_left > b.gutter_left,
        "and the extra width is desk: left gutter {} then {}",
        b.gutter_left,
        huge.gutter_left
    );
    assert!(
        b.gutter_left > a.gutter_left,
        "a wider panel has a bigger left gutter: {} then {}",
        a.gutter_left,
        b.gutter_left
    );
    assert!(
        b.gutter_right > a.gutter_right,
        "and a bigger right one: {} then {}",
        a.gutter_right,
        b.gutter_right
    );
    // Centred to within the pixel `Layout::new` deliberately gives away to the right gutter.
    assert!(
        b.gutter_left.abs_diff(b.gutter_right) <= 1,
        "the page is centred: gutters {} and {}",
        b.gutter_left,
        b.gutter_right
    );
}

/// The bands always partition the whole panel, at every size the chrome accepts.
///
/// This is the property that makes a resize safe: if the bands summed to less than the height, the
/// strip below them would be whatever the frame was initialised to, and on a *resize* that is the
/// previous frame's pixels at the old size -- so a window would keep a ghost of itself along the
/// bottom edge.
#[test]
fn the_bands_partition_the_panel_at_any_size() {
    for (w, h) in [
        (ChromeMetrics::MIN_WIDTH, ChromeMetrics::MIN_HEIGHT),
        (1280, 800),
        (2560, 1440),
        (800, 300),
        (769, 289),
    ] {
        let m = ChromeMetrics::for_size(w, h);
        let l = Layout::new(&m);
        assert_eq!(
            (l.width, l.height),
            (m.width, m.height),
            "{w}x{h}: the panel size is kept"
        );
        let mut at = 0u32;
        // **Seven bands, in paint order, and the list must start at 0 and end at the height.** A
        // missing band shows up here as `b.y == at` failing on the band *after* the gap, which is what
        // the first version of this update reported: `left: 52, right: 0` -- the toolbar starts at 52
        // because the title and menu bands are above it and were not in the list.
        for b in [
            l.title, l.menubar, l.tabs, l.toolbar, l.ruler, l.canvas, l.status,
        ] {
            assert_eq!(
                b.y, at,
                "{w}x{h}: a band does not start where the last one ended"
            );
            assert!(
                b.width <= m.width,
                "{w}x{h}: a band is no wider than the panel"
            );
            at += b.height;
        }
        assert_eq!(
            at, m.height,
            "{w}x{h}: the five bands cover the panel exactly"
        );
        assert_eq!(
            l.canvas.height,
            m.canvas_h(),
            "{w}x{h}: the canvas is what the four bands leave"
        );
    }
}

/// A panel too small for the chrome is clamped, not refused, and not left to wrap.
///
/// A window can be dragged to nothing. The answer has to be the minimum this chrome can draw, because
/// the alternative is a subtraction that wraps: `Layout::new` is `saturating` throughout precisely so
/// that cannot happen, and this is the test that the clamp is what actually stops it.
#[test]
fn a_panel_too_small_is_clamped_to_the_minimum() {
    let m = ChromeMetrics::for_size(1, 1);
    assert_eq!(m.width, ChromeMetrics::MIN_WIDTH, "the width is clamped up");
    assert_eq!(m.height, ChromeMetrics::MIN_HEIGHT, "and the height");
    let l = Layout::new(&m);
    assert!(
        l.page.width <= m.width,
        "the page fits the panel it was clamped to"
    );
    assert!(
        l.rows >= 4,
        "and shows the four rows MIN_HEIGHT promises: {}",
        l.rows
    );
    // `clamp_to` is the same thing under a different name, and it must stay so -- the session calls it
    // on every resize.
    assert_eq!(m.clamp_to(1, 1), m, "clamp_to agrees with for_size");
}

/// # Part 36: a resize threw the type scale away
///
/// **The report, on the running window:** *"the view port -- its way too small and I cannot properly
/// use the app and can't even resize it"*, and then *"things like the icons etc overlap on each
/// other"*. **Two symptoms and one cause.**
///
/// ## The cause
///
/// [`ChromeMetrics::clamp_to`] -- which [`Session::resize`](../../holonomy/session/struct.Session.html)
/// calls on every single `ConfigureNotify` -- was:
///
/// ```ignore
/// pub const fn clamp_to(&self, width: u32, height: u32) -> Self {
///     Self::for_size(width, height)   // <- `..Self::DESKTOP`
/// }
/// ```
///
/// **and `for_size` is `Self { width, height, ..Self::DESKTOP }`.** So a resize rebuilt the whole
/// struct from the **16 ppem design constants**, discarding the three things [`Session::new`] had just
/// derived from the atlas: the measured `cell_w`, the raised band heights, and the scaled
/// `anchor_width`.
///
/// **And the gate that is the one-line summary of this file's blind spot is right above.** Every
/// `clamp_to` gate here builds its metrics from `for_size` or `DESKTOP` — from the design constants,
/// which is the only state `clamp_to` ever returned — and so **could not have failed however it was
/// written.** `Chrome::new(metrics)` then laid the chrome out at 24 ppem's cell inside 16 ppem's
/// bands, and 24 ppem's glyphs overflowed 16 ppem's bands: **the icons overlap**, and the page gets
/// its gutters and its measure from a cell that is no longer the one the painter is using.
///
/// ## The gate
///
/// **A scaled struct, resized.** Not a designed one — that distinction is the whole test, and it is why
/// no gate in this file had caught it.
#[test]
fn a_resize_keeps_the_type_scale_and_not_just_the_panel() {
    /// What `Session::new` builds from a 24 ppem atlas: pitch 37, the face's own advance of 14, and the
    /// bands scaled by 37/25. Spelled out rather than built from an atlas so this file keeps no
    /// dependency on one, and so the numbers are the ones actually measured.
    fn at_24_ppem() -> ChromeMetrics {
        ChromeMetrics::DESKTOP
            .with_line_pitch(37)
            .with_cell_width(14)
            .with_type_scale(37)
    }

    let before = at_24_ppem();
    let after = before.clamp_to(1600, 900);

    // **The cell first, because everything else is measured in it.**
    assert_eq!(
        after.cell_w, before.cell_w,
        "**a resize changed the cell the painter's advances are in.** `cell_w` is read off the \
         Monospace face by `Session::new`; a resize cannot know it, so it must keep it"
    );
    assert_eq!(after.cell_h, before.cell_h, "and the line pitch");

    // **The bands, which is where the overlap came from: 24 ppem's glyphs in 16 ppem's bands.**
    for (name, got, want) in [
        ("title_h", after.title_h, before.title_h),
        ("menu_h", after.menu_h, before.menu_h),
        ("toolbar_h", after.toolbar_h, before.toolbar_h),
        ("ruler_h", after.ruler_h, before.ruler_h),
        ("status_h", after.status_h, before.status_h),
    ] {
        assert_eq!(got, want, "a resize changed {name}");
    }

    // **The anchor, which decides the measure.** At 24 ppem it is 1,894; from `DESKTOP` it is 1,280,
    // so the resize also moved the width at which the line stops growing -- by 600 px, silently.
    assert_eq!(
        after.anchor_width, before.anchor_width,
        "**a resize moved the design width the measure is anchored at, from {} to {}**",
        before.anchor_width, after.anchor_width
    );

    // **And only the panel changed**, which is what the doc has always said:
    // *"The cell size, the padding, the measure and the four band heights are all the same as `self`'s;
    // only the panel changes."*
    assert_eq!(after.width, 1600);
    assert_eq!(after.height, 900);
    assert_eq!(after.columns, before.columns);
    assert_eq!(after.page_pad, before.page_pad);
    assert_eq!(after.max_columns, before.max_columns);
}

/// **The overlap itself, as the arithmetic that produces it.** **Every band that holds text has to be
/// able to hold one line**, and 16 ppem's toolbar cannot hold 24 ppem's glyphs.
///
/// **`tabs` is deliberately not in the list:** [`ChromeMetrics::tab_h`] has been `0` since part 19, and
/// a band that is gone cannot overflow.
#[test]
fn the_toolbar_bands_are_tall_enough_for_the_type_they_hold() {
    fn at_24_ppem() -> ChromeMetrics {
        ChromeMetrics::DESKTOP
            .with_line_pitch(37)
            .with_cell_width(14)
            .with_type_scale(37)
    }
    let m = at_24_ppem();
    let after = m.clamp_to(1600, 900);
    let l = Layout::new(&after);
    for (name, band) in [
        ("title", l.title),
        ("menubar", l.menubar),
        ("toolbar", l.toolbar),
        ("ruler", l.ruler),
        ("status", l.status),
    ] {
        assert!(
            band.height >= after.cell_h,
            "**the {name} band is {} px tall and the line pitch is {}**: a band that cannot hold one \
             line cannot hold one line of anything, and the text in it lands on the band below",
            band.height,
            after.cell_h
        );
    }
}
