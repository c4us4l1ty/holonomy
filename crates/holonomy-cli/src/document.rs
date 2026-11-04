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
    /// Key derivation settings; `Test` is for tests only.
    pub params: ParamsClass,
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
/// safe for concurrent writers or shared-pad accounting across containers.
pub struct DocumentSession {
    file: ContainerFile,
    pad: Box<dyn PadSource>,
    passphrase: SealedPassphrase,
    kappa: Zeroizing<[u8; 32]>,
    options: DocumentSessionOptions,
    state: DocumentSessionState,
}

impl DocumentSession {
    /// Open an existing container and return both the session and its plaintext.
    ///
    /// The caller must provide the cursor which wrote the current image. The
    /// container's authenticated cursor is checked by the underlying store.
    pub fn open(
        path: &Path,
        mut pad: Box<dyn PadSource>,
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
        let (_, document) = store::open_document(&mut file, pad.as_mut(), &context)?;
        Ok((
            Self {
                file,
                pad,
                passphrase,
                kappa,
                options,
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
    /// On any store failure, the session enters `RecoveryRequired`, even if the
    /// failure happened before writing. The store can also fail after replacement
    /// or during ratcheting; do not assume that the old cursor is still current.
    pub fn save(&mut self, document: &[u8]) -> Result<SaveReport, String> {
        if self.state != DocumentSessionState::Ready {
            return Err(
                "save blocked: reopen and verify the container and pad after the previous failure".into(),
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
        let context = SaveContext {
            passphrase: &self.passphrase,
            kappa_usb: &self.kappa,
            topology: self.options.topology,
            params: self.options.params,
            cursor,
        };
        match store::save(&mut self.file, self.pad.as_mut(), &context, document) {
            Ok(report) => {
                self.options.cursor = cursor;
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
    fn failed_ratchet_requires_recovery_even_after_image_was_replaced() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("container");
        let kappa = [0x42; 32];
        let passphrase = SealedPassphrase::new("test passphrase").expect("valid");
        let context = SaveContext {
            passphrase: &passphrase,
            kappa_usb: &kappa,
            topology: Topology::UmbilicalUsb,
            params: ParamsClass::Test,
            cursor: 0,
        };
        store::provision(&path, &mut DerivedPad::new(), &context).expect("provision");
        let (mut session, document) = DocumentSession::open(
            &path,
            Box::new(FailedRatchet(DerivedPad::new())),
            passphrase,
            kappa,
            options(0, true),
        )
        .expect("open");
        assert!(document.as_slice().is_empty());
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
