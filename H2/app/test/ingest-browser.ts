/**
 * Image ingestion, in a real browser.
 *
 * # Why these run here rather than under Node
 *
 * Because `core/ingest.ts` imports `./assets.js`, and `node --experimental-strip-types` erases
 * types without transforming module resolution — it looks for `assets.js` on disk and there is
 * only `assets.ts`. That is a known constraint of this repo's Node harness, and the workaround of
 * duplicating `assetUrl` into `ingest.ts` so the import could be dropped is exactly the
 * duplication `DOCTRINE.md` §8 exists to prevent.
 *
 * So the tests run where the module actually loads. That is not a workaround so much as the
 * correct place: paste and drop are browser events, `crypto.subtle` is a browser API, and the
 * module is browser-facing. Running it here tests the real thing.
 *
 * # What each test is for
 *
 * The three steps of ingestion each fail in a way that is invisible downstream:
 *
 * - a figure that went in with no dimensions has no layout box, so its section's height changes
 *   when the image loads — the failure the whole geometry subsystem exists to prevent;
 * - a figure stored under an address that does not resolve is a broken-image glyph forever; and
 * - a decompression bomb measured *after* decoding has already allocated its gigabyte.
 *
 * Run: node --experimental-strip-types test/ingest-browser.ts
 */

import { chromium, type Page } from 'playwright'

const URL = process.env.HOLO_APP_URL ?? 'http://localhost:5184/'

/** Imported into the page as a runtime value; see `BRIDGE_MODULE` in `test/scroll.ts`. */
const INGEST_MODULE = '/src/core/ingest.ts'
const ASSET_MODULE = '/src/core/assets.ts'

let passed = 0
let failed = 0
const failures: string[] = []

