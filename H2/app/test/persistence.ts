/**
 * The persistence loop's frontend half: the cache, the hydrator's generation guard, and
 * the eviction flush.
 *
 * # Why these run in Node
 *
 * Nothing here touches layout or the DOM. `SectionCache` is a data structure,
 * `SectionHydrator` is a closure over an injected transport and an injected window
 * predicate, and `EditCommitter`'s throttle is a timer. A browser would add a server and
 * a page and test nothing extra.
 *
 * The unmount-persistence test *does* need an editor, and it is in `test/scroll.ts`
 * against the product page, because that is the only place a real ProseMirror instance
 * and the real registry both exist.
 *
 * Run: node --experimental-strip-types test/persistence.ts
 */

import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

import { EditCommitter } from '../src/core/edit-commit.ts'
import { SectionCache, CONTENT_CACHE_CAPACITY } from '../src/core/section-cache.ts'
import { SectionHydrator, type SectionFetcher } from '../src/core/section-hydrator.ts'
import type { CommitResponse, SectionContent } from '../src/core/boot.ts'

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

const fixtures = join(dirname(fileURLToPath(import.meta.url)), 'fixtures')

/**
 * Real zstd frames, from the real Rust encoder, with distinguishable text.
 *
 * # Why real frames and not a stub
 *
 * The first version of this file used an empty `Uint8Array`, on the reasoning that the
 * guard being tested is about ordering and not decoding. That was true and it hid a
 * second bug: the test that checks *which* response was applied needs the two responses
 * to be distinguishable, and identical decodable content would not distinguish them
 * either. So the frames have to decode, and a hand-written stub would have been a third
 * thing that only the test believed.
 *
 * `rust-zstd-{a,b,c}.bin` were written by `holonomy_core::store::encode` and hold
 * "section A", "section B" and "section C". `app/test/boot.ts` and
 * `crates/holonomy-core/tests/zstd-interop.rs` both read them from the other side.
 */
const FRAMES = {
  'content of s0': readFileSync(join(fixtures, 'rust-zstd-a.bin')),
  'content of s1': readFileSync(join(fixtures, 'rust-zstd-b.bin')),
  'content of s2': readFileSync(join(fixtures, 'rust-zstd-c.bin')),
} as Record<string, Uint8Array>

/** A frame holding distinguishable text, in the shape `get_section` returns. */
function content(id: string, label: string): SectionContent {
  const bytes = FRAMES[label]
  if (!bytes) throw new Error(`no fixture frame for ${label}; add one to app/test/fixtures`)
  return { id, content_zstd: new Uint8Array(bytes) }
}

/**
 * A fetcher whose resolutions the test controls.
 *
 * `resolveAll` is what makes the fling test deterministic: it decides the order responses
 * come back in, which is the entire subject.
 */
/**
 * A fetcher whose resolutions the test controls.
 *
 * `resolveNewest` and `resolveOldest` are what make the ordering tests deterministic: they
 * decide the order responses come back in, which is the entire subject. A test that let
 * them resolve themselves would pass or fail by timing.
 *
 * Each request is answered with a *different* frame, assigned when the request is issued
 * and carried on the pending entry. Assigning it at resolution time looked equivalent and
 * was not: the label was looked up by position in a list the resolution had already
 * removed itself from, so every response got the first frame and a test asserting *which*
 * response applied could not tell them apart.
 */
function controlledFetcher() {
  type Pending = { id: string; label: string; resolve: (c: SectionContent) => void }
  const FRAME_LABELS = ['content of s0', 'content of s1', 'content of s2']
  const pending: Pending[] = []
  const requested: string[] = []

  const fetchSection: SectionFetcher = id => {
    requested.push(id)
    // `- 1` because `requested` has already been pushed to, so without it the first
    // request would get the second frame and a test naming frames by order would be off
    // by one -- which it was.
    const label = FRAME_LABELS[(requested.length - 1) % FRAME_LABELS.length]!
    return new Promise<SectionContent>(resolve => pending.push({ id, label, resolve }))
  }

  const answer = (entry: Pending): void => {
    const at = pending.indexOf(entry)
    if (at >= 0) pending.splice(at, 1)
    entry.resolve(content(entry.id, entry.label))
  }

  return {
    fetchSection,
    requested,
    get outstanding() {
      return pending.length
    },
    answer,
    resolveOldest() {
      const entry = pending.shift()
      if (!entry) throw new Error('nothing outstanding')
      answer(entry)
    },
    resolveNewest() {
      const entry = pending.pop()
      if (!entry) throw new Error('nothing outstanding')
      answer(entry)
    },
    /** Resolve in reverse order, which is what a fling looks like from here. */
    resolveAllReversed() {
      while (pending.length) answer(pending.pop()!)
    },
  }
}

