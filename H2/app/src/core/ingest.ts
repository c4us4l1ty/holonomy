/**
 * Getting bytes out of a paste or a drop, and into the document as an address.
 *
 * # Why this module exists rather than living in the editor's paste handler
 *
 * Because the path from "there is a PNG in the clipboard" to "there is a `holo-asset://` node in
 * the document" has three steps and two failure modes, and each of them is worth a test that does
 * not need a window:
 *
 * 1. **Find the image.** A paste carries HTML *and* an image and, on Linux, sometimes neither.
 * 2. **Measure it.** The `image` node's `width`/`height` are what give a figure its layout box
 *    before its bytes arrive, and a figure with no box is a section whose height changes as you
 *    scroll — which is the exact failure the whole geometry exists to prevent.
 * 3. **Store it and address it.** SHA-256 into the `assets` table, then the node names the digest.
 *
 * # Why the bytes never go into the document
 *
 * Because that is the invariant `core/assets.ts` exists to enforce, and this is the only place
 * that could violate it. A pasted image has to cross the IPC boundary as bytes exactly once, be
 * hashed, and be written; after that the document holds a 64-character address and the bytes live
 * in SQLite. A base64 `data:` URL in section JSON would be a third more bytes, re-parsed on every
 * read of the section, and copied by every keystroke that snapshots it.
 *
 * # Why the digest is computed here and not in Rust
 *
 * Because `crypto.subtle.digest` is one call, needs no round trip, and the frontend already has
 * the bytes — it just pulled them out of a clipboard. Sending them to Rust to be hashed would be
 * a second copy of the same buffer for no gain. What Rust does is the *write*, which is where the
 * bytes have to end up anyway.
 *
 * # Why dimensions are read here and not left to the image node
 *
 * Because the node view cannot: it renders after the section is mounted, and by then the section's
 * height has already been committed to the geometry. Reading them at insertion time is the only
 * point where a measurement can still affect the layout.
 */

import { assetUrl, isInlineDataUrl } from './assets.js'

/** What a figure becomes, once it is in the document. */
export interface IngestedImage {
  /** The SHA-256 of the bytes, lowercase hex. The asset's key and its address. */
  readonly hash: string
  readonly mime: string
  /** Pixel dimensions, or `null` when the format could not be measured. */
  readonly width: number | null
  readonly height: number | null
  /** ProseMirror JSON for the node. */
  readonly node: Record<string, unknown>
}

/** The writer this module needs. Injected so a test does not need a Tauri host. */
export interface AssetWriter {
  /** Store bytes and return their digest. */
  put(bytes: Uint8Array, mime: string): Promise<string>
}

/** The raster and vector formats a document may contain. */
export const ACCEPTED_MIME = [
  'image/png',
  'image/jpeg',
  'image/gif',
  'image/webp',
  'image/avif',
  'image/bmp',
  'image/svg+xml',
] as const

/**
 * Whether a MIME type is one this module will store.
 *
 * Not a prefix test on `image/`, deliberately: `image/tiff` and `image/x-icon` satisfy it and
 * neither decodes in the webview, so a document could end up holding a figure that renders as a
 * broken-image glyph. The allowlist is the honest version and it is short enough to read.
 */
export function isAcceptedImage(mime: string): boolean {
  return (ACCEPTED_MIME as readonly string[]).includes(mime.toLowerCase())
}

/** The file extension for a MIME type, for a download or a status message. */
export function extensionFor(mime: string): string {
  switch (mime.toLowerCase()) {
    case 'image/png':
      return 'png'
    case 'image/jpeg':
      return 'jpg'
    case 'image/gif':
      return 'gif'
    case 'image/webp':
      return 'webp'
    case 'image/avif':
      return 'avif'
    case 'image/bmp':
      return 'bmp'
    case 'image/svg+xml':
      return 'svg'
    default:
      return 'bin'
  }
}

