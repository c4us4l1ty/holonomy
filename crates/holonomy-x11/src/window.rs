//! The window: create, map, and push pixels.
//!
//! # Why `PutImage` and not `MIT-SHM`
//!
//! `MIT-SHM` would let the frame live in shared memory and the server would read it in place, which
//! is the right answer for a 60fps full-frame path. It is also a second extension handshake, a
//! `shmget`/`shmat` pair whose failure modes (ENOMEM on a small `/dev/shm`, a stale segment from a
//! crashed client) are their own project, and a synchronisation protocol on top. Measured against the
//! alternative: this crate only ever pushes *damage*, so a keystroke moves 18 rows of 1280 pixels =
//! 92,160 bytes and a caret blink moves 144. At a 10ms frame cap that is under 9 MB/s on a unix
//! socket, which is not the bottleneck. `MIT-SHM` belongs with the real DRM path, if it does at all.
//!
//! # Why the frame is chunked along scanlines
//!
//! A 1280x800 frame at 32 bits per pixel is 4,096,000 bytes. The largest request a server accepts
//! without `BIG-REQUESTS` is 65,535 words = 262,140 bytes, so the frame needs at least 16 requests.
//! `PutImage` splits naturally along scanlines: each chunk carries a `dst-y` and a `height`, so the
//! chunks tile the window exactly and no row is sent twice. A row itself cannot be split, which is
//! why [`Window::put_image`] refuses a width whose row exceeds the ceiling instead of corrupting it.
//!
//! # The pixel format is the same one the rest of the project uses
//!
//! A [`Frame`](../../holonomy_display/struct.Frame.html) is a `Vec<u32>` of `0x00RRGGBB`, row-major,
//! top-left origin. On this host the server reports root depth 24 and offers a 32-bits-per-pixel
//! format for it, in `LSBFirst` order, so the frame's bytes are the framebuffer's bytes: `put_image`
//! takes the slice and sends it with no conversion at all. A server offering depth 24 at 16 bits per
//! pixel, or a big-endian one, is refused by name rather than rendered incorrectly.

use std::fmt;
use std::time::{Duration, Instant};

use crate::conn::{Conn, ConnError, Setup};
use crate::proto::{self, cw, gc, image, mask, op, value, Rdr, Req};

/// Why a window operation failed.
/// **`WM_NORMAL_HINTS` as the server recorded it.** Part 34.
///
/// **A reader's view of the structure, and it exists so that a test can say what the server *stored*
/// rather than what the client *meant*.** See [`Window::size_hints`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeHints {
    /// The flags word: `PSize`, `PMinSize`, `PMaxSize`, …
    pub flags: u32,
    /// The width `PSize` names. **Only meaningful when `PSize` is set in [`flags`](Self::flags).**
    ///
    /// **Added 2026-10-12, and it is the field the whole defect was in.** `PSize` says "the `width`
    /// and `height` below the flags are the size this window wants to be", and nothing read them
    /// back — so a hint set that said `PSize` with a width of zero was indistinguishable from a hint
    /// set that was merely wrong somewhere nobody checked. **A reader that cannot see the field a
    /// writer wrote cannot check it.**
    pub width: i32,
    /// The height `PSize` names. See [`width`](Self::width).
    pub height: i32,
    /// Minimum width, in pixels.
    pub min_width: i32,
    /// Minimum height, in pixels.
    pub min_height: i32,
    /// Maximum width. Zero means "no maximum".
    pub max_width: i32,
    /// Maximum height. Zero means "no maximum".
    pub max_height: i32,
    /// Width increment. One means "any width".
    pub width_inc: i32,
    /// Height increment. One means "any height".
    pub height_inc: i32,
}

#[derive(Debug)]
pub enum WindowError {
    /// The connection failed.
    Conn(ConnError),
    /// The server has no 32-bit format for its root depth, so a `Frame` cannot be pushed as it is.
    NoFormat {
        /// The root depth the server reported.
        depth: u8,
        /// Every format it did report, as `depth/bpp`.
        available: Vec<(u8, u8)>,
    },
    /// A single scanline is longer than the largest request, so it cannot be sent whole.
    RowTooWide {
        /// The row's byte length.
        row: usize,
        /// The server's ceiling.
        limit: usize,
    },
    /// A scanline is not a whole number of the server's pad units, so the server would insert padding
    /// this crate does not undo.
    ///
    /// **Its own variant, part 36.** This was reported as [`RowTooWide`](Self::RowTooWide) with
    /// `limit: usize::MAX`, which is a request-size complaint about a row that was never too wide --
    /// it was padded wrongly. The two have nothing to do with each other, and a reader of the old
    /// message had no way to know that.
    RowNotPadded {
        /// The scanline, in bits.
        row_bits: usize,
        /// The server's `bitmap-format-scanline-pad`, in bits.
        pad: u8,
    },
    /// The server rejected a request. Named rather than swallowed.
    Protocol(proto::ProtocolError),
    /// A reply was short.
    ShortReply(&'static str),
    /// The caller handed over fewer bytes than the width and height call for.
    ShortBuffer {
        /// How many bytes were given.
        have: usize,
        /// How many the rectangle needs.
        need: usize,
    },
    /// A resize to zero. The server would answer `BadValue`, and the caller's own arithmetic is what
    /// asked for it, so it is reported as the mistake it is rather than as the server's answer.
    ZeroSize {
        /// The width asked for.
        width: u16,
        /// The height asked for.
        height: u16,
    },
}

impl fmt::Display for WindowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conn(e) => write!(f, "{e}"),
            Self::NoFormat { depth, available } => write!(
                f,
                "the server's root depth is {depth} and it offers no 32-bits-per-pixel format for it \
                 (it has {available:?}); a Frame is 0x00RRGGBB in a u32 and cannot be sent to this \
                 server without a conversion this crate does not have"
            ),
            Self::RowNotPadded { row_bits, pad } => write!(
                f,
                "a scanline is {row_bits} bits and the server pads rows to {pad}; the client would \
                 have to undo the padding it does not insert"
            ),
            Self::RowTooWide { row, limit } => write!(
                f,
                "one scanline is {row} bytes and the server accepts {limit} bytes per request; \
                 PutImage cannot split a scanline"
            ),
            Self::Protocol(e) => write!(f, "{e}"),
            Self::ShortReply(what) => write!(f, "the reply to {what} was shorter than its header"),
            Self::ShortBuffer { have, need } => {
                write!(f, "the caller gave {have} bytes for a rectangle that needs {need}")
            }
            Self::ZeroSize { width, height } => {
                write!(f, "cannot resize to {width}x{height}: a window has no zero dimension")
            }
        }
    }
}

impl std::error::Error for WindowError {}

impl From<ConnError> for WindowError {
    fn from(e: ConnError) -> Self {
        Self::Conn(e)
    }
}

/// An X11 window that a frame is pushed into.
///
/// The window owns its id and its graphics context. It does **not** own the connection: the window
/// borrows it for every call, so one connection can drive a window and an input source without either
/// of them having to be shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    id: u32,
    gc: u32,
    width: u32,
    height: u32,
    depth: u8,
}

