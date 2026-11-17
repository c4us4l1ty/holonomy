//! Passphrase intake: NFC normalization, bounds, zeroized carriage.
//!
//! Two inherited lessons shape this small module:
//!
//! * **v5's legacy mapping lowercased** passphrases (silently discarding entropy and
//!   surprising users). RFC-001 rejects that: v6 accepts the user's exact text, normalized to
//!   NFC so the *same logical string* types identically across macOS/Windows/Linux input
//!   methods — and nothing else is touched.
//! * **Bounds exist:** an empty passphrase is an accident, not a choice; an absurdly long one
//!   is a paste mishap. Both are refused. Everything else — memorable or brutal — is the
//!   user's security decision, and the UI spec's strength meter warns rather than blocks.

use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroizing;

/// Longest accepted NFC-normalized passphrase in UTF-8 bytes.
pub const MAX_PASSPHRASE_BYTES: usize = 1_024;

/// Why a passphrase was refused at intake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PassphraseError {
    /// Empty after NFC normalization (including whitespace-free empty strings).
    Empty,
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
    fn nfc_expansion_is_counted_without_exceeding_the_fixed_buffer() {
        // U+0344 canonically decomposes to two UTF-8 combining marks under NFC. After 257
        // inputs, the normalized prefix crosses the 1,024-byte limit at 1,028 bytes.
        let raw = "\u{344}".repeat(513);
        assert_eq!(raw.len(), 1_026);
        assert_eq!(
            PassphraseInput::new(&raw).unwrap_err(),
            PassphraseError::TooLong { len: 1_028 }
        );
    }

    #[test]
    fn debug_is_redacted() {
        let p = PassphraseInput::new("hunter2").unwrap();
        assert_eq!(format!("{p:?}"), "PassphraseInput(<redacted>)");
    }
}
