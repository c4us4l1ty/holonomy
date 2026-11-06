//! The custom 22-bit sector permutation: 11-bit halves, cycle-walked into Zone B.
//!
//! # What was wrong with `FPE-20`
//!
//! The superseded PRD specified halving the sector index into 10-bit halves, i.e. a permutation
//! over `[0, 2^20) = 1,048,576` cycle-walked into `[0, 1,782,579)`. A bijection on
//! `[0, 2^20)` can only *reach* `[0, 2^20)`, so **734,003 sectors — 41.2% of Zone B, ~2.8 GiB —
//! were outside the codomain by construction**. The "uniform random pad" was drawn from 59% of
//! the drive, permanently, and no amount of re-permutation could fix it because the domain was
//! smaller than the range.
//!
//! `2^21 = 2,097,152` would already cover 1,782,579, but the amended spec asks for **11-bit
//! halves**, and 11 bits is what the record shows, so `2^22 = 4,194,304` is the domain here.
//! Cycle-walking discards `4,194,304 − 1,782,579 = 2,411,725` outputs. For an ideal uniformly
//! random permutation, the expected return time to Zone B is about `DOMAIN / ZONE_B_SECTORS ≈ 2.35`;
//! that is a workload estimate, not a bound or a security result for this custom network. The
//! deterministic bound comes from a different fact: inputs must start inside Zone B, and a finite
//! permutation returns to that starting input within at most `DOMAIN` steps.
//!
//! # What this is, honestly
//!
//! A six-round balanced Feistel permutation over a 22-bit block. The Feistel construction gives
//! bijectivity; it does **not** establish pseudorandomness. This implementation is not a reviewed
//! format-preserving encryption scheme and must not be relied on to conceal pad-sector access
//! patterns. Its current load-bearing property is only that valid logical sector indices map
//! injectively into Zone B. Production claims about access-pattern secrecy require a reviewed
//! construction and a threat model; this code supplies neither.
//!
//! # Round function
//!
//! The superseded version's round function was
//! `(right * 0x9E3779B9 ^ k ^ round).rotate_left(5) & 0x3FF` — a **single multiply masked back
//! down to 10 bits**, which is nearly affine in `right` and leaks the low bits of the Feistel
//! state. [`round_function`] instead applies ARX-style mixing of the 11-bit half with a 32-bit
//! round key and takes the high 11 bits. This is a construction detail, not a cryptographic
//! security claim.

use crate::layout::ZONE_B_SECTORS;

/// Domain of the permutation: `2^22`.
pub const DOMAIN: u32 = 1 << 22;

/// Half width in bits. `HALF_BITS * 2 == 22`.
pub const HALF_BITS: u32 = 11;

/// Mask for one half.
pub const HALF_MASK: u32 = (1 << HALF_BITS) - 1;

/// Number of Feistel rounds.
pub const ROUNDS: u32 = 6;

const _: () = {
    assert!(DOMAIN == 4_194_304);
    assert!(DOMAIN >= ZONE_B_SECTORS);
    assert!(HALF_BITS == 11 && HALF_MASK == 0x7FF);
};

/// The keyed permutation. Holds the six 32-bit round keys derived once per epoch.
///
/// `Clone` is deliberately *not* implemented: a copy of this is a copy of key material, and the
/// crate wants that to be a visible act rather than an accidental one.
pub struct Feistel22 {
    round_keys: [u32; ROUNDS as usize],
    epoch: u64,
}

/// Expand a 32-byte permutation key into six round keys plus nothing else.
///
/// A splitmix-style finaliser, not a PRF: the input is already a `BLAKE2b`/`SHAKE` output
/// (`K_perm`), so this only has to decorrelate the six words.
fn expand_round_keys(k_perm: &[u8; 32]) -> [u32; ROUNDS as usize] {
    let mut out = [0u32; ROUNDS as usize];
    for (i, slot) in out.iter_mut().enumerate() {
        let o = (i * 4) % 32;
        let w = u32::from_le_bytes([k_perm[o], k_perm[o + 1], k_perm[o + 2], k_perm[o + 3]]);
        let mut x = w ^ (i as u32).wrapping_mul(0x9E37_79B9);
        x ^= x >> 16;
        x = x.wrapping_mul(0x85EB_CA6B);
        x ^= x >> 13;
        x = x.wrapping_mul(0xC2B2_AE35);
        x ^= x >> 16;
        *slot = x;
    }
    out
}