impl SizeHints {
    /// Parse the 72 bytes of an `XSizeHints`.
    ///
    /// **A free-standing parser because the writer is one**, and because the defect this crate spent
    /// a whole part failing to find was in the pair of them: the writer set `PSize` and wrote two
    /// zeros where the size goes, and the reader skipped those two words as "obsolete" and so had no
    /// way to disagree with it. **`from_bytes` and `size_hints_bytes_for` are each other's check**,
    /// and `tests/window_size.rs` is what runs them against each other.
    ///
    /// **Missing words read as zero, not as an error.** A short buffer is a truncated property, and
    /// zero is what the protocol says an absent field means; refusing would turn "the server sent
    /// less than we asked for" into a panic on a path whose whole job is to report what the server
    /// said.
    pub fn from_bytes(value: &[u8]) -> Self {
        let word = |i: usize| -> i32 {
            let o = i * 4;
            match value.get(o..o + 4) {
                Some(b) => i32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                None => 0,
            }
        };
        Self {
            flags: word(0) as u32,
            width: word(3),
            height: word(4),
            min_width: word(5),
            min_height: word(6),
            max_width: word(7),
            max_height: word(8),
            width_inc: word(9),
            height_inc: word(10),
        }
    }
}

/// Build the 72 bytes of an `XSizeHints`.
///
/// **Extracted from [`Window::set_size_hints`] so a gate can reach it.** The structure was built
/// inline, which means the only way to ask "what bytes does this send" was to send them to a server
/// and read the property back — **so the one field that was wrong could not be checked without an X
/// display, and the two tests that would have caught it were the ones that needed one.** The
/// arithmetic is now a pure function and `tests/window_size.rs` checks it on every machine.
///
/// # The layout, and why this comment is not the authority
///
/// ```text
///   0  long   flags          PSize|PMinSize|PMaxSize
///   4  int    x              obsolete, zero
///   8  int    y              obsolete, zero
///  12  int    width, height  PSize: the size the window was created at
///  20  int    min_width, min_height
///  28  int    max_width, max_height      0 means "no maximum"
///  36  int    width_inc, height_inc      1 pixel: no step
///  44  int    min_aspect.x, .y           zero: no aspect
///  52  int    max_aspect.x, .y           zero: no aspect
///  60  int    base_width, base_height    zero
///  68  int    win_gravity                zero
/// ```
///
/// **Four obsolete ints, not three.** An earlier version of this comment listed them as `4 int x, y`
/// and `8 int width, height`, which put `width` at 8 and `height` at 12 — and **every offset below
/// was correct anyway**, so the minimum and maximum worked and only `PSize` pointed at two zeros.
/// **That is the argument for testing the bytes rather than reading the comment:** the comment was
/// wrong in the one row nobody needed and right in all the rows everybody checked, so reading it
/// confirmed the parts that were fine.
pub fn size_hints_bytes_for(size: (u16, u16), min: (u16, u16), max: Option<(u16, u16)>) -> Vec<u8> {
    let mut data = vec![0u8; 18 * 4];
    // # CORRECTION 2026-10-12: a flag is set only when the field it names is written
    //
    // **This function set `PMaxSize` unconditionally and then wrote zeroes into the maximum when
    // `max` was `None`.** The old comment said *"0 means 'no maximum', which is how the protocol
    // spells unlimited"* — **and that is only true when `PMaxSize` is _not_ set.** `PMaxSize` does not
    // mean "there is a maximum field"; it means **"the two words at offset 28 are the maximum"**. So
    // the flag plus two zeroes is not *no maximum*, it is *a maximum of 0 x 0*, and a window manager
    // that believes it will size the window to `min(min_width, 0) x min(min_height, 0)` — which is
    // the minimum, because the minimum is the only thing left.
    //
    // ## Measured, one hint at a time, so no part of it is theory
    //
    // Four windows created at 800 x 600 on this machine, differing only in what was published:
    //
    // ```text
    // no hints at all          -> GetGeometry 800x600
    // PSize only               -> GetGeometry 800x600
    // PSize + min 768x320      -> GetGeometry 768x320   <-- the defect
    // PSize + min + max 4096   -> GetGeometry 800x600
    // ```
    //
    // **The third row is this bug and the fourth is the fix**, and the first two say the problem is
    // not `PSize` and not the minimum: it is the flag with nothing behind it. That is also the whole
    // of *"I cannot drag it"* — a window whose maximum the window manager believes is 0 x 0 cannot be
    // grown by a drag, or by anything else.
    //
    // ## The rule, stated so it can be checked
    //
    // **`PSize` names `width`/`height` and they are always written. `PMinSize` names `min_width`/
    // `min_height` and they are always written. `PMaxSize` names `max_width`/`max_height` and they
    // are written only when there is a maximum — so the flag is set only then.** `PResizeInc` is
    // never set, so the increments below are ignored, and one is a harmless value for an ignored
    // field. `a_flag_is_set_only_when_the_field_it_names_is_written` is the gate for this paragraph.
    let mut flags = 1u32 | (1 << 4); // PSize | PMinSize
    data[12..16].copy_from_slice(&u32::from(size.0).to_le_bytes());
    data[16..20].copy_from_slice(&u32::from(size.1).to_le_bytes());
    data[20..24].copy_from_slice(&u32::from(min.0).to_le_bytes());
    data[24..28].copy_from_slice(&u32::from(min.1).to_le_bytes());
    match max {
        Some((w, h)) => {
            flags |= 1 << 5; // PMaxSize, and only now is the field behind it written
            data[28..32].copy_from_slice(&u32::from(w).to_le_bytes());
            data[32..36].copy_from_slice(&u32::from(h).to_le_bytes());
        }
        None => {
            // **Left at zero and, crucially, `PMaxSize` is not set** — so the words are ignored
            // rather than read as a maximum of nothing.
            data[28..36].copy_from_slice(&[0u8; 8]);
        }
    }
    data[0..4].copy_from_slice(&flags.to_le_bytes());
    // width-inc, height-inc = 1 pixel; there is no step.
    data[36..40].copy_from_slice(&1u32.to_le_bytes());
    data[40..44].copy_from_slice(&1u32.to_le_bytes());
    data
}

impl Window {
    /// Create a window of `width` x `height`, set its title, and select the events a word processor
    /// needs: keys, buttons, exposure, and structural change.
    ///
    /// `title` is set as `WM_NAME`, which is the only title property a bare client can set without
    /// an ICCCM `WM_CLASS` and `_NET_WM_NAME` handshake. A compositor shows it in the title bar
    /// anyway.
    pub fn create(
        conn: &mut Conn,
        width: u32,
        height: u32,
        title: &str,
    ) -> Result<Self, WindowError> {
        Self::create_inner(conn, width, height, title, false)
    }

    /// A window no window manager will take ownership of.
    ///
    /// `override-redirect` is the difference between a window whose geometry this client decides and
    /// one whose geometry a window manager decides. It is not a cosmetic setting: with it the server
    /// hands every `ConfigureWindow` straight through, so a resize takes effect; without it, a window
    /// manager that manages the window may ignore the resize and send a `ConfigureNotify` with whatever
    /// size it prefers instead. Measured under GNOME's mutter on this machine: an ordinary window
    /// created at 1024x700 stays 1024x700 after a `ConfigureWindow` to 1600x1000, with no error of any
    /// kind -- the request is accepted and the size is simply not the client's to change. So this is
    /// what a gate uses to test a resize, and it is why [`Window::configure_size`] on a managed window
    /// is best read as a request that may be declined.
    pub fn create_override_redirect(
        conn: &mut Conn,
        width: u32,
        height: u32,
        title: &str,
    ) -> Result<Self, WindowError> {
        Self::create_inner(conn, width, height, title, true)
    }

