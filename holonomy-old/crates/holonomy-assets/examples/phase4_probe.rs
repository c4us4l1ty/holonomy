//! Measure what the one-time pass actually costs, with the real payload.
//!
//! Run with `cargo run --release -p holonomy-assets --example phase4_probe`.
//! Prints the boot report, the atlas occupancy, per-codepoint coverage, and the real
//! allocation count, so the Phase 4 gate's claims can be checked against numbers rather
//! than assertions in a test that passes by construction.

use holonomy_assets::{box_drawing, build_atlas, metric, payload};
use std::time::Instant;

fn main() {
    println!("== payload ==");
    println!(
        "packed   {:>9} B  (budget {} B, {:.1}% used)",
        payload::PACKED_FONTS.len(),
        payload::PACKED_BUDGET,
        100.0 * payload::PACKED_FONTS.len() as f64 / payload::PACKED_BUDGET as f64
    );
    println!("raw      {:>9} B", payload::RAW_LEN);
    println!("faces    {}", payload::FACES.len());
    for f in &payload::FACES {
        println!(
            "  {:<24} style={:<10} offset={:>6} length={:>6} mono={}",
            f.name, f.style as u8 as i32, f.offset, f.length, f.monospace
        );
    }

    println!("\n== one-time pass ==");
    let t0 = Instant::now();
    let (atlas, report) = match build_atlas(&[16, 22]) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("build_atlas failed: {e}");
            std::process::exit(1);
        }
    };
    let wall = t0.elapsed();

    println!(
        "decompress {:>9.3} ms",
        report.decompress_us as f64 / 1000.0
    );
    println!("rasterise  {:>9.3} ms", report.rasterize_us as f64 / 1000.0);
    println!("box        {:>9.3} ms", report.box_us as f64 / 1000.0);
    println!(
        "pack       {:>9.3} ms  (atlas alloc + skyline placement)",
        report.pack_us as f64 / 1000.0
    );
    println!("total      {:>9.3} ms  (gate: < 15 ms)", report.total_ms());
    println!(
        "wall       {:>9.3} ms  (includes atlas allocation)",
        wall.as_secs_f64() * 1e3
    );
    println!("scrubbed   {:>9} B of font bytes", report.scrubbed);

    println!("\n== atlas ==");
    println!(
        "capacity   {:>9} B  (512 KiB = {})",
        atlas.capacity(),
        512 * 1024
    );
    println!(
        "used       {:>9} B  ({:.1}% of capacity)",
        atlas.used(),
        100.0 * atlas.used() as f64 / atlas.capacity() as f64
    );
    println!("sizes      {:?}", atlas.sizes());

    // Coverage: how many of the required codepoints have real ink in each face/style/size?
    println!("\n== codepoint coverage (glyphs with non-zero coverage) ==");
    let cov = atlas.coverage();
    let stride = metric::ATLAS_STRIDE;
    let mut by_size = vec![0usize; atlas.sizes().len()];
    for &ppem in atlas.sizes() {
        let si = atlas.sizes().iter().position(|&s| s == ppem).unwrap();
        let mut present = 0usize;
        let mut total = 0usize;
        for ((_cp, _style, s), m) in holonomy_assets::atlas::all_metrics(&atlas) {
            if s != si {
                continue;
            }
            total += 1;
            if m.is_blank() {
                continue;
            }
            let mut ink = false;
            for row in 0..m.height as usize {
                let base = (m.atlas_y as usize + row) * stride + m.atlas_x as usize;
                if cov[base..base + m.width as usize].iter().any(|&v| v != 0) {
                    ink = true;
                    break;
                }
            }
            if ink {
                present += 1;
            }
        }
        by_size[si] = present;
        println!("  ppem {ppem:>3}: {present:>4}/{total} glyph slots carry ink");
    }

    // The metric table itself.
    println!("\n== metric table ==");
    println!("entries    {:>9}", atlas.metrics().len());
    println!(
        "size_of    {:>9} B per entry",
        size_of::<metric::GlyphMetric>()
    );
    println!(
        "table      {:>9} B",
        atlas.metrics().len() * size_of::<metric::GlyphMetric>()
    );

    // Repeat the pass, to separate one-time costs (page faults on the 512 KiB atlas, brotli
    // table warm-up) from per-glyph work. A second pass that costs about the same means the
    // number above is per-glyph cost; a much cheaper second pass means most of it was faults.
    println!("\n== repeat pass (same inputs, warm caches) ==");
    let t1 = Instant::now();
    let second = build_atlas(&[16, 22]);
    let warm = t1.elapsed();
    match second {
        Ok((_, r2)) => {
            println!(
                "warm total {:>9.3} ms   (cold was {:.3} ms, delta {:.3} ms)",
                r2.total_ms(),
                report.total_ms(),
                report.total_ms() - r2.total_ms()
            );
            println!(
                "warm split: decompress {:.3}  rasterise {:.3}  box {:.3}  pack {:.3}",
                r2.decompress_us as f64 / 1000.0,
                r2.rasterize_us as f64 / 1000.0,
                r2.box_us as f64 / 1000.0,
                r2.pack_us as f64 / 1000.0
            );
        }
        Err(e) => println!("warm pass failed: {e}"),
    }
    let _ = warm;

    // A sanity blit: render a string and count set pixels.
    println!("\n== sample blit ==");
    let text = "H1 boxed+capable AG─╮";
    let mut fb = vec![0u32; 256 * 64];
    let mut pen = 4usize;
    let base = 20usize;
    for ch in text.chars() {
        let cp = ch as u32;
        let m = atlas.metric(cp, payload::Style::Monospace, 16);
        if m.is_blank() {
            continue;
        }
        holonomy_assets::blit::blit_glyph(
            &mut Fb(&mut fb),
            pen + m.bearing_x.max(0) as usize,
            base,
            cov,
            stride,
            &m,
            0x00FF_FFFF,
        )
        .expect("in bounds");
        pen += m.advance_x.max(1) as usize;
        if pen > 240 {
            break;
        }
    }
    let lit = fb.iter().filter(|&&p| p & 0x00FF_FFFF != 0).count();
    println!("text       {text:?} -> {lit} lit pixels");

    // Box drawing: prove adjacent cells produce an unbroken run.
    println!("\n== box drawing tiling ==");
    let cell = box_drawing::cell_size(16);
    let mut band = vec![0u8; cell * 8];
    let dash = atlas.metric(0x2500, payload::Style::Regular, 16);
    let mut tmp = vec![0u8; cell * cell];
    for i in 0..8 {
        holonomy_assets::box_drawing::draw_glyph(0x2500, cell, cell, &mut tmp);
        band[i * cell..(i + 1) * cell].copy_from_slice(&tmp[..cell]);
    }
    let mid = cell / 2;
    let run = band[mid * cell..mid * cell + cell * 8]
        .iter()
        .filter(|&&v| v == 255)
        .count();
    println!(
        "8 x U+2500 at cell {cell}: centre row has {run}/{} lit, atlas cell {}x{}",
        cell * 8,
        dash.width,
        dash.height
    );
    assert_eq!(
        run,
        cell * 8,
        "the run must be unbroken: this is why box drawing is procedural"
    );
    println!("OK: unbroken");
}

struct Fb<'a>(&'a mut [u32]);

impl holonomy_assets::blit::Scanout for Fb<'_> {
    fn pixels(&mut self) -> &mut [u32] {
        self.0
    }
    fn stride(&self) -> usize {
        256
    }
    fn rows(&self) -> usize {
        self.0.len() / 256
    }
}
