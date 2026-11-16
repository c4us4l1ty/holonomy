//! An opt-in, unencrypted Wordcraft scratch window for integration development.
//!
//! This is intentionally not a Holonomy container editor. Wordcraft's own file,
//! export, clipboard and other functions can put document data outside a container.

use std::process::ExitCode;

use eframe::egui;
use wordcraft_doc::Document;
use wordcraft_engine::Session;
use wordcraft_ui_egui::{Services, WordApp};

const OPT_IN: &str = "--unencrypted-scratch-preview";
const WARNING: &str = "UNENCRYPTED SCRATCH PREVIEW — NOT A HOLONOMY CONTAINER. Do not enter sensitive or irreplaceable data. Wordcraft file, export, print and clipboard actions may expose plaintext.";

struct ScratchWindow {
    app: WordApp,
}

impl ScratchWindow {
    fn new() -> Self {
        // No Holonomy store, credentials, auto-recovery or profile directory is
        // supplied. Upstream file/export features are still plaintext, not sandboxed.
        Self {
            app: WordApp::new(Session::new(Document::new()), Services::default()),
        }
    }
}

impl eframe::App for ScratchWindow {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.app.logic(ctx);
        let closing = ctx.input(|input| input.viewport().close_requested());
        if closing && !self.app.close_requested() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        } else if self.app.quit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.app.raw_input_hook(ctx, raw);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("holonomy_scratch_warning").show(ui, |ui| {
            ui.colored_label(egui::Color32::from_rgb(190, 35, 35), WARNING);
        });
        self.app.ui(ui);
    }
}

fn opted_in(args: &[String]) -> bool {
    args.len() == 1 && args[0] == OPT_IN
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !opted_in(&args) {
        eprintln!(
            "{WARNING}\nTo deliberately launch this development preview: holonomy-preview {OPT_IN}"
        );
        return ExitCode::FAILURE;
    }
    eprintln!("{WARNING}");
    let options = eframe::NativeOptions::default();
    match eframe::run_native(
        "Holonomy — UNENCRYPTED SCRATCH PREVIEW",
        options,
        Box::new(|_| Ok(Box::new(ScratchWindow::new()))),
    ) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Could not start the scratch window: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_requires_the_explicit_single_opt_in_flag() {
        assert!(!opted_in(&[]));
        assert!(!opted_in(&["--help".into()]));
        assert!(!opted_in(&[OPT_IN.into(), "unexpected".into()]));
        assert!(opted_in(&[OPT_IN.into()]));
    }

    #[test]
    fn scratch_starts_blank_without_a_file_path_or_recovery() {
        let app = ScratchWindow::new().app;
        assert!(app.session.doc.same_content(&Document::new()));
        assert!(app.session.path.is_none());
        assert!(!app.session.recovery_ready());
    }
}
