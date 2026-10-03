# Holonomy

A word processor that stays responsive on documents of 2000+ pages, by rendering
only the sections near the caret and resolving scroll geometry in Rust.

Rust for storage, geometry and the desktop shell; TypeScript for the editor
surface; a plain-TS UI with no framework. The editor and the CRDT are Tiptap and
Loro — this repository is the document model around them, not a reimplementation
of either.

## What is here

| | |
|---|---|
| `crates/holonomy-core` | SQLite store, section ordering, height geometry, zstd interop fixture. 106 tests. |
| `crates/holonomy-shell` | Tauri 2 window, the bridge commands, the generated contract, Typst PDF export. 74 tests. |
| `app` | The editor and the application. Vite, plain TS, no framework. 138 tests. |
| `spikes/` | M0–M4 measurement spikes and what they concluded. |
| `Plan/2000.md` | The original architecture proposal. |

`STATUS.md` records every architecture decision with the measurement that produced
it, and what is still owed. `DOCTRINE.md` records ten rules, each with the failure
that caused it.

## The shape of it

A document is a list of **sections** of 1500 words / 3000 marks. Only the sections
near the viewport are mounted, each in its own Tiptap editor instance. The
scrollbar needs to know every section's height, including the 640 that have never
been rendered, so heights live in a Fenwick tree on the Rust side and the frontend
gets a manifest at boot rather than a lookup per scroll event.

```
        frontend (TypeScript)                     Rust
   ┌───────────────────────────────┐   ┌──────────────────────────┐
   │ editor instances              │   │ holonomy-core            │
   │   one per mounted section     │   │   SQLite: zstd JSON      │
   │ scroller + LocalGeometry      │   │   Fenwick height tree    │
   │   (synchronous, no IPC)       │   │   u64 order keys         │
   │ toolbar + shortcuts           │   │   assets, keyed SHA-256  │
   │ AssetResolver (refcounted)    │   │ holonomy-shell           │
   │ export panel                  │   │   Tauri window           │
   └──────────────┬────────────────┘   │   bridge commands        │
                  │                    │   HoloWorld (typst)      │
                  │  get_document_boot │   typst translate+pdf    │
                  │  ─ MessagePack     │                          │
                  │  sync_section_..   │                          │
                  │  ─ JSON, debounced │                          │
                  │  commit_section_.. │                          │
                  │  ─ JSON, rare      │                          │
                  │  export_pdf        │                          │
                  │  ◀─ progress event │                          │
                  ▼                    └──────────────────────────┘
           user scrolls
```

Zero IPC on the scroll hot path. `LocalGeometry` runs in the frontend and mirrors
the Rust rule exactly, so a scroll event never waits on a round trip; Rust holds the
authoritative tree and the frontend reconciles against it in debounced batches.

## Why some of it is the way it is

Almost every non-obvious choice here came out of a measurement, and several
contradict the plan this started from. The short version:

- **One editor per section, not one editor with content swapped in.** Measured 10–100x
  cheaper for window slides. `spikes/m0-section-seam/FINDINGS.md`
- **No Fenwick-tree optimisation on the critical path.** A linear scan measured
  0.46µs at 667 sections, which is 0.003% of a frame. The tree is retained because
  it is already written and because the linear path is gated under 0.5ms — not
  because a prefix-sum tree is required. `Plan/2000.md` says it is; that is wrong at
  this size.
- **`block_count` is stored, not derived.** Estimating paragraph count from character
  count measured 225% error on short multi-paragraph sections and 41% on long sparse
  ones. No fixed density fixes it, because the paragraph ratio spans 1 to 20.
- **The height constants are measured, not reasoned about.** `GeometryCalibration`
  is fitted against real rendered sections: 8.3% mean error, and a chrome of 72.6px
  against a CSS-predicted 58px.
- **No version history.** Undo is 500 in-memory ProseMirror steps and nothing else.
- **Cross-section selection is a known limitation** of the multi-instance strategy.
  Accepted rather than fixed; a virtual selection layer was judged not worth its
  maintenance and accessibility cost.
- **ts-rs generates the bridge contract.** Seven TypeScript shapes were hand-written
  mirrors of Rust structs. They are generated now, and a test fails if the checked-in
  file stops matching the Rust definition.

