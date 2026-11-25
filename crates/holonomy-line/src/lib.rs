//! The Witness — the append-only `.wavefunction` world-line container (ROADMAP Block 3).
//!
//! A file has a fixed public KDF/envelope header followed by framed, sealed segments and
//! sealed witnesses. Segment bodies are never read by [`WorldLine::open`]; the witness chain
//! authenticates the ordered segment metadata and the current envelope epoch. A normal save
//! appends segment records, syncs them, then appends and syncs one witness commit point.
//!
//! > **History is storage, not a feature.** Writes append; compaction is a separate explicit
//! > operation. Power loss before a complete witness leaves the preceding witness authoritative.

mod format;
mod mmr;

pub use format::LineHeader;

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use holonomy_seal::{
    open_object, seal_object, CompactionApproval, DEMKey, OpenedPlaintext, SealedObject,
    SecretArenas,
};

use crate::format::{HEADER_BYTES, RECORD_FIXED_BYTES, RECORD_HEADER_BYTES};
use crate::mmr::{segment_leaf, Mmr};

const RECORD_MAGIC: &[u8; 4] = b"W6RC";
const RECORD_SEGMENT: u8 = 1;
const RECORD_WITNESS: u8 = 2;
const RECORD_MANIFEST: u8 = 3;
const RECORD_BOOKMARK: u8 = 4;
const AEAD_TAG_BYTES: u32 = 16;
const MAX_SEGMENT_BYTES: u32 = 64 * 1024;
const MAX_BOOKMARK_NAME_BYTES: usize = 256;
const BOOKMARK_MIN_BYTES: usize = 1 + 8 + 8 + 2 + 1;
const MAX_BOOKMARK_BYTES: usize = 1 + 8 + 8 + 2 + MAX_BOOKMARK_NAME_BYTES;
const MAX_BOOKMARK_CIPHERTEXT_BYTES: u32 = (MAX_BOOKMARK_BYTES as u32) + AEAD_TAG_BYTES;
const MAX_BOOKMARKS: usize = 65_536;
const WITNESS_BASE_BYTES: usize = 1 + 8 + 8 + 32 + 32 + 32 + 32;
const WITNESS_MANIFEST_REF_BYTES: usize = 8 + 4 + 32;
const MAX_WITNESS_BYTES: usize = WITNESS_BASE_BYTES + 64 * 32 + WITNESS_MANIFEST_REF_BYTES;
const MAX_WITNESS_CIPHERTEXT_BYTES: u32 = (MAX_WITNESS_BYTES as u32) + AEAD_TAG_BYTES;
const MANIFEST_HEADER_BYTES: usize = 4;
const MANIFEST_ENTRY_BYTES: usize = 8 + 4 + 4;
const MANIFEST_V2_MAGIC: &[u8; 4] = b"M6V2";
const MANIFEST_STYLE_BYTES: usize = 8 + 8 + 8 + 4 + 4 + 4;
const MAX_MANIFEST_SLABS: usize = 1_048_576;
const MAX_MANIFEST_STYLES: usize = 262_144;
const MAX_MANIFEST_LINKS: usize = 65_536;
const MAX_LINK_BYTES: usize = 4_096;
const MAX_MANIFEST_BYTES: u32 = 32 * 1024 * 1024;
const MAX_MANIFEST_CIPHERTEXT_BYTES: u32 = MAX_MANIFEST_BYTES + AEAD_TAG_BYTES;

// Test-only hooks exercise short writes and failed durability barriers through the same
// append path used in production. The thread-local scope keeps unrelated parallel tests from
// sharing failpoints or counters.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AppendIoWrite {
    offset: u64,
    bytes: usize,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AppendIoSync {
    bytes_written: u64,
    succeeded: bool,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AppendIoTruncate {
    length: u64,
    succeeded: bool,
}

#[cfg(test)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct AppendIoSnapshot {
    writes: Vec<AppendIoWrite>,
    syncs: Vec<AppendIoSync>,
    truncations: Vec<AppendIoTruncate>,
    bytes_written: u64,
}

#[cfg(test)]
struct AppendIoState {
    fail_after_write_bytes: Option<u64>,
    fail_on_sync: Option<usize>,
    fail_truncate: bool,
    snapshot: AppendIoSnapshot,
}

#[cfg(test)]
thread_local! {
    static APPEND_IO_STATE: std::cell::RefCell<Option<AppendIoState>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
struct AppendIoProbeGuard;

#[cfg(test)]
impl AppendIoProbeGuard {
    fn install(
        fail_after_write_bytes: Option<u64>,
        fail_on_sync: Option<usize>,
        fail_truncate: bool,
    ) -> Self {
        APPEND_IO_STATE.with(|slot| {
            let mut slot = slot.borrow_mut();
            assert!(slot.is_none(), "append I/O probe already installed");
            *slot = Some(AppendIoState {
                fail_after_write_bytes,
                fail_on_sync,
                fail_truncate,
                snapshot: AppendIoSnapshot::default(),
            });
        });
        Self
    }

    fn snapshot(&self) -> AppendIoSnapshot {
        APPEND_IO_STATE.with(|slot| {
            slot.borrow()
                .as_ref()
                .expect("append I/O probe was removed")
                .snapshot
                .clone()
        })
    }
}

#[cfg(test)]
impl Drop for AppendIoProbeGuard {
    fn drop(&mut self) {
        APPEND_IO_STATE.with(|slot| *slot.borrow_mut() = None);
    }
}

/// Public failures from the world-line format and storage layer.
#[derive(Debug)]
pub enum LineError {
    /// The filesystem rejected a read, write, sync, or path operation.
    Io(io::Error),
    /// The file header is truncated or malformed.
    InvalidHeader,
    /// The path is a symlink, non-regular, or was unlinked after being opened.
    UnsafePath,
    /// The file uses an unsupported header version or reserved field.
    UnsupportedHeader,
    /// The header's Argon2 parameters fall outside the production fence.
    KdfParametersRejected,
    /// A framed record is malformed or violates the append order.
    InvalidRecord,
    /// A sealed witness or segment failed authentication; wrong key and tampering are not
    /// distinguished at this boundary.
    AuthenticationRejected,
    /// The file has no complete authenticated genesis witness.
    NoCommittedWitness,
    /// The append-only sequence would overflow the wire format's u64 counters.
    TooManySegments,
    /// Empty segments do not have a canonical representation.
    EmptySegment,
    /// A segment exceeds the fixed 64 KiB quantum.
    SegmentTooLarge,
    /// Another writer changed the file since this handle was opened.
    ConcurrentModification,
    /// A previous write or sync failed; reopen the file before attempting another operation.
    ReopenRequired,
    /// A commit record was written but its durability could not be confirmed.
    DurabilityUncertain,
    /// The OS entropy source failed while creating a file identifier.
    EntropyUnavailable,
    /// This target has no implemented durable directory-sync primitive yet.
    UnsupportedDurability,
    /// A segment index is not present in the committed witness.
    NoSuchSegment,
    /// A witness index is not present in the world-line history.
    NoSuchWitness,
    /// Memory could not be reserved for bounded segment metadata.
    ResourceExhausted,
    /// The one-shot, passphrase-rederived approval is absent or belongs to another file.
    CompactionConfirmationRequired,
    /// Compaction must create a distinct world-line identifier and witness epoch.
    CompactionIdentityReuse,
    /// The requested creation staging path cannot serve as a distinct, safe destination.
    InvalidCreationDestination,
    /// The requested compacted output path cannot serve as a distinct, safe destination.
    InvalidCompactionDestination,
    /// A proposed spine manifest names an empty, overflowing, or out-of-range segment span.
    InvalidManifest(&'static str),
    /// A bookmark name or anchor witness is invalid.
    InvalidBookmark(&'static str),
}

impl core::fmt::Display for LineError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "world-line I/O failed: {error}"),
            Self::InvalidHeader => f.write_str("invalid world-line header"),
            Self::UnsafePath => {
                f.write_str("world-line path is not safely linked to a regular file")
            }
            Self::UnsupportedHeader => f.write_str("unsupported world-line header version"),
            Self::KdfParametersRejected => {
                f.write_str("world-line KDF parameters are outside the permitted fence")
            }
            Self::InvalidRecord => f.write_str("invalid world-line record"),
            Self::AuthenticationRejected => {
                f.write_str("the world-line could not be authenticated")
            }
            Self::NoCommittedWitness => f.write_str("no committed world-line witness exists"),
            Self::TooManySegments => f.write_str("world-line sequence limit reached"),
            Self::EmptySegment => f.write_str("empty world-line segments are not allowed"),
            Self::SegmentTooLarge => f.write_str("world-line segment exceeds 64 KiB"),
            Self::ConcurrentModification => f.write_str("world-line changed since it was opened"),
            Self::ReopenRequired => f.write_str("reopen the world-line before continuing"),
            Self::DurabilityUncertain => {
                f.write_str("world-line commit durability could not be confirmed")
            }
            Self::EntropyUnavailable => f.write_str("system entropy unavailable"),
            Self::UnsupportedDurability => {
                f.write_str("durable directory sync is unsupported on this target")
            }
            Self::NoSuchSegment => f.write_str("world-line segment does not exist"),
            Self::NoSuchWitness => f.write_str("world-line witness does not exist"),
            Self::ResourceExhausted => f.write_str("world-line metadata exceeds available memory"),
            Self::CompactionConfirmationRequired => {
                f.write_str("passphrase confirmation is required for this history compaction")
            }
            Self::CompactionIdentityReuse => {
                f.write_str("compaction must create a new world-line identity")
            }
            Self::InvalidCreationDestination => {
                f.write_str("creation requires a destination distinct from its staging path")
            }
            Self::InvalidCompactionDestination => {
                f.write_str("compaction requires a distinct destination file path")
            }
            Self::InvalidManifest(reason) => {
                write!(f, "invalid world-line spine manifest: {reason}")
            }
            Self::InvalidBookmark(reason) => {
                write!(f, "invalid world-line bookmark: {reason}")
            }
        }
    }
}

impl std::error::Error for LineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for LineError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Recovery information for an incomplete or uncommitted physical tail.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryInfo {
    /// Bytes after the last complete witness. They are ignored on open and discarded before the
    /// next append; they never replace the last committed state.
    pub trailing_bytes: u64,
    /// Byte offset immediately after the last authenticated witness.
    pub last_committed_offset: u64,
}

impl RecoveryInfo {
    /// Whether a prior interrupted append left physical bytes to recover or discard.
    pub fn needs_recovery(self) -> bool {
        self.trailing_bytes != 0
    }
}

/// Receipt for one append transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppendReceipt {
    /// Sequence number of the newly committed witness.
    pub witness_sequence: u64,
    /// Total committed segments after this transaction.
    pub segment_count: u64,
    /// Bytes written for segment records and the witness (not counting any recovered tail that
    /// was truncated before the transaction).
    pub bytes_appended: u64,
    /// Authenticated root of the append-only segment set.
    pub segment_root: [u8; 32],
}

/// One contiguous plaintext span of a committed segment in document order.
///
/// A world-line that has only ever been appended to has an implicit manifest of one full-length
/// slab per segment (`0..segment_count`); once a cut, mid-segment insertion, or deletion has been
/// committed, the latest witness names an authenticated manifest of [`LineSlab`] spans — or an
/// empty manifest when the entire document has been deleted — so reopening never decrypts segment
/// bodies and never resurrects removed text (AUDIT OQ-29, OQ-30).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LineSlab {
    /// Zero-based sequence number of the committed segment.
    pub segment: u64,
    /// Starting byte offset inside that segment's plaintext.
    pub start: u32,
    /// Non-zero byte length inside that segment's plaintext (`start + len <= plaintext_len`).
    pub len: u32,
}

/// One canonical rich-text style span over `[start, end)` in document byte coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineStyleRun {
    /// Inclusive start byte offset in the active document (`start < end`).
    pub start: u64,
    /// Exclusive end byte offset in the active document (`end <= total_doc_bytes`).
    pub end: u64,
    /// Packed boolean inline flags (`0x01` bold, `0x02` italic, `0x04` underline,
    /// `0x08` strikethrough, `0x10` code_span, `0x20` horizontal_rule).
    pub flags: u8,
    /// Font family selector (`0` = Sans, `1` = Serif, `2` = Mono).
    pub family: u8,
    /// Paragraph heading level (`0` = None, `1` = Title, `2` = H1, `3` = H2, `4` = H3).
    pub heading: u8,
    /// Paragraph alignment (`0` = Start, `1` = Center, `2` = End, `3` = Justify).
    pub alignment: u8,
    /// List kind (`0` = None, `1` = Bullet, `2` = Ordered).
    pub list_kind: u8,
    /// Nesting depth (`0..=8`; `0` when `list_kind == 0`).
    pub list_depth: u8,
    /// Blockquote nesting depth (`0..=8`).
    pub blockquote: u8,
    /// Font size in thousandths (`1_000` = `1.0x`; must be `> 0`).
    pub size_milli: u32,
    /// Starting item index for ordered lists (`>= 1` when `list_kind == 2`, else `0`).
    pub list_start: u32,
    /// 1-based index into [`LineManifest::links`], or `0` when unlinked.
    pub link_id: u32,
}

/// Complete authenticated structural manifest: slab spans plus optional rich-text style layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineManifest {
    /// Ordered [`LineSlab`] spans forming the active document byte stream.
    pub slabs: Vec<LineSlab>,
    /// Sorted, non-overlapping style spans over `[0, total_doc_bytes)`.
    pub styles: Vec<LineStyleRun>,
    /// Link target table referenced by 1-based [`LineStyleRun::link_id`].
    pub links: Vec<String>,
}

/// Result of fully authenticating all committed segment bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifyReport {
    /// Number of segments authenticated.
    pub segments_verified: u64,
    /// Sum of plaintext bytes authenticated (plaintext is wiped as each segment leaves scope).
    pub plaintext_bytes_verified: u64,
    /// Number of committed witnesses, including the genesis witness.
    pub witnesses_verified: u64,
    /// Any uncommitted tail observed while scanning.
    pub recovery: RecoveryInfo,
}

/// Authenticated summary of one committed witness point in the world-line (Block 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WitnessSummary {
    /// Zero-based witness sequence number (`0` is genesis).
    pub sequence: u64,
    /// Total committed segments present at this witness point.
    pub segment_count: u64,
    /// Explicit manifest sequence active at this witness, or `None` if implicit `0..segment_count`.
    pub manifest_sequence: Option<u64>,
}

/// Authenticated named bookmark anchored to a committed witness in the world-line (Block 8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bookmark {
    /// Zero-based bookmark sequence number.
    pub sequence: u64,
    /// Committed witness sequence this bookmark names.
    pub witness_sequence: u64,
    /// Unix timestamp in seconds when the bookmark was recorded.
    pub timestamp_secs: i64,
    /// Human-readable label (`1..=256` UTF-8 bytes).
    pub name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

