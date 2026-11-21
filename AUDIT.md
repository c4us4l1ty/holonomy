# HOLONOMY v6 — AUDIT (living document)

> This is a living record, not a post-mortem. It follows the v5.1.0 cycle — spec, attack,
> verdict, then (and only then) build — and it carries the wall of killed claims so dead
> ideas cannot be resurrected by enthusiasm alone.

## Protocol

1. Every block starts from an RFC in `docs/rfc/`. An RFC is a *target for attack*, not a
   celebration of intent: it must name its adversary cases (from `docs/THREAT-MODEL.md`) and
   the test or gate that would falsify it.
2. Anyone may file an attack case in this file against an open RFC. A case names: the claim
   attacked, the mechanism of failure, and the smallest demonstration.
3. Verdicts: **killed** (claim removed from the design), **amended** (claim survives in
   weakened, explicitly-stated form), **survived** (attack answered by construction or gate).
4. Implementation audits (Block 9) re-attack *code*, not just specs.
5. Nothing lands in `GATES.md` status `gated` without its attack cycle closed.

## The wall of killed claims

Claims that were proposed somewhere in Holonomy's history and destroyed by their own audit.
Listed so they stay dead.

| # | Claim | Killed by | Generation |
| - | ----- | --------- | ---------- |
| K-1 | One-time-pad reuse across saves is acceptable | counter-mode nonce reuse = worn-twice pad; found in H1's own spec during implementation | H1 |
| K-2 | 384 MiB × t=16 Argon2id at "180 ms" | measured 10,180 ms — 57× off; a DoS on the user's own machine | H1 |
| K-3 | π(0) KEM self-encapsulation unlocks the capsule that holds it | circular by construction — deadlock at slot `π(0)` | v4 |
| K-4 | "CryptScanline" — zero-plaintext rendering as v5.0 specified it | plaintext exists at paint or it is not on screen; claim impossible as written | v5.0-OMEGA |
| K-5 | SPOC ±42 visual flicker for covert signaling | cross-correlationable; an epilepsy hazard; decorative | v5.0-OMEGA |
| K-6 | PEED self-modifying anti-debug keystream | breaks its own execution invariants | v5.0-OMEGA |
| K-7 | 16.60 MiB RSS budget for a full editor | impossible arithmetic, claimed anyway | v5.0-OMEGA |
| K-8 | Monospace-cell caret geometry under proportional paint | 41-bug audit; caret/selection/click point at different bytes — the two-geometry disease | H1 UI |
| K-9 | Page breaks by newline padding | corrupts document semantics to please layout | H1 UI |
| K-10 | Fixed-size 134 MiB payload as a lifetime document format | L-1 (60 years of growth) kills it; v6 container is append-only | v5.1 format choice |

## Open attack ledger (seeded)

| Case | Target | Status |
| ---- | ------ | ------ |
| OQ-1 | Toolchain: root `rust-toolchain.toml` used floating `stable`. Pinned to Rust 1.100.0 after observing the CI compiler; no local sandbox toolchain was available for an independent pin check. | **closed 2026-11-16** |
| OQ-2 | Undo exposure: world-line keeps all drafts (Vision III) vs. an adversary who wants the right to truly delete a draft. Compaction is explicit, file-scoped, and passphrase-confirmed; RFC-002 states that cloud mirrors may retain pre-compaction files and their history, so users must also prune external backups. | **closed — residual disclosed** |
| OQ-3 | Clipboard egress on each OS: flag-confidential + clear-on-blur must be verified per platform (Block 9) or labeled unverified in the Security panel | open |
| OQ-4 | Scroll-timing side channel: slab fault-in pattern leaks *where* you read, to whoever sees slab access timing. | **closed 2026-11-17 — accept and disclose.** The pattern is already visible at the I/O layer: fetching an old page reads its ciphertext at a known file offset, so any observer who can time slab faults can observe the reads themselves. Smoothing would buy nothing and would spend L-5 (idle is idle) on traversal cost the architecture exists to delete. Residual recorded in `docs/THREAT-MODEL.md` §5. |
| OQ-5 | Argon2's internal working arena: `MasterKeys::derive`, `kdf::derive_root_secret*`, and the calibration probe all allocated the m-cost arena inside `hash_password_into` and freed it unwiped (v5's H1 hit exactly this and switched to caller-owned memory + a scrubbed block). Fixed by making the arena caller-owned everywhere: `kdf::Arena(Vec<argon2::Block>)` zeroizes the slab on Drop via argon2's `zeroize` feature, and the single `run_argon2` runner serves creation, replay, *and* the calibration probe, with a test pinning byte-equality against argon2's own allocator | **closed** 2026-11-16 |
| OQ-6 | `ParamsClass::Test` misuse guard: a release build could derive a user container at the weakened schedule, and replayed records had no fence. Closed at both boundaries: `derive_root_secret` refuses `Test`; `derive_root_secret_with` refuses below-floor schedules and accepts only records emitted by the calibrated production selector; Block 3's header rejects weak memory, out-of-range memory, non-step-aligned memory, or unbounded time/lanes **before Argon2 runs**. Test/debug profiles admit only the exact test tuple. | **closed** 2026-11-16 |
| OQ-8 | Undo depth as memory: `History` keeps one `Spine` root per state. Roots share subtrees, so the cost per edit is O(log N) nodes, but an unbounded history over years of typing still grows with the **number of edits**, not with the document — and the world-line on disk already holds that history. **Answered in the session by D-14 and D-15:** a length is a `usize` and a root is a pointer, and both are rings — 1024 lengths, 64 roots — so history memory is `1024 words + 64 × O(log leaves)` and does not grow with the number of edits. What that buys is stated exactly: undo depth is 64 destructive actions back, and a seal ends the epoch. Deeper history is the world-line's job (Block 8: reconstruct a root from the log), which is named and not built. | **closed 2026-11-17 for the session — bounded rings; deeper undo is Block 8** |
| OQ-7 | Valid-prefix rollback: a byte-perfect earlier witness prefix is indistinguishable from a file that legitimately stopped there without trusted state outside the file. RFC-002 amended to claim integrity of present bytes and crash consistency, not freshness against full-prefix rollback. An external transparency/monotonic anchor would resolve this but cannot be mandatory under passphrase portability (L-9). | **open — accepted limitation, not locally solvable** |
| OQ-10 | A full resident slab has **no continuation policy** — solved by D-7 and built as D-8/D-9: a leaf names a *source*, and a source is **open** (bytes in a resident buffer) or **sealed** (bytes in the world-line), at the same address. Text being typed is a leaf from its first byte, so `byte_window` and the height index see it; a seal appends the bytes once, renames the source, and corrects one leaf's length in the current root — **no history entry**, so undo never steps through a seal; each byte is written once. `Typing::insert_at` handles mid-document typing by cutting the leaf the caret lands in (pure geometry — the halves name adjacent ranges of one segment, so nothing moves). Deletion is D-11 (a pointer operation; the bytes stay in the log — OQ-14). Not yet: selection. | **closed 2026-11-17 — `typing.rs`** |
| OQ-9 | Leaf fragmentation: a split that lands inside a leaf leaves the segment spread over two leaves, and `join` never coalesces adjacent fragments of the same segment. Content stays correct and the fragments share one segment's storage, but churn inflates the leaf count and with it the height and every slab-count-derived budget. **Narrowed 2026-11-17 by D-3:** the commit boundary does not create fragments — it replaces exactly the extent one leaf covers, and the commit tests assert the leaf count is unchanged — so the remaining exposure is structural inserts (`insert_slab` cutting a leaf) that are never rejoined, plus a document loaded from a log whose geometry is already fragmented. Coalescing at commit, or a pass in `from_slabs`, is still unimplemented; the cheaper of the two is coalescing same-segment neighbours when a document is built from the log. **Narrowed again 2026-11-19 by D-29:** the load-time half is built. `Spine::from_slabs` merges neighbours that name contiguous bytes of one segment, so a document rebuilt from a log carries the document's shape rather than its edit history; merging cannot overflow, because the second slab's range bounds the merged one. What is left is the live session: a structural insert cuts a leaf and nothing rejoins it until the document is written and read back. The seam-healing variant is not just unbuilt, it is the wrong half to build first: healing inside `join` renumbers leaves under `holonomy-shell`'s cached height model, which is a two-crate change and not this row's. | **open — narrowed again 2026-11-19: load-time coalescing done (D-29); a live session still fragments** |
| OQ-11 | **Cluster segmentation: the rules are UAX #29's; the properties are the database's.** Segmentation classifies each code point into a property (CR, LF, Control, Extend, ZWJ, Regional_Indicator, Prepend, SpacingMark, Hangul L/V/T/LV/LVT, Extended_Pictographic, Other) and applies **GB3–GB13 in their numbered order**, carrying the cluster's own state (has it seen Extended_Pictographic, how many regional indicators) so GB11 and GB12/13 decide about the cluster and not the pair (D-16). The old code's real error is gone: it joined anything after a ZWJ — GB9 read as GB11 — so `a`+ZWJ+`b` was one cluster instead of two, an unsafe answer rather than a conservative one. The **property data is no longer typed by hand**: `cluster.rs`'s tables are generated from UCD 18.0.0 by `scripts/gen-cluster-tables.py` (D-27), 750 range triples behind one binary search, so a mark from *any* script joins its base — the coverage gap this row used to carry, where an unlisted mark split from its base and the seam became a caret position no renderer has, is closed by construction. GB9c is built too (D-28): the conjunct run is state carried in the cluster, the two properties it reads are generated, and a Devanagari conjunct (KA VIRAMA SSA) is one cluster. What remains is **one** gap: **BiDi reordering is not implemented at all**, because it is a paint-order question and belongs beside the run tree, not in the module that decides byte boundaries. Block 5's third gate (BiDi and multi-script cluster-correct carets) therefore still **cannot** be claimed: multi-script cluster honesty is now the database's, and direction has no answer. The API surface a fix would touch is still `cluster.rs` alone for GB9c, and the run tree's for BiDi. | **open — narrowed 2026-11-19: the rules run through GB13 (D-16, D-28) over generated data (D-27); BiDi is the one thing left** |
| OQ-12 | **The window is viewport-sized, and now measured on the size the gate names.** The chain — `HeightModel` (O(log n) counted reads), `Spine::slab_span` (one descent), `holonomy_eye::byte_window` (contiguous, holes impossible since D-7) — is swept over a **180 MB / 44,000-leaf** document, every screen. Bytes handed to layout are bounded by one screen plus two leaves, **with the identical bound at 1/1000 the length**; index reads stay ≤ 64 and grow by at most a logarithm. **Narrowed 2026-11-18 by D-18:** the chain now has its missing link — `window_text` reads the window out of the resident set and the pending buffer, so "here is a scroll position" reaches "here is the text to shape" — and it faults the leaves of one screen whether the document has eight leaves or 4,096, and faults nothing on a second look. What is still missing is the timing half: seconds need a real shaper and a font, which is Block 6, and a fake shaper would make the number about the fake. Gate 2 therefore stays unclaimed — its *inputs* are measured end to end, its clock is not. | **Narrowed again by D-19:** the chain now has a *loop* — `holonomy-shell` finds the window, reads it, lays it out and writes the measured heights back, and its frames report the counts (clusters shaped, index reads) on a 4,000-leaf document. The clock is still the missing half and still needs a real shaper (Block 8), so gate 2 stays unclaimed. | **open — chain and loop complete and measured 2026-11-18; timing needs a real shaper** |
| OQ-13 | **How quiet-md enters the tree — decided: as source, not as a dependency.** Cargo ignores a dependency's `[patch.crates-io]`, and quiet-md's app code calls API that only its 43 GPUI patches add, so it is not consumable as a dependency at all; reproducing the patches at our root is the fork-in-the-core-tree Block 1 forbids, and exiling the UI to a second workspace is the other shape it forbids. quiet-md is therefore prior art and an attributed source of MIT code (`THIRD_PARTY.md`), with GPUI pinned upstream and no patch section. The cost is named in `docs/UI-SPEC.md` §8: CJK/bidi line breaking and font fidelity are ours to earn, and quiet-md's patches #4/#23/#30/#31/#32 are the reference for the first of those — the same gap OQ-11 records. | **closed 2026-11-17 — source, not dependency** |
| OQ-14 | **Deletion does not erase.** `Typing::delete` removes a range from the tree, which is a pointer operation: no bytes written, no segment sealed. The deleted text therefore stays in the world-line as ciphertext, decryptable by anyone holding the passphrase. That is inherent to an authenticated append-only log — the log *is* the history, and the document is a view of it — and it is also why the log is trustworthy. A policy is needed (segment-by-segment re-encryption in a compaction pass, or a per-document epoch the user is expected to rotate), and until one exists "deleted" must not be read as "erased" in any user-facing claim. Found 2026-11-17 while implementing deletion. | **open — found 2026-11-17, no policy yet** |
| OQ-15 | **Character boundaries are a precondition the storage layer cannot check.** `Typing::insert_at` and `Typing::delete` take *document* offsets, and a cut or a split that lands inside a multi-byte character leaves a leaf whose bytes are not valid UTF-8. The asymmetry was the finding: offsets inside the open buffer are checked (`SlabText` returns `NotACharBoundary`), and offsets in sealed text are not, because the session holds a `SegmentSink`, which appends and never reads. **Narrowed by D-18:** the reader refuses a window whose edge falls inside a character, naming the offset. **Closed for the session by D-19:** the check OQ-15 asked for exists — `holonomy-shell` holds the reader and the edit in one place, and its test asserts over every caret every frame offers that the byte is a character boundary of the frame's text and hit-tests back to itself. What remains is not a missing check but a missing *type*: the offset is still a `u64` a caller can get wrong, and a caller that bypasses the Eye still asks for a mid-character cut — which is now refused at read time instead of silently rendered. Making it impossible would need an offset type the Eye alone can mint, which is a Block 6 API decision, not a hole in this one. | **closed 2026-11-18 — the caret check passes; a bypassing caller is refused at read** |

