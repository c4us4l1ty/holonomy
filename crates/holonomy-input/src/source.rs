//! Where events come from: the [`InputSource`] trait, the record decoder, and the scripted source.
//!
//! # The decoder is shared, so the test exercises the hardware path
//!
//! A real `/dev/input/eventN` delivers records; a test fixture is a byte array. If those took
//! different code, the integration test would be testing a parser that no keyboard ever runs. So both
//! go through [`RecordDecoder`], and [`crate::ScriptedInputSource`] is a [`InputSource`] that simply
//! hands its bytes to the same decoder.
//!
//! # Frames, not records: `next_event` returns an [`Event`]
//!
//! `next_event` used to return an `InputEvent` and drop every non-`EV_KEY` record by contract. That
//! contract is gone, because `EV_REL` is motion and **a mouse exists**; see the
//! [`pointer`](crate::pointer) module for what replaced it and what it costs.
//!
//! **The decoder here is where the coalescing happens**, and its rule is one sentence: **motion
//! accumulates until the kernel says the frame is over, and everything else is emitted as it
//! arrives.** Keys are emitted per record rather than per frame on purpose — a per-frame coalescer
//! would take five bare keystrokes and emit one, and there is a gate asserting that a fixture written
//! without `EV_SYN` produces the same commands as one written with them.
//!
//! # Partial reads are real
//!
//! `read` on an evdev node returns whole records, but nothing in the interface promises it will, and
//! a buffer sized as `N * 24` bytes that comes back `N * 24 - 13` is a hang, not an error: the
//! remaining 13 bytes have no event until more arrive, which may be never. So [`RecordDecoder`]
//! carries a partial tail across calls rather than assuming a record boundary lands on the read
//! boundary.
//!
//! [`InputSource`]: crate::InputSource

use crate::event::{InputEvent, RECORD_BYTES};
use crate::pointer::{decode_record, Event, Frame, Pointer, Record};

/// Why a source could not produce an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputError {
    /// `read` failed. Carries `errno`.
    ///
    /// `ENODEV` (19) is the one the session cares about: the keyboard was unplugged. It is reported
    /// as an error rather than as end-of-stream so the session can distinguish "device gone, reopen
    /// it" from "no more events", which is the difference between a hotplug and a closed stream.
    Read(i32),
    /// The source has no more bytes and will not get any. Not an error -- a scripted stream ends.
    Eof,
}

impl std::fmt::Display for InputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read(e) => write!(
                f,
                "read on the input device failed: {e} ({})",
                errno_name(*e)
            ),
            Self::Eof => write!(f, "the input stream ended"),
        }
    }
}

impl std::error::Error for InputError {}

/// The name of an `errno`, for a message that is worth reading.
pub fn errno_name(e: i32) -> &'static str {
    match e {
        1 => "EPERM",
        4 => "EINTR",
        5 => "EIO",
        9 => "EBADF",
        11 => "EAGAIN",
        19 => "ENODEV",
        21 => "EISDIR",
        22 => "EINVAL",
        25 => "ENOTTY",
        _ => "?",
    }
}

/// Somewhere events come from.
pub trait InputSource {
    /// The next event, or `None` when the source has nothing and never will.
    ///
    /// # What this returns, and why it changed
    ///
    /// **It used to return [`InputEvent`] and drop every non-`EV_KEY` record by contract**, on the
    /// reasoning that a caller "cannot forget to filter" a record it never sees. That reasoning is
    /// sound for a keyboard and wrong for an editor: `EV_REL` is defined in the same file, in the same
    /// vocabulary, and **is the mouse**. A contract that makes the mouse unreachable is not a
    /// convenience.
    ///
    /// So it returns [`Event`], which is keys *and* motion *and* buttons *and* the wheel. **The
    /// filtering did not disappear — it moved**, to the place where the information to filter by
    /// exists: `decode_record` turns a keyboard code into `Record::Key` and a `BTN_LEFT` into
    /// `Record::Button`, so `Record::Noise` really is only noise, and a caller that is handed an
    /// `Event` has been handed something it can act on.
    ///
    /// `None` means a *closed* stream; a device that went away is [`InputError::Read`] with
    /// `ENODEV`.
    fn next_event(&mut self) -> Result<Option<Event>, InputError>;

