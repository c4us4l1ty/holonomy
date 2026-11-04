//! The save path: seal a stratum's payload into container slots and lay them out in an image.
//!
//! This is the first code in the project that ever *produces* a container. Everything before it
//! verified, described or budgeted one; nothing had written the structures that share those
//! 134,217,728 bytes, which is why the salt sat inside the root capsule for so long — the two
//! structures had nothing to collide in. Writing the writer is what made that a failing test.
//!
//! # What a save does
//!
//! ```text
//!   payload ──seal_committed(K_DEM, N_epoch, slot AAD)──► ciphertext ‖ CMT-4 tag
//!   envelope ──create_envelope(K_unwrap, N_epoch, κ_USB)──► ct ‖ wrapped SK ‖ K_DEM
//!   directory ──ledger(version, stratum, cursor, lengths) + tag──► sealed under K_DEM
//!   capsule  ──both phase commitments──► slot 0
//!   salt'    ◄── chosen only after the old salt was read  (crate::salt)
//! ```
//!
//! # What it deliberately does not do
//!
//! * **It does not write.** [`SealedContainer::encode_into`] fills a caller-owned image; the aligned
//!   direct-I/O write, the pad fold and the ratchet belong to the store, which needs a device. This
//!   module is testable without one.
//! * **It does not decide whether a save is allowed.** The cursor, the pad budget and the refusal
//!   rule are [`crate::session`]'s, and they were written and tested before this module could run.
//! * **It does not choose the salt.** It is an argument, so the ordering rule in [`crate::salt`]
//!   stays visible at the call site instead of being satisfied by a convenience method that happens
//!   to get it right today.
//!
//! # The USB share is an argument, not a derivation
//!
//! `kappa_usb` is the 32-byte root share from the stick's Zone A, and it is a required parameter.
//! An earlier draft derived it from the salt "for now": that would make a container sealed from the
//! container alone, which is precisely the property the whole design exists to deny. As a parameter
//! it cannot be dropped silently, and `the_container_depends_on_the_usb_share` fails if it is
//! ignored.

use blake2::{Blake2b512, Digest};

use holonomy_core::capsule::{CapsuleWire, StratumRootCapsuleV5, WRAPPED_SK_BYTES};
use holonomy_core::layout::{
    slot_range, CONTAINER_BYTES, PAYLOAD_SLOTS, PAYLOAD_SLOT_BASE, SALT_BYTES, SALT_SLOT_INDEX,
    SLOT_BYTES, TOTAL_BLOCKS,
};
use holonomy_core::stratum::{ActiveStratum, StratumKeys};
use holonomy_crypto::commit::{
    self as commit_mod, seal_committed, slot_associated_data, slot_nonce, AeadError,
    CommittedCiphertext, CMT4_TAG_BYTES, NONCE_BYTES, POLY1305_TAG_BYTES,
};
use holonomy_crypto::envelope::{
    create_envelope, phase1_commitment, phase2_commitment, HybridCiphertext, MlKem1024X25519,
};

use crate::salt::SaltBytes;

/// One sealed payload slot, ready to be written at whatever physical index the permutation says.
#[derive(Clone)]
pub struct SealedPayloadSlot {
    /// Ciphertext of the plaintext plus its Poly1305 tag.
    pub ciphertext: Vec<u8>,
    /// `CMT-4` commitment over `(K_DEM, nonce, AAD, ciphertext)`.
    pub commitment: [u8; CMT4_TAG_BYTES],
    /// The nonce used, kept so an opener can rebuild the commitment input.
    pub nonce: [u8; NONCE_BYTES],
}

impl std::fmt::Debug for SealedPayloadSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SealedPayloadSlot({} bytes ciphertext, {} byte commitment)",
            self.ciphertext.len(),
            CMT4_TAG_BYTES
        )
    }
}

/// Everything one save produced, in a form that can be written to a device.
pub struct SealedContainer {
    /// The stratum index this container was sealed for. Only the index: the keys stay in the
    /// caller's [`ActiveStratum`] and are not copied here.
    pub stratum: u8,
    /// New `σ_epoch`, for slot 1.
    pub salt: SaltBytes,
    /// The root capsule for this stratum, in wire form, for slot 0.
    pub capsule: CapsuleWire,
    /// Sealed payload slots, in *logical* order. Physical placement is the permutation's job and is
    /// applied by the store, which is the only component that knows `K_perm`.
    pub slots: Vec<SealedPayloadSlot>,
}

impl std::fmt::Debug for SealedContainer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "SealedContainer(stratum {}, {} bytes of salt, capsule, {} payload slots)",
            self.stratum,
            SALT_BYTES,
            self.slots.len()
        )
    }
}

