# Holonomy desktop editor — bug list and fix list

Audited surface: **only** the real document editor — the window, the menu bar, the ruler, the page,
Normal/Heading/Subheading styles, image and table insertion, and mouse/keyboard responsiveness.
Everything else in the repository is out of scope.

Baseline: `main` @ `7cd016c` (the build the user ran). No code was changed; this is analysis.

Two sources of evidence:

1. **The shipped pixels.** The user's own run (`cargo run --release -p holonomy -- --window`), captured
   at 1917×1157 with the app content occupying x 459–1458, y 317–941 (a **999 × 624 px** client area).
   Every measurement below is in *capture pixels*; absolute scale is stated only where it is derived
   from the capture itself rather than assumed, because the capture may be compositor-scaled.
2. **The source.** Every item names the code that produces it.

## How to read a verdict

The authoritative target is `Plan/Plan.md` **Part 4 (v3.0.0-SINGULARITY)**.

* `CONFORMS` — the behaviour is required (or permitted) by Part 4 as written.
* `VIOLATES` — a specific clause forbids it.
* `AMENDMENT` — **Part 4 is silent.** It has no clause about the editor's visual design, no clause about
  most editor commands, and no clause about Chrome-parity of any control. Silence is not permission:
  each of these needs an amendment to Part 4 before the work can claim compliance. The amendment
  numbers (`AM-n`) are collected in §5.

**Part 4 says nothing about how this application looks.** That is the single largest finding: the
window the user is judging has no specification behind it, so nothing in the engine's very precise
budgets constrains the thing on screen.

## Counts

| Class | Items | Of which PRD-silent (amendment required) |
|---|---|---|
| A. What the window looks like | 16 | 16 |
| B. Interaction, latency, affordances | 11 | 11 (2 also violate NFR-1) |
| C. Document model and commands | 14 | 12 (2 violate FR-1) |
| **Total** | **41** | **39** |

---

# Part 1 — Bug list

## A. What the window looks like

### A1 — There is no toolbar band; a single full-width lozenge is the toolbar
**Seen.** The toolbar row is one rounded capsule 988 × 82 px spanning the entire client width. There is
no band behind it: the pixel value either side of the capsule and the pixel value of the menu row and
of the title row are all the same `(248,250,253)`.
**Cause.** `colour::BAND` and `colour::CHROME` are the *same constant*, `0xFFF8FAFD`
(`crates/holonomy-render/src/chrome.rs:57` and `:61`). `Chrome::tree` does push a band fill for the
toolbar and the status bar (`chrome.rs:2024`), but with a colour identical to the panel it is invisible.
The capsule is `rounded(8, toolbar.y+3, width−16, height−6, r = height/2, PILL)`
(`chrome.rs:2636`) — height/2 on an 82-px band gives a 41-px corner radius, which is why it reads as a
lozenge rather than a toolbar.
**Verdict.** `AMENDMENT` (AM-1): no clause describes chrome surfaces or their contrast.
**Fix.** See F-A1 in §4.

### A2 — The capsule is 76 % empty and the tools are crushed into the left third
**Seen.** 30 ink clusters between x 473 and 1190; the right-hand group (overflow, pencil, collapse) sits
at 1384–1444. Between the last left-group tool and the right group there are ~460 px of bare capsule
(46 % of its width), while the left group is laid out with 5–8 px gaps.
**Cause.** `place_toolbar` lays the left group out from `TOOLBAR_PAD` and the right group from the
right edge, with nothing absorbing the slack (`crates/holonomy-render/src/widgets.rs:356`); the pill is
sized to the band, not to the contents (`chrome.rs:2636`).
**Verdict.** `AMENDMENT` (AM-1).

### A3 — Nothing separates the menu row from the title row or from the toolbar
**Seen.** 40 px of title band, then 70 px of menu band, then the capsule. No rule, no fill, no
elevation anywhere; the menu headings float in the same colour as everything above and below.
**Cause.** No band fill and no rule is emitted for the menu band. The only horizontal rules in the
chrome are the tab bar's and the ruler's baselines (`chrome.rs:2028–2034`), and `tab_h` is 0 with the
tab band gone (`chrome.rs:331`).
**Verdict.** `AMENDMENT` (AM-1).

### A4 — The status bar shows "Saved" and therefore shows *nothing else*
**Seen.** Bottom-left of the client area, one word: `Saved`. No line/column, no word count, no byte
count, no document state.
**Cause.** `chrome.rs:2145`:
`let status = state.notice.clone().unwrap_or_else(|| format!("Ln {}, Col {} …"))`. A notice
**replaces** the counters instead of sitting beside them. `set_notice` is called on every save
(`windowed.rs:428`), so after the first autosave — i.e. essentially always — the status bar is a
single word. The dirty dot beside it is conditional on `state.dirty`, which is false right after a save.
**Verdict.** `AMENDMENT` (AM-2).

### A5 — The sheet is flush against the sidebar with 196 px of empty desk on its right
**Seen.** Page sheet 650–1257 horizontally; the desk starts at 648 (sidebar ends at 648) and ends at
1458. Left gutter **2 px**, right gutter **200 px**.
**Cause.** `Layout::new`'s three-way gutter rule (`chrome.rs:893–906`): when `page_w + 24 > desk` it
takes the "page wider than the desk" branch and sets `gutter_left = sidebar_w, gutter_right = 0`; the
page is then clipped by `page_w.min(width − page_x)` (`chrome.rs:925`). With the sidebar reserving a
fixed 280 units (`ChromeMetrics::DESKTOP.sidebar_w`, `chrome.rs:326`) and `measure_for` clamping the
measure to `[columns, max_columns]` = 72, the sheet cannot be centred in what is left.
**Why it matters.** An off-centre sheet with one gutter at 2 px is the single strongest "this is broken"
signal in the picture: it reads as a layout that fell over, not as a page on a desk.
**Verdict.** `AMENDMENT` (AM-3).

### A6 — The sheet has no bottom margin and no page shape
**Seen.** Page 650–1257 × 554–903: **607 × 349 px**, i.e. 61 % of the client width and **56 % of its
height, ending exactly where the canvas ends** (app content bottom 941, status band start ≈ 903).
**Cause.** `page_h = canvas_h − shadow` (`chrome.rs:911`) — the sheet is defined as "the canvas", not
as a page. There is no page height, no page break, no desk below the sheet, and therefore no cue that
this is a document at all.
**Verdict.** `AMENDMENT` (AM-3/AM-4).

### A7 — The sheet has no shadow and a 1 px grey rim
**Seen.** One `(196,199,197)` hairline on each edge; the "shadow" is a 1 px band that is invisible
against the panel.
**Cause.** `m.shadow` is 1 (`chrome.rs:336`) and `colour::SHADOW` is `0xFFE0E0E0` (`chrome.rs:144`)
drawn as a flat 1-px band (`chrome.rs:2203–2218`), then the four edge hairlines in `colour::RULE`
(`chrome.rs:2225–2231`). Against `CHROME` `0xFFF8FAFD` that is a ΔL of ~24/255 — a line the eye cannot
see at one pixel.
**Verdict.** `AMENDMENT` (AM-4).

