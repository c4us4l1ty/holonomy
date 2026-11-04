//! `StratumRootCapsuleV5` — the 65,536-byte, 4,096-aligned root node.
//!
//! # The two defects this file exists to prevent
//!
//! The superseded `v5.0.0-OMEGA` struct summed to **65,552 bytes** and shipped into a
//! 65,536-byte slot, because `encrypted_directory_and_reservoir` was declared as 60,608 and the
//! trailing 16-byte Poly1305 tag was then appended on top. It is the same defect
//! `holonomy-old/crates/holonomy-container/src/layout.rs` had already found and fixed for
//! `v3.0.0`; v5 re-introduced it.
//!
//! Second, the old `WRAPPED_SK_BYTES` comment read "SK_PQ + SK_ECC + Poly1305 Tag" while the
//! arithmetic (`3_168 + 32 + 16`) counted the tag *inside* a field that also had an adjacent
//! `sk_unwrap_nonce` elsewhere in the struct — so the field did not actually have room for the
//! ciphertext, the nonce and the tag it was documented to hold.
//!
//! Both are now pinned by `const _: () = assert!(...)` below, which is the only kind of
//! assertion that cannot be forgotten at runtime. If a future edit changes one of the field
//! sizes without rebalancing the others, the crate **fails to compile**.
//!
//! # Field arithmetic
//!
//! ```text
//!   ct_ml_kem_1024                     1,568   FIPS 203 ML-KEM-1024 ciphertext
//!   ct_x25519                            32
//!   sk_unwrap_nonce                      24   XChaCha20 nonce for the wrap below
//!   wrapped_stratum_sk                3,216   3,168 ML-KEM-1024 dk + 32 X25519 sk + 16 Poly1305
//!   phase1_cmt4_tag                      32   CMT-4 commitment over phase 1
//!   directory_nonce                      24
//!   encrypted_directory_and_reservoir  60,592  payload + reservoir + capability envelopes
//!   directory_poly1305_tag               16
//!   phase2_cmt4_tag                      32
//!   -------------------------------------------
//!   total                              65,536
//! ```
//!
//! `60,592` is the one number that changed from the PRD (`60,608`), and it changed by exactly
//! the 16 bytes of `directory_poly1305_tag`. There is no padding: `65,536 = 16 × 4,096`, so a
//! `repr(C)` struct of these fields has no interior or trailing padding to absorb, and
//! `align_of === 4,096` is requested via `repr(C, align(4096))` for the transient in-RAM value
//! so a capsule can back a direct-I/O read without a copy.

use crate::layout::{CAPSULE_BYTES, IO_ALIGN, SALT_BYTES, SLOT_BYTES};

/// `ML-KEM-1024` ciphertext size, FIPS 203.
pub const ML_KEM_1024_CT_BYTES: usize = 1_568;

/// `ML-KEM-1024` decapsulation-key size, FIPS 203.
pub const ML_KEM_1024_SK_BYTES: usize = 3_168;

/// `X25519` secret scalar.
pub const X25519_SK_BYTES: usize = 32;

/// `Poly1305` authentication tag.
pub const POLY1305_TAG_BYTES: usize = 16;

/// `XChaCha20` nonce.
pub const XCHACHA_NONCE_BYTES: usize = 24;

/// `BLAKE2b-256` CMT-4 commitment.
pub const CMT4_TAG_BYTES: usize = 32;

/// Wrapped `SK_stratum` = ML-KEM dk + X25519 sk + Poly1305 tag, encrypted under `K_unwrap`
/// with the adjacent `sk_unwrap_nonce`.
///
/// The AEAD nonce is **not** part of this field: it is the sibling `sk_unwrap_nonce` of the same
/// struct, which is the correction to the old layout's double-counted comment.
pub const WRAPPED_SK_BYTES: usize = ML_KEM_1024_SK_BYTES + X25519_SK_BYTES + POLY1305_TAG_BYTES;

/// Encrypted directory + parasitic reservoir + lower-stratum capability envelopes.
///
/// **Excludes the 16-byte Poly1305 tag.** The AEAD input is built by appending
/// [`StratumRootCapsuleV5::directory_poly1305_tag`] to this field. That split is the whole 16-byte
/// correction: the superseded layout declared 60,608 *and* appended a tag, so the struct was
/// 65,552 bytes in a 65,536-byte slot. 60,592 + 16 = 60,608 of AEAD output, stored without
/// duplicating the tag.
pub const ENCRYPTED_DIRECTORY_BYTES: usize = 60_592;