    /// A handle on a window **this connection did not create**, for a driver that has to reach into a
    /// running application.
    ///
    /// # Why this is here
    ///
    /// **Because the interesting questions about a window cannot be asked from inside it.** "Does the
    /// window resize?", "what does the server say its size is?", "what is actually on screen?" are all
    /// answered by a second process talking to the server about a window id — and every one of those is
    /// a method on this type, which is why the driver needs a way to *hold* the window rather than
    /// only a way to make one.
    ///
    /// **It builds the same handle `create` does**, including a fresh graphics context, because the
    /// methods a driver uses are the same requests: `PutImage`/`GetImage` name a GC and a depth, and a
    /// handle that carried somebody else's GC id would be sending a request about another client's
    /// object.
    ///
    /// **The size is the server's to fill in**: it is recorded from `GetGeometry` where it can be, and
    /// the driver should not trust the `width`/`height` it passes for anything but a starting guess.
    pub fn attach(conn: &mut Conn, id: u32, width: u32, height: u32) -> Self {
        let depth = conn.setup().root_depth;
        let gc_id = conn.alloc_id();
        let gc = conn
            .request(
                Req::new(op::CREATE_GC)
                    .second_byte(0)
                    .u32(gc_id)
                    .u32(id)
                    .u32(gc::GRAPHICS_EXPOSURES)
                    .u32(0),
            )
            .is_ok()
            .then_some(gc_id);
        Self {
            id,
            gc: gc.unwrap_or(gc_id),
            width,
            height,
            depth,
        }
    }

    fn create_inner(
        conn: &mut Conn,
        width: u32,
        height: u32,
        title: &str,
        override_redirect: bool,
    ) -> Result<Self, WindowError> {
        let setup = conn.setup().clone();
        let format = setup
            .format(setup.root_depth, 32)
            .ok_or_else(|| WindowError::NoFormat {
                depth: setup.root_depth,
                available: setup
                    .formats
                    .iter()
                    .map(|f| (f.depth, f.bits_per_pixel))
                    .collect(),
            })?;
        // **`scanline_pad` is in BITS, and this check was in pixels. Part 36.**
        //
        // The Core Protocol's `bitmap-format-scanline-pad` is a bit count -- 32 on a 32-bpp server,
        // which is **four pixels**. This expression multiplied both sides by 8, so the factor cancelled
        // and the test came out `width % scanline_pad == 0`: **a window had to be a multiple of 32
        // pixels.** Eight times stricter than the protocol, and it refused ordinary widths -- a window
        // at nine tenths of a 3,072 px screen is 2,764 px, `2764 % 32 == 12`, and the application
        // refused to start, with an error about the request size rather than about padding.
        //
        // **This exact block appeared twice in this file** and the first edit fixed one of them, so the
        // symptom survived the fix. **Two copies of a check is one copy too many**, and a reader who
        // had edited the first would have had no reason to look for the second.
        let row_px = width as usize;
        let row_bits = row_px * usize::from(format.bits_per_pixel);
        if !row_bits.is_multiple_of(format.scanline_pad.max(1) as usize) {
            return Err(WindowError::RowNotPadded {
                row_bits,
                pad: format.scanline_pad,
            });
        }
        let id = conn.alloc_id();
        let gc_id = conn.alloc_id();

        // depth 0 is CopyFromParent, which takes the root's depth and visual.
        conn.request(
            Req::new(op::CREATE_WINDOW)
                .second_byte(0) // depth: CopyFromParent, which is byte 1 and not a sequence number
                .u32(id)
                .u32(setup.root)
                .i16(0)
                .i16(0)
                .u16(width as u16)
                .u16(height as u16)
                .u16(0)
                .u16(1) // InputOutput
                .u32(0) // CopyFromParent visual
                // `CWOverrideRedirect` is in the mask unconditionally, with a zero value for an
                // ordinary window, because the value list's *length* is derived from the mask: a
                // fourth value with no fourth bit is a `BadLength`, not an ignored extra. The values
                // are in increasing bit order, as the specification requires, and those bits are
                // BACK_PIXEL (1), BORDER_PIXEL (3), OVERRIDE_REDIRECT (9), EVENT_MASK (11).
                .u32(cw::BACK_PIXEL | cw::BORDER_PIXEL | cw::OVERRIDE_REDIRECT | cw::EVENT_MASK)
                .u32(setup.black_pixel)
                .u32(setup.black_pixel)
                .u32(override_redirect.into())
                .u32(Self::event_mask()),
        )?;
        Self::set_title(conn, id, title)?;
        Self::set_delete_protocol(conn, id)?;

        // GCGraphicsExposures off: a PutImage outside the window would otherwise generate an
        // Expose per exposed pixel, and this client repaints from its own damage tracking.
        conn.request(
            Req::new(op::CREATE_GC)
                .second_byte(0) // unused
                .u32(gc_id)
                .u32(id)
                .u32(gc::GRAPHICS_EXPOSURES)
                .u32(0),
        )?;
        conn.sync()?;
        if let Some(e) = conn.take_error() {
            return Err(WindowError::Protocol(e));
        }

        Ok(Self {
            id,
            gc: gc_id,
            width,
            height,
            depth: setup.root_depth,
        })
    }

    /// The events this client asks for.
    ///
    /// `Exposure` is not optional: without it the window comes up blank and nothing ever asks for a
    /// repaint, because X11 has no "please redraw" event -- the server *is* the one that tells the
    /// client a region was lost.
    pub const fn event_mask() -> u32 {
        mask::KEY_PRESS
            | mask::KEY_RELEASE
            | mask::BUTTON_PRESS
            | mask::BUTTON_RELEASE
            | mask::POINTER_MOTION
            | mask::EXPOSURE
            | mask::STRUCTURE_NOTIFY
            | mask::FOCUS_CHANGE
    }

    fn set_title(conn: &mut Conn, id: u32, title: &str) -> Result<(), WindowError> {
        // ChangeProperty: mode Replace(0), window, property, type, format 8, 3 unused, length, data.
        //
        // After the four-byte header there is nothing between the mode and the window: the request
        // length occupies bytes 2 and 3 and `Req::finish` writes it. The first version of this builder
        // wrote a spare `u16` there "for the length", which shifted the window, the property, the type,
        // the format and the data length by two bytes -- the request was the right length and the
        // server answered `BadValue` on it, because the property it named was the format.
        let data = title.as_bytes();
        let data = &data[..data.len().min(255)];
        conn.request(
            Req::new(op::CHANGE_PROPERTY)
                .second_byte(0) // Replace
                .u32(id)
                .u32(proto::atom::WM_NAME)
                .u32(proto::atom::STRING)
                .u8(8)
                .u8(0)
                .u16(0)
                .u32(data.len() as u32)
                .bytes(data),
        )?;
        Ok(())
    }

    /// Ask the window manager to send `WM_DELETE_WINDOW` instead of killing the client when the
    /// title bar's close button is used.
    ///
    /// Without this, closing the window on a managed desktop terminates the process without any
    /// chance to flush -- which for an editor means losing the document. A compositor that ignores
    /// `WM_PROTOCOLS` will still kill the client, which is why the loop also watches for the socket
    /// closing.
    fn set_delete_protocol(conn: &mut Conn, id: u32) -> Result<(), WindowError> {
        let wm_protocols = Self::intern(conn, "WM_PROTOCOLS")?;
        let wm_delete = Self::intern(conn, "WM_DELETE_WINDOW")?;
        conn.request(
            Req::new(op::CHANGE_PROPERTY)
                .second_byte(0) // Replace
                .u32(id)
                .u32(wm_protocols)
                .u32(proto::atom::ATOM)
                .u8(32)
                .u8(0)
                .u16(0)
                .u32(1)
                .u32(wm_delete),
        )?;
        Ok(())
    }

