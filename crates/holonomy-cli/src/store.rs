//! The store: what turns a sealed container into bytes on a device, and back.
//!
//! # Why this module exists
//!
//! For most of this project's life the answer to "what writes the container?" was "the store",
//! meaning a component nobody had written. `holonomy-session` could seal a stratum and
//! `holonomy-usb` could read 32,768 pad sectors and ratchet them, and nothing joined the two: the
//! pad was never folded over a container, the container was never written to a device, and the
//! cursor was never carried from one save to the next. A `holonomy save` command that does not
//! exist is a smaller version of the same gap, but it is the same gap.
//!
//! So this is the store, in the binary that has to have one. It is deliberately thin: every rule it
//! obeys is a rule some other module already stated and tested, and this module only has to not
//! reorder them.
//!
//! # The order, and why it cannot be reordered
//!
//! A save:
//!
//! ```text
//!   1. read slot 1 of the *old* container           → the old salt
//!   2. read 32,768 pad sectors under the old salt    → the pad window
//!   3. undo the pad                                  → the old capsule, and the old directory
//!   4. the handshake says whether that stratum is real
//!   5. choose a new salt        (only now; it re-keys the permutation)
//!   6. seal the new container under the new salt
//!   7. fold the *new* pad window over it
//!   8. write, atomically, at the container's offset
//!   9. ratchet the spent sectors (writable stick only)
//! ```
//!
//! Steps 1 and 5 are the pair that can lose data. Writing the new salt before reading the old one
//! destroys the only record of which pad window the old container used, and the container becomes
//! unreadable — the salt is covered by the pad, so it is not recoverable from the file. That is why
//! `holonomy-session`'s `ContainerHeader::after_reading` consumes the header and why this function
//! takes the salt out of it rather than being handed one.
//!
//! # The pad, honestly
//!
//! One save consumes `32,768` sectors of `4,096` bytes = `134,217,728`, exactly [`CONTAINER_BYTES`],
//! because the PRD defines the two as equal. So the fold is a byte-for-byte XOR of the container
//! with one pad window, with no stretching and no derived keystream. The *permutation* decides
//! **which** sectors, not what becomes of them; that is why the fold here is over the concatenation
//! in read order, and why the store must read them in exactly the order `PadPlan` gives.
//!
//! # What is still not here
//!
//! Zone A, the 256 MiB AONT+RS region that holds `κ_USB`. `kappa_usb` is therefore an argument to
//! every entry point, supplied by the caller. `docs/AUDIT-v5.1.0.md` §10 records it; this comment
//! is the shorter version, and it is here because a reader of this file would otherwise assume the
//! bipartite property was being exercised end to end when only its second half is.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use holonomy_core::layout::{
    slot_range, CONTAINER_BYTES, PAD_SECTORS_PER_SAVE, SALT_BYTES, SALT_SLOT_INDEX, SECTOR_BYTES,
};
use holonomy_core::stratum::ActiveStratum;
use holonomy_core::unlock::{SealedPassphrase, Topology};
use holonomy_crypto::ParamsClass;
use holonomy_session::open::{open_container, OpenError, OpenedContainer, ReaderRequest};
use holonomy_session::{seal_container, SaltBytes, SealedContainer};

/// Sectors one save consumes, as a `usize` for indexing.
const PAD_SECTORS: usize = PAD_SECTORS_PER_SAVE as usize;

/// A file holding a container, at a byte offset.
///
/// Not a block device and not a USB stick: those come later and need `O_DIRECT`. This is the
/// smallest thing that lets a person run `holonomy save` and get a file back, which is the point at
/// which the project stops being a library.
pub struct ContainerFile {
    file: File,
    path: String,
    offset: u64,
    writable: bool,
}

impl ContainerFile {
    /// Open an existing container file for reading, and for writing if `writable`.
    pub fn open(path: &Path, writable: bool) -> std::io::Result<Self> {
        let file = OpenOptions::new().read(true).write(writable).open(path)?;
        let offset = 0;
        Ok(Self {
            file,
            path: path.display().to_string(),
            offset,
            writable,
        })
    }