/**
 * The image files in a `DataTransfer`, in preference order.
 *
 * # Why files are preferred over `text/html`
 *
 * Because `text/html` is what a *pasted web page* carries, and it is full of things that are not
 * the image: a wrapper element, a `srcset`, a tracking pixel, sometimes a `data:` URL of the whole
 * rendered element. Taking `text/html` and fishing an `<img>` out of it is how a paste ends up
 * inserting a 1×1 spacer or the site's logo instead of the figure the user copied.
 *
 * # Why several may be returned
 *
 * Because a multi-image paste is ordinary — copying three screenshots from a file manager produces
 * three files — and inserting only the first would silently drop two. The caller decides whether
 * to insert them all or only the first; this function's job is to not decide that.
 */
export function imagesInTransfer(transfer: DataTransfer | null): File[] {
  if (!transfer) return []
  const fromFiles: File[] = []
  if (transfer.files) {
    for (const file of Array.from(transfer.files)) {
      if (isAcceptedImage(file.type)) fromFiles.push(file)
    }
  }
  if (fromFiles.length > 0) return fromFiles

  // No usable files. Fall back to items, which is where a paste puts things on some engines.
  const fromItems: File[] = []
  if (transfer.items) {
    for (const item of Array.from(transfer.items)) {
      if (item.kind !== 'file') continue
      const file = item.getAsFile()
      if (file && isAcceptedImage(file.type)) fromItems.push(file)
    }
  }
  return fromItems
}

/**
 * The image in a `ClipboardEvent`, if there is one.
 *
 * # Why this is not the paste handler
 *
 * Because "is there an image here" is a question with an answer that changes what the paste
 * *means*: an image paste should not also insert the page's text. A handler that returns a boolean
 * and leaves the decision to the caller keeps that policy in one place instead of spread across
 * every listener.
 */
export function imageInClipboard(event: ClipboardEvent | null): File | null {
  if (!event?.clipboardData) return null
  const files = imagesInTransfer(event.clipboardData)
  return files[0] ?? null
}

/**
 * A rejection from ingestion, with a message a person can act on.
 *
 * # Why `reason` is a closed set and not just the message
 *
 * Because the four reasons need four different responses from a caller, and a caller branching on
 * message text is a caller that breaks when a message is reworded. `too-large` and `unmeasured`
 * are the user's problem and deserve a sentence; `write-failed` is the store's problem and
 * deserves a log; `not-an-image` is a UI filter that should not have been reached.
 *
 * # Why the fields are assigned rather than declared as parameter properties
 *
 * Because this module is loaded under `node --experimental-strip-types`, which erases types
 * without transforming syntax and rejects `constructor(private readonly x: T)`. The fields are
 * still `readonly` in the type; they are just not written as a shorthand.
 */
export type IngestFailure = 'too-large' | 'unreadable' | 'unmeasured' | 'write-failed' | 'not-an-image'

export class IngestError extends Error {
  /** Which kind of failure this is. See the type for why it is not the message. */
  readonly reason: IngestFailure
  /** Anything a caller might want to log: a size, a MIME type, two hashes. */
  readonly detail: string

  constructor(message: string, reason: IngestFailure, detail: string = '') {
    super(message)
    this.name = 'IngestError'
    this.reason = reason
    this.detail = detail
  }
}

/** The largest figure this module will store, in bytes. */
export const MAX_IMAGE_BYTES = 32 * 1024 * 1024

/**
 * Why there is a size limit, and why it is 32MB.
 *
 * Because the limit that matters is not the file's size but what it does to the *renderer*. A
 * decoded image occupies roughly `width * height * 4` bytes, so a 20,000×20,000 PNG under the
 * limit is 1.6GB in the renderer's heap — the document would have to be closed to recover, in an
 * application whose entire argument is that memory does not track document length.
 *
 * 32MB of compressed bytes is roughly 8,000×8,000 decoded, which is larger than any figure a
 * person pastes and small enough that a handful of them is not a problem. The check is on
 * *compressed* size because that is what can be checked before decoding, and the pixel-count
 * check below is what catches the decompression bomb.
 */

