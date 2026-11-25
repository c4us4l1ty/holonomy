# HOLONOMY v6 — GATES

> Machine-generated contract. Source of truth: `LAWS` in
> `crates/holonomy-ledger/src/laws.rs`. Regenerate: `cargo run -p xtask -- gates`.
> Verify: `cargo run -p xtask -- gates --check`. Do not edit by hand.

| ID | Law | Bound | Enforcing block | Status |
| -- | --- | ----- | --------------- | ------ |
| L-1 | Lifetime scale | ≥ 30,000,000 words, ≥ 10,000 pages, 60 years of daily growth | Block 4 — The Spine | gated |
| L-2 | Eyes-only residency | resident plaintext bounded by the viewport neighborhood; the same bound at 1 page and at 30M words; zero allocations on the keystroke path | Block 4 — The Spine | gated |
| L-3 | Cold open | ≤ 10 s cold open of a maximum-size document on reference hardware | Block 6 — First Light | spec |
| L-4 | Echo | keystroke-to-paint ≤ 8 ms p99 at any document offset and any document size | Block 6 — First Light | spec |
| L-5 | Idle is idle | zero repaint and zero text-path timers while the view is unchanged | Block 6 — First Light | spec |
| L-6 | Atomic world-line | power loss at any instant reopens at the last committed witness; zero silent corruption | Block 3 — The Witness | spec |
| L-7 | Verified wipe | all resident plaintext wiped and canary-verified ≤ 50 ms from tripwire on reference hardware | Block 9 — The Walls | spec |
| L-8 | One geometry | for every shaped glyph: hit_test(paint_point(glyph)) == glyph, at every zoom and DPI | Block 5 — The Eye | gated |
| L-9 | Passphrase portability | a copied .wavefunction opens on a fresh machine with a passphrase alone; no account, token, or device required | Block 6 — First Light | spec |
| L-10 | Tail-only sync | backing up after a save transfers O(appended bytes), never the whole file | Block 3 — The Witness | spec |

## Constitutive budgets (VISION.md §4)

> Machine-generated from `BUDGETS` in `crates/holonomy-ledger/src/laws.rs`.
> "Measured by" names what CI runs today; "still owed" is what must be true
> before the law may be called `gated`.

| Budget | Law | Measured by | Still owed |
| ------ | --- | ----------- | ---------- |
| Document size: ≥ 30,000,000 words, ≥ 10,000 pages, 60-year growth | L-1 | `holonomy-text` tests::open_memory and tests::keystroke_latency (30 M-word spine), `holonomy-eye` viewport_latency (30 M-word shaped layout), and `holonomy-gui` persist::{sixty_year_21900_witness_scrub_and_reverify, cold_open_30m_word_document_stays_under_residency_budget} | nothing in software: 30 M-word scale and 60-year (21,900 witness) growth, reopen, scrub, and re-verify are all asserted in CI |
| Open: cold open of a maximum-size document in seconds, not minutes | L-3 | `holonomy-text` tests::open_memory prints HOLONOMY_OPEN_MEMORY per size, and `holonomy-gui` persist::cold_open_30m_word_document_stays_under_residency_budget times a real sealed 30 M-word reopen + first frame (HOLONOMY_COLD_OPEN_30M <= 10 s) | reference-hardware calibration on target consumer hardware (Block 10) |
| Resident plaintext: bounded by viewport neighborhood, independent of length | L-2 | `holonomy-text` tests::open_memory asserts the ResidentSet bound at two sizes; tests::keystroke_allocations counts zero allocations on the keystroke path; `holonomy-gui` persist::cold_open_30m_word_document_stays_under_residency_budget asserts <= 2 MiB resident at 30 M words | nothing for the plaintext bound itself — this is the one row whose measured half is the whole claim, and the index is separate (D-36) |
| Character echo: under one display refresh, at any offset, at any size | L-4 | `holonomy-text` tests::keystroke_latency interleaves offset 0 and N-1 and prints HOLONOMY_KEYSTROKE_LATENCY p50/p99 | the p99 on reference hardware and the paint half — keystroke-to-*paint*, not keystroke-to-edit, which needs Block 6's renderer |
| Idle work: zero repaint, zero background scanning when nothing changes | L-5 | `holonomy-gui` bin tests::an_idle_window_writes_nothing and `holonomy-gui` canvas::tests::desktop_app_idle_draw_performs_zero_repaints (AUDIT D-68) | the same guarantee inside Block 6's GPUI GPU paint path (unbuilt under --locked); tested on both CellSurface and PixelSurface (D-68/D-69) |
| Save: touches appended segments only; atomic; survives power loss | L-6 | L-6 CI job: `scripts/qualify-ext4-enospc.sh` drives a real ext4 image out of space and checks the refusal is clean | a controlled power cut at arbitrary instants, controller-cache evaluation and a supported-device matrix — Block 10, and the job name says so |
| Wipe: everything readable, wiped in milliseconds and *proven* gone | L-7 | `holonomy-text` tests::wipe_gate fires the reaper across a 4 MiB ResidentSet, live capability keys and 32 zeroize canaries and prints HOLONOMY_WIPE_GATE; the shipped window keeps `ReaperOutcome` and reports a confirmed wipe (counts, exit 0) versus an unconfirmed one (reason, exit 1) — AUDIT D-89 | OS suspend / USB-yank hooks wired into the shipped window and reference-hardware calibration — Block 9's OS shell and Block 10 |
| Portability: one passphrase, one file, any machine, no account | L-9 | `scripts/qualify-l9-portability.sh` (CI job `portability`) copies a document and imports it in a second process under `env -i` with every `HOME`/`XDG_*`/`TMPDIR` pointed at an empty directory that must stay empty (`HOLONOMY_PORTABILITY L-9=partial`), plus cross-OS matrix compilation and tests on Linux, macOS, and Windows | a copied file opened on a second physical machine in the Block 10 release drill |
| Tail-only sync: backing up after a save transfers O(appended bytes) | L-10 | L-10 CI job: the ignored rsync harness measures one named client over TCP and prints HOLONOMY_RSYNC_MEASUREMENT | every other client and backend — Block 10's matrix — or an explicit narrowing of the law (ROADMAP Block 3) |

