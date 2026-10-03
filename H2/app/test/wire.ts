/**
 * The MessagePack wire format, pinned from the frontend side.
 *
 * # Why this file exists, and what it cost to not have it
 *
 * `content_zstd` is declared `#[ts(type = "Uint8Array")]`. That annotation says what the
 * *TypeScript* is; it does nothing to what the *encoder* writes. `rmp-serde` serialises a
 * plain `Vec<u8>` as a MessagePack array of numbers, and `@msgpack/msgpack` decodes an
 * array to `number[]`, so every section payload coming off the bridge was a `number[]`
 * and both frontend decoders — which check `instanceof Uint8Array` and throw rather than
 * repair — refused it.
 *
 * The boot content path had therefore never worked. It went unnoticed because
 * `test/boot.ts` encoded its fixture with `@msgpack/msgpack`'s own `encode`, which *does*
 * write a `Uint8Array` as `bin`. The test agreed with itself and with neither the real
 * encoder nor the real decoder, which is the exact shape of a parity test that pins
 * nothing.
 *
 * These fixtures are written by `cargo run -p holonomy-shell --example gen_wire_fixture`,
 * so they are the real encoder's output. `crates/holonomy-shell/tests/wire-format.rs` is
 * the same claim from the other end. If `serde_bytes` is ever dropped from
 * `SectionContent`, both fail.
 *
 * Run: node --experimental-strip-types test/wire.ts
 */

import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { decode, encode } from '@msgpack/msgpack'
import { decompress } from 'fzstd'

const fixtures = join(dirname(fileURLToPath(import.meta.url)), 'fixtures')

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

function fixture(name: string): Uint8Array {
  return new Uint8Array(readFileSync(join(fixtures, name)))
}

console.log('wire: the MessagePack bytes rmp-serde actually writes')
console.log('='.repeat(72))

test('section content arrives as binary, not as an array of numbers', () => {
  const decoded = decode(fixture('rust-msgpack-section.bin')) as { id: string; content_zstd: unknown }
  ok(decoded.id === 'wire-fixture', `the payload should name its section, got ${decoded.id}`)
  // The assertion that was failing in the real window. `typeof` cannot distinguish a
  // `Uint8Array` from a plain object, so this is written as the same check the decoders
  // perform — which is the point: if the decoders change, this changes with them.
  ok(
    decoded.content_zstd instanceof Uint8Array,
    `content_zstd arrived as ${
      decoded.content_zstd === null ? 'null' : typeof decoded.content_zstd
    }, not a Uint8Array; rmp-serde serialises a plain Vec<u8> as a MessagePack array, so ` +
      'SectionContent::content_zstd needs #[serde(with = "serde_bytes")]',
  )
  return { bytes: (decoded.content_zstd as Uint8Array).length }
})

test('the fixture really decompresses to ProseMirror JSON', () => {
  // Guards the guard: a `bin` tag with garbage inside it would pass the shape assertion
  // above while proving nothing about the path.
  const decoded = decode(fixture('rust-msgpack-section.bin')) as { content_zstd: Uint8Array }
  const json = JSON.parse(new TextDecoder().decode(decompress(decoded.content_zstd)))
  ok(json.type === 'doc', `the payload should be a document, got type=${json.type}`)
  ok(Array.isArray(json.content) && json.content.length === 8, `expected 8 paragraphs, got ${json.content?.length}`)
  ok(
    JSON.stringify(json).includes('wire fixture') === false,
    'the fixture text should identify itself by section number, not a fixed string',
  )
  return { paragraphs: json.content.length }
})

test('a whole boot payload decodes, with every section as binary', () => {
  const boot = decode(fixture('rust-msgpack-boot.bin')) as {
    document_id: string
    sections: Array<{ block_count: number }>
    visible: Array<{ id: string; content_zstd: unknown }>
    calibration: unknown
    scroll_top: number | null
  }
  ok(boot.document_id === 'wire-fixture', `document_id was ${boot.document_id}`)
  ok(boot.sections.length === 8, `expected 8 manifest rows, got ${boot.sections.length}`)
  ok(boot.visible.length === 2, `expected 2 sections with content, got ${boot.visible.length}`)
  for (const v of boot.visible) {
    ok(
      v.content_zstd instanceof Uint8Array,
      `visible section ${v.id} content is not binary; the boot payload has the same defect the ` +
        'get_section path had, because it is the same type',
    )
  }
  // The counts the geometry depends on. A boot payload that decoded but lost an integer
  // would look fine here and then estimate every height wrong.
  ok(
    boot.sections.every(r => typeof r.block_count === 'number' && r.block_count > 0),
    'block_count must survive as a number; the height model has no fallback for it',
  )
  ok(typeof boot.calibration === 'object' && boot.calibration !== null, 'calibration must decode')
  ok(boot.scroll_top === null, 'a first open has no scroll offset, and it must decode as null')
  return { manifest: boot.sections.length, withContent: boot.visible.length }
})

