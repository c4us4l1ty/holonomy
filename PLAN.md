# Holonomy → Google Docs GUI Rework Plan

## Current state (Wordcraft-themed ribbon)
- chrome.rs Layout bands: title(40) → menubar(32) → tabs → toolbar(ribbon groups) → ruler → canvas → status
- ribbon.rs: 8 ribbon tabs — but `state.ribbon` defaults to `false`, so `tree()` takes the non-ribbon path (paint_title/paint_menubar/paint_toolbar)
- widgets.rs Tool enum: now includes Strikethrough, Comment, Share (added)
- menus.rs Action enum: now includes ToggleComment, Share (added)
- session.rs: routes ToggleComment, Share, ToggleStrike → Strikethrough tool; armed_style now 4 bools
- Colours: Google Docs Material Design light palette (DONE)

## Target state (Google Docs, per DOCS.md)
Whole-screen bands top→bottom:
1. **Menubar**: File / Edit / View / Insert / Format / Tools / Extensions / Help
2. **Single formatting toolbar** (no ribbon tabs): font box, size box, B/I/U/S, font colour, highlight, link, comment, alignment, line spacing, lists, indent/outdent, clear formatting; then editing-mode selector (Editing/Suggesting/Viewing), comments icon, Share button (blue pill)
3. **Rulers** (top + left): grey margin zones + white live zone + blue indent markers + blue first-line indent rectangles
4. **Document canvas**: white page(s), 1-inch default margins, caret as blinking absolutely positioned element, selection as teal overlay
5. **Right sidebar**: comments / suggestions (green insertions, strikethrough deletions, margin accept/reject cards)
6. **Bottom status bar**

## Constraints (in force)
- 2 MiB static musl binary ceiling (current 1,478,456 bytes → ~619 KB headroom)
- No runtime rasterisation / sealed 512 KiB atlas
- Native Rust crates only (holonomy-render, holonomy-display, holonomy-text, holonomy-input, holonomy)
- No egui/winit/compositor/GPU

---

## Progress

### ✅ Phase 1a: chrome.rs colour retheme — DONE
- CHROME=0xFFFFFF, PAGE=0xFFFFFF, DESK=0xFFF5F5F5, BAND=0xFFFFFF
- RULE=RULE_DIM=0xFFDADCE0 (Google border grey)
- INK=0xFF202124, INK_CHROME=0xFF5F6368, INK_DIM=0xFF9AA0A6, INK_STRONG=0xFF202124
- PILL=0xFFF8FAFD, PILL_HOVER=0xFFE8EAED, PILL_ACTIVE=0xFFD2E3FC
- SELECTION=0xFFBDFFFF (teal overlay)
- POPUP_EDGE=0xFFDADCE0, SEALED=0xFF1A73E8, ACTIVE=0xFF1A73E8, CARET=0xFF202124
- TRACK=0xFFF5F5F5, THUMB=0xFFDADCE0, SHADOW=0xFFDADCE0, BLUE=0xFF1A73E8
- SHARE_BG=0xFF1A73E8, SHARE_INK=0xFFFFFF, UPGRADE_BG=0xFFD2E3FC, UPGRADE_INK=0xFF1A73E8

### ✅ Phase 1b: widgets.rs Tool enum — DONE
- Added Tool::Strikethrough (IconId::Strike), Tool::Comment (IconId::Comment), Tool::Share (IconId::Share)
- icon()/label()/name()/has_dropdown()/separated()/width()/label_cells() all handle new tools via catch-alls
- TOOLBAR list: ..., Link, Comment, Image, Overflow, Mode, Share, Collapse

### ✅ Phase 1c: icons.rs — DONE
- Added IconId::Comment, IconId::Share with COMMENT and SHARE 16×16 masks
- mask() match updated

### ✅ Phase 1d: menus.rs Action enum — DONE
- Added ToggleComment, Share variants
- name() handles both

### ✅ Phase 1e: session.rs routing — DONE
- press(): Comment→ToggleComment, Share→Share
- apply_action(): ToggleComment→pointer_inert (sealed, no backend), Share→pointer_inert
- apply_command(): both → return None
- tool_command(): Strikethrough→Hotkey::Strike
- set_armed_style(): 4 bools (bold, italic, underline, strikethrough)
- armed_style(): returns 4 bools
- StyleFlags: strikethrough bool added; StyleFlagsSlot::Strikethrough added; slot_for handles Strikethrough
- overflow excludes Share from hidden-tools calculation

### ✅ Tests updated
- armed_style.rs: 4-tuple throughout, STYLE_STRIKE import, model_of/flags_of take 4 bools

---

## Remaining work

### Phase 1f: Structural — verify ribbon bypass
- `chrome.rs:2614` already bypasses ribbon when `state.ribbon == false` (default)
- `Session::new` sets `ribbon: false` — the Docs toolbar path is active
- ribbon.rs Wordcraft code is dead but harmless; can be removed in cleanup

### Phase 2: Suggestion mode + comments sidebar
- editor.rs: suggestion spans (green insertions, strikethrough deletions)
- tree.rs: Node::SuggestionInsert, Node::SuggestionDelete, Node::CommentAnchor, Node::MarginCard
- paint.rs: green/teal selection overlay, suggestion visuals
- session.rs: Suggesting mode routing, margin accept/reject
- ViewMode: add Suggesting variant

### Phase 3: Polish — rulers, Smart Compose, squiggles, caret
- Rulers: grey margin zones + white live zone + blue indent markers
- Smart Compose: inline grey predictions
- Spelling: red squiggle SVG underlines
- Caret: blinking absolutely positioned

### Acceptance criteria (from DOCS.md)
- Whole-screen layout: menubar, pill toolbar, canvas, rulers, sidebar, status bar
- Default 1-inch margins; Pages vs Pageless mode
- Caret blinking absolutely positioned; selection teal overlay
- Suggestion mode: green insertions, strikethrough deletions, margin accept/reject cards
- Smart Compose inline grey predictions; red spelling squiggles
- Toolbar actions: bold/italic/underline/strikethrough, font/color, alignment, bullets/numbering, indent/outdent, page break, image, equation, find/replace, word count, commenting, suggesting mode, version history, share
- Keyboard shortcuts; collaboration cursors
- 2 MiB binary ceiling respected; no runtime rasterisation