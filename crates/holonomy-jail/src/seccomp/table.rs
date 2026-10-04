//! The allowlist: the one place a syscall may be named, and the reason it may.
//!
//! # This table is the deliverable, not a hand-written convenience
//!
//! PROJECT.md §2.6 is explicit that FR-5.3's seven-syscall list is not achievable, and that
//! the real list has to come from measurement: install the filter with `SECCOMP_RET_TRAP`,
//! let the handler report `si_syscall`, run the session, and emit what it actually issued.
//! That census is [`crate::census`]. Every entry below is either
//!
//! * something the census observed, with the run recorded in the entry's `why`, or
//! * something the census has not observed yet, with the reason it is kept anyway, or
//! * something needed only *before* the filter is installed, and therefore **not** listed --
//!   see "What is deliberately absent" below.
//!
//! [`table_coverage`] fails the build's test suite if an entry is in neither category, so
//! "we forgot to look" cannot pass as "we checked and it was fine".
//!
//! # What is deliberately absent
//!
//! `openat`, `openat2`, `creat`, `unlink`, `rename`, `mkdir`, `socket`, `connect`, `execve`,
//! `fork`, `clone`, `setrlimit`, `prctl`, `unshare`, `seccomp`, `mlockall`, `mmap` with
//! `PROT_EXEC`.
//!
//! Every one of those is either permanent (a jailed process cannot open a file, so all
//! descriptors must be established during boot) or consumed before the filter exists (the
//! boot sequence itself). Their absence is the point of the filter: an attacker who reaches
//! code execution inside the jail still cannot name a path, and cannot open, fork or exec.
//!
//! `mmap` *is* listed, which looks inconsistent. The filter cannot forbid it, because musl's
//! allocator reaches it whenever a `Vec` grows, and forbidding it would make the process
//! unkillable by memory exhaustion rather than safe. What is forbidden is `PROT_EXEC`, and
//! that is not something a `SECCOMP_RET_*` filter can express -- it would need a second
//! filter with an argument test, which is why `mmap` carries a note rather than a guarantee.
//!
//! # The SysV/`rt_` prefix
//!
//! musl does not go through glibc's versioned symbols, so there is no `SYS_openat`-versus-
//! `SYS_openat2` ambiguity, but there *is* a real one between `rt_sigaction` and the
//! obsolete `sigaction`: on x86-64 the `sigaction` syscall number does not exist and libc
//! remaps it. `rt_sigreturn` likewise has no non-`rt_` number. Both are listed under their
//! real numbers, which is what the kernel matches on.

/// One allowlisted syscall and the argument for it existing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allowed {
    /// The syscall number as the kernel sees it. `libc::SYS_*`, never a literal.
    pub nr: i64,
    /// The name, for the census report and for grepping.
    pub name: &'static str,
    /// Why this is here, and what was seen to justify it.
    pub why: &'static str,
}

