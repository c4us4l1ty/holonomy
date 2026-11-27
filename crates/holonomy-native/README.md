# Native vector frontend — development build

This is an opt-in migration, **not yet the default release editor**. GPUI draws
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
- Save and save-before-close/Home; a save refusal keeps the session open. Uncommitted IME
  composition blocks saving/closing until the platform commits or cancels it.
- Streaming Find, read-only witness previews, bookmark creation and integrity verification.
- Explicit bounded clipboard copy/cut/paste with plaintext-egress disclosure.
- Cooperative canary-verified resident-buffer/key wipe and clipboard clearing; **not** a
  GPU-erasure guarantee. Optional `HOLONOMY_DEADMAN_MS` enables the existing latched dead-man
  watcher. A foreground poll observes a trip without repainting while idle; reads/saves after a
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
text, save retry, quit and independent encrypted reopen verification. CI
screenshots are test specimens, not proof of quality on the user's actual monitor.

Still outstanding before replacing the default:

- Suspend/USB tripwire producers, complete scene/cache lifecycle review and shutdown/OS-quit
  failure handling. Manual/dead-man CPU/key wipe is not a full lifecycle qualification.
- Full IME reconversion/selection and accessibility qualification. The input context
  deliberately does not expose surrounding document/passphrase text to the framework.
- Full toolbar/chrome parity (including all rich formatting, links, folders/recent-file
  navigation, import/export UI, complete history navigation and document zoom).
- Native-platform runtime qualification, mixed-monitor DPI, fractional-scale visual
  checks and user confirmation at 200%.

**Use test documents only until these gates close.** The old frontend remains the
release default; none of its installers or launchers is silently replaced.

See `docs/QUIET-MD-UI-MIGRATION.md` and `vendor/xattr/HOLONOMY.md` for the migration
and temporary compatibility patch. Linux build dependencies are listed in
`.github/workflows/native-ui.yml`.
