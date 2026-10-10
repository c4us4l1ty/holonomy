//! **What the input crate delivers: keys, motion, buttons and the wheel.** Phase 14 part 20.
//!
//! # What this file is for
//!
//! `event.rs` is about the kernel's 24-byte record and knows no keys and no commands. This file is
//! about the other end: the handful of things a caller can be asked to do, decoded out of those
//! records. **They are separate subjects and putting them in one file is how a parser ends up
//! knowing what a toolbar is.**
//!
//! # The contract that changed, and why it was wrong
//!
//! Until part 20, [`InputSource::next_event`](crate::InputSource::next_event) returned
//! [`InputEvent`] and **dropped every non-`EV_KEY` record by contract**:
//!
//! > `EV_SYN` and every non-`EV_KEY` record are consumed and skipped internally, so a caller never
//! > sees one and cannot forget to filter them.
//!
//! That is a well-written sentence about a *keyboard* input layer, and it was the wrong contract for
//! an editor with a toolbar. `EV_REL` is defined in the same file, in the same vocabulary, and is
//! motion: **the mouse exists and the crate was discarding it.** A caller could not "forget to
//! filter" a pointer event, because a caller could not *obtain* one.
//!
//! The replacement is not "stop filtering". It is two layers, which is what the hardware already has:
//!
//! * [`Record`] -- one decoded 24-byte record, with no coalescing. Pure parsing, and the layer
//!   `event.rs`'s tests already pin.
//! * [`Event`] -- what the decoder accumulates a whole `EV_SYN` frame into. One pointer move, one
//!   button transition, or one keystroke.
//!
//! # Why the frame, and why keys are exempt
//!
//! **A relative pointer's two axes arrive as two records.** `REL_X` then `REL_Y` is one movement, and
//! emitting an event per record would make a diagonal drag arrive as a staircase of horizontal then
//! vertical hops — which on a 125 Hz mouse is a visibly jagged caret.
//!
//! **So motion coalesces, at the frame boundary the kernel marks with `EV_SYN`.** Buttons are the
//! exception in *timing* only: a button record is emitted immediately, carrying the position
//! accumulated so far. That is correct because evdev orders a frame as motion-then-button
//! (`REL_X, REL_Y, BTN_LEFT, SYN`), so by the time the button is seen the position already includes
//! the movement that preceded it.
//!
//! **Keys are emitted per record, never per frame**, and that is not a simplification — it is a
//! requirement. [`ScriptedInputSource::from_events_bare`] encodes a stream with *no* `EV_SYN` between
//! records, and there is a gate asserting it produces the same commands as the spaced version. A
//! per-frame coalescer would take five bare keystrokes and emit one. **So the coalescing is
//! restricted to records that cannot stand alone**, and `EV_REL` is the only such thing.

use crate::event::{InputEvent, EV_ABS, EV_KEY, EV_MSC, EV_REL, EV_SYN, RECORD_BYTES};

/// `REL_X`. Horizontal relative motion.
pub const REL_X: u16 = 0x00;
/// `REL_Y`. Vertical relative motion.
pub const REL_Y: u16 = 0x01;
/// `REL_HWHEEL`. Horizontal wheel.
pub const REL_HWHEEL: u16 = 0x06;
/// `REL_WHEEL`. Vertical wheel, positive away from the user.
pub const REL_WHEEL: u16 = 0x08;

/// `BTN_LEFT`.
pub const BTN_LEFT: u16 = 0x110;
/// `BTN_RIGHT`.
pub const BTN_RIGHT: u16 = 0x111;
/// `BTN_MIDDLE`.
pub const BTN_MIDDLE: u16 = 0x112;
/// `BTN_SIDE`, which is what most mice call the wheel click.
pub const BTN_SIDE: u16 = 0x113;

