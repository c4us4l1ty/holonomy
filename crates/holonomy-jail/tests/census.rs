//! The seccomp gate: trap-mode census, the derived allowlist, and the same session re-run under
//! `SECCOMP_RET_KILL_PROCESS`.
//!
//! Run with `cargo test -p holonomy-jail`.
//!
//! # What the census is, and why it is iterative
//!
//! PROJECT.md §2.6 asks for the exact set of syscalls a session issues. One pass with
//! `SECCOMP_RET_TRAP` on everything cannot produce it, because a trapped syscall does not execute:
//! the session diverges from real behaviour at the first trap, and everything after it measures a
//! program running on broken syscalls. The convergence argument is in [`holonomy_jail::census`].
//!
//! The short version, which is what these tests rely on: run with a table missing syscalls, let the
//! first missing one trap and be reported, add it, run again. When a run *completes*, the union of
//! everything reported across runs is exactly the set the session issues -- not a superset, because
//! a completed run had every syscall it executed already on the table.
//!
//! The census is a one-off development activity; the table it produced is a `const`. These tests do
//! not regenerate it. They assert two things:
//!
//! 1. **the harness can detect a missing entry** --
//!    [`the_census_finds_a_syscall_the_allowlist_is_missing`] drops one from the table and requires
//!    the census to name it, and
//! 2. **the real table suffices** -- [`the_session_completes_under_kill_process`].
//!
//! (1) is there because of H2's DOCTRINE §4: a harness that cannot demonstrate the failure it
//! detects proves nothing about the passes.
//!
//! # The session lives in an example, not in this file
//!
//! `unshare(CLONE_NEWUSER)` fails with `EINVAL` when more than one thread is alive, and `libtest`
//! runs every test on a spawned thread. **No `#[test]` can isolate the network**, so the session has
//! to be a program whose `main` runs on the only thread in the process -- which is
//! `examples/census_session.rs`, and is the same shape as Phase 8's boot. See
//! [`holonomy_jail::netns`] for the measurement.
//!
//! The example is located relative to this test binary (`deps/census-<hash>` ->
//! `<profile>/examples/census_session`), so nothing has to be built or looked up on disk.
//!
//! # What the workload does and does not cover
//!
//! It runs the real boot sequence -- all nine [`holonomy_jail`] stages through the type-state chain
//! -- and then a session touching every subsystem that exists today: a container commit and read on
//! an `O_DIRECT` fd, `Editor` typing and undo, the Fenwick geometry, the surface tree and damage
//! tracker, and an `epoll_wait` over a pipe followed by a `write` through a boot-established fd.
//!
//! **It is not the Phase 8 session loop.** Phase 8 assembles the real one and re-runs this census
//! against it; `seccomp::table` says so where someone editing the table will read it. What the
//! workload does guarantee is that every subsystem the session is built from is represented, so a
//! Phase 8 addition cannot silently need a syscall nobody measured.

use std::path::{Path, PathBuf};
use std::process::Command;

use holonomy_jail::seccomp::{table, Action};
use holonomy_jail::{AltStack, Enter, JailError, Limits, ALT_STACK_BYTES};

// ---------------------------------------------------------------- the child

/// Prefix of the single facts line the session prints when it completes.
const OK_MARKER: &str = "session.ok ";
/// Printed instead of the facts line when something failed.
const FAIL_MARKER: &str = "session.fail ";
/// Printed by the workload once every phase has run.
const WORKLOAD_DONE: &str = "session: workload complete";
/// Printed by the teardown on its way to `_exit(0)`.
const TEARDOWN_DONE: &str = "holonomy-teardown ";

