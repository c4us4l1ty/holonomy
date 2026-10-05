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
            } else if rest.starts_with("ctrl-m") {
                // `Ctrl+M`, Phase 9B's math chord. Same `with_ctrl` path as `ctrl-t`; the only reason
                // it is a separate arm is that the *key* differs, and the key is looked up below by
                // hard-coding `t`.
                (true, 6, false, true)
            } else if rest.starts_with("tab") {
                (true, 3, false, false)
            } else if rest.starts_with("right") {
                // Phase 9B: leaving a formula is an arrow key, and a newline will not do it. A newline
                // inserted while the caret is inside leaves the closing `$$` at the start of the next
                // line, where it opens a *second*, empty span -- so the run compiles two formulas and
                // the counter reads 2 for one. Two Rights step over the two delimiter bytes, which is
                // what a user does.
                (true, 5, false, false)
            } else if rest.starts_with("left") {
                (true, 4, false, false)
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
                // The chord's letter, as a keysym: `t` for `ctrl-t`, `m` for `ctrl-m`.
                let letter = if rest.starts_with("ctrl-m") {
                    b'm'
                } else {
                    b't'
                };
                let t = holonomy_x11::XTest::keycode_for_keysym(&mut conn, u32::from(letter))
                    .expect("mapping")
                    .expect("a t or m key");
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
                let arrow = if rest.starts_with("right") {
                    Some(0xFF53)
                } else if rest.starts_with("left") {
                    Some(0xFF51)
                } else {
                    None
                };
                if let Some(keysym) = arrow {
                    // Arrow keys are looked up by keysym rather than by a hard-coded code, because a
                    // US layout puts them wherever it likes and the block above's codes are all
                    // main-row keys.
                    let key = holonomy_x11::XTest::keycode_for_keysym(&mut conn, keysym)
                        .expect("mapping")
                        .expect("an arrow key");
                    xtest
                        .fake_key_in(&mut conn, 0, key, true)
                        .expect("arrow down");
                    xtest
                        .fake_key_in(&mut conn, 0, key, false)
                        .expect("arrow up");
                } else if rest.starts_with("ctrl-t") || rest.starts_with("ctrl-m") {
                    xtest
                        .fake_key_in(&mut conn, 0, t, true)
                        .expect("chord letter down");
                    xtest
                        .fake_key_in(&mut conn, 0, t, false)
                        .expect("chord letter up");
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
            // **Ask the server which key produces this character, rather than trusting a table.**
            //
            // `keycode_for` hard-codes `38 + (c - 'a')` for the letters, which is only right if the
            // letters are contiguous -- and they are not. On this host the X keycodes are evdev + 8, and
            // evdev's letter codes are `a = 30, b = 48, c = 46, d = 32, e = 18, f = 33, ...`, so
            // `38 + (c - 'a')` sends `f` as keycode 43, which the server reports as `h`. A live 9B run
            // typed the quadratic formula's fraction and the document came out as `uhvad23` -- seven
            // wrong bytes, and the log's "1 compiled, 0 procedural fills" was *that*.
            //
            // The server already has the answer in its keymap, and `keycode_for_keysym` is what the
            // chord keys above already use. Every character goes through it now. The table survives for
            // the *shift* decision, which a keysym lookup cannot answer.
            // One lookup, and it answers both halves: which key, and whether shift must be held.
            // Two lookups -- the character then its lowercase -- get the keycode but leave the shift
            // flag to guesswork, and `}` was being dropped precisely because of that.
            let found = match holonomy_x11::XTest::keycode_for_keysym_shifted(&mut conn, c as u32) {
                Ok(found) => found,
                Err(e) => {
                    eprintln!("keysym lookup for {c:?} failed: {e:?}");
                    None
                }
            };
            let Some((code, needs_shift)) = found else {
                eprintln!("skipping {c:?}: no keycode for it in this server's keymap");
                rest = &rest[c.len_utf8()..];
                continue;
            };
            // **Shift is held across the key, not tapped.** `tap_in` is press-then-release, so tapping
            // shift sends `shift down, shift up, key down` and the key arrives unshifted -- the same
            // mistake this harness already documents for Ctrl, and it had the same effect: every `{`
            // and `}` vanished, because the app saw an unshifted `[` and `]` on a key it does not map
            // to a command and the harness's own counter cheerfully reported them as typed. A live run
            // recorded `rac12` from `rac{1}{2}`.
            if needs_shift {
                xtest
                    .fake_key_in(&mut conn, 0, shift, true)
                    .expect("shift down");
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
