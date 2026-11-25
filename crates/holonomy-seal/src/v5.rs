//! Read-only streaming opener for legacy Holonomy v5.1.0 containers (`ROADMAP.md` Block 10).
//!
//! v5.1.0 containers are fixed `134,217,728`-byte (`2,048 × 65,536`-byte) images optionally
//! XOR-folded with a 32,768-sector (`4,096`-byte/sector) Zone B pad window selected by a monotonic
//! cursor (`0..54`) through a 22-bit balanced Feistel permutation (`Feistel22`).
//!
//! Rather than allocating the 128 MiB image and 128 MiB pad buffer in RAM like v5.1.0's CLI did,
//! [`open_v5_streaming`] reads and unfolds **one 64 KiB physical slot / 4 KiB sector at a time**
//! using zeroized buffers and invokes the caller's callback with each logical payload slot in
//! order. The source container is never modified.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use blake2::{Blake2b512, Digest};
use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::{Tag, XChaCha20Poly1305, XNonce};
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::commit::{
    cmt4_tag, cmt4_verify, open_committed, seal_committed, CommittedCiphertext, CMT4_TAG_BYTES,
    NONCE_BYTES, POLY1305_TAG_BYTES,
};
use crate::envelope::{HybridCiphertext, HybridDecapsulationKey, HybridKem, MlKem1024X25519};
use crate::kdf::{derive_root_secret_raw, KdfError, ParamsClass};

/// Fixed byte length of every Holonomy v5.1.0 container (`128 MiB`).
pub const V5_CONTAINER_BYTES: usize = 134_217_728;
/// Byte length of each physical slot (`64 KiB`).
pub const V5_SLOT_BYTES: usize = 65_536;
/// Total physical blocks in a v5.1.0 container (`2,048`).
pub const V5_TOTAL_BLOCKS: usize = 2_048;
/// Physical block index of the root capsule (`slot 0`).
pub const V5_ROOT_SLOT_INDEX: usize = 0;
/// Physical block index of `σ_epoch` (`slot 1`).
pub const V5_SALT_SLOT_INDEX: usize = 1;
/// First physical block index of the permuted payload region (`slot 2`).
pub const V5_PAYLOAD_SLOT_BASE: usize = 2;
/// Number of payload slots in a v5.1.0 container (`2,046`).
pub const V5_PAYLOAD_SLOTS: usize = V5_TOTAL_BLOCKS - V5_PAYLOAD_SLOT_BASE;
/// Length of `σ_epoch` (`64` bytes).
pub const V5_SALT_BYTES: usize = 64;
/// Logical sector byte size (`4 KiB`).
pub const V5_SECTOR_BYTES: usize = 4_096;
/// Number of 4 KiB sectors consumed by one save window (`32,768`).
pub const V5_PAD_SECTORS_PER_SAVE: u32 = 32_768;
/// Total 4 KiB sectors in USB Zone B (`1,782,579`).
pub const V5_ZONE_B_SECTORS: u32 = 1_782_579;
/// Default LBA where Zone B starts on a provisioned USB device (`32,768`).
pub const V5_ZONE_B_LBA_START: u64 = 32_768;
/// Save-window no-reuse budget (`54`).
pub const V5_SAVES_BEFORE_DOMAIN_REUSE: u64 = (V5_ZONE_B_SECTORS / V5_PAD_SECTORS_PER_SAVE) as u64;
/// Maximum plaintext bytes inside one v5.1.0 payload slot (`65,488`).
pub const V5_PAYLOAD_PLAINTEXT_PER_SLOT: usize =
    V5_SLOT_BYTES - POLY1305_TAG_BYTES - CMT4_TAG_BYTES;
/// Maximum number of strata supported by a v5.1.0 capsule (`4`).
pub const V5_MAX_STRATA: usize = 4;

const SECTORS_PER_SLOT: usize = V5_SLOT_BYTES / V5_SECTOR_BYTES;
const ML_KEM_1024_CT_BYTES: usize = 1_568;
const ML_KEM_1024_SK_BYTES: usize = 3_168;
const ML_KEM_1024_SEED_BYTES: usize = 64;
const X25519_SK_BYTES: usize = 32;
const WRAPPED_SK_BYTES: usize = ML_KEM_1024_SK_BYTES + X25519_SK_BYTES + POLY1305_TAG_BYTES;
const WRAPPED_SK_PLAIN_BYTES: usize = ML_KEM_1024_SK_BYTES + X25519_SK_BYTES;
const DIRECTORY_PLAINTEXT_BYTES: usize = 60_592;
const SEALED_DIRECTORY_BYTES: usize = DIRECTORY_PLAINTEXT_BYTES + POLY1305_TAG_BYTES;
const DIRECTORY_VERSION_V1: u16 = 1;
const DIR_HEADER_BYTES: usize = 2 + 1 + 1 + 8 + 4;
const DIR_LEN_BYTES: usize = 4;
const DIR_TAG_OFFSET: usize = DIR_HEADER_BYTES + V5_PAYLOAD_SLOTS * DIR_LEN_BYTES;

const FEISTEL_DOMAIN: u32 = 1 << 22;
const FEISTEL_HALF_BITS: u32 = 11;
const FEISTEL_HALF_MASK: u32 = (1 << FEISTEL_HALF_BITS) - 1;
const FEISTEL_ROUNDS: u32 = 6;

const _: () = {
    assert!(V5_TOTAL_BLOCKS * V5_SLOT_BYTES == V5_CONTAINER_BYTES);
    assert!(V5_PAD_SECTORS_PER_SAVE as usize * V5_SECTOR_BYTES == V5_CONTAINER_BYTES);
    assert!(SECTORS_PER_SLOT == 16);
    assert!(V5_PAYLOAD_SLOTS == 2_046);
    assert!(V5_PAYLOAD_PLAINTEXT_PER_SLOT == 65_488);
    assert!(V5_SAVES_BEFORE_DOMAIN_REUSE == 54);
    assert!(WRAPPED_SK_BYTES == 3_216);
    assert!(SEALED_DIRECTORY_BYTES == 60_608);
    assert!(DIR_TAG_OFFSET == 8_200);
    assert!(
        ML_KEM_1024_CT_BYTES
            + X25519_SK_BYTES
            + NONCE_BYTES
            + WRAPPED_SK_BYTES
            + CMT4_TAG_BYTES
            + NONCE_BYTES
            + DIRECTORY_PLAINTEXT_BYTES
            + POLY1305_TAG_BYTES
            + CMT4_TAG_BYTES
            == V5_SLOT_BYTES
    );
};