/**
 * The pixel count above which a figure is refused.
 *
 * Checked from the header rather than by decoding, because decoding a decompression bomb is the
 * damage. Reading a PNG's `IHDR` or a JPEG's `SOF` is a few bytes of arithmetic and cannot be made
 * to allocate 1.6GB.
 */
export const MAX_IMAGE_PIXELS = 64_000_000 // 8000x8000

/** The header bytes needed to measure a PNG or a JPEG without decoding it. */
const HEADER_BYTES = 65_536

/**
 * Read a PNG or JPEG's dimensions from its header, without decoding the image.
 *
 * # Why the header and not `createImageBitmap`
 *
 * Because `createImageBitmap` decodes, and decoding is the thing being guarded against. A
 * decompression bomb is a small file that expands to a gigabyte, so the measurement has to come
 * before the decode.
 *
 * # Why PNG and JPEG only
 *
 * Because those are the two formats with a fixed, simple header layout, and they are what
 * screenshots and photographs actually are. WebP, AVIF and GIF have variable or chunked headers
 * that would need real parsers, so for those the measurement is skipped and `null` is reported
 * rather than a guess. A figure with `null` dimensions still renders; it just has no layout box
 * until the image node view adds one, and it is reported so a caller can say so.
 */
export function measureFromHeader(bytes: Uint8Array, mime: string): { width: number; height: number } | null {
  if (mime === 'image/png') return measurePng(bytes)
  if (mime === 'image/jpeg') return measureJpeg(bytes)
  return null
}

function measurePng(bytes: Uint8Array): { width: number; height: number } | null {
  // 8-byte signature, then an IHDR chunk whose payload starts at 16: width and height are two
  // big-endian uint32s.
  if (bytes.length < 24) return null
  const signature = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]
  for (let i = 0; i < signature.length; i++) {
    if (bytes[i] !== signature[i]) return null
  }
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength)
  return { width: view.getUint32(16), height: view.getUint32(20) }
}

function measureJpeg(bytes: Uint8Array): { width: number; height: number } | null {
  // Walk the marker segments looking for a start-of-frame, which is where the dimensions are.
  // The first three bytes are SOI and then a segment; the interesting markers are C0-C3, C5-C7,
  // C9-CB, CD-CF, all of which carry a frame header.
  if (bytes.length < 4 || bytes[0] !== 0xff || bytes[1] !== 0xd8) return null
  let offset = 2
  while (offset + 9 < bytes.length) {
    if (bytes[offset] !== 0xff) {
      offset += 1
      continue
    }
    const marker = bytes[offset + 1]!
    // Start of scan or end of image: no frame header follows, so there is nothing to find.
    if (marker === 0xda || marker === 0xd9) return null
    const length = (bytes[offset + 2]! << 8) | bytes[offset + 3]!
    const isFrame =
      (marker >= 0xc0 && marker <= 0xc3) ||
      (marker >= 0xc5 && marker <= 0xc7) ||
      (marker >= 0xc9 && marker <= 0xcb) ||
      (marker >= 0xcd && marker <= 0xcf)
    if (isFrame) {
      if (offset + 9 >= bytes.length) return null
      return {
        height: (bytes[offset + 5]! << 8) | bytes[offset + 6]!,
        width: (bytes[offset + 7]! << 8) | bytes[offset + 8]!,
      }
    }
    // A marker with no length, or a standalone one like 0x01, occupies a single byte.
    if (length < 2) {
      offset += 2
      continue
    }
    offset += 2 + length
  }
  return null
}

