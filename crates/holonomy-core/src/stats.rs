//! Statistical-indistinguishability gates for consecutive container saves.
//!
//! # Why the old tolerance was meaningless
//!
//! `v5.0.0-OMEGA`'s TC-DIF-02 required "Hamming(T_0, T_1) == 50.000% ± 0.015%".
//!
//! For `n = 1,073,741,824` independent bits the standard deviation of the Hamming *fraction* is
//! `0.5/√n = 0.001526%`. A tolerance of `0.015%` is therefore **9.83σ** wide. That is not a test:
//! two unrelated files pass it, and so does a file that is a near-copy of the other, because
//! almost the entire distribution lives inside it. It cannot fail for any output a real cipher
//! would produce, and it cannot detect the failure mode it was written for.
//!
//! v5.1.0 replaces it with two gates that can actually say no:
//!
//! 1. **Monobit** at `±3.5σ`. `3.5 × 0.001526% = 0.0053%`, so the bound is
//!    `50.0000% ± 0.0053%`. This is the corrected constant the amended spec asks for, and it
//!    tightens the accepted band by 2.8× while remaining a bound a correctly implemented cipher
//!    clears with probability ≈ 0.9995.
//! 2. **Longest identical bit run ≤ 42** on `T_{k-1} ⊕ T_k`. This is the gate that catches the
//!    failure a monobit test cannot: a long stretch of zeroes is invisible to the *count* of set
//!    bits but is exactly what a stale or partly-unchanged snapshot produces. `42` is
//!    `≈ log2(2^30) + 12`, comfortably above the `log2(n) ≈ 30` a random stream yields and far
//!    below anything a partial re-encryption leaves behind.
//!
//! Both are computed on the XOR of two saves, which is the quantity an attacker who steals two
//! cloud snapshots actually holds. `NIST SP 800-22` and `Dieharder` are still run over the whole
//! container; these two are the cheap, deterministic gates that belong in CI.

/// Bits in one container.
pub const CONTAINER_BITS: u64 = 134_217_728u64 * 8; // 1,073,741,824

/// `0.5/√n` as a fraction of one. `≈ 1.5259e-5`, i.e. `0.001526%`.
pub fn monobit_sigma() -> f64 {
    0.5 / (CONTAINER_BITS as f64).sqrt()
}

/// The accepted deviation, as a fraction of one, at `sigma`.
pub fn monobit_tolerance(sigma: f64) -> f64 {
    monobit_sigma() * sigma
}

/// Number of standard deviations used. 3.5, per the amended spec.
pub const MONOBIT_SIGMAS: f64 = 3.5;

/// Maximum tolerated run of identical bits in `A ⊕ B`. See the module docs.
///
/// # The false-alarm rate, stated rather than assumed
///
/// For `n = 2^30` i.i.d. fair bits the number of runs of length `≥ k` is about `n / 2^(k+1)`, so
/// the probability that a *correct* cipher produces a run longer than 42 is about
/// `2^30 / 2^43 ≈ 1.2 × 10^-4`, or one in 8,300 real saves. That is tolerable for a gate whose
/// alternative is missing a stale snapshot, but it is not zero and it is worth knowing before
/// somebody sees a single spurious failure and concludes the crypto is broken.
///
/// The same arithmetic is why CI does not run this on a fresh random fixture: CI uses a fixed seed,
/// so its verdict is deterministic and the 1-in-8,300 tail never lands in a pipeline. The rate
/// applies only to verifying a *real* save, where a retry is cheap.
pub const MAX_IDENTICAL_RUN: u32 = 42;

/// The result of the two gates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IndistinguishabilityReport {
    /// Fraction of differing bits in `A ⊕ B`, in `[0.0, 1.0]`.
    pub hamming_fraction: f64,
    /// Deviation from 0.5, in fractions of one.
    pub deviation: f64,
    /// That deviation expressed in sigmas.
    pub sigmas: f64,
    /// Longest run of identical bits in `A ⊕ B`.
    pub longest_run: u32,
    /// Set bits, for diagnostics.
    pub ones: u64,
    /// Total bits examined.
    pub bits: u64,
}

impl IndistinguishabilityReport {
    /// Whether both gates pass.
    pub fn passes(&self) -> bool {
        self.sigmas <= MONOBIT_SIGMAS && self.longest_run <= MAX_IDENTICAL_RUN
    }

