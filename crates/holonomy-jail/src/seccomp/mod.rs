//! The seccomp filter: assembled by hand from the table, installed once, closed forever.
//!
//! # Why hand-assembled BPF rather than a crate
//!
//! `seccompiler` and friends build the same program, and they are good. Two reasons for doing
//! it here anyway:
//!
//! 1. **The allowlist is the deliverable, and it should be greppable.** PROJECT.md §2.6 wants
//!    the census emitted and the list derived from it. If the list is a sequence of `.allow_all([...])`
//!    builder calls, the relationship between "what the census saw" and "what the filter
//!    permits" is a diff of Rust call syntax. If it is a `&'static [Allowed]` in
//!    [`table`], it is a table, and the census report can be printed as a diff against it.
//! 2. **It keeps the dependency edge honest.** `holonomy-jail` is a leaf with `libc` and
//!    nothing else. Adding a code-generation crate to the crate that seals the process is
//!    the kind of dependency that later wants to allocate.
//!
//! The program is four instructions of prologue, two per allowlisted number, and one epilogue:
//!
//! ```text
//!   ld    [4]                        ; seccomp_data.arch
//!   jeq   AUDIT_ARCH_X86_64, jt=1     ; match -> skip the kill
//!   ret   KILL                       ; wrong architecture is never negotiable
//!   ld    [0]                        ; seccomp_data.nr
//!   jset  0x40000000, jt=0           ; the x32 ABI marker bit; set -> fall through
//!   ret   KILL                       ; ...which means kill
//!   jeq   <nr>, jt=0 ; ret ACTION    ; per allowlisted number
//!   ret   KILL                       ; epilogue
//! ```
//!
//! # Why the linear chain and not a jump table
//!
//! A sorted binary search would be ~5 compares per syscall instead of ~55, and 55 compares is
//! about 55 ns of a syscall that already costs microseconds. A tree would be measurably
//! faster to *read as code* and measurably harder to read as a specification. The linear
//! chain is the specification: "these numbers, in this order, or die".
//!
//! # `SECCOMP_FILTER_FLAG_TSYNC` as an assertion, not a convenience
//!
//! The jail is single-threaded by design. `TSYNC` asks the kernel to apply the filter to
//! every thread and fails with `EAGAIN` if any thread cannot be synchronised. So it turns a
//! design invariant that would otherwise be a comment into an error: if something has
//! spawned a thread, the boot stops with a clear message instead of silently protecting one
//! of two threads.
//!
//! # Why `KILL_PROCESS` rather than `KILL`
//!
//! `SECCOMP_RET_KILL_PROCESS` (Linux 4.14+) kills the whole process group with `SIGSYS`
//! instead of delivering `SIGKILL` to one thread. Three differences, all of which matter:
//! a process that has spawned threads dies entirely; the signal is catchable in the
//! *parent*, so the gate can tell "killed by the filter" from "died on its own"; and the
//! choice is visible in the wait status as `SIGSYS` rather than `SIGKILL`, which is a
//! distinction a test can assert.
//!
//! Availability is probed with `SECCOMP_GET_ACTION_AVAIL` rather than assumed, and the
//! fallback to `RET_KILL` is recorded in [`SeccompFilter::action_used`] rather than being a
//! silent downgrade.

pub mod table;

use table::Allowed;

/// Instruction budget for [`Program`].
///
/// Six prologue/epilogue instructions plus two per allowlisted number. The table has ~55
/// entries, so this leaves room for it to roughly triple before it has to become an error
/// instead of a truncating write.
pub const MAX_INSTRUCTIONS: usize = 256;

