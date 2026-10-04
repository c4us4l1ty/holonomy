//! The modifier keys currently held down.
//!
//! # Why a bitfield and not a counter
//!
//! There are two shift keys, two ctrls and two alts on a keyboard, and evdev reports them as
//! distinct codes. The interesting cases are all about the pair:
//!
//! * Hold Left Shift, hold Right Shift, release Left Shift. Shift must still be on. A counter gets
//!   this right and a pair of `bool`s does not.
//! * Hold Left Ctrl, press and release Left Ctrl without ever releasing it (a keyboard that repeats,
//!   or a hotplug that replays state). A `bool` set to true on every press is fine; one *toggled*
//!   per press is not.
//! * Autorepeat a modifier. Held down, `value == 2` arrives forever. It must not count.
//!
//! So this is a bitfield of *key codes*, updated only on `value == 1` and `value == 0`, and
//! [`is_shift`] and friends ask "is either bit set" rather than counting. The autorepeat exclusion
//! is not a special case in the update; it falls out of keying off the two exact values, which is
//! the only way it cannot be forgotten later.

/// The modifier keys this state tracks.
///
/// `KEY_LEFTSHIFT` and friends are kernel ABI (`linux/input-event-codes.h`) and are stable across
/// every kernel since forever. They are duplicated from [`crate::keymap`] rather than imported so
/// that this file reads without the table, at the cost of one compile error if a value drifts.
use crate::keymap::{
    KEY_CAPSLOCK, KEY_LEFTALT, KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_RIGHTALT, KEY_RIGHTCTRL,
    KEY_RIGHTSHIFT,
};

/// Which physical modifier keys are down right now.
///
/// `Default` is all-up, which is the state a process starts in and the only correct one: the kernel
/// does not replay held modifiers to a newly opened evdev device, so a user pressing Ctrl+Q in the
/// first second after launch genuinely means it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModifierState {
    shift: u8,
    ctrl: u8,
    alt: u8,
    caps_lock: bool,
}

/// The bits used inside the three `u8` masks.
///
/// `KEY_*` values for the modifier keys are 42, 54, 29, 97, 56, 100 -- too sparse to use as bit
/// positions directly, so each is mapped to one. The mapping is explicit rather than computed
/// because a computed one would read `1 << (code % 8)` and collide the moment the kernel picks a
/// code that does.
mod bit {
    pub const LSHIFT: u8 = 1 << 0;
    pub const RSHIFT: u8 = 1 << 1;
    pub const LCTRL: u8 = 1 << 2;
    pub const RCTRL: u8 = 1 << 3;
    pub const LALT: u8 = 1 << 4;
    pub const RALT: u8 = 1 << 5;
}

impl ModifierState {
    /// All modifiers up.
    pub const fn new() -> Self {
        Self {
            shift: 0,
            ctrl: 0,
            alt: 0,
            caps_lock: false,
        }
    }

    /// Fold an event into the state.
    ///
    /// # The two exact values, and why
    ///
    /// `value == 1` sets the bit, `value == 0` clears it, and **`value == 2` does nothing.** That
    /// third case is the whole reason this is a function with a `match` on the value rather than
    /// `if value != 0 { set } else { clear }`, which is the shape that gets autorepeat wrong.
    ///
    /// A released key whose bit was never set is not an error: evdev does not guarantee a press
    /// preceded every release, and a session that started mid-hold will see a release for a key it
    /// never saw go down. Clearing an unset bit is a no-op, and that is the right reading.
    ///
    /// Caps Lock is the exception to the bitfield and the reason it is not one: it is *latched*, not
    /// held. It follows `value == 1` like the others, because on a standard keyboard the LED and the
    /// kernel's view agree, but it survives the key going back up.
    #[inline]
    pub fn update(&mut self, code: u16, value: i32) {
        match value {
            1 => self.set(code, true),
            0 => self.set(code, false),
            // 2 is autorepeat. The key is already down and the bit already reflects that.
            _ => {}
        }
    }

