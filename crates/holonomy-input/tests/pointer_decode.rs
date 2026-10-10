//! **The pointer path: records become frames become events.** 9 tests.
//!
//! # The contract that changed
//!
//! Until part 20, [`InputSource::next_event`] dropped every non-`EV_KEY` record **by contract**:
//!
//! > `EV_SYN` and every non-`EV_KEY` record are consumed and skipped internally, so a caller never
//! > sees one and cannot forget to filter them.
//!
//! The replacement is two layers: [`Record`] for one 24-byte record, [`Event`] for what a whole
//! `EV_SYN` frame accumulates to. These tests are about the join between them, because **that join is
//! where the two kinds of requirement collide**:
//!
//! * **Keys must survive a stream with no `EV_SYN` in it.** There is a gate in `source.rs` asserting
//!   `from_events_bare` and `from_events` produce identical command streams, and a per-frame
//!   coalescer would take five bare keystrokes and emit one.
//! * **Motion must coalesce.** A mouse's `REL_X` and `REL_Y` are one movement, and emitting per record
//!   makes a diagonal drag a staircase.
//!
//! So the rule is one sentence — **motion accumulates to the frame boundary; everything else is
//! emitted as it arrives** — and each of these tests is a case of that sentence.
//!
//! | what it proves | test |
//! | --- | --- |
//! | five bare keystrokes are five events | [`five_bare_keystrokes_are_five_events`] |
//! | the two axes of a diagonal are one motion | [`two_axes_are_one_movement`] |
//! | a frame's buttons do not swallow its motion | [`a_button_and_motion_in_one_frame_are_two_events`] |
//! | a button carries a *current* position | [`a_click_carries_the_movement_that_preceded_it`] |
//! | wheel arrives as wheel, not as motion | [`a_wheel_notch_is_a_wheel_event`] |
//! | horizontal wheel is deliberately dropped | [`a_horizontal_wheel_notch_is_dropped_not_faked`] |
//! | a mouse button is not a keystroke | [`a_button_code_is_not_a_key_code`] |
//! | the panel edge is a clamp, not a wrap | [`motion_past_the_left_edge_clamps_at_zero`] |
//! | and the whole-record path still agrees | [`decode_still_returns_only_ev_key`] |

use holonomy_input::event::{encode, InputEvent, EV_KEY, EV_REL, EV_SYN, RECORD_BYTES};
use holonomy_input::pointer::{
    decode_record, encode_record, Button, Event, Record, BTN_LEFT, BTN_RIGHT, REL_HWHEEL,
    REL_WHEEL, REL_X, REL_Y,
};
use holonomy_input::{decode, InputSource, RecordDecoder, ScriptedInputSource};

/// **One frame: every record, then a single `EV_SYN` at the end.**
///
/// # Why this helper exists, and why the obvious one is wrong
///
/// The first version of this file put an `EV_SYN` after *every* record, on the theory that devices
/// emit a sync per event. **They do not — a frame is several records and one sync**, and a mouse's
/// click is `REL_X, REL_Y, BTN_LEFT, SYN`. That fixture said `REL_X 3` and `REL_Y 4` were two frames,
/// so the decoder correctly emitted two motions and the test failed with
/// `left: [Motion{10,0}, Motion{10,5}, …]`.
///
/// **The failure was the fixture's, and the decoder was right** — which is the useful thing about it:
/// a gate built on a wrong premise tells you which of the two is wrong, and here the answer was the
/// gate. `per_record` is kept below for the one case that is genuinely per-record: keystrokes, which
/// a real keyboard does sync individually.
fn framed(records: &[(u16, u16, i32)]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(k, c, v) in records {
        out.extend_from_slice(&encode_record(k, c, v));
    }
    out.extend_from_slice(&encode_record(EV_SYN, 0, 0));
    out
}

/// One `EV_SYN` after each record, which is what a keyboard does and a mouse does not.
fn per_record(records: &[(u16, u16, i32)]) -> Vec<u8> {
    let mut out = Vec::new();
    for &(k, c, v) in records {
        out.extend_from_slice(&encode_record(k, c, v));
        out.extend_from_slice(&encode_record(EV_SYN, 0, 0));
    }
    out
}

