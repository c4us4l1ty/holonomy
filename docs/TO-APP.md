# TO-APP — the road from here to an editor you can open and write in

`ROADMAP.md` orders the *blocks*: the intellectual sequence, where each block is a law the next one
must obey. This document orders the *delivery*: the same gates, cut into steps small enough that a
single CI cycle can settle whether the step is done, and arranged so that the last steps are the
only ones that need a display.

## What "complete" means

The app is complete when, on a machine that is not this repository's CI:

1. a person can open the window, create a document with a passphrase, and type into it;
2. what they type is what they see — the caret sits where the next letter will go, the selection
   covers exactly the letters it says it covers, and scrolling does not move text that did not move;
3. undo and redo walk the same history the headless core walks today;
4. closing the window — or losing power mid-keystroke — loses nothing committed and reopens to the
   last keystroke that reached disk;
5. the file opens on a fresh account with only the passphrase (the hand-off drill);
6. the remaining gates in `ROADMAP.md` are claimed on numbers CI printed, and the ones deliberately
   not claimed are documented as deferred with a reason;
7. `cargo install`-shaped instructions put that binary in front of a user.

Nothing on that list is "a beautiful page". The beauty is `docs/UI-SPEC.md` §8's acceptance table
and Block 6's business; this roadmap exists to get a *working* window in front of it.

## The constraint that shapes the order

There is no Rust toolchain in the environment where this work is being done, and no display. CI is
the compiler (~6–10 minutes per push), and CI cannot show a window either. So every step below is
written to be settled by one of two kinds of acceptance:

- **headless** — a harness that runs in CI and *prints numbers*: geometry, cells, frames, bytes;
- **compile + checklist** — the step only makes sense with a GPU, so CI proves it *builds* with the
  platform libraries present, and a written checklist (six or seven actions) is the manual half.

A step that would need "look at it and tell me if it looks right" without a harness is a step that
is not ready to be attempted.

## The steps

### M0 — A frame that agrees with itself — **done** (AUDIT D-39)

A frame is a function of (document, viewport, scroll, selection): frame it twice and you get the
same picture. This was not true — a second frame at the same scroll started 16 bytes early, because
the head leaf of a window is handed over clipped and its carry was written back as the leaf's own.
Fixed; the regression asserts the whole snapshot across two frames at four scroll positions.
*Why first:* every later visual claim is checked against a frame, and a frame that disagrees with
itself makes a renderer bug look like a core bug and a core bug look like a renderer bug.

### M1 — The frame as a picture — **done**

`Frame::picture()` returns `holonomy_eye::Picture`: an owned, drawable view model — line boxes, a
flat cluster list with per-line ranges, the caret, and the selection as runs of each line.
`Layout::selection_spans` is where selection became *geometry* rather than a pair of offsets a
renderer re-derives; a partial cluster paints whole, stated once, there.

**Acceptance (headless, `crates/holonomy-shell/tests/picture.rs`), by assertion:**

- every cluster owns its cells once — no double claim, no cluster with no width, nothing outside the
  wrap width — over four scroll positions;
- line boxes start at zero, each begins where the last ended, each is one line box tall, their sum
  is the picture's height and the layout's;
- the caret lands on its own cluster's left edge (or on a line end that offers one) for every
  clickable cluster on the visible lines;
- a selection paints exactly the clusters between its ends — checked in both directions, painted
  and unpainted — and selecting everything starts every line at the margin.

The runs print `HOLONOMY_PICTURE pictures= clusters= cells=`, `HOLONOMY_PICTURE_CARETS carets=` and
`HOLONOMY_PICTURE_SELECTION painted= unpainted=` under
`cargo test -p holonomy-shell --test picture -- --nocapture`. The counts are asserted in the same
run — four pictures, more than a hundred clusters, ten or more carets, both painted and unpainted
clusters present — so a green run is a run that drew something and found it consistent. All four
tests are green on `main`.

### M2 — Intents as data — **done**

