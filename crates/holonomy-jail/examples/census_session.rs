//! The census session: a real program that boots the jail and runs a session under the filter.
//!
//! Run by `tests/census.rs`, which re-executes this binary with `HOLONOMY_JAIL_CENSUS_ACTION` and
//! `HOLONOMY_JAIL_CENSUS_DROP` set. Not run by hand; see that file for the method.
//!
//! # Why an example and not a `#[test]`
//!
//! `unshare(CLONE_NEWUSER)` fails with `EINVAL` when more than one thread is alive, because the
//! kernel will not change a thread group's credentials while it does. `libtest` runs every test on a
//! spawned thread, so **no `#[test]` can isolate the network**, however it is written -- the boot
//! always fails at stage 6 with an errno that names neither the cause nor the fix. See
//! [`holonomy_jail::netns`].
//!
//! An example's `main` runs on the process's only thread, which is what the boot needs. It also gets
//! dev-dependencies, which is how this can use `holonomy-container` and friends while the
//! `holonomy-jail` library itself still depends on nothing but `libc`. A `[[bin]]` target would not:
//! bins get `[dependencies]`, and giving the jail normal dependencies on the container and the text
//! engine would invert the dependency direction the whole crate exists to protect.
//!
//! And it is the right shape anyway. Phase 8's boot lives in a binary's `main` for the same reason,
//! so the census exercises the same sequence the product will.
//!
//! # Output contract
//!
//! Three lines on stderr, each parseable by the parent:
//!
//! * `session.fail <reason>` -- the workload failed; nothing else is printed.
//! * `holonomy-census syscall=<n> name=<n|first=yes|no>` -- written by the SIGSYS handler under
//!   `Action::Trap`, then the process exits 90.
//! * `session.ok <facts>` and `holonomy-teardown <summary>` -- the session completed and the
//!   teardown ran; the process exits 0.

use std::os::fd::IntoRawFd;
use std::path::{Path, PathBuf};

use holonomy_jail::seccomp::{table, Action};
use holonomy_jail::{AltStack, Enter, Sealed, ALT_STACK_BYTES};

const OK_MARKER: &str = "session.ok ";
const FAIL_MARKER: &str = "session.fail ";
const WORKLOAD_DONE: &str = "session: workload complete";

/// Deliberately weak. Argon2id's `m` is fixed by the project constants; the VDF count is not what is
/// being measured here.
const PASSPHRASE: &str = "census";
const VDF_ITERATIONS: u64 = 2;
const SESSION_TEXT: &[u8] = b"edited after open";

