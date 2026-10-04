//! The guard-page tripwire: `SIGSEGV`/`SIGBUS` → scrub → exit 137.
//!
//! # The sequence, and what each step is for
//!
//! ```text
//! SIGSEGV / SIGBUS  (SA_SIGINFO | SA_ONSTACK | SA_NODEFER)
//!   1. reentrancy guard          -- a fault inside this handler must not recurse
//!   2. read si_addr, classify   -- was it a *registered* guard, or an unowned address?
//!   3. zero the signal frame    -- the faulting register state, which rt_sigreturn would restore
//!   4. zero caller-saved state  -- see "register zeroing", below
//!   5. registry::scrub_all()    -- every live SecureBlock payload, volatile, no locks
//!   6. write one report line    -- async-signal-safe, to a pre-opened fd
//!   7. _exit(137)               -- never unwinds, never flushes stdio
//! ```
//!
//! # Why `_exit(137)` and not `exit`
//!
//! `exit(3)` runs atexit handlers, flushes stdio, and with `panic = "abort"` in the release
//! profile there is no unwinder to speak of anyway -- but `exit` still tries. Every one of
//! those steps is a chance to touch memory, take a lock another thread might hold, or write
//! a buffer that has not been scrubbed. `_exit(137)` is a single syscall to
//! `exit_group(2)`.
//!
//! 137 is `128 + 9`, the conventional shell encoding of "died on SIGKILL", and it is used
//! here for the same reason `SIGKILL` is: the outcome is not negotiable by anything the
//! process itself can do at that point.
//!
//! # Register zeroing, stated honestly
//!
//! Step 4 clears this handler's own caller-saved registers with inline `xor`, because it was
//! asked for and because it costs nothing. **It is not the load-bearing step, and it is worth
//! being clear about why, because the distinction is easy to get backwards.**
//!
//! The register state that actually holds the faulting context is not the handler's. It is in
//! the `ucontext_t` the kernel pushed onto the alternate stack, which `rt_sigreturn` would
//! restore if we returned. We never return, so the only version of that context that can leak
//! is the copy on the alternate stack -- and the alternate stack is a registered
//! [`SecureBlock`](crate::registry), so step 5 overwrites it wholesale. Step 3 exists to do
//! that explicitly, for the case where the caller supplied an unregistered stack.
//!
//! And in the other direction: the kernel destroys a task's registers when the process exits,
//! regardless of what they contained. So register contents do not outlive `_exit` either.
//! What *does* outlive the process is memory -- swap, a core file, another `fork`. That is why
//! step 5 is the one that matters, and why [`crate::rlimits::seal_core_dumps`] is step 1 of the
//! boot sequence: the registers are already gone by the time the core file would be written.
//!
//! The `cld` in the asm is the one part of step 4 with a consequence for the code that runs
//! after it: direction-flag state is caller-saved on SysV, and a set `DF` would make every
//! subsequent `rep movs` run backwards.
//!
//! # `SA_NODEFER`, which is the opposite of the usual advice
//!
//! Blocked-signal handling means a fault *inside* this handler cannot be delivered: the kernel
//! forces the default action and the process dies on `SIGSEGV` with exit 139 and no scrub.
//! `SA_NODEFER` lets the nested signal in, the reentrancy guard at step 1 catches it, and the
//! process exits 137 having scrubbed. That is the outcome the gate asserts, so `SA_NODEFER`
//! is the correct choice for this handler specifically.
//!
//! # `SI_KERNEL`, `SIGBUS` and faults that are not ours
//!
//! `SIGBUS` is handled because a guard page is `BUS_ADRERR` on some paths and because
//! `SIGBUS` is what a `SIGSEGV` becomes when the faulting address is not readable at all.
//! The exit code does not depend on which arrived: containment was lost either way.
//!
//! A `SIGSEGV` that is *not* at a registered guard (`FaultSite::Unregistered`) still scrubs and
//! still exits 137. It is reported as `site=unregistered` so the report distinguishes "the
//! guard worked" from "something dereferenced a wild pointer", which matters when reading a
//! crash after the fact. Refusing to scrub on that basis would be worse: the two are
//! indistinguishable from inside a handler that has already lost control of the process.

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, AtomicU8, AtomicUsize, Ordering};