/// Why a save could not be sealed.
///
/// `Copy` and `PartialEq` because every variant is a pair of `usize` or an error type that is
/// already both, and a caller matching on this should not have to borrow it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealError {
    /// The plaintext was too large for one slot after sealing.
    PayloadTooLarge {
        /// Bytes offered.
        offered: usize,
        /// Bytes that fit.
        capacity: usize,
    },
    /// More slots were offered than the container addresses.
    TooManySlots {
        /// Slots offered.
        offered: usize,
        /// Slots available.
        capacity: usize,
    },
    /// The envelope layer refused to build a capsule.
    Envelope(holonomy_crypto::envelope::EnvelopeError),
    /// A slot failed to open. Impossible for a container this process just sealed, so it is a bug
    /// rather than an attack, and it is reported as such.
    Aead(AeadError),
    /// The image buffer was the wrong size. A container is never a partial file.
    WrongImageSize {
        /// Bytes supplied.
        found: usize,
        /// Bytes required.
        needed: usize,
    },
    /// The placement function returned a block the payload may not occupy, or one already used.
    BadPlacement {
        /// Which logical slot.
        slot: usize,
        /// The physical block it was sent to.
        physical: usize,
    },
}

impl std::fmt::Display for SealError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PayloadTooLarge { offered, capacity } => write!(
                f,
                "payload of {offered} bytes exceeds the {capacity}-byte capacity of one slot"
            ),
            Self::TooManySlots { offered, capacity } => write!(
                f,
                "{offered} payload slots offered; the container addresses {capacity}"
            ),
            Self::Envelope(e) => write!(f, "envelope: {e}"),
            Self::Aead(e) => write!(f, "slot: {e}"),
            Self::WrongImageSize { found, needed } => {
                write!(f, "image buffer is {found} bytes; a container is {needed}")
            }
            Self::BadPlacement { slot, physical } => write!(
                f,
                "logical slot {slot} was placed at block {physical}, which is reserved, out of \
                 range, or already written"
            ),
        }
    }
}

impl std::error::Error for SealError {}

/// Bytes of plaintext one payload slot holds: the slot, minus the `Poly1305` tag, minus the `CMT-4`
/// commitment.
///
/// # The 32 bytes that used to be over-counted
///
/// This was `SLOT_BYTES - POLY1305_TAG_BYTES`, on the reasoning that "the `CMT-4` commitment lives
/// beside the ciphertext rather than inside the block". It does not: [`SealedContainer::encode_into`]
/// writes the commitment into the **last `CMT4_TAG_BYTES` of the slot**, after the ciphertext,
/// because that is where a reader looks for it before it has a ledger to tell it the length. A
/// plaintext of exactly this constant therefore sealed to a ciphertext that did not fit, and
/// `encode_into` refused the whole save with `PayloadTooLarge` -- a full-size chunk of a real
/// document could not be written, and the store's three-slot test is what found it.
///
/// [`crate::directory::DirectoryLedger::decode`] already used the correct bound
/// (`SLOT_BYTES - CMT4_TAG_BYTES`, which is this constant plus the tag); the writer's constant was
/// the odd one out.
pub const PAYLOAD_PLAINTEXT_PER_SLOT: usize = SLOT_BYTES - POLY1305_TAG_BYTES - CMT4_TAG_BYTES;

const _: () =
    assert!(PAYLOAD_PLAINTEXT_PER_SLOT + POLY1305_TAG_BYTES + CMT4_TAG_BYTES == SLOT_BYTES);

/// Seal one save.
///
/// `slots` are the plaintext payload slots in logical order; `salt` must be the value the caller
/// obtained from [`crate::salt::ContainerHeader::after_reading`]; `kappa_usb` is the stick's root
/// share. Every payload slot is sealed under the same `K_DEM` with a per-slot nonce derived from
/// `(save_cursor, slot_index, stratum)`, so a nonce cannot repeat within a save or across two saves
/// of the same budget window.
pub fn seal_container(
    stratum: &ActiveStratum,
    slots: &[&[u8]],
    salt: SaltBytes,
    save_cursor: u64,
    kappa_usb: &[u8; 32],
) -> Result<SealedContainer, SealError> {
    if slots.len() > PAYLOAD_SLOTS {
        return Err(SealError::TooManySlots {
            offered: slots.len(),
            capacity: PAYLOAD_SLOTS,
        });
    }
    for plaintext in slots {
        if plaintext.len() > PAYLOAD_PLAINTEXT_PER_SLOT {
            return Err(SealError::PayloadTooLarge {
                offered: plaintext.len(),
                capacity: PAYLOAD_PLAINTEXT_PER_SLOT,
            });
        }
    }
    let keys = stratum.keys();
    let index = stratum.id().index();

    // The envelope is created **first**, because it is what produces the `K_DEM` the rest of this
    // function uses. An earlier draft sealed the payload under the stratum's own `k_dem` and then
    // built the envelope and sealed the *directory* under the envelope's key, so the two halves of
    // one container were written under two unrelated keys. Nothing could open it: the reader's
    // phase-2 commitment verifies (it uses the envelope key) and then every payload slot fails its
    // `CMT-4`, which reports as "the document is corrupt" on a correct passphrase.
    let (ct, wrapped, dem_keys) =
        create_envelope(&keys.k_unwrap, &keys.n_epoch, kappa_usb, &MlKem1024X25519)
            .map_err(SealError::Envelope)?;

    let mut sealed = Vec::with_capacity(slots.len());
    for (slot_index, plaintext) in slots.iter().enumerate() {
        let nonce = slot_nonce(&dem_keys.n_epoch, save_cursor, slot_index as u32, index);
        let aad = slot_associated_data(index, slot_index as u32);
        let committed = seal_committed(&dem_keys.k_dem, &nonce, &aad, plaintext);
        sealed.push(SealedPayloadSlot {
            ciphertext: committed.ciphertext,
            commitment: committed.cmt4,
            nonce,
        });
    }

    let capsule = build_capsule(keys, &sealed, index, save_cursor, (ct, wrapped, &dem_keys))?;
    Ok(SealedContainer {
        stratum: index,
        salt,
        capsule,
        slots: sealed,
    })
}

