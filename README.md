# Holonomy

A local-first encrypted document editor that runs on bare silicon. No compositor, no display server,
no runtime configuration files: the binary is statically linked, it draws to a DRM/KMS framebuffer
itself, and once it has opened its container it cannot open anything else.

The project is being built in phases, each one ending in a gate with recorded evidence. It is not
finished. `PROJECT.md` is the build plan and the authority on scope; this file is the map.

---

## What it is

Three things, in this order:

1. **A sealed process.** `main` opens every descriptor it will ever need, `mlockall`s, isolates the
   network, drops privileges, and installs a seccomp filter. The filter contains no `openat`. From
   that point the process has no way to name a file, so the editor cannot be tricked into reading one.
2. **An encrypted container.** `.wavefunction` is exactly 134,217,728 bytes of indistinguishable
   noise. The document, the fonts and the export targets are encrypted chunks inside it, addressed by
   a 32-byte BLAKE2b id and read with `pread64` — an allowlisted call — because `openat` is not.
3. **An editor.** A CAGR text engine (gap-rope leaves, a style interval map, undo, search), a renderer
   with an SSE2 blitter, damage tracking and a surface tree, and export to streaming HTML and PDF.

The key derivation is Argon2id → a verifiable delay function → HKDF → a derived-key root. The VDF is a
CIOS squaring chain `S_i = S_{i-1}² (mod N)` over the RSA-2048 challenge number, hardcoded, because a
VDF that occasionally stalls the machine for an hour is a VDF that gets skipped.

## The one hard constraint

The release binary must fit in **2 MiB**, statically linked, stripped, with `panic = "abort"`. That
number is why there is no TeX engine, no SVG, no JPEG, no WebP, no variable-length tables, and no
sync protocol. It is not an optimisation target that was missed; it is the constraint the design is
derived from, and `crates/holonomy/tests/release_artifact.rs` fails the build if it is crossed.

Current: **1,478,456 bytes**, static-pie, against a 2,097,152-byte ceiling — 618,696 bytes of room.
Measured by the gate itself, so the number above is one `cargo test` away rather than a remembered one.

## Build and run

```sh
# Tests. --release because the artifact gate resolves the binary relative to its own profile.
cargo test --workspace --release

# The shipping artifact.
cargo build --release --target x86_64-unknown-linux-musl

# Headless: drives the session through the jail's own harness rather than a framebuffer.
cargo run --release --target x86_64-unknown-linux-musl -p holonomy -- --headless

# A real window, for development. Behind a feature that is off by default and unreachable from the
# sealed boot chain; the artifact gate fails if any of it reaches a default-features binary.
cargo run --release --features desktop -p holonomy -- --window
```

The live X11 gates need a display and must run single-threaded, because two tests fighting over the
keyboard focus defeat each other:

```sh
HOLONOMY_X11_LIVE=1 cargo test -p holonomy-x11 --test live -- --test-threads=1
```

## Layout

| Crate | What it owns |
| --- | --- |
| `holonomy` | The session, the boot sequence, the binary |
| `holonomy-jail` | seccomp, privilege drop, tripwires, teardown — `libc` only |
| `holonomy-secure` | `SecureBlock`: page-locked, guard-bounded, provably scrubbed |
| `holonomy-crypto` | Argon2id, the VDF, HKDF, the derived-key root |
| `holonomy-container` | `.wavefunction`: 128 MiB of indistinguishable noise |
| `holonomy-assets` | Brotli fonts, the A8 glyph atlas, procedural box drawing |
| `holonomy-text` | CAGR leaves, the style interval map, undo, search, spans, the document payload and its asset catalog |
| `holonomy-geometry` | Fenwick line geometry, font-metric line heights |
| `holonomy-render` | SSE2 blitter, damage tracking, the surface tree, table grids |
| `holonomy-display` | The `Scanout` trait and its backends |
| `holonomy-input` | evdev, a code-based keymap, X11 key translation |
| `holonomy-export` | Streaming HTML, PDF via `pdf-writer` |
| `holonomy-image` | The PNG chunk reader, the fixed-point scaler, the Iceberg cache |
| `holonomy-x11` | An X11 core-protocol client, spoken directly over a unix socket |

`H2/` is a superseded Tauri + TypeScript + SQLite stack. It is excluded from the workspace and is
never a dependency: it is a reference for what to salvage and, mostly, what to refuse.

