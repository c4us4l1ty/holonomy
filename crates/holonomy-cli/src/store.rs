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
//!   2. read 32,768 pad sectors under the old cursor  → the pad window
//!   3. undo the pad                                  → the old capsule, and the old directory
//!   4. the handshake says whether that stratum is real
//!   5. choose a new salt        (only now; the old capsule is legible)
//!   6. seal the new container under the new salt
//!   7. fold the *new* pad window over it
//!   8. write, atomically, at the container's offset
//!   9. ratchet the sectors window `c-1` spent (writable stick only)
//! ```
//!
//! Step 9 ratchets the *old* window, not the new one. The container just written is folded with
//! window `c`, so ratcheting `c` would overwrite the pad that container needs and it would be
//! unreadable the moment the write returned. What is spent is `c-1`: its container has been
//! replaced, so refreshing those sectors is the ratchet the 54-save budget is counting, and the next
//! save reads window `c`.
//!
//! Steps 1 and 5 are the pair that can lose data. Writing the new salt before reading the old one
//! destroys the only record of which pad window the old container used, and the container becomes
//! unreadable — the salt is covered by the pad, so it is not recoverable from the file. That is why
//! `holonomy-session`'s `ContainerHeader::after_reading` consumes the header and why this function
//! takes the salt out of it rather than being handed one.
//!
//! # Where the pad comes from
//!
//! From a [`PadSource`]. With `--device` that is [`pad::DevicePad`], which reads Zone B of a real
//! stick; without one it is [`pad::DerivedPad`], which derives sector material from the permutation
//! and says so in every report. The trait exists so the second cannot be mistaken for the first: a
//! stand-in that is called directly is a stand-in that ships.
//!
//! # The pad, honestly
//!
//! One save consumes `32,768` sectors of `4,096` bytes = `134,217,728`, exactly [`CONTAINER_BYTES`],
//! because the PRD defines the two as equal. So the fold is a byte-for-byte XOR of the container
//! with one pad window, with no stretching and no derived keystream. The *permutation* decides
//! **which** sectors, not what becomes of them; that is why the fold here is over the concatenation
//! in read order, and why the store must read them in exactly the order `PadPlan` gives.
//!
//! # The epoch permutation
//!
//! Payload slots are not written in logical order. Each save chooses a fresh salt, and the salt keys
//! a permutation of the payload block range, so the same document saved twice does not share a
//! layout. The reader needs the same mapping and recovers it from the salt, which is the first thing
//! it reads — nothing is stored, because storing it would put a map to the document in the one
//! structure an adversary reads without a passphrase. See [`epoch_placement`].
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

pub mod pad;

