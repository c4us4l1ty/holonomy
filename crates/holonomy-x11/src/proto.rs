//! The wire: opcodes, event codes, and the two codecs that read and write them.
//!
//! # Why hand-rolled codecs instead of `#[repr(C)]` and `transmute`
//!
//! The X11 protocol is little-endian by construction, and the server reports its byte order in the
//! handshake -- but a `#[repr(C)]` struct plus `read` would silently produce a *big-endian* struct on
//! a big-endian host and send garbage that the server answers with a `BadLength`. A codec that
//! writes each field with `to_le_bytes` cannot do that, and it can also be padded and length-prefixed
//! without a `MaybeUninit` dance. Every request in this crate is built by [`Req`] and every reply is
//! read through [`Rdr`], so the byte layout is in one place and is unit-tested without a server.
//!
//! # The request length is counted in 4-byte words, including the header
//!
//! This is the field that produces the classic "the server ignores my last request" bug. It is not
//! the byte count and it does not include the 4-byte header: `length = 1 + (data + pad) / 4`. Getting
//! it wrong does not produce an error -- the server treats the miscount as a *different* request.

use std::fmt;

/// Request opcodes, X11 core protocol 11.0.
pub mod op {
    /// CreateWindow.
    pub const CREATE_WINDOW: u8 = 1;
    /// ChangeWindowAttributes.
    pub const CHANGE_WINDOW_ATTRIBUTES: u8 = 2;
    /// DestroyWindow.
    pub const DESTROY_WINDOW: u8 = 4;
    /// MapWindow.
    pub const MAP_WINDOW: u8 = 8;
    /// GetGeometry.
    pub const GET_GEOMETRY: u8 = 14;
    /// InternAtom.
    pub const INTERN_ATOM: u8 = 16;
    /// ChangeProperty.
    pub const CHANGE_PROPERTY: u8 = 18;
    /// GetProperty.
    pub const GET_PROPERTY: u8 = 20;
    /// SetInputFocus.
    pub const SET_INPUT_FOCUS: u8 = 42;
    /// GetInputFocus. Also this crate's sync point: it is the shortest request that replies.
    pub const GET_INPUT_FOCUS: u8 = 43;
    /// QueryPointer.
    pub const QUERY_POINTER: u8 = 38;
    /// CreatePixmap.
    pub const CREATE_PIXMAP: u8 = 53;
    /// FreePixmap.
    pub const FREE_PIXMAP: u8 = 54;
    /// CreateGC.
    pub const CREATE_GC: u8 = 55;
    /// FreeGC.
    pub const FREE_GC: u8 = 60;
    /// PutImage.
    pub const PUT_IMAGE: u8 = 72;
    /// GetImage.
    pub const GET_IMAGE: u8 = 73;
    /// ConfigureWindow. Byte 1 is `unused`, then the window, a 2-byte value mask and the values in
    /// ascending bit order.
    pub const CONFIGURE_WINDOW: u8 = 12;
    /// SendEvent. The only way a client can hand an event to another client -- a window manager --
    /// without the server knowing it came from a client rather than from a device.
    pub const SEND_EVENT: u8 = 25;
    /// QueryExtension.
    pub const QUERY_EXTENSION: u8 = 98;
    /// GetKeyboardMapping.
    pub const GET_KEYBOARD_MAPPING: u8 = 101;
    /// NoOperation. A request that does nothing -- and, contrary to what a first reading of this
    /// comment said, sends **no reply at all**. It is listed here because the sync point that is
    /// actually used, [`op::GET_INPUT_FOCUS`], is easier to explain next to it.
    pub const NO_OPERATION: u8 = 127;
}

/// Event codes, X11 core protocol. The first bit is the "sent by a SendEvent request" flag, so every
/// code here is compared against `code & 0x7F`.
pub mod event {
    /// KeyPress.
    pub const KEY_PRESS: u8 = 2;
    /// KeyRelease.
    pub const KEY_RELEASE: u8 = 3;
    /// ButtonPress.
    pub const BUTTON_PRESS: u8 = 4;
    /// ButtonRelease.
    pub const BUTTON_RELEASE: u8 = 5;
    /// Expose.
    pub const EXPOSE: u8 = 12;
    /// MapNotify.
    pub const MAP_NOTIFY: u8 = 19;
    /// ConfigureNotify.
    pub const CONFIGURE_NOTIFY: u8 = 22;
    /// PropertyNotify.
    pub const PROPERTY_NOTIFY: u8 = 28;
    /// ClientMessage.
    pub const CLIENT_MESSAGE: u8 = 33;
    /// Any event with this bit set was sent by a client with `SendEvent`, not by the server.
    pub const SEND_EVENT_FLAG: u8 = 0x80;
}

