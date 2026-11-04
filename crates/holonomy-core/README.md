# holonomy-core

Container geometry, the 65,536-byte root capsule, the 22-bit sector permutation, duress
bookkeeping, statistical gates and the memory budget.

Pure: no I/O, no OS calls, no cryptographic primitives. Every claim is arithmetic or a state-machine
invariant, and every one of them is a test. The audited identities are `const` assertions, so a
future edit that unbalances the capsule **fails to compile** rather than failing at runtime on a
user's machine.

The full argument for each design decision lives in [`docs/PRD-v5.1.0.md`](../../docs/PRD-v5.1.0.md)
and [`docs/AUDIT-v5.1.0.md`](../../docs/AUDIT-v5.1.0.md); the module docs here carry the short
version, including what was deleted and why.
