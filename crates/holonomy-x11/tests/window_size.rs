//! The window is resizable, and the gate is two instruments reading the same fact.
//!
//! # The defect, measured
//!
//! `WM_NORMAL_HINTS` had `PSize` set in its flags and `width`/`height` left at **zero**. `PSize` is
//! not "this client has a size" — it is "**the `width` and `height` fields below the flags are
//! meaningful**". A window manager reading `PSize` with a zero size is being told *this window wants
//! to be 0 x 0* and clamps it to `min_width x min_height`.
//!
//! On this machine the app asked for **1600 x 1000**, printed that it had, and `GetGeometry` on the
//! same window answered:
//!
//! ```text
//! [ 8] ed 04 00 00  32 00 7c 00    root = 1261, x = 50, y = 124
//! [16] 00 03 40 01                 width = 768,  height = 320
//! ```
//!
//! **768 x 320 is `MIN_WIDTH x MIN_HEIGHT`** — the clamp floor, not a size anything asked for.
//!
//! # Why the gate is a round trip and not a struct check
//!
//! Part 34 fixed two real bugs in `set_size_hints` — the fields were written twelve bytes early, and
//! the request was two bytes too long so the server answered `BadLength` — **and it checked that a
//! `GetProperty` answered at all, not what it said.** So the property existed, had the right length,
//! had a correct minimum, and was wrong in the one field that decided the window's size.
//!
//! **A test that inspects the bytes this crate is about to send is checking the writer with the
//! writer.** These two read the *server's* answer: one path reads what the client asked for
//! (`GetProperty`), the other asks the server what it did (`GetGeometry`). They share no code, and
//! they can only agree if both are right.
//!
//! # Why the last test needs a display, and says so
//!
//! Everything before it runs anywhere. **This one is `#[ignore]`d, and it prints why when it is
//! selected** rather than passing quietly — the rule in this project is that a gate which cannot run
//! must say so out loud.

use holonomy_x11::{Conn, Window};

/// **Every `PSize` flag this crate sets names a real size.** 1 test.
///
/// A pure check, so it runs everywhere: if `PSize` is set, the width and height written beside it
/// must be non-zero. **`PSize` with a zero size is the defect** — a window manager is entitled to
/// honour it, and Mutter does.
#[test]
fn a_psize_flag_never_names_a_zero_size() {
    use holonomy_x11::window::{size_hints_bytes_for, SizeHints};

    for (size, min, max) in [
        ((1600u16, 1000u16), (768u16, 320u16), None),
        ((768, 320), (768, 320), None),
        ((1, 1), (1, 1), None),
        ((336, 200), (336, 200), None),
        ((3072, 1920), (768, 320), Some((4096u16, 4096u16))),
    ] {
        let bytes = size_hints_bytes_for(size, min, max);
        let h = SizeHints::from_bytes(&bytes);
        assert_eq!(h.flags & 1, 1, "PSize is set for {size:?}");
        assert!(
            h.width > 0 && h.height > 0,
            "{size:?}: **PSize is set but names {}x{} — a window manager will clamp this to the \\
             minimum**",
            h.width,
            h.height
        );
        // **The minimum can never exceed the size.** `opening_size` clamps a window into a screen
        // that may be smaller than `MIN_WIDTH`, and a `PMinSize` above the window's own geometry asks
        // the window manager to grow it back past the edge of the display — the layout clamp again,
        // one layer up. This assertion is what found that interaction.
        assert!(
            h.width >= h.min_width && h.height >= h.min_height,
            "{size:?}: the minimum {}x{} is above the size {}x{} asked for",
            h.min_width,
            h.min_height,
            h.width,
            h.height
        );
        assert!(h.width <= u32::from(u16::MAX) as i32, "and fits the field it is written into");
    }
}

/// **The size hints offsets are the protocol's.** 1 test.
///
/// Checked against the protocol's own table rather than against this crate's comment — **which is
/// where this defect lived.** The comment said `4 int x, y` and `8 int width, height`, which is three
/// ints where the protocol has four, and every offset below `height` was right anyway, so only the
/// fields nobody wrote were wrong.
#[test]
fn the_size_hints_offsets_are_the_protocols() {
    use holonomy_x11::window::{size_hints_bytes_for, SizeHints};

    let bytes = size_hints_bytes_for((1600, 1000), (768, 320), Some((4096, 4096)));
    assert_eq!(bytes.len(), 72, "XSizeHints is 18 longs");

    let word = |i: usize| i32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
    assert_eq!(word(0) as u32, 1 | (1 << 4) | (1 << 5), "flags: PSize|PMinSize|PMaxSize");
    assert_eq!(word(1), 0, "x, obsolete");
    assert_eq!(word(2), 0, "y, obsolete");
    assert_eq!(word(3), 1600, "**width is at word 3 (byte 12)**, the first of the four the table called obsolete");
    assert_eq!(word(4), 1000, "height is at word 4 (byte 16)");
    assert_eq!(word(5), 768, "min_width at word 5 (byte 20)");
    assert_eq!(word(6), 320, "min_height");
    assert_eq!(word(7), 4096, "max_width");
    assert_eq!(word(8), 4096, "max_height");
    assert_eq!(word(9), 1, "width_inc: one pixel, no step");
    assert_eq!(word(10), 1, "height_inc");

    let h = SizeHints::from_bytes(&bytes);
    assert_eq!((h.width, h.height), (1600, 1000), "and the reader agrees with the writer");
}