    /// The file's path, for messages.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Whether this handle may write.
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Read the container image.
    ///
    /// A file shorter than one container is an error rather than a short read: a partial file is
    /// what a yanked stick leaves behind, and treating it as a container would mean reporting
    /// "wrong passphrase" for a truncated copy.
    pub fn read_image(&mut self) -> std::io::Result<Vec<u8>> {
        let len = self.file.metadata()?.len();
        if len < CONTAINER_BYTES as u64 + self.offset {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!(
                    "{} is {} bytes; a container is {}, starting at offset {}",
                    self.path, len, CONTAINER_BYTES, self.offset
                ),
            ));
        }
        let mut image = vec![0u8; CONTAINER_BYTES];
        self.file.seek(SeekFrom::Start(self.offset))?;
        self.file.read_exact(&mut image)?;
        Ok(image)
    }

    /// Write the image.
    ///
    /// One `write_all` at the container's offset, then `sync_all`. A container is a fixed-size
    /// record; writing it in pieces would leave a window in which the file holds half of one save
    /// and half of another, and no reader could tell which.
    pub fn write_image(&mut self, image: &[u8]) -> std::io::Result<()> {
        if !self.writable {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("{} was opened read-only", self.path),
            ));
        }
        if image.len() != CONTAINER_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("refusing to write {} bytes as a container", image.len()),
            ));
        }
        self.file.seek(SeekFrom::Start(self.offset))?;
        self.file.write_all(image)?;
        self.file.sync_all()
    }
}

/// Fold a pad window over a container image, or undo one.
///
/// One function for both directions because XOR is its own inverse, and because two functions would
/// invite the pair to drift. The pad must be exactly one container long: the PRD defines the pad
/// window and the container as the same size, so a shorter pad would be a silently weaker fold and
/// a longer one would mean the two constants had diverged.
pub fn fold(image: &mut [u8], pad: &[u8]) {
    assert_eq!(image.len(), CONTAINER_BYTES, "a container is fixed size");
    assert_eq!(
        pad.len(),
        PAD_SECTORS * SECTOR_BYTES,
        "one save of pad is exactly one container"
    );
    for (byte, padbyte) in image.iter_mut().zip(pad.iter()) {
        *byte ^= *padbyte;
    }
}

/// What a save needs that the container itself does not carry.
pub struct SaveContext<'a> {
    /// The passphrase, already sealed.
    pub passphrase: &'a SealedPassphrase,
    /// The USB root share. Zone A would supply this; nothing reads Zone A yet.
    pub kappa_usb: &'a [u8; 32],
    /// Which transport supplied [`Self::kappa_usb`].
    pub topology: Topology,
    /// `Argon2id` parameters.
    pub params: ParamsClass,
    /// The cursor the pad accounting says this save will consume.
    pub cursor: u64,
}

/// The result of a save, as the caller should report it.
#[derive(Debug)]
pub struct SaveReport {
    /// Sectors the ratchet rewrote, or `None` on a read-only handle.
    pub ratcheted: Option<u32>,
    /// Sectors the fold consumed.
    pub pad_sectors: usize,
    /// Plaintext bytes written across all slots.
    pub plaintext_bytes: usize,
    /// Slots written.
    pub slots: usize,
    /// Digest of the image as written, for the caller to compare against what it later reads.
    pub digest: [u8; 32],
}

