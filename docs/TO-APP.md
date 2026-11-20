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

M0, M1, M2 and M3 done. M4 next: the window itself — GPUI, the event loop, the keymap, the IME
path — drawing M1's picture and speaking M2's intents, with M3's metrics behind the text.
