//! A conservative document session for frontends using the experimental store.
//!
//! This is not an editor and does not implement USB provisioning or durable pad
//! accounting. In particular, a failed save can happen after replacement, so the
//! session refuses further saves until the caller explicitly reopens and recovers.

use std::path::Path;

use holonomy_core::unlock::{SealedPassphrase, Topology};
use holonomy_crypto::ParamsClass;
use holonomy_session::seal::Zeroizing as Plaintext;
use zeroize::Zeroizing;

use crate::store::{self, ContainerFile, PadSource, SaveContext, SaveReport};

/// Parameters for opening a document session.
pub struct DocumentSessionOptions {
    /// Cursor used to write the container that is currently on disk.
    pub cursor: u64,
    /// Whether the container file should be opened for saving.
    pub writable: bool,
    /// Transport represented by the supplied root share.
    pub topology: Topology,
    // Private so production frontends cannot select the weak test KDF profile.
    params: ParamsClass,
}

impl DocumentSessionOptions {
    /// Construct options using the production Argon2id parameter profile.
    pub fn new(cursor: u64, writable: bool, topology: Topology) -> Self {
        Self {
            cursor,
            writable,
            topology,
            params: ParamsClass::Legacy,
        }
    }

    /// The Argon2id profile used for this session.
    pub fn params(&self) -> ParamsClass {
        self.params
    }
}

/// Whether a session may initiate another save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentSessionState {
    /// The last open or save succeeded.
    Ready,
    /// A save failed and may have modified the container or pad.
    RecoveryRequired,
}

/// Owns one open container and tracks its cursor for a frontend.
///
/// This type does not retain the document itself. The returned plaintext buffer is
/// wiped on drop, but callers must manage any copies they make. The session is not
/// safe for concurrent writers or shared-pad accounting across containers. The
/// pad source must be transferable to a worker thread (`Send`); this does not
/// make the session safe for simultaneous use from multiple threads.
pub struct DocumentSession {
    file: ContainerFile,
    pad: Box<dyn PadSource + Send>,
    passphrase: SealedPassphrase,
    kappa: Zeroizing<[u8; 32]>,
    options: DocumentSessionOptions,
    last_image_digest: [u8; 32],
    state: DocumentSessionState,
}

impl DocumentSession {
    /// Create a new empty container and verify it can be opened.
    ///
    /// Creation requires a writable session at cursor zero. If creation or the
    /// verification step fails, the path may contain an incomplete container; it
    /// is not removed automatically. USB provisioning and pad history are not
    /// handled here, and a reused pad cannot be detected by this method.
    pub fn create(
        path: &Path,
        mut pad: Box<dyn PadSource + Send>,
        passphrase: SealedPassphrase,
        kappa: [u8; 32],
        options: DocumentSessionOptions,
    ) -> Result<Self, String> {
        if !options.writable || options.cursor != 0 {
            return Err("creating a session requires writable mode and cursor zero".into());
        }
        if !cfg!(unix) {
            return Err("creating an editable session is unavailable on this platform".into());
        }
        let kappa = Zeroizing::new(kappa);
        let context = SaveContext {
            passphrase: &passphrase,
            kappa_usb: &kappa,
            topology: options.topology,
            params: options.params,
            cursor: 0,
        };
        store::provision(path, pad.as_mut(), &context).map_err(|e| {
            format!("creation did not complete; the path may exist and must be inspected: {e}")
        })?;
        let (session, document) =
            Self::open(path, pad, passphrase, *kappa, options).map_err(|e| {
                format!("the container was created but could not be verified; inspect it before use: {e}")
            })?;
        if !document.as_slice().is_empty() {
            return Err(
                "the created container did not open as an empty document; inspect it before use"
                    .into(),
            );
        }
        Ok(session)
    }

    /// Open an existing container and return both the session and its plaintext.
    ///
    /// The caller must provide the cursor which wrote the current image. The
    /// container's authenticated cursor is checked by the underlying store.
    pub fn open(
        path: &Path,
        mut pad: Box<dyn PadSource + Send>,
        passphrase: SealedPassphrase,
        kappa: [u8; 32],
        options: DocumentSessionOptions,
    ) -> Result<(Self, Plaintext), String> {
        let kappa = Zeroizing::new(kappa);
        let mut file = ContainerFile::open(path, options.writable)
            .map_err(|e| format!("opening {}: {e}", path.display()))?;
        let context = SaveContext {
            passphrase: &passphrase,
            kappa_usb: &kappa,
            topology: options.topology,
            params: options.params,
            cursor: options.cursor,
        };
        let (_, document, last_image_digest) =
            store::open_document_with_digest(&mut file, pad.as_mut(), &context)?;
        Ok((
            Self {
                file,
                pad,
                passphrase,
                kappa,
                options,
                last_image_digest,
                state: DocumentSessionState::Ready,
            },
            document,
        ))
    }