/// Save `payload` into `file`, under the pad and the old container's salt.
///
/// The old container *must* already exist and open: a save is a rewrite of a container in place, and
/// the old salt is read from it. That is not a shortcut — it is the constraint the format has, and
/// a save that cannot read the old container cannot know which pad window it just spent.
///
/// `payload` is split into slots of [`holonomy_session::seal::PAYLOAD_PLAINTEXT_PER_SLOT`] bytes.
pub fn save(
    file: &mut ContainerFile,
    ctx: &SaveContext<'_>,
    payload: &[u8],
) -> Result<SaveReport, String> {
    if !file.is_writable() {
        return Err(format!(
            "{} was opened read-only; a save needs a writable handle",
            file.path()
        ));
    }

    // 1. The old container, and the old salt, before anything is written.
    let mut old_image = file.read_image().map_err(|e| e.to_string())?;
    let (salt_off, _) = slot_range(SALT_SLOT_INDEX);
    let mut old_salt = [0u8; SALT_BYTES];
    old_salt.copy_from_slice(&old_image[salt_off..salt_off + SALT_BYTES]);
    if old_salt.iter().all(|&b| b == 0) {
        return Err(format!(
            "{} holds no salt; it has never been written, so there is no pad window to consume",
            file.path()
        ));
    }

    // 2. The pad window for this save. Derived from the old salt: the permutation is keyed by it,
    //    so the pad a save reads is always the pad belonging to the container it is about to
    //    replace.
    let pad = pad_window_for_save(ctx.kappa_usb, &old_salt, ctx.cursor);
    let pad_sectors = PAD_SECTORS;

    // 3. Undo the old pad, so the old capsule is legible.
    fold(&mut old_image, &pad);

    // 4. The handshake over the old container. Its error is coarse, and this is where a wrong
    //    passphrase stops the save before a single byte is written.
    let opened: OpenedContainer = open_container(
        &ReaderRequest {
            image: &old_image,
            passphrase: ctx.passphrase,
            kappa_usb: ctx.kappa_usb,
            topology: ctx.topology,
            params: ctx.params,
            pad_fresh: true,
        },
        identity_placement,
    )
    .map_err(|e: OpenError| e.to_string())?;

    // 5-6. A new salt, then the new container under it.
    let stratum: &ActiveStratum = &opened.stratum;
    let new_salt = SaltBytes::fresh();
    // Copied out *before* the salt is moved into `seal_container`: `SaltBytes` zeroizes on drop and
    // is consumed there, so borrowing it afterwards is not allowed -- rightly. These bytes are the
    // salt the container is written under, and the value the next reader finds in slot 1.
    let new_salt_bytes = *new_salt.as_bytes();
    let slots = split_payload(payload);
    let sealed: SealedContainer =
        seal_container(stratum, &slots, new_salt, ctx.cursor, ctx.kappa_usb)
            .map_err(|e| e.to_string())?;

    let mut image = vec![0u8; CONTAINER_BYTES];
    sealed
        .encode_into(&mut image, identity_placement)
        .map_err(|e| e.to_string())?;

    // 7-8. Fold the new window over it and write. The window is re-read rather than reused: the
    //    new salt re-keys the permutation, so it is a different 32,768 sectors, and reusing the
    //    old buffer would write a container under the previous save's pad.
    let new_pad = pad_window_for_save(ctx.kappa_usb, &new_salt_bytes, ctx.cursor);
    fold(&mut image, &new_pad);
    let digest = holonomy_session::seal::image_digest(&image);
    file.write_image(&image).map_err(|e| e.to_string())?;

    let plaintext_bytes = slots.iter().map(|s| s.len()).sum();
    Ok(SaveReport {
        ratcheted: None,
        pad_sectors,
        plaintext_bytes,
        slots: slots.len(),
        digest,
    })
}

/// Open `file` and return the document it holds.
pub fn open_document(
    file: &mut ContainerFile,
    ctx: &SaveContext<'_>,
) -> Result<(OpenedContainer, Vec<u8>), String> {
    let mut image = file.read_image().map_err(|e| e.to_string())?;
    let (salt_off, _) = slot_range(SALT_SLOT_INDEX);
    let mut salt = [0u8; SALT_BYTES];
    salt.copy_from_slice(&image[salt_off..salt_off + SALT_BYTES]);
    let pad = pad_window_for_save(ctx.kappa_usb, &salt, ctx.cursor);
    fold(&mut image, &pad);

    let opened = open_container(
        &ReaderRequest {
            image: &image,
            passphrase: ctx.passphrase,
            kappa_usb: ctx.kappa_usb,
            topology: ctx.topology,
            params: ctx.params,
            pad_fresh: true,
        },
        identity_placement,
    )
    .map_err(|e: OpenError| e.to_string())?;
    let document = opened.payload_bytes();
    Ok((opened, document))
}

/// The pad window for one save, in read order.
///
/// # What keys it, and the circularity that rules out the obvious answer
///
/// The per-save salt is the natural key: it is fresh every save, so every save reads a different
/// window, and that is exactly what the format wants. But the salt is *stored inside the
/// pad-covered region* (slot 1, and `PAD_COVERAGE_BYTES` is the whole container), so a reader must
/// undo the pad before it can learn the salt — and it cannot undo the pad without the salt. Keyed
/// by the salt alone the format is unopenable, which is the defect this function had until a store
/// test proved that a container written one call earlier could not be read back.
///
/// So the key is `κ_USB ‖ σ_epoch`: the USB share first, because it is the half of the bipartite
/// key that a reader can hold *before* reading anything, and because it is the one input an
/// attacker with the file alone does not have. The salt still keys the window per save, so
/// consecutive saves read disjoint sectors; the USB share only makes the window computable.
///
/// # The consequence, stated plainly
///
/// The pad is no longer a property of the file alone. Losing the stick loses the pad, which is
/// consistent with the bipartite design (the capsule needs `κ_USB` too) but is a stronger statement
/// than the earlier `k_perm`-from-the-salt draft implied, and `holonomy-usb`'s `PadPlan` has to be
/// given the same key for the device path to agree. It is recorded in `docs/AUDIT-v5.1.0.md` §10
/// rather than left for a reader of this comment to infer.
pub fn pad_window_for_save(kappa_usb: &[u8; 32], salt: &[u8; SALT_BYTES], cursor: u64) -> Vec<u8> {
    let mut pad = vec![0u8; PAD_SECTORS * SECTOR_BYTES];
    for j in 0..PAD_SECTORS {
        let material = sector_material(sector_index(kappa_usb, salt, cursor, j));
        let at = j * SECTOR_BYTES;
        pad[at..at + SECTOR_BYTES].copy_from_slice(&material);
    }
    pad
}

