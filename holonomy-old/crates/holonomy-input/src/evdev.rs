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
use crate::source::{InputError, InputSource, RecordDecoder, Wait, RECORDS_PER_READ};

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
    /// **The one epoll instance this source waits on.** `None` only when `epoll_create1` refused.
    ///
    /// **A private instance, and the allowlist's rationale describes a shared one.** The seccomp
    /// table says `epoll_create1` exists because *"the session loop's single epoll instance; three
    /// `epoll_wait` entries would otherwise need three instances"*, and `epoll_ctl` because it is for
    /// *"registering the container fd, the DRM fd and the evdev fd with the loop"*. **This is the
    /// evdev fd on its own instance**, and the single shared loop the rationale describes does not
    /// exist yet — the session has no way to hand its container and DRM descriptors to an input
    /// crate, and inventing that plumbing before there is a second waiter to share it with would be
    /// the speculative generality this project does not build.
    ///
    /// **What is built is the part that was broken**: the loop waits instead of spinning, and an
    /// idle keyboard does not end the session. One instance for one waiter costs one descriptor.
    ///
    /// ## Why `None` degrades rather than failing
    ///
    /// `from_raw_fd` is `unsafe fn -> Self` and cannot report a failure, and a panic inside a sealed
    /// session is worse than a busy loop. **The sealed path cannot reach this**: `epoll_create1` is
    /// allowlisted at stage 11, and [`open`](Self::open) — which *is* the sealed path, and *can*
    /// report a failure — propagates it rather than storing `None`. So the fallback exists only for
    /// an adopted descriptor on a host where epoll is unavailable, and it degrades to today's
    /// behaviour, which is a defect but not a crash.
    epoll: Option<Epoll>,
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
        // **This is the sealed path, so the failure is propagated rather than absorbed.** `open` can
        // return an error and this one would mean "no epoll, so the session will spin", which is
        // exactly the defect `InputSource::wait` was added to remove — better said at start-up than
        // discovered as a hot laptop.
        let epoll = Epoll::create().map_err(io_err)?;
        epoll.register(file.as_raw_fd()).map_err(io_err)?;
        Ok(Self {
            file,
            decoder: RecordDecoder::new(),
            label: "evdev",
            epoll: Some(epoll),
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
        // SAFETY: the caller's contract says `fd` is valid, open and readable, and ownership
        // transfers here.
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        // **Fail closed in `wait`, not by pretending to have waited.** `open` propagates epoll
        // failure at construction; this unsafe constructor cannot return `Result`, so it records
        // the absence and `wait` reports an error rather than returning `Ready` forever. A quiet
        // keyboard must not become a 100% CPU spin just because epoll could not be established.
        let epoll = match Epoll::create() {
            Ok(ep) => match ep.register(file.as_raw_fd()) {
                Ok(()) => Some(ep),
                // The instance exists but the registration did not. Keeping it would be a wait on
                // an empty set, which times out forever and looks exactly like a dead keyboard.
                Err(_) => None,
            },
            Err(_) => None,
        };
        Self {
            file,
            decoder: RecordDecoder::new(),
            label: "evdev",
            epoll,
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

    /// **Wait on the device instead of spinning.** FR-3.5, NFR-1.2.
    ///
    /// # `epoll_wait` and why not `read` with a timeout
    ///
    /// The descriptor is `O_NONBLOCK` — see [`EvdevSource::open`] — so a read cannot wait, and the
    /// only other options were a `sleep` (the same spin, slower) or a blocking read (a hang, and the
    /// session has a caret blink and a teardown path that must not wait on a keypress). `epoll_wait`
    /// on a level-triggered `EPOLLIN` registration is the one that blocks in the kernel, wakes on the
    /// device, and returns on a timeout so the loop can tick.
    ///
    /// **It is also the call the seccomp allowlist has been reserving for the session loop since
    /// Phase 2**, along with `epoll_create1`, `epoll_ctl` and `epoll_pwait`.
    ///
    /// # A timeout is `Ready`, not `Ended`
    ///
    /// The session's caret blinks on a 500 ms period and nothing else in the loop is time-driven. A
    /// timeout is therefore a reason to tick, and returning `Ended` on one would reintroduce the
    /// original defect under a new name: the session exits 125 ms after the last keystroke.
    fn wait(&mut self, timeout_ms: i32) -> Result<Wait, InputError> {
        // Records already decoded from a previous partial read need no syscall: they are already
        // in the buffer, and going to sleep on them would add a blink period of latency to the last
        // keypress of a burst.
        if self.decoder.has_record() {
            return Ok(Wait::Ready);
        }
        let Some(epoll) = self.epoll.as_ref() else {
            // No epoll instance: report a read failure rather than `Ready`. `Ready` here made the
            // session loop spin at full CPU on an adopted descriptor that could not be waited on.
            return Err(InputError::Read(libc::EIO));
        };
        let mut events = [libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: 0,
        }; 1];
        // SAFETY: `epoll` is a live epoll descriptor owned by this value; `events` is a one-element
        // array whose length is passed. A negative timeout blocks indefinitely; the sealed session
        // deliberately passes `-1` so an idle keyboard costs no wakeups.
        let n = unsafe { libc::epoll_wait(epoll.as_raw_fd(), events.as_mut_ptr(), 1, timeout_ms) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            // **A signal is not a failure.** `EINTR` means a handler ran, and the right answer is to
            // report readiness so the loop retries; returning an error would turn every `SIGWINCH`
            // into a dead session.
            if e.raw_os_error() == Some(libc::EINTR) {
                return Ok(Wait::Ready);
            }
            return Err(InputError::Read(raw_os_error(&e)));
        }
        // `n == 0` is the timeout and `n > 0` is the device. Both are `Ready`.
        let _ = events;
        Ok(Wait::Ready)
    }

    fn describe(&self) -> &'static str {
        self.label
    }
}

