//! A minimal X11 core-protocol client, spoken directly over a unix socket.
//!
//! # Why this exists at all, and why there is no `minifb` in it
//!
//! The user asked for an interactive window on an unprivileged host. The obvious move is a windowing
//! crate, and the obvious candidates all resolve to the same problem: `minifb` and `softbuffer` reach
//! X11 through `x11-dl`, which is `dlopen("libX11.so.6")` at run time. A static musl binary cannot
//! `dlopen` anything, and Plan.md Part 4 §1 prohibits the dynamic loader anyway -- that is the
//! Zero-Compositor Invariant, and it is the reason this project is static in the first place.
//!
//! So the desktop path would have needed a *second* toolchain (`--target x86_64-unknown-linux-gnu`),
//! a new direct dependency, and an invariant exception. Talking the protocol ourselves costs about
//! the same amount of code as the dependency's transitive tree and costs no more: a socket, a
//! handshake, four request opcodes and an event stream. Measured on this host: the whole client is
//! `#[cfg]`-free, links against `libc` only, and the release binary that does *not* use it is
//! unchanged in size -- see `crates/holonomy/tests/release_artifact.rs`.
//!
//! **This crate is a development convenience and is not part of the bare-silicon product.** On the
//! ThinkPad there is no X server; [`crate::window::Window`] exists so that a human can type a
//! document without `sudo`. The production path is [`holonomy_display::drm::DrmScanout`] and
//! [`holonomy_input::EvdevSource`]. Nothing in the sealed boot chain refers to this crate, and
//! `holonomy-display`'s `desktop` feature is off by default.
//!
//! # What is implemented, and what is deliberately not
//!
//! Implemented, because the window needs it: the connection handshake, `CreateWindow`, `ChangeProperty`
//! for the title, `InternAtom`, `CreateGC`, `MapWindow`, `PutImage`, `GetImage`, `SetInputFocus`,
//! `QueryExtension`, `GetKeyboardMapping`, `NoOperation` as a sync point, and the event decode.
//!
//! Not implemented: `MIT-SHM` (so a full 1280x800 frame goes over the socket as 4 MiB rather than
//! being shared), `BigRequests` (so `PutImage` is chunked along scanlines), extensions beyond the
//! one query used to ask whether `XTEST` is there, and the whole of the ICCCM.
//!
//! # The one measurement this crate's correctness rests on
//!
//! X11 keycodes and Linux input event codes are the same numbering with an offset of 8: X keycode 38
//! is `a`, and `KEY_A` is 30. That is a *claim*, so
//! `tests/keycodes.rs` checks it against the live server's `GetKeyboardMapping` and
//! `tests/live.rs` checks it again while synthesising keystrokes. If the server ever disagreed,
//! `holonomy_input::Keymap` would be reading the wrong key and the gate would say so by name.

pub mod auth;
pub mod conn;
pub mod proto;
pub mod window;
pub mod xtest;

pub use conn::{Conn, ConnError, Setup};
pub use proto::{Event, ProtocolError, MAX_REQUEST_BYTES};
pub use window::{Pixmap, Window, WindowError};
pub use xtest::XTest;

/// A display number, the way `$DISPLAY` spells it, or `None` if this crate cannot serve it.
///
/// `None` is not only "unparseable". A display with a *host* part is a TCP display, and this crate
/// opens one unix socket and no TCP socket; reporting that as a number would send the caller to a
/// `/tmp/.X11-unix/X<n>` path that belongs to somebody else's server.
pub fn display_number(display: &str) -> Option<u32> {
    let (host, spec) = display.rsplit_once(':')?;
    if !(host.is_empty() || host == "unix" || host == "localhost") {
        return None;
    }
    // "unix:0.1" is display 0 screen 1; the screen is irrelevant to a client with no reason to open
    // more than one of them, and refusing the parse would be refusing a working display.
    let digits: String = spec.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::display_number;

    /// `DISPLAY` is not `:0` in every session, and a client that only understands `:0` is a client
    /// that fails on a workstation with two monitors.
    #[test]
    fn a_display_spec_yields_its_number() {
        assert_eq!(display_number(":0"), Some(0));
        assert_eq!(display_number(":1"), Some(1));
        assert_eq!(display_number("unix:0"), Some(0));
        assert_eq!(display_number(":0.0"), Some(0));
        assert_eq!(display_number("localhost:10.0"), Some(10));
        // A remote display needs TCP, which this crate does not speak. Reported as "no number" so the
        // caller says so rather than opening a local socket that does not exist.
        assert_eq!(display_number("build-box.internal:12.0"), None);
        assert_eq!(display_number(""), None);
        assert_eq!(display_number(":"), None);
    }
}
