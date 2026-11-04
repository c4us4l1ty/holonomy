//! The directory field: the container's index, and the only place a save's cursor is recorded.
//!
//! # Why the cursor lives here
//!
//! The pad window for save `k` is selected by `k`, so a reader must know which save produced the
//! container before it can find any pad at all. The root capsule cannot hold it: every field in that
//! 65,536-byte structure is accounted for (FR-4.3), and adding a cursor would change a size the
//! whole format depends on. The directory has space, is encrypted under `K_DEM`, authenticates with
//! a commitment, and is read on every open. So the cursor goes here.
//!
//! # Fixed length, always
//!
//! The plaintext is *always* [`DIRECTORY_PLAINTEXT_BYTES`], zero-padded. A container holding one
//! slot and a container holding 2,046 produce byte-identical field lengths, so an observer of the
//! capsule learns nothing about the document's size. That is the same argument
//! [`holonomy_crypto::commit::seal_directory`] makes, enforced here rather than assumed:
//! [`DirectoryLedger::encode`] returns a fixed-size array, so a short index is not representable.
//!
//! # Layout
//!
//! ```text
//!   offset  bytes  field
//!   0       2      format version (u16 LE, currently 1)
//!   2       1      stratum index (1..=4)
//!   3       1      flags (reserved; must be 0, so a future use is a version bump)
//!   4       8      save cursor (u64 LE)
//!   12      4      slot count (u32 LE)
//!   16      8,184  ciphertext length per slot (u32 LE each, capacity 2,046)
//!   8,200   32     ledger tag: BLAKE2b-256 over every field above and every slot's CMT-4 tag
//!   8,232   ...    zero padding to 60,592
//! ```
//!
//! # Why a hash list and not a commitment table
//!
//! The container addresses 2,046 payload slots, each with a 32-byte `CMT-4` commitment. Stored as a
//! table that is `2,046 × 36 = 73,656` bytes against a 60,592-byte field: it does not fit, and the
//! failure would have arrived at the 2,046th slot of a full document rather than anywhere a test
//! reaches. The directory instead keeps a per-slot length list and one tag over every slot's
//! commitment, so a slot cannot be swapped, dropped or reordered without the tag failing. A Merkle
//! tree would allow partial verification, which nothing needs — an opener reads every slot anyway.
//!
//! The arithmetic is a `const _` assertion beside [`DIRECTORY_CAPACITY_SLOTS`]; the behaviour it
//! implies — that a full index encodes and decodes, and one slot past the capacity is refused — is
//! `a_full_container_fits`.

use blake2::{Blake2b512, Digest};
use holonomy_core::layout::{MAX_STRATA, PAYLOAD_SLOTS, SLOT_BYTES};
#[cfg(test)]
use holonomy_crypto::commit::NONCE_BYTES;
use holonomy_crypto::commit::{CMT4_TAG_BYTES, DIRECTORY_PLAINTEXT_BYTES};
pub use holonomy_crypto::commit::DIRECTORY_VERSION;

use crate::seal::{SealError, SealedPayloadSlot};

/// Bytes each slot's length costs.
const LEN_BYTES: usize = 4;
/// Fixed header: version, stratum, flags, cursor, count.
const HEADER_BYTES: usize = 2 + 1 + 1 + 8 + 4;
/// Tag over the whole index.
pub const LEDGER_TAG_BYTES: usize = CMT4_TAG_BYTES;
/// Offset of the tag.
const TAG_OFFSET: usize = HEADER_BYTES + PAYLOAD_SLOTS * LEN_BYTES;
/// Smallest field that can hold a full container's index. The rest is padding.
pub const DIRECTORY_MIN_BYTES: usize = TAG_OFFSET + LEDGER_TAG_BYTES;

const _: () = assert!(DIRECTORY_MIN_BYTES == 8_232);
const _: () = assert!(DIRECTORY_MIN_BYTES <= DIRECTORY_PLAINTEXT_BYTES);

/// How many slots this format can describe. Must cover the container's payload.
pub const DIRECTORY_CAPACITY_SLOTS: usize =
    (DIRECTORY_PLAINTEXT_BYTES - HEADER_BYTES - LEDGER_TAG_BYTES) / LEN_BYTES;