    /// A name for the status bar and for logs.
    fn describe(&self) -> &'static str;
}

/// Accumulates bytes and hands back whole `EV_KEY` records.
///
/// Byte-at-a-time would be absurd and reading-by-24 would drop a short tail, so this keeps the
/// remainder and tries again on the next push.
#[derive(Debug, Clone)]
pub struct RecordDecoder {
    /// Pending bytes, which may start mid-record.
    buf: Vec<u8>,
    /// Where the next complete record starts.
    cursor: usize,
    /// Where the pointer is, accumulated out of `REL_X`/`REL_Y`.
    pointer: Pointer,
    /// Motion since the last frame boundary.
    frame: Frame,
}

impl Default for RecordDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl RecordDecoder {
    /// An empty decoder.
    ///
    /// The initial capacity is [`Decode prefill`](RECORDS_PER_READ) records, which is what
    /// [`crate::EvdevSource`] reads at a time -- so the steady state allocates nothing.
    pub fn new() -> Self {
        Self {
            buf: Vec::with_capacity(RECORDS_PER_READ * RECORD_BYTES),
            cursor: 0,
            pointer: Pointer::new(),
            frame: Frame::default(),
        }
    }

    /// A decoder over an existing slice. The slice is copied, so the caller may drop it.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let mut d = Self::new();
        d.push(bytes);
        d
    }

    /// Add bytes, keeping any partial record for the next call.
    pub fn push(&mut self, bytes: &[u8]) {
        // Reclaim the consumed prefix first, so a long stream does not grow without bound. `copy_within`
        // rather than `drain(..n)`, which would be a memmove per event.
        if self.cursor > 0 {
            let rest = self.buf.len() - self.cursor;
            if rest > 0 {
                self.buf.copy_within(self.cursor.., 0);
            }
            self.buf.truncate(rest);
            self.cursor = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// The next event, coalescing motion to the frame boundary.
    ///
    /// # The order, and why it is that order
    ///
    /// Records are scanned in order, and the first one that *is* an event is returned:
    ///
    /// 1. **A key.** Emitted immediately. Never held for the frame, because a per-frame rule would
    ///    drop keystrokes from a stream with no `EV_SYN` in it.
    /// 2. **A button.** Emitted immediately, carrying the position accumulated so far. Correct
    ///    because evdev orders a frame as motion-then-button, so the movement that preceded the click
    ///    is already in the position.
    /// 3. **An `EV_SYN`.** The boundary. If motion accumulated and nothing else was emitted in this
    ///    frame, it becomes a [`Event::Motion`] or a [`Event::Wheel`].
    /// 4. **Anything else.** Skipped, which is what `Record::Noise` is for.
    ///
    /// **A frame with both motion and a button yields two events**, the button first and the motion
    /// second. That is not a tidy story but it is a true one: they are two things that happened, and
    /// a session that applies them in order ends in the right place either way.
    pub fn next_event(&mut self) -> Option<Event> {
        loop {
            if self.buf.len() - self.cursor < RECORD_BYTES {
                return None;
            }
            let record = &self.buf[self.cursor..self.cursor + RECORD_BYTES];
            self.cursor += RECORD_BYTES;
            let mut bytes = [0u8; RECORD_BYTES];
            bytes.copy_from_slice(record);
            match decode_record(&bytes) {
                Record::Key(ev) => return Some(Event::Key(ev)),
                Record::Button { button, pressed } => {
                    self.pointer.press(button, pressed);
                    return Some(Event::Button {
                        button,
                        pressed,
                        x: self.pointer.x,
                        y: self.pointer.y,
                    });
                }
                Record::Motion { axis, delta } => {
                    // **The position moves here, on the record, and not at the frame boundary.**
                    //
                    // The first version accumulated the delta into `frame` and added it to
                    // `pointer.x` when it saw `EV_SYN` -- which meant a `BTN_LEFT` in the same frame
                    // saw the position from *before* the frame. `a_click_carries_the_movement_that_
                    // preceded_it` caught it: the click was at (0, 0) when the pointer was at
                    // (40, 20), so a caret placed from a click would have landed 40 pixels left of
                    // where the user clicked.
                    //
                    // **Applying it per record makes the boundary do nothing but report.** The
                    // boundary's only remaining job is to decide *whether there was any motion* --
                    // because a frame that folded nothing should emit nothing, which is the
                    // horizontal-wheel rule -- and to clear the accumulator.
                    self.frame.fold(axis, delta);
                    match axis {
                        crate::pointer::REL_X => {
                            self.pointer.x = self.pointer.x.saturating_add(delta).max(0)
                        }
                        crate::pointer::REL_Y => {
                            self.pointer.y = self.pointer.y.saturating_add(delta).max(0)
                        }
                        _ => {}
                    }
                }
                Record::Noise if axis_is_syn(&bytes) => {
                    let frame = self.frame;
                    self.frame = Frame::default();
                    if !frame.moved {
                        continue;
                    }
                    // **Nothing to apply: the records already moved the pointer.** See the arm above.
                    if frame.wheel != 0 {
                        return Some(Event::Wheel {
                            dy: frame.wheel,
                            x: self.pointer.x,
                            y: self.pointer.y,
                        });
                    }
                    return Some(Event::Motion {
                        x: self.pointer.x,
                        y: self.pointer.y,
                    });
                }
                Record::Noise => continue,
            }
        }
    }

    /// Where the pointer is, as this decoder has accumulated it.
    ///
    /// **Exposed so a source can carry the position across `next_event` calls** — which is what a
    /// scripted fixture of `REL_X` records needs in order to produce the same absolute positions a
    /// device would. A caller that cannot ask where the pointer is has no way to answer "and where
    /// was it when the button went down", and that question is the whole of hit testing.
    pub const fn pointer(&self) -> Pointer {
        self.pointer
    }

    /// Bytes held that do not yet form a record.
    ///
    /// Non-zero across a stream boundary is normal. Zero forever on a device means the driver is
    /// giving a size that is not a multiple of 24.
    pub fn pending(&self) -> usize {
        self.buf.len() - self.cursor
    }

    /// Whether any complete record is waiting.
    pub fn has_record(&self) -> bool {
        self.buf.len() - self.cursor >= RECORD_BYTES
    }
}

/// Records read from a device in one `read`. Four is what Phase 8's session uses; it is small enough
/// that a keystroke's latency is not a scheduling question and large enough that the read is not a
/// syscall per key.
pub const RECORDS_PER_READ: usize = 4;

/// An [`InputSource`] over a fixed byte buffer: the scripted stream.
///
/// Owns the bytes, so a fixture can be a `const` or built at run time, and the source is `Send` +
/// `'static`, which is what lets the integration test build one without borrowing anything.
#[derive(Debug, Clone)]
pub struct ScriptedInputSource {
    decoder: RecordDecoder,
    label: &'static str,
}

impl ScriptedInputSource {
    /// A source over raw 24-byte records.
    pub fn new(bytes: &[u8]) -> Self {
        Self {
            decoder: RecordDecoder::from_bytes(bytes),
            label: "scripted",
        }
    }

    /// A source over a list of events, encoded for you.
    ///
    /// The convenience form, and the one the tests use, because writing
    /// `encode(InputEvent::press(KEY_A))` fifty times in a row is noise. Each event is separated by
    /// an `EV_SYN`, as a real device does, so a fixture written this way and one captured from
    /// hardware go down the same path.
    pub fn from_events(events: &[InputEvent]) -> Self {
        let mut bytes = Vec::with_capacity(events.len() * 2 * RECORD_BYTES);
        for ev in events {
            bytes.extend_from_slice(&crate::event::encode(*ev));
            bytes.extend_from_slice(&crate::event::encode(crate::event::syn_report()));
        }
        Self::new(&bytes)
    }

    /// The same, but with no `EV_SYN` between records.
    ///
    /// Exists to be tested *against* [`from_events`](Self::from_events): the two must produce
    /// identical command streams, because [`decode`] drops `EV_SYN` for both. A fixture that only ever
    /// omitted them would never notice a regression in the filter.
    pub fn from_events_bare(events: &[InputEvent]) -> Self {
        let mut bytes = Vec::with_capacity(events.len() * RECORD_BYTES);
        for ev in events {
            bytes.extend_from_slice(&crate::event::encode(*ev));
        }
        Self::new(&bytes)
    }

    /// Whether the buffer has been consumed.
    pub fn is_drained(&self) -> bool {
        !self.decoder.has_record() && self.decoder.pending() == 0
    }
}

impl InputSource for ScriptedInputSource {
    fn next_event(&mut self) -> Result<Option<Event>, InputError> {
        match self.decoder.next_event() {
            Some(ev) => Ok(Some(ev)),
            // `pending() != 0` here means the fixture is truncated mid-record, which is a fixture bug
            // rather than a stream ending -- so it is reported rather than swallowed.
            None if self.decoder.pending() != 0 => Err(InputError::Eof),
            None => Ok(None),
        }
    }

    fn describe(&self) -> &'static str {
        self.label
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{encode, syn_report, InputEvent, EV_MSC, EV_SYN};

    fn press_seq(codes: &[u16]) -> Vec<InputEvent> {
        let mut v = Vec::new();
        for c in codes {
            v.push(InputEvent::press(*c));
            v.push(InputEvent::release(*c));
        }
        v
    }

    #[test]
    fn a_record_decodes() {
        let mut d = RecordDecoder::new();
        d.push(&encode(InputEvent::press(30)));
        assert_eq!(d.next_event(), Some(Event::Key(InputEvent::press(30))));
        assert_eq!(d.next_event(), None);
    }

    #[test]
    fn syn_records_are_skipped_transparently() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&encode(InputEvent::press(30)));
        bytes.extend_from_slice(&encode(syn_report()));
        bytes.extend_from_slice(&encode(InputEvent::release(30)));
        bytes.extend_from_slice(&encode(syn_report()));
        let mut d = RecordDecoder::from_bytes(&bytes);
        assert_eq!(d.next_event(), Some(Event::Key(InputEvent::press(30))));
        assert_eq!(d.next_event(), Some(Event::Key(InputEvent::release(30))));
        assert_eq!(d.next_event(), None);
    }

    #[test]
    fn a_partial_record_is_held_until_the_rest_arrives() {
        let full = encode(InputEvent::press(30));
        let mut d = RecordDecoder::new();
        // Everything but the last byte.
        d.push(&full[..RECORD_BYTES - 1]);
        assert_eq!(d.next_event(), None, "13 bytes short is not an event");
        assert_eq!(d.pending(), RECORD_BYTES - 1);

        d.push(&full[RECORD_BYTES - 1..]);
        assert_eq!(d.next_event(), Some(Event::Key(InputEvent::press(30))));
        assert_eq!(d.pending(), 0);
    }

    #[test]
    fn a_partial_record_split_across_many_pushes() {
        let full = encode(InputEvent::press(30));
        let mut d = RecordDecoder::new();
        for i in 0..RECORD_BYTES {
            d.push(&full[i..i + 1]);
            if i + 1 < RECORD_BYTES {
                assert_eq!(d.next_event(), None, "after {i} bytes");
            }
        }
        assert_eq!(d.next_event(), Some(Event::Key(InputEvent::press(30))));
    }

    #[test]
    fn the_buffer_does_not_grow_without_bound() {
        let mut d = RecordDecoder::new();
        for _ in 0..10_000 {
            d.push(&encode(InputEvent::press(30)));
            d.next_event();
            d.push(&encode(InputEvent::release(30)));
            d.next_event();
        }
        // The prefix is reclaimed each push, so this stays at the steady-state capacity rather than
        // growing by 480 KB.
        assert!(
            d.buf.capacity() <= RECORDS_PER_READ * RECORD_BYTES * 4,
            "capacity grew to {}",
            d.buf.capacity()
        );
    }

    #[test]
    fn the_scripted_source_ends_cleanly() {
        let evs = press_seq(&[30, 31, 32]);
        // **`Event::Key` wrapped, because part 20 widened what `next_event` returns.** The comparison
        // is still *the same assertion* -- three keys in, three events out, in order -- which is the
        // property this gate has always been about. What changed is the wrapper, and that a
        // contract change should be a mechanical edit in a test whose subject is something else.
        let want: Vec<crate::pointer::Event> = evs
            .iter()
            .copied()
            .map(crate::pointer::Event::Key)
            .collect();
        let mut src = ScriptedInputSource::from_events(&evs);
        let mut got = Vec::new();
        while let Some(ev) = src.next_event().expect("no error") {
            got.push(ev);
        }
        assert_eq!(got, want);
        assert!(src.is_drained());
        // And it stays ended.
        assert_eq!(src.next_event().expect("no error"), None);
    }

    #[test]
    fn with_and_without_syn_give_the_same_stream() {
        let evs = press_seq(&[30, 31, 32]);

        let mut with = ScriptedInputSource::from_events(&evs);
        let mut bare = ScriptedInputSource::from_events_bare(&evs);

        let mut a = Vec::new();
        while let Some(ev) = with.next_event().expect("no error") {
            a.push(ev);
        }
        let mut b = Vec::new();
        while let Some(ev) = bare.next_event().expect("no error") {
            b.push(ev);
        }
        // **This is the gate that forbids a per-frame coalescer**, and it is the reason the decoder
        // emits keys per record rather than per frame. `from_events_bare` writes no `EV_SYN` at all,
        // so a coalescer keyed on the boundary would take these three keystrokes and return one.
        let want: Vec<crate::pointer::Event> = evs
            .iter()
            .copied()
            .map(crate::pointer::Event::Key)
            .collect();
        assert_eq!(a, want);
        assert_eq!(b, want);
        assert_eq!(a, b, "the EV_SYN filter changed the stream");
    }

    #[test]
    fn noise_types_are_filtered_from_a_scripted_stream() {
        let mut bytes = Vec::new();
        for kind in [EV_SYN, EV_MSC, EV_SYN] {
            bytes.extend_from_slice(&encode(InputEvent {
                kind,
                code: 0,
                value: 0,
            }));
        }
        bytes.extend_from_slice(&encode(InputEvent::press(30)));
        for kind in [EV_MSC, EV_SYN] {
            bytes.extend_from_slice(&encode(InputEvent {
                kind,
                code: 0,
                value: 0,
            }));
        }
        let mut src = ScriptedInputSource::new(&bytes);
        assert_eq!(
            src.next_event().expect("no error"),
            Some(Event::Key(InputEvent::press(30)))
        );
        assert_eq!(src.next_event().expect("no error"), None);
    }

    #[test]
    fn a_truncated_fixture_is_reported_rather_than_silently_ending() {
        let full = encode(InputEvent::press(30));
        let mut src = ScriptedInputSource::new(&full[..RECORD_BYTES - 5]);
        assert_eq!(src.next_event(), Err(InputError::Eof));
    }

    #[test]
    fn an_empty_fixture_ends_immediately() {
        let mut src = ScriptedInputSource::new(&[]);
        assert_eq!(src.next_event().expect("no error"), None);
        assert!(src.is_drained());
    }

    #[test]
    fn a_long_scripted_stream_is_complete() {
        let evs = press_seq(&(30..60).collect::<Vec<u16>>());
        let mut src = ScriptedInputSource::from_events(&evs);
        let mut n = 0usize;
        while src.next_event().expect("no error").is_some() {
            n += 1;
        }
        assert_eq!(n, evs.len());
    }

    #[test]
    fn errno_names_cover_the_ones_the_session_acts_on() {
        assert_eq!(errno_name(19), "ENODEV");
        assert_eq!(errno_name(11), "EAGAIN");
        assert_eq!(errno_name(4), "EINTR");
        assert_eq!(errno_name(9999), "?");
    }
}

/// Whether a 24-byte record is an `EV_SYN` frame boundary.
///
/// **Read from the bytes rather than from a decoded `Record`, because `decode_record` collapses
/// `EV_SYN` into [`Record::Noise`]** along with `EV_MSC` and everything else -- and the boundary is
/// the one piece of "noise" the decoder absolutely cannot ignore. Keeping the check here, against
/// the raw record, is what makes it possible to see.
fn axis_is_syn(bytes: &[u8]) -> bool {
    u16::from_le_bytes([bytes[16], bytes[17]]) == crate::event::EV_SYN
}