/// A passphrase normalised by Holonomy v5.1.0's legacy Unicode lowercase mapping (`1..=255` UTF-8
/// bytes), stored in a fixed non-reallocating array wiped on drop.
///
/// v5.1.0 normalised passphrases with `char::to_lowercase` rather than v6's NFC rule; retaining
/// that exact rule here guarantees migrated passphrases reproduce the exact v5.1.0 keys.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct V5Passphrase {
    bytes: [u8; 256],
    len: u8,
}

impl V5Passphrase {
    /// Normalises `passphrase` using the legacy v5.1.0 lowercase mapping.
    ///
    /// Returns `None` if the resulting UTF-8 sequence is empty or exceeds 255 bytes.
    #[must_use]
    pub fn new(passphrase: &str) -> Option<Self> {
        let normalized = Zeroizing::new(
            passphrase
                .chars()
                .flat_map(char::to_lowercase)
                .collect::<String>(),
        );
        let raw = normalized.as_bytes();
        if raw.is_empty() || raw.len() > 255 {
            return None;
        }
        let mut bytes = [0u8; 256];
        bytes[..raw.len()].copy_from_slice(raw);
        Some(Self {
            bytes,
            len: raw.len() as u8,
        })
    }

    /// The normalised passphrase bytes fed into v5.1.0 Argon2id.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

impl core::fmt::Debug for V5Passphrase {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("V5Passphrase(<redacted>)")
    }
}

/// Where the pad fold for a v5.1.0 container comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum V5PadSource {
    /// Unfolded container image (`all-zero` pad mask, e.g. unpadded v5.1.0 images).
    None,
    /// v5.1.0 CLI's stand-in `DerivedPad` (material hashed from `Feistel22(cursor, j)`).
    Derived {
        /// Monotonic save cursor (`0..54`) used to write the image.
        cursor: u64,
    },
    /// Physical USB stick or Zone B file image read sector-by-sector on demand.
    DeviceFile {
        /// Path to the Zone B block device or backing image file.
        path: PathBuf,
        /// Monotonic save cursor (`0..54`) used to write the image.
        cursor: u64,
        /// Starting 4 KiB LBA of Zone B (defaults to [`V5_ZONE_B_LBA_START`]).
        zone_b_lba_start: u64,
    },
}

impl V5PadSource {
    /// Opens a Zone B device or full backing image at the standard [`V5_ZONE_B_LBA_START`] offset.
    #[must_use]
    pub fn device(path: impl Into<PathBuf>, cursor: u64) -> Self {
        Self::DeviceFile {
            path: path.into(),
            cursor,
            zone_b_lba_start: V5_ZONE_B_LBA_START,
        }
    }

    /// Opens a relocated Zone B file at a custom starting 4 KiB sector offset.
    #[must_use]
    pub fn device_relocated(path: impl Into<PathBuf>, cursor: u64, zone_b_lba_start: u64) -> Self {
        Self::DeviceFile {
            path: path.into(),
            cursor,
            zone_b_lba_start,
        }
    }
}

/// Authenticated summary returned when a v5.1.0 container finishes streaming cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct V5OpenReport {
    /// Unlocked stratum index (`1..=4`).
    pub stratum: u8,
    /// Authenticated save cursor stored inside the directory ledger.
    pub cursor: u64,
    /// Number of logical payload slots decrypted and streamed.
    pub slot_count: usize,
    /// Total plaintext bytes streamed across all slots.
    pub total_payload_bytes: u64,
}

/// Failure opening or streaming a v5.1.0 container.
#[derive(Debug)]
pub enum V5Error {
    /// Underlying filesystem error.
    Io(io::Error),
    /// Input file length did not equal the fixed [`V5_CONTAINER_BYTES`] container geometry.
    BadContainerSize {
        /// Bytes observed in the source file.
        found: u64,
        /// Fixed v5.1.0 container byte size (`134,217,728`).
        expected: usize,
    },
    /// Pad source refused or could not be read.
    Pad(String),
    /// Authenticated directory version is not supported by this importer.
    UnsupportedVersion(u16),
    /// `ParamsClass::Test` reached a release build.
    TestScheduleRefused,
    /// Coarse refusal for wrong credentials, wrong cursor, corrupted capsule, or tampered data.
    Rejected,
    /// Streaming consumer aborted while receiving slot plaintext.
    Consumer(String),
}

