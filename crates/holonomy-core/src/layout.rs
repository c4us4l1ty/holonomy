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
//! on the 2,047 payload slots. This is not a cosmetic choice — it is forced by the key schedule.
//! The container's first 64 bytes are [`SALT_OFFSET`]'s `σ_epoch`, and the permutation is keyed
//! from `σ_epoch`, so an epoch-permuted root at `π(0)` could not be located without already
//! holding the salt hidden inside it. That is the same circularity `holonomy-old` documented for
//! its `Ω` offset, and it is why the 2,047 in `docs/PRD-v5.1.0.md` stays 2,047 rather than 2,048.

/// Every container is exactly this many bytes. Never variable, never compressed.
pub const CONTAINER_BYTES: usize = 134_217_728;

/// Container is divided into this many fixed slots.
pub const SLOT_BYTES: usize = 65_536;

/// `CONTAINER_BYTES / SLOT_BYTES`. Total physical blocks including the root.
pub const TOTAL_BLOCKS: usize = 2_048;

/// Physical block index of the stratum root node. Fixed, not permuted — see the module docs.
pub const ROOT_SLOT_INDEX: usize = 0;

/// Payload (document/media) slots addressed by the epoch permutation: blocks 1..=2,047.
pub const PAYLOAD_SLOTS: usize = TOTAL_BLOCKS - 1;

/// Byte offset of the 64-byte epochal salt that keys everything else.
///
/// At a **fixed** offset, not at a derived one. See the module docs for the circularity argument.
pub const SALT_OFFSET: usize = 0;

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

/// How many consecutive saves read mutually disjoint pad sectors.
///
/// `ZONE_B_SECTORS / PAD_SECTORS_PER_SAVE = 54`. Epochs `0..=53` consume
/// `[0, 1_769_472)` and are pairwise disjoint; epoch 54 is the first that wraps past the end of
/// Zone B and begins reusing pad material.
///
/// This is the number the fine print is about. It is a *budget*, not a guarantee: after 54 saves
/// without a ratchet, pad reuse is real, and the confidential payload is protected from then on
/// by the post-quantum layer (`ML-KEM-1024` + `X25519` + `XChaCha20-Poly1305` with `CMT-4`
/// commitment) rather than by the pad. [`crate::feistel`] and the USB crate both read this
/// constant so the statement cannot drift from the code.
pub const SAVES_BEFORE_DOMAIN_REUSE: u64 =
    (ZONE_B_SECTORS / PAD_SECTORS_PER_SAVE) as u64;

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
    assert!(CONTAINER_BYTES % SLOT_BYTES == 0);
    assert!(SLOT_BYTES % IO_ALIGN == 0);
    assert!(TOTAL_BLOCKS * SLOT_BYTES == CONTAINER_BYTES);
    assert!(PAYLOAD_SLOTS as u64 * PAD_SECTORS_PER_SAVE as u64 == 134_152_192);
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
    offset % IO_ALIGN as u64 == 0 && len % IO_ALIGN == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_geometry_is_exact() {
        assert_eq!(TOTAL_BLOCKS * SLOT_BYTES, CONTAINER_BYTES);
        assert_eq!(CONTAINER_BYTES, 134_217_728);
        assert_eq!(PAYLOAD_SLOTS, 2_047);
        assert_eq!(slot_range(0), (0, 65_536));
        // Last block ends exactly at the end of the file: no slack, no truncation.
        let (off, len) = slot_range(TOTAL_BLOCKS - 1);
        assert_eq!(off + len, CONTAINER_BYTES);
    }

    #[test]
    fn root_block_is_fixed_and_permutation_covers_the_rest() {
        assert_eq!(ROOT_SLOT_INDEX, 0);
        // Every payload slot is addressable and none aliases the root.
        let first = ROOT_SLOT_INDEX + 1;
        assert_eq!(first, 1);
        assert_eq!(first + PAYLOAD_SLOTS - 1, TOTAL_BLOCKS - 1);
    }

    #[test]
    fn salt_sits_at_offset_zero_and_is_padded_over() {
        assert_eq!(SALT_OFFSET, 0);
        assert!(SALT_BYTES < SLOT_BYTES);
        assert_eq!(PAD_COVERAGE_BYTES, CONTAINER_BYTES);
    }

    #[test]
    fn save_budget_is_the_documented_fifty_four() {
        assert_eq!(SAVES_BEFORE_DOMAIN_REUSE, 54);
        assert!(SAVES_BEFORE_DOMAIN_REUSE * PAD_SECTORS_PER_SAVE as u64 <= ZONE_B_SECTORS as u64);
        // And the 55th does wrap: the budget is tight, not slack.
        assert!((SAVES_BEFORE_DOMAIN_REUSE + 1) * PAD_SECTORS_PER_SAVE as u64
            > ZONE_B_SECTORS as u64);
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
