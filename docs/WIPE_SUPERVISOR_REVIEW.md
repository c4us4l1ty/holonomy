# Wipe supervisor: inventory and review plan

**Status: engineering inventory and test design, not a complete audit or a production security
claim.** The terminal currently uses a cooperative reaper; the Linux supervisor is a test fixture
and does not run the editor. L-7 remains `spec`. This inventory identifies concrete paths in the
current source and questions a reviewer must close; absence from the table is not evidence of
absence. It neither promises a hard deadline nor expands the threat model.

## Current exposure inventory

| Source / lifetime | Current handling | Gap for an isolated wipe supervisor |
| --- | --- | --- |
| Passphrase input: `holonomy-gui/src/bin/holonomy.rs::read_passphrase_from`, `holonomy-seal/src/passphrase.rs::PassphraseInput` | The raw prompt vector and normalized output use `Zeroizing`; input also passes through a one-byte stack buffer and normalization. | Drop is not a stalled-thread wipe. Passphrase input and dependency scratch precede the live reaper's `run` and are not in its registered set. Review stack, parser, normalization, errors and allocation/reallocation paths. |
| KDF and envelope: `holonomy-seal/src/kdf.rs`, `envelope.rs`, `commit.rs` | Project-owned Argon2 arena and multiple transient keys/plaintexts use zeroizing owners; dependency wipe features are enabled. | Unlock, verification, compaction and crypto can hold stack, registers and dependency temporaries outside a shared arena. These are not proven erased; the KDF arena can be large. |
| Long-lived keys: `holonomy-gui/src/store.rs::Document` holds `MasterKeys` and a `WorldLine` with its `DEMKey`; `holonomy-seal/src/secret.rs` | Key cells are zeroized on drop, with best-effort locking. `Document::arm_reaper` registers the unwrap and DEM key after the UI quiesces. | They are currently ordinary process-private allocations, not supervisor-writable mappings. Locking can fail; arming results are not checked in `wipe_quiescent`. Copies and temporary key material need independent review. |
| Editable text: `holonomy-text/src/residency.rs`, `typing.rs`, `draft.rs` | Resident slabs and the open typing buffer are wiped on drop/in place; held undo/redo bytes are zeroizing and can be registered. | Registrations happen only after the UI cooperates and can fail. Checkpoint, flush, error, undo/redo and replacement paths create or move copies; they are not all proven present in a protected shared inventory. |
| Search and replace: `holonomy-text/src/search.rs` and `typing.rs` | Segment cache and overlap/seam vectors use `Zeroizing`; replacement uses an atomic publication path. | A stalled operation prevents drop; loaded source buffers, replacement chunks, checkpoints and callbacks need lifetime/copy accounting. Search inputs are separate from these vectors. |
| View/layout: `holonomy-eye/src/window.rs::WindowText`, `holonomy-shell/src/lib.rs::Editor` | The current window has an in-place wipe and can be registered after quiescence. | Layout/rendering and temporary clones/strings can create more copies. The registration covers the specific window allocation, not every derived representation. |
| Terminal input, prompt and output: `holonomy-gui/src/bin/holonomy.rs`, `term.rs`, `lib.rs::CellSurface` | The app renders a cell surface to strings and writes to the terminal; `SearchPrompt` contains ordinary `String`s and is cloned for actions. | The input stack buffer/decoder, search needle, replacement, formatted status, render string, retained `shown` state and cell characters are not covered by the current registered wipe set. Text already sent to the terminal and its scrollback is outside the editor process. |
| Styles and links: `holonomy-text/src/formatting.rs::StyleMap` | Style runs and links are carried with the spine and persisted in an authenticated encrypted manifest. | Link strings and other metadata may themselves be sensitive. Their allocations, history copies and serialized temporaries need explicit classification and wipe coverage. |
| Selection and external surfaces: `holonomy-text/src/select.rs`, shell effects, OS/terminal | Selection can copy plaintext; platform clipboard policy is not established by the text copy operation. | Any future clipboard, accessibility, IME, compositor, terminal, swap, dump and IPC copy needs an explicit policy and platform verification. A supervisor cannot retroactively prove a third party erased its copy. |
| Runtime / OS: stacks, registers, allocator, kernel and descendants | Some project-owned buffers are zeroized and some pages are locked or marked nondumpable on a best-effort basis. | Compiler temporaries, allocator remnants, other threads, forked descendants, core dumps, swap and kernel copies are not enumerated or generally wipe-verifiable by the prototype. |

