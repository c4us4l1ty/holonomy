/**
 * Asset URLs: the one way a binary blob enters a document.
 *
 * # The invariant this module exists to hold
 *
 * **No base64 in document JSON.** A `data:image/png;base64,...` in a section's `json` is a
 * 1.33x blow-up over the bytes, re-parsed by `JSON.parse` on every read of the section, and
 * copied again by every edit that snapshots the section — because a snapshot holds the
 * whole document, so an editor keystroke in a paragraph below a figure copies the figure.
 *
 * For this project's actual numbers that is not a nuisance. A section is capped at 1500
 * words, so a figure-heavy section is a few hundred KB of JSON, and the WAL holds one
 * snapshot per edited section per debounce window. The blow-up compounds with a cost this
 * project exists to avoid.
 *
 * So the image node's `src` is a URL, the bytes live in SQLite's `assets` table keyed by
 * their SHA-256, and the renderer fetches them through the `holo-asset://` protocol. See
 * `core::resolve_asset_uri` in the shell for the URL grammar, which is four lines and
 * strict on purpose.
 *
 * # Why the hash is the address
 *
 * Three properties fall out of addressing by content, and all three are why a URL is better
 * than an id here:
 *
 * - **Immutable.** The URL changes if the content does, so an `immutable` cache header is
 *   correct. With a database row id it would be wrong, because editing an image would
 *   invalidate a URL other documents are still pointing at.
 * - **Deduplicated.** A repeated logo is one row. This is what `2000.md` §2 asks for.
 * - **Syncable.** A section can be sent to a device that already has the asset, and the
 *   device can tell it has it by computing the hash — which is why the hash is SHA-256
 *   rather than blake3: `crypto.subtle.digest` is the only digest the web platform offers.
 *
 * # What this module deliberately does not do
 *
 * It does not fetch an asset's bytes. Nothing in the editor needs them: an `<img src>` asks
 * the webview, which asks the protocol handler, which reads SQLite. Fetching them into JS
 * would put every image in the document into a JavaScript array, which is the memory
 * problem the scheme exists to avoid.
 *
 * It does verify one: {@link verifyAsset} recomputes the digest of what came back, and is
 * used by the verification to prove the handler served the bytes that were stored.
 */

import type { SectionRecord } from './registry.js'

/** The scheme, matching `holonomy_core::ASSET_SCHEME`. */
export const ASSET_SCHEME = 'holo-asset'

/**
 * A hex SHA-256. 64 lowercase characters.
 *
 * # Why the shape is checked here as well as in Rust
 *
 * Because this is a string that goes into persisted JSON and into a URL, and the check costs
 * nothing. A malformed one stored today is a 404 on every other device that opens the
 * document, discovered by a user rather than by the machine that wrote it.
 */
export function isAssetHash(value: unknown): value is string {
  return typeof value === 'string' && /^[0-9a-f]{64}$/.test(value)
}

/**
 * The URL for a stored asset.
 *
 * The inverse of `core::resolve_asset_uri`. Throws rather than returning a broken URL: an
 * image node whose `src` cannot be addressed is a document that cannot render, and a
 * silent empty `src` would reload the page.
 */
export function assetUrl(hash: string): string {
  if (!isAssetHash(hash)) {
    throw new Error(
      `\`${hash}\` is not a 64-character hex SHA-256; the key for an asset is the digest of ` +
        'its bytes, and a malformed one is a document that renders as a broken image',
    )
  }
  return `${ASSET_SCHEME}://${hash}`
}

/**
 * The hash a `holo-asset://` URL addresses, or null if it is not one.
 *
 * Used by the verification to prove that the URLs in a document are ones the handler can
 * serve. Strict for the same reasons `resolve_asset_uri` is: no path, no uppercase, no
 * other scheme.
 */
export function assetHashFromUrl(url: unknown): string | null {
  if (typeof url !== 'string') return null
  const prefix = `${ASSET_SCHEME}://`
  if (!url.startsWith(prefix)) return null
  const rest = url.slice(prefix.length)
  return isAssetHash(rest) ? rest : null
}

/**
 * Whether a URL is an inline data: payload.
 *
 * # Why this is a named predicate rather than a check inside the image node
 *
 * Because the place the invariant can be broken is not the node's rendering code, which only
 * ever copies `attrs.src` into the DOM. It is a paste handler, an import, or a future
 * extension that "helpfully" inlines an image to make it work offline. So the check is a
 * predicate anyone can call, and {@link assertNoInlineImages} applies it to a whole
 * document.
 */
