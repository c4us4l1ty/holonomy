/**
 * The asset contract: addresses, not payloads.
 *
 * # What is being held here
 *
 * Section JSON never carries image bytes. An image node's `src` is `holo-asset://<sha256>`,
 * the bytes are a row in SQLite's `assets` table, and the renderer fetches them through the
 * protocol handler. `crates/holonomy-shell/tests/geometry-bridge.rs` covers the store and
 * the URL grammar from the Rust side; this covers the frontend's half of the same grammar,
 * because it is written separately and a URL that only one side accepts is a document that
 * renders on the machine that wrote it and not anywhere else.
 *
 * The second thing held here is the *absence* — no `data:` URL anywhere in a document — and
 * that one is worth a test of its own because its failure is silent: the document renders,
 * the image renders, and the only symptom is a section blob a third larger than it should be
 * with a WAL full of copies.
 *
 * Run: node --experimental-strip-types test/assets.ts
 */

import {
  ASSET_SCHEME,
  assertNoInlineImages,
  assetHashFromUrl,
  assetHashesIn,
  AssetResolver,
  assetUrl,
  isAssetHash,
  isInlineDataUrl,
} from '../src/core/assets.ts'
import type { SectionRecord } from '../src/core/registry.ts'

let passed = 0
let failed = 0
const failures: string[] = []

/**
 * Tests may be sync or async, and the summary waits for them.
 *
 * # Why the harness changed
 *
 * It used to call `fn()` and report on the return value, which counts a test that returns a
 * promise as *passed* the moment the promise is created. Every assertion inside it then ran
 * unobserved: a failing assertion surfaced as an unhandled rejection long after the summary
 * had printed "16 passed, 0 failed", and one of them did. So the summary claimed a pass while
 * the run was in the middle of a failure — which is the specific outcome this project's
 * doctrine exists to prevent.
 *
 * The pending list is awaited before the summary, so a rejected test is counted as failed
 * and names itself.
 */
const pending: Array<Promise<unknown>> = []

function test(name: string, fn: () => unknown | Promise<unknown>): void {
  pending.push(
    Promise.resolve()
      .then(fn)
      .then(detail => {
        passed++
        console.log(`PASS  ${name}${detail !== undefined ? `  ${JSON.stringify(detail)}` : ''}`)
      })
      .catch((e: any) => {
        failed++
        failures.push(name)
        console.log(`FAIL  ${name}\n        ${e.message}`)
      }),
  )
}

/** Wait for every registered test and report. */
async function report(): Promise<never | void> {
  await Promise.all(pending)
  console.log('='.repeat(72))
  console.log(`${passed} passed, ${failed} failed`)
  if (failed) {
    console.log(`failing: ${failures.join(', ')}`)
    process.exit(1)
  }
}

function ok(cond: unknown, msg: string): asserts cond {
  if (!cond) throw new Error(msg)
}

/** The published SHA-256 vectors, so a change of hash function cannot pass by being self-consistent. */
const EMPTY_SHA = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'
const ABC_SHA = 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad'

function record(id: string, json: unknown): SectionRecord {
  return { id, json, metrics: { words: 0, marks: 0, chars: 0, blocks: 1 }, loaded: true, dirty: false }
}

function imageDoc(src: string): unknown {
  return {
    type: 'doc',
    content: [
      { type: 'paragraph', content: [{ type: 'text', text: 'before' }] },
      { type: 'image', attrs: { src, alt: 'a figure' } },
      { type: 'paragraph', content: [{ type: 'text', text: 'after' }] },
    ],
  }
}

console.log('assets: content-addressed URLs, never inline payloads')
console.log('='.repeat(72))

test('an asset URL is the scheme and a 64-character hex digest', () => {
  ok(assetUrl(ABC_SHA) === `${ASSET_SCHEME}://${ABC_SHA}`, `got ${assetUrl(ABC_SHA)}`)
  ok(ABC_SHA.length === 64, `the ABC vector should be 64 characters, got ${ABC_SHA.length}`)
  ok(EMPTY_SHA.length === 64, 'the empty-string vector should also be 64')
})

test('a malformed hash is refused rather than turned into a broken URL', () => {
  // An empty `src` reloads the page, which is worse than a broken image: the user's
  // unsaved typing would go with it.
  for (const bad of ['', 'short', 'z'.repeat(64), ABC_SHA.toUpperCase(), '0'.repeat(63), '0'.repeat(65)]) {
    let threw = false
    try {
      assetUrl(bad)
    } catch {
      threw = true
    }
    ok(threw, `should have refused ${JSON.stringify(bad)}`)
  }
})

