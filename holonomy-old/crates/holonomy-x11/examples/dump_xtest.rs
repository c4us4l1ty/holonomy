// Every packet the server sends after two XTEST key injections, with no filtering at all.
//
//   cargo run -p holonomy-x11 --example dump_xtest

use holonomy_x11::proto::Event;
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
    println!("XTEST major opcode {}", xtest.major());
    let win = Window::create(&mut conn, 320, 240, "dump_xtest").expect("create");
    win.map(&mut conn).expect("map");
    println!("activate: {:?}", win.activate(&mut conn));
    win.focus(&mut conn).expect("focus");
    println!(
        "focus is now {:#x}, window is {:#x}",
        conn.input_focus().unwrap_or(0),
        win.id()
    );
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Settle.
    while conn
        .next_event(Some(std::time::Duration::from_millis(300)))
        .unwrap()
        .is_some()
    {}

    println!("\ninjecting keycode 38 down, then up:");
    xtest
        .fake_key_in(&mut conn, win.id(), 38, true)
        .expect("press");
    xtest
        .fake_key_in(&mut conn, win.id(), 38, false)
        .expect("release");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let mut seen = 0;
    while let Some(left) = deadline.checked_duration_since(std::time::Instant::now()) {
        match conn.next_event(Some(left.min(std::time::Duration::from_millis(300)))) {
            Ok(Some(ev)) => {
                seen += 1;
                let what = match ev {
                    Event::KeyPress { keycode, state, .. } => {
                        format!("KeyPress keycode {keycode} state {state:#06x}")
                    }
                    Event::KeyRelease { keycode, state, .. } => {
                        format!("KeyRelease keycode {keycode} state {state:#06x}")
                    }
                    Event::Expose { .. } => "Expose".to_string(),
                    Event::ConfigureNotify { width, height } => {
                        format!("ConfigureNotify {width}x{height}")
                    }
                    Event::Other { code } => format!("Other {code}"),
                    other => format!("{other:?}")
                        .split_whitespace()
                        .next()
                        .unwrap_or("?")
                        .to_string(),
                };
                println!("   {what}");
            }
            Ok(None) => {}
            Err(e) => {
                println!("   error: {e}");
                break;
            }
        }
    }
    println!("\n{seen} events in 3s after one press and one release");
}