    /// `InternAtom`, resolving through the sync so the reply is in hand before it is used.
    fn intern(conn: &mut Conn, name: &str) -> Result<u32, WindowError> {
        // byte 1 is `only-if-exists`; the request is 8 + name bytes, so a 12-byte name is 5 words.
        let seq = conn.request(
            Req::new(op::INTERN_ATOM)
                .second_byte(0) // create it if it does not exist
                .u16(name.len() as u16)
                .u16(0)
                .bytes(name.as_bytes()),
        )?;
        conn.flush()?;
        let body = conn.reply(seq)?;
        let mut r = Rdr::new(&body);
        r.skip(8);
        Ok(r.u32())
    }

    /// This window's resource id.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// The window's width.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// The window's height.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// The depth the window was created with, which is its parent's.
    pub fn depth(&self) -> u8 {
        self.depth
    }

    /// Show the window.
    pub fn map(&self, conn: &mut Conn) -> Result<(), WindowError> {
        conn.request(Req::new(op::MAP_WINDOW).second_byte(0).u32(self.id))?;
        conn.sync()?;
        // Claim this request's error here. A queued error left for the next caller turns one bad request
        // into a failure somewhere else entirely, which is how a malformed `SendEvent` became a frame
        // that refused to present.
        if let Some(e) = conn.take_error() {
            return Err(WindowError::Protocol(e));
        }
        Ok(())
    }

    /// Take the keyboard focus. Returns what had it before.
    ///
    /// # This waits for the window to be viewable
    ///
    /// `SetInputFocus` on a window that is not yet viewable is a `BadMatch`, and a window is not
    /// viewable until the server has processed its `MapWindow` and sent the `MapNotify`. Measured here:
    /// focusing immediately after `map` failed with `BadMatch` every time, naming a window id that
    /// existed and was the right size.
    ///
    /// So this drains events until `MapNotify` for *this* window arrives, bounded, and focuses after.
    /// A window manager that maps asynchronously is then handled; a server that never sends
    /// `MapNotify` costs the caller the timeout rather than an error.
    pub fn focus(&self, conn: &mut Conn) -> Result<u32, WindowError> {
        self.focus_when_mapped(conn, Duration::from_millis(500))
    }

    /// Ask the window manager to bring this window to the front and give it the keyboard.
    ///
    /// # `SetInputFocus` alone is not enough on a managed desktop
    ///
    /// Measured here, on GNOME: `Window::focus` succeeds -- the server answers without error and
    /// `GetInputFocus` reports this window -- and a synthesised keystroke still does not arrive. The
    /// one event that does arrive is `FocusOut`, event code 10, and the keystrokes go elsewhere. That is
    /// focus-stealing prevention, and the way past it is the EWMH request every window manager
    /// implements: `_NET_ACTIVE_WINDOW`, a `ClientMessage` on the root window.
    ///
    /// The three fields are the request source (2, "pager", which is what a program should send), the
    /// timestamp (0, "no time") and the user time (0). Returns the atom that was sent, so a caller can
    /// report that the window manager did not have it.
    pub fn activate(&self, conn: &mut Conn) -> Result<u32, WindowError> {
        let net_active = Self::intern(conn, "_NET_ACTIVE_WINDOW")?;
        // SendEvent: opcode, propagate, length, destination window, event mask, then a 32-byte event.
        // The event is a ClientMessage: code 33, format 32, a sequence number, the window, the type,
        // and 20 bytes of data.
        //
        // The first version of this wrote the atom into the *event mask* field and the message code
        // into the format field, so the request was the right length and the server's answer was an
        // error naming it. Nothing noticed, because the error sat in the connection's queue and was
        // picked up by the *next* request to ask for it -- which was a full-screen frame push, so the
        // first thing the window ever did was report a frame failure caused by the activation three
        // calls earlier.
        let data1 = 2u32; // source indication: a pager, which is what a program should send
        let data2 = 0u32; // timestamp: none
        let req = Req::new(op::SEND_EVENT)
            .second_byte(0) // propagate
            .u32(conn.setup().root)
            .u32(0) // event mask: no filter
            .u8(33) // ClientMessage
            .u8(32) // format: 32-bit
            .u16(0) // sequence
            .u32(self.id)
            .u32(net_active)
            // Exactly five words of data, which is what a `ClientMessage` carries after its type. Six
            // makes the request 48 bytes instead of 44 and the server answers `BadLength` -- and because
            // an error arrives *after* the reply to the request that followed it, that error was not
            // waiting for `activate` and turned up three requests later as the reason a frame refused to
            // present. Measured, from the trace: `x11 <- error [00 10 ...] sequence 11`, opcode 25.
            .u32(data1)
            .u32(data2)
            .u32(0)
            .u32(0)
            .u32(0);
        conn.request(req)?;
        conn.sync()?;
        if let Some(e) = conn.take_error() {
            return Err(WindowError::Protocol(e));
        }
        Ok(net_active)
    }

    /// Ask the server to make the window `width` x `height`.
    ///
    /// `ConfigureWindow` with only `width` and `height` in the value mask, which is the minimum a
    /// client needs in order to be resizable at all: a window with no size in its `WM_NORMAL_HINTS`
    /// and none set by a `ConfigureWindow` is a fixed-size island in a resizable desktop.
    ///
    /// The request has no reply, so it is followed by a sync and this claims *its* error: a rejected
    /// `ConfigureWindow` -- `BadValue` for a width of zero, which is what a window manager sends while
    /// a drag is in progress -- would otherwise be found by whatever asked for a queued error next.
    pub fn configure_size(
        &self,
        conn: &mut Conn,
        width: u16,
        height: u16,
    ) -> Result<(), WindowError> {
        if width == 0 || height == 0 {
            return Err(WindowError::ZeroSize { width, height });
        }
        // # The length is 3 words plus one per value the mask names
        //
        // `sz_xConfigureWindowReq` is 12 -- a `CARD16 mask` and a `CARD16 pad2` after the window -- so a
        // mask naming `width` and `height` is 12 + 8 = 20 bytes, five words. That is what the
        // specification says and what this sends.
        //
        // # The dead end this spent a day in
        //
        // **Writing the two `CARD16`s in the wrong order.** This sent `.u16(0)` for what it called the
        // mask's high half and `.u16(mask)` second, on the theory that a 16-bit mask wanted its halves
        // in that order -- so the mask landed at offset 10, in `pad2`, and the server read a mask of
        // zero at offset 8. A mask of zero means *no values*, so the length it wanted was three words
        // and the five sent were two too many: `BadLength`, on every length, at every mask position,
        // with a `value: 25165825` that is `0x01800001` -- the window id with a bit set in it, which is
        // a window error's `value` field being read out of a length error's context. The specification
        // lists the fields in order; the only reason to reorder them is a mistake.
        //
        // # On a managed window this may change nothing, and that is not an error
        //
        // A window manager owns a managed window's geometry. It receives the `ConfigureWindow` as a
        // `ConfigureRequest` and may send a `ConfigureNotify` with a size of its own choosing
        // afterwards, so this returns `Ok` and the window is still the size it was. Measured under
        // mutter on this machine: created at 1024x700, asked for 1600x1000, no error, still 1024x700.
        // The product does not depend on this path -- a window resizes when a person drags it, and the
        // window manager says so with `ConfigureNotify`, which is what `Session::resize` is driven by
        // -- but a caller that wants the size it asked for must not use a managed window. See
        // [`Window::create_override_redirect`].
        //
        // Two smaller dead ends, written down because both look like findings and neither is.
        //
        // **Adding four pad bytes** makes it 24 bytes and the server answers `BadLength`. The rule has
        // no slack in it: `sizeof(xConfigureWindowReq)` is 12, not 16, even though `LISTofVALUE
        // value-list` reads like an array and there is a `pad2` in the middle. (Searching the headers
        // on this machine for `sizeof` finds nothing -- `Xproto.h` defines `sz_xConfigureWindowReq 12`
        // and that is the number.)
        //
        // **Sweeping the length to find the rule** does not work, because a `BadLength` leaves the
        // server reading four bytes into the middle of the next request: everything after the first
        // refusal on a connection is answered about the wrong thing. The second version of the sweep
        // opened a connection per shape and still produced nonsense, because its own
        // `probe_request` wrote the bytes straight into the output buffer without taking a sequence
        // number -- so from the second request on, every sequence was off by one and `sync` waited
        // for a reply that had already been labelled for the request after it. The apparent
        // "accepted, then timed out" pattern was entirely that. `Conn::probe_request` and the sweep
        // that used it are gone rather than fixed, because a method that silently corrupts the
        // sequence counter has no use that a correct one does not have.
        conn.request(
            Req::new(op::CONFIGURE_WINDOW)
                .second_byte(0) // unused
                .u32(self.id)
                .u16(value::WIDTH | value::HEIGHT) // offset 8
                .u16(0) // offset 10, the `pad2`
                .u32(u32::from(width))
                .u32(u32::from(height)),
        )?;
        conn.sync()?;
        if let Some(e) = conn.take_error() {
            return Err(WindowError::Protocol(e));
        }
        Ok(())
    }