| OQ-16 | **A leaf's height is measured apart from the line it is in, so the scroll bar counts a line the document does not have.** `reflow` laid each leaf out alone — necessarily, because measuring the continuous document would mean decrypting it — so a paragraph continuing across a boundary broke there in the measurement, and the per-leaf sum exceeded the continuous layout's height by up to one line per crossed boundary. **Closed by D-20**: the line is carried across the boundary as two words (the x the next cluster sits at, and whether the line began earlier), `layout_styled_from` lays a piece out as the continuation of a line, and `reflow`'s total now equals one continuous layout of the same text — proven by a test that lays the whole document out once. The fix also closed a bug that had not been written down: a frame used to begin at a leaf, which re-wrapped the paragraph from a point the document never breaks at. | **closed 2026-11-18 — the partition is exact, and frames begin at lines** |
| OQ-17 | **A window that begins at a leaf decided its first cluster without the text before it.** It made a boundary the document does not have into a caret position, and stopped backspace/delete at the leaf edge. **Closed by D-22:** the UAX #29 state (last property, GB11 run, regional-indicator count) is carried beside the line position — `Clusters::continuing`/`carry`, `Layout::carry`, `layout_styled_continuing` — so a piece that opens inside a cluster segments as the document does, and its first line offers no boundary there. A frame was never affected: it begins at a line, and a line break is between clusters (D-20). The *advance* of a split cluster is OQ-18, measured, not assumed away. | **closed 2026-11-18 — segmentation is a function of the text and the state carried into it** |
| OQ-18 | **A cluster split by a leaf cut pays its advance twice.** The eye shapes one cluster at a time, so a cluster whose bytes fall either side of a leaf cut was advanced on both sides: `e` + U+0301 is one unit wide in the document and two when a 64-unit wrap and a 64-byte leaf put the cut between them, which is one extra line in the scroll bar. **Closed by D-25:** the leaf the cluster starts in places it whole — a tail read of the leaf after the cut, a `skip` in the carry, and the next leaf's geometry beginning after the bytes it did not place — so one cluster costs one advance. The frame path computes the same skips from its own window, so the two measurements agree. | **closed 2026-11-18 — residual: a cluster longer than the leaf after the cut keeps the double advance, stated not hidden** |
| OQ-19 | **A piece laid out without knowing what follows it offers its end as a caret, and shapes its last cluster short.** The eye sees text, not a document: a window's last byte can be inside a cluster, and `Line::boundaries` offers it while `Shaper::advance` was handed only the cluster's first part. **Closed for this app by D-26** (the frame reads the cluster's tail with the window, so the piece it hands the eye ends on a cluster) and by D-23's `legal_caret` for what the frame cannot finish; the advance half was D-25. What remains open in *principle*: another caller of `View::hit_test` on a piece it truncated itself still gets the piece's end. | **closed 2026-11-18 for the only caller there is — residual: a cluster longer than the leaf after the cut, and any external caller that truncates its own piece** |
| OQ-20 | **GB9c was not implemented, so an Indic conjunct was two clusters.** Unicode 15.1's rule — `InCB=Consonant [InCB=Extend InCB=Linker]* InCB=Linker [InCB=Extend InCB=Linker]* × InCB=Consonant` — makes KA + VIRAMA + SSA one cluster in Devanagari and the same shape one in Bengali, Gujarati, Oriya, Telugu, Malayalam and the historic scripts; without it the seam between the consonant and its conjunct was offered as a caret position, in a script `docs/UI-SPEC.md` §8 names. Not a misread rule but an absent one, which is the class of gap no test can fail — hence the row, and hence building it rather than pinning it again. **Closed 2026-11-19 (D-28)**: the two properties are generated (`InCB=Consonant`, 78 ranges; `InCB=Linker`, 23 code points), `InCB=Extend` is derived from the break class by the standard's own formula, and the run is state in `Context`, so it travels across a leaf cut like every other left context. Residual: `unicode-segmentation` suppresses the break before a consonant on having seen *any* linker, where the standard's left context must be rooted in `InCB=Consonant`; this reads the rule, so `a` + VIRAMA + SSA is two clusters here, and a test pins it. | **closed — 2026-11-19, D-28** |
| OQ-21 | **Redo was not always the inverse of the undo it undid.** Found by D-30's runner, 2026-11-19; fixed by D-33 the same day. Unwinding the retained stack and rewinding it reported a document one byte short: seed 2, twenty thousand actions, 409 bytes expected, 408 returned, first difference at byte 260 (the region of `delete(260, 261)` + `insert(260, 1)` with a `mark`/`insert` pair above it, and a root recorded before a cut as the step being restored). Undo and redo were exact for *marks* — a mark is a length, and the bytes stay in the buffer — and wrong only for a **root**: `leave` records the state to come back to as `settled_spine()`, the tree without the open leaf, because a root names written bytes only; the undo is right to drop them (they were typed after the cut) but the redo then re-applied the cut to a document that had already lost them. The fix is D-33: `Step::Root` carries a copy of the bytes that were open, and `restore_root` re-attaches them under a new provisional id, so undo then redo is a no-op. | **resolved 2026-11-19 by D-33** for this reproduction — the walk no longer reports seed 2 at byte 260; unit test `redo_of_a_cut_puts_back_the_bytes_that_were_open`. The same walk then found OQ-22, and the million with undo found OQ-23 |

| OQ-22 | **A redo put the bytes an undo took back in the wrong place.** Found by D-30's walk, 2026-11-19, on the first run that carried D-33. Seed 0, redo five: 276 bytes where 275 were expected, first difference at byte 113, `want="long enough 漢ke.ehe "` against `got="long enough e漢ke.ehe"`, `pending=1 open=1 spine=276 slabs=75`, with the ring naming `delete(113, 385)` then `insert(113, 1)`. Two causes, and only one of them was in `Typing`: `restore_mark` applied a live mark's *length* to whatever leaf was open, ignoring the offset the mark recorded, so a leaf that had moved was restored in the wrong place; and the walk itself subtracted the open span from its first state, an exception a root's redo used to force, which after D-34 is a one-byte lie. Instrumenting `leave` with the arithmetic it depends on (`settled + pending == document`, and the open leaf really in the tree at its offset) is what separated the two. | **resolved 2026-11-19 by D-34** — the walk passes for six seeds without the exception, and the million keystrokes pass with undo and redo in the mix |
| OQ-23 | **`Typing::pending` sliced past the end of its buffer.** Found 2026-11-19 by running the million keystrokes with undo and redo in the mix: `range end index 8 out of range for slice of length 3` at `pending()`, after about 165 s. `live_len()` falls back to `open.registered` when the overlay has nothing to say, so a state left behind by the paths that clear or shrink the buffer — a root restore cleared it, and re-attached only what was open — could claim more bytes than the buffer held, turning a length disagreement into a panic in a public accessor. | **resolved 2026-11-19 by D-34** — a root restore keeps the buffer, so nothing it does can leave a length over bytes that are not there; `len <= buffer.len()` is asserted in `attach`, `set_live` and `pending`, and the million keystrokes with undo and redo in the mix now pass |

## Audit trail

- **2026-11-21 — D-65 — `docs/TO-APP.md` counted gates that were never claimed.**
  "Where we are" ended "`ROADMAP.md` counts 15 gates claimed and 18 open". Both numbers are
  invented. `crates/holonomy-ledger/src/laws.rs` holds **ten** laws and every one is
  `LawStatus::Spec`; `docs/GATES.md`, which `xtask gates` generates from that table, shows the same
  ten rows all reading `spec`, and no other document in the repo repeats the figure. The claim was
  worse than a stale count because of what it asserted: a `gated` law is one an enforcing block has
  measured in CI, so "15 claimed" said the opposite of the standing rule that a law is called
  `gated` only when a harness ran green on it. Two nearby statements in the same section had also
  gone false — "M5 to M8 are untouched" and "a session is not yet durable" — when M5 and M6 landed.
  All three are rewritten to what the code does, and `README.md` now leads its Status section with
  the count that is true: **zero of ten**. Found while re-reading the docs for this merge, which is
  the point of reading the code rather than the prose. Docs only; the CI run for this commit is
  what verifies it, and `AUDIT.md` names the run once it is green (OQ-1).
- **2026-11-21 — D-64 — the durability claim was stronger than the durability.** The window's
  autosave comment read "nothing is lost if the machine dies between keystrokes", and the module
  documentation promised that "what you write is still there when you open it again". The first is
  true of every **sealed** byte: `Typing::seal` appends and witnesses the moment the buffer fills,
  so the exposure between saves is bounded by `store::CAPACITY` (16 KiB) and does not grow with how
  long someone types. It is false of the bytes in the open buffer, which are written only by
  `Ctrl+S`, by the pause autosave, or on quit — and a crash before any of those loses them. The
  second sentence is true but was doing the work of a guarantee it does not make. Both now say the
  bounded thing, which is a real property and worth stating precisely: **at most one buffer's
  worth of the most recent typing**, never anything older. Docs only; **verified by CI run
  44908126224 on `636af4b`, every job green.**
- **2026-11-21 — D-63 — an empty passphrase was accepted, and a closed tty was read as one.
  FIXED.** `prompt` collected bytes until `\r`; `stdin.read` returning **0** — `Ctrl+D` at a
  terminal, or a tty that went away — left the loop by the same `break` a typed line takes, and the
  phrase was then whatever had been typed, which could be nothing. Nothing downstream refuses an
  empty one: `MasterKeys::derive_with` accepts it, `Document::create` writes the file, and the
  window prints "saved". A document protected by nothing is indistinguishable from a document
  protected by something until the day it matters, and the prompt is the only place it can be
  caught. Both cases
  are refused now — end of input, and an empty line — with the reason beside each, and the `Ctrl+C`
  path keeps its own message so a cancel is not reported as an empty phrase. **Verified by CI run
  44907174415 on `f6ce706`, every job green.** Residual: the phrase's bytes are still best effort,
  which OQ-28 owns.
- **2026-11-21 — D-62 — a document still inside its buffer had no height at all. FIXED.**
  `Measured` keeps one height per **spine slab**, and the bytes a user is typing are in the open
  leaf — which is in the spine, so it has an entry, but nothing measures it until a frame draws it.
  Every other entry was fine and the total was still wrong for the commonest state in the product:
  a new document. `reflow` laid out no leaves at all when nothing had been sealed, `scroll_to`
  clamped every scroll against a total of zero, and Page Down did nothing on a document several
  screens long. The same zero made `caret_place` report a caret in an unmeasured run at the top of
  the document, which is how D-53's reveal scrolled to a fraction of the right line: the failure
  was found by pushing the branch through CI and reading `caret=166 scroll=23040` against a frame
  covering bytes 40..65.
  Two fixes, at the two places the missing measurement was read. `measure_through` measures the
  unmeasured run a geometry question depends on — one leaf at a time, carrying the line as `reflow`
  does — so a prefix sum is exact before it is used; the leaf before the run is re-measured rather
  than trusted through `carry_after`, because the index stores the carry a leaf was *measured with*
  and the cluster tail it placed past its own end differs by one advance, an error `is_measured`
  would then make permanent. And `Editor::tail` keeps the open leaf's height beside the index,
  invalidated by a length change, a width change, or a reflow, so `scroll_to` and `reflow` see the
  whole document. The tail is kept out of `Measured` on purpose: `byte_window` asserts that the
  index's leaf *i* is the spine's leaf *i*.
  A third fault fell out of the same investigation: `caret_place` read `prefix(leaf)` with a leaf
  number from the spine while the index was a snapshot only `frame` kept in step, so a reveal asked
  for between a keystroke and a frame indexed past the end of the model and panicked with "prefix
  past the model". It syncs first now — the invariant it was silently relying on.
  Pinned by `typing_past_the_last_visible_line_scrolls_the_screen_with_the_caret`
  (`crates/holonomy-gui/tests/keys.rs`) and by `editing`'s two tail-leaf tests. **Verified by CI run
  44906611678 on `e898979`: every job green** — rustfmt, clippy `-D warnings`, the workspace test
  suite, the gates contract, the million-keystroke walk, the rsync measurement, and the ext4
  rehearsal.
- **2026-11-21 — D-61 — `Escape` was documented as dismissing and dismissed nothing. FIXED.**
  `Key::Escape` carries the doc line "dismisses, never edits", and the keymap does the second half
  correctly: it refuses to turn Escape into an edit. Nothing did the first half, so the key was
  inert in every state and the only way to close `Ctrl+G`'s security panel was the chord that
  opened it. The panel belongs to the binary rather than the editor and never reaches an `Intent`,
  so the binary dismisses it now, and its module docs say so beside the rest of the key list. This
  is the class of defect the audit exists for: not a wrong line of code, but a documented behaviour
  with no implementation behind it.
- **2026-11-21 — D-60 — a page was the screen the window had when it was built. FIXED.**
  `Keymap::page` is one number, set by `Keymap::new` and never told again — but the window's
  screen changes under it. The terminal is the visible case: `Ctrl+G`'s security panel takes rows
  from the text area and the binary hands that smaller height to `App::resize` on every loop, while
  Page Down kept stepping the height the window had at construction, so a page skipped lines the
  user never saw. A resized terminal does the same with no panel involved. `Keymap::set_page`
  exists now and `App::resize` sets it from `Editor::viewport`, so the page and the screen are the
  same number by construction. Pinned by `a_page_is_the_screen_the_window_is_showing_now`
  (`crates/holonomy-gui/tests/terminal.rs`).
- **2026-11-21 — D-59 — a refused MMR merge left the accumulator half-consumed. FIXED.**
  `Mmr::append` (`crates/holonomy-line/src/mmr.rs`) walked its merge by taking peaks **out of
  `self`** and returned early through `?` when one was missing. The caller received `Err`, but the
  accumulator kept a `count` its peaks no longer backed, and the next `append` merged against the
  holes while `root` and `encode_peaks` — both of which `expect` a peak for every set bit — could
  panic on a value an earlier call had already reported as an error. The merge is computed into a
  scratch copy now and committed only when it completes, and the height guard is checked before the
  indexing rather than after it, so the walk cannot index `peaks[HEIGHTS]` on its last iteration.
  Honest scope: the state that triggers this needs a corrupt-but-authenticated witness, so it is
  defence in depth rather than a reachable user failure. Pinned by
  `a_refused_merge_leaves_the_accumulator_exactly_as_it_was`.
- **2026-11-21 — D-58 — a reveal into an unmeasured leaf was clamped away. FIXED.** Found while
  writing D-57's test, and it is the same defect one layer down. `Editor::scroll_to` clamps the
  scroll position to `measured.total()`, which is the document's height only while every leaf is
  measured. A tail leaf pushed by `sync_measured` has no height until a frame draws it, so between
  a keystroke and that frame the total is short by exactly the leaf being typed into. A reveal
  computed the right position and then clamped it back to zero: the caret stayed off screen and
  `reveal_caret` returned `false`, so nothing asked for a repaint either. This is why D-53's fix
  was not enough on its own — the reveal had to exist **and** be able to reach the position it
  computed. `scroll_to_within` clamps against the larger of the measured total and the extent the
  caller has evidence for, `reveal_caret` passes the caret's own bottom, and `scroll_to` keeps the
  plain behaviour for every other caller. Pinned by
  `a_pushed_tail_leaf_is_laid_out_from_its_predecessors_carry`, which asserts both the position and
  that the reveal reported a scroll.
- **2026-11-21 — D-57 — a leaf the frame has not measured has no carry. FIXED.** Found while
  checking D-53's reveal against D-50's index. `sync_measured` grows the index for a tail append
  by `push()`, which records a zero height and a default carry; the frame measures the leaf the
  first time it draws it. Everything that lays a leaf out *before* that frame ran from
  `Measured::carry(leaf)`, which answered `Carry::default()` — and `Carry::default()` says "this
  leaf begins a line", true only when it does. A seal cuts on a byte count, not a line boundary,
  so a new tail leaf regularly begins mid-line, and then `caret_place` reported the caret one line
  too high, `reveal_caret` scrolled to that line, `line_start` found the wrong line for a scroll
  position, and `cluster_before` could cut a `\n` away from the `\r` in front of it (the phantom
  line D-25's `leaf_text` exists to prevent). The first version of the fix tested the height for
  zero, which is wrong: a leaf all of whose bytes its predecessor placed measures exactly zero and
  is fully measured. `Measured::carries` is now `Vec<Option<Carry>>`, presence is the record, and
  `Editor::carry_of` answers an unmeasured leaf from the last measured leaf at or before it. Three
  call sites that assumed a carry was always there ask it. Unverified: no compiler here. Pinned by
  `a_pushed_tail_leaf_is_laid_out_from_its_predecessors_carry`
  (`crates/holonomy-shell/tests/editing.rs`), which cuts a leaf mid-line and asks for the caret's
  place before any frame has measured the new leaf.
- **2026-11-21 — D-56 — an escape mid-character abandoned the UTF-8 state, not the character.
  FIXED.** `Decoder::feed` (`crates/holonomy-gui/src/term.rs`) tested the escape state before the
  multi-byte state, so an `ESC` arriving between a character's lead byte and its continuation
  bytes left `expect` standing; the continuation bytes then completed a character out of an
  abandoned lead, and the keystroke after them was swallowed as the next continuation. A tty
  delivers `ESC` between any two bytes — an arrow pressed during a paste, an interrupted writer,
  noise on a serial line — so the sequence is not hypothetical. An escape now discards the
  half-character, and so does `flush_escape` (the end of a read), because a stream that dies
  mid-character must not poison the next one. Pinned by
  `an_escape_mid_character_does_not_swallow_the_bytes_after_it`.
- **2026-11-21 — D-55 — a font could make a line box run backwards. FIXED.** `Face::line_height`
  is ascender − descender, read straight out of `hhea` as two `i16`s. A font with `descender >
  ascender` — malformed, hostile, or a hand-edited file — produced a **negative** line height, and
  nothing downstream catches it: `HeightModel::set` only *debug*-asserts that a height is not
  negative, so a release build would store it and every prefix sum after that leaf would be wrong.
  The visible result is a scroll bar that lies and a reveal that scrolls to the wrong row, both
  silently. `Face::parse` now refuses such a font with `FaceError::BadTable("hhea")`, which is an
  error a caller can show; the alternative — clamping in `line_height` — would have left every
  other consumer of a malformed `hhea` still trusting it. Pinned by
  `a_font_that_would_make_a_line_run_backwards_is_refused`
  (`crates/holonomy-eye/tests/real_face.rs`), which mutates the shipped Inter's `hhea` and checks
  both the refusal and that the unmutated font still parses.
- **2026-11-21 — D-54 — a character wider than the buffer was an infinite loop. FIXED.** Found by
  reading the keystroke loop against `SlabText`'s own rules rather than against its comments.
  `Typing::type_bytes` sizes each chunk to the room left in the buffer and walks `take` down until
  the split lands between characters. When the *next character alone* is wider than the whole
  buffer, `take` reaches zero and the loop's only other move is `seal()` — which, with nothing
  open, returns `Ok` and changes nothing. A `Typing::new(1)` session and a three-byte character
  spun forever inside a public API that owed the caller an error. `take == 0` with nothing open
  now returns `TypingError::TooLarge { needed, capacity }`. With a source open the seal is real
  progress, so that path still seals and continues. Pinned by
  `a_character_wider_than_the_buffer_is_refused_rather_than_spun_on`.
- **2026-11-21 — D-53 — nothing measured the screen, revealed the caret, or re-measured on resize.
  FIXED.** Three omissions with one symptom: the window stopped following the person typing.
  `Editor::reflow` — the one place in the loop that measures every leaf, and therefore the only
  way a scroll position past the first screen can be found — was called by no one but an intent a
  front end never sent, so a document longer than a screen had one measured screen and could
  scroll no further. `reveal_caret` had the same status: the shell has the policy, and no front
  end asked. And `resize` empties the measured index on a width change *by design* (every height
  described the old wrap) while leaving the re-measure to a caller that did not re-measure. The
  window now reflows when it opens a document, asks for `Intent::RevealCaret` after a keystroke
  that moves the caret, and asks for `Intent::Reflow` when the width changed. It asks through the
  intent layer rather than calling the editor, because a scroll performed behind the layer's back
  is a scroll no `Effect` told a renderer about. Pinned by
  `typing_past_the_last_visible_line_scrolls_the_screen_with_the_caret`
  (`crates/holonomy-gui/tests/keys.rs`).
- **2026-11-21 — D-52 — a seal over no bytes answered `Ok` and kept the source open. FIXED.**
  `Typing::seal` began `if len == 0 { return Ok(()) }`, and `attach(0)` opened a source whose leaf
  `insert_slab` then dropped as empty. Both leave the register holding a source that owns no
  bytes: `open_count` says one, `pending()` is empty, and the window's "is the file up to date"
  question — which it asks every pause — can never be answered consistently. `seal` over zero
  bytes now takes the open leaf out of the tree and abandons the source; `attach(0)` moves the
  caret and opens nothing. Honest scope: the reachable paths already avoided this state (`delete`
  closes a leaf it empties, and `restore_mark` is the only caller that can hand `attach` a zero),
  so these are guards plus the removal of a lie, not a repair of a user-visible failure — the
  tests for them are white-box for that reason
  (`a_seal_over_no_bytes_abandons_the_source_instead_of_pretending`,
  `attach_over_no_bytes_moves_the_caret_without_opening_a_source`).
- **2026-11-21 — D-51 — the shell compared live offsets with sealed geometry. FIXED.** A document
  has two coordinate spaces while a leaf is open: the **sealed** offsets the spine stores, and the
  **live** offsets the user's caret, selection and reveal use. `leaf_of` binary-searched sealed
  leaf starts against a live caret, and `leaf_bounds` returned sealed bounds for a live lookup.
  Every leaf after the open one moves by the difference between its live and registered length, so
  both were off by that amount whenever the open leaf had grown or shrunk — which is every
  keystroke after the first. `Draft::live_offset` is the reverse of `locate`: the same edit walk,
  answering where a sealed leaf start lands in the live document. Both call sites ask it, and
  `caret_place` goes through `leaf_bounds` instead of a raw `slab_span`. Pinned by
  `a_sealed_leaf_start_maps_to_the_live_offset_that_holds_it`
  (`crates/holonomy-text/src/draft.rs`).
- **2026-11-21 — D-50 — any one-leaf growth was taken for a tail append. FIXED.** `sync_measured`
  kept the height index sized to the leaf count with a fast path: one new leaf, one `push`, O(log
  n), because — the comment said — a new leaf can only appear at the end. It can: a caret moved
  into sealed text and typed into **splits** a leaf, which also adds one. The index then kept
  every measured height after the caret attached to leaves that had moved, and the leaf past the
  end of the index was never measured at all, so the total height was wrong by an unbounded
  amount. The fast path is now byte-anchored: the new last leaf must begin at `len_synced`, the
  document length when the index was last true. Anything else rebuilds the index, which the next
  frame repopulates for the leaves it can see. Pinned by
  `a_tail_append_keeps_the_index_and_a_mid_document_insert_discards_it`, which asserts the two
  cases against each other: a tail append keeps the heights before it, and a mid-document insert
  is followed by a re-measure that agrees with one continuous layout of the whole document.
- **2026-11-19 — D-31 — a seal writes the live bytes, not the buffer behind them (found by the
  gate it was written for).** The first run of D-30's harness failed within a second, on a length
  the session and the independent model disagreed about by exactly the three bytes an undo had
  taken back. `Typing::seal` appended `buffer.as_bytes()` — the whole resident buffer — while the
  document's length comes from the *live* prefix of it; an undo deliberately leaves the bytes it
  took back *behind* that prefix so redo can hand them over without copying anything. A seal is an
  undo floor (`steps` and `undone` are cleared), so the tail can never be redone, and writing it
  appended text that no mark, no root and no leaf could name: the document grew by exactly what
  undo had removed. `seal` now appends `live_len()` bytes and the corrected slab carries that same
  length, so a flush after an undo writes what the document says. Pinned by
  `a_seal_writes_the_live_bytes_and_not_the_undone_tail_behind_them`, and the harness keeps what
  it found the bug with: a failing step now names the action, its offsets and the caret, so the
  next one is a sentence rather than a bisect. Nothing about the *path* changed — a keystroke
  still allocates nothing (`tests/keystroke_allocations.rs`) and a seal still appends once.
- **2026-11-21 — OQ-27 — the window leaks one DEM key per opened document. OPEN, by design for
  now.** `WorldLine<'key>` borrows the `DEMKey` it seals with, so a `Document` that owns both the
  line and the key would be self-referential. The alternatives were `unsafe` transmutes, a
  self-reference crate (a new dependency this environment cannot fetch), or leaking the boxed key —
  and the leak is the smallest of the three: thirty-two bytes, once per open, for as long as the
  process lives, which is exactly how long the window would have kept it. The cost is not memory but
  hygiene: a leaked key's `Drop` never runs, so it is not zeroized at exit, and
  `crates/holonomy-gui/src/store.rs` says so at the leak site. The fix is an owning `WorldLine` (an
  `Arc<DEMKey>` inside the line, or a key handle the container owns), which removes the leak and the
  sentence together; it is a change to `holonomy-line`'s public shape, not to any format.
- **2026-11-21 — OQ-28 — the passphrase typed at the prompt is zeroed, but it was never a
  `Zeroizing`.** The terminal front end collects the phrase into a `Vec<u8>`, hands it to
  `MasterKeys::derive_with`, and overwrites the bytes before dropping the vector
  (`crates/holonomy-gui/src/bin/holonomy.rs`). That is best effort, not the standard the rest of the
  workspace keeps: a `Vec` that reallocated as it grew leaves earlier copies in allocator slack, and
  nothing wipes them. `holonomy_seal::PassphraseInput` exists to carry exactly this kind of material
  and is what the fix should use on the way in — the prompt is the only place in the window a secret
  exists as bytes at all, and it should exist in the wrapping the seal crate already provides.
- **2026-11-21 — D-43 — one arrow key threw the whole undo history away. FIXED.** Found by
  `tests/terminal.rs` the moment it drove `Ctrl+Z` through the bytes a terminal actually sends:
  after `helo`, a backspace, `lo` and two Left presses, sixteen `Ctrl+Z` presses each came back with
  no effects and the document still five bytes — the undo stack was not at the wrong state, it was
  empty. The mechanism: moving the caret goes through `Editor::move_caret` → `Typing::insert_at(at,
  b"")`, which flushes whenever the caret leaves its offset, and a flush is a **seal** — the open
  leaf is appended to the world-line and the resident buffer it lived in is dropped. `seal` ended
  with `steps.clear(); undone.clear()` on the reasoning that no mark could promise bytes the buffer
  no longer held; true, and the consequence was that every caret move — an arrow, Home, End, a click
  — silently ended undo for everything typed into the open leaf. The window made it visible; the
  defect was in the session layer and would have been in any front end (D-40 and D-41 had each
  closed a neighbour of it — typing was undoable, and typing was visible — without noticing that
  *moving* cancelled the first). The fix: a seal keeps exactly **one** state, `Step::Root { spine:
  settled, at, pending, held: Some(copy of the open bytes) }`, whose `pending` is the top mark's
  length when the top step is a mark — so one `Ctrl+Z` after a move takes back the burst that was
  being typed, or the whole typing when the mark named an empty document — and the leaf's own length
  when there is no mark at all. `Step::Root` gained `held`, the copy an undo re-attaches from now
  that the buffer is gone; every root step carries it whenever it has open bytes, because a redo can
  land after a seal has replaced the buffer with another leaf's. Two wrong versions died on the way,
  both caught by the suite rather than by review: keeping *all* the marks behind the seal left marks
  of older leaves applying lengths to a buffer a later leaf had become — the million-keystroke walk
  reported it as a document cut inside a character (`valid_up_to: 114`, a lone `\xe2` where a ZWJ
  belonged) — and a first version without `held` panicked slicing fourteen bytes out of a four-byte
  buffer. Keeping one state is what makes the remaining history sound: a mark is a *length*, two
  leaves can start at the same offset, and a length is only meaningful against the bytes it was
  measured on. Residual, recorded rather than hidden: history deeper than the last mark does not
  survive a sealing move — that is the floor the seal was, narrowed to one step. Pinned by
  `a_caret_move_does_not_throw_the_undo_history_away` and `a_seal_keeps_the_last_burst_undoable`
  (`crates/holonomy-text/src/typing.rs`), by the end-to-end terminal test that found it, and by the
  million-keystroke walk's rewind.- **2026-11-21 — D-42 — every arrow key typed a letter into the document. FIXED.** Found by the
  terminal backend's own unit test, on the first run that reached it: `keys(b"\x1b[A")` came back as
  `[Ignored, Character('A')]` instead of `[Up]`, and `keys(b"\x1b[C")` as `[Ignored,
  Character('C')]`. The mechanism, in `Decoder::feed` (`crates/holonomy-gui/src/term.rs`): while an
  escape sequence was being accumulated the decoder looked for a CSI *final* byte — the range
  `0x40..=0x7e` — in the byte it had just appended, with no exception for the second byte. `[` is
  0x5b, inside that range, so `ESC [` was complete the moment it arrived; `interpret` saw a two-byte
  sequence with nothing after the introducer and answered `Ignored`, and the `A` that followed was
  read as an ordinary printable byte. The arrows therefore walked into the edit path as text: press
  Up and the letter `A` appears in the document, `C` for Right, `D` for Left, `B` for Down — and
  with modifiers the rest of the sequence (`[1;2` then `D`) becomes a run of digits. This was
  invisible to the keymap (`keys.rs` presses `Key` values directly) and to everything before M4b,
  because nothing had ever encoded a tty's bytes. The fix: the second byte is the introducer and
  cannot be a final byte — `len == 2` is complete only when that byte is neither `[` nor `O`, and
  from the third byte on the `0x40..=0x7e` rule applies. The test that found it
  (`escape_sequences_are_arrows_and_their_modifiers`) covers `ESC[A`, `ESC[C`, `ESC[1;2D`, `ESC[3~`
  and `ESC[5~`; `tests/terminal.rs` re-proves the path end to end by feeding those same bytes
  through the decoder and asserting where the caret landed.
- **2026-11-20 — D-41 — a frame showed nothing of the bytes being typed into the open leaf.
  FIXED.** Found by M4's window tests: five keystrokes left a document of five bytes and a frame of
  two, and only `Ctrl+S` made the rest appear. The mechanism, traced through the code: the frame's
  window is `byte_window` over the `HeightModel`, and that function ends the window at
  `(last leaf offset + last leaf len).min(spine.len())` — and `Spine::len` is the **sealed**
  document. Bytes typed into the open leaf are in the draft, one leaf past the last sealed one, so
  they were cut off the end of every window: a user would type and watch the screen stay still until
  something happened to seal. `sync_measured` had the same blind spot — it sizes the height index by
  `spine.slab_count()`, so the open leaf never had an entry at all. The fix, in `Editor::frame`: a
  window whose end reaches the sealed spine's end reaches the **document's** end instead, and the
  frame says `provisional` because the leaf holding that tail has no measured height yet; the
  write-back loop now drops a measurement for a leaf the index does not have (the open one) rather
  than writing past its end. Nothing about sealing changed: the open leaf is still not written to
  the world-line until it is sealed, which is what keeps L-6's one-append-per-leaf budget. The
  former reproducer `typing_is_visible_before_the_bytes_are_written_through`
  (`crates/holonomy-gui/tests/keys.rs`) is a plain green test now: five keystrokes, five bytes, five
  clusters on the surface, the caret after the last one.
- **2026-11-20 — D-40 — typing was not undoable: nothing marked a burst, so undo had no step to
  return to. FIXED.** Found by M4's keymap test, which typed `abc`, pressed `Ctrl+Z`, and expected
  an empty document. Undo in this system is a stack of **marks**, not keystrokes: `Typing::undo`
  returns to the last `Step::Mark`, and nothing in the shell or the loop ever called `Typing::mark`
  — so a window could type a paragraph and `Ctrl+Z` would report "nothing to undo" while the
  paragraph stood there. Every headless test that exercised undo did it after a **cut**, and a cut
  marks on its own, which is why the gap survived three blocks of tests. The fix has two halves: the
  loop now exposes `Editor::mark`, and the window's half (`holonomy_gui::App`) marks at the start of
  a typing **burst**, so consecutive keystrokes coalesce into one undo step and `Ctrl+Z` takes back
  the run — the behaviour every editor has, now stated in one place. The regression is
  `shortcuts_are_the_loops_own` in `crates/holonomy-gui/tests/keys.rs`: type `abc`, undo, empty,
  redo, `abc`. Nothing could be lost by this defect — the bytes were always the document's — but a
  user could not take back a paragraph, which for a writing application is a defect in the first
  sentence of the manual.
- **2026-11-21 — D-49 — the window said "saved" over a save that had failed. FIXED.** `run`
  returned `Ok`, and `main` printed `outcome_words(true, None)`-style words for it; the final save's
  own answer was thrown away with `let _ = save(...)`. A session whose last append was refused
  therefore exited 0 and told the user everything was written. The save's answer is now part of the
  sentence (`Ended::{NothingToWrite, Saved, SaveFailed}`), a `SaveFailed` exits non-zero, and a
  dead session outranks a failed save in the wording. Pinned by
  `a_failed_save_is_never_called_saved` (`crates/holonomy-gui/src/bin/holonomy.rs`).
- **2026-11-21 — D-44 — backspace did nothing on a terminal whose erase key is `^H`. FIXED.** A tty
  with `stty erase ^H` sends `0x08`; every other sends `0x7f`. The decoder mapped only `0x7f`, so
  the backspace key produced no intent at all — while the passphrase prompt, which reads raw bytes,
  accepted both, so the same key worked before the document opened and stopped working after.
  `control` maps `0x08` to `Key::Backspace` now, and the test
  `backspace_arrives_as_either_byte_a_terminal_sends` pins both bytes to the same key.
- **2026-11-21 — D-45 — `Ctrl+Y` was documented as redo and was not decoded. FIXED.** The module
  documentation offers `Ctrl+Y` precisely because a terminal sends `Ctrl+Shift+Z` as `0x1a`, the
  same byte as `Ctrl+Z` — so the chord the keymap would need cannot be distinguished from the one
  that undoes. Nothing mapped `0x19`, which left the documented redo key inert and redo unreachable
  from a keyboard. `control` maps `0x19` to `Ctrl+Y` now. The `Ctrl+Shift+Z` ambiguity is not a
  bug to fix but a fact to state, which the doc list and
  `ctrl_y_is_redo_and_ctrl_shift_z_cannot_be_told_apart` both do.
- **2026-11-21 — D-46 — a lone `ESC` and the start of an arrow were the same thing. FIXED.** The
  window flushed a pending escape after every read, so a terminal that delivered `ESC` and `[A` in
  two writes — over ssh, or under load — had `[A` typed into the document at the caret. An Up arrow
  inserted two characters. The decoder keeps the partial sequence (`Decoder::pending`) and the
  window settles it only when no further byte is waiting for a grace period; reads that arrive empty
  stay non-destructive. Pinned by `escape_sequences_are_arrows_and_their_modifiers`.
- **2026-11-21 — D-47 — "unsaved" was a flag that lied in both directions. FIXED.** `App` kept its
  own dirty bit and set it on **every** key. A caret move seals the open leaf, so an arrow key
  commits the bytes — after which the status line still said "unsaved" about text that already had a
  witness — while a refused save left bytes unwritten and the line could say "saved". The truth is
  in the session: `has_pending` asks `Typing::pending`, so the status line cannot disagree with the
  file. `an_arrow_key_writes_the_bytes_so_the_document_is_not_unsaved`
  (`crates/holonomy-gui/tests/keys.rs`) asserts the seal, the append, and that a flush with nothing
  open writes nothing.
- **2026-11-21 — OQ-25 — the terminal surface is one cell per cluster, so a wide character claims
  half the screen the terminal gives it. OPEN.** `CellShaper` advances one logical unit per cluster
  and the renderer writes one character per column, while a terminal draws an East Asian wide
  character, most emoji and a handful of symbols in **two** cells. The disagreement is structural,
  not a rounding error: `screen_with` lays out `columns` characters per row, so a row containing a
  wide character is drawn two cells wider than the row it was measured for, and everything below it
  shifts by one column — the same class of failure D-48 fixed for rows. It was cited in
  `crates/holonomy-gui/src/term.rs` as "recorded as OQ-25" while no such entry existed, which is the
  failure mode this file exists to prevent: a deferral with no owner, no reason and no date. Closing
  it means a display-width function in the eye (`unicode-width`, or the East Asian Width tables
  `holonomy-eye` already generates) consulted by both `CellShaper::advance` and the renderer, so the
  cell grid and the terminal agree by construction rather than by hope. Nothing else is blocked on
  it: the document, the geometry and the world-line are unaffected, and a document of ASCII and
  narrow characters — which is what this release is typed in, per OQ-24's same scope — is exact.
- **2026-11-20 — OQ-24 — shaping is metrics only, and nothing reads `GSUB`/`GPOS`. OPEN, by
  design for now.** `holonomy_eye::Face` reads `head`, `hhea`, `hmtx`, `maxp` and `cmap` (M3), so
  advances, ascent and descent are the font's own numbers — but a cluster's width is the *sum of
  its characters' advances*. That is exactly right for one-glyph-per-character text, which is what
  this release is typed in, and it is a measurement rather than a promise for everything else:
  kerning pairs, `fi`-style ligatures, mark positioning and Arabic joining are not applied, and a
  renderer that *did* apply them would draw glyphs the Eye measured differently — the two-geometry
  disease with a nicer name. The rule until a real shaper lands: **whatever draws must not
  substitute glyphs**, and `Face`'s widths are the only geometry. Closing this means putting
  HarfBuzz (or an equivalent) behind the `Shaper` trait, which changes no geometry code because the
  trait is the boundary; it is also what Block 5 gate 3 (BiDi) and any non-Latin script need.
- **2026-11-20 — D-39 — a frame wrote a clipped leaf's carry back as the leaf's own, so the
  second frame at a scroll position started 16 bytes early. FIXED.** Found by the frame-stability
  hunt (`crates/holonomy-shell/tests/frame_stability.rs`). On a four-thousand-leaf document at
  `scroll = 40` units — the top of the third line — the first `frame()` reported `origin = 400`,
  where that line starts, and an immediately following frame with nothing edited reported
  `origin = 384`, the first byte of the leaf the line starts sixteen clusters into. The heights
  were never involved: every leaf's measured height and the measured total were asserted unchanged
  across both frames, which is what pointed at the *carries* rather than the heights. The
  mechanism: `Spine::spans_in` returns spans **clipped** to the requested range, so the leaf a
  window begins inside arrives as a head span whose offset is the window's own start, not the
  leaf's; `measure_window` treated every span whose offset is at or after the window's start as
  wholly measured and wrote its `Carry` back, and for that head the carry is the geometry of a
  layout beginning in the *middle* of the leaf — first cluster at x = 0, no skip. The next frame
  read that carry, laid the leaf out from x = 0, and found a line start at the leaf's own first
  byte. The fix: `frame` decides whether the window's first byte is a leaf's first byte
  (`Draft::locate` → `LiveHit::within`), and `measure_window` writes a head span's carries back only
  when it is — heights are written either way, because the lines a clipped head holds start inside
  it wherever the window began. A clipped span at the window's *end* was already excluded. The
  regression is `a_second_frame_at_the_same_scroll_is_the_first_frame`, which asserts the whole
  `Snapshot` — text, origin, height, lines, caret — is identical across two consecutive frames at
  four scroll positions, and the companion test asserts no leaf's height or the total moves.
  Nothing could be lost by this defect: it was the screen's top line moving under a scroll offset
  that had not changed.
- **2026-11-19 — D-38 — Block 5 gate 1 is claimed, and proving it corrected two beliefs about
  layout — one of them in the code's own documentation.** The gate reads *"for arbitrary documents,
  `hit_test(paint_to_point(g)) == g` for every shaped glyph"*. `crates/holonomy-eye/tests/
  one_geometry.rs` proves it over **generated** documents (four seeds, three each, drawn from an
  alphabet of proportional extremes, combining marks, emoji sequences, CJK, Devanagari-shaped
  clusters and all three hard-break spellings) at three wrap widths and the 8 zoom × 5 DPI sweep,
  asserting: every painted caret comes back from its own point with the same line, top, height and
  x — and the same *byte* wherever that line's boundaries are distinct at device resolution; clicks
  on a boundary land on it, clicks between two boundaries choose one of those two; and the printed
  counts (`HOLONOMY_ONE_GEOMETRY`) are part of the claim, so a run that checked nothing could not be
  green. A **monospace control** measures the v3 bug directly: on `mimimimi`, a cell-per-cluster
  model puts at least half the carets somewhere paint did not, which is what makes the property
  test discriminating rather than decorative. Claimed on CI run `44672133052` (`2caa52a`), test job green.
  Two beliefs the harness had to correct, both about layout and both now pinned by tests: an
  **empty document is one empty line with exactly one caret** (byte zero, which both consumers
  answer identically — the first version of the test asserted no caret at all), and **a line's
  boundaries include its end whenever it offers one**, which the document's last line always does
  even with no hard break after it. The second was not merely a wrong test: `Line::has_end`'s
  documentation said a line offers an end caret "when the line ends in a hard break", while the
  layout gives the final line one unconditionally — so a caller reading the comment would have
  concluded the last glyph had no caret to its right. The comment is amended to say what the code
  has always done, and this gate's harness is the evidence for it.
- **2026-11-19 — D-37 — what "statistically identical latency curves" means, since no shared
  machine can certify that two nanosecond figures are equal.** Block 4's second gate reads:
  *"Keystroke at offset 0 and at offset `N-1`: statistically identical latency curves."* Taken
  literally it is unfalsifiable in CI (two equal figures would be a rounder of the clock, not a
  result) and, if measured naively, it measures the machine: two curves taken one after the other
  differ whenever the runner changes speed between them. The gate is therefore given an operational
  definition, and `crates/holonomy-text/tests/keystroke_latency.rs` implements it.
  **Interleaved, not sequential:** blocks of 256 samples at offset 0, then 256 at `N-1`, repeated to
  4,096 each, so drift under load moves both curves and a difference that survives belongs to the
  code. **One keystroke per sample:** `Spine::locate` then an in-place edit of the resident slab —
  D-2's keystroke path — on a 180 MB, 43,945-leaf document; insert and backspace alternate with the
  same parity at both ends, so the sampled mixes are identical and the fixture cannot drift. Both
  viewports are warmed first, because a fault-in is slab-boundary work and not the keystroke.
  **The band, stated rather than implied:** p50 within a factor of two and p99 within a factor of
  four, which rejects the failure mode the clause exists to catch — cost proportional to the offset,
  a factor of thousands — and does not pretend to certify equality a shared runner cannot deliver.
  **A ceiling, not a certificate:** L-4's 8 ms p99 is asserted as an upper bound, not claimed as an
  echo measurement on reference hardware (Block 6 owns that). **A control:** a whole-document walk
  is timed by the same clock and must cost the keystroke's price a hundred times over, so a fake
  clock or an idle fixture fails loudly. The numbers are printed as `HOLONOMY_KEYSTROKE_LATENCY`
  and `HOLONOMY_KEYSTROKE_CONTROL`, and every one of them is also an assertion — so the run that
  claims the gate is the run that enforced the numbers. Claimed on CI run `44665191051` (`32c58c9`), test job
  green: the interleaving ran, both bands held, the echo ceiling held at both ends, and the
  whole-document walk cost the keystroke's price more than a hundred times over. The first attempt
  failed, and its failure was the harness's own: `summarise` and `measure` must agree that p50 is
  the **upper** median, which is now pinned by a unit test at the entry point that takes the
  caller's samples.
- **2026-11-19 — D-36 — "memory at open" is two claims, and only one of them is fixed.** Block 4's
  first gate reads *"Open a synthetic 30 M-word document: memory at open is within a fixed bound
  that identical input of 1 page also satisfies."* Measured, the sentence splits in half.
  `crates/holonomy-text/tests/open_memory.rs` opens both documents — 180 MB of text as 43,946
  leaves, and one page as one leaf — with the counting allocator installed, and asks them the same
  questions. **Resident plaintext is exactly what L-2 says**: a `ResidentSet` with a fixed
  viewport budget, asked for four thousand segments of the big document, evicts and holds the same
  bound the page-sized one holds; the budget is a construction argument, never the document's
  length. **The index is not fixed**: a spine is `2n - 1` nodes of `Arc<Node>` — a 16-byte header
  over two child pointers and three aggregates — so it costs a constant a leaf: the layout's
  arithmetic gives 112 bytes (4.9 MB for the big document, some 56 bytes for the page), and CI
  proves the band that arithmetic sits in rather than trusting the digits: 64-192 bytes a leaf,
  and the index under five percent of the document's own bytes. That is the price of `locate` being a descent rather than a scan (D-1);
  making it fixed would mean lazily materialising the tree, which is a redesign of the one
  structure every operation descends, and it would buy a few megabytes on a document that already
  needs a world-line to exist. The clause is therefore **amended** rather than failed silently:
  gate 1 now claims a fixed bound on *resident plaintext* (the memory L-2 bounds) plus a measured
  per-leaf cost for the index, and both are asserted by the named test — printed, and asserted, so
  a green run *is* the measurement rather than a log nobody reads. Claimed on CI run `44579774910`, whose
  test job enforced the resident bound (equal at both sizes), eviction under four thousand faults,
  the per-leaf band, and the index under five percent of the document.
- **2026-11-19 — D-35 — a cut wipes what it vacates, and a slab wipes its whole allocation.**
  Found by review while hunting (not by a failing test, because nothing in safe code can read the
  region involved). `SlabText::delete` shifts the surviving bytes left and truncates, which leaves
  the bytes it removed — a cut password among them — inside the allocation but outside the length.
  `SlabText` holds a `Zeroizing<Vec<u8>>`, and `Zeroize for Vec<u8>` wipes what the **length**
  covers, so the vacated bytes were freed un-wiped, and so was any spare capacity an earlier, longer
  slab had filled. Two changes: `delete` zeroizes the range it vacates before the length stops
  covering it (pinned by a `debug_assert` that the range reads zero, so any regression fails every
  debug test that cuts), and `SlabText`'s drop extends the vector to its capacity first, which puts
  every byte of the allocation inside the guard's reach — `resize` within the capacity allocates
  nothing. The leak is invisible to a test: after the cut the vacated range is outside the length,
  and the spare is outside it always. What is stated here is the argument, not a measurement, and
  the unit test pins the observable half (a cut shrinks the length, keeps the capacity, and does not
  reallocate).
- **2026-11-19 — D-34 — a root restore keeps the buffer, and a mark that names another leaf moves
  it.** D-33 made undo-then-redo exact for the case it was written for and left two things behind:
  OQ-22 (a redo that put its open bytes back in the wrong place) and OQ-23 (`Typing::pending`
  slicing past its buffer, eight bytes claimed over three). Both came from the same decision — D-33
  *cleared* the buffer on a root restore and carried the open bytes in the step as a copy — and both
  go away when that decision is reversed:
  (a) **The buffer survives a root restore.** A mark is a length into the buffer, never a copy; that
  is why undo is allocation-free, and it is also why the buffer cannot be thrown away while marks
  that measure it are still live. `Step::Root` therefore carries `pending: usize` — how many bytes
  were open — instead of a `Vec<u8>`, and `restore_root` closes the source, keeps the buffer, and
  re-attaches `pending` bytes by length. A copy of at most a capacity per undo becomes a length; the
  bytes redo needs were never anywhere else.
  (b) **A mark whose leaf is not the open one moves the leaf.** `restore_mark` used to apply
  `Mark::Live { at, len }` to whatever leaf was open, ignoring `at` — a length-only restore that is
  right only when the leaf never moved, and undo and redo can move one. It now takes the open leaf
  out of the tree and re-attaches it at the mark's offset when the offsets differ.
  (c) **The invariant is asserted where it is used**, because this family of bugs is invisible in a
  length check and obvious in the arithmetic: `settled_spine().len() + pending == document_len()`
  inside `leave`, "the open leaf is in the tree at its stated offset" inside `settled_spine`, and
  `len <= buffer.len()` in `attach`, `set_live` and `pending`.
  OQ-22's *last* byte was not in `Typing` at all: the walk had been subtracting the open span from
  its first recorded state (the exception a root's redo used to force), and once a redo hands the
  open bytes back, that subtraction is a one-byte lie about the state the run started in. The walk
  now records the document as it is. With all four changes the six-seed walk passes, and the million
  keystrokes pass **with undo and redo in the mix** — which is what lets
  `ROADMAP.md`'s third gate be claimed. `StepSummary`/`top_undo`/`top_redo` are `#[doc(hidden)]`
  additions that exist so a failure says which state was wrong; nothing in the session reads them.
- **2026-11-19 — D-33 — a root's undo keeps the bytes it drops, so its redo can put them back.**
  OQ-21's arithmetic, found by D-30's runner: undo of a root step, then redo, came back one byte
  short (seed 2, 409 bytes expected, 408 returned, first difference at byte 260). The mechanism was
  not a stray byte but a design gap. `leave` records the state to come back to; for a root that
  state is `settled_spine()` — the tree **without** whatever was open — because a root names written
  bytes only and a leaf over pending bytes would name a source undo abandoned. That is right for the
  undo: the pending bytes were typed after the cut, so the state before the cut did not have them.
  It was wrong for the redo, which then re-applied the cut to a document that had already lost them:
  undo-then-redo silently deleted the last thing typed, one byte at a time if that is how it was
  typed. `Step::Root` now carries `pending: Option<Pending>` — the open bytes, **copied** (at most
  one capacity, taken on an undo, never on a keystroke, because the buffer itself is about to be
  cleared) — and `restore_root` re-attaches them under a new provisional id at the offset they sat
  at, so the document reads as it did and the bytes are open again, exactly as before. They stay
  pending: nothing is written twice, and a later seal appends them the ordinary way. What the copy
  buys is the involution — undo then redo is a no-op — which is what the gate's third clause
  measures and what a user expects from Ctrl-Z, Ctrl-Shift-Z. Pinned by
  `redo_of_a_cut_puts_back_the_bytes_that_were_open` (it replaces a test that asserted the old,
  lossy behaviour). It does not by itself make the gate's third clause hold: the six-seed walk moved
  from OQ-21's seed-2 reproduction to OQ-22, and with undo in the million-keystroke mix
  `Typing::pending` breaks an invariant (OQ-23). Those two are the reasons the gate stays unclaimed.
- **2026-11-19 — D-32 — a burst that fills the buffer splits between characters, not inside
  one.** Found by D-30's runner at seed 0, step 529 894 of the million: `insert at 309` refused
  `Text(InvalidUtf8)` with 239 bytes open and a three-byte character next in the burst.
  `Typing::type_bytes` took `min(room, rest.len())` bytes and handed them to `SlabText::insert`,
  which validates UTF-8 — so when the room left in the buffer was smaller than the character at the
  split point (two bytes of room, a three-byte arrow) the edit was refused outright, and the
  keystroke was lost. The split now walks back to a character start, and when the next character
  does not fit whole the seal goes first, so the character begins the next source. A seal that
  lands *inside* a character would have been worse than the refusal: two segments whose halves are
  each not text, and no reader could ever put them back together. Pinned by
  `a_burst_that_fills_the_buffer_splits_between_characters_and_never_inside_one`, which would have
  failed with exactly the error the fuzz run reported.
- **2026-11-19 — D-30 — the gate has a runner in the tree, and the gate is not claimed.**
  Of Block 4's three gates this is the only one that is a test rather than a measurement on
  reference hardware, so it is *approached* with a test: `crates/holonomy-text/tests/keystroke_fuzz.rs`
  drives one seeded generator over a real `Typing` session, a real `ResidentSet` and one shared
  segment store — a world-line stand-in that is a `Vec`, because the seam that matters (the sink
  appends, the source faults in the same bytes) is what `tests/commit_boundary.rs` already proves
  against the real container. The file is honest about the difference between running a gate and
  passing one: **it says in its own module comment that the gate is not claimed**, and ROADMAP's
  checkbox stays empty.
  What it does assert, on every push, over a short slice (six seeds of 20 000 actions, every action
  checked) and over the gate's million keystrokes (four runs of 250 000, `#[ignore]`d, release, run
  by the `test` job):
  (a) **the oracle is a second implementation.** A `String` receives every edit and the document is
  read back through `copy` — the reader the Eye and the shell use — so a leaf, a draft or a
  residency bug cannot pass by agreeing with itself; the leaves are additionally checked to tile
  the document once and to name only segments that exist, with the *span's* extent (a drafted leaf's
  slab keeps the length it was registered with, which is not the length it carries).
  (b) **"zero leaks" is stated as bytes.** The view is the model's, every leaf is backed, and a wipe
  leaves nothing resident; freed heap memory is not observable from a test and is `zeroize`'s claim,
  not this file's.
  (c) **"undo past every point in the log" is the session's retained stack** — `UNDO_DEPTH` 1024
  marks, `ROOT_DEPTH` 64 roots, oldest dropped, and a cut drops the marks above it — unwound state by
  state with every redo required to be the exact inverse of the undo it undoes, against the *settled*
  document: a root recorded before a cut is the tree without whatever was open, because pending bytes
  are in no segment and no root can name them (`restore_root`, OQ-8), so undo across an unsealed cut
  returns to the last *written* document and a seal is how a user keeps what they typed. That clause
  is where the gate currently stops: see OQ-21.
  Two runs, two bugs, which is what a fuzz harness is for: D-31 (fixed here) and OQ-21 (filed, with
  its seed and step).
