//! The experimental, read-only Wordcraft container viewer.
//!
//! This binary opens containers only through the deterministic demo pad. It does not support
//! physical USB, editing, saving, or recovery beyond a read-only cursor-candidate scan, and it is
//! not suitable for sensitive data. The
//! read-only service boundary and input filter are shared with `holonomy-preview`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;

use eframe::egui;
use holonomy_core::layout::SAVES_BEFORE_DOMAIN_REUSE;
use holonomy_core::unlock::SealedPassphrase;
use holonomy_preview::{
    filter_events, find_read_only_cursor, open_read_only, parse_kappa, prompt_credentials,
    read_only_services, CursorSearch, OpenedView, ViewInput, READ_ONLY_LIMITS, READ_ONLY_RULE,
};
use wordcraft_doc::{Document, StoryRef};
use wordcraft_engine::Session;
use wordcraft_ui_egui::WordApp;
use zeroize::{Zeroize, Zeroizing};

const TITLE: &str = "Holonomy — DEMO PAD · READ-ONLY WORDCRAFT VIEW";
const DEMO_WARNING: &str = "DEMO PAD — deterministic stand-in, not physical USB. Prototype protections such as capture exclusion, editor-wide memory locking, and the watchdog are not integrated. Never use sensitive or irreplaceable data.";
const USAGE: &str = "usage: holonomy [--help] [--version] [--check]\n       holonomy --view PATH CURSOR\n       holonomy --inspect-only PATH CURSOR";

#[derive(Debug, PartialEq, Eq)]
enum Launch {
    /// Open a read-only window.
    View(PathBuf, u64),
    /// Decode and report without creating a window.
    InspectOnly(PathBuf, u64),
}

fn parse_args(args: &[OsString]) -> Result<Launch, &'static str> {
    let [action, path, cursor] = args else {
        return Err(USAGE);
    };
    let cursor = cursor
        .to_str()
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or("cursor must be an unsigned integer")?;
    let path = PathBuf::from(path.as_os_str());
    match action.to_str() {
        Some("--view") => Ok(Launch::View(path, cursor)),
        Some("--inspect-only") => Ok(Launch::InspectOnly(path, cursor)),
        _ => Err(USAGE),
    }
}

/// The window owns only Wordcraft's in-memory document, never a path or writable handle.
struct ReadOnlyWindow {
    app: WordApp,
    cursor: u64,
    quit: bool,
    dropped_events: usize,
}

impl ReadOnlyWindow {
    fn new(document: Document, cursor: u64) -> Self {
        Self {
            app: WordApp::new(Session::new(document), read_only_services()),
            cursor,
            quit: false,
            dropped_events: 0,
        }
    }
}

impl eframe::App for ReadOnlyWindow {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.app.logic(ctx);
        if self.quit || self.app.quit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        let ViewInput {
            quit_requested,
            dropped,
        } = filter_events(raw);
        self.dropped_events = self.dropped_events.saturating_add(dropped);
        if quit_requested {
            self.quit = true;
        }
        self.app.raw_input_hook(ctx, raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("holonomy_read_only_banner").show(ui, |ui| {
            ui.colored_label(egui::Color32::from_rgb(190, 35, 35), DEMO_WARNING);
            ui.colored_label(egui::Color32::from_rgb(190, 35, 35), READ_ONLY_RULE);
            ui.colored_label(egui::Color32::GRAY, READ_ONLY_LIMITS);
            ui.label(format!("Opened at cursor {}.", self.cursor));
            ui.label("Press Ctrl+Q to close. The window ignores every other key and click.");
        });
        self.app.ui(ui);
    }
}

struct OpenForm {
    path: String,
    cursor: String,
    passphrase: String,
    root_share: String,
    error: Option<String>,
    notice: Option<String>,
}

impl Default for OpenForm {
    fn default() -> Self {
        Self {
            path: String::new(),
            cursor: "0".into(),
            passphrase: String::new(),
            root_share: String::new(),
            error: None,
            notice: None,
        }
    }
}