/// Everything the decoder will give, until it runs dry.
fn drain(d: &mut RecordDecoder) -> Vec<Event> {
    let mut v = Vec::new();
    while let Some(e) = d.next_event() {
        v.push(e);
    }
    v
}

/// **Five keystrokes with no `EV_SYN` between them are five events.**
///
/// **This is the test that forbids a per-frame coalescer**, and it is here rather than in
/// `source.rs` because that file's gate asserts it at the *source* level while this asserts it at the
/// *decoder* level, which is where a future edit would break it. A coalescer keyed on `EV_SYN` would
/// see no boundary at all here and return one event for the whole buffer.
#[test]
fn five_bare_keystrokes_are_five_events() {
    let bytes: Vec<u8> = [30u16, 48, 46, 32, 16]
        .iter()
        .flat_map(|c| encode(InputEvent::press(*c)))
        .collect();
    let got = drain(&mut RecordDecoder::from_bytes(&bytes));
    assert_eq!(got.len(), 5, "got {got:?}");
    for (e, c) in got.iter().zip([30u16, 48, 46, 32, 16]) {
        assert_eq!(*e, Event::Key(InputEvent::press(c)));
    }
}

/// **A diagonal movement is one motion event, not two.**
///
/// `REL_X 3` then `REL_Y 4` in one frame is a single diagonal step. Emitting two motions would give
/// the session two absolute positions — the intermediate one — and a caret dragged along a diagonal
/// would step horizontally and then vertically instead of arriving.
#[test]
fn two_axes_are_one_movement() {
    let mut d = RecordDecoder::from_bytes(&framed(&[(EV_REL, REL_X, 3), (EV_REL, REL_Y, 4)]));
    let got = drain(&mut d);
    assert_eq!(
        got,
        vec![Event::Motion { x: 3, y: 4 }],
        "one frame, one event"
    );
}

/// **A frame with a button *and* motion yields two events, button first.**
///
/// **A real mouse frame is exactly this**: `REL_X, REL_Y, BTN_LEFT, SYN`. The button must come first
/// because a caret placed from the click should not be corrected by the motion that followed it — and
/// because a session that applies them in order ends in the right place either way, which is asserted
/// by `the_last_event_wins` below rather than assumed here.
#[test]
fn a_button_and_motion_in_one_frame_are_two_events() {
    let mut d = RecordDecoder::from_bytes(&framed(&[
        (EV_REL, REL_X, 10),
        (EV_REL, REL_Y, 5),
        (EV_KEY, BTN_LEFT, 1),
    ]));
    let got = drain(&mut d);
    assert_eq!(
        got,
        vec![
            Event::Button {
                button: Button::Left,
                pressed: true,
                x: 10,
                y: 5
            },
            Event::Motion { x: 10, y: 5 },
        ],
        "the button is emitted as it arrives and the motion at the boundary"
    );
    // **And the last event wins**, which is the property that makes the order above safe rather than
    // merely documented. The motion carried the same position, so a session that tracks the newest
    // event has the right one whichever it applied last.
    let (x, y) = got.last().and_then(Event::position).expect("a position");
    assert_eq!((x, y), (10, 5));
}

/// **A click's position includes the movement in the same frame.**
///
/// **This is why buttons are not held to the frame boundary.** A press emitted at the boundary would
/// carry the position from *before* the frame, so a click placed from it would land one frame's
/// movement behind the pointer — about 8 ms and 3 pixels on a fast flick, which is exactly the amount
/// by which "the caret is where I clicked" would be almost true.
#[test]
fn a_click_carries_the_movement_that_preceded_it() {
    let mut d = RecordDecoder::from_bytes(&framed(&[
        (EV_REL, REL_X, 40),
        (EV_REL, REL_Y, 20),
        (EV_KEY, BTN_LEFT, 1),
    ]));
    let got = drain(&mut d);
    let press = got
        .iter()
        .find(|e| matches!(e, Event::Button { pressed: true, .. }))
        .expect("a press");
    assert_eq!(press.position(), Some((40, 20)), "not (0, 0)");
}

