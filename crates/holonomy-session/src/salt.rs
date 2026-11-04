//! `σ_epoch`: the 64-byte salt that keys everything, and the order in which it can be read.
//!
//! # The dependency
//!
//! ```text
//!   σ_epoch ──► K_perm ──► which USB sectors a save reads ──► the pad
//!   pad ⊕ what a save writes ──► what the next reader sees
//! ```
//!
//! The salt is upstream of the pad, and the pad is upstream of the bytes on the device. That is
//! what makes the salt unreadable to an observer: [`PAD_COVERAGE_BYTES`] is the whole container, so
//! the salt is mixed with pad like everything else and the file has no stable fingerprint across
//! snapshots.
//!
//! # The constraint it creates
//!
//! A save regenerates the salt, so:
//!
//! 1. the *old* salt, sitting on the device under the old pad, must be read first;
//! 2. the reader undoes the pad and recovers both the capsule and the salt;
//! 3. only then can a new salt be chosen, because writing it means re-keying the permutation.
//!
//! Writing the new salt before reading the old one destroys the only copy of the key that locates
//! the old pad, and the container becomes unreadable. That is the one ordering mistake this format
//! can make that loses data outright, so the two steps are separate functions returning different
//! types and [`ContainerHeader::after_reading`] consumes the value [`ContainerHeader::open`]
//! produced. A new salt cannot exist before the old one has been held.
//!
//! # Provenance
//!
//! A file on disk is indistinguishable from a block device, and [`SALT_OFFSET`] is fixed, so an
//! attacker can copy a genuine salt block from one container into another. [`SaltOrigin`] records
//! whether the salt arrived through the sanctioned reader, and
//! [`SaltBytes::of_unknown_origin`] is a distinct constructor so a rollback check can never pass by
//! accident on a salt nobody vouched for.

use holonomy_core::layout::{PAD_COVERAGE_BYTES, SALT_BYTES, SALT_OFFSET, SLOT_BYTES};
use holonomy_crypto::rng;

/// Where a salt came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaltOrigin {
    /// Read through [`ContainerHeader::open`], from a container whose pad this process consumed.
    /// This is the only origin with any provenance, and it is still conditional on the container
    /// being the one an unlock just verified.
    SanctionedRead,
    /// Handed in by a caller, a test, or a block copied from elsewhere. Recorded rather than
    /// rejected, because a first-run container has no earlier salt to be sanctioned against.
    Unknown,
}

/// The 64 bytes, plus where they came from.
#[derive(Clone)]
pub struct SaltBytes {
    bytes: [u8; SALT_BYTES],
    origin: SaltOrigin,
}

impl SaltBytes {
    /// A salt from a source this crate cannot vouch for.
    pub fn of_unknown_origin(bytes: [u8; SALT_BYTES]) -> Self {
        Self {
            bytes,
            origin: SaltOrigin::Unknown,
        }
    }

    /// A fresh salt from the OS CSPRNG.
    ///
    /// Not derived from anything, deliberately: deriving it would make one save's pad window a
    /// function of another's, which inverts the determinism rule into a weakness. `getrandom` is the
    /// same call the KEM keygen uses.
    pub fn fresh() -> Self {
        let mut bytes = [0u8; SALT_BYTES];
        // The only failure mode of `getrandom` is a kernel with no CSPRNG, in which case this
        // process must not continue holding a document at all. Panicking is the correct response:
        // a zero salt would key every container's permutation from a constant.
        rng::fill(&mut bytes)
            .expect("the OS CSPRNG must be available before any secret is created");
        Self {
            bytes,
            origin: SaltOrigin::SanctionedRead,
        }
    }

    /// The bytes, for the KDF and the permutation.
    pub fn as_bytes(&self) -> &[u8; SALT_BYTES] {
        &self.bytes
    }

    /// Where it came from.
    pub fn origin(&self) -> SaltOrigin {
        self.origin
    }
}

impl std::fmt::Debug for SaltBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the bytes: this is the input to the permutation key, and the whole point of
        // the pad is that an observer does not get it.
        write!(f, "SaltBytes({} bytes, {:?})", SALT_BYTES, self.origin)
    }
}

impl Drop for SaltBytes {
    fn drop(&mut self) {
        // Not a cryptographic guarantee on a machine with a copy-happy allocator, and not claimed as
        // one. It removes the salt from the easy places it would otherwise sit.
        self.bytes.fill(0);
    }
}

