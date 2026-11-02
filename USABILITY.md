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

Follow-up verification: GitHub Actions run `41311676953` on commit `92752de` passed the default and
desktop checks, the full release sweep, and the desktop-feature tests (including the X11 selection
wire-decoding gate, compose tests, managed clipboard request gate, and the 2 MiB artifact ceiling).

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

## Follow-up: modal usability from a fresh-clone run (item 3)

A release/optimized run on the user's machine showed four defects in the unlock/setup dialog. They
are fixed without changing the two-passphrase model, the container format, or the sealed boot path.

- **Visible passphrase mask.** The mask was U+2022 BULLET, which is outside the FR-2.3 atlas coverage
  (ASCII + Latin-1 + the existing math/box windows). `Painter::label` advances over a missing glyph
  without blitting, so typing changed the value and drew nothing. The mask is now ASCII `*`
  (`dialog::MASK`), masked once by the driver and again by the renderer. A pixel-level gate paints the
  dialog through the real atlas and asserts the field has visible ink and `PaintStats::missing == 0`.
- **Readable, well-spaced dialogs.** `dialog::geometry` is responsive instead of a fixed 620 px panel
  with 34 px fields and 100x36 buttons. The panel scales with the window, padding/rows/buttons scale
  with the chrome's measured cell, and a normal desktop window asks for the larger boot atlas size.
  `Painter::label` now honours `Label::size` (clamped to the sizes the atlas holds) instead of ignoring
  it, so dialog type can be larger without raising global `--ppem` or shrinking the document viewport.
  Long values keep the caret inside the field; long labels clip with an ASCII ellipsis; choice lists
  scroll to keep the selection visible. Gates cover minimum and large window sizes.
- **Immediate pointer focus.** `ButtonPress` used `Window::focus`, which waits for a *new* `MapNotify`
  and drains events. On an already-mapped window the event was long consumed, so every click burned the
  500 ms timeout and could eat queued input. Clicks now use `Window::focus_now` (issue `SetInputFocus`
  immediately, leave the event queue alone); the bounded map wait remains only for the initial focus in
  `Desktop::open`.
- **No full-window redraw while a form is open.** `Session::set_dialog` is a no-op for an unchanged
  view, damages only the union of the old/new panel while a dialog stays open, and takes full damage
  only when the scrim appears/disappears. The driver no longer re-syncs the form every loop iteration;
  it ticks immediately after a form edit/focus change so the caret moves with the key instead of waiting
  for the next 16 ms event timeout. A session gate asserts an unchanged dialog does not repaint and a
  modal keystroke damages less than the full frame.

Verification: GitHub Actions run `41324506992` on commit `28ad4a6` passed the default and desktop
`cargo check --workspace --all-targets`, the full release sweep (`--test-threads=1`), and the
desktop-feature tests, including the new dialog geometry/paint gates and the 2 MiB artifact ceiling.
No live interactive X11 usability test was run (`HOLONOMY_X11_LIVE=1` remains unset); the evidence is
the automated gates plus the user's reproduced release-build symptoms.

## Paragraph formatting: initial step

The style menu now applies **Normal text**, **Heading**, or **Quote** to the current
paragraph or the paragraphs touched by a selection. Heading uses the existing header
flag (and exports as a heading), bold, and teal; the editor currently draws it at body
size. Choosing a style on an empty paragraph arms the next text; Enter at the end of a
heading starts normal text. The former Title, Heading 1/2/3 and Caption entries were
removed because the model cannot represent their distinct sizes or levels. Inline bold,
italic and colour changes preserve the other properties across mixed selections, and
bold plus colour can be armed together. The colour menu also has a Default choice.

Formatting commands now record one bounded undo/redo transaction per operation. Mixed
runs retain their original styles on undo, and a new edit discards the redo branch.
Paragraph presets reset inline formatting in
the affected paragraphs; the on-screen heading is not enlarged, and the style dropdown
is not yet derived from the caret's paragraph. Lists, page layout, navigation by headings,
multiple heading levels, tables of contents, and rich document import remain future work.
WordCraft is a useful reference, but its snapshot-based history and egui frontend do not
fit Holonomy's bounded undo arena or sealed framebuffer architecture directly.

`File > New sample` creates an editable copy of the studio handbook text adapted from
WordCraft. It is saved as a new document in the unlocked desktop vault. This adaptation
uses Holonomy's existing heading and inline-style spans; its lists and equipment entries
are plain text, not WordCraft list or table objects. The upstream MIT license is included
in `third_party/wordcraft/LICENSE-MIT`.

`Ctrl+B` toggles bold and `Ctrl+Shift+I` toggles italic through the same selection-aware
commands as the toolbar. `Ctrl+I` remains the existing image shortcut.


## Document library (desktop)

After unlocking, the desktop opens a document library inspired by the Plan references. Select
**Blank document** to name and create a document in the current encrypted space, or **Studio
handbook** to create an editable sample. Document cards open saved documents, including documents
previously closed in the editor. The card artwork is a generic illustration, not a preview of the
document's contents. Type to filter titles, use Backspace to correct the search, and press Enter to
open the first visible match. Previous/Next show more results. **Open...** offers the existing vault
and container picker; **Lock** saves before returning to the unlock screen. From the editor, use
**File > Documents** to return to the library. The library uses the currently unlocked space only;
no titles or previews are written to separate files.

The library and desktop forms are part of the X11 desktop build, not the sealed boot interface.
This UI work does not add a table editor, real list objects, or additional font sizes to the document
model; the existing editing and export limitations still apply.

## Desktop long-document view

Managed desktop documents soft-wrap prose into indexed visual rows without changing stored text.
The editor stacks individual sheets with a gap, drawing only sheets and rows near the viewport.
The wheel, scrollbar, arrow keys and Page Up/Down can move through the document; the status bar reports the
sheet containing the first visible row. Sheet height is derived from its width, rather than the
window height. Zoom or a width change can reflow text and alter page boundaries.

This is not yet a full print-layout engine: page dimensions are not selectable paper sizes and PDF
export paginates independently. Logical lines containing `$$` or table separators retain the old
unwrapped block rendering. Variable-height formulas, images, and multiline tables are not yet
repaginated or split to fit sheets, and their click geometry can differ from plain text. Layout
counts Unicode scalars rather than measuring styled glyph widths; complex shaping and soft-boundary
caret affinity remain limitations.

The index keeps offsets and row lengths, not a copy of the document text. An edit normally rewraps
its paragraph; changes in row count rebuild the row index, and newline edits and resize may re-index
the whole document. This is not yet H2's bounded section-level strategy, nor a validated 2,000-page
performance claim. In a standalone synthetic run of the index (not the app), a single 5.28 MB
paragraph rebuilt in about 44 ms and a middle edit took about 46 ms. These figures are illustrative,
not a hardware-independent performance guarantee.