### A8 — The ruler's ticks and numbers are effectively invisible
**Seen.** Ticks exist — 15 of them, at 683, 721, 758, 796, 834, 871, 909, 946, 984, 1021, 1059, 1096,
1134, 1171, 1209 (pitch 37.5 px) — and numerals exist at 685.5, 762.5, 837.5, 912, 987.5, 1062, 1136.5,
1212 (every 74.5 px, `1`–`8`). Both are only detectable at a threshold of ≥715/765, i.e. they are
almost the background. The earlier claim that the ruler had *no* ticks was wrong; the ticks are there
and too faint to see.
**Cause.** Ticks are 1 × 2 px in `colour::RULE_DIM` `0xFFE0E0E0` on `CHROME` `0xFFF8FAFD`
(`chrome.rs:2039–2055`); numerals are 12-px `INK_DIM` `0xFF747775` on the *same* row as the tick tops
rather than centred in the band (`chrome.rs:2078–2090`).
**Verdict.** `AMENDMENT` (AM-5).

### A9 — The ruler measures the text column only, with no margin or indent markers
**Seen.** The first numeral sits at 685 — 35 px **inside** the page's left edge (650) — and the last at
1212, 46 px short of the right edge (1257). There are no margin triangles, no indent carets, no tab
stops, and no page-edge brackets.
**Cause.** Ticks are placed at `l.text.x + col * m.cell_w` (`chrome.rs:2042`), i.e. on the text
rectangle, and there is no margin model in the document at all (`Tab` inserts four literal spaces —
`session.rs:4441`; the first-line indent does not exist).
**Verdict.** `AMENDMENT` (AM-5).

### A10 — Body text does not sit on the grid the caret, selection and ruler use
**Seen.** The visible line is 13 identical `f` glyphs spanning 55 px — **4.2 px per glyph** — while the
same capture's ruler proves a grid pitch of **7.5 px** (5 columns per 37.5-px tick). The glyphs are
therefore ~44 % narrower than the grid, they do not land on it, and a line of text occupies a
completely different width from the cells the cursor navigates.
**Cause.** Two layout models. Painting advances the pen by the glyph's own atlas advance
(`Painter::advance_of`, `crates/holonomy-display/src/paint.rs:241`, used at `paint.rs:916`), while
*every* geometric decision uses `cell_w` from the Monospace `'m'` advance
(`Painter::cell_width`, `paint.rs:1047`): `Layout::caret_at` (`chrome.rs:1023–1039`),
`Layout::column_at` (`chrome.rs:1133`), `cells_per_row` (`chrome.rs:1070`), the selection rectangles and
`ChromeMetrics::page_w_for` (`chrome.rs:521`). The proportional face is drawn, the monospace grid is
measured, and the two disagree by a third on a line of `f`s.
**Why it matters.** This is the defect that makes the page look "unready" even when the text is right:
the caret lands between characters, a selection covers the wrong pixels, and a click resolves to the
wrong byte. It is also the one defect whose fix has to choose a side.
**Verdict.** `VIOLATES` FR-1.3's *intent* and `AMENDMENT` in letter. FR-1.3 requires geometry to resolve
"without measuring visual glyph elements"; taking an advance from the **atlas metric table** is an O(1)
table read, not glyph measurement, so the compliant fix is to make the table the single source for both
the pen and the grid — not to start measuring outlines at paint time.
**Fix.** F-A10.

### A11 — The sidebar is open on launch and steals 19 % of the window
**Seen.** 648–459 = 189 px of panel (18.9 %), containing a 24-px circular back arrow, the heading
"Document tabs", one pill row "Untitled" and a trailing glyph — then 430 px of nothing.
**Cause.** `ChromeState::default().sidebar_open = true` (`chrome.rs:1574`), and `Session::new` applies
the reservation at construction (`session.rs:1019–1022`), so a first-time user pays for a rail they did
not open, and pays for it with the sheet's centring (A5).
**Verdict.** `AMENDMENT` (AM-6).

### A12 — The sidebar has no edge, no header, and no rows model
**Seen.** The panel is the same colour as the page's surround; the only structure is the `PILL` row. The
heading sits 40 px in from the left while the back arrow sits at +8, and the row has an orphaned `⋮`
at its extreme right.
**Cause.** `paint_sidebar` (`chrome.rs:2785`) fills with `colour::CHROME` — the panel colour — then
places an icon at `+8` and text at `+40`; there is no rule, no group header, no empty state.
**Verdict.** `AMENDMENT` (AM-6).

### A13 — The scrollbar is a 5-px sliver with an invisible track
**Seen.** x 1448–1452, i.e. 4–5 px of `THUMB` at the extreme right edge, floating vertically with no
track visible around it, no arrows, and no gutter between it and the sheet.
**Cause.** `scrollbar_w` 12, x = `width − 12 − 8` (`chrome.rs:935`), `TRACK` is `0xFFF8FAFD`
(`chrome.rs:140`) — the panel colour again — and the thumb is drawn 4 px narrower than the rail
(`thumb.width.saturating_sub(4)`, `chrome.rs:2246`) with `MIN_THUMB` 16 (`chrome.rs:1941`).
**Verdict.** `AMENDMENT` (AM-7).

### A14 — The sheet's top edge collides with the ruler's baseline
**Seen.** Three horizontal rules stack up in a 9-px band: the page's top rim at y 553 (spanning exactly
the page width, 648–1259), then two full-width rules at y 559 and 561 with one row of panel between them.
Over the sheet's own width the second pair is occluded by the white page.
**Cause.** `Layout::new` sets `canvas_y = ruler_y + ruler_h` and `page_y = canvas_y + shadow`
(`chrome.rs:867`, `:909`), while the two baseline rules are emitted at `l.tabs.bottom()` and
`l.ruler.bottom()` (`chrome.rs:2028–2034`) — and with `tab_h: 0` the tab rule lands on the ruler's own
baseline. The page then starts at the ruler's edge with `shadow = 1` of separation, so the sheet
intrudes into the ruler strip.
**Verdict.** `AMENDMENT` (AM-5).

### A15 — One type size for everything, and no hierarchy
**Seen.** Title, menu headings, toolbar labels (`125%`, `Quote`, `Inter`, `11`), ruler numerals and the
status text are all the same 12-px proportional face. The document title "Untitled" is one step larger
and one step bolder; nothing else differs. The page's own body type is the same size as the chrome's.
**Cause.** `push_text` takes `Style::REGULAR` and the *painter's* single chrome size at every call site
(`chrome.rs:2932`); there is no chrome type scale, no bold chrome face, and the only two sizes in the
whole atlas are the zoom pair.
**Verdict.** `AMENDMENT` (AM-1).

### A16 — No DPI-awareness anywhere
**Seen.** Everything is drawn at 1 : 1 device pixels. On any HiDPI display the window is either tiny or
blurry, and the 16-px icons and 12-px type become 8-px and 6-px equivalents.
**Cause.** `grep -rn 'dpi\|scale_factor\|hidpi\|Xft'` over the crates returns **nothing**. Glyph
rasterisation is fixed at boot (`atlas_plan::sizes_for`, `--ppem 16`, `args.rs:132`); the only scale
mechanism is `with_type_scale(pitch)` (`chrome.rs:459`), which multiplies *band heights* by
pitch/`BASE_PITCH`, not pixels.
**Verdict.** `CONFORMS` for the sealed target — FR-3.3 fixes a 1280×800 panel, so one pixel ratio is
correct there — and `AMENDMENT` for the desktop window (AM-8).

