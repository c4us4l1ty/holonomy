//! `SecureBlock`: page-locked, guard-bounded memory that can be proven scrubbed.
//!
//! Every buffer that holds plaintext passes through here. `mmap(MAP_PRIVATE|
//! MAP_ANONYMOUS)`, `mlock`, `madvise(MADV_DONTDUMP|MADV_DONTFORK)`, and a `PROT_NONE`
//! page above and below. `Drop` scrubs behind a compiler fence before `munmap`, so the
//! plaintext is not left in a mapping the allocator may hand to something else.
//!
//! Lands in Phase 1. Gate: a test allocates 4096 bytes and proves both neighbouring
//! pages fault. See PROJECT.md §5 Phase 1 and PRD §8.1 TC-MEM-02.

pub use holonomy_jail::PHASE_0_PLACEHOLDER;