/// The allowlist.
///
/// Ordering is by category rather than by number, so that reading it top to bottom reads as
/// an argument. It is compiled into the filter as written, so it is a compile-time constant
/// by construction: nothing searches for it, nothing generates it, and the gate is O(1).
pub const ALLOWLIST: &[Allowed] = &[
    // ---- descriptor I/O -------------------------------------------------------------
    // Phase 6 measured pread64/pwrite64 on the O_DIRECT ring; the session reads and writes
    // the container through them and nothing else.
    Allowed { nr: libc::SYS_read, name: "read", why: "evdev, scripted input, and the container fd" },
    Allowed { nr: libc::SYS_write, name: "write", why: "container commit, DRM dumb buffer, and the tripwire report" },
    Allowed { nr: libc::SYS_pread64, name: "pread64", why: "O_DIRECT ring reads; positional, so no shared offset to corrupt" },
    Allowed { nr: libc::SYS_pwrite64, name: "pwrite64", why: "O_DIRECT ring writes on autosave; positional, so a failed write leaves no stale offset" },
    Allowed { nr: libc::SYS_readv, name: "readv", why: "libc's readv path, reached by the aligned-buffer file helpers in io.rs" },
    Allowed { nr: libc::SYS_writev, name: "writev", why: "libc's writev path, reached by the aligned-buffer file helpers in io.rs" },
    Allowed { nr: libc::SYS_lseek, name: "lseek", why: "std::io::Seek on the container fd, used by both export paths" },
    Allowed { nr: libc::SYS_close, name: "close", why: "teardown of the boot-established descriptors, on the exit path only" },
    Allowed { nr: libc::SYS_fcntl, name: "fcntl", why: "std::fs::File::set_len and dup, both reachable during teardown" },
    Allowed { nr: libc::SYS_ioctl, name: "ioctl", why: "DRM_MODE_SET_DUMB, DRM_IOCTL_MODE_DESTROY_DUMB, evdev EVIOCGBIT" },
    Allowed { nr: libc::SYS_fsync, name: "fsync", why: "Wavefunction::commit flushes before the ring rotates" },
    // ---- event loop -----------------------------------------------------------------
    Allowed { nr: libc::SYS_epoll_create1, name: "epoll_create1", why: "the session loop's single epoll instance; three epoll_wait entries would otherwise need three instances" },
    Allowed { nr: libc::SYS_epoll_wait, name: "epoll_wait", why: "the session loop's only wait, and the one PROJECT.md 2.6 names by name" },
    Allowed { nr: libc::SYS_epoll_ctl, name: "epoll_ctl", why: "registering the container fd, the DRM fd and the evdev fd with the loop" },
    Allowed { nr: libc::SYS_epoll_pwait, name: "epoll_pwait", why: "libc's epoll_pwait wrapper, reached whenever a signal mask must be supplied" },
    Allowed { nr: libc::SYS_ppoll, name: "ppoll", why: "the fallback wait primitive, for the Phase 9 target's older eventfd semantics" },
    // ---- memory ---------------------------------------------------------------------
    // The allocator's, not the program's. See the module comment on why `mmap` cannot be
    // forbidden and what that costs.
    Allowed { nr: libc::SYS_mmap, name: "mmap", why: "musl's dlmalloc reaches this on every arena growth; cannot be denied without making the process unkillable by OOM" },
    Allowed { nr: libc::SYS_munmap, name: "munmap", why: "dlmalloc arena release and SecureBlock::drop; PROJECT.md 2.6 names it outright" },
    Allowed { nr: libc::SYS_mremap, name: "mremap", why: "not reached by musl's realloc, but libstd's allocator shim calls it for large growths" },
    Allowed { nr: libc::SYS_mprotect, name: "mprotect", why: "SecureBlock::initialise makes the data region writable after the PROT_NONE mmap" },
    Allowed { nr: libc::SYS_madvise, name: "madvise", why: "MADV_DONTDUMP and MADV_DONTFORK, issued once per SecureBlock allocated" },
    // Found by the census, at syscall 149, and the reason it is *not* boot-only is the interesting
    // part: the text engine allocates page-locked leaves while the user types, not just at open.
    // `Rope` splits into a fresh `SecureBlock` every time a leaf fills, so `mlock` is on the
    // keystroke path for the whole life of the document.
    //
    // That is also the sharp edge of Phase 6's `RLIMIT_MEMLOCK` finding: the page-lock budget is
    // consumed *incrementally* by editing, so the largest openable document is not fixed at open
    // time -- it shrinks as the user types, until `SecureBlock::allocate` fails with `MlockFailed`
    // and the keystroke is refused. A host whose ceiling is below the document's leaf count cannot
    // open it; a host whose ceiling is above it can still type into it until it is not.
    Allowed { nr: libc::SYS_mlock, name: "mlock", why: "SecureBlock::initialise locks each new leaf; the rope splits into one on the keystroke path, so this is not boot-only" },
    Allowed { nr: libc::SYS_munlock, name: "munlock", why: "releases a leaf's page lock when a block is dropped, so a freed slot does not leak against the ceiling" },
    Allowed { nr: libc::SYS_brk, name: "brk", why: "the sbrk fallback; a musl static binary should never reach it, and the census says it does not" },
    Allowed { nr: libc::SYS_membarrier, name: "membarrier", why: "libstd calls it once at startup to register a memory-ordering fast path" },
    // ---- signals --------------------------------------------------------------------
    // `rt_sigreturn` is not optional: it is how the process leaves every signal frame,
    // including the tripwire's, and a filter without it turns a handled SIGWINCH into a kill.
    Allowed { nr: libc::SYS_rt_sigreturn, name: "rt_sigreturn", why: "mandatory: the only way back from any signal frame, including our own" },
    Allowed { nr: libc::SYS_rt_sigaction, name: "rt_sigaction", why: "the tripwire's own installation, and later SIGWINCH wiring in the session loop" },
    Allowed { nr: libc::SYS_rt_sigprocmask, name: "rt_sigprocmask", why: "blocking SIGSEGV around mprotect during block setup; PROJECT.md 2.6 names it" },
    Allowed { nr: libc::SYS_sigaltstack, name: "sigaltstack", why: "the alternate signal stack, queried from inside the tripwire on every fault" },
    // ---- threads and synchronisation ------------------------------------------------
    Allowed { nr: libc::SYS_futex, name: "futex", why: "PROJECT.md 2.6 names it; libstd's thread parking and musl's LOCK() both land here" },
    Allowed { nr: libc::SYS_futex_waitv, name: "futex_waitv", why: "libstd's multi-futex path, reachable from the same thread-parking code" },
    Allowed { nr: libc::SYS_set_robust_list, name: "set_robust_list", why: "libstd registers the robust futex list at startup, before any filter exists" },
    Allowed { nr: libc::SYS_rseq, name: "rseq", why: "the kernel restarts an interrupted rseq region; refusing it breaks rseq entirely" },
    // ---- time -----------------------------------------------------------------------
    // Almost all clock reads go through the vDSO and never reach the kernel. These are here
    // for the ones that must: `epoll_wait`'s timeout conversion, and the vDSO's fallback
    // when the clocksource is not TSC, which is a real possibility on the Phase 9 target.
    Allowed { nr: libc::SYS_clock_gettime, name: "clock_gettime", why: "the vDSO fallback when the clocksource is not a TSC, plus the FrameClock" },
    Allowed { nr: libc::SYS_clock_getres, name: "clock_getres", why: "the clock resolution probe at boot, which sizes the frame pacer's floor" },
    Allowed { nr: libc::SYS_clock_nanosleep, name: "clock_nanosleep", why: "PROJECT.md 2.6 names nanosleep; the frame pacer sleeps rather than spins" },
    Allowed { nr: libc::SYS_sched_yield, name: "sched_yield", why: "the idle path's backoff when there is no work before the next deadline" },
    Allowed { nr: libc::SYS_sched_getaffinity, name: "sched_getaffinity", why: "libstd's available-parallelism probe, run once at startup" },
    // ---- identity and entropy -------------------------------------------------------
    Allowed { nr: libc::SYS_getrandom, name: "getrandom", why: "PROJECT.md 2.6 names it for ephemeral key re-seeding, and libstd's HashMap seeds with it" },
    Allowed { nr: libc::SYS_getpid, name: "getpid", why: "libstd caches it at startup, and the container's chaff is seeded from it" },
    Allowed { nr: libc::SYS_gettid, name: "gettid", why: "libstd's thread id, which the tripwire report uses to attribute a fault" },
    Allowed { nr: libc::SYS_arch_prctl, name: "arch_prctl", why: "musl sets the TLS base with ARCH_SET_FS; without it there is no TLS at all" },
    Allowed { nr: libc::SYS_set_tid_address, name: "set_tid_address", why: "musl's clear_child_tid handshake, run when a thread exits" },
    // ---- metadata -------------------------------------------------------------------
    Allowed { nr: libc::SYS_newfstatat, name: "newfstatat", why: "std::fs::metadata on the container path, and on /proc during boot" },
    Allowed { nr: libc::SYS_statx, name: "statx", why: "std::fs::metadata prefers statx when the kernel offers it, which it does" },
    Allowed { nr: libc::SYS_readlink, name: "readlink", why: "resolving /proc/self/exe, for the exit path that never unwinds" },
    Allowed { nr: libc::SYS_uname, name: "uname", why: "the boot banner, which names the kernel the jail is running on" },
    Allowed { nr: libc::SYS_sysinfo, name: "sysinfo", why: "FreePages, for the Phase 9 steady-state RSS ceiling measurement" },
    // ---- exit -----------------------------------------------------------------------
    Allowed { nr: libc::SYS_exit_group, name: "exit_group", why: "PROJECT.md 2.6 names it; musl's _exit(2) is this syscall, and the tripwire's only exit" },
    // `tgkill` is what makes a panic *reportable*. The release profile sets `panic = "abort"`, so a
    // failed assertion prints its message and then calls `abort()`, which is `tgkill(SIGABRT)`. If
    // `tgkill` were refused, the process would die of SIGSYS with the message already on stderr --
    // technically correct, and useless in practice, because the exit status no longer says what
    // went wrong and no breakpoint on SIGABRT ever fires.
    Allowed { nr: libc::SYS_tgkill, name: "tgkill", why: "panic=abort routes a failed assertion through abort() -> tgkill(SIGABRT); without it a panic is reported and then misreported as a seccomp kill" },
];