impl OpenForm {
    fn prepare_credentials(
        &mut self,
    ) -> Result<(PathBuf, SealedPassphrase, Zeroizing<[u8; 32]>), String> {
        if self.path.is_empty() {
            return Err("enter a container path".into());
        }
        let passphrase = SealedPassphrase::new(&self.passphrase)
            .ok_or_else(|| "passphrase is empty or too long".to_string())?;
        let kappa = Zeroizing::new(parse_kappa(&self.root_share).map_err(str::to_owned)?);
        self.passphrase.zeroize();
        self.root_share.zeroize();
        Ok((PathBuf::from(self.path.as_str()), passphrase, kappa))
    }

    fn prepare_request(&mut self) -> Result<OpenRequest, String> {
        let cursor = self
            .cursor
            .parse::<u64>()
            .map_err(|_| "cursor must be an unsigned integer".to_string())?;
        if cursor >= SAVES_BEFORE_DOMAIN_REUSE {
            return Err(format!(
                "cursor must be between 0 and {} for this demo pad",
                SAVES_BEFORE_DOMAIN_REUSE - 1
            ));
        }
        let (path, passphrase, kappa) = self.prepare_credentials()?;
        Ok(OpenRequest {
            path,
            cursor,
            passphrase,
            kappa,
        })
    }

    fn prepare_scan_request(&mut self) -> Result<ScanRequest, String> {
        let (path, passphrase, kappa) = self.prepare_credentials()?;
        Ok(ScanRequest {
            path,
            passphrase,
            kappa,
        })
    }

    fn clear_feedback_for_edits(
        &mut self,
        path_or_cursor_changed: bool,
        credentials_changed: bool,
    ) {
        if path_or_cursor_changed {
            self.error = None;
            self.notice = None;
        } else if credentials_changed {
            self.error = None;
        }
    }

    fn show(&mut self, ui: &mut egui::Ui, pending: Option<PendingDisplay>) -> FormAction {
        ui.heading("Open a container for read-only viewing");
        let mut path_or_cursor_changed = false;
        let mut credentials_changed = false;
        ui.add_enabled_ui(pending.is_none(), |ui| {
            ui.label("Container path");
            path_or_cursor_changed |= ui.text_edit_singleline(&mut self.path).changed();
            ui.label(format!(
                "Cursor used to write this image (0–{})",
                SAVES_BEFORE_DOMAIN_REUSE - 1
            ));
            path_or_cursor_changed |= ui.text_edit_singleline(&mut self.cursor).changed();
            ui.label("Legacy passphrase (ASCII case variants are equivalent)");
            credentials_changed |= ui
                .add(egui::TextEdit::singleline(&mut self.passphrase).password(true))
                .changed();
            ui.label("Demo root share (exactly 64 hexadecimal characters)");
            credentials_changed |= ui
                .add(egui::TextEdit::singleline(&mut self.root_share).password(true))
                .changed();
        });
        self.clear_feedback_for_edits(path_or_cursor_changed, credentials_changed);
        ui.label(
            "Credentials are only visually masked; clipboard, swap, crash dumps, and UI allocations may expose them.",
        );
        ui.label(format!(
            "Find cursor scans up to {SAVES_BEFORE_DOMAIN_REUSE} complete images and derives a key per attempt. It is read-only, may take time, and can be cancelled between attempts."
        ));
        let mut action = FormAction::None;
        match pending {
            Some(PendingDisplay {
                kind: PendingKind::Opening,
                ..
            }) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Opening read-only…");
                });
            }
            Some(PendingDisplay {
                kind: PendingKind::Scanning,
                completed_attempts,
            }) => {
                let total = SAVES_BEFORE_DOMAIN_REUSE as f32;
                ui.add(
                    egui::ProgressBar::new(completed_attempts as f32 / total).text(format!(
                        "{completed_attempts} of {SAVES_BEFORE_DOMAIN_REUSE} cursor candidates checked"
                    )),
                );
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Searching cursor candidates…");
                    if ui.button("Cancel search").clicked() {
                        action = FormAction::CancelSearch;
                    }
                });
            }
            None => {
                ui.horizontal(|ui| {
                    if ui.button("Open read-only").clicked() {
                        action = FormAction::Open;
                    }
                    if ui.button("Find cursor").clicked() {
                        action = FormAction::FindCursor;
                    }
                });
            }
        }
        if let Some(notice) = &self.notice {
            ui.colored_label(egui::Color32::from_rgb(30, 110, 60), notice);
        }
        if let Some(error) = &self.error {
            ui.colored_label(egui::Color32::from_rgb(190, 35, 35), error);
        }
        action
    }
}

