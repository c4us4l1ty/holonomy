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

use holonomy_seal::{
    open_object, seal_object, CompactionApproval, DEMKey, OpenedPlaintext, SealedObject,
};

use crate::format::{HEADER_BYTES, RECORD_FIXED_BYTES, RECORD_HEADER_BYTES};
use crate::mmr::{segment_leaf, Mmr};

const RECORD_MAGIC: &[u8; 4] = b"W6RC";
const RECORD_SEGMENT: u8 = 1;
const RECORD_WITNESS: u8 = 2;
const AEAD_TAG_BYTES: u32 = 16;
const MAX_SEGMENT_BYTES: u32 = 64 * 1024;
const WITNESS_BASE_BYTES: usize = 1 + 8 + 8 + 32 + 32 + 32 + 32;
const MAX_WITNESS_BYTES: usize = WITNESS_BASE_BYTES + 64 * 32;
const MAX_WITNESS_CIPHERTEXT_BYTES: u32 = (MAX_WITNESS_BYTES as u32) + AEAD_TAG_BYTES;

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

/// An opened append-only world-line. The key remains an opaque capability borrowed from
/// `holonomy-seal`; this crate can request seal/open operations but cannot read key bytes.
pub struct WorldLine<'key> {
    path: PathBuf,
    header: LineHeader,
    epoch: [u8; 32],
    key: &'key DEMKey,
    file_identity: FileIdentity,
    segments: Vec<SegmentDescriptor>,
    mmr: Mmr,
    next_witness_sequence: u64,
    last_witness_tag: [u8; 32],
    last_segment_tag: [u8; 32],
    committed_end: u64,
    observed_len: u64,
    recovery: RecoveryInfo,
    poisoned: bool,
}

