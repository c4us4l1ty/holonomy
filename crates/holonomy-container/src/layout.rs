//! On-disk geometry of the `.wavefunction` container.
//!
//! Everything here is pure arithmetic over the constants, with no I/O and no crypto, so
//! every offset rule can be tested exhaustively without touching a disk.
//!
//! ```text
//! 0                32                      Ω                              134,217,728
//! ├────────────────┼───────────────────────┼──────────────────────────────┤
//! │ master salt    │ chaff (K_chaff)        │ payload: chunk 0, 1 .. N     │ chaff
//! │ (32 B clear)   │                       │ each slot 65,536 B on disk   │
//! └────────────────┴───────────────────────┴──────────────────────────────┘
//! ```
//!
//! # Why Ω has to be aligned, which the PRD does not say
//!
//! FR-2.3.3 defines `Ω = Read_U64_LE(OffsetBytes) mod (134,217,728 − S_max_payload)` and
//! stops there. Taken literally that is an arbitrary byte offset in `[0, 2^64)`, and FR-4.7
//! requires every access to be `O_DIRECT`. Those two requirements are incompatible:
//! `O_DIRECT` requires the *file offset* of each transfer to be a multiple of the logical
//! block size, and on this filesystem that is 4096. An arbitrary Ω makes every single read
//! and write fail with `EINVAL`.
//!
//! So Ω is aligned down to [`CHUNK_SLOT`]. That is enough rather than merely necessary:
//! chunk `i` lives at `Ω + i·CHUNK_SLOT`, and since `CHUNK_SLOT` is itself a multiple of
//! 4096, aligning Ω to `CHUNK_SLOT` makes every chunk address aligned too. Aligning to 4096
//! would *not* be enough, because `Ω + CHUNK_SLOT` would inherit Ω's 4096-alignment only
//! if `CHUNK_SLOT` were also 4096-aligned — which it is, but the point is that aligning
//! Ω to the chunk slot is what makes the whole arithmetic chain come out aligned.
//!
//! The cost is that Ω has ~1920 possible values instead of 2^57. That is not a secret we
//! need to protect. An adversary who knows the passphrase still cannot read the payload
//! without `K_enc`; an adversary without it cannot test a candidate Ω by reading, because
//! the payload region is XChaCha20 ciphertext under a key they do not have and the chaff
//! around it is unauthenticated. Ω being guessable costs nothing; Ω being *unreadable* is
//! what FR-4.1 needs, and alignment does not change that.
//!
//! # The salt sits at offset 0, not at Ω
//!
//! Ω is derived *from* the passphrase, so a salt stored at Ω could not be read without
//! first deriving the key you are trying to derive. Fixed offset 0 is the only coherent
//! choice. It does not weaken IND-URN: 32 CSPRNG bytes are indistinguishable from noise,
//! and FR-4.1 forbids *magic numbers and headers*, not random bytes.
//!
//! # The chunk slot is 65,536 bytes and the tag lives inside it
//!
//! PRD §5.4 describes "64 KiB block + 16-byte appended Poly1305 tag" = 65,552 bytes on
//! disk. 65,552 is not a multiple of 512, let alone 4096, so that shape cannot be written
//! with `O_DIRECT` either. Taking 65,536 as the *on-disk* unit and putting the tag in the
//! last 16 bytes of it costs 16 bytes of plaintext per chunk and keeps every transfer
//! aligned. [`CHUNK_PLAINTEXT`] is the resulting payload size.

/// The container is exactly this many bytes, always. FR-4.1 / FR-2.1.1.
pub const CONTAINER_SIZE: u64 = 134_217_728;

/// Length of the master Argon2id salt. 32 bytes, at [`SALT_OFFSET`].
pub const SALT_LEN: usize = 32;

/// Where the master salt lives. Fixed, and *not* Ω — see the module docs.
pub const SALT_OFFSET: u64 = 0;

/// Length of the recorded VDF iteration count. 8 bytes, at [`VDF_ITERATIONS_OFFSET`].
pub const VDF_ITERATIONS_LEN: usize = 8;

