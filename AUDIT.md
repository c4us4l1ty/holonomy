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
| OQ-3 | Clipboard egress on each OS: flag-confidential + clear-on-blur must be verified per platform (Block 9) or labeled unverified in the Security panel. `Editor::copy_selection`, `holonomy-gui/src/canvas.rs` focus-blur zeroization, Cocoa `NSPasteboard` `TransientType`/`ConcealedType` clearing, and Win32 `ExcludeClipboardContentFromMonitorProcessing` + `EmptyClipboard` clearing are wired in the desktop window backends while X11 ICCCM clipboard ownership & remote/compositor policy cannot suppress third-party clipboard watchers and is labeled honestly in the Security panel (`window_security`). | **open — in-window + Cocoa/Win32 clear-on-blur implemented (`D-94`/`D-95`); X11 ICCCM server policy disclosed open** |
| OQ-4 | Scroll-timing side channel: slab fault-in pattern leaks *where* you read, to whoever sees slab access timing. | **closed 2026-11-17 — accept and disclose.** The pattern is already visible at the I/O layer: fetching an old page reads its ciphertext at a known file offset, so any observer who can time slab faults can observe the reads themselves. Smoothing would buy nothing and would spend L-5 (idle is idle) on traversal cost the architecture exists to delete. Residual recorded in `docs/THREAT-MODEL.md` §5. |
| OQ-5 | Argon2's internal working arena: `MasterKeys::derive`, `kdf::derive_root_secret*`, and the calibration probe all allocated the m-cost arena inside `hash_password_into` and freed it unwiped (v5's H1 hit exactly this and switched to caller-owned memory + a scrubbed block). Fixed by making the arena caller-owned everywhere: `kdf::Arena(Vec<argon2::Block>)` zeroizes the slab on Drop via argon2's `zeroize` feature, and the single `run_argon2` runner serves creation, replay, *and* the calibration probe, with a test pinning byte-equality against argon2's own allocator | **closed** 2026-11-16 |
| OQ-6 | `ParamsClass::Test` misuse guard: a release build could derive a user container at the weakened schedule, and replayed records had no fence. Closed at both boundaries: `derive_root_secret` refuses `Test`; `derive_root_secret_with` refuses below-floor schedules and accepts only records emitted by the calibrated production selector; Block 3's header rejects weak memory, out-of-range memory, non-step-aligned memory, or unbounded time/lanes **before Argon2 runs**. Test/debug profiles admit only the exact test tuple. | **closed** 2026-11-16 |
| OQ-8 | Undo depth as memory: `History` keeps one `Spine` root per state. Roots share subtrees, so the cost per edit is O(log N) nodes, but an unbounded history over years of typing still grows with the **number of edits**, not with the document — and the world-line on disk already holds that history. **Answered in the session by D-14 and D-15:** a length is a `usize` and a root is a pointer, and both are rings — 1024 lengths, 64 roots — so history memory is `1024 words + 64 × O(log leaves)` and does not grow with the number of edits. What that buys is stated exactly: undo depth is 64 destructive actions back, and a seal ends the epoch. Deeper history is the world-line's job (Block 8: reconstruct a root from the log), which is named and not built. | **closed 2026-11-17 for the session — bounded rings; deeper undo is Block 8** |
| OQ-7 | Valid-prefix rollback: a byte-perfect earlier witness prefix is indistinguishable from a file that legitimately stopped there without trusted state outside the file. RFC-002 amended to claim integrity of present bytes and crash consistency, not freshness against full-prefix rollback. An external transparency/monotonic anchor would resolve this but cannot be mandatory under passphrase portability (L-9). | **open — accepted limitation, not locally solvable** |
| OQ-10 | A full resident slab has **no continuation policy** — solved by D-7 and built as D-8/D-9: a leaf names a *source*, and a source is **open** (bytes in a resident buffer) or **sealed** (bytes in the world-line), at the same address. Text being typed is a leaf from its first byte, so `byte_window` and the height index see it; a seal appends the bytes once, renames the source, and corrects one leaf's length in the current root — **no history entry**, so undo never steps through a seal; each byte is written once. `Typing::insert_at` handles mid-document typing by cutting the leaf the caret lands in (pure geometry — the halves name adjacent ranges of one segment, so nothing moves). Deletion is D-11 (a pointer operation; the bytes stay in the log — OQ-14). Not yet: selection. | **closed 2026-11-17 — `typing.rs`** |
| OQ-9 | Leaf fragmentation: a split that lands inside a leaf leaves the segment spread over two leaves, and `join` never coalesces adjacent fragments of the same segment. **Narrowed 2026-11-17 by D-3** (commit boundary leaves leaf count unchanged), **2026-11-19 by D-29** (load-time `Spine::from_slabs` coalesces adjacent same-segment slabs), and **2026-11-25 by D-94** (live-session `Spine::coalesce`, `Typing::coalesce`, and `Editor::coalesce` explicitly merge contiguous same-segment leaves, invalidate `holonomy-shell` layout heights once, and mark `structure_dirty` so coalesced manifests persist). Residual: automatic seam-healing inside every mid-keystroke `join` remains deliberately deferred rather than mutating height indices on every edit. | **closed for explicit & load-time coalescing 2026-11-25 (D-29, D-94) — automatic per-edit `join` coalescing deferred** |
| OQ-11 | **Cluster segmentation and UAX #9 bidirectional reordering: both halves are UCD 18.0.0 data + specification rules, and L-8 holds across LTR, RTL, and mixed text.** Segmentation classifies each code point into a property and applies **GB3–GB13** (including Unicode 15.1's GB9c conjuncts, `D-27`, `D-28`) over generated UCD 18.0.0 range tables. **Closed 2026-11-22 (D-77):** `crates/holonomy-eye/src/bidi.rs` and generated `bidi/tables.rs` (`scripts/gen-bidi-tables.py`: `766` `Bidi_Class` ranges + `130` `Bidi_Paired_Bracket` entries) implement O(N) UAX #9 resolution (`P2`–`P3`, `X1`–`X10`, `W1`–`W7`, `N0`–`N2` with stack-based `BD9` and canonical angle-bracket pairing, `I1`–`I2`, `L1`–`L2`), verified offline against all `91,707 / 91,707` `BidiCharacterTest-18.0.0` vectors. Wrapped lines reorder visually and pair every logical boundary bijectively onto the line's `(m + 1)` visual boundary slots so `hit_test(paint_point(g)) == g` holds without duplicate `x` ties; logical selections that cross direction runs split into disjoint visual `SelectionSpan`s, and `holonomy-shell` accumulates multi-leaf `Carry.x` in logical byte order. Enforced on every push in `crates/holonomy-eye/tests/script_gate.rs` (`carets_inside_a_grapheme = 0`, `displaced = 0`, `L-8` `27,680` boundaries / `0` disagreements; CI run `45106234334` on `3b54969`). Residual: OpenType `GSUB`/`GPOS` contextual glyph joining remains `OQ-24`. | **closed 2026-11-22 (D-77) — UAX #9 visual order (`displaced = 0`), split selection, and L-8 bijection enforced in CI; OpenType `GSUB`/`GPOS` shaping remains OQ-24** |
| OQ-12 | **The window is viewport-sized, and both its structural counts and wall-clock latency with a real OpenType face are invariant in document length.** `HeightModel`, `Spine::slab_span`, `byte_window`, `window_text`, and `holonomy-shell` bound layout inputs to one screen plus two leaves across 44,000 leaves (`D-18`, `D-19`). **Closed 2026-11-22 (D-78):** `crates/holonomy-eye/tests/viewport_latency.rs` times the full viewport pipeline (`HeightModel::window` → `byte_window` → `window_text` → `layout_styled` with the real `Face` over `Inter-Regular.ttf`) interleaved across 256 samples each at 180 KB offset 0, 180 MB / 30 M words / 43,946 leaves at offset 0, and 180 MB near the end, alongside a 128-leaf control in the same process (first measurement CI run `45106234334` on `3b54969`: `p50 ≈ 697 µs` at all three offsets, control `53,041,922 ns`, `76×` slower). **The absolute number is the runner's, not the code's** — two runs sixteen minutes apart on 2026-11-24 measured `p50 = 696,731 ns` and `1,045,075 ns` for the identical workload (D-91) — so the claim rests on the invariance across the three offsets and on the control ratio (`76–90×`), both of which reproduce on every run. | **closed 2026-11-22 (D-78) — real-`Face` wall-clock invariance at 30 M words enforced in CI; absolute latency restated as a range 2026-11-24 (D-91)** |
| OQ-13 | **How quiet-md enters the tree — decided: as source, not as a dependency.** Cargo ignores a dependency's `[patch.crates-io]`, and quiet-md's app code calls API that only its 43 GPUI patches add, so it is not consumable as a dependency at all; reproducing the patches at our root is the fork-in-the-core-tree Block 1 forbids, and exiling the UI to a second workspace is the other shape it forbids. quiet-md is therefore prior art and an attributed source of MIT code (`THIRD_PARTY.md`), with GPUI pinned upstream and no patch section. The cost is named in `docs/UI-SPEC.md` §8: CJK/bidi line breaking and font fidelity are ours to earn, and quiet-md's patches #4/#23/#30/#31/#32 are the reference for the first of those — the same gap OQ-11 records. | **closed 2026-11-17 — source, not dependency** |
| OQ-14 | **Deletion does not erase on an ordinary save; explicit compaction rewrites the active spine.** `Typing::delete` removes a range from the tree and, on save, commits a sub-segment `RECORD_MANIFEST` (`OQ-29`, `OQ-30`) so deleted text never reappears on reopen, while the old ciphertext stays in the append-only `.wavefunction` by design (Vision III). **Closed for user-reachable compaction 2026-11-22:** `WorldLine::compact_active_to`, `Document::compact_to`, and CLI `holonomy compact <src> <dst>` re-confirm the passphrase via `confirm_compaction` and write a fresh, re-keyed world-line containing only the active `document_slabs()` with at most one segment plaintext resident at a time (`smoke-install.sh` + `persist.rs`). Residual by constitutional design: until the user runs `holonomy compact` (and prunes any external cloud mirror copies, `OQ-2`), deleted spans remain encrypted in the pre-compaction log. | **closed 2026-11-22 — `compact_active_to` + `holonomy compact`; pre-compaction ciphertext retention is disclosed design** |
| OQ-15 | **Character boundaries are a precondition the storage layer cannot check.** `Typing::insert_at` and `Typing::delete` take *document* offsets, and a cut or a split that lands inside a multi-byte character leaves a leaf whose bytes are not valid UTF-8. The asymmetry was the finding: offsets inside the open buffer are checked (`SlabText` returns `NotACharBoundary`), and offsets in sealed text are not, because the session holds a `SegmentSink`, which appends and never reads. **Narrowed by D-18:** the reader refuses a window whose edge falls inside a character, naming the offset. **Closed for the session by D-19:** the check OQ-15 asked for exists — `holonomy-shell` holds the reader and the edit in one place, and its test asserts over every caret every frame offers that the byte is a character boundary of the frame's text and hit-tests back to itself. What remains is not a missing check but a missing *type*: the offset is still a `u64` a caller can get wrong, and a caller that bypasses the Eye still asks for a mid-character cut — which is now refused at read time instead of silently rendered. Making it impossible would need an offset type the Eye alone can mint, which is a Block 6 API decision, not a hole in this one. | **closed 2026-11-18 — the caret check passes; a bypassing caller is refused at read** |