`Intent` and `Effect`, and `Editor::apply` as the one door a window speaks through:
`Intent::{Type, Backspace, DeleteForward, DeleteSelection, SelectAll, Click, DragTo, ScrollTo,
ScrollBy, Resize, SetView, RevealCaret, Undo, Redo, Flush, Reflow}`, answering with
`Effect::{Redraw, Caret, Saved}`. The door has no opinions: each intent calls the method a headless
caller would, so the window cannot invent an editing rule the core does not have.

**Acceptance (headless, `crates/holonomy-shell/tests/intents.rs`), by assertion:** two identical
editors are driven for 240 randomised steps — one through `apply`, one through the direct calls —
and after **every** step their whole visible state is compared: text, origin, height, line count,
caret, selection, document length, measured total, scroll, width, viewport and view. Alongside: the
caret never leaves the document, a frame's caret always agrees with the editor's, and no intent
sequence can panic. A second test pins the effects to the truth — a scroll that does not move, a
resize to the same size and a backspace at the top of the document ask for **no** repaint; typing
asks for a repaint and reports the caret; a flush says the bytes landed.

The run prints `HOLONOMY_INTENTS intents= redraws= carets= saves= bytes=` under
`cargo test -p holonomy-shell --test intents -- --nocapture`, and the counts are asserted: 240
steps, more than a hundred redraws, more than ten caret reports, at least one write, and a document
that is still non-empty at the end. Green on `main`.

### M3 — A real face — **done**

`holonomy_eye::Face` reads a real OpenType font's metrics — `head`, `hhea`, `hmtx`, `maxp` and
`cmap` — and implements `Shaper`, so proportional advances, the font's own ascent and descent and a
real character map stand behind every layout. The font is the one the previous editor shipped
(`holonomy-old/assets/fonts/Inter-Regular.ttf`, SIL OFL, licence beside it), compiled into the test
binary. The parser is a few hundred lines of integer arithmetic in the Eye: no dependency, because
the workspace lockfile is committed and CI builds `--locked`, so adding one is a supply-chain
decision rather than a convenience.

**Acceptance (headless, `crates/holonomy-eye/tests/real_face.rs`), by assertion:**

- the file parses and reports the design units, glyph count and mapped-character count of a real
  text face;
- every printable ASCII character maps to a glyph and has a width — 95 of them;
- the face is **proportional** — `i` is not `m`, and neither is `W` — so a monospace assumption
  anywhere in the geometry would show up as a failure instead of a coincidence;
- scaling to twice the size doubles the advance to within one sixty-fourth of a unit, and a
  cluster's width is the sum of its characters' advances;
- a hard break has no width, and line height is ascender minus descender with ascent inside it;
- a paragraph laid out with the real face keeps the Eye's law: `hit_test(paint_point(c)) == c` for
  every caret the layout offers, at real metrics rather than a test double's.

Prints `HOLONOMY_FACE …`, `HOLONOMY_FACE_SCALE …` and `HOLONOMY_FACE_LAYOUT …` under `--nocapture`;
the counts are asserted (95 printable ASCII advances, ≥5 scalings, ≥4 sums, >50 caret round trips,
≥2 lines), so a green run cannot be a run that read no font.

**What this is not:** shaping. Kerning, ligatures, mark positioning and Arabic joining live in
`GSUB`/`GPOS`, which nothing reads yet; a cluster's width is the *sum* of its characters, which is
exactly right for one-glyph-per-character text and a measurement — not a promise — elsewhere. That
gap is recorded as **OQ-24** in `AUDIT.md`, and Block 5 gate 2's timing half can now be measured
with these metrics.

### M4 — The window (Block 6, first half) — **half done**

**M4a, done: everything a window needs that is not pixels.** `crates/holonomy-gui`:

- **`Keymap`** — `Key` + `Modifiers` to `Intent`, the one place a keystroke becomes an edit:
  characters type, Enter breaks, arrows move (`Shift` extends, `Ctrl+Home/End` reach the document's
  ends), `Ctrl+Z/Y` undo and redo, `Ctrl+A` selects all, `Ctrl+S` writes through, PageUp/PageDown
  scroll a screen. A key the map does not use is silence, not an error.
- **Caret navigation in the loop** — `Intent::Move { to, extend }` and `Editor::move_caret_to`:
  left and right step one *cluster*, up and down keep the column, Home/End are the line's ends. The
  geometry moves go through the same `hit_test` a click does, so a keyboard caret lands only on
  boundaries a renderer drew.
- **Typing bursts are one undo step** — the app lays a mark when a run of typing begins, so `Ctrl+Z`
  takes back the run. This closed **D-40**: before it, `Ctrl+Z` after typing reported nothing to
  undo.
- **`App`** — editor, keymap, surface, and a dirty flag, so a repaint happens exactly when
  something changed.
- **`Surface`** — the pixel sink, in the advances the Eye measured (a backend never decides a
  cluster's width), and **`CellSurface`**, a text-grid sink so the whole stack can be run and read
  back with no GPU at all.

Acceptance (headless, `crates/holonomy-gui/tests/keys.rs`): typing reaches the frame and the
surface; arrows move a caret and keep the column; shift selects and the next key replaces; shortcuts
are the loop's own; an unchanged window does not repaint. Counts asserted and printed as
`HOLONOMY_WINDOW*`.

**M4b-ii — pixel window & GPUI status (`2026-11-25` update, AUDIT D-94):** GPUI itself remains
unbuilt in this offline `--locked` tree (adding ~200 transitive crates requires crates.io resolution
and `Cargo.lock` regeneration). Without adding any external crate, `crates/holonomy-eye/src/raster.rs`
and `crates/holonomy-gui/src/{x11.rs,canvas.rs}` now provide a pure-Rust/`libc` OpenType
`loca`/`glyf` rasterizer (`Inter-Regular`, `Inter-Bold`, `Inter-Italic`, `JetBrainsMono-Regular`),
`PixelSurface` rendering `Frame::picture()` directly (`L-8` pointer round-trips pinned across zooms
and DPIs in `canvas::tests`), and a pure-`libc` X11 desktop window (`holonomy gui <doc>` or
`holonomy-gui`, keeping PTY `holonomy <doc>` as the default CLI path). Cross-platform GPUI
(`macOS`/`Windows`/Wayland), `GSUB`/`GPOS` shaping (`OQ-24`), and live X-server screenshot tests
remain open.

**M4b-i, done: the terminal window.** The window this environment *can* build. `holonomy-gui`
gains `term.rs` — a `CellShaper` whose unit is one grid cell (so the Eye's geometry and the screen are
the same thing, and paint, hit-test and caret agree to the last cell), a byte→key `Decoder` for what
ttys actually send (UTF-8, `ESC [ A`, xterm modifier parameters, `ESC [ 3~`, control bytes), and an
ANSI renderer that is a pure function of what was drawn, so tests assert exactly what a terminal
prints. `cargo run -p holonomy-gui --bin holonomy` opens it: raw mode via `libc::tcgetattr`, the
window size via `ioctl(TIOCGWINSZ)`, a read loop into `App::press`, `Ctrl+S` to flush, `Ctrl+Q` or
`Ctrl+C` to quit, and a status line. It adds no dependency — `libc` is already in the lockfile for the
seal's page locking.

What it is: an app you can open and type in. Text appears on the measured geometry, arrows move the
caret (with shift to extend), `Ctrl+Z` takes back a burst, `Ctrl+S` writes the open leaf through. What
it is not: durable. The document lives in memory for the session, so closing the window loses it —
that is M5, and the memory store in the binary is exactly the socket M5 replaces with
`holonomy_line::WorldLine`. GPUI (M4b-ii) stays one `Surface` implementation, for a machine that can
fetch crates; this file is not that machine.

The acceptance is `crates/holonomy-gui/tests/terminal.rs`: tty bytes → `Decoder` → `App` →
`CellSurface` → ANSI renderer, asserting the letters in the cells, the caret after them, the status
line underneath, backspace, arrows, `Ctrl+Z`, and a resize. It found **D-42** on its first run and
**D-43** on the next (see `AUDIT.md`): the arrows were typing letters, and a single arrow press threw
the whole undo history away. Both are fixed in the layers that had them — the decoder, and the
session's seal — which is the argument for driving a front end end to end rather than trusting unit
tests to compose.

**D-41, fixed.** A frame used to stop at the *sealed* spine's end, so bytes typed into the open leaf
were invisible until something sealed them — a window that showed nothing while you typed. The frame
now reaches the document's live end (`provisional`, because the open leaf has no measured height
yet), and the write-back drops a measurement for a leaf the index does not have instead of writing
past its end. Its reproducer is a plain green test: five keystrokes, five bytes, five clusters on the
surface, the caret after the last one.

