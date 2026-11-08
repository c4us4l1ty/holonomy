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
//!   1. read the *old* container                      → its bytes, still folded
//!   2. read 32,768 pad sectors under the old cursor  → the pad window
//!   3. undo the pad                                  → the old salt, capsule and directory
//!   4. the handshake says whether that stratum is real
//!   5. choose a new salt        (only now; the old capsule is legible)
//!   6. seal the new container under the new salt
//!   7. fold the *new* pad window over it
//!   8. stage and replace the encrypted image, then sync its directory (Unix)
//!   9. ratchet the sectors window `c-1` spent (writable stick only)
//! ```
//!
//! Step 9 ratchets the *old* window, not the new one. The container just written is folded with
//! window `c`, so ratcheting `c` would overwrite the pad that container needs and it would be
//! unreadable the moment the write returned. What is spent is `c-1`: its container has been
//! replaced, so refreshing those sectors is the ratchet the 54-save budget is counting, and the next
//! save reads window `c`.
//!
//! Steps 3 and 5 are the pair that can lose data. Writing the new salt before recovering the old one
//! destroys the only record of which pad window the old container used *and* which permutation its
//! payload was written under, and the container becomes unreadable — the salt is covered by the pad,
//! so it is not recoverable from the file afterwards. That is why `holonomy-session`'s
//! `ContainerHeader::after_reading` consumes the header and why this function takes the salt out of
//! it rather than being handed one.
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
use std::path::{Path, PathBuf};

use holonomy_core::layout::{
    slot_range, CONTAINER_BYTES, PAD_SECTORS_PER_SAVE, SALT_BYTES, SALT_SLOT_INDEX,
    SAVES_BEFORE_DOMAIN_REUSE, SECTOR_BYTES,
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
    filesystem_path: PathBuf,
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
            filesystem_path: path.to_path_buf(),
            offset,
            writable,
        })
    }

    /// The file's path, for messages.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The original filesystem path, retaining platform-native path bytes.
    pub(crate) fn filesystem_path(&self) -> &Path {
        &self.filesystem_path
    }

    /// Whether this handle may write.
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Read the container image.
    ///
    /// A standalone file must contain exactly one container. A partial file is an error rather
    /// than a short read, and extra bytes are refused rather than silently discarded on save.
    /// Embedded containers and trailing metadata are not supported by this file store.
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
        if len > CONTAINER_BYTES as u64 + self.offset {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "{} is {} bytes; a standalone container must be exactly {} bytes; extra data will not be discarded",
                    self.path, len, CONTAINER_BYTES
                ),
            ));
        }
        let mut image = vec![0u8; CONTAINER_BYTES];
        self.file.seek(SeekFrom::Start(self.offset))?;
        self.file.read_exact(&mut image)?;
        if self.file.metadata()?.len() != len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "container size changed while reading; the image is not a reliable snapshot",
            ));
        }
        Ok(image)
    }

    /// Write the image.
    ///
    /// Writes in place with `write_all` and then `sync_all`. This is not atomic: a failed
    /// write or crash can leave a partially replaced, unreadable container. Keep a backup.
    pub(crate) fn write_image(&mut self, image: &[u8]) -> std::io::Result<()> {
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

    /// Hash the folded image on the open handle without allocating another image.
    /// This is only a best-effort change detector, not a lock or atomic snapshot.
    #[cfg(unix)]
    fn folded_digest(&mut self) -> std::io::Result<[u8; 32]> {
        use blake2::{Blake2b512, Digest};

        if self.file.metadata()?.len() != CONTAINER_BYTES as u64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "container size changed while checking the original image",
            ));
        }
        self.file.seek(SeekFrom::Start(0))?;
        let mut hash = Blake2b512::new();
        let mut buf = [0u8; 64 * 1024];
        let mut remaining = CONTAINER_BYTES;
        while remaining > 0 {
            let len = remaining.min(buf.len());
            self.file.read_exact(&mut buf[..len])?;
            hash.update(&buf[..len]);
            remaining -= len;
        }
        if self.file.metadata()?.len() != CONTAINER_BYTES as u64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "container size changed while checking the original image",
            ));
        }
        let result = hash.finalize();
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&result[..32]);
        Ok(digest)
    }

    /// Reject symlinks or a path no longer naming the file this handle opened.
    /// This check is not atomic with later I/O or replacement.
    #[cfg(unix)]
    fn validate_open_path(&self) -> std::io::Result<std::fs::Metadata> {
        use std::os::unix::fs::MetadataExt;

        let original = self.file.metadata()?;
        let path_metadata = std::fs::symlink_metadata(&self.filesystem_path)?;
        if !path_metadata.file_type().is_file()
            || original.dev() != path_metadata.dev()
            || original.ino() != path_metadata.ino()
        {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "container path must be the same regular file that was opened; symlinks and changed paths are refused"));
        }
        Ok(original)
    }

    /// Refuse writes through hard-linked container paths. Atomic replacement updates only the
    /// selected directory entry; leaving another hard link on the old image would create a stale
    /// fork that shares the same pad history.
    #[cfg(unix)]
    pub(crate) fn validate_single_link(&self) -> std::io::Result<()> {
        use std::os::unix::fs::MetadataExt;

        let metadata = self.validate_open_path()?;
        if metadata.nlink() != 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "container has {} hard links; writable saves require exactly one link so replacement cannot leave a stale copy",
                    metadata.nlink()
                ),
            ));
        }
        Ok(())
    }

    /// Stage a complete encrypted image beside the original and replace it on Unix.
    ///
    /// A successful return means the staged file and its containing directory were synced.
    /// A directory-sync error can occur *after* replacement, and is reported as such. This
    /// compares the old image again before replacement, but does not serialize writers,
    /// close the check-to-rename race, or guarantee storage hardware honors flushes.
    #[cfg(unix)]
    pub(crate) fn replace_image(
        &mut self,
        image: &[u8],
        expected_old_digest: &[u8; 32],
    ) -> std::io::Result<()> {
        use std::os::unix::fs::MetadataExt;

        if !self.writable {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "container is read-only",
            ));
        }
        if self.offset != 0 || image.len() != CONTAINER_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "replacement requires one whole container at offset zero",
            ));
        }
        let original = self.validate_open_path()?;
        if original.len() != CONTAINER_BYTES as u64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "container size changed or is not exactly one standalone image; replacement refused",
            ));
        }
        let parent = self
            .filesystem_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf();
        let stage_prefix = remnant_v2_prefix(original.dev(), original.ino(), expected_old_digest);
        let mut staged = tempfile::Builder::new()
            .prefix(&stage_prefix)
            .tempfile_in(&parent)?;
        staged
            .as_file_mut()
            .set_permissions(original.permissions())?;
        staged.as_file_mut().write_all(image)?;
        staged.as_file().sync_all()?;

        // Detect in-place changes to the image read earlier, including changes of
        // the same length. A writer can still race this check or modify a pad: this
        // is not a substitute for coordinating writers and pad accounting.
        if self.folded_digest()? != *expected_old_digest {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "container contents changed while saving; replacement refused",
            ));
        }
        // Catch stale handles before replacement. This does not close the race with a
        // concurrent writer between the check and rename; callers must serialize saves.
        let current = std::fs::symlink_metadata(&self.filesystem_path)?;
        if !current.file_type().is_file()
            || current.dev() != original.dev()
            || current.ino() != original.ino()
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "container path changed while saving; refusing to replace it",
            ));
        }
        if current.len() != CONTAINER_BYTES as u64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "container size changed while saving; replacement refused",
            ));
        }
        if current.nlink() != 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "container acquired {} hard links while saving; refusing replacement to avoid leaving a stale copy",
                    current.nlink()
                ),
            ));
        }
        let new_file = staged.persist(&self.filesystem_path).map_err(|e| e.error)?;
        self.file = new_file;
        File::open(&parent).and_then(|dir| dir.sync_all()).map_err(|e| {
            std::io::Error::new(e.kind(), format!("container replacement occurred, but directory sync failed; durability is uncertain and pad ratcheting was not started: {e}"))
        })
    }

    /// Non-Unix saves are refused until atomic replacement and directory durability are implemented.
    #[cfg(not(unix))]
    pub(crate) fn replace_image(
        &mut self,
        _image: &[u8],
        _expected_old_digest: &[u8; 32],
    ) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!(
                "safe container replacement for {} is not implemented on this platform",
                self.filesystem_path.display()
            ),
        ))
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
    /// Sector count reported by the pad source, or `None` when no ratchet was attempted.
    /// A full reported count is not independent verification of device writes or freshness.
    pub ratcheted: Option<u32>,
    /// Sectors the fold consumed.
    pub pad_sectors: usize,
    /// Plaintext bytes written across all slots.
    pub plaintext_bytes: usize,
    /// Slots written.
    pub slots: usize,
    /// Digest of the image as written, for the caller to compare against what it later reads.
    pub digest: [u8; 32],
    /// The protection layer attached to this write. Its `pad_fresh` is `None`: the store folds a
    /// pad but cannot establish that no sector was reused elsewhere.
    pub protection: holonomy_core::unlock::ProtectionLayer,
}

