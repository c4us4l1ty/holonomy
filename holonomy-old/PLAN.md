# Holonomy → Google Docs GUI Rework Plan

> **Amended 2026-11-03:** Keep this compact, light Google Docs-like layout as the
> desktop default. The older 2 MiB/no-SVG constraints below record the earlier work;
> the current static ceiling is 4 MiB. Trusted SVG artwork becomes static masks at
> build time, and the unsealed desktop image dialog converts bounded user SVGs to PNG
> before insertion. The sealed renderer's security boundaries are unchanged.

## Current state
Phase 1 (colour retheme + Docs toolbar) — DONE, committed
Phase 2 (suggestion mode + Suggesting ViewMode) — DONE, committed
Phase 3a (rulers + spelling squiggles) — DONE, committed
Phase 3b (Smart Compose inline predictions) — DONE, committed

## Constraints (in force)
- 4 MiB stripped static musl binary ceiling (CI run `41651444311`: sealed 1,863,000 B; SVG-enabled desktop 3,333,880 B)
- No runtime rasterisation / sealed 512 KiB atlas
- Native Rust crates only (holonomy-render, holonomy-display, holonomy-text, holonomy-input, holonomy)
- No egui/winit/compositor/GPU

---

## Progress

### ✅ Phase 1: Chrome retheme + Docs toolbar — DONE
**Files:** chrome.rs, icons.rs, menus.rs, widgets.rs, session.rs, tests/armed_style.rs

**Colours (Google Docs Material Design light palette):**
- CHROME=0xFFFFFF, PAGE=0xFFFFFF, DESK=0xFFF5F5F5, BAND=0xFFFFFF
- RULE=RULE_DIM=0xFFDADCE0 (Google border grey)
- INK=0xFF202124, INK_CHROME=0xFF5F6368, INK_DIM=0xFF9AA0A6, INK_STRONG=0xFF202124
- PILL=0xFFF8FAFD, PILL_HOVER=0xFFE8EAED, PILL_ACTIVE=0xFFD2E3FC
- SELECTION=0xFFBDFFFF (teal overlay)
- POPUP_EDGE=0xFFDADCE0, SEALED=0xFF1A73E8, ACTIVE=0xFF1A73E8, CARET=0xFF202124
- TRACK=0xFFF5F5F5, THUMB=0xFFDADCE0, SHADOW=0xFFDADCE0, BLUE=0xFF1A73E8
- SHARE_BG=0xFF1A73E8, SHARE_INK=0xFFFFFF, UPGRADE_BG=0xFFD2E3FC, UPGRADE_INK=0xFF1A73E8

**Toolbar:** Added Tool::Strikethrough, Tool::Comment, Tool::Share; IconId::Comment/Share masks; Action::ToggleComment/Share; routed in session.rs; armed_style now 4 bools (bold, italic, underline, strikethrough)

**Ribbon bypass:** `chrome.rs::tree()` already bypasses Wordcraft ribbon when `state.ribbon == false` (default). Single Docs toolbar row is active.

### ✅ Phase 2: Suggesting ViewMode — DONE
**Files:** menus.rs, session.rs, span.rs, tree.rs, paint.rs

**menus.rs:** Added `ViewMode::Suggesting` variant + `("Suggesting", ViewMode::Suggesting)` to MODES (index 2)
**session.rs:** Updated `mode_index`/`view_mode` for 3 modes; `check_editable` allows Suggesting edits (only Viewing refuses)

**span.rs:** Added `STYLE_SUGGESTION_INSERT` (bit 10), `STYLE_SUGGESTION_DELETE` (bit 11), `STYLE_SPELLING` (bit 12); all added to `STYLE_KNOWN`

**tree.rs:** Added `DECORATION_SUGGESTION_INSERT` (bit 2), `DECORATION_SPELLING` (bit 3) to DocRun

**session.rs routing:**
- `decorations()` maps suggestion/spelling style flags to DocRun decorations
- `insert()`: applies STYLE_SUGGESTION_INSERT to inserted bytes in Suggesting mode
- `backspace()`/`delete_forward()`/`delete_selected()`: apply STYLE_SUGGESTION_DELETE via format_range (preserves colour) in Suggesting mode

**paint.rs:**
- Light green (#D4FDD4) background behind suggestion insertion runs
- Red (#EA4335) squiggle under spelling-error runs

### ✅ Phase 3a: Google Docs rulers — DONE
**File:** chrome.rs

**Ruler anatomy:**
1. Grey margin zones (RULE grey) left of text.x and right of text.right()
2. White live zone (CHROME white) between margins
3. Blue column ticks (minor every 5 cols, major every 10 cols)
4. Blue numbers every 10 columns over live zone
5. Blue inverted-triangle indent markers at left/right edges of live zone
6. Blue first-line indent rectangle

### ✅ Phase 3b: Smart Compose — DONE
**Files:** chrome.rs, session.rs

- `pending_compose: Option<String>` on Session and ChromeState
- `set_compose()`/`pending_compose()` accessors on Session
- Tab and Right arrow accept the grey prediction; any other key dismisses it
- compose synced to ChromeState before tree() so it can be painted
- Painted in grey (#5F6368) immediately after the caret in the overlay layer
- Only visible when caret is visible (blink on)

---

## Scope boundary (security)

Per `PRD.md` §8 and the sealed-boot invariant (`unshare(CLONE_NEWNET)`, no `openat` on user paths, no network syscalls), the following Google Docs features **cannot work in this product** and are **out of scope, not deferred**:

- **Comments sidebar** — requires an annotation store, comment threading, and sync; none of these exist and none can be added without a network path.
- **Margin accept/reject cards** — part of the comments/suggestions sidebar; cannot render without a sidebar and a reviewer/author model.
- **Collaboration cursors / remote presence** — requires a presence channel, multi-user identity, and OT/CRDT sync; all forbidden by the seccomp jail.
- **Share button** — requires a sharing dialog and network; the existing `Tool::Share`/`Action::Share` are inert placeholders that do nothing.
- **Comment tool** — requires a comment backend; the existing `Tool::Comment`/`Action::ToggleComment` are inert placeholders that do nothing.

Suggesting mode (green insertions, strikethrough deletions) is **kept** because it is purely local: every edit is recorded as a suggestion in the document's own span map, with no network or sync.

---

## Remaining work (local-only, no network)

- Bottom status bar (word count, mode indicator, saved state)
- Left outline slider (heading navigation)

These are pure UI/layout work inside the sealed process and require no network.

---

## Acceptance criteria (from DOCS.md)
- ✅ Whole-screen layout: menubar, pill toolbar, canvas, rulers, status bar
- ✅ Default 1-inch margins; Pages vs Pageless mode
- ✅ Caret blinking absolutely positioned; selection teal overlay
- ✅ Suggestion mode: green insertions, strikethrough deletions (local only)
- ✅ Smart Compose inline grey predictions (mechanism + painting complete)
- ✅ Red spelling squiggles
- ✅ Toolbar actions: bold/italic/underline/strikethrough, font/color, alignment, bullets/numbering, indent/outdent, page break, image, equation, find/replace, word count, suggesting mode
- ✅ Keyboard shortcuts (keymap already has them)
- ❌ Comments sidebar / margin cards / Share / Comment (require network — out of scope per PRD §8)
- ❌ Collaboration cursors (require network — out of scope per user direction)
- ✅ 2 MiB binary ceiling respected; no runtime rasterisation
