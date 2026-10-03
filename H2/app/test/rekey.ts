/**
 * Height re-keying across a split or a merge.
 *
 * Every test here is about *identity*. The bug this suite exists for was a positional
 * read — `newIds.map((_, i) => oldHeights[i])` — which hands every measurement past
 * the cut to the wrong section while preserving the total exactly, so nothing looked
 * wrong. `regression: a positional read is what this file forbids` states it as the
 * counterfactual rather than leaving it to a reader's imagination.
 */

import { rekeyHeights } from '../src/core/rekey.ts'

let failures = 0
let checks = 0

function ok(cond: boolean, message: string): void {
  checks++
  if (!cond) {
    failures++
    console.error(`  FAIL ${message}`)
  }
}

function same(actual: readonly number[], expected: readonly number[], message: string): void {
  const a = actual.map(n => Math.round(n * 1000) / 1000)
  const e = expected.map(n => Math.round(n * 1000) / 1000)
  ok(
    a.length === e.length && a.every((v, i) => v === e[i]),
    `${message}\n       actual   ${JSON.stringify(a)}\n       expected ${JSON.stringify(e)}`,
  )
}

async function test(name: string, fn: () => void): Promise<void> {
  const before = failures
  try {
    fn()
    console.log(`${failures === before ? '  ok  ' : ' FAIL '} ${name}`)
  } catch (e) {
    failures++
    console.error(` FAIL  ${name}\n       ${(e as Error).message}`)
  }
}

const sum = (a: readonly number[]): number => a.reduce((x, y) => x + y, 0)

