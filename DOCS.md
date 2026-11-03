# GOOGLE DOCS GUI — PIXEL-LEVEL VISUAL ANATOMY REPORT

**Subject:** What the Google Docs graphical user interface looks like, down to the last pixel — the DOM class inventory, the visual rendering of every chrome element, the caret, selection, suggestions, comments, rulers, margins, and the canvas-tile painting surface.

**Method:** Web research only (search + page fetch). Every visual claim is traced to a cited public observation of the live DOM or to Google's own announcements. No access to Google source code. Where an account is a secondary UI description (not a direct DOM inspection), it is labeled.

**Key primary visual sources:**
- Grammarly gist `Grammarly-gDocsCanvasInjectedCs.js` (gzomer, last active Nov 2024) — the most complete public inventory of Google Docs DOM class names and selectors: https://gist.github.com/gzomer/2b809174ce380fced61040005a9a9576
- James Somers, "How I reverse-engineered Google Docs" (Nov 2014, Draftback) — kix-page, kix-lineview, kix-wordhtmlgenerator-word-node, cursor as a 2px-wide div: https://features.jsomers.net/how-i-reverse-engineered-google-docs/
- Stack Overflow #41419141 — kix-selection-overlay (teal selection background), kix-cursor-caret: https://stackoverflow.com/questions/41419141/
- Stack Overflow #12403863 — custom surface/layout engine, 1-pixel-wide div cursor, character widths calculated on the fly: https://stackoverflow.com/questions/12403863/
- Stack Overflow #70996296 — canvas tile DOM path, `_docs_force_html_by_ext`, `_docs_annotate_canvas_by_ext`: https://stackoverflow.com/questions/70996296/
- Stack Overflow WebApps #25165 — kix-selection-overlay opacity behavior: https://webapps.stackexchange.com/questions/25165/
- ApexGear / Lifewire / Phoenix Ghostwriting — ruler grey/white margin zones, blue indent markers, double-headed arrow drag, 1-inch defaults: https://apexgear.blog/how-to-check-margins-google-docs ; https://www.lifewire.com/change-margins-in-google-docs-4155086
- Android Police — Material You / Material Design 3 refresh for Docs on the web (Mar 2023): pill-shaped light-blue toolbar, rounded Share button, light-blue comments sidebar: https://www.androidpolice.com/google-docs-and-drive-ui-refresh/
- BleepingComputer — Docs gets Material Design on the web (Jan 2019), Google Sans typeface, Polymer/paper elements: https://www.bleepingcomputer.com/news/google/google-docs-sheets-slides-and-sites-get-material-design/
- GeeksforGeks interface tour — menu bar, toolbar, document area, rulers, Share button, comments icon, right sidebar (comments/suggestions/outline), left outline slider: https://www.geeksforgeeks.org/google-docs/what-is-google-docs/
- Google Workspace Updates May 2021 — canvas-based rendering announcement: https://workspaceupdates.googleblog.com/2021/05/Google-Docs-Canvas-Based-Rendering-Update.html
- The New Stack — side DOM, ARIA live regions, AOM (Dion Almaer / Paul Kinlan AMA): https://thenewstack.io/google-docs-switches-to-canvas-rendering-sidelining-the-dom/

---

## 1. THE WHOLE-SCREEN LAYOUT (TOP TO BOTTOM, LEFT TO RIGHT)

Opening a Google Doc presents a single-page application divided into fixed regions:

```
+-----------------------------------------------------------------------+
| [Menu bar]  File Edit View Insert Format Tools Extensions Help       [Share]  |
+-----------------------------------------------------------------------+
| [Toolbar] font | size | B I U | link | comment | ... | Editing v   [chat]   |
+-----------------------------------------------------------------------+
| Ruler:  [grey margin][===== white live area =====][grey margin]      (top)   |
+-----------------------------------------------------------------------+
| [grey margin vertical]                                                (left)   |
|                                                                       |
|    [PAGE CANVAS — white paper, text, caret, selection, remote         |
|     collaborators' carets, comments, suggestions, spelling squiggles] |
|                                                                       |
|                                                [right sidebar:        |
|                                                 comments / suggestions| |
|                                                 / outline / explore]  |
|                                                                       |
+-----------------------------------------------------------------------+
| Status: "All changes saved to Drive" / editing mode indicator         |
+-----------------------------------------------------------------------+
```

