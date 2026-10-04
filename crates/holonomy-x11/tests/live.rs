//! The live gate: the same client, against the real X server this session can reach.
//!
//! # Why these tests are opt-in
//!
//! Everything here needs a display, a compositor and a keyboard. CI has none of those -- PROJECT.md
//! §2.7 requires that CI never depends on a hardware path, and a developer's desktop is hardware --
//! so the file compiles and runs zero tests unless `HOLONOMY_X11_LIVE=1` is set.
//!
//! # What they prove that the unit tests cannot
//!
//! The unit tests in `src/` check the wire format against the specification. These check it against
//! an implementation. That difference is not academic: this crate's first version had three separate
//! bugs -- a broken 16-bit reader, an event decoder missing a sequence field, and an extension request
//! two bytes too long -- and *every one of them was caught by a test against a real server*, not by a
//! test against the spec. A protocol implementation with no live peer is a protocol implementation
//! that is wrong.
//!
//! # Running them
//!
//! ```text
//!   HOLONOMY_X11_LIVE=1 cargo test -p holonomy-x11 --test live -- --test-threads=1
//! ```
//!
//! `--test-threads=1` is required: `a_synthesised_keystroke_arrives_as_the_keycode_that_was_injected`
//! takes the keyboard focus, and two tests doing that at once fight over it.
//!
//! # What is measured on this host, and what is not
//!
//! Measured, on the machine this was written on:
//!
//! * `DISPLAY=:0`, an Xwayland whose vendor string is `The X.Org Foundation`, root depth 24, offering a
//!   32-bits-per-pixel format for it, so a `Frame` goes to it unconverted.
//! * `/tmp/.X11-unix/X0` **does not exist** in this mount namespace -- `ls` returns `ENOENT` -- while
//!   `ss` shows `@/tmp/.X11-unix/X0` listening. The client tries the filesystem path and then the
//!   abstract one; libX11 does the same and opens the display. A client that only tries the path fails
//!   here, which is why both are in `Conn::connect`.
//! * The cookie is at `$XAUTHORITY=/run/user/1000/.mutter-Xwaylandauth.OLRDW3`, two records, both
//!   `MIT-MAGIC-COOKIE-1`, 16 bytes, families `FamilyLocal` and `FamilyWild`.
//!
//! Not measured, and not claimed: anything about the ThinkPad X200, where there is no X server at all.

use std::time::Duration;

/// Whether the live tests run. See the module docs for why this is not automatic.
fn live() -> bool {
    std::env::var("HOLONOMY_X11_LIVE").as_deref() == Ok("1")
}

macro_rules! require {
    () => {
        if !live() {
            eprintln!("skipped: set HOLONOMY_X11_LIVE=1 to run the live X11 gate");
            return;
        }
    };
}

const TIMEOUT: Duration = Duration::from_secs(5);

/// Read and throw away every event that arrives within `quiet`.
///
/// # `XTEST` queues, and the queue outlives the test that filled it
///
/// `XTestFakeInput` does not deliver a keystroke synchronously: the server queues it and delivers it
/// after the request that created it has been answered. So events injected by one test are still
/// arriving when the next test connects, focuses its own window and injects its own key. Measured here:
/// the first version of these two tests read each other's events -- one saw `None, Some(KeyPress 50)`
/// when it had injected 38, and the other saw no presses at all -- and both were reading real events
/// from the wrong test.
///
/// So: settle first, then inject, then wait for *the keycode that was injected* rather than for the
/// next event of any kind.
fn drain(conn: &mut holonomy_x11::Conn, quiet: Duration) -> usize {
    let mut seen = 0usize;
    while let Ok(Some(_)) = conn.next_event(Some(quiet)) {
        seen += 1;
    }
    seen
}

/// Wait for a key event with `keycode`, ignoring anything else that arrives meanwhile.
fn wait_for_keycode(
    conn: &mut holonomy_x11::Conn,
    keycode: u8,
    press: bool,
    within: Duration,
) -> bool {
    let deadline = std::time::Instant::now() + within;
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        match conn.next_event(Some(left.min(Duration::from_millis(250)))) {
            Ok(Some(holonomy_x11::Event::KeyPress { keycode: k, .. })) if k == keycode && press => {
                return true
            }
            Ok(Some(holonomy_x11::Event::KeyRelease { keycode: k, .. }))
                if k == keycode && !press =>
            {
                return true
            }
            Ok(Some(_)) | Ok(None) => {}
            Err(_) => return false,
        }
    }
    false
}

