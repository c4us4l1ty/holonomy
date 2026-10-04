//! Process isolation, seccomp jail, tripwire handler and teardown.
//!
//! # Why every crate in this workspace depends on this one
//!
//! A seccomp `SECCOMP_RET_KILL_PROCESS` filter is a *closed world*. There is no way to add a
//! syscall to it once the session is running, so every syscall the process will ever issue must
//! already be reachable from code that exists when the filter is installed. Keeping the
//! sealing primitives in one leaf crate makes that constraint visible in the type graph rather
//! than in a comment, which is what [`BootStep`] is for.
//!
//! # The boot sequence, and why it is a type
//!
//! Each step returns a *new type* that has exactly one method, and that method is the next
//! step. There is no `skip_to_seccomp()`, no `Sealed::from_raw`, and no way to hold a value
//! that has been sealed without having gone through every stage, because the only way to
//! obtain one is to walk the whole chain:
//!
//! ```text
//! Enter
//!   .seal_core_dumps()               -> CoreDumpsSealed      RLIMIT_CORE=0, PR_SET_DUMPABLE=0
//!   .raise_memlock()                 -> MemlockRaised        RLIMIT_MEMLOCK soft -> hard
//!   .install_tripwires(alt)          -> TripwiresInstalled   sigaltstack + sigaction(SA_ONSTACK)
//!   .open_descriptors(open)          -> Opened<T>            container fd, DRM fd, evdev fd
//!   .lock_all_pages()                -> MemoryLocked<T>      mlockall(MCL_CURRENT|MCL_FUTURE)
//!   .isolate_network()               -> NetworkIsolated<T>   unshare(CLONE_NEWNET)
//!   .drop_privileges()               -> PrivilegesDropped<T> PR_SET_NO_NEW_PRIVS
//!   .seal(filter)                    -> Sealed<T>            seccomp(SET_MODE_FILTER, TSYNC)
//!   .run_session(f)                  -> R                    nothing after here may allocate
//!   .teardown()                      -> !                    noise, scrub, ioctl, _exit(0)
//! ```
//!
//! # `mlockall` after the descriptors, which is the reverse of PROJECT.md §5
//!
//! PROJECT.md Phase 7 lists `mlockall` first. This goes after the boot-established
//! descriptors, and the difference is nil rather than a matter of taste:
//!
//! `mlockall(MCL_CURRENT | MCL_FUTURE)` covers *all currently mapped pages* plus all future
//! mappings. So calling it after the descriptors were opened locks exactly the same set it
//! would have locked before, and the `MCL_FUTURE` half is unchanged either way -- a
//! `MCL_FUTURE`-locked process does not unlock anything later.
//!
//! The one thing that does change is *when* it can fail. Raised early, it fails on a process
//! that has already faulted in a lot of pages; raised here, it runs on the same process
//! `RLIMIT_MEMLOCK` was just raised to the hard limit on, which is the difference between 8 MiB
//! and whatever the target actually grants.
//!
//! And the reverse ordering has a specific hazard that this one does not: `unshare(CLONE_NEWUSER)`
//! changes the process's effective uid to an unmapped 65534, and `RLIMIT_MEMLOCK` is accounted
//! per uid. Locking *before* the unshare means every page is locked against the limit that
//! actually applies to us. See [`netns`].
//!
//! # Phase 7's scope, and what Phase 8 adds
//!
//! [`seccomp::table`] is the deliverable: the allowlist, derived from the census in
//! [`census`] rather than from the PRD. `Sealed::run_session` is where the loop goes, and
//! nothing allocates inside it.
//!
//! The census workload is not yet the session -- Phase 8 assembles the session, and it
//! re-runs the census against it. That is a real limit of the current allowlist and it is
//! stated in the table rather than left for someone to discover.
//!
//! # The exit codes, which are the observable contract
//!
//! | code | meaning |
//! |------|---------|
//! | 0 | orderly teardown: noise, scrub, hardware released |
//! | 90 | census halted on a syscall that is not on the table |
//! | 137 | containment lost: `SIGSEGV`/`SIGBUS` tripped a guard, everything scrubbed |
//! | `SIGSYS` | a syscall was refused by `SECCOMP_RET_KILL_PROCESS` |
//!
//! Distinct by construction, so a test can tell which of these happened without parsing output.

pub mod altstack;
pub mod census;
pub mod netns;
pub mod registry;
pub mod rlimits;
pub mod seccomp;
pub mod teardown;
pub mod tripwire;