#[derive(Clone, PartialEq, Eq)]
struct SegmentDescriptor {
    sequence: u64,
    ciphertext_offset: u64,
    ciphertext_len: u32,
    plaintext_len: u32,
    nonce: [u8; 24],
    tag: [u8; 32],
    previous_tag: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ManifestRef {
    sequence: u64,
    ciphertext_len: u32,
    tag: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ManifestDescriptor {
    sequence: u64,
    segment_limit: usize,
    ciphertext_offset: u64,
    ciphertext_len: u32,
    nonce: [u8; 24],
    tag: [u8; 32],
    previous_witness_tag: [u8; 32],
}

/// An opened append-only world-line. The key is an opaque capability owned by the handle;
/// this crate can request seal/open operations but cannot read key bytes.
pub struct WorldLine {
    path: PathBuf,
    header: LineHeader,
    epoch: [u8; 32],
    key: DEMKey,
    file_identity: FileIdentity,
    segments: Vec<SegmentDescriptor>,
    manifests: Vec<ManifestDescriptor>,
    witnesses: Vec<WitnessSummary>,
    bookmarks: Vec<Bookmark>,
    manifest: Option<LineManifest>,
    mmr: Mmr,
    next_witness_sequence: u64,
    last_witness_tag: [u8; 32],
    last_segment_tag: [u8; 32],
    last_bookmark_tag: [u8; 32],
    committed_end: u64,
    observed_len: u64,
    recovery: RecoveryInfo,
    poisoned: bool,
}

impl WorldLine {
    /// Create a new file without replacing an existing path. A fully synced staging inode is
    /// linked into place atomically, then the containing directory is synced.
    pub fn create(
        path: impl AsRef<Path>,
        header: LineHeader,
        key: DEMKey,
    ) -> Result<Self, LineError> {
        if !durability_supported() {
            return Err(LineError::UnsupportedDurability);
        }
        header.validate()?;
        let path = path.as_ref().to_path_buf();
        if path.file_name().is_none() {
            return Err(LineError::InvalidHeader);
        }
        let parent = parent_directory(&path);
        let stage = parent.join(stage_name(header.file_id()));
        if stage == path {
            return Err(LineError::InvalidCreationDestination);
        }
        let epoch = header.epoch_id();
        let empty_mmr = Mmr::new();
        let zero_tag = [0u8; 32];
        let (genesis, genesis_tag) = encode_witness_record(
            &header, &epoch, &key, 0, &zero_tag, &zero_tag, &empty_mmr, None,
        )?;

        let mut stage_options = OpenOptions::new();
        stage_options.read(true).write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            stage_options.mode(0o600);
        }
        let mut file = stage_options.open(&stage)?;

        let initialized = (|| -> Result<(u64, FileIdentity), LineError> {
            header.write_to(&mut file)?;
            file.write_all(&genesis)?;
            file.sync_all()?;
            let end = file.stream_position()?;
            Ok((end, file_identity(&file)?))
        })();
        let (committed_end, file_identity) = match initialized {
            Ok(initialized) => initialized,
            Err(error) => {
                let _ = fs::remove_file(&stage);
                return Err(error);
            }
        };
        let _creation_lock = match FileAppendLock::acquire(&file) {
            Ok(lock) => lock,
            Err(error) => {
                let _ = fs::remove_file(&stage);
                return Err(error);
            }
        };

        if let Err(error) = fs::hard_link(&stage, &path) {
            let _ = fs::remove_file(&stage);
            return Err(LineError::Io(error));
        }
        if let Err(error) = sync_parent_directory(&path) {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(&stage);
            let _ = sync_parent_directory(&path);
            return Err(if error.kind() == io::ErrorKind::Unsupported {
                LineError::UnsupportedDurability
            } else {
                LineError::DurabilityUncertain
            });
        }
        if let Err(error) = fs::remove_file(&stage) {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(&stage);
            let _ = sync_parent_directory(&path);
            return Err(LineError::Io(error));
        }
        if let Err(error) = sync_parent_directory(&path) {
            let _ = fs::remove_file(&path);
            let _ = sync_parent_directory(&path);
            return Err(if error.kind() == io::ErrorKind::Unsupported {
                LineError::UnsupportedDurability
            } else {
                LineError::DurabilityUncertain
            });
        }

        Ok(Self {
            path,
            header,
            epoch,
            key,
            file_identity,
            segments: Vec::new(),
            manifests: Vec::new(),
            witnesses: vec![WitnessSummary {
                sequence: 0,
                segment_count: 0,
                manifest_sequence: None,
            }],
            bookmarks: Vec::new(),
            manifest: None,
            mmr: empty_mmr,
            next_witness_sequence: 1,
            last_witness_tag: genesis_tag,
            last_segment_tag: zero_tag,
            last_bookmark_tag: zero_tag,
            committed_end,
            observed_len: committed_end,
            recovery: RecoveryInfo {
                trailing_bytes: 0,
                last_committed_offset: committed_end,
            },
            poisoned: false,
        })
    }

    /// Open a file using an already-unwrapped document key. This scans header and witness
    /// metadata, seeks over every segment body and historical manifest, and opens only the
    /// active spine manifest when one has been committed.
    pub fn open(path: impl AsRef<Path>, key: DEMKey) -> Result<Self, LineError> {
        let path = path.as_ref().to_path_buf();
        let mut file = open_worldline_file(&path, true, false)?;
        let _read_lock = FileReadLock::acquire(&file)?;
        validate_regular_file(&file)?;
        let file_identity = file_identity(&file)?;
        let header = LineHeader::read_from(&mut file)?;
        let epoch = header.epoch_id();
        let scan = scan_records(&mut file, &header, &epoch, &key)?;
        Ok(Self {
            path,
            header,
            epoch,
            key,
            file_identity,
            segments: scan.segments,
            manifests: scan.manifests,
            witnesses: scan.witnesses,
            bookmarks: scan.bookmarks,
            manifest: scan.manifest,
            mmr: scan.mmr,
            next_witness_sequence: scan.next_witness_sequence,
            last_witness_tag: scan.last_witness_tag,
            last_segment_tag: scan.last_segment_tag,
            last_bookmark_tag: scan.last_bookmark_tag,
            committed_end: scan.committed_end,
            observed_len: scan.observed_len,
            recovery: scan.recovery,
            poisoned: false,
        })
    }

    /// Read the public header used to derive the unwrap key before calling [`Self::open`].
    pub fn header(&self) -> &LineHeader {
        &self.header
    }

    /// Arms this world-line's live [`DEMKey`] capability cell in `arenas` for emergency zeroing.
    ///
    /// # Safety
    ///
    /// No key access may overlap an arena wipe, and registered pointers must remain valid.
    pub unsafe fn arm_reaper(&mut self, arenas: &Arc<SecretArenas>) -> bool {
        // SAFETY: the caller guarantees that key use and wiping are mutually exclusive.
        unsafe { self.key.arm_reaper(arenas) }
    }

    /// Whether the emergency reaper has zeroized this world-line's [`DEMKey`] in place.
    #[must_use]
    pub fn is_key_zeroized(&self) -> bool {
        self.key.is_zeroized()
    }

    /// The number of segments covered by the current authenticated witness.
    pub fn segment_count(&self) -> u64 {
        self.mmr.count()
    }

    /// The current authenticated root over segment record metadata.
    pub fn segment_root(&self) -> [u8; 32] {
        self.mmr.root()
    }

    /// The plaintext length of every committed segment, in order.
    ///
    /// This is the geometry a document reopens with: the lengths are authenticated metadata in the
    /// record headers, so a window can lay out a document without decrypting a byte of it — the
    /// same rule the resident set lives by (AUDIT OQ-9's load-time half).
    pub fn segment_lengths(&self) -> impl Iterator<Item = u32> + '_ {
        self.segments
            .iter()
            .map(|descriptor| descriptor.plaintext_len)
    }

    /// Whether an explicit spine manifest has been committed on this world-line.
    #[must_use]
    pub fn has_manifest(&self) -> bool {
        self.manifest.is_some()
    }

    /// The active document's [`LineSlab`] geometry, without opening any segment ciphertext.
    ///
    /// When no structural manifest has been committed, the implicit append-order geometry
    /// (`0..segment_count`, each spanning its full authenticated plaintext length) is returned.
    /// When a manifest has been committed, its exact authenticated slab list — which may be empty
    /// if the entire document was deleted — is returned (AUDIT OQ-29, OQ-30).
    #[must_use]
    pub fn document_slabs(&self) -> Vec<LineSlab> {
        if let Some(manifest) = &self.manifest {
            return manifest.slabs.clone();
        }
        self.segments
            .iter()
            .map(|descriptor| LineSlab {
                segment: descriptor.sequence,
                start: 0,
                len: descriptor.plaintext_len,
            })
            .collect()
    }

    /// The active document's full [`LineManifest`] (slab spans plus rich-text style and link
    /// tables), without opening any segment ciphertext.
    #[must_use]
    pub fn document_manifest(&self) -> LineManifest {
        if let Some(manifest) = &self.manifest {
            return manifest.clone();
        }
        LineManifest {
            slabs: self.document_slabs(),
            styles: Vec::new(),
            links: Vec::new(),
        }
    }

    /// Whether `desired` equals the implicit append-order geometry that would result from
    /// appending `plaintext_segments` with no explicit manifest.
    #[must_use]
    pub fn is_implicit_after_append(
        &self,
        plaintext_segments: &[&[u8]],
        desired: &[LineSlab],
    ) -> bool {
        if self.manifest.is_some() {
            return false;
        }
        if desired.len() != self.segments.len() + plaintext_segments.len() {
            return false;
        }
        for (index, descriptor) in self.segments.iter().enumerate() {
            let expected = LineSlab {
                segment: descriptor.sequence,
                start: 0,
                len: descriptor.plaintext_len,
            };
            if desired[index] != expected {
                return false;
            }
        }
        let base = self.segments.len();
        for (offset, plaintext) in plaintext_segments.iter().enumerate() {
            let Ok(len) = u32::try_from(plaintext.len()) else {
                return false;
            };
            let expected = LineSlab {
                segment: (base + offset) as u64,
                start: 0,
                len,
            };
            if desired[base + offset] != expected {
                return false;
            }
        }
        true
    }

    /// Number of committed witness records, including genesis.
    pub fn witness_count(&self) -> u64 {
        self.next_witness_sequence
    }

    /// Authenticated witness points in chronological order (`0` is genesis).
    #[must_use]
    pub fn witness_history(&self) -> &[WitnessSummary] {
        &self.witnesses
    }

    /// Authenticated named bookmarks stored in this world-line, in append order.
    #[must_use]
    pub fn bookmarks(&self) -> &[Bookmark] {
        &self.bookmarks
    }

    /// Reconstructs the exact [`LineManifest`] active at historical `witness_sequence` (`0..witness_count()`)
    /// without decrypting any segment bodies (Block 8).
    pub fn manifest_at_witness(&self, witness_sequence: u64) -> Result<LineManifest, LineError> {
        if self.poisoned {
            return Err(LineError::ReopenRequired);
        }
        let idx = usize::try_from(witness_sequence).map_err(|_| LineError::NoSuchWitness)?;
        let summary = self.witnesses.get(idx).ok_or(LineError::NoSuchWitness)?;
        if let Some(m_seq) = summary.manifest_sequence {
            let m_idx = usize::try_from(m_seq).map_err(|_| LineError::InvalidRecord)?;
            let descriptor = self.manifests.get(m_idx).ok_or(LineError::InvalidRecord)?;
            return self.read_manifest(descriptor);
        }
        let seg_count =
            usize::try_from(summary.segment_count).map_err(|_| LineError::InvalidRecord)?;
        let slice = self
            .segments
            .get(..seg_count)
            .ok_or(LineError::InvalidRecord)?;
        let slabs = slice
            .iter()
            .map(|descriptor| LineSlab {
                segment: descriptor.sequence,
                start: 0,
                len: descriptor.plaintext_len,
            })
            .collect();
        Ok(LineManifest {
            slabs,
            styles: Vec::new(),
            links: Vec::new(),
        })
    }

    /// Appends an authenticated named bookmark anchored to `witness_sequence` (Block 8).
    pub fn add_bookmark(
        &mut self,
        witness_sequence: u64,
        timestamp_secs: i64,
        name: &str,
    ) -> Result<Bookmark, LineError> {
        if !durability_supported() {
            return Err(LineError::UnsupportedDurability);
        }
        if self.poisoned {
            return Err(LineError::ReopenRequired);
        }
        if witness_sequence >= self.next_witness_sequence {
            return Err(LineError::NoSuchWitness);
        }
        if name.is_empty() || name.len() > MAX_BOOKMARK_NAME_BYTES {
            return Err(LineError::InvalidBookmark(
                "bookmark name must be 1..=256 UTF-8 bytes",
            ));
        }
        if self.bookmarks.len() >= MAX_BOOKMARKS {
            return Err(LineError::InvalidBookmark("bookmark limit reached"));
        }
        let result = self.append_bookmark_internal(witness_sequence, timestamp_secs, name);
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    /// Begins a bounded-memory streaming append transaction (`Block 7` dense replacement &
    /// `Block 10` streaming import), writing encrypted 64 KiB segments directly to the log one at
    /// a time and sealing them under one final witness commit point.
    pub fn begin_streaming_append(&mut self) -> Result<StreamingAppend<'_>, LineError> {
        let tx = self.start_streaming_tx()?;
        Ok(StreamingAppend {
            line: self,
            tx: Some(tx),
        })
    }

    /// Starts an owned bounded-memory streaming transaction with no lifetime borrow on `self`,
    /// suitable for [`std::cell::RefCell`]- or [`std::sync::Mutex`]-wrapped handles.
    ///
    /// While the returned [`StreamingTx`] is open, `self` rejects interleaved writes until
    /// [`Self::finish_streaming_tx`] or [`Self::abort_streaming_tx`] completes.
    pub fn start_streaming_tx(&mut self) -> Result<StreamingTx, LineError> {
        if !durability_supported() {
            return Err(LineError::UnsupportedDurability);
        }
        if self.poisoned {
            return Err(LineError::ReopenRequired);
        }
        if self.next_witness_sequence == u64::MAX {
            return Err(LineError::TooManySegments);
        }
        let (file, append_lock) = self.prepare_append_file()?;
        self.poisoned = true;
        Ok(StreamingTx {
            header: self.header.clone(),
            epoch: self.epoch,
            key: self.key.duplicate(),
            file,
            _append_lock: append_lock,
            mmr: self.mmr.clone(),
            last_segment_tag: self.last_segment_tag,
            next_segment_seq: self.mmr.count(),
            new_descriptors: Vec::new(),
            bytes_written: 0,
            committed_end: self.committed_end,
            failed: false,
        })
    }

