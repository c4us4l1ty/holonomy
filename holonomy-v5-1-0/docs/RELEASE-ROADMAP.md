# Release roadmap

This is a dependency-oriented assessment of work remaining after the prototype, not a
schedule or a security certification. The PRD and audit describe design goals, not
proof that those goals have been met. The current demos are not for sensitive or
irreplaceable data.

## 1. Make the storage lifecycle recoverable

- Design and test a crash-consistent creation, replacement and pad-ratchet protocol,
  with explicit outcomes at every failure point. A replacement can succeed before a
  subsequent sync or ratchet fails; reopening alone is not a repair protocol. A listing
  verb now reports staged `.holonomy-save-*` leftovers without decrypting them, but
  automated recovery and reconciliation are still missing.
- Establish durable, rollback-aware pad-use accounting across containers, copies,
  machines and concurrent writers, with identity rules that remain correct across file
  replacement and aliases. Existing digest, lock and file-identity checks only detect
  some changes; they do not close the race or coordinate pad use. Do not count cursor
  slots as fresh pads.
- Provide an operator recovery workflow, including non-destructive inspection, clear
  diagnosis, safe choices, and tests for crashes, torn writes, failed syncs, device
  removal and backups. The current demo's read-only inspection is only a preliminary
  tool for checking an authenticated candidate; it cannot complete recovery. The CLI
  `verify` command authenticates one candidate without printing document bytes, but does
  not inspect its application format or pad history. `remnants` reports staged
  leftovers and `recover` finds the cursor an image needs without writing. `recover-copy`
  can authenticate a selected staged image and copy it to a new, non-existing path without
  writing either source's contents. The generic `.holonomy-save-*` prefix is not bound to a
  particular container, and authentication covers the format's authenticated structures and
  declared payload, not unused image bytes. It does not repair or replace the container, reconcile
  a partially ratcheted pad, decide which of several copies is newest, or account for the pad it
  spent. Promotion and durable pad reconciliation remain manual and unimplemented.
- Specify and implement safe USB provisioning, root-share storage, device identity and
  destruction behavior. Test real hardware and supported operating-system paths.
- Specify versioned credentials and migrations; legacy passphrase mapping lowercases
  input. Define supported container and Wordcraft payload compatibility and migration.

## 2. Integrate the actual editor

- Connect the native Wordcraft UI to the document session while preventing unintended
  plaintext persistence. Audit and test every open, save-as, export, print, clipboard,
  drag-and-drop, autosave and recovery route; a command filter alone is insufficient.
- Preserve rich document content and formatting through edit, undo, save, reopen and
  version upgrades. Provide accessible, responsive save, close, error and recovery UX.
- Report actual per-save protection, verified pad budget and recovery state. Integrate
  platform capture exclusion, actual memory-locking coverage and watchdog priority,
  with honest unavailable/failure states. The demo now shows the protection layer the
  store attaches to each save — with pad freshness stated as *not established* — plus the
  source-reported ratchet count. It does not yet report a verified pad budget, capture
  exclusion, memory-locking coverage or watchdog priority; the status panel reports
  limitations, not deployed protection.

## 3. Validate supported platforms and operational security

- Complete platform-specific durable file replacement, locking, hardware I/O,
  suspend/disconnect notifications, capture handling and memory protection; document
  platform-specific limitations. Current saves are Unix-only and take only an advisory
  sidecar lock; normalized relative paths share it and writable saves reject existing
  hard links, but writers that ignore the lock and other alias/race cases remain.
- Measure memory, UI responsiveness, watchdog timing and failure behavior under realistic
  workloads and constrained hardware. The PRD's resource figures are not measurements of
  the finished product.
- Reduce and document plaintext exposure, including editor allocations, undo history,
  clipboard, swap, crash dumps and OS copies; test the actual protected regions rather
  than a capability probe.

## 4. Release gates

- End-to-end tests on the exact release commit for create, edit, rich-content save/reopen,
  read-only inspection, recovery, hardware removal, concurrency and fault injection.
- Cross-platform CI and hardware qualification on every supported platform; reproducible
  release builds, signed artifacts, upgrade/downgrade testing and dependency review.
- Independent cryptographic and security review, remediation and a published threat model;
  user documentation for backup, recovery, limitations and incident reporting.

Passing existing CI checks is necessary for changes but does not establish any of these
release gates or make the prototype safe for production use.