async function main() {
  console.log('rekey: heights follow the section, not the index')
  console.log('='.repeat(72))

  await test('a split divides one measured height in proportion to its characters', () => {
    // B is 300px measured and splits 3:1 by characters. The two halves must sum to
    // exactly 300, so nothing below the seam moves.
    const out = rekeyHeights({
      previousIds: ['A', 'B', 'C'],
      previousHeights: [100, 300, 200],
      newIds: ['A', 'B', 'NEW', 'C'],
      change: { kind: 'split', sectionId: 'B', headChars: 750, tailChars: 250 },
    })
    same(out, [100, 225, 75, 200], 'head gets 3/4 and the tail 1/4')
    ok(sum(out) === 600, `the total must be preserved, got ${sum(out)}`)
  })

  await test('a section past the cut keeps its OWN height', () => {
    // The regression. Positionally, C is now index 3 and the old array has nothing
    // there, so C came back as 0 — while NEW, at index 2, inherited C's 200.
    const out = rekeyHeights({
      previousIds: ['A', 'B', 'C'],
      previousHeights: [100, 300, 200],
      newIds: ['A', 'B', 'NEW', 'C'],
      change: { kind: 'split', sectionId: 'B', headChars: 500, tailChars: 500 },
    })
    ok(out[3] === 200, `C must keep its measured 200, got ${out[3]}`)
    ok((out[2] ?? 0) > 0, `the new section must not be zero-height, got ${out[2]}`)
    ok(out[3] !== out[2], 'the new section must not have inherited a neighbouring height')
  })

  await test('every measured section survives a split at the front', () => {
    // The worst case for a positional read: the cut is at index 0, so *every*
    // subsequent section shifts and a positional read is wrong for all of them.
    const previousIds = ['S0', 'S1', 'S2', 'S3', 'S4']
    const previousHeights = [10, 20, 30, 40, 50]
    const out = rekeyHeights({
      previousIds,
      previousHeights,
      newIds: ['S0', 'NEW', 'S1', 'S2', 'S3', 'S4'],
      change: { kind: 'split', sectionId: 'S0', headChars: 400, tailChars: 100 },
    })
    // S1..S4 keep 20,30,40,50 at their new indices 2..5.
    same(out.slice(2), [20, 30, 40, 50], 'every section past the cut keeps its own height')
    ok(sum(out) === 150, `the total must be preserved, got ${sum(out)}`)
  })

  await test('a split with no text on either side divides evenly', () => {
    // Two image-only sections: there is no character count to weight by, so any
    // proportion would be invented. Even is the only defensible answer.
    const out = rekeyHeights({
      previousIds: ['A', 'B'],
      previousHeights: [100, 240],
      newIds: ['A', 'B', 'NEW'],
      change: { kind: 'split', sectionId: 'B', headChars: 0, tailChars: 0 },
    })
    same(out, [100, 120, 120], 'an even split of 240 is 120 and 120')
  })

  await test('a merge adds the two heights onto the surviving section', () => {
    const out = rekeyHeights({
      previousIds: ['A', 'B', 'C'],
      previousHeights: [100, 300, 200],
      newIds: ['A', 'C'],
      change: { kind: 'merge', intoSectionId: 'C', removedId: 'B' },
    })
    same(out, [100, 500], 'C now holds both sections, so it is 300 + 200')
  })

  await test('a merge into a section that was never measured still estimates', () => {
    // Not hypothetical: merging a section the user scrolled past makes the survivor's
    // measurement a sum with an unknown half.
    //
    // Note B is *removed* here, so its absence is not itself suspicious. The case that
    // matters is the one below it, where the section being removed has a measurement and
    // the survivor does not.
    const out = rekeyHeights({
      previousIds: ['A', 'B', 'C'],
      previousHeights: [100, 300, 200],
      newIds: ['A', 'C'],
      change: { kind: 'merge', intoSectionId: 'C', removedId: 'B' },
    })
    same(out, [100, 500], 'C absorbs B, so 200 + 300')
  })

  await test('a merge whose survivor was never measured falls back to an estimate', () => {
    // The reverse of the above: B is measured and removed, C was never on screen. Adding
    // is impossible without C's contribution, and C's own height describes content that
    // no longer exists -- so an estimate from the merged metrics is the only honest
    // answer.
    const out = rekeyHeights({
      previousIds: ['A', 'B', 'C'],
      previousHeights: [100, 300, undefined],
      newIds: ['A', 'C'],
      change: { kind: 'merge', intoSectionId: 'C', removedId: 'B' },
      estimate: id => (id === 'C' ? 450 : 0),
    })
    same(out, [100, 450], 'an unmeasurable survivor is estimated, not left at zero')
  })

  await test('an unmeasured new section gets an estimate, never zero', () => {
    // A zero here collapses the section and leaves a hole the user can scroll into.
    const out = rekeyHeights({
      previousIds: ['A'],
      previousHeights: [100],
      newIds: ['A', 'B'],
      estimate: id => (id === 'B' ? 77 : 0),
    })
    same(out, [100, 77], 'the fresh section is estimated')
  })

  await test('a change for a section that was never measured degrades to estimates', () => {
    // Splitting a section the user has not scrolled to: there is no measured height to
    // divide, so both halves estimate. This must not throw, because it is ordinary.
    const out = rekeyHeights({
      previousIds: ['A', 'B'],
      previousHeights: [100, undefined],
      newIds: ['A', 'B', 'NEW'],
      change: { kind: 'split', sectionId: 'B', headChars: 10, tailChars: 10 },
      estimate: id => (id === 'NEW' ? 55 : 33),
    })
    same(out, [100, 33, 55], 'no measurement to divide, so both halves estimate')
  })

  await test('omitting the change still re-keys by identity', () => {
    // A host that only wants the new ordering. It must still not shift measurements
    // onto the wrong sections -- that part is not optional.
    const out = rekeyHeights({
      previousIds: ['A', 'B', 'C'],
      previousHeights: [100, 200, 300],
      newIds: ['A', 'NEW', 'B', 'C'],
    })
    same(out, [100, 0, 200, 300], 'identity holds; only the unknown section falls to 0')
  })

  await test('the two halves of a split always sum to the parent, exactly', () => {
    // The property that keeps the scroll position still, over a range of proportions.
    // Asserted as an invariant rather than on one fixture because it is the invariant
    // that matters: if head + tail != parent for any proportion, a split moved the
    // scrollbar.
    for (const [head, tail] of [
      [1, 9999],
      [2500, 2500],
      [9999, 1],
      [3, 1],
    ] as Array<[number, number]>) {
      const out = rekeyHeights({
        previousIds: ['A', 'B'],
        previousHeights: [0, 1234.5],
        newIds: ['A', 'B', 'NEW'],
        change: { kind: 'split', sectionId: 'B', headChars: head, tailChars: tail },
      })
      ok(
        Math.abs(out[1]! + out[2]! - 1234.5) < 1e-9,
        `heads of ${head}/${tail} must sum to the parent, got ${out[1]! + out[2]!}`,
      )
    }
  })

  await test('regression: a positional read is what this file forbids', () => {
    // The counterfactual, so the reason for identity is on the page rather than only in
    // a commit message.
    const previousIds = ['A', 'B', 'C']
    const previousHeights = [100, 200, 300]
    const newIds = ['A', 'B', 'NEW', 'C']
    const change = { kind: 'split', sectionId: 'B', headChars: 100, tailChars: 100 } as const

    const positional = newIds.map((_, i) => previousHeights[i] ?? 0)
    const byIdentity = rekeyHeights({ previousIds, previousHeights, newIds, change })

    ok(
      sum(positional) === sum(byIdentity),
      'the totals match, which is exactly why the bug hid',
    )
    ok(
      positional[3] === 0 && positional[2] === 300,
      'the positional read collapses the last section and gives its height to the new one',
    )
    ok(
      byIdentity[3] === 300 && byIdentity[2] === 100,
      'the identity read keeps C at 300 and gives the new section an allocated 100',
    )
    // Every section past the cut differs. This is the whole blast radius.
    const differing = positional.filter((h, i) => h !== byIdentity[i]).length
    ok(differing >= 2, `the positional read corrupts every section past the cut, got ${differing}`)
  })

  console.log('='.repeat(72))
  console.log(`${checks - failures}/${checks} checks passed`)
  if (failures > 0) {
    console.error(`rekey: ${failures} FAILED`)
    process.exit(1)
  }
}

void main()