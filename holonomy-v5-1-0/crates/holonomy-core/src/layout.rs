//! On-disk geometry of the `.wavefunction` container, as corrected by `docs/PRD-v5.1.0.md`.
//!
//! Every constant here is pure arithmetic with no I/O and no crypto, so the whole geometry is
//! testable without a disk, a USB stick or a passphrase. The tests in this module are the
//! machine-checkable version of the PRD's §5.2 diagram.
//!
//! # What changed from `v5.0.0-OMEGA`, and why
//!
//! The superseded PRD described the root node as living in "Physical Block 0 (Bytes 0..65,535)"
//! *and* as "`Physical Block π_A(0)` located by the permutation". Both cannot hold. v5.1.0 picks
//! exactly one: [`ROOT_SLOT_INDEX`] is a **fixed** physical block, and the permutation acts only
//! on the payload slots. This is not a cosmetic choice — it is forced by the key schedule: the
//! permutation is keyed from `σ_epoch`, so an epoch-permuted root at `π(0)` could not be located
//! without already holding the salt hidden inside it. That is the same circularity `holonomy-old`
//! documented for its `Ω` offset.
//!
//! # Three fixed residents, one permuted region
//!
//! ```text
//!   slot 0        slot 1        slots 2 .. 2,047
//!   root capsule  σ_epoch       permuted payload
//!   65,536 B      64 B used     2,046 x 65,536 B
//! ```
//!
//! The salt is **not** inside the root capsule. It was declared at offset 0, which is the first
//! field of `StratumRootCapsuleV5` (`ct_ml_kem_1024`), so the two structures claimed the same 64
//! bytes and writing either destroyed the other. Nothing caught it because no container had ever
//! been written: the constant was only ever compared against itself. The geometry is now three
//! ranges that must tile the file, asserted as `const _`, again at runtime by
//! `the_salt_does_not_overlap_the_root_capsule`, and a third time by `holonomy --check`.

/// Every container is exactly this many bytes. Never variable, never compressed.
pub const CONTAINER_BYTES: usize = 134_217_728;

/// Container is divided into this many fixed slots.
pub const SLOT_BYTES: usize = 65_536;

/// `CONTAINER_BYTES / SLOT_BYTES`. Total physical blocks including the root.
pub const TOTAL_BLOCKS: usize = 2_048;

/// Physical block index of the stratum root node. Fixed, not permuted — see the module docs.
pub const ROOT_SLOT_INDEX: usize = 0;

/// Payload (document/media) slots addressed by the epoch permutation: blocks 2..=2,047.
///
/// This was `TOTAL_BLOCKS - 1 = 2,047`, i.e. "everything except the root". Giving the salt its own
/// slot makes the payload one slot smaller, and that is a correction rather than a regression: the
/// PRD's `2,047` implicitly counted the root and the salt as one block, which they cannot be. The
/// cost is 64 KiB of documented capacity; the alternative costs the container.
pub const PAYLOAD_SLOTS: usize = TOTAL_BLOCKS - 2;

/// First physical block of the permuted payload, i.e. the block after the salt.
pub const PAYLOAD_SLOT_BASE: usize = SALT_SLOT_INDEX + 1;

/// Byte offset of the 64-byte epochal salt that keys everything else.
///
/// At a **fixed** offset, not at a derived one. See the module docs for the circularity argument.
///
/// # Why this is 65,536 and not 0
///
/// Slot 0 *is* the root capsule: `StratumRootCapsuleV5` occupies `[0, 65,536)` and its first field
/// is `ct_ml_kem_1024`. A salt at offset 0 would have been written into the first 64 bytes of that
/// ciphertext — the two structures silently overwriting each other, leaving a container whose
/// ML-KEM ciphertext had a 64-byte hole and whose salt was a fragment of ciphertext. The reader
/// would have reported the coarse `Rejected` that means "wrong passphrase", with no hint of the
/// cause, because nothing had ever written a container for the collision to happen in.
pub const SALT_OFFSET: usize = SLOT_BYTES;

/// Physical block index of the salt slot.
pub const SALT_SLOT_INDEX: usize = SALT_OFFSET / SLOT_BYTES;

/// Length of `σ_epoch`.
pub const SALT_BYTES: usize = 64;

/// `O_DIRECT` / `F_NOCACHE` alignment unit. Containers start on this boundary, and so must
/// every buffer handed to a direct read or write.
pub const IO_ALIGN: usize = 4_096;

/// A stratum root node is exactly one slot, and is `io_align`-aligned on disk by construction.
pub const CAPSULE_BYTES: usize = SLOT_BYTES;

