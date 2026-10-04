//! The developer's window: a [`Scanout`] that puts frames on a desktop display.
//!
//! # This is not the product
//!
//! On the ThinkPad X200 there is no X server, and the presentation path there is
//! [`crate::drm::DrmScanout`]. This backend exists so that a human can type a document on an ordinary
//! desktop without `sudo`, and it sits behind the `desktop` feature, which is off by default and is not
//! part of the sealed boot chain. `tests/release_artifact.rs` in the `holonomy` crate asserts that a
//! default-features release binary contains none of it.
//!
//! # What it does with a frame
//!
//! [`Scanout::present_damage`] is the interesting part. The session rasterises only what an edit
//! damaged but hands the backend the *whole* frame, so a backend that pushes all of it spends 4 MiB per
//! keystroke on a socket for 18 rows of change. This one pushes only the damaged rectangle, clipped to
//! the window, which for a keystroke at 1280 pixels is 18 rows = 92,160 bytes, and for a caret blink is
//! one cell of 144. The frame is still the source of truth -- the rect says which part of it to send,
//! not what the window should contain.
//!
//! # Events are drained here, because the connection lives here
//!
//! [`Desktop`] owns the socket, so it also owns reading from it. A backend that exposed the connection
//! would need shared ownership or a second reader; this one hands out decoded events instead, and the
//! session turns them into key events with [`holonomy_input::x11key`].

use std::collections::VecDeque;
use std::time::Duration;

use holonomy_x11::{Conn, Event, Window};

use crate::frame::{Frame, FrameError};
use crate::Scanout;

/// Why a window could not be opened or driven.
#[derive(Debug)]
pub enum DesktopError {
    /// The X connection failed. See [`holonomy_x11::ConnError`].
    Connect(holonomy_x11::ConnError),
    /// The window could not be created or driven.
    Window(holonomy_x11::WindowError),
    /// A frame was the wrong size for the window.
    SizeMismatch {
        /// What the window is.
        want: (u32, u32),
        /// What the frame was.
        got: (u32, u32),
    },
    /// The frame is smaller than the rectangle that was asked for.
    RectOutsideFrame {
        /// The rectangle.
        rect: (u32, u32, u32, u32),
        /// The frame's size.
        frame: (u32, u32),
    },
}

impl std::fmt::Display for DesktopError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(e) => write!(f, "cannot open a window: {e}"),
            Self::Window(e) => write!(f, "cannot drive the window: {e}"),
            Self::SizeMismatch { want, got } => write!(
                f,
                "the window is {}x{} and the frame is {}x{}",
                want.0, want.1, got.0, got.1
            ),
            Self::RectOutsideFrame { rect, frame } => write!(
                f,
                "the damaged rectangle {rect:?} is not inside the {frame:?} frame"
            ),
        }
    }
}

impl std::error::Error for DesktopError {}

impl DesktopError {
    /// The `Scanout` trait reports failures as a [`FrameError`], so a window's own error has to be
    /// flattened into one.
    ///
    /// `FrameError` has no string in it, which is a real loss of information: a caller that gets
    /// `Backend(errno)` cannot tell "no display" from "the window went away". The errno is kept where
    /// X11 has one -- [`holonomy_x11::ConnError::Io`] carries it -- and everything else becomes 0. The
    /// window's own [`DesktopError`] is what a caller should match on, which is why [`Desktop`] has
    /// methods that return it directly.
    fn into_frame_error(self) -> FrameError {
        match self {
            Self::SizeMismatch { want, got } => FrameError::SizeMismatch { want, got },
            Self::Connect(holonomy_x11::ConnError::Io(e)) => FrameError::Io(e),
            Self::Connect(holonomy_x11::ConnError::Socket { errno, .. }) => {
                FrameError::Backend(errno)
            }
            other => {
                let _ = other;
                FrameError::Backend(0)
            }
        }
    }
}

/// A window on a desktop display: the connection, the window, and the pixels in between.
#[derive(Debug)]
pub struct Desktop {
    conn: Conn,
    window: Window,
    width: u32,
    height: u32,
    presented: u64,
    /// Bytes pushed, which is the number worth watching: it is what the socket cost.
    bytes: u64,
    /// Frames pushed, and how many of those were damage-only.
    full_frames: u64,
    events: VecDeque<Event>,
}

impl Desktop {
    /// Open `width` x `height`, show it, and ask the window manager to bring it forward.
    ///
    /// `display` is `$DISPLAY` when `None`. Failing to open a display is an error, not a fallback to
    /// [`crate::HeadlessScanout`]: a caller that asked for a window and got silence would have no way
    /// to tell the user anything.
    pub fn open(
        display: Option<&str>,
        width: u32,
        height: u32,
        title: &str,
    ) -> Result<Self, DesktopError> {
        let mut conn = Conn::connect(display).map_err(DesktopError::Connect)?;
        let window =
            Window::create(&mut conn, width, height, title).map_err(DesktopError::Window)?;
        window.map(&mut conn).map_err(DesktopError::Window)?;
        // Best effort: a window manager that will not activate us is not a reason to refuse to open.
        let _ = window.activate(&mut conn);
        let _ = window.focus(&mut conn);
        let mut this = Self {
            conn,
            window,
            width,
            height,
            presented: 0,
            bytes: 0,
            full_frames: 0,
            events: VecDeque::with_capacity(64),
        };
        // The first paint has no damage behind it, so the whole window is sent once here rather than
        // being left to whatever damage the first real event produces.
        this.presented = 1;
        this.full_frames = 1;
        Ok(this)
    }

