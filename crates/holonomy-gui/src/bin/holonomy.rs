//! The terminal app: Holonomy, open, typeable and durable (`docs/TO-APP.md` M4b-i and M5).
//!
//! ```text
//! cargo run -p holonomy-gui --bin holonomy -- notes.wavefunction
//! ```
//!
//! The window: a terminal in raw mode, a grid of cells the Eye measured, keys decoded from the
//! bytes a tty actually sends. The document: a `.wavefunction` world-line under a passphrase —
//! created if the path does not exist, opened if it does. Every sealed leaf is one append and one
//! witness, so `Ctrl+S`, the autosave that fires when typing pauses, and the seal on quit all go
//! through the same commit path the L-6 rehearsal measures.
//!
//! Keys: printable text types; arrows, Home, End, PageUp and PageDown move the caret (hold shift to
//! extend); `Ctrl+Z` undoes a burst, `Ctrl+Shift+Z` or `Ctrl+Y` redoes; `Ctrl+A` selects all;
//! `Ctrl+S` saves now; `Ctrl+Q` or `Ctrl+C` saves and quits.
//!
//! What is still not here: the GPU backend (M4b-ii — it needs crates this environment cannot
//! fetch), chrome and the security panel (M6), and the remaining gates (M7). What *is* here is the
//! thing the question in `docs/TO-APP.md` asked: you can open the app and start writing, and what
//! you write is still there when you open it again.

use holonomy_eye::Fixed;
use holonomy_gui::panel::Facts;
use holonomy_gui::store::{self, Document};
use holonomy_gui::term::{self, CellShaper, Decoder, Input};
use holonomy_gui::{App, CellSurface, Key, Keymap, Modifiers};
use holonomy_shell::Editor;
use holonomy_shell::Effect;
use holonomy_text::{ResidentSet, Typing};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// How long a pause counts as a pause, for the autosave.
const PAUSE_MILLIS: i32 = 1_500;

type Window = App<store::DocumentSink, store::DocumentSource, CellShaper, CellSurface>;

/// The terminal in raw mode, restored when it is dropped.
struct RawMode {
    saved: libc::termios,
}