/// Where the recorded VDF iteration count lives, immediately after the salt.
///
/// # Why it is here and not in the master frame
///
/// **The master frame records `vdf_iterations` and it is useless there.** The frame is chunk 0, chunk 0 is
/// sealed under a key derived *from the VDF*, and the VDF's cost is what is in question — so reading the
/// recorded value requires having already run the computation whose parameters you are trying to learn.
/// That is a circle, and it is why `MasterFrame::with_kdf_params` carries the comment *"purely a record;
/// nothing reads it back to derive anything."*
///
/// Page 0 is the one region that is **plaintext**, because the salt has to be readable before any key
/// exists. Putting `T` beside it is the only place a container can state its own derivation cost and have
/// that statement available at the moment the cost is about to be paid.
///
/// **Little-endian**, matching `Omega`'s convention (FR-2.3.3) and for the same reason: both are read out of
/// a fixed plaintext page rather than a serialised structure, and byte order is a bijection either way, so
/// the argument is conformance and not correctness.
///
/// **Zero means "not recorded."** A container written before this field existed has whatever the chaff
/// generator put at this offset, which is not reliably zero — so the value is only *trusted* when it is
/// non-zero **and** the salt beside it decrypted the container. See
/// [`Wavefunction::open`](crate::Wavefunction::open) for the check, which is the part that matters.
pub const VDF_ITERATIONS_OFFSET: u64 = SALT_LEN as u64;

/// Alignment required of every `O_DIRECT` transfer: the file offset and the buffer address.
pub const IO_ALIGN: u64 = 4096;

/// On-disk bytes per chunk: ciphertext followed by the 16-byte Poly1305 tag.
pub const CHUNK_SLOT: u64 = 65_536;

/// Poly1305 tag length. FR-2.3.1.
pub const TAG_LEN: usize = 16;

/// Usable plaintext per chunk: the slot minus the tag it carries.
pub const CHUNK_PLAINTEXT: usize = (CHUNK_SLOT - TAG_LEN as u64) as usize;

/// The 3-stage ring holds `[N−1, N, N+1]`, three slots, 192 KiB.
pub const RING_STAGES: usize = 3;

/// Total resident bytes the ring is allowed. FR-3.1 / PRD's "192 KiB 3-stage ring".
pub const RING_BYTES: usize = RING_STAGES * CHUNK_SLOT as usize;

/// Upper bound on the encrypted payload, which is what Ω is computed to leave room for.
///
/// A deliberate cap and not an incidental one: the smaller the payload fraction of the
/// file, the closer the container is to pure CSPRNG output, which is what FR-2.1.3's
/// entropy test measures. 8 MiB of plaintext is roughly 8 million characters, far more
/// than a word processor document, and it leaves 120 MiB of noise.
pub const S_MAX_PAYLOAD: u64 = 8 * 1024 * 1024;

/// Chunk 0 holds the master frame; chunks 1..N hold content. PRD §5.4.
pub const MASTER_FRAME_CHUNK: u64 = 0;

/// Total number of chunks the container can address.
pub const MAX_CHUNKS: u64 = CONTAINER_SIZE / CHUNK_SLOT;

// The geometry has to hold or every offset below is a lie. Checked at compile time so a
// future edit to any constant breaks the build rather than producing a corrupt container.
const _: () = {
    assert!(
        CONTAINER_SIZE.is_multiple_of(CHUNK_SLOT),
        "container must divide into whole slots"
    );
    assert!(
        CHUNK_SLOT.is_multiple_of(IO_ALIGN),
        "a slot must be an exact number of I/O units"
    );
    assert!(
        IO_ALIGN.is_multiple_of(512),
        "I/O unit must satisfy the strictest sector size"
    );
    assert!(
        SALT_LEN as u64 <= CHUNK_SLOT,
        "salt must fit in the reserved head"
    );
    assert!(CHUNK_PLAINTEXT + TAG_LEN == CHUNK_SLOT as usize);
    assert!(
        S_MAX_PAYLOAD < CONTAINER_SIZE - CHUNK_SLOT,
        "payload cap must leave at least one slot of head chaff"
    );
    assert!(
        CHUNK_SLOT.is_multiple_of(IO_ALIGN),
        "Ω alignment must make chunk addresses aligned"
    );
};

