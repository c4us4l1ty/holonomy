/**
 * Keyboard shortcuts: one registry, two scopes, and a comparison that does not care which
 * order the user pressed the modifiers in.
 *
 * # The decision this module exists to hold
 *
 * **Undo is one stack for the whole document, not one per editor.** M0 chose strategy C --
 * one Tiptap editor per mounted section -- which means every editor instance brings its own
 * `UndoRedo` extension and therefore its own history. Undo pressed while section 4 is
 * focused would walk back through section 4 alone and strand the user inside one section of
 * a 2000-page document forever. `core/undo.ts` fixes that with an `UndoCoordinator`; the
 * consequence *here* is that `undo` and `redo` are declared with `scope: 'global'` and the
 * app supplies a single handler for each. There is deliberately no API on this class for
 * registering an undo handler per editor, and `test/shortcuts.ts` asserts that undo resolves
 * identically from an event inside any section and from one outside every section.
 *
 * # Why scopes exist at all
 *
 * `Ctrl+B` means "bold" to a text editor and "bookmarks" to a browser. Resolving it
 * globally would mean the browser toolbar wins when focus is in the app, and resolving it
 * per-editor would mean it does nothing in the sidebar. So a binding declares which it is:
 *
 * - `global` -- app-level. Export, save, undo. Fires wherever focus is, because the
 *   document is one document and the caret being in section 300 does not make saving a
 *   different action.
 * - `editor` -- text formatting. Fires only when the event happened inside a section
 *   editor's editable surface. Bold in the status bar is not a thing.
 *
 * The rule is: rank the bindings that match, take the most specific, and *then* ask whether
 * it is in scope. There is no fallback from an out-of-scope winner to a looser one. So with
 * an editor binding on `primary+alt+b` and a global one on `primary+alt+shift+b`, holding all
 * three modifiers in the sidebar runs the global command, because that is the chord that was
 * pressed and the global binding is the more specific match for it. The reverse -- letting
 * the editor binding win and reporting "outside the editor" -- would mean a strict subset
 * swallows a chord more specific than itself, and the keystroke does nothing.
 *
 * # Why the platform is read lazily and injected
 *
 * The primary modifier is Command on macOS and Control everywhere else, and the difference
 * is not cosmetic: `⌘S` must resolve to save on a Mac and `Ctrl+S` must not, while on Linux
 * the reverse. A module-level `const IS_MAC = ...` would be computed at import time from
 * whatever `navigator` happened to be then, which is untestable and wrong in the one case
 * that matters -- proving that the same code resolves differently on the two platforms.
 * So the platform is resolved in the constructor, from an injected `navigator`-shaped value
 * when the caller supplies one, and `test/shortcuts.ts` builds two registries from two
 * fakes to assert exactly that.
 *
 * # Why the key is read from `code` as well as `key`
 *
 * `event.key` is the character the key *produced*, which means it is a function of the
 * keyboard layout and of whether shift is down. On a US layout `Shift+1` reports `'!'`, and
 * on macOS `⌥1` reports `'¡'`. So a binding written `primary+alt+1` -- heading 1, which is in
 * this module's own default table -- would never match on a Mac if only `key` were consulted,
 * and the user would press a shortcut the menu advertises and nothing would happen.
 *
 * `event.code` is the *physical* key (`Digit1`, `KeyA`), so it survives both. Both are
 * matched: a binding hits if it equals the code-derived base key or the lowercased `key`.
 * The code-derived form is preferred when both match, which is recorded as a preference in
 * the binding rather than as a rule, because a user who writes `primary+shift+!` in their own
 * binding deserves it to work.
 *
 * `code` is absent on synthetic events and on some virtual keyboards, so its absence falls
 * back to `key` alone rather than refusing to match.
 *
 * # Why a bare key never fires while the user is typing
 *
 * A shortcut with no modifiers and a single-character key *is* a character. Pressing `b` in
 * a text field must type a `b`; intercepting it would make the document untypable, and the
 * symptom would be "some letters do not appear" rather than "a shortcut is broken". The same
 * applies inside a ProseMirror section, which is `contenteditable` and is therefore a text
 * field by any honest definition.
 *
 * The rule is stated positively -- *a modifier-free binding whose key is one character is
 * suppressed while a text field has focus* -- rather than as an allow-list of safe keys,
 * because the positive form keeps Escape resolvable without special-casing it. `Escape` is
 * not a character; neither are `Enter`, `Tab`, the arrows or `F5`, and all of them stay
 * available. A binding *with* a modifier is never suppressed: `Ctrl+B` is not a character on
 * any keyboard this ships to.
 *
 * # Why collisions are reported and never thrown
 *
 * A duplicate registration of an identical combination for the same command is a no-op,
 * because a module that registers the same table twice is harmless and should not stop the
 * boot. A *different* command claiming a combination that is already taken is a real
 * conflict, and it is collected on the registry and exposed through {@link conflicts} rather
 * than thrown or silently shadowed: silently shadowed is the failure mode where one plugin
 * takes `primary+s` and the user's save button stops working with nothing in any log.
 *
 * The winner is the *earlier* declaration, because "the first registration claimed it" is a
 * fact a reader can check by looking at the order of the table, whereas "the last one won" is
 * what array iteration happens to do and looks like an accident. The conflict report names
 * both, so the answer is available either way.
 */

// -- the platform -------------------------------------------------------------

