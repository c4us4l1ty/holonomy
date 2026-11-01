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
use crate::session::{DocumentRequest, Exit, Session};

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
    #[cfg(debug_assertions)]
    eprintln!("holonomy: debug build — use --release for responsive painting");
    let (w, h) = if args.sized { (metrics.width, metrics.height) } else {
        opening_size(Desktop::screen_size(args.display.as_deref()).unwrap_or((1600, 1000)), None)
    };
    let metrics = ChromeMetrics { width: w, height: h, ..metrics };
    let desktop = Desktop::open(args.display.as_deref(), w, h, "Holonomy").map_err(WindowedError::Desktop)?;
    let mut session = Session::new(Editor::new(), Painter::new(atlas, 0), Box::new(desktop), metrics);
    session.set_managed_documents(true);
    if let Err(e) = session.set_zoom(args.zoom) { eprintln!("holonomy: {e}"); }
    let path = args.container.clone().unwrap_or_else(|| std::path::PathBuf::from("holonomy.wavefunction"));
    let mut vault: Option<crate::vault::Vault> = None;
    let mut form: Option<Form> = Some(if path.exists() { Form::unlock(path.clone()) } else { Form::setup(path.clone()) });
    if let Ok(pass) = std::env::var("HOLONOMY_PASSPHRASE") {
        if !pass.is_empty() && path.exists() {
            match crate::vault::Vault::open(&path, &pass, holonomy_crypto::envelope::VDF_ITERATIONS) {
                Ok(v) => { v.load_all(&mut session).map_err(|e| WindowedError::Session(crate::session::SessionError::Sink(std::io::Error::other(e.to_string()))))?; vault = Some(v); form = None; }
                Err(e) => { if let Some(f) = form.as_mut() { f.error = Some(e.to_string()); } }
            }
        }
    }
    let (mut min_keycode, mut mapping) = window_of(&mut session)?.keyboard_mapping().map_err(WindowedError::Desktop)?;
    sync_form(&mut session, form.as_ref());
    session.repaint_all().map_err(WindowedError::Session)?;
    let start = Instant::now();
    let mut events = 0u64;
    let mut last_edit = Instant::now();
    loop {
        if let Some(f) = form.as_ref() { sync_form(&mut session, Some(f)); }
        if vault.is_some() && form.is_none() {
            if session.take_save_request() || (session.has_unsaved_documents() && last_edit.elapsed() >= Duration::from_millis(800)) {
                save(&mut vault, &mut session);
            }
            if let Some(req) = session.take_document_request() {
                match req {
                    DocumentRequest::New => { form = Some(Form::new_doc()); }
                    DocumentRequest::Rename => { form = Some(Form::rename(session.state.title.clone())); }
                    DocumentRequest::Delete => { form = Some(Form::delete(session.state.title.clone(), session.active_document_id())); }
                    DocumentRequest::Close => {
                        if save(&mut vault, &mut session) { let id = session.active_document_id(); session.close_document(id); session.set_notice(Some("Document closed. File > Open lists saved documents.".into())); }
                    }
                    DocumentRequest::Lock => {
                        if save(&mut vault, &mut session) { let p = vault.as_ref().map(|v| v.path().to_path_buf()).unwrap_or_else(|| path.clone()); lock(&mut vault, &mut session, &p); form = Some(Form::unlock(p)); }
                    }
                    DocumentRequest::OpenContainer => { form = Some(Form::open(&vault, &session)); }
                    DocumentRequest::ImportText => { form = Some(Form::import()); }
                    DocumentRequest::DocumentMenu(i) => {
                        session.switch_document_public(i);
                        form = Some(Form::doc_menu(session.state.title.clone(), session.active_document_id()));
                    }
                }
                sync_form(&mut session, form.as_ref());
            }
        }
        let event = { let desktop = window_of(&mut session)?; desktop.next_event(EVENT_WAIT).map_err(WindowedError::Desktop)? };
        let Some(ev) = event else {
            session.tick().map_err(WindowedError::Session)?; continue;
        };
        events += 1;
        if std::env::var_os("HOLONOMY_WINDOW_TRACE").is_some() { eprintln!("event {ev:?}"); }
        match ev {
            holonomy_x11::Event::KeyPress { keycode, state, .. } => {
                session.sync_modifiers(x11key::modifier_state(state));
                let ch = x11key::layout_char(&mapping, min_keycode, keycode, state);
                if let Some(f) = form.as_mut() {
                    let outcome = f.key(keycode, state, ch);
                    match outcome {
                        KeyOutcome::Changed => sync_form(&mut session, Some(f)),
                        KeyOutcome::Cancel => { if vault.is_none() { return Ok(Exit::Quit); } form = None; sync_form(&mut session, None); }
                        KeyOutcome::Submit => {
                            let f = form.take().unwrap();
                            match f.clone().submit(&mut vault, &mut session, holonomy_crypto::envelope::VDF_ITERATIONS) {
                                Ok(next) => { form = next; session.set_notice(None); last_edit = Instant::now(); }
                                Err(e) => { let mut f = f; f.error = Some(e); form = Some(f); }
                            }
                            sync_form(&mut session, form.as_ref());
                        }
                    }
                    continue;
                }
                if vault.is_none() { continue; }
                if state & (4 | 8) == 0 {
                    if let Some(c) = ch { session.insert_text(&c.to_string()).map_err(WindowedError::Session)?; last_edit = Instant::now(); continue; }
                }
                if let Some(key) = x11key::key_event(&ev) {
                    if let Some(Exit::Quit) = session.handle_event(key).map_err(WindowedError::Session)? {
                        if save(&mut vault, &mut session) { report(&session, events, start); return Ok(Exit::Quit); }
                        session.set_notice(Some("Save failed — not quitting. Fix the error and try again.".into()));
                    }
                    if session.has_unsaved_documents() { last_edit = Instant::now(); }
                }
            }
            holonomy_x11::Event::KeyRelease { state, .. } => {
                session.sync_modifiers(x11key::modifier_state(state));
                if form.is_none() { if let Some(key) = x11key::key_event(&ev) { session.handle_event(key).map_err(WindowedError::Session)?; } }
            }
            holonomy_x11::Event::ConfigureNotify { width, height } => {
                if session.resize(width as u32, height as u32).map_err(WindowedError::Session)? { session.repaint_all().map_err(WindowedError::Session)?; }
            }
            holonomy_x11::Event::Expose { .. } => { session.repaint_all().map_err(WindowedError::Session)?; }
            holonomy_x11::Event::ButtonPress { button, event_x, event_y, .. } => {
                window_of(&mut session)?.focus().map_err(WindowedError::Desktop)?;
                if button != 1 { continue; }
                if let Some(f) = form.as_mut() {
                    let l = holonomy_render::Layout::new(&session.chrome_metrics());
                    let view = f.view();
                    match holonomy_render::dialog::hit(&l, &view, event_x as i32, event_y as i32) {
                        holonomy_render::dialog::Hit::Field(i) => { f.focus = i; sync_form(&mut session, Some(f)); }
                        holonomy_render::dialog::Hit::Choice(i) => { f.choice = i; sync_form(&mut session, Some(f)); }
                        holonomy_render::dialog::Hit::Confirm => {
                            let f = form.take().unwrap();
                            match f.clone().submit(&mut vault, &mut session, holonomy_crypto::envelope::VDF_ITERATIONS) {
                                Ok(next) => { form = next; session.set_notice(None); }
                                Err(e) => { let mut f = f; f.error = Some(e); form = Some(f); }
                            }
                            sync_form(&mut session, form.as_ref());
                        }
                        holonomy_render::dialog::Hit::Cancel => { if vault.is_none() { return Ok(Exit::Quit); } form = None; sync_form(&mut session, None); }
                        _ => {}
                    }
                    continue;
                }
                if vault.is_some() {
                    if let Some(p) = x11key::pointer_event(&ev) { session.handle_pointer(&mut crate::store::NoSource, p).map_err(WindowedError::Session)?; }
                }
            }
            ev @ (holonomy_x11::Event::ButtonRelease { .. } | holonomy_x11::Event::MotionNotify { .. }) => {
                if form.is_none() && vault.is_some() { if let Some(p) = x11key::pointer_event(&ev) { session.handle_pointer(&mut crate::store::NoSource, p).map_err(WindowedError::Session)?; } }
            }
            holonomy_x11::Event::ClientMessage { data1, .. } if data1 == WM_DELETE_WINDOW => {
                if vault.is_none() || save(&mut vault, &mut session) { report(&session, events, start); return Ok(Exit::Quit); }
                session.set_notice(Some("Save failed — window kept open so your edits are not lost.".into()));
            }
            holonomy_x11::Event::Other { code: 10 } => session.reset_modifiers(),
            holonomy_x11::Event::Other { code: 34 } => {
                if let Ok((min, map)) = window_of(&mut session)?.keyboard_mapping() { min_keycode = min; mapping = map; }
                session.reset_modifiers();
            }
            _ => {}
        }
    }
}