impl RawMode {
    fn enable() -> std::io::Result<Self> {
        // SAFETY: `tcgetattr`/`tcsetattr` are called with a valid fd (0 is the tty this process was
        // started on) and a fully initialised `termios` this function owns.
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
        print!("\x1b[0m\x1b[2J\x1b[1;1H");
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

/// Whether a byte is waiting, within `millis`. `false` on a timeout is the autosave's cue.
fn readable(millis: i32) -> bool {
    // SAFETY: a `pollfd` for fd 0, which this process owns for its whole life.
    unsafe {
        let mut descriptor = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        matches!(libc::poll(&mut descriptor, 1, millis), count if count > 0)
    }
}

/// Asks for the passphrase with echo off, and returns what was typed.
///
/// Raw mode already turned echo off for the whole session, so this only has to collect bytes and
/// answer the line and backspace keys. Nothing is echoed, which is the point.
fn prompt(verb: &str, path: &Path) -> std::io::Result<Vec<u8>> {
    let mut stdout = std::io::stdout();
    write!(stdout, "{} {} (passphrase): ", verb, path.display())?;
    stdout.flush()?;
    let mut stdin = std::io::stdin();
    let mut phrase = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if stdin.read(&mut byte)? == 0 {
            break;
        }
        match byte[0] {
            b'\r' | b'\n' => break,
            0x7f | 0x08 => {
                phrase.pop();
            }
            0x03 => {
                writeln!(stdout)?;
                return Err(std::io::Error::other("cancelled at the passphrase"));
            }
            byte => phrase.push(byte),
        }
    }
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(phrase)
}

/// Builds the window over a document, with the file's own geometry.
fn window(
    document: &Document,
    columns: usize,
    rows: usize,
) -> Result<Window, Box<dyn std::error::Error>> {
    let spine = document.spine()?;
    let residency = ResidentSet::new(document.source(), store::BUDGET, store::SLACK)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    let editor = Editor::new(
        Typing::resume(document.sink(), spine, store::CAPACITY),
        residency,
        CellShaper,
        Fixed::units(columns as i64),
        Fixed::units(rows as i64),
    );
    Ok(App::new(
        editor,
        Keymap::new(Fixed::units(rows as i64)),
        CellSurface::new(columns, rows, 1),
    ))
}

/// What the status line says.
fn status(path: &Path, document: &Document, bytes: u64, unsaved: bool, message: &str) -> String {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let state = if unsaved { "unsaved" } else { "saved" };
    let extra = if message.is_empty() {
        String::new()
    } else {
        format!(" · {message}")
    };
    format!(
        " holonomy · {name} · {bytes} bytes · {} segments · {state} · ^S save · ^Q quit{extra} ",
        document.segment_count()
    )
}

/// Saves now: the same intent `Ctrl+S` produces, so the commit path is the keymap's.
fn save(app: &mut Window) -> bool {
    match app.press(Key::Character('s'), Modifiers::control()) {
        Ok(effects) => effects.iter().any(|effect| matches!(effect, Effect::Saved)),
        Err(_) => false,
    }
}

fn run(path: &Path, document: &Document) -> Result<(), Box<dyn std::error::Error>> {
    let (columns, rows) = size();
    let text_rows = rows.saturating_sub(1).max(1);
    let mut app = window(document, columns, text_rows)?;
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut decoder = Decoder::new();
    let mut bytes = [0u8; 256];
    let mut unsaved = false;
    let mut message = String::new();
    let mut panel: Option<Vec<String>> = None;
    let mut verified: Option<u64> = None;
    print!("\x1b[?25l\x1b[2J");
    loop {
        let (now_columns, now_rows) = size();
        let panel_lines = panel.clone().unwrap_or_default();
        let surface_rows = now_rows.saturating_sub(1 + panel_lines.len()).max(1);
        app.surface_mut().resize(now_columns, surface_rows);
        app.resize(
            Fixed::units(now_columns as i64),
            Fixed::units(surface_rows as i64),
        )?;
        app.draw()?;
        let line = status(
            path,
            document,
            app.document_len(),
            unsaved,
            message.as_str(),
        );
        write!(
            stdout,
            "{}",
            term::screen_with(app.surface(), now_columns, &panel_lines, &line)
        )?;
        if let Some((column, row)) = app.surface().caret_cell() {
            write!(stdout, "\x1b[?25h\x1b[{};{}H", row + 1, column + 1)?;
        }
        stdout.flush()?;

        // A pause is a save: nothing is lost if the machine dies between keystrokes.
        if !readable(PAUSE_MILLIS) {
            if unsaved && save(&mut app) {
                unsaved = false;
                message.clear();
            }
            continue;
        }
        let read = stdin.read(&mut bytes)?;
        if read == 0 {
            break;
        }
        let mut inputs = Vec::new();
        for byte in &bytes[..read] {
            if let Some(input) = decoder.feed(*byte) {
                inputs.push(input);
            }
        }
        // A read that ends mid-sequence leaves a lone Escape behind: deliver it.
        inputs.extend(decoder.flush_escape());
        let mut quit = false;
        for input in inputs {
            let Input::Key(key, modifiers) = input else {
                continue;
            };
            if modifiers.control {
                match key {
                    Key::Character('q') | Key::Character('c') => {
                        quit = true;
                        continue;
                    }
                    Key::Character('s') => {
                        unsaved = !save(&mut app);
                        message.clear();
                        continue;
                    }
                    Key::Character('g') => {
                        panel = if panel.is_some() {
                            None
                        } else {
                            // A full authentication is what "verified" may claim, so it is asked
                            // for when the panel opens — never assumed.
                            verified = document
                                .verify()
                                .ok()
                                .map(|report| report.segments_verified);
                            Some(Facts::gather(app.residency(), document, verified).lines())
                        };
                        continue;
                    }
                    _ => {}
                }
            }
            unsaved = true;
            if app.press(key, modifiers).is_err() {
                // A refused intent is not a crashed session: say so and keep the buffer.
                message = "the world-line refused the last edit".to_owned();
            }
        }
        if quit {
            break;
        }
    }
    // Seal whatever is open before the window goes away: a quit is a save.
    if unsaved || app.document_len() > 0 {
        let _ = save(&mut app);
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A window needs a terminal; saying so is better than a stack of escape sequences in a pipe.
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next().map(PathBuf::from) else {
        eprintln!("usage: holonomy <document.wavefunction>");
        eprintln!("  the file is created if it does not exist, opened if it does");
        std::process::exit(2);
    };
    let raw = RawMode::enable()?;
    let existing = path.exists();
    let passphrase = prompt(if existing { "open" } else { "create" }, &path)?;
    let document = if existing {
        Document::open(&path, &passphrase)?
    } else {
        Document::create(&path, &passphrase)?
    };
    let started = document.trailing_bytes();
    // Best effort: the phrase is the caller's, and this is the last moment it is needed. The
    // derived keys live in the document and wipe themselves; the bytes typed do not, yet (OQ-5's
    // neighbour, recorded as OQ-28).
    let mut passphrase = passphrase;
    passphrase.iter_mut().for_each(|byte| *byte = 0);
    drop(passphrase);
    let outcome = run(&path, &document);
    let segments = document.segment_count();
    let witnesses = document.witness_count();
    drop(raw);
    if started != 0 {
        println!(
            "recovered: {started} bytes past the last committed witness were discarded — \
             a previous run died mid-append"
        );
    }
    println!(
        "{}: {} segments, {} witnesses, {}",
        path.display(),
        segments,
        witnesses,
        if outcome.is_ok() {
            "saved"
        } else {
            "the session ended badly"
        }
    );
    outcome
}
