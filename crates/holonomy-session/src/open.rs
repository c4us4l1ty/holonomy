//! The open path: a container image in, an [`ActiveStratum`] and its payload out.
//!
//! This is the other half of [`crate::seal`]. That module proved a container could be *written*;
//! this one reads back what it wrote, using only the public unlock and AEAD APIs, and it is the
//! first code in the project that closes the loop:
//!
//! ```text
//!   salt (slot 1) ──Argon2id──► root ──► K_unwrap ──► SK_stratum ──► K_DEM
//!   slot 0 ──unlock_fixed──► ActiveStratum + the directory it verified
//!   directory ──ledger──► per-slot lengths and the save cursor
//!   each slot ──open_committed──► plaintext
//! ```
//!
//! # Why this is not [`holonomy_crypto::unlock`] again
//!
//! `unlock_fixed` answers exactly one question — *is this stratum real?* — and deliberately does
//! not interpret the directory, because interpreting it inside the handshake would turn "a newer
//! build wrote this" into "wrong passphrase". The consequence, unseen until something tried to read
//! a container written by [`crate::seal::seal_container`], is that the unlock **discards the very
//! directory the caller needs next**: it opens it to verify the phase-2 commitment and then drops
//! the plaintext. So the reader here re-derives the directory itself, and the two paths must agree.
//! `the_reader_agrees_with_the_handshake` is what holds them together: it opens the same image both
//! ways and asserts the capsule the reader trusts is one `unlock_fixed` accepts.
//!
//! # What the reader refuses, and at which step
//!
//! | Condition | Detected by | Reported as |
//! | :--- | :--- | :--- |
//! | wrong passphrase, wrong USB, or a capsule from another save | phase-1/phase-2 commitment in the handshake | [`OpenError::Rejected`] |
//! | a field that is not a directory of this version | ledger tag | [`OpenError::Directory`] |
//! | a slot relocated, or its ciphertext altered | `CMT-4` before the AEAD tag | [`OpenError::Slot`] |
//! | a length the image cannot hold | [`DirectoryLedger::decode`] | [`OpenError::Directory`] |
//!
//! No step reports *why* a credential failed, and the reader does not look at the container's
//! version before the handshake — for the same reason the handshake does not.

use holonomy_core::capsule::CapsuleWire;
use holonomy_core::layout::{slot_range, PAYLOAD_SLOTS, SALT_SLOT_INDEX, SLOT_BYTES, TOTAL_BLOCKS};
use holonomy_core::stratum::ActiveStratum;
use holonomy_core::unlock::{SealedPassphrase, Topology, UnlockError};
use holonomy_crypto::commit::{open_directory, CMT4_TAG_BYTES, DIRECTORY_PLAINTEXT_BYTES};
use holonomy_crypto::kdf::ParamsClass;
use holonomy_crypto::unlock::{unlock_fixed, UnlockRequest};

use crate::directory::{DirectoryError, DirectoryLedger};
use crate::salt::{ContainerHeader, HeaderError, SaltBytes};
use crate::seal::{open_payload_slot, SealError, Zeroizing};

/// Everything needed to open a container. Borrowed, like [`UnlockRequest`], because none of it is
/// secret except the passphrase and that is already sealed.
pub struct ReaderRequest<'a> {
    /// The 134,217,728-byte image, after the pad has been undone.
    pub image: &'a [u8],
    /// The passphrase.
    pub passphrase: &'a SealedPassphrase,
    /// The USB root share from Zone A.
    pub kappa_usb: &'a [u8; 32],
    /// Which transport supplied [`Self::kappa_usb`].
    pub topology: Topology,
    /// `Argon2id` parameters. [`ParamsClass::Test`] is for CI only.
    pub params: ParamsClass,
    /// Whether this session's pad is fresh, reported through `ProtectionLayer`.
    pub pad_fresh: bool,
}

/// What an open produced.
///
/// The stratum is kept because a session needs it to save again; the directory is kept because it
/// is the only record of which slots hold the document and in what order. Neither is optional: a
/// reader that returned payload without the index could not tell a truncated document from a short
/// one, and one that dropped the stratum forced the caller to unlock twice.
pub struct OpenedContainer {
    /// The one live stratum.
    pub stratum: ActiveStratum,
    /// The ledger the capsule's directory verified into.
    pub directory: DirectoryLedger,
    /// Plaintext payload slots, in logical order, zeroizing on drop.
    pub payload: Vec<Zeroizing>,
}

impl OpenedContainer {
    /// The payload concatenated, in slot order.
    ///
    /// Allocates a copy of the document, which is the caller's explicit choice: the slots
    /// themselves zeroize, and an editor that needs one contiguous buffer takes on that buffer's
    /// lifetime. Kept here rather than in the editor so the copy is made once, from the zeroizing
    /// source, rather than per redraw from decrypted material.
    pub fn payload_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.payload.iter().map(|s| s.as_slice().len()).sum());
        for slot in &self.payload {
            out.extend_from_slice(slot.as_slice());
        }
        out
    }

    /// The pad cursor the container says it consumed.
    pub fn cursor(&self) -> u64 {
        self.directory.cursor
    }
}

