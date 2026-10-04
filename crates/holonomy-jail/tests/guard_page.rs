//! TC-MEM-02: a guard-page fault is caught, every `SecureBlock` is scrubbed, the process
//! exits 137, and no core file is written.
//!
//! Run with `cargo test -p holonomy-jail`.
//!
//! # Why every case runs in a subprocess
//!
//! The tripwire's contract ends in `_exit(137)`. There is no version of "the handler ran and
//! the process is still alive" to assert against, so each case re-executes this test binary with
//! an environment variable naming the role, and the parent asserts on the child's exit status
//! and on its captured stderr.
//!
//! That also sidesteps a real hazard: `sigaction` is *process-wide*, so a unit test that
//! installed the handler would make every other test in this binary run under a handler that
//! exits 137. `sigaltstack` is per-thread and `SecureBlock::allocate` touches a process-wide page
//! lock, so neither is a reason to share a process between cases.
//!
//! # The core-dump assertion is differential, because a direct one would be vacuous here
//!
//! `/proc/sys/kernel/core_pattern` on this host is
//!
//! ```text
//! |/usr/lib/systemd/systemd-coredump %P %u %g %s %t %c %h %d %F
//! ```
//!
//! a pipe. So a core dump is *never* a file in the working directory, and asserting "no `core`
//! file appeared" would pass no matter what the process did. Two things replace it:
//!
//! 1. `/var/lib/systemd/coredump` is world-readable on this host, so core dumps *are* countable.
//!    [`count_coredumps`] reads it directly rather than shelling out to `coredumpctl`.
//! 2. [`without_the_core_seal_a_guard_fault_does_produce_a_coredump`] runs the same fault with
//!    the seal deliberately omitted and asserts a new entry appears. That is the control that
//!    makes the absence in the sealed case mean something -- H2's DOCTRINE §4, prove the harness
//!    can detect the failure it exists to detect.
//!
//! And the report itself re-reads `RLIMIT_CORE` and `PR_GET_DUMPABLE` at fault time, so the
//! handler's own view of the seal is on the record rather than inferred.

use std::path::PathBuf;
use std::process::{Command, Output};

use holonomy_jail::registry;
use holonomy_jail::{tripwire, AltStack, Limits, TripwireError, ALT_STACK_BYTES, TRIPWIRE_EXIT};
use holonomy_secure::{page_size, SecureBlock};

/// Selects the child's role. Set by the parent, read by the probe `#[test]`.
const ROLE: &str = "HOLONOMY_JAIL_GUARD_ROLE";

/// Payload size for the block whose guard we trip. A page multiple so the *high* guard starts
/// immediately after the last payload byte, which makes its address exact rather than derived.
const BLOCK_BYTES: usize = 8192;

/// The pattern the payload is filled with, so "scrubbed" is a claim about specific bytes.
const PLAINTEXT: u8 = 0xA5;

/// Which fault to cause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Read one byte below the payload, i.e. the low `PROT_NONE` guard.
    Underflow,
    /// Read one byte at the start of the high `PROT_NONE` guard.
    Overrun,
    /// Read address 0: a fault at an address the registry has never heard of.
    Null,
    /// The same as `Underflow` but with the core-dump seal deliberately skipped.
    Unsealed,
    /// Install a probe `SIGSEGV` handler that prints `si_code`/`si_addr` and exits 0, to check
    /// that this crate's `siginfo_t` layout is the kernel's.
    Siginfo,
    /// **No handler at all**, and no seal: let the process die from `SIGSEGV`.
    ///
    /// The control for the core-dump assertions. It has to have no handler, because a process
    /// that dies *by calling `_exit`* never reaches the kernel's `do_coredump` at all -- so the
    /// tripwire's own cases cannot demonstrate that a core dump would otherwise have happened.
    RawUnsealed,
    /// **No handler**, with the seal applied: die from `SIGSEGV`, no core dump.
    ///
    /// Isolates the seal's own contribution, with the tripwire out of the picture.
    RawSealed,
}

impl Role {
    fn parse() -> Option<Self> {
        match std::env::var(ROLE).ok()?.as_str() {
            "underflow" => Some(Self::Underflow),
            "overrun" => Some(Self::Overrun),
            "null" => Some(Self::Null),
            "unsealed" => Some(Self::Unsealed),
            "siginfo" => Some(Self::Siginfo),
            "raw-unsealed" => Some(Self::RawUnsealed),
            "raw-sealed" => Some(Self::RawSealed),
            _ => None,
        }
    }
}

/// What a child produced.
struct Child {
    exit: i32,
    report: String,
}