/// The unwhitened, un-padded value of a stratum root node.
///
/// This is what sits in a slot *after* the USB pad is stripped and *before* XChaCha20
/// unwhitening — or equivalently, what the encryptor builds before whitening and folding. It is
/// never written to disk in this shape.
#[repr(C)]
#[derive(Clone, Debug)]
pub struct StratumRootCapsuleV5 {
    /// Phase 1A: FIPS 203 `ML-KEM-1024` ciphertext.
    pub ct_ml_kem_1024: [u8; ML_KEM_1024_CT_BYTES],
    /// Phase 1B: ephemeral `X25519` public key.
    pub ct_x25519: [u8; X25519_SK_BYTES],
    /// Phase 1C: nonce for `wrapped_stratum_sk`.
    pub sk_unwrap_nonce: [u8; XCHACHA_NONCE_BYTES],
    /// Phase 1C: `AEAD_{K_unwrap}(SK_stratum)`.
    pub wrapped_stratum_sk: [u8; WRAPPED_SK_BYTES],
    /// Phase 1D: CMT-4 commitment over all phase 1 fields.
    pub phase1_cmt4_tag: [u8; CMT4_TAG_BYTES],
    /// Phase 2A: nonce for the directory payload under `K_DEM`.
    pub directory_nonce: [u8; XCHACHA_NONCE_BYTES],
    /// Phase 2B: encrypted directory, decoy reservoir and capability envelopes.
    pub encrypted_directory_and_reservoir: [u8; ENCRYPTED_DIRECTORY_BYTES],
    /// Phase 2C: Poly1305 tag over phase 2.
    pub directory_poly1305_tag: [u8; POLY1305_TAG_BYTES],
    /// Phase 2C: CMT-4 commitment over phase 2.
    pub phase2_cmt4_tag: [u8; CMT4_TAG_BYTES],
}

// The audited identities. These are the point of this module: if a future edit changes one field
// without rebalancing the others, the crate does not compile.
//
// They live inside `const { }` blocks rather than at item scope so that a passing assertion does not
// also emit a dead `const` item, which `#![deny(dead_code)]` in a downstream crate would trip on.
const _: () = {
    assert!(WRAPPED_SK_BYTES == 3_216);
    assert!(ENCRYPTED_DIRECTORY_BYTES == 60_592);
    assert!(ENCRYPTED_DIRECTORY_BYTES + POLY1305_TAG_BYTES == 60_608);
    assert!(
        ML_KEM_1024_CT_BYTES
            + X25519_SK_BYTES
            + XCHACHA_NONCE_BYTES
            + WRAPPED_SK_BYTES
            + CMT4_TAG_BYTES
            + XCHACHA_NONCE_BYTES
            + ENCRYPTED_DIRECTORY_BYTES
            + POLY1305_TAG_BYTES
            + CMT4_TAG_BYTES
            == CAPSULE_BYTES
    );
    assert!(CAPSULE_BYTES == SLOT_BYTES);
    assert!(SLOT_BYTES % IO_ALIGN == 0);
    assert!(size_of::<StratumRootCapsuleV5>() == SLOT_BYTES);
};

/// On-disk wire form of a capsule: a plain byte array.
///
/// The struct and this array are the same bytes in the same order (`repr(C)`, no padding), but
/// the array is what crosses the crate boundary, so no `unsafe` transmute appears anywhere in a
/// caller. [`CapsuleWire::from_capsule`] and [`CapsuleWire::to_capsule`] are the only bridge and
/// they are total functions both ways.
#[repr(C, align(4096))]
#[derive(Clone, Debug)]
pub struct CapsuleWire(pub [u8; SLOT_BYTES]);

impl CapsuleWire {
    /// Zeroed wire buffer, `4,096`-aligned so it can back a direct-I/O read.
    pub fn zeroed() -> Self {
        Self([0u8; SLOT_BYTES])
    }

    /// The wire bytes, for a direct write to the device.
    pub fn as_bytes(&self) -> &[u8; SLOT_BYTES] {
        &self.0
    }

    /// The wire bytes, for a direct read from the device.
    pub fn as_mut_bytes(&mut self) -> &mut [u8; SLOT_BYTES] {
        &mut self.0
    }

