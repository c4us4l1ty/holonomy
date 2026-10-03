/**
 * The formatting toolbar: a list of items, what the editor currently has active, and a
 * function that turns the two into DOM.
 *
 * # Why this module holds no framework
 *
 * There is no React in this project, and a toolbar is a row of buttons. What a framework
 * would contribute is a diffing algorithm and a runtime, and what it would cost is the
 * ability to say precisely what a button is -- which is the part that has to be right. So
 * the DOM here is `createElement` and `addEventListener`, and everything that decides
 * *anything* is a pure function over a plain description of the editor.
 *
 * # The split, and why it is drawn where it is
 *
 * Every decision is a **value**: {@link toolbarState} says what is on, {@link buttonSpec} says
 * what a button's attributes and behaviour are, {@link toolbarLabel} says what it is called.
 * The DOM functions only apply those values. `renderToolbar` is `createElement` plus a loop.
 *
 * That line was drawn by measurement rather than taste. The first version decided inside
 * `renderItem` -- `if (item.kind === 'toggle') button.setAttribute('aria-pressed', …)` --
 * and a mutation run found that **eleven** deliberate breaks to the rendering produced no test
 * failure at all: dropping `aria-pressed`, dropping `disabled`, dropping `aria-label`,
 * inverting the pressed state, letting a disabled button run its command. Every one of them was
 * a correct-looking `setAttribute` call, and there is no DOM in Node to observe any of them.
 *
 * The alternative considered was a fake DOM, and rejected for the reason `core/assets.ts`
 * records: stubs raced between tests and produced failures that looked like product bugs. A
 * source-level grep for `onclick` -- which this module's tests do have -- cannot tell whether
 * `aria-pressed` is ever set to the right value, and a check that only looks for a token's
 * presence is a check on the text rather than on the behaviour.
 *
 * So the decisions moved out, and the mutation run now catches all eleven.
 *
 * # Why an unsupported item is disabled and not hidden
 *
 * **Because a toolbar that changes shape between sections reads as a bug.** Sections are
 * separate editors with separate node types available, and a node that is absent from this
 * section's schema has no command on this editor. Hiding its button makes the toolbar a
 * different width and a different arrangement depending on where the caret is, so the user's
 * eye has to re-find Bold every time they cross a section boundary -- and the disappearance is
 * silent, so there is nothing to complain about precisely. Disabled says "not available
 * here", which is both true and actionable.
 *
 * The rejection was a tooltip-only version (leave the button, explain on hover): a tooltip is
 * unavailable to a keyboard user and to anyone not hovering, and it leaves the button looking
 * live. The rejection was also rendering the button normally and reporting unavailable only in
 * the status bar, which is the same problem with more machinery.
 *
 * # Why `aria-pressed` is set on toggles and `disabled` on both
 *
 * `aria-pressed` is what tells a screen reader the button is currently on, and it is only
 * meaningful for a toggle -- an action that inserts a horizontal rule has no pressed state
 * and setting one would be a lie. The `disabled` *attribute* is separate from it and is what
 * takes the button out of the tab order, which is correct: a disabled control should not be
 * reachable, and `aria-disabled` alone would leave a control focusable that does nothing.
 *
 * # Why the icons are inline path strings
 *
 * Because an icon font or an icon library is a dependency, a request, a licence question and
 * a class of failure where the icon silently does not load and the button is a blank box. A
 * path in a `<svg>` cannot fail to load. They are written as `d` attributes on a single
 * `<path>` in a 24x24 box, which is the convention every icon set uses and the one that
 * scales without a second asset.
 */

// -- the shortcut registry, as far as this module is concerned ---------------

/**
 * A declared chord, matching the shape `parseChord` returns in `shortcuts.ts`.
 *
 * # Why the shape is repeated instead of imported
 *
 * Because a *value* import of another source module cannot be loaded under
 * `node --experimental-strip-types`, which resolves exactly what is written and does not map
 * the `.js` extension the bundler wants back to a TypeScript source. Every cross-module
 * *type* import in this repo is erased before that matters, which is why `lifecycle.ts` can
 * import `registry.js` as a type and be tested in Node.
 *
 * A value import would therefore make this module untestable in Node, and the alternative --
 * the toolbar keeping its own copy of the chord grammar, or of the binding table -- is the
 * duplication `DOCTRINE.md` §8 is about. So the grammar stays in `shortcuts.ts` and this
 * module receives the two facts it needs through {@link ShortcutLookup}: the chord written for
 * a command, and that chord rendered for the platform. Both come from the registry's own
 * table, so the tooltip and the resolver still cannot disagree.
 */
export interface Chord {
  readonly key: string
  readonly mods: readonly string[]
}

/** How a command's chord is looked up and printed. Supplied by the app; see above. */
export interface ShortcutLookup {
  /**
   * The written chord for a *shortcut command name*, or null when it has no default binding.
   *
   * The name is the one in {@link ToolbarItem.shortcut} where present, else the item's Tiptap
   * `command` with a leading `toggle` removed -- which is what makes `bold` and `toggleBold`
   * the same binding without either module owning a table of both.
   */
  chordFor(command: string): string | null
  /** A written chord rendered for the platform, e.g. `Ctrl+B` or `⌘B`. */
  format(written: string): string
}

