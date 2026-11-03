//! The alternate signal stack, and why it must exist before the handler that needs it.
//!
//! # The failure this prevents
//!
//! A `SIGSEGV` handler runs on the stack that was interrupted, unless it was registered with
//! `SA_ONSTACK` and an alternate stack has been configured with `sigaltstack(2)`. If the
//! thing that faulted was *the stack itself* -- an underflowing pointer walking below the
//! stack, or a recursion deep enough to run off the end -- then there is nowhere to push the
//! signal frame. The kernel cannot deliver the signal, so it takes the default action, and
//! the process dies with no handler having run.
//!
//! That is the specific outcome worth avoiding, and it is the one that matters here, because
//! a guard-page tripwire exists precisely to catch memory-corruption bugs and corruption
//! bugs are what corrupt stacks. The handler that scrubs the plaintext would be skipped, and
//! the process would exit 139 with the plaintext still in a core-eligible mapping.
//!
//! So the ordering is not a style preference: `sigaltstack` first, then `sigaction(SA_ONSTACK)`.
//! [`AltStack::install`] and [`crate::tripwire::install`] are sequenced by the boot state
//! machine in [`crate::BootStep`] for the same reason.
//!
//! # Where the memory comes from
//!
//! A `SecureBlock`, which means page-locked, `MADV_DONTDUMP`, and -- the load-bearing part --
//! **registered in the tripwire's scrub table**. That is the whole reason the registry lives
//! in this crate rather than in `holonomy-secure`: the signal frame the kernel pushes here
//! contains the faulting address, the register state and whatever pointers were in it, so
//! the stack holding it is exactly the kind of memory that must not survive the crash.
//!
//! The caller supplies the block because this crate cannot construct one: it is a leaf and
//! `holonomy-secure` depends on it, not the reverse. See [`AltStack::install`].
//!
//! # `SS_AUTODISARM`
//!
//! Set deliberately. Without it, a fault *inside* the handler pushes a second frame onto this
//! same stack, and the handler is now re-entered on a stack it has already consumed the top
//! of. With it, the alternate stack is disarmed on entry, so a nested fault falls back to
//! the main stack -- which is where the signal handler's bounded work belongs, and where
//! `SA_NODEFER` plus the reentrancy guard turn it into exit 137 instead of an unreportable
//! double fault.
//!
//! `SS_AUTODISARM` is reported by `sigaltstack` on the way in, and if the kernel rejects it
//! the install fails rather than silently running without it: a 64 KiB stack is 32x
//! `MINSIGSTKSZ`, so the reason for wanting the flag is not stack depth, and losing it
//! silently would be exactly the kind of quiet degradation this project keeps rejecting.

/// Alternate signal stack size: 64 KiB.
///
/// Sixteen times the smallest stack with room for `siginfo_t` plus an x86-64 `ucontext_t`
/// plus the x87/SSE state, which is a little over 3 KiB all in. The margin is not for the
/// kernel's frame -- it is for the report buffer and for the compiler's own spill, which is
/// not something that can be sized in advance.
pub const ALT_STACK_BYTES: usize = 64 * 1024;

/// Why the alternate signal stack could not be configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AltStackError {
    /// `sigaltstack` returned non-zero. Carries `errno`.
    Refused(i32),
    /// The kernel does not support `SS_AUTODISARM`, and running without it is not the same
    /// configuration. See the module comment.
    NoAutodisarm(i32),
    /// Kept for callers that genuinely refuse to displace a registration. [`AltStack::install`]
    /// does not return it: replacement is the correct behaviour, and this variant exists only so
    /// a future strict variant has a name.
    #[allow(dead_code)]
    AlreadyActive,
    /// The region is not large enough to hold a signal frame.
    TooSmall,
}

impl core::fmt::Display for AltStackError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Refused(e) => write!(f, "sigaltstack refused: errno {e}"),
            Self::NoAutodisarm(e) => write!(f, "SS_AUTODISARM unsupported: errno {e}"),
            Self::AlreadyActive => f.write_str("an alternate signal stack is already active"),
            Self::TooSmall => f.write_str("alternate signal stack is below MINSIGSTKSZ"),
        }
    }
}

impl std::error::Error for AltStackError {}

/// A registered alternate signal stack.
///
/// Not `Clone`, and there is no `Drop`: unregistering the stack on the way out would be
/// wrong, because the only reason to be on the way out of this process is that something
/// has already gone wrong, and the handler needs the stack to still be there when it runs.
#[derive(Debug, Clone, Copy)]
pub struct AltStack {
    base: usize,
    len: usize,
    /// Whatever this registration displaced. Kept for the boot report, because a jail running
    /// on a stack it did not choose is worth knowing about.
    replaced: Option<ReplacedStack>,
}

