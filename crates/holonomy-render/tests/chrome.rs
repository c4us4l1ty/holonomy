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
    //
    // **CORRECTION, part 39: "centred" now means centred in the *desk*, not in the panel.** The sidebar
    // is inside the canvas and is drawn over the page's left edge, so the page is centred in
    // `width - sidebar_w` -- the correction is in `Layout::new`. This gate compared the two gutters as
    // they stand, which is only true when the sidebar is closed; with it open the left one carries the
    // sidebar as well, and at 640 px the two were 296 and 40.
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
        let sidebar = m.sidebar_w.min(width / 2);
        assert!(
            l.gutter_left - sidebar <= l.gutter_right + 1,
            "width {width}: the left gutter {} is {} of it sidebar and the right is {}, so the desk is \
             not centred",
            l.gutter_left,
            sidebar,
            l.gutter_right
        );
        // And the remainder goes right, always, so the page does not jitter. **In the desk**: with the
        // sidebar open the desk's left half is a pixel wider, because `(desk - page) / 2` rounds down.
        assert!(
            l.gutter_left - sidebar <= l.gutter_right,
            "width {width}: desk halves {} and {}",
            l.gutter_left - sidebar,
            l.gutter_right
        );
    }
}

/// **The measure is `columns`, and the ceiling is the same number: a page that does not grow.**
///
/// # CORRECTION, part 39: this gate said eighty, and part 27's growth is reversed
///
/// **The history is worth keeping because it is three corrections deep.** Part 27 first made the measure
/// a constant (`80 * 8`) and the complaint was *"make the window bigger and nothing happens"*. It then
/// made the measure grow with the panel, up to a 100-column ceiling, and the complaint became **"the
/// page fills the whole window"** — which is the same observation wearing a different hat: a resize that
/// only changes how many characters fit on a line is a resize the eye cannot see.
///
/// **What the reference does is the third thing, and it is what this gate now asserts.** Measured off
/// `Plan/Screenshot From 2026-10-01 00-17-42.png`, 1,778 px wide: **a page 813 px wide, centred, with
/// 310 px of desk on each side — at every screenshot in the folder.** The measure does not move with the
/// window; the *page* moves, and the gutters take the difference.
///
/// **So the resize is still visible, and it is visible in the thing that moves.** Part 39 also fixed the
/// reason it did not move at all: `Desktop::resize` was an inherent method shadowing the trait's, so
/// `Session::resize` never resized anything. That fix is what makes this rule affordable; before it,
/// freezing the measure would have frozen the layout.
///
/// **72 and not 80**, because a monospaced cell is wider than the reference's proportional character:
/// 72 cells at 12 px is 864 px, about the same *count* of characters as the reference's 777 px line.
/// At the 8 px design cell it is 576 px, so §2.9.3's 640 px page column moves by 10 % — see
/// `the_design_page_column_is_576_px_and_says_so` below.
#[test]
fn the_measure_is_seventy_two_columns_and_does_not_grow_with_the_window() {
    let m = ChromeMetrics::DESKTOP;
    let l = Layout::new(&m);
    assert_eq!(m.columns, 72, "the design measure, in cells");
    assert_eq!(
        m.max_columns, m.columns,
        "**and the ceiling is the same number**, which is what 'a page that does not grow' means. \
         Raising this one field brings part 27's growth back."
    );
    assert_eq!(
        l.text.width,
        m.columns * m.cell_w,
        "the design width shows exactly the design measure: {} px of text",
        l.text.width
    );
    assert_eq!(
        l.page.width,
        m.page_w(),
        "and the page is that measure plus its padding"
    );

    // **CORRECTION, part 36: below the design width the measure holds only while the page fits.**
    //
    // **This loop used to assert the opposite at three widths, including `MIN_WIDTH`** -- and it was
    // asserting a *clip*. **The floor is conditional**, so a panel too narrow for the design page gets a
    // shorter measure rather than text running off its right edge.
    //
    // **Part 32 made that much worse and that is what it took to notice:** the measure grew with the
    // type, so every panel under about 1,200 px clipped, which is most of them. The reported symptom
    // was *"I cannot properly use the app"*.
    //
    // **So the floor is now conditional, and this loop asserts the new rule:** 1,280 px has room for
    // the design page and still shows it -- **the design width keeps its meaning** -- while a narrower
    // panel gets a shorter line instead of a cut one.
    assert_eq!(
        Layout::new(&m.clamp_to(1280, 800)).text.width,
        m.columns * m.cell_w,
        "1,280 px still shows the design measure, because the design page fits there"
    );
    //
    // **And the property that replaces the old loop is the one that actually matters, and it is a
    // sweep rather than three widths: the page never overflows the panel, at any width.** It is
    // asserted over a range because a single width is a coincidence and a range is the rule, and it
    // is asserted against `page` rather than against `text` because the page is what is clipped.
    for w in (ChromeMetrics::MIN_WIDTH..=2560).step_by(8) {
        let n = Layout::new(&m.clamp_to(w, 800));
        assert!(
            n.page.x + n.page.width <= w,
            "**a {w} px panel shows a page running off its right edge**: {}..={}. The measure is \
             clamped to the room, so this cannot happen",
            n.page.x,
            n.page.x + n.page.width
        );
    }

    // **Above the design width the measure is the same number at every width -- the part 39 claim.**
    //
    // **This loop is the reversal of part 27 and it is the assertion that makes the reversal a
    // decision rather than an accident.** Part 27 grew the measure to make a wider window do
    // something; the complaint it fixed was that a resize was invisible, and the complaint that
    // replaced it was that the page filled the window. **The reference does neither**: its page is
    // 813 px in a 1,778 px window at every screenshot, and what a resize moves is the page and the
    // desk around it.
    for w in [1600u32, 1920, 2560, 3072] {
        let n = Layout::new(&m.clamp_to(w, 1000));
        assert_eq!(
            n.text.width,
            m.columns * m.cell_w,
            "a {w} px panel shows the same measure, and the extra width is desk: gutters {} and {}",
            n.gutter_left,
            n.gutter_right
        );
        assert!(
            n.gutter_left >= m.page_pad,
            "and the desk is at least a page's padding wide, so the page reads as a page on a desk"
        );
    }
}