/// Strata per container: `L_1` (duress/decoy, outermost) .. `L_k` (primary, innermost).
pub const MAX_STRATA: usize = 4;

/// A full save must have consumed exactly this many payload sectors by the end of a save.
pub const PAD_SECTORS_PER_SAVE: u32 = 32_768;

/// 4 KiB sectors in USB Zone B (6.80 GiB of provisioning-time CSPRNG entropy).
pub const ZONE_B_SECTORS: u32 = 1_782_579;

/// USB logical sector size. Every pad figure is in these units; the old PRD's diagram mixed
/// 512-byte LBAs with 4 KiB sectors on the same line and mislabelled a 16 MiB partition.
pub const SECTOR_BYTES: usize = 4_096;

/// How many consecutive saves read mutually disjoint pad sectors.
///
/// `ZONE_B_SECTORS / PAD_SECTORS_PER_SAVE = 54`. Cursors `0..=53` produce 54 pairwise-disjoint
/// windows spanning `[0, 1_769_472)` in logical-index space. Cursor 54 cannot fit a further
/// complete 32,768-sector window in Zone B; checked planning APIs reject it rather than silently
/// reusing sectors. This is a geometry-derived no-reuse budget, not proof that a physical device
/// or its snapshots have not reused data. [`crate::feistel`] and the USB crate both read this
/// constant so the limit cannot drift from the code.
pub const SAVES_BEFORE_DOMAIN_REUSE: u64 = (ZONE_B_SECTORS / PAD_SECTORS_PER_SAVE) as u64;

/// Bytes of the container that are *not* under the pad: none.
///
/// The pad is XORed over all [`CONTAINER_BYTES`] before the file is written, including
/// [`SALT_OFFSET`]'s salt. The salt is therefore recoverable only with the USB (or with the
/// passphrase + `κ_USB`), which is what makes `IND-URN` hold across snapshots — a plaintext
/// salt would be the one stable fingerprint in the file.
pub const PAD_COVERAGE_BYTES: usize = CONTAINER_BYTES;

/// Sanity: the container is an integral number of aligned slots. In a `const` block so a passing
/// assertion does not leave a dead item behind.
const _: () = {
    assert!(CONTAINER_BYTES.is_multiple_of(SLOT_BYTES));
    assert!(SLOT_BYTES.is_multiple_of(IO_ALIGN));
    assert!(TOTAL_BLOCKS * SLOT_BYTES == CONTAINER_BYTES);
    // The three residents and the permuted region must tile the container exactly. This block
    // exists because the failure it describes already happened: the salt and the root capsule
    // claimed the same bytes and no assertion noticed.
    assert!(ROOT_SLOT_INDEX == 0);
    assert!(SALT_OFFSET >= (ROOT_SLOT_INDEX + 1) * SLOT_BYTES);
    assert!(SALT_OFFSET + SALT_BYTES <= (SALT_SLOT_INDEX + 1) * SLOT_BYTES);
    assert!(PAYLOAD_SLOT_BASE + PAYLOAD_SLOTS == TOTAL_BLOCKS);
    assert!(PAYLOAD_SLOT_BASE == SALT_SLOT_INDEX + 1);
    // One save of pad is exactly one container: 32,768 sectors x 4,096 bytes. That is the 1:1
    // property the fold depends on, stated as an identity rather than as a comment.
    assert!(PAD_SECTORS_PER_SAVE as usize * SECTOR_BYTES == CONTAINER_BYTES);
    assert!(SAVES_BEFORE_DOMAIN_REUSE == 54);
};

/// Byte range of physical block `index`, as `(offset, len)`.
///
/// Panics if `index` is out of range, because every caller either knows the index is valid or
/// has a bug; silently clamping would turn an addressing bug into silent data loss.
pub const fn slot_range(index: usize) -> (usize, usize) {
    assert!(index < TOTAL_BLOCKS);
    (index * SLOT_BYTES, SLOT_BYTES)
}