    /// Commits all segments streamed into `tx` alongside an optional explicit [`LineManifest`]
    /// under a single durable witness commit point.
    pub fn finish_streaming_tx(
        &mut self,
        mut tx: StreamingTx,
        explicit_manifest: Option<&LineManifest>,
    ) -> Result<AppendReceipt, LineError> {
        if tx.failed {
            self.poisoned = true;
            return Err(LineError::ReopenRequired);
        }
        if tx.new_descriptors.is_empty() && explicit_manifest.is_none() {
            self.poisoned = false;
            return Ok(self.current_receipt());
        }
        let pending_lengths: Vec<u32> = tx
            .new_descriptors
            .iter()
            .map(|descriptor| descriptor.plaintext_len)
            .collect();
        if let Some(manifest) = explicit_manifest {
            if let Err(err) =
                validate_manifest_against_lengths(&self.segments, &pending_lengths, manifest)
            {
                self.poisoned = true;
                return Err(err);
            }
        }
        let next_manifest = match explicit_manifest {
            Some(manifest) => Some(manifest.clone()),
            None => {
                if let Some(existing) = &self.manifest {
                    if existing.slabs.len().saturating_add(tx.new_descriptors.len())
                        > MAX_MANIFEST_SLABS
                    {
                        self.poisoned = true;
                        return Err(LineError::InvalidManifest(
                            "manifest exceeds maximum slab count",
                        ));
                    }
                    let mut extended =
                        Vec::with_capacity(existing.slabs.len() + tx.new_descriptors.len());
                    extended.extend_from_slice(&existing.slabs);
                    for descriptor in &tx.new_descriptors {
                        extended.push(LineSlab {
                            segment: descriptor.sequence,
                            start: 0,
                            len: descriptor.plaintext_len,
                        });
                    }
                    Some(LineManifest {
                        slabs: extended,
                        styles: existing.styles.clone(),
                        links: existing.links.clone(),
                    })
                } else {
                    None
                }
            }
        };

        let result = (|| -> Result<AppendReceipt, LineError> {
            self.segments
                .try_reserve(tx.new_descriptors.len())
                .map_err(|_| LineError::ResourceExhausted)?;

            let mut new_manifest_descriptor = None;
            let manifest_ref = if let Some(slabs) = &next_manifest {
                self.manifests
                    .try_reserve(1)
                    .map_err(|_| LineError::ResourceExhausted)?;
                let manifest_sequence =
                    u64::try_from(self.manifests.len()).map_err(|_| LineError::TooManySegments)?;
                let encoded = encode_manifest_record(
                    &self.header,
                    &self.epoch,
                    &self.key,
                    manifest_sequence,
                    &self.last_witness_tag,
                    slabs,
                )?;
                let record_offset = tx.file.stream_position()?;
                write_append_bytes(&mut tx.file, &encoded.frame)?;
                tx.bytes_written = tx
                    .bytes_written
                    .checked_add(encoded.frame.len() as u64)
                    .ok_or(LineError::ResourceExhausted)?;
                let ciphertext_offset = record_offset
                    .checked_add(RECORD_HEADER_BYTES as u64 + RECORD_FIXED_BYTES as u64)
                    .ok_or(LineError::ResourceExhausted)?;
                let segment_limit = self.segments.len() + tx.new_descriptors.len();
                new_manifest_descriptor = Some(ManifestDescriptor {
                    sequence: manifest_sequence,
                    segment_limit,
                    ciphertext_offset,
                    ciphertext_len: encoded.ciphertext_len,
                    nonce: encoded.nonce,
                    tag: encoded.tag,
                    previous_witness_tag: self.last_witness_tag,
                });
                Some(ManifestRef {
                    sequence: manifest_sequence,
                    ciphertext_len: encoded.ciphertext_len,
                    tag: encoded.tag,
                })
            } else {
                None
            };

            sync_append_file(&tx.file)?;
            let witness_sequence = self.next_witness_sequence;
            let (witness_frame, witness_tag) = encode_witness_record(
                &self.header,
                &self.epoch,
                &self.key,
                witness_sequence,
                &self.last_witness_tag,
                &tx.last_segment_tag,
                &tx.mmr,
                manifest_ref,
            )?;
            write_append_bytes(&mut tx.file, &witness_frame)?;
            tx.bytes_written = tx
                .bytes_written
                .checked_add(witness_frame.len() as u64)
                .ok_or(LineError::ResourceExhausted)?;
            if sync_append_file(&tx.file).is_err() {
                return Err(LineError::DurabilityUncertain);
            }
            let committed_end = tx.file.stream_position()?;

            self.segments.append(&mut tx.new_descriptors);
            let manifest_sequence = new_manifest_descriptor
                .as_ref()
                .map(|descriptor| descriptor.sequence);
            if let Some(descriptor) = new_manifest_descriptor {
                self.manifests.push(descriptor);
            }
            self.witnesses.push(WitnessSummary {
                sequence: witness_sequence,
                segment_count: tx.mmr.count(),
                manifest_sequence,
            });
            self.manifest = next_manifest;
            self.mmr = tx.mmr;
            self.last_segment_tag = tx.last_segment_tag;
            self.last_witness_tag = witness_tag;
            self.next_witness_sequence = witness_sequence
                .checked_add(1)
                .ok_or(LineError::TooManySegments)?;
            self.committed_end = committed_end;
            self.observed_len = committed_end;
            self.recovery = RecoveryInfo {
                trailing_bytes: 0,
                last_committed_offset: committed_end,
            };
            self.poisoned = false;
            Ok(AppendReceipt {
                witness_sequence,
                segment_count: self.mmr.count(),
                bytes_appended: tx.bytes_written,
                segment_root: self.mmr.root(),
            })
        })();

        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    /// Aborts an uncommitted [`StreamingTx`], truncating any unwritten tail back to the last
    /// committed witness so the world-line handle remains usable if cleanup succeeds.
    pub fn abort_streaming_tx(&mut self, tx: StreamingTx) -> Result<(), LineError> {
        let res = (|| -> Result<(), LineError> {
            truncate_append_tail(&tx.file, tx.committed_end)?;
            sync_append_file(&tx.file)?;
            self.observed_len = tx.committed_end;
            self.recovery = RecoveryInfo {
                trailing_bytes: 0,
                last_committed_offset: tx.committed_end,
            };
            self.poisoned = false;
            Ok(())
        })();
        if res.is_err() {
            self.poisoned = true;
        }
        res
    }

    /// Recovery information observed when this handle was opened or last committed.
    pub fn recovery_info(&self) -> RecoveryInfo {
        self.recovery
    }

    /// Append one or more sealed segments and commit them with one witness. Empty input is a
    /// no-op. Every non-empty segment must fit the 64 KiB quantum.
    ///
    /// When no spine manifest is active, this writes a version-1 witness with zero manifest
    /// overhead; when a manifest is already active, the newly appended segments are appended to
    /// the manifest in the same atomic transaction.
    pub fn append_segments(
        &mut self,
        plaintext_segments: &[&[u8]],
    ) -> Result<AppendReceipt, LineError> {
        self.validate_append_inputs(plaintext_segments)?;
        if plaintext_segments.is_empty() {
            return Ok(self.current_receipt());
        }
        if let Some(existing) = &self.manifest {
            if existing
                .slabs
                .len()
                .saturating_add(plaintext_segments.len())
                > MAX_MANIFEST_SLABS
            {
                return Err(LineError::InvalidManifest(
                    "manifest exceeds maximum slab count",
                ));
            }
        }
        let result = self.commit_internal(plaintext_segments, None);
        if result.is_err() {
            // I/O may have left a partial, uncommitted tail. A fresh open is the only operation
            // that can unambiguously determine whether the witness reached durable storage.
            self.poisoned = true;
        }
        result
    }

    /// Atomically append one or more sealed segments and commit `manifest` as the active
    /// document geometry under a single witness commit point.
    pub fn append_segments_with_manifest(
        &mut self,
        plaintext_segments: &[&[u8]],
        manifest: &[LineSlab],
    ) -> Result<AppendReceipt, LineError> {
        let full = LineManifest {
            slabs: manifest.to_vec(),
            styles: Vec::new(),
            links: Vec::new(),
        };
        self.append_segments_with_full_manifest(plaintext_segments, &full)
    }

    /// Atomically append one or more sealed segments and commit `manifest` (slab spans, rich-text
    /// style runs, and link targets) under a single witness commit point.
    pub fn append_segments_with_full_manifest(
        &mut self,
        plaintext_segments: &[&[u8]],
        manifest: &LineManifest,
    ) -> Result<AppendReceipt, LineError> {
        if plaintext_segments.is_empty() {
            return self.commit_full_manifest(manifest);
        }
        self.validate_append_inputs(plaintext_segments)?;
        validate_manifest_against_pending(&self.segments, plaintext_segments, manifest)?;
        let result = self.commit_internal(plaintext_segments, Some(manifest));
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    /// Commit `manifest` as the active document geometry without reading, decrypting, or
    /// appending any segment plaintext.
    ///
    /// An empty `manifest` (`&[]`) records an explicitly emptied document (`Select All` + `Delete`)
    /// so reopening yields zero bytes rather than falling back to append order (AUDIT OQ-30).
    pub fn commit_manifest(&mut self, manifest: &[LineSlab]) -> Result<AppendReceipt, LineError> {
        let full = LineManifest {
            slabs: manifest.to_vec(),
            styles: Vec::new(),
            links: Vec::new(),
        };
        self.commit_full_manifest(&full)
    }

    /// Commit `manifest` (slab spans plus optional rich-text styles and links) without appending
    /// any segment plaintext.
    pub fn commit_full_manifest(
        &mut self,
        manifest: &LineManifest,
    ) -> Result<AppendReceipt, LineError> {
        if !durability_supported() {
            return Err(LineError::UnsupportedDurability);
        }
        if self.poisoned {
            return Err(LineError::ReopenRequired);
        }
        if self.next_witness_sequence == u64::MAX {
            return Err(LineError::TooManySegments);
        }
        validate_manifest_against_pending(&self.segments, &[], manifest)?;
        let result = self.commit_internal(&[], Some(manifest));
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }

    fn validate_append_inputs(&self, plaintext_segments: &[&[u8]]) -> Result<(), LineError> {
        if !durability_supported() {
            return Err(LineError::UnsupportedDurability);
        }
        if self.poisoned {
            return Err(LineError::ReopenRequired);
        }
        for segment in plaintext_segments {
            if segment.is_empty() {
                return Err(LineError::EmptySegment);
            }
            if segment.len() > MAX_SEGMENT_BYTES as usize {
                return Err(LineError::SegmentTooLarge);
            }
        }
        if !plaintext_segments.is_empty() {
            let pending_segments =
                u64::try_from(plaintext_segments.len()).map_err(|_| LineError::TooManySegments)?;
            self.mmr
                .count()
                .checked_add(pending_segments)
                .ok_or(LineError::TooManySegments)?;
            if self.next_witness_sequence == u64::MAX {
                return Err(LineError::TooManySegments);
            }
        }
        Ok(())
    }

    /// Open one segment on demand; the returned plaintext is zeroizing and should remain local
    /// to the caller's viewport/residency scope.
    pub fn open_segment(&self, sequence: u64) -> Result<OpenedPlaintext, LineError> {
        if self.poisoned {
            return Err(LineError::ReopenRequired);
        }
        let index = usize::try_from(sequence).map_err(|_| LineError::NoSuchSegment)?;
        let descriptor = self.segments.get(index).ok_or(LineError::NoSuchSegment)?;
        self.read_segment(descriptor)
    }

    /// Authenticate every committed manifest and segment body, one at a time. This is the
    /// explicit full-file integrity pass used by `verify`; ordinary open never reads segment
    /// ciphertext or historical manifests.
    pub fn verify(&self) -> Result<VerifyReport, LineError> {
        if self.poisoned {
            return Err(LineError::ReopenRequired);
        }
        let current = Self::open(&self.path, self.key.duplicate())?;
        if current.file_identity != self.file_identity
            || current.last_witness_tag != self.last_witness_tag
            || current.header.file_id() != self.header.file_id()
            || current.epoch != self.epoch
        {
            return Err(LineError::ConcurrentModification);
        }

        for manifest in &current.manifests {
            let _ = current.read_manifest(manifest)?;
        }

        let mut plaintext_bytes_verified = 0u64;
        for descriptor in &current.segments {
            let plaintext = current.read_segment(descriptor)?;
            plaintext_bytes_verified = plaintext_bytes_verified
                .checked_add(plaintext.len() as u64)
                .ok_or(LineError::ResourceExhausted)?;
            // `plaintext` is zeroized on drop before the next segment is opened.
        }
        Ok(VerifyReport {
            segments_verified: current.segment_count(),
            plaintext_bytes_verified,
            witnesses_verified: current.witness_count(),
            recovery: current.recovery_info(),
        })
    }

    /// Explicitly rewrite selected committed segments into a staged new file and re-base its
    /// witness chain. Only after all segments verify and commit is the result atomically linked
    /// into the destination without replacing an existing path. The source is never modified.
    /// The caller supplies a fresh header/key pair for the new file, a strictly increasing list
    /// of segments to retain, and a one-shot approval made by `holonomy-seal` after the
    /// passphrase is re-entered. At most one segment plaintext is resident at a time.
    pub fn compact_to(
        &self,
        destination: impl AsRef<Path>,
        retain_segments: &[u64],
        new_header: LineHeader,
        new_key: &DEMKey,
        approval: CompactionApproval,
    ) -> Result<WorldLine, LineError> {
        if self.poisoned {
            return Err(LineError::ReopenRequired);
        }
        let destination = destination.as_ref().to_path_buf();
        if destination.file_name().is_none() || destination == self.path {
            return Err(LineError::InvalidCompactionDestination);
        }
        new_header.validate()?;
        if !approval.is_for_file(self.header.file_id()) {
            return Err(LineError::CompactionConfirmationRequired);
        }
        if new_header.file_id() == self.header.file_id() {
            return Err(LineError::CompactionIdentityReuse);
        }
        if retain_segments.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(LineError::InvalidRecord);
        }
        if retain_segments
            .iter()
            .any(|&sequence| sequence >= self.segment_count())
        {
            return Err(LineError::NoSuchSegment);
        }

        self.verify()?;
        let temporary = compaction_path(&destination, new_header.file_id());
        if temporary == destination {
            return Err(LineError::InvalidCompactionDestination);
        }
        let mut compacted = WorldLine::create(&temporary, new_header, new_key.duplicate())?;
        let mut publication =
            CompactionPublication::new(temporary, destination.clone(), compacted.file_identity);
        for &sequence in retain_segments {
            let plaintext = self.open_segment(sequence)?;
            compacted.append_segments(&[&plaintext[..]])?;
            // `plaintext` drops and zeroizes before the next old segment is opened.
        }
        compacted.verify()?;
        publication.publish()?;
        compacted.path = destination;
        publication.commit();
        Ok(compacted)
    }

    /// Rewrite only the active [`Self::document_slabs`] into a fresh, re-keyed world-line at
    /// `destination`, omitting all deleted byte ranges and superseded history (AUDIT OQ-14).
    ///
    /// At most one source segment's [`OpenedPlaintext`] is resident in memory at any instant;
    /// the source world-line is verified first and never modified.
    pub fn compact_active_to(
        &self,
        destination: impl AsRef<Path>,
        new_header: LineHeader,
        new_key: &DEMKey,
        approval: CompactionApproval,
    ) -> Result<WorldLine, LineError> {
        if self.poisoned {
            return Err(LineError::ReopenRequired);
        }
        let destination = destination.as_ref().to_path_buf();
        if destination.file_name().is_none() || destination == self.path {
            return Err(LineError::InvalidCompactionDestination);
        }
        new_header.validate()?;
        if !approval.is_for_file(self.header.file_id()) {
            return Err(LineError::CompactionConfirmationRequired);
        }
        if new_header.file_id() == self.header.file_id() {
            return Err(LineError::CompactionIdentityReuse);
        }

        self.verify()?;
        let source_manifest = self.document_manifest();
        let temporary = compaction_path(&destination, new_header.file_id());
        if temporary == destination {
            return Err(LineError::InvalidCompactionDestination);
        }
        let mut compacted = WorldLine::create(&temporary, new_header, new_key.duplicate())?;
        let mut publication =
            CompactionPublication::new(temporary, destination.clone(), compacted.file_identity);
        for slab in &source_manifest.slabs {
            let plaintext = self.open_segment(slab.segment)?;
            let start = slab.start as usize;
            let end = start
                .checked_add(slab.len as usize)
                .ok_or(LineError::InvalidRecord)?;
            let slice = plaintext.get(start..end).ok_or(LineError::InvalidRecord)?;
            compacted.append_segments(&[slice])?;
            // `plaintext` drops and zeroizes before the next slab's segment is opened.
        }
        if !source_manifest.styles.is_empty() || !source_manifest.links.is_empty() {
            let preserved = LineManifest {
                slabs: compacted.document_slabs(),
                styles: source_manifest.styles,
                links: source_manifest.links,
            };
            compacted.commit_full_manifest(&preserved)?;
        }
        compacted.verify()?;
        publication.publish()?;
        compacted.path = destination;
        publication.commit();
        Ok(compacted)
    }

    fn current_receipt(&self) -> AppendReceipt {
        AppendReceipt {
            witness_sequence: self.next_witness_sequence.saturating_sub(1),
            segment_count: self.mmr.count(),
            bytes_appended: 0,
            segment_root: self.mmr.root(),
        }
    }

    fn prepare_append_file(&mut self) -> Result<(File, FileAppendLock), LineError> {
        let mut file = open_worldline_file(&self.path, true, true)?;
        validate_regular_file(&file)?;
        if file_identity(&file)? != self.file_identity {
            return Err(LineError::ConcurrentModification);
        }
        let append_lock = FileAppendLock::acquire(&file)?;
        let actual_len = file.metadata()?.len();
        if actual_len < self.committed_end {
            return Err(LineError::ConcurrentModification);
        }
        let mut reader = file.try_clone()?;
        let disk_header =
            LineHeader::read_from(&mut reader).map_err(|_| LineError::ConcurrentModification)?;
        let disk_epoch = disk_header.epoch_id();
        if disk_header.file_id() != self.header.file_id() || disk_epoch != self.epoch {
            return Err(LineError::ConcurrentModification);
        }
        let disk_state = scan_records(&mut reader, &disk_header, &disk_epoch, &self.key)
            .map_err(|_| LineError::ConcurrentModification)?;
        if disk_state.observed_len != actual_len
            || file.metadata()?.len() != actual_len
            || disk_state.committed_end != self.committed_end
            || disk_state.last_witness_tag != self.last_witness_tag
            || disk_state.last_segment_tag != self.last_segment_tag
            || disk_state.last_bookmark_tag != self.last_bookmark_tag
            || disk_state.next_witness_sequence != self.next_witness_sequence
            || disk_state.mmr != self.mmr
            || disk_state.segments != self.segments
            || disk_state.manifests != self.manifests
            || disk_state.witnesses != self.witnesses
            || disk_state.bookmarks != self.bookmarks
            || disk_state.manifest != self.manifest
        {
            return Err(LineError::ConcurrentModification);
        }
        self.observed_len = disk_state.observed_len;
        self.recovery = disk_state.recovery;

        if self.recovery.needs_recovery() {
            truncate_append_tail(&file, self.committed_end)?;
            sync_append_file(&file)?;
            self.observed_len = self.committed_end;
            self.recovery = RecoveryInfo {
                trailing_bytes: 0,
                last_committed_offset: self.committed_end,
            };
        }
        file.seek(SeekFrom::End(0))?;
        Ok((file, append_lock))
    }

    fn append_bookmark_internal(
        &mut self,
        witness_sequence: u64,
        timestamp_secs: i64,
        name: &str,
    ) -> Result<Bookmark, LineError> {
        let (mut file, _append_lock) = self.prepare_append_file()?;
        let sequence =
            u64::try_from(self.bookmarks.len()).map_err(|_| LineError::TooManySegments)?;
        let encoded = encode_bookmark_record(
            &self.header,
            &self.epoch,
            &self.key,
            sequence,
            &self.last_witness_tag,
            &self.last_bookmark_tag,
            witness_sequence,
            timestamp_secs,
            name,
        )?;
        write_append_bytes(&mut file, &encoded.frame)?;
        if sync_append_file(&file).is_err() {
            return Err(LineError::DurabilityUncertain);
        }
        let committed_end = file.stream_position()?;
        let bookmark = Bookmark {
            sequence,
            witness_sequence,
            timestamp_secs,
            name: name.to_owned(),
        };
        self.bookmarks.push(bookmark.clone());
        self.last_bookmark_tag = encoded.tag;
        self.committed_end = committed_end;
        self.observed_len = committed_end;
        self.recovery = RecoveryInfo {
            trailing_bytes: 0,
            last_committed_offset: committed_end,
        };
        Ok(bookmark)
    }

    fn commit_internal(
        &mut self,
        plaintext_segments: &[&[u8]],
        explicit_manifest: Option<&LineManifest>,
    ) -> Result<AppendReceipt, LineError> {
        let (mut file, _append_lock) = self.prepare_append_file()?;

        let next_manifest = match explicit_manifest {
            Some(manifest) => Some(manifest.clone()),
            None => {
                if let Some(existing) = &self.manifest {
                    let mut extended = Vec::new();
                    extended
                        .try_reserve(existing.slabs.len() + plaintext_segments.len())
                        .map_err(|_| LineError::ResourceExhausted)?;
                    extended.extend_from_slice(&existing.slabs);
                    let base = self.mmr.count();
                    for (index, plaintext) in plaintext_segments.iter().enumerate() {
                        let len = u32::try_from(plaintext.len())
                            .map_err(|_| LineError::SegmentTooLarge)?;
                        extended.push(LineSlab {
                            segment: base + index as u64,
                            start: 0,
                            len,
                        });
                    }
                    Some(LineManifest {
                        slabs: extended,
                        styles: existing.styles.clone(),
                        links: existing.links.clone(),
                    })
                } else {
                    None
                }
            }
        };

        let mut mmr = self.mmr.clone();
        let mut last_segment_tag = self.last_segment_tag;
        let mut new_descriptors = Vec::new();
        new_descriptors
            .try_reserve(plaintext_segments.len())
            .map_err(|_| LineError::ResourceExhausted)?;
        self.segments
            .try_reserve(plaintext_segments.len())
            .map_err(|_| LineError::ResourceExhausted)?;
        let mut bytes_written = 0u64;

        for plaintext in plaintext_segments {
            let sequence = mmr.count();
            let encoded = encode_segment_record(
                &self.header,
                &self.epoch,
                &self.key,
                sequence,
                &last_segment_tag,
                plaintext,
            )?;
            let record_offset = file.stream_position()?;
            write_append_bytes(&mut file, &encoded.frame)?;
            bytes_written = bytes_written
                .checked_add(encoded.frame.len() as u64)
                .ok_or(LineError::ResourceExhausted)?;
            let ciphertext_offset = record_offset
                .checked_add(RECORD_HEADER_BYTES as u64 + RECORD_FIXED_BYTES as u64)
                .ok_or(LineError::ResourceExhausted)?;
            new_descriptors.push(SegmentDescriptor {
                sequence,
                ciphertext_offset,
                ciphertext_len: encoded.ciphertext_len,
                plaintext_len: encoded.plaintext_len,
                nonce: encoded.nonce,
                tag: encoded.tag,
                previous_tag: last_segment_tag,
            });
            mmr.append(encoded.leaf)?;
            last_segment_tag = encoded.tag;
        }

        let mut new_manifest_descriptor = None;
        let manifest_ref = if let Some(slabs) = &next_manifest {
            self.manifests
                .try_reserve(1)
                .map_err(|_| LineError::ResourceExhausted)?;
            let manifest_sequence =
                u64::try_from(self.manifests.len()).map_err(|_| LineError::TooManySegments)?;
            let encoded = encode_manifest_record(
                &self.header,
                &self.epoch,
                &self.key,
                manifest_sequence,
                &self.last_witness_tag,
                slabs,
            )?;
            let record_offset = file.stream_position()?;
            write_append_bytes(&mut file, &encoded.frame)?;
            bytes_written = bytes_written
                .checked_add(encoded.frame.len() as u64)
                .ok_or(LineError::ResourceExhausted)?;
            let ciphertext_offset = record_offset
                .checked_add(RECORD_HEADER_BYTES as u64 + RECORD_FIXED_BYTES as u64)
                .ok_or(LineError::ResourceExhausted)?;
            let segment_limit = self.segments.len() + new_descriptors.len();
            new_manifest_descriptor = Some(ManifestDescriptor {
                sequence: manifest_sequence,
                segment_limit,
                ciphertext_offset,
                ciphertext_len: encoded.ciphertext_len,
                nonce: encoded.nonce,
                tag: encoded.tag,
                previous_witness_tag: self.last_witness_tag,
            });
            Some(ManifestRef {
                sequence: manifest_sequence,
                ciphertext_len: encoded.ciphertext_len,
                tag: encoded.tag,
            })
        } else {
            None
        };

        // Data and manifest records are durable before the witness commit point is written.
        sync_append_file(&file)?;
        let witness_sequence = self.next_witness_sequence;
        let (witness_frame, witness_tag) = encode_witness_record(
            &self.header,
            &self.epoch,
            &self.key,
            witness_sequence,
            &self.last_witness_tag,
            &last_segment_tag,
            &mmr,
            manifest_ref,
        )?;
        write_append_bytes(&mut file, &witness_frame)?;
        bytes_written = bytes_written
            .checked_add(witness_frame.len() as u64)
            .ok_or(LineError::ResourceExhausted)?;
        if sync_append_file(&file).is_err() {
            return Err(LineError::DurabilityUncertain);
        }
        let committed_end = file.stream_position()?;

        self.segments.extend(new_descriptors);
        let manifest_sequence = new_manifest_descriptor
            .as_ref()
            .map(|descriptor| descriptor.sequence);
        if let Some(descriptor) = new_manifest_descriptor {
            self.manifests.push(descriptor);
        }
        self.witnesses.push(WitnessSummary {
            sequence: witness_sequence,
            segment_count: mmr.count(),
            manifest_sequence,
        });
        self.manifest = next_manifest;
        self.mmr = mmr;
        self.last_segment_tag = last_segment_tag;
        self.last_witness_tag = witness_tag;
        self.next_witness_sequence = witness_sequence
            .checked_add(1)
            .ok_or(LineError::TooManySegments)?;
        self.committed_end = committed_end;
        self.observed_len = committed_end;
        self.recovery = RecoveryInfo {
            trailing_bytes: 0,
            last_committed_offset: committed_end,
        };

        Ok(AppendReceipt {
            witness_sequence,
            segment_count: self.mmr.count(),
            bytes_appended: bytes_written,
            segment_root: self.mmr.root(),
        })
    }

    fn read_manifest(&self, descriptor: &ManifestDescriptor) -> Result<LineManifest, LineError> {
        let mut file = open_worldline_file(&self.path, true, false)?;
        validate_regular_file(&file)?;
        if file_identity(&file)? != self.file_identity {
            return Err(LineError::ConcurrentModification);
        }
        let allowed = self
            .segments
            .get(..descriptor.segment_limit)
            .ok_or(LineError::InvalidRecord)?;
        read_manifest_from(
            &mut file,
            &self.header,
            &self.epoch,
            &self.key,
            descriptor,
            allowed,
        )
    }

    fn read_segment(&self, descriptor: &SegmentDescriptor) -> Result<OpenedPlaintext, LineError> {
        let mut file = open_worldline_file(&self.path, true, false)?;
        validate_regular_file(&file)?;
        if file_identity(&file)? != self.file_identity {
            return Err(LineError::ConcurrentModification);
        }
        file.seek(SeekFrom::Start(descriptor.ciphertext_offset))?;
        let mut ciphertext = vec![0u8; descriptor.ciphertext_len as usize];
        file.read_exact(&mut ciphertext)?;
        let sealed = SealedObject {
            nonce: descriptor.nonce,
            ciphertext,
            cmt4: descriptor.tag,
        };
        let aad = segment_aad(
            &self.header,
            &self.epoch,
            descriptor.sequence,
            &descriptor.previous_tag,
            descriptor.plaintext_len,
        );
        let plaintext =
            open_object(&self.key, &aad, &sealed).map_err(|_| LineError::AuthenticationRejected)?;
        if plaintext.len() != descriptor.plaintext_len as usize {
            return Err(LineError::AuthenticationRejected);
        }
        Ok(plaintext)
    }
}

fn write_append_bytes(file: &mut File, mut bytes: &[u8]) -> io::Result<()> {
    #[cfg(test)]
    let mut offset = file.stream_position()?;

    while !bytes.is_empty() {
        #[cfg(test)]
        let allowed = append_test_write_limit(bytes.len())?;
        #[cfg(not(test))]
        let allowed = bytes.len();

        match file.write(&bytes[..allowed]) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write world-line append record",
                ));
            }
            Ok(written) => {
                #[cfg(test)]
                {
                    record_append_test_write(offset, written);
                    offset = offset.saturating_add(written as u64);
                }
                bytes = &bytes[written..];
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn sync_append_file(file: &File) -> io::Result<()> {
    #[cfg(test)]
    let (event_index, injected_failure) = APPEND_IO_STATE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(state) = slot.as_mut() else {
            return (None, false);
        };
        let index = state.snapshot.syncs.len();
        let fail = state.fail_on_sync == Some(index + 1);
        state.snapshot.syncs.push(AppendIoSync {
            bytes_written: state.snapshot.bytes_written,
            succeeded: false,
        });
        (Some(index), fail)
    });
    #[cfg(not(test))]
    let injected_failure = false;

    let result = if injected_failure {
        Err(io::Error::other("injected world-line sync failure"))
    } else {
        file.sync_all()
    };

    #[cfg(test)]
    if let Some(index) = event_index {
        APPEND_IO_STATE.with(|slot| {
            slot.borrow_mut()
                .as_mut()
                .expect("append I/O probe disappeared during sync")
                .snapshot
                .syncs[index]
                .succeeded = result.is_ok();
        });
    }
    result
}

