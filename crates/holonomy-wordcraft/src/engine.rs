//! A narrow Wordcraft editing-engine adapter, not a window.
//!
//! Only explicitly selected editing commands are exposed here. In particular,
//! the upstream engine's general file, export, printing and recovery commands
//! are not made available through this API; they must not bypass the container.

use holonomy_cli::document::DocumentSessionState;
use holonomy_cli::store::SaveReport;
use serde_json::{json, Value};
use wordcraft_doc::Document;
use wordcraft_engine::Session;

use crate::editor::EditorSession;
use crate::BridgeError;

/// A supported editing command failed, without echoing its input.
#[derive(Debug)]
pub struct EditError;

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Wordcraft editing command failed")
    }
}

impl std::error::Error for EditError {}

/// A Wordcraft engine working on one container-backed document.
///
/// This is a limited integration prototype, not the Wordcraft UI. Wordcraft's
/// undo history, document, and intermediate text are not guaranteed to be wiped.
pub struct EngineEditor {
    editor: EditorSession,
    engine: Session,
}

impl EngineEditor {
    /// Attach the editing engine to a previously opened or created session.
    pub fn new(editor: EditorSession) -> Self {
        let engine = Session::new(editor.document().clone());
        Self { editor, engine }
    }

    /// Inspect the document currently held by the editing engine.
    pub fn document(&self) -> &Document {
        &self.engine.doc
    }

    /// Insert text at the current selection using Wordcraft's editing command.
    pub fn insert_text(&mut self, text: &str) -> Result<(), EditError> {
        self.run("text.insert", &json!({"text": text}))
    }

    /// Undo the most recent undoable Wordcraft edit, if there is one.
    pub fn undo(&mut self) -> Result<(), EditError> {
        self.run("edit.undo", &json!({}))
    }

    /// Redo the most recently undone Wordcraft edit, if there is one.
    pub fn redo(&mut self) -> Result<(), EditError> {
        self.run("edit.redo", &json!({}))
    }

    fn run(&mut self, command: &str, params: &Value) -> Result<(), EditError> {
        let result = self
            .engine
            .run(command, params)
            .map(|_| ())
            .map_err(|_| EditError);
        // Always resynchronize, including after a failed command. No file command
        // or arbitrary command ID is exposed by the public adapter.
        *self.editor.document_mut() = self.engine.doc.clone();
        result
    }

    /// Whether changes have not been saved or the disk state is uncertain.
    pub fn has_unsaved_changes(&self) -> bool {
        self.editor.has_unsaved_changes()
    }

    /// The last successfully opened or saved container cursor.
    pub fn cursor(&self) -> u64 {
        self.editor.cursor()
    }

    /// Whether a failed store operation requires recovery.
    pub fn state(&self) -> DocumentSessionState {
        self.editor.state()
    }

    /// Save through the encrypted container; unchanged documents spend no cursor.
    pub fn save(&mut self) -> Result<Option<SaveReport>, BridgeError> {
        self.editor.save()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use holonomy_cli::document::DocumentSessionOptions;
    use holonomy_cli::store::pad::DerivedPad;
    use holonomy_core::unlock::{SealedPassphrase, Topology};

    fn passphrase() -> SealedPassphrase {
        SealedPassphrase::new("test passphrase").expect("valid")
    }

    fn options(cursor: u64, writable: bool) -> DocumentSessionOptions {
        DocumentSessionOptions::new(cursor, writable, Topology::UmbilicalUsb)
    }

    #[test]
    fn editing_undo_redo_and_encrypted_reopen() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("document");
        let kappa = [0x45; 32];
        let session = EditorSession::create(
            &path,
            Box::new(DerivedPad::new()),
            passphrase(),
            kappa,
            options(0, true),
        )
        .expect("create");
        let mut editor = EngineEditor::new(session);
        assert!(!editor.has_unsaved_changes());
        editor.insert_text("Hello from Wordcraft").expect("insert");
        let edited = editor.document().clone();
        assert!(editor.has_unsaved_changes());
        editor.undo().expect("undo");
        assert!(!editor.has_unsaved_changes());
        assert!(editor.save().expect("unchanged").is_none());
        assert_eq!(editor.cursor(), 0);
        editor.redo().expect("redo");
        assert!(editor.document().same_content(&edited));
        assert!(editor.save().expect("save").is_some());
        assert_eq!(editor.cursor(), 1);
        drop(editor);
        let reopened = EditorSession::open(
            &path,
            Box::new(DerivedPad::new()),
            passphrase(),
            kappa,
            options(1, false),
        )
        .expect("reopen");
        assert!(reopened.document().same_content(&edited));
    }
}