fn main() {
    let action = match std::env::var("HOLONOMY_JAIL_CENSUS_ACTION")
        .unwrap_or_default()
        .as_str()
    {
        "trap" => Action::Trap,
        "kill" => Action::Kill,
        "kill-process" => Action::KillProcess,
        other => fail(format!("unknown action {other:?}")),
    };
    let dropped: Vec<String> = std::env::var("HOLONOMY_JAIL_CENSUS_DROP")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let entries: Vec<table::Allowed> = table::ALLOWLIST
        .iter()
        .copied()
        .filter(|entry| !dropped.iter().any(|name| name == entry.name))
        .collect();
    if entries.len() + dropped.len() != table::ALLOWLIST.len() {
        fail(format!(
            "each dropped name must match exactly one allowlist entry; dropped={dropped:?}"
        ));
    }

    // The census handler goes in before the filter, because the filter is what delivers SIGSYS.
    // Installed unconditionally so the child's syscall path is identical in every mode -- otherwise
    // the census and the production runs differ by an `rt_sigaction`, which is exactly the kind of
    // difference that makes a census wrong.
    if let Err(e) = holonomy_jail::census::install_handler(2) {
        fail(format!("install the census SIGSYS handler: {e}"));
    }

    // The alternate signal stack has to exist before stage 3, and it outlives everything.
    let alt = jail_altstack();

    let mut sealed = Enter
        .seal_core_dumps()
        .raise_memlock()
        .install_tripwires(alt)
        .unwrap_or_else(|e| fail(format!("stage 3, tripwires: {e}")))
        .open_descriptors(Descriptors::open)
        .unwrap_or_else(|e| fail(format!("stage 4, descriptors: {e}")))
        .lock_all_pages()
        .isolate_network()
        .drop_privileges()
        .unwrap_or_else(|e| fail(format!("stage 7, drop privileges: {e}")))
        // `seal_with`, not `seal`: the census has to install a deliberately incomplete table, and it
        // has to do so *through* the boot sequence, so that what it tests is the real boot rather
        // than a hand-assembled filter standing next to it.
        .seal_with(action, &entries)
        .unwrap_or_else(|e| fail(format!("stage 8, seal: {e}")));

    // From here the filter is in force: no allocation that can grow the heap, no path, no syscall
    // outside the table.
    sealed.run_session(|sealed| {
        if let Err(e) = session_workload(sealed.context_mut()) {
            eprintln!("{FAIL_MARKER}{e}");
            std::process::exit(1);
        }
        eprintln!("{OK_MARKER}{}", describe(sealed, entries.len()));
    });

    // Exit through the jail's own teardown, not through Rust's runtime.
    //
    // The census found `open` (syscall 2) being issued after the workload finished and before the
    // process reported -- libtest's teardown, in the version of this that was a `#[test]`. The
    // tempting fix was to allowlist `open`, which would surrender the single most valuable property
    // of the design: a jailed process cannot name a path. The real fix is that the production
    // process never runs a harness; it ends in `TeardownPlan::run_and_exit`.
    let (output_fd, input_fd) = {
        let ctx = sealed.context();
        (ctx.output_fd, ctx.input_fd)
    };
    sealed
        .teardown()
        .sync(output_fd)
        .unwrap_or_else(|e| fail(format!("plan the output fd's fsync: {e}")));
    sealed
        .teardown()
        .close(input_fd)
        .unwrap_or_else(|e| fail(format!("plan the event loop's pipe close: {e}")));
    sealed
        .teardown()
        .close(output_fd)
        .unwrap_or_else(|e| fail(format!("plan the output fd close: {e}")));
    sealed.teardown_and_exit(2);
}

/// Report a boot-stage failure and exit non-zero.
///
/// `_exit` rather than `exit`: this runs before the filter in every case, but a boot that cannot
/// complete must not run atexit handlers on the way out.
fn fail(reason: String) -> ! {
    eprintln!("{FAIL_MARKER}{reason}");
    // SAFETY: a single `exit_group`.
    unsafe { libc::_exit(1) }
}

/// A 64 KiB alternate signal stack on a page-locked `SecureBlock`, registered with the tripwire's
/// scrub table.
///
/// The block is leaked deliberately, and that is the point: it is the stack the `SIGSEGV` handler
/// runs on, which is after every ordinary Rust lifetime has ended, so it has to outlive all of them.
/// Dropping it would `munmap` the very stack the kernel is about to push a frame onto.
///
/// This is also the structural reason the registry lives in the jail rather than in
/// `holonomy-secure`: the signal frame the kernel writes here holds the faulting address and the
/// whole register state, so this block is exactly the memory that must not survive a crash -- and it
/// gets scrubbed by `registry::scrub_all` because `SecureBlock::allocate` registered it.
fn jail_altstack() -> AltStack {
    use holonomy_secure::SecureBlock;
    let block = Box::leak(Box::new(
        SecureBlock::allocate(ALT_STACK_BYTES).expect("alternate stack block"),
    ));
    AltStack::install(block.as_ptr() as usize, ALT_STACK_BYTES).expect("install the alt stack")
}

