/**
 * The boot coordinator: turning a `BootPayload` into loaded sections.
 *
 * # What this is responsible for
 *
 * 1. Work out where we are running — a Tauri window with a store behind it, or a
 *    plain browser with no store and no bridge.
 * 2. Get a payload, from whichever of those we are.
 * 3. Decompress the section content, because it crosses the bridge as zstd bytes.
 * 4. Turn the manifest into `SectionRecord`s, so the registry and the geometry have
 *    something to hold.
 *
 * Everything after this belongs to `main.ts`: seeding the geometry, mounting the
 * visible window, starting the scroller.
 *
 * # Why the environment is detected rather than configured
 *
 * A build flag or a query parameter would be one more thing that can be wrong, and
 * being wrong here means silently booting from the wrong source — a browser falling
 * back to a synthetic document inside a packaged app looks like a working app with
 * no data in it. `hasBridge()` asks the only question that matters: is there a host
 * that can answer?
 *
 * # Why zstd is decompressed here and not in Rust
 *
 * The payload carries `content_zstd: Vec<u8>` because the store's blobs are already
 * compressed and re-compressing them to send them uncompressed would be work thrown
 * away on one side to save bytes on the other. That leaves the renderer holding a
 * zstd decoder, which is a dependency and a place for a bug to live.
 *
 * The alternative was measured rather than argued: for a 12-section boot window the
 * frames are ~84KB compressed against ~144KB raw. 60KB on the path to first paint,
 * for a 10KB decoder that decodes at roughly 300MB/s. So the decoder earns its
 * place — but the interop is *verified*, not assumed: `test/boot.ts` decodes frames
 * produced by the real Rust encoder and requires byte-exact JSON back, Unicode
 * included. A decoder that quietly mangled a multi-byte character would be worse
 * than no compression at all.
 */

import { decompress } from 'fzstd'
import type { BootPayload, ManifestSection, SectionContent } from './boot'
import type { SectionRecord } from './registry.js'

/**
 * Whether a Tauri host is present.
 *
 * Duplicated from `geometry-bridge.ts` rather than imported, and that is deliberate:
 * this module takes its payload source as an argument, so it holds no transport and
 * no dependency on the module that does. That is what lets a Node test boot a real
 * payload without a bundler in the way — and `node --experimental-strip-types` cannot
 * resolve the `.js` extensions the browser sources use.
 *
 * It reads the same global for the same reason, so the two cannot disagree about
 * whether there is a host.
 */
export function hasBridge(): boolean {
  return typeof (globalThis as any).__TAURI__?.core?.invoke === 'function'
}

/** Fetches a payload from the bridge. The one place the IPC call happens. */
export type PayloadSource = (documentId?: string) => Promise<BootPayload>

/** Where a payload came from. Recorded so the UI and the tests can say so. */
export type BootSource = 'bridge' | 'fallback'

export interface BootResult {
  /** Sections in document order, ready for the registry. */
  readonly records: SectionRecord[]
  /** The payload the records were built from. */
  readonly payload: BootPayload
  readonly source: BootSource
  /**
   * Ids whose content arrived in the payload. A section outside this set has a
   * manifest row but no content, and mounting it would give the user an empty editor
   * whose first keystroke overwrites the stored section.
   *
   * # What happens to those sections today
   *
   * The payload carries the manifest for the whole document and content for the
   * first twelve. That is the design — the manifest is ~100 bytes per section, so a
   * 2000-page document is ~50KB and the scrollbar is correct before anything mounts —
   * but there is no `get_section` command to ask for the rest.
   *
   * So the honest behaviour is: **a document longer than the boot window is scrollable
   * but not editable past its end**, and `main.ts` says so in the status bar rather
   * than mounting an editor that would destroy content on first keystroke.
   *
   * Adding `get_section` is the fix and it is small — the store already has
   * `section_bytes`, and the same map-and-debounce batching applies. It is listed in
   * `STATUS.md` rather than smuggled in here, because a command on the IPC surface
   * deserves its own decision and its own capability review.
   */
  readonly withContent: ReadonlySet<string>
}

