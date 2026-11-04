//! Forward-only duress co-evolution: making `L_1` look edited at every snapshot, forever.
//!
//! # The problem this solves, stated precisely
//!
//! The coercive threat is retrospective: an inspector holds `T_0 … T_n` and makes you unlock one
//! of them with the duress passphrase. Two properties have to hold.
//!
//! 1. **`T_0` cleanliness.** The duress stratum at `T_0` must contain nothing that references
//!    the future — no pre-staged edit queue, no template of edits-to-come. If it does, the whole
//!    scheme is a pointer to the real vault.
//! 2. **Cross-snapshot plausibility.** If `L_1` is byte-identical in `T_0` and `T_17` while the
//!    primary grew, the inspector learns that `L_1` is not the document you were really editing.
//!    Cleanliness alone is not enough; the decoy has to *behave like a document* over time.
//!
//! # The v5.0.0 mechanism, and why it was replaced
//!
//! The old design kept a "parasitic decoy reservoir" of pre-authored future revisions inside the
//! primary stratum and injected them into `L_1` on each save. It satisfied (1) and (2) in the
//! steady state, but it had two defects. The reservoir had to be authored *before* it was used,
//! so `L_k` at `T_0` contained a plaintext-independent blob whose size revealed the intended
//! number of future saves. And nothing bound the duress passphrase to the stratum, so a save that
//! rotated `K_unwrap` would silently invalidate `P_1` on older snapshots — the exact failure the
//! amended spec calls out.
//!
//! # The v5.1.0 mechanism
//!
//! * **`K_unwrap` for every lower stratum is invariant across saves.** It is derived from
//!   `(passphrase, σ_epoch)` only, never from anything the save ratchets. Rotating a stratum's
//!   `ML-KEM` keypair (FR-3.2) is fine precisely because it is the *keypair* that rotates, not
//!   the passphrase binding. Consequence: `P_1` unlocks `T_0 … T_n`, which is the amended
//!   requirement, and [`crate::duress::duress_binding_is_invariant`] asserts it.
//! * **Synthetic edits are generated, not stored.** On each save of `L_k` we derive an
//!   effort-matched delta from `(κ_epoch, save_cursor)` with a deterministic PRNG and apply it to
//!   `L_1`. Nothing is authored in advance, so `T_0`'s `L_1` genuinely cannot contain future
//!   state — there is no future state to contain. This is a stronger guarantee than v5.0.0's,
//!   and it is the reason the reservoir is gone.
//! * **Word counts grow proportionally.** The synthetic delta's size is a fixed fraction of the
//!   real session's delta, so `L_1`'s growth tracks `L_k`'s within a bounded ratio:
//!   [`DuressProfile`] pins that ratio and [`check_proportional_growth`] is the test.

use zeroize::Zeroize;

/// The telemetry a save records about the real editing session, and the only input the synthetic
/// delta is allowed to be a function of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionDelta {
    /// Characters added in the primary stratum this session.
    pub chars_added: u64,
    /// Characters removed.
    pub chars_removed: u64,
    /// Blocks (paragraphs) touched.
    pub blocks_modified: u32,
    /// Wall-clock duration of the session, in milliseconds. Recorded so the injected timestamps
    /// can be plausible; never used as key material.
    pub duration_ms: u64,
}

impl SessionDelta {
    /// Net characters. May be negative.
    pub fn net(&self) -> i64 {
        self.chars_added as i64 - self.chars_removed as i64
    }
}

/// How the decoy is made to look like the primary.
///
/// The ratio is bounded rather than exact: an inspector comparing `L_1` and `L_k` growth should
/// see two documents being edited at similar but not identical rates, which is what two real
/// documents by one person look like. An *exactly* proportional decoy is itself a fingerprint —
/// it is the duress analogue of the "50.000%" mistake, except here the constant would be the tell
/// rather than the leak.
#[derive(Debug, Clone, Copy)]
pub struct DuressProfile {
    /// Fraction of the primary's net character delta the decoy receives, as `(num, den)`.
    pub ratio: (u32, u32),
    /// How far either side of `ratio` the actual injection may drift, in permille.
    pub jitter_permille: u32,
    /// Tolerance band on the long-run growth ratio between `L_1` and `L_k`.
    pub growth_band: (f64, f64),
}

impl DuressProfile {
    /// The default profile: decoy grows at ~62% of the primary's rate, ±4% (the amended spec's
    /// "effort-matched" wording), and the two documents' cumulative word counts must stay within
    /// `[0.45, 0.85]` of each other across `T_0 … T_n`.
    pub const DEFAULT: Self = Self {
        ratio: (62, 100),
        jitter_permille: 40,
        growth_band: (0.45, 0.85),
    };