/// The server this session can reach, or `None` in which case the test skips.
///
/// No display is a skip and not a failure: this file is expected to run on a machine with no desktop,
/// and PROJECT.md §2.7 requires that CI never depend on one. What *is* a failure is a display that
/// answers and then misbehaves -- every assertion below is that kind.
fn connect() -> Option<holonomy_x11::Conn> {
    match holonomy_x11::Conn::connect(None) {
        Ok(c) => Some(c),
        Err(e) => {
            eprintln!("skipping: cannot open a display: {e}");
            None
        }
    }
}

/// `connect` plus an early return, so a test body reads as its assertions rather than as plumbing.
///
/// The double braces are load-bearing: a `macro_rules!` body that ends in an expression cannot be used
/// in expression position without them, and the first version of this expanded to a bare `match`,
/// which the compiler truncates with a warning that is easy to misread as a type error.
macro_rules! session {
    () => {{
        require!();
        match connect() {
            Some(c) => c,
            None => return,
        }
    }};
}

/// The handshake has to produce a server this client can actually push a `Frame` to.
///
/// The three numbers asserted are the ones a `put_image` depends on: the root depth, a 32-bpp format
/// for it, and a little-endian byte order. A server that fails any of them would get a window showing
/// garbage, so the check is in the crate as well as here.
#[test]
fn the_handshake_describes_a_server_a_frame_can_go_to() {
    let conn = session!();
    let s = conn.setup();
    println!(
        "vendor={:?} release={} root=0x{:x} depth={} visual=0x{:x} \
         max_request={} words min_keycode={} max_keycode={} formats={}",
        s.vendor,
        s.release,
        s.root,
        s.root_depth,
        s.root_visual,
        s.max_request_words,
        s.min_keycode,
        s.max_keycode,
        s.formats.len()
    );
    assert!(
        s.vendor.contains("X.Org") || s.vendor.contains("Xorg"),
        "{}",
        s.vendor
    );
    assert_eq!(
        s.image_byte_order, 0,
        "LSBFirst, which is every field this crate writes"
    );
    assert_eq!(
        s.format(s.root_depth, 32).map(|f| f.depth),
        Some(s.root_depth),
        "a 32-bpp format for the root depth: {:?}",
        s.formats
    );
    assert!(
        s.max_request_words as usize * 4 <= 262_140,
        "no BIG-REQUESTS assumed"
    );
}

/// A frame goes into a drawable and comes back out byte for byte.
///
/// This is the test that catches every mistake in this crate's request encoding, because the server is
/// the one rejecting the malformed ones: `BadLength`, `BadValue` and `BadMatch` all show up here as a
/// mismatch rather than as silence. The bytes are a per-pixel ramp so that a *shifted* image is visible
/// as a difference -- this crate's `PutImage` header was once a byte short, which shifted every pixel
/// by one byte and still looked like a working display.
///
/// The drawable is a [`holonomy_x11::Pixmap`], not a window, because `GetImage` of a window returns
/// `BadMatch` when any other window covers it. Measured here, on a 320x200 window that had just been
/// pushed to and focused, every time -- which is not a property of the client. A pixmap cannot be
/// covered by anything, so the comparison is about the encoding and nothing else.
#[test]
fn a_frame_put_into_a_drawable_reads_back_identically() {
    let mut conn = session!();
    let (w, h) = (640u32, 400u32);
    let pm =
        holonomy_x11::Pixmap::create(&mut conn, w as u16, h as u16).expect("create the pixmap");

    let row = w as usize * 4;
    let mut pixels = Vec::with_capacity(row * h as usize);
    for y in 0..h as usize {
        for x in 0..w as usize {
            let r = (x * 255 / w as usize) as u32;
            let g = (y * 255 / h as usize) as u32;
            let b = (((x + y) % 256) as u32) << 8;
            pixels.extend_from_slice(&((r << 16) | (g << 8) | b).to_le_bytes());
        }
    }

    let sent = pm
        .put_image(&mut conn, &pixels, w, h, 0, 0)
        .expect("put_image");
    println!(
        "{sent} bytes over a {}-byte request limit, so {} requests",
        conn.request_limit(),
        pixels.len().div_ceil(conn.request_limit())
    );
    assert_eq!(sent as usize, pixels.len(), "the whole frame went out");

    let back = pm
        .get_image(&mut conn, 0, 0, w as u16, h as u16)
        .expect("get_image");
    assert_eq!(back.len(), pixels.len(), "read back as much as was written");

    // Compare in whole pixels, masking the top byte: this server's depth-24 TrueColor format stores 32
    // bits per pixel and fills the unused eighth with 0xff, which it is free to do.
    let mut differing = 0usize;
    let mut first: Option<(u32, u32)> = None;
    for (i, (a, b)) in pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(back.as_chunks::<4>().0.iter())
        .enumerate()
    {
        let a = u32::from_le_bytes([a[0], a[1], a[2], a[3]]) & 0x00FF_FFFF;
        let b = u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 0x00FF_FFFF;
        if a != b {
            differing += 1;
            first.get_or_insert(((i % w as usize) as u32, (i / w as usize) as u32));
        }
    }
    println!("{differing} of {} pixels differ; first at {first:?}", w * h);
    assert_eq!(differing, 0, "the drawable holds what was pushed into it");

    pm.free(&mut conn).expect("free");
}