fn truncate_append_tail(file: &File, length: u64) -> io::Result<()> {
    #[cfg(test)]
    let (event_index, injected_failure) = APPEND_IO_STATE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(state) = slot.as_mut() else {
            return (None, false);
        };
        let index = state.snapshot.truncations.len();
        let fail = state.fail_truncate;
        state.snapshot.truncations.push(AppendIoTruncate {
            length,
            succeeded: false,
        });
        (Some(index), fail)
    });
    #[cfg(not(test))]
    let injected_failure = false;

    let result = if injected_failure {
        Err(io::Error::other("injected world-line truncate failure"))
    } else {
        file.set_len(length)
    };

    #[cfg(test)]
    if let Some(index) = event_index {
        APPEND_IO_STATE.with(|slot| {
            slot.borrow_mut()
                .as_mut()
                .expect("append I/O probe disappeared during truncate")
                .snapshot
                .truncations[index]
                .succeeded = result.is_ok();
        });
    }
    result
}

#[cfg(test)]
fn append_test_write_limit(requested: usize) -> io::Result<usize> {
    APPEND_IO_STATE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(state) = slot.as_mut() else {
            return Ok(requested);
        };
        match state.fail_after_write_bytes {
            Some(0) => Err(io::Error::other("injected world-line short write")),
            Some(remaining) => Ok(requested.min(usize::try_from(remaining).unwrap_or(usize::MAX))),
            None => Ok(requested),
        }
    })
}

