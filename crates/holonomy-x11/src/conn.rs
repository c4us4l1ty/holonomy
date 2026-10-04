//! The connection: the socket, the handshake, the request queue, and the event stream.
//!
//! # One socket, two directions, and why reads are non-blocking
//!
//! The window's socket carries requests out and events in, and the two must not deadlock. If a read
//! blocked while the server was busy writing events, the client would stop consuming them and the
//! server's 256-deep event queue would fill, at which point the *server* blocks and stops answering
//! the requests we are waiting on. So after the handshake the socket is non-blocking and reads go
//! through [`libc::poll`] with a deadline, and every wait has a bound.
//!
//! # Errors are events, not failures
//!
//! An X11 error names one bad request; the connection stays usable. Treating the first one as fatal
//! would turn a wrong argument into a destroyed window, so they are collected and the caller decides.
//! [`Conn::take_error`] pops the oldest. Nothing here panics on a protocol error.
//!
//! # The request ceiling is enforced, not assumed
//!
//! Without `BIG-REQUESTS` a request may be at most `maximum-request-length` words (262,140 bytes on
//! every server that does not advertise the extension). [`Conn::request`] refuses an over-long request
//! instead of letting the server truncate it -- a truncated request is a *desynchronised* stream, the
//! worst failure this crate has, so it is checked where it can be seen.
//!
//! [`crate::window::Window::put_image`] is the caller that matters: it splits a frame along scanlines
//! to stay under the ceiling, which is why a 1280x800 frame can be pushed without `BIG-REQUESTS`.

use std::collections::VecDeque;
use std::fmt;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::time::{Duration, Instant};

use crate::auth::{self, AuthError};
use crate::proto::{self, op, Event, ProtocolError, Rdr, Req, MAX_REQUEST_BYTES};

/// How long the connection waits for the setup reply before giving up.
///
/// The server answers a setup request from its accept path, so a correct one replies in under a
/// millisecond. Five seconds is not a latency budget; it is the point at which the display is not
/// there at all and a hang is worse than an error.
const SETUP_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the client waits for the server's post-setup greeting before deciding there was none.
const GREETING_WINDOW: Duration = Duration::from_millis(60);

/// How many bytes of the greeting are kept for diagnostics.
const GREETING_KEEP: usize = 8;

/// The first byte of the post-setup greeting, and the length of the whole thing.
const GREETING_BYTE: u8 = 0xFF;
const GREETING_LEN: usize = 8;

/// `KeyPress`, the first of the four event codes whose size is measured rather than assumed.
const FIRST_KEY_OR_BUTTON: u8 = proto::event::KEY_PRESS;
/// `ButtonRelease`, the last of them.
const LAST_KEY_OR_BUTTON: u8 = proto::event::BUTTON_RELEASE;

/// How many waits a partly-delivered packet gets before the reader concludes it is out of step.
///
/// Three, because a socket that has delivered one byte of a packet will deliver the rest within
/// microseconds; three waits of a caller's deadline is already generous. Past this the reader drops a
/// byte and carries on, which loses at most one event per occurrence and never wedges the loop.
const STALL_LIMIT: u32 = 3;

/// How many bytes key and button events occupy unless [`Conn::set_key_event_bytes`] says otherwise.
///
/// 32, measured on this host: a synthesised tap arrives as 64 bytes, two 24-byte events with eight bytes
/// of padding after each. The protocol says 24, and reading 24 on this server desynchronises the stream
/// on the first keystroke -- see [`Conn::key_event_size`].
pub const KEY_EVENT_BYTES_DEFAULT: usize = 32;

/// Where a reply's own fields start: 1 for the reply marker, 1 for the second byte, 2 for the sequence
/// number, 4 for the extra-data length.
///
/// **Every reply parser in this crate starts here.** Reading one of these layouts from offset 0 gives
/// answers that look plausible: `QueryExtension`'s `present` reads the reply marker (always 1, so every
/// extension looks present) and its `major-opcode` reads the `unused` byte (always 0, so no extension
/// can be used). Measured: `query_extension("XTEST")` reported `present = true, major = 0`, and the
/// `first-event` it reported was the low byte of the sequence number.
const REPLY_FIELDS: usize = 8;

/// Whether to log every request and every packet to stderr, set with `HOLONOMY_X11_TRACE=1`.
///
/// This exists because a protocol client that is wrong is wrong *quietly*: a mistimed sequence number
/// or a field in the wrong byte produces a server that answers with something else, and the only way
/// to tell "the server is slow" from "my request was malformed" is to see both sides. Every bug in
/// this crate's first live run was found by reading this trace, and none of them would have been
/// visible from the API alone.
fn tracing() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("HOLONOMY_X11_TRACE").as_deref() == Ok("1"))
}

/// A pixmap format the server supports, from the handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    /// Bits of colour per pixel.
    pub depth: u8,
    /// Bits per pixel in memory.
    pub bits_per_pixel: u8,
    /// Scanline padding, in bits.
    pub scanline_pad: u8,
}

/// What the server said about itself in the handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setup {
    /// The server's `vendor` string. Xwayland reports `The X.Org Foundation`.
    pub vendor: String,
    /// Protocol release number.
    pub release: u32,
    /// The first resource id this client may use.
    pub resource_id_base: u32,
    /// A mask over the client's resource id range.
    pub resource_id_mask: u32,
    /// The longest request, in 4-byte words.
    pub max_request_words: u32,
    /// The root window of the first screen.
    pub root: u32,
    /// The root's depth.
    pub root_depth: u8,
    /// The root's visual.
    pub root_visual: u32,
    /// The root's `white-pixel`, which on a compositor's root window is usually 0.
    pub white_pixel: u32,
    /// The root's `black-pixel`.
    pub black_pixel: u32,
    /// The root window's width in pixels.
    pub root_width: u16,
    /// The root window's height in pixels.
    pub root_height: u16,
    /// The lowest keycode the keyboard mapping covers.
    pub min_keycode: u8,
    /// The highest keycode the keyboard mapping covers.
    pub max_keycode: u8,
    /// `LSBFirst` (0) or `MSBFirst` (1). Everything this crate writes is little-endian, so a server
    /// reporting `MSBFirst` is a server this client cannot talk to and [`Conn::connect`] refuses it.
    pub image_byte_order: u8,
    /// The pixmap formats the server supports.
    pub formats: Vec<Format>,
}

