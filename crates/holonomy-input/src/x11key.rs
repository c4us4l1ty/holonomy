//! Turning X11 key events into the project's own key events.
//!
//! # The whole module is one subtraction
//!
//! `holonomy_input::Keymap` is indexed by Linux input codes -- `KEY_A`, `KEY_LEFTSHIFT`, the numbers
//! `/dev/input/event*` reports. X11 sends *X keycodes*, and the two numberings are the same table offset
//! by eight: X keycode 38 is `a`, `KEY_A` is 30.
//!
//! That is a claim, so it is checked rather than assumed. `crates/holonomy-x11/tests/live.rs` asks a
//! live server, through `GetKeyboardMapping`, which keycode carries each keysym, and asserts
//! `keycode - 8` equals the `KEY_*` constant for 23 keys -- letters, digits, punctuation, the modifiers,
//! the arrows, Tab, Enter, Backspace and F1/F12. All 23 agree.
//!
//! # Modifiers come from the press and release events, not from the state field
//!
//! X11 sends a modifier state with every key event, and this module ignores it. The reason is the same
//! as in `evdev.rs`: this project's modifier tracking is a fold over press and release events, done once
//! in `ModifierState::update`, and a second source of truth would be a second thing to be wrong. The
//! state field is decoded and available -- `Event::KeyPress`'s `state` is right there -- and the gate for
//! the *pair* of them agreeing is `a_shifted_character_arrives_as_four_key_events`.
//!
//! [`autorepeat`]: crate::Event::KeyPress

use holonomy_x11::Event;

use crate::event::{InputEvent, EV_KEY};

/// The offset between an X keycode and a Linux input code.
///
/// Xorg, Xwayland and every X server since the evdev migration number their keycodes eight above the
/// Linux codes, so that keycode 8 -- the first a server will report -- is `KEY_ESC` at 1. Verified on the
/// live server for 23 keys; see the module docs.
pub const KEYCODE_OFFSET: u8 = 8;

/// The Linux input code for an X keycode, if it is one this client has room for.
///
/// Keycodes below the offset would be a negative Linux code, which no device reports; they are `None`
/// rather than a wrapped number.
pub const fn evdev_code(keycode: u8) -> Option<u16> {
    if keycode < KEYCODE_OFFSET {
        None
    } else {
        Some((keycode - KEYCODE_OFFSET) as u16)
    }
}

/// The X keycode for a Linux input code.
pub const fn keycode_for(evdev: u16) -> Option<u8> {
    let k = evdev as u8;
    if evdev > u8::MAX as u16 || evdev + KEYCODE_OFFSET as u16 > u8::MAX as u16 {
        None
    } else {
        Some(k + KEYCODE_OFFSET)
    }
}

/// `BTN_LEFT`, for the arithmetic in [`x_button`].
const BTN_LEFT: u16 = crate::pointer::BTN_LEFT;

/// The key event an X event carries, or `None` if it carries no key.
///
/// A `KeyPress` becomes a press, a `KeyRelease` a release. Everything else -- exposure, focus, the
/// window manager's `ClientMessage` -- is `None`, because the transport is not the thing that decides
/// what a keystroke means.
pub fn key_event(event: &Event) -> Option<InputEvent> {
    let (code, value) = match event {
        Event::KeyPress { keycode, .. } => (*keycode, 1),
        Event::KeyRelease { keycode, .. } => (*keycode, 0),
        _ => return None,
    };
    Some(InputEvent {
        kind: EV_KEY,
        code: evdev_code(code)?,
        value,
    })
}

/// The pointer event an X event carries, or `None` if it carries no pointer.
///
/// # Why this is a separate function and not a second arm of `key_event`
///
/// **`key_event` returns an [`InputEvent`](crate::InputEvent), which cannot express a position.**
/// Bolt one into it and it stops being "the key event" while still being called that, and the next
/// caller reaches for it on a `MotionNotify` and gets a `None` they have to explain. Two functions,
/// each total, each returning one kind of thing.
///
/// # The button numbering, which is the trap
///
/// **X numbers buttons from 1; evdev numbers them from `0x110`.** Mapping `1 -> BTN_LEFT` is not a
/// cast, it is a decision, and getting it wrong means the right button does the left thing --
/// silently, because both are "a button".
///
/// **Buttons 4 and 5 are the wheel**, not buttons, on every X mouse. A pointer event for a wheel
/// notch has to say so, or a scroll would arrive as a click at a fixed position.
pub fn pointer_event(event: &Event) -> Option<crate::pointer::Event> {
    use crate::pointer::Event as P;
    match event {
        Event::MotionNotify { event_x, event_y } => Some(P::Motion {
            x: *event_x as i32,
            y: *event_y as i32,
        }),
        Event::ButtonPress {
            button,
            event_x,
            event_y,
        } => {
            let (x, y) = (*event_x as i32, *event_y as i32);
            match button {
                4 => Some(P::Wheel { dy: 1, x, y }),
                5 => Some(P::Wheel { dy: -1, x, y }),
                b => Some(P::Button {
                    button: x_button(*b),
                    pressed: true,
                    x,
                    y,
                }),
            }
        }
        Event::ButtonRelease {
            button,
            event_x,
            event_y,
        } => Some(P::Button {
            button: x_button(*button),
            pressed: false,
            x: *event_x as i32,
            y: *event_y as i32,
        }),
        _ => None,
    }
}