/// An alternate signal stack that was registered before the jail's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplacedStack {
    /// Base of the displaced region.
    pub base: usize,
    /// Its length. This is how the boot report shows the replacement really happened: the
    /// runtime's default is 8 KiB, the jail's is 64 KiB.
    pub len: usize,
}

/// Identity is the region, not the provenance.
///
/// A stack re-read from the kernel has no `replaced`, so `PartialEq` compares only `base` and
/// `len` -- which is what makes [`crate::tripwire::install`] able to check that the stack the
/// handler will run on is the one the caller intended, rather than merely that some stack is
/// registered.
impl PartialEq for AltStack {
    fn eq(&self, other: &Self) -> bool {
        self.base == other.base && self.len == other.len
    }
}

impl Eq for AltStack {}

impl AltStack {
    /// Register `base..base+len` as the alternate signal stack.
    ///
    /// The caller passes the region rather than this crate allocating it, for the dependency
    /// reason in the module comment. The region is expected to be the data area of a
    /// [`SecureBlock`](https://docs.rs/holonomy-secure) that has already been registered with
    /// [`crate::registry`], so that a fault scrubs the frame the kernel just wrote.
    ///
    /// **Replaces** whatever is registered, and records what it displaced.
    ///
    /// Refusing because something is already there would be wrong on this platform, and the
    /// reason is measured rather than assumed: **every thread of a Rust binary already has an
    /// alternate signal stack.** On this host, with the musl target, both the main thread and
    /// a freshly spawned thread report `ss_sp` in the mmap range with `ss_size == 8192` and
    /// `ss_flags == 0` -- an 8 KiB stack installed by the runtime, without `SS_AUTODISARM`, and
    /// outside anything the jail registered.
    ///
    /// So "is one registered" is not a precondition that can hold, and treating its presence as
    /// an error would make the tripwire uninstallable. The kernel permits direct replacement
    /// (measured: `sigaltstack` with a new region returns 0, and a subsequent query reports the
    /// new base, the new size and `ss_flags == SS_AUTODISARM`), and replacement is what a jail
    /// needs anyway -- an 8 KiB runtime stack with no autodisarm is exactly the configuration the
    /// tripwire is designed not to run on.
    ///
    /// Idempotent for an identical region, so a re-exec of the boot sequence is harmless.
    pub fn install(base: usize, len: usize) -> Result<Self, AltStackError> {
        let previous = Self::current();
        if let Some(existing) = previous {
            if existing.base == base && existing.len == len {
                return Ok(existing);
            }
        }
        // `base` must be the start of a mapping of at least `len` bytes that outlives the
        // process; the kernel writes only the signal frame into it.
        let requested = libc::stack_t {
            ss_sp: base as *mut libc::c_void,
            ss_flags: crate::SS_AUTODISARM,
            ss_size: len,
        };
        // `old_ss` is where the kernel reports what it displaced, which is how `replaced` is
        // obtained without a second query.
        let mut active = libc::stack_t {
            ss_sp: std::ptr::null_mut(),
            ss_flags: 0,
            ss_size: 0,
        };
        // SAFETY: `active` is a valid, writable `stack_t`.
        if unsafe { libc::sigaltstack(&requested, &mut active) } != 0 {
            return Err(AltStackError::Refused(
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EINVAL),
            ));
        }
        // `active` is now the *old* stack, so it cannot be used to check the new one: its
        // flags describe what was displaced, which on this platform is `0`. Re-query instead.
        // (The first version of this checked `active.ss_flags` and failed with
        // `NoAutodisarm(0)` on every run, because the runtime's stack has no autodisarm and the
        // kernel helpfully reported exactly that.)
        if !Self::current_has_autodisarm() {
            // Put the previous registration back rather than leaving a half-applied stack, and
            // report rather than continue.
            let restore = libc::stack_t {
                ss_sp: previous.map_or(std::ptr::null_mut(), |old| old.base as *mut libc::c_void),
                ss_flags: previous.map_or(libc::SS_DISABLE, |_| crate::SS_AUTODISARM),
                ss_size: previous.map_or(0, |old| old.len),
            };
            // SAFETY: `restore` is a valid `stack_t` describing either the displaced stack or
            // `SS_DISABLE`.
            unsafe { libc::sigaltstack(&restore, std::ptr::null_mut()) };
            return Err(AltStackError::NoAutodisarm(
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::ENOSYS),
            ));
        }
        Ok(Self {
            base,
            len,
            replaced: previous.map(|old| ReplacedStack {
                base: old.base,
                len: old.len,
            }),
        })
    }

    /// Reconstruct a recorded `AltStack` from its base and length.
    ///
    /// For the tripwire handler, which cannot be handed a reference (its signature is fixed by
    /// `sigaction`) and cannot ask the kernel (autodisarm hides the answer). The values are the
    /// ones [`AltStack::install`] published, so this is the same stack rather than a new claim.
    ///
    /// `pub(crate)` because it bypasses the kernel: a caller supplying numbers from anywhere else
    /// would be asserting rather than observing.
    pub(crate) fn recorded(base: usize, len: usize) -> Self {
        Self {
            base,
            len,
            replaced: None,
        }
    }

    /// What this registration displaced, if anything.
    pub const fn replaced(&self) -> Option<ReplacedStack> {
        self.replaced
    }

    /// The stack the kernel currently has registered, if any.
    pub fn current() -> Option<Self> {
        let mut probe = libc::stack_t {
            ss_sp: std::ptr::null_mut(),
            ss_flags: 0,
            ss_size: 0,
        };
        // SAFETY: `probe` is a valid, writable `stack_t`.
        if unsafe { libc::sigaltstack(std::ptr::null(), &mut probe) } != 0 {
            return None;
        }
        // `SS_DISABLE` in the returned flags means "there is none".
        if probe.ss_flags & libc::SS_DISABLE != 0 || probe.ss_sp.is_null() {
            return None;
        }
        Some(Self {
            base: probe.ss_sp as usize,
            len: probe.ss_size,
            replaced: None,
        })
    }

    /// Whether the registered stack has `SS_AUTODISARM` set.
    ///
    /// Separate from `current()` because the two properties are different: a stack can be
    /// registered without the flag, and that is precisely the runtime's 8 KiB default. A boot
    /// that checked only `current()` would accept it.
    pub fn current_has_autodisarm() -> bool {
        let mut probe = libc::stack_t {
            ss_sp: std::ptr::null_mut(),
            ss_flags: 0,
            ss_size: 0,
        };
        // SAFETY: `probe` is a valid, writable `stack_t`.
        let rc = unsafe { libc::sigaltstack(std::ptr::null(), &mut probe) };
        rc == 0 && probe.ss_flags & crate::SS_AUTODISARM != 0
    }

    /// Unregister the alternate signal stack, restoring `SS_DISABLE`.
    ///
    /// Only for tests that need to observe the pre-existing state, and only safe when the
    /// caller is not about to run a handler. The boot never calls it.
    pub fn disable() -> bool {
        let disabled = libc::stack_t {
            ss_sp: std::ptr::null_mut(),
            ss_flags: libc::SS_DISABLE,
            ss_size: 0,
        };
        // SAFETY: `SS_DISABLE` with a null `ss_sp` is the documented way to unregister.
        unsafe { libc::sigaltstack(&disabled, std::ptr::null_mut()) == 0 }
    }

    /// Base of the registered region.
    pub const fn base(&self) -> usize {
        self.base
    }

    /// Length of the registered region.
    pub const fn len(&self) -> usize {
        self.len
    }

    /// One past the last byte of the registered region.
    ///
    /// The bound the tripwire's final step wipes up to, immediately before `exit_group`. Public
    /// because the tripwire is another module and the alternative is `base() + len()` arithmetic at
    /// the call site -- where the wrapping add is a real thing to get wrong.
    pub const fn top(&self) -> usize {
        self.base.wrapping_add(self.len)
    }

    /// A registered alternate stack is never zero-length.
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Whether *this stack frame* lies inside the region `self` describes.
    ///
    /// Takes `&self` rather than querying the kernel, and that is not a shortcut -- it is forced,
    /// by a measured kernel behaviour:
    ///
    /// **`SS_AUTODISARM` hides the alternate stack from `sigaltstack` inside a handler running on
    /// it.** Measured on this host: with the flag set and a handler executing on the stack,
    /// `sigaltstack(NULL, &old)` returns `0` and fills `old` with `ss_sp = NULL`,
    /// `ss_size = 0`, `ss_flags = SS_DISABLE` -- the kernel's way of saying "this stack is
    /// disabled for the duration of the handler", which is the whole point of autodisarm, since
    /// otherwise a nested signal would reuse a stack that is already consumed.
    ///
    /// So the first version of this method re-queried the kernel and compared against the answer,
    /// and it could only ever return `false` from the one place that mattered. The tripwire calls
    /// it from inside the handler by definition.
    ///
    /// Comparing against the value the jail recorded at install time is exact, needs no syscall,
    /// and is available exactly when it is needed. The recorded base is page-aligned and the
    /// region is `len` bytes, so a stack pointer inside it is unambiguously on the alternate
    /// stack.
    pub fn contains_address(&self, addr: usize) -> bool {
        self.base <= addr && addr < self.base.wrapping_add(self.len)
    }
}