---

## B. Interaction, latency and dead affordances

### B1 — The 800 ms autosave re-seals and fsyncs the whole container on the UI thread
**Symptom.** "I click a button, it appears with lag, and only after several seconds does it show what I
did." The dominant cause is not the click path — it is that the click path is queued behind a
synchronous disk write.
**Cause.** `windowed.rs:268`:
`if session.take_save_request() || (session.has_unsaved_documents() && last_edit.elapsed() >= 800ms) { save(...) }`.
`save` (`windowed.rs:428`) → `Vault::save` (`vault.rs:145`) → `session.document_snapshots()` (an
`Editor::payload()` per document), `encode_bundle`, then `container.write_content(&bytes)` with
`O_SYNC`/`O_DIRECT` semantics. This runs **inside the event loop**, between two events, with no time
slicing, no coalescing and no background writer. The budget in Part 4 for a keystroke→scanout is 0.50 ms;
one container write is orders of magnitude larger, and it happens every 800 ms *while the user is typing*.
**Verdict.** `VIOLATES` NFR-1 (and frustrates FR-3.4's model, which assumes the loop is free between
damage rects).

### B2 — `Conn::next_event` does not wait out its deadline, so the loop polls
**Cause.** `crates/holonomy-x11/src/conn.rs:919–936`: when `read_packet` finds nothing,
`return match deadline { Some(d) if Instant::now() < d => Ok(None), … }`. `ensure` returns `Ok(false)`
as soon as the first `read` would block, so `next_event` returns `None` immediately instead of sleeping
until the deadline. `windowed.rs:297` then calls `session.tick()` and loops. With `EVENT_WAIT` = 16 ms
(`windowed.rs:92`) this is a spin at the size of one syscall, and every iteration is a chance to paint
caret damage.
**Verdict.** `VIOLATES` FR-3.5 / NFR-2's *intent*: the 0.0 %-idle guarantee is implemented for evdev in
the sealed path and not for the developer window.

### B3 — One event per loop iteration, with no drain and no coalescing
**Cause.** `windowed.rs:297` takes exactly one event from `Desktop::next_event`; `Desktop` has a
`VecDeque` (`desktop.rs:410`) and returns the queue one item at a time. A mouse moving 300 px produces
300 `MotionNotify` events; each one runs `hover_at`, invalidates up to two rects and (see B4) paints. A
burst of keystrokes is serialised behind them. There is no `while let Some(ev) = poll()` drain, no
motion coalescing to the last position in the batch, and no key-repeat batching.
**Verdict.** `VIOLATES` NFR-1 in aggregate (the latency of the *n*-th event grows with the backlog);
`AMENDMENT` for the loop shape (AM-9).

### B4 — Every pointer motion repaints, and the repaint is a full-window damage when anything else is open
**Cause.** `handle_pointer`'s `Motion` arm (`session.rs:2470–2535`) invalidates the old and new hover
rects, and — if a popup is open — the popup and its opener, then `tick_with` presents. Separately,
`Session::apply_action`'s `SetFont` and `ToggleBold`/`SetColour` selection paths set
`self.damage = self.chrome.full_damage()` (`session.rs:4040`, `:4015`, `:3977`), which `Desktop::present_damage`
then pushes as a full-frame `PutImage` (`desktop.rs:533`).
**Verdict.** `VIOLATES` FR-3.4 in those paths ("full-screen redraws during typing are strictly
prohibited"), which is why they feel different from typing.

### B5 — There is no I-beam: the pointer is the server's default cursor
**Cause.** No `CreateGlyphCursor` / `cursor_font` request exists anywhere (`grep` over
`crates/holonomy-x11/src/window.rs` returns nothing). The window never sets a cursor for the page, for
text selection, for the resizable edges, or for the toolbar, and there is no `hover`-driven cursor
change.
**Why it matters.** The cursor is the hand's feedback. With an arrow over a white sheet, text editing
feels broken before a single key is pressed, and there is no way to tell a clickable control from a
label.
**Verdict.** `AMENDMENT` (AM-9).

### B6 — Seven of the eight title-bar buttons are inert, including both pills
**Seen.** In the capture: star, folder, cloud, history, comments, sparkle icons, plus an `Export` pill
and a `Local` pill.
**Cause.** `title_command` returns `Some` only for `Cloud → Save` (`session.rs:8110–8120`); every other
`TitleButton` falls to `self.stats.pointer_inert += 1` (`session.rs:3075`). `Export` sets
`export_requested = true` (`session.rs:3893`) but **`windowed.rs` never drains it** — `take_export_request`
has no caller in the window driver — so the one prominent, coloured button in the title bar does nothing
at all. `Local` is a badge drawn as a pill (`chrome.rs:2398–2410`) that is rendered identically to the
button beside it.
**Verdict.** `AMENDMENT` (AM-10). Note `Action::Open`/`Import` *do* have a driver path; `Export` is the
odd one out.

### B7 — Two menu rows open nothing: `View > Mode` and `View > Zoom`
**Cause.** `menus.rs:352–355` builds both with `sub(...)`, which sets `Item::submenu = true`; the
`submenu` field has **zero readers** in the whole workspace (`grep -rn submenu --include=*.rs crates/`
finds only the definitions and the icon). A click on either row therefore produces an item with
`action: None` — drawn with a right chevron, doing nothing. `tests/actions.rs:355` records the same fact
("a submenu that has no renderer").
**Verdict.** `AMENDMENT` (AM-10).

### B8 — The `Format` menu cannot format
**Seen.** `Format` contains exactly two rows: Bold and Italic.
**Cause.** `menus.rs:363–366`. There is no Heading row, no Normal/Subheading row, no alignment, no
spacing, no list. The styles the requirement names (Normal, Heading, Subheading) are unreachable from
the menu that would own them.
**Verdict.** `AMENDMENT` (AM-11).

### B9 — The style dropdown changes a label and nothing else
**Cause.** `Action::SetStyle(i)` sets `self.state.style_index` and invalidates the button
(`session.rs:4021–4028`). Nothing writes a style span; nothing reads `style_index` outside the toolbar
label (`widgets.rs:546`). In the capture the control reads `Quote` while the document is plain text.
**Verdict.** `AMENDMENT` (AM-11).

### B10 — The eight inert tools, one of which is styled to look like the most functional control on the bar
**Cause.** `tests/toolbar_wiring.rs` names the inert set: **Search, Spellcheck, PaintFormat, FontSize,
Underline, Highlight, Link, Comment**. `Comment`'s removal (recorded as part 46's item) never landed, so
it is still in `TOOLBAR` (`widgets.rs:290–313`). `FontSize` is worse than inert: `paint_toolbar` draws a
`RoundRect` in `colour::PAGE` (white) behind it and re-draws the label in `INK`
(`chrome.rs:2754–2760`), so the one control that looks like an input field is a dead rectangle. Underline
and Highlight cannot work at all: `TextIntervalSpan::style_flags` is a `u16` whose `atlas_style` maps
only CODE/BOLD/ITALIC/HEADER (`span.rs:138`), so there is no bit to store them in.
**Verdict.** `AMENDMENT` (AM-10/AM-12).

