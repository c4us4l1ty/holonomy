/**
 * Which sections the renderer holds content for, and which it must fetch.
 *
 * # The problem
 *
 * A 2000-page document at 1500 words per section is 667 sections. Their content is about
 * 7KB compressed each, so keeping every decoded JSON in memory is roughly 4.7MB — not
 * catastrophic, but it grows without bound with document length and it is all content
 * the user cannot see. A word processor that holds a whole novel in the renderer is a
 * word processor whose memory use tracks document size, which is the thing this project
 * exists to avoid.
 *
 * # The bound
 *
 * Thirty sections. Chosen to be comfortably larger than the scroller's working set — a
 * 1080px window shows about a third of a 3400px section, and the window slides by whole
 * sections, so five mounted plus overscan is what is live at any moment — while being
 * small enough that 30 is a number rather than a policy. A user flinging the scrollbar
 * does not notice 30 cached sections; they would notice 667.
 *
 * # Why the count is a constant here and not a boot-payload field
 *
 * It is a property of the renderer, not of the document. Putting it in the payload would
 * mean Rust has an opinion about frontend memory, and it would mean changing it requires
 * a contract change.
 *
 * # What eviction means
 *
 * A section that leaves the cache has its content dropped, not blanked. Its manifest row
 * — counts, order, title — stays, so the geometry still sizes it correctly and the
 * scrollbar is unaffected. Only the bytes go, and {@link SectionHydrator} fetches them
 * when the section is next mounted.
 *
 * That distinction is the reason eviction is safe at all. Blanking the content would look
 * identical in the cache and catastrophic in the document: an editor mounted on blank
 * content and typed into would overwrite the stored section on its first keystroke.
 */

/** How many sections keep their content in memory. See the module header. */
export const CONTENT_CACHE_CAPACITY = 30

/** What the cache holds for one section. */
export interface CachedContent {
  json: unknown
}

export class SectionCache {
  private readonly cap: number
  /** Insertion/access order, least recent first. */
  private order: string[] = []
  private readonly entries = new Map<string, CachedContent>()

  private onEvict: ((id: string) => void) | null = null
  private evictions = 0
  private hits = 0
  private misses = 0

  constructor(capacity: number = CONTENT_CACHE_CAPACITY) {
    // At least one. A capacity of zero would mean every `set` immediately evicts its own
    // entry, which is not a cache and is not a configuration anybody wants.
    this.cap = Math.max(1, capacity)
  }

  get capacity(): number {
    return this.cap
  }

  get size(): number {
    return this.entries.size
  }

  has(id: string): boolean {
    if (this.entries.has(id)) this.hits++
    else this.misses++
    return this.entries.has(id)
  }

  /**
   * Get without counting a hit or a miss.
   *
   * For the existence question the scroller asks on every mount decision. Counting that
   * as a miss would make the hit rate a measure of how often the scroller looks rather
   * than of how often the cache was right.
   */
  includes(id: string): boolean {
    return this.entries.has(id)
  }

  /** Fetch and mark as most recently used. */
  get(id: string): CachedContent | undefined {
    const hit = this.entries.get(id)
    if (hit) this.hits++
    else this.misses++
    if (hit) this.touch(id)
    return hit
  }

  /**
   * Store content, evicting the least recently used section if over capacity.
   *
   * Returns the id that was evicted, or `null`. The caller needs it: an evicted section
   * that happens to be the one being scrolled back to has to be re-fetched, and only the
   * caller knows which section the eviction concerns.
   */
  set(id: string, json: unknown): string | null {
    if (this.entries.has(id)) {
      this.entries.set(id, { json })
      this.touch(id)
      return null
    }
    this.entries.set(id, { json })
    this.order.push(id)
    if (this.order.length <= this.cap) return null

    const victim = this.order.shift()
    if (victim !== undefined) {
      this.entries.delete(victim)
      this.evictions++
      this.onEvict?.(victim)
    }
    return victim ?? null
  }

  /**
   * Mark as most recently used, without fetching.
   *
   * Distinct from `get` so that a section the scroller merely *mounted* counts as used
   * without also counting as a hit — it was already resident.
   */
  touch(id: string): void {
    const at = this.order.indexOf(id)
    if (at < 0) return
    this.order.splice(at, 1)
    this.order.push(id)
  }

  /**
   * Drop one section, for an eviction that is not about capacity.
   *
   * Used when a section is destroyed: its bytes are already in SQLite, and keeping them
   * here would mean the cache and the store could disagree after an edit.
   */
  delete(id: string): boolean {
    const had = this.entries.delete(id)
    const at = this.order.indexOf(id)
    if (at >= 0) this.order.splice(at, 1)
    return had
  }

  clear(): void {
    this.entries.clear()
    this.order = []
  }

  /**
   * Told when a section leaves the cache, whatever the reason.
   *
   * # Why the cache reports its own evictions
   *
   * Because a cache that drops content and a record that still holds it are two different
   * pieces of truth, and the record is the one that decides whether an editor may be
   * mounted. The cache cannot fix that — it does not know about records — so it reports
   * and the caller reacts.
   *
   * Capacity evictions call this; {@link delete} does not, because a deliberate removal is
   * already synchronised by whoever asked for it.
   */
  onEviction(fn: (id: string) => void): void {
    this.onEvict = fn
  }

  /** Ids currently resident, least recently used first. */
  residentIds(): string[] {
    return [...this.order]
  }

  /** The capacity and the ids that would be dropped next. */
  inspect(): {
    capacity: number
    size: number
    order: string[]
    nextToEvict: string | null
    hits: number
    misses: number
    evictions: number
  } {
    return {
      capacity: this.cap,
      size: this.entries.size,
      order: [...this.order],
      nextToEvict: this.order[0] ?? null,
      hits: this.hits,
      misses: this.misses,
      evictions: this.evictions,
    }
  }
}
