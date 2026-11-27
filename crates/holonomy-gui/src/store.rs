//! A document on disk — `docs/TO-APP.md` M5.
//!
//! Until this module existed the window had nowhere to put anything: `Typing` wrote its sealed
//! leaves into whatever `SegmentSink` it was handed, and the terminal front end handed it a
//! `Vec<Vec<u8>>` that died with the process. This is the real sink: a `.wavefunction` world-line
//! under a passphrase, created and opened the way the container crate says, with the two adapters
//! the commit-boundary test proved — a sink that appends one segment per seal, and a source that
//! reads one segment on demand.
//!
//! What the window gets from here:
//!
//! 1. **Create.** `calibrate()` measures this host's Argon2 and records the schedule in the header;
//!    the passphrase derives the unwrap key; the capsule holds the DEM key the segments are sealed
//!    with. Nothing about the passphrase is stored.
//! 2. **Open.** The public header is read first (no body is touched), the passphrase replays the
//!    recorded schedule, the capsule yields the DEM key or a single coarse refusal, and the
//!    world-line authenticates its witness chain before the window sees it.
//! 3. **Geometry.** [`Document::spine`] builds the spine from the authenticated segment lengths, so
//!    a long document opens as fast as a short one and no plaintext is read until a frame asks.
//! 4. **Save.** Every seal of the open leaf is one append plus one witness, which is the commit
//!    path L-6 measures; the durability work is `holonomy-line`'s and unchanged.
//!
//! # Key ownership
//!
//! [`WorldLine`] owns its `DEMKey`, so the document needs no leaked key and no self-reference.
//! The only deliberate duplication is `DEMKey::duplicate()` for `verify` and compaction; each copy
//! is independently locked and zeroized on drop.

use std::cell::RefCell;
use std::fmt;
use std::io;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use zeroize::Zeroizing;

use holonomy_line::{
    Bookmark, LineError, LineHeader, LineManifest, LineSlab, LineStyleRun, StreamingTx,
    WitnessSummary, WorldLine,
};
use holonomy_seal::{
    calibrate, confirm_compaction, create_capsule_for, generate_kdf_salt, open_capsule_for,
    KdfParamsRecord, MasterKeys, PassphraseInput, SecretArenas,
};
use holonomy_text::{
    find_streaming, visit_streaming, Appended, DocStyle, DocStyleSpan, Draft, OpenSources,
    SearchError, SearchSummary, SegmentId, SegmentSink, SegmentSource, Slab, Spine, SpineDiffHunk,
    StreamSegmentWriter, StyleMap,
};

/// How much text one open leaf holds before the session seals it.
///
/// A quarter of the container's 64 KiB quantum: one seal is one append, and four leaves per
/// quantum leave the resident set room to hold several leaves at once.
pub const CAPACITY: usize = 16 * 1024;

/// The resident set's byte budget for the window — enough for 256 leaves of [`CAPACITY`].
pub const BUDGET: usize = 4 * 1024 * 1024;

/// Spare capacity reserved per resident leaf, so a keystroke inside it edits in place.
pub const SLACK: usize = 1024;

/// Why a document could not be created, opened or saved.
#[derive(Debug)]
pub enum StoreError {
    /// The file system refused: no such directory, no permission, no space.
    Io(io::Error),
    /// One coarse refusal for everything the file says: a wrong passphrase, a capsule that does
    /// not open, a witness chain that does not authenticate, a header that does not parse.
    ///
    /// Deliberately coarse. A caller that can tell "wrong passphrase" from "altered ciphertext" can
    /// tell an attacker which half to work on (AUDIT AC-3), so the window shows one sentence.
    Refused,
    /// The key schedule recorded in the header is outside the production fence, or the derivation
    /// refused it. An untrusted header must not be able to demand arbitrary work.
    Params,
    /// The world-line accepted the file and then refused an operation — a poisoned handle after a
    /// failed append, most often. Reopening is the remedy, and the message says so.
    Line(String),
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "the file system refused: {error}"),
            Self::Refused => write!(
                f,
                "the document did not open: wrong passphrase, or the file has been altered"
            ),
            Self::Params => write!(
                f,
                "the document's key schedule is outside the production fence"
            ),
            Self::Line(reason) => write!(f, "the world-line refused: {reason}"),
        }
    }
}

impl std::error::Error for StoreError {}