/// Build this stratum's root capsule over the sealed payload.
///
/// The envelope arrives already created: [`seal_container`] needs its key set to seal the payload
/// before the capsule can be assembled, and creating it twice would produce a capsule over a
/// different key than the slots. It is passed as a tuple rather than as three arguments so the
/// three pieces that must come from one `create_envelope` call cannot be mixed with another's.
///
/// The envelope is fresh every save: a new KEM keypair per save is the ratchet, and
/// `create_envelope` draws it from the OS CSPRNG. `kappa_usb` binds the derived `K_DEM` to the
/// stick, so the capsule alone is not decryptable.
fn build_capsule(
    keys: &StratumKeys,
    sealed: &[SealedPayloadSlot],
    stratum_index: u8,
    save_cursor: u64,
    (ct, wrapped, dem_keys): (HybridCiphertext, [u8; WRAPPED_SK_BYTES], &StratumKeys),
) -> Result<CapsuleWire, SealError> {
    // The directory is this container's index. Written first because the capsule must carry its
    // commitment, and because it is the only place the save cursor is recorded.
    let commitments = crate::directory::commitments_of(sealed);
    let ledger = crate::directory::DirectoryLedger::new(stratum_index, save_cursor, sealed);
    let plaintext = ledger.encode(&commitments)?;
    let directory_sealed =
        commit_mod::seal_directory(&dem_keys.k_dem, &dem_keys.n_epoch, &plaintext)
            .map_err(SealError::Aead)?;
    let (body, tag) = holonomy_crypto::unlock::split_directory_ciphertext(&directory_sealed);
    let p2 = phase2_commitment(&dem_keys.k_dem, &dem_keys.n_epoch, &directory_sealed);
    let p1 = phase1_commitment(&keys.k_unwrap, &keys.n_epoch, &ct, &wrapped);

    Ok(CapsuleWire::from_capsule(&StratumRootCapsuleV5 {
        ct_ml_kem_1024: ct.ml_kem,
        ct_x25519: ct.x25519,
        sk_unwrap_nonce: keys.n_epoch,
        wrapped_stratum_sk: wrapped,
        phase1_cmt4_tag: p1,
        directory_nonce: dem_keys.n_epoch,
        encrypted_directory_and_reservoir: body,
        directory_poly1305_tag: tag,
        phase2_cmt4_tag: p2,
    }))
}

impl SealedContainer {
    /// Encode this container into a full-size image, placing each payload slot at a physical block.
    ///
    /// `placement(i)` returns the physical **block** index for logical slot `i`; the store supplies
    /// it from the epoch permutation, and it must be injective. This function checks that it is,
    /// because a silent collision would overwrite one slot with another and the ledger would only
    /// notice at the next open.
    ///
    /// The image it produces is *not yet padded*: the store folds it against the save's pad window
    /// before writing. A container written straight from this buffer would have a plaintext capsule
    /// in slot 0.
    pub fn encode_into(
        &self,
        image: &mut [u8],
        placement: impl Fn(usize) -> usize,
    ) -> Result<(), SealError> {
        if image.len() != CONTAINER_BYTES {
            return Err(SealError::WrongImageSize {
                found: image.len(),
                needed: CONTAINER_BYTES,
            });
        }

        // Root capsule, slot 0.
        let (root_off, root_len) = slot_range(0);
        image[root_off..root_off + root_len].copy_from_slice(self.capsule.as_bytes());

        // Salt slot: the salt in its first 64 bytes. The rest of the slot carries pad-bound noise
        // once the store folds it, which is why nothing else is written here.
        let (salt_off, _) = slot_range(SALT_SLOT_INDEX);
        image[salt_off..salt_off + SALT_BYTES].copy_from_slice(self.salt.as_bytes());

        // Payload, at the permuted indices. Both fixed residents are pre-marked so a placement that
        // returns 0 or 1 is caught here rather than silently overwriting the capsule or the salt.
        let mut seen = [false; TOTAL_BLOCKS];
        seen[0] = true;
        seen[SALT_SLOT_INDEX] = true;
        for (logical, slot) in self.slots.iter().enumerate() {
            let physical = placement(logical);
            if !(PAYLOAD_SLOT_BASE..TOTAL_BLOCKS).contains(&physical) || seen[physical] {
                return Err(SealError::BadPlacement {
                    slot: logical,
                    physical,
                });
            }
            seen[physical] = true;
            let (off, len) = slot_range(physical);
            let body = &mut image[off..off + len];
            body[..slot.ciphertext.len()].copy_from_slice(&slot.ciphertext);
            // The commitment goes in the last CMT4_TAG_BYTES of the slot, after the Poly1305 tag.
            // A reader knows where to look because the ledger gives the ciphertext length.
            let at = len - CMT4_TAG_BYTES;
            body[at..].copy_from_slice(&slot.commitment);
        }
        Ok(())
    }
}

