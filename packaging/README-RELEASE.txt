holonomy — experimental, not production-ready

This is an UNSIGNED, UNPUBLISHED-BY-DEFAULT build for one platform:
x86_64 Linux (glibc). It will not run on macOS or Windows, and there is no installer.

Before you rely on it in any way, read the "What is real, and what is not" table in the
bundled README.md (share/doc/holonomy/README.md after unpacking). In particular:

  * No security or durability gate in docs/GATES.md is claimed ("gated"). The world-line,
    the seal and the editor are implemented and measured in CI, but the gates that would
    let you trust this with irreplaceable work are open. Treat this as a document you can
    lose.
  * Unsaved edits are lost until a save commits. Ctrl+S, the 1.5 s autosave and quit all
    seal what is open, but a crash before a save loses the open buffer.
  * One document, one window, one caret. No panes, no settings, no GPU rendering, no IME.
    Wide characters and right-to-left text are handled; contextual glyph shaping is not.
  * The emergency wipe now reports the reaper's own measured outcome and exits non-zero when
    it cannot confirm one. That is a receipt for the arenas the editor registered, not proof
    of complete plaintext coverage.

Run it:
  ./bin/holonomy notes.wavefunction        # create or open; asks for a passphrase
  ./bin/holonomy verify notes.wavefunction
  ./bin/holonomy compact src.wavefunction dst.wavefunction

The binary is not signed: there is no signature to check, so verify the .sha256 file against
how you obtained the archive. It is not byte-for-byte reproducible across machines yet.

Project: https://github.com/c4us4l1ty/holonomy — licence MIT OR Apache-2.0.