    /// Set or clear one key's bit.
    #[inline]
    fn set(&mut self, code: u16, down: bool) {
        match code {
            KEY_LEFTSHIFT => self.shift = apply(self.shift, bit::LSHIFT, down),
            KEY_RIGHTSHIFT => self.shift = apply(self.shift, bit::RSHIFT, down),
            KEY_LEFTCTRL => self.ctrl = apply(self.ctrl, bit::LCTRL, down),
            KEY_RIGHTCTRL => self.ctrl = apply(self.ctrl, bit::RCTRL, down),
            KEY_LEFTALT => self.alt = apply(self.alt, bit::LALT, down),
            KEY_RIGHTALT => self.alt = apply(self.alt, bit::RALT, down),
            // Caps Lock is latched, and the *toggle* is the keyboard's: each physical press arrives
            // as a fresh `value == 1`, and the keyboard's own state decides whether that turns the
            // latch on or off. So the press toggles and the release is ignored.
            //
            // Assigning `down` instead of toggling would make Caps Lock a one-way door -- press it and
            // the only way out is the Caps Lock *hotkey* -- which is a bug the hardware does not have.
            KEY_CAPSLOCK if down => self.caps_lock = !self.caps_lock,
            KEY_CAPSLOCK => {}
            _ => {}
        }
    }

    /// Either shift, from either side of the keyboard.
    #[inline]
    pub const fn shift(&self) -> bool {
        self.shift != 0
    }

    #[inline]
    pub const fn ctrl(&self) -> bool {
        self.ctrl != 0
    }

    #[inline]
    pub const fn alt(&self) -> bool {
        self.alt != 0
    }

    #[inline]
    pub const fn caps_lock(&self) -> bool {
        self.caps_lock
    }

    /// The mask value, for a `Debug` dump that shows *which* keys are down rather than just that
    /// shift is.
    ///
    /// Only meaningful for shift, ctrl and alt; caps lock is not a mask.
    #[inline]
    pub const fn shift_mask(&self) -> u8 {
        self.shift
    }

    #[inline]
    pub const fn ctrl_mask(&self) -> u8 {
        self.ctrl
    }

    #[inline]
    pub const fn alt_mask(&self) -> u8 {
        self.alt
    }