impl Child {
    /// Parse one `key=value` out of the tripwire report.
    fn field(&self, key: &str) -> String {
        for token in self.report.split_whitespace() {
            if let Some(rest) = token.strip_prefix(&format!("{key}=")) {
                return rest.to_string();
            }
        }
        String::new()
    }
}

/// Run this test binary again in `role` and capture its stderr and exit status.
fn run(role: Role) -> Child {
    let exe = std::env::current_exe().expect("current test binary");
    let out: Output = Command::new(exe)
        .env(ROLE, role_name(role))
        // The probe is its own `#[test]`, so no other test in this binary runs in the child and
        // no fault in one can be confused with a fault in another.
        .args(["--exact", "probe", "--nocapture", "--test-threads=1"])
        .output()
        .expect("re-exec the probe");
    Child {
        exit: out.status.code().unwrap_or(-1),
        report: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Underflow => "underflow",
        Role::Overrun => "overrun",
        Role::Null => "null",
        Role::Unsealed => "unsealed",
        Role::Siginfo => "siginfo",
        Role::RawUnsealed => "raw-unsealed",
        Role::RawSealed => "raw-sealed",
    }
}

/// Whether this role applies the core-dump seal.
fn seals_core_dumps(role: Role) -> bool {
    matches!(
        role,
        Role::Underflow | Role::Overrun | Role::Null | Role::RawSealed
    )
}

/// Whether this role installs the tripwire.
fn uses_tripwire(role: Role) -> bool {
    matches!(
        role,
        Role::Underflow | Role::Overrun | Role::Null | Role::Unsealed
    )
}

/// Core dumps attributable to this test binary.
///
/// Matches on `comm`, which the kernel truncates to 15 bytes -- so the stem is truncated the
/// same way. `comm` is the executable's file name, which for a cargo test is
/// `guard_page-<hash>`.
fn count_coredumps() -> usize {
    let dir = PathBuf::from("/var/lib/systemd/coredump");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let stem: String = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default()
        .chars()
        .take(15)
        .collect();
    if stem.is_empty() {
        return 0;
    }
    entries
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("core.{stem}."))
        })
        .count()
}

// ---------------------------------------------------------------- the probe

/// The child's only test. Selected by `--exact probe`; never runs in the parent because the
/// parent re-executes rather than calling this.
#[test]
fn probe() {
    let Some(role) = Role::parse() else {
        // Parent process: the role variable is absent, so there is nothing to do.
        return;
    };

    // --- Step 1: seal core dumps, unless this role is a deliberate control.
    let mut limits = Limits::default();
    if seals_core_dumps(role) {
        holonomy_jail::rlimits::seal_core_dumps(&mut limits);
        assert_eq!(
            limits.core_still_permitted, None,
            "the child could not set RLIMIT_CORE=0, so its core-dump absence would prove nothing"
        );
    }

    // --- A block to trip, filled with a recognisable byte.
    let mut block = SecureBlock::allocate(BLOCK_BYTES).expect("allocate the tripping block");
    block.as_mut_slice().fill(PLAINTEXT);

    // --- And the alternate signal stack, as a second SecureBlock, so the signal frame the
    // kernel writes is itself registered and gets scrubbed.
    let alt_block = SecureBlock::allocate(ALT_STACK_BYTES).expect("allocate the alternate stack");
    let alt = AltStack::install(alt_block.as_ptr() as usize, ALT_STACK_BYTES)
        .expect("install the alternate signal stack");
    assert!(
        AltStack::current_has_autodisarm(),
        "SS_AUTODISARM must be set"
    );

    // Total payload the registry must scrub: both blocks. The tripwire reports the number, and
    // the parent compares it against this.
    let expected_scrubbed = BLOCK_BYTES + ALT_STACK_BYTES;

    if role == Role::Siginfo {
        probe_siginfo(&block);
        return;
    }

    if uses_tripwire(role) {
        tripwire::install(&alt).expect("install the tripwire");
    }
    eprintln!(
        "probe: role={} expect_scrubbed={expected_scrubbed} alt_base=0x{:x} tripwire={}",
        role_name(role),
        alt.base(),
        uses_tripwire(role)
    );

    // --- Trip it.
    let payload = block.as_ptr();
    let page = page_size().expect("page size");
    // The high guard begins immediately after the page-rounded payload, which is
    // `mapped_len - 2 pages` past the payload's first byte.
    let high_guard = (payload as usize) + block.mapped_len() - 2 * page;
    let target: *const u8 = match role {
        // One byte below the payload is, by construction, inside the low `PROT_NONE`
        // guard that `SecureBlock::allocate` put there.
        Role::Underflow | Role::Unsealed | Role::RawUnsealed | Role::RawSealed => unsafe {
            payload.sub(1)
        },
        Role::Overrun => high_guard as *const u8,
        Role::Null => core::ptr::null(),
        Role::Siginfo => unreachable!("handled above"),
    };

    // Read the byte. If the guard failed, this reads plaintext and the assertion below fires.
    let observed = unsafe { core::ptr::read_volatile(target) };
    panic!("the guard did not fault: read 0x{observed:02x} from 0x{target:p}");
}

