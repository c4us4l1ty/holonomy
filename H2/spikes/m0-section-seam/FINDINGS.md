# M0 — Section seam spike: findings

**Status:** complete. This decides M1's architecture.
**Corpus:** 700k target words → 1.12M actual, formatting-heavy (1.63 marks per text node),
with headings, lists, tables, code blocks, images, equations. Measured in headless
Chromium 153 via Playwright, Tiptap 3.30.3.

## The question

`2000.md` proposes materialising only the focused section and swapping content as the
user moves around. That leaves one hard problem: what happens *at* a section boundary,
which is where a user spends most of their time typing. M0 built three strategies and
measured them against each other.

| | Strategy A: content-swap | Strategy B: sliding-window | Strategy C: multi-instance |
|---|---|---|---|
| Shape | one editor, `setContent` per section | one editor holding [prev, focused, next] | one editor per visible section |
| Cross-boundary | needs an explicit swap | **native** (real doc boundary) | impossible |
| Frozen protection | none needed | decorations + `filterTransaction` | per-instance `editable` |

## Results

`wordsPerSection` is the central dial. Numbers are p50/p95 ms.

| words/section | sections | strategy | DOM nodes | keystroke p50 | cold parse p95 | **window slide p50** | frozen held |
|---|---|---|---|---|---|---|---|
| 500 | 1400 | A | 51 | 0.4 / 1.0 | 0.2 | 10.0 | – |
| 500 | 1400 | B | 280 | 0.5 / 1.3 | 0.3 | 15.7 | yes |
| 500 | 1400 | C | 285 | 0.3 / 1.4 | 0.2 | **0.3** | – |
| 1500 | 467 | A | 179 | 0.5 / 1.6 | 0.4 | 10.5 | – |
| 1500 | 467 | B | 797 | 0.7 / 1.8 | 0.4 | 31.7 | yes |
| 1500 | 467 | C | 802 | 0.4 / 1.2 | 0.4 | **0.3** | – |
| 6000 | 117 | A | 908 | 0.4 / 1.5 | 1.2 | 44.0 | – |
| 6000 | 117 | B | 2803 | 1.4 / 2.5 | 1.7 | 123.3 | yes |
| 6000 | 117 | C | 2808 | 1.0 / 1.9 | 1.5 | **1.3** | – |
| 12000 | 59 | A | 1862 | 0.5 / 1.8 | 1.6 | 70.9 | – |
| 12000 | 59 | B | 6016 | 2.8 / 4.5 | 2.4 | 200.8 | yes |
| 12000 | 59 | C | 6021 | 0.6 / 1.8 | 1.9 | **2.0** | – |

## Conclusions

**1. Keystroke latency is a non-problem at every size tested.** Worst p50 across the whole
sweep is 2.8ms (B at 12000 words/section), worst p95 is 4.5ms. The `near-zero latency`
goal is comfortably met. This is the one goal that needed no work.

**2. Strategy C (multi-instance) wins decisively, and not for the reason I expected.**
Its window slide is 10–100x cheaper than B (0.3ms vs 31.7ms at 1500 words/section)
because it never re-serialises a document — each editor is already holding its own
section, so "sliding" is just flipping an `editable` flag. B pays for its cross-boundary
elegance on every single slide, because `setContent` tears down and rebuilds the whole
window.

**3. B's cross-boundary advantage is real but I now think it is not worth 100x.**
B is the only strategy where typing across a boundary is a native document operation.
That is genuinely nicer. But it costs 31.7ms per slide at the recommended size, and
slides are the *most frequent* operation after typing. The trade is a permanent tax on
the common path to protect the uncommon one.

**4. Section size should be ~1500 words, not 2000.md's "10–20 pages".**
At 6000+ words/section, every strategy's slide cost roughly triples and keystrokes start
to move (B: 0.7 → 2.8ms p50). 1500 words is roughly 4–5 pages: small enough to keep
slides cheap, large enough that cold-parse frequency stays low, and it keeps sections
small for the Loro CRDT and for sync conflict granularity.