    /// How many characters the decoy gains for a given real session.
    ///
    /// Returns a signed value: a session that deleted text makes the decoy shrink too, because a
    /// decoy that only ever grows while the primary shrinks is a decoy that is not being edited.
    pub fn decoy_char_delta(&self, session: &SessionDelta, jitter_seed: u64) -> i64 {
        let net = session.net();
        let (num, den) = self.ratio;
        let base = (net as f64) * (num as f64 / den as f64);
        // Deterministic jitter in ±jitter_permille. Derived from the epoch, never from a clock,
        // so two runs of the same save produce the same decoy (determinism is a hard requirement
        // of v5.1.0 — see docs/PRD-v5.1.0.md §5.4).
        let span = self.jitter_permille as i64 * 2 + 1;
        let pick = (splitmix64(jitter_seed) % span as u64) as i64 - self.jitter_permille as i64;
        let jittered = base * (1000.0 + pick as f64) / 1000.0;
        jittered.round() as i64
    }
}

/// SplitMix64 finaliser. Used for deterministic, non-secret stream derivation.
///
/// Explicitly **not** a CSPRNG and not used for anything secret: it derives the *content* of a
/// decoy that is, by construction, not confidential.
pub fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The epoch-scoped secret that makes a decoy delta unreproducible by an inspector who holds only
/// the container. Wraps and zeroizes.
#[derive(Zeroize)]
pub struct DecoyEpochKey(pub [u8; 32]);

/// A generated decoy delta. Text is not secret; the *derivation* is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecoyDelta {
    /// Characters to add (positive) or remove (negative).
    pub char_delta: i64,
    /// Synthetic block inserts, each a paragraph break and a word run.
    pub block_inserts: u32,
    /// Distinct base-36 word tokens, generated deterministically, that the caller substitutes
    /// into `L_1`. Kept as tokens rather than prose because `L_1`'s language and style are
    /// whatever the user authored at provisioning; the delta only has to move the counters.
    pub tokens: Vec<String>,
}

impl DecoyDelta {
    /// Generate the delta for one save.
    ///
    /// `save_cursor` is the monotonic save counter; `epoch_key` is derived from `κ_epoch`, so two
    /// containers with the same save history do not generate the same decoy.
    pub fn generate(
        profile: DuressProfile,
        session: &SessionDelta,
        save_cursor: u64,
        epoch_key: &DecoyEpochKey,
    ) -> Self {
        let seed = u64::from_le_bytes(epoch_key.0[0..8].try_into().unwrap()) ^ save_cursor;
        let char_delta = profile.decoy_char_delta(session, seed);

        // One synthetic block per ~400 characters of magnitude, at least one, at most what the
        // session touched. Bounded so a pathological session cannot make one save rewrite `L_1`.
        let magnitude = char_delta.unsigned_abs();
        let blocks = ((magnitude / 400).clamp(1, 64) as u32)
            .min(session.blocks_modified.max(1).saturating_mul(2));

        let mut tokens = Vec::with_capacity(blocks as usize);
        let mut s = splitmix64(seed);
        for _ in 0..blocks {
            s = splitmix64(s);
            tokens.push(base36_token(s));
        }

        Self {
            char_delta,
            block_inserts: blocks,
            tokens,
        }
    }
}

/// A 5-character `[a-z0-9]` token. Deterministic from `value`.
pub fn base36_token(mut value: u64) -> String {
    const ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut out = [b'a'; 5];
    for slot in out.iter_mut().rev() {
        *slot = ALPHABET[(value % 36) as usize];
        value /= 36;
    }
    String::from_utf8(out.to_vec()).expect("base36 alphabet is ASCII")
}

/// One observed snapshot, for the growth check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotRecord {
    /// Monotonic save counter for this snapshot.
    pub save_cursor: u64,
    /// Cumulative word count of the primary stratum.
    pub primary_words: u64,
    /// Cumulative word count of the duress stratum.
    pub duress_words: u64,
}

/// Verify the pair of properties the amended spec asks for across `T_0 … T_n`:
/// proportional growth, and monotonicity of both documents.
///
/// Returns the observed ratio range on success so a test can print it.
pub fn check_proportional_growth(
    profile: DuressProfile,
    history: &[SnapshotRecord],
) -> Result<(f64, f64), String> {
    if history.len() < 2 {
        return Err("need at least two snapshots".into());
    }
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for pair in history.windows(2) {
        let (prev, next) = (pair[0], pair[1]);
        if next.save_cursor <= prev.save_cursor {
            return Err(format!(
                "save cursor did not advance: {} -> {}",
                prev.save_cursor, next.save_cursor
            ));
        }
        let dp = next.primary_words as i64 - prev.primary_words as i64;
        let dd = next.duress_words as i64 - prev.duress_words as i64;
        if dp == 0 && dd == 0 {
            continue; // an idle snapshot is plausible for both documents
        }
        if dp > 0 && dd <= 0 {
            return Err(format!(
                "primary grew by {dp} while duress did not grow ({dd}) at save {}",
                next.save_cursor
            ));
        }
        if dp != 0 {
            let ratio = dd as f64 / dp as f64;
            lo = lo.min(ratio);
            hi = hi.max(ratio);
            if ratio < profile.growth_band.0 || ratio > profile.growth_band.1 {
                return Err(format!(
                    "growth ratio {ratio:.3} outside band {:?} at save {}",
                    profile.growth_band, next.save_cursor
                ));
            }
        }
    }
    if !lo.is_finite() {
        return Ok((1.0, 1.0));
    }
    Ok((lo, hi))
}