/// Maps an open-time failure to the coarse refusal, keeping the file system's own answer.
fn map_open(error: LineError) -> StoreError {
    match error {
        LineError::Io(error) => StoreError::Io(error),
        LineError::UnsupportedDurability => StoreError::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            "this platform has no qualified durable document storage backend",
        )),
        _ => StoreError::Refused,
    }
}

/// Normalizes a passphrase for document creation, mapping invalid or empty input to
/// [`StoreError::Params`] (AUDIT OQ-28).
fn normalize_for_create(passphrase: &[u8]) -> Result<PassphraseInput, StoreError> {
    let text = std::str::from_utf8(passphrase).map_err(|_| StoreError::Params)?;
    PassphraseInput::new(text).map_err(|_| StoreError::Params)
}

/// Normalizes a passphrase for document open or compaction confirmation, mapping invalid or empty
/// input to the single coarse [`StoreError::Refused`] (AUDIT AC-3, OQ-28).
fn normalize_for_open(passphrase: &[u8]) -> Result<PassphraseInput, StoreError> {
    let text = std::str::from_utf8(passphrase).map_err(|_| StoreError::Refused)?;
    PassphraseInput::new(text).map_err(|_| StoreError::Refused)
}

/// An opened world-line and the geometry it presents to a window.
#[derive(Clone)]
pub struct Document {
    pub(crate) line: Rc<RefCell<Line>>,
}

pub(crate) struct Line {
    pub(crate) path: PathBuf,
    keys: MasterKeys,
    pub(crate) line: WorldLine,
}

impl Document {
    /// Creates a document, calibrating this host's Argon2 schedule first.
    ///
    /// # Errors
    ///
    /// As [`Self::create_with`], plus whatever the calibration refuses.
    pub fn create(path: &Path, passphrase: &[u8]) -> Result<Self, StoreError> {
        let input = normalize_for_create(passphrase)?;
        Self::create_input(path, &input)
    }

    /// Creates a document from an already NFC-normalized [`PassphraseInput`], calibrating this
    /// host's Argon2 schedule first (AUDIT OQ-28).
    ///
    /// # Errors
    ///
    /// As [`Self::create_with_input`], plus whatever the calibration refuses.
    pub fn create_input(path: &Path, passphrase: &PassphraseInput) -> Result<Self, StoreError> {
        let calibration = calibrate();
        Self::create_with_input(path, passphrase, calibration.params)
    }

    /// Creates a document under an explicit key schedule.
    ///
    /// This is the entry point for tests, which must not spend 256 MiB and a second per open; the
    /// fenced production path is [`Self::create`]. A record outside the fence is refused when the
    /// derivation replays it, so passing a weak schedule here cannot weaken the file.
    ///
    /// # Errors
    ///
    /// [`StoreError::Io`] if the path cannot hold a new file (it never replaces one), and
    /// [`StoreError::Params`] if the schedule or passphrase is refused.
    pub fn create_with(
        path: &Path,
        passphrase: &[u8],
        params: KdfParamsRecord,
    ) -> Result<Self, StoreError> {
        let input = normalize_for_create(passphrase)?;
        Self::create_with_input(path, &input, params)
    }

    /// Creates a document under an explicit key schedule from an already NFC-normalized
    /// [`PassphraseInput`].
    ///
    /// # Errors
    ///
    /// [`StoreError::Io`] if the path cannot hold a new file, and [`StoreError::Params`] if the
    /// schedule is refused.
    pub fn create_with_input(
        path: &Path,
        passphrase: &PassphraseInput,
        params: KdfParamsRecord,
    ) -> Result<Self, StoreError> {
        let salt = generate_kdf_salt().map_err(|_| StoreError::Params)?;
        let keys = passphrase
            .with_bytes(|bytes| MasterKeys::derive_with(bytes, &salt, None, &params))
            .map_err(|_| StoreError::Params)?;
        let (capsule, dem) =
            create_capsule_for(keys.unwrap_key(), None).map_err(|_| StoreError::Params)?;
        let header = LineHeader::new(params, salt, capsule)
            .map_err(|error| StoreError::Line(error.to_string()))?;
        let line = WorldLine::create(path, header, dem).map_err(map_open)?;
        Ok(Self {
            line: Rc::new(RefCell::new(Line {
                path: path.to_path_buf(),
                keys,
                line,
            })),
        })
    }