### M5 — Open, save, unlock, survive

Create-with-passphrase, drop a `.wavefunction`, open, autosave on pause, seal on quit, resume. The
crash-consistency laws (L-6 and company) already hold for the core's commit path; this step makes
the *window* use that path, and nothing else.

**Acceptance:** the kill-at-random-instant suite driven through the GUI's save calls; the hand-off
drill automated as far as a headless run can (copy the file mid-session, open it in a fresh process,
continue, verify every byte); and the negative case — a wrong passphrase reports a refusal, not a
crash.

**Done — the window has a file.** `holonomy-gui::store` is the application glue the commit-boundary
test said an app crate would write: a `Document` over a `.wavefunction` world-line, a `DocumentSink`
that appends one sealed leaf per seal (one append, one witness — the path L-6 measures), a
`DocumentSource` that faults one segment in on demand, and `Document::spine`, which builds the
geometry from the *authenticated segment lengths* so opening reads no plaintext at all
(`WorldLine::segment_lengths`, new). `Typing::resume` starts a session over that geometry with the
caret at the document's end and no history: the file is the floor, exactly as a seal leaves it.

The terminal binary now takes a path: `holonomy notes.wavefunction` creates the file (echo-off
passphrase prompt, `calibrate()` recording this host's Argon2 schedule) or opens it (public header
first, recorded schedule replayed, capsule yielding the DEM key or one coarse refusal, witness chain
authenticated before a byte is shown). `Ctrl+S` saves, a 1.5 s pause autosaves, and quitting seals
what is open — a quit is a save.

**Evidence,** `crates/holonomy-gui/tests/persist.rs`, seven tests over real files: a session
survives its window; a reopened session resumes where the file left off; a wrong passphrase is one
coarse refusal (`StoreError::Refused`, one sentence, no distinguishable halves — AC-3) and the
file still opens with the right one; a copy taken mid-session opens where it was copied; and
bytes past the last committed witness — what a kill mid-append leaves — are reported by
`Document::trailing_bytes`, discarded, and the document accepts new appends afterwards. The fixtures
create documents under `ParamsClass::Test`'s schedule so the suite does not spend 256 MiB per open;
the container itself (Argon2, capsule, witness chain, `fsync`) is real in every one.

**Mid-document edits, deletions, and explicit compaction are durable.** Append-only world-lines
keep version-1 witnesses; once a cut, mid-segment insertion, or deletion across sealed text is
saved, the world-line writes a sealed `RECORD_MANIFEST` of `(segment, start, len)` spans and a
version-2 witness (`OQ-29`, `OQ-30`). Deletions write **zero** replacement segments, open seeks
over all segment bodies and superseded manifests, emptying a document commits a zero-slab manifest
so reopen yields `""`, and `holonomy compact <src> <dst>` re-confirms the passphrase and writes a
fresh, re-keyed world-line holding only the surviving active spine (`OQ-14`). Both persistence
invariants continue to print `HOLONOMY_PERSIST order` and `HOLONOMY_PERSIST erase` on every push.