Fifteen external crates, all vendored into a static musl binary: `argon2`, `blake2`, `chacha20`,
`chacha20poly1305`, `getrandom`, `hkdf`, `sha2`, `libc`, `pdf-writer`, `ttf-parser`,
`brotli-decompressor`, `secrecy`, `zeroize`, `inout`, `unicode-normalization`, `miniz_oxide`.
`holonomy-jail` and `holonomy-x11` depend on `libc` and nothing else.

`miniz_oxide` is the only addition Phase 9C made, and it is reachable from exactly one crate —
`holonomy-image`. PROJECT.md §2.9.5's "no `png` crate" is the reason: a hand-written chunk reader plus
inflate measures **51.6 KiB** of binary, against 80–120 KiB for `png` with `flate2` and `crc32fast`
dragged in.

## Status

**Phase 9 is done and gated, all three halves of it.** Tables (9A), LaTeX math (9B), and the
viewport-bounded image cache (9C). Phases 0–8 were done before that, and 9X — a window a person can
type into on an ordinary desktop without `sudo` — is the designated target.

`Ctrl+T` inserts a 3×3 table and Tab moves between cells. `Ctrl+M` inserts an inline formula: with the
caret outside it the box draws as compiled math — glyphs from Noto Sans Math for the symbols, Inter
Italic for the variables, and 1 px integer fills for the fraction bars and radical overlines — and with
the caret inside it expands in place to the raw LaTeX in monospace. `Ctrl+I` inserts an image.