/** Read an SVG's `width`/`height` or `viewBox`, if it has one. */
export function measureSvg(text: string): { width: number; height: number } | null {
  // Only the root element's attributes, and by pattern rather than by parsing: a full XML parser
  // is a dependency, and the four things worth reading are four attributes on the first tag.
  const open = text.slice(0, 2048)
  const tag = /<svg\b[^>]*>/i.exec(open)
  if (!tag) return null
  const attrs = tag[0]

  const number = (name: string): number | null => {
    const match = new RegExp(`\\b${name}\\s*=\\s*["']?\\s*([0-9.]+)`, 'i').exec(attrs)
    if (!match) return null
    const value = Number.parseFloat(match[1]!)
    return Number.isFinite(value) && value > 0 ? value : null
  }
  const width = number('width')
  const height = number('height')
  if (width !== null && height !== null) return { width, height }

  // A `viewBox` gives the aspect ratio but no size, which is the common case for a generated
  // diagram: `viewBox="0 0 200 100"` with no width or height at all.
  //
  // So the box's own dimensions are used as a nominal size, scaled by whichever of
  // width/height *is* present if one is. The scale is arbitrary and the ratio is not — the ratio
  // is what layout needs, and taking it from the box is exact.
  //
  // The first version handled only the one-sided cases and fell through to `null` when both were
  // absent, which is the *most* common shape and so reported no dimensions for a typical diagram.
  // A figure with no dimensions has no layout box, and its section's height changes when the
  // image loads — the exact failure the geometry exists to prevent.
  const viewBox = /\bviewBox\s*=\s*["']\s*([0-9.\s-]+)["']/i.exec(attrs)
  if (!viewBox) return null
  const parts = viewBox[1]!.trim().split(/[\s,]+/).map(Number)
  if (parts.length !== 4 || parts.some(n => !Number.isFinite(n))) return null
  const boxWidth = parts[2]!
  const boxHeight = parts[3]!
  if (boxWidth <= 0 || boxHeight <= 0) return null

  if (width !== null) return { width, height: Math.round((width * boxHeight) / boxWidth) }
  if (height !== null) return { width: Math.round((height * boxWidth) / boxHeight), height }
  return { width: Math.round(boxWidth), height: Math.round(boxHeight) }
}

/** The digest of `bytes`, lowercase hex. */
export async function sha256(bytes: Uint8Array): Promise<string> {
  const digest = await crypto.subtle.digest('SHA-256', bytes as unknown as BufferSource)
  return Array.from(new Uint8Array(digest))
    .map(byte => byte.toString(16).padStart(2, '0'))
    .join('')
}

/**
 * Read a file's bytes.
 *
 * `arrayBuffer` rather than `FileReader`: it is a promise, it is on every engine this project
 * targets including webkit2gtk 2.60, and the callback API is not.
 */
export async function readBytes(file: Blob): Promise<Uint8Array> {
  return new Uint8Array(await file.arrayBuffer())
}

/**
 * Store an image and describe the node that will reference it.
 *
 * # The order, and why measurement is refused rather than guessed
 *
 * Measure, then hash, then write. Measurement first because it is the only step that can refuse
 * cheaply, and refusing a 20,000×20,000 figure before hashing it is better than after. Hash
 * before write because the write's return value is the hash anyway, so hashing here is one call
 * that also gives the caller the address for the node — and it means an address is never
 * constructed by a caller, which is how a document ends up pointing at nothing.
 *
 * # Why an unmeasurable format is still accepted
 *
 * Because refusing a WebP because its header is awkward to parse would be refusing a perfectly
 * ordinary screenshot. The dimensions are reported as `null`, the node carries no `width`/`height`,
 * and the image node view's placeholder keeps the section's height from moving once the image
 * loads. Refusing would be the worse failure, and the caller can see `null` and say so.
 */
export async function ingestImage(
  file: Blob,
  writer: AssetWriter,
  options: { alt?: string } = {},
): Promise<IngestedImage> {
  const mime = (file.type || 'application/octet-stream').toLowerCase()
  if (!isAcceptedImage(mime)) {
    throw new IngestError(
      `${mime || 'that file'} is not an image format a document can hold`,
      'not-an-image',
      mime,
    )
  }
  if (file.size > MAX_IMAGE_BYTES) {
    throw new IngestError(
      `the image is ${(file.size / 1024 / 1024).toFixed(1)}MB, over the ${MAX_IMAGE_BYTES / 1024 / 1024}MB limit`,
      'too-large',
      String(file.size),
    )
  }
  if (file.size === 0) {
    throw new IngestError('the image is empty', 'unreadable', '0 bytes')
  }

  const bytes = await readBytes(file)

  // Measured from the header where the format allows it, and from the first 64KB where it does
  // not -- enough for an SVG's root element, and cheap either way.
  let dimensions: { width: number; height: number } | null = measureFromHeader(bytes, mime)
  if (!dimensions && mime === 'image/svg+xml') {
    const text = new TextDecoder('utf-8', { fatal: false }).decode(bytes.subarray(0, HEADER_BYTES))
    dimensions = measureSvg(text)
  }

  if (dimensions) {
    const pixels = dimensions.width * dimensions.height
    if (pixels > MAX_IMAGE_PIXELS) {
      // Named, because the number is what the user can act on: a screenshot that came out at
      // 20000x20000 is usually an export that was meant to be a thumbnail.
      throw new IngestError(
        `the image is ${dimensions.width}x${dimensions.height}, over the ${Math.round(
          Math.sqrt(MAX_IMAGE_PIXELS),
        )}x${Math.round(Math.sqrt(MAX_IMAGE_PIXELS))} limit`,
        'unmeasured',
        `${dimensions.width}x${dimensions.height}`,
      )
    }
  }

  const hash = await sha256(bytes)
  const written = await writer.put(bytes, mime)
  // The writer's answer is authoritative. A mismatch would mean the store hashed something else,
  // and using the local hash would put an address in the document that resolves to nothing -- so
  // this is checked rather than assumed.
  if (written !== hash) {
    throw new IngestError(
      'the stored image hashed differently from the bytes read, so the address would not resolve',
      'write-failed',
      `${hash} != ${written}`,
    )
  }

  const src = assetUrl(hash)
  if (isInlineDataUrl(src)) {
    // Unreachable: `assetUrl` builds `holo-asset://<64 hex>` and `isInlineDataUrl` tests for
    // `data:`. Asserted because the alternative is a document holding a base64 payload, and the
    // check that catches that is the one place a mistake would be cheapest to make.
    throw new IngestError('the figure address was not a content address', 'write-failed', src)
  }

  const attrs: Record<string, unknown> = { src, alt: options.alt ?? '' }
  if (dimensions) {
    attrs.width = Math.round(dimensions.width)
    attrs.height = Math.round(dimensions.height)
  }
  return { hash, mime, width: dimensions?.width ?? null, height: dimensions?.height ?? null, node: { type: 'image', attrs } }
}

/**
 * Attach a paste or drop handler to an element and report the files it carried.
 *
 * # Why `preventDefault` is unconditional
 *
 * Because the browser's default for a dropped file is to *navigate to it*. A figure dropped on a
 * word processor window would otherwise replace the application with a PNG, losing every unsaved
 * keystroke. Calling `preventDefault` before looking at what arrived is the only safe order, and
 * the handler returns nothing so a caller cannot forget to.
 *
 * # Why it returns files rather than inserting them
 *
 * Because inserting needs the focused editor, and which editor is focused is the caller's
 * knowledge. This module's job ends at "there are these files".
 */
export function onImageDrop(
  target: EventTarget,
  handler: (files: File[], kind: 'paste' | 'drop') => void | Promise<void>,
): () => void {
  const paste = (event: Event): void => {
    // Not prevented: a paste that carries no image should paste its text, which is the useful
    // behaviour. Preventing unconditionally would break pasting a URL.
    const file = imageInClipboard(event as ClipboardEvent)
    if (!file) return
    event.preventDefault()
    void handler([file], 'paste')
  }
  const drop = (event: Event): void => {
    const transfer = (event as DragEvent).dataTransfer
    // Prevented unconditionally, and *before* looking at what arrived: a file drop's default is
    // to navigate the window to it.
    event.preventDefault()
    const files = imagesInTransfer(transfer)
    if (files.length === 0) return
    void handler(files, 'drop')
  }
  target.addEventListener('paste', paste)
  target.addEventListener('drop', drop)
  return () => {
    target.removeEventListener('paste', paste)
    target.removeEventListener('drop', drop)
  }
}