/// Origin information encoded in a staged-image filename.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemnantOrigin {
    /// New-format marker identifying the original file object and exact old image bytes.
    Bound {
        /// Unix device number of the original container.
        device: u64,
        /// Unix inode number of the original container.
        inode: u64,
        /// Digest of the complete old container image before the staged save.
        old_digest: [u8; 32],
    },
    /// Legacy `.holonomy-save-*` name with no origin marker.
    LegacyUnbound,
    /// Name claims the new format but its marker is malformed; do not downgrade it to legacy.
    MalformedBound,
}

/// Whether a remnant's encoded origin corresponds to the currently named container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemnantAssociation {
    /// The original container identity and pre-save bytes match this path now.
    BoundToCurrentContainer,
    /// A legacy name contains no association evidence; explicit operator choice is required.
    LegacyUnbound,
    /// A new-format origin marker is valid but does not match this container's current identity/state.
    DoesNotMatch,
    /// The name claims a binding marker but cannot be parsed safely.
    MalformedBinding,
}

/// A regular file matching the staging-name prefix; the name alone does not prove its origin.
#[derive(Debug)]
pub struct SaveRemnant {
    /// The staged path, inside the container's directory.
    pub path: PathBuf,
    /// Size on disk. A complete image is [`CONTAINER_BYTES`]; any other size is incomplete, but does not establish why.
    pub len: u64,
    /// Filename-encoded origin evidence. It is not authenticated and can be forged by a directory writer.
    pub origin: RemnantOrigin,
}

/// How many remnants a scan reports. The scan returns the lexicographically first entries, not
/// whichever files the filesystem happened to enumerate first; an explicit candidate check is not
/// limited by this display cap.
pub const MAX_REMNANTS_REPORTED: usize = 20;