/// Open a payload slot from a container image.
///
/// Takes the commitment from the *image* rather than from the ledger, so a slot moved by an attacker
/// fails here even if the directory were somehow rewritten; the ledger check is a second,
/// independent gate. The AAD carries the slot index, so relocating a slot inside the image
/// invalidates it even when ciphertext and tag travel together.
pub fn open_payload_slot(
    keys: &StratumKeys,
    stratum_index: u8,
    slot_index: u32,
    nonce: &[u8; NONCE_BYTES],
    ciphertext: &[u8],
    commitment: &[u8; CMT4_TAG_BYTES],
) -> Result<Zeroizing, SealError> {
    let aad = slot_associated_data(stratum_index, slot_index);
    let committed = CommittedCiphertext {
        ciphertext: ciphertext.to_vec(),
        cmt4: *commitment,
    };
    let plain = holonomy_crypto::commit::open_committed(&keys.k_dem, nonce, &aad, &committed)
        .map_err(SealError::Aead)?;
    Ok(Zeroizing(plain))
}

/// A decrypted payload that zeroizes on drop.
///
/// A newtype rather than a bare `Vec` because this is the one place a document's plaintext exists in
/// a heap allocation, and a bare `Vec<u8>` gives no hook to clear it.
pub struct Zeroizing(Vec<u8>);

impl Zeroizing {
    /// The plaintext.
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl std::fmt::Debug for Zeroizing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Zeroizing({} bytes of plaintext, not shown)",
            self.0.len()
        )
    }
}

impl Drop for Zeroizing {
    fn drop(&mut self) {
        holonomy_crypto::commit::scrub(&mut self.0);
    }
}

/// A truncated digest over an image, for the tests and for `--check`.
///
/// Not a security boundary — the commitments are — but it makes "did the image change" a cheap
/// question, which is how the tests assert that a save rewrote every slot rather than a few.
pub fn image_digest(image: &[u8]) -> [u8; 32] {
    let mut h = Blake2b512::new();
    h.update(image);
    let d = h.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&d[..32]);
    out
}