/// `P_1` must open every historical snapshot. This is the amended §3.6 requirement, expressed as
/// an assertion over a save history rather than as prose.
///
/// The function takes the *invariant* binding by reference and the per-snapshot bindings, and
/// fails if any snapshot's binding differs from the first. In practice the caller feeds
/// `K_unwrap`-derivation inputs; here the binding is any comparable bytes, which keeps the check
/// testable without pulling the KDF into this crate.
pub fn duress_binding_is_invariant<T: PartialEq>(bindings: &[T]) -> Result<(), String> {
    let Some((first, rest)) = bindings.split_first() else {
        return Err("no snapshots".into());
    };
    for (i, b) in rest.iter().enumerate() {
        if b != first {
            return Err(format!(
                "duress passphrase binding changed at snapshot {}; P_1 would not open T_0..T_n",
                i + 1
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> DecoyEpochKey {
        DecoyEpochKey([b; 32])
    }

    #[test]
    fn decoy_is_deterministic_for_the_same_epoch() {
        let s = SessionDelta {
            chars_added: 1_000,
            chars_removed: 100,
            blocks_modified: 4,
            duration_ms: 60_000,
        };
        let a = DecoyDelta::generate(DuressProfile::DEFAULT, &s, 9, &key(7));
        let b = DecoyDelta::generate(DuressProfile::DEFAULT, &s, 9, &key(7));
        assert_eq!(
            a, b,
            "same save + same epoch must regenerate the same decoy"
        );
    }

    #[test]
    fn decoy_differs_across_epochs_and_saves() {
        let s = SessionDelta {
            chars_added: 1_000,
            chars_removed: 100,
            blocks_modified: 4,
            duration_ms: 60_000,
        };
        let a = DecoyDelta::generate(DuressProfile::DEFAULT, &s, 9, &key(7));
        let b = DecoyDelta::generate(DuressProfile::DEFAULT, &s, 9, &key(8));
        let c = DecoyDelta::generate(DuressProfile::DEFAULT, &s, 10, &key(7));
        assert_ne!(a.char_delta, b.char_delta);
        assert_ne!(a.tokens, c.tokens);
    }

    #[test]
    fn decoy_tracks_a_deletion_too() {
        let s = SessionDelta {
            chars_added: 0,
            chars_removed: 900,
            blocks_modified: 2,
            duration_ms: 1_000,
        };
        let d = DecoyDelta::generate(DuressProfile::DEFAULT, &s, 1, &key(1));
        assert!(
            d.char_delta < 0,
            "deleting from the primary must shrink the decoy"
        );
    }

    #[test]
    fn decoy_magnitude_is_a_bounded_fraction() {
        let s = SessionDelta {
            chars_added: 10_000,
            chars_removed: 0,
            blocks_modified: 10,
            duration_ms: 1_000,
        };
        for save in 0..64u64 {
            let d = DecoyDelta::generate(DuressProfile::DEFAULT, &s, save, &key(3));
            let ratio = d.char_delta as f64 / 10_000.0;
            assert!(
                (0.55..0.70).contains(&ratio),
                "save {save}: decoy ratio {ratio:.3} escaped effort-matching"
            );
        }
    }

    #[test]
    fn tokens_are_five_char_alnum() {
        for v in [0u64, 1, 35, 36, u64::MAX] {
            let t = base36_token(v);
            assert_eq!(t.len(), 5);
            assert!(t
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()));
        }
    }

    #[test]
    fn proportional_growth_accepts_a_realistic_history() {
        let mut history = Vec::new();
        let mut primary = 1_000u64;
        let mut duress = 940u64;
        for save in 0..12u64 {
            history.push(SnapshotRecord {
                save_cursor: save,
                primary_words: primary,
                duress_words: duress,
            });
            primary += 100;
            duress += 62;
        }
        let (lo, hi) = check_proportional_growth(DuressProfile::DEFAULT, &history)
            .expect("realistic growth must pass");
        assert!((0.55..0.70).contains(&lo) && (0.55..0.70).contains(&hi));
    }

    #[test]
    fn proportional_growth_rejects_a_frozen_decoy() {
        // The cross-snapshot tell the PRD never addressed: `L_1` byte-identical while `L_k` grows.
        let history = [
            SnapshotRecord {
                save_cursor: 0,
                primary_words: 1_000,
                duress_words: 900,
            },
            SnapshotRecord {
                save_cursor: 1,
                primary_words: 1_500,
                duress_words: 900,
            },
        ];
        let err = check_proportional_growth(DuressProfile::DEFAULT, &history).unwrap_err();
        assert!(err.contains("did not grow"), "{err}");
    }

    #[test]
    fn duress_binding_must_be_invariant() {
        assert!(duress_binding_is_invariant(&[[0u8; 32], [0u8; 32], [0u8; 32]]).is_ok());
        let err = duress_binding_is_invariant(&[[0u8; 32], [0u8; 32], [1u8; 32]]).unwrap_err();
        assert!(err.contains("changed at snapshot 2"), "{err}");
        assert!(duress_binding_is_invariant::<[u8; 32]>(&[]).is_err());
    }
}