async function main() {
  console.log('persistence: the cache, the generation guard, and the eviction flush')
  console.log('='.repeat(72))

  // -----------------------------------------------------------------------
  // The cache

  await test('the cache holds thirty sections and no more', () => {
    ok(CONTENT_CACHE_CAPACITY === 30, `the cap should be 30, got ${CONTENT_CACHE_CAPACITY}`)
    const cache = new SectionCache()
    for (let i = 0; i < 100; i++) cache.set(`s${i}`, { text: `section ${i}` })
    ok(cache.size === 30, `expected 30 resident, got ${cache.size}`)
    return cache.inspect()
  })

  await test('the least recently used section is the one evicted', () => {
    // LRU, not FIFO. Using `s0` must save it and cost `s1` instead, because a user who
    // scrolls back to a section they just read is the case FIFO gets wrong.
    const cache = new SectionCache(3)
    cache.set('a', 1)
    cache.set('b', 2)
    cache.set('c', 3)
    ok(cache.get('a'), 'a should be resident')

    const victim = cache.set('d', 4)
    ok(victim === 'b', `b was least recently used, but the victim was ${victim}`)
    ok(!cache.includes('b'), 'b should have been evicted')
    ok(cache.includes('a') && cache.includes('c') && cache.includes('d'), 'a, c and d should remain')
    return { victim }
  })

  await test('a re-set of a resident section occupies one slot and refreshes it', async () => {
    // Two properties, and the first version of this test asserted the wrong second one.
    //
    // A re-set must not double-count: otherwise a section edited repeatedly pushes every
    // other section out while holding one slot, and the cache holds ten sections instead
    // of thirty.
    //
    // And a re-set must *touch*, because writing to a section is using it. The earlier
    // expectation -- that fifty writes to 'a' would keep it resident after three more
    // sections arrived -- was simply wrong: 'a' is then the least recently used section
    // in the cache, and evicting it is correct. What must hold is that touching it moved
    // it behind nothing else, so the eviction falls on the right section.
    const cache = new SectionCache(3)
    for (let i = 0; i < 50; i++) cache.set('a', { revision: i })
    // Read through a local: the assertion above narrows `size` to the literal 1, and the
    // count genuinely does change on the next two lines.
    const afterRepeats = cache.size
    ok(afterRepeats === 1, `one section should occupy one slot, got ${afterRepeats}`)

    cache.set('b', 1)
    cache.set('c', 2)
    ok(cache.size === 3, `three distinct sections, got ${cache.size}`)
    cache.set('a', { revision: 50 }) // touch
    cache.set('d', 3)
    ok(!cache.includes('b'), 'b is now the least recently used, so it should be the victim')
    ok(cache.includes('a'), 'the touched section should have survived')
    return cache.inspect()
  })

  await test('the cache is bounded even when every set is a miss', () => {
    // The bound is the property. A cache that only bounds itself when there are hits is
    // not a bound.
    for (let round = 0; round < 5; round++) {
      const cache = new SectionCache(30)
      for (let i = 0; i < 200; i++) cache.set(`r${round}s${i}`, i)
      ok(cache.size === 30, `round ${round}: expected 30, got ${cache.size}`)
    }
  })

  // -----------------------------------------------------------------------
  // The generation guard

  await test('a fling discards every in-flight response', async () => {
    // The required test. A fling requests a dozen sections and the user is somewhere else
    // before any of them answers. Without the guard each response applies, and each one
    // inserts a cache entry for a section that will not be mounted -- which is how the
    // cache fills with a fling's worth of content and evicts the working set.
    //
    // The window moves once per request, so every request but the last is stale by the
    // time it resolves. Resolutions are then forced in reverse order, which is what a
    // fling looks like from this side and which a first-in-first-out test would pass.
    const cache = new SectionCache(30)
    const fetcher = controlledFetcher()
    let window = new Set<string>()
    const hydrator = new SectionHydrator({
      fetchSection: fetcher.fetchSection,
      cache,
      isInWindow: id => window.has(id),
    })

    const inFlight: Array<Promise<void>> = []
    for (let i = 0; i < 20; i++) {
      const id = `s${i}`
      window = new Set([id])
      inFlight.push(hydrator.fetch(id, hydrator.currentGeneration))
      // The window moves on: this is the scroll.
      hydrator.newGeneration()
    }
    // The user stops somewhere completely different.
    window = new Set(['s19'])
    ok(fetcher.outstanding === 20, `20 requests should be outstanding, got ${fetcher.outstanding}`)

    fetcher.resolveAllReversed()
    await Promise.all(inFlight)

    const stats = hydrator.snapshot()
    // Total discarded, not one counter. A fling's responses are discarded for two
    // different reasons and which one applies is not the point: what matters is that
    // nineteen of twenty were thrown away and one -- the section the user actually
    // stopped at -- was kept.
    //
    // The first version asserted `discardedStale >= 19` alone. That passed when a stale
    // generation discarded everything on its own, and it is the assertion that would have
    // hidden the mount-path bug: discarding on the generation alone is *also* what stopped
    // every section past the boot window from loading, because the window is reported
    // after the mounts that issued the fetches. Asserting the total cannot be satisfied by
    // one over-broad rule.
    const discarded = stats.discardedStale + stats.discardedEvicted
    ok(
      discarded >= 19,
      `at least 19 of 20 responses should have been discarded, got ${discarded} ` +
        `(${stats.discardedStale} stale, ${stats.discardedEvicted} out of window)`,
    )
    ok(
      stats.applied <= 1,
      `at most the section the user stopped at should have been applied, got ${stats.applied}`,
    )
    ok(cache.size <= 1, `the cache should hold at most that one section, got ${cache.size}`)
    ok(stats.pending === 0, 'every request should have been retired, not left hanging')
    return stats
  })

  await test('a response for a section that left the window is discarded', () => {
    // One request, one scroll, one late response. The minimal form of the same defect.
    const cache = new SectionCache(30)
    const fetcher = controlledFetcher()
    const window = new Set(['s0'])
    const hydrator = new SectionHydrator({
      fetchSection: fetcher.fetchSection,
      cache,
      isInWindow: id => window.has(id),
    })

    const inFlight = hydrator.fetch('s0', hydrator.currentGeneration)
    hydrator.newGeneration() // the scroll
    window.clear() // and past it
    fetcher.resolveNewest()

    return inFlight.then(() => {
      const stats = hydrator.snapshot()
      ok(stats.applied === 0, `a stale response was applied: ${JSON.stringify(stats)}`)
      ok(stats.discardedEvicted === 1 || stats.discardedStale === 1,
        `it should be counted as discarded one way or the other, got ${JSON.stringify(stats)}`)
      ok(!cache.includes('s0'), 'the cache should not hold a section that left the window')
      return stats
    })
  })

  await test('a newer request for the same section supersedes the older one', async () => {
    // A fast scroll that leaves and re-enters a section sends two requests for it. If the
    // older response lands last it overwrites the newer, and the user arrives at an older
    // version of the section than the one they scrolled back from — which is the only
    // way this bug shows up, since either order "works" on its own.
    const cache = new SectionCache(30)
    const fetcher = controlledFetcher()
    const window = new Set(['s0'])
    const applied: string[] = []
    const hydrator = new SectionHydrator({
      fetchSection: fetcher.fetchSection,
      cache,
      isInWindow: id => window.has(id),
      onContent: id => applied.push(id),
    })

    const first = hydrator.fetch('s0', hydrator.currentGeneration)
    hydrator.newGeneration()
    const second = hydrator.fetch('s0', hydrator.currentGeneration)
    ok(fetcher.requested.length === 2, `two requests expected, got ${fetcher.requested.length}`)

    // Answer the *newer* one first, then the older. The older is the one that must be
    // discarded.
    fetcher.resolveNewest()
    await second
    fetcher.resolveOldest()
    await first

    const stats = hydrator.snapshot()
    ok(stats.requested === 2, `expected two requests, got ${stats.requested}`)
    ok(stats.superseded === 1, `the second should have superseded the first, got ${stats.superseded}`)
    ok(stats.applied === 1, `only the newer response should have been applied, got ${stats.applied}`)
    ok(stats.discardedStale === 1, `the older response should be discarded, got ${stats.discardedStale}`)
    // And it was the *newer* one that landed: the second request was issued second, so it
    // carries the second label, and that is the text now in the cache.
    const resident = JSON.stringify(cache.get('s0')?.json ?? null)
    ok(
      resident.includes('section B'),
      `the newer response should have applied; the cache holds ${resident}`,
    )
    ok(applied.length === 1, `onContent should have fired once, got ${applied.length}`)
    return stats
  })

  await test('a section that stayed in the window is not discarded for the window moving', () => {
    // The regression test for the bug the in-engine run found, and the one that could only
    // be written afterwards.
    //
    // A mount attempt issues its fetch *during* reconciliation. The scroller reports the
    // new buffer window *after* reconciliation, so the generation is bumped between the two
    // — before the fetch can resolve. A guard that discarded on a stale generation
    // therefore discarded every section the user was looking at, and nothing past the boot
    // window ever loaded.
    //
    // Modelled exactly: request under generation N, then move the window, then resolve --
    // with the section still wanted throughout, which is the mount path.
    const cache = new SectionCache(30)
    const fetcher = controlledFetcher()
    const window = new Set(['s0'])
    const applied: string[] = []
    const hydrator = new SectionHydrator({
      fetchSection: fetcher.fetchSection,
      cache,
      isInWindow: id => window.has(id),
      onContent: id => applied.push(id),
    })

    const inFlight = hydrator.fetch('s0', hydrator.currentGeneration)
    // The scroll. The window moves; s0 is still in it.
    hydrator.newGeneration()
    fetcher.resolveNewest()
    return inFlight.then(() => {
      const stats = hydrator.snapshot()
      ok(stats.applied === 1, `a wanted section must still load; applied=${stats.applied}`)
      ok(
        applied.includes('s0'),
        'the content callback should have run: this is the path that mounts the editor',
      )
      ok(cache.includes('s0'), 'the content should be cached')
      return stats
    })
  })

  await test('the same response is discarded once the section leaves the window', () => {
    // The other half, so the first test cannot be satisfied by simply never discarding.
    // Identical sequence, and the only difference is whether the section is still wanted.
    const cache = new SectionCache(30)
    const fetcher = controlledFetcher()
    const window = new Set(['s0'])
    const hydrator = new SectionHydrator({
      fetchSection: fetcher.fetchSection,
      cache,
      isInWindow: id => window.has(id),
    })

    const inFlight = hydrator.fetch('s0', hydrator.currentGeneration)
    hydrator.newGeneration()
    window.clear() // and now it is gone
    fetcher.resolveNewest()
    return inFlight.then(() => {
      const stats = hydrator.snapshot()
      ok(stats.applied === 0, `a section that left the window must not load; applied=${stats.applied}`)
      ok(stats.discardedEvicted === 1, `expected one discard, got ${JSON.stringify(stats)}`)
      return stats
    })
  })

  await test('a cached section needs no round trip', () => {
    const cache = new SectionCache(30)
    const fetcher = controlledFetcher()
    const hydrator = new SectionHydrator({
      fetchSection: fetcher.fetchSection,
      cache,
      isInWindow: () => true,
    })
    cache.set('s0', { text: 'resident' })
    return hydrator.ensure('s0').then(hit => {
      ok(hit !== null, 'a resident section should be returned, not refetched')
      ok(fetcher.requested.length === 0, `nothing should have been requested, got ${fetcher.requested}`)
      return hydrator.snapshot()
    })
  })

  await test('a fetch that fails is reported, not swallowed', async () => {
    // A section that cannot be fetched cannot be edited, and silence would present that
    // as "still loading" forever. Reported whether or not the response was stale.
    const cache = new SectionCache(30)
    const failures: string[] = []
    const hydrator = new SectionHydrator({
      fetchSection: async id => {
        throw new Error(`no such section: ${id}`)
      },
      cache,
      isInWindow: () => true,
      onError: id => failures.push(id),
    })
    await hydrator.fetch('s0', hydrator.currentGeneration)
    ok(failures.length === 1, `the failure should have been reported, got ${failures.length}`)
    ok(failures[0] === 's0', `the failing section should be named, got ${failures[0]}`)
    ok(!cache.includes('s0'), 'a failed fetch must not leave an entry behind')
  })

  await test('a response with a non-binary payload is refused', async () => {
    // The generated type says `Uint8Array` and that is what the wire delivers. An array
    // means something re-encoded as JSON upstream, and applying it would decode nothing
    // while still inserting a cache entry — the worst combination: the section looks
    // resident and is not.
    const cache = new SectionCache(30)
    const hydrator = new SectionHydrator({
      fetchSection: async id => ({ id, content_zstd: [1, 2, 3] as unknown as Uint8Array }),
      cache,
      isInWindow: () => true,
    })
    await hydrator.fetch('s0', hydrator.currentGeneration)
    ok(!cache.includes('s0'), 'a refused payload must not be cached as if it decoded')
  })

  // -----------------------------------------------------------------------
  // The eviction flush

  await test('an eviction sends immediately and folds the log', async () => {
    // The no-data-loss path. After `destroy()` the only copy of these bytes is in SQLite,
    // so waiting for a throttle window is not "slightly late", it is data loss.
    const sent: Array<{ id: string; text: string; marks: number }> = []
    const flushes: Array<string | undefined> = []
    const committer = new EditCommitter({
      send: async (id, json, markCount): Promise<CommitResponse> => {
        sent.push({ id, text: (json as any).text, marks: markCount })
        return {
          section_id: id,
          wal_row_id: sent.length,
          word_count: 1,
          char_count: 1,
          block_count: 1,
          mark_count: markCount,
          pending: 1,
        }
      },
      // A 1000ms window, so if `flushNow` waited for it the test would take a second.
      intervalMs: 1000,
      flush: async documentId => {
        flushes.push(documentId)
        return 1
      },
    })

    committer.record('s0', () => ({ json: { text: 'the last thing typed' }, markCount: 3 }))
    ok(committer.isPending('s0'), 'the section should be pending')

    const flushed = await committer.flushNow('s0', 'doc-1')
    ok(flushed, 'flushNow should report that it sent something')
    ok(sent.length === 1, `one send expected, got ${sent.length}`)
    ok(sent[0]!.text === 'the last thing typed', `the final state should be sent, got ${sent[0]!.text}`)
    ok(sent[0]!.marks === 3, 'the mark count should travel with it')
    ok(flushes.length === 1, 'the log should be folded after an eviction, not merely appended to')
    ok(flushes[0] === 'doc-1', 'the fold should name the document')
    ok(!committer.isPending('s0'), 'the section should no longer be pending')
  })

  await test('a clean section costs no send and no fold', async () => {
    // A scroll through a document nobody edited passes hundreds of sections. Folding the
    // WAL for each of them would put a write on the hot path for nothing.
    let sends = 0
    let flushes = 0
    const committer = new EditCommitter({
      send: async (): Promise<CommitResponse> => {
        sends++
        return {
          section_id: 'x',
          wal_row_id: 0,
          word_count: 0,
          char_count: 0,
          block_count: 0,
          mark_count: 0,
          pending: 0,
        }
      },
      flush: async () => {
        flushes++
        return 0
      },
    })
    const flushed = await committer.flushNow('never-edited')
    ok(!flushed, 'a section with no recorded edit should report nothing to send')
    ok(sends === 0 && flushes === 0, `expected no work, got ${sends} sends and ${flushes} folds`)
  })

  await test('an eviction does not disturb a window for another section', async () => {
    // The evictee and the window are independent. If flushing one section drained the
    // queue, a user typing in section 5 and scrolling section 2 out would lose the typing.
    const sent: string[] = []
    const committer = new EditCommitter({
      send: async id => {
        sent.push(id)
        return {
          section_id: id,
          wal_row_id: 0,
          word_count: 0,
          char_count: 0,
          block_count: 0,
          mark_count: 0,
          pending: 0,
        }
      },
      intervalMs: 1000,
      flush: async () => 0,
    })
    committer.record('typing', () => ({ json: { text: 'a' }, markCount: 0 }))
    committer.record('evicted', () => ({ json: { text: 'b' }, markCount: 0 }))

    await committer.flushNow('evicted')
    // The fold is separate from the send and best-effort; a transport is supplied so the
    // default's lazy import is not the thing under test.
    ok(sent.join() === 'evicted', `only the evicted section should have been sent, got ${sent}`)
    ok(committer.isPending('typing'), 'the section being typed into must still be pending')
  })

  await test('continuous typing produces one commit per window', async () => {
    // Same throttle-not-debounce argument as the height sync: a debounce would never fire
    // while the user types, which is exactly when it matters.
    let sends = 0
    let clock = 0
    const committer = new EditCommitter({
      send: async (): Promise<CommitResponse> => {
        sends++
        return {
          section_id: 's0',
          wal_row_id: 0,
          word_count: 0,
          char_count: 0,
          block_count: 0,
          mark_count: 0,
          pending: 0,
        }
      },
      intervalMs: 1000,
      now: () => clock,
    })
    // 300 keystrokes over 300 seconds: one window per second, forever typing.
    for (let i = 0; i < 300; i++) {
      committer.record('s0', () => ({ json: { revision: i }, markCount: 0 }))
      clock += 1000
      await committer.flush()
    }
    ok(sends > 250, `one commit per window should mean ~300 sends, got ${sends}`)
    return { sends, keystrokes: 300 }
  })

  await test('a window sends the state at send time, not at window open', async () => {
    // The reason the payload is read through a closure. Capturing the JSON when the
    // window opened would persist a state the user has already typed past — which is
    // data loss with extra steps.
    const sent: unknown[] = []
    const committer = new EditCommitter({
      send: async (_id, json) => {
        sent.push(json)
        return {
          section_id: 's0',
          wal_row_id: 0,
          word_count: 0,
          char_count: 0,
          block_count: 0,
          mark_count: 0,
          pending: 0,
        }
      },
      intervalMs: 1000,
    })
    let revision = 0
    committer.record('s0', () => ({ json: { revision: ++revision }, markCount: 0 }))
    revision = 100
    await committer.flush()
    ok(
      JSON.stringify(sent[0]) === JSON.stringify({ revision: 101 }),
      `the send should carry the state at send time, got ${JSON.stringify(sent[0])}`,
    )
  })

  await test('a failed window keeps its sections for the next one', async () => {
    // The log is the only copy. Dropping a batch because the transport was briefly
    // unavailable would lose every edit in it.
    let attempts = 0
    const committer = new EditCommitter({
      send: async id => {
        attempts++
        if (attempts === 1) throw new Error('bridge unavailable')
        return {
          section_id: id,
          wal_row_id: 0,
          word_count: 0,
          char_count: 0,
          block_count: 0,
          mark_count: 0,
          pending: 0,
        }
      },
      intervalMs: 1000,
    })
    committer.record('s0', () => ({ json: { text: 'important' }, markCount: 0 }))
    await committer.flush()
    ok(committer.pending() === 1, `the section should be back in the queue, pending=${committer.pending()}`)
    ok(committer.snapshot().failed === 1, 'the failure should be counted')

    await committer.flush()
    ok(committer.pending() === 0, 'the retry should clear the queue')
    ok(attempts === 2, `expected a retry, got ${attempts} attempts`)
  })

  console.log('='.repeat(72))
  console.log(`${passed} passed, ${failed} failed`)
  if (failed) {
    console.log(`failing: ${failures.join(', ')}`)
    process.exit(1)
  }
}

await main()
