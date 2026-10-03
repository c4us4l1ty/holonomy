/**
 * Section lifecycle: the cut point, the caret anchor, and the reconciliation.
 *
 * # The property everything else depends on
 *
 * A caret anchor is captured as an offset before a structural change and restored as
 * a position after it. If that round trip is wrong, the symptom is a caret in the
 * wrong paragraph — which is not a crash, not an error, and not something a user
 * would describe as "the editor is broken". They would say the cursor jumped.
 *
 * So the round trip is tested at *every* position in a document rather than at a
 * chosen one. A test with a handful of hand-picked positions would pass against an
 * implementation that is correct everywhere except in the middle of a list, and the
 * middle of a list is where people type.
 *
 * # Why Node and not a browser
 *
 * `lifecycle.ts` imports its collaborators as types, so it runs under
 * `--experimental-strip-types` with no bundler and no window. The functions here that
 * need a document are given one built by the harness's own schema-less builder, which
 * is enough because the algorithms only ask nodes about `isText`, `isTextblock`,
 * `content.size` and `nodesBetween`.
 *
 * Run: node --experimental-strip-types test/lifecycle.ts
 */

import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { Schema, type Node } from '@tiptap/pm/model'
import {
  applyMerge,
  applySplit,
  atomicTypesFromSchema,
  ATOMIC_BLOCK_TYPES,
  caretOffsetFromStart,
  chooseCutIndex,
  isAtomicBlock,
  localMetrics,
  positionFromCaretOffset,
  seamIsReversible,
  splitActionFor,
  splitTrigger,
  unsplittableBlocks,
  MAX_MARKS_PER_SECTION,
  MAX_WORDS_PER_SECTION,
  type CaretAnchor,
  type GeometryChange,
} from '../src/core/lifecycle.ts'
import type { LifecycleAction, LifecycleResult } from '../src/core/boot.ts'

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

// -- real documents ----------------------------------------------------------

/**
 * A real ProseMirror schema and real documents.
 *
 * # Why not a hand-built stand-in
 *
 * The first version of this file built a fake node tree implementing `nodesBetween`.
 * That was testing the fake: `caretOffsetFromStart` is now defined in terms of
 * ProseMirror's own `textBetween`, and a reimplementation of it in the test would have
 * agreed with the implementation by construction and proved nothing about the real
 * thing.
 *
 * `chooseCutIndex` and `metricsOf` read plain JSON, so those need nothing — but
 * anything that touches positions does, and those are exactly the parts where a
 * plausible-looking stand-in hides the bug.
 */
const fixtures = join(dirname(fileURLToPath(import.meta.url)), 'fixtures')

const schema = new Schema({
  nodes: {
    doc: { content: 'block+' },
    paragraph: { content: 'inline*', group: 'block', toDOM: () => ['p', 0] },
    heading: { content: 'inline*', group: 'block', attrs: { level: { default: 2 } }, toDOM: () => ['h2', 0] },
    bulletList: { content: 'listItem+', group: 'block', toDOM: () => ['ul', 0] },
    listItem: { content: 'block+', toDOM: () => ['li', 0] },
    text: { group: 'inline' },
    hardBreak: { group: 'inline', inline: true, toDOM: () => ['br'] },
  },
  marks: {
    bold: { toDOM: () => ['strong', 0] },
    italic: { toDOM: () => ['em', 0] },
    highlight: { toDOM: () => ['mark', 0] },
  },
})

/** A document of `n` paragraphs, each `size` characters, with a heading first. */
function uniformDoc(n: number, size: number): Node {
  const content: unknown[] = [
    { type: 'heading', attrs: { level: 2 }, content: [{ type: 'text', text: 'Head' }] },
  ]
  for (let i = 0; i < n; i++) {
    const text = `${String(i).padStart(3, '0')} ` + 'x'.repeat(Math.max(0, size - 4))
    content.push({ type: 'paragraph', content: [{ type: 'text', text }] })
  }
  return schema.nodeFromJSON({ type: 'doc', content })
}

/** A document whose paragraphs vary wildly in size, plus a list. */
function awkwardDoc(): Node {
  return schema.nodeFromJSON({
    type: 'doc',
    content: [
      { type: 'heading', attrs: { level: 2 }, content: [{ type: 'text', text: 'Mixed' }] },
      { type: 'paragraph', content: [{ type: 'text', text: 'a'.repeat(1200) }] },
      { type: 'paragraph', content: [{ type: 'text', text: 'b'.repeat(1200) }] },
      { type: 'paragraph', content: [{ type: 'text', text: 'c'.repeat(1200) }] },
      { type: 'bulletList', content: [{ type: 'listItem', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'item one' }] }] }] },
      { type: 'bulletList', content: [{ type: 'listItem', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'item two' }] }] }] },
      { type: 'paragraph', content: [{ type: 'text', text: 'z'.repeat(9000) }] },
      { type: 'paragraph', content: [{ type: 'text', text: 'short' }] },
    ],
  })
}

/** A document with hard breaks inside paragraphs. */
function breakDoc(): Node {
  return schema.nodeFromJSON({
    type: 'doc',
    content: [
      {
        type: 'paragraph',
        content: [
          { type: 'text', text: 'ab' },
          { type: 'hardBreak' },
          { type: 'text', text: 'cd' },
          { type: 'hardBreak' },
        ],
      },
      { type: 'paragraph', content: [{ type: 'text', text: 'ef' }] },
      { type: 'bulletList', content: [{ type: 'listItem', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'gh' }] }] }] },
    ],
  })
}

const json = (doc: Node): unknown => doc.toJSON()

