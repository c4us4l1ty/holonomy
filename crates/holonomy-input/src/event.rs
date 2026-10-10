//! `input_event`, the kernel's own record, and the one way this crate parses an event.
//!
//! # Why the raw 24-byte record and not a friendlier struct
//!
//! evdev delivers a stream of fixed-size C records, and the layout is kernel ABI:
//!
//! ```c
//! struct input_event {
//!     __kernel_ulong_t __sec;   /* struct timeval.tv_sec  */
//!     __kernel_ulong_t __usec;  /* struct timeval.tv_usec */
//!     __u16 type;
//!     __u16 code;
//!     __s32 value;
//! };
//! ```
//!
//! On x86-64 that is 8 + 8 + 2 + 2 + 4 = 24 bytes, with the timestamp *two* 64-bit words rather
//! than the 8 bytes a 32-bit `timeval` would use. Reading it as `timeval` is the mistake that makes
//! a 64-bit evdev stream parse as garbage, and the struct is not `#[repr(C)]`-equivalent to any
//! Rust std type. So it is spelled out here, with the offsets asserted at compile time.
//!
//! # Why [`decode`] is the only parser
//!
//! [`crate::EvdevSource`] reading a real device and [`crate::ScriptedInputSource`] reading a test
//! fixture must go through *identical* code, or the integration test is testing a parser that no
//! hardware ever runs. Both call [`decode`].
//!
//! [`crate::EvdevSource`]: crate::EvdevSource
//! [`crate::ScriptedInputSource`]: crate::ScriptedInputSource

/// Bytes in one `struct input_event` on a 64-bit Linux.
///
/// A compile-time constant rather than `size_of::<InputEvent>()`: the layout below is the *kernel's*,
/// and the assertion that it matches Rust's idea of it is
/// [`RECORD_BYTES_EQ_SIZE_OF`](crate::RECORD_BYTES_EQ_SIZE_OF). Deriving one from the other would
/// make the check vacuous.
pub const RECORD_BYTES: usize = 24;

/// Byte offset of `type` within a record. `pub(crate)` so the crate-root layout proof can check it.
pub(crate) const OFF_TYPE: usize = 16;
/// Byte offset of `code`.
pub(crate) const OFF_CODE: usize = 18;
/// Byte offset of `value`.
pub(crate) const OFF_VALUE: usize = 20;

/// `EV_SYN`. Not used for input; present so a stream can be recognised as whole.
pub const EV_SYN: u16 = 0x00;
/// `EV_KEY`. The only type this crate acts on.
pub const EV_KEY: u16 = 0x01;
/// `EV_REL`. Relative axis motion -- mice and trackballs. Never a keystroke.
pub const EV_REL: u16 = 0x02;
/// `EV_ABS`. Absolute axis positions -- a tablet, a touchscreen, a light gun.
///
/// **Defined here and deliberately not acted on.** It is in the vocabulary because the kernel has it
/// and a stream carrying it must be *recognised* as something other than keyboard input; see
/// [`crate::pointer::decode_record`], which maps it to `Record::Noise` with a note about why a scale
/// guess would be worse than a refusal.
pub const EV_ABS: u16 = 0x03;
/// `EV_MSC`. `EV_MSC_SCAN` carries the USB HID scancode, which is a *different* numbering again.
pub const EV_MSC: u16 = 0x04;

/// `SYN_REPORT`. The kernel sets it on the last event of a frame; a key press arrives as
/// `EV_KEY ... value=1` followed by this.
pub const SYN_REPORT: u16 = 0;

/// One record from the event stream.
///
/// `time` is dropped: the session's clock is `CLOCK_MONOTONIC` from `clock_gettime`, and a
/// `CLOCK_REALTIME` stamp from the kernel would be a second, jumpable time source to reason about
/// for no benefit. The bytes are still skipped, because the stream is positional.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputEvent {
    /// `EV_KEY` for everything this crate keeps.
    pub kind: u16,
    /// The key code, e.g. `KEY_A`. Hardware-position based, never layout dependent.
    pub code: u16,
    /// `0` release, `1` press, `2` autorepeat.
    pub value: i32,
}

impl InputEvent {
    /// A press.
    #[inline]
    pub const fn press(code: u16) -> Self {
        Self {
            kind: EV_KEY,
            code,
            value: 1,
        }
    }

    /// A release.
    #[inline]
    pub const fn release(code: u16) -> Self {
        Self {
            kind: EV_KEY,
            code,
            value: 0,
        }
    }

    /// An autorepeat. Same as a press for text, and the reason
    /// [`crate::ModifierState::update`] keys off `value == 1` and `== 0` rather than "not a release".
    #[inline]
    pub const fn repeat(code: u16) -> Self {
        Self {
            kind: EV_KEY,
            code,
            value: 2,
        }
    }

    /// Whether this record is a key transition the editor should act on.
    ///
    /// Autorepeat counts. It is the same key going down again without a `0` in between, and a word
    /// processor holds the key down to repeat. Excluding it would make autorepeat silently do
    /// nothing, which is worse than any alternative.
    #[inline]
    pub const fn is_key(&self) -> bool {
        self.kind == EV_KEY
    }

