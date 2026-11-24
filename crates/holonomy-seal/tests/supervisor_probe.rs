//! Linux-only **prototype**, not the shipped reaper or an L-7 qualification.
//!
//! A disposable subprocess writes a known shared memfd arena, then blocks in I/O or spins while
//! holding a lock. Its parent sends SIGSTOP, observes the group stop, sends SIGKILL, waits for
//! confirmed process exit, and only then volatile-wipes and verifies the shared mapping. This
//! removes the prototype worker's access before the wipe; it does not cover private heap, stacks,
//! registers, the real editor, terminal copies, locked memory, or hostile external processes.
//! Linux and the scheduler do not promise bounded signal delivery. Measurements are evidence for
//! this host only; this is not a real-time guarantee.
#![cfg(target_os = "linux")]

use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{fence, Ordering};
use std::time::{Duration, Instant};

const ARENA_BYTES: usize = 4 * 1024 * 1024;
const CANARY_BYTES: usize = 4 * 1024;
const STOP_TIMEOUT: Duration = Duration::from_secs(2);
const MODES: [&str; 3] = ["blocked_io", "locked_spin", "multithread"];
const TRIALS: usize = 4;

struct SharedArena {
    fd: OwnedFd,
    ptr: *mut u8,
}

impl SharedArena {
    fn create() -> io::Result<Self> {
        // SAFETY: static NUL-terminated name. CLOEXEC prevents accidental inheritance by other
        // children; only our worker clears it in its own post-fork, pre-exec context.
        let raw =
            unsafe { libc::memfd_create(c"holonomy-supervisor-probe".as_ptr(), libc::MFD_CLOEXEC) };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a freshly returned, owned descriptor.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: fd is valid, length fits off_t, and ftruncate does not touch Rust memory.
        if unsafe { libc::ftruncate(fd.as_raw_fd(), ARENA_BYTES as libc::off_t) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let ptr = map(fd.as_raw_fd())?;
        // Best effort only: this experiment does not establish locked/nondumpable memory.
        // SAFETY: `ptr` and length describe the just-created valid mapping.
        let _ = unsafe { libc::madvise(ptr.cast(), ARENA_BYTES, libc::MADV_DONTDUMP) };
        Ok(Self { fd, ptr })
    }

    // Only call after confirmed child exit; never while the worker can access the mapping.
    fn all_nonzero(&self) -> bool {
        (0..ARENA_BYTES).all(|index| {
            // SAFETY: the child has exited and the mapped byte lies within the live mapping.
            unsafe { self.ptr.add(index).read_volatile() != 0 }
        })
    }

    // Only call after confirmed child exit; the worker can never resume and touch the mapping.
    fn wipe_and_verify(&mut self) -> (Duration, Duration, usize) {
        let start = Instant::now();
        let word = std::mem::size_of::<usize>();
        assert_eq!((self.ptr as usize) % std::mem::align_of::<usize>(), 0);
        assert_eq!(ARENA_BYTES % word, 0);
        for index in 0..ARENA_BYTES / word {
            // SAFETY: mmap is page-aligned, the index stays in bounds, and the child has exited.
            unsafe { self.ptr.cast::<usize>().add(index).write_volatile(0) };
        }
        fence(Ordering::SeqCst);
        let wipe_elapsed = start.elapsed();
        let ghost_bytes = self.count_ghosts();
        (wipe_elapsed, start.elapsed(), ghost_bytes)
    }

    // Only call after confirmed child exit, while the shared arena is quiescent.
    fn count_ghosts(&self) -> usize {
        let mut ghost_bytes = 0usize;
        for index in 0..ARENA_BYTES {
            // SAFETY: child exited; this is an in-bounds volatile read of initialized bytes.
            if unsafe { self.ptr.add(index).read_volatile() } != 0 {
                ghost_bytes += 1;
            }
        }
        ghost_bytes
    }
}

impl Drop for SharedArena {
    fn drop(&mut self) {
        // SAFETY: this mapping was created by `map` and is unmapped exactly once here.
        unsafe { libc::munmap(self.ptr.cast(), ARENA_BYTES) };
    }
}

fn map(fd: libc::c_int) -> io::Result<*mut u8> {
    // SAFETY: null requests an OS-selected address, fd names the resized memfd, and the returned
    // pointer is checked for MAP_FAILED before use.
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            ARENA_BYTES,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        Err(io::Error::last_os_error())
    } else {
        Ok(ptr.cast())
    }
}

// Owns child lifecycle on success, error, and panic. Killing the worker is not a verified wipe.
struct Worker {
    child: Child,
    pidfd: OwnedFd,
    control: UnixStream,
    arena: SharedArena,
    reaped: bool,
}