### 1.1 Menu bar (top edge)
A standard menubar strip: **File, Edit, View, Insert, Format, Tools, Extensions, Help**. Each opens a dropdown. Far right of this row holds the **Share** button. This is ordinary HTML/CSS chrome (not canvas).

### 1.2 Toolbar (directly below menu bar)
A single-row formatting toolbar containing, left to right approximately: undo/redo, print, spell-check, zoom dropdown, font-family dropdown, font-size dropdown, bold/italic/underline/strikethrough, text color, highlight color, link, insert comment, alignment (left/center/right/justify), line spacing, numbered/bulleted lists, indent/outdent, clear formatting, and — at the far right — the **editing-mode indicator** ("Editing" by default, with a dropdown arrow) plus a pencil icon. On Material You builds this collapses into a **pill-shaped, light-blue toolbar** housing frequently used actions [Android Police Mar 2023].

### 1.3 Document canvas (center)
The dominant white area — the "page." In **Pages** mode it renders as discrete sheet(s) with a visible page break between them; in **Pageless** mode it is a continuous infinite scroll with no page boundary. Text is painted by the Kix engine (DOM-era: hand-positioned divs; canvas-era: per-page `<canvas>` tiles). Rulers frame the top and left of this area when "Show ruler" is enabled.

### 1.4 Right sidebar (collapsible)
Appears when comments, suggested edits, the document outline, or Explore are open. On Material You builds it carries a **light-blue background** to distinguish it from the white page [Android Police]. It lists comments chronologically, suggestion accept/reject cards, or outline headings.

### 1.5 Left outline slider
A narrow strip showing a navigable heading outline (View > Show document outline). Clicking a heading jumps the canvas to that section.

### 1.6 Status bar (bottom edge)
Read-only text: "All changes saved to Drive" (with occasional "Saving…"), plus the editing-mode pill. Autosave is continuous and implicit — there is no Save button.

---

## 2. VISUAL DESIGN LANGUAGE (MATERIAL DESIGN EVOLUTION)

The chrome (everything outside the canvas) follows Google's design system, which has been rewritten at least three times:

| Era | Visual signature | Source |
|-----|------------------|--------|
| Material Design (web, Jan 2019) | Google Sans typeface, updated buttons/dialogs/sidebars, crisp legible iconography, four Google colors, generous white space; implemented via Polymer + "paper elements" | BleepingComputer |
| Material You (Android, Sep 2021) | Rounded "squircle" shapes, subtle pastel shades, fully rounded oval search bar, flat plus icon, basic light/dark themes | Android Police Sep 2021 |
| Material Design 3 / Material You (web, Mar 2023) | Pill-shaped light-blue toolbar; rounded Share button (replacing rounded-corner rectangle); light-blue comments sidebar; removed line separators; "bubbly" interface; reduced clutter; sky-blue/gray tints to separate comments/taskbar from page | Android Police Mar 2023; Digital Trends |

Shared principles across eras: minimalist, content-over-clutter, no skeuomorphic textures, drop-shadow layering for depth (Material Design "cardstock" stacking), baseline grids and keylines for alignment [designmodo; tutsplus].

---

## 3. THE DOCUMENT CANVAS, PIXEL BY PIXEL

### 3.1 Pages vs Pageless
- **Pages mode:** discrete white sheets separated by visible page-break gaps; rulers show grey margin zones; header/footer areas exist at top/bottom of each sheet. Default margins: **1 inch (2.54 cm) on all four sides** (top/bottom/left/right) [ApexGear; Lifewire].
- **Pageless mode:** continuous scroll, no page boundaries, no traditional margins; instead a "Text Width" setting controls the live column. Rulers disappear [ApexGear].