#[cfg(unix)]
fn remnant_v2_prefix(device: u64, inode: u64, old_digest: &[u8; 32]) -> String {
    let digest = old_digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!(".holonomy-save-v2-{device:016x}-{inode:016x}-{digest}-")
}

fn parse_remnant_origin(path: &Path) -> RemnantOrigin {
    let Some(os_name) = path.file_name() else {
        return RemnantOrigin::LegacyUnbound;
    };
    if !os_name.as_encoded_bytes().starts_with(b".holonomy-save-v2") {
        return RemnantOrigin::LegacyUnbound;
    }
    let Some(name) = os_name.to_str() else {
        return RemnantOrigin::MalformedBound;
    };
    let Some(encoded) = name.strip_prefix(".holonomy-save-v2-") else {
        return RemnantOrigin::MalformedBound;
    };
    let mut fields = encoded.splitn(4, '-');
    let (Some(device), Some(inode), Some(digest), Some(nonce)) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return RemnantOrigin::MalformedBound;
    };
    if device.len() != 16 || inode.len() != 16 || nonce.is_empty() {
        return RemnantOrigin::MalformedBound;
    }
    let (Ok(device), Ok(inode), Some(old_digest)) = (
        u64::from_str_radix(device, 16),
        u64::from_str_radix(inode, 16),
        parse_digest_hex(digest),
    ) else {
        return RemnantOrigin::MalformedBound;
    };
    RemnantOrigin::Bound {
        device,
        inode,
        old_digest,
    }
}

fn parse_digest_hex(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut digest = [0u8; 32];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let byte = std::str::from_utf8(pair).ok()?;
        digest[index] = u8::from_str_radix(byte, 16).ok()?;
    }
    Some(digest)
}

fn remnant_directory(container: &Path) -> PathBuf {
    if container.is_dir() {
        container.to_path_buf()
    } else {
        container
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf()
    }
}

/// Check whether `candidate` is a regular `.holonomy-save-*` sibling of `container`.
///
/// This checks only the directory, name prefix, and regular-file type; it does not evaluate the
/// name's origin marker or authenticate file contents. Call [`remnant_association`] separately.
pub fn is_save_remnant_beside(container: &Path, candidate: &Path) -> std::io::Result<bool> {
    let directory = remnant_directory(container);
    let candidate_directory = candidate
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if std::fs::canonicalize(&directory)? != std::fs::canonicalize(candidate_directory)? {
        return Ok(false);
    }
    let Some(name) = candidate.file_name() else {
        return Ok(false);
    };
    if !name.as_encoded_bytes().starts_with(b".holonomy-save-") {
        return Ok(false);
    }
    match std::fs::symlink_metadata(candidate) {
        Ok(metadata) => Ok(metadata.file_type().is_file()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Stable identity and complete-image fingerprint for one currently opened container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContainerOrigin {
    device: u64,
    inode: u64,
    image_digest: [u8; 32],
}

/// Read the identity and full-image digest used to check v2 remnant markers.
pub fn current_container_origin(path: &Path) -> std::io::Result<ContainerOrigin> {
    #[cfg(unix)]
    {
        let (device, inode, image_digest) = container_origin_unix(path)?;
        Ok(ContainerOrigin {
            device,
            inode,
            image_digest,
        })
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "v2 remnant origin checks require Unix device/inode metadata",
        ))
    }
}

/// Compare a staged remnant's encoded origin with the current container.
///
/// New-format names bind to the old container's Unix device/inode and complete-image digest. This
/// is operational provenance, not a MAC: anyone who can create files in the directory can forge
/// the name. Legacy names remain explicitly classed as unbound.
pub fn remnant_association(
    container: &Path,
    candidate: &Path,
) -> std::io::Result<RemnantAssociation> {
    if !is_save_remnant_beside(container, candidate)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "selected path is not a regular .holonomy-save-* sibling",
        ));
    }
    let current_origin = match parse_remnant_origin(candidate) {
        RemnantOrigin::Bound { .. } => Some(current_container_origin(container)?),
        RemnantOrigin::LegacyUnbound | RemnantOrigin::MalformedBound => None,
    };
    remnant_association_with_origin(container, candidate, current_origin.as_ref())
}

/// Evaluate a candidate using a container origin already computed for a batch.
///
/// Passing no origin is sufficient for legacy or malformed names. A valid v2 marker requires
/// `current_origin`; callers scanning several candidates should compute it once and reuse it.
pub fn remnant_association_with_origin(
    container: &Path,
    candidate: &Path,
    current_origin: Option<&ContainerOrigin>,
) -> std::io::Result<RemnantAssociation> {
    if !is_save_remnant_beside(container, candidate)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "selected path is not a regular .holonomy-save-* sibling",
        ));
    }
    match parse_remnant_origin(candidate) {
        RemnantOrigin::LegacyUnbound => Ok(RemnantAssociation::LegacyUnbound),
        RemnantOrigin::MalformedBound => Ok(RemnantAssociation::MalformedBinding),
        RemnantOrigin::Bound {
            device,
            inode,
            old_digest,
        } => {
            let Some(current_origin) = current_origin else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "current container origin was not computed",
                ));
            };
            if (device, inode, old_digest)
                == (
                    current_origin.device,
                    current_origin.inode,
                    current_origin.image_digest,
                )
            {
                Ok(RemnantAssociation::BoundToCurrentContainer)
            } else {
                Ok(RemnantAssociation::DoesNotMatch)
            }
        }
    }
}