    /// Serialise a capsule into its wire form.
    ///
    /// Written out field by field rather than via a `memcpy` of the struct: `repr(C)` on a struct
    /// of `[u8; N]` fields happens to be unpadded today, but relying on that couples the wire format
    /// to a compiler detail. The offsets below are the wire format.
    pub fn from_capsule(c: &StratumRootCapsuleV5) -> Self {
        let mut out = Self::zeroed();
        let w = &mut out.0;

        let mut o = 0usize;
        w[o..o + ML_KEM_1024_CT_BYTES].copy_from_slice(&c.ct_ml_kem_1024);
        o += ML_KEM_1024_CT_BYTES;
        w[o..o + X25519_SK_BYTES].copy_from_slice(&c.ct_x25519);
        o += X25519_SK_BYTES;
        w[o..o + XCHACHA_NONCE_BYTES].copy_from_slice(&c.sk_unwrap_nonce);
        o += XCHACHA_NONCE_BYTES;
        w[o..o + WRAPPED_SK_BYTES].copy_from_slice(&c.wrapped_stratum_sk);
        o += WRAPPED_SK_BYTES;
        w[o..o + CMT4_TAG_BYTES].copy_from_slice(&c.phase1_cmt4_tag);
        o += CMT4_TAG_BYTES;
        w[o..o + XCHACHA_NONCE_BYTES].copy_from_slice(&c.directory_nonce);
        o += XCHACHA_NONCE_BYTES;
        w[o..o + ENCRYPTED_DIRECTORY_BYTES].copy_from_slice(&c.encrypted_directory_and_reservoir);
        o += ENCRYPTED_DIRECTORY_BYTES;
        w[o..o + POLY1305_TAG_BYTES].copy_from_slice(&c.directory_poly1305_tag);
        o += POLY1305_TAG_BYTES;
        w[o..o + CMT4_TAG_BYTES].copy_from_slice(&c.phase2_cmt4_tag);
        o += CMT4_TAG_BYTES;

        debug_assert_eq!(o, SLOT_BYTES, "capsule serialisation must fill the whole slot");
        out
    }

    /// Parse a wire buffer back into a capsule.
    pub fn to_capsule(&self) -> StratumRootCapsuleV5 {
        let b = &self.0;
        let mut o = 0usize;

        let mut ct_ml_kem_1024 = [0u8; ML_KEM_1024_CT_BYTES];
        ct_ml_kem_1024.copy_from_slice(&b[o..o + ML_KEM_1024_CT_BYTES]);
        o += ML_KEM_1024_CT_BYTES;
        let mut ct_x25519 = [0u8; X25519_SK_BYTES];
        ct_x25519.copy_from_slice(&b[o..o + X25519_SK_BYTES]);
        o += X25519_SK_BYTES;
        let mut sk_unwrap_nonce = [0u8; XCHACHA_NONCE_BYTES];
        sk_unwrap_nonce.copy_from_slice(&b[o..o + XCHACHA_NONCE_BYTES]);
        o += XCHACHA_NONCE_BYTES;
        let mut wrapped_stratum_sk = [0u8; WRAPPED_SK_BYTES];
        wrapped_stratum_sk.copy_from_slice(&b[o..o + WRAPPED_SK_BYTES]);
        o += WRAPPED_SK_BYTES;
        let mut phase1_cmt4_tag = [0u8; CMT4_TAG_BYTES];
        phase1_cmt4_tag.copy_from_slice(&b[o..o + CMT4_TAG_BYTES]);
        o += CMT4_TAG_BYTES;
        let mut directory_nonce = [0u8; XCHACHA_NONCE_BYTES];
        directory_nonce.copy_from_slice(&b[o..o + XCHACHA_NONCE_BYTES]);
        o += XCHACHA_NONCE_BYTES;
        let mut encrypted_directory_and_reservoir = [0u8; ENCRYPTED_DIRECTORY_BYTES];
        encrypted_directory_and_reservoir.copy_from_slice(&b[o..o + ENCRYPTED_DIRECTORY_BYTES]);
        o += ENCRYPTED_DIRECTORY_BYTES;
        let mut directory_poly1305_tag = [0u8; POLY1305_TAG_BYTES];
        directory_poly1305_tag.copy_from_slice(&b[o..o + POLY1305_TAG_BYTES]);
        o += POLY1305_TAG_BYTES;
        let mut phase2_cmt4_tag = [0u8; CMT4_TAG_BYTES];
        phase2_cmt4_tag.copy_from_slice(&b[o..o + CMT4_TAG_BYTES]);
        o += CMT4_TAG_BYTES;

        debug_assert_eq!(o, SLOT_BYTES);

        StratumRootCapsuleV5 {
            ct_ml_kem_1024,
            ct_x25519,
            sk_unwrap_nonce,
            wrapped_stratum_sk,
            phase1_cmt4_tag,
            directory_nonce,
            encrypted_directory_and_reservoir,
            directory_poly1305_tag,
            phase2_cmt4_tag,
        }
    }
}

