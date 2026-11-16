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
const fn nearest_rank(count: usize, p: usize) -> usize {
    let rank = (p * count + 99) / 100;
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
    fn zero_runs_are_refused() {
        let result = std::panic::catch_unwind(|| measure(0, || std::hint::black_box(1 + 1)));
        assert!(result.is_err());
    }
}