#[cfg(unix)]
fn container_origin_unix(path: &Path) -> std::io::Result<(u64, u64, [u8; 32])> {
    use std::os::unix::fs::MetadataExt;

    let mut file = OpenOptions::new().read(true).open(path)?;
    let opened = file.metadata()?;
    let path_metadata = std::fs::symlink_metadata(path)?;
    if !opened.file_type().is_file()
        || !path_metadata.file_type().is_file()
        || opened.dev() != path_metadata.dev()
        || opened.ino() != path_metadata.ino()
        || opened.len() != CONTAINER_BYTES as u64
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "container path is not a stable whole regular file",
        ));
    }
    let opened_modified = opened.modified()?;
    let digest = file_digest(&mut file)?;
    let after = file.metadata()?;
    let after_modified = after.modified()?;
    let path_after = std::fs::symlink_metadata(path)?;
    if after.dev() != opened.dev()
        || after.ino() != opened.ino()
        || after.len() != opened.len()
        || after_modified != opened_modified
        || !path_after.file_type().is_file()
        || path_after.dev() != opened.dev()
        || path_after.ino() != opened.ino()
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "container changed while its remnant association was checked",
        ));
    }
    Ok((opened.dev(), opened.ino(), digest))
}

/// Open an explicitly selected remnant and keep using that handle for verification and copying.
/// On Unix, `O_NOFOLLOW` plus descriptor/path identity checks closes the final-component
/// check-then-open race. Other platforms still need their own no-follow implementation. This
/// function validates only the selected path and handle; callers must separately evaluate
/// [`remnant_association`] and authenticate the staged image.
pub fn open_save_remnant_beside(
    container: &Path,
    candidate: &Path,
) -> std::io::Result<ContainerFile> {
    if !is_save_remnant_beside(container, candidate)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "selected path is not a regular .holonomy-save-* sibling",
        ));
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    let file = options.open(candidate)?;
    let opened = file.metadata()?;
    let path_metadata = std::fs::symlink_metadata(candidate)?;
    if !opened.file_type().is_file()
        || !path_metadata.file_type().is_file()
        || opened.len() != CONTAINER_BYTES as u64
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "selected remnant is not a whole regular container image",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != path_metadata.dev() || opened.ino() != path_metadata.ino() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "selected remnant changed while it was being opened",
            ));
        }
    }

    Ok(ContainerFile {
        file,
        path: candidate.display().to_string(),
        filesystem_path: candidate.to_path_buf(),
        offset: 0,
        writable: false,
    })
}

/// Find `.holonomy-save-*` staged images next to `container`, or inside it when it is a directory.
///
/// Saves use that prefix for a temporary staged image, so a surviving matching file can be a
/// crash remnant. The name alone does not prove it came from a save: files can be copied or planted,
/// and even v2 origin markers are forgeable. Nor does a matching name prove the container is intact;
/// another writer or an earlier failure can change it. This is a directory listing, not a recovery
/// protocol, and it decrypts nothing.
///
/// The scan examines all matching regular files to choose a stable lexicographic prefix, while
/// retaining at most [`MAX_REMNANTS_REPORTED`] paths. The second element is true iff more matched.
pub fn find_save_remnants(container: &Path) -> std::io::Result<(Vec<SaveRemnant>, bool)> {
    let directory = remnant_directory(container);
    let mut found = Vec::new();
    let mut truncated = false;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        if !name.as_encoded_bytes().starts_with(b".holonomy-save-") {
            continue;
        }
        // `symlink_metadata`: a planted symlink is reported as what it is, never followed. A
        // concurrently removed candidate is not a directory-scan failure; it simply no longer exists.
        let path = entry.path();
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if !metadata.file_type().is_file() {
            continue;
        }
        let candidate = SaveRemnant {
            origin: parse_remnant_origin(&path),
            path,
            len: metadata.len(),
        };
        let index = found
            .partition_point(|existing: &SaveRemnant| existing.path.cmp(&candidate.path).is_lt());
        if found.len() == MAX_REMNANTS_REPORTED {
            truncated = true;
            if index < MAX_REMNANTS_REPORTED {
                found.insert(index, candidate);
                found.pop();
            }
        } else {
            found.insert(index, candidate);
        }
    }
    Ok((found, truncated))
}

/// Unkeyed digest of a file's bytes, streamed in fixed-size chunks.
///
/// Reported so a user can compare a remnant with a container, and nothing more: it is not
/// authenticity, and it correlates equal files.
pub fn path_digest(path: &Path) -> std::io::Result<[u8; 32]> {
    file_digest(&mut File::open(path)?)
}

/// Unkeyed digest of an open file's complete contents, restoring its starting offset afterward.
pub fn file_digest(file: &mut File) -> std::io::Result<[u8; 32]> {
    use blake2::{Blake2b512, Digest};

    let start = file.stream_position()?;
    file.seek(SeekFrom::Start(0))?;
    let result: std::io::Result<[u8; 32]> = (|| {
        let mut hash = Blake2b512::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
        }
        let mut digest = [0u8; 32];
        digest.copy_from_slice(&hash.finalize()[..32]);
        Ok(digest)
    })();
    let restore = file.seek(SeekFrom::Start(start));
    match (result, restore) {
        (Ok(digest), Ok(_)) => Ok(digest),
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
    }
}