async function main() {
  console.log('lifecycle: the cut point, the caret anchor, and the reconciliation')
  console.log('='.repeat(72))

  // -- the cut point ------------------------------------------------------

  await test('the cut lands on a boundary, never inside a block', () => {
    // The only cut that does not leave a half-open structure behind. Every returned
    // index must be a legal boundary: between 1 and length-1 inclusive, so both halves
    // have at least one block.
    for (const [n, size] of [[2, 100], [5, 50], [20, 200], [100, 30], [1, 400]] as Array<[number, number]>) {
      const doc = uniformDoc(n, size)
      const blocks = (json(doc) as any).content.length
      const cut = chooseCutIndex(json(doc))!
      ok(Number.isInteger(cut), `with ${blocks} blocks the cut should be an integer, got ${cut}`)
      ok(
        cut >= 1 && cut <= blocks - 1,
        `with ${blocks} blocks the cut should be in 1..${blocks - 1}, got ${cut}`,
      )
      const content = (json(doc) as any).content as unknown[]
      ok(content.slice(0, cut).length > 0, `the head of a split at ${cut} is empty`)
      ok(content.slice(cut).length > 0, `the tail of a split at ${cut} is empty`)
    }
    return { cases: 5 }
  })

  await test('the cut is near the middle, measured by rendered size', () => {
    // Even by size, not by block count. Three huge paragraphs and many tiny ones should
    // not be cut at the midpoint *by index*.
    const doc = uniformDoc(3, 1200)
    // Append many tiny blocks after the three big ones.
    const content = (json(doc) as any).content as unknown[]
    for (let i = 0; i < 20; i++) {
      content.push({ type: 'paragraph', content: [{ type: 'text', text: `t${i}` }] })
    }
    const grown = schema.nodeFromJSON({ type: 'doc', content })
    const blocks = (json(grown) as any).content as Array<{ content: Array<{ text?: string }> }>
    // Measured the way `chooseCutIndex` measures: text length, not JSON length. The
    // first version of this test used `JSON.stringify(b).length`, which weights a block
    // by its markup, so the ratio it computed described a different split from the one
    // the function actually chose — and then failed against a correct implementation.
    const weights = blocks.map(b => (b.content[0]?.text ?? '').length)
    const total = weights.reduce((a, b) => a + b, 0)
    const cut = chooseCutIndex(json(grown))!

    ok(cut <= 5, `half the weight is in the first four blocks; the cut should be there, got ${cut}`)
    const head = weights.slice(0, cut).reduce((a, b) => a + b, 0)
    const ratio = Math.min(head, total - head) / Math.max(head, total - head)
    // The guarantee the implementation makes, which is the share window: a head holding
    // between 30% and 70% of the weight means a ratio of at least 0.43. Asserting more
    // than that would be asserting a tighter contract than the code offers — and on a
    // document whose weight is front-loaded, 0.43 is the *right* answer, because cutting
    // nearer the middle would need an illegal boundary.
    ok(
      ratio >= 0.3 / 0.7,
      `the head should hold 30-70% of the weight; ratio ${ratio.toFixed(3)}`,
    )
    ok(cut <= 5, `half the weight is in the first four blocks; the cut should be there, got ${cut}`)
    return { cut, balanceRatio: Number(ratio.toFixed(3)), blocks: blocks.length }
  })

  await test('a large trailing block does not drag the cut onto its edge', () => {
    // The reason the share constraint exists. Minimising |head - total/2| alone, on ten
    // 1000-character blocks and one 9000-character block, has its midpoint inside the
    // large block; every legal boundary is far from it, and the objective happily picks
    // the boundary at the large block's edge, which puts almost everything in the head
    // and produces a second "section" of a couple of characters.
    const content: unknown[] = [
      { type: 'heading', attrs: { level: 2 }, content: [{ type: 'text', text: 'Doc' }] },
      ...Array.from({ length: 10 }, () => ({
        type: 'paragraph',
        content: [{ type: 'text', text: 'y'.repeat(1000) }],
      })),
      { type: 'paragraph', content: [{ type: 'text', text: 'z'.repeat(9000) }] },
    ]
    const doc = schema.nodeFromJSON({ type: 'doc', content })
    const weights = (json(doc) as any).content.map((b: any) => (b.content[0]?.text ?? '').length)
    const total = weights.reduce((a: number, b: number) => a + b, 0)
    const cut = chooseCutIndex(json(doc))!

    const head = weights.slice(0, cut).reduce((a: number, b: number) => a + b, 0)
    const ratio = Math.min(head, total - head) / Math.max(head, total - head)
    ok(ratio >= 0.3 / 0.7, `the head should hold 30-70% of the weight; ratio ${ratio.toFixed(3)}`)
    ok(
      cut !== content.length - 1,
      `the cut landed on the large block's edge (${cut} of ${content.length}), which is the ` +
        'degenerate split the share constraint exists to prevent',
    )
    return { cut, blocks: content.length, balanceRatio: Number(ratio.toFixed(3)) }
  })

  await test('a document too concentrated to split evenly still gets a legal cut', () => {
    // The fallback, and the case where no good answer exists.
    //
    // `awkwardDoc` is 71% one block, so the midpoint falls inside it and every legal
    // boundary is worse than every other. The requirement is only that the result is a
    // legal boundary with two non-empty halves — not that it is even, because there is no
    // even cut to find. An implementation that reported "cannot split" here would be
    // refusing to divide a section it could perfectly well divide.
    //
    // The first version of this test asserted a ratio of 0.6 against this document, which
    // is unachievable, so it failed against correct code. The document and the claim were
    // both wrong.
    const doc = awkwardDoc()
    const blocks = (json(doc) as any).content as unknown[]
    const weights = blocks.map(b => textLengthOf(b))
    const largest = Math.max(...weights)
    ok(
      largest / weights.reduce((a: number, b: number) => a + b, 0) > 0.6,
      'precondition: this document really is dominated by one block',
    )

    const cut = chooseCutIndex(json(doc))!
    ok(
      Number.isInteger(cut) && cut >= 1 && cut <= blocks.length - 1,
      `the cut ${cut} should be a legal boundary of ${blocks.length} blocks`,
    )
    return { cut, blocks: blocks.length, largestShare: Number((largest / weights.reduce((a: number, b: number) => a + b, 0)).toFixed(3)) }
  })

  await test('a document with no interior boundary reports it', () => {
    // `null` rather than a number that would be clamped into producing an empty half.
    // The caller needs to know without comparing block counts itself — and without
    // sending a request that will be refused.
    ok(chooseCutIndex({ type: 'doc', content: [] }) === null, 'an empty document has no cut')
    ok(
      chooseCutIndex({ type: 'doc', content: [{ type: 'paragraph' }] }) === null,
      'a one-block document has no interior boundary',
    )
    ok(
      chooseCutIndex({ type: 'doc', content: [{ type: 'paragraph' }, { type: 'paragraph' }] }) === 1,
      'two blocks have exactly one interior boundary',
    )
    ok(chooseCutIndex(null) === null, 'a null document has no cut')
    ok(chooseCutIndex({}) === null, 'a document with no content array has no cut')
  })

  await test('a list at the cut point is not a reason to refuse', () => {
    // A list is a block like any other and the cut falls *between* blocks, never inside
    // one, so a list at the seam is fine. What must never happen is a cut that leaves a
    // half-open list — which `chooseCutIndex` cannot produce.
    const doc = awkwardDoc()
    const cut = chooseCutIndex(json(doc))!
    const blocks = (json(doc) as any).content as Array<{ type: string }>
    ok(Number.isInteger(cut) && cut >= 1 && cut <= blocks.length - 1,
      `the cut ${cut} should be a valid boundary of ${blocks.length} blocks`)
    return { cut, blockTypes: blocks.map(b => b.type) }
  })

  await test('the split trigger matches the measured limits', () => {
    // 1500 words and 3000 marks, from M0's rendering knee and M1b's mark ceiling.
    ok(splitTrigger({ words: 1500, marks: 0 }) === null, 'exactly at the limit is not over it')
    ok(splitTrigger({ words: 1501, marks: 0 }) === 'words', 'one word over triggers')
    ok(splitTrigger({ words: 0, marks: 3000 }) === null, 'exactly at the mark limit is not over')
    ok(splitTrigger({ words: 0, marks: 3001 }) === 'marks', 'one mark over triggers')
    // Whichever limit is passed first is reported, because that is the one being felt.
    ok(splitTrigger({ words: 2000, marks: 4000 }) === 'words', 'the word limit takes precedence')
    ok(MAX_WORDS_PER_SECTION === 1500, `word limit drifted to ${MAX_WORDS_PER_SECTION}`)
    ok(MAX_MARKS_PER_SECTION === 3000, `mark limit drifted to ${MAX_MARKS_PER_SECTION}`)
  })

  // -- metrics ------------------------------------------------------------

  await test('metrics match Rust on the shared parity fixture', () => {
    // The numbers here are asserted from the other side too, in
    // `crates/holonomy-core/tests/metrics-parity.rs`, against the same committed
    // fixture. Neither definition can change without one of the two failing.
    //
    // See that test for why a parity assertion is justified at all when DOCTRINE.md §8
    // forbids duplication: `analyze` is the definition, but it cannot answer the two
    // questions the renderer asks without putting the store on the typing path.
    //
    // The fixture is deliberately awkward. Adjacent inline runs with no space between
    // them are the case a naive per-run word count gets wrong by doubling; a nested
    // list separates lines without being a top-level block; two of the three mark
    // instances are on the same run.
    const fixture = JSON.parse(
      readFileSync(join(fixtures, 'metrics-parity.json'), 'utf8'),
    )
    const m = localMetrics(fixture)
    // Five words, because "three" and "four" are adjacent runs with no space between them
    // and form one word.
    ok(m.words === 5, `expected 5 words, got ${m.words}`)
    // Non-whitespace characters: the two separators do not count.
    ok(m.chars === 27, `expected 27 characters, got ${m.chars}`)
    // Three top-level blocks. The list is one; the paragraph inside it is not counted.
    ok(m.blocks === 3, `expected 3 blocks, got ${m.blocks}`)
    ok(m.marks === 3, `expected 3 mark instances, got ${m.marks}`)
    return m
  })

  await test('mark counting is exact, not a backstop of zero', () => {
    // An earlier version here returned `marks: 0` on the grounds that the mark ceiling
    // was only a fallback. It is not: 3000 marks is reachable well under 1500 words in a
    // heavily formatted section, and a permanent zero would mean such a section never
    // splits.
    const heavy = {
      type: 'doc',
      content: [
        {
          type: 'paragraph',
          content: [
            { type: 'text', marks: [{ type: 'bold' }, { type: 'italic' }], text: 'a' },
            { type: 'text', marks: [{ type: 'highlight' }], text: 'b' },
            { type: 'text', text: 'c' },
            { type: 'text', marks: [{ type: 'bold' }], text: 'd' },
          ],
        },
      ],
    }
    ok(localMetrics(heavy).marks === 4, `expected 4 marks, got ${localMetrics(heavy).marks}`)
  })

  await test('a hard break is a line, not a character', () => {
    // `analyze` counts non-whitespace characters and a `hardBreak` is neither, so the two
    // sides agree it is worth zero. An earlier version of this file asserted the
    // opposite on the reasoning that "a section of nothing but line breaks would count
    // as zero characters" — which is true of `analyze` too, and is a property of the
    // measure rather than a bug to fix in the frontend. Matching the authority is the
    // point; disagreeing with it would mean two height models.
    const m = localMetrics({
      type: 'doc',
      content: [
        {
          type: 'paragraph',
          content: [{ type: 'text', text: 'a' }, { type: 'hardBreak' }, { type: 'text', text: 'b' }],
        },
      ],
    })
    ok(m.chars === 2, `expected 2 characters (a and b), got ${m.chars}`)
    ok(m.words === 1, `"a" and "b" either side of a hard break are one run, so ${m.words} words is wrong`)
  })

  // -- the caret measure ---------------------------------------------------

  await test('the caret measure is non-decreasing in position', () => {
    // The binary search in `positionFromCaretOffset` depends on this. A non-monotone
    // measure would not error — it would return *a* position instead of the first one,
    // which is indistinguishable from correct until the caret is somewhere odd.
    for (const [name, doc] of [['uniform', uniformDoc(6, 60)], ['mixed', awkwardDoc()], ['breaks', breakDoc()]] as Array<[string, Node]>) {
      let previous = -1
      for (let pos = 1; pos < doc.content.size; pos++) {
        const offset = caretOffsetFromStart(doc, pos)
        ok(
          offset >= previous,
          `${name}: the measure went backwards at position ${pos} (${offset} after ${previous})`,
        )
        previous = offset
      }
    }
    return { documents: 3 }
  })

  await test('restoring an anchor is idempotent, which is what "same place" means', () => {
    // Not identity, and the difference matters.
    //
    // The measure cannot be injective: inside a paragraph a `hardBreak` contributes
    // nothing, so "after the last character" and "at the break" are the same offset. No
    // text-length measure distinguishes them, and an implementation claiming it did
    // would be lying about a property the measure does not have.
    //
    // The achievable and meaningful property is that restoring is idempotent — the
    // second time round changes nothing — and that it lands at the *first* position with
    // that much text before it, which is the canonical one.
    for (const [name, doc] of [['uniform', uniformDoc(6, 60)], ['mixed', awkwardDoc()], ['breaks', breakDoc()]] as Array<[string, Node]>) {
      let first = true
      for (let pos = 1; pos < doc.content.size; pos++) {
        const once = positionFromCaretOffset(doc, caretOffsetFromStart(doc, pos))
        ok(
          once >= 1 && once < doc.content.size,
          `${name}: position ${pos} restored to ${once}, outside the document`,
        )
        ok(
          caretOffsetFromStart(doc, once) === caretOffsetFromStart(doc, pos),
          `${name}: restoring ${pos} changed the text before it`,
        )
        if (first) {
          const twice = positionFromCaretOffset(doc, caretOffsetFromStart(doc, once))
          ok(twice === once, `${name}: restoring was not idempotent at ${pos}: ${once} then ${twice}`)
          first = false
        }
      }
    }
    return { documents: 3 }
  })

  await test('a restore lands on the first position with that measure', () => {
    // Not "the same position", and not "at or after it".
    //
    // The measure is not injective — inside a paragraph a `hardBreak` contributes
    // nothing, so several positions share an offset — so a restore can land at an
    // *earlier* position than the original. That is correct, and the earlier version of
    // this test asserted the opposite and failed. Which of the equal positions is
    // canonical does matter though: the first, because it is the start of the character
    // the user was pointing after, and picking among the others arbitrarily would make
    // the restored caret depend on the scan.
    for (const [name, doc] of [['uniform', uniformDoc(5, 70)], ['breaks', breakDoc()]] as Array<[string, Node]>) {
      for (let pos = 1; pos < doc.content.size; pos++) {
        const offset = caretOffsetFromStart(doc, pos)
        const back = positionFromCaretOffset(doc, offset)
        ok(
          caretOffsetFromStart(doc, back) === offset,
          `${name}: ${pos} (offset ${offset}) restored to ${back}, which measures differently`,
        )
        ok(
          caretOffsetFromStart(doc, back - 1) < offset || back === 1,
          `${name}: ${back} is not the first position with offset ${offset}; ${back - 1} has it too`,
        )
      }
    }
    return { documents: 2 }
  })

  await test('a caret offset survives a split', () => {
    // The property the reconciliation rests on. A split divides the content, so nothing
    // before the cut moves and the offset a position has in the whole document is the
    // offset it has in the head.
    //
    // Checked against the *head*, not against a hand-derived boundary arithmetic: the
    // first version of this test tried to work out which half a position belonged to
    // using an expression mixing block counts and content sizes, and it was wrong in a
    // way that made the assertion meaningless.
    const doc = uniformDoc(10, 50)
    const cut = chooseCutIndex(json(doc))!
    const blocks = (json(doc) as any).content as unknown[]
    const head = schema.nodeFromJSON({ type: 'doc', content: blocks.slice(0, cut) })
    const tail = schema.nodeFromJSON({ type: 'doc', content: blocks.slice(cut) })
    const headLength = head.textBetween(0, head.content.size, '\n').length
    const wholeLength = doc.textBetween(0, doc.content.size, '\n').length
    const tailLength = tail.textBetween(0, tail.content.size, '\n').length
    // The two halves' text adds up, plus one separator for the boundary between them.
    ok(
      headLength + tailLength + 1 === wholeLength,
      `halves should account for the whole: ${headLength} + ${tailLength} + 1 != ${wholeLength}`,
    )

    let inHead = 0
    let inTail = 0
    for (let pos = 1; pos < doc.content.size; pos++) {
      const offset = caretOffsetFromStart(doc, pos)
      if (offset <= headLength) {
        inHead++
        // The head is a prefix, so the same offset resolves to a position with the same
        // text before it. Compared through `positionFromCaretOffset` on both sides,
        // because `caretOffsetFromStart` takes a document *position* — passing it an
        // offset, which is what the first version of this test did, compares two
        // unrelated numbers.
        ok(
          caretOffsetFromStart(head, positionFromCaretOffset(head, offset)) === offset,
          `offset ${offset} means something different in the head`,
        )
      } else {
        inTail++
        // In the tail the offset restarts: subtract the head's text and the separator
        // that used to separate the last head block from the first tail block.
        const adjusted = offset - headLength - 1
        ok(
          adjusted >= 0 && adjusted <= tailLength,
          `tail offset ${offset} maps to ${adjusted}, outside 0..${tailLength}`,
        )
      }
    }
    ok(inHead > 10, `the head should hold most positions, got ${inHead}`)
    ok(inTail > 5, `the tail should hold some, got ${inTail}`)
    return { cut, headLength, tailLength, inHead, inTail }
  })

  await test('a hard break is still a placeable position', () => {
    // A line break has no text node, so an implementation that only counted text nodes
    // would misplace the caret at every newline in the document. The measure includes a
    // separator only *between* blocks, so offsets inside a paragraph with breaks are
    // ambiguous — which the idempotence test covers. What must hold is that every
    // offset resolves to a valid position.
    const doc = breakDoc()
    const total = doc.textBetween(0, doc.content.size, '\n').length
    for (let offset = 0; offset <= total; offset++) {
      const pos = positionFromCaretOffset(doc, offset)
      ok(pos >= 1 && pos < doc.content.size, `offset ${offset} gave ${pos}, outside the document`)
    }
    return { offsetsChecked: total + 1 }
  })

  await test('an anchor past the end clamps rather than throwing', () => {
    // A merge shortens the section the anchor was in. A caret at the join point is a
    // good outcome; no caret at all is worse and much harder to notice being wrong.
    const doc = uniformDoc(3, 10)
    const last = doc.content.size - 1
    for (const offset of [1000, 999999, Number.MAX_SAFE_INTEGER]) {
      const pos = positionFromCaretOffset(doc, offset)
      ok(pos >= 1 && pos < doc.content.size, `offset ${offset} gave ${pos}, outside the document`)
    }
    ok(positionFromCaretOffset(doc, 0) === 1, 'offset 0 should be the first position')
    ok(positionFromCaretOffset(doc, -5) === 1, 'a negative offset should clamp to the first position')
    ok(positionFromCaretOffset(doc, 1000) === last, 'an offset past the end should be the last position')
  })

  await test('restoring is fast enough to run on a full-size section', () => {
    // It runs on a structural change, which is rare, so "rare" is how an O(n²) scan
    // survives until a 2000-page document makes it common. A linear scan testing every
    // position would be about 80 million character visits here.
    const doc = uniformDoc(30, 620)
    const total = doc.textBetween(0, doc.content.size, '\n').length
    const t0 = performance.now()
    for (let i = 0; i < 200; i++) positionFromCaretOffset(doc, (i * 7919) % (total + 1))
    const perCall = ((performance.now() - t0) / 200) * 1000
    ok(
      perCall < 1000,
      `positionFromCaretOffset took ${perCall.toFixed(0)}µs on a ${total}-character section; a ` +
        'linear scan would be orders of magnitude worse',
    )
    return { sectionChars: total, perCallUs: Number(perCall.toFixed(1)) }
  })

  // -- reconciliation -----------------------------------------------------

  await test('a split inserts the new section after the one it came from', async () => {
    // Position, not just membership: the new section's content is the tail, so putting
    // it anywhere else would reorder the document. `at_block` is what says where.
    const host = new FakeHost(['a', 'b', 'c'])
    const doc = uniformDoc(6, 40)
    const cut = chooseCutIndex(json(doc))!
    host.reply = { applied: true, section_ids: ['a', 'a-tail-1', 'b', 'c'], reason: null }

    const result = await applySplit(host as never, 'a', json(doc), { sectionId: 'a', offset: 20 })

    ok(result.applied, 'the split should have applied')
    ok(host.lastAction?.kind === 'split', `expected a split, got ${host.lastAction?.kind}`)
    const action = host.lastAction as Extract<LifecycleAction, { kind: 'split' }>
    ok(action.at_block === cut, `at_block should be the chosen cut ${cut}, got ${action.at_block}`)
    ok(action.index === 0, `index should be the section's position, got ${action.index}`)
    ok(host.restored?.sectionId === 'a', `focus should stay in 'a', got ${host.restored?.sectionId}`)
    ok(host.restored?.offset === 20, 'the anchor offset should pass through unchanged')
    ok(host.ids().join() === 'a,a-tail-1,b,c', `ordering wrong: ${host.ids().join()}`)
    return { cut, ids: host.ids() }
  })

  await test('a split divides the content with nothing lost or duplicated', async () => {
    // The property that makes a split safe at all. Checked on the content, not just the
    // counts: a split that dropped the tail would silently delete half a section, and a
    // block-count check would pass if it dropped and added the same number.
    const host = new FakeHost(['a', 'b'])
    const doc = uniformDoc(9, 30)
    const total = (json(doc) as any).content.length
    const cut = chooseCutIndex(json(doc))!
    host.reply = { applied: true, section_ids: ['a', 'a-tail-1', 'b'], reason: null }

    await applySplit(host as never, 'a', json(doc), { sectionId: 'a', offset: 10 })

    const head = (host.registry.records['a'] as any).json.content as unknown[]
    const tail = (host.registry.records['a-tail-1'] as any).json.content as unknown[]
    ok(head.length === cut, `head should hold ${cut} of ${total} blocks, holds ${head.length}`)
    ok(tail.length === total - cut, `tail should hold ${total - cut} blocks, holds ${tail.length}`)
    ok(head.length + tail.length === total, 'blocks were lost or duplicated')
    const rejoined = [...head, ...tail].map((b: any) => b.content[0].text).join('')
    const original = (json(doc) as any).content.map((b: any) => b.content[0].text).join('')
    ok(rejoined === original, 'the split changed the document text')
    // And the metrics describe what each half now holds, not what the whole did.
    const headMetrics = (host.registry.records['a'] as any).metrics
    ok(
      headMetrics.blocks === cut,
      `the head's metrics should count its own ${cut} blocks, got ${headMetrics.blocks}`,
    )
  })

  await test('a refused split leaves everything alone', async () => {
    // `applied: false` means the document is unchanged. The frontend does not re-read
    // anything on a refusal, so anything done before the reply would be invisible and
    // permanent.
    const host = new FakeHost(['a', 'b'])
    host.reply = { applied: false, section_ids: [], reason: 'section has 1 block(s) and cannot be split' }
    const before = JSON.stringify(host.registry.records['a'])

    const result = await applySplit(host as never, 'a', json(uniformDoc(4, 30)), {
      sectionId: 'a',
      offset: 3,
    })

    ok(!result.applied, 'the refusal should be reported')
    ok(host.registry.records['a-tail-1'] === undefined, 'no section may be created on a refusal')
    ok(host.restored === null, 'a refusal must not move the caret')
    ok(host.reindexed === null, 'a refusal must not reindex')
    ok(JSON.stringify(host.registry.records['a']) === before, 'a refusal must not touch the section')
    ok(host.ids().join() === 'a,b', `the document changed: ${host.ids().join()}`)
  })

  await test('a split of a one-block document never reaches the bridge', async () => {
    // `chooseCutIndex` decides there is nowhere to cut, so the coordinator must not ask.
    // An unnecessary request is a round trip spent being told no.
    const host = new FakeHost(['a'])
    const result = await applySplit(
      host as never,
      'a',
      { type: 'doc', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'only' }] }] },
      { sectionId: 'a', offset: 0 },
    )
    ok(!result.applied, 'a one-block document cannot split')
    ok(host.lastAction === null, `nothing should have been sent, got ${JSON.stringify(host.lastAction)}`)
    ok(/no interior block boundary/.test(result.reason ?? ''), `the reason should explain, got ${result.reason}`)
  })

  await test('a merge removes the source and keeps the target', async () => {
    // The target survives because it is the section the caret is in. Merging the other
    // way would move the user's section out from under them.
    const host = new FakeHost(['a', 'b', 'c'])
    host.reply = { applied: true, section_ids: ['a', 'c'], reason: null }

    const result = await applyMerge(host as never, 'b', 'a', { sectionId: 'b', offset: 5 })

    ok(result.applied, 'the merge should have applied')
    ok(host.registry.records['b'] === undefined, 'the source must be gone')
    ok(host.registry.records['a'] !== undefined, 'the target must survive')
    ok(host.lastAction?.kind === 'merge', `expected a merge, got ${host.lastAction?.kind}`)
    ok(host.reindexed?.join() === 'a,c', `the new ordering should be a,c, got ${host.reindexed?.join()}`)
    ok(host.ids().join() === 'a,c', `the registry's order should match, got ${host.ids().join()}`)
  })

  await test('a merge hands the caret to the target, not to the deleted section', async () => {
    // Resolved by `applyMerge` rather than left to the host, because the merge is what
    // knows the content moved into `into_section_id`. A host that had to reconstruct
    // that fact would be reconstructing something the caller already had — and the first
    // version of this test asserted the host's side and so passed while the real code
    // path left the caret pointing at a section that no longer existed.
    const host = new FakeHost(['a', 'b'])
    host.reply = { applied: true, section_ids: ['a'], reason: null }
    await applyMerge(host as never, 'b', 'a', { sectionId: 'b', offset: 7 })
    ok(
      host.restored?.sectionId === 'a',
      `the caret should land in the target, got ${host.restored?.sectionId}`,
    )
    ok(host.restored?.offset === 7, 'the offset should be preserved')
    ok(host.registry.records['b'] === undefined, 'the anchor\'s own section is gone, as expected')
  })

  await test('a reply whose ordering contradicts the request is an error', async () => {
    // A split adds exactly one section. A reply that does not means Rust and the
    // frontend have diverged, and proceeding would put the new section's content at the
    // wrong index — silently, with a scrollbar wrong for the rest of the session.
    // Three ways a reply can contradict a split that adds one section, and one of them
    // the weaker "is there an id at index+1" check passed.
    for (const [what, sectionIds] of [
      ['no new section', ['a', 'b']],
      ['two new sections', ['a', 'n1', 'n2', 'b']],
      ['the source repeated', ['a', 'a', 'b']],
    ] as Array<[string, string[]]>) {
      const host = new FakeHost(['a', 'b'])
      host.reply = { applied: true, section_ids: sectionIds, reason: null }
      let message = ''
      try {
        await applySplit(host as never, 'a', json(uniformDoc(4, 20)), { sectionId: 'a', offset: 1 })
      } catch (e: any) {
        message = e.message
      }
      ok(message !== '', `${what} should have been rejected`)
      ok(
        /reported success but its ordering/.test(message),
        `${what}: expected a contradiction error, got: ${message}`,
      )
    }
  })

  await test('acting on a section that is not in the document is an error', async () => {
    // Distinct from a refusal. "This section cannot be split" is a fact about the
    // document; "that section does not exist" means the two sides disagree, and reporting
    // `applied: false` would let the frontend carry on regardless.
    const host = new FakeHost(['a'])
    for (const call of [
      () => applySplit(host as never, 'ghost', json(uniformDoc(3, 10)), { sectionId: 'ghost', offset: 0 }),
      () => applyMerge(host as never, 'ghost', 'a', { sectionId: 'ghost', offset: 0 }),
    ]) {
      let message = ''
      try {
        await call()
      } catch (e: any) {
        message = e.message
      }
      ok(/not in the document/.test(message), `expected "not in the document", got: ${message}`)
    }
  })

  console.log('='.repeat(72))
  console.log(`${passed} passed, ${failed} failed`)
  if (failed) {
    console.log(`failing: ${failures.join(', ')}`)
    process.exit(1)
  }
}