/**
 * The name an item's shortcut is registered under.
 *
 * # Why the `toggle` prefix is stripped rather than the item saying `shortcut: 'bold'`
 *
 * Because it is a rule with no exceptions in the table below, and writing it on all eleven mark
 * and block toggles would be eleven chances to typo it -- a typo produces a button whose
 * tooltip has no shortcut in it, and nothing says so. The three heading items override it,
 * because all three are the same Tiptap command and nothing in that name distinguishes them.
 */
export function shortcutName(item: ToolbarItem): string {
  if (item.shortcut) return item.shortcut
  const stripped = item.command.replace(/^toggle/, '')
  // Lowercased for the same reason `activeName` lowercases: the registry's command names are
  // lower case (`bold`, not `Bold`), and a capitalised lookup finds nothing -- which is a
  // button with no shortcut in its tooltip and no error anywhere.
  return stripped.charAt(0).toLowerCase() + stripped.slice(1)
}

/** A lookup that knows nothing, so no item claims a shortcut. The default. */
export const NO_SHORTCUTS: ShortcutLookup = {
  chordFor: () => null,
  format: written => written,
}

/** The primary modifier a label is spelled for. `'meta'` is a Mac, `'ctrl'` is everything else. */
export type PrimaryModifier = 'meta' | 'ctrl'

// -- the editor, as far as this module is concerned ---------------------------

/**
 * The slice of a Tiptap `Editor` this module reads.
 *
 * # Why the interface is declared here rather than imported
 *
 * Because the real `Editor` type drags in the whole of Tiptap, and this module needs exactly
 * two of its members -- `isActive` and `isDestroyed`, plus the command table reached through
 * {@link hasCommand}. Declaring them makes the dependency visible: if a future Tiptap changes
 * one of them, this is the line that stops compiling rather than a toolbar that quietly stops
 * updating. A real `Editor` satisfies it structurally, so nothing is cast.
 */
export interface EditorLike {
  /** Whether the selection currently carries a mark or sits in a node of this kind. */
  isActive(name: string, attrs?: Record<string, unknown>): boolean
}

/**
 * Whether an editor can still be used, read through its command table.
 *
 * # Why not `editor.isDestroyed`
 *
 * Because in Tiptap 3.30.3 it is `true` for a **live, headless** editor. The getter is
 * `editorView?.isDestroyed ?? true`, and `editorView` is null when the editor was built
 * without a DOM -- which is every editor in the Node suite and every editor mounted off-screen
 * by the registry. So a liveness check written against it reports "destroyed" for a perfectly
 * working editor, and every button in the toolbar renders disabled.
 *
 * That failure is invisible in the browser, where `editorView` exists and the getter is
 * correct. It is exactly the shape `DOCTRINE.md` §1 describes: the harness and the product
 * disagree about a shared fact, and only one of them is wrong.
 *
 * Reading `editor.commands` is the signal that works in both. `destroy()` sets
 * `commandManager = null` and the `commands` getter dereferences it, so a destroyed editor
 * throws while a live one does not -- with or without a DOM, because it is the same code path.
 */
function isUsable(editor: EditorLike | null | undefined): editor is EditorLike {
  if (!editor) return false
  try {
    return !!(editor as unknown as { commands?: unknown }).commands
  } catch {
    return false
  }
}

// -- items --------------------------------------------------------------------

/** Whether an item reflects editor state or performs an action. */
export type ItemKind = 'toggle' | 'action'

/** One button. */
export interface ToolbarItem {
  /** Stable id, used for tests and for `aria-*` wiring. Not shown. */
  readonly id: string
  /** The accessible name and the tooltip's leading word. */
  readonly label: string
  /**
   * An SVG path `d` string, drawn in a 24x24 box.
   *
   * One path rather than several because every icon here is a single glyph; a multi-path icon
   * would need a different field and a different renderer for no present benefit.
   */
  readonly icon: string
  /** The Tiptap command this invokes. */
  readonly command: string
  /** Fixed arguments for the command, for the commands that need them. */
  readonly args?: readonly unknown[]
  /** Whether the button reflects state, and so gets `aria-pressed`. */
  readonly kind: ItemKind
  /**
   * Arguments the command needs that the toolbar cannot know.
   *
   * Set for anything opening a dialog -- an equation, a table. A button that needs a value it
   * cannot have is not a button that silently does nothing: it becomes an action the app
   * handles, and this flag is how the app knows which ones those are.
   */
  readonly needsInput?: boolean
  /**
   * The command name this item's shortcut is registered under, when it differs from
   * {@link command}.
   *
   * # Why there are two names, and why only three items need the second
   *
   * Because Tiptap's command names and the application's command names are different
   * namespaces, and conflating them is the bug. The shortcut registry (see
   * `EDITOR_BINDINGS`) is keyed `bold`, `italic`, `bulletList`; Tiptap's commands are
   * `toggleBold`, `toggleItalic`, `toggleBulletList`. The `toggle` prefix is derivable, so it
   * is not written down.
   *
   * The heading items are the exception and the reason this field exists rather than a
   * derivation: all three are the *same* Tiptap command, `setHeading`, distinguished only by
   * the `level` attribute. Nothing in the command name says which of `heading1`, `heading2` or
   * `heading3` it is, so it is stated. Guessing it from the arguments would be a mapping that
   * silently produces `Ctrl+Alt+1` on H2 if the argument shape ever changes -- and a wrong
   * tooltip is exactly the kind of error nothing else reports.
   */
  readonly shortcut?: string
  /** Whether the command is in scope inside an editor, which is where this toolbar lives. */
  readonly requires?: 'inline' | 'block' | 'any'
}

