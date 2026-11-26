# Dense search and emergency-wipe design work

This is an engineering record, not a passed gate. The current terminal uses a cooperative reaper,
so L-7 remains open; Block 7's bounded search and dense replace-all are measured in CI, but a
total-process-memory qualification is still open.

## Dense search and replacement

`visit_streaming` visits matches without retaining their positions. It bounds the search needle
and scans in small byte windows, including for fragmented spines. The terminal uses `find_nth`
to count and select a match without collecting the result set. Source implementations and
visitors may allocate additional memory, so this is not a total-process-memory guarantee.

The collecting API still has a **functional limit**: `find_all` / `find_streaming` reject more
than 16,384 matches rather than attempting an unbounded allocation. Dense callers use
`find_nth`, which counts and selects one match without collecting the result set. That limit is
about the collecting API, not about what the editor can replace.

`Typing::replace_all` now has two paths (OQ-32). Sparse matches reuse one sealed replacement across
spine path copies. Above the collection limit it falls back to `stream_replace_chunks`, which
scans the original in bounded windows, emits valid UTF-8 output chunks, transforms style runs
incrementally and publishes through `WorldLine::StreamingTx` as one undoable transaction. CI
measures 30,013,649 dense matches at 1.83 MiB incremental heap against a 4 MiB bound, and
`persist.rs` verifies dense replacement plus undo across reopen. A sink without streaming support
still fails closed with `MatchLimitExceeded`, and the persisted manifest has a 1,048,576-slab
format limit; neither is removed by the streaming path.

What remains is qualification, not the missing mechanism: total-process RSS/working-set and
allocator peak under arbitrary allocator stress, fault injection on the staged transaction's
write/sync/rollback boundaries, and the same treatment for sparse path-copy replacements. The
incremental-heap number is not a total-memory ceiling.

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
process exit, and only then wipes and verifies that shared mapping. It creates the worker and pidfd
atomically using Linux `clone3(CLONE_PIDFD)`, then signals and waits through the pidfd, including
cleanup. Unsupported or denied clone3 fails without a numeric-PID fallback. Regressions model an
external reaper both immediately after creation and after readiness, and a failed exec. This closes
the earlier pidfd-acquisition window in the test fixture, not the broader production lifecycle
problem. The raw post-fork/exec code needs independent review; the fixture needs clone3, pidfds,
`waitid(P_PIDFD)` and procfs. Platform-specific tests are ignored by default and explicitly run in
the Linux CI measurement. The worker is tested in socket read, lock-holding spin, and multithreaded
cases; an unconfirmed stop refuses to wipe and a surviving canary is an error. The measured
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

See [`WIPE_SUPERVISOR_REVIEW.md`](WIPE_SUPERVISOR_REVIEW.md) for the current source-level
plaintext/key inventory, prototype failure model, and outstanding integration review.
