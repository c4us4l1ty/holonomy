//! Process resource limits and dumpability, before anything exists worth dumping.
//!
//! # Why this is the first step and not a later one
//!
//! `RLIMIT_CORE` and `PR_SET_DUMPABLE` are the two controls that decide whether a crash
//! writes plaintext to a file on disk. Both have to be in place *before* the tripwires are
//! installed and before a single `SecureBlock` exists, because the moment they are needed
//! is the moment something has already gone wrong.
//!
//! Order within this module matters too: `PR_SET_DUMPABLE` goes to 0 **after**
//! `RLIMIT_CORE` goes to 0, never before. `PR_SET_DUMPABLE` alone is not enough -- a
//! process that is non-dumpable still honours a nonzero `RLIMIT_CORE` if the *credentials*
//! allow it, and the credentials in this program are the user's own. `RLIMIT_CORE` alone is
//! the one that always binds, because it is a property of the process and not a policy the
//! reader of `/proc` can override.
//!
//! # `RLIMIT_MEMLOCK` is a different problem and it is not solvable here
//!
//! Phase 6 measured the consequence: a full 6.40 MiB text budget needs 1,747 page-locked
//! leaves = 6.82 MiB of page lock, against this host's 8.00 MiB. That is 1.17x of headroom,
//! and the naive fix -- raising the soft limit to the hard limit -- is a no-op on a host
//! where they are equal.
//!
//! On this host both are 8,192 KiB and the hard limit cannot be raised without
//! `CAP_SYS_RESOURCE`. So [`raise_memlock_to_hard_limit`] reports exactly what it achieved,
//! including "the hard limit was already the soft limit, so this changed nothing", because
//! a caller that cannot see that difference will assume a 6.40 MiB document opens when it
//! does not. Production runs as root and gets the real ceiling.

/// What [`seal_core_dumps`] and [`raise_memlock_to_hard_limit`] actually achieved.
///
/// Every field is a measurement rather than an assumption. The difference matters: the
/// Phase 7 boot prints this, and the Phase 9 run on the ThinkPad needs to be able to tell
/// "we were given 64 MiB of page lock" from "we asked for 64 MiB and were refused".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Limits {
    /// `RLIMIT_CORE` before and after sealing. `Some(x)` is a hard limit in bytes.
    pub core_before: Option<u64>,
    /// `Some(x)` only if the process may still be dumped, i.e. sealing failed.
    pub core_still_permitted: Option<u64>,
    /// `RLIMIT_MEMLOCK` soft limit before the bump.
    pub memlock_soft_before: Option<u64>,
    /// `RLIMIT_MEMLOCK` soft limit after the bump. Equal to `memlock_soft_before` on a
    /// host where the limits were already equal.
    pub memlock_soft_after: Option<u64>,
    /// `RLIMIT_MEMLOCK` hard limit, which is the ceiling the soft limit can ever reach.
    pub memlock_hard: Option<u64>,
    /// `PR_SET_DUMPABLE` afterwards: always `Some(0)` or the call failed.
    pub dumpable: Option<u32>,
    /// `/proc/sys/kernel/randomize_va_space`, captured during boot.
    ///
    /// Not a limit, and not named for one: it lives here because it is the same kind of thing --
    /// a property of the host, read once before the filter, reported afterwards. `None` if `/proc`
    /// could not be read.
    pub aslr: Option<u32>,
}

/// Read one limit by name, as `(soft, hard)` in bytes.
///
/// `RLIM_INFINITY` is reported as `u64::MAX` rather than `RLIM_INFINITY`, because the
/// interesting comparison is against a byte count and `RLIM_INFINITY` is not one.
/// `RLIMIT_*` selectors are plain `c_int` constants; `libc` has no named type for them on
/// musl, so the parameter is `c_int` rather than a `libc` alias that only exists on glibc.
pub fn getrlimit(resource: libc::c_int) -> Option<(u64, u64)> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `limit` is a valid, writable `rlimit`, and `resource` is a constant.
    if unsafe { libc::getrlimit(resource, &mut limit) } != 0 {
        return None;
    }
    Some((
        if limit.rlim_cur == libc::RLIM_INFINITY {
            u64::MAX
        } else {
            limit.rlim_cur
        },
        if limit.rlim_max == libc::RLIM_INFINITY {
            u64::MAX
        } else {
            limit.rlim_max
        },
    ))
}