## PDF export

An export is a Typst compile, and it is three pieces:

- **`core/export/translate.rs`** turns section JSON into Typst markup. It is a pure
  function, which is why it is testable by comparing strings — a failed compile
  points at a line in a generated `holonomy.typ` that does not exist on disk, which is
  a worse diagnostic than the string itself.
- **`core/export/world.rs`** is `HoloWorld`, a `typst::World`. It answers
  `holo-asset://<sha256>` out of the `assets` table and **nothing else** — no relative
  path, no absolute path, no `file://`. That is a security property, not a
  simplification: a document is user data that arrives by sync, and if `file()`
  honoured paths an imported document could name `file:///etc/passwd`. The reachable
  set is exactly the bytes this application chose to store, keyed by a digest it chose.
  No temporary files are written anywhere, per the reason: a figure-heavy document is
  hundreds of megabytes, and a temp copy means a second copy on disk, a cleanup path
  that has to be right on every crash, and a window in which a half-written temp file
  answers an export.
- **`core/export/pdf.rs`** drives the two and reports what it cost.

**Fonts are bundled, from `typst-assets`.** Libertinus Serif for the body, New
Computer Modern for equations, DejaVu Sans Mono for code — all SIL Open Font License,
all already in the dependency graph through `typst`, so nothing is checked in as a
binary blob and nothing is downloaded at build time. System fonts are still read, but
**after** the bundle, and a system font that duplicates a bundled one is skipped rather
than appended. So a family in the bundle renders as the same bytes on Linux, macOS and
Windows, and a family *outside* it renders as whatever the machine has. The earlier
version named no family at all, on the grounds that appearance "should follow the
system" — which makes two machines produce different documents from the same bytes,
the opposite of what a word processor is for.

**The export is long and says so.** A 1.33M-word document (667 sections) measures:

| phase | measured | whose cost |
|---|---|---|
| translating JSON to Typst | **60 ms** | Holonomy's |
| Typst layout, 2,541 pages | 18,577 ms | Typst's |
| writing 7 MB of PDF | 3,645 ms | Typst's |

So the panel shows four labelled steps rather than a percentage: the phases are 60 ms
and 18,577 ms, and a bar computed from those ratios would read as stuck and then jump.

**Cancel stops it.** Typesetting runs in a separate process, so a cancel is a kill rather
than a request Typst never sees: **180 ms** measured on the corpus, against 18.6 seconds for
the layout it replaced. Typst 0.15.1 offers no interruption point — `compile` takes a `World`
and nothing else, and layout cannot be chunked — so a process boundary is the only mechanism
that does not need Typst's cooperation. A cancel during serialisation still does nothing, and
that is deliberate: discarding 7MB of finished PDF and handing the user nothing is the worse
failure. Both are stated in the code and asserted in
`tests/export-progress.rs` rather than left as a surprise.

`translate_ms < 500ms` is the enforced gate, because that is the part a change here
could regress. The whole-document budget is `#[ignore]`d with the measured numbers on
it: **5 seconds is not met, and the gap is not in this codebase.** See `STATUS.md`.

## Running it

Requires Rust, Node, and the Tauri prerequisites for your platform. On Debian or
Ubuntu that is `libwebkit2gtk-4.1-dev`, `build-essential`, `curl`, `wget`,
`file`, `libssl-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`.

```bash
# everything at once
cargo test --workspace
cd app && npm install && npm run test:all

# the app
cd app && npm run tauri:dev        # vite on :5184 + a Tauri window

# production bundle (deb, rpm, AppImage)
cd app && npm run tauri:build

# the editor surface in a plain browser, with a synthetic document
cd app && npm run dev              # then open http://localhost:5184
```

### Tests