- **2026-11-19 — D-31 — a seal writes the live bytes, not the buffer behind them (found by the
  gate it was written for).** The first run of D-30's harness failed within a second, on a length
  the session and the independent model disagreed about by exactly the three bytes an undo had
  taken back. `Typing::seal` appended `buffer.as_bytes()` — the whole resident buffer — while the
  document's length comes from the *live* prefix of it; an undo deliberately leaves the bytes it
  took back *behind* that prefix so redo can hand them over without copying anything. A seal is an
  undo floor (`steps` and `undone` are cleared), so the tail can never be redone, and writing it
  appended text that no mark, no root and no leaf could name: the document grew by exactly what
  undo had removed. `seal` now appends `live_len()` bytes and the corrected slab carries that same
  length, so a flush after an undo writes what the document says. Pinned by
  `a_seal_writes_the_live_bytes_and_not_the_undone_tail_behind_them`, and the harness keeps what
  it found the bug with: a failing step now names the action, its offsets and the caret, so the
  next one is a sentence rather than a bisect. Nothing about the *path* changed — a keystroke
  still allocates nothing (`tests/keystroke_allocations.rs`) and a seal still appends once.
- **2026-11-19 — D-30 — Block 4's third gate is a seeded harness in the tree, and its words are
  defined.** Of the block's three gates this is the only one that is a test rather than a
  measurement on reference hardware, so it is claimed with a test:
  `crates/holonomy-text/tests/keystroke_fuzz.rs` drives one seeded generator over a real `Typing`
  session, a real `ResidentSet` and one shared segment store — a world-line stand-in that is a
  `Vec`, because the seam that matters (sink appends, source faults in the same bytes) is what
  `tests/commit_boundary.rs` already proves against the real container. Four runs of 250 000
  keystrokes make the million, and the cuts, marks, undos, wipes and fault-ins between them are
  *extra* actions rather than substitutes for keystrokes. Three decisions live in the file rather
  than in this paragraph:
  (a) **the oracle is a second implementation.** A `String` receives every edit and the document is
  read back through `copy` — the reader the Eye and the shell use — every 64 actions, so a leaf, a
  draft or a residency bug cannot pass by agreeing with itself; the leaves are additionally checked
  to tile the document once and to name only segments that exist.
  (b) **the count is rehearsed, not remembered.** The ignored case is executed by the `test` CI job
  in release on every push and its `HOLONOMY_KEYSTROKE_FUZZ` line is the gate's evidence, so the
  number cannot drift from the tree the way a typed-in measurement can. The default `cargo test`
  runs a shorter slice of the same harness, which is what a sandbox without a toolchain actually
  reads.
  (c) **the words are made smaller than they sound.** "Zero leaks" is asserted over bytes the engine
  can observe — the view is the model's, every leaf is backed, and a wipe leaves nothing resident —
  and never over freed heap memory, which no test can see and `zeroize` owns. "Undo past every
  point in the log" is the session's *retained* stack: `UNDO_DEPTH` 1024 marks, `ROOT_DEPTH` 64
  roots, oldest dropped, and a cut drops the marks above it. Undoing inside the run is checked
  against the set of lengths the session has actually been in; the stack itself is then unwound and
  rewound, with redo required to land on the byte-identical document, which is the half that does
  not trust the harness's idea of what an undo means. Deeper history is the log's, and Block 8's to
  reconstruct.
  Residual, named: the run keeps its document in a 64–512-byte band, which is what bounds the leaf
  count and keeps the million actions a test rather than a benchmark. It therefore proves the
  invariants under churn, not at document scale — gates 1 and 2 still carry that, and stay open.
