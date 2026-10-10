//! The developer window: the same session, driven by X11 events instead of a scripted source.
//!
//! # Why this is a driver and not another `InputSource`
//!
//! [`Session::run`] takes an [`InputSource`](holonomy_input::InputSource), which is the right shape for
//! `evdev` -- one fd, one blocking read, no lifetime. A window is not that shape: its events and its
//! frames share one socket, and the socket is owned by the backend, which the session owns. So a driver
//! takes the session's backend back out for the length of a read ([`Session::scanout_mut`]), reads one
//! event, and hands the session a key event. Nothing about the session, the editor or the keymap changes:
//! the same `handle_event`, the same `apply`, the same `tick`.
//!
//! # The loop's shape
//!
//! ```text
//!   loop {
//!       wait up to one blink period for an event
//!       key press/release  -> handle_event  (which ticks, so it paints)
//!       ConfigureNotify    -> resize the backend, then the session, then repaint
//!       Expose             -> repaint all of it
//!       ButtonPress        -> take the focus, repaint
//!       ClientMessage      -> the window manager asked us to close: quit
//!       nothing            -> tick, which blinks if it is due
//!   }
//! ```
//!
//! # Resizing
//!
//! The window is resizable, and a `ConfigureNotify` is handled with one call: `Session::resize`. That
//! method owns the order -- backend first, then the frame -- because `Scanout::present` checks the
//! frame's size against the backend's and refuses a mismatch, so a session that resized its frame first
//! would leave every paint failing until the backend caught up. The driver does not know that, and
//! should not have to.
//!
//! The measure is fixed at 80 columns and the page is centred, so a resize moves the gutters and
//! nothing else: the text does not re-wrap, the caret does not move, no edit is undone. That is what
//! makes a drag cheap, and it is
//! [`ChromeMetrics::for_size`](holonomy_render::chrome::ChromeMetrics::for_size) that decides it.
//!
//! The timeout is what paces this, and it is a quarter of the blink period: the caret has to blink on
//! time, and nothing else in the loop blocks. Between keystrokes the loop wakes 4 times a second and
//! sends no pixels at all, which is the property that makes a 1280x800 window cost nothing between
//! blinks.
//!
//! # What is *not* here
//!
//! Nothing from the sealed boot chain. No container, no `unshare`, no seccomp, no evdev. This path exists
//! so that a person can type a document on a desktop without `sudo`, and it is behind the `desktop`
//! feature, which is off by default. The production path is still the jail.

use std::time::{Duration, Instant};

use holonomy_assets::atlas::Atlas;
use holonomy_display::paint::Painter;
use holonomy_display::{Desktop, Scanout};
use holonomy_input::x11key;
use holonomy_render::chrome::{Blink, ChromeMetrics};
use holonomy_text::Editor;

use crate::args::Args;
use crate::session::{Exit, Session};

/// Why the window could not be driven.
#[derive(Debug)]
pub enum WindowedError {
    /// The window could not be opened.
    Desktop(holonomy_display::DesktopError),
    /// The session's backend was not the window the driver put there.
    NotAWindow,
    /// The session failed.
    Session(crate::session::SessionError),
}

