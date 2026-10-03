//! Is the mark-accumulation growth in M1 a real trend or measurement noise?
//!
//! M1 measured a single run and found 2.87x growth in styled-read cost over 1500
//! accumulated marks. That number is under budget, but it is the only measurement
//! in M1 that trends toward a limit rather than staying flat, and it is what
//! justifies treating compaction-on-write as a requirement in M2.
//!
//! A single sample cannot distinguish a real trend from allocator noise. This runs
//! the same accumulation across several seeds and reports the spread, then fits a
//! slope so we can say how the cost grows with mark count rather than just
//! "it got worse".
//!
//! Run: cargo run --release --bin loro-markcurve

use loro::{LoroDoc, LoroText, TextDelta};
use std::time::Instant;

const WORDS_PER_SECTION: usize = 1500;
const ROUNDS: usize = 30;
const MARKS_PER_ROUND: usize = 50;
const SEEDS: &[u64] = &[0x5EED_1234_ABCD_0001, 0x1111_2222_3333_4444, 0xDEAD_BEEF_CAFE_0007];

const WORDS: &[&str] = &[
    "the", "manifold", "representation", "of", "quaternion", "algebra", "yields",
    "canonical", "form", "induces", "natural", "transformation", "cohomology",
    "vanishing", "theorem", "obstruction", "compact", "Riemannian", "curvature",
    "fundamental", "group", "construction", "parametrized", "coincident",
    "proximate", "adjacent", "preceding", "subsequent", "convergence", "adaptively",
];
const MARK_CHOICES: &[&str] = &["bold", "italic", "underline", "code", "link"];

struct Rng(u64);
impl Rng {
    fn next_u32(&mut self) -> u32 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        if n == 0 { 0 } else { self.next_u32() % n }
    }
    fn chance(&mut self, p: u32) -> bool {
        self.below(100) < p
    }
}

fn count_marks(text: &LoroText) -> usize {
    text.to_delta()
        .iter()
        .map(|d| match d {
            TextDelta::Retain { attributes, .. } | TextDelta::Insert { attributes, .. } => {
                attributes.as_ref().map_or(0, |a| a.len())
            }
            TextDelta::Delete { .. } => 0,
        })
        .sum()
}

fn build(doc: &LoroDoc, words: usize, rng: &mut Rng) -> LoroText {
    let text = doc.get_text("body");
    let mut written = 0;
    while written < words {
        let para_words = 40 + rng.below(120) as usize;
        let mut in_para = 0;
        while in_para < para_words {
            let run = 3 + rng.below(12) as usize;
            let mut s = String::new();
            for _ in 0..run {
                s.push_str(WORDS[rng.below(WORDS.len() as u32) as usize]);
                s.push(' ');
            }
            let begin = text.len_unicode();
            text.insert(begin, &s).expect("insert");
            let end = begin + s.chars().count();
            if rng.chance(55) {
                let m = MARK_CHOICES[rng.below(MARK_CHOICES.len() as u32) as usize];
                text.mark(begin..end, m, true).expect("mark");
            }
            in_para += run;
            written += run;
        }
        let p = text.len_unicode();
        text.insert(p, "\n").expect("newline");
    }
    doc.commit();
    text
}

fn median(xs: &mut [f64]) -> f64 {
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    xs[xs.len() / 2]
}

/// Ordinary least squares slope of y on x, and R^2 so we can tell a real
/// trend from noise.
fn linfit(xs: &[f64], ys: &[f64]) -> (f64, f64) {
    let n = xs.len() as f64;
    let mx = xs.iter().sum::<f64>() / n;
    let my = ys.iter().sum::<f64>() / n;
    let mut sxy = 0.0;
    let mut sxx = 0.0;
    for (x, y) in xs.iter().zip(ys) {
        sxy += (x - mx) * (y - my);
        sxx += (x - mx) * (x - mx);
    }
    let slope = if sxx > 0.0 { sxy / sxx } else { 0.0 };
    let intercept = my - slope * mx;
    let ss_tot: f64 = ys.iter().map(|y| (y - my).powi(2)).sum();
    let ss_res: f64 = xs
        .iter()
        .zip(ys)
        .map(|(x, y)| (y - (slope * x + intercept)).powi(2))
        .sum();
    let r2 = if ss_tot > 0.0 { 1.0 - ss_res / ss_tot } else { 0.0 };
    (slope, r2)
}

/// Log-log fit, to distinguish linear-in-marks from Loro's documented
/// quadratic behaviour. A slope near 1.0 means linear; near 2.0 means the
/// O(n^2) `StyleRangeMap` path dominates.
fn logfit(xs: &[f64], ys: &[f64]) -> (f64, f64) {
    let lx: Vec<f64> = xs.iter().map(|v| v.max(1.0).ln()).collect();
    let ly: Vec<f64> = ys.iter().map(|v| v.max(1e-9).ln()).collect();
    linfit(&lx, &ly)
}

