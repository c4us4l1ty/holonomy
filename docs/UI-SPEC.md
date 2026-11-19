# HOLONOMY v6 — UI SPECIFICATION
*Status: Block 1 canon. Written before Block 6–7 UI code, because v3 built UI without one and
the audit found 41 bugs in what nobody had ever specified. Changes require an AUDIT.md entry.*

**UI law U0 (constitutional):** *the UI renders the Eye's run tree, and only that.* If the
toolbar thinks something the document doesn't, the toolbar is lying — and a lying toolbar is
a security defect (it edits somewhere else than it shows).

## 1. Canvas (the quiet part)

- **Page**: centered column, measure **66ch** at body size; margin never < 96 px at 1280 px
  window width; paper `#FBFBF9` (light) / `#1A1A19` (dark); 1 px hairline page edge at zoom ≤
  125%, none above.
- **Chrome budget**: while typing, the only persistent chrome is the toolbar (§3) and a
  24 px status strip (§6). Everything else (scrollbar, rulers) fades to 0% opacity after
  1.2 s idle, back on pointer motion. Reduced-motion: fade instant.
- **Scrolling**: momentum per OS convention; page-down/page-up move by viewport height minus
  one body line (never a "page" of the document — document pages are layout, not scroll
  units; RFC-004 §3).

## 2. Type scale (tokens, not opinions)

| Token | Size | Weight | Use |
| ----- | ---- | ------ | --- |
| body | 17 px / lh 1.58 | 400 | main text |
| h1 | 34 px / lh 1.2 | 600 | one per document top optional |
| h2 | 24 px / lh 1.3 | 600 | sections |
| h3 | 19 px / lh 1.4 | 600 | subsections |
| caption | 13 px / lh 1.4 | 400 | status strip, hints |
| code | 15 px / lh 1.6 | 400 | code spans (mono is a *style* — RFC-004 §1) |

Font stack: system text face (SF Pro / Segoe UI Variable / system sans) default; serif and
mono alternates via toolbar. No webfonts, no network, no telemetry.

## 3. Toolbar (the Google-Docs-shaped strip)

Fixed top bar, 40 px tall, content area starts at x-safe margin. Left-to-right groups,
separated by 8 px gaps and 1 px dividers:

`family ▾` `size ▾` │ `B` `I` `U` `S` │ `h1 h2 h3 ▾ (paragraph style)` │ `align ▾` │ `list ▾ (bullet, number)` │ `link` `hr` │ `quote` `code` │ **security dot** (§6)

Behavior rules:

- Every control reflects **caret/selection state from the Spine via the Eye** (U0). Mixed
  selection → control shows indeterminate state (dash), never the nearest guess.
- Every action is a single Spine transaction (one undo step; replace-all included).
- Disabled ≠ hidden: unavailable actions (e.g., list inside read-only history view) render
  grey with a reason tooltip.
- Overflow below ~760 px window: groups collapse into `⋯` menu **in declared order** — never
  reordered, so muscle memory survives resizing.
- The toolbar never steals focus in a way that eats keystrokes: after a click, focus returns
  to the canvas unless the control is text-entry (size box).

## 4. Caret, selection, input — semantics precisely enough to test

- **Caret** positions exist only at grapheme-cluster boundaries (RFC-004 §4). Arrow keys move
  cluster-by-cluster; word-jump per Unicode UAX #29. BiDi: visual-order arrows (what you see
  is where it goes).
- **Selection** is a set of cluster-aligned ranges rendered as true glyph-run highlights —
  the same rectangles paint and hit-test share (L-8).
- **Click**: single = caret at nearest cluster boundary left-of-point (tie → left);
  double = word; triple = line (visual line); quadruple = paragraph.
- **IME**: preedit string renders in a dedicated underline style; commit replaces the preedit
  atomically (one transaction). The app registers as secure-input where the OS offers it
  (macOS `EnableSecureEventInput` or equivalent; otherwise Security panel says so).
- **Duplicate-caret multi-editing: none in v6.0.** Surface simplicity is a security property.

## 5. Views and empty states

1. **Locked/open screen**: file drop area + passphrase field + "recent world-lines" (local
   cache of paths — paths are not secrets; the setting to disable it is honest about what it
   protects).