fn sync_form(session: &mut Session<'_>, form: Option<&Form>) { session.set_dialog(form.map(Form::view)); }
fn save(vault: &mut Option<crate::vault::Vault>, session: &mut Session<'_>) -> bool {
    let Some(v) = vault.as_mut() else { return true; };
    match v.save(session) { Ok(()) => { session.set_notice(Some("Saved".into())); true } Err(e) => { session.set_notice(Some(format!("Save failed: {e}"))); false } }
}
fn lock(vault: &mut Option<crate::vault::Vault>, session: &mut Session<'_>, _path: &std::path::Path) {
    *vault = None;
    let _ = session.replace_documents(Vec::new());
    session.clear_image_cache();
    session.reset_modifiers();
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FormKind { Setup, SetupNormal, Unlock, New, Rename, Delete, Open, OpenPath, Import, DocMenu }
#[derive(Clone)]
struct Form {
    kind: FormKind, path: std::path::PathBuf, values: Vec<zeroize::Zeroizing<String>>,
    focus: usize, choice: usize, choices: Vec<String>, ids: Vec<u64>, error: Option<String>, target_id: u64,
}
enum KeyOutcome { Changed, Cancel, Submit }
impl Form {
    fn base(kind: FormKind, path: std::path::PathBuf, n: usize) -> Self { Self { kind, path, values: (0..n).map(|_| zeroize::Zeroizing::new(String::new())).collect(), focus: 0, choice: 0, choices: Vec::new(), ids: Vec::new(), error: None, target_id: 0 } }
    fn setup(path: std::path::PathBuf) -> Self { Self::base(FormKind::Setup, path, 4) }
    fn unlock(path: std::path::PathBuf) -> Self { Self::base(FormKind::Unlock, path, 1) }
    fn new_doc() -> Self { let mut f = Self::base(FormKind::New, "".into(), 1); f.values[0].push_str("Untitled"); f }
    fn rename(title: String) -> Self { let mut f = Self::base(FormKind::Rename, "".into(), 1); f.values[0].push_str(&title); f }
    fn delete(title: String, id: u64) -> Self { let mut f = Self::base(FormKind::Delete, "".into(), 0); f.choices = vec![format!("Delete '{title}' permanently")]; f.target_id = id; f }
    fn import() -> Self { Self::base(FormKind::Import, "".into(), 1) }
    fn open(vault: &Option<crate::vault::Vault>, _session: &Session<'_>) -> Self {
        let mut f = Self::base(FormKind::Open, "".into(), 1);
        if let Some(v) = vault { for d in v.documents() { f.choices.push(d.title.clone()); f.ids.push(d.id); } }
        f.choices.push("Open another container…".into()); f.focus = 0; f
    }
    fn doc_menu(title: String, id: u64) -> Self { let mut f = Self::base(FormKind::DocMenu, "".into(), 0); f.choices = vec!["Rename".into(), "Delete…".into(), "Close".into()]; f.target_id = id; f.values.push(zeroize::Zeroizing::new(title)); f }
    fn view(&self) -> holonomy_render::dialog::DialogView {
        let (title, message, labels, secrets, confirm, cancel) = match self.kind {
            FormKind::Setup => ("Set up top-secret space", "Step 1 of 2. This passphrase opens your private documents.", vec!["Top-secret passphrase", "Confirm top-secret"], vec![true,true], "Next", Some("Quit")),
            FormKind::SetupNormal => ("Set up normal space", "Step 2 of 2. A different passphrase opens everyday documents.", vec!["Normal passphrase", "Confirm normal"], vec![true,true], "Create", Some("Quit")),
            FormKind::Unlock => ("Unlock", "Enter one passphrase. Only its own documents are mounted.", vec!["Passphrase"], vec![true], "Unlock", Some("Quit")),
            FormKind::New => ("New document", "Stored encrypted in the currently unlocked space.", vec!["Title"], vec![false], "Create", Some("Cancel")),
            FormKind::Rename => ("Rename document", "The title is encrypted with the document.", vec!["Title"], vec![false], "Rename", Some("Cancel")),
            FormKind::Delete => ("Delete document", "This rewrites the encrypted vault and cannot be undone.", vec![], vec![], "Delete", Some("Cancel")),
            FormKind::Open => ("Open", "Choose a saved document, or open another container.", vec!["Other container path (optional)"], vec![false], "Open", Some("Cancel")),
            FormKind::OpenPath => ("Open container", "The current vault is saved and locked before switching.", vec!["Container path"], vec![false], "Next", Some("Cancel")),
            FormKind::Import => ("Import text", "Import a UTF-8 text file into this unlocked space.", vec!["File path"], vec![false], "Import", Some("Cancel")),
            FormKind::DocMenu => ("Document", "Choose an action for this document.", vec![], vec![], "Choose", Some("Cancel")),
        };
        let offset = if self.kind == FormKind::SetupNormal { 2 } else { 0 };
        let fields = labels.iter().enumerate().map(|(i,label)| { let j=i+offset; holonomy_render::dialog::Field { label: (*label).into(), value: if secrets[i] { "•".repeat(self.values.get(j).map(|s| s.chars().count()).unwrap_or(0)) } else { self.values.get(j).map(|s| s.as_str()).unwrap_or("").into() }, secret: secrets[i] } }).collect();
        holonomy_render::dialog::DialogView { title: title.into(), message: message.into(), fields, focus: self.focus, error: self.error.clone(), confirm: confirm.into(), cancel: cancel.map(str::to_string), choices: self.choices.clone(), choice: self.choice }
    }
    fn key(&mut self, keycode: u8, state: u16, ch: Option<char>) -> KeyOutcome {
        let code = keycode.saturating_sub(8);
        let value_index = self.focus + if self.kind == FormKind::SetupNormal { 2 } else { 0 };
        match code {
            1 => return KeyOutcome::Cancel,
            15 => { let n = match self.kind { FormKind::Setup | FormKind::SetupNormal => 2, _ => self.values.len().max(1) }; self.focus = if state & 1 != 0 { (self.focus + n - 1) % n } else { (self.focus + 1) % n }; return KeyOutcome::Changed; }
            28 | 96 => return KeyOutcome::Submit,
            103 if !self.choices.is_empty() => { self.choice = self.choice.saturating_sub(1); return KeyOutcome::Changed; }
            108 if !self.choices.is_empty() => { self.choice = (self.choice + 1).min(self.choices.len()-1); return KeyOutcome::Changed; }
            14 => { if let Some(v) = self.values.get_mut(value_index) { v.pop(); } return KeyOutcome::Changed; }
            _ => {}
        }
        if state & (4|8) == 0 { if let Some(c) = ch { if let Some(v) = self.values.get_mut(value_index) { if v.len() < 4096 { v.push(c); } } return KeyOutcome::Changed; } }
        KeyOutcome::Changed
    }
    fn submit(self, vault: &mut Option<crate::vault::Vault>, session: &mut Session<'_>, vdf: u64) -> Result<Option<Form>, String> {
        let val = |i: usize| self.values.get(i).map(|s| s.as_str()).unwrap_or("");
        match self.kind {
            FormKind::Setup => {
                if val(0).is_empty() { return Err("Enter a passphrase.".into()); }
                if val(0) != val(1) { return Err("The passphrases do not match.".into()); }
                let mut next = self; next.kind = FormKind::SetupNormal; next.focus = 0; next.error = None; Ok(Some(next))
            }
            FormKind::SetupNormal => {
                crate::vault::Vault::create(&self.path, val(0), val(1), val(2), val(3), vdf).map_err(|e| e.to_string())?;
                Ok(Some(Form::unlock(self.path)))
            }
            FormKind::Unlock => {
                let v = crate::vault::Vault::open(&self.path, val(0), vdf).map_err(|e| e.to_string())?;
                v.load_all(session).map_err(|e| e.to_string())?; *vault = Some(v); Ok(None)
            }
            FormKind::New => {
                let title = val(0).trim().to_string(); if title.is_empty() || title.chars().count() > 128 || title.chars().any(char::is_control) { return Err("Enter a title of 1–128 characters.".into()); }
                let Some(v) = vault.as_mut() else { return Err("Unlock first.".into()); };
                let id = v.next_id(); session.create_document(id, title);
                if !save(vault, session) { return Err("Document created in memory but not saved; retry Ctrl+S.".into()); }
                Ok(None)
            }
            FormKind::Rename => {
                let title = val(0).trim().to_string(); if title.is_empty() || title.chars().count() > 128 || title.chars().any(char::is_control) { return Err("Enter a title of 1–128 characters.".into()); }
                session.rename_document(title); save(vault, session); Ok(None)
            }
            FormKind::Delete => {
                let Some(v) = vault.as_mut() else { return Err("Unlock first.".into()); };
                v.delete(session, self.target_id).map_err(|e| e.to_string())?; session.set_notice(Some("Document deleted.".into())); Ok(None)
            }
            FormKind::Import => {
                let p = val(0).trim(); if p.is_empty() { return Err("Enter a file path.".into()); }
                let bytes = std::fs::read(p).map_err(|e| e.to_string())?;
                let text = std::str::from_utf8(&bytes).map_err(|_| "Import expects UTF-8 text.".to_string())?;
                let Some(v) = vault.as_mut() else { return Err("Unlock first.".into()); };
                let id = v.next_id(); let title = std::path::Path::new(p).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Imported".into());
                session.create_document(id, title); session.insert_text(text).map_err(|e| e.to_string())?; save(vault, session); Ok(None)
            }
            FormKind::Open => {
                let p = val(0).trim();
                if !p.is_empty() {
                    if !save(vault, session) { return Err("Save failed; current vault kept open.".into()); }
                    let path = std::path::PathBuf::from(p); lock(vault, session, &path); return Ok(Some(Form::unlock(path)));
                }
                if self.choice < self.ids.len() {
                    let id = self.ids[self.choice]; let v = vault.as_ref().ok_or("Unlock first.")?;
                    let d = v.documents().iter().find(|d| d.id == id).ok_or("Document missing.")?;
                    session.open_stored_document(d.id, d.title.clone(), &d.payload, d.caret).map_err(|e| e.to_string())?; return Ok(None);
                }
                let mut f = Form::base(FormKind::OpenPath, "".into(), 1); f.values[0].push_str("holonomy.wavefunction"); Ok(Some(f))
            }
            FormKind::OpenPath => {
                let p = val(0).trim(); if p.is_empty() { return Err("Enter a container path.".into()); }
                if !save(vault, session) { return Err("Save failed; current vault kept open.".into()); }
                let path = std::path::PathBuf::from(p); lock(vault, session, &path); Ok(Some(if path.exists() { Form::unlock(path) } else { Form::setup(path) }))
            }
            FormKind::DocMenu => match self.choice {
                0 => Ok(Some(Form::rename(self.values[0].to_string()))),
                1 => Ok(Some(Form::delete(self.values[0].to_string(), self.target_id))),
                _ => { if save(vault, session) { session.close_document(self.target_id); } Ok(None) }
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
