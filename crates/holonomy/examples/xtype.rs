// Gated on the `desktop` feature, because this types into the focused window through XTEST and that feature is off by
// default. `cargo test --workspace` builds every example, and an example that
// needs a window cannot be built without one.
// Type into whatever window has the focus, through the server.
///
/// This is the end-to-end check for the developer window: a running `holonomy --window` holds the
// keyboard focus, so these keystrokes go through the whole chain -- X11 event, `x11key`, the session's
/// modifier fold, the keymap, the editor, the damage, and a damage-limited `PutImage` back to the
/// window. Nothing here is scripted into the app; the app is only being used as a keyboard target.
///
/// ```text
///   cargo run -p holonomy --features desktop --example xtype -- "Hello, Holonomy" ctrl-q
/// ```
///
/// It needs the `desktop` feature, which is off by default, so it lives in a module behind a `cfg`: a
/// file-level `#![cfg]` would take `main` with it and `cargo build --examples` would fail on a file it
/// can see and cannot use.
#[cfg(feature = "desktop")]
mod window_typing {
    // Type into whatever window has the focus, through the server.
    //
    // This is the end-to-end check for the developer window: the running `holonomy --window` holds the
    // keyboard focus, so these keystrokes go through the whole chain -- X11 event, `x11key`, the session's
    // modifier fold, the keymap, the editor, the damage, and a damage-limited `PutImage` back to the
    // window. Nothing here is scripted into the app; the app is only being used as a keyboard target.
    //
    //   cargo run -p holonomy --features desktop --example xtype -- "Hello, Holonomy" ctrl-q

    /// One character as the X keycode, and whether it needs shift held.
    ///
    /// The letter keycodes are `38 + (c - 'a')`, because X keycode 38 is `a` and they run from there --
    /// which is the same offset the rest of this workspace uses, `KEY_A` is 30 and 30 + 8 is 38. The first
    /// version of this table said `9 + (c - 'a')`, which is the *escape* keycode plus the offset, so it typed
    /// `q` when asked for `a` and the window looked like it was typing the wrong letters.
    fn keycode_for(c: char) -> Option<(u8, bool)> {
        Some(match c {
            'a'..='z' => (38 + (c as u8 - b'a'), false),
            'A'..='Z' => (38 + (c.to_ascii_lowercase() as u8 - b'a'), true),
            '0'..='9' => (10 + (c as u8 - b'0'), false),
            ' ' => (65, false), // KEY_SPACE 57 + 8
            ',' => (51, false), // KEY_COMMA 43 + 8
            '.' => (59, false), // KEY_DOT 51 + 8
            '\'' => (48, true), // KEY_APOSTROPHE 40 + 8
            '!' => (10, true),
            '-' => (20, false), // KEY_MINUS 12 + 8
            _ => return None,
        })
    }

    pub fn run() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let (text, quit) = match args.split_last() {
            Some((last, rest)) if last == "ctrl-q" => (rest.join(" "), true),
            _ => (args.join(" "), false),
        };

        let Ok(mut conn) = holonomy_x11::Conn::connect(None) else {
            eprintln!("no display");
            std::process::exit(1);
        };
        let Some(xtest) = holonomy_x11::XTest::open(&mut conn).expect("XTEST") else {
            eprintln!("no XTEST");
            std::process::exit(1);
        };
        println!("focus is window {:#x}", conn.input_focus().unwrap_or(0));
        xtest.grab_control(&mut conn, true).expect("grab");

        let shift = holonomy_x11::XTest::keycode_for_keysym(&mut conn, 0xFFE1)
            .expect("mapping")
            .expect("a shift key");
        let ctrl = holonomy_x11::XTest::keycode_for_keysym(&mut conn, 0xFFE3)
            .expect("mapping")
            .expect("a control key");

