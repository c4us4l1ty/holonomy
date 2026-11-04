//! The two properties the USB pad claims, as statistical tests rather than as prose.
//!
//! # Why these are separate from the unit tests
//!
//! The unit tests in `holonomy-usb` and `holonomy-core` assert structural facts — a plan has
//! 32,768 sectors, a permutation is injective on a slice. Those are cheap and deterministic. What
//! cannot be shown that way, and is what the checkpoints actually claim, is *distributional*:
//!
//! * **Checkpoint 4a, reachability.** All 1,782,579 sectors of Zone B are in the codomain of the
//!   permutation, so no sector is excluded by construction. `FPE-20` failed this: a bijection on
//!   `[0, 2^20)` can only reach `[0, 2^20)`, leaving 734,003 sectors (41.2%) permanently
//!   unreachable. The test below measures the *density* of observed sectors against the uniformity
//!   a correct construction implies, and asserts the specific sectors above 2^20 appear.
//!
//! * **Checkpoint 4b, non-overlap.** No two of the 54 saves share a sector, and this must hold
//!   against the *whole history*, not pairwise-adjacently.
//!
//! # What is asserted versus what is reported
//!
//! Reachability is asserted with a margin, because it is a property of the construction and a
//! failure means the construction is wrong. Uniformity is *reported* — a chi-square over 1.78M
//! buckets would fail on a correct implementation often enough to be useless as a gate, and a
//! permutation is not required to be statistically uniform, only injective. Writing that
//! distinction down is more useful than a flaky assertion that pretends otherwise.

use std::collections::HashSet;

use holonomy_core::feistel::DOMAIN;
use holonomy_core::layout::{PAD_SECTORS_PER_SAVE, SAVES_BEFORE_DOMAIN_REUSE, ZONE_B_SECTORS};

/// `ZONE_B_SECTORS` is a `u32`; comparisons against lengths and `u64` arithmetic want this form.
const ZONE_B_SECTORS_U64: u64 = ZONE_B_SECTORS as u64;
use holonomy_usb::pad::PadPlan;

fn key(seed: u8) -> [u8; 32] {
    let mut k = [0u8; 32];
    for (i, b) in k.iter_mut().enumerate() {
        *b = seed.wrapping_add(i as u8).wrapping_mul(31);
    }
    k
}

/// Collect every physical sector a full 54-save history reads.
fn full_history(k_perm: &[u8; 32]) -> HashSet<u32> {
    let mut seen = HashSet::with_capacity(54 * PAD_SECTORS_PER_SAVE as usize);
    for cursor in 0..SAVES_BEFORE_DOMAIN_REUSE {
        let plan = PadPlan::for_save(k_perm, cursor);
        for &lba in plan.lbas() {
            assert!(
                seen.insert(lba),
                "sector {lba} read twice: save {cursor} overlaps an earlier save"
            );
        }
    }
    seen
}

#[test]
fn fifty_four_saves_never_touch_the_same_sector() {
    // Across several independent keys, so this is not a lucky key.
    for seed in [0x01u8, 0x5A, 0xA5, 0xFF] {
        let seen = full_history(&key(seed));
        // `SAVES_BEFORE_DOMAIN_REUSE` is a u64 and a HashSet length is usize; compare as u64.
        assert_eq!(
            seen.len() as u64,
            SAVES_BEFORE_DOMAIN_REUSE * PAD_SECTORS_PER_SAVE as u64,
            "seed {seed:#04x}: {} distinct sectors, expected {}",
            seen.len(),
            SAVES_BEFORE_DOMAIN_REUSE * PAD_SECTORS_PER_SAVE as u64
        );
    }
}

#[test]
fn every_sector_read_is_inside_zone_b() {
    // Off-by-one here is a write past the end of the pad and into partition 1's launch stub.
    let seen = full_history(&key(0x22));
    assert!(seen.iter().all(|&lba| (lba as u64) < ZONE_B_SECTORS_U64));
    // And the maximum observed is close to the top, not clustered in the low half — the failure
    // mode FPE-20 had.
    let max = *seen.iter().max().expect("a 54-save history is not empty");
    assert!(
        (max as u64) > ZONE_B_SECTORS_U64 * 3 / 4,
        "highest sector reached is {max} of {ZONE_B_SECTORS}"
    );
}