    /// Opens an existing document.
    ///
    /// Reads the public header only, NFC-normalizes the passphrase, replays the recorded key
    /// schedule, and asks the capsule for the DEM key before the world-line authenticates a byte.
    ///
    /// # Errors
    ///
    /// [`StoreError::Refused`] for a wrong passphrase or an altered file — one answer for both,
    /// on purpose — and [`StoreError::Io`] if the file cannot be read.
    pub fn open(path: &Path, passphrase: &[u8]) -> Result<Self, StoreError> {
        let input = normalize_for_open(passphrase)?;
        Self::open_input(path, &input)
    }

    /// Opens an existing document from an already NFC-normalized [`PassphraseInput`] (AUDIT OQ-28).
    ///
    /// # Errors
    ///
    /// [`StoreError::Refused`] for a wrong passphrase or an altered file, and [`StoreError::Io`]
    /// if the file cannot be read.
    pub fn open_input(path: &Path, passphrase: &PassphraseInput) -> Result<Self, StoreError> {
        let header = LineHeader::read(path).map_err(map_open)?;
        let keys = passphrase
            .with_bytes(|bytes| {
                MasterKeys::derive_with(bytes, &header.salt, None, &header.kdf_params)
            })
            .map_err(|_| StoreError::Refused)?;
        let dem = open_capsule_for(keys.unwrap_key(), &header.capsule, None)
            .map_err(|_| StoreError::Refused)?;
        let line = WorldLine::open(path, dem).map_err(map_open)?;
        Ok(Self {
            line: Rc::new(RefCell::new(Line {
                path: path.to_path_buf(),
                keys,
                line,
            })),
        })
    }

