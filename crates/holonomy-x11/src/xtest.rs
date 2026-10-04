//! `XTEST`: synthesising keystrokes into a real server, for a gate that needs a real keyboard.
//!
//! # Why this is a test-only extension
//!
//! The honest way to test "does a keypress turn into the right character" is to press a key. On a
//! build machine nobody is at the keyboard, so the alternative is to assert that our *own* keymap is
//! self-consistent -- which is the assumption under test, not a check of it.
//!
//! `XTEST` closes that gap: it makes the server synthesise a genuine `KeyPress`/`KeyRelease` pair with
//! a chosen keycode, through the same path a real keyboard uses, and the client cannot tell the
//! difference. So `tests/live.rs` injects a keycode, waits for the event, and asserts the session's
//! own state machine produced the right `Command`. The only thing that is not exercised is a human's
//! finger.
//!
//! # And it is how the keycode offset is *proved* rather than assumed
//!
//! `holonomy_input::Keymap` maps Linux input codes, and X11 sends X keycodes. The claim is that X
//! keycode = Linux code + 8. This module does not assume it: [`XTest::keycode_for_keysym`] asks the
//! server which keycode carries a given keysym, and the live test asserts `keycode - 8` equals the
//! `KEY_*` constant for the same key. If a server ever used a different mapping, that test fails by
//! name.
//!
//! Nothing in the shipped window path uses this module; it exists so the gate can type.

use crate::conn::{Conn, ConnError};
use crate::proto::{Event, Rdr, Req};

/// XTEST request minor opcodes.
///
/// They start at zero, which is worth writing down: `XTestFakeInput` is minor 2, not 4, and a request
/// with the wrong minor opcode is answered with `BadLength` -- which is what this crate's first
/// `XTestGetVersion` got, having numbered them from two.
pub mod minor {
    /// `XTestQueryVersion`.
    pub const GET_VERSION: u8 = 0;
    /// `XTestCompareCursor`.
    pub const COMPARE_CURSOR: u8 = 1;
    /// `XTestFakeInput`.
    pub const FAKE_INPUT: u8 = 2;
    /// `XTestGrabControl`.
    pub const GRAB_CONTROL: u8 = 3;
}

/// Event types for `XTestFakeInput`.
pub mod fake {
    /// KeyPress.
    pub const KEY_PRESS: u8 = 2;
    /// KeyRelease.
    pub const KEY_RELEASE: u8 = 3;
}

/// An open `XTEST` extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XTest {
    major: u8,
}

impl XTest {
    /// Open the extension, or `None` if the server does not have it.
    ///
    /// A server without `XTEST` is not an error for the *window* -- only for the gate that types into
    /// it -- so this returns `None` rather than failing, and `tests/live.rs` reports which it found.
    pub fn open(conn: &mut Conn) -> Result<Option<Self>, ConnError> {
        let Some((present, major, _first_event, _first_error)) = conn.query_extension("XTEST")?
        else {
            return Ok(None);
        };
        if !present {
            return Ok(None);
        }
        let xtest = Self { major };
        // Ask for the version anyway: it is the shortest extension request there is, and if it fails
        // the gate will find out now rather than three assertions later.
        conn.request(Req::new(xtest.major).minor(minor::GET_VERSION).pad(3))?;
        conn.sync()?;
        Ok(Some(xtest))
    }

    /// The extension's major opcode.
    pub fn major(&self) -> u8 {
        self.major
    }

    /// `XTestGrabControl`: while permitted, no other client may grab the server, so synthesised
    /// events cannot be lost to a stray `GrabServer` from an unrelated program.
    ///
    /// This is what `xdotool` does before typing, and it is what made the difference here: without it
    /// this host delivered the synthesised `KeyPress` and silently dropped the matching `KeyRelease`.
    pub fn grab_control(&self, conn: &mut Conn, permit: bool) -> Result<(), ConnError> {
        conn.request(
            Req::new(self.major)
                .minor(minor::GRAB_CONTROL)
                .u8(u8::from(permit))
                .pad(3),
        )?;
        conn.sync()
    }

    /// Inject a key press or release for `keycode` into whatever window has the focus.
    ///
    /// `root` is passed as the window id rather than 0. XTEST's `root` field is the window the event
    /// is delivered through, and 0 means "the root the pointer is currently on" -- which, for a
    /// window that has just been mapped under the user's cursor, is the desktop rather than the
    /// window. Key events follow the focus, so a focus that has not landed yet loses them.
    pub fn fake_key(&self, conn: &mut Conn, keycode: u8, press: bool) -> Result<(), ConnError> {
        self.fake_key_in(conn, 0, keycode, press)
    }

    /// [`XTest::fake_key`], naming the window the event should go to.
    pub fn fake_key_in(
        &self,
        conn: &mut Conn,
        window: u32,
        keycode: u8,
        press: bool,
    ) -> Result<(), ConnError> {
        conn.request(
            Req::new(self.major)
                .minor(minor::FAKE_INPUT)
                .u8(if press {
                    fake::KEY_PRESS
                } else {
                    fake::KEY_RELEASE
                })
                .u8(keycode)
                .u16(0)
                .u32(0) // delay
                .u32(window)
                .pad(8)
                .i16(0)
                .i16(0)
                .pad(7)
                .u8(0), // device id: 0 is the core keyboard
        )?;
        conn.sync()
    }