### B11 — Pasting waits up to 700 ms and is then silently dropped
**Cause.** `windowed.rs:410`: a paste requests the clipboard, sets `pending_paste = Some(now)`, and the
loop top (`windowed.rs:261–266`) fires `paste_internal()` after **700 ms** regardless of whether the
selection has arrived. If the server delivers the selection after that, `handle_clipboard_event`'s bytes
find `pending_paste` already consumed (`windowed.rs:304`) and are discarded — the paste does nothing,
with no message.
**Verdict.** `AMENDMENT` (AM-9).

---

## C. Document model and commands

### C1 — Backspace on any non-ASCII character kills the session
**Cause.** `Editor::backspace` (`crates/holonomy-text/src/editor.rs:1011–1021`) calls
`delete_at(cursor − 1, 1)` — **one byte**, unconditionally — and `delete_at` refuses a non-boundary
offset with `EditorError::NotCharBoundary` (`editor.rs:876`). The session's `backspace`
(`session.rs:5024`) propagates it as `SessionError`, `handle_event_with` propagates anything that is not
`ReadOnly` (`session.rs:4336–4340`), and `windowed.rs` `?`s it out of `run` → the process exits with the
session error code (4).
**Repro.** Type `é` or `—` (the X11 keysym maps through `x11key::layout_char`'s
`0x0100_0000..=0x0110_FFFF` arm, `x11key.rs:317`, so any Unicode keysym is insertable), then press
Backspace. The window closes.
**Verdict.** `VIOLATES` the UTF-8 premise of FR-1.1/FR-1.2 (leaves hold UTF-8; the editing operations
must preserve it). This is the most severe single defect found and it is also the cheapest to fix.

### C2 — Lines longer than the measure are silently cut, and soft wrap exists but is unwired
**Cause.** `emit_body_text` still truncates at the measure; its own comment says so and points at
`holonomy_render::wrap::WrapMap` (`session.rs:6616–6637`): *"What is still missing is the `LineHeights`
half"*. `wrap.rs` has no caller in the session.
**Why it matters.** Text past the page's right edge is invisible while it is in the document. For a
writer this is a correctness failure, not a cosmetic one.

### C3 — `DocRun::MAX_BYTES` truncates long runs in the painter
**Cause.** `tree.rs:265` caps a run at 1024 bytes; `doc_text` clamps and counts `runs_truncated`
(`paint.rs:806–809`). A long line therefore draws only its first kilobyte even if C2 were fixed.

### C4 — Paste and text insertion have silent ceilings
**Cause.** `YANK_CAPACITY = 4096` (`editor.rs:366`) bounds the clipboard; `insert_text`
(`session.rs:1958`) loops `apply_with` per character and stops at the first refusal. Big pastes truncate
without a message.

### C5 — `Tab` inserts four literal spaces; there are no tab stops and no indent
**Cause.** `session.rs:4441` (`self.insert(b"    ", …)`), and `session.rs:4944` for the table indent.
Combined with A9 this is why the ruler has nothing to show.

### C6 — Headings and subheadings cannot be produced at all
**Cause.** Same as B9/B10: `STYLE_HEADER` is written **only** by the export tests
(`crates/holonomy-export/tests/export.rs:37`) and read **only** by the exporters (`html.rs:504`,
`pdf.rs:385`). No session path ever writes it, and `ChromeState` has no style slot beyond an index into
`STYLES`. So Normal is the only reachable paragraph style, and multi-size type does not exist in the
editor.
**Verdict.** `AMENDMENT` (AM-11) — and the amendment has to say what a heading *is* (a `style_flags` bit
plus a size from the atlas, or a paragraph attribute), because `STYLE_HEADER` currently conflates "this
run is a header" with "this line is taller", which is a line metric, not a run property.

### C7 — Image insertion writes U+FFFD into the document and always inserts the same fixture
**Cause.** `insert_image` (`session.rs:1280–1282`) calls `insert_image_bytes(TEST_CHART_PNG)` —
`include_bytes!("../assets/test-chart.png")`, a 1920×1080 committed fixture (`session.rs:7464`). The
anchor bytes it writes are `ANCHOR_BYTES = [0xEF,0xBF,0xBC]` (`crates/holonomy-text/src/asset.rs:71`) —
**U+FFFD, the replacement character**. It is outside `TEXT_RANGES = [(0x20,0x7E),(0xA0,0xFF)]`
(`payload.rs:125`), so the painter finds no glyph, advances one cell and increments `missing`. There is
no file picker, no size control, no wrap, no alignment and no alt text.
**Why it matters.** An image is visible but its anchor is an invisible blank cell, the document now
contains a byte sequence that every other tool reads as corruption, and the same picture is inserted
every time.

### C8 — Tables: fixed 3×3 or nothing, eight columns maximum, no row or column operations
**Cause.** `insert_table(3, 3)` from the menu and `Command::Hotkey(InsertTable)`
(`session.rs:4554–4557`); `TableSpan::MAX_COLS = 8` (`crates/holonomy-text/src/table.rs:25`). There is
no insert-row, delete-row, insert-column, merge, header-row or column-width command anywhere in the
session, and `col_widths` is a fixed `[u16; 8]`.

### C9 — No word-wise motion or deletion
**Cause.** `keymap.rs`'s key table (`keymap.rs:378–425`) has Left/Right/Up/Down/Home/End/PageUp/PageDown
and the Shift family, and nothing for Ctrl+Left, Ctrl+Right, Ctrl+Backspace or Ctrl+Delete. In a
2,000-page document every word boundary has to be walked one character at a time.

### C10 — `line_box` ignores the per-row heights, so tables and headings overlap text below them
**Cause.** `session.rs:5697`: `line_box` reads the uniform pitch, while `LineHeights` carries the extras
that `publish_line_heights` (`session.rs:6029`) computes. `Caret::locate` uses `line_heights.y(r)` and
`Layout::caret_at` uses `row * cell_h` (`chrome.rs:1023`); the two agree today only because no document
this session can hold produces a taller row. Inserting a table (C8) makes them disagree by whole rows.

### C11 — Undo and redo rescan the whole document
**Cause.** `Session::undo`/`redo` call `recount_counts()` (`session.rs:5093–5110`), which is O(document)
by design. Acceptable for an occasional keypress, but it is on the same thread as the paint and the
autosave, so an undo during a save window is two whole-document passes back to back.

### C12 — Undo of a deletion over 64 KiB is refused *after* the fact
**Cause.** `ARENA_BYTES = 64 KiB` (`undo.rs`) and `delete_at` returns `ActionTooLarge` when
`len > ARENA_BYTES` (`editor.rs:902–908`). The check is now before mutation (a previous fix), so this is
correct-but-visible: select-all-then-Delete on a document over 64 KiB does nothing at all, with no
message explaining why.

### C13 — The status bar's numbers are stale by construction
**Cause.** `publish_counts` (`session.rs:5823`) publishes on edit and `invalidate_status_bar`
(`session.rs:3683`) damages the band — but A4 means the numbers are rarely on screen in the first place.
Two defects, one visible symptom.

### C14 — Zoom is two steps, and the label can disagree with `--zoom`
**Cause.** `ZOOMS = [("100%",100),("125%",125)]` (`menus.rs:470`) and `zoom_size_index` refuses anything
else (`session.rs:4173`). Because `AtlasBuilder` refuses a third size, 125 % is the ceiling
(`atlas_plan.rs:14–27`). Separately, the capture shows `125%` on a window launched with the default
`--zoom 100` — consistent with an F11 press (`keymap.rs:412`), which is the only keyboard route, but it
means the label is the only cue and there is no other visible evidence of the size change on an empty
page.

