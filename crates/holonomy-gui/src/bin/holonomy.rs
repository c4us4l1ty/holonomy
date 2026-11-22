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
//! extend); `Ctrl+Z` undoes a burst and `Ctrl+Y` redoes one; `Ctrl+A` selects all; `Ctrl+S` saves
//! now; `Ctrl+Q` or `Ctrl+C` saves and quits. A terminal sends `Ctrl+Shift+Z` as the same byte as
//! `Ctrl+Z`, so `Ctrl+Y` is the redo this window can actually offer (D-45).
//!
//! `Ctrl+G` asks the document for the security panel: what is resident, how the memory is locked,
//! what the file itself says. `Esc` dismisses it. What is still not here: the GPU backend
//! (M4b-ii — it needs crates this environment cannot fetch) and the blocks after this one. What
//! *is* here is the thing the question in `docs/TO-APP.md` asked: you can open the app and start
//! writing, and what you write is still there when you open it again.
//!
//! Durability, stated as it is rather than as it sounds: a **seal** appends and witnesses the
//! moment the buffer fills, so text survives a crash once it has sealed. Text still in the open
//! buffer is written by `Ctrl+S`, by the autosave 1.5 s after typing stops, and on quit — so a
//! crash between keystrokes can lose up to one `store::CAPACITY` of the most recent typing, and
//! nothing older (D-64).

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
use zeroize::Zeroizing;

/// How long a pause counts as a pause, for the autosave.
const PAUSE_MILLIS: i32 = 1_500;

/// How long a begun escape sequence waits for its remaining bytes before a lone `ESC` is taken as
/// the Escape key. A terminal may split `ESC [ A` across reads; 25 ms is longer than a local write
/// and shorter than a person notices (D-46).
const ESCAPE_GRACE_MILLIS: i32 = 25;

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
fn prompt(verb: &str, path: &Path) -> std::io::Result<Zeroizing<Vec<u8>>> {
    let mut stdout = std::io::stdout();
    write!(stdout, "{} {} (passphrase): ", verb, path.display())?;
    stdout.flush()?;
    let mut stdin = std::io::stdin();
    let mut phrase = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        if stdin.read(&mut byte)? == 0 {
            // D-63: end of input is not an empty passphrase. `Ctrl+D` at a terminal, or a tty that
            // went away, used to fall through here and hand `create`/`open` an empty phrase — which
            // nothing downstream refuses, so the document was made and reported as saved while
            // being protected by nothing at all. The read returning zero means the user did not
            // finish the sentence the prompt asked for.
            writeln!(stdout)?;
            return Err(std::io::Error::other(
                "the passphrase was not completed (end of input)",
            ));
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
    if phrase.is_empty() {
        // The same rule as end-of-input, for the same reason: a file protected by nothing looks
        // exactly like a file protected by something until the day it matters.
        return Err(std::io::Error::other("the passphrase was empty"));
    }
    Ok(Zeroizing::new(phrase))
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
    let mut editor = Editor::new(
        Typing::resume(document.sink(), spine, store::CAPACITY),
        residency,
        CellShaper,
        Fixed::units(columns as i64),
        Fixed::units(rows as i64),
    );
    // D-53: nothing measured the document, so the scroll limit was the first screen and Page Down
    // could not leave it. `Editor::reflow` is written for exactly this moment — it faults one leaf
    // at a time, so opening a long document costs its leaf count and not its text — and no caller
    // was making it. A document that cannot be measured yet is still openable, so a refusal here is
    // the frame's answer, not the window's: the panel reports what a reader can see.
    let _ = editor.reflow();
    Ok(App::new(
        editor,
        Keymap::new(Fixed::units(rows as i64)),
        CellSurface::new(columns, rows, 1),
    ))
}

/// What the status line says.
///
/// `unsaved` is read from the session (`App::has_pending`), not remembered: a caret move seals the
/// open leaf, so an arrow key *writes* the bytes, and the window used to say "unsaved" about text
/// that already had a witness while saying nothing about bytes a refused save had left behind
/// (D-47).
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

/// How a session ended — which is what the window says on the way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// Nothing was left open, so there was nothing to write.
    NothingToWrite,
    /// Everything open was sealed and committed.
    Saved,
    /// The last save was refused: the bytes are in memory and the file does not have them.
    SaveFailed,
}