/**
 * The two properties of `navigator` this module reads.
 *
 * Declared rather than using the DOM type, because both are documented strings and the shape
 * is the whole of the dependency. It is also what makes the platform injectable: a caller in
 * a test supplies these two fields and gets a different answer.
 */
export interface NavigatorLike {
  /** `navigator.platform`: `MacIntel`, `Win32`, `Linux x86_64`, … */
  readonly platform?: string
  /** `navigator.userAgent`, consulted only when `platform` says nothing. */
  readonly userAgent?: string
}

/**
 * Which modifier is the primary one: Command on macOS, Control everywhere else.
 *
 * A union rather than the strings `meta`/`ctrl` leaking out of the module, so a caller cannot
 * accidentally compare a `Modifier` against a `PrimaryModifier` and get a compile error where
 * the answer differs by platform.
 */
export type PrimaryModifier = 'meta' | 'ctrl'

/**
 * The default `navigator`, frozen and exported so there is exactly one of it.
 *
 * `toolbar.ts` reads this to render keybinding labels, and the registry reads it to resolve.
 * Both go through `chordLabel` and `primaryModifier`, so the label a menu shows and the
 * combination the resolver accepts are produced by one function from one value -- which is
 * the alternative to a parity test between two hand-maintained tables.
 *
 * Frozen because it is a shared default, and a test that overwrote it would be the
 * test-isolation failure `core/assets.ts` documents in detail.
 */
export const DEFAULT_NAVIGATOR: NavigatorLike =
  typeof navigator === 'undefined' ? { platform: '', userAgent: '' } : { platform: navigator.platform, userAgent: navigator.userAgent }

/**
 * Which modifier is the primary one on this platform.
 *
 * # Why `platform` first and `userAgent` second
 *
 * `navigator.platform` is the declared answer and is what every engine this project ships to
 * populates (MacIntel on a Mac, "Linux x86_64" on the webkit2gtk build that is verified
 * here). It is deprecated, so `userAgent` is the fallback, and the empty default resolves to
 * `ctrl` rather than throwing: a registry that cannot tell which platform it is on should
 * behave like the majority of them, not refuse to construct.
 */
export function primaryModifier(nav?: NavigatorLike | null): PrimaryModifier {
  const n = nav ?? DEFAULT_NAVIGATOR
  if (/mac|iphone|ipad|ipod/i.test(n.platform ?? '')) return 'meta'
  if (/mac os x/i.test(n.userAgent ?? '')) return 'meta'
  return 'ctrl'
}

/** Whether the platform's primary modifier is Command, which is how labels are spelled. */
export function isMac(platform: PrimaryModifier): boolean {
  return platform === 'meta'
}

// -- chords -------------------------------------------------------------------

/**
 * A modifier, in the vocabulary a binding is written in.
 *
 * `mod` is the *portable* primary modifier: it means Command on macOS and Control
 * everywhere else, and it is replaced at parse time. The other four are literal, so a
 * binding can say `ctrl+s` and mean Control even on a Mac -- which is a different action
 * there, and occasionally the right one.
 */
export type Modifier = 'mod' | 'ctrl' | 'alt' | 'shift' | 'meta'

/** A key combination in comparable form: the lowercased key and the modifiers held. */
export interface Chord {
  readonly key: string
  readonly mods: readonly Modifier[]
}

/**
 * Every modifier name, in the order used to canonicalise a chord.
 *
 * `mod` is in the list because a chord can still be *unresolved* when it is compared or
 * printed: `parseChord` returns it, and `chordLabel` accepts it. An order that omitted it
 * would silently drop the portable modifier, and every `mod+x` binding would become a bare
 * `x` -- which reads as "the shortcut does not work" rather than as an error.
 */
const MODIFIER_ORDER: readonly Modifier[] = ['mod', 'ctrl', 'alt', 'shift', 'meta']

/**
 * Comparison order for modifiers, on a chord that has already been resolved.
 *
 * Apple's order -- Control, Option, Shift, Command -- because it is the one the platform
 * dictates and the one a Mac user's eye already reads. `formatChord` re-sorts for display,
 * because Windows and Linux put Shift before Option and a `Ctrl+Option+Shift+S` printed on a
 * Linux status bar is wrong in a way users notice even when they cannot say why.
 */
const RESOLVED_MOD_ORDER: readonly Modifier[] = ['ctrl', 'alt', 'shift', 'meta']

/** Display order off a Mac. Apple's Human Interface Guidelines modifier order. */
const MAC_MOD_ORDER: readonly Modifier[] = ['ctrl', 'alt', 'shift', 'meta']

/** Display order on Windows and Linux: Shift before Option, as Firefox and Chrome print it. */
const OTHER_MOD_ORDER: readonly Modifier[] = ['ctrl', 'shift', 'alt', 'meta']

/** Sort modifiers into a given order, dropping anything unrecognised. */
function sortMods(mods: Iterable<Modifier>, order: readonly Modifier[]): Modifier[] {
  const set = new Set<Modifier>(mods)
  return order.filter(m => set.has(m))
}

/**
 * Replace the portable `mod` with this platform's primary modifier.
 *
 * # Why this is one function and not two
 *
 * Because the registry's collision detection and the menu's label both have to make the same
 * substitution, and a place where they could differ is a shortcut that resolves on one screen
 * and is printed as something else. `chordLabel` accepts an unresolved chord for exactly this
 * reason: `toolbar.ts` holds a `Binding`'s written chord and has no registry to ask.
 */