/**
 * The items, in the order they appear.
 *
 * # Why the order is explicit here and not derived
 *
 * Because the order is a design decision, not a consequence of anything. Marks, then headings,
 * then blocks, then inserts: the groups a user reaches for in that order, and separators
 * between them come from {@link ITEM_GROUPS}.
 *
 * # Why each command name is the Tiptap one
 *
 * Because a rename here is a rename of a Tiptap command, and the item would then render as
 * permanently disabled rather than as an error. `test/toolbar.ts` checks every name against a
 * real editor built the way `main.ts` builds one, which is the only place that knowledge can
 * be checked without a browser.
 */
export const TOOLBAR_ITEMS: readonly ToolbarItem[] = [
  // -- inline marks
  { id: 'bold', label: 'Bold', icon: 'M7 5h6a3.5 3.5 0 0 1 0 7H7zM7 12h7a3.5 3.5 0 0 1 0 7H7z', command: 'toggleBold', kind: 'toggle' },
  { id: 'italic', label: 'Italic', icon: 'M17 5h-6M13 19H7M14 5l-4 14', command: 'toggleItalic', kind: 'toggle' },
  { id: 'underline', label: 'Underline', icon: 'M7 4v6a5 5 0 0 0 10 0V4M5 20h14', command: 'toggleUnderline', kind: 'toggle' },
  { id: 'strike', label: 'Strikethrough', icon: 'M16 5H10a3 3 0 0 0 0 6h4a3 3 0 0 1 0 6H8M4 12h16', command: 'toggleStrike', kind: 'toggle' },
  { id: 'code', label: 'Inline code', icon: 'M9 8l-4 4 4 4M15 8l4 4-4 4', command: 'toggleCode', kind: 'toggle' },
  { id: 'highlight', label: 'Highlight', icon: 'M13 4l7 7-8 8H6l-3-3zM4 20h16', command: 'toggleHighlight', kind: 'toggle' },

  // -- block structure
  // All three are `setHeading`; `shortcut` is what tells them apart. See the field's note.
  { id: 'h1', label: 'Heading 1', icon: 'M4 6v12M11 6v12M4 12h7M15 10l3-4v12', command: 'setHeading', args: [{ level: 1 }], kind: 'toggle', shortcut: 'heading1' },
  { id: 'h2', label: 'Heading 2', icon: 'M4 6v12M11 6v12M4 12h7M15 9.5a2.5 2.5 0 0 1 5 0c0 2-5 3-5 6h5', command: 'setHeading', args: [{ level: 2 }], kind: 'toggle', shortcut: 'heading2' },
  { id: 'h3', label: 'Heading 3', icon: 'M4 6v12M11 6v12M4 12h7M15 9.5a2.5 2.5 0 0 1 4.5 1.5c0 1.5-4.5 1.5-4.5 3.5a2.5 2.5 0 0 0 4.5 1.5', command: 'setHeading', args: [{ level: 3 }], kind: 'toggle', shortcut: 'heading3' },
  { id: 'bulletList', label: 'Bullet list', icon: 'M9 6h11M9 12h11M9 18h11M4.5 6h.01M4.5 12h.01M4.5 18h.01', command: 'toggleBulletList', kind: 'toggle' },
  { id: 'orderedList', label: 'Ordered list', icon: 'M10 6h10M10 12h10M10 18h10M4 6h1v4M3 15h2l-2 3h2', command: 'toggleOrderedList', kind: 'toggle' },
  { id: 'blockquote', label: 'Blockquote', icon: 'M4 5v14M20 5v14M4 12h16', command: 'toggleBlockquote', kind: 'toggle' },
  { id: 'codeBlock', label: 'Code block', icon: 'M4 5h16v14H4zM9 10l-2 2 2 2M15 10l2 2-2 2', command: 'toggleCodeBlock', kind: 'toggle' },

  // -- inserts. These need a value the toolbar cannot have, so they are app-handled.
  { id: 'inlineMath', label: 'Inline equation', icon: 'M5 19l6-14M13 5h6M13 12h4M13 19h6', command: 'setInlineMath', kind: 'action', needsInput: true },
  { id: 'blockMath', label: 'Block equation', icon: 'M4 5h16M4 12h16M4 19h16M8 7v10M16 7v10', command: 'setBlockMath', kind: 'action', needsInput: true },
  { id: 'table', label: 'Table', icon: 'M3 5h18v14H3zM3 10h18M3 15h18M9 5v14M15 5v14', command: 'insertTable', args: [{ rows: 3, cols: 3, withHeaderRow: true }], kind: 'action' },
  { id: 'horizontalRule', label: 'Horizontal rule', icon: 'M3 12h18M6 7h12M6 17h12', command: 'setHorizontalRule', kind: 'action' },
]

