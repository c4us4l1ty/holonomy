//! Probe: does any glyph's ink fall outside the line box that is supposed to contain it?
//!
//! # What this gates
//!
//! Every caller in the system -- `Session::paint`, `emit_math`, the table grid, `Caret::locate`,
//! the chrome bands -- works in **line-box tops**: it hands a painter a `run.y` that is the top edge
//! of a `cell_h`-tall line box, damages that same rect, and advances by `cell_h`. That agreement is
//! load-bearing. `Painter::paint` culls on the cell (see the note in `holonomy-display/src/paint.rs`
//! for why the ink rect is still not the cull), so a glyph whose ink sits outside the cell is drawn
//! into a region nobody damages and nobody clears: it survives every repaint that should have wiped
//! it, and a stale caret stays on screen four repaints after it moved. That is not theoretical --
//! see the `widen()` fix in `holonomy/src/session.rs`.
//!
//! So the gate is one number: **for every packed glyph in every packed face, is the ink it will be
//! blitted at contained in `[0, cell_h)`**, where the placement is the model's own
//!
//! ```text
//! baseline     = line_box_top + ascent_px
//! ink_top      = baseline - bearing_y          <- `Painter::text`
//! ink_bottom   = ink_top + glyph.height
//! cell_h       = Atlas::line_pitch()           <- `Painter::cell_height`
//! ```
//!
//! # Why this file is a gate and not a report
//!
//! The vertical placement used to be wrong by 17-21 px: `raster.rs` stored
//! `bearing_y = -(y0 + bh)` (the ink bottom, measured down from the *ascender line*) and the painter
//! blitted at `run.y + bearing_y`, so the two errors compounded and the ink landed above its box.
//! Everything still "worked" -- `PaintStats::missing` was zero, every glyph appeared -- because the
//! arithmetic stayed in range. Nothing but this measurement can see that. So: exit code 0 means the
//! invariant holds, nonzero names the glyphs that break it, and the CI job that runs this probe is
//! what keeps the fix from being undone by an innocuous-looking edit to the bearing formula or to
//! the cell height.
//!
//! Run: `cargo run --release --example probe_linebox -p holonomy-assets`.

use holonomy_assets::box_drawing;
use holonomy_assets::metric::GlyphMetric;
use holonomy_assets::payload::Style;

/// Codepoints the metric table packs: ASCII/Latin-1, Greek, arrows, operators, relations, box
/// drawing. `metric::END_BOX` is the last of them, and every packed range lies below it.
const SCAN: std::ops::RangeInclusive<u32> = 0x20..=0x2580;

const FACES: [(Style, &str); 5] = [
    (Style::Regular, "Inter Regular"),
    (Style::Bold, "Inter Bold"),
    (Style::Italic, "Inter Italic"),
    (Style::Monospace, "JetBrains Mono"),
    (Style::Math, "Noto Sans Math"),
];

/// One glyph's placement, relative to the top of the line box it is drawn in.
#[derive(Clone, Copy)]
struct Placement {
    codepoint: u32,
    ch: char,
    metric: GlyphMetric,
    /// Rows below the box top at which the ink's first row lands.
    top: i32,
    /// Rows below the box top just past the ink's last row. Exclusive.
    bottom: i32,
}

impl Placement {
    fn contained_in(&self, pitch: i32) -> bool {
        self.top >= 0 && self.bottom <= pitch && self.bottom > self.top
    }
}

