//! Evdev input, `code`-based keymap, and scripted event injection.
//!
//! Keymap matching is on `code`, never `key`: `key` is layout-dependent, `code` is not.
//! (H2's rule, and it is the right one.)
//!
//! `InputSource` is a trait with two implementations: `EvdevSource`
//! (`/dev/input/event*`) and `ScriptedInputSource` (a raw `input_event` byte stream, so
//! the decode path is byte-identical). PROJECT.md §2.7 measured that `/dev/input` is
//! `EACCES` on this host — not in the `input` group — so the scripted path is not a
//! convenience, it is the only way to run the decode tests here.
//!
//! Lands in Phase 8. See PROJECT.md §5 Phase 8 and §2.7.

pub use holonomy_jail::PHASE_0_PLACEHOLDER;