async function test(name: string, _page: Page, fn: () => Promise<unknown>): Promise<void> {
  try {
    const detail = await fn()
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

async function main(): Promise<void> {
  const browser = await chromium.launch({ args: ['--no-sandbox'] })
  const page = await browser.newPage()
  page.on('pageerror', e => console.log(`  [page error] ${e.message}`))
  await page.goto(URL)
  await page.waitForSelector('#canvas')

  // The fixtures are built in the page rather than passed in, because a `Uint8Array` cannot
  // cross `page.evaluate` as bytes and re-encoding it here would be a second implementation of
  // the thing under test.
  // One argument, not two: `page.evaluate`'s signature takes a single value, and an object is
  // also what a second module would need if a third were added.
  await page.evaluate(async (modules: { ingest: string; assets: string }) => {
    const ingest = await import(modules.ingest)
    const assets = await import(modules.assets)

    /** A valid PNG header carrying `width` and `height`, and nothing else. */
    const png = (width: number, height: number, padTo = 0): Uint8Array => {
      const signature = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]
      const ihdr = new Uint8Array(25)
      const view = new DataView(ihdr.buffer)
      view.setUint32(0, 13)
      ihdr.set([0x49, 0x48, 0x44, 0x52], 4)
      view.setUint32(8, width)
      view.setUint32(12, height)
      ihdr[16] = 8
      ihdr[17] = 2
      const body = new Uint8Array([...signature, ...ihdr])
      if (padTo <= body.length) return body
      const padded = new Uint8Array(padTo)
      padded.set(body)
      return padded
    }

    /** A JPEG: SOI, an APP0 segment, then SOF0 carrying the dimensions. */
    const jpeg = (width: number, height: number): Uint8Array => {
      const out = [0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10, ...new Array(14).fill(0)]
      out.push(0xff, 0xc0, 0x00, 0x11, 0x08)
      out.push((height >> 8) & 0xff, height & 0xff)
      out.push((width >> 8) & 0xff, width & 0xff)
      out.push(0x03, ...new Array(9).fill(0))
      return new Uint8Array(out)
    }

    /** A writer that records what it was asked to store and hands back a digest. */
    const writer = (recompute = true) => {
      const stored: Array<{ hash: string; bytes: number; mime: string }> = []
      return {
        stored,
        put: async (bytes: Uint8Array, mime: string) => {
          const digest = await crypto.subtle.digest('SHA-256', bytes as unknown as BufferSource)
          const hash = recompute
            ? Array.from(new Uint8Array(digest))
                .map(b => b.toString(16).padStart(2, '0'))
                .join('')
            : 'wrong'
          stored.push({ hash, bytes: bytes.length, mime })
          return hash
        },
      }
    }

    const blobOf = (bytes: Uint8Array, type: string): Blob => new Blob([bytes as unknown as BufferSource], { type })

    const w = window as any
    w.HOLO_INGEST = { ingest, assets, png, jpeg, writer, blobOf }
  }, { ingest: INGEST_MODULE, assets: ASSET_MODULE })

  console.log('ingest: bytes in, a content-addressed node out')
  console.log('='.repeat(72))

  await test('only formats a webview can decode are accepted', page, async () => {
    const { accepted, refused } = await page.evaluate(() => {
      const { ACCEPTED_MIME, isAcceptedImage } = (window as any).HOLO_INGEST.ingest
      return {
        accepted: ACCEPTED_MIME.filter((m: string) => isAcceptedImage(m)),
        // Both satisfy a `startsWith('image/')` test and neither renders, so a prefix check
        // would admit a figure the editor can only draw as a broken-image glyph.
        refused: ['image/tiff', 'image/x-icon', 'text/html'].filter((m: string) => isAcceptedImage(m)),
      }
    })
    ok(accepted.length >= 7, `expected a real allowlist, got ${accepted.length}`)
    ok(refused.length === 0, `a prefix test would admit these: ${refused.join(', ')}`)
    return { accepted: accepted.length }
  })

  await test('a PNG header yields its dimensions without decoding', page, async () => {
    const results = await page.evaluate(() => {
      const { measureFromHeader } = (window as any).HOLO_INGEST.ingest
      const { png } = (window as any).HOLO_INGEST
      return [
        [4, 3],
        [1, 1],
        [1920, 1080],
        [8000, 8000],
      ].map(([w, h]) => measureFromHeader(png(w, h), 'image/png'))
    })
    const expected = [
      [4, 3],
      [1, 1],
      [1920, 1080],
      [8000, 8000],
    ]
    for (let i = 0; i < expected.length; i++) {
      const [w, h] = expected[i]!
      ok(results[i] !== null, `a ${w}x${h} PNG should be measurable`)
      ok(results[i].width === w && results[i].height === h, `expected ${w}x${h}, got ${JSON.stringify(results[i])}`)
    }
    return { measured: results.length }
  })

  await test('a JPEG header yields its dimensions, walking past the APP0 segment', page, async () => {
    // The first segment in a real JPEG is APP0, not the frame header. A reader that does not walk
    // segments reads the wrong bytes and gets a plausible wrong answer, which is the failure this
    // input exists for.
    const measured = await page.evaluate(() => {
      const { measureFromHeader } = (window as any).HOLO_INGEST.ingest
      const { jpeg } = (window as any).HOLO_INGEST
      return measureFromHeader(jpeg(1024, 768), 'image/jpeg')
    })
    ok(measured !== null, 'a JPEG should be measurable')
    ok(measured.width === 1024 && measured.height === 768, `expected 1024x768, got ${JSON.stringify(measured)}`)
    return measured
  })

  await test('a truncated or non-image file measures as null rather than throwing', page, async () => {
    // Each of these arrives from a real failure: a half-written upload, a paste of something that
    // is not an image, a read that raced a delete. A throw here loses the paste.
    const result = await page.evaluate(() => {
      const { measureFromHeader } = (window as any).HOLO_INGEST.ingest
      const { png } = (window as any).HOLO_INGEST
      const cases: Array<[string, Uint8Array]> = [
        ['empty', new Uint8Array(0)],
        ['one byte', new Uint8Array([0x89])],
        ['PNG signature only', new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])],
        ['PNG header cut short', png(4, 3).slice(0, 20)],
        ['JPEG SOI only', new Uint8Array([0xff, 0xd8])],
        ['JPEG scan before any frame', new Uint8Array([0xff, 0xd8, 0xff, 0xda, 0, 2, 1, 2])],
        ['random bytes', new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12])],
        ['a JPEG segment claiming a huge length', new Uint8Array([0xff, 0xd8, 0xff, 0xe1, 0xff, 0xff, 0, 0])],
      ]
      const out: Array<{ what: string; threw: string | null; value: unknown }> = []
      for (const [what, bytes] of cases) {
        let threw: string | null = null
        let value: unknown = null
        try {
          value = measureFromHeader(bytes, 'image/png') ?? measureFromHeader(bytes, 'image/jpeg')
        } catch (e: any) {
          threw = e.message
        }
        out.push({ what, threw, value })
      }
      return out
    })
    for (const { what, threw, value } of result) {
      ok(threw === null, `measuring ${what} threw: ${threw}`)
      ok(value === null || typeof value === 'object', `measuring ${what} produced ${JSON.stringify(value)}`)
    }
    return { cases: result.length }
  })

  await test('an awkward header measures as null rather than being guessed', page, async () => {
    // WebP, AVIF, GIF and BMP have chunked headers needing real parsers. `null` is the honest
    // answer; inventing a number would put a wrong size in the document's geometry.
    const nulls = await page.evaluate(() => {
      const { measureFromHeader } = (window as any).HOLO_INGEST.ingest
      return ['image/webp', 'image/avif', 'image/gif', 'image/bmp'].filter(
        (m: string) => measureFromHeader(new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8]), m) === null,
      )
    })
    ok(nulls.length === 4, `all four should defer, got ${nulls.join(', ')}`)
    return { deferred: nulls.length }
  })

  await test('an SVG measures from width/height or a viewBox, with units stripped', page, async () => {
    const results = await page.evaluate(() => {
      const { measureSvg } = (window as any).HOLO_INGEST.ingest
      return {
        explicit: measureSvg('<svg width="100" height="50"></svg>'),
        withUnits: measureSvg('<svg width="100px" height="50px"></svg>'),
        viewBox: measureSvg('<svg viewBox="0 0 200 100"></svg>'),
        notSvg: measureSvg('<div></div>'),
        zero: measureSvg('<svg width="0" height="0"></svg>'),
        empty: measureSvg(''),
      }
    })
    ok(results.explicit?.width === 100 && results.explicit?.height === 50, `explicit: ${JSON.stringify(results.explicit)}`)
    // `50px` must not become the number 50 followed by a stray "px" in the geometry.
    ok(results.withUnits?.width === 100 && results.withUnits?.height === 50, `units: ${JSON.stringify(results.withUnits)}`)
    ok(results.viewBox !== null, 'a viewBox-only SVG should still measure')
    ok(
      Math.abs(results.viewBox.width / results.viewBox.height - 2) < 1e-9,
      `the aspect ratio should be exact; got ${JSON.stringify(results.viewBox)}`,
    )
    for (const bad of ['notSvg', 'zero', 'empty'] as const) {
      ok(results[bad] === null, `${bad} should not measure, got ${JSON.stringify(results[bad])}`)
    }
    return results.viewBox
  })

  await test('a pasted image becomes a node addressed by the digest of its bytes', page, async () => {
    const result = await page.evaluate(async () => {
      const { ingestImage, sha256 } = (window as any).HOLO_INGEST.ingest
      const { isInlineDataUrl, assetHashFromUrl } = (window as any).HOLO_INGEST.assets
      const { png, writer, blobOf } = (window as any).HOLO_INGEST
      const bytes = png(64, 48)
      const w = writer()
      const out = await ingestImage(blobOf(bytes, 'image/png'), w, { alt: 'a screenshot' })
      return {
        hash: out.hash,
        expected: await sha256(bytes),
        src: out.node.attrs.src,
        alt: out.node.attrs.alt,
        width: out.node.attrs.width,
        height: out.node.attrs.height,
        readBack: assetHashFromUrl(out.node.attrs.src),
        inline: isInlineDataUrl(out.node.attrs.src),
        stored: w.stored,
      }
    })
    ok(result.hash === result.expected, `the address is the digest of the bytes: ${result.hash} != ${result.expected}`)
    ok(!result.inline, `a figure must never be inline; got ${String(result.src).slice(0, 32)}`)
    ok(result.src === `holo-asset://${result.expected}`, `the node should name the digest: ${result.src}`)
    ok(result.readBack === result.expected, 'and the address reads back to the same digest')
    // Dimensions are what give the figure its layout box before the bytes arrive.
    ok(result.width === 64 && result.height === 48, `expected 64x48, got ${result.width}x${result.height}`)
    ok(result.alt === 'a screenshot', 'the alt text is kept')
    ok(result.stored.length === 1, `expected one write, got ${result.stored.length}`)
    return { hash: result.hash.slice(0, 12), dimensions: `${result.width}x${result.height}` }
  })

  await test('identical bytes produce one address, so a repeated figure is stored once', page, async () => {
    // Content addressing is what makes a logo in a header and in forty figures cheap. A UUID
    // would store forty copies, and the refcount in `AssetResolver` would still be right — it
    // would just be forty times the memory.
    const result = await page.evaluate(async () => {
      const { ingestImage } = (window as any).HOLO_INGEST.ingest
      const { png, writer, blobOf } = (window as any).HOLO_INGEST
      const bytes = png(8, 8)
      const a = await ingestImage(blobOf(bytes, 'image/png'), writer())
      const b = await ingestImage(blobOf(bytes, 'image/png'), writer())
      return { a: a.hash, b: b.hash, aSrc: a.node.attrs.src, bSrc: b.node.attrs.src }
    })
    ok(result.a === result.b, 'identical bytes should address identically')
    ok(result.aSrc === result.bSrc, 'so the nodes should carry the same address')
    return { shared: result.a.slice(0, 12) }
  })

  await test('an oversized image is refused before anything is written', page, async () => {
    // Before the write, not after. A 40MB image that gets hashed and sent across the bridge and
    // *then* refused has already cost the copy.
    const result = await page.evaluate(async () => {
      const { ingestImage, MAX_IMAGE_BYTES } = (window as any).HOLO_INGEST.ingest
      const { png, writer, blobOf } = (window as any).HOLO_INGEST
      const w = writer()
      let error: any = null
      try {
        await ingestImage(blobOf(png(4, 4, MAX_IMAGE_BYTES + 1), 'image/png'), w)
      } catch (e: any) {
        error = { reason: e.reason, message: e.message, name: e.name }
      }
      return { error, writes: w.stored.length }
    })
    ok(result.error !== null, 'an oversized image should be refused')
    ok(result.error.reason === 'too-large', `expected too-large, got ${result.error.reason}`)
    ok(result.error.message.includes('limit'), `the message should name the limit: ${result.error.message}`)
    ok(result.writes === 0, 'and nothing should have been written')
    return { refused: result.error.reason }
  })

  await test('a decompression bomb is refused from its header, without decoding', page, async () => {
    // 20000x20000 is 400 megapixels: 1.6GB decoded, from a few hundred bytes. The size check
    // passes, so the *header* is the only thing that can catch it. Measuring by decoding would
    // make this test measure an allocation rather than a rejection.
    const result = await page.evaluate(async () => {
      const { ingestImage, MAX_IMAGE_PIXELS } = (window as any).HOLO_INGEST.ingest
      const { png, writer, blobOf } = (window as any).HOLO_INGEST
      const w = writer()
      let error: any = null
      try {
        await ingestImage(blobOf(png(20000, 20000), 'image/png'), w)
      } catch (e: any) {
        error = { reason: e.reason, message: e.message }
      }
      return { error, writes: w.stored.length, overBudget: 20000 * 20000 > MAX_IMAGE_PIXELS }
    })
    ok(result.overBudget, 'precondition: this is over the pixel budget')
    ok(result.error !== null, 'a 20000x20000 image should be refused')
    ok(result.error.message.includes('20000'), `the message should name the dimensions: ${result.error.message}`)
    ok(result.writes === 0, 'and nothing should have been written')
    return { refused: result.error.message.slice(0, 44) }
  })

  await test('a non-image, a typeless file and an empty file are each refused with a reason', page, async () => {
    // Three different reasons, because a caller responds to them differently: a PDF is a UI
    // filter that should not have been reached, an empty file is a failed read, and a PDF named
    // by a user is worth a sentence while an empty one is worth a log.
    const results = await page.evaluate(async () => {
      const { ingestImage } = (window as any).HOLO_INGEST.ingest
      const { png, writer, blobOf } = (window as any).HOLO_INGEST
      const attempt = async (bytes: Uint8Array, type: string) => {
        try {
          await ingestImage(blobOf(bytes, type), writer())
          return { reason: null as string | null, message: null as string | null }
        } catch (e: any) {
          return { reason: e.reason as string, message: e.message as string }
        }
      }
      return {
        pdf: await attempt(new Uint8Array([1, 2, 3]), 'application/pdf'),
        typeless: await attempt(png(4, 4), ''),
        empty: await attempt(new Uint8Array(0), 'image/png'),
      }
    })
    ok(results.pdf.reason === 'not-an-image', `a PDF should be not-an-image, got ${results.pdf.reason}`)
    ok(results.pdf.message!.includes('application/pdf'), 'and the message should name what arrived')
    // `application/octet-stream` is what a drag from an unknown source produces, and guessing
    // "probably a PNG" would put an undecodable blob in the document.
    ok(results.typeless.reason === 'not-an-image', `a typeless file should be refused, got ${results.typeless.reason}`)
    // A zero-byte asset is a row that resolves to nothing, and a node pointing at it is a broken
    // image forever. Refuse at the door.
    ok(results.empty.reason === 'unreadable', `an empty file should be unreadable, got ${results.empty.reason}`)
    return { refused: [results.pdf.reason, results.typeless.reason, results.empty.reason] }
  })

  await test('a store that hashes differently is caught, rather than leaving a dead address', page, async () => {
    // The node is built from the local hash and the store is keyed by the writer's. If they
    // differ, the document names something that resolves to nothing — a broken image forever,
    // with no error anywhere to explain it. So the mismatch is a hard failure.
    const result = await page.evaluate(async () => {
      const { ingestImage } = (window as any).HOLO_INGEST.ingest
      const { png, writer, blobOf } = (window as any).HOLO_INGEST
      try {
        await ingestImage(blobOf(png(4, 4), 'image/png'), writer(false))
        return { reason: null, message: null }
      } catch (e: any) {
        return { reason: e.reason, message: e.message }
      }
    })
    ok(result.reason === 'write-failed', `expected write-failed, got ${result.reason}`)
    ok(result.message!.includes('hash'), `the message should say what diverged: ${result.message}`)
    return { refused: result.reason }
  })

  await test('a figure with an unparseable header is accepted, with no dimensions', page, async () => {
    // Refusing a WebP because its header is awkward to parse would be refusing an ordinary
    // screenshot. The node carries no width/height, the image node view's placeholder holds the
    // layout, and `null` comes back so a caller can say so.
    const result = await page.evaluate(async () => {
      const { ingestImage } = (window as any).HOLO_INGEST.ingest
      const { writer, blobOf } = (window as any).HOLO_INGEST
      const bytes = new Uint8Array([0x52, 0x49, 0x46, 0x46, 1, 2, 3, 4, 0, 0, 0, 0, 0x57, 0x45, 0x42, 0x50])
      const out = await ingestImage(blobOf(bytes, 'image/webp'), writer())
      return { width: out.width, height: out.height, node: out.node }
    })
    ok(result.width === null && result.height === null, `expected null, got ${result.width}x${result.height}`)
    ok(result.node.attrs.width === undefined, 'the node should carry no width')
    ok(result.node.attrs.height === undefined, 'the node should carry no height')
    ok(
      typeof result.node.attrs.src === 'string' && result.node.attrs.src.startsWith('holo-asset://'),
      'but it is still content-addressed',
    )
    return { dimensions: null }
  })

  await test('an SVG is measured from its markup and stored as SVG', page, async () => {
    const result = await page.evaluate(async () => {
      const { ingestImage, sha256 } = (window as any).HOLO_INGEST.ingest
      const { writer, blobOf } = (window as any).HOLO_INGEST
      const svg = '<svg width="200" height="100" xmlns="http://www.w3.org/2000/svg"><rect width="200" height="100"/></svg>'
      const bytes = new TextEncoder().encode(svg)
      const w = writer()
      const out = await ingestImage(blobOf(bytes, 'image/svg+xml'), w)
      return { width: out.width, height: out.height, mime: w.stored[0]?.mime, hash: out.hash, expected: await sha256(bytes) }
    })
    ok(result.width === 200 && result.height === 100, `expected 200x100, got ${result.width}x${result.height}`)
    ok(result.mime === 'image/svg+xml', 'stored as SVG, not re-encoded as PNG')
    ok(result.hash === result.expected, 'and the address is the digest of the markup, so it dedupes like anything else')
    return { dimensions: `${result.width}x${result.height}` }
  })

  await test('a dropped image cannot navigate the window away from the document', page, async () => {
    // The browser's default for a dropped file is to *navigate to it*. A figure dropped on a word
    // processor would otherwise replace the application with a PNG, losing every unsaved
    // keystroke. This drives a real `drop` event and checks the default was prevented.
    const result = await page.evaluate(async () => {
      const { onImageDrop, imagesInTransfer } = (window as any).HOLO_INGEST.ingest
      const target = document.createElement('div')
      document.body.append(target)

      let received = 0
      let kind = ''
      const detach = onImageDrop(target, (files: File[]) => {
        received += files.length
        kind = 'handled'
      })

      const fire = (type: string, files: File[]): boolean => {
        const transfer = new DataTransfer()
        for (const file of files) transfer.items.add(file)
        const event = new DragEvent(type, { dataTransfer: transfer, bubbles: true, cancelable: true })
        target.dispatchEvent(event)
        return event.defaultPrevented
      }

      const { png, blobOf } = (window as any).HOLO_INGEST
      const image = new File([blobOf(png(4, 4), 'image/png')], 'a.png', { type: 'image/png' })
      const text = new File(['hello'], 'a.txt', { type: 'text/plain' })

      const droppedImage = fire('drop', [image])
      const droppedText = fire('drop', [text])
      detach()
      target.remove()

      return { received, kind, droppedImage, droppedText, transferCount: imagesInTransfer(null).length }
    })
    ok(result.droppedImage, 'a dropped file must have its default prevented')
    ok(result.received === 1, `the handler should have received one image, got ${result.received}`)
    ok(result.kind === 'handled', 'and been told what kind of event it was')
    // A dropped *text* file is still prevented: its default is the same navigation, and a
    // document that silently navigates to a .txt is just as broken.
    ok(result.droppedText, 'a dropped text file must also be prevented')
    ok(result.transferCount === 0, 'and a null transfer is empty rather than an error')
    return { prevented: result.droppedImage && result.droppedText }
  })

  await test('a paste with no image is not prevented, so pasting a URL still works', page, async () => {
    // The opposite of the drop rule, deliberately. Preventing every paste would break pasting a
    // link or a word of text, which is the common case; the navigation risk is specific to drops.
    const result = await page.evaluate(async () => {
      const { onImageDrop } = (window as any).HOLO_INGEST.ingest
      const target = document.createElement('textarea')
      document.body.append(target)
      let received = 0
      const detach = onImageDrop(target, () => {
        received += 1
      })

      const pasteText = (): boolean => {
        const event = new ClipboardEvent('paste', { bubbles: true, cancelable: true })
        Object.defineProperty(event, 'clipboardData', { value: null })
        target.dispatchEvent(event)
        return event.defaultPrevented
      }
      const prevented = pasteText()
      detach()
      target.remove()
      return { prevented, received }
    })
    ok(!result.prevented, 'a paste carrying no image must not be prevented')
    ok(result.received === 0, 'and must not reach the image handler')
    return { prevented: false }
  })

  await test('a paste carrying an image is prevented and handled', page, async () => {
    // The reason the paste rule exists at all: a pasted image also carries the source page's text
    // and HTML, so not preventing it would insert both.
    const result = await page.evaluate(async () => {
      const { onImageDrop, imageInClipboard } = (window as any).HOLO_INGEST.ingest
      const target = document.createElement('div')
      document.body.append(target)
      const received: string[] = []
      const kinds: string[] = []
      const detach = onImageDrop(target, (files: File[], sort: string) => {
        received.push(...files.map((f: File) => f.name))
        kinds.push(sort)
      })

      const { png, blobOf } = (window as any).HOLO_INGEST
      const image = new File([blobOf(png(4, 4), 'image/png')], 'shot.png', { type: 'image/png' })
      const transfer = new DataTransfer()
      transfer.items.add(image)
      const event = new ClipboardEvent('paste', { bubbles: true, cancelable: true })
      Object.defineProperty(event, 'clipboardData', { value: transfer })
      target.dispatchEvent(event)

      const found = imageInClipboard(event as any)?.name ?? null
      detach()
      target.remove()
      return { prevented: event.defaultPrevented, received, kinds, found }
    })
    ok(result.prevented, 'a paste carrying an image must be prevented')
    ok(result.received.length === 1 && result.received[0] === 'shot.png', `expected shot.png, got ${result.received.join(', ')}`)
    ok(result.kinds[0] === 'paste', `expected the paste kind, got ${result.kinds.join(', ')}`)
    ok(result.found === 'shot.png', 'and imageInClipboard should find it independently')
    return { received: result.received }
  })

  await test('a multi-image paste reports every file rather than silently dropping two', page, async () => {
    // Copying three screenshots from a file manager produces three files. Taking only the first
    // would lose two without a word, so the caller is handed all of them and decides.
    const result = await page.evaluate(async () => {
      const { imagesInTransfer } = (window as any).HOLO_INGEST.ingest
      const { png, blobOf } = (window as any).HOLO_INGEST
      const transfer = new DataTransfer()
      for (const name of ['a.png', 'b.png', 'c.png']) {
        transfer.items.add(new File([blobOf(png(4, 4), 'image/png')], name, { type: 'image/png' }))
      }
      transfer.items.add(new File(['x'], 'notes.txt', { type: 'text/plain' }))
      return imagesInTransfer(transfer).map((f: File) => f.name)
    })
    ok(result.length === 3, `expected three images, got ${result.length}: ${result.join(', ')}`)
    ok(!result.includes('notes.txt'), 'and the text file should not be counted')
    return { files: result }
  })

  await test('readBytes returns exactly the file contents', page, async () => {
    // A reader that dropped a byte would store a truncated image and hash the wrong thing.
    const result = await page.evaluate(async () => {
      const { readBytes } = (window as any).HOLO_INGEST.ingest
      const { png, blobOf } = (window as any).HOLO_INGEST
      const bytes = png(16, 16)
      const read = await readBytes(blobOf(bytes, 'image/png'))
      return {
        expected: bytes.length,
        actual: read.length,
        matches: Array.from(read as unknown as number[]).every((b, i) => b === bytes[i]),
      }
    })
    ok(result.actual === result.expected, `expected ${result.expected} bytes, got ${result.actual}`)
    ok(result.matches, 'and every byte should match')
    return { bytes: result.actual }
  })

  // -------------------------------------------------------------------------------------------
  // The wiring, not the helper.
  //
  // Every test above attaches `onImageDrop` to a `div` the test creates itself. That proves the
  // helper works; it says nothing about whether the *application* ever attaches it, and it could
  // not have caught what was actually wrong: `attachImageIngestion` existed, was exported on the
  // harness surface, was never called, and the one expression inside it that was reached — the
  // focused-editor lookup — called `registry.focused`, which is a section id, not a function. A
  // `TypeError` on every paste, behind nineteen green tests.
  //
  // So these tests go through the real page: a real `paste` event on the real `#scroller`, with
  // `__TAURI__` faked so `put_asset` answers, and the real Tiptap editor that has focus.
  // -------------------------------------------------------------------------------------------

  /** Fake the bridge and boot a small document. Returns the handle the assertions read. */
  async function bootWithBridge(page: Page) {
    return page.evaluate(async () => {
      const w = window as any
      const stored: Array<{ hash: string; mime: string; bytes: number }> = []
      w.__TAURI__ = {
        core: {
          invoke: async (cmd: string, args: any) => {
            if (cmd === 'put_asset') {
              // A real digest, because `ingest.ts` re-hashes and compares: a fake writer that
              // returns a made-up string is rejected by design.
              const digest = await crypto.subtle.digest('SHA-256', new Uint8Array(args.bytes).buffer)
              const hash = Array.from(new Uint8Array(digest))
                .map(b => b.toString(16).padStart(2, '0'))
                .join('')
              stored.push({ hash, mime: args.mime, bytes: args.bytes.length })
              return hash
            }
            if (cmd === 'get_asset') {
              // The resolver's IPC fallback for `holo-asset://`, used when the custom protocol
              // does not dispatch — which is the case in every browser. Answering it with the
              // shape `getAssetBytes` destructures keeps the page free of errors; the tests
              // assert on the document, not on whether the figure rasterised.
              return ['image/png', []]
            }
            throw new Error(`the ingest wiring test called ${cmd}, which it does not stub`)
          },
        },
        event: { listen: async () => () => {} },
      }
      // Cleared, because the report is a single line that is *replaced* rather than appended,
      // and a test that asserts "nothing was inserted" must not be reading the previous test's
      // success.
      const report = document.getElementById('ingest-report')
      if (report) {
        report.textContent = ''
        report.hidden = true
        report.classList.remove('failed')
      }
      await w.HOLO_SCROLL.loadSynthetic(3, 4)
      w.HOLO_SCROLL.registry.focus('s1')
      return { stored: stored.length }
    })
  }

  /** The image node in the focused section, or null. */
  async function focusedImages(page: Page) {
    return page.evaluate(() => {
      const w = window as any
      const id = w.HOLO_SCROLL.registry.focused
      if (!id) return null
      const editor = w.HOLO_SCROLL.registry.editorIfMounted(id)
      if (!editor) return null
      const found: Array<{ src: string; attrs: Record<string, unknown> }> = []
      editor.state.doc.descendants((node: any) => {
        if (node.type.name === 'image') found.push({ src: node.attrs.src, attrs: { ...node.attrs } })
      })
      return found
    })
  }

  function reportText(page: Page) {
    return page.evaluate(() => {
      const el = document.getElementById('ingest-report') as HTMLElement | null
      return el ? { text: el.textContent ?? '', hidden: el.hidden } : null
    })
  }

  /** Dispatch a real `paste` carrying one PNG at whatever `#scroller` is. */
  function pastePng(page: Page, width = 8, height = 8) {
    return page.evaluate(
      async ({ w, h }) => {
        const { png, blobOf } = (window as any).HOLO_INGEST
        const transfer = new DataTransfer()
        transfer.items.add(new File([blobOf(png(w, h), 'image/png')], 'shot.png', { type: 'image/png' }))
        document
          .getElementById('scroller')!
          .dispatchEvent(new ClipboardEvent('paste', { clipboardData: transfer, bubbles: true }))
      },
      { w: width, h: height },
    )
  }

  await test('the application itself attaches the paste handler to the scroller', page, async () => {
    await bootWithBridge(page)
    await pastePng(page)
    await page.waitForFunction(
      () => {
        const el = document.getElementById('ingest-report') as HTMLElement | null
        return !!el && !el.hidden && (el.textContent ?? '').length > 0
      },
      undefined,
      { timeout: 5000 },
    )
    const report = await reportText(page)
    return { report }
  })

  await test('a pasted image lands in the focused section as a holo-asset:// node', page, async () => {
    await bootWithBridge(page)
    await pastePng(page, 12, 7)
    await page.waitForFunction(
      () => {
        const w = window as any
        const id = w.HOLO_SCROLL.registry.focused
        const editor = id ? w.HOLO_SCROLL.registry.editorIfMounted(id) : null
        let n = 0
        editor?.state.doc.descendants((node: any) => {
          if (node.type.name === 'image') n++
        })
        return n > 0
      },
      undefined,
      { timeout: 5000 },
    )
    const images = await focusedImages(page)
    ok(images !== null, 'no focused editor to inspect')
    ok(images!.length === 1, `expected one image node, got ${images!.length}`)
    // The address is a digest, and never a data: URL. The second half is the invariant the whole
    // pipeline exists for: a 69MB document whose sections carry base64 is a 69MB document whose
    // *edits* carry base64.
    ok(
      /^holo-asset:\/\/[0-9a-f]{64}$/.test(images![0]!.src),
      `src should be holo-asset://<sha256>, got ${images![0]!.src}`,
    )
    ok(!images![0]!.src.startsWith('data:'), `src is an inline data URL: ${images![0]!.src.slice(0, 40)}`)
    const dims = images![0]!.attrs
    ok(
      typeof dims.width === 'number' || dims.width === null,
      'the node should carry the measured width it was given',
    )
    return { src: images![0]!.src.slice(0, 24) + '…', count: images!.length }
  })

  await test('a drop of an image on the scroller inserts it too', page, async () => {
    await bootWithBridge(page)
    await page.evaluate(async () => {
      const { png, blobOf } = (window as any).HOLO_INGEST
      const transfer = new DataTransfer()
      transfer.items.add(new File([blobOf(png(5, 5), 'image/png')], 'drop.png', { type: 'image/png' }))
      document
        .getElementById('scroller')!
        .dispatchEvent(new DragEvent('drop', { dataTransfer: transfer, bubbles: true, cancelable: true }))
    })
    await page.waitForFunction(
      () => {
        const w = window as any
        const id = w.HOLO_SCROLL.registry.focused
        const editor = id ? w.HOLO_SCROLL.registry.editorIfMounted(id) : null
        let n = 0
        editor?.state.doc.descendants((node: any) => {
          if (node.type.name === 'image') n++
        })
        return n > 0
      },
      undefined,
      { timeout: 5000 },
    )
    return await focusedImages(page)
  })

  await test('a paste of no images inserts nothing and claims nothing was inserted', page, async () => {
    await bootWithBridge(page)
    await page.evaluate(() => {
      const transfer = new DataTransfer()
      transfer.items.add(new File(['just text'], 'notes.txt', { type: 'text/plain' }))
      document
        .getElementById('scroller')!
        .dispatchEvent(new ClipboardEvent('paste', { clipboardData: transfer, bubbles: true }))
    })
    await page.waitForTimeout(250)
    const images = await focusedImages(page)
    ok(images !== null, 'no focused editor to inspect')
    ok(images!.length === 0, `a text paste inserted ${images!.length} image node(s)`)
    const report = await reportText(page)
    // Silent, or a report that says nothing was inserted. Reporting a *success* here is the
    // failure that would matter: the user pasted a file and the status bar claimed an image.
    ok(
      report === null || report.hidden || !/inserted/.test(report.text),
      `a paste of no images reported "${report?.text}"`,
    )
    return { report }
  })

  console.log('='.repeat(72))
  console.log(`${passed} passed, ${failed} failed`)
  if (failed) console.log(`failing: ${failures.join(', ')}`)
  await browser.close()
  process.exit(failed === 0 ? 0 : 1)
}

main().catch(e => {
  console.error(e)
  process.exit(1)
})
