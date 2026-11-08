//! A read-only Wordcraft window over a Holonomy container.
//!
//! This is the first place the real editor renders a real container. It is deliberately one-way:
//! the document is decoded once, the container handle is dropped, every host service that could
//! write refuses, and the input stream is filtered so no command can be invoked at all.
//!
//! Two modes, and the second exists so CI can prove the container path without a display:
//!
//! ```text
//! holonomy-rich-view --view /path/to/container CURSOR
//! holonomy-rich-view --inspect-only /path/to/container CURSOR
//! ```
//!
//! Credentials are read from hidden terminal prompts. The pad is the deterministic stand-in: this
//! binary has no `--device`, so nothing here touches physical USB.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitCode;

use eframe::egui;
use holonomy_preview::{
    filter_events, open_read_only, prompt_credentials, read_only_services, ViewInput,
    READ_ONLY_LIMITS, READ_ONLY_RULE,
};
use wordcraft_doc::{Document, StoryRef};
use wordcraft_engine::Session;
use wordcraft_ui_egui::WordApp;

const USAGE: &str = "use --view PATH CURSOR or --inspect-only PATH CURSOR";
const TITLE: &str = "Holonomy — READ-ONLY WORDCRAFT VIEW";

#[derive(Debug, PartialEq, Eq)]
enum Launch {
    /// Open a window.
    View(PathBuf, u64),
    /// Decode and report, then exit without a window. Used by CI and by anyone on a headless host.
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

/// The window. Holds no path, no store and no writable handle: only the document it renders.
struct ReadOnlyWindow {
    app: WordApp,
    quit: bool,
    dropped_events: usize,
}

impl ReadOnlyWindow {
    fn new(document: Document) -> Self {
        // No `enable_recovery`, no path, no profile directory: the document exists only in memory.
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
            ..
        } = filter_events(raw);
        self.dropped_events = self.dropped_events.saturating_add(dropped);
        if quit_requested {
            self.quit = true;
        }
        self.app.raw_input_hook(ctx, raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("holonomy_read_only_banner").show(ui, |ui| {
            ui.colored_label(egui::Color32::from_rgb(190, 35, 35), READ_ONLY_RULE);
            ui.colored_label(egui::Color32::GRAY, READ_ONLY_LIMITS);
            ui.label("Press Ctrl+Q to close. The window ignores every other key and click.");
        });
        self.app.ui(ui);
    }
}

fn main() -> ExitCode {
    let args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let launch = match parse_args(&args) {
        Ok(launch) => launch,
        Err(error) => {
            eprintln!("holonomy-rich-view: {error}");
            return ExitCode::from(2);
        }
    };
    match launch {
        Launch::InspectOnly(path, cursor) => inspect_only(&path, cursor),
        Launch::View(path, cursor) => view(&path, cursor),
    }
}

/// Decode without a window, print what was found, and exit. Writes nothing.
fn inspect_only(path: &std::path::Path, cursor: u64) -> ExitCode {
    eprintln!("{READ_ONLY_RULE}");
    eprintln!("{READ_ONLY_LIMITS}");
    let (passphrase, kappa) = match prompt_credentials(false) {
        Ok(credentials) => credentials,
        Err(error) => {
            eprintln!("holonomy-rich-view: {error}");
            return ExitCode::FAILURE;
        }
    };
    let opened = match open_read_only(path, cursor, passphrase, kappa) {
        Ok(opened) => opened,
        Err(error) => {
            eprintln!("holonomy-rich-view: {error}");
            return ExitCode::FAILURE;
        }
    };
    // Counting characters creates no new copy of the text beyond the document already in memory.
    let body_characters = opened.document.plain_text(StoryRef::Body).chars().count();
    println!("Container: {}", path.display());
    println!("Cursor authenticated: {}", opened.cursor);
    println!(
        "Wordcraft document decoded: {} body character(s); the text is not printed",
        body_characters
    );
    println!("{}", READ_ONLY_RULE);
    println!("{}", READ_ONLY_LIMITS);
    println!("No window was opened, no container byte was written, and the pad was not ratcheted.");
    ExitCode::SUCCESS
}

fn view(path: &std::path::Path, cursor: u64) -> ExitCode {
    // Said before any prompt touches the terminal, so a wrapper can see the mode even if the
    // window never opens.
    eprintln!("{READ_ONLY_RULE}");
    eprintln!("{READ_ONLY_LIMITS}");
    let (passphrase, kappa) = match prompt_credentials(false) {
        Ok(credentials) => credentials,
        Err(error) => {
            eprintln!("holonomy-rich-view: {error}");
            return ExitCode::FAILURE;
        }
    };
    let opened = match open_read_only(path, cursor, passphrase, kappa) {
        Ok(opened) => opened,
        Err(error) => {
            eprintln!("holonomy-rich-view: {error}");
            return ExitCode::FAILURE;
        }
    };
    let options = eframe::NativeOptions::default();
    match eframe::run_native(
        TITLE,
        options,
        Box::new(move |_| Ok(Box::new(ReadOnlyWindow::new(opened.document)))),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Could not start the read-only window: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(|value| OsString::from(*value)).collect()
    }

    #[test]
    fn launch_forms_are_explicit_and_bounded() {
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
        assert!(parse_args(&args(&["--demo-create", "book", "1"])).is_err());
        assert!(parse_args(&args(&["--view", "book", "-1"])).is_err());
        assert!(parse_args(&args(&["--view", "book", "not-a-number"])).is_err());
    }

    /// The window's session has no file, no recovery and no dirty document, so Wordcraft has
    /// nothing to save even if a command were reachable.
    #[test]
    fn the_window_holds_no_file_or_recovery_state() {
        let window = ReadOnlyWindow::new(Document::from_text("viewed text"));
        assert!(window.app.session.path.is_none());
        assert!(!window.app.session.recovery_ready());
        assert!(!window.quit);
        assert_eq!(window.dropped_events, 0);
    }
}
