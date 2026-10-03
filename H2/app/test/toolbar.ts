/**
 * The formatting toolbar: what the editor has active, which commands are missing, and what
 * the buttons say.
 *
 * # Why the state is tested against a real Tiptap editor, not a fake
 *
 * The brief asked for a fake editor. A real one turned out to be constructible in Node --
 * `test/math.ts` has been building them without a DOM for two milestones -- and that changes
 * what the test is worth, so this uses the real thing and says why.
 *
 * `toolbarState` derives everything from `editor.isActive` and from whether a command exists
 * on the editor. A hand-written fake answers both, and would therefore agree with the
 * implementation by construction: a fake whose `isActive` returns whatever the test needs
 * proves that the code calls `isActive`, and nothing about whether it asks the right question.
 * The real editor can disagree. It did, twice, while this file was being written:
 *
 * - `can().chain().focus().toggleBold().run()` is **false** in a headless editor, because
 *   `focus()` cannot take focus without a DOM. A test written against the `can()` form would
 *   have concluded Bold was unsupported in every section. With a real editor and a real
 *   `can()` call this is visible immediately.
 * - `can().chain().notACommand().run()` **throws** rather than returning false, so a
 *   "supports" check written as a capability probe needs a `try`, and the `typeof` check this
 *   module uses does not.
 *
 * The real editor is also what makes the equation and table commands meaningful: they are
 * Holonomy's own, added by `core/math.ts`, and a fake would not have them at all.
 *
 * # What is Node-tested and what is not
 *
 * Node-tested: {@link toolbarState}, {@link toolbarLabel}, {@link runItem},
 * {@link hasCommand}, the item table's internal consistency, and a source-level check that
 * neither module uses an inline handler attribute.
 *
 * Browser-only: {@link renderToolbar}, {@link renderItem} and {@link updateToolbar}. They
 * build elements, and there is no DOM in Node. They are deliberately thin -- every decision
 * they make arrives from the functions above -- and the browser suite in `test/scroll.ts` is
 * where the rendered result is asserted. The one assertion that *is* made here about the
 * rendering is structural, and it is the one that would rot: that no item ends up with an
 * `onclick`.
 *
 * Run: node --experimental-strip-types test/toolbar.ts
 */

import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { Editor } from '@tiptap/core'
import StarterKit from '@tiptap/starter-kit'
import { Table, TableRow, TableCell, TableHeader } from '@tiptap/extension-table'
import { Highlight } from '@tiptap/extension-highlight'
import { BlockMath, InlineMath } from '../src/core/math.ts'
import { bindingFor, chordLabel, EDITOR_BINDINGS, parseChord, primaryModifier } from '../src/core/shortcuts.ts'
import {
  activeName,
  buttonSpec,
  EMPTY_STATE,
  GROUP_ATTRIBUTES,
  ITEM_GROUPS,
  runItem,
  shortcutName,
  toolbarGroups,
  TOOLBAR_ROOT_ATTRIBUTES,
  toolbarLabel,
  toolbarState,
  TOOLBAR_ITEMS,
  type EditorLike,
  type ShortcutLookup,
  type ToolbarItem,
} from '../src/core/toolbar.ts'

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

/**
 * The lookup the app would build, built here from `shortcuts.ts` for real.
 *
 * # Why this is wired up rather than stubbed
 *
 * Because `toolbar.ts` cannot value-import `shortcuts.ts` -- see the note on
 * {@link ShortcutLookup} -- so the wiring is a real risk rather than a type error. A lookup
 * that returned a made-up chord would let a broken wiring pass every label assertion in this
 * file. This one goes through `bindingFor` and `chordLabel`, the same two functions
 * `main.ts` will call, so the assertions below are about the real chain.
 */
function lookupOn(platform: string): ShortcutLookup {
  const primary = primaryModifier({ platform })
  return {
    chordFor: command => bindingFor(command)?.chord ?? null,
    format: written => chordLabel(parseChord(written), primary),
  }
}

const MAC = lookupOn('MacIntel')
const LINUX = lookupOn('Linux x86_64')
/** The resolved platform, for the one assertion that needs the chord rather than a lookup. */
const LINUX_PLATFORM = primaryModifier({ platform: 'Linux x86_64' })

/**
 * The extension list `main.ts` builds a section editor from.
 *
 * # Why this is written here rather than imported
 *
 * Because `buildExtensions` is a local function in `main.ts` and importing a module for its
 * side effects would boot the whole application. A copy is normally the thing `DOCTRINE.md` §8
 * warns about, and it is duplicated deliberately: the property being checked is "the toolbar's
 * command names are ones the shipped editor has", and that cannot be checked without
 * constructing the editor. If `main.ts` changes its extension list, this test fails -- which is
 * the correct outcome, because the toolbar would then be showing buttons for commands that no
 * longer exist.
 */
const SHIPPED_EXTENSIONS = [
  StarterKit.configure({ undoRedo: false }),
  Table,
  TableRow,
  TableCell,
  TableHeader,
  InlineMath,
  BlockMath,
]

/**
 * A real editor, headless.
 *
 * Content is given as JSON rather than an HTML string because Tiptap's HTML path needs
 * `window` to build a document from (`elementFromString` throws without it). The JSON path
 * does not, which is why this works in Node at all.
 */
function editorWith(content: unknown, extra: unknown[] = []): Editor {
  return new Editor({
    extensions: [...SHIPPED_EXTENSIONS, ...extra] as never[],
    content: content as never,
  })
}

const PARAGRAPH = {
  type: 'doc',
  content: [{ type: 'paragraph', content: [{ type: 'text', text: 'hello' }] }],
}

/**
 * Whether a name is a mark or a node in a real editor's schema.
 *
 * # Why this is a schema lookup and not a hardcoded list
 *
 * Because a list here would be a second copy of the schema, and `DOCTRINE.md` §8 is about
 * that arrangement: it fails at the worst moment, comparing a stale answer against a fresh one.
 * The schema is the authority, it is reachable without a DOM through `editor.state.schema`, and
 * asking it is the difference between "the toolbar names a thing that exists" and "the toolbar
 * names a thing I wrote down".
 */
function knownNodeOrMarkName(editor: Editor, name: string): boolean {
  const schema = editor.state.schema
  return name in schema.marks || name in schema.nodes
}

const HEADING = (level: number) => ({
  type: 'doc',
  content: [{ type: 'heading', attrs: { level }, content: [{ type: 'text', text: 'H' }] }],
})

/** Adapt a real editor to the interface the toolbar declares. */
function adapt(editor: Editor): EditorLike {
  return editor as unknown as EditorLike
}

