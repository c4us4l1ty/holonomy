//! Pure-Rust / `libc` X11 core-protocol client over Unix domain sockets (zero external GUI/X11
//! dependencies).
//!
//! Speaks `.Xauthority` `MIT-MAGIC-COOKIE-1` authentication, connection setup, `CreateWindow`,
//! `MapWindow`, `PutImage` scanline chunking (`<= 262,140` bytes per request),
//! `GetKeyboardMapping`, `WM_DELETE_WINDOW` close notifications, `WM_NORMAL_HINTS`, and
//! `CLIPBOARD` (`UTF8_STRING`) selection copy/paste with `Zeroizing` clipboard buffers.

use std::collections::VecDeque;
use std::fmt;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use zeroize::{Zeroize, Zeroizing};

use crate::{Key, Modifiers};

/// Maximum core-protocol request size without `BIG-REQUESTS` (`65,535 * 4` bytes).
pub const MAX_REQUEST_BYTES: usize = 65_535 * 4;
/// Fixed wire size of every core X11 event packet (`32` bytes).
pub const EVENT_BYTES: usize = 32;
/// Maximum clipboard paste size accepted into the editor in a single paste (`4 KiB`).
pub const MAX_CLIPBOARD_BYTES: usize = 4_096;

const SETUP_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_REPLY_BYTES: usize = 16 * 1024 * 1024;
const FAMILY_LOCAL: u16 = 256;
const FAMILY_WILD: u16 = 65_535;
const COOKIE_NAME: &str = "MIT-MAGIC-COOKIE-1";
const COOKIE_BYTES: usize = 16;

/// Extracts the local display number from `$DISPLAY` (e.g. `:0`, `:1.0`, `unix:0`,
/// `localhost:10.0`), returning `None` for TCP or malformed values.
#[must_use]
pub fn display_number(display: &str) -> Option<u32> {
    let (host, spec) = display.rsplit_once(':')?;
    if !(host.is_empty() || host == "unix" || host == "localhost") {
        return None;
    }
    let digits: String = spec.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Core protocol request opcodes.
pub mod op {
    /// CreateWindow.
    pub const CREATE_WINDOW: u8 = 1;
    /// DestroyWindow.
    pub const DESTROY_WINDOW: u8 = 4;
    /// MapWindow.
    pub const MAP_WINDOW: u8 = 8;
    /// InternAtom.
    pub const INTERN_ATOM: u8 = 16;
    /// ChangeProperty.
    pub const CHANGE_PROPERTY: u8 = 18;
    /// DeleteProperty.
    pub const DELETE_PROPERTY: u8 = 19;
    /// GetProperty.
    pub const GET_PROPERTY: u8 = 20;
    /// SetSelectionOwner.
    pub const SET_SELECTION_OWNER: u8 = 22;
    /// ConvertSelection.
    pub const CONVERT_SELECTION: u8 = 24;
    /// SendEvent.
    pub const SEND_EVENT: u8 = 25;
    /// SetInputFocus.
    pub const SET_INPUT_FOCUS: u8 = 42;
    /// GetInputFocus.
    pub const GET_INPUT_FOCUS: u8 = 43;
    /// CreateGC.
    pub const CREATE_GC: u8 = 55;
    /// FreeGC.
    pub const FREE_GC: u8 = 60;
    /// PutImage.
    pub const PUT_IMAGE: u8 = 72;
    /// GetKeyboardMapping.
    pub const GET_KEYBOARD_MAPPING: u8 = 101;
}

/// Core protocol event codes (`code & 0x7F`).
pub mod event_code {
    /// KeyPress.
    pub const KEY_PRESS: u8 = 2;
    /// KeyRelease.
    pub const KEY_RELEASE: u8 = 3;
    /// ButtonPress.
    pub const BUTTON_PRESS: u8 = 4;
    /// ButtonRelease.
    pub const BUTTON_RELEASE: u8 = 5;
    /// MotionNotify.
    pub const MOTION_NOTIFY: u8 = 6;
    /// Expose.
    pub const EXPOSE: u8 = 12;
    /// MapNotify.
    pub const MAP_NOTIFY: u8 = 19;
    /// ConfigureNotify.
    pub const CONFIGURE_NOTIFY: u8 = 22;
    /// SelectionClear.
    pub const SELECTION_CLEAR: u8 = 29;
    /// SelectionRequest.
    pub const SELECTION_REQUEST: u8 = 30;
    /// SelectionNotify.
    pub const SELECTION_NOTIFY: u8 = 31;
    /// ClientMessage.
    pub const CLIENT_MESSAGE: u8 = 33;
    /// SendEvent bit on packet byte 0.
    pub const SEND_EVENT_FLAG: u8 = 0x80;
}

/// Event-mask bits used in `CreateWindow`.
pub mod mask {
    /// KeyPressMask.
    pub const KEY_PRESS: u32 = 0x0000_0001;
    /// ButtonPressMask.
    pub const BUTTON_PRESS: u32 = 0x0000_0004;
    /// ButtonReleaseMask.
    pub const BUTTON_RELEASE: u32 = 0x0000_0008;
    /// PointerMotionMask.
    pub const POINTER_MOTION: u32 = 0x0000_0040;
    /// ExposureMask.
    pub const EXPOSURE: u32 = 0x0000_8000;
    /// StructureNotifyMask.
    pub const STRUCTURE_NOTIFY: u32 = 0x0002_0000;
}

mod cw {
    pub const BACK_PIXEL: u32 = 0x0000_0002;
    pub const BORDER_PIXEL: u32 = 0x0000_0008;
    pub const EVENT_MASK: u32 = 0x0000_0800;
}

mod gc {
    pub const FOREGROUND: u32 = 0x0000_0004;
    pub const BACKGROUND: u32 = 0x0000_0008;
    pub const GRAPHICS_EXPOSURES: u32 = 0x0001_0000;
}

mod atom {
    pub const ATOM: u32 = 4;
    pub const STRING: u32 = 31;
    pub const WM_NAME: u32 = 39;
    pub const WM_NORMAL_HINTS: u32 = 40;
    pub const WM_SIZE_HINTS: u32 = 41;
}

/// One parsed `.Xauthority` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRecord {
    /// Xauthority address family (`256` local, `65535` wild).
    pub family: u16,
    /// Display number text (`""` for family-local records).
    pub number: String,
    /// Auth scheme name (`MIT-MAGIC-COOKIE-1`).
    pub name: String,
    /// Raw cookie payload.
    pub data: Vec<u8>,
}

/// Parses big-endian binary `.Xauthority` bytes into [`AuthRecord`] entries.
///
/// # Errors
///
/// Returns [`X11Error::Auth`] if a length-prefixed record is truncated.
pub fn parse_xauthority(bytes: &[u8]) -> Result<Vec<AuthRecord>, X11Error> {
    let mut out = Vec::new();
    let mut at = 0usize;
    let take_u16 = |src: &[u8], off: &mut usize| -> Result<u16, X11Error> {
        let end = off
            .checked_add(2)
            .filter(|&e| e <= src.len())
            .ok_or_else(|| X11Error::Auth(format!("truncated .Xauthority at byte {}", *off)))?;
        let val = u16::from_be_bytes([src[*off], src[end - 1]]);
        *off = end;
        Ok(val)
    };
    let take_bytes = |src: &[u8], off: &mut usize| -> Result<Vec<u8>, X11Error> {
        let len = usize::from(take_u16(src, off)?);
        let end = off
            .checked_add(len)
            .filter(|&e| e <= src.len())
            .ok_or_else(|| X11Error::Auth(format!("truncated .Xauthority field at {}", *off)))?;
        let slice = src[*off..end].to_vec();
        *off = end;
        Ok(slice)
    };
    while at < bytes.len() {
        let family = take_u16(bytes, &mut at)?;
        let _address = take_bytes(bytes, &mut at)?;
        let number = take_bytes(bytes, &mut at)?;
        let name = take_bytes(bytes, &mut at)?;
        let data = take_bytes(bytes, &mut at)?;
        out.push(AuthRecord {
            family,
            number: String::from_utf8_lossy(&number).into_owned(),
            name: String::from_utf8_lossy(&name).into_owned(),
            data,
        });
    }
    Ok(out)
}

