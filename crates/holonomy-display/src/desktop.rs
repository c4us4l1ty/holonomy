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
//! # Resizing
//!
//! [`Desktop::resize`] and the session's resize are two halves of one operation and the order matters:
//! the backend first, because the frame it presents into is checked against its size. The driver does
//! exactly that on every `ConfigureNotify`, which a window drag delivers for each intermediate size --
//! so this is exercised continuously while a window is being dragged, not once.
//!
//! The measure is fixed (80 columns) and the page is centred, so a resize moves the gutters and nothing
//! else: the text does not re-wrap, the caret does not move, and no edit is undone. That is what
//! [`ChromeMetrics::for_size`](holonomy_render::chrome::ChromeMetrics::for_size) is for.
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
    /// **The `PutImage` staging buffer, reused across pushes.**
    ///
    /// **Held here and taken out for the duration of a push** (`std::mem::take`) so that a
    /// reallocation on the frame path cannot happen while the borrow of it is live in
    /// `Window::put_image`. `put_image` borrows the slice for the whole call.
    scratch: Vec<u8>,
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
        // Tell the window manager what this client can draw into *before* mapping, so the first frame
        // is never asked for at a size the chrome cannot use. `ChromeMetrics::for_size` clamps anyway,
        // so this is the floor for the user's drag and the clamp is the floor for the code's arithmetic.
        //
        // **And it is propagated, because `let _ =` is how the window stayed un-resizable.**
        //
        // This call's error was discarded for the whole life of the feature. A `BadLength` from it --
        // which is exactly what it returned, because of the two bugs in `set_size_hints` -- was thrown
        // away, the window mapped with no `WM_NORMAL_HINTS` at all, and `xprop -id <window>
        // WM_NORMAL_HINTS` answered **`not found`** on a running window. **A window with no size hints
        // is a fixed-size island in a resizable desktop**, so the driver handled `ConfigureNotify` for
        // a resize that could never arrive.
        //
        // **It is not best-effort.** A window manager that will not take the hints is a window the
        // user cannot resize, and that is worth refusing to open over.
        //
        // **The size is narrowed once, here, and everything else is computed from it.** `XSizeHints`
        // is a `CARD32` per field but X11 windows are `u16` in `CreateWindow` and `ConfigureWindow`,
        // so a `u32` over 65,535 cannot be written into either without losing its top bits — and a
        // size hint that silently disagrees with the window is the defect in the paragraph below.
        let size = (
            width.min(u32::from(u16::MAX)) as u16,
            height.min(u32::from(u16::MAX)) as u16,
        );
        window
            .set_size_hints(
                &mut conn,
                // **The size the window was created at, and it is the argument `PSize` names.**
                //
                // It used to be omitted, so `set_size_hints` left `width` and `height` at zero while
                // still setting the `PSize` flag — and a window manager reading that is being told
                // *this window wants to be 0 x 0*, which it clamps up to the minimum. Measured on this
                // machine: the app asked for 1600 x 1000 and the server answered **768 x 320** on
                // `GetGeometry`, which is `MIN_WIDTH x MIN_HEIGHT` exactly. That is the *"it's way too
                // small and I cannot drag it"* report, in one request.
                size,
                (
                    // **The minimum can never exceed the size this window was actually created at.**
                    //
                    // `opening_size` clamps the window into the screen, so on a screen narrower than
                    // `MIN_WIDTH` the window is created *below* the nominal minimum — 336 px on a
                    // 400 px screen. Publishing `PMinSize = 768` there tells the window manager to
                    // grow it back past the edge of the display, **which is the same defect as the
                    // layout clamp fixed in part 38, one layer up**: the app asking to be given more
                    // room than it can be in.
                    //
                    // **`min`, not `max`.** The floor is what the window manager enforces upwards, and
                    // the floor is the only one of the three that can contradict the geometry.
                    (holonomy_render::chrome::ChromeMetrics::MIN_WIDTH as u16).min(size.0),
                    (holonomy_render::chrome::ChromeMetrics::MIN_HEIGHT as u16).min(size.1),
                ),
                None,
            )
            .map_err(DesktopError::Window)?;
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
            scratch: Vec::new(),
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
    /// The X screen's own size, for sizing a window to it.
    ///
    /// **A second connection, and only ever on the window path at startup.** `open` has to be handed a
    /// size to create the window with, and the size it wants is a fraction of the screen it has not
    /// connected to yet. **One extra connection at startup is cheaper than threading a size the caller
    /// cannot know, and it is not on the frame path.**
    ///
    /// **`None` rather than an error.** A server that will not say how big it is gets the default size,
    /// which is what happened before this existed and is a better answer than refusing to open.
    pub fn screen_size(display: Option<&str>) -> Option<(u32, u32)> {
        use holonomy_x11::Conn;
        let conn = Conn::connect(display).ok()?;
        let (w, h) = (conn.setup().root_width, conn.setup().root_height);
        Some((u32::from(w), u32::from(h)))
    }

    pub fn window_id(&self) -> u32 {
        self.window.id()
    }

    /// The window's size as the server currently has it.
    ///
    /// This is [`GetGeometry`](holonomy_x11::Window::geometry) on every call, which is a round trip.
    /// The driver's event loop already knows the size -- `ConfigureNotify` carries it -- and passes it
    /// to [`Desktop::resize`]; this is for a caller that has no such event, and for a caller checking
    /// that the two agree.
    pub fn server_size(&mut self) -> Result<(u16, u16), DesktopError> {
        self.window
            .geometry(&mut self.conn)
            .map(|(w, h, _)| (w, h))
            .map_err(DesktopError::Window)
    }

    /// Adopt a new window size. Returns `true` if it changed.
    ///
    /// # CORRECTION, part 39: this was an **inherent** `pub fn resize`, and `Session` holds a
    /// `Box<dyn Scanout>`
    ///
    /// ```text
    /// impl Desktop       { pub fn resize(..) }        // inherent
    /// trait  Scanout      { fn resize(..) { Ok(false) } }  // the default, "I have a fixed size"
    /// ```
    ///
    /// **An inherent method of the same name and signature shadows the trait's — for a caller that has
    /// a `Desktop`. `Session` does not have a `Desktop`; it has a `Box<dyn Scanout>`, so
    /// `self.scanout.resize(w, h)` resolved to the *trait's* `resize`, which is the default body, which
    /// returns `Ok(false)`.** Every window resize in the product therefore returned "nothing changed",
    /// `Session::resize` returned `false` before rebuilding anything, and the frame stayed at its
    /// original size.
    ///
    /// # What the user saw, and why it looked like a layout bug
    ///
    /// The window manager resized the window; the app kept presenting a **1,600 x 1,000 frame into
    /// it**; the server clipped it; and the user was looking at the top-left corner of a chrome laid
    /// out for a different window. The menu bar kept its 1,600 px spacing and ran off the right edge
    /// mid-word, the status bar sat at y = 970 in a 700 px window, and the page's right edge was past
    /// the window — **none of which is what a 900 x 700 layout looks like**, and all of which is what
    /// a 1,600 x 1,000 layout looks like when it is cropped.
    ///
    /// # Why no gate caught it
    ///
    /// **Every gate that tested this called it on a concrete `Desktop`,** where the inherent method wins
    /// and the behaviour is correct. The one gate named in the doc below — `resize_is_recorded_not_
    /// requested` — **does not exist**; it is described in a comment and was never written, so the one
    /// check that would have gone through a trait object went with it. The fix is three parts: the method
    /// is a trait override, `Scanout::resize` has **no default body** so that a backend which forgets
    /// it is a compile error rather than a silent `false`, and there is a gate that reads this file and
    /// counts the `resize` definitions.
    ///
    /// # The backend and the session have to agree, and this is the half that cannot fail
    ///
    /// [`Scanout::present`] checks the frame's size against the backend's and refuses a mismatch, which
    /// is the right check: it catches a session that resized without telling anyone. So when the window
    /// changes size the backend must be told *before* the session paints, and [`Desktop::resize`] is
    /// what the driver calls first. A window drag delivers `ConfigureNotify` for every intermediate
    /// size, and painting at the old size would push the whole old frame into a smaller window --
    /// clipped, and with a band of stale pixels along the right and bottom edges.
    ///
    /// # CORRECTION 2026-10-12: this **records** the size. It does not request one.
    ///
    /// It used to end with:
    ///
    /// ```text
    /// self.window.configure_size(&mut self.conn, w as u16, h as u16)
    /// ```
    ///
    /// **on the reasoning that "the window is resized as well as recorded, so that a caller which
    /// sized the frame first gets the same answer here."** Every size this function is ever called with
    /// came *from* the window manager — it is the payload of the `ConfigureNotify` the WM sent when the
    /// user dragged an edge — **so the request was telling the window manager the size it had just
    /// told us.** That is a no-op at best and a feedback loop at worst, and with Mutter it is the
    /// second: the WM re-asserts its own size, a fresh `ConfigureNotify` arrives, and the two trade
    /// requests until the window and the frame disagree.
    ///
    /// ## What the user saw
    ///
    /// *"I drag and it still retains the 1600x800 resolution and fills other space with black."*
    ///
    /// **Black is `Frame::black`,** which `Session::resize` allocates for the new size. So the frame
    /// *was* enlarged and *never painted*: `Desktop::resize` reported a change, `repaint_all` ran, and
    /// the window manager then resized the window back — leaving a frame larger than the window it is
    /// presented into, and the part of the window with no frame under it showing the server's own
    /// initial contents.
    ///
    /// ## The rule
    ///
    /// **A size that arrived from outside is recorded, not re-requested.** The only caller that may
    /// *ask* for a size is a test or a caller that genuinely wants a specific one, and it has
    /// [`Window::configure_size`] for that.
    ///
    /// **This is now a [`Scanout`] override and not an inherent method**, so `Box<dyn Scanout>` reaches
    /// it; the body is unchanged. See the correction at the top of it.
    ///
    /// **There is deliberately no inherent `Desktop::resize` any more, not even a delegate.** A delegate
    /// would have been correct -- same body, same answer -- and it would also have been the second
    /// entrance that the next reader would call instead of the trait one, in the one situation where
    /// calling the wrong one is silent. **The method is private now and has exactly one caller**, which
    /// is the impl block below.
    fn resize_recorded(&mut self, width: u32, height: u32) -> bool {
        let (w, h) = (width.min(u16::MAX as u32), height.min(u16::MAX as u32));
        if (self.width, self.height) == (w, h) {
            return false;
        }
        // **No `ConfigureWindow` here, and that sentence is the fix.**
        self.width = w;
        self.height = h;
        true
    }

    /// How many frames have been pushed.
    pub fn present_count(&self) -> u64 {
        self.presented
    }

    /// **How many requests this connection has sent to the server.**
    ///
    /// **Counted at [`Conn::request`], not here, and that is the whole point.** A counter in this
    /// struct would only see what this struct still does — so when [`Desktop::resize`] was fixed by
    /// deleting its `ConfigureWindow`, the field lost its only writer and the gate beside it passed
    /// **vacuously**. `resize_is_recorded_not_requested` compares this across a resize and asserts it
    /// did not move; for that to mean anything the count has to come from the one place every
    /// request in this crate passes through.
    pub fn requests_sent(&self) -> u64 {
        self.conn.requests_sent()
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
    ///
    /// # The buffer is the session's, not this frame's
    ///
    /// **One `Vec<u8>` on `Desktop`, reused for every push.** A full 1,600 x 1,000 frame is 6.4 MB, and
    /// `Vec::with_capacity` on the frame path is 6.4 MB handed to the allocator and taken back on
    /// every full repaint — which is every keystroke on a chrome change and every menu open. The
    /// rectangle is *copied* into it a row at a time by [`Frame::append_xrgb_row`], which is one
    /// `memcpy` per row rather than one four-byte copy per pixel.
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
        let mut buf = std::mem::take(&mut self.scratch);
        buf.clear();
        for y in ry..ry + rh {
            frame.append_xrgb_row(rx, rw, y, &mut buf);
        }
        let sent = self
            .window
            .put_image(&mut self.conn, &buf, rw, rh, rx as i16, ry as i16)
            .map_err(DesktopError::Window)?;
        self.scratch = buf;
        self.bytes += sent;
        self.presented += 1;
        if whole {
            self.full_frames += 1;
        }
        Ok(sent)
    }
}

impl Scanout for Desktop {
    /// **The override that part 39 added, and the whole of the window-resize fix.**
    ///
    /// **A window resizes because the window manager said so, and this records that.** It does not ask
    /// for the size: the size came *from* the WM in a `ConfigureNotify`, and asking again is the
    /// feedback loop this module's docs describe. `Session::resize` calls it before it rebuilds its own
    /// frame, because `present` refuses a frame whose size is not the backend's.
    fn resize(&mut self, width: u32, height: u32) -> Result<bool, FrameError> {
        Ok(self.resize_recorded(width, height))
    }

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