use crate::altstack::AltStack;
use crate::registry::{self, FaultSite};
use crate::SigFault;

/// Exit code for any containment loss. See the module comment.
pub const TRIPWIRE_EXIT: i32 = 137;

/// Why the tripwire handlers could not be installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TripwireError {
    /// No alternate signal stack, so `SA_ONSTACK` would be a lie.
    NoAltStack,
    /// An alternate signal stack is registered, but it is not the one the caller passed.
    ///
    /// Distinct from [`TripwireError::NoAltStack`] because the fix differs: this one means the
    /// handler would run on some *other* stack -- in practice the runtime's 8 KiB default --
    /// and `SA_ONSTACK` would be true while providing none of the protection it promises.
    WrongAltStack,
    /// `sigaction` returned non-zero. Carries `errno`.
    Refused(i32),
}

impl core::fmt::Display for TripwireError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NoAltStack => f.write_str("no alternate signal stack is registered"),
            Self::WrongAltStack => {
                f.write_str("a different alternate signal stack is registered than the one given")
            }
            Self::Refused(e) => write!(f, "sigaction refused: errno {e}"),
        }
    }
}

impl std::error::Error for TripwireError {}

static IN_HANDLER: AtomicBool = AtomicBool::new(false);

/// Where the one-line report goes. Defaults to stderr; redirected by [`set_report_fd`].
static REPORT_FD: AtomicI32 = AtomicI32::new(2);

/// Set once the handlers are installed.
static INSTALLED: AtomicU8 = AtomicU8::new(0);

/// Faulting address of the first trip, for a test to assert on after a `fork`-ed child died.
static LAST_FAULT_ADDR: AtomicU64 = AtomicU64::new(0);

/// `FaultSite` of the first trip, as its discriminant.
static LAST_FAULT_SITE: AtomicU8 = AtomicU8::new(0);

/// Bytes scrubbed by the first trip.
static LAST_SCRUBBED: AtomicU64 = AtomicU64::new(0);

/// Send the tripwire report to `fd`.
///
/// Must be an `open` descriptor: the handler cannot open one, and the whole point is that
/// after [`crate::BootStep`] reaches the sealed state the filter may not contain `openat`.
fn set_report_fd(fd: i32) {
    REPORT_FD.store(fd, Ordering::Release);
}

/// The fd the tripwire report is written to.
fn report_fd() -> i32 {
    REPORT_FD.load(Ordering::Acquire)
}

/// Install the `SIGSEGV` and `SIGBUS` handlers, on the alternate stack.
///
/// `alt` is required rather than optional: installing `SA_ONSTACK` without a registered
/// alternate stack is not a degraded tripwire, it is a tripwire that behaves identically to
/// one without `SA_ONSTACK`, except that it lies about doing so.
pub fn install(alt: &AltStack) -> Result<(), TripwireError> {
    match AltStack::current() {
        // Identity, not mere presence. This crate is the one place that can get it wrong with a
        // plausible-looking result, because a runtime-installed stack is always present -- see
        // `AltStack::install`.
        Some(current) if current == *alt => {}
        Some(_) => return Err(TripwireError::WrongAltStack),
        None => return Err(TripwireError::NoAltStack),
    }
    for signal in [libc::SIGSEGV, libc::SIGBUS] {
        // SAFETY: a `sigaction` needs a valid `sigaction`; `handler` is an
        // `extern "C"` function with the mandated signature, and the flags are a plain mask.
        let rc = unsafe {
            libc::sigaction(
                signal,
                &libc::sigaction {
                    sa_sigaction: tripwire as *const () as libc::sighandler_t,
                    sa_mask: blocked_everything(),
                    sa_flags: libc::SA_SIGINFO | libc::SA_ONSTACK | libc::SA_NODEFER,
                    sa_restorer: None,
                },
                std::ptr::null_mut(),
            )
        };
        if rc != 0 {
            return Err(TripwireError::Refused(
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EINVAL),
            ));
        }
    }
    ALT_BASE.store(alt.base(), Ordering::Release);
    ALT_LEN.store(alt.len(), Ordering::Release);
    INSTALLED.store(1, Ordering::Release);
    Ok(())
}

