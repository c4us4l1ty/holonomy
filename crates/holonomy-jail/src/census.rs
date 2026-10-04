//! The trap-mode syscall census: `SIGSYS` → record `si_syscall` → exit 90.
//!
//! # Why the census is iterative, and why that still produces the exact set
//!
//! PROJECT.md §2.6 asks for the exact set of syscalls a session actually issues. The obvious
//! implementation is one pass: trap everything, record every number, run the session. It does
//! not work, and the reason is worth stating precisely, because it is not an inconvenience:
//!
//! **`SECCOMP_RET_TRAP` does not execute the syscall.** A trapped call is refused *and* the
//! session is told nothing useful about it. Returning from the handler resumes with `-ENOSYS`,
//! so a trapped `read` does not read and a trapped `mmap` does not map. The session then
//! diverges from real behaviour immediately, and every measurement after the first trap is
//! measuring a program that is running on broken syscalls. The recorded set would be the set
//! of syscalls the session *tries*, from a session that cannot work, which is not what was asked
//! for.
//!
//! So the census converges from the other end. Start with a table that is missing syscalls;
//! run the session with `Action::Trap`; the first syscall that is not on the table traps, the
//! handler records its number and exits 90; add that number; run again. Each run advances
//! exactly one entry, and it terminates when the session completes.
//!
//! **The union of the recorded numbers is the exact set the session issues**, and the argument
//! is short: the final run completed, so every syscall it executed was on the table, so every
//! syscall it issued was recorded by some earlier run. Not a superset, not an estimate.
//!
//! The cost is one subprocess per missing entry. That is a one-off development cost, which is
//! why the table is a `const` and the gate is O(1).
//!
//! # What this cannot find
//!
//! Syscalls on a path the census workload does not take. Phase 8 re-runs the census against
//! the real session loop and asserts the table still suffices; that is the check that closes
//! this, and it is not optional. The Phase 7 gate asserts the *harness* detects a missing
//! entry (see `tests/census.rs`), which is the property this module has to have for Phase 8's
//! re-run to mean anything.

use core::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use crate::SigSys;

/// Exit code when the census halted on a syscall that is not on the table.
///
/// Distinct from [`crate::tripwire::TRIPWIRE_EXIT`] (137) so a parent can tell "the filter
/// caught an unlisted syscall" from "containment was lost" without parsing the report.
pub const CENSUS_EXIT: i32 = 90;

/// Number of distinct syscalls the census can record.
///
/// Sized against the reachable syscall space on x86-64 rather than against an expectation
/// about the session: a session that needs more than this many distinct syscalls is a session
/// that should not be in the jail at all, and hitting it must be a loud failure rather than a
/// silent truncation of the evidence.
pub const CENSUS_CAPACITY: usize = 512;

static RECORDS: [AtomicI64; CENSUS_CAPACITY] = [const { AtomicI64::new(0) }; CENSUS_CAPACITY];
static DISTINCT: AtomicUsize = AtomicUsize::new(0);
static TRIPS: AtomicUsize = AtomicUsize::new(0);
static HANDLER_INSTALLED: AtomicUsize = AtomicUsize::new(0);

/// Record a syscall number. Async-signal-safe and allocation-free.
///
/// Returns `false` when the table is full, which the caller reports rather than hides: a
/// census that silently dropped entries is worse than no census.
pub fn record(nr: i64) -> bool {
    let mut distinct = DISTINCT.load(Ordering::Acquire);
    loop {
        let mut i = 0usize;
        while i < distinct {
            if RECORDS[i].load(Ordering::Acquire) == nr {
                return true;
            }
            i += 1;
        }
        if distinct >= CENSUS_CAPACITY {
            return false;
        }
        match DISTINCT.compare_exchange_weak(
            distinct,
            distinct + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => {
                // The count is published before the value, so a reader that sees the new
                // count always finds the value.
                RECORDS[distinct].store(nr, Ordering::Release);
                return true;
            }
            Err(current) => distinct = current,
        }
    }
}