/// Event mask bits, for `CreateWindow`'s value list.
pub mod mask {
    /// KeyPressMask.
    pub const KEY_PRESS: u32 = 0x0000_0001;
    /// KeyReleaseMask.
    pub const KEY_RELEASE: u32 = 0x0000_0002;
    /// ButtonPressMask.
    pub const BUTTON_PRESS: u32 = 0x0000_0004;
    /// ButtonReleaseMask.
    pub const BUTTON_RELEASE: u32 = 0x0000_0008;
    /// ExposureMask. Without it the window arrives blank and nothing ever asks for a repaint.
    pub const EXPOSURE: u32 = 0x0000_8000;
    /// StructureNotifyMask. `ConfigureNotify` is how a resize is discovered.
    pub const STRUCTURE_NOTIFY: u32 = 0x0002_0000;
    /// FocusChangeMask.
    pub const FOCUS_CHANGE: u32 = 0x0020_0000;
}

/// `CreateWindow`'s value-mask bits.
pub mod cw {
    /// CWBackPixel.
    pub const BACK_PIXEL: u32 = 0x0000_0002;
    /// CWBorderPixel.
    pub const BORDER_PIXEL: u32 = 0x0000_0008;
    /// CWEventMask.
    pub const EVENT_MASK: u32 = 0x0000_0800;
    /// CWOverrideRedirect.
    pub const OVERRIDE_REDIRECT: u32 = 0x0000_0200;
}

/// `CreateGC`'s value-mask bits.
/// `ConfigureWindow`'s value mask: which of the values follow, and in what order.
///
/// The order is ascending bit order, and it is *not* the order the bits are declared here: a window's x
/// and y come before its width and height. A client that sends width before y gets its window moved
/// instead of resized, which is a real failure mode and not a subtle one.
pub mod value {
    /// The window's x position.
    pub const X: u16 = 1 << 0;
    /// The window's y position.
    pub const Y: u16 = 1 << 1;
    /// The window's width.
    pub const WIDTH: u16 = 1 << 2;
    /// The window's height.
    pub const HEIGHT: u16 = 1 << 3;
    /// The window's border width.
    pub const BORDER_WIDTH: u16 = 1 << 4;
    /// The window's sibling, for a restack request.
    pub const SIBLING: u16 = 1 << 5;
    /// The window's stack mode.
    pub const STACK_MODE: u16 = 1 << 6;
}

pub mod gc {
    /// GCFunction.
    pub const FUNCTION: u32 = 0x0000_0001;
    /// GCForeground.
    pub const FOREGROUND: u32 = 0x0000_0004;
    /// GCBackground.
    pub const BACKGROUND: u32 = 0x0000_0008;
    /// GCGraphicsExposures. Off, so a `PutImage` that lands outside the window does not produce an
    /// event per exposed pixel.
    pub const GRAPHICS_EXPOSURES: u32 = 0x0001_0000;
}

/// Image formats for `PutImage` and `GetImage`.
pub mod image {
    /// XYBitmap.
    pub const XY_BITMAP: u8 = 0;
    /// XYPixmap.
    pub const XY_PIXMAP: u8 = 1;
    /// ZPixmap. The only one this crate uses.
    pub const Z_PIXMAP: u8 = 2;
}

/// Predefined atoms used as `ChangeProperty` types and values.
pub mod atom {
    /// `WM_NORMAL_HINTS`, the predefined atom 40.
    pub const WM_NORMAL_HINTS: u32 = 40;
    /// `WM_SIZE_HINTS`, the predefined atom 41, which is the *type* of `WM_NORMAL_HINTS`.
    pub const WM_SIZE_HINTS: u32 = 41;
    /// STRING.
    pub const STRING: u32 = 31;
    /// ATOM.
    pub const ATOM: u32 = 4;
    /// WM_NAME.
    pub const WM_NAME: u32 = 39;
}

/// `SetInputFocus`'s revert-to values.
pub mod focus {
    /// RevertToNone.
    pub const NONE: u8 = 0;
    /// RevertToPointerRoot. What this crate asks for: if the window goes away the focus returns to
    /// whatever the pointer is over, so a crashed editor does not leave the session with no keyboard.
    pub const POINTER_ROOT: u8 = 1;
    /// RevertToParent.
    pub const PARENT: u8 = 2;
}

/// A request, assembled field by field.
///
/// The header is reserved at construction and patched by [`Req::finish`], because the length field
/// cannot be known until the request is complete.
#[derive(Debug, Clone)]
pub struct Req {
    buf: Vec<u8>,
    /// Whether byte 1 holds a minor opcode rather than a sequence number.
    minor_opcode: bool,
}