| OQ-16 | **A leaf's height is measured apart from the line it is in, so the scroll bar counts a line the document does not have.** `reflow` laid each leaf out alone — necessarily, because measuring the continuous document would mean decrypting it — so a paragraph continuing across a boundary broke there in the measurement, and the per-leaf sum exceeded the continuous layout's height by up to one line per crossed boundary. **Closed by D-20**: the line is carried across the boundary as two words (the x the next cluster sits at, and whether the line began earlier), `layout_styled_from` lays a piece out as the continuation of a line, and `reflow`'s total now equals one continuous layout of the same text — proven by a test that lays the whole document out once. The fix also closed a bug that had not been written down: a frame used to begin at a leaf, which re-wrapped the paragraph from a point the document never breaks at. | **closed 2026-11-18 — the partition is exact, and frames begin at lines** |
| OQ-17 | **A window that begins at a leaf decided its first cluster without the text before it.** It made a boundary the document does not have into a caret position, and stopped backspace/delete at the leaf edge. **Closed by D-22:** the UAX #29 state (last property, GB11 run, regional-indicator count) is carried beside the line position — `Clusters::continuing`/`carry`, `Layout::carry`, `layout_styled_continuing` — so a piece that opens inside a cluster segments as the document does, and its first line offers no boundary there. A frame was never affected: it begins at a line, and a line break is between clusters (D-20). The *advance* of a split cluster is OQ-18, measured, not assumed away. | **closed 2026-11-18 — segmentation is a function of the text and the state carried into it** |
| OQ-18 | **A cluster split by a leaf cut pays its advance twice.** The eye shapes one cluster at a time, so a cluster whose bytes fall either side of a leaf cut was advanced on both sides: `e` + U+0301 is one unit wide in the document and two when a 64-unit wrap and a 64-byte leaf put the cut between them, which is one extra line in the scroll bar. **Closed by D-25:** the leaf the cluster starts in places it whole — a tail read of the leaf after the cut, a `skip` in the carry, and the next leaf's geometry beginning after the bytes it did not place — so one cluster costs one advance. The frame path computes the same skips from its own window, so the two measurements agree. | **closed 2026-11-18 — residual: a cluster longer than the leaf after the cut keeps the double advance, stated not hidden** |
| OQ-19 | **A piece laid out without knowing what follows it offers its end as a caret, and shapes its last cluster short.** The eye sees text, not a document: a window's last byte can be inside a cluster, and `Line::boundaries` offers it while `Shaper::advance` was handed only the cluster's first part. **Closed for this app by D-26** (the frame reads the cluster's tail with the window, so the piece it hands the eye ends on a cluster) and by D-23's `legal_caret` for what the frame cannot finish; the advance half was D-25. What remains open in *principle*: another caller of `View::hit_test` on a piece it truncated itself still gets the piece's end. | **closed 2026-11-18 for the only caller there is — residual: a cluster longer than the leaf after the cut, and any external caller that truncates its own piece** |
| OQ-20 | **GB9c was not implemented, so an Indic conjunct was two clusters.** Unicode 15.1's rule — `InCB=Consonant [InCB=Extend InCB=Linker]* InCB=Linker [InCB=Extend InCB=Linker]* × InCB=Consonant` — makes KA + VIRAMA + SSA one cluster in Devanagari and the same shape one in Bengali, Gujarati, Oriya, Telugu, Malayalam and the historic scripts; without it the seam between the consonant and its conjunct was offered as a caret position, in a script `docs/UI-SPEC.md` §8 names. Not a misread rule but an absent one, which is the class of gap no test can fail — hence the row, and hence building it rather than pinning it again. **Closed 2026-11-19 (D-28)**: the two properties are generated (`InCB=Consonant`, 78 ranges; `InCB=Linker`, 23 code points), `InCB=Extend` is derived from the break class by the standard's own formula, and the run is state in `Context`, so it travels across a leaf cut like every other left context. Residual: `unicode-segmentation` suppresses the break before a consonant on having seen *any* linker, where the standard's left context must be rooted in `InCB=Consonant`; this reads the rule, so `a` + VIRAMA + SSA is two clusters here, and a test pins it. | **closed — 2026-11-19, D-28** |
| OQ-21 | **Redo was not always the inverse of the undo it undid.** Found by D-30's runner, 2026-11-19; fixed by D-33 the same day. Unwinding the retained stack and rewinding it reported a document one byte short: seed 2, twenty thousand actions, 409 bytes expected, 408 returned, first difference at byte 260 (the region of `delete(260, 261)` + `insert(260, 1)` with a `mark`/`insert` pair above it, and a root recorded before a cut as the step being restored). Undo and redo were exact for *marks* — a mark is a length, and the bytes stay in the buffer — and wrong only for a **root**: `leave` records the state to come back to as `settled_spine()`, the tree without the open leaf, because a root names written bytes only; the undo is right to drop them (they were typed after the cut) but the redo then re-applied the cut to a document that had already lost them. The fix is D-33: `Step::Root` carries a copy of the bytes that were open, and `restore_root` re-attaches them under a new provisional id, so undo then redo is a no-op. | **resolved 2026-11-19 by D-33** for this reproduction — the walk no longer reports seed 2 at byte 260; unit test `redo_of_a_cut_puts_back_the_bytes_that_were_open`. The same walk then found OQ-22, and the million with undo found OQ-23 |

| OQ-22 | **A redo put the bytes an undo took back in the wrong place.** Found by D-30's walk, 2026-11-19, on the first run that carried D-33. Seed 0, redo five: 276 bytes where 275 were expected, first difference at byte 113, `want="long enough 漢ke.ehe "` against `got="long enough e漢ke.ehe"`, `pending=1 open=1 spine=276 slabs=75`, with the ring naming `delete(113, 385)` then `insert(113, 1)`. Two causes, and only one of them was in `Typing`: `restore_mark` applied a live mark's *length* to whatever leaf was open, ignoring the offset the mark recorded, so a leaf that had moved was restored in the wrong place; and the walk itself subtracted the open span from its first state, an exception a root's redo used to force, which after D-34 is a one-byte lie. Instrumenting `leave` with the arithmetic it depends on (`settled + pending == document`, and the open leaf really in the tree at its offset) is what separated the two. | **resolved 2026-11-19 by D-34** — the walk passes for six seeds without the exception, and the million keystrokes pass with undo and redo in the mix |
| OQ-23 | **`Typing::pending` sliced past the end of its buffer.** Found 2026-11-19 by running the million keystrokes with undo and redo in the mix: `range end index 8 out of range for slice of length 3` at `pending()`, after about 165 s. `live_len()` falls back to `open.registered` when the overlay has nothing to say, so a state left behind by the paths that clear or shrink the buffer — a root restore cleared it, and re-attached only what was open — could claim more bytes than the buffer held, turning a length disagreement into a panic in a public accessor. | **resolved 2026-11-19 by D-34** — a root restore keeps the buffer, so nothing it does can leave a length over bytes that are not there; `len <= buffer.len()` is asserted in `attach`, `set_live` and `pending`, and the million keystrokes with undo and redo in the mix now pass |
| OQ-29 | **The world-line now authenticates sub-segment document geometry via `RECORD_MANIFEST` + Witness v2.** Closed 2026-11-22: `holonomy-line` adds `RECORD_MANIFEST = 3` (`LineSlab { segment: u64, start: u32, len: u32 }`) and Witness v2's 44-byte `ManifestRef`. Append-only world-lines continue writing version-1 witnesses with zero manifest overhead; mid-segment insertions, reorderings, and cuts atomically seal a manifest and reference it in the witness. `WorldLine::open` seeks over all segment bodies and superseded manifests (`GuardedReader`-enforced) and opens only the active manifest. Pinned in `holonomy-line` (including byte-by-byte `AppendIoProbeGuard` short-write recovery) and `holonomy-gui/tests/persist.rs`. | **closed 2026-11-22 — `RECORD_MANIFEST` + Witness v2** |
| OQ-30 | **Deletions and post-seal undos persist across reopen with zero segment rewrites.** Closed 2026-11-22: `Typing` tracks `structure_dirty` across `delete` on sealed text and `restore_root`, and flushes the exact `Slab` spine via `SegmentSink::{append_segment_with_spine, commit_spine}` without reading or rewriting surviving segments (preserving L-2 and L-6). Emptying an entire document (`Select All` + `Backspace`) commits an explicit zero-slab manifest so reopen yields `""` instead of falling back to append order, and `App::has_pending()` includes `has_uncommitted_structure()`. Pinned in `typing.rs` and `holonomy-gui/tests/persist.rs`. | **closed 2026-11-22 — zero-rewrite spine manifest commits** |

## Audit trail

- **2026-11-27 — D-101 — Bug hunt across the D-100 chrome: two user-visible defects and three latent ones.**
  After D-100 landed on `main`, the whole app was re-read for defects rather than only the new
  lines. CI was green throughout, so every finding below is a runtime/logic defect rather than a
  build failure, and each got a regression test instead of a prose claim.
  - **The shortcut sheet showed 4 of its 14 entries.** `render_shortcut_help` placed seven rows in
    *two columns of `268 px`* inside a `560 px` panel, so ten entries were painted outside the
    panel's clip and silently dropped. The grid is now derived from the panel width by
    `shortcut_help_geometry` (one column below `520 px`, two above), and the sheet returns both its
    panel and close rect. Pinned by `shortcut_sheet_shows_every_entry_inside_its_panel` at four
    window sizes.
  - **Chrome ink was invisible on dark paper.** The D-100 rail, breadcrumb and sheet reused
    Home's *light* tokens (`HOME_INK #1f2225`, `HOME_MUTED`, `HOME_FAINT`, `HOME_SELECTION`,
    `HOME_WASH`) over `QUIET_MD_DARK_THEME.bg_body #1a1a19` — a measured **1.09:1** contrast, i.e.
    the folder rail was effectively blank in dark mode. All writing-view chrome now selects ink
    through `ink_for`/`muted_ink_for`/`accent_ink_for`/`faint_for`/`soft_fill_for`/`selection_for`;
    light paper still resolves to exactly the quoted quiet.md tokens. Pinned by
    `chrome_ink_stays_readable_on_both_papers` (WCAG ratios, both themes) and
    `dark_paper_rail_paints_inside_its_rect`.
  - **The sheet was pointer-modal but not keyboard-modal.** With it open, typed characters reached
    `app.press(...)` and edited the document underneath; the rail's `Shortcuts` row could also be
    toggled shut over a live find/link bar. The sheet now swallows every key except `Esc`,
    `Ctrl /`, and `Ctrl Q`/`Ctrl C` (quit, matching the pointer-modal behaviour), and opening it
    from either the rail or `Ctrl /` clears an anchored find/link bar first.
  - **New documents from the rail landed outside the library.** Home's `+` suggests a unique path
    inside `HOLONOMY_NOTES_DIR`; the writing view's equivalent fell back to `notes.wavefunction`
    in the process working directory, which Home then could not see. Both now share
    `home_new_document_path(&home_library_root())`.
  - **Documentation debt:** the Home token block carried two stacked doc comments (a `///` block
    followed by `//`), which reads as a stray comment on the constant; it is one block now.
  - **Also restored: `Ctrl+/` in the README.** D-100 removed that row from `README.md`'s shortcut
    list while keeping the sheet, so the documented set and the shipped set disagreed; the README
    documents it again.
  - **Kept as designed:** the rail's gradient stays light-paper only — reading the user's wallpaper
    to emulate quiet.md's real frosted glass would be an OS-surface dependency with no security
    benefit — so dark paper paints the theme's own chrome colour instead (`docs/UI-SPEC.md` §10.1).
  - **Found and deliberately not changed — `⋯` overflow menu can drop rows.** When the window is
    too short for every collapsed toolbar action, the packing loop `break`s and the remaining
    actions have no pointer target for that frame (`Scale`/theme/motion stay reachable by shortcut,
    and undo/redo by `Ctrl Z`/`Ctrl Y`). Fixing it means re-deriving the menu height from actual
    row packing, which cannot be verified in this sandbox (no Rust toolchain, no display), so it is
    recorded here rather than shipped as an unexercised layout rewrite.