/// A `sigset_t` with every signal blocked, for a handler's `sa_mask`.
///
/// Built with `libc::sigfillset` rather than a struct literal, because `sigset_t`'s fields are
/// private in `libc` and differ between the glibc (`val`) and musl (`__val`) spellings. The
/// libc function is the portable answer and there was no reason to reimplement it.
///
/// Blocking everything else while a handler runs is the default-correct choice: a `SIGTERM`
/// arriving mid-scrub must not run a handler that inspects half-scrubbed state.
///
/// `unsafe`: `libc::sigfillset` writes a whole `sigset_t`.
pub fn blocked_all() -> libc::sigset_t {
    // SAFETY: an all-zero bitmask is the correct initial state for every signal, and
    // `sigfillset` overwrites all of it regardless.
    let mut set: libc::sigset_t = unsafe { core::mem::zeroed() };
    // SAFETY: `set` is a valid, writable `sigset_t` and `sigfillset` fills exactly that type.
    let rc = unsafe { libc::sigfillset(&mut set) };
    assert_eq!(
        rc, 0,
        "sigfillset failed, which is not a recoverable condition"
    );
    set
}

/// Linux `SS_AUTODISARM`, absent from `libc`.
///
/// Kernel value `0x80000000` (`asm-generic/signal.h`). Reported back in `sigaltstack`'s
/// `old_ss` to say "this stack disarms itself on entry", and set on `ss_flags` to request it.
pub(crate) const SS_AUTODISARM: libc::c_int = 0x8000_0000u32 as libc::c_int;

/// Linux `AUDIT_ARCH_X86_64`, absent from `libc`.
///
/// `AUDIT_ARCH_*` values pack the ELF machine into the upper 16 bits with `__AUDIT_ARCH_LE`
/// in bit 31; `EM_X86_64` is 62 (`0x3e`), so the value is `0xC000003E`.
pub const AUDIT_ARCH_X86_64: u32 = 0xC000_003E;

/// Number of `greg_t` slots in x86-64 `mcontext_t`, absent from `libc`.
///
/// The kernel's `MAX_NRREG` for `PT_REGS`. `greg_t` is `i64`, and the array is fixed at 23.
pub const NGREG: usize = 23;

/// The kernel's `siginfo_t` as this crate needs it, for `SIGSEGV`/`SIGBUS`.
///
/// `_sifields._sigfault`, and only its first two members:
///
/// ```c
/// int          si_signo;   // offset  0
/// int          si_errno;   // offset  4
/// int          si_code;    // offset  8
/// int          __pad0;     // offset 12, so si_addr lands 8-aligned
/// __uint64_t   si_addr;    // offset 16
/// ```
///
/// Declared here rather than taken from `libc::siginfo_t` because `libc` does not expose
/// `siginfo_t`'s internals on musl at all, and because the offsets are then a claim this file
/// makes and `tests/guard_page.rs` *checks empirically* -- it faults, and asserts that
/// `si_code` reads back as `SEGV_MAPERR` and `si_addr` as the address it faulted on. A layout
/// guess that is wrong cannot survive that test.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SigFault {
    /// `SIGSEGV` or `SIGBUS`.
    pub signo: libc::c_int,
    /// Always 0 from the kernel for these signals.
    pub errno: libc::c_int,
    /// `SEGV_MAPERR`, `SEGV_ACCERR`, `BUS_ADRERR` or `BUS_ADRERR`+`SI_KERNEL`.
    pub code: libc::c_int,
    __pad0: libc::c_int,
    /// The faulting address.
    pub addr: libc::c_ulong,
}

/// The kernel's `siginfo_t` as this crate needs it, for a seccomp `SIGSYS`.
///
/// `_sifields._sigpoll`:
/// ```c
/// int      __pad[3];       // offset 12..24
/// __u64_t  si_call_addr;   // offset 24
/// ```
/// and, at the *same* offset, the `_sigsys` union holding `si_arch` and `si_syscall`. So this
/// struct carries the slot as a `u64` and the two interpretations are taken by splitting it,
/// which is exactly the aliasing the C union has.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SigSys {
    /// `SIGSYS`.
    pub signo: libc::c_int,
    /// `SECCOMP_RET_TRAP` sets this to the action's data, i.e. 0 for a bare `RET_TRAP`.
    pub errno: libc::c_int,
    /// `SYS_SECCOMP`.
    pub code: libc::c_int,
    __pad: [libc::c_int; 3],
    /// `_sigsys`: `si_syscall` on x86-64, `si_call_addr` on i386.
    slot: libc::c_ulong,
}

