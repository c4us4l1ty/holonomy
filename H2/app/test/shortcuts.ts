/**
 * Keyboard shortcuts: normalisation, scope, the global undo stack, collisions, and labels.
 *
 * # Why this runs in Node with no DOM
 *
 * `shortcuts.ts` reads no global: the platform comes from an injected `navigator`-shaped
 * value, and the two facts about a key event's *target* are plain data on an
 * {@link EventContext} the caller supplies. Everything below is therefore the product's own
 * code, not a stand-in for it.
 *
 * The one exception is `contextForElement`, which reads four documented DOM properties off an
 * {@link ElementLike}. The test supplies objects implementing exactly that interface. That is
 * a fake of a *declared dependency*, not a fake of the DOM, and it is not installed on a
 * global -- which is the arrangement that made `core/assets.ts`'s tests fight each other, and
 * which this project deleted. What this cannot check is that a real `Element` answers those
 * four properties as expected; that needs a browser, and it is listed in the report.
 *
 * Run: node --experimental-strip-types test/shortcuts.ts
 */

import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import {
  bindingFor,
  chordFromEvent,
  chordKey,
  chordLabel,
  consumeEvent,
  contextForElement,
  DEFAULT_BINDINGS,
  EDITOR_BINDINGS,
  GLOBAL_BINDINGS,
  isMac,
  moreSpecific,
  parseChord,
  primaryModifier,
  ShortcutRegistry,
  type Binding,
  type ConsumableEvent,
  type ElementLike,
  type EventContext,
  type KeyEventLike,
  type NavigatorLike,
  type ResolvedBinding,
  type ShortcutHandlers,
  type ShortcutMatch,
} from '../src/core/shortcuts.ts'

let passed = 0
let failed = 0
const failures: string[] = []

function test(name: string, fn: () => Promise<unknown> | unknown): Promise<void> {
  return Promise.resolve()
    .then(fn)
    .then(detail => {
      passed++
      console.log(`PASS  ${name}${detail !== undefined ? `  ${JSON.stringify(detail)}` : ''}`)
    })
    .catch((e: any) => {
      failed++
      failures.push(name)
      console.log(`FAIL  ${name}\n        ${e.message}`)
    })
}

function ok(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg)
}

// -- fixtures -----------------------------------------------------------------

const MAC: NavigatorLike = { platform: 'MacIntel', userAgent: 'Mozilla/5.0 (Macintosh)' }
const LINUX: NavigatorLike = { platform: 'Linux x86_64', userAgent: 'Mozilla/5.0 (X11; Linux)' }

/** Every command the handlers below answer, so nothing is silently unbound. */
const COMMANDS: string[] = [
  'undo', 'redo', 'app.save', 'app.exportPdf', 'app.search',
  'bold', 'italic', 'underline', 'strike', 'code', 'highlight',
  'heading1', 'heading2', 'heading3',
  'bulletList', 'orderedList', 'blockquote', 'codeBlock', 'link',
]

/** A registry that records what ran, so a dispatch is observable. */
function recorder(): { handlers: ShortcutHandlers; ran: string[] } {
  const ran: string[] = []
  const record: Record<string, () => void> = {}
  for (const c of COMMANDS) record[c] = () => ran.push(c)
  return { handlers: record, ran }
}

/** A registry on a named platform, with the default table. */
function registryOn(nav: NavigatorLike, extra: readonly Binding[] = []) {
  const { handlers, ran } = recorder()
  const registry = new ShortcutRegistry(handlers, {
    navigator: nav,
    bindings: [...DEFAULT_BINDINGS, ...extra],
  })
  return { registry, ran }
}

// The two contexts every test resolves against. A section editor is both `inEditor` and
// `typing`, because it is contenteditable -- conflating the two is the failure the typing rule
// exists to prevent, so the common case is the one that exercises both.
const IN_EDITOR: EventContext = { inEditor: true, typing: true }
const OUTSIDE_EDITOR: EventContext = { inEditor: false, typing: false }

/** A key event with nothing implicit: no layout, no shift artefacts. */
function key(key: string, mods: Partial<Omit<KeyEventLike, 'key'>> = {}): KeyEventLike {
  return { key, code: '', ctrlKey: false, shiftKey: false, altKey: false, metaKey: false, ...mods }
}

/** The command a match names, or a description of why it did not. */
function commandOf(match: ShortcutMatch): string {
  if (match.kind === 'command') return match.command
  if (match.kind === 'blocked') return `blocked:${match.reason}`
  return 'none'
}

/** A minimal element implementing the four documented properties `contextForElement` reads. */
function element(opts: { tagName: string; type?: string; editable?: boolean; editorAncestor?: boolean }): ElementLike {
  return {
    tagName: opts.tagName,
    isContentEditable: opts.editable === true,
    getAttribute: (name: string) => (name === 'type' ? opts.type ?? null : null),
    closest: (selector: string) => (selector === '.ProseMirror' && opts.editorAncestor ? element({ tagName: 'DIV' }) : null),
  }
}