**Both key-hygiene residuals in M5 are closed.** `WorldLine` owns its `DEMKey` directly so no key
is leaked across document opens (`OQ-27`), and passphrase prompts pre-allocate a bounded
`Zeroizing<Vec<u8>>` before NFC-normalizing into `holonomy_seal::PassphraseInput` (`OQ-28`).

### M6 — Chrome, honestly (Block 7 and Block 9)

The toolbar of Block 7, and the security panel of Block 9 reporting what is resident, locked and
wiped, and the world-line's quorum state — read from the same index the tests assert, never a
hard-coded string.

**Acceptance:** every number the panel shows is produced by a function with a unit test; a panel
claim that disagrees with `ProtectedReport`/`LockReport` fails a test.

**Done — the panel says only what it can measure.** `holonomy-gui::panel` assembles `Facts` from the
live `ResidentSet` (resident, allocated and budget bytes, resident leaves), a real
`probe_capability()` `mlock` of a probe page in this process (reported in the rung's own words,
`LockCapability::claim`, with `describe_how` explaining the mechanism), and the world-line's
committed segment and witness counts, its recovery report, and the result of a full `verify()` —
which is asked for when the panel opens, never assumed. `Facts` lines are pure functions of its
fields, so the tests mutate one field at a time and watch its line change; a hard-coded claim cannot
pass. `None` renders as "not verified", a partial verification as "verified 2 of 5", and bytes are
formatted by integer arithmetic (`1.5 KiB`, `1023.9 KiB`) so no number is rounded into a lie.
`Ctrl+G` toggles the panel above the status line; the toolbar this release has is the status line
itself (file, bytes, segments, saved/unsaved) plus that panel, which is what a terminal can show
honestly.

**Evidence:** `crates/holonomy-gui/tests/panel.rs` — the residency half against a real
`ResidentSet`; the file half against a real world-line with a real `verify()`; the lock half against
every rung, asserting each renders its own claim and no two rungs render alike; and the formatter's
boundaries.

**Deviation, stated:** the milestone names `ProtectedReport`, which does not exist in this codebase —
the equivalents are `holonomy_seal::LockReport`/`LockCapability` and the resident set's own
accounting, and those are what the panel reads. There is no separate "quorum" state in the
world-line (a single-writer append log has none); the witness count and the verification result are
what can be said, and the panel says only that.

### M7 — The gates that remain

Block 5 gates 2 & 3 (`OQ-12` viewport latency & `OQ-11` BiDi): **closed 2026-11-22 (D-77, D-78).**
`holonomy-eye` implements full UAX #9 bidirectional resolution and line reordering over generated
UCD 18.0.0 tables with bijective primary-caret boundaries (`displaced = 0` and `L-8` `27,680`
boundaries / `0` disagreements in `tests/script_gate.rs`, plus split `SelectionSpan`s and
multi-leaf logical `Carry.x` in `holonomy-shell`), and `tests/viewport_latency.rs` measures the
real OpenType `Face` (`Inter-Regular.ttf`) identically across 180 KB offset 0, 180 MB (30 M words)
offset 0, and 180 MB near the end (`76–90×` faster than a 128-leaf control). The absolute figure is
quoted as a range rather than a constant since D-91: two runs sixteen minutes apart measured
`p50` of 696,731 ns and 1,045,075 ns for identical inputs, while the invariance and the ratio
reproduced both times.
OpenType `GSUB`/`GPOS` contextual glyph shaping remains `OQ-24`.

Zero-CPU-repaint-while-idle: **the defect is fixed and the harness enforces it (D-68, D-69).** An
idle window used to rewrite its entire screen on every loop iteration; it now compares what
`screen_with` reads and skips the write, `tests::an_idle_window_writes_nothing` pins every way that
decision can be wrong, and the harness was verified by deliberately breaking the guard and watching
CI fail. L-5 itself stays `spec` because it names Block 6's GPU window — D-69 records what the
promotion still owes rather than letting a green test stand in for a met law.