/// What stage 4 established. The only point at which a file can be named.
struct Descriptors {
    output_fd: i32,
    /// The read end of a pipe, which is what the session waits on.
    ///
    /// A **pipe**, not a file. The first version used a regular file, and `epoll_ctl` refused it
    /// with `EPERM`: epoll cannot watch a regular file, because a regular file is always ready and
    /// has nothing to wait for. The kernel says so plainly and the syscall's name says nothing.
    input_fd: i32,
    /// The container the boot opened, kept so the session can commit through its `O_DIRECT` fd --
    /// which it has to, because after the filter there is no `openat` to get a second one.
    container: holonomy_container::Wavefunction,
}

impl Descriptors {
    fn open() -> Result<Self, StageError> {
        let dir = census_scratch();
        // Start from nothing. `Wavefunction::create` does not `O_TRUNC` -- deliberately, so that a
        // "create" never silently destroys -- which means a scratch directory left over from an
        // earlier run is *reused*, and the round trip then reads back the previous run's plaintext.
        // That is exactly what happened: the check reported `edited by the session` as the answer
        // to a question about `edited after open`.
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(StageError::Io)?;
        let container_path = dir.join("census.wavefunction");
        let output_path = dir.join("frame.ppm");

        // A pipe for the event loop's watch descriptor, so `pipe2` is boot-time only. See
        // `table::BOOT_ONLY`.
        let mut pipe_fds = [0i32; 2];
        // SAFETY: `pipe_fds` is a two-element array and `O_CLOEXEC` is a valid `O_*` mask.
        if unsafe { libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(StageError::Io(std::io::Error::last_os_error()));
        }

        // The container round trip belongs to stage 4, not to the session. That is forced, and the
        // reason points nowhere near the symptom.
        //
        // `Wavefunction::create`/`open` derive the root key with Argon2id at the project's fixed
        // `m = 128 MiB`, which is a 128 MiB allocation. Stage 5 calls
        // `mlockall(MCL_CURRENT | MCL_FUTURE)`, and under `MCL_FUTURE` the kernel marks every
        // *new* mapping `VM_LOCKED` and refuses it once `can_do_mlock()` finds the `RLIMIT_MEMLOCK`
        // budget spent -- measured here as `EAGAIN` from `mmap`. This host's ceiling is 8,192 KiB, so
        // a KDF that runs after `mlockall` cannot allocate its working buffer at all, and the peak
        // is 16x the ceiling.
        //
        // It surfaces as `ContainerError::Envelope(EnvelopeError::Argon2Failed)`, which mentions
        // neither memory nor `mlock` nor the order of two calls four stages apart.
        //
        // The real boot has the same shape, so this is not a harness artefact: the unlock happens at
        // container open, which is stage 4, and by stage 5 Argon2's buffer is gone. The consequence
        // is that `RLIMIT_MEMLOCK` only has to cover the session's *steady-state* working set and
        // not Argon2's peak -- which is true only because the KDF runs first, and that ordering is
        // now load-bearing rather than conventional.
        let container = container_round_trip(&container_path).map_err(StageError::Container)?;
        Ok(Self {
            output_fd: std::fs::File::create(&output_path)
                .map_err(StageError::Io)?
                .into_raw_fd(),
            input_fd: pipe_fds[0],
            container,
        })
    }
}

