//! Drive the running window the way a person does, and photograph what it did.
//!
//! # Why this is an example and not a test
//!
//! **Because every defect this file exists to find is a defect of the *application*, not of a
//! function.** "The window does not resize", "the dropdown opens behind the toolbar", "the buttons
//! lag" — none of them is visible from inside the process: the session resizes correctly in a unit
//! test with a headless scanout, and the chrome's layers are a tree no screenshot looks at. **They are
//! visible only from another process, on a real X server, with a real pointer.**
//!
//! So this is a harness, not an assertion: it drives, it dumps `.ppm` frames, and it prints the
//! measurements. `crates/holonomy/tests/window_size.rs` and
//! `crates/holonomy-render/tests/chrome.rs` are where the claims become gates; this is where the claim
//! is first looked at.
//!
//! # Usage
//!
//! ```text
//!   cargo run --release --features desktop -p holonomy --example uidrive -- <out-dir> [script...]
//! ```
//!
//! The script is a list of steps, run in order against the window the app prints on startup:
//!
//! | step | what it does |
//! |---|---|
//! | `shot <name>` | dump the window's own pixels |
//! | `move <x> <y>` | move the pointer to a window-relative point |
//! | `click <x> <y>` | move, press and release there |
//! | `click-tool <name>` | click the toolbar tool `name`, e.g. `style` |
//! | `click-menu <n>` | click menu-bar heading `n` |
//! | `resize <w> <h>` | ask the server to resize the window |
//! | `type <text>` | type ASCII text through XTEST |
//! | `wait <ms>` | sleep |
//!
//! **It needs a display and the `desktop` feature.** Without either it prints why and exits 0, so a
//! machine without an X server does not fail a build.
#![cfg(feature = "desktop")]

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use holonomy_x11::{Conn, Window, XTest};

/// How long a step waits for the app to settle before the frame is taken.
///
/// **Long enough for the caret's own blink tick and short enough to be usable**: 250 ms is two blink
/// quarters, so a step is not racing the animation, and a whole script of twenty steps is five
/// seconds.
const SETTLE: Duration = Duration::from_millis(250);

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(out_dir) = args.next() else {
        eprintln!("usage: uidrive <out-dir> [step...]");
        std::process::exit(2);
    };
    let script: Vec<String> = args.collect();
    std::fs::create_dir_all(&out_dir).expect("create the output directory");

    let (mut app, lines) = match spawn() {
        Some(spawned) => spawned,
        None => {
            eprintln!("uidrive: no display; not driving anything");
            return;
        }
    };

    let window_id = match window_id(&lines) {
        Some(id) => id,
        None => {
            eprintln!("uidrive: the app did not report a window id");
            let _ = app.kill();
            return;
        }
    };
    eprintln!("uidrive: driving window {window_id:#x}");

    // **Relay the app's stderr for the rest of the run, on its own thread.**
    //
    // `window_id` reads lines until it finds the id and then *stops reading*, which is right for
    // finding the id and wrong for everything after it: the app's own diagnostics — the
    // `HOLONOMY_WINDOW_TRACE` event stream, a `SizeMismatch`, a paint refusal — all arrive after the
    // line this harness was written to parse, and were dropped on the floor. **A harness that can only
    // see the first line of the process it is driving cannot diagnose that process**, which is the one
    // thing it is for.
    let relay = std::thread::spawn(move || {
        while let Ok(line) = lines.recv() {
            eprintln!("uidrive: app says: {line}");
        }
    });

    let mut conn = match Conn::connect(None) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("uidrive: cannot open a second connection: {e}");
            let _ = app.kill();
            return;
        }
    };
    let window = Window::attach(&mut conn, window_id, 1600, 1000);
    let xtest = match XTest::open(&mut conn) {
        Ok(Some(x)) => x,
        Ok(None) => {
            eprintln!("uidrive: the server has no XTEST; cannot synthesise a pointer");
            let _ = app.kill();
            return;
        }
        Err(e) => {
            eprintln!("uidrive: XTEST failed: {e}");
            let _ = app.kill();
            return;
        }
    };
    let _ = xtest.grab_control(&mut conn, true);

    for step in &script {
        if let Err(e) = run_step(&mut conn, &xtest, &window, &mut app, step, &out_dir) {
            eprintln!("uidrive: step {step:?} failed: {e}");
        }
    }

    let _ = xtest.grab_control(&mut conn, false);
    let _ = app.kill();
    let _ = app.wait();
    drop(relay);
}

