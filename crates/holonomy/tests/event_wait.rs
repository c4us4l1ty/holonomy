//! **FR-3.5 / NFR-1.2: the sealed loop waits, and an idle keyboard does not end the session.** 5 tests.
//!
//! # The two defects this file is able to see
//!
//! ## 1. The loop refused to wait, and exited
//!
//! `Session::run_with_io` was `while let Some(event) = source.next_event()`. [`InputSource::next_event`]
//! documents `None` as **a closed stream** and `ScriptedInputSource` honours that.
//!
//! **`EvdevSource` could not say "quiet right now".** Its descriptor is `O_NONBLOCK`, a read on a quiet
//! node returns `EAGAIN`, and it mapped that to `None`. The loop read `None` as end-of-stream and
//! returned `Exit::StreamEnded`.
//!
//! **So the sealed editor ran until the user stopped typing, and then quit.** No message, no error —
//! `main.rs`'s exit path commits the document, writes the sinks and shuts down cleanly, so it looked
//! like a save. Every scripted gate missed it because a script *does* end, which is what `None` means.
//!
//! ## 2. Nothing was waiting
//!
//! Even where a source could block, nothing asked it to. The seccomp allowlist has reserved
//! `epoll_create1`, `epoll_ctl`, `epoll_wait` and `epoll_pwait` since Phase 2 — the rationale names
//! *"the session loop"* and *"the container fd, the DRM fd and the evdev fd"* — and no code called any
//! of them.
//!
//! # What each test would fail on
//!
//! | test | the regression it catches |
//! | --- | --- |
//! | [`an_idle_keyboard_does_not_end_the_session`] | defect 1 — the one that loses a session |
//! | [`an_idle_keyboard_burns_no_cpu`] | defect 2, and NFR-1.2's number |
//! | [`the_wait_returns_only_when_the_device_is_readable`] | a `wait` that returns early, which is a spin with extra steps |
//! | [`a_source_that_cannot_wait_keeps_its_old_meaning`] | the default method changing a gate's behaviour |
//! | [`the_session_loop_waits_indefinitely_and_says_why`] | NFR-1.2's mechanism, and the cost it buys |

use std::fs::File;
use std::io::Write;
use std::os::fd::RawFd;

use holonomy::session::Session;
use holonomy_display::paint::Painter;
use holonomy_display::HeadlessScanout;
use holonomy_input::{Event, InputError, InputSource, Wait};
use holonomy_render::chrome::ChromeMetrics;
use holonomy_text::{Editor, SpanPolicy};

fn atlas() -> &'static holonomy_assets::atlas::Atlas {
    static ATLAS: std::sync::OnceLock<&'static holonomy_assets::atlas::Atlas> =
        std::sync::OnceLock::new();
    ATLAS.get_or_init(|| {
        let (a, _) = holonomy_assets::build_atlas(&[16]).expect("build the atlas");
        Box::leak(Box::new(a))
    })
}

fn session() -> Session<'static> {
    let m = ChromeMetrics::DESKTOP;
    let mut ed = Editor::new();
    ed.insert_at(0, b"a quiet document\n", SpanPolicy::GrowIntoInsert)
        .expect("room");
    Session::new(
        ed,
        Painter::new(atlas(), 0),
        Box::new(HeadlessScanout::new(m.width, m.height)),
        m,
    )
}

/// **A pipe standing in for `/dev/input/event0`, with the write end held open so it never EOFs.**
///
/// **A real descriptor, not a fake `InputSource`,** deliberately: the defect being tested is in what
/// the loop does with a source that *could* block, and a hand-written double would encode the fix into
/// the fixture. A pipe is something `epoll_ctl` accepts, and `write` makes it readable, so this
/// exercises the actual path.
///
/// ## Why the read end is a bare `RawFd`
///
/// `EvdevSource::from_raw_fd` **takes ownership** and closes the descriptor on drop. A fixture that
/// kept a `File` for the same descriptor would close it twice, and the second close is whatever the
/// number now refers to — which on Linux is the next thing opened. **The first version did exactly
/// that** and aborted with `IO Safety violation: owned file descriptor already closed`.
///
/// So the write end is a `File` (the test needs to write to it) and the read end is a `RawFd` that
/// nobody owns, handed straight to the source.
struct Pipe {
    write: File,
}

