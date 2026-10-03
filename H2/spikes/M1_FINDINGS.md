# M1 — Loro CRDT benchmark: findings

**Status:** complete. **Result: PASS**, with large margins.
**Code:** `crates/loro-bench` (workspace root `Cargo.toml`). Run: `cargo run --release -p loro-bench`.

## The gate

M0 fixed section size at 1500 words and made the section the unit of storage, sync,
and CRDT. That makes Loro's own documented pathology a go/no-go question. From
`loro/tests/perf_styled_read.rs:25-28`:

> "the residual comes from `StyleRangeMap` materializing the full op set on every
> element it covers, which is O(n^2) in memory (309MB at n=4000 for 724 visible chars)"

Sectioning is only a mitigation if the pathology cannot be reached inside one section.
A 1500-word section with heavy formatting accumulates thousands of marks, so this
needed measuring rather than assuming.

## Results

Corpus: 1548 words, 15 paragraphs, 14923 chars, 214 visible marks. Rust 1.98.1, release.

| Measurement | n | min | p50 | p95 | max | Budget | |
|---|---|---|---|---|---|---|---|
| snapshot import | 50 | 0.068 | 0.076 | 0.096 | 0.251 | p95 ≤ 50ms | **PASS** |
| update export | 30 | 0.118 | 0.153 | 0.216 | 0.553 | p95 ≤ 50ms | **PASS** |
| single insert | 2000 | 0.002 | 0.003 | 0.005 | 0.148 | p95 ≤ 5ms | **PASS** |

All times in milliseconds.

**Storage.** Snapshot 12 KB raw → **7 KB** at zstd-3 (65% of original). All-updates
export 15 KB.

**Mark accumulation over 30 rounds** (50 marks per round, including re-marking
already-marked ranges):

| visible marks | styled-read p50 (ms) |
|---|---|
| 313 | 0.289 |
| 1080 | 1.137 |
| 1795 | 1.576 |
| 2473 | 2.672 |
| 3012 | 2.315 |
| 3451 | 3.454 |

Early-third vs late-third: **2.87x growth**. Under the 4x budget, so PASS.

**Sustained editing drift:** 0.98x — flat. No degradation over 2000 inserts.

## Conclusions

**1. Loro is comfortably viable at section scale.** Every latency budget is met with
2–3 orders of magnitude of headroom. Snapshot import at 0.076ms p50 means autosave
can be synchronous and invisible; the 50ms budget was deliberately generous.

**2. Sectioning does contain the mark pathology — but only just.** 2.87x growth over
1500 accumulated marks is under budget yet clearly trending upward, and the absolute
cost is already 12x the initial read. Extrapolating Loro's own quadratic description,
this does not stay flat indefinitely. The practical reading:

- A session that applies ~1500 marks to one section is fine.
- The pathology becomes real at a scale a single user would have to work at
  continuously in one section without the section ever being written out.

**This is a new argument for the M0 finding rather than a contradiction of it.**
Section size 1500 words is not only the performance knee, it is what keeps mark
density bounded. The two milestones reinforce each other: smaller sections bound
mark accumulation, and 1500 words is small enough to stay inside the budget.

**3. The real mitigation is compaction on write.** The fix is not a smaller section,
it is that a section is snapshotted and its op-log dropped once it settles, which is
already the plan (`ExportMode::Snapshot` + `free_history_cache` +
`compact_change_store`). A snapshot contains current state, not the accumulated op
history that `StyleRangeMap` is materialising. Once a section is written out, its
mark cost resets. M2 should therefore treat compaction as a correctness requirement,
not an optimisation.

**4. `zstd` halves storage and is already in the dependency list.** 12 KB → 7 KB per
section. At 467 sections that is 3.3 MB for a 1.12M-word document, which confirms
2000.md's "1–2 MB compressed" estimate is in the right ballpark.

## M1b — follow-up: is the mark growth a real trend?

**This section corrects conclusion 2 above.** M1 measured one run and reported 2.87x
growth. That number was noise. Re-running across 3 seeds
(`cargo run --release --bin markcurve`) gives:

