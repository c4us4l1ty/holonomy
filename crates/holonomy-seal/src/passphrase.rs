//! Passphrase intake: NFC normalization, bounds, zeroized carriage.
//!
//! Two inherited lessons shape this small module:
//!
//! * **v5's legacy mapping lowercased** passphrases (silently discarding entropy and
//!   surprising users). RFC-001 rejects that: v6 accepts the user's exact text, normalized to
//!   NFC so the *same logical string* types identically across macOS/Windows/Linux input
//!   methods — and nothing else is touched.
//! * **Bounds exist:** empty input, raw input beyond [`MAX_PASSPHRASE_INPUT_BYTES`], and NFC
//!   output beyond [`MAX_PASSPHRASE_BYTES`] are refused. The raw cap bounds (but does not wipe)
//!   dependency-owned normalization scratch; the output is built in a fixed wipe-on-drop string.
//!   Within those bounds, passphrase strength remains the user's decision; the UI warns rather
//!   than blocks.

use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

/// Longest accepted NFC-normalized passphrase in UTF-8 bytes.
pub const MAX_PASSPHRASE_BYTES: usize = 1_024;

/// Hard UTF-8 input bound applied before normalization to bound dependency-owned scratch.
pub const MAX_PASSPHRASE_INPUT_BYTES: usize = 4_096;

/// Why a passphrase was refused at intake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassphraseError {
    /// Empty after NFC normalization (including whitespace-free empty strings).
    Empty,
    /// Raw UTF-8 input exceeds [`MAX_PASSPHRASE_INPUT_BYTES`] before normalization.
    InputTooLong {
        /// Raw input length in UTF-8 bytes.
        len: usize,
    },
    /// Longer than [`MAX_PASSPHRASE_BYTES`] after normalization.
    TooLong {
        /// The normalized UTF-8 length observed when the limit was crossed.
        len: usize,
    },
}

impl core::fmt::Display for PassphraseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Empty => f.write_str("passphrase is empty"),
            Self::InputTooLong { len } => write!(
                f,
                "raw passphrase exceeds the {MAX_PASSPHRASE_INPUT_BYTES}-byte input limit ({len} bytes)"
            ),
            Self::TooLong { len } => write!(
                f,
                "normalized passphrase exceeds the {MAX_PASSPHRASE_BYTES}-byte limit (at least {len} bytes)"
            ),
        }
    }
}

impl std::error::Error for PassphraseError {}

/// A normalized passphrase whose owned output buffer is zeroized on drop. Bytes are exposed
/// only inside a callback. The borrowed source text and unicode-normalization's internal
/// iterator buffers are outside this guard; `new` bounds, but does not wipe, the latter.
pub struct PassphraseInput(Zeroizing<String>);

impl PassphraseInput {
    /// NFC-normalize and bound-check borrowed user text into a zeroizing output buffer.
    /// This does not zeroize the caller's source text or dependency-owned normalization scratch.
    pub fn new(raw: &str) -> Result<Self, PassphraseError> {
        if raw.is_empty() {
            return Err(PassphraseError::Empty);
        }

        // unicode-normalization 0.1.26 buffers pending canonical-combining sequences in
        // non-zeroizing TinyVecs. Bound the source length before entering it. This limits
        // scratch growth; it does not wipe the dependency-owned buffers on drop.
        if raw.len() > MAX_PASSPHRASE_INPUT_BYTES {
            return Err(PassphraseError::InputTooLong { len: raw.len() });
        }

        // Allocate the maximum accepted size once and guard it before writing secret bytes.
        // Bounded pushes avoid String growth/reallocation, which could otherwise free an
        // intermediate normalized passphrase before the wipe-on-drop guard exists.
        let mut normalized = Zeroizing::new(String::with_capacity(MAX_PASSPHRASE_BYTES));
        let mut normalized_len = 0usize;
        for character in raw.nfc() {
            normalized_len = normalized_len.saturating_add(character.len_utf8());
            if normalized_len > MAX_PASSPHRASE_BYTES {
                #[cfg(test)]
                if !normalized.is_empty() {
                    crate::allocation_audit::watch(normalized.as_ptr(), normalized.len());
                }
                return Err(PassphraseError::TooLong {
                    len: normalized_len,
                });
            }
            normalized.push(character);
        }

        #[cfg(test)]
        if !normalized.is_empty() {
            crate::allocation_audit::watch(normalized.as_ptr(), normalized.len());
        }
        if normalized.is_empty() {
            return Err(PassphraseError::Empty);
        }
        Ok(Self(normalized))
    }

    /// Run `f` with the passphrase bytes, producing some derived value without the bytes
    /// themselves escaping the borrow. This is the *only* byte view.
    pub fn with_bytes<R>(&self, f: impl FnOnce(&[u8]) -> R) -> R {
        f(self.0.as_bytes())
    }
}