/// Start the app in its window mode, or `None` when there is no display to start it on.
fn spawn() -> Option<(Child, std::sync::mpsc::Receiver<String>)> {
    if std::env::var_os("DISPLAY").is_none() {
        return None;
    }
    let exe = std::env::current_exe()
        .ok()?
        .parent()?
        .parent()?
        .join("holonomy");
    let mut app = Command::new(exe)
        .arg("--window")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    // **A thread reads the app's stderr and sends whole lines.**
    //
    // `read_to_string` on the pipe blocks until the child exits, which is the one moment the window id
    // is of no use; `try_read` is the same problem on a different API. A reader thread with a channel
    // is the shape that fits: it blocks on the pipe for as long as it takes and this side asks for a
    // line with a timeout.
    let (tx, rx) = std::sync::mpsc::channel();
    let pipe = app.stderr.take()?;
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(pipe).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    Some((app, rx))
}

/// The window id, read from the app's own startup line.
///
/// **The app prints it and this reads it**, rather than searching the root's children: the window it
/// finds by search is the one *a* window, and a desktop running this has others.
fn window_id(rx: &std::sync::mpsc::Receiver<String>) -> Option<u32> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(line) = rx.recv_timeout(left.min(Duration::from_millis(250))) else {
            continue;
        };
        eprintln!("uidrive: app says: {line}");
        if let Some(at) = line.find("window 0x") {
            // **Nine, not seven.** `"window 0x"` is nine characters wide, and slicing at seven starts
            // the digits on the literal `0` -- which parses, as `0`, and produces a driver pointed at
            // window zero. The first run of this harness did exactly that and reported
            // `BadDrawable on opcode 55` from the server, which reads like an XTEST problem and is not
            // one.
            let hex = line[at + 9..]
                .split(|c: char| !c.is_ascii_hexdigit())
                .next()?;
            if let Ok(id) = u32::from_str_radix(hex, 16) {
                return Some(id);
            }
        }
    }
    None
}