    /// The file this document is.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.line.borrow().path.clone()
    }

    /// How many sealed segments the file holds — the document's leaves, before this session adds
    /// any.
    #[must_use]
    pub fn segment_count(&self) -> u64 {
        self.line.borrow().line.segment_count()
    }

    /// How many witness commits the file holds, genesis included.
    #[must_use]
    pub fn witness_count(&self) -> u64 {
        self.line.borrow().line.witness_count()
    }

    /// Bytes past the last committed witness, if a previous run died mid-append.
    #[must_use]
    pub fn trailing_bytes(&self) -> u64 {
        self.line.borrow().line.recovery_info().trailing_bytes
    }

    /// The document's geometry: one slab per active segment span, taken from the authenticated
    /// spine manifest (or implicit append order when no structural edit has occurred). No segment
    /// plaintext is read (AUDIT OQ-29, OQ-30).
    ///
    /// # Errors
    ///
    /// [`StoreError::Line`] if the shared handle is already borrowed, which only a re-entrant call
    /// inside an append could do.
    pub fn spine(&self) -> Result<Spine, StoreError> {
        let line = self
            .line
            .try_borrow()
            .map_err(|_| StoreError::Line("the document is busy".to_owned()))?;
        Ok(spine_of(&line.line))
    }

    /// Fully authenticates the committed segment bodies and the witness chain.
    ///
    /// This is what a panel may say "verified" on: it re-opens the file through the container's own
    /// check and compares the identity and the last witness tag, so a file that changed under this
    /// handle is reported rather than trusted.
    ///
    /// # Errors
    ///
    /// [`StoreError::Refused`] if the chain or a body does not authenticate — which is the same
    /// answer a wrong passphrase gets, and for the same reason.
    pub fn verify(&self) -> Result<holonomy_line::VerifyReport, StoreError> {
        let line = self
            .line
            .try_borrow()
            .map_err(|_| StoreError::Line("the document is busy".to_owned()))?;
        line.line.verify().map_err(map_open)
    }

    /// Rewrites only the active document spine into a fresh, re-keyed `.wavefunction` at
    /// `destination`, discarding deleted byte spans and superseded history after re-confirming the
    /// passphrase (`AUDIT OQ-14`).
    ///
    /// # Errors
    ///
    /// [`StoreError::Refused`] if `confirmed_passphrase` does not match this document's unlocked
    /// capability or source verification fails, [`StoreError::Io`] if `destination` already exists
    /// or cannot be written, and [`StoreError::Line`] if `destination` is invalid.
    pub fn compact_to(
        &self,
        destination: &Path,
        confirmed_passphrase: &[u8],
    ) -> Result<Self, StoreError> {
        let input = normalize_for_open(confirmed_passphrase)?;
        self.compact_to_input(destination, &input)
    }

    /// Rewrites only the active document spine into a fresh, re-keyed `.wavefunction` at
    /// `destination` using an already NFC-normalized confirmation passphrase (`AUDIT OQ-14`,
    /// `OQ-28`).
    ///
    /// # Errors
    ///
    /// As [`Self::compact_to`].
    pub fn compact_to_input(
        &self,
        destination: &Path,
        confirmed_passphrase: &PassphraseInput,
    ) -> Result<Self, StoreError> {
        let line = self
            .line
            .try_borrow()
            .map_err(|_| StoreError::Line("the document is busy".to_owned()))?;
        let header = line.line.header();
        let approval = confirmed_passphrase
            .with_bytes(|bytes| {
                confirm_compaction(
                    bytes,
                    &header.salt,
                    None,
                    &header.kdf_params,
                    line.keys.unwrap_key(),
                    header.file_id(),
                )
            })
            .map_err(|_| StoreError::Refused)?;
        let new_salt = generate_kdf_salt().map_err(|_| StoreError::Params)?;
        let new_keys = confirmed_passphrase
            .with_bytes(|bytes| MasterKeys::derive_with(bytes, &new_salt, None, &header.kdf_params))
            .map_err(|_| StoreError::Params)?;
        let (new_capsule, new_dem) =
            create_capsule_for(new_keys.unwrap_key(), None).map_err(|_| StoreError::Params)?;
        let new_header = LineHeader::new(header.kdf_params, new_salt, new_capsule)
            .map_err(|error| StoreError::Line(error.to_string()))?;
        let compacted =
            line.line
                .compact_active_to(destination, new_header, &new_dem, approval)
                .map_err(|error| match error {
                    LineError::Io(io_err) => StoreError::Io(io_err),
                    LineError::CompactionConfirmationRequired
                    | LineError::AuthenticationRejected => StoreError::Refused,
                    other => StoreError::Line(other.to_string()),
                })?;
        Ok(Self {
            line: Rc::new(RefCell::new(Line {
                path: destination.to_path_buf(),
                keys: new_keys,
                line: compacted,
            })),
        })
    }

    /// Arms this document's unlocked [`MasterKeys`] (`UnwrapKey`) and [`WorldLine`] (`DEMKey`) in
    /// `arenas` so an emergency reaper trip volatile-zeroes both keys in place (`L-7`).
    ///
    /// # Safety
    ///
    /// No document or key access may overlap a wipe, and registered pointers must remain valid.
    pub unsafe fn arm_reaper(&self, arenas: &Arc<SecretArenas>) -> usize {
        let mut inner = self.line.borrow_mut();
        let mut count = 0usize;
        // SAFETY: the caller guarantees exclusive access throughout the wipe.
        if unsafe { inner.keys.unwrap_key_mut().arm_reaper(arenas) } {
            count += 1;
        }
        // SAFETY: the same guarantee covers the world-line key.
        if unsafe { inner.line.arm_reaper(arenas) } {
            count += 1;
        }
        count
    }

    /// Whether an emergency reaper wipe has zeroized this document's keys in place.
    #[must_use]
    pub fn are_keys_zeroized(&self) -> bool {
        let inner = self.line.borrow();
        inner.keys.unwrap_key().is_zeroized() && inner.line.is_key_zeroized()
    }

    /// The sink half of the document, for a session that seals leaves into it.
    #[must_use]
    pub fn sink(&self) -> DocumentSink {
        DocumentSink(self.clone())
    }

    /// The source half, for a resident set that faults leaves back in.
    #[must_use]
    pub fn source(&self) -> DocumentSource {
        DocumentSource(self.clone())
    }

    /// Lists every committed witness checkpoint in chronological order (`Block 8`).
    #[must_use]
    pub fn witness_history(&self) -> Vec<WitnessSummary> {
        self.line.borrow().line.witness_history().to_vec()
    }

    /// Lists every authenticated named bookmark recorded on this document (`Block 8`).
    #[must_use]
    pub fn bookmarks(&self) -> Vec<Bookmark> {
        self.line.borrow().line.bookmarks().to_vec()
    }

    /// Reconstructs the immutable [`Spine`] active at committed `witness_sequence` without
    /// decrypting any segment plaintext (`Block 8`, `Law L-2`).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Line`] if `witness_sequence` does not exist or the historical manifest
    /// cannot be authenticated.
    pub fn spine_at_witness(&self, witness_sequence: u64) -> Result<Spine, StoreError> {
        let line = self
            .line
            .try_borrow()
            .map_err(|_| StoreError::Line("the document is busy".to_owned()))?;
        let manifest = line
            .line
            .manifest_at_witness(witness_sequence)
            .map_err(|err| StoreError::Line(err.to_string()))?;
        Ok(manifest_to_spine(manifest))
    }

    /// Records a named bookmark anchored to committed `witness_sequence` (`Block 8`).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Line`] if the witness does not exist, uncommitted segments are
    /// pending, or the bookmark name is invalid.
    pub fn add_bookmark(
        &self,
        witness_sequence: u64,
        timestamp_secs: i64,
        name: &str,
    ) -> Result<Bookmark, StoreError> {
        let mut line = self
            .line
            .try_borrow_mut()
            .map_err(|_| StoreError::Line("the document is busy".to_owned()))?;
        line.line
            .add_bookmark(witness_sequence, timestamp_secs, name)
            .map_err(|err| StoreError::Line(err.to_string()))
    }

    /// Appends `entries` so each entry seals its own segment and durable witness checkpoint under
    /// one exclusive write lock (`O(N)` total cost for multi-decade logs).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Line`] if streaming, sealing, or committing fails.
    pub fn append_witnessed_entries<I>(&self, entries: I) -> Result<(), StoreError>
    where
        I: IntoIterator,
        I::Item: AsRef<[u8]>,
    {
        let mut line = self
            .line
            .try_borrow_mut()
            .map_err(|_| StoreError::Line("the document is busy".to_owned()))?;
        let mut tx = line
            .line
            .start_streaming_tx()
            .map_err(|err| StoreError::Line(err.to_string()))?;
        for entry in entries {
            let step = match tx.push_segment(entry.as_ref()) {
                Ok(_) => tx.commit_witness(),
                Err(err) => Err(err),
            };
            if let Err(err) = step {
                let _ = line.line.abort_streaming_tx(tx);
                return Err(StoreError::Line(err.to_string()));
            }
        }
        line.line
            .finish_streaming_tx(tx, None)
            .map_err(|err| StoreError::Line(err.to_string()))?;
        Ok(())
    }

    /// Computes a structural diff between two committed witnesses without decrypting any segment
    /// plaintext (`Block 8`, `Law L-2`).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Line`] if either witness cannot be opened.
    pub fn diff_witnesses(
        &self,
        older_witness: u64,
        newer_witness: u64,
    ) -> Result<Vec<SpineDiffHunk>, StoreError> {
        let older = self.spine_at_witness(older_witness)?;
        let newer = self.spine_at_witness(newer_witness)?;
        Ok(older.diff_slabs(&newer))
    }

    /// Counts matches and locates match `index` in committed `witness_sequence` using bounded
    /// streaming search without expanding the active resident set (`Block 8`).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Line`] if reading the historical witness or searching fails.
    pub fn find_nth_at_witness(
        &self,
        witness_sequence: u64,
        needle: &str,
        index: u64,
    ) -> Result<SearchSummary, StoreError> {
        let spine = self.spine_at_witness(witness_sequence)?;
        if needle.is_empty() || spine.is_empty() {
            return Ok(SearchSummary {
                count: 0,
                selected: None,
            });
        }
        let mut source = self.source();
        let draft = Draft::new();
        let sources = OpenSources::new();
        let mut count = 0u64;
        let mut first = None;
        let mut selected = None;
        visit_streaming(
            &spine,
            &draft,
            &sources,
            &[],
            needle.as_bytes(),
            &mut source,
            |hit| {
                if first.is_none() {
                    first = Some(hit);
                }
                if count == index {
                    selected = Some(hit);
                }
                count = count.saturating_add(1);
                true
            },
        )
        .map_err(|err| StoreError::Line(err.to_string()))?;
        Ok(SearchSummary {
            count,
            selected: selected.or(first),
        })
    }

    /// Searches every committed witness across the document's history using bounded streaming
    /// search, returning each witness that contains at least one occurrence of `needle` (`Block 8`).
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Line`] if `needle` exceeds the scan limit or segment loading fails.
    pub fn search_history(&self, needle: &str) -> Result<Vec<HistorySearchHit>, StoreError> {
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let witnesses = self.witness_history();
        let mut hits = Vec::new();
        let mut source = self.source();
        let draft = Draft::new();
        let sources = OpenSources::new();
        for summary in witnesses {
            if summary.segment_count == 0 {
                continue;
            }
            let spine = self.spine_at_witness(summary.sequence)?;
            let mut count = 0u64;
            let mut first = None;
            visit_streaming(
                &spine,
                &draft,
                &sources,
                &[],
                needle.as_bytes(),
                &mut source,
                |hit| {
                    if first.is_none() {
                        first = Some(hit);
                    }
                    count = count.saturating_add(1);
                    true
                },
            )
            .map_err(|err| StoreError::Line(err.to_string()))?;
            if count > 0 {
                hits.push(HistorySearchHit {
                    witness_sequence: summary.sequence,
                    count,
                    first_offset: first.map(|h| h.start),
                });
            }
        }
        Ok(hits)
    }

    /// Reads a UTF-8 excerpt intersecting `[start, end)` from historical `witness_sequence`
    /// using bounded viewport reads (`Block 8` diff/preview pane), trimming any partial multi-byte
    /// UTF-8 edges caused by byte slicing instead of failing.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Line`] if `witness_sequence` or segments fail authentication, or
    /// [`StoreError::Refused`] if the interior bytes are not valid UTF-8.
    pub fn read_witness_excerpt(
        &self,
        witness_sequence: u64,
        start: u64,
        end: u64,
    ) -> Result<String, StoreError> {
        let spine = self.spine_at_witness(witness_sequence)?;
        let lo = start.min(spine.len());
        let hi = end.min(spine.len());
        if lo >= hi {
            return Ok(String::new());
        }
        let line = self.line.borrow();
        let mut out = Zeroizing::new(Vec::with_capacity((hi - lo) as usize));
        for span in spine.spans_in(lo, hi) {
            let slab = span.slab();
            let plain = line
                .line
                .open_segment(slab.segment().get())
                .map_err(|err| StoreError::Line(err.to_string()))?;
            out.extend_from_slice(&plain[slab.start() as usize..slab.end() as usize]);
        }
        let mut head = 0usize;
        if lo > 0 {
            while head < out.len() && head < 3 && (out[head] & 0b1100_0000) == 0b1000_0000 {
                head += 1;
            }
        }
        let slice = &out[head..];
        match std::str::from_utf8(slice) {
            Ok(text) => Ok(text.to_owned()),
            Err(err) if err.error_len().is_none() => {
                // Truncated at `hi` in the middle of a trailing multi-byte scalar value.
                let valid = std::str::from_utf8(&slice[..err.valid_up_to()])
                    .map_err(|_| StoreError::Refused)?;
                Ok(valid.to_owned())
            }
            Err(_) => Err(StoreError::Refused),
        }
    }

    /// Re-exports `search_matching_ranges` bounded at `MAX_COLLECTED_MATCHES` for a witness.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError::Line`] if scanning fails or exceeds collection bounds.
    pub fn collect_witness_matches(
        &self,
        witness_sequence: u64,
        needle: &[u8],
    ) -> Result<Vec<holonomy_text::SearchMatch>, StoreError> {
        let spine = self.spine_at_witness(witness_sequence)?;
        let mut source = self.source();
        let draft = Draft::new();
        let sources = OpenSources::new();
        find_streaming(&spine, &draft, &sources, &[], needle, &mut source).map_err(
            |err| match err {
                SearchError::MatchLimitExceeded { limit } => {
                    StoreError::Line(format!("exceeded {limit} collected matches"))
                }
                other => StoreError::Line(other.to_string()),
            },
        )
    }
}

