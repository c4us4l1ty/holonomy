//! Block 5's second gate (`OQ-12`): *"Layout cost on a 30 M-word doc is invariant in document
//! length (viewport-shape cost only)."*
//!
//! `tests/viewport_cost.rs` proved the counted half (O(log n) Fenwick reads and bounded window
//! bytes). This harness measures the **wall-clock** half end to end through the real OpenType
//! metrics reader ([`holonomy_eye::Face`] over `Inter-Regular.ttf`):
//!
//! ```text
//! byte_window (Fenwick + Spine) -> window_text (ResidentSet) -> layout_styled (Face) -> Picture
//! ```
//!
//! Three viewports are timed **interleaved** in one process so shared-runner drift moves every
//! curve together:
//! - `180KB@0`: top neighborhood of a 44-leaf (180 KB) document;
//! - `180MB@0`: top neighborhood of a 43,945-leaf (30 M-word / 180 MB) document;
//! - `180MB@end`: tail neighborhood (> 99.99% through the 30 M-word document).
//!
//! Honesty scope: `Face` parses real `head`/`hhea`/`hmtx`/`maxp`/`cmap` metrics plus OpenType
//! `GDEF`/`GSUB`/`GPOS` shaping tables from `Inter-Regular.ttf`, and shared CI runners enforce
//! relative invariance (`p50` within 2x, `p99` within 4x) plus a frame ceiling rather than
//! certifying reference-hardware numbers.

use std::hint::black_box;
use std::sync::Arc;
use std::time::{Duration, Instant};

use holonomy_eye::{
    byte_window, layout_styled, window_text, Face, Fixed, HeightModel, Style, StyledRun, StyledText,
};
use holonomy_ledger::timing::{measure, summarise, Summary};
use holonomy_text::{Draft, OpenSources, ResidentSet, SegmentId, SegmentSource, Slab, Spine};

const FONT: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../holonomy-old/assets/fonts/Inter-Regular.ttf"
));

const WORDS: u64 = 30_000_000;
const BYTES_PER_WORD: u64 = 6;
const LEAF_BYTES: u32 = 4_096;
const BUDGET: usize = 4 * 1024 * 1024;
const PAGE_WIDTH: Fixed = Fixed::units(680);
const VIEWPORT_HEIGHT: Fixed = Fixed::units(800);
const BODY_SIZE_MILLI: i64 = 16_000;

const SAMPLES: usize = if cfg!(debug_assertions) { 64 } else { 256 };
const BLOCK: usize = if cfg!(debug_assertions) { 8 } else { 16 };
const WARMUP: usize = 8;

const P50_FACTOR: u32 = 2;
const P99_FACTOR: u32 = 4;
const FRAME_CEILING: Duration = Duration::from_millis(50);
const CONTROL_LEAVES: usize = 128;
const CONTROL_FACTOR: u32 = 25;

#[derive(Clone)]
struct ProsePages {
    page: Arc<[u8]>,
}

impl ProsePages {
    fn new(len: usize) -> Self {
        let sentence = b"The quiet page shapes only what sits in view; thirty million words wait \
                         sealed in the world-line with proportional Inter advances (i, m, W).\n";
        let mut buf = Vec::with_capacity(len);
        while buf.len() + sentence.len() < len {
            buf.extend_from_slice(sentence);
        }
        if buf.len() + 1 < len {
            buf.resize(len - 1, b'a');
        }
        if buf.len() < len {
            buf.push(b'\n');
        }
        assert_eq!(buf.len(), len);
        Self { page: buf.into() }
    }

    fn as_str(&self) -> &str {
        std::str::from_utf8(&self.page).expect("ASCII prose page")
    }
}

impl SegmentSource for ProsePages {
    fn load(&mut self, _segment: SegmentId) -> Result<Vec<u8>, String> {
        Ok(self.page.to_vec())
    }
}

struct Doc {
    spine: Spine,
    heights: HeightModel,
    residency: ResidentSet<ProsePages>,
}

impl Doc {
    fn new(leaves: u64, leaf_height: Fixed, pages: ProsePages) -> Self {
        let spine =
            Spine::from_slabs((0..leaves).map(|id| Slab::new(SegmentId::new(id), 0, LEAF_BYTES)));
        assert_eq!(spine.slab_count() as u64, leaves);
        let mut heights = HeightModel::new(leaves as usize);
        for idx in 0..leaves as usize {
            heights.set(idx, leaf_height);
        }
        let residency = ResidentSet::new(pages, BUDGET, 0).expect("budget");
        Self {
            spine,
            heights,
            residency,
        }
    }
}

struct Rig<'a> {
    face: &'a Face,
    draft: Draft,
    sources: OpenSources,
    runs: &'a [StyledRun],
}

impl Rig<'_> {
    fn shape(&self, doc: &mut Doc, top: Fixed) -> (usize, u64) {
        let win = byte_window(&doc.spine, &doc.heights, top, VIEWPORT_HEIGHT);
        let read = window_text(
            &mut doc.residency,
            &doc.spine,
            &self.draft,
            &self.sources,
            win,
            b"",
        )
        .expect("window readable");
        let styled = StyledText::new(read.text(), self.runs);
        let laid = layout_styled(styled, read.origin(), PAGE_WIDTH, self.face);
        let picture = laid.picture(Some(read.origin()), None);
        (picture.clusters.len(), win.len())
    }
}

fn within_factor(one: Duration, other: Duration, factor: u32) -> bool {
    one <= other * factor && other <= one * factor
}

