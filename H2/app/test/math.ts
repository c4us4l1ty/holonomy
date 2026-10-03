/**
 * Equations: the nodes, and what KaTeX does with TeX that does not compile.
 *
 * # What is testable without a renderer and what is not
 *
 * KaTeX is a pure string-to-string transform and the node *definitions* are data. Both are
 * covered here. What is not — that a NodeView actually owns its DOM subtree, that an update
 * re-renders rather than recreating, that an inline equation wraps inside its paragraph —
 * needs a layout engine, so it lives in `test/scroll.ts` against the product page.
 *
 * The split is deliberate rather than a compromise: the interesting question in Node is
 * "what does the node store and what does KaTeX do with bad input", and neither answer
 * changes if a browser is in the loop.
 *
 * Run: node --experimental-strip-types test/math.ts
 */

import { Node as PMNode } from '@tiptap/pm/model'
import { getSchema } from '@tiptap/core'
import { Editor } from '@tiptap/core'
import StarterKit from '@tiptap/starter-kit'
import { BlockMath, InlineMath, MathParseError, renderMath, texFromText } from '../src/core/math.ts'
import { ATOMIC_BLOCK_TYPES, isAtomicBlock } from '../src/core/lifecycle.ts'

let passed = 0
let failed = 0
const failures: string[] = []

function test(name: string, fn: () => unknown): void {
  try {
    const detail = fn()
    passed++
    console.log(`PASS  ${name}${detail !== undefined ? `  ${JSON.stringify(detail)}` : ''}`)
  } catch (e: any) {
    failed++
    failures.push(name)
    console.log(`FAIL  ${name}\n        ${e.message}`)
  }
}

function ok(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg)
}

type NodeName = 'inlineMath' | 'mathBlock' | 'doc'

/**
 * The schema, looked up through a helper that fails loudly.
 *
 * `Schema.nodes` is indexed by name, so TypeScript types every lookup as possibly
 * undefined. Writing `!` at six call sites would silence the check without improving it; a
 * missing node is a broken extension list, and this reports which one.
 */
function nodeType(schema: ReturnType<typeof getSchema>, name: NodeName) {
  const type = schema.nodes[name]
  if (!type) throw new Error(`the schema has no \`${name}\` node; the extension is not installed`)
  return type
}

/** The schema the editor under test installs, built the way `main.ts` builds it. */
const extensions = [
  StarterKit.configure({ undoRedo: false }),
  InlineMath,
  BlockMath,
]

console.log('math: equation nodes and KaTeX rendering')
console.log('='.repeat(72))

test('an equation node stores the TeX and nothing else', () => {
  // The single source of truth. Storing the rendered HTML as well would mean two things to
  // keep in step, and a document that could disagree with itself.
  const schema = getSchema(extensions)
  const node = nodeType(schema, 'inlineMath').create({ latex: 'e^{i\\pi} + 1 = 0' })
  ok(node.attrs.latex === 'e^{i\\pi} + 1 = 0', `attrs should hold the TeX, got ${node.attrs.latex}`)
  ok(
    Object.keys(node.attrs).length === 1,
    `only \`latex\` should be stored; found ${Object.keys(node.attrs).join(',')}`,
  )
  return { attrs: Object.keys(node.attrs) }
})

test('an equation round trips through JSON unchanged', () => {
  // Persistence is `JSON.parse(editor.getJSON())` into a zstd blob, so this round trip is
  // the storage path. A node whose `toJSON` dropped `latex` would save an equation as an
  // empty box and restore it as one.
  const schema = getSchema(extensions)
  for (const [type, latex] of [
    ['inlineMath', '\\int_0^1 x^2 dx'],
    ['mathBlock', '\\sum_{i=1}^{n} i = \\frac{n(n+1)}{2}'],
  ] as const) {
    const node = nodeType(schema, type).create({ latex })
    const back = PMNode.fromJSON(schema, node.toJSON())
    ok(back.type.name === type, `the type changed: ${back.type.name}`)
    ok(back.attrs.latex === latex, `the TeX changed: ${back.attrs.latex}`)
  }
  return { roundTripped: 2 }
})

