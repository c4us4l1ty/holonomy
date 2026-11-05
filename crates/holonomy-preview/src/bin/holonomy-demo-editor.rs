//! A restricted, plain-text, demo-pad container editor. Not a security product.
//!
//! Unlike the scratch preview, this binary does not use Wordcraft's file menus,
//! exporters or recovery. Editing refuses rich Wordcraft documents; read-only
//! inspection offers only a potentially incomplete plain-text body preview.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{mpsc, Arc, Mutex};

use eframe::egui;
use holonomy_cli::document::{DocumentSessionOptions, DocumentSessionState};
use holonomy_cli::store::pad::DerivedPad;
use holonomy_cli::store::SaveReport;
use holonomy_core::unlock::{SealedPassphrase, Topology};
use holonomy_wordcraft::editor::EditorSession;
use wordcraft_doc::{Document, StoryRef};
use zeroize::Zeroizing;

#[path = "demo_editor/status.rs"]
mod status;

const WARNING: &str = "EXPERIMENTAL DEMO — deterministic stand-in pad, no physical USB protection. Do not use sensitive or irreplaceable data. Clipboard and memory may expose plaintext.";

#[derive(Debug, PartialEq, Eq)]
enum Launch {
    Create(PathBuf),
    Open(PathBuf, u64),
    Inspect(PathBuf, u64),
}

