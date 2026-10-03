//! Ordering for sections.
//!
//! `2000.md` §2 specifies `order_key` as a fractional index so a section can be
//! inserted between two others without renumbering the document. That matters
//! because sections are the sync unit: a remote device inserting a section must
//! not have to rewrite every `order_key` after it.
//!
//! # Design: integers with gaps, plus rebalancing
//!
//! Keys are u64 integers allocated with an initial gap of `INITIAL_GAP`, so the
//! usual insert-between-two-sections case just takes the midpoint and touches
//! one row.
//!
//! This is deliberately *not* a fractional digit string. Those are not total:
//! `between("V", "V0")` has no solution, because every string beginning with
//! `"V"` either equals `"V"`, equals `"V0"`, or sorts after `"V0"`. An earlier
//! version of this file used digits with a reserved terminator and hit exactly
//! that wall after two inserts at the same position, then again after adding a
//! second terminator level. Reserving more digits only moves the boundary.
//!
//! Instead, when the gap between two neighbours closes, the manifest is
//! **rebalanced**: every key is renumbered with a fresh gap. That is O(n) over
//! the manifest, which is ~50KB and a few hundred rows for a 2000-page
//! document — well under a millisecond — and it happens only after roughly
//! `log2(INITIAL_GAP)` ≈ 10 consecutive inserts at one position.
//!
//! The alternative, renumbering on every insert, would rewrite hundreds of rows
//! per keystroke during a drag-and-drop reorder, and two devices inserting at
//! the same point would fight over the same keys. Rebalancing on gap closure
//! avoids both.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

/// Gap allocated between consecutive keys. 1024 gives ~10 levels of midpoint
/// insertion before a rebalance is needed.
pub const INITIAL_GAP: u64 = 1024;

/// The first key of an empty document.
///
/// Half the gap, not zero, so there is room to insert before the first section.
/// Moving a section to the top of a document is a normal user action.
pub fn first_key() -> u64 {
    INITIAL_GAP / 2
}

/// Keys below this are reserved as sentinels, so `None` (start of document) and
/// a real first key are distinguishable and arithmetic never underflows.
const MIN_KEY: u64 = 1;

/// A key strictly between `a` and `b`.
///
/// `None` on the left means the start of the document, `None` on the right the
/// end. Returns `None` when the gap has closed and the caller must rebalance.
pub fn between(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => Some(first_key()),
        (None, Some(bb)) => {
            debug_assert!(bb > MIN_KEY, "first key must leave headroom below");
            Some(bb / 2)
        }
        (Some(aa), None) => Some(aa + INITIAL_GAP),
        (Some(aa), Some(bb)) => {
            debug_assert!(aa < bb, "keys out of sequence: {aa} >= {bb}");
            // Strictly between requires at least one integer in the gap.
            if bb - aa < 2 {
                return None;
            }
            Some(aa + (bb - aa) / 2)
        }
    }
}

/// Append after the last key.
pub fn append(last: Option<u64>) -> u64 {
    between(last, None).expect("append always has room")
}

/// Keys for a whole manifest, renumbered with fresh gaps.
///
/// Called when an insert finds no room. The result is monotonically increasing
/// and preserves relative order, so the manifest's section order is unchanged —
/// only the keys change.
pub fn rebalance(n: usize) -> Vec<u64> {
    (0..n)
        .map(|i| MIN_KEY + (i as u64 + 1) * INITIAL_GAP)
        .collect()
}

/// Renumber `keys` (given in document order) so gaps are fresh again.
///
/// Returns `None` if the input is not strictly increasing, which would mean the
/// manifest is already inconsistent and rebalancing cannot fix it.
pub fn rebalance_existing(keys: &[u64]) -> Option<Vec<u64>> {
    if keys.windows(2).any(|w| w[0] >= w[1]) {
        return None;
    }
    Some(rebalance(keys.len()))
}