/// Refuse wraparound until there is durable accounting of the pad's history.
pub(crate) fn validate_save_cursor(cursor: u64) -> Result<(), String> {
    let limit = holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE;
    if cursor == 0 {
        return Err(
            "save cursor must be at least 1; use the previous container cursor plus one".into(),
        );
    }
    if cursor >= limit {
        return Err(format!(
            "save cursor {cursor} would enter the pad reuse window (starting at {limit}); \
             saving is disabled until durable pad accounting and recovery are implemented"
        ));
    }
    Ok(())
}

/// Reject oversized payloads before any container or pad I/O.
pub(crate) fn validate_payload_size(len: usize) -> Result<(), String> {
    let capacity =
        holonomy_core::layout::PAYLOAD_SLOTS * holonomy_session::seal::PAYLOAD_PLAINTEXT_PER_SLOT;
    if len > capacity {
        return Err(format!(
            "document exceeds the {capacity}-byte container payload limit"
        ));
    }
    Ok(())
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
    save_inner(file, pad, ctx, payload, None, None)
}

/// The sidecar path that serializes saves of one container.
#[cfg(unix)]
fn save_lock_path(container: &Path) -> std::io::Result<PathBuf> {
    let parent = container
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = std::fs::canonicalize(parent)?;
    let mut name = container
        .file_name()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "container path has no file name",
            )
        })?
        .to_os_string();
    name.push(".holonomy-lock");
    Ok(parent.join(name))
}

/// Non-Unix builds refuse saves before this type could matter; it exists so signatures are
/// uniform across platforms.
#[cfg(not(unix))]
#[allow(dead_code)]
pub(crate) struct SaveLock(());

/// An advisory exclusive lock held for the duration of one save.
///
/// It narrows the window between reading the image and replacing it: a second Holonomy save of
/// the same normalized path fails instead of interleaving its read, replacement and ratchet. The
/// parent directory is canonicalized so relative-path spellings share the lock. It does not close
/// the race with a program that ignores the lock or every alias case; writable saves separately
/// reject existing hard links. The sidecar stays empty and holds no key material.
#[cfg(unix)]
pub(crate) struct SaveLock {
    _file: File,
}

#[cfg(unix)]
impl SaveLock {
    pub(crate) fn acquire(container: impl AsRef<Path>) -> std::io::Result<Self> {
        use std::os::unix::io::AsRawFd;

        let lock_path = save_lock_path(container.as_ref())?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        // SAFETY: `file` owns a live descriptor for as long as `Self` holds it, and `flock`
        // only takes the integer.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = std::io::Error::last_os_error();
            return Err(std::io::Error::new(
                error.kind(),
                format!(
                    "another Holonomy save holds {}: {error}; nothing was written to the container, retry after that save finishes or inspect it first",
                    lock_path.display()
                ),
            ));
        }
        Ok(Self { _file: file })
    }
}

/// Save only if the image still matches the snapshot authenticated by the session.
///
/// Non-Unix only: it exists so the session compiles where saves are refused anyway. On Unix the
/// session calls [`save_if_unchanged_locked`] with the lock it acquired first.
#[cfg(not(unix))]
pub(crate) fn save_if_unchanged(
    file: &mut ContainerFile,
    pad: &mut dyn PadSource,
    ctx: &SaveContext<'_>,
    payload: &[u8],
    opened_digest: &[u8; 32],
) -> Result<SaveReport, String> {
    save_inner(file, pad, ctx, payload, Some(opened_digest), None)
}

/// Save while a caller-held [`SaveLock`] covers the whole operation.
///
/// A session acquires the lock before it enters the store so that a contended save is refused
/// with the session still usable; passing the lock in avoids taking it twice.
#[cfg(unix)]
pub(crate) fn save_if_unchanged_locked(
    file: &mut ContainerFile,
    pad: &mut dyn PadSource,
    ctx: &SaveContext<'_>,
    payload: &[u8],
    opened_digest: &[u8; 32],
    lock: &SaveLock,
) -> Result<SaveReport, String> {
    save_inner(file, pad, ctx, payload, Some(opened_digest), Some(lock))
}