test('binary encoding is materially smaller than the array it replaced', () => {
  // The reason MessagePack was chosen for this one payload, and the reason `serde_bytes`
  // is not a detail. `rmp-serde` with a plain `Vec<u8>` writes one integer per byte; at
  // level 3 a 7KB section becomes roughly 30KB, which is *larger* than the JSON it
  // replaced — the argument in `get_document_boot`'s doc comment inverted by the field it
  // was about.
  //
  // Asserted rather than assumed, because "it is smaller" is the kind of claim that
  // quietly stops being true when the encoder or the compression level changes.
  const real = decode(fixture('rust-msgpack-boot.bin')) as {
    visible: Array<{ id: string; content_zstd: Uint8Array }>
  }
  const compressed = real.visible.reduce((a, v) => a + v.content_zstd.length, 0)

  // Re-encode the same bytes the way the un-annotated field would have: an array of
  // numbers, which is exactly what `rmp-serde` produces for a bare `Vec<u8>`.
  const asArray = real.visible.reduce(
    (a, v) => a + encode({ id: v.id, content_zstd: Array.from(v.content_zstd) }).length,
    0,
  )
  const ratio = asArray / compressed

  // # What is not asserted here, and why
  //
  // The cost that mattered most is not on the wire: a `number[]` of N elements occupies
  // at least 8N bytes of JS heap, one 64-bit slot each, where a `Uint8Array` costs N.
  // Thirty resident sections of ~7KB is ~210KB as binary and ~1.7MB as arrays, against
  // a cache whose whole purpose is to bound renderer memory.
  //
  // That is arithmetic about a runtime's object layout, not a measurement, so it is not
  // turned into an assertion. An earlier version of this check asserted it and the
  // assertion compared `compressed * 8` against a figure derived from `compressed * 8` —
  // a tautology that could only fail if the code above it were edited. An assertion
  // about heap layout belongs behind a measurement, and the honest version is
  // `performance.measureUserAgentSpecificMemory()`, which is not available in the
  // webkit2gtk window this project verifies on. The reasoning stands; it is not pinned.
  //
  // The threshold is 1.4, not 2, and the arithmetic says why. A MessagePack array element
  // is one byte for a value under 128, two under 256, three under 65536 — so compressed
  // data, which is not ASCII, averages around 1.6 bytes per byte and can reach 5x for a
  // payload of 0xff bytes. Asserting 2x here would be asserting a number chosen to be
  // convenient rather than one the encoder produces, and it would fail the moment the
  // compressor's output distribution shifted.
  ok(
    ratio > 1.4,
    `the array encoding should be at least 1.4x the size for compressed data, got ${ratio.toFixed(2)}x ` +
      `(${compressed} bytes as bin, ${asArray} as an array)`,
  )

  return { compressed, asArray, ratio: Number(ratio.toFixed(2)) }
})

test('the tag the fixture records is a bin tag', () => {
  // `gen_wire_fixture` writes the tag it found alongside the payload. Asserting on it
  // means a failure can be diagnosed from a two-byte file rather than by reading the
  // MessagePack stream, which matters when the thing that failed is the encoder.
  const tag = parseInt(readFileSync(join(fixtures, 'rust-msgpack-section.tag'), 'utf8').trim(), 16)
  ok(
    tag >= 0xc4 && tag <= 0xc6,
    `the recorded value tag is 0x${tag.toString(16)}; 0xc4..0xc6 is MessagePack bin and 0x90..0x9f is an array`,
  )
  return { tag: `0x${tag.toString(16)}` }
})

console.log('='.repeat(72))
console.log(`${passed} passed, ${failed} failed`)
if (failed) {
  console.log(`failing: ${failures.join(', ')}`)
  process.exit(1)
}