| command | what it covers |
|---|---|
| `cargo test --workspace` | store, ordering, geometry, bridge commands, contract staleness, export, fonts, backup |
| `cd app && npm test` | M3 editor core: undo, boundary traversal, columns — 39 |
| `cd app && npm run test:scroll` | M4 viewport and compensation — 24 |
| `cd app && npm run test:blocks` | that a caller cannot omit a block count — 4 |
| `cd app && npm run test:boot` | boot, zstd interop with Rust, the boot geometry — 15 |
| `cd app && npm run test:heights` | height batching: throttle, not debounce — 12 |
| `cd app && npm run test:lifecycle` | cut points, caret anchors, split/merge — 25 |
| `cd app && npm run test:persist` | the LRU, the generation guard, the eviction flush — 18 |
| `cd app && npm run test:wire` | the MessagePack bytes `rmp-serde` actually writes — 5 |
| `cd app && npm run test:assets` | asset URLs, and the absence of base64 in a document — 9 |
| `cd app && npm run test:math` | equation nodes, and KaTeX on input it cannot parse — 15 |
| `cd app && npm run test:export-math` | TeX to Typst math, pinned from the real encoder — 12 |
| `cd app && npm run test:export` | the export client: phases, cancel, two overlapping jobs — 13 |
| `cd app && npm run test:ingest` | paste and drop: headers, limits, addressing, `preventDefault`, **and the application's own handler on the real `#scroller`** — 23 |
| `cd app && npm run test:shortcuts` | the shortcut registry, global vs editor, one undo stack — 24 |
| `cd app && npm run test:toolbar` | formatting toolbar state, disabled-not-hidden, DOM — 27 |
| `cd app && npm run test:chrome` | the chrome **wired**: toolbar in the page, chords consumed — 7 |
| `cargo test -p holonomy-core --test holo-file` | `.holo` identity: a JPEG, a foreign database, a v2 file — 7 |
| `cargo test -p holonomy-shell --test tauri-config` | the file association, the CSP's reasoning, and that every declared icon exists — 5 |
| `cargo test -p holonomy-shell --test pagination-parity` | the cross-platform fingerprint and its fixture — 15 |
| `cargo test -p holonomy-shell --test tex-spacing` | spacing commands, and one dead arm that was a live bug — 4 |
| `scripts/smoke-production.sh` | the **built** binary, in-engine, under the production CSP, plus a two-process single-instance probe — 52 |
| `cd app && npm run test:calibrate` | re-fits the height model; reports mean error; **refuses to report if no embedded face loaded** |
| `cd app && npm run test:node` | the twelve suites that need no browser — what CI runs before Playwright exists |
| `cd app && npm run test:browser` | the five that need Chromium and a dev server, in order |
| `cd app && npm run test:all` | typecheck, then `test:node`, then `test:browser` |
| `scripts/verify-engine.sh` | the M4 suite again, inside a real Tauri window |

The browser suites need Vite on `:5184`. The in-engine verification drives a real
window and needs `WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1` and
`GDK_BACKEND=wayland`, both of which `scripts/verify-engine.sh` sets for you.

### Regenerating

Two generated artifacts, both derived and both committed:

```bash
# the height model, after any typography change
cargo run --release --bin emit-calibration > app/public/calibration.json

# the Rust <-> TypeScript bridge contract, after any struct change
scripts/gen-bridge-types.sh
```

The second is order-insensitive but prints a diff when the output changed, and
`crates/holonomy-shell/tests/bridge-contract.rs` fails if the committed file has
drifted from the Rust definitions.

There is a third committed fixture, read from both languages rather than regenerated by
hand:

```bash
# the zstd frame Rust writes and the renderer decodes, plus the JSON it holds
cargo run -p holonomy-core --example gen_interop_fixture
```

`app/test/fixtures/rust-zstd-frame.bin` is read by a Rust test that requires Rust to
decode it back to the committed JSON, and by a TypeScript test that requires `fzstd` to
produce the same. A decoder that mangled a multi-byte character would pass either one
alone.

## Cross-engine

Every number in M0/M1/M4 comes from Chromium. The strategy they justify was
unverified on the engines that ship, so the M4 suite also runs inside a real Tauri
window and reports which renderer produced the result — from Rust, never from the
user agent, because webkit2gtk's is unreliable.

| engine | platform | state |
|---|---|---|
| webkit2gtk 60.5 | Linux | **56/56** passing, in a release binary under the production CSP |