#[cfg_attr(not(unix), allow(unused_variables))]
fn save_inner(
    file: &mut ContainerFile,
    pad: &mut dyn PadSource,
    ctx: &SaveContext<'_>,
    payload: &[u8],
    opened_digest: Option<&[u8; 32]>,
    held_lock: Option<&SaveLock>,
) -> Result<SaveReport, String> {
    if !file.is_writable() {
        return Err(format!(
            "{} was opened read-only; a save needs a writable handle",
            file.path()
        ));
    }

    validate_payload_size(payload.len())?;

    // Provisioning uses window zero. Until we can prove the history of refreshed
    // sectors, do not enter a window that can overlap the first 54 windows.
    validate_save_cursor(ctx.cursor)?;

    // Serialize Holonomy saves of this path across the read, the replacement, the read-back and
    // the ratchet. Best effort by design: it cannot stop a writer that ignores the lock. A caller
    // that already acquired it (a session, so contention is a preflight refusal) passes it in.
    #[cfg(unix)]
    let _save_lock = match held_lock {
        Some(_) => None,
        None => Some(SaveLock::acquire(file.filesystem_path()).map_err(|e| e.to_string())?),
    };

    // Refuse stale, aliased or multiply-linked files before consulting the pad or staging an image.
    // These checks narrow known hazards but are not atomic with later I/O or replacement.
    #[cfg(unix)]
    file.validate_single_link()
        .map_err(|e| format!("save refused before reading the pad or staging an image: {e}"))?;

    // 1. The old container, before anything is written.
    let mut old_image = file.read_image().map_err(|e| e.to_string())?;
    let expected_old_digest = holonomy_session::seal::image_digest(&old_image);
    if opened_digest.is_some_and(|opened| opened != &expected_old_digest) {
        return Err("container image changed since this session opened or last saved; no save was attempted. Inspect and recover before continuing".into());
    }

    //    The salt cannot be read yet. It lives in slot 1, which is *inside* the pad-covered region, so
    //    every byte of it on disk is folded. Reading it here and checking for zeros -- which this used
    //    to do -- inspected pad bytes, and the "never written" test only ever fired by luck. It has to
    //    come after step 3.
    //
    //    A file that has never been written is still detectable, and step 4 finds it: the handshake
    //    fails on a capsule that is really pad, which is the same answer with a better reason.

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
    let old_cursor = ctx.cursor - 1;
    let old_pad = pad
        .window(ctx.kappa_usb, old_cursor)
        .map_err(|e| format!("reading pad window {old_cursor}: {e}"))?;
    let pad_sectors = PAD_SECTORS;

    // 3. Undo the old pad, so the old capsule and the old salt are legible.
    fold(&mut old_image, old_pad.as_slice());

    //    *Now* the salt. It keys the permutation the container was written under, so this is the line
    //    that makes the epoch permutation recoverable rather than stored: the map to the payload
    //    follows from a value the reader already has to extract.
    let (salt_off, _) = slot_range(SALT_SLOT_INDEX);
    let mut old_salt = [0u8; SALT_BYTES];
    old_salt.copy_from_slice(&old_image[salt_off..salt_off + SALT_BYTES]);
    if old_salt.iter().all(|&b| b == 0) {
        return Err(format!(
            "{} has no salt in slot 1 after unfolding; it was never written, or the cursor {} is \
             wrong for it",
            file.path(),
            old_cursor
        ));
    }

    // 4. The handshake over the old container. Its error is coarse, and this is where a wrong
    //    passphrase stops the save before a single byte is written.
    //
    //    The returned container is deliberately *not* used below except as proof: its stratum holds
    //    keys derived from the old save's KEM keypair, and sealing under those would produce a
    //    capsule and a payload that disagree. See step 5.
    //    The placement is `epoch_placement(&old_salt)`: the container on disk was written under the
    //    old salt's permutation, and it is the *only* permutation that can read it. This is why the
    //    epoch permutation needs no stored table -- the salt is step 1, and the map follows from it.
    let opened: OpenedContainer = open_container(
        &ReaderRequest {
            image: &old_image,
            passphrase: ctx.passphrase,
            kappa_usb: ctx.kappa_usb,
            topology: ctx.topology,
            params: ctx.params,
            pad_fresh: None,
        },
        epoch_placement(&old_salt),
    )
    .map_err(|e: OpenError| e.to_string())?;
    if opened.cursor() != old_cursor {
        return Err(format!(
            "container cursor {} does not match expected previous cursor {old_cursor}; refusing to save",
            opened.cursor()
        ));
    }

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
    file.replace_image(&image, &expected_old_digest).map_err(|e| {
        format!(
            "saving cursor {}: {e}; a staged encrypted image may have been written. Do not retry until the container and pad state have been inspected; the cursor may be spent",
            ctx.cursor
        )
    })?;

    // 8b. Read the replacement back *before* the ratchet destroys the old window.
    //
    //     The digest was computed over the bytes handed to `replace_image`; this compares them with
    //     the bytes this process can read back. It catches a replacement that wrote a different or
    //     truncated image, which would otherwise be discovered only after ratcheting had made the
    //     old window unreadable. It is not a durability guarantee -- the read may be served from
    //     cache -- and it says nothing about media corruption later.
    let readback = file.folded_digest().map_err(|e| {
        format!(
            "container was written with cursor {}; reading the replacement back failed: {e}. Do not retry with the old cursor and do not ratchet; inspect the container first",
            ctx.cursor
        )
    })?;
    if readback != digest {
        return Err(format!(
            "container was written with cursor {}, but the replacement reads back with a different digest; the old pad window has not been ratcheted. Do not retry with the old cursor; inspect the container before continuing",
            ctx.cursor
        ));
    }

    // 9. Ratchet window `c-1`, *after* the container is on the device.
    //
    //    Two decisions in one line, and this tree has had both wrong.
    //
    //    **After, not before.** Ratcheting first destroys the pad the container on disk was folded
    //    with, so a write that then failed would leave a container nothing can open -- not this
    //    build, not a later one. A failed staged write leaves the old pad intact. A directory
    //    sync error may occur after replacement, so the error warns that the image changed.
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
        let count = pad.ratchet(ctx.kappa_usb, old_cursor).map_err(|e| {
            format!(
                "container was written with cursor {}; ratcheting old pad window {old_cursor} failed: {e}. Do not retry this save with the old cursor; check the new container and pad before continuing",
                ctx.cursor
            )
        })?;
        validate_ratchet_count(count, old_cursor, ctx.cursor)?;
        Some(count)
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
        protection: protection_layer(),
    })
}

/// A short or impossible count is an uncertain, post-replacement failure. A
/// full count remains only the source's report, not proof of fresh pad history.
fn validate_ratchet_count(count: u32, old_cursor: u64, new_cursor: u64) -> Result<(), String> {
    if count != PAD_SECTORS_PER_SAVE {
        return Err(format!(
            "container was written with cursor {new_cursor}, but ratcheting old pad window {old_cursor} reported {count} of {PAD_SECTORS_PER_SAVE} sectors; pad state may be partially changed. Do not retry this save with the old cursor; inspect the new container and pad before continuing"
        ));
    }
    Ok(())
}