// ---------------------------------------------------------------- the gate

#[test]
fn a_one_byte_underflow_into_the_low_guard_page_exits_137() {
    let before = count_coredumps();
    let child = run(Role::Underflow);

    assert_eq!(
        child.exit, TRIPWIRE_EXIT,
        "expected exit 137, stderr:\n{}",
        child.report
    );
    assert!(
        child.report.contains("exit=137"),
        "the report must restate its own exit code:\n{}",
        child.report
    );
    assert_eq!(
        child.field("site"),
        "guard",
        "si_addr must land in a registered guard page:\n{}",
        child.report
    );
    assert_eq!(child.field("signal"), "11", "SIGSEGV");
    assert_eq!(
        child.field("scrubbed"),
        (BLOCK_BYTES + ALT_STACK_BYTES).to_string(),
        "every registered payload must be scrubbed, and nothing else:\n{}",
        child.report
    );
    assert_eq!(
        child.field("stack"),
        "on-alt",
        "the handler must be running on the alternate stack, which is the whole reason \
         sigaltstack exists:\n{}",
        child.report
    );
    assert_eq!(
        child.field("core"),
        "0",
        "RLIMIT_CORE must read 0 at fault time:\n{}",
        child.report
    );
    assert_eq!(
        child.field("dumpable"),
        "0",
        "PR_GET_DUMPABLE must read 0 at fault time:\n{}",
        child.report
    );
    assert_eq!(
        count_coredumps(),
        before,
        "a sealed guard fault must not produce a core dump"
    );
}

#[test]
fn a_one_byte_overrun_past_the_high_guard_page_exits_137() {
    let before = count_coredumps();
    let child = run(Role::Overrun);
    assert_eq!(
        child.exit, TRIPWIRE_EXIT,
        "expected exit 137, stderr:\n{}",
        child.report
    );
    assert_eq!(child.field("site"), "guard", "\n{}", child.report);
    assert_eq!(child.field("stack"), "on-alt", "\n{}", child.report);
    assert_eq!(count_coredumps(), before, "no core dump");
}

#[test]
fn a_null_dereference_is_scrubbed_and_exits_137_too() {
    // A fault at an address the registry has never heard of is a bug rather than a containment
    // event, and the report says so. It still exits 137, because at that point the process has
    // lost control either way and the plaintext must go regardless of why.
    let child = run(Role::Null);
    assert_eq!(child.exit, TRIPWIRE_EXIT, "stderr:\n{}", child.report);
    assert_eq!(child.field("site"), "unregistered", "\n{}", child.report);
    // `0x0`: `addr` is printed in hex, so the null page is `0x0` and not `0`.
    assert_eq!(child.field("addr"), "0x0", "\n{}", child.report);
    assert_eq!(
        child.field("scrubbed"),
        (BLOCK_BYTES + ALT_STACK_BYTES).to_string(),
        "a fault at an unknown address must still scrub every block:\n{}",
        child.report
    );
}