#[test]
fn sectors_that_fpe_20_could_never_reach_are_reached() {
    // Checkpoint 4a, stated as the specific defect it repairs. A 20-bit permutation cannot produce
    // any sector >= 2^20. 41.2% of Zone B sits above that line.
    let seen = full_history(&key(0x33));
    let above = seen.iter().filter(|&&lba| lba >= (1 << 20)).count();
    let expected_share = (ZONE_B_SECTORS_U64 - (1 << 20)) as f64 / ZONE_B_SECTORS_U64 as f64;
    let observed_share = above as f64 / seen.len() as f64;
    // A permutation is injective, not uniform, but over 1.77M draws from 1.78M sectors the
    // observed share should be very close. Generous band: this is a construction check, not a
    // statistical gate.
    assert!(
        (observed_share - expected_share).abs() < 0.02,
        "observed {:.4} of reads above 2^20, expected about {:.4}",
        observed_share,
        expected_share
    );
    assert!(above > 1_000, "only {above} reads above 2^20");
}

#[test]
fn the_possible_sector_count_is_what_the_geometry_says() {
    // The arithmetic the whole budget rests on, checked against the constants rather than restated.
    assert_eq!(DOMAIN, 4_194_304);
    assert_eq!(ZONE_B_SECTORS_U64, 1_782_579);
    // 1,782,579 x 4,096 = 7,301,443,584 bytes = 6.80 GiB of pad reservoir.
    assert_eq!(ZONE_B_SECTORS_U64 * 4_096, 7_301_443_584);
    // 54 saves fit; 55 do not. Pigeonhole, so this is a fact about the device.
    let per_save = PAD_SECTORS_PER_SAVE as u64;
    assert!(SAVES_BEFORE_DOMAIN_REUSE * per_save <= ZONE_B_SECTORS_U64);
    assert!((SAVES_BEFORE_DOMAIN_REUSE + 1) * per_save > ZONE_B_SECTORS_U64);
}

#[test]
fn the_pad_budget_uses_more_than_half_the_drive() {
    // Reported because the original design's error was a pad that silently used 59% of the drive.
    // 54 saves cover 1,769,472 of 1,782,579 sectors: 99.3%. Anything much lower means the
    // construction is wasting Zone B again.
    let seen = full_history(&key(0x44));
    let coverage = seen.len() as f64 / ZONE_B_SECTORS_U64 as f64;
    assert!(
        coverage > 0.99,
        "54 saves cover {:.2}% of Zone B; the budget is supposed to use almost all of it",
        coverage * 100.0
    );
    println!("54-save coverage: {:.4}% of Zone B", coverage * 100.0);
}

#[test]
fn plans_are_deterministic_for_a_key_and_reproducible() {
    // A save that crashed mid-read has to be able to rebuild the same plan, so the mapping may not
    // depend on anything but (k_perm, cursor).
    let k = key(0x55);
    let a = PadPlan::for_save(&k, 7);
    let b = PadPlan::for_save(&k, 7);
    assert_eq!(a.lbas(), b.lbas());
    // And a different cursor gives a different sector set, or the cursor is not doing anything.
    let c = PadPlan::for_save(&k, 8);
    assert_ne!(a.lbas(), c.lbas());
}

#[test]
fn each_single_save_is_collision_free_and_covers_its_own_window() {
    // The per-save half. A plan with a duplicate would read one sector twice and leave another
    // unread, which weakens the fold without failing anything visible.
    for cursor in [0u64, 1, 26, 53] {
        let plan = PadPlan::for_save(&key(0x66), cursor);
        assert_eq!(plan.len(), PAD_SECTORS_PER_SAVE as usize);
        assert!(
            plan.is_collision_free(),
            "save {cursor} plans a sector twice"
        );
    }
}