impl<'key> WorldLine<'key> {
    /// Create a new file without replacing an existing path. A fully synced staging inode is
    /// linked into place atomically, then the containing directory is synced.
    pub fn create(
        path: impl AsRef<Path>,
        header: LineHeader,
        key: &'key DEMKey,
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
        let (genesis, genesis_tag) =
            encode_witness_record(&header, &epoch, key, 0, &zero_tag, &zero_tag, &empty_mmr)?;

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
            mmr: empty_mmr,
            next_witness_sequence: 1,
            last_witness_tag: genesis_tag,
            last_segment_tag: zero_tag,
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
    /// metadata but seeks over every segment ciphertext body.
    pub fn open(path: impl AsRef<Path>, key: &'key DEMKey) -> Result<Self, LineError> {
        let path = path.as_ref().to_path_buf();
        let mut file = open_worldline_file(&path, true, false)?;
        let _read_lock = FileReadLock::acquire(&file)?;
        validate_regular_file(&file)?;
        let file_identity = file_identity(&file)?;
        let header = LineHeader::read_from(&mut file)?;
        let epoch = header.epoch_id();
        let scan = scan_records(&mut file, &header, &epoch, key)?;
        Ok(Self {
            path,
            header,
            epoch,
            key,
            file_identity,
            segments: scan.segments,
            mmr: scan.mmr,
            next_witness_sequence: scan.next_witness_sequence,
            last_witness_tag: scan.last_witness_tag,
            last_segment_tag: scan.last_segment_tag,
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

    /// Number of committed witness records, including genesis.
    pub fn witness_count(&self) -> u64 {
        self.next_witness_sequence
    }

    /// Recovery information observed when this handle was opened or last committed.
    pub fn recovery_info(&self) -> RecoveryInfo {
        self.recovery
    }

    /// Append one or more sealed segments and commit them with one witness. Empty input is a
    /// no-op. Every non-empty segment must fit the 64 KiB quantum.
    pub fn append_segments(
        &mut self,
        plaintext_segments: &[&[u8]],
    ) -> Result<AppendReceipt, LineError> {
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
        if plaintext_segments.is_empty() {
            return Ok(self.current_receipt());
        }
        let pending_segments =
            u64::try_from(plaintext_segments.len()).map_err(|_| LineError::TooManySegments)?;
        self.mmr
            .count()
            .checked_add(pending_segments)
            .ok_or(LineError::TooManySegments)?;
        if self.next_witness_sequence == u64::MAX {
            return Err(LineError::TooManySegments);
        }

        let result = self.append_validated(plaintext_segments);
        if result.is_err() {
            // I/O may have left a partial, uncommitted tail. A fresh open is the only operation
            // that can unambiguously determine whether the witness reached durable storage.
            self.poisoned = true;
        }
        result
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

    /// Authenticate every committed segment body, one at a time. This is the explicit full-file
    /// integrity pass used by `verify`; ordinary open never reads segment ciphertext.
    pub fn verify(&self) -> Result<VerifyReport, LineError> {
        if self.poisoned {
            return Err(LineError::ReopenRequired);
        }
        let current = Self::open(&self.path, self.key)?;
        if current.file_identity != self.file_identity
            || current.last_witness_tag != self.last_witness_tag
            || current.header.file_id() != self.header.file_id()
            || current.epoch != self.epoch
        {
            return Err(LineError::ConcurrentModification);
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
    pub fn compact_to<'new_key>(
        &self,
        destination: impl AsRef<Path>,
        retain_segments: &[u64],
        new_header: LineHeader,
        new_key: &'new_key DEMKey,
        approval: CompactionApproval,
    ) -> Result<WorldLine<'new_key>, LineError> {
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
        let mut compacted = WorldLine::create(&temporary, new_header, new_key)?;
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

    fn current_receipt(&self) -> AppendReceipt {
        AppendReceipt {
            witness_sequence: self.next_witness_sequence.saturating_sub(1),
            segment_count: self.mmr.count(),
            bytes_appended: 0,
            segment_root: self.mmr.root(),
        }
    }

    fn append_validated(
        &mut self,
        plaintext_segments: &[&[u8]],
    ) -> Result<AppendReceipt, LineError> {
        let mut file = open_worldline_file(&self.path, true, true)?;
        validate_regular_file(&file)?;
        if file_identity(&file)? != self.file_identity {
            return Err(LineError::ConcurrentModification);
        }
        let _append_lock = FileAppendLock::acquire(&file)?;
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
        let disk_state = scan_records(&mut reader, &disk_header, &disk_epoch, self.key)
            .map_err(|_| LineError::ConcurrentModification)?;
        if disk_state.observed_len != actual_len
            || file.metadata()?.len() != actual_len
            || disk_state.committed_end != self.committed_end
            || disk_state.last_witness_tag != self.last_witness_tag
            || disk_state.last_segment_tag != self.last_segment_tag
            || disk_state.next_witness_sequence != self.next_witness_sequence
            || disk_state.mmr != self.mmr
            || disk_state.segments != self.segments
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
                self.key,
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

        // Data records are durable before the commit point is written.
        sync_append_file(&file)?;
        let witness_sequence = self.next_witness_sequence;
        let (witness_frame, witness_tag) = encode_witness_record(
            &self.header,
            &self.epoch,
            self.key,
            witness_sequence,
            &self.last_witness_tag,
            &last_segment_tag,
            &mmr,
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
            open_object(self.key, &aad, &sealed).map_err(|_| LineError::AuthenticationRejected)?;
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

fn encode_witness_record(
    header: &LineHeader,
    epoch: &[u8; 32],
    key: &DEMKey,
    sequence: u64,
    previous_witness_tag: &[u8; 32],
    last_segment_tag: &[u8; 32],
    mmr: &Mmr,
) -> Result<(Vec<u8>, [u8; 32]), LineError> {
    let plaintext =
        encode_witness_plain(sequence, previous_witness_tag, last_segment_tag, epoch, mmr);
    let aad = witness_aad(header, epoch, sequence, previous_witness_tag);
    let sealed = seal_object(key, &aad, &plaintext);
    let tag = sealed.cmt4;
    Ok((encode_record(RECORD_WITNESS, sequence, sealed)?, tag))
}

fn encode_witness_plain(
    sequence: u64,
    previous_witness_tag: &[u8; 32],
    last_segment_tag: &[u8; 32],
    epoch: &[u8; 32],
    mmr: &Mmr,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(WITNESS_BASE_BYTES + mmr.count().count_ones() as usize * 32);
    out.push(1); // Witness payload version.
    out.extend_from_slice(&sequence.to_le_bytes());
    out.extend_from_slice(&mmr.count().to_le_bytes());
    out.extend_from_slice(previous_witness_tag);
    out.extend_from_slice(last_segment_tag);
    out.extend_from_slice(epoch);
    out.extend_from_slice(&mmr.root());
    mmr.encode_peaks(&mut out);
    out
}

struct DecodedWitness {
    sequence: u64,
    segment_count: u64,
    previous_witness_tag: [u8; 32],
    last_segment_tag: [u8; 32],
    epoch: [u8; 32],
    mmr: Mmr,
}

fn decode_witness_plain(bytes: &[u8]) -> Result<DecodedWitness, LineError> {
    if bytes.len() < WITNESS_BASE_BYTES || bytes[0] != 1 {
        return Err(LineError::InvalidRecord);
    }
    let mut offset = 1;
    let sequence = take_u64(bytes, &mut offset)?;
    let segment_count = take_u64(bytes, &mut offset)?;
    let previous_witness_tag = take_array::<32>(bytes, &mut offset)?;
    let last_segment_tag = take_array::<32>(bytes, &mut offset)?;
    let epoch = take_array::<32>(bytes, &mut offset)?;
    let root = take_array::<32>(bytes, &mut offset)?;
    let peaks = Mmr::from_peak_bytes(segment_count, &bytes[offset..])?;
    if peaks.root() != root {
        return Err(LineError::InvalidRecord);
    }
    Ok(DecodedWitness {
        sequence,
        segment_count,
        previous_witness_tag,
        last_segment_tag,
        epoch,
        mmr: peaks,
    })
}

fn segment_aad(
    header: &LineHeader,
    epoch: &[u8; 32],
    sequence: u64,
    previous_tag: &[u8; 32],
    plaintext_len: u32,
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(21 + 16 + 32 + 8 + 32 + 4);
    aad.extend_from_slice(b"HOLONOMY_V6_SEGMENT\0");
    aad.extend_from_slice(header.file_id());
    aad.extend_from_slice(epoch);
    aad.extend_from_slice(&sequence.to_le_bytes());
    aad.extend_from_slice(previous_tag);
    aad.extend_from_slice(&plaintext_len.to_le_bytes());
    aad
}

fn witness_aad(
    header: &LineHeader,
    epoch: &[u8; 32],
    sequence: u64,
    previous_witness_tag: &[u8; 32],
) -> Vec<u8> {
    let mut aad = Vec::with_capacity(21 + 16 + 32 + 8 + 32);
    aad.extend_from_slice(b"HOLONOMY_V6_WITNESS\0");
    aad.extend_from_slice(header.file_id());
    aad.extend_from_slice(epoch);
    aad.extend_from_slice(&sequence.to_le_bytes());
    aad.extend_from_slice(previous_witness_tag);
    aad
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
    mmr: Mmr,
    next_witness_sequence: u64,
    last_witness_tag: [u8; 32],
    last_segment_tag: [u8; 32],
    committed_end: u64,
    observed_len: u64,
    recovery: RecoveryInfo,
}

#[derive(Clone, Copy)]
struct Checkpoint {
    committed_segment_count: usize,
    mmr: MmrCheckpoint,
    next_witness_sequence: u64,
    last_witness_tag: [u8; 32],
    last_segment_tag: [u8; 32],
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
    let mut mmr = Mmr::new();
    let mut scanned_last_segment_tag = [0u8; 32];
    let mut next_witness_sequence = 0u64;
    let mut last_witness_tag = [0u8; 32];
    let mut checkpoint = None;

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
        if kind != RECORD_SEGMENT && kind != RECORD_WITNESS {
            return Err(LineError::InvalidRecord);
        }
        let body_len = u32::from_le_bytes(frame_header[8..12].try_into().unwrap());
        if body_len < RECORD_FIXED_BYTES as u32 {
            return Err(LineError::InvalidRecord);
        }
        let maximum = if kind == RECORD_SEGMENT {
            RECORD_FIXED_BYTES as u32 + MAX_SEGMENT_BYTES + AEAD_TAG_BYTES
        } else {
            RECORD_FIXED_BYTES as u32 + MAX_WITNESS_CIPHERTEXT_BYTES
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
                        || witness.last_segment_tag != [0u8; 32]))
            {
                return Err(LineError::AuthenticationRejected);
            }
            last_witness_tag = tag;
            next_witness_sequence = sequence.checked_add(1).ok_or(LineError::TooManySegments)?;
            checkpoint = Some(Checkpoint {
                committed_segment_count: segments.len(),
                mmr: MmrCheckpoint::from_mmr(&mmr),
                next_witness_sequence,
                last_witness_tag,
                last_segment_tag: scanned_last_segment_tag,
                committed_end: record_end,
            });
        }

        if reader.stream_position()? != record_end {
            return Err(LineError::InvalidRecord);
        }
    }

    let checkpoint = checkpoint.ok_or(LineError::NoCommittedWitness)?;
    segments.truncate(checkpoint.committed_segment_count);
    let mmr = checkpoint.mmr.to_mmr()?;
    let trailing_bytes = observed_len.saturating_sub(checkpoint.committed_end);
    Ok(ScanResult {
        segments,
        mmr,
        next_witness_sequence: checkpoint.next_witness_sequence,
        last_witness_tag: checkpoint.last_witness_tag,
        last_segment_tag: checkpoint.last_segment_tag,
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