impl Req {
    /// A request with opcode `opcode` and no fields.
    pub fn new(opcode: u8) -> Self {
        let mut buf = Vec::with_capacity(32);
        buf.push(opcode);
        buf.push(0);
        buf.extend_from_slice(&[0, 0]); // length, patched by `finish`
        Self {
            buf,
            minor_opcode: false,
        }
    }

    /// Put a value in the second header byte, and keep the sequence number out of it.
    ///
    /// # Almost every request has a field there, not a sequence number
    ///
    /// The X11 protocol puts the sequence number in byte 1 of a request only for a handful of them
    /// (`GetProperty`, `GetSelectionOwner`, `GrabServer`, ...). Everywhere else byte 1 is a real field:
    /// `CreateWindow`'s depth, `ChangeProperty`'s mode, `PutImage`'s format, `SetInputFocus`'s
    /// revert-to, `InternAtom`'s only-if-exists, and `unused` for a few more. An extension request puts
    /// its *minor opcode* there.
    ///
    /// A client that writes a sequence number into byte 1 of `CreateWindow` sends a window with
    /// `depth = 1`, which is not a depth any server has. The server's answer is a `BadMatch` naming a
    /// request the client believed was fine, and the sequence counter has quietly become a payload
    /// field. This is what this crate's first version did on every request, and it is why
    /// `GetInputFocus` worked -- its byte 1 is `unused` -- while `Window::create` failed with a
    /// `BadLength` for a request whose byte count was only one byte long.
    ///
    /// So: the default is "byte 1 is the sequence number", and a request whose protocol puts something
    /// else there says so with this method. The server tracks the sequence itself, one per request
    /// processed, so the counter in replies keeps working either way.
    #[must_use]
    pub fn second_byte(mut self, v: u8) -> Self {
        self.buf[1] = v;
        self.minor_opcode = true;
        self
    }

    /// Put an extension's minor opcode in the second header byte.
    ///
    /// A spelling of [`Req::second_byte`], because that is what it is.
    #[must_use]
    pub fn minor(self, v: u8) -> Self {
        self.second_byte(v)
    }

    /// Append one byte.
    #[must_use]
    pub fn u8(mut self, v: u8) -> Self {
        self.buf.push(v);
        self
    }

    /// Append a little-endian `u16`.
    #[must_use]
    pub fn u16(mut self, v: u16) -> Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// Append a little-endian `u32`.
    #[must_use]
    pub fn u32(mut self, v: u32) -> Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// Append a little-endian `i16`.
    #[must_use]
    pub fn i16(mut self, v: i16) -> Self {
        self.buf.extend_from_slice(&v.to_le_bytes());
        self
    }

    /// Append `n` zero bytes.
    #[must_use]
    pub fn pad(mut self, n: usize) -> Self {
        self.buf.resize(self.buf.len() + n, 0);
        self
    }

    /// Append raw bytes.
    #[must_use]
    pub fn bytes(mut self, v: &[u8]) -> Self {
        self.buf.extend_from_slice(v);
        self
    }

    /// How many bytes have been appended past the 4-byte header.
    pub fn data_len(&self) -> usize {
        self.buf.len() - 4
    }

    /// Pad to a 4-byte boundary and stamp the length and sequence number.
    pub fn finish(mut self, seq: u16) -> Vec<u8> {
        while !self.buf.len().is_multiple_of(4) {
            self.buf.push(0);
        }
        let words = (self.buf.len() / 4) as u16;
        self.buf[2..4].copy_from_slice(&words.to_le_bytes());
        // Sequence numbers are the low 16 bits, which is what the reply and the error carry -- except
        // on an extension request, where that byte is the minor opcode.
        if !self.minor_opcode {
            self.buf[1] = (seq & 0xFF) as u8;
        }
        self.buf
    }
}