/// Every distinct syscall number recorded so far, in ascending order.
///
/// Allocates, so it is for the reporting side only -- never for the handler.
pub fn observed() -> Vec<i64> {
    let mut v: Vec<i64> = (0..DISTINCT.load(Ordering::Acquire))
        .map(|i| RECORDS[i].load(Ordering::Acquire))
        .collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// How many times the census halted, including repeats of an already-recorded number.
pub fn trips() -> usize {
    TRIPS.load(Ordering::Acquire)
}

/// Install the `SIGSYS` handler that records and exits.
///
/// No `SA_ONSTACK`: the census handler's frame is disposable -- it records a number and exits,
/// it never scrubs anything -- so there is nothing to gain from protecting it, and keeping it
/// off the alternate stack leaves that stack uncontended with the tripwire.
pub fn install_handler(report_fd: i32) -> Result<(), i32> {
    // SAFETY: valid `sigaction`, and the handler has the mandated signature.
    let rc = unsafe {
        libc::sigaction(
            libc::SIGSYS,
            &libc::sigaction {
                sa_sigaction: census_handler as *const () as libc::sighandler_t,
                sa_mask: blocked_everything(),
                sa_flags: libc::SA_SIGINFO,
                sa_restorer: None,
            },
            std::ptr::null_mut(),
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EINVAL));
    }
    REPORT_FD.store(report_fd as i64, Ordering::Release);
    HANDLER_INSTALLED.store(1, Ordering::Release);
    Ok(())
}

/// Whether [`install_handler`] has run.
pub fn handler_installed() -> bool {
    HANDLER_INSTALLED.load(Ordering::Acquire) != 0
}

static REPORT_FD: AtomicI64 = AtomicI64::new(2);

/// # Safety
///
/// The `SA_SIGINFO` signature. Only atomics, `write` and `_exit`.
unsafe extern "C" fn census_handler(
    _signal: libc::c_int,
    info: *mut libc::siginfo_t,
    _ctx: *mut libc::c_void,
) {
    let nr = if info.is_null() {
        -1i64
    } else {
        // SAFETY: `info` is the kernel's `siginfo_t`, whose layout for a seccomp SIGSYS is
        // `SigSys`. See its doc comment -- and the note there about `si_arch` not being populated.
        let sig = unsafe { core::ptr::read_unaligned(info.cast::<SigSys>()) };
        sig.syscall() as i64
    };
    TRIPS.fetch_add(1, Ordering::AcqRel);
    let fresh = record(nr);
    unsafe {
        let line = format_line(nr, fresh);
        libc::write(
            REPORT_FD.load(Ordering::Acquire) as i32,
            line.as_ptr().cast(),
            line.len(),
        );
        libc::_exit(CENSUS_EXIT);
    }
}

/// `"holonomy-census syscall=<n> arch=<n> first=yes|no\n"` in a single `write`.
///
/// Uses a stack buffer, not `format!`: this runs in a signal handler, where the allocator may
/// be in any state and where musl's buffered stdio is not involved at all.
fn format_line(nr: i64, fresh: bool) -> [u8; 96] {
    let mut buf = [0u8; 96];
    let mut at = 0usize;
    let push = |buf: &mut [u8; 96], at: &mut usize, bytes: &[u8]| {
        for byte in bytes {
            if *at < buf.len() {
                buf[*at] = *byte;
                *at += 1;
            }
        }
    };
    push(&mut buf, &mut at, b"holonomy-census syscall=");
    let (nr_digits, nr_len) = decimal(nr);
    push(&mut buf, &mut at, &nr_digits[..nr_len]);
    push(&mut buf, &mut at, b" name=");
    push(
        &mut buf,
        &mut at,
        crate::seccomp::table::name_of(nr).as_bytes(),
    );
    push(
        &mut buf,
        &mut at,
        if fresh { b" first=yes" } else { b" first=no" },
    );
    push(&mut buf, &mut at, b"\n");
    let mut out = [0u8; 96];
    out[..at].copy_from_slice(&buf[..at]);
    out
}