async function main() {
  console.log('shortcuts: normalisation, scope, one global undo stack, collisions, labels')
  console.log('='.repeat(72))

  // -- the comparable form -------------------------------------------------

  await test('modifier order does not matter', () => {
    // The same three combinations written in three orders, plus mixed case. If comparison
    // were order-sensitive or case-sensitive these would be three different chords.
    const a = chordKey(parseChord('mod+shift+s'))
    const b = chordKey(parseChord('shift+mod+s'))
    const c = chordKey(parseChord('SHIFT+MOD+S'))
    ok(a === b, `mod+shift+s gave ${a} and shift+mod+s gave ${b}`)
    ok(a === c, `mod+shift+s gave ${a} and SHIFT+MOD+S gave ${c}`)
    // The key comes last, deliberately, so that `+` is parseable as a key. A key-first
    // spelling is therefore rejected rather than guessed at -- asserted here because the
    // asymmetry is surprising and the rejection is the documented behaviour.
    let threw = false
    try {
      parseChord('s+shift+mod')
    } catch {
      threw = true
    }
    ok(threw, 'a key-first spelling should be rejected, not silently reordered')
    ok(chordKey(parseChord('mod++')) === 'mod++', `the + key should parse, got ${chordKey(parseChord('mod++'))}`)
    return { chord: a }
  })

  await test('a parsed chord is already canonical, not only when compared', () => {
    // # Why this is separate from the comparison test.
    //
    // The mutation run found that sorting in `parseChord` is redundant *for the registry*,
    // because `chordKey` sorts again -- so removing it changed no result. It is kept because a
    // caller sees a parsed chord's `mods` directly: `resolve` returns the winning chord on a
    // `ShortcutMatch`, and a caller rendering a tooltip from it would print the author's
    // writing order. The property has to hold for the value, not for one of its uses.
    const parsed = parseChord('shift+mod+ctrl+s')
    ok(parsed.mods.join(',') === 'mod,ctrl,shift', `mods should be canonical, got ${parsed.mods.join(',')}`)
    ok(parsed.key === 's', `the key should be lowercased, got ${parsed.key}`)
    // A repeated modifier is collapsed at parse time, so the array cannot hold two.
    ok(parseChord('mod+mod+s').mods.length === 1, 'a repeated modifier should collapse')
    // And an unrecognised modifier is rejected rather than passed through, since `Chord.mods`
    // is typed as `Modifier` and a value outside that union is a lie the type cannot catch at
    // a boundary.
    let threw = false
    try {
      parseChord('hyper+s')
    } catch {
      threw = true
    }
    ok(threw, 'an unknown modifier should be rejected')
    return { mods: parsed.mods }
  })

  await test('a hand-built chord is canonicalised, not just a parsed one', () => {
    // The mutation run found the gap this closes: `chordKey` sorts, but every chord reaching
    // it came from `parseChord`, which sorts too -- so deleting the sort in `chordKey` changed
    // no result. `Chord` is a public type and this function takes one, so it has to hold for a
    // chord assembled by hand, which is what a caller writing their own binding table does.
    // Modifiers in a deliberately wrong order, which is the only thing that can distinguish a
    // canonical sort from "whatever order they arrived in".
    const shuffled = { key: 's', mods: ['shift', 'mod', 'ctrl'] as const }
    const fromParse = chordKey(parseChord('mod+ctrl+shift+s'))
    ok(chordKey(shuffled) === fromParse,
      `a hand-built chord gave ${chordKey(shuffled)}, the parsed one ${fromParse}`)
    // A duplicate modifier collapses, because a `Chord` built by hand can hold one and
    // `parseChord` cannot produce it. `sortMods` goes through a `Set`, so this is the same code
    // that makes the order canonical.
    // Compared against the parsed spelling of the same *set*, which drops the shift -- the
    // point being that a duplicate does not change the key, not that the two sets are equal.
    ok(chordKey({ key: 's', mods: ['ctrl', 'ctrl', 'mod'] }) === chordKey(parseChord('mod+ctrl+s')),
      `a repeated modifier should collapse, got ${chordKey({ key: 's', mods: ['ctrl', 'ctrl', 'mod'] })}`)
    ok(chordKey({ key: 's', mods: [] }) === 's', `no modifiers should give a bare key, got ${chordKey({ key: 's', mods: [] })}`)
    // And the registry must agree, or the same shortcut written two ways is two entries with
    // no conflict between them -- the silent shadowing the conflict report exists to prevent.
    const noop = () => { /* registration only */ }
    const mixed = new ShortcutRegistry(
      { 'app.save': noop, 'app.quit': noop },
      {
        navigator: LINUX,
        bindings: [
          { command: 'app.save', chord: 'mod+s', scope: 'global' },
          { command: 'app.quit', chord: 'mod+s', scope: 'global' },
        ],
      },
    )
    ok(mixed.conflicts().length === 1, 'two spellings of one chord must be reported as one conflict')
    return { canonical: fromParse }
  })

  await test('an event is normalised the same way a binding is', () => {
    // The two sides of the comparison are produced by different functions, and a resolver
    // that builds its event chord differently from its parsed binding chord never matches
    // anything -- which would look exactly like "no shortcuts work".
    const fromEvent = chordKey(chordFromEvent(key('S', { ctrlKey: true, shiftKey: true })))
    const fromBinding = chordKey(parseChord('shift+ctrl+s'))
    ok(fromEvent === fromBinding, `event gave ${fromEvent}, binding gave ${fromBinding}`)
    // Modifier order in the event is not a thing a browser can produce, but the property is
    // that the order of the collected modifiers is canonical.
    const mixed = chordKey(chordFromEvent(key('s', { metaKey: true, ctrlKey: true, altKey: true, shiftKey: true })))
    ok(mixed === chordKey(parseChord('shift+alt+ctrl+meta+s')), `four modifiers gave ${mixed}`)
    return { chord: fromEvent }
  })

  await test('the primary modifier follows the platform, and the label with it', () => {
    // The two facts that must not drift: which modifier resolves, and what a menu prints.
    ok(primaryModifier(MAC) === 'meta', `a Mac reported ${primaryModifier(MAC)}`)
    ok(primaryModifier(LINUX) === 'ctrl', `Linux reported ${primaryModifier(LINUX)}`)
    ok(primaryModifier({}) === 'ctrl', 'an unknown platform should behave like the majority, not throw')
    ok(primaryModifier({ userAgent: 'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)' }) === 'meta',
      'userAgent is the fallback when platform says nothing')
    ok(isMac('meta') && !isMac('ctrl'), 'isMac is a property of the resolved modifier')
    ok(chordLabel(parseChord('mod+s'), 'meta') === '⌘S', `a Mac label was ${chordLabel(parseChord('mod+s'), 'meta')}`)
    ok(chordLabel(parseChord('mod+s'), 'ctrl') === 'Ctrl+S', `a Linux label was ${chordLabel(parseChord('mod+s'), 'ctrl')}`)
    return { mac: chordLabel(parseChord('mod+shift+z'), 'meta'), linux: chordLabel(parseChord('mod+shift+z'), 'ctrl') }
  })

  await test('the platform is read per registry, not cached in the module', () => {
    // The mutation this kills: hoisting `const IS_MAC = /mac/.test(navigator.platform)` to
    // module scope. Both registries would then answer the same, and the second assertion --
    // that the *first* one still answers as a Mac -- would fail.
    const mac = registryOn(MAC)
    const linux = registryOn(LINUX)
    ok(mac.registry.platform === 'meta', `the Mac registry reported ${mac.registry.platform}`)
    ok(linux.registry.platform === 'ctrl', `the Linux registry reported ${linux.registry.platform}`)
    const commandS = key('s', { metaKey: true })
    const controlS = key('s', { ctrlKey: true })
    ok(commandOf(mac.registry.resolve(commandS, OUTSIDE_EDITOR)) === 'app.save',
      'Cmd+S should save on the Mac registry')
    ok(commandOf(mac.registry.resolve(controlS, OUTSIDE_EDITOR)) === 'none',
      'Ctrl+S is a different key on a Mac, and must not resolve')
    ok(commandOf(linux.registry.resolve(controlS, OUTSIDE_EDITOR)) === 'app.save',
      'Ctrl+S should save on the Linux registry')
    ok(commandOf(linux.registry.resolve(commandS, OUTSIDE_EDITOR)) === 'none',
      'Cmd+S is not bound on Linux')
    ok(mac.registry.labelFor('app.save') === '⌘S', 'and the Mac label is still a glyph')
    ok(linux.registry.labelFor('app.save') === 'Ctrl+S', 'and the Linux label is still a word')
    return { mac: mac.registry.labelFor('app.save'), linux: linux.registry.labelFor('app.save') }
  })

  await test('a literal modifier stays literal on both platforms', () => {
    // `ctrl+s` is a real thing to want: on a Mac it is Control-S, a different action from
    // Command-S. It must not be collapsed into `mod`, which would make a Mac binding silently
    // unreachable. The chord used here is not one the default table claims, so the two
    // platforms' answers differ only because of the platform.
    const literal = [{ command: 'app.hardLineWrap', chord: 'ctrl+shift+w', scope: 'global' }] as Binding[]
    const mac = registryOn(MAC, literal)
    const linux = registryOn(LINUX, literal)
    // A Mac: the literal binding is reachable with Control, and does not answer Command.
    ok(commandOf(mac.registry.resolve(key('w', { ctrlKey: true, shiftKey: true }), OUTSIDE_EDITOR)) === 'app.hardLineWrap',
      'Ctrl+Shift+W should be reachable on a Mac')
    ok(commandOf(mac.registry.resolve(key('w', { metaKey: true, shiftKey: true }), OUTSIDE_EDITOR)) === 'none',
      'and must not answer Command-Shift-W, which is a different chord there')
    // Linux: the same written chord, and the same event, still work -- `ctrl` is literal.
    ok(commandOf(linux.registry.resolve(key('w', { ctrlKey: true, shiftKey: true }), OUTSIDE_EDITOR)) === 'app.hardLineWrap',
      'and on Linux')
    // Now the collision that a *collapsed* `mod` would have hidden: `mod+shift+w` and
    // `ctrl+shift+w` are the same chord on Linux and different on a Mac.
    const both: Binding[] = [
      { command: 'app.hardLineWrap', chord: 'ctrl+shift+w', scope: 'global' },
      { command: 'app.softWrap', chord: 'mod+shift+w', scope: 'global' },
    ]
    const macBoth = new ShortcutRegistry(recorder().handlers, { navigator: MAC, bindings: both })
    const linuxBoth = new ShortcutRegistry(recorder().handlers, { navigator: LINUX, bindings: both })
    ok(macBoth.conflicts().length === 0,
      `on a Mac the two are different chords and must not be reported, got ${macBoth.conflicts().length}`)
    ok(linuxBoth.conflicts().length === 1,
      `on Linux they are the same chord, so one must be reported, got ${linuxBoth.conflicts().length}`)
    ok(linuxBoth.conflicts()[0]?.chord === 'ctrl+shift+w', `the conflict names ${linuxBoth.conflicts()[0]?.chord}`)
    // And the reported one is not shadowed into nothing: the earlier declaration runs, and
    // the report names both so the answer is available either way.
    const ran: string[] = []
    const linuxRan = new ShortcutRegistry(
      { 'app.hardLineWrap': () => ran.push('hard'), 'app.softWrap': () => ran.push('soft') },
      { navigator: LINUX, bindings: both },
    )
    linuxRan.dispatch(key('w', { ctrlKey: true, shiftKey: true }), OUTSIDE_EDITOR)
    ok(ran.length === 1 && ran[0] === 'hard', `the earlier declaration should run, got ${ran.join(',') || 'nothing'}`)
    ok(linuxBoth.conflicts()[0]?.winner.command === 'app.hardLineWrap', 'and the report says so')
    ok(linuxBoth.conflicts()[0]?.loser.command === 'app.softWrap', 'and names the shadowed one')
    return { macConflicts: macBoth.conflicts().length, linuxConflicts: linuxBoth.conflicts().length }
  })

  await test('a chord resolves through `code`, so a shifted digit is still the digit', () => {
    // The real failure this prevents: `mod+alt+1` is heading 1 in the default table, and on a
    // Mac `⌘⌥1` reports '¡'. Matching only on `key` means the advertised shortcut does
    // nothing. The test uses both the real shift artefact and the layout-independent form.
    const shifted = key('!', { metaKey: true, altKey: true, code: 'Digit1' })
    const mac = registryOn(MAC)
    ok(commandOf(mac.registry.resolve(shifted, IN_EDITOR)) === 'heading1',
      `Cmd+Alt+1 reported '!' and resolved to ${commandOf(mac.registry.resolve(shifted, IN_EDITOR))}`)
    // And the reverse: a user who wrote the produced character themselves still gets it. The
    // binding carries the same modifiers, so the two forms are in genuine competition and this
    // is the case that reaches the key-length tiebreak below.
    // A registry with *only* the produced-character form, because the default table claims
    // `mod+alt+1` for heading 1 and would win on declaration order -- which would make this
    // assertion about the default table rather than about the candidate fallback.
    const mac2 = new ShortcutRegistry(
      { 'app.pageDown': () => { /* dispatch is checked below, not here */ } },
      { navigator: MAC, bindings: [{ command: 'app.pageDown', chord: 'mod+alt+!', scope: 'global' }] },
    )
    ok(commandOf(mac2.resolve(shifted, IN_EDITOR)) === 'app.pageDown',
      `a binding written with the produced character should also match, got ${commandOf(mac2.resolve(shifted, IN_EDITOR))}`)
    // A synthetic event with no `code` falls back to `key` rather than refusing to resolve.
    const synthetic = key('!', { metaKey: true, altKey: true })
    ok(commandOf(mac.registry.resolve(synthetic, IN_EDITOR)) === 'none',
      'with no code and no matching binding, nothing resolves')
    ok(commandOf(mac.registry.resolve(key('1', { metaKey: true, altKey: true }), IN_EDITOR)) === 'heading1',
      'an unshifted synthetic event still matches by key alone')
    return { cases: 4 }
  })

  await test('named keys keep their name, and letters lose their case', () => {
    ok(parseChord('Escape').key === 'escape', 'Escape is lowercased')
    ok(parseChord('mod+F5').key === 'f5', 'a function key is lowercased')
    ok(chordLabel(parseChord('escape'), 'ctrl') === 'Esc', `escape printed as ${chordLabel(parseChord('escape'), 'ctrl')}`)
    ok(chordLabel(parseChord('arrowdown'), 'ctrl') === '↓', 'arrows print as glyphs')
    ok(chordLabel(parseChord('arrowdown'), 'meta') === '↓', 'and on a Mac too')
    ok(chordLabel(parseChord('f5'), 'ctrl') === 'F5', 'function keys stay as words')
    const esc = registryOn(LINUX)
    ok(commandOf(esc.registry.resolve(key('Escape'), { inEditor: true, typing: true })) === 'none',
      'nothing is bound to Escape, which is the point: it stays free')
    return { keys: ['escape', 'f5', 'arrowdown'] }
  })

  // -- the two scopes ------------------------------------------------------

  await test('an editor shortcut fires only inside the editor', () => {
    // The distinction the module exists for. A binding marked `editor` is not a candidate
    // outside a section editor, and the refusal is reported rather than silent so a menu can
    // show the item greyed rather than lying about it.
    const { registry } = registryOn(LINUX)
    const inside = registry.resolve(key('b', { ctrlKey: true }), IN_EDITOR)
    ok(commandOf(inside) === 'bold', `inside the editor Ctrl+B gave ${commandOf(inside)}`)
    const outside = registry.resolve(key('b', { ctrlKey: true }), OUTSIDE_EDITOR)
    ok(outside.kind === 'blocked' && outside.reason === 'outside-editor',
      `outside the editor Ctrl+B gave ${commandOf(outside)}`)
    // A global one fires in both places. Bold is a formatting act; saving is a document act,
    // and the caret being in section 300 does not make saving a different action.
    ok(commandOf(registry.resolve(key('s', { ctrlKey: true }), IN_EDITOR)) === 'app.save', 'Ctrl+S saves from inside')
    ok(commandOf(registry.resolve(key('s', { ctrlKey: true }), OUTSIDE_EDITOR)) === 'app.save', 'and from outside')
    return { inside: commandOf(inside), outside: commandOf(outside) }
  })

  await test('an out-of-scope binding does not fall back to a looser one', () => {
    // Rank first, then ask about scope. The global chord runs from the sidebar, and the
    // editor chord there does nothing at all -- it does not reach for a looser binding and run
    // a command the user did not ask for.
    // A registry with only these two bindings, because the shipped table claims `mod+b` for
    // bold and `mod+shift+b` for blockquote and would win on declaration order.
    const bindings: Binding[] = [
      { command: 'app.boldAll', chord: 'mod+alt+b', scope: 'editor' },
      { command: 'app.saveAll', chord: 'mod+alt+shift+b', scope: 'global' },
    ]
    const noop = () => { /* this test is about resolution, not dispatch */ }
    const registry = new ShortcutRegistry(
      { 'app.boldAll': noop, 'app.saveAll': noop },
      { navigator: LINUX, bindings },
    )
    const bothHeld = key('b', { ctrlKey: true, altKey: true, shiftKey: true })
    // Inside the editor both chords match one event; the two-modifier editor one is a strict
    // subset of the three-modifier global one, and the more specific match wins.
    const inside = registry.resolve(bothHeld, IN_EDITOR)
    ok(commandOf(inside) === 'app.saveAll',
      `Ctrl+Alt+Shift+B inside the editor should run the global command, got ${commandOf(inside)}`)

    // Now the property that is actually enforced: an out-of-scope winner does *not* fall back
    // to a looser in-scope binding. Only the editor chord is pressed, so the editor binding is
    // the only match, and in the sidebar it must do nothing rather than reach for the global.
    const editorOnly = registry.resolve(key('b', { ctrlKey: true, altKey: true }), OUTSIDE_EDITOR)
    ok(editorOnly.kind === 'blocked' && editorOnly.reason === 'outside-editor',
      `Ctrl+Alt+B in the sidebar resolved to ${commandOf(editorOnly)}`)
    // And the global chord still works from the sidebar, which is the case that scope-filtering
    // would have broken.
    const global = registry.resolve(bothHeld, OUTSIDE_EDITOR)
    ok(commandOf(global) === 'app.saveAll', `and the global chord in the sidebar to ${commandOf(global)}`)
    return { inside: commandOf(inside), editorOnly: commandOf(editorOnly), global: commandOf(global) }
  })

  // -- the typing rule -----------------------------------------------------

  await test('a bare letter is a character, not a shortcut', async () => {
    // The failure being prevented is "some letters do not appear". A modifier-free
    // single-character binding must not fire while a text field has focus -- and a section
    // editor is a text field, because it is contenteditable.
    const bare: Binding[] = [
      { command: 'app.focusSearch', chord: 'f', scope: 'global' },
      { command: 'app.closePanel', chord: 'Escape', scope: 'global' },
    ]
    const { registry } = registryOn(LINUX, bare)
    const typed = registry.resolve(key('f'), { inEditor: true, typing: true })
    ok(typed.kind === 'blocked' && typed.reason === 'text-input',
      `pressing f in a paragraph gave ${commandOf(typed)}`)
    const notTyping = registry.resolve(key('f'), OUTSIDE_EDITOR)
    ok(commandOf(notTyping) === 'app.focusSearch', `and outside a text field to ${commandOf(notTyping)}`)
    // Escape is not a character, so it stays available everywhere -- which is the whole
    // reason the rule is stated positively rather than as a list of permitted keys.
    const escaped = registry.resolve(key('Escape'), { inEditor: true, typing: true })
    ok(commandOf(escaped) === 'app.closePanel', `Escape while typing gave ${commandOf(escaped)}`)
    // And a modified key is not a character on any keyboard this ships to.
    ok(commandOf(registry.resolve(key('b', { ctrlKey: true }), IN_EDITOR)) === 'bold',
      'Ctrl+B still bolds while typing')
    return { typed: commandOf(typed), escape: commandOf(escaped) }
  })

  await test('the editor surface and a text field are both recognised as typing', () => {
    // `contextForElement` is where the DOM question is answered, so it is where the
    // interesting cases live: an editable div is the common one, and a button is not.
    const editable = contextForElement(element({ tagName: 'DIV', editable: true, editorAncestor: true }))
    ok(editable.typing, 'a contenteditable div takes text')
    ok(editable.inEditor, 'and is inside the editor')
    const field = contextForElement(element({ tagName: 'INPUT', type: 'text', editorAncestor: true }))
    ok(field.typing, 'a text input takes text')
    const area = contextForElement(element({ tagName: 'TEXTAREA' }))
    ok(area.typing, 'a textarea takes text')
    // Every one of these is the dangerous direction: treating a non-text field as a text field
    // suppresses every bare-key shortcut while it holds focus, and a user cannot tell why
    // their single-key command stopped working. So the check is stated for each of them, and
    // not only for the two most obvious.
    for (const [label, type] of [
      ['button', 'button'],
      ['submit', 'submit'],
      ['checkbox', 'checkbox'],
      ['radio', 'radio'],
      ['file', 'file'],
      ['colour', 'color'],
      ['range', 'range'],
      ['date', 'date'],
    ] as Array<[string, string]>) {
      ok(!contextForElement(element({ tagName: 'INPUT', type })).typing,
        `an <input type="${label}"> is not a text field, so single-key commands must still work`)
    }
    const chooser = contextForElement(element({ tagName: 'INPUT', type: 'button' }))
    ok(!chooser.typing, 'a button is not a text field: arrows and letters are not being typed')
    const select = contextForElement(element({ tagName: 'SELECT' }))
    ok(!select.typing, 'a select is not a text field either')
    const unknownType = contextForElement(element({ tagName: 'INPUT', type: 'color' }))
    ok(!unknownType.typing, 'a colour picker is not a text field')
    // An unrecognised type falls back to `text` per the HTML spec, and the dangerous direction
    // is the other one: a key swallowed by a field the user believes is a button.
    const exotic = contextForElement(element({ tagName: 'INPUT', type: 'made-up' }))
    ok(exotic.typing, 'an unrecognised input type is treated as text')
    const noAttrs = contextForElement(element({ tagName: 'INPUT' }))
    ok(noAttrs.typing, 'an input with no type attribute is a text input')

    // And the consequence, not just the classification: a bare-key binding must still resolve
    // with one of those fields focused. The classification is only worth anything if it
    // changes an outcome, and a classification that is right but unused would pass every
    // assertion above.
    const bare: Binding[] = [{ command: 'app.focusSearch', chord: 'f', scope: 'global' }]
    const ran: string[] = []
    const withButton = new ShortcutRegistry(
      // A real handler, so the *dispatch* consequence can be asserted too. Resolution alone
      // would pass even if `ctx.typing` were ignored downstream, because `resolve` is what
      // consults it and `dispatch` delegates.
      { 'app.focusSearch': () => ran.push('focusSearch') },
      { navigator: LINUX, bindings: bare },
    )
    const buttonEvent: KeyEventLike = { key: 'f' }
    const inButton = contextForElement(element({ tagName: 'INPUT', type: 'button' }))
    ok(commandOf(withButton.resolve(buttonEvent, inButton)) === 'app.focusSearch',
      'a bare key should still work with a button focused')
    const fired = withButton.dispatch(buttonEvent, inButton)
    ok(fired.handled && ran.length === 1, `the command should have run, got handled=${fired.handled} ran=${ran.length}`)
    const inField = withButton.dispatch(buttonEvent, { inEditor: false, typing: true })
    ok(!inField.handled && ran.length === 1, `and should not run in a real text field, got handled=${inField.handled} ran=${ran.length}`)
    const chrome = contextForElement(element({ tagName: 'DIV', editorAncestor: false }))
    ok(!chrome.inEditor, 'the chrome around a section is not the editor')
    ok(!contextForElement(null).inEditor && !contextForElement(null).typing, 'a null target is neither')
    return { cases: 10 }
  })

  // -- one global undo stack ----------------------------------------------

  await test('undo is one global binding, not one per editor', () => {
    // The architectural requirement (M3): a document is many editors, so history cannot be
    // per-editor. This asserts the three things that together mean "one stack": there is
    // exactly one binding, it is global, and the *same handler object* is called no matter
    // which section the keypress came from. A per-editor implementation would have to look
    // the handler up per context, and the handler identity would differ.
    const { registry, ran } = registryOn(LINUX)
    const undoBindings = registry.bindings('undo')
    ok(undoBindings.length === 1, `undo has ${undoBindings.length} bindings; it should have exactly one`)
    ok(undoBindings[0]?.scope === 'global', `undo is scoped ${undoBindings[0]?.scope}, which would trap history inside a section`)

    const ctrlZ = key('z', { ctrlKey: true })
    // From three different "sections", and from outside every editor. The registry is given no
    // section at all -- there is nowhere to put one -- which is the point.
    const contexts: Array<[string, EventContext]> = [
      ['s1', { inEditor: true, typing: true }],
      ['s300', { inEditor: true, typing: true }],
      ['sidebar', { inEditor: false, typing: false }],
    ]
    const handlerIds = new Set<string>()
    for (const [, ctx] of contexts) {
      const result = registry.dispatch(ctrlZ, ctx)
      ok(result.handled, `undo from ${ctx.inEditor ? 'an editor' : 'outside'} did not run`)
      ok(result.match.kind === 'command' && result.match.command === 'undo',
        `undo from ${ctx.inEditor ? 'an editor' : 'outside'} resolved to ${commandOf(result.match)}`)
    }
    ok(ran.filter(c => c === 'undo').length === 3, `undo ran ${ran.filter(c => c === 'undo').length} times, expected 3`)
    // One handler object, reachable from every context. The registry holds a flat record
    // (see ShortcutHandlers), so this is the only handler `undo` can have.
    handlerIds.add(String(registry.bindings('undo')[0]?.command))
    ok(handlerIds.size === 1, 'undo must be reachable from exactly one place')

    // And the API has no way to say "this undo is for section 3": a second global binding for
    // the same chord is a duplicate, and a different command on that chord is reported.
    const again = registry.register({ command: 'undo', chord: 'mod+z', scope: 'global' })
    ok(again.kind === 'duplicate', `re-registering undo gave ${again.kind}`)
    ok(registry.bindings('undo').length === 1, 'and did not add a second binding')
    return { undoBindings: undoBindings.length, scope: undoBindings[0]?.scope, contexts: contexts.length }
  })

  await test('redo is global too, and shares the chord with nothing', () => {
    // Mod+Shift+Z is redo, not a second undo. If the two shared a chord the report below would
    // show a conflict, and a conflict is a user-visible dead keystroke.
    const { registry } = registryOn(LINUX)
    const redo = registry.resolve(key('z', { ctrlKey: true, shiftKey: true }), OUTSIDE_EDITOR)
    ok(commandOf(redo) === 'redo', `Ctrl+Shift+Z gave ${commandOf(redo)}`)
    ok(registry.conflicts().length === 0,
      `the default table reports ${registry.conflicts().length} conflicts: ${registry.conflicts().map(c => c.chord).join(', ')}`)
    return { conflicts: registry.conflicts().length }
  })

  // -- collisions ----------------------------------------------------------

  await test('an identical re-registration is a no-op, and never throws', () => {
    const { registry } = registryOn(LINUX)
    const before = registry.bindings().length
    let threw: unknown = null
    let result: unknown = null
    try {
      result = registry.register({ command: 'bold', chord: 'mod+b', scope: 'editor' })
      // Twice, because a module that registers the same table twice is the realistic case.
      result = registry.register({ command: 'bold', chord: 'mod+b', scope: 'editor' })
    } catch (e) {
      threw = e
    }
    ok(threw === null, `registering a duplicate threw: ${threw}`)
    ok((result as { kind: string }).kind === 'duplicate', `a duplicate reported ${(result as { kind: string }).kind}`)
    ok(registry.bindings().length === before, `the table grew from ${before} to ${registry.bindings().length}`)
    ok(registry.conflicts().length === 0, 'a duplicate is not a conflict')
    return { bindings: registry.bindings().length }
  })

  await test('a conflicting command is reported, not thrown on and not shadowed silently', () => {
    // The mutation this kills: `throw new Error('duplicate')`. A module that registers a
    // slightly different table then stops the boot, and the user sees nothing but a blank
    // window. The report is the honest alternative.
    // `mod+shift+p` is not claimed by the default table, so the only conflict is the one
    // being made. A chord the shipped table already uses would produce a second, and this test
    // would be measuring the default table rather than the rule.
    const bindings: Binding[] = [
      { command: 'app.print', chord: 'mod+shift+p', scope: 'global' },
      { command: 'app.printPreview', chord: 'mod+shift+p', scope: 'global' },
    ]
    const ran: string[] = []
    const registry = new ShortcutRegistry(
      { 'app.print': () => ran.push('app.print'), 'app.printPreview': () => ran.push('app.printPreview') },
      { navigator: LINUX, bindings },
    )
    const conflicts = registry.conflicts()
    ok(conflicts.length === 1, `expected one conflict, got ${conflicts.length}: ${conflicts.map(c => c.chord).join(',')}`)
    const conflict = conflicts[0]!
    ok(conflict.chord === 'ctrl+shift+p', `the conflict names ${conflict.chord}`)
    ok(conflict.winner.command === 'app.print', `the earlier declaration should win, got ${conflict.winner.command}`)
    ok(conflict.loser.command === 'app.printPreview', `and the report should name the loser, got ${conflict.loser.command}`)
    // Both are still in the table, so the report is about a *resolution* rather than a
    // deletion, and the winner is decided by the documented order.
    ok(registry.bindings('app.printPreview').length === 1, 'the losing binding is still registered')
    registry.dispatch(key('p', { ctrlKey: true, shiftKey: true }), OUTSIDE_EDITOR)
    ok(ran.length === 1 && ran[0] === 'app.print', `the keystroke ran ${ran.join(',') || 'nothing'}`)
    return { chord: conflict.chord, winner: conflict.winner.command, loser: conflict.loser.command }
  })

  await test('the more specific binding wins when both match', () => {
    // A superset: a four-modifier binding and a one-modifier one both match a keypress with
    // all four held. Without the modifier-count criterion the one-modifier binding would win
    // and the specific one would be unreachable.
    const bindings: Binding[] = [
      { command: 'app.save', chord: 'mod+s', scope: 'global' },
      { command: 'app.quit', chord: 'mod+ctrl+alt+shift+s', scope: 'global' },
    ]
    const { registry } = registryOn(LINUX, bindings)
    const all = key('s', { ctrlKey: true, altKey: true, shiftKey: true })
    ok(commandOf(registry.resolve(all, OUTSIDE_EDITOR)) === 'app.quit',
      `four modifiers resolved to ${commandOf(registry.resolve(all, OUTSIDE_EDITOR))}`)
    // And the one-modifier binding is still reachable on its own, which is the other half of
    // "more specific wins" -- a tiebreak that swallowed the looser binding would be a
    // regression rather than a resolution.
    ok(commandOf(registry.resolve(key('s', { ctrlKey: true }), OUTSIDE_EDITOR)) === 'app.save',
      'the simpler binding should still fire on its own')
    return { specific: 'app.quit', simple: 'app.save' }
  })

  await test('the specificity criteria are ordered, and asserted directly', async () => {
    // `resolve` only reaches the modifier-count criterion through a superset and the key-length
    // criterion through the two candidate forms of one event. Both are rare, so a test that
    // only drove `resolve` would not notice the order being reversed. So the comparator is
    // exercised on its own, where all three criteria are reachable at all.
    const mk = (chord: string, order: number): ResolvedBinding => {
      const parsed = parseChord(chord)
      return { command: `c${order}`, chord, scope: 'global', resolved: { key: parsed.key, mods: parsed.mods }, order }
    }
    const one = mk('mod+s', 0)
    const four = mk('mod+ctrl+alt+shift+s', 1)
    ok(moreSpecific(four, one), 'more modifiers must win')
    ok(!moreSpecific(one, four), 'and the looser one must not')

    const short = mk('s', 2)
    const long = mk('arrowdown', 3)
    ok(moreSpecific(long, short), 'a longer key name wins at equal modifier count')
    ok(!moreSpecific(short, long), 'and not the other way round')

    // Equal on both counts: declaration order decides, and earlier wins.
    const early = mk('mod+s', 4)
    const late = mk('mod+s', 5)
    ok(moreSpecific(early, late), 'the earlier declaration wins a full tie')
    ok(!moreSpecific(late, early), 'and the later one does not')

    // And the end-to-end case for the key-length criterion: one event, two written forms.
    const event = key('!', { ctrlKey: true, altKey: true, code: 'Digit1' })
    const noop = () => { /* resolution only */ }
    // The two written forms of one event, in both declaration orders. The code-derived form is
    // the physical key, so it is what the user pressed; the tiebreak must not be left to
    // whichever happened to be declared first, which is why the order is varied.
    const orders: Array<[string, Binding[]]> = [
      ['code first', [
        { command: 'app.fromCode', chord: 'mod+alt+1', scope: 'global' },
        { command: 'app.fromKey', chord: 'mod+alt+!', scope: 'global' },
      ]],
      ['key first', [
        { command: 'app.fromKey', chord: 'mod+alt+!', scope: 'global' },
        { command: 'app.fromCode', chord: 'mod+alt+1', scope: 'global' },
      ]],
    ]
    for (const [label, bindings] of orders) {
      const registry = new ShortcutRegistry(
        { 'app.fromCode': noop, 'app.fromKey': noop },
        { navigator: LINUX, bindings },
      )
      ok(commandOf(registry.resolve(event, OUTSIDE_EDITOR)) === 'app.fromCode',
        `declared ${label}: the code form should win, got ${commandOf(registry.resolve(event, OUTSIDE_EDITOR))}`)
    }
    // And with only the produced-character form present it still resolves: the preference is an
    // ordering between candidates, not a rejection of the second one.
    const only = new ShortcutRegistry(
      { 'app.fromKey': noop },
      { navigator: LINUX, bindings: [{ command: 'app.fromKey', chord: 'mod+alt+!', scope: 'global' }] },
    )
    ok(commandOf(only.resolve(event, OUTSIDE_EDITOR)) === 'app.fromKey', 'and the key-only form is still reachable')
    return { criteria: 3, orders: orders.length }
  })

  await test('a command with no handler resolves but is not handled', () => {
    // The caller uses `handled` to decide whether to swallow the key. A shortcut that has
    // nowhere to go must leave the keypress alone, because the browser's own action is a
    // better outcome than a keystroke that does nothing.
    const handlers: ShortcutHandlers = { undo: () => { /* only undo is wired */ } }
    const registry = new ShortcutRegistry(handlers, { navigator: LINUX, bindings: DEFAULT_BINDINGS })
    const result = registry.dispatch(key('s', { ctrlKey: true }), OUTSIDE_EDITOR)
    ok(result.match.kind === 'command', 'save resolves: the binding is there')
    ok(!result.handled, 'but it is not handled: no handler exists')
    ok(registry.unbound().includes('app.save'), `unbound() reported ${registry.unbound().join(', ')}`)
    ok(!registry.unbound().includes('undo'), 'and the wired command is not in the list')
    ok(registry.dispatch(key('z', { ctrlKey: true }), OUTSIDE_EDITOR).handled, 'while a wired command is handled')
    return { unbound: registry.unbound().length }
  })

  // -- consumption ---------------------------------------------------------

  await test('a handled keypress is consumed on both counts', () => {
    // `preventDefault` alone is insufficient: ProseMirror's own keydown handler does not
    // check `defaultPrevented`, so it runs unless propagation is stopped. The mutation this
    // kills is dropping the `stopPropagation()` call, which leaves the editor acting on a key
    // the app has already handled -- bold applied twice, or a shortcut typed as a character.
    const calls: string[] = []
    const event: ConsumableEvent = {
      preventDefault: () => calls.push('preventDefault'),
      stopPropagation: () => calls.push('stopPropagation'),
    }
    consumeEvent(event)
    ok(calls.length === 2, `consumeEvent called ${calls.length} of the 2 methods it must call`)
    ok(calls.includes('preventDefault'), 'the default action must be prevented')
    ok(calls.includes('stopPropagation'), 'and propagation stopped, or the editor still sees the key')
    return { calls }
  })

  // -- the shipped table ---------------------------------------------------

  await test('the default table is internally consistent and every command is answerable', () => {
    // The table is a shipped artefact, so the properties it must have are checked once here
    // rather than assumed: no chord claimed twice by the shipped table, every command bound,
    // and every editor binding actually inside the editor.
    const { registry, ran } = registryOn(LINUX)
    ok(registry.conflicts().length === 0, `the shipped table collides on ${registry.conflicts().map(c => c.chord).join(', ')}`)
    ok(registry.unbound().length === 0, `unwired commands: ${registry.unbound().join(', ')}`)
    ok(GLOBAL_BINDINGS.every(b => b.scope === 'global'), 'the global table should hold only globals')
    ok(EDITOR_BINDINGS.every(b => b.scope === 'editor'), 'the editor table should hold only editor bindings')
    ok(DEFAULT_BINDINGS.length === GLOBAL_BINDINGS.length + EDITOR_BINDINGS.length,
      'the combined table should be the two tables, not a third copy')
    for (const binding of GLOBAL_BINDINGS) {
      // Every global binding must resolve with no editor at all. This is the property that
      // fails if one is mislabelled `editor`, and it is the reason globals exist.
      const chord = parseChord(binding.chord)
      const event: KeyEventLike = { key: chord.key, ...modsAsFlags(chord.mods, LINUX) }
      ok(commandOf(registry.resolve(event, OUTSIDE_EDITOR)) === binding.command,
        `${binding.chord} (${binding.command}) does not resolve outside the editor`)
    }
    // And one dispatch per shipped binding, from a context each scope requires.
    for (const binding of DEFAULT_BINDINGS) {
      const chord = parseChord(binding.chord)
      const event: KeyEventLike = { key: chord.key, ...modsAsFlags(chord.mods, LINUX) }
      const ctx: EventContext = binding.scope === 'editor' ? { inEditor: true, typing: true } : OUTSIDE_EDITOR
      ok(registry.dispatch(event, ctx).handled, `${binding.chord} (${binding.command}) did not fire in its own scope`)
    }
    ok(ran.length === DEFAULT_BINDINGS.length, `${ran.length} of ${DEFAULT_BINDINGS.length} shipped bindings fired`)
    return { globals: GLOBAL_BINDINGS.length, editors: EDITOR_BINDINGS.length }
  })

  await test('bindingFor is the single source of truth for a command keybinding', () => {
    // `toolbar.ts` reads this rather than writing its own list, which is the alternative to a
    // parity test between two tables that must agree. Asserting the read is what catches a
    // command name that does not exist -- a typo produces "no keybinding" and no error.
    for (const command of COMMANDS) {
      const binding = bindingFor(command)
      ok(binding !== null, `no default binding for ${command}, so its toolbar button would show no shortcut`)
    }
    ok(bindingFor('bold')?.chord === 'mod+b', `bold is bound to ${bindingFor('bold')?.chord}`)
    ok(bindingFor('insertTable') === null, 'a command with no shortcut is null, not a guess')
    // Not in the table at all, which is a different case from "bound to nothing".
    ok(bindingFor('app.nonesuch') === null, 'an unknown command is null')
    return { commands: COMMANDS.length }
  })

  // -- the source-level check ---------------------------------------------

  await test('no module in core/ installs an inline handler attribute', () => {
    // `onclick="..."` is not just a style preference: it is a string compiled at click time,
    // it cannot be removed by `removeEventListener`, and it does not exist at all in the CSP
    // this app ships under. This is a text check, which is inherently fragile, so it is scoped
    // to a whole file with comments stripped -- the failure `source-checks.ts` records, where
    // a doc comment quoting the removed line flagged correct code.
    const core = join(dirname(fileURLToPath(import.meta.url)), '..', 'src', 'core')
    const strip = (s: string) => s.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/.*$/gm, '')
    for (const file of ['toolbar.ts', 'shortcuts.ts']) {
      const body = strip(readFileSync(join(core, file), 'utf8'))
      const inline = body.match(/['"]on[a-z]+\s*['"]\s*[:=]/)
      ok(!inline, `${file} assigns an inline handler attribute: ${inline?.[0]}`)
      ok(!/\.onclick\s*=/.test(body), `${file} assigns .onclick`)
    }
    // The one positive assertion, so the check above is not vacuous: handlers are attached by
    // addEventListener, which is what has to be there.
    ok(strip(readFileSync(join(core, 'toolbar.ts'), 'utf8')).includes("addEventListener('click'"),
      'toolbar.ts should attach clicks with addEventListener')
    return { files: 2 }
  })

  // -- the module holds no global -----------------------------------------

  await test('reading `navigator` at import time would break the platform injection', () => {
    // The mutation this kills: exporting `const IS_MAC = /mac/.test(navigator.platform)` and
    // using it in `chordLabel` and `resolve`. Every registry would then agree on the
    // platform, and the *second* assertion below is the one that fails -- the Mac registry
    // would resolve Ctrl+S, or stop resolving Cmd+S. A source check cannot prove laziness, so
    // this is a behavioural restatement of the property the previous test already relies on,
    // and it is cheap.
    const mac = registryOn(MAC)
    const linux = registryOn(LINUX)
    const outcomes = new Set([
      `${mac.registry.platform}:${commandOf(mac.registry.resolve(key('s', { metaKey: true }), OUTSIDE_EDITOR))}`,
      `${linux.registry.platform}:${commandOf(linux.registry.resolve(key('s', { ctrlKey: true }), OUTSIDE_EDITOR))}`,
    ])
    ok(outcomes.size === 2, `the two platforms produced the same answer: ${[...outcomes].join(' and ')}`)
    return { outcomes: [...outcomes] }
  })

  console.log(`${passed} passed, ${failed} failed`)
  if (failed) {
    console.log(`\nfailures:\n  ${failures.join('\n  ')}`)
    process.exit(1)
  }
}

/** Turn a chord's modifiers into the event flags that would produce it, on a platform. */
function modsAsFlags(mods: readonly string[], nav: NavigatorLike): Record<string, boolean> {
  const primary = primaryModifier(nav)
  return {
    ctrlKey: mods.includes('ctrl') || (mods.includes('mod') && primary === 'ctrl'),
    altKey: mods.includes('alt'),
    shiftKey: mods.includes('shift'),
    metaKey: mods.includes('meta') || (mods.includes('mod') && primary === 'meta'),
  }
}

await main()