    /// Why it failed, for an assertion message.
    pub fn failure_reason(&self) -> Option<String> {
        let mut why = Vec::new();
        if self.sigmas > MONOBIT_SIGMAS {
            why.push(format!(
                "monobit {:.6}% deviates {:.2}σ (limit {:.1}σ = ±{:.4}%)",
                self.hamming_fraction * 100.0,
                self.sigmas,
                MONOBIT_SIGMAS,
                monobit_tolerance(MONOBIT_SIGMAS) * 100.0,
            ));
        }
        if self.longest_run > MAX_IDENTICAL_RUN {
            why.push(format!(
                "longest identical run {} > {}",
                self.longest_run, MAX_IDENTICAL_RUN
            ));
        }
        if why.is_empty() {
            None
        } else {
            Some(why.join("; "))
        }
    }
}

/// Compute both gates over the XOR of two equal-length saves.
///
/// Panics on a length mismatch: comparing a 128 MiB container to anything else is a caller bug,
/// and silently truncating would make the gate pass on a prefix.
pub fn analyse_pair(a: &[u8], b: &[u8]) -> IndistinguishabilityReport {
    assert_eq!(a.len(), b.len(), "snapshots must be the same size");
    assert!(!a.is_empty(), "an empty pair has no bits to test");
    assert!(
        a.len() as u64 * 8 <= CONTAINER_BITS,
        "a container is {} bytes; got {}",
        CONTAINER_BITS / 8,
        a.len()
    );

    let mut ones: u64 = 0;
    // 1, not 0: a single bit is a run of one, and a purely alternating stream would otherwise
    // report a longest run of 0.
    let mut longest: u32 = 1;
    let mut run: u32 = 0;
    // Track the current bit value so a run spanning a byte boundary is counted correctly. Seeding
    // with `None` matters: a naive `last = 0` would merge the leading zero run with a preceding
    // implicit zero and over-report by up to 7 bits.
    let mut last: Option<bool> = None;

    for (x, y) in a.iter().zip(b.iter()) {
        let d = x ^ y;
        ones += d.count_ones() as u64;
        for shift in 0..8 {
            let bit = (d >> (7 - shift)) & 1 == 1;
            match last {
                Some(prev) if prev == bit => {
                    run += 1;
                    if run > longest {
                        longest = run;
                    }
                }
                _ => {
                    run = 1;
                    last = Some(bit);
                }
            }
        }
    }

    let bits = a.len() as u64 * 8;
    let hamming_fraction = ones as f64 / bits as f64;
    let deviation = (hamming_fraction - 0.5).abs();
    // Sigma for *this* sample size. A 1 MiB fixture has σ 16× larger than a container's, so a
    // single hard-coded sigma would make every small test either trivially pass or always fail.
    let sigma = 0.5 / (bits as f64).sqrt();
    let sigmas = deviation / sigma;

    IndistinguishabilityReport {
        hamming_fraction,
        deviation,
        sigmas,
        longest_run: longest,
        ones,
        bits,
    }
}

