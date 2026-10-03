//! Split thresholds, derived from the M0 and M1b spikes.
//!
//! These are the numbers that keep a 2000-page document responsive, and they
//! come from measurement rather than intuition:
//!
//! | threshold | value | source |
//! |---|---|---|
//! | words per section | 1500 | M0 rendering knee — window slide cost roughly triples between 1500 and 6000 words/section |
//! | marks per section | 3000 | M1b — styled read is linear in marks and crosses 5ms at ~5,500; 3000 leaves headroom for a formatting burst |
//! | styled read budget | 5ms | one styled read per render of the focused section, so it is a per-frame cost |
//!
//! Both limits are needed because they bound different things. Word count bounds
//! DOM and layout cost; mark count bounds CRDT read cost. A short but heavily
//! formatted passage hits the mark ceiling long before the word ceiling.

/// Soft maximum words per section (M0). Exceeding this asks for a split.
pub const MAX_WORDS_PER_SECTION: u32 = 1500;

/// Soft maximum marks per section (M1b). Roughly half the observed 5,500-mark
/// budget, leaving room for a burst of formatting before the next write.
pub const MAX_MARKS_PER_SECTION: u32 = 3000;

/// Per-frame budget for a styled read of the focused section (M1b).
pub const STYLED_READ_BUDGET_MS: f64 = 5.0;

/// Measured styled-read cost per 1000 marks, from the M1b linear fit
/// (`read_ms = 0.000921 * marks - 0.0548`). Used to project the cost of a
/// section before it is rendered, so the splitter can reason about a section it
/// has not yet read.
#[derive(Debug, Clone, Copy)]
pub struct MarkCostModel {
    /// Slope, milliseconds per mark.
    pub per_mark_ms: f64,
    /// Intercept, milliseconds.
    pub intercept_ms: f64,
}

impl Default for MarkCostModel {
    fn default() -> Self {
        Self { per_mark_ms: 0.000_921, intercept_ms: -0.0548 }
    }
}

impl MarkCostModel {
    /// Project the styled-read cost of a section with `marks` marks.
    pub fn predict(&self, marks: u32) -> f64 {
        (self.per_mark_ms * marks as f64 + self.intercept_ms).max(0.0)
    }

    /// The mark count at which the predicted cost reaches the budget.
    pub fn marks_at_budget(&self) -> u32 {
        let n = (STYLED_READ_BUDGET_MS - self.intercept_ms) / self.per_mark_ms;
        if n <= 0.0 {
            0
        } else {
            n as u32
        }
    }
}

/// Why a section should be split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitReason {
    /// Over the word limit, so rendering it is expensive.
    TooManyWords { words: u32 },
    /// Over the mark limit, so reading it from the CRDT is expensive.
    TooManyMarks { marks: u32 },
    /// A single block is larger than a whole section may be, so it can never be
    /// split. Tracked separately so the splitter reports it instead of looping.
    Unsplitable { words: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectionMetrics {
    pub words: u32,
    pub marks: u32,
    pub chars: u32,
}

impl SectionMetrics {
    pub fn new(words: u32, marks: u32, chars: u32) -> Self {
        Self { words, marks, chars }
    }
}

/// Should this section be split, and why?
pub fn should_split(m: SectionMetrics) -> Option<SplitReason> {
    if m.words > MAX_WORDS_PER_SECTION {
        Some(SplitReason::TooManyWords { words: m.words })
    } else if m.marks > MAX_MARKS_PER_SECTION {
        Some(SplitReason::TooManyMarks { marks: m.marks })
    } else {
        None
    }
}

/// Projected styled-read cost, for logging and for the render path's own
/// budget check.
pub fn projected_styled_read_ms(marks: u32) -> f64 {
    MarkCostModel::default().predict(marks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_thresholds_match_the_measurements() {
        // These are load-bearing. If a spike result changes, this fails and
        // forces a deliberate update rather than a silent drift.
        assert_eq!(MAX_WORDS_PER_SECTION, 1500, "M0 rendering knee");
        assert_eq!(MAX_MARKS_PER_SECTION, 3000, "M1b mark ceiling");
    }

    #[test]
    fn under_both_limits_is_not_split() {
        assert_eq!(should_split(SectionMetrics::new(1400, 2900, 8000)), None);
    }

    #[test]
    fn word_limit_triggers() {
        assert_eq!(
            should_split(SectionMetrics::new(1501, 10, 9000)),
            Some(SplitReason::TooManyWords { words: 1501 })
        );
    }

    #[test]
    fn mark_limit_triggers_independently() {
        // The case 2000.md misses entirely: a short, heavily formatted section.
        assert_eq!(
            should_split(SectionMetrics::new(200, 3001, 1200)),
            Some(SplitReason::TooManyMarks { marks: 3001 })
        );
    }

    #[test]
    fn mark_limit_is_checked_after_words() {
        // Both exceeded: report the word overflow, since splitting on words
        // addresses both.
        assert!(matches!(
            should_split(SectionMetrics::new(5000, 5000, 30000)),
            Some(SplitReason::TooManyWords { .. })
        ));
    }

    #[test]
    fn cost_model_reproduces_the_m1b_fit() {
        let m = MarkCostModel::default();
        // Values from the M1b projection table.
        assert!((m.predict(5_000) - 4.55).abs() < 0.05, "got {}", m.predict(5_000));
        assert!((m.predict(10_000) - 9.15).abs() < 0.05, "got {}", m.predict(10_000));
        assert!((m.predict(25_000) - 22.96).abs() < 0.10, "got {}", m.predict(25_000));
    }

    #[test]
    fn budget_mark_count_is_about_5500() {
        let n = MarkCostModel::default().marks_at_budget();
        assert!((5_000..6_000).contains(&n), "marks at 5ms budget = {n}");
    }

    #[test]
    fn the_two_limits_bound_different_costs() {
        // A section can be inside the render budget and outside the CRDT
        // budget. This is the whole reason both limits exist: word count bounds
        // DOM/layout cost, mark count bounds CRDT read cost, and neither
        // substitutes for the other.
        //
        // 500 words is comfortably under the 1500-word limit, so only the mark
        // limit can fire.
        let m = SectionMetrics::new(500, 4_000, 3_000);
        assert!(m.words < MAX_WORDS_PER_SECTION, "precondition: under word limit");
        assert_eq!(
            should_split(m),
            Some(SplitReason::TooManyMarks { marks: 4_000 }),
            "a short but heavily formatted section must still split"
        );
        // And at 4000 marks the projected read is approaching the budget, which
        // is what makes the split necessary rather than merely tidy.
        assert!(
            projected_styled_read_ms(4_000) > STYLED_READ_BUDGET_MS / 2.0,
            "4000 marks should be within striking distance of the budget, got {:.2}ms",
            projected_styled_read_ms(4_000)
        );
    }

    #[test]
    fn split_ceiling_stays_inside_the_budget() {
        // The configured ceiling must leave headroom under the measured budget.
        assert!(
            projected_styled_read_ms(MAX_MARKS_PER_SECTION) < STYLED_READ_BUDGET_MS,
            "ceiling {} marks projects to {:.2}ms, over the {:.0}ms budget",
            MAX_MARKS_PER_SECTION,
            projected_styled_read_ms(MAX_MARKS_PER_SECTION),
            STYLED_READ_BUDGET_MS
        );
    }
}