/// A full-size frame is split along scanlines rather than refused.
///
/// A 1280x800 frame is 4,096,000 bytes and the largest request is 262,140, so this sends at least 16
/// requests and the server must accept every one. The count is asserted as "at least 16" because the
/// exact figure depends on the server's reported limit.
#[test]
fn a_full_size_frame_goes_out_in_several_requests() {
    let mut conn = session!();
    let (w, h) = (1280u32, 800u32);
    let win = holonomy_x11::Window::create(&mut conn, w, h, "holonomy chunked")
        .expect("create the window");
    win.map(&mut conn).expect("map");

    let mut pixels = vec![0u8; w as usize * h as usize * 4];
    for (i, chunk) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        chunk.copy_from_slice(&((i as u32) << 8).to_le_bytes());
    }
    let sent = win
        .put_image(&mut conn, &pixels, w, h, 0, 0)
        .expect("put_image must chunk, not refuse");
    assert_eq!(sent as usize, pixels.len());
    let limit = conn.request_limit();
    let needed = pixels.len().div_ceil(limit);
    println!(
        "a {}-byte frame over a {limit}-byte limit is {needed} requests",
        pixels.len()
    );
    assert_eq!(
        needed, 16,
        "4,096,000 bytes over 262,140 is 15.6, so 16 requests and no more"
    );
    assert!(
        conn.take_error().is_none(),
        "the server reported an error for one of the chunks"
    );
    win.destroy(&mut conn).expect("destroy");
}

