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
use holonomy_gui::{App, CellSurface, Key, Keymap, Modifiers, SealWipeBridge};
use holonomy_seal::{
    probe_capability, CanarySet, LockCapability, PassphraseInput, ReaperGuard, SecretArenas,
    TripReason, Tripwire, MAX_PASSPHRASE_INPUT_BYTES,
};
use holonomy_shell::{Editor, Effect, Intent};
use holonomy_text::{ResidentSet, Typing};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
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

/// Turns terminal echo off for a one-line prompt when `stdin` is a tty, without clearing the
/// screen on drop (used by `verify` and `compact`).
struct EchoGuard {
    saved: Option<libc::termios>,
}

impl EchoGuard {
    fn acquire() -> std::io::Result<Self> {
        // SAFETY: `isatty`/`tcgetattr`/`tcsetattr` operate on `STDIN_FILENO` with a zeroed local
        // `termios`.
        unsafe {
            if libc::isatty(libc::STDIN_FILENO) == 0 {
                return Ok(Self { saved: None });
            }
            let mut current: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut current) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let saved = current;
            current.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
            current.c_iflag &= !libc::ICRNL;
            current.c_cc[libc::VMIN] = 1;
            current.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &current) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self { saved: Some(saved) })
        }
    }
}

impl Drop for EchoGuard {
    fn drop(&mut self) {
        if let Some(saved) = &self.saved {
            // SAFETY: restoring the exact `termios` captured in `acquire`.
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, saved);
            }
        }
    }
}

/// Removes one UTF-8 scalar value from the tail of a raw input buffer, so Backspace on a
/// multi-byte character never leaves orphaned continuation bytes behind.
fn pop_utf8_scalar(buffer: &mut Vec<u8>) {
    while let Some(byte) = buffer.pop() {
        if (byte & 0b1100_0000) != 0b1000_0000 {
            break;
        }
    }
}

/// Reads and NFC-normalizes a passphrase from `reader` into a [`PassphraseInput`] without heap
/// reallocation (`AUDIT OQ-28`).
fn read_passphrase_from(
    reader: &mut impl Read,
    echo_newline: Option<&mut dyn Write>,
) -> std::io::Result<PassphraseInput> {
    let mut phrase = Zeroizing::new(Vec::with_capacity(MAX_PASSPHRASE_INPUT_BYTES));
    let mut byte = [0u8; 1];
    loop {
        if reader.read(&mut byte)? == 0 {
            // D-63: end of input is not an empty passphrase.
            if let Some(out) = echo_newline {
                writeln!(out)?;
            }
            return Err(std::io::Error::other(
                "the passphrase was not completed (end of input)",
            ));
        }
        match byte[0] {
            b'\r' | b'\n' => break,
            0x7f | 0x08 => pop_utf8_scalar(&mut phrase),
            0x03 => {
                if let Some(out) = echo_newline {
                    writeln!(out)?;
                }
                return Err(std::io::Error::other("cancelled at the passphrase"));
            }
            next => {
                if phrase.len() >= MAX_PASSPHRASE_INPUT_BYTES {
                    if let Some(out) = echo_newline {
                        writeln!(out)?;
                    }
                    return Err(std::io::Error::other(
                        "the passphrase exceeds the input byte limit",
                    ));
                }
                phrase.push(next);
            }
        }
    }
    if let Some(out) = echo_newline {
        writeln!(out)?;
        out.flush()?;
    }
    let raw = std::str::from_utf8(&phrase)
        .map_err(|_| std::io::Error::other("the passphrase is not valid UTF-8"))?;
    PassphraseInput::new(raw).map_err(|error| std::io::Error::other(error.to_string()))
}

/// Asks for the passphrase with echo off, and returns an NFC-normalized [`PassphraseInput`] whose
/// raw and normalized buffers are both pre-allocated and zeroized on drop (`AUDIT OQ-28`).
fn prompt(verb: &str, path: &Path) -> std::io::Result<PassphraseInput> {
    let mut stdout = std::io::stdout();
    let is_tty = unsafe { libc::isatty(libc::STDIN_FILENO) != 0 };
    if is_tty {
        write!(stdout, "{} {} (passphrase): ", verb, path.display())?;
        stdout.flush()?;
    }
    let mut stdin = std::io::stdin();
    if is_tty {
        read_passphrase_from(&mut stdin, Some(&mut stdout))
    } else {
        read_passphrase_from(&mut stdin, None)
    }
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
    /// Emergency reaper wipe fired (`Ctrl+L` or deadman timeout): resident memory was zeroized.
    Wiped,
}