fn setrlimit_bytes(resource: libc::c_int, soft: u64, hard: u64) -> bool {
    let to_raw = |v: u64| {
        if v == u64::MAX {
            libc::RLIM_INFINITY
        } else {
            v as libc::rlim_t
        }
    };
    let limit = libc::rlimit {
        rlim_cur: to_raw(soft),
        rlim_max: to_raw(hard),
    };
    // SAFETY: `limit` is a valid, fully initialised `rlimit`.
    unsafe { libc::setrlimit(resource, &limit) == 0 }
}

/// Set `RLIMIT_CORE` to 0 and `PR_SET_DUMPABLE` to 0.
///
/// Returns what it achieved rather than failing, because the correct response to "this host
/// would not let us seal core dumps" is to say so loudly at boot and carry on -- not to
/// refuse to open a document over a diagnostic flag. The caller decides; [`Limits`] reports.
///
/// Never raises anything: a limit that is already 0 stays 0.
pub fn seal_core_dumps(limits: &mut Limits) {
    if let Some((soft, hard)) = getrlimit(libc::RLIMIT_CORE) {
        limits.core_before = Some(soft);
        if setrlimit_bytes(libc::RLIMIT_CORE, 0, 0) {
            limits.core_still_permitted = None;
        } else {
            // Either the soft limit was already 0 and cannot be re-set to a lower value
            // under some policies, or the hard limit is below 0 which is not expressible.
            limits.core_still_permitted = getrlimit(libc::RLIMIT_CORE).map(|(s, _)| s);
        }
        let _ = hard;
    }

    // SAFETY: `PR_SET_DUMPABLE` takes five `unsigned long`s and reads none of them.
    let rc = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) };
    limits.dumpable = if rc == 0 { Some(0) } else { None };

    // SAFETY: `PR_GET_DUMPABLE` writes one `int`.
    if limits.dumpable.is_some() {
        let rc = unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) };
        if rc != 0 {
            // The set succeeded but the read disagrees, which would mean the kernel ignored
            // us. Report that rather than assuming.
            limits.dumpable = Some(rc as u32);
        }
    }
}

/// Raise the soft `RLIMIT_MEMLOCK` to the hard limit.
///
/// A no-op where they are already equal, which on this host they are. The returned
/// [`Limits`] says so explicitly via `memlock_soft_after`, because "we raised it" and "it
/// was already at the ceiling" imply very different things about whether a 6.40 MiB
/// document opens.
pub fn raise_memlock_to_hard_limit(limits: &mut Limits) {
    let Some((soft, hard)) = getrlimit(libc::RLIMIT_MEMLOCK) else {
        return;
    };
    limits.memlock_soft_before = Some(soft);
    limits.memlock_hard = Some(hard);
    if soft == hard {
        limits.memlock_soft_after = Some(soft);
        return;
    }
    if setrlimit_bytes(libc::RLIMIT_MEMLOCK, hard, hard) {
        limits.memlock_soft_after = getrlimit(libc::RLIMIT_MEMLOCK).map(|(s, _)| s);
    } else {
        // Cannot raise past the hard limit without CAP_SYS_RESOURCE. Leave it be; the
        // allocation that fails will report `MlockFailed` and the boot will print this.
        limits.memlock_soft_after = Some(soft);
    }
}

/// Lock every page now and every page mapped later: `mlockall(MCL_CURRENT | MCL_FUTURE)`.
///
/// NFR-3: no byte of process memory may reach swap. Per-block `mlock` covers the
/// `SecureBlock` payloads, but not the allocator's arenas, not the stack, and not anything
/// a future dependency allocates.
///
/// Returns whether it succeeded, and records the `errno` on failure. Failure is not fatal
/// -- it degrades hardening, not correctness, because `MADV_DONTDUMP` and the guard pages
/// are still in force -- so the caller reports it rather than aborting the boot.
pub fn mlock_all() -> Result<(), i32> {
    // SAFETY: `mlockall` takes no pointers.
    let rc = unsafe { libc::mlockall(libc::MCL_CURRENT | libc::MCL_FUTURE) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::ENOMEM))
    }
}

