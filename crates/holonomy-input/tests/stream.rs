//! A whole event stream, from bytes to commands.
//!
//! The unit tests in the crate check one piece at a time: a table entry, a modifier rule, a decoder.
//! This file checks the *seam*, which is where an integration test earns its keep -- and the seam is
//! the thing the jail makes non-negotiable, because inside the jail there is no `open`, so the session
//! cannot obtain a keyboard after sealing and the scripted source is the only way to drive it.
//!
//! # Why the fixture is built from `InputEvent` and not from bytes
//!
//! The brief says to script "a deterministic `input_event` stream". Both halves of that matter:
//!
//! * `ScriptedInputSource::from_events` encodes to real 24-byte kernel records, so the fixture goes
//!   through [`RecordDecoder`] and [`decode`] exactly as a device would. Nothing is short-circuited.
//! * [`ScriptedInputSource::from_events_bare`] omits the `EV_SYN` separators a real device sends, and
//!   `with_and_without_syn_give_the_same_stream` below asserts the two produce identical command
//!   streams. A fixture that only ever omitted them would never notice a regression in the filter.
//!
//! So the scripted path is not a shortcut around the hardware path. It is the hardware path with a
//! different tail.
//!
//! # Determinism
//!
//! No timing, no threads, no device. Every test here is a fixed byte sequence in and a fixed
//! `Vec<Command>` out, so a failure is reproducible and a diff is meaningful.

use holonomy_input::*;

/// Drain a source into commands, folding modifier state as it goes.
fn run(src: &mut dyn InputSource) -> Vec<Command> {
    let km = Keymap::us();
    let mut mods = ModifierState::new();
    let mut out = Vec::new();
    while let Some(ev) = src.next_event().expect("scripted stream never errors") {
        if let Some(cmd) = km.dispatch_into(ev, &mut mods) {
            out.push(cmd);
        }
    }
    out
}

/// The same, from a bare event list with no `EV_SYN`.
fn run_bare(events: &[InputEvent]) -> Vec<Command> {
    let mut src = ScriptedInputSource::from_events_bare(events);
    run(&mut src)
}

/// Press/release a key.
fn tap(code: u16) -> Vec<InputEvent> {
    vec![InputEvent::press(code), InputEvent::release(code)]
}

/// The `(code, needs_shift)` pair that produces `c`.
///
/// A search rather than a reverse table, because a reverse table would be a second copy of the
/// layout to keep in sync -- and the whole point of this file is that there is exactly one.
fn find_key(c: char) -> (u16, bool) {
    let km = Keymap::us();
    let up = ModifierState::new();
    let mut shifted = ModifierState::new();
    shifted.update(KEY_LEFTSHIFT, 1);
    for code in 0..=127u16 {
        if km.text_for(code, &up) == Some(c) {
            return (code, false);
        }
        if km.text_for(code, &shifted) == Some(c) {
            return (code, true);
        }
    }
    panic!("no key produces {c:?}");
}

/// Type a string as a real user would: Shift where the character needs it, down and up per key.
///
/// Shift is only re-pressed when the next character needs it, so `type_str("Hello, World!")` emits
/// three Shift presses, not one per character. That is what a keyboard does, and it is what makes the
/// count in [`shift_covers_only_what_needs_it`] mean something.
fn type_str(s: &str) -> Vec<InputEvent> {
    let mut events = Vec::new();
    let mut shift_down = false;
    for c in s.chars() {
        let (code, needs_shift) = find_key(c);
        if needs_shift && !shift_down {
            events.push(InputEvent::press(KEY_LEFTSHIFT));
            shift_down = true;
        } else if !needs_shift && shift_down {
            events.push(InputEvent::release(KEY_LEFTSHIFT));
            shift_down = false;
        }
        events.extend(tap(code));
    }
    if shift_down {
        events.push(InputEvent::release(KEY_LEFTSHIFT));
    }
    events
}

/// The text of a command stream, ignoring anything that is not an `Insert`.
fn typed(cmds: &[Command]) -> String {
    cmds.iter()
        .filter_map(|c| match c {
            Command::Insert(c) => Some(*c),
            _ => None,
        })
        .collect()
}

/// A whole sentence types correctly, one key at a time.
#[test]
fn a_sentence_comes_out_as_typed() {
    let events = type_str("The quick brown fox.");
    let cmds = run_bare(&events);
    assert_eq!(typed(&cmds), "The quick brown fox.");
}

