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

### M1 — The frame as a picture

`Frame` today is text + layout + caret. A renderer needs a view model it can consume without
reaching into the Eye: for each visible line, its box; for each cluster, its byte and x; the caret
rect; the selection rects; and the total height. Deliver it as `Frame::picture()`.

**Acceptance (headless):** an ASCII rasteriser in `holonomy-shell`'s tests draws each frame into a
grid of cells and asserts, over the generated-document corpus the Block 5 gates already use:

- every cluster occupies exactly one set of cells, with no cell claimed twice and none skipped;
- the caret rect lands on a cell that belongs to the caret's own cluster (or the space before it at
  a line start), at every zoom × DPI the one-geometry gate covers;
- a selection covers exactly the clusters between its ends, and nothing else;
- line boxes tile the viewport with no gap and no overlap, and their count equals the lines the
  `HeightModel` says are visible.

Prints `HOLONOMY_PICTURE documents=… frames=… cells=… carets=… selections=…`; the gate is claimed
when that line is green and the counts are non-trivial.

### M2 — Intents as data

The window will need one entry point, not twenty methods: `Intent` (type text, move caret, select,
scroll, resize, zoom, undo, redo, save, quit) and `Effect` (read clipboard, write clipboard, seal,
notice). `Editor::apply(Intent) -> Vec<Effect>`.

**Acceptance (headless):** the mapping is *behaviour-preserving* — for a randomised intent stream,
`apply` and the equivalent direct method calls produce byte-identical frames, snapshot by snapshot
— and no intent panics, loses the caret, or leaves the caret outside the text it offers.

### M3 — A real shaper (Block 8, the half Block 5 waits on)

`Shaper` is a trait; the only implementation in the tree is a test double. This step adds
`holonomy-shape`: font loading, shaping, metrics, fallback, cluster segmentation that agrees with
`Clusters`. No window, no GPU — shaping is a pure function and can be gated headlessly.

**Acceptance (headless):** the one-geometry gate's matrix (documents × widths × zooms × DPIs) re-run
with the real shaper; widths within the font's own metrics; CJK and emoji clusters measured whole;
fallback for a missing glyph chosen deterministically; and Block 5 gate 2's *timing* half measured at
30 M words, which is the clause its checkbox has been waiting for.

### M4 — The window (Block 6, first half)

`holonomy-gui` on GPUI: window, event loop, keymap, scroll, resize, caret blink, clipboard, the IME
input path. It draws M1's picture and speaks M2's intents. No unlock screen yet and no autosave —
this step is "a window you can type in", opened on an in-memory document.

**Acceptance:** CI builds it with the platform libraries (`wayland`, `xkbcommon`, `vulkan`, `fontconfig`)
in a dedicated job — a green build is the gate that the pixel layer is real. Headless: a
`WindowlessApp` drives key events through the same mapping the window uses and asserts the frames
match `Editor`'s. Manual, on your machine: open, type a paragraph, undo, redo, scroll to the end and
back, resize the window, copy and paste.

### M5 — Open, save, unlock, survive

Create-with-passphrase, drop a `.wavefunction`, open, autosave on pause, seal on quit, resume. The
crash-consistency laws (L-6 and company) already hold for the core's commit path; this step makes
the *window* use that path, and nothing else.

**Acceptance:** the kill-at-random-instant suite driven through the GUI's save calls; the hand-off
drill automated as far as a headless run can (copy the file mid-session, open it in a fresh process,
continue, verify every byte); and the negative case — a wrong passphrase reports a refusal, not a
crash.

### M6 — Chrome, honestly (Block 7 and Block 9)

The toolbar of Block 7, and the security panel of Block 9 reporting what is resident, locked and
wiped, and the world-line's quorum state — read from the same index the tests assert, never a
hard-coded string.

**Acceptance:** every number the panel shows is produced by a function with a unit test; a panel
claim that disagrees with `ProtectedReport`/`LockReport` fails a test.

### M7 — The gates that remain

Block 5 gate 3 (BiDi): decide — implement runs, or record the deferral with the reason; it is
OQ-11. Block 3's two deferred clauses. Zero-CPU-repaint-while-idle. The full VISION budget table.
Each one either prints its numbers green or moves to `AUDIT.md` as an open question with a reason.

### M8 — Packaging

A release binary per platform, an install line that works, a smoke script that opens, types, saves
and reopens a document under a temporary home, and a README that tells the truth about what is and
is not protected.

## What could derail this, and the fallback

GPUI's system dependencies on a CI runner are the most likely hard stop: it wants Wayland/X11,
xkbcommon, Vulkan, fontconfig. If the build cannot be made green, the fallback is
`winit` + `softbuffer`/`wgpu` + `holonomy-shape` — the same crate boundary, a different pixel sink,
and `ROADMAP.md` records the change as an open question rather than a quiet swap. M1 and M2 are
deliberately renderer-agnostic: they survive that fallback untouched.

## Where we are

M0 done. M1 next: it is pure logic, it can be proved headlessly, and it is the last thing that can
be settled before a font and a window are in the way.