/// **A wheel notch is a wheel event.**
///
/// **Not a motion event, and the distinction is the whole point of having a `Wheel` variant.** A wheel
/// notch carries no position change, so a session that treated it as motion would scroll by zero and
/// report `pointer_events` rising with nothing happening.
#[test]
fn a_wheel_notch_is_a_wheel_event() {
    let mut d = RecordDecoder::from_bytes(&framed(&[(EV_REL, REL_WHEEL, -1)]));
    assert_eq!(drain(&mut d), vec![Event::Wheel { dy: -1, x: 0, y: 0 }]);
    let mut d = RecordDecoder::from_bytes(&framed(&[(EV_REL, REL_WHEEL, 3)]));
    assert_eq!(drain(&mut d), vec![Event::Wheel { dy: 3, x: 0, y: 0 }]);
}

/// **A horizontal wheel notch is dropped, not turned into anything.**
///
/// **The alternative considered and rejected:** scaling it into zoom. A horizontal wheel means "scroll
/// sideways", and there is nothing sideways to scroll — the panel's chrome has no horizontal
/// scrollbar and the reference's does not either. Scaling it into zoom would make the mouse appear to
/// work while doing something the user did not ask for, and **"the mouse does something surprising" is
/// worse than "the mouse does nothing"**, because the user cannot predict it.
///
/// The assertion is that *no event at all* comes out, which is a stronger statement than "not a wheel".
#[test]
fn a_horizontal_wheel_notch_is_dropped_not_faked() {
    let mut d = RecordDecoder::from_bytes(&framed(&[(EV_REL, REL_HWHEEL, 1)]));
    assert!(
        drain(&mut d).is_empty(),
        "a horizontal notch must produce nothing. If this ever produces motion or zoom it is \\
         guessing at what the user asked for."
    );
}

/// **A `BTN_LEFT` record is a button, and not a keystroke with a button's code.**
///
/// **`decode` still returns it as a key, and that is deliberate and asserted.** `decode` is the
/// keymap's parser and three gates are stated against its contract; changing it would break the
/// keymap for a fix nobody asked for. `decode_record` is the one that resolves the ambiguity, and this
/// test is the one that says which of them is allowed to be confused.
#[test]
fn a_button_code_is_not_a_key_code() {
    let bytes = encode_record(EV_KEY, BTN_LEFT, 1);
    assert_eq!(
        decode_record(&bytes),
        Record::Button {
            button: Button::Left,
            pressed: true
        }
    );
    // A keycode above the button range is still a key.
    assert!(matches!(
        decode_record(&encode_record(EV_KEY, 30, 1)),
        Record::Key(_)
    ));
    // **And `decode`, unchanged, still calls a button a key.** Recorded so the day somebody "fixes"
    // it, this test fails and says what breaks.
    assert_eq!(
        decode(&bytes).map(|k| k.code),
        Some(BTN_LEFT),
        "decode() keeps returning every EV_KEY. It is the keymap's parser and three gates are \\
         stated against it; decode_record is the one that separates buttons."
    );
    // **X's numbering, which is the trap on the other side.** X button 1 is evdev's BTN_LEFT, and the
    // offset is arithmetic in `x11key::x_button` rather than a table; this pins the result.
    assert_eq!(Button::from_code(BTN_LEFT), Some(Button::Left));
    assert_eq!(Button::from_code(BTN_RIGHT), Some(Button::Right));
    assert_eq!(Button::from_code(0x200), Some(Button::Other(0x200)));
    assert_eq!(Button::from_code(30), None, "KEY_A is not a button");
}

/// **Motion past the left edge clamps at zero, and does not wrap.**
///
/// **A wrapped position of `u32::MAX` hits every widget.** `x = -3` is a real event from a real mouse
/// at the edge of the desk; the alternatives are to clamp (chosen), to wrap (a caret in the far corner)
/// or to report it as out of range (a source that had to be asked whether it is).
#[test]
fn motion_past_the_left_edge_clamps_at_zero() {
    let mut d = RecordDecoder::from_bytes(&framed(&[(EV_REL, REL_X, -5), (EV_REL, REL_Y, -9)]));
    let got = drain(&mut d);
    let (x, y) = got.last().and_then(Event::position).expect("a position");
    assert_eq!((x, y), (0, 0), "clamped, not wrapped");
}