/// Summary of historical matches found inside one committed witness (`Block 8`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistorySearchHit {
    /// Committed witness sequence where `needle` appeared.
    pub witness_sequence: u64,
    /// Number of non-overlapping matches in that snapshot.
    pub count: u64,
    /// Byte offset of the first match in that snapshot.
    pub first_offset: Option<u64>,
}

/// The spine a world-line presents: one [`Slab`] per active [`LineSlab`] plus any persisted
/// rich-text [`StyleMap`] runs and links, in authenticated document order (AUDIT OQ-29, OQ-30,
/// Block 7).
fn spine_of(line: &WorldLine) -> Spine {
    manifest_to_spine(line.document_manifest())
}

fn manifest_to_spine(manifest: LineManifest) -> Spine {
    let slabs: Vec<Slab> = manifest
        .slabs
        .into_iter()
        .map(|slab| Slab::new(SegmentId::new(slab.segment), slab.start, slab.len))
        .collect();
    if manifest.styles.is_empty() && manifest.links.is_empty() {
        return Spine::from_slabs(slabs);
    }
    let runs: Vec<DocStyleSpan> = manifest
        .styles
        .into_iter()
        .filter_map(|r| {
            let style = DocStyle::from_wire(
                r.flags,
                r.family,
                r.heading,
                r.alignment,
                r.list_kind,
                r.list_depth,
                r.blockquote,
                r.size_milli,
                r.list_start,
                r.link_id,
            )?;
            Some(DocStyleSpan {
                start: r.start,
                end: r.end,
                style,
            })
        })
        .collect();
    let styles = StyleMap::from_raw(runs, manifest.links);
    Spine::from_slabs_and_styles(slabs, styles)
}