impl Drop for OpenForm {
    fn drop(&mut self) {
        self.passphrase.zeroize();
        self.root_share.zeroize();
    }
}

struct OpenRequest {
    path: PathBuf,
    cursor: u64,
    passphrase: SealedPassphrase,
    kappa: Zeroizing<[u8; 32]>,
}

struct ScanRequest {
    path: PathBuf,
    passphrase: SealedPassphrase,
    kappa: Zeroizing<[u8; 32]>,
}

#[derive(Clone, Copy)]
enum PendingKind {
    Opening,
    Scanning,
}

#[derive(Clone, Copy)]
struct PendingDisplay {
    kind: PendingKind,
    completed_attempts: u64,
}

enum FormAction {
    None,
    Open,
    FindCursor,
    CancelSearch,
}

enum WorkerResult {
    Open(Result<Box<OpenedView>, String>),
    Scan(Result<CursorSearch, String>),
}

struct PendingTask {
    kind: PendingKind,
    receiver: Receiver<WorkerResult>,
    cancel: Option<Arc<AtomicBool>>,
    progress: Option<Arc<AtomicU64>>,
}

struct ViewerApp {
    form: OpenForm,
    pending: Option<PendingTask>,
    viewer: Option<ReadOnlyWindow>,
}

impl ViewerApp {
    fn new() -> Self {
        Self {
            form: OpenForm::default(),
            pending: None,
            viewer: None,
        }
    }

    fn start_open(&mut self) {
        self.form.error = None;
        self.form.notice = None;
        let request = match self.form.prepare_request() {
            Ok(request) => request,
            Err(error) => {
                self.form.error = Some(error);
                return;
            }
        };
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let OpenRequest {
                path,
                cursor,
                passphrase,
                kappa,
            } = request;
            let result = open_read_only(&path, cursor, passphrase, *kappa).map(Box::new);
            let _ = sender.send(WorkerResult::Open(result));
        });
        self.pending = Some(PendingTask {
            kind: PendingKind::Opening,
            receiver,
            cancel: None,
            progress: None,
        });
    }

    fn start_scan(&mut self) {
        self.form.error = None;
        self.form.notice = None;
        let request = match self.form.prepare_scan_request() {
            Ok(request) => request,
            Err(error) => {
                self.form.error = Some(error);
                return;
            }
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let worker_cancelled = Arc::clone(&cancelled);
        let progress = Arc::new(AtomicU64::new(0));
        let worker_progress = Arc::clone(&progress);
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let ScanRequest {
                path,
                passphrase,
                kappa,
            } = request;
            let result = find_read_only_cursor(
                &path,
                passphrase,
                *kappa,
                &worker_cancelled,
                &worker_progress,
            );
            let _ = sender.send(WorkerResult::Scan(result));
        });
        self.pending = Some(PendingTask {
            kind: PendingKind::Scanning,
            receiver,
            cancel: Some(cancelled),
            progress: Some(progress),
        });
    }

    fn cancel_scan(&mut self) {
        if let Some(cancel) = self.pending.as_ref().and_then(|task| task.cancel.as_ref()) {
            cancel.store(true, Ordering::Relaxed);
            self.form.notice = Some(
                "Cancel requested. The current full-image attempt will finish before stopping."
                    .into(),
            );
        }
    }

    fn finish_pending(&mut self) {
        let result = self.pending.as_ref().and_then(|task| {
            match task.receiver.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Disconnected) => Some(match task.kind {
                    PendingKind::Opening => WorkerResult::Open(Err(
                        "read-only open worker stopped unexpectedly".into(),
                    )),
                    PendingKind::Scanning => WorkerResult::Scan(Err(
                        "cursor search worker stopped unexpectedly".into(),
                    )),
                }),
                Err(TryRecvError::Empty) => None,
            }
        });
        if let Some(result) = result {
            let task = self.pending.take().expect("pending task exists");
            match result {
                WorkerResult::Open(Ok(opened)) => {
                    self.viewer = Some(ReadOnlyWindow::new(opened.document, opened.cursor));
                }
                WorkerResult::Open(Err(error)) => self.form.error = Some(error),
                WorkerResult::Scan(Ok(found)) => {
                    self.form.cursor = found.cursor.to_string();
                    self.form.notice = Some(format!(
                        "Cursor {} authenticated on attempt {} with the search credentials. Re-enter those credentials to open. This is not proof of freshness, uniqueness or pad history.",
                        found.cursor, found.attempts
                    ));
                }
                WorkerResult::Scan(Err(error)) => {
                    if task
                        .cancel
                        .as_ref()
                        .is_some_and(|cancel| cancel.load(Ordering::Relaxed))
                    {
                        self.form.notice = Some(error);
                    } else {
                        self.form.error = Some(error);
                    }
                }
            }
        }
    }
}