/// Selects the `MIT-MAGIC-COOKIE-1` cookie matching `display` from `xauth_bytes`.
///
/// # Errors
///
/// Returns [`X11Error::Auth`] if parsing fails, no matching cookie exists, or the cookie length is
/// not 16 bytes.
pub fn select_xauth_cookie(xauth_bytes: &[u8], display: u32) -> Result<Vec<u8>, X11Error> {
    let records = parse_xauthority(xauth_bytes)?;
    let wanted = display.to_string();
    let pick = records
        .iter()
        .filter(|r| r.name == COOKIE_NAME)
        .find(|r| r.family == FAMILY_LOCAL && r.number.is_empty())
        .or_else(|| {
            records
                .iter()
                .find(|r| r.name == COOKIE_NAME && r.number == wanted)
        })
        .or_else(|| {
            records
                .iter()
                .find(|r| r.name == COOKIE_NAME && r.family == FAMILY_WILD)
        })
        .ok_or_else(|| {
            X11Error::Auth(format!(
                "no {COOKIE_NAME} cookie in .Xauthority for display :{display}"
            ))
        })?;
    if pick.data.len() != COOKIE_BYTES {
        return Err(X11Error::Auth(format!(
            "cookie length {} != {COOKIE_BYTES}",
            pick.data.len()
        )));
    }
    Ok(pick.data.clone())
}

fn default_xauthority_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("XAUTHORITY") {
        if !p.is_empty() {
            return Some(PathBuf::from(p));
        }
    }
    std::env::var_os("HOME").map(|h| Path::new(&h).join(".Xauthority"))
}

/// Little-endian X11 request encoder.
#[derive(Debug, Clone)]
pub struct Req {
    buf: Vec<u8>,
    has_second_byte: bool,
}

impl Req {
    /// Starts a request with `opcode` and a reserved 4-byte header.
    #[must_use]
    pub fn new(opcode: u8) -> Self {
        Self {
            buf: vec![opcode, 0, 0, 0],
            has_second_byte: false,
        }
    }

    /// Sets protocol header byte 1 (for requests whose byte 1 carries a field rather than sequence).
    #[must_use]
    pub fn second_byte(mut self, v: u8) -> Self {
        self.buf[1] = v;
        self.has_second_byte = true;
        self
    }

    /// Appends one byte.
    #[must_use]
    pub fn u8(mut self, v: u8) -> Self {
        self.buf.push(v);
        self
    }

    /// Appends one little-endian `u16`.
    #[must_use]
    pub fn u16(mut self, v: u16) -> Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// Appends one little-endian `i16`.
    #[must_use]
    pub fn i16(mut self, v: i16) -> Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// Appends one little-endian `u32`.
    #[must_use]
    pub fn u32(mut self, v: u32) -> Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// Appends `n` zero padding bytes.
    #[must_use]
    pub fn pad(mut self, n: usize) -> Self {
        self.buf.resize(self.buf.len() + n, 0);
        self
    }

    /// Appends raw payload bytes.
    #[must_use]
    pub fn bytes(mut self, slice: &[u8]) -> Self {
        self.buf.extend_from_slice(slice);
        self
    }

    /// 4-byte-pads and finishes the request packet with its word length and sequence byte.
    #[must_use]
    pub fn finish(mut self, seq: u16) -> Vec<u8> {
        while (self.buf.len() & 3) != 0 {
            self.buf.push(0);
        }
        let words = (self.buf.len() / 4) as u16;
        self.buf[2..4].copy_from_slice(&words.to_le_bytes());
        if !self.has_second_byte {
            self.buf[1] = (seq & 0xFF) as u8;
        }
        self.buf
    }
}

/// Little-endian reply/event slice reader.
pub struct Rdr<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Rdr<'a> {
    /// Creates a wire reader over `buf`.
    #[must_use]
    pub const fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }

    fn take(&mut self, n: usize) -> &'a [u8] {
        let end = self
            .at
            .checked_add(n)
            .expect("X11 packet field out of bounds");
        let slice = &self.buf[self.at..end];
        self.at = end;
        slice
    }

    /// Reads a `u8`.
    pub fn u8(&mut self) -> u8 {
        self.take(1)[0]
    }

    /// Reads a little-endian `u16`.
    pub fn u16(&mut self) -> u16 {
        let b = self.take(2);
        u16::from_le_bytes([b[0], b[1]])
    }

    /// Reads a little-endian `i16`.
    pub fn i16(&mut self) -> i16 {
        let b = self.take(2);
        i16::from_le_bytes([b[0], b[1]])
    }

    /// Reads a little-endian `u32`.
    pub fn u32(&mut self) -> u32 {
        let b = self.take(4);
        u32::from_le_bytes([b[0], b[1], b[2], b[3]])
    }

    /// Reads `n` raw bytes.
    pub fn bytes(&mut self, n: usize) -> &'a [u8] {
        self.take(n)
    }

    /// Advances `n` bytes.
    pub fn skip(&mut self, n: usize) {
        self.take(n);
    }
}

/// A decoded 32-byte core X11 event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireEvent {
    /// Key went down.
    KeyPress {
        /// Raw X11 keycode.
        keycode: u8,
        /// Server modifier mask (`Shift=1`, `Lock=2`, `Control=4`, `Mod1=8`).
        state: u16,
    },
    /// Key came up.
    KeyRelease {
        /// Raw X11 keycode.
        keycode: u8,
        /// Server modifier mask.
        state: u16,
    },
    /// Pointer button pressed (`1=left`, `4=wheel-up`, `5=wheel-down`).
    ButtonPress {
        /// X button code.
        button: u8,
        /// Event window local X coordinate (`px`).
        x: i16,
        /// Event window local Y coordinate (`px`).
        y: i16,
        /// Server modifier mask.
        state: u16,
    },
    /// Pointer button released.
    ButtonRelease {
        /// X button code.
        button: u8,
        /// Event window local X coordinate (`px`).
        x: i16,
        /// Event window local Y coordinate (`px`).
        y: i16,
    },
    /// Pointer moved.
    MotionNotify {
        /// Event window local X coordinate (`px`).
        x: i16,
        /// Event window local Y coordinate (`px`).
        y: i16,
    },
    /// Damage/expose rectangle.
    Expose {
        /// Exposed region X.
        x: u16,
        /// Exposed region Y.
        y: u16,
        /// Exposed width.
        width: u16,
        /// Exposed height.
        height: u16,
        /// Trailing queued expose count (`0` means final expose of batch).
        count: u16,
    },
    /// Window mapped.
    MapNotify,
    /// Window geometry updated.
    ConfigureNotify {
        /// Window pixel width.
        width: u16,
        /// Window pixel height.
        height: u16,
    },
    /// Window manager client message (`WM_DELETE_WINDOW` arrives here).
    ClientMessage {
        /// Protocol/message type atom.
        type_atom: u32,
        /// First `CARD32` payload slot (`data[0]`).
        data0: u32,
    },
    /// Lost ownership of a selection.
    SelectionClear {
        /// Selection atom.
        selection: u32,
    },
    /// Peer requests selection data.
    SelectionRequest {
        /// Request timestamp.
        time: u32,
        /// Requestor window ID.
        requestor: u32,
        /// Selection atom (`CLIPBOARD`).
        selection: u32,
        /// Target atom (`TARGETS`, `UTF8_STRING`, `STRING`).
        target: u32,
        /// Destination property atom on `requestor`.
        property: u32,
    },
    /// Selection data conversion ready.
    SelectionNotify {
        /// Selection atom.
        selection: u32,
        /// Target atom.
        target: u32,
        /// Property holding converted data (`0` if conversion failed).
        property: u32,
    },
    /// Any unhandled X11 event.
    Other(u8),
}