impl Setup {
    /// The format for `depth` at `bpp` bits per pixel, if the server has one.
    pub fn format(&self, depth: u8, bpp: u8) -> Option<Format> {
        self.formats
            .iter()
            .copied()
            .find(|f| f.depth == depth && f.bits_per_pixel == bpp)
    }
}

/// Why a connection could not be made, or could not be kept.
#[derive(Debug)]
pub enum ConnError {
    /// The socket could not be opened at either the filesystem or the abstract path.
    Socket {
        /// The display that was asked for.
        display: u32,
        /// The filesystem path tried.
        path: String,
        /// The `errno` from the last attempt.
        errno: i32,
    },
    /// No usable cookie.
    Auth(AuthError),
    /// The server refused the connection outright.
    Refused {
        /// The reason string, without its trailing NUL.
        reason: String,
    },
    /// The handshake said something this client cannot use.
    Unsupported(String),
    /// A request was longer than the server accepts.
    TooLarge {
        /// The request's byte length.
        request: usize,
        /// The server's ceiling.
        limit: usize,
    },
    /// The server rejected the request this reply was meant for.
    Protocol(ProtocolError),
    /// A read or write failed.
    Io(std::io::Error),
    /// A wait ran out with no data.
    TimedOut {
        /// What was being waited for.
        what: &'static str,
    },
}

impl fmt::Display for ConnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Socket {
                display,
                path,
                errno,
            } => write!(
                f,
                "cannot reach display :{display}: neither {path} nor its abstract socket exists \
                 (errno {errno}) -- is XWAYLAND running and is DISPLAY set?"
            ),
            Self::Auth(e) => write!(f, "{e}"),
            Self::Refused { reason } => {
                write!(f, "the X server refused the connection: {reason}")
            }
            Self::Unsupported(s) => write!(f, "unsupported X server: {s}"),
            Self::TooLarge { request, limit } => write!(
                f,
                "a {request}-byte request exceeds the server's {limit}-byte limit"
            ),
            Self::Protocol(e) => write!(f, "{e}"),
            Self::Io(e) => write!(f, "the X connection failed: {e}"),
            Self::TimedOut { what } => write!(f, "timed out waiting for {what}"),
        }
    }
}

impl std::error::Error for ConnError {}

impl From<std::io::Error> for ConnError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<AuthError> for ConnError {
    fn from(e: AuthError) -> Self {
        Self::Auth(e)
    }
}

/// One packet off the wire.
#[derive(Debug)]
enum Packet {
    /// A reply, already-stripped of its 32-byte header's type field but keeping the extra data.
    Reply {
        /// The low 16 bits of the request's sequence number.
        sequence: u16,
        /// Everything after the 8-byte header: 24 bytes of reply body plus the extra.
        body: Vec<u8>,
    },
    /// A protocol error, which is not fatal.
    Error(ProtocolError),
    /// An event.
    Event(Event),
}

/// A live connection to an X server.
#[derive(Debug)]
pub struct Conn {
    stream: UnixStream,
    setup: Setup,
    /// Sequence number of the next request.
    seq: u16,
    /// The next resource id to hand out.
    next_id: u32,
    /// Requests queued but not yet written.
    out: Vec<u8>,
    /// Bytes read from the socket and not yet consumed.
    rbuf: Vec<u8>,
    /// How much of `rbuf` has been consumed.
    rstart: usize,
    events: VecDeque<Event>,
    errors: VecDeque<ProtocolError>,
    /// The display this connection is to, for `describe`-style messages.
    display: u32,
    /// Bytes the server sent after the setup reply that no request asked for. See
    /// [`Conn::drain_greeting`].
    greeting: Option<(usize, [u8; GREETING_KEEP])>,
    /// How many bytes this server's key events occupy. See [`Conn::key_event_size`].
    key_event_bytes: usize,
    /// How many post-setup greetings have been discarded, including the one at connect.
    greetings: u32,
    /// Bytes still owed for the packet at the front of the read buffer.
    ///
    /// A short read is normal -- a socket delivers what it delivers -- and it must not be mistaken for
    /// "nothing to read", or the event loop spins. Measured: a packet that never completed made every
    /// `next_event` return immediately, and the developer window looped as fast as it could, painting a
    /// caret blink on every pass: 420,000 requests in twenty seconds.
    pending: usize,
    /// How many times the pending packet has gone unanswered, for the resynchronisation.
    stalls: u32,
    /// How many times the reader has had to drop a byte to get back in step.
    resyncs: u32,
}