        let mut typed = 0usize;
        // Tokens first, then characters.
        //
        // A token is a key that is not a character, and the harness needs three of them for the table
        // run: `ctrl-t` to insert a table, `tab` and `shift-tab` to move between cells. Typing the
        // literal string "ctrl-t" would have sent `c`, `t`, `r`, `o`, `l`, `-`, `t` -- which is how the
        // first attempt at this harness put a table of nonsense in a document.
        //
        // Scanned left to right and consumed greedily, so "Name" is text and "tab" is a token, but a
        // literal word "tab" in the middle of some other text cannot be typed. That is a real
        // limitation and it is a harness, not a product.
        let mut rest = text.as_str();
        while !rest.is_empty() {
            rest = rest.trim_start();
            if rest.is_empty() {
                break;
            }
            let (is_token, width, with_shift, with_ctrl) = if rest.starts_with("shift-tab") {
                (true, 9usize, true, false)
            } else if rest.starts_with("ctrl-t") {
                (true, 6, false, true)
            } else if rest.starts_with("tab") {
                (true, 3, false, false)
            } else {
                (false, 0, false, false)
            };
            if is_token {
                let tab = holonomy_x11::XTest::keycode_for_keysym(&mut conn, 0xFF09)
                    .expect("mapping")
                    .expect("a tab key");
                // Lowercase `t`, not `T`: `keycode_for_keysym` matches the *first* keysym at a
                // keycode, and that is the unshifted one. Asking for `T` finds nothing on a US layout,
                // which is how this first failed with "a T key" as the panic message.
                let t = holonomy_x11::XTest::keycode_for_keysym(&mut conn, b't' as u32)
                    .expect("mapping")
                    .expect("a t key");
                // The modifier is **held across** the key: press, key down, key up, release.
                //
                // `tap_in` is press-then-release, and using it for the modifier sends
                // `ctrl down, ctrl up, t down` -- the app sees a plain `t` and types one. The windowed
                // trace is what showed it:
                //
                //   dispatch InputEvent { code: 29, value: 1 }   ctrl down
                //   dispatch InputEvent { code: 29, value: 0 }   ctrl up
                //   dispatch InputEvent { code: 20, value: 1 }   t down, with no modifier held
                //
                // which is why Ctrl+T silently did nothing for a whole run while every scripted gate
                // passed. This is the same reason `ctrl-q` below uses `fake_key_in`.
                if with_ctrl {
                    xtest
                        .fake_key_in(&mut conn, 0, ctrl, true)
                        .expect("ctrl down");
                }
                if with_shift {
                    xtest
                        .fake_key_in(&mut conn, 0, shift, true)
                        .expect("shift down");
                }
                if rest.starts_with("ctrl-t") {
                    xtest.fake_key_in(&mut conn, 0, t, true).expect("T down");
                    xtest.fake_key_in(&mut conn, 0, t, false).expect("T up");
                } else {
                    xtest
                        .fake_key_in(&mut conn, 0, tab, true)
                        .expect("tab down");
                    xtest.fake_key_in(&mut conn, 0, tab, false).expect("tab up");
                }
                if with_shift {
                    xtest
                        .fake_key_in(&mut conn, 0, shift, false)
                        .expect("shift up");
                }
                if with_ctrl {
                    xtest
                        .fake_key_in(&mut conn, 0, ctrl, false)
                        .expect("ctrl up");
                }
                typed += 1;
                rest = &rest[width..];
                std::thread::sleep(std::time::Duration::from_millis(12));
                continue;
            }
            let c = rest.chars().next().expect("a character");
            let Some((code, needs_shift)) = keycode_for(c) else {
                eprintln!("skipping {c:?}: no keycode in this example's table");
                rest = &rest[c.len_utf8()..];
                continue;
            };
            if needs_shift {
                xtest.tap_in(&mut conn, 0, shift).expect("shift");
            }
            xtest.tap_in(&mut conn, 0, code).expect("the character");
            if needs_shift {
                xtest.tap_in(&mut conn, 0, shift).expect("shift");
            }
            typed += 1;
            rest = &rest[c.len_utf8()..];
            std::thread::sleep(std::time::Duration::from_millis(12));
        }
        println!("typed {typed} characters into the focused window");

        if quit {
            let q = holonomy_x11::XTest::keycode_for_keysym(&mut conn, 0x0071)
                .expect("mapping")
                .expect("a 'q' key");
            // Hold the modifier across the key. Tapping ctrl and *then* q -- which is what this example did
            // first -- leaves ctrl up by the time q arrives, so the session types a `q` instead of quitting,
            // and the window looks like it ignored the shortcut.
            xtest
                .fake_key_in(&mut conn, 0, ctrl, true)
                .expect("ctrl down");
            std::thread::sleep(std::time::Duration::from_millis(20));
            xtest.fake_key_in(&mut conn, 0, q, true).expect("q down");
            xtest.fake_key_in(&mut conn, 0, q, false).expect("q up");
            xtest
                .fake_key_in(&mut conn, 0, ctrl, false)
                .expect("ctrl up");
            println!("sent Ctrl+Q, holding the modifier across the key");
        }
        // Give the server a moment to deliver before this process goes away.
        std::thread::sleep(std::time::Duration::from_millis(400));
    }
}

fn main() {
    #[cfg(feature = "desktop")]
    window_typing::run();

    #[cfg(not(feature = "desktop"))]
    eprintln!(
        "xtype needs the developer window. Run it with:\n  \
         cargo run -p holonomy --features desktop --example xtype -- \"some text\" ctrl-q"
    );
}