// A pidfd refers to the original process even if its numeric PID is recycled. This prototype
// requires Linux pidfd support; it does not fall back to signaling a numeric PID after setup.
fn send_pidfd_signal(fd: RawFd, signal: libc::c_int) -> io::Result<()> {
    // SAFETY: fd is an open pidfd, no siginfo is supplied, and flags must be zero.
    let result = unsafe {
        libc::syscall(
            libc::SYS_pidfd_send_signal,
            fd,
            signal,
            std::ptr::null::<libc::siginfo_t>(),
            0u32,
        )
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

// Observe a child event through its pidfd, never a potentially recycled numeric PID. With
// WNOHANG, a zero pid means no event. The caller chooses whether WNOWAIT retains the event.
fn pidfd_event(fd: RawFd, options: libc::c_int) -> io::Result<Option<(libc::c_int, libc::c_int)>> {
    // SAFETY: all-zero is a valid initial siginfo_t; waitid writes it before any fields are read.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: the pidfd identifies our child, and info points to valid writable storage.
    let result = unsafe { libc::waitid(libc::P_PIDFD, fd as libc::id_t, &mut info, options) };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: waitid initialized siginfo; si_pid is zero when WNOHANG found no event.
    if unsafe { info.si_pid() } == 0 {
        return Ok(None);
    }
    // SAFETY: this is a child-status event returned by waitid.
    Ok(Some((info.si_code, unsafe { info.si_status() })))
}

impl Worker {
    fn start(arena: SharedArena, mode: &str) -> io::Result<Self> {
        let (parent, child_socket) = UnixStream::pair()?;
        let socket = child_socket.as_raw_fd();
        let arena_fd = arena.fd.as_raw_fd();
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args([
                "--exact",
                "supervisor_worker",
                "--ignored",
                "--test-threads=1",
            ])
            .env("HOLONOMY_PROBE_ARENA_FD", arena_fd.to_string())
            .env("HOLONOMY_PROBE_CONTROL_FD", socket.to_string())
            .env("HOLONOMY_PROBE_MODE", mode)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        use std::os::unix::process::CommandExt;
        // SAFETY: after fork and before exec, the closure invokes only async-signal-safe fcntl
        // operations on descriptors held open by this function. Parent flags remain CLOEXEC.
        unsafe {
            command.pre_exec(move || {
                for fd in [arena_fd, socket] {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        // SAFETY: pid comes from the newly spawned child; flags zero is required by pidfd_open.
        let raw_pidfd =
            unsafe { libc::syscall(libc::SYS_pidfd_open, child.id() as libc::pid_t, 0u32) };
        if raw_pidfd < 0 {
            let error = io::Error::last_os_error();
            // Setup failed before acquiring a pidfd. This controlled test owns and must reap the
            // freshly spawned child. Atomic pidfd creation is still needed in a production design.
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        // SAFETY: a successful pidfd_open returns a fresh owned descriptor fitting in c_int.
        let pidfd = unsafe { OwnedFd::from_raw_fd(raw_pidfd as RawFd) };
        drop(child_socket);
        let mut worker = Self {
            child,
            pidfd,
            control: parent,
            arena,
            reaped: false,
        };
        worker.control.set_read_timeout(Some(STOP_TIMEOUT))?;
        let mut ready = [0u8; 1];
        worker.control.read_exact(&mut ready)?;
        if ready != *b"R" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid worker readiness",
            ));
        }
        if mode == "blocked_io" || mode == "multithread" {
            worker.wait_for_socket_read()?;
        }
        Ok(worker)
    }

    // Linux exposes each blocked thread's kernel wait channel. Confirm that the worker really
    // reached its socket read, rather than merely signaling immediately before it.
    fn wait_for_socket_read(&mut self) -> io::Result<()> {
        let deadline = Instant::now() + STOP_TIMEOUT;
        let path = format!("/proc/{}/task", self.child.id());
        loop {
            for task in std::fs::read_dir(&path)? {
                let wait_channel = std::fs::read_to_string(task?.path().join("wchan"))?;
                if wait_channel.trim().starts_with("unix_stream_") {
                    return Ok(());
                }
            }
            if pidfd_event(
                self.pidfd.as_raw_fd(),
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )?
            .is_some()
            {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "worker exited before blocking",
                ));
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "worker socket read was not observed",
                ));
            }
            std::thread::yield_now();
        }
    }

    fn stop_and_terminate(&mut self, timeout: Duration) -> io::Result<()> {
        if self.reaped {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "worker already reaped",
            ));
        }
        let deadline = Instant::now() + timeout;
        let fd = self.pidfd.as_raw_fd();
        send_pidfd_signal(fd, libc::SIGSTOP)?;
        loop {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "child stop was not confirmed",
                ));
            }
            match pidfd_event(
                fd,
                libc::WSTOPPED | libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            ) {
                Ok(Some((libc::CLD_STOPPED, libc::SIGSTOP))) => break,
                Ok(Some(_)) => {
                    return Err(io::Error::other("worker exited or stopped unexpectedly"))
                }
                Ok(None) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
            std::thread::yield_now();
        }
        send_pidfd_signal(fd, libc::SIGKILL)?;
        loop {
            match pidfd_event(fd, libc::WEXITED | libc::WNOHANG) {
                Ok(Some((code, signal))) => {
                    self.reaped = true;
                    if code != libc::CLD_KILLED || signal != libc::SIGKILL {
                        return Err(io::Error::other("worker did not exit by SIGKILL"));
                    }
                    return Ok(());
                }
                Ok(None) => {}
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "child exit was not confirmed",
                ));
            }
            std::thread::yield_now();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Best-effort cleanup for failed tests; does not authorize the caller to wipe concurrently.
        if !self.reaped {
            let _ = send_pidfd_signal(self.pidfd.as_raw_fd(), libc::SIGKILL);
            loop {
                match pidfd_event(self.pidfd.as_raw_fd(), libc::WEXITED) {
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    _ => break,
                }
            }
        }
    }
}