/// Whether [`install`] has run in this process.
pub fn is_installed() -> bool {
    INSTALLED.load(Ordering::Acquire) != 0
}

/// `sa_mask`: every signal blocked while the handler runs.
fn blocked_everything() -> libc::sigset_t {
    crate::blocked_all()
}

// SAFETY: the handler's signature is the one `sigaction` mandates for `SA_SIGINFO`, and it
// only calls async-signal-safe functions: atomics, volatile stores, `write`, `_exit`.
unsafe extern "C" fn tripwire(
    signal: libc::c_int,
    info: *mut libc::siginfo_t,
    ctx: *mut libc::c_void,
) {
    // Step 1. `SA_NODEFER` lets a nested fault in here be delivered, so this is reachable.
    // Recursing would mean pushing another frame onto a stack that is mid-scrub.
    if IN_HANDLER.swap(true, Ordering::AcqRel) {
        // SAFETY: `_exit` is a single syscall to `exit_group` and is async-signal-safe.
        unsafe { libc::_exit(TRIPWIRE_EXIT) };
    }

    // Step 2.
    // SAFETY: `info` is the kernel's `siginfo_t` and `si_addr` is a valid accessor for
    // SIGSEGV/SIGBUS. A null `info` is tolerated rather than dereferenced.
    let fault = if info.is_null() {
        None
    } else {
        // SAFETY: `info` is the kernel's `siginfo_t`, whose layout for SIGSEGV/SIGBUS is
        // `SigFault`. See its doc comment -- and `tests/guard_page.rs` checks the layout
        // empirically rather than trusting it.
        Some(unsafe { core::ptr::read_unaligned(info.cast::<SigFault>()) })
    };
    let addr = fault.map_or(0, |f| f.addr as usize);
    let site = registry::classify(addr);

    // Step 3.
    unsafe { zero_signal_frame(ctx) };

    let blocks = registry::active_count();
    // Step 5. This is the step that matters; see the module comment.
    let scrubbed = registry::scrub_all();

    // Whether the handler really is running on the alternate stack.
    //
    // Reported rather than assumed, because `SA_ONSTACK` is a *request* to the kernel and the
    // kernel's compliance is exactly what fails when the alternate stack is missing or exhausted
    // -- the failure mode this module exists to prevent.
    //
    // Answered from the record `install` left behind, not from a fresh `sigaltstack`: with
    // `SS_AUTODISARM` set, the kernel reports `SS_DISABLE` to `sigaltstack` from inside a handler
    // running on that stack, which is the whole point of the flag. See
    // [`AltStack::contains_address`].
    let altstack = installed_altstack();
    // A stack address, so a wrong answer is diagnosable rather than merely wrong.
    let handler_sp = &altstack as *const Option<AltStack> as usize;
    let on_altstack = altstack.is_some_and(|alt| alt.contains_address(handler_sp));
    // What the kernel says, for the record. `disabled` is the expected answer here and is not an
    // error; a nonzero `sp` would mean autodisarm is *not* in effect and a nested fault could
    // reuse a consumed stack.
    let kernel_says = kernel_altstack_state();

    // Record for a same-process observer (used by the `fork`-based unit test, where the
    // child's `_exit` is observed by the parent rather than this process).
    LAST_FAULT_ADDR.store(addr as u64, Ordering::Release);
    LAST_FAULT_SITE.store(site as u8, Ordering::Release);
    LAST_SCRUBBED.store(scrubbed as u64, Ordering::Release);

    // Step 6.
    unsafe {
        emit_report(&Report {
            signal,
            addr,
            site,
            blocks,
            scrubbed,
            on_altstack,
            handler_sp,
            altstack,
            kernel_says,
        });
    }

    // Step 4 -- **last**, after the report rather than before the scrub.
    //
    // The first version ran it second, straight after classifying, and it silently corrupted the
    // report: `signal` came back as 0 on every run instead of 11. The reason is that the handler's
    // arguments arrive in caller-saved registers (`rdi`, `rsi`, `rdx`), and `signal` is not read
    // until the report is built -- by which point this function had zeroed the very register it
    // was still living in. `addr` survived because it is copied out of the signal frame into a
    // local at the top; `signal` had no such copy.
    //
    // Moving it here costs nothing and removes the whole class of bug: after this, no handler
    // argument is read again, so no argument can be destroyed by the routine that destroys
    // registers. It also cannot be placed *after* `_exit`, so this is as late as is correct.
    //
    // Step 7.
    unsafe {
        zero_caller_saved_state();
        libc::_exit(TRIPWIRE_EXIT);
    }
}