fn open_authenticated(
    file: &mut ContainerFile,
    pad: &mut dyn PadSource,
    ctx: &SaveContext<'_>,
    image_digest: Option<&mut [u8; 32]>,
) -> Result<OpenedContainer, String> {
    let mut image = file.read_image().map_err(|e| e.to_string())?;
    // Fingerprint the folded bytes actually read, before the pad is undone. This
    // is an unkeyed comparison aid, not authentication of the whole image.
    if let Some(digest) = image_digest {
        *digest = holonomy_session::seal::image_digest(&image);
    }
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
            pad_fresh: None,
        },
        epoch_placement(&file_salt),
    )
    .map_err(|e: OpenError| e.to_string())?;
    // A different cursor can select the same physical window after wraparound (or
    // through a faulty pad source). Authenticate the caller's cursor before returning data.
    if opened.cursor() != ctx.cursor {
        return Err(format!(
            "container cursor {} does not match requested cursor {}; refusing to open",
            opened.cursor(),
            ctx.cursor
        ));
    }
    Ok(opened)
}

/// Open `file` and return the document it holds.
pub fn open_document(
    file: &mut ContainerFile,
    pad: &mut dyn PadSource,
    ctx: &SaveContext<'_>,
) -> Result<(OpenedContainer, holonomy_session::seal::Zeroizing), String> {
    let opened = open_authenticated(file, pad, ctx, None)?;
    let document = opened.payload_bytes();
    Ok((opened, document))
}

/// Open a session and retain a fingerprint of the same folded bytes that were authenticated.
pub(crate) fn open_document_with_digest(
    file: &mut ContainerFile,
    pad: &mut dyn PadSource,
    ctx: &SaveContext<'_>,
) -> Result<(OpenedContainer, holonomy_session::seal::Zeroizing, [u8; 32]), String> {
    let mut digest = [0u8; 32];
    let opened = open_authenticated(file, pad, ctx, Some(&mut digest))?;
    let document = opened.payload_bytes();
    Ok((opened, document, digest))
}

/// Authenticate and read a candidate container without returning its plaintext.
///
/// This performs the same container and payload authentication as [`open_document`],
/// then drops the plaintext. Only the authenticated directory and its declared payload
/// slots are checked, not all unused image bytes. It does not write or ratchet the pad.
/// On success returns the unkeyed image digest of the folded bytes read, calculated
/// identically to [`SaveReport::digest`]. It is a comparison aid, not an authentication
/// tag or proof of durability; concurrent modification can make it a mixed snapshot.
/// Successful authentication does not verify application-level document decoding, pad freshness,
/// rollback history, durability, or recovery. Plaintext and copies may still exist in
/// process and operating-system memory. File access metadata may change.
pub fn verify_candidate(
    file: &mut ContainerFile,
    pad: &mut dyn PadSource,
    ctx: &SaveContext<'_>,
) -> Result<[u8; 32], String> {
    let mut digest = [0u8; 32];
    let _opened = open_authenticated(file, pad, ctx, Some(&mut digest))?;
    Ok(digest)
}

/// A cursor that opened the image during a scan.
#[derive(Debug, PartialEq, Eq)]
pub struct CursorHit {
    /// The cursor that authenticated the directory and declared payload.
    pub cursor: u64,
    /// The same unkeyed encrypted-image digest `verify` prints.
    pub digest: [u8; 32],
}

/// What a cursor scan found.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CursorScan {
    /// Every cursor that authenticated, in the order tried.
    pub hits: Vec<CursorHit>,
    /// How many cursors were tried before stopping.
    pub attempts: u64,
    /// The last refusal, kept because "nothing opened" is not a diagnosis on its own.
    pub last_error: Option<String>,
    /// Whether the caller requested cancellation between candidate attempts.
    pub cancelled: bool,
}

/// Try every cursor a save may legitimately have written, and report the ones that open.
///
/// This is the recovery question a crash leaves behind -- *which* cursor does this image need? --
/// answered without guessing and without writing. It is strictly read-only: no ratchet, no image
/// write, no cursor accounting change. Each attempt unfolds the image with that cursor's pad
/// window and runs a full unlock, so a scan costs one image read and one key derivation per cursor;
/// the range is bounded by [`SAVES_BEFORE_DOMAIN_REUSE`] because saves refuse cursors beyond it.
///
/// What it cannot do is stated where it is used: a hit proves the image opens with these
/// credentials at that cursor, not that the image is fresh, undamaged, the newest copy, or that no
/// other copy exists.
pub fn scan_cursors(
    file: &mut ContainerFile,
    pad: &mut dyn PadSource,
    template: &SaveContext<'_>,
    stop_at_first: bool,
) -> CursorScan {
    scan_cursor_range(
        file,
        pad,
        template,
        0..holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE,
        stop_at_first,
    )
}

/// Scan candidate cursors while allowing a caller to stop between expensive attempts.
///
/// Cancellation is cooperative: an in-progress candidate still completes its full image read and
/// key derivation. It never interrupts a candidate midway through authentication. The scan remains
/// strictly read-only and never ratchets the pad.
pub fn scan_cursors_cancellable(
    file: &mut ContainerFile,
    pad: &mut dyn PadSource,
    template: &SaveContext<'_>,
    stop_at_first: bool,
    cancelled: &std::sync::atomic::AtomicBool,
) -> CursorScan {
    let completed_attempts = std::sync::atomic::AtomicU64::new(0);
    scan_cursors_with_progress(
        file,
        pad,
        template,
        stop_at_first,
        cancelled,
        &completed_attempts,
    )
}