#[test]
#[ignore = "subprocess worker; started only by the supervisor test"]
fn supervisor_worker() {
    let Ok(arena_fd) = std::env::var("HOLONOMY_PROBE_ARENA_FD") else {
        // Running the ignored tests directly does not create a subprocess fixture.
        return;
    };
    let fd: libc::c_int = arena_fd.parse().unwrap();
    let control_fd: libc::c_int = std::env::var("HOLONOMY_PROBE_CONTROL_FD")
        .unwrap()
        .parse()
        .unwrap();
    let mode = std::env::var("HOLONOMY_PROBE_MODE").unwrap();
    let ptr = map(fd).unwrap();
    // SAFETY: the process owns this fresh mapping; its disjoint slices are assigned to exactly
    // one worker thread apiece, and the supervisor does not access it until this process exits.
    let bytes = unsafe { std::slice::from_raw_parts_mut(ptr, ARENA_BYTES) };
    bytes.fill(0x5a);
    bytes[..CANARY_BYTES].fill(0xa5);
    // SAFETY: this inherited descriptor is owned by this worker and converted exactly once.
    let mut control = unsafe { UnixStream::from_raw_fd(control_fd) };
    match mode.as_str() {
        "blocked_io" => {
            control.write_all(b"R").unwrap();
            let mut byte = [0u8; 1];
            let _ = control.read(&mut byte);
            std::hint::black_box(bytes);
        }
        "locked_spin" => {
            let mutex = std::sync::Mutex::new(());
            let _guard = mutex.lock().unwrap();
            control.write_all(b"R").unwrap();
            loop {
                // SAFETY: the exclusive mutable slice remains owned by this thread.
                unsafe { bytes[CANARY_BYTES..].as_mut_ptr().write_volatile(0x7e) };
                std::hint::spin_loop();
            }
        }
        "multithread" => {
            std::thread::scope(|scope| {
                let (_, payload) = bytes.split_at_mut(CANARY_BYTES);
                let (writer, held) = payload.split_at_mut(1);
                let (tx, rx) = std::sync::mpsc::sync_channel(0);
                scope.spawn(move || {
                    tx.send(()).unwrap();
                    loop {
                        // SAFETY: this byte is disjoint from the slice held by the main thread.
                        unsafe { writer.as_mut_ptr().write_volatile(0x7e) };
                        std::hint::spin_loop();
                    }
                });
                rx.recv().unwrap();
                control.write_all(b"R").unwrap();
                let mut byte = [0u8; 1];
                let _ = control.read(&mut byte);
                std::hint::black_box(held);
            });
        }
        _ => panic!("unknown prototype worker mode"),
    }
}

#[derive(Debug)]
struct Report {
    stop_and_exit: Duration,
    trip_to_verified: Duration,
    ghost_bytes: usize,
}

// This is the sole test path to a wipe. An unconfirmed stop or exit returns an error before
// inspecting or modifying the shared bytes. The pre-wipe read is a negative control and is
// intentionally included in the measured time.
fn supervise(worker: &mut Worker, timeout: Duration) -> io::Result<Report> {
    let start = Instant::now();
    worker.stop_and_terminate(timeout)?;
    let stop_and_exit = start.elapsed();
    if !worker.arena.all_nonzero() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "negative control contains zeros",
        ));
    }
    let (_, _, ghost_bytes) = worker.arena.wipe_and_verify();
    if ghost_bytes != 0 {
        return Err(io::Error::other(format!(
            "shared arena has {ghost_bytes} ghost bytes"
        )));
    }
    Ok(Report {
        stop_and_exit,
        trip_to_verified: start.elapsed(),
        ghost_bytes,
    })
}