const _: () = assert!(DIRECTORY_CAPACITY_SLOTS >= PAYLOAD_SLOTS);
// Why this is a hash list and not a commitment table: 2,046 slots x 36 bytes of (length, tag) is
// 73,656 bytes against a 60,592-byte field. A compile-time fact, so it cannot drift.
const _: () = assert!(PAYLOAD_SLOTS * (LEN_BYTES + CMT4_TAG_BYTES) > DIRECTORY_PLAINTEXT_BYTES);

/// The immutable index one save writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryLedger {
    /// Stratum this container was written for.
    pub stratum: u8,
    /// The save cursor: which pad window this container consumed.
    pub cursor: u64,
    /// Ciphertext length of each sealed payload slot, in logical order.
    pub lengths: Vec<u32>,
}

impl DirectoryLedger {
    /// Build the ledger for a set of sealed slots.
    ///
    /// The cursor is a required argument rather than defaulting to zero: it is the only record of
    /// which pad window this container consumed, and a constructor that defaulted it would write
    /// `0` into every container that forgot to say, producing a save that can never be read
    /// correctly again.
    pub fn new(stratum: u8, cursor: u64, slots: &[SealedPayloadSlot]) -> Self {
        Self {
            stratum,
            cursor,
            lengths: slots.iter().map(|s| s.ciphertext.len() as u32).collect(),
        }
    }

    /// How many slots this ledger describes.
    pub fn slot_count(&self) -> usize {
        self.lengths.len()
    }

    /// Encode to the fixed-length directory plaintext.
    ///
    /// The tag is computed over every field plus every slot's `CMT-4` tag, so the ledger binds the
    /// commitments that live in the slots. Returns a fixed-size array: a caller cannot produce a
    /// short directory, which makes `seal_directory`'s own length check unreachable rather than
    /// load-bearing.
    pub fn encode(
        &self,
        commitments: &[[u8; CMT4_TAG_BYTES]],
    ) -> Result<[u8; DIRECTORY_PLAINTEXT_BYTES], SealError> {
        if self.lengths.len() > PAYLOAD_SLOTS || commitments.len() != self.lengths.len() {
            return Err(SealError::TooManySlots {
                offered: self.lengths.len(),
                capacity: PAYLOAD_SLOTS,
            });
        }

        let mut out = [0u8; DIRECTORY_PLAINTEXT_BYTES];
        out[0..2].copy_from_slice(&DIRECTORY_VERSION.to_le_bytes());
        out[2] = self.stratum;
        out[3] = 0; // flags: reserved and zero, so an old reader cannot be fooled by a new bit
        out[4..12].copy_from_slice(&self.cursor.to_le_bytes());
        out[12..16].copy_from_slice(&(self.lengths.len() as u32).to_le_bytes());
        for (i, len) in self.lengths.iter().enumerate() {
            let at = HEADER_BYTES + i * LEN_BYTES;
            out[at..at + LEN_BYTES].copy_from_slice(&len.to_le_bytes());
        }
        let tag = Self::tag(&out[..TAG_OFFSET], commitments);
        out[TAG_OFFSET..TAG_OFFSET + LEDGER_TAG_BYTES].copy_from_slice(&tag);
        Ok(out)
    }