#[derive(Debug, Clone)]
enum SearchPrompt {
    Find { needle: String, next_idx: usize },
    Replace { needle: String, replacement: String },
}

impl SearchPrompt {
    fn render(&self) -> String {
        match self {
            Self::Find { needle, .. } => {
                format!("Find (Enter next · Tab replace · Esc close): {needle}")
            }
            Self::Replace {
                needle,
                replacement,
            } => format!("Replace all '{needle}' with (Enter run · Esc close): {replacement}"),
        }
    }
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

// Only the UI thread owns the editor and document. Register fresh pointers after it has stopped
// editing, then let the worker wipe; do not touch registered allocations until it has joined.
fn wipe_quiescent(
    app: &mut Window,
    document: &Document,
    arenas: &Arc<SecretArenas>,
    reaper: ReaperGuard,
) -> Ended {
    // SAFETY: the UI thread has stopped using the document until the worker has joined.
    let _ = unsafe { document.arm_reaper(arenas) };
    // SAFETY: the UI thread has stopped editing and will not touch any registered allocation
    // again until the reaper has completed.
    unsafe {
        app.editor()
            .set_wipe_registry(Arc::new(SealWipeBridge(Arc::clone(arenas))));
    }
    // SAFETY: this thread holds the only app/document handles used by the editor, and it does not
    // access the registered buffers or canaries again until the worker has joined.
    unsafe { reaper.acknowledge_quiescence() };
    let _ = reaper.wait_outcome();
    app.editor().wipe_all_in_place();
    Ended::Wiped
}

fn run(
    path: &Path,
    document: &Document,
    deadman_timeout: Option<Duration>,
) -> Result<Ended, Box<dyn std::error::Error>> {
    let arenas = SecretArenas::new();
    let mut canary_set = CanarySet::new(8, 256);
    let _ = canary_set.arm_all(&arenas);
    let canaries = Arc::new(canary_set);
    let tripwire = Tripwire::new(deadman_timeout);
    tripwire.beat();
    let reaper = ReaperGuard::spawn_cooperative(
        Arc::clone(&arenas),
        Arc::clone(&canaries),
        Arc::clone(&tripwire),
        Duration::from_millis(50),
    );

    let (columns, rows) = size();
    let text_rows = rows.saturating_sub(1).max(1);
    let mut app = window(document, columns, text_rows)?;

    let mut stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut decoder = Decoder::new();
    let mut bytes = [0u8; 256];
    let mut message = String::new();
    let mut search: Option<SearchPrompt> = None;
    let mut panel: Option<(LockCapability, Option<u64>)> = None;
    print!("\x1b[?25l\x1b[2J");
    // What the screen currently shows, so an unchanged view is not written again (D-68).
    // `None` means "nothing has been written yet", which is what makes the first pass paint.
    let mut shown: Option<Shown> = None;
    loop {
        if reaper.has_tripped() || tripwire.evaluate().is_some() {
            return Ok(wipe_quiescent(&mut app, document, &arenas, reaper));
        }
        let (now_columns, now_rows) = size();
        // The panel gives way, never the surface: `1 + panel + 1` lines that do not fit make the
        // terminal scroll, and then the caret the renderer places is a row off (D-48).
        let panel_room = if panel.is_some() {
            now_rows.saturating_sub(2).min(5)
        } else {
            0
        };
        let surface_rows = now_rows.saturating_sub(1 + panel_room).max(1);
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
        let panel_lines = match panel {
            Some((capability, verified)) => {
                let facts = Facts::gather_with(app.residency(), document, capability, verified)
                    .with_cooperative_reaper(&arenas, &canaries, reaper.has_tripped());
                term::panel_fits(&facts.lines(), now_rows)
            }
            None => Vec::new(),
        };
        let ribbon_or_msg = if let Some(prompt) = &search {
            prompt.render()
        } else if !message.is_empty() {
            message.clone()
        } else if app.selection().is_some() {
            app.toolbar_summary()
        } else {
            String::new()
        };
        let line = status(
            path,
            document,
            app.document_len(),
            app.has_pending(),
            ribbon_or_msg.as_str(),
        );
        let caret = app
            .surface()
            .caret_cell()
            .filter(|&(_, row)| row < surface_rows);
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
            } else {
                write!(stdout, "\x1b[?25l")?;
            }
            stdout.flush()?;
            shown = Some((size_now, line, caret, panel_lines.clone()));
        }