fn parse_args(args: &[OsString]) -> Result<Launch, &'static str> {
    match args {
        [action, path] if action == "--demo-create" => {
            Ok(Launch::Create(PathBuf::from(path.as_os_str())))
        }
        [action, path, cursor] if action == "--demo-open" || action == "--demo-inspect" => {
            let cursor = cursor
                .to_str()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or("cursor must be an unsigned integer")?;
            let path = PathBuf::from(path.as_os_str());
            if action == "--demo-inspect" {
                Ok(Launch::Inspect(path, cursor))
            } else {
                Ok(Launch::Open(path, cursor))
            }
        }
        _ => Err("use --demo-create PATH, --demo-open PATH CURSOR, or --demo-inspect PATH CURSOR"),
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
    let options =
        |cursor, writable| DocumentSessionOptions::new(cursor, writable, Topology::UmbilicalUsb);
    let pad = Box::new(DerivedPad::new());
    match launch {
        Launch::Create(path) => {
            EditorSession::create(path, pad, passphrase, *kappa, options(0, true))
        }
        Launch::Open(path, cursor) => {
            EditorSession::open(path, pad, passphrase, *kappa, options(*cursor, true))
        }
        Launch::Inspect(path, cursor) => {
            EditorSession::open(path, pad, passphrase, *kappa, options(*cursor, false))
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

/// Editable sessions require an exact round trip. Read-only inspection may
/// show a lossy body-text preview because it cannot modify or save the source.
fn display_text(doc: &Document, writable: bool) -> Result<Zeroizing<String>, &'static str> {
    if writable {
        plain_text(doc)
    } else {
        Ok(Zeroizing::new(doc.plain_text(StoryRef::Body)))
    }
}

enum SaveOutcome {
    Saved(Option<SaveReport>),
    Failed(String),
    Interrupted,
}

#[derive(Debug, PartialEq, Eq)]
enum CloseDisposition {
    Allow,
    Confirm,
    WaitForSave,
}

struct PendingSave {
    session: Arc<Mutex<Option<EditorSession>>>,
    receiver: mpsc::Receiver<SaveOutcome>,
}

struct DemoEditor {
    session: Option<EditorSession>,
    pending: Option<PendingSave>,
    cursor: u64,
    last_saved_digest: Option<[u8; 32]>,
    last_ratchet_report: Option<Option<u32>>,
    last_protection: Option<String>,
    writable: bool,
    recovery_blocked: bool,
    text: Zeroizing<String>,
    text_unrepresentable: bool,
    close_confirmation: bool,
    show_session_status: bool,
    allow_close: bool,
    status: String,
}

impl DemoEditor {
    fn new(session: EditorSession) -> Result<Self, &'static str> {
        let writable = session.is_writable();
        let text = display_text(session.document(), writable)?;
        let cursor = session.cursor();
        Ok(Self {
            session: Some(session),
            pending: None,
            cursor,
            last_saved_digest: None,
            last_ratchet_report: None,
            last_protection: None,
            writable,
            recovery_blocked: false,
            text,
            text_unrepresentable: false,
            close_confirmation: false,
            show_session_status: false,
            allow_close: false,
            status: if writable {
                "Ready. Record the cursor after each save."
            } else {
                "Read-only inspection. This does not verify pad freshness or complete recovery."
            }
            .into(),
        })
    }

    fn sync_text(&mut self) {
        if !self.writable {
            return;
        }
        let document = Document::from_text(&self.text);
        // Wordcraft strips trailing CR characters from lines. Never save a
        // silently modified version of the text the user sees.
        self.text_unrepresentable = document.plain_text(StoryRef::Body) != *self.text;
        if !self.text_unrepresentable {
            if let Some(session) = &mut self.session {
                *session.document_mut() = document;
            }
        }
    }

    fn needs_recovery(&self) -> bool {
        self.recovery_blocked
            || self
                .session
                .as_ref()
                .is_some_and(|session| session.state() == DocumentSessionState::RecoveryRequired)
    }

    fn dirty(&self) -> bool {
        self.pending.is_some()
            || self.needs_recovery()
            || self.text_unrepresentable
            || self
                .session
                .as_ref()
                .is_some_and(EditorSession::has_unsaved_changes)
    }

    fn session_status(&self) -> status::SessionStatus {
        status::SessionStatus::new(
            self.cursor,
            self.pending.is_some(),
            self.needs_recovery(),
            self.text_unrepresentable,
            self.dirty(),
        )
    }

    fn cursor_limit_reached(&self) -> bool {
        self.cursor >= holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE.saturating_sub(1)
    }

    fn close_disposition(&self) -> CloseDisposition {
        if self.pending.is_some() {
            CloseDisposition::WaitForSave
        } else if !self.allow_close && self.dirty() {
            CloseDisposition::Confirm
        } else {
            CloseDisposition::Allow
        }
    }

    fn save(&mut self, ctx: &egui::Context) {
        if !self.writable {
            self.status = "Read-only inspection: saving is disabled.".into();
            return;
        }
        if self.pending.is_some() || self.needs_recovery() {
            return;
        }
        if self.text_unrepresentable {
            self.status = "Text cannot be represented exactly; nothing was saved.".into();
            return;
        }
        if self.cursor_limit_reached() {
            self.status =
                "Cursor limit reached: no save attempted. This demo cannot migrate to a fresh pad."
                    .into();
            return;
        }
        let Some(session) = self.session.take() else {
            self.recovery_blocked = true;
            self.status = "Session unavailable; recovery required.".into();
            return;
        };
        if !session.has_unsaved_changes() {
            self.session = Some(session);
            self.status = "No changes; no cursor was spent.".into();
            return;
        }
        let slot = Arc::new(Mutex::new(Some(session)));
        let worker_slot = Arc::clone(&slot);
        let (sender, receiver) = mpsc::channel();
        let wake = ctx.clone();
        let spawned = std::thread::Builder::new()
            .name("holonomy-save".into())
            .spawn(move || {
                // Hold no mutex during I/O. A panic leaves the disk state uncertain,
                // even if the session object can be returned to the UI.
                let session = worker_slot.lock().ok().and_then(|mut slot| slot.take());
                let outcome = if let Some(mut session) = session {
                    let saved =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| session.save()));
                    let result = match saved {
                        Ok(Ok(report)) => SaveOutcome::Saved(report),
                        Ok(Err(error)) => SaveOutcome::Failed(error.to_string()),
                        Err(_) => SaveOutcome::Interrupted,
                    };
                    if let Ok(mut slot) = worker_slot.lock() {
                        *slot = Some(session);
                        result
                    } else {
                        SaveOutcome::Interrupted
                    }
                } else {
                    SaveOutcome::Interrupted
                };
                let _ = sender.send(outcome);
                wake.request_repaint();
            });
        match spawned {
            Ok(_) => {
                self.pending = Some(PendingSave {
                    session: slot,
                    receiver,
                });
                self.status = "Saving in the background; editing and closing are paused.".into();
            }
            Err(error) => {
                self.session = slot.lock().ok().and_then(|mut slot| slot.take());
                if self.session.is_some() {
                    self.status =
                        format!("Could not start save worker; nothing was written: {error}");
                } else {
                    self.recovery_blocked = true;
                    self.status = "Save worker failed to start and session is unavailable; recovery required.".into();
                }
            }
        }
    }

    fn finish_save(&mut self, outcome: SaveOutcome) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.session = pending.session.lock().ok().and_then(|mut slot| slot.take());
        if let Some(session) = &self.session {
            self.cursor = session.cursor();
        } else {
            self.recovery_blocked = true;
        }
        match outcome {
            SaveOutcome::Saved(Some(report)) if !self.recovery_blocked => {
                self.last_saved_digest = Some(report.digest);
                self.last_ratchet_report = Some(report.ratcheted);
                self.last_protection = Some(report.protection.describe().to_string());
                self.status = format!(
                    "Saved at cursor {}. Record this cursor for reopening.",
                    self.cursor
                );
            }
            SaveOutcome::Saved(None) if !self.recovery_blocked => {
                self.status = "No changes; no cursor was spent.".into();
            }
            SaveOutcome::Failed(error) if !self.recovery_blocked => {
                self.status = format!("Save failed: {error}");
            }
            _ => {
                self.recovery_blocked = true;
                self.status = "Save worker was interrupted; container or pad may have changed. Recovery required.".into();
            }
        }
    }

    fn poll_save(&mut self) {
        let outcome = self
            .pending
            .as_ref()
            .and_then(|pending| match pending.receiver.try_recv() {
                Ok(outcome) => Some(outcome),
                Err(mpsc::TryRecvError::Empty) => None,
                Err(mpsc::TryRecvError::Disconnected) => Some(SaveOutcome::Interrupted),
            });
        if let Some(outcome) = outcome {
            self.finish_save(outcome);
        }
    }
}

