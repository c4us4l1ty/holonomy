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

/// One 60 Hz frame: how long the loop waits for an event before giving up and ticking.
///
/// `Blink` counts frames, so the idle tick has to arrive at the frame rate the blink period
/// assumes. The old `DEFAULT_PERIOD / 4` was 7 ms (~143 wakes/s) and still advanced only one
/// blink frame per wake, so it burned CPU without making the caret blink at the documented
/// cadence. 16 ms is ~60 wakes/s, no pixels between keystrokes.
pub const EVENT_WAIT: Duration = Duration::from_millis(16);

/// Run a session in a window until Ctrl+Q, the window is closed, or the display goes away.
///
/// `atlas` is the already-built atlas: building one takes milliseconds and `include_bytes!` data, and
/// this path is not a place to be clever about either.
/// The size an unsized window opens at, on a screen of `screen`.
///
/// # This is the rule, and it is *the window asks for what the content uses*
///
/// **Not a fraction of the screen, and not the screen.** Both of those were tried and both are wrong
/// for the same underlying reason, which is only visible once the numbers are laid side by side.
///
/// # The measurement this is built on
///
/// On this machine the screen is **3,072 x 1,920**, and `opening_size` was asked for the window's
/// size while the app was running. The answer, from the app's own startup line:
///
/// ```text
/// holonomy: window 0xc00001 is 2764x1728, opening on x11 window
///           (asked 2764x1728, screen Some((3072, 1920)), 89% x 90% of the screen)
/// ```
///
/// **So at 90% of the screen the window opened correctly and still looked "way too small"** — the
/// report this replaces, and the third time it has been filed. The reason is not the window:
///
/// | panel | measure | text | page | page as % of panel |
/// |---|---|---|---|---|
/// | 1,280 | 80 cols | 640 px | 736 px | 58% |
/// | 2,764 | 100 cols | 800 px | **896 px** | **32%** |
/// | 3,072 | 100 cols | 800 px | 896 px | 26% |
///
/// **The page is 896 px at every one of those sizes.** `max_columns: 100` caps the measure, and at
/// 16 ppem a column is 8 px, so 100 columns is 800 px and the page is 896 px **however wide the
/// window is**. That is also the whole answer to *"I can resize it and it is way too small"*: making
/// the window wider does not make the content bigger, it makes the **desk** bigger. At 90% of a
/// 3,072 px screen, 68% of the window is empty grey.
///
/// # Why the window cannot simply be made bigger to compensate
///
/// The obvious alternative is a larger type, and **it does not fit.** The atlas is budgeted at
/// `holonomy_assets::metric::ATLAS_BYTES` = 458,752 bytes and is built once, before the jail:
///
/// | ppem | atlas bytes | of 458,752 |
/// |---|---|---|
/// | 16 | 153,571 | 33% |
/// | 20 | 225,279 | 49% |
/// | 24 | 308,864 | 67% |
/// | 28 | 406,068 | 89% |
/// | **32** | **panics** | `AtlasFull` |
///
/// **ppem 28 is the ceiling**, 1.75x the type, which takes the page from 896 px to 1,216 px — 32% of
/// the window to 44%. **That is a real improvement and it is not enough**, and it spends every byte
/// of headroom the sealed profile has. **So the window is sized to the content instead, and the type
/// stays at 16 ppem where the whole memory budget is comfortable.**
///
/// # Why 1,600 x 1,000 is the target, and why it is not just a constant
///
/// **1,600 x 1,000 is the size at which the design's own proportions hold.** At 1,600 px the measure
/// is 100 columns and the page is 896 px, which is **56% of the window** — close to the 58% the design
/// width of 1,280 px gives, rather than the 32% a 2,764 px window gives. **The ratio is the thing
/// being held constant**, and 1,600 x 1,000 is simply where that ratio sits for a monitor-shaped
/// window.
///
/// **It is not a constant, though, and that is the part that matters on a small screen.** A fixed
/// 1,600 px window on an 800 px screen is a window bigger than the screen, which is the same class of
/// defect as the layout clamp fixed in part 38 — the app drawing outside what it was given. **So the
/// screen is the ceiling**: [`SCREEN_MARGIN`] is left around the window for a window manager's frame
/// and any panel, and the target is clamped into what is left.
///
/// # And never below the minimum
///
/// [`ChromeMetrics::MIN_WIDTH`] is the floor, **clamped to the same room.** `MIN_WIDTH` is what the
/// app asks the window manager for and it is advisory; asking for 768 px on a 400 px screen is a
/// request the window manager will refuse, and the answer has to be the screen's.
///
/// # A free function, and why
///
/// **The rule is arithmetic over four numbers, and inline arithmetic is not testable.** Parts 36 and
/// 37 both got this wrong in the same way: the rule was written inside `run`, so no gate could reach
/// it, and the only way to find out what the app would open was to open it and look. This one is
/// reachable, and `opening_size_matches_the_design_ratio_not_the_screen` in
/// `tests/window_size.rs` is the gate.
///
/// **`floor` is a parameter rather than read from the metrics** so the gate can state the floor it is
/// reasoning about instead of depending on the design constant moving underneath it.
pub fn opening_size(screen: (u32, u32), floor: Option<(u32, u32)>) -> (u32, u32) {
    let (fw, fh) = floor.unwrap_or((
        holonomy_render::chrome::ChromeMetrics::MIN_WIDTH,
        holonomy_render::chrome::ChromeMetrics::MIN_HEIGHT,
    ));
    let fit = |want: u32, screen: u32, floor: u32| -> u32 {
        // **A screen with no extent is a screen that was not read**, and the honest answer to "I do
        // not know how big this is" is the size the app wants, not one pixel. `screen_size` answers
        // `None` when the connection fails and `Some((0, 0))` when the setup carries nothing, and both
        // land here — the window is about to be created blind, and a 1 x 1 window is the one size that
        // is certainly wrong.
        if screen == 0 {
            return want;
        }
        let room = screen.saturating_sub(SCREEN_MARGIN);
        // **`floor.min(room)`, not `floor`.** `max(floor)` on a screen smaller than the floor would
        // hand back a window larger than the display, which is the defect `ChromeMetrics::for_size`
        // stopped committing twenty minutes ago.
        want.min(room).max(floor.min(room)).max(1)
    };
    (fit(DESIGN_W, screen.0, fw), fit(DESIGN_H, screen.1, fh))
}