/** A registry stand-in: records, ordering, and what the coordinator did to them. */
class FakeHost {
  registry: {
    records: Record<string, { id: string; json: unknown; metrics: unknown; dirty: boolean }>
    order: string[]
    record(id: string): { id: string; json: unknown; metrics: unknown; dirty: boolean } | undefined
    insert(index: number, record: { id: string; json: unknown; metrics: unknown }): void
    remove(id: string): void
  }
  lastAction: LifecycleAction | null = null
  restored: CaretAnchor | null = null
  reindexed: string[] | null = null
  reindexChange: GeometryChange | undefined = undefined
  reply: LifecycleResult = { applied: false, section_ids: [], reason: 'not configured' }

  constructor(ids: string[]) {
    const records: FakeHost['registry']['records'] = {}
    for (const id of ids) records[id] = { id, json: { type: 'doc', content: [] }, metrics: {}, dirty: false }
    this.registry = {
      records,
      order: [...ids],
      record: (id: string) => records[id],
      insert: (index, record) => {
        records[record.id] = { id: record.id, json: record.json, metrics: record.metrics, dirty: true }
        this.registry.order.splice(index, 0, record.id)
      },
      remove: id => {
        delete records[id]
        const at = this.registry.order.indexOf(id)
        if (at >= 0) this.registry.order.splice(at, 1)
      },
    }
  }