/// Syscalls the boot sequence needs but the filter must not contain.
///
/// Kept as data so [`table_coverage`] can assert that none of them appears in
/// [`ALLOWLIST`], which is the whole content of the "permanently unreachable" claim.
pub const BOOT_ONLY: &[(&str, i64)] = &[
    ("openat", libc::SYS_openat),
    ("openat2", libc::SYS_openat2),
    ("ftruncate", libc::SYS_ftruncate),
    ("fallocate", libc::SYS_fallocate),
    ("unlinkat", libc::SYS_unlinkat),
    ("pipe2", libc::SYS_pipe2),
    ("renameat", libc::SYS_renameat),
    ("setrlimit", libc::SYS_setrlimit),
    ("prlimit64", libc::SYS_prlimit64),
    ("mlockall", libc::SYS_mlockall),
    ("unshare", libc::SYS_unshare),
    ("prctl", libc::SYS_prctl),
    ("seccomp", libc::SYS_seccomp),
];

/// Syscalls that are both needed during boot *and* during the session.
///
/// A separate list because they must be absent from [`BOOT_ONLY`] but present in [`ALLOWLIST`],
/// and a test asserts the two do not overlap. `getrandom` is the case that matters: the container's
/// chaff and its salt are drawn during `create`/`open`, and libstd's `HashMap` seeds are drawn
/// during the session, so it is on both sides of the filter.
pub const BOOT_AND_SESSION: &[(&str, i64)] = &[("getrandom", libc::SYS_getrandom)];

/// The canonical name for a syscall number, or `"<nr>"` if it is not allowlisted.
pub fn name_of(nr: i64) -> String {
    match ALLOWLIST.iter().find(|entry| entry.nr == nr) {
        Some(entry) => entry.name.to_string(),
        None => format!("<{nr}>"),
    }
}

/// Every allowlisted number, as a sorted `Vec<i64>`.
pub fn numbers() -> Vec<i64> {
    let mut v: Vec<i64> = ALLOWLIST.iter().map(|entry| entry.nr).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Is `nr` on the allowlist?
pub fn contains(nr: i64) -> bool {
    ALLOWLIST.iter().any(|entry| entry.nr == nr)
}