**5. Strategy C's weakness is cross-section selection, and it is narrower than it looks.**
C cannot express a selection spanning two sections. But C also cannot *corrupt* one,
whereas B must actively prevent it with a guard that took four separate bugs to get
right (below). For v1, "selection stops at a section boundary" is an acceptable
limitation. 2000.md §9 already anticipated this ("Start with single-section selection").

## What this changes in the plan

- **Adopt Strategy C**, not the content-swap that `2000.md` §4 describes.
- **Section size 1500 words (~4–5 pages)**, replacing "10–20 pages" / "~20 pages".
  This is the input to M1's Loro benchmark, since section = CRDT doc = sync unit.
- **A: rejected.** Dominated by C on every axis — same keystroke cost, 30x worse slides,
  and it additionally needs explicit swap-and-reposition logic to make boundaries
  navigable at all.
- The Fenwick/prefix-sum geometry layer in `2000.md` §2 is unaffected. All strategies
  need it, and the manifest is tiny either way (53KB for 467 sections).

## Sustained-editing check

The numbers above come from short bursts. A real session is not a burst: a user
opens a document and edits one place for twenty minutes. `sustained.ts` types 2000
edits in the focused section and reports the cost by decile, so drift is visible
rather than hidden in an average.

| strategy | DOM nodes | edits accepted | p50 first decile | p50 last decile | drift | worst p95 |
|---|---|---|---|---|---|---|
| A: content-swap | 180 | 2000/2000 | 0.4 ms | 0.6 ms | 1.5x | 1.2 ms |
| B: sliding-window | 798 | 2000/2000 | 0.6 ms | 1.0 ms | 1.67x | 1.5 ms |
| C: multi-instance | 803 | 2000/2000 | 0.3 ms | 0.6 ms | 2.0x | 1.6 ms |

**No strategy degrades meaningfully over a session.** All three stay at or under
1ms p50 after 2000 edits, with worst-case p95 of 1.6ms. C shows the largest
*relative* drift (2x) but from the lowest base, ending at the same 0.6ms as A.

This strengthens the keystroke conclusion: editing cost is not merely fast, it is
*stable*, which is the property the `near-zero latency` goal actually needs. A
2x drift from 0.3ms to 0.6ms is invisible to a user.

**The measurement initially reported strategy B as 0.00ms and invalid.** That was
the test editing at the raw document midpoint, which for a windowed document lands
in a *frozen neighbour* section. Strategy B correctly rejected every edit, and the
harness read that as "infinitely fast". The `valid` guard caught it. This is the
frozen-section guard from bug 4 below working exactly as designed, and it is the
fifth time in this spike that a guard against measuring nothing has caught a real
defect rather than a hypothetical one.

## Four bugs this spike caught

Each of these produced *plausible but wrong* measurements, which is why the sanity
harness (`sanity.ts`) exists and asserts that the editor is actually editing.

1. **`sectionIndex` silently dropped.** Tiptap ignores attributes not declared in the
   schema. Tagging blocks with `sectionIndex` without declaring it left every section
   boundary invisible and collapsed the windowed document to `docSize: 2`. No warning.
2. **Tables silently dropped.** The corpus generated `tableRow`/`tableCell` but
   StarterKit has no table extension, so `Node.fromJSON` threw a `RangeError` that
   ProseMirror swallowed, quietly producing a much smaller document than intended.
3. **A raw ProseMirror `Plugin` in Tiptap's `extensions` array is ignored.** It must be
   a Tiptap `Extension` returning the plugin from `addProseMirrorPlugins`. The frozen
   guard was never installed and reported "no section boundaries" while believing it
   was protecting them.
4. **`filterTransaction` is a plugin-spec field, not a `props` field.** Nesting it in
   `props` compiles, runs, and is never consulted. Even once correctly placed, deletes
   still escaped: a deletion maps to an *empty* range in the resulting document, so the
   original `fromB === toB` early-return skipped it. The guard now resolves deletion
   ranges against the pre-step document (`tr.docs[i]`).