/// Scan with a cooperative cancellation flag and a live count of completed full-image attempts.
pub fn scan_cursors_with_progress(
    file: &mut ContainerFile,
    pad: &mut dyn PadSource,
    template: &SaveContext<'_>,
    stop_at_first: bool,
    cancelled: &std::sync::atomic::AtomicBool,
    completed_attempts: &std::sync::atomic::AtomicU64,
) -> CursorScan {
    completed_attempts.store(0, std::sync::atomic::Ordering::Relaxed);
    scan_cursor_range_with_cancel(
        file,
        pad,
        template,
        0..holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE,
        stop_at_first,
        || cancelled.load(std::sync::atomic::Ordering::Relaxed),
        |attempts| completed_attempts.store(attempts, std::sync::atomic::Ordering::Relaxed),
    )
}

/// The scan itself, with the range named so tests do not have to read the whole legal range to
/// prove the loop and the early stop. Production always passes the full legal range above.
fn scan_cursor_range(
    file: &mut ContainerFile,
    pad: &mut dyn PadSource,
    template: &SaveContext<'_>,
    range: std::ops::Range<u64>,
    stop_at_first: bool,
) -> CursorScan {
    scan_cursor_range_with_cancel(file, pad, template, range, stop_at_first, || false, |_| {})
}

fn scan_cursor_range_with_cancel(
    file: &mut ContainerFile,
    pad: &mut dyn PadSource,
    template: &SaveContext<'_>,
    range: std::ops::Range<u64>,
    stop_at_first: bool,
    mut should_cancel: impl FnMut() -> bool,
    mut report_attempts: impl FnMut(u64),
) -> CursorScan {
    let mut scan = CursorScan::default();
    for cursor in range {
        if should_cancel() {
            scan.cancelled = true;
            break;
        }
        let ctx = SaveContext {
            passphrase: template.passphrase,
            kappa_usb: template.kappa_usb,
            topology: template.topology,
            params: template.params,
            cursor,
        };
        scan.attempts += 1;
        let result = verify_candidate(file, pad, &ctx);
        report_attempts(scan.attempts);
        match result {
            Ok(digest) => {
                scan.hits.push(CursorHit { cursor, digest });
                if stop_at_first {
                    break;
                }
            }
            Err(error) => scan.last_error = Some(error),
        }
        if should_cancel() {
            scan.cancelled = true;
            break;
        }
    }
    scan
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
/// stick loses the pad, and the file alone yields nothing legible. Panics if `cursor` is outside
/// `0..SAVES_BEFORE_DOMAIN_REUSE` or `j` is not a slot in one save.
pub fn sector_index(kappa_usb: &[u8; 32], cursor: u64, j: usize) -> u32 {
    assert!(
        cursor < SAVES_BEFORE_DOMAIN_REUSE,
        "pad cursor exceeds the no-reuse budget"
    );
    assert!(
        j < PAD_SECTORS_PER_SAVE as usize,
        "pad slot is out of range"
    );
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
    // A fresh container starts at the first window; accepting an arbitrary initial
    // cursor would bypass the conservative boundary on later saves.
    if ctx.cursor != 0 {
        return Err("provision cursor must be 0; pad history is not tracked yet".into());
    }
    // `create_new` atomically rejects existing paths, including symlinks. Checking
    // `exists` before `write` would allow another process to replace the path in between.
    let handle = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| {
            format!(
                "creating {} (existing paths are never overwritten): {e}",
                path.display()
            )
        })?;
    let mut file = ContainerFile {
        file: handle,
        path: path.display().to_string(),
        filesystem_path: path.to_path_buf(),
        offset: 0,
        writable: true,
    };
    let mut bytes = vec![0u8; CONTAINER_BYTES];
    holonomy_crypto::rng::fill(&mut bytes)
        .map_err(|e| format!("the OS CSPRNG is unavailable: {e}"))?;
    file.write_image(&bytes)
        .map_err(|e| format!("initializing {}: {e}", path.display()))?;
    drop(bytes);
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
/// The protection layer a store write attaches to its stratum.
///
/// The store knows it folded a pad; it does not know whether any sector was reused on another
/// device, in a snapshot, or after a rollback, so freshness is `None` rather than a claim.
pub(crate) fn protection_layer() -> holonomy_core::unlock::ProtectionLayer {
    holonomy_core::unlock::ProtectionLayer {
        pad: true,
        pad_fresh: None,
        post_quantum: true,
    }
}

fn stratum_for(ctx: &SaveContext<'_>, salt: &SaltBytes) -> Result<ActiveStratum, String> {
    let root =
        holonomy_crypto::derive_root_secret(ctx.passphrase.as_bytes(), salt.as_bytes(), ctx.params)
            .map_err(|e| format!("deriving the root secret: {e}"))?;
    let keys = holonomy_crypto::envelope::derive_stratum_keys(&root, 1, ctx.kappa_usb);
    let id = holonomy_core::stratum::StratumId::new(1).ok_or("stratum 1 is out of range")?;
    let layer = protection_layer();
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
pub(crate) fn first_write(
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
    use holonomy_core::layout::{PAYLOAD_SLOTS, PAYLOAD_SLOT_BASE};

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