**Images.** The container format is frozen, so an image lives in the document payload's tail as a
content-addressed catalog entry — `BLAKE2b | w u16 | h u16 | len | PNG` — and its *position* is a
U+FFFC OBJECT REPLACEMENT CHARACTER in the document's own text. That is the design, not an
implementation detail: the frozen entry shape has nowhere to record which anchor an asset belongs to, so
the pairing is positional whether it is wanted or not, and a character in the text is the one anchor
that survives every edit for free. Decoded rasters are page-column-width (§2.9.3's arithmetic), held
in an Iceberg cache bounded at 8.0 MiB with a ±1-page window, and scrubbed on eviction before the next
frame. `Ctrl+I` inserts a committed 1920×1080 chart rather than a file you choose, because the sealed
allowlist has no `openat` on a user path — everything either side of that boundary is real.

**Cost.** 1,439,288 B against the 2,097,152 B ceiling: **657,864 B of room**. The decoder is **51.6 KiB**
of it, measured by symbol size out of an unstripped build.

**1005 tests pass in release.** 125 files / 67,294 lines under `crates/*/src` and `crates/*/tests`
(`git ls-files 'crates/*/src/*.rs' 'crates/*/tests/*.rs' | xargs wc -l`) — the previous "109 files,
58,214 lines" was measured before Phase 9C and is superseded by this line rather than by a claim.

## Things that are true and non-obvious

**The boot order is a compile error to get wrong.** Each arrow is a distinct Rust type. There is no
method on `Opened` that skips `lock_all_pages`, and no way to reach `Sealed` except through
`PrivilegesDropped::seal`. Reaching for a path after sealing is not a slow failure — it is `SIGSYS`
and exit 137, which reads like a crash rather than like a design rule.

**The glyphs used to be drawn 17–21 px above the line box meant to hold them, and nothing noticed.**
`raster.rs` stored `bearing_y` as the distance from the *ascender line* down to the ink's bottom rather
than from the baseline up to the ink's top, and the painter blitted at `run.y + bearing_y` when every
caller passes a *line box top*. The two errors compounded. It read as working: every glyph appeared,
`PaintStats::missing` stayed at zero, and the arithmetic stayed in range. `holonomy-assets/examples/
probe_linebox.rs` is the gate, and it is a gate rather than a report because a probe that cannot go red
is not a gate — it is a printout. It measures by symbol-level mutation: revert the bearing formula and
336 glyphs fall outside the box; revert the line pitch and 210 do.

**A line box of 18 px was `16 ppem + 2`, an identity with nothing to do with the fonts.** The packed
faces need 20 px (Inter), 22 (JetBrains Mono) and 24 (Noto Sans Math) of ascent-plus-descent at 16 ppem,
so no placement of the baselines could have contained them. The pitch is now `Atlas::line_pitch()` — the
tallest ascent plus descent in the atlas, plus the one pixel the rasteriser's antialiasing pad needs —
and the session and the chrome take it from the atlas rather than from a constant. 25 px at 16 ppem.

**Everything important is an integer.** Table geometry, line heights, cell rectangles and border
positions are computed in integers and asserted against hand-computed pixel coordinates. "Within a
pixel" is not a passing test.

**No constants are generated at build or test time.** Cryptographic and layout constants are
hardcoded, compile-time values, so that every gate is O(1).

**Tests are sentences.** `a_run_of_keystrokes_arrives_as_coherent_thirty_two_byte_events`, not
`test_key_event_size`. Each carries a `///` saying *why* it exists, and each `assert!` carries a
message with the values inline, because a failure that does not print the number is a failure you have
to re-run to learn anything from.

**Dead ends are written down where the next person will hit them.** Several protocol facts in
`holonomy-x11` contradict the specification's field list, and each one cost a day. `ConfigureWindow`
puts its `CARD16 mask` at offset 8 and its `pad2` at offset 10 — this had them the other way round, so
every resize asked the server for a mask of zero and was refused as `BadLength` at every length, until
someone read the hex. A request's value list length is derived from its mask, and its values must be in
increasing bit order. And the key-event size is not a per-server property: every core X11 event is 32
bytes, because libX11 reads 32 bytes into an `xAnyEvent` and never varies it. An earlier pass measured
32 bytes *here* and built a runtime heuristic to detect servers that disagreed, plus a stall counter
and a resynchronisation path to recover from it. There was nothing to detect. All of it is deleted, and
`HOLONOMY_X11_TRACE=1` prints request hex, because every bug in that crate was a field in the wrong
byte and "the server said `BadValue`" does not say which field.

## Not done, and not claimed

Sync and CRDT. A second compositor path. CFF outlines. SVG, JPEG, WebP. Real evdev on hardware — the
input path is driven by a script, though the pointer path now decodes real `EV_REL` and `BTN_*` records
through the same decoder a device uses. Real DRM presentation — `SETCRTC` needs DRM master, so the
framebuffer path has been verified unprivileged but not presented.

**Most of the toolbar does nothing yet, and the build says so rather than hiding it.** The chrome, the
menus, the sidebar and the hit testing are real and gated; the *commands* behind the buttons are
mostly not. Undo, Redo, Image and the sidebar's collapse are wired, and **the Zoom, Style and Font
dropdowns are real — they open, they tick the current value, and choosing one applies it.** Bold, italic,
underline, print, spellcheck and 48 of the 54 menu items are drawn, hovered and pressable, and do
nothing — because the document model has no representation for any of them.
**`SessionStats::pointer_inert` counts every one of those presses**, so the number is visible rather
than inferred: if it were ever zero it would mean either that every button works or that nothing is
being hit-tested, and those two need to be distinguishable. Also not built: submenus, drag, and a
multi-document model behind the sidebar (`state.docs` is a list of *titles*; `adopt_document` is still
the only way a document gets in).

**Choosing a font changes the toolbar's label and nothing else.** The atlas is built from one body
face, so `Action::SetFont` records the choice without changing a pixel of the page. That is stated in
the session's handler rather than left to be discovered.

**The caret counts characters, and the case where it does not is recorded.** `caret_column` is a count
of UTF- scalars, so a caret after a two-byte `é` is drawn in the right place — it was a byte count
until Phase 14 part 22, and a caret after any non-ASCII character was drawn a cell too far right.
**A combining mark is a second scalar at the same place and a CJK ideograph is two cells wide, so both
are still counted wrong.** Fixing that needs a display-width table, and the caret's position would need
the same one; nothing here pretends otherwise, and a gate asserts the limitation rather than leaving it
to be found by accident.

There was also an unclaimed ~30% gap in CIOS throughput on this host: ~2,657 ns per squaring measured
here against 2,077 ns on the target. Because the VDF's iteration count `T` is derived from that
per-squaring cost, the gap translated directly into ~30% more squarings inside the same latency budget.
**A container now records `T` beside its salt**, in the one region that is readable before the key that
would unseal it exists, so a container opens under its own derivation cost rather than the caller's. The
host gap remains a host gap — it is why the recorded `T` is what it is.

**Editing a container-backed document end to end is also not claimed.** The seam is complete and gated
— an edit survives an eviction, a fault after an edit is byte-exact, a commit repairs a shift that
per-leaf write-back provably could not, and **a document larger than the resident window paints on any
page**. What is not built is the faulting *mutators*: an edit is refused if its leaf is not already
resident, so typing past the window does not yet work. See `PROJECT.md` §7.

## Licence

Unlicensed and unpublished. `publish = false`.