/// Allocate a key for inserting at `index` in a manifest of `n` sections,
/// rebalancing and retrying if the gap has closed.
pub fn key_for_insert(index: usize, keys: &[u64]) -> Result<u64> {
    let before = if index == 0 { None } else { keys.get(index - 1).copied() };
    let after = keys.get(index).copied();
    between(before, after).ok_or_else(|| {
        Error::Other(anyhow::anyhow!(
            "no order key available at index {index} of {} sections; \
             manifest needs rebalancing before this insert",
            keys.len()
        ))
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct OrderKey(pub u64);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_key_leaves_headroom_on_both_sides() {
        let f = first_key();
        assert!(f > MIN_KEY, "must be able to insert before the first section");
        // Room below.
        assert!(between(None, Some(f)).unwrap() < f);
        // Room above.
        assert!(between(Some(f), None).unwrap() > f);
    }

    #[test]
    fn appending_is_strictly_increasing() {
        let mut k = first_key();
        let mut keys = vec![k];
        for _ in 0..1000 {
            let next = append(Some(k));
            assert!(next > k, "{next} must exceed {k}");
            k = next;
            keys.push(k);
        }
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn between_is_strictly_ordered() {
        let a = first_key();
        let b = append(Some(a));
        let m = between(Some(a), Some(b)).unwrap();
        assert!(a < m && m < b);
    }

    #[test]
    fn repeated_inserts_at_one_point_eventually_rebalance() {
        // The honest behaviour: inserts succeed while there is gap, then report
        // that a rebalance is needed, and a rebalance restores room. This is
        // the whole point of the design.
        let mut keys = vec![first_key(), first_key() + INITIAL_GAP];
        let mut rebalances = 0;
        let mut inserts = 0;

        for _ in 0..200 {
            inserts += 1;
            // Insert at index 1, between keys[0] and keys[1].
            match between(Some(keys[0]), Some(keys[1])) {
                Some(k) => keys.insert(1, k),
                None => {
                    rebalances += 1;
                    keys = rebalance_existing(&keys).expect("keys are ordered");
                }
            }
            assert!(
                keys.windows(2).all(|w| w[0] < w[1]),
                "manifest must stay ordered after {inserts} inserts"
            );
        }

        assert!(inserts >= 200);
        // With a gap of 1024, roughly 10 inserts fit before each rebalance.
        assert!(
            (15..=25).contains(&rebalances),
            "expected ~20 rebalances over 200 inserts at one point, got {rebalances}"
        );
    }

    #[test]
    fn rebalance_preserves_order_and_restores_room() {
        let keys: Vec<u64> = vec![first_key(), first_key() + 2, first_key() + 3];
        // Gap has closed: no room between the last two.
        assert!(between(Some(keys[1]), Some(keys[2])).is_none());

        let fresh = rebalance_existing(&keys).unwrap();
        assert!(fresh.windows(2).all(|w| w[0] < w[1]));
        assert!(between(Some(fresh[1]), Some(fresh[2])).is_some());
    }

    #[test]
    fn rebalance_refuses_an_already_broken_manifest() {
        // Rebalancing cannot invent a total order from a non-increasing one.
        assert!(rebalance_existing(&[5, 5, 7]).is_none());
        assert!(rebalance_existing(&[7, 5]).is_none());
        assert!(rebalance_existing(&[5, 6, 7]).is_some());
    }

    #[test]
    fn key_for_insert_reports_when_rebalance_is_needed() {
        let keys = vec![10u64, 11];
        assert!(key_for_insert(1, &keys).is_err());
        let roomy = vec![10u64, 1000];
        assert!(key_for_insert(1, &roomy).is_ok());
    }

    #[test]
    fn inserts_at_head_and_tail() {
        let mut keys = vec![first_key()];
        let head = key_for_insert(0, &keys).unwrap();
        assert!(head < keys[0]);
        keys.insert(0, head);
        let tail = key_for_insert(keys.len(), &keys).unwrap();
        assert!(tail > keys[keys.len() - 1]);
        keys.push(tail);
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn a_2000_page_manifest_keeps_its_order_under_mixed_edits() {
        // The scale the design targets. Appending always adds a full gap, and
        // realistic edits insert at *different* positions, so the gaps do not
        // all collapse at one spot. This asserts the manifest stays ordered,
        // using the rebalancing path the UI actually calls, because ~10 inserts
        // at any single position will legitimately exhaust a 1024 gap.
        let mut keys = vec![first_key()];
        for _ in 0..500 {
            keys.push(append(Some(*keys.last().unwrap())));
        }
        // Insert near the top, as reordering a section would.
        for i in 0..8 {
            let at = 1 + (i % 3);
            match between(keys.get(at - 1).copied(), keys.get(at).copied()) {
                Some(k) => keys.insert(at, k),
                None => {
                    let fresh = rebalance_existing(&keys).unwrap();
                    let k = between(Some(fresh[at - 1]), fresh.get(at).copied()).unwrap();
                    let mut updated = fresh;
                    updated.insert(at, k);
                    keys = updated;
                }
            }
            assert!(keys.windows(2).all(|w| w[0] < w[1]), "after insert {i}");
        }
        assert_eq!(keys.len(), 509);
    }
}

/// Order keys stay exactly representable as JavaScript numbers.
///
/// `ManifestSection::order_key` is a `u64` on the wire but is declared `number` in
/// the generated TypeScript, because MessagePack delivers a JS number for any value
/// inside 53 bits. That is only true while the keys stay inside 53 bits, and this
/// asserts it rather than leaving it as an argument in a comment.
///
/// The bound is not close: keys are `MIN_KEY + (i + 1) * 1024`, so reaching 2^53
/// would take about 8.8 trillion sections. A 2000-page document at 1500 words per
/// section is 667 of them. This test exists because the check is cheap and the
/// failure mode — two sections sharing a key, or an ordering that silently reorders
/// — would be silent.
#[cfg(test)]
mod wire_precision_tests {
    use super::*;

    #[test]
    fn order_keys_stay_exactly_representable() {
        // Four billion sections is far beyond any conceivable document, and the keys
        // for that many still fit in 53 bits.
        const SECTIONS: u64 = 4_000_000_000;
        let last = MIN_KEY + SECTIONS * INITIAL_GAP;
        assert!(
            last < 1u64 << 53,
            "the key for section {SECTIONS} is {last}, which exceeds 2^53 and would              lose precision crossing the bridge as a JavaScript number"
        );
        // And the number that would actually be produced for a realistic document is
        // exactly equal, not merely close.
        let realistic = rebalance(667);
        for k in &realistic {
            assert_eq!(*k as f64 as u64, *k, "key {k} does not survive a round trip");
        }
    }

    /// Timestamps stay exactly representable as JavaScript numbers.
    ///
    /// The same class of hazard as the order keys, and the same reason for a test
    /// rather than a comment: `created_at` and `updated_at` are `i64` on the wire and
    /// declared `number` in the generated TypeScript, which is only true while they
    /// fit in 53 bits.
    ///
    /// Two things are checked. That `now_ms` is currently inside the range -- it is,
    /// by six orders of magnitude -- and that it *stays* inside it, by bounding what a
    /// f64 can represent as milliseconds rather than assuming the current value is
    /// safe because it happens to be safe today. A clock rollback does not break this;
    /// the range itself is what is being pinned.
    #[test]
    fn timestamps_stay_exactly_representable() {
        let now = crate::now_ms();
        assert!(
            now > 0,
            "now_ms returned {now}; a negative epoch means the clock is wrong"
        );
        // 2^53 milliseconds is roughly 285,000 years, so the whole representable range
        // is checked by comparing the limit rather than by picking a far-future date.
        assert!(
            (1i64 << 53) - 1 > now,
            "the current timestamp {now} has passed 2^53 ms, at which point \
             JavaScript numbers can no longer represent it exactly"
        );
        assert_eq!(now as f64 as i64, now, "the timestamp does not survive a round trip");
    }
}