use holonomy_core::unlock::{SealedPassphrase, Topology};
use holonomy_crypto::ParamsClass;
use holonomy_session::open::{open_container, OpenError, OpenedContainer, ReaderRequest};
use holonomy_session::{seal_container, SaltBytes, SealedContainer};
pub use pad::PadSource;

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
    pad: &mut dyn PadSource,
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

    // 2. The pad window the container on disk was written under -- which is *not* this save's.
    //
    //    This is the asymmetry the store got wrong for three rounds. `ctx.cursor` is the window
    //    this save will spend and write; the bytes already on disk were folded by the save before
    //    it, at `ctx.cursor - 1`. Unfolding with `ctx.cursor` reads the container through the wrong
    //    window, so the capsule that comes out is noise, and the handshake reports "wrong
    //    passphrase or damaged container" for a passphrase that was never wrong.
    //
    //    It has to be derived rather than stored: the cursor is in the sealed directory, and the
    //    directory cannot be read until the pad is undone. So the caller supplies the window it is
    //    *writing* and the store subtracts one for the window it is *reading*. Provisioning writes
    //    window 0, so the first real save is cursor 1 and reads window 0 -- which is exactly what
    //    `provision` printed at the end of its run.
    let old_cursor = ctx.cursor.saturating_sub(1);
    let old_pad = pad
        .window(ctx.kappa_usb, old_cursor)
        .map_err(|e| format!("reading pad window {old_cursor}: {e}"))?;
    let pad_sectors = PAD_SECTORS;

    // 3. Undo the old pad, so the old capsule is legible.
    fold(&mut old_image, old_pad.as_slice());

    // 4. The handshake over the old container. Its error is coarse, and this is where a wrong
    //    passphrase stops the save before a single byte is written.
    //
    //    The returned container is deliberately *not* used below except as proof: its stratum holds
    //    keys derived from the old save's KEM keypair, and sealing under those would produce a
    //    capsule and a payload that disagree. See step 5.
    //    The placement is `epoch_placement(&old_salt)`: the container on disk was written under the
    //    old salt's permutation, and it is the *only* permutation that can read it. This is why the
    //    epoch permutation needs no stored table -- the salt is step 1, and the map follows from it.
    let _opened: OpenedContainer = open_container(
        &ReaderRequest {
            image: &old_image,
            passphrase: ctx.passphrase,
            kappa_usb: ctx.kappa_usb,
            topology: ctx.topology,
            params: ctx.params,
            pad_fresh: true,
        },
        epoch_placement(&old_salt),
    )
    .map_err(|e: OpenError| e.to_string())?;

    // 5-6. A new salt, and a stratum for the *new* container.
    //
    // The stratum the open returned cannot be reused here. `seal_container` generates a fresh KEM
    // keypair per save and derives `K_DEM` from it, so a stratum from the old container carries
    // keys derived from the old keypair: sealing with it writes payload slots under one key while
    // the capsule in the same image is built over another. The result is a container nothing can
    // read -- including the next `save` -- reported as a wrong passphrase.
    //
    // What a save needs from the old container is proof that the credential is right, and the
    // cursor. The *keys* are re-derived from `(passphrase, new salt, κ)`, which is exactly what a
    // reader does, and therefore the only derivation that can work.
    let new_salt = SaltBytes::fresh();
    // The bytes, before `seal_container` takes the salt by value: the permutation is a function of
    // them, and a placement derived from a moved-out value would not compile for a reason worth
    // reading rather than silencing.
    let new_salt_bytes = *new_salt.as_bytes();
    let slot_bytes = split_payload(payload);
    let sealed: SealedContainer = {
        let stratum = stratum_for(ctx, &new_salt)?;
        seal_container(&stratum, &slot_bytes, new_salt, ctx.cursor, ctx.kappa_usb)
            .map_err(|e| e.to_string())?
    };

    let mut image = vec![0u8; CONTAINER_BYTES];
    sealed
        .encode_into(&mut image, epoch_placement(&new_salt_bytes))
        .map_err(|e| e.to_string())?;

    // 7-8. Fold the *new* image with *this* save's window and write.
    //
    // Step 3 read window `c - 1`; this writes window `c`. Two different windows, and the difference
    // is the point: the pad a save consumes is fresh, and the container it produces must be
    // readable only by someone with the stick and the cursor that produced it. Writing here with
    // `old_pad` would be just as wrong as reading the new cursor's window was.
    let new_pad = pad
        .window(ctx.kappa_usb, ctx.cursor)
        .map_err(|e| format!("reading pad window {}: {e}", ctx.cursor))?;
    fold(&mut image, new_pad.as_slice());
    let digest = holonomy_session::seal::image_digest(&image);
    file.write_image(&image).map_err(|e| e.to_string())?;

    // 9. Ratchet window `c-1`, *after* the container is on the device.
    //
    //    Two decisions in one line, and this tree has had both wrong.
    //
    //    **After, not before.** Ratcheting first destroys the pad the container on disk was folded
    //    with, so a write that then failed would leave a container nothing can open -- not this
    //    build, not a later one. Done after, a failed write leaves the pad intact and the previous
    //    container readable under the cursor it was last written with.
    //
    //    **`old_cursor`, not `ctx.cursor`.** The image just written is folded with window `c`, so
    //    ratcheting `c` would overwrite the pad that image needs and it would be unreadable the
    //    moment the write returned. What is spent is `c-1`: its container has been replaced, so
    //    refreshing those sectors is exactly the ratchet the 54-save budget counts, and the next
    //    save reads window `c`.
    //
    //    A source that cannot ratchet is not an error: `ratcheted` is `None` and the caller says so.
    //    That is the difference between "this pad is fresh" and "this is the pad the last save
    //    used", and a caller that conflates the two reports freshness it does not have.
    let ratcheted = if pad.can_ratchet() {
        Some(
            pad.ratchet(ctx.kappa_usb, old_cursor)
                .map_err(|e| format!("ratcheting pad window {old_cursor}: {e}"))?,
        )
    } else {
        None
    };

    let plaintext_bytes = slot_bytes.iter().map(|s| s.len()).sum();
    Ok(SaveReport {
        ratcheted,
        pad_sectors,
        plaintext_bytes,
        slots: slot_bytes.len(),
        digest,
    })
}

