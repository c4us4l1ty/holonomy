//! One coarse error facade for the whole boundary.
//!
//! Errors that cross the boundary deliberately say little: a "why" that distinguishes
//! wrong-passphrase from tampered-capsule from short-file is a decryption oracle's vocabulary.
//! Internally the module errors stay specific (they are test hooks); what is `pub` and leaves
//! the crate is this.

use crate::commit::AeadError;
use crate::envelope::EnvelopeError;
use crate::kdf::KdfError;

/// The public error type of `holonomy-seal`. Display text is intentionally generic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealError {
    /// The passphrase KDF failed (parameters or internal failure).
    DerivationFailed,
    /// The capsule could not be opened with the presented unwrap key: wrong passphrase, wrong
    /// USB slot state, or damaged bytes — deliberately undifferentiated.
    OpenRejected,
    /// A key commitment did not match presented key material.
    CommitmentMismatch,
    /// An AEAD authentication check failed after a matching commitment.
    AuthenticationFailed,
    /// An encoded field had a length the format does not allow.
    BadEncoding,
    /// The platform CSPRNG refused or failed; no fallback exists on purpose.
    EntropyUnavailable,
}

impl std::fmt::Display for SealError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DerivationFailed => f.write_str("key derivation failed"),
            Self::OpenRejected => f.write_str("the sealed object could not be opened"),
            Self::CommitmentMismatch => f.write_str("key commitment mismatch"),
            Self::AuthenticationFailed => f.write_str("authentication failed"),
            Self::BadEncoding => f.write_str("malformed sealed encoding"),
            Self::EntropyUnavailable => f.write_str("system entropy unavailable"),
        }
    }
}

impl std::error::Error for SealError {}

impl From<KdfError> for SealError {
    fn from(_: KdfError) -> Self {
        Self::DerivationFailed
    }
}

impl From<EnvelopeError> for SealError {
    fn from(e: EnvelopeError) -> Self {
        match e {
            EnvelopeError::UnwrapFailed
            | EnvelopeError::Phase1CommitmentMismatch
            | EnvelopeError::KemRejected => Self::OpenRejected,
            EnvelopeError::BadLength => Self::BadEncoding,
        }
    }
}

impl From<AeadError> for SealError {
    fn from(e: AeadError) -> Self {
        match e {
            AeadError::CommitmentMismatch => Self::CommitmentMismatch,
            AeadError::AuthenticationFailed => Self::AuthenticationFailed,
            AeadError::TooLarge { .. } | AeadError::OutputTooSmall => Self::BadEncoding,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_strings_never_distinguish_wrong_passphrase_from_tampering() {
        // If a UI ever needs the distinction, that is the moment to name it as an oracle
        // design decision in AUDIT.md — not to widen this enum casually.
        let open = SealError::OpenRejected.to_string();
        assert_eq!(open, "the sealed object could not be opened");
    }

    #[test]
    fn module_errors_map_into_the_coarse_facade() {
        let se: SealError = EnvelopeError::UnwrapFailed.into();
        assert_eq!(se, SealError::OpenRejected);
        let se: SealError = AeadError::CommitmentMismatch.into();
        assert_eq!(se, SealError::CommitmentMismatch);
        let se: SealError = KdfError::InvalidParams.into();
        assert_eq!(se, SealError::DerivationFailed);
    }
}