function resolveMods(mods: readonly Modifier[], primary: PrimaryModifier): Modifier[] {
  return mods.map(m => (m === 'mod' ? primary : m))
}

/**
 * Parse a written chord such as `mod+shift+s`.
 *
 * # Why the *last* segment is the key
 *
 * Because `+` is itself a key, and `mod++` has to parse. Splitting on `+` and rejecting
 * anything that is not a modifier but the last segment handles it with no branch: the empty
 * segment is the key. The cost is that a key-first spelling like `s+mod` is *rejected* rather
 * than guessed at, which is the right trade -- a binding whose key is named `s` because the
 * author put it first is more likely a typo than an intention, and a typo should be loud.
 *
 * # Why it throws
 *
 * A binding with no key can never fire, and a segment that is neither a key nor a modifier is
 * almost always a typo. Both are wiring mistakes that should stop the boot loudly, unlike a
 * duplicate registration, which is a thing that legitimately happens.
 */
export function parseChord(spec: string): Chord {
  const parts = spec.split('+')
  let key = parts[parts.length - 1]!
  if (key === '' && parts[parts.length - 2] === '') {
    // `mod++` splits to ['mod', '', ''], and the key is the two trailing separators joined.
    // Read as a literal `+` rather than as "no key", because `+` is a key on every layout and
    // rejecting the only spelling that can express it would be a gap in the grammar rather
    // than a guard against a typo.
    parts.pop()
    key = '+'
  }
  if (key.length === 0) {
    throw new Error(`\`${spec}\` is not a chord: it names no key`)
  }
  key = key.toLowerCase()
  const mods: Modifier[] = []
  for (let i = 0; i < parts.length - 1; i++) {
    const part = parts[i]!.toLowerCase() as Modifier
    if (!MODIFIER_ORDER.includes(part)) {
      throw new Error(
        `\`${spec}\` is not a chord: \`${parts[i]}\` is neither a key nor a modifier ` +
          `(expected the key last, preceded by any of ${MODIFIER_ORDER.join(', ')})`,
      )
    }
    if (!mods.includes(part)) mods.push(part)
  }
  // Sorted here, and not only in `chordKey`, so that a *parsed* chord is already canonical for
  // a reader: `parseChord('shift+mod+s').mods` is `['mod', 'shift']` and not the order it was
  // written in. The mutation run is what settled this -- with the sort only in `chordKey`, the
  // two functions' output is identical for every input and the sort here is redundant, so the
  // question is whether a caller ever sees a parsed chord's `mods` directly. One does:
  // `resolve` reports the winning chord on a `ShortcutMatch`, and a caller rendering a tooltip
  // from it would print the author's order. Sorting at the point of construction means the
  // property holds for the value rather than for one of its uses.
  return { key, mods: sortMods(mods, MODIFIER_ORDER) }
}

/**
 * The canonical string for a chord: modifiers in a fixed order, then the key.
 *
 * This is the registry's map key, so two chords are the same shortcut exactly when this
 * string is equal -- which is what makes modifier order irrelevant rather than merely
 * tolerated. A chord that still contains `mod` is canonicalised as such and only resolved
 * later, so the same string means the same thing at every stage.
 *
 * # Why the sort is here and not only in `parseChord`
 *
 * Because `Chord` is a public type and this function takes one, so it must hold for a chord
 * built by hand. The mutation run found that: deleting the sort changed no test result, because
 * every chord reaching it had come from `parseChord`, which already sorts. That is the whole
 * class of bug `DOCTRINE.md` §2 warns about -- a guard that cannot be reached is not a guard,
 * and it is not evidence of anything. A caller who builds `{ key: 's', mods: ['shift', 'mod'] }`
 * by hand would get an order-sensitive key from the un-sorted version, and the two spellings of
 * the same shortcut would be two entries in the registry with no conflict reported between
 * them -- which is the silent shadowing the conflict report exists to prevent.
 */
export function chordKey(chord: Chord): string {
  return [...sortMods(chord.mods, MODIFIER_ORDER), chord.key].join('+')
}

/**
 * Render a chord for a menu or a tooltip.
 *
 * # Why the glyphs and not the words on a Mac
 *
 * Because `Ctrl+S` on a Mac means nothing to a Mac user and `⌘⇧S` means nothing to a Linux
 * one, and a label that does not look like the platform's own convention is decoration. The
 * word forms are still available off-Mac, where that is what the platform prints.
 *
 * `mod` is resolved here rather than requiring a resolved chord, because the caller that most
 * needs a label -- a toolbar button holding a written binding -- has no registry to ask.
 */
export function chordLabel(chord: Chord, platform: PrimaryModifier): string {
  const mac = isMac(platform)
  const parts: string[] = []
  for (const mod of sortMods(resolveMods(chord.mods, platform), mac ? MAC_MOD_ORDER : OTHER_MOD_ORDER)) {
    if (mod === 'meta') parts.push(mac ? '⌘' : 'Meta')
    else if (mod === 'ctrl') parts.push(mac ? '⌃' : 'Ctrl')
    else if (mod === 'alt') parts.push(mac ? '⌥' : 'Alt')
    else parts.push(mac ? '⇧' : 'Shift')
  }
  parts.push(keyGlyph(chord.key))
  return parts.join(mac ? '' : '+')
}