/** Where the separators go, by item id. Drives `renderToolbar`'s `aria` grouping. */
export const ITEM_GROUPS: readonly (readonly string[])[] = [
  ['bold', 'italic', 'underline', 'strike', 'code', 'highlight'],
  ['h1', 'h2', 'h3'],
  ['bulletList', 'orderedList', 'blockquote', 'codeBlock'],
  ['inlineMath', 'blockMath', 'table', 'horizontalRule'],
]

// -- state --------------------------------------------------------------------

/** What the editor has active right now, per item id. */
export interface ToolbarState {
  /** Toggles that are on. An action is never in here: it has no on state. */
  readonly active: ReadonlySet<string>
  /**
   * Items whose command this editor does not have at all.
   *
   * Distinct from "not applicable right now": an item in this set is *disabled*, because
   * pressing it could never do anything. See the module header on why that is not a hide.
   */
  readonly unsupported: ReadonlySet<string>
}

/** The state of a toolbar over an editor that is not there, which is most of the time. */
export const EMPTY_STATE: ToolbarState = { active: new Set(), unsupported: new Set(TOOLBAR_ITEMS.map(i => i.id)) }

/**
 * Which items are active, and which the editor cannot do at all.
 *
 * # Why `unsupported` is a `typeof` check and not `canRun`
 *
 * Because they are different questions and only one of them means "disable this button".
 * `canRun` is a *selection* question: Bold cannot run in an empty selection on some node
 * types, and greying Bold out every time the user has no selection selected would be worse
 * than useless -- they are about to type, and Bold is what they will want. So `canRun` is not
 * consulted for availability.
 *
 * Whether a command exists at all is a different and much more stable fact, and it is checked
 * the only way it can be honestly checked: by looking for the command on the editor. The
 * interface this module declares does not expose the command table directly, so this goes
 * through {@link hasCommand}, which the caller can narrow. In practice the check is
 * `typeof editor.commands[name] === 'function'`, and the reason it is injected rather than
 * called here is in {@link hasCommand}.
 *
 * # Why the editor being absent marks everything unsupported
 *
 * Because a toolbar over a destroyed or absent editor is one whose every button would do
 * nothing. Marking them disabled is the honest rendering, and it means the caller does not
 * need a separate "is the editor ready" branch that can disagree with the buttons.
 *
 * # Why this is pure with respect to the editor
 *
 * It reads and never dispatches, so calling it is safe on every transaction. The test calls
 * it against a real Tiptap editor and asserts the document is byte-identical afterwards, which
 * is the only way to catch a `toggleX` that slips in where an `isActive` was meant.
 */
export function toolbarState(editor: EditorLike | null | undefined): ToolbarState {
  // Narrowed once, so the loops below work on a non-nullable editor and the compiler can see
  // that `isActive` and `supports` never receive one.
  if (!isUsable(editor)) return EMPTY_STATE
  const live = editor
  const active = new Set<string>()
  const unsupported = new Set<string>()
  for (const item of TOOLBAR_ITEMS) {
    if (item.kind !== 'toggle') continue
    // Whether a command *could* run right now is deliberately not consulted; see the note
    // above. A toggle that exists is never disabled for being inapplicable at this instant.
    if (isActive(item, live)) active.add(item.id)
  }
  for (const item of TOOLBAR_ITEMS) {
    if (!supports(item, live)) unsupported.add(item.id)
  }
  return { active, unsupported }
}

/** Whether a toggle's mark or node is currently on. */
function isActive(item: ToolbarItem, editor: EditorLike): boolean {
  // A heading needs its level, or "Heading 1" would be pressed in a Heading 2. The attrs are
  // the item's own command arguments, so there is no second place to keep a level.
  const attrs = item.command === 'setHeading' ? (item.args?.[0] as Record<string, unknown> | undefined) : undefined
  return editor.isActive(activeName(item), attrs)
}