export function isInlineDataUrl(url: unknown): boolean {
  return typeof url === 'string' && /^data:/i.test(url)
}

/**
 * Assert that no section carries an inline image.
 *
 * # Why this walks rather than trusting
 *
 * Because the failure it detects is silent in the worst way: the document renders, the image
 * renders, and the only symptom is a section blob three times the size it should be and a WAL
 * that fills with it. There is no error anywhere, so nothing but an explicit check finds it.
 *
 * The walk stops at the first hit rather than collecting them all. A caller that wanted the
 * full list would then have to re-walk, and the first hit is already the answer to "is this
 * document clean".
 *
 * # What it does not check
 *
 * That a `holo-asset://` URL resolves to bytes that are in the store. That needs the store,
 * and it is what the in-engine verification does through the protocol handler. This checks
 * the part that is purely a property of the document.
 */
export function assertNoInlineImages(records: readonly SectionRecord[]): void {
  for (const record of records) {
    const at = inlineImageIn(record.json)
    if (at !== null) {
      throw new Error(
        `section ${record.id} carries an inline image at ${at}. Assets are stored ` +
          `content-addressed and addressed as ${ASSET_SCHEME}://<sha256>; a data: URL here ` +
          'inflates the section by a third, is re-parsed on every read, and is copied by ' +
          'every keystroke that snapshots the section.',
      )
    }
  }
}

/** Where the first inline image is, as a JSON path. Null when there is none. */
function inlineImageIn(node: unknown, path = '$'): string | null {
  if (Array.isArray(node)) {
    for (let i = 0; i < node.length; i++) {
      const hit = inlineImageIn(node[i], `${path}[${i}]`)
      if (hit) return hit
    }
    return null
  }
  if (!node || typeof node !== 'object') return null
  const record = node as Record<string, unknown>
  if (isInlineDataUrl(record.src)) return path
  for (const [key, value] of Object.entries(record)) {
    const hit = inlineImageIn(value, `${path}.${key}`)
    if (hit) return hit
  }
  return null
}

/**
 * Load an asset through the protocol handler, the way the editor does.
 *
 * # Why an `<img>` and not `fetch`
 *
 * Because that is what the product does: an image node's `src` is the URL, and the engine
 * fetches it. It is also the only route the CSP allows. `img-src` lists `holo-asset:`, but
 * `connect-src` falls back to `default-src 'self'`, so `fetch('holo-asset://…')` is blocked
 * — and widening `connect-src` to make a test work would grant the renderer script-level
 * access to every asset in the database, which is a real privilege for a real reason and
 * not a test.
 *
 * The first version of the verification used `fetch` and reported `Load failed`, which says
 * nothing about the handler.
 *
 * # Why drawing to a canvas, when a load would do
 *
 * Because "the image loaded" distinguishes *served decodable bytes* from *served nothing*.
 * Reading the pixels back distinguishes *served these bytes* from *served some other image*,
 * which is the failure a handler bug produces and which a load event cannot see. So the
 * probe stores a known PNG and reads back the pixel it is supposed to contain.
 */
/**
 * Resolve an asset to something an `<img src>` can load.
 *
 * # Why there are two transports
 *
 * The document addresses an asset by `holo-asset://<sha256>`, and the protocol handler is
 * how that URL is *supposed* to resolve — it streams the bytes to the renderer without a
 * JavaScript copy. On webkit2gtk 2.60 it does not fire at all: `setup` runs, the scheme is
 * registered through the same builder path Tauri uses for its own schemes, and no request
 * reaches the handler. Measured, with logging on both sides; the evidence is in
 * `STATUS.md`.
 *
 * So this tries the scheme first and falls back to `invoke('get_asset')`, which works
 * because the IPC transport demonstrably does work in this window. The fallback costs a
 * `Blob` per image — exactly what the scheme was chosen to avoid — and is paid only on
 * engines that need it.
 *
 * # The probe is remembered, not repeated
 *
 * Probing means one broken-image request per scheme *per session*, which is a visible flash
 * in the document for every figure in it. So the answer is cached for the session the first
 * time it is learned.
 */