impl core::fmt::Debug for PassphraseInput {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("PassphraseInput(<redacted>)")
    }
}

/// Qualitative passphrase strength bucket returned by [`estimate_passphrase_strength`].
///
/// The UI warns rather than blocks: any non-empty passphrase within the byte bounds is accepted
/// by [`PassphraseInput::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrengthTier {
    /// Empty passphrase (`0` characters).
    Empty,
    /// Below `40` estimated entropy bits (e.g. short words, repeats, or simple sequences).
    Weak,
    /// `40..64` estimated entropy bits.
    Fair,
    /// `64..88` estimated entropy bits.
    Strong,
    /// `>= 88` estimated entropy bits.
    VeryStrong,
}

impl StrengthTier {
    /// Short UI label suitable for the unlock/create screen (`UI-SPEC.md` §5.2).
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Empty => "Empty",
            Self::Weak => "Weak",
            Self::Fair => "Fair",
            Self::Strong => "Strong",
            Self::VeryStrong => "Very strong",
        }
    }
}

/// Non-theatrical passphrase-strength estimate paired with `holonomy-seal`'s Argon2id fence
/// (`FENCE_MIN_KIB..=FENCE_MAX_KIB`, `CALIBRATION_T`, `CALIBRATION_P`, `TARGET_BAND_MS`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PassphraseStrength {
    /// Conservative estimated entropy bits accounting for character classes, distinct symbols,
    /// and repeated/sequential runs.
    pub estimated_bits: u16,
    /// Unicode scalar count in the inspected input.
    pub char_count: usize,
    /// Classified strength bucket.
    pub tier: StrengthTier,
    /// Argon2id memory-hardness floor in MiB (`FENCE_MIN_KIB / 1024`).
    pub kdf_min_mib: u32,
    /// Argon2id memory-hardness ceiling in MiB (`FENCE_MAX_KIB / 1024`).
    pub kdf_max_mib: u32,
    /// Target per-derivation time floor/ceiling in milliseconds (`TARGET_BAND_MS`).
    pub kdf_target_ms: (u64, u64),
}