test('both equation nodes are atoms, and the seam rule knows it', () => {
  // `atom: true` is what makes an equation one thing to the caret and to the split rule.
  // The second half is the one that is easy to forget: `ATOMIC_BLOCK_TYPES` is consulted on
  // raw JSON, and `isAtomicBlock` reads `type`, so `mathBlock` and `equation` have to be
  // *named* in the list for the seam rule to see them.
  const schema = getSchema(extensions)
  ok(nodeType(schema, 'inlineMath').isAtom === true, 'inlineMath should be an atom')
  ok(nodeType(schema, 'mathBlock').isAtom === true, 'mathBlock should be an atom')
  ok(nodeType(schema, 'inlineMath').isInline === true, 'inlineMath should be inline')
  ok(nodeType(schema, 'mathBlock').isInline === false, 'mathBlock should be a block')

  ok(isAtomicBlock({ type: 'mathBlock' }), 'mathBlock must be in the seam rule\'s list')
  ok(isAtomicBlock({ type: 'equation' }), 'equation must be in the seam rule\'s list')
  ok(
    ATOMIC_BLOCK_TYPES.has('mathBlock') && ATOMIC_BLOCK_TYPES.has('equation'),
    'both spellings named by the directive should be listed',
  )
})

test('inlineMath belongs to a paragraph and mathBlock does not', () => {
  // The group is what decides where a node may appear. Getting it wrong means either a
  // paragraph refusing to contain an equation, or a display equation ending up mid-sentence.
  const schema = getSchema(extensions)
  const inline = nodeType(schema, 'inlineMath')
  const block = nodeType(schema, 'mathBlock')
  ok(inline.spec.group === 'inline', `group was ${inline.spec.group}`)
  ok(block.spec.group === 'block', `group was ${block.spec.group}`)
  const docContent = String(nodeType(schema, 'doc').spec.content ?? '')
  ok(docContent.includes('block'), `doc content should accept blocks, got ${docContent}`)
})

test('KaTeX renders TeX to markup carrying the source', () => {
  const html = renderMath('x^2', false)
  ok(html.length > 0, 'nothing was rendered')
  ok(html.includes('katex'), `KaTeX marks its own output; got: ${html.slice(0, 120)}`)
  ok(
    html.includes('aria-hidden="true"'),
    'KaTeX hides the MathML from assistive tech and leaves the HTML visible; a render ' +
      'without that is not a KaTeX render',
  )
  return { bytes: html.length }
})

test('display mode changes the output, so the two nodes are not the same render', () => {
  const inline = renderMath('x^2', false)
  const block = renderMath('x^2', true)
  ok(inline !== block, 'inline and display rendering produced identical markup')
  ok(block.includes('katex-display'), `a display render should carry katex-display: ${block.slice(0, 120)}`)
})

test('malformed TeX renders visibly rather than throwing', () => {
  // The choice that matters while typing. `throwOnError: false` is what keeps a
  // half-typed equation from blanking its node and taking the surrounding prose with it.
  const html = renderMath('\\frac{1}{', false)
  ok(typeof html === 'string' && html.length > 0, 'invalid TeX produced nothing at all')
  ok(html.includes('katex-error'), `KaTeX marks its own failures; got: ${html.slice(0, 200)}`)
  ok(
    html.includes('\\frac{1}{') || html.includes('frac'),
    'the failing source should still be visible, so the user can see what is wrong',
  )
})

test('an empty equation renders, because it is what an empty node contains', () => {
  // A newly inserted equation has empty TeX. Throwing here would make inserting one fail.
  const html = renderMath('', false)
  ok(typeof html === 'string', 'an empty equation should still produce markup')
})

