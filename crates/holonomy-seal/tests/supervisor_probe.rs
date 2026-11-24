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
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
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
        let mut ghost_bytes = 0usize;
        for index in 0..ARENA_BYTES {
            // SAFETY: child exited; this is an in-bounds volatile read of initialized bytes.
            if unsafe { self.ptr.add(index).read_volatile() } != 0 {
                ghost_bytes += 1;
            }
        }
        (wipe_elapsed, start.elapsed(), ghost_bytes)
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
    control: UnixStream,
    arena: SharedArena,
    reaped: bool,
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
        let child = command.spawn()?;
        drop(child_socket);
        let mut worker = Self {
            child,
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
            if self.child.try_wait()?.is_some() {
                self.reaped = true;
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
        let pid = self.child.id() as libc::pid_t;
        // SAFETY: pid is the live child spawned and owned by this object, not an arbitrary pid.
        if unsafe { libc::kill(pid, libc::SIGSTOP) } != 0 {
            return Err(io::Error::last_os_error());
        }
        loop {
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "child stop was not confirmed",
                ));
            }
            let mut status = 0;
            // SAFETY: `status` is valid writable memory; waitpid only observes this direct child.
            let result =
                unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG | libc::WUNTRACED) };
            if result == pid {
                if libc::WIFSTOPPED(status) && libc::WSTOPSIG(status) == libc::SIGSTOP {
                    break;
                }
                // waitpid has already reaped a terminal status; do not signal this PID again,
                // since it may be reused by the kernel before cleanup runs.
                if libc::WIFEXITED(status) || libc::WIFSIGNALED(status) {
                    self.reaped = true;
                }
                return Err(io::Error::other("worker exited or stopped unexpectedly"));
            }
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
            std::thread::yield_now();
        }
        self.child.kill()?;
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.reaped = true;
                use std::os::unix::process::ExitStatusExt;
                if status.signal() != Some(libc::SIGKILL) {
                    return Err(io::Error::other("worker did not exit by SIGKILL"));
                }
                return Ok(());
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
            let _ = self.child.kill();
            let _ = self.child.wait();
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
    Ok(Report {
        stop_and_exit,
        trip_to_verified: start.elapsed(),
        ghost_bytes,
    })
}

#[test]
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
fn repeated_trip_never_signals_a_reaped_pid() {
    let arena = SharedArena::create().unwrap();
    let mut worker = Worker::start(arena, "locked_spin").unwrap();
    assert_eq!(supervise(&mut worker, STOP_TIMEOUT).unwrap().ghost_bytes, 0);
    let error = supervise(&mut worker, STOP_TIMEOUT).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn unconfirmed_stop_does_not_authorize_a_wipe() {
    let arena = SharedArena::create().unwrap();
    let mut worker = Worker::start(arena, "locked_spin").unwrap();
    let error = supervise(&mut worker, Duration::ZERO).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    // Confirm exit first, then inspect: never touch the mapping while the child may access it.
    worker.child.kill().unwrap();
    let _ = worker.child.wait().unwrap();
    worker.reaped = true;
    assert!(
        worker.arena.all_nonzero(),
        "unconfirmed stop must not wipe the shared arena"
    );
}