/**
 * The printable form of a key.
 *
 * Single characters are uppercased because every platform prints a letter shortcut that way,
 * and the named keys have glyphs on a Mac and words elsewhere. The words are used for
 * `Escape` rather than a glyph on the Mac too, since `Esc` is what a Mac menu says and the
 * glyph set people expect is arrows and the function keys.
 */
function keyGlyph(key: string): string {
  if (key.length === 1) return key.toUpperCase()
  const named: Record<string, string> = {
    escape: 'Esc',
    enter: 'Enter',
    tab: 'Tab',
    backspace: 'Backspace',
    delete: 'Del',
    space: 'Space',
    arrowup: '↑',
    arrowdown: '↓',
    arrowleft: '←',
    arrowright: '→',
  }
  if (named[key]) return named[key]!
  // Function keys are already `f1`..`f24` and every platform prints them capitalised.
  if (/^f\d{1,2}$/.test(key)) return key.toUpperCase()
  return key.charAt(0).toUpperCase() + key.slice(1)
}

// -- events -------------------------------------------------------------------

/**
 * The parts of a `KeyboardEvent` this module reads.
 *
 * Declared rather than taking the DOM type, because `code` and the four modifier flags are
 * the whole of the dependency and naming them makes it visible. Every property is optional
 * except `key`, so a test can construct one with three fields.
 */
export interface KeyEventLike {
  readonly key: string
  readonly code?: string
  readonly ctrlKey?: boolean
  readonly shiftKey?: boolean
  readonly altKey?: boolean
  readonly metaKey?: boolean
}

/**
 * Physical key to the character it produces on a US layout.
 *
 * # Why a table and not arithmetic
 *
 * Because the mapping is a property of the DOM's `code` names, not of anything this module
 * decides. Reconstructing it from the name (`KeyA` minus `Key`, `Digit4` minus `Digit`)
 * handles the letters and digits and silently mangles every punctuation key, and those are
 * exactly the ones a keyboard-layout disagreement shows up on.
 *
 * Keys whose `code` does not vary with the layout are not here: an event with
 * `code: 'KeyA'` already yields `'a'` under every layout, and an event with no usable `code`
 * falls back to `key` alone.
 */
const CODE_TO_KEY: Readonly<Record<string, string>> = {
  Backquote: '`',
  Backslash: '\\',
  BracketLeft: '[',
  BracketRight: ']',
  Comma: ',',
  Equal: '=',
  Minus: '-',
  Period: '.',
  Quote: "'",
  Semicolon: ';',
  Slash: '/',
  Space: ' ',
  NumpadAdd: '+',
  NumpadSubtract: '-',
  NumpadMultiply: '*',
  NumpadDivide: '/',
  NumpadDecimal: '.',
  Enter: 'enter',
  NumpadEnter: 'enter',
  Escape: 'escape',
  Tab: 'tab',
  Backspace: 'backspace',
  Delete: 'delete',
  Insert: 'insert',
  Home: 'home',
  End: 'end',
  PageUp: 'pageup',
  PageDown: 'pagedown',
  ArrowLeft: 'arrowleft',
  ArrowUp: 'arrowup',
  ArrowRight: 'arrowright',
  ArrowDown: 'arrowdown',
}

/** The base character a physical key produces, or null when the code says nothing useful. */
function baseKeyFromCode(code: string | undefined): string | null {
  if (!code) return null
  const fixed = CODE_TO_KEY[code]
  if (fixed) return fixed
  // `KeyA`..`KeyZ` and `Digit0`..`Digit9`, plus `F1`..`F24`.
  const letter = /^Key([A-Z])$/.exec(code)
  if (letter) return letter[1]!.toLowerCase()
  const digit = /^Digit([0-9])$/.exec(code)
  if (digit) return digit[1]!
  const fn = /^(?:F|Digit)([0-9]{1,2})$/.exec(code)
  if (fn) return code.toLowerCase()
  return null
}

/** The modifiers held, as a concrete set with no `mod` in it. */
function heldMods(event: KeyEventLike): Modifier[] {
  const mods: Modifier[] = []
  if (event.ctrlKey) mods.push('ctrl')
  if (event.altKey) mods.push('alt')
  if (event.shiftKey) mods.push('shift')
  if (event.metaKey) mods.push('meta')
  return sortMods(mods, RESOLVED_MOD_ORDER)
}

/**
 * Normalise a key event into a chord.
 *
 * Modifier order is dropped by construction -- {@link chordKey} sorts -- and case by
 * lowercasing, so `Ctrl+Shift+S` and `shift+ctrl+s` produce the same chord. `mod` never
 * appears: an event reports which modifiers were *held*, and the portable spelling is a
 * property of the binding, not of the key press.
 */
export function chordFromEvent(event: KeyEventLike): Chord {
  return { key: event.key.toLowerCase(), mods: heldMods(event) }
}

// -- the event's context ------------------------------------------------------

/**
 * The two facts about *where* a key event happened that resolution needs.
 *
 * Plain data, supplied by the caller. That is deliberate: deciding whether a key press
 * happened inside a section editor means asking the registry, which is the app's, and asking
 * whether a `<select>` takes text input is a DOM question. Putting the answers in a record
 * keeps this module testable in Node with no document, and the one function that reads them
 * off an element is {@link contextForElement}.
 */
