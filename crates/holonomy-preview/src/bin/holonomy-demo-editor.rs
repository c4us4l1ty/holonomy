//! A restricted, plain-text, demo-pad container editor. Not a security product.
//!
//! Unlike the scratch preview, this binary does not use Wordcraft's file menus,
//! exporters or recovery. It deliberately refuses rich Wordcraft documents.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use eframe::egui;
use holonomy_cli::document::{DocumentSessionOptions, DocumentSessionState};
use holonomy_cli::store::pad::DerivedPad;
use holonomy_core::unlock::{SealedPassphrase, Topology};
use holonomy_wordcraft::editor::EditorSession;
use wordcraft_doc::{Document, StoryRef};
use zeroize::Zeroizing;

const WARNING: &str = "EXPERIMENTAL DEMO — deterministic stand-in pad, no physical USB protection. Do not use sensitive or irreplaceable data. Clipboard and memory may expose plaintext.";

#[derive(Debug, PartialEq, Eq)]
enum Launch {
    Create(PathBuf),
    Open(PathBuf, u64),
}

fn parse_args(args: &[OsString]) -> Result<Launch, &'static str> {
    match args {
        [action, path] if action == "--demo-create" => {
            Ok(Launch::Create(PathBuf::from(path.as_os_str())))
        }
        [action, path, cursor] if action == "--demo-open" => {
            let cursor = cursor
                .to_str()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or("cursor must be an unsigned integer")?;
            Ok(Launch::Open(PathBuf::from(path.as_os_str()), cursor))
        }
        _ => Err("use --demo-create PATH or --demo-open PATH CURSOR"),
    }
}

fn parse_kappa(hex: &str) -> Result<[u8; 32], &'static str> {
    if hex.len() != 64 || !hex.is_ascii() {
        return Err("root share must be exactly 64 ASCII hex characters");
    }
    let mut bytes = Zeroizing::new([0u8; 32]);
    for (i, pair) in hex.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let digit = |b: u8| match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        };
        bytes[i] = (digit(pair[0]).ok_or("root share contains non-hex characters")? << 4)
            | digit(pair[1]).ok_or("root share contains non-hex characters")?;
    }
    Ok(*bytes)
}

fn secret(prompt: &str, confirm: bool) -> Result<Zeroizing<String>, String> {
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

fn open_session(launch: &Launch) -> Result<EditorSession, String> {
    let creating = matches!(launch, Launch::Create(_));
    eprintln!("{WARNING}");
    eprintln!(
        "The legacy passphrase mapping lowercases input; ASCII case variants are equivalent."
    );
    let pass = secret("Passphrase: ", creating)?;
    let share = secret("Demo root share (64 hex characters): ", creating)?;
    let kappa = Zeroizing::new(parse_kappa(&share).map_err(str::to_owned)?);
    let passphrase = SealedPassphrase::new(&pass).ok_or("invalid passphrase length")?;
    let options = |cursor| DocumentSessionOptions::new(cursor, true, Topology::UmbilicalUsb);
    let pad = Box::new(DerivedPad::new());
    match launch {
        Launch::Create(path) => EditorSession::create(path, pad, passphrase, *kappa, options(0)),
        Launch::Open(path, cursor) => {
            EditorSession::open(path, pad, passphrase, *kappa, options(*cursor))
        }
    }
    .map_err(|e| e.to_string())
}

/// Convert only documents representable by the text editor without losing data.
fn plain_text(doc: &Document) -> Result<Zeroizing<String>, &'static str> {
    let text = Zeroizing::new(doc.plain_text(StoryRef::Body));
    if !doc.same_content(&Document::from_text(&text)) {
        return Err("This document has formatting, metadata, or structure the text-only demo cannot preserve. It was not opened for editing.");
    }
    Ok(text)
}

struct DemoEditor {
    session: EditorSession,
    text: Zeroizing<String>,
    text_unrepresentable: bool,
    close_confirmation: bool,
    allow_close: bool,
    status: String,
}

impl DemoEditor {
    fn new(session: EditorSession) -> Result<Self, &'static str> {
        let text = plain_text(session.document())?;
        Ok(Self {
            session,
            text,
            text_unrepresentable: false,
            close_confirmation: false,
            allow_close: false,
            status: "Ready. Saves may block the window while the container is written.".into(),
        })
    }

    fn sync_text(&mut self) {
        let document = Document::from_text(&self.text);
        // Wordcraft strips trailing CR characters from lines. Never save a
        // silently modified version of the text the user sees.
        self.text_unrepresentable = document.plain_text(StoryRef::Body) != *self.text;
        if !self.text_unrepresentable {
            *self.session.document_mut() = document;
        }
    }

    fn dirty(&self) -> bool {
        self.text_unrepresentable || self.session.has_unsaved_changes()
    }

    fn save(&mut self) {
        if self.text_unrepresentable {
            self.status =
                "Text contains line endings this demo cannot preserve; nothing was saved.".into();
            return;
        }
        match self.session.save() {
            Ok(Some(_)) => {
                self.status = format!(
                    "Saved at cursor {}. Record this cursor for reopening.",
                    self.session.cursor()
                )
            }
            Ok(None) => self.status = "No changes; no cursor was spent.".into(),
            Err(error) => self.status = format!("Save failed: {error}"),
        }
    }
}