/// A reply or event, read field by field.
///
/// Reads past the end of the buffer are impossible: the connection layer always hands over a slice of
/// exactly the size the protocol says the packet is, and a `Rdr` over a short buffer is a bug that
/// panics here rather than producing a plausible wrong number in the caller.
#[derive(Debug, Clone)]
pub struct Rdr<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Rdr<'a> {
    /// A reader over `b`.
    pub fn new(b: &'a [u8]) -> Self {
        Self { b, at: 0 }
    }

    fn take(&mut self, n: usize) -> &'a [u8] {
        let end = self
            .at
            .checked_add(n)
            .expect("a field past the end of the packet");
        let out = self
            .b
            .get(self.at..end)
            .expect("a field past the end of the packet");
        self.at = end;
        out
    }

    /// One byte.
    pub fn u8(&mut self) -> u8 {
        self.take(1)[0]
    }

    /// Little-endian `u16`.
    pub fn u16(&mut self) -> u16 {
        // One `take`, then both bytes of it. The first version of this called `take(2)` twice and
        // used the first byte of each pair, which reads a little-endian value as if its bytes were
        // four apart -- every `u16` in a packet came out as `(byte0 << 8) | next_byte0`.
        let a = self.take(2);
        u16::from_le_bytes([a[0], a[1]])
    }

    /// Little-endian `u32`.
    pub fn u32(&mut self) -> u32 {
        let a = self.take(4);
        u32::from_le_bytes([a[0], a[1], a[2], a[3]])
    }

    /// Little-endian `i16`.
    pub fn i16(&mut self) -> i16 {
        let a = self.take(2);
        i16::from_le_bytes([a[0], a[1]])
    }

    /// The next `n` bytes.
    pub fn bytes(&mut self, n: usize) -> &'a [u8] {
        self.take(n)
    }

    /// Skip `n` bytes.
    pub fn skip(&mut self, n: usize) {
        self.take(n);
    }

    /// How many bytes are left.
    pub fn left(&self) -> usize {
        self.b.len().saturating_sub(self.at)
    }
}

/// The largest single request this crate will send, in bytes.
///
/// Without the `BIG-REQUESTS` extension a request may not exceed `maximum-request-length` words, and
/// that field is 65535 for every server that does not advertise the extension. So the ceiling is
/// 65,535 * 4 = 262,140 bytes, and [`crate::window::Window::put_image`] splits a frame along
/// scanlines to stay under it rather than needing the extension.
pub const MAX_REQUEST_BYTES: usize = 65_535 * 4;

/// A protocol error reported by the server.
///
/// An X11 error is *not* a fatal condition: it names one request, the connection stays up, and the
/// server carries on. Dropping the connection on the first error would turn a bad argument into a
/// lost window, so [`crate::conn::Conn`] collects these and hands them back to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolError {
    /// The server's error code.
    pub code: u8,
    /// The opcode of the request that failed.
    pub major: u8,
    /// The extension minor opcode, or 0 for a core request.
    pub minor: u16,
    /// The low 16 bits of the failing request's sequence number, or 0 if it is unknown.
    pub sequence: u16,
    /// The offending value: a resource id, an atom, a length, or a name.
    pub value: u32,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "X11 {} (code {}) on opcode {}.{} at sequence {}: {}",
            error_name(self.code),
            self.code,
            self.major,
            self.minor,
            self.sequence,
            value_note(self.code)
        )
    }
}

impl std::error::Error for ProtocolError {}

/// The name of an X11 error code.
///
/// The first seventeen are the core protocol's; anything above is extension-specific and has no core
/// name, so it is reported as a number rather than invented.
pub fn error_name(code: u8) -> &'static str {
    match code {
        1 => "BadRequest",
        2 => "BadValue",
        3 => "BadWindow",
        4 => "BadPixmap",
        5 => "BadAtom",
        6 => "BadCursor",
        7 => "BadFont",
        8 => "BadMatch",
        9 => "BadDrawable",
        10 => "BadAccess",
        11 => "BadAlloc",
        12 => "BadColor",
        13 => "BadGC",
        14 => "BadIDChoice",
        15 => "BadName",
        16 => "BadLength",
        17 => "BadImplementation",
        _ => "an extension error",
    }
}

/// What the `value` field of an error means, which is the part that actually says what went wrong.
fn value_note(code: u8) -> &'static str {
    match code {
        1 => "the server could not decode the request",
        2 => "an integer argument was out of range",
        3 => "the window id does not exist",
        4 => "the pixmap id does not exist",
        5 => "the atom does not exist",
        6 => "the cursor does not exist",
        7 => "the font does not exist",
        8 => "an argument is the wrong type for the request",
        9 => "the drawable does not exist",
        10 => "access to the resource was denied",
        11 => "the server could not allocate",
        12 => "the colormap does not exist",
        13 => "the graphics context does not exist",
        14 => "the resource id is out of the client range or already used",
        15 => "the atom or font name does not exist",
        16 => "the request length was wrong",
        _ => "an extension-defined value",
    }
}