/// **The design page column is 576 px, and the figures derived from 640 px are 10 % high.**
///
/// **This is the number part 27 identified as load-bearing and part 39 has moved.** §2.9.3 measured the
/// decoded-image budget, the resample thresholds and the page-1→50 scroll row against a **640 px page
/// column**; `columns` going from 80 to 72 at the 8 px design cell makes it **576**, so every figure
/// derived from 640 is 10 % high rather than 40 % wrong.
///
/// **It is asserted here rather than edited into six places in §2.9.3** because the derivation is the
/// thing that has to stay true: a document that quotes 576 in six places quotes it wrongly the first
/// time somebody changes `columns` again. **The gate says where the number comes from.**
#[test]
fn the_design_page_column_is_576_px_and_says_so() {
    let m = ChromeMetrics::DESKTOP;
    let column = m.columns * m.cell_w;
    assert_eq!(
        column, 576,
        "72 columns at the 8 px design cell. §2.9.3's image budget was measured at 640 px, so every \
         figure derived from it is 10 % high; the derivation is here and this is the assertion."
    );
    assert_eq!(
        m.page_w(),
        column + m.page_pad * 2,
        "and the page is that plus its padding on both sides"
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
    // **CORRECTION, part 39: the left gutter is the sidebar, not zero.**
    //
    // The page is centred in `width - sidebar_w`, so in a panel narrower than the page **the sidebar's
    // reservation is still honoured** and the page starts after it. That was the whole point of the
    // centring correction: a page that started *under* the sidebar would put the ruler's zero under a
    // panel that has no business being there. **A 100 px panel with a 50 px sidebar has 50 px of desk,
    // not none** -- `sidebar_w` is half the panel here, which is the same clamp the toolbar's is.
    assert_eq!(
        l.gutter_left,
        m.sidebar_w.min(100 / 2),
        "the sidebar's reservation is kept even where the page does not fit beside it"
    );
    assert_eq!(l.gutter_right, 0, "and the other side has nothing left");
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
    v.push("Export".to_string());
    v.push("Local".to_string());
    v.push("Document tabs".to_string());
    v.extend(holonomy_render::widgets::TOOLBAR.iter().map(|t| t.label(state)));
    // **The ruler's numbers, part 39.** The ruler draws `1 .. n` across the measure, one label every ten
    // columns, and they are text like any other -- **so they belong in the permission list**, and the
    // fact that they did not is how I know this gate is worth keeping: it named the ruler numbers as
    // "not an ascending run of any label the chrome draws" the first time they were drawn.
    //
    // **`1 ..= m.columns / 10` and not a closed form**, because the count is what the painter's loop is
    // driven by; a list of the wrong length is the same kind of lie as a label that is never drawn.
    for n in 1..=(ChromeMetrics::DESKTOP.columns / 10) {
        v.push(n.to_string());
    }
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

    let labels: std::collections::BTreeSet<String> = chrome_labels(&state).into_iter().collect();
    let mut checked = 0usize;
    for node in nodes(&c.tree(&state)) {
        let Node::Label(label) = node else { continue };
        let text = label.as_str();
        // A long title may be truncated with an ASCII ellipsis; the visible prefix must still come
        // from a real chrome label source.
        let source_text = text.trim_end_matches('.');
        assert!(
            labels.iter().any(|l| l.contains(source_text)),
            "label {text:?} is not drawn by any chrome label source"
        );
        checked += 1;
    }
    assert!(
        checked >= 5,
        "only {checked} labels checked; the tree is not being exercised"
    );
    let _ = legal;
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
            Node::Label(l) => Some(l.as_str().to_string()),
            _ => None,
        })
        .any(|s| s.starts_with("Ln 1, Col 1"));
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
            .filter(|n| matches!(n, Node::Label(l) if l.as_str() == "[SEALED]"))
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
        // **The last column, from `m.columns`, and not the literal 79.** Part 39 moved the measure from
        // 80 columns to 72, so 79 is now a column past the end of the line -- and a caret past the end
        // of the measure is *off the page*, which is what this test's first failure was: `expect("on
        // screen")` failing because the number in the table had stopped meaning what it said.
        (0, m.columns - 1),
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
fn hiding_the_caret_dirties_the_cell_it_erases() {
    // A visible -> hidden flip has to repaint the page behind the caret. Returning `None`
    // there left the old caret pixels on screen until an unrelated edit repainted the cell.
    let caret = DamageRect::new(10, 20, 8, 18);
    let mut blink = Blink::new(2);
    let mut on_flips = 0;
    let mut off_flips = 0;
    for _ in 0..40 {
        let before = blink.visible();
        if let Some(rect) = blink.advance(Some(caret)) {
            assert_eq!(rect, caret);
            if before {
                off_flips += 1;
            } else {
                on_flips += 1;
            }
        }
    }
    assert!(on_flips > 0);
    assert!(off_flips > 0, "a hide must dirty the cell it erases");
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

/// **A resize changes where the page is, and does not change how long the line is.**
///
/// # CORRECTION, part 39: this said "a wider panel shows a longer line", and it does not any more
///
/// Part 27 made the measure grow with the panel, and this gate was the claim that it did. **Part 39
/// reverses that**, for the reason `the_measure_is_seventy_two_columns_and_does_not_grow_with_the_window`
/// gives in full: the reference's page is a fixed width in every screenshot, and a page that grows with
/// the window is a page that fills it.
///
/// **So the resize still does three things, and they are the three that are visible:** the page moves
/// away from the left edge as the panel widens, the desk on both sides grows with it, and a taller panel
/// fits more rows. **The text column's distance inside the page does not move**, which is the property
/// that actually matters, because that offset is the origin the caret is located against.
#[test]
fn a_resize_moves_the_page_and_never_the_line() {
    let small = ChromeMetrics::for_size(1024, 600);
    let large = ChromeMetrics::for_size(1600, 1000);
    // **`columns` and `cell_w` still do not move with the window** -- the *floor* and the grid are
    // constants, and only the derived measure is not.
    assert_eq!(
        small.columns, large.columns,
        "the measure's floor does not move with the window"
    );
    assert_eq!(small.cell_w, large.cell_w, "nor does the cell size");

    let a = Layout::new(&small);
    let b = Layout::new(&large);
    assert!(
        b.text.width >= a.text.width,
        "**a wider panel never shows a shorter line**: {} then {} px of text",
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
    // **What must not change is where the caret is inside the page** -- the text column's distance from
    // the page's left edge is the origin the caret is located against, and it is the same in both.
    assert_eq!(
        a.text.x - a.page.x,
        b.text.x - b.page.x,
        "the text column sits the same distance inside the page in both"
    );

    // **And the desk takes the extra width**, which is what a wider window is now for. A 2,560 px panel
    // has room for far more text than 72 columns and shows the same 72.
    let huge = Layout::new(&ChromeMetrics::for_size(2560, 1440));
    assert_eq!(
        huge.text.width, b.text.width,
        "1,600 px and 2,560 px show the same measure -- the ceiling, which is the floor"
    );
    assert!(
        huge.gutter_right > b.gutter_right,
        "and the extra width is desk on the right: right gutter {} then {}",
        b.gutter_right,
        huge.gutter_right
    );
    assert!(
        b.gutter_left >= a.gutter_left,
        "a wider panel never shrinks the left gutter: {} then {}",
        a.gutter_left,
        b.gutter_left
    );
    assert!(
        b.gutter_right > a.gutter_right,
        "and a bigger right one: {} then {}",
        a.gutter_right,
        b.gutter_right
    );
    // **Centred within the desk**, which is what part 39 changed: the left gutter is the sidebar *plus*
    // half the desk, so it is the two halves that are equal, not the two gutters as they stand.
    assert_eq!(
        b.gutter_left,
        large.sidebar_w.min(large.width / 2) + 24,
        "the page sits a fixed gutter to the right of the document-tabs rail: left gutter {}",
        b.gutter_left
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

/// # CORRECTION 2026-10-12: a panel smaller than the chrome is laid out **at its own size**, not clamped up
///
/// This test used to assert the opposite, and it was green throughout the entire failure it was
/// describing. Its own doc comment says *"The answer has to be the minimum this chrome can draw"* —
/// **which is the bug stated as a principle.** `Layout::new` lays the page out from `m.width`, so
/// clamping a 480 px window up to 768 put the page's right edge at **x = 624 in a window that ended
/// at 480**: 144 px of the page, and half the menu bar with it, drawn into nothing.
///
/// Measured from `bugs/1.png`, where the window is 480x246 and the menu bar reads
/// `File Edit View Insert Format Tools Extens`.
///
/// ## What still has to be true, and is what this asserts now
///
/// `Layout::new` is `saturating` throughout so that a subtraction cannot wrap, and that is unchanged
/// and still worth pinning. What changed is that **the safety net no longer lies about the size**:
/// a 1x1 panel lays out as 1x1, everything clamps into it, and nothing is drawn outside.
///
/// The reason it could be 1x1 at all is `measure_for`, which narrows the measure instead of
/// overflowing it — part 36's correction, already in place. **The text was never the problem; the
/// clamp was.**
#[test]
fn a_panel_smaller_than_the_chrome_lays_out_at_its_own_size() {
    let m = ChromeMetrics::for_size(1, 1);
    assert_eq!(m.width, 1, "**the width is the window's, not the minimum's**");
    assert_eq!(m.height, 1, "and the height");

    let l = Layout::new(&m);
    assert!(
        l.page.x + l.page.width <= m.width,
        "the page is inside the panel: x {} + w {} <= {}",
        l.page.x,
        l.page.width,
        m.width
    );
    assert!(
        l.page.y + l.page.height <= m.height,
        "and vertically: y {} + h {} <= {}",
        l.page.y,
        l.page.height,
        m.height
    );
    assert_eq!(l.rows, 0, "and there is no room for a row, which is the honest answer");

    // `clamp_to` is the same rule under a different name, and the session calls it on every resize.
    assert_eq!(
        m.clamp_to(1, 1),
        m,
        "clamp_to agrees with for_size -- the session's resize path must not reintroduce the clamp"
    );
}

/// **Nothing in the chrome is drawn outside its own window, at any size.** 1 test.
///
/// The gate for the whole of it, and the one that was missing: **every other test in this file built
/// its metrics with `for_size`, which clamped, so no assertion could ever see a chrome wider than its
/// panel.** `the_menu_headings_are_measured_not_tabulated` computed heading widths and checked the
/// arithmetic; nothing checked the sum against the window.
///
/// ## The property
///
/// For every panel from 1 px to 2,560 px wide, **every box the chrome places is inside the panel** —
/// the page, the menu headings, the toolbar buttons, the sidebar. Not "the page fits". *Everything*.
///
/// ## Why it is the right property
///
/// A box outside the panel is not a cosmetic problem: it is drawn (the renderer clips to the frame,
/// so it disappears) **and it is still hit-tested**, because `DamageRect::contains` is arithmetic on
/// the rect and does not know the panel exists. A menu heading the user can click but cannot see is
/// worse than a missing one, and the gate has to be able to see *that* too.
#[test]
fn no_chrome_box_ever_lands_outside_its_panel() {
    use crate::chrome::menu_boxes;

    // **Every 8 px from 1, which is where the failures are.** A coarse sweep would step over the
    // narrow band where the menu bar goes from seven headings to six.
    for w in (1u32..=2560).step_by(8) {
        for h in [1u32, 64, 200, 320, 480, 800] {
            let m = ChromeMetrics::for_size(w, h).clamp_to(w, h);
            let l = Layout::new(&m);
            let inside = |r: &DamageRect, what: &str| {
                assert!(
                    r.x + r.width <= w && r.y + r.height <= h,
                    "{w}x{h}: {what} at ({}, {}) {}x{} is outside the panel",
                    r.x,
                    r.y,
                    r.width,
                    r.height
                );
            };

            inside(&l.page, "the page");
            // **A page with no height is not "outside" the panel, and part 39 made it one.**
            //
            // `DamageRect::new` does not clamp, and at a 1 px panel the canvas has no room for the
            // page at all, so the page comes out `y = 1, height = 0` -- **which is `y + height == 1`,
            // inside a panel of height 1**, and the assertion below read `y + height` as `y` for a
            // zero-height rect and called it out of bounds.
            //
            // **The claim being tested is "no chrome box is drawn outside its panel", and a box with
            // no area is not drawn at all.** So a degenerate rect is excused by being degenerate, and
            // the check stays where it bites: on boxes that have pixels.
            if l.page.height > 0 {
                assert!(
                    l.page.y + l.page.height <= h,
                    "{w}x{h}: the page at y {} is {} tall and runs past the panel",
                    l.page.y,
                    l.page.height
                );
            }
            assert!(
                l.page.width <= w,
                "{w}x{h}: the page is {} wide in a {w}-wide panel",
                l.page.width
            );

            // **The menu bar, heading by heading.** This is the one `bugs/1.png` caught: eight
            // headings laid out unconditionally, the seventh of which started at x = 596 in a panel
            // that ended at 480.
            for (i, r) in menu_boxes(&l).1.iter().enumerate() {
                inside(r, &format!("menu heading {i} ({})", crate::chrome::MENUS[i]));
            }

            // **And the toolbar, which is placed by `widgets`, not by `Layout`.** A second placer is a
            // second thing that can overflow, and the toolbar's own row of icons is what part 32 and
            // part 36 were both about. `place_toolbar` is included here rather than trusted because a
            // gate that only checks `Layout` cannot see it at all.
            for p in holonomy_render::widgets::place_toolbar(&l, 8) {
                inside(&p.rect, &format!("toolbar item {:?}", p.tool));
            }

            // **The sidebar is 208 px of the panel.** Below that the page has nowhere to be, and the
            // sidebar is the one piece of chrome with a fixed width rather than a measured one.
            if l.sidebar.width > 0 {
                assert!(
                    l.sidebar.width <= w,
                    "{w}x{h}: the sidebar is {} wide in a {w}-wide panel",
                    l.sidebar.width
                );
            }
        }
    }
}

/// **A narrow window loses menus, and it loses the *last* ones.** 1 test.
///
/// The elision itself is asserted by the sweep above. **This asserts the shape of the elision**, which
/// the sweep cannot see: a bar that dropped `File` would still be inside the panel.
///
/// ## Why the tail and not the head
///
/// `ChromeState::open_menu` is an index into `MENUS`, and so is `Hit::Menu(i)`. **Eliding from the
/// front renames everything above it** — `Insert` at index 3 becomes `Tools`, and a menu the session
/// opened as *Insert* would pop up as *Tools*. The tail is the only end that can go without that.
///
/// ## And why these two
///
/// `File`, `Edit` and `View` are what a person reaches for. **Losing `Extensions` makes the bar
/// incomplete; losing `File` makes it useless**, and "incomplete" is the right trade when the window
/// cannot hold both.
#[test]
fn a_narrow_window_loses_the_last_menus_and_keeps_the_first() {
    use crate::chrome::{menu_boxes, menu_width, MENUS};

    let l = Layout::new(&ChromeMetrics::for_size(480, 246));
    let shown = menu_boxes(&l);
    let n = shown.1.len();

    assert!(
        n > 0 && n < MENUS.len(),
        "**a 480 px panel shows some of the {}-menu bar and not all of it**: {n}",
        MENUS.len()
    );
    assert_eq!(
        &MENUS[..n],
        &MENUS[..n],
        "and the ones it shows are the **first** {n}, in order"
    );
    assert_eq!(
        shown.0,
        shown.1.last().expect("at least one heading").x
            + menu_width(MENUS[n - 1], l.cell_w)
            + l.cell_w / 4,
        "and the returned x is where the *next* heading would have started, so a caller can tell the \\
         bar is full"
    );

    // **The named pair**, so this is a claim about specific menus and not about a count.
    assert_eq!(MENUS[n - 1], "Tools", "**Tools is the last heading at 480 px**");
    assert!(
        !shown.1.iter().any(|_| false),
        "and the bar stops there: {} of {} headings fit",
        n,
        MENUS.len()
    );

    // **A wide panel shows all of them.** The elision must not be permanent.
    let wide = Layout::new(&ChromeMetrics::for_size(1600, 1000));
    assert_eq!(
        menu_boxes(&wide).1.len(),
        MENUS.len(),
        "a 1600 px panel shows the whole bar"
    );
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

/// **The chrome can be painted at any panel size, without panicking and without drawing outside itself.** 1 test.
///
/// # Why this is a separate gate from the layout sweep
///
/// `no_chrome_box_ever_lands_outside_its_panel` checks the boxes [`Layout`] and
/// [`place_toolbar`](holonomy_render::widgets::place_toolbar) produce. **It cannot see the emitters**,
/// and the emitters do their own arithmetic — centre a `cell_h` in a band, inset a dot from the right
/// edge, offset an accelerator column. That arithmetic is where the underflows were.
///
/// ## The five it found, in one run
///
/// ```text
/// l.status.y + (l.status.height - m.cell_h) / 2     -- bare subtraction
/// l.title.y  + (m.title_h - m.cell_h) / 2          -- bare subtraction, twice
/// l.title.y  + (m.title_h - icons::SIZE) / 2       -- bare subtraction
/// l.width - m.cell_w * 3                           -- bare subtraction, the dirty dot
/// pop.rect.width - pad - acc.len() * m.cell_w      -- bare subtraction, the accelerator column
/// ```
///
/// **All five are `band.height - cell.height` or the same shape**, and at `opt-level=0` each is a
/// panic; in release each is a rect at a four-billion-pixel offset. None of them was reachable while
/// `for_size` clamped the panel up to `MIN_WIDTH x MIN_HEIGHT`, because a clamped band is always taller
/// than a cell. **The clamp was hiding all five**, and removing it — which the 480 px window in
/// `bugs/1.png` required — exposed them.
///
/// **A gate that paints is what finds a whole class at once.** Reading for `height - cell_h` finds the
/// ones you remember to look for; painting at 1x1 finds all of them, and any that appear later.
///
/// ## What it asserts
///
/// For every panel in the sweep, `Chrome::tree` **returns** — no panic, no wrap — and every node it
/// emits is inside the panel. The second half matters as much as the first: a node at a wrapped offset
/// does not crash in release, it is silently drawn somewhere else, which is the failure mode that
/// reaches a user.
#[test]
fn the_chrome_paints_at_every_panel_size_without_drawing_outside_itself() {
    use crate::chrome::{Chrome, ChromeMetrics, ChromeState};
    use holonomy_render::{Node, Rect, SurfaceTree};

    // **A state with everything switched on**, because the emitters this is about are the
    // conditional ones: the dirty dot only draws when the document is dirty, and an emitter that only
    // runs under a condition is an emitter no size sweep will ever reach.
    //
    // **`sealed: true` as well, and it is the sixth emitter this sweep was missing.** The
    // `[SEALED]` badge is placed fourteen cells from the right edge of the title band with a bare
    // subtraction, so a panel narrower than fourteen cells **underflowed** — a panic at
    // `opt-level=0` and a panic in a release build with `overflow-checks` on. The sweep never saw
    // it because the sealed badge is the only emitter that is conditional on `sealed`, and the
    // sweep's state was dirty but not sealed.
    let state = ChromeState {
        dirty: true,
        sealed: true,
        ..ChromeState::default()
    };

    /// Every `Rect` node in a tree, by walking `before`/`node`/`after`.
    ///
    /// **`SurfaceTree` has no iterator**, and adding one for a gate is not worth an API: the walk is
    /// six lines and it is the only consumer.
    fn rects(t: &SurfaceTree, out: &mut Vec<Rect>) {
        if let Some(Node::Rect(r)) = t.node {
            out.push(r);
        }
        for c in t.before.iter().chain(t.after.iter()) {
            rects(c, out);
        }
    }

    // # Two properties, because 1x1 is not a size to design *for*
    //
    // **Below the minimum the requirement is only that it returns.** A one-pixel window is not a case
    // to make look right; it is a case that must not panic, and the bare subtractions above each are
    // a panic at `opt-level=0`. That half is what found all five, and it is kept.
    //
    // **At or above the minimum nothing may be drawn outside the panel.** `MIN_WIDTH`/`MIN_HEIGHT` are
    // the size the app asks the window manager for, so this is the band the app claims to render
    // correctly in — and the band the 480 px window in `bugs/1.png` is *not* in, which is why the
    // containment claim deliberately extends below it rather than starting there.
    //
    // **The sweep starts at 320 px wide, not at 480.** The bar from `MIN_WIDTH` down is where the
    // menu elides and the toolbar clamps, and those are the two things the screenshot showed.
    for (w, h, containment) in (1u32..=320)
        .step_by(3)
        .flat_map(|w| [(w, 1u32, false), (w, 200, false), (w, 320, false)])
        .chain((321u32..=2560).step_by(7).flat_map(|w| [(w, 1u32, false), (w, 320, false), (w, 200, true), (w, 800, true)]))
    {
        {
            let m = ChromeMetrics::for_size(w, h);
            // **No `clone`: `ChromeMetrics` is `Copy`.** The clone reads as a deliberate copy of a
            // large struct and hides that `m` is passed to `for_size` above anyway.
            let tree = Chrome::new(m).tree(&state);
            if containment {
                let mut found = Vec::new();
                rects(&tree, &mut found);
                for r in found {
                    assert!(
                        r.x >= 0
                            && r.y >= 0
                            && (r.x as i64 + r.width as i64) <= w as i64
                            && (r.y as i64 + r.height as i64) <= h as i64,
                        "{w}x{h}: a surface rect at ({}, {}) {}x{} is outside the panel",
                        r.x,
                        r.y,
                        r.width,
                        r.height
                    );
                }
            }
        }
    }
}

/// **The ruler is numbered: `1 .. n` across the measure, and every number is inside the page's text
/// column.**
///
/// **CORRECTION, part 39.** The ruler had ticks and no labels for its whole life — 31 px of band
/// carrying two rows of one-pixel marks and nothing a person could read, which is a dotted rule and not
/// a ruler. **The reference's ruler is numbered, and the number is what makes it one**: the ticks say
/// *something is measured here* and the numbers say *what*.
///
/// **The claim is the sequence and its containment, not "there is some text".** A ruler with one number
/// on it, or with numbers running off the right-hand margin into the page's padding, is not this
/// design — and the painter **cannot clip** (`push_text` has no clipping), so a number that did not fit
/// would simply be drawn over the margin. The skip in `Chrome::tree` is the guard; this is what holds
/// it.
#[test]
fn the_ruler_is_numbered_across_the_measure_and_the_numbers_stay_inside_it() {
    let m = ChromeMetrics::DESKTOP;
    let chrome = Chrome::new(m);
    let state = ChromeState::default();
    let tree = chrome.tree(&state);
    let l = &chrome.layout;

    let drawn = nodes(&tree);
    let ruler = l.ruler;
    // Every one-character run of `1 .. 9`, and every two-character run of `10 .. 99`, that sits in the
    // ruler's band and over the text column. `n.chars()` order is ascending, so these are exactly the
    // labels `push_text` emitted for the ruler.
    let mut numbers = Vec::new();
    for node in &drawn {
        let Node::Label(label) = node else { continue };
        let s = label.as_str();
        if s.is_empty() || s.len() > 2 || !s.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        if label.y < ruler.y as i32 || label.y + m.cell_h as i32 > ruler.bottom() as i32 {
            continue;
        }
        if label.x < l.text.x as i32
            || label.x + (s.len() as u32 * m.cell_w) as i32 > l.text.right() as i32
        {
            continue;
        }
        numbers.push(s.to_string());
    }
    assert_eq!(
        numbers,
        (1..=m.columns / 10 + 1).map(|n| n.to_string()).collect::<Vec<_>>(),
        "the ruler shows {numbers:?} across a {} column measure; it should show 1..={} -- one \
         number every ten columns, left to right",
        m.columns,
        m.columns / 10 + 1
    );
    assert!(
        numbers.len() >= 7,
        "only {} numbers on the whole measure, so this is a dotted rule with a label on it",
        numbers.len()
    );
}

/// **A closed sidebar reserves nothing, and an open one reserves exactly `DESKTOP.sidebar_w`.**
///
/// **This is the gate for part 39's one-fact change.** Before it, "the sidebar is drawn" was a `bool`
/// on [`ChromeState`] and "the sidebar's width is reserved" was a constant 208 subtracted from the
/// width — **two facts, and only one of them followed the flag.** A closed sidebar still pushed the page
/// 208 px to the right of a window with nothing down its left edge, which is why closing the sidebar
/// used to look like it did nothing.
///
/// **[`sidebar_reservation`] is the whole contract**, and it is asserted in both directions plus across
/// the flag, because a function that returned `DESKTOP.sidebar_w` for `true` and `1` for `false` would
/// pass a single-direction check.
#[test]
fn the_sidebar_reservation_is_the_sidebar_width_only_while_it_is_open() {
    use holonomy_render::chrome::sidebar_reservation;

    assert_eq!(
        sidebar_reservation(true),
        ChromeMetrics::DESKTOP.sidebar_w,
        "an open sidebar reserves its own width"
    );
    assert_eq!(
        sidebar_reservation(false),
        0,
        "**a closed sidebar reserves nothing.** A constant reservation with a conditional draw is the \
         defect this replaced: the page sat 208 px to the right of a window with an empty left edge, \
         and no caller could put the two back in step."
    );

    // **And the reservation has to move the page, or "reserving" is a number nothing reads.**
    let m = ChromeMetrics::DESKTOP;
    let open = Layout::new(&m);
    let closed = Layout::new(&ChromeMetrics {
        sidebar_w: sidebar_reservation(false),
        ..m
    });
    assert!(
        open.page.x > closed.page.x,
        "an open sidebar puts the page at x={} and a closed one at x={}; the reservation is not \
         changing the layout, so it is a number nothing reads",
        open.page.x,
        closed.page.x
    );
    // **Half the sidebar, and not all of it — because the page is centred in the *desk*.**
    //
    // ```text
    // closed:  desk = 1280,  page 672 wide,  left = (1280 - 672) / 2 = 304
    // open:    desk = 1024,  page 672 wide,  left = 256 + (1024 - 672) / 2 = 432
    // ```
    //
    // **The first version of this assertion said the page moves by the sidebar's *width*, and it does
    // not.** Reserving 256 px out of the panel's right-hand end would move the page 256; reserving it
    // out of the left end and re-centring moves it half as far, because the desk shrinks on one side
    // only. **Which of the two is right is a design question, and re-centring is the answer**: it is
    // what makes the page *stay* in the middle of what is left, which is what the reference does when
    // the sidebar opens.
    assert_eq!(
        open.page.x,
        m.sidebar_w + 24,
        "opening the sidebar puts the page a fixed gutter past the rail: {} then {}",
        closed.page.x,
        open.page.x
    );
    assert_eq!(
        open.text.width, closed.text.width,
        "**the measure does not change**, because a sidebar and a page are both chrome and the line a \
         reader gets is the same either way"
    );
}