impl eframe::App for DemoEditor {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if ctx.input(|input| input.viewport().close_requested())
            && !self.allow_close
            && self.dirty()
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_confirmation = true;
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("warning").show(ui, |ui| {
            ui.colored_label(egui::Color32::from_rgb(190, 35, 35), WARNING);
            ui.label("Plain-text documents only. No Wordcraft file, export or print menus. ASCII passphrase case variants are equivalent.");
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let recovery = self.session.state() == DocumentSessionState::RecoveryRequired;
                if ui.add_enabled(!recovery, egui::Button::new("Save to demo container")).clicked() {
                    self.save();
                }
                ui.label(format!("Cursor: {} · {}", self.session.cursor(), if self.dirty() { "Unsaved or recovery required" } else { "Saved" }));
            });
            ui.label(&self.status);
            if self.session.state() == DocumentSessionState::RecoveryRequired {
                ui.colored_label(egui::Color32::RED, "Recovery required: the container or pad may have changed. Further saves are blocked; inspect and recover before reopening.");
            }
            if self.text_unrepresentable {
                ui.colored_label(egui::Color32::RED, "The current text cannot be represented without changes; saving is blocked.");
            }
        });
        egui::CentralPanel::default().show(ui, |ui| {
            let response = ui.add_sized(
                ui.available_size(),
                egui::TextEdit::multiline(&mut *self.text).desired_width(f32::INFINITY),
            );
            if response.changed() {
                self.sync_text();
            }
        });
        if self.close_confirmation {
            egui::Window::new("Unsaved changes or recovery required")
                .collapsible(false)
                .show(ui.ctx(), |ui| {
                    ui.label("Closing can discard edits. If recovery is required, the on-disk state is uncertain.");
                    ui.horizontal(|ui| {
                        if ui.button("Keep editing").clicked() {
                            self.close_confirmation = false;
                        }
                        if ui.button("Discard and close").clicked() {
                            self.allow_close = true;
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
        }
    }
}

fn run(launch: &Launch) -> Result<(), String> {
    let session = open_session(launch)?;
    let editor = DemoEditor::new(session).map_err(str::to_owned)?;
    eframe::run_native(
        "Holonomy — EXPERIMENTAL TEXT DEMO",
        eframe::NativeOptions::default(),
        Box::new(move |_| Ok(Box::new(editor))),
    )
    .map_err(|e| format!("could not start the demo window: {e}"))
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let launch = match parse_args(&args) {
        Ok(launch) => launch,
        Err(error) => {
            eprintln!("{WARNING}\n{error}\nExample: holonomy-demo-editor --demo-create PATH");
            return ExitCode::FAILURE;
        }
    };
    match run(&launch) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_and_root_share_are_checked() {
        assert!(matches!(
            parse_args(&["--demo-create".into(), "demo".into()]),
            Ok(Launch::Create(_))
        ));
        assert!(matches!(
            parse_args(&["--demo-open".into(), "demo".into(), "1".into()]),
            Ok(Launch::Open(_, 1))
        ));
        assert!(parse_args(&["--demo-open".into(), "demo".into(), "-1".into()]).is_err());
        assert!(parse_args(&["--demo-create".into(), "demo".into(), "extra".into()]).is_err());
        assert_eq!(parse_kappa(&"aB".repeat(32)).expect("hex"), [0xab; 32]);
        assert!(parse_kappa(&"é".repeat(32)).is_err());
        assert!(parse_kappa(&"zz".repeat(32)).is_err());
    }

    #[test]
    fn rejects_rich_documents_instead_of_discarding_metadata() {
        let mut doc = Document::from_text("hello\nworld");
        assert_eq!(plain_text(&doc).expect("plain").as_str(), "hello\nworld");
        doc.core.title = "important metadata".into();
        assert!(plain_text(&doc).is_err());
        let mut formatted = Document::from_text("hello");
        if let wordcraft_doc::Block::Para(paragraph) =
            std::sync::Arc::make_mut(&mut formatted.body[0])
        {
            paragraph.runs[0].props.bold = Some(true);
        }
        assert!(plain_text(&formatted).is_err());
        assert_eq!(plain_text(&Document::new()).expect("blank").as_str(), "");
    }

    #[cfg(unix)]
    #[test]
    fn text_edit_save_reopen_and_close_guard_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("document");
        let options = |cursor| DocumentSessionOptions::new(cursor, true, Topology::UmbilicalUsb);
        let pass = || SealedPassphrase::new("test passphrase").expect("valid");
        let kappa = [0x46; 32];
        let session = EditorSession::create(
            &path,
            Box::new(DerivedPad::new()),
            pass(),
            kappa,
            options(0),
        )
        .expect("create");
        let mut editor = DemoEditor::new(session).expect("plain");
        assert!(!editor.dirty());
        *editor.text = "hello\nworld".into();
        editor.sync_text();
        assert!(editor.dirty());
        editor.save();
        assert!(!editor.dirty());
        assert_eq!(editor.session.cursor(), 1);
        *editor.text = "cannot preserve\r\n".into();
        editor.sync_text();
        assert!(editor.text_unrepresentable);
        editor.save();
        assert!(editor.dirty());
        assert_eq!(editor.session.cursor(), 1);
        drop(editor);
        let session = EditorSession::open(
            &path,
            Box::new(DerivedPad::new()),
            pass(),
            kappa,
            options(1),
        )
        .expect("open");
        assert_eq!(
            DemoEditor::new(session).expect("plain").text.as_str(),
            "hello\nworld"
        );
    }
}