fn report(tag: &str, summary: &Summary, bytes: u64, clusters: usize) {
    println!(
        "HOLONOMY_VIEWPORT_LATENCY at={tag} samples={} bytes={bytes} clusters={clusters} \
         mean_ns={} p50_ns={} p99_ns={} max_ns={}",
        summary.samples,
        summary.mean.as_nanos(),
        summary.p50.as_nanos(),
        summary.p99.as_nanos(),
        summary.max.as_nanos()
    );
}

#[test]
fn viewport_layout_with_real_face_is_invariant_at_thirty_million_words() {
    let face = Face::parse(FONT).expect("Inter-Regular.ttf parses");
    let body = Style::at(BODY_SIZE_MILLI);
    let runs = [StyledRun {
        start: 0,
        style: body,
    }];

    let pages = ProsePages::new(LEAF_BYTES as usize);
    let one_leaf = layout_styled(StyledText::new(pages.as_str(), &runs), 0, PAGE_WIDTH, &face);
    let leaf_height = one_leaf.height();
    assert!(leaf_height > Fixed::ZERO);

    let big_leaves = WORDS * BYTES_PER_WORD / u64::from(LEAF_BYTES);
    let small_leaves = (big_leaves / 1_000).max(8);
    let mut small_doc = Doc::new(small_leaves, leaf_height, pages.clone());
    let mut big_doc = Doc::new(big_leaves, leaf_height, pages.clone());

    let half = Fixed::from_64ths(leaf_height.sixtyfourths() / 2);
    let top_small = half;
    let top_start = half;
    let top_end = big_doc.heights.prefix((big_leaves - 4) as usize) + half;

    let rig = Rig {
        face: &face,
        draft: Draft::new(),
        sources: OpenSources::new(),
        runs: &runs,
    };

    for _ in 0..WARMUP {
        let _ = rig.shape(&mut small_doc, top_small);
        let _ = rig.shape(&mut big_doc, top_start);
        let _ = rig.shape(&mut big_doc, top_end);
    }

    let mut t_small = Vec::with_capacity(SAMPLES);
    let mut t_start = Vec::with_capacity(SAMPLES);
    let mut t_end = Vec::with_capacity(SAMPLES);
    let mut meta_small = (0usize, 0u64);
    let mut meta_start = (0usize, 0u64);
    let mut meta_end = (0usize, 0u64);

    for _ in 0..SAMPLES / BLOCK {
        for _ in 0..BLOCK {
            let t0 = Instant::now();
            meta_small = black_box(rig.shape(&mut small_doc, top_small));
            t_small.push(t0.elapsed());
        }
        for _ in 0..BLOCK {
            let t0 = Instant::now();
            meta_start = black_box(rig.shape(&mut big_doc, top_start));
            t_start.push(t0.elapsed());
        }
        for _ in 0..BLOCK {
            let t0 = Instant::now();
            meta_end = black_box(rig.shape(&mut big_doc, top_end));
            t_end.push(t0.elapsed());
        }
    }

    assert_eq!(
        meta_small, meta_start,
        "180KB and 180MB@0 shaped different windows"
    );
    assert_eq!(
        meta_start, meta_end,
        "180MB@0 and 180MB@end shaped different windows"
    );
    assert!(
        meta_start.0 > 1_000,
        "shaped too few clusters: {}",
        meta_start.0
    );

    let s_small = summarise(t_small);
    let s_start = summarise(t_start);
    let s_end = summarise(t_end);
    report("180KB@0", &s_small, meta_small.1, meta_small.0);
    report("180MB@0", &s_start, meta_start.1, meta_start.0);
    report("180MB@end", &s_end, meta_end.1, meta_end.0);

    for (label_a, a, label_b, b) in [
        ("180KB@0", &s_small, "180MB@0", &s_start),
        ("180MB@0", &s_start, "180MB@end", &s_end),
        ("180KB@0", &s_small, "180MB@end", &s_end),
    ] {
        assert!(
            within_factor(a.p50, b.p50, P50_FACTOR),
            "p50 {label_a}={} ns vs {label_b}={} ns exceeds {P50_FACTOR}x",
            a.p50.as_nanos(),
            b.p50.as_nanos()
        );
        assert!(
            within_factor(a.p99, b.p99, P99_FACTOR),
            "p99 {label_a}={} ns vs {label_b}={} ns exceeds {P99_FACTOR}x",
            a.p99.as_nanos(),
            b.p99.as_nanos()
        );
    }

    for (tag, summary) in [
        ("180KB@0", &s_small),
        ("180MB@0", &s_start),
        ("180MB@end", &s_end),
    ] {
        assert!(
            summary.p99 < FRAME_CEILING,
            "{tag}: p99 {:?} exceeded ceiling {FRAME_CEILING:?}",
            summary.p99
        );
    }

    // Control: shaping 128 leaves (< 0.3% of the 30 M-word document) must cost tens of times a
    // single viewport, proving document-proportional layout would fail by orders of magnitude.
    let bulk = pages.as_str().repeat(CONTROL_LEAVES);
    let control = measure(4, || {
        let laid = layout_styled(StyledText::new(&bulk, &runs), 0, PAGE_WIDTH, &face);
        black_box(laid.lines().len())
    });
    println!(
        "HOLONOMY_VIEWPORT_CONTROL control_leaves={CONTROL_LEAVES} control_p50_ns={} \
         viewport_p50_ns={}",
        control.p50.as_nanos(),
        s_start.p50.as_nanos()
    );
    assert!(
        control.p50 > s_start.p50 * CONTROL_FACTOR && control.p50 > s_end.p50 * CONTROL_FACTOR,
        "128-leaf control ({:?}) did not exceed viewport p50 ({:?}, {:?}) by {CONTROL_FACTOR}x",
        control.p50,
        s_start.p50,
        s_end.p50
    );
}