export interface EventContext {
  /** The event happened inside a section editor's editable surface. */
  readonly inEditor: boolean
  /** A text field has focus, so a bare character key is being typed rather than commanded. */
  readonly typing: boolean
}

/**
 * The parts of a DOM `Element` {@link contextForElement} reads.
 *
 * The names are the real DOM ones, deliberately: `tagName`, `isContentEditable`,
 * `getAttribute` and `closest` are what a real element answers, so a caller passes its event
 * target through untouched. The test in `test/shortcuts.ts` supplies objects implementing
 * exactly this interface, which fakes *a documented dependency* rather than the DOM -- the
 * difference that `core/assets.ts` records about global stubs racing between tests. What is
 * still unverified here is that a real `Element` answers these four properties as expected,
 * which needs a browser.
 */
export interface ElementLike {
  readonly tagName: string
  /**
   * The *effective* editability, including inheritance.
   *
   * Preferred over the `contenteditable` attribute because a ProseMirror surface is
   * `contenteditable="true"` on a div that may sit inside another editable region, and only
   * the effective value is the honest answer to "can the user type here".
   */
  readonly isContentEditable?: boolean
  getAttribute(name: string): string | null
  closest(selector: string): ElementLike | null
}

/**
 * `input` types that take text, and so count as typing.
 *
 * # Why the list is explicit and short
 *
 * Because the failure is asymmetric. Treating `<input type="button">` as a text field
 * suppresses every bare-key shortcut while a button happens to hold focus, which is a
 * plausible bug with no visible cause. Treating `<input type="text">` as not-a-text-field
 * means typing `b` into the find box fires bold instead of inserting a character, which is
 * worse and *is* visible. The list is therefore every type that can hold a text cursor, and
 * nothing else; an absent or unrecognised `type` defaults to `text` per the HTML spec, so it
 * is treated as typing.
 */
const TEXT_INPUT_TYPES: ReadonlySet<string> = new Set([
  'text',
  'search',
  'email',
  'url',
  'tel',
  'password',
  'number',
  '',
])

/**
 * The selector that identifies a section editor's editable surface.
 *
 * `.ProseMirror` is what ProseMirror itself puts on every editable view, so it is true for
 * every editor the registry creates without this module knowing anything about the registry.
 * The section *container* (`.section-slice`) was the rejected alternative: it includes the
 * chrome around the text, so a click on the section label would enable formatting commands
 * that then act on whichever editor happens to be focused.
 */
export const EDITOR_SELECTOR = '.ProseMirror'

/** Tags that always take text, whatever their attributes. */
const TEXT_TAGS: ReadonlySet<string> = new Set(['TEXTAREA'])

/** Read `inEditor` and `typing` off a key event's target. */
export function contextForElement(
  element: ElementLike | null,
  opts: { editorSelector?: string } = {},
): EventContext {
  if (!element) return { inEditor: false, typing: false }
  const selector = opts.editorSelector ?? EDITOR_SELECTOR
  const inEditor = element.closest(selector) !== null
  return { inEditor, typing: isTypingTarget(element) }
}

/**
 * Whether an element takes text input from the keyboard.
 *
 * `contenteditable` is checked first, because an editable div is the common case here -- every
 * section editor is one -- and a `<textarea>` cannot be contenteditable anyway, so the order
 * cannot change an answer.
 */
function isTypingTarget(element: ElementLike): boolean {
  if (element.isContentEditable === true) return true
  const tag = (element.tagName ?? '').toUpperCase()
  if (TEXT_TAGS.has(tag)) return true
  if (tag !== 'INPUT') return false
  const type = (element.getAttribute('type') ?? '').toLowerCase()
  // An unrecognised type is `text` as far as the user is concerned, and treating it as
  // non-typing is the direction that swallows keystrokes.
  return TEXT_INPUT_TYPES.has(type) || !TYPE_IS_KNOWN(type)
}

/** `input` types that are definitely not text. Anything else falls back to `text`. */
const NON_TEXT_INPUT_TYPES: ReadonlySet<string> = new Set([
  'button',
  'submit',
  'reset',
  'checkbox',
  'radio',
  'file',
  'color',
  'range',
  'date',
  'time',
  'datetime-local',
  'month',
  'week',
  'image',
  'hidden',
])

function TYPE_IS_KNOWN(type: string): boolean {
  return NON_TEXT_INPUT_TYPES.has(type)
}

// -- bindings -----------------------------------------------------------------

/** Which bindings resolve: everywhere in the app, or only inside a section editor. */
export type ShortcutScope = 'global' | 'editor'

/** A command name: a binding to a handler, and the thing a menu item calls. */
export type CommandName = string

/** A registered combination and what it does. */
export interface Binding {
  readonly command: CommandName
  /** The written form, e.g. `'mod+shift+s'`. Kept for reporting; the registry parses it. */
  readonly chord: string
  readonly scope: ShortcutScope
}

/** A binding with its chord parsed and its `mod` replaced for a specific platform. */
export interface ResolvedBinding extends Binding {
  /** The parsed chord, with `mod` already resolved to the platform's primary modifier. */
  readonly resolved: Chord
  /** Position in declaration order, which is the last tiebreak. */
  readonly order: number
}