- **2026-11-19 — D-29 — a rebuilt spine carries the document's shape, not its edit history.**
  OQ-9's load-time half. A cut inside a leaf leaves one segment naming the same plaintext from two
  leaves: content correct, storage shared, but every leaf is a height-model row, a residency slot and
  a read, so a document that has been edited keeps paying for its history forever. The tree cannot
  heal the seam by itself — a `join` does not look at what it joins, and teaching it to would change
  what every structural operation returns, which is a larger claim than this row makes. The load is
  the one place where geometry is *data* rather than history, so the merge happens there.

  **The change.** `Spine::from_slabs` keeps one open slab and folds the next into it when the two
  name contiguous bytes of the same segment; anything else closes the open slab and starts a new
  one. One pass, still O(n), and the signature does not change — the *leaf count* it returns does,
  and that is the point. No other operation moves: `split_at`, `concat`, `insert_slab`,
  `remove_range` and `replace_range` return exactly what they returned yesterday, which is what
  keeps the fragment tests meaningful rather than vacuous.

  **Why merging cannot overflow.** A slab's range fits in `u32` and its constructor asserts it. The
  second slab is contiguous with the first, so the merged range is `second.start + second.len` —
  already valid — and the merge's own `tail.len() + slab.len()` is bounded by it. A different
  segment is a different plaintext and a gap is a range the document does not name, so nothing else
  can move, and the pass only ever looks at the immediately preceding slab.

  **Checked.** `contiguous_fragments_of_one_segment_become_one_leaf` — two fragments plus a second
  segment load as two leaves, with the spans naming the same bytes at the same document offsets;
  `fragments_of_different_segments_or_with_a_gap_stay_apart` — four leaves stay four;
  `a_split_and_rejoin_stays_two_leaves_until_the_document_is_rebuilt` — the residual, pinned: split
  then concat is still two leaves, reloading that geometry is one, and `locate` lands on the same
  byte. Every other suite's fixture names one segment per leaf, so no other assertion should move;
  the CI run is what says so rather than my reading of it.

  **Residual, exactly.** A live session still fragments — a structural insert cuts a leaf and
  nothing rejoins it until the document is written and read back. The self-healing seam is not the
  next step either: merging inside `join` renumbers every leaf after the seam, and `holonomy-shell`
  caches a `HeightModel` keyed by leaf index, so that is a two-crate change with a re-keying story
  to design first. OQ-9 is narrowed, not closed.

- **2026-11-19 — D-28 — GB9c: the conjunct run is state, not a pair.**
  OQ-20, built the day it was filed. UAX #29's GB9c is the rule that makes a Devanagari conjunct one
  caret: `InCB=Consonant [InCB=Extend InCB=Linker]* InCB=Linker [InCB=Extend InCB=Linker]* ×
  InCB=Consonant`. It is the only rule whose right-hand side is not a break class, and the only one
  whose left context is a *run* rather than the previous character.

  **Why it cannot be a property.** `InCB=Consonant` characters are `Other` to every other rule, so
  folding them into `Property` would mean teaching GB3–GB999 a class none of them reads.
  `InCB=Extend` is not a class at all: the standard defines it as `[gcb=Extend gcb=ZWJ] -
  InCB=Linker - InCB=Consonant - U+200C`, a formula over data already here, so it is derived rather
  than stored and cannot disagree with the table it reads. `InCB=Linker` is the awkward one: since
  Unicode 18 it is **not** a subset of Extend — U+1CF5, U+1CF6 and U+11A3A are linkers whose break
  class is something else — so it is data of its own, asked before a break class says anything.
  Hence an `Incb` role asked where GB9c asks it, and nowhere else.

  **The state.** Two flags in `Context`: `conjunct` (the cluster so far ends with
  `Consonant [Extend Linker]*`) and `linked` (that run has seen a linker). Not a count: GB9c asks
  whether a linker is there, not how many, and a second linker changes nothing. They are folded in
  by `absorb` alongside the GB11 and GB12/13 state, which means the conjunct run **travels across a
  leaf cut** for free: `ClusterContext` already carries `Context`, so a window that opens between a
  virama and the consonant it joins to answers as the whole document does. The piece test's corpus
  gained a conjunct for exactly that reason.

  **The rule's root, and where this is stricter than `unicode-segmentation`.** The standard's left
  context begins with `\p{InCB=Consonant}`. `unicode-segmentation`'s scan suppresses the break on
  the first linker it sees, without requiring the run to be rooted in a consonant; this reads the
  rule, so `a` + VIRAMA + SSA is two clusters here and one there.
  `a_conjunct_run_needs_both_a_consonant_and_a_linker` pins ours. The difference cannot arise in
  well-formed Indic text — a virama only follows a consonant there — but a divergence is a thing to
  write down, not to leave to be discovered.

  **The data.** The generator now emits `INCB_CONSONANT` (78 ranges) and `INCB_LINKER` (23 code
  points, read out of the source's `matches!` rather than retyped), and maps `GC_InCB_Consonant` to
  `Other` in the break table as before. D-27 dropped that class with the argument that data no rule
  reads is not behaviour; this is the other half of the same argument, in the direction it always
  allowed: the rule exists now, so the data is carried. Cost, stated: at worst three binary searches
  per character (750, 78 and 23 ranges) instead of one — the conjunct tables are only consulted
  while a character is classified, not per frame.

  **Checked.** `an_indic_conjunct_is_one_cluster` (Devanagari and Bengali, an `InCB=Extend` inside
  the run, and a two-linker chain), `a_conjunct_run_needs_both_a_consonant_and_a_linker` (no
  linker; an Extend alone; a run not rooted in a consonant), `a_zwnj_ends_the_conjunct_run` (ZWNJ is
  that character's whole purpose), the pinned table counts for both new tables, and the piece test's
  corpus. Before the tests ran, the segmentation — GB9c included — was replayed in Python against
  the generated tables: 30 behaviours, the eight conjunct cases and every assertion the older tests
  make, all matching. The Rust is syntax- and width-checked here; CI is the compiler.

  **Residual, exactly.** Nothing in UAX #29 is now unimplemented except BiDi reordering, which OQ-11
  keeps as its own out-of-scope half (paint order; beside the run tree). OQ-20 is closed; OQ-11 is
  narrowed to BiDi and stays open.

- **2026-11-19 — D-27 — the properties are the database's; the rules stay ours.**
  OQ-11's data half, and the last of this module's *silent* gaps. The rules have been UAX #29's since
  D-16, but the property table they read was typed out by hand, a script at a time, so coverage was
  whatever someone had thought of: a combining mark whose script was not on the list was `Other`, it
  split from its base, and the seam became a caret position a renderer does not have. Two kinds of
  mistake live in a segmenter and only one of them has a test. A wrong rule contradicts the standard
  and a test fails. A missing table entry contradicts nothing: it answers confidently, and the answer
  is only wrong for text nobody wrote a case for.

  **The change.** `crates/holonomy-eye/src/cluster/tables.rs` is generated — 750 disjoint
  `(first, last, class)` triples from UCD 18.0.0, one binary search per code point — and `property()`
  is that lookup plus the Hangul syllable arithmetic. `is_control`, `is_regional_indicator`,
  `is_hangul`, `is_prepend`, `is_extend`, `is_spacing_mark` and `is_pictographic` are gone; the last
  was ~120 lines of hand-copied emoji blocks. `scripts/gen-cluster-tables.py` is the generator and
  the record of what its input was. The precomposed syllables stay arithmetic and are *dropped* from
  the table (798 of the database's 1626 ranges), and so is the `InCB=Consonant` class, because a
  class no rule reads is data pretending to be behaviour (OQ-20).

  **The generator's own bug, kept as a guard.** The first pass read 1144 of the 1626 rows and
  *looked* perfectly plausible: sorted, disjoint, and missing U+0300. The table is laid out to a
  width, so a row can break between its literals and its class, and the row reader matched only the
  rows that did not. It now collapses whitespace first and refuses to run unless every class token
  in its input became a row, which is the check that would have caught it.

  **What changed in behaviour, said plainly.** Marks and spacing marks from scripts the old table
  never listed now join their base — pinned on Kharoshthi and Mende Kikakui. Two approximations were
  wrong in the *other* direction and are now right: the soft hyphen and the Arabic letter mark are
  `Cf`, which UAX #29 classes as Control, so they break on both sides instead of joining a
  neighbour. No rule changed, so fifteen of the module's sixteen cluster tests are untouched; the
  sixteenth was the coverage statement itself, rewritten with its reason (`an_unassigned_code_point`
  rather than `an_unlisted_code_point`), not its answer.

  **Checked.** `the_table_is_the_database_and_not_a_sample_of_it` pins the range count per class and
  the total, so a regeneration is a diff somebody reads rather than a table that quietly shrinks;
  `the_table_is_sorted_and_disjoint` pins the shape the binary search depends on;
  `every_class_the_rules_read_has_code_points_in_it` pins a representative for all fourteen classes,
  so no arm of `joins` can be unreachable; `a_mark_from_a_script_the_table_never_listed_joins_its_base`
  is OQ-11's gap, closed; `an_indic_conjunct_is_two_clusters_until_gb9c_is_written` is the residue,
  pinned. There is no toolchain in this sandbox, so before the tests ran the segmentation was
  replayed in Python against the generated ranges — the 22 cluster behaviours the module's tests
  assert, plus every representative — and the answers match. The Rust is syntax-checked and
  width-checked here; CI is the compiler.

  **Residual, exactly.** GB9c (OQ-20) is not implemented, and BiDi stays out of scope for the reason
  OQ-11 gives: it is paint order, so it belongs beside the run tree. OQ-11 is narrowed to those two
  and **not** closed.

- **2026-11-18 — D-26 — the frame ends where the document has a position, not where a leaf does.**
  Closes OQ-19 the short way. The eye laid out a piece of text and could not know what followed it,
  so the piece's end was offered as a caret whether or not the document had a position there (D-23
  corrected the click instead). Teaching the eye a second fact about a piece — that the document
  continues inside its last cluster — is a parameter on `layout_styled_continuing`, a `Layout` field
  and its mirror flag on the last line, and a read on every frame to learn the answer. The caller
  can do the smaller thing: **stop handing the eye a piece that ends inside a cluster.**

  **The change.** `frame()` reads one leaf past the window's end (the leaf a read's edge has to stop
  on, because only a leaf boundary is both a byte count and a character boundary), lets the window's
  text reach that far, and asks `tail_bytes` how many of those bytes belong to the cluster the cut
  opened. The window's end grows by that much and the layout sees exactly `start..end` — the tail is
  painted with the line it belongs to, the height model already charges that line to the leaf it
  starts in, and `Line::boundaries` then offers only positions the document has. `hit_test` keeps
  `legal_caret` (D-23) for what the frame cannot finish.

  **What this costs, stated.** One extra leaf faulted per frame, and only when the document has a
  leaf after the window at all: it is the leaf the next frame reads anyway, and the resident set
  holds it, so the counted property OQ-12 records is unchanged — a screen's leaves, whatever the
  document's length. The bytes handed to layout are still one screen plus the leaves already allowed.

  **Residual, exactly.** A cluster longer than the leaf after the cut cannot be finished without a
  longer look (D-25's bound, `CLUSTER_LEAF_LIMIT`'s spirit): the frame ends inside it and
  `legal_caret` moves the click to where the cluster begins. So the eye's own `View::hit_test` still
  offers a truncated piece's end to a caller that hands it one — OQ-19 is closed *for this app*,
  which is the only caller there is, and the route to closing it for good is the piece flag above,
  filed rather than built.

  **Checked.** `a_frame_ends_where_the_document_has_a_position` — the every-leaf-boundary-inside-a-
  cluster text: the frame ends past the cut (not on a multiple of 64), the end is a cluster start,
  and every boundary the frame offers round-trips through a click with nothing corrected.
  `a_cluster_too_long_for_the_look_is_still_not_a_caret` — `e` + 400 marks: the frame stops at 320
  inside the cluster, and the click lands on 255, the `e` where the cluster begins. D-23's
  `a_click_cannot_put_the_caret_inside_the_cluster_the_window_cut_open` became the first of those
  two by rewrite, since the frame no longer offers the boundary it was written against.

- **2026-11-18 — D-25 — one cluster, one advance: the leaf a cluster starts in places it whole.**
  OQ-18's fix, and the same mechanism OQ-19 is waiting on. A leaf cut is a byte count, so a cluster
  can straddle one; the shell laid each leaf out on its own, so the cluster's advance was paid
  twice — once for the bytes in the leaf before the cut, once for the rest — and at a wrap that
  could add a line the document does not have.

  **The mechanism.** The leaf the cluster *starts* in places it whole: `cluster_tail` asks whether
  the byte after the leaf would join what is already there (the segmenter's own answer, from the
  leaf's text alone), looks at the leaf after it whole, and if the cluster ends inside that look,
  hands its bytes back as a tail. The leaf lays the tail out with the rest and records how many
  bytes it placed in the carry (`Carry::skip`), so the next leaf's geometry begins *after* them:
  `leaf_text` drops them, and the next leaf's `x` and segmentation state are the ones at the point
  it begins. A leaf whose every byte was a tail shapes nothing and hands the position on.

  **Why the look is a leaf, whole.** A range edge has to be a character boundary as well as a byte
  count, and a leaf boundary is the only one that is both — a seal cuts on a character, so leaf
  edges are character edges, but a fixed byte count past one is not. The look is therefore the leaf
  after the cut, taken only when the cut opened a cluster, and it is the leaf the walk reads next
  anyway: the resident set holds it, so `reflow` faults each leaf once, not twice. The frame path
  takes no look at all — its window is already text — and computes the same skips from it, which is
  what makes the two measurements agree (`measure_window` now derives each leaf's skip and the
  state its geometry begins in, from one pass over the window's clusters).

  **Residual, stated.** A cluster longer than the leaf after the cut is not completed: the double
  advance stands for that case, and the audit says so rather than pretending a bound does not
  exist. `CLUSTER_LEAF_LIMIT` still bounds the walks.

  **Checked.** `a_cluster_split_by_a_leaf_costs_its_advance_once` — 63 `a`, `e`, U+0301 at a 64-unit
  wrap and a 64-byte leaf: one line (20 units where the old code measured 40), the leaf walk and a
  frame agreeing, and two leaves to disagree in. `a_backspace_crosses_two_leaf_boundaries_to_take_its_cluster`
  still takes 81 bytes across three leaves with the skip in place, and the click test still refuses
  the caret the cut opened.

- **2026-11-18 — D-24 — the carries a frame wrote were a leaf late.** Found while re-reading the
  carry path to build D-25, and introduced by D-22 three commits earlier. `measure_window` derived
  each leaf's carry from that leaf's **own** end: the segmentation state after folding in the leaf's
  text (`contexts[index]`), and the `after_cr` flag from the leaf's last byte. A `Carry` is the
  carry a leaf is **measured with** — `reflow` stores it that way, `Measured`'s own doc says so — so
  every leaf in a frame later got its successor's state.

  **Failure scenario.** `a`×63, `e`, two U+0301, `\n`, then a leaf of `a`×58 and `\n`. Leaf 0 ends
  on the `e`, leaf 1 opens with the marks. A click runs a frame, which writes both carries back;
  `Backspace` at 68 then asked `cluster_before`, got the state at leaf 1's **end** (a `\n`, so GB4
  breaks), read the marks as a cluster of their own, and stopped at 64 — four bytes instead of five,
  half a grapheme left behind. That is D-22's symptom, reached from the other direction. The same
  off-by-one moved `after_cr` onto the wrong leaf, which is D-21's CRLF rule one leaf out for
  `line_start` and `caret_place`.

  **The fix.** Record the context *before* folding the leaf's text in, and read `after_cr` from the
  byte before the leaf's **start** — the window's own start has none and needs none, because it is
  a line start.

  **Checked.** `a_frame_hands_each_leaf_the_state_from_before_it` clicks (which runs a frame, then
  the carries are in play) and asserts the backspace takes `e` + two marks: 123 bytes. The old code
  answered 124.

- **2026-11-18 — D-23 — a frame's end is the one boundary a frame can offer that the document may
  not have.** OQ-19's shell half, found by asking what D-22 left on the *other* side of a piece. A
  window ends at a leaf, a leaf cut is a byte count, so the cut can fall inside a cluster — and the
  frame's last line ends there, offering that byte as a caret. The eye is right to offer every
  boundary of the text it was given; it simply cannot know that the text it was given is a *prefix*
  of a cluster. Every other boundary a frame offers is a cluster start of the frame's own text, and
  that text begins at a line start (`choose_window` asks `line_start`), so the end is the *only*
  one in question — which is what makes the fix small.

  **Failure scenario.** A text whose every 64-byte block is `mark a×(61) e`, so every leaf boundary
  falls inside an `e` + U+0301 cluster. A click at the frame's last boundary — the leaf the window
  stops at — moved the caret to that byte (128 in the fixture), which is inside a grapheme; typing
  there split it and left the document half a cluster long. `Backspace` from it then took the mark
  without its base.

  **The fix.** `crates/holonomy-shell/src/lib.rs` `hit_test` asks the document about that one byte:
  `legal_caret` reads the cluster containing the byte *before* it (`cluster_before`) and asks where
  that cluster ends (`cluster_after`); if it ends past the byte, the position the document has is
  where the cluster begins. The caret is then re-placed **in the frame that drew it**
  (`View::paint_point`), so the box a click reports is the box paint draws — L-8 rather than a
  claim about it. The cluster hops are the ones D-22 already bounds (`CLUSTER_LEAF_LIMIT`), and
  only a click that lands on the frame's end pays for them.

  **Checked.** `a_click_cannot_put_the_caret_inside_the_cluster_the_window_cut_open` clicks every
  boundary the frame offers — not a sample — and asserts each caret is a cluster start of the
  document (or its end), and that the frame's end lands on the cluster before the cut. On the old
  code the same click landed on 128, which `Clusters::new` says is inside a cluster.

  **Not fixed, and filed.** The eye still offers a truncated piece's end to any other caller
  (OQ-19): to fix it there, `layout_styled_continuing` has to be told a second thing about the
  piece — that the document continues inside its last cluster — which the caller can only know by
  reading the byte after the window. That is a two-crate API change and a read added to every
  frame, so it stays a row rather than a patch.

- **2026-11-18 — D-22 — the cluster carry: a window that opens inside a cluster stops inventing a
  caret.** OQ-17's fix. The geometry's answer depended on where a leaf ended, which is the one thing
  a document's geometry may never do: a leaf cut is a byte count, so a window can open on a
  combining mark whose base is in the window before, and the piece was segmented **from its own
  first byte**. Two symptoms followed. A caret position the document does not have appeared at the
  window's first byte — L-8's exact subject — and `backspace`/`delete` stopped at the leaf edge,
  taking half a grapheme, which D-21 fixed one leaf deep and left the rest to this.

  **The state is two words, not text.** UAX #29 needs only the property of the last character
  consumed (the left context GB3–GB13 read), whether the run behind a ZWJ is `Pictographic Extend*`
  (GB11), and the regional-indicator count within the cluster (GB12/GB13). `ClusterContext` is
  exactly that, `Clusters::carry()` is the state at the end of a piece, `Clusters::continuing()` is
  the state at the start of the next — so a piece can begin inside a cluster and still segment as
  the document does. `Layout` carries it out (`Layout::carry`) and takes it in
  (`layout_styled_continuing`), and a first line whose cluster began earlier **does not offer its
  first byte as a caret** (`Line::boundaries` skips it, reading a private flag on that first line).

  **What it buys, exactly.** `a_piece_that_continues_a_cluster_segments_as_the_whole_text_does`
  cuts a text with a mark, a joiner chain and a flag at *every* character boundary and checks that
  the piece's first cluster ends where the document's does;
  `a_window_that_opens_inside_a_cluster_offers_no_boundary_there` pins the caret claim in both
  directions (with the state, byte 1 is not offered; without it, it is). The shell's
  `cluster_before`/`cluster_after` now follow a cluster across leaves — up to eight, then stop,
  because a document of nothing but marks must not make one backspace a walk of the document — so
  `a_backspace_crosses_two_leaf_boundaries_to_take_its_cluster` takes a base and forty marks that
  span three leaves, where D-21's one-leaf context stopped at the second.

  **What it does not buy, measured rather than claimed.** A cluster's *advance* is exact only when
  the cluster is shaped whole: the eye shapes cluster by cluster, so a cluster split by a cut pays
  its advance on both sides and can gain the next leaf a line. That is OQ-18, opened by this entry
  and pinned by `a_cluster_split_by_a_leaf_costs_its_advance_twice` — a test that asserts the
  residual (40 units where the document has 20) precisely so that fixing it has to change a number
  rather than slip past.

  **Why this was not part of D-21.** It changes the Eye's API (`Clusters::continuing`, `ClusterContext`,
  `Layout::carry`, `layout_styled_continuing`, `Line::continues_cluster`) and the shell's carry at
  the same time, and a half-done version — state without the boundary filter, or a filter without
  the state — is worse than the bound D-21 stated.

- **2026-11-18 — D-21 — a hunt across the tree: four defects, three fixed, one filed.** The sweep
  was over boundary arithmetic — every `+ 1`, every `- 1`, every "and then the next leaf" — because
  that is where this codebase's bugs have all been, and it found four. Three are fixed here with a
  test that fails without the fix; the fourth is OQ-17, filed with its design rather than
  half-built.

  **1. GB11 let a second ZWJ reach across itself** (`holonomy-eye/src/cluster.rs`). The rule is
  `Pictographic Extend* ZWJ × Pictographic`, and the implementation kept the pictographic flag alive
  *through* the joiner — which is what makes `👨‍👩‍👧` one cluster, and also what made `👨‍‍👩` one when
  the standard says two: a second ZWJ's left context is a ZWJ, which is not `Extend*`. The flag now
  remembers the run *behind* the joiner and the joiner ends it.
  `a_second_zwj_ends_the_pictographic_run` pins both directions — the family sequence still joins,
  the double joiner does not.

  **2. A `\r\n` split by a leaf cut was charged two lines** (`holonomy-shell`). The Eye treats CRLF
  as **one** break, and a leaf cut is a byte count — so a hundred-byte paste of `abc\r\n` into
  sixty-four-byte leaves puts the twenty-first break's CR at the last byte of one leaf and its LF at
  the first byte of the next. Laid out apart, the LF started a line the document does not have: a
  phantom blank line, and twenty units of scroll for a twenty-one-line document. The carry now says
  whether the predecessor ended on a bare CR, and a leaf that did drops that LF before it is laid
  out. `a_crlf_split_across_a_leaf_is_one_break` compares the measured total against one continuous
  layout rather than against arithmetic.

  **3. `resize` left the scroll past the end** (`holonomy-shell`). The maximum scroll is
  `total − viewport`, a function of the viewport — so making the window *taller* could leave the
  position past the end, with the document's last line at the top of an empty screen and no way back
  except scrolling up. A width change already emptied the geometry; a viewport change did nothing.
  The clamp lives in one place, so the fix asks that place again.
  `a_taller_viewport_pulls_the_scroll_back_inside_the_document` fails with the old `resize`.

  **4. Backspace and delete stopped at the leaf boundary** (`holonomy-shell`). Same cause as (2):
  a leaf is cut on a byte count, so a combining mark or a joiner can be the *first* byte of a leaf
  with its base at the *last* byte of the one before. `cluster_before` read only the caret's leaf and
  asked the segmentation where the last cluster began — which was the mark, so backspace deleted half
  a grapheme and left a bare base behind; `cluster_after` had the mirror bug on delete. Both now read
  one leaf of left (respectively right) context, and the statement of the bound is in OQ-17: a
  cluster that crosses **two** boundaries is still split.

  **What the sweep checked and did not fault.** The Fenwick index (`set`, `push`, `prefix`,
  `leaf_at`, `window`) against its rebuild-comparison tests; the resident set's budget arithmetic
  (`retain`, `ensure`, admission against allocated capacity); `select::copy`'s offset derivation,
  which is the one place a leaf's segment offset and its document offset are mixed; the MMR's peak
  carry and its `from_peak_bytes` round trip; the seal's nonce draw (one `getrandom` per seal, no
  counter to reuse) and its CMT-4 commitment. Each of those has a test that can fail for the reason
  it exists, which is why "no finding" is worth writing down rather than silence.