test('a URL is read back to the hash that addresses it', () => {
  ok(assetHashFromUrl(assetUrl(ABC_SHA)) === ABC_SHA, 'round trip failed')
  ok(isAssetHash(ABC_SHA), 'the vector should be a valid hash')
  ok(!isAssetHash(ABC_SHA.toUpperCase()), 'uppercase is a typo, not a spelling')
})

test('anything that is not a bare asset URL is not an asset URL', () => {
  // Mirrors `core::resolve_asset_uri`'s refusals exactly. The two lists have to agree or a
  // document opens on one platform and renders broken on another.
  for (const bad of [
    'https://example.com/x.png',
    'file:///etc/passwd',
    `${ASSET_SCHEME}://${ABC_SHA}/../..`,
    `${ASSET_SCHEME}://${ABC_SHA}/variant`,
    `${ASSET_SCHEME}://${ABC_SHA.toUpperCase()}`,
    `${ASSET_SCHEME}://`,
    '',
    null,
    42,
  ]) {
    ok(assetHashFromUrl(bad) === null, `should have refused ${JSON.stringify(bad)}`)
  }
})

test('a document with asset URLs passes the no-inline-images check', () => {
  assertNoInlineImages([record('s0', imageDoc(assetUrl(ABC_SHA)))])
})

test('an inline image anywhere in a document is caught, and named', () => {
  const inline = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=='
  // Deeply nested, because the place this actually happens is an extension inserting a
  // caption or a figure wrapper, not a top-level image.
  const nested = {
    type: 'doc',
    content: [
      { type: 'paragraph', content: [{ type: 'text', text: 'intro' }] },
      {
        type: 'blockquote',
        content: [{ type: 'figure', content: [{ type: 'image', attrs: { src: inline } }] }],
      },
    ],
  }
  let message = ''
  try {
    assertNoInlineImages([record('s7', nested)])
  } catch (e: any) {
    message = e.message
  }
  ok(message !== '', 'an inline image should have been rejected')
  ok(message.includes('s7'), `the message should name the section, got: ${message}`)
  ok(message.includes('$.content[1]'), `the message should say where, got: ${message}`)
})

test('uppercase DATA: is caught too', () => {
  // Schemes are case-insensitive per RFC 3986, so a paste that produced `DATA:` would slip
  // past a lowercase-only check and still be a 1.33x inline payload.
  ok(isInlineDataUrl('DATA:image/png;base64,AAAA'), 'the scheme comparison must be case-insensitive')
  let threw = false
  try {
    assertNoInlineImages([record('s0', imageDoc('DATA:image/png;base64,AAAA'))])
  } catch {
    threw = true
  }
  ok(threw, 'an uppercase DATA: image should still be rejected')
})

test('a document whose only large field is not an image is fine', () => {
  // The check must not be a size heuristic. A 200KB paragraph is legitimate prose.
  assertNoInlineImages([
    record('s0', {
      type: 'doc',
      content: [{ type: 'paragraph', content: [{ type: 'text', text: 'x'.repeat(200_000) }] }],
    }),
  ])
})

test('an asset URL is not mistaken for an inline one', () => {
  ok(!isInlineDataUrl(assetUrl(ABC_SHA)), 'an asset URL is exactly what should be used')
  ok(!isInlineDataUrl('https://example.com/a.png'), 'a remote URL is not inline, but is also not ours')
})

// -- reference counting and revocation ---------------------------------------
//
// The leak these hold: a `Blob` holds the whole decoded image in the renderer's heap. A
// document with two hundred figures, scrolled through, would hold two hundred of them for
// the life of the session in a project whose entire argument is that memory does not track
// document length. So revocation has to be *proved*, not asserted as an intention.

/**
 * A resolver whose URL creation and revocation are recorded.
 *
 * `URL.createObjectURL` and `revokeObjectURL` are not in Node, and stubbing the globals
 * would mean the test was asserting against its own stub. Injected instead: the resolver
 * takes its bytes fetcher, and the test owns the `Blob`-URL bookkeeping.
 *
 * `schemeWorks: false` skips the `Image` probe, which needs a DOM — and is the *real* answer
 * on webkit2gtk 2.60 anyway, so this is the path shipping code takes there.
 */