/// Open `file` and return the document it holds.
pub fn open_document(
    file: &mut ContainerFile,
    pad: &mut dyn PadSource,
    ctx: &SaveContext<'_>,
) -> Result<(OpenedContainer, Vec<u8>), String> {
    let mut image = file.read_image().map_err(|e| e.to_string())?;
    // The salt is deliberately *not* read here to build the window: it is inside the pad-covered
    // region, so reading it before unfolding would read folded bytes. `ctx.cursor` says which
    // window the container was written under, and that is enough -- see `PadSource`.
    let window = pad
        .window(ctx.kappa_usb, ctx.cursor)
        .map_err(|e| format!("reading pad window {}: {e}", ctx.cursor))?;
    fold(&mut image, window.as_slice());

    //    The placement comes from the salt, which is in slot 1 of the image we just unfolded. It has
    //    to be read here rather than passed in: `--cursor` says which pad window to undo, and nothing
    //    in the caller's arguments says which permutation the file was written under.
    let (salt_off, _) = slot_range(SALT_SLOT_INDEX);
    let mut file_salt = [0u8; SALT_BYTES];
    file_salt.copy_from_slice(&image[salt_off..salt_off + SALT_BYTES]);
    if file_salt.iter().all(|&b| b == 0) {
        return Err(format!(
            "{} holds no salt after unfolding; the cursor is probably wrong",
            file.path()
        ));
    }
    let opened = open_container(
        &ReaderRequest {
            image: &image,
            passphrase: ctx.passphrase,
            kappa_usb: ctx.kappa_usb,
            topology: ctx.topology,
            params: ctx.params,
            pad_fresh: true,
        },
        epoch_placement(&file_salt),
    )
    .map_err(|e: OpenError| e.to_string())?;
    let document = opened.payload_bytes();
    Ok((opened, document))
}

/// The `j`-th physical sector of save `cursor`.
///
/// The permutation key is `BLAKE2b("HOLONOMY_V51_K_PERM" ‖ κ_USB)` — derived from the stick alone, so
/// it can be computed before any byte is read. `holonomy-usb`'s `PadPlan` must produce the same
/// indices, and `the_store_and_the_usb_crate_choose_the_same_sectors` holds the two together.
///
/// # Keyed by the stick, never by the salt — and why that is forced
///
/// The per-save salt looks like the natural key: it is fresh every save, so every save would read a
/// different window. But the salt is stored inside the pad-covered region (slot 1, and the pad covers
/// the whole container — that is the point, since a plaintext salt would be the one stable
/// fingerprint in the file). A reader would therefore have to undo the pad to learn the salt, and
/// know the salt to undo the pad. Keyed by the salt, the format cannot be opened at all.
///
/// That is not hypothetical: it is why five store tests failed with "wrong passphrase or damaged
/// container" on containers the same process had written a moment earlier.
/// `the_store_round_trips_one_raw_save_without_the_pad` isolates it — the seal/open path was fine,
/// the fold was what broke, because the reader reconstructed a different window from a salt it had
/// read out of folded bytes.
///
/// So the window is keyed by `κ_USB` and the cursor. Both are inputs the reader holds *before*
/// reading anything: `κ_USB` from the stick, the cursor from the pad accounting. Save-to-save
/// disjointness comes from the cursor, via `holonomy_core::feistel::logical_sector`; the salt keeps
/// its documented job of keying the KDF, and not this one.
///
/// The consequence, stated plainly: the pad is a property of the stick, not of the file. Losing the
/// stick loses the pad, and the file alone yields nothing legible.
pub fn sector_index(kappa_usb: &[u8; 32], cursor: u64, j: usize) -> u32 {
    use holonomy_core::feistel::Feistel22;
    Feistel22::provisioned(&pad::k_perm_key(kappa_usb))
        .permute(Feistel22::logical_sector(cursor, j as u32))
}

/// Write the first container at `path`: full size, then one save of an empty document.
///
/// A container is never created empty-on-disk. A sparse file of zeros is indistinguishable from an
/// unprovisioned device, and the reader says so rather than pretending to open it — so the file is
/// filled with CSPRNG bytes first, exactly as an unformatted stick would be, and then given a real
/// capsule, a real salt and a real fold.
pub fn provision(
    path: &Path,
    pad: &mut dyn PadSource,
    ctx: &SaveContext<'_>,
) -> Result<(), String> {
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
    first_write(&mut file, pad, ctx, SaltBytes::fresh())
}

