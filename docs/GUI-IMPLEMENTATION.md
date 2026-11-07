# GUI implementation status

The historical [PRD](PRD-v5.1.0.md) §11 is the interface target, not a description
of a finished or security-reviewed product. The current windows are separate prototypes;
neither is suitable for sensitive or irreplaceable data.

`crates/holonomy-ui` now has its own build/test workspace and a status projection that consumes
an actual store `SaveReport`. It no longer simulates a save cadence or infers fresh pad from an
`O_RDWR` handle. This status code is not wired into a window or a real editor workflow; the
`holonomy` binary there is still a placeholder. A reported ratchet count is not independent proof
of persistent device writes, and the store still reports pad freshness as *not established*.

| PRD interface goal | Current state | Work still needed |
| --- | --- | --- |
| Native Wordcraft editor | An opt-in, unencrypted Wordcraft scratch window exists. The demo-container editor is restricted to plain text and is not the full Wordcraft UI. | Integrate the Wordcraft window with the container session only after controlling and testing all open, save-as, export, print, clipboard, drag/drop and recovery paths that can expose plaintext. Preserve formatting and make data-loss behavior explicit. |
| Protection layer per save | The demo status identifies its deterministic pad and experimental container. | Report the actual source and result of each save in a real session, including ratchet and failure states; never infer physical USB or fresh-pad protection from a cursor. |
| Fresh-pad saves remaining | The demo reports only cursor slots up to the configured boundary. It does not track pad freshness or history. A pending or uncertain save hides the remaining count. | Implement durable, rollback-aware pad-use and ratchet accounting and recovery before presenting a verified fresh-pad budget. |
| Capture exclusion | Not attempted in the demo; the document can be captured. Linux has no portable window capture exclusion. | Integrate platform-specific attempts where available and report their actual result and limitations. |
| Memory-locking rung | Editor-wide protection has not been verified. A capability probe alone cannot establish that the editor's plaintext is locked or excluded from dumps. | Track the actual protected regions and their obtained rung; account for Wordcraft, egui, undo, clipboard and OS copies. |
| Reaper priority | No watchdog is running in the demo. | Integrate disconnect detection, suspend notifications and the reaper; report its actual running state and priority. Measure and test its failure behavior. |
| CPU rendering and resource envelope | The preview uses eframe's `glow` backend; the PRD's display/compositor and GPU assertions are not established for this build. | Validate a supported rendering and windowing architecture and measure real memory and latency on target systems; do not present documented budgets as measured results. |

The demo's session status is derived from editor and save-worker state. It distinguishes
saved, modified, unrepresentable text, saving and recovery-required states. The last
confirmed cursor may be stale while a save is in progress or after an uncertain failure;
the panel does not report it as the current on-disk cursor in those states. An interrupted
process or power failure can still require manual recovery. The status panel does not
itself provide any of the protections it describes. After a successful save, it shows
the save report's unkeyed image fingerprint for comparison with `holonomy verify`;
the fingerprint is hidden during a save or after an uncertain failure. It is not
persisted across restarts or proof of authenticity or durability. The last-save ratchet
result is likewise reported only from a successful save and hidden while the image or
pad is uncertain; a reported count is not independent proof of device writes or freshness.
Protection reporting distinguishes *fresh*, *reused* and *not established*; the container
store reports *not established* because it sees one image and one pad source, not sector
history across devices, snapshots or rollbacks. The demo surfaces the layer attached to
the last successful save (with freshness *not established*) and hides it while the save
or pad state is uncertain; capture exclusion, memory-locking coverage and watchdog
priority are still reported only as unavailable.

The restricted demo also offers `--demo-inspect PATH CURSOR`: it opens the container
read-only, disables editing and saving, and displays a prominent mode indicator. Rich documents
can be shown only as a potentially incomplete plain-text body preview; formatting, images, tables,
headers, footers and other content may be missing. The editor still refuses to edit rich documents.
Successful authentication and decoding confirm only that the supplied candidate can be opened with
that demo pad and cursor. They do not repair anything or establish pad freshness, disk
durability, or complete recovery. Reading can update OS file-access metadata.

## Read-only Wordcraft rendering of a container

`crates/holonomy-preview/src/bin/holonomy-rich-view.rs` is the first path where the real
Wordcraft renderer draws a real container document. It is one-way by construction, not by
configuration:

```text
holonomy-rich-view --view PATH CURSOR          # a window
holonomy-rich-view --inspect-only PATH CURSOR  # decode and report, no window
```

The container is opened with `writable = false`, the decoded document is cloned, and the
container handle is dropped before the window is built. Wordcraft is given
`Session::new(document)`, so the engine has no path, no recovery state and no writable
handle; the dirty flag it reports is ignored because nothing can be saved.

Two boundaries stand between the renderer and the host:

1. **Services.** Every host hook that could write, export or read the clipboard is replaced
   in `read_only_services()` — file dialogs answer "cancelled", `download`, `host_save`,
   `print` and `make_default_app` return an error naming the read-only view, clipboard
   reads answer `None` and clipboard writes report failure. Printer discovery and
   submission are absent. This is deliberate: a missing hook can make Wordcraft fall back
   to another destination, so every persistence hook is present and refuses.
2. **Input filtering.** `filter_events()` keeps pointer motion, wheel scrolling, pinch zoom
   and window focus. Keys, typed text, paste, copy, cut, IME, pre-edit and every pointer
   button are dropped before Wordcraft sees the frame, so no ribbon button, context menu or
   shortcut can reach a command. `Ctrl+Q` is the one chord the host itself honours, because
   a keyboard-inert window still has to close.

Neither boundary is a sandbox and neither is a claim about Wordcraft's internals. The
decoded document is plaintext in the process, the renderer allocates further copies, and
the OS, swap and core dumps can expose them. Capture exclusion, memory locking and the
reaper are still not integrated, so the window's own banner says what it is: read-only,
not protected.

What this does **not** yet provide: editing a container, saving from Wordcraft, rich
formatting beyond what Wordcraft's own renderer draws from the decoded payload, physical
USB pad, or any recovery. The demo's restricted text editor remains the only writable
container editor, and it still refuses rich documents.