impl Conn {
    /// Connect to `display` (`$DISPLAY` if `None`) and read the handshake.
    ///
    /// Both socket spellings are tried, in this order: `/tmp/.X11-unix/X<n>`, then the abstract
    /// namespace socket of the same name. The second is not a fallback for exotic setups -- it is what
    /// this host needs. Measured here: `/tmp/.X11-unix/X0` does not exist in this mount namespace at
    /// all (`ls` returns ENOENT) while `ss` shows `@/tmp/.X11-unix/X0` listening, and libX11 opens the
    /// display anyway because it tries the abstract socket too. A client that only tries the
    /// filesystem path fails on this machine, which is the whole reason both are here.
    pub fn connect(display: Option<&str>) -> Result<Self, ConnError> {
        let spec = display
            .map(str::to_string)
            .or_else(|| std::env::var("DISPLAY").ok())
            .unwrap_or_else(|| ":0".to_string());
        let number = crate::display_number(&spec).ok_or_else(|| {
            ConnError::Unsupported(format!(
                "DISPLAY={spec:?} names a host, and this client speaks only a local unix socket"
            ))
        })?;

        let path = format!("/tmp/.X11-unix/X{number}");
        let mut stream = Self::open_socket(number, &path)?;
        stream
            .set_read_timeout(Some(SETUP_TIMEOUT))
            .map_err(ConnError::Io)?;
        stream
            .set_write_timeout(Some(SETUP_TIMEOUT))
            .map_err(ConnError::Io)?;

        let cookie = match auth::default_path() {
            Some(p) => auth::cookie_for(&p, number)?,
            None => {
                return Err(ConnError::Auth(AuthError::Unreadable {
                    path: "$HOME/.Xauthority".into(),
                    errno: libc::ENOENT,
                }))
            }
        };

        // The handshake request is written by hand rather than through `Req`, because its 12-byte
        // fixed part is not a sequence of protocol fields -- the two lengths in it are *byte* counts
        // that are padded independently, which is the mistake the first probe made.
        let mut body = Vec::with_capacity(32 + cookie.len());
        body.extend_from_slice(&[0x6C, 0]); // little-endian, unused
        body.extend_from_slice(&11u16.to_le_bytes()); // protocol major
        body.extend_from_slice(&0u16.to_le_bytes()); // protocol minor
        body.extend_from_slice(&(auth::MIT_MAGIC_COOKIE_1.len() as u16).to_le_bytes());
        body.extend_from_slice(&(cookie.len() as u16).to_le_bytes());
        body.extend_from_slice(&0u16.to_le_bytes()); // unused
        body.extend_from_slice(auth::MIT_MAGIC_COOKIE_1.as_bytes());
        while body.len() % 4 != 0 {
            body.push(0);
        }
        body.extend_from_slice(&cookie);
        while body.len() % 4 != 0 {
            body.push(0);
        }
        stream.write_all(&body).map_err(ConnError::Io)?;

        let mut head = [0u8; 8];
        read_exact(&mut stream, &mut head)?;
        let mut r = Rdr::new(&head);
        let status = r.u8();
        let _unused = r.u8();
        let _major = r.u16();
        let _minor = r.u16();
        // The additional-data length is a CARD16, not a CARD32. Reading it as a u32 runs off the end
        // of the 8-byte header -- which is what `Rdr` is for: it panicked here rather than letting a
        // 65,540-byte length through.
        let extra_words = u32::from(r.u16());
        if status == 0 {
            // A refusal carries the reason length in the *second* byte and the reason itself counted
            // in words, which is a different packing from the success reply two fields away.
            let reason_len = head[1] as usize;
            let reason_words = u16::from_le_bytes([head[6], head[7]]) as usize;
            let mut reason = vec![0u8; reason_words * 4];
            read_exact(&mut stream, &mut reason)?;
            let reason = String::from_utf8_lossy(&reason[..reason_len.min(reason.len())])
                .trim_end_matches('\0')
                .to_string();
            return Err(ConnError::Refused { reason });
        }
        if status != 1 {
            return Err(ConnError::Unsupported(format!(
                "setup reply status {status}"
            )));
        }

        let mut rest = vec![0u8; extra_words as usize * 4 - 8];
        read_exact(&mut stream, &mut rest)?;
        let setup = Self::parse_setup(&rest)?;

        // From here on, nothing may block indefinitely.
        stream.set_nonblocking(true).map_err(ConnError::Io)?;
        let mut conn = Self {
            stream,
            setup,
            seq: 0,
            next_id: 0,
            out: Vec::with_capacity(4096),
            rbuf: Vec::with_capacity(4096),
            rstart: 0,
            events: VecDeque::with_capacity(64),
            errors: VecDeque::with_capacity(8),
            display: number,
            greeting: None,
            key_event_bytes: KEY_EVENT_BYTES_DEFAULT,
            greetings: 0,
            pending: 0,
            stalls: 0,
            resyncs: 0,
        };
        conn.next_id = conn.setup.resource_id_base.wrapping_add(1);
        // X.Org increments a client's sequence number *before* using it, so the first request a
        // client sends is sequence 1 and not 0. Measured here: sending NoOperation as 0 and
        // GetInputFocus as 1 produces replies carrying sequences 1 and 2 -- a client whose counter
        // starts at 0 matches neither and then hangs waiting for a reply that already went past.
        conn.seq = conn.seq.wrapping_add(1);
        // The greeting. See `Conn::drain_greeting`.
        let greeting = conn.drain_greeting(GREETING_WINDOW)?;
        conn.greetings = 1;
        conn.greeting = Some(greeting);
        Ok(conn)
    }

    /// Parse the setup reply's additional data.
    ///
    /// The layout is: release, id-base, id-mask, motion-buffer-size (16 bytes), the vendor and its
    /// pad, the maximum request length, the screen and format counts, six bytes of image format,
    /// the pixmap formats (8 bytes each), then the first `SCREEN`.
    /// Parse the setup reply's additional data.
    ///
    /// # The order is the X.Org implementation's, not the specification's listing
    ///
    /// The X11 protocol specification lists the additional data as release, id-base, id-mask,
    /// motion-buffer-size, *vendor length, vendor*, pad, maximum-request-length, screen and format
    /// counts, the six image-format bytes, the keycode range, four unused bytes, then the formats.
    ///
    /// `dix/dispatch.c`'s `SendConnectionSetup` does not do that. It writes the fourteen fixed bytes
    /// and *then* the vendor string, and every server in the field is built from that code -- Xorg,
    /// Xwayland, Xnest, Xdummy. Measured on this host, dumping the reply byte for byte
    /// (`cargo run -p holonomy-x11 --example dump_setup`):
    ///
    /// ```text
    /// 0000  75 39 bd 00 00 00 c0 00 ff ff 1f 00 00 01 00 00   release, id-base, id-mask, motion
    /// 0010  14 00 ff ff 01 07 00 00 20 20 08 ff 00 00 00 00   vendor-len 20, then the 14 fixed bytes:
    ///                                                        max-request 0xffff, 1 screen, 7 formats,
    ///                                                        LSBFirst, LSBFirst, unit 32, pad 32,
    ///                                                        min-keycode 8, max-keycode 255
    /// 0020  54 68 65 20 58 2e 4f 72 67 20 46 6f 75 6e ...   "The X.Org Foundation", 20 bytes
    /// 0034  01 01 20 00 00 00 00 00 | 04 08 20 00 00 00 00 00   the FORMATs: depth 1/bpp 1, 4/8, ...
    /// 005c  18 20 20 00 00 00 00 00                            depth 24, 32 bpp, pad 32
    /// 0064  20 20 20 00 00 00 00 00                            depth 32, 32 bpp
    /// 006c  ed 04 00 00 22 00 00 00 ...                        SCREEN: root 0x4ed, colormap 0x22
    /// ```
    ///
    /// Two things in that dump are the check: the root window lands at 0x6c, which is exactly
    /// 0x34 + 7 * 8 -- the end of the seven format records and no other offset -- and every one of
    /// the fourteen fixed bytes has a sane value. A parser written to the specification's order reads
    /// the vendor's first fourteen bytes as the fixed fields and produces a maximum-request-length of
    /// 0xffff from `MIT-MAGIC-C` and a min-keycode of 1, which is how this crate's first version
    /// failed: `Rdr` panicked reading past the end of the packet.
    fn parse_setup(body: &[u8]) -> Result<Setup, ConnError> {
        let mut r = Rdr::new(body);
        let release = r.u32();
        let resource_id_base = r.u32();
        let resource_id_mask = r.u32();
        let _motion_buffer = r.u32();
        let vendor_len = r.u16() as usize;
        let max_request_words = u32::from(r.u16());
        let _screens = r.u8();
        let nformats = r.u8() as usize;
        let image_byte_order = r.u8();
        r.skip(3); // bit order, scanline unit, scanline pad -- unused by this client
        let min_keycode = r.u8();
        let max_keycode = r.u8();
        r.skip(4);
        let vendor = String::from_utf8_lossy(r.bytes(vendor_len)).into_owned();
        r.skip((4 - vendor_len % 4) % 4);
        let mut formats = Vec::with_capacity(nformats);
        for _ in 0..nformats {
            // 8 bytes: depth, bits-per-pixel, scanline-pad, 5 unused.
            formats.push(Format {
                depth: r.u8(),
                bits_per_pixel: r.u8(),
                scanline_pad: r.u8(),
            });
            r.skip(5);
        }
        let root = r.u32();
        let _colormap = r.u32();
        let white_pixel = r.u32();
        let black_pixel = r.u32();
        let _input_masks = r.u32();
        let width = r.u16();
        let height = r.u16();
        let _mm_width = r.u16();
        let _mm_height = r.u16();
        let _min_maps = r.u16();
        let _max_maps = r.u16();
        let root_visual = r.u32();
        let _backing_stores = r.u8();
        let _save_unders = r.u8();
        let root_depth = r.u8();
        let _ndepths = r.u8();

        if image_byte_order != 0 {
            // Everything this crate writes is little-endian, because the request encoding is
            // little-endian by definition. A big-endian server would need every field swapped, and
            // silently sending the wrong byte order produces `BadLength` for every request.
            return Err(ConnError::Unsupported(format!(
                "image-byte-order {image_byte_order} (MSBFirst); this client is little-endian only"
            )));
        }
        if max_request_words == 0 {
            return Err(ConnError::Unsupported(
                "the server reported a maximum-request-length of zero".into(),
            ));
        }
        if width == 0 || height == 0 {
            return Err(ConnError::Unsupported(format!(
                "the server reported a {width}x{height} root window"
            )));
        }

        Ok(Setup {
            vendor,
            release,
            resource_id_base,
            resource_id_mask,
            max_request_words,
            root,
            root_depth,
            root_visual,
            white_pixel,
            black_pixel,
            min_keycode,
            max_keycode,
            image_byte_order,
            formats,
            root_width: width,
            root_height: height,
        })
    }