/// Path to the session program.
///
/// Derived from this test binary's own location: `.../<profile>/deps/census-<hash>` and
/// `.../<profile>/examples/census_session`. Two `parent()`s, then into `examples`. That is
/// cargo's fixed layout for a package's test and example targets, and it means the gate never shells
/// out to cargo or guesses a triple.
fn session_program() -> PathBuf {
    let exe = std::env::current_exe().expect("current test binary");
    let profile_dir = exe
        .parent()
        .and_then(Path::parent)
        .expect(".../<profile>/deps/<bin>");
    let program = profile_dir.join("examples").join("census_session");
    assert!(
        program.is_file(),
        "the census session program is missing at {}; `cargo test` builds examples, so this \
         means the example failed to build",
        program.display()
    );
    program
}

/// A per-run counter, so each spawned session gets its own scratch directory.
///
/// `std::process::id()` is *not* enough: `libtest` runs these tests as threads of one process, so
/// every test in this binary shares a pid. They also run in parallel, and a shared scratch meant
/// one run's `create` read another run's committed plaintext -- reported as
/// `the container round trip changed the plaintext: got "edited by the session"`, which is a
/// spectacularly unhelpful symptom of a directory collision.
static RUN_SEQUENCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// Where one run's child will put its 128 MiB container.
fn scratch_for(sequence: u32) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("census-{}-{sequence}", std::process::id()))
}

struct ChildRun {
    code: Option<i32>,
    stderr: String,
}

impl ChildRun {
    fn facts(&self) -> &str {
        self.stderr
            .lines()
            .find_map(|l| l.strip_prefix(OK_MARKER))
            .unwrap_or_else(|| {
                panic!(
                    "the session printed no facts line, so it did not complete:\n{}",
                    self.stderr
                )
            })
    }