#[cfg(test)]
fn record_append_test_write(offset: u64, bytes: usize) {
    APPEND_IO_STATE.with(|slot| {
        if let Some(state) = slot.borrow_mut().as_mut() {
            state.snapshot.writes.push(AppendIoWrite { offset, bytes });
            state.snapshot.bytes_written =
                state.snapshot.bytes_written.saturating_add(bytes as u64);
            if let Some(remaining) = &mut state.fail_after_write_bytes {
                *remaining = remaining.saturating_sub(bytes as u64);
            }
        }
    });
}

struct EncodedSegment {
    frame: Vec<u8>,
    ciphertext_len: u32,
    plaintext_len: u32,
    nonce: [u8; 24],
    tag: [u8; 32],
    leaf: [u8; 32],
}

fn encode_segment_record(
    header: &LineHeader,
    epoch: &[u8; 32],
    key: &DEMKey,
    sequence: u64,
    previous_tag: &[u8; 32],
    plaintext: &[u8],
) -> Result<EncodedSegment, LineError> {
    let plaintext_len = u32::try_from(plaintext.len()).map_err(|_| LineError::SegmentTooLarge)?;
    if plaintext_len == 0 {
        return Err(LineError::EmptySegment);
    }
    if plaintext_len > MAX_SEGMENT_BYTES {
        return Err(LineError::SegmentTooLarge);
    }
    let aad = segment_aad(header, epoch, sequence, previous_tag, plaintext_len);
    let sealed = seal_object(key, &aad, plaintext);
    let ciphertext_len =
        u32::try_from(sealed.ciphertext.len()).map_err(|_| LineError::SegmentTooLarge)?;
    let nonce = sealed.nonce;
    let tag = sealed.cmt4;
    let leaf = segment_leaf(
        header.file_id(),
        epoch,
        sequence,
        &nonce,
        ciphertext_len,
        previous_tag,
        &tag,
    );
    let frame = encode_record(RECORD_SEGMENT, sequence, sealed)?;
    Ok(EncodedSegment {
        frame,
        ciphertext_len,
        plaintext_len,
        nonce,
        tag,
        leaf,
    })
}

struct EncodedManifest {
    frame: Vec<u8>,
    ciphertext_len: u32,
    nonce: [u8; 24],
    tag: [u8; 32],
}

fn scrub_manifest_buffer(buf: &mut [u8]) {
    for byte in buf {
        // SAFETY: `byte` is a live mutable reference inside `buf`.
        unsafe { core::ptr::write_volatile(byte, 0) };
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

fn encode_manifest_record(
    header: &LineHeader,
    epoch: &[u8; 32],
    key: &DEMKey,
    sequence: u64,
    previous_witness_tag: &[u8; 32],
    manifest: &LineManifest,
) -> Result<EncodedManifest, LineError> {
    let mut plaintext = encode_manifest_plain(manifest)?;
    let plaintext_len = u32::try_from(plaintext.len()).map_err(|_| {
        scrub_manifest_buffer(&mut plaintext);
        LineError::InvalidManifest("manifest plaintext exceeds maximum record length")
    })?;
    let aad = manifest_aad(header, epoch, sequence, previous_witness_tag, plaintext_len);
    let sealed = seal_object(key, &aad, &plaintext);
    scrub_manifest_buffer(&mut plaintext);
    let ciphertext_len =
        u32::try_from(sealed.ciphertext.len()).map_err(|_| LineError::InvalidRecord)?;
    let nonce = sealed.nonce;
    let tag = sealed.cmt4;
    let frame = encode_record(RECORD_MANIFEST, sequence, sealed)?;
    Ok(EncodedManifest {
        frame,
        ciphertext_len,
        nonce,
        tag,
    })
}

fn encode_manifest_plain(manifest: &LineManifest) -> Result<Vec<u8>, LineError> {
    if manifest.slabs.len() > MAX_MANIFEST_SLABS {
        return Err(LineError::InvalidManifest(
            "manifest exceeds maximum slab count",
        ));
    }
    if manifest.styles.len() > MAX_MANIFEST_STYLES {
        return Err(LineError::InvalidManifest(
            "manifest exceeds maximum style run count",
        ));
    }
    if manifest.links.len() > MAX_MANIFEST_LINKS {
        return Err(LineError::InvalidManifest(
            "manifest exceeds maximum link count",
        ));
    }
    let slab_count = u32::try_from(manifest.slabs.len())
        .map_err(|_| LineError::InvalidManifest("manifest exceeds maximum slab count"))?;

    if manifest.styles.is_empty() && manifest.links.is_empty() {
        let total = MANIFEST_HEADER_BYTES + manifest.slabs.len() * MANIFEST_ENTRY_BYTES;
        let mut out = Vec::new();
        out.try_reserve_exact(total)
            .map_err(|_| LineError::ResourceExhausted)?;
        out.extend_from_slice(&slab_count.to_le_bytes());
        for slab in &manifest.slabs {
            if slab.len == 0 || slab.start.checked_add(slab.len).is_none() {
                return Err(LineError::InvalidManifest(
                    "manifest slab has zero or overflowing byte span",
                ));
            }
            out.extend_from_slice(&slab.segment.to_le_bytes());
            out.extend_from_slice(&slab.start.to_le_bytes());
            out.extend_from_slice(&slab.len.to_le_bytes());
        }
        return Ok(out);
    }

    let style_count = u32::try_from(manifest.styles.len())
        .map_err(|_| LineError::InvalidManifest("manifest exceeds maximum style run count"))?;
    let link_count = u32::try_from(manifest.links.len())
        .map_err(|_| LineError::InvalidManifest("manifest exceeds maximum link count"))?;
    let mut link_bytes_total = 0usize;
    for link in &manifest.links {
        if link.is_empty() || link.len() > MAX_LINK_BYTES {
            return Err(LineError::InvalidManifest(
                "manifest link target must be 1..=4096 UTF-8 bytes",
            ));
        }
        link_bytes_total = link_bytes_total
            .checked_add(2 + link.len())
            .ok_or(LineError::ResourceExhausted)?;
    }
    let total = 4usize
        .checked_add(4)
        .and_then(|n| n.checked_add(manifest.slabs.len().checked_mul(MANIFEST_ENTRY_BYTES)?))
        .and_then(|n| n.checked_add(4))
        .and_then(|n| n.checked_add(manifest.styles.len().checked_mul(MANIFEST_STYLE_BYTES)?))
        .and_then(|n| n.checked_add(4))
        .and_then(|n| n.checked_add(link_bytes_total))
        .ok_or(LineError::ResourceExhausted)?;
    if total > MAX_MANIFEST_BYTES as usize {
        return Err(LineError::InvalidManifest(
            "manifest plaintext exceeds maximum record length",
        ));
    }

    let mut out = Vec::new();
    out.try_reserve_exact(total)
        .map_err(|_| LineError::ResourceExhausted)?;
    out.extend_from_slice(MANIFEST_V2_MAGIC);
    out.extend_from_slice(&slab_count.to_le_bytes());
    for slab in &manifest.slabs {
        if slab.len == 0 || slab.start.checked_add(slab.len).is_none() {
            return Err(LineError::InvalidManifest(
                "manifest slab has zero or overflowing byte span",
            ));
        }
        out.extend_from_slice(&slab.segment.to_le_bytes());
        out.extend_from_slice(&slab.start.to_le_bytes());
        out.extend_from_slice(&slab.len.to_le_bytes());
    }
    out.extend_from_slice(&style_count.to_le_bytes());
    for run in &manifest.styles {
        out.extend_from_slice(&run.start.to_le_bytes());
        out.extend_from_slice(&run.end.to_le_bytes());
        out.push(run.flags);
        out.push(run.family);
        out.push(run.heading);
        out.push(run.alignment);
        out.push(run.list_kind);
        out.push(run.list_depth);
        out.push(run.blockquote);
        out.push(0);
        out.extend_from_slice(&run.size_milli.to_le_bytes());
        out.extend_from_slice(&run.list_start.to_le_bytes());
        out.extend_from_slice(&run.link_id.to_le_bytes());
    }
    out.extend_from_slice(&link_count.to_le_bytes());
    for link in &manifest.links {
        let len = u16::try_from(link.len())
            .map_err(|_| LineError::InvalidManifest("manifest link exceeds u16"))?;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(link.as_bytes());
    }
    Ok(out)
}

fn decode_slabs(
    bytes: &[u8],
    offset: &mut usize,
    count: usize,
) -> Result<Vec<LineSlab>, LineError> {
    if count > MAX_MANIFEST_SLABS {
        return Err(LineError::InvalidRecord);
    }
    let mut slabs = Vec::new();
    slabs
        .try_reserve_exact(count)
        .map_err(|_| LineError::ResourceExhausted)?;
    for _ in 0..count {
        let segment = take_u64(bytes, offset)?;
        let start = u32::from_le_bytes(take_array::<4>(bytes, offset)?);
        let len = u32::from_le_bytes(take_array::<4>(bytes, offset)?);
        if len == 0 || start.checked_add(len).is_none() {
            return Err(LineError::InvalidRecord);
        }
        slabs.push(LineSlab {
            segment,
            start,
            len,
        });
    }
    Ok(slabs)
}

fn decode_manifest_plain(bytes: &[u8]) -> Result<LineManifest, LineError> {
    if bytes.len() < MANIFEST_HEADER_BYTES || bytes.len() > MAX_MANIFEST_BYTES as usize {
        return Err(LineError::InvalidRecord);
    }
    let mut offset = 0usize;
    let first = take_array::<4>(bytes, &mut offset)?;
    if &first != MANIFEST_V2_MAGIC {
        let count = u32::from_le_bytes(first) as usize;
        if count > MAX_MANIFEST_SLABS {
            return Err(LineError::InvalidRecord);
        }
        let expected_len = MANIFEST_HEADER_BYTES
            .checked_add(
                count
                    .checked_mul(MANIFEST_ENTRY_BYTES)
                    .ok_or(LineError::InvalidRecord)?,
            )
            .ok_or(LineError::InvalidRecord)?;
        if bytes.len() != expected_len {
            return Err(LineError::InvalidRecord);
        }
        let slabs = decode_slabs(bytes, &mut offset, count)?;
        return Ok(LineManifest {
            slabs,
            styles: Vec::new(),
            links: Vec::new(),
        });
    }

    let slab_count = u32::from_le_bytes(take_array::<4>(bytes, &mut offset)?) as usize;
    let slabs = decode_slabs(bytes, &mut offset, slab_count)?;

    let style_count = u32::from_le_bytes(take_array::<4>(bytes, &mut offset)?) as usize;
    if style_count > MAX_MANIFEST_STYLES {
        return Err(LineError::InvalidRecord);
    }
    let mut styles = Vec::new();
    styles
        .try_reserve_exact(style_count)
        .map_err(|_| LineError::ResourceExhausted)?;
    for _ in 0..style_count {
        let start = take_u64(bytes, &mut offset)?;
        let end = take_u64(bytes, &mut offset)?;
        let meta = take_array::<8>(bytes, &mut offset)?;
        if meta[7] != 0 {
            return Err(LineError::InvalidRecord);
        }
        let size_milli = u32::from_le_bytes(take_array::<4>(bytes, &mut offset)?);
        let list_start = u32::from_le_bytes(take_array::<4>(bytes, &mut offset)?);
        let link_id = u32::from_le_bytes(take_array::<4>(bytes, &mut offset)?);
        styles.push(LineStyleRun {
            start,
            end,
            flags: meta[0],
            family: meta[1],
            heading: meta[2],
            alignment: meta[3],
            list_kind: meta[4],
            list_depth: meta[5],
            blockquote: meta[6],
            size_milli,
            list_start,
            link_id,
        });
    }

    let link_count = u32::from_le_bytes(take_array::<4>(bytes, &mut offset)?) as usize;
    if link_count > MAX_MANIFEST_LINKS {
        return Err(LineError::InvalidRecord);
    }
    if style_count == 0 && link_count == 0 {
        return Err(LineError::InvalidRecord);
    }
    let mut links = Vec::new();
    links
        .try_reserve_exact(link_count)
        .map_err(|_| LineError::ResourceExhausted)?;
    for _ in 0..link_count {
        let len = u16::from_le_bytes(take_array::<2>(bytes, &mut offset)?) as usize;
        if len == 0 || len > MAX_LINK_BYTES {
            return Err(LineError::InvalidRecord);
        }
        let end = offset.checked_add(len).ok_or(LineError::InvalidRecord)?;
        let raw = bytes.get(offset..end).ok_or(LineError::InvalidRecord)?;
        let text = core::str::from_utf8(raw).map_err(|_| LineError::InvalidRecord)?;
        offset = end;
        links.push(text.to_owned());
    }
    if offset != bytes.len() {
        return Err(LineError::InvalidRecord);
    }
    Ok(LineManifest {
        slabs,
        styles,
        links,
    })
}

/// Owned streaming bounded-memory append transaction started via
/// [`WorldLine::start_streaming_tx`] (Block 7 dense replacement & Block 10 streaming import).
///
/// Writes encrypted `<= 64 KiB` segments directly to the log one at a time and commits all
/// segments plus an optional manifest under one witness via [`WorldLine::finish_streaming_tx`].
pub struct StreamingTx {
    header: LineHeader,
    epoch: [u8; 32],
    key: DEMKey,
    file: File,
    _append_lock: FileAppendLock,
    mmr: Mmr,
    last_segment_tag: [u8; 32],
    next_segment_seq: u64,
    new_descriptors: Vec<SegmentDescriptor>,
    bytes_written: u64,
    committed_end: u64,
    failed: bool,
}

impl StreamingTx {
    /// Returns the zero-based segment sequence number that the next [`Self::push_segment`] call
    /// will assign.
    #[must_use]
    pub fn next_segment_sequence(&self) -> u64 {
        self.next_segment_seq
    }

    /// Seals and writes one non-empty `<= 64 KiB` plaintext segment immediately to the log,
    /// returning its full-length [`LineSlab`] handle without retaining the plaintext in memory.
    pub fn push_segment(&mut self, plaintext: &[u8]) -> Result<LineSlab, LineError> {
        if self.failed {
            return Err(LineError::ReopenRequired);
        }
        if plaintext.is_empty() {
            return Err(LineError::EmptySegment);
        }
        if plaintext.len() > MAX_SEGMENT_BYTES as usize {
            return Err(LineError::SegmentTooLarge);
        }
        let _next_total = self
            .mmr
            .count()
            .checked_add(1)
            .ok_or(LineError::TooManySegments)?;

        let res = (|| -> Result<LineSlab, LineError> {
            let sequence = self.next_segment_seq;
            let encoded = encode_segment_record(
                &self.header,
                &self.epoch,
                &self.key,
                sequence,
                &self.last_segment_tag,
                plaintext,
            )?;
            let record_offset = self.file.stream_position()?;
            write_append_bytes(&mut self.file, &encoded.frame)?;
            self.bytes_written = self
                .bytes_written
                .checked_add(encoded.frame.len() as u64)
                .ok_or(LineError::ResourceExhausted)?;
            let ciphertext_offset = record_offset
                .checked_add(RECORD_HEADER_BYTES as u64 + RECORD_FIXED_BYTES as u64)
                .ok_or(LineError::ResourceExhausted)?;
            self.new_descriptors
                .try_reserve(1)
                .map_err(|_| LineError::ResourceExhausted)?;
            self.new_descriptors.push(SegmentDescriptor {
                sequence,
                ciphertext_offset,
                ciphertext_len: encoded.ciphertext_len,
                plaintext_len: encoded.plaintext_len,
                nonce: encoded.nonce,
                tag: encoded.tag,
                previous_tag: self.last_segment_tag,
            });
            self.mmr.append(encoded.leaf)?;
            self.last_segment_tag = encoded.tag;
            self.next_segment_seq = sequence
                .checked_add(1)
                .ok_or(LineError::TooManySegments)?;
            Ok(LineSlab {
                segment: sequence,
                start: 0,
                len: encoded.plaintext_len,
            })
        })();
        if res.is_err() {
            self.failed = true;
        }
        res
    }
}

/// Borrowed streaming bounded-memory append transaction over `&mut WorldLine`.
pub struct StreamingAppend<'a> {
    line: &'a mut WorldLine,
    tx: Option<StreamingTx>,
}

impl<'a> StreamingAppend<'a> {
    /// Returns the zero-based segment sequence number that the next [`Self::push_segment`] call
    /// will assign.
    #[must_use]
    pub fn next_segment_sequence(&self) -> u64 {
        self.tx.as_ref().map_or(0, StreamingTx::next_segment_sequence)
    }

    /// Seals and writes one non-empty `<= 64 KiB` plaintext segment immediately to the log,
    /// returning its full-length [`LineSlab`] handle without retaining the plaintext in memory.
    pub fn push_segment(&mut self, plaintext: &[u8]) -> Result<LineSlab, LineError> {
        self.tx
            .as_mut()
            .ok_or(LineError::ReopenRequired)?
            .push_segment(plaintext)
    }

    /// Commits all streamed segments and an optional explicit [`LineManifest`] under a single
    /// durable witness commit point.
    pub fn finish(
        mut self,
        explicit_manifest: Option<&LineManifest>,
    ) -> Result<AppendReceipt, LineError> {
        let tx = self.tx.take().ok_or(LineError::ReopenRequired)?;
        self.line.finish_streaming_tx(tx, explicit_manifest)
    }
}

impl Drop for StreamingAppend<'_> {
    fn drop(&mut self) {
        if let Some(tx) = self.tx.take() {
            let _ = self.line.abort_streaming_tx(tx);
        }
    }
}