/// X's button number as an evdev button.
///
/// **X's 1 is evdev's `BTN_LEFT`, and the offset is 0x110 - 1.** Written as arithmetic rather than a
/// table so a fourth and fifth X button map to `BTN_SIDE` and `BTN_EXTRA` without a new arm, and so
/// the gate can assert the arithmetic rather than a list.
fn x_button(button: u8) -> crate::pointer::Button {
    let code = BTN_LEFT + (button.saturating_sub(1) as u16);
    crate::pointer::Button::from_code(code).unwrap_or(crate::pointer::Button::Other(code))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{InputEvent, EV_KEY};

    /// The offset, on the keys the project's keymap actually pairs. If this fails, every character the
    /// window path types is the wrong one.
    #[test]
    fn an_x_keycode_is_eight_above_the_linux_code() {
        assert_eq!(evdev_code(38), Some(30), "X 38 is 'a', KEY_A is 30");
        assert_eq!(evdev_code(9), Some(1), "X 9 is Escape, KEY_ESC is 1");
        assert_eq!(
            evdev_code(50),
            Some(42),
            "X 50 is Shift_L, KEY_LEFTSHIFT is 42"
        );
        assert_eq!(
            evdev_code(105),
            Some(97),
            "X 105 is Left, KEY_LEFT is 105 - 8 = 97"
        );
        assert_eq!(evdev_code(10), Some(2), "X 10 is '1', KEY_1 is 2");
        assert_eq!(evdev_code(23), Some(15), "X 23 is Tab, KEY_TAB is 15");
        assert_eq!(evdev_code(36), Some(28), "X 36 is Enter, KEY_ENTER is 28");
    }

    /// A keycode below the offset is not a key; wrapping it would produce a large code that lands on
    /// some unrelated key in the keymap.
    #[test]
    fn a_keycode_below_the_offset_is_not_a_key() {
        assert_eq!(evdev_code(0), None);
        assert_eq!(evdev_code(7), None);
        assert_eq!(evdev_code(8), Some(0));
    }

    /// The two directions round-trip, because a caller that wants to *inject* a key needs the inverse
    /// and a one-way pair is a bug waiting for its use.
    #[test]
    fn the_offset_round_trips() {
        for evdev in [1u16, 30, 42, 57, 97, 183] {
            let keycode = keycode_for(evdev).expect("a keycode for a Linux code");
            assert_eq!(evdev_code(keycode), Some(evdev), "{evdev} round trips");
        }
    }

    /// A press and a release become the two events the modifier fold needs, with the value that says
    /// which. This is the whole contract of the module.
    #[test]
    fn a_press_and_a_release_become_the_two_events() {
        let press = key_event(&Event::KeyPress {
            keycode: 38,
            state: 0,
            time: 0,
            event_x: 0,
            event_y: 0,
        });
        assert_eq!(
            press,
            Some(InputEvent {
                kind: EV_KEY,
                code: 30,
                value: 1
            }),
            "X keycode 38 is KEY_A, a press"
        );
        let release = key_event(&Event::KeyRelease {
            keycode: 38,
            state: 0,
            time: 0,
        });
        assert_eq!(
            release,
            Some(InputEvent {
                kind: EV_KEY,
                code: 30,
                value: 0
            })
        );
    }

    /// Everything that is not a key is nothing. A window that repaints, gains focus or is resized must
    /// not produce a keystroke, and the alternative -- mapping an `Expose` to some default code -- is how
    /// a stray character gets typed.
    #[test]
    fn every_other_event_is_no_key() {
        let others = [
            Event::Expose {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                count: 0,
            },
            Event::ConfigureNotify {
                width: 1,
                height: 1,
            },
            Event::MapNotify,
            Event::ButtonPress {
                button: 1,
                event_x: 0,
                event_y: 0,
            },
            Event::ButtonRelease { button: 1 },
            Event::ClientMessage {
                type_atom: 0,
                data1: 0,
            },
            Event::PropertyNotify { atom: 0 },
            Event::Other { code: 42 },
        ];
        for ev in others {
            assert_eq!(key_event(&ev), None, "{ev:?} is not a key");
        }
    }

    /// The X state field is ignored on purpose, and a press that carries ShiftMask is still a press of
    /// the same code. The shift is known from the `KeyPress` on the shift key itself.
    #[test]
    fn the_state_field_does_not_change_the_code() {
        let plain = key_event(&Event::KeyPress {
            keycode: 38,
            state: 0x0000,
            time: 1,
            event_x: 0,
            event_y: 0,
        });
        let shifted = key_event(&Event::KeyPress {
            keycode: 38,
            state: 0x0001,
            time: 2,
            event_x: 0,
            event_y: 0,
        });
        assert_eq!(plain, shifted, "both are KEY_A presses");
    }
}