Block 3's two deferred clauses: **written down as decisions (D-70, D-71), not left in a roadmap
checkbox.** D-70 records that the ENOSPC rehearsal proves the *refusal* is clean but cannot prove
the *torn write* is recoverable, because a loop filesystem still flushes through the host page
cache — so L-6's physical qualification is owned by Block 6 and Block 10 and stays `spec`. D-71
records exactly which client L-10's number covers (rsync 3.4.1, local daemon over TCP, one
workload) and that every other client and backend is Block 10's, or an explicit narrowing of the
law.

The full VISION budget table: **now generated, not written.** `BUDGETS` in
`crates/holonomy-ledger/src/laws.rs` maps VISION §4 onto the law table and onto the harness that
measures each row, `xtask gates` renders it into `GATES.md`, and `gates --check` fails the build if
the two disagree. Block 9's `L-7` wipe row is now measured on every push by
`holonomy-text/tests/wipe_gate.rs` (`< 1 ms` trip-to-verified across a 4 MiB `ResidentSet`, live
`DEMKey`, and 32 zeroize canaries with `ghosts = 0` and `0` allocations; `D-79`), leaving only one
row — `L-9`'s cross-machine portability — read **"nothing yet"**; as of 2026-11-24 (D-92) its
measured column names `scripts/qualify-l9-portability.sh`, which proves the process half of the law
on one host and says `L-9=partial` in its own output, with the second-machine half owned by Block
10's release matrix. No remaining M7 item is unaddressed: each one either prints its numbers green
or moved to `AUDIT.md` with an owner and a reason.

### M8 — Packaging

**Partially done (2026-11-21, extended 2026-11-24).** The parts that need one Linux machine and no
distribution are built; the parts that need signing, a second platform or a signing identity still
are not — but there is now a pipeline, and it publishes nothing it has not first run.

**Done — the release pipeline (D-90).** `.github/workflows/release.yml` builds the binary from the
pinned toolchain and the committed lockfile, **runs `scripts/smoke-install.sh` against the artefact
before packaging it**, and only then assembles
`holonomy-<version>-x86_64-unknown-linux-gnu.tar.gz`: `bin/holonomy`, the licences and
`THIRD_PARTY.md`, a `README-RELEASE.txt` that states in its first line that the build is unsigned,
experimental and for one platform, a `.sha256`, and a `.provenance.txt` recording the commit, ref,
target and the fact that no signature exists. Publishing is reachable **only** by pushing a `v*`
tag — there is no manual path that can name a release after a branch — and the artefact is attached
to a GitHub release whose body says what it is. A `workflow_dispatch` run builds and attests
without publishing. Two consequences are deliberate: a tarball whose binary failed the smoke test is
never uploaded, and "there is no packaged release" is no longer true, so the README's row says
*unsigned, one platform, no installer* instead. **L-9 does not move:** its bound is the same file
opened on a second machine, and a tarball is the prerequisite for that drill rather than the drill.

**Done — the smoke test (D-72, D-73, D-74).** `scripts/smoke-install.sh` builds the release binary
and drives it through a real pty, giving it a passphrase, a sentence and `Ctrl+Q`. It asserts that
the document is created and sealed, that it begins with the `HOLWAV6` magic, that **no fragment** of
the typed sentence is readable in its bytes, that the same passphrase reopens it with the sentence
on screen, that a wrong passphrase is refused without touching the file, and that an empty one is
refused without creating a file. It runs in CI on every push as its own job, and it is the first
check in the project that runs the binary a user runs rather than a library under it. The pty comes
from `scripts/drive-pty.py` because `script(1)` hangs (D-73).

**Not done — and each needs something this environment does not have:**

- **A release artefact per platform.** Linux x86_64 now exists (D-90). macOS and Windows still need
  runner time, a second toolchain and code signing, and nothing is signed on any platform: the
  artefact carries a `.sha256` and a `.provenance.txt` and says in its own README that there is no
  signature to check.
