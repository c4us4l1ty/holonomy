# Holonomy → Google Docs GUI Rework Plan

## Current state
Phase 1 (colour retheme + Docs toolbar) — DONE, committed
Phase 2 (suggestion mode + Suggesting ViewMode) — DONE, committed
Phase 3 (rulers + spelling squiggles) — IN PROGRESS

## Constraints (in force)
- 2 MiB static musl binary ceiling (current 1,478,456 bytes → ~619 KB headroom)
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

### Remaining Phase 3 work
- Smart Compose: inline grey predictions (stub with pending_compose field)
- Caret: blinking absolutely positioned (blink already exists, just needs Docs styling)
- Comments sidebar (right panel)
- Margin accept/reject cards

---

## Acceptance criteria (from DOCS.md)
- Whole-screen layout: menubar, pill toolbar, canvas, rulers, sidebar, status bar
- Default 1-inch margins; Pages vs Pageless mode
- Caret blinking absolutely positioned; selection teal overlay
- Suggestion mode: green insertions, strikethrough deletions, margin accept/reject cards
- Smart Compose inline grey predictions; red spelling squiggles
- Toolbar actions: bold/italic/underline/strikethrough, font/color, alignment, bullets/numbering, indent/outdent, page break, image, equation, find/replace, word count, commenting, suggesting mode, version history, share
- Keyboard shortcuts; collaboration cursors
- 2 MiB binary ceiling respected; no runtime rasterisation