impl SigSys {
    /// `si_syscall`: the syscall number, or a negative value the kernel did not implement.
    pub fn syscall(&self) -> libc::c_int {
        self.slot as libc::c_int
    }

    // **There is no `arch()` accessor, and that is not an oversight.**
    //
    // `siginfo_t._sifields._sigpoll` puts `si_arch` and `si_syscall` in a union at the same offset,
    // which makes it look like the kernel reports the offending architecture. It does not.
    // `seccomp_send_kill_signal` does `memset(&info, 0, sizeof(info))`, sets `si_signo`,
    // `si_errno` and `si_code`, and then switches on `sd->arch` to write *either* `si_syscall`
    // (x86-64) or `si_call_addr` (i386) into that union -- without ever writing `si_arch`.
    //
    // So on x86-64 the low 32 bits of `si_arch` are `sd->nr`, and reading it back yields the
    // syscall number. Measured here: for the trapped syscall 149 the report printed
    // `syscall=149 arch=149`, which is the signature of the two fields being the same 8 bytes.
    //
    // The architecture is therefore taken from the *filter*, not from the signal: `Program::build`
    // hard-kills anything whose `seccomp_data.arch` is not `AUDIT_ARCH_X86_64`, so a SIGSYS this
    // crate ever sees is by construction an x86-64 one.
}

/// `SEGV_MAPERR`: the address is not mapped to the process.
pub const SEGV_MAPERR: libc::c_int = 1;

/// `SEGV_ACCERR`: the address is mapped but the access is not permitted.
///
/// What a `PROT_NONE` guard page produces, and therefore the code the guard-page test should
/// see. If it sees `SEGV_MAPERR` instead, the block's mapping is not what it claims to be.
pub const SEGV_ACCERR: libc::c_int = 2;

pub use altstack::{AltStack, AltStackError, ALT_STACK_BYTES};

// The `siginfo_t` layouts and the `si_code` constants above are `pub` because a harness that
// verifies the tripwire has to read the signal frame itself: `tests/guard_page.rs` checks
// `si_code == SEGV_ACCERR` and that `si_addr` is the address it faulted on, which is what makes
// the layout claim in `SigFault` a checked fact rather than a comment.
pub use census::CENSUS_EXIT;
pub use netns::NetworkIsolation;
pub use registry::{FaultSite, RegisterError, RegistryHandle};
pub use rlimits::Limits;
pub use seccomp::{Action, Installed, Program, SeccompError};
pub use teardown::{TeardownError, TeardownPlan, TEARDOWN_EXIT};
pub use tripwire::{TripReport, TripwireError, TRIPWIRE_EXIT};

use altstack::AltStack as AltStackRef;

/// Retained from Phase 0.
///
/// Every crate in the workspace depends on this one, and Phase 0 needed something to depend on.
/// It is kept so that `holonomy-secure`'s re-export keeps compiling, and it doubles as the
/// assertion that the dependency edge is still in the graph.
pub const PHASE_0_PLACEHOLDER: () = ();

/// Why the boot sequence stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JailError {
    /// `mlockall` was refused. Carries `errno`. Not fatal to the *process*: it degrades
    /// hardening, because `MADV_DONTDUMP` and the guard pages still apply.
    MlockAll(i32),
    /// The tripwire could not be installed.
    Tripwire(TripwireError),
    /// `PR_SET_NO_NEW_PRIVS` was refused. Carries `errno`. Fatal: without it the filter cannot
    /// be installed at all, and the whole reason the process exists has gone.
    NoNewPrivs(i32),
    /// The filter was refused.
    Seccomp(SeccompError),
}

impl core::fmt::Display for JailError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::MlockAll(e) => write!(f, "mlockall refused: errno {e} (pages may reach swap)"),
            Self::Tripwire(e) => write!(f, "tripwire: {e}"),
            Self::NoNewPrivs(e) => write!(f, "PR_SET_NO_NEW_PRIVS refused: errno {e}"),
            Self::Seccomp(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for JailError {}

/// Everything the boot sequence learned, for the boot banner and the Phase 9 report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BootReport {
    /// Resource limits as they were sealed and raised.
    pub limits: Limits,
    /// Whether `mlockall` took.
    pub all_pages_locked: bool,
    /// `mlockall`'s `errno` if it did not.
    pub mlock_errno: i32,
    /// `/proc/sys/kernel/randomize_va_space`, which is the target's ASLR posture.
    pub aslr: Option<u32>,
    /// Whether `PR_SET_NO_NEW_PRIVS` is set. Always true in a `Sealed` state.
    pub no_new_privs: bool,
}