/// What the filter does with a syscall it does not recognise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// `SECCOMP_RET_KILL`. The universal fallback, and what `RET_KILL_PROCESS` degrades to.
    Kill,
    /// `SECCOMP_RET_KILL_PROCESS`. Preferred: no thread survives, and `SIGSYS` is visible.
    KillProcess,
    /// `SECCOMP_RET_TRAP`: deliver `SIGSYS` so the census handler can record `si_syscall`.
    ///
    /// **Census only.** A trapped syscall does not run, so a session under this action
    /// cannot finish. It exists to be run repeatedly, with the allowlist growing between
    /// runs, until the session completes -- at which point the accumulated set is the exact
    /// set of syscalls the session issued. See [`crate::census`] for why that converges.
    Trap,
}

impl Action {
    /// The `SECCOMP_RET_*` value for this action.
    ///
    /// This is what happens to a syscall **the filter does not recognise** -- the deny arm. An
    /// allowlisted syscall is always `SECCOMP_RET_ALLOW`.
    ///
    /// The first version applied the action to *both* arms, so `Action::Trap` trapped the
    /// allowlisted syscalls too. Every session died with `SIGSYS` on its first `getrandom` and no
    /// census handler output at all, because... no: because `SECCOMP_RET_TRAP` on an allowlisted
    /// syscall delivers `SIGSYS` and the handler *does* run. What actually happened is subtler and
    /// worth recording: the handler ran, and its own `_exit(90)` was itself a syscall the filter
    /// rejected, so the process died of `SIGSYS` with an empty stderr. A deny-path filter whose own
    /// escape hatch is denied is not a filter, it is a brick.
    ///
    /// `KillProcess` is *not* downgraded here, and that is the point. The obvious place to
    /// decide whether the kernel supports it is `SECCOMP_GET_ACTION_AVAIL`, and on this host
    /// that call returns `EINVAL` for every action while `RET_KILL_PROCESS` demonstrably works
    /// -- so a probe-based decision would have installed the weaker filter on a kernel that
    /// fully supports the stronger one, invisibly, with every assertion still passing.
    ///
    /// [`Program::install`] therefore decides empirically: it installs with the requested action
    /// and, only if the kernel refuses with `EINVAL`, retries with `Kill`. The measurement is
    /// recorded in `rlimits`'s `seccomp_action_avail_is_unreliable` test.
    /// The name used in the boot report.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Kill => "kill",
            Self::KillProcess => "kill-process",
            Self::Trap => "trap",
        }
    }

    fn seccomp_value(self) -> u32 {
        match self {
            Self::Kill => libc::SECCOMP_RET_KILL,
            Self::KillProcess => libc::SECCOMP_RET_KILL_PROCESS,
            Self::Trap => libc::SECCOMP_RET_TRAP,
        }
    }
}

/// Why a filter could not be built or installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeccompError {
    /// The table needed more instructions than [`MAX_INSTRUCTIONS`].
    TooManyInstructions,
    /// An empty table was requested. Permitting nothing is never what a caller means, and it would
    /// produce a death that looks exactly like a filter bug.
    EmptyTable,
    /// `PR_SET_NO_NEW_PRIVS` has not been set. A process without it gets `EPERM` from
    /// `seccomp` unless it has `CAP_SYS_ADMIN`, and the error would be ambiguous with a
    /// genuine policy problem.
    NoNewPrivsRequired,
    /// The kernel rejected the program with an action the kernel may not support. Carries
    /// `errno`. Only `EINVAL` triggers the `KillProcess` -> `Kill` retry.
    Rejected(i32),
    /// The kernel rejected the program even with `SECCOMP_RET_KILL`, so the program itself is
    /// the problem rather than the action.
    ///
    /// Kept distinct from [`SeccompError::Rejected`] because that retry exists to handle one
    /// specific cause of `EINVAL` -- an unsupported action -- and folding the two together would
    /// report "unsupported action" for a malformed filter.
    ProgramRejected(i32),
    /// `SECCOMP_FILTER_FLAG_TSYNC` failed, which means a thread exists that the filter could
    /// not be applied to. The jail is single-threaded by design.
    ThreadOutOfSync(i32),
}

