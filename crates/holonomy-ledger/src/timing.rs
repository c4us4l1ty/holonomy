//! Minimal timing measurement for gates.
//!
//! Deliberately tiny and std-only: gate harnesses in later blocks need a way to turn Laws
//! L-3/L-4/L-7 (seconds, milliseconds) from `Spec` into `Gated` without importing a benchmark
//! framework whose own allocations and threads would blur the thing being measured. What a
//! gate needs is a distribution, not a mean, so [`measure`] returns percentiles.

use std::time::{Duration, Instant};

/// A measured distribution over `samples` timed runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// Number of timed runs.
    pub samples: usize,
    /// Arithmetic mean.
    pub mean: Duration,
    /// Median (p50).
    pub p50: Duration,
    /// 99th percentile (nearest-rank), the number the latency laws bind to.
    pub p99: Duration,
    /// Slowest run.
    pub max: Duration,
}

/// Run `f` `samples` times, timing each run, and return the distribution.
///
/// The closure is timed individually; its return values are dropped unmeasured. Warm-up is the
/// caller's responsibility (cold-cache effects belong in the numbers for L-3 and must not be
/// warmed away there).
///
/// # Panics
///
/// Panics if `samples` is zero.
pub fn measure<T>(samples: usize, mut f: impl FnMut() -> T) -> Summary {
    assert!(
        samples > 0,
        "a distribution over zero runs measures nothing"
    );
    let mut runs = Vec::with_capacity(samples);
    for _ in 0..samples {
        let start = Instant::now();
        std::hint::black_box(f());
        runs.push(start.elapsed());
    }
    summarise(runs)
}

/// Summarise samples the caller timed itself.
///
/// This exists for the measurements that cannot be made in one loop: Block 4's second gate
/// compares a keystroke at the start of a document with one at its end, and the honest way to
/// compare two curves is to measure them **alternately**, in blocks, so that a machine drifting
/// under load moves both of them together instead of being blamed on whichever ran second. The
/// caller owns the timing loop; the ledger owns the distribution, so there is one definition of a
/// percentile in the workspace rather than one per harness.
///
/// # Panics
///
/// Panics if `runs` is empty.
pub fn summarise(mut runs: Vec<Duration>) -> Summary {
    assert!(
        !runs.is_empty(),
        "a distribution over zero runs measures nothing"
    );
    runs.sort_unstable();
    let total: Duration = runs.iter().sum();
    let count = runs.len();
    Summary {
        samples: count,
        mean: total / (count as u32),
        p50: runs[count / 2],
        p99: runs[nearest_rank(count, 99)],
        max: runs[count - 1],
    }
}

/// Nearest-rank percentile index for `p` percent over `count` sorted samples.
fn nearest_rank(count: usize, p: usize) -> usize {
    let rank = (p * count).div_ceil(100);
    if rank == 0 {
        0
    } else if rank > count {
        count - 1
    } else {
        rank - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distribution_is_sane() {
        let s = measure(64, || std::hint::black_box(2 + 2));
        assert_eq!(s.samples, 64);
        assert!(s.p50 <= s.p99);
        assert!(s.p99 <= s.max);
        assert!(s.mean <= s.max);
    }

    #[test]
    fn summarising_collected_samples_agrees_with_measuring_them() {
        let samples = vec![
            Duration::from_nanos(30),
            Duration::from_nanos(10),
            Duration::from_nanos(20),
            Duration::from_nanos(40),
        ];
        let summary = summarise(samples);
        assert_eq!(summary.samples, 4);
        // With an even number of samples p50 is the **upper** of the two middle ones, exactly as
        // `measure` has always taken it: sorted, that is `runs[count / 2]`, not the mean of the
        // middle pair. Pinned here because a difference between the two entry points would make
        // one harness's percentile incomparable with another's.
        assert_eq!(summary.p50, Duration::from_nanos(30));
        assert_eq!(summary.max, Duration::from_nanos(40));
        assert_eq!(summary.mean, Duration::from_nanos(25));
    }

    #[test]
    fn zero_samples_are_refused() {
        let result = std::panic::catch_unwind(|| summarise(Vec::new()));
        assert!(result.is_err());
    }

    #[test]
    fn zero_runs_are_refused() {
        let result = std::panic::catch_unwind(|| measure(0, || std::hint::black_box(1 + 1)));
        assert!(result.is_err());
    }
}