/// Why a container could not be opened.
#[derive(Debug)]
pub enum OpenError {
    /// The image is not one container long.
    WrongImageSize {
        /// Bytes supplied.
        found: usize,
        /// Bytes required.
        needed: usize,
    },
    /// The salt slot could not be read, or the device has never been provisioned.
    Header(HeaderError),
    /// The credential or the capsule did not verify. Coarse by construction; see
    /// [`holonomy_core::unlock::UnlockError`].
    Rejected(UnlockError),
    /// The directory decrypted but is not an index this build understands, or disagrees with the
    /// image. Distinct from [`Self::Rejected`] on purpose: the credential was accepted, so this is
    /// a format problem and reporting it as a bad passphrase would be a lie.
    Directory(DirectoryError),
    /// A payload slot failed its commitment or its AEAD.
    Slot(SealError),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongImageSize { found, needed } => {
                write!(f, "the image is {found} bytes; a container is {needed}")
            }
            Self::Header(e) => write!(f, "{e}"),
            Self::Rejected(e) => write!(f, "the container did not open: {e}"),
            Self::Directory(e) => write!(f, "the directory is not usable: {e}"),
            Self::Slot(e) => write!(f, "a payload slot did not open: {e}"),
        }
    }
}

impl std::error::Error for OpenError {}

impl From<HeaderError> for OpenError {
    fn from(e: HeaderError) -> Self {
        Self::Header(e)
    }
}

impl From<UnlockError> for OpenError {
    fn from(e: UnlockError) -> Self {
        Self::Rejected(e)
    }
}

impl From<DirectoryError> for OpenError {
    fn from(e: DirectoryError) -> Self {
        Self::Directory(e)
    }
}

impl From<SealError> for OpenError {
    fn from(e: SealError) -> Self {
        Self::Slot(e)
    }
}

/// Open a container image.
///
/// The order is the format's: the salt first, because it keys everything; the capsule next, because
/// only the handshake can say whether the stratum is real; the directory after that, because until
/// the capsule verified its contents are attacker-controlled; and the payload last.
///
/// `placement(i)` is the **same** permutation the save used, and it is a function here rather than
/// stored in the container. Storing it would be storing the map to the document in the one
/// structure an adversary reads without a passphrase.
pub fn open_container(
    req: &ReaderRequest<'_>,
    placement: impl Fn(usize) -> usize,
) -> Result<OpenedContainer, OpenError> {
    if req.image.len() != holonomy_core::layout::CONTAINER_BYTES {
        return Err(OpenError::WrongImageSize {
            found: req.image.len(),
            needed: holonomy_core::layout::CONTAINER_BYTES,
        });
    }

    // 1. The salt, through the sanctioned reader. `open` has no pad to check against, because the
    //    caller owns the pad; what it does check is that the slot exists and is not all zeros.
    let mut salt_slot = vec![0u8; SLOT_BYTES];
    let (salt_off, salt_len) = slot_range(SALT_SLOT_INDEX);
    let salt_source = &req.image[salt_off..salt_off + salt_len];
    let mut read = |_offset: usize, buf: &mut [u8]| {
        buf[..salt_len].copy_from_slice(salt_source);
        Ok(())
    };
    let header = ContainerHeader::open(&mut read, &mut salt_slot)?;

    let (stratum, directory, payload) = open_with_salt(req, placement, header.salt())?;
    Ok(OpenedContainer {
        stratum,
        directory,
        payload,
    })
}