/// A pointer button, numbered as the kernel numbers it.
///
/// **Four variants and an `Other(u16)`.** The `Other` arm is what makes this total: a mouse with a
/// thumb button reports `BTN_5`, and an enum without it would either drop the button or panic. It is
/// also what lets the session say "I do not know what button 8 is" instead of guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Button {
    /// Left. The primary button.
    Left,
    /// Right. The secondary button.
    Right,
    /// Middle.
    Middle,
    /// Anything else.
    Other(u16),
}

impl Button {
    /// The kernel's code for this button.
    pub const fn code(self) -> u16 {
        match self {
            Button::Left => BTN_LEFT,
            Button::Right => BTN_RIGHT,
            Button::Middle => BTN_MIDDLE,
            Button::Other(c) => c,
        }
    }

    /// This button from the kernel's code, or [`None`] for a code that is not a button at all.
    ///
    /// **`>= BTN_LEFT` rather than a list.** The kernel numbers mouse buttons from `0x110` upward
    /// contiguously, and a match on four codes would call `BTN_9` "not a button" — which would then
    /// be dispatched as a *keystroke*, putting button 9's code through the keymap. **The bound is the
    /// honest one**: everything above the keyboard range is a button.
    pub const fn from_code(code: u16) -> Option<Self> {
        match code {
            BTN_LEFT => Some(Button::Left),
            BTN_RIGHT => Some(Button::Right),
            BTN_MIDDLE => Some(Button::Middle),
            c if c >= BTN_LEFT => Some(Button::Other(c)),
            _ => None,
        }
    }
}

/// One decoded record, before any coalescing.
///
/// # Why this is a separate type from [`InputEvent`]
///
/// `InputEvent` is the keyboard's record: `kind`, `code`, `value`. Motion has no meaningful `code`
/// in the same sense and no `value` in the same sense — `value` is a *delta*, not a state — so
/// reusing the triple would mean every consumer re-deriving what `kind` and `code` mean. **A record
/// is not an event until it has been coalesced with its neighbours**, and that is the distinction
/// this type exists to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Record {
    /// A keyboard key, still carrying its autorepeat value.
    Key(InputEvent),
    /// A relative axis moved by `delta`.
    Motion {
        /// `REL_X`, `REL_Y`, `REL_HHEEL`, ...
        axis: u16,
        /// Signed distance. **Negative is left and up.**
        delta: i32,
    },
    /// A button went down or came up.
    Button {
        /// Which.
        button: Button,
        /// `true` for the press.
        pressed: bool,
    },
    /// `EV_SYN`, `EV_MSC`, and anything this crate does not act on.
    Noise,
}

impl Record {
    /// Whether this record is worth coalescing — that is, whether it can only be understood next to
    /// its neighbours.
    ///
    /// **`Key` and `Button` are `false`, and that is the whole of the keystroke guarantee.** See the
    /// module docs: a per-frame coalescer would drop four of five bare keystrokes.
    pub const fn coalesces(&self) -> bool {
        matches!(self, Record::Motion { .. })
    }
}

/// Decode one 24-byte record, keeping everything this crate acts on.
///
/// # Why `decode` is not called here
///
/// [`crate::decode`] filters to `EV_KEY` and returns `None` for everything else, **and it must keep
/// doing that**: three gates and the whole keymap are stated against that contract, and one of them
/// asserts that a `BTN_LEFT` record decodes to a *key* — which is true and is exactly the confusion
/// this type is here to resolve. So this function parses the fields itself rather than going through
/// a filter that would throw away the half it wants.
///
/// # Panics
///
/// If `bytes` is shorter than [`RECORD_BYTES`]. Callers slice; they do not check.
pub fn decode_record(bytes: &[u8]) -> Record {
    let le = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
    let le32 =
        |at: usize| i32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    let kind = le(16);
    let code = le(18);
    let value = le32(20);
    match kind {
        EV_KEY => match Button::from_code(code) {
            // `value` is 0/1/2 here, and autorepeat does not apply to a button: 2 would be a
            // third state a button does not have. Treated as pressed, which is what a driver that
            // emits 2 for a button means.
            Some(button) => Record::Button {
                button,
                pressed: value != 0,
            },
            None => Record::Key(InputEvent { kind, code, value }),
        },
        EV_REL => Record::Motion {
            axis: code,
            delta: value,
        },
        // **`EV_ABS` is dropped rather than guessed at.** A tablet's absolute position is in a
        // device's own range, which `open` would have to scale to the panel, and a wrong scale is a
        // caret in the wrong place with no way to tell why. `Record::Noise` and a note here is the
        // honest answer: the record is not lost, it is not understood.
        EV_ABS | EV_MSC | EV_SYN | _ => Record::Noise,
    }
}