### 3.2 The rulers (visual measurement UI)
When View > Show ruler is on:
- **Horizontal ruler:** sits directly above the page. Grey segments at the far left and right represent the margins; the white segment is the live text zone. On it sit blue markers:
  - A **blue inverted triangle** = left/right indent handle.
  - A **blue rectangle** above the triangle = first-line indent.
  - The grey-to-white boundary = the margin itself; hovering it turns the cursor into a **double-headed arrow** for drag adjustment. Dragging draws a **blue vertical guide line** down the page previewing the new margin [Lifewire; Phoenix Ghostwriting; ApexGear].
- **Vertical ruler:** left edge, grey top/bottom segments = top/bottom margins and header/footer space; white middle = live area [Phoenix Ghostwriting].

### 3.3 Text rendering (DOM era, pre-2021)
The visible text was not plain `<p>`/`<span>` flow. It was a **deep hierarchy of absolutely positioned divs**, each hand-measured:
- `kix-page` — a page sheet
- `kix-page-column` / `kix-page-column-bottom` — column within a page
- `kix-paragraphrenderer` — a paragraph
- `kix-lineview` — a line of text (`div.kix-lineview`)
- `kix-lineview-text-block` — text within a line
- `kix-wordhtmlgenerator-word-node` — a word/text node holding actual characters
- `kix-tablerenderer` / `kix-cellrenderer` — tables and cells
- `kix-paginated-footnoteview` — footnotes

The engine calculated each character's width on the fly and positioned divs accordingly [Stack Overflow #12403863; James Somers]. Because the browser was forbidden from line-wrapping, the JS engine did its own paragraph layout.