/// The stratum `(passphrase, salt, κ_USB)` derives: one derivation, used by every write path.
///
/// This is the *fallback* key set — `derive_stratum_keys` — and that is correct here, and the reason
/// is worth stating because getting it wrong is the defect this module has hit most often. A writer
/// needs only two fields from the set: `k_unwrap` and `n_epoch`, the two values the capsule is keyed
/// by. `K_DEM` is not taken from here at all; `seal_container` creates the envelope and derives it
/// from the KEM round trip. So the fallback `k_dem` is ignored by the writer, while its `k_unwrap`
/// is by construction the value the phase-1 commitment binds.
///
/// `holonomy-crypto`'s `the_fallback_keys_equal_the_ones_the_capsule_was_built_with` is where that
/// equality is asserted.
fn stratum_for(ctx: &SaveContext<'_>, salt: &SaltBytes) -> Result<ActiveStratum, String> {
    let root =
        holonomy_crypto::derive_root_secret(ctx.passphrase.as_bytes(), salt.as_bytes(), ctx.params)
            .map_err(|e| format!("deriving the root secret: {e}"))?;
    let keys = holonomy_crypto::envelope::derive_stratum_keys(&root, 1, ctx.kappa_usb);
    let id = holonomy_core::stratum::StratumId::new(1).ok_or("stratum 1 is out of range")?;
    let layer = holonomy_core::unlock::ProtectionLayer {
        pad: true,
        pad_fresh: true,
        post_quantum: true,
    };
    Ok(holonomy_core::unlock::Unlocked::new(id, ctx.topology, id, layer).into_active(keys))
}

/// Seal one container under `salt`, fold its pad window over it, and write it.
///
/// This is what `save` does at its own steps 6–8, factored out because `provision` needs the same
/// thing. Two copies of "how a container is written" is exactly the drift that made a save after
/// provisioning impossible once already: the first version wrote an unpadded capsule while `save`
/// undid a pad before opening, so the very next save reported "wrong passphrase" for a correct one.
/// Every write goes through this function for that reason, and `save` folds with the same
/// `PadSource::window` call.
pub fn first_write(
    file: &mut ContainerFile,
    pad: &mut dyn PadSource,
    ctx: &SaveContext<'_>,
    salt: SaltBytes,
) -> Result<(), String> {
    let stratum = stratum_for(ctx, &salt)?;
    // The bytes before `seal_container` consumes the salt: the permutation is a function of them.
    let salt_bytes = *salt.as_bytes();
    let sealed = seal_container(&stratum, &[&[]], salt, ctx.cursor, ctx.kappa_usb)
        .map_err(|e| e.to_string())?;
    let mut image = vec![0u8; CONTAINER_BYTES];
    sealed
        .encode_into(&mut image, epoch_placement(&salt_bytes))
        .map_err(|e| e.to_string())?;
    let window = pad
        .window(ctx.kappa_usb, ctx.cursor)
        .map_err(|e| format!("reading pad window {}: {e}", ctx.cursor))?;
    fold(&mut image, window.as_slice());
    // No ratchet here, deliberately. Provisioning *creates* window `cursor`; ratcheting it would
    // destroy the pad the container was just folded with and the first `open` would fail for a
    // container that had only just been made. A save ratchets the window it spent, because the
    // container it wrote is meant to be read under the *next* one; provisioning has no next one yet.
    file.write_image(&image).map_err(|e| e.to_string())
}

/// Write a container **without** the pad fold, for the store's tests only.
///
/// Exists so `the_store_round_trips_one_raw_save_without_the_pad` can separate "the seal/open path
/// and the salt derivation agree" from "the fold is symmetric". A test that could not remove one of
/// two variables can only report that the pair is broken, which is where this defect spent four
/// rounds before the isolation test was added.
#[cfg(test)]
pub(crate) fn first_write_unpadded_for_test(
    file: &mut ContainerFile,
    ctx: &SaveContext<'_>,
    salt: [u8; SALT_BYTES],
) -> Result<(), String> {
    let salt = SaltBytes::of_unknown_origin(salt);
    let stratum = stratum_for(ctx, &salt)?;
    let salt_bytes = *salt.as_bytes();
    let sealed = seal_container(&stratum, &[&[]], salt, ctx.cursor, ctx.kappa_usb)
        .map_err(|e| e.to_string())?;
    let mut image = vec![0u8; CONTAINER_BYTES];
    sealed
        .encode_into(&mut image, epoch_placement(&salt_bytes))
        .map_err(|e| e.to_string())?;
    file.write_image(&image).map_err(|e| e.to_string())
}