/// Converts a text-layer [`Slab`] slice into container [`LineSlab`] records.
fn to_line_slabs(slabs: &[Slab]) -> Vec<LineSlab> {
    slabs
        .iter()
        .map(|slab| LineSlab {
            segment: slab.segment().get(),
            start: slab.start(),
            len: slab.len(),
        })
        .collect()
}

/// Converts resolved [`LineSlab`] spans and a [`StyleMap`] into a [`LineManifest`].
pub(crate) fn to_line_manifest(slabs: Vec<LineSlab>, styles: &StyleMap) -> LineManifest {
    let mut runs = Vec::with_capacity(styles.runs().len());
    for span in styles.runs() {
        let st = span.style;
        let (list_kind, list_depth, list_start) = st.list.to_wire();
        runs.push(LineStyleRun {
            start: span.start,
            end: span.end,
            flags: st.flags_byte(),
            family: st.family.code(),
            heading: st.heading.code(),
            alignment: st.alignment.code(),
            list_kind,
            list_depth,
            blockquote: st.blockquote.min(8),
            size_milli: st.size_milli.max(1),
            list_start,
            link_id: st.link_id.map_or(0, NonZeroU32::get),
        });
    }
    LineManifest {
        slabs,
        styles: runs,
        links: styles.links().to_vec(),
    }
}