- **2026-11-27 — D-100 — quiet.md chrome parity in both views, with the laws kept load-bearing.**
  The user asked for the UI of [`s1dashu/quiet-md`](https://github.com/s1dashu/quiet-md) across
  the writing view *and* Home, and asked explicitly that security and resource usage not be
  traded away. `docs/UI-SPEC.md` §8 already made quiet.md's page tokens canon and OQ-13 already
  decided that its code is source, not a dependency; this pass extends the quoted look to the
  chrome without importing any of its machinery.
  - **Built:** a 216 px frosted folder rail and a 40 px breadcrumb in the writing view; Home's
    rail, stacked folder cards and library breadcrumb; quiet.md ink/accent tokens shared with
    the drift-guarded `QUIET_MD_THEME`; a static `Ctrl+/` shortcut sheet (quiet.md's `⌘ /`).
    Clicking a folder in the rail returns to Home with that folder selected, then leaves the
    document session — no second file is open at once.
  - **Attacked and answered.** *Can chrome widen the plaintext set?* No: `DocumentChromeData`
    has no `Document`, `Spine`, `SegmentSource` or text buffer and is built only from file and
    folder names, so the rail cannot read a sealed segment. *Can chrome move the caret away from
    the glyph?* No: `PixelSurface::recompute_geometry` reserves rail and breadcrumb before
    deriving `page_origin`/`canvas_clip`, and the existing L-8 caret-click test still passes.
    *Can chrome add idle work?* No: all of it paints inside the existing `pending_paint ||
    app.dirty()` branch, with no timer and no per-keystroke work. *Does it add a dependency,
    network call, telemetry or desktop capture?* No: the frosted wash is interpolated in the
    existing framebuffer; no wallpaper pixels are read. The security dot, status strip,
    witness-history and wipe affordances remain visible rather than being hidden for a closer
    screenshot.
  - **Refused, with the reason recorded in `docs/UI-SPEC.md` §10:** multi-document tabs or an
    in-session world-line switcher (one document per window), whole-document outlines or note
    snippets (L-2), a second `Editor`/`Document`, whiteboard/images/tables/math, CSS themes or
    downloaded fonts, telemetry, per-keystroke Markdown auto-formatting (L-2's allocation-free
    keystroke gate), and extra style packs (one pinned token table). These are quiet.md features;
    Holonomy's law wins where they conflict.
  - **Evidence and its limit:** 47 `holonomy-gui` library tests pass locally, including four new
    guards for token drift, hit-test origin, metadata-only painting inside the rail's rect and
    the Home sidebar `+` hit order (which the map of the Home shell found was swallowed by the
    library row); `cargo clippy -p holonomy-gui --all-targets -- -D warnings` is clean. A local
    framebuffer render was inspected at 1280×800 for both screens. That is inspection, not the
    screenshot-diff acceptance check §8 requires for a Block 6 gate, and no gate status changes
    here: L-4/L-5 remain `spec`, and no law was promoted by editing prose.

- **2026-11-26 — D-99 — Bug hunt: a committed style manifest could reopen with its runs gone.**
  `WorldLine`'s `validate_manifest_against_lengths` accepted list wire values that
  `holonomy_text::DocStyle::from_wire` rejects as a whole: a bullet or ordered run with `depth = 0`,
  a bullet carrying an ordinal, or plain text carrying an ordinal. It also accepted a 1-based
  `link_id` one past the end of the link table. Since `manifest_to_spine` drops any run the decoder
  refuses, all of those let an authenticated commit reopen with *less formatting than it wrote* —
  the entire run, not just the bad attribute, disappeared. The commit gate now checks the exact
  tuple of list kind/depth/start and the 1-based link range before writing a witness. A regression
  test in `holonomy-line/src/tests.rs` commits the valid combinations and refuses each invalid
  one. Found by reading the container/text boundary during the app-wide bug hunt, not by a failing
  test; local Rust tooling is unavailable, so verification is by GitHub Actions on this branch.

- **2026-11-26 — D-98 — Bug hunt: interrupted UTF-8 no longer swallows a keystroke, and a shrunk
  terminal no longer positions the caret outside its grid.**
  - `holonomy_gui::term::Decoder` used to accept *any* byte as the continuation of a partial UTF-8
    character. A lead byte without its continuation — a paste cut mid-character, a serial line
    that dropped a byte — therefore absorbed the next real key until `expect` was reached and
    reported `Ignored`: Ctrl+S never saved, Enter never confirmed, a letter never typed. D-56 had
    already fixed the special case where that byte was `ESC`; the general case is now fixed too by
    accepting only `0x80..=0xbf` as continuation bytes and handling any other byte as a fresh
    token. Regression tests cover a control byte, printable ASCII, Enter, an intact multi-byte
    character and a character split across two reads.
  - `screen_with` checked the caret's row before emitting an ANSI cursor-position escape but not
    its column. After `CellSurface::resize` shrank the window, a caret left outside the new grid
    was clamped by the terminal into the wrong cell. The renderer now omits an off-grid caret;
    the next frame that can show it positions it. Both defects were found by reading the input and
    rendering paths, not by a failing CI job, and both are pinned by tests in `term.rs`.

- **2026-11-26 — D-97 — Binary-level SIGKILL recovery evidence, and the last doc contradictions
  reconciled.**
  - `scripts/drive-pty.py` gained an explicit, recorded `SIGKILL` (`--kill-after`, `--expect-kill`)
    distinct from its deadline kill, so a crash a test asked for cannot be confused with a hung
    child. `scripts/qualify-crash-recovery.sh` drives the shipped binary through a real pty, types
    a distinctive sentence, commits it with `Ctrl+S`, kills the process with `SIGKILL` at three
    different points after the commit, reopens the same file, asserts the sentence is on screen and
    runs `holonomy verify`. The `smoke` CI job runs it against the same release binary it already
    smoke-tests and posts `HOLONOMY_CRASH_RECOVERY` beside the smoke evidence. This is process-death
    evidence for committed witnesses only: it does not emulate a power cut, controller caches, a
    torn sector or an arbitrary kill in the *unsaved* window, and **L-6 stays `spec`**.
  - `docs/TO-APP.md` said M8 had only a Linux artefact and that `L-9` still needed a second machine,
    while `release.yml`, `GATES.md` and AUDIT D-95/D-96 had already moved on. M7/M8 now describe
    unsigned three-OS archives, the `l9-cross-os` hand-off that gated `L-9`, and the real residual
    (no signing identity, no installer, hardware/external gates). `docs/BOUNDED_SEARCH_AND_WIPE.md`
    described dense replace-all as a plausible design even though `stream_replace_chunks` and its
    30 M-word measurement already exist; it now states the implemented two-path behaviour and
    leaves total-process-memory qualification open.

- **2026-11-26 — D-96 — Platform IME & OS suspend tripwires across all OSes, Block 6–10 gates,
  Block 10 handbook/whitepaper/self-hosting/24-year lifecycle drill, and 3-OS `L-9` hand-off
  promotion (`4/10` laws `gated`).**
  - **Platform IME/dead-key composition & OS suspend tripwires (`platform.rs`, `cocoa.rs`,
    `win32.rs`, `x11.rs`, `canvas.rs`, `bin/holonomy.rs`):**
    - `DesktopEvent` added `Preedit(String)`, `CommitText(String)`, and `SystemSuspend`.
    - `cocoa.rs`: implemented `NSTextInputClient` protocol selectors (`hasMarkedText`,
      `markedRange`, `selectedRange`, `setMarkedText:selectedRange:replacementRange:`,
      `unmarkText`, `validAttributesForMarkedText`,
      `attributedSubstringForProposedRange:actualRange:`, `insertText:replacementRange:`,
      `doCommandBySelector:`, `characterIndexForPoint:`,
      `firstRectForCharacterRange:actualRange:`) + `interpretKeyEvents:` in `view_key_down` and
      registered `NSWorkspaceWillSleepNotification` observer on `NSWorkspace`.
    - `win32.rs`: dynamically resolved `imm32.dll` (`ImmGetContext`/`ImmGetCompositionStringW`/
      `ImmReleaseContext`) to emit `Preedit` / `CommitText` from `WM_IME_STARTCOMPOSITION`,
      `WM_IME_COMPOSITION`, and `WM_IME_ENDCOMPOSITION` without double-delivering `WM_CHAR`, and
      handled `WM_POWERBROADCAST` (`PBT_APMSUSPEND`) by clearing clipboard & emitting
      `SystemSuspend`.
    - `x11.rs` & `bin/holonomy.rs`: added `DeadComposeState` (13 X11 `XK_dead_*` diacritics +
      `XK_Multi_key` Compose accumulator) and installed async-signal-safe POSIX suspend/power
      tripwires (`SIGUSR2`/`SIGTSTP`/`SIGPWR` on X11, `SIGUSR2`/`SIGPWR` on TUI).
    - `canvas.rs`: renders live `ime_preedit` pill anchored at caret (`render_ime_preedit`), clears
      preedit on blur/commit, routes `CommitText` into active prose or prompt bars, and trips
      `TripReason::Suspend` on `SystemSuspend`.
  - **Block 6, 7, 8 & 10 gates (`persist.rs`, `paint_echo.rs`, `self_hosting.rs`,
    `scripts/qualify-24yr-lifecycle.sh`, `.github/workflows/ci.yml`):**
    - `persist.rs`: added `every_toolbar_action_round_trips_byte_identical_style_state` (Block 7
      Gate 1), `formatting_torture_corpus_across_slab_boundaries_single_undo` over 6 sealed slabs
      (Block 7 Gate 3, via `Editor::apply_styles_batch`), `twenty_four_year_fresh_user_lifecycle_drill`
      (`8,760` witnesses -> backup -> wipe -> restore -> continue -> compact), and wrapped
      `sixty_year_21900_witness_scrub_and_reverify` in live `Editor` + `ResidentSet` (`<= 8 MiB`
      budget) across years 1, 10, 30, and 60 (Block 8 Gate 2).
    - `paint_echo.rs`: measures software `PixelSurface` keystroke-to-pixel latency at head & tail
      of a real sealed 30 M-word `.wavefunction` (`HOLONOMY_PAINT_ECHO_30M`).
    - `self_hosting.rs`: Block 10 self-hosting gate authoring, formatting, bookmarking, compacting,
      verifying, and Markdown-exporting/re-importing the Block 10 specification in `.wavefunction`.
    - `scripts/qualify-24yr-lifecycle.sh` & `lifecycle` CI job: fresh-profile `env -i` create ->
      backup -> wipe -> restore -> PTY continue -> compact -> verify -> export drill paired with
      the 8,760-witness release scale drill.
  - **Block 10 prose deliverables & 3-OS `L-9` promotion (`docs/MIGRATION-V5.md`,
    `docs/SECURITY-WHITEPAPER.md`, `.github/workflows/ci.yml`, `laws.rs`, `docs/GATES.md`):**
    - Added [`docs/MIGRATION-V5.md`](docs/MIGRATION-V5.md) and
      [`docs/SECURITY-WHITEPAPER.md`](docs/SECURITY-WHITEPAPER.md).
    - Wired `portability` (`ubuntu-latest`) artifact upload (`l9-linux-handoff`) into `l9-cross-os`
      (`macos-latest` `aarch64` + `windows-latest` `x86_64`), verifying Linux-authored
      `.wavefunction` opens in scrubbed empty profiles with passphrase alone and byte-identical
      plaintext export; promoted `L-9` to `gated` (`4/10` laws `gated`: `L-1`, `L-2`, `L-8`, `L-9`)
      while keeping `L-3`..`L-7` and `L-10` honestly `spec` pending consumer hardware benchmarks,
      physical power-cut testing, out-of-process `SIGSTOP`/`SIGKILL` editor supervision, and
      non-rsync sync clients.

- **2026-11-26 — D-95 — Complete desktop & TUI UX pass, cross-OS native FFI parity, OpenType
  `GDEF`/`GSUB`/`GPOS` shaping, `L-1`/`L-2`/`L-8` gate promotion, and unsigned 3-OS release bundles.**
  - **Crypto & Eye (`holonomy-seal`, `holonomy-eye`):** Added constant-stack/zeroized integer
    passphrase strength estimation (`StrengthTier`, `PassphraseStrength`,
    `estimate_passphrase_strength`) without plaintext heap copies; added `FontSet::style_face` so
    `Shaper::line_height`/`ascent` reflect active Regular/Bold/Italic/Mono style; closed `OQ-24`
    with pure-Rust `GDEF`/`GSUB`/`GPOS` OpenType shaping (`crates/holonomy-eye/src/opentype.rs`);
    and promoted `L-1`, `L-2`, and `L-8` in `crates/holonomy-ledger/src/laws.rs` + `docs/GATES.md`
    (`xtask gates --check` verified).
  - **Headless shell (`holonomy-shell`):** Added `QUIET_MD_DARK_THEME` (`#1a1a19` dark paper /
    `#e8e6e1` ink), `Move::WordLeft`/`WordRight`, `Intent::DeleteWordBefore`/`DeleteWordAfter`,
    word/paragraph multi-click selection helpers, `DocumentMetrics` + `Editor::status_metrics`,
    `Editor::link_at_caret`, and zero-copy `ClusterMeta` (`<= 8` leaves, `Clusters::continuing`).
  - **TUI, Desktop & Native Platform Backends (`holonomy-gui`):**
    - `platform.rs`, `x11.rs`, `cocoa.rs`, `win32.rs`: Added `CaptureExclusionState`,
      `SecureInputState`, and `WindowSecurityInfo` wired into `Facts::lines()` (7 lines with
      verification age); macOS Cocoa `[NSWindow setSharingType:NSWindowSharingNone]` + HIToolbox
      `EnableSecureEventInput` and `NSPasteboard` concealed/transient scrub on blur; Windows
      `SetWindowDisplayAffinity(..., WDA_EXCLUDEFROMCAPTURE)` with `WDA_MONITOR` fallback and
      `ExcludeClipboardContentFromMonitorProcessing` + `EmptyClipboard` on blur.
    - `term.rs`, `bin/holonomy.rs`: Wired `Ctrl/Alt` word navigation/delete, `Ctrl+R` (`0x12`) TUI
      witness history browser & restore, dynamic 7-line `Facts` panel, and idle `status_metrics()`
      ribbon.
    - `canvas.rs`: Added dark mode (`Ctrl+Shift+D`), font-size presets (`CycleSize`), clickable
      `⋯` overflow popover (`ToggleOverflow`), reduced-motion toggle (`Ctrl+Shift+M`), 1.2 s idle
      chrome fade scrollbar (`CHROME_IDLE_FADE_MS`, preserving `L-5`), visual list bullets/ordinals,
      dismissible recovery banner, interactive `Ctrl+K` hyperlink dialog, 1–4 multi-click selection,
      live passphrase strength meter & opt-in `recent.txt` (default OFF so CLI `L-9` portability
      stays zero-artifact), and updated `scripts/qualify-x11-gui.sh` to verify dark mode, link
      creation, security panel, and history drawer inside `Xvfb`.
  - **Release packaging (`.github/workflows/release.yml`, `packaging/README-RELEASE.txt`):** Expanded
    unsigned experimental release bundles to Linux `x86_64`, macOS `aarch64`, and Windows `x86_64`
    with SHA-256 checksums, provenance manifests, and a single serialized `publish-release` job.

- **2026-11-25 — D-94 — Blocks 7, 8, 10, OpenType glyph rasterizer, and pure-`libc` X11 desktop
  window implemented and verified in CI run `45566921069` (`a047181`), all 8 jobs green.**
  - **Block 7 (dense streaming replace-all + formatting persistence):** `holonomy-line` added
    `StreamingTx` (`start_streaming_tx` / `push_segment` / `finish_streaming_tx` / `abort_streaming_tx`)
    with read-only `open_segment` kept available during healthy streaming via `WorldLine::streaming_active`
    separate from `poisoned`. `holonomy-text` added `StreamSegmentWriter`, `StyleMap::normalize_runs`,
    and `stream_replace_chunks` (removing the hard 16,384-match failure whenever the sink supports
    streaming publication, preserving UTF-8 boundaries, style/link spans, fault rollback, and
    single-step undo). CI (`a047181`) measured `HOLONOMY_ENCRYPTED_SEARCH segments=2748 bytes=180092928 words=30013653 matches=5 resident=199680 find_peak=1176232 replace_peak=1783625 dense_replace_peak=1832689 bound=4194304`
    (`HOLONOMY_DENSE_REPLACE matches=30013649 peak_heap_delta=1832689`), and
    `holonomy-gui/tests/persist.rs::dense_streaming_replace_all_over_real_document_persists_and_undoes`
    verified full 68,000-byte replacement and 51,000-byte undo persistence on real containers.
  - **Block 8 (witness history, bookmarks, diff, live coalescing, and non-`Intent` spine restore):**
    `holonomy-line` added `RECORD_BOOKMARK = 4`, `Bookmark`, `WitnessSummary`, `witness_history`,
    `bookmarks`, `add_bookmark`, `open_manifest_at`, and `diff_witnesses`; `holonomy-text` /
    `holonomy-shell` added `coalesce` (`OQ-9`), `restore_spine`, and bounded `copy_selection`
    (`OQ-3`). Verified by `holonomy-line` tamper/reopen witness-history tests and
    `persist.rs::document_witness_history_bookmarks_diff_search_and_restore_persist`. *Open
    residual:* X11 desktop interactive 60-year timeline scrubber UI and a dedicated 60-year
    history-scrub resident-memory benchmark remain open (`ROADMAP.md` Block 8).
  - **OpenType `loca`/`glyf` rasterizer + pure-`libc` X11 pixel desktop (`holonomy-eye::raster`,
    `holonomy-gui::{x11,canvas}`, `holonomy gui`, `holonomy-gui` binary):** `holonomy-eye::raster`
    parses `head`, `loca`, `glyf`, `hhea`, `hmtx`, `maxp`, `cmap` for embedded `Inter-Regular`,
    `Inter-Bold`, `Inter-Italic`, and `JetBrainsMono-Regular` (SIL OFL 1.1; attributions in
    `THIRD_PARTY.md` and `LICENSES/*.txt`) with analytic scanline coverage (`tests/raster_fonts.rs`).
    `x11.rs` implements raw X11 local Unix-socket wire framing (`POLLOUT` non-blocking request
    flush, `.Xauthority` MIT-MAGIC-COOKIE-1 with cookie-less fallback, `PutImage` scanline
    chunking, `CLIPBOARD` selection handling, and `FocusOut` clipboard zeroization) + X11 keymap
    and keypad decoding; `canvas.rs` implements `FrameBuffer`, `PixelSurface` driven from
    `Frame::picture()`, Block 7 formatting ribbon + overflow popovers, find/replace, security
    panel, `Ctrl+X` selection cut, idle deadman evaluation, and focus-blur clipboard zeroization.
    `canvas::tests::pixel_surface_pointer_hit_test_matches_painted_caret_across_zooms_and_dpis`
    pins `L-8` pointer round-trips across zooms and DPIs. *Open residuals:* headless CI has no live
    X11 display server (`holonomy gui` is compiled and tested at wire/pixel/unit level, not screenshot
    tested against a running X server), OpenType `GSUB`/`GPOS` shaping remains `OQ-24`, GPUI /
    macOS / Windows native shells remain unbuilt, and all 10 `docs/GATES.md` laws remain `spec`.
  - **Block 10 (streaming `.txt`/`.md` import/export & one-way v5.1.0 container migration):**
    `holonomy_seal::v5` implements the legacy 134,217,728-byte container (`2,048 × 65,536`-byte
    slots, `V5Passphrase` lowercase normalization, Argon2id, CMT-4, `MlKem1024X25519`, `Feistel22`,
    `V5PadSource::{None, Derived, DeviceFile}`, `scan_v5_cursors`, and `Zeroizing<Vec<u8>>`
    secret slot/directory buffers). `holonomy-gui::transfer` + `holonomy {import,export,migrate-v5}`
    implement UTF-8-bounded streaming import (cleanup on error), explicit plaintext-egress disclosure
    (`PLAINTEXT_EGRESS_DISCLOSURE`, no overwrite without `--force`), and multibyte slot-seam
    `.wf5 → .wavefunction` migration without mutating legacy containers (`tests/transfer_migrate.rs`).
  - **CI run `45566921069` (`a047181`) supporting gate logs:**
    `HOLONOMY_SUPERVISOR_SUMMARY trials=12 p50_us=2526 max_us=2590 over_50ms=0 scope=shared_test_arena_only`;
    `HOLONOMY_SCRIPT_GATE_HONESTY scripts=14 clusters=45 carets=59 carets_inside_a_grapheme=0`;
    `HOLONOMY_SCRIPT_GATE_L8 boundaries=27680 disagreements=0`;
    `HOLONOMY_SCRIPT_GATE_ORDER scripts=6 clusters=40 displaced=0 note=uax9-visual-order`;
    `HOLONOMY_VIEWPORT_LATENCY at=180MB@0 samples=256 p50_ns=777988 p99_ns=823515` (`control_p50_ns=76535414`);
    `HOLONOMY_WIPE_GATE trials=24 arenas=1041 bytes_wiped=4324384 ghosts=0 sched=NormalFallback reaper_p50_us=131 wall_p50_us=154`;
    `HOLONOMY_PORTABILITY L-9=partial ... copied_bytes=5622 resumed_bytes=6010 verify_near=ok verify_far=ok`;
    `HOLONOMY_SMOKE binary=holonomy document_bytes=5629 sentence_in_ciphertext=no wrong_phrase=refused empty_phrase=refused reopen=ok`;
    `HOLONOMY_ENOSPC_MEASUREMENT ... recovered_segments=1 recovered_tail_bytes=221184 verified_segments=1`;
    `HOLONOMY_RSYNC_MEASUREMENT ... delta_application_bytes=334908` (vs `new_file_bytes=60356483`);
    `1,000,000` random keystrokes (`2,601,638` actions, `210.06s`, exit `0`).

- **2026-11-24 — D-93 — A failed verification skipped the wipe it was evidence against.**
  The emergency trip scrubs two populations, and they fail differently. `SecretArenas` holds the
  resident plaintext the reaper reaches through registered slots, and its canaries are what say the
  scrub happened. The **editor** separately holds slabs, the typing buffer and its undo copies, and
  the window holds viewport text; `Editor::wipe_all_in_place` is the product-level scrub of those.
  D-89, added to make the closing line stop claiming a wipe it had not verified, folded that second
  scrub *into* the confirmed branch: `WipeReceipt::from_outcome` gated on a clean canary read, and
  the `wipe_all_in_place` call sat inside the `Ok` arm. On `Err(unconfirmed)` — ghosts survived the
  reaper's scrub, or nothing was armed, or the join produced no outcome at all — and on `None`, the
  editor's plaintext was therefore **never wiped**, and freed into the heap intact at process exit.
  The reasoning that produced it was that a wipe nobody could verify should not be announced as if
  it had happened, which is right about the *sentence* and wrong about the *scrub*: the sentence is
  a report, `wipe_all_in_place` is the action, and the case where the reaper's canaries came back
  dirty is the strongest reason to scrub the rest, not a reason to abstain from it. Found by
  reading the change back rather than by any test, because no test reached the branch: the defect
  is in a binary's shutdown path, and the path is only taken when a live reaper disagrees.
  Fixed by hoisting the call above the `match`, so the scrub runs on every path and the verdict is
  decided afterwards. To keep that from being a claim rather than a fact, the decision after the
  join is now a pure function, `classify_trip(outcome, wiped_in_place) -> Ended`, which takes no
  editor: the in-place scrub is unreachable from it **by construction**, so a future edit cannot
  re-conditionalise it without changing the signature, and `every_trip_classifies_as_a_wipe_whatever_the_reaper_said`
  drives all four shapes (clean, nothing armed, ghost, no outcome) through it.
  The receipt reports the editor's bytes as its own field, `in_place_bytes`, distinct from
  `bytes_wiped`: the first number is a return value from the editor and the second is measured by
  canaries in the reaper, and summing them would let the unmeasured half ride on the measured one.
  The closing line names them separately.
  Hoisting the call exposed a second, quieter half of the same defect, in the *reporting*:
  `WipeUnconfirmed::describe` had no way to mention the scrub, so the new all-path scrub would have
  been invisible on exactly the trips where it matters. The failure sentence now ends with what the
  editor scrubbed ("still scrubbed 8 KiB in place"), and says "the editor had nothing left to
  scrub" rather than "0 bytes" when the number is zero — a zero there is an absence, not a
  measurement. `Ended::WipeUnconfirmed` carries the count so the two cannot drift apart.

- **2026-11-24 — D-92 — L-9's drill now measures the half a single host can carry, and says so in
  its own output.** `docs/GATES.md`'s L-9 row read **nothing yet**: the law is "a copied
  `.wavefunction` opens on a fresh machine with a passphrase alone; no account, token, or device
  required", and the project had no harness that touched it. The existing `a_copy_taken_mid_session_opens_where_it_was_copied`
  test in `holonomy-gui/tests/persist.rs` copies a file within one process family, which proves the
  commit boundary and nothing about portability.
  Added `scripts/qualify-l9-portability.sh` and a `portability` CI job. It authors a document with
  the release binary through a pty, copies **only the file** into a separate delivered tree, and
  then imports it under `env -i` with `HOME`, all four `XDG_*` directories, `XDG_RUNTIME_DIR` and
  `TMPDIR` pointed at a directory that is **asserted empty immediately before the import**. No
  environment variable crosses from the authoring side except `PATH`. The import must `verify`
  under the passphrase alone, show the sentence the near side typed, accept a continuation that is
  durable, and re-verify on a third open.
  The falsifiable half is asserted after the fact: the importer's dependency-state tree must
  **still be empty**. If a future change taught the app to keep a session, a wrapped key, a keyring
  entry or a cache, this drill fails instead of passing on a host that happened to share state.
  The marker says `L-9=partial` with `same_kernel=yes same_libc=yes same_fs=yes second_machine=no`,
  and the script's header spells out that a different kernel, libc, filesystem, architecture and OS
  are exactly what it does **not** prove. **L-9 stays `spec`**: the machine half needs the release
  tarball opened on a second machine, which is the drill this one is the prerequisite for.
  Three defects were found and fixed, and the last two only by running the thing:

  1. **Read, not run (fixed before the first push):** a process substitution passed to `env -i`
     (which cannot see the replaced path), and the driver's key schedule fed through a fifo rather
     than a file in the delivered tree.
  2. **Found by CI, not by me (fixed after the first red push):** `env -i PATH=/usr/bin:/bin` then
     `python3` — the confined environment cannot resolve the driver's interpreter wherever it is not
     in those two directories, so the import died at `execv` with 127. The pty driver is scaffolding,
     not the importer; it is now resolved with `command -v` *outside* the confinement and passed in
     as an absolute path.
  3. **Found while reproducing (2) locally:** a pty inherits the caller's window size, and in CI that
     is whatever `tee` had. A narrow window wraps the rendered sentence and makes the "did the far
     side see what the near side wrote" assertion fail for a reason that is about the terminal, not
     about portability. `scripts/drive-pty.py` now takes `--cols`/`--rows` and applies `TIOCSWINSZ`
     in the child before exec — verified by reading the size back in a child at two different
     requested sizes — and the drill declares a 120×30 window and keeps its asserted sentence short
     enough not to wrap at it.

  4. **Found only by the diag bridge (fixed after the second red push):** the drill's own `drive()`
     helper forwarded its three capture arguments to the binary — it was missing the `shift 3` that
     `scripts/smoke-install.sh` has, so `holonomy` received `author.session "correct horse…" "a
     document…"` as positional arguments, printed its usage text and exited 2. Two CI cycles went
     to a one-line omission in test scaffolding; the product was never involved.

  The lesson is worth recording because it is the same one D-73 recorded about `script(1)`: a pty
  harness has failure modes of its own, and from a distance they look like product failures. After
  fix (4) the whole script was run end to end outside CI against a stand-in binary — author, copy,
  confined import, verify near and far, growth check, state-leftover check — and it printed
  `HOLONOMY_PORTABILITY L-9=partial …` and `l9: PASS`. The same run was then repeated against a
  stand-in that deliberately writes `$HOME/.config/holonomy/session.key`, and the drill **failed**,
  naming the file. That negative control matters more than the positive one: it is the only evidence
  that the empty-state assertion can fail at all, and a check that cannot fail is not a check.

  **Measured 2026-11-24, CI run `45415761926` on `db22eb7`** (green, all eight jobs):
  `HOLONOMY_PORTABILITY L-9=partial same_kernel=yes same_libc=yes same_fs=yes second_machine=no`
  `inherited_state=none dependency_state_after_import=empty copied_bytes=5622 resumed_bytes=6010`
  `verify_near=ok verify_far=ok continuation=durable`. A 5,622-byte document crossed the boundary,
  verified under the passphrase alone in a process that could not have had any prior state, was
  continued to 6,010 bytes, and re-verified; the importer's dependency tree was empty afterwards.
  The negative control in the same design (a stand-in that writes `$HOME/.config/holonomy/session.key`)
  fails the drill and names the file.

- **2026-11-24 — D-91 — The viewport gate's absolute number was quoted as a property of the code;
  it is a property of the runner.** Harness bookkeeping, not a product defect, but the class this
  audit exists to catch: a measurement cited in a form the measurement does not support.
  `OQ-12`/`D-78` and Block 5's second gate read
  `p50 = 697,961 ns / 697,047 ns / 697,753 ns` at three offsets — the numbers from run
  `45106234334` — and `ROADMAP.md` and `docs/TO-APP.md` repeated "~697 µs p50" as though the
  software had a latency. It does not have one; the runner does. The unmodified gate was re-run
  twice on 2026-11-24, sixteen minutes apart on the same runner class:

  | run | 180 MB @ 0 | 180 KB @ 0 | 180 MB @ end | control | ratio |
  | --- | --- | --- | --- | --- | --- |
  | 13:34 | 696,731 ns | 697,311 ns | 693,836 ns | 62,947,880 ns | 90× |
  | 13:50 | 1,045,075 ns | 1,044,182 ns | 1,043,401 ns | 92,377,254 ns | 88× |

  The `p50` for byte-identical inputs moves `1.5×` between runs. Within each run the three offsets
  agree to `< 0.1%`, and the control ratio is `76×`, `88×`, `90×` across three runs. So the gate
  establishes exactly two things — cross-offset invariance and the ratio against a
  document-proportional control — and neither is the absolute figure the documents were quoting.
  Fixed by restating the claim where it is made: `AUDIT.md`'s `OQ-12` row, `ROADMAP.md`'s Block 5
  summary and gate, and `docs/TO-APP.md`'s M7 note now quote the **measured range** across runs,
  say that the absolute number belongs to the runner, and keep the original run's figures only as
  the historical record of that run (annotated in `D-78`).
  The test itself needed no change and deliberately did not get one: `P50_FACTOR = 2` and
  `P99_FACTOR = 4` bound cross-offset agreement, `FRAME_CEILING = 50 ms` sits ~50× above the
  measured value, and `CONTROL_FACTOR = 25` sits ~3× below the smallest observed ratio — margins
  wide enough that runner variance cannot flake them, which is why the spread surfaced as a
  documentation error rather than a red build. Loosening or tightening them to "fit" this table
  would have been the mistake. `L-4`'s `BUDGETS` row is unaffected: it is about
  keystroke-to-*paint*, measured by `holonomy-text`'s `tests::keystroke_latency`, and still owes
  the p99 on reference hardware and the paint half.

- **2026-11-24 — D-90 — The first packaged artifact, and the gate that stops it being a zip.**
  README's table said "A packaged release or installer — **None.**" and `docs/TO-APP.md` M8 listed
  "the workspace has no release workflow at all" as a known gap. Added
  `.github/workflows/release.yml`, deliberately the smallest honest step: **one** target
  (`x86_64-unknown-linux-gnu`), the pinned toolchain and `--locked` so the build is the one the
  lockfile describes, and — the part that makes it a release rather than an archive —
  `scripts/smoke-install.sh` runs against `target/release/holonomy` *before* anything is assembled.
  A binary that cannot create, seal, reopen, `verify` and `compact` a document through a real pty,
  or that accepts a wrong or empty passphrase, therefore cannot reach a tarball.
  The artefact is `holonomy-<version>-<target>.tar.gz` containing `bin/holonomy`, `LICENSE-MIT`,
  `LICENSE-APACHE`, `THIRD_PARTY.md`, `README.md`, the `LICENSES/` attribution set (the Eye compiles
  Unicode-3.0 tables), and `packaging/README-RELEASE.txt`, whose first line names the build unsigned,
  experimental and single-platform; beside it a `.sha256` and a `.provenance.txt` that record commit,
  ref, target and that no signature exists. Publishing is reachable only through a `v*` tag — a
  `workflow_dispatch` run builds and attests without publishing — so no manual run can name a release
  after a branch. The release body repeats the scope rather than implying more.
  `packaging/README-RELEASE.txt` is a file in the tree rather than a heredoc inside the workflow for a
  found reason: a heredoc terminator at column zero ends the YAML `run:` block scalar, which the
  first draft did, and which a structural check caught before any push.
  Verified without a local toolchain (AUDIT OQ-1): the archive is assembled from the real tree in a
  scratch directory and its contents and checksum inspected; the YAML is checked structurally (no
  tabs, no column-zero lines inside blocks, every step named). **Not yet run by Actions as of this
  entry** — the workflow only triggers on a tag or a manual dispatch, so the first real proof is the
  first `workflow_dispatch`. Treat the pipeline as reviewed, not exercised.
  **What does not move.** L-9 stays `spec` and its `BUDGETS` row still reads **nothing yet**: the law
  is one file opened on a *second machine*, and an unsigned tarball for one platform is the
  prerequisite for that drill, not the drill. Nothing is signed on any platform, macOS and Windows
  have no build, and reproducibility is not claimed.

- **2026-11-24 — D-89 — The window discarded its own wipe evidence; a confirmed wipe now exits 0
  and an unconfirmed one exits 1.** Found while writing an inventory of what the *live* editor can
  say about `L-7` rather than what the headless harness measures. `holonomy-gui`'s
  `wipe_quiescent` called `let _ = reaper.wait_outcome()` — the `ReaperOutcome` carries
  `bytes_wiped`, `arenas_wiped`, `canaries.ghost_canaries`, `ghost_bytes` and
  `trip_to_verified_ns`, and every one of those was dropped on the floor. The window then printed
  the fixed string `"emergency wipe attempted — full memory coverage not verified"` and **exited
  0**, so a session in which the canary read-back found a surviving byte was indistinguishable, to
  a person or a script, from one in which the wipe was clean. That is D-49's defect (a failed save
  exited 0) in the one place the project cannot afford it: the runtime protection.
  Fixed by classifying the outcome instead of discarding it. `WipeReceipt::from_outcome` yields
  `WipeReceipt` (bytes, arenas, canaries, µs to verified, and whether scheduling was real-time)
  only when at least one arena was actually scrubbed **and** no canary survived; every other shape
  becomes a `WipeUnconfirmed` — `NoOutcome` (join yielded nothing), `NothingArmed` (`arenas_wiped
  == 0`, so zero bytes may have been wiped) or `Ghosts { ghost_canaries, ghost_bytes }` — which
  prints the reason and exits 1 on the same policy as a refused save. `Ended::Wiped` became
  `Ended::Wiped(WipeReceipt)` / `Ended::WipeUnconfirmed(_)` so the compiler forces every site to
  handle both. The unconfirmed branch deliberately does **not** run the in-place editor scrub: an
  unverified wipe gets no supporting gesture.
  The panel had the matching defect one level down: a tripped reaper over an **empty** registry
  rendered `"reaper: TRIPPED (0 arenas, 4/4 canaries zeroized)"` — a *successful*-looking sentence
  built from a stale ghost count — and a tripped reaper with survivors said nothing about them.
  `Facts::reaper_line` now names `"no arena armed — nothing was proven wiped"` for the empty case
  and appends `"— GHOSTS SURVIVED"` when canaries remain, so the two failure shapes can no longer
  read as success.
  Tests pin the classification, not the prose: five bin tests cover the confirmed shape (including
  that a fallback-scheduled reaper is described as such and nanoseconds are reported as
  microseconds), zero-arena, a single ghost outranking a large wipe, pluralisation, and that only a
  confirmed wipe avoids the non-zero exit path; two panel tests cover the empty-registry and
  ghost-bearing lines. Not yet run on CI as of this entry — the environment that wrote it has no
  Rust toolchain (AUDIT OQ-1), so the next push's `clippy -D warnings` and `test` jobs are the
  compiler of record for it.
  **What this does not claim.** It is a receipt about *registered* arenas, not coverage of resident
  plaintext: the inventory in `docs/WIPE_SUPERVISOR_REVIEW.md` is still the list of what escapes
  registration (passphrase parsing, the KDF arena, terminal strings and scrollback, stacks,
  registers, descendants). The watcher is still cooperative, so a blocked UI thread still cannot
  meet the 50 ms deadline from the tripwire. `L-7` therefore stays `spec` in `docs/GATES.md`; what
  changed is that the *live* editor no longer has a code path that can call an unverified wipe a
  success, which is the part of L-7 the shipped window can enforce today.

- **2026-11-22 — D-88 — Wipe inventory and atomic-identity prototype.** Corrected the README's
  overstated live-wipe and obsolete no-search claims. `docs/WIPE_SUPERVISOR_REVIEW.md` inventories
  current plaintext/key sources and their known coverage gaps, and specifies failure and review
  obligations; it is not a completeness audit. The isolated Linux test now uses fork-like
  `clone3(CLONE_PIDFD)` and a pidfd for all subsequent signals/waits, without numeric-PID fallback,
  eliminating the D-87 acquisition window in this test fixture. Regressions exercise early external
  reaping and failed exec. Unsupported or denied clone3 fails startup. Raw post-fork code and
  inherited state need independent review; this is not a production spawn implementation.
  An optimized local Rust 1.99 manual build passed all eight ignored probe tests: 12 shared-arena
  trials, p50 3,283 µs, max 7,056 µs, zero ghosts. CI on the pinned toolchain is still required.
  The editor is unchanged; complete coverage, locked memory, failure policy and L-7 remain open.


- **2026-11-22 — D-87 — Supervisor probe bug hunt.** A worker externally reaped before a
  signal could make numeric-PID cleanup target a reused PID. After setup, the Linux prototype now
  signals and waits through a pidfd, with a stale-pidfd/external-reaper regression; its initial
  `pidfd_open` still is not atomic with child creation and is not a production-safe spawn design.
  Added a nonzero-canary negative control and made a verification ghost an error rather than a
  successful report. The probe requires Linux pidfd and `waitid(P_PIDFD)` support; its tests are
  ignored in ordinary workspace runs and explicitly exercised by the Linux CI measurement.
  Pinned push CI [45343917300](https://github.com/c4us4l1ty/holonomy/actions/runs/45343917300)
  passed for that revision. D-88 supersedes the non-atomic setup in the test fixture. Neither
  establishes L-7 or complete live-editor wipe coverage.

- **2026-11-22 — D-86 — Linux isolated supervisor experiment, not L-7.** Added a test-only
  subprocess probe: a controlled worker owns a 4 MiB memfd mapping while blocked in an observed
  socket read, spinning while holding a lock, or running a concurrent writer. The supervisor
  confirms a group stop, kills the worker, confirms its exit, then wipes and verifies the shared
  mapping; an unconfirmed stop refuses the wipe. The timer includes a pre-wipe negative control.
  Locally, an optimized Rust 1.99 build with the real libc 0.2.191 source passed 12 trials
  (p50 3,733 µs, max 5,351 µs, zero ghosts, no trial over 50 ms); debug timings exceeded 50 ms.
  This is host-specific prototype evidence only: the live editor, private allocations, stacks,
  registers, terminal copies, descendants, page locking, external interference and uninterruptible
  waits are not covered. SIGKILL or process exit is not itself a verified wipe, and the cleanup
  path may block indefinitely. The L-7 deadline and production wipe coverage remain unproven.
  Pinned push CI [45333626167](https://github.com/c4us4l1ty/holonomy/actions/runs/45333626167)
  passed the workspace and release probe: 12 shared-arena trials, p50 2,768 µs, max 4,261 µs,
  zero ghosts and zero trials over 50 ms. These CI timings include the negative-control scan and
  are not reference-hardware certification or evidence of a live-editor deadline.

- **2026-11-22 — D-85 — Bounded dense search work; gates remain open.** Added a visitor and
  count/index API that avoid accumulating match positions, limit needle size, and use small scan
  windows so a heavily fragmented spine cannot fill a full-document span vector. The terminal's
  find operation uses the count/index API. The collecting API and actual replace-all now refuse
  more than 16,384 matches before modifying the document; identical no-op replacement can count
  densely without collecting. This is a visible functional limitation, not dense replace-all
  support. Push run [45321900958](https://github.com/c4us4l1ty/holonomy/actions/runs/45321900958)
  scanned 30,013,649 dense matches in the encrypted fixture: peak Rust live heap 1,310,741 bytes,
  1,174,991 bytes over the pre-open baseline, and 4,415,488 bytes peak sampled Linux process RSS.
  RSS was sampled every 64 segments, so it cannot prove an instantaneous peak or account for kernel
  page cache. Workspace tests and the encrypted measurement passed; clippy found use of an API newer
  than the declared MSRV, corrected afterward and awaiting CI. The architecture and qualification
  paths for streaming transactional dense replacement and a stalled-UI wipe are in
  `docs/BOUNDED_SEARCH_AND_WIPE.md`. No 50 ms guarantee, full wipe coverage, total-memory bound, or
  Block 7/9 gate is claimed.

- **2026-11-22 — D-84 — Dead-man and no-op replacement follow-up.** A heartbeat arriving
  after a dead-man deadline could previously erase an expiry before the watcher saw it, leaving
  a cooperative wipe waiting indefinitely; expiry and heartbeat now compete on one atomic
  heartbeat state, and expiry latches the reason and wakes the watcher. Concurrent trip producers
  also previously could pair one producer's reason with another's timestamp; those values now
  publish as a single atomic word, saturating the timestamp after roughly 73 years. Regression
  tests cover concurrent producers, an observed expiry followed by a late heartbeat, and a late
  heartbeat without a prior observer.
  The terminal checks for a trip before treating EOF/read errors, input batches, or quit as normal
  exit. These checks do not prove that every trip racing shutdown is handled, nor do they meet
  L-7's 50 ms deadline when the UI is blocked. A no-match or identical replace now searches the
  live draft without flushing, preserving its undo/redo history. A Rust 1.99 reproduction found
  redo depth dropping from 1 to 0 when a replacement's atomic commit failed after flushing a live
  draft. Replacement now checkpoints the live editor state before a needed flush and restores it
  on failure; the previously failing redo regression passes locally. A successful preliminary
  flush may already be durable, and an uncertain I/O failure still requires reopening the poisoned
  world-line. Checkpointing can copy undo-held plaintext and has not been included in the sparse
  no-draft memory measurement. Full pinned CI and adversarial review remain necessary.

- **2026-11-22 — D-83 — Encrypted sparse search memory diagnostic.** Push CI run
  [45238499667](https://github.com/c4us4l1ty/holonomy/actions/runs/45238499667) on
  `17161dd` ran the release-mode `encrypted_search_memory` test successfully: a reopened,
  production-fenced encrypted world-line contained 180,092,928 plaintext bytes and 30,013,653
  whitespace-delimited words. Five matches included three segment seams. The Rust global
  allocator measured a peak live-heap increase over the pre-open baseline of 1,177,528 bytes
  for find and 1,781,497 bytes for replace-all, including reopened document metadata, editor,
  resident allocations, and operation allocations; resident capacity was 199,680 bytes. One undo
  restored the original in-memory root. The 4 MiB threshold was asserted for this fixture.
  This is not total process memory: the pre-open baseline (including keys), stack, allocator
  overhead, memory mappings and OS page cache are excluded; only one low-match-density workload
  and a partly occupied resident set were exercised. Search collects matches in a vector, so
  dense-match behavior is not bounded by this result. Block 7's memory gate remains open.
  The successful measurement does not remove the cooperative reaper's timeout, coverage, and
  safety-review limitations in D-82; no live L-7 deadline or full wipe guarantee is claimed.

- **2026-11-22 — D-82 — Draft fixes for D-81; release gates still open.**
  Replace-all now asks the sink to reserve IDs and publish all replacement chunks and the final
  styled manifest in one world-line transaction; unsupported sinks fail closed. The editor root
  and undo history change only after the sink returns success. An injected sink failure test and
  a multi-segment styled-manifest short-write/reopen test cover the new boundary; the pinned
  workspace tests passed in CI run 45232126253 on `59287b7`.
  The terminal now uses a cooperative reaper that waits for the UI thread to stop before registering
  live allocations and allowing the wipe; existing undo buffers are included, and canary sampling
  serializes with wipe. This avoids the identified editor/reaper concurrent-access path, but a
  blocked UI thread cannot acknowledge the trip, so the L-7 50 ms deadline is not established.
  Terminal surface, temporary search strings and other transient plaintext still need a complete
  wipe-coverage and pointer-provenance audit. The panel labels cooperative operation honestly.
  `StyleMap` also coalesces adjacent runs after link-ID remapping. Local tests used Rust 1.99 with
  zeroize 1.9.1 for text/eye and a minimal libc binding for reaper tests; subsequent pinned CI and
  the limited encrypted memory measurement are recorded above.

- **2026-11-22 — D-81 — Ribbon and live-reaper work is a draft, not a closed gate.**
  Cross-crate review found two release blockers in the current working tree:
  (1) `SecretArenas` prevents a registered allocation from being freed during `wipe`, but its
  slot state does not prevent the editor from reading or writing that allocation concurrently.
  A dead-man trip can therefore race with normal plaintext or key access; volatile accesses do
  not make that data race safe. Canary sampling can race with wiping for the same reason. The
  app wiring must not be considered safe until ownership/access is synchronized or the editor
  is quiesced before any registered data is wiped, with tests for trip/edit races.
  (2) `Typing::replace_all` calls `append_segment` for replacement chunks before committing the
  final manifest. The production sink publishes each append in the active document, so a crash
  or later failure can expose a partial replacement transaction. It also swaps the in-memory root
  before the final commit succeeds. Replace-all needs a single atomic sink operation and failure
  injection/reopen coverage before its atomicity claim is valid.
  The 30 M-word test uses a generated `SegmentSource`, not an encrypted world-line, and checking
  only `ResidentSet::resident_bytes()` does not bound the search seam buffers or match list. No
  Block 7 gate is claimed by this draft. Local Rust tools are unavailable. CI run `45218714739`
  failed formatting and compilation (including the shell's style-run lifetime and stale test APIs);
  the independent million-keystroke rehearsal and gates contract did pass. Subsequent fixes require
  another full CI run. Run `45220066975` passed formatting, gates, rsync and ext4 qualification
  but still failed compilation on a GUI lifetime error; that error was corrected afterward.
  Passing CI alone will not resolve the two release blockers above.

- **2026-11-22 — D-80 — Cross-crate bug hunt closed split-BiDi selection rendering, multi-leaf
  `ParagraphContext` and `Carry` invariants, UAX #9 `P2`/`L1.4` edge cases, reaper provenance and
  timestamp ordering, and residual plaintext buffers.**
  Auditing `holonomy-eye`, `holonomy-shell`, `holonomy-gui`, `holonomy-text`, and `holonomy-seal`
  together with differential multi-leaf fuzzing uncovered and closed six defects:
  (1) `Picture::selection_on` and `App::draw` took `.find(...)` per line and dropped subsequent
  disjoint visual `SelectionSpan`s on mixed-BiDi lines; `Picture::selections_on` and `App::draw`
  now render every span (`picture.rs`, `holonomy-gui/tests/terminal.rs`).
  (2) `ResolvedParagraph::cluster_info` classified `" \u{0301}"` (`WS` + `NSM`) from its leading
  `WS` alone so `L1.4` reset the cluster across a non-resettable `NSM`, and `first_strong_level`
  stopped `P2` early on an unmatched leading `PDI`; both now follow UAX #9 (`bidi.rs`).
  (3) Multi-leaf `holonomy-shell` lost an open RTL paragraph's base direction when scrolled to a
  pure-ASCII soft-wrap continuation in a later leaf, undercounted a final 1-byte `"\n"` leaf
  completing a split `"\r\n"`, checked `spans[index + 1].len()` instead of `span.len()` for
  straddling-cluster `skip`, and clobbered `Carry.x` in `measure_window` when a leaf opened on
  `'\n'` or a wrapped wide cluster (`'中'`); fixed via `ParagraphContext`, `layout_styled_in_paragraph`,
  `window_opening`, and predecessor-end `Carry.x` (`frame_stability.rs`).
  (4) `Tripwire::fire` published `explicit_reason` before `tripped_at_ns`, allowing a concurrent
  `evaluate()` to observe a zero sentinel clamped to `1`; `tripped_at_ns` is now won with `AcqRel`
  before `explicit_reason` (`reaper.rs`).
  (5) `LockedSecret<N>` and `CanarySlab` held `Box` (`Unique`/non-`UnsafeCell`) while armed in
  `SecretArenas`, risking pointer-provenance invalidation when moved by value; both now hold raw
  `NonNull` over `UnsafeCell` (`secret.rs`, `reaper.rs`).
  (6) `Typing`'s `Step::Root.held` undo/redo copies, `WindowText`, and `App::draw`'s temporary
  frame `String` freed plaintext without scrubbing; `Step::Root.held` and `App::draw` now use
  `Zeroizing`, `Typing::wipe_in_place` scrubs both open and historical buffers, and `WindowText`
  volatile-zeroes its backing capacity on `Drop` (`typing.rs`, `window.rs`).

- **2026-11-22 — D-79 — Block 9 re-audit caught an unwiped `realloc` in `SlabText::from_plaintext`,
  and `L-7` wipes a full 4 MiB viewport + live keys + 32 canaries in under 1 ms with zero
  allocations.**
  Re-auditing plaintext ownership across `holonomy-text` and `holonomy-seal` (Block 9's forced
  implementation recheck) found a concrete defect in `SlabText::from_plaintext`
  (`crates/holonomy-text/src/text.rs`): wrapping `plaintext: Vec<u8>` in `Zeroizing` and calling
  `bytes.reserve(slack)` allowed `System::realloc` to free the old heap buffer whenever
  `plaintext.capacity() < plaintext.len() + slack` — leaving a full copy of decrypted segment
  plaintext unwiped on the heap — and early-returned on invalid UTF-8 without zeroing spare
  capacity. Fixed by wrapping `plaintext` in `Zeroizing` immediately, copying into a
  zero-initialized `vec![0u8; needed]`, and scrubbing `0..source_cap` before dropping the input.
  For `L-7`, `crates/holonomy-seal/src/reaper.rs` implements a lock-free atomic slot table
  (`SecretArenas`, `MAX_ARENAS = 2_048`) whose state machine (`EMPTY`, `INIT`, `ARMED`, `WIPING`,
  `UNREGISTERING`) guarantees `Drop` unregistration spins on `WIPING` rather than racing a volatile
  scrub; `LockedSecret<N>` (`DEMKey`, `UnwrapKey`) arms its 64 KiB page inside `holonomy-seal`
  without exposing key pointers; `CanarySet` plants `0xA5` canaries and samples them via
  `read_volatile`; and `ReaperGuard` runs the zero-allocation emergency thread across `Manual`,
  `Suspend`, `UmbilicalDisconnect`, and `DeadmanTimeout` tripwires. Measured green in **CI run
  `45106234334` on `3b54969`** (`crates/holonomy-text/tests/wipe_gate.rs`):
  `HOLONOMY_WIPE_CONTROL sampled=32 ghost_canaries=1 ghost_bytes=4096 quiet_allocs=0` and
  `HOLONOMY_WIPE_GATE trials=24 arenas=1041 bytes_wiped=4324384 ghosts=0 sched=NormalFallback
  reaper_p50_us=878 reaper_p99_us=922 reaper_max_us=922 wall_p50_us=913 wall_p99_us=949
  wall_max_us=949`. `BUDGETS` in `holonomy-ledger` updates `L-7` (`unmeasured == 1`, leaving only
  `L-9`). Remaining user-visible limits stay named: `OQ-3` (OS clipboard in Block 6's GPUI shell;
  the terminal front end has no OS clipboard integration) and OS-level suspend/capture hooks.

- **2026-11-22 — D-78 — 30 M-word real-`Face` viewport latency is invariant to 0.1%, and testing
  multi-leaf BiDi reflow exposed three leaf-seam bugs in `holonomy-shell`.**
  `crates/holonomy-eye/tests/viewport_latency.rs` closes `OQ-12` and Block 5's second gate by
  timing the real OpenType `Face` (`Inter-Regular.ttf`) across 256 interleaved samples at 180 KB
  offset 0 (`p50 = 697,961 ns`, `p99 = 732,505 ns`), 180 MB / 30 M words / 43,946 leaves at offset
  0 (`p50 = 697,047 ns`, `p99 = 735,296 ns`), and 180 MB near the end (`p50 = 697,753 ns`,
  `p99 = 727,547 ns`), with a 128-leaf control (`53,041,922 ns`, `76×` slower) in **CI run
  `45106234334` on `3b54969`**.
  *Amended 2026-11-24 (D-91): those absolute numbers are one run, not a property of the code.* Two
  runs of the unmodified gate sixteen minutes apart on the same runner class measured
  `p50 = 696,731 ns` (control `90×`) and `p50 = 1,045,075 ns` (control `88×`) for the same
  180 MB-at-offset-0 workload — a `1.5×` spread with byte-identical inputs and identical
  intra-run spread (`< 0.1%`). What the gate establishes is therefore the **invariance and the
  ratio**, which reproduce on every run, not the absolute latency, which belongs to the runner.
  The citations in this file and in `ROADMAP.md` now quote the measured range and say so. Writing the multi-leaf narrow-viewport BiDi test in
  `crates/holonomy-shell/tests/editing.rs` uncovered three pre-existing leaf-seam bugs in
  `crates/holonomy-shell/src/lib.rs`: (1) `tail_bytes` returned `cluster.len()` instead of `0`
  whenever a cluster started cleanly on `at`, extending every non-EOF frame by one cluster into the
  next leaf; (2) `measure_window` lacked the tail twin of `D-39`'s `head_whole`, so whenever
  `Draft::spans_in` clipped a sealed tail leaf to `end`, `span.offset() + span.len() <= end` held
  and overwrote the next leaf's full measured height with the clipped prefix's height; and (3)
  `cluster_after` hopped across a leaf boundary without checking `ctx.joins(first)`, causing
  `Delete`, `Move::Right`, and `legal_caret` at the end of a leaf to absorb the first cluster of
  the following leaf. All three are fixed and pinned in `crates/holonomy-shell/tests/editing.rs`.

- **2026-11-22 — D-77 — UAX #9 bidirectional reordering, bijective primary carets, and split
  selections close OQ-11 and Block 5's third gate.**
  `scripts/gen-bidi-tables.py` extracts UCD 18.0.0 `DerivedBidiClass.txt` (`766` disjoint ranges)
  and `BidiBrackets.txt` (`130` entries) into `crates/holonomy-eye/src/bidi/tables.rs`, and
  `crates/holonomy-eye/src/bidi.rs` implements O(N) UAX #9 paragraph/explicit/weak/neutral/implicit
  level resolution (including stack-based `BD9` isolate-run assembly and `N0` canonical angle
  brackets, verified offline against `91,707 / 91,707` `BidiCharacterTest-18.0.0` cases). A fast
  `may_reorder` check (`< U+0590`) keeps pure-LTR layout allocation-free (`Line::bidi_boundaries ==
  None`). On wrapped lines that reorder, mapping logical boundary `k` to the higher-embedding-level
  adjacent cluster's edge (`trailing(k - 1)` when `L_{k-1} <= L_k`, else `leading(k)`) pairs the
  `(m + 1)` logical boundaries bijectively onto the `(m + 1)` visual boundary slots (`0..=m`),
  preserving `L-8` (`hit_test(paint_point(g)) == g`) with zero duplicate-`x` ambiguity; logical
  selections that span mixed runs emit split `SelectionSpan`s in visual `x` order; and
  `holonomy-shell::measure_window` accumulates `Carry.x` in logical byte order so leaf-by-leaf
  wrapping matches continuous layout. Verified in **CI run `45106234334` on `3b54969`**:
  `HOLONOMY_SCRIPT_GATE_HONESTY scripts=14 clusters=45 carets=59 carets_inside_a_grapheme=0`,
  `HOLONOMY_SCRIPT_GATE_L8 boundaries=27680 disagreements=0`, and
  `HOLONOMY_SCRIPT_GATE_ORDER scripts=6 clusters=40 displaced=0 note=uax9-visual-order`.

- **2026-11-21 — D-75 — the file records what was written, never what the document *is*.**
  Reading `WorldLine`'s record grammar rather than its docs: there are exactly two record kinds,
  `RECORD_SEGMENT = 1` and `RECORD_WITNESS = 2` (`holonomy-line/src/lib.rs:28`). A segment record
  carries its plaintext and the authenticated plaintext length; a witness carries the MMR over
  segment leaves. Neither carries a position, an order, or an edit — the file is an append log and
  nothing else. That is sufficient for the durability claims the gates make, and insufficient for a
  *document*. `store::spine_of` builds the load-time spine from `segment_lengths()` in file order,
  and `Spine::from_slabs` preserves that order, so **append order becomes document order**.
  The consequence is not corruption: every segment authenticates, `verify()` passes, no byte is
  lost. It is that editing before the end of a document is not durable as an *edit*. Type
  `world`, save, put the caret at 0, type `hello `, save — the session reads `hello world`; the
  reopen reads `worldhello `. Pinned by
  `a_document_typed_before_its_end_reads_in_document_order_and_reopens_in_append_order`
  (`holonomy-gui/tests/persist.rs`), which asserts today's behaviour so that fixing it is a red
  build with a name on it rather than a silent change. Measured green in **CI run 44991335879 on
  `e6a3b90`**, whose job posted the real numbers to issue #51:
  `HOLONOMY_PERSIST order live="hello world" reopened="worldhello " segments=2 witnesses=3`.
  Found 2026-11-21 while tracing OQ-14; filed as OQ-29.

- **2026-11-21 — D-76 — a deletion is hidden for one session and undone by the next open.**
  OQ-14 recorded that a deletion does not erase, which is true and is the deliberate cost of an
  authenticated append-only log (OQ-2, Vision III). Audit of what it *does* do found the sharper
  statement: it is not persisted at all. `Typing::delete` on sealed text calls
  `Spine::remove_range` and writes nothing (`typing.rs:333`); `Ctrl+S` is `Intent::Flush`, which
  seals and appends and never mentions the spine. So the segment a cut removed stays committed and
  authentic, and because D-75 means the load path has no structure to reload, deleting the last
  four bytes of a saved `deletable` and reopening **brings the text back**. The user is shown
  "saved"; the file never learned about the edit. Pinned by
  `a_deleted_tail_is_hidden_and_returns_on_reopen` (same file), which asserts: a cut writes no new
  segment, the file still verifies every segment it held before the cut, and the deleted text is
  what the next open shows. Measured in the same run: `HOLONOMY_PERSIST erase live="delet"
  reopened="deletable" segments=1 witnesses_after_cut=2 plaintext_bytes_verified=9` — the
  segment count did not move, the witness count did not move, and all nine plaintext bytes are
  still there and still verify. Recorded as OQ-30; OQ-14 is amended to point at it, because "deleted
  text is still in the file" and "deleted text comes back" are different sentences and only the
  first one was on the record.

- **2026-11-21 — D-72 — nothing tested the binary a user actually runs.** Every check in this
  project called a library: `holonomy-text`, `holonomy-eye`, `holonomy-gui`'s `App`, the shell's
  intents. Not one of them ran `main()`, so argument handling, raw mode, the passphrase prompt and
  the save-on-quit path had no end-to-end coverage at all — a regression in any of them would have
  passed every gate. `scripts/smoke-install.sh` closes that: it builds the release binary, drives it
  through a **real pty** under a throwaway `HOME`, and asserts five things — a document is created
  by typing and saved on `Ctrl+Q`; the file begins with the `HOLWAV6` header magic read from
  `format.rs`; no fragment of the typed sentence appears in its bytes; the same passphrase reopens
  it and the sentence is on screen; a wrong passphrase is refused leaving the file byte-identical,
  and an empty one is refused without creating a file (D-63). Verified by **CI run 44930021440 on
  `64899f5`**, where the job passed and posted the real binary's run to issue #51: a 5,629-byte
  sealed document, `HOLONOMY_SMOKE binary=holonomy ... reopen=ok`.
- **2026-11-21 — D-73 — `script(1)` cannot drive a pty, and a smoke test that hangs hides that.**
  The first version of the smoke job used `script -qec` to give the child a terminal. It hung on CI
  for 25 minutes and had to be left to time out. Reproduced locally with a stand-in before changing
  anything: the child printed `EOF at prompt` having never received the passphrase, because `script`
  delivers end-of-input before the piped bytes are readable and a program reading one byte at a time
  then blocks on a terminal nobody is typing into. The fix is `scripts/drive-pty.py`, which owns the
  pty, writes keys on a schedule, and enforces its deadline with a `SIGKILL` — reporting `exit=None`
  so a hang is distinguishable from a failure. Verified on three paths locally: success, a hanging
  child killed at the deadline, and a non-zero child passed through. The job is capped at 10 minutes
  as well, because a smoke test that hangs is worse than one that fails.
- **2026-11-21 — D-74 — my own leak check passed a leaking file.** The smoke script's plaintext
  assertion searched the document's bytes for the whole 43-byte sentence. A negative control — a
  stand-in writing `the quick brown fox jumps`, a truncated form — **passed it**, which is the check
  failing at the thing it exists for. It now searches four fragments, and the same control fails.
  Recorded because it is the second time this session a test verified itself rather than the code,
  and because the pattern is specific: an assertion that matches one exact long string is weaker
  than it looks, since a partial leak is still a leak.
- **2026-11-21 — D-70 — L-6 physical qualification: deferred by scope, with an owner and a named
  test.** What is *evidenced*: short-write injection at every append byte, both synchronous failure
  modes, torn-tail recovery, and a real ext4 loop filesystem driven out of space
  (`scripts/qualify-ext4-enospc.sh`, its own CI job whose name says "not power-loss
  qualification"). What is **not** evidenced: a controlled power cut at arbitrary instants,
  write-cache/controller behaviour beneath the filesystem, and any device matrix. The ENOSPC
  rehearsal proves the *refusal* is clean; it cannot prove the *torn write* is recoverable, because
  a loop filesystem still flushes through the host page cache. This is deferred, not waived: the
  write ordering the suite will test is frozen now with a witness per append, and the suite itself
  is **owned by Block 6** (kill-power-at-random-instant) and **Block 10** (all three OSes at the
  release gate). L-6 stays `spec`, exactly as `docs/THREAT-MODEL.md` §5 states to a reader. A green
  gate would need hardware this project does not control, and holding Block 3 open for equipment
  access — rather than for an unproven design claim — is the trade recorded here.
- **2026-11-21 — D-71 — L-10 client and backend scope: deferred, and the law says which client it
  means.** The measurement is real and runs every push: the ignored rsync harness transfers a
  ~60 MB file, appends ~263 KB, and asserts the client's literal data stays within appended bytes
  plus one 64 KiB block while application-layer bytes stay within 8× appended. CI run 44923942692
  printed `HOLONOMY_RSYNC_MEASUREMENT client=rsync 3.4.1 protocol version 32` with
  `delta_application_bytes=334907`. What that establishes is *one client, one transport (a local
  rsync daemon over TCP), one build, one workload*. It says nothing about other clients, cloud sync
  services, or remote backends, any of which may re-read the whole file or re-download it.
  **Owned by Block 10**: either a backend matrix runs there, or L-10 is explicitly narrowed in
  `LAWS` at the release gate to name the client class it covers. The GATES budget table renders
  this row with its `measured_by` and its `owes`, so the narrowing sits in the contract rather
  than in a reader's memory. L-10 stays `spec`.
- **2026-11-21 — D-68 — an idle window rewrote its whole screen every 1.5 seconds.** GATES law L-5
  is "zero repaint and zero text-path timers while the view is unchanged", and the window broke the
  first clause on every loop iteration. `Window::draw` already returns `Ok(false)` when nothing is
  dirty, and `crates/holonomy-gui/tests/keys.rs` asserts exactly that ("an unchanged window
  repainted") — but the binary wrote `app.draw()?;` and discarded the bool. The loop then blocks
  1.5 s in `readable(PAUSE_MILLIS)` for input; on a timeout it saves if needed and `continue`s
  straight back to a complete `screen_with` render and `flush`. So a window left open and idle
  emitted the full surface — every cell of an 80×24 grid, `\x1b[K` and `\r\n` per row — every
  1.5 s, for as long as it stayed open. Not a few cells: a whole screen, string-built, on a window
  where nothing had happened. The library was honest and tested; the front end was not. Fixed by
  comparing what `screen_with` actually reads — size, status line, caret, panel — and skipping the
  write when all of it matches and `draw` reported no repaint. The size is compared because a
  taller terminal repaints no cell and would otherwise keep the old frame until a keystroke came.
  **The harness was verified by breaking it, not by reading it**: a throwaway commit made
  `unchanged` return true unconditionally, CI run 44920267815 failed with
  `tests::an_idle_window_writes_nothing ... panicked at "a repaint must reach the terminal"`, and
  the sabotage was reverted (`c17c3b7` → revert `git revert`). Verified green by **CI run
  44919655611 on `914bbd1`**.
- **2026-11-21 — D-69 — L-5 stays `spec`, and the reason is a scope, not a failure.** With D-68
  fixed, both of L-5's clauses hold for the window that exists: no repaint while unchanged (D-68's
  harness now fails the build on regression) and no text-path timer, because the crate starts none —
  `intent.rs` notes that "IME, timers and the paint callback are the window's". The law is still not
  promoted to `gated`, because L-5 names **Block 6**, and Block 6's deliverable is the GPUI window
  with real glyph painting. A benchmark on the character-grid window would measure the absence of a
  renderer, not the absence of repaint in the shipped app. What the promotion owes is named: the
  same decision, enforced inside the GPU paint path when M4b-ii lands, on a window that can actually
  paint. Recorded so the next reader does not mistake "the test is green" for "the law is met".
- **2026-11-21 — D-66 — the crate had no RTL coverage at all, and OQ-11 was describing one
  problem as two.** Block 5's third gate was written as a single unmet item, "Bi-directional and
  multi-script text handle cluster boundaries correctly". Reading `layout.rs` and the generated
  property tables separates it into two claims of very different status. **Grapheme honesty was
  already true**: Arabic harakat (U+064B–U+065F) and Hebrew niqqud are `gcb=Extend` in
  `cluster/tables.rs`, so a mark joins its base and no caret can land inside a grapheme, in any
  RTL script, because the data is the database's (D-27) and not a hand-typed table. **Paint order
  was entirely absent**: `layout.rs` places every cluster at `x += advance` and reverses nothing,
  so a pure Hebrew run is drawn left-to-right. The gate conflated a property that held with one
  that did not, and — the part worth recording — **there was no RTL text in the crate's tests
  outside its property tables**. A writing instrument claiming lifetime-scale use had never laid
  out a Hebrew word. Found by asking what "BiDi" meant in the code rather than accepting the
  audit's phrasing. New harness `crates/holonomy-eye/tests/script_gate.rs`: 14 corpus lines over
  Hebrew, Arabic, Thaana, Syriac, N'Ko, Adlam and the Arabic presentation forms, with the
  expected cluster counts taken from UAX #29 and **independently checked against Unicode
  properties before being written down**. Verified by **CI run 44915351909 on `7fb5632`**, where
  step 8 ("Measure Block 5's script gate") succeeded — which proves all three
  `HOLONOMY_SCRIPT_GATE_*` lines were emitted, because the trailing `grep`s fail the step if any
  is missing.
- **2026-11-21 — D-67 — the first run of the new harness was wrong, and the code was right.**
  The corpus asserted three clusters for Thaana `U+0780 U+0783 U+07A9`; the crate returned two.
  U+07A9 THAANA EEBEEFILI is `Mn` — a combining mark — so it is `Extend` and joins U+0783, and
  **two is the standard's answer**. My expectation was the bug, not the segmentation. The fix is
  the corpus, and the mistake is recorded rather than quietly amended because it is evidence for
  the practice: a table checked against the standard catches the test author as readily as the
  code. Verified by CI run 44914737196 on `cb4c2b3` (test job green, all 14 lines).
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
  the point of reading the code rather than the prose. Docs only; **verified by CI run 44909881863
  on `e4c193e`, every job green.**
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
- **2026-11-21 — OQ-27 — the window leaks one DEM key per opened document. CLOSED (30dbcd1).**
  `WorldLine` now owns its `DEMKey` directly (`DEMKey::duplicate()` is used only for explicit
  `verify` and compaction), so `Document` needs neither `Box::leak` nor self-reference and every
  key zeroizes on drop.
- **2026-11-21 — OQ-28 — the passphrase typed at the prompt is zeroed, but it was never a
  `Zeroizing`. CLOSED 2026-11-22.** `read_passphrase_from` in `crates/holonomy-gui/src/bin/holonomy.rs`
  now pre-allocates `Zeroizing<Vec<u8>>` to `MAX_PASSPHRASE_INPUT_BYTES` (so keystrokes never
  reallocate on the heap and whole UTF-8 scalars pop on Backspace) before NFC-normalizing into
  `holonomy_seal::PassphraseInput`, and `Document::{create, open, compact_to}` normalize all UTF-8
  passphrases through `PassphraseInput` so canonically equivalent NFD and NFC passphrases derive
  the identical key.
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
  half the screen the terminal gives it. CLOSED 2026-11-22.** `scripts/gen-cluster-tables.py` now
  extracts the 129 disjoint UCD 18.0.0 two-column ranges (`WIDE_RANGES`, attributed in
  `THIRD_PARTY.md` + `LICENSES/unicode-width-MIT.txt`) and `holonomy_eye::cluster_cells` computes
  `0`, `1`, or `2` terminal columns (handling East Asian Wide/Fullwidth, Hangul, default emoji,
  ZWJ emoji sequences, regional-indicator flag pairs, and `FE0E`/`FE0F` selectors; `Ambiguous` = 1).
  Both `CellShaper::advance` and `term::screen_with` (via `CellSurface::cluster_at` and
  column-aware truncation) read `cluster_cells`, pinned in `holonomy-eye` and
  `holonomy-gui/tests/terminal.rs`.
- **2026-11-26 — OQ-24 — shaping now parses and applies OpenType `GDEF`/`GSUB`/`GPOS`. CLOSED
  2026-11-26 (D-95).** `crates/holonomy-eye/src/opentype.rs` reads `GDEF` (`GlyphClassDef`), `GSUB`
  (`SingleSubst`, `LigatureSubst`), and `GPOS` (`PairPos` Formats 1 & 2, `MarkBasePos`, `MarkLigPos`)
  and `holonomy-eye::raster::{Face, FontSet}` apply shaped glyph substitutions, kerning pairs, mark
  attachments, and style-matched (`Regular` / `Bold` / `Italic` / `Mono`) line metrics while
  `PixelSurface` renders from `Frame::picture()` so paint and hit-testing keep one geometry (`L-8`
  gated). Arabic contextual joining & complex Indic reordering remain bounded to the subset covered
  by `GSUB` ligature/single lookups + UAX #9 (`OQ-11`).
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

- **2026-11-27** — **Test-harness flake closed: mock X11 server vs. client teardown.**
  `full_x11_wire_protocol_loopback_handshake_render_events_and_clipboard` failed once on macOS on
  the post-merge run of `2bbcfe2` and passed on rerun. The fault was in the harness, not the
  product: the client stops polling and drops its socket as soon as it has what it needs, so the
  mock server's next `write_all` can land on a closed peer, which macOS surfaces as `EPIPE`; the
  `.unwrap()`/`.expect()` turned an intended teardown into a panic. The mock now treats
  `BrokenPipe` / `ConnectionReset` / `UnexpectedEof` as the end of its script and still panics on
  every other error, so a real wiring fault is not masked. Two structural fixes came with it: the
  `GET_INPUT_FOCUS` event batch is queued into one buffer (a partial batch could otherwise be
  delivered if the first write raced), and a truncated final request ends the script instead of
  panicking. Client-side assertions are unchanged, so the test still proves the handshake, the
  scanline `PutImage` framing, the queued events and the clipboard round-trip.

- **2026-11-27** — **Second flake in the Xvfb qualification job, recorded so it is not
  re-investigated.** The post-merge run of `09aa0e6` failed `X11 GUI live Xvfb qualification`
  with exit 100 and passed on `gh run rerun --failed` with the identical tree; the same job
  passed on the pre-merge run of the same commit. The change under test was Win32-only, so no
  X11 code was in the delta. This is the second flake in the display-server qualification path
  this week (the first was the mock X11 harness, closed in `5d2c375`), and both point the same
  way: the failure is in the timing of the live X session, not in the product. Nothing was
  changed for it — there is still no evidence of a product defect, and guessing at a fix would
  hide the next real one. If it recurs, the next step is to capture the script's own output
  through the diagnostics bridge rather than to loosen an assertion.

- **2026-11-27** — **Blurry rendering on scaled Windows displays: the process was DPI-unaware.**
  Reported symptom: every glyph, icon and hairline in the native window looked soft while other
  applications on the same screen were crisp. Measured from a screenshot, a one-pixel icon stem
  spanned five columns (`252, 222, 33, 186, 229`) instead of resolving to a solid core with one
  antialiased neighbour — the signature of a *smooth* resample, not the integer pixel-repeat the
  UI scale path performs at 200 %. The resample was not in Holonomy: the process declared no DPI
  awareness anywhere (no `SetProcessDpiAwareness*` call, no application manifest, no `.rc`), so
  Windows treated it as DPI-unaware, reported a 96 dpi client area, and bitmap-stretched the
  finished window to the real display scale. The renderer had been drawing a correct 96 dpi
  picture that the OS then blurred. Two related faults shared the same root: `GetDeviceCaps`
  could only ever answer 96 dpi, so the document view's own `View::dpi` never saw the real
  display either; and because `ScaledWindow` presents a *logical* framebuffer, the Win32 blit
  passed equal source and destination rectangles to `StretchDIBits`, which smooth-stretches
  whenever the client area is not the framebuffer size. Fixes, smallest-first: (1) opt into
  per-monitor awareness before the first window exists — `SetProcessDpiAwarenessContext`
  (PER_MONITOR_AWARE_V2) resolved from `shcore.dll`, falling back to `SetProcessDpiAwareness`
  and then `SetProcessDPIAware`, with `ERROR_ACCESS_DENIED` treated as "already pinned", so the
  call is idempotent and harmless on any Windows version; (2) take the window DPI from
  `GetDpiForWindow`, which tracks the monitor the window is actually on and so also fixes
  dragging between displays of different scale, with `GetDeviceCaps` retained as the fallback;
  (3) crop an oversized framebuffer to the visible client area in `blit_viewport` rather than
  handing both rectangles to GDI, so pixels reach the screen exactly as rasterized. Awareness
  also stops the OS from stretching a requested client area, so `physical_size_hint` re-expresses
  the requested layout size in device pixels to keep the window the same apparent size, and the
  frame is re-requested from `SetWindowPos` if the window lands on a non-primary monitor. The
  geometry is pure and covered by tests in `platform.rs` (one-to-one, crop, empty, and four
  scale conversions including `u16` clamping); the FFI paths are compile- and review-verified
  only, since a scaled Windows display is not available to this audit. Layout constants, corner
  radii and the 1 px rules are untouched, so nothing about the 96 dpi picture moves.

- **2026-11-27** — **`D-102` bug hunt (post-design audit, before merge).** Seven defects found
  and fixed, each with a regression test where the behaviour is observable:

  1. **The bare-header rule leaked into popups, the drawer and the banner.** Removing the resting
     pill globally made the history drawer's `Live` / `PinBase` / `DiffPrev` and the recovery
     banner's `Dismiss` render as unstyled body copy — the fill was the only thing telling the
     user those words were pressable. `draw_toolbar_button` now takes a `ButtonSurface`: `Bare`
     for header controls, `Chipped` for popup rows, drawer actions and the banner.
  2. **Home painted an empty pane at the window minimum.** A 5:6 card body is 378 pt tall from
     `y = 110`, and the grid loop breaks on the first card that does not fit, so at 480×360 the
     library showed nothing at all. Bodies are capped to the available pane, and the paper stack
     is derived from the pocket position rather than from `body_h` independently.
  3. **The settings sheet was not modal to the pointer.** It painted over the cards without a hit
     entry covering itself, so a click on the sheet fell through to the card underneath and opened
     a document behind the overlay.
  4. **`Enter` in an empty search box opened a document.** An empty query matches every entry, so
     pressing Enter on a freshly focused field opened whichever document sorted first and dropped
     the user out of Home without a choice.
  5. **A sort control that lied.** `manual_order` was labelled "Manual order" but only reverses
     the catalogue's name order; there is no stored manual order. Renamed `reverse_order`, labelled
     `A–Z` / `Z–A`.
  6. **Text painted outside its container** in two places: folder-card labels used a `card_w − 4`
     inset (over the neighbouring card) and breadcrumb crumbs were bounded only by their own 180 pt
     cap, so long names ran under the `Find` button.
  7. **`Favorites` claimed "No documents yet"** in an empty state where no document list exists.

  Also hardened while auditing: an out-of-range `active_folder` no longer indexes `folders` on the
  paint path (`is_some_and` guard), and the settings sheet skips rows the clamped height cannot
  hold instead of painting past its edge.

  Process note: the Actions log hosts were unreachable from the fixing sandbox for these runs and
  the diagnostics bridge (`gates`-gated on eleven jobs) did not fire, so the formatter's patch was
  recovered by temporarily emitting it as `::error::FMTDIFF…` workflow annotations — readable
  through the API — then reverting that probe. The workflow file is byte-identical to `7e554a1`.

- **2026-11-26** — **`D-102`: design convergence to quiet.md, Holonomy first.** The Home shell
  was rebuilt on the reference's own geometry — 30 px rail rows at radius 9 with a 14 px inset and
  only the *active* row filled (the previous build washed every row, which read as a column of
  buttons), a one-row 56 px header with a 17 px title and four bare 30 px glyphs (`+`, grid/list,
  search, `...`), and an inline search field that is painted only while focused or non-empty. The
  card grid now uses the reference's folder-page arithmetic: 24 px pane inset, 20 px gutter, 4:5
  body columns from a 208 px minimum clamped to 1–4, folders drawn as three fanned papers behind a
  pocket with the label *below* the stack, documents as white sheets with metadata-only preview
  rules. The toolbar lost its resting pill entirely: only `on` states fill (quiet wash + 1 px
  border at radius 8), so the writing view no longer reads as a wall of grey chips. List rows were
  flattened to text over hairlines and the phantom hardcoded `"Open"` label — never a separate
  control — is gone. An out-of-range `active_folder` now degrades to "no filter" instead of
  indexing `folders` on the paint path.

  What the reference asked for and Holonomy refused: previews stay data-free line patterns seeded
  from folder metadata, never decrypted excerpts (`L-2`); the header hides the search field at
  rest but *every* painted target still has exactly one hit rect from the same arithmetic
  (`L-8`); the empty state distinguishes Favorites from an empty library rather than claiming "No
  documents yet" where no document list exists. `HOME_CARD_H` was deleted because the card height
  is now derived from its column width. Open caveat carried forward: the `⋯` overflow still drops
  rows in very short windows (needs a layout rewrite, not exercised by CI).

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
