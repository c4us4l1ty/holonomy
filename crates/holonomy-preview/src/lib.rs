//! Shared pieces for the preview binaries.
//!
//! The point of this crate is a **read-only** Wordcraft view of a Holonomy container: the document
//! is decoded once, handed to Wordcraft's renderer, and the window is made incapable of editing or
//! persisting it. Two boundaries do that work, and neither is a claim about Wordcraft's internals:
//!
//! * every host service Wordcraft can reach is replaced by one that refuses, so a file dialog,
//!   download, save, print or clipboard write fails loudly instead of succeeding quietly;
//! * the raw input stream is filtered down to pointer motion, scrolling and focus. No key, text,
//!   paste or button event ever reaches the app, so no command that could edit, export or copy can
//!   be invoked from the window.
//!
//! Both are tested here. Neither is a sandbox: the decoded document is plaintext in this process,
//! the renderer may allocate copies of it, and the operating system can expose them.

use std::path::Path;
use std::sync::mpsc;

use eframe::egui;
use holonomy_cli::document::DocumentSessionOptions;
use holonomy_cli::store::pad::DerivedPad;
use holonomy_core::unlock::{SealedPassphrase, Topology};
use holonomy_wordcraft::editor::EditorSession;
use wordcraft_doc::Document;
use wordcraft_ui_egui::{DefaultApp, FileDialogAnswer, FileDialogRequest, Services};
use zeroize::Zeroizing;

/// The sentence a viewer must show, and the sentence its tests assert.
pub const READ_ONLY_RULE: &str = "READ-ONLY VIEW — editing, save, save-as, open, export, print, download and clipboard writes are disabled. The decrypted document is plaintext in this process; memory, swap and OS copies may expose it.";

/// Why a read-only window is not a sandbox, stated once for the banner and the docs.
pub const READ_ONLY_LIMITS: &str = "Rendering is Wordcraft's own. Formatting Wordcraft cannot represent may not appear, and nothing here proves the container is fresh, undamaged or recovered.";

/// Parse the 64-hex-character root share.
pub fn parse_kappa(hex: &str) -> Result<[u8; 32], &'static str> {
    if hex.len() != 64 || !hex.is_ascii() {
        return Err("root share must be exactly 64 ASCII hex characters");
    }
    let mut bytes = Zeroizing::new([0u8; 32]);
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    };
    for (index, pair) in hex.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        bytes[index] = (digit(pair[0]).ok_or("root share contains non-hex characters")? << 4)
            | digit(pair[1]).ok_or("root share contains non-hex characters")?;
    }
    Ok(*bytes)
}

/// Read one secret from the terminal, optionally confirmed.
pub fn secret(prompt: &str, confirm: bool) -> Result<Zeroizing<String>, String> {
    let value = Zeroizing::new(
        rpassword::prompt_password(prompt).map_err(|e| format!("prompt failed: {e}"))?,
    );
    if confirm {
        let again = Zeroizing::new(
            rpassword::prompt_password("Confirm: ").map_err(|e| format!("prompt failed: {e}"))?,
        );
        if *value != *again {
            return Err("confirmation did not match".into());
        }
    }
    Ok(value)
}

/// Ask for the two secrets a container unlock needs.
///
/// Prompts are hidden and read from the terminal, so a value never appears in a process listing or
/// shell history. The passphrase mapping is still the legacy lowercase one; the caller says so.
pub fn prompt_credentials(confirm: bool) -> Result<(SealedPassphrase, [u8; 32]), String> {
    announce_credentials();
    let passphrase = secret("Passphrase: ", confirm)?;
    let share = secret("Demo root share (64 hex characters): ", confirm)?;
    let kappa = parse_kappa(&share).map_err(str::to_owned)?;
    let sealed = SealedPassphrase::new(&passphrase).ok_or("invalid passphrase length")?;
    Ok((sealed, kappa))
}

fn announce_credentials() {
    eprintln!(
        "The legacy passphrase mapping lowercases input; ASCII case variants are equivalent."
    );
}