/// **One epoll instance, wrapping the three syscalls the allowlist reserves for the session loop.**
///
/// `epoll_create1(EPOLL_CLOEXEC)` — **the close-on-exec flag is not optional.** The boot chain
/// installs tripwires and a seccomp filter and then runs the session in the same process; a
/// descriptor that survives an `exec` anywhere in that chain is a descriptor the filter was not
/// written for.
#[derive(Debug)]
struct Epoll(std::os::fd::OwnedFd);

impl AsRawFd for Epoll {
    fn as_raw_fd(&self) -> RawFd {
        self.0.as_raw_fd()
    }
}

impl Epoll {
    fn create() -> std::io::Result<Self> {
        // SAFETY: `epoll_create1` takes only flags; there are no pointers to get wrong.
        let fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `epoll_create1` returned a fresh descriptor, and ownership passes to the
        // `OwnedFd` which closes it on drop.
        Ok(Self(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) }))
    }

    /// Add `fd` at level-triggered `EPOLLIN`, with `u64` data 0.
    fn register(&self, fd: RawFd) -> std::io::Result<()> {
        let mut event = libc::epoll_event {
            events: libc::EPOLLIN as u32,
            u64: 0,
        };
        // SAFETY: `self.0` is a live epoll descriptor, `fd` is open and owned elsewhere, and
        // `event` outlives the call. `EPOLL_CTL_ADD` fails with `EEXIST` if called twice, which is
        // why each source registers exactly once, at construction.
        let r = unsafe { libc::epoll_ctl(self.0.as_raw_fd(), libc::EPOLL_CTL_ADD, fd, &mut event) };
        if r < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }
}

/// A `std::io::Error` as this crate's [`InputError::Read`], carrying the `errno`.
///
/// **One function rather than `.map_err(InputError::Read)` at each site**, because `InputError::Read`
/// takes an `i32` and `std::io::Error` is not one. Writing `map_err(InputError::Read)` is a type
/// error the compiler catches, which is the right way to learn this; writing `map_err(io_err)` is
/// what it should have been.
fn io_err(e: std::io::Error) -> InputError {
    InputError::Read(raw_os_error(&e))
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
    #[test]
    fn adopted_descriptor_without_epoll_fails_closed() {
        use std::os::fd::IntoRawFd;

        // Regular files cannot be registered with epoll on Linux (EPERM). That gives a
        // deterministic way to exercise the adopted-fd path where `epoll` is absent.
        let path = std::env::temp_dir().join(format!(
            "holonomy-evdev-no-epoll-{}.tmp",
            std::process::id()
        ));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .expect("open scratch file");
        let fd = file.into_raw_fd();
        // SAFETY: `fd` is fresh, open, readable, and ownership passes to the source exactly once.
        let mut src = unsafe { EvdevSource::from_raw_fd(fd) };
        let err = src
            .wait(0)
            .expect_err("a descriptor that cannot be epoll-waited must not report Ready");
        assert!(matches!(err, InputError::Read(_)));
        drop(src);
        let _ = std::fs::remove_file(path);
    }
}