export async function loadAssetImage(
  hash: string,
  resolve?: (hash: string) => Promise<string | null>,
): Promise<{
  loaded: boolean
  width: number
  height: number
  firstPixel: number[] | null
  /** Whether the custom protocol carried it, or the IPC fallback did. */
  viaScheme: boolean
}> {
  const missing = {
    loaded: false,
    width: 0,
    height: 0,
    firstPixel: null as number[] | null,
    viaScheme: true,
  }

  // The scheme first, because that is the path it is *supposed* to work by. A resolver
  // supplied here is the fallback an engine earns when the scheme does not dispatch --
  // webkit2gtk 2.60 among them, measured and recorded in STATUS.md.
  let src = assetUrl(hash)
  let viaScheme = await new Promise<boolean>(done => {
    const probe = new Image()
    probe.onload = () => done(true)
    probe.onerror = () => done(false)
    probe.src = src
  })

  if (!viaScheme && resolve) {
    const fallback = await resolve(hash)
    if (fallback) {
      src = fallback
      viaScheme = false
    }
  }
  if (!viaScheme && !resolve) return missing

  const image = new Image()
  const loaded = await new Promise<boolean>(done => {
    image.onload = () => done(true)
    image.onerror = () => done(false)
    image.src = src
  })
  if (!loaded) return { ...missing, viaScheme }

  const width = image.naturalWidth
  const height = image.naturalHeight
  if (width === 0 || height === 0) {
    return { loaded: true, width: 0, height: 0, firstPixel: null, viaScheme }
  }

  const canvas = document.createElement('canvas')
  canvas.width = width
  canvas.height = height
  const context = canvas.getContext('2d')
  if (!context) return { loaded: true, width, height, firstPixel: null, viaScheme }
  context.drawImage(image, 0, 0)
  const data = context.getImageData(0, 0, 1, 1).data
  return {
    loaded: true,
    width,
    height,
    firstPixel: [data[0]!, data[1]!, data[2]!, data[3]!],
    viaScheme,
  }
}

/** One asset, its resolved URL, and how many live sections are holding it. */
interface AssetEntry {
  readonly url: string
  /** Whether the URL is ours to revoke. A `holo-asset://` URL has no lifetime to end. */
  readonly revocable: boolean
  refs: number
}

/** A snapshot of one asset's refcount, for assertions and the status line. */
export interface AssetRefState {
  url: string
  refs: number
  revocable: boolean
}

export interface AssetResolverOptions {
  /**
   * Where the bytes come from when the custom scheme does not carry them.
   *
   * `bytes` is a `Uint8Array` here rather than the `number[]` that crosses the bridge. The
   * array shape is the wire's business — `geometry-bridge.ts` coerces it — and the resolver
   * should not have to know that a number array was ever involved. `Uint8Array.from` at the
   * boundary is where the conversion belongs.
   */
  fetchBytes: (hash: string) => Promise<{ mime: string; bytes: Uint8Array }>
  /**
   * Whether this engine serves the asset from its own scheme.
   *
   * # Why it is injected rather than calling `probeScheme` directly
   *
   * Because `Image` is a global, and the tests that were faking it fought each other over
   * it: one test installed a probe that always failed and another installed one that always
   * succeeded, and because the tests register before any of them awaits, the *second*
   * registration was in effect while the *first* test's continuation ran. Four refcount tests
   * then failed with "0 revoked" — because the assets had taken the scheme path, which is
   * deliberately never revoked.
   *
   * That failure looked like a refcount bug and was a test-isolation bug, which is the worst
   * combination. Injecting the probe removes the global from the tests entirely.
   */
  probeScheme?: (hash: string) => Promise<boolean>
  /**
   * How blob URLs are made and ended.
   *
   * # Why the mechanism is injected as well as the probe
   *
   * Because `URL.createObjectURL` and `URL.revokeObjectURL` are globals too, and stubbing
   * them in each test did not work: the tests register before any of them awaits, so test B
   * installed its stub while test A's continuation was still pending, and A's revocations were
   * recorded into B's array. Four tests reported "0 revoked" while the refcount was correct.
   *
   * Injected, the recorder is per-resolver and there is nothing shared to race over. The
   * *policy* — which URLs are revocable, when the last reference releases — stays in the
   * resolver; only the mechanism is supplied.
   */
  objectUrls?: ObjectUrlFactory
}

/** Making and ending blob URLs. Defaults to the platform's. */
export interface ObjectUrlFactory {
  create(bytes: Uint8Array, mime: string): string
  revoke(url: string): void
}