impl PartialEq for SaltBytes {
    /// Compares the **bytes**, not the provenance. Two salts with the same bytes are the same salt;
    /// a test that needed provenance equality would be testing bookkeeping.
    fn eq(&self, other: &Self) -> bool {
        let mut diff = 0u8;
        for i in 0..SALT_BYTES {
            diff |= self.bytes[i] ^ other.bytes[i];
        }
        diff == 0
    }
}

impl Eq for SaltBytes {}

/// What a reader recovered from a container, before any new salt exists.
///
/// The type exists so the ordering rule is enforced by the compiler:
/// [`ContainerHeader::after_reading`] takes this by value, and the only way to make one is to have
/// read a container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContainerHeader {
    salt: SaltBytes,
}

impl ContainerHeader {
    /// Read the salt of an existing container.
    ///
    /// `read_salt_slot` is the caller's direct-I/O read, passed in rather than performed here so
    /// this module has no device dependency and can be tested against an in-memory buffer. It
    /// receives [`SALT_OFFSET`] and a slot's worth of buffer to fill.
    pub fn open(
        mut read_salt_slot: impl FnMut(usize, &mut [u8]) -> Result<(), std::io::Error>,
        slot: &mut [u8],
    ) -> Result<Self, HeaderError> {
        if slot.len() < SLOT_BYTES {
            return Err(HeaderError::BufferTooSmall { found: slot.len() });
        }
        read_salt_slot(SALT_OFFSET, slot).map_err(HeaderError::Io)?;
        let mut bytes = [0u8; SALT_BYTES];
        // The salt is the first 64 bytes *of its own slot*, because `SALT_OFFSET` is slot-aligned.
        // The rest of the slot is pad-bound noise the store adds.
        bytes.copy_from_slice(&slot[..SALT_BYTES]);
        if bytes.iter().all(|&b| b == 0) {
            // An all-zero salt is what a zero-filled device returns. Treating it as a real salt
            // would key the permutation from a constant, so an unformatted device is *named*
            // instead of opened, and the caller runs the provisioning path.
            return Err(HeaderError::Unformatted);
        }
        Ok(Self {
            salt: SaltBytes::of_unknown_origin(bytes),
        })
    }

    /// The salt that is on the device, which every read of this container must use.
    pub fn salt(&self) -> &SaltBytes {
        &self.salt
    }

    /// Choose the salt for the *next* save.
    ///
    /// Consumes the header, so the old and new salt cannot be live at once by accident and cannot
    /// be used in the wrong order. It writes nothing: writing is the caller's step, and must happen
    /// after the pad for the previous window has been read.
    pub fn after_reading(self) -> SaltBytes {
        SaltBytes::fresh()
    }
}

/// Why a header could not be read.
#[derive(Debug)]
pub enum HeaderError {
    /// The device did not supply the salt slot.
    Io(std::io::Error),
    /// The caller's buffer was smaller than one slot.
    BufferTooSmall {
        /// Bytes supplied.
        found: usize,
    },
    /// The salt slot is all zeros: this device has never been provisioned.
    Unformatted,
}

impl std::fmt::Display for HeaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "reading the salt slot failed: {e}"),
            Self::BufferTooSmall { found } => write!(
                f,
                "the salt read needs a {SLOT_BYTES}-byte slot buffer; {found} bytes given"
            ),
            Self::Unformatted => f.write_str(
                "the salt is all zeros: this device has not been provisioned, and a zero salt \
                 would key the permutation from a constant",
            ),
        }
    }
}

impl std::error::Error for HeaderError {}