impl eframe::App for DemoEditor {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_save();
        if ctx.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::S))
            && !self.close_confirmation
        {
            self.save(ctx);
        }
        if ctx.input(|input| input.key_pressed(egui::Key::Q) && input.modifiers.command) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        if ctx.input(|input| input.viewport().close_requested()) {
            match self.close_disposition() {
                CloseDisposition::WaitForSave => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    self.allow_close = false;
                    self.status = "Save in progress. Please wait before closing.".into();
                }
                CloseDisposition::Confirm => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    self.close_confirmation = true;
                }
                CloseDisposition::Allow => {}
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("warning").show(ui, |ui| {
            ui.colored_label(egui::Color32::from_rgb(190, 35, 35), WARNING);
            if self.writable {
                ui.label("Plain-text editing only. No Wordcraft file, export or print menus. ASCII passphrase case variants are equivalent.");
            } else {
                ui.strong("READ-ONLY BODY-TEXT PREVIEW — editing and saving are disabled.");
                ui.label("This is not the full document: formatting, images, tables, headers, footers, and other content may be missing. No file, export or print menus. ASCII passphrase case variants are equivalent.");
            }
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                let can_save = self.writable && self.pending.is_none() && !self.needs_recovery() && !self.cursor_limit_reached();
                if ui.add_enabled(can_save, egui::Button::new("Save to demo container")).clicked() {
                    self.save(ui.ctx());
                }
                if self.writable {
                    ui.label(self.session_status().phase.label());
                } else {
                    ui.label("Read-only");
                }
                if ui.button("Session status").clicked() {
                    self.show_session_status = true;
                }
            });
            ui.label(self.session_status().cursor_label());
            ui.label("Demo pad · Capture exclusion not active · Memory not verified · Watchdog not running");
            ui.label(&self.status);
            if self.writable && self.cursor_limit_reached() && !self.needs_recovery() && self.pending.is_none() {
                ui.colored_label(egui::Color32::RED, "Cursor limit reached. Editing and saving are disabled; this demo has no fresh-pad migration.");
            }
            if self.needs_recovery() {
                ui.colored_label(egui::Color32::RED, "Recovery required: the container or pad may have changed. Further saves are blocked; inspect and recover before reopening.");
            }
            if self.text_unrepresentable {
                ui.colored_label(egui::Color32::RED, "The current text cannot be represented without changes; saving is blocked.");
            }
        });
        egui::CentralPanel::default().show(ui, |ui| {
            let enabled = self.pending.is_none() && self.session.is_some();
            let editable = self.writable && !self.cursor_limit_reached();
            let response = ui
                .add_enabled_ui(enabled, |ui| {
                    ui.add_sized(
                        ui.available_size(),
                        egui::TextEdit::multiline(&mut *self.text)
                            .desired_width(f32::INFINITY)
                            .interactive(editable),
                    )
                })
                .inner;
            if response.changed() {
                self.sync_text();
            }
        });
        if self.show_session_status {
            let snapshot = self.session_status();
            let fingerprint = status::fingerprint_label(snapshot.phase, self.last_saved_digest);
            let ratchet = status::ratchet_label(snapshot.phase, self.last_ratchet_report);
            let protection =
                status::protection_label(snapshot.phase, self.last_protection.as_deref());
            let writable = self.writable;
            egui::Window::new("Session status")
                .open(&mut self.show_session_status)
                .show(ui.ctx(), |ui| {
                    if writable {
                        ui.strong(snapshot.phase.label());
                    } else {
                        ui.strong("Read-only inspection");
                    }
                    ui.label(snapshot.cursor_label());
                    ui.strong("Last-save image fingerprint");
                    ui.add(egui::Label::new(fingerprint).selectable(true));
                    ui.separator();
                    ui.strong("Protection layer");
                    ui.label(status::PROTECTION);
                    ui.strong("Pad freshness and accounting");
                    ui.label(status::PAD_FRESHNESS);
                    ui.strong("Last-save ratchet result");
                    ui.label(ratchet);
                    ui.strong("Last-save protection layer");
                    ui.label(protection);
                    ui.strong("Screen capture");
                    ui.label(status::capture());
                    ui.strong("Memory locking");
                    ui.label(status::MEMORY);
                    ui.strong("Watchdog and priority");
                    ui.label(status::WATCHDOG);
                });
        }
        if self.close_confirmation {
            egui::Window::new("Unsaved changes or recovery required")
                .collapsible(false)
                .show(ui.ctx(), |ui| {
                    ui.label("Closing can discard edits. If recovery is required, the on-disk state is uncertain.");
                    ui.horizontal(|ui| {
                        if ui.button("Keep editing").clicked() {
                            self.close_confirmation = false;
                        }
                        if ui.add_enabled(self.pending.is_none(), egui::Button::new("Discard and close")).clicked() {
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
    let title = if editor.writable {
        "Holonomy — EXPERIMENTAL TEXT DEMO"
    } else {
        "Holonomy — EXPERIMENTAL TEXT DEMO — READ ONLY"
    };
    eframe::run_native(
        title,
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
        assert!(matches!(
            parse_args(&["--demo-inspect".into(), "demo".into(), "1".into()]),
            Ok(Launch::Inspect(_, 1))
        ));
        assert!(parse_args(&["--demo-inspect".into(), "demo".into(), "-1".into()]).is_err());
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
        assert!(display_text(&formatted, true).is_err());
        let original = formatted.clone();
        assert_eq!(
            display_text(&formatted, false).expect("preview").as_str(),
            "hello"
        );
        assert!(formatted.same_content(&original));
        assert_eq!(
            display_text(&doc, false).expect("preview").as_str(),
            "hello\nworld"
        );
        assert_eq!(plain_text(&Document::new()).expect("blank").as_str(), "");
    }

    #[cfg(unix)]
    #[test]
    fn rich_document_is_previewed_read_only_and_refused_for_editing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("rich-document");
        let options = |cursor, writable| {
            DocumentSessionOptions::new(cursor, writable, Topology::UmbilicalUsb)
        };
        let pass = || SealedPassphrase::new("test passphrase").expect("valid");
        let kappa = [0x48; 32];
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
        if let wordcraft_doc::Block::Para(paragraph) =
            std::sync::Arc::make_mut(&mut session.document_mut().body[0])
        {
            paragraph.runs[0].props.bold = Some(true);
        }
        let original = session.document().clone();
        assert!(session.save().expect("save rich document").is_some());
        drop(session);

        let inspection = EditorSession::open(
            &path,
            Box::new(DerivedPad::new()),
            pass(),
            kappa,
            options(1, false),
        )
        .expect("open for inspection");
        let mut editor = DemoEditor::new(inspection).expect("read-only body preview");
        assert!(!editor.writable);
        assert_eq!(editor.text.as_str(), "rich body");
        assert_eq!(editor.close_disposition(), CloseDisposition::Allow);
        *editor.text = "attempted replacement".into();
        editor.sync_text();
        editor.save(&egui::Context::default());
        assert!(editor.pending.is_none());
        assert!(editor.status.contains("saving is disabled"));
        assert!(editor
            .session
            .as_ref()
            .expect("session")
            .document()
            .same_content(&original));
        drop(editor);

        let writable = EditorSession::open(
            &path,
            Box::new(DerivedPad::new()),
            pass(),
            kappa,
            options(1, true),
        )
        .expect("open writable");
        assert!(
            DemoEditor::new(writable).is_err(),
            "rich content must not enter text editing"
        );
        let reopened = EditorSession::open(
            &path,
            Box::new(DerivedPad::new()),
            pass(),
            kappa,
            options(1, false),
        )
        .expect("reopen");
        assert!(reopened.document().same_content(&original));
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
        assert_eq!(editor.session_status().phase, status::SavePhase::Saved);
        assert_eq!(editor.session_status().remaining_cursor_saves, Some(53));
        assert_eq!(editor.last_saved_digest, None);
        assert_eq!(editor.last_ratchet_report, None);
        assert_eq!(editor.last_protection, None);
        assert_eq!(editor.close_disposition(), CloseDisposition::Allow);
        *editor.text = "hello\nworld".into();
        editor.sync_text();
        assert!(editor.dirty());
        assert_eq!(editor.close_disposition(), CloseDisposition::Confirm);
        let ctx = egui::Context::default();
        editor.save(&ctx);
        assert!(editor.pending.is_some());
        assert!(editor.dirty());
        assert_eq!(editor.session_status().phase, status::SavePhase::Saving);
        assert_eq!(editor.session_status().remaining_cursor_saves, None);
        editor.allow_close = true;
        assert_eq!(editor.close_disposition(), CloseDisposition::WaitForSave);
        editor.allow_close = false;
        // A second request cannot start a concurrent save.
        editor.save(&ctx);
        let outcome = editor
            .pending
            .as_ref()
            .expect("pending")
            .receiver
            .recv_timeout(std::time::Duration::from_secs(180))
            .expect("save finished");
        editor.finish_save(outcome);
        assert!(!editor.dirty());
        assert_eq!(editor.cursor, 1);
        assert_eq!(editor.session_status().remaining_cursor_saves, Some(52));
        assert_eq!(editor.last_ratchet_report, Some(None));
        let protection = editor
            .last_protection
            .clone()
            .expect("save reports protection");
        assert!(protection.contains("not established"), "{protection}");
        assert!(!protection.contains("fresh sectors"), "{protection}");
        assert!(
            status::ratchet_label(editor.session_status().phase, editor.last_ratchet_report)
                .contains("not attempted")
        );
        let fingerprint = editor
            .last_saved_digest
            .expect("save returned an image fingerprint");
        let mut file =
            holonomy_cli::store::ContainerFile::open(&path, false).expect("open for verification");
        let mut pad = DerivedPad::new();
        let passphrase = pass();
        let context = holonomy_cli::store::SaveContext {
            passphrase: &passphrase,
            kappa_usb: &kappa,
            topology: Topology::UmbilicalUsb,
            params: holonomy_cli::document::DocumentSessionOptions::new(
                1,
                false,
                Topology::UmbilicalUsb,
            )
            .params(),
            cursor: 1,
        };
        assert_eq!(
            holonomy_cli::store::verify_candidate(&mut file, &mut pad, &context).expect("verify"),
            fingerprint
        );
        // Exercise the frontend limit without doing 52 expensive saves. The actual
        // session's cursor remains 1; the guard must not start any worker.
        editor.cursor = holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE - 1;
        assert!(editor.cursor_limit_reached());
        *editor.text = "unsaved test change".into();
        editor.sync_text();
        editor.save(&ctx);
        assert!(editor.pending.is_none());
        assert!(editor.dirty());
        assert_eq!(editor.close_disposition(), CloseDisposition::Confirm);
        assert!(!editor.needs_recovery());
        assert!(editor.status.contains("no save attempted"));
        editor.cursor = 1;
        *editor.text = "hello\nworld".into();
        editor.sync_text();
        assert!(!editor.cursor_limit_reached());
        assert!(!editor.dirty());
        editor.save(&ctx);
        assert!(editor.pending.is_none());
        *editor.text = "cannot preserve\r\n".into();
        editor.sync_text();
        assert!(editor.text_unrepresentable);
        editor.save(&ctx);
        assert!(editor.pending.is_none());
        assert!(editor.dirty());
        assert_eq!(editor.cursor, 1);
        *editor.text = "hello\nworld".into();
        editor.sync_text();
        let (sender, receiver) = mpsc::channel();
        drop(sender);
        let slot = Arc::new(Mutex::new(editor.session.take()));
        editor.pending = Some(PendingSave {
            session: slot,
            receiver,
        });
        editor.poll_save();
        assert!(editor.needs_recovery());
        assert!(editor.dirty());
        assert_eq!(
            editor.session_status().phase,
            status::SavePhase::RecoveryRequired
        );
        assert_eq!(editor.session_status().remaining_cursor_saves, None);
        assert_eq!(editor.last_saved_digest, Some(fingerprint));
        assert!(
            status::ratchet_label(editor.session_status().phase, editor.last_ratchet_report)
                .contains("uncertain")
        );
        let fingerprint_hex = fingerprint
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert!(!status::fingerprint_label(
            editor.session_status().phase,
            editor.last_saved_digest
        )
        .contains(&fingerprint_hex));
        assert_eq!(editor.close_disposition(), CloseDisposition::Confirm);
        editor.save(&ctx);
        assert!(editor.pending.is_none());
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
        let session = EditorSession::open(
            &path,
            Box::new(DerivedPad::new()),
            pass(),
            kappa,
            DocumentSessionOptions::new(1, false, Topology::UmbilicalUsb),
        )
        .expect("open read-only");
        let mut inspector = DemoEditor::new(session).expect("plain");
        assert!(!inspector.writable);
        assert_eq!(inspector.text.as_str(), "hello\nworld");
        assert_eq!(inspector.close_disposition(), CloseDisposition::Allow);
        *inspector.text = "attempted edit".into();
        inspector.sync_text();
        assert_eq!(
            plain_text(inspector.session.as_ref().expect("session").document())
                .expect("plain")
                .as_str(),
            "hello\nworld"
        );
        inspector.save(&ctx);
        assert!(inspector.pending.is_none());
        assert_eq!(inspector.cursor, 1);
        assert!(inspector.status.contains("saving is disabled"));
        drop(inspector);
        let reopened = EditorSession::open(
            &path,
            Box::new(DerivedPad::new()),
            pass(),
            kappa,
            DocumentSessionOptions::new(1, false, Topology::UmbilicalUsb),
        )
        .expect("image remains readable");
        assert_eq!(
            plain_text(reopened.document()).expect("plain").as_str(),
            "hello\nworld"
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_background_ratchet_requires_recovery_after_replacement() {
        use holonomy_cli::store::pad::{PadSource, PadWindow};

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

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("document");
        let options = |cursor| DocumentSessionOptions::new(cursor, true, Topology::UmbilicalUsb);
        let pass = || SealedPassphrase::new("test passphrase").expect("valid");
        let kappa = [0x47; 32];
        let session = EditorSession::create(
            &path,
            Box::new(FailedRatchet(DerivedPad::new())),
            pass(),
            kappa,
            options(0),
        )
        .expect("create");
        let mut editor = DemoEditor::new(session).expect("plain");
        *editor.text = "new image".into();
        editor.sync_text();
        let ctx = egui::Context::default();
        editor.save(&ctx);
        let outcome = editor
            .pending
            .as_ref()
            .expect("pending")
            .receiver
            .recv_timeout(std::time::Duration::from_secs(180))
            .expect("save finished");
        editor.finish_save(outcome);
        assert!(editor.needs_recovery());
        assert!(editor.dirty());
        assert_eq!(editor.cursor, 0);
        assert_eq!(editor.close_disposition(), CloseDisposition::Confirm);
        editor.save(&ctx);
        assert!(editor.pending.is_none());
        drop(editor);

        let session = EditorSession::open(
            &path,
            Box::new(DerivedPad::new()),
            pass(),
            kappa,
            options(1),
        )
        .expect("new image was written before the ratchet failed");
        assert_eq!(
            DemoEditor::new(session).expect("plain").text.as_str(),
            "new image"
        );
    }
}
