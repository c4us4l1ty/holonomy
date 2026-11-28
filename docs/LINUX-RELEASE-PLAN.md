# Linux-first production release plan

Status: **planning, not production approval**. The first production target is Linux
x86_64 with the GPUI desktop. Other platforms remain experimental and their existing
builds are not evidence of production support. This plan does not change the laws in
`GATES.md` or claim that any open gate has passed. See `ROADMAP.md`, `AUDIT.md`,
`THREAT-MODEL.md`, and `crates/holonomy-native/README.md` for the governing details.

## 1. Define the support contract

- [x] Select Linux x86_64 / GPUI as the first production target.
- [ ] Select and document exact distribution versions, kernels, filesystems, graphics
  drivers, X11/Wayland sessions and hardware to support, with named owners and a
  maintenance window. Fedora 44/45 Xvfb/software Vulkan tests are candidates and
  existing evidence, **not** a physical-hardware or Wayland support certification.
- [ ] Decide whether a Linux-only release requires a formal amendment to Block 10's
  three-OS gate. Do not relabel that gate as passed without an approved amendment.
- [ ] Define the minimum feature set, backup and restore guidance, accessibility
  requirements, and what the release explicitly does not promise.

## 2. Durability and recovery

- [ ] Choose the GPUI autosave/recovery policy; until implemented and tested, require
  an explicit successful save and warn that unsaved work can be lost.
- [ ] Exercise create, edit, failed save, close refusal, crash and authenticated reopen
  using the **exact release binaries** on every supported configuration. Verify that
  a failed or uncertain save never presents itself as a confirmed commit.
- [ ] Perform controlled power-cut tests at varied commit stages on named devices,
  filesystems and cache configurations; publish the protocol, failures and results.
  Existing process-kill, fault injection and ext4 ENOSPC tests are not substitutes.
- [ ] Test backup, restoration, verification and explicit compaction, including disk
  exhaustion and interrupted operations. Define how users recover after a refusal.

## 3. Security lifecycle

- [ ] Review and inventory plaintext and key lifetimes in the GPUI process, including
  temporary copies, caches, input, compositor/GPU and external clipboard boundaries.
- [ ] Design, implement, independently review and qualify a supervisor and tripwire
  lifecycle for the stated threat model, including blocked/stopped/killed editor cases.
  The supervisor fixture is not integrated and cannot establish complete erasure.
- [ ] Qualify suspend and any promised USB trigger, failure reporting and shutdown.
  Explicitly document what cannot be erased or verified and obtain security sign-off.

## 4. Actual desktop qualification

- [ ] Record L-3/L-4/L-5/L-7 results for the actual GPUI release build on named
  reference consumer hardware, using reproducible workloads and thresholds.
- [ ] Test real displays, mixed monitors, fractional scaling, graphics drivers and
  supported sessions; obtain visual approval including 200% scale.
- [ ] Qualify IME, accessibility, save/close and OS-quit behavior in the support
  matrix. Synthetic Xvfb tests alone cannot establish these properties.

## 5. Finish the agreed feature set

- [ ] Compare the chosen minimum feature set with the GPUI frontend's documented
  gaps: formatting and links, navigation, import/export, history and document zoom.
  Implement and test required features; list the rest as deferred, not silently done.
- [ ] Run usability and accessibility acceptance on the exact release candidate.

## 6. Distribution and operations

- [ ] Build and test the exact production artifacts, including the desktop executable,
  installation, upgrades, removal and clean-machine encrypted round trips.
- [ ] Establish independently verifiable signatures, key custody/rotation, artifact
  provenance and a release approval process. Checksums alone are not signatures.
- [ ] Choose an installer/update mechanism and document supported upgrade, rollback,
  security disclosure and dependency maintenance procedures.
- [ ] Ensure published artifacts and documentation state the same support boundaries.

## 7. Release evidence and decision

- [ ] Collect immutable CI runs, artifact hashes, test protocols, environment details,
  independent review and sign-offs for the **same candidate**. Re-run affected gates
  if the candidate changes. Current README and CI describe existing evidence, not a
  certification of every scenario.
- [ ] Keep L-3 through L-7 and L-10 as `spec` until their actual contracts are met, or
  explicitly approve, document and implement a narrower contract. In particular, one
  rsync measurement does not prove all backup clients transfer only appended bytes.
- [ ] Resolve the Block 10 all-three-OS release requirement before describing a
  Linux-only candidate as satisfying it. Publish known limits and issue a go/no-go
  decision; absence of a failed test is not evidence for an untested claim.
