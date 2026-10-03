//! Criterion harness for the section-sized operations that sit on a latency
//! budget. The binary in `main.rs` is the gate; this tracks regressions over
//! time with proper statistics.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use loro::{ExportMode, LoroDoc, LoroText};
use std::time::Instant;

const WORDS_PER_SECTION: usize = 1500;

const WORDS: &[&str] = &[
    "the", "manifold", "representation", "of", "quaternion", "algebra", "yields",
    "canonical", "form", "induces", "natural", "transformation", "cohomology",
    "vanishing", "theorem", "obstruction", "compact", "Riemannian", "curvature",
    "fundamental", "group", "construction", "desingularization", "parametrized",
];

struct Rng(u64);
impl Rng {
    fn below(&mut self, n: u32) -> u32 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        if n == 0 { 0 } else { (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32 % n }
    }
}

fn build(words: usize) -> (LoroDoc, LoroText) {
    let doc = LoroDoc::new();
    let text = doc.get_text("body");
    let mut rng = Rng(0x1234_5678_9abc_def0);
    let mut written = 0;
    while written < words {
        let para_words = 40 + rng.below(120) as usize;
        let start = text.len_unicode();
        let mut in_para = 0;
        while in_para < para_words {
            let run = 3 + rng.below(12) as usize;
            let mut s = String::new();
            for _ in 0..run {
                s.push_str(WORDS[rng.below(WORDS.len() as u32) as usize]);
                s.push(' ');
            }
            let end = start + s.chars().count();
            let _ = text.mark(start..end, "bold", true);
            text.insert(end, &s).expect("insert");
            in_para += run;
            written += run;
        }
        let pos = text.len_unicode();
        text.insert(pos, "\n").expect("insert");
    }
    doc.commit();
    (doc, text)
}

fn snapshot_import(c: &mut Criterion) {
    let (doc, _) = build(WORDS_PER_SECTION);
    let bytes = doc.export(ExportMode::Snapshot).expect("export");
    c.bench_function("snapshot_import_1500w", |b| {
        b.iter(|| {
            let fresh = LoroDoc::new();
            fresh.import(black_box(&bytes)).expect("import");
        })
    });
}

fn snapshot_export(c: &mut Criterion) {
    let (doc, _) = build(WORDS_PER_SECTION);
    c.bench_function("snapshot_export_1500w", |b| {
        b.iter(|| doc.export(ExportMode::Snapshot).expect("export"))
    });
}

fn styled_read(c: &mut Criterion) {
    let (_, text) = build(WORDS_PER_SECTION);
    c.bench_function("styled_read_1500w", |b| {
        b.iter(|| text.to_delta())
    });
}

fn single_insert(c: &mut Criterion) {
    let (_, text) = build(WORDS_PER_SECTION);
    c.bench_function("single_insert_1500w", |b| {
        b.iter(|| {
            let pos = (black_box(text.len_unicode()) / 2).max(1);
            let _ = text.insert(pos, "x");
        })
    });
}

criterion_group!(
    benches,
    snapshot_import,
    snapshot_export,
    styled_read,
    single_insert
);
criterion_main!(benches);

// Keep Instant imported for future ratio benchmarks without warnings.
#[allow(dead_code)]
fn _unused(t: Instant) -> Instant {
    t
}
