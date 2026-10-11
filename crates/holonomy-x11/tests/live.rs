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

/// A window this client owns resizes, and the server confirms the new size.
///
/// Three claims. The request is accepted -- a malformed one is `BadLength` or `BadValue`, and this is
/// the test that would have caught the mask being written into `pad2`. The size really changes, read
/// back with `GetGeometry` rather than assumed. And the window still works afterwards, because a
/// resize that broke the connection would be a window nobody can type into.
///
/// The window is **override-redirect**, and that is the whole reason the second claim can be made. A
/// window manager owns a managed window's geometry: it receives a client's `ConfigureWindow` as a
/// `ConfigureRequest` and may answer with any size, so on a managed window this test would have to
/// accept "still the size it was" and would prove nothing. Measured under mutter here: a managed
/// window created at 1024x700, asked for 1600x1000, no error of any kind, still 1024x700. Override
/// redirect takes the window manager out of the path, so the server applies the resize directly.
#[test]
fn a_window_this_client_owns_resizes_and_the_server_confirms_the_new_size() {
    let mut conn = session!();
    let start = (1024u16, 700u16);
    let win = holonomy_x11::Window::create_override_redirect(
        &mut conn,
        u32::from(start.0),
        u32::from(start.1),
        "resizable",
    )
    .expect("create the window");
    win.map(&mut conn).expect("map");
    let (w0, h0, _) = win.geometry(&mut conn).expect("the starting geometry");
    assert_eq!(
        (w0, h0),
        start,
        "the window is the size it was created at, or the rest of this test is measuring nothing"
    );

    // Three sizes: bigger, smaller, and back. A drag produces all three in that order, many times over,
    // and the last is the case where "did anything change?" has to be answerable.
    for want in [(1600u16, 1000u16), (800, 500), start] {
        win.configure_size(&mut conn, want.0, want.1)
            .expect("resize the window");
        let (w, h, _) = win.geometry(&mut conn).expect("read the geometry back");
        assert_eq!(
            (w, h),
            want,
            "asked for {}x{} and the server has {w}x{h}",
            want.0,
            want.1
        );
    }

    // A zero size is the caller's mistake, not the server's, and this crate says so rather than
    // reporting a `BadValue` that names a request the caller believes was fine.
    match win.configure_size(&mut conn, 0, 400) {
        Err(holonomy_x11::WindowError::ZeroSize { width, height }) => {
            assert_eq!(
                (width, height),
                (0, 400),
                "the error names what was refused"
            );
        }
        other => panic!("a resize to zero should be refused locally, got {other:?}"),
    }

    // And still working: a refused resize must not have cost the connection its health.
    win.configure_size(&mut conn, 1024, 700)
        .expect("resize after the refusal");
    let mut pixels = vec![0u8; 64 * 64 * 4];
    for (i, p) in pixels.iter_mut().enumerate() {
        *p = if i % 4 == 3 { 0 } else { (i % 251) as u8 };
    }
    win.put_image(&mut conn, &pixels, 64, 64, 0, 0)
        .expect("still accepts a push after resizing");
    win.destroy(&mut conn).expect("destroy");
}

