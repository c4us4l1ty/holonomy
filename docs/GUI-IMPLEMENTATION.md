# GUI implementation status

The historical [PRD](PRD-v5.1.0.md) §11 is the interface target, not a description
of a finished or security-reviewed product. The current windows are separate prototypes;
neither is suitable for sensitive or irreplaceable data.

`crates/holonomy-ui` now has its own build/test workspace, an interactive read-only Wordcraft
container viewer, and a status projection that consumes an actual store `SaveReport`. The viewer
uses the preview crate's tested read-only boundary and deterministic demo pad. Its GUI masks
credentials visually, but makes no guarantee that UI allocations are memory-locked or wiped. Its
UI tests create temporary demo containers and exercise form request preparation, the asynchronous
read-only cursor search, and the open worker. They assert that a candidate cursor is found without
changing bytes, credentials are cleared, and a subsequent open produces a read-only viewer. This does
not simulate native keyboard or mouse entry; the Xvfb smoke check only verifies the open screen starts.
The status code is not wired into a writable editor workflow. A reported ratchet count is not independent proof of
persistent device writes, and the store still reports pad freshness as *not established*.

| PRD interface goal | Current state | Work still needed |
| --- | --- | --- |
| Native Wordcraft editor | An opt-in, unencrypted scratch window and an interactive read-only Wordcraft viewer over a demo-pad container exist. The restricted writable container editor is plain text only; no editable Wordcraft container UI exists. | Integrate writable Wordcraft only after controlling and testing every open, save-as, export, print, clipboard, drag/drop, autosave and recovery route that can expose plaintext. Preserve formatting and make data-loss behavior explicit. |
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

`crates/holonomy-preview/src/bin/holonomy-rich-view.rs` was the first path where the real
Wordcraft renderer drew a container document. `crates/holonomy-ui/src/main.rs` now exposes an
interactive open screen as well as CLI modes from the standalone UI package. The screen can search
legal cursor candidates through the existing read-only store scan; it reports the first authenticating
candidate with attempt progress, clears credentials and requires them to be re-entered before opening.
In the interactive viewer, `Ctrl/Cmd+O` closes the current view and returns to the open form;
`Ctrl/Cmd+Q` closes the window. The search is cancellable between full-image attempts, but does not
establish freshness, uniqueness, recency or pad history and does not repair anything. The window is
one-way by construction, not by configuration:

```text
holonomy                                  # open the interactive screen
holonomy --view PATH CURSOR               # hidden terminal credential prompts + window
holonomy --inspect-only PATH CURSOR       # hidden prompts + decode/report, no window
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
   shortcut can reach a command. The host consumes `Ctrl/Cmd+Q` to close and, in the interactive
   viewer only, `Ctrl/Cmd+O` to return to the open form; neither chord reaches Wordcraft.

Neither boundary is a sandbox and neither is a claim about Wordcraft's internals. The
decoded document is plaintext in the process, the renderer allocates further copies, and
the OS, swap and core dumps can expose them. Capture exclusion, memory locking and the
reaper are still not integrated, so the window's own banner says what it is: read-only,
not protected.

What this does **not** yet provide: editing a container, saving from Wordcraft, rich
formatting beyond what Wordcraft's own renderer draws from the decoded payload, physical
USB pad, or any recovery. The demo's restricted text editor remains the only writable
container editor, and it still refuses rich documents.
