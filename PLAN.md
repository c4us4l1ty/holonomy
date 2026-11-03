# Holonomy → Google Docs GUI Rework Plan

## Current state
Phase 1 (colour retheme + Docs toolbar) — DONE, committed
Phase 2 (suggestion mode + Suggesting ViewMode) — IN PROGRESS

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

### ✅ Phase 2a: Suggesting ViewMode — DONE
**Files:** menus.rs, session.rs, span.rs, tree.rs, paint.rs

**menus.rs:** Added `ViewMode::Suggesting` variant + `("Suggesting", ViewMode::Suggesting)` to MODES (index 2)
**session.rs:** Updated `mode_index`/`view_mode` for 3 modes; `check_editable` allows Suggesting edits (only Viewing refuses)

**span.rs:** Added `STYLE_SUGGESTION_INSERT` (bit 10) and `STYLE_SUGGESTION_DELETE` (bit 11); added to `STYLE_KNOWN` via `STYLE_SUGGESTIONS`

**tree.rs:** Added `DECORATION_SUGGESTION_INSERT` (bit 2) to DocRun

**session.rs routing:**
- `decorations()` maps STYLE_SUGGESTION_DELETE→DECORATION_STRIKE, STYLE_SUGGESTION_INSERT→DECORATION_SUGGESTION_INSERT
- `insert()`: applies STYLE_SUGGESTION_INSERT to inserted bytes in Suggesting mode
- `backspace()`: applies STYLE_SUGGESTION_DELETE via format_range (preserves colour) in Suggesting mode
- `delete_forward()`: same for forward delete
- `delete_selected()`: same for selection delete

**paint.rs:** Draws light green (#D4FDD4) background behind suggestion insertion runs

### Remaining Phase 2 work
- Comments sidebar (right panel with comment list)
- Margin accept/reject cards for suggestions
- Suggesting mode indicator in toolbar (mode dropdown already shows 3 options)

### Phase 3: Polish
- Rulers: grey margin zones + white live zone + blue indent markers
- Smart Compose: inline grey predictions
- Spelling: red squiggle SVG underlines
- Caret: blinking absolutely positioned (blink already exists)

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