    /// The window's id, for logs and for a caller that wants to raise it itself.
    pub fn window_id(&self) -> u32 {
        self.window.id()
    }

    /// How many frames have been pushed.
    pub fn present_count(&self) -> u64 {
        self.presented
    }

    /// How many bytes have gone over the socket.
    pub fn bytes_pushed(&self) -> u64 {
        self.bytes
    }

    /// How many of the frames pushed were the whole window rather than a rectangle.
    pub fn full_frames(&self) -> u64 {
        self.full_frames
    }

    /// The next event, waiting up to `timeout` for one.
    ///
    /// Returns `None` on a timeout, which is the normal case between keystrokes: a word processor's
    /// event loop has to wake up regularly anyway, because the caret blinks.
    pub fn next_event(&mut self, timeout: Duration) -> Result<Option<Event>, DesktopError> {
        if let Some(ev) = self.events.pop_front() {
            return Ok(Some(ev));
        }
        loop {
            match self.conn.next_event(Some(timeout)) {
                Ok(Some(ev)) => return Ok(Some(ev)),
                Ok(None) => return Ok(None),
                // An error event is not fatal; the connection carries on and the caller never sees it.
                Err(holonomy_x11::ConnError::Protocol(_)) => continue,
                Err(e) => return Err(DesktopError::Connect(e)),
            }
        }
    }

    /// Take the keyboard focus. A click handler calls this, because a window manager may take the focus
    /// back at any time -- measured on GNOME, which answers `SetInputFocus` and then sends `FocusOut`.
    pub fn focus(&mut self) -> Result<(), DesktopError> {
        self.window
            .focus(&mut self.conn)
            .map(|_| ())
            .map_err(DesktopError::Window)
    }

    /// Push `frame`'s pixels for `rect`.
    fn push(
        &mut self,
        frame: &Frame,
        rect: (u32, u32, u32, u32),
        whole: bool,
    ) -> Result<u64, DesktopError> {
        let (rx, ry, rw, rh) = rect;
        if rw == 0 || rh == 0 {
            return Ok(0);
        }
        if rx + rw > frame.width() || ry + rh > frame.height() {
            return Err(DesktopError::RectOutsideFrame {
                rect,
                frame: frame.size(),
            });
        }
        // A `Frame` is a `Vec<u32>` of `0x00RRGGBB`, row-major and tightly packed, and the server's
        // depth-24 TrueColor format wants exactly those bytes in exactly that order -- measured, the
        // visual's masks are R=0x00FF0000 G=0x0000FF00 B=0x000000FF in LSBFirst order, and a frame put
        // into a pixmap comes back byte for byte. So the rectangle is copied out row by row and sent
        // as it is, with no conversion and no palette.
        let row_bytes = rw as usize * 4;
        let mut buf = Vec::with_capacity(row_bytes * rh as usize);
        for y in ry..ry + rh {
            let start = (y * frame.width() + rx) as usize;
            let row = &frame.pixels()[start..start + rw as usize];
            for px in row {
                buf.extend_from_slice(&px.to_le_bytes());
            }
        }
        let sent = self
            .window
            .put_image(&mut self.conn, &buf, rw, rh, rx as i16, ry as i16)
            .map_err(DesktopError::Window)?;
        self.bytes += sent;
        self.presented += 1;
        if whole {
            self.full_frames += 1;
        }
        Ok(sent)
    }
}

impl Scanout for Desktop {
    fn present(&mut self, frame: &Frame) -> Result<u64, FrameError> {
        if frame.size() != (self.width, self.height) {
            return Err(FrameError::SizeMismatch {
                want: (self.width, self.height),
                got: frame.size(),
            });
        }
        self.push(frame, (0, 0, self.width, self.height), true)
            .map_err(DesktopError::into_frame_error)
    }

    fn present_damage(
        &mut self,
        frame: &Frame,
        damage: Option<crate::DamageRect>,
    ) -> Result<u64, FrameError> {
        let Some(d) = damage else {
            return self.present(frame);
        };
        if frame.size() != (self.width, self.height) {
            return Err(FrameError::SizeMismatch {
                want: (self.width, self.height),
                got: frame.size(),
            });
        }
        // Clipped to the frame: a rect that runs past the edge would be a session bug, and pushing the
        // part that exists is better than refusing to paint at all -- but it is reported by the byte
        // count being smaller than the rect asked for, which is what `push` returns.
        let r = d.clip(&crate::DamageRect::new(0, 0, self.width, self.height));
        self.push(frame, (r.x, r.y, r.width, r.height), false)
            .map_err(DesktopError::into_frame_error)
    }

    fn width(&self) -> u32 {
        self.width
    }

    fn height(&self) -> u32 {
        self.height
    }

    fn describe(&self) -> &'static str {
        "x11 window (developer desktop; not the bare-silicon path)"
    }
}