/// A container opened for viewing: the decoded document and the cursor that opened it.
pub struct OpenedView {
    /// Wordcraft's model. Plaintext; it is not wiped on drop.
    pub document: Document,
    /// The cursor the caller supplied, authenticated by the store.
    pub cursor: u64,
}

/// Open a container **read-only** and decode its Wordcraft document.
///
/// The session is opened with `writable = false`, its document is cloned, and the session handle is
/// dropped before returning: the viewer never holds a writable container handle, so there is no
/// save path to reach. The deterministic demo pad stands in for physical USB; there is no device
/// path in this function.
pub fn open_read_only(
    path: &Path,
    cursor: u64,
    passphrase: SealedPassphrase,
    kappa: [u8; 32],
) -> Result<OpenedView, String> {
    let options = DocumentSessionOptions::new(cursor, false, Topology::UmbilicalUsb);
    let session = EditorSession::open(
        path,
        Box::new(DerivedPad::new()),
        passphrase,
        kappa,
        options,
    )
    .map_err(|error| error.to_string())?;
    let cursor = session.cursor();
    let document = session.document().clone();
    drop(session);
    Ok(OpenedView { document, cursor })
}

/// Host services in which every persistence, export and clipboard-write path refuses.
///
/// `None` alone would be ambiguous: several Wordcraft paths fall back to another destination when a
/// hook is missing. Every hook that could write is therefore `Some`, and it says no.
pub fn read_only_services() -> Services {
    Services {
        // Printing needs discovery and submission workers; leaving both absent means no printer
        // path exists at all.
        printers: None,
        print_native: None,
        // Picking is refused rather than left to the built-in file browser, which would let the
        // window read unrelated host files.
        pick_open: Some(Box::new(|_| None)),
        pick_save: Some(Box::new(|_| None)),
        file_dialog: Some(Box::new(|_request: FileDialogRequest| {
            // A dropped sender counts as "the user cancelled", the same answer the app gives when
            // a dialog is dismissed, and it cannot arrive late with a path.
            let (_sender, receiver) = mpsc::channel::<FileDialogAnswer>();
            receiver
        })),
        open_async: None,
        inbox: None,
        pasted: None,
        download: Some(Box::new(|_, _| {
            Err("read-only view: downloads are disabled".to_string())
        })),
        async_saves: None,
        host_save: Some(Box::new(|_, _, _| {
            Err("read-only view: saving is disabled".to_string())
        })),
        // The dirty flag is reported after each pass; ignoring it means the browser's leave-page
        // guard never claims unsaved work, because there is none.
        on_dirty: Some(Box::new(|_| {})),
        print: Some(Box::new(|_| {
            Err("read-only view: printing is disabled".to_string())
        })),
        // Paste cannot be invoked (input is filtered), and leaving the clipboard unreadable means a
        // future path cannot pull host text into the window either.
        clipboard_picture: None,
        animations_enabled: Some(Box::new(|| Some(false))),
        clipboard_text: Some(Box::new(|| None)),
        clipboard_html: Some(Box::new(|| None)),
        set_clipboard_html: Some(Box::new(|_, _, _| false)),
        clipboard_rtf: Some(Box::new(|| None)),
        clipboard_formats: Some(Box::new(Vec::new)),
        // Fonts are host state: accept nothing, write nothing.
        store_font: Some(Box::new(|_, _| {})),
        forget_font: Some(Box::new(|_| {})),
        load_system_fonts: None,
        make_default_app: Some(Box::new(|| -> Result<DefaultApp, String> {
            Err("read-only view: file associations are disabled".to_string())
        })),
        templates_dir: None,
    }
}

/// What the input filter did with one frame.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ViewInput {
    /// The user asked to quit with Ctrl+Q (or Cmd+Q). The app never sees that chord.
    pub quit_requested: bool,
    /// How many events were discarded. Never printed per frame; useful in tests and diagnostics.
    pub dropped: usize,
}

