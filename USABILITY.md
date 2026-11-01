# Desktop usability repair — PRD / Plan trace

The authoritative product target is still the bare-silicon build (`Plan/Plan.md` Part 4). The
`desktop` feature is the existing developer window used to exercise the same editor, renderer and
container without DRM/evdev privileges. This repair makes that window usable; it does not replace the
sealed boot path or claim the X11 window satisfies the Zero-X11 invariant.

| User-facing feature | PRD / Plan clause | Implementation | Verification |
|---|---|---|---|
| Two passphrases | FR-4.9; Plan Part 4 duress amendment | Setup asks for `P_A` (top-secret/primary) and a different `P_B` (normal/decoy). Unlock asks for one passphrase and mounts only the payload that passphrase authenticates. The UI does not label which payload opened. | `vault.rs` creates both payloads; `two_passphrases_mount_isolated_multi_document_bundles_and_deletion_persists` opens each side and checks titles/body stay isolated. |
| Multiple documents per space | FR-4.1 fixed-size deniable container; FR-4.9 isolation | One encrypted `HVAULT1` bundle per payload carries ids, titles, caret positions and full HDOC documents. New vaults reserve a full 8 MiB growth range for each payload so later saves cannot grow into the other space. No per-document cleartext files or title/count metadata outside AEAD. | bundle round-trip and cross-passphrase reopen tests. |
| Create / rename | Plan UI sidebar `+` and active-row overflow | In-window title dialogs; sidebar `+`, File > New/Rename and row overflow. New documents are saved into the mounted bundle, not parked only in RAM. | managed lifecycle test; vault create/save/reopen test. |
| Delete | FR-4.9 decoy must not alter primary; deterministic storage | Confirmation dialog; smaller bundle is authenticated-written before the resident editor is removed. Freed payload tail is re-chaffed by `Wavefunction::write_content`. | deletion persists across reopen in the isolation test. |
| Edit / save | FR-1 text model; FR-3.4 damage; FR-4.7/4.8 streaming container | Desktop typing uses the X server keysym layout for Unicode text and physical shortcuts for commands. Ctrl+A now selects all. Explicit Ctrl+S/File > Save and 800 ms autosave debounce write the whole bundle through the container API. Save errors stay visible and block lock/quit/close. | select-all regression; existing editor/render/store gates; vault formatted-payload round trip. |
| Open / close / lock | FR-4.9 one mounted payload; Plan File menu | Open lists saved documents in the current space and can switch to another container after saving/locking. Close saves and removes the resident tab but keeps the encrypted document for Open. Lock saves, drops keys/editors/image cache and returns to unlock. | managed lifecycle request test; vault reopen tests. |
| Legacy documents | compatibility, not a new PRD format | Existing raw UTF-8 and HDOC payloads open as one document and are upgraded to the encrypted bundle on next save. | legacy path in `Vault::open`; existing HDOC gates. |
| Bare-silicon boot | FR-3, FR-5; PROJECT execution order | Default non-desktop build and sealed `main` path are unchanged. Desktop-only vault/dialog code is feature-gated. | default and desktop `cargo check --workspace --all-targets`; default release tests. |

## Known PRD gaps not silently claimed as fixed

- FR-4.9 dummy modification timestamps remain unimplemented, exactly as recorded in Plan Part 4.
  The single container has one filesystem mtime; adding a decoy-visible timestamp would be a format
  decision, not a UI shortcut.
- The desktop window is not the FR-3 DRM/KMS product surface and does not satisfy Zero-X11. It is a
  development/usability harness for the same session model.
- FR-5.4 periodic RAM scrambling and the full measured seccomp profile are outside this repair.
- Container writes are authenticated per chunk but not a crash-atomic multi-chunk transaction. A
  power loss during a bundle rewrite can still require the existing container recovery story; the UI
  keeps edits resident and reports write errors rather than pretending a failed save succeeded.

## Verification

GitHub Actions run `41294039611` on commit `8afadbc` passed:

- `cargo check --workspace --all-targets` (default static-musl target)
- `cargo check --workspace --all-targets --features holonomy/desktop`
- `cargo test --workspace --release -- --test-threads=1`
- `cargo test -p holonomy --features desktop --release -- --test-threads=2`

The release sweep is serialized because hosted runners cap `RLIMIT_MEMLOCK` near the rope's locked-leaf
working set; parallel unrelated tests can otherwise exhaust it. The boot-evidence gate now distinguishes
"could not read `/proc/self/ns/net`" from "not isolated" instead of panicking on a hosted runner.

The temporary `verify-session.yml` workflow used to obtain this evidence is removed before merge.

## Follow-up: everyday usability (item 2)

- **System clipboard (desktop only):** Copy/Cut publishes UTF-8 to the X11 `CLIPBOARD` selection;
  Paste requests the current selection and falls back to the internal yank buffer if no owner answers.
  Locking, switching container, and quitting release the selection and scrub the local copy. The app
  does not spool clipboard data to disk. Clipboard managers outside the app may still retain what the
  user copied; that is an OS-level caveat, not a vault leak.
- **Compose / dead keys:** the desktop driver now feeds X keysyms through a small composer for common
  Latin dead keys and `Multi_key` sequences. It uses a compact precomposed table to stay under the
  2 MiB binary ceiling. Native XIM/IME preedit is still not implemented.
- **Menu cleanup:** inert cloud/add-on/voice/translation rows were removed, the Extensions heading was
  dropped, and local Word count / Keyboard shortcuts rows were wired. The remaining unwired rows are
  the View > Mode/Zoom submenu headers.