  /** The contract calls this `sectionIds`; named that way so the fake satisfies the
   *  interface rather than the test being shaped around the fake. */
  sectionIds(): string[] {
    return [...this.registry.order]
  }

  ids(): string[] {
    return this.sectionIds()
  }

  async commit(action: LifecycleAction): Promise<LifecycleResult> {
    this.lastAction = action
    // Returns the configured reply verbatim and does not touch the ordering.
    //
    // The first version spliced the new id into its own `order` as a stand-in for the
    // bridge's naming, and then `applySplit` inserted it too — so the fake reported
    // `a,a-1,a-1,b,c` and the test failed against correct code. A fake that simulates
    // the effect of the thing under test is a second implementation of it.
    return this.reply
  }

  reindex(ids: string[], change?: GeometryChange): void {
    this.reindexed = ids
    this.reindexChange = change
  }

  onApplied(_r: LifecycleResult, focus: CaretAnchor): void {
    this.restored = focus
  }
}

// -- seams and atoms -------------------------------------------------------
//
// The claim these hold: a split never leaves a boundary the user cannot cross back.
//
// That is not about cutting *inside* a table -- a seam is a top-level index, so that cannot
// happen -- it is about which seam gets chosen. `SectionRegistry.mergeBackward` refuses to
// move anything that is not a textblock, so a seam with an atom first in the tail creates a
// boundary that Backspace cannot undo. The browser test in `test/scroll.ts` closes the loop
// through a real editor; these pin the decision and the reason it can be trusted.