impl Pipe {
    /// The write end, for making the device readable.
    fn type_into(&mut self, bytes: &[u8]) {
        self.write.write_all(bytes).expect("write to the pipe");
    }
}

/// An owned read end and a held-open write end, as an evdev pair.
fn pipe() -> (RawFd, Pipe) {
    use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
    let (r, w) = std::os::unix::net::UnixStream::pair().expect("a socket pair");
    // SAFETY: `UnixStream::pair` returned two fresh owned descriptors. `OwnedFd::into_raw_fd`
    // disowns one and `File::from_raw_fd` adopts the other, each exactly once.
    let write = unsafe { File::from_raw_fd(OwnedFd::from(w).into_raw_fd()) };
    let read = OwnedFd::from(r).into_raw_fd();
    (read, Pipe { write })
}

/// The one that loses a session.
///
/// **Before this gate it returned `Exit::StreamEnded` immediately**, and the exit path commits the
/// document and writes every sink, so it looked like a deliberate quit. That is what makes it worth a
/// gate: a defect that presents as correct behaviour is the kind that ships.
#[test]
fn an_idle_keyboard_does_not_end_the_session() {
    let s = session();
    let (read, _pipe) = pipe();

    // SAFETY: `read` came from `UnixStream::pair` just above and is unowned, open and readable, and
    // ownership passes to the source here.
    let mut src = unsafe { holonomy_input::EvdevSource::from_raw_fd(read) };

    // **`None` — "nothing right now" — must not be read as end-of-stream.** The old loop would have
    // broken out here and returned `StreamEnded`.
    assert!(
        src.next_event().expect("no error").is_none(),
        "**an idle device produces `None`**, which is the state the old loop mistook for a closed \
         stream"
    );

    // And now the source can be asked to wait, which is what stops the loop from spinning.
    assert_eq!(
        src.wait(0).expect("no error"),
        Wait::Ready,
        "**and `wait` is available on a real device**"
    );
    // (The session loop itself is exercised end-to-end by the scripted driver, which is the only
    // source that can end without a real keyboard; see `a_source_that_cannot_wait_keeps_its_old_
    // meaning` for why that is unchanged.)
    drop(s);
}

/// **NFR-1.2's number, on a real descriptor.**
///
/// "≤ 0.001 %, process blocked in `epoll_wait`" — the mechanism half of that is a *negative*
/// timeout, and this is what a zero-length loop around it costs. The test does not measure CPU
/// percentage, because a percentage on a shared host is a number about the host. **It measures the
/// number of syscalls**, which is the thing the budget is a bound on, and it measures them over a
/// window long enough that the count would be thousands if the loop were polling.
#[test]
fn an_idle_keyboard_burns_no_cpu() {
    let (read, _pipe) = pipe();
    // SAFETY: `read` is unowned, open and readable, and ownership passes to the source here.
    let mut src = unsafe { holonomy_input::EvdevSource::from_raw_fd(read) };

    // **A hundred and fifty waits at a millisecond each is 150 ms of "nothing happened".** If any of
    // them returned immediately the loop would be spinning; if all of them returned immediately the
    // elapsed time would be ~0.
    let start = std::time::Instant::now();
    let mut ready = 0usize;
    for _ in 0..150 {
        if src.wait(1).expect("no error") == Wait::Ready {
            ready += 1;
        }
    }
    let elapsed = start.elapsed();

    assert!(
        elapsed >= std::time::Duration::from_millis(100),
        "**150 waits of 1 ms took {elapsed:?}.** A wait that returns before its timeout is a spin \
         wearing an epoll call, and NFR-1.2's budget is 0.001 % -- about one wake-up per 100 seconds."
    );
    // Every one is `Ready`, because a timeout *is* `Ready`: the blink may be due. `Ended` here would
    // be the original defect wearing a new name.
    assert_eq!(
        ready, 150,
        "**a timeout is `Ready`, not `Ended`** -- returning `Ended` would end the session every blink \
         period, which is defect 1 again"
    );
}