### 3.4 Text rendering (canvas era, 2021–present)
The document area migrated to **per-page `<canvas>` tiles**:
- `canvas.kix-canvas-tile-content` — the actual canvas element for a page
- `div.kix-rotatingtilemanager.docs-ui-hit-region-surface` — the container that reuses/recycles canvas tiles as you scroll (like mobile list cells)
- `div.kix-page-paginated.canvas-first-page` — the first visible canvas page [Stack Overflow #70996296]
- Scrolling remains **native DOM scrolling** of a regular scroll area holding the canvas tiles [HN chrismorgan].
- The browser still performs **glyph shaping and painting** via the canvas text API; Kix does macro-layout (line breaking) itself [HN chrismorgan].
- Images and graphic elements remain **SVG/DOM overlays**, not canvas pixels [HN neals].

### 3.5 Escape hatches for extensions (visual proof of the dual render paths)
Google exposes two temporary, AS-IS flags:
- `window._docs_force_html_by_ext = <extension-id>` → force legacy HTML/DOM rendering
- `window._docs_annotate_canvas_by_ext = <extension-id>` → annotate the canvas with mirror DOM elements so extensions can compute text positions [Stack Overflow #70996296].

---

## 4. THE COMPLETE VISUAL CLASS INVENTORY (THE "LAST PIXEL" LAYER)

From the Grammarly gist (gzomer, Google Docs Canvas integration), the selectors used to locate and style every visible surface:

| Selector / class | Visual role |
|---|---|
| `.kix-appview-editor` | The editor root / scroll surface |
| `.kix-zoomdocumentplugin-outer` | Layout root (zoom/scale container) |
| `.kix-paginateddocumentplugin` | Content root for paginated view |
| `.kix-page` | A rendered page sheet |
| `.kix-page-column` / `.kix-page-column-bottom` | Page columns |
| `.kix-paragraphrenderer` | Paragraph block |
| `.kix-lineview` | A line of text |
| `.kix-lineview-text-block` | Text inside a line |
| `.kix-wordhtmlgenerator-word-node` | A word / text node with characters |
| `.kix-tablerenderer` / `.kix-cellrenderer` | Tables and cells |
| `.kix-paginated-footnoteview` | Footnote area |
| `.kix-lineview-decorations` | Line-level decorations |
| `.kix-canvas-tile-content` | Canvas tile for a page |
| `.kix-rotatingtilemanager` | Canvas tile manager (recycling) |
| `.docs-texteventtarget-iframe` | Hidden input iframe (receives all keystrokes) |
| `.kix-cursor` | Cursor wrapper |
| `.kix-cursor-caret` | The caret rectangle itself |
| `docs-text-ui-cursor-blink` | Active blink state class |
| `.kix-selection-overlay` | The painted selection highlight (teal) |
| `[class*="suggestion-text"]` | Suggested new text |
| `.suggest-changes-top-colorbar` | Suggestion margin color bar |
| `.docs-smartcompose-suggestion` | Smart Compose inline suggestion |
| `.kix-spelling-error-overlay-container.kix-htmloverlay.docs-ui-unprintable` | Spelling error overlay container |
| `svg.kix-spelling-error-overlay` | The red squiggle under misspelled words |
| `.kix-spell-bubble` | Spelling suggestion popup bubble |
| `.kix-person-entity-view` / `.kix-person-entity-name` | Smart chips / person mentions |
| `.docos-input` | Comment input container |
| `#docs-revisions-sidebar` | Version history sidebar |
| `.docs-icon-acl-view-only-on-brand-color-background` | View-mode icon |
| `#titlebar-mode-indicator-container .kix-titlebar-request-access-button` | Request-edit-access button |

**Caret details:** The caret is a **1px-wide (reported as ~2px-wide by Somers) absolutely positioned `<div>`** (`kix-cursor-caret`) whose `display` is toggled `none`/`inline` by a JS timer to produce the blink [Stack Overflow #12403863; #7877225; #8392270]. It ignores the OS "solid cursor / no blink" preference; users must override via `.kix-cursor-caret { animation: none !important }` [Latenode 2025]. The `docs-text-ui-cursor-blink` class marks the active blink state [Grammarly gist].

**Selection details:** Selection is **not** the browser's native `::selection`. It is a hand-painted overlay div, `.kix-selection-overlay`, given a teal background **with opacity** (so setting `background:#2f65c0` yields an actual `#d5e0f3` due to alpha compositing) [WebApps SE #25165]. It is positioned independently of the text divs — not connected to the containing text element [Stack Overflow #41419141].

---

## 5. CURSORS AND PRESENCE (SINGLE-USER ONLY)

- **Local caret:** the blinking caret at the insertion point.
- **No remote collaborators.** This product runs inside a sealed process (`unshare(CLONE_NEWNET)`, no network syscalls) per `PRD.md` §5.5 and §8. There is no presence channel, no multi-user identity, and no OT/CRDT sync. Only the local caret is painted.

---

## 6. SUGGESTING MODE — THE VISUAL APPEARANCE OF TRACKED CHANGES

Toggled from the pencil icon top-right (Editing / Suggesting / Viewing). Shortcut: `Ctrl+Alt+Shift+Y` (Win/Linux) / `Cmd+Option+Shift+Y` (Mac).

When Suggesting is on, every edit becomes a **visible proposal**:

| Change | Visual rendering |
|---|---|
| **Insertion** | New text highlighted/underlined, typically **green**, wrapped in suggestion markup |
| **Deletion** | Original text kept with a **strikethrough** (struck through), typically green-tinted |
| **Replacement** | Deletion strikethrough + new green text inline |
| **Formatting change** | Marginal note describing the style change |
| **Margin card** | Not available — the comments/suggestions sidebar requires a network path and is out of scope per `PRD.md` §8. |
| **Keyboard** | Not available — comment shortcuts require a comment backend, which is out of scope per `PRD.md` §8. |
| **Bulk review** | Not available — the review sidebar requires a comment backend, which is out of scope per `PRD.md` §8. |

Data model: each `TextRun` carries `suggestedInsertionIds[]` / `suggestedDeletionIds[]`, so suggestions are first-class elements in the document model, not overlays hacked on top [Docs API].

**Viewing mode:** shows the document as if all suggestions were accepted (clean final output), read-only.

---

## 7. COMMENTS UI (NOT AVAILABLE)

- Comments are **not available** in this product. The comments sidebar, comment threads, and the "Add comment" action require an annotation store and a network path for sync, which are forbidden by `PRD.md` §5.5 (`unshare(CLONE_NEWNET)`) and §8. No comment UI is rendered.

---

## 8. INLINE ASSISTANCE VISUALS

### 8.1 Smart Compose
Inline predictive text suggestions appear as `.docs-smartcompose-suggestion` — typically greyed-out suggested words after the caret that the user can accept with Tab [Grammarly gist].

### 8.2 Spelling / grammar
Misspelled words get:
- `.kix-spelling-error-overlay-container.kix-htmloverlay.docs-ui-unprintable` — overlay container
- `svg.kix-spelling-error-overlay` — the **red squiggle** drawn under the word
- `.kix-spell-bubble` — the popup bubble with correction options [Grammarly gist]

---

## 9. THE HIDDEN INPUT LAYER (WHAT YOU ACTUALLY TYPE INTO)

**Critical:** The user never types into the visible canvas/DOM page. All keyboard input is captured by:

```
<iframe class="docs-texteventtarget-iframe">
  <body contenteditable role="textbox">...</body>
</iframe>
```

- It is **not** `display:none` (that would make it unfocusable); it is hidden geometrically — reported as **1px tall, positioned at top -10000** (offscreen) [SebA-R/orbi PR #85].
- Its body is `contentEditable` / `[role=textbox]` [Interceptor].
- `keydown`/`beforeinput`/`input`/paste events land there; Google's JS **copies the text into its own model and immediately clears the iframe** [Stack Overflow #8752466].
- Paste arrives as a synthetic `ClipboardEvent('paste')` carrying `text/html` dispatched into that iframe; Docs converts HTML into real headings, lists, tables, links [browser-use/browser-harness #924].
- With canvas rendering, `document.execCommand` and direct DOM edits on the visible page do nothing — this iframe is the sole write channel [browser-use #924; openstream #368].
- Physical US-layout keycodes are used (a char's charCode as keyCode breaks Docs: `%`→ArrowLeft, `!`→PageUp) [theluckmaster/typist].

---

## 10. VERSION HISTORY SIDEBAR (VISUAL)

- Selector `#docs-revisions-sidebar` [Grammarly gist].
- Opens via the "All changes saved to Drive" link / File > Version history.
- Shows a timeline of named/auto-saved revisions with timestamps; clicking a revision renders the document as it was at that point; a "Restore this revision" button promotes it.
- "All changes saved in Drive" status lives in the bottom status bar.

---

## 11. KNOWN VISUAL QUIRKS AND FAILURE MODES

1. **Blinking caret ignores OS settings:** no native option for a solid cursor; must be overridden via user CSS on `kix-cursor-caret` [Stack Overflow #8392270; Latenode].
2. **Selection color not directly overridable:** `.kix-selection-overlay` has opacity, so forced background colors composite unpredictably [WebApps SE #25165].
3. **Firefox canvas blur:** early canvas rendering produced blurry text/links on Firefox versus the crisp HTML fallback [Stack Overflow #70996296 comments].
4. **No visible margin lines:** margins are perceived as whitespace only; rulers (View > Show ruler) are the visual guide [TinyGrab].
5. **Pageless mode hides rulers and page breaks** [ApexGear].
6. **Extension breakage after canvas migration:** scrapers that read HTML broke; Google offered the two AS-IS flags above [Google Workspace Updates May 2021; Slashdot].
7. **Accessibility side DOM:** canvas painting removes the semantic tree; screen-reader support is opt-in (Tools > Accessibility) and depends on a hidden "side DOM" + ARIA live regions + AOM [WebAIM; The New Stack]. WebAIM's testing of the canvas alpha found AT support rudimentary.

---

## 12. THE ONE-SENSE SUMMARY

Holonomy's Google Docs redesign — menubar, pill-shaped light-blue toolbar, a white page framed by grey-margin rulers, a blinking caret, teal selection boxes, green suggestion insertions with strikethrough deletions, red spelling squiggles — every pixel hand-painted by the renderer inside a sealed process: no network, no collaboration, no comments sidebar, no remote carets. The document surface is painted by the SSE2 blitter from the A8 glyph atlas, with the caret as a blinking absolutely positioned line and selection as a teal overlay.