/// Stage 0: nothing has been done.
#[derive(Debug, Clone, Copy, Default)]
pub struct Enter;

impl Enter {
    /// Stage 1. `RLIMIT_CORE = 0`, then `PR_SET_DUMPABLE = 0`.
    ///
    /// In that order, and both before anything exists worth dumping: see
    /// [`rlimits::seal_core_dumps`].
    pub fn seal_core_dumps(self) -> CoreDumpsSealed {
        let mut limits = Limits::default();
        rlimits::seal_core_dumps(&mut limits);
        // Captured here, while `/proc` can still be opened. See `rlimits::address_space_randomisation`.
        limits.aslr = rlimits::address_space_randomisation();
        CoreDumpsSealed { limits }
    }
}

/// Stage 1 complete: core dumps are sealed.
#[derive(Debug, Clone, Copy)]
pub struct CoreDumpsSealed {
    limits: Limits,
}

impl CoreDumpsSealed {
    /// Stage 2. `RLIMIT_MEMLOCK` soft limit to the hard limit.
    ///
    /// Before `mlockall`, `before any allocation`, and — for the unprivileged path — before
    /// `unshare(CLONE_NEWUSER)`, because the limit is accounted per uid and the unshare changes
    /// the effective one. See [`netns`].
    pub fn raise_memlock(mut self) -> MemlockRaised {
        rlimits::raise_memlock_to_hard_limit(&mut self.limits);
        MemlockRaised {
            limits: self.limits,
        }
    }
}

/// Stage 2 complete: the page-lock ceiling is as high as this process may ask for.
#[derive(Debug, Clone, Copy)]
pub struct MemlockRaised {
    limits: Limits,
}

impl MemlockRaised {
    /// Stage 3. Register the alternate signal stack, then `sigaction(SIGSEGV|SIGBUS, SA_ONSTACK)`.
    ///
    /// The stack must be a registered `SecureBlock`, so that the signal frame the kernel pushes
    /// onto it is scrubbed by the very handler that was about to write to it. `alt` carries
    /// that block's data pointer.
    pub fn install_tripwires(self, alt: AltStackRef) -> Result<TripwiresInstalled, JailError> {
        tripwire::install(&alt).map_err(JailError::Tripwire)?;
        Ok(TripwiresInstalled {
            limits: self.limits,
            alt,
        })
    }
}

/// Stage 3 complete: guard-page faults are caught and scrub.
#[derive(Debug, Clone, Copy)]
pub struct TripwiresInstalled {
    limits: Limits,
    alt: AltStackRef,
}

impl TripwiresInstalled {
    /// Stage 4. Establish every descriptor the session will ever have.
    ///
    /// The jail's filter contains no `openat`, so this is the *only* place a file can be
    /// named. `open` returns the session's context, which is threaded through the rest of the
    /// sequence and handed back by [`Sealed::run_session`] — so that the descriptors and the
    /// types that use them cannot drift apart.
    pub fn open_descriptors<T, E>(
        self,
        open: impl FnOnce() -> Result<T, E>,
    ) -> Result<Opened<T>, E> {
        Ok(Opened {
            limits: self.limits,
            alt: self.alt,
            context: open()?,
        })
    }
}

/// Stage 4 complete: the descriptors exist and nothing can open another.
#[derive(Debug, Clone, Copy)]
pub struct Opened<T> {
    limits: Limits,
    alt: AltStackRef,
    context: T,
}

impl<T> Opened<T> {
    /// Stage 5. `mlockall(MCL_CURRENT | MCL_FUTURE)`.
    ///
    /// Deliberately *after* the descriptors, not before. See the module comment.
    pub fn lock_all_pages(self) -> MemoryLocked<T> {
        let (locked, errno) = match rlimits::mlock_all() {
            Ok(()) => (true, 0),
            Err(e) => (false, e),
        };
        MemoryLocked {
            limits: self.limits,
            alt: self.alt,
            context: self.context,
            all_pages_locked: locked,
            mlock_errno: errno,
        }
    }
}

/// Stage 5 complete: nothing can reach swap.
#[derive(Debug, Clone, Copy)]
pub struct MemoryLocked<T> {
    limits: Limits,
    alt: AltStackRef,
    context: T,
    all_pages_locked: bool,
    mlock_errno: i32,
}

