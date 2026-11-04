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
    assert_eq!(
        a.len() as u64 * 8,
        CONTAINER_BITS,
        "not a container-sized pair"
    );

    let mut ones: u64 = 0;
    let mut longest: u32 = 0;
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
    let sigmas = deviation / monobit_sigma();

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
    fn stream(seed: u64, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut x = seed | 1;
        while out.len() < len {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            out.extend_from_slice(&x.to_le_bytes());
        }
        out.truncate(len);
        out
    }

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
        let a = stream(0xDEAD_BEEF, 134_217_728);
        let b = stream(0x1234_5678_9ABC_DEF0, 134_217_728);
        let r = analyse_pair(&a, &b);
        assert!(r.passes(), "{:?} {:?}", r, r.failure_reason());
        assert!(r.longest_run <= MAX_IDENTICAL_RUN);
    }

    #[test]
    fn identical_snapshots_fail_loudly() {
        let a = stream(1, 134_217_728);
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
        let a = stream(2, 134_217_728);
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