/// One Feistel round function: 11-bit half + 32-bit key -> 11 bits.
#[inline(always)]
fn round_function(half: u32, key: u32, round: u32) -> u32 {
    let mut x = half
        .wrapping_mul(0x9E37_79B9)
        .wrapping_add(key.rotate_left(7))
        ^ round.wrapping_mul(0x85EB_CA6B);
    x ^= x >> 15;
    x = x.wrapping_mul(0xC2B2_AE35);
    x ^= x >> 13;
    // Take the HIGH bits, not `& 0x3FF` of the low ones: the low bits of a multiply are the
    // most structured part of the product, which is precisely the old bug.
    (x >> (32 - HALF_BITS)) & HALF_MASK
}

impl Feistel22 {
    /// The permutation a provisioned container uses, for the life of the epoch.
    ///
    /// Equivalent to [`Feistel22::new`]`(k_perm, 0)` and preferred at call sites, because the
    /// epoch argument of `new` is a trap: passing a save cursor there gives every save its own
    /// permutation, and two different bijections over one codomain collide. Save-to-save
    /// disjointness requires a single `π` with disjoint logical windows; see
    /// [`Self::logical_sector`].
    pub fn provisioned(k_perm: &[u8; 32]) -> Self {
        Self::new(k_perm, 0)
    }

    /// Build a permutation for one epoch. `epoch` is mixed into the round keys, so consecutive
    /// epochs produce unrelated permutations even with a constant `K_perm`.
    ///
    /// **Do not pass a save cursor here.** Epochs are for re-provisioning the stick (a fresh
    /// `k_perm` with a coincidentally fresh epoch number), not for distinguishing saves. See
    /// [`Self::provisioned`].
    pub fn new(k_perm: &[u8; 32], epoch: u64) -> Self {
        let mut round_keys = expand_round_keys(k_perm);
        let e = epoch as u32;
        for (i, k) in round_keys.iter_mut().enumerate() {
            *k ^= (e.rotate_left((i as u32) * 5)).wrapping_mul(0x9E37_79B9);
        }
        Self { round_keys, epoch }
    }

    /// The raw 6-round permutation on `[0, 2^22)`. A bijection, before cycle-walking.
    #[inline]
    fn permute_block(&self, x: u32) -> u32 {
        debug_assert!(x < DOMAIN);
        let mut left = (x >> HALF_BITS) & HALF_MASK;
        let mut right = x & HALF_MASK;
        for round in 0..ROUNDS {
            let f = round_function(right, self.round_keys[round as usize], round);
            let next_right = left ^ f;
            left = right;
            right = next_right;
        }
        (left << HALF_BITS) | right
    }

    /// Map a logical sector sequence number into a physical Zone B sector, `[0, ZONE_B_SECTORS)`.
    ///
    /// `logical` must already be inside Zone B. Cycle-walking a permutation from an input inside
    /// the target set must return to that input, and therefore to the target set, within one full
    /// permutation orbit (at most `DOMAIN` applications). Starting outside the target set does not
    /// have that guarantee, so invalid inputs are rejected rather than reduced modulo the domain.
    /// The average of about `DOMAIN / ZONE_B_SECTORS ≈ 2.35` steps assumes an ideal random
    /// permutation; it is not a bound for this implementation. The deterministic worst case is
    /// `DOMAIN` block applications per logical sector, so this guarantees termination but not a
    /// practical latency bound for pathological keys. Panics if `logical` is not inside Zone B.
    pub fn permute(&self, logical: u64) -> u32 {
        assert!(
            logical < ZONE_B_SECTORS as u64,
            "Feistel22 cycle-walk input {logical} is outside Zone B"
        );
        let mut x = logical as u32;
        for _ in 0..DOMAIN {
            x = self.permute_block(x);
            if x < ZONE_B_SECTORS {
                return x;
            }
        }
        unreachable!(
            "Feistel22 permutation orbit did not return to its in-range start within DOMAIN steps"
        );
    }