impl<T> MemoryLocked<T> {
    /// Stage 6. `unshare(CLONE_NEWNET)`, with the unprivileged path and the evidence.
    ///
    /// See [`netns`] for why the direct call is `EPERM` without `CAP_SYS_ADMIN` and why a user
    /// namespace is the way through.
    pub fn isolate_network(self) -> NetworkIsolated<T> {
        let (outcome, evidence) = netns::isolate_and_verify();
        NetworkIsolated {
            limits: self.limits,
            alt: self.alt,
            context: self.context,
            network: outcome,
            netns_evidence: evidence,
            all_pages_locked: self.all_pages_locked,
            mlock_errno: self.mlock_errno,
        }
    }
}

/// Stage 6 complete: no network namespace is shared with the host.
#[derive(Debug, Clone, Copy)]
pub struct NetworkIsolated<T> {
    limits: Limits,
    alt: AltStackRef,
    context: T,
    network: NetworkIsolation,
    netns_evidence: Option<(u64, u64)>,
    all_pages_locked: bool,
    mlock_errno: i32,
}

impl<T> NetworkIsolated<T> {
    /// Stage 7. `PR_SET_NO_NEW_PRIVS`.
    ///
    /// Last of the prerequisites, and immediately before the filter, because that is the only
    /// thing that reads it: an unprivileged `seccomp(SET_MODE_FILTER)` returns `EPERM` without
    /// it. It also independently prevents a `setuid` binary being mapped in to gain privilege.
    pub fn drop_privileges(self) -> Result<PrivilegesDropped<T>, JailError> {
        seccomp::set_no_new_privs().map_err(JailError::NoNewPrivs)?;
        // Read it back here, while `prctl` is still permitted, and carry the answer forward.
        //
        // The census found this the hard way: `BootReport` called `prctl(PR_GET_NO_NEW_PRIVS)` to
        // fill in its own report, which is read after the filter is installed, and the process was
        // killed on syscall 157.
        //
        // The tempting fix -- allowlist `prctl` -- is exactly wrong. `PR_SET_DUMPABLE` is a
        // `prctl`, and setting it back to 1 re-enables core dumps for a process whose entire
        // plaintext argument rests on `dumpable == 0`. A filter that permits `prctl` cannot claim
        // its core-dump seal; it can only claim to have applied one. So `prctl` stays forbidden and
        // the answer is carried.
        debug_assert!(
            seccomp::new_privs_is_set(),
            "PR_SET_NO_NEW_PRIVS returned success but the kernel disagrees"
        );
        Ok(PrivilegesDropped {
            limits: self.limits,
            alt: self.alt,
            context: self.context,
            network: self.network,
            netns_evidence: self.netns_evidence,
            all_pages_locked: self.all_pages_locked,
            mlock_errno: self.mlock_errno,
        })
    }
}

/// Stage 7 complete: the process cannot regain privilege by any route.
#[derive(Debug, Clone, Copy)]
pub struct PrivilegesDropped<T> {
    limits: Limits,
    alt: AltStackRef,
    context: T,
    network: NetworkIsolation,
    netns_evidence: Option<(u64, u64)>,
    all_pages_locked: bool,
    mlock_errno: i32,
}

impl<T> PrivilegesDropped<T> {
    /// Stage 8. Install the seccomp filter. After this returns the world is closed.
    ///
    /// Uses [`crate::seccomp::table::ALLOWLIST`].
    pub fn seal(self, filter: Action) -> Result<Sealed<T>, JailError> {
        self.seal_with(filter, crate::seccomp::table::ALLOWLIST)
    }

    /// Stage 8 with a caller-supplied table.
    ///
    /// Exists for the census harness, which must install a deliberately *incomplete* table and then
    /// observe what the session reaches for. Without it the harness would have to bypass the boot
    /// sequence to test the filter, and then it would not be testing the boot.
    ///
    /// An empty table would install a filter that permits nothing -- not even the `exit_group` the
    /// harness needs to finish -- so it is rejected rather than left to produce a confusing death.
    pub fn seal_with(
        self,
        filter: Action,
        entries: &[crate::seccomp::table::Allowed],
    ) -> Result<Sealed<T>, JailError> {
        if entries.is_empty() {
            return Err(JailError::Seccomp(crate::SeccompError::EmptyTable));
        }
        let program = Program::build(filter, entries).map_err(JailError::Seccomp)?;
        let installed = program.install().map_err(JailError::Seccomp)?;
        Ok(Sealed {
            limits: self.limits,
            alt: self.alt,
            context: self.context,
            network: self.network,
            netns_evidence: self.netns_evidence,
            all_pages_locked: self.all_pages_locked,
            mlock_errno: self.mlock_errno,
            filter: installed,
            teardown: TeardownPlan::new(),
        })
    }
}

