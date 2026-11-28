# GPUI desktop — experimental build

The desktop launchers now select this frontend by default. **This is not a
production-qualified release.** GPUI draws
window chrome; document text is shaped by Holonomy and painted as vector glyph
outlines at the monitor's device scale. No finished logical framebuffer is enlarged,
and document strings are not passed into GPUI's text layout cache.

```sh
cargo run --locked -p holonomy-native --features native-ui -- /absolute/document.wavefunction
```

Omit the path for Home. Choose New or Open, enter a path, Tab/click to the masked
passphrase field, then Enter. Creation refuses existing files; an open failure
never falls back to creation. Development builds are unsigned, not installers.
The Native UI preview workflow publishes platform binaries as run artifacts.

## Implemented

- Encrypted create/open using the existing store, bounded residency and document engine.
- Native vector document rendering; logical paint/hit-test geometry is shared with the core.
- Native text-input callbacks and bounded, zeroizing IME preedit. Document composition is
  committed as one text insertion rather than persisting each intermediate candidate.
- Keyboard editing, mouse selection, drag, scroll, undo/redo, select-all and basic formatting.
- Save and save-before-close/Home; a save refusal keeps the session open and displays a
  persistent SAVE FAILED warning until a successful save or explicit wipe. See
  `docs/LINUX-SAVE-RECOVERY.md` for recovery and plaintext-egress cautions. Uncommitted IME
  composition blocks unlock/create/save/close until the platform commits or cancels it.
- Streaming Find, read-only witness previews, bookmark creation and integrity verification.
- Explicit bounded clipboard copy/cut/paste with plaintext-egress disclosure.
- Cooperative canary-verified resident-buffer/key wipe and clipboard clearing; **not** a
  GPU-erasure guarantee. Optional `HOLONOMY_DEADMAN_MS` enables the existing latched dead-man
  watcher; an explicitly configured invalid/zero timeout is refused rather than disabling it.
  A foreground poll observes a trip without repainting while idle; reads/saves after a
  latched trip are refused until the wipe completes.
- Fixed-capacity secret input buffers; deleted graphemes and dropped glyph contour buffers
  are zeroized. Search/bookmark input is painted through the same vector path.

## Qualification and platform limits

**Windows encrypted editing is unavailable.** The pre-existing world-line backend
refuses platforms without its qualified durability primitives. The native frontend
now reports this before KDF/file creation rather than hitting a debug assertion.
A compiling Windows UI binary is not evidence of working Windows persistence.
Do not remove that fence without implementing and qualifying a Windows storage backend.

Linux/macOS exercise real encrypted round trips in session tests. Linux additionally
runs Xvfb smoke tests at 1x, 1.25x, 1.5x, 1.75x and 2x: wrong-passphrase refusal,
unlock, native input, editing, undo/redo, find, formatting, refused close with pending
text, continued refusal of a poisoned handle, explicit wipe/quit, and independent
encrypted reopen verification of the last committed version. A failed append requires
recovery/reopen; restoring a pathname never clears the storage safety fence. CI
screenshots are test specimens, not proof of quality on the user's actual monitor.

Still outstanding before production qualification:

- Suspend/USB tripwire producers, complete scene/cache lifecycle review and shutdown/OS-quit
  failure handling. Manual/dead-man CPU/key wipe is not a full lifecycle qualification.
- Full IME reconversion/selection and accessibility qualification. The input context
  deliberately does not expose surrounding document/passphrase text to the framework.
- Full toolbar/chrome parity (including all rich formatting, links, folders/recent-file
  navigation, import/export UI, complete history navigation and document zoom).
- Native-platform runtime qualification, mixed-monitor DPI, fractional-scale visual
  checks and user confirmation at 200%.

**Use test documents only until these gates close.** `holonomy-gui` and `holonomy gui`
launch the sibling `holonomy-native` executable. Release bundles include it. The old
pixel frontend is available only via explicit `HOLONOMY_LEGACY_GUI=1`; missing GPUI
binaries never silently fall back. Use `scripts/run-desktop.sh` for a source checkout.

See `docs/QUIET-MD-UI-MIGRATION.md` and `vendor/xattr/HOLONOMY.md` for the migration
and temporary compatibility patch. Linux build dependencies are listed in
`.github/workflows/native-ui.yml`.


## Workspace design and responsiveness

The shell adapts Quiet MD's neutral palette and Vega geometry (MIT attribution in
`THIRD_PARTY.md`): a recessed library rail, compact ghost tools, bounded full-bleed
writing measure and centered unlock form. Narrow windows hide the rail and wrap tools;
short Home/unlock screens use a separate scroll viewport rather than shrinking text.

Calibration and memory-hard passphrase derivation now run on a single background worker.
The UI stays interactive, displays progress, and supports Cancel/Escape. Cancellation
revokes the result; Argon2 itself cannot be interrupted, so its zeroizing temporary is
cleared when the worker returns. No file is created for a cancelled preparation. Existing
locked key owners and the editor remain on their original thread, without unsafe Send.
Final world-line authentication, save, search and verification remain synchronous, so
this is not a guarantee against every slow-disk or large-document stall.

The real-KDF 200% smoke test requires a theme repaint while derivation is still running,
then creates, saves and independently reopens the document. It also records narrow-window
screenshots. Downloaded development executables use opt-level=1; they remain experimental.
Cmd/Ctrl+N and Cmd/Ctrl+O open New/Open; Cmd/Ctrl+Shift+T switches theme.