/// The width an unsized window opens at, however wide the screen is.
///
/// **A target, not a constant**, because [`opening_size`] clamps it into the screen and a target that
/// is applied without clamping is a window bigger than the display.
pub const DESIGN_W: u32 = 1600;

/// The height an unsized window opens at. See [`DESIGN_W`].
pub const DESIGN_H: u32 = 1000;

/// Left around the window on every side, for a window manager's frame and any panel.
///
/// **Sixty-four pixels, which is a multiple of the 8 px cell and of the 16 px ppem.** Not a fraction
/// of the screen: a margin that scales with the screen is a bigger margin on a bigger screen, and the
/// thing being protected here is a window manager's title bar, which is a fixed number of pixels
/// everywhere.
pub const SCREEN_MARGIN: u32 = 64;

pub fn run(
    atlas: &'static Atlas,
    args: &Args,
    metrics: ChromeMetrics,
) -> Result<Exit, WindowedError> {
    // **Said once, at startup, because the symptom reads as the product's fault.** A debug build of
    // a software rasteriser is ~30x slower than the release build on the paint path, and the report
    // it produces is "I click a button and it takes almost a whole second to respond" — filed
    // against the UI, not against the profile that built it. `--release` is in every run command
    // the README shows; this is for the invocation that forgot it. (For measuring a *release*
    // build's latency, HOLONOMY_WINDOW_TRACE=1 prints per-click milliseconds.)
    #[cfg(debug_assertions)]
    eprintln!(
        "holonomy: THIS IS A DEBUG BUILD — painting is unoptimised and clicks will lag. \
         Run with --release: cargo run --release --features desktop -p holonomy -- --window"
    );
    let (w, h) = if args.sized {
        (metrics.width, metrics.height)
    } else {
        opening_size(
            Desktop::screen_size(args.display.as_deref()).unwrap_or((1600, 1000)),
            None,
        )
    };
    let metrics = ChromeMetrics {
        width: w,
        height: h,
        ..metrics
    };
    let desktop =
        Desktop::open(args.display.as_deref(), w, h, "Holonomy").map_err(WindowedError::Desktop)?;
    // **The screen size is printed too, and it is in this line for a reason.**
    //
    // A bug report of *"the window is way too small"* cannot be settled from the window's size alone.
    // The window is what the app asked for *after* reading the screen, so a window smaller than
    // `90%` of the screen means one of three things — `screen_size` reported wrong, the window
    // manager constrained the request, or the screenshot was scaled — and **they have three different
    // fixes**. The line below carries both numbers so that one run distinguishes them, which is the
    // measurement that `bugs/1.png` could not supply: a screenshot has no absolute scale in it.
    let asked = (w, h);
    let screen = Desktop::screen_size(args.display.as_deref());
    eprintln!(
        "holonomy: window {:#x} is {}x{}, opening on {} (asked {}x{}, screen {:?}, \
         {} of the screen)",
        desktop.window_id(),
        desktop.width(),
        desktop.height(),
        desktop.describe(),
        asked.0,
        asked.1,
        screen,
        match screen {
            Some((sw, sh)) if sw > 0 && sh > 0 => format!(
                "{}% x {}%",
                100 * desktop.width() as u32 / sw,
                100 * desktop.height() as u32 / sh
            ),
            _ => "unknown".to_string(),
        }
    );

    let mut session = Session::new(
        Editor::new(),
        Painter::new(atlas, 0),
        Box::new(desktop),
        metrics,
    );
    // **A zoom this atlas cannot do is reported, not clamped.** Part 43: `set_zoom` returns
    // `Result` because "125 % drawn under a '200 %' label" is a lie the label is uniquely positioned
    // to tell, and `--zoom` is exactly where such a lie would arrive from.
    if let Err(e) = session.set_zoom(args.zoom) {
        eprintln!("holonomy: {e}");
        eprintln!("holonomy: continuing at 100 %");
    }
    // **The document's name, in the title bar and in the sidebar.**
    //
    // **Part 39: the window said `untitled` and the sidebar said nothing at all**, whatever the user
    // passed. `--container notes.wavefunction` is a real path the app opens, and its name belongs on the
    // screen; `file_stem` and not the whole path because a title bar is not a place for `/home/…`.
    // The default is the same string `ChromeState::default` carries, so a window with no `--container`
    // looks exactly as it did.
    let name = args
        .container
        .as_deref()
        .and_then(|p| p.file_stem())
        .map_or_else(|| "untitled".to_string(), |s| s.to_string_lossy().into_owned());
    session.set_document(&name);
    session.repaint_all().map_err(WindowedError::Session)?;

    // **Part 36: dump the first frame, because "the viewport is too small" is not a number.** A layout
    // can be measured and still be wrong, and four of this part's claims were only settled by looking
    // at the pixels. Off unless named; a path, because `HOLONOMY_WINDOW_TRACE` is already the flag for
    // "tell me what the driver is doing" and this is the other half of it -- "show me what it drew".
    if let Some(path) = std::env::var_os("HOLONOMY_WINDOW_PPM") {
        let mut f = std::fs::File::create(&path).expect("create the dump");
        session.frame().to_ppm(&mut f).expect("write the dump");
        eprintln!("holonomy: first frame written to {}", path.to_string_lossy());
    }

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
                // **Every event's code, under the trace flag, and this is a diagnosis tool.**
                //
                // **Added in part 34 because the resize defect could not be located by reading the
                // code**: `Session::resize`, `Layout::for_size`, `Desktop::resize` and
                // `Event::decode` were each correct in isolation and the application still did not
                // resize. **The only question that settled it was "does a `ConfigureNotify` arrive at
                // all",** and answering that needs the event stream, not another reading of the source.
                //
                // **`{:?}` on the whole event rather than a code number**, because a bare `22` is a
                // claim and `ConfigureNotify { width: 1600, height: 1000 }` is the measurement.
                if std::env::var_os("HOLONOMY_WINDOW_TRACE").is_some() {
                    eprintln!("event {ev:?}");
                }
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
                        let changed = session
                            .resize(width as u32, height as u32)
                            .map_err(WindowedError::Session)?;
                        // **The four numbers that decide whether a resize happened, printed under the
                        // same flag as the event stream.**
                        //
                        // **`ConfigureNotify { width: 900, height: 700 }` is a claim about the server.**
                        // Whether the *session* believed it is a different question, and it was being
                        // answered by looking at a screenshot: a window that is 900 px wide and still
                        // showing the chrome for 1,600 is indistinguishable from one that resized and
                        // repainted wrongly. `Session::resize` returning `false` is the whole difference
                        // between those two, and nothing in the trace said so.
                        if std::env::var_os("HOLONOMY_WINDOW_TRACE").is_some() {
                            let f = session.frame_size();
                            let m = session.chrome_metrics();
                            eprintln!(
                                "resize: server {width}x{height}, changed {changed}, frame {}x{}, \
                                 metrics {}x{}, cell {}x{}, sidebar {}",
                                f.0,
                                f.1,
                                m.width,
                                m.height,
                                m.cell_w,
                                m.cell_h,
                                m.sidebar_w
                            );
                        }
                        if changed {
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
                        // **The same measurement as a motion, and the one that matters most:** "the
                        // buttons lag" is about a *click*, and a click's latency is this interval.
                        let at = if std::env::var_os("HOLONOMY_WINDOW_TRACE").is_some() {
                            Some(Instant::now())
                        } else {
                            None
                        };
                        session
                            .handle_pointer(&mut crate::store::NoSource, p)
                            .map_err(WindowedError::Session)?;
                        if let Some(at) = at {
                            eprintln!(
                                "press: {:.1} ms, damage {:?}, {} px, {} glyphs",
                                at.elapsed().as_secs_f64() * 1e3,
                                session.last_damage(),
                                session.paint_stats().pixels,
                                session.paint_stats().glyphs,
                            );
                        }
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
                        // **The pointer's own latency, printed under the same flag as the events.**
                        //
                        // *"The buttons lag" is the report; a frame count cannot answer it, because a
                        // slow frame and a fast frame paint the same number of times.** This is the
                        // interval from "the server handed us the event" to "the frame went out on the
                        // socket", which is the whole of what the user waits for — the session's
                        // dispatch, the chrome tree, the raster, and the `PutImage`.
                        let at = if std::env::var_os("HOLONOMY_WINDOW_TRACE").is_some() {
                            Some(Instant::now())
                        } else {
                            None
                        };
                        session
                            .handle_pointer(&mut crate::store::NoSource, p)
                            .map_err(WindowedError::Session)?;
                        if let Some(at) = at {
                            eprintln!(
                                "pointer: {:.1} ms, damage {:?}, {} px, {} glyphs",
                                at.elapsed().as_secs_f64() * 1e3,
                                session.last_damage(),
                                session.paint_stats().pixels,
                                session.paint_stats().glyphs,
                            );
                        }
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
