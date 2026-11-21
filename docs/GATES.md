# HOLONOMY v6 — GATES

> Machine-generated contract. Source of truth: `LAWS` in
> `crates/holonomy-ledger/src/laws.rs`. Regenerate: `cargo run -p xtask -- gates`.
> Verify: `cargo run -p xtask -- gates --check`. Do not edit by hand.

| ID | Law | Bound | Enforcing block | Status |
| -- | --- | ----- | --------------- | ------ |
| L-1 | Lifetime scale | ≥ 30,000,000 words, ≥ 10,000 pages, 60 years of daily growth | Block 4 — The Spine | spec |
| L-2 | Eyes-only residency | resident plaintext bounded by the viewport neighborhood; the same bound at 1 page and at 30M words; zero allocations on the keystroke path | Block 4 — The Spine | spec |
| L-3 | Cold open | ≤ 10 s cold open of a maximum-size document on reference hardware | Block 6 — First Light | spec |
| L-4 | Echo | keystroke-to-paint ≤ 8 ms p99 at any document offset and any document size | Block 6 — First Light | spec |
| L-5 | Idle is idle | zero repaint and zero text-path timers while the view is unchanged | Block 6 — First Light | spec |
| L-6 | Atomic world-line | power loss at any instant reopens at the last committed witness; zero silent corruption | Block 3 — The Witness | spec |
| L-7 | Verified wipe | all resident plaintext wiped and canary-verified ≤ 50 ms from tripwire on reference hardware | Block 9 — The Walls | spec |
| L-8 | One geometry | for every shaped glyph: hit_test(paint_point(glyph)) == glyph, at every zoom and DPI | Block 5 — The Eye | spec |
| L-9 | Passphrase portability | a copied .wavefunction opens on a fresh machine with a passphrase alone; no account, token, or device required | Block 6 — First Light | spec |
| L-10 | Tail-only sync | backing up after a save transfers O(appended bytes), never the whole file | Block 3 — The Witness | spec |

## Constitutive budgets (VISION.md §4)

> Machine-generated from `BUDGETS` in `crates/holonomy-ledger/src/laws.rs`.
> "Measured by" names what CI runs today; "still owed" is what must be true
> before the law may be called `gated`.

| Budget | Law | Measured by | Still owed |
| ------ | --- | ----------- | ---------- |
| Document size: ≥ 30,000,000 words, ≥ 10,000 pages, 60-year growth | L-1 | `holonomy-text` tests::open_memory and tests::keystroke_latency both build a 30 M-word / 44,000-leaf spine, so the scale is exercised, not argued | the surviving-life requirement: growth, seal, reopen and re-verify over simulated decades, which is Block 8's history scrub |
| Open: cold open of a maximum-size document in seconds, not minutes | L-3 | `holonomy-text` tests::open_memory prints HOLONOMY_OPEN_MEMORY per size, with the index asserted | an actual clock on a cold open of a sealed 30 M-word file, which needs the real reader and reference hardware |
| Resident plaintext: bounded by viewport neighborhood, independent of length | L-2 | `holonomy-text` tests::open_memory asserts the ResidentSet bound at two sizes; tests::keystroke_allocations counts zero allocations on the keystroke path | nothing for the plaintext bound itself — this is the one row whose measured half is the whole claim, and the index is separate (D-36) |
| Character echo: under one display refresh, at any offset, at any size | L-4 | `holonomy-text` tests::keystroke_latency interleaves offset 0 and N-1 and prints HOLONOMY_KEYSTROKE_LATENCY p50/p99 | the p99 on reference hardware and the paint half — keystroke-to-*paint*, not keystroke-to-edit, which needs Block 6's renderer |
| Idle work: zero repaint, zero background scanning when nothing changes | L-5 | `holonomy-gui` bin tests::an_idle_window_writes_nothing, verified by breaking it (AUDIT D-68) | the same guarantee inside Block 6's GPU paint path; on the character grid it is tested but the law names Block 6 (D-69) |
| Save: touches appended segments only; atomic; survives power loss | L-6 | L-6 CI job: `scripts/qualify-ext4-enospc.sh` drives a real ext4 image out of space and checks the refusal is clean | a controlled power cut at arbitrary instants, controller-cache evaluation and a supported-device matrix — Block 10, and the job name says so |
| Wipe: everything readable, wiped in milliseconds and *proven* gone | L-7 | **nothing yet** | Block 9 entirely: the reaper fire test, the canary ghosts and the counted wipe |
| Portability: one passphrase, one file, any machine, no account | L-9 | **nothing yet** | a copied file opened on a second machine and platform, which needs a release build — Block 10 |
| Tail-only sync: backing up after a save transfers O(appended bytes) | L-10 | L-10 CI job: the ignored rsync harness measures one named client over TCP and prints HOLONOMY_RSYNC_MEASUREMENT | every other client and backend — Block 10's matrix — or an explicit narrowing of the law (ROADMAP Block 3) |