/**
 * The name `isActive` is asked about.
 *
 * The command with `toggle` or `set` removed and the first letter lowercased: `toggleBold`
 * asks about `bold` and `setHeading` about `heading`, because a ProseMirror mark or node is
 * named in lower case and `isActive('Bold')` silently answers `false` forever -- a button that
 * is never pressed, with no error anywhere.
 *
 * # Why there is no `setHeading` special case
 *
 * Because there does not need to be one, and having one was worse than not. The mutation run
 * caught it: removing the `if (item.command === 'setHeading') return 'heading'` branch changed
 * no test result, because `'setHeading'.replace(/^set/, '')` is `Heading` and lower-casing the
 * first letter already gives `heading`. A branch that cannot change an answer is one more thing
 * to read and to keep true, so it is deleted rather than asserted. The heading *level* still
 * needs saying, and it is in {@link isActive}'s attributes, not here.
 *
 * The mapping is a function rather than a field on every item for the same reason: a renamed
 * Tiptap command would then need the active name updated too, and forgetting produces a button
 * that is never pressed.
 */
export function activeName(item: ToolbarItem): string {
  // Exported, rather than private, because this is the step between a Tiptap command name and
  // a schema name, and a wrong answer produces a button that is never pressed. Asserted
  // directly in `test/toolbar.ts` for every item, since `toolbarState` only reports `false`
  // for a wrong name and a false negative is indistinguishable from an off toggle.
  const stripped = item.command.replace(/^toggle/, '').replace(/^set/, '')
  return stripped.charAt(0).toLowerCase() + stripped.slice(1)
}

/**
 * Whether this editor has the command at all.
 *
 * Injected rather than called, because {@link EditorLike} deliberately does not expose the
 * command table -- and because a real Tiptap `Editor` does not expose it in a way this module
 * can rely on across versions, while `editor.commands[name]` does. The caller's adapter is
 * two lines, and the alternative -- widening the interface to a whole `Editor` -- would put
 * Tiptap's whole surface into this module's type.
 */
export function hasCommand(editor: EditorLike | null | undefined, command: string): boolean {
  // `isUsable` reads `commands` too, so the try is here rather than at the call site: a
  // destroyed editor throws on the *getter*, not on a missing key, and reading the table
  // inside the guard is what turns that into a `false`.
  if (!isUsable(editor)) return false
  const table = (editor as unknown as { commands?: Record<string, unknown> }).commands
  return !!table && typeof table[command] === 'function'
}

/** Whether an item's command exists on this editor. */
function supports(item: ToolbarItem, editor: EditorLike): boolean {
  return hasCommand(editor, item.command)
}

// -- labels -------------------------------------------------------------------

/**
 * A button's accessible name and tooltip.
 *
 * # Why the keybinding is in the tooltip and not the name
 *
 * Because the name is what a screen reader announces and what a voice-control user says.
 * "Bold Ctrl+B" is not a name; "Bold" is. The binding goes in `title`, which is where a mouse
 * user looks.
 *
 * # Why the chord is looked up rather than written here
 *
 * Because the shortcut registry's table is the one the resolver actually uses, and a second
 * list of keybindings in this module is a table that can silently disagree with it -- bold
 * showing `Ctrl+B` after someone rebinds it to `Ctrl+Shift+B`, with no error anywhere. So the
 * chord is asked for and rendered through {@link ShortcutLookup}, which the app builds from
 * `shortcuts.ts`. A lookup that knows nothing is the default, and then no item claims a
 * shortcut rather than claiming a wrong one.
 */
export function toolbarLabel(
  item: ToolbarItem,
  shortcuts: ShortcutLookup = NO_SHORTCUTS,
): { name: string; title: string } {
  const name = item.label
  const written = shortcuts.chordFor(shortcutName(item))
  // No chord, or a chord the app cannot render: the plain name. Showing `Bold (mod+b)` to a
  // user would be worse than showing nothing, and it is what a lookup that has not been wired
  // up yet would otherwise produce.
  if (!written) return { name, title: name }
  const keys = shortcuts.format(written)
  if (!keys) return { name, title: name }
  return { name, title: `${name} (${keys})` }
}

// -- rendering ----------------------------------------------------------------

/** What a click on a button means. */
export type ToolbarHandlers = {
  /** The command to run, with the item's own arguments. */
  run: (command: string, args: readonly unknown[], item: ToolbarItem) => void
  /**
   * Called for an item marked `needsInput`, before anything is inserted.
   *
   * Separate from `run` because the value is the app's to collect -- a dialog, a prompt -- and
   * a toolbar that opened one itself would own a modal it has no business owning.
   */
  requestInput?: (item: ToolbarItem, done: (value: string) => void) => void
}

/**
 * The toolbar root's attributes, and its groups'.
 *
 * Constants rather than inline `setAttribute` calls for the reason {@link ButtonSpec} exists:
 * a decision written as a call at the call site cannot be asserted without a DOM. As data they
 * are a value, and `test/toolbar.ts` asserts every key.
 *
 * `role="toolbar"` because the toolbar is a group of controls and the group is what a screen
 * reader's toolbar navigation moves between -- without it the buttons are a flat list of the
 * whole document, which is not what they are. `aria-orientation` is horizontal, which is what
 * the arrow-key pattern that role implies expects.
 *
 * Roving focus -- one tab stop, arrows to move within -- is the rest of the APG pattern and is
 * **not** implemented here. It is a behaviour rather than a name, it needs the caller to own
 * key handling, and half-building it would be worse than leaving it out. The role is claimed
 * because it is true of what is rendered; the arrow keys are recorded in the report as owed.
 */