`cargo clippy --workspace --all-targets --release -- -D warnings` is clean. It was
27 warnings short of clean the first time it ran, which is the more useful fact — a CI
workflow written but never executed is a statement of intent. One of the 27 was not
cosmetic: an `unreachable_pattern` on the spacing matcher pointed at a dead arm
whose comment said the *opposite* of what the live code did, so `\!` — a negative
thin space, which must emit nothing — had quietly become a positive one. Alongside
it were 79 lines of `frac`/`sqrt`/`left`/`right` arms appended without removing the
originals. There is deliberately no `cargo fmt --check`: the codebase is not
rustfmt-formatted, and adopting it would be a several-thousand-line diff unrelated
to anything under test.
| WKWebView | macOS | **not run** — `.github/workflows/ci.yml` runs it; it has never been executed |
| WebView2 | Windows | **not run** — same |

webkit2gtk agrees with Chromium closely enough that the fitted height model
transfers unchanged: identical column width, identical convergence deltas, mean
predicted/measured ratio 0.945 on both. That answers the risk M0 flagged, and
clears neither of the other two engines.

## Cross-platform pagination parity

Bundling the fonts made "the same pages come out everywhere" checkable, so there
is now a fingerprint that a runner can emit and three runners can be compared on.

`crates/holonomy-shell/tests/pagination-parity.rs` compiles a deterministic
54-page fixture and hashes, per page, every text run's position, size, font and
glyph sequence. `.github/workflows/ci.yml` runs that on
`ubuntu-22.04`/`macos-14`/`windows-latest`, uploads each fingerprint, and a
fourth job compares all three — as a Rust test, because "do these agree, and if
not which page" is the part that can be wrong, and `diff` over pretty-printed
JSON is a false failure waiting to happen.

Two things about this worth knowing before trusting it:

- **It is not a PDF comparison, and cannot be.** `typst_pdf` writes a
  `/CreationDate`, so two runs a second apart produce different bytes from
  identical layout. The fingerprint is taken from the laid-out document, before
  serialisation.
- **Two of its parts cannot be verified on one machine, and are not claimed to
  be.** Position quantisation and glyph ids exist only for differences *between*
  machines; removing either leaves all 15 tests green here. What is tested is the
  rule (a rounding that is not symmetric about zero folds the two halves of the
  page together) and font selection (a second family must appear in the recorded
  font list). `STATUS.md` has the table.

The comparison has never been run, because there is one machine here.

## What the first CI run actually found

The workflow ran. Five of six jobs failed, and every failure was in the harness rather than in
the product — which is the useful shape for a first run to have.

| job | what happened | cause |
|---|---|---|
| `frontend` | died in 15s | `test:all` includes five **browser** suites, and it ran them before Playwright was installed and before anything served `:5184` |
| `release smoke` (all three) | died in under a minute | `cargo tauri build` is a CLI subcommand and the CLI was never installed on the runner |
| `rust` (macos-14) | failed after 10m | `FONT_DIRS` was Linux's three directories, so the macOS font book held only the bundle and the ordering test could not observe what it asserts |

Three more were latent — no job had reached them:

- **`windows-latest` could not have built at all.** `tauri-build` takes the first
  `bundle.icon` entry ending in `.ico`, falls back to a literal `icons/icon.ico`, and **returns
  an error** if it is absent (`tauri-build-2.7.1/src/lib.rs`, the `target_triple.contains(
  "windows")` block). The list held three PNGs. `icon.ico` and `icon.icns` are now committed
  and declared, and `tests/tauri-config.rs` asserts both the presence of a `.ico` and the
  existence of every declared icon — a repository test, so it runs on the platform where the
  failure cannot be reproduced.
- **The smoke script could not have run on Windows.** `run:` defaults to PowerShell there, which
  does not execute shell scripts, so `./scripts/smoke-production.sh` was a line PowerShell
  accepted and silently did not run — on a leg named "release smoke". The step now pins
  `shell: bash`, and the script itself dropped `python3` (absent on the Windows image), `/tmp`
  (absent), and an unqualified `GDK_BACKEND`, which is a GTK variable that means nothing on
  WKWebView or WebView2.
- **Node 22.** The suites run under `--experimental-strip-types`. Node 24 is what the local
  machine runs and what the lockfile is resolved against.

The macOS and Windows legs are still **[INFERENCE]**: the fixes are read off the sources and
the diff, not observed passing. That is what the next push is for.

## What is owed