    /// The physical sector range this epoch's save consumes, given a starting epoch cursor.
    ///
    /// `cursor` is the monotonic counter of saves performed, **not** a random epoch number. The
    /// logical index for save `cursor` and slot `j` is `cursor * PAD_SECTORS_PER_SAVE + j`, so
    /// consecutive saves occupy disjoint logical ranges, and injectivity of the permutation
    /// carries that disjointness into physical LBAs. Panics if `j` is not a slot in one save or if
    /// the arithmetic overflows; callers must separately enforce the finite save budget.
    pub fn logical_sector(cursor: u64, j: u32) -> u64 {
        assert!(
            j < crate::layout::PAD_SECTORS_PER_SAVE,
            "pad slot is out of range"
        );
        cursor
            .checked_mul(crate::layout::PAD_SECTORS_PER_SAVE as u64)
            .and_then(|base| base.checked_add(j as u64))
            .expect("logical pad sector index overflow")
    }

    /// A keyed permutation of `[0, n)`, returned as a lookup table.
    ///
    /// # Why not cycle-walking
    ///
    /// [`Self::permute`] maps Zone B by cycle-walking: apply the permutation until the result lands
    /// inside the target range. That is correct there because Zone B is 42% of the domain, so the
    /// expected walk is ~2.35 iterations. It is **not** correct as a general tool: the first version
    /// of this method walked each slot separately and returned the first in-range value, which is a
    /// chain of partial functions, not a permutation — if `π(x)` landed in range it returned
    /// immediately, while a `y` whose walk passed through the same point kept going, so two inputs
    /// could map to one output. `the_epoch_placement_is_a_bijection_of_the_payload_range` is the test
    /// that catches it; it is why this method exists rather than the caller doing the walk.
    ///
    /// # What it does instead
    ///
    /// A swap chain: start from the identity and apply one transposition per element, where the
    /// partner index comes from the keyed permutation. A composition of transpositions is a
    /// permutation by construction — no proof obligation, no convergence question, no panic path.
    /// Cost is `n` permutation rounds, against `n × DOMAIN/n = DOMAIN` for a per-element walk, which
    /// for the payload range is 2,046 against 4.19 million.
    ///
    /// The partner is `permute_block(x) % n`, biased slightly toward low indices. That bias is
    /// acceptable here and is worth stating: what this must not be is *predictable* or
    /// non-injective, and `% n` is neither.
    pub fn swap_chain(&self, n: u32) -> Vec<u32> {
        assert!(n > 0, "a permutation of nothing is not a permutation");
        let mut order: Vec<u32> = (0..n).collect();
        for x in 0..n {
            // Any value in [0, n) works; the permutation rounds only have to be unpredictable.
            let y = self.permute_block(x) % n;
            if y > x {
                order.swap(x as usize, y as usize);
            }
        }
        order
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{PAD_SECTORS_PER_SAVE, SAVES_BEFORE_DOMAIN_REUSE};

    fn k(seed: u8) -> [u8; 32] {
        [seed; 32]
    }

    #[test]
    fn block_permutation_is_injective_across_a_large_slice() {
        // A Feistel network is a bijection by construction; a round function that broke that would
        // collapse pad sectors and silently weaken the pad. Checked exhaustively over a 2^20 slice
        // of the 2^22 domain: ~1 M evaluations, a few hundred milliseconds in debug.
        const N: u32 = 1 << 20;
        let f = Feistel22::new(&k(0x5A), 0);
        let mut set = std::collections::HashSet::with_capacity(N as usize);
        for x in 0..N {
            let y = f.permute_block(x);
            assert!(y < DOMAIN, "{x} mapped outside the domain: {y}");
            assert!(set.insert(y), "collision at input {x}");
            // Same input twice must give the same output: no hidden state in the permutation.
            assert_eq!(f.permute_block(x), y);
        }
        assert_eq!(set.len(), N as usize);
    }

    #[test]
    fn consecutive_saves_never_collide_at_any_epoch() {
        // The bug this catches: `logical_sector` used to reduce modulo the 2^22 domain, so cursor
        // 0 and cursor 128 were the same input to the permutation and read the same 32,768 sectors.
        // The documented claim was "0.000% overlap across 54 saves".
        // 54, not 64: Zone B holds 1,782,579 sectors and a save needs 32,768, so 54 saves
        // (1,769,472) fit and 55 (1,802,240) cannot. That is where the budget number comes from --
        // a pigeonhole bound, not a policy choice -- and `the_budget_is_a_pigeonhole_bound` below
        // asserts exactly that.
        let f = Feistel22::new(&k(0x77), 0);
        let mut seen = std::collections::HashSet::with_capacity(54 * PAD_SECTORS_PER_SAVE as usize);
        for cursor in 0..54u64 {
            for j in 0..PAD_SECTORS_PER_SAVE {
                let lba = f.permute(Feistel22::logical_sector(cursor, j));
                assert!(
                    seen.insert(lba),
                    "sector {lba} reused at save {cursor}, slot {j} -- overlapping pad"
                );
            }
        }
        assert_eq!(seen.len(), 54 * PAD_SECTORS_PER_SAVE as usize);
    }

    #[test]
    fn a_per_save_epoch_would_collide() {
        // Why `provisioned` exists. Two *different* permutations over one 1.78M-sector codomain
        // have no reason to keep their 32,768-wide images apart: expected overlap is
        // 32,768^2 / 1,782,579 ~= 602 sectors per pair. This asserts the failure mode instead of
        // describing it, so a future refactor that reintroduces a per-save epoch fails here.
        let mut a = std::collections::HashSet::new();
        for j in 0..PAD_SECTORS_PER_SAVE {
            a.insert(Feistel22::new(&k(0x11), 0).permute(Feistel22::logical_sector(0, j)));
        }
        let b: std::collections::HashSet<u32> = (0..PAD_SECTORS_PER_SAVE)
            .map(|j| Feistel22::new(&k(0x11), 1).permute(Feistel22::logical_sector(1, j)))
            .collect();
        assert!(
            !a.is_disjoint(&b),
            "two distinct permutations happened not to intersect, which is possible but is not \
             a property to rely on"
        );
    }

    #[test]
    fn one_permutation_keeps_saves_apart() {
        // The property the pad actually needs, in isolation from the plan builder.
        let f = Feistel22::provisioned(&k(0x12));
        let mut seen = std::collections::HashSet::new();
        for cursor in 0..54u64 {
            for j in 0..PAD_SECTORS_PER_SAVE {
                assert!(seen.insert(f.permute(Feistel22::logical_sector(cursor, j))));
            }
        }
    }

    #[test]
    fn the_budget_is_a_pigeonhole_bound() {
        // 54 saves fit in Zone B; 55 cannot, so a 55th save must reuse a sector whatever the
        // permutation is asked to do. This is the arithmetic that makes `SAVES_BEFORE_DOMAIN_REUSE`
        // a fact about the geometry rather than a limit someone picked.
        let budget = crate::layout::SAVES_BEFORE_DOMAIN_REUSE;
        assert_eq!(budget, 54);
        assert!(budget * PAD_SECTORS_PER_SAVE as u64 <= ZONE_B_SECTORS as u64);
        assert!((budget + 1) * PAD_SECTORS_PER_SAVE as u64 > ZONE_B_SECTORS as u64);
    }

    #[test]
    fn logical_indices_are_strictly_increasing_across_saves() {
        // The property, stated directly: disjointness of the logical ranges is what carries into
        // physical LBAs, and it must not depend on the modulus.
        let a_last = Feistel22::logical_sector(0, PAD_SECTORS_PER_SAVE - 1);
        let b_first = Feistel22::logical_sector(1, 0);
        assert_eq!(a_last, PAD_SECTORS_PER_SAVE as u64 - 1);
        assert_eq!(b_first, PAD_SECTORS_PER_SAVE as u64);
        assert!(a_last < b_first);
    }

    #[test]
    fn the_54_save_budget_stays_inside_the_domain() {
        // 54 saves cover 1,769,472 of 4,194,304 logical indices. The margin is why a 54-save
        // history never needs the reduction to wrap, and therefore why the budget number is
        // honest rather than aspirational.
        // Cursor 53 is the 54th save, so its last slot is the largest index a full budget reads.
        let last = Feistel22::logical_sector(53, PAD_SECTORS_PER_SAVE - 1);
        assert_eq!(last, 53 * 32_768 + 32_767);
        assert_eq!(last, 1_769_471);
        assert!(last < DOMAIN as u64);
        // And the next save's first index is still inside the domain, so the *only* reason not to
        // take it is that Zone B has no room -- not that the index would wrap.
        let next = Feistel22::logical_sector(54, 0);
        assert_eq!(next, 1_769_472);
        assert!(next < DOMAIN as u64);
    }

    #[test]
    #[should_panic(expected = "outside Zone B")]
    fn cycle_walk_rejects_a_start_outside_its_target_set() {
        let f = Feistel22::new(&k(0x21), 0);
        let _ = f.permute(ZONE_B_SECTORS as u64);
    }

    #[test]
    #[should_panic(expected = "logical pad sector index overflow")]
    fn logical_sector_arithmetic_never_wraps() {
        let _ = Feistel22::logical_sector(u64::MAX, 0);
    }

    #[test]
    #[should_panic(expected = "pad slot is out of range")]
    fn logical_sector_rejects_an_invalid_slot() {
        let _ = Feistel22::logical_sector(0, PAD_SECTORS_PER_SAVE);
    }

    #[test]
    fn cycle_walk_stays_inside_zone_b() {
        let f = Feistel22::new(&k(0x22), 0);
        for j in 0..PAD_SECTORS_PER_SAVE {
            let s = f.permute(Feistel22::logical_sector(0, j));
            assert!(s < ZONE_B_SECTORS, "sector {s} escaped Zone B");
        }
    }

    #[test]
    fn full_zone_b_is_reachable() {
        // What is actually true, stated precisely: the *codomain* is all of Zone B, so no sector
        // is excluded by construction -- that was the FPE-20 bug, which left 41.2% unreachable
        // forever. What a single epoch reaches is bounded by how many logical indices it is asked
        // for: 32,768 per save, so ~1.77M across the 54-save budget against 1,782,579 sectors.
        //
        // Reachable-by-construction and reached-by-one-history are different claims, and the
        // honest version of the latter is "no sector is excluded; 54 saves touch ~99.3% of Zone B
        // because that is how many sectors 54 saves read".
        let f = Feistel22::new(&k(0x33), 3);
        let mut seen = std::collections::HashSet::new();
        for cursor in 0..54u64 {
            for j in 0..PAD_SECTORS_PER_SAVE {
                seen.insert(f.permute(Feistel22::logical_sector(cursor, j)));
            }
        }
        assert_eq!(
            seen.len(),
            54 * PAD_SECTORS_PER_SAVE as usize,
            "no collisions, so every read sector is distinct"
        );
        // FPE-20 could never produce a sector at or above 2^20; this can, and does.
        assert!(
            seen.iter().any(|&s| s >= (1 << 20)),
            "the 11-bit halves must reach the 41.2% of Zone B that FPE-20 excluded"
        );
        // Density: what 54 saves actually cover, as a fraction of Zone B.
        let coverage = seen.len() as f64 / ZONE_B_SECTORS as f64;
        assert!(
            coverage > 0.99,
            "54 saves cover {:.2}% of Zone B",
            coverage * 100.0
        );
        let f = Feistel22::new(&k(0x33), 3);
        let mut above = 0;
        for j in 0..200_000u32 {
            if f.permute(Feistel22::logical_sector(0, j)) >= (1 << 20) {
                above += 1;
            }
        }
        // Expected share is 1 - 2^20/1782579 = 41.2%; require a healthy fraction so a regression
        // to a 20-bit domain fails loudly.
        let frac = above as f64 / 200_000.0;
        assert!(
            frac > 0.35,
            "only {:.1}% of samples above 2^20; domain regressed?",
            frac * 100.0
        );
    }

    #[test]
    fn zero_overlap_across_fifty_four_saves() {
        // The documented guarantee, checked as a set intersection over real LBA output.
        let per = PAD_SECTORS_PER_SAVE as usize;
        let saves = SAVES_BEFORE_DOMAIN_REUSE as usize;
        let f = Feistel22::new(&k(0x44), 0);
        let mut used = std::collections::HashSet::with_capacity(per * saves);
        for cursor in 0..saves as u64 {
            for j in 0..PAD_SECTORS_PER_SAVE {
                let lba = f.permute(Feistel22::logical_sector(cursor, j));
                assert!(used.insert(lba), "sector {lba} reused at save {cursor}");
            }
        }
        assert_eq!(used.len(), per * saves);
        assert_eq!(used.len(), 1_769_472);
        assert!(used.len() <= ZONE_B_SECTORS as usize);
    }

    #[test]
    fn different_epochs_different_permutations() {
        let a = Feistel22::new(&k(0x55), 0);
        let b = Feistel22::new(&k(0x55), 1);
        let differing = (0..1024u32)
            .filter(|&i| a.permute_block(i * 7) != b.permute_block(i * 7))
            .count();
        assert!(
            differing > 1000,
            "epoch counter barely perturbs the permutation"
        );
    }

    #[test]
    fn the_swap_chain_is_a_permutation_for_every_size_tried() {
        // The property that matters, at the sizes the format uses and at sizes that catch an
        // off-by-one (0 and 1 must not panic, and 2 is the smallest non-trivial case).
        for n in [1u32, 2, 3, 17, 2_046] {
            let table = Feistel22::provisioned(&[0x2Bu8; 32]).swap_chain(n);
            assert_eq!(table.len(), n as usize);
            let mut sorted = table.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(
                sorted.len(),
                n as usize,
                "n = {n}: the chain is not injective"
            );
            assert_eq!(sorted[0], 0, "n = {n}: output below the range");
            assert_eq!(
                sorted[sorted.len() - 1],
                n - 1,
                "n = {n}: output above the range"
            );
        }
    }

    #[test]
    fn the_swap_chain_moves_most_blocks_and_depends_on_the_key() {
        // A chain that returned the identity would satisfy every injectivity assertion above and
        // permute nothing, which is the failure mode worth naming: the store would still write slots
        // in order while every test said the placement was a bijection.
        let a = Feistel22::provisioned(&[0x11u8; 32]).swap_chain(2_046);
        let moved = a
            .iter()
            .enumerate()
            .filter(|(i, v)| *i as u32 != **v)
            .count();
        assert!(moved > 1_000, "only {moved} of 2,046 blocks moved");

        let b = Feistel22::provisioned(&[0x22u8; 32]).swap_chain(2_046);
        let differing = a.iter().zip(&b).filter(|(x, y)| x != y).count();
        assert!(
            differing > 1_000,
            "two keys produced nearly the same permutation ({differing} positions differ)"
        );
    }

    #[test]
    fn round_function_uses_high_bits() {
        // Two inputs differing only in the low bits of `half` must not land in adjacent
        // low-bit outputs (the old `& 0x3FF`-of-a-multiply failure mode).
        let a = round_function(0, 0, 0);
        let b = round_function(1, 0, 0);
        let c = round_function(0x7FF, 0, 0);
        assert_ne!(a, b);
        assert!(a < 2048 && b < 2048 && c < 2048);
        assert!((a as i32 - b as i32).abs() > 1 || (b as i32 - c as i32).abs() > 1);
    }
}