/// `K_perm` for one epoch: `BLAKE2b("HOLONOMY_V51_K_PERM_SLOT" ‖ 0x00 ‖ σ_epoch)`.
///
/// Domain-separated from [`pad::k_perm_key`] on purpose. The pad permutation is keyed by `κ_USB`,
/// because it must be computable before any byte is read; this one is keyed by `σ_epoch`, because it
/// is a property of the container rather than of the stick. Sharing a domain would make one
/// permutation a function of the other's key, and a fixed point in either would move a payload slot
/// onto the salt.
fn slot_perm_key(salt: &[u8; SALT_BYTES]) -> [u8; 32] {
    use blake2::{Blake2b512, Digest};
    let mut h = Blake2b512::new();
    h.update(b"HOLONOMY_V51_K_PERM_SLOT");
    h.update([0u8]);
    h.update(salt);
    let d = h.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&d[..32]);
    out
}

/// The epoch permutation over payload slots: logical slot `i` goes to a block the save's `σ_epoch`
/// selects.
///
/// # Why it is a function and not a stored table
///
/// The reader must recover the same mapping, and storing it would put a map to the document in the
/// very structure an adversary reads without a passphrase. So it is derived: the salt is the first
/// thing a reader extracts, and `π(salt)` is then cheap to recompute. `holonomy-session`'s
/// `open_container` takes this as its `placement` argument for exactly that reason.
///
/// # Why it is a permutation of the payload range and not of the whole container
///
/// `encode_into` pre-marks blocks 0 and 1 — the capsule and the salt — and rejects a placement that
/// returns either. So the permutation targets `[0, PAYLOAD_SLOTS)` and is shifted by
/// [`PAYLOAD_SLOT_BASE`], and every output is already a legal payload block.
///
/// It is a permutation by construction, not by argument. The first version cycle-walked each slot
/// individually — apply `π` until the result lands in range — which is how `Feistel22::permute`
/// handles Zone B and is **not** a permutation when applied per-element to a small range: a slot
/// whose first application landed in range returned immediately, while a slot whose walk passed
/// through that same point kept going, so two inputs could map to one output. The swap chain that
/// replaced it (`holonomy_core::feistel::Feistel22::swap_chain`) applies one transposition per element, and a composition
/// of transpositions cannot fail to be a permutation. It is also 2,049 times cheaper: 2,046 rounds
/// against `DOMAIN / PAYLOAD_SLOTS × PAYLOAD_SLOTS = 4.19 million`.
///
/// # What this does and does not buy
///
/// It stops a container's slot order from being a fingerprint: two saves of the same document do not
/// share a layout, and the physical position of slot `i` says nothing about `i` to anyone without the
/// salt. It does **not** hide the slot count or which blocks are in use — every container fills
/// blocks `2..` contiguously after permutation, so the occupancy map is the same for every container
/// of the same size. That is what the padding slots in the session reservoir are for, and they are
/// not wired into this store yet.
pub fn epoch_placement(salt: &[u8; SALT_BYTES]) -> impl Fn(usize) -> usize {
    use holonomy_core::layout::{PAYLOAD_SLOT_BASE, PAYLOAD_SLOTS};

    // Built once, as a table: `swap_chain` costs 2,046 permutation rounds, and a save places at most
    // 2,046 slots, so the table is paid for by one use and every later lookup is free.
    let feistel = holonomy_core::feistel::Feistel22::provisioned(&slot_perm_key(salt));
    let table: Vec<u16> = feistel
        .swap_chain(PAYLOAD_SLOTS as u32)
        .into_iter()
        // The chain permutes [0, PAYLOAD_SLOTS); shift it onto payload blocks so every output is a
        // legal block and neither the capsule (0) nor the salt (1) can be addressed.
        .map(|block| (block as usize + PAYLOAD_SLOT_BASE) as u16)
        .collect();

    move |logical: usize| {
        debug_assert!(logical < PAYLOAD_SLOTS, "logical slot out of range");
        table[logical] as usize
    }
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
