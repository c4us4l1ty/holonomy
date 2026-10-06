//! NIST SP 800-22 Rev 1a statistical tests, the subset Phase 3's gate calls for:
//! frequency, block frequency, runs, cumulative sums (forward and reverse), Monte Carlo, and
//! Shannon entropy.
//!
//! # Why this file exists at all
//!
//! FR-2.1.3 and the gate both demand that the produced container be *demonstrably*
//! indistinguishable from random rather than merely asserted to be. A test that says "we
//! wrote 128 MiB of ChaCha20 keystream, so it is random" is a claim, not evidence. These are
//! the evidence.
//!
//! # Validation, because a statistical test that is itself wrong proves nothing
//!
//! No SciPy, no NumPy, and no reference STS implementation on this host, so the special
//! functions and the p-value chain are validated three ways:
//!
//! 1. `erfc` against published values, and the incomplete gamma against itself: the series
//!    (`gammp`) and continued fraction (`gammq`) are independent algorithms, and their sum
//!    must be 1 even at the argument sizes this gate uses — `a ≈ 4.2e6`, `x ≈ 8.4e6`, where a
//!    naive implementation cancels away most of its precision.
//! 2. **Discrimination**: every test must pass on CSPRNG output and fail on structured
//!    input. A test that cannot tell ChaCha20 from all-zeros is not a test.
//! 3. **Calibration**: over many independent CSPRNG draws, p-values must be roughly
//!    uniform, and the fraction below the threshold must match the threshold.
//!
//! # The threshold, and why it is 0.001 and not 0.01
//!
//! SP 800-22 recommends α = 0.01. Applying that to six tests makes the *gate* fail by
//! chance about 1 − 0.99⁶ = **5.9%** of the time, and the container is created with a fresh
//! random salt on every run, so those are genuinely independent draws. A gate that fails
//! roughly one run in seventeen is not a gate.
//!
//! α = 0.001 per test bounds the family-wise false-positive rate at 6 × 0.001 = 0.6%, while
//! still rejecting: the structured inputs used in validation produce p-values below 1e-300,
//! so nothing real is lost by tightening. See [`ALPHA`].

use std::fmt::Write as _;

/// Per-test significance level.
///
/// 0.001 rather than SP 800-22's 0.01, for the family-wise reason in the module docs. With
/// six tests the gate's spurious failure rate is 0.6%, not 5.9%.
pub const ALPHA: f64 = 0.001;

/// Bits per block for the block-frequency test. SP 800-22 suggests M = 128.
pub const BLOCK_M: usize = 128;

/// Bytes per block-frequency block, `BLOCK_M / 8`. A block is never materialised as a 128-bit
/// value; see the `chunks_exact` note in [`block_frequency`].
const BLOCK_SLOT_BYTES: usize = BLOCK_M / 8;

/// How many tests [`run`] produces. The [`ALPHA`] budget is quoted against this number, so it
/// is a constant rather than a `results.len()` nobody can check.
pub const TEST_COUNT: usize = 6;

/// Block size in bits for the Monte Carlo test, SP 800-22 §2.3.4. 15 bits gives
/// `M ≈ 71.6 M` blocks over a 128 MiB container, which is more than enough for the
/// normal approximation to the binomial to hold to well under a percent.
pub const MC_BLOCK_BITS: usize = 15;

/// π used by the Monte Carlo test. A block passes when its value falls in
/// `[0, π·k) ∪ [π·k + (1−π)·k, k)`.
pub const MC_PI: f64 = 0.4;

/// Shannon entropy the gate requires, in bits per byte. `entropy_gate_requirements` pins
/// this against the measured spread of real CSPRNG output.
pub const ENTROPY_FLOOR: f64 = 7.999_99;

// ---------------------------------------------------------------------------
// Special functions
// ---------------------------------------------------------------------------

/// Upper bound on the relative error of [`erfc`], which the reference-table test demands of
/// it.
///
/// The implementation behind [`erfc`] is the incomplete gamma, whose accuracy is asserted
/// separately and far more tightly; this constant is the bar the *table* comparison sets, so
/// it is a statement about the test rather than about the code.
pub const ERFC_ACCURACY: f64 = 1.2e-7;