/// **A release carries the pointer's position and clears the button mask.**
///
/// The release's position is what the X11 crate used to discard (`ButtonRelease` had no `event_x`),
/// and it is what lets a press-then-move-then-release be three events rather than two and a guess.
#[test]
fn a_release_carries_the_pointer_position() {
    // **Three frames**, as three things a person does: move, press, move-and-let-go.
    let mut d = RecordDecoder::from_bytes(&per_record(&[
        (EV_REL, REL_X, 7),
        (EV_KEY, BTN_LEFT, 1),
        (EV_REL, REL_X, 11),
    ]));
    d.push(&encode_record(EV_KEY, BTN_LEFT, 0));
    d.push(&encode_record(EV_SYN, 0, 0));
    let got = drain(&mut d);
    let release = got
        .iter()
        .find(|e| matches!(e, Event::Button { pressed: false, .. }))
        .expect("a release");
    assert_eq!(
        release.position(),
        Some((18, 0)),
        "the release is at 7 + 11, not at 7 and not at 0"
    );
    assert!(!d.pointer().is_down(Button::Left), "and the mask is clear");
    let _ = RECORD_BYTES;
}

/// **A key has no position, and that is the distinction that keeps the two apart.**
///
/// `Event::position` returning `None` for a key is what stops the session's pointer handler from being
/// handed a keystroke, and it is the reason `run_with` can branch on `is_pointer` rather than on a
/// flag.
#[test]
fn a_key_has_no_position_and_a_pointer_event_does() {
    assert_eq!(Event::Key(InputEvent::press(30)).position(), None);
    assert!(!Event::Key(InputEvent::press(30)).is_pointer());
    let p = Event::Motion { x: 1, y: 2 };
    assert_eq!(p.position(), Some((1, 2)));
    assert!(p.is_pointer());
}

/// **The scripted source delivers pointer events through the same decoder a device does.**
///
/// **`from_records` rather than a pointer-specific builder**, because a fixture that encoded pointer
/// events its own way would be testing a second path. The bytes go in and come out of
/// [`RecordDecoder`], which is what `EvdevSource` uses.
#[test]
fn the_scripted_source_delivers_pointer_events_the_same_way_a_device_does() {
    // **A press and a release in one frame, deliberately.** A real click is two frames -- the user
    // takes longer than one sync to let go -- and the *interesting* property here is that putting them
    // in one frame does not lose either. Both are emitted as they arrive and the motion follows at
    // the boundary, in the order the records were written. **Three events from four records**, and
    // the four records are `REL_X, REL_Y, BTN_LEFT, BTN_LEFT`.
    let bytes = framed(&[
        (EV_REL, REL_X, 30),
        (EV_REL, REL_Y, 12),
        (EV_KEY, BTN_LEFT, 1),
        (EV_KEY, BTN_LEFT, 0),
    ]);
    let mut src = ScriptedInputSource::new(&bytes);
    let got: Vec<Event> = std::iter::from_fn(|| src.next_event().ok().flatten()).collect();
    assert_eq!(
        got,
        vec![
            Event::Button {
                button: Button::Left,
                pressed: true,
                x: 30,
                y: 12
            },
            Event::Button {
                button: Button::Left,
                pressed: false,
                x: 30,
                y: 12
            },
            Event::Motion { x: 30, y: 12 },
        ],
        "both buttons as they arrived, then the motion at the boundary"
    );
    assert!(src.is_drained());
    // The release, in its own frame.
    let bytes = framed(&[(EV_KEY, BTN_LEFT, 0)]);
    let mut src = ScriptedInputSource::new(&bytes);
    assert_eq!(
        src.next_event().ok().flatten(),
        Some(Event::Button {
            button: Button::Left,
            pressed: false,
            x: 0,
            y: 0
        }),
        "a source starts with the pointer at the origin, which is what makes the first REL_X move \
         it by its delta"
    );
}