impl core::fmt::Display for SeccompError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooManyInstructions => write!(
                f,
                "seccomp program needs more than {MAX_INSTRUCTIONS} instructions"
            ),
            Self::EmptyTable => f.write_str("an empty syscall table would permit nothing at all"),
            Self::NoNewPrivsRequired => {
                f.write_str("PR_SET_NO_NEW_PRIVS must be set before the filter is installed")
            }
            Self::Rejected(e) => write!(f, "seccomp rejected the filter: errno {e}"),
            Self::ProgramRejected(e) => {
                write!(f, "seccomp rejected the program itself: errno {e}")
            }
            Self::ThreadOutOfSync(e) => write!(
                f,
                "seccomp TSYNC failed (errno {e}); a second thread exists, which the jail \
                 forbids because the filter would not cover it"
            ),
        }
    }
}

impl std::error::Error for SeccompError {}

/// An assembled filter, in a fixed buffer.
///
/// Fixed-size rather than `Vec` so that installing needs no allocation. That is not
/// theoretical: the whole point of the filter is that it goes on last, and an allocator call
/// between "allocate the program" and "install the program" is a window.
pub struct Program {
    insns: [libc::sock_filter; MAX_INSTRUCTIONS],
    len: usize,
    /// The action the program was built for, kept so the report can state what was installed.
    action: Action,
}

impl core::fmt::Debug for Program {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Program")
            .field("len", &self.len)
            .field("action", &self.action)
            .finish()
    }
}

impl Program {
    /// Assemble a filter that returns `action` for anything not in `entries`.
    pub fn build(action: Action, entries: &[Allowed]) -> Result<Self, SeccompError> {
        // The arch mismatch and the x32 bit are always fatal, in every mode: a wrong-ABI syscall
        // number means the table describes the wrong ABI, and there is nothing to census about it.
        let kill = Action::Kill.seccomp_value();
        // An allowlisted syscall is always allowed. Never `action`: see `Action::seccomp_value`.
        let allow = libc::SECCOMP_RET_ALLOW;
        // What an unrecognised syscall gets.
        let deny = action.seccomp_value();

        let mut insns = [libc::sock_filter {
            code: 0,
            jt: 0,
            jf: 0,
            k: 0,
        }; MAX_INSTRUCTIONS];
        let mut len = 0usize;

        let push = |insns: &mut [libc::sock_filter; MAX_INSTRUCTIONS],
                    len: &mut usize,
                    code: u16,
                    jt: u8,
                    jf: u8,
                    k: u32|
         -> Result<(), SeccompError> {
            if *len >= MAX_INSTRUCTIONS {
                return Err(SeccompError::TooManyInstructions);
            }
            insns[*len] = libc::sock_filter { code, jt, jf, k };
            *len += 1;
            Ok(())
        };

        // seccomp_data.arch is at offset 4: `struct seccomp_data { int nr; __u32 arch; ... }`.
        push(&mut insns, &mut len, BPF_LD_W_ABS, 0, 0, 4)?;
        push(
            &mut insns,
            &mut len,
            BPF_JMP_JEQ_K,
            1,
            0,
            crate::AUDIT_ARCH_X86_64,
        )?;
        // Wrong architecture: never negotiable. In census mode this is still `KILL`, because a
        // wrong-architecture syscall is not something the session should be reporting on --
        // it means the syscall numbers in the table are for the wrong ABI.
        push(&mut insns, &mut len, BPF_RET_K, 0, 0, kill)?;

        // seccomp_data.nr is at offset 0.
        push(&mut insns, &mut len, BPF_LD_W_ABS, 0, 0, 0)?;
        // The x32 ABI uses bit 30 of the syscall number. Refusing it closes the hole where a
        // 32-bit syscall number is smuggled past a 64-bit allowlist -- which matters here,
        // because the filter has no way to distinguish `read` from `__X32_SYSCALL + read`.
        push(&mut insns, &mut len, BPF_JMP_JSET_K, 0, 1, 0x4000_0000)?;
        push(&mut insns, &mut len, BPF_RET_K, 0, 0, kill)?;

        for entry in entries {
            push(&mut insns, &mut len, BPF_JMP_JEQ_K, 0, 1, entry.nr as u32)?;
            push(&mut insns, &mut len, BPF_RET_K, 0, 0, allow)?;
        }
        push(&mut insns, &mut len, BPF_RET_K, 0, 0, deny)?;

        Ok(Self { insns, len, action })
    }

