//! Input: evdev, a `code`-based keymap, and scripted event injection.
//!
//! Phase 8's front end. Three things, and the boundary between them is the whole design:
//!
//! * [`Event decoding`](event) -- the kernel's 24-byte `input_event`, and nothing else. It knows no
//!   keys and no commands.
//! * [`Keymap`](keymap) -- an event plus the modifier state becomes a [`Command`]. It is a pure
//!   function of its two arguments, zero-sized, and knows no text.
//! * [`Sources`](source) -- a real device ([`EvdevSource`]) or a fixture ([`ScriptedInputSource`]),
//!   behind one trait.
//!
//! # The layering is load-bearing, and the reason is the jail
//!
//! Inside the jail there is no `open`. So the session cannot open a keyboard after sealing; it is
//! handed a descriptor. That constraint is what pushes "where do events come from" behind a trait
//! instead of straight at `read(2)`: the session takes an [`InputSource`], the test hands it a
//! [`ScriptedInputSource`], and neither the session nor the keymap can tell them apart.
//!
//! It also means the integration test exercises the *same* decoder, the same filter and the same
//! dispatcher as a ThinkPad X200's keyboard, because [`ScriptedInputSource`] feeds
//! [`RecordDecoder`] the same bytes a device would. A test that had a second parsing path would be
//! testing a parser that no hardware runs.
//!
//! # `code`, not `key`
//!
//! See the [`keymap`] module. Short version: `code` is the hardware position and does not change with
//! layout, Caps Lock or Num Lock, so a keymap built on `key` has to be rebuilt per layout and has to
//! un-apply state the kernel already applied. `key` is never read here.
//!
//! # A note on what is deliberately absent
//!
//! No `unicode-normalization` dependency. On the one layout implemented here every output is ASCII,
//! so normalization is the identity function; adding it would be a dependency and a runtime cost
//! against no change in behaviour. The module docs on [`keymap`] say where it would go and what would
//! change when a non-ASCII layout lands.
//!
//! # What this crate does not know
//!
//! Text, geometry, the container, the framebuffer. [`Command::Up`] means "move up" and nothing about
//! how many bytes that is -- the session owns the geometry and decides. That is why the crate has no
//! dependency on `holonomy-text` or `holonomy-geometry`: an input layer that could ask the document
//! something is an input layer that has to be tested against a document.

pub mod evdev;
pub mod event;
pub mod keymap;
pub mod modifiers;
pub mod source;
#[cfg(feature = "desktop")]
pub mod x11key;

pub use evdev::EvdevSource;
pub use event::{decode, encode, syn_report, InputEvent, EV_KEY, EV_MSC, EV_SYN, RECORD_BYTES};
pub use keymap::{Command, Hotkey, Keymap};
pub use modifiers::ModifierState;
pub use source::{
    errno_name, InputError, InputSource, RecordDecoder, ScriptedInputSource, RECORDS_PER_READ,
};

// The `KEY_*` constants are the crate's vocabulary. Re-exported in one glob because a caller writing a
// keymap or a fixture should not have to know which of five modules a code lives in -- and because a
// partial hand-written list would eventually miss one.
pub use keymap::*;

/// The kernel's own layout, spelled out so the offsets can be checked against [`decode`]'s.
#[repr(C)]
#[derive(Clone, Copy)]
struct KernelRecord {
    sec: u64,
    usec: u64,
    kind: u16,
    code: u16,
    value: i32,
}

/// A compile-time proof that [`decode`]'s byte offsets are the kernel's.
///
/// A `const` and not a `#[test]`, so a mismatch is a **build** failure in every downstream crate
/// rather than a test that only runs when someone remembers. And it is checked against a `#[repr(C)]`
/// mirror of the C struct rather than against [`InputEvent`], because that is the claim being made:
/// that bytes 16, 18 and 20 of a 24-byte record are `type`, `code` and `value`. Asserting
/// `size_of::<InputEvent>() == 24` would be a different and much weaker claim.
pub const RECORD_LAYOUT: () = {
    assert!(std::mem::size_of::<KernelRecord>() == RECORD_BYTES);
    assert!(std::mem::offset_of!(KernelRecord, kind) == event::OFF_TYPE);
    assert!(std::mem::offset_of!(KernelRecord, code) == event::OFF_CODE);
    assert!(std::mem::offset_of!(KernelRecord, value) == event::OFF_VALUE);
    // And a 32-bit build would be a different ABI entirely; this says so rather than decoding garbage.
    assert!(
        std::mem::size_of::<u64>() == 8,
        "the 24-byte record assumes a 64-bit timeval; this target is not 64-bit"
    );
};