export const TOOLBAR_ROOT_ATTRIBUTES: Readonly<Record<string, string>> = {
  class: 'holo-toolbar',
  role: 'toolbar',
  'aria-label': 'Formatting',
  'aria-orientation': 'horizontal',
}

/** Each group's attributes: a labelled group of related controls, between separators. */
export const GROUP_ATTRIBUTES: Readonly<Record<string, string>> = {
  class: 'holo-toolbar__group',
  role: 'group',
}

/**
 * Build the toolbar.
 *
 * # Why this is thin and the state is not
 *
 * Everything decided -- pressed, disabled, labelled -- arrives from {@link toolbarState} and
 * {@link toolbarLabel}. What is left here is `createElement` and `addEventListener`, and that
 * is deliberate: the decisions are the part that can be wrong in a way a user would call a
 * bug, and they are the part that can be tested without a browser.
 *
 * # Why `type="button"`
 *
 * Because this toolbar is going to sit in a document, and a `<button>` with no type inside a
 * form is a submit button. There is no form here, and the toolbar must not depend on that
 * staying true.
 *
 * # Why the handler is added, not assigned
 *
 * `onclick` is a string attribute, so it does not exist under a CSP that forbids
 * `unsafe-inline`, it is compiled at click time, and it cannot be removed by
 * `removeEventListener` -- which matters because the toolbar is rebuilt as sections mount.
 * `addEventListener` is removable and CSP-clean. The browser suite asserts no item ends up
 * with an inline handler.
 *
 * # Why `mousedown` is prevented, and `click` is the handler
 *
 * Because a click on a button moves focus to it, and a ProseMirror editor that loses focus
 * mid-command applies the command to the wrong place -- or not at all, since the selection is
 * gone. Preventing `mousedown`'s default keeps focus in the editor, and the `click` still
 * fires. This is the single most common way a hand-built editor toolbar loses the caret, and
 * the alternative (`focus()` on the editor inside the handler) is a guess about whether the
 * editor is still mounted.
 *
 * # Why an unavailable item still gets a listener
 *
 * A disabled button does not fire events, so the listener would be unreachable anyway. It is
 * attached to all of them so the two paths cannot diverge, and the `disabled` attribute is
 * what actually prevents the activation.
 */
export function renderToolbar(
  state: ToolbarState,
  handlers: ToolbarHandlers,
  shortcuts: ShortcutLookup = NO_SHORTCUTS,
): HTMLElement {
  const root = document.createElement('div')
  for (const [name, value] of Object.entries(TOOLBAR_ROOT_ATTRIBUTES)) root.setAttribute(name, value)
  for (const items of toolbarGroups()) {
    const groupEl = document.createElement('div')
    for (const [name, value] of Object.entries(GROUP_ATTRIBUTES)) groupEl.setAttribute(name, value)
    for (const item of items) groupEl.appendChild(renderItem(item, state, handlers, shortcuts))
    root.appendChild(groupEl)
  }
  return root
}

/**
 * The items, grouped as they will be rendered.
 *
 * # Why the grouping is resolved here and not inside `renderToolbar`
 *
 * Because the alternative was a `find` and a `throw` inside a DOM-building loop, and the
 * mutation run showed what that costs: replacing the `throw` with `continue` changed **no** test
 * result, because nothing in the shipped tables trips it and the renderer needs a DOM nothing
 * in Node can provide. A guard that no test can reach is a comment.
 *
 * So the resolution is a function returning a value, and `test/toolbar.ts` calls it with a
 * group table that names an id which is not an item. That makes the throw observable: the
 * table-level test asserts the shipped tables agree, and this one asserts the *consequence* of
 * them disagreeing. Both directions are covered, which is what makes the guard real rather
 * than decorative.
 */
export function toolbarGroups(groups: readonly (readonly string[])[] = ITEM_GROUPS): ToolbarItem[][] {
  return groups.map(group =>
    group.map(id => {
      const item = TOOLBAR_ITEMS.find(i => i.id === id)
      // Loud rather than skipped: a button that silently is not there is a hole in the toolbar
      // whose position depends on which group lost the item, and the user sees a toolbar that
      // rearranges itself for no reason.
      if (!item) {
        throw new Error(
          `toolbar group names \`${id}\`, which is not an item; ITEM_GROUPS and TOOLBAR_ITEMS disagree`,
        )
      }
      return item
    }),
  )
}