/// The save path, with the envelope key set returned alongside the container.
///
/// # Why this exists
///
/// `seal_container` creates its own envelope and drops the key set, which is right for production:
/// no caller should hold the `K_DEM` a container was written under. But a test that needs to prove
/// "the payload and the directory were sealed under one key" has to have that key, and the
/// alternative -- fabricating one -- would test nothing about the round trip.
///
/// `#[cfg(test)]` *and* `pub(crate)`: the attribute keeps it out of every non-test build, so it
/// cannot become part of the crate's surface or be reached from another crate by accident. It
/// duplicates the body rather than calling `seal_container` because the key set is created inside
/// that function and dropped there; the two are checked against each other by
/// `the_seam_writes_what_the_save_path_writes`, which asserts both produce identical capsules for
/// identical inputs.
#[cfg(test)]
pub(crate) fn seal_container_with_keys(
    stratum: &ActiveStratum,
    slots: &[&[u8]],
    salt: SaltBytes,
    save_cursor: u64,
    kappa_usb: &[u8; 32],
) -> Result<(SealedContainer, StratumKeys), SealError> {
    if slots.len() > PAYLOAD_SLOTS {
        return Err(SealError::TooManySlots {
            offered: slots.len(),
            capacity: PAYLOAD_SLOTS,
        });
    }
    for plaintext in slots {
        if plaintext.len() > PAYLOAD_PLAINTEXT_PER_SLOT {
            return Err(SealError::PayloadTooLarge {
                offered: plaintext.len(),
                capacity: PAYLOAD_PLAINTEXT_PER_SLOT,
            });
        }
    }
    let keys = stratum.keys();
    let index = stratum.id().index();
    let (ct, wrapped, dem_keys) =
        create_envelope(&keys.k_unwrap, &keys.n_epoch, kappa_usb, &MlKem1024X25519)
            .map_err(SealError::Envelope)?;

    let mut sealed = Vec::with_capacity(slots.len());
    for (slot_index, plaintext) in slots.iter().enumerate() {
        let nonce = slot_nonce(&dem_keys.n_epoch, save_cursor, slot_index as u32, index);
        let aad = slot_associated_data(index, slot_index as u32);
        let committed = seal_committed(&dem_keys.k_dem, &nonce, &aad, plaintext);
        sealed.push(SealedPayloadSlot {
            ciphertext: committed.ciphertext,
            commitment: committed.cmt4,
            nonce,
        });
    }
    let capsule = build_capsule(keys, &sealed, index, save_cursor, (ct, wrapped, &dem_keys))?;
    let container = SealedContainer {
        stratum: index,
        salt,
        capsule,
        slots: sealed,
    };
    // The key set is `Clone`-free and drop-zeroizing, so a copy is made deliberately and returned
    // for the test to use; it is wiped when the test's binding drops.
    let copy = StratumKeys {
        k_unwrap: dem_keys.k_unwrap,
        k_dem: dem_keys.k_dem,
        n_epoch: dem_keys.n_epoch,
    };
    Ok((container, copy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use holonomy_core::stratum::StratumId;
    use holonomy_core::unlock::{ProtectionLayer, Topology, Unlocked};
    use holonomy_crypto::envelope::HybridKem;

    /// Stand-in for the USB root share. Present in every call because the parameter is what keeps
    /// the bipartite property from being quietly dropped.
    const KAPPA: [u8; 32] = [0x5Au8; 32];

    /// Build a live stratum. `ActiveStratum` has a sealed constructor, so this goes through the two
    /// public gates the real unlock passes.
    fn active(index: u8, keys: StratumKeys) -> ActiveStratum {
        let id = StratumId::new(index).expect("index in range");
        let layer = ProtectionLayer {
            pad: true,
            pad_fresh: true,
            post_quantum: true,
        };
        Unlocked::new(id, Topology::UmbilicalUsb, id, layer).into_active(keys)
    }

    fn keys(seed: u8) -> StratumKeys {
        StratumKeys {
            k_unwrap: [seed; 32],
            k_dem: [seed.wrapping_add(1); 32],
            n_epoch: [seed.wrapping_add(2); 24],
        }
    }

    fn salt(seed: u8) -> SaltBytes {
        SaltBytes::of_unknown_origin([seed; SALT_BYTES])
    }

    /// Open a capsule's envelope and return the `K_DEM` the directory was sealed with.
    ///
    /// This mirrors `holonomy_crypto::unlock` deliberately and narrowly: phase 1 unwraps `SK_stratum`
    /// with a κ-free `K_unwrap`, phase 2 decapsulates, and `derive_dem_key` rebuilds the key from the
    /// shared secrets, both ciphertexts, `K_unwrap` and κ. It does not check the commitments, because
    /// the point of this helper is to reach the directory — the commitments have their own tests.
    fn envelope_keys_for_test(
        capsule: &CapsuleWire,
        k_unwrap: &[u8; 32],
        kappa: &[u8; 32],
    ) -> Option<StratumKeys> {
        use holonomy_crypto::envelope::{
            derive_dem_key, unwrap_stratum_key, HybridCiphertext, MlKem1024X25519,
        };
        let c = capsule.to_capsule();
        let dk = unwrap_stratum_key(k_unwrap, &c.sk_unwrap_nonce, &c.wrapped_stratum_sk).ok()?;
        let ct = HybridCiphertext {
            ml_kem: c.ct_ml_kem_1024,
            x25519: c.ct_x25519,
        };
        let ss = MlKem1024X25519.decapsulate(&dk, &ct).ok()?;
        Some(derive_dem_key(&ss, &ct, k_unwrap, kappa))
    }

    /// The identity placement the store replaces with the epoch permutation.
    fn identity(i: usize) -> usize {
        PAYLOAD_SLOT_BASE + i
    }

    #[test]
    fn the_seam_writes_what_the_save_path_writes() {
        // `seal_container_with_keys` duplicates `seal_container`'s body so a test can have the
        // envelope key set. Two implementations of the save path is exactly the kind of thing that
        // drifts, so this pins them together: same inputs, same slots, and the capsules must be
        // equal except for the fields a fresh KEM keypair randomises. Those are the whole point of
        // the envelope, so what is compared is the *shape*: both must be a valid capsule that opens
        // under its own `K_unwrap` and yields the same plaintext.
        let s = active(2, keys(0x11));
        let doc: [&[u8]; 2] = [b"first", b"second"];

        let (container, dem) =
            seal_container_with_keys(&s, &doc, salt(0x12), 5, &KAPPA).expect("seam");
        let plain = seal_container(&s, &doc, salt(0x12), 5, &KAPPA).expect("save path");

        assert_eq!(container.stratum, plain.stratum);
        assert_eq!(container.slots.len(), plain.slots.len());
        for i in 0..container.slots.len() {
            assert_eq!(
                container.slots[i].ciphertext.len(),
                plain.slots[i].ciphertext.len(),
                "slot {i} length differs between the two write paths"
            );
        }

        // Both capsules open to the same document, under their own envelope keys.
        let dem_direct = envelope_keys_for_test(&plain.capsule, &keys(0x11).k_unwrap, &KAPPA)
            .expect("the save path's envelope opens");
        for (sealed, opened_with) in [(&container, &dem), (&plain, &dem_direct)] {
            for (i, expected) in doc.iter().enumerate() {
                let nonce = slot_nonce(&opened_with.n_epoch, 5, i as u32, 2);
                let text = open_payload_slot(
                    opened_with,
                    2,
                    i as u32,
                    &nonce,
                    &sealed.slots[i].ciphertext,
                    &sealed.slots[i].commitment,
                )
                .unwrap_or_else(|e| panic!("slot {i}: {e}"));
                assert_eq!(text.as_slice(), *expected, "slot {i}");
            }
        }
    }

    #[test]
    fn the_image_places_every_resident_where_the_layout_says() {
        // Placement, not decryption: this asserts the three writers of the image -- capsule, salt,
        // ciphertext, commitment -- land in the ranges `holonomy-core::layout` says they own. The
        // end-to-end read is `crate::open::tests`, which needs a real unlock.
        let s = active(2, keys(0x10));
        let plaintexts: [&[u8]; 3] = [b"the first slot", b"the second", b"and a third one"];
        let sealed = seal_container(&s, &plaintexts, salt(0x33), 7, &KAPPA).expect("seal");

        let mut image = vec![0u8; CONTAINER_BYTES];
        sealed.encode_into(&mut image, identity).expect("encode");

        assert_eq!(
            &image[..SLOT_BYTES],
            sealed.capsule.as_bytes(),
            "capsule in slot 0"
        );
        let (salt_off, _) = slot_range(SALT_SLOT_INDEX);
        assert_eq!(
            &image[salt_off..salt_off + SALT_BYTES],
            sealed.salt.as_bytes(),
            "salt in its own slot, not inside the capsule"
        );
        // The ciphertext starts at the beginning of its block and the commitment ends it.
        for (i, slot) in sealed.slots.iter().enumerate() {
            let (off, len) = slot_range(identity(i));
            assert_eq!(
                &image[off..off + slot.ciphertext.len()],
                &slot.ciphertext[..],
                "slot {i} ciphertext offset"
            );
            assert_eq!(
                &image[off + len - CMT4_TAG_BYTES..off + len],
                &slot.commitment[..],
                "slot {i} commitment at the end of the block"
            );
        }
    }

    #[test]
    fn the_payload_and_the_directory_are_sealed_under_one_key() {
        // The defect this pins: an earlier writer sealed payload slots under the stratum's `k_dem`
        // and the directory under the envelope's, so one container held two unrelated keys and
        // nothing could open it. Here the envelope is opened *once* and must open both.
        let s = active(1, keys(0x20));
        let sealed = seal_container(
            &s,
            &[b"payload under the envelope key"],
            salt(0x21),
            3,
            &KAPPA,
        )
        .expect("seal");
        let dem = envelope_keys_for_test(&sealed.capsule, &keys(0x20).k_unwrap, &KAPPA)
            .expect("envelope opens");

        // The directory.
        let capsule = sealed.capsule.to_capsule();
        let field = holonomy_crypto::unlock::directory_ciphertext(
            &capsule.encrypted_directory_and_reservoir,
            &capsule.directory_poly1305_tag,
        );
        assert!(
            holonomy_crypto::commit::open_directory(&dem.k_dem, &capsule.directory_nonce, &field)
                .is_ok(),
            "the directory must open under the envelope key"
        );

        // The payload slot, with the nonce the ledger reconstructs from the cursor.
        let nonce = slot_nonce(&dem.n_epoch, 3, 0, 1);
        let opened = open_payload_slot(
            &dem,
            1,
            0,
            &nonce,
            &sealed.slots[0].ciphertext,
            &sealed.slots[0].commitment,
        )
        .expect("the slot must open under the same envelope key");
        assert_eq!(opened.as_slice(), b"payload under the envelope key");

        // And not under the stratum's own key, which is the half of the old bug that a careless
        // fix would have kept.
        assert!(
            open_payload_slot(
                s.keys(),
                1,
                0,
                &nonce,
                &sealed.slots[0].ciphertext,
                &sealed.slots[0].commitment
            )
            .is_err(),
            "the stratum's static k_dem must not open a save-path slot"
        );
    }

    #[test]
    fn the_directory_records_the_cursor_the_pad_needs() {
        // The cursor selects the pad window and the capsule has no field for it. If the ledger
        // loses it, every container this writer produces is unopenable.
        let s = active(3, keys(0x40));
        let sealed = seal_container(&s, &[b"x"], salt(0x41), 54, &KAPPA).expect("seal");
        let mut image = vec![0u8; CONTAINER_BYTES];
        sealed.encode_into(&mut image, identity).expect("encode");

        let (off, len) = slot_range(identity(0));
        let mut commitment = [0u8; CMT4_TAG_BYTES];
        commitment.copy_from_slice(&image[off + len - CMT4_TAG_BYTES..off + len]);

        let capsule = sealed.capsule.to_capsule();
        let field = holonomy_crypto::unlock::directory_ciphertext(
            &capsule.encrypted_directory_and_reservoir,
            &capsule.directory_poly1305_tag,
        );
        // The directory is sealed under the *envelope-derived* `K_DEM`, not under the stratum's own
        // `k_dem`: it is written by the save path that owns `SK_stratum` (which lets a background
        // save encapsulate without the private key present), whereas the vault blob is sealed under
        // the static key. Reading it therefore needs the envelope, which is opened the same way the
        // unlock path opens it. This test is what pins that down -- guessing the stratum's key fails
        // here rather than in review.
        // `k_unwrap` comes from the stratum this test built, which is the same value the writer used
        // and the same value a reader derives from `(root, stratum)` -- see
        // `holonomy-crypto`'s `the_fallback_keys_equal_the_ones_the_capsule_was_built_with`, which
        // is where that equality is actually asserted.
        let k_unwrap = keys(0x40).k_unwrap;
        let dem =
            envelope_keys_for_test(&sealed.capsule, &k_unwrap, &KAPPA).expect("envelope opens");
        let plain =
            holonomy_crypto::commit::open_directory(&dem.k_dem, &capsule.directory_nonce, &field)
                .expect("directory opens");
        // `open_directory` returns the fixed-length array, so no copy is needed to know its size.
        let ledger =
            crate::directory::DirectoryLedger::decode(&plain, &[commitment]).expect("decode");
        assert_eq!(ledger.cursor, 54);
        assert_eq!(ledger.stratum, 3);
        assert_eq!(ledger.slot_count(), 1);
    }

    #[test]
    fn a_tampered_slot_fails_its_commitment() {
        let s = active(1, keys(0x50));
        let (sealed, dem) =
            seal_container_with_keys(&s, &[b"confidential"], salt(0x51), 0, &KAPPA).expect("seal");
        let mut image = vec![0u8; CONTAINER_BYTES];
        sealed.encode_into(&mut image, identity).expect("encode");

        let (off, len) = slot_range(identity(0));
        image[off] ^= 1; // flip a ciphertext bit
        let ct_len = sealed.slots[0].ciphertext.len();
        let mut commitment = [0u8; CMT4_TAG_BYTES];
        commitment.copy_from_slice(&image[off + len - CMT4_TAG_BYTES..off + len]);
        let err = open_payload_slot(
            &dem,
            1,
            0,
            &sealed.slots[0].nonce,
            &image[off..off + ct_len],
            &commitment,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            SealError::Aead(AeadError::CommitmentMismatch)
        ));
    }

    #[test]
    fn a_slot_moved_to_another_index_fails_because_the_aad_binds_it() {
        // The commitment covers `(K_DEM, nonce, AAD, ciphertext)` and the AAD carries the slot index,
        // so relocating a slot invalidates it even when ciphertext and tag travel together.
        let s = active(1, keys(0x60));
        let (sealed, dem) =
            seal_container_with_keys(&s, &[b"a", b"b"], salt(0x61), 0, &KAPPA).expect("seal");
        let ct_len = sealed.slots[0].ciphertext.len();
        let err = open_payload_slot(
            &dem,
            1,
            1, // wrong index: this slot was sealed for slot 0
            &sealed.slots[0].nonce,
            &sealed.slots[0].ciphertext[..ct_len],
            &sealed.slots[0].commitment,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            SealError::Aead(AeadError::CommitmentMismatch)
        ));
    }

    #[test]
    fn the_placement_function_cannot_overwrite_a_fixed_resident() {
        let s = active(1, keys(0x70));
        let sealed = seal_container(&s, &[b"x"], salt(0x71), 0, &KAPPA).expect("seal");
        let mut image = vec![0u8; CONTAINER_BYTES];

        for bad in [0usize, 1] {
            let err = sealed.encode_into(&mut image, |_| bad).unwrap_err();
            assert!(
                matches!(err, SealError::BadPlacement { physical, .. } if physical == bad),
                "{err}"
            );
        }
        // A colliding placement is refused rather than silently writing the same block twice.
        let two = seal_container(&s, &[b"x", b"y"], salt(0x72), 0, &KAPPA).expect("seal");
        let err = two.encode_into(&mut image, |_| 2).unwrap_err();
        assert!(
            matches!(err, SealError::BadPlacement { slot: 1, .. }),
            "{err}"
        );
    }

    #[test]
    fn an_oversized_payload_is_refused_rather_than_truncated() {
        let s = active(1, keys(0x80));
        let big = vec![0u8; PAYLOAD_PLAINTEXT_PER_SLOT + 1];
        let err = seal_container(&s, &[&big], salt(0x81), 0, &KAPPA).unwrap_err();
        assert!(
            matches!(
                err,
                SealError::PayloadTooLarge {
                    offered,
                    capacity
                } if offered == PAYLOAD_PLAINTEXT_PER_SLOT + 1
                    && capacity == PAYLOAD_PLAINTEXT_PER_SLOT
            ),
            "{err}"
        );
        // Exactly at capacity must be *writable*, not merely sealable.
        //
        // This assertion used to stop at `seal_container(...).is_ok()`, which passed while a
        // full-size slot could not be encoded: the clash between the ciphertext and the commitment
        // for the last `CMT4_TAG_BYTES` of the slot only surfaces in `encode_into`. Two halves of
        // one claim, and only the half that could not fail was being tested.
        let exact = vec![0xABu8; PAYLOAD_PLAINTEXT_PER_SLOT];
        let sealed = seal_container(&s, &[&exact], salt(0x82), 0, &KAPPA).expect("seals");
        let mut image = vec![0u8; CONTAINER_BYTES];
        sealed
            .encode_into(&mut image, identity)
            .expect("a slot filled to capacity must still encode");
        // And the ciphertext plus its commitment fill the slot exactly, with nothing overlapping.
        let (off, len) = slot_range(identity(0));
        let ct = sealed.slots[0].ciphertext.len();
        assert_eq!(
            ct + CMT4_TAG_BYTES,
            len,
            "ciphertext {} + commitment {} must be exactly one slot of {}",
            ct,
            CMT4_TAG_BYTES,
            len
        );
        assert_eq!(
            &image[off + len - CMT4_TAG_BYTES..off + len],
            &sealed.slots[0].commitment[..]
        );
    }

    #[test]
    fn more_slots_than_the_container_addresses_is_refused() {
        let s = active(1, keys(0x90));
        let slot = b"x";
        let many: Vec<&[u8]> = (0..=PAYLOAD_SLOTS).map(|_| slot.as_slice()).collect();
        let err = seal_container(&s, &many, salt(0x91), 0, &KAPPA).unwrap_err();
        assert!(matches!(err, SealError::TooManySlots { .. }), "{err}");
    }

    #[test]
    fn a_wrong_size_image_is_refused() {
        let s = active(1, keys(0xA0));
        let sealed = seal_container(&s, &[b"x"], salt(0xA1), 0, &KAPPA).expect("seal");
        for len in [0usize, SLOT_BYTES, CONTAINER_BYTES - 1, CONTAINER_BYTES + 1] {
            let mut image = vec![0u8; len];
            let err = sealed.encode_into(&mut image, identity).unwrap_err();
            assert!(
                matches!(err, SealError::WrongImageSize { found, .. } if found == len),
                "{err}"
            );
        }
    }

    #[test]
    fn each_save_produces_a_different_capsule_and_different_ciphertext() {
        // The ratchet, observed at the writer: two seals of identical plaintext under the same
        // stratum must share neither a ciphertext nor a capsule, because the envelope keypair and
        // the nonce schedule both move.
        let s = active(1, keys(0xB0));
        let a = seal_container(&s, &[b"same text"], salt(0xB1), 0, &KAPPA).expect("seal");
        let b = seal_container(&s, &[b"same text"], salt(0xB2), 1, &KAPPA).expect("seal");
        assert_ne!(a.slots[0].ciphertext, b.slots[0].ciphertext);
        assert_ne!(a.capsule.as_bytes(), b.capsule.as_bytes());
        assert_ne!(
            image_digest(a.capsule.as_bytes()),
            image_digest(b.capsule.as_bytes())
        );
    }

    #[test]
    fn the_container_depends_on_the_usb_share() {
        // The bipartite property, at the writer. If `seal_container` ignored `kappa_usb`, two
        // containers sealed with different sticks would be identical -- and a test that only checked
        // "the same input round-trips" would pass while the product lost the property.
        let s = active(1, keys(0xC8));
        let a = seal_container(&s, &[b"x"], salt(0xC9), 0, &[1u8; 32]).expect("seal");
        let b = seal_container(&s, &[b"x"], salt(0xC9), 0, &[2u8; 32]).expect("seal");
        assert_ne!(
            a.capsule.as_bytes(),
            b.capsule.as_bytes(),
            "a different USB share must produce a different capsule"
        );
    }

    #[test]
    fn debug_output_never_contains_plaintext_or_key_material() {
        let s = active(1, keys(0xC0));
        let sealed =
            seal_container(&s, &[b"SECRET-DOCUMENT-TEXT"], salt(0xC1), 0, &KAPPA).expect("seal");
        let rendered = format!("{sealed:?}");
        assert!(!rendered.contains("SECRET"), "{rendered}");
        assert!(!rendered.contains("192"), "a key byte leaked: {rendered}");
        let slot = format!("{:?}", sealed.slots[0]);
        assert!(!slot.contains("SECRET"), "{slot}");
    }
}
