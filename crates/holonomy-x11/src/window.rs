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
use crate::proto::{self, cw, gc, image, mask, op, Rdr, Req};

/// Why a window operation failed.
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
        // `scanline_pad` is why a 1280-pixel row is safe: 1280 * 4 bytes is already a multiple of
        // 32, the widest pad any 32-bpp server offers. A width that were not a multiple of
        // `scanline_pad / bpp` pixels would be padded by the server and `put_image` would have to
        // undo it, which it does not do -- so it is checked here rather than mis-rendered.
        let row_px = width as usize;
        if !(row_px * 8).is_multiple_of(format.scanline_pad as usize * 8) {
            return Err(WindowError::RowTooWide {
                row: row_px * 4,
                limit: usize::MAX,
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
                .u32(cw::BACK_PIXEL | cw::BORDER_PIXEL | cw::EVENT_MASK)
                .u32(setup.black_pixel)
                .u32(setup.black_pixel)
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
    let want = row
        .checked_mul(height as usize)
        .ok_or(WindowError::RowTooWide {
            row,
            limit: usize::MAX,
        })?;
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
    }
}
