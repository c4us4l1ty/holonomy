holonomy — experimental, not production-ready

This is an UNSIGNED, UNPUBLISHED-BY-DEFAULT release archive bundle for:
  - x86_64 Linux (glibc, X11 GUI + TUI/CLI)
  - aarch64 macOS (Cocoa GUI + TUI/CLI, unnotarized)
  - x86_64 Windows (Win32 GUI + TUI/CLI)

Before you rely on it in any way, read the "What is real, and what is not" table in the
bundled README.md (share/doc/holonomy/README.md after unpacking). In particular:

  * Three laws in docs/GATES.md are gated (L-1 lifetime scale, L-2 eyes-only residency,
    L-8 one geometry); the remaining laws (L-3..L-7, L-9 cross-machine, L-10) remain open
    ("spec") pending hardware power-loss, OS/hardware wipe, and multi-machine qualification.
  * Unsaved edits are lost until a save commits. Ctrl+S, the 1.5 s autosave and quit all
    seal what is open, but a crash before a save loses the open buffer.
  * One document, one window, one caret. OpenType GDEF/GSUB/GPOS shaping and UAX #9 BiDi
    run over embedded Inter & JetBrains Mono fonts; platform IME composition remains open.
  * The emergency wipe reports the reaper's own measured outcome and exits non-zero when
    it cannot confirm one. That is a receipt for the arenas the editor registered, not proof
    of complete plaintext coverage.

Run it:
  ./bin/holonomy notes.wavefunction        # TUI editor (Ctrl+R history, Ctrl+G panel, Ctrl+S save)
  ./bin/holonomy-gui notes.wavefunction    # Desktop pixel window (Ctrl+K link, Ctrl+Shift+D dark mode)
  ./bin/holonomy verify notes.wavefunction
  ./bin/holonomy compact src.wavefunction dst.wavefunction

The binaries are not signed or notarized: verify the .sha256 file against how you obtained
the archive.

Project: https://github.com/c4us4l1ty/holonomy — licence MIT OR Apache-2.0.