/// How the session ended, in the window's own words.
///
/// The first version printed "saved" whenever the run itself returned `Ok` — and the final save's
/// result was thrown away (`let _ = save(...)`), so a session whose last append failed said it had
/// saved (D-49). The save's answer is now part of the sentence.
fn closing(path: &Path, segments: u64, witnesses: u64, outcome: &str) -> String {
    format!(
        "{}: {segments} segments, {witnesses} witnesses, {outcome}",
        path.display()
    )
}

/// What to say about a run that ended: the failure, the failed save, or the plain truth.
fn outcome_words(ended_badly: bool, final_save: Option<bool>) -> &'static str {
    if ended_badly {
        return "the session ended badly";
    }
    match final_save {
        Some(false) => "the last save failed — the bytes after the last witness are in memory only",
        Some(true) | None => "saved",
    }
}

/// Saves now: the same intent `Ctrl+S` produces, so the commit path is the keymap's.
fn save(app: &mut Window) -> bool {
    match app.press(Key::Character('s'), Modifiers::control()) {
        Ok(effects) => effects.iter().any(|effect| matches!(effect, Effect::Saved)),
        Err(_) => false,
    }
}

/// What the screen was last written as: its size, its status line, its caret and its panel.
type Shown = ((usize, usize), String, Option<(usize, usize)>, Vec<String>);

/// Whether the next frame would put the same bytes on the terminal as the last one did.
///
/// This is L-5 in the only place the window itself can break it. `Window::draw` already answers
/// "did anything change" for the surface, and the binary used to ignore it and rewrite the whole
/// screen on every loop iteration — a full-screen escape dump every 1.5 s on an idle window
/// (D-68). The four things compared here are what `screen_with` actually reads, so equal inputs
/// mean identical output and the write can be skipped.
///
/// The size is part of it because a taller terminal repaints no cell: without it a window that
/// grew in rows would keep the old frame until a keystroke arrived.
fn unchanged(
    shown: Option<&Shown>,
    repainted: bool,
    size: (usize, usize),
    line: &str,
    caret: Option<(usize, usize)>,
    panel: &[String],
) -> bool {
    shown.is_some_and(|(old_size, old_line, old_caret, old_panel)| {
        !repainted
            && size == *old_size
            && line == old_line.as_str()
            && caret == *old_caret
            && panel == old_panel.as_slice()
    })
}