/** Two commands claiming one combination. */
export interface Conflict {
  /** The canonical chord both claim. */
  readonly chord: string
  /** The binding that resolution picks. */
  readonly winner: ResolvedBinding
  /** The binding that was registered later and is therefore shadowed. */
  readonly loser: ResolvedBinding
}

/** What registering a binding did. */
export type RegisterResult =
  | { readonly kind: 'added'; readonly binding: ResolvedBinding }
  | { readonly kind: 'duplicate'; readonly existing: ResolvedBinding }
  | { readonly kind: 'conflict'; readonly conflict: Conflict }

/** What a key event means. */
export type ShortcutMatch =
  | {
      readonly kind: 'command'
      readonly command: CommandName
      readonly binding: ResolvedBinding
      readonly chord: Chord
    }
  | {
      readonly kind: 'blocked'
      readonly reason: BlockReason
      readonly binding: ResolvedBinding
      readonly chord: Chord
    }
  | { readonly kind: 'none' }

/** Why a matching binding did not fire. Both are the app's context, not a bug. */
export type BlockReason = 'outside-editor' | 'text-input'

/** The result of resolving *and* invoking. */
export interface DispatchResult {
  readonly match: ShortcutMatch
  /** Whether a handler ran, which is what the caller uses to decide whether to consume. */
  readonly handled: boolean
}

/**
 * # Whether a handler may swallow the event
 *
 * A `KeyboardEvent` at the window level in the capture phase reaches the target's listeners
 * only if propagation continues, and ProseMirror's own `keydown` handler is one of them. It
 * does not check `defaultPrevented`; it is reached or it is not. So a shortcut that has run
 * has to stop propagation, not merely prevent the default action, or the editor will also act
 * on the key. `preventDefault` alone is the usual advice and it is insufficient here.
 *
 * `stopImmediatePropagation` is *not* used: it would also silence listeners registered later
 * on the same node, which the app cannot see and does not own.
 */
export interface ConsumableEvent {
  preventDefault(): void
  stopPropagation(): void
}

/** Consume a key event whose shortcut has run. */
export function consumeEvent(event: ConsumableEvent): void {
  event.preventDefault()
  event.stopPropagation()
}

// -- the default table --------------------------------------------------------

/**
 * App-level bindings.
 *
 * Separate from {@link EDITOR_BINDINGS} because the split *is* the module's central
 * distinction and a reader should be able to see which is which without filtering. The two
 * arrays are the same declaration as {@link DEFAULT_BINDINGS}, not a second table to keep in
 * step.
 */
export const GLOBAL_BINDINGS: readonly Binding[] = [
  // Global because the history is document-wide. See the module header and `core/undo.ts`.
  { command: 'undo', chord: 'mod+z', scope: 'global' },
  { command: 'redo', chord: 'mod+shift+z', scope: 'global' },
  { command: 'app.save', chord: 'mod+s', scope: 'global' },
  { command: 'app.exportPdf', chord: 'mod+shift+e', scope: 'global' },
  // `mod+f` deliberately claimed from the browser. The webview's own find searches the
  // mounted DOM, and a Holonomy document mounts 3 to 5 sections out of 1,300 -- so native
  // find reports "no results" for a term that is on page 400, and a user reads that as
  // "the text is not there". Binding the chord here is what routes it to the FTS5 index
  // instead; see `core/search-panel.ts`.
  { command: 'app.search', chord: 'mod+f', scope: 'global' },
]

/**
 * Text-formatting bindings, which only fire inside a section editor.
 *
 * # Why `mod+alt+1` for headings rather than `mod+1`
 *
 * Because `mod+1` in a browser is a tab switch, and inside the webview that steals the
 * keystroke before this module ever sees it. `mod+alt+1` is what other editors use and is
 * not bound by anything else. This is also the binding that made the `code`-versus-`key`
 * question in the module header concrete: `⌘⌥1` reports `'¡'` on a US-layout Mac, so it only
 * resolves because `code` is read as well as `key`.
 */
export const EDITOR_BINDINGS: readonly Binding[] = [
  { command: 'bold', chord: 'mod+b', scope: 'editor' },
  { command: 'italic', chord: 'mod+i', scope: 'editor' },
  { command: 'underline', chord: 'mod+u', scope: 'editor' },
  { command: 'strike', chord: 'mod+shift+s', scope: 'editor' },
  { command: 'code', chord: 'mod+e', scope: 'editor' },
  { command: 'highlight', chord: 'mod+shift+h', scope: 'editor' },
  { command: 'heading1', chord: 'mod+alt+1', scope: 'editor' },
  { command: 'heading2', chord: 'mod+alt+2', scope: 'editor' },
  { command: 'heading3', chord: 'mod+alt+3', scope: 'editor' },
  { command: 'bulletList', chord: 'mod+shift+8', scope: 'editor' },
  { command: 'orderedList', chord: 'mod+shift+7', scope: 'editor' },
  { command: 'blockquote', chord: 'mod+shift+b', scope: 'editor' },
  { command: 'codeBlock', chord: 'mod+alt+c', scope: 'editor' },
  { command: 'link', chord: 'mod+k', scope: 'editor' },
]

/** Everything the app ships with, globals first so they read before the formatting table. */
export const DEFAULT_BINDINGS: readonly Binding[] = [...GLOBAL_BINDINGS, ...EDITOR_BINDINGS]

