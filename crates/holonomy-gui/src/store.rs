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
//! # The leaked key, said plainly
//!
//! [`WorldLine`] borrows its `DEMKey`, so a struct that owns both would be self-referential. Rather
//! than reach for `unsafe` or a self-reference crate, the key is leaked once per opened document:
//! thirty-two bytes, for as long as the process lives — which is exactly how long the window would
//! have held it anyway. The cost is that the key's `Drop` never runs, so it is not zeroized at exit.
//! An owning `WorldLine` would remove both the leak and that sentence; it is recorded as OQ-27.

use std::cell::RefCell;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use holonomy_line::{LineError, LineHeader, WorldLine};
use holonomy_seal::{
    calibrate, create_capsule_for, generate_kdf_salt, open_capsule_for, DEMKey, KdfParamsRecord,
    MasterKeys,
};
use holonomy_text::{Appended, SegmentId, SegmentSink, SegmentSource, Slab, Spine};

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
        other => {
            debug_assert!(
                !matches!(other, LineError::UnsupportedDurability),
                "a world-line needs a file system with working barriers"
            );
            StoreError::Refused
        }
    }
}

/// An opened world-line and the geometry it presents to a window.
#[derive(Clone)]
pub struct Document {
    line: Rc<RefCell<Line>>,
}

struct Line {
    path: PathBuf,
    line: WorldLine<'static>,
    /// The key the world-line borrows; see the module note on why it is leaked.
    #[allow(dead_code)]
    key: &'static DEMKey,
}

impl Document {
    /// Creates a document, calibrating this host's Argon2 schedule first.
    ///
    /// # Errors
    ///
    /// As [`create_with`], plus whatever the calibration refuses.
    pub fn create(path: &Path, passphrase: &[u8]) -> Result<Self, StoreError> {
        let calibration = calibrate();
        Self::create_with(path, passphrase, calibration.params)
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
    /// [`StoreError::Params`] if the schedule is refused.
    pub fn create_with(
        path: &Path,
        passphrase: &[u8],
        params: KdfParamsRecord,
    ) -> Result<Self, StoreError> {
        let salt = generate_kdf_salt().map_err(|_| StoreError::Params)?;
        let keys = MasterKeys::derive_with(passphrase, &salt, None, &params)
            .map_err(|_| StoreError::Params)?;
        let (capsule, dem) =
            create_capsule_for(keys.unwrap_key(), None).map_err(|_| StoreError::Params)?;
        let header = LineHeader::new(params, salt, capsule)
            .map_err(|error| StoreError::Line(error.to_string()))?;
        let key: &'static DEMKey = Box::leak(Box::new(dem));
        let line = WorldLine::create(path, header, key).map_err(map_open)?;
        Ok(Self {
            line: Rc::new(RefCell::new(Line {
                path: path.to_path_buf(),
                line,
                key,
            })),
        })
    }

    /// Opens an existing document.
    ///
    /// Reads the public header only, replays the recorded key schedule against the passphrase, and
    /// asks the capsule for the DEM key before the world-line authenticates a byte.
    ///
    /// # Errors
    ///
    /// [`StoreError::Refused`] for a wrong passphrase or an altered file — one answer for both,
    /// on purpose — and [`StoreError::Io`] if the file cannot be read.
    pub fn open(path: &Path, passphrase: &[u8]) -> Result<Self, StoreError> {
        let header = LineHeader::read(path).map_err(map_open)?;
        let keys = MasterKeys::derive_with(passphrase, &header.salt, None, &header.kdf_params)
            .map_err(|_| StoreError::Refused)?;
        let dem = open_capsule_for(keys.unwrap_key(), &header.capsule, None)
            .map_err(|_| StoreError::Refused)?;
        let key: &'static DEMKey = Box::leak(Box::new(dem));
        let line = WorldLine::open(path, key).map_err(map_open)?;
        Ok(Self {
            line: Rc::new(RefCell::new(Line {
                path: path.to_path_buf(),
                line,
                key,
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

    /// The document's geometry: one slab per committed segment, its length taken from the
    /// authenticated record header. No plaintext is read.
    ///
    /// Read [`spine_of`]'s note before relying on this: the geometry is *file* order, which is
    /// document order only for a document that was never edited before its end (OQ-29).
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
}

/// The spine a world-line presents: one slab per segment, **in file order**.
///
/// That is not the same as document order, and the difference is a defect (OQ-29, D-75). A segment
/// record carries its bytes and its authenticated length and nothing else — no position, no edit,
/// no tombstone — so the geometry a document had when it was being edited is not in the file
/// to be read back. Building the spine from `segment_lengths()` in append order is therefore the
/// only thing this function *can* do, and it makes a document that was only ever appended to load
/// exactly right, which is every document the existing tests wrote.
///
/// It does not make an edited document load right: type `world`, then put the caret at 0 and type
/// `hello `, and the session reads `hello world` while this reloads `worldhello `. The tests in
/// `holonomy-gui/tests/persist.rs` assert that, so the day a structural record exists the change is
/// visible rather than silent. Do not "fix" this here — the missing information is in the format.
fn spine_of(line: &WorldLine<'_>) -> Spine {
    Spine::from_slabs(
        line.document_order()
            .into_iter()
            .map(|(seq, length)| {
                Slab::new(
                    SegmentId::new(u64::from(seq)),
                    0,
                    length,
                )
            })
            .collect::<Vec<_>>(),
    )
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
