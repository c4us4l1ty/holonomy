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
//! extend, `Ctrl/Alt+Left/Right` steps by word); `Backspace`/`Delete` (`Ctrl/Alt` by word) removes;
//! `Ctrl+Z` undoes a burst and `Ctrl+Y` redoes one; `Ctrl+A` selects all; `Ctrl+S` saves now;
//! `Ctrl+R` browses and restores witness history; `Ctrl+Q` or `Ctrl+C` saves and quits. A terminal
//! sends `Ctrl+Shift+Z` as the same byte as `Ctrl+Z`, so `Ctrl+Y` is the redo this window can
//! actually offer (D-45).
//!
//! `Ctrl+G` asks the document for the security panel: what is resident, how the memory is locked,
//! what the file itself says, and window capture/clipboard protections. `Esc` dismisses overlays.
//! The pure-FFI pixel desktop (`holonomy gui <doc>` / `holonomy-gui`) lives alongside this TUI.
//!
//! Durability, stated as it is rather than as it sounds: a **seal** appends and witnesses the
//! moment the buffer fills, so text survives a crash once it has sealed. Text still in the open
//! buffer is written by `Ctrl+S`, by the autosave 1.5 s after typing stops, and on quit — so a
//! crash between keystrokes can lose up to one `store::CAPACITY` of the most recent typing, and
//! nothing older (D-64).

use holonomy_eye::Fixed;
use holonomy_gui::canvas::{run_desktop, DesktopEnd, GuiWipeUnconfirmed};
use holonomy_gui::panel::{self, Facts};
use holonomy_gui::store::{self, Document};
use holonomy_gui::term::{self, CellShaper, Decoder, Input};
use holonomy_gui::transfer::{self, TransferFormat};
use holonomy_gui::{App, CellSurface, Key, Keymap, Modifiers, SealWipeBridge};
use holonomy_seal::v5::{V5PadSource, V5Passphrase, V5_ZONE_B_LBA_START};
use holonomy_seal::{
    probe_capability, CanarySet, LockCapability, ParamsClass, PassphraseInput, ReaperGuard,
    ReaperOutcome, SchedClass, SecretArenas, TripReason, Tripwire, MAX_PASSPHRASE_INPUT_BYTES,
};
use holonomy_shell::{Editor, Effect, Intent};
use holonomy_text::{ResidentSet, Typing};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

/// How long a pause counts as a pause, for the autosave.
const PAUSE_MILLIS: i32 = 1_500;

/// How long a begun escape sequence waits for its remaining bytes before a lone `ESC` is taken as
/// the Escape key. A terminal may split `ESC [ A` across reads; 25 ms is longer than a local write
/// and shorter than a person notices (D-46).
const ESCAPE_GRACE_MILLIS: i32 = 25;

type Window = App<store::DocumentSink, store::DocumentSource, CellShaper, CellSurface>;

#[cfg(unix)]
mod sys_console {
    use std::io::Write;

    /// The terminal in raw mode, restored when it is dropped.
    pub(super) struct RawMode {
        saved: libc::termios,
    }