/// Appends one sealed leaf per call — the sink half of a [`Document`].
pub struct DocumentSink(Document);

/// Reads one segment per call — the source half of a [`Document`].
pub struct DocumentSource(Document);

impl SegmentSink for DocumentSink {
    fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String> {
        let mut line = self.0.line.borrow_mut();
        let receipt = line
            .line
            .append_segments(&[plaintext])
            .map_err(|error| error.to_string())?;
        Ok(Appended {
            // Sequences are dense from zero, so the segment just appended is the last one.
            segment: SegmentId::new(receipt.segment_count.saturating_sub(1)),
            witness_sequence: receipt.witness_sequence,
            segment_root: receipt.segment_root,
            bytes_written: receipt.bytes_appended,
        })
    }

    fn append_segment_with_spine(
        &mut self,
        plaintext: &[u8],
        before: &[Slab],
        after: &[Slab],
    ) -> Result<Appended, String> {
        self.append_segment_with_manifest(plaintext, before, after, &StyleMap::empty())
    }

    fn append_segment_with_manifest(
        &mut self,
        plaintext: &[u8],
        before: &[Slab],
        after: &[Slab],
        styles: &StyleMap,
    ) -> Result<Appended, String> {
        let mut line = self.0.line.borrow_mut();
        let next_segment = SegmentId::new(line.line.segment_count());
        let Ok(len) = u32::try_from(plaintext.len()) else {
            return Err("segment exceeds u32".to_owned());
        };
        let mut slabs = Vec::with_capacity(before.len() + 1 + after.len());
        slabs.extend(to_line_slabs(before));
        slabs.push(LineSlab {
            segment: next_segment.get(),
            start: 0,
            len,
        });
        slabs.extend(to_line_slabs(after));
        let receipt =
            if styles.is_empty() && line.line.is_implicit_after_append(&[plaintext], &slabs) {
                line.line
                    .append_segments(&[plaintext])
                    .map_err(|error| error.to_string())?
            } else {
                let full = to_line_manifest(slabs, styles);
                line.line
                    .append_segments_with_full_manifest(&[plaintext], &full)
                    .map_err(|error| error.to_string())?
            };
        Ok(Appended {
            segment: next_segment,
            witness_sequence: receipt.witness_sequence,
            segment_root: receipt.segment_root,
            bytes_written: receipt.bytes_appended,
        })
    }

