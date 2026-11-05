//! Document ownership and save state for an eventual Wordcraft frontend.
//!
//! This is not a window or an event loop. In particular it does not prevent
//! callers from closing with unsaved changes; a frontend must ask the user.

use std::path::Path;

use holonomy_cli::document::{DocumentSession, DocumentSessionOptions, DocumentSessionState};
use holonomy_cli::store::{PadSource, SaveReport};
use holonomy_core::unlock::SealedPassphrase;
use wordcraft_doc::Document;

use crate::{decode, save, BridgeError};

/// An open container and its editable Wordcraft document.
///
/// Wordcraft's model and the saved snapshot can contain plaintext and do not
/// promise to wipe their allocations. The underlying container session is not
/// safe for concurrent writers or for shared-pad accounting across containers.
pub struct EditorSession {
    session: DocumentSession,
    document: Document,
    saved: Document,
    writable: bool,
}

impl EditorSession {
    /// Provision and verify a new empty container, with a blank editor document.
    ///
    /// On failure the path may exist and must be inspected; provisioning is not
    /// atomic. Options must be writable and have cursor zero.
    pub fn create(
        path: &Path,
        pad: Box<dyn PadSource + Send>,
        passphrase: SealedPassphrase,
        kappa: [u8; 32],
        options: DocumentSessionOptions,
    ) -> Result<Self, BridgeError> {
        let writable = options.writable;
        let session = DocumentSession::create(path, pad, passphrase, kappa, options)
            .map_err(BridgeError::Session)?;
        Ok(Self::with_document(session, Document::new(), writable))
    }

    /// Open and decode a container, refusing unsupported or non-Wordcraft data.
    ///
    /// The cursor is supplied by the caller and authenticated by the store. The
    /// empty payload of a newly provisioned container opens as a blank document.
    pub fn open(
        path: &Path,
        pad: Box<dyn PadSource + Send>,
        passphrase: SealedPassphrase,
        kappa: [u8; 32],
        options: DocumentSessionOptions,
    ) -> Result<Self, BridgeError> {
        let writable = options.writable;
        let (session, plaintext) = DocumentSession::open(path, pad, passphrase, kappa, options)
            .map_err(BridgeError::Session)?;
        let document = decode(plaintext.as_slice())?;
        Ok(Self::with_document(session, document, writable))
    }

    fn with_document(session: DocumentSession, document: Document, writable: bool) -> Self {
        Self {
            session,
            saved: document.clone(),
            document,
            writable,
        }
    }

    /// Current Wordcraft document.
    pub fn document(&self) -> &Document {
        &self.document
    }

    /// Mutable Wordcraft document. Changes remain in memory until saved.
    ///
    /// This also works in read-only mode for local inspection or editing, but
    /// attempts to save changed content will fail until opened writable.
    pub fn document_mut(&mut self) -> &mut Document {
        &mut self.document
    }

    /// Whether the document has changed or the on-disk state is uncertain.
    ///
    /// Recovery-required sessions remain dirty even if edits are undone, because
    /// a failed save may already have replaced the on-disk image.
    pub fn has_unsaved_changes(&self) -> bool {
        self.state() != DocumentSessionState::Ready || !self.document.same_content(&self.saved)
    }

    /// Whether the underlying container was opened for writing.
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Cursor of the last successfully opened or saved image.
    pub fn cursor(&self) -> u64 {
        self.session.cursor()
    }

    /// Whether the underlying session requires recovery after a failed save.
    pub fn state(&self) -> DocumentSessionState {
        self.session.state()
    }

    /// Save changes, returning `None` without spending a cursor when unchanged.
    ///
    /// A failure after entering the store requires recovery, even if the user
    /// later undoes their edits. Serialization and preflight errors do not attempt
    /// a container write and leave the session ready.
    pub fn save(&mut self) -> Result<Option<SaveReport>, BridgeError> {
        if self.state() != DocumentSessionState::Ready {
            return Err(BridgeError::Save(
                "recovery required after a previous save failure".into(),
            ));
        }
        if !self.has_unsaved_changes() {
            return Ok(None);
        }
        let report = save(&mut self.session, &self.document)?;
        self.saved = self.document.clone();
        Ok(Some(report))
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use holonomy_cli::store::pad::{DerivedPad, PadWindow};
    use holonomy_core::unlock::Topology;

    fn options(cursor: u64, writable: bool) -> DocumentSessionOptions {
        DocumentSessionOptions::new(cursor, writable, Topology::UmbilicalUsb)
    }

    fn passphrase() -> SealedPassphrase {
        SealedPassphrase::new("test passphrase").expect("valid")
    }

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

    #[test]
    fn editor_round_trip_and_read_only_behavior() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("document");
        let kappa = [0x43; 32];
        let mut editor = EditorSession::create(
            &path,
            Box::new(DerivedPad::new()),
            passphrase(),
            kappa,
            options(0, true),
        )
        .expect("create");
        assert!(editor.is_writable());
        assert!(!editor.has_unsaved_changes());
        assert!(editor.save().expect("unchanged").is_none());
        assert_eq!(editor.cursor(), 0);
        *editor.document_mut() = Document::from_text("Hello, Wordcraft!\nSecond paragraph.");
        editor.document_mut().core.title = "A title".into();
        assert!(editor.has_unsaved_changes());
        let original_tab = editor.document().settings.default_tab;
        editor.document_mut().settings.default_tab = f32::NAN;
        assert!(editor.save().is_err());
        assert_eq!(editor.state(), DocumentSessionState::Ready);
        assert_eq!(editor.cursor(), 0);
        assert!(editor.has_unsaved_changes());
        editor.document_mut().settings.default_tab = original_tab;
        assert!(editor.save().expect("save").is_some());
        assert!(!editor.has_unsaved_changes());
        assert_eq!(editor.cursor(), 1);
        assert!(editor.save().expect("unchanged").is_none());
        let expected = editor.document().clone();
        drop(editor);

        let mut reopened = EditorSession::open(
            &path,
            Box::new(DerivedPad::new()),
            passphrase(),
            kappa,
            options(1, false),
        )
        .expect("open");
        assert!(!reopened.is_writable());
        assert!(reopened.document().same_content(&expected));
        assert!(!reopened.has_unsaved_changes());
        reopened.document_mut().core.title = "unsaved edit".into();
        assert!(reopened.save().is_err());
        assert!(reopened.has_unsaved_changes());
        assert_eq!(reopened.state(), DocumentSessionState::Ready);
        assert_eq!(reopened.cursor(), 1);
    }

    #[test]
    fn failed_save_stays_blocked_even_after_edits_are_undone() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("document");
        let kappa = [0x44; 32];
        let mut editor = EditorSession::create(
            &path,
            Box::new(FailedRatchet(DerivedPad::new())),
            passphrase(),
            kappa,
            options(0, true),
        )
        .expect("create");
        *editor.document_mut() = Document::from_text("new content");
        assert!(editor.save().is_err());
        assert_eq!(editor.state(), DocumentSessionState::RecoveryRequired);
        assert!(editor.has_unsaved_changes());
        assert_eq!(editor.cursor(), 0);
        *editor.document_mut() = Document::new();
        assert!(editor.has_unsaved_changes());
        assert!(editor.save().is_err());
        drop(editor);
        let reopened = EditorSession::open(
            &path,
            Box::new(DerivedPad::new()),
            passphrase(),
            kappa,
            options(1, false),
        )
        .expect("the new image was written before ratchet failed");
        assert!(reopened
            .document()
            .same_content(&Document::from_text("new content")));
    }
}