/// Build one record's bytes, for a fixture or a round-trip assertion.
///
/// **The mirror of [`decode_record`]**, and separate from
/// [`crate::event::encode`] because that one writes an [`InputEvent`] and an `InputEvent` cannot
/// express `EV_REL`. A second encoder is cheaper than widening the first one's type to something
/// that is not a keyboard record.
pub fn encode_record(kind: u16, code: u16, value: i32) -> [u8; RECORD_BYTES] {
    let mut out = [0u8; RECORD_BYTES];
    out[16..18].copy_from_slice(&kind.to_le_bytes());
    out[18..20].copy_from_slice(&code.to_le_bytes());
    out[20..24].copy_from_slice(&value.to_le_bytes());
    out
}

/// One delivered event.
///
/// **A pointer event carries its position**, and that is the decision everything else follows from.
/// The alternative is to deliver a bare delta and make every consumer keep a position — which means
/// the session, the chrome and the hit test would each hold a copy and they would disagree, and a hit
/// test that disagrees with the frame about where the pointer is will eventually press the wrong
/// button. **One position, computed once, in one place.**
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// A keystroke.
    Key(InputEvent),
    /// The pointer moved to an absolute position.
    Motion {
        /// Where, in panel pixels.
        x: i32,
        y: i32,
    },
    /// A button changed state.
    Button {
        /// Which.
        button: Button,
        /// `true` for the press.
        pressed: bool,
        /// Where it happened.
        x: i32,
        y: i32,
    },
    /// A wheel notch.
    Wheel {
        /// Notches, positive away from the user.
        dy: i32,
        /// Where the pointer was.
        x: i32,
        y: i32,
    },
}

impl Event {
    /// This event's position, if it has one.
    ///
    /// **A key has no position**, which is the whole distinction between this and
    /// [`Event::Key`]: a key goes to the document and a pointer goes to a widget, and asking for the
    /// position of a keystroke is the first sign of code that is routing them together.
    pub const fn position(&self) -> Option<(i32, i32)> {
        match self {
            Event::Key(_) => None,
            Event::Motion { x, y } => Some((*x, *y)),
            Event::Button { x, y, .. } => Some((*x, *y)),
            Event::Wheel { x, y, .. } => Some((*x, *y)),
        }
    }

    /// Whether this is a pointer event, i.e. something that is routed to a widget rather than to the
    /// document.
    pub const fn is_pointer(&self) -> bool {
        !matches!(self, Event::Key(_))
    }
}

/// The pointer's state between frames, accumulated out of relative records.
///
/// # Why the position is clamped rather than left to go negative
///
/// `REL_X` of -5 from `x = 2` is `x = -3`, and `-3` is a real event from a real mouse at the left
/// edge of the desk. **It is not a bug in the mouse and not a bug in the decoder**, and the honest
/// thing is to clamp at zero rather than to wrap — a wrapped position of 4294967293 hits test against
/// every widget and lands somewhere absurd. The cost is that a pointer cannot leave the panel, which
/// on a bare scanout with no window manager is also the truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pointer {
    /// Panel x.
    pub x: i32,
    /// Panel y.
    pub y: i32,
    /// Buttons currently down, as a bitmask of [`Button::code`].
    pub buttons: u32,
}