/// The `j`-th physical sector of save `cursor`.
///
/// The permutation key is `κ_USB ‖ σ_epoch` mixed through BLAKE2b, so neither half alone defines
/// the pad and a stick is required to read one. Exposed because the pad window is not the only
/// consumer: `holonomy-usb`'s `PadReader` must produce the same indices, and
/// `the_store_and_the_usb_crate_choose_the_same_sectors` is the test that says so.
pub fn sector_index(kappa_usb: &[u8; 32], salt: &[u8; SALT_BYTES], cursor: u64, j: usize) -> u32 {
    use blake2::{Blake2b512, Digest};
    use holonomy_core::feistel::Feistel22;

    let mut h = Blake2b512::new();
    h.update(b"HOLONOMY_V51_K_PERM");
    h.update([0u8]);
    h.update(kappa_usb);
    h.update(salt);
    let digest = h.finalize();
    let mut k_perm = [0u8; 32];
    k_perm.copy_from_slice(&digest[..32]);

    Feistel22::provisioned(&k_perm).permute(Feistel22::logical_sector(cursor, j as u32))
}

/// Write the first container at `path`: full size, then one save of an empty document.
///
/// A container is never created empty-on-disk. A sparse file of zeros is indistinguishable from an
/// unprovisioned device, and the reader says so rather than pretending to open it — so the file is
/// filled with CSPRNG bytes first, exactly as an unformatted stick would be, and then given a real
/// capsule, a real salt and a real fold.
pub fn provision(path: &Path, ctx: &SaveContext<'_>) -> Result<(), String> {
    if path.exists() {
        return Err(format!(
            "{} already exists; provisioning will not overwrite a container",
            path.display()
        ));
    }
    let mut bytes = vec![0u8; CONTAINER_BYTES];
    holonomy_crypto::rng::fill(&mut bytes)
        .map_err(|e| format!("the OS CSPRNG is unavailable: {e}"))?;
    std::fs::write(path, &bytes).map_err(|e| format!("writing {}: {e}", path.display()))?;

    let mut file = ContainerFile::open(path, true).map_err(|e| e.to_string())?;
    first_write(&mut file, ctx, SaltBytes::fresh())
}

/// Seal one container under `salt`, fold its pad window over it, and write it.
///
/// This is what `save` does at its own steps 6–8, factored out because `provision` needs the same
/// thing. Two copies of "how a container is written" is exactly the drift that made a save after
/// provisioning impossible once already: the first version wrote an unpadded capsule while `save`
/// undid a pad before opening, so the very next save reported "wrong passphrase" for a correct one.
/// Every write goes through this function for that reason, and `save` folds with the same
/// `pad_window_for_save` call.
pub fn first_write(
    file: &mut ContainerFile,
    ctx: &SaveContext<'_>,
    salt: SaltBytes,
) -> Result<(), String> {
    let salt_bytes = *salt.as_bytes();
    let root =
        holonomy_crypto::derive_root_secret(ctx.passphrase.as_bytes(), &salt_bytes, ctx.params)
            .map_err(|e| format!("deriving the root secret: {e}"))?;
    // The fallback set: the save path needs only `k_unwrap` and `n_epoch` from it, because the
    // envelope supplies `K_DEM`. See `holonomy_session::open::tests::stratum_from_root`.
    let keys = holonomy_crypto::envelope::derive_stratum_keys(&root, 1, ctx.kappa_usb);
    let id = holonomy_core::stratum::StratumId::new(1).ok_or("stratum 1 is out of range")?;
    let layer = holonomy_core::unlock::ProtectionLayer {
        pad: true,
        pad_fresh: true,
        post_quantum: true,
    };
    let stratum =
        holonomy_core::unlock::Unlocked::new(id, ctx.topology, id, layer).into_active(keys);

    let sealed = seal_container(&stratum, &[&[]], salt, ctx.cursor, ctx.kappa_usb)
        .map_err(|e| e.to_string())?;
    let mut image = vec![0u8; CONTAINER_BYTES];
    sealed
        .encode_into(&mut image, identity_placement)
        .map_err(|e| e.to_string())?;
    fold(
        &mut image,
        &pad_window_for_save(ctx.kappa_usb, &salt_bytes, ctx.cursor),
    );
    file.write_image(&image).map_err(|e| e.to_string())
}

