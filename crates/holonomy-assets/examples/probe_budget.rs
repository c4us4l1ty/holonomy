//! Probe: what a boot-rasterised, pruned math face actually costs, measured rather than assumed.
//!
//! Run twice: once with `MATH_RANGES` as shipped, once narrowed to the parser's symbol set. The
//! `apply_insert`-style discipline matters here -- the numbers are only evidence if they come from
//! the real rasteriser and the real builder, which is why this drives `build_atlas` and not a
//! re-derivation of its arithmetic.
//!
//! The directive's figure ("402 KiB now, 415 KiB after, 97 KiB to spare") compares `used()` --
//! glyph ink -- against the ceiling, while `tests/phase4_gate.rs` asserts
//! `coverage().len() + table`, where `coverage()` is the *allocated* bitmap. So this prints both,
//! plus the two terms the directive does not mention: the metric table's growth from a third
//! codepoint window and a fifth style.

use holonomy_assets::{metric, payload};
use std::mem::size_of;

fn main() {
    let sizes = [16u16, 22u16];
    let ceiling = 512 * 1024usize;

    println!("== MATH_RANGES as compiled");
    println!("   {:?}", payload::MATH_RANGES);
    let declared: usize = payload::codepoints_in_math_ranges().count();
    println!("   {declared} codepoints declared");

    let (a, _) = holonomy_assets::build_atlas(&sizes).expect("the shipped atlas fits");
    let table = a.metrics().len() * size_of::<metric::GlyphMetric>();
    let alloc = a.coverage().len();
    let ink = a.used();

    println!();
    println!("== what the gate measures");
    println!(
        "   coverage allocated {alloc} ({:.1} KiB)",
        alloc as f64 / 1024.0
    );
    println!(
        "   glyph ink (used)   {ink} ({:.1} KiB)",
        ink as f64 / 1024.0
    );
    println!("   metric table       {table}");
    println!(
        "   combined           {} ({:.1}% of ceiling)",
        alloc + table,
        100.0 * (alloc + table) as f64 / ceiling as f64
    );
    println!("   headroom           {}", ceiling - (alloc + table));

    // What the math face costs in coverage, attributed: the four text faces plus the procedural
    // Box Drawing, with the math face left out. `raster.rs`'s note on `glyphs_of` cites this number.
    let without_math = {
        let mut builder = holonomy_assets::atlas::AtlasBuilder::new(&sizes).expect("a builder");
        let mut input = payload::PACKED_FONTS;
        let mut out = Vec::new();
        {
            use std::io::Read;
            brotli_decompressor::Decompressor::new(&mut input, 4096)
                .read_to_end(&mut out)
                .expect("decompress");
        }
        for e in payload::FACES.iter() {
            if e.style == payload::Style::Math {
                continue;
            }
            let face =
                ttf_parser::Face::parse(&out[e.offset as usize..(e.offset + e.length) as usize], 0)
                    .expect("a text face");
            holonomy_assets::raster::rasterize_face(&face, e, &sizes, &mut builder)
                .expect("a text face rasterises");
        }
        holonomy_assets::box_drawing::add_box_drawing(&sizes, &mut builder).expect("box drawing");
        let (_atlas, used, _) = builder.finish().expect("an atlas without math");
        used
    };
    println!();
    println!("== what the math face costs in coverage");
    println!("   text faces + procedural Box Drawing {without_math}");
    println!("   with the math face                  {}", a.used());
    println!(
        "   difference                          {}",
        a.used() - without_math
    );

    // Where the declared math codepoints land relative to the two metric windows.
    let inside: Vec<u32> = payload::codepoints_in_math_ranges()
        .filter(|&c| metric::slot_of(c).is_some())
        .collect();
    let outside: Vec<u32> = payload::codepoints_in_math_ranges()
        .filter(|&c| metric::slot_of(c).is_none())
        .collect();
    println!();
    println!("== declared math codepoints vs the metric windows");
    println!("   inside the existing windows  {}", inside.len());
    println!("   needing a new window        {}", outside.len());
    if !outside.is_empty() {
        println!(
            "   span {:#06X}..={:#06X}",
            outside.iter().min().unwrap(),
            outside.iter().max().unwrap()
        );
    }

    // The tightest window set that covers every out-of-window symbol: two Greek blocks, a narrow
    // arrow pair, and one operator block. PROJECT.md 2.9.2 point 3 proposed "Greek 144 + 32
    // hand-enumerated operators"; these are measured spans rather than that guess.
    let greek: Vec<u32> = outside
        .iter()
        .copied()
        .filter(|c| (0x370..0x400).contains(c))
        .collect();
    let arrows: Vec<u32> = outside
        .iter()
        .copied()
        .filter(|c| (0x2190..=0x21FF).contains(c))
        .collect();
    let ops: Vec<u32> = outside
        .iter()
        .copied()
        .filter(|c| !(0x370..0x400).contains(c) && !(0x2190..=0x21FF).contains(c))
        .collect();
    let span = |v: &[u32]| -> usize {
        match (v.iter().min(), v.iter().max()) {
            (Some(&lo), Some(&hi)) => (hi - lo + 1) as usize,
            _ => 0,
        }
    };
    let extra = span(&greek) + span(&arrows) + span(&ops);
    println!();
    println!("== a window set sized to the symbols actually needed");
    println!(
        "   Greek    {} needed, {} slots for {:#06X}..={:#06X}",
        greek.len(),
        span(&greek),
        greek.iter().copied().min().unwrap_or(0),
        greek.iter().copied().max().unwrap_or(0)
    );
    println!(
        "   arrows   {} needed, {} slots",
        arrows.len(),
        span(&arrows)
    );
    println!(
        "   ops      {} needed, {} slots for {:#06X}..={:#06X}",
        ops.len(),
        span(&ops),
        ops.iter().copied().min().unwrap_or(0),
        ops.iter().copied().max().unwrap_or(0)
    );
    println!(
        "   new slots total {extra} (window arithmetic must be contiguous, so this is a floor)"
    );

    println!();
    println!("== the table and the combined total");
    for styles in [4usize, 5] {
        for cps in [metric::CODEPOINTS, metric::CODEPOINTS + extra] {
            let t = sizes.len() * styles * cps * size_of::<metric::GlyphMetric>();
            let combined = alloc + t;
            println!(
                "   {styles} styles x {cps} cps: table {t:>6}, combined {combined} = {:>5.1}%{}",
                100.0 * combined as f64 / ceiling as f64,
                if combined <= ceiling { "" } else { "   OVER" }
            );
        }
    }

    // Coverage height that would leave room, at each style count.
    println!();
    println!("== coverage height that fits the 5-style table at {extra} new slots");
    let cps5 = metric::CODEPOINTS + extra;
    let t5 = sizes.len() * 5 * cps5 * size_of::<metric::GlyphMetric>();
    let max_cov = ceiling - t5;
    println!(
        "   table {t5} -> coverage must be <= {max_cov} = {} rows",
        max_cov / metric::ATLAS_STRIDE
    );
    for h in [416u16, 448, 456, 464, 472, 480] {
        let cap = metric::ATLAS_STRIDE * h as usize;
        let combined = cap + t5;
        println!(
            "   height {h:>3}: coverage {cap}, combined {combined} = {:>5.1}%{}",
            100.0 * combined as f64 / ceiling as f64,
            if combined <= ceiling {
                "   fits"
            } else {
                "   OVER"
            }
        );
    }

    // How full is the atlas today, as a fraction of *allocated* coverage? This is the number that
    // decides whether a shorter atlas could still hold its glyphs.
    println!();
    println!("== occupancy, which is what a shorter coverage has to absorb");
    println!(
        "   ink {ink} / allocated {alloc} = {:.1}%",
        100.0 * ink as f64 / alloc as f64
    );
    for h in [416u16, 448, 456, 464] {
        let cap = metric::ATLAS_STRIDE * h as usize;
        println!(
            "   at height {h}: today's ink alone would be {:.1}% of the arena",
            100.0 * ink as f64 / cap as f64
        );
    }
}