- **An install line that works.** `cargo install --path crates/holonomy-gui` would work for someone
  with the toolchain; a user without one has no path, and the README says so.
- **L-9's portability claim.** It needs the same file opened on a second machine, which is the only
  way the whole law can be measured. **The half a single host can carry is now measured (D-92):**
  `scripts/qualify-l9-portability.sh` and the `portability` CI job author a document, copy *only
  the file* into a delivered tree, and import it in a second process under `env -i` with every
  `HOME`/`XDG_*`/`TMPDIR` pointed at a directory that must be empty before the import and empty
  after it — so nothing could have been carried in, and nothing was left behind. It prints
  `HOLONOMY_PORTABILITY L-9=partial same_kernel=yes same_libc=yes second_machine=no`. That rules
  out an account, a token, a device, a keyring entry and a config file; it cannot rule out a
  different kernel, libc, filesystem or architecture, which is what the release tarball on a second
  machine is for. L-9 stays `spec` in `GATES.md`, but its measured column now names this drill
  instead of reading **nothing yet**.

## What could derail this, and the fallback

GPUI's system dependencies on a CI runner are the most likely hard stop: it wants Wayland/X11,
xkbcommon, Vulkan, fontconfig. If the build cannot be made green, the fallback is
`winit` + `softbuffer`/`wgpu` + `holonomy-shape` — the same crate boundary, a different pixel sink,
and `ROADMAP.md` records the change as an open question rather than a quiet swap. M1 and M2 are
deliberately renderer-agnostic: they survive that fallback untouched.

## Where we are

M0, M1, M2 and M3 done. M4a done: keymap, caret navigation, bursts, app loop, a surface. **D-41
fixed**, so a frame shows every byte typed into the open leaf. M4b-i — the terminal window — is done:
the app opens, takes keys, and draws through `term.rs`. M4b-ii, the GPUI adapter, is blocked in this
environment: adding the dependency needs registry access to regenerate `Cargo.lock`, which nothing
here has.

**M5 done (2026-11-22).** A `.wavefunction` is created or opened under an NFC-normalized
`PassphraseInput` (`OQ-28`), `WorldLine` owns its `DEMKey` (`OQ-27`), a seal appends and witnesses,
mid-segment insertions and deletions commit sealed `RECORD_MANIFEST` sub-segment geometry with zero
segment rewrites (`OQ-29`, `OQ-30`), `Ctrl+S` writes through, the autosave fires 1.5 s after typing
stops, quitting seals what is open, a failed save exits non-zero (D-49), and `holonomy verify` /
`holonomy compact` expose full verification and passphrase-confirmed active compaction (`OQ-14`).

**M6 done (2026-11-21).** `Ctrl+G` asks the document for the security panel, and every number in it
comes from the thing it describes — the live resident set, a real `mlock` probe in this process
today, and the file's own counts — not from a constant. `Esc` dismisses it (D-61), a panel taller
than the terminal gives way rather than scrolling the screen (D-48), and no panel line claims
verification unless a full `verify` ran.

The distance left, plainly: the remaining gates (M7), packaging (M8), plus the GPUI backend
(M4b-ii) for a machine with registry access. **Not one of the ten laws in `docs/GATES.md` is
`gated`** — all ten read `spec`, and `xtask gates --check` fails the build if that file and
`crates/holonomy-ledger/src/laws.rs` ever disagree.

What is still not here, in the order it would meet a user: no GPU or font rendering (the window is
a terminal cell grid whose wide characters use UCD 18.0.0 two-cell advances — `OQ-25` closed — and
whose RTL/mixed lines reorder per UAX #9 — `OQ-11` closed — while OpenType `GSUB`/`GPOS` glyph
joining remains `OQ-24`), one document at a time, deleted bytes remain encrypted in the append-only
`.wavefunction` until explicit `holonomy compact` (`OQ-14`), and no physical power-loss
qualification (the L-6 rehearsal is an ENOSPC test and says so in its own job name).