impl WireEvent {
    /// Decodes one 32-byte X11 wire event packet.
    ///
    /// Uses exact X11 core protocol byte offsets (`event_x`/`event_y` at bytes `24..28` on input
    /// events, `Expose` window at `4..8` and rect at `8..18`).
    #[must_use]
    pub fn decode(packet: &[u8; EVENT_BYTES]) -> Self {
        let mut r = Rdr::new(packet);
        let code = r.u8() & !event_code::SEND_EVENT_FLAG;
        match code {
            event_code::KEY_PRESS => {
                let keycode = r.u8();
                let _seq = r.u16();
                let _time = r.u32();
                let _root = r.u32();
                let _event = r.u32();
                let _child = r.u32();
                let _rx = r.i16();
                let _ry = r.i16();
                let _ex = r.i16();
                let _ey = r.i16();
                let state = r.u16();
                Self::KeyPress { keycode, state }
            }
            event_code::KEY_RELEASE => {
                let keycode = r.u8();
                let _seq = r.u16();
                let _time = r.u32();
                let _root = r.u32();
                let _event = r.u32();
                let _child = r.u32();
                let _rx = r.i16();
                let _ry = r.i16();
                let _ex = r.i16();
                let _ey = r.i16();
                let state = r.u16();
                Self::KeyRelease { keycode, state }
            }
            event_code::BUTTON_PRESS => {
                let button = r.u8();
                let _seq = r.u16();
                let _time = r.u32();
                let _root = r.u32();
                let _event = r.u32();
                let _child = r.u32();
                let _rx = r.i16();
                let _ry = r.i16();
                let x = r.i16();
                let y = r.i16();
                let state = r.u16();
                Self::ButtonPress {
                    button,
                    x,
                    y,
                    state,
                }
            }
            event_code::BUTTON_RELEASE => {
                let button = r.u8();
                let _seq = r.u16();
                let _time = r.u32();
                let _root = r.u32();
                let _event = r.u32();
                let _child = r.u32();
                let _rx = r.i16();
                let _ry = r.i16();
                let x = r.i16();
                let y = r.i16();
                Self::ButtonRelease { button, x, y }
            }
            event_code::MOTION_NOTIFY => {
                let _detail = r.u8();
                let _seq = r.u16();
                let _time = r.u32();
                let _root = r.u32();
                let _event = r.u32();
                let _child = r.u32();
                let _rx = r.i16();
                let _ry = r.i16();
                let x = r.i16();
                let y = r.i16();
                Self::MotionNotify { x, y }
            }
            event_code::EXPOSE => {
                let _unused = r.u8();
                let _seq = r.u16();
                let _window = r.u32();
                let x = r.u16();
                let y = r.u16();
                let width = r.u16();
                let height = r.u16();
                let count = r.u16();
                Self::Expose {
                    x,
                    y,
                    width,
                    height,
                    count,
                }
            }
            event_code::MAP_NOTIFY => Self::MapNotify,
            event_code::CONFIGURE_NOTIFY => {
                let _unused = r.u8();
                let _seq = r.u16();
                let _event = r.u32();
                let _window = r.u32();
                let _above = r.u32();
                let _x = r.i16();
                let _y = r.i16();
                let width = r.u16();
                let height = r.u16();
                Self::ConfigureNotify { width, height }
            }
            event_code::CLIENT_MESSAGE => {
                let _format = r.u8();
                let _seq = r.u16();
                let _window = r.u32();
                let type_atom = r.u32();
                let data0 = r.u32();
                Self::ClientMessage { type_atom, data0 }
            }
            event_code::SELECTION_CLEAR => {
                let _unused = r.u8();
                let _seq = r.u16();
                let _time = r.u32();
                let _owner = r.u32();
                let selection = r.u32();
                Self::SelectionClear { selection }
            }
            event_code::SELECTION_REQUEST => {
                let _unused = r.u8();
                let _seq = r.u16();
                let time = r.u32();
                let _owner = r.u32();
                let requestor = r.u32();
                let selection = r.u32();
                let target = r.u32();
                let property = r.u32();
                Self::SelectionRequest {
                    time,
                    requestor,
                    selection,
                    target,
                    property,
                }
            }
            event_code::SELECTION_NOTIFY => {
                let _unused = r.u8();
                let _seq = r.u16();
                let _time = r.u32();
                let _requestor = r.u32();
                let selection = r.u32();
                let target = r.u32();
                let property = r.u32();
                Self::SelectionNotify {
                    selection,
                    target,
                    property,
                }
            }
            other => Self::Other(other),
        }
    }
}

/// Converts a keysym and raw X11 modifier state into an editor [`Key`] and [`Modifiers`].
#[must_use]
pub fn keysym_to_key(keysym: u32, state: u16) -> Option<(Key, Modifiers)> {
    let shift = (state & 0x01) != 0;
    let control = (state & 0x04) != 0;
    let alt = (state & 0x08) != 0;
    let modifiers = Modifiers {
        shift,
        control,
        alt,
    };
    let key = match keysym {
        0xff08 => Key::Backspace,
        0xff09 | 0xfe20 => Key::Tab,
        0xff0d | 0xff8d => Key::Enter,
        0xff1b => Key::Escape,
        0xff50 | 0xff95 => Key::Home,
        0xff51 | 0xff96 => Key::Left,
        0xff52 | 0xff97 => Key::Up,
        0xff53 | 0xff98 => Key::Right,
        0xff54 | 0xff99 => Key::Down,
        0xff55 | 0xff9a => Key::PageUp,
        0xff56 | 0xff9b => Key::PageDown,
        0xff57 | 0xff9c => Key::End,
        0xffff | 0xff9f => Key::Delete,
        0x0020..=0x007e | 0x00a0..=0x00ff => {
            let mut ch = char::from_u32(keysym)?;
            if control {
                ch = ch.to_ascii_lowercase();
            }
            Key::Character(ch)
        }
        0x0100_0000..=0x0110_ffff => {
            let cp = keysym & 0x00ff_ffff;
            let ch = char::from_u32(cp)?;
            Key::Character(ch)
        }
        _ => return None,
    };
    Some((key, modifiers))
}

/// Keycode-to-keysym lookup table retrieved from `GetKeyboardMapping`.
#[derive(Debug, Clone)]
pub struct KeyboardMap {
    min_keycode: u8,
    keysyms_per_keycode: usize,
    table: Vec<u32>,
}

impl KeyboardMap {
    /// Parses a `GetKeyboardMapping` reply payload (`keysyms_per_keycode`, raw keysym words).
    #[must_use]
    pub fn new(min_keycode: u8, keysyms_per_keycode: usize, table: Vec<u32>) -> Self {
        Self {
            min_keycode,
            keysyms_per_keycode,
            table,
        }
    }