- **WKWebView and WebView2 verification.** Both need hardware this machine does not
  have. Not a code problem — the suite is engine-agnostic and one definition, and the
  CI matrix runs it. The workflow **has now been executed once**, and both legs failed on
  harness defects rather than on the product; those are fixed above and the two legs have
  not yet been observed green.
- **The `.holo` file association reaches no user's file manager yet.** It is declared in
  `tauri.conf.json` and `tests/tauri-config.rs` keeps the declaration honest, but
  registration happens when an installer runs, and the smoke test builds with
  `--no-bundle` because AppImage tooling fails for reasons unrelated to the app. The
  smoke report prints that the claim is open rather than implying otherwise.
- **Single-instance protection needs a D-Bus session, and degrades without one.** The
  plugin claims a well-known bus name; on a session with no bus it swallows the failure and
  the app runs normally with two instances possible. That is the right direction — two
  instances are strictly better than refusing to start — but the guarantee is not met
  there, and `scripts/smoke-production.sh` fails its second-launch probe rather than
  pretending otherwise. The probe itself is the only check in the suite that needs two live
  processes, and a probe that could not run is reported as a failure rather than a skip.
- **The 5-second PDF budget.** Measured at 22.4s for the full 667-section corpus, of
  which **60ms is Holonomy's** and the rest is Typst laying out 2,541 pages
  single-threaded. The gate is `#[ignore]`d with the numbers written on it. Getting under
  5s needs ~510 pages/second, nearly four times Typst's rate on this hardware, and none of the
  three routes to it is a change in this repository. `STATUS.md` has the arithmetic for
  each.
- **A document cannot be set in a font outside the bundle.** The font book is the bundle
  and nothing else, so nothing is read from the OS and the same bytes paginate the same on
  all three platforms. A document naming a family the bundle lacks is *not* refused — Typst
  warns `unknown font family` and lays it out in the bundled default, so it exports and the
  substitution is reported. What is given up is the ability to set a document in a face
  neither machine happens to have.
- **Cancel is coarse.** It takes effect at the next phase boundary, so cancelling during
  layout waits out the layout (45 of the 54 seconds), and cancelling during serialisation
  does nothing at all, because that is the last boundary. Typst's `World` exposes no
  per-page callback; a real interrupt needs a different renderer, or a page-count
  estimate to cancel against.
- **Matrices, cases and `\begin{...}` in equations are not translated.** An unrecognised
  command survives to be named by Typst, so the document refuses to export and says
  which command — it is never silently wrong. `app/test/export-math.ts` asserts the set
  of failures is exactly the recorded set, so a fix and a regression both fail it.
  `docs/parser-backlog.md` has the argument, and the four constructs this was written to
  excuse turned out to be bugs in the translator rather than Typst limitations — three of
  which *compiled*.
- **M6 sync.** Loro, materialised per section.
- **Session restore.** The boot payload reports no caret or scroll position because
  nothing stores them yet. It returns null rather than a guess.
- **`holo-asset://` does not dispatch on webkit2gtk 2.60.** The handler is registered
  correctly and is never entered; `STATUS.md` has the measurement. Images resolve through an
  IPC fallback, and the verification reports which transport carried each one.
- **Shell icons.** `crates/holonomy-shell/icons/` holds placeholders.

## The document file

A document is one SQLite database with the extension `.holo`. It is self-describing:
`PRAGMA user_version` carries the format version in the file header, readable with a
100-byte read and no Holonomy code, so a tool that has never heard of Holonomy can
still tell what a file is. (The `meta.schema_version` row validates a file; the header
*introduces* one. Both are written in one statement so a migration cannot half-succeed.)

Opening the wrong file produces a sentence a person can act on. The classification
reads the first sixteen bytes before SQLite touches the file — `PRAGMA journal_mode`
writes a header, so classify afterwards and a renamed JPEG reports "that is a
database, but not a document", which points the user at the wrong fix. And a file that
is not a document is *refused*, not migrated into: `init` would otherwise write eight
Holonomy tables into somebody's `invoices.db`, from a double-click.

Double-clicking one goes through two paths, because the platforms genuinely differ:
macOS delivers a percent-escaped `file://` URL in `RunEvent::Opened`, and that variant
does not compile on Linux or Windows, which pass the path as `argv` instead. Both end
in the same function.

## Licence

Not yet chosen.