impl core::fmt::Display for V5Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "v5 container I/O error: {err}"),
            Self::BadContainerSize { found, expected } => write!(
                f,
                "invalid v5.1.0 container length: found {found} bytes, expected {expected}"
            ),
            Self::Pad(reason) => write!(f, "v5.1.0 pad error: {reason}"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported v5.1.0 directory version {v}"),
            Self::TestScheduleRefused => {
                f.write_str("test-only kdf schedule refused in release build")
            }
            Self::Rejected => f.write_str(
                "v5.1.0 container rejected: wrong credentials, cursor, or altered bytes",
            ),
            Self::Consumer(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for V5Error {}

impl From<io::Error> for V5Error {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<KdfError> for V5Error {
    fn from(err: KdfError) -> Self {
        match err {
            KdfError::TestScheduleRefused => Self::TestScheduleRefused,
            KdfError::InvalidParams | KdfError::Argon2Failed => Self::Rejected,
        }
    }
}

/// 22-bit balanced Feistel permutation (`11`-bit halves, cycle-walked into Zone B), matching
/// `holonomy-v5-1-0/crates/holonomy-core/src/feistel.rs` byte-for-byte.
struct Feistel22 {
    round_keys: [u32; FEISTEL_ROUNDS as usize],
}

impl Feistel22 {
    fn provisioned(k_perm: &[u8; 32]) -> Self {
        let mut round_keys = [0u32; FEISTEL_ROUNDS as usize];
        for (i, slot) in round_keys.iter_mut().enumerate() {
            let o = (i * 4) % 32;
            let w = u32::from_le_bytes([k_perm[o], k_perm[o + 1], k_perm[o + 2], k_perm[o + 3]]);
            let mut x = w ^ (i as u32).wrapping_mul(0x9E37_79B9);
            x ^= x >> 16;
            x = x.wrapping_mul(0x85EB_CA6B);
            x ^= x >> 13;
            x = x.wrapping_mul(0xC2B2_AE35);
            x ^= x >> 16;
            *slot = x;
        }
        Self { round_keys }
    }

    #[inline]
    fn round_fn(half: u32, key: u32, round: u32) -> u32 {
        let mut x = half
            .wrapping_mul(0x9E37_79B9)
            .wrapping_add(key.rotate_left(7))
            ^ round.wrapping_mul(0x85EB_CA6B);
        x ^= x >> 15;
        x = x.wrapping_mul(0xC2B2_AE35);
        x ^= x >> 13;
        (x >> (32 - FEISTEL_HALF_BITS)) & FEISTEL_HALF_MASK
    }

    #[inline]
    fn permute_block(&self, x: u32) -> u32 {
        let mut left = (x >> FEISTEL_HALF_BITS) & FEISTEL_HALF_MASK;
        let mut right = x & FEISTEL_HALF_MASK;
        for round in 0..FEISTEL_ROUNDS {
            let f = Self::round_fn(right, self.round_keys[round as usize], round);
            let next_right = left ^ f;
            left = right;
            right = next_right;
        }
        (left << FEISTEL_HALF_BITS) | right
    }

    fn permute(&self, logical: u64) -> u32 {
        assert!(logical < u64::from(V5_ZONE_B_SECTORS));
        let mut x = logical as u32;
        for _ in 0..FEISTEL_DOMAIN {
            x = self.permute_block(x);
            if x < V5_ZONE_B_SECTORS {
                return x;
            }
        }
        unreachable!("Feistel22 orbit always re-enters Zone B within DOMAIN steps")
    }

    fn logical_sector(cursor: u64, j: u32) -> u64 {
        assert!(j < V5_PAD_SECTORS_PER_SAVE);
        cursor
            .checked_mul(u64::from(V5_PAD_SECTORS_PER_SAVE))
            .and_then(|base| base.checked_add(u64::from(j)))
            .expect("logical pad sector index overflow")
    }

    fn swap_chain(&self, n: u32) -> Vec<u32> {
        let mut order: Vec<u32> = (0..n).collect();
        for x in 0..n {
            let y = self.permute_block(x) % n;
            if y > x {
                order.swap(x as usize, y as usize);
            }
        }
        order
    }
}

fn k_perm_key(kappa_usb: &[u8; 32]) -> Zeroizing<[u8; 32]> {
    let mut h = Blake2b512::new();
    h.update(b"HOLONOMY_V51_K_PERM");
    h.update([0u8]);
    h.update(kappa_usb);
    let mut d = h.finalize();
    let mut out = Zeroizing::new([0u8; 32]);
    out.copy_from_slice(&d[..32]);
    d.as_mut_slice().zeroize();
    out
}

fn sector_material_into(sector: u32, out: &mut [u8; V5_SECTOR_BYTES]) {
    for (block, chunk) in out.chunks_mut(64).enumerate() {
        let mut h = Blake2b512::new();
        h.update(b"HOLONOMY_V51_STAND_IN_PAD");
        h.update(sector.to_le_bytes());
        h.update((block as u64).to_le_bytes());
        let mut d = h.finalize();
        chunk.copy_from_slice(&d[..chunk.len()]);
        d.as_mut_slice().zeroize();
    }
}

fn slot_perm_key(salt: &[u8; V5_SALT_BYTES]) -> [u8; 32] {
    let mut h = Blake2b512::new();
    h.update(b"HOLONOMY_V51_K_PERM_SLOT");
    h.update([0u8]);
    h.update(salt);
    let d = h.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&d[..32]);
    out
}

fn epoch_placement(salt: &[u8; V5_SALT_BYTES]) -> Vec<usize> {
    let feistel = Feistel22::provisioned(&slot_perm_key(salt));
    feistel
        .swap_chain(V5_PAYLOAD_SLOTS as u32)
        .into_iter()
        .map(|block| block as usize + V5_PAYLOAD_SLOT_BASE)
        .collect()
}

fn derive_v5_subkey(root: &[u8; 64], label: &[u8], context: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut h = Blake2b512::new();
    h.update((label.len() as u64).to_le_bytes());
    h.update(label);
    h.update([0u8]);
    h.update(root);
    h.update((context.len() as u64).to_le_bytes());
    h.update(context);
    let mut d = h.finalize();
    let mut out = Zeroizing::new([0u8; 32]);
    out.copy_from_slice(&d[..32]);
    d.as_mut_slice().zeroize();
    out
}

enum PadReaderState {
    None,
    Derived {
        cursor: u64,
        feistel: Feistel22,
    },
    Device {
        file: File,
        cursor: u64,
        zone_b_lba_start: u64,
        feistel: Feistel22,
    },
}

struct V5SectorReader {
    state: PadReaderState,
    sector_buf: Zeroizing<[u8; V5_SECTOR_BYTES]>,
}

impl V5SectorReader {
    fn open(pad: &V5PadSource, kappa_usb: &[u8; 32]) -> Result<Self, V5Error> {
        let state = match pad {
            V5PadSource::None => PadReaderState::None,
            V5PadSource::Derived { cursor } => {
                if *cursor >= V5_SAVES_BEFORE_DOMAIN_REUSE {
                    return Err(V5Error::Pad(format!(
                        "pad cursor {cursor} is at or beyond the {V5_SAVES_BEFORE_DOMAIN_REUSE}-save budget"
                    )));
                }
                let k_perm = k_perm_key(kappa_usb);
                PadReaderState::Derived {
                    cursor: *cursor,
                    feistel: Feistel22::provisioned(&k_perm),
                }
            }
            V5PadSource::DeviceFile {
                path,
                cursor,
                zone_b_lba_start,
            } => {
                if *cursor >= V5_SAVES_BEFORE_DOMAIN_REUSE {
                    return Err(V5Error::Pad(format!(
                        "pad cursor {cursor} is at or beyond the {V5_SAVES_BEFORE_DOMAIN_REUSE}-save budget"
                    )));
                }
                let file = File::open(path).map_err(|e| {
                    V5Error::Pad(format!("opening Zone B device {}: {e}", path.display()))
                })?;
                let len = file
                    .metadata()
                    .map_err(|e| V5Error::Pad(format!("stat {}: {e}", path.display())))?
                    .len();
                let required = zone_b_lba_start
                    .checked_add(u64::from(V5_ZONE_B_SECTORS))
                    .and_then(|s| s.checked_mul(V5_SECTOR_BYTES as u64))
                    .ok_or_else(|| V5Error::Pad("Zone B byte offset overflow".to_owned()))?;
                if len < required {
                    return Err(V5Error::Pad(format!(
                        "{} is {} bytes, shorter than required {required} bytes for Zone B",
                        path.display(),
                        len
                    )));
                }
                let k_perm = k_perm_key(kappa_usb);
                PadReaderState::Device {
                    file,
                    cursor: *cursor,
                    zone_b_lba_start: *zone_b_lba_start,
                    feistel: Feistel22::provisioned(&k_perm),
                }
            }
        };
        Ok(Self {
            state,
            sector_buf: Zeroizing::new([0u8; V5_SECTOR_BYTES]),
        })
    }

    fn cursor(&self) -> Option<u64> {
        match &self.state {
            PadReaderState::None => None,
            PadReaderState::Derived { cursor, .. } | PadReaderState::Device { cursor, .. } => {
                Some(*cursor)
            }
        }
    }

    fn xor_sector(&mut self, sector_in_container: u32, buf: &mut [u8]) -> Result<(), V5Error> {
        debug_assert_eq!(buf.len(), V5_SECTOR_BYTES);
        match &mut self.state {
            PadReaderState::None => Ok(()),
            PadReaderState::Derived { cursor, feistel } => {
                let phys = feistel.permute(Feistel22::logical_sector(*cursor, sector_in_container));
                sector_material_into(phys, &mut self.sector_buf);
                for (dst, src) in buf.iter_mut().zip(self.sector_buf.iter()) {
                    *dst ^= *src;
                }
                Ok(())
            }
            PadReaderState::Device {
                file,
                cursor,
                zone_b_lba_start,
                feistel,
            } => {
                let phys = feistel.permute(Feistel22::logical_sector(*cursor, sector_in_container));
                let byte_offset = (*zone_b_lba_start + u64::from(phys)) * V5_SECTOR_BYTES as u64;
                file.seek(SeekFrom::Start(byte_offset))
                    .map_err(|e| V5Error::Pad(format!("seeking Zone B sector {phys}: {e}")))?;
                file.read_exact(&mut self.sector_buf[..])
                    .map_err(|e| V5Error::Pad(format!("reading Zone B sector {phys}: {e}")))?;
                for (dst, src) in buf.iter_mut().zip(self.sector_buf.iter()) {
                    *dst ^= *src;
                }
                Ok(())
            }
        }
    }

    fn read_slot<R: Read + Seek>(
        &mut self,
        container: &mut R,
        block_index: usize,
        out: &mut [u8],
    ) -> Result<(), V5Error> {
        if out.len() != V5_SLOT_BYTES {
            return Err(V5Error::Rejected);
        }
        let offset = (block_index as u64) * (V5_SLOT_BYTES as u64);
        container.seek(SeekFrom::Start(offset))?;
        container.read_exact(out)?;
        for sub in 0..SECTORS_PER_SLOT {
            let sector_idx = (block_index * SECTORS_PER_SLOT + sub) as u32;
            let start = sub * V5_SECTOR_BYTES;
            self.xor_sector(sector_idx, &mut out[start..start + V5_SECTOR_BYTES])?;
        }
        Ok(())
    }

    fn read_slot_commitment<R: Read + Seek>(
        &mut self,
        container: &mut R,
        block_index: usize,
    ) -> Result<[u8; CMT4_TAG_BYTES], V5Error> {
        let last_sub = SECTORS_PER_SLOT - 1;
        let offset =
            (block_index as u64) * (V5_SLOT_BYTES as u64) + (last_sub * V5_SECTOR_BYTES) as u64;
        container.seek(SeekFrom::Start(offset))?;
        let mut tail_sector = Zeroizing::new([0u8; V5_SECTOR_BYTES]);
        container.read_exact(&mut tail_sector[..])?;
        let sector_idx = (block_index * SECTORS_PER_SLOT + last_sub) as u32;
        self.xor_sector(sector_idx, &mut tail_sector[..])?;
        let mut tag = [0u8; CMT4_TAG_BYTES];
        tag.copy_from_slice(&tail_sector[V5_SECTOR_BYTES - CMT4_TAG_BYTES..]);
        Ok(tag)
    }
}

#[derive(Zeroize, ZeroizeOnDrop)]
struct V5Unlocked {
    stratum: u8,
    k_dem: [u8; 32],
    n_epoch: [u8; NONCE_BYTES],
    directory: Vec<u8>,
}

fn directory_aad() -> [u8; 30] {
    let mut aad = [0u8; 30];
    aad[..22].copy_from_slice(b"HOLONOMY_V51_DIRECTORY");
    aad[22..30].copy_from_slice(&(DIRECTORY_PLAINTEXT_BYTES as u64).to_le_bytes());
    aad
}

fn unwrap_v5_stratum_key(
    k_unwrap: &[u8; 32],
    nonce: &[u8; NONCE_BYTES],
    wrapped: &[u8; WRAPPED_SK_BYTES],
) -> Option<HybridDecapsulationKey> {
    let cipher = XChaCha20Poly1305::new_from_slice(k_unwrap).ok()?;
    let mut plain = Zeroizing::new(wrapped[..WRAPPED_SK_PLAIN_BYTES].to_vec());
    let tag = Tag::from_slice(&wrapped[WRAPPED_SK_PLAIN_BYTES..]);
    cipher
        .decrypt_in_place_detached(
            XNonce::from_slice(nonce),
            b"HOLONOMY_V51_SK_WRAP",
            &mut plain[..],
            tag,
        )
        .ok()?;
    let mut ml_kem = Zeroizing::new([0u8; ML_KEM_1024_SEED_BYTES]);
    let mut x25519 = Zeroizing::new([0u8; X25519_SK_BYTES]);
    ml_kem.copy_from_slice(&plain[..ML_KEM_1024_SEED_BYTES]);
    x25519.copy_from_slice(&plain[ML_KEM_1024_SK_BYTES..WRAPPED_SK_PLAIN_BYTES]);
    Some(HybridDecapsulationKey {
        ml_kem: *ml_kem,
        x25519: *x25519,
    })
}

fn derive_v5_dem_key(
    pq: &[u8; 32],
    ecc: &[u8; 32],
    ct: &HybridCiphertext,
    k_unwrap: &[u8; 32],
    kappa_usb: &[u8; 32],
) -> (Zeroizing<[u8; 32]>, [u8; NONCE_BYTES]) {
    let mut h = Blake2b512::new();
    h.update(b"HOLONOMY_V51_DEM");
    h.update([0u8]);
    h.update(pq);
    h.update(ecc);
    h.update(ct.ml_kem);
    h.update(ct.x25519);
    h.update(k_unwrap);
    h.update(kappa_usb);
    let mut d = h.finalize();
    let mut k_dem = Zeroizing::new([0u8; 32]);
    let mut n_epoch = [0u8; NONCE_BYTES];
    k_dem.copy_from_slice(&d[..32]);
    n_epoch.copy_from_slice(&d[32..32 + NONCE_BYTES]);
    d.as_mut_slice().zeroize();
    (k_dem, n_epoch)
}

fn open_v5_directory(
    k_dem: &[u8; 32],
    nonce: &[u8; NONCE_BYTES],
    sealed: &[u8],
) -> Option<Zeroizing<Vec<u8>>> {
    if sealed.len() != SEALED_DIRECTORY_BYTES {
        return None;
    }
    let cipher = XChaCha20Poly1305::new_from_slice(k_dem).ok()?;
    let mut buf = Zeroizing::new(sealed[..DIRECTORY_PLAINTEXT_BYTES].to_vec());
    let tag = Tag::from_slice(&sealed[DIRECTORY_PLAINTEXT_BYTES..]);
    let aad = directory_aad();
    cipher
        .decrypt_in_place_detached(XNonce::from_slice(nonce), &aad, &mut buf[..], tag)
        .ok()?;
    Some(buf)
}

fn unlock_v5_capsule(
    passphrase: &V5Passphrase,
    salt: &[u8; V5_SALT_BYTES],
    capsule_bytes: &[u8],
    kappa_usb: &[u8; 32],
    params: ParamsClass,
) -> Result<V5Unlocked, V5Error> {
    if capsule_bytes.len() != V5_SLOT_BYTES {
        return Err(V5Error::Rejected);
    }
    let root = derive_root_secret_raw(passphrase.as_bytes(), salt, params)?;

    let mut pos = 0usize;
    let mut take = |n: usize| -> &[u8] {
        let s = &capsule_bytes[pos..pos + n];
        pos += n;
        s
    };
    let mut ct_ml = [0u8; ML_KEM_1024_CT_BYTES];
    ct_ml.copy_from_slice(take(ML_KEM_1024_CT_BYTES));
    let mut ct_x = [0u8; X25519_SK_BYTES];
    ct_x.copy_from_slice(take(X25519_SK_BYTES));
    let mut sk_unwrap_nonce = [0u8; NONCE_BYTES];
    sk_unwrap_nonce.copy_from_slice(take(NONCE_BYTES));
    let mut wrapped_sk = [0u8; WRAPPED_SK_BYTES];
    wrapped_sk.copy_from_slice(take(WRAPPED_SK_BYTES));
    let mut phase1_tag = [0u8; CMT4_TAG_BYTES];
    phase1_tag.copy_from_slice(take(CMT4_TAG_BYTES));
    let mut dir_nonce = [0u8; NONCE_BYTES];
    dir_nonce.copy_from_slice(take(NONCE_BYTES));
    let dir_ct_slice = take(DIRECTORY_PLAINTEXT_BYTES);
    let dir_poly_slice = take(POLY1305_TAG_BYTES);
    let mut phase2_tag = [0u8; CMT4_TAG_BYTES];
    phase2_tag.copy_from_slice(take(CMT4_TAG_BYTES));

    let ct = HybridCiphertext {
        ml_kem: ct_ml,
        x25519: ct_x,
    };

    let mut dir_sealed = Zeroizing::new(vec![0u8; SEALED_DIRECTORY_BYTES]);
    dir_sealed[..DIRECTORY_PLAINTEXT_BYTES].copy_from_slice(dir_ct_slice);
    dir_sealed[DIRECTORY_PLAINTEXT_BYTES..].copy_from_slice(dir_poly_slice);

    let mut p1_ad = Vec::with_capacity(NONCE_BYTES + ML_KEM_1024_CT_BYTES + X25519_SK_BYTES);
    p1_ad.extend_from_slice(&sk_unwrap_nonce);
    p1_ad.extend_from_slice(&ct.ml_kem);
    p1_ad.extend_from_slice(&ct.x25519);

    for index in 1..=V5_MAX_STRATA as u8 {
        let ctx = [index, 0u8];
        let k_unwrap = derive_v5_subkey(&root, b"HOLONOMY_V51_K_UNWRAP", &ctx);
        let Some(dk) = unwrap_v5_stratum_key(&k_unwrap, &sk_unwrap_nonce, &wrapped_sk) else {
            continue;
        };
        let expected_p1 = cmt4_tag(&k_unwrap, &sk_unwrap_nonce, &p1_ad, &wrapped_sk);
        if !cmt4_verify(&expected_p1, &phase1_tag) {
            continue;
        }
        let Ok(ss) = MlKem1024X25519.decapsulate(&dk, &ct) else {
            continue;
        };
        drop(dk);
        let (k_dem, n_epoch) = derive_v5_dem_key(&ss.pq, &ss.ecc, &ct, &k_unwrap, kappa_usb);
        let expected_p2 = cmt4_tag(
            &k_dem,
            &dir_nonce,
            b"HOLONOMY_V51_DIRECTORY",
            &dir_sealed[..],
        );
        if !cmt4_verify(&expected_p2, &phase2_tag) {
            continue;
        }
        let Some(mut directory) = open_v5_directory(&k_dem, &dir_nonce, &dir_sealed) else {
            continue;
        };
        let owned_dir = std::mem::take(&mut *directory);
        return Ok(V5Unlocked {
            stratum: index,
            k_dem: *k_dem,
            n_epoch,
            directory: owned_dir,
        });
    }

    Err(V5Error::Rejected)
}

fn ledger_tag(index: &[u8], commitments: &[[u8; CMT4_TAG_BYTES]]) -> [u8; CMT4_TAG_BYTES] {
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
    let mut out = [0u8; CMT4_TAG_BYTES];
    out.copy_from_slice(&d[..CMT4_TAG_BYTES]);
    out
}

fn v5_slot_aad(stratum: u8, slot_index: u32) -> [u8; 8] {
    let mut ad = [0u8; 8];
    ad[0] = stratum;
    ad[1..5].copy_from_slice(&slot_index.to_le_bytes());
    ad
}

fn v5_slot_nonce(
    base: &[u8; NONCE_BYTES],
    save_cursor: u64,
    slot_index: u32,
    stratum: u8,
) -> [u8; NONCE_BYTES] {
    let mut n = *base;
    let counter = save_cursor
        .wrapping_shl(32)
        .wrapping_add(u64::from(slot_index));
    let mut tail = [0u8; 16];
    tail[0..8].copy_from_slice(&counter.to_le_bytes());
    tail[8] = stratum;
    for i in 0..16 {
        n[8 + i] ^= tail[i];
    }
    n
}

/// Streams and authenticates an existing Holonomy v5.1.0 container slot-by-slot without loading the
/// 128 MiB image or pad window into memory.
///
/// The container's directory ledger, slot commitments, and AEAD tags are verified before trusting
/// any length; `on_slot` receives `(logical_slot_index, decrypted_slot_bytes)` in logical order,
/// with each slot buffer zeroized immediately before the next slot is opened.
///
/// # Errors
///
/// Returns [`V5Error`] on I/O failure, container size mismatch, pad failure, version mismatch,
/// authentication failure, or consumer error.
pub fn open_v5_streaming<R, F>(
    container: &mut R,
    passphrase: &V5Passphrase,
    kappa_usb: &[u8; 32],
    pad: &V5PadSource,
    expected_cursor: Option<u64>,
    params: ParamsClass,
    mut on_slot: F,
) -> Result<V5OpenReport, V5Error>
where
    R: Read + Seek,
    F: FnMut(usize, &[u8]) -> Result<(), String>,
{
    let len = container.seek(SeekFrom::End(0))?;
    if len != V5_CONTAINER_BYTES as u64 {
        return Err(V5Error::BadContainerSize {
            found: len,
            expected: V5_CONTAINER_BYTES,
        });
    }

    let mut reader = V5SectorReader::open(pad, kappa_usb)?;

    let mut slot_buf = Zeroizing::new(vec![0u8; V5_SLOT_BYTES]);
    reader.read_slot(container, V5_SALT_SLOT_INDEX, &mut slot_buf)?;
    let mut salt = [0u8; V5_SALT_BYTES];
    salt.copy_from_slice(&slot_buf[..V5_SALT_BYTES]);
    if salt.iter().all(|&b| b == 0) {
        return Err(V5Error::Rejected);
    }

    reader.read_slot(container, V5_ROOT_SLOT_INDEX, &mut slot_buf)?;
    let unlocked = unlock_v5_capsule(passphrase, &salt, &slot_buf, kappa_usb, params)?;

    let dir = &unlocked.directory;
    let version = u16::from_le_bytes([dir[0], dir[1]]);
    if version != DIRECTORY_VERSION_V1 {
        return Err(V5Error::UnsupportedVersion(version));
    }
    if dir[3] != 0 {
        return Err(V5Error::Rejected);
    }
    let dir_stratum = dir[2];
    if dir_stratum == 0
        || usize::from(dir_stratum) > V5_MAX_STRATA
        || dir_stratum != unlocked.stratum
    {
        return Err(V5Error::Rejected);
    }
    let dir_cursor = u64::from_le_bytes(dir[4..12].try_into().expect("8-byte cursor"));
    if let Some(expected) = expected_cursor {
        if dir_cursor != expected {
            return Err(V5Error::Rejected);
        }
    }
    if let Some(pad_cursor) = reader.cursor() {
        if dir_cursor != pad_cursor {
            return Err(V5Error::Rejected);
        }
    }

    let count = u32::from_le_bytes(dir[12..16].try_into().expect("4-byte count")) as usize;
    if count > V5_PAYLOAD_SLOTS {
        return Err(V5Error::Rejected);
    }

    let placement = epoch_placement(&salt);
    let mut commitments = Vec::with_capacity(count);
    for &phys_block in placement.iter().take(count) {
        commitments.push(reader.read_slot_commitment(container, phys_block)?);
    }
    let expected_ledger = ledger_tag(&dir[..DIR_TAG_OFFSET], &commitments);
    let mut actual_ledger = [0u8; CMT4_TAG_BYTES];
    actual_ledger.copy_from_slice(&dir[DIR_TAG_OFFSET..DIR_TAG_OFFSET + CMT4_TAG_BYTES]);
    if !cmt4_verify(&expected_ledger, &actual_ledger) {
        return Err(V5Error::Rejected);
    }

    let max_slot_ct = (V5_SLOT_BYTES - CMT4_TAG_BYTES) as u32;
    let mut ct_lengths = Vec::with_capacity(count);
    for i in 0..count {
        let at = DIR_HEADER_BYTES + i * DIR_LEN_BYTES;
        let ct_len = u32::from_le_bytes(dir[at..at + 4].try_into().expect("4-byte length"));
        if ct_len < POLY1305_TAG_BYTES as u32 || ct_len > max_slot_ct {
            return Err(V5Error::Rejected);
        }
        ct_lengths.push(ct_len as usize);
    }

    let mut total_payload_bytes = 0u64;
    for logical in 0..count {
        let physical = placement[logical];
        reader.read_slot(container, physical, &mut slot_buf)?;
        let ct_len = ct_lengths[logical];
        let nonce = v5_slot_nonce(
            &unlocked.n_epoch,
            dir_cursor,
            logical as u32,
            unlocked.stratum,
        );
        let aad = v5_slot_aad(unlocked.stratum, logical as u32);
        let committed = CommittedCiphertext {
            ciphertext: slot_buf[..ct_len].to_vec(),
            cmt4: commitments[logical],
        };
        let plaintext = open_committed(&unlocked.k_dem, &nonce, &aad, &committed)
            .map_err(|_| V5Error::Rejected)?;
        total_payload_bytes = total_payload_bytes
            .checked_add(plaintext.len() as u64)
            .ok_or(V5Error::Rejected)?;
        on_slot(logical, &plaintext).map_err(V5Error::Consumer)?;
    }

    Ok(V5OpenReport {
        stratum: unlocked.stratum,
        cursor: dir_cursor,
        slot_count: count,
        total_payload_bytes,
    })
}

/// Scans candidate pad cursors `0..54` to find the cursors under which a folded v5.1.0 container
/// authenticates (the read-only `recover-copy` discovery path).
pub fn scan_v5_cursors<R: Read + Seek>(
    container: &mut R,
    passphrase: &V5Passphrase,
    kappa_usb: &[u8; 32],
    pad_for_cursor: &mut dyn FnMut(u64) -> V5PadSource,
    params: ParamsClass,
) -> Result<Vec<u64>, V5Error> {
    let mut hits = Vec::new();
    for cursor in 0..V5_SAVES_BEFORE_DOMAIN_REUSE {
        let pad = pad_for_cursor(cursor);
        match open_v5_streaming(
            container,
            passphrase,
            kappa_usb,
            &pad,
            Some(cursor),
            params,
            |_, _| Ok(()),
        ) {
            Ok(report) => hits.push(report.cursor),
            Err(V5Error::Rejected | V5Error::Pad(_)) => {}
            Err(other) => return Err(other),
        }
    }
    Ok(hits)
}

/// Builds a genuine v5.1.0 container file for migration verification.
///
/// Refuses [`ParamsClass::Test`] in release builds without `debug_assertions`, mirroring `holonomy-seal`'s
/// KDF guard.
#[doc(hidden)]
pub fn write_v5_test_container(
    container_path: &Path,
    passphrase: &V5Passphrase,
    kappa_usb: &[u8; 32],
    pad: &V5PadSource,
    payload: &[u8],
    params: ParamsClass,
) -> Result<(), V5Error> {
    let mut salt = [0u8; V5_SALT_BYTES];
    crate::rng::fill(&mut salt).map_err(|_| V5Error::Rejected)?;
    let root = derive_root_secret_raw(passphrase.as_bytes(), &salt, params)?;
    let k_unwrap = derive_v5_subkey(&root, b"HOLONOMY_V51_K_UNWRAP", &[1u8, 0u8]);

    let (dk, ek) = MlKem1024X25519.keygen();
    let (ct, ss) = MlKem1024X25519
        .encapsulate(&ek)
        .map_err(|_| V5Error::Rejected)?;
    let sk_unwrap_nonce = crate::rng::fill_24();

    let mut plain_sk = Zeroizing::new([0u8; WRAPPED_SK_PLAIN_BYTES]);
    plain_sk[..ML_KEM_1024_SEED_BYTES].copy_from_slice(&dk.ml_kem);
    plain_sk[ML_KEM_1024_SK_BYTES..WRAPPED_SK_PLAIN_BYTES].copy_from_slice(&dk.x25519);
    let wrap_cipher = XChaCha20Poly1305::new_from_slice(&k_unwrap[..]).expect("32-byte key");
    let wrap_tag = wrap_cipher
        .encrypt_in_place_detached(
            XNonce::from_slice(&sk_unwrap_nonce),
            b"HOLONOMY_V51_SK_WRAP",
            &mut plain_sk[..],
        )
        .expect("bounded wrap");
    let mut wrapped_sk = [0u8; WRAPPED_SK_BYTES];
    wrapped_sk[..WRAPPED_SK_PLAIN_BYTES].copy_from_slice(&plain_sk[..]);
    wrapped_sk[WRAPPED_SK_PLAIN_BYTES..].copy_from_slice(wrap_tag.as_slice());

    let mut p1_ad = Vec::with_capacity(NONCE_BYTES + ML_KEM_1024_CT_BYTES + X25519_SK_BYTES);
    p1_ad.extend_from_slice(&sk_unwrap_nonce);
    p1_ad.extend_from_slice(&ct.ml_kem);
    p1_ad.extend_from_slice(&ct.x25519);
    let phase1_tag = cmt4_tag(&k_unwrap, &sk_unwrap_nonce, &p1_ad, &wrapped_sk);

    let (k_dem, n_epoch) = derive_v5_dem_key(&ss.pq, &ss.ecc, &ct, &k_unwrap, kappa_usb);
    drop(dk);

    let cursor = match pad {
        V5PadSource::None => 0,
        V5PadSource::Derived { cursor } | V5PadSource::DeviceFile { cursor, .. } => *cursor,
    };

    let slices: Vec<&[u8]> = if payload.is_empty() {
        vec![&[]]
    } else {
        payload.chunks(V5_PAYLOAD_PLAINTEXT_PER_SLOT).collect()
    };
    if slices.len() > V5_PAYLOAD_SLOTS {
        return Err(V5Error::Rejected);
    }

    let mut image = vec![0u8; V5_CONTAINER_BYTES];
    crate::rng::fill(&mut image).map_err(|_| V5Error::Rejected)?;

    image[V5_SLOT_BYTES..V5_SLOT_BYTES + V5_SALT_BYTES].copy_from_slice(&salt);
    image[V5_SLOT_BYTES + V5_SALT_BYTES..2 * V5_SLOT_BYTES].fill(0);

    let placement = epoch_placement(&salt);
    let mut commitments = Vec::with_capacity(slices.len());
    let mut lengths = Vec::with_capacity(slices.len());
    for (slot_idx, plain) in slices.iter().enumerate() {
        let nonce = v5_slot_nonce(&n_epoch, cursor, slot_idx as u32, 1);
        let aad = v5_slot_aad(1, slot_idx as u32);
        let committed = seal_committed(&k_dem, &nonce, &aad, plain);
        let phys = placement[slot_idx];
        let base = phys * V5_SLOT_BYTES;
        image[base..base + V5_SLOT_BYTES].fill(0);
        image[base..base + committed.ciphertext.len()].copy_from_slice(&committed.ciphertext);
        image[base + V5_SLOT_BYTES - CMT4_TAG_BYTES..base + V5_SLOT_BYTES]
            .copy_from_slice(&committed.cmt4);
        lengths.push(committed.ciphertext.len() as u32);
        commitments.push(committed.cmt4);
    }

    let mut dir_plain = Zeroizing::new([0u8; DIRECTORY_PLAINTEXT_BYTES]);
    dir_plain[0..2].copy_from_slice(&DIRECTORY_VERSION_V1.to_le_bytes());
    dir_plain[2] = 1;
    dir_plain[3] = 0;
    dir_plain[4..12].copy_from_slice(&cursor.to_le_bytes());
    dir_plain[12..16].copy_from_slice(&(slices.len() as u32).to_le_bytes());
    for (i, len) in lengths.iter().enumerate() {
        let at = DIR_HEADER_BYTES + i * DIR_LEN_BYTES;
        dir_plain[at..at + 4].copy_from_slice(&len.to_le_bytes());
    }
    let ltag = ledger_tag(&dir_plain[..DIR_TAG_OFFSET], &commitments);
    dir_plain[DIR_TAG_OFFSET..DIR_TAG_OFFSET + CMT4_TAG_BYTES].copy_from_slice(&ltag);

    let dir_nonce = crate::rng::fill_24();
    let dir_cipher = XChaCha20Poly1305::new_from_slice(&k_dem[..]).expect("32-byte key");
    let dir_aad = directory_aad();
    let dir_tag = dir_cipher
        .encrypt_in_place_detached(XNonce::from_slice(&dir_nonce), &dir_aad, &mut dir_plain[..])
        .expect("bounded dir");

    let mut dir_sealed = [0u8; SEALED_DIRECTORY_BYTES];
    dir_sealed[..DIRECTORY_PLAINTEXT_BYTES].copy_from_slice(&dir_plain[..]);
    dir_sealed[DIRECTORY_PLAINTEXT_BYTES..].copy_from_slice(dir_tag.as_slice());
    let phase2_tag = cmt4_tag(
        &k_dem,
        &dir_nonce,
        b"HOLONOMY_V51_DIRECTORY",
        &dir_sealed[..],
    );

    let mut cap = 0usize;
    let mut put = |bytes: &[u8]| {
        image[cap..cap + bytes.len()].copy_from_slice(bytes);
        cap += bytes.len();
    };
    put(&ct.ml_kem);
    put(&ct.x25519);
    put(&sk_unwrap_nonce);
    put(&wrapped_sk);
    put(&phase1_tag);
    put(&dir_nonce);
    put(&dir_sealed[..DIRECTORY_PLAINTEXT_BYTES]);
    put(&dir_sealed[DIRECTORY_PLAINTEXT_BYTES..]);
    put(&phase2_tag);
    assert_eq!(cap, V5_SLOT_BYTES);

    let mut sector_reader = V5SectorReader::open(pad, kappa_usb)?;
    for block in 0..V5_TOTAL_BLOCKS {
        for sub in 0..SECTORS_PER_SLOT {
            let s_idx = (block * SECTORS_PER_SLOT + sub) as u32;
            let off = block * V5_SLOT_BYTES + sub * V5_SECTOR_BYTES;
            sector_reader.xor_sector(s_idx, &mut image[off..off + V5_SECTOR_BYTES])?;
        }
    }

    std::fs::write(container_path, &image)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::io::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("holonomy-v5-{name}-{}-{nanos}", std::process::id()))
    }

    #[test]
    fn feistel22_54_saves_are_mutually_disjoint() {
        let f = Feistel22::provisioned(&[0x5Au8; 32]);
        let mut seen = HashSet::with_capacity(54 * V5_PAD_SECTORS_PER_SAVE as usize);
        for cursor in 0..V5_SAVES_BEFORE_DOMAIN_REUSE {
            for j in 0..V5_PAD_SECTORS_PER_SAVE {
                let phys = f.permute(Feistel22::logical_sector(cursor, j));
                assert!(phys < V5_ZONE_B_SECTORS);
                assert!(seen.insert(phys), "collision at cursor={cursor}, j={j}");
            }
        }
        assert_eq!(seen.len(), 54 * V5_PAD_SECTORS_PER_SAVE as usize);
    }

    #[test]
    fn passphrases_lowercase_compatibly() {
        let upper = V5Passphrase::new("Correct-Horse-Battery").unwrap();
        let lower = V5Passphrase::new("correct-horse-battery").unwrap();
        assert_eq!(upper.as_bytes(), lower.as_bytes());
        assert!(V5Passphrase::new("").is_none());
    }

    #[test]
    fn round_trip_unpadded_derived_and_relocated_device_pad() {
        let kappa = [0x42u8; 32];
        let pass = V5Passphrase::new("LegacyPass!").unwrap();
        let mut payload = Vec::new();
        for i in 0..5_000 {
            payload.extend_from_slice(format!("entry-{i:04}-日本語-café\n").as_bytes());
        }
        assert!(payload.len() > V5_PAYLOAD_PLAINTEXT_PER_SLOT);

        // 1. Raw unpadded image
        let p_raw = temp_path("raw.wf5");
        write_v5_test_container(
            &p_raw,
            &pass,
            &kappa,
            &V5PadSource::None,
            &payload,
            ParamsClass::Test,
        )
        .unwrap();
        let mut recovered = Vec::new();
        let mut f = File::open(&p_raw).unwrap();
        let rep = open_v5_streaming(
            &mut f,
            &pass,
            &kappa,
            &V5PadSource::None,
            Some(0),
            ParamsClass::Test,
            |_, bytes| {
                recovered.extend_from_slice(bytes);
                Ok(())
            },
        )
        .unwrap();
        let expected_slots = payload.len().div_ceil(V5_PAYLOAD_PLAINTEXT_PER_SLOT);
        assert_eq!(rep.slot_count, expected_slots);
        assert_eq!(recovered, payload);

        // Wrong passphrase or wrong kappa rejects cleanly
        let bad_pass = V5Passphrase::new("wrong").unwrap();
        assert!(matches!(
            open_v5_streaming(
                &mut f,
                &bad_pass,
                &kappa,
                &V5PadSource::None,
                Some(0),
                ParamsClass::Test,
                |_, _| Ok(()),
            ),
            Err(V5Error::Rejected)
        ));
        let _ = std::fs::remove_file(&p_raw);

        // 2. Derived pad at cursor 17
        let p_der = temp_path("derived.wf5");
        let pad_der = V5PadSource::Derived { cursor: 17 };
        write_v5_test_container(&p_der, &pass, &kappa, &pad_der, &payload, ParamsClass::Test)
            .unwrap();
        let mut recovered_der = Vec::new();
        let mut f_der = File::open(&p_der).unwrap();
        let rep_der = open_v5_streaming(
            &mut f_der,
            &pass,
            &kappa,
            &pad_der,
            Some(17),
            ParamsClass::Test,
            |_, bytes| {
                recovered_der.extend_from_slice(bytes);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(rep_der.cursor, 17);
        assert_eq!(recovered_der, payload);

        // Cursor scan discovers cursor 17
        let found = scan_v5_cursors(
            &mut f_der,
            &pass,
            &kappa,
            &mut |c| V5PadSource::Derived { cursor: c },
            ParamsClass::Test,
        )
        .unwrap();
        assert_eq!(found, vec![17]);
        let _ = std::fs::remove_file(&p_der);

        // 3. Relocated device Zone B backing file with sparse sectors
        let p_dev = temp_path("zoneb.img");
        let mut dev = File::create(&p_dev).unwrap();
        let zone_b_start = 16u64;
        let total_dev_bytes =
            (zone_b_start + u64::from(V5_ZONE_B_SECTORS)) * V5_SECTOR_BYTES as u64;
        // Write deterministic pattern only at the sectors cursor 3 will actually read!
        let k_perm = k_perm_key(&kappa);
        let feistel = Feistel22::provisioned(&k_perm);
        dev.set_len(total_dev_bytes).unwrap();
        for j in 0..V5_PAD_SECTORS_PER_SAVE {
            let phys = feistel.permute(Feistel22::logical_sector(3, j));
            let off = (zone_b_start + u64::from(phys)) * V5_SECTOR_BYTES as u64;
            dev.seek(SeekFrom::Start(off)).unwrap();
            let block = [j as u8; V5_SECTOR_BYTES];
            dev.write_all(&block).unwrap();
        }
        drop(dev);

        let p_cont = temp_path("devcont.wf5");
        let pad_dev = V5PadSource::device_relocated(&p_dev, 3, zone_b_start);
        write_v5_test_container(
            &p_cont,
            &pass,
            &kappa,
            &pad_dev,
            b"hello usb zone b",
            ParamsClass::Test,
        )
        .unwrap();
        let mut recovered_dev = Vec::new();
        let mut f_cont = File::open(&p_cont).unwrap();
        open_v5_streaming(
            &mut f_cont,
            &pass,
            &kappa,
            &pad_dev,
            Some(3),
            ParamsClass::Test,
            |_, bytes| {
                recovered_dev.extend_from_slice(bytes);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(recovered_dev, b"hello usb zone b");
        let _ = std::fs::remove_file(&p_dev);
        let _ = std::fs::remove_file(&p_cont);
    }
}
