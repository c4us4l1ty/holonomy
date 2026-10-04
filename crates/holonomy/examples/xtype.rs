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
        for c in text.chars() {
            let Some((code, needs_shift)) = keycode_for(c) else {
                eprintln!("skipping {c:?}: no keycode in this example's table");
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