    /// Resolves `(keycode, state)` to `(Key, Modifiers)` applying Shift (`col 1`) and CapsLock.
    #[must_use]
    pub fn translate(&self, keycode: u8, state: u16) -> Option<(Key, Modifiers)> {
        if keycode < self.min_keycode || self.keysyms_per_keycode == 0 {
            return None;
        }
        let base = usize::from(keycode - self.min_keycode) * self.keysyms_per_keycode;
        if base >= self.table.len() {
            return None;
        }
        let sym0 = self.table[base];
        let sym1 = if self.keysyms_per_keycode > 1 {
            self.table.get(base + 1).copied().unwrap_or(0)
        } else {
            0
        };
        let shift = (state & 0x01) != 0;
        let lock = (state & 0x02) != 0;
        let is_ascii_letter = (0x61..=0x7a).contains(&sym0) || (0x41..=0x5a).contains(&sym0);
        let upper = if is_ascii_letter { shift ^ lock } else { shift };
        let sym = if upper && sym1 != 0 { sym1 } else { sym0 };
        keysym_to_key(sym, state)
    }
}

/// High-level input/window events dispatched from the X11 server to the GUI loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopEvent {
    /// Key press translated through [`KeyboardMap`].
    Key {
        /// Key symbol.
        key: Key,
        /// Active modifier flags.
        modifiers: Modifiers,
    },
    /// Left mouse press (`x, y` in window coordinates).
    MouseDown {
        /// Window-relative X pixel.
        x: i16,
        /// Window-relative Y pixel.
        y: i16,
        /// Active modifiers (`Shift` extends selection).
        modifiers: Modifiers,
    },
    /// Left mouse drag motion (`x, y` in window coordinates).
    MouseDrag {
        /// Window-relative X pixel.
        x: i16,
        /// Window-relative Y pixel.
        y: i16,
    },
    /// Left mouse release.
    MouseUp,
    /// Mouse scroll wheel delta (`lines < 0` scrolls up, `> 0` scrolls down).
    Scroll {
        /// Scroll direction in logical steps (`-1` up, `+1` down).
        delta_lines: i32,
    },
    /// Window resized to `(width, height)` pixels.
    Resize {
        /// Window width (`px`).
        width: u16,
        /// Window height (`px`).
        height: u16,
    },
    /// Window content must be pushed to the X server again.
    Expose,
    /// Window manager asked to close (`WM_DELETE_WINDOW`).
    CloseRequested,
    /// Clipboard UTF-8 contents arrived in response to `request_clipboard`.
    ClipboardPasted(Zeroizing<Vec<u8>>),
}

/// Errors returned while connecting to X11 or communicating over the display socket.
#[derive(Debug)]
pub enum X11Error {
    /// Invalid or remote `$DISPLAY`.
    BadDisplay(String),
    /// `.Xauthority` missing, unreadable, or lacks a matching cookie.
    Auth(String),
    /// X11 connection setup refused by server.
    Refused(String),
    /// Server pixel format incompatible with 32-bpp `LSBFirst` `0x00RRGGBB`.
    Unsupported(String),
    /// Wire request/reply failed.
    Protocol(String),
    /// Underlying socket I/O error.
    Io(std::io::Error),
}

impl fmt::Display for X11Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadDisplay(s) => write!(f, "X11 display error: {s}"),
            Self::Auth(s) => write!(f, "X11 auth error: {s}"),
            Self::Refused(s) => write!(f, "X11 server refused connection: {s}"),
            Self::Unsupported(s) => write!(f, "unsupported X11 server: {s}"),
            Self::Protocol(s) => write!(f, "X11 protocol error: {s}"),
            Self::Io(e) => write!(f, "X11 socket I/O error: {e}"),
        }
    }
}

impl std::error::Error for X11Error {}

impl From<std::io::Error> for X11Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Server screen & resource allocation metadata learned during X11 setup.
#[derive(Debug, Clone)]
pub struct SetupInfo {
    /// Base resource ID allocated to this client.
    pub resource_id_base: u32,
    /// Mask of resource ID bits this client owns.
    pub resource_id_mask: u32,
    /// Maximum server request size in 4-byte words.
    pub max_request_words: u32,
    /// Root window XID.
    pub root: u32,
    /// Root depth (`24`).
    pub root_depth: u8,
    /// Root visual ID.
    pub root_visual: u32,
    /// Root white pixel.
    pub white_pixel: u32,
    /// Root width (`px`).
    pub root_width: u16,
    /// Root height (`px`).
    pub root_height: u16,
    /// Minimum X keycode.
    pub min_keycode: u8,
    /// Maximum X keycode.
    pub max_keycode: u8,
    /// Estimated screen DPI (`96` fallback when physical mm are zero).
    pub dpi: i64,
}

/// Builds the 72-byte `WM_SIZE_HINTS` property enforcing initial `PSize` and minimum `PMinSize`.
#[must_use]
pub fn size_hints_bytes(width: u16, height: u16, min_w: u16, min_h: u16) -> [u8; 72] {
    const P_SIZE: u32 = 1 << 3;
    const P_MIN_SIZE: u32 = 1 << 4;
    let mut bytes = [0u8; 72];
    let put = |dst: &mut [u8], word: usize, val: u32| {
        dst[word * 4..word * 4 + 4].copy_from_slice(&val.to_le_bytes());
    };
    put(&mut bytes, 0, P_SIZE | P_MIN_SIZE);
    put(&mut bytes, 3, u32::from(width));
    put(&mut bytes, 4, u32::from(height));
    put(&mut bytes, 5, u32::from(min_w));
    put(&mut bytes, 6, u32::from(min_h));
    bytes
}

/// Live X11 desktop window with keyboard, pointer, resize, close protocol, and clipboard support.
pub struct X11Window {
    stream: UnixStream,
    setup: SetupInfo,
    seq: u16,
    next_id: u32,
    window: u32,
    gc: u32,
    width: u16,
    height: u16,
    wm_protocols: u32,
    wm_delete_window: u32,
    net_wm_name: u32,
    utf8_string: u32,
    clipboard_atom: u32,
    targets_atom: u32,
    sel_prop_atom: u32,
    keymap: KeyboardMap,
    rbuf: Vec<u8>,
    rstart: usize,
    events: VecDeque<WireEvent>,
    left_button_down: bool,
    clipboard_owned: Zeroizing<Vec<u8>>,
    staging_bytes: Vec<u8>,
}