- **2026-11-18 — D-20 — a leaf boundary is not a line boundary.** D-19 said the geometry was a
  document's, and it was not: it was the leaves'. Two bugs had the same cause — the wrap state at a
  leaf boundary, which is two numbers (where the next cluster would go, and whether the line it lands
  on began earlier), was thrown away.

  **The scroll bar was wrong.** `reflow` laid each leaf out *alone*, which measures a leaf's own
  layout and not the document's. A paragraph split across 64 leaves counted 64 lines where the
  document has 21: **1,280 units of scroll for a 420-unit document**, and the error grew with the
  number of boundaries a paragraph crossed. That was OQ-16, recorded as a standing tax.

  **The screen was wrong, and nobody had said so.** A frame began at the *leaf* the scroll position
  fell in. A line can start inside a leaf — in a 4,000-leaf document, line 19 starts at byte 3,800,
  which is 24 bytes into leaf 59 — so the frame re-wrapped the paragraph from a point the document
  never breaks at. Paint and hit-test still agreed with each other (L-8 holds for the frame), and
  every cluster on screen was drawn where the *frame* says, which is not where the document says.
  The first bug made the second visible; the second was the serious one, and it had never been
  written down.

  **The fix is one idea in two places.** `layout_styled_from` lays text out as the continuation of a
  line that is already `start_x` wide, wrapping before its first cluster if that cluster does not
  fit, and marking the first line as inherited. The shell carries `start_x` from leaf to leaf in
  `reflow`, and stores it with each leaf's height in one `Measured` value — the two have the same
  lifetime, because both describe a wrap the document no longer has after an edit or a resize. A
  frame then asks the index which leaf the scroll position falls in, lays *that* leaf out from its
  carry, and begins at the byte where the position's line starts.

  **What is measured, and the test that could fail.** `reflow`'s total now equals one continuous
  layout of the same text — 420 units for a 4,096-letter paragraph in 64 leaves, and 20,500 for 1,024
  hard breaks in 64 leaves — and the test computes both by laying the whole document out once, which
  is the one thing the shell may never do. A frame at y = 380 begins at byte 3,800 with a full line
  of 200 clusters, and the height it writes back leaves every leaf it only began inside alone.
  `cargo test` proves the partition; nothing here needs a font.

  **The third bug, found on the way.** `reveal_caret` read a *frame*-relative caret top as a
  document coordinate, and could only see a caret the frame already held — which is the case a scroll
  is not needed for. It scrolled to the top of the document to show a caret one line above the scroll
  position, and did nothing when the caret was far below. It now asks the document: binary-search the
  leaf, lay it out with its carry, and add the leaf's measured top. `revealing_the_caret_scrolls_...`
  pins the smallest scroll in three cases, including a caret that no frame holds.

  **What is still owed.** A line crossing the *window's* end is charged the height it has inside the
  window, which is exact while every line is the same height — true of the shell, which lays out
  plain text, and not true of a styled document, which would have to carry the running line height
  and correct the charge when the line ends. The residency story is unchanged: `reflow` still holds
  one faulted leaf at a time, and a carry is two words, not text.

- **2026-11-18 — D-19 — the loop exists, and the caret check OQ-15 asked for passes.** Block 5 ended
  with a chain of parts that each had a test and none had a caller: an index that knew where the
  viewport was, a join that turned leaves into bytes, a reader that turned bytes into text, and a run
  tree both consumers walked. `holonomy-shell` is the loop that closes the ring — intents in, a
  frame out — and the first thing it proves is that the chain's *edges* agree.

  **Closes OQ-15's test, not its asymmetry.** OQ-15 said the missing check was "every caret the Eye
  can produce is a character boundary of the document, and that needs the reader and the edit in one
  place". They are in one place now, and the check runs over every caret every frame offers — the
  cluster-honest corpus (a combining mark, a wide glyph, a three-person ZWJ sequence) at every
  scroll position, asserting both that the byte is a boundary of the frame's text and that it
  hit-tests back to itself. It passes. What it does **not** do is make a mid-character cut
  impossible: `Typing::insert_at` still takes any offset a caller offers, so a caller that bypasses
  the Eye can still ask for one — and the reader now *refuses* the result instead of handing it to a
  renderer (`WindowTextError::NotText`, D-18). Prevention-by-construction, detection-at-read, and no
  third thing claimed.

  **Two measurement conventions, stated because they are choices.**
  1. A leaf's height is the height of the **lines that start in it**. That partitions a layout
     exactly — every line starts once, so the heights sum to the layout's height to the 64th — and it
     is the only attribution that stays exact when a line straddles two leaves. The cost is that a
     line spilling into the next leaf is charged to the leaf it began in, which is exactly the leaf a
     scroll position must resolve to.
  2. Measuring a whole document (`reflow`) lays each leaf out **alone**, one faulted leaf at a time
     and never more. That is what keeps the constitutional clause true while measuring 4,000 leaves —
     and in the first draft it also introduced OQ-16: a paragraph that would have continued across a
     leaf boundary was charged the line it breaks there. **D-20 is the rest of that sentence**: the
     leaf is laid out alone, but the *line* is carried across the boundary, so convention 1 is now
     true of `reflow` as well as of frames, and OQ-16 is closed.

  **What the loop measures, in counts.** On a 4,000-leaf document: reflow faults each leaf exactly
  once and the resident set never holds more than its budget; a frame halfway down shapes 384
  clusters (two lines of 200 units) and reads the index at most a couple of hundred times, for four
  thousand leaves; and a keystroke at the document's end appends one leaf —
  `HeightModel::push`, O(log n) — instead of rebuilding the index, so no measured height is
  invalidated by typing. A **cut** cannot do that: it moves every offset after it, so the index is
  emptied and the next frame re-measures what it shows. That asymmetry is the honest one.

  **What is still owed, and it is the same thing it was.** No font. The shaper is the trait the
  shell is generic over, and the fake stands where Block 8's pipeline will stand, so gate 2's clock
  is unclaimed and every number above is a count rather than a second. Block 6's own gates — the
  budget table on reference hardware, kill-power recovery, the hand-off drill, zero idle repaint —
  are untouched by this and by construction, because there is no window here to be idle.

- **2026-11-18 — D-18 — the Eye reads its window, and the reader is where OQ-15 stops being an
  assumption.** Block 5's chain had a hole in the middle: `byte_window` said *which bytes* are on
  screen and `layout_styled` shaped *some text*, and nothing in the tree joined them — the Eye had
  no reader, so the window was a number in a test and never text. That hole is now `window_text`.

  **It is a join, not a reader.** The document already had one reader — `select::copy`, D-13 — and
  it is the only one that resolves a leaf's source through the register. A second reader in the Eye
  would be a second place that decides whether a leaf is a buffer or a segment, which is exactly
  the bug D-13 was found by (reading `span.slab().segment()` directly asks the world-line for a
  provisional id). So `window_text` hands its byte range to that reader as a selection and decodes
  what comes back. The Eye obeys the constitutional clause by construction: it can only hold text
  that a window covers.

  **What is measured.** A three-leaf screen faults three segments, on a document of eight leaves
  and on one of four thousand and ninety-six — the same count at 512× the length, which is the
  input half of gate 2 stated as a number rather than as a complexity class. Reading the same
  screen twice faults nothing new (the resident set answers, the world-line is not asked), and an
  open leaf is read out of the pending buffer, so typing is visible to the Eye without the Eye
  knowing that buffers exist. What is **not** measured is time: the shaper is still the fake one,
  and a fake would make the number about the fake. Gate 2 stays unclaimed, with its caller built.

  **Two edges, stated rather than smooth.**
  1. A window is assembled into an owned `String`, one per frame, because a viewport is contiguous
     in the document and not in memory. That allocation is the paint path's, not the keystroke
     path's — the keystroke path is still measured allocation-free — and its size is the window's,
     which is what keeps the law true while making it a copy.
  2. A window whose edge falls inside a character is **refused** with the offset of the offending
     byte. OQ-15 said the sealed side cannot check where a cut lands; the reader cannot prevent
     the cut, but it is the first place that can *detect* one, and refusing is the only honest
     answer: half a character is not text, and handing it to a renderer would be mojibake with a
     caret in it.

- **2026-11-18 — D-17 — a history step records a state that can be reproduced, not the state as it
  stands.** Found by hunting for bugs in the paths the next slice stands on, and it is one family
  with two members: both directions of the history were recording something that could not be
  reproduced, and both failures are silent — the document stays structurally valid and reads wrong.

  **The length had no position.** `Mark::Live` recorded how long the open leaf was and not where it
  was, which is enough while the leaf is open and wrong the moment undo takes it out of the tree.
  Undo of a mark whose source the *action* had opened removes the leaf and leaves the caret at the
  mark's offset; redo then re-attached the text at that caret, because `next_at` was carrying two
  meanings — where the caret goes, and where the leaf goes. Typing `XY` at offset 1 in `abcdef`,
  undoing and redoing gave `adefXY` instead of `aXYdef`: the same bytes, a different document, no
  error anywhere. A mark now records `at` as well as `len`, and only the offset is used for
  placement.

  **The root was recorded over bytes it was about to throw away.** A root step is a pointer, and
  undo of one discards whatever is open — safely, because a seal ends the epoch, so anything still
  open was never written (D-15). But the *redo* step was built from the live tree, which still had
  the open leaf in it. `delete`, type, undo, redo therefore restored a spine whose leaf named a
  source that undo had abandoned and whose bytes were gone: `document_len` counted two bytes no
  reader could produce, and the next keystroke would have split that phantom leaf and left it in
  the document forever. The fix is `settled_spine`: a root step records the tree without the open
  leaf, which is the state undo can actually come back to. `leave` is now the single place undo and
  redo compute what they are leaving behind, so the two directions cannot disagree again.

  **Why this class slips through.** Every assertion in the suite asked about lengths, leaf counts
  and written bytes — all of them true in both failures. What catches it is asking the *register*
  whether each leaf names a source with text behind it, and asking the spanning geometry whether a
  re-inserted leaf is where it was. Two tests do exactly that, and neither would have been written
  from the code's own docs, because the docs described the intent correctly.