/// The body of [`open_container`], split out so the salt's provenance is visible at the boundary.
fn open_with_salt(
    req: &ReaderRequest<'_>,
    placement: impl Fn(usize) -> usize,
    salt: &SaltBytes,
) -> Result<(ActiveStratum, DirectoryLedger, Vec<Zeroizing>), OpenError> {
    let (capsule_off, capsule_len) = slot_range(0);
    let mut capsule_bytes = [0u8; SLOT_BYTES];
    capsule_bytes.copy_from_slice(&req.image[capsule_off..capsule_off + capsule_len]);
    let capsule = CapsuleWire(capsule_bytes);

    // 2. The handshake. This is the only step that can say the credential is right, and it is the
    //    one step whose error text must stay coarse.
    let unlock_req = UnlockRequest {
        passphrase: req.passphrase,
        salt: salt.as_bytes(),
        capsule: &capsule,
        kappa_usb: req.kappa_usb,
        topology: req.topology,
        params: req.params,
        pad_fresh: req.pad_fresh,
    };
    let stratum = unlock_fixed(&unlock_req, &holonomy_crypto::MlKem1024X25519)?;

    // 3. The directory. `unlock_fixed` verified it and dropped the plaintext, so it is opened again
    //    here -- under the key set the handshake returned, which is the one `create_envelope`
    //    produced when it built this capsule and therefore the one the slots were sealed with.
    let keys = stratum.keys();
    let capsule_body = capsule.to_capsule();
    let directory_field = holonomy_crypto::unlock::directory_ciphertext(
        &capsule_body.encrypted_directory_and_reservoir,
        &capsule_body.directory_poly1305_tag,
    );
    // `open_directory` returns an `AeadError`; `SealError::Aead` is the variant that carries one,
    // so the mapping is explicit rather than a bare constructor reference.
    let plaintext = open_directory(&keys.k_dem, &capsule_body.directory_nonce, &directory_field)
        .map_err(|e| OpenError::Slot(SealError::Aead(e)))?;

    // 4. The commitments, read from the image at the permuted positions, and the ledger over
    //    them. The slot count comes from the directory *plaintext*, which is safe because the AEAD
    //    above authenticated it: reading the count to know how many commitments to hash is not
    //    trusting an unauthenticated length. It cannot be avoided by reading the count from the
    //    commitments instead, because the commitments are exactly what the ledger's tag covers.
    let count = slot_count_of(&plaintext)?;
    let commitments = read_commitments(req.image, &placement, count);
    let directory = DirectoryLedger::decode(&plaintext, &commitments)?;

    // 5. The payload. AAD binds the stratum and logical slot index, so a slot that was moved to a
    //    different physical block but kept its ciphertext still fails.
    let commitment_len = CMT4_TAG_BYTES;
    let mut payload = Vec::with_capacity(directory.slot_count());
    for (logical, &len) in directory.lengths.iter().enumerate() {
        let physical = placement(logical);
        if physical < holonomy_core::layout::PAYLOAD_SLOT_BASE || physical >= TOTAL_BLOCKS {
            return Err(OpenError::Slot(SealError::BadPlacement {
                slot: logical,
                physical,
            }));
        }
        let (off, slot_len) = slot_range(physical);
        let body = &req.image[off..off + slot_len];
        let len = len as usize;
        if len + commitment_len > slot_len {
            return Err(OpenError::Slot(SealError::PayloadTooLarge {
                offered: len,
                capacity: slot_len - commitment_len,
            }));
        }
        let ciphertext = &body[..len];
        let mut commitment = [0u8; CMT4_TAG_BYTES];
        commitment.copy_from_slice(&body[slot_len - commitment_len..]);
        let nonce = holonomy_crypto::commit::slot_nonce(
            &keys.n_epoch,
            directory.cursor,
            logical as u32,
            stratum.id().index(),
        );
        payload.push(open_payload_slot(
            &keys,
            stratum.id().index(),
            logical as u32,
            &nonce,
            ciphertext,
            &commitment,
        )?);
    }

    Ok((stratum, directory, payload))
}

/// The slot count as the authenticated directory states it.
///
/// Bounded here rather than left to [`DirectoryLedger::decode`], because this number is used to
/// decide how much of the image to read before that function runs.
fn slot_count_of(plaintext: &[u8; DIRECTORY_PLAINTEXT_BYTES]) -> Result<usize, OpenError> {
    let count =
        u32::from_le_bytes([plaintext[12], plaintext[13], plaintext[14], plaintext[15]]) as usize;
    if count > PAYLOAD_SLOTS {
        return Err(OpenError::Directory(DirectoryError::TooManySlots {
            found: count,
        }));
    }
    Ok(count)
}

/// The `CMT-4` tag of each of the first `count` logical slots, read from the image.
///
/// The tag lives in the last [`CMT4_TAG_BYTES`] of a slot, after the ciphertext and its `Poly1305`
/// tag, so a reader finds it without knowing the ciphertext length. That is why it can be read
/// before the ledger is verified.
fn read_commitments(
    image: &[u8],
    placement: &dyn Fn(usize) -> usize,
    count: usize,
) -> Vec<[u8; CMT4_TAG_BYTES]> {
    let mut out = Vec::with_capacity(count);
    for logical in 0..count {
        let physical = placement(logical);
        if physical < holonomy_core::layout::PAYLOAD_SLOT_BASE || physical >= TOTAL_BLOCKS {
            break;
        }
        let (off, len) = slot_range(physical);
        let mut tag = [0u8; CMT4_TAG_BYTES];
        tag.copy_from_slice(&image[off + len - CMT4_TAG_BYTES..off + len]);
        out.push(tag);
    }
    out
}

/// The directory plaintext length, re-exported so a caller asserting on it does not reach into
/// `holonomy-crypto` for a number this crate already depends on.
pub const DIRECTORY_BYTES: usize = DIRECTORY_PLAINTEXT_BYTES;

#[cfg(test)]
mod tests;