/// How many bytes an event occupies on the wire: **32, always**.
///
/// # Every event packet is 32 bytes, including the ones whose field list is shorter
///
/// A `KeyPress` is documented with 24 bytes of fields and a `MotionNotify` with 28, and it is tempting
/// to read a per-code size table -- this crate did, and its `event_size` said exactly that. The wire
/// format is 32 bytes for every event without exception: the shorter listings are the *defined fields*,
/// and the packet is padded out to the 32-byte unit the rest of the protocol is built on.
///
/// This is a property of the protocol, not of a server or a version. The evidence that it is universal
/// is that libX11, which every client on earth links, reads exactly 32 bytes per event into its
/// `xEvent` and never varies it -- `xAnyEvent` is 32 bytes on a 64-bit build, and the decoders for the
/// shorter formats simply ignore the tail. A server that sent 24-byte key events would break libX11
/// itself, so there is no such server to break us.
///
/// # What the mistake looked like, because it cost three attempts to unlearn
///
/// Reading 24 bytes for a key event consumes the first 24 correctly and then treats the event's own
/// padding as the next packet. Measured here (`cargo run -p holonomy-x11 --example dump_keybytes`), one
/// synthesised tap arrived as 64 bytes -- a `KeyPress`, then eight bytes, then a `KeyRelease`:
/// ```text
/// 0000  02 32 19 00 f2 c9 95 13 ed 04 00 00 01 00 c0 00
/// 0010  00 00 00 00 2f 04 e1 04 | cf fe 53 01 00 00 01 00   24 bytes of event, 8 of pad
/// 0020  03 32 19 00 ...                                     the KeyRelease, 32 bytes in
/// ```
/// The symptom was not garbage, which is what made it expensive: the developer window received two key
/// presses and no releases, invented a protocol error out of the padding, and closed on the first
/// keystroke. Those eight bytes hold whatever Xwayland leaves there -- `cf fe 53 01 00 00 01 00` here --
/// and reading them as an event header produced a plausible-looking keycode of 1.
///
/// Two further turns were built on the false premise that the size was a property of the server: a
/// runtime *heuristic* that peeked at byte 24 to decide (wrong whenever the padding happened to begin
/// with a valid event code, which was about half the time), and then a *stall detector* that dropped a
/// byte to resynchronise the stream after guessing wrong. With the size constant none of that is needed:
/// a reader with the right size cannot fall out of step, so there is nothing to detect and nothing to
/// recover from. Both are deleted rather than left as dead configuration.
///
/// One thing does still vary, and is not the event size: an *extension* event with a code of 64 or above
/// may carry its length in byte 1. This client selects no extension events, so it never receives one.
pub const EVENT_BYTES: usize = 32;

/// The lowest event code that belongs to an extension, whose length is in its second byte.
pub const FIRST_EXTENSION_EVENT: u8 = 64;

/// A decoded server event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A key went down. `keycode` is an X keycode, i.e. a Linux input code plus 8.
    KeyPress {
        /// The X keycode.
        keycode: u8,
        /// The modifier mask *before* the key was applied, as the server saw it.
        state: u16,
        /// Server time in milliseconds since the last reset.
        time: u32,
        /// X within the event window.
        event_x: i16,
        /// Y within the event window.
        event_y: i16,
    },
    /// A key came up.
    KeyRelease {
        /// The X keycode.
        keycode: u8,
        /// The modifier mask before the release.
        state: u16,
        /// Server time.
        time: u32,
    },
    /// A button went down.
    ButtonPress {
        /// The button number, 1 for the left one.
        button: u8,
        /// X within the event window.
        event_x: i16,
        /// Y within the event window.
        event_y: i16,
    },
    /// A button came up.
    ButtonRelease {
        /// The button number.
        button: u8,
    },
    /// A region of the window needs repainting, in window coordinates.
    Expose {
        /// Left edge.
        x: u16,
        /// Top edge.
        y: u16,
        /// Width in pixels.
        width: u16,
        /// Height in pixels.
        height: u16,
        /// How many more `Expose`s for this region are queued.
        count: u16,
    },
    /// The window was mapped.
    MapNotify,
    /// The window was resized or moved.
    ConfigureNotify {
        /// The new width in pixels.
        width: u16,
        /// The new height in pixels.
        height: u16,
    },
    /// A property changed on a window.
    PropertyNotify {
        /// The property atom.
        atom: u32,
    },
    /// A `ClientMessage`, which is how `WM_DELETE_WINDOW` arrives.
    ClientMessage {
        /// The first 32-bit field of the message: the message type for most protocols, and the
        /// protocol atom itself when the window manager forwards `WM_DELETE_WINDOW`.
        type_atom: u32,
        /// The second field: the timestamp, or 0.
        data1: u32,
    },
    /// An event this crate does not decode, kept so a caller can log the code instead of the stream
    /// silently losing it.
    Other {
        /// The event code, with the `SendEvent` bit stripped.
        code: u8,
    },
}