/**
 * The default binding for a command, or null.
 *
 * The single source of truth for "what is the shortcut for bold", used by
 * `toolbar.ts` to label a button. A second table in the toolbar module would be a parity test
 * waiting to fail at the worst moment, which is the arrangement `DOCTRINE.md` §8 is about.
 */
export function bindingFor(command: CommandName, table: readonly Binding[] = DEFAULT_BINDINGS): Binding | null {
  return table.find(b => b.command === command) ?? null
}

// -- the registry -------------------------------------------------------------

/** A command's handler. Anything needing arguments closes over them. */
export type ShortcutHandler = () => void

/**
 * Command name to handler.
 *
 * A flat record, and the shape is load-bearing: there is one slot per command name, so there
 * is no way to express "undo, for section 3". A per-section handler would be a nested record
 * and this is not one. A command with no entry is not an error -- it is reported through
 * {@link ShortcutRegistry.unbound} -- because a shortcut that has no effect yet is how a
 * partially wired app boots.
 */
export type ShortcutHandlers = Readonly<Record<CommandName, ShortcutHandler | undefined>>

export interface ShortcutOptions {
  /**
   * Where the platform comes from.
   *
   * Read once, in the constructor, from this value or the ambient `navigator`. See the module
   * header on why this is not a module-level constant.
   */
  readonly navigator?: NavigatorLike | null
  /** Bindings to seed with. Defaults to {@link DEFAULT_BINDINGS}. */
  readonly bindings?: readonly Binding[]
}

/**
 * Which of two matching bindings is more specific.
 *
 * The order is more modifiers, then a longer key name, then the earlier declaration.
 *
 * # Why the first two criteria are reached at all
 *
 * Because a modifier set is matched exactly, so two bindings normally cannot both match one
 * event -- the exception is a *superset*: a four-modifier binding and a one-modifier one both
 * match a keypress with all four held, and the more specific one is the one the user asked
 * for. The key-length criterion is the same idea for the other source of multiplicity, the two
 * candidate forms of one event: `Shift+1` can be written `shift+1` or `shift+!`, and both
 * match, so the tiebreak is needed there. Declaration order settles whatever is left, and
 * settles it in favour of the table the reader can see.
 *
 * Exported because the criteria are worth asserting directly; `resolve` only reaches them in
 * the cases above, and a test that only exercised `resolve` would not notice if the order
 * were reversed.
 */
export function moreSpecific(a: ResolvedBinding, b: ResolvedBinding): boolean {
  if (a.resolved.mods.length !== b.resolved.mods.length) {
    return a.resolved.mods.length > b.resolved.mods.length
  }
  if (a.resolved.key.length !== b.resolved.key.length) {
    return a.resolved.key.length > b.resolved.key.length
  }
  return a.order < b.order
}

/** A registry of bindings, and the handlers they call. */
export class ShortcutRegistry {
  private readonly handlers: ShortcutHandlers
  private readonly entries: ResolvedBinding[] = []
  private readonly byChord = new Map<string, ResolvedBinding>()
  private readonly found: Conflict[] = []
  private readonly primary: PrimaryModifier

  // The handler table is a field with an assignment rather than a `constructor(private
  // readonly handlers: ...)` parameter property, because this module is loaded under
  // `node --experimental-strip-types`, which erases types without transforming syntax and
  // rejects parameter properties. The field is still private and still readonly. See the
  // same note on `AssetResolver`'s constructor in `core/assets.ts`.
  constructor(handlers: ShortcutHandlers, opts: ShortcutOptions = {}) {
    this.handlers = handlers
    this.primary = primaryModifier(opts.navigator)
    for (const binding of opts.bindings ?? DEFAULT_BINDINGS) this.register(binding)
  }

  /** The platform's primary modifier. Every label and every `mod` resolution uses this. */
  get platform(): PrimaryModifier {
    return this.primary
  }

  /**
   * Add a binding.
   *
   * Never throws on a duplicate. An identical combination for the same command is a no-op,
   * and a different command claiming a taken combination is recorded and returned rather than
   * thrown on or silently shadowed.
   */
  register(binding: Binding): RegisterResult {
    const parsed = parseChord(binding.chord)
    const resolved: Chord = {
      key: parsed.key,
      // `mod` is replaced here, once, so the registry's map key is concrete and a collision
      // between `mod+z` and `ctrl+z` on a platform where they are the same key is detected
      // rather than discovered by a user pressing an unbound combination.
      mods: sortMods(resolveMods(parsed.mods, this.primary), RESOLVED_MOD_ORDER),
    }
    const entry: ResolvedBinding = { ...binding, resolved, order: this.entries.length }
    const key = chordKey(resolved)
    const existing = this.byChord.get(key)
    if (existing) {
      if (existing.command === entry.command) return { kind: 'duplicate', existing }
      const conflict: Conflict = { chord: key, winner: existing, loser: entry }
      this.found.push(conflict)
      this.entries.push(entry)
      return { kind: 'conflict', conflict }
    }
    this.byChord.set(key, entry)
    this.entries.push(entry)
    return { kind: 'added', binding: entry }
  }

  /**
   * Every registered binding, in declaration order, optionally filtered by command.
   *
   * Exposed because "how many bindings does `undo` have, and are they global" is a
   * question about the architecture and the only honest way to answer it is to look.
   */
  bindings(command?: CommandName): ResolvedBinding[] {
    return command === undefined ? [...this.entries] : this.entries.filter(b => b.command === command)
  }