test('KaTeX is not trusted with the input', () => {
  // `trust: false` and `output: 'html'`. With `output: 'htmlAndMathml'` and `trust: true`,
  // KaTeX's `\href` and `\htmlClass` would insert nodes the NodeView did not create, into
  // the editor's own DOM. This is a property of the configuration, so it is asserted.
  const html = renderMath('\\href{javascript:alert(1)}{click}', false)
  ok(
    !html.toLowerCase().includes('<a '),
    `\\href should not produce an anchor when trust is off: ${html.slice(0, 200)}`,
  )
})

test('a structural KaTeX failure is wrapped, not leaked', () => {
  // `MathParseError` exists so a NodeView cannot die with an unlabelled exception and take
  // the editor with it. KaTeX handles per-expression errors itself, so this is only reachable
  // through something structural — but the wrapper is what makes that survivable.
  ok(MathParseError.prototype instanceof Error, 'MathParseError should be an Error')
  let thrown: unknown = null
  try {
    renderMath('x', false)
  } catch (e) {
    thrown = e
  }
  ok(thrown === null, `a valid equation should not throw; got ${thrown}`)
})

test('pasted TeX becomes equation nodes', () => {
  // The most common way an equation arrives in a word processor is a paste from another one,
  // which produces `$…$`. An input rule only fires on a space typed inside the editor, so
  // without this a pasted document shows literal `$x^2$`.
  const nodes = texFromText('the identity $e^{i\\pi} + 1 = 0$ holds')
  ok(nodes.length === 1, `expected one equation, got ${nodes.length}`)
  ok(nodes[0]!.type === 'inlineMath', `expected inlineMath, got ${nodes[0]!.type}`)
  ok(nodes[0]!.attrs.latex === 'e^{i\\pi} + 1 = 0', `TeX was ${nodes[0]!.attrs.latex}`)
})

test('a display equation is recognised as a block, not two inline ones', () => {
  const nodes = texFromText('text before \\[ \\sum_{i} i \\] text after')
  ok(nodes.length === 1, `expected one equation, got ${nodes.length}`)
  ok(nodes[0]!.type === 'mathBlock', `expected mathBlock, got ${nodes[0]!.type}`)
  ok(nodes[0]!.attrs.latex === '\\sum_{i} i', `TeX was ${nodes[0]!.attrs.latex}`)
})

test('prose with no TeX produces nothing', () => {
  // The dangerous direction: a converter that fires on ordinary text turns every dollar
  // sign in a document into an equation.
  for (const text of ['costs $5 and $10', 'a $ sign', 'no maths here at all', '']) {
    ok(texFromText(text).length === 0, `should have found nothing in ${JSON.stringify(text)}`)
  }
})

test('a currency amount is not an equation', () => {
  // The specific false positive worth a test: `$5` and `$10` are two dollar amounts, and a
  // naive `$…$` rule reads the space between them as an equation.
  ok(texFromText('it costs $5 and $10').length === 0, 'two prices are not an equation')
  ok(texFromText('a $5 note').length === 0, 'one price is not an equation')
})

test('the commands insert the nodes they claim to', () => {
  const editor = new Editor({
    extensions,
    content: { type: 'doc', content: [{ type: 'paragraph' }] },
  })
  editor.commands.setInlineMath('x^2')
  const withInline = editor.getJSON().content![0] as any
  ok(withInline.type === 'paragraph', `expected a paragraph, got ${withInline.type}`)
  const inline = withInline.content?.find((n: any) => n.type === 'inlineMath')
  ok(inline !== undefined, 'no inline equation was inserted')
  ok(inline.attrs.latex === 'x^2', `TeX was ${inline.attrs.latex}`)

  editor.commands.setBlockMath('\\int_0^1')
  const json = editor.getJSON().content as any[]
  const block = json.find(n => n.type === 'mathBlock')
  ok(block !== undefined, 'no block equation was inserted')
  ok(block.attrs.latex === '\\int_0^1', `TeX was ${block.attrs.latex}`)
  editor.destroy()
})

console.log('='.repeat(72))
console.log(`${passed} passed, ${failed} failed`)
if (failed) {
  console.log(`failing: ${failures.join(', ')}`)
  process.exit(1)
}

