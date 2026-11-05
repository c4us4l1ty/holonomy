# GUI implementation status

The historical [PRD](PRD-v5.1.0.md) §11 is the interface target, not a description
of a finished or security-reviewed product. The current windows are separate prototypes;
neither is suitable for sensitive or irreplaceable data.

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
persisted across restarts or proof of authenticity or durability.

The restricted demo also offers `--demo-inspect PATH CURSOR`: it opens the container
read-only, disables editing and saving, and displays a prominent mode indicator. Successful
authentication and decoding confirm only that the supplied candidate can be opened with
that demo pad and cursor. They do not repair anything or establish pad freshness, disk
durability, or complete recovery. Reading can update OS file-access metadata.