/** A document of `n` paragraphs, each `chars` long. */
function prose(n: number, chars = 100): unknown {
  return {
    type: 'doc',
    content: Array.from({ length: n }, (_, i) => ({
      type: 'paragraph',
      content: [{ type: 'text', text: `block ${i} ` + 'lorem ipsum '.repeat(Math.ceil(chars / 12)) }],
    })),
  }
}

const table: unknown = {
  type: 'table',
  content: [
    { type: 'tableRow', content: [{ type: 'tableCell', content: [{ type: 'paragraph', content: [{ type: 'text', text: 'a' }] }] }] },
  ],
}
const codeBlock: unknown = {
  type: 'codeBlock',
  attrs: { language: null },
  content: [{ type: 'text', text: 'fn main() {}' }],
}
const image: unknown = { type: 'image', attrs: { src: 'holo-asset://' + 'a'.repeat(64) } }

await test('a seam never leaves an atom first in the tail', () => {
  // The defect this prevents is one-way: Backspace at position 0 refuses to move a
  // non-textblock, so a section split in front of a table cannot be rejoined by the gesture
  // that exists to rejoin split sections.
  //
  // Twelve paragraphs then a table. Every interior boundary except the last is reversible,
  // and the cut is chosen among them -- never at 12, which would open the new section on a
  // table.
  for (const [tail, label] of [[table, 'table'], [codeBlock, 'code block'], [image, 'image']] as const) {
    const doc = { type: 'doc', content: [...(prose(12) as any).content, tail] }
    const cut = chooseCutIndex(doc)
    ok(cut !== null, `a 12-paragraph document followed by a ${label} should still split`)
    const blocks = (doc as any).content
    ok(
      !isAtomicBlock(blocks[cut!]),
      `the cut at ${cut} opens the tail on a ${label}; that seam cannot be Backspaced across`,
    )
    ok(cut! < 12, `the cut should avoid the seam at 12 entirely, got ${cut}`)
    return { label, cut }
  }
})

await test('a document whose only interior seam follows an atom refuses to split', () => {
  // Two blocks: a paragraph and a table. The only interior boundary is between them, and it
  // is irreversible. Splitting anyway would produce a section the user can type across but
  // never undo, so the right answer is to refuse and grow.
  ok(chooseCutIndex({ type: 'doc', content: [prose(1) && (prose(1) as any).content[0], table] }) === null,
    'a paragraph followed by a table must refuse to split')
  ok(chooseCutIndex({ type: 'doc', content: [(prose(1) as any).content[0], codeBlock] }) === null,
    'a paragraph followed by a code block must refuse to split')
  ok(chooseCutIndex({ type: 'doc', content: [(prose(1) as any).content[0], image] }) === null,
    'a paragraph followed by an image must refuse to split')
})

await test('a seam *before* an atom is still allowed when it is the better one', () => {
  // Refusing is the fallback, not the rule. With four paragraphs and a table at the end,
  // seams 1, 2 and 3 are all reversible and seam 4 is not, so the split happens -- it just
  // does not happen at 4. A rule that refused any document ending in a table would reject
  // the overwhelming majority of documents for no reason.
  const doc = { type: 'doc', content: [...(prose(4) as any).content, table] }
  const cut = chooseCutIndex(doc)
  ok(cut !== null, 'a document of prose ending in a table should split')
  ok(cut! <= 3, `the cut should not land on the irreversible seam, got ${cut}`)
  ok(seamIsReversible((doc as any).content, cut!), 'the chosen seam should be reversible')
  return { cut }
})