    /// Assemble the production filter from [`table::ALLOWLIST`].
    pub fn build_default(action: Action) -> Result<Self, SeccompError> {
        Self::build(action, table::ALLOWLIST)
    }

    /// Number of instructions in the assembled program.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// An assembled filter is never empty; the prologue alone is six instructions.
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The action this program was built for, as requested. See [`Action::seccomp_value`] for
    /// the downgrade case.
    pub const fn action(&self) -> Action {
        self.action
    }

    /// Install the filter. After this returns, the process can never issue a syscall outside
    /// the table again, and cannot widen the filter: `seccomp` allows only
    /// `SECCOMP_SET_MODE_FILTER` with `SECCOMP_FILTER_FLAG_TS_ESRCH`, which is strictly *more*
    /// restrictive.
    pub fn install(&self) -> Result<Installed, SeccompError> {
        if !new_privs_is_set() {
            return Err(SeccompError::NoNewPrivsRequired);
        }
        let requested = self.action;
        let strong = requested.seccomp_value();
        match self.try_install(strong) {
            Ok(value) => Ok(Installed {
                action: requested,
                value,
                instructions: self.len,
                allowlisted: self.allowlisted(),
                downgraded: false,
            }),
            // `EINVAL` from `SECCOMP_SET_MODE_FILTER` means either "this action is not
            // supported" (Linux < 4.14, for `KILL_PROCESS`) or "this program is malformed".
            // Retrying with `KILL` is what tells the two apart.
            //
            // Retrying is safe because a rejected `seccomp` call installs nothing at all: the
            // kernel validates the whole program and the action before touching any thread's
            // filter list.
            Err(SeccompError::Rejected(libc::EINVAL)) if requested == Action::KillProcess => {
                let weak = Action::Kill.seccomp_value();
                match self.try_install(weak) {
                    Ok(value) => Ok(Installed {
                        action: Action::Kill,
                        value,
                        instructions: self.len,
                        allowlisted: self.allowlisted(),
                        downgraded: true,
                    }),
                    Err(_) => Err(SeccompError::ProgramRejected(libc::EINVAL)),
                }
            }
            Err(e) => Err(e),
        }
    }

    /// Allowlisted entries the program encodes, recovered from its own instruction count.
    ///
    /// Six prologue instructions, two per entry, one epilogue -- so this needs no second copy of
    /// the table's length that could disagree with [`Program::build`], which is the point.
    fn allowlisted(&self) -> usize {
        self.len.saturating_sub(7) / 2
    }

    fn try_install(&self, value: u32) -> Result<u32, SeccompError> {
        let prog = libc::sock_fprog {
            len: self.len as libc::c_ushort,
            filter: self.insns.as_ptr() as *mut libc::sock_filter,
        };
        // SAFETY: `prog` points at `self.insns[..self.len]`, which is initialised, and the
        // kernel copies the program before returning. The filter is compiled, not referenced,
        // so the caller's buffer does not need to outlive the call.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_seccomp,
                libc::SECCOMP_SET_MODE_FILTER,
                libc::SECCOMP_FILTER_FLAG_TSYNC,
                &prog as *const libc::sock_fprog,
            )
        };
        if rc != 0 {
            let errno = std::io::Error::last_os_error()
                .raw_os_error()
                .unwrap_or(libc::EINVAL);
            return Err(if errno == libc::EAGAIN {
                SeccompError::ThreadOutOfSync(errno)
            } else {
                SeccompError::Rejected(errno)
            });
        }
        Ok(value)
    }
}

