# GPUI save and recovery on Linux

This is operational guidance for the **experimental** desktop, not a power-loss
or complete recovery guarantee. Use test documents and keep independently
verified backups. The guidance applies to the default GPUI frontend, not the
legacy pixel or terminal editors.

## Normal use

Press Ctrl+S and wait for **Saved to an authenticated witness**. The title shows
**unsaved** while the editor has uncommitted changes. A crash before a successful
save can lose those changes. There is currently no GPUI autosave guarantee, and
a successful software save is not a qualification against physical power loss.

Closing or returning Home requests a save; a refused save keeps the document
open. Finish or cancel any active IME composition before saving or closing.
The status bar can show other messages after a save, so it is not a permanent
receipt of an earlier save.

## If saving fails

The window title displays **SAVE FAILED** and the status bar retains the save
error. Do not assume a retry succeeded, and do not terminate or wipe the editor
if you need the unsaved content: a crash or wipe can discard it. A successful
save clears the warning. A failed append can poison the current storage handle;
restoring the pathname or free space alone does not make that handle safe again.
The editor may keep refusing save and close, even if the underlying problem is
corrected. Do not force a retry by replacing the file or editing it externally.

If you must preserve unsaved text, consider explicitly selecting and copying it
while the editor remains open. This is **plaintext egress** to the OS clipboard,
clipboard history and any application receiving it. Use only on a trusted system
and destination, and follow your security policy. Copy is limited to 1 MiB per
selection and may not preserve formatting, metadata or all document content.
Do not assume the clipboard or destination is encrypted or can be wiped. The
application's emergency wipe intentionally discards the session and cannot recover
unsaved text.

Once you have safely dealt with the unsaved content, reopen the existing file
and use `holonomy verify <document.wavefunction>` to authenticate it. Reopening
recovers committed state, not necessarily the last unsaved edits. Preserve an
untouched copy of the original file before attempting any further recovery; do
not overwrite the only copy. Seek assistance if verification or reopening fails.
A verification result is not proof that every intended edit was committed.

## Qualification boundary

The native smoke test exercises a refused close, a poisoned handle and an
independent reopen after SIGKILL with pending edits. That test must pass for the
candidate being evaluated; it does not simulate power failure, guarantee
recovery of unsaved edits, or prove behavior on all filesystems and hardware.