---

# Part 2 — Fix list

Ordered by what unblocks the acceptance test (authoring the two reference documents), not by severity
alone. Each fix names the bugs it closes, and the clause it has to satisfy.

## Tier 1 — the app must not be able to lose itself or the user's text

**F-C1. Delete whole codepoints on Backspace.** Replace `Editor::backspace`'s
`delete_at(cursor − 1, 1)` with a boundary walk backwards from `cursor − 1` (the mirror of
`delete_forward`, `editor.rs:1720–1736`), and have `Session::backspace` read the same byte count so
`TextCounts::after_delete` stays exact. Closes **C1**. Verdict after fix: `CONFORMS` FR-1.1/1.2
(the leaf stays valid UTF-8; no allocation added). *Regression risk:* `after_delete` takes a byte slice —
it must be given all of them, not one.

**F-C2. Never let a model error end the window.** In `windowed.rs`, the `Session` errors that are
*policy* (`NotCharBoundary`, `ReadOnly`, `ActionTooLarge`, `LeafAbsent`) must be counted and dropped, the
way `ReadOnly` already is (`session.rs:4336`), leaving `SessionError::Display` and I/O as the only fatal
ones. Closes the *crash* half of **C1** and the invisible-failure half of **C12**. Verdict: `AMENDMENT`
(AM-13) — Part 4 has no clause on error policy in the UI.

**F-B1. Move the save off the event loop.** Keep the 800 ms debounce, but split it: take the snapshots
(`document_snapshots`) and hand the encoded bundle to a writer that is *not* the paint thread. Two
compliant shapes: (a) a second thread that owns the container handle and a one-slot mailbox
(drop-oldest, never block the loop); (b) keep it single-threaded but write only on an idle deadline and
never between a `ButtonPress` and its repaint. The invariant to protect is FR-4.7/FR-4.8: the container
still goes through O_DIRECT with the locked 3-stage ring, and `mark_documents_saved` still moves only on
success. Closes **B1**. Verdict: `CONFORMS` NFR-1 once the write is out of the keystroke path.

**F-B2. Make `Conn::next_event` actually wait.** When `read_packet` returns `None` and a deadline has not
passed, `poll()` for the remaining time instead of returning. `ensure` already calls `wait`; the bug is
that `next_event` treats "first read would block" as "nothing to do" (`conn.rs:928–935`). Closes **B2**.
Verdict: `CONFORMS` FR-3.5 / NFR-2 (0.0 % idle) for the developer window as well as the sealed one.

## Tier 2 — the page and its chrome have to look like a document

**F-A1. Give the chrome a surface scale, and let one constant own each step.**
`CHROME` stays `0xFFF8FAFD`; `BAND` becomes a distinct value (e.g. `0xFFF1F4F8`) and is actually
visible; the toolbar capsule becomes a *content-fitted* pill (left group, then the labelled controls,
then the right group) with `r = 8`, not `height/2`; the menu band gets a 1-px `RULE_DIM` under it.
One `paint_band` helper so "the toolbar band's colour" is one constant, not two call sites. Closes
**A1, A2, A3**; contributes to **A15**. Verdict: `AMENDMENT` (AM-1). *Proof:* `PaintStats::rects` and
`full_damage` are the instruments — the reference captures in `Plan/` can be compared band by band.

**F-A2. Split the status bar into two regions.** Left: `Ln, Col · words · bytes` from `ChromeState`,
always drawn. Right: the notice, with a timeout. `set_notice` must damage the status band only, which it
already does. Closes **A4**, and makes **C13** visible. Verdict: `AMENDMENT` (AM-2).

**F-A3. Make the sheet a page on a desk.** Three changes, all in `Layout::new` and the page emitter:
(a) centre the sheet in `width − sidebar_w` on *both* axes of the desk, with the four-cell minimum
enforced by clamping the measure rather than by taking the "no gutters" branch
(`chrome.rs:893–906`); (b) give the page a height of its own (a page is a fixed aspect, and the desk
below it is the cue that a second page follows) with `page_h = min(canvas_h − desk, page_height)`;
(c) replace the 1-px flat "shadow" with a 2–3 px band of a *darker* value plus a 1-px rim, the way
`POPUP_SHADOW` already works (`chrome.rs:2508`). Closes **A5, A6, A7**. Verdict: `AMENDMENT` (AM-3,
AM-4). *Constraint:* FR-3.4 — this is damage on resize/scroll only, never on a keystroke.

**F-A4. Rebuild the ruler.** Ticks 1 × 6 px in `INK_DIM` at 10-column intervals and 1 × 3 px in
`RULE` at 5; numerals centred *in the band* (`l.ruler.y + (ruler_h − cell_h)/2`), aligned to the page's
left edge (column 0 = page edge, not text edge) and to the measure's right edge; margin markers (two
triangles at the margins) and an indent caret, driven by a real margin/indent model added by F-C5.
Closes **A8, A9, A14**, and gives **C5** something to mean. Verdict: `AMENDMENT` (AM-5).
*Note:* the fix must not reintroduce the earlier defect class — a tick that is invisible is worse than
no tick, so the gate is a minimum contrast between `ruler` ink and `BAND`.

**F-A5. One advance model for text and for geometry.** Pick the atlas table as the single source:
`Painter::advance_of` already answers per codepoint and style in O(1) from the metric table. Then derive
*everything* geometric from the same function — `Layout::caret_at`, `column_at`, `cells_per_row`, the
selection rectangles, the caret pen and the page's measure — by building a per-line column index once per
paint from the runs the emitter already produces, exactly as `refresh_caret_pen` already walks a line
(`session.rs:3567`). The alternative (drawing on the monospace grid) is one line of change and makes the
page look like a terminal, so it is wrong. Closes **A10**. Verdict: `CONFORMS` FR-1.3 — this is a table
lookup, not glyph measurement, and it stays O(1) per column, so no Bézier evaluation enters the keystroke
path. *Regression risk:* the measure becomes variable-width, so `page_w_for` and `measure_for` need the
average advance (they can use the `'m'` cell as a bound and the pen for the truth).

**F-A6. Sidebar: closed by default, and give it an edge.** `sidebar_open: false` in
`ChromeState::default`; a 1-px `RULE_DIM` on its right edge; a real header row; an empty-state line when
there is one document. Closes **A11, A12**. Verdict: `AMENDMENT` (AM-6).

**F-A7. A visible scrollbar.** Track in a value one step off the panel, thumb 8 px wide, a gutter
between the sheet and the rail, and hover/active states. Closes **A13**. Verdict: `AMENDMENT` (AM-7).

**F-A8. An I-beam, and the other five cursors.** Add `CreateGlyphCursor` to `holonomy-x11` (the
`cursor_font` "cursor" glyphs are the only font this may open — it is a server-side font, not a system
font path, so FR-2.1 is untouched), and switch on hover state: text over the page, pointer over
controls, resize arrows on edges. Closes **B5**. Verdict: `AMENDMENT` (AM-9) with a `CONFORMS` note on
FR-2.1.

## Tier 3 — the controls have to do what they look like they do