fn validate_manifest_against_pending(
    committed: &[SegmentDescriptor],
    pending: &[&[u8]],
    manifest: &LineManifest,
) -> Result<(), LineError> {
    let mut pending_lengths = Vec::with_capacity(pending.len());
    for slice in pending {
        let len = u32::try_from(slice.len()).map_err(|_| LineError::SegmentTooLarge)?;
        pending_lengths.push(len);
    }
    validate_manifest_against_lengths(committed, &pending_lengths, manifest)
}

fn validate_manifest_against_lengths(
    committed: &[SegmentDescriptor],
    pending_lengths: &[u32],
    manifest: &LineManifest,
) -> Result<(), LineError> {
    if manifest.slabs.len() > MAX_MANIFEST_SLABS {
        return Err(LineError::InvalidManifest(
            "manifest exceeds maximum slab count",
        ));
    }
    if manifest.styles.len() > MAX_MANIFEST_STYLES {
        return Err(LineError::InvalidManifest(
            "manifest exceeds maximum style run count",
        ));
    }
    if manifest.links.len() > MAX_MANIFEST_LINKS {
        return Err(LineError::InvalidManifest(
            "manifest exceeds maximum link count",
        ));
    }
    for link in &manifest.links {
        if link.is_empty() || link.len() > MAX_LINK_BYTES {
            return Err(LineError::InvalidManifest(
                "manifest link target must be 1..=4096 UTF-8 bytes",
            ));
        }
    }
    let total_segments = committed
        .len()
        .checked_add(pending_lengths.len())
        .ok_or(LineError::TooManySegments)?;
    let mut total_doc_bytes = 0u64;
    for slab in &manifest.slabs {
        if slab.len == 0 {
            return Err(LineError::InvalidManifest("manifest slab has zero length"));
        }
        let end = slab
            .start
            .checked_add(slab.len)
            .ok_or(LineError::InvalidManifest(
                "manifest slab byte range overflows u32",
            ))?;
        let index = usize::try_from(slab.segment)
            .map_err(|_| LineError::InvalidManifest("manifest slab names out-of-range segment"))?;
        if index >= total_segments {
            return Err(LineError::InvalidManifest(
                "manifest slab names uncommitted segment",
            ));
        }
        let seg_len = if index < committed.len() {
            committed[index].plaintext_len
        } else {
            pending_lengths[index - committed.len()]
        };
        if end > seg_len {
            return Err(LineError::InvalidManifest(
                "manifest slab exceeds segment plaintext length",
            ));
        }
        total_doc_bytes =
            total_doc_bytes
                .checked_add(u64::from(slab.len))
                .ok_or(LineError::InvalidManifest(
                    "manifest total document length overflows u64",
                ))?;
    }

    let mut prev_end = 0u64;
    for run in &manifest.styles {
        if run.start >= run.end || run.end > total_doc_bytes || run.start < prev_end {
            return Err(LineError::InvalidManifest(
                "manifest style run is empty, out of bounds, or overlapping",
            ));
        }
        if (run.flags & !0x3F) != 0
            || run.family > 2
            || run.heading > 4
            || run.alignment > 3
            || run.list_kind > 2
            || run.list_depth > 8
            || run.blockquote > 8
            || run.size_milli == 0
            || (run.link_id as usize) > manifest.links.len()
            || (run.list_kind == 0 && (run.list_depth != 0 || run.list_start != 0))
            || (run.list_kind == 2 && run.list_start == 0)
        {
            return Err(LineError::InvalidManifest(
                "manifest style run carries invalid attribute values",
            ));
        }
        prev_end = run.end;
    }
    Ok(())
}

fn read_manifest_from<R: Read + Seek>(
    reader: &mut R,
    header: &LineHeader,
    epoch: &[u8; 32],
    key: &DEMKey,
    descriptor: &ManifestDescriptor,
    allowed_segments: &[SegmentDescriptor],
) -> Result<LineManifest, LineError> {
    if descriptor.ciphertext_len < AEAD_TAG_BYTES + MANIFEST_HEADER_BYTES as u32
        || descriptor.ciphertext_len > MAX_MANIFEST_CIPHERTEXT_BYTES
    {
        return Err(LineError::InvalidRecord);
    }
    let plaintext_len = descriptor.ciphertext_len - AEAD_TAG_BYTES;
    reader.seek(SeekFrom::Start(descriptor.ciphertext_offset))?;
    let mut ciphertext = Vec::new();
    ciphertext
        .try_reserve_exact(descriptor.ciphertext_len as usize)
        .map_err(|_| LineError::ResourceExhausted)?;
    ciphertext.resize(descriptor.ciphertext_len as usize, 0);
    reader.read_exact(&mut ciphertext)?;
    let sealed = SealedObject {
        nonce: descriptor.nonce,
        ciphertext,
        cmt4: descriptor.tag,
    };
    let aad = manifest_aad(
        header,
        epoch,
        descriptor.sequence,
        &descriptor.previous_witness_tag,
        plaintext_len,
    );
    let plaintext =
        open_object(key, &aad, &sealed).map_err(|_| LineError::AuthenticationRejected)?;
    if plaintext.len() != plaintext_len as usize {
        return Err(LineError::AuthenticationRejected);
    }
    let manifest =
        decode_manifest_plain(&plaintext).map_err(|_| LineError::AuthenticationRejected)?;
    validate_manifest_against_pending(allowed_segments, &[], &manifest)
        .map_err(|_| LineError::AuthenticationRejected)?;
    Ok(manifest)
}

// All eight fields are the authenticated witness payload; a wrapper struct would only restate
// them at every call site.
#[allow(clippy::too_many_arguments)]
fn encode_witness_record(
    header: &LineHeader,
    epoch: &[u8; 32],
    key: &DEMKey,
    sequence: u64,
    previous_witness_tag: &[u8; 32],
    last_segment_tag: &[u8; 32],
    mmr: &Mmr,
    manifest_ref: Option<ManifestRef>,
) -> Result<(Vec<u8>, [u8; 32]), LineError> {
    let plaintext = encode_witness_plain(
        sequence,
        previous_witness_tag,
        last_segment_tag,
        epoch,
        mmr,
        manifest_ref,
    );
    let aad = witness_aad(header, epoch, sequence, previous_witness_tag);
    let sealed = seal_object(key, &aad, &plaintext);
    let tag = sealed.cmt4;
    Ok((encode_record(RECORD_WITNESS, sequence, sealed)?, tag))
}

/// Encode a witness payload.
///
/// Version 1 (`manifest_ref: None`) is kept byte-for-byte identical for world-lines that have
/// only ever been appended to. Version 2 appends a fixed 44-byte authenticated reference
/// (`sequence`, `ciphertext_len`, `cmt4` tag) to the active [`RECORD_MANIFEST`] record after the
/// MMR peaks, keeping every witness `O(log segments)` in size while committing to arbitrary
/// slab-span geometry and deletions (AUDIT OQ-29, OQ-30).
fn encode_witness_plain(
    sequence: u64,
    previous_witness_tag: &[u8; 32],
    last_segment_tag: &[u8; 32],
    epoch: &[u8; 32],
    mmr: &Mmr,
    manifest_ref: Option<ManifestRef>,
) -> Vec<u8> {
    let extra = if manifest_ref.is_some() {
        WITNESS_MANIFEST_REF_BYTES
    } else {
        0
    };
    let mut out = Vec::with_capacity(WITNESS_BASE_BYTES + 64 * 32 + extra);
    out.push(if manifest_ref.is_none() { 1 } else { 2 });
    out.extend_from_slice(&sequence.to_le_bytes());
    out.extend_from_slice(&mmr.count().to_le_bytes());
    out.extend_from_slice(previous_witness_tag);
    out.extend_from_slice(last_segment_tag);
    out.extend_from_slice(epoch);
    out.extend_from_slice(&mmr.root());
    mmr.encode_peaks(&mut out);
    if let Some(mref) = manifest_ref {
        out.extend_from_slice(&mref.sequence.to_le_bytes());
        out.extend_from_slice(&mref.ciphertext_len.to_le_bytes());
        out.extend_from_slice(&mref.tag);
    }
    out
}

struct DecodedWitness {
    sequence: u64,
    segment_count: u64,
    previous_witness_tag: [u8; 32],
    last_segment_tag: [u8; 32],
    epoch: [u8; 32],
    mmr: Mmr,
    manifest_ref: Option<ManifestRef>,
}