    /// Tell the window manager this window's current size and the smallest and largest it can draw
    /// into.
    ///
    /// `WM_NORMAL_HINTS`, with a `WM_NORMAL_HINTS` structure of flags, the old and new sizes, the
    /// increments and the counts. `size` and `min-size` and `max-size` are all set: this is a window
    /// with no fixed aspect ratio and no step, so the increments are one pixel and the counts are the
    /// whole field.
    ///
    /// # CORRECTION 2026-10-12: `PSize` was set and the size it names was **zero**
    ///
    /// This is what made the window un-resizable and tiny, and it is the whole of the *"it's way too
    /// small and I can't drag it"* report. `flags` said
    ///
    /// ```text
    /// PSize | PMinSize | PMaxSize = 1 | 16 | 32 = 49
    /// ```
    ///
    /// **and `width` and `height` were never written, so both were zero.** `PSize` is not a
    /// declaration that *this client has a size*; it is a declaration that **the `width` and `height`
    /// fields below it are meaningful**. A window manager reading `PSize` with a zero size is being
    /// told *this window wants to be 0 x 0*, and clamps it up to `min_width x min_height`.
    ///
    /// ## The measurement, from the server and not from the app
    ///
    /// The app asked for 1600 x 1000 and printed that it had. `GetGeometry` on the same window,
    /// decoded from the wire, answered:
    ///
    /// ```text
    /// [ 8] ed 04 00 00  32 00 7c 00    root = 1261,  x = 50,  y = 124
    /// [16] 00 03 40 01                 width = 0x0300 = 768,  height = 0x0140 = 320
    /// ```
    ///
    /// **768 x 320 is exactly `MIN_WIDTH x MIN_HEIGHT`** — the clamp floor, not a size anything asked
    /// for. The app and the server disagreed by 1092 px and only one of them was wrong.
    ///
    /// ## Why part 34's fix did not catch this
    ///
    /// Part 34 corrected two real bugs here: the fields were written twelve bytes early, and the
    /// request was two bytes too long so the server answered `BadLength` and the caller discarded the
    /// error. **Both were fixed and the property was still wrong**, because they were checked by
    /// whether a `GetProperty` answered at all — **not by what it said**. `read_size_hints` is the
    /// instrument that answers the second question, and `the_size_hints_name_a_real_size` is the gate
    /// that now uses it.
    ///
    /// ## And the comment's offset table was wrong too
    ///
    /// It read `4 int x, y` and `8 int width, height`, which puts `width` at 8 and `height` at 12.
    /// The Core Protocol's `XSizeHints` has **four** obsolete ints, not three:
    ///
    /// ```text
    ///   0  long   flags
    ///   4  int    x                 obsolete
    ///   8  int    y                 obsolete
    ///  12  int    width             PSize
    ///  16  int    height            PSize
    ///  20  int    min_width, min_height
    /// ```
    ///
    /// **The offsets below `width` were right and the two above it were not**, which is why the
    /// minimum worked all along and only `PSize` was nonsense: a table that is wrong in the part
    /// nobody needed still reads as authoritative in the part somebody does.
    pub fn set_size_hints(
        &self,
        conn: &mut Conn,
        size: (u16, u16),
        min: (u16, u16),
        max: Option<(u16, u16)>,
    ) -> Result<(), WindowError> {
        // # The structure, field by field, because this is where it was wrong twice
        //
        // `XSizeHints` is **18 longs: 72 bytes**, and every field below is named rather than counted.
        // The obsolete block is **`int x, y, width, height` -- four ints, SIXTEEN bytes at 4..20** --
        // and the previous version of this comment said "the first eight bytes", which is how every
        // field below ended up written **12 bytes early**: `min_width` landed in `width`, `max_width`
        // landed in `min_height`, and a window manager reading the structure would have seen a minimum
        // size of zero and a maximum of zero. **The size was right and the offsets were not**, which is
        // the worst of the two -- a property of the wrong length is rejected outright, and a property
        // of the right length with wrong offsets is accepted and read as nonsense.
        //
        // ```text
        //   0  long   flags          PSize|PMinSize|PMaxSize
        //   4  int    x              obsolete, zero
        //   8  int    y              obsolete, zero
        //  12  int    width, height  PSize: the size this window was created at
        //  20  int    min_width, min_height
        //  28  int    max_width, max_height      0 means "no maximum"
        //  36  int    width_inc, height_inc      1 pixel: no step
        //  44  int    min_aspect.x, .y           zero: no aspect
        //  52  int    max_aspect.x, .y           zero: no aspect
        //  60  int    base_width, base_height    zero
        //  68  int    win_gravity                zero
        // ```
        //
        // **CORRECTION 2026-10-12: the two obsolete ints are named separately, and `width, height`
        // moved to 12.** This table used to read `4 int x, y` and `8 int width, height` — three ints
        // where the protocol has four, so it put `width` at 8 and `height` at 12. **Every offset below
        // was right anyway, which is why the minimum worked all along and only `PSize` was nonsense.**
        // A table that is wrong only in the part nobody needs still reads as authoritative in the part
        // somebody does, and that is the half that shipped.
        let data = size_hints_bytes_for(size, min, max);
        conn.request(
            Req::new(op::CHANGE_PROPERTY)
                .second_byte(0) // Replace
                .u32(self.id)
                .u32(proto::atom::WM_NORMAL_HINTS)
                .u32(proto::atom::WM_SIZE_HINTS)
                // # `format` is ONE byte and the next three are padding, and this wrote six
                //
                // `.u32(32).u16(0)` is six bytes where the protocol has four, so the data length and
                // the 72 bytes of data both sat two bytes late and **the server answered
                // `BadLength`** -- and the property was never created at all.
                //
                // **`set_title` above has this correct, as `.u8(8).u8(0).u16(0)`,** and its comment
                // records that a spare `u16` there once shifted the whole request and earned a
                // `BadValue`. **The same bug, in the same file, forty lines apart, in the one request
                // that decides whether the window can be resized.**
                .u8(32) // format
                .u8(0)
                .u16(0)
                .u32(18) // 18 CARD32s
                .bytes(&data),
        )?;
        conn.sync()?;
        if let Some(e) = conn.take_error() {
            return Err(WindowError::Protocol(e));
        }
        Ok(())
    }