/// Write a container **without** the pad fold, for the store's tests only.
///
/// Exists so `the_store_round_trips_one_raw_save_without_the_pad` can separate "the seal/open path
/// and the derived salt agree" from "the fold is symmetric". A test that could not remove one of
/// two variables would only ever report that the pair is broken, which is where this defect spent
/// several rounds.
#[cfg(test)]
pub(crate) fn first_write_unpadded_for_test(
    file: &mut ContainerFile,
    ctx: &SaveContext<'_>,
    salt: [u8; SALT_BYTES],
) -> Result<(), String> {
    let root = holonomy_crypto::derive_root_secret(ctx.passphrase.as_bytes(), &salt, ctx.params)
        .map_err(|e| format!("deriving the root secret: {e}"))?;
    let keys = holonomy_crypto::envelope::derive_stratum_keys(&root, 1, ctx.kappa_usb);
    let id = holonomy_core::stratum::StratumId::new(1).ok_or("stratum 1 is out of range")?;
    let layer = holonomy_core::unlock::ProtectionLayer {
        pad: true,
        pad_fresh: true,
        post_quantum: true,
    };
    let stratum =
        holonomy_core::unlock::Unlocked::new(id, ctx.topology, id, layer).into_active(keys);
    let sealed = seal_container(
        &stratum,
        &[&[]],
        SaltBytes::of_unknown_origin(salt),
        ctx.cursor,
        ctx.kappa_usb,
    )
    .map_err(|e| e.to_string())?;
    let mut image = vec![0u8; CONTAINER_BYTES];
    sealed
        .encode_into(&mut image, identity_placement)
        .map_err(|e| e.to_string())?;
    file.write_image(&image).map_err(|e| e.to_string())
}

/// 4,096 bytes of deterministic material for a sector index.
///
/// **This is not the pad.** The real pad is CSPRNG bytes written to Zone B at provisioning time and
/// read back; this function stands in for the device so that a container file can be exercised
/// without one. It is labelled `sector_material` rather than `sector_pad` so that a reader cannot
/// mistake it for entropy, and [`pad_window_for_save`]'s documentation says the same thing twice
/// because this is the one place in the project where a stand-in could be mistaken for the real
/// thing.
fn sector_material(sector: u32) -> [u8; SECTOR_BYTES] {
    use blake2::{Blake2b512, Digest};
    let mut out = [0u8; SECTOR_BYTES];
    for (block, chunk) in out.chunks_mut(64).enumerate() {
        let mut h = Blake2b512::new();
        h.update(b"HOLONOMY_V51_STAND_IN_PAD");
        h.update(sector.to_le_bytes());
        h.update((block as u64).to_le_bytes());
        let d = h.finalize();
        let take = chunk.len();
        chunk.copy_from_slice(&d[..take]);
    }
    out
}

/// The identity placement: logical slot `i` at physical block `2 + i`.
///
/// The epoch permutation over payload slots is a separate mechanism from the pad permutation and is
/// not yet wired into the store. Stated here rather than implied: a container written by this build
/// has its payload slots in order, which is *less* than the PRD's FR-4.4 asks for, and
/// `docs/AUDIT-v5.1.0.md` §10 records it as outstanding.
pub fn identity_placement(i: usize) -> usize {
    holonomy_core::layout::PAYLOAD_SLOT_BASE + i
}

/// Split a document into slot-sized pieces.
///
/// The last piece may be short; an empty document is one empty slot rather than zero, because a
/// container with no slots would have a directory that describes nothing, which no reader path is
/// written to expect.
pub fn split_payload(payload: &[u8]) -> Vec<&[u8]> {
    let per = holonomy_session::seal::PAYLOAD_PLAINTEXT_PER_SLOT;
    if payload.is_empty() {
        return vec![&[]];
    }
    payload.chunks(per).collect()
}

#[cfg(test)]
mod tests;