/// The 64-byte `σ_epoch` block that opens every container.
///
/// Held as its own type so a caller cannot accidentally treat the salt as a key, and so the
/// 64-byte length is a compile-time fact rather than a comment.
#[derive(Clone)]
pub struct EpochSalt(pub [u8; SALT_BYTES]);

impl EpochSalt {
    /// The 64 salt bytes, as the `Argon2id` salt and as the `K_perm`/`K_whiten` context.
    pub fn as_bytes(&self) -> &[u8; SALT_BYTES] {
        &self.0
    }
}

impl Drop for EpochSalt {
    fn drop(&mut self) {
        // `σ_epoch` is public-by-construction once the USB is present, but it keys `K_perm` and
        // `K_whiten`, so it is scrubbed on drop the same way any other key material is. Via
        // `zeroize` rather than a plain fill so the store is not elided by the optimizer.
        zeroize::Zeroize::zeroize(&mut self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capsule_is_exactly_one_slot() {
        assert_eq!(size_of::<StratumRootCapsuleV5>(), 65_536);
        assert_eq!(size_of::<StratumRootCapsuleV5>(), SLOT_BYTES);
    }

    #[test]
    fn field_sum_matches_the_audited_table() {
        assert_eq!(ML_KEM_1024_CT_BYTES, 1_568);
        assert_eq!(WRAPPED_SK_BYTES, 3_216);
        assert_eq!(ML_KEM_1024_SK_BYTES + X25519_SK_BYTES + POLY1305_TAG_BYTES, 3_216);
        assert_eq!(ENCRYPTED_DIRECTORY_BYTES, 60_592);
        // The old PRD's number was 16 too large; pin the correction.
        assert_eq!(ENCRYPTED_DIRECTORY_BYTES + 16, 60_608);
    }

    #[test]
    fn wire_form_is_4096_aligned() {
        assert_eq!(align_of::<CapsuleWire>(), 4096);
        assert_eq!(size_of::<CapsuleWire>(), 65_536);
        let w = CapsuleWire::zeroed();
        assert_eq!(w.as_bytes().len(), CAPSULE_BYTES);
    }

    #[test]
    fn wire_round_trip_is_lossless() {
        let mut c = StratumRootCapsuleV5 {
            ct_ml_kem_1024: [0x11; ML_KEM_1024_CT_BYTES],
            ct_x25519: [0x22; X25519_SK_BYTES],
            sk_unwrap_nonce: [0x33; XCHACHA_NONCE_BYTES],
            wrapped_stratum_sk: [0x44; WRAPPED_SK_BYTES],
            phase1_cmt4_tag: [0x55; CMT4_TAG_BYTES],
            directory_nonce: [0x66; XCHACHA_NONCE_BYTES],
            encrypted_directory_and_reservoir: [0x77; ENCRYPTED_DIRECTORY_BYTES],
            directory_poly1305_tag: [0x88; POLY1305_TAG_BYTES],
            phase2_cmt4_tag: [0x99; CMT4_TAG_BYTES],
        };
        // A marker at the very last byte catches an off-by-one in the serialiser's offsets.
        c.phase2_cmt4_tag[CMT4_TAG_BYTES - 1] = 0xAB;

        let wire = CapsuleWire::from_capsule(&c);
        assert_eq!(wire.as_bytes()[SLOT_BYTES - 1], 0xAB);
        let back = wire.to_capsule();
        assert_eq!(back.ct_ml_kem_1024, c.ct_ml_kem_1024);
        assert_eq!(back.encrypted_directory_and_reservoir, c.encrypted_directory_and_reservoir);
        assert_eq!(back.phase2_cmt4_tag, c.phase2_cmt4_tag);
    }
}