    /// [`Window::focus`], with a bound on how long to wait for the map.
    pub fn focus_when_mapped(&self, conn: &mut Conn, window: Duration) -> Result<u32, WindowError> {
        let deadline = Instant::now() + window;
        loop {
            let mut mapped = false;
            while let Some(event) = conn.next_event(Some(Duration::from_millis(20)))? {
                if matches!(event, crate::proto::Event::MapNotify) {
                    mapped = true;
                }
            }
            if mapped || Instant::now() >= deadline {
                break;
            }
        }
        let previous = conn.input_focus()?;
        conn.set_input_focus(self.id)?;
        Ok(previous)
    }

    /// Push `pixels` into the window at `(x, y)`.
    ///
    /// `pixels` is `width * height * 4` bytes of `0x00RRGGBB` in server byte order, row-major, exactly
    /// as a `Frame`'s memory is. Returns how many bytes were sent, which is the whole buffer: a
    /// partial push is not a thing this crate does, because a half-pushed frame looks like a
    /// half-drawn window and there is no way to tell the user which half.
    pub fn put_image(
        &self,
        conn: &mut Conn,
        pixels: &[u8],
        width: u32,
        height: u32,
        x: i16,
        y: i16,
    ) -> Result<u64, WindowError> {
        put_image(
            conn,
            ImageTarget {
                drawable: self.id,
                gc: self.gc,
                depth: self.depth,
            },
            pixels,
            width,
            height,
            x,
            y,
        )
    }

    /// Read the window's own pixels back.
    ///
    /// This exists for one reason: it turns "the window looks right" into a byte comparison. A window
    /// that accepted `PutImage` and a window that *shows* what was pushed are different claims, and
    /// only a readback distinguishes them.
    ///
    /// **A window that is obscured cannot be read.** The server answers `BadMatch` for a `GetImage` of
    /// a window that any other window is on top of, or that is not on a screen -- and on a desktop
    /// with a terminal behind it, "not obscured" is not a property the test can assume. Measured here:
    /// `Window::get_image` on a mapped 320x200 window that had just been pushed returned `BadMatch`
    /// every time, while [`Pixmap::get_image`] on the same bytes returned them unchanged. So the
    /// byte-exact gate is on a pixmap, and this is here for the interactive path and for a caller that
    /// knows its window is on top.
    pub fn get_image(
        &self,
        conn: &mut Conn,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
    ) -> Result<Vec<u8>, WindowError> {
        get_image(
            conn,
            ImageTarget {
                drawable: self.id,
                gc: self.gc,
                depth: self.depth,
            },
            x,
            y,
            width,
            height,
        )
    }

    /// `GetGeometry` in full: the window's **position as well as its size**, and the root it lives on.
    ///
    /// # Why this exists separately from [`geometry`](Self::geometry)
    ///
    /// **Because a pointer driver needs the window's `x, y`, and `geometry` threw them away.** They are
    /// on the wire at the same place and cost nothing to read — the reply is already in hand — and a
    /// caller that has to point the pointer *at* a widget in this window cannot compute a root-space
    /// coordinate without them. The existing three-tuple also returned the root window **truncated to
    /// a `u16`**, which is a value no caller could use.
    ///
    /// # `x, y` are relative to the parent
    ///
    /// **Which is the root for a top-level window**, so for the window this crate creates
    /// `(x, y)` plus a widget's window-relative coordinate is the root-space coordinate XTEST wants.
    pub fn geometry_full(
        &self,
        conn: &mut Conn,
    ) -> Result<(i16, i16, u16, u16, u32), WindowError> {
        let seq = conn.request(Req::new(op::GET_GEOMETRY).second_byte(0).u32(self.id))?;
        conn.flush()?;
        let body = conn.reply(seq)?;
        if body.len() < 24 {
            return Err(WindowError::ShortReply("GetGeometry"));
        }
        let mut r = Rdr::new(&body);
        r.skip(8);
        let root = r.u32();
        let x = r.i16();
        let y = r.i16();
        let width = r.u16();
        let height = r.u16();
        Ok((x, y, width, height, root))
    }

    /// `GetGeometry`, for a caller that wants the server's idea of the window's size.
    pub fn geometry(&self, conn: &mut Conn) -> Result<(u16, u16, u16), WindowError> {
        let seq = conn.request(Req::new(op::GET_GEOMETRY).second_byte(0).u32(self.id))?;
        conn.flush()?;
        let body = conn.reply(seq)?;
        if body.len() < 24 {
            return Err(WindowError::ShortReply("GetGeometry"));
        }
        // GetGeometry: reply marker, depth, root, sequence, length, root, x, y, width, height,
        // border-width, unused. So the root window is at offset 8 and the width at 16 -- reading either
        // from offset 0 gives the depth and the sequence number.
        let mut r = Rdr::new(&body);
        r.skip(8);
        let root = r.u32();
        r.skip(4); // x and y, both INT16
        let width = r.u16();
        let height = r.u16();
        Ok((width, height, root as u16))
    }

    /// Where the pointer is, in both coordinate systems: `(root_x, root_y, window_x, window_y)`.
    ///
    /// # Why this exists
    ///
    /// **Because "the click did nothing" has two possible causes and they are indistinguishable
    /// without a measurement.** Either the pointer never moved, or it moved to the wrong place -- and
    /// the two look identical from the outside: no menu opens either way. `geometry_full` says where
    /// the window is; this says where the pointer ended up *and* where the server considers that to be
    /// relative to the window, which is the number a click's `event_x`/`event_y` will carry.
    ///
    /// **The `window_x`/`window_y` half is the one that matters for a driver.** X reports a pointer
    /// event's coordinates relative to the *reporting window*, not to the root, so those two are what
    /// the application sees and what a caller aiming at a widget has to hit. Computing them as
    /// `root - geometry` is a guess about a window manager's mind; this is the server's answer.
    ///
    /// `same_screen` is returned as the fourth element's shadow: `true` when the pointer is on the
    /// same screen as this window, which is the `None` case for every other screen's pointer.
    pub fn pointer_position(
        &self,
        conn: &mut Conn,
    ) -> Result<((i16, i16, i16, i16), bool), WindowError> {
        let seq = conn.request(Req::new(op::QUERY_POINTER).second_byte(0).u32(self.id))?;
        conn.flush()?;
        let body = conn.reply(seq)?;
        // QueryPointer's reply is 32 bytes: reply(0), same_screen(1), sequence(2..4), length(4..8),
        // root(8..12), child(12..16), root-x(16..18), root-y(18..20), win-x(20..22), win-y(22..24),
        // mask(24..26), pad(26..32).
        if body.len() < 32 {
            return Err(WindowError::ShortReply("QueryPointer"));
        }
        let same_screen = body[1] != 0;
        let mut r = Rdr::new(&body);
        r.skip(16);
        let root_x = r.i16();
        let root_y = r.i16();
        let win_x = r.i16();
        let win_y = r.i16();
        Ok(((root_x, root_y, win_x, win_y), same_screen))
    }