/// Overwrite the saved register context with zeroes.
///
/// This is the state `rt_sigreturn` would put back into the registers. Zeroing `rip` and
/// `rsp` along with the rest is not meaningful -- we never return -- but leaving them out
/// would mean selectively trusting two of twenty-three fields, which is a worse invariant to
/// reason about than "the frame is all zeroes".
///
/// # Safety
///
/// `ctx` is the kernel's `ucontext_t` for a signal handler, which is valid for reads and
/// writes of its `uc_mcontext`.
unsafe fn zero_signal_frame(ctx: *mut libc::c_void) {
    if ctx.is_null() {
        return;
    }
    let gregs = core::ptr::addr_of_mut!((*ctx.cast::<libc::ucontext_t>()).uc_mcontext.gregs)
        as *mut libc::greg_t;
    let mut i = 0usize;
    while i < crate::NGREG {
        // SAFETY: `gregs` is an array of `libc::greg_t` (u64) and `NGREG` is its length.
        unsafe { core::ptr::write_volatile(gregs.add(i), 0) };
        i += 1;
    }
    // The x87/SSE state in the same frame is deliberately *not* reached for.
    //
    // On glibc `mcontext_t` has a `fpregs` pointer; on musl it does not -- libc replaces
    // `fpregs` and `__reserved1` with a single private `__private: [u64; 9]`, so the fpstate's
    // address is not reachable through libc at all. Computing its offset from the documented
    // struct layout would mean writing 512 bytes at a hand-derived offset inside a kernel-owned
    // frame, which is a worse failure mode than not doing it.
    //
    // It is also unnecessary, and not merely as a fallback: the fpstate lives inside the
    // alternate stack, and the alternate stack is a registered `SecureBlock`, so
    // `registry::scrub_all` -- two lines below, in the caller -- overwrites every byte of it,
    // fpstate included. The scrub is the stronger mechanism and it does not depend on libc
    // exposing a field.
    core::sync::atomic::compiler_fence(Ordering::SeqCst);
}

