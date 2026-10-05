//! Probe: what line box does each packed face actually need?
//!
//! `Painter::cell_height` is `ppem + 2` = 18 px, and every caller treats `run.y` as the line box
//! top. `raster.rs` stores `bearing_y = -(y0 + bh)`, so today the painter draws ink at
//! `run.y - 17..-21` -- above the box. This prints, per face, the ink extent of a representative
//! sample so the *corrected* bearing and the line box that contains it can be chosen from
//! measurement rather than from the assumption that 18 px is enough.
//!
//! Run: `cargo run --release --example probe_linebox -p holonomy-assets`.

use holonomy_assets::payload::Style;

fn main() {
    let (atlas, _) =
        holonomy_assets::build_atlas(&[16]).expect("the atlas every test already builds");
    let ppem = 16u16;
    let cell_h_today = i32::from(ppem) + 2;
    println!("ppem={ppem}  cell_h today = {cell_h_today}\n");

    let faces = [
        ("Inter", Style::Regular),
        ("Bold", Style::Bold),
        ("Italic", Style::Italic),
        ("Mono", Style::Monospace),
        ("Math", Style::Math),
    ];
    // The extremes that decide a line box: a flat-topped ascender, a full descender, and the
    // parentheses/operators, which in a math face overshoot the Latin by a wide margin.
    let probes: [(u32, char, &str); 8] = [
        (u32::from(b'H'), 'H', "cap"),
        (u32::from(b'b'), 'b', "ascender"),
        (u32::from(b'p'), 'p', "descender"),
        (u32::from(b'g'), 'g', "descender"),
        (u32::from(b'('), '(', "paren"),
        (u32::from(b'2'), '2', "digit"),
        (0x03B1, '\u{3b1}', "alpha"),
        (0x2211, '\u{2211}', "sum"),
    ];

    // Every glyph in each face, so the answer is a property of the face and not of the sample.
    let mut worst_asc = 0i32; // ink height above the baseline
    let mut worst_desc = 0i32; // ink depth below the baseline
    println!(
        "{:<6} {:<5} {:>3} {:>3} {:>9} {:>9}",
        "glyph", "what", "w", "h", "bearing_y", "ink rows"
    );
    for (_name, style) in faces {
        for (cp, ch, what) in probes {
            let m = atlas.metric(cp, style, ppem);
            if m.is_blank() {
                continue;
            }
            let stored = i32::from(m.bearing_y);
            // Undo today's encoding to recover the flattener's ink box: `-(y0 + bh)`.
            let y0 = -stored - i32::from(m.height);
            let _ = y0;
            // In the flattener y runs downward from the ascender line, so `-(y0 + bh)` is the
            // distance from the ascender line up to the ink's *bottom* edge. Ink height above the
            // baseline follows once the face ascender is known; here report the raw span so the
            // per-face range is visible.
            println!(
                "{ch:<6} {what:<5} {:>3} {:>3} {stored:>9} {:>9}",
                m.width,
                m.height,
                format!("{}..{}", stored, stored + i32::from(m.height)),
            );
            let _ = (&mut worst_asc, &mut worst_desc);
        }
        println!();
    }

    // Decode the real ascent: for a glyph, ink bottom above the ascender line is `-bearing_y`, and
    // ink top above it is `-bearing_y - height`. The face ascender in pixels is unknown here, but
    // `ppem` is the only sane baseline placement for a `ppem + 2` box, so report what each glyph
    // needs *relative to a baseline `ppem` below the box top*.
    println!(
        "Under the corrected bearing (`y0 - (ascender - ppem)`), a glyph's ink top lands at\n\
         `ppem + bearing_y_corrected` rows below the box top. Today's value is `bearing_y` itself."
    );
    for (name, style) in faces {
        let mut lo = i32::MAX;
        let mut hi = i32::MIN;
        for cp in 0x20u32..0x100 {
            let m = atlas.metric(cp, style, ppem);
            if m.is_blank() {
                continue;
            }
            let stored = i32::from(m.bearing_y);
            lo = lo.min(stored);
            hi = hi.max(stored + i32::from(m.height));
        }
        println!(
            "  {name:<6} ASCII ink currently spans rows {lo}..{hi} of a {cell_h_today} px box; \
             a box containing it needs {need} px",
            need = hi - lo
        );
    }
}