function countingResolver(probeResult = false) {
  const created: string[] = []
  const revoked: string[] = []
  // Read through a function rather than `revoked.length` directly. The type checker narrows
  // the length from an earlier `ok(revoked.length === 0, ...)`, so the *next* comparison is
  // reported as comparing two literals with no overlap -- which reads as a type error rather
  // than as the fact that the array really did grow. A function defeats the narrowing and
  // keeps the assertion honest.
  const revocations = () => revoked.length
  let next = 0
  const resolver = new AssetResolver({
    fetchBytes: async hash => ({
      mime: 'image/png',
      bytes: Uint8Array.from([hash.length, ...hash.slice(0, 4).split('').map(c => c.charCodeAt(0))]),
    }),
    // Injected rather than faked through the `Image` global. See `AssetResolverOptions`.
    probeScheme: async () => probeResult,
    objectUrls: {
      create: (_bytes, _mime) => {
        const url = `blob:test/${next++}`
        created.push(url)
        return url
      },
      revoke: url => {
        revoked.push(url)
      },
    },
  })
  return { resolver, created, revoked, revocations }
}

const ABC = ABC_SHA

test('a second reference to one asset does not fetch it again', async () => {
  const env = countingResolver()
  try {
    const [a, b] = await Promise.all([env.resolver.acquire(ABC), env.resolver.acquire(ABC)])
    ok(a === b, `the same asset should resolve to the same URL: ${a} vs ${b}`)
    ok(env.created.length === 1, `one asset should be fetched once, got ${env.created.length}`)
    ok(env.resolver.refState(ABC)?.refs === 2, `expected 2 refs, got ${env.resolver.refState(ABC)?.refs}`)
    ok(env.resolver.size === 1, 'one asset should be held')
    return { refs: env.resolver.totalRefs }
  } finally {
    // In the `finally` of an *async* function, so the stubs are still installed while the
    // awaits run. The first version returned a promise from a synchronous function, so `finally`
    // fired on the way out and restored the real `URL.revokeObjectURL` — which is a silent
    // no-op on an unknown blob id. Every revocation assertion then read "0 revoked" and the
    // test failed for a reason that had nothing to do with the refcount.
  }
})

test('the object URL is revoked when the last reference goes', async () => {
  const env = countingResolver()
  try {
    await Promise.all([env.resolver.acquire(ABC), env.resolver.acquire(ABC)])

    // Two sections hold one figure. The first eviction must not free it.
    ok(env.resolver.release(ABC) === false, 'the first release should not revoke')
    ok(env.revocations() === 0, `nothing should be revoked yet, got ${env.revocations()}`)
    ok(env.resolver.refState(ABC)?.refs === 1, `expected 1 ref, got ${env.resolver.refState(ABC)?.refs}`)
    ok(env.resolver.size === 1, 'the asset should still be held')

    // The second is the last, and the bytes go.
    ok(env.resolver.release(ABC) === true, 'the last release should revoke')
    ok(env.revocations() === 1, `expected one revocation, got ${env.revocations()}`)
    // Read through `refState` rather than `size`, which the checker has just narrowed to a
    // literal by the assertion above.
    ok(env.resolver.refState(ABC) === null, `the entry should be gone, got ${JSON.stringify(env.resolver.refState(ABC))}`)
    return { revoked: env.revocations() }
  } finally {
  }
})

test('an over-release is a no-op rather than a corrupt count', async () => {
  // A section can be evicted twice: the scroller unmounts it, and a retry unmounts it again if
  // the window moved while the flush was in flight. A decrement below zero would revoke an
  // asset another section was still displaying.
  const env = countingResolver()
  try {
    await env.resolver.acquire(ABC)
    env.resolver.release(ABC)
    env.resolver.release(ABC)
    env.resolver.release(ABC)
    ok(env.revocations() === 1, `three releases should revoke once, got ${env.revocations()}`)
    ok(env.resolver.size === 0, 'still nothing held')
  } finally {
  }
})

test('releasing by content frees only what that section held', async () => {
  // The resolver is document-wide. A naive release-all would drop assets belonging to
  // sections that are still mounted, which is a blank image in a section nobody touched.
  const env = countingResolver()
  const other = 'c'.repeat(64)
  try {
    const [, otherUrl] = await Promise.all([
      env.resolver.acquire(ABC),
      env.resolver.acquire(other),
    ])
    const dropped = env.resolver.releaseAll([ABC])
    ok(dropped.length === 1, `one asset should have been dropped, got ${dropped.length}`)
    ok(env.revocations() === 1, `one revocation expected, got ${env.revocations()}`)
    ok(env.resolver.refState(other)?.refs === 1, "the other section's asset must still be held")
    ok(env.resolver.refState(other)?.url === otherUrl, 'and it must still resolve')
    return { dropped, stillHeld: env.resolver.size }
  } finally {
  }
})