    /// The cursor used to write the last successfully opened or saved image.
    pub fn cursor(&self) -> u64 {
        self.options.cursor
    }

    /// Whether another save is allowed without recovery.
    pub fn state(&self) -> DocumentSessionState {
        self.state
    }

    /// Replace the document using the next cursor.
    ///
    /// Preflight rejection for a read-only session, an oversized document or an invalid
    /// next cursor performs no store I/O and leaves the session ready. On any store
    /// failure, the session enters `RecoveryRequired`, even if it happened before writing. The store can also fail after replacement
    /// or during ratcheting; do not assume that the old cursor is still current.
    pub fn save(&mut self, document: &[u8]) -> Result<SaveReport, String> {
        if self.state != DocumentSessionState::Ready {
            return Err(
                "save blocked: reopen and verify the container and pad after the previous failure"
                    .into(),
            );
        }
        if !self.options.writable {
            return Err("session is read-only".into());
        }
        let cursor = self
            .options
            .cursor
            .checked_add(1)
            .ok_or("save cursor overflow")?;
        // These deterministic checks cannot write the image or ratchet the pad.
        // Keep the session usable if they reject a save. Once the store is entered,
        // any error remains conservatively recovery-required.
        store::validate_payload_size(document.len())?;
        store::validate_save_cursor(cursor)?;
        let context = SaveContext {
            passphrase: &self.passphrase,
            kappa_usb: &self.kappa,
            topology: self.options.topology,
            params: self.options.params,
            cursor,
        };
        // Take the store's save lock here, so contention is a refusal that writes nothing and
        // leaves the session usable, instead of a failure discovered mid-save.
        #[cfg(unix)]
        let lock = match store::SaveLock::acquire(self.file.path()) {
            Ok(lock) => lock,
            Err(error) => {
                return Err(format!(
                    "save refused before any read or write; the session is still usable: {error}"
                ));
            }
        };
        #[cfg(unix)]
        let outcome = store::save_if_unchanged_locked(
            &mut self.file,
            self.pad.as_mut(),
            &context,
            document,
            &self.last_image_digest,
            &lock,
        );
        #[cfg(not(unix))]
        let outcome = store::save_if_unchanged(
            &mut self.file,
            self.pad.as_mut(),
            &context,
            document,
            &self.last_image_digest,
        );
        match outcome {
            Ok(report) => {
                self.options.cursor = cursor;
                self.last_image_digest = report.digest;
                Ok(report)
            }
            Err(error) => {
                self.state = DocumentSessionState::RecoveryRequired;
                Err(format!(
                    "save failed; container or pad may have changed, so reopen and verify before continuing: {error}"
                ))
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::store::pad::{DerivedPad, PadWindow};

    struct FailedRatchet(DerivedPad);

    impl PadSource for FailedRatchet {
        fn window(&mut self, kappa: &[u8; 32], cursor: u64) -> Result<PadWindow, String> {
            self.0.window(kappa, cursor)
        }

        fn ratchet(&mut self, _kappa: &[u8; 32], _cursor: u64) -> Result<u32, String> {
            Err("injected ratchet failure".into())
        }

        fn can_ratchet(&self) -> bool {
            true
        }

        fn describe(&self) -> String {
            "test pad".into()
        }
    }

    fn options(cursor: u64, writable: bool) -> DocumentSessionOptions {
        DocumentSessionOptions {
            cursor,
            writable,
            topology: Topology::UmbilicalUsb,
            params: ParamsClass::Test,
        }
    }

    #[test]
    fn preflight_rejections_do_not_touch_file_or_require_recovery() {
        struct NeverReadPad;
        impl PadSource for NeverReadPad {
            fn window(&mut self, _kappa: &[u8; 32], _cursor: u64) -> Result<PadWindow, String> {
                panic!("preflight must not read pad")
            }
            fn ratchet(&mut self, _kappa: &[u8; 32], _cursor: u64) -> Result<u32, String> {
                panic!("preflight must not ratchet")
            }
            fn can_ratchet(&self) -> bool {
                panic!("preflight must not access pad")
            }
            fn describe(&self) -> String {
                "test pad".into()
            }
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("container");
        std::fs::write(&path, b"untouched").expect("create sentinel");
        let mut session = DocumentSession {
            file: ContainerFile::open(&path, true).expect("open"),
            pad: Box::new(NeverReadPad),
            passphrase: SealedPassphrase::new("test").expect("passphrase"),
            kappa: Zeroizing::new([0; 32]),
            options: options(0, true),
            last_image_digest: [0; 32],
            state: DocumentSessionState::Ready,
        };
        let capacity = holonomy_core::layout::PAYLOAD_SLOTS
            * holonomy_session::seal::PAYLOAD_PLAINTEXT_PER_SLOT;
        let oversized = vec![0u8; capacity + 1];
        assert!(session
            .save(&oversized)
            .expect_err("size refused")
            .contains("payload limit"));
        assert_eq!(session.state(), DocumentSessionState::Ready);
        assert_eq!(session.cursor(), 0);
        drop(oversized);
        session.options.cursor = holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE - 1;
        assert!(session
            .save(b"edit")
            .expect_err("cursor refused")
            .contains("pad reuse"));
        assert_eq!(session.state(), DocumentSessionState::Ready);
        assert_eq!(
            session.cursor(),
            holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE - 1
        );
        session.options.cursor = u64::MAX;
        assert!(session
            .save(b"edit")
            .expect_err("overflow refused")
            .contains("overflow"));
        assert_eq!(session.state(), DocumentSessionState::Ready);
        assert_eq!(std::fs::read(&path).expect("read sentinel"), b"untouched");
    }

    #[test]
    fn session_refuses_image_changed_since_open_before_pad_access() {
        use std::os::unix::fs::MetadataExt;
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        struct CountingPad {
            inner: DerivedPad,
            reads: Arc<AtomicUsize>,
        }
        impl PadSource for CountingPad {
            fn window(&mut self, kappa: &[u8; 32], cursor: u64) -> Result<PadWindow, String> {
                self.reads.fetch_add(1, Ordering::SeqCst);
                self.inner.window(kappa, cursor)
            }
            fn ratchet(&mut self, _kappa: &[u8; 32], _cursor: u64) -> Result<u32, String> {
                panic!("changed snapshot must not be ratcheted")
            }
            fn can_ratchet(&self) -> bool {
                false
            }
            fn describe(&self) -> String {
                "counting demo pad".into()
            }
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("first");
        let other_path = dir.path().join("second");
        let kappa = [0x51; 32];
        let pass = || SealedPassphrase::new("test passphrase").expect("passphrase");
        let reads = Arc::new(AtomicUsize::new(0));
        let mut session = DocumentSession::create(
            &path,
            Box::new(CountingPad {
                inner: DerivedPad::new(),
                reads: Arc::clone(&reads),
            }),
            pass(),
            kappa,
            options(0, true),
        )
        .expect("create first");
        let other = DocumentSession::create(
            &other_path,
            Box::new(DerivedPad::new()),
            pass(),
            kappa,
            options(0, true),
        )
        .expect("create another valid cursor-zero image");
        let other_digest = other.last_image_digest;
        assert_ne!(session.last_image_digest, other_digest);
        drop(other);
        let inode = std::fs::metadata(&path).expect("metadata").ino();
        let mut src = std::fs::File::open(&other_path).expect("open second");
        let mut dest = std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open first");
        assert_eq!(
            std::io::copy(&mut src, &mut dest).expect("copy"),
            holonomy_core::layout::CONTAINER_BYTES as u64
        );
        dest.sync_all().expect("sync");
        drop(dest);
        assert_eq!(std::fs::metadata(&path).expect("metadata").ino(), inode);
        let before_reads = reads.load(Ordering::SeqCst);
        let error = session
            .save(b"my edited document")
            .expect_err("changed image refused");
        assert!(error.contains("changed since this session"), "{error}");
        assert_eq!(
            reads.load(Ordering::SeqCst),
            before_reads,
            "do not read or ratchet pad"
        );
        assert_eq!(session.state(), DocumentSessionState::RecoveryRequired);
        assert_eq!(session.cursor(), 0);
        assert!(session.save(b"retry").is_err());
        let mut image = ContainerFile::open(&path, false).expect("inspect");
        assert_eq!(
            holonomy_session::seal::image_digest(&image.read_image().expect("read")),
            other_digest
        );
        // The save took the advisory lock, so its sidecar exists next to the two containers.
        // It carries coordination state only: it is empty and holds no key material.
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .expect("list")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "first".to_string(),
                "first.holonomy-lock".to_string(),
                "second".to_string()
            ]
        );
        assert_eq!(
            std::fs::metadata(dir.path().join("first.holonomy-lock"))
                .expect("lock metadata")
                .len(),
            0,
            "the save lock must not record document bytes"
        );
    }

    #[test]
    fn session_updates_baseline_after_each_successful_save() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("document");
        let kappa = [0x52; 32];
        let pass = || SealedPassphrase::new("test passphrase").expect("passphrase");
        let mut session = DocumentSession::create(
            &path,
            Box::new(DerivedPad::new()),
            pass(),
            kappa,
            options(0, true),
        )
        .expect("create");
        let first = session.save(b"first").expect("first save");
        assert_eq!(session.last_image_digest, first.digest);
        let second = session.save(b"second").expect("second save");
        assert_eq!(session.last_image_digest, second.digest);
        assert_eq!(session.cursor(), 2);
        drop(session);
        let (_, document) = DocumentSession::open(
            &path,
            Box::new(DerivedPad::new()),
            pass(),
            kappa,
            options(2, false),
        )
        .expect("reopen");
        assert_eq!(document.as_slice(), b"second");
    }

    #[test]
    fn create_rejects_invalid_options_without_creating_a_file() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("container");
        for opts in [options(1, true), options(0, false)] {
            let result = DocumentSession::create(
                &path,
                Box::new(DerivedPad::new()),
                SealedPassphrase::new("test").expect("valid"),
                [0x42; 32],
                opts,
            );
            assert!(result.is_err());
            assert!(!path.exists());
        }
    }

    #[test]
    fn a_concurrent_save_is_refused_without_requiring_recovery() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("container");
        let kappa = [0x54; 32];
        let pass = || SealedPassphrase::new("test passphrase").expect("passphrase");
        let mut session = DocumentSession::create(
            &path,
            Box::new(DerivedPad::new()),
            pass(),
            kappa,
            options(0, true),
        )
        .expect("create");

        let lock = crate::store::SaveLock::acquire(&path).expect("take the save lock");
        let error = session
            .save(b"blocked by the lock")
            .expect_err("a held lock must refuse the save");
        assert!(error.contains("session is still usable"), "{error}");
        assert!(error.contains("another Holonomy save holds"), "{error}");
        assert_eq!(session.state(), DocumentSessionState::Ready);
        assert_eq!(session.cursor(), 0);

        drop(lock);
        session.save(b"allowed after release").expect("save");
        assert_eq!(session.cursor(), 1);
        assert_eq!(session.state(), DocumentSessionState::Ready);
    }