impl std::fmt::Display for WindowedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Desktop(e) => write!(f, "{e}"),
            Self::NotAWindow => write!(f, "the session's backend is not the window"),
            Self::Session(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for WindowedError {}

/// A quarter of the blink period: how long the loop waits for an event before giving up and ticking.
///
/// A blink period of 500 ms divided by four. The loop has no other timer, so this is also the loop's
/// frame rate when nothing is happening: 4 wake-ups a second, no pixels, which is why the window costs
/// nothing between keystrokes.
pub const EVENT_WAIT: Duration = Duration::from_millis(Blink::DEFAULT_PERIOD as u64 / 4);

/// Run a session in a window until Ctrl+Q, the window is closed, or the display goes away.
///
/// `atlas` is the already-built atlas: building one takes milliseconds and `include_bytes!` data, and
/// this path is not a place to be clever about either.
pub fn run(
    atlas: &'static Atlas,
    args: &Args,
    metrics: ChromeMetrics,
) -> Result<Exit, WindowedError> {
    let desktop = Desktop::open(
        args.display.as_deref(),
        metrics.width,
        metrics.height,
        "Holonomy",
    )
    .map_err(WindowedError::Desktop)?;
    eprintln!(
        "holonomy: window {:#x} is {}x{}, opening on {}",
        desktop.window_id(),
        desktop.width(),
        desktop.height(),
        desktop.describe()
    );

    let mut session = Session::new(
        Editor::new(),
        Painter::new(atlas, 0),
        Box::new(desktop),
        metrics,
    );
    session.state.zoom_percent = args.zoom;
    session.repaint_all().map_err(WindowedError::Session)?;

    let start = Instant::now();
    let mut events = 0u64;
    loop {
        // One event, with a bound. The borrow of the session ends at the end of this block, so the
        // session is free again for the dispatch below.
        let event = {
            let desktop = window_of(&mut session)?;
            match desktop
                .next_event(EVENT_WAIT)
                .map_err(WindowedError::Desktop)?
            {
                None => None,
                Some(ev) => {
                    events += 1;
                    Some(ev)
                }
            }
        };

        match event {
            None => {
                // Nothing happened. `tick` blinks if it is due and paints nothing if it is not.
                session.tick().map_err(WindowedError::Session)?;
            }
            Some(ev) => {
                match ev {
                    // A key. The whole point.
                    ev @ (holonomy_x11::Event::KeyPress { .. }
                    | holonomy_x11::Event::KeyRelease { .. }) => {
                        if let Some(key) = x11key::key_event(&ev) {
                            if std::env::var_os("HOLONOMY_WINDOW_TRACE").is_some() {
                                eprintln!("dispatch {key:?}");
                            }
                            // Nothing else: the key goes to the session, which folds the modifier
                            // state and dispatches. Keeping the trace here rather than inside the
                            // session means it cannot change what the session does.
                            if let Some(exit) =
                                session.handle_event(key).map_err(WindowedError::Session)?
                            {
                                report(&session, events, start);
                                return Ok(exit);
                            }
                        }
                    }
                    // The window changed size.
                    //
                    // Two halves, in this order: the backend first, because the frame it presents into
                    // is checked against its size and a paint at the old size would be refused. A drag
                    // delivers one of these per intermediate size, so this runs continuously while a
                    // window is being dragged rather than once at the end.
                    holonomy_x11::Event::ConfigureNotify { width, height } => {
                        // One call, because the order -- backend first, then the frame -- is the
                        // session's business and not the driver's. A drag delivers one of these per
                        // intermediate size, so this is a per-frame cost while a window is moving.
                        if session
                            .resize(width as u32, height as u32)
                            .map_err(WindowedError::Session)?
                        {
                            session.repaint_all().map_err(WindowedError::Session)?;
                        }
                    }
                    // The window is on screen and the server does not know what is where. Repaint all of
                    // it: this is the server saying "this region is undefined", and the only honest
                    // answer is the whole frame.
                    holonomy_x11::Event::Expose { .. } => {
                        session.repaint_all().map_err(WindowedError::Session)?;
                    }
                    // A click is a request for the keyboard, and a window manager is entitled to take
                    // the focus back at any time -- measured on GNOME, which answers `SetInputFocus` and
                    // then sends `FocusOut`.
                    //
                    // **The press is handled as well as focused, and in this order.** The first version
                    // matched `ButtonPress` and only took the focus, so every click in a window was a
                    // click that did nothing: the driver had the event, the session had the routing,
                    // and the arm that matched the event chose neither. **Focus first** because a
                    // window manager's `FocusOut` round trip must not delay the click's own paint --
                    // but handle it in the same arm, because an arm that *matches* an event and then
                    // declines to act on it is the shape of this bug.
                    holonomy_x11::Event::ButtonPress { .. } => {
                        window_of(&mut session)?
                            .focus()
                            .map_err(WindowedError::Desktop)?;
                        let Some(p) = x11key::pointer_event(&ev) else {
                            continue;
                        };
                        session
                            .handle_pointer(&mut crate::store::NoSource, p)
                            .map_err(WindowedError::Session)?;
                    }
                    // Motion, release and the wheel: routed, never focused on, never interpreted here.
                    //
                    // **`x11key::pointer_event` is the only place X's button numbering and X's
                    // "buttons 4 and 5 are the wheel" convention are translated.** The driver sees a
                    // `PointerEvent` or `None` and knows nothing about either convention, which is what
                    // makes it the same code the scripted path runs.
                    ev @ (holonomy_x11::Event::ButtonRelease { .. }
                    | holonomy_x11::Event::MotionNotify { .. }) => {
                        let Some(p) = x11key::pointer_event(&ev) else {
                            continue;
                        };
                        session
                            .handle_pointer(&mut crate::store::NoSource, p)
                            .map_err(WindowedError::Session)?;
                    }
                    // The title bar's close button, forwarded as `WM_DELETE_WINDOW`.
                    holonomy_x11::Event::ClientMessage { data1, .. }
                        if data1 == WM_DELETE_WINDOW =>
                    {
                        report(&session, events, start);
                        return Ok(Exit::Quit);
                    }
                    _ => {}
                }
            }
        }
    }
}

/// `WM_DELETE_WINDOW`, the predefined-ish atom every window manager forwards. `Window::create` interns
/// it and sets it on the window; this is only the value it comes back with.
const WM_DELETE_WINDOW: u32 = 33;

/// The session's backend, as the window the driver put there.
fn window_of<'s>(session: &'s mut Session<'_>) -> Result<&'s mut Desktop, WindowedError> {
    // The session owns the backend -- that is what presents frames -- and this is how the driver gets
    // it back for the length of a read. `Scanout: Any` is what makes the downcast possible; see the
    // trait's docs for why it is there.
    let any: &mut dyn std::any::Any = session.scanout_mut();
    any.downcast_mut::<Desktop>()
        .ok_or(WindowedError::NotAWindow)
}

/// What the session did, on the way out.
///
/// The table counters are in this line rather than behind a debug flag because a table's whole point
/// is that it is *visible*, and "is there a table on screen" is not a question a log line should need a
/// flag to answer. A windowed run that inserted a table and drew no cells would otherwise report
/// success.
fn report(session: &Session<'_>, events: u64, start: Instant) {
    let t = &session.stats;
    eprintln!(
        "holonomy: {} commands, {} edits, {} frames, {} pixels, {} events in {:.1}s -- \
         press Ctrl+Q to quit",
        t.commands,
        t.edits,
        t.frames,
        t.pixels,
        events,
        start.elapsed().as_secs_f64()
    );
    if t.table_inserts > 0 {
        eprintln!(
            "holonomy: tables: {} inserted, {} cell navigations, {} refused, {} in-cell newlines, \
             {} cells and {} border runs drawn in the last frame",
            t.table_inserts,
            t.table_navs,
            t.table_nav_nowhere,
            t.table_newlines,
            t.table_cells_drawn,
            t.table_borders_drawn,
        );
    }
    if t.math_inserts > 0 {
        // Same argument as the table line above: a formula's whole point is that it is visible, and
        // the split between "compiled" and "raw" is the part that can silently be wrong in both
        // directions -- a formula stuck in raw looks like a formula being edited, which is a state the
        // user chose deliberately, so it cannot be told apart by looking.
        eprintln!(
            "holonomy: math: {} inserted, {} compiled and {} raw in the last frame, {} procedural \
             fills, {} parse errors",
            t.math_inserts, t.math_compiled, t.math_raw, t.math_rules, t.math_parse_errors,
        );
        // The source of every formula in the document, so a live check can be compared against what
        // was typed. Without it, "1 compiled, 0 procedural fills" for a `\frac` is an unexplained
        // number: the counters say the formula compiled and drew no bar, which means the thing that
        // compiled was not the thing that was typed. Reading the bytes settles it.
        if let Ok(text) = session.text() {
            let mut shown = 0usize;
            holonomy_text::for_each_math_span(&text, |sp| {
                shown += 1;
                if shown <= 4 {
                    let src = String::from_utf8_lossy(&text[sp.inner()]);
                    eprintln!(
                        "holonomy: formula at {}..={} is {:?}",
                        sp.start,
                        sp.end,
                        &src[..src.len().min(48)]
                    );
                }
            });
            eprintln!("holonomy: {shown} formulas in the document");
        }
        match session.active_math() {
            Some(sp) => eprintln!(
                "holonomy: caret inside a formula at bytes {}..={} ({})",
                sp.start,
                sp.end,
                if sp.closed { "closed" } else { "unclosed" }
            ),
            None => eprintln!("holonomy: caret is not inside a formula"),
        }
    }
    if t.table_inserts > 0 {
        match (session.active_cell(), session.active_table()) {
            (Some(c), Some(s)) => eprintln!(
                "holonomy: caret in cell ({}, {}) of a {}x{} table at bytes {}..={}",
                c.row, c.col, s.rows, s.cols, s.start_byte, s.end_byte,
            ),
            (cell, span) => eprintln!(
                "holonomy: no caret cell ({:?}) for span {:?}",
                cell.map(|c| (c.row, c.col)),
                span.map(|s| (s.rows, s.cols)),
            ),
        }
    }
}