await test('the atom list agrees with the schema the editor installs', () => {
  // `mergeBackward` reads ProseMirror's own `atom` flag, while `ATOMIC_BLOCK_TYPES` is a
  // hand-written list. Two sources for one fact, so they are compared rather than trusted:
  // a node type the editor adds that is not listed would be accepted by `chooseCutIndex` and
  // then refused by the merge, which is the exact failure above.
  //
  // The schema here is the one the tests already build, and `test/scroll.ts` runs the same
  // comparison against the *installed* extensions, so a type that only exists in the app's
  // extension set is covered too.
    const fromSchema = atomicTypesFromSchema(schema.nodes as any)
  const listed = [...ATOMIC_BLOCK_TYPES].sort()
  const missing = fromSchema.filter(t => !ATOMIC_BLOCK_TYPES.has(t))
  ok(
    missing.length === 0,
    `the schema declares atoms this list omits: ${missing.join(', ')}. A seam in front of ` +
      'one of them would be accepted by chooseCutIndex and then refused by mergeBackward',
  )
  return { schemaAtoms: fromSchema, listed: listed.length }
})

await test('a single-block section still refuses to split', () => {
  // The original case, kept so the atom rule has not replaced it.
  ok(chooseCutIndex({ type: 'doc', content: [(prose(1) as any).content[0]] }) === null,
    'one block has no interior boundary')
  ok(chooseCutIndex({ type: 'doc', content: [] }) === null, 'an empty section has none either')
})

await test('an atom in the head does not make the seam irreversible', () => {
  // Only the tail's first block matters. A section that *ends* in a table merges forward
  // into its neighbour happily, because the block that moves is the neighbour's first.
  const doc = { type: 'doc', content: [(prose(1) as any).content[0], table, ...(prose(4) as any).content] }
  const cut = chooseCutIndex(doc)
  ok(cut !== null, 'should split')
  ok(cut! > 1, `the cut should be past the table, got ${cut}`)
  ok(!isAtomicBlock((doc as any).content[cut!]), 'the tail must not open on the table')
  return { cut }
})

// -- oversized blocks ------------------------------------------------------
//
// A section can be over its word limit for a reason no seam can fix: one table with 2,500
// words in it. A table is atomic, so it is the whole of one section or none of it. The
// requirement is that the splitter handles that gracefully -- snapping to a real block
// boundary, refusing rather than cutting inside an atom, and not panicking on the shapes a
// real document produces.

/** A table whose cells hold `words` words in total. */
function bigTable(words: number, rows = 25): unknown {
  const perCell = Math.max(1, Math.ceil(words / (rows * 2)))
  const filler = 'lorem ipsum dolor sit amet '.repeat(Math.ceil(perCell / 27)).slice(0, perCell)
  return {
    type: 'table',
    content: Array.from({ length: rows }, (_, r) => ({
      type: 'tableRow',
      content: Array.from({ length: 2 }, (_, c) => ({
        type: 'tableCell',
        content: [
          { type: 'paragraph', content: [{ type: 'text', text: `r${r}c${c} ${filler}` }] },
          { type: 'paragraph', content: [{ type: 'text', text: `${filler} tail ${filler}` }] },
        ],
      })),
    })),
  }
}

await test('a section that is one oversized table does not split', () => {
  // The required regression: 2,500 words in a single table, over the 1,500-word limit, with
  // no legal seam anywhere. Before the guard this returned a cut, and `applySplit` would have
  // written two sections that were each still over budget.
  const json = { type: 'doc', content: [bigTable(2500)] }
  const metrics = localMetrics(json)
  ok(metrics.words > MAX_WORDS_PER_SECTION, `the fixture must be over the limit: ${metrics.words} words`)
  ok(splitTrigger(metrics) === 'words', 'precondition: the section is over its word limit')
  ok(chooseCutIndex(json) === null, 'a single over-budget table has no legal seam and must refuse')
  ok(splitActionFor('s0', 0, json) === null, 'so no split action is even built')
  return { words: metrics.words }
})

await test('three oversized tables do not multiply into nine sections', () => {
  // The compounding failure. Each "split" would divide a table from a table, which the atomic
  // rule exists to prevent, and each half would be just as over budget, so the next keystroke
  // splits again: three sections, then nine, then twenty-seven.
  //
  // The reason it is refused is worth stating because the obvious guard is the wrong one: every
  // legal seam puts an atom first in the tail, so there is no reversible seam at all. A separate
  // "every block is over budget" rule was written, and the mutation run showed removing it
  // changed nothing — unreachable.
  const json = { type: 'doc', content: [bigTable(2000), bigTable(2200), bigTable(2400)] }
  ok(splitTrigger(localMetrics(json)) === 'words', 'precondition: over the limit')
  const nodes = (json as any).content
  for (let i = 1; i < nodes.length; i++) {
    ok(!seamIsReversible(nodes, i), `seam ${i} starts the tail on a table`)
  }
  ok(chooseCutIndex(json) === null, 'so no cut is chosen')
  return { words: localMetrics(json).words }
})

await test('over-budget prose is still split, because splitting it helps', () => {
  // The case that distinguishes the two. A 900-word paragraph is over half the limit and is
  // not atomic, so cutting between two of them makes two sections that are each smaller than
  // they were and can be divided again. A guard phrased "every block is over budget, so refuse"
  // would reject this, and reject it wrongly.
  //
  // This is why the refusal for a giant table rests on the seam rule and not on a budget rule.
  // 4,000 *characters* is about 740 words across four blocks -- under the limit. Sized from
  // characters and then asserted against words, which is the mistake worth naming: the limit is
  // in words and the fixture has to be built to it.
  const huge = 'lorem ipsum dolor sit amet '.repeat(160).slice(0, 8000)
  const json = {
    type: 'doc',
    content: [1, 2, 3, 4].map(i => ({
      type: 'paragraph',
      content: [{ type: 'text', text: `block ${i} ${huge}` }],
    })),
  }
  ok(splitTrigger(localMetrics(json)) === 'words', 'precondition: over the limit')
  const cut = chooseCutIndex(json)
  ok(cut !== null, 'a section of very long paragraphs should split')
  ok(cut === 2, `the midpoint of four equal blocks is 2, got ${cut}`)
  // And the halves are smaller than the whole, which is what makes the split worth doing.
  const head = (json as any).content.slice(0, cut)
  ok(head.length === 2, 'the head should hold half the blocks')
  return { cut }
})

await test('an oversized table among paragraphs is still splittable, at a paragraph seam', () => {
  // Refusing wholesale would be wrong. `paragraph, table(2500), paragraph` has two real seams
  // and dividing there is strictly better than not dividing: two over-budget sections instead
  // of one, at a boundary the user can undo.
  const json = { type: 'doc', content: [...(prose(6) as any).content, bigTable(2500), ...(prose(6) as any).content] }
  const cut = chooseCutIndex(json)
  ok(cut !== null, 'a document with prose either side of a giant table should still split')
  // Anywhere in the prose -- the share constraint is what chooses, and with the table holding
  // most of the weight the midpoint falls in the *trailing* prose. What must not happen is the
  // one seam at index 6, which opens the new section on the table.
  ok(cut !== 6, `the seam opening on the table must be refused, got ${cut}`)
  ok(cut !== null && !isAtomicBlock((json as any).content[cut]), 'the seam must not open on the table')
  ok(cut !== null && seamIsReversible((json as any).content, cut), 'and it must be reversible')
  return { cut }
})

await test('a trailing oversized table does not drag the cut onto it', () => {
  // The share constraint already handles this, but the table's weight is enormous and it is
  // the one case where the arithmetic could plausibly put the midpoint inside it.
  const json = { type: 'doc', content: [...(prose(8) as any).content, bigTable(9000)] }
  const cut = chooseCutIndex(json)
  ok(cut !== null, 'should split in the prose')
  ok(cut! <= 8, `the cut must stay in the prose, got ${cut}`)
  return { cut }
})

await test('the oversized blocks are named, so a caller can log once instead of every keystroke', () => {
  // The coordinator asks `splitTrigger` on every keystroke and is told yes forever. This is
  // what lets it stop asking.
  const json = { type: 'doc', content: [...(prose(4) as any).content, bigTable(2500), ...(prose(4) as any).content] }
  const blocks = unsplittableBlocks(json)
  ok(blocks.length === 1, `expected one oversized atom, got ${blocks.length}`)
  ok(blocks[0]!.type === 'table', `expected a table, got ${blocks[0]!.type}`)
  ok(blocks[0]!.index === 4, `expected index 4, got ${blocks[0]!.index}`)
  ok(blocks[0]!.weight > MAX_WORDS_PER_SECTION, `weight should exceed the limit, got ${blocks[0]!.weight}`)

  // And nothing is reported for a section that is merely long rather than un-splittable.
  const proseOnly = unsplittableBlocks({ type: 'doc', content: (prose(40) as any).content })
  ok(proseOnly.length === 0, `long prose is splittable; got ${JSON.stringify(proseOnly)}`)
})