/// The same, through the `EV_SYN`-decorated source a device would produce.
#[test]
fn syn_separators_do_not_change_a_single_command() {
    let events = type_str("The quick brown fox.");
    let mut with_syn = ScriptedInputSource::from_events(&events);
    let mut bare = ScriptedInputSource::from_events_bare(&events);
    assert_eq!(run(&mut with_syn), run(&mut bare));
}

/// Shift covers only the characters that need it.
#[test]
fn shift_covers_only_what_needs_it() {
    let events = type_str("Hello, World!");
    let mut src = ScriptedInputSource::from_events(&events);
    let cmds = run(&mut src);
    assert_eq!(typed(&cmds), "Hello, World!");

    // "H" and "W" need shift; the "!" does too but comes after a non-letter.
    let shifts_up = events
        .iter()
        .filter(|e| e.code == KEY_LEFTSHIFT && e.value == 0)
        .count();
    assert_eq!(
        shifts_up, 3,
        "H, W and ! each need their own Shift press; 4 would mean a shift leaked"
    );
}

/// A hotkey typed as two keys, exactly as a user does it, fires once.
#[test]
fn ctrl_hotkeys_from_a_real_two_key_press() {
    for (code, want) in [
        (KEY_Q, Hotkey::Quit),
        (KEY_Z, Hotkey::Undo),
        (KEY_Y, Hotkey::Redo),
        (KEY_S, Hotkey::Save),
    ] {
        let mut events = vec![InputEvent::press(KEY_LEFTCTRL)];
        events.extend(tap(code));
        events.push(InputEvent::release(KEY_LEFTCTRL));

        let mut src = ScriptedInputSource::from_events(&events);
        let cmds = run(&mut src);
        assert_eq!(cmds, vec![Command::Hotkey(want)], "Ctrl+{code}");
        assert!(
            typed(&cmds).is_empty(),
            "Ctrl+{code} typed something as well as firing: {:?}",
            typed(&cmds)
        );
    }
}

/// Ctrl held across several keys does not leak into the text after it.
#[test]
fn releasing_ctrl_returns_to_letters() {
    let mut events = vec![InputEvent::press(KEY_LEFTCTRL)];
    events.extend(tap(KEY_Z));
    events.push(InputEvent::release(KEY_LEFTCTRL));
    events.extend(tap(KEY_A));
    events.extend(tap(KEY_B));

    let mut src = ScriptedInputSource::from_events(&events);
    let cmds = run(&mut src);
    assert_eq!(
        cmds,
        vec![
            Command::Hotkey(Hotkey::Undo),
            Command::Insert('a'),
            Command::Insert('b')
        ]
    );
}

/// Autorepeat, as a held key produces, and the interleaved releases.
#[test]
fn autorepeat_produces_the_expected_text() {
    let mut events = vec![InputEvent::press(KEY_X)];
    for _ in 0..4 {
        events.push(InputEvent::repeat(KEY_X));
    }
    events.push(InputEvent::release(KEY_X));

    let mut src = ScriptedInputSource::from_events(&events);
    assert_eq!(typed(&run(&mut src)), "xxxxx");
}

/// Autorepeat of a *modifier* must not resurrect it, which is the bug the `value == 1` rule prevents.
#[test]
fn autorepeat_of_a_modifier_does_not_resurrect_it() {
    // Shift down, 'A', then the kernel's autorepeats for Shift *while it is still held*, then Shift
    // up, then 'b'. The autorepeats must change nothing and 'b' must be lower-case.
    let mut events = vec![InputEvent::press(KEY_LEFTSHIFT)];
    events.extend(tap(KEY_A));
    for _ in 0..10 {
        events.push(InputEvent::repeat(KEY_LEFTSHIFT));
    }
    events.push(InputEvent::release(KEY_LEFTSHIFT));
    events.extend(tap(KEY_B));

    let mut src = ScriptedInputSource::from_events(&events);
    // 'A' from the held Shift, then 'b' lower-case because Shift was released. The autorepeats in
    // between changed nothing -- which is the point: they did not latch, and they did not prevent
    // the release from registering.
    assert_eq!(typed(&run(&mut src)), "Ab");
}