impl X11Window {
    /// Connects to `$DISPLAY` (or `display`), authenticates via `.Xauthority`, creates and maps a
    /// `width × height` window titled `title`, and installs `WM_DELETE_WINDOW`.
    ///
    /// # Errors
    ///
    /// Returns [`X11Error`] if the display cannot be reached, lacks a 32-bpp format for root
    /// depth, or refuses connection setup.
    pub fn open(
        display: Option<&str>,
        title: &str,
        width: u16,
        height: u16,
    ) -> Result<Self, X11Error> {
        let spec = display
            .map(str::to_owned)
            .or_else(|| std::env::var("DISPLAY").ok())
            .unwrap_or_else(|| ":0".to_owned());
        let number = display_number(&spec).ok_or_else(|| {
            X11Error::BadDisplay(format!(
                "DISPLAY={spec:?} is missing or uses TCP; local Unix X11 socket required"
            ))
        })?;
        let sock_path = format!("/tmp/.X11-unix/X{number}");
        let mut stream = connect_x11_socket(&sock_path)?;
        stream.set_read_timeout(Some(SETUP_TIMEOUT))?;
        stream.set_write_timeout(Some(SETUP_TIMEOUT))?;

        let cookie = default_xauthority_path()
            .map(|p| std::fs::read(&p).map_err(|e| X11Error::Auth(format!("{}: {e}", p.display()))))
            .transpose()?
            .map(|bytes| select_xauth_cookie(&bytes, number))
            .transpose()?
            .unwrap_or_default();

        let mut init = Vec::with_capacity(32 + cookie.len());
        init.extend_from_slice(&[0x6C, 0]);
        init.extend_from_slice(&11u16.to_le_bytes());
        init.extend_from_slice(&0u16.to_le_bytes());
        if cookie.is_empty() {
            init.extend_from_slice(&0u16.to_le_bytes());
            init.extend_from_slice(&0u16.to_le_bytes());
            init.extend_from_slice(&0u16.to_le_bytes());
        } else {
            init.extend_from_slice(&(COOKIE_NAME.len() as u16).to_le_bytes());
            init.extend_from_slice(&(cookie.len() as u16).to_le_bytes());
            init.extend_from_slice(&0u16.to_le_bytes());
            init.extend_from_slice(COOKIE_NAME.as_bytes());
            while (init.len() & 3) != 0 {
                init.push(0);
            }
            init.extend_from_slice(&cookie);
            while (init.len() & 3) != 0 {
                init.push(0);
            }
        }
        stream.write_all(&init)?;

        let mut head = [0u8; 8];
        stream.read_exact(&mut head)?;
        let status = head[0];
        let extra_words = u16::from_le_bytes([head[6], head[7]]) as usize;
        if status == 0 {
            let reason_len = usize::from(head[1]);
            let mut reason = vec![0u8; extra_words * 4];
            stream.read_exact(&mut reason)?;
            let msg = String::from_utf8_lossy(&reason[..reason_len.min(reason.len())]).into_owned();
            return Err(X11Error::Refused(msg));
        }
        if status != 1 {
            return Err(X11Error::Unsupported(format!("setup status {status}")));
        }
        let body_bytes = extra_words.saturating_mul(4);
        if body_bytes < 72 || body_bytes > MAX_REPLY_BYTES {
            return Err(X11Error::Unsupported("short X11 setup reply".to_owned()));
        }
        let mut body = vec![0u8; body_bytes];
        stream.read_exact(&mut body)?;
        let setup = parse_setup_body(&body)?;
        stream.set_nonblocking(true)?;

        let next_id = setup.resource_id_base.wrapping_add(1);
        let mut conn = Self {
            stream,
            setup,
            seq: 1,
            next_id,
            window: 0,
            gc: 0,
            width: width.max(320),
            height: height.max(240),
            wm_protocols: 0,
            wm_delete_window: 0,
            net_wm_name: 0,
            utf8_string: 0,
            clipboard_atom: 0,
            targets_atom: 0,
            sel_prop_atom: 0,
            keymap: KeyboardMap::new(8, 0, Vec::new()),
            rbuf: Vec::with_capacity(8_192),
            rstart: 0,
            events: VecDeque::with_capacity(64),
            left_button_down: false,
            clipboard_owned: Zeroizing::new(Vec::new()),
            staging_bytes: Vec::new(),
        };

        conn.wm_protocols = conn.intern_atom("WM_PROTOCOLS", false)?;
        conn.wm_delete_window = conn.intern_atom("WM_DELETE_WINDOW", false)?;
        conn.net_wm_name = conn.intern_atom("_NET_WM_NAME", false)?;
        conn.utf8_string = conn.intern_atom("UTF8_STRING", false)?;
        conn.clipboard_atom = conn.intern_atom("CLIPBOARD", false)?;
        conn.targets_atom = conn.intern_atom("TARGETS", false)?;
        conn.sel_prop_atom = conn.intern_atom("HOLONOMY_SELECTION", false)?;

        conn.keymap = conn.fetch_keyboard_mapping()?;

        let win = conn.alloc_id()?;
        let gc_id = conn.alloc_id()?;
        conn.window = win;
        conn.gc = gc_id;

        let ev_mask = mask::KEY_PRESS
            | mask::BUTTON_PRESS
            | mask::BUTTON_RELEASE
            | mask::POINTER_MOTION
            | mask::EXPOSURE
            | mask::STRUCTURE_NOTIFY;
        let create = Req::new(op::CREATE_WINDOW)
            .second_byte(conn.setup.root_depth)
            .u32(win)
            .u32(conn.setup.root)
            .i16(0)
            .i16(0)
            .u16(conn.width)
            .u16(conn.height)
            .u16(0)
            .u16(1) // InputOutput
            .u32(conn.setup.root_visual)
            .u32(cw::BACK_PIXEL | cw::BORDER_PIXEL | cw::EVENT_MASK)
            .u32(0x00FF_FFFF)
            .u32(0x00FF_FFFF)
            .u32(ev_mask);
        conn.send_req(create)?;

        let create_gc = Req::new(op::CREATE_GC)
            .u32(gc_id)
            .u32(win)
            .u32(gc::FOREGROUND | gc::BACKGROUND | gc::GRAPHICS_EXPOSURES)
            .u32(0x001F_2225)
            .u32(0x00FF_FFFF)
            .u32(0);
        conn.send_req(create_gc)?;

        conn.set_property_bytes(win, atom::WM_NAME, atom::STRING, 8, title.as_bytes())?;
        conn.set_property_bytes(win, conn.net_wm_name, conn.utf8_string, 8, title.as_bytes())?;
        let hints = size_hints_bytes(conn.width, conn.height, 480, 360);
        conn.set_property_bytes(win, atom::WM_NORMAL_HINTS, atom::WM_SIZE_HINTS, 32, &hints)?;
        conn.set_property_bytes(
            win,
            conn.wm_protocols,
            atom::ATOM,
            32,
            &conn.wm_delete_window.to_le_bytes(),
        )?;

        conn.send_req(Req::new(op::MAP_WINDOW).u32(win))?;
        conn.sync()?;
        Ok(conn)
    }

