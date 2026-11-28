# Reference UI correction and implementation plan

## User intent
Settings is an internal application page, NOT a second desktop window. The supplied Quiet MD screenshots define the desired layout: settings navigation rail and grouped rows; a file-card homepage with New/View/More menus and a keyboard-driven search palette; close/maximize/minimize controls in the top-right.

## Implementation order
1. Replace auxiliary-window settings with reversible in-app navigation. Preserve underlying editor, form, undo state, pending save failure and IME boundaries. Settings must not register document text input. Escape/Back restores the previous surface; emergency wipe and guarded close still work.
2. Adapt Quiet MD's grouped settings-row/card geometry and category rail, using the upstream source in `/tmp/quiet-md-reference` as a design reference. Implement real appearance, toolbar/sidebar and library-view preferences; show unsupported storage/network capabilities honestly rather than fake toggles. Keep privacy and encrypted persistence invariants.
3. Replace New/Open promotional homepage cards with actual folder/document cards. Add grid/list view, title sort direction, New/View/More menus and metadata-only keyboard search. Folder creation must validate a single path component and refuse overwrite. No decrypted previews or disk-backed recent-file index.
4. Add vector-painted top-right minimize, maximize/restore and guarded close controls; titlebar drag region must not swallow control clicks.
5. Add pure navigation/filter/folder validation tests and update real native runtime drills to assert the SAME native window enters/exits Settings, preserves failed saves, wipes while Settings has focus, and navigates the homepage/search/menus.
6. Run exact-SHA workspace/native/Fedora checks; inspect actual screenshots at normal and compact/high-DPI sizes. Correct visible layout faults before claiming visual verification.

## Acceptance boundaries
- Layout/style and implemented interactions are the target. Do not claim exact pixel parity before direct screenshot comparison.
- Quiet MD's database/board/Markdown-import/agent/trash features are not Holonomy engine capabilities and must not be presented as working controls. Network search/images remain off; no plaintext content previews.
- Preferences are session-only unless a separately audited metadata-only persistence design is implemented.
- Existing security, failure, wipe and performance thresholds must not be relaxed to qualify the redesign.
- Stay on the current Arena branch. Do not merge a partial redesign merely because behavioral tests are green.