test('an asset held by four sections is only revoked by the fourth eviction', async () => {
  // The realistic figure: one logo in a header and in three figures. Three `releaseAll` calls
  // for the same hash must leave the count right, which `releaseAll` gets from de-duplicating
  // its own input and from `release` refusing to go below zero.
  //
  // The first version acquired three times and expected no revocation, which is
  // arithmetically wrong: three refs and three releases *should* revoke, because the third
  // release is the last reference.
  const env = countingResolver()
  try {
    await Promise.all([
      env.resolver.acquire(ABC),
      env.resolver.acquire(ABC),
      env.resolver.acquire(ABC),
      env.resolver.acquire(ABC),
    ])
    ok(env.resolver.refState(ABC)?.refs === 4, `expected 4 refs, got ${env.resolver.refState(ABC)?.refs}`)
    // One `releaseAll` per *section*, not per reference. A section whose content mentions the
    // same figure twice holds one reference — one editor, one NodeView — and `releaseAll`
    // de-duplicates its input to match. The first version passed `[ABC, ABC, ABC]` and expected
    // the count to fall by three, which contradicts the very de-duplication it relied on.
    env.resolver.releaseAll([ABC])
    env.resolver.releaseAll([ABC])
    env.resolver.releaseAll([ABC])
    ok(env.resolver.refState(ABC)?.refs === 1, 'three evictions leave one section still holding it')
    ok(env.revocations() === 0, 'so nothing may be revoked yet')
    ok(env.resolver.release(ABC), 'the fourth release is the last reference')
    ok(env.revocations() === 1, 'and it revokes exactly once')
    return { totalRefs: env.resolver.totalRefs }
  } finally {
  }
})

test('a scheme-backed asset is not revoked, because it has no lifetime to end', async () => {
  // `URL.revokeObjectURL` on a `holo-asset://` URL is a no-op, and calling it anyway would look
  // like a bug to the next reader. Asserted by refcount: the count still falls, and the
  // revocation list stays empty.
  try {
    const resolver = new AssetResolver({
      fetchBytes: async () => ({ mime: 'image/png', bytes: Uint8Array.from([1]) }),
      // The engine *does* serve the scheme here, which is the other half of the branch.
      probeScheme: async () => true,
    })
    const url = await resolver.acquire(ABC)
    ok(url!.startsWith(ASSET_SCHEME), `expected the scheme URL, got ${url}`)
    ok(resolver.refState(ABC)?.revocable === false, 'a scheme URL must not be marked revocable')
    resolver.release(ABC)
    ok(resolver.size === 0, 'the entry should still be dropped')
    ok(resolver.revokedUrls().length === 0, 'and nothing revoked, because there is nothing to revoke')
  } finally {
  }
})

test("a section's content names the assets it holds", () => {
  // The eviction path releases by content. If this returned nothing, every asset would leak;
  // if it returned something wrong, a live section's figure would be revoked.
  const a = 'a'.repeat(64)
  const b = 'b'.repeat(64)
  const json = {
    type: 'doc',
    content: [
      { type: 'paragraph', content: [{ type: 'text', text: 'intro' }] },
      { type: 'image', attrs: { src: assetUrl(a), alt: 'one' } },
      { type: 'paragraph', content: [{ type: 'text', text: 'middle' }] },
      { type: 'blockquote', content: [{ type: 'image', attrs: { src: assetUrl(b) } }] },
      // A remote image: not ours, so not counted, and not something to revoke.
      { type: 'image', attrs: { src: 'https://example.com/x.png' } },
      // The same figure twice in one section: one reference, because one section holds one.
      { type: 'image', attrs: { src: assetUrl(a) } },
    ],
  }
  const hashes = assetHashesIn(json)
  ok(hashes.length === 2, `expected two assets, got ${hashes.length}: ${hashes.join(',')}`)
  ok(hashes.includes(a) && hashes.includes(b), `wrong hashes: ${hashes.join(',')}`)
  ok(assetHashesIn({ type: 'doc', content: [] }).length === 0, 'prose should name no assets')
})

await report()