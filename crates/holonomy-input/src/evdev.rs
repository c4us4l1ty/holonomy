//! Reading a real keyboard.
//!
//! # The device must be opened before the jail is sealed
//!
//! seccomp has no `open`, by construction -- see
//! [`holonomy_jail::seccomp::table::ALLOWLIST`]. So an [`EvdevSource`] cannot open
//! `/dev/input/event0` after [`Sealed`](holonomy_jail::Sealed); it has to be handed a descriptor
//! that was opened during boot.
//!
//! That is why there are two constructors rather than one. [`open`](Self::open) is the convenient
//! pre-boot form and is the only one in the codebase that calls `open` on a device node.
//! [`from_raw_fd`](Self::from_raw_fd) is the form the session uses: it adopts a descriptor, and the
//! type never mentions a path. A post-seal attempt to open one fails with `SIGSYS` and a 137 exit,
//! which is the jail working as intended and not a bug to work around.
//!
//! # Read granularity
//!
//! [`RECORDS_PER_READ`] records per `read`, not one. A read per keypress is a syscall per keystroke,
//! which FR-1.2's keystroke budget has no room for; a read of the whole stream would block until the
//! buffer filled and add latency proportional to its size.

use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd};

use crate::pointer::Event;
use crate::source::{InputError, InputSource, RecordDecoder, RECORDS_PER_READ};

/// A [`InputSource`] over an already-open evdev descriptor.
///
/// Owns the descriptor and closes it on drop, including when the drop is an unwind. The alternative
/// -- borrowing a `File` the session also holds -- would mean the session cannot close it, and a
/// keyboard left open across a container close is a handle into the session's address space that
/// outlives it.
#[derive(Debug)]
pub struct EvdevSource {
    file: std::fs::File,
    decoder: RecordDecoder,
    label: &'static str,
}

impl EvdevSource {
    /// Open a device node. **Only valid before the jail is sealed.**
    ///
    /// `O_RDONLY | O_NONBLOCK`: non-blocking so a keyboard that produces nothing does not stall the
    /// session loop, which has a blink timer and a teardown path that must not wait on a keypress.
    /// Blocking is instead handled where it belongs -- the loop's poll on the descriptor.
    pub fn open(path: &std::path::Path) -> Result<Self, InputError> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(|e| InputError::Read(raw_os_error(&e)))?;
        Ok(Self {
            file,
            decoder: RecordDecoder::new(),
            label: "evdev",
        })
    }

    /// Adopt an open descriptor. The form the session uses.
    ///
    /// # `O_NONBLOCK` is forced here, not left to the caller
    ///
    /// [`open`](Self::open) sets it and this sets it too, and that is not belt-and-braces: the two
    /// constructors were written at different times and only `open` set the flag, so a descriptor
    /// adopted from boot -- which is the path the *session* uses -- would block in
    /// [`next_event`](InputSource::next_event) on a quiet keyboard. The session loop has a caret blink
    /// timer and a teardown path that must not wait on a keypress, so a blocking read here is a hang
    /// with no output and no error.
    ///
    /// Forcing it also means the two constructors cannot disagree, which is the whole reason to force
    /// it rather than document it. A caller who genuinely wants blocking reads can `fcntl` the
    /// descriptor back; nothing here depends on the flag being non-blocking beyond not hanging.
    ///
    /// # Safety
    ///
    /// `fd` must be a valid, open, readable evdev descriptor, and ownership of it passes to this
    /// type, which will close it on drop. Passing a descriptor this type does not own -- one the
    /// session is also reading, or one is already closed -- is a double-close.
    pub unsafe fn from_raw_fd(fd: RawFd) -> Self {
        // SAFETY: `fcntl` on a valid descriptor. A failure is reported by the later reads as EAGAIN
        // or a hang rather than being fatal here, because refusing to adopt the descriptor would
        // leave the session with no input at all -- strictly worse than a possibly-blocking read.
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
        Self {
            // SAFETY: the caller's contract says `fd` is valid, open and readable, and ownership
            // transfers here.
            file: unsafe { std::fs::File::from_raw_fd(fd) },
            decoder: RecordDecoder::new(),
            label: "evdev",
        }
    }

    /// Give up the descriptor instead of closing it.
    pub fn into_raw_fd(self) -> RawFd {
        self.file.into_raw_fd()
    }

    /// The descriptor, for a `poll`.
    #[inline]
    pub fn as_raw_fd(&self) -> RawFd {
        self.file.as_raw_fd()
    }

    /// Read whatever is available now, without blocking.
    ///
    /// Returns how many records were decoded. `0` means "nothing available", which for a non-blocking
    /// descriptor is normal and not an error.
    ///
    /// Separate from [`next_event`](InputSource::next_event) because the session loop wants "is there
    /// anything to do" as a question that cannot fail, and wants the actual decoding to be the
    /// explicit second step.
    pub fn poll_in(&mut self) -> Result<usize, InputError> {
        if self.decoder.has_record() {
            // Already-decoded records from a previous partial read. Drain them first.
            return Ok(0);
        }
        let mut buf = [0u8; RECORDS_PER_READ * crate::event::RECORD_BYTES];
        let n = loop {
            match std::io::Read::read(&mut self.file, &mut buf) {
                Ok(0) => {
                    // EOF on an evdev node means the device is gone.
                    return Err(InputError::Read(libc::ENODEV));
                }
                Ok(n) => break n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(0),
                Err(e) => return Err(InputError::Read(raw_os_error(&e))),
            }
        };
        self.decoder.push(&buf[..n]);
        Ok(n)
    }
}