    impl RawMode {
        pub(super) fn enable() -> std::io::Result<Self> {
            // SAFETY: `tcgetattr`/`tcsetattr` operate on stdin (fd 0) with a local `termios`.
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

    /// The terminal's size in cells, or 80×24 when it cannot be asked.
    pub(super) fn size() -> (usize, usize) {
        // SAFETY: `ioctl` with `TIOCGWINSZ` writes a local `winsize`.
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

    /// Whether a byte is waiting on stdin within `millis`.
    pub(super) fn readable(millis: i32) -> bool {
        // SAFETY: `pollfd` points to valid stack storage over fd 0.
        unsafe {
            let mut descriptor = libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            };
            matches!(libc::poll(&mut descriptor, 1, millis), count if count > 0)
        }
    }

    /// Whether stdin refers to an interactive TTY.
    pub(super) fn stdin_is_tty() -> bool {
        // SAFETY: `isatty` checks stdin fd without dereferencing pointers.
        unsafe { libc::isatty(libc::STDIN_FILENO) != 0 }
    }

    /// Turns terminal echo off for a one-line prompt when `stdin` is a tty.
    pub(super) struct EchoGuard {
        saved: Option<libc::termios>,
    }

    impl EchoGuard {
        pub(super) fn acquire() -> std::io::Result<Self> {
            // SAFETY: `isatty`/`tcgetattr`/`tcsetattr` operate on stdin with a local `termios`.
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
}

#[cfg(windows)]
mod sys_console {
    use std::ffi::c_void;
    use std::io::Write;

    type Handle = *mut c_void;
    const STD_INPUT_HANDLE: u32 = (-10i32) as u32;
    const STD_OUTPUT_HANDLE: u32 = (-11i32) as u32;
    const INVALID_HANDLE_VALUE: Handle = (-1isize) as Handle;
    const ENABLE_PROCESSED_INPUT: u32 = 0x0001;
    const ENABLE_LINE_INPUT: u32 = 0x0002;
    const ENABLE_ECHO_INPUT: u32 = 0x0004;
    const ENABLE_VIRTUAL_TERMINAL_INPUT: u32 = 0x0200;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    const FILE_TYPE_CHAR: u32 = 0x0002;
    const WAIT_OBJECT_0: u32 = 0;

    #[repr(C)]
    #[derive(Default)]
    struct Coord {
        x: i16,
        y: i16,
    }

    #[repr(C)]
    #[derive(Default)]
    struct SmallRect {
        left: i16,
        top: i16,
        right: i16,
        bottom: i16,
    }

    #[repr(C)]
    #[derive(Default)]
    struct ConsoleScreenBufferInfo {
        dw_size: Coord,
        dw_cursor_position: Coord,
        w_attributes: u16,
        sr_window: SmallRect,
        dw_maximum_window_size: Coord,
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn GetStdHandle(n_std_handle: u32) -> Handle;
        fn GetConsoleMode(h_console_handle: Handle, lp_mode: *mut u32) -> i32;
        fn SetConsoleMode(h_console_handle: Handle, dw_mode: u32) -> i32;
        fn GetConsoleScreenBufferInfo(
            h_console_output: Handle,
            lp_console_screen_buffer_info: *mut ConsoleScreenBufferInfo,
        ) -> i32;
        fn WaitForSingleObject(h_handle: Handle, dw_milliseconds: u32) -> u32;
        fn GetFileType(h_file: Handle) -> u32;
    }

    pub(super) struct RawMode {
        in_handle: Handle,
        saved_in: u32,
        out_handle: Handle,
        saved_out: Option<u32>,
    }

    impl RawMode {
        pub(super) fn enable() -> std::io::Result<Self> {
            // SAFETY: queries standard console handles and saves/updates Windows VT console modes.
            unsafe {
                let in_handle = GetStdHandle(STD_INPUT_HANDLE);
                if in_handle.is_null() || in_handle == INVALID_HANDLE_VALUE {
                    return Err(std::io::Error::last_os_error());
                }
                let mut saved_in = 0u32;
                if GetConsoleMode(in_handle, &mut saved_in) == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let raw_in = (saved_in
                    & !(ENABLE_LINE_INPUT | ENABLE_ECHO_INPUT | ENABLE_PROCESSED_INPUT))
                    | ENABLE_VIRTUAL_TERMINAL_INPUT;
                if SetConsoleMode(in_handle, raw_in) == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let out_handle = GetStdHandle(STD_OUTPUT_HANDLE);
                let mut saved_out = None;
                let mut cur_out = 0u32;
                if !out_handle.is_null()
                    && out_handle != INVALID_HANDLE_VALUE
                    && GetConsoleMode(out_handle, &mut cur_out) != 0
                {
                    saved_out = Some(cur_out);
                    let _ =
                        SetConsoleMode(out_handle, cur_out | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
                }
                Ok(Self {
                    in_handle,
                    saved_in,
                    out_handle,
                    saved_out,
                })
            }
        }
    }

    impl Drop for RawMode {
        fn drop(&mut self) {
            // SAFETY: restores captured console input/output modes.
            unsafe {
                let _ = SetConsoleMode(self.in_handle, self.saved_in);
                if let Some(m) = self.saved_out {
                    let _ = SetConsoleMode(self.out_handle, m);
                }
            }
            print!("\x1b[0m\x1b[2J\x1b[1;1H");
            let _ = std::io::stdout().flush();
        }
    }

    pub(super) fn size() -> (usize, usize) {
        // SAFETY: reads console window rectangle from `STD_OUTPUT_HANDLE`.
        unsafe {
            let out = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut info = ConsoleScreenBufferInfo::default();
            if !out.is_null()
                && out != INVALID_HANDLE_VALUE
                && GetConsoleScreenBufferInfo(out, &mut info) != 0
            {
                let cols =
                    usize::try_from(info.sr_window.right - info.sr_window.left + 1).unwrap_or(80);
                let rows =
                    usize::try_from(info.sr_window.bottom - info.sr_window.top + 1).unwrap_or(24);
                if cols >= 20 && rows >= 4 {
                    return (cols, rows);
                }
            }
        }
        (80, 24)
    }

    pub(super) fn readable(millis: i32) -> bool {
        // SAFETY: waits on `STD_INPUT_HANDLE` up to `millis`.
        unsafe {
            let h = GetStdHandle(STD_INPUT_HANDLE);
            if h.is_null() || h == INVALID_HANDLE_VALUE {
                return false;
            }
            WaitForSingleObject(h, millis.max(0) as u32) == WAIT_OBJECT_0
        }
    }

    pub(super) fn stdin_is_tty() -> bool {
        // SAFETY: checks `GetFileType` + `GetConsoleMode` on `STD_INPUT_HANDLE`.
        unsafe {
            let h = GetStdHandle(STD_INPUT_HANDLE);
            if h.is_null() || h == INVALID_HANDLE_VALUE || GetFileType(h) != FILE_TYPE_CHAR {
                return false;
            }
            let mut mode = 0u32;
            GetConsoleMode(h, &mut mode) != 0
        }
    }

    pub(super) struct EchoGuard {
        saved: Option<(Handle, u32)>,
    }

    impl EchoGuard {
        pub(super) fn acquire() -> std::io::Result<Self> {
            if !stdin_is_tty() {
                return Ok(Self { saved: None });
            }
            // SAFETY: stdin is a console handle; disables echo + line input for prompt.
            unsafe {
                let h = GetStdHandle(STD_INPUT_HANDLE);
                let mut cur = 0u32;
                if GetConsoleMode(h, &mut cur) == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                let _ = SetConsoleMode(h, cur & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT));
                Ok(Self {
                    saved: Some((h, cur)),
                })
            }
        }
    }

    impl Drop for EchoGuard {
        fn drop(&mut self) {
            if let Some((h, cur)) = self.saved {
                // SAFETY: restores original console mode on drop.
                unsafe {
                    let _ = SetConsoleMode(h, cur);
                }
            }
        }
    }
}

use sys_console::{readable, size, stdin_is_tty, EchoGuard, RawMode};

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
    let is_tty = stdin_is_tty();
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
    let spine_len = spine.len();
    let mut editor = Editor::new(
        Typing::resume(document.sink(), spine, store::CAPACITY),
        residency,
        CellShaper,
        Fixed::units(columns as i64),
        Fixed::units(rows as i64),
    );
    // D-53 + L-1/L-2: measure normal documents upfront so Page Down knows total height; on huge
    // multi-million-word documents (> 2 MiB), let `frame()` bootstrap only the visible viewport so
    // cold-open latency and resident memory stay bounded.
    if spine_len <= 2 * 1024 * 1024 {
        let _ = editor.reflow();
    }
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
    /// Emergency reaper wipe fired (`Ctrl+L` or deadman timeout) and a wipe was **confirmed**:
    /// the reaper ran, reported that it scrubbed at least one registered arena, and read back no
    /// surviving canary byte. This is a receipt, not a proof of total coverage — see
    /// [`WipeReceipt`] and `docs/WIPE_SUPERVISOR_REVIEW.md` for what it does not cover.
    Wiped(WipeReceipt),
    /// Emergency reaper wipe fired but could **not** be confirmed: the reaper join produced no
    /// outcome, it scrubbed no arenas, or a canary survived. The window must say this rather than
    /// claim a wipe (D-89) — an unconfirmed wipe is an *unmet protection*, not a success.
    /// The count is what the editor scrubbed in place anyway (`D-93`), so the closing line can
    /// report the scrub a reader cannot otherwise see happened.
    WipeUnconfirmed(WipeUnconfirmed, usize),
}

/// The measured facts of a wipe that was confirmed, kept so the window can show them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WipeReceipt {
    /// Bytes of registered plaintext and key material volatile-zeroed.
    bytes_wiped: usize,
    /// Registered arenas the reaper actually scrubbed. Coverage of *all* resident plaintext is a
    /// larger claim than this number, which is why the sentence it renders says "registered
    /// arenas only".
    arenas_wiped: usize,
    /// Canary buffers read back after the wipe.
    canaries: usize,
    /// Monotonic microseconds from the tripwire's instant to verified zero canaries.
    trip_to_verified_us: u64,
    /// Whether the reaper got real-time scheduling on this host, so the number is read honestly.
    real_time: bool,
    /// Bytes the *editor* volatile-zeroed in place after the reaper joined (`D-93`). Separate from
    /// `bytes_wiped` on purpose: one number is measured by canaries in the reaper, the other by a
    /// return value from the editor, and merging them would let the unmeasured half hide.
    in_place_bytes: usize,
}

/// Why a wipe could not be confirmed, in the reaper's own terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WipeUnconfirmed {
    /// The reaper thread produced no outcome (join yielded nothing).
    NoOutcome,
    /// The reaper ran, but nothing was registered to scrub: resident plaintext was never armed, so
    /// zero bytes may have been wiped.
    NothingArmed,
    /// The reaper ran and canaries were read back, but `ghost_canaries` of them still held non-zero
    /// bytes. The invariant broke *on this machine*, and the window says so.
    Ghosts {
        /// Canary buffers with at least one surviving non-zero byte.
        ghost_canaries: usize,
        /// Total surviving non-zero bytes.
        ghost_bytes: usize,
    },
}

impl WipeUnconfirmed {
    /// A sentence for the closing line, in voice with the window's other outcome words.
    ///
    /// `in_place_bytes` is what the editor scrubbed on its own account (`D-93`). It is reported
    /// here for the same reason it is reported on a confirmed wipe: the scrub happens whatever the
    /// verdict, and a line that stayed silent about it would leave a reader unable to tell a
    /// scrub that ran from one that regressed.
    fn describe(self, in_place_bytes: usize) -> String {
        let scrubbed = if in_place_bytes == 0 {
            "the editor had nothing left to scrub".to_owned()
        } else {
            format!(
                "the editor still scrubbed {} in place",
                panel::human(in_place_bytes)
            )
        };
        match self {
            Self::NoOutcome => format!(
                "emergency wipe fired but could not be confirmed — the reaper reported no outcome; \
                 {scrubbed}"
            ),
            Self::NothingArmed => format!(
                "emergency wipe fired but could not be confirmed — no resident arena was armed, \
                 so nothing was proven wiped; {scrubbed}"
            ),
            Self::Ghosts {
                ghost_canaries,
                ghost_bytes,
            } => format!(
                "emergency wipe FAILED verification — {ghost_canaries} canar{} still held \
                 {ghost_bytes} bytes: the wipe invariant broke on this machine; {scrubbed}",
                if ghost_canaries == 1 { "y" } else { "ies" }
            ),
        }
    }
}

impl WipeReceipt {
    /// Decides what a reaper outcome is entitled to claim.
    ///
    /// This is the whole of D-89: `wipe_quiescent` used to drop the outcome on the floor and the
    /// window printed a fixed sentence that neither affirmed nor denied a wipe. Splitting the
    /// outcome here means the *confirmed* branch can only be reached by an outcome that actually
    /// scrubbed bytes and left no ghost, and every other shape has to say so.
    fn from_outcome(outcome: &ReaperOutcome) -> Result<Self, WipeUnconfirmed> {
        if outcome.canaries.ghost_canaries > 0 {
            return Err(WipeUnconfirmed::Ghosts {
                ghost_canaries: outcome.canaries.ghost_canaries,
                ghost_bytes: outcome.canaries.ghost_bytes,
            });
        }
        if outcome.wipe.arenas_wiped == 0 || outcome.wipe.bytes_wiped == 0 {
            return Err(WipeUnconfirmed::NothingArmed);
        }
        Ok(Self {
            bytes_wiped: outcome.wipe.bytes_wiped,
            arenas_wiped: outcome.wipe.arenas_wiped,
            canaries: outcome.canaries.canaries_sampled,
            trip_to_verified_us: outcome.trip_to_verified_ns / 1_000,
            real_time: outcome.sched_class == SchedClass::RealTime,
            in_place_bytes: 0,
        })
    }