/// Where the census scratch lives, and why it is not `/tmp`.
///
/// Each run creates a container, and a container is exactly `layout::CONTAINER_SIZE` = 128 MiB of
/// preallocated file. The census spawns one process per test, so the scratch holds well over a
/// gigabyte of mostly-sparse files.
///
/// The first version used `std::env::temp_dir()`, which is a 7.7 GiB tmpfs on this host already 80%
/// used. After about twelve runs `Wavefunction::create` failed with
/// `Io(Os { code: 122, kind: QuotaExceeded, message: "Quota exceeded" })` -- `EDQUOT` from
/// `ftruncate` to 128 MiB. A genuinely confusing way to run out of room, and one a user's own
/// `TMPDIR` could reproduce.
///
/// `CARGO_TARGET_TMPDIR` is set by cargo for examples and integration tests, points into the
/// workspace's `target/`, and the parent removes the tree after every run.
fn census_scratch() -> PathBuf {
    // The parent names it, so parallel runs cannot collide. See `scratch_for` in `tests/census.rs`:
    // `libtest` shares a pid across its threads, so a pid-derived path is not unique enough, and a
    // shared directory meant one run's `create` read another run's committed plaintext.
    if let Ok(dir) = std::env::var("HOLONOMY_JAIL_CENSUS_SCRATCH") {
        return PathBuf::from(dir);
    }
    // `CARGO_TARGET_TMPDIR` is set for examples at *run* time, not compile time, so it cannot be
    // `env!`-ed. The fallback keeps the "not on a tmpfs" property, which is the property that
    // matters -- it is what stops the run dying with `EDQUOT` for no visible reason.
    let root = std::env::var("CARGO_TARGET_TMPDIR").unwrap_or_else(|_| {
        std::env::temp_dir()
            .join("holonomy-target")
            .to_string_lossy()
            .into_owned()
    });
    PathBuf::from(root).join(format!("census-session-{}", std::process::id()))
}

/// Why stage 4 could not complete.
///
/// Two variants because the two failures have different owners -- the filesystem's and the
/// container's -- and collapsing them loses which half of the boot sequence is broken. No `anyhow`
/// here or anywhere: a seccomp-jailed process gets an error type it can match exhaustively
/// (PROJECT.md §3).
#[derive(Debug)]
enum StageError {
    /// A syscall-backed filesystem operation failed.
    Io(std::io::Error),
    /// The container round trip failed.
    Container(String),
}

impl core::fmt::Display for StageError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "descriptor setup: {e}"),
            Self::Container(e) => f.write_str(e),
        }
    }
}

/// Container create, edit, commit, reopen, read -- the `O_DIRECT` path.
///
/// `pread64`/`pwrite64` at block-aligned offsets, and the `fsync` `commit` issues, are exactly what
/// FR-5.3's seven-syscall list omits and what PROJECT.md §2.6 says has to be measured.
fn container_round_trip(path: &Path) -> Result<holonomy_container::Wavefunction, String> {
    use holonomy_container::Wavefunction;
    let content = b"Holonomy phase 7 census workload.\n".repeat(64);
    let mut container = Wavefunction::create(path, PASSPHRASE, "census", &content, VDF_ITERATIONS)
        .map_err(|e| format!("create the container: {e:?}"))?;
    container
        .write_content(SESSION_TEXT)
        .map_err(|e| format!("write content: {e:?}"))?;
    container
        .set_title("census edited")
        .map_err(|e| format!("set title: {e:?}"))?;
    container.commit().map_err(|e| format!("commit: {e:?}"))?;

    let mut reopened = Wavefunction::open(path, PASSPHRASE, VDF_ITERATIONS)
        .map_err(|e| format!("reopen the container: {e:?}"))?;
    let read = reopened
        .read_content()
        .map_err(|e| format!("read content: {e:?}"))?;
    if read != SESSION_TEXT {
        return Err(format!(
            "the container round trip changed the plaintext: got {:?}, want {SESSION_TEXT:?}",
            String::from_utf8_lossy(&read)
        ));
    }
    Ok(reopened)
}