/// Zero this handler's own caller-saved register state, and clear `DF`.
///
/// # Safety
///
/// Safe to call anywhere. Declares every register it writes as clobbered, so the compiler
/// cannot have kept a live value there across this point.
#[cfg(target_arch = "x86_64")]
unsafe fn zero_caller_saved_state() {
    // `xorps` zeroes the low 128 bits of an XMM register, which is the whole of it without
    // AVX. The ymm upper halves are untouched, and that is not an oversight to fix: the
    // Phase 9 target is a Core 2 Duo, which has no AVX, so there is no upper state to zero.
    // xmm0-15 cover every caller-saved vector register on the SysV ABI.
    //
    // **Intel syntax, not AT&T.** Rust's `asm!` defaults to Intel syntax on x86 -- `att_syntax`
    // is opt-in -- so registers are written bare. Two wrong turns are recorded here because the
    // compiler's error for each is the same unhelpful "unknown token in expression":
    //
    // * `xor %rax, %rax` fails: a single `%` is Rust's operand-reference escape, so `%rax` names
    //   an operand that does not exist.
    // * `xor %%rax, %%rax` also fails, because Intel-syntax LLVM has no `%` prefix at all.
    //
    // The `out(..) _` declarations are what tell the compiler the registers are written, and
    // they are needed independently of the template text.
    core::arch::asm!(
        "cld",
        "xorps xmm0,  xmm0",
        "xorps xmm1,  xmm1",
        "xorps xmm2,  xmm2",
        "xorps xmm3,  xmm3",
        "xorps xmm4,  xmm4",
        "xorps xmm5,  xmm5",
        "xorps xmm6,  xmm6",
        "xorps xmm7,  xmm7",
        "xorps xmm8,  xmm8",
        "xorps xmm9,  xmm9",
        "xorps xmm10, xmm10",
        "xorps xmm11, xmm11",
        "xorps xmm12, xmm12",
        "xorps xmm13, xmm13",
        "xorps xmm14, xmm14",
        "xorps xmm15, xmm15",
        "xor  rax,    rax",
        "xor  rcx,    rcx",
        "xor  rdx,    rdx",
        "xor  rsi,    rsi",
        "xor  rdi,    rdi",
        "xor  r8,     r8",
        "xor  r9,     r9",
        "xor  r10,    r10",
        "xor  r11,    r11",
        out("xmm0")  _, out("xmm1")  _, out("xmm2")  _, out("xmm3")  _,
        out("xmm4")  _, out("xmm5")  _, out("xmm6")  _, out("xmm7")  _,
        out("xmm8")  _, out("xmm9")  _, out("xmm10") _, out("xmm11") _,
        out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
        out("rax")   _, out("rcx")   _, out("rdx")   _,
        out("rsi")   _, out("rdi")   _,
        out("r8")    _, out("r9")    _, out("r10")   _, out("r11")   _,
        options(nostack),
    );
    // `cld` is invisible to the compiler, and a caller-saved flag is exactly the kind of
    // state a compiler is entitled to assume it set up. Say so explicitly.
    core::sync::atomic::compiler_fence(Ordering::SeqCst);
}

/// Non-x86-64 fallback: nothing to do.
///
/// The register state of a foreign ABI is not this crate's business, and the guard pages and
/// the registry scrub -- the parts that matter -- are architecture-independent.
#[cfg(not(target_arch = "x86_64"))]
unsafe fn zero_caller_saved_state() {}

/// A fixed-size, allocation-free line builder for the handler's report.
///
/// `format!` is not usable here: it allocates, and the allocator may be in any state at all
/// when a signal handler runs.
struct ReportBuf {
    /// 256, not the 192 this started at, because a truncated report is worse than a short one.
    ///
    /// The 192-byte version cut off `exit=137` -- the single most important field -- on exactly
    /// the runs where `RLIMIT_CORE` was `RLIM_INFINITY` and printed as twenty digits of
    /// `18446744073709551615`. That was the unsealed *control*, so the truncation landed precisely
    /// on the evidence a post-mortem needs most. The buffer is now comfortably larger than the
    /// longest possible line rather than sized to the common one.
    bytes: [u8; 256],
    at: usize,
}

impl ReportBuf {
    const fn new() -> Self {
        Self {
            bytes: [0u8; 256],
            at: 0,
        }
    }

    fn str(&mut self, s: &str) {
        for byte in s.as_bytes() {
            self.byte(*byte);
        }
    }