/// Number of distinct aligned Ω values, for documentation and tests.
///
/// One fewer than `(CONTAINER_SIZE - S_MAX_PAYLOAD) / CHUNK_SLOT`, because the Ω = 0 slot
/// is claimed by the salt and gets folded up into the first legal slot. Counted by
/// `omega_spans_the_container` rather than assumed, because the two silently disagreeing
/// is how a test ends up asserting something false.
pub const OMEGA_POSITIONS: u64 = (CONTAINER_SIZE - S_MAX_PAYLOAD) / CHUNK_SLOT - 1;

/// Derive Ω from the eight little-endian HKDF offset bytes. FR-2.3.3, plus the alignment
/// rule from the module docs.
///
/// The `if aligned < CHUNK_SLOT` clamp is not dead code: `offset_bytes % span` is zero for
/// roughly one input in 1920, and Ω = 0 would put the payload on top of the salt. Guarding
/// it matters more than the bias it introduces.
pub fn omega(offset_bytes: u64) -> u64 {
    let span = CONTAINER_SIZE - S_MAX_PAYLOAD;
    let raw = offset_bytes % span;
    let aligned = raw & !(CHUNK_SLOT - 1);
    if aligned < CHUNK_SLOT {
        CHUNK_SLOT
    } else {
        aligned
    }
}

/// File offset of chunk `index`, given Ω.
pub fn chunk_offset(omega: u64, index: u64) -> u64 {
    omega + index * CHUNK_SLOT
}

/// Whether `payload_len` bytes of payload fit at `omega`.
pub fn payload_fits(omega: u64, payload_len: u64) -> bool {
    payload_len <= S_MAX_PAYLOAD
        && omega
            .checked_add(payload_len)
            .is_some_and(|end| end <= CONTAINER_SIZE)
}

/// Bytes of payload a container with `chunks` chunks occupies on disk.
pub fn payload_len(chunks: u64) -> u64 {
    chunks * CHUNK_SLOT
}

/// Bytes needed to hold `plaintext_len` bytes of content, including the master frame.
pub fn chunks_for(plaintext_len: u64) -> Result<u64, LayoutError> {
    let content = plaintext_len.div_ceil(CHUNK_PLAINTEXT as u64);
    let chunks = content + 1; // + the master frame
    if payload_len(chunks) > S_MAX_PAYLOAD {
        return Err(LayoutError::PayloadTooLarge {
            requested: plaintext_len,
            cap: S_MAX_PAYLOAD,
        });
    }
    Ok(chunks)
}

/// A layout rule was violated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    /// The content does not fit under [`S_MAX_PAYLOAD`].
    PayloadTooLarge {
        /// What was asked for.
        requested: u64,
        /// The cap.
        cap: u64,
    },
    /// A chunk index was addressed that does not exist in this container.
    ChunkOutOfRange {
        /// The index asked for.
        index: u64,
        /// How many chunks exist.
        chunks: u64,
    },
    /// An offset was not a multiple of [`IO_ALIGN`], so `O_DIRECT` would reject it.
    UnalignedOffset {
        /// The offending offset.
        offset: u64,
    },
}

impl core::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::PayloadTooLarge { requested, cap } => {
                write!(f, "payload {requested} exceeds cap {cap}")
            }
            Self::ChunkOutOfRange { index, chunks } => {
                write!(f, "chunk {index} out of range for {chunks} chunks")
            }
            Self::UnalignedOffset { offset } => {
                write!(f, "offset {offset} is not {IO_ALIGN}-aligned")
            }
        }
    }
}

impl std::error::Error for LayoutError {}