/// Stage 8 complete. The process can never issue a syscall outside [`seccomp::table`] again.
#[derive(Debug)]
pub struct Sealed<T> {
    limits: Limits,
    /// Kept so the boot report can state the alternate stack's extent, which is the one piece
    /// of the tripwire configuration a post-mortem reader will want.
    alt: AltStackRef,
    context: T,
    network: NetworkIsolation,
    netns_evidence: Option<(u64, u64)>,
    all_pages_locked: bool,
    mlock_errno: i32,
    filter: Installed,
    teardown: TeardownPlan,
}

impl<T> Sealed<T> {
    /// The session context built by stage 4.
    pub fn context(&self) -> &T {
        &self.context
    }

    /// The session context mutably, for a session that has to edit what the boot built.
    ///
    /// The container is the reason: the session commits *through* the handle the boot opened,
    /// because after the filter there is no `openat` with which to open a second one. That makes
    /// `&Descriptors` the wrong shape -- [`Descriptors::container`] has to be mutable to commit --
    /// and a `&Self` borrow of the sealed state cannot express it.
    pub fn context_mut(&mut self) -> &mut T {
        &mut self.context
    }

    /// The session context, consumed.
    pub fn into_context(self) -> T {
        self.context
    }

    /// Which filter is in force.
    pub const fn filter(&self) -> &Installed {
        &self.filter
    }

    /// The alternate signal stack the tripwire runs on.
    pub const fn alt_stack(&self) -> &AltStackRef {
        &self.alt
    }

    /// Whether the network namespace was actually created.
    pub const fn network_isolation(&self) -> NetworkIsolation {
        self.network
    }

    /// The `(before, after)` namespace inodes, if `/proc` could be read.
    pub const fn netns_evidence(&self) -> Option<(u64, u64)> {
        self.netns_evidence
    }

    /// What the boot learned.
    pub fn report(&self) -> BootReport {
        BootReport {
            limits: self.limits,
            all_pages_locked: self.all_pages_locked,
            mlock_errno: self.mlock_errno,
            aslr: self.limits.aslr,
            // Carried from stage 7 rather than re-read: `prctl` is not in the filter, on purpose.
            no_new_privs: true,
        }
    }

    /// Build the teardown plan. Still permitted to allocate — this runs before the session.
    pub fn teardown(&mut self) -> &mut TeardownPlan {
        &mut self.teardown
    }

    /// Stage 9. Run the session.
    ///
    /// `&mut self`, so the session can register its teardown actions -- the ring buffer's range,
    /// the descriptors to release, the DRM buffer to destroy. They are registered through pointers
    /// and fds the session captured *during boot*, because by now it cannot allocate a plan and it
    /// cannot open anything to learn a new address.
    ///
    /// Nothing in `session` may allocate or issue an unlisted syscall. The seccomp filter enforces
    /// the second absolutely; the first is a discipline, and Phase 6's counting-allocator harness
    /// is what proves it.
    pub fn run_session<R>(&mut self, session: impl FnOnce(&mut Self) -> R) -> R {
        session(self)
    }

    /// Orderly exit: noise the ring, scrub every block, release the hardware, `_exit(0)`.
    pub fn teardown_and_exit(self, report_fd: i32) -> ! {
        self.teardown.run_and_exit(report_fd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_boot_chain_has_no_shortcut() {
        // Not a compile-time assertion -- a doc-level one. What it does check is that the
        // only way to name `Sealed` is through `PrivilegesDropped::seal`, and that the chain
        // runs in the order the module comment claims.
        let report: fn(&Sealed<u8>) -> BootReport = Sealed::report;
        let _ = report;
    }

    #[test]
    fn stage_types_carry_the_session_context_through() {
        fn at_stage_9<T: Copy>(sealed: &Sealed<T>) -> &T {
            sealed.context()
        }
        let _ = at_stage_9::<u8>;
    }

    #[test]
    fn mlockall_reports_errno_rather_than_a_bool() {
        // A boot that cannot say *why* it is unlocked cannot be acted on.
        match rlimits::mlock_all() {
            Ok(()) | Err(_) => {}
        }
    }
}