    /// Forget everything, for a session that has lost track.
    ///
    /// There is a real case for this beyond error recovery: the keyboard device is unplugged and
    /// replugged, and the kernel gives the new device fresh state. If the session had Shift held
    /// when it went away, it will otherwise believe Shift is still down forever, and every
    /// subsequent letter types uppercase.
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

/// Set or clear `bit` in `mask`, returning the new mask.
#[inline]
const fn apply(mask: u8, bit: u8, down: bool) -> u8 {
    if down {
        mask | bit
    } else {
        mask & !bit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::InputEvent;

    fn press(ed: &mut ModifierState, code: u16) {
        ed.update(code, 1);
    }
    fn release(ed: &mut ModifierState, code: u16) {
        ed.update(code, 0);
    }

    #[test]
    fn starts_all_up() {
        let ed = ModifierState::new();
        assert!(!ed.shift() && !ed.ctrl() && !ed.alt() && !ed.caps_lock());
    }

    #[test]
    fn the_two_shifts_are_independent() {
        let mut ed = ModifierState::new();
        press(&mut ed, KEY_LEFTSHIFT);
        assert!(ed.shift());
        press(&mut ed, KEY_RIGHTSHIFT);
        release(&mut ed, KEY_LEFTSHIFT);
        assert!(
            ed.shift(),
            "Left Shift released while Right Shift is still held"
        );
        assert_eq!(ed.shift_mask(), bit::RSHIFT);
        release(&mut ed, KEY_RIGHTSHIFT);
        assert!(!ed.shift());
    }

    #[test]
    fn the_two_ctrs_are_independent() {
        let mut ed = ModifierState::new();
        press(&mut ed, KEY_LEFTCTRL);
        press(&mut ed, KEY_RIGHTCTRL);
        release(&mut ed, KEY_LEFTCTRL);
        assert!(ed.ctrl());
        assert_eq!(ed.ctrl_mask(), bit::RCTRL);
    }

    #[test]
    fn the_two_alts_are_independent() {
        let mut ed = ModifierState::new();
        press(&mut ed, KEY_LEFTALT);
        press(&mut ed, KEY_RIGHTALT);
        release(&mut ed, KEY_LEFTALT);
        assert!(ed.alt());
        assert_eq!(ed.alt_mask(), bit::RALT);
    }

    #[test]
    fn autorepeat_does_not_re_assert_a_released_modifier() {
        let mut ed = ModifierState::new();
        press(&mut ed, KEY_LEFTSHIFT);
        release(&mut ed, KEY_LEFTSHIFT);
        assert!(!ed.shift());
        // The kernel keeps sending `2` for a key it believes is down. It must not turn Shift back on.
        ed.update(KEY_LEFTSHIFT, 2);
        assert!(!ed.shift(), "autorepeat resurrected a released modifier");
    }

    #[test]
    fn autorepeat_while_held_is_a_no_op() {
        let mut ed = ModifierState::new();
        press(&mut ed, KEY_LEFTSHIFT);
        for _ in 0..100 {
            ed.update(KEY_LEFTSHIFT, 2);
        }
        release(&mut ed, KEY_LEFTSHIFT);
        assert!(!ed.shift());
    }

    #[test]
    fn caps_lock_latches_across_the_key_going_up() {
        let mut ed = ModifierState::new();
        press(&mut ed, KEY_CAPSLOCK);
        assert!(ed.caps_lock());
        release(&mut ed, KEY_CAPSLOCK);
        assert!(ed.caps_lock(), "Caps Lock is latched, not held");
        press(&mut ed, KEY_CAPSLOCK);
        release(&mut ed, KEY_CAPSLOCK);
        assert!(!ed.caps_lock(), "a second press should toggle it off");
    }

    #[test]
    fn caps_lock_ignores_autorepeat() {
        let mut ed = ModifierState::new();
        press(&mut ed, KEY_CAPSLOCK);
        for _ in 0..100 {
            ed.update(KEY_CAPSLOCK, 2);
        }
        assert!(ed.caps_lock(), "autorepeat toggled Caps Lock");
    }

    #[test]
    fn a_release_with_no_matching_press_is_a_no_op() {
        let mut ed = ModifierState::new();
        release(&mut ed, KEY_LEFTSHIFT);
        release(&mut ed, KEY_LEFTCTRL);
        release(&mut ed, KEY_LEFTALT);
        release(&mut ed, KEY_CAPSLOCK);
        assert!(!ed.shift() && !ed.ctrl() && !ed.alt() && !ed.caps_lock());
    }

    #[test]
    fn a_non_modifier_code_is_ignored() {
        let mut ed = ModifierState::new();
        press(&mut ed, KEY_A_PLACEHOLDER);
        assert!(!ed.shift() && !ed.ctrl() && !ed.alt());
    }

    #[test]
    fn reset_forgets_everything() {
        let mut ed = ModifierState::new();
        press(&mut ed, KEY_LEFTSHIFT);
        press(&mut ed, KEY_LEFTCTRL);
        press(&mut ed, KEY_LEFTALT);
        press(&mut ed, KEY_CAPSLOCK);
        ed.reset();
        assert!(!ed.shift() && !ed.ctrl() && !ed.alt() && !ed.caps_lock());
    }

    #[test]
    fn update_through_an_event_matches_update_through_a_code() {
        let mut a = ModifierState::new();
        let mut b = ModifierState::new();
        for ev in [
            InputEvent::press(KEY_LEFTSHIFT),
            InputEvent::repeat(KEY_A_PLACEHOLDER),
            InputEvent::press(KEY_LEFTCTRL),
            InputEvent::release(KEY_LEFTSHIFT),
        ] {
            a.update(ev.code, ev.value);
            b.update(ev.code, ev.value);
        }
        assert_eq!(a, b);
    }

    const KEY_A_PLACEHOLDER: u16 = 30;
}
