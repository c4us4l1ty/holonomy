//! Integration tests for pure-Rust SFNT `loca`/`glyf` simple + compound outline extraction,
//! 4x4 A8 scanline rasterization, and embedded [`FontSet`] layout parity (`L-8`, `UI-SPEC.md`).

use holonomy_eye::{
    layout_styled, Fixed, FontSet, FontVariant, Point, Shaper, Style, StyledRun, StyledText, View,
};
use holonomy_text::FontFamily;

#[test]
fn embedded_fonts_rasterize_simple_and_compound_glyphs_accurately() {
    let fonts = FontSet::embedded(15).expect("embedded OFL fonts parse cleanly");
    assert_eq!(fonts.body_px(), 15);

    // Proportional Inter vs monospace JetBrains Mono advances.
    let w_i = fonts.advance("i", Style::DEFAULT);
    let w_m = fonts.advance("m", Style::DEFAULT);
    assert!(w_m > w_i, "Inter must be proportional: i={w_i:?} m={w_m:?}");

    let mono_style = Style::DEFAULT.with_family(FontFamily::Mono);
    assert_eq!(
        fonts.advance("i", mono_style),
        fonts.advance("W", mono_style),
        "JetBrains Mono must assign equal advances"
    );

    // Uppercase 'H' placement relative to baseline (D-32 vertical shift regression guard).
    let bmp_h = fonts
        .rasterize_char(Style::DEFAULT, 1_000, 96, 'H')
        .expect("'H' rasterizes");
    let ascent_px = fonts.ascent(Style::DEFAULT).sixtyfourths() as f32 / 64.0;
    assert!(bmp_h.width >= 6 && bmp_h.height >= 8);
    assert!(bmp_h.bearing_y > 6 && (bmp_h.bearing_y as f32) <= ascent_px + 1.0);
    assert!(
        bmp_h.coverage.iter().any(|&c| c >= 200),
        "'H' must have solid ink"
    );

    // Compound accented glyphs ('é', 'ñ', 'ü') produce taller bitmaps than base 'e' with non-zero diacritic ink.
    let bmp_e = fonts
        .rasterize_char(Style::DEFAULT, 1_000, 96, 'e')
        .expect("'e' rasterizes");
    for ch in ['é', 'ñ', 'ü', 'à'] {
        let bmp = fonts
            .rasterize_char(Style::DEFAULT, 1_000, 96, ch)
            .unwrap_or_else(|| panic!("compound glyph {ch:?} must rasterize"));
        let ink_sum: u64 = bmp.coverage.iter().map(|&c| u64::from(c)).sum();
        assert!(ink_sum > 500, "compound glyph {ch:?} has too little ink");
        if ch == 'é' {
            assert!(
                bmp.bearing_y > bmp_e.bearing_y,
                "accented 'é' top ({}) must rise above 'e' ({})",
                bmp.bearing_y,
                bmp_e.bearing_y
            );
        }
    }

    // Non-zero winding rule must preserve the interior counter of 'O' at high scale.
    let bmp_o = fonts
        .rasterize_char(Style::DEFAULT, 2_000, 96, 'O')
        .expect("'O' at 2x rasterizes");
    let cx = bmp_o.width / 2;
    let cy = bmp_o.height / 2;
    let center_alpha = bmp_o.coverage[cy * bmp_o.width + cx];
    assert_eq!(center_alpha, 0, "interior counter of 'O' must stay empty");

    // Bold variant covers more ink than Regular on 'M', and Italic + Mono variants resolve.
    let reg_m = fonts
        .rasterize_char(Style::DEFAULT, 1_500, 96, 'M')
        .expect("reg M");
    let bold_m = fonts
        .rasterize_char(Style::DEFAULT.bold(), 1_500, 96, 'M')
        .expect("bold M");
    let reg_ink: u64 = reg_m.coverage.iter().map(|&c| u64::from(c)).sum();
    let bold_ink: u64 = bold_m.coverage.iter().map(|&c| u64::from(c)).sum();
    assert!(
        bold_ink > reg_ink,
        "bold 'M' ink ({bold_ink}) should exceed regular 'M' ({reg_ink})"
    );
    assert_eq!(
        fonts.resolve_variant(Style::DEFAULT.italic(), 'g').0,
        FontVariant::Italic
    );
    assert_eq!(fonts.resolve_variant(mono_style, 'x').0, FontVariant::Mono);
}

#[test]
fn font_set_preserves_single_geometry_hit_test_across_styles_and_zooms() {
    let fonts = FontSet::embedded(15).unwrap();
    let sample = "Quiet Markdown: café, **bold**, code(); end.";
    let styles = [
        StyledRun {
            start: 0,
            style: Style::DEFAULT,
        },
        StyledRun {
            start: 15,
            style: Style::DEFAULT.italic(),
        },
        StyledRun {
            start: 21,
            style: Style::DEFAULT.bold(),
        },
        StyledRun {
            start: 31,
            style: Style::DEFAULT.code_span(),
        },
        StyledRun {
            start: 40,
            style: Style::DEFAULT,
        },
    ];
    let styled = StyledText::new(sample, &styles);
    let layout = layout_styled(styled, 0, Fixed::units(280), &fonts);

    for zoom in [750, 1_000, 1_250, 1_500, 2_000] {
        for dpi in [96, 144] {
            let view = View::new(zoom, dpi).expect("positive zoom and dpi");
            for (idx, _) in sample.char_indices() {
                let offset = u64::try_from(idx).expect("offset fits u64");
                let dev = view
                    .paint_point(&layout, offset)
                    .expect("char index sits on cluster boundary");
                let half_h = dev.height.sixtyfourths() / 2;
                let mid_y = Fixed::from_64ths(dev.top.sixtyfourths() + half_h);
                let caret = view
                    .hit_test(&layout, Point::new(dev.x, mid_y))
                    .expect("hit_test finds device point");
                assert_eq!(
                    caret.byte, offset,
                    "round-trip drifted at zoom={zoom} dpi={dpi} byte={idx}"
                );
            }
        }
    }
}