    fn dec(&mut self, mut v: u64) {
        let mut digits = [0u8; 20];
        let mut n = 0usize;
        loop {
            digits[n] = b'0' + (v % 10) as u8;
            n += 1;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        while n > 0 {
            n -= 1;
            self.byte(digits[n]);
        }
    }

    fn hex(&mut self, mut v: u64) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        if v == 0 {
            self.byte(b'0');
            return;
        }
        let mut digits = [0u8; 16];
        let mut n = 0usize;
        while v != 0 {
            digits[n] = HEX[(v % 16) as usize];
            n += 1;
            v /= 16;
        }
        while n > 0 {
            n -= 1;
            self.byte(digits[n]);
        }
    }

    fn byte(&mut self, b: u8) {
        if self.at < self.bytes.len() {
            self.bytes[self.at] = b;
            self.at += 1;
        }
    }
}

/// `"holonomy-tripwire ..."`, and its length.
///
/// One syscall, no formatting library, no allocation: the report has to survive the condition
/// it is reporting. The fields are, in order:
///
/// ```text
/// holonomy-tripwire signal=11 addr=0x7f... site=guard blocks=2 scrubbed=65536 \
///     stack=on-alt core=0 exit=137
/// ```
///
/// * `site` -- `guard`, `data` or `unregistered`: containment held, a wild write into live
///   data, or an address the registry has never heard of.
/// * `scrubbed` -- bytes of `SecureBlock` payload overwritten. Compare against the sum of the
///   registered payload lengths to prove the scrub was complete.
/// * `stack=on-alt` -- the handler is running on the alternate stack, which is the fact
///   `SA_ONSTACK` is supposed to guarantee and cannot be checked any other way.
/// * `core=0` -- `RLIMIT_CORE` re-read at fault time. Must be 0; a non-zero value means the
///   plaintext was eligible for a core file at the moment it was scrubbed.
/// * `exit=137` -- the exit code, restated so a log line is self-contained.
///
/// # Safety
///
/// Safe to call from a signal handler; the buffer is a local.
/// Everything the one-line report needs, gathered so the builder stays readable.
///
/// A struct rather than eight positional parameters because `emit_report` went over clippy's
/// argument limit once the alternate-stack diagnostics were added -- and a report whose fields are
/// matched by position is one that silently transposes two numbers the next time it grows.
#[derive(Debug, Clone, Copy)]
struct Report {
    /// Signal number the kernel delivered.
    signal: libc::c_int,
    /// `si_addr`, as the kernel reported it.
    addr: usize,
    /// Where that address sat relative to the registry.
    site: FaultSite,
    /// Blocks in the registry at fault time.
    blocks: usize,
    /// Payload bytes overwritten.
    scrubbed: usize,
    /// Whether the handler is running on the alternate stack.
    on_altstack: bool,
    /// Address of a local in the handler, i.e. roughly where its frame is.
    handler_sp: usize,
    /// The alternate stack as recorded at install time.
    altstack: Option<AltStack>,
    /// What `sigaltstack` said when asked from inside the handler. `0` is not expected here;
    /// `SS_DISABLE` (2) is, and is what the jail wants to see.
    kernel_says: libc::c_int,
}

/// # Safety
///
/// Safe to call from a signal handler; the buffer is a local.
unsafe fn emit_report(report: &Report) {
    let mut buf = ReportBuf::new();
    buf.str("holonomy-tripwire signal=");
    buf.dec(report.signal as u64);
    buf.str(" addr=0x");
    buf.hex(report.addr as u64);
    buf.str(" site=");
    buf.str(report.site.as_str());
    buf.str(" blocks=");
    buf.dec(report.blocks as u64);
    buf.str(" scrubbed=");
    buf.dec(report.scrubbed as u64);
    buf.str(" stack=");
    buf.str(if report.on_altstack {
        "on-alt"
    } else {
        "NOT-ALT"
    });
    buf.str(" sp=0x");
    buf.hex(report.handler_sp as u64);
    buf.str(" alt=0x");
    buf.hex(report.altstack.map_or(0, |a| a.base()) as u64);
    buf.str("+");
    buf.dec(report.altstack.map_or(0, |a| a.len()) as u64);
    buf.str(" kflags=0x");
    buf.hex(report.kernel_says as u32 as u64);
    buf.str(" core=");
    // `inf` rather than 18446744073709551615: shorter, and `RLIM_INFINITY` read back is exactly
    // what an unsealed process looks like.
    match crate::rlimits::getrlimit(libc::RLIMIT_CORE) {
        Some((soft, _)) if soft != u64::MAX => buf.dec(soft),
        _ => buf.str("inf"),
    }
    buf.str(" dumpable=");
    buf.dec(read_dumpable() as u64);
    buf.str(" exit=");
    buf.dec(TRIPWIRE_EXIT as u64);
    buf.byte(b'\n');
    // SAFETY: `buf.bytes[..buf.at]` is initialised, and `write` is async-signal-safe.
    unsafe {
        libc::write(report_fd(), buf.bytes.as_ptr().cast(), buf.at);
    }
}