fn main() {
    println!("M1b — is mark accumulation a real trend?");
    println!("{ROUNDS} rounds x {MARKS_PER_ROUND} marks, {} seeds\n", SEEDS.len());

    // Per-seed curves, indexed [seed][round].
    let mut curves: Vec<Vec<(usize, f64)>> = Vec::new();

    for (si, &seed) in SEEDS.iter().enumerate() {
        let mut rng = Rng(seed);
        let doc = LoroDoc::new();
        let text = build(&doc, WORDS_PER_SECTION, &mut rng);
        let base_marks = count_marks(&text);
        let mut curve = Vec::with_capacity(ROUNDS);

        for _ in 1..=ROUNDS {
            let len = text.len_unicode();
            for _ in 0..MARKS_PER_ROUND {
                if len < 2 {
                    break;
                }
                let a = (rng.below(len.saturating_sub(1) as u32) as usize).min(len - 2);
                let b = (a + 1 + rng.below(20) as usize).min(len - 1);
                let m = MARK_CHOICES[rng.below(MARK_CHOICES.len() as u32) as usize];
                text.mark(a..b, m, true).expect("mark");
            }
            doc.commit();

            // Median of repeated reads: one sample is far too noisy at the
            // sub-millisecond scale these land at.
            let mut t = Vec::with_capacity(15);
            for _ in 0..15 {
                let t0 = Instant::now();
                let _ = text.to_delta();
                t.push(t0.elapsed().as_secs_f64() * 1000.0);
            }
            curve.push((count_marks(&text), median(&mut t)));
        }
        println!(
            "  seed {si}: base {base_marks} marks -> {} marks, read {:.3} -> {:.3} ms",
            curve.last().unwrap().0,
            curve.first().map(|(_, ms)| *ms).unwrap_or(0.0),
            curve.last().map(|(_, ms)| *ms).unwrap_or(0.0)
        );
        curves.push(curve);
    }

    // Aggregate across seeds: median read cost per round.
    println!("\n  round   median marks   median read (ms)   per-1k-marks");
    let mut xs = Vec::new();
    let mut ys = Vec::new();
    for r in 0..ROUNDS {
        let mut ms: Vec<f64> = curves.iter().map(|c| c[r].1).collect();
        let mut mk: Vec<f64> = curves.iter().map(|c| c[r].0 as f64).collect();
        let read = median(&mut ms);
        let marks = median(&mut mk);
        xs.push(marks);
        ys.push(read);
        if r % 5 == 0 || r == ROUNDS - 1 {
            println!(
                "  {r:>5}   {marks:>12.0}   {read:>15.4}   {:>12.4}",
                read * 1000.0 / marks
            );
        }
    }

    let (slope, r2) = linfit(&xs, &ys);
    println!("\n  linear fit: read_ms = {:.6} * marks {:+.4}   (R^2 = {:.3})", slope, ys[0] - slope * xs[0], r2);

    // The distinction that decides the remedy. Loro documents an O(n^2) mark
    // pathology; if the observed exponent is ~1.0 then what we are seeing is
    // linear cost per mark and a very different mitigation applies.
    let (log_slope, log_r2) = logfit(&xs, &ys);
    println!(
        "  log-log fit: exponent = {log_slope:.2} (R^2 = {log_r2:.3})  \
         -> {}",
        if log_slope < 1.3 {
            "LINEAR in marks"
        } else if log_slope < 1.7 {
            "MIXED, mildly superlinear"
        } else {
            "SUPERLINEAR, approaching the documented O(n^2)"
        }
    );

    let growth = ys[ROUNDS - 1] / ys[0];
    println!("  growth first->last round: {growth:.2}x");

    // What matters for M2: project the cost at mark counts a long session reaches.
    println!("\n  projection (linear fit, section stays {} words):", WORDS_PER_SECTION);
    for target_marks in [5_000usize, 10_000, 25_000, 50_000] {
        let ms = slope * target_marks as f64 + (ys[0] - slope * xs[0]);
        println!("    {target_marks:>6} marks -> {ms:>8.3} ms per styled read");
    }

    // How long is a mark budget of, say, 5ms? A styled read happens on every
    // render of the focused section, so this is a per-frame cost.
    let budget_ms = 5.0f64;
    let marks_at_budget = ((budget_ms - (ys[0] - slope * xs[0])) / slope).max(0.0);
    println!("\n  marks before a styled read exceeds {budget_ms:.0}ms: {marks_at_budget:.0}");

    // Verdict. Growth alone is not the question: a linear cost with a large
    // constant is a budgeting problem, whereas a superlinear one is a
    // structural one.
    let verdict = if log_slope >= 1.7 {
        "STRUCTURAL — mark count must be bounded, not just budgeted"
    } else if growth < 4.0 && log_r2 < 0.5 {
        "FLAT — safe, no mitigation needed"
    } else {
        "BOUNDED BUT TRENDING — compaction-on-write is required, and the \
         section write cadence sets the mark ceiling"
    };
    println!(
        "\n  RESULT: growth {growth:.2}x, linear R^2 {r2:.3}, exponent {log_slope:.2}\n  {verdict}"
    );
}