await test('pathological shapes return null rather than throwing', () => {
  // The other half of "gracefully": a section from a half-written paste, or one whose content
  // is not an array, must produce an answer rather than an exception. The editor is live while
  // this runs, and a throw in the splitter would be a keystroke that does nothing at all.
  const cases: Array<[string, unknown]> = [
    ['null document', null],
    ['undefined', undefined],
    ['a string', 'not a document'],
    ['a number', 42],
    ['no content key', { type: 'doc' }],
    ['content not an array', { type: 'doc', content: 'paragraph' }],
    ['content null', { type: 'doc', content: null }],
    ['empty content', { type: 'doc', content: [] }],
    ['one null block', { type: 'doc', content: [null] }],
    ['two null blocks', { type: 'doc', content: [null, null] }],
    ['a block with no type', { type: 'doc', content: [{ content: [] }, { content: [] }] }],
    ['a cycle-free deep nest', { type: 'doc', content: [nest(400)] }],
  ]
  for (const [what, json] of cases) {
    let cut: number | null | undefined
    let threw: string | null = null
    try {
      cut = chooseCutIndex(json)
    } catch (e: any) {
      threw = e.message
    }
    ok(threw === null, `chooseCutIndex threw on ${what}: ${threw}`)
    ok(cut === null || (typeof cut === 'number' && Number.isInteger(cut)), `bad cut for ${what}: ${cut}`)
    // The metrics path must be equally total, since it is what decides to ask.
    let metrics: unknown
    let metricsThrew: string | null = null
    try {
      metrics = localMetrics(json)
    } catch (e: any) {
      metricsThrew = e.message
    }
    ok(metricsThrew === null, `localMetrics threw on ${what}: ${metricsThrew}`)
    ok(
      metrics !== null && typeof (metrics as any).words === 'number' && Number.isFinite((metrics as any).words),
      `bad metrics for ${what}: ${JSON.stringify(metrics)}`,
    )
  }
  return { cases: cases.length }
})

await test('a cut index always lands on a real boundary of the document it came from', () => {
  // The invariant the whole seam design rests on, checked across the shapes above as well as
  // the realistic ones: a cut is an index into `content`, so `content.slice(0, cut)` and
  // `content.slice(cut)` must both be non-empty and together cover everything.
  const docs: unknown[] = [
    { type: 'doc', content: (prose(12) as any).content },
    { type: 'doc', content: [...(prose(12) as any).content, bigTable(2500)] },
    { type: 'doc', content: [...(prose(6) as any).content, bigTable(2500), ...(prose(6) as any).content] },
    { type: 'doc', content: [{ type: 'codeBlock', content: [{ type: 'text', text: 'fn main() {}' }] }, ...(prose(9) as any).content] },
  ]
  for (const doc of docs) {
    const cut = chooseCutIndex(doc)
    if (cut === null) continue
    const blocks = (doc as any).content
    ok(cut > 0, `cut ${cut} would leave an empty head`)
    ok(cut < blocks.length, `cut ${cut} would leave an empty tail`)
    ok(
      blocks.slice(0, cut).length + blocks.slice(cut).length === blocks.length,
      'the two halves must cover every block exactly once',
    )
  }
  return { docs: docs.length }
})

/** A block nested `depth` levels deep, for the stack-depth check. */
function nest(depth: number): unknown {
  let node: unknown = { type: 'text', text: 'x' }
  for (let i = 0; i < depth; i++) {
    node = { type: i % 2 === 0 ? 'blockquote' : 'tableCell', content: [node] }
  }
  return node
}

await main()

// ============================================================================
// Directive 1: the 1,500-word boundary must not fall inside a table
// ============================================================================
//
// The headline requirement, stated as a fixture: a 1,600-word section carrying a
// 300-word table positioned so the word limit lands *inside* the table. The seam has
// to snap to a real block boundary, and the table has to survive the split whole.
//
// Why this is a real hazard and not a hypothetical: the split trigger counts words and
// the cut chooser indexes blocks. Those are different granularities, so between them
// lies a region -- the interior of an atomic block -- where the limit says "split here"
// and the representation says "you may not". `chooseCutIndex` answers it by never
// emitting a position inside a block at all, so the question becomes whether the
// *chosen* boundary keeps the table whole, which is what the assertions below check
// against a real ProseMirror schema rather than a JSON shape.

/** A schema with real table and code-block nodes, so "not broken" means what it says. */
const tableSchema = new Schema({
  nodes: {
    ...schema.spec.nodes.toObject(),
    table: { content: 'tableRow+', group: 'block', isolating: true, atom: true, toDOM: () => ['table', 0] },
    tableRow: { content: 'tableCell+', toDOM: () => ['tr', 0] },
    tableCell: { content: 'block+', isolating: true, toDOM: () => ['td', 0] },
    codeBlock: { content: 'text*', group: 'block', code: true, isolating: true, atom: true, toDOM: () => ['pre', 0] },
  },
})

/**
 * A run of text holding approximately `n` words.
 *
 * Sized by *word count*, not character count, because the threshold under test is
 * 1,500 words. The first version of this fixture repeated a 40-character phrase 200
 * times per paragraph and came to 6,230 words — three times the intended size, and it
 * still passed every assertion except the one that checked the fixture was the shape
 * the requirement describes. A test that cannot tell the difference between the case it
 * was written for and one four times larger is not testing the case.
 */
function wordsOf(n: number): string {
  const out: string[] = []
  for (let i = 0; out.length < n; i++) out.push(`w${i}`)
  return out.slice(0, n).join(' ')
}

/** A ProseMirror `Node`, or a throw naming the block that failed. */
function parseDoc(j: any): Node {
  try {
    return tableSchema.nodeFromJSON(j)
  } catch (e) {
    throw new Error(`${(e as Error).message} -- in ${JSON.stringify(j).slice(0, 200)}`)
  }
}

/** Split like `applySplit` does, returning both halves as real nodes. */
function splitHalves(doc: unknown): { head: Node; tail: Node; cut: number } {
  const cut = chooseCutIndex(doc)
  if (cut === null) throw new Error('chooseCutIndex refused, so there is no split to inspect')
  const content = (doc as any).content as unknown[]
  return {
    cut,
    head: parseDoc({ type: 'doc', content: content.slice(0, cut) }),
    tail: parseDoc({ type: 'doc', content: content.slice(cut) }),
  }
}

const countTables = (j: any): number => {
  if (!j || typeof j !== 'object') return 0
  const here = j.type === 'table' ? 1 : 0
  return here + (Array.isArray(j.content) ? j.content.reduce((a: number, c: any) => a + countTables(c), 0) : 0)
}

/** ProseMirror words, counted the way the split trigger counts them. */
function wordsIn(j: any): number {
  let text = ''
  const visit = (n: any): void => {
    if (typeof n?.text === 'string') {
      text += n.text
      return
    }
    if (Array.isArray(n?.content)) n.content.forEach(visit)
    if (text && !text.endsWith('\n') && ['paragraph', 'heading', 'tableCell'].includes(n?.type)) text += '\n'
  }
  visit(j)
  return text.split(/\s+/).filter(w => w.length > 0).length
}

