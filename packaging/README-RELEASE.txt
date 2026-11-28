holonomy — experimental, not production-ready

This archive is unsigned. It is not a production release or installer. Read the
bundled README.md and docs/GATES.md in the source checkout before relying on it.
Do not use sensitive or irreplaceable documents.

The first production target is being planned for Linux x86_64, but no Linux
configuration is production-qualified yet. macOS and Windows archives, when
provided, are experimental; Windows currently refuses encrypted editing because
its storage durability primitives are not qualified.

Important limits:
  * Four laws (L-1, L-2, L-8, L-9) are gated under their documented test scopes;
    L-3 through L-7 and L-10 remain open. See docs/GATES.md for exact claims.
  * The default desktop is GPUI. It does not promise autosave: use Ctrl+S and
    verify success. See share/doc/holonomy/docs/LINUX-SAVE-RECOVERY.md in Linux
    bundles for refusal and recovery guidance. Unsaved edits can be lost on crash.
    A successful software
    test does not prove physical power-loss safety.
  * A reported emergency wipe covers registered arenas, not every plaintext
    copy, GPU surface, OS or external application. Full wipe qualification is open.
  * IME, accessibility and native desktop feature parity remain incomplete.
  * Deleted content remains encrypted in history until explicit compaction.

Run it (keep holonomy-native beside holonomy-gui):
  ./bin/holonomy-gui notes.wavefunction    # experimental GPUI desktop
  ./bin/holonomy notes.wavefunction        # terminal editor
  ./bin/holonomy verify notes.wavefunction
  ./bin/holonomy compact src.wavefunction dst.wavefunction

SHA-256 checksums detect corruption but are not independent signatures. The old
pixel frontend requires explicit HOLONOMY_LEGACY_GUI=1.
Project: https://github.com/c4us4l1ty/holonomy — licence MIT OR Apache-2.0.