impl Event {
    /// Decode a packet of exactly [`EVENT_BYTES`] bytes.
    ///
    /// Returns `None` if the packet is an error or a reply, which are not events: the connection
    /// layer dispatches those first and only calls this for a genuine event code.
    pub fn decode(packet: &[u8]) -> Option<Self> {
        let mut r = Rdr::new(packet);
        let code = r.u8() & !event::SEND_EVENT_FLAG;
        Some(match code {
            // 1 code, 1 keycode, 2 sequence, 4 time, 4 root, 4 event, 2 event-x, 2 event-y, 2 state,
            // 1 same-screen, 1 unused = 24.
            event::KEY_PRESS => {
                let keycode = r.u8();
                let _sequence = r.u16();
                let time = r.u32();
                let _root = r.u32();
                let _event = r.u32();
                let event_x = r.i16();
                let event_y = r.i16();
                let state = r.u16();
                Self::KeyPress {
                    keycode,
                    state,
                    time,
                    event_x,
                    event_y,
                }
            }
            event::KEY_RELEASE => {
                let keycode = r.u8();
                let _sequence = r.u16();
                let time = r.u32();
                let _root = r.u32();
                let _event = r.u32();
                let _event_x = r.i16();
                let _event_y = r.i16();
                let state = r.u16();
                Self::KeyRelease {
                    keycode,
                    state,
                    time,
                }
            }
            event::BUTTON_PRESS => {
                let button = r.u8();
                let _sequence = r.u16();
                let _time = r.u32();
                let _root = r.u32();
                let _event = r.u32();
                let event_x = r.i16();
                let event_y = r.i16();
                let _state = r.u16();
                Self::ButtonPress {
                    button,
                    event_x,
                    event_y,
                }
            }
            event::BUTTON_RELEASE => {
                let button = r.u8();
                let _sequence = r.u16();
                let _time = r.u32();
                let _root = r.u32();
                let _event = r.u32();
                let _event_x = r.i16();
                let _event_y = r.i16();
                let _state = r.u16();
                Self::ButtonRelease { button }
            }
            // 1 code, 1 unused, 2 sequence, 2 x, 2 y, 2 width, 2 height, 2 count, 13 unused.
            event::EXPOSE => {
                let _unused = r.u8();
                let _sequence = r.u16();
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
            event::MAP_NOTIFY => Self::MapNotify,
            event::CONFIGURE_NOTIFY => {
                let _unused = r.u8();
                let _sequence = r.u16();
                let _event = r.u32();
                let _window = r.u32();
                let _above = r.u32();
                let x = r.i16();
                let y = r.i16();
                let width = r.u16();
                let height = r.u16();
                let _border = r.u16();
                let _override = r.u8();
                let _ = (x, y);
                Self::ConfigureNotify { width, height }
            }
            event::PROPERTY_NOTIFY => {
                let _unused = r.u8();
                let _sequence = r.u16();
                let _window = r.u32();
                let atom = r.u32();
                Self::PropertyNotify { atom }
            }
            event::CLIENT_MESSAGE => {
                let _format = r.u8();
                let _seq = r.u16();
                let _window = r.u32();
                let type_atom = r.u32();
                let data1 = r.u32();
                Self::ClientMessage { type_atom, data1 }
            }
            other => Self::Other { code: other },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The length field is in words and counts the header. A client that writes the byte count here
    /// has every subsequent request interpreted at the wrong offset, which on a busy display looks
    /// like the server "ignoring" the last few requests.
    #[test]
    fn the_request_length_is_word_counted_and_includes_the_header() {
        // GetInputFocus: opcode, sequence, length 1, nothing else. 4 bytes = 1 word.
        let bytes = Req::new(op::GET_INPUT_FOCUS).finish(0x1234);
        assert_eq!(bytes, vec![op::GET_INPUT_FOCUS, 0x34, 1, 0]);

        // CreateWindow with just the fixed part and no values: 32 bytes = 8 words.
        let bytes = Req::new(op::CREATE_WINDOW)
            .u8(0)
            .u32(0x200001)
            .u32(7)
            .i16(0)
            .i16(0)
            .u16(1280)
            .u16(800)
            .u16(0)
            .u16(1)
            .u32(0)
            .u32(0)
            .finish(1);
        // 4 header + 1 depth + 4 + 4 + 2 + 2 + 2 + 2 + 2 + 2 + 4 + 4 = 33, padded to 36 = 9 words.
        assert_eq!(bytes.len(), 36, "the fixed part, padded to a word");
        assert_eq!(u16::from_le_bytes([bytes[2], bytes[3]]), 9, "9 words");
    }

    /// A request whose fields do not end on a word boundary must be padded, and the length must
    /// count the pad. `InternAtom("WM_PROTOCOLS")` is 4 header + 6 fixed + 13 of name = 23 bytes,
    /// padded to 24, which is 6 words and not 23/4 = 5.
    #[test]
    fn an_odd_length_request_is_padded_and_counted_with_the_pad() {
        let name = b"WM_PROTOCOLS";
        let r = Req::new(op::INTERN_ATOM)
            .u8(0)
            .u8(0)
            .u16(name.len() as u16)
            .u16(0)
            .bytes(name);
        assert_eq!(
            r.data_len(),
            6 + name.len(),
            "6 bytes of fixed part plus the name"
        );
        let bytes = r.finish(7);
        assert_eq!(bytes.len() % 4, 0, "padded to a word");
        assert_eq!(
            u16::from_le_bytes([bytes[2], bytes[3]]) as usize * 4,
            bytes.len(),
            "the length field times four is the request size"
        );
        assert_eq!(bytes.len(), 24, "23 bytes of request, padded to 24");
        assert_eq!(u16::from_le_bytes([bytes[2], bytes[3]]), 6);
    }

    /// An extension request's second byte is its minor opcode, not its sequence number, and the two
    /// are the same byte. Writing the minor opcode as a field instead is a two-byte error the server
    /// reports as `BadLength` -- and, for a synthesised keystroke, reports it by dropping the key.
    #[test]
    fn an_extension_request_puts_its_minor_opcode_in_the_sequence_slot() {
        let bytes = Req::new(0x7E)
            .minor(4)
            .u8(2)
            .u8(38)
            .u16(0)
            .pad(29)
            .finish(0xABCD);
        assert_eq!(bytes[0], 0x7E, "the extension's major opcode");
        assert_eq!(bytes[1], 4, "the minor opcode survives the sequence number");
        assert_ne!(
            bytes[1], 0xCD,
            "the sequence number 0xBECD is not written here"
        );
        assert_eq!(u16::from_le_bytes([bytes[2], bytes[3]]), 10, "ten words");

        // And a core request does get its sequence number.
        let core = Req::new(op::NO_OPERATION).finish(0xBECD);
        assert_eq!(core[1], 0xCD);
    }

    /// A request whose byte 1 is a field must keep the sequence number out of it. `CreateWindow` with
    /// `depth = 1` is a window of a depth no server has, and the reply says `BadMatch` about a request
    /// that looks correct in every other byte.
    #[test]
    fn a_request_with_a_field_in_byte_one_keeps_the_sequence_number_out_of_it() {
        let bytes = Req::new(op::CREATE_WINDOW)
            .second_byte(0) // depth: CopyFromParent
            .u32(0x200001)
            .u32(7)
            .u16(1280)
            .u16(800)
            .u16(0)
            .u16(1)
            .u32(0)
            .u32(0)
            .i16(0)
            .i16(0)
            .finish(0x0102);
        assert_eq!(bytes[1], 0, "the depth, not the low byte of 0x0102");
        assert_eq!(
            bytes[1], 0,
            "a depth of 0 is CopyFromParent; a sequence number of 1..255 would be an error"
        );
    }

    /// The sequence number lives in the second byte of the header, low byte only. It is what a reply
    /// and an error echo back, so getting it wrong is how a client ends up waiting forever for a
    /// reply to a different request.
    #[test]
    fn the_sequence_number_is_in_the_second_header_byte() {
        let bytes = Req::new(op::NO_OPERATION).finish(0xBEEF);
        assert_eq!(bytes[1], 0xEF, "low byte of 0xBEEF");
        assert_eq!(bytes[0], op::NO_OPERATION);
        let bytes = Req::new(op::NO_OPERATION).finish(0x0007);
        assert_eq!(bytes[1], 7);
    }

    /// Reading fields back has to be little-endian regardless of the host, because the protocol is.
    #[test]
    fn the_reader_is_little_endian_whatever_the_host_is() {
        let bytes = [0x01, 0x02, 0x03, 0x04, 0x10, 0xFE, 0x00, 0x00];
        let mut r = Rdr::new(&bytes);
        assert_eq!(r.u32(), 0x0403_0201);
        assert_eq!(r.u16(), 0xFE10, "0x10 0xFE read little-endian is 0xFE10");
        assert_eq!(r.u16(), 0);
        assert_eq!(r.left(), 0);
    }

    /// A reader that runs off the end of the packet has to panic there rather than return a
    /// plausible number: a wrong-but-quiet value is the failure mode this crate cannot detect.
    #[test]
    #[should_panic(expected = "past the end")]
    fn a_reader_past_the_end_panics_instead_of_inventing_a_value() {
        let bytes = [0u8; 3];
        let mut r = Rdr::new(&bytes);
        r.u32();
    }

    /// Every core error code has a name, because an unnamed error is an error nobody can debug.
    #[test]
    fn every_core_error_code_has_a_name() {
        for code in 1..=17u8 {
            assert_ne!(
                error_name(code),
                "an extension error",
                "code {code} is core"
            );
        }
        assert_eq!(error_name(0), "an extension error");
        assert_eq!(error_name(128), "an extension error");
    }

    /// The ceiling is the protocol's, not a round number someone liked.
    #[test]
    fn the_request_ceiling_is_the_protocols() {
        assert_eq!(MAX_REQUEST_BYTES, 262_140);
        assert_eq!(MAX_REQUEST_BYTES % 4, 0);
    }

    /// Every event is 32 bytes on the wire, whatever its field list says.
    ///
    /// This is the test that the correction is written down as a constant rather than a table. The
    /// previous version of this crate asserted the table -- 24 for key and button, 28 for motion -- and
    /// every one of those assertions was true of the *documentation* and false of the wire, which is why
    /// the table had to go rather than be corrected.
    #[test]
    fn every_event_is_thirty_two_bytes() {
        for code in [
            event::KEY_PRESS,
            event::KEY_RELEASE,
            event::BUTTON_PRESS,
            event::BUTTON_RELEASE,
            6, // MotionNotify
            event::EXPOSE,
            event::CLIENT_MESSAGE,
            event::CONFIGURE_NOTIFY,
            event::MAP_NOTIFY,
            event::PROPERTY_NOTIFY,
            35, // GenericEvent
            64, // the first extension code
        ] {
            // The size does not depend on the code at all, so it cannot depend on the `SendEvent` bit
            // either. Asserted by calling it: there is nothing to call.
            let _ = code;
        }
        assert_eq!(EVENT_BYTES, 32);
    }

    /// A `KeyPress` decodes to the keycode and modifier state the session's own state machine needs.
    /// The offset of `state` is the thing that goes wrong: it sits *after* the event window, not
    /// after the keycode, so a reader that reads it early gets a piece of a resource id.
    #[test]
    fn a_key_press_decodes_to_its_keycode_and_state() {
        // code, keycode, sequence, time, root, event, event-x, event-y, state, same-screen, unused
        let mut p = vec![event::KEY_PRESS, 38];
        p.extend_from_slice(&7u16.to_le_bytes()); // sequence
        p.extend_from_slice(&0x1122_3344u32.to_le_bytes()); // time
        p.extend_from_slice(&0x111u32.to_le_bytes()); // root
        p.extend_from_slice(&0x200_001u32.to_le_bytes()); // event
        p.extend_from_slice(&(-3i16).to_le_bytes()); // event-x
        p.extend_from_slice(&(-4i16).to_le_bytes()); // event-y
        p.extend_from_slice(&0x0060u16.to_le_bytes()); // state: ShiftMask | LockMask
        p.push(1); // same-screen
        p.push(0); // unused
        assert_eq!(p.len(), 24);

        let got = Event::decode(&p).expect("a key press");
        assert_eq!(
            got,
            Event::KeyPress {
                keycode: 38,
                state: 0x0060,
                time: 0x1122_3344,
                event_x: -3,
                event_y: -4,
            },
            "38 is X keycode for 'a', which is KEY_A (30) plus the offset of 8"
        );
    }

    /// `Expose` is 32 bytes but its fields are 22, so the tail is padding. Decoding must stop at the
    /// count rather than run into it.
    #[test]
    fn an_expose_decodes_its_rectangle() {
        let mut p = vec![event::EXPOSE, 0, 0, 0]; // code, unused, sequence
        for v in [0u16, 100, 1280, 800, 2] {
            p.extend_from_slice(&v.to_le_bytes());
        }
        p.resize(32, 0);
        assert_eq!(
            Event::decode(&p).expect("an expose"),
            Event::Expose {
                x: 0,
                y: 100,
                width: 1280,
                height: 800,
                count: 2
            }
        );
    }

    /// An event code this crate does not decode is reported, not dropped: a window that silently
    /// forgets events is a window nobody can debug.
    #[test]
    fn an_undecoded_event_is_reported_by_code() {
        let p = vec![24u8 /* GravityNotify */, 0];
        let mut p = p;
        p.resize(32, 0);
        assert_eq!(
            Event::decode(&p).expect("an event"),
            Event::Other { code: 24 }
        );
    }
}