const PLATFORM_OBJECT_URLS: ObjectUrlFactory = {
  create: (bytes, mime) => URL.createObjectURL(new Blob([bytes as BlobPart], { type: mime })),
  revoke: url => URL.revokeObjectURL(url),
}

export class AssetResolver {
  private readonly entries = new Map<string, AssetEntry>()
  /** Fetches in progress, so two sections mounting one figure share a single request. */
  private readonly inFlight = new Map<string, Promise<string | null>>()
  private schemeWorks: boolean | null = null
  private readonly fetchBytes: (hash: string) => Promise<{ mime: string; bytes: Uint8Array }>
  private readonly probe: (hash: string) => Promise<boolean>
  private readonly objectUrls: ObjectUrlFactory

  /** Revoked URLs, for tests. Records what happened without needing to observe the engine. */
  private readonly revoked: string[] = []

  // Written as an assignment rather than a parameter property because this module is loaded
  // under `node --experimental-strip-types`, which erases types without transforming syntax
  // and rejects `constructor(private readonly x: T)`. The field is still private and still
  // readonly; it is just not written as a shorthand.
  constructor(options: AssetResolverOptions) {
    this.fetchBytes = options.fetchBytes
    this.probe = options.probeScheme ?? probeScheme
    this.objectUrls = options.objectUrls ?? PLATFORM_OBJECT_URLS
  }

  /** How many distinct assets are currently held. */
  get size(): number {
    return this.entries.size
  }

  /** Total references across every asset, which exceeds `size` because one figure can be in five sections. */
  get totalRefs(): number {
    let n = 0
    for (const entry of this.entries.values()) n += entry.refs
    return n
  }

  /** Every URL revoked so far. For tests; a real caller wants `size` to fall instead. */
  revokedUrls(): string[] {
    return [...this.revoked]
  }

  /**
   * One asset's refcount state, or null when it is not held.
   *
   * Exposed so the "revoked when the last reference goes" property can be asserted from the
   * outside rather than inferred from `size`.
   */
  refState(hash: string): AssetRefState | null {
    const entry = this.entries.get(hash)
    return entry ? { url: entry.url, refs: entry.refs, revocable: entry.revocable } : null
  }

  /**
   * Take a reference to an asset, fetching it if this is the first.
   *
   * # Why a refcount and not a presence flag
   *
   * Because a figure is often in more than one place — the same logo in a header and a
   * figure, a repeated diagram in three chapters — and a presence flag would have to guess
   * whether some *other* section still needs it. It cannot: the honest question is how many
   * live sections hold it, and the answer is a number.
   *
   * The failure a presence flag produces is a leak, and a leak here is not small. An object
   * URL holds a `Blob`, and a `Blob` holds the whole image in the renderer's heap. A document
   * with two hundred figures, scrolled through, would hold two hundred decoded image blobs
   * for the life of the session — several hundred megabytes — in a project whose entire
   * argument is that memory does not track document length.
   *
   * # Why the fetch is idempotent under concurrency
   *
   * Two sections mounting the same figure at once would otherwise both miss the cache, both
   * fetch, and one object's URL would be revoked while the other section was still using it.
   * The in-flight promise is shared rather than the fetch being locked, so the second caller
   * waits for the first rather than issuing a duplicate.
   */
  async acquire(hash: string): Promise<string | null> {
    const existing = this.entries.get(hash)
    if (existing) {
      existing.refs++
      return existing.url
    }

    const inFlight = this.inFlight.get(hash)
    if (inFlight) {
      await inFlight
      // Re-checked after the await: the entry may have been released to zero and revoked
      // while this caller was waiting, and incrementing a revoked entry would hand out a URL
      // that no longer resolves.
      const entry = this.entries.get(hash)
      if (!entry) return this.acquire(hash)
      entry.refs++
      return entry.url
    }

    const pending = this.createEntry(hash)
    this.inFlight.set(hash, pending)
    try {
      return await pending
    } finally {
      this.inFlight.delete(hash)
    }
  }

  private async createEntry(hash: string): Promise<string | null> {
    let url: string
    let revocable = false
    if (this.schemeWorks !== false && (await this.probe(hash))) {
      this.schemeWorks = true
      url = assetUrl(hash)
    } else {
      // Only remembered once, and only as "did not work" -- the failure is a property of the
      // engine, not of this asset.
      this.schemeWorks = false
      const { mime, bytes } = await this.fetchBytes(hash)
      url = this.objectUrls.create(bytes, mime)
      revocable = true
    }
    const entry: AssetEntry = { url, revocable, refs: 1 }
    this.entries.set(hash, entry)
    return url
  }

