# Quiet MD UI investigation and replacement direction

## Status

Source inspected locally from `https://github.com/s1dashu/quiet-md`, commit
`eb741299a25e0a9d19c0ef52e27a5e00feab286a`.
The checkout is `.reference-quiet-md/`, excluded locally using `.git/info/exclude`.
It is reference material, not a new dependency or part of the shipped application.
**First slice implemented:** `crates/holonomy-native` is an opt-in GPUI 0.2.2
Home/specimen shell with native-scale text, physical-pixel rules, theme switching
and an explicit separate-process launcher for the existing editor. It does not
load plaintext or passphrases. Native unlock/editor parity and visual qualification
are not complete. See the crate README for build/run instructions.

The user reports that the current application remains blurry at 200% after the
Windows DPI-awareness change. That report supersedes the previous assertion that
integer framebuffer scaling was sufficient to make the application crisp.

## What the source actually does

Quiet MD is a Rust/GPUI application, not an Electron or WebView application.

- `Cargo.toml`: GPUI and GPUI platform are path dependencies under `vendor/zed`.
  The application and vendored GPUI use Rust edition 2024. Holonomy currently
  declares edition 2021 and Rust 1.81; adopting these dependencies requires an
  explicit toolchain/MSRV decision, not just adding a crate name.
- `src/ui.rs::icon` and `src/editor_ui.rs::icon`: SVG elements sized in logical
  units, rather than icons baked into an enlarged whole-window bitmap.
- `vendor/zed/crates/gpui/src/window.rs`: window scale factor is obtained from the
  platform window and refreshed with window state.
- `vendor/zed/crates/gpui/src/text_system.rs`: glyph raster parameters include
  scale factor (including it in their hash), and rasterization goes through the
  platform text system.
- `vendor/zed/crates/gpui/src/util.rs`: helpers round geometry and strokes to
  device pixels. Native-scale rendering still needs deliberate stroke alignment.
- `src/zoom.rs`: document text zoom is separate from sidebar and toolbar sizing.

This is an architectural difference, not a theme or a sharpening filter.

## Why the previous fix is insufficient

`crates/holonomy-gui/src/platform.rs::ScaledWindow` explicitly gives the renderer
an inverse-scaled logical viewport. `present_argb` then enlarges the completed
framebuffer: pixel repetition at integer scales and bilinear sampling otherwise.
At 200%, a glyph rasterized at logical size still has only the original number
of samples. Repeating each sample cannot recover the detail of a glyph rasterized
at twice the size. Existing antialias coverage is enlarged too. The DPI-awareness
fix can remove a second OS stretch, but cannot remove this application-level one.

This explains a remaining limitation; it is not a visual diagnosis proven on the
user's machine. The replacement must be checked there at the reported scale.

## Chosen direction

Build a GPUI frontend alongside the existing frontend, then replace the default
only after parity and visual qualification. Do not embed the existing framebuffer
in a GPUI image: that would preserve precisely the low-resolution text path the
replacement is meant to remove.

Keep:

- `holonomy-seal`, `holonomy-line`, `holonomy-ledger`: encryption and persistence.
- `holonomy-text`: document mutations, selections and formatting semantics.
- `holonomy-shell`: intent routing and bounded document access where suitable.
- Existing save-failure, witness/history and secure-session behaviour.

Replace/adapt:

- Native window/event integration, text entry/IME, clipboard and focus handling.
- Canvas-drawn Home, unlock prompt, toolbar, menus, search, history and settings.
- Text measurement, glyph drawing, selection/caret painting and hit testing as a
  coherent pipeline. GPUI text measurement and painting must use the same shaped
  runs; retaining incompatible old advances would cause caret drift.
- Whole-window bitmap scaling with GPUI logical layout and device-scale drawing.
  Keep document zoom separate from display scale. Avoid applying an extra 200%
  application scale on top of an OS-provided 2x device scale.

Do not import Quiet MD's plain Markdown/XML storage, telemetry, updater, network
font downloads or agent connector. Its functionality is not equivalent to
Holonomy's encrypted document model.

## Implementation gates

1. Pin a GPUI revision and review its dependency/toolchain/platform requirements.
   Prefer a reproducible dependency over copying the entire Quiet MD repository.
   Any copied Quiet MD code/assets must retain its MIT notice; vendored GPUI has
   its own Apache-2.0 license and transitive notices requiring review.
2. Add an opt-in frontend binary in the workspace, sharing Holonomy session and
   intent APIs. First render a text/SVG/stroke specimen and Home/unlock screens.
   Keep the old default available while this is unqualified.
3. Connect a real encrypted document: bounded visible text, typing, selection,
   scrolling, undo/redo, formatting, search and save. No full-document plaintext
   copy into a framework widget. Review framework glyph/text caches and their
   lifetime against the existing wipe guarantees before claiming equivalence.
4. Port history/bookmarks, navigation, themes and shortcuts. Verify failed saves
   never discard the open session. Use a feature-parity checklist, not screenshots
   alone, to decide when the old frontend can be retired.
5. Qualify Windows, macOS and Linux builds and real display behaviour. Do not
   change the default until these pass.

## Visual acceptance

Capture native-resolution screenshots (no image-viewer scaling) at 100%, 125%,
150%, 175% and the user's 200%. Include text, plus/minus icons and hairline rules.
Compare the same logical size against Quiet MD and a native text application.
At 200%, glyphs must be independently rasterized at device size, not composed
of repeated 2x2 logical samples. Antialiasing itself is expected, not a failure.
Verify caret positions and hit testing at every scale, resize and move between
monitors with different DPI, and test IME, selection and keyboard focus.

Automated geometry tests and green CI are necessary but cannot certify the
reported visual defect fixed. Obtain the user's confirmation before calling it
resolved.