/// Estimates passphrase guessing resistance without allocating or retaining secret bytes.
///
/// Uses observed alphabet classes, distinct-character coverage, and sequential/repeat penalties
/// so trivial inputs like `"aaaaaa"` or `"123456"` are never over-scored as strong.
#[must_use]
pub fn estimate_passphrase_strength(raw: &str) -> PassphraseStrength {
    let kdf_min_mib = crate::FENCE_MIN_KIB / 1024;
    let kdf_max_mib = crate::FENCE_MAX_KIB / 1024;
    let kdf_target_ms = crate::TARGET_BAND_MS;

    if raw.is_empty() {
        return PassphraseStrength {
            estimated_bits: 0,
            char_count: 0,
            tier: StrengthTier::Empty,
            kdf_min_mib,
            kdf_max_mib,
            kdf_target_ms,
        };
    }

    let mut has_lower = false;
    let mut has_upper = false;
    let mut has_digit = false;
    let mut has_space = false;
    let mut has_symbol = false;
    let mut has_non_ascii = false;

    let mut seen_chars: Zeroizing<Vec<u32>> = Zeroizing::new(Vec::with_capacity(64));
    let mut char_count = 0usize;
    let mut non_seq_steps = 0usize;
    let mut prev_code: Option<u32> = None;

    for ch in raw.chars() {
        char_count += 1;
        if ch.is_ascii_lowercase() {
            has_lower = true;
        } else if ch.is_ascii_uppercase() {
            has_upper = true;
        } else if ch.is_ascii_digit() {
            has_digit = true;
        } else if ch == ' ' {
            has_space = true;
        } else if ch.is_ascii() {
            has_symbol = true;
        } else {
            has_non_ascii = true;
        }

        let code = u32::from(ch);
        if !seen_chars.contains(&code) && seen_chars.len() < 128 {
            seen_chars.push(code);
        }
        if let Some(prev) = prev_code {
            let diff = code.abs_diff(prev);
            if diff > 1 {
                non_seq_steps += 1;
            }
        }
        prev_code = Some(code);
    }

    let mut pool_milli_bits: u32 = 0;
    let mut pool_size: u32 = 0;
    if has_lower {
        pool_size += 26;
    }
    if has_upper {
        pool_size += 26;
    }
    if has_digit {
        pool_size += 10;
    }
    if has_space {
        pool_size += 1;
    }
    if has_symbol {
        pool_size += 32;
    }
    if has_non_ascii {
        pool_size += 64;
    }
    if pool_size > 1 {
        // log2(pool_size) in milli-bits: integer log2 plus linear interpolation between powers of 2.
        let floor_log2 = 31 - pool_size.leading_zeros();
        let lo = 1u32 << floor_log2;
        let frac = ((pool_size - lo) * 1_000) / lo;
        pool_milli_bits = floor_log2 * 1_000 + (frac * 721) / 1_000;
    }

    // Effective character count blends unique characters with non-sequential steps so repeats
    // ("aaaaaa") and runs ("12345678") cannot accumulate high bit counts.
    let eff_len = if non_seq_steps == 0 {
        1usize
    } else {
        (seen_chars.len().min(non_seq_steps + 1) + char_count) / 2
    };
    let raw_bits = ((eff_len as u32).saturating_mul(pool_milli_bits) / 1_000).min(1_024);
    let estimated_bits = raw_bits as u16;
    let tier = if estimated_bits < 40 {
        StrengthTier::Weak
    } else if estimated_bits < 64 {
        StrengthTier::Fair
    } else if estimated_bits < 88 {
        StrengthTier::Strong
    } else {
        StrengthTier::VeryStrong
    };

    PassphraseStrength {
        estimated_bits,
        char_count,
        tier,
        kdf_min_mib,
        kdf_max_mib,
        kdf_target_ms,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfc_unifies_canonical_equivalents() {
        // é typed as one codepoint vs. e + combining accent must derive the same key material.
        let precomposed = PassphraseInput::new("caf\u{e9}").unwrap();
        let decomposed = PassphraseInput::new("cafe\u{301}").unwrap();
        let a = precomposed.with_bytes(|b| b.to_vec());
        let b = decomposed.with_bytes(|b| b.to_vec());
        assert_eq!(a, b);
    }

    #[test]
    fn exact_text_is_preserved_after_nfc() {
        // The anti-v5-legacy rule: no lowercasing, no trimming, ever.
        let p = PassphraseInput::new("  Correct Horse Battery Staple  ").unwrap();
        p.with_bytes(|b| assert_eq!(b, b"  Correct Horse Battery Staple  "));
    }

    #[test]
    fn empty_and_oversize_are_refused() {
        assert_eq!(
            PassphraseInput::new("").unwrap_err(),
            PassphraseError::Empty
        );
        let long = "a".repeat(MAX_PASSPHRASE_BYTES + 1);
        assert!(matches!(
            PassphraseInput::new(&long),
            Err(PassphraseError::TooLong { .. })
        ));
    }

    #[test]
    fn raw_input_limit_is_checked_before_normalization() {
        let raw = "x".repeat(MAX_PASSPHRASE_INPUT_BYTES + 1);
        assert_eq!(
            PassphraseInput::new(&raw).unwrap_err(),
            PassphraseError::InputTooLong {
                len: MAX_PASSPHRASE_INPUT_BYTES + 1
            }
        );
    }

    #[test]
    fn maximum_bounded_combining_sequence_is_rejected_at_the_nfc_output_limit() {
        // Fill the allowed raw input with one non-starter run. unicode-normalization 0.1.26
        // buffers this sequence in dependency-owned TinyVecs before it can emit normalized
        // output. This exercises the input bound and output limit; it does not prove that the
        // iterator's scratch is wiped on drop.
        let mark = char::from_u32(0x0301).unwrap();
        let raw = format!(
            "a{}b",
            mark.to_string()
                .repeat((MAX_PASSPHRASE_INPUT_BYTES - 2) / mark.len_utf8())
        );
        assert_eq!(raw.len(), MAX_PASSPHRASE_INPUT_BYTES);
        assert_eq!(
            PassphraseInput::new(&raw).unwrap_err(),
            PassphraseError::TooLong { len: 1_026 }
        );
    }

    #[test]
    fn nfc_expansion_is_counted_without_exceeding_the_fixed_buffer() {
        // U+0344 canonically decomposes to two two-byte combining marks under NFC. The first
        // mark of input 257 crosses the 1,024-byte limit at 1,026 bytes; the second is not
        // written into the fixed-size buffer.
        let raw = "\u{344}".repeat(513);
        assert_eq!(raw.len(), 1_026);
        assert_eq!(
            PassphraseInput::new(&raw).unwrap_err(),
            PassphraseError::TooLong { len: 1_026 }
        );
    }

    #[test]
    fn debug_is_redacted() {
        let p = PassphraseInput::new("hunter2").unwrap();
        assert_eq!(format!("{p:?}"), "PassphraseInput(<redacted>)");
    }

    #[test]
    fn strength_estimator_penalizes_repeats_and_scores_passphrases_honestly() {
        assert_eq!(estimate_passphrase_strength("").tier, StrengthTier::Empty);
        assert_eq!(
            estimate_passphrase_strength("aaaaaaaaaa").tier,
            StrengthTier::Weak
        );
        assert_eq!(
            estimate_passphrase_strength("123456").tier,
            StrengthTier::Weak
        );
        let diceware = estimate_passphrase_strength("correct horse battery staple");
        assert!(matches!(
            diceware.tier,
            StrengthTier::Strong | StrengthTier::VeryStrong
        ));
        assert!(diceware.estimated_bits >= 64);
    }
}