/// Reduce a frame's input to viewing only.
///
/// Kept: pointer motion, wheel scrolling, pinch zoom and window focus. Everything else -- keys,
/// typed text, paste, copy, cut, IME, pre-edit and every pointer button -- is dropped, so no click
/// can press a ribbon button and no chord can reach a command. Ctrl+Q is honoured here, in the
/// host's own filter, because a keyboard-inert window still has to be closable.
pub fn filter_events(raw: &mut egui::RawInput) -> ViewInput {
    let mut outcome = ViewInput::default();
    let total = raw.events.len();
    let mut kept = Vec::with_capacity(total);
    for event in std::mem::take(&mut raw.events) {
        match event {
            egui::Event::Key {
                key: egui::Key::Q,
                pressed: true,
                modifiers,
                ..
            } if modifiers.command || modifiers.ctrl => outcome.quit_requested = true,
            egui::Event::PointerMoved(_)
            | egui::Event::MouseWheel { .. }
            | egui::Event::Zoom(_)
            | egui::Event::WindowFocused(_) => kept.push(event),
            _ => {}
        }
    }
    outcome.dropped = total - kept.len();
    raw.events = kept;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use holonomy_wordcraft::BridgeError;
    use wordcraft_doc::Block;

    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn click() -> egui::Event {
        egui::Event::PointerButton {
            pos: egui::pos2(1.0, 2.0),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }
    }

    fn wheel() -> egui::Event {
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -60.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
            source: egui::MouseWheelSource::Unknown,
        }
    }

    /// The services boundary: nothing that could leave the process may say yes.
    #[test]
    fn read_only_services_refuse_every_persistence_path() {
        let services = read_only_services();

        assert_eq!(
            (services.pick_save.expect("pick_save hook"))("document.docx"),
            None
        );
        assert_eq!(
            (services.pick_open.expect("pick_open hook"))("document"),
            None
        );
        let receiver = (services.file_dialog.expect("dialog hook"))(FileDialogRequest::Open {
            purpose: "document".into(),
        });
        assert!(
            receiver.try_recv().is_err(),
            "a dropped dialog sender must read as cancelled"
        );

        let download = (services.download.expect("download hook"))("x.docx", b"bytes");
        assert!(download.unwrap_err().contains("read-only view"));
        let save = (services.host_save.expect("host_save hook"))("x.docx", b"bytes", false);
        assert!(save.unwrap_err().contains("read-only view"));
        let print = (services.print.expect("print hook"))(b"pdf");
        assert!(print.unwrap_err().contains("read-only view"));
        let default_app = (services.make_default_app.expect("default hook"))();
        assert!(default_app.is_err());

        assert!(services.printers.is_none(), "no printer discovery");
        assert!(services.print_native.is_none(), "no print submission");
        assert_eq!((services.clipboard_text.expect("clipboard text"))(), None);
        assert_eq!((services.clipboard_html.expect("clipboard html"))(), None);
        assert_eq!(services.clipboard_rtf.expect("clipboard rtf")(), None);
        assert!((services.clipboard_formats.expect("formats"))().is_empty());
        assert!(!(services.set_clipboard_html.expect("set html"))(
            "<b>x</b>", "x", ""
        ));
        assert_eq!(
            services.animations_enabled.expect("animations")(),
            Some(false)
        );
    }

    /// The input boundary: viewing survives, everything that could edit or copy does not.
    #[test]
    fn input_filter_keeps_viewing_and_drops_editing() {
        let mut raw = egui::RawInput::default();
        raw.events = vec![
            egui::Event::Text("secret".into()),
            key(egui::Key::S, egui::Modifiers::CTRL),
            egui::Event::Paste("pasted".into()),
            egui::Event::Copy,
            egui::Event::Cut,
            click(),
            egui::Event::PointerMoved(egui::pos2(10.0, 20.0)),
            wheel(),
            egui::Event::Zoom(1.1),
            egui::Event::WindowFocused(true),
        ];
        let outcome = filter_events(&mut raw);
        assert!(!outcome.quit_requested);
        assert_eq!(
            outcome.dropped, 6,
            "keys, text, paste, copy, cut and clicks"
        );
        assert_eq!(
            raw.events,
            vec![
                egui::Event::PointerMoved(egui::pos2(10.0, 20.0)),
                wheel(),
                egui::Event::Zoom(1.1),
                egui::Event::WindowFocused(true),
            ],
            "scrolling and focus survive; nothing else does"
        );
    }

    /// Ctrl+Q closes the window, and the app still never sees a key event.
    #[test]
    fn quit_chord_is_handled_by_the_filter_only() {
        for modifiers in [egui::Modifiers::CTRL, egui::Modifiers::COMMAND] {
            let mut raw = egui::RawInput::default();
            raw.events = vec![key(egui::Key::Q, modifiers)];
            let outcome = filter_events(&mut raw);
            assert!(outcome.quit_requested);
            assert_eq!(outcome.dropped, 1);
            assert!(raw.events.is_empty(), "the chord must not reach the app");
        }

        // Q on its own is typing, not quitting, and is still dropped.
        let mut raw = egui::RawInput::default();
        raw.events = vec![key(egui::Key::Q, egui::Modifiers::NONE)];
        let outcome = filter_events(&mut raw);
        assert!(!outcome.quit_requested);
        assert!(raw.events.is_empty());
    }

    #[test]
    fn kappa_accepts_exactly_sixty_four_hex_characters() {
        assert_eq!(parse_kappa(&"11".repeat(32)).expect("valid"), [0x11u8; 32]);
        assert_eq!(parse_kappa(&"Aa".repeat(32)).expect("valid"), [0xAAu8; 32]);
        assert!(parse_kappa(&"11".repeat(31)).is_err());
        assert!(parse_kappa(&"zz".repeat(32)).is_err());
        assert!(parse_kappa("0").is_err());
    }

    /// End to end through the store: a rich document decodes read-only, keeps its content and
    /// leaves the container bytes untouched.
    #[cfg(unix)]
    #[test]
    fn a_rich_container_opens_read_only_and_is_not_modified() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rich");
        let pass = || SealedPassphrase::new("test passphrase").expect("valid");
        let kappa = [0x61u8; 32];
        let options = |cursor, writable| {
            DocumentSessionOptions::new(cursor, writable, Topology::UmbilicalUsb)
        };
        let mut session = EditorSession::create(
            &path,
            Box::new(DerivedPad::new()),
            pass(),
            kappa,
            options(0, true),
        )
        .expect("create");
        *session.document_mut() = Document::from_text("rich body");
        session.document_mut().core.title = "important metadata".into();
        if let Block::Para(paragraph) = Arc::make_mut(&mut session.document_mut().body[0]) {
            paragraph.runs[0].props.bold = Some(true);
        }
        let original = session.document().clone();
        session.save().expect("save rich document");
        drop(session);

        let before = std::fs::read(&path).expect("read container");
        let opened = open_read_only(&path, 1, pass(), kappa).expect("read-only open");
        assert!(
            opened.document.same_content(&original),
            "rich content must survive the read-only path"
        );
        assert_eq!(opened.cursor, 1);
        assert_eq!(
            std::fs::read(&path).expect("read container again"),
            before,
            "a read-only view must not touch the container"
        );

        // A wrong passphrase fails before anything is decoded.
        let wrong = open_read_only(
            &path,
            1,
            SealedPassphrase::new("wrong passphrase").expect("valid"),
            kappa,
        );
        assert!(wrong.is_err(), "the wrong passphrase must be refused");
    }

    /// The viewer hands Wordcraft a document with no path, so Wordcraft has no file to save to.
    #[test]
    fn the_rendered_session_has_no_file_or_recovery() {
        use wordcraft_engine::Session;
        let session = Session::new(Document::from_text("viewed text"));
        assert!(session.path.is_none());
        assert!(!session.recovery_ready());
        assert!(!session.doc.same_content(&Document::new()));
    }

    /// A payload this build cannot decode must be refused rather than rendered half-way.
    #[test]
    fn undecodable_payloads_are_rejected_by_the_bridge() {
        let error = holonomy_wordcraft::decode(b"not a wordcraft document")
            .expect_err("garbage must not decode");
        assert!(matches!(
            error,
            BridgeError::UnrecognizedPayload
                | BridgeError::InvalidDocument
                | BridgeError::UnsupportedRepresentation
        ));
    }
}
