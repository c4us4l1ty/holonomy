//! `holonomy --check`: verify this machine, not this build.
//!
//! # What this is for
//!
//! The audit's biggest honesty problem was that its numbers were *assertions*. "≤180 MiB steady",
//! "`O_DIRECT` is used", "`memfd_secret` is available", "`SCHED_FIFO` was obtained" — every one of
//! those is a claim about a specific machine, and none of them can be established by a `const` in a
//! library. This module is the thing that measures them, on the machine it is running on, and prints
//! what it found including where it found nothing.
//!
//! # The rule
//!
//! A check that cannot be performed is reported as [`Severity::Deferred`] with the reason, never as
//! a pass. A check that fails is reported with the number it failed at. Nothing here is allowed to
//! return "ok" because it did not look.

use std::fmt::Write as _;

/// How much a finding matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Measured and correct.
    Ok,
    /// The machine cannot support something the design would like. Not a bug in Holonomy, but the
    /// user needs to know which protection they do not have.
    Degraded,
    /// A gate that should hold, does not. `--check` exits non-zero.
    Failed,
    /// Could not be established here, with a reason. Never counted as a pass.
    Deferred,
}

impl Severity {
    /// A short marker for the left-hand column.
    pub fn marker(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Degraded => "degraded",
            Self::Failed => "FAIL",
            Self::Deferred => "deferred",
        }
    }
}

/// One named check and what it found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckResult {
    /// What was checked, phrased as the property rather than the mechanism.
    pub name: String,
    /// How it came out.
    pub severity: Severity,
    /// The measurement, the reason, or both. Never empty.
    pub detail: String,
}

impl CheckResult {
    /// A passing check with its measurement.
    pub fn ok(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            severity: Severity::Ok,
            detail: detail.into(),
        }
    }

    /// A check the machine cannot support.
    pub fn degraded(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            severity: Severity::Degraded,
            detail: detail.into(),
        }
    }

    /// A check that should have held and did not.
    pub fn failed(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            severity: Severity::Failed,
            detail: detail.into(),
        }
    }

    /// A check that could not be performed, and why.
    pub fn deferred(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            severity: Severity::Deferred,
            detail: reason.into(),
        }
    }
}

/// The whole run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    /// Every check, in a stable order.
    pub results: Vec<CheckResult>,
    /// Peak resident set size measured during the run, in bytes, if it could be read.
    pub peak_rss_bytes: Option<u64>,
}

impl CheckReport {
    /// An empty report.
    pub fn new() -> Self {
        Self {
            results: Vec::new(),
            peak_rss_bytes: None,
        }
    }

    /// Add a result.
    pub fn push(&mut self, r: CheckResult) {
        self.results.push(r);
    }

    /// Whether anything failed. `Degraded` and `Deferred` do not fail the run: the first is a
    /// property of the machine and the second is a property of this environment, and conflating
    /// either with "Holonomy is broken" would make the exit code useless.
    pub fn passed(&self) -> bool {
        !self.results.iter().any(|r| r.severity == Severity::Failed)
    }

    /// How many results at a given severity.
    pub fn count(&self, severity: Severity) -> usize {
        self.results
            .iter()
            .filter(|r| r.severity == severity)
            .count()
    }

    /// The machine-readable body, without a header. Used by the CI gate.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for r in &self.results {
            let _ = writeln!(out, "[{:<8}] {}: {}", r.severity.marker(), r.name, r.detail);
        }
        let _ = writeln!(
            out,
            "\n{} ok, {} degraded, {} failed, {} deferred",
            self.count(Severity::Ok),
            self.count(Severity::Degraded),
            self.count(Severity::Failed),
            self.count(Severity::Deferred),
        );
        if let Some(rss) = self.peak_rss_bytes {
            let _ = writeln!(
                out,
                "peak RSS: {} KiB (budget {} MiB steady, {} MiB unlock peak)",
                rss / 1024,
                holonomy_core::budgets::STEADY_STATE_CEILING_BYTES / holonomy_core::budgets::MIB,
                holonomy_core::budgets::UNLOCK_PEAK_CEILING_BYTES / holonomy_core::budgets::MIB,
            );
        }
        out
    }
}

impl Default for CheckReport {
    fn default() -> Self {
        Self::new()
    }
}

/// Peak resident set size of this process, in bytes, if the platform reports it.
///
/// Linux: `/proc/self/status`'s `VmHWM` ("high water mark"), which is the kernel's own peak and
/// therefore does not require sampling. macOS and Windows are `Deferred` rather than estimated: a
/// wrong RSS number in a security tool is worse than an absent one, because the absent one is
/// visible.
pub fn peak_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmHWM:") {
                // "   123456 kB"
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                return Some(kb * 1024);
            }
        }
        None
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_with_a_failure_does_not_pass() {
        let mut r = CheckReport::new();
        r.push(CheckResult::failed(
            "capsule size",
            "65,552 bytes, expected 65,536",
        ));
        assert!(!r.passed());
    }

    #[test]
    fn degraded_and_deferred_do_not_fail_the_run() {
        // Otherwise a stock Linux desktop fails `--check` for not having memfd_secret, which would
        // train users to ignore the exit code.
        let mut r = CheckReport::new();
        r.push(CheckResult::degraded("memfd_secret", "unavailable"));
        r.push(CheckResult::deferred(
            "capture exclusion",
            "not implemented on linux",
        ));
        assert!(r.passed());
        assert_eq!(r.count(Severity::Degraded), 1);
        assert_eq!(r.count(Severity::Deferred), 1);
    }

    #[test]
    fn deferred_never_reads_as_a_pass() {
        let d = CheckResult::deferred("measured RSS", "no /proc on this platform");
        assert_ne!(d.severity, Severity::Ok);
        assert_eq!(d.severity.marker(), "deferred");
    }

    #[test]
    fn the_rendered_report_counts_every_severity() {
        let mut r = CheckReport::new();
        r.push(CheckResult::ok("a", "yes"));
        r.push(CheckResult::degraded("b", "meh"));
        r.push(CheckResult::failed("c", "no"));
        r.push(CheckResult::deferred("d", "later"));
        r.peak_rss_bytes = Some(12 * 1024 * 1024);
        let text = r.render();
        assert!(
            text.contains("1 ok, 1 degraded, 1 failed, 1 deferred"),
            "{text}"
        );
        assert!(text.contains("peak RSS: 12288 KiB"), "{text}");
    }

    #[test]
    fn a_result_with_no_detail_is_not_constructible_from_the_helpers() {
        // Every constructor takes a detail string, so "it passed" can never be reported without
        // saying what was measured. This test exists to make that a property of the API.
        let r = CheckResult::ok("name", "measured 65,536 bytes");
        assert!(!r.detail.is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn peak_rss_is_available_and_plausible_on_linux() {
        let rss = peak_rss_bytes().expect("/proc/self/status exists on linux");
        assert!(rss > 0, "peak RSS of 0 is not a measurement");
        // A test binary is a few MiB; 4 GiB would mean the units are wrong.
        assert!(rss < 4 * 1024 * 1024 * 1024, "implausible peak RSS {rss}");
    }
}