fn decode_witness_plain(bytes: &[u8]) -> Result<DecodedWitness, LineError> {
    if bytes.len() < WITNESS_BASE_BYTES || (bytes[0] != 1 && bytes[0] != 2) {
        return Err(LineError::InvalidRecord);
    }
    let mut offset = 1;
    let sequence = take_u64(bytes, &mut offset)?;
    let segment_count = take_u64(bytes, &mut offset)?;
    let previous_witness_tag = take_array::<32>(bytes, &mut offset)?;
    let last_segment_tag = take_array::<32>(bytes, &mut offset)?;
    let epoch = take_array::<32>(bytes, &mut offset)?;
    let root = take_array::<32>(bytes, &mut offset)?;
    let peaks_len = segment_count.count_ones() as usize * 32;
    let peaks_end = offset
        .checked_add(peaks_len)
        .ok_or(LineError::InvalidRecord)?;
    let peaks_bytes = bytes
        .get(offset..peaks_end)
        .ok_or(LineError::InvalidRecord)?;
    let peaks = Mmr::from_peak_bytes(segment_count, peaks_bytes)?;
    if peaks.root() != root {
        return Err(LineError::InvalidRecord);
    }
    offset = peaks_end;

    let manifest_ref = if bytes[0] == 2 {
        let m_seq = take_u64(bytes, &mut offset)?;
        let m_len = u32::from_le_bytes(take_array::<4>(bytes, &mut offset)?);
        let m_tag = take_array::<32>(bytes, &mut offset)?;
        Some(ManifestRef {
            sequence: m_seq,
            ciphertext_len: m_len,
            tag: m_tag,
        })
    } else {
        None
    };
    if offset != bytes.len() {
        return Err(LineError::InvalidRecord);
    }
    Ok(DecodedWitness {
        sequence,
        segment_count,
        previous_witness_tag,
        last_segment_tag,
        epoch,
        mmr: peaks,
        manifest_ref,
    })
}

fn segment_aad(
    header: &LineHeader,
    epoch: &[u8; 32],
    sequence: u64,
    previous_tag: &[u8; 32],
    plaintext_len: u32,
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(20 + 16 + 32 + 8 + 32 + 4);
    aad.extend_from_slice(b"HOLONOMY_V6_SEGMENT\0");
    aad.extend_from_slice(header.file_id());
    aad.extend_from_slice(epoch);
    aad.extend_from_slice(&sequence.to_le_bytes());
    aad.extend_from_slice(previous_tag);
    aad.extend_from_slice(&plaintext_len.to_le_bytes());
    aad
}

fn manifest_aad(
    header: &LineHeader,
    epoch: &[u8; 32],
    sequence: u64,
    previous_witness_tag: &[u8; 32],
    plaintext_len: u32,
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(21 + 16 + 32 + 8 + 32 + 4);
    aad.extend_from_slice(b"HOLONOMY_V6_MANIFEST\0");
    aad.extend_from_slice(header.file_id());
    aad.extend_from_slice(epoch);
    aad.extend_from_slice(&sequence.to_le_bytes());
    aad.extend_from_slice(previous_witness_tag);
    aad.extend_from_slice(&plaintext_len.to_le_bytes());
    aad
}

fn witness_aad(
    header: &LineHeader,
    epoch: &[u8; 32],
    sequence: u64,
    previous_witness_tag: &[u8; 32],
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(20 + 16 + 32 + 8 + 32);
    aad.extend_from_slice(b"HOLONOMY_V6_WITNESS\0");
    aad.extend_from_slice(header.file_id());
    aad.extend_from_slice(epoch);
    aad.extend_from_slice(&sequence.to_le_bytes());
    aad.extend_from_slice(previous_witness_tag);
    aad
}

fn bookmark_aad(
    header: &LineHeader,
    epoch: &[u8; 32],
    sequence: u64,
    last_witness_tag: &[u8; 32],
    previous_bookmark_tag: &[u8; 32],
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(21 + 16 + 32 + 8 + 32 + 32);
    aad.extend_from_slice(b"HOLONOMY_V6_BOOKMARK\0");
    aad.extend_from_slice(header.file_id());
    aad.extend_from_slice(epoch);
    aad.extend_from_slice(&sequence.to_le_bytes());
    aad.extend_from_slice(last_witness_tag);
    aad.extend_from_slice(previous_bookmark_tag);
    aad
}

struct EncodedBookmark {
    frame: Vec<u8>,
    tag: [u8; 32],
}

#[allow(clippy::too_many_arguments)]
fn encode_bookmark_record(
    header: &LineHeader,
    epoch: &[u8; 32],
    key: &DEMKey,
    sequence: u64,
    last_witness_tag: &[u8; 32],
    previous_bookmark_tag: &[u8; 32],
    witness_sequence: u64,
    timestamp_secs: i64,
    name: &str,
) -> Result<EncodedBookmark, LineError> {
    let name_len = u16::try_from(name.len())
        .map_err(|_| LineError::InvalidBookmark("bookmark name exceeds u16"))?;
    if name.is_empty() || name.len() > MAX_BOOKMARK_NAME_BYTES {
        return Err(LineError::InvalidBookmark(
            "bookmark name must be 1..=256 UTF-8 bytes",
        ));
    }
    let mut plaintext = Vec::with_capacity(1 + 8 + 8 + 2 + name.len());
    plaintext.push(1);
    plaintext.extend_from_slice(&witness_sequence.to_le_bytes());
    plaintext.extend_from_slice(&timestamp_secs.to_le_bytes());
    plaintext.extend_from_slice(&name_len.to_le_bytes());
    plaintext.extend_from_slice(name.as_bytes());
    let aad = bookmark_aad(header, epoch, sequence, last_witness_tag, previous_bookmark_tag);
    let sealed = seal_object(key, &aad, &plaintext);
    scrub_manifest_buffer(&mut plaintext);
    let tag = sealed.cmt4;
    let frame = encode_record(RECORD_BOOKMARK, sequence, sealed)?;
    Ok(EncodedBookmark { frame, tag })
}

fn decode_bookmark_plain(
    sequence: u64,
    bytes: &[u8],
    next_witness_sequence: u64,
) -> Result<Bookmark, LineError> {
    if bytes.len() < BOOKMARK_MIN_BYTES || bytes.len() > MAX_BOOKMARK_BYTES || bytes[0] != 1 {
        return Err(LineError::InvalidRecord);
    }
    let mut offset = 1;
    let witness_sequence = take_u64(bytes, &mut offset)?;
    if witness_sequence >= next_witness_sequence {
        return Err(LineError::InvalidRecord);
    }
    let timestamp_secs = i64::from_le_bytes(take_array::<8>(bytes, &mut offset)?);
    let name_len = u16::from_le_bytes(take_array::<2>(bytes, &mut offset)?) as usize;
    if name_len == 0 || name_len > MAX_BOOKMARK_NAME_BYTES {
        return Err(LineError::InvalidRecord);
    }
    let end = offset.checked_add(name_len).ok_or(LineError::InvalidRecord)?;
    let raw = bytes.get(offset..end).ok_or(LineError::InvalidRecord)?;
    if end != bytes.len() {
        return Err(LineError::InvalidRecord);
    }
    let name = core::str::from_utf8(raw)
        .map_err(|_| LineError::InvalidRecord)?
        .to_owned();
    Ok(Bookmark {
        sequence,
        witness_sequence,
        timestamp_secs,
        name,
    })
}

fn encode_record(kind: u8, sequence: u64, sealed: SealedObject) -> Result<Vec<u8>, LineError> {
    let ciphertext_len =
        u32::try_from(sealed.ciphertext.len()).map_err(|_| LineError::InvalidRecord)?;
    let body_len = (RECORD_FIXED_BYTES as u32)
        .checked_add(ciphertext_len)
        .ok_or(LineError::InvalidRecord)?;
    let total_len = RECORD_HEADER_BYTES
        .checked_add(body_len as usize)
        .ok_or(LineError::InvalidRecord)?;
    let mut frame = Vec::with_capacity(total_len);
    frame.extend_from_slice(RECORD_MAGIC);
    frame.push(kind);
    frame.extend_from_slice(&[0u8; 3]);
    frame.extend_from_slice(&body_len.to_le_bytes());
    frame.extend_from_slice(&sequence.to_le_bytes());
    frame.extend_from_slice(&sealed.nonce);
    frame.extend_from_slice(&ciphertext_len.to_le_bytes());
    frame.extend_from_slice(&sealed.cmt4);
    frame.extend_from_slice(&sealed.ciphertext);
    Ok(frame)
}

struct ScanResult {
    segments: Vec<SegmentDescriptor>,
    manifests: Vec<ManifestDescriptor>,
    witnesses: Vec<WitnessSummary>,
    bookmarks: Vec<Bookmark>,
    manifest: Option<LineManifest>,
    mmr: Mmr,
    next_witness_sequence: u64,
    last_witness_tag: [u8; 32],
    last_segment_tag: [u8; 32],
    last_bookmark_tag: [u8; 32],
    committed_end: u64,
    observed_len: u64,
    recovery: RecoveryInfo,
}

#[derive(Clone, Copy)]
struct Checkpoint {
    committed_segment_count: usize,
    committed_manifest_count: usize,
    committed_witness_count: usize,
    committed_bookmark_count: usize,
    active_manifest: Option<usize>,
    mmr: MmrCheckpoint,
    next_witness_sequence: u64,
    last_witness_tag: [u8; 32],
    last_segment_tag: [u8; 32],
    last_bookmark_tag: [u8; 32],
    committed_end: u64,
}

#[derive(Clone, Copy)]
struct MmrCheckpoint {
    count: u64,
    root: [u8; 32],
    peaks: [[u8; 32]; 64],
    occupied: u64,
}

impl MmrCheckpoint {
    fn from_mmr(mmr: &Mmr) -> Self {
        let mut peaks = [[0u8; 32]; 64];
        let mut occupied = 0u64;
        let mut encoded = Vec::new();
        mmr.encode_peaks(&mut encoded);
        let mut offset = 0;
        for (height, peak) in peaks.iter_mut().enumerate() {
            if mmr.count() & (1u64 << height) != 0 {
                peak.copy_from_slice(&encoded[offset..offset + 32]);
                occupied |= 1u64 << height;
                offset += 32;
            }
        }
        Self {
            count: mmr.count(),
            root: mmr.root(),
            peaks,
            occupied,
        }
    }

    fn to_mmr(self) -> Result<Mmr, LineError> {
        let mut encoded = Vec::with_capacity(self.occupied.count_ones() as usize * 32);
        for (height, peak) in self.peaks.iter().enumerate() {
            if self.occupied & (1u64 << height) != 0 {
                encoded.extend_from_slice(peak);
            }
        }
        let mmr = Mmr::from_peak_bytes(self.count, &encoded)?;
        if mmr.root() != self.root {
            return Err(LineError::InvalidRecord);
        }
        Ok(mmr)
    }
}