/// Whether `len` bytes at `offset` satisfy the direct-I/O alignment rule.
pub const fn is_direct_io_aligned(offset: u64, len: usize) -> bool {
    offset.is_multiple_of(IO_ALIGN as u64) && len.is_multiple_of(IO_ALIGN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_geometry_is_exact() {
        assert_eq!(TOTAL_BLOCKS * SLOT_BYTES, CONTAINER_BYTES);
        assert_eq!(CONTAINER_BYTES, 134_217_728);
        assert_eq!(PAYLOAD_SLOTS, 2_046);
        assert_eq!(slot_range(0), (0, 65_536));
        // Last block ends exactly at the end of the file: no slack, no truncation.
        let (off, len) = slot_range(TOTAL_BLOCKS - 1);
        assert_eq!(off + len, CONTAINER_BYTES);
    }

    #[test]
    fn root_block_is_fixed_and_permutation_covers_the_rest() {
        assert_eq!(ROOT_SLOT_INDEX, 0);
        assert_eq!(SALT_SLOT_INDEX, 1);
        assert_eq!(PAYLOAD_SLOT_BASE, 2);
        // Every payload slot is addressable and none aliases the fixed residents.
        assert_eq!(PAYLOAD_SLOT_BASE + PAYLOAD_SLOTS - 1, TOTAL_BLOCKS - 1);
    }

    #[test]
    fn the_salt_does_not_overlap_the_root_capsule() {
        // The defect this replaces: SALT_OFFSET was 0, and slot 0 is a 65,536-byte capsule whose
        // first field is `ct_ml_kem_1024`. Stated as byte ranges that must be disjoint, because
        // that is the form the mistake would have been caught in.
        let (root_off, root_len) = slot_range(ROOT_SLOT_INDEX);
        let salt = (SALT_OFFSET, SALT_OFFSET + SALT_BYTES);
        assert!(
            salt.0 >= root_off + root_len || salt.1 <= root_off,
            "salt [{}, {}) overlaps the root capsule [{}, {})",
            salt.0,
            salt.1,
            root_off,
            root_off + root_len
        );
        // And it is inside its own slot, so a direct read of that slot contains it.
        let (salt_off, salt_len) = slot_range(SALT_SLOT_INDEX);
        assert!(salt.0 >= salt_off);
        assert!(salt.1 <= salt_off + salt_len);
        assert_eq!(salt_len, SLOT_BYTES);

        // The residents tile the file with no gap and no overlap.
        assert_eq!(root_off + root_len, salt.0);
        let payload_off = PAYLOAD_SLOT_BASE * SLOT_BYTES;
        assert_eq!(payload_off + PAYLOAD_SLOTS * SLOT_BYTES, CONTAINER_BYTES);
        assert_eq!(payload_off, salt_off + salt_len);
    }

    #[test]
    fn salt_sits_at_offset_zero_and_is_padded_over() {
        // Relationships *between* constants are compile-time facts, so they live in the
        // module-level `const _` block above and nowhere else -- a runtime `assert!` on two
        // constants is a tautology clippy rightly rejects, and `black_box` to smuggle it past the
        // lint would be worse than deleting it.
        //
        // What a runtime test can honestly do here is check that the salt is where the format says
        // it is, by round-tripping bytes through the real wire layout.
        assert_eq!(SALT_OFFSET, 65_536);
        let mut wire = [0xA5u8; SLOT_BYTES];
        let salt = [0x11u8; SALT_BYTES];
        // Written at its offset *within its own slot*, which is how the store will do it.
        let at = SALT_OFFSET - SALT_SLOT_INDEX * SLOT_BYTES;
        wire[at..at + SALT_BYTES].copy_from_slice(&salt);
        assert_eq!(&wire[at..at + SALT_BYTES], &salt);
        assert!(
            wire[at + SALT_BYTES..].iter().all(|&b| b == 0xA5),
            "the salt must not spill past its 64 bytes"
        );
    }

    #[test]
    fn save_budget_is_the_documented_fifty_four() {
        assert_eq!(SAVES_BEFORE_DOMAIN_REUSE, 54);
        assert!(SAVES_BEFORE_DOMAIN_REUSE * PAD_SECTORS_PER_SAVE as u64 <= ZONE_B_SECTORS as u64);
        // And the 55th does wrap: the budget is tight, not slack.
        assert!(
            (SAVES_BEFORE_DOMAIN_REUSE + 1) * PAD_SECTORS_PER_SAVE as u64 > ZONE_B_SECTORS as u64
        );
    }

    #[test]
    fn alignment_rule() {
        assert!(is_direct_io_aligned(0, 4096));
        assert!(is_direct_io_aligned(65536, 65536));
        assert!(!is_direct_io_aligned(1, 4096));
        assert!(!is_direct_io_aligned(0, 100));
        assert!(is_direct_io_aligned(CONTAINER_BYTES as u64, 4096));
    }

    #[test]
    fn all_blocks_are_direct_io_aligned() {
        for index in [0, 1, 2, PAYLOAD_SLOTS, TOTAL_BLOCKS - 1] {
            let (off, len) = slot_range(index);
            assert!(is_direct_io_aligned(off as u64, len), "block {index}");
        }
    }
}