**F-B3. Drain and coalesce the event queue.** `while let Some(ev) = desktop.poll()` with a motion
coalescer (last position wins per batch), then one paint per batch. Closes **B3** and de-risks **B4**.
Verdict: `CONFORMS` FR-3.4 (`typing` damage per batch stays bounded) and NFR-1.

**F-B4. Wire or remove every dead control.** Either implement or delete, never leave drawn-and-inert:
* the seven title buttons — wire `History` to a no-op-with-notice only if the notice is honest, otherwise
  **remove the icons**; wire `Export` by draining `take_export_request` in `windowed.rs` (the sinks exist
  for the sealed path; the window needs a path prompt or a fixed destination, and either is one arm);
* `View > Mode` / `View > Zoom` — implement a submenu *or* flatten them into ticked rows, which needs no
  new renderer at all;
* the inert eight — `Underline` and `Highlight` need a bit in `style_flags` and a face/effect (F-C6);
  `FontSize` needs to become the real size control or the zoom pair's twin (which it nearly is already,
  `session.rs:3035–3055`); `Link`/`Comment`/`Search`/`Spellcheck`/`PaintFormat` are out of scope for a
  document editor and should be **deleted from `TOOLBAR`**, which is the fix part 46 started for
  `Comment` and never finished.
Closes **B6, B7, B10**. Verdict: `AMENDMENT` (AM-10, AM-12).

**F-C6. Give the model a paragraph style, then wire the style dropdown.** One enum
(`Normal | Title | Heading1..3 | Subheading | Quote | Caption`) stored per paragraph — a `TextIntervalSpan`
with `STYLE_HEADER` is not enough, because heading level and size are per-*paragraph* facts. The span
gains the header bit as it already does, plus a level; `LineMetrics::from_font` takes its size from the
level; `emit_body_text` draws the level's size; export already reads the bit. Then `Action::SetStyle`
applies the paragraph style at the caret or over the selection, exactly as `style_selection` already does
for bold (`session.rs:5288`). Closes **B8, B9, C6**. Verdict: `AMENDMENT` (AM-11), and it is the single
largest piece of work on this list.

**F-C2b. Soft wrap, using the map that already exists.** `WrapMap::build` over the visible lines, then a
visual-row index in `LineHeights` so `scroll_line`, `Caret::locate` and the damage rects agree — the
four call sites and their order are written down in `PROJECT.md` part 45 section H and referenced in
`session.rs:6627`. Closes **C2**, and **C3** falls out because runs are then line-sized. Verdict:
`AMENDMENT` (AM-4); must preserve FR-3.4 by damaging rows, not the page.

**F-C7. Images: a real anchor, a real picker.** Replace `ANCHOR_BYTES = U+FFFD` with a codepoint the
atlas has and no writer would type — the reserved plane is unavailable (`TEXT_RANGES` is ASCII+Latin-1),
so use a **span with zero bytes**: an image is an `AssetId` plus a `TextIntervalSpan`-like record with
`start == end` at a document offset, and the painter draws it by position rather than by finding a glyph.
That is the same shape `TableSpan` already uses (`start_byte`/`end_byte`/`cols`), and it removes U+FFFD
from every document. Then a path prompt (the `Form` machinery exists), a size/alignment control, and alt
text. Closes **C7**. Verdict: `AMENDMENT` (AM-14); `CONFORMS` FR-1.4 (the interval map already holds such
records and the text stays contiguous).

**F-C8. Tables: rows and columns as operations.** `insert_row`, `delete_row`, `insert_col`, `delete_col`,
`set_col_width`, header-row styling, and a table context in the toolbar. Closes **C8** and enables **C10**
to be exercised. Verdict: `AMENDMENT` (AM-14).

**F-C5. Tabs and indents as model facts.** A `\t` codepoint with a tab-stop list on the paragraph, plus
`indent_first` / `indent_left` on the same record; `Tab` stops inserting four spaces. Closes **C5**, feeds
**A9**. Verdict: `AMENDMENT` (AM-5/AM-11).

**F-C9. Word-wise motion.** Ctrl+Left/Right (and Shift variants), Ctrl+Backspace/Delete. `word_around`
already exists and is correct (`session.rs:2739`) — it is a keymap-plus-command change. Closes **C9**.
Verdict: `CONFORMS` (no clause forbids it; AM-13 covers command coverage).

**F-C10. Make `line_box` read `LineHeights`.** One call site; the height model already exists. Closes
**C10**. Verdict: `CONFORMS` FR-1.3 (the Fenwick tree is the geometry).

**F-C11. Report the ceilings instead of swallowing them.** Paste over `YANK_CAPACITY`, insert over the
scratch ceiling, a delete over 64 KiB, a missing glyph: all of these must raise a visible notice (with
F-A2, there is a place to put it). Closes **C4, C12, C3**'s silent half. Verdict: `AMENDMENT` (AM-2/AM-13).

**F-A9. A chrome type scale.** Two sizes (title/label) and a bold chrome face, both already in the atlas
(Bold is style 1). Closes **A15**. Verdict: `AMENDMENT` (AM-1) — and it costs nothing in FR-2.3's budget
because bold is already rasterised.

**F-A10. DPI.** On the desktop path only: read the Xft DPI or the `_X11_DPI`-style property, and choose
`--ppem` (and therefore the atlas pair) plus a whole-window integer scale from it. The sealed path keeps
1280×800 at one ratio. Closes **A16**. Verdict: `CONFORMS` FR-3.3 for the sealed path; `AMENDMENT`
(AM-8) for the window.

---

# Part 3 — Amendments Part 4 needs before any of this can claim compliance

Part 4 is a specification of the *engine*: rope, Fenwick, atlas, blit, evdev, container. It says nothing
about the document editor's appearance, its command set, or its keyboard map. Every finding marked
`AMENDMENT` above is a behaviour for which the authoritative document has no clause. The following are
the clauses that must be added (short form; each is written to be checkable).

* **AM-1 (Chrome surfaces and type scale).** The chrome is drawn from a named surface ramp
  (`panel < band < pill < pill-hover < pill-active`) where every adjacent pair differs by a stated
  minimum contrast, and from a two-size type scale with a bold chrome face. A band fill equal to the
  panel colour is a defect, not a style.
* **AM-2 (Status bar).** The status bar always shows `Ln, Col`, word count and byte count; a notice is an
  additional, time-limited region and must never replace them.
* **AM-3 (Page geometry).** The sheet is centred in the desk on both axes, with a stated minimum gutter
  (≥ 32 px at the design width) that the measure is clamped against; a sheet flush to any edge is a
  defect.
* **AM-4 (Page identity).** A page has its own height, a visible surround and a bottom margin; text that
  exceeds the measure wraps (soft) rather than being cut, and damage on wrap stays row-bounded (FR-3.4).
* **AM-5 (Ruler).** The ruler is numbered from the page's left edge, ticks and numerals meet a stated
  minimum contrast against the band, and the ruler shows the margins and the first-line indent. Tab and
  indent are document facts, not inserted spaces.
* **AM-6 (Sidebar).** The document rail is closed unless the user opened it, is separated from the desk
  by a rule, and has a defined empty state.
* **AM-7 (Scrollbar).** The rail and thumb are distinguishable from the panel, the thumb is grabbable
  (≥ 8 px), and the rail never overlaps the sheet.