/**
 * What a button should be, with no DOM involved.
 *
 * # Why this is separated from `renderItem`
 *
 * Because the first version put both in one function, and the mutation run showed the cost
 * exactly: eleven deliberate breaks to the rendering -- dropping `aria-pressed`, dropping
 * `disabled`, dropping `aria-label`, inverting the pressed state, letting a disabled button
 * run its command -- produced **no** test failure, because all of them are decisions that
 * happen to be expressed as `setAttribute` calls. There is no DOM in Node, so a suite that
 * only exercised rendering would have measured nothing at all.
 *
 * So every decision is here, as data, and `renderItem` only applies it. What is left in the
 * browser-only function is `createElement`, `setAttribute` in a loop, and two
 * `addEventListener` calls -- none of which can be wrong in a way a user would notice, and all
 * of which are checked once by the browser suite.
 *
 * The split is also the reason `aria-pressed` is correct. It is set for *every* toggle,
 * including when false, because the value is computed from state rather than applied
 * conditionally at the call site -- a conditional `setAttribute` is exactly the shape that
 * leaves a stale `aria-pressed="true"` on a button that has been toggled off.
 */
export interface ButtonSpec {
  /** Attributes to set, in insertion order. Order is irrelevant to the DOM and to tests. */
  readonly attributes: Record<string, string>
  /** The SVG attributes for the icon, for the same reason. */
  readonly iconAttributes: Record<string, string>
  /** The item's path, carried through so the renderer does not look it up again. */
  readonly icon: string
  /** Whether clicking this button should run its command at all. */
  readonly runnable: boolean
  /** Whether a click should ask the app for a value first. */
  readonly needsInput: boolean
  /** The command and its fixed arguments, ready to pass to a handler. */
  readonly command: string
  readonly args: readonly unknown[]
}

/**
 * The complete description of one button.
 *
 * Pure: reads `state` and `shortcuts`, returns data, and touches nothing. `test/toolbar.ts`
 * asserts every attribute on every item, which is what makes the rendering decisions
 * measurable without a browser.
 */
export function buttonSpec(
  item: ToolbarItem,
  state: ToolbarState,
  shortcuts: ShortcutLookup = NO_SHORTCUTS,
): ButtonSpec {
  const { name, title } = toolbarLabel(item, shortcuts)
  const disabled = state.unsupported.has(item.id)

  const attributes: Record<string, string> = {
    // `type="button"` because this toolbar is going to sit in a document, and a `<button>`
    // with no type inside a form is a submit button. There is no form here, and the toolbar
    // must not depend on that staying true.
    type: 'button',
    class: 'holo-toolbar__button',
    // The name is an `aria-label` and not text content because the visible content is an SVG,
    // which contributes nothing to the accessible name. Without this the button is announced
    // as "button" and the toolbar is unusable with a screen reader.
    'aria-label': name,
    title,
  }

  if (item.kind === 'toggle') {
    // Every toggle, always. See the type's note.
    attributes['aria-pressed'] = state.active.has(item.id) ? 'true' : 'false'
  }
  if (disabled) {
    // The `disabled` attribute, not `aria-disabled` alone: a control that cannot be used
    // should not be in the tab order, and `aria-disabled` by itself would leave a focusable
    // control that does nothing. Both are set, because a `disabled` attribute is announced
    // inconsistently across engines and webkit2gtk -- the one this project verifies on -- is
    // not consistent about it.
    attributes.disabled = ''
    attributes['aria-disabled'] = 'true'
  }

  return {
    attributes,
    iconAttributes: ICON_ATTRIBUTES,
    icon: item.icon,
    // False for a disabled item even though the browser would ignore the click anyway: a
    // disabled button does not fire events, so the two agree in practice, and relying on the
    // browser's suppression is relying on a detail this module can state itself.
    runnable: !disabled,
    needsInput: item.needsInput === true,
    command: item.command,
    // A copy, not the item's own array: `handlers.run` is the app's, and a handler that pushed
    // onto the array it was handed would corrupt the table for every later click.
    args: [...(item.args ?? [])],
  }
}

/**
 * The icon's attributes, identical for every item.
 *
 * A constant rather than a per-item function because the only per-item part is the path. It is
 * hoisted so `buttonSpec` stays a description of *this* button rather than repeating the
 * drawing rules seventeen times.
 *
 * `aria-hidden` because the button's accessible name is the `aria-label` and the glyph
 * duplicates it -- without it a screen reader announces "Bold graphic Bold".
 * `focusable="false"` stops the SVG taking a tab stop of its own in engines that would
 * otherwise make it focusable, and is harmless where it is ignored.
 *
 * `stroke-width` 1.75 rather than 2 because at 18px a 2px stroke fills in the small numerals
 * in the heading and list icons. That is a judgement about rendering that no Node test can
 * check, and it is recorded here rather than left as an unexplained number.
 */
const ICON_ATTRIBUTES: Readonly<Record<string, string>> = {
  viewBox: '0 0 24 24',
  width: '18',
  height: '18',
  fill: 'none',
  stroke: 'currentColor',
  'stroke-width': '1.75',
  'stroke-linecap': 'round',
  'stroke-linejoin': 'round',
  'aria-hidden': 'true',
  focusable: 'false',
}