    fn open_socket(display: u32, path: &str) -> Result<UnixStream, ConnError> {
        match UnixStream::connect(path) {
            Ok(s) => Ok(s),
            Err(first) => {
                // An abstract socket has no filesystem entry at all, so `SocketAddr` is the only way to
                // name one. The name is the path *without* a leading NUL -- `from_abstract_name` adds
                // the NUL itself, and passing one in produces a socket at `\0\0/tmp/...`, which does
                // not exist and fails with the same ENOENT as the missing filesystem path. That is
                // exactly the failure the first version of this had, and it is why the error message
                // names both spellings.
                if let Ok(addr) = SocketAddr::from_abstract_name(path.as_bytes()) {
                    if let Ok(s) = UnixStream::connect_addr(&addr) {
                        return Ok(s);
                    }
                }
                Err(ConnError::Socket {
                    display,
                    path: path.to_string(),
                    errno: first.raw_os_error().unwrap_or(libc::EIO),
                })
            }
        }
    }

    /// Read and discard whatever the server sends before the first request is answered.
    ///
    /// # This host sends eight bytes nobody asked for
    ///
    /// Measured here, on Xwayland behind GNOME: the setup reply declares 10,284 bytes of additional
    /// data, and a walk through the formats, the screen and its seven depths and 420 visuals accounts
    /// for exactly 10,284 of them -- and then the server sends eight more, `ff 00 00 00 00 00 00 00`,
    /// once per connection, before any request has been sent. Run
    /// `cargo run -p holonomy-x11 --example dump_setup` to print the declared length, the walk, and
    /// the eight bytes.
    ///
    /// Nothing in the core protocol sends an unsolicited packet there, and eight bytes is not a packet
    /// length: core replies and errors are 32 bytes and events are 24, 28 or 32. A client that reads
    /// only the declared length leaves those eight bytes in the socket, and then every reply it parses
    /// begins eight bytes into a packet. That is what this crate's first live run did, and it
    /// presented as a hang rather than as garbage -- which is why the live gate has a test for
    /// something this boring.
    ///
    /// So they are drained, deliberately and visibly: [`Conn::greeting`] reports how many bytes were
    /// discarded and what they were. A server that sends none is the normal case elsewhere.
    fn drain_greeting(
        &mut self,
        window: Duration,
    ) -> Result<(usize, [u8; GREETING_KEEP]), ConnError> {
        let deadline = Instant::now() + window;
        let mut seen = 0usize;
        let mut first = [0u8; GREETING_KEEP];
        let mut chunk = [0u8; 64];
        loop {
            if Instant::now() >= deadline || seen >= 256 {
                break;
            }
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    return Err(ConnError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the X server closed the connection right after the handshake",
                    )))
                }
                Ok(n) => {
                    for (i, b) in chunk[..n].iter().enumerate() {
                        if i < GREETING_KEEP {
                            first[i] = *b;
                        }
                    }
                    seen += n;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    self.wait(false, deadline.saturating_duration_since(Instant::now()))?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(ConnError::Io(e)),
            }
        }
        Ok((seen, first))
    }

    /// How many times this connection had to drop a byte to get back in step with the server.
    ///
    /// Zero on a well-behaved connection. A non-zero count means some event was read at the wrong size,
    /// which on this host means an extension event this crate does not decode; the cost is one lost
    /// event, and it is counted rather than hidden.
    pub fn resyncs(&self) -> u32 {
        self.resyncs
    }

    /// How many post-setup greetings this connection has discarded.
    ///
    /// One at connect is normal. Two means the server sent the eight bytes later than the connect-time
    /// window allowed, which it does; the count is here so that is visible rather than inferred.
    pub fn greetings(&self) -> u32 {
        self.greetings
    }

    /// How many bytes this server's key events occupy.
    pub fn key_event_bytes(&self) -> usize {
        self.key_event_bytes
    }

    /// Say how many bytes this server's key and button events occupy.
    ///
    /// The default is [`KEY_EVENT_BYTES_DEFAULT`], which is what this host was measured at. A server that
    /// follows the protocol's 24 wants this called before the first key event arrives.
    pub fn set_key_event_bytes(&mut self, bytes: usize) {
        self.key_event_bytes = bytes;
    }

    /// How many bytes the server sent before any request was answered, and the first few of them.
    ///
    /// See [`Conn::drain_greeting`]. On this host it is 8, and a server that sends 0 is not broken.
    pub fn greeting(&self) -> Option<(usize, [u8; GREETING_KEEP])> {
        self.greeting
    }

    /// The socket, for a caller that needs the descriptor. Used by `examples/dump_reply.rs`.
    pub fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.stream.as_raw_fd()
    }

    /// What the server said about itself.
    pub fn setup(&self) -> &Setup {
        &self.setup
    }

    /// The display number this connection is to.
    pub fn display(&self) -> u32 {
        self.display
    }

    /// The longest request this server accepts, in bytes.
    pub fn request_limit(&self) -> usize {
        (self.setup.max_request_words as usize)
            .saturating_mul(4)
            .min(MAX_REQUEST_BYTES)
    }

    /// A fresh resource id from this client's range.
    pub fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1) & self.setup.resource_id_mask;
        id | self.setup.resource_id_base
    }

    /// Queue a request. Not written until [`Conn::flush`] or a syncing call.
    pub fn request(&mut self, req: Req) -> Result<u16, ConnError> {
        let bytes = req.finish(self.seq);
        if bytes.len() > self.request_limit() {
            return Err(ConnError::TooLarge {
                request: bytes.len(),
                limit: self.request_limit(),
            });
        }
        if tracing() {
            eprintln!(
                "x11 -> opcode {} sequence {} ({} bytes)",
                bytes[0],
                self.seq,
                bytes.len()
            );
        }
        self.out.extend_from_slice(&bytes);
        let used = self.seq;
        self.seq = self.seq.wrapping_add(1);
        Ok(used)
    }

    /// Write everything queued, in full, or fail.
    ///
    /// # This must not return early, and the first version did
    ///
    /// The socket is non-blocking and a `PutImage` chunk is 262,116 bytes, which is larger than the
    /// default `SO_SNDBUF` for an `AF_UNIX` socket on this host (measured: the write blocks after
    /// roughly 208 KB). So `write` returns `EAGAIN` partway through a request.
    ///
    /// The first version stashed the unwritten tail and returned `Ok(())`, on the theory that a later
    /// `flush` would pick it up. It does not arrive: the *next* thing to be written is a four-byte
    /// `GetInputFocus`, and the server is still waiting for the rest of the `PutImage` before it looks
    /// at anything. So the sync waited out its five-second timeout with a reply that was never going to
    /// come, and the symptom -- a timeout on a request the server had not received -- pointed at the
    /// wrong thing entirely.
    ///
    /// So this loop is the whole contract: every queued byte is written before it returns, or it is an
    /// error. Anything unwritten stays queued for a later attempt.
    pub fn flush(&mut self) -> Result<(), ConnError> {
        if self.out.is_empty() {
            return Ok(());
        }
        let deadline = Instant::now() + SETUP_TIMEOUT;
        let mut written = 0usize;
        while written < self.out.len() {
            match self.stream.write(&self.out[written..]) {
                Ok(0) => {
                    return Err(ConnError::Io(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "the X socket accepted no bytes",
                    )))
                }
                Ok(n) => written += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        // Keep what has not gone out; the caller may retry or fail.
                        self.out.drain(..written);
                        return Err(ConnError::TimedOut {
                            what: "the X socket to accept a request",
                        });
                    }
                    self.wait(true, deadline.saturating_duration_since(Instant::now()))?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    self.out.drain(..written);
                    return Err(ConnError::Io(e));
                }
            }
        }
        self.out.clear();
        Ok(())
    }

    /// Send `GetInputFocus` and read until its reply, queueing every event and error on the way.
    ///
    /// This is the only way this crate knows a request landed. X11 has no other synchronisation: a
    /// request with no reply is reported by an error *if and only if* it failed, and a request that
    /// succeeded leaves no trace. So anything that must not be silently dropped ends with a sync.
    ///
    /// # Why `GetInputFocus` and not `NoOperation`
    ///
    /// `NoOperation` is the obvious choice -- it is the shortest request there is, and its own byte 1
    /// is genuinely `unused`, so it is one of the few requests where the sequence number belongs there.
    /// It also sends **no reply**. That is in the specification, and this crate's `sync` used it for
    /// two days' worth of debugging on the strength of a comment that said its reply was "guaranteed".
    /// The result was a sync that waited out its timeout after every successful request, with nothing
    /// in the socket, which is indistinguishable from a server that had stopped answering.
    ///
    /// `GetInputFocus` is four bytes, has no arguments, and always replies -- the same four bytes that
    /// arrive in the trace as `opcode 43`.
    pub fn sync(&mut self) -> Result<(), ConnError> {
        let seq = self.request(Req::new(op::GET_INPUT_FOCUS))?;
        self.flush()?;
        let deadline = Instant::now() + SETUP_TIMEOUT;
        loop {
            match self.read_packet(Some(deadline))? {
                Some(Packet::Reply { sequence, .. }) if sequence == seq => {
                    // A reply is not a barrier for errors; see `drain_ready`.
                    self.drain_ready();
                    return Ok(());
                }
                Some(Packet::Error(e)) if e.sequence == seq => return Err(ConnError::Protocol(e)),
                Some(Packet::Reply { .. }) | None => {}
                Some(other) => self.queue(other),
            }
            if Instant::now() >= deadline {
                return Err(ConnError::TimedOut {
                    what: "a GetInputFocus reply",
                });
            }
        }
    }

    /// Take everything already in the buffer, without waiting.
    ///
    /// # A reply is not a barrier for errors
    ///
    /// X11 reports a failed request by sending an error *as well as* the replies to the requests that
    /// followed it, and the server makes no promise about which arrives first. Measured here: a
    /// `SendEvent` whose length field was one word too long was answered with `BadLength`, but the reply
    /// to the `GetInputFocus` after it arrived first, so the sync that followed the `SendEvent` saw its
    /// own reply, found no error waiting, and returned. The error turned up three requests later --
    /// during a full-screen frame push -- and *that* is what the window reported as the reason it failed.
    ///
    /// So after matching a reply this takes whatever is already readable. It cannot promise that an
    /// error still in flight will be seen here, which is why every request that must not be lost ends
    /// with a sync rather than trusting this alone.
    fn drain_ready(&mut self) {
        while let Ok(Some(packet)) = self.read_packet(None) {
            self.queue(packet);
        }
    }

    fn queue(&mut self, packet: Packet) {
        match packet {
            Packet::Event(e) => {
                if self.events.len() < 4096 {
                    self.events.push_back(e);
                }
            }
            Packet::Error(e) => {
                if self.errors.len() < 64 {
                    self.errors.push_back(e);
                }
            }
            // A reply nobody asked for. Dropped, because there is nothing to match it to and holding
            // it would only hide the next real reply.
            Packet::Reply { .. } => {}
        }
    }

    /// Pop the oldest queued event, waiting up to `timeout` for one if the queue is empty.
    pub fn next_event(&mut self, timeout: Option<Duration>) -> Result<Option<Event>, ConnError> {
        if let Some(e) = self.events.pop_front() {
            return Ok(Some(e));
        }
        let deadline = timeout.map(|t| Instant::now() + t);
        loop {
            match self.read_packet(deadline)? {
                Some(Packet::Event(e)) => return Ok(Some(e)),
                Some(other) => self.queue(other),
                None => {
                    // Nothing arrived and either there is no deadline or it has not passed.
                    return match deadline {
                        Some(d) if Instant::now() < d => Ok(None),
                        Some(_) => Ok(None),
                        None => Ok(None),
                    };
                }
            }
        }
    }

    /// Take the oldest protocol error, if the server reported one.
    pub fn take_error(&mut self) -> Option<ProtocolError> {
        self.errors.pop_front()
    }

    /// How many events are queued without a read.
    pub fn queued_events(&self) -> usize {
        self.events.len()
    }

    /// Close the connection.
    pub fn close(self) {}

    /// Read one packet, waiting until `deadline`.
    ///
    /// `None` means: return immediately if nothing is buffered. The read buffer is compacted only
    /// when it has been fully consumed, so the amortised cost of the event loop is one `memmove` per
    /// buffer's worth of events rather than one per event.
    fn read_packet(&mut self, deadline: Option<Instant>) -> Result<Option<Packet>, ConnError> {
        self.compact();
        let first = self.ensure(1, deadline)?;
        if !first {
            return Ok(None);
        }
        if self.pending > 0 {
            // A packet was started and not finished. Wait for the rest of it rather than reading the
            // next one, which is the difference between waiting and spinning.
            if !self.ensure(self.pending, deadline)? {
                self.stalls += 1;
                if self.stalls >= STALL_LIMIT {
                    // The bytes are never coming, so the stream is out of step with the server: some
                    // event was read at the wrong size. Resynchronising by dropping one byte is crude and
                    // it is also the only thing that works without knowing the size of an event this
                    // crate does not decode -- and it is bounded, so a client that gets it wrong loses
                    // events rather than wedging.
                    self.rstart += 1;
                    self.pending = 0;
                    self.stalls = 0;
                    self.resyncs += 1;
                    if tracing() {
                        eprintln!(
                            "x11    out of step; dropped a byte (resync number {})",
                            self.resyncs
                        );
                    }
                }
                return Ok(None);
            }
            self.stalls = 0;
        }
        // The greeting, wherever it turns up.
        //
        // [`Conn::drain_greeting`] catches the one that follows the handshake, but it is a *window*, not
        // a barrier: measured, the same eight bytes can arrive after the client has been busy creating a
        // window and interning atoms, which is what the developer window does. The first version of this
        // reader had no idea what to do with them -- `0xff` is not a core event code and not a plausible
        // extension code, so it asked for 32 bytes, never got them, and the window reported a failure on
        // its first keystroke. It is eight bytes of nothing and is recognised by that.
        if self.rbuf[self.rstart] == GREETING_BYTE {
            if !self.ensure(GREETING_LEN, deadline)? {
                return Ok(None);
            }
            self.rstart += GREETING_LEN;
            self.pending = 0;
            self.greetings += 1;
            if tracing() {
                eprintln!(
                    "x11    discarded a {GREETING_LEN}-byte greeting (number {})",
                    self.greetings
                );
            }
            return Ok(None);
        }

        let code = self.rbuf[self.rstart] & !proto::event::SEND_EVENT_FLAG;
        // 0 is an error and 1 is a reply, both 32 bytes; every other event has the size its core code
        // says.
        //
        // # Extension events are 32 bytes here, and that is a decision
        //
        // An event with a code of 64 or above belongs to an extension, and the protocol has two
        // conventions for those and no way to tell them apart from the code. A `GenericEvent` and
        // everything `XInput2` sends carries its length in byte 1, counted in 4-byte units; `MIT-SHM`'s
        // `ShmCompletionNotify` -- code 65 on this host -- carries a *drawable* there, and is 32 bytes.
        //
        // This crate tried the byte-1 rule and desynchronised on a real `ShmCompletionNotify` that this
        // client never asked for: the readback reported "the server sent 32 of a packet's bytes and then
        // stopped", and every event after it was garbage. So: unknown events are 32 bytes, and this is
        // recorded as a limitation rather than a bug -- a client that wants an extension's events
        // selects them, knows their convention, and sizes them itself. A word processor wants key,
        // button, exposure and structure events, all of which are core and all of which are fixed size.
        let want = match code {
            0 | 1 => 32,
            // Keys and buttons: 24 bytes by the protocol, and this host says otherwise.
            2..=5 => self.key_event_size(code),
            other => proto::event_size(other),
        };
        self.pending = want;
        if !self.ensure(want, deadline)? {
            // A short read, not a protocol failure: some bytes of a packet arrived and the rest did
            // not, before the deadline. The right answer is "nothing yet", so the loop comes back.
            //
            // The first version of this returned an error, and the developer window died on it: a few
            // bytes of an event arrived, the 125 ms the loop will wait elapsed, and the whole session
            // exited with "the X server sent 32 of a packet's bytes and then stopped". The server had
            // not stopped; the packet was late. A client that treats a short read as a broken
            // connection is a client that quits under load.
            return Ok(None);
        }
        let packet = self.rbuf[self.rstart..self.rstart + want].to_vec();
        self.rstart += want;
        self.pending = 0;
        self.stalls = 0;
        if tracing() {
            let first = packet[0];
            let what = match first {
                0 => "error",
                1 => "reply",
                other => holonomy_x11_event_name(other).unwrap_or("unknown"),
            };
            let head: Vec<String> = packet.iter().take(6).map(|b| format!("{b:02x}")).collect();
            eprintln!(
                "x11 <- {what} {} bytes [{}]{}",
                want,
                head.join(" "),
                if first == 1 || first == 0 {
                    format!(" sequence {}", u16::from_le_bytes([packet[2], packet[3]]))
                } else {
                    String::new()
                }
            );
        }
        Ok(Some(match code {
            0 => {
                let mut r = Rdr::new(&packet);
                let _ = r.u8();
                let code = r.u8();
                let sequence = r.u16();
                let value = r.u32();
                let minor = r.u16();
                let major = r.u8();
                Packet::Error(ProtocolError {
                    code,
                    major,
                    minor,
                    sequence,
                    value,
                })
            }
            1 => {
                let mut r = Rdr::new(&packet);
                let _ = r.u8();
                let _kind = r.u8();
                let sequence = r.u16();
                let extra = r.u32() as usize * 4;
                // The 32-byte header is already consumed -- `self.rstart` moved past it above -- so
                // what is still missing is the extra data alone. Asking for `want + extra` here is off
                // by the size of the header for every reply, which is why a `GetKeyboardMapping` reply
                // of 6,976 bytes was reported as short when all 6,976 of them had arrived.
                if extra > 0 && !self.ensure(extra, deadline)? {
                    // Also a short read, and also not a failure: the rest is in flight and the next read
                    // will pick it up.
                    return Ok(None);
                }
                self.rstart += extra;
                // The whole packet, header and extra data. The first version took
                // `rbuf[rstart - want - extra .. rstart - extra]`, which is the 32-byte header with the
                // extra data cut off -- so `GetKeyboardMapping` decoded an empty keysym table and
                // `GetImage` an empty picture, and both looked like the server having said nothing.
                let body = self.rbuf[self.rstart - want - extra..self.rstart].to_vec();
                Packet::Reply { sequence, body }
            }
            _ => Packet::Event(
                Event::decode(&self.rbuf[self.rstart - want..self.rstart]).expect("an event code"),
            ),
        }))
    }

    /// How many bytes this server's key and button events occupy.
    ///
    /// # 32 on this host, 24 by the protocol, and the difference is fatal
    ///
    /// The specification says a key or button event is 24 bytes, and every core client reads 24. This
    /// host does not: with nothing between the socket and the bytes
    /// (`cargo run -p holonomy-x11 --example dump_keybytes`), one synthesised tap arrives as 64 bytes --
    /// a `KeyPress`, then `cf fe 53 01 00 00 01 00`, then a `KeyRelease` -- so both events are 24 bytes
    /// of real event followed by eight bytes that are in no specification and in no extension this client
    /// asked for, and the next event starts 32 bytes in.
    ///
    /// Read as 24, the client consumes the first event correctly, then reads the padding as an event: its
    /// first byte is `0xcf` or `0x02` depending on the key, which is not a keycode, and from there every
    /// event is garbage. Measured in the developer window: two key events, no releases, a protocol error
    /// the client had invented, and the window closing on its first keystroke.
    ///
    /// # Why a constant and not a guess
    ///
    /// This looked like a thing to *learn* from the stream -- read 24, look at what follows, decide -- and
    /// both attempts at that failed on real packets, because the padding's first byte sometimes looks
    /// like a valid event code: it was `0xcf` in a clean capture and `0x02` in the window, and a test that
    /// accepts extension codes says 24 when the answer is 32. So the size is a property of the server,
    /// measured, and a constructor-level override rather than a per-packet guess:
    /// [`Conn::set_key_event_bytes`].
    ///
    /// A server that sends 24-byte key events is one call away, and `tests/live.rs` covers the 32-byte
    /// case on this host.
    fn key_event_size(&self, code: u8) -> usize {
        let spec = proto::event_size(code);
        if (FIRST_KEY_OR_BUTTON..=LAST_KEY_OR_BUTTON).contains(&code) {
            self.key_event_bytes
        } else {
            spec
        }
    }

    fn compact(&mut self) {
        if self.rstart > 0 && self.rstart == self.rbuf.len() {
            self.rbuf.clear();
            self.rstart = 0;
        } else if self.rstart > 4096 {
            self.rbuf.drain(..self.rstart);
            self.rstart = 0;
        }
    }

    /// Make sure `want` bytes are buffered. Returns false if the deadline passed first.
    fn ensure(&mut self, want: usize, deadline: Option<Instant>) -> Result<bool, ConnError> {
        let mut chunk = [0u8; 4096];
        while self.rbuf.len() - self.rstart < want {
            // The deadline is checked *before* every read attempt, not only inside `wait`. A `wait`
            // that is handed a zero timeout returns immediately, so a loop that only checks the
            // deadline there spins forever once it has passed -- which is exactly what happened: two
            // live tests hung instead of reporting that the server had sent nothing.
            if let Some(d) = deadline {
                if Instant::now() >= d {
                    return Ok(false);
                }
            }
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    return Err(ConnError::Io(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "the X server closed the connection",
                    )))
                }
                Ok(n) => {
                    if tracing() {
                        eprintln!(
                            "x11    read {n} bytes, {} buffered, {want} wanted",
                            self.rbuf.len() - self.rstart
                        );
                    }
                    self.rbuf.extend_from_slice(&chunk[..n])
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if tracing() {
                        eprintln!(
                            "x11    would block: {} buffered, {want} wanted",
                            self.rbuf.len() - self.rstart
                        );
                    }
                    let Some(deadline) = deadline else {
                        return Ok(false);
                    };
                    self.wait(false, deadline.saturating_duration_since(Instant::now()))?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(ConnError::Io(e)),
            }
        }
        Ok(true)
    }

    /// Wait for the socket to become readable or writable.
    fn wait(&self, writable: bool, timeout: Duration) -> Result<(), ConnError> {
        if timeout.is_zero() {
            return Ok(());
        }
        let mut pfd = libc::pollfd {
            fd: self.stream.as_raw_fd(),
            events: if writable {
                libc::POLLOUT
            } else {
                libc::POLLIN
            },
            revents: 0,
        };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        let n = unsafe { libc::poll(&mut pfd, 1, ms) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::Interrupted {
                return Err(ConnError::Io(e));
            }
        }
        Ok(())
    }

    /// `QueryExtension`, returning `(present, major-opcode, first-event, first-error)`.
    pub fn query_extension(&mut self, name: &str) -> Result<Option<(bool, u8, u8, u8)>, ConnError> {
        // byte 1 is `unused`, then the name's length as a CARD8, then **three** unused bytes, then the
        // name. The specification lists two unused bytes there, which puts the name at offset 7 -- and
        // this server answers `BadLength` for that. Measured, by sending the same name in 3-, 4- and
        // 5-word requests (`cargo run -p holonomy-x11 --example dump_lengths`): 3 words is refused,
        // 4 and 5 are accepted, so the name starts at offset 8 and the fixed part after the header is
        // four bytes rather than three.
        let seq = self.request(
            Req::new(op::QUERY_EXTENSION)
                .second_byte(0)
                .u8(name.len() as u8)
                .pad(3)
                .bytes(name.as_bytes()),
        )?;
        self.flush()?;
        let body = self.reply(seq)?;
        let mut r = Rdr::new(&body);
        r.skip(REPLY_FIELDS);
        let present = r.u8() != 0;
        let major = r.u8();
        let first_event = r.u8();
        let first_error = r.u8();
        Ok(Some((present, major, first_event, first_error)))
    }

    /// `GetKeyboardMapping`: one `Vec` of keysyms per keycode, indexed from `min_keycode`.
    pub fn keyboard_mapping(&mut self) -> Result<Vec<Vec<u32>>, ConnError> {
        let first = self.setup.min_keycode;
        let count = self
            .setup
            .max_keycode
            .saturating_sub(self.setup.min_keycode)
            .saturating_add(1);
        let seq = self.request(
            Req::new(op::GET_KEYBOARD_MAPPING)
                .second_byte(0) // unused
                .u8(first)
                .u8(count)
                .u16(0),
        )?;
        self.flush()?;
        let body = self.reply(seq)?;
        let per = body[1] as usize;
        if per == 0 {
            return Ok(Vec::new());
        }
        let mut r = Rdr::new(&body[32..]);
        let mut out = Vec::with_capacity(count as usize);
        for _ in 0..count as usize {
            let mut syms = Vec::with_capacity(per);
            for _ in 0..per {
                syms.push(r.u32());
            }
            out.push(syms);
        }
        Ok(out)
    }

    /// `SetInputFocus` to `window`, reverting to the pointer if the window goes away.
    ///
    /// The error is claimed here rather than left in the queue. `SetInputFocus` on a window that is not
    /// yet viewable is a `BadMatch` -- measured, every time, when the focus is taken immediately after
    /// `MapWindow` -- and a caller that treats that as "best effort" still has to *consume* it, because
    /// the next request to ask for a queued error gets this one. That is how a failed focus became the
    /// reason a window refused to present its first frame.
    pub fn set_input_focus(&mut self, window: u32) -> Result<(), ConnError> {
        self.request(
            Req::new(op::SET_INPUT_FOCUS)
                .second_byte(proto::focus::POINTER_ROOT) // revert-to lives in byte 1
                .u32(window)
                .u32(0), // CurrentTime
        )?;
        self.sync()?;
        if let Some(e) = self.take_error() {
            return Err(ConnError::Protocol(e));
        }
        Ok(())
    }

    /// Send `GetInputFocus`, for a caller that wants to know who has the keyboard.
    pub fn input_focus(&mut self) -> Result<u32, ConnError> {
        let seq = self.request(Req::new(op::GET_INPUT_FOCUS))?;
        self.flush()?;
        let body = self.reply(seq)?;
        let mut r = Rdr::new(&body);
        // GetInputFocus: reply marker, revert-to, sequence, length, then the focus window.
        r.skip(1);
        let _revert_to = r.u8();
        r.skip(6);
        Ok(r.u32())
    }

    /// Read until the reply to `seq` arrives, queueing events on the way.
    ///
    /// **An error naming `seq` is returned, not queued.** X11 reports a failed request by sending an
    /// error instead of a reply, so a caller that waited for the reply would wait out the whole
    /// timeout with the reason sitting in a queue it never looks at -- which is what the first version
    /// of this did, and the effect was a `Window::create` that "timed out" for five seconds with the
    /// `BadValue` that caused it already in hand and unread. Errors for *other* requests are queued,
    /// because they belong to someone else.
    pub fn reply(&mut self, seq: u16) -> Result<Vec<u8>, ConnError> {
        let deadline = Instant::now() + SETUP_TIMEOUT;
        loop {
            match self.read_packet(Some(deadline))? {
                Some(Packet::Reply { sequence, body }) if sequence == seq => {
                    // A reply is not a barrier for errors; see `drain_ready`.
                    self.drain_ready();
                    return Ok(body);
                }
                // An error that arrives while a reply is outstanding belongs to it. Matching the
                // error's own sequence number cannot be done: X.Org fills that field from the failing
                // request's *second byte*, which for most requests is a field rather than a sequence
                // number -- so an error for `GetKeyboardMapping` arrives saying sequence 0, which is
                // that request's `unused` byte. Measured here: four errors in a row all reading
                // "sequence 0" for requests that carried no sequence number at all.
                //
                // So: this crate never has two requests in flight -- `request` then `flush` then
                // `reply` -- and any error seen while waiting belongs to the reply being waited for.
                Some(Packet::Error(e)) => return Err(ConnError::Protocol(e)),
                Some(other) => self.queue(other),
                None if Instant::now() >= deadline => {
                    return Err(ConnError::TimedOut { what: "a reply" })
                }
                None => {}
            }
        }
    }
}

/// A readable name for a core event code, for the trace.
fn holonomy_x11_event_name(code: u8) -> Option<&'static str> {
    Some(match code {
        proto::event::KEY_PRESS => "KeyPress",
        proto::event::KEY_RELEASE => "KeyRelease",
        proto::event::BUTTON_PRESS => "ButtonPress",
        proto::event::BUTTON_RELEASE => "ButtonRelease",
        proto::event::EXPOSE => "Expose",
        proto::event::MAP_NOTIFY => "MapNotify",
        proto::event::CONFIGURE_NOTIFY => "ConfigureNotify",
        proto::event::PROPERTY_NOTIFY => "PropertyNotify",
        proto::event::CLIENT_MESSAGE => "ClientMessage",
        6 => "MotionNotify",
        _ => return None,
    })
}

fn read_exact(stream: &mut UnixStream, buf: &mut [u8]) -> Result<(), ConnError> {
    let mut at = 0;
    while at < buf.len() {
        match stream.read(&mut buf[at..]) {
            Ok(0) => {
                return Err(ConnError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "the X server closed the connection during the handshake",
                )))
            }
            Ok(n) => at += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(ConnError::Io(e)),
        }
    }
    Ok(())
}
