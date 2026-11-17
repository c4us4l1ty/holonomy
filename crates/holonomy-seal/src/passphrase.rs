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

/// A normalized, zeroizing passphrase. Bytes are exposed only inside a callback so nothing
/// holds a second copy after the row of work is done.
pub struct PassphraseInput(Zeroizing<String>);

impl PassphraseInput {
    /// Accept user text: NFC-normalize, bound-check, wrap zeroizing.
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
            mark.to_string().repeat((MAX_PASSPHRASE_INPUT_BYTES - 2) / mark.len_utf8())
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
}