/// Check that an offset is usable for `O_DIRECT`.
pub fn require_aligned(offset: u64) -> Result<(), LayoutError> {
    if offset.is_multiple_of(IO_ALIGN) {
        Ok(())
    } else {
        Err(LayoutError::UnalignedOffset { offset })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The container size is not negotiable. 128 MiB exactly.
    #[test]
    fn container_is_exactly_128_mib() {
        assert_eq!(CONTAINER_SIZE, 134_217_728);
        assert_eq!(CONTAINER_SIZE, 128 * 1024 * 1024);
    }

    /// The ring is 192 KiB, which is the figure PRD and NFR-3.1 both quote.
    #[test]
    fn ring_is_three_slots_and_192_kib() {
        assert_eq!(RING_STAGES, 3);
        assert_eq!(RING_BYTES, 196_608);
        assert_eq!(RING_BYTES, 192 * 1024);
    }

    /// The tag lives inside the slot, so slot + tag must not be added.
    #[test]
    fn chunk_slot_carries_its_own_tag() {
        assert_eq!(CHUNK_SLOT, 65_536);
        assert_eq!(CHUNK_PLAINTEXT, 65_520);
        assert_eq!(CHUNK_PLAINTEXT + TAG_LEN, CHUNK_SLOT as usize);
    }

    /// Every chunk address must be `O_DIRECT`-legal, which is the whole reason Ω is
    /// aligned. Checked at many Ω values, including every boundary.
    #[test]
    fn every_chunk_address_is_io_aligned() {
        for seed in [0u64, 1, 65_535, 65_536, 65_537, 1 << 40, u64::MAX] {
            let om = omega(seed);
            assert_eq!(om % CHUNK_SLOT, 0, "omega {om} must be slot-aligned");
            require_aligned(om).expect("omega must be I/O aligned");
            for index in [0u64, 1, 2, 127, 1919] {
                require_aligned(chunk_offset(om, index)).unwrap_or_else(|e| {
                    panic!("chunk {index} at omega {om} is unusable: {e}");
                });
            }
        }
    }

    /// Ω must never land on the salt.
    #[test]
    fn omega_never_overlaps_the_salt() {
        // offset_bytes = 0 gives raw = 0, which would be Ω = 0 if not for the clamp.
        assert_eq!(omega(0), CHUNK_SLOT);
        for seed in 0..4096u64 {
            assert!(
                omega(seed) >= CHUNK_SLOT,
                "omega {} hit the salt",
                omega(seed)
            );
        }
    }

    /// Ω must leave room for a full payload at every position, including the largest.
    #[test]
    fn every_omega_leaves_room_for_a_full_payload() {
        assert!(payload_fits(omega(0), S_MAX_PAYLOAD));
        assert!(payload_fits(omega(u64::MAX), S_MAX_PAYLOAD));
        // The cap itself is enforced.
        assert!(!payload_fits(omega(0), S_MAX_PAYLOAD + 1));
    }

    /// Ω must be a pure function of the offset bytes: the same passphrase must always
    /// resolve the same location, or a document would become unopenable.
    #[test]
    fn omega_is_deterministic_and_input_separated() {
        let seeds: Vec<u64> = (0..2000)
            .map(|i| (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
            .collect();
        let first: Vec<u64> = seeds.iter().map(|&s| omega(s)).collect();
        let second: Vec<u64> = seeds.iter().map(|&s| omega(s)).collect();
        assert_eq!(first, second);
        // Different offset bytes should land in different places almost always. 2000 draws
        // from ~1920 slots: collisions are expected, but they should not be universal.
        let mut distinct = std::collections::HashSet::new();
        for &o in &first {
            distinct.insert(o);
        }
        assert!(
            distinct.len() > first.len() / 2,
            "only {} distinct offsets from {} draws",
            distinct.len(),
            first.len()
        );
    }

    /// Duress requires Ω_A ≠ Ω_B. Two different passphrases must not collide in practice.
    ///
    /// The offsets are a slot apart rather than adjacent, which matters: Ω is aligned down to
    /// [`CHUNK_SLOT`], so its low 16 bits are always zero and two offset values less than
    /// 64 KiB apart land on the *same* Ω. `omega_aliases_below_the_alignment` pins that; this
    /// test checks the property that actually matters, which is that independent offsets
    /// give independent offsets.
    #[test]
    fn distinct_offsets_give_distinct_omegas() {
        // Far enough apart that alignment cannot merge them: see omega_aliases_below_the_
        // alignment, which pins that Ω's low 16 bits are discarded.
        let a = omega(0x0123_4567_89AB_CDEF);
        let b = omega(0x0123_4567_89CD_0000);
        assert!(
            (a as i64 - b as i64).abs() >= CHUNK_SLOT as i64,
            "test inputs are too close to distinguish"
        );
        assert_ne!(a, b);
        assert_eq!(a % CHUNK_SLOT, 0, "both are slot aligned");
        assert_eq!(b % CHUNK_SLOT, 0, "both are slot aligned");
    }

    /// Ω alignment means the low 16 bits of the offset are discarded, so offsets within the
    /// same 64 KiB slot alias.
    ///
    /// This is a real property of the format and it is not a weakness: an adversary who
    /// guesses Ω still cannot read anything, because the payload is XChaCha20-Poly1305
    /// ciphertext under a key derived from the passphrase and the chaff around it is
    /// unauthenticated. Alignment costs entropy in a quantity that is not secret and does
    /// not leak the key. Pinned so a future change that tries to make Ω finer-grained
    /// without fixing `O_DIRECT` alignment fails visibly rather than silently.
    #[test]
    fn omega_aliases_below_the_alignment() {
        let base = 0x0123_4567_0000_0000u64;
        assert_eq!(omega(base), omega(base + 1));
        assert_eq!(omega(base), omega(base + 0xFFFF));
        // A whole slot away is a different Ω.
        assert_ne!(omega(base), omega(base + CHUNK_SLOT));
    }

    /// Ω must cover most of the file, not a corner of it — otherwise the chaff and the
    /// payload are trivially separable by a histogram.
    #[test]
    fn omega_spans_the_container() {
        let mut seen = std::collections::HashSet::new();
        let mut min = u64::MAX;
        let mut max = 0;
        for i in 0..20_000u64 {
            let om = omega(i.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            seen.insert(om);
            min = min.min(om);
            max = max.max(om);
        }
        assert!(min <= CHUNK_SLOT, "offsets must reach near the front");
        assert!(
            max >= CONTAINER_SIZE - S_MAX_PAYLOAD - CHUNK_SLOT,
            "and near the back"
        );
        assert_eq!(
            seen.len() as u64,
            OMEGA_POSITIONS,
            "all positions are reachable"
        );
    }

    /// Chunk arithmetic.
    #[test]
    fn chunk_offsets_are_contiguous_and_in_range() {
        let om = omega(0xDEAD_BEEF_CAFE);
        assert_eq!(chunk_offset(om, 0), om);
        assert_eq!(chunk_offset(om, 1), om + CHUNK_SLOT);
        assert_eq!(chunk_offset(om, 2), om + 2 * CHUNK_SLOT);
        // The last slot in the container ends exactly at the end of the file.
        assert_eq!(chunk_offset(0, MAX_CHUNKS - 1) + CHUNK_SLOT, CONTAINER_SIZE);
    }

    /// `chunks_for` includes the master frame, which is the easy thing to forget.
    #[test]
    fn chunks_for_accounts_for_the_master_frame() {
        assert_eq!(chunks_for(0).expect("empty"), 1);
        assert_eq!(chunks_for(1).expect("one byte"), 2);
        assert_eq!(chunks_for(CHUNK_PLAINTEXT as u64).expect("exact fit"), 2);
        assert_eq!(chunks_for(CHUNK_PLAINTEXT as u64 + 1).expect("one over"), 3);
    }

    /// The cap is enforced by `chunks_for`, not merely documented.
    #[test]
    fn chunks_for_enforces_the_payload_cap() {
        let too_big = S_MAX_PAYLOAD;
        assert_eq!(
            chunks_for(too_big),
            Err(LayoutError::PayloadTooLarge {
                requested: too_big,
                cap: S_MAX_PAYLOAD,
            })
        );
        // One byte under the cap still fits, and the master frame is included.
        let just_fits = chunks_for(S_MAX_PAYLOAD - CHUNK_SLOT - CHUNK_PLAINTEXT as u64)
            .expect("just under the cap");
        assert!(payload_len(just_fits) <= S_MAX_PAYLOAD);
    }

    /// Unaligned offsets are rejected loudly rather than producing an `EINVAL` from the
    /// kernel with no context.
    #[test]
    fn unaligned_offsets_are_rejected() {
        assert!(require_aligned(0).is_ok());
        assert!(require_aligned(4096).is_ok());
        assert!(require_aligned(65536).is_ok());
        assert_eq!(
            require_aligned(1),
            Err(LayoutError::UnalignedOffset { offset: 1 })
        );
        assert_eq!(
            require_aligned(65537),
            Err(LayoutError::UnalignedOffset { offset: 65_537 })
        );
    }

    /// MAX_CHUNKS must not be reachable in practice: S_MAX_PAYLOAD caps it far lower.
    #[test]
    fn max_chunks_is_an_addressing_bound_not_a_capacity_one() {
        assert_eq!(MAX_CHUNKS, 2048);
        const {
            assert!(S_MAX_PAYLOAD / CHUNK_SLOT < MAX_CHUNKS);
        }
    }
}