impl Pointer {
    /// A pointer at `(0, 0)` with nothing down.
    ///
    /// **`(0, 0)` rather than "off the panel", and this is deliberate.** A pointer that starts
    /// nowhere would have to be represented as an `Option`, and every consumer would carry the
    /// `Option`. Starting at the origin means the first `REL_X` moves it right by its delta, which is
    /// what a mouse that was already centred on the desk does. The cost is that before the first
    /// event the pointer sits on the top-left widget, and no widget reads hover until it has been
    /// told a pointer moved.
    pub const fn new() -> Self {
        Self {
            x: 0,
            y: 0,
            buttons: 0,
        }
    }

    /// Whether `button` is down.
    pub const fn is_down(&self, button: Button) -> bool {
        let bit = 1u32 << (button.code() & 31);
        self.buttons & bit != 0
    }

    /// Record a button going down or coming up.
    ///
    /// **`pub(crate)`, not public, and that is the point.** The button mask is the decoder's business
    /// — it answers "which buttons are down" for a source that wants to report drag state — but a
    /// caller cannot set it, because a caller that synthesises a press without a `BTN_*` record
    /// would be inventing hardware. **The bitmask is derived from records, never assigned.**
    pub(crate) fn press(&mut self, button: Button, down: bool) {
        let bit = 1u32 << (button.code() & 31);
        if down {
            self.buttons |= bit;
        } else {
            self.buttons &= !bit;
        }
    }
}

/// The accumulated pointer motion since the last frame boundary.
///
/// **Separate from [`Pointer`] because it is transient.** `Pointer` is where the pointer *is*; this
/// is how far it moved since the kernel last said "that frame is done". They are different lifetimes
/// and putting them in one struct would mean clearing the position every frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Frame {
    /// Accumulated x.
    pub dx: i32,
    /// Accumulated y.
    pub dy: i32,
    /// Accumulated vertical wheel notches.
    pub wheel: i32,
    /// Whether anything moved at all, so the caller can skip a no-op frame.
    pub moved: bool,
}

impl Frame {
    /// Fold one motion record in.
    ///
    /// # `moved` is set by the axes, not by the record
    ///
    /// **This is a CORRECTION, and the gate `a_horizontal_wheel_notch_is_dropped_not_faked` is what
    /// found it.** The first version set `moved = true` unconditionally, on the reasoning that "a
    /// record arrived, so something happened". A frame carrying only `REL_HWHEEL` therefore reported
    /// `moved` with `dx == dy == wheel == 0`, and the decoder emitted `Event::Motion { x, y }` at the
    /// pointer's *current* position — which is a no-op event that looks like motion, and which made
    /// the comment's promise ("a horizontal notch produces nothing") false in the only way that
    /// matters: the code said one thing in prose and did another.
    ///
    /// **The fix is that an axis nobody accumulates into does not count as movement.** A frame with
    /// nothing in it is a frame with nothing in it, and reporting it as movement is how a no-op
    /// becomes an event.
    pub fn fold(&mut self, axis: u16, delta: i32) {
        match axis {
            REL_X => {
                self.dx += delta;
                self.moved = true;
            }
            REL_Y => {
                self.dy += delta;
                self.moved = true;
            }
            REL_WHEEL => {
                self.wheel += delta;
                self.moved = true;
            }
            // **`REL_HWHEEL` is dropped, on purpose.** The panel has no horizontal scrollbar --
            // the reference's chrome has none either -- so a horizontal wheel has nowhere to go.
            // Turning it into zoom is a lie about what the user asked for, and **"the mouse does
            // something surprising" is worse than "the mouse does nothing"**, because the user cannot
            // predict it. So it is dropped, and it does not even set `moved`.
            _ => {}
        }
    }
}