impl eframe::App for ViewerApp {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.finish_pending();
        if self.pending.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
        if let Some(viewer) = &mut self.viewer {
            eframe::App::logic(viewer, ctx, frame);
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        if let Some(viewer) = &mut self.viewer {
            eframe::App::raw_input_hook(viewer, ctx, raw);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if let Some(viewer) = &mut self.viewer {
            eframe::App::ui(viewer, ui, frame);
            return;
        }
        egui::Panel::top("holonomy_demo_warning").show(ui, |ui| {
            ui.colored_label(egui::Color32::from_rgb(190, 35, 35), DEMO_WARNING);
            ui.colored_label(egui::Color32::from_rgb(190, 35, 35), READ_ONLY_RULE);
            ui.colored_label(egui::Color32::GRAY, READ_ONLY_LIMITS);
        });
        let pending_display = self.pending.as_ref().map(|task| PendingDisplay {
            kind: task.kind,
            completed_attempts: task
                .progress
                .as_ref()
                .map_or(0, |progress| progress.load(Ordering::Relaxed)),
        });
        match self.form.show(ui, pending_display) {
            FormAction::Open => self.start_open(),
            FormAction::FindCursor => self.start_scan(),
            FormAction::CancelSearch => self.cancel_scan(),
            FormAction::None => {}
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.is_empty() {
        return interactive_open();
    }
    if args.len() == 1 && args[0].to_str() == Some("--help") {
        print_usage();
        return ExitCode::SUCCESS;
    }
    if args.len() == 1 {
        match args[0].to_str() {
            Some("--version") => {
                println!("holonomy-ui {}", holonomy_core::VERSION);
                return ExitCode::SUCCESS;
            }
            Some("--check") => return self_check(),
            _ => {}
        }
    }

    let launch = match parse_args(&args) {
        Ok(launch) => launch,
        Err(error) => {
            eprintln!("holonomy-ui: {error}");
            return ExitCode::from(2);
        }
    };
    match launch {
        Launch::InspectOnly(path, cursor) => inspect_only(&path, cursor),
        Launch::View(path, cursor) => view(&path, cursor),
    }
}

fn print_usage() {
    println!("{USAGE}");
    println!("With no arguments, launch the interactive read-only open screen.");
    println!();
    println!("The viewer uses a deterministic demo pad, not physical USB.");
    println!("It can search candidate cursors but cannot repair or reconcile recovery. Never use sensitive data.");
    println!(
        "Interactive credentials are visually masked only; clipboard, swap, crash dumps, and UI allocations may expose them."
    );
}

fn interactive_open() -> ExitCode {
    announce_mode();
    match eframe::run_native(
        TITLE,
        eframe::NativeOptions::default(),
        Box::new(|_| Ok(Box::new(ViewerApp::new()))),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Could not start the read-only open screen: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Decode without creating a window; prints metadata but never document text.
fn inspect_only(path: &Path, cursor: u64) -> ExitCode {
    announce_mode();
    let (passphrase, kappa) = match prompt_credentials(false) {
        Ok(credentials) => credentials,
        Err(error) => {
            eprintln!("holonomy-ui: {error}");
            return ExitCode::FAILURE;
        }
    };
    let opened = match open_read_only(path, cursor, passphrase, kappa) {
        Ok(opened) => opened,
        Err(error) => {
            eprintln!("holonomy-ui: {error}");
            return ExitCode::FAILURE;
        }
    };
    let body_characters = opened.document.plain_text(StoryRef::Body).chars().count();
    println!("Container: {}", path.display());
    println!("Cursor authenticated: {}", opened.cursor);
    println!(
        "Wordcraft document decoded: {} body character(s); the text is not printed",
        body_characters
    );
    println!("{DEMO_WARNING}");
    println!("{READ_ONLY_RULE}");
    println!("{READ_ONLY_LIMITS}");
    println!("No window was opened, no container byte was written, and the pad was not ratcheted.");
    ExitCode::SUCCESS
}

fn view(path: &Path, cursor: u64) -> ExitCode {
    announce_mode();
    let (passphrase, kappa) = match prompt_credentials(false) {
        Ok(credentials) => credentials,
        Err(error) => {
            eprintln!("holonomy-ui: {error}");
            return ExitCode::FAILURE;
        }
    };
    let opened = match open_read_only(path, cursor, passphrase, kappa) {
        Ok(opened) => opened,
        Err(error) => {
            eprintln!("holonomy-ui: {error}");
            return ExitCode::FAILURE;
        }
    };
    match eframe::run_native(
        TITLE,
        eframe::NativeOptions::default(),
        Box::new(move |_| {
            Ok(Box::new(ReadOnlyWindow::new(
                opened.document,
                opened.cursor,
            )))
        }),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Could not start the read-only window: {error}");
            ExitCode::FAILURE
        }
    }
}

fn announce_mode() {
    eprintln!("{DEMO_WARNING}");
    eprintln!("{READ_ONLY_RULE}");
    eprintln!("{READ_ONLY_LIMITS}");
}

/// Run selected invariants; this is not a security audit.
fn self_check() -> ExitCode {
    let mut ok = true;
    println!("holonomy {} self-check", holonomy_core::VERSION);

    let capsule = std::mem::size_of::<holonomy_core::capsule::StratumRootCapsuleV5>();
    report(
        "capsule is exactly one 65,536-byte slot",
        capsule == 65_536,
        &format!("{capsule} bytes"),
    );
    ok &= capsule == 65_536;

    let geometry = holonomy_usb::Geometry::nominal();
    report(
        "8 GB device fits the documented pad budget",
        geometry.meets_pad_budget(),
        &format!("{} saves afforded", geometry.saves_afforded()),
    );
    ok &= geometry.meets_pad_budget();

    let permutation = holonomy_core::feistel::Feistel22::provisioned(&[0x5A; 32]);
    let above_2_20 = (0..100_000u32)
        .filter(|&index| {
            permutation.permute(holonomy_core::feistel::Feistel22::logical_sector(0, index))
                >= (1 << 20)
        })
        .count();
    report(
        "22-bit Feistel reaches sectors above 2^20",
        above_2_20 > 35_000,
        &format!("{above_2_20}/100,000 samples"),
    );
    ok &= above_2_20 > 35_000;

    let steady = holonomy_core::budgets::STEADY_STATE_CEILING_BYTES;
    let peak = holonomy_core::budgets::UNLOCK_PEAK_CEILING_BYTES;
    let fits = holonomy_core::budgets::check_against_envelope(steady, peak).is_ok();
    report(
        "memory envelope fits a 2 GiB host",
        fits,
        &format!(
            "{} MiB steady, {} MiB peak",
            steady / holonomy_core::budgets::MIB,
            peak / holonomy_core::budgets::MIB
        ),
    );
    ok &= fits;

    let capability = holonomy_crypto::lock::probe_capability();
    println!("  [info] memory locking: {}", capability.claim());
    println!(
        "  [info] capture exclusion: {}",
        holonomy_ui::CaptureReport::platform_default().describe()
    );
    println!();
    if ok {
        println!("selected checks passed; this is not a security audit");
        ExitCode::SUCCESS
    } else {
        println!("one or more invariants failed");
        ExitCode::FAILURE
    }
}

fn report(what: &str, pass: bool, detail: &str) {
    println!("  [{}] {what} — {detail}", if pass { "ok" } else { "FAIL" });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn view_modes_are_explicit_and_bounded() {
        assert_eq!(
            parse_args(&args(&["--view", "book", "3"])),
            Ok(Launch::View(PathBuf::from("book"), 3))
        );
        assert_eq!(
            parse_args(&args(&["--inspect-only", "book", "0"])),
            Ok(Launch::InspectOnly(PathBuf::from("book"), 0))
        );
        assert!(parse_args(&args(&[])).is_err());
        assert!(parse_args(&args(&["book"])).is_err());
        assert!(parse_args(&args(&["--view", "book"])).is_err());
        assert!(parse_args(&args(&["--view", "book", "1", "extra"])).is_err());
        assert!(parse_args(&args(&["--device", "/dev/sda", "0"])).is_err());
        assert!(parse_args(&args(&["--view", "book", "-1"])).is_err());
        assert!(parse_args(&args(&["--view", "book", "not-a-number"])).is_err());
    }

    #[test]
    fn preparing_an_open_request_clears_gui_credential_fields() {
        let mut form = OpenForm {
            path: "demo-container".into(),
            cursor: "4".into(),
            passphrase: "demo passphrase".into(),
            root_share: "11".repeat(32),
            error: None,
            notice: None,
        };
        let request = form.prepare_request().expect("valid open form");
        assert_eq!(request.path, PathBuf::from("demo-container"));
        assert_eq!(request.cursor, 4);
        assert!(form.passphrase.is_empty());
        assert!(form.root_share.is_empty());
    }

    #[test]
    fn preparing_a_scan_request_clears_credentials_without_requiring_a_cursor() {
        let mut form = OpenForm {
            path: "demo-container".into(),
            cursor: "cursor is discovered by the scan".into(),
            passphrase: "demo passphrase".into(),
            root_share: "11".repeat(32),
            error: None,
            notice: None,
        };
        let request = form.prepare_scan_request().expect("valid scan credentials");
        assert_eq!(request.path, PathBuf::from("demo-container"));
        assert!(form.passphrase.is_empty());
        assert!(form.root_share.is_empty());
    }

    #[test]
    fn candidate_feedback_survives_credential_reentry_but_not_path_or_cursor_edits() {
        let mut form = OpenForm::default();
        form.notice = Some("candidate cursor found".into());
        form.error = Some("previous validation error".into());

        form.clear_feedback_for_edits(false, true);
        assert!(
            form.notice.is_some(),
            "re-entering credentials must retain the search result"
        );
        assert!(form.error.is_none(), "credential edits clear stale errors");

        form.clear_feedback_for_edits(true, false);
        assert!(
            form.notice.is_none(),
            "a path or cursor edit invalidates the result"
        );
    }

    #[test]
    fn open_form_rejects_the_pad_reuse_boundary_before_request_creation() {
        let mut form = OpenForm {
            path: "demo-container".into(),
            cursor: SAVES_BEFORE_DOMAIN_REUSE.to_string(),
            passphrase: String::new(),
            root_share: String::new(),
            error: None,
            notice: None,
        };
        let error = match form.prepare_request() {
            Err(error) => error,
            Ok(_) => panic!("reuse boundary must not create an open request"),
        };
        assert_eq!(
            error,
            format!(
                "cursor must be between 0 and {} for this demo pad",
                SAVES_BEFORE_DOMAIN_REUSE - 1
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn open_and_cursor_search_workers_use_read_only_paths() {
        use std::time::{Duration, Instant};

        use holonomy_cli::document::DocumentSessionOptions;
        use holonomy_cli::store::pad::DerivedPad;
        use holonomy_core::unlock::Topology;
        use holonomy_wordcraft::editor::EditorSession;

        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("container");
        let passphrase = "synthetic UI test passphrase";
        let root_share = [0x11; 32];
        let session = EditorSession::create(
            &path,
            Box::new(DerivedPad::new()),
            SealedPassphrase::new(passphrase).expect("valid test passphrase"),
            root_share,
            DocumentSessionOptions::new(0, true, Topology::UmbilicalUsb),
        )
        .expect("create test container");
        drop(session);
        let original_image = std::fs::read(&path).expect("read created container");

        let mut app = ViewerApp::new();
        app.form = OpenForm {
            path: path.display().to_string(),
            cursor: "not-required-for-search".into(),
            passphrase: passphrase.into(),
            root_share: "11".repeat(32),
            error: None,
            notice: None,
        };
        app.start_scan();
        assert!(app.form.passphrase.is_empty());
        assert!(app.form.root_share.is_empty());

        let deadline = Instant::now() + Duration::from_secs(90);
        while app.pending.is_some() && Instant::now() < deadline {
            app.finish_pending();
            if app.pending.is_some() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        assert!(app.pending.is_none(), "cursor search worker did not finish in time");
        assert_eq!(app.form.cursor, "0");
        assert!(app
            .form
            .notice
            .as_deref()
            .is_some_and(|notice| notice.contains("not proof of freshness")));
        assert!(app.viewer.is_none(), "scan must not open a document");
        assert_eq!(
            std::fs::read(&path).expect("read after cursor search"),
            original_image,
            "cursor search must leave the container bytes unchanged"
        );

        // The scan deliberately cleared credentials; re-enter them for the separate viewer open.
        app.form.passphrase = passphrase.into();
        app.form.root_share = "11".repeat(32);
        app.start_open();
        assert!(app.form.passphrase.is_empty());
        assert!(app.form.root_share.is_empty());
        let deadline = Instant::now() + Duration::from_secs(90);
        while app.pending.is_some() && Instant::now() < deadline {
            app.finish_pending();
            if app.pending.is_some() {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        assert!(app.pending.is_none(), "open worker did not finish in time");
        let viewer = app.viewer.as_ref().unwrap_or_else(|| {
            panic!(
                "container open failed: {}",
                app.form.error.as_deref().unwrap_or("no error reported")
            )
        });
        assert_eq!(viewer.cursor, 0);
        assert!(viewer.app.session.path.is_none());
        assert!(!viewer.app.session.recovery_ready());
        assert_eq!(
            std::fs::read(&path).expect("read after read-only open"),
            original_image,
            "read-only open must leave the container bytes unchanged"
        );
    }

    #[test]
    fn the_view_window_has_no_path_or_recovery_state() {
        let window = ReadOnlyWindow::new(Document::from_text("viewed text"), 3);
        assert!(window.app.session.path.is_none());
        assert!(!window.app.session.recovery_ready());
        assert!(!window.quit);
        assert_eq!(window.dropped_events, 0);
    }
}