fn run_step(
    conn: &mut Conn,
    xtest: &XTest,
    window: &Window,
    app: &mut Child,
    step: &str,
    out: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let parts: Vec<&str> = step.split_whitespace().collect();
    let name = parts.first().copied().unwrap_or("");
    match name {
        "wait" => {
            let ms: u64 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(200);
            std::thread::sleep(Duration::from_millis(ms));
        }
        "shot" => {
            let label = parts.get(1).copied().unwrap_or("frame");
            sleep_settle();
            shot(conn, window, out, label)?;
        }
        "move" => {
            let (x, y) = point(&parts)?;
            at(conn, window, xtest, x, y, false)?;
        }
        "click" => {
            let (x, y) = point(&parts)?;
            at(conn, window, xtest, x, y, true)?;
        }
        "type" => {
            let text = parts[1..].join(" ");
            for c in text.chars() {
                let keysym = u32::from(c) as u32;
                let Some((keycode, shift)) = XTest::keycode_for_keysym_shifted(conn, keysym)? else {
                    eprintln!("uidrive: no keycode for {c:?}");
                    continue;
                };
                if shift {
                    let Some(shift) = XTest::keycode_for_keysym(conn, 0xFFE1)? else {
                        continue;
                    };
                    xtest.fake_key(conn, shift, true)?;
                    xtest.fake_key(conn, keycode, true)?;
                    xtest.fake_key(conn, keycode, false)?;
                    xtest.fake_key(conn, shift, false)?;
                } else {
                    xtest.tap(conn, keycode)?;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        "resize" => {
            let w: u32 = parts.get(1).and_then(|s| s.parse().ok()).ok_or("resize needs a width")?;
            let h: u32 = parts.get(2).and_then(|s| s.parse().ok()).ok_or("resize needs a height")?;
            // **Through the window manager when there is one.** `ConfigureWindow` on a managed window
            // is a request the WM may override, so the measurement that matters is what the server
            // says afterwards -- which is `the_window_manager_agrees_or_the_gate_reads_it_back`.
            window.configure_size(conn, w as u16, h as u16)?;
            conn.sync()?;
            std::thread::sleep(Duration::from_millis(600));
            let (x, y, gw, gh, _) = window.geometry_full(conn)?;
            eprintln!("uidrive: after asking for {w}x{h}, the server says {gw}x{gh} at ({x},{y})");
        }
        other => eprintln!("uidrive: unknown step {other:?}"),
    }
    let _ = app;
    Ok(())
}

fn point(parts: &[&str]) -> Result<(i32, i32), Box<dyn std::error::Error>> {
    let x: i32 = parts
        .get(1)
        .and_then(|s| s.parse().ok())
        .ok_or("a point needs an x")?;
    let y: i32 = parts
        .get(2)
        .and_then(|s| s.parse().ok())
        .ok_or("a point needs a y")?;
    Ok((x, y))
}

/// Move (and optionally click) at a **window-relative** point.
///
/// # Why this aims twice instead of once
///
/// **`move`/`click` take window-relative coordinates**, XTEST wants root coordinates, and the obvious
/// conversion -- `window position + wanted point`, with the position from
/// [`Window::geometry_full`] -- **is wrong on this host, and wrong in a way that reads as a bug in
/// the application.** Measured, asking for window `(72, 50)`:
///
/// ```text
/// GetGeometry says the window is at (50, 124)
/// QueryPointer says root (122, 174) is window (122, 36)
/// ```
///
/// **An offset of (-50, +14), stable across moves, unchanged by the window's size.** `GetGeometry`
/// reports `x, y` relative to the *parent*, and on a desktop with a reparenting window manager the
/// parent is not the root, so the two requests are answering different questions and the harness was
/// trusting one of them. **The symptom is "the click did nothing"** -- the pointer lands between two
/// menu headings -- which is indistinguishable from a broken hit test, a menu that refuses to open, or
/// a popup that paints behind the toolbar.
///
/// So it moves, **asks the server where the pointer actually is in the window's own frame**, and
/// corrects by the difference, up to [`AIM_ATTEMPTS`] times. **`QueryPointer` is the only authority**
/// for this question, because its window-relative answer is the number X will put in `event_x` and
/// `event_y` -- the numbers the application sees.
fn at(
    conn: &mut Conn,
    window: &Window,
    xtest: &XTest,
    x: i32,
    y: i32,
    click: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let (_, _, _, _, root) = window.geometry_full(conn)?;
    // **Two probes, and then a solve.**
    //
    // **The mapping from a root coordinate to the window coordinate the application sees is affine
    // but it is not the identity, and on this host it is not even a translation.** Measured: root
    // `(72, 50)` arrives as window `(144, -38)`, root `(100, 106)` as `(200, 74)`, root `(165, 88)`
    // as `(330, 38)` -- so `window = 2 x root` with a fixed offset, which is what an X server with a
    // scale factor of 2 does to `XTestFakeMotionEvent`'s coordinates. **`Window::geometry_full` does
    // not describe it either**: it reported the window at `(50, 124)` while the server placed root
    // `(122, 174)` at window `(122, 36)`.
    //
    // So the harness **measures** the mapping rather than assuming one: two known root points, two
    // readings from `QueryPointer`, one linear solve, and a verification move afterwards. **A third
    // residual correction closes the loop**, because a person moving the mouse on this desktop moves
    // the pointer between two requests of ours -- measured: a click aimed at `(72, 50)` arrived at
    // `(373, 276)`, and the application was perfectly blameless.
    let probe = |conn: &mut Conn, xtest: &XTest, rx: i16, ry: i16| -> Result<(i32, i32), Box<dyn std::error::Error>> {
        xtest.move_pointer(conn, root, rx, ry)?;
        std::thread::sleep(Duration::from_millis(15));
        let ((prx, pry, px, py), _) = window.pointer_position(conn)?;
        println!(
            "uidrive: probe root ({rx},{ry}) -> server root ({prx},{pry}), window ({px},{py})"
        );
        Ok((i32::from(px), i32::from(py)))
    };
    eprintln!("uidrive: the server's root window is {root:#x}");
    let w0 = probe(conn, xtest, 200, 200)?;
    let w1 = probe(conn, xtest, 200 + CALIBRATION_STEP, 200 + CALIBRATION_STEP)?;
    let sx = (w1.0 - w0.0) as f64 / f64::from(CALIBRATION_STEP);
    let sy = (w1.1 - w0.1) as f64 / f64::from(CALIBRATION_STEP);
    let (sx, sy) = (sx.clamp(0.05, 20.0), sy.clamp(0.05, 20.0));
    let solve = |target: i32, zero: i32, one: i32, scale: f64| -> i16 {
        let want = f64::from(target - zero) / scale;
        (want.round() as i32 + one).clamp(-32_000, 32_000) as i16
    };
    let mut ax = solve(x, w0.0, 200, sx);
    let mut ay = solve(y, w0.1, 200, sy);
    let mut seen = (i32::MAX, i32::MAX);
    let mut clicked = false;
    for attempt in 1..=AIM_ATTEMPTS {
        xtest.move_pointer(conn, root, ax, ay)?;
        std::thread::sleep(Duration::from_millis(15));
        let ((_rx, _ry, px, py), _) = window.pointer_position(conn)?;
        seen = (i32::from(px), i32::from(py));
        // **Within `AIM_TOLERANCE`, not exactly.**
        //
        // **On a server with a scale factor of 2 the pointer only lands on even physical pixels**, so a
        // target at an odd window coordinate is not reachable at all: asked for `(165, 88)` the server
        // answered `(164, 88)`, and a harness that treats that as a failure clicks nothing at all. **A
        // pixel is a pixel against a 21 px button**, and the alternative -- refusing to click because a
        // target is unreachable -- loses the interaction rather than approximating it.
        if (seen.0 - x).abs() <= AIM_TOLERANCE && (seen.1 - y).abs() <= AIM_TOLERANCE {
            // **The click goes here rather than after the loop.** Every request between "the pointer
            // is where I want it" and "click" is a place for something else to move it.
            if click {
                xtest.click_at(conn, root, ax, ay, 1)?;
                clicked = true;
            }
            break;
        }
        // **The residual is corrected in *scaled* space**, because the correction has to be expressed in
        // the units the request is written in: a one-pixel error at scale 2 is half a root pixel, and
        // adding a whole root pixel would overshoot by as much as it missed.
        let nx = i32::from(ax) + ((x - seen.0) as f64 / sx).round() as i32;
        let ny = i32::from(ay) + ((y - seen.1) as f64 / sy).round() as i32;
        ax = nx.clamp(-32_000, 32_000) as i16;
        ay = ny.clamp(-32_000, 32_000) as i16;
        if attempt == AIM_ATTEMPTS {
            eprintln!(
                "uidrive: could not put the pointer on window ({x},{y}) after {AIM_ATTEMPTS} \
                 attempts; the server says ({},{}) -- something else is moving the pointer",
                seen.0, seen.1
            );
        }
    }
    sleep_settle();
    eprintln!(
        "uidrive: window ({x},{y}) -> root ({ax},{ay}); the server calls that window ({},{}){}",
        seen.0,
        seen.1,
        if clicked { ", clicked" } else { "" }
    );
    Ok(())
}

/// How far apart the two calibration probes are, in root pixels.
///
/// **Ten, and not one.** A one-pixel probe measures the same number the pointer already reports, so a
/// scale of exactly 2 rounds to 2 either way and a scale of 1.4 rounds to 1; ten pixels apart is the
/// difference between those answers and the right one. **The probe is two requests and is paid once per
/// `click`/`move` step**, which is a few hundred microseconds against a 250 ms settle.
const CALIBRATION_STEP: i16 = 10;

/// How far off the target the pointer may land and still count as on it, in window pixels.
///
/// **One, and not zero**, because an X server with a scale factor of 2 only places the pointer on even
/// physical pixels: measured, a request for window `(165, 88)` came back as `(164, 88)` every time.
/// **Every widget this harness clicks is at least 21 px wide**, so a pixel of residual picks the same
/// widget the target named, and a tolerance of zero would have the harness refuse to click at all.
const AIM_TOLERANCE: i32 = 1;

/// How many times [`at`] will move, re-read and correct before it believes the pointer is where it
/// asked for.
///
/// **Three, and the third is a diagnostic rather than a hope.** One correction is enough for any
/// offset that is constant while the window is still, which is the case here; a second covers a window
/// manager that finishes placing the window after the first move. **A third that fails is printed and
/// then ignored**, because a screenshot of the wrong place is still a screenshot, and the printed line
/// says which point it is of.
const AIM_ATTEMPTS: u32 = 3;

fn sleep_settle() {
    std::thread::sleep(SETTLE);
}

/// Read the window's own pixels back and write a `.ppm`.
///
/// **`GetImage`, not a screen capture.** The window is the app's framebuffer as the server holds it,
/// so this is what the user would see, and it does not need a screenshot tool to be installed.
fn shot(
    conn: &mut Conn,
    window: &Window,
    out: &str,
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (_, _, w, h, _) = window.geometry_full(conn)?;
    let w16 = u16::try_from(w)?;
    let h16 = u16::try_from(h)?;
    let bytes = window.get_image(conn, 0, 0, w16, h16)?;
    let stride = w as usize * 4;
    if bytes.len() < stride * h as usize {
        return Err(format!("short readback: {} bytes for {w}x{h}", bytes.len()).into());
    }
    let path = format!("{out}/{label}.ppm");
    let mut f = std::fs::File::create(&path)?;
    write!(f, "P6\n{w} {h}\n255\n")?;
    let mut row = vec![0u8; w as usize * 3];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let p = &bytes[y * stride + x * 4..y * stride + x * 4 + 4];
            // XRGB little-endian: B, G, R, pad.
            row[x * 3] = p[2];
            row[x * 3 + 1] = p[1];
            row[x * 3 + 2] = p[0];
        }
        f.write_all(&row)?;
    }
    eprintln!("uidrive: wrote {path} ({w}x{h})");
    Ok(())
}