/** Decode a payload's zstd section content into ProseMirror JSON. */
function decodeSection(c: SectionContent): { id: string; json: unknown } {
  // Strict about the shape, because the generated type says `Uint8Array` and that is
  // what the wire delivers: `rmp-serde` writes a MessagePack binary value and
  // `@msgpack/msgpack` decodes it to a `Uint8Array`. A plain array would mean the
  // bytes took a detour through JSON, which would be a bug somewhere upstream and
  // worth failing on rather than quietly repairing with `Uint8Array.from`.
  if (!(c.content_zstd instanceof Uint8Array)) {
    throw new Error(
      `section ${c.id} content arrived as ${
        c.content_zstd === null ? 'null' : typeof c.content_zstd
      }, not a Uint8Array. rmp-serde writes Vec<u8> as a MessagePack binary and ` +
        '@msgpack/msgpack decodes it to a Uint8Array, so this means the payload was ' +
        're-encoded as JSON somewhere on the path.',
    )
  }
  let bytes: Uint8Array
  try {
    bytes = decompress(c.content_zstd)
  } catch (e) {
    // A corrupt frame must name the section and the byte length. "zstd error" on its
    // own, from a boot path, is the least actionable message available.
    throw new Error(
      `section ${c.id} could not be decompressed (${c.content_zstd.length} bytes): ${e}`,
    )
  }
  const text = new TextDecoder().decode(bytes)
  try {
    return { id: c.id, json: JSON.parse(text) }
  } catch (e) {
    throw new Error(`section ${c.id} decompressed to ${bytes.length} bytes that are not JSON: ${e}`)
  }
}

/**
 * Build a `SectionRecord` from a manifest row.
 *
 * The manifest carries the metrics and the geometry needs them; the content comes
 * from the boot window if it was included and is otherwise left as an empty doc.
 *
 * # Why an absent section gets an empty doc rather than being skipped
 *
 * Two bad options were available. Skipping it would make the registry's index and
 * the geometry's index disagree, and every offset in the document would be wrong by
 * one — a silently corrupt scrollbar. An empty doc keeps the indices aligned and
 * makes the section visibly empty, which `main.ts` reports.
 */
function recordFrom(s: ManifestSection, content: Map<string, unknown>): SectionRecord {
  const json = content.get(s.id)
  return {
    id: s.id,
    // The presence of the json *is* the answer: this function is given a map of the
    // sections whose content actually arrived, and a section absent from it has bytes
    // still in SQLite. So `loaded` is not a separate fact to be threaded -- it is read
    // off the same thing.
    loaded: json !== undefined,
    json: json ?? { type: 'doc', content: [] },
    metrics: {
      words: s.word_count,
      marks: s.mark_count,
      chars: s.char_count,
      // Straight from the manifest, never derived. See `estimateHeight`: a derived
      // block count is 225% out on short multi-paragraph sections, and the whole
      // point of storing it is that it is not derivable.
      blocks: s.block_count,
    },
    dirty: false,
  }
}

/** Assemble records from a payload that has already been fetched. */
export function recordsFrom(payload: BootPayload): BootResult {
  const content = new Map<string, unknown>()
  for (const c of payload.visible) {
    const decoded = decodeSection(c)
    content.set(decoded.id, decoded.json)
  }
  return {
    records: payload.sections.map(s => recordFrom(s, content)),
    payload,
    source: 'bridge',
    withContent: new Set(content.keys()),
  }
}

/**
 * The calibration, for a caller that has not loaded one.
 *
 * Synchronous because the browser fallback cannot await: `main.ts` fetches
 * `/calibration.json` once before boot, and this only reports what it found. If it is
 * absent the caller gets an error naming the command that generates it, because a
 * default here would produce a geometry measured against a model nothing else uses.
 */
let harnessCalibration: BootPayload['calibration'] | null = null

export function setHarnessCalibration(cal: BootPayload['calibration']): void {
  harnessCalibration = cal
}

function requireSyntheticCalibration(): BootPayload['calibration'] {
  if (!harnessCalibration) {
    throw new Error(
      'no calibration loaded. main.ts fetches /calibration.json before boot; a ' +
        'missing one means it was not generated. Run:\n' +
        '  cargo run --release --bin emit-calibration > app/public/calibration.json',
    )
  }
  return harnessCalibration
}

/** One harness section's text, shared by the manifest row and the content. */
function harnessParagraph(i: number, p: number, charsPerPara: number): string {
  return `Body ${i}.${p} ` + 'lorem ipsum dolor sit amet '.repeat(Math.ceil(charsPerPara / 27)).slice(0, charsPerPara)
}

