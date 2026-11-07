//! The experimental, read-only Wordcraft container viewer.
//!
//! This binary opens containers only through the deterministic demo pad. It does not support
//! physical USB, editing, saving, or recovery, and it is not suitable for sensitive data. The
//! read-only service boundary and input filter are shared with `holonomy-preview`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use eframe::egui;
use holonomy_preview::{
    filter_events, open_read_only, prompt_credentials, read_only_services, ViewInput,
    READ_ONLY_LIMITS, READ_ONLY_RULE,
};
use wordcraft_doc::{Document, StoryRef};
use wordcraft_engine::Session;
use wordcraft_ui_egui::WordApp;

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
    quit: bool,
    dropped_events: usize,
}

impl ReadOnlyWindow {
    fn new(document: Document) -> Self {
        Self {
            app: WordApp::new(Session::new(document), read_only_services()),
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
            ui.label("Press Ctrl+Q to close. The window ignores every other key and click.");
        });
        self.app.ui(ui);
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    if args.is_empty() || (args.len() == 1 && args[0].to_str() == Some("--help")) {
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
    println!();
    println!("The viewer is read-only and uses a deterministic demo pad, not physical USB.");
    println!("It cannot edit, save, or recover containers. Never use sensitive data.");
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
        Box::new(move |_| Ok(Box::new(ReadOnlyWindow::new(opened.document)))),
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
    fn the_view_window_has_no_path_or_recovery_state() {
        let window = ReadOnlyWindow::new(Document::from_text("viewed text"));
        assert!(window.app.session.path.is_none());
        assert!(!window.app.session.recovery_ready());
        assert!(!window.quit);
        assert_eq!(window.dropped_events, 0);
    }
}