  /** Every combination two different commands claim, oldest first. */
  conflicts(): Conflict[] {
    return [...this.found]
  }

  /** Commands that resolve to a binding but have no handler, so they would do nothing. */
  unbound(): CommandName[] {
    const out: CommandName[] = []
    for (const entry of this.entries) {
      if (!this.handlers[entry.command] && !out.includes(entry.command)) out.push(entry.command)
    }
    return out
  }

  /** A binding's human-readable label on this registry's platform, or null if unbound. */
  labelFor(command: CommandName): string | null {
    const entry = this.entries.find(b => b.command === command)
    return entry ? chordLabel(entry.resolved, this.primary) : null
  }

  /**
   * The chord a key event names, ignoring scope and the typing rule.
   *
   * The code-derived base key first, the lowercased `key` second, so that a binding written
   * either way matches and the physical key wins. See `resolve` for why the order is the
   * preference, and the module header for why `key` alone is not enough.
   */
  private candidates(event: KeyEventLike): Chord[] {
    const mods = heldMods(event)
    const fromCode = baseKeyFromCode(event.code)
    const fromKey = event.key.toLowerCase()
    // The `key`-derived candidate is kept even when it is the *same* string as the code-derived
    // one, because `resolve` takes the first candidate that matches and the two are not
    // interchangeable downstream: a binding written `shift+1` must match as readily as one
    // written `shift+!`. Collapsing them here would make the code-derived form the only one
    // that ever matches, and a synthetic event with no `code` would have no candidate at all.
    if (fromCode === null) return [{ key: fromKey, mods }]
    if (fromCode === fromKey) return [{ key: fromKey, mods }]
    return [{ key: fromCode, mods }, { key: fromKey, mods }]
  }

  /**
   * What a key event means in a given context.
   *
   * # Why the first *candidate* that matches anything wins
   *
   * Because the candidates are ordered by how authoritative they are: the code-derived base
   * key first, the `key`-derived one second. When one event matches two written forms --
   * `mod+alt+1` and `mod+alt+!` both match `⌘⌥1` on a US layout -- the one naming the
   * physical key is what the user pressed and what they meant. Ranking the two by key length
   * instead would be arbitrary: `1` and `!` are the same length, so the criterion would not
   * apply and declaration order would decide instead.
   *
   * # Why scope is checked on the winner rather than used to filter candidates
   *
   * Because a global binding on `mod+alt+shift+b` and an editor one on `mod+alt+b` are two
   * distinct commands on two distinct chords, and pressing the global chord in the sidebar
   * should run the global command. Filtering by scope first would hand the keypress to the
   * editor binding, report "outside the editor", and do nothing -- a strict subset swallowing
   * a chord that is more specific than itself.
   *
   * What *is* enforced is that there is no fallback: once the most specific matching binding
   * is chosen, an out-of-scope winner means the keypress does nothing. It does not drop to a
   * looser binding, because the user asked for one specific thing and silently running a
   * different command is the outcome to avoid.
   */
  resolve(event: KeyEventLike, ctx: EventContext): ShortcutMatch {
    for (const chord of this.candidates(event)) {
      const key = chordKey(chord)
      const matches = this.entries.filter(entry => chordKey(entry.resolved) === key)
      if (matches.length === 0) continue
      matches.sort((a, b) => (moreSpecific(a, b) ? -1 : moreSpecific(b, a) ? 1 : 0))
      return this.interpret(matches[0]!, ctx)
    }
    return { kind: 'none' }
  }

  /** Turn the winning binding into a match, or a stated reason it does not fire. */
  private interpret(winner: ResolvedBinding, ctx: EventContext): ShortcutMatch {
    const chord = winner.resolved
    // A blocked binding is reported rather than passed over, so a caller can grey out a menu
    // item instead of leaving the user to press a key that will not do anything. A `Ctrl+B`
    // typed into a find box is a correct refusal, not a silence.
    if (winner.scope === 'editor' && !ctx.inEditor) {
      return { kind: 'blocked', reason: 'outside-editor', binding: winner, chord }
    }
    // A modifier-free single-character key is a character, not a command. See the module
    // header on why this is stated as a suppression rather than an allow-list.
    if (chord.mods.length === 0 && isTypableKey(chord.key) && ctx.typing) {
      return { kind: 'blocked', reason: 'text-input', binding: winner, chord }
    }
    return { kind: 'command', command: winner.command, binding: winner, chord }
  }

  /**
   * Resolve and, if it names a command, call its handler.
   *
   * A command with no handler resolves but is not handled, so the caller does not consume the
   * event -- the browser's own action is the better outcome than a keypress that does
   * nothing at all.
   */
  dispatch(event: KeyEventLike, ctx: EventContext): DispatchResult {
    const match = this.resolve(event, ctx)
    if (match.kind !== 'command') return { match, handled: false }
    const handler = this.handlers[match.command]
    if (!handler) return { match, handled: false }
    handler()
    return { match, handled: true }
  }
}

/**
 * Whether a key is one a user types as a character.
 *
 * After normalisation every named key is a word (`escape`, `arrowdown`, `f5`) and every
 * typable thing is one character -- including space and the punctuation keys, which is
 * correct. So the test is the length, and it is what keeps Escape resolvable without
 * naming it anywhere.
 */
function isTypableKey(key: string): boolean {
  return key.length === 1
}