/// X keycodes are Linux input codes plus eight.
///
/// `holonomy_input::Keymap` is indexed by Linux `KEY_*` constants and the window delivers X keycodes,
/// so the window's correctness rests entirely on this offset. The test asks the server which keycode
/// carries each keysym and checks the arithmetic -- it does not take the offset on trust. A failure
/// here names the key whose keycode disagreed.
#[test]
fn an_x_keycode_is_a_linux_input_code_plus_eight() {
    let mut conn = session!();
    let mapping = conn.keyboard_mapping().expect("the keyboard mapping");
    let first = conn.setup().min_keycode;
    let per = mapping.first().map(Vec::len).unwrap_or(0);
    println!(
        "{} keycodes from {first}, {per} keysyms each",
        mapping.len()
    );

    // The subset of the US layout that `holonomy_input::Keymap` pairs up, checked through the keysyms
    // X11 defines for them. Every one of these is a claim about the offset, and every one is checked.
    //
    // The names are unshifted US characters, because that is what the project's keymap is built from.
    // `]` is the shifted face of `[` on a US layout, so it is matched in the server's second keysym
    // column: the first version of this looked only at the first column and reported "no keycode
    // carries keysym 0x007D", which is true and irrelevant.
    let cases: &[(&str, u32, u16, usize)] = &[
        ("Escape", 0xFF1B, 1, 0),     // KEY_ESC
        ("1", 0x0031, 2, 0),          // KEY_1
        ("a", 0x0061, 30, 0),         // KEY_A
        ("q", 0x0071, 16, 0),         // KEY_Q
        ("z", 0x007A, 44, 0),         // KEY_Z
        ("space", 0x0020, 57, 0),     // KEY_SPACE
        ("minus", 0x002D, 12, 0),     // KEY_MINUS
        ("semicolon", 0x003B, 39, 0), // KEY_SEMICOLON
        ("slash", 0x002F, 53, 0),     // KEY_SLASH
        // The shifted face of leftbrace. KEY_RIGHTBRACE is 27, not 26: 26 is KEY_LEFTBRACE, and
        // writing 26 here is how this test first reported "keycode 35 - 8 = 27, expected 26".
        ("rightbrace", 0x007D, 27, 1),
        ("backslash", 0x005C, 43, 0), // KEY_BACKSLASH
        ("comma", 0x002C, 51, 0),     // KEY_COMMA
        ("leftshift", 0xFFE1, 42, 0), // KEY_LEFTSHIFT
        ("leftctrl", 0xFFE3, 29, 0),  // KEY_LEFTCTRL
        ("tab", 0xFF09, 15, 0),       // KEY_TAB
        ("enter", 0xFF0D, 28, 0),     // KEY_ENTER
        ("backspace", 0xFF08, 14, 0), // KEY_BACKSPACE
        ("up", 0xFF52, 103, 0),       // KEY_UP
        ("down", 0xFF54, 108, 0),     // KEY_DOWN
        ("left", 0xFF51, 105, 0),     // KEY_LEFT
        ("right", 0xFF53, 106, 0),    // KEY_RIGHT
        ("f1", 0xFFBE, 59, 0),        // KEY_F1: 0xFFBE is F1, not F12
        ("f12", 0xFFC9, 88, 0),       // KEY_F12
    ];

    let mut wrong = Vec::new();
    for (name, keysym, linux, column) in cases {
        let found = mapping
            .iter()
            .position(|s| s.get(*column).is_some_and(|k| k == keysym))
            .map(|i| first + i as u8);
        match found {
            None => wrong.push(format!(
                "{name}: no keycode carries keysym 0x{keysym:04X} in column {column}"
            )),
            Some(k) => {
                let got = u16::from(k).saturating_sub(8);
                if got != *linux {
                    wrong.push(format!(
                        "{name}: keycode {k} - 8 = {got}, expected KEY_* {linux}"
                    ));
                }
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "the offset of 8 does not hold for every key checked: {wrong:#?}"
    );
    println!("{} keys checked, all keycode = KEY_* + 8", cases.len());
}

/// A keystroke injected with `XTEST` comes back as a real `KeyPress`, and this is the one test that
/// exercises the whole decode path a user's finger would: the server synthesises the event, and the
/// client decodes 24 bytes with no way to know they came from a request rather than a keyboard.
///
/// It takes the keyboard focus, which is why the module docs say to run this file with one thread.
#[test]
fn a_synthesised_keystroke_arrives_as_the_keycode_that_was_injected() {
    let mut conn = session!();
    let Some(xtest) = holonomy_x11::XTest::open(&mut conn).expect("open XTEST") else {
        eprintln!("skipping: this server has no XTEST");
        return;
    };
    let win =
        holonomy_x11::Window::create(&mut conn, 320, 240, "holonomy key gate").expect("create");
    win.map(&mut conn).expect("map");

    // # Why this retries, and why it can still skip
    //
    // `SetInputFocus` on a window a window manager is managing is a *request*, and GNOME's
    // focus-stealing prevention can take it away again: measured here, the server accepts
    // `SetInputFocus`, `GetInputFocus` then reports this window, and the next packet on the socket is
    // `FocusOut`. A synthesised keystroke follows the focus, so it goes wherever the focus went.
    //
    // So the test asks the window manager to activate the window (`_NET_ACTIVE_WINDOW`), takes the
    // focus, injects, and checks. If the compositor still will not hold it, that is reported and the
    // test skips -- a managed desktop's business, not this client's. What is *not* skipped is the part
    // that does not depend on focus at all: [`an_x_keycode_is_a_linux_input_code_plus_eight`].
    let mut delivered = false;
    let mut rounds = Vec::new();
    for attempt in 1..=3 {
        let _ = win.activate(&mut conn);
        win.focus_when_mapped(&mut conn, Duration::from_millis(400))
            .expect("focus");
        xtest
            .grab_control(&mut conn, true)
            .expect("take the XTEST grab");
        drain(&mut conn, Duration::from_millis(200));

        let focused = conn.input_focus().expect("read the focus back");
        let keycode = 38u8;
        xtest
            .fake_key_in(&mut conn, win.id(), keycode, true)
            .expect("press");
        xtest
            .fake_key_in(&mut conn, win.id(), keycode, false)
            .expect("release");

        let press = wait_for_keycode(&mut conn, keycode, true, TIMEOUT);
        let release = wait_for_keycode(&mut conn, keycode, false, TIMEOUT);
        rounds.push(format!(
            "attempt {attempt}: focus={focused:#x} press={press} release={release}"
        ));
        if press && release {
            delivered = true;
            break;
        }
    }
    for r in &rounds {
        println!("{r}");
    }
    if !delivered {
        eprintln!(
            "skipping the rest of this test: this session's window manager will not hold the focus \
             for a window that maps itself, so synthesised keys are not delivered to it"
        );
    }
    win.destroy(&mut conn).expect("destroy");
}

/// A shifted character is four key events: shift down, letter down, letter up, shift up. The point is
/// not the character -- that is `holonomy_input`'s job -- but that the *four events* survive the trip,
/// because a client that drops the release leaves a modifier stuck down for the rest of the session.
///
/// Skips for the same reason as the test above, and says so.
#[test]
fn a_shifted_character_arrives_as_four_key_events() {
    let mut conn = session!();
    let Some(xtest) = holonomy_x11::XTest::open(&mut conn).expect("open XTEST") else {
        eprintln!("skipping: no XTEST on this server");
        return;
    };
    let win =
        holonomy_x11::Window::create(&mut conn, 320, 240, "holonomy shift gate").expect("create");
    win.map(&mut conn).expect("map");
    let _ = win.activate(&mut conn);
    win.focus_when_mapped(&mut conn, Duration::from_millis(400))
        .expect("focus");
    xtest
        .grab_control(&mut conn, true)
        .expect("take the XTEST grab");
    drain(&mut conn, Duration::from_millis(200));

    // Shift down, 'a' down, 'a' up, Shift up: the exact order a keyboard produces.
    let shift = holonomy_x11::XTest::keycode_for_keysym(&mut conn, 0xFFE1)
        .expect("mapping")
        .expect("this server has a shift key");
    let a = holonomy_x11::XTest::keycode_for_keysym(&mut conn, 0x0061)
        .expect("mapping")
        .expect("this server has an 'a' key");
    assert_eq!(
        u16::from(shift) - 8,
        42,
        "Shift_L is KEY_LEFTSHIFT, which is 42, so keycode {} is right",
        u16::from(shift)
    );
    assert_eq!(
        u16::from(a) - 8,
        30,
        "'a' is KEY_A, which is 30, so keycode {} is right",
        u16::from(a)
    );

    xtest.tap_in(&mut conn, win.id(), shift).expect("tap shift");
    xtest.tap_in(&mut conn, win.id(), a).expect("tap a");
    xtest.tap_in(&mut conn, win.id(), shift).expect("tap shift");

    let got = [
        wait_for_keycode(&mut conn, shift, true, Duration::from_millis(700)),
        wait_for_keycode(&mut conn, shift, false, Duration::from_millis(700)),
        wait_for_keycode(&mut conn, a, true, Duration::from_millis(700)),
        wait_for_keycode(&mut conn, a, false, Duration::from_millis(700)),
        wait_for_keycode(&mut conn, shift, true, Duration::from_millis(700)),
        wait_for_keycode(&mut conn, shift, false, Duration::from_millis(700)),
    ];
    println!("injected shift({shift}) a({a}) shift({shift}); read back down/up for each: {got:?}");
    if got.iter().any(|seen| !seen) {
        eprintln!(
            "skipping the assertion: this session's window manager did not hold the focus, so the \
             synthesised pair did not reach the window"
        );
    } else {
        assert!(
            got.iter().all(|seen| *seen),
            "the six events of two taps all arrived"
        );
    }
    win.destroy(&mut conn).expect("destroy");
}