* **AM-8 (DPI, desktop path only).** The developer window resolves its pixel ratio at boot and builds its
  atlas pair from it; the sealed path stays fixed at FR-3.3's panel.
* **AM-9 (Input latency and pointer feedback).** The event loop drains its queue before painting,
  coalesces motion, blocks in `poll` until the deadline, and every pointer region has a defined cursor
  and a hover state. Clipboard delivery is awaited, not timed out at a fixed 700 ms.
* **AM-10 (Affordance honesty).** No control is drawn that has no effect. A control that has no effect in
  a given build is removed from the build, not greyed. Submenu chevrons exist only where a submenu can be
  opened.
* **AM-11 (Paragraph styles).** Normal, Title, Heading 1–3, Subheading, Quote and Caption are paragraph
  attributes with a stated size and spacing for each, applied from the style dropdown and the Format
  menu, stored so that they survive a save round-trip, and readable by the exporters.
* **AM-12 (Command coverage).** The editor's command set is enumerated (a table in Part 4), and any drawn
  control must name its command or not be drawn. Inline attributes are enumerated too — underline and
  highlight need a bit in `TextIntervalSpan::style_flags` before either may appear on the toolbar.
* **AM-13 (Error policy).** Non-fatal model conditions (a refused edit, a read-only session, an oversized
  delete, an absent leaf) are counted and reported; they never terminate the session. Fatal conditions
  are enumerated.
* **AM-14 (Objects).** Images and tables are positional records in the interval map (no anchor
  codepoint), and each has a stated minimum operation set: insert, delete, resize/reorder, and — for
  tables — row/column insert and delete.

---

# Part 4 — The acceptance bar, stated as a gap list

The bar is: *after the fixes, the user writes the two reference documents in Holonomy and stops opening
Google Docs.* As of `7cd016c`, on a blank page, the following are **impossible**, independent of taste:

| The reference documents need | State today | Blocking item |
|---|---|---|
| Typed text that does not disappear at the right margin | truncated | C2 |
| A caret that lands where the click was | off by up to a third of a line | A10 |
| Non-ASCII text that can be deleted | kills the session | C1 |
| A title, a Heading 1 and a Heading 2 | unreachable | C6 |
| Bold and italic over a selection | works | — |
| Underline, highlight | no storage bit | B10 |
| A page that looks like a page | fills the canvas, off-centre | A5, A6 |
| A ruler that can be read | ticks at Δ24/255 | A8 |
| A status bar with a word count | replaced by "Saved" | A4 |
| Insert an image of one's choosing | same fixture every time | C7 |
| A table with more than 3×3 and a header row | fixed 3×3, max 8 cols | C8 |
| An export that produces a file | `Export` does nothing in the window | B6 |
| Typing without the window stalling | 800 ms whole-container fsync on the UI thread | B1 |
| Clicking without a queue behind it | one event per 16 ms tick, polling | B2, B3 |
| A pointer that says "text goes here" | default cursor | B5 |

Nine of those fifteen are Tier 1 or Tier 3 model fixes, not visual polish. That is the honest summary of
the screenshot: the window looks unready because, for half the things it draws, there is nothing behind
the drawing — and because the two things the eye checks first (a page on a desk, and text on a grid) are
both wrong by construction.

---

# Part 5 — What was fixed, and what was not

**Nothing in this section was compiled.** This sandbox has no Rust toolchain and no network route to
one (`crates.io`, `static.rust-lang.org` and the distro mirrors are all unreachable; only `github.com`,
`pypi`, `npm` and `files.pythonhosted.org` answer). Every change below was made by reading the call
sites and the gates that guard them, and is **unverified by the compiler**. The commits are `9756cbc`,
`ec690ed`, `c2fc1b6`.

## Landed