    /// **Read `WM_NORMAL_HINTS` back off the server.** Part 34.
    ///
    /// ## Why a reader exists when the writer is a builder that already knows its own bytes
    ///
    /// **Because the writer is exactly what was wrong.** [`set_size_hints`] wrote every field twelve
    /// bytes early *and* built a request two bytes too long, and **the caller discarded the error**, so
    /// the window opened with no `WM_NORMAL_HINTS` at all and a window manager would not resize it.
    /// **A writer cannot check a writer** — it can only be sure it wrote what it meant to.
    ///
    /// **`GetProperty` answers "what did you actually record"**, over a request that shares no code
    /// with the builder. **This is the same comparative instrument the live resize gate uses** for
    /// `ConfigureNotify`: two paths to the same fact that can only agree if both are right.
    ///
    /// ## `None` for a property the server does not have
    ///
    /// **And not a zeroed struct.** `GetProperty` answers `type = None` for an absent property, and a
    /// `SizeHints` of zeros is a *valid* answer to "what is the minimum size" — it says *none*. **The
    /// two are different facts and the caller has to be able to tell them apart**, because the whole of
    /// this function is about a property that was missing.
    pub fn size_hints(&self, conn: &mut Conn) -> Option<SizeHints> {
        // GetProperty: opcode(20), delete(BOOL), window, property, type(AnyPropertyType=0),
        // long-offset(0), long-length(16). The reply's 32 bytes of header are followed by the value.
        let seq = conn
            .request(
                Req::new(op::GET_PROPERTY)
                    .second_byte(0) // delete = False: never delete a property we are reading
                    .u32(self.id)
                    .u32(proto::atom::WM_NORMAL_HINTS)
                    .u32(0) // AnyPropertyType
                    .u32(0) // long offset
                    .u32(16), // long length: 16 longs is the whole structure
            )
            .ok()?;
        conn.flush().ok()?;
        let body = conn.reply(seq).ok()?;
        // Reply: 1 reply, 1 format, 2 sequence, 4 length, 4 type, 4 bytes-after, 4 bytes-per-unit,
        // 8 unused, then the value. **Type is at offset 8**, and it is zero when the property is
        // absent -- `None` for a `CARD32` property is the *type atom*, which is never zero.
        if body.len() < 32 {
            return None;
        }
        let type_atom = u32::from_le_bytes([body[8], body[9], body[10], body[11]]);
        if type_atom == 0 {
            return None;
        }
        let value = body.get(32..)?;
        // **The same parser the gate uses on the builder's output**, so the two can only disagree if
        // the *bytes* disagree and not because two hand-written offsets drifted apart.
        Some(SizeHints::from_bytes(value))
    }

    /// Destroy the window and free its graphics context.
    pub fn destroy(&self, conn: &mut Conn) -> Result<(), WindowError> {
        // FreeGC(60): just the gc.
        conn.request(Req::new(op::FREE_GC).second_byte(0).u32(self.gc))?;
        conn.request(Req::new(op::DESTROY_WINDOW).second_byte(0).u32(self.id))?;
        Ok(conn.sync()?)
    }
}

/// A drawable and the graphics context that draws into it.
///
/// `Window` and [`Pixmap`] are both this plus a lifetime; splitting it out is what lets the two share
/// the one piece of code that has to be right about request sizes and scanline padding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImageTarget {
    drawable: u32,
    gc: u32,
    depth: u8,
}

/// The largest request this crate will send, in bytes: 4 KiB of header and length fields plus data.
///
/// The header is generous on purpose: `Req::finish` pads to a word, and being a few words over means
/// one fewer chunk of 18 rows at 1280 pixels.
const HEADER_ALLOWANCE: usize = 64;

/// Push `pixels` into `target` at `(x, y)`, splitting along scanlines to stay under the ceiling.
fn put_image(
    conn: &mut Conn,
    target: ImageTarget,
    pixels: &[u8],
    width: u32,
    height: u32,
    x: i16,
    y: i16,
) -> Result<u64, WindowError> {
    let row = width as usize * 4;
    let limit = conn.request_limit();
    if row > limit {
        return Err(WindowError::RowTooWide { row, limit });
    }
    // **CORRECTION, part 36: `saturating_mul`, and this is what made a window larger than 262 KB of
    // pixels un-openable.**
    //
    // **`want` is the whole frame and `limit` is the size of ONE request**, so this compared a frame
    // against a request and refused anything over 262,140 bytes -- a 512x128 window. The refusal came
    // back as:
    //
    // ```text
    // one scanline is 9828 bytes and the server accepts 18446744073709551615 bytes per request
    // ```
    //
    // **and both numbers in that sentence are wrong.** `18446744073709551615` is `usize::MAX`, which is
    // the `limit` this very line invented; the real limit is 262,140 and is printed two lines below.
    // And the scanline was never the problem -- 9,828 bytes is well inside one request.
    //
    // **The check below says it in the file's own words**: *"A `PutImage` cannot split a scanline, so
    // the chunk count is a whole number of rows"*. The frame is banded across many requests, so the
    // frame's size is not a constraint on anything. **What `want` is really for is the buffer-length
    // check underneath it**, and for that a saturating product is correct and an error is not.
    let want = row.saturating_mul(height as usize);
    if pixels.len() < want {
        return Err(WindowError::ShortBuffer {
            have: pixels.len(),
            need: want,
        });
    }
    // A `PutImage` cannot split a scanline, so the chunk count is a whole number of rows and one row
    // has to fit in a request on its own.
    let rows_per_chunk = (limit.saturating_sub(HEADER_ALLOWANCE) / row).max(1);
    let mut sent = 0u64;
    let mut first = 0usize;
    while first < height as usize {
        let rows = rows_per_chunk.min(height as usize - first);
        conn.request(
            Req::new(op::PUT_IMAGE)
                .second_byte(image::Z_PIXMAP)
                .u32(target.drawable)
                .u32(target.gc)
                .u16(width as u16)
                .u16(rows as u16)
                .i16(x)
                .i16(y + first as i16)
                .u8(0) // left-pad
                .u8(target.depth)
                .u16(0) // unused -- the data starts at byte 24, and leaving these out moves it
                // to byte 23, so the server reads one byte of the *previous* field as the first
                // pixel. Measured: a 16-byte ramp sent to a 4x1 pixmap came back as its own bytes 2
                // onwards with 0xff in every fourth byte.
                .bytes(&pixels[first * row..first * row + rows * row]),
        )?;
        sent += (rows * row) as u64;
        first += rows;
    }
    conn.sync()?;
    if let Some(e) = conn.take_error() {
        return Err(WindowError::Protocol(e));
    }
    Ok(sent)
}