fn scan_records<R: Read + Seek>(
    reader: &mut R,
    header: &LineHeader,
    epoch: &[u8; 32],
    key: &DEMKey,
) -> Result<ScanResult, LineError> {
    let observed_len = reader.seek(SeekFrom::End(0))?;
    if observed_len < HEADER_BYTES as u64 {
        return Err(LineError::InvalidHeader);
    }
    reader.seek(SeekFrom::Start(HEADER_BYTES as u64))?;

    let mut segments = Vec::new();
    let mut manifests = Vec::new();
    let mut witnesses = Vec::new();
    let mut bookmarks = Vec::new();
    let mut mmr = Mmr::new();
    let mut scanned_last_segment_tag = [0u8; 32];
    let mut next_witness_sequence = 0u64;
    let mut last_witness_tag = [0u8; 32];
    let mut last_bookmark_tag = [0u8; 32];
    let mut checkpoint: Option<Checkpoint> = None;

    loop {
        let record_start = reader.stream_position()?;
        if record_start == observed_len {
            break;
        }
        let remaining = observed_len.saturating_sub(record_start);
        if remaining < RECORD_HEADER_BYTES as u64 {
            break; // A torn frame header can only be an uncommitted physical tail.
        }

        let mut frame_header = [0u8; RECORD_HEADER_BYTES];
        reader.read_exact(&mut frame_header)?;
        if &frame_header[..4] != RECORD_MAGIC
            || frame_header[5] != 0
            || frame_header[6] != 0
            || frame_header[7] != 0
        {
            return Err(LineError::InvalidRecord);
        }
        let kind = frame_header[4];
        if kind != RECORD_SEGMENT
            && kind != RECORD_WITNESS
            && kind != RECORD_MANIFEST
            && kind != RECORD_BOOKMARK
        {
            return Err(LineError::InvalidRecord);
        }
        let body_len = u32::from_le_bytes(frame_header[8..12].try_into().unwrap());
        if body_len < RECORD_FIXED_BYTES as u32 {
            return Err(LineError::InvalidRecord);
        }
        let maximum = match kind {
            RECORD_SEGMENT => RECORD_FIXED_BYTES as u32 + MAX_SEGMENT_BYTES + AEAD_TAG_BYTES,
            RECORD_WITNESS => RECORD_FIXED_BYTES as u32 + MAX_WITNESS_CIPHERTEXT_BYTES,
            RECORD_MANIFEST => RECORD_FIXED_BYTES as u32 + MAX_MANIFEST_CIPHERTEXT_BYTES,
            RECORD_BOOKMARK => RECORD_FIXED_BYTES as u32 + MAX_BOOKMARK_CIPHERTEXT_BYTES,
            _ => unreachable!(),
        };
        if body_len > maximum {
            return Err(LineError::InvalidRecord);
        }
        let record_end = record_start
            .checked_add(RECORD_HEADER_BYTES as u64)
            .and_then(|offset| offset.checked_add(body_len as u64))
            .ok_or(LineError::InvalidRecord)?;
        if record_end > observed_len {
            break; // Incomplete record at EOF: the prior complete witness remains authoritative.
        }

        let mut fixed = [0u8; RECORD_FIXED_BYTES];
        reader.read_exact(&mut fixed)?;
        let sequence = u64::from_le_bytes(fixed[..8].try_into().unwrap());
        let nonce: [u8; 24] = fixed[8..32].try_into().unwrap();
        let ciphertext_len = u32::from_le_bytes(fixed[32..36].try_into().unwrap());
        let tag: [u8; 32] = fixed[36..68].try_into().unwrap();
        let expected_body_len = (RECORD_FIXED_BYTES as u32)
            .checked_add(ciphertext_len)
            .ok_or(LineError::InvalidRecord)?;
        if body_len != expected_body_len {
            return Err(LineError::InvalidRecord);
        }
        let ciphertext_offset = reader.stream_position()?;

        if kind == RECORD_SEGMENT {
            if checkpoint.is_none() {
                return Err(LineError::InvalidRecord);
            }
            if ciphertext_len <= AEAD_TAG_BYTES
                || ciphertext_len > MAX_SEGMENT_BYTES + AEAD_TAG_BYTES
                || sequence != mmr.count()
            {
                return Err(LineError::InvalidRecord);
            }
            let plaintext_len = ciphertext_len - AEAD_TAG_BYTES;
            let leaf = segment_leaf(
                header.file_id(),
                epoch,
                sequence,
                &nonce,
                ciphertext_len,
                &scanned_last_segment_tag,
                &tag,
            );
            segments
                .try_reserve(1)
                .map_err(|_| LineError::ResourceExhausted)?;
            segments.push(SegmentDescriptor {
                sequence,
                ciphertext_offset,
                ciphertext_len,
                plaintext_len,
                nonce,
                tag,
                previous_tag: scanned_last_segment_tag,
            });
            mmr.append(leaf)?;
            scanned_last_segment_tag = tag;
            reader.seek(SeekFrom::Current(i64::from(ciphertext_len)))?;
        } else if kind == RECORD_MANIFEST {
            let Some(prev) = checkpoint else {
                return Err(LineError::InvalidRecord);
            };
            if manifests.len() != prev.committed_manifest_count
                || ciphertext_len < AEAD_TAG_BYTES + MANIFEST_HEADER_BYTES as u32
                || ciphertext_len > MAX_MANIFEST_CIPHERTEXT_BYTES
                || sequence != manifests.len() as u64
            {
                return Err(LineError::InvalidRecord);
            }
            manifests
                .try_reserve(1)
                .map_err(|_| LineError::ResourceExhausted)?;
            manifests.push(ManifestDescriptor {
                sequence,
                segment_limit: segments.len(),
                ciphertext_offset,
                ciphertext_len,
                nonce,
                tag,
                previous_witness_tag: last_witness_tag,
            });
            // Seek past historical manifests during the scan; only the active manifest named by
            // the final witness is opened below.
            reader.seek(SeekFrom::Current(i64::from(ciphertext_len)))?;
        } else if kind == RECORD_BOOKMARK {
            let Some(prev) = checkpoint else {
                return Err(LineError::InvalidRecord);
            };
            if segments.len() != prev.committed_segment_count
                || manifests.len() != prev.committed_manifest_count
                || bookmarks.len() >= MAX_BOOKMARKS
                || sequence != bookmarks.len() as u64
                || ciphertext_len < AEAD_TAG_BYTES + BOOKMARK_MIN_BYTES as u32
                || ciphertext_len > MAX_BOOKMARK_CIPHERTEXT_BYTES
            {
                return Err(LineError::InvalidRecord);
            }
            let mut ciphertext = vec![0u8; ciphertext_len as usize];
            reader.read_exact(&mut ciphertext)?;
            let sealed = SealedObject {
                nonce,
                ciphertext,
                cmt4: tag,
            };
            let aad = bookmark_aad(header, epoch, sequence, &last_witness_tag, &last_bookmark_tag);
            let plaintext =
                open_object(key, &aad, &sealed).map_err(|_| LineError::AuthenticationRejected)?;
            let bookmark = decode_bookmark_plain(sequence, &plaintext, next_witness_sequence)
                .map_err(|_| LineError::AuthenticationRejected)?;
            bookmarks
                .try_reserve(1)
                .map_err(|_| LineError::ResourceExhausted)?;
            bookmarks.push(bookmark);
            last_bookmark_tag = tag;
            checkpoint = Some(Checkpoint {
                committed_segment_count: prev.committed_segment_count,
                committed_manifest_count: prev.committed_manifest_count,
                committed_witness_count: prev.committed_witness_count,
                committed_bookmark_count: bookmarks.len(),
                active_manifest: prev.active_manifest,
                mmr: prev.mmr,
                next_witness_sequence: prev.next_witness_sequence,
                last_witness_tag: prev.last_witness_tag,
                last_segment_tag: prev.last_segment_tag,
                last_bookmark_tag,
                committed_end: record_end,
            });
        } else {
            if ciphertext_len < AEAD_TAG_BYTES + WITNESS_BASE_BYTES as u32
                || ciphertext_len > MAX_WITNESS_CIPHERTEXT_BYTES
                || sequence != next_witness_sequence
            {
                return Err(LineError::InvalidRecord);
            }
            let mut ciphertext = vec![0u8; ciphertext_len as usize];
            reader.read_exact(&mut ciphertext)?;
            let sealed = SealedObject {
                nonce,
                ciphertext,
                cmt4: tag,
            };
            let aad = witness_aad(header, epoch, sequence, &last_witness_tag);
            let plaintext =
                open_object(key, &aad, &sealed).map_err(|_| LineError::AuthenticationRejected)?;
            let witness =
                decode_witness_plain(&plaintext).map_err(|_| LineError::AuthenticationRejected)?;
            if witness.sequence != sequence
                || witness.segment_count != mmr.count()
                || witness.previous_witness_tag != last_witness_tag
                || witness.last_segment_tag != scanned_last_segment_tag
                || witness.epoch != *epoch
                || witness.mmr != mmr
                || (sequence == 0
                    && (witness.segment_count != 0
                        || witness.previous_witness_tag != [0u8; 32]
                        || witness.last_segment_tag != [0u8; 32]
                        || witness.manifest_ref.is_some()))
            {
                return Err(LineError::AuthenticationRejected);
            }
            let prev_manifest_count = checkpoint.map_or(0, |c| c.committed_manifest_count);
            let prev_active = checkpoint.and_then(|c| c.active_manifest);
            let active_manifest = match witness.manifest_ref {
                None => {
                    if manifests.len() != prev_manifest_count || prev_active.is_some() {
                        return Err(LineError::AuthenticationRejected);
                    }
                    None
                }
                Some(mref) => {
                    let index = usize::try_from(mref.sequence)
                        .map_err(|_| LineError::AuthenticationRejected)?;
                    let wrote_new =
                        manifests.len() == prev_manifest_count + 1 && index == prev_manifest_count;
                    let kept_prev =
                        manifests.len() == prev_manifest_count && prev_active == Some(index);
                    if !wrote_new && !kept_prev {
                        return Err(LineError::AuthenticationRejected);
                    }
                    let descriptor = manifests
                        .get(index)
                        .ok_or(LineError::AuthenticationRejected)?;
                    if descriptor.ciphertext_len != mref.ciphertext_len
                        || descriptor.tag != mref.tag
                    {
                        return Err(LineError::AuthenticationRejected);
                    }
                    Some(index)
                }
            };
            witnesses
                .try_reserve(1)
                .map_err(|_| LineError::ResourceExhausted)?;
            witnesses.push(WitnessSummary {
                sequence,
                segment_count: witness.segment_count,
                manifest_sequence: witness.manifest_ref.map(|mref| mref.sequence),
            });
            last_witness_tag = tag;
            next_witness_sequence = sequence.checked_add(1).ok_or(LineError::TooManySegments)?;
            checkpoint = Some(Checkpoint {
                committed_segment_count: segments.len(),
                committed_manifest_count: manifests.len(),
                committed_witness_count: witnesses.len(),
                committed_bookmark_count: bookmarks.len(),
                active_manifest,
                mmr: MmrCheckpoint::from_mmr(&mmr),
                next_witness_sequence,
                last_witness_tag,
                last_segment_tag: scanned_last_segment_tag,
                last_bookmark_tag,
                committed_end: record_end,
            });
        }

        if reader.stream_position()? != record_end {
            return Err(LineError::InvalidRecord);
        }
    }

    let checkpoint = checkpoint.ok_or(LineError::NoCommittedWitness)?;
    segments.truncate(checkpoint.committed_segment_count);
    manifests.truncate(checkpoint.committed_manifest_count);
    witnesses.truncate(checkpoint.committed_witness_count);
    bookmarks.truncate(checkpoint.committed_bookmark_count);
    let manifest = if let Some(index) = checkpoint.active_manifest {
        let descriptor = manifests.get(index).ok_or(LineError::InvalidRecord)?;
        let allowed = segments
            .get(..descriptor.segment_limit)
            .ok_or(LineError::InvalidRecord)?;
        Some(read_manifest_from(
            reader, header, epoch, key, descriptor, allowed,
        )?)
    } else {
        None
    };
    let mmr = checkpoint.mmr.to_mmr()?;
    let trailing_bytes = observed_len.saturating_sub(checkpoint.committed_end);
    Ok(ScanResult {
        segments,
        manifests,
        witnesses,
        bookmarks,
        manifest,
        mmr,
        next_witness_sequence: checkpoint.next_witness_sequence,
        last_witness_tag: checkpoint.last_witness_tag,
        last_segment_tag: checkpoint.last_segment_tag,
        last_bookmark_tag: checkpoint.last_bookmark_tag,
        committed_end: checkpoint.committed_end,
        observed_len,
        recovery: RecoveryInfo {
            trailing_bytes,
            last_committed_offset: checkpoint.committed_end,
        },
    })
}

fn take_array<const N: usize>(bytes: &[u8], offset: &mut usize) -> Result<[u8; N], LineError> {
    let end = (*offset).checked_add(N).ok_or(LineError::InvalidRecord)?;
    let value = bytes
        .get(*offset..end)
        .ok_or(LineError::InvalidRecord)?
        .try_into()
        .map_err(|_| LineError::InvalidRecord)?;
    *offset = end;
    Ok(value)
}

fn take_u64(bytes: &[u8], offset: &mut usize) -> Result<u64, LineError> {
    Ok(u64::from_le_bytes(take_array::<8>(bytes, offset)?))
}

#[cfg(unix)]
fn open_worldline_file(path: &Path, read: bool, write: bool) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = OpenOptions::new();
    options
        .read(read)
        .write(write)
        .custom_flags(libc::O_NOFOLLOW);
    options.open(path)
}

#[cfg(not(unix))]
fn open_worldline_file(path: &Path, read: bool, write: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(read).write(write).open(path)
}

fn file_identity(file: &File) -> Result<FileIdentity, LineError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok(FileIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = file.metadata()?;
        Ok(FileIdentity {})
    }
}

#[cfg(unix)]
struct FileReadLock {
    file: File,
}

#[cfg(unix)]
impl FileReadLock {
    fn acquire(file: &File) -> Result<Self, LineError> {
        use std::os::fd::AsRawFd;

        let file = file.try_clone()?;
        loop {
            // SAFETY: the guard owns this duplicated fd until `Drop` releases the lock.
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_SH) };
            if result == 0 {
                return Ok(Self { file });
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(LineError::Io(error));
            }
        }
    }
}

#[cfg(unix)]
impl Drop for FileReadLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;

        // SAFETY: the guard owns a live fd; unlocking cannot access memory.
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(not(unix))]
struct FileReadLock;

#[cfg(not(unix))]
impl FileReadLock {
    fn acquire(_file: &File) -> Result<Self, LineError> {
        Ok(Self)
    }
}

#[cfg(unix)]
struct FileAppendLock {
    file: File,
}

#[cfg(unix)]
impl FileAppendLock {
    fn acquire(file: &File) -> Result<Self, LineError> {
        use std::os::fd::AsRawFd;

        let file = file.try_clone()?;
        loop {
            // SAFETY: the guard owns this duplicated fd until `Drop` releases the lock.
            let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if result == 0 {
                return Ok(Self { file });
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(LineError::Io(error));
            }
        }
    }
}

#[cfg(unix)]
impl Drop for FileAppendLock {
    fn drop(&mut self) {
        use std::os::fd::AsRawFd;

        // SAFETY: the guard owns a live fd; unlocking cannot access memory.
        let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(not(unix))]
struct FileAppendLock;

#[cfg(not(unix))]
impl FileAppendLock {
    fn acquire(_file: &File) -> Result<Self, LineError> {
        Err(LineError::UnsupportedDurability)
    }
}

fn validate_regular_file(file: &File) -> Result<(), LineError> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(LineError::UnsafePath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() == 0 {
            return Err(LineError::UnsafePath);
        }
    }
    Ok(())
}

struct CompactionPublication {
    temporary: PathBuf,
    destination: PathBuf,
    file_identity: FileIdentity,
    published: bool,
    committed: bool,
}

impl CompactionPublication {
    fn new(temporary: PathBuf, destination: PathBuf, file_identity: FileIdentity) -> Self {
        Self {
            temporary,
            destination,
            file_identity,
            published: false,
            committed: false,
        }
    }

    fn publish(&mut self) -> Result<(), LineError> {
        let file = open_worldline_file(&self.temporary, true, true)?;
        validate_regular_file(&file)?;
        if file_identity(&file)? != self.file_identity {
            return Err(LineError::ConcurrentModification);
        }
        let _publication_lock = FileAppendLock::acquire(&file)?;
        fs::hard_link(&self.temporary, &self.destination).map_err(LineError::Io)?;
        self.published = true;

        if let Err(error) = sync_parent_directory(&self.destination) {
            let rolled_back = self.cleanup().is_ok();
            return Err(if !rolled_back {
                LineError::DurabilityUncertain
            } else {
                map_directory_sync_error(error)
            });
        }
        if let Err(error) = fs::remove_file(&self.temporary) {
            let rolled_back = self.cleanup().is_ok();
            return Err(if rolled_back {
                LineError::Io(error)
            } else {
                LineError::DurabilityUncertain
            });
        }
        if let Err(error) = sync_parent_directory(&self.destination) {
            let rolled_back = self.cleanup().is_ok();
            return Err(if !rolled_back {
                LineError::DurabilityUncertain
            } else {
                map_directory_sync_error(error)
            });
        }
        Ok(())
    }

    fn commit(&mut self) {
        self.committed = true;
    }

    fn cleanup(&mut self) -> io::Result<()> {
        let mut first_error = None;
        if self.published {
            match fs::remove_file(&self.destination) {
                Ok(()) => self.published = false,
                Err(error) if error.kind() == io::ErrorKind::NotFound => self.published = false,
                Err(error) => first_error = Some(error),
            }
        }
        match fs::remove_file(&self.temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) if first_error.is_none() => first_error = Some(error),
            Err(_) => {}
        }
        if let Err(error) = sync_parent_directory(&self.destination) {
            if first_error.is_none() {
                first_error = Some(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for CompactionPublication {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.cleanup();
        }
    }
}

fn compaction_path(destination: &Path, file_id: &[u8; 16]) -> PathBuf {
    let mut name = stage_name(file_id);
    name.push_str(".compacting");
    parent_directory(destination).join(name)
}

fn map_directory_sync_error(error: io::Error) -> LineError {
    if error.kind() == io::ErrorKind::Unsupported {
        LineError::UnsupportedDurability
    } else {
        LineError::DurabilityUncertain
    }
}

fn parent_directory(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn stage_name(file_id: &[u8; 16]) -> String {
    let mut name = String::from(".holonomy-stage-");
    for byte in file_id {
        use core::fmt::Write as _;
        write!(&mut name, "{byte:02x}").expect("writing a byte to String cannot fail");
    }
    name
}

#[cfg(unix)]
fn durability_supported() -> bool {
    true
}

#[cfg(not(unix))]
fn durability_supported() -> bool {
    false
}

#[cfg(unix)]
fn sync_parent_directory(path: &Path) -> io::Result<()> {
    File::open(parent_directory(path))?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_path: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "directory fsync is not implemented on this target",
    ))
}

#[cfg(test)]
mod tests;