    /// Records the bytes the editor scrubbed in place, after the reaper joined (`D-93`).
    fn with_in_place(mut self, bytes: usize) -> Self {
        self.in_place_bytes = bytes;
        self
    }

    /// What the window says on the way out after a confirmed wipe.
    fn describe(self) -> String {
        format!(
            "emergency wipe confirmed — {} in {} arenas, plus {} scrubbed in place, {} canaries \
             clean, {} µs to verified ({}; coverage is registered arenas only)",
            panel::human(self.bytes_wiped),
            self.arenas_wiped,
            panel::human(self.in_place_bytes),
            self.canaries,
            self.trip_to_verified_us,
            if self.real_time {
                "real-time reaper"
            } else {
                "normal-priority reaper"
            }
        )
    }
}

#[derive(Debug, Clone)]
enum SearchPrompt {
    Find { needle: String, next_idx: u64 },
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

#[derive(Debug, Clone)]
struct TuiHistory {
    witnesses: Vec<holonomy_line::WitnessSummary>,
    selected: usize,
}

impl TuiHistory {
    fn render(&self) -> String {
        if let Some(w) = self.witnesses.get(self.selected) {
            let m_tag = match w.manifest_sequence {
                Some(m) => format!(", manifest #{m}"),
                None => String::new(),
            };
            format!(
                "History #{}/{} ({} segs{m_tag}) · Up/Down select · Enter restore · Esc close",
                w.sequence,
                self.witnesses.len(),
                w.segment_count
            )
        } else {
            "History empty (Esc close)".to_owned()
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
//
// D-89: the reaper's outcome is the *only* thing entitled to say a wipe happened, so it is kept
// and classified rather than dropped. `acknowledge_quiescence` is called before the join: it is
// this thread reporting that it has stopped touching the registered buffers, which is exactly the
// condition `wipe_quiescent` exists to establish — so the reaper is never left waiting on a
// "cooperative" gate for a deadline it already cannot meet.
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
    let outcome = reaper.wait_outcome();

    // D-93: the in-place scrub runs on **every** path, including the unconfirmed ones. D-89 moved
    // it inside the confirmed branch, reasoning that "an unverified wipe gets no supporting
    // gesture" — which confused a *report* with a *wipe*. `wipe_all_in_place` is the product-level
    // scrub of the buffers the editor owns (resident slabs, the typing buffer and its undo copies,
    // the window's viewport text); it is not the evidence the reaper's canaries provide. Skipping
    // it meant that on exactly the trips where the reaper could not prove a wipe, the editor's
    // plaintext was freed unwiped into the heap — the worst case of the state this function exists
    // to prevent. A failed verification is the strongest reason to scrub, not a reason to abstain.
    let wiped_in_place = app.editor().wipe_all_in_place();
    classify_trip(outcome, wiped_in_place)
}

/// What a finished trip is allowed to claim, given the reaper's outcome and the bytes the editor
/// scrubbed in place (`D-93`).
///
/// Pure on purpose: this is the *whole* decision `wipe_quiescent` makes after the join, so the
/// classification can be driven exhaustively in a test without an editor, a document or a thread.
/// The in-place scrub is deliberately **not** part of this function — it happens before it, on every
/// path — because making it conditional on the outcome was exactly the bug D-93 fixed.
fn classify_trip(outcome: Option<ReaperOutcome>, wiped_in_place: usize) -> Ended {
    match outcome {
        Some(outcome) => match WipeReceipt::from_outcome(&outcome) {
            Ok(receipt) => Ended::Wiped(receipt.with_in_place(wiped_in_place)),
            Err(unconfirmed) => Ended::WipeUnconfirmed(unconfirmed, wiped_in_place),
        },
        None => Ended::WipeUnconfirmed(WipeUnconfirmed::NoOutcome, wiped_in_place),
    }
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
    let mut history: Option<TuiHistory> = None;
    let mut panel: Option<(LockCapability, Option<u64>, Option<Instant>)> = None;
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
            now_rows.saturating_sub(2).min(7)
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
            Some((capability, verified, verified_at)) => {
                let age = verified_at.map(|t| t.elapsed().as_secs());
                let facts = Facts::gather_with(app.residency(), document, capability, verified)
                    .with_cooperative_reaper(&arenas, &canaries, reaper.has_tripped())
                    .with_window_security(
                        holonomy_gui::platform::WindowSecurityInfo {
                            clipboard_policy: "terminal selection unmanaged",
                            ..Default::default()
                        },
                        age,
                        false,
                    );
                term::panel_fits(&facts.lines(), now_rows)
            }
            None => Vec::new(),
        };
        let ribbon_or_msg = if let Some(prompt) = &search {
            prompt.render()
        } else if let Some(hist) = &history {
            hist.render()
        } else if !message.is_empty() {
            message.clone()
        } else if app.selection().is_some() {
            app.toolbar_summary()
        } else {
            let m = app.editor().status_metrics();
            let approx = if m.words_exact { "" } else { "~" };
            format!(
                "{approx}{}w · p.{}/{}",
                m.total_words, m.current_page, m.total_pages
            )
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
        let read = stdin.read(&mut bytes);
        // EOF or a read error must not take the ordinary exit path if a trip fired during read.
        if reaper.has_tripped() || tripwire.evaluate().is_some() {
            return Ok(wipe_quiescent(&mut app, document, &arenas, reaper));
        }
        let read = read?;
        if read == 0 {
            break;
        }
        tripwire.beat();
        // A heartbeat may itself latch an already-expired deadline.
        if reaper.has_tripped() || tripwire.evaluate().is_some() {
            return Ok(wipe_quiescent(&mut app, document, &arenas, reaper));
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
            if reaper.has_tripped() || tripwire.evaluate().is_some() {
                return Ok(wipe_quiescent(&mut app, document, &arenas, reaper));
            }
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
                                match app.editor().find_nth(&needle, next_idx) {
                                    Ok(summary) if summary.count == 0 => {
                                        message = format!("0 matches for '{needle}'");
                                        search = None;
                                    }
                                    Ok(summary) => {
                                        let idx = if next_idx < summary.count {
                                            next_idx
                                        } else {
                                            0
                                        };
                                        let hit = summary
                                            .selected
                                            .expect("a nonempty result has a selected match");
                                        let _ = app.editor().select_range(hit.start, hit.end);
                                        let _ = app.editor().apply(Intent::RevealCaret);
                                        message = format!("match {}/{}", idx + 1, summary.count);
                                        search = Some(SearchPrompt::Find {
                                            needle,
                                            next_idx: idx + 1,
                                        });
                                    }
                                    Err(error) => {
                                        message = format!("search refused: {error}");
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
                                    Err(error) => {
                                        message = format!("replace-all refused: {error}");
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
            if let Some(hist) = history.as_mut() {
                if modifiers.control && matches!(key, Key::Character('q') | Key::Character('c')) {
                    quit = true;
                    continue;
                }
                match key {
                    Key::Escape => {
                        history = None;
                    }
                    Key::Up | Key::Left => {
                        hist.selected = hist.selected.saturating_sub(1);
                    }
                    Key::Down | Key::Right => {
                        hist.selected =
                            (hist.selected + 1).min(hist.witnesses.len().saturating_sub(1));
                    }
                    Key::Enter => {
                        if let Some(w) = hist.witnesses.get(hist.selected) {
                            match document.spine_at_witness(w.sequence) {
                                Ok(sp) => {
                                    if app.editor().restore_spine(sp).is_ok() {
                                        let _ = app.editor().apply(Intent::RevealCaret);
                                        app.mark_dirty();
                                        message = format!("restored witness #{}", w.sequence);
                                    } else {
                                        message = "restore refused".to_owned();
                                    }
                                }
                                Err(err) => {
                                    message = format!("history load failed: {err}");
                                }
                            }
                        }
                        history = None;
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
                    Key::Character('r') => {
                        let witnesses = document.witness_history();
                        let selected = witnesses.len().saturating_sub(1);
                        history = Some(TuiHistory {
                            witnesses,
                            selected,
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
                            Some((
                                probe_capability(),
                                verified,
                                verified.map(|_| Instant::now()),
                            ))
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
        if reaper.has_tripped() || tripwire.evaluate().is_some() {
            return Ok(wipe_quiescent(&mut app, document, &arenas, reaper));
        }
        if quit {
            break;
        }
    }
    if reaper.has_tripped() || tripwire.evaluate().is_some() {
        return Ok(wipe_quiescent(&mut app, document, &arenas, reaper));
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
        "  holonomy [--gui | gui] [--deadman <ms>] [document.wavefunction]\n",
        "  holonomy verify <document.wavefunction>\n",
        "  holonomy compact <source.wavefunction> <destination.wavefunction>\n",
        "  holonomy import [--format plain|md] <source.txt|.md> <destination.wavefunction>\n",
        "  holonomy export [--force] [--format plain|md] <source.wavefunction> <destination.txt|.md>\n",
        "  holonomy migrate-v5 [--unpadded | --derived-cursor <n> | --device <path> [--cursor <n>] [--lba <n>] | --scan-cursor] [--usb-key <path>] <source.holo> <destination.wavefunction>\n",
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

fn parse_gui_subargs(args: &[String]) -> Option<(Option<PathBuf>, Option<Duration>)> {
    let mut deadman = None;
    let mut doc = None;
    let mut idx = 0usize;
    while idx < args.len() {
        match args[idx].as_str() {
            "--deadman" => {
                let raw = args.get(idx + 1)?;
                let millis = raw.parse::<u64>().ok().filter(|&v| v > 0)?;
                deadman = Some(Duration::from_millis(millis));
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

fn parse_format_flag(s: &str) -> Option<TransferFormat> {
    match s {
        "plain" | "txt" | "text" => Some(TransferFormat::PlainText),
        "md" | "markdown" => Some(TransferFormat::Markdown),
        _ => None,
    }
}

fn parse_import_args(args: &[String]) -> Option<(PathBuf, PathBuf, Option<TransferFormat>)> {
    let mut format = None;
    let mut pos = Vec::new();
    let mut idx = 0usize;
    while idx < args.len() {
        match args[idx].as_str() {
            "--format" => {
                let f = parse_format_flag(args.get(idx + 1)?)?;
                format = Some(f);
                idx += 2;
            }
            flag if flag.starts_with('-') => return None,
            p => {
                pos.push(PathBuf::from(p));
                idx += 1;
            }
        }
    }
    match pos.as_slice() {
        [src, dst] => Some((src.clone(), dst.clone(), format)),
        _ => None,
    }
}

fn parse_export_args(args: &[String]) -> Option<(PathBuf, PathBuf, Option<TransferFormat>, bool)> {
    let mut format = None;
    let mut force = false;
    let mut pos = Vec::new();
    let mut idx = 0usize;
    while idx < args.len() {
        match args[idx].as_str() {
            "--force" => {
                force = true;
                idx += 1;
            }
            "--format" => {
                let f = parse_format_flag(args.get(idx + 1)?)?;
                format = Some(f);
                idx += 2;
            }
            flag if flag.starts_with('-') => return None,
            p => {
                pos.push(PathBuf::from(p));
                idx += 1;
            }
        }
    }
    match pos.as_slice() {
        [src, dst] => Some((src.clone(), dst.clone(), format, force)),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct MigrateArgs {
    source: PathBuf,
    destination: PathBuf,
    unpadded: bool,
    derived_cursor: u64,
    device: Option<PathBuf>,
    device_cursor: u64,
    lba_start: u64,
    scan_cursor: bool,
    usb_key: Option<PathBuf>,
}

fn parse_migrate_args(args: &[String]) -> Option<MigrateArgs> {
    let mut unpadded = false;
    let mut derived_cursor = 0u64;
    let mut device = None;
    let mut device_cursor = 0u64;
    let mut lba_start = V5_ZONE_B_LBA_START;
    let mut scan_cursor = false;
    let mut usb_key = None;
    let mut pos = Vec::new();
    let mut idx = 0usize;
    while idx < args.len() {
        match args[idx].as_str() {
            "--unpadded" => {
                unpadded = true;
                idx += 1;
            }
            "--scan-cursor" => {
                scan_cursor = true;
                idx += 1;
            }
            "--derived-cursor" => {
                derived_cursor = args.get(idx + 1)?.parse::<u64>().ok()?;
                idx += 2;
            }
            "--device" => {
                device = Some(PathBuf::from(args.get(idx + 1)?));
                idx += 2;
            }
            "--cursor" => {
                device_cursor = args.get(idx + 1)?.parse::<u64>().ok()?;
                derived_cursor = device_cursor;
                idx += 2;
            }
            "--lba" => {
                lba_start = args.get(idx + 1)?.parse::<u64>().ok()?;
                idx += 2;
            }
            "--usb-key" => {
                usb_key = Some(PathBuf::from(args.get(idx + 1)?));
                idx += 2;
            }
            flag if flag.starts_with('-') => return None,
            p => {
                pos.push(PathBuf::from(p));
                idx += 1;
            }
        }
    }
    match pos.as_slice() {
        [src, dst] => Some(MigrateArgs {
            source: src.clone(),
            destination: dst.clone(),
            unpadded,
            derived_cursor,
            device,
            device_cursor,
            lba_start,
            scan_cursor,
            usb_key,
        }),
        _ => None,
    }
}

fn prompt_raw_line(verb: &str, path: &Path) -> std::io::Result<Zeroizing<String>> {
    let mut stdout = std::io::stdout();
    let is_tty = stdin_is_tty();
    if is_tty {
        write!(stdout, "{} {} (passphrase): ", verb, path.display())?;
        stdout.flush()?;
    }
    let mut stdin = std::io::stdin();
    let mut phrase = Zeroizing::new(Vec::with_capacity(MAX_PASSPHRASE_INPUT_BYTES));
    let mut byte = [0u8; 1];
    loop {
        if stdin.read(&mut byte)? == 0 {
            if is_tty {
                writeln!(stdout)?;
            }
            return Err(std::io::Error::other(
                "the passphrase was not completed (end of input)",
            ));
        }
        match byte[0] {
            b'\r' | b'\n' => break,
            0x7f | 0x08 => pop_utf8_scalar(&mut phrase),
            0x03 => {
                if is_tty {
                    writeln!(stdout)?;
                }
                return Err(std::io::Error::other("cancelled at the passphrase"));
            }
            next => {
                if phrase.len() >= MAX_PASSPHRASE_INPUT_BYTES {
                    return Err(std::io::Error::other(
                        "the passphrase exceeds the input byte limit",
                    ));
                }
                phrase.push(next);
            }
        }
    }
    if is_tty {
        writeln!(stdout)?;
        stdout.flush()?;
    }
    let s = std::str::from_utf8(&phrase)
        .map_err(|_| std::io::Error::other("the passphrase is not valid UTF-8"))?;
    Ok(Zeroizing::new(s.to_owned()))
}

fn run_import(
    source: &Path,
    destination: &Path,
    format: Option<TransferFormat>,
) -> Result<(), Box<dyn std::error::Error>> {
    let _echo = EchoGuard::acquire()?;
    let passphrase = prompt("encrypt imported document", destination)?;
    let (doc, report) = transfer::import_from_path(source, destination, &passphrase, format)?;
    drop(passphrase);
    let verified = doc.verify()?;
    println!(
        "{}: imported {} as {} ({} bytes, {} segments, {} style runs; verified {} segments)",
        destination.display(),
        source.display(),
        report.format.label(),
        report.plaintext_bytes,
        report.segments_written,
        report.style_runs,
        verified.segments_verified,
    );
    Ok(())
}

fn run_export(
    source: &Path,
    destination: &Path,
    format: Option<TransferFormat>,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let _echo = EchoGuard::acquire()?;
    let passphrase = prompt("open for plaintext export", source)?;
    let document = Document::open_input(source, &passphrase)?;
    drop(passphrase);
    let report = transfer::export_to_path(&document, destination, format, force)?;
    println!(
        "{}: exported {} as {} ({} bytes, {} segments read)\n{}",
        report.destination,
        source.display(),
        report.format.label(),
        report.bytes_written,
        report.segments_read,
        report.disclosure,
    );
    Ok(())
}

fn read_usb_key_32(path: &Path) -> Result<Zeroizing<[u8; 32]>, Box<dyn std::error::Error>> {
    let raw = Zeroizing::new(std::fs::read(path)?);
    if raw.len() != 32 {
        return Err(format!(
            "usb key file {} must be exactly 32 bytes (got {})",
            path.display(),
            raw.len()
        )
        .into());
    }
    let mut out = Zeroizing::new([0u8; 32]);
    out.copy_from_slice(&raw);
    Ok(out)
}

fn run_migrate_v5(cfg: &MigrateArgs) -> Result<(), Box<dyn std::error::Error>> {
    let _echo = EchoGuard::acquire()?;
    let raw_v5 = prompt_raw_line("unlock v5 container", &cfg.source)?;
    let v5_pass = V5Passphrase::new(&raw_v5).ok_or_else(|| {
        std::io::Error::other("v5 passphrase is empty or exceeds 255 UTF-8 bytes")
    })?;
    drop(raw_v5);
    let v6_pass = prompt("create v6 destination", &cfg.destination)?;
    let kappa_usb = match &cfg.usb_key {
        Some(p) => read_usb_key_32(p)?,
        None => Zeroizing::new([0u8; 32]),
    };

    let pad = if cfg.unpadded {
        V5PadSource::None
    } else if let Some(dev) = &cfg.device {
        let mut cursor = cfg.device_cursor;
        if cfg.scan_cursor {
            let _ = std::fs::File::open(dev)?;
            let mut cfile = std::fs::File::open(&cfg.source)?;
            let hits = holonomy_seal::v5::scan_v5_cursors(
                &mut cfile,
                &v5_pass,
                &kappa_usb,
                &mut |c| V5PadSource::DeviceFile {
                    path: dev.clone(),
                    cursor: c,
                    zone_b_lba_start: cfg.lba_start,
                },
                ParamsClass::Legacy,
            )?;
            cursor = *hits.first().ok_or_else(|| {
                std::io::Error::other("no matching v5 pad cursor found on device")
            })?;
        }
        V5PadSource::DeviceFile {
            path: dev.clone(),
            cursor,
            zone_b_lba_start: cfg.lba_start,
        }
    } else if cfg.scan_cursor {
        let mut cfile = std::fs::File::open(&cfg.source)?;
        let hits = holonomy_seal::v5::scan_v5_cursors(
            &mut cfile,
            &v5_pass,
            &kappa_usb,
            &mut |c| V5PadSource::Derived { cursor: c },
            ParamsClass::Legacy,
        )?;
        let cursor = *hits
            .first()
            .ok_or_else(|| std::io::Error::other("no matching derived v5 pad cursor found"))?;
        V5PadSource::Derived { cursor }
    } else {
        V5PadSource::Derived {
            cursor: cfg.derived_cursor,
        }
    };

    let report = transfer::migrate_v5_container(
        &cfg.source,
        &cfg.destination,
        &v5_pass,
        &v6_pass,
        &kappa_usb,
        &pad,
        None,
    )?;
    println!(
        "{}: migrated from v5.1.0 {} (stratum {}, cursor {}, {} v5 slots -> {} v6 segments, {} plaintext bytes, {} verified)",
        cfg.destination.display(),
        cfg.source.display(),
        report.source_stratum,
        report.source_cursor,
        report.source_slots,
        report.imported_segments,
        report.imported_bytes,
        report.verified_segments,
    );
    Ok(())
}

fn run_gui_command(
    doc: Option<&Path>,
    deadman: Option<Duration>,
) -> Result<(), Box<dyn std::error::Error>> {
    match run_desktop(doc, deadman)? {
        DesktopEnd::Saved | DesktopEnd::NothingToWrite | DesktopEnd::Cancelled => Ok(()),
        DesktopEnd::SaveFailed => {
            eprintln!(
                "holonomy gui: final save refused — bytes after last witness are unpersisted"
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
        [subcommand, rest @ ..] if subcommand == "gui" || subcommand == "--gui" => {
            let Some((doc, deadman)) = parse_gui_subargs(rest) else {
                eprintln!("{}", usage_text());
                std::process::exit(2);
            };
            run_gui_command(doc.as_deref(), deadman)
        }
        [subcommand, rest @ ..] if subcommand == "import" => {
            let Some((src, dst, fmt)) = parse_import_args(rest) else {
                eprintln!("{}", usage_text());
                std::process::exit(2);
            };
            run_import(&src, &dst, fmt)
        }
        [subcommand, rest @ ..] if subcommand == "export" => {
            let Some((src, dst, fmt, force)) = parse_export_args(rest) else {
                eprintln!("{}", usage_text());
                std::process::exit(2);
            };
            run_export(&src, &dst, fmt, force)
        }
        [subcommand, rest @ ..] if subcommand == "migrate-v5" => {
            let Some(cfg) = parse_migrate_args(rest) else {
                eprintln!("{}", usage_text());
                std::process::exit(2);
            };
            run_migrate_v5(&cfg)
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
                // An unconfirmed wipe must not exit zero: a person scripting around this app has to
                // see the protection fail, exactly as a refused save exits non-zero (D-49).
                Ok(Ended::Wiped(receipt)) => {
                    println!("{}", receipt.describe());
                    outcome_words(false, None)
                }
                Ok(Ended::WipeUnconfirmed(reason, scrubbed)) => {
                    println!("{}", reason.describe(scrubbed));
                    outcome_words(true, None)
                }
                Ok(Ended::SaveFailed) => outcome_words(false, Some(false)),
                Ok(Ended::Saved) => outcome_words(false, Some(true)),
                Ok(Ended::NothingToWrite) => outcome_words(false, None),
                Err(_) => outcome_words(true, None),
            };
            println!("{}", closing(&path, segments, witnesses, summary));
            if matches!(outcome, Ok(Ended::SaveFailed | Ended::WipeUnconfirmed(..))) {
                // A window that could not write, or could not verify its own wipe, must not exit as
                // though it had.
                std::process::exit(1);
            }
            outcome.map(|_| ())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use holonomy_seal::{CanaryReport, WipeStats};

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

    // --- D-89: a wipe is claimed only when the reaper measured it ----------------------------

    fn outcome(
        arenas_wiped: usize,
        bytes_wiped: usize,
        ghost_canaries: usize,
        ghost_bytes: usize,
    ) -> ReaperOutcome {
        ReaperOutcome {
            reason: TripReason::Manual,
            sched_class: SchedClass::NormalFallback,
            wipe: WipeStats {
                arenas_wiped,
                bytes_wiped,
            },
            canaries: CanaryReport {
                canaries_sampled: 8,
                bytes_sampled: 2048,
                ghost_canaries,
                ghost_bytes,
            },
            trip_to_wiped_ns: 300,
            trip_to_verified_ns: 880_000,
        }
    }

    #[test]
    fn a_wiped_arena_with_no_ghost_is_the_only_confirmed_wipe() {
        // The one shape entitled to say "confirmed": bytes were actually scrubbed and every canary
        // read back zero.
        let receipt = WipeReceipt::from_outcome(&outcome(1041, 4_324_384, 0, 0))
            .expect("a clean, non-empty wipe is confirmed");
        assert_eq!(receipt.bytes_wiped, 4_324_384);
        assert_eq!(receipt.arenas_wiped, 1041);
        assert_eq!(receipt.canaries, 8);
        // Nanoseconds are reported in microseconds, so a sub-millisecond wipe is not rounded to 0.
        assert_eq!(receipt.trip_to_verified_us, 880);
        assert!(
            !receipt.real_time,
            "a fallback-scheduled reaper must say so"
        );
        let described = receipt.describe();
        assert!(described.contains("confirmed"), "{described}");
        assert!(described.contains("1041 arenas"), "{described}");
        assert!(
            described.contains("normal-priority reaper"),
            "the receipt must not imply a real-time guarantee it did not get: {described}"
        );
    }

    #[test]
    fn a_wipe_that_scrubbed_nothing_is_never_called_confirmed() {
        // Zero armed arenas means zero bytes may have been wiped. A window that called this
        // "confirmed" would be the decorative-security failure L-7 exists to prevent.
        assert_eq!(
            WipeReceipt::from_outcome(&outcome(0, 0, 0, 0)),
            Err(WipeUnconfirmed::NothingArmed)
        );
        assert!(WipeUnconfirmed::NothingArmed
            .describe(0)
            .contains("could not be confirmed"));
    }

    #[test]
    fn a_surviving_canary_outranks_everything_else() {
        // Even a large wipe with a single surviving canary is a failure, and it is reported with
        // the counts rather than as a generic error.
        let reason = WipeReceipt::from_outcome(&outcome(1041, 4_324_384, 1, 4096))
            .expect_err("a ghost canary must not be confirmed");
        assert_eq!(
            reason,
            WipeUnconfirmed::Ghosts {
                ghost_canaries: 1,
                ghost_bytes: 4096
            }
        );
        let described = reason.describe(4096);
        assert!(described.contains("FAILED verification"), "{described}");
        assert!(described.contains("1 canary"), "{described}");
        assert!(described.contains("4096 bytes"), "{described}");
        // Pluralisation is not cosmetic here: the sentence is the only thing a user reads.
        let plural = WipeUnconfirmed::Ghosts {
            ghost_canaries: 3,
            ghost_bytes: 12,
        }
        .describe(0);
        assert!(plural.contains("3 canaries"), "{plural}");
    }

    #[test]
    fn an_outcome_that_never_arrived_is_not_a_wipe() {
        assert!(WipeUnconfirmed::NoOutcome
            .describe(0)
            .contains("could not be confirmed"));
    }

    #[test]
    fn an_unconfirmed_trip_still_reports_the_scrub_it_performed() {
        // D-93's reporting half. The scrub now runs on every path, so the closing line has to be
        // able to show it on every path: a failure sentence that went quiet about the in-place
        // bytes would make a working scrub and a regressed one read identically to a user.
        let scrubbed = WipeUnconfirmed::NoOutcome.describe(8192);
        assert!(scrubbed.contains("could not be confirmed"), "{scrubbed}");
        // `panel::human` renders 8192 as "8 KiB" — the number a reader gets is the formatted one.
        assert!(scrubbed.contains("scrubbed 8 KiB in place"), "{scrubbed}");
        // And it says the other thing when there was nothing to scrub, rather than "0 bytes".
        let nothing = WipeUnconfirmed::NothingArmed.describe(0);
        assert!(
            nothing.contains("had nothing left to scrub"),
            "a zero-byte scrub must not read as a measurement: {nothing}"
        );
        assert!(!nothing.contains("0 bytes"), "{nothing}");
    }

    #[test]
    fn every_trip_classifies_as_a_wipe_whatever_the_reaper_said() {
        // D-93, pinned exhaustively rather than by shape. `classify_trip` is the whole decision
        // `wipe_quiescent` makes once the reaper has joined, so every branch of it can be driven
        // here without a live editor, a document or a thread.
        //
        // The scrub itself is *not* tested here, and cannot be: `classify_trip` takes no editor, so
        // the in-place wipe is unreachable from this function by construction. That is the fix —
        // D-89 had made the scrub a branch of the verdict, so an unprovable trip skipped it and the
        // buffers the editor owns were freed unwiped. The scrub now precedes the call, on every
        // input including `None`; this test pins the verdict side, the signature pins the other.
        let cases: [Option<ReaperOutcome>; 4] = [
            // clean: bytes scrubbed, arenas wiped, no surviving canary
            Some(outcome(1041, 4_324_384, 0, 0)),
            // the reaper ran but nothing was armed
            Some(outcome(0, 0, 0, 0)),
            // bytes scrubbed, but a canary still held non-zero data
            Some(outcome(1041, 4_324_384, 1, 4096)),
            // the join yielded nothing at all
            None,
        ];
        for case in cases {
            let ended = classify_trip(case, 64 * 1024);
            // Whatever the verdict, it is a wipe verdict: a trip can never read as "saved".
            assert!(
                matches!(ended, Ended::Wiped(_) | Ended::WipeUnconfirmed(..)),
                "a trip must never classify as a save: {ended:?}"
            );
            // And the confirmed variant is the only one that may claim a wipe.
            let confirms = matches!(ended, Ended::Wiped(_));
            let should_confirm = matches!(
                case,
                Some(o) if o.wipe.arenas_wiped > 0
                    && o.wipe.bytes_wiped > 0
                    && o.canaries.ghost_canaries == 0
            );
            assert_eq!(confirms, should_confirm, "{ended:?} for {case:?}");
        }
        // The in-place count travels with a confirmed receipt.
        match classify_trip(Some(outcome(2, 4096, 0, 0)), 777) {
            Ended::Wiped(receipt) => assert_eq!(receipt.in_place_bytes, 777),
            other => panic!("expected a confirmed wipe, got {other:?}"),
        }
    }

    #[test]
    fn a_confirmed_wipe_is_the_only_wipe_that_does_not_end_the_session_badly() {
        // The exit status is the contract for anyone scripting around the app: an unconfirmed
        // wipe exits non-zero, exactly as a refused save does (D-49). Pinned here so a future
        // refactor cannot quietly route it through `outcome_words(false, None)`.
        let ended: Ended = Ended::Wiped(
            WipeReceipt::from_outcome(&outcome(2, 4096, 0, 0)).expect("clean wipe confirmed"),
        );
        assert!(
            !matches!(ended, Ended::SaveFailed | Ended::WipeUnconfirmed(..)),
            "a confirmed wipe is not a failure"
        );
        let unconfirmed: Ended = Ended::WipeUnconfirmed(WipeUnconfirmed::NoOutcome, 0);
        assert!(
            matches!(unconfirmed, Ended::WipeUnconfirmed(..)),
            "an unconfirmed wipe must take the non-zero exit path"
        );
    }

    #[test]
    fn parses_gui_import_export_and_migrate_subcommand_arguments() {
        let s = |xs: &[&str]| xs.iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();

        assert_eq!(parse_gui_subargs(&[]), Some((None, None)));
        assert_eq!(
            parse_gui_subargs(&s(&["--deadman", "2500", "notes.wavefunction"])),
            Some((
                Some(PathBuf::from("notes.wavefunction")),
                Some(Duration::from_millis(2500))
            ))
        );
        assert_eq!(parse_gui_subargs(&s(&["--unknown"])), None);

        assert_eq!(
            parse_import_args(&s(&["--format", "md", "notes.md", "out.wavefunction"])),
            Some((
                PathBuf::from("notes.md"),
                PathBuf::from("out.wavefunction"),
                Some(TransferFormat::Markdown)
            ))
        );
        assert_eq!(parse_import_args(&s(&["only_one.txt"])), None);

        assert_eq!(
            parse_export_args(&s(&[
                "--force",
                "--format",
                "plain",
                "doc.wavefunction",
                "out.txt"
            ])),
            Some((
                PathBuf::from("doc.wavefunction"),
                PathBuf::from("out.txt"),
                Some(TransferFormat::PlainText),
                true
            ))
        );
        assert_eq!(
            parse_export_args(&s(&["doc.wavefunction", "out.md"])),
            Some((
                PathBuf::from("doc.wavefunction"),
                PathBuf::from("out.md"),
                None,
                false
            ))
        );

        assert_eq!(
            parse_migrate_args(&s(&[
                "--device",
                "/dev/sdb",
                "--cursor",
                "3",
                "--lba",
                "65536",
                "--usb-key",
                "key.bin",
                "v5.holo",
                "v6.wavefunction"
            ])),
            Some(MigrateArgs {
                source: PathBuf::from("v5.holo"),
                destination: PathBuf::from("v6.wavefunction"),
                unpadded: false,
                derived_cursor: 3,
                device: Some(PathBuf::from("/dev/sdb")),
                device_cursor: 3,
                lba_start: 65536,
                scan_cursor: false,
                usb_key: Some(PathBuf::from("key.bin")),
            })
        );
    }
}