/// One line of facts, every one of them read back from the kernel rather than assumed.
fn describe(sealed: &Sealed<Descriptors>, allowed: usize) -> String {
    let report = sealed.report();
    let (netns_before, netns_after) = sealed
        .netns_evidence()
        .map_or(("unknown".to_string(), "unknown".to_string()), |(b, a)| {
            (b.to_string(), a.to_string())
        });
    // Byte counts, and `RLIM_INFINITY` spelled out rather than as 18446744073709551615 -- an
    // earlier version of this line rendered `core_still_permitted = None` as "inf", which reads like
    // "still unlimited" when it means the opposite: the seal worked.
    let num = |v: Option<u64>| match v {
        Some(u64::MAX) => "infinity".to_string(),
        None => "unreadable".to_string(),
        Some(n) => n.to_string(),
    };
    // `core_after` is `Limits::core_still_permitted`, which is `None` exactly when the seal took.
    let core_after = match report.limits.core_still_permitted {
        None => "sealed".to_string(),
        Some(v) if v == u64::MAX => "still_unlimited".to_string(),
        Some(v) => format!("still_permitted={v}"),
    };
    format!(
        "actions={} downgraded={} instructions={} allowed={} \
         netns_before={netns_before} netns_after={netns_after} netns_isolated={} \
         netns_route={} via_user_namespace={} mlockall={} mlock_errno={} \
         memlock_soft_before_bytes={} memlock_soft_after_bytes={} memlock_hard_bytes={} \
         core_before={} core_after={core_after} dumpable={} no_new_privs={} aslr={}",
        sealed.filter().action.as_str(),
        sealed.filter().downgraded,
        sealed.filter().instructions,
        allowed,
        sealed.network_isolation().is_isolated(),
        sealed
            .network_isolation()
            .reason()
            .describe()
            .replace(' ', "_"),
        sealed.network_isolation().via_user_namespace(),
        report.all_pages_locked,
        report.mlock_errno,
        num(report.limits.memlock_soft_before),
        num(report.limits.memlock_soft_after),
        num(report.limits.memlock_hard),
        num(report.limits.core_before),
        report
            .limits
            .dumpable
            .map_or_else(|| "unknown".to_string(), |v| v.to_string()),
        report.no_new_privs,
        report
            .aslr
            .map_or_else(|| "unknown".to_string(), |v| v.to_string()),
    )
}

/// Stage 9. Returns; the caller then runs the teardown and `_exit(0)`.
///
/// Errors travel as `Result` and are printed, never through `panic!`. Under the filter a panic is
/// survivable -- `tgkill` is allowlisted so `panic = "abort"` can report itself -- but the panic
/// path formats and allocates, and the entire point of the exercise is that nothing after the filter
/// does either. A `Result` keeps the diagnostic path itself honest.
///
/// **No KDF here.** The session commits through the container handle the boot already opened; see
/// `Descriptors::open` for why it cannot re-derive a key.
fn session_workload(descriptors: &mut Descriptors) -> Result<(), String> {
    descriptors
        .container
        .write_content(b"edited by the session")
        .map_err(|e| format!("session write: {e:?}"))?;
    descriptors
        .container
        .commit()
        .map_err(|e| format!("session commit: {e:?}"))?;
    let read = descriptors
        .container
        .read_content()
        .map_err(|e| format!("session read: {e:?}"))?;
    if read != b"edited by the session" {
        return Err(format!(
            "the session's own commit did not read back: got {:?}",
            String::from_utf8_lossy(&read)
        ));
    }

    edit_geometry_and_render()?;
    event_loop_round_trip(descriptors.output_fd, descriptors.input_fd);
    eprintln!("{WORKLOAD_DONE}");
    Ok(())
}