Bugs 3 and 4 are the dangerous kind: the guard's own logic was provably correct
(it reported `verdict: "FROZEN"` for a position inside a frozen block) while edits
 sailed straight through. A guard that silently does nothing is worse than no guard,
because the architecture is designed assuming it holds.

## Caveats

### The webkit2gtk cross-check: sandbox fixed, driver handshake still blocked

Every number above is from headless Chromium 153. Tauri on Linux uses **webkit2gtk
2.54**, flagged in earlier research as the weakest of the three webviews
(large-DOM drag-select lag, tauri-apps/tauri#3988). So the question stands: does
strategy C's slide advantage survive the weaker engine? **Still unmeasured.**

The review supplied `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1`, which was the
missing piece. It works: with it set, MiniBrowser spawns `WebKitWebProcess` and
`WebKitNetworkProcess` that **stay alive**, where previously the network process
died inside the glycin/bwrap sandbox before any page rendered. Verified by
inspecting the process tree while the browser runs.

Three environment problems were solved:

1. The driver defaults to `/usr/libexec/webkitgtk-6.0/MiniBrowser`, but only
   webkit2gtk **4.1** is installed. Sessions must pass an explicit
   `webkitgtk:browserOptions.binary`.
2. The bwrap/glycin sandbox cannot start in this container.
   `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1` resolves it. Not a security
   concern here: a local test browser loading a localhost dev server.
3. No X11 socket exists (XWayland auth present, socket absent) and **Xvfb is not
   installed and cannot be** (no sudo, `dnf` present but unprivileged). The review
   suggested `xvfb-run -a --server-args="-screen 0 1280x1024x24"`; that path is
   unavailable on this machine. A real Wayland session is (gnome-shell,
   `wayland-0`), so `GDK_BACKEND=wayland` is the equivalent, and MiniBrowser runs
   cleanly under it.

**What still fails:** `POST /session` hangs indefinitely, returning zero bytes,
even with the browser confirmed healthy. `GET /status` responds normally, so the
driver itself is up. Running MiniBrowser directly under
`G_MESSAGES_DEBUG=webkitautomation` produces no errors and no automation-related
output, and the binary does export the automation symbols
(`webkit_web_context_set_automation_allowed`, `webkit_web_view_is_controlled_by_automation`),
so the feature is compiled in. The hang is in the driver's session handshake, not
in the browser and not in the sandbox.

This is recorded rather than worked around because further diagnosis would mean
building WebKitGTK from source, which is out of proportion to the check.

**Assessment of the risk, given it remains unmeasured.** The review's judgement is
that strategy C is safe because it mounts only ~800 DOM nodes and WebKitGTK handles
sub-1000-node trees without Blink-specific regressions. That is consistent with what
the rest of the evidence shows, and two independent properties of C point the same
way:

- C keeps ~800 DOM nodes mounted (measured, above) versus B's single editor over a
  merged document. Small DOMs are the standard mitigation for the webkit2gtk
  weakness that prompted the concern.
- C never calls `setContent` on the hot path, so it avoids the full
  deserialise/rebuild cycle that dominates B's slide cost in *any* engine. The
  10–100x gap is structural, not engine-specific.

The honest position: the architectural conclusion rests on Chromium measurements
plus a structural argument, and the engine-specific confirmation is owed. Per the
review, this does not gate M3.

**Action:** re-run `sweep.ts` and `sustained.ts` inside a real Tauri window on
webkit2gtk, WKWebView, and WebView2 before release. Both harnesses are
engine-agnostic and need no changes.

### Other caveats

- Absolute timings include Playwright/Vite overhead and are pessimistic in that
  sense. The *relative* ordering between strategies is the durable result.
- The frozen-edit guard only matters for B. C uses per-instance `editable`, a much
  simpler mechanism, though C still needs a check that API-level content injection
  cannot reach a non-focused editor.
- IME composition across a section swap is untested. Headless Chromium has no IME.
  This is a real risk for C and should be checked in the Tauri spike.
- The corpus is synthetic. It is formatting-dense and structurally varied by
  design, but real documents with real editing history may behave differently.
  Re-running the sweep against a genuine 2000-page document is the obvious next
  check.