    #[test]
    fn partial_ratchet_requires_recovery_after_replacement() {
        struct PartialRatchet(DerivedPad);
        impl PadSource for PartialRatchet {
            fn window(&mut self, kappa: &[u8; 32], cursor: u64) -> Result<PadWindow, String> {
                self.0.window(kappa, cursor)
            }
            fn ratchet(&mut self, _kappa: &[u8; 32], cursor: u64) -> Result<u32, String> {
                assert_eq!(cursor, 0, "only the previous window can be ratcheted");
                Ok(holonomy_core::layout::PAD_SECTORS_PER_SAVE - 1)
            }
            fn can_ratchet(&self) -> bool {
                true
            }
            fn describe(&self) -> String {
                "partial test ratchet".into()
            }
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("container");
        let kappa = [0x53; 32];
        let pass = || SealedPassphrase::new("test passphrase").expect("passphrase");
        let mut session = DocumentSession::create(
            &path,
            Box::new(PartialRatchet(DerivedPad::new())),
            pass(),
            kappa,
            options(0, true),
        )
        .expect("create");
        let error = session
            .save(b"new document")
            .expect_err("partial ratchet must fail");
        assert!(
            error.contains("container was written with cursor 1"),
            "{error}"
        );
        assert!(error.contains("32767 of 32768"), "{error}");
        assert!(error.contains("Do not retry"), "{error}");
        assert_eq!(session.state(), DocumentSessionState::RecoveryRequired);
        assert_eq!(
            session.cursor(),
            0,
            "only the last confirmed cursor is retained"
        );
        assert!(session.save(b"retry").is_err());
        drop(session);
        // The injected pad does not actually change sectors; this proves the image
        // was replaced, not that a real partially ratcheted device is recoverable.
        let (reopened, document) = DocumentSession::open(
            &path,
            Box::new(DerivedPad::new()),
            pass(),
            kappa,
            options(1, false),
        )
        .expect("test image was written at cursor one");
        assert_eq!(reopened.cursor(), 1);
        assert_eq!(document.as_slice(), b"new document");
    }

    #[test]
    fn failed_ratchet_requires_recovery_even_after_image_was_replaced() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("container");
        let kappa = [0x42; 32];
        let passphrase = SealedPassphrase::new("test passphrase").expect("valid");
        let mut session = DocumentSession::create(
            &path,
            Box::new(FailedRatchet(DerivedPad::new())),
            passphrase,
            kappa,
            options(0, true),
        )
        .expect("create and verify");
        assert!(session.save(b"new document").is_err());
        assert_eq!(session.state(), DocumentSessionState::RecoveryRequired);
        assert_eq!(session.cursor(), 0);
        assert!(session.save(b"must not write").is_err());
        drop(session);
        let (reopened, document) = DocumentSession::open(
            &path,
            Box::new(DerivedPad::new()),
            SealedPassphrase::new("test passphrase").expect("valid"),
            kappa,
            options(1, false),
        )
        .expect("new image was written at cursor 1");
        assert_eq!(reopened.cursor(), 1);
        assert_eq!(document.as_slice(), b"new document");
    }
}