fn run(path: &Path, document: &Document) -> Result<Ended, Box<dyn std::error::Error>> {
    let (columns, rows) = size();
    let text_rows = rows.saturating_sub(1).max(1);
    let mut app = window(document, columns, text_rows)?;
    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut decoder = Decoder::new();
    let mut bytes = [0u8; 256];
    let mut message = String::new();
    let mut panel: Option<Vec<String>> = None;
    print!("\x1b[?25l\x1b[2J");
    // What the screen currently shows, so an unchanged view is not written again (D-68).
    // `None` means "nothing has been written yet", which is what makes the first pass paint.
    let mut shown: Option<Shown> = None;
    loop {
        let (now_columns, now_rows) = size();
        // The panel gives way, never the surface: `1 + panel + 1` lines that do not fit make the
        // terminal scroll, and then the caret the renderer places is a row off (D-48).
        let panel_lines = term::panel_fits(&panel.clone().unwrap_or_default(), now_rows);
        let surface_rows = now_rows.saturating_sub(1 + panel_lines.len()).max(1);
        app.surface_mut().resize(now_columns, surface_rows);
        app.resize(
            Fixed::units(now_columns as i64),
            Fixed::units(surface_rows as i64),
        )?;
        // L-5, "idle is idle": `draw` answers whether anything changed, and this loop used to
        // throw that answer away and rewrite the whole screen every iteration — a full-screen
        // escape dump every 1.5 s for as long as the window sat open (D-68). The write is skipped
        // nothing the screen shows has changed. The status line, the caret and the panel are
        // tracked alongside the surface because they change without dirtying the editor: a save
        // clears `has_pending`, a keystroke can move the caret over unchanged text, and Ctrl+G
        // swaps the panel.
        let repainted = app.draw()?;
        let line = status(
            path,
            document,
            app.document_len(),
            app.has_pending(),
            message.as_str(),
        );
        let caret = app.surface().caret_cell();
        // The terminal's own size is part of what is on screen: a taller window with the same
        // width repaints no cell and would otherwise never be filled at its new height.
        let size_now = (now_columns, surface_rows);
        if !unchanged(
            shown.as_ref(),
            repainted,
            size_now,
            &line,
            caret,
            &panel_lines,
        ) {
            write!(
                stdout,
                "{}",
                term::screen_with(app.surface(), now_columns, &panel_lines, &line)
            )?;
            if let Some((column, row)) = caret {
                write!(stdout, "\x1b[?25h\x1b[{};{}H", row + 1, column + 1)?;
            }
            stdout.flush()?;
            shown = Some((size_now, line, caret, panel_lines.clone()));
        }

        // A pause is a save. What is at risk between saves is the **open buffer** — a seal appends
        // to the world-line the moment the buffer fills, so nothing before that is ever lost, and
        // the exposure is bounded by `store::CAPACITY` rather than by how long someone types. The
        // first version of this comment said "nothing is lost if the machine dies between
        // keystrokes", which is true of every sealed byte and false of the ones just typed (D-64).
        if !readable(PAUSE_MILLIS) {
            if app.has_pending() && save(&mut app) {
                message.clear();
            }
            continue;
        }
        let read = stdin.read(&mut bytes)?;
        if read == 0 {
            break;
        }
        let mut inputs = decoder.feed_all(&bytes[..read]);
        // A read can end mid-sequence: `ESC` may be the Escape key or the start of an arrow the
        // terminal is still delivering. Only a stream that stays quiet for the grace period settles
        // it — flushing eagerly typed `[A` into the document (D-46).
        if decoder.pending() && !readable(ESCAPE_GRACE_MILLIS) {
            inputs.extend(decoder.flush_escape());
        }
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
                        if !save(&mut app) {
                            message = "the save was refused".to_owned();
                        } else {
                            message.clear();
                        }
                        continue;
                    }
                    Key::Character('g') => {
                        panel = if panel.is_some() {
                            None
                        } else {
                            // A full authentication is what "verified" may claim, so it is asked
                            // for when the panel opens — never assumed.
                            let verified = document
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
            // D-61: `Key::Escape` is documented as "dismisses, never edits", and the keymap
            // correctly refuses to turn it into an edit — but nothing was doing the dismissing, so
            // the key was inert and the only way to close the panel was the chord that opened it.
            // The panel is the window's, not the editor's, so it is dismissed here.
            if key == Key::Escape && panel.take().is_some() {
                continue;
            }
            if app.press(key, modifiers).is_err() {
                // A refused intent is not a crashed session: say so and keep the buffer.
                message = "the world-line refused the last edit".to_owned();
            }
        }
        if quit {
            break;
        }
    }
    // Seal whatever is open before the window goes away: a quit is a save, and whether it worked is
    // part of what the window says on the way out (D-49).
    Ok(if !app.has_pending() {
        Ended::NothingToWrite
    } else if save(&mut app) {
        Ended::Saved
    } else {
        Ended::SaveFailed
    })
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
    let passphrase: Zeroizing<Vec<u8>> = prompt(if existing { "open" } else { "create" }, &path)?;
    let document = if existing {
        Document::open(&path, &passphrase)?
    } else {
        Document::create(&path, &passphrase)?
    };
    let started = document.trailing_bytes();
    // Best effort: the phrase is the caller's, and this is the last moment it is needed. The
    // derived keys live in the document and wipe themselves; the bytes typed do not, yet (OQ-5's
    // neighbour, recorded as OQ-28).
    // OQ-28: Zeroizing<Vec<u8>> wipes on Drop; manual zero is redundant but kept as defence.
    let mut passphrase = passphrase;
    passphrase.iter_mut().for_each(|byte| *byte = 0);
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
        "{}",
        closing(
            &path,
            segments,
            witnesses,
            outcome_words(
                outcome.is_err(),
                match outcome {
                    Ok(Ended::SaveFailed) => Some(false),
                    Ok(Ended::Saved) => Some(true),
                    Ok(Ended::NothingToWrite) | Err(_) => None,
                }
            )
        )
    );
    if matches!(outcome, Ok(Ended::SaveFailed)) {
        // A window that could not write must not exit as though it had.
        std::process::exit(1);
    }
    outcome.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_save_is_never_called_saved() {
        // D-49: the window used to print "saved" whenever the run returned `Ok`, and the final
        // save's own answer was dropped on the floor.
        assert_eq!(outcome_words(false, Some(true)), "saved");
        assert_eq!(outcome_words(false, None), "saved");
        assert!(
            outcome_words(false, Some(false)).contains("failed"),
            "a refused save must say so"
        );
        assert!(
            outcome_words(true, Some(false)).contains("ended badly"),
            "a dead session outranks a failed save"
        );
        let line = closing(Path::new("/tmp/notes.wavefunction"), 3, 4, "saved");
        assert!(line.contains("3 segments"), "{line}");
        assert!(line.contains("4 witnesses"), "{line}");
    }

    #[test]
    fn an_idle_window_writes_nothing() {
        // D-68: L-5 is "zero repaint while the view is unchanged", and the loop broke it by
        // rewriting the full screen on every iteration regardless of what `draw` answered.
        let size = (80, 24);
        let line = "holonomy · notes · 0 bytes".to_owned();
        let caret = Some((0, 0));
        let panel: Vec<String> = Vec::new();
        let shown: Shown = (size, line.clone(), caret, panel.clone());

        // Nothing changed: skip the write. This is the case that used to burn a full screen
        // of escapes every 1.5 s for as long as the window sat open.
        assert!(
            unchanged(Some(&shown), false, size, &line, caret, &panel),
            "an idle window must not rewrite the screen"
        );

        // The first pass has written nothing yet, so it must paint.
        assert!(
            !unchanged(None, false, size, &line, caret, &panel),
            "the first frame must be written"
        );
        // A redraw is a change even when every other input matches.
        assert!(
            !unchanged(Some(&shown), true, size, &line, caret, &panel),
            "a repaint must reach the terminal"
        );
        // A save clears `has_pending`, which changes the status line and nothing else.
        assert!(
            !unchanged(Some(&shown), false, size, "saved", caret, &panel),
            "a changed status line must reach the terminal"
        );
        // A keystroke can move the caret over text that did not change.
        assert!(
            !unchanged(Some(&shown), false, size, &line, Some((7, 2)), &panel),
            "a moved caret must reach the terminal"
        );
        // Ctrl+G swaps the panel without dirtying the document.
        let opened = vec!["residency: 16 KiB".to_owned()];
        assert!(
            !unchanged(Some(&shown), false, size, &line, caret, &opened),
            "an opened panel must reach the terminal"
        );
        // A taller terminal repaints no cell but needs the rows filled.
        assert!(
            !unchanged(Some(&shown), false, (80, 30), &line, caret, &panel),
            "a resized terminal must be repainted at its new height"
        );
    }
}