    fn fact(&self, key: &str) -> String {
        self.facts()
            .split_whitespace()
            .find_map(|t| t.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
            .unwrap_or_else(|| panic!("no {key}= in facts: {}", self.facts()))
            .to_string()
    }

    fn flag(&self, key: &str) -> bool {
        match self.fact(key).as_str() {
            "true" => true,
            "false" => false,
            other => panic!("{key}={other} is neither true nor false: {}", self.facts()),
        }
    }

    /// The syscall the census reported trapping, if any.
    fn trapped(&self) -> Option<String> {
        self.stderr
            .lines()
            .find_map(|l| l.strip_prefix("holonomy-census syscall="))
            .and_then(|rest| {
                rest.split_whitespace()
                    .find_map(|t| t.strip_prefix("name=").map(str::to_string))
            })
    }

    fn failure(&self) -> Option<String> {
        self.stderr
            .lines()
            .find_map(|l| l.strip_prefix(FAIL_MARKER.trim()))
            .map(str::to_string)
    }

    fn teardown(&self) -> Option<&str> {
        self.stderr
            .lines()
            .find_map(|l| l.strip_prefix(TEARDOWN_DONE))
    }
}

/// Re-run the session program with `action` and, optionally, an incomplete table.
fn run_child(action: &str, drop: &[&str]) -> ChildRun {
    let sequence = RUN_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let scratch = scratch_for(sequence);
    let out = Command::new(session_program())
        .env("HOLONOMY_JAIL_CENSUS_ACTION", action)
        .env("HOLONOMY_JAIL_CENSUS_DROP", drop.join(","))
        .env("HOLONOMY_JAIL_CENSUS_SCRATCH", &scratch)
        .output()
        .expect("run the census session program");
    // Removed before the assertions run, so a failing test does not leave a 128 MiB container
    // behind for the next one.
    let _ = std::fs::remove_dir_all(&scratch);
    ChildRun {
        code: out.status.code(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

// ---------------------------------------------------------------- the gate

/// PROJECT.md §5 Phase 7's gate, second half: the derived allowlist, then the same session re-run
/// under `SECCOMP_RET_KILL_PROCESS` and asserted to complete.
#[test]
fn the_session_completes_under_kill_process() {
    let run = run_child("kill-process", &[]);
    assert_eq!(
        run.code,
        Some(0),
        "the session must complete with every syscall on the table ({}):\n{}",
        run.failure().unwrap_or_else(|| "no failure marker".into()),
        run.stderr
    );
    assert!(
        run.stderr.contains(WORKLOAD_DONE),
        "the workload did not run to completion:\n{}",
        run.stderr
    );
    assert!(
        !run.flag("downgraded"),
        "SECCOMP_RET_KILL_PROCESS must be the action in force, not a fallback:\n{}",
        run.stderr
    );
    assert_eq!(
        run.fact("allowed"),
        table::ALLOWLIST.len().to_string(),
        "the filter must permit the whole table:\n{}",
        run.stderr
    );
    assert_eq!(
        run.fact("instructions"),
        (6 + 2 * table::ALLOWLIST.len() + 1).to_string(),
        "the compiled program's size is derived from the table, so this catches a stale build:\n{}",
        run.stderr
    );
}

/// The teardown ran, and said what it did.
///
/// The exit code alone would not prove it: `_exit(0)` from a normal `return` looks identical from
/// the outside. The report line is the evidence.
#[test]
fn the_teardown_runs_and_reports_before_exiting() {
    let run = run_child("kill-process", &[]);
    assert_eq!(run.code, Some(0), "\n{}", run.stderr);
    let teardown = run.teardown().unwrap_or_else(|| {
        panic!(
            "the teardown printed nothing, so `_exit(0)` did not come from \
             TeardownPlan::run_and_exit:\n{}",
            run.stderr
        )
    });
    assert!(teardown.contains("actions=3"), "{teardown}");
    assert!(teardown.contains("scrubbed="), "{teardown}");
    assert!(teardown.contains("exit=0"), "{teardown}");
    // And the scrub count has to be real: the alternate signal stack alone is 64 KiB, plus every
    // SecureBlock the boot and the session allocated.
    let scrubbed: u64 = teardown
        .split_whitespace()
        .find_map(|t| t.strip_prefix("scrubbed="))
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("no scrubbed= in {teardown:?}"));
    assert!(
        scrubbed >= ALT_STACK_BYTES as u64,
        "the teardown scrubbed {scrubbed} bytes, less than the {ALT_STACK_BYTES}-byte alternate \
         signal stack alone"
    );
}

/// The census harness detects a missing entry.
///
/// Drops `getrandom` and requires the trap-mode run to halt at 90 naming exactly that syscall.
/// Without this, `the_session_completes_under_kill_process` could be passing because the census
/// never worked.
#[test]
fn the_census_finds_a_syscall_the_allowlist_is_missing() {
    let run = run_child("trap", &["getrandom"]);
    assert_eq!(
        run.code,
        Some(holonomy_jail::CENSUS_EXIT),
        "a trap-mode run with a missing entry must halt at 90:\n{}",
        run.stderr
    );
    assert_eq!(
        run.trapped().as_deref(),
        Some("getrandom"),
        "the census must name the syscall that is missing:\n{}",
        run.stderr
    );
    assert!(
        run.stderr.contains("first=yes"),
        "the first trap in a fresh process is always a new syscall:\n{}",
        run.stderr
    );
}

/// A syscall the filter refuses kills the process, with no exit code at all.
///
/// `Command::status` exposes a signal death as `code() == None`; it does not surface *which* signal,
/// so this asserts the observable half. That the signal is `SIGSYS` rather than `SIGKILL` is what
/// `SECCOMP_RET_KILL_PROCESS` buys over `SECCOMP_RET_KILL`, and `downgraded=false` in the boot
/// report is what says the strong action was really in force.
#[test]
fn an_unlisted_syscall_kills_the_process() {
    let run = run_child("kill-process", &["write", "fsync"]);
    assert_eq!(
        run.code, None,
        "the child must die from a signal rather than exit:\n{}",
        run.stderr
    );
    assert!(
        !run.stderr.contains(WORKLOAD_DONE),
        "the session must not have finished:\n{}",
        run.stderr
    );
    assert!(
        !run.stderr.contains(OK_MARKER.trim()),
        "a session the filter killed must not print facts:\n{}",
        run.stderr
    );
    assert!(
        !run.stderr.contains("holonomy-census"),
        "there is no census handler in production mode; the kill must come from the filter:\n{}",
        run.stderr
    );
}

/// The boot sequence's evidence, read back rather than assumed.
#[test]
fn the_boot_sequence_reports_its_evidence() {
    let run = run_child("kill-process", &[]);
    assert_eq!(run.code, Some(0), "\n{}", run.stderr);

    let before: u64 = run.fact("netns_before").parse().expect("before inode");
    let after: u64 = run.fact("netns_after").parse().expect("after inode");
    assert_ne!(
        before,
        after,
        "unshare(CLONE_NEWNET) did not change the network namespace inode:\n{}",
        run.facts()
    );
    assert!(run.flag("netns_isolated"), "\n{}", run.facts());
    assert!(run.flag("no_new_privs"), "\n{}", run.facts());
    assert_eq!(run.fact("dumpable"), "0", "\n{}", run.facts());
    assert_eq!(
        run.fact("core_after"),
        "sealed",
        "RLIMIT_CORE must be 0 once sealed; `core_before` is whatever the session inherited:\n{}",
        run.facts()
    );
    assert!(
        run.flag("mlockall"),
        "mlockall must have succeeded, because the KDF has already released its 128 MiB by now \
         and the session's steady state is well under the ceiling:\n{}",
        run.facts()
    );
    assert_eq!(run.fact("mlock_errno"), "0", "\n{}", run.facts());
}

/// `mlockall` succeeded *because* the KDF ran first, and it is the difference between working and
/// not.
///
/// Two claims, one number apart. `Descriptors::open` documents the mechanism: `MCL_FUTURE` makes
/// the kernel refuse an `mmap` once `can_do_mlock()` finds the `RLIMIT_MEMLOCK` budget spent, and
/// Argon2id's working buffer is 128 MiB against a 8 MiB ceiling. The session, by contrast, allocates
/// only page-locked `SecureBlock` leaves, which are a few KiB each.
#[test]
fn the_kdf_cannot_run_after_mlockall_but_the_session_can() {
    let mut limits = Limits::default();
    holonomy_jail::rlimits::raise_memlock_to_hard_limit(&mut limits);
    let ceiling = limits.memlock_hard.expect("RLIMIT_MEMLOCK");
    let argon2_peak = u64::from(holonomy_crypto::envelope::ARGON2_M_COST_KIB) * 1024;
    println!(
        "RLIMIT_MEMLOCK ceiling {} KiB vs Argon2id peak {} KiB ({:.1}x)",
        ceiling / 1024,
        argon2_peak / 1024,
        argon2_peak as f64 / ceiling as f64
    );

    if argon2_peak <= ceiling {
        eprintln!(
            "RLIMIT_MEMLOCK ({ceiling} KiB) covers Argon2id ({argon2_peak} KiB); the ordering \
             constraint is satisfied on this host and the probe is skipped"
        );
        return;
    }

    // Prove the causal claim rather than only the arithmetic.
    //
    // **`MCL_FUTURE` alone**, not `MCL_CURRENT | MCL_FUTURE`, and that is not a detail:
    // `MCL_CURRENT` has to page-lock *everything this process already has resident*, and a
    // `cargo test` binary is bigger than the 8 MiB ceiling -- so the combined call fails with
    // `ENOMEM` before it ever reaches the mechanism under test. `MCL_FUTURE` is what marks new
    // mappings `VM_LOCKED`, which is the whole cause, and it costs nothing on pages that are not
    // already mapped.
    //
    // (The session program uses both, and succeeds: it is a small process, and it reaches stage 5
    // with Argon2's 128 MiB already freed.)
    // SAFETY: a no-argument call with no preconditions.
    assert_eq!(
        unsafe { libc::mlockall(libc::MCL_FUTURE) },
        0,
        "mlockall(MCL_FUTURE) must succeed in a process this small"
    );
    let peak = holonomy_crypto::envelope::ARGON2_M_COST_KIB as usize * 1024;
    // SAFETY: `PROT_NONE`, so nothing is written; the length is used only for the size accounting
    // the kernel performs. `MAP_FAILED` is the expected outcome.
    let probe = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            peak,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    assert_eq!(
        probe,
        libc::MAP_FAILED,
        "an {peak}-byte mapping succeeded under mlockall(MCL_FUTURE) with a {ceiling} KiB \
         RLIMIT_MEMLOCK; if so the boot order stops being load-bearing and the KDF could move \
         back into the session"
    );
    // The *refusal* is the claim; the exact `errno` is the kernel's business and is not pinned.
    // Measured here: `EAGAIN`. An earlier version asserted `EPERM` on the reasoning that
    // `can_do_mlock` returning false is an `EPERM`, and it failed. Pinning one value would assert a
    // detail about the kernel's error path that has nothing to do with the ordering constraint, and
    // would break on a kernel that picks a third value for the same refusal.
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    assert!(
        matches!(errno, libc::EAGAIN | libc::ENOMEM | libc::EPERM),
        "a refused VM_LOCKED mapping should report one of EAGAIN/ENOMEM/EPERM; got errno {errno} \
         ({})",
        std::io::Error::from_raw_os_error(errno)
    );
    println!(
        "a {peak}-byte VM_LOCKED mapping under a {} KiB limit is refused: errno {errno}",
        ceiling / 1024
    );
    // SAFETY: no arguments, no preconditions.
    assert_eq!(unsafe { libc::munlockall() }, 0);

    // And the session does complete under the same limit, which is the other half of the claim.
    let run = run_child("kill-process", &[]);
    assert_eq!(run.code, Some(0), "\n{}", run.stderr);
}

/// The page-lock ceiling is reported, not assumed.
///
/// Phase 6's finding: this host's soft and hard `RLIMIT_MEMLOCK` are both 8,192 KiB, so raising the
/// soft limit to the hard limit is a no-op and a full 6.40 MiB document has only 1.17x headroom. The
/// report has to distinguish "raised it" from "it was already at the ceiling", because the two imply
/// very different things.
#[test]
fn the_page_lock_ceiling_is_reported_honestly() {
    let run = run_child("kill-process", &[]);
    assert_eq!(run.code, Some(0), "\n{}", run.stderr);

    let parse = |key: &str| -> u64 {
        let raw = run.fact(key);
        assert_ne!(
            raw, "inf",
            "{key}=inf: RLIMIT_MEMLOCK is not a sentinel, it is a byte count"
        );
        raw.parse()
            .unwrap_or_else(|_| panic!("{key}={raw} is not a number"))
    };
    let soft_before = parse("memlock_soft_before_bytes");
    let soft_after = parse("memlock_soft_after_bytes");
    let hard = parse("memlock_hard_bytes");
    assert!(soft_before <= hard, "soft {soft_before} > hard {hard}");
    assert!(
        soft_after <= hard,
        "the bump overshot: {soft_after} > {hard}"
    );
    assert_eq!(
        soft_after, hard,
        "the soft limit should end at the hard limit"
    );
    assert!(
        soft_after > 0,
        "a ceiling of 0 means nothing can be page-locked"
    );
    // Reported in KiB because that is the unit `ulimit -l` uses; `getrlimit` answers in bytes.
    println!(
        "RLIMIT_MEMLOCK: soft {} -> {} KiB (hard {} KiB)",
        soft_before / 1024,
        soft_after / 1024,
        hard / 1024
    );
}

/// The table is well formed and self-describing.
///
/// Cheap, and the things that actually go wrong when a table is edited by hand: a duplicated
/// number, a boot-only syscall that crept in, an entry with no stated reason.
#[test]
fn the_allowlist_is_well_formed() {
    let numbers = table::numbers();
    assert_eq!(
        numbers.len(),
        table::ALLOWLIST.len(),
        "the table has a duplicate syscall number: {} entries, {} distinct",
        table::ALLOWLIST.len(),
        numbers.len()
    );
    for (name, nr) in table::BOOT_ONLY {
        assert!(
            !table::contains(*nr),
            "{name} is boot-only and must never reach the allowlist: a jailed process that can \
             still open a file is not jailed"
        );
        assert!(
            !table::ALLOWLIST.iter().any(|e| e.name == *name),
            "{name} appears in both BOOT_ONLY and ALLOWLIST"
        );
    }
    // The dual-role list must be *in* the allowlist, or the session loses a syscall the boot needs.
    for (name, nr) in table::BOOT_AND_SESSION {
        assert!(
            table::contains(*nr),
            "{name} is needed both before and after the filter and must be allowlisted"
        );
        assert!(
            !table::BOOT_ONLY.iter().any(|(n, _)| n == name),
            "{name} is on both BOOT_ONLY and BOOT_AND_SESSION"
        );
    }
    for entry in table::ALLOWLIST {
        assert!(
            entry.nr >= 0,
            "{}: syscall numbers are non-negative",
            entry.name
        );
        assert!(
            entry.why.trim().len() > 20,
            "{}: every entry must carry a real reason, not a word",
            entry.name
        );
        assert!(
            !entry.name.contains(','),
            "{}: names are comma-separated in the child's DROP variable",
            entry.name
        );
    }
}

/// `mlock` is on the keystroke path, not only at boot.
///
/// Found by the census at syscall 149. The rope splits into a fresh page-locked `SecureBlock` every
/// time a leaf fills, so the page-lock budget is consumed *incrementally by editing* -- which means
/// the largest document a host can hold shrinks as the user types, until
/// `SecureBlock::allocate` fails with `MlockFailed` and the keystroke is refused. Phase 6 measured
/// the ceiling (1.17x for a full 6.40 MiB document); this asserts that the path which spends it is
/// the one the allowlist permits.
#[test]
fn mlock_is_allowlisted_because_the_rope_splits_at_runtime() {
    assert!(
        table::contains(libc::SYS_mlock),
        "mlock must be allowlisted: SecureBlock::allocate locks each new rope leaf"
    );
    assert!(
        table::contains(libc::SYS_munlock),
        "munlock must be allowlisted so a dropped leaf does not leak against the ceiling"
    );
    assert!(
        table::BOOT_ONLY.iter().all(|(n, _)| *n != "mlock"),
        "mlock is not boot-only, which is the whole reason the census found it"
    );
}

// ---------------------------------------------------------------- in-process checks

/// The boot chain refuses the wrong alternate stack, and says which precondition failed.
#[test]
fn the_boot_chain_refuses_a_stack_that_is_not_registered() {
    let mut first = vec![0u8; ALT_STACK_BYTES];
    let stale = AltStack::install(first.as_mut_ptr() as usize, ALT_STACK_BYTES).expect("install");
    let mut second = vec![0u8; ALT_STACK_BYTES];
    let registered =
        AltStack::install(second.as_mut_ptr() as usize, ALT_STACK_BYTES).expect("install");
    assert_ne!(stale, registered);

    let err = Enter
        .seal_core_dumps()
        .raise_memlock()
        .install_tripwires(stale)
        .expect_err("must refuse a stack the kernel does not have registered");
    assert_eq!(
        err,
        JailError::Tripwire(holonomy_jail::TripwireError::WrongAltStack)
    );
    let text = format!("{err}");
    assert!(text.contains("different alternate signal stack"), "{text}");

    // Unregister, and the *absence* is a different condition with a different fix.
    assert!(AltStack::disable());
    let err = Enter
        .seal_core_dumps()
        .raise_memlock()
        .install_tripwires(registered)
        .expect_err("must refuse with nothing registered");
    assert_eq!(
        err,
        JailError::Tripwire(holonomy_jail::TripwireError::NoAltStack)
    );
    assert!(!holonomy_jail::tripwire::is_installed());
    AltStack::install(second.as_mut_ptr() as usize, ALT_STACK_BYTES).expect("restore");
}

/// No unit test in this binary may install a filter: a filter cannot be removed.
#[test]
fn no_filter_is_installed_in_this_process() {
    assert_eq!(
        holonomy_jail::seccomp::filters_installed(),
        0,
        "a seccomp filter was installed in the parent test process, which would seal every later \
         test in this binary"
    );
}

/// Installing a filter without `PR_SET_NO_NEW_PRIVS` is refused, for the right reason.
#[test]
fn installing_without_no_new_privs_is_refused_with_its_own_error() {
    let program =
        holonomy_jail::Program::build(Action::KillProcess, table::ALLOWLIST).expect("build");
    if holonomy_jail::seccomp::new_privs_is_set() {
        eprintln!("PR_SET_NO_NEW_PRIVS is already set here; the assertion is vacuous");
        return;
    }
    assert_eq!(
        program.install().err(),
        Some(holonomy_jail::SeccompError::NoNewPrivsRequired),
        "the refusal must name the missing precondition, not report a rejected program"
    );
}

/// An empty table still assembles, and permits nothing.
///
/// `seal_with` is where the refusal lives, because a filter that permits nothing is never what a
/// caller means and it produces a death that looks exactly like a filter bug. The program itself
/// builds -- six prologue instructions and an epilogue -- and the refusal is one layer up, where the
/// intent is visible.
#[test]
fn an_empty_table_assembles_to_a_prologue_and_an_epilogue() {
    let program = holonomy_jail::Program::build(Action::KillProcess, &[]).expect("build");
    assert_eq!(
        program.len(),
        7,
        "6 prologue instructions plus 1 epilogue, with no entries between them"
    );
    assert!(
        format!("{}", holonomy_jail::SeccompError::EmptyTable).contains("permit nothing"),
        "the refusal must say what is wrong with an empty table"
    );
}

/// `JailError` names its stage, and `Limits` says what the host allowed.
#[test]
fn a_jail_error_names_its_stage() {
    assert!(format!("{}", JailError::MlockAll(libc::EPERM)).contains("swap"));
    assert!(format!("{}", JailError::NoNewPrivs(libc::EINVAL)).contains("NO_NEW_PRIVS"));
    assert!(format!(
        "{}",
        JailError::Seccomp(holonomy_jail::SeccompError::ThreadOutOfSync(libc::EAGAIN))
    )
    .contains("second thread"));
    assert!(format!(
        "{}",
        JailError::Seccomp(holonomy_jail::SeccompError::ProgramRejected(libc::EINVAL))
    )
    .contains("program itself"));
    assert!(format!(
        "{}",
        JailError::Seccomp(holonomy_jail::SeccompError::EmptyTable)
    )
    .contains("permit nothing"));

    let mut limits = Limits::default();
    holonomy_jail::rlimits::raise_memlock_to_hard_limit(&mut limits);
    assert!(limits.memlock_hard.is_some(), "getrlimit failed");
    assert!(limits.memlock_soft_after.unwrap() <= limits.memlock_hard.unwrap());
}

/// The scratch directory is the workspace's, not `/tmp`.
///
/// Asserted because the alternative is a failure mode with no resemblance to its cause: 128 MiB
/// containers per run accumulating in a tmpfs until `ftruncate` returns `EDQUOT`.
#[test]
fn the_census_scratch_is_not_tmp() {
    for sequence in [0, 1] {
        let scratch = scratch_for(sequence);
        assert!(
            !scratch.starts_with("/tmp"),
            "a 128 MiB container per run does not belong on a tmpfs: got {}",
            scratch.display()
        );
        assert!(
            scratch.starts_with(std::env::temp_dir().join("holonomy-target"))
                || scratch.to_string_lossy().contains("target"),
            "the scratch must live under the workspace target directory, got {}",
            scratch.display()
        );
    }
    assert_ne!(
        scratch_for(0),
        scratch_for(1),
        "each run needs its own scratch: libtest shares a pid across its threads"
    );
}