    /// Parse a directory, verifying the ledger tag against the commitments found in the slots.
    ///
    /// The tag is checked **before** any length is trusted, so a forged slot count cannot make the
    /// parser index outside the field.
    pub fn decode(
        field: &[u8; DIRECTORY_PLAINTEXT_BYTES],
        commitments: &[[u8; CMT4_TAG_BYTES]],
    ) -> Result<Self, DirectoryError> {
        let version = u16::from_le_bytes([field[0], field[1]]);
        if version != DIRECTORY_VERSION {
            return Err(DirectoryError::UnsupportedVersion { found: version });
        }
        if field[3] != 0 {
            return Err(DirectoryError::ReservedFlagsSet { flags: field[3] });
        }
        let stratum = field[2];
        if stratum == 0 || stratum as usize > MAX_STRATA {
            return Err(DirectoryError::BadStratum { found: stratum });
        }
        let mut cursor_bytes = [0u8; 8];
        cursor_bytes.copy_from_slice(&field[4..12]);
        let cursor = u64::from_le_bytes(cursor_bytes);
        let count = u32::from_le_bytes([field[12], field[13], field[14], field[15]]) as usize;
        if count > PAYLOAD_SLOTS {
            return Err(DirectoryError::TooManySlots { found: count });
        }
        if commitments.len() != count {
            return Err(DirectoryError::SlotCountMismatch {
                ledger: count,
                image: commitments.len(),
            });
        }

        // Tag first: nothing below is trusted until this holds.
        let expected = Self::tag(&field[..TAG_OFFSET], commitments);
        if expected != field[TAG_OFFSET..TAG_OFFSET + LEDGER_TAG_BYTES] {
            return Err(DirectoryError::LedgerTagMismatch);
        }

        let mut lengths = Vec::with_capacity(count);
        for i in 0..count {
            let at = HEADER_BYTES + i * LEN_BYTES;
            let len = u32::from_le_bytes([field[at], field[at + 1], field[at + 2], field[at + 3]]);
            // A slot's ciphertext is at most the slot minus the commitment stored beside it, and at
            // least one Poly1305 tag. Rejecting impossible lengths here means the reader never has
            // to.
            let max = (SLOT_BYTES - CMT4_TAG_BYTES) as u32;
            if len < 16 || len > max {
                return Err(DirectoryError::ImpossibleLength { slot: i, len });
            }
            lengths.push(len);
        }

        Ok(Self {
            stratum,
            cursor,
            lengths,
        })
    }

    /// BLAKE2b-256 over the index and the commitments it refers to.
    fn tag(index: &[u8], commitments: &[[u8; CMT4_TAG_BYTES]]) -> [u8; LEDGER_TAG_BYTES] {
        let mut h = Blake2b512::new();
        h.update(b"HOLONOMY_V51_LEDGER");
        h.update([0u8]);
        h.update((index.len() as u64).to_le_bytes());
        h.update(index);
        h.update((commitments.len() as u64).to_le_bytes());
        for c in commitments {
            h.update(c);
        }
        let d = h.finalize();
        let mut out = [0u8; LEDGER_TAG_BYTES];
        out.copy_from_slice(&d[..LEDGER_TAG_BYTES]);
        out
    }
}

/// Why a directory could not be opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryError {
    /// A version this build does not know.
    UnsupportedVersion {
        /// Version found.
        found: u16,
    },
    /// The reserved flags byte is not zero.
    ReservedFlagsSet {
        /// Byte found.
        flags: u8,
    },
    /// The stratum index is outside `1..=4`.
    BadStratum {
        /// Index found.
        found: u8,
    },
    /// The ledger claims more slots than a container has.
    TooManySlots {
        /// Count claimed.
        found: usize,
    },
    /// The ledger and the image disagree about how many slots exist.
    SlotCountMismatch {
        /// Count in the ledger.
        ledger: usize,
        /// Count found in the image.
        image: usize,
    },
    /// The commitment over the index and its slots did not match.
    LedgerTagMismatch,
    /// A slot length no real slot could have.
    ImpossibleLength {
        /// Which slot.
        slot: usize,
        /// Length claimed.
        len: u32,
    },
}

impl std::fmt::Display for DirectoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion { found } => write!(
                f,
                "directory version {found} is not supported by this build; newer than {DIRECTORY_VERSION}"
            ),
            Self::ReservedFlagsSet { flags } => write!(f, "reserved flags byte is {flags}, not 0"),
            Self::BadStratum { found } => {
                write!(f, "directory names stratum {found}, outside L_1..=L_4")
            }
            Self::TooManySlots { found } => write!(f, "directory claims {found} slots"),
            Self::SlotCountMismatch { ledger, image } => {
                write!(f, "directory claims {ledger} slots; the image holds {image}")
            }
            Self::LedgerTagMismatch => {
                f.write_str("ledger tag mismatch: the index or a slot was altered")
            }
            Self::ImpossibleLength { slot, len } => write!(
                f,
                "slot {slot} claims a {len}-byte ciphertext, which no slot can hold"
            ),
        }
    }
}