2. **First run**: create → passphrase (with measured-strength meter driven by
   `holonomy-seal`'s KDF estimate, not entropy theater) → land in an empty document whose
   first slab exists only on first keystroke.
3. **History scrub (Block 8)**: full-bleed timeline bottom sheet; document at date shown
   **read-only** (toolbar grey per §3); "restore from here" forks, never rewrites.
4. **Recovery**: after any crash, next open shows last committed witness state (L-6) and a
   one-line banner: "Recovered to the last safe point — nothing else was stored."
5. **Empty document**: a single faint line of placeholder text on the page. No onboarding
   modals. The app opens like paper.

## 6. Status strip + Security panel

- **Status strip (24 px)**: word count up to caret / total, page N of M from the Fenwick
  skeleton (O(log N), RFC-003 §4), save witness time, residency meter (plaintext slabs in
  memory vs. bound — the honest number v5 pioneered).
- **Security dot** (toolbar right end): green = ladder fully engaged; amber = degraded, hover
  says exactly which rung failed (e.g., "memory locking unavailable on this OS — reported,
  not pretended"). Red = canary tripped (Block 9) — the app says "plaintext containment
  failed verification" and offers immediate lock.
- **Security panel** (click dot): session facts — resident plaintext bound, wipe proof state
  (canary), clipboard policy state, input capturing mode, container integrity line
  (`verify` age), optional USB hardener state: *inactive* (default) / *armed*.

## 7. Themes, motion, accessibility

- Light/dark follow OS; explicit override in settings. Contrast targets WCAG AA at minimum
  for text, AAA for body (paper pairs above are specified to it).
- Reduced-motion: no fades, no slide; history scrub becomes discrete steps.
- Zoom: 50–400%, pure view transform (RFC-004 §5) — caret geometry is the paint geometry at
  every zoom by construction.
- Screen-reader: the Eye's run tree is the a11y tree source (U0 again) — announced text is
  byte-identical to painted text.

## 8. Parity with quiet-md: the page, to the pixel

The shell's *appearance* is quiet-md's. That is a commitment with numbers, not an aesthetic
adjective, so the numbers are here — read out of quiet-md 0.1.13's own theme source
(`src/theme.rs`, `src/themes.rs`) rather than guessed from screenshots. Its engineering is not
ours to reuse (OQ-13 in `AUDIT.md`: its patched GPUI cannot travel, and Block 1 forbids reproducing
the patch), so the split is exactly this: **the look is quoted, the machinery is ours.**

### The page

| token | value | quiet-md source |
| --- | --- | --- |
| page max width | 820 | `page.width` |
| body size / line height | 15 / 22 | `BODY_SIZE`, `BODY_LINE` |
| paragraph gap | 13 | `BLOCK_GAP`, `p.gap` |
| page background | `#ffffff` | `bg_body` = `craft::N00` |
| body text | `#1f2225` | `text` = `craft::N900` |
| captions, meta | `#737477` | `text_caption` = `craft::N600` |
| placeholders | `#9c9fa0` | `text_placeholder` = `craft::N500` |
| caret | `#3a85f7`, 2pt wide | `caret` = `craft::B500` |
| selection | `#3a85f7` at 24% | `selection` |
| link | `#0f5492` | `mix(0x0087ff, N900, 0.5)` |
| dividers | black at 6% / 8% | `line_divider`, `line_border` |
| quote bar | `N900` at 82% | `quote_bar` |
| code background / gutter | `#fbfbfb` / `#f8f8f8` | `code_bg`, `code_gutter` |

### Headings and title

| level | size | line | margin-top | margin-bottom | weight |
| --- | --- | --- | --- | --- | --- |
| title | 34 | 55.25 | 0 | 0 | 700 |
| h1 | 21 | 27 | 48 | 16 | 700 |
| h2 | 19 | 25 | 36 | 16 | 700 |
| h3 | 18 | 24 | 28 | 12 | 600 |
| h4 | 16 | 25 | 16 | 16 | 600 |
| h5–h9 | 15 | 22 | 13 | 13 | 600 |

### Type

System UI family for chrome, and the platform's monospace for code — `SystemUIFont` +
`AppleSystemUIFontMonospaced` on macOS, `Consolas` on Windows, `Noto Sans` / `Noto Sans Mono` on
Linux. **No webfonts, no network fetches** (§2), which quiet-md also honours: it uses the system's
own families and bundles only what it draws itself.

### What parity means operationally

1. These values live in one table in the shell crate (Block 6), and a test asserts the table equals
   the numbers above. Drift is a failing test, not a design review.
2. A screenshot comparison at matched zoom and DPI is the acceptance check for Block 6, recorded in
   `AUDIT.md` with the two images named — a claim of "looks like quiet-md" that is not a diff is not
   a claim.
3. A deliberate deviation is written down with its reason and its measurement. quiet-md does this
   itself: its title is 34/55.25 against a 21pt reference, and its source says the larger title is
   "an intentional deviation, not a measurement". That is the standard.

### What parity does not include

- **Its code.** OQ-13: a dependency's `[patch.crates-io]` does not travel, quiet-md's app calls API
  its 43 GPUI patches add, and reproducing them here is the shape Block 1 forbids. `THIRD_PARTY.md`
  is the ledger if a file is ever adapted.
- **Its line breaking.** CJK 避头尾, script-aware word classes (Hangul, Hebrew, Arabic, Syriac) and
  RTL line order live in GPUI's line wrapper, which this design does not use: the Eye lays out the
  page. Patches #4, #23, #30, #31, #32 are the reference list for what correct means there, and the
  gap AUDIT records as OQ-11's remaining half: BiDi.
- **Its font fidelity patches.** Smoothing parity, synthesized oblique and the PingFang PostScript
  collision are #1, #12, #14. Without them the page is stock GPUI, and a fidelity claim in a gate is
  measured against stock GPUI, never against quiet-md's pixels.

## 9. What is deliberately absent in v6.0

Comments/threads, inline images, tables, math (Block 10+ composition), sharing/people UI,
chat, AI panels, telemetry of any kind, and any UI element that requires materializing more
than the viewport (that is law L-2, and the UI obeys it too).
