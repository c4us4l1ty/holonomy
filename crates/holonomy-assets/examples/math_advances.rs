//! Measure the atlas's real advances against `MathMetrics::cell_w`, for the glyphs a formula uses.
//!
//! `math_layout` positions every glyph in a whole cell of `cell_w` px, on the stated principle that
//! the layout is integers throughout so the gate can assert hand-computed numbers. That is only true
//! on screen if a glyph's *actual* advance is close to `cell_w`; if the face is proportional the two
//! disagree and glyphs overlap or gap. This prints the disagreement.
fn main() {
    let m = holonomy_assets::metric::CODEPOINTS;
    let atlas = holonomy_assets::build_atlas(&[16]).expect("atlas").0;
    let mut worst = 0i32;
    let mut rows = Vec::new();
    for (label, cp, style) in [
        ("b", 0x62u32, "Monospace"),
        ("x", 0x78, "Monospace"),
        ("a", 0x61, "Monospace"),
        ("c", 0x63, "Monospace"),
        ("2", 0x32, "Monospace"),
        ("4", 0x34, "Monospace"),
        ("-", 0x2D, "Monospace"),
        ("alpha", 0x3B1, "Math"),
        ("pm", 0xB1, "Math"),
        ("sum", 0x2211, "Math"),
        ("leq", 0x2264, "Math"),
        ("times", 0xD7, "Math"),
    ] {
        use holonomy_assets::payload::Style as PS;
        let s = if style == "Math" {
            PS::Math
        } else {
            PS::Monospace
        };
        let g = atlas.metric(cp, s, 16);
        let d = g.advance_x as i32 - 8;
        if d.abs() > worst {
            worst = d.abs();
        }
        rows.push((
            label,
            cp,
            g.advance_x,
            g.width,
            g.height,
            g.bearing_x,
            g.bearing_y,
            d,
        ));
    }
    println!("cell_w is 8 px; a script cell is 5 px (5/8 of 8)");
    println!(
        "{:>7} {:>10} {:>8} {:>7} {:>7} {:>8} {:>8} {:>10}",
        "glyph", "codepoint", "advance", "width", "height", "bear_x", "bear_y", "adv-8"
    );
    for (l, cp, a, w, h, bx, by, d) in &rows {
        // Where the painter puts the ink: it blits at run.y + bearing_y, `cell_h` = 18 rows tall,
        // and `cell_w` = 8 columns wide regardless of the metric's own width.
        println!("{l:>7} {cp:#010x} {a:>8} {w:>7} {h:>7} {bx:>8} {by:>8} {d:>8}   ink y rel baseline {:>4}..{:>4}",
                 -(*by as i32 + *h as i32), -*by as i32);
        if *w > 8 {
            println!(
                "        ^ WIDTH {} > painter cell_w 8: blit truncates to 8 columns",
                w
            );
        }
        if *h > 18 {
            println!("        ^ HEIGHT {} > painter cell_h 18: blit truncates", h);
        }
    }
    println!("\nworst deviation from cell_w: {worst} px");
    println!("CODEPOINTS {m}, TABLE slots per size = {m} * 5");
}