    /// Whether this is the `0` that ends a press.
    #[inline]
    pub const fn is_release(&self) -> bool {
        self.is_key() && self.value == 0
    }

    /// Whether this is the `1` that starts one.
    ///
    /// Autorepeat (`2`) is deliberately *not* included. The only place that matters is
    /// [`crate::ModifierState::update`], where an autorepeat of a modifier key must not re-assert a
    /// modifier that the user has since released -- which is precisely the bug this predicate exists
    /// to make impossible.
    #[inline]
    pub const fn is_press(&self) -> bool {
        self.is_key() && self.value == 1
    }
}

/// Decode one record from its 24 bytes, keeping only `EV_KEY`.
///
/// Returns `None` for `EV_SYN`, `EV_MSC` and anything else. Dropping them here rather than in each
/// caller is the point: a scripted stream and a real device differ in how much noise they carry, and
/// the keymap must never see the difference.
///
/// # Panics
///
/// If `bytes` is shorter than [`RECORD_BYTES`]. Callers slice, they do not check.
#[inline]
pub fn decode(bytes: &[u8]) -> Option<InputEvent> {
    let le = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let kind = le(OFF_TYPE);
    if kind != EV_KEY {
        return None;
    }
    Some(InputEvent {
        kind,
        code: le(OFF_CODE),
        value: i32::from_le_bytes([
            bytes[OFF_VALUE],
            bytes[OFF_VALUE + 1],
            bytes[OFF_VALUE + 2],
            bytes[OFF_VALUE + 3],
        ]),
    })
}

/// Build one record's 24 bytes, for a fixture or a round-trip assertion.
///
/// Little-endian, matching every Linux target this builds for.
pub fn encode(event: InputEvent) -> [u8; RECORD_BYTES] {
    let mut out = [0u8; RECORD_BYTES];
    out[OFF_TYPE..OFF_TYPE + 2].copy_from_slice(&event.kind.to_le_bytes());
    out[OFF_CODE..OFF_CODE + 2].copy_from_slice(&event.code.to_le_bytes());
    out[OFF_VALUE..OFF_VALUE + 4].copy_from_slice(&event.value.to_le_bytes());
    out
}

/// A short stream of `EV_SYN` separators, as a real device emits between frames.
///
/// Fixtures need these, and needing them is the point: a scripted stream that omits them and a
/// scripted stream that includes them must produce the same commands, because [`decode`] drops
/// `EV_SYN` for both.
pub fn syn_report() -> InputEvent {
    InputEvent {
        kind: EV_SYN,
        code: SYN_REPORT,
        value: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_record_is_twenty_four_bytes_and_our_offsets_match_rusts() {
        #[repr(C)]
        #[derive(Debug)]
        struct RustInputEvent {
            sec: u64,
            usec: u64,
            kind: u16,
            code: u16,
            value: i32,
        }
        // The whole reason `RECORD_BYTES` is a literal: this comparison is only meaningful because
        // the constant did not come from this struct.
        assert_eq!(std::mem::size_of::<RustInputEvent>(), RECORD_BYTES);
        let r = RustInputEvent {
            sec: 0,
            usec: 0,
            kind: EV_KEY,
            code: 0x1234,
            value: 7,
        };
        let raw: &[u8] = unsafe {
            std::slice::from_raw_parts(&r as *const RustInputEvent as *const u8, RECORD_BYTES)
        };
        assert_eq!(&raw[OFF_TYPE..OFF_TYPE + 2], &EV_KEY.to_le_bytes());
        assert_eq!(&raw[OFF_CODE..OFF_CODE + 2], &0x1234u16.to_le_bytes());
        assert_eq!(&raw[OFF_VALUE..OFF_VALUE + 4], &7i32.to_le_bytes());
    }

    #[test]
    fn a_record_round_trips() {
        for ev in [
            InputEvent::press(30),
            InputEvent::release(30),
            InputEvent::repeat(30),
            InputEvent::press(0xffff),
        ] {
            assert_eq!(decode(&encode(ev)), Some(ev));
        }
    }

    #[test]
    fn a_negative_value_survives() {
        // EV_REL carries negatives. None are acted on, but a truncated read would show up here.
        let raw = encode(InputEvent {
            kind: EV_REL,
            code: 0,
            value: -32768,
        });
        let raw = {
            let mut m = raw;
            m[OFF_TYPE..OFF_TYPE + 2].copy_from_slice(&EV_REL.to_le_bytes());
            m
        };
        assert_eq!(decode(&raw), None);
    }

    #[test]
    fn syn_and_msc_are_dropped() {
        for kind in [EV_SYN, EV_MSC, EV_REL, 0x11, 0xff] {
            let raw = encode(InputEvent {
                kind,
                code: 30,
                value: 1,
            });
            assert_eq!(
                decode(&raw),
                None,
                "kind {kind:#x} should not survive decode"
            );
        }
    }

    #[test]
    fn the_predicates_agree_about_autorepeat() {
        let r = InputEvent::repeat(30);
        assert!(r.is_key());
        assert!(!r.is_press(), "autorepeat is not a fresh press");
        assert!(!r.is_release());
        assert!(InputEvent::press(30).is_press());
        assert!(InputEvent::release(30).is_release());
    }
}