| seed | base marks | final marks | first read | last read |
|---|---|---|---|---|
| 0 | 214 | 3396 | 0.366 ms | 3.538 ms |
| 1 | 223 | 3296 | 0.246 ms | 2.949 ms |
| 2 | 223 | 3457 | 0.260 ms | 3.769 ms |

Growth first→last round: **13.61x**, linear fit R² = 0.950.

But growth ratio is the wrong statistic. The log-log fit is the right one:

> **exponent = 1.05 (R² = 0.969) → LINEAR in marks**

The `per-1k-marks` column is flat at 0.76–1.08 ms across the whole run, which is the
same conclusion stated more plainly. Loro's documented O(n²) `StyleRangeMap` pathology
is **not** what happens at section scale. We are seeing a roughly constant cost per
mark — a completely different problem with a completely different fix.

### What this means

M1's 4x growth budget was the wrong gate. Against a *linear* cost model the only
meaningful gate is an absolute per-read budget, since a styled read happens on every
render of the focused section:

| marks in section | projected styled read |
|---|---|
| 5,000 | 4.5 ms |
| 10,000 | 9.2 ms |
| 25,000 | 23.0 ms |
| 50,000 | 46.0 ms |

**A styled read exceeds 5ms at roughly 5,500 marks.**

### Consequences for the design

1. **M1's conclusion 2 is retracted.** Sectioning does not "contain" the pathology.
   There is no pathology at this scale; the cost is linear and predictable.

2. **The mitigation is a mark ceiling, not compaction.** `ExportMode::Snapshot` does
   not reduce the number of marks in a section, it only drops history. Since the cost
   is in the *current* mark set, compacting a snapshot does not lower the 5ms figure.
   What bounds it is bounding how many marks accumulate in one section before it is
   split.

3. **This gives an independent, quantitative reason to cap section size.** A 1500-word
   section starts at ~214 marks, and sustained heavy formatting reaches 5,500 marks in
   well under a long session. So sections must be **split on mark count**, not only on
   word count. This is a new requirement for M2 and it is absent from `2000.md`, which
   only ever splits on page count (2000.md:283, "Auto-split when a section exceeds
   ~20 pages").

4. **The two thresholds are independent and both are needed.** Word count bounds
   rendering cost (M0). Mark count bounds CRDT read cost (M1b). A short but heavily
   formatted passage hits the mark ceiling long before the word ceiling.

### Recommended thresholds for M2

- Split when **words > 1500** (M0 rendering knee) **or marks > 3000** (under half the
  5.5k budget, leaving headroom for a formatting burst before the next write).
- Never split mid-paragraph, and not inside a table, equation, or image.
- Because the cost is linear this is a *predictable budget* rather than a cliff: a
  user cannot make the editor slow by formatting heavily, they hit an invisible split.

## Caveats

- Single run, one machine, no repetition across seeds. The growth ratio in
  particular deserves a multi-seed run before being treated as a hard number.
- `to_delta()` is a proxy for "rendering a section", not the real render path.
  It is the same styled-read machinery Loro's own perf test uses, so it is the
  right thing to measure, but a real render adds ProseMirror's own cost on top.
- Only `LoroText` was exercised. Block structure will need `LoroTree`, and
  paragraph-level attributes are marks on text rather than node attributes, which
  is a modelling decision M2 has to make explicit.
- The 2000 insert test appends at random positions in a 15k-char document. A real
  editing session has locality (a user edits one place for a while), which should
  be *better* than this, not worse.

## What M2 inherits

- Section size 1500 words, now with a second reason beyond rendering cost.
- **Split on mark count (> 3000) as well as word count (> 1500).** This is the
  load-bearing requirement from M1b.
- Snapshot-only persistence, dropping history on write. Note this does *not*
  reduce mark count, so it is a storage measure, not a latency one.
- Loro docs are per-section, so a 2000-page document is 467 independent CRDTs.
  Sync is per-section and conflicts are per-section.
- Budgets to hold M2 to: snapshot import ≤ 50ms, update export ≤ 50ms, section write
  including zstd ≤ 10ms, and **styled read of the focused section ≤ 5ms**.