/// A filter that is in force. Proof, and the only thing that can be reported about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Installed {
    /// The action in force. Differs from the requested action only on a downgrade.
    pub action: Action,
    /// The `SECCOMP_RET_*` value actually installed.
    pub value: u32,
    /// Instructions in the compiled program.
    pub instructions: usize,
    /// Allowlisted syscall numbers in the table it was built from.
    pub allowlisted: usize,
    /// Whether `KillProcess` had to fall back to `Kill`.
    ///
    /// Reported rather than absorbed: `Kill` is strictly weaker -- it delivers `SIGKILL`, which
    /// the process cannot catch and which carries no `si_syscall` -- so a downgrade is a real
    /// loss of a property the boot would otherwise be claiming.
    pub downgraded: bool,
}

impl Installed {
    /// Whether the strong action is the one in force.
    pub const fn used_kill_process(&self) -> bool {
        self.value == libc::SECCOMP_RET_KILL_PROCESS
    }

    /// One line for the boot report, naming a downgrade if there was one.
    pub fn report(&self) -> String {
        let base = format!(
            "seccomp: {} instructions, {} syscalls allowed, action=0x{:08x}",
            self.instructions, self.allowlisted, self.value
        );
        if self.downgraded {
            format!(
                "{base} -- DOWNGRADED from SECCOMP_RET_KILL_PROCESS to SECCOMP_RET_KILL; \
                 this kernel predates 4.14"
            )
        } else {
            base
        }
    }
}

/// Read `PR_GET_NO_NEW_PRIVS`.
///
/// Checked rather than tracked in a `static`, because `no_new_privs` is a property of the
/// process that a `fork` inherits and nothing in this crate can unset it -- so a stale
/// `static` would be a lie in exactly the situation where the kernel's answer is interesting.
pub fn new_privs_is_set() -> bool {
    // SAFETY: `PR_GET_NO_NEW_PRIVS` writes one `int` and reads none.
    unsafe { libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) == 1 }
}

/// Set `PR_SET_NO_NEW_PRIVS`.
///
/// Required before an unprivileged `seccomp(SECCOMP_SET_MODE_FILTER)` will install anything,
/// and independently desirable: it is what stops a `setuid` binary from being mapped into the
/// process to gain privilege.
pub fn set_no_new_privs() -> Result<(), i32> {
    // SAFETY: `PR_SET_NO_NEW_PRIVS` takes five `unsigned long`s and reads none of them.
    if unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EINVAL))
    }
}

/// Number of seccomp filters currently attached to this process.
///
/// Zero is the honest precondition for the gate: the census and the final filter each run in
/// a fresh subprocess, because a filter cannot be removed. A non-zero count in a parent test
/// process means the boot sequence ran here, which no test should do.
pub fn filters_installed() -> i32 {
    // SAFETY: `prctl` returns the count as its return value; `-1` means the call failed,
    // which is distinguishable from a real count of 0.
    unsafe { libc::prctl(libc::PR_GET_SECCOMP, 0, 0, 0, 0) as i32 }
}