/**
 * A document's serialised JSON.
 *
 * `editor.getHTML()` needs `window` (it runs the DOM serialiser), so every assertion here reads
 * `getJSON()`. The marks are in it: an empty selection with a stored mark still serialises the
 * mark onto the text once something is typed, and `isActive` covers the transient case.
 */
function doc(editor: Editor): string {
  return JSON.stringify(editor.getJSON())
}

/** The item with this id, or a thrown error naming what is there instead. */
function item(id: string): ToolbarItem {
  const found = TOOLBAR_ITEMS.find(i => i.id === id)
  if (!found) throw new Error(`no toolbar item \`${id}\`; there are ${TOOLBAR_ITEMS.map(i => i.id).join(', ')}`)
  return found
}

async function main() {
  console.log('toolbar: item metadata, editor state, availability, labels')
  console.log('='.repeat(72))

  // -- the item table ------------------------------------------------------

  await test('every required item is present, once', () => {
    // The brief's list, checked literally. A dropped item is invisible in a screenshot until
    // someone notices it is missing, so the count and the ids are both asserted.
    const required = [
      'bold', 'italic', 'underline', 'strike', 'code', 'highlight',
      'h1', 'h2', 'h3',
      'bulletList', 'orderedList', 'blockquote', 'codeBlock',
      'inlineMath', 'blockMath', 'table', 'horizontalRule',
    ]
    const ids = TOOLBAR_ITEMS.map(i => i.id)
    const missing = required.filter(id => !ids.includes(id))
    ok(missing.length === 0, `missing items: ${missing.join(', ')}`)
    const extra = ids.filter(id => !required.includes(id))
    ok(extra.length === 0, `unexpected items: ${extra.join(', ')}`)
    ok(ids.length === new Set(ids).size, `duplicate ids: ${ids.join(', ')}`)
    ok(TOOLBAR_ITEMS.length === required.length, `${TOOLBAR_ITEMS.length} items, expected ${required.length}`)
    return { items: TOOLBAR_ITEMS.length }
  })

  await test('every item declares what a button needs to be built and explained', () => {
    for (const i of TOOLBAR_ITEMS) {
      ok(typeof i.id === 'string' && i.id.length > 0, `item ${i.id} has no id`)
      ok(typeof i.label === 'string' && i.label.length > 0, `${i.id} has no label, so it has no accessible name`)
      // A path string, checked for the shape rather than the artwork. An empty or
      // whitespace-only `d` renders as nothing, which is a button with no visible content and
      // no error -- exactly the "blank box" failure the module header is about.
      ok(typeof i.icon === 'string' && i.icon.trim().length > 0, `${i.id} has no icon path`)
      ok(/^[Mm][\d\s.,-]/.test(i.icon.trim()), `${i.id}'s icon does not look like a path: ${i.icon.slice(0, 20)}`)
      ok(typeof i.command === 'string' && i.command.length > 0, `${i.id} names no command`)
      ok(i.kind === 'toggle' || i.kind === 'action', `${i.id} has kind ${i.kind}`)
    }
    // Toggles and actions are not interchangeable, and the split is what decides whether a
    // button gets `aria-pressed`. Asserted on the items where it is unambiguous.
    for (const id of ['bold', 'italic', 'h1', 'bulletList', 'codeBlock', 'table', 'horizontalRule', 'inlineMath']) {
      const expected = id === 'table' || id === 'horizontalRule' || id === 'inlineMath' ? 'action' : 'toggle'
      ok(item(id).kind === expected, `${id} should be a ${expected}, is a ${item(id).kind}`)
    }
    return { items: TOOLBAR_ITEMS.length }
  })

  await test('the group table covers every item exactly once', () => {
    // `renderToolbar` builds from `ITEM_GROUPS`, not from `TOOLBAR_ITEMS`, so an item missing
    // from a group is a button that never renders -- and one in two groups renders twice.
    //
    // This test is also what makes `renderToolbar`'s `throw` reachable in a useful sense. The
    // mutation run found the throw itself unobservable -- nothing in the shipped tables trips
    // it, so changing it to `continue` passed -- and the reason to keep it is that this test
    // fails the moment the two tables disagree, which is the only way that guard is ever needed.
    // A guard with no failing test behind it and no failing table behind it is a comment.
    const grouped = ITEM_GROUPS.flat()
    const ids = TOOLBAR_ITEMS.map(i => i.id)
    const ungrouped = ids.filter(id => !grouped.includes(id))
    ok(ungrouped.length === 0, `never rendered, because no group names them: ${ungrouped.join(', ')}`)
    const twice = grouped.filter((id, i) => grouped.indexOf(id) !== i)
    ok(twice.length === 0, `in more than one group: ${twice.join(', ')}`)
    ok(grouped.length === ids.length, `${grouped.length} grouped against ${ids.length} items`)
    // The order groups are declared in is the order they appear, so it must match the table.
    ok(grouped.join() === ids.join(), 'the group order and the item order disagree')
    // The reverse direction, which is what would trip the guard: a group naming an id that is
    // not an item. Asserted by construction, and then *behaviourally* below.
    const known = new Set(ids)
    const dangling = grouped.filter(id => !known.has(id))
    ok(dangling.length === 0, `a group names ids with no item: ${dangling.join(', ')}`)
    // And every group is non-empty, so a stray `[]` cannot produce a separator with nothing
    // in it -- which is a visible gap in the toolbar's shape.
    for (const [i, group] of ITEM_GROUPS.entries()) {
      ok(group.length > 0, `group ${i} is empty, which renders an empty separator`)
    }
    return { groups: ITEM_GROUPS.length }
  })

  await test('a group naming a missing item is loud, not a hole in the toolbar', () => {
    // # This test exists because the mutation run found the guard unobservable.
    //
    // `renderToolbar` originally did the `find` and the `throw` inline, inside a loop that
    // needs a DOM. Replacing the `throw` with `continue` changed no result in any suite: the
    // shipped tables agree, so the line is never reached, and nothing in Node can reach the
    // renderer. A guard no test can reach is a comment.
    //
    // The resolution moved into `toolbarGroups`, which returns a value, and this drives it with
    // a deliberately broken table. The shipped tables are checked for agreement separately --
    // the two together are what make the guard load-bearing rather than decorative.
    const resolved = toolbarGroups()
    ok(resolved.length === ITEM_GROUPS.length, `expected ${ITEM_GROUPS.length} groups, got ${resolved.length}`)
    ok(resolved.flat().length === TOOLBAR_ITEMS.length, `expected ${TOOLBAR_ITEMS.length} items, got ${resolved.flat().length}`)
    // The shipped tables resolve to the items themselves, in order.
    ok(resolved.flat()[0] === item('bold'), 'the first rendered item should be bold')
    // Now the failure the guard exists for.
    let threw = ''
    try {
      toolbarGroups([['bold', 'noSuchItem']])
    } catch (e: any) {
      threw = e.message
    }
    // # Why the message is asserted, not just that it threw.
    //
    // The mutation run found a version of this guard whose message named only the id. The throw
    // still happened and every other test still passed -- because nothing asserted the text. So
    // a boot-time failure said "group names x" and left the reader to work out that two tables
    // had to agree, which is the whole diagnosis.
    //
    // `DOCTRINE.md` §2's corollary: a failure message must point at the cause, and a message
    // that could equally come from three other mistakes is not pointing at one.
    ok(threw.includes('noSuchItem'), `the message should name the offending id, got "${threw}"`)
    ok(threw.includes('ITEM_GROUPS') && threw.includes('TOOLBAR_ITEMS'),
      `the message should name both tables, since that is the actual disagreement, got "${threw}"`)
    // And a group naming an item that exists but is not grouped still renders it, so the guard
    // is about *unknown* ids rather than about ordering.
    const extra = toolbarGroups([['italic']])
    ok(extra.length === 1 && extra[0]?.length === 1 && extra[0]![0] === item('italic'), 'a valid custom group should resolve')
    return { groups: resolved.length, items: resolved.flat().length }
  })

  await test('every command name is one the shipped editor actually has', () => {
    // The test that a fake editor could not do. If `main.ts` drops an extension, the
    // corresponding button renders permanently disabled and nothing anywhere says why.
    const editor = editorWith(PARAGRAPH)
    try {
      const missing = TOOLBAR_ITEMS.filter(i => {
        const table = (editor as unknown as { commands: Record<string, unknown> }).commands
        return typeof table[i.command] === 'undefined'
      })
      // `toggleHighlight` is the expected one: `main.ts` installs neither Highlight nor
      // Underline explicitly, and StarterKit 3.30.3 does not include them. That is a real
      // finding about the shipped editor, not a defect in the toolbar -- and the disabled
      // rendering is exactly what it should produce. Asserted rather than skipped, so a
      // future change to the extension list is a deliberate edit here.
      const expected = ['highlight']
      const unexpected = missing.map(i => i.id).filter(id => !expected.includes(id))
      ok(unexpected.length === 0,
        `the shipped editor has no command for: ${unexpected.join(', ')}. Either the extension is not ` +
        'installed in main.ts, or the command name is wrong.')
      ok(missing.length === expected.length,
        `expected ${expected.join(', ')} to be unavailable, got ${missing.map(i => i.id).join(', ') || 'nothing missing'}`)
      return { unavailable: missing.map(i => i.id) }
    } finally {
      editor.destroy()
    }
  })

  // -- state ---------------------------------------------------------------

  await test('an action is never in the active set, whatever the editor says', () => {
    // The half of the active/unsupported split that `toolbarState` gets wrong silently. A
    // mutation that made every item active passed the "is bold on?" assertions, because bold
    // genuinely was on -- the false ones were the actions, which have no on state at all, and
    // an action in `active` renders as a permanently-pressed "Insert table".
    //
    // So this is asserted as a set property over every item, and against an editor where the
    // answer differs per item rather than being uniformly one value.
    const editor = editorWith(PARAGRAPH, [Highlight])
    try {
      editor.commands.toggleBold()
      const state = toolbarState(adapt(editor))
      for (const i of TOOLBAR_ITEMS) {
        if (i.kind === 'action') {
          ok(!state.active.has(i.id), `${i.id} is an action and must not be in the active set`)
        }
      }
      // And the toggles are not all one value, so the loop above is not passing vacuously: at
      // least one is on and at least one is off in this editor state.
      const toggles = TOOLBAR_ITEMS.filter(i => i.kind === 'toggle')
      const on = toggles.filter(i => state.active.has(i.id))
      const off = toggles.filter(i => !state.active.has(i.id))
      ok(on.length > 0, 'expected at least one active toggle in this editor state')
      ok(off.length > 0, 'and at least one inactive, or the action check proves nothing')
      ok(on.map(i => i.id).includes('bold'), `bold should be the active one, got ${on.map(i => i.id).join(', ')}`)
      // The unsupported set is the other half of the split, and it must be the exact
      // complement of "has a command" -- an item in both sets is rendered pressed *and*
      // disabled, which is a contradiction a user cannot resolve.
      for (const i of TOOLBAR_ITEMS) {
        ok(!(state.active.has(i.id) && state.unsupported.has(i.id)),
          `${i.id} is both active and unsupported, which renders as pressed and disabled at once`)
      }
      return { on: on.length, off: off.length, actions: TOOLBAR_ITEMS.filter(i => i.kind === 'action').length }
    } finally {
      editor.destroy()
    }
  })

  await test('a toggle is active exactly when the editor says so', () => {
    // The property the whole toolbar rests on, checked against a real editor rather than a
    // stub: toggling bold makes bold active and leaves italic alone, and turning it off
    // clears it again. A fake answering `true` unconditionally would pass the first half and
    // fail the second, which is the point of using the real thing.
    const editor = editorWith(PARAGRAPH)
    try {
      let state = toolbarState(adapt(editor))
      ok(!state.active.has('bold'), 'bold should be off in a plain paragraph')
      ok(!state.active.has('italic'), 'and italic')

      editor.commands.toggleBold()
      state = toolbarState(adapt(editor))
      ok(state.active.has('bold'), 'bold should be on after toggling it')
      ok(!state.active.has('italic'), 'and toggling bold must not activate italic')

      editor.commands.toggleItalic()
      state = toolbarState(adapt(editor))
      ok(state.active.has('bold') && state.active.has('italic'), 'both should be on')

      editor.commands.toggleBold()
      state = toolbarState(adapt(editor))
      ok(!state.active.has('bold'), 'and off again after a second toggle')
      ok(state.active.has('italic'), 'without disturbing italic')
      return { checks: 6 }
    } finally {
      editor.destroy()
    }
  })

  await test('every item derives a lower-case schema name from its command', () => {
    // # Why this is asserted directly rather than through `toolbarState`.
    //
    // `activeName` is the step between a Tiptap command name and a schema name, and a wrong
    // answer produces `isActive('false')`, which is indistinguishable from "this toggle is
    // off". The mutation run found that removing the `setHeading` branch changed nothing --
    // because `'setHeading'.replace(/^set/, '')` is `Heading` and lower-casing the first letter
    // already yields `heading`, so the branch was dead code rather than a guard. It is deleted.
    //
    // What is asserted here is the property that makes it safe to delete: the one rule covers
    // every item, with no exceptions, so there is no branch to get out of step.
    for (const i of TOOLBAR_ITEMS) {
      if (i.kind !== 'toggle') continue
      const name = activeName(i)
      ok(name.length > 0, `${i.id} derives an empty active name`)
      // Only the *first* letter, not the whole name: `bulletList` and `inlineMath` are
      // legitimately camelCase in the schema. What is not legitimate is a leading capital,
      // because every Tiptap command is written `toggleBulletList` and the schema says
      // `bulletList`.
      ok(name.charAt(0) === name.charAt(0).toLowerCase(),
        `${i.id}'s active name is "${name}"; a leading capital makes isActive answer false forever, so the button would never press`)
    }
    const editor = editorWith(PARAGRAPH, [Highlight])
    try {
      for (const i of TOOLBAR_ITEMS) {
        if (i.kind !== 'toggle') continue
        const name = activeName(i)
        const known = knownNodeOrMarkName(editor, name)
        ok(known, `${i.id} derives "${name}", which is not a mark or node the shipped schema has, so its button would never press`)
      }
      // The two derivations that are not a prefix rule, stated so the values cannot drift.
      ok(activeName(item('bold')) === 'bold', `bold should derive "bold", got ${activeName(item('bold'))}`)
      ok(activeName(item('h2')) === 'heading', `setHeading should derive "heading", got ${activeName(item('h2'))}`)
      ok(activeName(item('bulletList')) === 'bulletList', `bulletList, got ${activeName(item('bulletList'))}`)
      return { toggles: TOOLBAR_ITEMS.filter(i => i.kind === 'toggle').length }
    } finally {
      editor.destroy()
    }
  })

  await test('each heading is active only at its own level', () => {
    // The reason `setHeading` needs special handling: all three items share one command, so
    // asking `isActive('setHeading')` would mark all three pressed in a Heading 2. Asserted
    // at every level, because the failure at the wrong level is what a "close enough" check
    // would miss.
    for (const level of [1, 2, 3]) {
      const editor = editorWith(HEADING(level))
      try {
        const state = toolbarState(adapt(editor))
        for (const candidate of [1, 2, 3]) {
          const id = `h${candidate}`
          ok(state.active.has(id) === (candidate === level),
            `in a Heading ${level}, ${id} reported ${state.active.has(id)}`)
        }
      } finally {
        editor.destroy()
      }
    }
    return { levels: 3 }
  })

  await test('block toggles follow the node the caret is in', () => {
    // Real node types, not a mocked `isActive`. Inside a code block, the code-block button is
    // pressed and the list button is not -- which is the state a toolbar must show, and the
    // reason availability is not derived from `can()`.
    const editor = editorWith(PARAGRAPH)
    try {
      editor.commands.toggleCodeBlock()
      const state = toolbarState(adapt(editor))
      ok(state.active.has('codeBlock'), 'codeBlock should be on inside one')
      ok(!state.active.has('blockquote'), 'and blockquote off')
      editor.commands.toggleBulletList()
      const after = toolbarState(adapt(editor))
      ok(after.active.has('bulletList'), 'bulletList on after toggling it')
      ok(!after.active.has('codeBlock'), 'and leaving the code block, codeBlock off')
      return { checks: 4 }
    } finally {
      editor.destroy()
    }
  })

  await test('reading the state does not change the document', () => {
    // # The purity property, asserted rather than claimed.
    //
    // `toolbarState` is called on every transaction, so a `toggleX` where an `isActive` was
    // meant would toggle the user's formatting on every keystroke -- and the toolbar would
    // look correct, because it would be reading the state it just changed. This compares the
    // serialised document before and after, which is the only observation that catches it.
    const editor = editorWith(PARAGRAPH)
    try {
      const before = JSON.stringify(editor.getJSON())
      for (let i = 0; i < 5; i++) toolbarState(adapt(editor))
      const after = JSON.stringify(editor.getJSON())
      ok(before === after, `reading the state changed the document:\n  before ${before}\n  after  ${after}`)
      ok(doc(editor).includes('hello'), 'and the text is still there')
      return { calls: 5 }
    } finally {
      editor.destroy()
    }
  })

  // -- availability --------------------------------------------------------

  await test('a missing command makes its item unsupported, not absent', () => {
    // The module header's central claim, checked against a real editor built two ways: the
    // shipped list (no Highlight) and one with Highlight installed. The item must appear in
    // the same `unsupported` set in both cases' shape -- present, not missing -- so that
    // `renderToolbar` renders it disabled and the toolbar keeps its width.
    const without = editorWith(PARAGRAPH)
    const with_ = editorWith(PARAGRAPH, [Highlight])
    try {
      const a = toolbarState(adapt(without))
      const b = toolbarState(adapt(with_))
      ok(a.unsupported.has('highlight'), 'highlight is unsupported without the extension')
      ok(!b.unsupported.has('highlight'), 'and supported with it')
      // The item is in the table either way. This is the assertion that distinguishes
      // "disabled" from "hidden": the id is there to be rendered.
      ok(TOOLBAR_ITEMS.some(i => i.id === 'highlight'), 'the highlight item must still be in the table')
      ok(ITEM_GROUPS.flat().includes('highlight'), 'and still in a group, so it still renders')
      // And an installed extension does not make the rest of the table change shape.
      const onlyOne = [...b.unsupported].filter(id => id !== 'highlight')
      ok(onlyOne.length === 0, `installing Highlight should not change other items, but these changed: ${onlyOne.join(', ')}`)

      // What the duplicate-extension warning was actually telling us: StarterKit 3.30.3
      // supplies `toggleUnderline` on its own, so the underline button works even though
      // `main.ts` never lists Underline. Recorded here because it is a fact about the shipped
      // editor that is easy to assume the other way round -- and the toolbar's underline button
      // is *not* one of the disabled ones, which would surprise anyone reasoning from
      // `buildExtensions()` alone.
      ok(!a.unsupported.has('underline'), 'StarterKit should already provide toggleUnderline')
      return { without: [...a.unsupported], with: [...b.unsupported] }
    } finally {
      without.destroy()
      with_.destroy()
    }
  })

  await test('an absent or destroyed editor marks everything unavailable', () => {
    // The state of the toolbar for the whole of a document's lifetime outside a section. Every
    // button would do nothing, so every button is disabled -- and the caller does not need a
    // separate "not ready" branch that could disagree with the buttons.
    for (const [label, editor] of [
      ['null', null],
      ['undefined', undefined],
    ] as Array<[string, EditorLike | null | undefined]>) {
      const state = toolbarState(editor)
      ok(state.unsupported.size === TOOLBAR_ITEMS.length, `for ${label}, ${state.unsupported.size} of ${TOOLBAR_ITEMS.length} are unsupported`)
      ok(state.active.size === 0, `and none should be active for ${label}`)
    }
    // # Why only Highlight is added and not Underline as well.
    //
    // `Highlight` is the extension whose absence is the point of the test. `Underline` is
    // *already* in StarterKit 3.30.3, so adding it again prints Tiptap's "Duplicate extension
    // names" warning on every run -- noise in a suite whose output is meant to be read. What
    // the duplicate actually reveals is that StarterKit supplies `toggleUnderline` without
    // `main.ts` asking, which is worth knowing and is asserted below rather than left as a
    // warning in the log.
    //
    // A destroyed editor is the same situation, and it is a real one: the registry destroys
    // editors on unmount as the user scrolls. It is also the *harder* of the two cases in
    // Node, and the reason is worth recording.
    //
    // `editor.isDestroyed` is `true` for a live headless editor in Tiptap 3.30.3 -- the getter
    // is `editorView?.isDestroyed ?? true` and `editorView` is null without a DOM. So a
    // liveness check written against it would call this editor destroyed *before* the
    // `destroy()` below, and every assertion in this file would pass for the wrong reason.
    // That is why the module reads `editor.commands` instead, and it is asserted here so the
    // distinction cannot be lost: a live headless editor must be usable, and only then is a
    // destroyed one unusable.
    const editor = editorWith(PARAGRAPH)
    const live = toolbarState(adapt(editor))
    ok(live.unsupported.size !== TOOLBAR_ITEMS.length,
      'a live headless editor reports itself destroyed; the liveness check is reading the wrong signal')
    ok(editor.isDestroyed === true,
      'this documents the Tiptap quirk: isDestroyed is true for a live headless editor, which is why the module does not use it')
    const allGood = live.unsupported.size
    editor.destroy()
    const state = toolbarState(adapt(editor))
    ok(state.unsupported.size === TOOLBAR_ITEMS.length,
      `a destroyed editor should disable everything, but only ${state.unsupported.size} of ${TOOLBAR_ITEMS.length} are unsupported (a live one had ${allGood} unavailable)`)
    ok(state.active.size === 0, 'and activate nothing')
    ok(EMPTY_STATE.unsupported.size === TOOLBAR_ITEMS.length, 'and EMPTY_STATE should agree')
    return { items: TOOLBAR_ITEMS.length, liveUnavailable: allGood }
  })

  await test('a command that is missing cannot be run', () => {
    // `runItem` returns false rather than throwing, because the toolbar renders a disabled
    // button and a click on it must not take the editor down. The throw is reserved for a
    // genuine wiring error, and there is not one here.
    const editor = editorWith(PARAGRAPH)
    try {
      ok(runItem(adapt(editor), item('highlight')) === false, 'running an unsupported item should return false')
      ok(runItem(adapt(editor), item('bold')) === true, 'and a supported one should run')
      // The mark is a stored mark on an empty selection, so it appears in the state rather
      // than in the serialised text; `isActive` is the honest witness here.
      ok(editor.isActive('bold'), 'which should have applied a strong mark')
      ok(runItem(null, item('bold')) === false, 'and with no editor it should return false')
    } finally {
      editor.destroy()
    }
    return { checks: 4 }
  })

  // -- running -------------------------------------------------------------

  await test('running an item applies its command with its own arguments', () => {
    // The three shapes a command takes: none, a fixed value, and a value the app supplies.
    // Each is checked by looking at the document, because "the handler was called" is not the
    // property -- "the document changed the way the button promised" is.
    const editor = editorWith(PARAGRAPH)
    try {
      // A toggle, no arguments.
      runItem(adapt(editor), item('bold'))
      ok(editor.isActive('bold'), 'bold should have applied a strong mark')

      // A fixed argument: heading 2, which is `{ level: 2 }` and not a bare command. The node
      // *type* is the assertion -- "an h2 appeared" would be satisfied by a command that ran
      // with the wrong level and also by one that ran with no argument at all.
      runItem(adapt(editor), item('h2'))
      const afterHeading = editor.getJSON() as { content?: Array<{ type?: string; attrs?: Record<string, unknown> }> }
      ok(afterHeading.content?.[0]?.type === 'heading', `heading 2 should have produced a heading, got ${afterHeading.content?.[0]?.type}`)
      ok(afterHeading.content?.[0]?.attrs?.level === 2, `at level 2, got ${afterHeading.content?.[0]?.attrs?.level}`)

      // A table, with the item's own dimensions.
      runItem(adapt(editor), item('table'))
      const json = doc(editor)
      ok(json.includes('"table"'), 'a table should have been inserted')
      ok(json.includes('"tableRow"'), 'with rows in it')
      return { checks: 5 }
    } finally {
      editor.destroy()
    }
  })

  await test('an item needing input runs only with a value', () => {
    // The equation buttons. Inserting an empty equation would produce a node rendering an
    // error, so the no-value case must be a refusal rather than a call.
    const editor = editorWith(PARAGRAPH)
    try {
      ok(runItem(adapt(editor), item('inlineMath')) === false, 'with no TeX it should refuse')
      ok(!doc(editor).includes('inlineMath'), 'and insert nothing')
      ok(runItem(adapt(editor), item('inlineMath'), 'x^2') === true, 'with TeX it should run')
      ok(doc(editor).includes('inlineMath'), 'and insert an inline equation')
      ok(doc(editor).includes('x^2'), 'carrying the TeX it was given')

      ok(runItem(adapt(editor), item('blockMath')) === false, 'block math with no TeX should refuse too')
      ok(runItem(adapt(editor), item('blockMath'), '\\int_0^1') === true, 'and run with it')
      ok(doc(editor).includes('mathBlock'), 'inserting a block equation')
      return { checks: 7 }
    } finally {
      editor.destroy()
    }
  })

  // -- labels --------------------------------------------------------------

  await test('a label is the item name, and the binding goes in the tooltip', () => {
    // The name and the shortcut are different things for different readers. A screen reader
    // announcing "Bold Ctrl+B" is announcing a string the user cannot say to a voice-control
    // system; the tooltip is where a mouse user looks.
    const bold = toolbarLabel(item('bold'), LINUX)
    ok(bold.name === 'Bold', `the accessible name should be "Bold", got "${bold.name}"`)
    ok(bold.title === 'Bold (Ctrl+B)', `the tooltip should carry the binding, got "${bold.title}"`)
    // And the platform spelling, from the shortcut registry's own table.
    ok(toolbarLabel(item('bold'), MAC).title === 'Bold (⌘B)', 'and a Mac glyph on a Mac')
    // An item with no binding still gets a name, and a tooltip that is just the name.
    const table = toolbarLabel(item('table'), LINUX)
    ok(table.name === 'Table', 'the table button should still be named')
    ok(!/\(.+\)$/.test(table.title), `and should not claim a shortcut it does not have: "${table.title}"`)
    return { bold: bold.title, table: table.title }
  })

  await test('the shortcut name is derived for marks and stated for headings', async () => {
    // The two namespaces, and the one place a rule cannot be trusted. `toggleBold` is `bold`
    // by a prefix rule; all three headings are `setHeading`, so the level has to be stated or
    // H1 and H2 would show the same shortcut. This asserts both halves, and that the stated
    // ones are real bindings in the registry rather than plausible-looking names.
    ok(shortcutName(item('bold')) === 'bold', `bold should map to "bold", got ${shortcutName(item('bold'))}`)
    ok(shortcutName(item('italic')) === 'italic', 'italic to "italic"')
    ok(shortcutName(item('bulletList')) === 'bulletList', 'bulletList to "bulletList"')
    ok(shortcutName(item('h1')) === 'heading1', `h1 to "heading1", got ${shortcutName(item('h1'))}`)
    ok(shortcutName(item('h2')) === 'heading2', `h2 to "heading2", got ${shortcutName(item('h2'))}`)
    ok(shortcutName(item('h3')) === 'heading3', `h3 to "heading3", got ${shortcutName(item('h3'))}`)

    // The stated names must be real. A typo here produces a button with no shortcut in its
    // tooltip and no error, which is the failure this exists to catch.
    //
    // The command name, on its own, must resolve to a *toggle* in the shortcut registry.
    // `EDITOR_BINDINGS` is the registry's own list of editor bindings, and a toolbar item
    // whose command is not in it has no chord -- so the tooltip comes back bare. The check is
    // here because it is the one that actually constrains the table, rather than a comment.
    const editorCommands = new Set(EDITOR_BINDINGS.map(b => b.command))
    for (const i of TOOLBAR_ITEMS) {
      if (i.kind !== 'toggle') continue
      ok(editorCommands.has(shortcutName(i)),
        `${i.id} maps to "${shortcutName(i)}", which is not an editor binding; the registry's own list is the authority`)
    }

    // Only the *toggles* are required to have a binding. An action that opens a dialog --
    // insert table, set an equation -- has no default chord, and that is a decision rather
    // than a gap: there is no single key that means "ask me for a table's dimensions".
    for (const i of TOOLBAR_ITEMS) {
      if (i.kind !== 'toggle') continue
      const name = shortcutName(i)
      ok(bindingFor(name) !== null,
        `${i.id} maps to shortcut command "${name}", which is not in the registry; its tooltip would be missing a shortcut`)
    }
    // And the actions are consistent about it: none claims a shortcut, so no tooltip shows
    // one. A dialog-opening action that advertised `Ctrl+Shift+T` would be a promise the
    // toolbar keeps and the user has no way to know is the same one.
    for (const i of TOOLBAR_ITEMS) {
      if (i.kind !== 'action') continue
      ok(bindingFor(shortcutName(i)) === null,
        `${i.id} is an action but ${shortcutName(i)} is bound in the registry; one of the two is wrong`)
    }
    // And the headings must be *distinct*, which is the whole reason they are stated.
    const headingKeys = [1, 2, 3].map(l => chordLabel(parseChord(bindingFor(shortcutName(item(`h${l}`)))!.chord), LINUX_PLATFORM))
    ok(new Set(headingKeys).size === 3, `the three headings show ${headingKeys.join(', ')}`)
    return { bold: shortcutName(item('bold')), h1: shortcutName(item('h1')), headings: headingKeys }
  })

  await test('the keybinding shown comes from the shortcut registry', () => {
    // # Why this is a real assertion and not a consequence of the wiring.
    //
    // `toolbarLabel` is handed a `ShortcutLookup` rather than importing one, because a value
    // import of `shortcuts.ts` cannot be loaded in Node (see the note on `ShortcutLookup`). So
    // the two *could* be wired wrongly, and the failure is silent: a lookup reading a different
    // table produces plausible-looking tooltips that do not match what the keys do.
    //
    // What is asserted here is that the chain the app will build -- `bindingFor` then
    // `chordLabel` -- produces the right string, for both platforms, and that every bound item
    // gets a *different* binding. A lookup that returned a constant would pass the first
    // assertion and fail the second, which is why both are here.
    ok(toolbarLabel(item('bold'), LINUX).title === 'Bold (Ctrl+B)',
      `on Linux bold should read "Bold (Ctrl+B)", got "${toolbarLabel(item('bold'), LINUX).title}"`)
    ok(toolbarLabel(item('bold'), MAC).title === 'Bold (⌘B)',
      `on a Mac it should be the glyph, got "${toolbarLabel(item('bold'), MAC).title}"`)
    ok(toolbarLabel(item('italic'), MAC).title === 'Italic (⌘I)',
      `and italic should be distinct from it, got "${toolbarLabel(item('italic'), MAC).title}"`)

    // Every item that has a binding must show a *distinct* one, and no two may collide. A
    // lookup reading a table where everything is `mod+b` would pass the bold assertion and
    // fail this.
    const titles = TOOLBAR_ITEMS.map(i => toolbarLabel(i, LINUX).title)
    const bound = titles.filter(t => t.includes('('))
    ok(bound.length >= 10, `expected many bound items, got ${bound.length}: ${titles.join(' | ')}`)
    ok(new Set(bound).size === bound.length, `two items claim the same tooltip: ${bound.join(' | ')}`)

    // And the tooltips must be spelled for a platform, never in the portable `mod` form. A
    // tooltip reading "Bold (mod+b)" is the symptom of a lookup that formats without
    // substituting, and it is the failure this catches.
    for (const t of titles) {
      ok(!/\(mod/.test(t), `a tooltip is not platform-spelled: "${t}"`)
    }
    return { bound: bound.length, mac: toolbarLabel(item('bold'), MAC).title }
  })

  await test('a lookup that knows nothing claims no shortcut rather than a wrong one', () => {
    // The default. `main.ts` has to pass a real lookup for tooltips to carry bindings, and if
    // it forgets, the result should be a bare name -- never a raw `mod+b` shown to a user.
    for (const i of TOOLBAR_ITEMS) {
      const { name, title } = toolbarLabel(i)
      ok(title === name, `${i.id}'s default title should be just its name, got "${title}"`)
    }
    // A lookup that returns a chord but cannot render it is the same situation: showing
    // "Bold (mod+b)" would be worse than showing nothing.
    const broken: ShortcutLookup = { chordFor: () => 'mod+b', format: () => '' }
    ok(toolbarLabel(item('bold'), broken).title === 'Bold', 'an unrenderable chord should fall back to the plain name')
    return { items: TOOLBAR_ITEMS.length }
  })

  // -- the source-level check ---------------------------------------------

  // -- what a button is ---------------------------------------------------

  await test('a toggle always has aria-pressed, and an action never does', () => {
    // # This is the test the first version of the module did not have.
    //
    // The decisions used to live inside `renderItem` as conditional `setAttribute` calls, and
    // a mutation run found that eleven deliberate breaks to the rendering -- dropping
    // `aria-pressed`, dropping `disabled`, dropping `aria-label`, inverting the pressed value,
    // letting a disabled button run -- produced *no* failure, because every one of them is a
    // plausible-looking call and there is no DOM in Node to observe it. The decisions are
    // values now, so they can be asserted.
    const editor = editorWith(PARAGRAPH, [Highlight])
    try {
      editor.commands.toggleBold()
      const state = toolbarState(adapt(editor))
      for (const i of TOOLBAR_ITEMS) {
        const spec = buttonSpec(i, state, LINUX)
        if (i.kind === 'toggle') {
          // Present on every toggle, *including* when false. The mutation this kills is
          // setting it only when active, which leaves a stale `aria-pressed="true"` on a
          // button that has been toggled off.
          ok('aria-pressed' in spec.attributes, `${i.id} is a toggle but has no aria-pressed`)
          const expected = state.active.has(i.id) ? 'true' : 'false'
          ok(spec.attributes['aria-pressed'] === expected,
            `${i.id}'s aria-pressed is "${spec.attributes['aria-pressed']}", expected "${expected}"`)
        } else {
          // An action has no on state, so claiming one is a lie to a screen reader.
          ok(!('aria-pressed' in spec.attributes),
            `${i.id} is an action but claims aria-pressed="${spec.attributes['aria-pressed']}"`)
        }
      }
      // The positive half: the editor really does have bold on, so the attribute has to say so
      // rather than the test passing because nothing was ever pressed.
      ok(buttonSpec(item('bold'), state, LINUX).attributes['aria-pressed'] === 'true', 'bold should be pressed')
      ok(buttonSpec(item('italic'), state, LINUX).attributes['aria-pressed'] === 'false', 'and italic should not be')
      return { toggles: TOOLBAR_ITEMS.filter(i => i.kind === 'toggle').length }
    } finally {
      editor.destroy()
    }
  })

  await test('an unavailable item is disabled, still named, and not runnable', () => {
    // # Disabled, not hidden -- the module's central claim, asserted on the value the renderer
    // applies rather than on a screenshot.
    //
    // Three things have to hold together, and each has a failure that looks fine on its own:
    // the button keeps its place in the toolbar (so its shape does not change between
    // sections), it is visibly and semantically unavailable, and clicking it does nothing.
    const editor = editorWith(PARAGRAPH)
    try {
      const state = toolbarState(adapt(editor))
      const highlight = item('highlight')
      ok(state.unsupported.has('highlight'), 'highlight should be unavailable in the shipped editor')
      const spec = buttonSpec(highlight, state, LINUX)
      // Not hidden: the item is still in the table and still in a group, so it still renders.
      ok(ITEM_GROUPS.flat().includes('highlight'), 'the item must still be in a group or it does not render at all')
      // Disabled, and announced as such. `disabled` takes it out of the tab order; the
      // `aria-*` pair is what a screen reader says, and webkit2gtk is not consistent about
      // the `disabled` attribute on its own.
      ok('disabled' in spec.attributes, 'an unavailable item must be disabled')
      ok(spec.attributes['aria-disabled'] === 'true', 'and carry aria-disabled')
      // Named anyway. A disabled control with no name is a blank rectangle to a screen reader,
      // and a user cannot find out which command is unavailable.
      ok(spec.attributes['aria-label'] === 'Highlight', `it should still be named, got "${spec.attributes['aria-label']}"`)
      ok((spec.attributes.title ?? '').startsWith('Highlight'), 'and keep its tooltip')
      // And it must not run. The browser would ignore the click anyway, but relying on that is
      // relying on a detail this module can state itself.
      ok(!spec.runnable, 'an unavailable item must not be runnable')
      // The control half: a supported item is runnable, so the flag is not simply false
      // everywhere.
      ok(buttonSpec(item('bold'), state, LINUX).runnable, 'a supported item must be runnable')
      // And when the extension *is* installed, the same item is no longer disabled -- which is
      // what makes the disabled rendering a statement about the editor rather than about the item.
      const full = editorWith(PARAGRAPH, [Highlight])
      try {
        const ok2 = toolbarState(adapt(full))
        const spec2 = buttonSpec(highlight, ok2, LINUX)
        ok(!('disabled' in spec2.attributes), 'with Highlight installed it must not be disabled')
        ok(spec2.runnable, 'and must be runnable')
        ok(!('aria-disabled' in spec2.attributes), 'and must not claim to be disabled')
      } finally {
        full.destroy()
      }
      return { unavailable: [...state.unsupported] }
    } finally {
      editor.destroy()
    }
  })

  await test('every button carries the attributes a real button needs', () => {
    // The set that, missing one at a time, produces a control that misbehaves in a way nothing
    // else reports. Asserted for all seventeen items, because the failure is per-item.
    const editor = editorWith(PARAGRAPH, [Highlight])
    try {
      const state = toolbarState(adapt(editor))
      for (const i of TOOLBAR_ITEMS) {
        const spec = buttonSpec(i, state, LINUX)
        const a = spec.attributes
        // `type` because a `<button>` with no type inside a form is a submit button, and this
        // toolbar is going to sit in a document.
        ok(a.type === 'button', `${i.id} has type="${a.type}"; without it the button submits a form`)
        // A name and a tooltip. The visible content is an SVG, which contributes nothing to
        // the accessible name, so without `aria-label` the button is announced as "button".
        const name = a['aria-label']
        const tip = a.title
        ok(typeof name === 'string' && name.length > 0, `${i.id} has no accessible name`)
        ok(typeof tip === 'string' && tip.length > 0, `${i.id} has no tooltip`)
        ok(a.class === 'holo-toolbar__button', `${i.id} has class "${a.class}", which the stylesheet will not match`)
        // The icon carries its own a11y rules: hidden from assistive tech because it duplicates
        // the label, and not focusable so it cannot take a tab stop of its own.
        ok(spec.iconAttributes['aria-hidden'] === 'true', `${i.id}'s icon is not aria-hidden, so it is announced twice`)
        ok(spec.iconAttributes.focusable === 'false', `${i.id}'s icon may take its own tab stop`)
        ok(spec.iconAttributes.viewBox === '0 0 24 24', `${i.id}'s icon has viewBox "${spec.iconAttributes.viewBox}"`)
        ok(spec.iconAttributes.stroke === 'currentColor', `${i.id}'s icon is not coloured by the surrounding text`)
        // And no inline handler, which needs `unsafe-inline` under this app's CSP and cannot
        // be removed by `removeEventListener` as the toolbar is rebuilt.
        for (const key of Object.keys(a)) {
          ok(!/^on/i.test(key), `${i.id} sets an inline handler attribute: ${key}`)
        }
        // The command travels with the spec, and needsInput is explicit rather than inferred.
        ok(spec.command === i.command, `${i.id}'s spec carries command "${spec.command}"`)
        ok(spec.needsInput === (i.needsInput === true), `${i.id}'s needsInput disagrees with the item`)
      }
      // The root and group roles: without them the buttons are a flat list of the whole
      // document rather than a toolbar a screen reader can navigate.
      for (const [name, attrs] of [['root', TOOLBAR_ROOT_ATTRIBUTES], ['group', GROUP_ATTRIBUTES]] as Array<[string, Record<string, string>]>) {
        ok(attrs.role !== undefined, `the ${name} should declare a role`)
      }
      ok(TOOLBAR_ROOT_ATTRIBUTES['aria-orientation'] === 'horizontal', 'a horizontal toolbar should say so')
      ok(TOOLBAR_ROOT_ATTRIBUTES.class === 'holo-toolbar', 'the root class is what the stylesheet matches')
      return { items: TOOLBAR_ITEMS.length }
    } finally {
      editor.destroy()
    }
  })

  await test('an item needing input says so, and a plain one does not', () => {
    // The distinction between "runs a command" and "opens a dialog the app owns". A toolbar
    // that ran `setInlineMath` with no argument would insert an equation rendering an error,
    // so the flag is what stops it -- and it has to be carried in the spec, not re-derived
    // from the item at the call site where nobody would notice it was wrong.
    const editor = editorWith(PARAGRAPH)
    try {
      const state = toolbarState(adapt(editor))
      for (const i of TOOLBAR_ITEMS) {
        const spec = buttonSpec(i, state, LINUX)
        if (i.needsInput) {
          ok(spec.needsInput, `${i.id} is marked needsInput and must say so in its spec`)
        } else {
          ok(!spec.needsInput, `${i.id} is not marked needsInput and must not claim to be`)
        }
      }
      // The three that need a value, and the one that does not despite inserting a node.
      ok(buttonSpec(item('inlineMath'), state, LINUX).needsInput, 'an equation needs its TeX')
      ok(buttonSpec(item('blockMath'), state, LINUX).needsInput, 'and so does a display equation')
      ok(!buttonSpec(item('horizontalRule'), state, LINUX).needsInput, 'a rule needs nothing')
      // And a plain item carries its declared arguments, ready to hand a handler.
      const table = buttonSpec(item('table'), state, LINUX)
      ok(table.args.length === 1 && (table.args[0] as { rows?: number }).rows === 3,
        `the table spec should carry its dimensions, got ${JSON.stringify(table.args)}`)
      return { needsInput: TOOLBAR_ITEMS.filter(i => i.needsInput).length }
    } finally {
      editor.destroy()
    }
  })

  await test('a spec does not alias the item table', () => {
    // `buttonSpec` copies the argument array, and the reason is that `handlers.run` is the
    // app's: a handler that pushed onto the array it was handed would corrupt `TOOLBAR_ITEMS`
    // for every later click, and the second click would insert two tables. Cheap to get
    // wrong, invisible in review, and impossible to undo without a reload.
    const table = item('table')
    const spec = buttonSpec(table, EMPTY_STATE, LINUX)
    ok(spec.args !== table.args, 'the spec must not share the item table\'s array')
    ;(spec.args as unknown[]).push('injected')
    const again = buttonSpec(table, EMPTY_STATE, LINUX)
    ok(again.args.length === 1, `the item table was mutated: ${JSON.stringify(again.args)}`)
    return { args: again.args.length }
  })

  await test('no module builds a button with an inline handler attribute', () => {
    // `onclick="..."` is a string attribute: it needs `unsafe-inline` under the CSP this app
    // ships under, it is compiled at click time, and `removeEventListener` cannot remove it --
    // which matters because `updateToolbar` mutates a toolbar that is rebuilt as sections
    // mount. This is a text check, which is inherently fragile, so it is scoped to the two
    // files with comments stripped: the failure `source-checks.ts` records, where a doc
    // comment quoting a removed line flagged correct code.
    const core = join(dirname(fileURLToPath(import.meta.url)), '..', 'src', 'core')
    const strip = (s: string) => s.replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/.*$/gm, '')
    for (const file of ['toolbar.ts', 'shortcuts.ts']) {
      const body = strip(readFileSync(join(core, file), 'utf8'))
      const inline = body.match(/['"]on[a-z]+\s*['"]\s*[:=]/)
      ok(!inline, `${file} assigns an inline handler attribute: ${inline?.[0]}`)
      ok(!/\.onclick\s*=/.test(body), `${file} assigns .onclick`)
      ok(!/\.setAttribute\(\s*['"]on/i.test(body), `${file} setAttribute's an on* attribute`)
    }
    // The positive half, so the checks above are not vacuous: handlers are attached, and the
    // mousedown that keeps focus in the editor is present.
    const toolbar = strip(readFileSync(join(core, 'toolbar.ts'), 'utf8'))
    ok(toolbar.includes("addEventListener('click'"), 'toolbar.ts should attach clicks with addEventListener')
    ok(toolbar.includes("addEventListener('mousedown'"), 'and the mousedown that keeps the caret')
    // And the two attributes the toolbar's accessibility rests on are set on real elements,
    // not through a framework that would do it for us.
    ok(toolbar.includes("'aria-pressed'"), 'aria-pressed should be set explicitly')
    ok(toolbar.includes("'aria-label'"), 'and aria-label, since an SVG icon names nothing')
    return { files: 2 }
  })

  await test('the toolbar declares a name for every icon-only button', () => {
    // Every button here is an SVG and no text, so without an `aria-label` each one is
    // announced as just "button" and the toolbar is unusable with a screen reader. Asserted
    // over the item table because that is where the names live, and `renderItem` is
    // browser-only.
    for (const i of TOOLBAR_ITEMS) {
      const { name } = toolbarLabel(i, LINUX)
      ok(name.trim().length > 0, `${i.id} has no accessible name`)
      ok(!/^\s*$/.test(name), `${i.id}'s name is blank`)
    }
    // And the labels are distinct enough to tell apart by ear.
    const names = TOOLBAR_ITEMS.map(i => toolbarLabel(i, LINUX).name)
    ok(new Set(names).size === names.length, `two items share a name: ${names.join(', ')}`)
    return { names: names.length }
  })

  console.log(`${passed} passed, ${failed} failed`)
  if (failed) {
    console.log(`\nfailures:\n  ${failures.join('\n  ')}`)
    process.exit(1)
  }
}

await main()