#[test]
#[ignore = "Linux pidfd and procfs prototype; run the dedicated CI measurement"]
fn isolated_supervisor_stops_terminates_wipes_and_checks_the_shared_arena() {
    let mut timings = Vec::new();
    for mode in MODES {
        for _ in 0..TRIALS {
            let arena = SharedArena::create().unwrap();
            let mut worker = Worker::start(arena, mode).unwrap();
            let report = supervise(&mut worker, STOP_TIMEOUT).unwrap();
            assert_eq!(report.ghost_bytes, 0, "shared arena retained nonzero bytes");
            println!("HOLONOMY_SUPERVISOR_PROBE mode={mode} bytes={ARENA_BYTES} stop_and_exit_us={} trip_to_verified_us={} ghosts={}", report.stop_and_exit.as_micros(), report.trip_to_verified.as_micros(), report.ghost_bytes);
            timings.push(report.trip_to_verified);
        }
    }
    timings.sort_unstable();
    let over_50ms = timings
        .iter()
        .filter(|elapsed| **elapsed > Duration::from_millis(50))
        .count();
    println!("HOLONOMY_SUPERVISOR_SUMMARY trials={} p50_us={} max_us={} over_50ms={over_50ms} scope=shared_test_arena_only", timings.len(), timings[timings.len() / 2].as_micros(), timings.last().unwrap().as_micros());
}

#[test]
#[ignore = "Linux pidfd and procfs prototype; run the dedicated CI measurement"]
fn repeated_trip_never_signals_a_reaped_pid() {
    let arena = SharedArena::create().unwrap();
    let mut worker = Worker::start(arena, "locked_spin").unwrap();
    assert_eq!(supervise(&mut worker, STOP_TIMEOUT).unwrap().ghost_bytes, 0);
    let error = supervise(&mut worker, STOP_TIMEOUT).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[test]
#[ignore = "Linux pidfd and procfs prototype; run the dedicated CI measurement"]
fn ghost_control_detects_a_byte_left_in_the_canary() {
    let arena = SharedArena::create().unwrap();
    let mut worker = Worker::start(arena, "locked_spin").unwrap();
    worker.stop_and_terminate(STOP_TIMEOUT).unwrap();
    let (_, _, ghosts) = worker.arena.wipe_and_verify();
    assert_eq!(ghosts, 0);
    // SAFETY: the child is reaped and the first byte belongs to the valid shared mapping.
    unsafe { worker.arena.ptr.write_volatile(0xa5) };
    assert_eq!(worker.arena.count_ghosts(), 1);
    let (_, _, ghosts) = worker.arena.wipe_and_verify();
    assert_eq!(ghosts, 0);
}

#[test]
#[ignore = "Linux pidfd and procfs prototype; run the dedicated CI measurement"]
fn externally_reaped_child_cannot_be_signaled_through_its_stale_pidfd() {
    let arena = SharedArena::create().unwrap();
    let mut worker = Worker::start(arena, "locked_spin").unwrap();
    send_pidfd_signal(worker.pidfd.as_raw_fd(), libc::SIGKILL).unwrap();
    let mut status = 0;
    // SAFETY: this deliberately models another child reaper consuming our direct child's status.
    let waited = unsafe { libc::waitpid(worker.child.id() as libc::pid_t, &mut status, 0) };
    assert_eq!(waited, worker.child.id() as libc::pid_t);
    assert!(libc::WIFSIGNALED(status));
    let error = supervise(&mut worker, STOP_TIMEOUT).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(libc::ESRCH));
    assert!(
        worker.arena.all_nonzero(),
        "stale pidfd must not authorize a wipe"
    );
    // Leave reaped false: Drop must also be harmless with a stale pidfd and ECHILD.
}

#[test]
#[ignore = "Linux pidfd and procfs prototype; run the dedicated CI measurement"]
fn unconfirmed_stop_does_not_authorize_a_wipe() {
    let arena = SharedArena::create().unwrap();
    let mut worker = Worker::start(arena, "locked_spin").unwrap();
    let error = supervise(&mut worker, Duration::ZERO).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    // Confirm exit first, then inspect: never touch the mapping while the child may access it.
    send_pidfd_signal(worker.pidfd.as_raw_fd(), libc::SIGKILL).unwrap();
    let event = pidfd_event(worker.pidfd.as_raw_fd(), libc::WEXITED).unwrap();
    assert_eq!(event, Some((libc::CLD_KILLED, libc::SIGKILL)));
    worker.reaped = true;
    assert!(
        worker.arena.all_nonzero(),
        "unconfirmed stop must not wipe the shared arena"
    );
}