    /// Current window width and height in pixels.
    #[must_use]
    pub const fn size(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    /// Monitor DPI estimate from the X screen metadata (`96` fallback).
    #[must_use]
    pub const fn dpi(&self) -> i64 {
        self.setup.dpi
    }

    /// Presents a row-major `0x00RRGGBB` pixel framebuffer (`pixels.len() >= width * height`) to the
    /// X11 window via scanline-chunked `PutImage`.
    ///
    /// # Errors
    ///
    /// Returns [`X11Error`] if dimensions exceed server limits or writing fails.
    pub fn present_argb(
        &mut self,
        pixels: &[u32],
        width: u16,
        height: u16,
    ) -> Result<(), X11Error> {
        let w = usize::from(width);
        let h = usize::from(height);
        if w == 0 || h == 0 || pixels.len() < w.saturating_mul(h) {
            return Ok(());
        }
        let max_req = (self.setup.max_request_words as usize)
            .saturating_mul(4)
            .min(MAX_REQUEST_BYTES);
        let header_bytes = 24usize;
        let row_bytes = w
            .checked_mul(4)
            .ok_or_else(|| X11Error::Protocol("framebuffer row byte length overflow".to_owned()))?;
        if header_bytes + row_bytes > max_req {
            return Err(X11Error::Protocol(format!(
                "single scanline ({row_bytes} B) exceeds X11 request ceiling ({max_req} B)"
            )));
        }
        let rows_per_chunk = ((max_req - header_bytes) / row_bytes).max(1);

        let mut y = 0usize;
        while y < h {
            let chunk_h = rows_per_chunk.min(h - y);
            let slice = &pixels[y * w..(y + chunk_h) * w];
            self.staging_bytes.clear();
            self.staging_bytes.reserve(slice.len() * 4);
            for &px in slice {
                self.staging_bytes.extend_from_slice(&px.to_le_bytes());
            }
            let pkt = Req::new(op::PUT_IMAGE)
                .second_byte(2) // ZPixmap
                .u32(self.window)
                .u32(self.gc)
                .u16(width)
                .u16(chunk_h as u16)
                .i16(0)
                .i16(y as i16)
                .u8(0) // left-pad
                .u8(self.setup.root_depth)
                .pad(2)
                .bytes(&self.staging_bytes);
            self.send_req(pkt)?;
            y += chunk_h;
        }
        self.stream.flush()?;
        Ok(())
    }

    /// Advertises UTF-8 selection ownership on `CLIPBOARD` and stores `utf8` locally (wiped on
    /// clear or drop).
    ///
    /// # Errors
    ///
    /// Returns [`X11Error`] on socket failure.
    pub fn set_clipboard(&mut self, utf8: &[u8]) -> Result<(), X11Error> {
        self.clipboard_owned.zeroize();
        self.clipboard_owned.extend_from_slice(utf8);
        self.send_req(
            Req::new(op::SET_SELECTION_OWNER)
                .u32(self.window)
                .u32(self.clipboard_atom)
                .u32(0), // CurrentTime
        )?;
        Ok(())
    }

    /// Requests UTF-8 contents from the current `CLIPBOARD` owner (`DesktopEvent::ClipboardPasted`
    /// arrives asynchronously once the owner responds).
    ///
    /// # Errors
    ///
    /// Returns [`X11Error`] on socket failure.
    pub fn request_clipboard(&mut self) -> Result<(), X11Error> {
        self.send_req(
            Req::new(op::CONVERT_SELECTION)
                .u32(self.window)
                .u32(self.clipboard_atom)
                .u32(self.utf8_string)
                .u32(self.sel_prop_atom)
                .u32(0), // CurrentTime
        )?;
        Ok(())
    }

    /// Waits up to `timeout_ms` for server events, drains available packets non-blockingly, and
    /// returns coalesced high-level [`DesktopEvent`] items.
    ///
    /// # Errors
    ///
    /// Returns [`X11Error`] if the X server closes or the socket errors.
    pub fn poll_events(&mut self, timeout_ms: i32) -> Result<Vec<DesktopEvent>, X11Error> {
        if self.events.is_empty() {
            let mut pfd = libc::pollfd {
                fd: self.stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `pfd` points to one valid `pollfd` over the live socket fd.
            let rc = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
            if rc < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() != std::io::ErrorKind::Interrupted {
                    return Err(X11Error::Io(err));
                }
            }
            if rc > 0 {
                self.drain_incoming()?;
            }
        } else {
            self.drain_incoming()?;
        }

        let mut out = Vec::new();
        let mut pending_drag: Option<(i16, i16)> = None;
        let mut pending_resize: Option<(u16, u16)> = None;
        let mut expose_needed = false;

        while let Some(ev) = self.events.pop_front() {
            match ev {
                WireEvent::KeyPress { keycode, state } => {
                    if let Some((key, modifiers)) = self.keymap.translate(keycode, state) {
                        out.push(DesktopEvent::Key { key, modifiers });
                    }
                }
                WireEvent::KeyRelease { .. } | WireEvent::MapNotify | WireEvent::Other(_) => {}
                WireEvent::ButtonPress {
                    button,
                    x,
                    y,
                    state,
                } => match button {
                    1 => {
                        self.left_button_down = true;
                        out.push(DesktopEvent::MouseDown {
                            x,
                            y,
                            modifiers: Modifiers {
                                shift: (state & 0x01) != 0,
                                control: (state & 0x04) != 0,
                                alt: (state & 0x08) != 0,
                            },
                        });
                    }
                    4 => out.push(DesktopEvent::Scroll { delta_lines: -3 }),
                    5 => out.push(DesktopEvent::Scroll { delta_lines: 3 }),
                    _ => {}
                },
                WireEvent::ButtonRelease { button, .. } => {
                    if button == 1 {
                        self.left_button_down = false;
                        out.push(DesktopEvent::MouseUp);
                    }
                }
                WireEvent::MotionNotify { x, y } => {
                    if self.left_button_down {
                        pending_drag = Some((x, y));
                    }
                }
                WireEvent::Expose { count, .. } => {
                    if count == 0 {
                        expose_needed = true;
                    }
                }
                WireEvent::ConfigureNotify { width, height } => {
                    if width > 0 && height > 0 {
                        pending_resize = Some((width, height));
                    }
                }
                WireEvent::ClientMessage { type_atom, data0 } => {
                    if type_atom == self.wm_protocols && data0 == self.wm_delete_window {
                        out.push(DesktopEvent::CloseRequested);
                    }
                }
                WireEvent::SelectionClear { selection } => {
                    if selection == self.clipboard_atom {
                        self.clipboard_owned.zeroize();
                        self.clipboard_owned.clear();
                    }
                }
                WireEvent::SelectionRequest {
                    time,
                    requestor,
                    selection,
                    target,
                    property,
                } => {
                    self.answer_selection_request(time, requestor, selection, target, property)?;
                }
                WireEvent::SelectionNotify {
                    selection,
                    property,
                    ..
                } => {
                    if selection == self.clipboard_atom && property != 0 {
                        if let Some(bytes) = self.read_selection_property(property)? {
                            out.push(DesktopEvent::ClipboardPasted(bytes));
                        }
                    }
                }
            }
        }

        if let Some((x, y)) = pending_drag {
            out.push(DesktopEvent::MouseDrag { x, y });
        }
        if let Some((w, h)) = pending_resize {
            if (w, h) != (self.width, self.height) {
                self.width = w;
                self.height = h;
                out.push(DesktopEvent::Resize {
                    width: w,
                    height: h,
                });
            }
        }
        if expose_needed {
            out.push(DesktopEvent::Expose);
        }
        Ok(out)
    }

    fn alloc_id(&mut self) -> Result<u32, X11Error> {
        let id = self.next_id;
        let offset = (id & self.setup.resource_id_mask)
            .checked_add(1)
            .filter(|&v| (v & !self.setup.resource_id_mask) == 0)
            .ok_or_else(|| X11Error::Protocol("X11 resource IDs exhausted".to_owned()))?;
        self.next_id = self.setup.resource_id_base | offset;
        Ok(id)
    }

    fn next_seq(&mut self) -> u16 {
        let s = self.seq;
        self.seq = self.seq.wrapping_add(1);
        s
    }

    fn send_req(&mut self, req: Req) -> Result<u16, X11Error> {
        let seq = self.next_seq();
        let bytes = req.finish(seq);
        if bytes.len() > MAX_REQUEST_BYTES {
            return Err(X11Error::Protocol(format!(
                "request size {} exceeds {MAX_REQUEST_BYTES}",
                bytes.len()
            )));
        }
        self.stream.write_all(&bytes)?;
        Ok(seq)
    }

    fn intern_atom(&mut self, name: &str, only_if_exists: bool) -> Result<u32, X11Error> {
        let seq = self.send_req(
            Req::new(op::INTERN_ATOM)
                .second_byte(u8::from(only_if_exists))
                .u16(name.len() as u16)
                .pad(2)
                .bytes(name.as_bytes()),
        )?;
        let body = self.wait_for_reply(seq)?;
        if body.len() < 4 {
            return Err(X11Error::Protocol("short InternAtom reply".to_owned()));
        }
        Ok(u32::from_le_bytes([body[0], body[1], body[2], body[3]]))
    }

    fn sync(&mut self) -> Result<(), X11Error> {
        let seq = self.send_req(Req::new(op::GET_INPUT_FOCUS))?;
        let _ = self.wait_for_reply(seq)?;
        Ok(())
    }

    fn set_property_bytes(
        &mut self,
        window: u32,
        property: u32,
        prop_type: u32,
        format: u8,
        data: &[u8],
    ) -> Result<(), X11Error> {
        let unit = match format {
            8 => 1,
            16 => 2,
            32 => 4,
            _ => return Err(X11Error::Protocol("invalid X11 property format".to_owned())),
        };
        let count = (data.len() / unit) as u32;
        self.send_req(
            Req::new(op::CHANGE_PROPERTY)
                .second_byte(0) // Replace
                .u32(window)
                .u32(property)
                .u32(prop_type)
                .u8(format)
                .pad(3)
                .u32(count)
                .bytes(data),
        )?;
        Ok(())
    }

    fn fetch_keyboard_mapping(&mut self) -> Result<KeyboardMap, X11Error> {
        let min = self.setup.min_keycode;
        let max = self.setup.max_keycode;
        let count = max.saturating_sub(min).saturating_add(1);
        let req_seq = self.send_req(Req::new(op::GET_KEYBOARD_MAPPING).u8(min).u8(count).pad(2))?;
        let (second_byte, body) = self.wait_for_reply_full(req_seq)?;
        let syms_per = usize::from(second_byte);
        let mut table = Vec::with_capacity(body.len().saturating_sub(24) / 4);
        if body.len() >= 24 {
            for chunk in body[24..].chunks_exact(4) {
                table.push(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
            }
        }
        Ok(KeyboardMap::new(min, syms_per, table))
    }

    fn answer_selection_request(
        &mut self,
        time: u32,
        requestor: u32,
        selection: u32,
        target: u32,
        property: u32,
    ) -> Result<(), X11Error> {
        let dest_prop = if property == 0 { target } else { property };
        let reply_prop = if selection != self.clipboard_atom || self.clipboard_owned.is_empty() {
            0
        } else if target == self.targets_atom {
            let targets = [self.targets_atom, self.utf8_string, atom::STRING];
            let mut raw = Vec::with_capacity(12);
            for t in targets {
                raw.extend_from_slice(&t.to_le_bytes());
            }
            self.set_property_bytes(requestor, dest_prop, atom::ATOM, 32, &raw)?;
            dest_prop
        } else if target == self.utf8_string || target == atom::STRING {
            let owned = self.clipboard_owned.to_vec();
            self.set_property_bytes(requestor, dest_prop, target, 8, &owned)?;
            dest_prop
        } else {
            0
        };

        let mut ev = [0u8; 32];
        ev[0] = event_code::SELECTION_NOTIFY;
        ev[4..8].copy_from_slice(&time.to_le_bytes());
        ev[8..12].copy_from_slice(&requestor.to_le_bytes());
        ev[12..16].copy_from_slice(&selection.to_le_bytes());
        ev[16..20].copy_from_slice(&target.to_le_bytes());
        ev[20..24].copy_from_slice(&reply_prop.to_le_bytes());
        self.send_req(
            Req::new(op::SEND_EVENT)
                .second_byte(0)
                .u32(requestor)
                .u32(0)
                .bytes(&ev),
        )?;
        Ok(())
    }

    fn read_selection_property(
        &mut self,
        property: u32,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, X11Error> {
        let seq = self.send_req(
            Req::new(op::GET_PROPERTY)
                .second_byte(1) // delete = true
                .u32(self.window)
                .u32(property)
                .u32(0) // AnyPropertyType
                .u32(0)
                .u32((MAX_CLIPBOARD_BYTES / 4) as u32),
        )?;
        let (format, body) = self.wait_for_reply_full(seq)?;
        if format != 8 || body.len() < 24 {
            return Ok(None);
        }
        let n_items = u32::from_le_bytes([body[8], body[9], body[10], body[11]]) as usize;
        let avail = body.len().saturating_sub(24);
        let take = n_items.min(avail).min(MAX_CLIPBOARD_BYTES);
        if take == 0 {
            return Ok(None);
        }
        let mut out = Zeroizing::new(Vec::with_capacity(take));
        out.extend_from_slice(&body[24..24 + take]);
        Ok(Some(out))
    }

    fn drain_incoming(&mut self) -> Result<(), X11Error> {
        let mut tmp = [0u8; 4_096];
        loop {
            match self.stream.read(&mut tmp) {
                Ok(0) => {
                    return Err(X11Error::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "X server closed display connection",
                    )));
                }
                Ok(n) => {
                    self.rbuf.extend_from_slice(&tmp[..n]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(X11Error::Io(e)),
            }
        }
        self.parse_buffered_packets()
    }

    fn parse_buffered_packets(&mut self) -> Result<(), X11Error> {
        loop {
            let unread = &self.rbuf[self.rstart..];
            if unread.len() < EVENT_BYTES {
                break;
            }
            match unread[0] {
                1 => {
                    // Replies are waited for synchronously; queue wait handles them.
                    break;
                }
                0 => {
                    let pkt: [u8; EVENT_BYTES] = unread[..EVENT_BYTES].try_into().unwrap();
                    self.rstart += EVENT_BYTES;
                    let code = pkt[1];
                    let major = pkt[10];
                    if major != op::GET_PROPERTY && major != op::INTERN_ATOM {
                        return Err(X11Error::Protocol(format!(
                            "X11 error code {code} on opcode {major}"
                        )));
                    }
                }
                _ => {
                    let pkt: [u8; EVENT_BYTES] = unread[..EVENT_BYTES].try_into().unwrap();
                    self.rstart += EVENT_BYTES;
                    self.events.push_back(WireEvent::decode(&pkt));
                }
            }
        }
        if self.rstart > 4_096 {
            self.rbuf.drain(..self.rstart);
            self.rstart = 0;
        }
        Ok(())
    }

    fn wait_for_reply(&mut self, target_seq: u16) -> Result<Vec<u8>, X11Error> {
        let (_, body) = self.wait_for_reply_full(target_seq)?;
        Ok(body)
    }

    fn wait_for_reply_full(&mut self, target_seq: u16) -> Result<(u8, Vec<u8>), X11Error> {
        let deadline = Instant::now() + SETUP_TIMEOUT;
        loop {
            while self.rbuf.len().saturating_sub(self.rstart) >= EVENT_BYTES {
                let unread = &self.rbuf[self.rstart..];
                if unread[0] == 1 {
                    let second_byte = unread[1];
                    let seq = u16::from_le_bytes([unread[2], unread[3]]);
                    let extra_words =
                        u32::from_le_bytes([unread[4], unread[5], unread[6], unread[7]]) as usize;
                    let total = EVENT_BYTES
                        .checked_add(extra_words.saturating_mul(4))
                        .filter(|&n| n <= MAX_REPLY_BYTES)
                        .ok_or_else(|| X11Error::Protocol("X11 reply too large".to_owned()))?;
                    if unread.len() < total {
                        break;
                    }
                    let body = unread[8..total].to_vec();
                    self.rstart += total;
                    if self.rstart > 4_096 {
                        self.rbuf.drain(..self.rstart);
                        self.rstart = 0;
                    }
                    if seq == target_seq {
                        return Ok((second_byte, body));
                    }
                } else if unread[0] == 0 {
                    let pkt: [u8; EVENT_BYTES] = unread[..EVENT_BYTES].try_into().unwrap();
                    self.rstart += EVENT_BYTES;
                    let seq = u16::from_le_bytes([pkt[2], pkt[3]]);
                    if seq == target_seq {
                        return Err(X11Error::Protocol(format!(
                            "X11 error {} on opcode {}",
                            pkt[1], pkt[10]
                        )));
                    }
                } else {
                    let pkt: [u8; EVENT_BYTES] = unread[..EVENT_BYTES].try_into().unwrap();
                    self.rstart += EVENT_BYTES;
                    self.events.push_back(WireEvent::decode(&pkt));
                }
            }

            let now = Instant::now();
            if now >= deadline {
                return Err(X11Error::Protocol(
                    "timed out waiting for X11 reply".to_owned(),
                ));
            }
            let rem_ms = deadline.duration_since(now).as_millis().min(500) as i32;
            let mut pfd = libc::pollfd {
                fd: self.stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: `pfd` references valid stack memory for a live fd.
            let rc = unsafe { libc::poll(&mut pfd, 1, rem_ms) };
            if rc > 0 {
                let mut tmp = [0u8; 4_096];
                match self.stream.read(&mut tmp) {
                    Ok(0) => {
                        return Err(X11Error::Io(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "X11 socket closed while waiting for reply",
                        )));
                    }
                    Ok(n) => self.rbuf.extend_from_slice(&tmp[..n]),
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(e) => return Err(X11Error::Io(e)),
                }
            }
        }
    }
}

impl Drop for X11Window {
    fn drop(&mut self) {
        self.clipboard_owned.zeroize();
        self.staging_bytes.zeroize();
        let _ = self.send_req(Req::new(op::FREE_GC).u32(self.gc));
        let _ = self.send_req(Req::new(op::DESTROY_WINDOW).u32(self.window));
        let _ = self.stream.flush();
    }
}

fn connect_x11_socket(path: &str) -> Result<UnixStream, X11Error> {
    if let Ok(stream) = UnixStream::connect(path) {
        return Ok(stream);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::SocketAddr;
        if let Ok(addr) = SocketAddr::from_abstract_name(path.as_bytes()) {
            if let Ok(stream) = UnixStream::connect_addr(&addr) {
                return Ok(stream);
            }
        }
    }
    Err(X11Error::BadDisplay(format!(
        "cannot connect to X11 display socket {path}"
    )))
}

fn parse_setup_body(body: &[u8]) -> Result<SetupInfo, X11Error> {
    if body.len() < 72 {
        return Err(X11Error::Unsupported("short setup body".to_owned()));
    }
    let mut r = Rdr::new(body);
    let _release = r.u32();
    let resource_id_base = r.u32();
    let resource_id_mask = r.u32();
    let _motion_buf = r.u32();
    let vendor_len = usize::from(r.u16());
    let max_request_words = u32::from(r.u16());
    let _screens = r.u8();
    let nformats = usize::from(r.u8());
    let image_byte_order = r.u8();
    r.skip(3);
    let min_keycode = r.u8();
    let max_keycode = r.u8();
    r.skip(4);
    if image_byte_order != 0 {
        return Err(X11Error::Unsupported(
            "X server is MSBFirst; little-endian X11 server required".to_owned(),
        ));
    }
    let vendor_padded = (vendor_len + 3) & !3;
    let needed = 32 + vendor_padded + nformats * 8 + 40;
    if body.len() < needed {
        return Err(X11Error::Unsupported(
            "truncated X11 setup formats/screen".to_owned(),
        ));
    }
    r.skip(vendor_padded);
    let mut has_root_32 = false;
    let mut formats = Vec::with_capacity(nformats);
    for _ in 0..nformats {
        let depth = r.u8();
        let bpp = r.u8();
        let pad = r.u8();
        r.skip(5);
        formats.push((depth, bpp, pad));
    }
    let root = r.u32();
    let _cmap = r.u32();
    let white_pixel = r.u32();
    let _black_pixel = r.u32();
    let _input_masks = r.u32();
    let root_width = r.u16();
    let root_height = r.u16();
    let mm_width = r.u16();
    let _mm_height = r.u16();
    let _min_maps = r.u16();
    let _max_maps = r.u16();
    let root_visual = r.u32();
    let _backing = r.u8();
    let _save_unders = r.u8();
    let root_depth = r.u8();
    let _ndepths = r.u8();

    for (d, bpp, _) in formats {
        if d == root_depth && bpp == 32 {
            has_root_32 = true;
        }
    }
    if !has_root_32 {
        return Err(X11Error::Unsupported(format!(
            "X11 root depth {root_depth} lacks a 32-bpp pixmap format"
        )));
    }
    let dpi = if mm_width > 0 && root_width > 0 {
        ((i64::from(root_width) * 254) / (i64::from(mm_width) * 10)).clamp(72, 288)
    } else {
        96
    };
    Ok(SetupInfo {
        resource_id_base,
        resource_id_mask,
        max_request_words: max_request_words.max(16_384),
        root,
        root_depth,
        root_visual,
        white_pixel,
        root_width,
        root_height,
        min_keycode,
        max_keycode,
        dpi,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_display_numbers_strictly() {
        assert_eq!(display_number(":0"), Some(0));
        assert_eq!(display_number(":12.0"), Some(12));
        assert_eq!(display_number("unix:3"), Some(3));
        assert_eq!(display_number("localhost:10.0"), Some(10));
        assert_eq!(display_number("remote.box:0"), None);
        assert_eq!(display_number(""), None);
    }

    #[test]
    fn parses_binary_xauthority_and_selects_cookie() {
        let mut blob = Vec::new();
        let push_field = |out: &mut Vec<u8>, s: &[u8]| {
            out.extend_from_slice(&(s.len() as u16).to_be_bytes());
            out.extend_from_slice(s);
        };
        blob.extend_from_slice(&FAMILY_LOCAL.to_be_bytes());
        push_field(&mut blob, b"workstation");
        push_field(&mut blob, b"");
        push_field(&mut blob, COOKIE_NAME.as_bytes());
        let cookie = [0xABu8; COOKIE_BYTES];
        push_field(&mut blob, &cookie);

        let selected = select_xauth_cookie(&blob, 0).expect("selects local cookie");
        assert_eq!(selected, cookie);
    }

    #[test]
    fn decodes_wire_events_at_exact_protocol_offsets() {
        // ButtonPress: button=1, x=412, y=195, state=Shift(1)
        let mut pkt = [0u8; EVENT_BYTES];
        pkt[0] = event_code::BUTTON_PRESS;
        pkt[1] = 1;
        pkt[24..26].copy_from_slice(&412i16.to_le_bytes());
        pkt[26..28].copy_from_slice(&195i16.to_le_bytes());
        pkt[28..30].copy_from_slice(&1u16.to_le_bytes());
        assert_eq!(
            WireEvent::decode(&pkt),
            WireEvent::ButtonPress {
                button: 1,
                x: 412,
                y: 195,
                state: 1
            }
        );

        // Expose: x=10, y=20, w=800, h=600, count=0
        let mut exp = [0u8; EVENT_BYTES];
        exp[0] = event_code::EXPOSE;
        exp[8..10].copy_from_slice(&10u16.to_le_bytes());
        exp[10..12].copy_from_slice(&20u16.to_le_bytes());
        exp[12..14].copy_from_slice(&800u16.to_le_bytes());
        exp[14..16].copy_from_slice(&600u16.to_le_bytes());
        assert_eq!(
            WireEvent::decode(&exp),
            WireEvent::Expose {
                x: 10,
                y: 20,
                width: 800,
                height: 600,
                count: 0
            }
        );
    }

    #[test]
    fn translates_keyboard_mapping_with_shift_capslock_and_ctrl() {
        let mut table = vec![0u32; 4 * 2];
        // keycode 8: 'a' / 'A'
        table[0] = u32::from(b'a');
        table[1] = u32::from(b'A');
        // keycode 9: BackSpace
        table[2] = 0xff08;
        let map = KeyboardMap::new(8, 2, table);

        assert_eq!(
            map.translate(8, 0),
            Some((Key::Character('a'), Modifiers::default()))
        );
        assert_eq!(
            map.translate(8, 0x01),
            Some((
                Key::Character('A'),
                Modifiers {
                    shift: true,
                    ..Modifiers::default()
                }
            ))
        );
        // Ctrl+Shift+A normalizes character cleanly with control=true so bindings match.
        assert_eq!(
            map.translate(8, 0x05),
            Some((
                Key::Character('a'),
                Modifiers {
                    shift: true,
                    control: true,
                    alt: false,
                }
            ))
        );
        assert_eq!(
            map.translate(9, 0),
            Some((Key::Backspace, Modifiers::default()))
        );
    }
}