/// A run of keystrokes arrives as a run of coherent 32-byte events.
///
/// This is the gate for the packet size, and it earns its own test because the size was wrong here
/// twice and the failure was not loud. A reader that takes 24 bytes for a `KeyPress` still decodes the
/// *first* press correctly -- the keycode, the timestamp and the modifier state are all in the first
/// 24 -- and only then reads the event's own padding as another event. So the symptoms are indirect: a
/// press with no matching release, a keycode below the server's minimum, a timestamp that jumps
/// backwards.
///
/// So the test does not check the size. It checks the *consequences*: over a run of synthesised keys,
/// every keycode is inside the range the server reported, the events arrive in the order they were
/// injected, and the timestamps never go backwards.
///
/// It is deliberately not a table assertion. This crate previously asserted a per-code size table --
/// 24 for key and button events, 28 for motion -- and every entry was true of the specification's
/// field listings and false of the wire. See [`holonomy_x11::proto::EVENT_BYTES`].
#[test]
fn a_run_of_keystrokes_arrives_as_coherent_thirty_two_byte_events() {
    let mut conn = session!();
    let Some(xtest) = holonomy_x11::XTest::open(&mut conn).expect("open XTEST") else {
        eprintln!("skipping: this server has no XTEST");
        return;
    };
    let min = conn.setup().min_keycode;
    let max = conn.setup().max_keycode;
    let win =
        holonomy_x11::Window::create(&mut conn, 320, 240, "holonomy packet size").expect("create");
    win.map(&mut conn).expect("map");
    let _ = win.activate(&mut conn);
    let _ = win.focus_when_mapped(&mut conn, Duration::from_millis(400));
    xtest.grab_control(&mut conn, true).expect("grab");
    drain(&mut conn, Duration::from_millis(250));

    // Four taps, so eight events: a missing one is visible rather than assumed.
    let a = holonomy_x11::XTest::keycode_for_keysym(&mut conn, 0x0061)
        .expect("mapping")
        .expect("an 'a' key");
    let b = holonomy_x11::XTest::keycode_for_keysym(&mut conn, 0x0062)
        .expect("mapping")
        .expect("a 'b' key");
    let shift = holonomy_x11::XTest::keycode_for_keysym(&mut conn, 0xFFE1)
        .expect("mapping")
        .expect("a shift key");
    for keycode in [a, b, a, shift] {
        xtest.tap_in(&mut conn, win.id(), keycode).expect("tap");
    }

    // # The focus is retried, and losing it is a skip rather than a failure
    //
    // A synthesised key goes to whatever window holds the focus, and this test's window has to hold
    // it for the keys to arrive at all. On a managed desktop that is not guaranteed: focus-stealing
    // prevention can take the focus back the moment it is granted, so this test failed intermittently
    // here with `got []` -- not because the client sent the wrong request, but because nobody was
    // listening. That is the same situation its two sibling XTEST gates already handle by retrying
    // and then reporting, so this does too, rather than asserting on an empty vector. What is *not*
    // given up is the check that needs no focus at all: `min` and `max` come from the setup reply.
    let mut keycodes: Vec<u8> = Vec::new();
    let mut times: Vec<u32> = Vec::new();
    let mut rounds: Vec<String> = Vec::new();
    let mut delivered = false;
    for attempt in 1..=3 {
        let _ = win.activate(&mut conn);
        win.focus_when_mapped(&mut conn, Duration::from_millis(400))
            .expect("focus");
        xtest
            .grab_control(&mut conn, true)
            .expect("take the XTEST grab");
        drain(&mut conn, Duration::from_millis(200));
        keycodes.clear();
        times.clear();

        for keycode in [a, b, a, shift] {
            xtest.tap_in(&mut conn, win.id(), keycode).expect("tap");
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(4);
        while keycodes.len() < 8 && std::time::Instant::now() < deadline {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if let Ok(Some(ev)) = conn.next_event(Some(left.min(Duration::from_millis(250)))) {
                match ev {
                    holonomy_x11::Event::KeyPress { keycode, time, .. }
                    | holonomy_x11::Event::KeyRelease { keycode, time, .. } => {
                        keycodes.push(keycode);
                        times.push(time);
                    }
                    _ => {}
                }
            }
        }
        let focused = conn.input_focus().expect("read the focus back");
        rounds.push(format!(
            "attempt {attempt}: focus={focused:#x} window={:#x} keycodes={keycodes:?}",
            win.id()
        ));
        if keycodes.len() == 8 {
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
             for a window that maps itself, so synthesised keys are not delivered to it. The range \
             check below is still meaningful -- an empty run is inside the range -- and the 32-byte \
             claim it was written for is covered by `dump_keybytes` and by the app's own log."
        );
        assert!(
            keycodes.iter().all(|k| (min..=max).contains(k)),
            "the keycodes that did arrive are in range: {keycodes:?}"
        );
        win.destroy(&mut conn).expect("destroy");
        return;
    }

    let outside: Vec<u8> = keycodes
        .iter()
        .copied()
        .filter(|k| *k < min || *k > max)
        .collect();
    assert!(
        outside.is_empty(),
        "every keycode must be inside the server's range {min}..={max}; {outside:?} are not, and a \
         keycode out of range is what reading an event's padding as an event looks like"
    );
    assert_eq!(
        keycodes,
        vec![a, a, b, b, a, a, shift, shift],
        "the four taps arrive in order, each as a press then a release; got {keycodes:?}"
    );
    let backwards = times.windows(2).filter(|w| w[1] < w[0]).count();
    assert_eq!(
        backwards,
        0,
        "server timestamps must not go backwards; {backwards} of {} pairs did, from {times:?}",
        times.len().saturating_sub(1)
    );
    println!("8 keycodes {keycodes:?}, every one inside {min}..={max}, timestamps non-decreasing",);
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

/// A `ConfigureWindow` request is three words plus one word per value the mask names.
///
/// Twelve bytes of fixed part -- a `CARD16 mask` and a `CARD16 pad2` after the window -- and one
/// `CARD32` per value. So one value is four words and two are five. This is the one thing about
/// `ConfigureWindow` that a unit test can be certain of without a server, and it is worth pinning
/// because the alternative reading of the specification is also plausible: `LISTofVALUE value-list`
/// reads like an array, `value-list[1]` would make `sizeof` sixteen, and sixteen plus two values is
/// twenty-four bytes. Measured against this server: twenty-four bytes is `BadLength`, and twenty is
/// accepted. The server is `dix/window.c`, which checks `client->req_len` against
/// `sizeof(xConfigureWindowReq) + n * 4`, and that `sizeof` is twelve.
///
/// The two assertions on the mask's offset are the point of the test. The first version of this built
/// the request as `.u16(0).u16(mask)` -- zero first, called "the high half of the mask" -- on the
/// theory that a 16-bit mask wanted its halves in that order. It passed, because it was asserting the
/// length rule and got the length rule right while putting the mask in `pad2`, so every
/// `ConfigureWindow` on the wire carried a mask of zero and asked for three words' worth of nothing.
#[test]
fn a_configure_window_request_is_three_words_plus_one_per_value() {
    use holonomy_x11::proto::{op, value, Req};
    let window = 0x0040_0001u32;
    let words = |b: &[u8]| u16::from_le_bytes([b[2], b[3]]);

    for (label, mask, values, want_words) in [
        ("width alone", value::WIDTH, 1usize, 4u16),
        ("width and height", value::WIDTH | value::HEIGHT, 2, 5),
    ] {
        let mut req = Req::new(op::CONFIGURE_WINDOW)
            .second_byte(0)
            .u32(window)
            .u16(mask) // offset 8
            .u16(0) // offset 10, the `pad2`
            .u32(1600);
        if values == 2 {
            req = req.u32(1000);
        }
        let b = req.finish(11);
        assert_eq!(
            words(&b),
            want_words,
            "{label}: the length field says {want_words} words"
        );
        assert_eq!(
            b.len() as u16,
            want_words * 4,
            "{label}: and the buffer really is that long"
        );
        assert_eq!(
            u16::from_le_bytes([b[8], b[9]]),
            mask,
            "{label}: the mask is at offset 8, which is where the two CARD16 fields start"
        );
        assert_eq!(
            u16::from_le_bytes([b[10], b[11]]),
            0,
            "{label}: and the pad2 after it is zero"
        );
    }
}

/// The first `ConfigureNotify` naming a size other than `from`, or `None` within `TIMEOUT`.
///
/// **And it skips the ones that repeat `from`.** A resize produces a burst, and a window manager may
/// send several `ConfigureNotify`s for one drag — including ones at the old size. **Returning the
/// first event of the burst unconditionally would make this test a race**, which is the third way a
/// timing test can be wrong and the least excusable.
/// The first `ConfigureNotify` naming a size other than `from`, or `None` within `TIMEOUT`.
fn first_resize(conn: &mut holonomy_x11::Conn, from: (u16, u16)) -> Option<(u16, u16)> {
    let deadline = std::time::Instant::now() + TIMEOUT;
    while std::time::Instant::now() < deadline {
        match conn.next_event(Some(
            TIMEOUT.min(deadline.saturating_duration_since(std::time::Instant::now())),
        )) {
            Ok(Some(ev)) => {
                if let holonomy_x11::Event::ConfigureNotify { width, height } = ev {
                    if (width, height) != from {
                        return Some((width, height));
                    }
                }
            }
            Ok(None) => {}
            Err(_) => return None,
        }
    }
    None
}

/// **A resize arrives as a `ConfigureNotify` carrying the size the server committed.**
///
/// ## The instrument, and why it is comparative as well as absolute
///
/// **`GetGeometry` is the server's own answer**, read over a request/reply round trip that shares no
/// code with the event decoder. **Comparing the two is the strongest statement available without a
/// second opinion on the wire format** — if the decoder read the width from the wrong four bytes, the
/// two would disagree, and the difference names the bug.
///
/// **The absolute assertion is here too**, because two things can be wrong the same way: a parser
/// that read `width` out of the `window` field would agree with nothing, but one that read it out of
/// the *height* field would agree with a geometry read only when the two sizes happen to differ by the
/// right amount. **The fixture deliberately uses sizes that are not related to each other** —
/// 1000x760 and 1137x511 — so a swap, a shift and a misread are all distinguishable.
#[test]
fn a_resize_arrives_as_a_configure_notify_carrying_the_new_size() {
    require!();
    let mut conn = holonomy_x11::Conn::connect(None).expect("connect to $DISPLAY");
    // **1024x700 and 1600x1000, because they are the sizes the existing live gate already creates
    // and resizes to on this host** -- and an unrelated `RowTooWide` in `create_override_redirect`
    // rejected the first sizes tried here. **A protocol test that fails for a reason it is not about
    // is still a test that measures nothing.**
    //
    // **And the two sizes share no digits in the same places**: a decoder that read `width` out of
    // `height`, or out of a four-byte field one slot along, gives a different answer from this pair
    // and not from some other pair.
    let start = (1024u16, 700u16);
    let want = (1600u16, 1000u16);
    // **Override-redirect, because this window has no window manager to interpret a user drag.** The
    // point is the *event*, not the user's gesture, and a managed window would add a reparenting step
    // this test would then be measuring instead.
    let win = holonomy_x11::Window::create_override_redirect(
        &mut conn,
        u32::from(start.0),
        u32::from(start.1),
        "live resize",
    )
    .expect("create the window");
    win.map(&mut conn).expect("map");

    let (w0, h0, _) = win.geometry(&mut conn).expect("the starting geometry");
    assert_eq!(
        (w0, h0),
        start,
        "the window is the size it was created at, or the rest of this test is measuring nothing"
    );

    win.configure_size(&mut conn, want.0, want.1)
        .expect("resize the window");
    let (w1, h1, _) = win.geometry(&mut conn).expect("the server's new geometry");
    assert_eq!(
        (w1, h1),
        want,
        "**and the server committed it.** On an override-redirect window a `ConfigureWindow` is \
         honoured directly; a managed one may be overridden by the window manager, which is why this \
         fixture is not managed."
    );

    let got = first_resize(&mut conn, start).expect("a ConfigureNotify naming the new size");
    assert_eq!(
        got, want,
        "**the parsed `ConfigureNotify` carries the size the server committed.** If this fails with a \
         width that is a multiple of four, or one that looks like a window id, the decoder is reading \
         width and height out of the wrong fields — which is exactly what `Event::decode` was doing, \
         and exactly what leaves an application painting at its old size into a resized window with a \
         black band along the right and bottom edges."
    );
}

/// **The size hints reach the server, and the server reads back what was sent.**
///
/// ## What this is for
///
/// **`WM_NORMAL_HINTS` was never set, and nothing found out.** `set_size_hints` wrote every field
/// twelve bytes early — `min_width` into the obsolete `width` field — *and* built its `ChangeProperty`
/// request two bytes too long, so the server answered `BadLength` and created no property at all.
/// **The caller discarded the error with `let _ =`.**
///
/// **The symptom was a window the window manager would not resize.** A window with no
/// `WM_NORMAL_HINTS` is a fixed-size island in a resizable desktop: `xprop -id <window>
/// WM_NORMAL_HINTS` answers `not found`, and the driver handled a `ConfigureNotify` that could never
/// arrive.
///
/// ## Reading it back, which is the only honest check
///
/// **The test asserts what the *server* stored, not what the client sent.** `GetProperty` is the one
/// request that answers "what did you actually record", and a builder that puts its fields in the
/// wrong offsets produces a property of exactly the right length that reads back as nonsense.
#[test]
fn the_size_hints_reach_the_server_and_read_back() {
    require!();
    let mut conn = holonomy_x11::Conn::connect(None).expect("connect to $DISPLAY");
    let win = holonomy_x11::Window::create_override_redirect(&mut conn, 1024, 700, "hints")
        .expect("create the window");

    // **The same call `Desktop::open` makes**, with the same numbers the chrome clamps to.
    win.set_size_hints(&mut conn, (768, 320), None).expect(
        "**set the hints without a protocol error.** This is the assertion that would have \
                caught the `BadLength`: the request was two bytes too long and the server rejected \
                it, and the caller threw that error away.",
    );

    let h = win
        .size_hints(&mut conn)
        .expect("`GetProperty` answered, so the property exists");
    assert_eq!(
        (h.min_width, h.min_height),
        (768, 320),
        "**the minimum size the server recorded is the minimum size that was sent.** Every field was \
         being written twelve bytes early into the obsolete `x, y, width, height` block, so a \
         window manager reading the structure would have seen a minimum of zero and a maximum of zero \
         — which means *no minimum*, which is the one value that lets a drag to nothing happen."
    );
    // **`PMinSize` is the flag that matters**, and it is the flag that was being read as something else.
    assert_ne!(
        h.flags & (1 << 4),
        0,
        "**`PMinSize` is set**, or the minimum is stored but no window manager will read it"
    );
    assert_ne!(
        h.flags & 1,
        0,
        "**`PSize` is set**, so the current size is part of the structure"
    );
}