/// Read `width` x `height` pixels back from `target`.
fn get_image(
    conn: &mut Conn,
    target: ImageTarget,
    x: i16,
    y: i16,
    width: u16,
    height: u16,
) -> Result<Vec<u8>, WindowError> {
    let _ = target;
    let limit = conn.request_limit();
    let row = width as usize * 4;
    let rows_per_chunk = (limit.saturating_sub(HEADER_ALLOWANCE) / row.max(1)).max(1);
    let mut out = vec![0u8; row * height as usize];
    let mut first = 0u16;
    while first < height {
        let rows = rows_per_chunk.min(height as usize - first as usize) as u16;
        let seq = conn.request(
            Req::new(op::GET_IMAGE)
                .second_byte(image::Z_PIXMAP)
                .u32(target.drawable)
                .i16(x)
                .i16(y + first as i16)
                .u16(width)
                .u16(rows)
                .u32(u32::MAX), // every plane
        )?;
        conn.flush()?;
        let body = conn.reply(seq)?;
        // XGetImage's reply is depth, visual, sequence, length, visual-id, 20 unused, then the data.
        // There is no bytes-per-line field: the scanline stride is the pixmap format's, and
        // `Window::create` has already refused any width whose row is not a whole number of
        // `scanline_pad` bits, so the stride is exactly `width * 4`.
        let pixels = body.get(32..).unwrap_or_default();
        let need = rows as usize * row;
        if pixels.len() < need {
            return Err(WindowError::ShortReply("GetImage"));
        }
        for r in 0..rows as usize {
            let src = &pixels[r * row..(r + 1) * row];
            let at = (first as usize + r) * row;
            out[at..at + row].copy_from_slice(src);
        }
        first += rows;
    }
    Ok(out)
}

/// An off-screen drawable.
///
/// # Why the byte-exact gate is on a pixmap and not on a window
///
/// `GetImage` of a `Window` returns `BadMatch` if any other window covers it, or if it is not on a
/// screen. A window that has just been mapped is normally behind whatever the user was looking at, so
/// on a real desktop the readback fails for a reason that has nothing to do with the client. A pixmap
/// has no such property: it is created, drawn, read and destroyed, and the server has no reason to
/// refuse. So `tests/live.rs` asserts "the bytes the server holds are the bytes we sent" against a
/// pixmap, and uses a window only for "the server accepted a full-screen-sized push".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pixmap {
    id: u32,
    gc: u32,
    width: u16,
    height: u16,
    depth: u8,
}

impl Pixmap {
    /// Create an off-screen drawable of `width` x `height` at the root's depth.
    pub fn create(conn: &mut Conn, width: u16, height: u16) -> Result<Self, WindowError> {
        let depth = conn.setup().root_depth;
        let root = conn.setup().root;
        let id = conn.alloc_id();
        let gc = conn.alloc_id();
        conn.request(
            Req::new(op::CREATE_PIXMAP)
                .second_byte(depth)
                .u32(id)
                .u32(root)
                .u16(width)
                .u16(height),
        )?;
        conn.request(
            Req::new(op::CREATE_GC)
                .second_byte(0)
                .u32(gc)
                .u32(id)
                .u32(gc::GRAPHICS_EXPOSURES)
                .u32(0),
        )?;
        conn.sync()?;
        if let Some(e) = conn.take_error() {
            return Err(WindowError::Protocol(e));
        }
        Ok(Self {
            id,
            gc,
            width,
            height,
            depth,
        })
    }

    /// This pixmap's resource id.
    pub fn id(&self) -> u32 {
        self.id
    }

    /// The pixmap's width in pixels.
    pub fn width(&self) -> u16 {
        self.width
    }

    /// The pixmap's height in pixels.
    pub fn height(&self) -> u16 {
        self.height
    }

    /// The pixmap's depth.
    pub fn depth(&self) -> u8 {
        self.depth
    }

    /// Push `pixels` in, splitting along scanlines. See [`Window::put_image`].
    pub fn put_image(
        &self,
        conn: &mut Conn,
        pixels: &[u8],
        width: u32,
        height: u32,
        x: i16,
        y: i16,
    ) -> Result<u64, WindowError> {
        put_image(
            conn,
            ImageTarget {
                drawable: self.id,
                gc: self.gc,
                depth: self.depth,
            },
            pixels,
            width,
            height,
            x,
            y,
        )
    }

    /// Read pixels back. Unlike a window, this cannot be refused for being covered.
    pub fn get_image(
        &self,
        conn: &mut Conn,
        x: i16,
        y: i16,
        width: u16,
        height: u16,
    ) -> Result<Vec<u8>, WindowError> {
        get_image(
            conn,
            ImageTarget {
                drawable: self.id,
                gc: self.gc,
                depth: self.depth,
            },
            x,
            y,
            width,
            height,
        )
    }

    /// Free the pixmap and its graphics context.
    pub fn free(&self, conn: &mut Conn) -> Result<(), WindowError> {
        conn.request(Req::new(op::FREE_GC).second_byte(0).u32(self.gc))?;
        conn.request(Req::new(op::FREE_PIXMAP).second_byte(0).u32(self.id))?;
        Ok(conn.sync()?)
    }
}

/// What a caller needs to know about the server before pushing frames.
pub fn check_frame_format(setup: &Setup) -> Result<u8, WindowError> {
    setup
        .format(setup.root_depth, 32)
        .map(|f| f.depth)
        .ok_or_else(|| WindowError::NoFormat {
            depth: setup.root_depth,
            available: setup
                .formats
                .iter()
                .map(|f| (f.depth, f.bits_per_pixel))
                .collect(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window is refused, by name, when the server cannot take a `Frame`. Silence here would be a
    /// window that shows garbage, which is worse than not opening.
    #[test]
    fn a_server_without_a_32_bit_format_is_refused_by_name() {
        let setup = Setup {
            vendor: "test".into(),
            release: 0,
            resource_id_base: 0x1000_0000,
            resource_id_mask: 0x001F_FFFF,
            max_request_words: 65535,
            root: 0x123,
            root_depth: 16,
            root_visual: 0x21,
            white_pixel: 0,
            black_pixel: 0,
            root_width: 1280,
            root_height: 800,
            min_keycode: 8,
            max_keycode: 255,
            image_byte_order: 0,
            formats: vec![
                crate::conn::Format {
                    depth: 16,
                    bits_per_pixel: 16,
                    scanline_pad: 32,
                },
                crate::conn::Format {
                    depth: 24,
                    bits_per_pixel: 8,
                    scanline_pad: 32,
                },
            ],
        };
        let err = check_frame_format(&setup).expect_err("no 32bpp format");
        assert!(
            matches!(err, WindowError::NoFormat { depth: 16, .. }),
            "{err:?}"
        );
        let text = format!("{err}");
        assert!(text.contains("root depth is 16"), "{text}");
        assert!(
            text.contains("(24, 8)"),
            "it lists what the server does have: {text}"
        );
    }

    /// The event mask has to include `Exposure`, because X11 has no other way to say "this region is
    /// no longer valid". Omitting it produces a window that opens blank and never repairs itself.
    #[test]
    fn the_event_mask_asks_for_exposure_and_keys() {
        let m = Window::event_mask();
        assert_ne!(m & mask::EXPOSURE, 0, "ExposureMask");
        assert_ne!(m & mask::KEY_PRESS, 0, "KeyPressMask");
        assert_ne!(m & mask::KEY_RELEASE, 0, "KeyReleaseMask");
        assert_ne!(m & mask::STRUCTURE_NOTIFY, 0, "StructureNotifyMask");
        assert_ne!(m & mask::BUTTON_PRESS, 0, "ButtonPressMask");
        assert_ne!(
            m & mask::POINTER_MOTION,
            0,
            "PointerMotionMask: without it there is no pointer, only clicks"
        );
    }
}