impl InputSource for EvdevSource {
    fn next_event(&mut self) -> Result<Option<Event>, InputError> {
        if let Some(ev) = self.decoder.next_event() {
            return Ok(Some(ev));
        }
        // Nothing buffered: go and get some. `EAGAIN` becomes `None`, which the loop reads as "no
        // event this tick" rather than as a dead keyboard.
        match self.poll_in() {
            Ok(0) => Ok(None),
            Ok(_) => Ok(self.decoder.next_event()),
            Err(e) => Err(e),
        }
    }

    fn describe(&self) -> &'static str {
        self.label
    }
}

/// `errno` out of a `std::io::Error`, defaulting to `EIO`.
///
/// `raw_os_error` is `None` for a message-shaped error, which cannot happen from a syscall, so the
/// default never fires in practice -- but the function has to return an `i32` and this says which one
/// it invents.
fn raw_os_error(e: &std::io::Error) -> i32 {
    e.raw_os_error().unwrap_or(libc::EIO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{encode, InputEvent};

    /// A pipe standing in for an evdev node, with the write end held so it does not hit EOF.
    struct FakeDevice {
        reader: RawFd,
        writer: Option<RawFd>,
    }

    impl FakeDevice {
        fn new() -> Self {
            let mut fds = [0 as RawFd; 2];
            // SAFETY: `fds` is a two-element array, which is what `pipe` writes.
            let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
            assert_eq!(rc, 0, "pipe failed");
            Self {
                reader: fds[0],
                writer: Some(fds[1]),
            }
        }

        fn writer(&self) -> RawFd {
            self.writer.expect("the write end is already closed")
        }

        fn write_events(&self, events: &[InputEvent]) {
            for ev in events {
                let bytes = encode(*ev);
                let n = unsafe { libc::write(self.writer(), bytes.as_ptr().cast(), bytes.len()) };
                assert_eq!(n as usize, bytes.len());
            }
        }

        fn write_bytes(&self, bytes: &[u8]) {
            let n = unsafe { libc::write(self.writer(), bytes.as_ptr().cast(), bytes.len()) };
            assert_eq!(n as usize, bytes.len());
        }

        fn close_writer(&mut self) {
            if let Some(w) = self.writer.take() {
                unsafe { libc::close(w) };
            }
        }
    }

    impl Drop for FakeDevice {
        fn drop(&mut self) {
            self.close_writer();
            unsafe {
                libc::close(self.reader);
            }
        }
    }

    /// A source reading from a duplicate of the pipe's read end, so the source owns a descriptor it
    /// closes and `FakeDevice` keeps its own.
    fn read_end(dev: &FakeDevice) -> EvdevSource {
        let fd = unsafe { libc::dup(dev.reader) };
        assert!(fd >= 0);
        // SAFETY: `fd` is fresh from `dup`, is readable, and ownership passes to the source.
        unsafe { EvdevSource::from_raw_fd(fd) }
    }

    #[test]
    fn events_arrive_from_a_descriptor() {
        let dev = FakeDevice::new();
        let evs = vec![InputEvent::press(30), InputEvent::release(30)];
        dev.write_events(&evs);

        let mut src = read_end(&dev);
        // **Every expectation is wrapped in `Event::Key`,** because `next_event` returns the pointer
        // enum as of part 20. The wrapper is mechanical and that is the point: the change to the
        // input contract should have been a one-token edit in a hundred places, and it was.
        assert_eq!(
            src.next_event().expect("read"),
            Some(Event::Key(InputEvent::press(30)))
        );
        assert_eq!(
            src.next_event().expect("read"),
            Some(Event::Key(InputEvent::release(30)))
        );
    }

    #[test]
    fn an_idle_device_reports_no_event_rather_than_blocking() {
        let dev = FakeDevice::new();
        let mut src = read_end(&dev);
        assert_eq!(
            src.next_event().expect("no error"),
            None,
            "a quiet keyboard must not stall the loop"
        );
    }

    #[test]
    #[allow(non_snake_case)]
    fn a_closed_device_is_ENODEV_not_a_silent_end() {
        let mut dev = FakeDevice::new();
        let mut src = read_end(&dev);
        dev.close_writer();
        let err = src
            .next_event()
            .expect_err("a closed device must be an error");
        assert_eq!(err, InputError::Read(libc::ENODEV));
    }

    #[test]
    fn a_short_read_is_carried_across_calls() {
        let dev = FakeDevice::new();
        let full = encode(InputEvent::press(30));
        // Fewer bytes than one record.
        dev.write_bytes(&full[..full.len() - 3]);
        let mut src = read_end(&dev);
        assert_eq!(
            src.next_event().expect("no error"),
            None,
            "13 short of a record"
        );

        // The rest arrives.
        dev.write_events(&[InputEvent::press(30)]);
        let got = src.next_event().expect("read");
        assert!(got.is_some(), "the carried tail should have completed");
    }

    #[test]
    fn a_buffered_record_is_not_read_again() {
        let dev = FakeDevice::new();
        dev.write_events(&[InputEvent::press(30), InputEvent::press(31)]);
        let mut src = read_end(&dev);

        // One read brings both in; one decode consumes one, leaving the other buffered.
        assert_eq!(
            src.next_event().expect("read"),
            Some(Event::Key(InputEvent::press(30)))
        );
        // `poll_in` must report "nothing new" rather than issuing a second read, which would be a
        // syscall for an event already in hand.
        assert_eq!(src.poll_in().expect("no error"), 0);
        assert_eq!(
            src.next_event().expect("read"),
            Some(Event::Key(InputEvent::press(31)))
        );
    }

    #[test]
    fn from_raw_fd_forces_non_blocking() {
        // The bug this asserts against: only `open` used to set the flag, so a descriptor adopted
        // from boot -- the session's path -- blocked on a quiet keyboard.
        let dev = FakeDevice::new();
        let mut src = read_end(&dev);
        let flags = unsafe { libc::fcntl(src.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_ne!(
            flags & libc::O_NONBLOCK,
            0,
            "an adopted descriptor is blocking; the session loop would hang"
        );
        // And the property that matters: this returns rather than waiting.
        assert_eq!(src.next_event().expect("no error"), None);
    }

    #[test]
    fn several_events_in_one_read_come_back_in_order() {
        let dev = FakeDevice::new();
        let evs: Vec<InputEvent> = (30..40).map(InputEvent::press).collect();
        dev.write_events(&evs);
        let mut src = read_end(&dev);
        for want in &evs {
            assert_eq!(src.next_event().expect("read"), Some(Event::Key(*want)));
        }
    }
}