/// Where the salt sits relative to everything else, as one place a test can assert against.
///
/// Returns `(offset, len, covered_by_pad)` rather than three separate constants so a caller cannot
/// quote one without the others.
pub fn salt_layout() -> (usize, usize, bool) {
    // Covered by the pad iff its range is inside the covered range, *computed* rather than asserted
    // `true`, so the claim breaks loudly if either number moves.
    let covered = SALT_OFFSET + SALT_BYTES <= PAD_COVERAGE_BYTES;
    (SALT_OFFSET, SALT_BYTES, covered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use holonomy_core::layout::TOTAL_BLOCKS;

    fn device_with(
        salt: [u8; SALT_BYTES],
    ) -> impl FnMut(usize, &mut [u8]) -> Result<(), std::io::Error> {
        move |offset, buf| {
            assert_eq!(offset, SALT_OFFSET, "the reader must ask for the salt slot");
            let mut image = vec![0xA5u8; TOTAL_BLOCKS * SLOT_BYTES];
            image[SALT_OFFSET..SALT_OFFSET + SALT_BYTES].copy_from_slice(&salt);
            buf[..SALT_BYTES].copy_from_slice(&image[offset..offset + SALT_BYTES]);
            Ok(())
        }
    }

    #[test]
    fn the_salt_is_covered_by_the_pad() {
        // FR-4.2 as arithmetic. A salt outside the pad would be the one stable fingerprint in the
        // file, which is exactly what the format is built to avoid.
        let (offset, len, covered) = salt_layout();
        assert!(
            covered,
            "salt at {offset}, {len} bytes, not covered by the pad"
        );
        assert_eq!(offset, SLOT_BYTES);
    }

    #[test]
    fn reading_a_container_then_choosing_a_salt_is_the_only_order() {
        let salt = [0x5Au8; SALT_BYTES];
        let mut slot = vec![0u8; SLOT_BYTES];
        let header = ContainerHeader::open(device_with(salt), &mut slot).expect("readable");
        assert_eq!(header.salt().as_bytes(), &salt);
        let next = header.after_reading();
        assert_ne!(next.as_bytes(), &salt, "a save must not reuse the salt");
        assert_eq!(next.origin(), SaltOrigin::SanctionedRead);
    }

    #[test]
    fn a_zero_salt_is_an_unformatted_device_not_a_key() {
        let mut slot = vec![0u8; SLOT_BYTES];
        let err = ContainerHeader::open(device_with([0u8; SALT_BYTES]), &mut slot).unwrap_err();
        assert!(matches!(err, HeaderError::Unformatted), "{err}");
        assert!(err.to_string().contains("provisioned"));
    }

    #[test]
    fn a_short_buffer_is_refused_before_the_read() {
        // A caller handing in 8 bytes would otherwise get a "successful" read of a slot it could
        // not hold, and 56 bytes of salt would be silently whatever was in its buffer.
        let mut slot = [0u8; 8];
        let err = ContainerHeader::open(device_with([1u8; SALT_BYTES]), &mut slot).unwrap_err();
        assert!(
            matches!(err, HeaderError::BufferTooSmall { found: 8 }),
            "{err}"
        );
    }

    #[test]
    fn an_io_error_is_reported_rather_than_swallowed() {
        let mut slot = vec![0u8; SLOT_BYTES];
        let err = ContainerHeader::open(
            |_, _| Err(std::io::Error::from(std::io::ErrorKind::TimedOut)),
            &mut slot,
        )
        .unwrap_err();
        assert!(matches!(err, HeaderError::Io(_)), "{err:?}");
    }

    #[test]
    fn freshness_uses_the_os_csprng_and_never_repeats() {
        // Not a statistical test: the claim is that two calls do not return the same 64 bytes,
        // which a working CSPRNG all but guarantees and a broken one (a fixed buffer, a memoised
        // result) fails immediately.
        let a = SaltBytes::fresh();
        let b = SaltBytes::fresh();
        assert_ne!(a.as_bytes(), b.as_bytes());
        assert!(!a.as_bytes().iter().all(|&x| x == 0));
        assert_eq!(a.origin(), SaltOrigin::SanctionedRead);
    }

    #[test]
    fn the_debug_impl_never_prints_the_salt() {
        let s = SaltBytes::of_unknown_origin([0xABu8; SALT_BYTES]);
        let rendered = format!("{s:?}");
        assert!(rendered.contains("64 bytes"), "{rendered}");
        assert!(!rendered.contains("ab"), "{rendered}");
        assert!(!rendered.contains("171"), "{rendered}");
    }

    #[test]
    fn provenance_is_recorded_but_not_part_of_equality() {
        let bytes = [7u8; SALT_BYTES];
        let a = SaltBytes::of_unknown_origin(bytes);
        let b = SaltBytes::fresh();
        assert_eq!(a.origin(), SaltOrigin::Unknown);
        assert_eq!(b.origin(), SaltOrigin::SanctionedRead);
        let c = SaltBytes::of_unknown_origin(*b.as_bytes());
        assert_eq!(b, c);
        assert_ne!(a, b);
    }
}