/// **A flag is set only when the field it names is written.** 1 test.
///
/// # The rule, and the defect it generalises
///
/// `PSize` names `width`/`height`, `PMinSize` names `min_width`/`min_height`, `PMaxSize` names
/// `max_width`/`max_height`. **A flag does not mean "this client has such a field" — it means "the
/// words at that offset are meaningful."** So a flag with nothing behind it is not a harmless
/// redundancy: it is a declaration that the value is zero, and a window manager will believe it.
///
/// Both halves of this were wrong in the same function, one row apart:
///
/// ```text
/// PSize   set, width/height never written  -> the window wants to be 0 x 0
/// PMaxSize set, max written as zero       -> the window may never exceed 0 x 0
/// ```
///
/// The second is why the window was **768 x 320** and would not grow on a drag. Measured on this
/// machine, four windows created at 800 x 600 differing only in what was published:
///
/// ```text
/// no hints at all          -> GetGeometry 800x600
/// PSize only               -> GetGeometry 800x600
/// PSize + min 768x320      -> GetGeometry 768x320   <-- PMaxSize with zeroes behind it
/// PSize + min + max 4096   -> GetGeometry 800x600
/// ```
#[test]
fn a_flag_is_set_only_when_the_field_it_names_is_written() {
    use holonomy_x11::window::{size_hints_bytes_for, SizeHints};

    for (size, min, max) in [
        ((1600u16, 1000u16), (768u16, 320u16), None),
        ((1600, 1000), (768, 320), Some((4096u16, 4096u16))),
        ((800, 600), (800, 600), None),
        ((1, 1), (1, 1), None),
    ] {
        let h = SizeHints::from_bytes(&size_hints_bytes_for(size, min, max));

        for (flag, bit, width, height, what) in [
            (h.flags & 1, 1u32, h.width, h.height, "PSize"),
            (h.flags & (1 << 4), 1 << 4, h.min_width, h.min_height, "PMinSize"),
            (h.flags & (1 << 5), 1 << 5, h.max_width, h.max_height, "PMaxSize"),
        ] {
            if flag == 0 {
                continue;
            }
            assert!(
                width > 0 && height > 0,
                "{size:?} max={max:?}: **{what} is set and names {width}x{height}.** A window \\
                 manager reads that as a real bound and sizes the window to it."
            );
            let _ = bit;
        }

        // **And the converse: a maximum that exists must be announced, or it is silently ignored.**
        if let Some((w, hh)) = max {
            assert_eq!(
                h.flags & (1 << 5),
                1 << 5,
                "{size:?}: a maximum of {w}x{hh} was given, so PMaxSize must be set or the window \\
                 manager never learns about it"
            );
            assert_eq!((h.max_width, h.max_height), (w as i32, hh as i32));
        } else {
            assert_eq!(
                h.flags & (1 << 5),
                0,
                "**no maximum was given, so PMaxSize must not be set** -- setting it with zeroes \\
                 behind it is what made the window 768x320 and un-draggable"
            );
        }
    }
}

/// **A window is created, resized and read back, on a real display.** 1 test, ignored by default.
///
/// The round trip the two tests above cannot make: a window created at 800 x 600 must **answer
/// `GetGeometry` as 800 x 600**, not as the minimum, because that is the failure this whole file is
/// about. **`ConfigureWindow` is the same request a window manager sends when a user drags an edge**,
/// so the second half is the resize path end to end.
#[test]
#[ignore = "needs an X display; run with --ignored"]
fn a_window_opens_at_its_size_and_resizes_to_another() {
    use std::time::Duration;

    let mut conn = match Conn::connect(None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("SKIPPED: no X display ({e:?}) — the hints-offset and PSize tests above still ran");
            return;
        }
    };
    let w = Window::create(&mut conn, 800, 600, "gate").expect("create");
    w.set_size_hints(&mut conn, (800, 600), (768, 320), None)
        .expect("set hints");
    w.map(&mut conn).expect("map");

    // The hints the **server** recorded, not the ones this crate meant to send.
    let hints = w.size_hints(&mut conn).expect("the server recorded WM_NORMAL_HINTS");
    assert_eq!(hints.flags & 1, 1, "PSize");
    assert_eq!(
        (hints.width, hints.height),
        (800, 600),
        "**PSize names the size the window was created at**"
    );

    // Let the window manager place it before asking it what it did.
    let deadline = std::time::Instant::now() + Duration::from_millis(600);
    while std::time::Instant::now() < deadline {
        let _ = conn.next_event(Some(Duration::from_millis(50)));
    }

    let (gw, gh, _) = w.geometry(&mut conn).expect("GetGeometry");
    assert_eq!(
        (gw, gh),
        (800, 600),
        "**the server says the window is {gw}x{gh}, and the client asked for 800x600.** A window \\
         smaller than this is the `PSize`-is-zero defect: the window manager clamped it to the \\
         minimum, which is 768x320."
    );

    // Now the resize: the request a window manager sends when an edge is dragged.
    w.configure_size(&mut conn, 1100, 820).expect("resize");
    let deadline = std::time::Instant::now() + Duration::from_millis(600);
    let mut notified = None;
    while std::time::Instant::now() < deadline {
        match conn.next_event(Some(Duration::from_millis(100))) {
            Ok(Some(holonomy_x11::Event::ConfigureNotify { width, height })) => {
                notified = Some((width, height));
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let (gw, gh, _) = w.geometry(&mut conn).expect("GetGeometry after resize");
    assert_eq!(
        (gw, gh),
        (1100, 820),
        "**a resize moved the window to {gw}x{gh}** — the same request a window manager sends when a \\
         user drags an edge"
    );
    assert_eq!(
        notified,
        Some((1100, 820)),
        "and ConfigureNotify said the same thing, so the session is told to resize too"
    );
}
