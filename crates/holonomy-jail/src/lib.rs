//! Process isolation, seccomp jail, tripwire handler and teardown.
//!
//! # Why every crate in this workspace depends on this one
//!
//! This crate starts empty and every other crate in the workspace depends on it. That
//! is deliberate, and it is the reason the edge exists from the first commit rather than
//! being retrofitted in Phase 7.
//!
//! A seccomp `SECCOMP_RET_KILL_PROCESS` filter is a *closed world*. There is no way to
//! add a syscall to it once the session is running, so every syscall the process will
//! ever issue must already be reachable from code that exists when the filter is
//! installed. Two consequences fall out of making the jail a shared dependency:
//!
//! 1. **One allocation chokepoint.** The global allocator lives here, in the crate that
//!    seals the process. PROJECT.md §1.1 already measured that the PRD's syscall
//!    allowlist is unreachable because the allocator issues `mmap`/`munmap`/`futex`.
//!    Owning the allocator is what makes that allowlist measurable in Phase 7 rather
//!    than aspirational.
//! 2. **Order is visible in the type graph.** `unshare` → `no_new_privs` → `seccomp`
//!    is a boot-ordering constraint, not a convention. Keeping the sealing primitives in
//!    one crate makes it obvious that anything past `seccomp` must not allocate.
//!
//! The crate is intentionally empty in Phase 0. Filling it early would put unmeasured
//! code in front of the sandbox, which is the ordering error the whole plan is written
//! to avoid.
//!
//! Lands in Phase 7. See PROJECT.md §5 Phase 7 and §2.6.

/// Phase marker. Phase 0 gate only requires that this crate compiles and links.
pub const PHASE_0_PLACEHOLDER: () = ();