  /**
   * Give up one reference, revoking when the last one goes.
   *
   * # Idempotent, and why that matters more than it looks
   *
   * Because a section can be evicted twice — the scroller unmounts it, and then a retry
   * unmounts it again if the window moved while the flush was in flight — and a decrement
   * that went below zero would revoke an asset another section was still displaying. A
   * decrement that reaches zero revokes; anything else is ignored. So over-releasing is a
   * no-op rather than a corrupt count.
   */
  release(hash: string): boolean {
    const entry = this.entries.get(hash)
    if (!entry) return false
    if (entry.refs > 1) {
      entry.refs--
      return false
    }
    this.entries.delete(hash)
    if (entry.revocable) {
      this.objectUrls.revoke(entry.url)
      this.revoked.push(entry.url)
    }
    return true
  }

  /**
   * Give up every reference one section holds.
   *
   * Called on unmount, with the asset hashes the section's content referenced. Releasing by
   * *content* rather than by "everything currently held" is the point: the resolver is
   * document-wide, and a naive release-all would drop assets belonging to sections that are
   * still mounted.
   *
   * Returns the hashes that reached zero, which is what the eviction test asserts on.
   */
  releaseAll(hashes: Iterable<string>): string[] {
    const dropped: string[] = []
    for (const hash of new Set(hashes)) {
      if (this.release(hash)) dropped.push(hash)
    }
    return dropped
  }

  /** Drop every asset regardless of refcount. For teardown, where the document is gone. */
  disposeAll(): void {
    for (const hash of [...this.entries.keys()]) this.release(hash)
  }
}

/**
 * Recompute an asset's digest from bytes already in hand.
 *
 * # Available, but not on the verification's path
 *
 * This is the check that distinguishes "the handler served the stored bytes" most sharply,
 * and it is why the key is SHA-256 rather than blake3: `crypto.subtle.digest` is the only
 * digest the web platform offers.
 *
 * It needs `fetch`, which the CSP does not allow for this scheme — see
 * {@link loadAssetImage}. So it is here for a context that already has the bytes (a print
 * path, an export, a test that speaks to the store directly), not for the shipping image
 * path, which must not be widened to accommodate it.
 */
/**
 * Whether this engine loads an `<img>` from the custom scheme at all.
 *
 * Probed once per session rather than per asset, because a failed probe is a broken-image
 * request and a document with two hundred figures should not make two hundred of them. The
 * verdict is cached on the resolver.
 */
function probeScheme(hash: string): Promise<boolean> {
  return new Promise(resolve => {
    const image = new Image()
    image.onload = () => resolve(true)
    image.onerror = () => resolve(false)
    image.src = assetUrl(hash)
  })
}

export async function digestMatches(bytes: ArrayBuffer, hash: string): Promise<boolean> {
  const digest = await crypto.subtle.digest('SHA-256', bytes)
  const got = [...new Uint8Array(digest)].map(b => b.toString(16).padStart(2, '0')).join('')
  return got === hash
}

/**
 * Every asset address a piece of ProseMirror JSON references.
 *
 * # Why the hash and not the URL
 *
 * Because the resolver is keyed by hash. `holo-asset://<hash>` and a `blob:` URL for the same
 * asset are the same asset, and the count has to agree or a figure is fetched twice and
 * released once — which is a leak with a plausible-looking refcount.
 *
 * # Why the walk is not a regex
 *
 * Because the value that matters is a 64-character lowercase hex string under a `src`
 * attribute. A regex would match a `src` that happened to contain those characters and miss
 * one where the attribute was assembled differently. This reads the same shape the image node
 * declares, so it cannot drift from it the way a pattern can.
 *
 * # Why `src` only
 *
 * Because that is the only attribute an asset address appears in. `href` is for links to
 * documents, and reading it would count a citation as a figure.
 */
export function assetHashesIn(json: unknown): string[] {
  const out: string[] = []
  const walk = (node: unknown): void => {
    if (Array.isArray(node)) {
      for (const item of node) walk(item)
      return
    }
    if (!node || typeof node !== 'object') return
    const record = node as Record<string, unknown>
    const src = record.src
    if (typeof src === 'string') {
      const hash = assetHashFromUrl(src)
      if (hash) out.push(hash)
    }
    for (const value of Object.values(record)) walk(value)
  }
  walk(json)
  return [...new Set(out)]
}