/**
 * The browser's stand-in for a document.
 *
 * # This is a stand-in, and it says so
 *
 * There is no store in a browser, so there is no document. The harness needs *a*
 * shape — with a real calibration, so the geometry is the geometry — and this builds
 * the smallest one that exercises every code path that cares: many sections, so the
 * scrollbar is non-trivial, and content for all of them, so every section is
 * mountable.
 *
 * Content is built **once** and used twice: as the local JSON and, via
 * `blocksFor`, as the source of the manifest's character and block counts. An earlier
 * version built the paragraphs twice, in two functions, and the two loops had to be
 * kept in step by hand — which is the arrangement where a fixture's metrics quietly
 * stop describing its content, and the only symptom is a wrong scrollbar.
 *
 * It is deliberately *not* cached across calls. A cached fixture a test mutated would
 * make the next test's document depend on the previous test's leftovers, which is the
 * failure mode this project has hit often enough to be suspicious of shared mutable
 * state.
 */
function harnessDocument(sections: number, paragraphs: number, charsPerPara: number) {
  const blocksFor = (i: number): unknown[] =>
    Array.from({ length: paragraphs }, (_, p) => ({
      type: 'paragraph',
      content: [{ type: 'text', text: harnessParagraph(i, p, charsPerPara) }],
    }))

  const cal = requireSyntheticCalibration()
  const manifest: ManifestSection[] = []
  const content = new Map<string, unknown>()
  for (let i = 0; i < sections; i++) {
    const id = `s${i}`
    const blocks = blocksFor(i)
    content.set(id, { type: 'doc', content: blocks })
    // Every count is derived from the text that was just built, by measuring it.
    //
    // This was `paragraphs * charsPerPara` and `paragraphs * 90`, which is wrong by
    // construction: the `Body ${i}.${p} ` prefix means each paragraph is longer than
    // `charsPerPara`, so the manifest claimed 2480 characters for content holding
    // 2516. `test/boot.ts` caught it. That is the fixture's whole job — the scroll
    // tests measure compensation against these numbers, and a manifest that
    // misdescribes its own content makes them measure the wrong thing while still
    // passing.
    const text = blocks.map(b => (b as any).content[0].text as string).join('')
    manifest.push({
      id,
      order_key: 1024 * (i + 1),
      title: null,
      word_count: text.split(/\s+/).filter(w => w.length > 0).length,
      mark_count: 0,
      char_count: text.length,
      block_count: blocks.length,
      created_at: 0,
      updated_at: 0,
    })
  }

  const payload: BootPayload = {
    document_id: 'harness',
    title: 'Harness document',
    calibration: cal,
    sections: manifest,
    // An empty frame, never decoded: the harness document bypasses `recordsFrom`
    // entirely because it already has local JSON. `visible` is populated so the
    // payload's *shape* is the real one, which is what makes it usable as a fixture
    // — a payload whose `visible` is empty would not exercise the join between the
    // manifest and the content list.
    visible: [...content.keys()].map(id => ({ id, content_zstd: new Uint8Array(0) })),
    focused_section_id: null,
    scroll_top: null,
  }
  return { payload, content }
}

/**
 * Boot.
 *
 * Under Tauri this fetches the real payload over MessagePack and decompresses the
 * section frames. In a browser it builds the harness document, and reports
 * `source: 'fallback'` so nothing downstream mistakes it for real data.
 */
export async function bootDocument(
  options: {
    documentId?: string
    fallbackSections?: number
    fallbackParagraphs?: number
    /**
     * How to reach the bridge. Injected rather than imported so this module has no
     * transport in it, and so all three bridge calls are named in one place —
     * `main.ts`.
     *
     * Defaults to a call that fails with a message naming the command, rather than
     * quietly falling back: a caller who asked to boot from the bridge and got the
     * harness document instead would have no way to tell.
     */
    fetchPayload?: PayloadSource
  } = {},
): Promise<BootResult> {
  const source = options.fetchPayload ?? defaultPayloadSource
  if (hasBridge()) {
    const payload = await source(options.documentId)
    return recordsFrom(payload)
  }

  const { payload, content } = harnessDocument(
    options.fallbackSections ?? 12,
    options.fallbackParagraphs ?? 15,
    620,
  )
  return {
    records: payload.sections.map(s => recordFrom(s, content)),
    payload,
    source: 'fallback',
    withContent: new Set(content.keys()),
  }
}

/**
 * The payload source used when the caller supplies none.
 *
 * Fails rather than falling back. `bootDocument` already checks for a host, so
 * reaching this means one of the two disagreed about whether there is a bridge — and
 * a boot that quietly used a synthetic document there would look like a working app
 * with an empty document in it.
 */
const defaultPayloadSource: PayloadSource = async () => {
  throw new Error(
    'bootDocument was asked for a bridge payload with no source configured. Pass ' +
      '`fetchPayload`, which in the app is `(id) => getDocumentBoot(id)` from ' +
      'geometry-bridge.ts.',
  )
}