`wipe_quiescent` registers after the UI has stopped touching data, acknowledges quiescence, waits
for the reaper and calls the editor's in-place wipe. This avoids deliberately racing Rust accesses,
but it needs the UI to reach that code and does not prove that all registrations succeeded or that
all copies were covered. Slot lifetime synchronization is not permission to write concurrently with
arbitrary readers. Zeroizing on drop does not help while a thread is stuck before drop.

## Linux prototype lifecycle and failure model

The test uses `clone3(CLONE_PIDFD)` to create a worker and pidfd in one kernel operation. There is
no `pidfd_open` window and no numeric-PID signal fallback: if the kernel or seccomp denies clone3,
startup fails. The fork-like child uses only prebuilt arguments and `fcntl`, `dup2`, `execve` or
`_exit` before exec. This raw post-fork code is test-only and needs independent unsafe-code and
platform review before any reuse. It is **not** a safe production spawn abstraction. In
particular, fork-like creation inherits a copy of the supervisor address space until exec; it must
not be assumed to protect secrets in a real supervisor.

The returned pidfd identifies that exact worker even if an external reaper consumes its status.
Signals and `waitid(P_PIDFD)` use it; `/proc/<pid>/task` is used only as a test observation of a
socket wait, not to authorize a wipe or signal a process. An outside reaper can make confirmation
fail; it must never be interpreted as a successful wipe. Startup is complete only after an exec'd
controlled worker sends readiness. Failed exec/readiness, stop or exit confirmation returns an error;
cleanup attempts pidfd kill/reap and can block indefinitely. Only confirmed group stop **and** exit
permit the shared-arena wipe and verification. A repeated trip or a nonzero canary is an error.
The child is terminated, never resumed. The fixture doesn't cover hostile processes or descendants
that inherit/map the memfd; process exit alone is not proof of a verified wipe.

This closes the prior *numeric identity acquisition* window in the test fixture; it does not solve
complete wipe coverage, real-editor integration, process creation on other operating systems, or
hard-real-time scheduling. Even for Linux, supported kernel versions, seccomp policies, descriptor
inheritance, signals, other child reapers, process namespaces, ptrace and descendant policy need
review. Where creation, locking, stop, termination or verification cannot be confirmed, the future
product must explicitly report **unmet protection**, not a successful emergency wipe. Define what
happens to user data and terminal state on this fail-closed path; an indefinitely blocked cleanup is
not a 50 ms success.

## Work required before integration or an L-7 claim

1. Trace every row to owners, allocation sites, copies, lifetimes, error paths and tests. Include
   open/unlock, ordinary editing, live undo/redo, rendering, search/replace, verify, compaction,
   shutdown and trips while each is in progress. Produce an independently reviewed completeness
   argument, including dependency-internal scratch and inputs/outputs outside the process.
2. Specify a bounded shared-arena and key ownership API: create and lock before secrets enter,
   define capacity/registration exhaustion and page-lock failure, prevent untracked reallocations,
   define cross-process pointer/alias rules and ensure no worker or descendant can access it at
   wipe time. Prove canaries cover every claimed arena, not merely one synthetic mapping.
3. Design supervisor startup, worker identity and containment for each supported OS. Audit the
   unsafe spawn path, descriptor and secret inheritance, crash handling, reapers and unsupported
   kernels. Run controlled stalled-I/O, lock-held, concurrent-thread, startup/exec failure,
   external-reaper, descendant, repeated-trip and supervisor-crash tests against real editor work.
4. Measure trip-to-confirmed-stop-to-wipe-to-verification on reference hardware under load,
   including all claimed arenas and a falsifiable negative control. Include timeout and cleanup
   cases; disclose scheduler and uninterruptible-kernel limits. If the deadline or coverage is
   impossible for the scope, keep L-7 open or explicitly revise the law rather than silently
   weakening it.
5. Separately pursue bounded dense replace-all and total-process-memory measurement, described in
   `docs/BOUNDED_SEARCH_AND_WIPE.md`; neither is established by the wipe prototype.