/// Autorepeats *after* a modifier is released must not bring it back, or every subsequent letter in
/// the session types with the modifier stuck on.
#[test]
fn autorepeat_after_a_modifier_release_does_not_resurrect_it() {
    let mut events = vec![InputEvent::press(KEY_LEFTSHIFT)];
    events.extend(tap(KEY_A));
    events.push(InputEvent::release(KEY_LEFTSHIFT));
    // A kernel that believes the key is still down keeps sending `2` forever.
    for _ in 0..100 {
        events.push(InputEvent::repeat(KEY_LEFTSHIFT));
    }
    events.extend(type_str("bcdef").iter().copied());

    let mut src = ScriptedInputSource::from_events(&events);
    assert_eq!(
        typed(&run(&mut src)),
        "Abcdef",
        "Shift came back to life and the rest of the word is upper-case"
    );
}

/// Navigation and editing keys produce their commands and no text.
#[test]
fn editing_keys_produce_no_text() {
    let mut events = Vec::new();
    for code in [
        KEY_LEFT,
        KEY_RIGHT,
        KEY_UP,
        KEY_DOWN,
        KEY_HOME,
        KEY_END,
        KEY_PAGEUP,
        KEY_PAGEDOWN,
        KEY_BACKSPACE,
        KEY_DELETE,
        KEY_ENTER,
        KEY_TAB,
        KEY_ESC,
    ] {
        events.extend(tap(code));
    }
    let mut src = ScriptedInputSource::from_events(&events);
    let cmds = run(&mut src);
    assert_eq!(
        typed(&cmds),
        "",
        "none of these is an Insert; Enter and Tab are their own commands, not characters"
    );
    assert_eq!(cmds[0], Command::Left);
    assert_eq!(cmds[3], Command::Down);
    assert_eq!(cmds[8], Command::Backspace);
    assert_eq!(cmds[9], Command::DeleteForward);
    assert_eq!(cmds[10], Command::Newline);
    assert_eq!(cmds[11], Command::Tab);
    assert_eq!(cmds[12], Command::Escape);
}

/// A multi-sentence paragraph, which is what the session gate types.
#[test]
fn a_paragraph_survives_the_pipeline() {
    const PARAGRAPH: &str = "\
        Holonomy keeps the text in leaves and the leaves in a ring. \
        The ring is committed, not written back, so a crash costs a session \
        and not a document. Undo is a second ring, and it is zeroized when \
        its arena wraps.";
    let events = type_str(PARAGRAPH);
    let mut src = ScriptedInputSource::from_events(&events);
    assert_eq!(typed(&run(&mut src)), PARAGRAPH);
}

/// The fixture really is 24-byte kernel records, not something this crate invented.
#[test]
fn the_fixture_is_the_kernels_record_size() {
    let events = [InputEvent::press(KEY_A), syn_report()];
    let bytes = events.iter().flat_map(|e| encode(*e)).collect::<Vec<u8>>();
    assert_eq!(bytes.len(), 2 * RECORD_BYTES);
    // type is at 16, code at 18, value at 20 -- checked by hand against the layout, not by calling
    // `decode`, which is the thing under test elsewhere.
    assert_eq!(u16::from_le_bytes([bytes[16], bytes[17]]), EV_KEY);
    assert_eq!(u16::from_le_bytes([bytes[18], bytes[19]]), KEY_A);
    assert_eq!(
        i32::from_le_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]),
        1
    );
}

/// Noise around the keys -- `EV_SYN`, `EV_MSC` -- changes nothing.
#[test]
fn device_noise_between_keys_is_invisible() {
    let events = type_str("abc");
    let mut noisy = Vec::new();
    for ev in &events {
        noisy.push(*ev);
        noisy.push(syn_report());
        noisy.push(InputEvent {
            kind: EV_MSC,
            code: 0x7003,
            value: 0x1234,
        });
        noisy.push(syn_report());
    }
    let mut src =
        ScriptedInputSource::new(&noisy.iter().flat_map(|e| encode(*e)).collect::<Vec<u8>>());
    assert_eq!(typed(&run(&mut src)), "abc");
}

/// The whole thing is a pure function of its bytes: same fixture, same commands, every time.
#[test]
fn the_pipeline_is_deterministic() {
    let events = type_str("The quick brown fox jumps over the lazy dog. 0123456789!");
    let first = run_bare(&events);
    for _ in 0..8 {
        assert_eq!(run_bare(&events), first);
    }
    // The sentence, the space, ten digits and the bang: 56 characters, every one an `Insert`.
    assert_eq!(first.len(), 56);
    assert!(first.iter().all(|c| matches!(c, Command::Insert(_))));
}