| Item | What changed | Where |
|---|---|---|
| A1, A3 | `BAND` and `CHROME` were the *same value* (`0xFFF8FAFD`), so the toolbar and status bands had no surface. `TRACK` was the same value too, and `SHADOW` was `RULE_DIM`'s. All four now differ. | `chrome.rs` `colour::` |
| A2 | The toolbar capsule was a stadium spanning the band with ~46 % of it empty and corners bigger than the buttons. It is fitted to the placed tools, radius 8, and inset from the band. | `chrome::toolbar_pill`, `TOOLBAR_PILL_RADIUS` |
| A3, A14 | **`hline` did not draw a hairline.** It repeated `─` once per cell and a box glyph rasterises to its whole cell, so each "rule" was a **20 px slab**. Deleted; rules are one-pixel fills, and there are four of them (title, menu, toolbar, ruler). | `chrome.rs` `Chrome::tree` |
| A4 | A notice *replaced* `Ln/Col/words/bytes`; `set_notice("Saved")` runs on every save, so the counters were blank for the whole session. Two regions now. | `chrome.rs` status bar |
| A5 | The sheet was flush against the rail with all slack on its right (24 px left, 200 px right), and the left gutter collapsed to **zero** once `page_w + 24 > desk`. Centred with a 16 px minimum either side. | `chrome.rs` `Layout::new` |
| A6 | `page_h` was `canvas_h - shadow` — the sheet *was* the canvas and had no bottom margin. Now a page aspect ratio, clamped so a short window still shows a whole page. | `chrome.rs` `Layout::new` |
| A7 | The shadow bands were **above and to the left** of the sheet (light source below-right), reading as an inset seam. Now below and right. | `chrome.rs` page layer |
| A8, A9 | Ticks were 1×2 px in `RULE_DIM` twelve pixels from their labels. Now 5/10-column ticks in chrome ink, numbers centred in the band, full-height margin markers inside the band. | `chrome.rs` ruler |
| A11, A12 | The sidebar defaulted open, costing the page 280 px before the user asked. Closed by default, with an edge. | `ChromeState::default`, `paint_sidebar` |
| A13 | `TRACK` was the panel colour, so the rail was invisible. | `colour::TRACK` |
| B1 | The 800 ms autosave ran at the *top* of the loop, i.e. between a keystroke and its frame. Timed saves now run on an idle wake after `tick` has painted; explicit saves still run before the read. | `windowed.rs` |
| B3 | A drag dispatched and repainted one `MotionNotify` per pixel. The run is folded to its newest position — **including from the socket**, which is where the events actually are. | `conn::take_motion_run`, `read_packet_now`, `windowed.rs` |
| B6 | Seven of eight title buttons were inert, including both pills. `place_title` draws only `Cloud` (save) and `Share` (export); a gate asserts the exact drawn set. | `widgets::TitleButton::has_action` |
| B7 | `View > Mode` and `View > Zoom` were `sub()` headers with no action — and there is no submenu renderer at all. Replaced by four real rows. **Every menu row now carries an action (0 unwired of 26).** | `menus.rs` |
| B10 | `Tool::Comment` had no annotation store and could never work; it is removed with its icon, mask and `TitleButton::Comments` (**34 → 33 icons, 23 → 22 tools, 8 → 7 inert** — the count the repo's own note pre-authorised). `FontSize` drew the only white rectangle on the chrome, shaped as a text input, with no setter behind it; drawn as a plain labelled button now. | `icons.rs`, `widgets.rs`, `chrome.rs` |
| B11 | Ctrl+V waited 700 ms and then pasted the session's **own** clipboard; the real bytes arrived later and were dropped silently. The timeout reports, empty/non-UTF-8 answers are reported, and any answer completes the request. | `windowed.rs` |
| Export | `File > Export` set `export_requested` and nothing in the windowed driver ever read it. It has a reader and writes HTML and PDF beside the vault. | `session::take_export_request`, `windowed.rs` |
| C1 | Backspace and Delete read one byte and the counters were told about one byte, so a non-ASCII deletion drifted the word count. Both take the byte range from the same UTF-8 boundary walk that performs the delete. | `editor::backspace_range`, `session.rs` |

## Not landed, and why

**These are the ones that matter for the acceptance bar, and none of them is a polish item.**

| Item | Why not |
|---|---|
| **C2** — lines past the measure are cut | Soft wrap exists (`wrap::WrapMap`) but is unwired from `emit_body_text`, and the caret, selection, `offset_of` and `column_at_pen` are **all** on the unwrapped line model. Re-pointing four interacting subsystems at a wrap map is a change that cannot be made without running the gates that measure it. |
| **C6** — Heading 1 / Heading 2 | `style_flags` is a `u16` whose `atlas_style` maps CODE/BOLD/ITALIC/HEADER only, and `HEADER` is not reachable from any drawn control. Needs a style model, an atlas face, and a toolbar/menu surface — a feature, not a fix. |
| **C7** — image insertion | `Action::InsertImage` writes the same fixture every time and one bad path writes U+FFFD into the document. Choosing a file needs a dialog and, in the sealed build, an `openat` the filter does not grant. |
| **C8** — tables | Fixed 3×3, `TableSpan::MAX_COLS = 8`, no row/column operations. Same reason as C6. |
| **B2** — the loop polls | Investigated and **the audit's diagnosis was wrong**: `Conn::ensure` `poll`s until its deadline, so the loop does not spin when idle. The lag was B3 (the queue), which is fixed. This row is retracted rather than patched. |
| **B4** — full-window damage | Needs a damage tracker over the emitters; unverifiable without the pixel gates. |
| **B5** — no I-beam cursor | Requires `CreateCursor`/`DefineCursor` on the X11 path and a cursor font in the sealed build. |
| **B8** — `Format` cannot format | Bold and Italic *are* wired and do apply spans; the row is real. What is missing is that the menu does not show which is active. |
| **B9** — the style dropdown | `Action::SetStyle` sets an index nothing reads. It needs the same style model as C6. |
| **C3, C4, C5, C9–C14** | Model ceilings (`DocRun::MAX_BYTES`, paste ceilings, tab stops, object anchors). Each needs a model change plus its gates. |
| **A10** | **Partly retracted.** The *click* already resolves x→column by the pen (`column_at_pen`), so a click and the drawn caret agree; the caret, selection and run boundaries all use `pen_of`, which is `advance_of`. The remaining mismatch is that the **ruler** is calibrated to `cell_w` while body text advances by the face — and the ruler has no access to the document's style spans, so making it pen-exact would need a document callback the chrome does not have. |
| **A15, A16** | One type size for the whole chrome; no DPI awareness. `ChromeMetrics::for_size` exists and scales, so this is a design pass, not a defect fix. |

## What the user should expect from this build

The window should no longer read as *broken*: the bands have surfaces, the rules are one pixel, the
toolbar capsule is the size of its tools, the sheet is centred on a desk with a shadow below it, the
ruler is legible and numbered, the status bar shows the counters, the rail is closed, and no drawn
control is silent except the seven tools that are recorded by name in `toolbar_wiring.rs`.

**It will still not be enough to write the two documents in.** Soft wrap (C2) and headings (C6) are the
two blockers that no amount of visual work substitutes for, and both are feature-sized. **And none of
this has been compiled** — the first `cargo test` is the real review.

---

# Part 6 — The icon set was redrawn

The first generation of `icons.rs` artwork was typed by hand, one `b"...."` row at a time, and **nothing
ever compared one icon to another.** The result is visible in the shipped build: a toolbar whose marks
read as unrelated doodles rather than as a family.

## What was actually wrong

Not the colour — measured from the user's screenshot, the toolbar ink is `(68, 71, 70)`, which is exactly
`colour::INK_CHROME` = `#444746`. The icons were never washed out. **The artwork was.**

| Icon | What was drawn | What it should be |
|---|---|---|
| `Minus` | six rows tall, 4 px of ink each: two stubs joined by a bar | one 10 px dash |
| `Plus` | 2 px cross bleeding into neighbours, widest row 6, tallest column 7 | two crossing 1 px arms |
| `Bold` | 2 px strokes that close both counters | a readable `B` |
| `TextColour` | a colour bar *plus* a filled wedge over it | an `A` over a colour bar |
| `Star` | a solid blob, 3x the ink of a normal glyph | a star |
| `Undo`/`Redo` | a circle with a detached splinter of an arrowhead | a return arrow |
| `History` | a broken ring | a clock |
| `Cloud`, `Sparkle`, `Search`, `Pen` | various blobs and combs | as named |
| `Tab` | a mark in the corner of its box | tab stop |

**Every one of these passed the gates.** `every_icon_is_sixteen_square_and_not_empty` counted ink, and
`no_icon_is_mostly_ink` bounded it at a third of the box. A dash-outline is neither empty nor over
budget. **A count of inked pixels cannot tell a picture from a smudge** — that sentence is the finding.

## How they were redrawn

The 33 masks are now **geometrically authored and rasterised**, not typed: each icon is described as
lines, arcs, circles and filled polygons, then rasterised with Bresenham (no antialiasing) to a 16x16
bitmap. Two attempts at antialiased vector downsampling were thrown away first — at 16 px a 1.15 px
stroke straddles two pixel centres at ~50 % coverage, so a threshold either loses every diagonal or
doubles it. **Bresenham was the whole fix.**

The result is still `&'static [u64]` from `.rodata` built by a `const fn`, so §2 of PROJECT.md — no SVG
runtime, no font parser for chrome — is untouched. Only the *authoring* step changed.

## Inspiration

[`storytold/wordcraft`](https://github.com/storytold/wordcraft) (MIT) draws its icon library as path data
on a 24×24 grid in `crates/render/src/icon_lib.rs` and rasterises at runtime. Holonomy cannot take the
runtime half — that is the SVG dependency §2 forbids — but the **design principles transferred
directly**: one pen weight across the whole set, round joins, and geometry described rather than
pixel-poked. Nothing was copied; the artwork here is new.

## The gates that would have caught it

Two new tests, both verified to **fail against the old masks and pass against the new** — which is the
only thing that makes a gate a gate, and the mistake the first two candidates made:

* `a_horizontal_rule_is_one_wide_row_and_not_an_outline` — `Minus` had a widest row of 4 px and 12 px of
  bleed in the rows around it. A rule is one wide row; an outline of a rule is not.
* `a_plus_crosses_the_box_in_both_axes` — `Plus` reached 6 wide and 7 tall in a 16 px box.

## Still open

`Tool::Search`, `Spellcheck`, `PaintFormat`, `FontSize`, `Underline`, `Highlight` and `Link` are drawn
and inert (the seven in `toolbar_wiring.rs`). **They now look like they work, which is worse than looking
broken** — see the note in that test.

**And none of this has been compiled.** The masks were validated by round-tripping the bytes in the file
back to pixels and re-rendering them, not by `cargo test`.