const BPF_LD_W_ABS: u16 = (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16;
const BPF_JMP_JEQ_K: u16 = (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16;
const BPF_JMP_JSET_K: u16 = (libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K) as u16;
const BPF_RET_K: u16 = (libc::BPF_RET | libc::BPF_K) as u16;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_filter_fits_the_instruction_budget() {
        let program = Program::build_default(Action::KillProcess).expect("filter builds");
        assert!(program.len() < MAX_INSTRUCTIONS);
        assert_eq!(
            program.len(),
            6 + 2 * table::ALLOWLIST.len() + 1,
            "prologue(6) + 2 per entry + epilogue(1)"
        );
    }

    #[test]
    fn the_program_is_bounded_rather_than_truncating() {
        // A table too large for the buffer must be an error, not a program that silently
        // omits the tail of the allowlist.
        let too_many: Vec<Allowed> = (0..(MAX_INSTRUCTIONS + 8))
            .map(|i| Allowed {
                nr: 1000 + i as i64,
                name: "synthetic",
                why: "budget test",
            })
            .collect();
        assert_eq!(
            Program::build(Action::Kill, &too_many).err(),
            Some(SeccompError::TooManyInstructions)
        );
    }

    #[test]
    fn prologue_rejects_a_foreign_architecture() {
        let program = Program::build_default(Action::Kill).unwrap();
        // instruction 0 loads offset 4 (arch), 1 compares to AUDIT_ARCH_X86_64, 2 returns kill.
        assert_eq!(program.insns[0].k, 4);
        assert_eq!(program.insns[1].k, crate::AUDIT_ARCH_X86_64);
        assert_eq!(program.insns[1].jt, 1, "a matching arch must skip the kill");
        assert_eq!(program.insns[2].k, libc::SECCOMP_RET_KILL);
        assert_eq!(
            program.insns[2].k,
            Action::Kill.seccomp_value(),
            "a foreign arch is killed even in trap mode"
        );
    }

    #[test]
    fn prologue_rejects_the_x32_abi() {
        let program = Program::build_default(Action::Trap).unwrap();
        // instruction 3 loads offset 0 (nr), 4 tests bit 30, 5 kills.
        assert_eq!(program.insns[3].k, 0);
        assert_eq!(program.insns[4].k, 0x4000_0000);
        assert_eq!(
            program.insns[5].k,
            libc::SECCOMP_RET_KILL,
            "the x32 marker bit must be fatal in every mode, or a 32-bit number sneaks past"
        );
    }

    #[test]
    fn every_table_entry_becomes_a_conditional_allow() {
        let program = Program::build_default(Action::Trap).unwrap();
        // Walk the chain and recover the numbers it permits, independently of the table.
        let mut permitted = Vec::new();
        let mut i = 6usize;
        while i + 1 < program.len() {
            assert_eq!(program.insns[i].code, BPF_JMP_JEQ_K);
            assert_eq!(program.insns[i + 1].code, BPF_RET_K);
            assert_eq!(
                program.insns[i + 1].k,
                libc::SECCOMP_RET_ALLOW,
                "an allowlisted syscall must be ALLOWed, never sent to the deny action"
            );
            permitted.push(program.insns[i].k as i64);
            i += 2;
        }
        assert_eq!(program.insns[i].code, BPF_RET_K, "epilogue");
        assert_eq!(
            program.insns[i].k,
            libc::SECCOMP_RET_TRAP,
            "the epilogue is the deny arm, and must return the configured action"
        );
        assert_eq!(permuted(&permitted), table::numbers());
    }

    fn permuted(v: &[i64]) -> Vec<i64> {
        let mut v = v.to_vec();
        v.sort_unstable();
        v.dedup();
        v
    }

    #[test]
    fn install_refuses_without_no_new_privs() {
        // In a `cargo test` process this is false unless another test set it, and no unit test
        // in this crate does. If it ever is true the assertion is vacuous, so say so instead of
        // passing quietly.
        if new_privs_is_set() {
            eprintln!("PR_SET_NO_NEW_PRIVS is already set in this test process");
            return;
        }
        let program = Program::build_default(Action::KillProcess).unwrap();
        assert_eq!(
            program.install(),
            Err(SeccompError::NoNewPrivsRequired),
            "installing without no_new_privs must be refused here, not silently allowed"
        );
    }

    #[test]
    fn no_test_in_this_crate_has_installed_a_filter() {
        // A filter cannot be removed, so a unit test that installed one would poison every
        // test after it. Asserted rather than assumed.
        assert_eq!(filters_installed(), 0);
    }
}
