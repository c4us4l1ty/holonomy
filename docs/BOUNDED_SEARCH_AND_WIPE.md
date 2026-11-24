# Dense search and emergency-wipe design work

This is an engineering plan, not a passed gate. The current terminal uses a cooperative reaper;
L-7 and Block 7's complete find/replace memory gate remain open.

## Dense search and replacement

`visit_streaming` visits matches without retaining their positions. It bounds the search needle
and scans in small byte windows, including for fragmented spines. The terminal uses `find_nth`
to count and select a match without collecting the result set. Source implementations and
visitors may allocate additional memory, so this is not a total-process-memory guarantee.

`find_all` and the current transactional `replace_all` still build a vector of match positions.
They now explicitly reject more than 16,384 matches before modifying the document, rather than
attempting an unbounded allocation. This is a **functional limit**, not a solution for dense
replace-all. The persisted manifest also has a 1,048,576-slab format limit; repeated path-copy
edits can produce one or more slabs per match. Removing only the result-vector limit would not
make arbitrary dense replacement correct or bounded.

A plausible full implementation needs a streaming output transaction, not millions of slab
references: scan the original in bounded chunks, produce replacement text and transformed style
runs incrementally, re-seal the result in bounded segments, and publish all resulting segment
records and the final manifest under one witness. The transaction needs a bounded staging API in
`WorldLine`, including explicit rollback/reopen semantics on every write and sync failure. It must
preserve UTF-8, cross-segment matches, style/link semantics, and single-step undo; bound and check
its manifest and style representation; and wipe transient plaintext on all exits. This trades
extra encrypted writes for memory independent of the number of matches. It needs fault injection,
reopen/undo tests, and dense 30-million-word tests before the restriction can be removed.

Memory qualification must distinguish plaintext residency, Rust heap, total process RSS/working
set, allocator overhead, stacks, mappings and kernel cache. A sampled RSS is only a lower bound on
the instantaneous peak, not proof of a ceiling. Use an isolated measurement process for each
operation, define whether KDF/unlock is in scope, report baseline and operating-system peak or
high-frequency external samples with known blind spots, and separately exercise sparse and dense
matches, highly fragmented spines, long needles, live drafts/undo checkpoints and replacements.

## Wipe when the editor stalls

The current reaper waits for the UI to relinquish access. Wiping a Rust allocation while another
thread may read or mutate it is a data race, even with volatile stores; a mutex or cooperative
acknowledgement cannot provide a 50 ms deadline if its holder is indefinitely blocked. Killing a
process or discarding pages is not proof that all plaintext was zeroed or that canaries verified.

A Linux-only test prototype now lives in `crates/holonomy-seal/tests/supervisor_probe.rs`. It
executes a separate controlled worker with a 4 MiB shared memfd, confirms a group stop and then
process exit, and only then wipes and verifies that shared mapping. After setup, signals and waits
use a Linux pidfd to avoid targeting a recycled numeric PID, with a regression for an externally
reaped child. The prototype requires Linux pidfd and `waitid(P_PIDFD)` support; acquiring the pidfd
is not atomic with process creation, so a production design must still close that setup race. The
platform-specific tests are ignored by default and explicitly run in the Linux CI measurement, so
ordinary tests do not require those kernel facilities. The worker is tested in socket read,
lock-holding spin, and multithreaded cases; an unconfirmed stop
refuses to wipe and a surviving canary is an error. The measured
trip-to-verification time includes a pre-wipe negative-control read. The worker is terminated, not
resumed. The prototype is not wired to the editor, and its known shared mapping is neither all
plaintext nor an audited protected arena. It does not verify private heap, stack, registers, keys,
terminal output or descendants, and the stop/exit and cleanup waits have no hard real-time bound.

A possible platform-specific design is a supervisor isolated from the editor process: keep all
plaintext and keys in explicitly inventoried, locked, shared wipe arenas; prohibit uncontrolled
plaintext copies; and on a trip use an OS-supported mechanism to stop every editor thread, with a
confirmed stop, before the supervisor wipes and verifies those arenas. Define a fail-closed policy
for a failed or late stop, and do not call termination or page reclamation a verified wipe. This
is not a drop-in change: allocator/dependency temporaries, thread stacks and registers, terminal
surfaces, clipboard, IPC, child processes, pointer provenance, lock ownership and crashes must be
accounted for. OS scheduling and uninterruptible kernel waits prevent an unconditional hard
real-time promise on a general-purpose OS.

Before claiming L-7, build and independently review a per-platform prototype; inject a UI blocked
in I/O and in a critical section, concurrent trips, shutdown/EOF and register/unregister races;
measure trip-to-confirmed-stop-to-wipe-to-canary verification on reference hardware under load;
and audit every plaintext path. A timeout must be reported as an unmet security guarantee, not as
success. Until then, the existing cooperative mode is a mitigation, not the required deadline.