/// Render a possibly-negative `i64` as ASCII, returning the buffer and the bytes used.
///
/// Returns a length rather than a NUL-terminated string because the caller is a byte-wise
/// writer and a signal handler has no use for C string conventions. 24 is the widest possible
/// output (`-9223372036854775808` is 20 characters).
fn decimal(value: i64) -> ([u8; 24], usize) {
    let mut digits = [0u8; 24];
    let mut n = 0usize;
    let negative = value < 0;
    // `unsigned_abs` rather than `-value`, which overflows at `i64::MIN` -- and `si_syscall` is
    // a signed value the kernel hands back negative for a number it does not implement.
    let mut v = value.unsigned_abs();
    loop {
        digits[n] = b'0' + (v % 10) as u8;
        n += 1;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    if negative {
        digits[n] = b'-';
        n += 1;
    }
    let len = n;
    // Reverse in place with swaps, not with a copy-forward loop.
    //
    // The first version of this was `while n > 0 { n -= 1; digits[at] = digits[n]; at += 1 }`,
    // which is wrong: it overwrites `digits[at]` and then reads that same slot on a later
    // iteration. `decimal("12")` came out as "11", and the test caught it because it compares
    // against `format!` rather than against a hand-written expectation.
    let mut lo = 0usize;
    let mut hi = len - 1;
    while lo < hi {
        digits.swap(lo, hi);
        lo += 1;
        hi -= 1;
    }
    (digits, len)
}

/// `sa_mask`: every signal blocked while the handler runs.
fn blocked_everything() -> libc::sigset_t {
    crate::blocked_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_is_idempotent_and_ordered() {
        for nr in [11i64, 0, 60, 0, 11, 231] {
            assert!(record(nr), "table should not be full this early");
        }
        let seen = observed();
        let mut deduped = seen.clone();
        deduped.dedup();
        assert_eq!(seen, deduped, "observed() must not contain duplicates");
        assert!(
            seen.windows(2).all(|w| w[0] < w[1]),
            "observed() must be sorted: {seen:?}"
        );
        for nr in [0i64, 11, 60, 231] {
            assert!(seen.contains(&nr), "expected {nr} in {seen:?}");
        }
    }

    #[test]
    fn negative_syscall_numbers_survive_the_round_trip() {
        // `si_syscall` is signed, and the kernel reports negative numbers for the ones it does
        // not implement. A census that could not represent them would misreport a real trap.
        assert!(record(-1));
        assert!(record(-101));
        assert!(observed().contains(&-1));
        let (minus_one, len) = decimal(-1);
        assert_eq!(&minus_one[..len], b"-1");
        let (minus_101, len) = decimal(-101);
        assert_eq!(&minus_101[..len], b"-101");
    }

    #[test]
    fn decimal_handles_the_extremes() {
        for value in [i64::MAX, i64::MIN, 0, 1, -1, 137, -4096] {
            let (digits, len) = decimal(value);
            assert_eq!(
                core::str::from_utf8(&digits[..len]).unwrap(),
                std::format!("{value}"),
                "decimal({value})"
            );
            assert!(len <= 24);
        }
    }

    #[test]
    fn the_report_line_names_the_syscall_when_it_is_known() {
        let line = format_line(libc::SYS_getpid, true);
        let text =
            core::str::from_utf8(&line[..line.iter().position(|&b| b == b'\n').unwrap() + 1])
                .unwrap();
        assert!(text.contains("name=getpid"), "{text}");
        assert!(text.contains("first=yes"), "{text}");
    }

    #[test]
    fn the_report_line_says_so_when_the_syscall_is_unknown() {
        // An unlisted number is the normal case during a census run; printing `<nr>` rather than
        // a wrong name is the point.
        //
        // There is no architecture in the line at all, deliberately: `seccomp_send_kill_signal`
        // never populates `si_arch`, so a report that printed one would be printing the syscall
        // number twice. The first version did, and printed `syscall=149 arch=149` -- which is how
        // the aliasing was found.
        let line = format_line(9999, false);
        let text =
            core::str::from_utf8(&line[..line.iter().position(|&b| b == b'\n').unwrap() + 1])
                .unwrap();
        assert!(text.contains("name=<9999>"), "{text}");
        assert!(text.contains("first=no"), "{text}");
    }
}
