//! Standalone pure-`libc` X11 desktop entry point for Holonomy (`quiet-md` layout, live
//! `ToolbarState` Ribbon, embedded `Inter` + `JetBrains Mono` A8 rasterization, security dot &
//! residency overlay, Block 8 history timeline & bookmark drawer, clipboard, autosave, and
//! emergency memory-wipe reaper).
//!
//! ```text
//! holonomy-gui [--deadman <ms>] [document.wavefunction]
//! ```

use std::path::PathBuf;
use std::time::Duration;

use holonomy_gui::canvas::{run_desktop, DesktopEnd, GuiWipeUnconfirmed};

const USAGE: &str = "usage:\n  holonomy-gui [--deadman <ms>] [document.wavefunction]";

fn parse_gui_args(args: &[String]) -> Option<(Option<PathBuf>, Option<Duration>)> {
    let mut deadman = None;
    let mut doc = None;
    let mut idx = 0usize;
    while idx < args.len() {
        match args[idx].as_str() {
            "--deadman" => {
                let raw = args.get(idx + 1)?;
                let ms = raw.parse::<u64>().ok().filter(|&v| v > 0)?;
                deadman = Some(Duration::from_millis(ms));
                idx += 2;
            }
            flag if flag.starts_with('-') => return None,
            path => {
                if doc.is_some() {
                    return None;
                }
                doc = Some(PathBuf::from(path));
                idx += 1;
            }
        }
    }
    Some((doc, deadman))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if matches!(args.as_slice(), [f] if f == "-h" || f == "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    let Some((doc, deadman)) = parse_gui_args(&args) else {
        eprintln!("{USAGE}");
        std::process::exit(2);
    };
    match run_desktop(doc.as_deref(), deadman)? {
        DesktopEnd::Saved
        | DesktopEnd::NothingToWrite
        | DesktopEnd::Cancelled
        | DesktopEnd::GoHome => Ok(()),
        DesktopEnd::SaveFailed => {
            eprintln!(
                "holonomy-gui: final save refused — bytes after last witness are unpersisted"
            );
            std::process::exit(1);
        }
        DesktopEnd::Wiped(receipt) => {
            println!(
                "emergency wipe confirmed — {} bytes in {} arenas, {} in-place bytes, {} canaries clean, {} us",
                receipt.bytes_wiped,
                receipt.arenas_wiped,
                receipt.in_place_bytes,
                receipt.canaries,
                receipt.trip_to_verified_us,
            );
            Ok(())
        }
        DesktopEnd::WipeUnconfirmed(reason, in_place) => {
            match reason {
                GuiWipeUnconfirmed::NoOutcome => eprintln!(
                    "emergency wipe NOT confirmed (reaper produced no outcome; scrubbed {in_place} bytes in place)"
                ),
                GuiWipeUnconfirmed::NothingArmed => eprintln!(
                    "emergency wipe NOT confirmed (no arenas armed; scrubbed {in_place} bytes in place)"
                ),
                GuiWipeUnconfirmed::Ghosts {
                    ghost_canaries,
                    ghost_bytes,
                } => eprintln!(
                    "emergency wipe FAILED: {ghost_canaries} ghost canaries ({ghost_bytes} bytes) survived; scrubbed {in_place} bytes in place"
                ),
            }
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_gui_cli_arguments() {
        assert_eq!(parse_gui_args(&[]), Some((None, None)));
        assert_eq!(
            parse_gui_args(&["notes.wavefunction".to_owned()]),
            Some((Some(PathBuf::from("notes.wavefunction")), None))
        );
        assert_eq!(
            parse_gui_args(&[
                "--deadman".to_owned(),
                "5000".to_owned(),
                "notes.wavefunction".to_owned()
            ]),
            Some((
                Some(PathBuf::from("notes.wavefunction")),
                Some(Duration::from_millis(5000))
            ))
        );
        assert_eq!(parse_gui_args(&["--bogus".to_owned()]), None);
    }
}