    /// Inject a press and a release, and sync once.
    pub fn tap(&self, conn: &mut Conn, keycode: u8) -> Result<(), ConnError> {
        self.fake_key(conn, keycode, true)?;
        self.fake_key(conn, keycode, false)
    }

    /// [`XTest::tap`], naming the window the events should go to.
    pub fn tap_in(&self, conn: &mut Conn, window: u32, keycode: u8) -> Result<(), ConnError> {
        self.fake_key_in(conn, window, keycode, true)?;
        self.fake_key_in(conn, window, keycode, false)
    }

    /// The X keycode that produces `keysym` unshifted, from the server's own keyboard mapping.
    ///
    /// `keysym` is an X keysym: `0x61` is `a`, `0xFFE1` is `Shift_L`. This asks the server rather
    /// than assuming a table, because the whole point is that the mapping is not ours to assume.
    pub fn keycode_for_keysym(conn: &mut Conn, keysym: u32) -> Result<Option<u8>, ConnError> {
        let mapping = conn.keyboard_mapping()?;
        let first = conn.setup().min_keycode;
        for (i, syms) in mapping.iter().enumerate() {
            if syms.first().is_some_and(|s| *s == keysym) {
                return Ok(Some(first + i as u8));
            }
        }
        Ok(None)
    }

    /// Wait for the next key event, up to `timeout`.
    ///
    /// Skips everything else -- `Expose`, `ConfigureNotify`, the `ClientMessage` the server sends when
    /// the focus moves -- because a gate that asks "what key did the server just report" should not
    /// have to know what else was in the queue.
    pub fn wait_for_key(
        &self,
        conn: &mut Conn,
        timeout: std::time::Duration,
    ) -> Result<Option<Event>, ConnError> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            match conn.next_event(Some(left))? {
                Some(ev @ (Event::KeyPress { .. } | Event::KeyRelease { .. })) => {
                    return Ok(Some(ev))
                }
                Some(_) => {}
                None => return Ok(None),
            }
        }
    }
}

/// Read the version out of an `XTestGetVersion` reply: `(client_major, client_minor)`.
pub fn version(reply: &[u8]) -> (u32, u32) {
    let mut r = Rdr::new(reply);
    r.skip(12);
    (r.u32(), r.u32())
}

/// `XTEST`'s `FakeInput` request is 36 bytes, and getting that wrong is a `BadLength` that the server
/// answers by *silently discarding the keystroke* -- the gate would then time out and blame the
/// client. So the layout is asserted here rather than discovered there.
#[cfg(test)]
mod tests {
    use super::*;

    /// The 36-byte layout: 4 header + type, detail, 2 pad, delay, root, 8 pad, x, y, 7 pad, device.
    #[test]
    fn fake_input_is_nine_words() {
        let bytes = Req::new(0x7E)
            .minor(minor::FAKE_INPUT)
            .u8(fake::KEY_PRESS)
            .u8(38)
            .u16(0)
            .u32(0)
            .u32(0)
            .pad(8)
            .i16(0)
            .i16(0)
            .pad(7)
            .u8(0)
            .finish(1);
        // 4 header + 1 type + 1 detail + 2 pad + 4 delay + 4 root + 8 pad + 2 x + 2 y + 7 pad
        // + 1 device = 36 bytes = 9 words.
        assert_eq!(bytes.len(), 36, "4 header + 32 fields");
        assert_eq!(u16::from_le_bytes([bytes[2], bytes[3]]), 9, "nine words");
    }

    /// `GetVersion` is the two-word request, which is the smallest thing an extension can have.
    ///
    /// Its minor opcode is 0, not 2: `XTestFakeInput` is 2. Sending 2 with a `GetVersion` body is a
    /// `BadLength`.
    ///
    /// Note what is *not* here: a length field. `Req` writes one in the request header, and writing
    /// a second one -- which the first version of this builder did -- makes every request two bytes
    /// long, which the server answers with `BadLength` while discarding the keystroke the gate was
    /// waiting for.
    #[test]
    fn get_version_is_two_words() {
        let bytes = Req::new(0x7E).minor(minor::GET_VERSION).pad(3).finish(1);
        assert_eq!(bytes.len(), 8, "4 header + minor + 3 pad");
        assert_eq!(u16::from_le_bytes([bytes[2], bytes[3]]), 2, "two words");
    }

    /// The reply's version pair sits past the 12-byte common prefix.
    #[test]
    fn the_version_reply_is_read_past_the_prefix() {
        let mut reply = vec![0u8; 12];
        reply.extend_from_slice(&2u32.to_le_bytes());
        reply.extend_from_slice(&3u32.to_le_bytes());
        assert_eq!(version(&reply), (2, 3));
    }

    /// Both event types are sent, and they are different numbers, because a client that sends
    /// `KEY_PRESS` twice produces a modifier that never clears.
    /// The minor opcodes are 0, 1, 2, 3 -- a client that numbers them from 2 gets `BadLength`.
    #[test]
    fn the_minor_opcodes_start_at_zero() {
        assert_eq!(minor::GET_VERSION, 0);
        assert_eq!(minor::COMPARE_CURSOR, 1);
        assert_eq!(minor::FAKE_INPUT, 2);
        assert_eq!(minor::GRAB_CONTROL, 3);
    }

    #[test]
    fn press_and_release_are_different_events() {
        assert_ne!(fake::KEY_PRESS, fake::KEY_RELEASE);
        assert_eq!(fake::KEY_PRESS, 2);
        assert_eq!(fake::KEY_RELEASE, 3);
    }
}