        // A pause is a save. What is at risk between saves is the **open buffer** — a seal appends
        // to the world-line the moment the buffer fills, so nothing before that is ever lost, and
        // the exposure is bounded by `store::CAPACITY` rather than by how long someone types. The
        // first version of this comment said "nothing is lost if the machine dies between
        // keystrokes", which is true of every sealed byte and false of the ones just typed (D-64).
        let poll_ms = deadman_timeout.map_or(PAUSE_MILLIS, |d| {
            i32::try_from(d.as_millis() / 2)
                .unwrap_or(PAUSE_MILLIS)
                .clamp(10, PAUSE_MILLIS)
        });
        if !readable(poll_ms) {
            if reaper.has_tripped() || tripwire.evaluate().is_some() {
                return Ok(wipe_quiescent(&mut app, document, &arenas, reaper));
            }
            if app.has_pending() && save(&mut app) {
                message.clear();
            }
            continue;
        }
        let read = stdin.read(&mut bytes)?;
        if read == 0 {
            break;
        }
        if reaper.has_tripped() || tripwire.evaluate().is_some() {
            return Ok(wipe_quiescent(&mut app, document, &arenas, reaper));
        }
        tripwire.beat();
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
            if modifiers.control && matches!(key, Key::Character('l' | 'L')) {
                tripwire.fire(TripReason::Manual);
                return Ok(wipe_quiescent(&mut app, document, &arenas, reaper));
            }
            if let Some(prompt_state) = search.as_mut() {
                if modifiers.control && matches!(key, Key::Character('q') | Key::Character('c')) {
                    quit = true;
                    continue;
                }
                match key {
                    Key::Escape => {
                        search = None;
                    }
                    Key::Backspace => match prompt_state {
                        SearchPrompt::Find {
                            needle, next_idx, ..
                        } => {
                            needle.pop();
                            *next_idx = 0;
                        }
                        SearchPrompt::Replace { replacement, .. } => {
                            replacement.pop();
                        }
                    },
                    Key::Character('\t') if !modifiers.control => {
                        if let SearchPrompt::Find { needle, .. } = prompt_state {
                            search = Some(SearchPrompt::Replace {
                                needle: needle.clone(),
                                replacement: String::new(),
                            });
                        }
                    }
                    Key::Enter => match prompt_state.clone() {
                        SearchPrompt::Find { needle, next_idx } => {
                            if needle.is_empty() {
                                search = None;
                            } else {
                                match app.editor().find_all(&needle) {
                                    Ok(hits) if hits.is_empty() => {
                                        message = format!("0 matches for '{needle}'");
                                        search = None;
                                    }
                                    Ok(hits) => {
                                        let idx = next_idx % hits.len();
                                        let hit = hits[idx];
                                        let _ = app.editor().select_range(hit.start, hit.end);
                                        let _ = app.editor().apply(Intent::RevealCaret);
                                        message = format!("match {}/{}", idx + 1, hits.len());
                                        search = Some(SearchPrompt::Find {
                                            needle,
                                            next_idx: idx + 1,
                                        });
                                    }
                                    Err(_) => {
                                        message = "streaming search refused".to_owned();
                                        search = None;
                                    }
                                }
                            }
                        }
                        SearchPrompt::Replace {
                            needle,
                            replacement,
                        } => {
                            search = None;
                            if !needle.is_empty() {
                                match app.editor().replace_all(&needle, &replacement) {
                                    Ok(count) => {
                                        let _ = app.editor().apply(Intent::RevealCaret);
                                        message = format!("replaced {count} matches");
                                    }
                                    Err(_) => {
                                        message = "replace-all refused".to_owned();
                                    }
                                }
                            }
                        }
                    },
                    Key::Character(ch) if !modifiers.control && !modifiers.alt => {
                        match prompt_state {
                            SearchPrompt::Find { needle, next_idx } => {
                                needle.push(ch);
                                *next_idx = 0;
                            }
                            SearchPrompt::Replace { replacement, .. } => {
                                replacement.push(ch);
                            }
                        }
                    }
                    _ => {}
                }
                continue;
            }
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
                    Key::Character('f') => {
                        search = Some(SearchPrompt::Find {
                            needle: String::new(),
                            next_idx: 0,
                        });
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
                            Some((probe_capability(), verified))
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

fn usage_text() -> &'static str {
    concat!(
        "usage:\n",
        "  holonomy [--deadman <ms>] <document.wavefunction>\n",
        "  holonomy verify <document.wavefunction>\n",
        "  holonomy compact <source.wavefunction> <destination.wavefunction>\n",
        "  the file is created if it does not exist, opened if it does"
    )
}

fn parse_editor_args(args: &[String]) -> Option<(PathBuf, Option<Duration>)> {
    match args {
        [single] if !single.starts_with('-') => Some((PathBuf::from(single), None)),
        [flag, ms, target] if flag == "--deadman" && !target.starts_with('-') => {
            let millis = ms.parse::<u64>().ok().filter(|&v| v > 0)?;
            Some((PathBuf::from(target), Some(Duration::from_millis(millis))))
        }
        _ => None,
    }
}

fn run_verify(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let _echo = EchoGuard::acquire()?;
    let passphrase = prompt("verify", path)?;
    let document = Document::open_input(path, &passphrase)?;
    drop(passphrase);
    let report = document.verify()?;
    println!(
        "{}: verified ({} segments, {} witnesses, {} plaintext bytes)",
        path.display(),
        report.segments_verified,
        report.witnesses_verified,
        report.plaintext_bytes_verified,
    );
    Ok(())
}

fn run_compact(source: &Path, destination: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let _echo = EchoGuard::acquire()?;
    let open_passphrase = prompt("open", source)?;
    let document = Document::open_input(source, &open_passphrase)?;
    drop(open_passphrase);
    let confirm_passphrase = prompt("confirm compaction into", destination)?;
    let compacted = document.compact_to_input(destination, &confirm_passphrase)?;
    drop(confirm_passphrase);
    let report = compacted.verify()?;
    println!(
        "{}: compacted from {} ({} segments, {} witnesses, {} plaintext bytes)",
        destination.display(),
        source.display(),
        report.segments_verified,
        report.witnesses_verified,
        report.plaintext_bytes_verified,
    );
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // A window needs a terminal; saying so is better than a stack of escape sequences in a pipe.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => {
            eprintln!("{}", usage_text());
            std::process::exit(2);
        }
        [flag] if flag == "--help" || flag == "-h" => {
            println!("{}", usage_text());
            Ok(())
        }
        [subcommand, target] if subcommand == "verify" => {
            let path = Path::new(target);
            run_verify(path)
        }
        [subcommand, source, destination] if subcommand == "compact" => {
            let src = Path::new(source);
            let dst = Path::new(destination);
            run_compact(src, dst)
        }
        [subcommand, ..] if subcommand == "verify" || subcommand == "compact" => {
            eprintln!("{}", usage_text());
            std::process::exit(2);
        }
        other => {
            let Some((path, deadman)) = parse_editor_args(other) else {
                eprintln!("{}", usage_text());
                std::process::exit(2);
            };
            let raw = RawMode::enable()?;
            let existing = path.exists();
            let passphrase = prompt(if existing { "open" } else { "create" }, &path)?;
            let document = if existing {
                Document::open_input(&path, &passphrase)?
            } else {
                Document::create_input(&path, &passphrase)?
            };
            let started = document.trailing_bytes();
            drop(passphrase);
            let outcome = run(&path, &document, deadman);
            let segments = document.segment_count();
            let witnesses = document.witness_count();
            drop(raw);
            if started != 0 {
                println!(
                    "recovered: {started} bytes past the last committed witness were discarded — \
                     a previous run died mid-append"
                );
            }
            let summary = match outcome {
                Ok(Ended::Wiped) => "emergency wipe attempted — full memory coverage not verified",
                Ok(Ended::SaveFailed) => outcome_words(false, Some(false)),
                Ok(Ended::Saved) => outcome_words(false, Some(true)),
                Ok(Ended::NothingToWrite) => outcome_words(false, None),
                Err(_) => outcome_words(true, None),
            };
            println!("{}", closing(&path, segments, witnesses, summary));
            if matches!(outcome, Ok(Ended::SaveFailed)) {
                // A window that could not write must not exit as though it had.
                std::process::exit(1);
            }
            outcome.map(|_| ())
        }
    }
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

    #[test]
    fn passphrase_prompt_normalizes_nfc_and_backspaces_whole_scalars() {
        let mut nfd = "cafe\u{0301}中\x7f!\n".as_bytes();
        let mut nfc = "caf\u{00E9}!\n".as_bytes();
        let parsed_nfd = read_passphrase_from(&mut nfd, None).expect("reads NFD");
        let parsed_nfc = read_passphrase_from(&mut nfc, None).expect("reads NFC");
        let left = parsed_nfd.with_bytes(<[u8]>::to_vec);
        let right = parsed_nfc.with_bytes(<[u8]>::to_vec);
        assert_eq!(left, right);
        assert_eq!(left, "caf\u{00E9}!".as_bytes());

        let mut eof = b"unterminated".as_slice();
        assert!(read_passphrase_from(&mut eof, None).is_err());
        let mut empty = b"\n".as_slice();
        assert!(read_passphrase_from(&mut empty, None).is_err());
    }
}