/// `ln Γ(x)` by the Lanczos approximation, `g = 7`, 9 coefficients.
///
/// Reference values come from CPython's `math.lgamma`, which is an independent implementation.
pub fn log_gamma(x: f64) -> f64 {
    const C: [f64; 9] = [
        0.9999999999998099,
        676.5203681218851,
        -1_259.1392167224028,
        771.3234287776531,
        -176.6150291621406,
        12.507343278686905,
        -0.13857109526572012,
        9.984369578019572e-6,
        1.5056327351493116e-7,
    ];
    debug_assert!(x > 0.0, "log_gamma is only defined for positive arguments");
    if x < 0.5 {
        // Reflection: Γ(x)Γ(1-x) = π / sin(πx)
        return (std::f64::consts::PI / (std::f64::consts::PI * x).sin()).ln() - log_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = C[0];
    for (i, &c) in C.iter().enumerate().skip(1) {
        a += c / (x + i as f64);
    }
    let t = x + 7.5;
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

/// Regularised lower incomplete gamma, `P(a, x) = γ(a,x)/Γ(a)`, by series expansion.
///
/// Accurate for `x < a + 1`. Terms are accumulated as a running sum of
/// `Σ_{n≥1} x^n / (n! (a+n))` scaled by `exp(-x + a ln x - lnΓ(a))`, which stays in range
/// even when the prefactor and the sum are each enormous on their own — the cancellation is
/// between terms of the *same* series, not between two large quantities.
pub fn gammp(a: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= a + 1.0 {
        return 1.0 - gammq(a, x);
    }
    // The series is P = exp(-x + a·ln x − lnΓ(a)) · Σ_{n≥0} xⁿ / (a(a+1)…(a+n)).
    //
    // Written as an exponential prefactor times a sum rather than as a recurrence on the
    // answer, because for the block-frequency test's arguments (a ≈ 4.2e6, x ≈ 4.2e6) the
    // prefactor is around e^-4.2e6 — underflows to zero on its own. The sum carries the
    // magnitude instead, and the two are combined in the log domain at the end. That is the
    // whole reason this is not the textbook `gser`: the textbook form multiplies the
    // prefactor into the running total and returns 0 for every input this gate produces.
    let ln_pre = -x + a * x.ln() - log_gamma(a);
    let mut term = 1.0 / a;
    let mut total = term;
    for n in 1..100_000 {
        let ap = a + n as f64;
        term *= x / ap;
        total += term;
        if term < total * 1e-17 {
            break;
        }
    }
    ln_pre.exp() * total
}

/// Regularised upper incomplete gamma, `Q(a, x) = Γ(a,x)/Γ(a)`, by continued fraction
/// (modified Lentz).
///
/// This is the branch that must stay accurate for *small* p-values, since a structured input
/// is exactly the case where the gate has to return "this is not random". The CF computes
/// `Q` directly, so there is no `1 - x` subtraction anywhere and no cancellation.
pub fn gammq(a: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 1.0;
    }
    if x < a + 1.0 {
        return 1.0 - gammp(a, x);
    }
    // Modified Lentz. `h` accumulates the continued fraction
    //
    //   1/(b0 + a1/(b1 + a2/(b2 + …))),   b0 = x+1−a,  ai = −i(i−a),  bi = b0 + 2i
    //
    // and the result is `exp(−x + a·ln x − lnΓ(a)) · h`.
    //
    // The prefactor underflows for the block-frequency test's arguments (a ≈ 4.2e6), and
    // `h` is correspondingly enormous. Their product is O(1), so the two must never be
    // multiplied separately: they are combined as `exp(ln_pre + ln(h))`, keeping the
    // exponent in range throughout. Computing `h` in f64 and then exponentiating is fine
    // because `h` itself is only O(1e6) here, not O(1e1800000).
    const FPMIN: f64 = 1e-300;
    const EPS: f64 = 1e-16;
    let mut b = x + 1.0 - a;
    let mut c = 1.0 / FPMIN;
    let mut d = 1.0 / b;
    let mut h = d;
    for i in 1..200_000 {
        let ii = i as f64;
        let an = -ii * (ii - a);
        b += 2.0;
        d = an * d + b;
        if d.abs() < FPMIN {
            d = FPMIN;
        }
        c = b + an / c;
        if c.abs() < FPMIN {
            c = FPMIN;
        }
        d = 1.0 / d;
        let del = d * c;
        h *= del;
        if (del - 1.0).abs() < EPS {
            break;
        }
    }
    let ln_pre = -x + a * x.ln() - log_gamma(a);
    // `h` is positive for this CF, so the sum of logs is the log of the product.
    (ln_pre + h.ln()).exp()
}

/// Complementary error function, `1 - erf(x)`.
///
/// # Implemented as a special case of the incomplete gamma, deliberately
///
/// `erfc(x) = Q(1/2, x²)`. That looks roundabout and is the reason this function is
/// trustworthy: [`gammq`] is validated against closed forms, against the `a+1` recurrence, and
/// at the argument sizes the block-frequency test uses, whereas a hand-rolled `erfc` is a
/// second special function with none of that.
///
/// Two hand-rolled versions came first and both were wrong in ways that did not look wrong.
/// One summed the continued fraction instead of multiplying the Lentz deltas and returned
/// *negative* values past x = 1. The other used the right structure but got the CF's
/// coefficients wrong and still returned −0.109 for `erfc(1)`. In both cases the only
/// symptom was a p-value outside [0, 1] on some inputs, which is why
/// `p_values_are_always_in_range` exists as a test.
///
/// Conditioning: for `x < √1.5` this computes `1 − P(1/2, x²)`, which cancels, but the
/// answer is at least 0.09 there so fewer than one digit is lost. Above that the continued
/// fraction branch computes the small quantity directly and no cancellation occurs at all,
/// which is the regime where p-values need to be tiny and correct.
pub fn erfc(x: f64) -> f64 {
    if x < 0.0 {
        return 2.0 - erfc(-x);
    }
    if !x.is_finite() {
        return if x.is_sign_positive() { 0.0 } else { 2.0 };
    }
    gammq(0.5, x * x)
}

/// Shannon entropy of a byte slice, in bits per byte. Maximum 8.0.
pub fn shannon_entropy(data: &[u8]) -> f64 {
    let mut counts = [0u64; 256];
    for &b in data {
        counts[b as usize] += 1;
    }
    let n = data.len() as f64;
    if n == 0.0 {
        return 0.0;
    }
    let mut h = 0.0;
    for &c in counts.iter() {
        if c > 0 {
            let p = c as f64 / n;
            h -= p * p.log2();
        }
    }
    h
}

// ---------------------------------------------------------------------------
// Bit access
// ---------------------------------------------------------------------------

/// A bit reader that hands out fixed-width blocks from a byte slice.
///
/// LSB-first within each byte, which is an arbitrary but *consistent* convention; the
/// frequency-style tests are invariant to bit order and the block tests only require that
/// the same rule is used everywhere.
///
/// # Width limit, which is a real limit
///
/// `acc` is a `u64` and bytes are folded in eight at a time, so the accumulator must never
/// hold more than 64 bits. That caps `width` at 56: the loop tops up until `in_acc >= width`,
/// so the peak is `width + 7`, and `width = 57` would shift a byte by 64 and panic.
///
/// This is not hypothetical. `block_frequency` wants 128-bit blocks -- SP 800-22's suggested
/// `M` -- and an earlier version of this reader happily took them, shifted bytes out by up to
/// 128 positions, and in a debug build panicked with an arithmetic overflow while a release
/// build produced silently wrong blocks. Hence the assertion here rather than a doc comment,
/// and hence `block_frequency` counts ones directly instead of materialising 128-bit values.
struct BitBlocks<'a> {
    data: &'a [u8],
    width: usize,
    /// Bits held in `acc` and not yet emitted.
    in_acc: u32,
    /// Partial block under construction, lowest bits first.
    acc: u64,
    /// Next byte to fold into `acc`.
    byte: usize,
}

impl<'a> BitBlocks<'a> {
    /// # Panics
    ///
    /// If `width` is 0 or greater than 56. See the type docs for why the upper bound exists;
    /// exceeding it is a compile-time-looking mistake that a release build would hide.
    fn new(data: &'a [u8], width: usize) -> Self {
        assert!(
            (1..=56).contains(&width),
            "BitBlocks supports widths 1..=56; {width} would overflow the u64 accumulator"
        );
        Self {
            data,
            width,
            in_acc: 0,
            acc: 0,
            byte: 0,
        }
    }

    /// Next block value, or `None` at the end of the input.
    ///
    /// Bytes are folded into the accumulator a whole byte at a time rather than a bit at a
    /// time, which is worth ~30× on the Monte Carlo test: over a 128 MiB container that is
    /// 134 M byte folds instead of 1.07 G bit shifts, and it is the difference between the
    /// entropy gate being part of the gate and being skipped.
    ///
    /// Any leftover bits in the final partial block are never emitted, so the block count is
    /// `floor(n / width)`, which is what SP 800-22 prescribes for the block-based tests.
    fn next(&mut self) -> Option<u64> {
        while self.in_acc < self.width as u32 {
            let &b = self.data.get(self.byte)?;
            self.acc |= (b as u64) << self.in_acc;
            self.byte += 1;
            self.in_acc += 8;
        }
        self.in_acc -= self.width as u32;
        let block = self.acc & ((1u64 << self.width) - 1);
        self.acc >>= self.width;
        Some(block)
    }
}