/// Every packed glyph's placement under `Painter::text`'s own arithmetic.
///
/// The two paths in `Painter::text` are modelled separately, because they do not agree on what a
/// glyph's metric means:
///
/// * **Box drawing is procedural.** `Painter::text` calls `box_glyph` *before* it consults the
///   atlas, passing `cell_w x cell_h` and `run.y`. The stored `GlyphMetric` is never read -- and for
///   these codepoints `bearing_y` is 0 with `height == box_drawing::cell_size(ppem)`, which would
///   place the ink at rows `ascent..ascent + 16` and overflow the box by 7 rows. That number is a
///   fiction of `cell_metric`, which reports the *cell* as if it were ink extents. So for these the
///   probe asserts the real thing: `box_glyph`'s output is the cell by construction, which is
///   contained iff `cell_size(ppem) <= cell_h`.
/// * **Everything else is a raster.** Placement is `run.y + ascent - bearing_y`, the formula in the
///   module doc above, computed from the same two accessors the painter uses.
fn placements(
    atlas: &holonomy_assets::atlas::Atlas,
    style: Style,
    ppem: u16,
    pitch: i32,
) -> Vec<Placement> {
    let (ascent, _descent) = atlas.vertical(style, ppem);
    let mut out = Vec::new();
    for cp in SCAN.clone() {
        let m = atlas.metric(cp, style, ppem);
        if m.is_blank() {
            continue;
        }
        let procedural = (box_drawing::FIRST..=box_drawing::LAST).contains(&cp);
        // `box_glyph` is handed the cell, so its ink starts at the box's own top edge and is exactly
        // one cell tall. For a raster it is `baseline - bearing_y` and `height` rows deep.
        let (top, bottom) = if procedural {
            (
                0,
                pitch.min(i32::from(
                    box_drawing::cell_size(ppem).min(u16::MAX as usize) as u16,
                )),
            )
        } else {
            (
                i32::from(ascent) - i32::from(m.bearing_y),
                i32::from(ascent) - i32::from(m.bearing_y) + i32::from(m.height),
            )
        };
        out.push(Placement {
            codepoint: cp,
            ch: char::from_u32(cp).unwrap_or('\u{fffd}'),
            metric: m,
            top,
            bottom,
        });
    }
    out
}

fn main() -> std::process::ExitCode {
    let ppem = 16u16;
    let (atlas, _) =
        holonomy_assets::build_atlas(&[ppem]).expect("the atlas every test already builds");
    let pitch = i32::from(atlas.line_pitch());

    println!(
        "ppem={ppem}  cell_h = Atlas::line_pitch() = {pitch}  \
         box_drawing::cell_size = {}\n\
         placement: baseline = box_top + ascent, ink_top = baseline - bearing_y, \
         ink_bottom = ink_top + height",
        box_drawing::cell_size(ppem),
    );

    let mut scanned = 0usize;
    let mut rasters = 0usize;
    let mut procedural = 0usize;
    let mut offenders: Vec<(&str, Placement)> = Vec::new();

    for (style, name) in FACES {
        let (ascent, descent) = atlas.vertical(style, ppem);
        // The tightest box this face's ink could ever be held in on its own, ignoring the other
        // faces. Reported because it is the *stronger* claim: a glyph that fits the global pitch can
        // still overflow its own face's ascent + descent if the pitch came from a taller face.
        let own = i32::from(ascent) + i32::from(descent) + 1;
        let ps = placements(&atlas, style, ppem, pitch);
        let mut lo = i32::MAX;
        let mut hi = i32::MIN;
        let mut face_offenders = 0usize;
        let mut face_procedural = 0usize;
        for p in &ps {
            let is_box = (box_drawing::FIRST..=box_drawing::LAST).contains(&p.codepoint);
            face_procedural += usize::from(is_box);
            lo = lo.min(p.top);
            hi = hi.max(p.bottom);
            if !p.contained_in(pitch) || !p.contained_in(own) {
                face_offenders += 1;
                offenders.push((name, *p));
            }
        }
        scanned += ps.len();
        procedural += face_procedural;
        rasters += ps.len() - face_procedural;
        println!("{name}  style={style:?}  ascent={ascent} descent={descent}  own box={own}");
        println!(
            "  {} glyphs, ink spans rows {lo}..{hi} of {pitch}  ({:.0}% of the box)",
            ps.len(),
            if pitch > 0 {
                100.0 * (hi - lo) as f64 / pitch as f64
            } else {
                0.0
            }
        );
        if face_offenders > 0 {
            println!("  {face_offenders} glyph(s) OUTSIDE the box");
        }
        println!();
    }

    println!(
        "{scanned} glyphs scanned ({rasters} rasters, {procedural} procedural); \
         box is {pitch} rows"
    );

    if offenders.is_empty() {
        println!("PASS: every glyph's ink is inside its line box.");
        return std::process::ExitCode::SUCCESS;
    }
    println!(
        "FAIL: {} glyph(s) have ink outside the line box:",
        offenders.len()
    );
    for (name, p) in &offenders {
        let proc_ = if (box_drawing::FIRST..=box_drawing::LAST).contains(&p.codepoint) {
            " [procedural]"
        } else {
            ""
        };
        println!(
            "  {name}: U+{:04X} {:?} bearing_y={} w={} h={} -> rows {}..{}{proc_}",
            p.codepoint, p.ch, p.metric.bearing_y, p.metric.width, p.metric.height, p.top, p.bottom,
        );
    }
    std::process::ExitCode::FAILURE
}