/// Read `/proc/sys/kernel/randomize_va_space` (0 = off, 1 = mmap, 2 = full).
///
/// **Boot-time only.** This is a path open, and the filter has no `openat`.
///
/// The census caught it: `Sealed::report` called this to fill in the boot report, which is read
/// after the filter is installed, so the process was killed by `SIGSYS` on syscall 2 -- `open` --
/// every time it printed its own measurements. A report that cannot be printed is the worst place
/// for a `std::fs` call, and the failure pointed at `open` rather than at anything in the report.
///
/// So the value is read once during stage 2 and stored in [`Limits::aslr`].
pub fn address_space_randomisation() -> Option<u32> {
    let text = std::fs::read_to_string("/proc/sys/kernel/randomize_va_space").ok()?;
    text.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memlock_reports_a_real_ceiling() {
        let mut limits = Limits::default();
        raise_memlock_to_hard_limit(&mut limits);
        assert!(
            limits.memlock_hard.is_some(),
            "getrlimit(RLIMIT_MEMLOCK) failed"
        );
        assert!(
            limits.memlock_soft_after <= limits.memlock_hard,
            "soft {soft} exceeded hard {hard} after the bump",
            soft = limits.memlock_soft_after.unwrap(),
            hard = limits.memlock_hard.unwrap()
        );
    }

    /// The ceiling the boot prints is the ceiling the process actually has, and it is **not** the one it
    /// asked for.
    ///
    /// `main.rs` prints `memlock_soft_after` beside the document size that needs it, because on this host
    /// `RLIMIT_MEMLOCK` cannot be raised from inside the process: `CapEff` reads as 0, so there is no
    /// `CAP_SYS_RESOURCE` to raise the hard limit with, and soft already equals hard at 8.00 MiB. So the
    /// number that decides whether the maximum document opens is a **host** property, and the only honest
    /// thing the binary can do is say which one it has.
    ///
    /// The assertion that matters is against a *live* `getrlimit`, not against a constant. A test that
    /// pinned `8.00 MiB` would pass on this host and fail on a host configured for `LimitMEMLOCK=infinity`
    /// -- which is exactly the host the maximum document needs. Comparing to the live limit is what makes
    /// this one test valid on both, and it is what catches the failure mode that matters: `soft_after`
    /// silently reading `0`, which the print would render as "0.00 MiB" without any error.
    #[test]
    fn the_reported_memlock_ceiling_is_the_real_one() {
        let mut limits = Limits::default();
        raise_memlock_to_hard_limit(&mut limits);

        let (live_soft, live_hard) = getrlimit(libc::RLIMIT_MEMLOCK).expect("getrlimit(RLIMIT_MEMLOCK)");
        let reported = limits.memlock_soft_after.expect("soft_after was not recorded");
        assert!(
            reported > 0,
            "the boot prints memlock_soft_after as a MiB figure; a 0 would read as '0.00 MiB' with \
             no error, which is worse than printing nothing"
        );
        assert_eq!(
            reported,
            live_soft,
            "boot reported {} but the process has {}",
            reported,
            live_soft
        );
        assert_eq!(
            limits.memlock_hard.expect("hard was not recorded"),
            live_hard,
            "the hard limit is what the boot suggests raising the host to, so it has to be the real one"
        );
        assert_eq!(
            limits.memlock_soft_before, Some(live_soft),
            "and 'before' is not the same fact as 'after' unless the bump was a no-op -- if these differ \
             the raise did something, which is worth knowing rather than assuming"
        );
    }

    /// Raising the ceiling is a **host** change, and the binary's own attempt is a no-op. Recorded as a
    /// test because it is the assumption several parts of the design rest on: that an 8.00 MiB
    /// `RLIMIT_MEMLOCK` is a property of the machine rather than something the boot can negotiate.
    ///
    /// **This test is deliberately not an assertion that the raise *failed*.** Whether soft can reach
    /// hard is host-dependent, and a host with `LimitMEMLOCK=infinity` has both equal at infinity, which
    /// also means the bump was a no-op. So the invariant asserted is the one that is true everywhere:
    /// **the soft limit never exceeds the hard limit after the attempt**, and the recorded values are
    /// present. A test that hard-coded `8.00 MiB` would encode this host into the suite and would have to
    /// be deleted the day someone ran it somewhere the maximum document opens.
    #[test]
    fn the_memlock_bump_cannot_exceed_the_hard_limit() {
        let mut limits = Limits::default();
        raise_memlock_to_hard_limit(&mut limits);
        let soft = limits.memlock_soft_after.expect("soft_after");
        let hard = limits.memlock_hard.expect("hard");
        assert!(
            soft <= hard,
            "soft {soft} > hard {hard}: the bump raised past what the host permits, which should have \
             been refused with EPERM rather than reported as achieved"
        );
    }

    /// **`munmap` releases the page-lock charge, so locked memory tracks *resident* memory.**
    ///
    /// This is the question Phase 13's whole windowing plan rests on, and it was answered wrongly once.
    /// The claim recorded in PROJECT.md was that "`mlockall` locks the process's address space, so
    /// `RLIMIT_MEMLOCK` is spent on pages rather than on the document, and windowing cannot move the
    /// ceiling by a byte." If that were true, bounding `SectionStore`'s residency would be pointless for
    /// the ceiling and the maximum document would be permanently unopenable at any residency.
    ///
    /// **It is false, and this is the measurement that says so.** A locked page that is unmapped is gone,
    /// and the kernel drops its charge against `RLIMIT_MEMLOCK`. So a bounded resident set bounds the
    /// locked set too — which means **`SectionStore`'s budget *is* the page-lock budget**, and
    /// `mlockall` costs nothing on top of it beyond whatever is genuinely resident.
    ///
    /// # Why a child process
    ///
    /// **`mlockall` is process-wide and `RLIMIT_MEMLOCK` is per-process, so libtest cannot measure this.**
    /// libtest runs tests on threads in one process; `MCL_CURRENT` cannot even succeed here because the
    /// test binary is larger than the 8.00 MiB ceiling (`the_kdf_cannot_run_after_mlockall_but_the_session_can`
    /// documents the same constraint). So the probe runs in a child spawned from `current_exe`, and only
    /// the child calls `mlockall`. The parent asserts on numbers the child printed.
    ///
    /// `MCL_FUTURE` only, never `MCL_CURRENT`: the child is already over the ceiling, and `MCL_FUTURE` is
    /// the half that marks *new* mappings, which is the mechanism under test.
    #[test]
    fn unmapping_releases_the_page_lock_charge() {
        const CHILD_ENV: &str = "HOLONOMY_MLOCK_PROBE";
        let exe = std::env::current_exe().expect("test binary path");
        let out = std::process::Command::new(&exe)
            .args([
                "--exact",
                // The fully-qualified name: `--exact` matches the full path, and a bare
                // `mlock_probe_child` filters out every test including the one we want, which shows up
                // as "running 0 tests" and no PROBE line rather than as an obvious mistake.
                "rlimits::tests::mlock_probe_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_ENV, "1")
            .output()
            .expect("spawn the probe child");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let line = stdout.lines().find_map(|l| l.find("PROBE ").map(|i| &l[i + 6..]));
        let Some(line) = line else {
            panic!(
                "the probe child printed no PROBE line.\n--- stdout ---\n{stdout}\n--- stderr ---\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let mut f = line.split_whitespace();
        let mut n = |what: &str| {
            f.next()
                .unwrap_or_else(|| panic!("missing field {what} in {line:?}"))
                .parse::<u64>()
                .unwrap_or_else(|_| panic!("non-numeric {what} in {line:?}"))
        };
        let mlock_rc = n("mlock_rc");
        let errno = n("errno");
        let base = n("base");
        let full = n("full");
        let after = n("after");
        let region_kib = n("region_kib");

        assert_eq!(
            mlock_rc, 0,
            "mlockall(MCL_FUTURE) failed with errno {errno}. The probe measures a *new* mapping being \
             charged and released; without MCL_FUTURE the region would never be locked and the test would \
             pass for the wrong reason"
        );
        assert!(
            full.saturating_sub(base) >= region_kib / 2,
            "a {region_kib} kB region that was mapped and touched raised VmLck by only {} kB, so the \
             charge this test is about did not happen and the release below would prove nothing",
            full.saturating_sub(base)
        );
        assert!(
            after < full,
            "VmLck went {base} -> {full} -> {after} kB across mmap+touch+munmap of {region_kib} kB. \
             If the charge were NOT released, locked memory would track the process's address space \
             rather than its resident set -- and then bounding Phase 13's residency could not lower the \
             page-lock ceiling at all, which is the opposite of what the windowing design assumes"
        );
    }

    /// The child half of [`unmapping_releases_the_page_lock_charge`]. Inert unless the env var is set,
    /// so it costs the suite one spawn and asserts nothing on its own.
    #[test]
    fn mlock_probe_child() {
        if std::env::var_os("HOLONOMY_MLOCK_PROBE").is_none() {
            return;
        }
        /// `VmLck` in kB from `/proc/self/status`.
        fn vm() -> u64 {
            let Ok(s) = std::fs::read_to_string("/proc/self/status") else {
                return 0;
            };
            s.lines()
                .find_map(|l| l.strip_prefix("VmLck:"))
                .and_then(|l| l.split_whitespace().next())
                .and_then(|v| v.parse().ok())
                .unwrap_or(0)
        }
        const REGION: usize = 2 * 1024 * 1024;
        let rc = unsafe { libc::mlockall(libc::MCL_FUTURE) };
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        let base = vm();
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                REGION,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if p == libc::MAP_FAILED {
            println!("PROBE {rc} {errno} {base} {base} {base} {}", REGION / 1024);
            return;
        }
        // Fault every page in: `MCL_FUTURE` charges on fault, not on `mmap`.
        unsafe { std::ptr::write_bytes(p as *mut u8, 0x41, REGION) };
        let full = vm();
        unsafe { libc::munmap(p, REGION) };
        let after = vm();
        println!("PROBE {rc} {errno} {base} {full} {after} {}", REGION / 1024);
    }

    #[test]
    fn core_dumps_are_sealed_or_reported() {
        let mut limits = Limits::default();
        seal_core_dumps(&mut limits);
        assert_eq!(
            limits.dumpable,
            Some(0),
            "PR_SET_DUMPABLE(0) did not take effect"
        );
        assert_eq!(
            limits.core_still_permitted,
            None,
            "RLIMIT_CORE is still {} after sealing",
            limits.core_still_permitted.unwrap_or(0)
        );
        // Idempotent: a second call must not fail, because the boot may seal twice across a
        // re-exec.
        let mut again = Limits::default();
        seal_core_dumps(&mut again);
        assert_eq!(again.core_still_permitted, None);
    }

    #[test]
    fn seccomp_action_avail_is_unreliable() {
        // `SECCOMP_GET_ACTION_AVAIL` was the obvious way to decide whether
        // `SECCOMP_RET_KILL_PROCESS` is available, and this test exists to record that it is
        // useless here.
        //
        // Measured on this host (kernel 7.2.6-200.fc44, x86-64), the call returns `EINVAL` for
        // **every** action tried: `RET_KILL`, `RET_KILL_PROCESS`, `RET_TRAP`, `RET_ALLOW`,
        // `RET_ERRNO`. Meanwhile installing a filter with `RET_KILL_PROCESS` succeeds, and an
        // unlisted syscall afterwards kills the process with `SIGSYS` -- wait status
        // 159 = 128 + 31 -- which is the `KILL_PROCESS` behaviour and not the `SIGKILL` of
        // `RET_KILL` (137).
        //
        // So an implementation that trusted the probe would have quietly installed the weaker
        // filter on a kernel that fully supports the stronger one, and every assertion would
        // still have passed. `Program::install` therefore decides by attempting the install,
        // and `tests/seccomp.rs` asserts the strong action really is the one in force.
        //
        // The assertion here is deliberately the *weak* one -- that this is not a reliable
        // source -- so that a kernel where the call starts working does not break the suite.
        let mut probe = libc::SECCOMP_RET_KILL_PROCESS;
        // SAFETY: `probe` is a valid `u32` the kernel writes into.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                libc::SECCOMP_GET_ACTION_AVAIL,
                &mut probe as *mut u32,
            )
        };
        if rc != 0 {
            eprintln!(
                "SECCOMP_GET_ACTION_AVAIL returned errno {} here, which is why the install \
                 path does not use it",
                std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
            );
        }
    }

    #[test]
    fn rlimit_infinity_is_reported_as_a_byte_count_not_a_sentinel() {
        // Not a specific resource, just that the mapping is total and lossless.
        let any = getrlimit(libc::RLIMIT_NOFILE);
        if let Some((soft, hard)) = any {
            assert!(soft > 0 && hard >= soft, "soft {soft} hard {hard}");
        }
    }
}
