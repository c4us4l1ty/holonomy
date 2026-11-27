# Native UI preview (not the replacement editor yet)

An opt-in GPUI 0.2.2 shell that renders text and device-pixel rules without
Holonomy's existing logical-framebuffer enlargement. This is the first migration
slice, not a claim that the document editor's blur has been fixed.

```sh
cargo build --locked -p holonomy-gui --bin holonomy-gui
cargo run --locked -p holonomy-native --features native-ui
```

The Native UI preview workflow also builds downloadable binaries for Windows,
macOS and Linux. Extract both binaries into the same directory. Run
`holonomy-native` (`holonomy-native.exe` on Windows). These are unsigned development
builds, not installers. Linux builds need system GPUI graphics/text dependencies
and a working GPU/display; CI package installation is listed in the workflow.

- Home shell, light/dark switch, rendering specimen and live device-scale readout.
- Text at 12, 16 and 24 logical pixels plus one-device-pixel rules.
- Explicit launcher for the **existing** editor in a separate process; launching
  it does not change that editor's rendering.
- No encrypted document, passphrase or recent-file list is loaded into GPUI.
- No updater, telemetry or font downloader is included in application code.

Not implemented: native unlock, document editing, IME/input routing, search,
history, secure text-cache lifecycle, accessibility and parity qualification.
Keep using the existing frontend for actual work. The preview is not a default
launcher or part of the release installers.

For visual qualification, set the OS display to 200%, open Rendering specimen,
and capture at native resolution without resizing the screenshot. Repeat at
fractional scales and on mixed-DPI monitors. Compilation does not establish
visual quality; user confirmation is still required.

See `docs/QUIET-MD-UI-MIGRATION.md` and `vendor/xattr/HOLONOMY.md` for architecture
and the temporary dependency compatibility patch.