async function directive1() {
  console.log('='.repeat(72))
  console.log('directive 1: the word limit must not cut a table in half')
  console.log('='.repeat(72))

  await test('a 1,600-word section with a 300-word table at the threshold splits cleanly', () => {
    // 1,300 words of prose across five paragraphs, then a 300-word table: 1,600 in
    // total, so the limit fires, and the limit is crossed 200 words *into* the table.
    // That is the case the requirement describes -- a word boundary that lands inside
    // an atomic block.
    const paragraphs = [1, 2, 3, 4, 5].map(i => ({
      type: 'paragraph',
      content: [{ type: 'text', text: `paragraph ${i} ${wordsOf(260)}` }],
    }))
    const table = bigTable(300, 10)
    const doc = { type: 'doc', content: [...paragraphs, table] }

    const words = wordsIn(doc)
    ok(
      words > MAX_WORDS_PER_SECTION && words < MAX_WORDS_PER_SECTION + 200,
      `the fixture must be just over the 1,500-word limit, got ${words}`,
    )
    ok(splitTrigger(localMetrics(doc)) === 'words', 'precondition: the split trigger fires')

    const { head, tail, cut } = splitHalves(doc)

    // The hard invariant: the table exists in exactly one half, whole.
    const inHead = countTables(head.toJSON())
    const inTail = countTables(tail.toJSON())
    ok(
      inHead + inTail === 1,
      `the table must survive the split exactly once, found ${inHead} in the head and ${inTail} in the tail`,
    )

    // And it must be the *whole* table: every row intact, not a prefix. This is the
    // assertion that fails if the seam ever moves inside the table.
    const where = inHead === 1 ? head.toJSON() : tail.toJSON()
    const originalRows = (table as any).content.length
    const surviving = (where.content as any[]).find(n => n.type === 'table')
    ok(surviving !== null, 'the surviving table should be a top-level block')
    ok(
      surviving.content.length === originalRows,
      `the table must keep all ${originalRows} rows, got ${surviving?.content?.length}`,
    )

    // No broken schema structure: both halves are real, mountable documents. A cut
    // inside the table would put a `tableRow` at a top level, and this throws.
    ok(head.childCount > 0, 'the head must not be empty')
    ok(tail.childCount > 0, 'the tail must not be empty')
    parseDoc(head.toJSON())
    parseDoc(tail.toJSON())

    // The cut lands on a block boundary, so the tail never opens on the table: a seam
    // the user could not Backspace across would be a one-way seam.
    ok(!isAtomicBlock((doc as any).content[cut]), `the tail must not open on the table, got block ${cut}`)
    ok(seamIsReversible((doc as any).content, cut), 'the seam must be reversible by a merge')

    // No content invented or lost. Words are conserved, which is stronger than "the
    // table arrived" and is the property that catches a dropped row.
    ok(
      wordsIn(head.toJSON()) + wordsIn(tail.toJSON()) === words,
      `words must be conserved: ${wordsIn(head.toJSON())} + ${wordsIn(tail.toJSON())} != ${words}`,
    )

    return { words, cut, headBlocks: head.childCount, tailBlocks: tail.childCount, inHead }
  })

  await test('splitting every over-limit section repeatedly still never cuts the table', () => {
    // The compounding failure. A section is not split once and left alone: it is split
    // again the next time either half passes the limit, so a seam that wandered inside
    // the table eventually would. This drives that loop to convergence over real
    // multi-block sections.
    //
    // The first version wrapped each *block* in its own document and asked for a cut,
    // which can never succeed — one block has no interior boundary — so it ran zero
    // rounds and passed vacuously. A test that exercises nothing and asserts only its
    // own emptiness is worse than no test, because it reports coverage.
    let sections: unknown[][] = [
      [1, 2].map(i => ({ type: 'paragraph', content: [{ type: 'text', text: `p${i} ${wordsOf(700)}` }] })),
      [bigTable(300, 10)],
      // Sized so each *half* is still over the limit. A section of 3,000 words splits
      // into two 1,500-word halves, neither over, and the loop stops after one round —
      // which is correct, and tests nothing about repeated splitting. 4,800 words
      // leaves both halves at 2,400, so each is split again, and the recursion is
      // actually exercised.
      [4, 5, 6].map(i => ({ type: 'paragraph', content: [{ type: 'text', text: `p${i} ${wordsOf(1600)}` }] })),
    ]
    const originalWords = wordsIn({ type: 'doc', content: sections.flat() })

    let rounds = 0
    for (let pass = 0; pass < 200; pass++) {
      // Find a section that is both over the limit *and* has a legal seam. Searching for
      // "over the limit" and then giving up when it has no seam was the first version's
      // bug: a section of one over-limit paragraph cannot be cut, so the loop stopped
      // there while other sections were still splittable, and the test reported one
      // round for a document that needed four.
      let at = -1
      let cut = -1
      for (let i = 0; i < sections.length; i++) {
        if (splitTrigger(localMetrics({ type: 'doc', content: sections[i]! })) === null) continue
        const c = chooseCutIndex({ type: 'doc', content: sections[i]! })
        if (c === null) continue // over limit, but a lone atom: nothing legal to do
        at = i
        cut = c
        break
      }
      if (at < 0) break
      const halves = splitHalves({ type: 'doc', content: sections[at]! })
      sections.splice(at, 1, halves.head.toJSON().content as unknown[], halves.tail.toJSON().content as unknown[])
      rounds++
      ok(cut > 0, 'a cut must land on an interior boundary')
    }

    ok(rounds > 1, `the recursion must actually recur, got ${rounds} rounds`)
    const flat = sections.flat()
    ok(countTables({ type: 'doc', content: flat }) === 1, 'exactly one table must remain')
    ok(
      wordsIn({ type: 'doc', content: flat }) === originalWords,
      `repeated splitting must conserve words: got ${wordsIn({ type: 'doc', content: flat })}, had ${originalWords}`,
    )
    // The table is whole: every section parses, and no block is a stray row or cell
    // that lost its parent.
    for (const s of sections) {
      const d = parseDoc({ type: 'doc', content: s })
      ok(d.childCount > 0, 'a section must not be empty')
      for (const b of s as any[]) {
        ok(
          !['tableRow', 'tableCell', 'paragraph'].includes(b.type) || b.type === 'paragraph',
          `a stray ${b.type} reached the top level of a section`,
        )
      }
    }
    return { rounds, sections: sections.length, words: originalWords }
  })

  await test('a code block at the threshold is treated the same as a table', () => {
    // `codeBlock` is in `ATOMIC_BLOCK_TYPES` for the same reason `table` is: a seam in
    // front of it cannot be crossed back by a merge.
    const code = { type: 'codeBlock', content: [{ type: 'text', text: wordsOf(300) }] }
    const doc = {
      type: 'doc',
      content: [
        ...[1, 2, 3].map(i => ({
          type: 'paragraph',
          content: [{ type: 'text', text: `intro ${i} ${wordsOf(430)}` }],
        })),
        code,
        ...[4, 5].map(i => ({
          type: 'paragraph',
          content: [{ type: 'text', text: `outro ${i} ${wordsOf(430)}` }],
        })),
      ],
    }
    ok(splitTrigger(localMetrics(doc)) === 'words', 'precondition: over the limit')

    const cut = chooseCutIndex(doc)
    ok(cut !== null, 'this fixture has legal seams and must choose one')
    ok(!isAtomicBlock((doc as any).content[cut]), 'the tail must not open on the code block')
    ok(seamIsReversible((doc as any).content, cut!), 'the seam must be reversible')

    const { head, tail } = splitHalves(doc)
    // Compare the block itself, not a serialization of the whole half. The first
    // version string-matched `{"type":"doc","content":[codeBlock...]}`, which only
    // holds when the code block is the half's *first* child — so the assertion passed
    // or failed depending on where the cut happened to land, and reported a broken
    // seam for a seam that was fine. Reading the node out and comparing it is what was
    // meant.
    const halves = [head.toJSON(), tail.toJSON()] as Array<{ content?: Array<{ type: string }> }>
    const containing = halves.filter(h => (h.content ?? []).some(n => n.type === 'codeBlock'))
    ok(containing.length === 1, `the code block must be whole in exactly one half, found ${containing.length}`)
    const block = (containing[0]!.content ?? []).find(n => n.type === 'codeBlock')!
    ok(
      JSON.stringify(block) === JSON.stringify(code),
      'the code block must survive byte-for-byte, not be truncated or rewrapped',
    )
    return { cut, headBlocks: head.childCount, tailBlocks: tail.childCount }
  })

  console.log('='.repeat(72))
  console.log(`${passed} passed, ${failed} failed`)
  if (failed) {
    console.error(`directive 1 FAILED: ${failures.join('; ')}`)
    process.exit(1)
  }
}

await directive1()

/** Every text character under a block, however deeply nested. */
function textLengthOf(node: any): number {
  if (typeof node?.text === 'string') return node.text.length
  if (!Array.isArray(node?.content)) return 0
  return node.content.reduce((a: number, c: any) => a + textLengthOf(c), 0)
}