/// `Editor` typing and undo, the Fenwick geometry, and the surface tree's damage query.
fn edit_geometry_and_render() -> Result<(), String> {
    use holonomy_geometry::{FontMetrics, LineGeometry, LineMetrics};
    use holonomy_render::{DamageRect, DamageTracker, Node, Rect, Style, SurfaceTree};
    use holonomy_text::{Editor, SpanPolicy};

    let mut editor =
        Editor::from_text(b"the quick brown fox\n").map_err(|e| format!("editor: {e:?}"))?;
    for _ in 0..256 {
        editor
            .insert_char(b'x', SpanPolicy::Strict)
            .map_err(|e| format!("type: {e:?}"))?;
    }
    if !editor.can_undo() {
        return Err("typing 256 characters left nothing undoable".into());
    }
    editor.undo().map_err(|e| format!("undo: {e:?}"))?;
    for _ in 0..64 {
        editor
            .backspace()
            .map_err(|e| format!("backspace: {e:?}"))?;
    }
    editor
        .style_range(0, 8, holonomy_text::STYLE_BOLD, 0x0000_00FF)
        .map_err(|e| format!("style: {e:?}"))?;

    let metrics = LineMetrics::from_font(&FontMetrics::INTER, 22);
    let mut geometry = LineGeometry::uniform(512, metrics);
    geometry
        .insert_line(0, metrics, 0)
        .map_err(|e| format!("insert_line: {e:?}"))?;
    geometry.check_invariants();
    if geometry.total_height() == 0 {
        return Err("512 lines of Inter at 22 ppem have zero total height".into());
    }
    if geometry.visible_range(0, 200, 8).is_none() {
        return Err("a 200px viewport over 512 lines should see some".into());
    }

    let mut tree = SurfaceTree::group();
    tree.push(Node::Rect(Rect::new(0, 0, 800, 600, 0xFF00_0000)));
    tree.push(Node::Rect(Rect::new(10, 10, 200, 40, 0xFF00_00FF)));
    let mut damage = DamageTracker::new(800, 600);
    damage.add(DamageRect::new(0, 0, 800, 600));
    if tree.nodes_visited(&damage.bounds(), metrics.height(), 8) == 0 {
        return Err("the surface tree visited no nodes for a full-panel damage rect".into());
    }
    if Style::BOLD.0 != 1 {
        return Err(format!(
            "Style::BOLD is atlas style {}, not 1",
            Style::BOLD.0
        ));
    }
    Ok(())
}

/// `epoll_create1` + `epoll_ctl` + `epoll_wait`, then a `write` through the boot-established fd.
///
/// Nothing here names a path. That is the point: the filter has no `openat`, so every byte that
/// leaves the process has to go out through a descriptor the boot opened.
///
/// Returns `()` rather than `Result`: every assertion here is a hard invariant of the boot itself --
/// a valid epoll fd, a watchable descriptor -- and a failure means the *harness* is broken rather
/// than the workload. `Result` would suggest the workload could recover, which it cannot.
fn event_loop_round_trip(output_fd: i32, input_fd: i32) {
    // SAFETY: `epoll_create1(0)` has no preconditions and returns an fd or -1.
    let epoll = unsafe { libc::epoll_create1(0) };
    assert!(epoll >= 0, "epoll_create1 failed");
    let mut event = libc::epoll_event {
        events: libc::EPOLLIN as u32,
        u64: 0,
    };
    // SAFETY: a valid epoll fd, a valid event pointer, and a valid fd to watch.
    let rc = unsafe { libc::epoll_ctl(epoll, libc::EPOLL_CTL_ADD, input_fd, &mut event) };
    assert_eq!(
        rc,
        0,
        "epoll_ctl(EPOLL_CTL_ADD) on the pipe failed: {}",
        std::io::Error::last_os_error()
    );

    let mut events = [libc::epoll_event { events: 0, u64: 0 }; 4];
    // A zero timeout, in **milliseconds**: musl declares `epoll_wait(fd, ev, max, int timeout)`
    // while glibc's has a `struct timespec *`. An integer is the portable spelling, and it makes
    // the call non-blocking, so the return cannot be perturbed by machine load.
    //
    // SAFETY: a valid epoll fd, an initialised array of 4 events, and a zero timeout.
    let n = unsafe { libc::epoll_wait(epoll, events.as_mut_ptr(), 4, 0) };
    assert_eq!(n, 0, "an empty pipe should produce no events");

    let frame = b"P6\n2 2\n255\n\xff\x00\x00\x00\xff\x00\x00\x00\xff\xff\xff\x00";
    // SAFETY: a valid fd and a valid buffer; the result is checked.
    let written = unsafe { libc::write(output_fd, frame.as_ptr().cast(), frame.len()) };
    assert_eq!(
        written,
        frame.len() as isize,
        "short write to the output fd"
    );
    // SAFETY: as above.
    assert_eq!(unsafe { libc::close(output_fd) }, 0, "close the output fd");
    // SAFETY: as above.
    assert_eq!(unsafe { libc::close(epoll) }, 0, "close the epoll fd");
}