    fn atomic_replace(
        &mut self,
        chunks: &[&[u8]],
        build: &mut holonomy_text::ReplacementBuilder<'_>,
    ) -> Result<(), String> {
        let mut line = self.0.line.borrow_mut();
        let first = line.line.segment_count();
        let ids = (0..chunks.len())
            .map(|index| {
                first
                    .checked_add(index as u64)
                    .map(SegmentId::new)
                    .ok_or_else(|| "segment ID overflow".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?;
        let (slabs, styles) = build(&ids)?;
        let manifest = to_line_manifest(to_line_slabs(&slabs), &styles);
        line.line
            .append_segments_with_full_manifest(chunks, &manifest)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn commit_spine(&mut self, slabs: &[Slab]) -> Result<(), String> {
        self.commit_manifest(slabs, &StyleMap::empty())
    }

    fn commit_manifest(&mut self, slabs: &[Slab], styles: &StyleMap) -> Result<(), String> {
        let mut line = self.0.line.borrow_mut();
        let desired = to_line_manifest(to_line_slabs(slabs), styles);
        if desired == line.line.document_manifest() {
            return Ok(());
        }
        line.line
            .commit_full_manifest(&desired)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn supports_stream_publish(&self) -> bool {
        true
    }

    fn begin_stream_publish(&mut self) -> Result<Box<dyn StreamSegmentWriter + '_>, String> {
        struct StoreStreamWriter {
            doc: Document,
            tx: Option<StreamingTx>,
        }

        impl StreamSegmentWriter for StoreStreamWriter {
            fn write_chunk(&mut self, plaintext: &[u8]) -> Result<SegmentId, String> {
                let slab = self
                    .tx
                    .as_mut()
                    .ok_or_else(|| "streaming tx closed".to_owned())?
                    .push_segment(plaintext)
                    .map_err(|error| error.to_string())?;
                Ok(SegmentId::new(slab.segment))
            }

            fn finish(
                mut self: Box<Self>,
                slabs: &[Slab],
                styles: &StyleMap,
            ) -> Result<(), String> {
                let tx = self
                    .tx
                    .take()
                    .ok_or_else(|| "streaming tx closed".to_owned())?;
                let manifest = to_line_manifest(to_line_slabs(slabs), styles);
                self.doc
                    .line
                    .borrow_mut()
                    .line
                    .finish_streaming_tx(tx, Some(&manifest))
                    .map(|_| ())
                    .map_err(|error| error.to_string())
            }
        }

        impl Drop for StoreStreamWriter {
            fn drop(&mut self) {
                if let Some(tx) = self.tx.take() {
                    if let Ok(mut guard) = self.doc.line.try_borrow_mut() {
                        let _ = guard.line.abort_streaming_tx(tx);
                    }
                }
            }
        }

        let tx = self
            .0
            .line
            .borrow_mut()
            .line
            .start_streaming_tx()
            .map_err(|error| error.to_string())?;
        Ok(Box::new(StoreStreamWriter {
            doc: self.0.clone(),
            tx: Some(tx),
        }))
    }
}

impl SegmentSource for DocumentSource {
    fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
        let line = self.0.line.borrow();
        let bytes = line
            .line
            .open_segment(segment.get())
            .map_err(|error| error.to_string())?;
        Ok(bytes.to_vec())
    }
}

/// The key schedule a test document uses: [`holonomy_seal::ParamsClass::Test`]'s costs, recorded
/// so that opening replays exactly what creation used.
///
/// # Panics
///
/// If the seal crate's test costs change shape — a test fixture that silently followed such a
/// change would hide a format move.
#[must_use]
pub fn test_params() -> KdfParamsRecord {
    let (m_kib, t, p) = holonomy_seal::ParamsClass::Test.costs();
    assert_eq!(
        (m_kib, t, p),
        (8_192, 1, 1),
        "the test key schedule moved; every fixture that records it must be revisited"
    );
    KdfParamsRecord { m_kib, t, p }
}

#[cfg(test)]
mod durability_error_tests {
    #[test]
    fn missing_durability_is_an_explicit_error_not_a_debug_build_panic() {
        let error = super::map_open(holonomy_line::LineError::UnsupportedDurability);
        match error {
            super::StoreError::Io(error) => {
                assert_eq!(error.kind(), std::io::ErrorKind::Unsupported)
            }
            _ => panic!("unsupported durability must remain a platform error"),
        }
    }
}
