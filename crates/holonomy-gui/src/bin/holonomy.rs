//! The terminal app: Holonomy, open and typeable (`docs/TO-APP.md` M4b-i).
//!
//! ```text
//! cargo run -p holonomy-gui --bin holonomy
//! ```
//!
//! What this is: a real window in the sense that matters — you open it, you type, the text appears
//! on the geometry the Eye measured, arrows move a caret, `Ctrl+Z` takes back a typing burst,
//! `Ctrl+S` writes the open leaf through. It runs on the terminal's own cell grid, so the geometry
//! and the screen are the same thing (`CellShaper`), which is what keeps paint and hit-test honest
//! down to the last cell.
//!
//! What this is not, yet: durable. The document lives in memory for the length of the session; a
//! closing window loses it, and nothing is sealed to a `.wavefunction` file. That is **M5** (open,
//! save, unlock), and it is the next step: the world-line is already on disk-capable
//! (`holonomy_line::WorldLine`) and the shape here — a `SegmentSink` that appends, a `SegmentSource`
//! that reads back — is the socket it plugs into.
//!
//! The parts that need a tty live here and only here: raw mode, the window size, the read loop.
//! Everything they call is tested without one (`term.rs`, `tests/terminal.rs`).

use holonomy_eye::Fixed;
use holonomy_gui::term::{self, CellShaper, Decoder, Input};
use holonomy_gui::{App, CellSurface, Key, Keymap};
use holonomy_shell::Editor;
use holonomy_text::{Appended, ResidentSet, SegmentId, SegmentSink, SegmentSource, Typing};
use std::io::{Read, Write};

/// The document's world-line, in memory: the sink appends, the source reads back.
///
/// One object doing both is what M5 will not do — a real world-line is a file, opened under a
/// passphrase — but it is the same socket, and the app cannot tell the difference.
#[derive(Debug, Clone, Default)]
struct Memory(std::sync::Arc<std::sync::Mutex<Vec<Vec<u8>>>>);

impl SegmentSink for Memory {
    fn append_segment(&mut self, plaintext: &[u8]) -> Result<Appended, String> {
        let mut segments = self.0.lock().map_err(|_| "the store is poisoned")?;
        let id = segments.len() as u64;
        segments.push(plaintext.to_vec());
        Ok(Appended {
            segment: SegmentId::new(id),
            witness_sequence: id + 1,
            segment_root: [0u8; 32],
            bytes_written: plaintext.len() as u64,
        })
    }
}

impl SegmentSource for Memory {
    fn load(&mut self, segment: SegmentId) -> Result<Vec<u8>, String> {
        let segments = self.0.lock().map_err(|_| "the store is poisoned")?;
        segments
            .get(usize::try_from(segment.get()).unwrap_or(usize::MAX))
            .cloned()
            .ok_or_else(|| format!("no {segment}"))
    }
}

type Window = App<Memory, Memory, CellShaper, CellSurface>;

/// The terminal in raw mode, restored when it is dropped.
struct RawMode {
    saved: libc::termios,
}

impl RawMode {
    fn enable() -> std::io::Result<Self> {
        // SAFETY: `tcgetattr`/`tcsetattr` are called with a valid fd (0 and 1 are the tty this
        // process was started on) and a fully initialised `termios` this function owns.
        unsafe {
            let mut current: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut current) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let saved = current;
            current.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
            current.c_iflag &= !(libc::IXON | libc::ICRNL);
            current.c_cc[libc::VMIN] = 1;
            current.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &current) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self { saved })
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: restoring the exact `termios` read in `enable`, on the same fd.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.saved);
        }
        // Leave the cursor visible and the screen clean.
        print!("\x1b[0m\x1b[{};1H\x1b[J", 24);
        let _ = std::io::stdout().flush();
    }
}

/// The terminal's size in cells, or a sane 80×24 when it cannot be asked.
fn size() -> (usize, usize) {
    // SAFETY: `ioctl` with `TIOCGWINSZ` writes a `winsize` this function owns.
    let size = unsafe {
        let mut size: libc::winsize = std::mem::zeroed();
        if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) == 0 {
            Some((usize::from(size.ws_col), usize::from(size.ws_row)))
        } else {
            None
        }
    };
    match size {
        Some((columns, rows)) if columns >= 20 && rows >= 4 => (columns, rows),
        _ => (80, 24),
    }
}

fn window(columns: usize, rows: usize) -> Window {
    let store = Memory::default();
    let residency = ResidentSet::new(store.clone(), 64 * 1024, 0).expect("a budget");
    let editor = Editor::new(
        Typing::new(store, 64 * 1024),
        residency,
        CellShaper,
        Fixed::units(columns as i64),
        Fixed::units(rows as i64),
    );
    let surface = CellSurface::new(columns, rows, 1);
    App::new(editor, Keymap::new(Fixed::units(rows as i64)), surface)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut app = window(80, 23);
    let raw = RawMode::enable()?;
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut decoder = Decoder::new();
    let mut bytes = [0u8; 256];
    print!("\x1b[?25l\x1b[2J");
    loop {
        let (columns, rows) = size();
        let text_rows = rows.saturating_sub(1).max(1);
        app.surface_mut().resize(columns, text_rows);
        app.resize(Fixed::units(columns as i64), Fixed::units(text_rows as i64))?;
        app.draw()?;
        let status = format!(
            " holonomy · {} bytes · in memory · ^S save · ^Q quit ",
            app.document_len()
        );
        write!(stdout, "{}", term::screen(app.surface(), columns, &status))?;
        if let Some((column, row)) = app.surface().caret_cell() {
            write!(stdout, "\x1b[?25h\x1b[{};{}H", row + 1, column + 1)?;
        }
        stdout.flush()?;

        let read = stdin.read(&mut bytes)?;
        if read == 0 {
            break;
        }
        let mut quit = false;
        let mut inputs = Vec::new();
        for byte in &bytes[..read] {
            if let Some(input) = decoder.feed(*byte) {
                inputs.push(input);
            }
        }
        // A read that ends mid-sequence leaves a lone Escape behind: deliver it.
        inputs.extend(decoder.flush_escape());
        for input in inputs {
            if let Input::Key(key, modifiers) = input {
                if modifiers.control {
                    match key {
                        Key::Character('q') | Key::Character('c') => {
                            quit = true;
                            continue;
                        }
                        _ => {}
                    }
                }
                app.press(key, modifiers)?;
            }
        }
        if quit {
            break;
        }
    }
    drop(raw);
    Ok(())
}