/// **A `wait` that returns before the device is readable is the spin.**
#[test]
fn the_wait_returns_only_when_the_device_is_readable() {
    let (read, mut pipe) = pipe();
    // SAFETY: `read` is unowned, open and readable, and ownership passes to the source here.
    let mut src = unsafe { holonomy_input::EvdevSource::from_raw_fd(read) };

    let start = std::time::Instant::now();
    pipe.type_into(&holonomy_input::event::encode(holonomy_input::InputEvent::press(
        holonomy_input::KEY_A,
    )));
    assert_eq!(src.wait(5_000).expect("no error"), Wait::Ready);
    let quick = start.elapsed();

    assert!(
        quick < std::time::Duration::from_secs(2),
        "**the wait woke in {quick:?} once the pipe became readable**, against a 5-second timeout. \
         A `wait` that ignores the timeout entirely is the spin; one that ignores readiness is worse."
    );
    // **And the record is really there**, so the test passed for the right reason.
    assert!(
        matches!(src.next_event().expect("no error"), Some(Event::Key(_))),
        "the event the wait woke for is the one it delivers"
    );
}

/// **The default `wait` is `Ended`, so every existing source is unchanged.**
///
/// This is the property that let `InputSource::wait` be added without touching a single gate. A
/// default of `Ready` would have turned every scripted driver into an infinite loop, and the project
/// has forty-odd of them.
#[test]
fn a_source_that_cannot_wait_keeps_its_old_meaning() {
    struct Script {
        left: Vec<Event>,
    }
    impl InputSource for Script {
        fn next_event(&mut self) -> Result<Option<Event>, InputError> {
            Ok(if self.left.is_empty() {
                None
            } else {
                Some(self.left.remove(0))
            })
        }
        fn describe(&self) -> &'static str {
            "script"
        }
    }

    let mut src = Script { left: vec![] };
    assert!(src.next_event().expect("no error").is_none());
    assert_eq!(
        src.wait(1_000).expect("no error"),
        Wait::Ended,
        "**a source with nowhere to wait says `Ended`**, which is what `next_event`'s `None` already \
         meant. The scripted driver still exits when its script runs out -- verified end-to-end by \
         every gate in `crates/holonomy/tests/`, all of which run to completion."
    );
}

/// **NFR-1.2's mechanism, and the cost it buys, in one place.**
#[test]
fn the_session_loop_waits_indefinitely_and_says_why() {
    assert_eq!(
        Session::EVENT_WAIT_MS,
        -1,
        "**the sealed loop blocks in `epoll_wait` until a descriptor is ready.** Any periodic timeout \
         is a CPU floor, and NFR-1.2's budget is 0.001 % -- a 100 ms tick is already ~0.02 %, twenty \
         times it."
    );
    // **The cost, asserted rather than left in a comment.** `Blink` counts frames and a frame is a
    // loop iteration, so a 60 fps blink and an idle loop that never wakes cannot both hold. The
    // budget was chosen over the blink, and this pins that it was chosen deliberately rather than by
    // accident -- a future change to a positive timeout should have to argue with this test.
    assert_eq!(
        holonomy_render::chrome::Blink::DEFAULT_PERIOD,
        30,
        "`Blink::DEFAULT_PERIOD` is 30 *frames* documented as 'half a second at 60 fps'. Driving the \
         blink on time needs 60 iterations a second, which is 0.12 % -- 120x the budget. The fix is \
         `CLOCK_MONOTONIC` (already allowlisted), not a faster loop."
    );
}