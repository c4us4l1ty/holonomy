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
