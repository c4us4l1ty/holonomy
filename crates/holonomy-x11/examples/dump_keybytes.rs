// Raw socket bytes for a sequence of taps, with none of this crate's framing in the way.
//
// The client's reader desynchronises after a synthesised keystroke. Everything except this has been a
// guess about which layer is wrong.
//
//   cargo run -p holonomy-x11 --example dump_keybytes

use holonomy_x11::{Conn, Window, XTest};

fn main() {
    let Ok(mut conn) = Conn::connect(None) else {
        eprintln!("no display");
        return;
    };
    let Some(xtest) = XTest::open(&mut conn).expect("XTest::open") else {
        eprintln!("no XTEST");
        return;
    };
    let win = Window::create(&mut conn, 320, 240, "dump_keybytes").expect("create");
    win.map(&mut conn).expect("map");
    let _ = win.activate(&mut conn);
    win.focus(&mut conn).expect("focus");
    let _ = xtest.grab_control(&mut conn, true);
    // Settle through the client's own reader, then read the socket raw from here on.
    while conn
        .next_event(Some(std::time::Duration::from_millis(300)))
        .unwrap()
        .is_some()
    {}

    let shift = holonomy_x11::XTest::keycode_for_keysym(&mut conn, 0xFFE1)
        .expect("mapping")
        .expect("shift");
    xtest.tap_in(&mut conn, win.id(), shift).expect("tap shift");

    let fd = conn.as_raw_fd();
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let mut buf = vec![0u8; 4096];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut all: Vec<u8> = Vec::new();
    while std::time::Instant::now() < deadline {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let ms = left.as_millis().min(i32::MAX as u128) as i32;
        if unsafe { libc::poll(&mut pfd, 1, ms.max(1)) } <= 0 {
            break;
        }
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
        all.extend_from_slice(&buf[..n as usize]);
    }
    println!("{} raw bytes after one shift tap", all.len());
    for (i, chunk) in all.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        println!("{:04x}  {}", i * 16, hex.join(" "));
    }
    // Walk it under the core-event sizes.
    let names: &[(u8, &str)] = &[
        (2, "KeyPress"),
        (3, "KeyRelease"),
        (4, "ButtonPress"),
        (5, "ButtonRelease"),
        (9, "FocusIn"),
        (10, "FocusOut"),
        (12, "Expose"),
        (16, "CreateNotify"),
        (19, "MapNotify"),
        (22, "ConfigureNotify"),
        (28, "PropertyNotify"),
        (33, "ClientMessage"),
        (34, "MappingNotify"),
    ];
    let size_of = |c: u8| match c {
        2..=5 => 24,
        6 => 28,
        _ => 32,
    };
    let mut at = 0usize;
    while at < all.len() {
        let raw = all[at];
        let code = raw & 0x7F;
        let size = size_of(raw);
        let name = names
            .iter()
            .find(|(c, _)| *c == code)
            .map(|(_, n)| *n)
            .unwrap_or("(extension or unknown)");
        let extra = if code == 2 || code == 3 {
            format!("keycode {}", all[at + 1])
        } else {
            String::new()
        };
        println!(
            "  offset {at:3}: raw {raw:#04x} -> {name} {extra}, read as {size} bytes{}",
            if raw & 0x80 != 0 { " (SendEvent)" } else { "" }
        );
        at += size;
    }
}
