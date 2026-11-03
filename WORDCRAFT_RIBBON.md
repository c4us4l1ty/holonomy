# The Wordcraft ribbon: what landed, what is unverified, and what is broken

This document is the honest counterpart to the feature work in this branch. It exists because the
branch changes what a person sees when they open Holonomy, and *"it looks right"* is not something
this repository accepts as evidence. Everything below is either **verified by a CI run whose log I
read**, or marked **not verified** with the reason.

The upstream reference is [`storytold/wordcraft`](https://github.com/storytold/wordcraft) at
`242e850aaad6bf38e1cd2be4122cc7cb9e5a15a8`, MIT OR Apache-2.0. Attribution and the licence are in
`third_party/wordcraft/`. No egui, no GPU renderer, no `winit` — the ribbon is drawn with Holonomy's
own surface tree, integer geometry and damage model.

---

## 1. What this branch actually does

* **A Wordcraft-shaped ribbon, drawn natively.** `crates/holonomy-render/src/ribbon.rs`: title row
  (autosave/save/undo/redo on the left, centred document title, search and lock on the right), an
  eight-tab strip (File, Home, Insert, Design, Layout, Review, View, Help), a grouped Home tab
  (Clipboard, Font, Bold/Italic, Paragraph, Styles), a collapse control, an overflow square, and a
  status band reporting page, word count, UTF-8 size, mode and zoom. Colours are Wordcraft's
  (`#3B5BDB` accent, `#292929` ribbon, `#141414` canvas, `#EEEEEE` text, and the light-mode set).
* **One layout, two consumers.** `ribbon::layout` returns the groups *and* the buttons; the painter
  and `widgets::hit` both read it, so a button cannot be drawn in one place and pressed in another.
  `tests/wordcraft_ribbon.rs` is the gate.
* **Real commands behind the buttons**, not inert lookalikes: Find, Replace, bullets and numbering,
  align left/centre/right, indent and outdent, page break, six paragraph styles, theme and autosave
  toggles, navigation. Every one of them has an `Action`; every action is routed or accounted for
  (`tests/actions.rs`).
* **Icons.** 17 new 16×16 masks for the tools Wordcraft's ribbon shows and Holonomy had no picture
  for, plus a compile-time 20 px enlargement for the desktop icon size. The sealed 512 KiB atlas and
  the "no runtime rasterisation" rule are untouched.
* **Editing behaviours** that make the ribbon mean something: list continuation on Enter (bullets and
  numbers, with renumbering), Find/Replace as a modal `Form`, alignment carried on span flags
  (`STYLE_ALIGN_CENTER`/`STYLE_ALIGN_RIGHT`, inside `STYLE_KNOWN` so a file carrying them is not
  called corrupt), and `Ctrl+Shift+I` for italic — leaving `Ctrl+I` as the image shortcut rather than
  taking a shipped accelerator away.

`set_managed_documents(true)` is what turns it on, and the desktop window already calls it.

## 2. What is verified

Three CI runs, all read from a summary the workflow committed (this sandbox cannot read Actions logs
or download artifacts; that scaffolding has been removed from the branch).

* **Formatting** passes (`cargo fmt --all -- --check`).
* **`cargo check --workspace --all-targets`** passes for both the default and
  `--features holonomy/desktop` builds. The sealed boot path is untouched.
* **Desktop tests** pass.
* **Regressions against `main`: none that I have not fixed.** `main` itself fails 17 tests; this
  branch failed 25 at its worst and 8 at the last measurement, 7 of which are `main`'s own. The 8th
  is listed in §4.

### The tests `main` already fails (17)

These are **not** caused by this branch and are worth a separate issue. Verified by running an
identical suite against `main` in the same workflow:

| test | what it says |
|---|---|
| `decorations.rs` ×6 | underline/strike/superscript/subscript draw no rule and do not move; a heading's line is not taller than a body line |
| `armed_style.rs` ×2 | arming Bold repaints 600 px outside Bold's button; 6 of 8 (bold, italic, underline) combinations are unreachable |
| `pointer.rs` ×2 | `link` is routed nowhere; the chrome-changing tool list is missing `overflow` |
| `toolbar_wiring.rs` ×2 | the overflow square opens something at 1,280 px; the inert-tool list disagrees with itself |
| `session.rs` | a rendered table border is 4 px off the 8 px cell grid |
| `zoom.rs` | the page does not scale the way the test asserts |
| `release_artifact.rs` ×2 | the default binary exceeds 2 MiB |

## 3. What is NOT verified, and why

The user compiles on their own machine; this sandbox has no `cargo`, no `rustc` and no display. So:

* **The ribbon has now been seen, and it was wrong.** The user sent a screenshot of the running
  desktop build at 125 % (the zoom `initial_desktop_zoom` picks on any screen 1,800 px or wider, so it
  is the shipping default) and the ribbon was unusable: every small-button label longer than six cells
  ran past its button into the next one (`Int`/`Ital` read as one word, `Supe x`, `Subs`), the tab
  strip read `HomeInsert`, the Styles tiles overlapped the Editing group, and the group captions were
  drawn over the ruler's number row. **All five symptoms were one defect** — see §6.
* **The corrected ribbon has now been seen too — and looking at it found three more defects that every
  gate passed** (§7). Rendering the real `Session` to a `Frame` and reviewing all seven tabs at both
  type scales and three widths is what caught them; no gate could have, because each was a number the
  layout had computed cleanly and *wrongly*. Those three are fixed and gated.
* **What is still unseen: the corrected ribbon in a real window.** The frames came from a `Frame` at
  `with_ribbon_bands`, not from X11 `GetImage`, and nothing interactive — hover, press, the overflow
  popup, an open menu, a dialog — was in a frame. The seal is intact either way: this is a
  software-rasterised frame, not a GPU path.
* **Interactive behaviours are untested on a real X11 session**: tab switching, collapse, the overflow
  popup at a narrow width, the search box, `Ctrl+F`/`Ctrl+Shift+H`, drag-select with the new
  alignment offsets, and idle autosave. The compile gate does not exercise them.
* **Replace-all is not one undo step.** `Editor` has no transaction helper, so `replace_all` issues
  one delete+insert per match; `tests/wordcraft_editing.rs` asserts the 4 steps for 2 replacements
  rather than pretending otherwise. A user who presses undo after "Replace all" undoes one
  replacement. **This is a behaviour gap, not a bug I have hidden.**
* **Lists are literal prefixes** (`- `, `1. `), not a document model. Enter continues and renumbers
  them, but they are text — as Wordcraft's are, which is why this is a design match rather than a
  shortcut.
* **Search is ASCII case-insensitive**, and Find/Replace is a blocking modal `Form` rather than a
  docked panel.
* **Alignment is span-flag based and desktop-only.** A sealed (non-managed) session ignores it.

## 4. Known defects still present

* `paint_chrome.rs::the_separators_are_drawn_from_the_table_and_not_from_the_font` and
  `the_chrome_paints_without_missing_a_single_glyph` fail on this branch and pass on `main`. Both
  demand `box_glyphs > 4` from `Chrome::tree`, and the tree emits **no box-drawing rune at all** —
  `paint_toolbar` draws separators as 1 px `Rect` fills, which is deliberate (a box glyph fills its
  whole cell). `main` passes those two tests for a reason I was not able to identify without being
  able to build; the arithmetic of the toolbar's rects, the measurement and the fill is byte-identical
  to `main`'s. Treat this as an open question, not as a fix.
* **Ribbon tab 0 ("File")** dispatches `Action::Home`, not a backstage view. Wordcraft's File tab is
  a backstage; this is a gap.
* **Theme** recolours the chrome and the ribbon but not the document's page colours.
* **Page breaks** pad the document with newlines up to the next page row — a rendering trick, not a
  pagination model.

## 5. Repo hygiene, disclosed

The CI rustfmt commits (`e56501c`, `267b95c`, `10759b4`, `2212f05`) reformatted large parts of the
repository, so the diff against `main` is much larger than the feature work — **121 files,
+8,389/−1,829**, of which the substantive changes are roughly nine files (`ribbon.rs`, `icons.rs`,
`chrome.rs`, `widgets.rs`, `menus.rs`, `session.rs`, `windowed.rs`, `keymap.rs`, the two new test
files). `git diff -w` confirms the rest is whitespace and line breaks.

Every temporary instrument — the `contents: write` permission, the auto-format-and-push step, the
`ci-*.log` summaries, the baseline job — has been removed from the branch, and `main` carries no
workflow at all.

## 6. The usability pass: one measurement, five symptoms

**Reported as a screenshot, not a failing test, and it is worth writing down why no gate caught it.**

The ribbon measured every string in a hardcoded `let advance = 8` — the cell width at the 16 ppem
design. The painter never read that number: it advances by `atlas.metric(c, ..).advance_x`, the face's
own metric, which is **12** at the 125 % the desktop actually boots into. So the ribbon drew 12 px
glyphs into boxes measured at 8 px each, and every reported symptom follows from that one ratio:

| Symptom in the screenshot | The arithmetic |
|---|---|
| `Int` / `Ital` run together, `Supe x`, `Subs` clipped | a 54 px label area believed it held 6 cells and holds **4**; the `...` guard never fired, so the *font's* renderer clipped at the button edge instead |
| `Stri...`, `Clea...`, `Font...` | the elision fired only after the label was already wider than the box, so it elided to a string still too wide |
| `HomeInsert` with no gap | tabs were placed `len * 8 + 22` apart with **no gap at all**; at 12 px `Home` + `Insert` is 120 px of ink in 124 px of box |
| Styles tiles in the Editing group, blue tile over `Ed` | the Font group's last column advanced by its *own* width while the group advanced by a different sum — the same two-expressions-for-one-number defect, again |
| `Paragraph` printed over `232 234 236` on the ruler | the caption strip was a literal 20 px box under a **30 px** line, at `rect.bottom() - 20` — so its bottom four rows were drawn in the next band |
| `AutoSa` with the toggle on top of the `v` | AutoSave's label box was a literal 76 px in a 112 px item and the switch was drawn at `x + 78` |
| `Find in docun` and a clipped `Ctrl+F` | the search field was a fixed 244 px for a 24-cell placeholder (288 px at a 12 px cell) |

**What changed.** Every width is now [`ribbon::text_w`] of the string the painter will draw — the Font
button's included, because it draws the face *name* and `Inter` and `JetBrains Mono` are 5 and 14
cells. [`ribbon::cells`] is the only measurement of text in the module and it counts **characters**,
not bytes. The row pitch and the caption strip are a line of type plus their air rather than `29` and
`20`. And because the honest widths reveal that Home's five groups need **2,088 px at 16 ppem and 2,664
at 125 %** against a 1,552 px limit, the ribbon now gives the small buttons' **labels** up from the
right until the tab fits — Word's *Collapse the Group*, Wordcraft's `ribbon_fit::COMPACT` — keeping the
icon and the full name. A button with no icon never loses its label, and no command is ever hidden for
want of room at the width the product opens.

The five bands also had to move: `Session::resize` and `apply_type_scale` overwrote the type-scaled
heights with `title_h: 76, menu_h: 32, toolbar_h: 112, …` **after** `with_type_scale` had scaled them,
so at 125 % a 30 px line sat in a ribbon built for 25. Both callers now go through
`ChromeMetrics::with_ribbon_bands`, which scales the design's ribbon heights by the measured pitch and
then raises each band to at least one line.

### The gates this pass added (`crates/holonomy/tests/wordcraft_ribbon.rs`)

Each runs at **both** 16 ppem and 20 ppem, because 16 is the only scale the old gates knew and 20 is
the scale the product boots into:

| Gate | The defect it would have caught |
|---|---|
| `every_ribbon_label_fits_inside_its_button` | the `Italic`/`Ital` overflow, as an inequality on ink vs. usable width |
| `no_two_ribbon_buttons_overlap` | the Font-group overlap, geometrically rather than as a wrong click |
| `every_group_caption_is_inside_the_ribbon` | the caption drawn into the ruler |
| `ribbon_tabs_do_not_touch_and_never_clip_their_labels` | `HomeInsert` |
| `the_quick_access_row_fits_its_labels` | `AutoSa` under the toggle |
| `the_home_tab_fits_the_window_the_product_opens` | the Styles/Editing collision, and that fitting it never hides a command |

**Still not verified:** that the corrected ribbon *looks* right. The gates assert geometry, not
appearance, and there is still no display attached to this sandbox.

## 7. The ribbon was looked at, and it was still wrong

**§3 said the corrected ribbon had never been seen. It has now been seen, and looking at it found
three defects that every gate in §6 passed.** That is the honest lesson of this section: a gate that
asserts a *number* the layout computed cannot catch a defect that is a number the layout computed
*wrongly*. The frames were produced by rendering the real `Session` to a `Frame` and dumping PPM —
no display and no GPU, so the seal is intact — and the review was a contact sheet of all seven tabs
at 16 ppem and 20 ppem, three window widths each.

| What the frame showed | What the numbers said |
|---|---|
| `Illustr…` — the *group caption*, not a button label | the `Illustrations` group was 92 px wide and its caption is 104 px of type. The group's width was the sum of its **buttons**, so a group whose name is longer than its content centred that name in a box too small to hold it |
| `Paste` printed on `Clipboard`, `Page break` on `Pages`, `Normal` on `Document formatting` | a large button stacks a 20 px icon over its label and its label box ran to the **bottom** of the group, where the caption's box began: two strips of 20 px type in a 70 px group with nothing between them. `every_group_caption_is_inside_the_ribbon` passed throughout, because both rects *were* inside the group |
| only the top strip of small buttons visible on Home at 16 ppem | the band's air was two literals, `- 10` and `+ 7` — **17 px of a 71 px band**, while a caption strip plus two rows needs 69. The second row was clamped away and the caption took the last 20 |

**What changed.**

* A group is `max(buttons, caption)` wide. That is the whole rule, and it is asserted per group rather
  than spot-checked on the caption that was reported.
* `CAPTION_GAP` reserves air between a button's content box and the caption's strip. It is read by
  `layout` and by `paint` — one constant, both readers, which is the discipline the rest of this file
  arrived at the hard way.
* The band's inset is **derived from the tallest group that can go in the band** rather than written
  down: `inset = (band − (3 + 2·row_h + CAPTION_GAP + caption_h)) / 2`. At cell 25 it reproduces the
  shipped 96 px group in a 112 px band exactly, and it now leaves room at *every* cell height from 12
  to 40 — where the old literals did not even leave room at 14.

**Two new gates**, plus one that is a sweep rather than two points:

| Gate | The defect it would have caught |
|---|---|
| `every_group_caption_fits_inside_its_group` | `Illustr…` |
| `no_button_label_is_drawn_over_its_group_caption` | `Paste`/`Clipboard`, as **ink** rather than rects — the rects were inside the group the whole time; what overlapped was where the painter puts a centred line in the box it was handed |
| `every_type_scale_leaves_room_for_a_two_row_group` | the band being too short, over cell heights 12..=40. **16 and 20 both happened to fit and 14 did not** — two sample points are not a sweep |

**The frames are in the repository**: `docs/images/ribbon-tabs-16ppem.png` is all seven tabs at 16 ppem
and 1,600 px, rendered from the real `Session` at the commit this section describes. It is the artefact
to check §7 against, and it is why "looked at" now means something specific.

**What is still not verified.** The ribbon has been *looked at* at 16 and 20 ppem and three widths by
an agent, from dumped pixels, not by the person who reported the screenshot. Hover, press, the overflow
popup, the open menus and the dialog paths were not in any frame. And the frames were rendered at
`ChromeMetrics::with_ribbon_bands`, not through a window — so anything that depends on a real X11
`GetImage` (colour depth, the shadow, the compositor) is still unseen.