/// `PR_GET_DUMPABLE`, or -1 if the call failed.
///
/// Read at fault time rather than remembered from boot, because "was it sealed" is exactly the
/// question a post-mortem is asking.
fn read_dumpable() -> libc::c_int {
    // SAFETY: `PR_GET_DUMPABLE` writes one `int` and reads none.
    unsafe { libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) }
}

/// The alternate stack `install` was given, in two atomics rather than one.
///
/// The handler cannot ask the kernel (see [`AltStack::contains_address`]) and cannot be handed an
/// `&AltStack` either -- the handler's signature is fixed by `sigaction`. So the installation is
/// recorded here, at install time, which is the only point at which the value is both available
/// and unambiguous.
///
/// Two `AtomicUsize` rather than one packed word: packing needs the halves disjoint, and `len` is
/// a full-width `usize` on every target, so it is not.
static ALT_BASE: AtomicUsize = AtomicUsize::new(0);
static ALT_LEN: AtomicUsize = AtomicUsize::new(0);

/// The recorded alternate stack, or `None` if [`install`] has not run.
fn installed_altstack() -> Option<AltStack> {
    let len = ALT_LEN.load(Ordering::Acquire);
    if len == 0 {
        return None;
    }
    let base = ALT_BASE.load(Ordering::Acquire);
    if base == 0 {
        return None;
    }
    Some(AltStack::recorded(base, len))
}

/// Ask the kernel what it thinks the alternate stack is, from inside the handler.
///
/// Returns the `ss_flags` it reported, or `-1` if the call failed. Under `SS_AUTODISARM` the
/// expected answer is `SS_DISABLE` (2): the stack is deliberately disabled while a handler runs on
/// it. An answer with the `SS_ONSTACK` bit set would mean autodisarm is not in effect.
fn kernel_altstack_state() -> libc::c_int {
    let mut probe = libc::stack_t {
        ss_sp: std::ptr::null_mut(),
        ss_flags: 0,
        ss_size: 0,
    };
    // SAFETY: `probe` is a valid, writable `stack_t`.
    if unsafe { libc::sigaltstack(std::ptr::null(), &mut probe) } != 0 {
        return -1;
    }
    probe.ss_flags
}

/// What the first trip recorded, for a test or a post-mortem helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TripReport {
    /// Faulting address, as `si_addr` reported it.
    pub addr: u64,
    /// Where that address sat relative to the registry.
    pub site: FaultSite,
    /// Bytes of `SecureBlock` payload overwritten.
    pub scrubbed: u64,
    /// Blocks in the registry at the time.
    pub blocks: usize,
}

/// The first trip's record. Meaningless unless `is_installed()` is true.
pub fn last_trip() -> TripReport {
    TripReport {
        addr: LAST_FAULT_ADDR.load(Ordering::Acquire),
        site: match LAST_FAULT_SITE.load(Ordering::Acquire) {
            0 => FaultSite::Guard,
            1 => FaultSite::Data,
            _ => FaultSite::Unregistered,
        },
        scrubbed: LAST_SCRUBBED.load(Ordering::Acquire),
        blocks: registry::active_count(),
    }
}