/// Maximum absolute cumulative sum of the ±1 walk, plus the net walk value.
///
/// Uses a 256-entry table of per-byte excursions so the walk costs one lookup per byte
/// rather than one iteration per bit. Over 2^30 bits that is 134 M fewer iterations, which
/// is the difference between this test running in the gate and not running in it.
fn cusum_max_abs(data: &[u8]) -> (f64, i64) {
    // For each byte value: (upward excursion, downward excursion) of the running sum within
    // that byte, starting from 0, and the net change.
    let mut up = [0i32; 256];
    let mut down = [0i32; 256];
    let mut net = [0i32; 256];
    for b in 0..256usize {
        let (mut s, mut u, mut d) = (0i32, 0i32, 0i32);
        for k in 0..8 {
            let bit = if (b >> k) & 1 == 1 { 1 } else { -1 };
            s += bit;
            u = u.max(s);
            d = d.max(-s);
        }
        up[b] = u;
        down[b] = d;
        net[b] = s;
    }

    let mut s: i64 = 0;
    let mut best: i64 = 0;
    for &b in data {
        s += net[b as usize] as i64;
        // The excursion within this byte is measured from the value at its start.
        best = best
            .max(s)
            .max(s - net[b as usize] as i64 + up[b as usize] as i64);
        best = best
            .max(-s)
            .max(net[b as usize] as i64 - s + down[b as usize] as i64);
    }
    (best as f64, s)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// One test's outcome.
#[derive(Debug, Clone)]
pub struct TestResult {
    /// SP 800-22 section name.
    pub name: &'static str,
    /// The test statistic, whose meaning differs per test.
    pub statistic: f64,
    /// p-value in `[0, 1]`.
    pub p_value: f64,
    /// Passed against [`ALPHA`].
    pub passed: bool,
}

/// Every test run over one input.
#[derive(Debug, Clone)]
pub struct Report {
    /// Total bits examined, `data.len() * 8`.
    pub bits: u64,
    /// Shannon entropy in bits per byte.
    pub entropy: f64,
    /// One entry per test, in a fixed order.
    pub results: Vec<TestResult>,
}

impl Report {
    /// Did every test pass?
    pub fn all_passed(&self) -> bool {
        self.results.iter().all(|r| r.passed)
    }

    /// Number of tests that failed.
    pub fn failures(&self) -> usize {
        self.results.iter().filter(|r| !r.passed).count()
    }

    /// A human-readable table, used in test failure messages.
    pub fn summary(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(
            s,
            "bits = {} ({:.2} MiB), Shannon = {:.8} bits/byte",
            self.bits,
            self.bits as f64 / 8.0 / 1048576.0,
            self.entropy
        );
        for r in &self.results {
            let _ = writeln!(
                s,
                "  {:<28} stat = {:>14.6}  p = {:<10.3e}  {}",
                r.name,
                r.statistic,
                r.p_value,
                if r.passed { "pass" } else { "FAIL" }
            );
        }
        s
    }
}

fn make(name: &'static str, statistic: f64, p_value: f64) -> TestResult {
    TestResult {
        name,
        statistic,
        // A p-value of exactly 0 is a legitimate "infinitely unlikely" answer for
        // structured input; keep it rather than clamping, since clamping would hide the
        // difference between "1e-300" and "no evidence at all".
        p_value,
        passed: p_value >= ALPHA,
    }
}

/// Run the gate's subset over `data`.
///
/// `data` should be the whole container. Passing less measures less, and the block-frequency
/// test in particular needs `≥ 200·BLOCK_M` bits to be meaningful.
pub fn run(data: &[u8]) -> Report {
    let bits = (data.len() as u64) * 8;
    let n = bits as f64;
    // Fixed order, and the count is what the alpha arithmetic in the module docs assumes.
    let results = vec![
        frequency(data, n),
        block_frequency(data, n),
        runs(data, n),
        cumulative_sums(data, n, true),
        cumulative_sums(data, n, false),
        monte_carlo(data),
    ];
    debug_assert_eq!(
        results.len(),
        TEST_COUNT,
        "the alpha budget assumes this many tests"
    );

    Report {
        bits,
        entropy: shannon_entropy(data),
        results,
    }
}

/// SP 800-22 §2.1 Monobit / frequency.
///
/// `p = erfc(|π̂ − 0.5| / √(2/n))`, where `π̂` is the fraction of set bits.
///
/// Note the argument is `√(n/2)`, not `√(2n)` -- the two differ by a factor of 2, which is
/// easy to type wrong and silently makes the test twice as strict as specified. At `n = 2^30`
/// a correct reading gives p near 1 for a 50/50 stream and the wrong one gives p near 0, so
/// `monobit_argument_matches_the_spec` pins the exponent.
pub fn frequency(data: &[u8], n: f64) -> TestResult {
    let ones: u64 = data.iter().map(|&b| b.count_ones() as u64).sum();
    let pi_hat = ones as f64 / n;
    // |pi_hat - 0.5| / sqrt(2/n)  ==  |pi_hat - 0.5| * sqrt(n/2)
    let stat = (pi_hat - 0.5).abs() * (n / 2.0).sqrt();
    make("frequency (monobit)", stat, erfc(stat))
}

/// SP 800-22 §2.2 Block frequency, `M = 128` bits per block.
///
/// `χ² = 4M · Σ (πᵢ − 0.5)²`, `p = Q(m/2, χ²/2)` with `m = ⌊n/M⌋`.
pub fn block_frequency(data: &[u8], n: f64) -> TestResult {
    let m = (n / BLOCK_M as f64).floor() as u64;
    let mut chi = 0.0f64;
    // Count ones per 128-bit block without ever holding a 128-bit value: sixteen popcounts
    // per block. Routing this through `BitBlocks` is what overflowed its `u64` accumulator
    // with a 128-bit shift; see that type's docs.
    //
    // `as_chunks` rather than `chunks_exact` so the block width is a const generic and cannot
    // drift from `BLOCK_M`. The trailing remainder is dropped by `.0`, which is the
    // `floor(n / M)` behaviour SP 800-22 prescribes.
    let (blocks, _) = data.as_chunks::<BLOCK_SLOT_BYTES>();
    let mut seen = 0u64;
    for block in blocks {
        let ones = block.iter().map(|&b| b.count_ones() as u64).sum::<u64>();
        let pi = ones as f64 / BLOCK_M as f64;
        chi += (pi - 0.5) * (pi - 0.5);
        seen += 1;
    }
    debug_assert_eq!(seen, m, "block count must be floor(n/M)");
    let chi = 4.0 * BLOCK_M as f64 * chi;
    let p = gammq(m as f64 / 2.0, chi / 2.0);
    make("block frequency (M=128)", chi, p)
}

/// SP 800-22 §2.3 Runs.
///
/// The prerequisite is the monobit test: if `|π̂ − 0.5| ≥ 2/√n` the run distribution is not
/// the geometric one this test assumes, and SP 800-22 says the test should not be applied.
/// That is reported as a p-value of 0 rather than a wrong answer.
pub fn runs(data: &[u8], n: f64) -> TestResult {
    let ones: u64 = data.iter().map(|&b| b.count_ones() as u64).sum();
    let pi = ones as f64 / n;
    // Prerequisite: the monobit test must have passed, i.e. |pi - 0.5| < 2/sqrt(n). Outside
    // that band the run lengths are not geometric and this test's null distribution does not
    // apply. Reported as p = 0 rather than a computed number, which is the honest answer:
    // "not applicable" and "decisively not random" lead to the same gate verdict.
    let two_over_sqrt_n = 2.0 / n.sqrt();
    if (pi - 0.5).abs() >= two_over_sqrt_n {
        return make("runs", (pi - 0.5).abs(), 0.0);
    }

    // Count transitions exactly, word at a time. Bit 63 of one word is adjacent to bit 0 of
    // the next, so the intra-word count excludes bit 0 and the inter-word bit is added
    // separately. Runs = transitions + 1.
    let mut transitions = 0u64;
    let mut prev_msb = false;
    let mut first = true;
    for w in data.chunks(8) {
        let mut word = 0u64;
        for (i, &b) in w.iter().enumerate() {
            word |= (b as u64) << (8 * i);
        }
        let t = word ^ (word << 1);
        // Bits 1..63 are intra-word transitions.
        transitions += (t & 0xFFFF_FFFF_FFFF_FFFE).count_ones() as u64;
        let lsb = word & 1 == 1;
        if !first && lsb != prev_msb {
            transitions += 1;
        }
        prev_msb = word >> 63 == 1;
        first = false;
    }
    // SP 800-22 sec 2.3.3:
    //
    //   E[V] = 2n*pi*(1-pi)
    //   p    = erfc( |V - E[V]| / (2*sqrt(2n*pi*(1-pi))) )
    //
    // The (2n-1)/2n correction to the variance is dropped, which SP 800-22 itself does; it
    // moves p by O(1/n) and keeping it would mean reproducing a formula the current revision
    // no longer prints.
    let v = (transitions + 1) as f64;
    let e = 2.0 * n * pi * (1.0 - pi);
    let denom = 2.0 * e.sqrt();
    let stat = (v - e).abs() / denom;
    make("runs", stat, erfc(stat))
}

/// SP 800-22 §2.4 Cumulative sums, forward (`forward = true`) and reverse.
///
/// `z = max_k |S_k|`, and `p = erfc(|z| adjusted / √(2(n−1)))`. For the forward test the
/// argument is `(z + 0.5)/√(n−1)`; for the reverse test `(−z + 0.5)/√(n−1)`, i.e. `(z −
/// 0.5)/√(n−1)`. Both are written via `erfc` rather than `1 − Φ` so that a genuinely tiny
/// p-value is computed instead of cancelling to zero.
pub fn cumulative_sums(data: &[u8], n: f64, forward: bool) -> TestResult {
    let (z, _) = cusum_max_abs(data);
    let s = (n - 1.0).sqrt();

    // SP 800-22 sec 2.4.4, both tails and the z < 0.5 continuation:
    //
    //   forward:  1 - Phi( ( z + 0.5)/sqrt(n-1))   if z >= 0.5
    //             1 - Phi( ( z - 0.5)/sqrt(n-1))   if z <  0.5
    //   reverse:  1 - Phi((-z + 0.5)/sqrt(n-1))   if z >= 0.5
    //             1 - Phi((-z - 0.5)/sqrt(n-1))   if z <  0.5
    //
    // and then the conversion that is easy to get wrong:
    //
    //   Phi(t) = (1 + erf(t/sqrt2))/2   =>   1 - Phi(t) = erfc(t/sqrt2) / 2
    //
    // The factor of one half is not cosmetic. Without it every cumulative-sums p-value came
    // out exactly twice the truth, which never sent one out of range and never failed an
    // obvious check -- it just made the test silently twice as strict as specified and
    // pushed its p-values up the uniform distribution, where over-rejection only shows up
    // statistically. `p_value_matches_the_normal_tail` pins the factor, and
    // `p_values_are_calibrated_against_csprng` is what would catch its loss.
    //
    // With the factor in place the reverse branch is also well defined: without the z >= 0.5
    // split, the reverse argument goes positive for large z and erfc of it exceeds 1, which
    // is what an earlier revision returned -- a p-value of 1.53 on real random data.
    let t = if forward {
        if z >= 0.5 {
            (z + 0.5) / s
        } else {
            (z - 0.5) / s
        }
    } else if z >= 0.5 {
        (0.5 - z) / s
    } else {
        (-z - 0.5) / s
    };
    make(
        if forward {
            "cumulative sums (forward)"
        } else {
            "cumulative sums (reverse)"
        },
        z,
        0.5 * erfc(t / std::f64::consts::SQRT_2),
    )
}

/// SP 800-22 §2.3.4 Monte Carlo ("π") test.
///
/// # A deliberate deviation, stated
///
/// The construction is SP 800-22's: split into [`MC_BLOCK_BITS`]-bit blocks and count the
/// fraction `q_obs` landing in `[0, πk) ∪ [πk + (1−π)k, k)` with `k = 2^15` and `π = 0.4`.
///
/// The p-value is **not** the paper's printed asymptotic form. Under the null hypothesis
/// each block hits with probability exactly `q = 2π = 0.8`, independent of the block size —
/// that is a statement about the construction, not an approximation. So the test is a
/// two-sided test of a binomial proportion with `M` trials at `q`, evaluated as
///
/// ```text
/// z = (|count − M·q| − 0.5) / √(M·q·(1−q)),    p = erfc(|z| / √2)
/// ```
///
/// with a continuity correction. This is strictly better than the paper's normal
/// approximation to the same quantity: same statistic family, but derived from the null
/// distribution rather than fitted to it. It is recorded here as a deviation because the
/// printed constants could not be verified against the publication on this host, and
/// shipping a formula quoted from memory into the one test the gate leans on would be worse
/// than deriving the right one.
pub fn monte_carlo(data: &[u8]) -> TestResult {
    let k = (1u64 << MC_BLOCK_BITS) as f64;
    let n = (data.len() as u64) * 8;
    let m = n / MC_BLOCK_BITS as u64;
    // The acceptance region is the two tails of [0, k): [0, pi*k) and [(1-pi)*k, k), each of
    // width pi*k, so the null probability is exactly 2*pi = 0.8.
    //
    // The upper bound is `(1 - pi) * k`, NOT `k`. Writing it as `pi*k + (1-pi)*k` looks like
    // a faithful rendering of the spec and equals `k` exactly, which silently makes the second
    // interval empty and drops the acceptance probability to 0.4 -- a 5-sigma rejection of
    // perfect random data, which is how this was found.
    let lo = (MC_PI * k) as u64;
    let hi = ((1.0 - MC_PI) * k) as u64;

    let mut count = 0u64;
    let mut reader = BitBlocks::new(data, MC_BLOCK_BITS);
    while let Some(block) = reader.next() {
        if block < lo || block >= hi {
            count += 1;
        }
    }

    let q = 2.0 * MC_PI; // 0.8 exactly, the null probability of a block hitting.
    debug_assert!(
        (hi - lo) as f64 * 2.0 / k - q < 1e-4,
        "the acceptance region must cover 2*pi of [0, k)"
    );
    let mf = m as f64;
    let delta = (count as f64 - mf * q).abs() - 0.5;
    let sd = (mf * q * (1.0 - q)).sqrt();
    let z = delta / sd;
    make(
        "monte carlo (pi)",
        z,
        erfc(z.abs() / std::f64::consts::SQRT_2),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `len` bytes of real CSPRNG output.
    fn random(len: usize) -> Vec<u8> {
        let mut v = vec![0u8; len];
        getrandom::fill(&mut v).expect("os rng");
        v
    }

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol * b.abs().max(1.0)
    }

    // -- Special functions -------------------------------------------------

    /// `erfc` against reference values.
    ///
    /// The table is generated from CPython's `math.erfc`, which is a separate
    /// implementation in a separate language -- not from this crate's own output, which would
    /// make the test a tautology. The points straddle the branch inside [`gammq`] at
    /// `x = √1.5 ≈ 1.2247`, which is where the implementation switches from the cancelling
    /// series to the continued fraction.
    #[test]
    fn erfc_matches_an_independent_implementation() {
        let table = [
            (0.0, 1.0),
            (0.1, 0.887_537_083_981_715),
            (0.25, 0.723_673_609_831_763_1),
            (0.5, 0.479_500_122_186_953_5),
            (0.75, 0.288_844_366_346_484_86),
            (1.0, 0.157_299_207_050_285_13),
            (1.25, 0.077_099_871_743_541_77),
            (1.5, 0.033_894_853_524_689_274),
            (2.0, 0.004_677_734_981_047_266),
            (2.5, 0.000_406_952_017_444_958_9),
            (3.0, 2.209_049_699_858_544e-5),
            (4.0, 1.541_725_790_028_002e-8),
            (5.0, 1.537_459_794_428_035e-12),
            (6.0, 2.151_973_671_249_891_3e-17),
            (8.0, 1.122_429_717_298_292_6e-29),
        ];
        for (x, want) in table {
            let got = erfc(x);
            let err = (got - want).abs() / want.abs();
            assert!(
                err < ERFC_ACCURACY,
                "erfc({x}) = {got:.8e}, reference {want:.8e}, relative error {err:.2e}"
            );
        }
    }

    /// `erfc(-x) = 2 - erfc(x)`.
    #[test]
    fn erfc_is_symmetric_about_one_half() {
        for &x in &[0.0, 0.3, 1.0, 2.5, 5.0] {
            assert!(close(erfc(x) + erfc(-x), 2.0, 1e-12), "at x = {x}");
        }
    }

    /// `log_gamma` against exact values.
    #[test]
    fn log_gamma_matches_exact_values() {
        // Gamma(1) = 1, Gamma(2) = 1, Gamma(5) = 24, Gamma(0.5) = sqrt(pi).
        assert!(close(log_gamma(1.0), 0.0, 1e-13));
        assert!(close(log_gamma(2.0), 0.0, 1e-13));
        // Reference values from CPython's `math.lgamma`, an independent implementation.
        let table = [
            (0.5, 0.572_364_942_924_700_4),
            (1.0, 0.0),
            (2.0, 0.0),
            (5.0, 3.178_053_830_347_945),
            (9.5, 11.689_333_420_797_269),
            (100.0, 359.134_205_369_575_4),
            // The magnitude the block-frequency test actually reaches.
            (4_194_304.0, 59_765_629.118_568_19),
        ];
        for (x, want) in table {
            assert!(
                close(log_gamma(x), want, 1e-13),
                "log_gamma({x}) = {:.10}, reference {want:.10}",
                log_gamma(x)
            );
        }
        // The closed forms, so the table is not merely a copy of a shared bug.
        assert!(close(log_gamma(5.0), 24f64.ln(), 1e-14));
        assert!(close(
            log_gamma(0.5),
            std::f64::consts::PI.sqrt().ln(),
            1e-14
        ));
    }

    /// `Q(1, x) = e^-x` exactly, and `Q(2, x) = e^-x(1+x)`. These are the only values of the
    /// regularised upper incomplete gamma available in closed form, and they pin the
    /// continued fraction at small `a` where the algebra is checkable by hand.
    #[test]
    fn gammq_matches_its_closed_forms() {
        for &x in &[0.25, 0.5, 1.0, 2.0, 4.0, 8.0] {
            assert!(
                close(gammq(1.0, x), (-x).exp(), 1e-12),
                "Q(1, {x}): {} vs {}",
                gammq(1.0, x),
                (-x).exp()
            );
            let want = (-x).exp() * (1.0 + x);
            assert!(
                close(gammq(2.0, x), want, 1e-12),
                "Q(2, {x}): {} vs {want}",
                gammq(2.0, x)
            );
        }
    }

    /// The block-frequency test runs the incomplete gamma at `a ≈ 4.2e6`, where a naive
    /// implementation underflows its prefactor to zero and returns 0. This walks `a` upward
    /// and checks `Q` stays finite, in range, and close to 0.5 as `x` approaches `a`.
    #[test]
    fn gammq_survives_the_block_frequency_arguments() {
        for &m in &[1_000u64, 100_000, 4_194_304, 8_388_608] {
            let a = m as f64 / 2.0;
            // chi^2 concentrates at m, so x = chi^2/2 concentrates at m/2 = a.
            // Q(a, a) -> 1/2, but not exactly: the deviation is the first non-vanishing
            // asymptotic term, of order 1/sqrt(a). At a = 500 that is 4.5e-2 and the measured
            // value is 0.4940, so a fixed tolerance has to be generous or the test is really
            // only testing the largest argument.
            let at_mode = gammq(a, a);
            let tol = 3.0 / a.sqrt();
            assert!(
                (at_mode - 0.5).abs() < tol,
                "Q(a, a) at a = {a} was {at_mode}, expected 0.5 +/- {tol}"
            );
            // A standard deviation either side of the mode.
            let sd = (2.0 * m as f64).sqrt() / 2.0;
            for k in [-3.0f64, -1.0, 1.0, 3.0] {
                let x = a + k * sd;
                let q = gammq(a, x);
                assert!(
                    q.is_finite() && (0.0..=1.0).contains(&q),
                    "Q({a}, {x}) = {q}, out of range at a = {a}"
                );
            }
            // Monotone decreasing in x.
            assert!(gammq(a, a - sd) > at_mode && at_mode > gammq(a, a + sd));
        }
    }

    /// Cross-check the continued fraction against the **recurrence relation**
    /// `Q(a+1, x) = Q(a, x) + x^a e^-x / Γ(a+1)`.
    ///
    /// This is the check that matters at large arguments. Both sides of the equation go
    /// through the same continued fraction, so this does not compare two different
    /// algorithms — but it does compare two *different arguments*, and a continued fraction
    /// truncated at too few iterations, or one whose Lentz iteration underflows, disagrees
    /// with the recurrence immediately. Truncation is the failure mode that a
    /// `gammp + gammq == 1` check cannot see, because that identity holds by construction.
    #[test]
    fn gammq_satisfies_the_a_plus_one_recurrence() {
        for &(a, x) in &[
            (1.0, 2.0),
            (3.5, 7.0),
            (1e3, 1.1e3),
            (1e5, 1.0e5),
            (1e5, 1.1e5),
            (4.194_304e0, 4.194_304e0),
            (4.194_304e0, 4.194_304e0 + 2_048.0),
            (4.194_304e0, 4.194_304e0 + 16384.0),
        ] {
            let lhs = gammq(a + 1.0, x);
            let increment = (a * x.ln() - x - log_gamma(a + 1.0)).exp();
            let rhs = gammq(a, x) + increment;
            // The continued fraction carries about 1e-10 of relative error at these
            // magnitudes, so the bar is 1e-9. That is still tight enough to catch a
            // truncated Lentz loop, which is what this test is for: truncation shows up as a
            // disagreement many orders of magnitude larger than 1e-9.
            let err = (lhs - rhs).abs() / rhs.abs().max(1e-300);
            assert!(
                err < 1e-9,
                "Q(a+1, x) recurrence at a = {a}, x = {x}: {lhs:.12e} vs {rhs:.12e} (err {err:.2e})"
            );
        }
    }

    /// `P` and `Q` must complement.
    #[test]
    fn gammp_and_gammq_complement() {
        for &(a, x) in &[
            (0.5, 0.5),
            (1.0, 1.0),
            (10.0, 3.0),
            (10.0, 30.0),
            (1e4, 9e3),
            (1e4, 12e3),
            (4.194_304e0, 4.194_304e0),
        ] {
            assert!(
                close(gammp(a, x) + gammq(a, x), 1.0, 1e-12),
                "at a = {a}, x = {x}"
            );
        }
    }

    // -- Building blocks ---------------------------------------------------

    /// The bit reader must emit `floor(bits / width)` blocks and drop the tail.
    #[test]
    fn bit_blocks_drops_the_tail() {
        // 10 bytes = 80 bits. width 7 -> 11 blocks (77 bits), 3 bits discarded.
        let data: Vec<u8> = (0..10).map(|i| i as u8).collect();
        let mut r = BitBlocks::new(&data, 7);
        let mut count = 0;
        while r.next().is_some() {
            count += 1;
        }
        assert_eq!(count, 80 / 7);

        // And the values must be the right bits, LSB-first within each byte.
        // Byte 0b1011_0010 read LSB-first is the bit sequence 0,1,0,0,1,1,0,1.
        // Grouped into 4-bit blocks, least significant bit first within each:
        //   block 0 = b0,b1,b2,b3 = 0,1,0,0 -> 0b0010 = 2
        //   block 1 = b4,b5,b6,b7 = 1,1,0,1 -> 0b1011 = 11
        let mut r = BitBlocks::new(&[0b1011_0010], 4);
        let got: Vec<u64> = std::iter::from_fn(|| r.next()).collect();
        assert_eq!(got, vec![2, 11]);
    }

    /// The byte-table cumulative-sum walk must agree with a naive bit-at-a-time walk.
    ///
    /// The table exists for speed (one lookup per byte instead of one iteration per bit), and
    /// a fast path that disagrees with the obvious implementation is the classic way to ship
    /// a subtly wrong number. Checked on adversarial patterns, not just random data, because
    /// random data almost never reaches the excursion extremes the table encodes.
    #[test]
    fn cusum_matches_a_naive_walk() {
        let cases: Vec<Vec<u8>> = vec![
            vec![0u8; 64],
            vec![0xFFu8; 64],
            (0..64)
                .map(|i| if i % 2 == 0 { 0xAA } else { 0x55 })
                .collect(),
            vec![0x00; 31].into_iter().chain(vec![0xFF; 33]).collect(),
            random(500),
            random(997),
        ];
        for data in cases {
            let (fast, net) = cusum_max_abs(&data);
            let (mut s, mut best) = (0i64, 0i64);
            for &b in &data {
                for k in 0..8 {
                    s += if (b >> k) & 1 == 1 { 1 } else { -1 };
                    best = best.max(s.abs());
                }
            }
            assert_eq!(net, s, "net walk differs on {} bytes", data.len());
            assert_eq!(
                fast,
                best as f64,
                "max |S| differs on {} bytes: table {fast}, naive {}",
                data.len(),
                best
            );
        }
    }

    /// Block frequency over 128-bit blocks must count ones correctly and drop the tail.
    ///
    /// Pins the chi-square to a hand-computed value. A 128-bit block all-ones has `pi = 1`,
    /// so each block contributes `(1 - 0.5)^2 = 0.25` and `chi² = 4 * 128 * 0.25 * blocks`.
    #[test]
    fn block_frequency_counts_ones_per_128_bit_block() {
        const BLOCKS: usize = 8;
        let all_ones = vec![0xFFu8; BLOCKS * BLOCK_M / 8];
        let r = block_frequency(&all_ones, (all_ones.len() * 8) as f64);
        assert_eq!(r.statistic, 4.0 * 128.0 * 0.25 * BLOCKS as f64);

        // All-zero blocks give the same chi-square: the statistic is symmetric about 0.5.
        let all_zero = vec![0u8; BLOCKS * BLOCK_M / 8];
        assert_eq!(
            block_frequency(&all_zero, (all_zero.len() * 8) as f64).statistic,
            r.statistic
        );

        // Half-and-half blocks give chi-square 0.
        // Half 0xFF then half 0x00 *within each 16-byte block*, so every block has pi = 0.5.
        // Making the first 16 bytes all 0xFF instead would give block 0 pi = 1 and the rest
        // pi = 0 -- the same chi-square as the all-ones case, which is what this test caught
        // when it was written that way.
        let mixed: Vec<u8> = (0..BLOCKS * BLOCK_M / 8)
            .map(|i| if i % 16 < 8 { 0xFF } else { 0x00 })
            .collect();
        assert_eq!(
            block_frequency(&mixed, (mixed.len() * 8) as f64).statistic,
            0.0
        );

        // A trailing partial block is dropped, so a 17-byte input is one whole block.
        let ragged = vec![0xFFu8; BLOCK_M / 8 + 1];
        let r = block_frequency(&ragged, (ragged.len() * 8) as f64);
        assert_eq!(r.statistic, 4.0 * 128.0 * 0.25);
    }

    /// `BitBlocks` must refuse widths it cannot represent rather than overflowing.
    ///
    /// `#[should_panic]`, because the alternative was a release build silently producing
    /// wrong blocks -- which is exactly how a 128-bit block width got this far in the first
    /// place.
    #[test]
    #[should_panic(expected = "1..=56")]
    fn bit_blocks_refuses_an_unrepresentable_width() {
        let _ = BitBlocks::new(&[0u8; 64], 128);
    }

    /// A single test's statistic must match the formula in its doc comment, computed by hand.
    #[test]
    fn monobit_argument_matches_the_spec() {
        // 1024 bytes = 8192 bits, with 4104 bits set: pi_hat - 0.5 = 8/8192.
        let mut data = vec![0xFFu8; 1024];
        for b in data.iter_mut() {
            *b = 0xFF;
        }
        // Clear 4088 bits, one byte at a time: 511 whole bytes cleared leaves 8 bits set.
        for b in data.iter_mut().take(511) {
            *b = 0x00;
        }
        let ones: u64 = data.iter().map(|b| b.count_ones() as u64).sum();
        assert_eq!(ones, 8 * 1024 - 511 * 8);
        let n = (data.len() * 8) as f64;
        let r = frequency(&data, n);
        // |pi_hat - 0.5| / sqrt(2/n)
        let expect = (ones as f64 / n - 0.5).abs() / (2.0 / n).sqrt();
        assert!(
            close(r.statistic, expect, 1e-12),
            "statistic {} vs spec {expect}",
            r.statistic
        );
        assert!(close(r.p_value, erfc(expect), 1e-12));
        // And explicitly *not* the factor-of-2 variant that a plausible typo produces.
        assert!(
            !close(r.statistic, expect * 2.0, 1e-6),
            "statistic is exactly twice the spec value, which is the typo this pins"
        );
    }

    // -- Discrimination: can these tests tell random from not? -----------------

    /// Real CSPRNG output must pass every test. If this fails, the tests are broken, not the
    /// container.
    #[test]
    fn random_data_passes_every_test() {
        let data = random(1 << 18);
        let r = run(&data);
        assert!(r.all_passed(), "random data was rejected:\n{}", r.summary());
        assert_eq!(r.bits, (1 << 18) as u64 * 8);
    }

    /// Structured inputs must be rejected. A statistical test that cannot tell ChaCha20 from
    /// all-zeros is not a test, and this is the assertion that gives the entropy gate its
    /// force.
    ///
    /// Each case names the test that should catch it, so a failure points at the specific
    /// weakness rather than just "something rejected it".
    #[test]
    fn structured_data_is_rejected() {
        let n = 1 << 16; // 65536 bytes
        let cases: Vec<(&str, Vec<u8>, &str)> = vec![
            ("all zero", vec![0u8; n], "frequency (monobit)"),
            ("all 0xFF", vec![0xFFu8; n], "frequency (monobit)"),
            (
                "alternating bytes 0x55/0xAA",
                (0..n)
                    .map(|i| if i % 2 == 0 { 0x55 } else { 0xAA })
                    .collect(),
                "monte carlo (pi)",
            ),
            (
                "1 KiB of 0x00 alternating with 1 KiB of 0xFF",
                (0..n)
                    .map(|i| if (i / 1024) % 2 == 0 { 0x00 } else { 0xFF })
                    .collect(),
                "block frequency (M=128)",
            ),
            (
                "repeating 3-byte ramp",
                (0..n).map(|i| (i % 3) as u8).collect(),
                "cumulative sums (forward)",
            ),
            // Not block frequency. At a 43-byte period the per-128-bit-block bias is small
            // enough that chi-square lands *below* its expectation and p comes out near 1 --
            // the test correctly reports "no evidence here". The monobit test is what fires,
            // because Shannon is 4.39 bits/byte against a maximum of 8. Recorded rather than
            // quietly re-specified, since "block frequency should catch text" is a natural
            // and wrong assumption: SP 800-22's block frequency is weak against periodicity
            // that does not line up with the block size.
            (
                "English-ish text",
                (0..n)
                    .map(|i| b"the quick brown fox jumps over the lazy dog "[i % 43])
                    .collect(),
                "frequency (monobit)",
            ),
        ];
        for (label, data, expect_fires) in cases {
            let r = run(&data);
            assert!(
                !r.all_passed(),
                "{label:?} was accepted as random:\n{}",
                r.summary()
            );
            let fired: Vec<&str> = r
                .results
                .iter()
                .filter(|t| !t.passed)
                .map(|t| t.name)
                .collect();
            assert!(
                fired.contains(&expect_fires),
                "{label:?}: expected {expect_fires:?} to fire, got {fired:?}\n{}",
                r.summary()
            );
        }
    }

    /// A single flipped bit in 128 MiB of random data must be rejected by the monobit test.
    ///
    /// This is the sharpest version of "can it tell random from not": one bit in 1.07 billion
    /// shifts `π̂` by 9.3e-10, which over `n = 2^30` bits is a statistic of about 0.043, so
    /// p ≈ 0.965 — **not** rejected. It is included because that is the honest limit of what
    /// these tests can do, and the assertion records it rather than pretending otherwise.
    ///
    /// The corresponding claim that *is* tested is in
    /// `a_few_percent_bias_is_rejected`: 0.5% of the bits flipped is caught with certainty.
    #[test]
    fn a_single_flipped_bit_is_below_the_floor_of_these_tests() {
        // **Deterministic, and the first version of this was not.**
        //
        // It asserted `p_value < 1.0` on `random(1 << 16)` with one bit flipped. The monobit p-value is
        // `erfc(|pi_hat - 0.5| * sqrt(2n))`, so it is 1.0 exactly when the post-flip bit count lands
        // *closer to the mean* than the pre-flip count -- which is a coin flip, not a rare event. It
        // passed 500-odd consecutive runs and then failed on one of them, in a workspace run where the
        // only thing that had changed was several crates away.
        //
        // The claim the test is actually making is about **detection floor**, and that is a statement
        // about the *statistic*, which moves by exactly one and whose sign is known. So assert that:
        let mut data = random(1 << 16);
        let before = run(&data);
        let count_before: u64 = data.iter().map(|b| b.count_ones() as u64).sum();

        data[1234] ^= 0x01;
        let after = run(&data);
        let count_after: u64 = data.iter().map(|b| b.count_ones() as u64).sum();

        // Exactly one bit moved, in the direction the flip dictated: clearing 0x01 loses a one, setting
        // it gains one.
        assert_eq!(
            count_after.abs_diff(count_before),
            1,
            "flipping one bit changed the count by {}",
            count_after.abs_diff(count_before)
        );
        // And the statistic moved with it, monotonically: |pi_hat - 0.5| is a distance to the mean, so
        // moving the count *toward* `n/2` shrinks it and moving away grows it.
        // Whether the statistic grows or shrinks depends on which side of the mean the count started,
        // and that is the coin flip the old assertion lost. What is always true is that it *moved*, and
        // by an amount on the order of one bit in 2^19 -- which is the point of the test: the statistic
        // does move, and it moves by almost nothing.
        let delta = (after.results[0].statistic - before.results[0].statistic).abs();
        assert!(
            delta > 0.0 && delta < 0.01,
            "one bit in 2^19 moved the statistic by {delta}, which is neither nothing nor a lot"
        );
        // The p-value is still a probability -- which is the part the original assertion meant, and it
        // is now asserted where it cannot be a coin flip.
        assert!(
            (0.0..=1.0).contains(&after.results[0].p_value),
            "monobit p-value {} is not a probability",
            after.results[0].p_value
        );
    }

    /// A small but real bias must be caught. This is the claim the entropy gate actually
    /// rests on: not "a flipped bit is visible", but "a systematically skewed stream is".
    #[test]
    fn a_few_percent_bias_is_rejected() {
        let n = 1 << 16;
        for pct in [1u32, 2, 5, 20] {
            // Force `pct`% of the bits to 1 by construction, rest random.
            let mut data = random(n);
            for (i, b) in data.iter_mut().enumerate() {
                if i % 100 < pct as usize {
                    *b = 0xFF;
                }
            }
            let r = run(&data);
            assert!(
                !r.all_passed(),
                "a {pct}% bias was accepted:\n{}",
                r.summary()
            );
        }
    }

    // -- Calibration -------------------------------------------------------

    /// P-values must be roughly uniform, so the gate fails at the rate it claims.
    ///
    /// Honest about its power: 120 draws × 6 tests = 720 instances at `α = 0.001` gives
    /// 0.72 expected failures, so this can only detect gross over-rejection — a rate of 5%
    /// or worse — not a subtle one. Detecting a 1% inflation properly needs ~10⁵ instances,
    /// which does not belong in a unit test. What it does rule out is the failure mode that
    /// would silently invalidate the gate: a formula error that rejects good data.
    #[test]
    fn p_values_are_calibrated_against_csprng() {
        const DRAWS: usize = 120;
        let mut failures = 0;
        for _ in 0..DRAWS {
            failures += run(&random(1 << 14)).failures();
        }
        assert!(
            failures <= 3,
            "{failures} failures in {DRAWS} draws x 6 tests at alpha = {ALPHA}; \
             expected about {}. A high count means a p-value formula is wrong, not that the \
             CSPRNG is bad.\n",
            (DRAWS * 6) as f64 * ALPHA
        );
    }

    /// Characterise the entropy floor, because the specified value is size-dependent and
    /// that is not obvious.
    ///
    /// The plug-in estimator is biased low: `E[H] = 8 − 255/(2m·ln2)`, so the mean falls as
    /// the sample shrinks. At the 128 MiB the gate uses the bias is 1.37e-6 and the floor of
    /// 7.99999 sits ~8.6e-6 below it. At 16 MiB the bias is 1.1e-5 and the *mean itself* is
    /// below the floor -- so the same check run on a 16 MiB slice of a perfectly valid
    /// container would fail. The floor is only meaningful at the full container size, and
    /// that constraint is now written down instead of left to be rediscovered.
    ///
    /// The spread is the more surprising half, and it is measured rather than derived. The
    /// textbook asymptotic result says `sd(H) ∝ 1/sqrt(m)`, which at 128 MiB predicts
    /// 1.4e-3 and would put the floor 0.006 sd below the mean -- a gate failing about half the
    /// time on good data. Measurement over 60 draws at four sizes says otherwise:
    ///
    /// ```text
    /// m = 2^20  mean 7.999825490  sd 1.420e-5
    /// m = 2^22  mean 7.999955800  sd 3.540e-6
    /// m = 2^24  mean 7.999989173  sd 8.604e-7
    /// m = 2^26  mean 7.999997246  sd 2.273e-7
    /// ```
    ///
    /// `sd·m` is 14.9, 14.8, 14.4, 15.3 -- flat, so `sd ∝ 1/m`. The reason is that the
    /// first-order multinomial fluctuation cancels exactly: for a uniform distribution
    /// `Var(sum w_i d_i) = (1/m)(sum w_i^2 p_i - (sum w_i p_i)^2) = w^2 - w^2 = 0`, so the
    /// deviation is second order. `sd ∝ 1/sqrt(m)` applies when the true distribution is
    /// non-uniform; it does not apply here, and taking it at face value would have condemned
    /// a correct threshold.
    #[test]
    fn the_entropy_floor_is_valid_at_the_container_size() {
        const M: f64 = 134_217_728.0; // 128 MiB

        // The measured 1/m law, with the constant taken from the table above.
        const SD_NUMERATOR: f64 = 14.9;
        let mean = 8.0 - 255.0 / (2.0 * M * std::f64::consts::LN_2);
        let sd = SD_NUMERATOR / M;

        assert_eq!(M, 134_217_728.0, "the gate's sample size is not negotiable");
        let margin_sd = (mean - ENTROPY_FLOOR) / sd;
        assert!(
            margin_sd > 10.0,
            "the floor is only {margin_sd:.1} sd below the mean at 128 MiB;              expected tens of sd. Either sd is being estimated as 1/sqrt(m) again, or              ENTROPY_FLOOR was raised."
        );

        // And the size dependence, stated as an assertion so it cannot be forgotten.
        let mean_16mib = 8.0 - 255.0 / (2.0 * (M / 8.0) * std::f64::consts::LN_2);
        assert!(
            mean_16mib < ENTROPY_FLOOR,
            "at 16 MiB the mean {mean_16mib:.9} is below the floor {ENTROPY_FLOOR}, so the \
             floor must not be applied to samples smaller than the container"
        );
        assert!(
            close(mean, 7.999_998_629_513_251, 1e-12),
            "the 128 MiB mean is {mean:.15}"
        );
    }

    /// The measured entropy of real CSPRNG output must track the predicted mean, at a size
    /// small enough to run in a unit test.
    #[test]
    fn measured_entropy_tracks_the_predicted_mean() {
        // 1 MiB is where the earlier 60-draw measurement put sd at 1.42e-5, so a single draw
        // should land within a few times that of 8 - 1.754e-4.
        let m = 1usize << 20;
        let predicted = 8.0 - 255.0 / (2.0 * m as f64 * std::f64::consts::LN_2);
        let h = shannon_entropy(&random(m));
        assert!(
            (h - predicted).abs() < 6.0 * (14.9 / m as f64),
            "entropy {h:.9} vs predicted {predicted:.9} is more than 6 sd out"
        );
        assert!(h < 8.0, "the estimator cannot exceed log2(256) = 8");
    }

    /// Entropy of degenerate inputs must be obviously low.
    #[test]
    fn entropy_of_structured_data_is_low() {
        assert_eq!(shannon_entropy(&[0u8; 4096]), 0.0);
        assert_eq!(shannon_entropy(&[0xFFu8; 4096]), 0.0);
        // Not "close to 8": the plug-in estimator is biased low by (k-1)/(2m ln2), which is
        // 1.75e-4 at 1 MiB -- a thousand times the tolerance one would reach for by eye.
        let m = 1usize << 20;
        let bias = 255.0 / (2.0 * m as f64 * std::f64::consts::LN_2);
        let h = shannon_entropy(&random(m));
        assert!(
            close(h, 8.0 - bias, 0.05),
            "entropy {h:.9} is not within 5% of the predicted mean {:.9}",
            8.0 - bias
        );
        // Two equally likely symbols -> exactly 1 bit.
        let two: Vec<u8> = (0..4096).map(|i| (i % 2) as u8).collect();
        assert!(close(shannon_entropy(&two), 1.0, 1e-12));
    }

    /// The cumulative-sums p-value must be `1 - Phi(t)` exactly, including the factor of
    /// one half.
    ///
    /// A deliberately hand-built walk whose excursion is exactly 1.
    ///
    /// `0x55` is `0101_0101` and bits are read LSB-first, so the walk goes +1, -1, +1, -1
    /// and `S_k` never leaves {0, 1}: `z = 1` exactly, over `n = 512` bits.
    ///
    /// An all-zero buffer also gives an exact `z`, but it gives `z = n`, whose p-value
    /// underflows to zero -- and a p-value of zero cannot distinguish a correct factor from
    /// a doubled one, so it cannot test the thing this test is testing.
    #[test]
    fn p_value_matches_the_normal_tail() {
        let data = vec![0x55u8; 64];
        let n = (data.len() * 8) as f64;
        let z = 1.0;

        let fwd = cumulative_sums(&data, n, true);
        let t = (z + 0.5) / (n - 1.0).sqrt();
        assert_eq!(
            fwd.statistic, z,
            "an alternating walk must have excursion 1, not {}",
            fwd.statistic
        );
        assert!(
            close(fwd.p_value, 0.5 * erfc(t / std::f64::consts::SQRT_2), 1e-12),
            "forward p = {} vs {}",
            fwd.p_value,
            0.5 * erfc(t / std::f64::consts::SQRT_2)
        );
        // And explicitly not the factor-of-two variant.
        assert!(
            !close(fwd.p_value, erfc(t / std::f64::consts::SQRT_2), 1e-6),
            "the 1/2 in 1 - Phi(t) = erfc(t/sqrt2)/2 is missing"
        );

        let rev = cumulative_sums(&data, n, false);
        assert!(
            (0.0..=1.0).contains(&rev.p_value),
            "reverse p = {} is not a probability",
            rev.p_value
        );
        let tr = (0.5 - z) / (n - 1.0).sqrt();
        assert!(close(
            rev.p_value,
            0.5 * erfc(tr / std::f64::consts::SQRT_2),
            1e-12
        ));
    }

    /// The Monte Carlo acceptance region must cover exactly `2*pi` of `[0, k)`.
    ///
    /// With the upper bound written as `k` instead of `(1-pi)*k` the region collapses to
    /// `[0, pi*k)`, the null probability becomes 0.4, and real random data is rejected at
    /// about 5 sigma. This asserts the region's width directly.
    #[test]
    fn the_monte_carlo_region_covers_two_pi() {
        let k = (1u64 << MC_BLOCK_BITS) as f64;
        let lo = (MC_PI * k) as u64;
        let hi = ((1.0 - MC_PI) * k) as u64;
        let covered = ((lo as f64) + (k - hi as f64)) / k;
        // 1e-3, not 1e-6: lo and hi are integers, so the region's width is quantised to
        // 2/k = 6.1e-5 and an exact comparison would fail on the truncation alone.
        assert!(
            close(covered, 2.0 * MC_PI, 1e-3),
            "acceptance region covers {covered} of [0, k), expected {}",
            2.0 * MC_PI
        );
        // And the interval must be a strict subset, so the second tail exists at all.
        assert!(hi < k as u64, "the upper tail is empty");
        assert!(lo < hi, "the lower tail is empty");
    }

    /// The Monte Carlo p-value on real random data must not be extreme.
    #[test]
    fn monte_carlo_does_not_reject_random_data() {
        let data = random(1 << 16);
        let r = monte_carlo(&data);
        assert!(
            r.p_value > 1e-4,
            "the Monte Carlo test rejected {}-byte random data: stat = {}, p = {}",
            data.len(),
            r.statistic,
            r.p_value
        );
    }

    /// A p-value must always be a probability, for any input. Guards against the sign error
    /// in the reverse cumulative-sums formula, which produces `erfc` of a positive argument
    /// and so returns a number above 1.
    #[test]
    fn p_values_are_always_in_range() {
        let mut inputs: Vec<Vec<u8>> = vec![
            vec![0u8; 4096],
            vec![0xFFu8; 4096],
            random(4096),
            vec![0x55; 4096],
        ];
        inputs.push(
            (0..4096)
                .map(|i| if i % 2 == 0 { 0xAA } else { 0x55 })
                .collect(),
        );
        for data in inputs {
            let r = run(&data);
            for t in &r.results {
                assert!(
                    (0.0..=1.0).contains(&t.p_value),
                    "{} returned p = {} for a {}-byte input",
                    t.name,
                    t.p_value,
                    data.len()
                );
            }
            assert!((0.0..=8.0).contains(&r.entropy));
        }
    }
}