/** One button: the spec applied to a real element. */
function renderItem(
  item: ToolbarItem,
  state: ToolbarState,
  handlers: ToolbarHandlers,
  shortcuts: ShortcutLookup,
): HTMLElement {
  const spec = buttonSpec(item, state, shortcuts)
  const button = document.createElement('button')
  for (const [name, value] of Object.entries(spec.attributes)) button.setAttribute(name, value)
  button.dataset.item = item.id
  button.dataset.command = item.command
  button.appendChild(renderIcon(spec))

  button.addEventListener('mousedown', event => {
    // Preventing `mousedown`'s default is what keeps focus in the editor; a click on a button
    // otherwise moves focus to it, and a ProseMirror editor that has lost focus mid-command
    // applies the command to the wrong place or not at all. Only for the left button: a
    // right-click should not be able to move focus away from the editor either.
    if (event.button === 0) event.preventDefault()
  })
  button.addEventListener('click', () => {
    if (!spec.runnable) return
    if (spec.needsInput) {
      // The app collects the value. If it does not, nothing is inserted -- an equation button
      // that inserts an empty one is worse than one that does nothing.
      handlers.requestInput?.(item, value => {
        if (value) handlers.run(spec.command, [value, ...spec.args.slice(1)], item)
      })
      return
    }
    handlers.run(spec.command, spec.args, item)
  })
  return button
}

/** The icon, as an SVG element. The attributes come from {@link ICON_ATTRIBUTES}. */
function renderIcon(spec: ButtonSpec): SVGElement {
  const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg')
  for (const [name, value] of Object.entries(spec.iconAttributes)) svg.setAttribute(name, value)
  const path = document.createElementNS('http://www.w3.org/2000/svg', 'path')
  path.setAttribute('d', spec.icon)
  svg.appendChild(path)
  return svg
}

/**
 * Update a rendered toolbar in place.
 *
 * # Why not re-render
 *
 * Because re-rendering destroys the element the user has just clicked, which loses focus and
 * — if anything is holding a reference, which the caret tracker does — the reference with it.
 * A user clicking through six toolbar buttons in a row would be fighting a toolbar that
 * rebuilds itself on every transaction. So `renderToolbar` builds once and this adjusts
 * attributes, and the caller calls it on `transaction`.
 *
 * Mutating `aria-pressed` and `disabled` only, and never the DOM structure, is also what
 * keeps the toolbar's shape stable -- which is the property the module header is about. An
 * item that becomes unsupported *gains* a `disabled` attribute; it does not disappear.
 */
export function updateToolbar(root: HTMLElement, state: ToolbarState): number {
  let changed = 0
  for (const item of TOOLBAR_ITEMS) {
    const button = root.querySelector<HTMLElement>(`[data-item="${item.id}"]`)
    // A button that is not there is a bug in the caller (it updated a toolbar it had replaced),
    // and it is counted rather than thrown on, because this runs on every transaction and a
    // throw here would take the editor down with it. The count is what the caller can log.
    if (!button) continue
    if (item.kind === 'toggle') {
      const pressed = state.active.has(item.id) ? 'true' : 'false'
      if (button.getAttribute('aria-pressed') !== pressed) {
        button.setAttribute('aria-pressed', pressed)
        changed++
      }
    }
    const disabled = state.unsupported.has(item.id)
    if (disabled !== button.hasAttribute('disabled')) {
      if (disabled) button.setAttribute('disabled', '')
      else button.removeAttribute('disabled')
      button.setAttribute('aria-disabled', disabled ? 'true' : 'false')
      changed++
    }
  }
  return changed
}

/**
 * Invoke a command for an item, or report that the editor cannot.
 *
 * # Why this exists rather than a `handlers.run` closure per item
 *
 * Because the caller would otherwise write the same lookup, the same argument spread and the
 * same "is this command even here" question at every call site, and one of them would get it
 * wrong. It also means the toolbar's command names are checked in one place.
 */
export function runItem(
  editor: EditorLike | null | undefined,
  item: ToolbarItem,
  value?: string,
): boolean {
  // `hasCommand` returning true is what proves the editor is live: it reads the command table
  // inside a guard, and a destroyed editor throws there rather than reporting a missing key.
  if (!hasCommand(editor, item.command)) return false
  if (item.needsInput && !value) return false
  // A fresh array rather than the item's own `args`, because `apply` would otherwise receive
  // the declared array and a Tiptap command that mutates it would corrupt the table for every
  // later invocation.
  const args: unknown[] = item.needsInput ? [value!, ...(item.args ?? []).slice(1)] : [...(item.args ?? [])]
  // Read once: the `commands` getter is cheap but not free, and reading it twice invites a
  // version where the two reads are not the same object.
  const commands = (editor as unknown as { commands: Record<string, (...a: unknown[]) => unknown> }).commands
  const run = commands[item.command]
  if (typeof run !== 'function') return false
  // `commands` is the receiver, not the editor: Tiptap's command wrappers read `editor.view`
  // and `editor.state` off their `this`.
  run.apply(commands, args)
  return true
}