/// Redirect the report. Test-and-boot hook; see [`set_report_fd`].
pub fn use_report_fd(fd: i32) {
    set_report_fd(fd);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::altstack::ALT_STACK_BYTES;

    #[test]
    fn report_buf_formats_without_allocating() {
        let mut buf = ReportBuf::new();
        buf.str("holonomy-tripwire signal=");
        buf.dec(11);
        buf.str(" addr=0x");
        buf.hex(0xdead_beef);
        buf.str(" site=guard\n");
        let text = core::str::from_utf8(&buf.bytes[..buf.at]).unwrap();
        assert_eq!(
            text,
            "holonomy-tripwire signal=11 addr=0xdeadbeef site=guard\n"
        );
    }

    #[test]
    fn report_buf_truncates_instead_of_overflowing() {
        let mut buf = ReportBuf::new();
        for _ in 0..1_000 {
            buf.str("0123456789");
        }
        assert_eq!(buf.at, buf.bytes.len());
    }

    #[test]
    fn hex_of_zero_is_a_single_digit() {
        let mut buf = ReportBuf::new();
        buf.hex(0);
        assert_eq!(&buf.bytes[..buf.at], b"0");
    }

    /// Every thread of a Rust binary arrives with an alternate signal stack already
    /// registered. Measured on this host with the musl target: 8,192 bytes, `ss_flags == 0`.
    ///
    /// Which is the whole reason the tripwire checks *identity* rather than presence. A
    /// presence check would pass here and the handler would run on the runtime's 8 KiB stack
    /// with no `SS_AUTODISARM` -- `SA_ONSTACK` true, and none of the protection it implies.
    #[test]
    fn a_runtime_installed_stack_is_present_and_is_not_the_one_we_want() {
        let Some(runtime_stack) = AltStack::current() else {
            // If a future toolchain stops installing one, the refusal path below is still
            // covered by `no_alternate_stack_is_refused`; say so rather than pretending.
            eprintln!("no pre-existing alternate signal stack on this thread");
            return;
        };
        assert!(
            !AltStack::current_has_autodisarm(),
            "the runtime's stack has SS_AUTODISARM set, which would make the identity check \
             below pass for the wrong reason"
        );

        // Install the real one, over the top.
        let region = vec![0u8; ALT_STACK_BYTES];
        let ours = crate::altstack::AltStack::install(region.as_ptr() as usize, region.len())
            .expect("a 64 KiB stack with SS_AUTODISARM installs over the runtime's");
        assert_eq!(ours.len(), ALT_STACK_BYTES);
        assert!(
            AltStack::current_has_autodisarm(),
            "SS_AUTODISARM must survive"
        );
        assert_eq!(
            ours.replaced().map(|old| (old.base, old.len)),
            Some((runtime_stack.base(), runtime_stack.len())),
            "the displaced stack must be reported, and must be the runtime's"
        );

        // Ours is now registered, so asking for the *runtime's* must be refused by identity.
        assert_eq!(
            install(&runtime_stack),
            Err(TripwireError::WrongAltStack),
            "installing for a stack that is not the registered one must be refused, not accepted \
             with a silently useless SA_ONSTACK"
        );

        // And with the registration removed, by presence.
        assert!(crate::altstack::AltStack::disable());
        assert!(AltStack::current().is_none());
        assert_eq!(install(&ours), Err(TripwireError::NoAltStack));
    }

    /// The success path is deliberately **not** exercised by any unit test.
    ///
    /// `sigaction` is process-wide, so a unit test that installed the real handler would make
    /// every later test in this binary run under a handler that exits 137. The real
    /// installation is exercised in `tests/guard_page.rs`, in a subprocess, which is the only
    /// place it belongs.
    #[test]
    fn the_success_path_is_exercised_only_in_a_subprocess() {
        assert!(!is_installed(), "no unit test may install the handler");
    }
}