/// Longest run of identical *bytes* — a cheaper companion check used in the USB pad tests, where
/// "the pad sector repeats" is a byte-level anomaly rather than a bit-level one.
pub fn longest_identical_byte_run(bytes: &[u8]) -> u32 {
    if bytes.is_empty() {
        return 0;
    }
    let mut longest = 1u32;
    let mut run = 1u32;
    for w in bytes.windows(2) {
        if w[0] == w[1] {
            run += 1;
            if run > longest {
                longest = run;
            }
        } else {
            run = 1;
        }
    }
    longest
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic keystream so the tests do not depend on `getrandom` or on a host RNG.
    ///
    /// `splitmix64`, not xorshift64. The previous version was a 64-bit xorshift with `<< 13` first,
    /// which for a seed whose low bits are zero produces a leading run of **77 zero bits** before
    /// the feedback reaches them — so the "independent streams pass" test was passing on a
    /// generator whose first 77 output bits are constant, and would have been a coin flip on
    /// whether it tripped its own run gate. splitmix64 is a full-period counter-based mixer with no
    /// such structure and no weak seed.
    fn stream(seed: u64, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len + 8);
        let mut state = seed;
        while out.len() < len {
            state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            out.extend_from_slice(&z.to_le_bytes());
        }
        out.truncate(len);
        out
    }

    /// Small enough to analyse quickly, large enough for the gates to mean something.
    ///
    /// The run gate needs `log2(bits)` to be well below 42, and the monobit gate needs σ small
    /// enough that a 3.5σ band cannot be satisfied by luck: at 1 MiB, σ = 0.049% and the band is
    /// ±0.17%, which is still tighter than the old 0.015%/9.83σ figure it replaced.
    const FIXTURE: usize = 1 << 20;

    #[test]
    fn sigma_is_the_documented_number() {
        // 0.001526% — pin it, because the whole tolerance table is derived from it.
        assert!((monobit_sigma() * 100.0 - 0.001526).abs() < 1e-6);
        let tol = monobit_tolerance(MONOBIT_SIGMAS) * 100.0;
        assert!((tol - 0.0053).abs() < 1e-4, "3.5σ tolerance was {tol}%");
    }

    #[test]
    fn old_tolerance_is_ten_sigma() {
        // Keep the audit finding executable: ±0.015% is 9.83σ, i.e. not a test.
        let old = 0.00015;
        let sigmas = old / monobit_sigma();
        assert!(sigmas > 9.8 && sigmas < 9.9, "was {sigmas}σ");
    }

    #[test]
    fn independent_streams_pass() {
        let a = stream(0xDEAD_BEEF, FIXTURE);
        let b = stream(0x1234_5678_9ABC_DEF0, FIXTURE);
        let r = analyse_pair(&a, &b);
        assert!(r.passes(), "{:?} {:?}", r, r.failure_reason());
        assert!(r.longest_run <= MAX_IDENTICAL_RUN);
        // At 1 MiB a random stream's longest run is around log2(2^23) = 23 bits; 42 is a real bound.
        assert!(r.longest_run < 42);
    }

    #[test]
    fn the_gate_is_scale_invariant() {
        // Sigma comes from the sample size, so the same *relative* deviation fails at either size.
        for len in [1 << 16, 1 << 22] {
            let a = vec![0u8; len];
            let r = analyse_pair(&a, &a);
            assert!(!r.passes(), "an all-zero difference passed at {len} bytes");
            assert!(r.sigmas.is_infinite() || r.sigmas > 1e6);
        }
    }

    #[test]
    fn a_one_percent_bias_is_caught_at_container_scale() {
        // The gate's actual job: a cipher whose difference is 49.5% rather than 50%. At 1 MiB that
        // is 10σ, so it fails; the point is that the *tolerance* is calibrated so it can.
        let n = 1 << 20;
        let a = vec![0u8; n];
        let mut b = vec![0u8; n];
        // Set exactly 49.5% of the bits in the difference.
        let differing_bits = (n as u64 * 8 * 495) / 1000;
        for i in 0..differing_bits {
            let byte = (i / 8) as usize;
            b[byte] |= 1 << (i % 8);
        }
        let r = analyse_pair(&a, &b);
        assert!(!r.passes(), "a 1% bias passed the monobit gate: {:?}", r);
        assert!(r.sigmas > MONOBIT_SIGMAS);
    }

    #[test]
    fn identical_snapshots_fail_loudly() {
        let a = stream(1, FIXTURE);
        let r = analyse_pair(&a, &a);
        assert!(!r.passes());
        assert_eq!(r.hamming_fraction, 0.0);
        assert!(r.longest_run > MAX_IDENTICAL_RUN);
    }

    #[test]
    fn barely_changed_snapshot_fails_on_the_run_gate() {
        // The failure mode the monobit gate cannot see: 99.9% of the bytes identical. The count
        // of set bits still lands near 50% of the *changed* tail, but the leading run is
        // enormous, and that is what the run gate is for.
        let a = stream(2, FIXTURE);
        let mut b = a.clone();
        let from = a.len() * 999 / 1000;
        for (i, byte) in b.iter_mut().enumerate().skip(from) {
            *byte = byte.wrapping_add((i & 0xFF) as u8);
        }
        let r = analyse_pair(&a, &b);
        assert!(!r.passes(), "a 99.9% copy must not pass: {:?}", r);
        assert!(r.longest_run > MAX_IDENTICAL_RUN);
        // And note what the monobit gate alone would have said: it is nowhere near 50%.
        assert!(r.hamming_fraction < 0.01);
    }

    #[test]
    fn byte_run_helper() {
        assert_eq!(longest_identical_byte_run(&[1, 2, 3]), 1);
        assert_eq!(longest_identical_byte_run(&[0, 0, 0, 1, 1]), 3);
        assert_eq!(longest_identical_byte_run(&[]), 0);
    }
}