/// Differential core-dump gate, in three cases.
///
/// The naive assertion -- "a guard fault produces a 0-byte core dump" -- cannot be made on this
/// host, and it is worth being precise about why, because the reason turned out to be the more
/// interesting result:
///
/// * **`core_pattern` is a pipe.** `/proc/sys/kernel/core_pattern` reads
///   `|/usr/lib/systemd/systemd-coredump %P %u %g %s %t %c %h %d %F`, so a core dump is never a
///   file in the working directory. Asserting "no `core` file appeared" would pass no matter what
///   the process did. The substitute is to count entries in `/var/lib/systemd/coredump`, which is
///   world-readable here.
///
/// * **The tripwire cannot produce a core dump at all.** It calls `_exit(137)`, i.e. `exit_group`,
///   so the process exits *normally* and the kernel never reaches `do_coredump`. Exit 137 from a
///   signal handler is not "killed with signal 9"; it is an ordinary exit with an unusual status.
///   So the absence of a core dump in the tripwire cases is not evidence that the seal worked --
///   it would happen anyway.
///
/// Three cases, each isolating one mechanism, so the absence means something:
///
/// | case | handler | seal | how it dies | core dump? |
/// |------|---------|------|------------|------------|
/// | `raw-unsealed` | none | no | from `SIGSEGV` | **yes** -- proves the machinery is live |
/// | `raw-sealed` | none | yes | from `SIGSEGV` | no -- isolates the seal |
/// | `underflow` | tripwire | yes | from `_exit(137)` | no -- isolates the handler |
///
/// If the first case stops producing a core dump, the other two are vacuous and this test says so
/// rather than passing quietly.
#[test]
fn the_core_dump_gate_is_differential_and_all_three_cases_agree() {
    // --- Case 1: the control. The process dies from SIGSEGV with nothing in the way.
    let before = count_coredumps();
    let raw = run(Role::RawUnsealed);
    assert_ne!(
        raw.exit, TRIPWIRE_EXIT,
        "the control has no handler, so it must die from the signal rather than exit 137"
    );
    let after_raw = count_coredumps();
    assert!(
        after_raw > before,
        "an unsealed process dying from SIGSEGV should leave a core dump in \
         /var/lib/systemd/coredump ({before} -> {after_raw}). If this host records none, the \
         two cases below prove nothing and the 'zero-length core dump' claim must be restated \
         in terms of RLIMIT_CORE and PR_SET_DUMPABLE alone."
    );

    // --- Case 2: the seal alone.
    let before = after_raw;
    let sealed = run(Role::RawSealed);
    assert_ne!(
        sealed.exit, TRIPWIRE_EXIT,
        "the control has no handler, so it must die from the signal rather than exit 137"
    );
    assert_eq!(
        count_coredumps(),
        before,
        "RLIMIT_CORE=0 plus PR_SET_DUMPABLE=0 must suppress the core dump on its own"
    );

    // --- Case 3: the tripwire. Note this one never reaches the core-dump path at all.
    let before = count_coredumps();
    let tripped = run(Role::Underflow);
    assert_eq!(tripped.exit, TRIPWIRE_EXIT, "stderr:\n{}", tripped.report);
    assert_eq!(
        count_coredumps(),
        before,
        "the tripwire exits via _exit(137), so the kernel never reaches do_coredump"
    );
    // And the seal was in force anyway, so the exit is not relying on that alone.
    assert_eq!(tripped.field("core"), "0", "\n{}", tripped.report);
    assert_eq!(tripped.field("dumpable"), "0", "\n{}", tripped.report);
}

/// Checks the `siginfo_t` layout this crate assumes, empirically.
///
/// `SigFault` is a hand-written `repr(C)` projection of the kernel's `siginfo_t`, because
/// `libc` does not expose its internals on musl. A layout guess is worthless unless it is
/// checked, so this reads the same two fields through a separate probe handler and asserts they
/// say what the tripwire would have said.
///
/// If `si_code` reads back as `SEGV_MAPERR` rather than `SEGV_ACCERR`, the offset is wrong: a
/// `PROT_NONE` guard page is *mapped but not permitted*, which is `SEGV_ACCERR`. And `si_addr`
/// must be the guard address, not an offset by the same mistake.
#[test]
fn the_siginfo_layout_the_tripwire_assumes_is_the_kernel_s() {
    let child = run(Role::Siginfo);
    let expected = child.field("addr").trim_start_matches("0x").to_string();
    let report = &child.report;

    assert_eq!(child.exit, 0, "the probe handler should exit 0:\n{report}");
    assert!(
        report.contains("siginfo.signo=11"),
        "expected SIGSEGV:\n{report}"
    );
    assert!(
        report.contains("siginfo.code=2"),
        "expected SEGV_ACCERR (2) for a PROT_NONE guard page; SEGV_MAPERR (1) would mean the \
         siginfo_t offset is wrong:\n{report}"
    );
    assert!(
        report.contains(&format!("siginfo.addr=0x{expected}")),
        "si_addr must be the guard address the probe deliberately read:\n{report}"
    );
}

// ---------------------------------------------------------------- in-process checks

