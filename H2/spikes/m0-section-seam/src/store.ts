import type { GeneratedSection } from './corpus.js'

/**
 * Minimal in-memory section store for the spike.
 *
 * Mirrors the shape of the real thing (2000.md §2): a manifest plus opaque
 * compressed blobs, with an LRU of parsed sections. The compression step is
 * deliberately left as a stub returning the raw bytes, because the spike is
 * measuring editor behaviour, not the codec. The Blob round-trip through
 * structuredClone is kept because it forces a real parse, which is the cost
 * we actually care about.
 */
export interface Section {
  id: string
  index: number
  wordCount: number
  json: Record<string, unknown>
}

export interface ManifestEntry {
  id: string
  index: number
  orderKey: string
  wordCount: number
  byteSize: number
  estimatedHeight: number
}

export class SectionStore {
  private blobs = new Map<string, string>()
  private parsed = new Map<string, Section>()
  private lru: string[] = []
  manifest: ManifestEntry[] = []

  constructor(
    private sections: GeneratedSection[],
    private lruSize = 4,
  ) {
    this.manifest = sections.map((s, i) => {
      const bytes = JSON.stringify(s.json)
      this.blobs.set(`sec-${i}`, bytes)
      return {
        id: `sec-${i}`,
        index: i,
        orderKey: fractionalKey(i),
        wordCount: s.wordCount,
        byteSize: bytes.length,
        // Mirrors 2000.md §2: chars/65 * 24px
        estimatedHeight: (s.wordCount * 6) / 65 * 24,
      }
    })
  }

  get sectionCount(): number {
    return this.manifest.length
  }

  manifestBytes(): number {
    return JSON.stringify(this.manifest).length
  }

  totalBytes(): number {
    return [...this.blobs.values()].reduce((a, b) => a + b.length, 0)
  }

  /** Parse-or-serve from LRU. This is the cold path the viewport pays. */
  load(index: number): Section {
    const id = `sec-${index}`
    const hit = this.parsed.get(id)
    if (hit) {
      this.touch(id)
      return hit
    }
    const raw = this.blobs.get(id)
    if (!raw) throw new Error(`no such section ${index}`)
    const json = JSON.parse(raw) as Record<string, unknown>
    const sec: Section = {
      id,
      index,
      wordCount: this.manifest[index].wordCount,
      json,
    }
    this.parsed.set(id, sec)
    this.touch(id)
    this.evict()
    return sec
  }

  /** Timing wrapper around the cold path. */
  loadTimed(index: number): { section: Section; ms: number } {
    const t0 = performance.now()
    const section = this.load(index)
    return { section, ms: performance.now() - t0 }
  }

  private touch(id: string) {
    const i = this.lru.indexOf(id)
    if (i >= 0) this.lru.splice(i, 1)
    this.lru.push(id)
  }

  private evict() {
    while (this.lru.length > this.lruSize) {
      const drop = this.lru.shift()!
      this.parsed.delete(drop)
    }
  }

  get cachedCount(): number {
    return this.parsed.size
  }
}

/** Base-62 fractional index, same idea as 2000.md's `order_key`. */
const ALPHABET = '0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz'
export function fractionalKey(i: number): string {
  if (i === 0) return 'a0'
  let s = ''
  let n = i
  while (n > 0) {
    s = ALPHABET[n % ALPHABET.length] + s
    n = Math.floor(n / ALPHABET.length)
  }
  return 'a' + s
}