impl std::error::Error for DirectoryError {}

/// Every sealed slot's commitment, in logical order, as [`DirectoryLedger::encode`] wants them.
pub fn commitments_of(slots: &[SealedPayloadSlot]) -> Vec<[u8; CMT4_TAG_BYTES]> {
    slots.iter().map(|s| s.commitment).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(len: usize) -> SealedPayloadSlot {
        SealedPayloadSlot {
            ciphertext: vec![0x5Au8; len],
            commitment: [0x11u8; CMT4_TAG_BYTES],
            nonce: [0x22u8; NONCE_BYTES],
        }
    }

    #[test]
    fn a_full_container_fits() {
        // The behavioural half of the capacity claim: a ledger describing every payload slot the
        // container addresses must encode into the field and decode back unchanged, and one slot
        // past that capacity must be refused rather than wrapping or truncating.
        //
        // The arithmetic half -- that a 2,046-entry commitment table would overflow the field, which
        // is why this is a hash list -- is a `const _` assertion above, because two constants
        // compared at runtime is not a test anything can fail. Clippy caught that.
        let slots: Vec<_> = (0..PAYLOAD_SLOTS).map(|_| slot(64)).collect();
        let commits = commitments_of(&slots);
        let ledger = DirectoryLedger::new(2, 41, &slots);
        let field = ledger.encode(&commits).expect("a full index must fit");
        assert_eq!(field.len(), DIRECTORY_PLAINTEXT_BYTES);
        let back = DirectoryLedger::decode(&field, &commits).expect("round trip");
        assert_eq!(back, ledger);
        assert_eq!(back.slot_count(), PAYLOAD_SLOTS);
        assert_eq!(back.cursor, 41);

        let over: Vec<_> = (0..=PAYLOAD_SLOTS).map(|_| slot(64)).collect();
        assert!(DirectoryLedger::new(2, 0, &over)
            .encode(&commitments_of(&over))
            .is_err());
    }

    #[test]
    fn the_encoding_is_always_the_full_field_length() {
        // One slot and a hundred slots produce the same length, which is the point: a short
        // directory would disclose the document's size.
        let one = DirectoryLedger::new(1, 0, &[slot(32)]);
        let many: Vec<_> = (0..100).map(|_| slot(32)).collect();
        let a = one.encode(&[[0x11u8; CMT4_TAG_BYTES]]).unwrap();
        let b = DirectoryLedger::new(1, 0, &many)
            .encode(&commitments_of(&many))
            .unwrap();
        assert_eq!(a.len(), b.len());
        assert_eq!(a.len(), 60_592);
        assert!(a[DIRECTORY_MIN_BYTES..].iter().all(|&x| x == 0));
    }

    #[test]
    fn altering_a_slot_commitment_breaks_the_ledger() {
        let s = vec![slot(64), slot(64)];
        let mut commits = commitments_of(&s);
        let field = DirectoryLedger::new(2, 7, &s).encode(&commits).unwrap();
        commits[1][0] ^= 1;
        assert_eq!(
            DirectoryLedger::decode(&field, &commits).unwrap_err(),
            DirectoryError::LedgerTagMismatch
        );
    }

    #[test]
    fn the_tag_is_checked_before_any_length_is_trusted() {
        // Corrupt the slot count and leave the tag alone. The count must not be used to index: it
        // is compared against what the image actually holds, and the mismatch is the error.
        let s = vec![slot(64)];
        let commits = commitments_of(&s);
        let mut field = DirectoryLedger::new(2, 0, &s).encode(&commits).unwrap();
        field[12..16].copy_from_slice(&2_000u32.to_le_bytes());
        assert!(matches!(
            DirectoryLedger::decode(&field, &commits).unwrap_err(),
            DirectoryError::SlotCountMismatch {
                ledger: 2_000,
                image: 1
            }
        ));
    }

    #[test]
    fn an_impossible_length_is_refused() {
        let s = vec![slot(64)];
        let commits = commitments_of(&s);
        let mut field = DirectoryLedger::new(2, 0, &s).encode(&commits).unwrap();
        // Zero-length ciphertext, with the tag recomputed, so this exercises the length check and
        // not the tag check.
        field[HEADER_BYTES..HEADER_BYTES + 4].copy_from_slice(&0u32.to_le_bytes());
        let tag = DirectoryLedger::tag(&field[..TAG_OFFSET], &commits);
        field[TAG_OFFSET..TAG_OFFSET + LEDGER_TAG_BYTES].copy_from_slice(&tag);
        assert_eq!(
            DirectoryLedger::decode(&field, &commits).unwrap_err(),
            DirectoryError::ImpossibleLength { slot: 0, len: 0 }
        );
    }

    #[test]
    fn version_flags_and_stratum_are_all_refused_when_wrong() {
        let s = vec![slot(64)];
        let commits = commitments_of(&s);
        let good = DirectoryLedger::new(2, 0, &s).encode(&commits).unwrap();

        let mut v = good;
        v[0..2].copy_from_slice(&2u16.to_le_bytes());
        assert!(matches!(
            DirectoryLedger::decode(&v, &commits).unwrap_err(),
            DirectoryError::UnsupportedVersion { found: 2 }
        ));

        let mut f = good;
        f[3] = 1;
        assert!(matches!(
            DirectoryLedger::decode(&f, &commits).unwrap_err(),
            DirectoryError::ReservedFlagsSet { flags: 1 }
        ));

        let mut s0 = good;
        s0[2] = 0;
        assert!(matches!(
            DirectoryLedger::decode(&s0, &commits).unwrap_err(),
            DirectoryError::BadStratum { found: 0 }
        ));

        let mut s5 = good;
        s5[2] = 5;
        assert!(matches!(
            DirectoryLedger::decode(&s5, &commits).unwrap_err(),
            DirectoryError::BadStratum { found: 5 }
        ));
    }

    #[test]
    fn the_cursor_survives_the_round_trip() {
        // The cursor is the only thing in the container that says which pad window to use, and the
        // capsule has no room for it. Losing it in serialisation would make every container this
        // writer produces unreadable in a way no other test here would notice.
        let s = vec![slot(64)];
        let commits = commitments_of(&s);
        for cursor in [0u64, 1, 53, 54, u64::MAX] {
            let field = DirectoryLedger::new(3, cursor, &s).encode(&commits).unwrap();
            let back = DirectoryLedger::decode(&field, &commits).unwrap();
            assert_eq!(back.cursor, cursor);
            assert_eq!(back.stratum, 3);
        }
    }

    #[test]
    fn more_slots_than_the_container_has_is_refused_at_encode_time() {
        let s: Vec<_> = (0..=PAYLOAD_SLOTS).map(|_| slot(16)).collect();
        let commits = commitments_of(&s);
        let err = DirectoryLedger::new(1, 0, &s).encode(&commits).unwrap_err();
        assert!(err.to_string().contains("payload slots"), "{err}");
    }

    #[test]
    fn the_nonce_schedule_is_injective_over_the_plausible_history() {
        // The ledger does not store nonces; an opener rebuilds them from
        // `(N_epoch, cursor, slot, stratum)`. If two slots ever shared a nonce under one key, the
        // AEAD would still accept both ciphertexts and the confidentiality would be gone, so this
        // is asserted rather than assumed.
        let base = [0x77u8; NONCE_BYTES];
        let mut seen = std::collections::HashSet::new();
        for cursor in 0..holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE {
            for slot in 0..PAYLOAD_SLOTS as u32 {
                let n = holonomy_crypto::commit::slot_nonce(&base, cursor, slot, 2);
                assert!(seen.insert(n), "collision at cursor {cursor} slot {slot}");
            }
        }
        assert_eq!(
            seen.len() as u64,
            holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE * PAYLOAD_SLOTS as u64
        );
    }
}