- **2026-11-18 — D-16 — segmentation is the rules, and the tables are the residue.** OQ-11 said the
  Eye's cluster segmentation was a hand-written *approximation* — and it was, in the way that matters
  least and in one way that matters most. The old loop joined **anything** after a ZWJ, which is GB9
  (× ZWJ) read as if it were GB11: `a` + ZWJ + `b` came out as one cluster, where UAX #29 gives two.
  That is the direction an approximation must not err in. Being *conservative* would mean joining too
  little, so that a caret is refused a position a renderer allows; joining too much means a caret is
  **refused a position between two glyphs the renderer draws apart** — and the caret that is missing
  is the one a user needs to type there.

  **The shape now is the standard's.** Each code point is classified into a `Property` — CR, LF,
  Control, Extend, ZWJ, Regional_Indicator, Prepend, SpacingMark, Hangul L/V/T/LV/LVT, Extended_
  Pictographic, Other — and one function, `joins`, applies GB3 through GB13 **in their numbered
  order**, each rule's comment naming the rule it is. Order is the whole content of the algorithm:
  GB4/GB5 (break on both sides of a control) must be asked before GB9 (don't break before Extend),
  and GB11 (a ZWJ joins pictographic sides) only after GB9 has been given its chance. The state a
  rule needs beyond its neighbours — whether the cluster so far has seen Extended_Pictographic, and
  how many Regional_Indicators it has — lives in a `Context` the iterator carries and the joiner
  updates, which is what makes GB11 and GB12/13 decisions about the *cluster*, not about the pair.

  **What is earned, and what is not.** Hangul now composes by GB6–GB8 including the LV/LVT arithmetic
  (a jamo after a syllable becomes a T only if the syllable is LV or LVT); Prepend characters attach
  forward; SpacingMark stays with its base; controls break on both sides — including the CRLF that
  stays one, which is the case that makes "break on both sides" a rule pair rather than a rule. The
  **tables** are hand-written and that is the remaining gap, stated in the crate's own docs rather
  than here alone: they cover the scripts `docs/UI-SPEC.md` §8 names, general diacritics, variation
  selectors, tag characters, joiners, and the emoji blocks. A mark outside that coverage splits from
  its base — a coverage gap, and one whose direction is now *known* instead of assumed, because the
  rule that would have joined it is implemented and merely lacks the property.

  **BiDi is still absent, deliberately.** OQ-11 bundled two things; this takes the half that is
  segmentation. Reordering is not a segmentation question at all — it changes the order runs are
  *painted* in, so it belongs beside the run tree and the caret's visual order (L-8's "at every
  zoom and DPI" says nothing about direction, but the third Block 5 gate does). Doing it here would
  put a second coordinate system in the module that decides byte boundaries, which is the shape of
  the drift this crate exists to prevent.

  **L-8 stays `spec`.** The rules being the standard's does not make the *pipeline* real: the shaper
  is a fake, the tables have coverage ends, and BiDi has no answer. What changed is which of those a
  reader is told about, and that one of them is no longer a misread rule.

- **2026-11-17 — D-15 — undo of a deletion is a root, and the asymmetry is the design.**
  D-14 made typing undoable without keeping a byte. This closes the other half — the actions that
  *destroy* bytes — and it closes it cheaply for a reason worth stating plainly: **the bytes a cut
  removes are still in the world-line.** So undo does not have to remember them; it has to remember
  the tree that named them. A root is a pointer, and naming it again restores the text without
  decrypting anything (OQ-14, arriving from the other side).

  **The asymmetry, stated as the rule.** A length undoes typing; a root undoes destruction. They are
  not interchangeable, and the session keeps them in one stack of `Step`s so undo is a single
  gesture: `Typing::delete` seals what is open, records the root, then cuts; `Typing::mark` records a
  length.

  **The invariant that keeps it sound.** A root recorded *before* an append would orphan the segment
  that append wrote — the exact thing `commit_slab` refuses at the commit boundary. So a seal ends
  the epoch for both halves: any seal drops every step. Undoing a root is therefore only ever a swap
  between sealed states, and a test seals once after a cut to prove the step is gone.

  **Undo across pending typing discards it.** If the user types after a cut and then undoes, the
  pending bytes are dropped and the root restored. That is safe *because* of the rule above: a seal
  would have dropped the step, so anything still open was never written, and nothing in the log is
  lost. The pending text is undo's, and undo takes it.

  **A replacement is finally one action.** `Typing::replace` = delete + insert; the delete records the
  root, the insert's bytes stay pending, and one undo puts the old text back with the caret where the
  selection began. That is the promise made two slices ago, now true and tested.

  **OQ-8, closed for the session.** Two rings: 1024 lengths and 64 roots. History memory is
  `1024 words + 64 × O(log leaves)` nodes, independent of the number of edits; a test makes 74 cuts
  and shows the depth stays at 64 while the document keeps changing. What the bound gives up is depth,
  not correctness — and deeper history is already on disk, because the world-line is append-only, so
  "reconstruct a root from the log" is Block 8's work rather than a new question. The row now says
  that instead of "open".

  **Two expectations caught before CI this time.** A seal needs the *ninth* byte into an eight-byte
  buffer, not the eighth — the buffer-full check is exact, so the seal is what the next byte causes —
  and one undo moves back one cut, not sixty-four. Both were my arithmetic, both were fixed by
  reading the engine rather than by a failed run, which is the cheaper order.

- **2026-11-17 — D-14 — undo of typing is a length, and that is the whole trick.**
  Undo usually means keeping the states you might return to, which in this architecture means keeping
  plaintext — the one thing the lib.rs clause forbids ("undo is a pointer walk, never a plaintext
  buffer — otherwise the wipe set would grow with history"). The way out was already in the design:
  an open source's text is one buffer, and the overlay is the authority on how much of it the
  document includes.

  **The claim.** A mark records a **length**. Undo lowers the overlay's length; redo raises it. The
  bytes never move, so a mark is a `usize`, undo and redo copy no root, write no byte and allocate
  nothing, and redo works because nothing was thrown away — the tail simply stops being part of the
  document. `undo_depth()` is a number a test can read; the assertions that matter are that the spine
  still says five while the overlay says eleven and back again, and that `sealed_bytes` is the same
  before the undo and after the redo.

  **Undoing the first byte is the one case that costs a root.** With nothing open at mark time, the
  action opened a source; undoing it removes the leaf and abandons the source — a tree may not carry
  a leaf that names nothing (D-11's rule, arriving from the other side). The bytes stay in the
  buffer, so redo re-attaches them under a *new* provisional id: the abandoned one is never reused,
  and the document's text is identical.

  **What undo cannot do, and why the honesty is structural.** A mark promises that the bytes before
  its length are unchanged. A delete inside the buffer breaks that promise and pops the marks above
  its cut; a seal breaks all of them, because the bytes move to the world-line and the buffer is
  dropped. So typing is undoable for free and destruction is not undoable at all yet — and the fix
  for the second is not "keep the bytes" (forbidden) but "keep the root": undo of a deletion is a
  pointer swap, and it is *cheap* precisely because deletion does not erase (OQ-14). The property
  D-11 recorded as a cost is what makes the next slice possible.

  **OQ-8, answered for typing.** The stack is capped at `UNDO_DEPTH` marks and a seal ends the epoch,
  so history's memory is 1024 words plus one buffer — it does not grow with the number of edits, and
  a test types 1074 times to prove the cap holds while the document keeps growing. The destructive
  half (a ring of roots, or reconstruction from the log) stays open, and OQ-8's row now says exactly
  which half is which rather than pretending the question is closed.

  **Also landed:** `Typing::replace`, which is `delete` then `insert_at` — one call, one caret rule,
  and a test that a replacement inside the buffer writes no byte.

- **2026-11-17 — D-13 — a selection costs the bytes it covers, and nothing else.**
  Selection turned out not to be storage at all: two offsets — the anchor where the gesture began and
  the focus where the caret is — plus a normalisation for the right-to-left case and a clamp to the
  document. What it *is* is the last consumer the engine was missing, and it wants two different
  halves of the same range.

  **One resolution, two consumers.** A painter wants the leaves a range covers with live offsets;
  that is `Selection::spans`, which is `Draft::spans_in` with the clamping done, so a leaf being
  typed into is reported at the length it has *now*. The clipboard wants the bytes; that is
  `select::copy`, which walks the same spans and reads exactly the part of each leaf the selection
  covers. Two consumers, one geometry — the Block 5 clause applied to selection instead of to paint.

  **The measured claim.** Copying two bytes out of the middle of an eight-byte sealed leaf returns
  two bytes and faults in one segment — through the register, and re-asserted by a regression test
  that pins the provisional id to segment `0` before copying anything. Copying across a seal returns the sealed fragment then the
  buffer, faults only the sealed leaf, and faults **nothing** when the selection lies entirely inside
  the open buffer. The residency set is unchanged by a copy that touches no sealed byte, which is the
  only way that test can be written and still mean something.

  **What a copy does grow.** The returned buffer — the clipboard's payload — is O(selection); that is
  inherent, since the bytes have to exist somewhere. What does not grow is residency: leaves are read
  one at a time and copied out immediately, so the resident set stays at its budget while a large
  selection moves through it. The bound that changes is the clipboard's, not L-2's, and the shell
  owns it.

  **The bug the tests found — the register is the only authority.** The first version faulted in
  `span.slab().segment()`, which is a **provisional** id even after a seal, because sealing renames the
  source and not the leaf (D-7's whole point). Five tests failed at once asking the world-line for
  segment `9223372036854775808`. The fix is not a guard but a correction: `copy` takes the register
  and asks `OpenSources::locate`, which is the single place that knows whether an id is a buffer or a
  segment. This is the crate's **first reader**, so it is the first code that had to know, and the
  note belongs on the record for the second: the Eye's residency path will face the same question,
  and faulting a leaf's own segment id will be wrong there for the same reason.

  **Three refusals instead of three guesses.** A drafted leaf whose bytes are *not* the session's
  buffer — the shape the commit boundary records against resident text edited in place — returns
  `DraftNotInTheSession` rather than reading whatever happens to be at that index; a leaf that claims
  more bytes than its segment holds returns `SegmentTooShort` rather than truncating; and a leaf
  naming a source the register never handed out returns `UnknownSource` rather than reading the
  buffer or the world-line. All three are unreachable in a correct session and all three would be
  silent corruption if guessed at.

  **What is still not decided.** The operating system's clipboard is per-platform work — flag
  confidential, clear on blur — and stays open as OQ-3. And *typing replaces the selection* composes
  from what exists (`Typing::delete` then `insert`, caret at the range's start), but the shell has to
  present it as one user action so that undo sees one entry; that is the next slice, and it is where
  OQ-8's unbounded history stops being deferrable.

- **2026-11-17 — D-12 — a bug hunt on deletion: two findings, one of them a trap for undo.**
  Deletion is small enough to read in one sitting, so it was re-read as an adversary rather than as
  its author.

  **What held.** Clamping past the end, an empty range, deleting after a caret move with nothing
  typed, deleting past the open leaf into its sealed tail, and deleting the whole buffer twice in a
  row all behave as documented, and no path lost text, wrote a byte twice, or left the caret
  anywhere but `start`. The fragments were the one claim the tests asserted only by *count*, so the
  sealed-deletion test now reads the leaves back and checks their inner offsets: keeping `0..2` of
  `01234567` and `12..16` of `89abcdef` must leave inner starts of `2` and `4`. It does.

  **Finding 1 — `close` destroyed the evidence.** `OpenSources::close` removed the provisional id
  from the open list, so `locate` answered `Unknown` — the same answer a stranger id gets. The two
  are not the same fact: one id named text that was typed and deleted *before it was ever written*,
  the other was never minted. Nothing in today's tree can tell them apart because no root holds the
  abandoned leaf; the moment `History` offers undo across a whole-buffer deletion, a resurrected root
  does, and it names text that is unrecoverable — never sealed, never in the world-line. Fixed by
  deriving rather than storing: provisional ids are minted by counting, so
  `OpenSources::was_abandoned` answers from the counter and cannot drift from it.

  **Finding 2 — the boundary check is asymmetric** (OQ-15). `SlabText::delete` refuses an offset
  inside a character; the spine path accepts one, because this session holds no reader. The
  asymmetry is now stated in both methods' documentation and pinned by a test that deletes half of
  an `é` in the buffer (refused) and half of it sealed (accepted), so a future fix cannot land
  quietly.

  **What this says about the record.** Neither finding was a crash; both were places where the code
  was *almost* honest. The hunt did not find a bug in the deletion path — it found that the path
  leaves two facts unrecorded, and the second one only becomes a bug in the presence of undo.

- **2026-11-17 — D-11 — deletion is a pointer operation, and that word has to be chosen carefully.**
  Removing text turned out to be the smallest structural change in the engine and the largest
  semantic one.

  **What it costs.** Backspace inside the open buffer edits the buffer in place: one
  `SlabText::delete`, allocation-free, nothing sealed — the keystroke path stays the keystroke path.
  A range that reaches sealed text seals what is open (one append) and then calls `remove_range`:
  one root copy, **zero bytes written**, because the document is geometry and the bytes were never
  the document's. Cutting a million bytes out of a document costs what cutting ten out of it costs.

  **What it means, and this is the part worth the ink.** The world-line still holds the ciphertext
  of everything deleted; a reader with the passphrase can decrypt it. That is not a defect in the
  deletion path — it is what an authenticated append-only log *is*. But it means "deleted" and
  "erased" are different words, and a product that uses the first while its users hear the second is
  lying by vocabulary. **OQ-14** records the gap and names the two shapes a policy could take; until
  one exists, no user-facing claim may say erased.

  **A cut leaves fragments, and that is OQ-9, not a bug.** Deleting a range that spans leaf
  boundaries splits the leaves it lands in and rejoins nothing, so the leaf count can grow while the
  document shrinks. Found by a test that asserted two leaves where the engine correctly leaves
  three; the test now says three and names the open question rather than the code being bent to fit
  a guess.

  **The empty-leaf case, again.** Deleting the whole open buffer leaves a source with no bytes, and
  the tree may not hold a leaf that carries nothing. The leaf leaves the spine, the register
  abandons the source (`OpenSources::close`), and the provisional id becomes `Unknown` rather than
  pointing at a segment that was never written — the same instinct that made `tree.rs` refuse empty
  leaves in the first place.

  **What is still missing, on purpose.** Selection is a pair of offsets in the caller's view, so the
  storage layer owes it nothing new; undo of a deletion needs `History` to observe the root swap,
  which is the next slice, and it is where OQ-8's unbounded-history question has to be answered
  rather than deferred again.

- **2026-11-17 — D-10 — the viewport, measured on thirty million words.** Gate 2 says layout cost is
  invariant in document length. Half of that is about fonts and belongs to Block 6; the half that
  decides it is how much work the document's *length* can force, and that is measurable now.

  **What was measured, and what was deliberately not.** Index reads and bytes handed to the layout,
  on a document of 180 MB of text as geometry — 43,945 leaves of 4 KiB, one height each — swept one
  viewport per screen from the first byte to the last. Seconds were not measured: shaping needs a
  real font, and a fake shaper would make a timing number that is about the fake. `tests/`
  `viewport_cost.rs` is the sweep.

  **The result.** The worst viewport asks the layout for one screen of text plus the two leaves its
  edges land in — 1.2 MB at 300 leaves a screen — and the *same bound* holds on a document a
  thousand times shorter, which is the invariance claim stated as a bound that does not contain the
  document's length. Finding that window costs **59 counted index reads on 43,945 leaves** and **18
  on 43 leaves**: the bound is a multiple of the tree's depth plus a constant, so ten more levels
  cost about forty more reads — a logarithm, not a walk. The number came from the first CI run that
  failed the assertion I guessed (`59 reads at 1000x the length against 18`), which is the correct
  way for a measurement to arrive.

  **Why this is not yet gate 2.** The gate is about cost, and cost has a clock. What this shows is
  that the clock's *input* — bytes shaped per viewport — cannot grow with the document, and that the
  index work to find it is logarithmic. The remaining measurement needs a shaper with a real font,
  which arrives with Block 6's shell, and the gate stays unclaimed until then. OQ-12 says exactly
  that instead of a softer version of it.

- **2026-11-17 — D-9 — typing in the middle: the caret says where, the tree does the cut.** D-8 left
  the session append-only, and said so. Removing that limit turned out to be mostly a matter of
  noticing what the spine already does.

  **The cut is geometry, not a rewrite.** `insert_slab(at, slab)` splits the leaf the offset lands
  in: the two halves name adjacent ranges of the *same* sealed segment, so no byte is read, no byte
  is written, and nothing is re-sealed. Only the new text is appended. The naive editor re-seals the
  whole leaf the caret is in; this one pays for the new bytes and nothing else.

  **The rule that makes it safe: moving the caret seals what is open.** One open source at a time,
  so a jump appends the pending bytes once and leaves the document fully sealed while the new source
  is placed. The alternative — several open sources drafting lengths at once — would make every
  offset query walk all of them and turn one overlay entry into a list. The rule costs one append
  per caret jump, which a user cannot perceive and a test can pin.

  **What is now exact by construction.** Every offset before the open leaf is the tree's offset, so
  `insert_at` needs no correction for drafts; the overlay stays a single entry; and the segment
  arrangement of a mid-document insert is assertable rather than argued — the tests read the leaves
  back and check that the outer two are `segment 0` at `0..5` and `5..11` with the new text between
  them.

  **What is still missing, on purpose.** Deletion, selection, and undo of typing. Each needs the same
  caret rule and each will get its own record rather than being implied by this one.

- **2026-11-17 — D-8 — the typing session, and the overclaim it caught.** D-7 was a design with a
  testable claim attached. Building it (`typing.rs`) found one place where the claim was wrong, and
  the measurement now exists for the rest.

  **What a keystroke costs.** One insert into a spare-capacity [`SlabText`] — measured
  allocation-free since Block 4 — and one `Draft::record`, which updates an entry in place. No root
  is copied, no segment is written, nothing is allocated. The first byte of a source registers its
  leaf with the length it has at that instant, so the tree needs no draft yet; only later keystrokes
  are drafts.

  **The overclaim.** "Sealing leaves the root untouched" was false in one respect: the tree has been
  carrying the leaf's *registered* length, so a seal must correct that leaf to its live length before
  the draft can be dropped. A root object changes. D-7 is amended in place rather than left standing,
  because a record that is 95% right is the most expensive kind to leave in a repo.

  **What survives.** No history entry is made: `Typing` owns the current root and rewrites it, so
  undo steps over a seal the way it steps over an idle moment. It is one root per 64 KiB of text,
  not one per keystroke — 500× fewer copies at a realistic buffer size. And nothing is written twice:
  the sink receives each byte exactly once, which is now a test rather than an argument.

  **The claim D-7 cared most about is now witnessed, not asserted.** `holonomy-eye`'s
  `typing_visible.rs` types into a session that has sealed nothing, asks `byte_window` what the
  viewport should shape, and gets the text being typed — the exact thing that was structurally
  impossible before D-7.

  **What is deliberately not done.** The session is append-only: the open source is always the
  document's tail. Typing in the middle splits a leaf, which needs the caret to say where, and the
  caret belongs to the editor. The record says so instead of implying the harder case works.

- **2026-11-17 — D-7 — OQ-10 solved: a leaf names a place, and a place can be open.** D-6 was
  correct about the arithmetic and wrong about the shape. It accepted the frame "a leaf names a
  sealed segment, so text that is not sealed has no leaf" and then paid for that frame with a third
  overlay case, a caret-dependent commit, and a hole in `byte_window` exactly where the user is
  typing. The frame was the bug.

  **The turn.** Separate two questions that were tangled: *where does this leaf's text live* and
  *which sealed segment is it in*. A leaf needs the first; only the storage layer cares about the
  second. Let a leaf name a **source**, and let a source be **open** (bytes in a resident buffer,
  being typed into) or **sealed** (bytes in the world-line). Same address either way.

  **What that buys, in order of how much it matters.**

  *The typed text is a leaf.* It gets a leaf index, a `slab_span`, a height and a byte range from
  its first byte. `byte_window` sees it with no new case, so OQ-12's caller is no longer blocked by
  a hole in the middle of the viewport. The "overlay cannot express an insertion" problem was never
  about insertions: it was about leaves being restricted to sealed territory.

  *Sealing is a state change, not an edit.* When the buffer fills, the source is sealed — same id,
  bytes appended once — and a fresh open source takes over. **The root does not change.** So the
  world-line, `History`, undo and `verify` never observe a seal at all; the document never changed,
  only where its bytes live did. That also answers the question D-6 left dangling: undo across a
  seal is not "an ordinary commit's undo", it is *no undo event at all*.

  *Each byte is written once.* Typed into an open source, sealed once. One megabyte of typing seals
  one megabyte, against option A's 8.5 MiB for the same megabyte and 8 GiB by thirty-two — and the
  difference is structural rather than tuned: nothing is ever re-sealed, because nothing that is
  sealed is ever typed into again.

  *The caret stops mattering.* D-6 needed the commit to know where the caret was, because it was
  sealing an edited leaf's prefix. With open sources the editor only ever seals a source it owns,
  whole — no split, no prefix, no caret. Where the caret sits is a *view* concern again, which is
  where it belonged.

  **What it costs, stated before it is built.** Ids become identities rather than positions, so a
  segment id is allocated when its bytes are sealed and the world-line records it in the segment
  header; a world-line whose ids are not positions must say so, and `verify` reads the id from the
  segment rather than inferring it. Residency gains a lookup: a source resolves to a buffer or to a
  segment. And at most a handful of sources may be open at once, because each one is a resident
  buffer and residency is a bounded resource (L-2) — the editor's insert path must seal before it
  opens another, which is one `if` and one test, not a policy.

  **What it deletes.** D-6's third overlay case, D-6's empty-leaf workaround, D-6's caret-dependent
  commit, and the sentence in OQ-12 that said the viewport could not see what the user is typing.
  The overlay keeps exactly one job — the live length of a source being edited — which is the job it
  had all along (D-2).

  **Amended the same day, when D-8 built it: "sealing leaves the root untouched" was too strong.**
  The tree carries the leaf's *registered* length — the length it had when the leaf first got a byte
  — while the overlay carries the live one, so the seal has to correct that one leaf's length before
  the draft can be dropped. A root object therefore does change. What remains true, and is the
  claim that matters: **no history entry is made**, so undo steps over a seal as it steps over an
  idle moment; nothing is re-read; and it is one root per buffer, not one per keystroke. The
  correction is stated here rather than quietly left in the code.

- **2026-11-17 — D-6 — OQ-10, first answer: seal at the caret, and the pending leaf is an overlay, not a leaf.** *(Superseded the same day by D-7: the awkward part was not the overlay's shape but the assumption that a leaf names a *sealed* segment. Kept because the arithmetic it records is still the reason option A is dead.)*
  `SlabText::insert` refuses with `WouldSplit` when a resident slab is full, and nothing said what
  happens next. There are exactly two continuations, and one of them does not scale.

  **Why not re-seal the leaf.** Committing the full leaf and re-faulting it works and is one line of
  wiring, but each exhaustion seals the leaf's whole text again: at 64 KiB, typing past a full leaf
  seals 64 KiB, then 128, then 192 — the k-th seal writes k·64 KiB, so n typed bytes seal about
  n²/128 KiB. The first megabyte costs 8.5 MiB of redundant sealed plaintext; by 32 MiB it costs
  8 GiB, and every byte is a ciphertext a future `verify` must walk. The ratio is not a constant,
  which is the tell: 8× at one megabyte, 256× at thirty-two.

  **What we do instead.** Seal only the prefix, at the caret: `commit_slab` the resident prefix and
  `Spine::replace_range(leaf_start, caret, new_slab)`. This is expressible with the API the commit
  boundary already has — `replace_range` collapses a range to one slab — and the tail leaf keeps
  naming its old segment, unchanged, because the bytes after the caret were not edited. Total
  sealed: exactly the new text. The world-line grows with what was typed, not with what exists.

  **Why the pending leaf is not a spine leaf.** The cleanest identity for the new text would be a
  leaf of its own, so that `slab_span` gives it an index and `byte_window` covers it. `tree.rs`
  refuses it: `assert!(!slab.is_empty(), "a leaf must carry bytes")`. That refusal is right — a leaf
  that names nothing is a handle to nothing, the same reason `Draft` returns drafted leaves whole
  instead of clipping them. And `Draft` cannot host it either: it maps a leaf that **exists** to the
  length its resident text now has, and an insertion is text that no leaf names.

  So the pending text is a third case in the live-document overlay, beside `LiveSpan::Sealed` and
  `LiveSpan::Draft`: an insertion with an offset and a length and **no slab** — which is why
  `segment_offset` is already `Option` and why the case costs no new lie. `byte_window` then reports
  the pending extent to the Eye along with the spine range, so newly typed bytes are shaped like any
  other.

  **The ordering this exposes.** OQ-12 was recorded as waiting on OQ-10, and the reason is now
  concrete rather than procedural: `byte_window` maps leaves to bytes, and the bytes the user is
  actively typing are not in a leaf. A viewport chain that cannot see the caret's own text is not a
  caller. The overlay's insertion case is therefore the next slice, and the gate claim waits for it.

  **One thing this does not change.** Undo after a seal-at-the-caret commit is an ordinary commit's
  undo: the root after `replace_range` is a normal root naming two leaves instead of one, and
  `History` returns to the root before it. No new mechanism, no new invariant — which is the
  standard this record has to meet before it is believed.

- **2026-11-17 — D-5 — OQ-13 closed: quiet-md is source, not a dependency.** The question was
  presented as three shapes (published dependency, vendored subtree, fork of a fork) and one of them
  turned out not to exist.

  **The dependency is impossible, not merely unattractive.** Cargo applies `[patch.crates-io]` only
  from the workspace root, so consuming `quiet_md` builds it against stock GPUI 0.2.2. quiet-md's own
  code calls what its patches *add*: `grid_col_widths(Vec<f32>)`, `Window::paint_pixels`,
  `Window::replace_root`, `Window::set_window_min_size`, `RenderImage::with_scale_factor`,
  `Path::transformed`, `WindowTextSystem::glyph_outline`, `PathPromptOptions.extensions`,
  `ListState::estimated_scroll_metrics`. Three tables, three vendor trees and 43 patches are
  load-bearing, so the crate as published (if it were published) would not compile for us.

  **The other two shapes are what Block 1 exists to forbid.** Copying `vendor/gpui` in means a
  `[patch.crates-io]` onto a fork inside the core workspace — the exact mechanism that held v5's UI
  crates hostage. Moving the UI to its own workspace to keep the patch out of the core tree is the
  other half of the same law ("one workspace, one lockfile, every crate a member"). The law decided
  this, which is the point: the decision was available before quiet-md was ever opened, and the
  research only confirmed it.

  **What we therefore do not inherit, said out loud.** quiet-md's patches #4, #23, #30, #31, #32 are
  CJK 避头尾 line breaking, script-aware word classes for Hangul/Hebrew/Arabic/Syriac and RTL
  wrapping with per-line reordering — which is *the Eye's territory*, not GPUI's, because the Eye
  lays out the page. Those five patches are now the best available reference for what OQ-11's
  segmentation must eventually do, and #30 in particular is a worked example of the bidi line-order
  problem. Patches #1, #12 and #14 are rendering fidelity; without them the page is stock GPUI, and
  `docs/UI-SPEC.md` §8 forbids ever measuring a gate against quiet-md's pixels instead.

  **The ledger, because MIT requires one.** `THIRD_PARTY.md` records adapted source: nothing yet,
  and the four rules that govern the first file that ever is (provenance line in the file, licence
  text travelling with it, same pull request, and nothing that needs a patch section to build).

- **2026-11-17 — Block 5, fifth slice: the viewport chain closes.** Three pieces that had been built
  for each other but never introduced: the height index answers *which leaves*, the spine answers
  *where those leaves live*, and nothing joined them.

  **`Spine::slab_span(index)`** is an order statistic: descend the cached slab counts, O(log N),
  no scan. Without it the Eye would have to read `spans_in` and count leaves to answer "which leaf is
  the twelfth", and the cost of finding the viewport would grow with the document — the thing Block
  5's second gate forbids.

  **`byte_window`** is the join, and its interesting decision is that the byte range is *contiguous*
  from the first covered leaf's start to the last covered leaf's end. Building it leaf by leaf would
  be the obvious implementation and would leave holes: an unmeasured leaf has zero height, so a
  viewport can skip over it, and shaping the covered leaves individually would silently drop the text
  in between. The range therefore includes it, and the docs say why. An unmeasured *prefix* or
  suffix is still excluded — that remains the provisional-scroll limitation recorded with the model.

  **The dependency, stated.** `holonomy-eye` now depends on `holonomy-text` (workspace path, no
  registry cost) for exactly this join. Geometry still enters through `Shaper` alone: the Eye reads
  byte offsets and leaf geometry, never text, and the Spine still holds no plaintext.

  **Still open.** OQ-12's caller — the editor loop — and its 30 M-word measurement arrive with Block
  6. OQ-10's pending leaf has no leaf index and no `slab_span`, so it is in no window this function
  produces; that decision is unchanged and still gates the editor.

- **2026-11-17 — The UI basis is quiet-md, recorded rather than assumed.** The user named
  [`s1dashu/quiet-md`](https://github.com/s1dashu/quiet-md) — a Rust + GPUI notes app, MIT, an app in
  `src/`, a library in `crates/core`, and a **patched GPUI under `vendor/`** — as the codebase the
  editor's shell is built on, with a Google-Docs-shaped toolbar above the page. Two parts of that
  were already the plan; one was not, and the difference is the point of this entry.

  **Already specified.** The toolbar is Block 7's entire subject ("the Google-Docs toolbar inside the
  calm", `ROADMAP.md`), with `docs/UI-SPEC.md` §3 specifying its behaviour — focus returns after a
  click, read-only actions grey out, the security dot at the right end. Block 6 already says the
  shell is a GPUI window with a quiet-md-*grade* page. What did **not** exist anywhere in this repo
  was quiet-md as a dependency: it appeared only as that adjective, an aesthetic benchmark, with no
  licence, version or commit recorded.

  **What the decision adds.** quiet-md is the basis of the UI rather than a reference to it, so Block
  6 starts from a codebase that already solves GPUI text drawing, IME and the page feel. Its licence
  is MIT, compatible with this workspace's `MIT OR Apache-2.0`.

  **The law it collides with, stated before anything depends on it.** Block 1 forbids
  `[patch.crates-io]` onto forks in the core tree, and quiet-md's tree carries a *patched* GPUI with
  a numbered patch list (`vendor/PATCHES.md`, #43 as of 0.1.13). Both cannot stand unchanged. The
  three shapes that can: depend on quiet-md's published crate and leave GPUI alone; vendor quiet-md
  under an audited patch list of our own; or upstream the patches we need and keep the core tree on
  upstream GPUI. Each has a different failure mode, none should be chosen by accident, so it is
  **OQ-13** and it must be settled before Block 6 opens rather than during it.

  **What this does not change.** The Spine stays authoritative for text, styles and history:
  quiet-md is the shell and the editing surface, not the document engine. The Eye is headless, so
  L-8 and the single-geometry law are untouched by which framework draws the result.

- **2026-11-17 — D-4 — the height index lives beside the tree, not inside it.** `tree.rs` ends with a
  standing invitation: "Weights are byte lengths today; the Eye's layout heights will ride the same
  aggregate once Block 5 defines them." Block 5 has now defined them — `Fixed` 64ths, per style, per
  leaf — and they still do not ride that aggregate. This entry is the argument, because the
  alternative is a comment left standing that the code silently contradicts.

  **Why not the tree.** A leaf's height is *measured*, lazily, at the current wrap width. A window
  resize invalidates every measured height at once, and an immutable path-copied tree can only
  express that by rewriting all of its nodes: O(n) nodes for an event as common as dragging a window
  edge. The Fenwick model is invalidated in O(1) and re-measures only what the viewport looks at.

  **Why this is not the K-8 disease.** The danger `tree.rs` guards against is duplicate *geometry* —
  two arithmetics that must agree and therefore can disagree. `HeightModel` stores the Eye's own
  measurements, produced by the one geometry, and nothing computes a height twice: it holds numbers,
  it does not hold a second opinion about where text goes. The tree keeps its byte, slab and height
  aggregates, and `Spine::slab_span` is the join between the two indexes — leaf position to byte
  range in one descent — so neither side ever walks the other's structure.

- **2026-11-17 — Block 5, bug hunt before merge: a break that stopped being a break.** Three real
  defects in the segmentation and transform, found by reading the code adversarially rather than by
  a failing user report.

  **1. A mark after a newline swallowed the line break.** Segmentation joined a combining mark to
  whatever preceded it, including `"\n"`. The cluster then read `"\n\u{301}"`, which is not
  `"\n"`, so the layout never took the hard-break branch: the newline was laid out as nothing and
  the next line's text joined the previous one. The caret and any renderer would have disagreed
  about where the line ended — the exact failure class this crate exists to make impossible.

  **2. A zero-width joiner reached across a newline** for the same reason, from the other side: the
  ZWJ rule was unconditional, so `👨\u{200D}\n` was one cluster.

  **3. A lone carriage return was not a break at all.** UAX #29 makes CR, LF and CR LF all mandatory
  breaks (GB3/GB4); the layout only knew two of the three, so a CR-only document rendered its line
  ends as glyphs and its carets ran through them.

  The fix is a single guard: nothing joins across or onto a break, and CR LF is the only pair that
  merges. The regression tests are the interesting part. Besides six segmentation assertions, the
  layout now has an invariant that would have caught all three at once: **the window's bytes,
  reassembled from the run tree in line order, must equal the window**. Clusters carry their text
  and lines carry their boundaries, so the bytes between one line's end and the next line's start
  are exactly the break that separated them — nothing at all for a soft wrap. A swallowed newline
  disappears from that reconstruction. The check runs over a fixed corpus and inside the randomized
  property test, so future segmentation edits have to keep the bytes.

  **Also hardened, not fixed.** `View::to_device` multiplied a coordinate by the zoom numerator in
  `i128` and cast the result with `as i64`: an absurd zoom (or a coordinate near the end of the
  range) wrapped in release and panicked in debug. It now saturates, because a coordinate that
  large is nowhere near a viewport and a saturated number stays visibly absurd where a wrapped one
  looks like geometry. `View::hit_test` debug-asserts that a line box does not map to zero device
  units, which is the one input that would make "which line is this click on" undefined.

- **2026-11-17 — Block 5, fourth slice: the height index, and a cost claim that is counted.**
  Block 5's second gate says layout cost must be invariant in document length. Half of that is the
  index that finds the viewport without reading the document; the other half is the caller that
  supplies windowed input, and the caller does not exist yet (OQ-12).

  **What landed.** `HeightModel` keeps one measured height per leaf — Block 4's slab, the unit the
  Spine already splits and re-faults, so the Eye does not invent a second partition of the text —
  in a Fenwick tree. `prefix`, `set`, `leaf_at` and `window` are O(log n) cell reads. A window is a
  leaf range plus its two document coordinates, so a scrolled viewport can be described without
  touching the index again.

  **Why the cost is counted.** A wall-clock test would measure the runner; the gate is a claim about
  the algorithm. The model counts the index cells it reads, and the tests assert numbers: a window
  over 2^18 leaves costs fewer than 160 reads, and sixteen times as many leaves costs less than
  twice the reads. This is the same discipline as the counting-allocator gate, applied to reads
  instead of allocations.

  **Zero is a value, not a hole.** A leaf is unmeasured until it has been laid out, and its height
  is zero until then. `leaf_at` steps over unmeasured leaves rather than pretending they occupy
  space, which means a window that contains them is provisional — the docs say so, and a caller that
  trusts such a window too early will see its scroll geometry move as measurement catches up. That
  is a real limitation, recorded rather than smoothed over.

  **What this does not do.** It does not decide where a leaf's text comes from when the leaf is a
  *pending* draft that exists in no spine (OQ-10), and it does not yet ask the Spine for anything.
  The next slice is that wiring, and it is the slice where OQ-10's answer has to be chosen, because
  the same walk that picks leaves must decide what a pending leaf's height means before it is
  committed.

- **2026-11-17 — Block 5, second and third slices: zoom as a view transform, style runs as layout
  data.** Both slices answer the same question — *which arithmetic is allowed to change a
  coordinate* — and both answer it by adding none.

  **Zoom/DPI has no inverse.** The tempting shape is paint multiplying by the scale and hit-test
  dividing by it: two roundings, two answers, and zoom becomes the seam Block 5 exists to close.
  [`View`] therefore exposes one mapping, `to_device`, and `hit_test` never maps a device point back
  into layout space — it compares the point against the device positions paint would draw, computed
  by that same mapping. The transform is exact rational integer scaling (`logical × zoom_milli × dpi
  / 96_000`, rounded half away from zero in `i128`). A line box maps as a whole, so one line's
  bottom is exactly the next line's top and a caret's own `y` always selects its own line; the
  caret's painted height is the distance between the mapped top and bottom, so the box a renderer
  draws is the box hit-testing tested. `Layout::paint_point` and `Layout::hit_test` are now
  `View::IDENTITY`'s answers — one implementation of the walk, not two that must agree.

  **Styles are geometry, not decoration.** A heading is not the same layout drawn bigger: bigger
  fonts have bigger advances and taller line boxes, so a renderer that applies a style the layout
  did not shape with owns a second geometry — the v3 disease wearing a hat. `Style` (size in
  thousandths of a logical unit, bold, italic) now travels as `StyledRun`s in `StyledText`; the
  `Shaper` is asked every metric question *with* the style it applies to; `Placed` carries the style
  it was shaped with; a line box is as tall as its tallest cluster and an empty line takes the
  height of the style that reaches it. Font families are deliberately absent — they are Block 8's
  pipeline decision and cannot be tested before that pipeline exists.

  **What the tests pin.** L-8's property test now sweeps a ten-string corpus (Latin, combining
  marks, ZWJ emoji, CJK, hard breaks, trailing newline, empty text) over six wrap widths × eight
  zoom steps × five DPIs, and asserts the strong form — *the same byte back* — wherever device
  positions are distinct; a zero-width-cluster shaper pins the tie rule where they are not, because
  two carets at one device position cannot be told apart by a click and the answer must not depend
  on rounding. Style tests pin the run boundary, a wrapping window with three mixed-size runs, and
  empty lines inheriting the style that reaches them. `View::IDENTITY` is asserted to answer exactly
  as the layout does, so the transform is provably the only difference between the two spaces.

  **Still `spec`.** `L-8` remains `spec`: the property holds for a fake shaper, and Block 8's real
  pipeline has not been through it. Block 5's second gate (layout cost invariant in document length)
  has no evidence at all — `layout_styled` shapes whatever window it is handed, and nothing yet
  constrains that window to a viewport (OQ-12) — and its third gate (BiDi and multi-script
  cluster-honest carets) still rests on the approximate segmentation recorded in OQ-11.

- **2026-11-17 — Block 5, first slice: one geometry, two consumers.** The Eye's geometry core lands
  as four modules with no dependencies, no font, and no window, because the property that matters
  is provable without any of them.

  **Coordinates are integers.** [`Fixed`] is an `i64` in 64ths of a logical unit. L-8 compares
  positions for equality, and floats would make "the same point" a question about rounding: paint
  would round one way and hit-test the other, reopening the seam inside the arithmetic itself. The
  only float in the crate is `to_f32`, for the GPU, and nothing compares against it.

  **Caret positions are cluster boundaries.** Segmentation is a hand-written, conservative
  approximation of UAX #29: combining marks, variation selectors, ZWJ sequences, skin tones,
  regional-indicator pairs and CRLF merge; **nothing ever splits a mark from its base**. The
  approximation errs in the safe direction — a caret may sit where a real renderer would not allow
  one (Hangul jamo stays split; OQ-11), but it can never produce two answers for one point.

  **The one list.** `layout_window` builds lines of placed clusters; `Line::boundaries` yields
  `(x, byte)` for every caret position on a line, and `paint_point` and `hit_test` are the same
  walk in opposite directions. A soft-wrapped line deliberately exposes **no** end caret, because
  wrapping inserts no character: offering one would give a byte two visual positions, which is the
  drift this block exists to prevent. Ties in `hit_test` go to the earlier boundary, so the answer
  never depends on storage order.

  **What the tests pin.** Golden geometry (`"abc"` at three-unit glyphs, exact 64ths), wrap and
  hard-break line splitting, empty lines keeping exactly one caret, click tie-breaks, every caret
  position in nine corpus strings round-tripping at six widths, and 64 randomized windows over an
  alphabet of combining marks, ZWJ emoji, skin tones, wide clusters and newlines. Advances enter
  only through the `Shaper` trait, so a fake shaper proves the law without a font.

  **Still `spec`.** `L-8` stays `spec` for two reasons, both stated in the crate docs rather than
  hidden: segmentation is approximate (gate 3's BiDi half is not attempted at all), and the
  zoom/DPI view transform does not exist yet, so "at every zoom and DPI" quantifies over nothing.
  The transform is the next slice; promoting `L-8` before it lands would be the audit failure this
  document exists to prevent.

- **2026-11-17 — Block 4, fourth slice: the live-length overlay, so a stale spine still answers.**
  D-2 recorded its own cost in plain words: the resident slab is authoritative while it is edited,
  the spine is path-copied at commit, and *the editor must carry a live-length overlay until
  commit*. Without it, a keystroke followed by a viewport query is answered by geometry that is
  wrong by exactly the number of characters typed. [`Draft`] is that overlay.

  **What it is.** A record of a handful of leaves and the lengths their resident text now has.
  Every query partitions the walk at those leaves and asks the spine its own O(log N) question per
  region, translating offsets by the accumulated difference. `document_len`, `locate` and
  `spans_in` therefore stay correct while the spine is stale, and **the view does not move when the
  commit lands**: the integration test types a character, records the draft, queries offsets,
  lengths and spans against the stale root, commits, forgets the draft, and asserts the answers are
  the same against the sealed one.

  **Why it is a projection and not a document.** It holds no plaintext, so the wipe set is
  untouched; nothing in the spine consults it, so geometry stays the only thing the tree knows; and
  `forget`/`clear` restore the sealed view exactly. That last property is what makes
  undo-after-commit sound: the old root is unchanged, so the overlay must go back to matching it.

  **Two asymmetries, both deliberate and tested.**
  - *Sealed leaves are clipped to the requested range; drafted leaves are returned whole.* A
    drafted leaf's visible portion cannot be expressed as a `Slab` — its bytes exist in no segment
    until commit — so a partial extent would name nothing. The caller clips inside the resident
    buffer it already holds.
  - *An emptied leaf occupies no space.* A drafted leaf whose text was deleted out is not a span
    and is never located; the commit boundary removes it rather than sealing an empty segment. The
    two agree because both treat "no bytes" as "not there".

  **What this entry does not claim.**
  - **No measurement, again.** Re-recording a leaf that is already drafted updates in place and
    allocates nothing; the *first* record for a leaf may allocate unless `Draft::with_capacity`
    covers the dirty set, and the editor sizes it from the viewport the way it sizes the resident
    budget. None of that is measured: the L-2 gate's window is still `locate` → resident hit →
    in-place edit, and the overlay sits outside it until a future slice folds it in or measures it
    separately. No law is promoted.
  - **Still no editor.** The overlay answers geometry questions; caret placement, selection,
    scrolling and paint are Block 5's Eye, and the wiring that decides *when* to commit — a slab
    change, a scroll, an idle timer — is a policy this crate does not make.

- **2026-11-17 — Block 4, third slice: the commit boundary, proven against the real container.**
  D-2 said the spine is path-copied "at commit" and admitted the commit boundary did not exist.
  It exists now: `commit::commit_slab` seals an edited resident slab into the world-line, takes
  the witness, and returns a new root. The chain is executed, not asserted — `holonomy-line`'s
  real `WorldLine`, `holonomy-seal`'s real capsule and DEM key, the real durability barrier, the
  real MMR — in `crates/holonomy-text/tests/commit_boundary.rs`.

  **D-3 — the commit unit is one slab's document extent, not one segment's, and the fence precedes
  the append.** Two decisions, both forced:
  - *One slab's extent.* A re-seal replaces exactly the range one leaf covers
    (`Spine::replace_range`), so no offset outside that range moves and no neighbour is rewritten.
    Committing at **segment** granularity instead would mean rewriting every leaf naming the edited
    segment and shifting their start offsets after the edit point — the offset arithmetic that
    silently duplicates or loses bytes, in every leaf, on every commit. A segment already cut into
    several leaves is committed leaf by leaf; each commit seals that leaf's bytes as a fresh
    segment.
  - *The fence precedes the append.* `commit_slab` refuses a span the spine no longer holds
    (offset moved, leaf replaced, or offset no longer at a leaf start) **before** the world-line is
    touched. Appending first and discovering the span was stale afterwards would leave an orphan
    segment — durable, authenticated, unreferenced, and indistinguishable from a real one in the
    file. Undo-then-recommit is exactly how a real editor would produce that, so it is a test, not
    a hypothetical.

  **What the tests prove.** One typed character inside a resident slab becomes segment 1; the new
  root names it over the whole extent the old leaf covered; the old root still describes the
  document it was cut from; a fresh handle on the file sees both segments, the same segment root,
  and passes a full `verify()`; a `ResidentSet` bound to that world-line faults in exactly the
  committed bytes for the segment the new root names. Committing an emptied slab removes the leaf
  and appends **nothing** (no empty segment, no witness), committing the same span twice is refused
  with the world-line byte-for-byte unchanged, and a refused append leaves the spine alone. Undo
  across a commit walks back to a root whose segment was never rewritten.

  **What this entry does not claim.**
  - **No measurement.** Commitment allocates — path copies, the segment record, the witness — and
    is supposed to. L-2's zero-allocation claim stays exactly as narrow as the keystroke binary
    measures it: `locate` → resident hit → in-place edit. Nothing here promotes L-2, and no new
    number was taken.
  - **No editor.** The spine stays stale between commits by design; the editor's live-length
    overlay is still unbuilt, so a keystroke followed by a viewport query is *not* answered by this
    code — the overlay answers it. That overlay, and wiring it to this boundary, is Block 5's.
  - **No coalescing.** The commit replaces one leaf and never merges neighbours, so it creates no
    fragments (the tests assert the leaf count is unchanged across a commit) but it does not repair
    fragments either. OQ-9 is narrowed below, not closed.

  **What the first push cost.** Two compile-level findings, both about the test, neither about the
  boundary: `drop(sink)` on a type that implements no `Drop` (clippy's `drop_non_drop`; NLL already
  ends the borrow at the sink's last use) and `assert_eq!` over `Result<Committed, _>`, which cannot
  work because `Committed` holds a `Spine` and `Spine` deliberately has no `PartialEq` — two spines
  with equal bytes are not the same spine. The refusals are compared as errors instead. A design
  constraint became a compile error in the tests, which is where it belongs.

  **Dependency direction.** `holonomy-line` and `holonomy-seal` are **dev**-dependencies of
  `holonomy-text`: the shipped Spine links neither, and speaks to both through `SegmentSink` /
  `SegmentSource`. The adapters live in the test file because the application crate that will own
  the production wiring does not exist yet — putting them there means CI executes the seam rather
  than a comment describing it.

- **2026-11-17 — bug hunt before the commit boundary: property tests and three hardening fixes.**
  The Spine's own tests were scripted — a 4,096-append sequence, every byte of a 7-slab split — and
  scripted tests cannot find the shape nobody wrote down. This entry adds the machinery that can,
  and fixes what inspection found in the meantime.

  **Property tests now in CI:**
  - `tree::check_invariants` recomputes bytes, slab count and height from the leaves and panics on
    any disagreement with the cached aggregates, on an AVL violation, on an `Empty` child that
    should have been normalized away, or on a zero-byte leaf.
  - `random_join_and_split_shapes_keep_every_invariant` — 400 random appends, splits, rejoins and
    block concatenations per seed, over four seeds, checking aggregates against an independent
    count after **every** operation.
  - `random_edits_match_a_byte_level_model` — 250 random appends, insertions, removals and
    split/rejoins per seed, over four seeds, verified against a trivial `Vec<(segment, offset)>`
    model: same length after every step, spans tile the document contiguously, the expanded
    geometry is byte-identical to the model, and `locate` agrees with it. This is the test that
    catches a tree that keeps the right length while scrambling where the bytes came from.
  - `random_edits_match_a_string_model` — 500 random insertions and deletions per seed against a
    `String`, over four seeds, using multi-byte characters (é, 中, 🙂) so the boundary rules are
    exercised at every width.

  **Fixed by inspection (each now has a test or an invariant that would catch a regression):**
  - Zero-length slabs could enter the tree as leaves that carry no bytes, inflating `slab_count`,
    path copies and any budget computed from slab counts. `from_slabs`, `append` and `insert_slab`
    now drop them, and `check_invariants` rejects a zero-byte leaf outright.
  - `Slab::new` accepted `start + len` beyond `u32`, which would have made `end()` (and every
    offset derived from a slab) lie. It now refuses at construction.
  - `SlabText::insert` computed `len + text.len()` in a plain addition; it now saturates, so the
    refusal condition cannot wrap on a 32-bit target, and the one remaining panic in
    `from_plaintext` (an absurd configured slack) is documented rather than discovered.

  **Found and not fixed — recorded as OQ-8:** `History` keeps one root per state, so an unbounded
  undo history grows with the number of *edits*, not with the document. Subtrees are shared, so
  each root costs O(log N) nodes, but years of typing is still an unbounded set of them, and the
  world-line on disk already stores the history. Bounding the in-memory history is a design
  decision (ring of roots plus reconstruction from the log), not a patch, and it belongs before
  Block 6 rather than after it.

  **What CI said about the hunt itself, on the first push.** The property tests were written
  without a local compiler, and CI (run
  [44046796516](https://github.com/c4us4l1ty/holonomy/actions/runs/44046796516)) found exactly the
  class of error that reading cannot: `check_invariants` reported no drift, the AVL checks held,
  and the only failures were in the *test's own model*. The new shape test compared the tree's leaf
  count with the model's slab count, and seed 1 disagreed by one — six leaves against five slabs.
  The tree was right and the model was wrong: a cut inside a leaf produces two leaves for one
  segment and `join` never merges them back, which is the documented design (a segment's bytes may
  span several slabs) and the reason `splitting_and_rejoining_every_byte_preserves_the_sequence`
  already compares bytes rather than leaves. Counting "inside the document" was still one leaf too
  generous, and the second red run said so (28 modelled against 27 real): a cut can be strictly
  inside the document and still fall exactly on a leaf boundary, which spends nothing. The test now
  carries an independent set of leaf-start offsets and asserts leaves equal its size, which is exact
  — and it was replayed against a mirror of `join`/`split` for 16,000 random operations before the
  third push rather than trusting a fourth blind run. Two `clippy::unusual_byte_groupings` lints in
  the new seed arrays were the only other findings.

  **A bug in the diagnostic channel itself.** The third push went red on the `rustfmt` job while
  the bridge reported `cargo fmt --all -- --check` as **exit 0** — because the bridge ran
  `cargo fmt --all` (to capture the formatter's patch) *before* the loop, so the check it printed
  was inspecting a tree the formatter had already rewritten and could never fail. The check now
  runs first and the rewrite happens after it; the patch is appended to the report afterwards, so
  the verbatim patch is still what gets applied and the printed status is finally the real one.
  This is the second time this session a tool that reported success was the problem — the first
  was the keystroke gate measuring the default allocator.

  **What this entry does not claim.** The hunt covered `holonomy-text` — the code written this
  session and the only part not yet exercised by a property test. `holonomy-seal` and
  `holonomy-line` were read for consistency with their audits but not re-attacked; that is Block 9's
  implementation audit, still owed. No new measurement was taken: these are tests that run in CI,
  and every one of them is a claim a future change can fail.

- **2026-11-17 — Block 4, second slice: the keystroke path is measured, not asserted.** CI run
  [44045023810](https://github.com/c4us4l1ty/holonomy/actions/runs/44045023810) is green, and the
  workspace test job now executes a binary whose global allocator **counts**.

  **The claim.** `locate` (a descent over cached aggregates) → resident hit → in-place insert of one
  typed character performs **zero** allocations. Also zero: a backspace, filling the slab's spare
  capacity, and the refusal that follows when it is full. The count is taken by
  `holonomy_ledger::alloc::claim_quiet`, which panics with the measured delta rather than logging
  it. A control in the same binary asserts that faulting a segment in on a *miss* **does** allocate:
  if that control ever stops counting, the binary is not measuring anything and every zero above is
  worthless — the exact failure Block 1 recorded when its first unit-test version measured the
  default allocator.

  **Design decisions this slice forced, both recorded as decisions rather than preferences:**

  - **D-2 — edits are in place per keystroke; the spine is path-copied at commit.** With an
    immutable path-copied tree, updating a leaf's length per character would allocate O(log N)
    nodes per keystroke and make the L-2 gate unreachable by construction. So the resident slab is
    authoritative while it is being edited, and the spine is path-copied when the slab is re-sealed
    or the viewport moves — counted, occasional work rather than per-character work. The consequence
    is explicit: the editor must carry a live-length overlay until commit, and that overlay is not
    built. Nothing here claims a keystroke updates the spine.
  - **Recency is a counter, not a deque.** A `VecDeque` hit moves the entry to the back, and
    `push_back` may reallocate — an allocator call inside the very path being measured. The resident
    set now scans for the smallest `last_used` on a *miss* instead, which cannot allocate and costs
    O(resident), and the resident set is viewport-sized.

  **The budget got more honest.** `ResidentSet` now accounts its bound against **allocated
  capacity**, not text length: the slack reserved for typing is memory, and a bound that ignored it
  would be a bound in name only. Allocation-side accounting is what a memory adversary actually
  measures. Plaintext length is still reported separately.

  **Growth is refused, never done quietly.** `SlabText::insert` returns
  `EditOutcome::WouldSplit { needed, capacity }` and leaves the buffer untouched when the edit does
  not fit. A buffer that grew instead would hide an allocation inside the keystroke path and break
  the law silently; refusing turns the event into a structural one, where allocation is expected
  and counted.

  **UTF-8 is checked where the cost is, not everywhere.** Whole-buffer validation on each keystroke
  would cost O(slab) per character — precisely the "cost proportional to what changed" failure the
  architecture exists to prevent. The buffer is validated once at fault-in; each edit validates its
  own inserted bytes and its own boundary bytes. Grapheme clusters remain the Eye's business, not
  this type's.

  **CI trail.** Run 44044481285 caught rustfmt drift plus one real test-arithmetic error: an
  insertion at offset 9 into a buffer that had become 8 bytes long — caught by the test asserting
  the outcome of an earlier insert, not by review. 44045023810 is green, including the new counting
  binary. Clippy was clean on the first pass this time.

  **What this does not claim.** L-2 as a law stays `spec`: the keystroke half is measured, but the
  document-scale half — the same resident bound at one page and at thirty million words on reference
  hardware — has not been measured, and the commit boundary, the editor, and the Eye do not exist.
  The counting binary measures one test function in its own process; the module docs explain why
  sibling tests would race the counters. Nothing here promotes a law in `docs/GATES.md`.

- **2026-11-17 — Block 4, first slice: the Spine and the resident set.** `holonomy-text` now
  holds the structural half of RFC-003 and nothing of its measured half. CI run
  [44042785220](https://github.com/c4us4l1ty/holonomy/actions/runs/44042785220) is green: rustfmt,
  Clippy, 35 in-crate tests, the gates contract, and both qualification rehearsals.

  **Landed.** `Spine` — an immutable tree over `Slab { segment, start, len }` references, one
  slab being a byte range of one sealed segment. It is AVL-balanced, so ordered appends cannot
  degenerate into a list (a 4,096-append test pins height ≤ 24 against a degenerate 4,096);
  interior nodes carry bytes, slab count and height, so `locate` and `spans_in` are descents.
  Every edit returns a new root and path-copies O(log N) nodes — a test counts the shared
  `Arc` pointers and fails if an append copies more than its path. `History` keeps roots, so
  undo and redo are pointer swaps and no plaintext enters history. `ResidentSet` enforces a byte
  budget fixed at construction, evicts LRU from the front, wipes on eviction through
  `Zeroizing`, and offers `retain` for viewport moves; `SegmentSource` is the seam the
  world-line will fill, so the Spine still does not depend on a crypto crate.

  The L-2 *mechanism* is visible in test form: a 916-slab, 60,000,000-byte simulated lifetime
  scrolls end to end at a four-segment budget, resident bytes never exceed 262,144, each segment
  is faulted exactly once, and the same budget serves a two-slab document unchanged. This is the
  mechanism, not the gate: L-2 still reads `spec` in `docs/GATES.md`, and no reference-hardware
  memory measurement exists.

  **Decision D-1 — the flat Fenwick is subsumed by the tree's aggregates.** RFC-003 §4 named a
  Fenwick over slab heights. A flat Fenwick would need rebuilding beside every path copy and
  could disagree with the tree it indexes while computing the same prefix sums; the tree already
  carries bytes, slab count and height per subtree, so the descent *is* the index. Adopted
  subtree aggregates (with a note that the Eye's layout heights will ride the same aggregate),
  rejected the second structure, and amended RFC-003 §4 and the roadmap deliverable. The trade
  is explicit: one index cannot drift, and a future Eye height update path-copies O(log N) nodes
  instead of updating one array slot.

  **What this slice does not do.** No mutable resident text buffer, therefore no keystroke path,
  therefore no zero-allocation claim — asserting that before building it is the exact unmeasured
  promise VISION §5 refuses. No Eye, so no layout heights, no paint/hit-test geometry. Wipe is by
  construction (`Zeroizing` on eviction), not proven: the canary and the counting allocator that
  would prove it belong to Block 9. And the Block 4 gates below remain untouched:
  open-memory equivalence at 30 M words, offset-invariant keystroke latency, and the 1 M-keystroke
  fuzz run have not been attempted on any hardware.

  **CI trail for this slice** (recorded because "the compiler is CI" is a claim worth checking):
  run 44041603841 caught formatting, a missing `is_empty` companion and four `assert_eq!`s
  comparing `Result<Spine, _>` (Spine deliberately has no `PartialEq`); 44041980922 caught the
  same comparison errors after the formatter was satisfied; 44042360027 caught a real test-logic
  error — the LRU assertion expected eviction before the budget was full; 44042785220 is green.
  The diagnostics bridge was also made lossless in this slice: it now runs `cargo fmt --all`,
  posts the formatter's patch verbatim for `git apply`, and base64-chunks its report, because the
  previous single 60,000-byte comment silently dropped every command after the fmt diff.

- **2026-11-17 — gate-closure amendment: Block 2 closed, Block 3 closed in scope, Block 4
  unblocked.** Two gate items could not be answered with evidence available to this project — an
  allocator-wide proof of dependency-internal wiping, and physical power-loss/device qualification —
  and leaving them "open" had the same effect as killing the roadmap, because Block 4 may not
  start. The protocol already defines the third path: **amended** — the claim survives in weakened,
  explicitly stated form. Each claim below is narrowed to what a test establishes, the remainder is
  named as an unproven residual, and it is assigned to the gate that can measure it. Earlier audit
  entries that still describe these gates as open are left exactly as written — the trail records
  what was known on the day, and this entry supersedes them without rewriting them.

  **Evidence base:** the code and test tree at `8ac3618`, whose CI run
  [44034435423](https://github.com/c4us4l1ty/holonomy/actions/runs/44034435423) passed rustfmt,
  Clippy, the feature-graph check, workspace tests, the gates contract, and both the rsync and
  ext4-ENOSPC rehearsals. Nothing in this entry is a new measurement; it is a decision about what
  the existing measurements are allowed to mean.

  | Item | Verdict | Now claimed | Not claimed | Owed to |
  | ---- | ------- | ----------- | ----------- | ------- |
  | Block 2 — dependency-temporary zeroization | **amended** | every project-owned secret buffer is zero before free under the registered-allocation audit (nonzero-at-free, secret reallocation, and registry overflow all fail the gate); the reviewed wipe features are enabled and CI-enforced | that dependency-internal stack/heap temporaries are erased; a whole-allocator proof; any register or physical-memory guarantee | future hardening; residual in TM §5 |
  | Block 2 — RFC-001 audit closure | **amended** | every open RFC-001 item is killed, scoped with its exact limit, or assigned to its owning block | that the original unbounded wording was ever proven | — |
  | Block 3 — L-6 crash consistency | **amended** | the append/commit/recover ordering behaves correctly under injected short writes at every byte, both sync barriers, truncate failure, and real ENOSPC on an ext4 loop image | power-loss, controller/device-cache durability, journal replay, any supported-device matrix | Block 6 *kill-power-at-random-instant* gate; Block 10 release gate |
  | Block 3 — L-10 tail transfer | **amended** | one named client (rsync 3.2.7, protocol 31, 64 KiB block, local daemon over TCP) transfers literal data ≤ appended bytes + one block and ≤ 8× appended application-layer bytes for the measured workload | that any other client, backend, or remote service behaves the same | Block 10 backend matrix, or an explicit narrowing of L-10 in `LAWS` |
  | OQ-4 — scroll-timing side channel | **closed — accept and disclose** | the slab fault-in pattern is observable, and is already observable at the I/O layer: reading an old page reads its ciphertext at a known offset, so timing smoothing would hide the pattern from nobody who can observe the reads | that the pattern is hidden from a timing observer | TM §5 residual |

  **Protocol consequences, written into the repo by this entry.** `ROADMAP.md` Blocks 2 and 3 now
  carry these wordings and the named deferrals; `RFC-001` and `RFC-002` status lines and their
  zeroization/power-loss paragraphs match them; `docs/THREAT-MODEL.md` §4 and §5 gain the
  dependency-scratch, power-loss and scroll-timing residuals; OQ-4 moves from open to closed.
  `crates/holonomy-ledger/src/laws.rs` and the generated `docs/GATES.md` are **untouched**: no law is
  promoted, and L-6 and L-10 still read `spec` because neither has been measured to its bound.

  **What this entry does not do.** It does not certify v6, promote a law, or convert a residual into
  a claim. It moves an obligation from "nobody owns this" to "these gates own this". A future entry
  that treats these obligations as settled because this one filed them is invalid. If Block 10
  cannot obtain the hardware matrix, the honest outcome is to narrow L-6's bound in `laws.rs` — a
  visible edit to the machine-readable contract — not to pass it silently.

- **2026-11-16 — Block 2 dependency-temporary zeroization audit (gate remains open):**
  Reviewed the selected secret-bearing source paths at the release revisions below. `Cargo.lock`
  pins registry archives by version and checksum; the source links identify the corresponding
  release commits used for this static review. This is not a binary, allocator-wide, register, or
  physical-memory erasure proof.

  | Review area | Pinned source revisions reviewed |
  | ----------- | ------------------------------- |
  | NFC normalization | [`unicode-normalization` 0.1.26 (16c9597)](https://github.com/unicode-rs/unicode-normalization/tree/16c9597b73ec5b6b8ed55f4249a5ac854892544c); [`tinyvec` 1.13.3 (e343bbb)](https://github.com/Lokathor/tinyvec/tree/e343bbbb5a6594c9d57de718e6f232e6c0115dcb) |
  | KDF/hash | [`argon2` 0.5.3 (2b6f968)](https://github.com/RustCrypto/password-hashes/tree/2b6f9688625db3db798b35c8a209e4799bb60e11/argon2); [`blake2` 0.10.6 (1f727ce)](https://github.com/RustCrypto/hashes/tree/1f727ce37ff40fa0cce84eb8543a45bdd3ca4a4e/blake2); `digest` 0.10.7 ([3443894](https://github.com/RustCrypto/traits/tree/344389411fd9718a0742435152e933a9e71461ee/digest)) |
  | Hybrid KEM / DH | [`ml-kem` 0.3.4 (c53fd02)](https://github.com/RustCrypto/KEMs/tree/c53fd02ba7dcf5dc2c9287f814d1e401f26bfcce/ml-kem); [`x25519-dalek` 2.0.1 (4ac84dd)](https://github.com/dalek-cryptography/curve25519-dalek/tree/4ac84dd0668b1d2e51654fcdffe4e6a687bef00/x25519-dalek); `curve25519-dalek` 4.1.3 ([5312a03](https://github.com/dalek-cryptography/curve25519-dalek/tree/5312a0311ec40df95be953eacfa8a11b9a34bc54/curve25519-dalek)) |
  | AEAD / MAC | [`chacha20poly1305` 0.10.1 (c5c1999)](https://github.com/RustCrypto/AEADs/tree/c5c199953aba561261bed1c55f62b9c19d0f49cf/chacha20poly1305); [`chacha20` 0.9.1 (9c40806)](https://github.com/RustCrypto/stream-ciphers/tree/9c4080658507df4994508c1ef788f7eb89fe628c/chacha20); `poly1305` 0.8.0 ([35de31a](https://github.com/RustCrypto/universal-hashes/tree/35de31af7396fe57fa28265e91532c816183c47f/poly1305)) |
  | Sponge state / wipe support | [`sha3` 0.12.0 (0bdea4f)](https://github.com/RustCrypto/hashes/tree/0bdea4f919f86c17f75c6177567520010fedb735/sha3); [`shake` 0.1.0 (8f3e56d)](https://github.com/RustCrypto/XOFs/tree/8f3e56dc4e197b8adbc3d5a9ed109e326d6b159c/shake); [`sponge-cursor` 0.1.0 (9c8674f)](https://github.com/RustCrypto/utils/tree/9c8674f4fdf00fb524cd06d89da29d125db07582/sponge-cursor); `digest` 0.11.3 ([2fb9ed8](https://github.com/RustCrypto/traits/tree/2fb9ed8922e244117040bb037a7d141a6a2b8228/digest)), `block-buffer` 0.12.1 ([cbd0963](https://github.com/RustCrypto/utils/tree/cbd0963c685df025c42bed31f78c50d1bada3805/block-buffer)), and `zeroize` 1.9.1 ([34a6ebc](https://github.com/RustCrypto/utils/tree/34a6ebcb9b920ec7ae5809e470fce94fe7c0cb55/zeroize)) |

  The ML-KEM source checkout was at peeled commit `c53fd02ba7dcf5dc2c9287f814d1e401f26bfcce`;
  its annotated release tag `ml-kem/v0.3.4` has tag-object ID
  `6855b16a6cca9fc1c3eaea76776a7cd50ad67846`. This resolves the earlier tag-object/commit
  ambiguity.

  Findings and scoped mitigations:

  - **Passphrase normalization:** `unicode-normalization`'s NFC iterator stores pending canonical
    decomposition/recomposition sequences in `TinyVec` buffers with inline capacity four
    (`src/decompose.rs`, `src/recompose.rs`). Long combining runs spill to heap. The reviewed
    `tinyvec` source has no `Zeroize` or wipe-on-drop integration; normal drop releases its owned
    buffer but does not erase its bytes, and a moving growth allocation cannot recover/wipe old
    storage. `PassphraseInput::new` checks raw UTF-8 length before invoking NFC, so the 4,096-byte
    cap bounds the input driving that scratch. The normalized output is separately written to a
    fixed-capacity `Zeroizing<String>` and is not grown. A regression now fills the full 4,096-byte
    input allowance with one long U+0301 combining run and verifies rejection at 1,026 normalized
    bytes. This test exercises the cap/guard path; it does not observe or prove wiping of TinyVec
    scratch. The input is borrowed as `&str`, so the caller's original text is not wiped by this
    API. The raw-input cap also rejects inputs over 4,096 bytes even before their normalized
    length is known.

  - **Argon2 and BLAKE2:** the `argon2` `zeroize` feature gives `Block` an explicit `Zeroize`
    implementation, but not automatic drop wiping. Holonomy's caller-owned `Arena` iterates over
    all blocks and wipes them before freeing the slab; the byte-equality test and registered
    allocation test cover that project-owned arena. In the reviewed Argon2 source, the 1 KiB
    password-derived `hash` array is loaded into a block but not then wiped; `Block::compress`
    creates secret-derived `Copy` temporaries (`r` and `q`) without visible wipes; and
    `blake2b_long`/BLAKE2 hasher state and intermediate digest material have no reviewed wipe
    hooks. Argon2 explicitly wipes `initial_hash` after seed-block generation and its final block
    plus byte copy after successful final hashing, but error returns can precede those wipes. The
    direct `blake2` 0.10.6 pin has no `zeroize` feature. Holonomy also wipes BLAKE2 digest output
    arrays in `derive_subkey` and CMT-4, but those output wipes do not erase the dependency's
    internal hasher state (CMT-4's input includes the key).

  - **ML-KEM:** with the enabled `zeroize` feature, `DecapsulationKey::drop` wipes stored `dk_pke`,
    seed `d` when present, and `z`; the PKE decryption key exposes `Zeroize` for its stored
    `s_hat`. That covers the retained key object, not its working copies. Reviewed key-generation
    and PKE locals (`sigma`, `s`, `e`, `s_hat`/`e_hat` and arithmetic results), decrypt locals
    (`sTu`, `w`, and message output), and the decapsulation `mp`, `Kp`/`rp`, `Kbar`, and reencryption
    `cp` have no visible zeroize-on-drop path. The feature-graph check cannot establish cleanup of
    these locals.

  - **X25519 and XChaCha20-Poly1305:** the active `x25519-dalek` `zeroize` edge enables drop wipes
    for `StaticSecret` and `SharedSecret`. `SharedSecret::to_bytes()` makes an ordinary array copy;
    Holonomy wraps its copies in `Zeroizing` at the envelope call sites. This is not proof that
    every curve-operation stack intermediate is erased. The pinned AEAD wipes its stored key on
    drop, its local Poly1305 key after MAC-state construction, and the ChaCha core state under the
    feature. A direct `poly1305/zeroize` edge is required because `chacha20poly1305` 0.10.1 does
    not forward that optional feature; its state now has a feature-gated drop wipe. In the used
    XChaCha path, `hchacha`'s stack state and the returned local derived-subkey copy have no
    explicit wipe before return/drop.

  - **SHA-3/SHAKE:** direct `sha3/zeroize` and `shake/zeroize` edges enable feature-gated state
    and reader-state wiping; the `sponge-cursor` implementation itself zeroizes only its position
    field. `digest` 0.11.3's `zeroize` feature weakly forwards the feature to `block-buffer` when
    that optional dependency is present; `block-buffer` 0.12.1's gated Drop implementation wipes
    its internal buffer and cursor. The CI feature-graph check requires the SHA-3/SHAKE, digest,
    and sponge-cursor edges, along with Argon2, ChaCha20, curve25519-dalek, ML-KEM, Poly1305, and
    X25519; it passed after the lockfile correction. Feature activation is useful coverage, not
    evidence that every implementation or compiler-generated temporary is wiped.

  The first follow-up run, [44031374849](https://github.com/c4us4l1ty/holonomy/actions/runs/44031374849),
  exposed a stale `Cargo.lock`: the SHA-3/SHAKE feature-unified dependency resolution produced a
  `block-buffer` 0.12.1 package entry and disambiguated package edges, but the earlier manual lock
  update had omitted them. The lockfile now records that metadata (commit `8e796f7`). The earlier failed-run logs for
  [44031050938](https://github.com/c4us4l1ty/holonomy/actions/runs/44031050938) were unavailable;
  no cause is inferred for that run. CI [44031769120](https://github.com/c4us4l1ty/holonomy/actions/runs/44031769120)
  passed rustfmt, Clippy, the feature-graph check, workspace tests, the gates contract, and both
  L-6/L-10 rehearsals. Those two gates remain `spec`; Block 4 has not started. The next run,
  [44032166408](https://github.com/c4us4l1ty/holonomy/actions/runs/44032166408), exercised the
  maximum-input combining-sequence regression: its test, Clippy, gates, and both qualification
  jobs passed, while rustfmt requested a line-wrap correction. Diagnostics confirmed the failure
  was formatting-only. Commit `a190519` applies that correction and adds an explicit `digest/zeroize`
  graph assertion; CI [44033037131](https://github.com/c4us4l1ty/holonomy/actions/runs/44033037131)
  then passed rustfmt, Clippy, the strengthened feature-graph check, workspace tests, the gates
  contract, and both L-6/L-10 rehearsals. All code and feature-graph checks are green; L-6 and
  L-10 remain `spec` and Block 4 has not started.

  **Verdict:** the dependency-temporary zeroization gate remains open. Current bounded input and
  enabled feature edges narrow exposure and improve cleanup for selected retained states, but
  known stack/heap temporaries remain unwiped or not proven wiped. The registered-allocation test
  only checks project buffers explicitly registered with it; neither that test nor source review
  establishes universal secret erasure. See RFC-001's remaining zeroization question.

- **2026-11-16 — Block 2/3 CI repair and qualification evidence review (Block 4 not started):**
  CI run [44017105999](https://github.com/c4us4l1ty/holonomy/actions/runs/44017105999) passed
  rustfmt, clippy (`-D warnings`), workspace tests, the gates contract, and both qualification
  jobs; the diagnostics bridge was skipped because no job failed. The rustfmt failure from run
  44013849921 was actual formatting drift under pinned Rust 1.100.0 (the diagnostics reproduction
  also exited 1), not a toolchain mismatch. Clippy's three `needless_borrows_for_generic_args`
  findings were corrected with byte-slice borrows, avoiding by-value secret-array copies. The NFC
  expansion test now expects 1,026 bytes: the first two-byte mark from input 257 crosses the
  1,024-byte bound, and the bounded normalizer does not write the second mark. A first ext4-test
  repair still failed in run 44016177594 because the filler reported ENOSPC with 352,256 bytes
  still available. The final harness aligns the prior witness to the 4,096-byte block boundary
  and makes one append request more than the entire bounded filesystem using repeated borrowed
  64 KiB slices; it verifies the appender's actual ENOSPC error and recovery.
  - **L-6:** the [recorded ext4 result](https://github.com/c4us4l1ty/holonomy/issues/51#issuecomment-6938306377)
    used a 128 MiB image, ext4 on `/dev/loop0`, Linux 7.0.0-1017-azure x86_64, and e2fsprogs
    1.47.0. The filesystem reported 108,974,080 bytes total, 4,096-byte blocks, and 106,266,624
    initially free. The filler consumed 105,906,176 bytes and received ENOSPC on write, with
    352,256 bytes still available. The append requested 1,663 segments (108,986,368 plaintext
    bytes, more than the filesystem size) and returned OS error 28 (`ENOSPC`). Reopen recovered
    one committed segment, retained a 352,256-byte uncommitted tail, and verified that segment.
    This is evidence for that ext4 loop configuration only, not physical power loss, controller
    caches, or a supported-device matrix; **L-6 remains `spec`**.
  - **L-10:** the [passing rsync run](https://github.com/c4us4l1ty/holonomy/issues/51#issuecomment-6938309911)
    used rsync 3.2.7 / protocol 31 over a local daemon on TCP with a 65,536-byte block. Old/new
    file sizes were 60,093,554 / 60,356,483 bytes; the append was 262,929 bytes. Delta literal
    data was 325,507 bytes; client bytes sent/received were 329,363 / 5,545, for 334,908 total
    application-layer bytes. This remains one client/build/configuration/workload rather than a
    qualification of remote services or other clients; **L-10 remains `spec`**.
  - **CI trigger review:** `.github/workflows/ci.yml` contains a `pull_request` trigger. PRs run
    the read-only validation jobs; `diag`, `sync`, and `filesystem` are explicitly push-only
    before their issue-writing steps can run. The earlier check that reported no PR trigger was
    a false negative. The diagnostics bridge now captures each command to a file before capping
    displayed output, preserving its actual exit status instead of risking SIGPIPE from `head`.
  The allocation audit still does not cover every dependency-owned temporary, so RFC-001's
  dependency-temporary zeroization gate remains open.

- **2026-11-16 — follow-up Block 2/3 review (implementation pending CI):** found and corrected
  five concrete boundary defects: Linux `MADV_DONTDUMP` used the wrong advice value and did not
  page-align the common small heap-cell range; `LockedSecret` declared its boxed bytes before the
  lock guard, so field drop order could free the allocation before `munlock`; page-granular
  `munlock` calls on separate tiny heap boxes could unlock a page still holding another key; the
  public `HybridKem`/`MlKem1024X25519` test abstraction could return `SharedSecrets` with raw key
  bytes outside the capability API; and `x25519-dalek` had default features disabled without
  enabling its optional `zeroize` feature, leaving `StaticSecret`/`SharedSecret` drops unscrubbed.
  The KEM plumbing is now crate-private, the lock advice is page-aligned and uses libc's Linux
  constant, each secret cell gets a separate 64 KiB-aligned lock unit with the guard dropping
  before its allocation is freed, and the X25519 `zeroize` feature is enabled and locked.
  The threat model/RFC wording was also corrected: `memfd_secret` is probed on supported Linux
  architectures but not used, and there is no global core-dump disable or `DONTFORK` implementation.
  Added a test-only registered-allocation
  wipe audit, AEAD/envelope corruption tests, an ignored rsync transfer harness, and an ext4-loop
  ENOSPC/recovery rehearsal. Follow-up review also fixed the private `MlKem1024X25519` import,
  wrapped ML-KEM shared-key/seed copies, and changed AEAD encryption to use wipe-on-drop
  in-place buffers. A further passphrase review replaced `NFC.collect::<String>()` with a
  pre-sized, wipe-on-drop buffer and bounded pushes, avoiding growth reallocations before the
  guard exists; an NFC-expansion regression test covers the size boundary. At that point, the
  Rust changes, tests, and manual lockfile feature edges had not been compiled or independently
  validated; workflow YAML parsing and shell syntax checks passed. The allocation test does not by
  itself audit every dependency-owned temporary; RFC-001's zeroization gate remains open. Local
  execution was blocked because `cargo`, `rustc`, `rsync`, `mkfs.ext4`, and QEMU were absent;
  invoking `scripts/qualify-ext4-enospc.sh` exited 127 at line 20 (`mkfs.ext4: command not
  found`). No filesystem ENOSPC result or rsync measurement had been collected yet. The ext4
  rehearsal cannot emulate physical power loss or controller caches, and the rsync run is limited
  to its measured version/configuration/workload; keep L-6 and L-10 `spec` pending evidence review.

- **2026-11-16** — Block 3 fault-injection and tail-write evidence: routed append data, witness,
  sync, and recovery truncation through private I/O wrappers with `cfg(test)` failpoints only.
  Tests inject a short write after every byte of a two-segment append, fail the data and final
  witness sync barriers separately, and fail both tail truncation and its sync. Each uncertain
  operation poisons the handle; reopen/verify finds only the last complete authenticated witness
  or a complete authenticated new witness. A 30-million-word simulated file now records the
  completed byte counts returned by the appender's actual `File::write` calls and offsets sampled
  immediately before them; writes begin at the previous EOF, total `AppendReceipt::bytes_appended`,
  and preserve the old prefix.
  This is local writer evidence, not physical disk-sector or backup-network measurement. Injected
  errors are returned before OS sync/truncate operations; no real ENOSPC, filesystem failure,
  process/device power cut, or filesystem/hardware durability qualification was run, so **L-6
  remains `spec`**. No concrete backup client was measured, so **L-10 remains `spec`**. OQ-7's
  valid-prefix rollback limitation remains accepted and open. CI run 43926260051 first caught
  Clippy's `missing_const_for_thread_local`; fixed in `58ee6ac`. CI run 43926666233 then passed
  rustfmt, clippy, workspace tests, and the gates contract.

- **2026-11-16** — Block 3 implementation draft: added the fixed `.wavefunction` header,
  fenced KDF replay, sealed segment frames, MMR witness commits, lazy body reads, explicit
  passphrase-confirmed compaction, and same-directory atomic publication. Tests specify
  every-byte append-tail cuts, 30-million-word simulated growth with exact-prefix preservation,
  historical metadata/body tampering, stale-writer refusal, shared-open/exclusive-append lock
  contention, no-replace creation/compaction, and compaction behavior. Local Rust tooling is
  absent; the first CI run (43922948005) found a missing `Debug` derive used by MMR tests, fixed
  in `50ac36f`. CI run 43923405534 then passed rustfmt, clippy, workspace tests, and the gates
  contract. Block 3 remains a draft, not a passed L-6/L-10 gate: the initial crash matrix used
  synthetic byte-prefix truncations only, which did not establish real power-loss or filesystem
  failure behavior. `metadata_digest` uses variable-output BLAKE2b-256 rather than truncating
  BLAKE2b-512; its reference-vector regression test passed in workspace CI.

- **2026-11-16** — Block 2 residuals closed. **OQ-5**: Argon2 now runs exclusively through
  `kdf::run_argon2`, which derives over a caller-owned `Arena` whose `Drop` zeroizes the
  whole m-cost slab before free (argon2 `zeroize` feature enabled at the workspace pin);
  byte-equality with argon2's internal-allocator output is test-pinned so the scrub layer
  can never silently alter derivations. **OQ-6**: `derive_root_secret` carries the
  `TestScheduleRefused` guard, active exactly in non-debug non-test builds; the nil-profile
  twin keeps call sites identical. The replay path (`derive_root_secret_with` →
  `MasterKeys::derive_with`) gained its first consumer plus a pairwise test pinning
  `derive == derive_with` for equal parameters — the cross-machine portability contract
  Block 3's header will rely on.

- **2026-11-16** — Block 2 implementation + RFC-001 attack cycle. Verdicts: **AC-1**
  (capsule replay across files) → amended — RFC-002 §6 makes the witness commit to the
  envelope epoch; Block 3's gate gains a cross-file open-refusal test. **AC-2** (argon2
  arena wipe) → open as OQ-5 (v5's fix is the port reference). **AC-3** (wrong-passphrase
  distinguishability) → survived — KDF draws no USB material, the `UnwrapCapsule`
  commitment runs before any KEM at fixed schedule, and `OpenRejected` stays coarse with
  tests pinning indistinguishability. **AC-4** (USB slot malleability) → survived — the
  presence flag sits inside both the unwrap context and the commitment's hashed material,
  and the tests pin the cross-state refusal. **AC-5** (`ParamsClass::Test` misuse) → open
  as OQ-6. **AC-6** (Elligator absence re-attack) → survived — no Montgomery-format
  Elligator2 exists in the dalek ecosystem; the capsule remains sealed inside the wrapped
  slot. Corruption-matrix tests (garbage capsule, wrong-key fast-no, bit-flip, wrong
  offset, wire truncation) run in-tree. Allocation hygiene is by construction today:
  capability tokens are fixed arrays behind zeroize, passphrases are callback-scoped
  `Zeroizing<String>`, and calibrated parameters are plain records; the heap-proof
  counting-allocator pass is scheduled for Block 9.

- **2026-11-16** — Block 1 ground laid: workspace unified (the v5 fork-exile failure closed),
  gates machinery in `holonomy-ledger`, this protocol instituted, RFC-001…004 filed as
  attack targets, UI spec filed *before* UI code (the H1 inversion corrected).
- **2026-11-16** — Block 1 gates **passed** (CI all green). Process notes recorded for later
  blocks: (a) counting-allocator proofs must live in binaries that *install* the allocator —
  the first unit-test version measured nothing and CI caught it; the same honesty rule
  applies to every future gate; (b) the diagnostics bridge (issue #51) remains in CI,
  dormant unless a step fails.