/// Installing the tripwire for a stack that is not the registered one must be refused.
///
/// Safe in-process because it never installs anything -- it returns before the `sigaction`.
/// `sigaction` being process-wide is precisely why this refusal needs no subprocess and the
/// success path needs one.
#[test]
fn tripwire_install_refuses_a_stack_that_is_not_the_registered_one() {
    let mut first = vec![0u8; ALT_STACK_BYTES];
    let stale = AltStack::install(first.as_mut_ptr() as usize, ALT_STACK_BYTES)
        .expect("install our own alternate stack");

    // Register a *different* region, which displaces `stale`. Now `stale` is a plausible-looking
    // `AltStack` value the kernel does not have registered -- exactly the case the identity check
    // exists to catch.
    //
    // The registered one is never passed to `tripwire::install`, because that call would *succeed*
    // and install process-wide signal handlers from inside a `cargo test` binary, making every
    // later test in this file run under a handler that exits 137.
    let mut second = vec![0u8; ALT_STACK_BYTES];
    let registered = AltStack::install(second.as_mut_ptr() as usize, ALT_STACK_BYTES)
        .expect("install a second, different stack");
    assert_ne!(stale, registered, "the two regions must not compare equal");
    assert_eq!(AltStack::current(), Some(registered));
    assert_eq!(
        tripwire::install(&stale),
        Err(TripwireError::WrongAltStack),
        "installing for a stack the kernel did not register would give SA_ONSTACK with none of \
         its protection"
    );

    // And with nothing registered at all.
    assert!(AltStack::disable());
    assert_eq!(
        tripwire::install(&registered),
        Err(TripwireError::NoAltStack)
    );
    assert!(!tripwire::is_installed());
    // Put it back so the rest of this binary's threads are unaffected.
    AltStack::install(second.as_mut_ptr() as usize, ALT_STACK_BYTES).expect("restore");
}

/// A zero-length noise range and a zero-length registry range are both refused.
///
/// In-process and side-effect-free, unlike the `_exit`-ing probes.
#[test]
fn the_registry_refuses_a_zero_length_payload() {
    let before = registry::active_count();
    let byte = 0u8;
    assert_eq!(
        registry::register(0, 4096, &byte as *const u8 as usize, 0),
        Err(holonomy_jail::RegisterError::EmptyPayload),
        "a zero-length payload must not be registrable: the slot would be published with \
         len == 0, which every reader treats as *empty*, so the mapping would look \
         unregistered both to the scrub and to classify"
    );
    assert_eq!(registry::active_count(), before);
}

/// Reads `si_code`/`si_addr` through a separate handler and prints them, then exits 0.
///
/// The point is independence: this handler knows only [`holonomy_jail::SigFault`] and the
/// constant names, and it is the only thing standing between the layout and being wrong.
fn probe_siginfo(block: &SecureBlock) {
    // SAFETY: a valid `sigaction` for a probe handler; `SA_SIGINFO` is the only flag it needs,
    // and it never returns.
    let rc = unsafe {
        libc::sigaction(
            libc::SIGSEGV,
            &libc::sigaction {
                sa_sigaction: siginfo_probe as *const () as libc::sighandler_t,
                sa_mask: blocked_all(),
                sa_flags: libc::SA_SIGINFO,
                sa_restorer: None,
            },
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "sigaction for the probe handler");

    // SAFETY: one byte below the payload is the low `PROT_NONE` guard page, which is the
    // whole point of this probe.
    let target = unsafe { block.as_ptr().sub(1) };
    eprintln!("siginfo.expect=0x{:x}", target as usize);
    let observed = unsafe { core::ptr::read_volatile(target) };
    panic!("the guard did not fault: read 0x{observed:02x}");
}

/// # Safety
///
/// The `SA_SIGINFO` signature. Prints and exits.
unsafe extern "C" fn siginfo_probe(
    _signal: libc::c_int,
    info: *mut libc::siginfo_t,
    _ctx: *mut libc::c_void,
) {
    // SAFETY: `info` is the kernel's `siginfo_t`; the layout is what this test is checking.
    let fault = unsafe { core::ptr::read_unaligned(info.cast::<holonomy_jail::SigFault>()) };
    // SAFETY: a stack buffer and `write`.
    let line = std::format!(
        "siginfo.signo={} siginfo.code={} siginfo.addr=0x{:x}\n",
        fault.signo,
        fault.code,
        fault.addr
    );
    unsafe {
        libc::write(2, line.as_ptr().cast(), line.len());
        libc::_exit(0);
    }
}

/// A `sigset_t` with every bit set, for the probe handler's `sa_mask`.
fn blocked_all() -> libc::sigset_t {
    // SAFETY: an all-zero bitmask is the correct initial state for a signal mask.
    let mut set: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: `set` is a valid, writable `sigset_t`.
    assert_eq!(unsafe { libc::sigfillset(&mut set) }, 0);
    set
}
